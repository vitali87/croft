//! Local session persistence: run croft under the built-in session host
//! ([`crate::session_host`], the multiplayer mux) so a session launched on
//! this machine (its terminals, LSP, DAP, editor state) survives closing the
//! terminal window and can be reattached later, by one or several clients.
//! This mirrors the remote persistence in [`crate::remote`] (same socket
//! keying) but for the box croft already runs on.
//!
//! - `croft attach [path]` attaches to (or creates) the persistent session for
//!   a workspace. Several attachers share the session (see
//!   docs/MULTIPLAYER.md).
//! - Detach by closing the window: the client dies, the host keeps croft
//!   alive.
//! - `croft ls` lists the live sessions and prunes dead sockets.
//! - Sessions created by an older croft under dtach keep reattaching through
//!   dtach until they end; new sessions never need dtach.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::session_state::dirs_cache_croft;

/// Directory holding one `<hash>.sock` dtach control socket per persistent
/// workspace, each with a `<hash>.json` sidecar recording which workspace it
/// belongs to. Same `$HOME/.cache/croft/sessions` path the remote wrapper uses.
fn sessions_dir() -> PathBuf {
    dirs_cache_croft().join("sessions")
}

/// Fixed-seed hash of the canonical workspace path, so a reattach finds the
/// same session (mirrors `remote::dtach_socket_path`; `DefaultHasher` uses
/// fixed SipHash keys, so the name is stable across croft processes).
fn socket_name(workspace: &Path) -> String {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(workspace.to_string_lossy().as_bytes());
    format!("{:016x}", hasher.finish())
}

fn socket_path(workspace: &Path) -> PathBuf {
    sessions_dir().join(format!("{}.sock", socket_name(workspace)))
}

/// Socket for a mux (session-host) session. A distinct name from the legacy
/// dtach socket, so a client never speaks croft's frame protocol at a live
/// dtach server (which would feed the frames into the PTY as input).
fn mux_socket_path(workspace: &Path) -> PathBuf {
    sessions_dir().join(format!("{}.mux.sock", socket_name(workspace)))
}

/// Socket carrying Phase D collab ops between independent-viewport
/// participants (never PTY bytes), sibling to the mux socket with the same
/// keying (see docs/MULTIPLAYER.md).
pub(crate) fn collab_socket_path(workspace: &Path) -> PathBuf {
    sessions_dir().join(format!("{}.collab.sock", socket_name(workspace)))
}

fn meta_path(socket: &Path) -> PathBuf {
    socket.with_extension("json")
}

/// Sidecar recording the resident navigator's activation for a workspace:
/// `croft pair` writes it and exits; a running croft notices it (or reads it
/// at startup) and seats the pilot in-process. Same keying as the other
/// workspace sidecars.
pub(crate) fn pair_record_path(workspace: &Path) -> PathBuf {
    sessions_dir().join(format!("{}.pair.json", socket_name(workspace)))
}

/// Advisory lock guarding self-appointed navigator ownership for a workspace:
/// exactly one plain croft may host the pilot (and claim collab owner site 1)
/// per workspace. The lock is held for the App's lifetime and released by the
/// OS on exit, so a crashed host hands off to the next croft automatically.
pub(crate) fn pair_host_lock_path(workspace: &Path) -> PathBuf {
    sessions_dir().join(format!("{}.pair-host.lock", socket_name(workspace)))
}

/// Holds the workspace's pair-host lock; the flock releases when this (and its
/// file) drop.
pub(crate) struct PairHostLock {
    _file: std::fs::File,
}

/// Try to claim the single-host lock at `path` without blocking. `Some` means
/// this croft may self-appoint owner; `None` means another croft already holds
/// it and this one must not host (else two owners would both claim site 1 and
/// corrupt the shared buffer).
pub(crate) fn try_acquire_pair_host_lock(path: &Path) -> Option<PairHostLock> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // flock is per open-file-description, so two independent opens contend
        // even within one process — the mutual exclusion we want across croft
        // instances. LOCK_NB: fail fast instead of blocking the tick.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return None;
        }
    }
    Some(PairHostLock { _file: file })
}

