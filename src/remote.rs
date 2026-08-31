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

/// Programs whose command line this understands.
///
/// **`ssh` only, and that is a correctness limit rather than an oversight.**
/// `mosh` and `et` reach a box the same way and were in this list, but they
/// take LONG options and `ssh` does not — and the parse below is built on
/// ssh's single-letter grammar. `mosh --port 60000 box` yields `60000`,
/// because `strip_prefix('-')` leaves `-port`, the scan finds `p` mid-word,
/// and the attached-value branch consumes nothing. That is a wrong host, not
/// a refusal, and a wrong host here re-roots the workspace onto the wrong
/// machine. Parsing them properly needs a per-program table of which long
/// options take a separate word; until that exists, refusing is the only
/// honest answer.
///
/// `sshfs` is absent for a different reason: it mounts a filesystem rather
/// than giving you a shell, so re-rooting onto it answers a different
/// question. `ssh-agent` and `ssh-keygen` share a prefix with `ssh` and
/// nothing else, which is why the match is on the whole name.
const SSH_PROGRAMS: [&str; 1] = ["ssh"];

/// `ssh` flags that take a SEPARATE argument, per ssh(1).
///
/// This list is what makes the parse work: without it, `ssh -p 2222 box`
/// yields `2222` as the destination, because `2222` is the first word that
/// does not begin with `-`. The boolean flags need no listing — anything
/// starting with `-` that is not here is simply skipped, so a SHORT flag
/// added to ssh in future degrades to "skipped" rather than to a wrong host.
/// That guarantee does NOT extend to long options — `--port` scans as `-port`
/// and finds `p` mid-word — which is why [`SSH_PROGRAMS`] admits only `ssh`,
/// the one program in this family that has none.
const SSH_FLAGS_WITH_ARG: [char; 22] = [
    'B', 'b', 'c', 'D', 'E', 'e', 'F', 'I', 'i', 'J', 'L', 'l', 'm', 'O', 'o', 'P', 'p', 'Q', 'R',
    'S', 'W', 'w',
];

/// The destination an `ssh`-family command line names, or `None` when the
/// line is not one of those programs or names no host yet (#364).
///
/// `cmd` is the process's argv. The rule is ssh's own: skip flags, skip the
/// argument of any flag that takes one, and the first remaining word is the
/// destination — everything after it is the remote command.
///
/// Deliberately pure and argv-shaped rather than reading `/proc`: the whole
/// difficulty is the flag grammar, and a function that takes argv can be
/// swept against real invocations without a live process.
pub fn ssh_destination(cmd: &[&str]) -> Option<String> {
    // `rsplit` always yields at least one item, so there is no empty case.
    let program = cmd.first()?.rsplit('/').next()?;
    if !SSH_PROGRAMS.contains(&program) {
        return None;
    }
    let takes_arg = |c: char| SSH_FLAGS_WITH_ARG.contains(&c);
    let mut rest = cmd[1..].iter();
    while let Some(word) = rest.next() {
        // `--` ends option parsing: the next word is the destination even if
        // it begins with a dash. Without this, a host named like a flag is
        // skipped and the parse runs off the end.
        if *word == "--" {
            return rest
                .next()
                .and_then(|w| (!w.is_empty()).then(|| (*w).to_string()));
        }
        let Some(flags) = word.strip_prefix('-') else {
            // The first non-flag word is the destination. An empty string is
            // not a host, and neither is a bare `-`.
            return (!word.is_empty()).then(|| (*word).to_string());
        };
        if flags.is_empty() {
            continue;
        }
        // Bundled booleans (`-4qt`) take nothing. A flag that takes an
        // argument consumes the REST OF THIS WORD if there is any (`-p2222`),
        // and otherwise the next word.
        for (i, c) in flags.char_indices() {
            if takes_arg(c) {
                if i + c.len_utf8() == flags.len() {
                    rest.next();
                }
                break;
            }
        }
    }
    None
}

/// The configured host an ssh destination names, if croft knows one (#364).
///
/// Matches on the alias or the `HostName`, either of which people type, and
/// ignores any `user@` prefix — the user is how you log in, not which machine
/// it is, and offering a different workspace per login name would split one
/// box into several.
///
/// `None` for a host with no `~/.ssh/config` entry. That is a real limit and
/// the right one for now: the offer exists to hand the destination to the
/// remote-connect flow, which is keyed on a config entry, so offering for an
/// unknown host would promise something the next step cannot deliver.
pub fn resolve_offer_host<'a>(dest: &str, targets: &'a [RemoteTarget]) -> Option<&'a RemoteTarget> {
    let host = dest.rsplit('@').next().unwrap_or(dest).trim();
    if host.is_empty() {
        return None;
    }
    // Alias first, across ALL entries, before falling back to HostName.
    // A single `find` testing both would let an earlier entry's HostName beat
    // a later entry's own alias: with `Host jump / HostName db-1` above
    // `Host db-1`, typing `ssh db-1` resolved to `jump` — an offer to re-root
    // onto a different machine than the one named.
    targets
        .iter()
        .find(|t| t.alias.eq_ignore_ascii_case(host))
        .or_else(|| {
            targets.iter().find(|t| {
                t.host_name
                    .as_deref()
                    .is_some_and(|h| h.eq_ignore_ascii_case(host))
            })
        })
}

/// The offer's answer, or the reason there is none (#364).
///
/// Split from the app handler so all four outcomes are assertable without a
/// live pane: the handler samples argv and renders this, and nothing else
/// decides what the user is told.
pub fn ssh_reroot_decision<'a>(
    argv: &[&str],
    targets: &'a [RemoteTarget],
) -> Result<&'a RemoteTarget, String> {
    match ssh_destination(argv) {
        Some(dest) => resolve_offer_host(&dest, targets).ok_or_else(|| {
            // An ssh session to a box with no config entry: name it, since
            // that is the actionable half of the answer.
            format!("{dest} is not in your ~/.ssh/config, so croft cannot open a workspace on it")
        }),
        None => Err(String::from("This pane is not an SSH session")),
    }
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

