use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

/// The name of the git status worker's thread, which the crash notice
/// names.
pub(super) const THREAD: &str = "git-status";

use crate::git::{GitRequest, GitResponse, GitStateWatch, GitStatus, git_worker_loop};
use crate::jobs::{Start, Worker, WorkerPoll};
use crate::widgets::source_control::SourceControlPanel;

const MIN_GAP: Duration = Duration::from_millis(400);

pub struct GitWorker {
    /// The `git-status` thread: one per workspace folder, started again
    /// once if it crashes, then stopped (#2589).
    worker: Worker<GitRequest, GitResponse>,
    /// The folder the worker reads, sent again to a restarted thread.
    root: PathBuf,
    /// The worker stopped since the last [`Self::take_stopped`].
    stopped_news: bool,
    status: GitStatus,
    last_check: Instant,
    /// A refresh that arrived inside the `MIN_GAP` debounce window and was
    /// coalesced rather than dropped. `flush_pending` ships it once the
    /// window elapses, so the final change in a burst always lands without
    /// waiting for another filesystem event.
    pending: Option<GitRequest>,
    /// A status response landed since the last `take_status_arrival`,
    /// whether or not it changed `status` (#471). An open `git diff --staged`
    /// view is rebuilt from the INDEX, and replacing an already-staged
    /// file's bytes leaves porcelain status identical, so "status changed"
    /// alone would never re-run that view; "a status refresh completed" is
    /// the signal that the index may have moved.
    status_arrived: bool,
    /// The worker has answered a status for its current root at least once:
    /// until then `status` is a default that reads as "no repository"
    /// (#871).
    reported: bool,
    /// Stamps on the files in git's own folder that git used outside croft
    /// writes (#2577); see [`Self::refresh_if_git_moved`].
    state_watch: GitStateWatch,
}

impl GitWorker {
    pub fn spawn(git_root: PathBuf) -> Self {
        Self::spawn_with(git_root, git_worker_loop)
    }

    /// [`Self::spawn`] with the worker's loop given, so a test can crash it.
    fn spawn_with(
        git_root: PathBuf,
        work: impl Fn(PathBuf, Receiver<GitRequest>, Sender<GitResponse>) + Send + Sync + 'static,
    ) -> Self {
        let first_root = git_root.clone();
        // The first frame shows no git state; its git processes wait.
        let worker = Worker::spawn(THREAD, Start::AfterFirstFrame, move |rx, tx| {
            work(first_root.clone(), rx, tx)
        })
        .expect("start the git status worker");
        worker.send(GitRequest::Status);
        Self {
            worker,
            root: git_root,
            stopped_news: false,
            status: GitStatus::default(),
            last_check: Instant::now(),
            pending: None,
            status_arrived: false,
            reported: false,
            state_watch: GitStateWatch::default(),
        }
    }

    pub fn status(&self) -> &GitStatus {
        &self.status
    }

    pub fn request_status_debounced(&mut self, want_changes: bool) {
        let req = if want_changes {
            GitRequest::StatusAndChanges
        } else {
            GitRequest::Status
        };
        if self.last_check.elapsed() < MIN_GAP {
            // Inside the window: coalesce instead of dropping, so the last
            // change in a burst is still fetched on the trailing edge.
            self.pending = Some(match self.pending.take() {
                Some(prev) => prev.merge(req),
                None => req,
            });
            return;
        }
        self.last_check = Instant::now();
        self.pending = None;
        self.worker.send(req);
    }

    /// Ship a refresh that was coalesced by the debounce window once that
    /// window has elapsed. Called every tick from `drain_git_responses`, so
    /// the trailing edge of an edit burst is fetched on its own a few ms
    /// after the gap clears, instead of being dropped until the next FS
    /// event or a view switch.
    pub fn flush_pending(&mut self) {
        if self.last_check.elapsed() < MIN_GAP {
            return;
        }
        if let Some(req) = self.pending.take() {
            self.last_check = Instant::now();
            self.worker.send(req);
        }
    }

    /// Re-read the status when git's own state moved since the last call:
    /// a commit, checkout, staging, fetch or push run outside croft (#2577),
    /// which the file watcher cannot see. A few file stamps, so it rides the
    /// watcher's poll. Debounced like any refresh. True when it asked.
    pub fn refresh_if_git_moved(&mut self, want_changes: bool) -> bool {
        if !self.state_watch.moved(&self.status) {
            return false;
        }
        self.request_status_debounced(want_changes);
        true
    }

