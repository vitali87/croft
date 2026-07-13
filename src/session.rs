//! Local session persistence: run croft under dtach so a session launched on
//! this machine (its terminals, LSP, DAP, editor state) survives closing the
//! terminal window and can be reattached later. This mirrors the remote
//! persistence in [`crate::remote`] (same dtach flags and socket keying) but
//! for the box croft already runs on.
//!
//! - `croft attach [path]` attaches to (or creates) the persistent session for
//!   a workspace.
//! - Detach by closing the window: the dtach client dies, the server keeps
//!   croft alive. (`-E` disables dtach's own detach key so croft keeps every
//!   Ctrl chord, exactly as on the remote path — an in-app detach chord would
//!   need a separate control channel and is left for a follow-up.)
//! - `croft ls` lists the live sessions and prunes dead sockets.

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

fn meta_path(socket: &Path) -> PathBuf {
    socket.with_extension("json")
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

/// A dtach server holds an open listening socket; a crashed server leaves a
/// stale socket file behind (unix sockets aren't auto-unlinked), so existence
/// isn't liveness — probe by connecting.
// ponytail: the connect registers as a fleeting client attach, costing the
// detached croft one WINCH repaint; fine for a handful of sessions, swap for an
// lsof/peer check if the session count ever grows large.
#[cfg(unix)]
fn is_alive(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

#[cfg(not(unix))]
fn is_alive(_socket: &Path) -> bool {
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
/// workspace. Falls back to a plain non-persistent launch when dtach is absent,
/// same as the remote wrapper's `else exec` branch.
pub fn attach(path: Option<PathBuf>) -> Result<()> {
    let workspace = match path {
        Some(p) => p,
        None => std::env::current_dir().context("resolving workspace path")?,
    }
    .canonicalize()
    .context("resolving workspace path")?;
    if !workspace.is_dir() {
        anyhow::bail!("{} is not a directory", workspace.display());
    }

    let croft = std::env::current_exe().context("resolving croft binary path")?;
    let inner = vec![
        croft.to_string_lossy().into_owned(),
        workspace.to_string_lossy().into_owned(),
    ];

    if !dtach_on_path() {
        eprintln!(
            "dtach not found; launching a non-persistent session. Install dtach \
             (e.g. `brew install dtach`) for detach/reattach."
        );
        return crate::app::run(workspace, None, None, false);
    }

    let dir = sessions_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let socket = socket_path(&workspace);
    // Reattaching to an existing session keeps its original creation time, so
    // `ls` uptime reflects when the session started, not this reattach.
    let created_unix = read_meta(&socket)
        .map(|m| m.created_unix)
        .unwrap_or_else(now_unix);
    write_meta(
        &socket,
        &SessionMeta {
            workspace: workspace.clone(),
            created_unix,
        },
    )?;
    exec_dtach(&socket, &inner)
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

#[cfg(test)]
mod tests {
    use super::*;

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
