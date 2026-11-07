//! `croft attach` and `croft ls`, local session persistence: run croft under the built-in session host
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
//!
//! The session files these commands read and write (socket paths, sidecars,
//! the liveness probe) live in [`crate::session`], below the session host.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::session::{
    collab_socket_path, is_alive, is_session_socket, meta_path, mux_socket_path, now_unix,
    read_meta, sessions_dir, socket_path, write_meta_preserving_created,
};

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

fn dtach_on_path() -> bool {
    // dtach with no args prints usage and exits non-zero, but it still *ran* —
    // Ok means the binary is on PATH; Err (ENOENT) means it isn't.
    std::process::Command::new("dtach").output().is_ok()
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
        return crate::app::run(workspace, None, None, None, false, Vec::new());
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

/// One live session in `croft ls`.
#[derive(Debug)]
struct SessionRow {
    id: String,
    workspace: String,
    uptime: String,
    /// The host survived a binary update (#238).
    stale: bool,
    /// Where `croft web` serves it, if it does (#343).
    web: Option<String>,
}

/// The live persistent sessions under `dir`, pruning any dead session
/// sockets.
fn session_rows(dir: &Path) -> Vec<SessionRow> {
    let mut rows: Vec<SessionRow> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let socket = entry.path();
            // The folder also holds other listeners: the collab relay and
            // agent hook socket share the hash keying, and pane hosts live
            // here too. None is a session: listing them printed phantom
            // `(unknown)` rows whose ids collided with the real session's,
            // and probing them cost each a connect-and-drop. Their
            // lifecycles belong to their own bind paths, not to `ls` pruning.
            if !socket
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_session_socket)
            {
                continue;
            }
            if !is_alive(&socket) {
                let _ = std::fs::remove_file(&socket);
                let _ = std::fs::remove_file(meta_path(&socket));
                let _ = std::fs::remove_file(crate::web::record_path(&socket));
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
            // #238: the host wrote its stale-image marker — it survived a
            // binary update and keeps running the old code until restarted.
            let stale = crate::session_host::stale_marker_path(&socket).exists();
            let web = crate::web::live_record(&crate::web::record_path(&socket)).map(|r| r.url);
            rows.push(SessionRow {
                id,
                workspace,
                uptime,
                stale,
                web,
            });
        }
    }
    rows
}

/// `croft ls`: print the live persistent sessions, pruning any dead sockets.
pub fn list() -> Result<()> {
    let rows = session_rows(&sessions_dir());
    if rows.is_empty() {
        println!("No persistent croft sessions. Start one with `croft attach`.");
        return Ok(());
    }
    println!("{:<10}  {:<44}  UPTIME", "SESSION", "WORKSPACE");
    for row in rows {
        let hint = if row.stale {
            "  (host pre-dates a binary update; reattach to refresh)"
        } else {
            ""
        };
        let web = row
            .web
            .map(|url| format!("  web: {url}"))
            .unwrap_or_default();
        println!(
            "{:<10}  {:<44}  {}{hint}{web}",
            row.id, row.workspace, row.uptime
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::*;

    /// #343: `croft ls` says which sessions `croft web` exposes, and where.
    #[test]
    fn ls_shows_where_a_session_is_served_on_the_web() {
        let dir = tempfile::tempdir().unwrap();
        let mux = dir.path().join("0123456789abcdef.mux.sock");
        let _mux_listener = bind_socket_0600(&mux).unwrap();
        write_meta(
            &mux,
            &SessionMeta {
                workspace: PathBuf::from("/tmp/ws"),
                created_unix: now_unix(),
            },
        )
        .unwrap();
        assert_eq!(session_rows(dir.path())[0].web, None);
        std::fs::write(
            crate::web::record_path(&mux),
            format!(
                r#"{{"pid":{},"url":"wss://127.0.0.1:7681/"}}"#,
                std::process::id()
            ),
        )
        .unwrap();
        let rows = session_rows(dir.path());
        assert_eq!(rows.len(), 1, "the record is not a session: {rows:?}");
        assert_eq!(rows[0].web.as_deref(), Some("wss://127.0.0.1:7681/"));
    }

    /// A workspace's collab relay socket (`<hash>.collab.sock`) shares the
    /// sessions directory and hash keying with the real session socket, and
    /// is a live listener whenever `croft pair` seated a navigator. It is not
    /// a session: `croft ls` used to print it as a phantom `(unknown)` row
    /// whose 8-char id collided with the real session's.
    #[test]
    fn ls_does_not_report_the_collab_relay_as_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let hash = "deadbeefcafef00d";
        // A live mux session with its workspace sidecar.
        let mux = dir.path().join(format!("{hash}.mux.sock"));
        let _mux_listener = bind_socket_0600(&mux).unwrap();
        write_meta(
            &mux,
            &SessionMeta {
                workspace: PathBuf::from("/tmp/ws"),
                created_unix: now_unix(),
            },
        )
        .unwrap();
        // The same workspace's live collab relay.
        let collab = dir.path().join(format!("{hash}.collab.sock"));
        let _collab_listener = bind_socket_0600(&collab).unwrap();

        let rows = session_rows(dir.path());
        assert_eq!(
            rows.len(),
            1,
            "only the session is a session; the relay must not be listed: {rows:?}"
        );
        assert_eq!(rows[0].workspace, "/tmp/ws");
        assert!(
            collab.exists(),
            "ls must not prune the relay socket either — its lifecycle belongs to the relay"
        );
    }

    /// #881: every croft binds `<hash>.hook.sock` for agent approvals, and
    /// hosted panes bind `pane-<id>.sock`, in the same folder as the
    /// sessions. Only a session's own socket is a session: `ls` lists the
    /// mux and legacy dtach sockets and neither lists, probes nor prunes
    /// anything else, live or dead.
    #[test]
    fn ls_reports_only_session_sockets_and_leaves_the_rest_alone() {
        let dir = tempfile::tempdir().unwrap();
        let hash = "0db75869317a960b";
        let mux = dir.path().join(format!("{hash}.mux.sock"));
        let _mux_listener = bind_socket_0600(&mux).unwrap();
        write_meta(
            &mux,
            &SessionMeta {
                workspace: PathBuf::from("/tmp/ws"),
                created_unix: now_unix(),
            },
        )
        .unwrap();
        let hook = dir.path().join(format!("{hash}.hook.sock"));
        let _hook_listener = bind_socket_0600(&hook).unwrap();
        let pane = dir.path().join("pane-42-1a2b-0.sock");
        let _pane_listener = bind_socket_0600(&pane).unwrap();
        // A hook socket whose croft is gone: still not ls's to remove.
        let stale_hook = dir.path().join("1111222233334444.hook.sock");
        drop(bind_socket_0600(&stale_hook).unwrap());
        // A legacy dtach session is still a session.
        let legacy = dir.path().join("aaaabbbbccccdddd.sock");
        let _legacy_listener = bind_socket_0600(&legacy).unwrap();

        let mut ids: Vec<String> = session_rows(dir.path()).into_iter().map(|r| r.id).collect();
        ids.sort();
        assert_eq!(ids, ["0db75869", "aaaabbbb"]);
        for kept in [&hook, &pane, &stale_hook] {
            assert!(kept.exists(), "ls must not prune {}", kept.display());
        }
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

    #[test]
    fn humanize_age_scales_units() {
        assert_eq!(humanize_age(5), "5s");
        assert_eq!(humanize_age(120), "2m");
        assert_eq!(humanize_age(7_200), "2h");
        assert_eq!(humanize_age(180_000), "2d");
    }
}