    /// Refresh the full source-control state in one shot: branch, ahead/behind,
    /// dirty flag, AND the change list. Used by the explicit refresh action and
    /// after a commit, where the branch sync counts (e.g. `↑1`) must update,
    /// not just the file list. Still off-thread — the worker replies async.
    /// A worker that stopped after crashing twice starts again here: the
    /// user asked (#2589).
    pub fn request_status_and_changes(&mut self) {
        if self.worker.is_stopped() && self.worker.restart().is_ok() {
            self.worker.send(GitRequest::SetRoot(self.root.clone()));
        }
        self.worker.send(GitRequest::StatusAndChanges);
    }

    pub fn set_root(&mut self, root: PathBuf) {
        self.root = root.clone();
        self.reported = false;
        self.worker.send(GitRequest::SetRoot(root));
    }

    pub fn bypass_debounce(&mut self) {
        self.last_check = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
    }

    /// Drain replies, refreshing the cached status; `panel` is `None` for
    /// a worker whose repository the Source Control panel is NOT showing
    /// (multi-root, #149) — its change lists are discarded (the panel
    /// re-requests them when its repo becomes active) while the status
    /// still lands for the status line, gutter invalidation, and the
    /// ignored union.
    pub fn drain_into(&mut self, mut panel: Option<&mut SourceControlPanel>) -> bool {
        let mut changed = false;
        loop {
            match self.worker.poll() {
                WorkerPoll::Answer(GitResponse::Status(s)) => {
                    self.status_arrived = true;
                    self.reported = true;
                    if let Some(p) = panel.as_deref_mut() {
                        p.status_known = true;
                    }
                    if self.status != s {
                        self.status = s.clone();
                        if let Some(p) = panel.as_deref_mut() {
                            p.status = s;
                            p.seed_prepared_message();
                        }
                        changed = true;
                    }
                }
                WorkerPoll::Answer(GitResponse::StatusAndChanges(s, entries)) => {
                    self.status_arrived = true;
                    self.reported = true;
                    self.status = s.clone();
                    if let Some(p) = panel.as_deref_mut() {
                        p.status_known = true;
                        p.set_status(s, entries);
                    }
                    changed = true;
                }
                WorkerPoll::Idle => break,
                // Its thread crashed, and the crash notice said so. A fresh
                // one reads the same folder and answers the status the old
                // one owed, so nothing about git freezes (#2589).
                WorkerPoll::Restarted => {
                    self.worker.send(GitRequest::SetRoot(self.root.clone()));
                    self.worker.send(GitRequest::StatusAndChanges);
                }
                WorkerPoll::Stopped => {
                    self.stopped_news = true;
                    changed = true;
                    break;
                }
            }
        }
        changed
    }

    /// True once when the worker has stopped after crashing again soon
    /// after a restart (#2589), so the app can say so.
    pub fn take_stopped(&mut self) -> bool {
        std::mem::take(&mut self.stopped_news)
    }

    /// Whether the worker has answered a status for its current root yet.
    pub fn reported(&self) -> bool {
        self.reported
    }