/// What `croft pair` records for the workspace's resident navigator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PairRecord {
    /// Model handed to the claude CLI (None = its default).
    pub(crate) model: Option<String>,
    /// The caret name the navigator sits under.
    pub(crate) name: String,
    /// False = explicitly deactivated (`croft pair --off` / palette toggle).
    pub(crate) enabled: bool,
    /// Vestigial: older records may carry a start task, but resident seating
    /// never replays it (a persisted task would re-fire on every launch, and
    /// an @file task would freeze the UI thread on seat). Instructions come
    /// from Cmd+K Q; the --repl driver still takes a one-shot task directly.
    #[serde(default)]
    pub(crate) task: Option<String>,
    /// Which backend seats the pilot: None or "claude" = the claude CLI;
    /// "ollama" = a local Anthropic-compatible endpoint (absent on 0.1.635
    /// records, which are all claude).
    #[serde(default)]
    pub(crate) provider: Option<String>,
    /// The local endpoint, e.g. http://localhost:11434. Never a token — auth
    /// for keyed gateways stays in the environment (ANTHROPIC_AUTH_TOKEN).
    #[serde(default)]
    pub(crate) base_url: Option<String>,
}

pub(crate) fn write_pair_record(path: &Path, record: &PairRecord) -> Result<()> {
    let mut v = serde_json::to_value(record).context("serializing pair record")?;
    // Every write is observable: the App re-arms a downed navigator when
    // the record CHANGES, and an enabled→enabled rewrite (`croft pair`
    // after a failed seat) would otherwise be byte-identical — and mtime
    // alone misses a same-grain rewrite on coarse-timestamp filesystems.
    // The read path ignores the stamp (serde skips unknown fields).
    v["written_at_nanos"] = serde_json::Value::from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
    );
    // The clock alone cannot guarantee distinct bytes (SystemTime can
    // repeat for rapid writes under coarse granularity): pid + a
    // process-local counter differs on every call, in every process.
    static WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    v["write_nonce"] = serde_json::Value::from(
        (u64::from(std::process::id()) << 32)
            | (WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xffff_ffff),
    );
    std::fs::write(path, v.to_string()).with_context(|| format!("writing {}", path.display()))
}

pub(crate) fn read_pair_record(path: &Path) -> Option<PairRecord> {
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

/// Sidecar recorded next to a socket: the socket name is only a hash, so `ls`
/// reads this to show the real workspace path and the session's age.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SessionMeta {
    workspace: PathBuf,
    created_unix: u64,
}

/// The dtach argv that runs `inner` (the real croft TUI) under an
/// attach-or-create session on `socket`. Same flags as the remote wrapper:
/// `-A` attach-or-create, `-E`/`-z` keep dtach's hands off croft's Ctrl
/// chords, `-r winch` repaints on reattach. Pure so it can be unit-tested.
fn dtach_attach_argv(socket: &str, inner: &[String]) -> Vec<String> {
    let mut argv = vec![
        String::from("-A"),
        socket.to_string(),
        String::from("-E"),
        String::from("-z"),
        String::from("-r"),
        String::from("winch"),
    ];
    argv.extend(inner.iter().cloned());
    argv
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn dtach_on_path() -> bool {
    // dtach with no args prints usage and exits non-zero, but it still *ran* —
    // Ok means the binary is on PATH; Err (ENOENT) means it isn't.
    std::process::Command::new("dtach").output().is_ok()
}

fn write_meta(socket: &Path, meta: &SessionMeta) -> Result<()> {
    let json = serde_json::to_string(meta).context("serializing session meta")?;
    let path = meta_path(socket);
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))
}

fn read_meta(socket: &Path) -> Option<SessionMeta> {
    let json = std::fs::read_to_string(meta_path(socket)).ok()?;
    serde_json::from_str(&json).ok()
}

/// Remove a socket's meta sidecar (best effort). Called by the session host on
/// clean exit so a later server for the same workspace starts a fresh uptime.
pub(crate) fn remove_meta(socket: &Path) {
    let _ = std::fs::remove_file(meta_path(socket));
}

/// Record the workspace sidecar for a session socket, keeping the original
/// creation time across reattaches so `ls` uptime reflects the session's
/// real start. Used by the session host when it takes ownership of a socket.
pub(crate) fn write_meta_preserving_created(socket: &Path, workspace: &Path) -> Result<()> {
    let created_unix = read_meta(socket)
        .map(|m| m.created_unix)
        .unwrap_or_else(now_unix);
    write_meta(
        socket,
        &SessionMeta {
            workspace: workspace.to_path_buf(),
            created_unix,
        },
    )
}

