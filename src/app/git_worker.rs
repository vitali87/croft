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
        }
    }

    pub fn status(&self) -> &GitStatus {
        &self.status
    }

    pub fn request_status_debounced(&mut self, want_changes: bool) {
        if self.last_check.elapsed() < MIN_GAP {
            return;
        }
        self.last_check = Instant::now();
        let req = if want_changes {
            GitRequest::StatusAndChanges
        } else {
            GitRequest::Status
        };
        let _ = self.request_tx.send(req);
    }

    pub fn request_changes(&mut self) {
        let _ = self.request_tx.send(GitRequest::Changes);
    }

    pub fn set_root(&mut self, root: PathBuf) {
        let _ = self.request_tx.send(GitRequest::SetRoot(root));
    }

    pub fn bypass_debounce(&mut self) {
        self.last_check = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
    }

    pub fn drain(&mut self, source_control: &mut SourceControlPanel) -> bool {
        let mut changed = false;
        loop {
            match self.response_rx.try_recv() {
                Ok(GitResponse::Status(s)) => {
                    if self.status != s {
                        self.status = s.clone();
                        source_control.status = s;
                        changed = true;
                    }
                }
                Ok(GitResponse::Changes(entries)) => {
                    source_control.set_status(self.status.clone(), entries);
                    changed = true;
                }
                Ok(GitResponse::StatusAndChanges(s, entries)) => {
                    self.status = s.clone();
                    source_control.set_status(s, entries);
                    changed = true;
                }
                Err(_) => break,
            }
        }
        changed
    }
}
