//! The live config re-push: watching the synced files during a session and shipping
//! each change over the bulk lane.

use super::*;

/// Re-pushes a config file edited on this machine while a remote session
/// runs (#262), so a keybinding changed mid-session reaches the remote
/// croft, which applies it on arrival, instead of waiting for the next
/// connect. A thread polls the files with [`crate::config_sync::ConfigWatch`]
/// and ships each change over the bulk lane; it logs to
/// `~/.cache/croft/config-sync.log`, never to the terminal, which belongs to
/// the remote croft. Dropping it stops the thread.
pub(super) struct ConfigRepush {
    /// Host and control socket to push through; swapped on a reconnect.
    target: std::sync::Arc<std::sync::Mutex<(String, PathBuf)>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Joined on drop, so no push starts after the session has ended.
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ConfigRepush {
    /// Start re-pushing changes relative to `watch`, whose baseline the
    /// caller took before the connect-time push.
    pub(super) fn start(ssh: &SshControl, watch: crate::config_sync::ConfigWatch) -> Self {
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
                    &mut |files| {
                        push_over_bulk_lane(
                            &host,
                            &socket,
                            files,
                            &crate::config_sync::SyncState::path(),
                        )
                    },
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
    pub(super) fn retarget(&self, ssh: &SshControl) {
        if let Ok(mut g) = self.target.lock() {
            *g = (ssh.host.clone(), ssh.socket_path.clone());
        }
    }
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

/// What the exclusions say about a pending re-push.
pub(super) enum RepushGate {
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
pub(super) const REPUSH_RETRY: std::time::Duration = std::time::Duration::from_secs(10);

impl RepushState {
    pub(super) fn new(watch: crate::config_sync::ConfigWatch) -> Self {
        RepushState {
            watch,
            pending: std::collections::BTreeSet::new(),
            retry_at: None,
        }
    }
}

/// Config files to push, each with the local path it is read from.
pub(super) type Pushes = [(crate::config_sync::Syncable, PathBuf)];

/// One poll of a live re-push: push the syncable files that changed and
/// exist, and log what went and what did not. `push` returns the names it
/// could not ship.
pub(super) fn repush_tick(
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

/// Ship `files` over a bulk lane opened for this batch, recording what was
/// pushed in the sync state at `state_path`; the names that failed.
pub(super) fn push_over_bulk_lane(
    host: &str,
    socket: &Path,
    files: &[(crate::config_sync::Syncable, PathBuf)],
    state_path: &Path,
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
    let mut state = crate::config_sync::SyncState::load(state_path);
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
                // The terminal belongs to the remote croft: rsync, and the
                // ssh it starts, get none of it (#2716). A failure is logged
                // by name below.
                let ok = matches!(
                    ship_file_rsync_command(&bulk.lane, socket, local, &dest)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status(),
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
    let _ = state.save(state_path);
    result
}
