//! Hot exit (#862): a copy of the workspace's unsaved buffers, kept on disk
//! while they are unsaved, so a kill, a crash or an OOM kill loses at most
//! the last few seconds of typing instead of every unsaved edit. A clean
//! quit, or discarding at the unsaved-changes prompt, removes it; the next
//! launch of the workspace brings back what it holds as unsaved tabs.
//!
//! Each running croft writes its own file,
//! `<dir>/<workspace digest>/<pid>.json`, so two windows on one workspace
//! never overwrite, remove or restore each other's copy: a launch takes over
//! only the files of crofts that are gone.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::session_state::OpenTabState;

/// One unsaved buffer: the tab as the update relaunch carries it (path,
/// cursor, unsaved text), plus the file's (mtime, length) when the buffer
/// last matched it, so a restore can tell the file changed on disk since.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Buffer {
    #[serde(flatten)]
    pub tab: OpenTabState,
    pub disk_stamp: Option<(SystemTime, u64)>,
}

/// One croft's backup of a workspace's unsaved buffers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backup {
    pub workspace_root: PathBuf,
    pub buffers: Vec<Buffer>,
}

impl Backup {
    /// Write the backup to `path`, owner-only and atomically, so a crash in
    /// the middle leaves the previous backup rather than half of this one.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string(self).context("serializing the hot-exit backup")?;
        crate::session_state::write_private_atomically(path, json.as_bytes())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let json =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&json).with_context(|| format!("parsing {}", path.display()))
    }

    /// Whether this backup was taken of the workspace at `root`, however
    /// either path spells it (a symlink, a trailing component).
    pub fn is_of(&self, root: &Path) -> bool {
        canonical(&self.workspace_root) == canonical(root)
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The directory under `dir` holding the backups of the workspace at
/// `root`: named for a digest of its canonical path, so the name is the
/// same across runs and croft versions, and two workspaces never share one.
pub fn workspace_dir(dir: &Path, root: &Path) -> PathBuf {
    use sha2::Digest;
    let digest = format!(
        "{:x}",
        sha2::Sha256::digest(canonical(root).to_string_lossy().as_bytes())
    );
    dir.join(&digest[..16])
}

/// This croft's own backup file for the workspace at `root`.
pub fn own_path(dir: &Path, root: &Path) -> PathBuf {
    workspace_dir(dir, root).join(format!("{}.json", std::process::id()))
}

/// Remove the backup at `path`, and its workspace directory once nothing
/// else is left in it.
pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
    if let Some(parent) = path.parent() {
        // Fails, harmlessly, while another croft's backup is still there.
        let _ = std::fs::remove_dir(parent);
    }
}

/// The backups of the workspace at `root` under `dir` that no running croft
/// owns: named for a pid that is gone, or for this process (a dead croft's
/// pid, recycled). A running croft's file is its own, live copy.
pub fn orphaned(dir: &Path, root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(workspace_dir(dir, root)) else {
        return Vec::new();
    };
    let me = std::process::id();
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<u32>().ok())
                .is_some_and(|pid| pid == me || is_gone(pid))
        })
        .collect();
    files.sort();
    files
}

/// Whether no process `pid` exists. Signal 0 checks without delivering
/// anything; ESRCH is the only answer that means "gone" (EPERM is a live
/// process of another user).
fn is_gone(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    pid > 0
        && unsafe { libc::kill(pid, 0) } != 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backup(root: &Path) -> Backup {
        Backup {
            workspace_root: root.to_path_buf(),
            buffers: vec![Buffer {
                tab: OpenTabState {
                    path: Some(root.join("a.txt")),
                    cursor_row: 0,
                    cursor_col: 1,
                    scroll: 0,
                    scroll_col: 0,
                    dirty: true,
                    unsaved_text: Some(String::from("xalpha")),
                },
                disk_stamp: Some((SystemTime::UNIX_EPOCH, 6)),
            }],
        }
    }

    #[test]
    fn a_backup_round_trips_through_disk_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = own_path(dir.path(), root.path());
        backup(root.path()).save(&path).unwrap();
        assert_eq!(Backup::load(&path).unwrap(), backup(root.path()));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn workspaces_get_their_own_stable_directories() {
        let dir = Path::new("/cache/hot-exit");
        let a = workspace_dir(dir, Path::new("/nonexistent/work/a"));
        assert_eq!(a, workspace_dir(dir, Path::new("/nonexistent/work/a")));
        assert_ne!(a, workspace_dir(dir, Path::new("/nonexistent/work/b")));
        assert_eq!(a.parent(), Some(dir));
    }

    #[test]
    fn only_the_backups_of_gone_crofts_are_orphaned() {
        let root = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace_dir(dir.path(), root.path());
        std::fs::create_dir_all(&ws).unwrap();
        // pid 1 is always running; this pid is ours; the last never exists.
        for name in ["1.json", "999999999.json", "notes.json", "7.json.1.tmp"] {
            std::fs::write(ws.join(name), "{}").unwrap();
        }
        std::fs::write(ws.join(format!("{}.json", std::process::id())), "{}").unwrap();
        let found: Vec<String> = orphaned(dir.path(), root.path())
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let mut want = vec![
            format!("{}.json", std::process::id()),
            String::from("999999999.json"),
        ];
        want.sort();
        assert_eq!(found, want);
    }

    #[test]
    fn removing_the_last_backup_removes_its_directory_but_not_a_neighbours() {
        let root = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mine = own_path(dir.path(), root.path());
        let other = mine.with_file_name("1.json");
        backup(root.path()).save(&mine).unwrap();
        backup(root.path()).save(&other).unwrap();
        remove(&mine);
        assert!(!mine.exists() && other.exists(), "the other window's stays");
        remove(&other);
        assert!(!workspace_dir(dir.path(), root.path()).exists());
    }
}
