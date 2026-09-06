use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use crate::git::{GitRequest, GitResponse, GitStatus, git_worker_loop};
use crate::widgets::source_control::SourceControlPanel;

const MIN_GAP: Duration = Duration::from_millis(400);

pub struct GitWorker {
    request_tx: Sender<GitRequest>,
    response_rx: Receiver<GitResponse>,
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
}

impl GitWorker {
    pub fn spawn(git_root: PathBuf) -> Self {
        let (request_tx, request_rx) = std::sync::mpsc::channel::<GitRequest>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<GitResponse>();
        std::thread::spawn(move || {
            git_worker_loop(git_root, request_rx, response_tx);
        });
        let _ = request_tx.send(GitRequest::Status);
        Self {
            request_tx,
            response_rx,
            status: GitStatus::default(),
            last_check: Instant::now(),
            pending: None,
            status_arrived: false,
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
        let _ = self.request_tx.send(req);
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
            let _ = self.request_tx.send(req);
        }
    }

    /// Refresh the full source-control state in one shot: branch, ahead/behind,
    /// dirty flag, AND the change list. Used by the explicit refresh action and
    /// after a commit, where the branch sync counts (e.g. `↑1`) must update,
    /// not just the file list. Still off-thread — the worker replies async.
    pub fn request_status_and_changes(&mut self) {
        let _ = self.request_tx.send(GitRequest::StatusAndChanges);
    }

    pub fn set_root(&mut self, root: PathBuf) {
        let _ = self.request_tx.send(GitRequest::SetRoot(root));
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
            match self.response_rx.try_recv() {
                Ok(GitResponse::Status(s)) => {
                    self.status_arrived = true;
                    if self.status != s {
                        self.status = s.clone();
                        if let Some(p) = panel.as_deref_mut() {
                            p.status = s;
                        }
                        changed = true;
                    }
                }
                Ok(GitResponse::StatusAndChanges(s, entries)) => {
                    self.status_arrived = true;
                    self.status = s.clone();
                    if let Some(p) = panel.as_deref_mut() {
                        p.set_status(s, entries);
                    }
                    changed = true;
                }
                Err(_) => break,
            }
        }
        changed
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
            let (_response_tx, response_rx) = std::sync::mpsc::channel::<GitResponse>();
            let worker = Self {
                request_tx,
                response_rx,
                status: GitStatus::default(),
                last_check: Instant::now()
                    .checked_sub(MIN_GAP * 2)
                    .unwrap_or_else(Instant::now),
                pending: None,
                status_arrived: false,
            };
            (worker, request_rx)
        }

        /// Like `for_test`, keeping the response sender so a test can feed
        /// the worker responses as the background thread would.
        fn for_test_with_responses() -> (Self, Sender<GitResponse>) {
            let (request_tx, _request_rx) = std::sync::mpsc::channel::<GitRequest>();
            let (response_tx, response_rx) = std::sync::mpsc::channel::<GitResponse>();
            let worker = Self {
                request_tx,
                response_rx,
                status: GitStatus::default(),
                last_check: Instant::now(),
                pending: None,
                status_arrived: false,
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
}
