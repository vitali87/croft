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
    /// `manifest_dir` is the baked `CARGO_MANIFEST_DIR`, `baked_full` the
    /// baked `CROFT_GIT_HASH_FULL` — the full commit ID, because
    /// abbreviation length follows `core.abbrev`/repo size and can differ
    /// between build time and probe time for the same commit. The git
    /// calls run off-thread so a slow or unreachable directory can never
    /// stall startup.
    pub fn start(manifest_dir: String, baked_full: String) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(probe_drift(&manifest_dir, &baked_full));
        });
        Self { rx }
    }

    /// The probe's verdict once it lands: `Some(Some(hash))` = the repo has
    /// moved to `hash` (a short display label), `Some(None)` = no drift (or
    /// no way to tell), `None` = still probing.
    pub fn take(&self) -> Option<Option<String>> {
        self.rx.try_recv().ok()
    }
}

fn probe_drift(manifest_dir: &str, baked_full: &str) -> Option<String> {
    // `unknown` = built outside git (registry/git snapshot, tarball): the
    // source can never move, and the snapshot warning already covers it.
    if baked_full == "unknown" {
        return None;
    }
    // git discovery walks up from `-C`, so a manifest dir that merely sits
    // INSIDE some repo (a snapshot unpacked under a $HOME that is itself a
    // git repo) would be answered for by that ancestor — and the hint would
    // then offer to `cargo install` an immutable snapshot over itself. Only
    // a dir that is its own repo toplevel speaks for this source (mirrors
    // the same guard in `build.rs`).
    let toplevel = git_in(manifest_dir, &["rev-parse", "--show-toplevel"])?;
    if std::fs::canonicalize(manifest_dir).ok() != std::fs::canonicalize(&toplevel).ok() {
        return None;
    }
    let head = git_in(manifest_dir, &["rev-parse", "HEAD"])?;
    let dirty = !git_in(manifest_dir, &["status", "--porcelain"])?.is_empty();
    drift_label(baked_full, &head, dirty)?;
    // Drift confirmed on full IDs; what surfaces in the status bar is the
    // short label, matching how the baked hash is displayed.
    let short = git_in(manifest_dir, &["rev-parse", "--short", "HEAD"])
        .unwrap_or_else(|| head.chars().take(7).collect());
    Some(if dirty {
        format!("{short}-dirty")
    } else {
        short
    })
}

/// Pure comparison mirroring how `build.rs` composes `CROFT_GIT_HASH_FULL`:
/// the repo's current label is `<head>` or `<head>-dirty`, and any
/// difference from the baked value means the binary predates its own source
/// tree. (Same-hash-still-dirty edits are invisible here — content-precise
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

    /// A finished install with a scripted outcome, for exercising the app's
    /// event handling without running a real `cargo install`.
    #[cfg(test)]
    pub fn preloaded(events: &[UpdateEvent]) -> Self {
        let (tx, rx) = channel();
        for ev in events {
            let _ = tx.send(*ev);
        }
        Self { rx }
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
        let full = git(&["rev-parse", "HEAD"]);
        let short = git(&["rev-parse", "--short", "HEAD"]);

        assert_eq!(probe_drift(dir.to_str().unwrap(), &full), None);
        assert_eq!(probe_drift(dir.to_str().unwrap(), "unknown"), None);
        // The comparison runs on full commit IDs, so an abbreviation-length
        // change between build and probe (`core.abbrev` is auto-scaled by
        // repo size) can never read as drift...
        git(&["config", "core.abbrev", "12"]);
        assert_eq!(probe_drift(dir.to_str().unwrap(), &full), None);
        git(&["config", "--unset", "core.abbrev"]);
        // ...and a baked short label must never be passed for comparison.
        assert_ne!(probe_drift(dir.to_str().unwrap(), &short), None);
        // Real drift surfaces the short display label, not the full ID.
        assert_eq!(
            probe_drift(dir.to_str().unwrap(), &"0".repeat(40)),
            Some(short.clone())
        );
        std::fs::write(dir.join("scratch.txt"), "edit").unwrap();
        assert_eq!(
            probe_drift(dir.to_str().unwrap(), &full),
            Some(format!("{short}-dirty"))
        );
        // A directory git can't answer for (deleted repo, moved tree) is
        // silence, never a false alarm.
        assert_eq!(probe_drift("/nonexistent-croft-drift-probe", "abc"), None);
        // A directory merely INSIDE a repo is answered for by its ancestor
        // (`cargo publish` verify under target/package/, a snapshot under a
        // $HOME that is a git repo): that ancestor's commits say nothing
        // about this source, so the probe must stay silent rather than
        // offer to reinstall an immutable snapshot over itself.
        let nested = dir.join("vendored").join("croft-software-0.1.758");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            probe_drift(nested.to_str().unwrap(), &"0".repeat(40)),
            None,
            "an ancestor repo must never supply drift provenance"
        );
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