/// A dtach server holds an open listening socket; a crashed server leaves a
/// stale socket file behind (unix sockets aren't auto-unlinked), so existence
/// isn't liveness — probe by connecting.
// ponytail: the connect registers as a fleeting client attach, costing the
// detached croft one WINCH repaint; fine for a handful of sessions, swap for an
// lsof/peer check if the session count ever grows large.
#[cfg(unix)]
pub(crate) fn is_alive(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

#[cfg(not(unix))]
pub(crate) fn is_alive(_socket: &Path) -> bool {
    false
}

fn humanize_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

#[cfg(unix)]
fn exec_dtach(socket: &Path, inner: &[String]) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let argv = dtach_attach_argv(&socket.to_string_lossy(), inner);
    // exec replaces this process with dtach, which forks the persistent server
    // (running the inner croft) and attaches us as its client. Only returns on
    // failure to exec. CROFT_SESSION_PERSISTENT switches on the WINCH mode-
    // reassert so the mouse survives a reattach, exactly as on the remote path.
    let err = std::process::Command::new("dtach")
        .args(&argv)
        .env("CROFT_SESSION_PERSISTENT", "1")
        .exec();
    Err(err).context("exec dtach")
}

#[cfg(not(unix))]
fn exec_dtach(_socket: &Path, _inner: &[String]) -> Result<()> {
    anyhow::bail!("session persistence requires a Unix host")
}

/// `croft attach [path]`: attach to (or create) the persistent session for a
/// workspace via the built-in session host (multiplayer mux; see
/// docs/MULTIPLAYER.md), which needs no external binary. A workspace with a
/// live legacy dtach session keeps reattaching through dtach so sessions
/// started under an older croft are never orphaned.
///
/// With `solo`, open an independent viewport instead: this process runs its
/// own croft (no shared PTY) and replicates shared-file edits with the other
/// participants over the workspace's collab relay (Phase D).
pub fn attach(path: Option<PathBuf>, solo: bool) -> Result<()> {
    let workspace = match path {
        Some(p) => p,
        None => std::env::current_dir().context("resolving workspace path")?,
    }
    .canonicalize()
    .context("resolving workspace path")?;
    if !workspace.is_dir() {
        anyhow::bail!("{} is not a directory", workspace.display());
    }

    if solo {
        let collab = collab_socket_path(&workspace);
        crate::collab::ensure_relay(&collab)?;
        // The app reads these through CollabChannel::env_config, exactly as
        // the remote launch tail exports them over SSH. Startup here is
        // still single-threaded; the app's threads spawn inside run().
        unsafe {
            std::env::set_var("CROFT_COLLAB_SOCKET", &collab);
            std::env::set_var("CROFT_COLLAB_ROLE", "guest");
        }
        return crate::app::run(workspace, None, None, false);
    }

    let croft = std::env::current_exe().context("resolving croft binary path")?;
    let inner = vec![
        croft.to_string_lossy().into_owned(),
        workspace.to_string_lossy().into_owned(),
    ];

    let dir = sessions_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let legacy = socket_path(&workspace);
    if is_alive(&legacy) && dtach_on_path() {
        // Reattaching keeps the original creation time, so `ls` uptime
        // reflects when the session started, not this reattach.
        write_meta_preserving_created(&legacy, &workspace)?;
        return exec_dtach(&legacy, &inner);
    }

    let mux = mux_socket_path(&workspace);
    let code = crate::session_host::attach_or_create(&mux, Some(&workspace), &inner)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// `croft ls`: print the live persistent sessions, pruning any dead sockets.
pub fn list() -> Result<()> {
    let dir = sessions_dir();
    let mut rows: Vec<(String, String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let socket = entry.path();
            if socket.extension().and_then(|e| e.to_str()) != Some("sock") {
                continue;
            }
            if !is_alive(&socket) {
                let _ = std::fs::remove_file(&socket);
                let _ = std::fs::remove_file(meta_path(&socket));
                continue;
            }
            let meta = read_meta(&socket);
            let stem = socket
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let id = stem.chars().take(8).collect::<String>();
            let workspace = meta
                .as_ref()
                .map(|m| m.workspace.display().to_string())
                .unwrap_or_else(|| String::from("(unknown)"));
            let uptime = meta
                .as_ref()
                .map(|m| humanize_age(now_unix().saturating_sub(m.created_unix)))
                .unwrap_or_else(|| String::from("?"));
            rows.push((id, workspace, uptime));
        }
    }
    if rows.is_empty() {
        println!("No persistent croft sessions. Start one with `croft attach`.");
        return Ok(());
    }
    println!("{:<10}  {:<44}  UPTIME", "SESSION", "WORKSPACE");
    for (id, ws, up) in rows {
        println!("{id:<10}  {ws:<44}  {up}");
    }
    Ok(())
}

