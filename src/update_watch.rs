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
