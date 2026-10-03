//! Hot exit (#862): a copy of the workspace's unsaved buffers, kept on disk
//! while they are unsaved, so a kill, a crash or an OOM kill loses at most
//! the last few seconds of typing instead of every unsaved edit. A clean
//! quit, or discarding at the unsaved-changes prompt, removes it; the next
//! launch of the workspace brings back what it holds as unsaved tabs.
//!
//! Each running croft writes its own file,
//! `<dir>/<workspace digest>/<pid>-<start time>.json`, so two windows on one
//! workspace never overwrite, remove or restore each other's copy: a launch
//! takes over only the files of crofts that are gone. The start time tells a
//! croft from a later process that was given its recycled pid.
//!
//! Limits, chosen rather than missed: a file counts as changed on disk since
//! a backup when its modification time or size differ, the stamp croft's
//! own on-disk change detection uses, so a rewrite that keeps both goes
//! unnoticed. A backup that cannot be read is reported and kept for
//! [`UNREADABLE_KEPT_FOR`], for recovery by hand, then removed.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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

/// How long a backup that cannot be read is kept, reported at each launch
/// in case it can be recovered by hand, before a launch removes it.
pub const UNREADABLE_KEPT_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// This croft's own backup file for the workspace at `root`, named for the
/// process writing it: `<pid>-<start time>.json`, or `<pid>.json` where the
/// platform will not tell the start time.
pub fn own_path(dir: &Path, root: &Path) -> PathBuf {
    let pid = std::process::id();
    let name = match own_start() {
        Some(start) => format!("{pid}-{start}.json"),
        None => format!("{pid}.json"),
    };
    workspace_dir(dir, root).join(name)
}

/// When process `pid` started, in whole seconds since the epoch, or `None`
/// when the platform will not say (the process is gone, or hidden from this
/// user). With the pid it names one process: a later process given the
/// same recycled pid started at another time.
pub fn start_time_of(pid: u32) -> Option<u64> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let p = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[p]), true);
    sys.process(p).map(|pr| pr.start_time()).filter(|&t| t > 0)
}

/// This process's start time, read once.
fn own_start() -> Option<u64> {
    static START: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *START.get_or_init(|| start_time_of(std::process::id()))
}

/// Remove the backup at `path`, and its workspace directory once nothing
/// else is left in it. The text goes first: the file is overwritten with an
/// empty backup of `root` before it is unlinked, so where the unlink fails
/// (a directory made read-only) no text is left behind, and the next launch
/// removes the empty backup instead of restoring anything. `Err` only when
/// neither worked, and the text may still be on disk.
pub fn remove(path: &Path, root: &Path) -> std::io::Result<()> {
    let cleared = clear(path, root);
    match std::fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                // Fails, harmlessly, while another croft's backup is there.
                let _ = std::fs::remove_dir(parent);
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => cleared.map_err(|_| e),
    }
}

/// Overwrite the backup at `path` in place with an empty backup of `root`.
/// Never through a symbolic link: one in the backup directory is not a
/// backup, and opening it would truncate whatever it points at.
fn clear(path: &Path, root: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let empty = Backup {
        workspace_root: root.to_path_buf(),
        buffers: Vec::new(),
    };
    let json = serde_json::to_vec(&empty).map_err(std::io::Error::other)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    file.write_all(&json)?;
    // On disk before the unlink is tried: a power cut between the two must
    // not leave the discarded text to come back on the next launch.
    file.sync_all()
}

/// Whether the backup at `path` was last written longer than
/// [`UNREADABLE_KEPT_FOR`] ago.
pub fn is_expired(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > UNREADABLE_KEPT_FOR)
}

/// The backups of the workspace at `root` under `dir` that no running croft
/// owns: their croft is gone (see [`owner_runs`]), or the file carries this
/// process's pid (a dead croft's pid, recycled, or an earlier run in this
/// process). A running croft's file is its own, live copy. Only regular
/// files count: a symbolic link there is not a backup, and is never read,
/// expired or removed through.
///
/// Newest written first, so where two backups hold the same file, the
/// newer is the one restored into its tab, and the older stays in its
/// backup for a later launch (#862 review). The name breaks a tie.
pub fn orphaned(dir: &Path, root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(workspace_dir(dir, root)) else {
        return Vec::new();
    };
    let me = std::process::id();
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file()))
        .filter(|p| owner_of(p).is_some_and(|(pid, start)| pid == me || !owner_runs(pid, start)))
        .collect();
    files.sort_by_cached_key(|p| {
        let written = std::fs::metadata(p).and_then(|m| m.modified()).ok();
        std::cmp::Reverse((written, p.clone()))
    });
    files
}

/// The croft a backup's file name records: its pid, and its start time
/// when the name carries one. `<pid>.json`, written before hot exit
/// recorded start times or where the platform would not say, has the pid
/// alone. `None` for anything that is not a backup.
fn owner_of(path: &Path) -> Option<(u32, Option<u64>)> {
    if path.extension().is_none_or(|x| x != "json") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    match stem.split_once('-') {
        Some((pid, start)) => Some((pid.parse().ok()?, Some(start.parse().ok()?))),
        None => Some((stem.parse().ok()?, None)),
    }
}

