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
mod cross_build;
mod forward;
mod hosts;
mod install;
mod install_stream;
mod launch_command;
mod offer;
mod pull;
mod relay;
mod session;
mod source;
pub(crate) use clipboard::MAX_COPY_PAYLOAD_BYTES;
use clipboard::*;
pub use control::AdoptedMaster;
pub use control::ssh_control_dir;
use control::*;
pub(crate) use cross_build::cross_build_static;
pub(crate) use cross_build::cross_build_unavailable;
pub(crate) use cross_build::raise_fd_limit;
use cross_build::*;
use forward::*;
pub use hosts::RemoteTarget;
pub use hosts::SshConfigState;
pub use hosts::discover_ssh_targets;
pub use hosts::primary_ssh_config_path;
pub use hosts::ssh_config_state;
use hosts::*;
pub(crate) use install::FORCE_SOURCE_BUILD;
pub use install::REMOTE_COMPILE_SIDE_EFFECTS;
pub use install::arch_to_musl_triple;
use install::*;
pub use install_stream::install_only_streaming;
use install_stream::*;
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
pub(crate) use source::shell_quote_for_e_arg;
use source::*;

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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod offer_tests;
