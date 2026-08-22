use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// Transitions a remote-launched croft observes while a newer binary is
/// being installed under it by the local cross-build that launched it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateEvent {
    /// The `updating` marker appeared: an install is in flight.
    InProgress,
    /// The marker vanished without the stamp advancing: the install
    /// failed or was abandoned. The old binary keeps running.
    Failed,
    /// The install-stamp advanced to a value different from the one this
    /// process launched with: the new binary is in place at the install
    /// path and croft should re-exec into it.
    Ready,
}

/// Filesystem signals the watcher polls, relative to `~/.cache/croft`.
const STAMP_FILE: &str = "install-stamp";
const MARKER_FILE: &str = "updating";
const POLL_INTERVAL: Duration = Duration::from_millis(1000);

pub struct UpdateWatch {
    rx: Receiver<UpdateEvent>,
}

impl UpdateWatch {
    /// Spawn a 1 Hz poller over the two single files under `cache_dir`.
    /// `launch_stamp` is the install-stamp content present when this
    /// process started; any different non-empty value means a newer
    /// binary has landed. Polling one stat per second is negligible and
    /// avoids the per-tree inotify/FSEvents asymmetry the workspace
    /// watcher has to manage.
    pub fn start(cache_dir: PathBuf, launch_stamp: String) -> Self {
        let (tx, rx): (Sender<UpdateEvent>, Receiver<UpdateEvent>) = channel();
        std::thread::spawn(move || poll_loop(&cache_dir, &launch_stamp, &tx));
        Self { rx }
    }

    pub fn drain(&self) -> Vec<UpdateEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

/// One-shot background probe answering: does the repo this binary was
/// installed from now sit at a different commit/dirty state than the binary
/// has baked in? The local half of the deploy-verification story (#242) —
/// remotes are re-stamped on every connect, but `~/.cargo/bin/croft` only
/// changes when someone reinstalls, so it can silently fall behind the tree
/// it came from.
pub struct DriftProbe {
    rx: Receiver<Option<String>>,
}

impl DriftProbe {
    /// `manifest_dir` is the baked `CARGO_MANIFEST_DIR`, `baked_hash` the
    /// baked `CROFT_GIT_HASH`. The git calls run off-thread so a slow or
    /// unreachable directory can never stall startup.
    pub fn start(manifest_dir: String, baked_hash: String) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(probe_drift(&manifest_dir, &baked_hash));
        });
        Self { rx }
    }

    /// The probe's verdict once it lands: `Some(Some(hash))` = the repo has
    /// moved to `hash`, `Some(None)` = no drift (or no way to tell),
    /// `None` = still probing.
    pub fn take(&self) -> Option<Option<String>> {
        self.rx.try_recv().ok()
    }
}

fn probe_drift(manifest_dir: &str, baked_hash: &str) -> Option<String> {
    // `unknown` = built outside git (registry/git snapshot, tarball): the
    // source can never move, and the snapshot warning already covers it.
    if baked_hash == "unknown" {
        return None;
    }
    let head = git_in(manifest_dir, &["rev-parse", "--short", "HEAD"])?;
    let dirty = !git_in(manifest_dir, &["status", "--porcelain"])?.is_empty();
    drift_label(baked_hash, &head, dirty)
}

/// Pure comparison mirroring how `build.rs` composes `CROFT_GIT_HASH`: the
/// repo's current label is `<head>` or `<head>-dirty`, and any difference
/// from the baked value means the binary predates its own source tree.
/// (Same-hash-still-dirty edits are invisible here — content-precise
/// comparison is the remote stamp's job; this is a hint, not a proof.)
fn drift_label(baked_hash: &str, head: &str, dirty: bool) -> Option<String> {
    let current = if dirty {
        format!("{head}-dirty")
    } else {
        head.to_string()
    };
    (current != baked_hash).then_some(current)
}

fn git_in(dir: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

/// A background `cargo install --path <repo> --locked` reinstalling the
/// local croft, reported through the same [`UpdateEvent`] lifecycle the
/// remote watcher uses so the app consumes both with one state machine:
/// `InProgress` immediately, then `Ready` (new binary at the install path,
/// F9 re-execs into it) or `Failed` (old binary keeps running).
pub struct SelfInstall {
    rx: Receiver<UpdateEvent>,
}

impl SelfInstall {
    /// Full build output lands in `log_path` so a failure is diagnosable
    /// without scrollback.
    pub fn start(manifest_dir: String, log_path: PathBuf) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(UpdateEvent::InProgress);
            let result = std::process::Command::new("cargo")
                .args(["install", "--path", &manifest_dir, "--locked"])
                .output();
            let event = match result {
                Ok(out) => {
                    let mut log = out.stdout;
                    log.extend_from_slice(&out.stderr);
                    let _ = std::fs::write(&log_path, &log);
                    if out.status.success() {
                        UpdateEvent::Ready
                    } else {
                        UpdateEvent::Failed
                    }
                }
                Err(err) => {
                    let _ = std::fs::write(&log_path, format!("failed to run cargo: {err}"));
                    UpdateEvent::Failed
                }
            };
            let _ = tx.send(event);
        });
        Self { rx }
    }

    pub fn drain(&self) -> Vec<UpdateEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