/// Bind a unix listener at `socket`, owner-only (0600) with no window in
/// which anyone else could connect. Possession of the account is the trust
/// boundary for every croft socket (the session mux, the collab relay):
/// another user must never be able to connect. Shared by both binders so the
/// permission discipline lives in exactly one place.
pub fn bind_socket_0600(socket: &Path) -> std::io::Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    use std::os::unix::io::AsRawFd;
    // Creation is serialized per target through an owner-only lock file held
    // across the liveness probe, the stale-file removal, the bind and the
    // publication. Without it, two attach-or-create racers could both probe
    // "nobody alive", both publish, and the later publisher replaced the
    // earlier one's directory entry - a live but pathless listener, i.e. a
    // stranded session host invisible to every future client. The loser now
    // finds the winner alive under the lock and is told to attach instead
    // (`AddrInUse`). The flock releases on drop (or on crash, by the OS).
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(socket.with_extension("bind.lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if is_alive(socket) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("a listener is already serving {}", socket.display()),
        ));
    }
    // Proven dead under the lock: a crashed owner's stale file (unix sockets
    // are not auto-unlinked) must not block the next creator.
    let _ = std::fs::remove_file(socket);
    // `bind` starts accepting connections the instant it returns, and the
    // socket file's mode comes from the process umask. A save/restore umask
    // dance around the bind is process-global state: two concurrent binds
    // interleaving their restores corrupted the mask for the rest of the
    // process's life, and one of the sockets got created under the loose
    // caller mask. Instead, bind inside a fresh 0700 staging dir next to
    // the target (same filesystem; the short `.s` name keeps the AF_UNIX
    // path-length budget), fix the socket's own mode to 0600 while nobody
    // can traverse to it, then rename(2) it into place atomically. No
    // process-global state anywhere.
    static STAGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = STAGE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let parent = socket.parent().unwrap_or(Path::new("."));
    let stage = parent.join(format!(".s{}-{n}", std::process::id()));
    // A crash can strand a same-named staging dir (the counter restarts
    // with the process and pids recycle); it is ours by construction.
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::DirBuilder::new().mode(0o700).create(&stage)?;
    let result = (|| {
        let tmp = stage.join("s");
        let listener = std::os::unix::net::UnixListener::bind(&tmp)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, socket)?;
        Ok(listener)
    })();
    let _ = std::fs::remove_dir_all(&stage);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An enabled→enabled rewrite must NEVER serialize byte-identically:
    /// the App re-arms a downed navigator on record-content change, and the
    /// clock alone cannot carry that guarantee (SystemTime can repeat for
    /// rapid writes under coarse clock granularity). A per-write nonce
    /// (pid + process-local counter) differs even when the clock stands
    /// still.
    #[test]
    fn every_pair_record_write_serializes_differently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pair.json");
        let record = PairRecord {
            model: None,
            name: String::from("nav"),
            enabled: true,
            task: None,
            provider: None,
            base_url: None,
        };
        write_pair_record(&path, &record).unwrap();
        let a = std::fs::read(&path).unwrap();
        write_pair_record(&path, &record).unwrap();
        let b = std::fs::read(&path).unwrap();
        assert_ne!(a, b, "identical records must still write distinct bytes");
        let va: serde_json::Value = serde_json::from_slice(&a).unwrap();
        let vb: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert!(
            va["write_nonce"].is_u64(),
            "the nonce is part of the record"
        );
        assert_ne!(
            va["write_nonce"], vb["write_nonce"],
            "the nonce differs even when the clock stands still"
        );
        // The stamp fields stay invisible to the typed read path.
        assert_eq!(read_pair_record(&path).as_ref(), Some(&record));
    }

    /// Two attach-or-create racers for the SAME target must never both
    /// publish: before creation was serialized, the later publisher replaced
    /// the earlier one's directory entry, leaving a live but pathless
    /// listener (a stranded session host, invisible to every future client).
    /// Exactly one racer may win; the loser is told the address is in use so
    /// it attaches to the winner instead.
    #[test]
    fn racing_binds_on_one_target_produce_exactly_one_listener() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("same.sock");
        let outcomes: Vec<std::io::Result<std::os::unix::net::UnixListener>> =
            std::thread::scope(|s| {
                let handles: Vec<_> = (0..2)
                    .map(|_| {
                        let sock = sock.clone();
                        s.spawn(move || bind_socket_0600(&sock))
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
        let winners = outcomes.iter().filter(|o| o.is_ok()).count();
        assert_eq!(
            winners, 1,
            "exactly one racer may publish a listener on one target"
        );
        for o in &outcomes {
            if let Err(e) = o {
                assert_eq!(
                    e.kind(),
                    std::io::ErrorKind::AddrInUse,
                    "the loser must be told to attach, got {e:?}"
                );
            }
        }
        // And the published listener is the live one: a client reaches it.
        let l = outcomes.into_iter().find_map(|o| o.ok()).unwrap();
        let _c = std::os::unix::net::UnixStream::connect(&sock)
            .expect("the surviving pathname must reach the winning listener");
        drop(l);
    }

    /// A stale socket file (its owner crashed; nobody accepts) must not block
    /// the next creator: the binder proves it dead and replaces it.
    #[test]
    fn a_dead_stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("stale.sock");
        drop(bind_socket_0600(&sock).expect("first bind"));
        assert!(sock.exists(), "a dropped listener leaves its file behind");
        let _l = bind_socket_0600(&sock).expect("a dead socket file must be replaced");
        std::os::unix::net::UnixStream::connect(&sock).expect("the new listener answers");
    }

    /// Every croft socket is owner-only from its first observable instant.
    #[test]
    fn a_bound_socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("a.sock");
        let _l = bind_socket_0600(&sock).expect("bind");
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket must be 0600, got {mode:o}");
    }

    /// Binding must not go through process-global state: the parallel test
    /// suite (and any future in-process threading) binds relay and mux
    /// sockets from many threads at once. The old save/restore umask dance
    /// raced - two interleaved binds could restore each other's masks
    /// (permanently corrupting the process umask) and one socket could be
    /// created under the caller's loose umask for its whole life.
    #[test]
    fn concurrent_binds_never_corrupt_the_process_umask_or_a_socket() {
        use std::os::unix::fs::PermissionsExt;
        let read_umask = || unsafe {
            // Read is a write on this API; probe with the tightest value so
            // the blink can only ever make a concurrent file MORE private.
            let cur = libc::umask(0o077);
            libc::umask(cur);
            cur
        };
        let before = read_umask();
        let dir = tempfile::tempdir().unwrap();
        let worst = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        std::thread::scope(|s| {
            for t in 0..2 {
                let dir = dir.path().to_path_buf();
                let worst = std::sync::Arc::clone(&worst);
                s.spawn(move || {
                    for i in 0..300 {
                        let sock = dir.join(format!("{t}-{i}.sock"));
                        let l = bind_socket_0600(&sock).expect("bind");
                        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
                        let mut w = worst.lock().unwrap();
                        if mode > *w {
                            *w = mode;
                        }
                        drop(l);
                        let _ = std::fs::remove_file(&sock);
                    }
                });
            }
        });
        assert_eq!(
            *worst.lock().unwrap(),
            0o600,
            "a racing bind produced a socket looser than 0600"
        );
        assert_eq!(
            read_umask(),
            before,
            "racing binds corrupted the process umask"
        );
    }

    /// The single-host lock is exclusive: a second acquirer is refused while
    /// the first holds it, and the lock frees when the holder drops (the OS
    /// releases it, which is how a crashed host hands off).
    #[test]
    fn pair_host_lock_is_exclusive_and_frees_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.pair-host.lock");
        let first = try_acquire_pair_host_lock(&path).expect("first acquires");
        assert!(
            try_acquire_pair_host_lock(&path).is_none(),
            "a second croft must be refused while the first holds the lock"
        );
        drop(first);
        assert!(
            try_acquire_pair_host_lock(&path).is_some(),
            "the lock frees when the holder drops"
        );
    }

    #[test]
    fn dtach_attach_argv_has_expected_flags_and_inner() {
        let inner = vec![
            String::from("/usr/local/bin/croft"),
            String::from("/work/repo"),
        ];
        let argv = dtach_attach_argv("/home/u/.cache/croft/sessions/abc.sock", &inner);
        assert_eq!(
            argv,
            vec![
                "-A",
                "/home/u/.cache/croft/sessions/abc.sock",
                "-E",
                "-z",
                "-r",
                "winch",
                "/usr/local/bin/croft",
                "/work/repo",
            ]
        );
    }

    /// The navigator activation record: written by `croft pair`, read by a
    /// running croft; keyed like the other workspace sidecars; roundtrips.
    #[test]
    fn pair_record_roundtrips_and_shares_the_workspace_keying() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.pair.json");
        assert_eq!(read_pair_record(&path), None);
        let record = PairRecord {
            model: Some("claude-haiku-4-5-20251001".into()),
            name: "navigator".into(),
            enabled: true,
            task: Some("look around".into()),
            provider: None,
            base_url: None,
        };
        write_pair_record(&path, &record).unwrap();
        assert_eq!(read_pair_record(&path), Some(record));

        let keyed = pair_record_path(Path::new("/work/repo"));
        assert!(keyed.to_string_lossy().ends_with(".pair.json"));
        assert_eq!(
            keyed.parent(),
            collab_socket_path(Path::new("/work/repo")).parent()
        );
    }

    /// A record activated for a local provider keeps the provider and its
    /// endpoint across the write/read cycle.
    #[test]
    fn pair_record_roundtrips_local_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.pair.json");
        let record = PairRecord {
            model: Some("qwen3-coder:30b".into()),
            name: "navigator".into(),
            enabled: true,
            task: None,
            provider: Some("ollama".into()),
            base_url: Some("http://localhost:11434".into()),
        };
        write_pair_record(&path, &record).unwrap();
        assert_eq!(read_pair_record(&path), Some(record));
    }

    /// A 0.1.635 record (no provider fields) still reads: absent fields mean
    /// the claude provider, exactly what those records were written for.
    #[test]
    fn legacy_record_without_provider_is_claude() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.pair.json");
        std::fs::write(
            &path,
            r#"{"model":null,"name":"claude","enabled":true,"task":null}"#,
        )
        .unwrap();
        let record = read_pair_record(&path).expect("legacy record reads");
        assert_eq!(record.provider, None);
        assert_eq!(record.base_url, None);
    }

    #[test]
    fn socket_path_is_stable_per_workspace_and_differs_across_paths() {
        let a1 = socket_path(Path::new("/work/repo"));
        let a2 = socket_path(Path::new("/work/repo"));
        let b = socket_path(Path::new("/work/other"));
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        assert!(a1.to_string_lossy().contains(".cache/croft/sessions"));
        assert_eq!(a1.extension().and_then(|e| e.to_str()), Some("sock"));
    }

    #[test]
    fn collab_socket_shares_keying_but_not_name_with_the_mux_socket() {
        let ws = Path::new("/work/repo");
        let collab = collab_socket_path(ws);
        assert_eq!(collab, collab_socket_path(ws), "keying must be stable");
        assert!(
            collab
                .to_string_lossy()
                .ends_with(&format!("{}.collab.sock", socket_name(ws)))
        );
        assert_ne!(collab, mux_socket_path(ws));
        assert_ne!(collab, socket_path(ws));
    }

    #[test]
    fn meta_round_trips_next_to_socket() {
        let dir = std::env::temp_dir().join(format!("croft-session-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("deadbeef.sock");
        let meta = SessionMeta {
            workspace: PathBuf::from("/work/repo"),
            created_unix: 1_700_000_000,
        };
        write_meta(&socket, &meta).unwrap();
        assert_eq!(read_meta(&socket), Some(meta));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reattach_preserves_original_created_unix() {
        let dir =
            std::env::temp_dir().join(format!("croft-session-reattach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("cafef00d.sock");
        // First attach records creation time.
        write_meta(
            &socket,
            &SessionMeta {
                workspace: PathBuf::from("/work/repo"),
                created_unix: 1_700_000_000,
            },
        )
        .unwrap();
        // Reattach resolves creation time from the existing sidecar, not "now".
        let created = read_meta(&socket)
            .map(|m| m.created_unix)
            .unwrap_or_else(now_unix);
        assert_eq!(created, 1_700_000_000);
        // A fresh workspace with no sidecar falls back to now.
        let fresh = dir.join("00000000.sock");
        let created_fresh = read_meta(&fresh)
            .map(|m| m.created_unix)
            .unwrap_or_else(now_unix);
        assert!(created_fresh >= 1_700_000_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn humanize_age_scales_units() {
        assert_eq!(humanize_age(5), "5s");
        assert_eq!(humanize_age(120), "2m");
        assert_eq!(humanize_age(7_200), "2h");
        assert_eq!(humanize_age(180_000), "2d");
    }
}