    /// True once per completed status refresh since the last call, changed
    /// or not (#471). See the `status_arrived` field.
    pub fn take_status_arrival(&mut self) -> bool {
        std::mem::take(&mut self.status_arrived)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    impl GitWorker {
        /// Build a worker with no background thread, returning the request
        /// receiver so a test can observe exactly which requests the debounce
        /// logic emits. `last_check` is backdated so the first debounced
        /// request fires immediately, matching a freshly idle worker.
        fn for_test() -> (Self, Receiver<GitRequest>) {
            let (request_tx, request_rx) = std::sync::mpsc::channel::<GitRequest>();
            let (response_tx, response_rx) = std::sync::mpsc::channel::<GitResponse>();
            // Kept open for the test's life: a closed answer channel reads
            // as a crashed worker (#2589).
            std::mem::forget(response_tx);
            let worker = Self {
                worker: Worker::from_channels(request_tx, response_rx),
                root: PathBuf::new(),
                stopped_news: false,
                status: GitStatus::default(),
                last_check: Instant::now()
                    .checked_sub(MIN_GAP * 2)
                    .unwrap_or_else(Instant::now),
                pending: None,
                status_arrived: false,
                reported: false,
                state_watch: GitStateWatch::default(),
            };
            (worker, request_rx)
        }

        /// Like `for_test`, keeping the response sender so a test can feed
        /// the worker responses as the background thread would.
        fn for_test_with_responses() -> (Self, Sender<GitResponse>) {
            let (request_tx, _request_rx) = std::sync::mpsc::channel::<GitRequest>();
            let (response_tx, response_rx) = std::sync::mpsc::channel::<GitResponse>();
            let worker = Self {
                worker: Worker::from_channels(request_tx, response_rx),
                root: PathBuf::new(),
                stopped_news: false,
                status: GitStatus::default(),
                last_check: Instant::now(),
                pending: None,
                status_arrived: false,
                reported: false,
                state_watch: GitStateWatch::default(),
            };
            (worker, response_tx)
        }
    }

    /// #471: a status identical to the last one is not a CHANGE, but it is
    /// an ARRIVAL, and the staged diff view keys off the latter.
    #[test]
    fn a_status_equal_to_the_last_still_counts_as_an_arrival() {
        let (mut worker, tx) = GitWorker::for_test_with_responses();
        assert!(!worker.take_status_arrival(), "nothing has landed yet");
        tx.send(GitResponse::Status(GitStatus::default())).unwrap();
        assert!(!worker.drain_into(None), "an equal status is not a change");
        assert!(worker.take_status_arrival(), "but it did arrive");
        assert!(
            !worker.take_status_arrival(),
            "the flag is consumed by the take"
        );
    }

    #[test]
    fn coalesced_refresh_fires_on_the_trailing_edge_without_another_event() {
        let (mut worker, req_rx) = GitWorker::for_test();
        // Leading edge: the first refresh after an idle gap fires at once.
        worker.request_status_debounced(true);
        assert_eq!(req_rx.try_recv(), Ok(GitRequest::StatusAndChanges));
        // A second refresh inside the debounce window must not shell out now.
        worker.request_status_debounced(true);
        assert!(
            req_rx.try_recv().is_err(),
            "a refresh inside the debounce window must not fire immediately",
        );
        // Flushing before the window elapses is a no-op.
        worker.flush_pending();
        assert!(req_rx.try_recv().is_err());
        // Once the window passes, the coalesced refresh fires on its own with
        // NO further FS event - the trailing edge that used to be dropped,
        // leaving the Source Control panel stale until a view switch.
        std::thread::sleep(MIN_GAP + Duration::from_millis(20));
        worker.flush_pending();
        assert_eq!(
            req_rx.try_recv(),
            Ok(GitRequest::StatusAndChanges),
            "the final change in a burst must be fetched on the trailing edge",
        );
    }

    /// A folder that is a git repository, for a real worker to read.
    fn repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let ok = crate::test_git::test_git_command()
            .arg("-C")
            .arg(tmp.path())
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success();
        assert!(ok, "git init");
        tmp
    }

    /// The real worker loop, whose first `crashes` threads panic on their
    /// first request.
    fn crashing_worker(root: PathBuf, crashes: usize) -> GitWorker {
        let started = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        GitWorker::spawn_with(root, move |root, rx, tx| {
            if started.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < crashes {
                let _ = rx.recv();
                panic!("the git worker crashed (on purpose, in a test)");
            }
            git_worker_loop(root, rx, tx);
        })
    }

    /// Drain `worker` until `done`, within a load-scaled second.
    fn drain_until(
        worker: &mut GitWorker,
        what: &str,
        mut done: impl FnMut(&mut GitWorker) -> bool,
    ) {
        crate::test_budget::await_spawned(Duration::from_secs(5), what, || {
            worker.drain_into(None);
            done(worker)
        });
    }

    /// #2589: a git worker whose thread crashed is started again and
    /// answers the status the old one owed, instead of every git read
    /// freezing until a restart.
    #[test]
    fn a_git_worker_whose_thread_crashed_still_answers() {
        let tmp = repo();
        let mut worker = crashing_worker(tmp.path().to_path_buf(), 1);
        drain_until(&mut worker, "the restarted worker's status", |w| {
            w.status().in_repo
        });
        assert!(!worker.take_stopped());
    }

    /// #2589: a worker that crashes again soon after its restart stops,
    /// says so once, and Refresh starts it again.
    #[test]
    fn a_git_worker_that_crashes_again_stops_until_refresh() {
        let tmp = repo();
        let mut worker = crashing_worker(tmp.path().to_path_buf(), 2);
        drain_until(&mut worker, "the worker to stop", GitWorker::take_stopped);
        assert!(!worker.take_stopped(), "it says so once");
        assert!(!worker.status().in_repo);
        worker.request_status_and_changes();
        drain_until(&mut worker, "the status after Refresh", |w| {
            w.status().in_repo
        });
    }
}