/// Whether the croft that wrote a backup still runs: its pid is alive and,
/// when the backup names a start time, the process on that pid started
/// then. A live pid under another start time was recycled to a later
/// process after the croft died. A backup named by its pid alone, or a
/// process whose start time cannot be read, is judged by the pid alone:
/// better to leave a backup to a croft that may still run than to take it
/// from under one that does.
fn owner_runs(pid: u32, start: Option<u64>) -> bool {
    if is_gone(pid) {
        return false;
    }
    match (start, start_time_of(pid)) {
        (Some(then), Some(now)) => then == now,
        _ => true,
    }
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

    /// A process this test starts and owns, so its start time is readable
    /// wherever the test runs; killed when dropped.
    struct Running(std::process::Child);

    impl Running {
        fn start() -> Self {
            Self(
                std::process::Command::new("sleep")
                    .arg("60")
                    .spawn()
                    .expect("spawn sleep"),
            )
        }
        fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn only_the_backups_of_gone_crofts_are_orphaned() {
        let root = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace_dir(dir.path(), root.path());
        std::fs::create_dir_all(&ws).unwrap();
        // `running` runs for the whole test; 999999999 never exists.
        let running = Running::start();
        let pid = running.pid();
        let started = start_time_of(pid).expect("a child's start time");
        let names = [
            // pid alone: judged by the pid.
            format!("{pid}.json"),
            String::from("999999999.json"),
            // pid and start time: the same process, or a recycled pid.
            format!("{pid}-{started}.json"),
            format!("{pid}-1.json"),
            String::from("999999999-7.json"),
            // Not backups.
            String::from("notes.json"),
            String::from("1-x.json"),
            String::from("7.json.1.tmp"),
        ];
        for name in &names {
            std::fs::write(ws.join(name), "{}").unwrap();
        }
        let own = own_path(dir.path(), root.path());
        std::fs::write(&own, "{}").unwrap();
        let mut found: Vec<String> = orphaned(dir.path(), root.path())
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        let mut want = vec![
            own.file_name().unwrap().to_string_lossy().into_owned(),
            String::from("999999999.json"),
            format!("{pid}-1.json"),
            String::from("999999999-7.json"),
        ];
        want.sort();
        assert_eq!(found, want);
    }

    /// #862 review (CodeRabbit, security): an entry in the backup
    /// directory that is a symbolic link is not a backup. `orphaned`
    /// offered it for restore and for expiry as an unreadable backup, and
    /// removing it opened it for writing, truncating whatever it pointed at
    /// outside the cache, before unlinking the link.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_entry_is_never_listed_nor_written_through() {
        let root = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("notes.txt");
        std::fs::write(&victim, "keep me\n").unwrap();
        let ws = workspace_dir(dir.path(), root.path());
        std::fs::create_dir_all(&ws).unwrap();
        let link = ws.join("999999998-1.json");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert_eq!(orphaned(dir.path(), root.path()), Vec::<PathBuf>::new());
        remove(&link, root.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me\n");
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "the link itself goes"
        );
    }

    /// #862 review: where two gone crofts' backups hold the same file, the
    /// one restored first fills its tab and the other stays in its backup,
    /// so the newer backup comes first, whatever the names would sort to.
    #[test]
    fn the_newest_orphaned_backup_comes_first() {
        let root = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace_dir(dir.path(), root.path());
        std::fs::create_dir_all(&ws).unwrap();
        // By name, "1000000000-1" sorts before "999999998-1".
        let older = ws.join("999999998-1.json");
        let newer = ws.join("1000000000-1.json");
        let at = |path: &Path, secs: u64| {
            std::fs::write(path, "{}").unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
                .unwrap();
        };
        at(&older, 1_000_000);
        at(&newer, 2_000_000);
        assert_eq!(orphaned(dir.path(), root.path()), vec![newer, older]);
    }

    #[test]
    fn own_path_names_this_process_and_when_it_started() {
        let own = own_path(Path::new("/cache"), Path::new("/nonexistent/w"));
        let start = start_time_of(std::process::id()).expect("our own start time");
        assert_eq!(
            own.file_name().unwrap().to_string_lossy(),
            format!("{}-{start}.json", std::process::id())
        );
    }

    #[test]
    fn removing_the_last_backup_removes_its_directory_but_not_a_neighbours() {
        let root = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mine = own_path(dir.path(), root.path());
        let other = mine.with_file_name("1.json");
        backup(root.path()).save(&mine).unwrap();
        backup(root.path()).save(&other).unwrap();
        remove(&mine, root.path()).unwrap();
        assert!(!mine.exists() && other.exists(), "the other window's stays");
        remove(&other, root.path()).unwrap();
        assert!(!workspace_dir(dir.path(), root.path()).exists());
        assert!(remove(&other, root.path()).is_ok(), "gone already is fine");
    }

    /// Clearing leaves an empty backup of the workspace: no text, and the
    /// next launch removes it rather than restoring anything.
    #[test]
    fn a_cleared_backup_holds_no_text() {
        let root = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = own_path(dir.path(), root.path());
        backup(root.path()).save(&path).unwrap();
        clear(&path, root.path()).unwrap();
        let cleared = Backup::load(&path).unwrap();
        assert!(cleared.buffers.is_empty() && cleared.is_of(root.path()));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("xalpha"));
    }

    /// A removal that cannot happen at all (a directory in the backup's
    /// place) is an error, not silence.
    #[test]
    fn a_removal_that_cannot_happen_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = own_path(dir.path(), root.path());
        std::fs::create_dir_all(path.join("inside")).unwrap();
        assert!(remove(&path, root.path()).is_err());
    }
}