pub fn launch_croft(host: &str, path: Option<&str>, solo: bool) -> Result<RemoteOutcome> {
    launch_croft_with(host, path, None, solo)
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

/// True when the remote host has `dtach`, which croft launches its session
/// under for persistence across an SSH transport drop. Best-effort over the
/// existing control master; any error (host unreachable, no dtach) reports
/// `false` so croft simply runs without persistence.
fn remote_has_session_supervisor(ssh: &SshControl) -> bool {
    ssh.command()
        .arg(&ssh.host)
        .arg("command -v dtach >/dev/null 2>&1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Re-establish the SSH control master after a transport drop (laptop sleep,
/// network change), printing progress. Each attempt is bounded by the master's
/// `ConnectTimeout`, then backs off; the user presses Ctrl+C to stop and fall
/// back to the local shell. Returns the fresh control once the host answers.
fn reconnect_master(host: &str) -> Option<SshControl> {
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
fn run_croft_session(
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
    push_config_files(&ssh, &mut |msg| println!("{msg}"));
    let mut bootstrapped = false;
    // The relay rendezvous is keyed on the launch identity — the very same
    // `hash(launch arg)` the dtach socket uses — NOT the remote croft's
    // `workspace_root`. The workspace diverges from the launch identity (a
    // no-path launch opens the login dir, but the user may then open a
    // subfolder, and that opened root is also preserved across the F9 re-exec),
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
                    Some(new_ssh) => ssh = new_ssh,
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
        // remote croft offers the F9 reload once the new binary lands. The
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
            // Bound each connect attempt so a reconnect against a host that is
            // still asleep/offline fails fast instead of hanging, and let the
            // master tear itself down promptly once the link goes away.
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("ServerAliveInterval=10")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
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
        ssh_socket_command(&self.socket_path, false)
    }

    /// A command for the background install path, which runs while the user is
    /// already attached. See [`ssh_socket_command`] for why `-n` is
    /// load-bearing here.
    fn background_command(&self) -> Command {
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
    fn background_shell(&self, script: &str) -> Command {
        let mut command = self.background_command();
        command.arg(&self.host).arg(script);
        command
    }
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
fn ssh_socket_command(socket_path: &Path, background: bool) -> Command {
    let mut command = Command::new("ssh");
    command
        .arg("-S")
        .arg(socket_path)
        .arg("-o")
        .arg("ControlMaster=no")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("-o")
        .arg("ServerAliveInterval=10")
        .arg("-o")
        .arg("ServerAliveCountMax=3");
    if background {
        command
            .arg("-n")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    command
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
    stop: Arc<AtomicBool>,
    tail: Option<Child>,
    handle: Option<JoinHandle<()>>,
}

/// Failure message for the relay-setup ssh. The stderr suffix is appended
/// only when the child actually said something: a signal-killed ssh or a
/// silent `set -e` exit (full disk under `mkdir -p`/`printf`) has an empty
/// stderr, and the message must not end in a dangling ": ".
fn relay_setup_error(status: ExitStatus, stderr: &[u8]) -> String {
    let err = String::from_utf8_lossy(stderr);
    let err = err.trim();
    if err.is_empty() {
        format!("remote relay setup failed with {status}")
    } else {
        format!("remote relay setup failed with {status}: {err}")
    }
}

impl DropPump {
    fn start(ssh: &SshControl, relay_id: &str) -> Result<Self> {
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
    Pull {
        id: String,
        src: String,
    },
    Clipboard {
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

/// Run `cmd` on the remote over the existing master socket (no second auth, no
/// nested master). Used for the relay inbox bookkeeping (`mkdir`, sentinel
/// writes). Returns whether it exited zero.
fn ssh_exec(host: &str, socket: &Path, cmd: &str) -> bool {
    Command::new("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-o")
        .arg("ControlMaster=no")
        .arg(host)
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Hand `url` to the local platform opener (`open` on macOS, `xdg-open` on
/// Linux). The caller has already validated the URL.
fn open_local_url(url: &str) -> std::io::Result<std::process::ExitStatus> {
    let program = if cfg!(target_os = "linux") {
        "xdg-open"
    } else {
        "open"
    };
    Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
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
    if !ssh_exec(
        host,
        socket,
        &format!("mkdir -p {}", shell_quote(&dest_dir)),
    ) {
        write_relay_err(host, socket, inbox_dir, request_id, "remote mkdir failed");
        return;
    }
    match open_local_url(url) {
        Ok(s) if s.success() => {
            let _ = ssh_exec(
                host,
                socket,
                &format!("touch {}/.ok", shell_quote(&dest_dir)),
            );
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

/// Mirror the remote port locally when it's free, else take an ephemeral port
/// from the OS. The tiny bind race (port taken between probe and `ssh -L`) just
/// surfaces as a forward failure the user can retry.
fn pick_local_port(remote: u16) -> u16 {
    use std::net::TcpListener;
    if TcpListener::bind(("127.0.0.1", remote)).is_ok() {
        return remote;
    }
    TcpListener::bind(("127.0.0.1", 0))
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(remote)
}

/// Forward remote loopback `port` to the local machine by adding an `-L`
/// channel to the live SSH master (`ssh -O forward` — no second auth), then,
/// when `open`, launch the local browser at the forwarded address. The chosen
/// local port is written as the `.ok` payload so remote croft can label the
/// forward and re-open it; only digits ever reach the `-L` spec, so there's no
/// metacharacter to smuggle.
fn handle_forward_request(
    host: &str,
    socket: &Path,
    inbox_dir: &str,
    request_id: &str,
    port: &str,
    open: bool,
) {
    let Some(remote_port) = port.parse::<u16>().ok().filter(|p| *p >= 1) else {
        write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            "rejected: port must be 1..=65535",
        );
        return;
    };
    let dest_dir = format!("{inbox_dir}/{request_id}");
    if !ssh_exec(
        host,
        socket,
        &format!("mkdir -p {}", shell_quote(&dest_dir)),
    ) {
        write_relay_err(host, socket, inbox_dir, request_id, "remote mkdir failed");
        return;
    }
    let local_port = pick_local_port(remote_port);
    let spec = format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}");
    let forwarded = Command::new("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-O")
        .arg("forward")
        .arg("-L")
        .arg(&spec)
        .arg(host)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !forwarded {
        write_relay_err(host, socket, inbox_dir, request_id, "ssh -O forward failed");
        return;
    }
    if open {
        let _ = open_local_url(&format!("http://127.0.0.1:{local_port}/"));
    }
    let _ = ssh_exec(
        host,
        socket,
        &format!("printf %s {local_port} > {}/.ok", shell_quote(&dest_dir)),
    );
}

/// Tear down a forward added by [`handle_forward_request`] via `ssh -O cancel`
/// against the live master. Best-effort and silent: only digits reach the `-L`
/// spec, and a stale cancel (the forward already gone) is harmless.
fn handle_unforward_request(host: &str, socket: &Path, local: &str, remote: &str) {
    let (Ok(local), Ok(remote)) = (local.parse::<u16>(), remote.parse::<u16>()) else {
        return;
    };
    let spec = format!("127.0.0.1:{local}:127.0.0.1:{remote}");
    let _ = Command::new("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-O")
        .arg("cancel")
        .arg("-L")
        .arg(&spec)
        .arg(host)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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
/// that is invariant across an in-session workspace change and the F9 re-exec.
/// `DefaultHasher` has fixed SipHash keys, so the value is deterministic across
/// croft processes.
pub(crate) fn relay_session_id(launch_arg: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(launch_arg.as_bytes());
    format!("{:016x}", hasher.finish())
}

/// A value unique to THIS client process, for the session-host client
/// identity (#229). Call once per process and carry the result; calling it
/// twice yields different values by design.
///
/// pid alone is not enough — pids are recycled, and two croft clients on
/// different machines attaching to one remote session can share one. Mixing
/// in the start time and the address of a local allocation distinguishes
/// those without needing a uuid dependency.
pub(crate) fn client_process_nonce() -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::process::id().hash(&mut hasher);
    if let Ok(since_epoch) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        since_epoch.as_nanos().hash(&mut hasher);
    }
    // ASLR plus allocator state: distinct between concurrent processes that
    // started in the same nanosecond and share a recycled pid.
    let here = Box::new(0u8);
    (Box::as_ref(&here) as *const u8 as usize).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
    solo: bool,
) -> Result<ExitStatus> {
    let mut command = ssh.command();
    command
        .arg("-tt")
        .arg(&ssh.host)
        .arg(remote_croft_command(path, env, solo));
    command.status().context("starting ssh")
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
fn push_config_files(ssh: &SshControl, log: &mut dyn FnMut(String)) {
    let files = crate::config_sync::local_files();
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

    let bulk = crate::remote_bulk::establish(&ssh.host, &ssh.socket_path, |_| {});
    let mut pushed = Vec::new();
    let mut failed = Vec::new();
    for (syncable, local) in &files {
        let dest = crate::config_sync::remote_dest(&ssh.host, syncable.name);
        let mut rsync = ship_file_rsync_command(&bulk.lane, &ssh.socket_path, local, &dest);
        match rsync.status() {
            Ok(st) if st.success() => pushed.push(syncable),
            _ => failed.push(syncable.name),
        }
    }

    if !pushed.is_empty() {
        // Name the files rather than counting them: "4 files" tells a user
        // nothing about whether the one they just edited went.
        let names: Vec<&str> = pushed.iter().map(|s| s.name).collect();
        log(format!("Config sync: pushed {}", names.join(", ")));
        // ALL of it applies on the remote's next launch, not just the files
        // with no reload arm. The reload path is driven by croft's own save
        // (`reload_config_for_path`), and nothing watches ~/.config/croft, so
        // a file that arrives by rsync is not noticed at all until relaunch.
        // Saying "these four are live" would be the lie this message exists
        // to prevent.
        log(format!(
            "Config sync: {} applies on the remote's next launch",
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

pub fn remote_croft_command(path: Option<&str>, env: &[(String, String)], solo: bool) -> String {
    remote_croft_command_for_terminal(
        path,
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        crate::iterm2_inline::detect_osk_auto(),
        env,
        solo,
    )
}

fn remote_croft_command_for_terminal(
    path: Option<&str>,
    term_program: Option<&str>,
    term: Option<&str>,
    osk: bool,
    env: &[(String, String)],
    solo: bool,
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
    prefix.push_str("export PATH=\"$HOME/.cargo/bin:$PATH\"; command -v croft >/dev/null 2>&1 || { echo 'croft not found on remote PATH' >&2; exit 127; }; ");
    let croft_invocation = match path.filter(|p| !p.is_empty()) {
        Some(p) => format!("croft {}", shell_quote(p)),
        None => String::from("croft"),
    };
    // A solo guest never attaches the shared PTY: it runs its own croft
    // (independent viewport) wired to the workspace's collab relay for
    // shared-file edits (docs/MULTIPLAYER.md, Phase D). The probe keeps a
    // freshly shipped local croft from driving an unknown subcommand at a
    // remote binary the background updater hasn't replaced yet, exactly
    // like the session-host probe below; such a host falls back to a plain
    // (non-collab) croft rather than failing the connect. No persistence
    // supervisor: a solo viewport is per-guest scratch state, and wrapping
    // it in the mux would just share its PTY again.
    if solo {
        let collab_socket = collab_socket_path(path);
        return format!(
            "{prefix}if croft collab-relay --probe >/dev/null 2>&1; then mkdir -p \"$(dirname \"{collab_socket}\")\"; croft collab-relay --ensure --socket \"{collab_socket}\"; export CROFT_COLLAB_SOCKET=\"{collab_socket}\"; export CROFT_COLLAB_ROLE=guest; exec {croft_invocation}; else exec {croft_invocation}; fi"
        );
    }
    // Run croft under a persistence supervisor so the session survives an SSH
    // transport drop (laptop sleep, network change). Preferred: croft's own
    // session host (`croft session-host`, the multiplayer mux; see
    // docs/MULTIPLAYER.md) — attach-or-create semantics, byte-transparent
    // broadcast to every attached client, server-side write control, and it
    // propagates the inner exit code (dtach never did, so drop-to-local's 88
    // only works on this branch). The `--probe` guard keeps a freshly shipped
    // local croft from sending an unknown subcommand at a remote binary the
    // background updater hasn't replaced yet. Fallback: dtach, exactly as
    // before (`-A` attach-or-create, `-E`/`-z` keep its hands off croft's
    // Ctrl chords, `-r winch` repaints on reattach). Both supervisors are
    // byte-transparent so OSC-1337 / Kitty graphics pass through untouched —
    // tmux corrupts the Kitty protocol, which is why neither branch uses it.
    // Hosts with neither exec croft directly, no persistence.
    // `CROFT_SESSION_PERSISTENT=1` is exported only on the persistent
    // branches so the remote croft can tell whether its session survives a
    // transport drop and surface that on its status line.
    let socket = dtach_socket_path(path);
    let mux_socket = mux_socket_path(path);
    let workspace_flag = match path.filter(|p| !p.is_empty()) {
        Some(p) => format!(" --workspace {}", shell_quote(p)),
        None => String::new(),
    };
    format!(
        "{prefix}if croft session-host --probe >/dev/null 2>&1; then mkdir -p \"$(dirname \"{mux_socket}\")\"; export CROFT_SESSION_PERSISTENT=1; exec croft session-host --socket \"{mux_socket}\"{workspace_flag} -- {croft_invocation}; elif command -v dtach >/dev/null 2>&1; then mkdir -p \"$(dirname \"{socket}\")\"; export CROFT_SESSION_PERSISTENT=1; exec dtach -A \"{socket}\" -E -z -r winch {croft_invocation}; else exec {croft_invocation}; fi"
    )
}

/// Remote path of the dtach control socket for a workspace. Keyed on a stable
/// hash of the workspace path so a reconnect reattaches the same session.
/// `DefaultHasher` uses fixed SipHash keys, so the name is deterministic across
/// croft processes (a randomized hasher would orphan the session on relaunch).
fn dtach_socket_path(path: Option<&str>) -> String {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(path.unwrap_or("").as_bytes());
    format!("$HOME/.cache/croft/sessions/{:016x}.sock", hasher.finish())
}

/// Remote socket for a mux (session-host) session: same keying as the dtach
/// socket (the raw launch arg, per the relay-key post-mortem above
/// `relay_session_id`) but a distinct name, so a mux client never speaks
/// croft's frame protocol at a live dtach server left over from an older
/// binary.
fn mux_socket_path(path: Option<&str>) -> String {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(path.unwrap_or("").as_bytes());
    format!(
        "$HOME/.cache/croft/sessions/{:016x}.mux.sock",
        hasher.finish()
    )
}

/// Remote socket carrying Phase D collab ops between independent-viewport
/// participants (never PTY bytes): same keying as the dtach and mux sockets,
/// its own endpoint (see docs/MULTIPLAYER.md).
fn collab_socket_path(path: Option<&str>) -> String {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(path.unwrap_or("").as_bytes());
    format!(
        "$HOME/.cache/croft/sessions/{:016x}.collab.sock",
        hasher.finish()
    )
}

/// True for ssh's own connection-failure exit code (255), as opposed to any
/// status the remote croft itself returned. Only this warrants an auto-
/// reconnect; a real remote crash (e.g. 101) must surface to the user.
fn is_transport_failure(code: Option<i32>) -> bool {
    code == Some(255)
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
if pgrep -x croft >/dev/null 2>&1; then
  CROFT_JOBS=1
fi
CROFT_NICE=""
if command -v nice >/dev/null 2>&1; then CROFT_NICE="nice -n 19"; fi
CROFT_IONICE=""
if command -v ionice >/dev/null 2>&1; then CROFT_IONICE="ionice -c3"; fi
# eval, because this script runs under the remote user's login shell and
# zsh does not word-split unquoted parameters: bare `$CROFT_NICE ...` would
# try to run a command literally named "nice -n 19". eval re-parses the
# assembled line, which splits correctly under both sh/bash and zsh.
eval "$CROFT_NICE $CROFT_IONICE"' cargo install --path "$HOME/.cache/croft/source" --jobs "$CROFT_JOBS" --force --locked'
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
    source_stamp_for(&PathBuf::from(env!("CARGO_MANIFEST_DIR")))
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
fn ssh_control_socket_path_for_test(dir: &Path) -> PathBuf {
    dir.join("ctl")
}

#[cfg(test)]
mod tests {

    /// The destination an ssh command line names, from the shapes people
    /// actually type (#364).
    ///
    /// Everything here is a real invocation rather than a synthetic one,
    /// because the whole difficulty is that `ssh` takes flags with separate
    /// arguments — `-p 2222`, `-i key`, `-o Opt=v` — and a parser that does
    /// not know which flags consume the next word will take `2222` as the
    /// host.
    #[test]
    fn an_ssh_command_line_yields_its_destination() {
        let d = |cmd: &[&str]| ssh_destination(cmd);
        assert_eq!(d(&["ssh", "box"]).as_deref(), Some("box"));
        assert_eq!(d(&["ssh", "user@box"]).as_deref(), Some("user@box"));
        // A flag that takes a separate argument must not donate it.
        assert_eq!(d(&["ssh", "-p", "2222", "box"]).as_deref(), Some("box"));
        assert_eq!(
            d(&["ssh", "-i", "~/.ssh/id", "box"]).as_deref(),
            Some("box")
        );
        assert_eq!(
            d(&["ssh", "-o", "StrictHostKeyChecking=no", "box"]).as_deref(),
            Some("box")
        );
        // Bundled boolean flags take no argument.
        assert_eq!(d(&["ssh", "-4qt", "box"]).as_deref(), Some("box"));
        // An attached value (`-p2222`) consumes nothing further.
        assert_eq!(d(&["ssh", "-p2222", "box"]).as_deref(), Some("box"));
        // Every arg-taking flag in ssh(1)'s synopsis, each given a value that
        // would be taken as the host if the flag were missing from the list.
        // `-P` was missing on the first pass, which is exactly this bug.
        for f in [
            "B", "b", "c", "D", "E", "e", "F", "I", "i", "J", "L", "l", "m", "O", "o", "P", "p",
            "Q", "R", "S", "W", "w",
        ] {
            let flag = format!("-{f}");
            assert_eq!(
                d(&["ssh", &flag, "decoy", "box"]).as_deref(),
                Some("box"),
                "-{f} swallowed its argument's place"
            );
        }
        // The FIRST non-flag word is the destination; anything after it is
        // the remote command, not another host.
        assert_eq!(
            d(&["ssh", "box", "tail", "-f", "log"]).as_deref(),
            Some("box")
        );
        // `--` ends option parsing.
        assert_eq!(d(&["ssh", "--", "box"]).as_deref(), Some("box"));
        assert_eq!(d(&["ssh", "-4", "--", "box"]).as_deref(), Some("box"));
        // An absolute path invocation is still ssh.
        assert_eq!(d(&["/usr/bin/ssh", "box"]).as_deref(), Some("box"));
        // `--` ends option parsing, so even a host spelled like a flag is
        // taken verbatim — which is what real ssh does.
        assert_eq!(d(&["ssh", "--", "-4"]).as_deref(), Some("-4"));
    }

    /// The offer's whole decision, as one pure function (#364).
    ///
    /// Kept pure so the interesting cases can be swept without a live ssh
    /// session: what the user sees is decided here, and the app layer only
    /// samples argv and renders the result.
    #[test]
    fn the_offer_appears_only_for_a_known_host_in_an_ssh_pane() {
        let targets = parse_ssh_config("Host db-1\n  HostName 10.0.0.4\n");
        let o = |cmd: &[&str]| {
            ssh_reroot_decision(cmd, &targets)
                .ok()
                .map(|h| h.alias.clone())
        };

        assert_eq!(o(&["ssh", "db-1"]).as_deref(), Some("db-1"));
        assert_eq!(
            o(&["ssh", "-p", "2222", "deploy@10.0.0.4"]).as_deref(),
            Some("db-1")
        );

        // A shell is not an ssh session.
        assert_eq!(o(&["zsh"]), None);
        // An ssh session to a box croft has no config entry for: no offer,
        // because the remote flow it would hand off to is keyed on one.
        assert_eq!(o(&["ssh", "unknown-box"]), None);
        // ssh with no destination yet — the user is still typing.
        assert_eq!(o(&["ssh"]), None);
        // sshfs is a mount, not a session; offering to re-root onto it would
        // answer a question the user did not ask.
        assert_eq!(o(&["sshfs", "db-1:/srv", "/mnt"]), None);
    }

    /// A destination is matched to a configured host, and the user part is
    /// not part of the identity (#364).
    #[test]
    fn a_destination_resolves_against_the_configured_hosts() {
        let targets = parse_ssh_config(
            "Host db-1\n  HostName 10.0.0.4\n  User deploy\n\nHost web\n  HostName web.example\n",
        );
        let m = |dest: &str| resolve_offer_host(dest, &targets).map(|t| t.alias.clone());

        assert_eq!(m("db-1").as_deref(), Some("db-1"));
        // `user@alias` names the same box: the user is how you log in, not
        // which machine it is.
        assert_eq!(m("deploy@db-1").as_deref(), Some("db-1"));
        assert_eq!(m("root@db-1").as_deref(), Some("db-1"));
        // The HostName resolves too — people type either.
        assert_eq!(m("10.0.0.4").as_deref(), Some("db-1"));
        assert_eq!(m("web.example").as_deref(), Some("web"));
        // Aliases are case-insensitive, as ssh treats them.
        assert_eq!(m("DB-1").as_deref(), Some("db-1"));

        // A host croft knows nothing about gets no offer: the prompt exists
        // to reuse the remote machinery, which is keyed on a config entry.
        assert_eq!(m("some-random-box"), None);
        assert_eq!(m(""), None);
        // A port suffix is not part of the host name.
        assert_eq!(m("db-1:22"), None);

        // An entry's own alias beats an EARLIER entry's HostName. Testing
        // both in one pass let `jump` win here, so `ssh db-1` offered to
        // re-root onto a different machine than the one named.
        let shadowed =
            parse_ssh_config("Host jump\n  HostName db-1\n\nHost db-1\n  HostName 10.0.0.4\n");
        assert_eq!(
            resolve_offer_host("db-1", &shadowed)
                .map(|t| t.alias.clone())
                .as_deref(),
            Some("db-1"),
            "an alias must beat another entry's HostName"
        );
        // The HostName fallback still works when no alias matches.
        assert_eq!(
            resolve_offer_host("10.0.0.4", &shadowed)
                .map(|t| t.alias.clone())
                .as_deref(),
            Some("db-1")
        );
    }

    /// Every outcome the user can be told, decided in one place.
    #[test]
    fn the_decision_names_the_host_or_says_why_not() {
        let targets = parse_ssh_config("Host db-1\n  HostName 10.0.0.4\n");
        let d = |argv: &[&str]| ssh_reroot_decision(argv, &targets).map(|t| t.alias.clone());
        assert_eq!(d(&["ssh", "db-1"]), Ok(String::from("db-1")));

        // A known shape of failure names the host, because that is the half
        // the user can act on — add it to ~/.ssh/config and try again.
        let err = d(&["ssh", "other-box"]).unwrap_err();
        assert!(err.contains("other-box"), "unhelpful: {err}");
        assert!(err.contains("ssh/config"), "unhelpful: {err}");

        // Not an ssh session at all is a different message: there is no host
        // to name, and saying one is missing would be misleading.
        let err = d(&["zsh"]).unwrap_err();
        assert_eq!(err, "This pane is not an SSH session");
        assert!(!err.contains("ssh/config"));
    }

    /// mosh and et are refused rather than mis-parsed.
    ///
    /// They reach a box the same way and were once in `SSH_PROGRAMS`, but
    /// they take LONG options and ssh's single-letter grammar mis-reads
    /// those: `--port` scans as `-port`, finds `p` mid-word, consumes
    /// nothing, and the VALUE becomes the host. A wrong host re-roots onto
    /// the wrong machine, so refusing is the only honest answer until a
    /// per-program table exists. Pinned here so re-adding them without that
    /// table fails rather than silently regressing.
    #[test]
    fn a_long_option_program_is_refused_rather_than_misparsed() {
        let d = |cmd: &[&str]| ssh_destination(cmd);
        for cmd in [
            vec!["mosh", "box"],
            vec!["mosh", "--port", "60000", "box"],
            vec!["mosh", "--ssh", "ssh -p 2222", "box"],
            vec!["et", "box"],
            vec!["et", "--port", "2022", "box"],
        ] {
            assert_eq!(
                d(&cmd),
                None,
                "{cmd:?} must be refused while the grammar cannot parse it"
            );
        }
    }

    /// Things that look like an ssh session and are not one.
    #[test]
    fn a_non_ssh_command_line_yields_nothing() {
        let d = |cmd: &[&str]| ssh_destination(cmd);
        for cmd in [
            vec!["zsh"],
            vec!["ssh"],               // no destination yet
            vec!["ssh", "-p", "2222"], // flag ate the last word
            vec!["ssh-agent"],         // not ssh
            vec!["ssh-keygen", "-t", "ed25519"],
            vec!["sshfs", "box:/srv", "/mnt"], // a mount, not a session
            vec!["git", "push", "ssh://box/repo"],
            vec![],
        ] {
            assert_eq!(d(&cmd), None, "must not match: {cmd:?}");
        }
    }

    // The destination host must come BEFORE the script. ssh reads the first
    // non-flag argument as the destination, so a missing host does not fail
    // loudly - it turns the command itself into the hostname, the connection
    // fails, and whatever depended on it silently does nothing. That is
    // exactly what happened to config sync: `mkdir -p ~/.config/croft` became
    // the host, the mkdir "failed", and the feature returned before pushing a
    // single file.
    #[test]
    fn a_background_shell_puts_the_host_before_the_script() {
        let ssh = SshControl {
            host: String::from("somebox"),
            socket_dir: PathBuf::from("/tmp/x"),
            socket_path: PathBuf::from("/tmp/x/ctl"),
        };
        let cmd = ssh.background_shell("mkdir -p ~/.config/croft");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let host_at = args.iter().position(|a| a == "somebox");
        let script_at = args.iter().position(|a| a == "mkdir -p ~/.config/croft");
        assert!(
            host_at.is_some(),
            "the destination host must be passed: {args:?}"
        );
        assert!(script_at.is_some(), "the script must be passed: {args:?}");
        assert!(
            host_at < script_at,
            "ssh takes the destination BEFORE the command, or it reads the \
             command as the hostname: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "-n"),
            "a background command must not inherit stdin: {args:?}"
        );
    }

    use super::*;

    /// The relay-setup failure message carries ssh's own stderr when there is
    /// one, and never ends in a dangling ": " when there is not.
    #[test]
    #[cfg(unix)]
    fn relay_setup_error_appends_stderr_only_when_present() {
        use std::os::unix::process::ExitStatusExt as _;
        let status = ExitStatus::from_raw(256); // exit code 1
        assert_eq!(
            relay_setup_error(status, b"  ssh: connect refused \n"),
            format!("remote relay setup failed with {status}: ssh: connect refused")
        );
        assert_eq!(
            relay_setup_error(status, b" \n"),
            format!("remote relay setup failed with {status}")
        );
        assert!(!relay_setup_error(status, b"").ends_with(": "));
    }

    /// The source sync must ship the BUILD INPUTS and nothing else.
    ///
    /// It used to deny-list `target`, which never matched this repo's actual
    /// build directory, `target.noindex`. Every fallback install therefore
    /// rsynced 176 GB of build artifacts to the user's box, throttled to a
    /// couple of MB/s, to deliver a source tree whose only job was to be
    /// compiled there. Three boxes were 23-27 GB deep in it before anyone
    /// noticed. The stamp learned this lesson already (SOURCE_STAMP_INPUTS);
    /// the rsync did not, so now they share one list and cannot drift apart.
    #[test]
    fn the_source_sync_ships_only_build_inputs() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("checkout");
        let dst = tmp.path().join("staged");
        for dir in [
            "src",
            "assets",
            "target.noindex/debug",
            "target/debug",
            ".git",
        ] {
            std::fs::create_dir_all(src.join(dir)).unwrap();
        }
        std::fs::write(src.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(src.join("assets/logo.png"), "png").unwrap();
        std::fs::write(src.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(src.join("Cargo.lock"), "lock").unwrap();
        std::fs::write(src.join("build.rs"), "fn main() {}").unwrap();
        std::fs::write(src.join("rust-toolchain.toml"), "[toolchain]").unwrap();
        // The three things that must never cross the wire.
        std::fs::write(src.join("target.noindex/debug/croft"), "HUGE").unwrap();
        std::fs::write(src.join("target/debug/croft"), "HUGE").unwrap();
        std::fs::write(src.join(".git/config"), "gitdir").unwrap();
        // Nested junk: a root-anchored allow-list alone re-admits these,
        // because `/assets/***` beats `--exclude=*`. The stamp walk skips them
        // at every level, so the sync has to as well or the two disagree.
        std::fs::write(src.join("assets/.DS_Store"), "finder").unwrap();
        std::fs::create_dir_all(src.join("src/vendor/target")).unwrap();
        std::fs::write(src.join("src/vendor/target/blob"), "HUGE").unwrap();

        let mut rsync = Command::new("rsync");
        rsync.arg("-a").args(source_sync_filter_args());
        rsync
            .arg(format!("{}/", src.display()))
            .arg(format!("{}/", dst.display()));
        let Ok(status) = rsync.status() else {
            return; // no rsync on this box; the remote path is unusable anyway
        };
        assert!(status.success(), "rsync exited with {status}");

        for shipped in [
            "src/main.rs",
            "assets/logo.png",
            "Cargo.toml",
            "Cargo.lock",
            "build.rs",
            "rust-toolchain.toml",
        ] {
            assert!(dst.join(shipped).exists(), "{shipped} must be shipped");
        }
        for withheld in [
            "target.noindex",
            "target",
            ".git",
            "assets/.DS_Store",
            "src/vendor/target",
        ] {
            assert!(
                !dst.join(withheld).exists(),
                "{withheld} must never be shipped: this is the 176 GB bug"
            );
        }
    }

    /// Stage a remote source dir holding both legitimate content and every
    /// kind of junk a previous croft could have left behind.
    fn staged_remote_source(home: &std::path::Path) -> std::path::PathBuf {
        let source = home.join(".cache/croft/source");
        std::fs::create_dir_all(source.join("target/remote-fast")).unwrap();
        std::fs::create_dir_all(source.join("target.noindex/debug")).unwrap();
        std::fs::create_dir_all(source.join(".cargo")).unwrap();
        std::fs::write(source.join(".cargo/config.toml"), "[build]\njobs = 1\n").unwrap();
        std::fs::write(source.join("build.rs"), "fn main() {}").unwrap();
        std::fs::create_dir_all(source.join("src")).unwrap();
        std::fs::write(source.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(source.join("Cargo.toml"), "[package]\n").unwrap();
        source
    }

    fn assert_prep_outcome(source: &std::path::Path) {
        assert!(
            source.join("target/remote-fast").is_dir(),
            "removing the remote's own target/ would force a from-scratch rebuild every install"
        );
        assert!(
            source.join("src/main.rs").is_file() && source.join("Cargo.toml").is_file(),
            "inputs present in the local checkout must survive: deleting them re-ships \
             everything and yanks the tree out from under a concurrent cargo install"
        );
        assert!(
            !source.join("target.noindex").exists(),
            "the artifact tree a previous croft shipped must be reclaimed"
        );
        assert!(
            !source.join(".cargo").exists(),
            "a stale dotfile like .cargo/config.toml would silently reconfigure every later remote build"
        );
        assert!(
            !source.join("build.rs").exists(),
            "an input no longer in the local checkout must not survive on the remote (tar never deletes)"
        );
    }

    /// rsync will not clean up what a previous croft already shipped: the
    /// allow-list's `--exclude=*` PROTECTS every unlisted path from
    /// `--delete`, and the tar fallback never deletes anything. So the prep
    /// must clear the remote source dir of everything except `target/` (the
    /// incremental build cache) and the inputs the local checkout actually
    /// contains - shipped artifact trees, a stale `.cargo/config.toml` that
    /// would silently reconfigure every later remote build, and inputs since
    /// removed from the checkout all go. Current inputs stay: deleting them
    /// would re-ship the whole tree every sync and race a concurrent
    /// `cargo install` reading it. The test runs the real command against a
    /// staged HOME.
    #[test]
    fn the_remote_dir_prep_evicts_stale_entries_but_keeps_cache_and_inputs() {
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(checkout.path().join("src")).unwrap();
        std::fs::write(checkout.path().join("Cargo.toml"), "[package]\n").unwrap();
        // No build.rs in this checkout: the staged remote copy is stale.
        let prep = remote_source_dir_prep(checkout.path());

        let tmp = tempfile::tempdir().unwrap();
        let source = staged_remote_source(tmp.path());
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&prep)
            .env("HOME", tmp.path())
            .status()
            .unwrap();
        assert!(status.success(), "the prep command must run under plain sh");
        assert_prep_outcome(&source);

        let empty = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&prep)
            .env("HOME", empty.path())
            .status()
            .unwrap();
        assert!(status.success(), "a first install has no source dir yet");
    }

    /// A relocated cache is ordinary on a small-root VPS: the source dir is a
    /// symlink onto a bigger volume. `find` does not follow a symlinked start
    /// point on its own, silently cleaning nothing with a success exit code,
    /// so the prep must dereference it explicitly.
    #[test]
    fn the_remote_dir_prep_follows_a_symlinked_source_dir() {
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(checkout.path().join("src")).unwrap();
        std::fs::write(checkout.path().join("Cargo.toml"), "[package]\n").unwrap();
        let prep = remote_source_dir_prep(checkout.path());

        let tmp = tempfile::tempdir().unwrap();
        let real = tempfile::tempdir().unwrap();
        let source = staged_remote_source(real.path());
        std::fs::create_dir_all(tmp.path().join(".cache/croft")).unwrap();
        std::os::unix::fs::symlink(
            real.path().join(".cache/croft/source"),
            tmp.path().join(".cache/croft/source"),
        )
        .unwrap();

        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&prep)
            .env("HOME", tmp.path())
            .status()
            .unwrap();
        assert!(status.success(), "the prep command must run under plain sh");
        assert_prep_outcome(&source);
    }

    /// The tar fallback (no rsync on either side) shipped `.` under the same
    /// broken `--exclude=target`, so it packed the whole artifact tree into a
    /// gzip pipe - worse than the rsync path, and reached on exactly the same
    /// first-install flow. It names its members from the allow-list now.
    #[test]
    fn the_tar_fallback_packs_only_build_inputs() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("checkout");
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::create_dir_all(src.join("target.noindex")).unwrap();
        std::fs::write(src.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(src.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(src.join("target.noindex/blob"), "HUGE").unwrap();

        let members = source_sync_tar_members(&src);
        assert!(members.contains(&"src"), "src is a build input");
        assert!(
            members.contains(&"Cargo.toml"),
            "Cargo.toml is a build input"
        );
        assert!(
            !members.iter().any(|m| m.starts_with("target")),
            "the artifact tree must never be packed"
        );
        assert!(
            !members.contains(&"build.rs"),
            "a member that does not exist must not be named, or tar fails"
        );
    }

    /// A rustup target belongs to ONE toolchain. Bumping `rust-toolchain.toml`
    /// therefore orphans every cross target, and `croft <host>` degrades from
    /// "ship a 34 MB prebuilt binary" to "compile the whole crate graph on the
    /// user's VPS" with nothing but a line in `~/.cache/croft/install.log` to
    /// say so. That is exactly what the 1.95.0 -> 1.97.1 bump did, unnoticed
    /// for four days. This test is the thing that notices.
    #[test]
    fn the_pinned_toolchain_has_every_cross_target() {
        // Only machines that have actually set up the cross fast path are held
        // to it. A fresh clone, a CI lint runner, Termux, a Nix shell with no
        // zig: none of them ship remote installs, and failing their `cargo
        // test` over a target they will never use is noise, not a signal. The
        // skip is announced, because a guard that vanishes silently is how the
        // original guard managed to be useless.
        if let Some(reason) = cross_compile_unavailable_reason() {
            eprintln!("SKIP the_pinned_toolchain_has_every_cross_target: {reason}");
            return;
        }
        let Ok(out) = cross_tool_command_in_checkout("rustup")
            .args(["target", "list", "--installed"])
            .output()
        else {
            eprintln!("SKIP the_pinned_toolchain_has_every_cross_target: rustup would not run");
            return;
        };
        if !out.status.success() {
            eprintln!("SKIP the_pinned_toolchain_has_every_cross_target: rustup errored");
            return;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let installed: Vec<&str> = text.lines().map(str::trim).collect();
        let missing: Vec<&str> = crate::cli::CROSS_TARGETS
            .iter()
            .copied()
            .filter(|t| !installed.contains(t))
            .collect();
        assert!(
            missing.is_empty(),
            "the pinned toolchain is missing {missing:?}, so every remote \
             update will compile croft on the remote box instead of shipping \
             a prebuilt binary. Fix from inside this checkout, so the \
             rust-toolchain.toml override applies:\n    rustup target add {}",
            missing.join(" ")
        );
    }

    /// The target query and the build must ask the SAME toolchain.
    /// `cargo zigbuild` runs in the checkout, where `rust-toolchain.toml` pins
    /// the channel; a rustup query with no working directory answers for the
    /// DEFAULT channel instead. When those differ the guard reports "target
    /// installed" and the build dies with E0463, which is why a missing target
    /// went unreported for four days.
    ///
    /// The plain probes must NOT inherit that directory: their answer does not
    /// depend on the toolchain, and running `cargo --version` inside the
    /// checkout makes an availability check auto-install the pinned channel.
    #[test]
    fn only_toolchain_sensitive_calls_run_in_the_checkout() {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            cross_tool_command_in_checkout("rustup").get_current_dir(),
            Some(source.as_path()),
            "a target query must resolve the pinned toolchain"
        );
        assert_eq!(
            cross_tool_command("cargo").get_current_dir(),
            None,
            "a presence probe must not trigger a toolchain auto-install"
        );
    }

    #[test]
    fn source_stamp_tracks_build_inputs_only() {
        // The stamp decides whether a remote reinstall is needed, so it must
        // cover exactly the inputs that shape the shipped binary. Build
        // artifacts (`target.noindex` reached 98 GB) and docs must not feed
        // it: hashing them made every connect stall for a minute and reship
        // croft even when the source was untouched.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target.noindex")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("target.noindex/dep.rlib"), "artifact-v1").unwrap();
        std::fs::write(root.join("docs/NOTES.md"), "notes-v1").unwrap();
        let base = source_stamp_for(&root).unwrap();

        std::fs::write(root.join("target.noindex/dep.rlib"), "artifact-v2").unwrap();
        std::fs::write(root.join("docs/NOTES.md"), "notes-v2").unwrap();
        assert_eq!(
            source_stamp_for(&root).unwrap(),
            base,
            "build artifacts and docs must not change the source stamp"
        );

        std::fs::write(root.join("src/main.rs"), "fn main() { changed(); }").unwrap();
        assert_ne!(
            source_stamp_for(&root).unwrap(),
            base,
            "a source edit must change the stamp"
        );
    }

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
        // A workspace path with a single quote must be shell-quoted in the
        // croft invocation (which now appears in both the dtach and the
        // direct-exec branch).
        let command = remote_croft_command_for_terminal(
            Some("/tmp/it's here"),
            None,
            None,
            false,
            &[],
            false,
        );
        assert!(command.contains("croft '/tmp/it'\"'\"'s here'"));
        assert!(command.starts_with("export CROFT_REMOTE_AUTOUPDATE=1;"));
    }

    #[test]
    fn remote_croft_command_forwards_supported_terminal_program() {
        let command =
            remote_croft_command_for_terminal(None, Some("iTerm.app"), None, false, &[], false);
        assert!(command.contains("export CROFT_FORCE_INLINE_IMAGES=1 TERM_PROGRAM='iTerm.app';"));
    }

    #[test]
    fn remote_croft_command_exports_kitty_hint_for_ghostty() {
        // Ghostty: TERM_PROGRAM=ghostty locally, but SSH does not forward it, so
        // croft must export the hint itself. It must NOT force the OSC-1337 path
        // (Ghostty parses but ignores it); the remote resolves the Kitty
        // protocol from the exported TERM_PROGRAM.
        let command =
            remote_croft_command_for_terminal(None, Some("ghostty"), None, false, &[], false);
        assert!(command.contains("export TERM_PROGRAM=ghostty;"));
        assert!(!command.contains("CROFT_FORCE_INLINE_IMAGES"));
    }

    #[test]
    fn remote_croft_command_exports_kitty_hint_from_term_only() {
        // A bare `kitty` terminal sets no TERM_PROGRAM; detection keys off TERM.
        let command =
            remote_croft_command_for_terminal(None, None, Some("xterm-kitty"), false, &[], false);
        assert!(command.contains("export TERM_PROGRAM=ghostty;"));
    }

    #[test]
    fn remote_launch_carries_only_a_deterministic_relay_key() {
        // The relay rendezvous reaches the long-lived remote croft as the
        // deterministic `CROFT_RELAY_KEY` env var, NOT the old per-connection
        // `CROFT_DROP_RELAY_*` paths. Because the key is `hash(launch arg)` it is
        // identical on every reconnect, so dtach freezing it at first launch can
        // never desync the running croft from a fresh pump.
        let id = relay_session_id("");
        let env = vec![(String::from("CROFT_RELAY_KEY"), id.clone())];
        let command = remote_croft_command_for_terminal(None, None, None, false, &env, false);
        assert!(
            command.contains(&format!("export CROFT_RELAY_KEY='{id}'")),
            "launch must export the deterministic relay key, got: {command}",
        );
        assert!(
            !command.contains("CROFT_DROP_RELAY"),
            "launch must not use the old per-connection relay env, got: {command}",
        );
    }

    #[test]
    fn relay_session_id_matches_dtach_socket_and_is_deterministic() {
        // The relay id is keyed on the launch arg, exactly like the dtach socket,
        // so `relay-<id>` and `sessions/<id>.sock` share one id and the two sides
        // always agree across reconnects. It must be deterministic (same arg ->
        // same id) and arg-sensitive; the random per-connection id it replaced
        // silently broke every drop after the first reconnect.
        assert_eq!(relay_session_id(""), relay_session_id(""));
        assert_ne!(relay_session_id("/srv/app"), relay_session_id("/srv/other"));
        // Same hash the dtach socket path embeds for the same launch arg.
        assert!(dtach_socket_path(None).contains(&relay_session_id("")));
        assert!(dtach_socket_path(Some("/srv/app")).contains(&relay_session_id("/srv/app")));
    }

    #[test]
    fn cross_tool_search_dirs_cover_gui_launch_tool_locations() {
        // launchd hands a Croft.app / Ghostty launch a stripped PATH; the
        // toolchain lives in dirs that PATH omits, so the search must add them.
        let launchd_path = OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");
        let home = PathBuf::from("/Users/example");
        let dirs =
            cross_tool_search_dirs_from(Some(launchd_path.as_os_str()), Some(home.as_os_str()));

        assert_eq!(dirs.first(), Some(&PathBuf::from("/usr/bin")));
        assert!(
            dirs.contains(&home.join(".cargo").join("bin")),
            "cargo, rustup, and cargo-zigbuild installed by rustup live here, but launchd omits it"
        );
        assert!(
            dirs.contains(&PathBuf::from("/opt/homebrew/bin")),
            "Apple Silicon Homebrew's zig lives here, but launchd omits it"
        );
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
    }

    #[test]
    fn cross_tool_lookup_finds_home_cargo_bin_outside_inherited_path() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join(".cargo").join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let tool = bin_dir.join("cargo-zigbuild");
        std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();

        let launchd_path = OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");
        let dirs = cross_tool_search_dirs_from(
            Some(launchd_path.as_os_str()),
            Some(tmp.path().as_os_str()),
        );

        assert_eq!(
            find_executable_in_dirs("cargo-zigbuild", &dirs),
            Some(tool),
            "the remote fast path must work when Croft.app starts with launchd's stripped PATH"
        );
    }

    #[test]
    fn remote_croft_command_forwards_osk_flag_when_local_osk_is_armed() {
        // SSH from Termux does not forward TERMUX_VERSION, so a remote croft
        // would never auto-arm the on-screen keyboard; the launcher carries
        // the local detection across the hop explicitly.
        let command = remote_croft_command_for_terminal(None, None, None, true, &[], false);
        assert!(command.contains("export CROFT_FORCE_OSK=1;"));
        let command = remote_croft_command_for_terminal(None, None, None, false, &[], false);
        assert!(!command.contains("CROFT_FORCE_OSK"));
    }

    #[test]
    fn remote_command_prefers_session_host_and_falls_back_to_dtach() {
        let command =
            remote_croft_command_for_terminal(Some("/srv/app"), None, None, false, &[], false);
        // Preferred: croft's own session host (multiplayer mux), gated on a
        // probe so an old remote binary is never sent an unknown subcommand.
        assert!(command.contains("if croft session-host --probe"));
        assert!(command.contains("exec croft session-host --socket"));
        assert!(command.contains(".mux.sock"));
        assert!(command.contains("--workspace '/srv/app' -- croft '/srv/app'"));
        // Fallback: dtach with the exact flags croft needs (attach-or-create,
        // no detach/suspend key theft, WINCH redraw on reattach).
        assert!(command.contains("elif command -v dtach"));
        assert!(command.contains("dtach -A"));
        assert!(command.contains("-E"));
        assert!(command.contains("-z"));
        assert!(command.contains("-r winch"));
        // Both persistent branches flag the session for the status line.
        assert!(command.contains("export CROFT_SESSION_PERSISTENT=1;"));
        // Hosts with neither supervisor still launch croft directly.
        assert!(command.contains("else exec croft"));
    }

    #[test]
    fn remote_command_omits_workspace_flag_without_a_path() {
        let command = remote_croft_command_for_terminal(None, None, None, false, &[], false);
        assert!(command.contains("exec croft session-host --socket"));
        assert!(!command.contains("--workspace"));
        assert!(command.contains(" -- croft;"));
    }

    #[test]
    fn dtach_socket_is_stable_per_workspace_and_differs_across_paths() {
        let a1 = dtach_socket_path(Some("/srv/app"));
        let a2 = dtach_socket_path(Some("/srv/app"));
        let b = dtach_socket_path(Some("/srv/other"));
        assert_eq!(a1, a2, "same workspace must map to the same dtach session");
        assert_ne!(a1, b, "different workspaces must not share a session");
        assert!(a1.contains("/.cache/croft/sessions/"));
        // The mux socket shares the keying but never the name, so a mux
        // client can never connect to a live legacy dtach server.
        let m = mux_socket_path(Some("/srv/app"));
        assert_ne!(m, a1);
        assert!(m.ends_with(".mux.sock"));
        assert!(m.contains(&relay_session_id("/srv/app")));
        // The collab socket (Phase D op relay) shares the keying too but is
        // its own endpoint: it never carries PTY bytes.
        let c = collab_socket_path(Some("/srv/app"));
        assert_ne!(c, a1);
        assert_ne!(c, m);
        assert!(c.ends_with(".collab.sock"));
        assert!(c.contains(&relay_session_id("/srv/app")));
    }

    #[test]
    fn remote_command_solo_skips_the_mux_and_wires_the_collab_relay() {
        let command = remote_croft_command_for_terminal(
            Some("/srv/app"),
            Some("iTerm.app"),
            Some("xterm-256color"),
            false,
            &[],
            true,
        );
        // A solo guest never attaches the shared PTY.
        assert!(!command.contains("session-host --socket"));
        assert!(!command.contains("dtach -A"));
        // It probes for collab support (old remote binaries fall back to a
        // plain croft), ensures the relay, and exports the channel env.
        assert!(command.contains("if croft collab-relay --probe"));
        assert!(command.contains("collab-relay --ensure --socket"));
        assert!(command.contains(&collab_socket_path(Some("/srv/app"))));
        assert!(command.contains("export CROFT_COLLAB_SOCKET="));
        assert!(command.contains("export CROFT_COLLAB_ROLE=guest;"));
        assert!(command.contains("exec croft '/srv/app'"));
        // Non-solo stays on the shared-session path, untouched.
        let shared = remote_croft_command_for_terminal(
            Some("/srv/app"),
            Some("iTerm.app"),
            Some("xterm-256color"),
            false,
            &[],
            false,
        );
        assert!(shared.contains("if croft session-host --probe"));
        assert!(!shared.contains("CROFT_COLLAB_ROLE"));
    }

    #[test]
    fn transport_failure_is_exit_255_only() {
        // 255 is ssh's own connection-died code; everything else is a real
        // remote exit and must not trigger an auto-reconnect.
        assert!(is_transport_failure(Some(255)));
        assert!(!is_transport_failure(Some(0)));
        assert!(!is_transport_failure(Some(1)));
        assert!(!is_transport_failure(Some(101)));
        assert!(!is_transport_failure(None));
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
    fn parse_relay_request_forward_extracts_port_and_open_flag() {
        match super::parse_relay_request("forward\tfwd-1\t3000\t1") {
            Some(super::RelayRequest::Forward { id, port, open }) => {
                assert_eq!(id, "fwd-1");
                assert_eq!(port, "3000");
                assert!(open);
            }
            _ => panic!("expected Forward variant"),
        }
    }

    #[test]
    fn parse_relay_request_forward_defaults_open_to_false() {
        match super::parse_relay_request("forward\tfwd-2\t8080") {
            Some(super::RelayRequest::Forward { open, .. }) => assert!(!open),
            _ => panic!("expected Forward variant"),
        }
        assert!(super::parse_relay_request("forward\tfwd-3\t").is_none());
    }

    #[test]
    fn pick_local_port_mirrors_a_free_port() {
        // Port 0 can never be mirrored (it's the "any" sentinel), but a high
        // port is almost certainly free in the test environment.
        let p = super::pick_local_port(54321);
        assert!(p == 54321 || p >= 1024, "got {p}");
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
        tee.send(String::from("Cross-compiling croft locally"))
            .unwrap();
        tee.send(String::from("Local cross-build skipped (zig missing)"))
            .unwrap();
        let timeout = std::time::Duration::from_secs(2);
        assert_eq!(
            down_rx.recv_timeout(timeout).unwrap(),
            "Cross-compiling croft locally",
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
        assert!(contents.contains("Cross-compiling croft locally"));
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

    // The other half of the launch-now bug class. Bytes are not the only
    // shared resource: ssh forwards the caller's stdin to the remote command
    // unless `-n` is given, and on this path that stdin is the terminal the
    // attached remote croft is reading. Two processes then read() the same
    // tty and each keystroke reaches exactly one of them, so typing into the
    // remote session silently vanishes for as long as the background command
    // runs. Verified against a real host: `ssh h 'sleep 1' < probe` drains
    // all 16 bytes even though the remote command never reads stdin, while
    // `ssh -n h 'sleep 1' < probe` leaves every byte for the next reader.
    #[test]
    fn background_ssh_never_reads_the_attached_sessions_stdin() {
        let socket = PathBuf::from("/tmp/croft-ctl/ctl");
        let background = args_of(&ssh_socket_command(&socket, true));
        assert!(
            background.iter().any(|a| a == "-n"),
            "a background command sharing the session's tty steals its keystrokes"
        );
        let interactive = args_of(&ssh_socket_command(&socket, false));
        assert!(
            !interactive.iter().any(|a| a == "-n"),
            "the interactive session is the one process that must keep the tty"
        );
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
        let cmd = ship_file_rsync_command(
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
    fn a_config_push_rides_the_bulk_lane_like_the_binary_does() {
        // Config files are small, but they ship on the same connection as
        // the live session; an unpaced push still queues ahead of keystrokes.
        let lane = crate::remote_bulk::BulkLane::new(
            crate::remote_bulk::LaneMode::Dedicated {
                socket_path: PathBuf::from("/tmp/bulk/ctl"),
            },
            512,
        );
        let cmd = ship_file_rsync_command(
            &lane,
            Path::new("/tmp/interactive/ctl"),
            Path::new("/home/u/.config/croft/keybindings.json"),
            &crate::config_sync::remote_dest("host", "keybindings.json"),
        );
        let args = args_of(&cmd);
        let e = args
            .iter()
            .position(|a| a == "-e")
            .map(|i| args[i + 1].clone())
            .expect("rsync must use an explicit -e remote shell");
        assert!(e.contains("/tmp/bulk/ctl"));
        assert!(!e.contains("/tmp/interactive/ctl"));
        assert!(args.iter().any(|a| a == "--bwlimit=512"));
        assert!(
            args.iter().any(|a| a == "--checksum"),
            "content comparison is what makes the push a no-op when nothing changed"
        );
        assert!(
            args.iter()
                .any(|a| a == "host:.config/croft/keybindings.json"),
            "the file must land in the remote's config dir, got {args:?}"
        );
    }

    #[test]
    fn a_config_push_never_carries_the_trust_bearing_config_json() {
        // The argv is the last place this can be caught, so assert on it
        // rather than only on the allow-list the argv is built from.
        let lane = crate::remote_bulk::BulkLane::new(crate::remote_bulk::LaneMode::SharedMux, 300);
        for s in crate::config_sync::SYNCABLE {
            let cmd = ship_file_rsync_command(
                &lane,
                Path::new("/tmp/ctl"),
                Path::new("/home/u/.config/croft").join(s.name).as_path(),
                &crate::config_sync::remote_dest("host", s.name),
            );
            let args = args_of(&cmd);
            assert!(
                !args.iter().any(|a| a.ends_with("config.json")),
                "config.json carries MCP consent and must never be in a push argv, got {args:?}"
            );
        }
    }

    #[test]
    fn binary_ship_throttles_on_the_shared_mux_when_no_lane_is_available() {
        let lane = crate::remote_bulk::BulkLane::new(crate::remote_bulk::LaneMode::SharedMux, 300);
        let cmd = ship_file_rsync_command(
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

    /// The lane wiring, plus the allow-list that replaced the deny-list.
    ///
    /// The previous version of this test asserted `--exclude=target` under the
    /// comment "shipping target/ would be gigabytes through the lane". It
    /// passed on every run while 176 GB of `target.noindex` went over the wire,
    /// because it checked the RULE and never the OUTCOME. The outcome is now
    /// pinned by `the_source_sync_ships_only_build_inputs`, which runs rsync
    /// for real; this one only guarantees the allow-list reaches the command.
    #[test]
    fn source_sync_honors_the_bulk_lane_and_allow_lists_the_build_inputs() {
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
        for name in SOURCE_STAMP_INPUTS {
            assert!(
                args.iter().any(|a| a == &format!("--include=/{name}")),
                "{name} is a build input and must be shipped"
            );
        }
        assert!(
            args.iter().any(|a| a == "--exclude=*"),
            "the allow-list is only an allow-list if everything else is denied"
        );
        for name in SOURCE_SKIP_NAMES {
            assert!(
                args.iter().any(|a| a == &format!("--exclude={name}")),
                "{name} is skipped by the stamp walk, so it must not be shipped"
            );
        }
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
            .split("name = \"croft-software\"")
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
            "Cargo.lock croft version {lock_version} drifted from Cargo.toml {toml_version}; run `cargo update -p croft-software`"
        );
    }

    // 2026-08-22: a croft installed from `git#de1a7ab7` hashed its frozen
    // ~/.cargo checkout, said "already up to date", and silently shipped
    // nothing while the fix sat in the working repo. The snapshot warning
    // exists so that no-op announces itself.
    #[test]
    fn snapshot_dirs_are_detected_and_real_repos_are_not() {
        assert!(source_dir_is_snapshot(
            "/Users/u/.cargo/registry/src/index.crates.io-6f17d22bba15001f/croft-software-0.1.757"
        ));
        assert!(source_dir_is_snapshot(
            "/home/u/.cargo/git/checkouts/croft-9a1f/de1a7ab"
        ));
        assert!(source_dir_is_snapshot(
            "/custom/cargo-home/.cargo/git/checkouts/croft-9a1f/de1a7ab"
        ));
        // CARGO_HOME=/custom/cargo-home has no `.cargo` component at all
        assert!(source_dir_is_snapshot(
            "/custom/cargo-home/registry/src/index.crates.io-6f17d22bba15001f/croft-software-0.1.757"
        ));
        assert!(source_dir_is_snapshot(
            "/custom/cargo-home/git/checkouts/croft-9a1f/de1a7ab"
        ));
        assert!(!source_dir_is_snapshot("/Users/u/Documents/croft"));
        assert!(!source_dir_is_snapshot("/home/u/work/croft"));
        assert!(
            source_snapshot_warning().is_none(),
            "the test build itself must come from a working tree, not a snapshot"
        );
    }
}