fn poll_loop(cache_dir: &std::path::Path, launch_stamp: &str, tx: &Sender<UpdateEvent>) {
    let stamp_path = cache_dir.join(STAMP_FILE);
    let marker_path = cache_dir.join(MARKER_FILE);
    let mut announced_in_progress = false;
    loop {
        std::thread::sleep(POLL_INTERVAL);
        let stamp = std::fs::read_to_string(&stamp_path)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if !stamp.is_empty() && stamp != launch_stamp {
            let _ = tx.send(UpdateEvent::Ready);
            return;
        }
        let marker_present = marker_path.exists();
        if marker_present && !announced_in_progress {
            announced_in_progress = true;
            if tx.send(UpdateEvent::InProgress).is_err() {
                return;
            }
        } else if !marker_present && announced_in_progress {
            announced_in_progress = false;
            if tx.send(UpdateEvent::Failed).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("croft-update-watch-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wait_for(watch: &UpdateWatch, want: UpdateEvent) -> bool {
        for _ in 0..50 {
            if watch.drain().contains(&want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn emits_ready_when_stamp_advances() {
        let dir = scratch_dir("ready");
        std::fs::write(dir.join(STAMP_FILE), "old").unwrap();
        let watch = UpdateWatch::start(dir.clone(), String::from("old"));
        std::fs::write(dir.join(STAMP_FILE), "new").unwrap();
        assert!(wait_for(&watch, UpdateEvent::Ready));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drift_label_matches_how_build_rs_composes_the_hash() {
        assert_eq!(drift_label("abc123", "abc123", false), None);
        assert_eq!(drift_label("abc123-dirty", "abc123", true), None);
        assert_eq!(
            drift_label("abc123", "def456", false),
            Some(String::from("def456"))
        );
        // The same commit gaining uncommitted changes is drift: the binary
        // no longer matches the tree.
        assert_eq!(
            drift_label("abc123", "abc123", true),
            Some(String::from("abc123-dirty"))
        );
        // ...and so is the reverse (the dirty edits were committed away).
        assert_eq!(
            drift_label("abc123-dirty", "abc123", false),
            Some(String::from("abc123"))
        );
    }

    // 2026-08-22 (#242): a Mac croft shipped a remote main @ 84a31a0 while
    // itself running a binary built an hour earlier — the machine deployed
    // code newer than itself, invisibly. The probe exists so that state
    // announces itself.
    #[test]
    fn probe_reports_drift_against_a_real_repo_and_silence_when_current() {
        let dir = scratch_dir("drift");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "one",
        ]);
        let head = git(&["rev-parse", "--short", "HEAD"]);

        assert_eq!(probe_drift(dir.to_str().unwrap(), &head), None);
        assert_eq!(probe_drift(dir.to_str().unwrap(), "unknown"), None);
        assert_eq!(
            probe_drift(dir.to_str().unwrap(), "0000000"),
            Some(head.clone())
        );
        std::fs::write(dir.join("scratch.txt"), "edit").unwrap();
        assert_eq!(
            probe_drift(dir.to_str().unwrap(), &head),
            Some(format!("{head}-dirty"))
        );
        // A directory git can't answer for (deleted repo, moved tree) is
        // silence, never a false alarm.
        assert_eq!(probe_drift("/nonexistent-croft-drift-probe", "abc"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn emits_in_progress_then_failed_when_marker_clears_without_stamp_change() {
        let dir = scratch_dir("fail");
        std::fs::write(dir.join(STAMP_FILE), "same").unwrap();
        let watch = UpdateWatch::start(dir.clone(), String::from("same"));
        std::fs::write(dir.join(MARKER_FILE), "").unwrap();
        assert!(wait_for(&watch, UpdateEvent::InProgress));
        std::fs::remove_file(dir.join(MARKER_FILE)).unwrap();
        assert!(wait_for(&watch, UpdateEvent::Failed));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
