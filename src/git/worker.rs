//! The background git worker: the requests the app posts to it, how a burst of them
//! coalesces into one, and the answers it sends back.

use super::*;

/// Unit of work the App posts to the background git worker. Coalesced
/// inside the loop so a burst of clicks / FS-watcher ticks turns into a
/// single shell-out instead of N. `SetRoot` rebinds the worker's working
/// directory in-place — used by `App::change_workspace_root` so a Make
/// Root that lands inside a git repo can flip the Source Control panel
/// out of its no-repo empty state without recreating the worker thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitRequest {
    Status,
    StatusAndChanges,
    SetRoot(PathBuf),
}

impl GitRequest {
    /// Merge two pending *query* requests into the strongest one.
    /// `StatusAndChanges` dominates a bare `Status`; same+same is the same.
    /// `SetRoot` is never a query and the worker drains it inline before
    /// merging, so it must not appear here.
    pub fn merge(self, other: Self) -> Self {
        use GitRequest::*;
        match (self, other) {
            (SetRoot(_), _) | (_, SetRoot(_)) => {
                unreachable!(
                    "SetRoot is drained inline before merge; it cannot reach the query merge"
                )
            }
            (StatusAndChanges, _) | (_, StatusAndChanges) => StatusAndChanges,
            (Status, Status) => Status,
        }
    }
}

/// Result the worker ships back. The variants mirror `GitRequest` 1:1
/// so the App can route each response to the right consumer without an
/// extra request-id channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitResponse {
    Status(GitStatus),
    StatusAndChanges(GitStatus, Vec<ChangeEntry>),
    /// A `SetRoot` was taken: every answer after this reads the new root,
    /// and any before it read the old one, so the App drops those (#871).
    RootSet,
}

/// Background worker loop. Reads `GitRequest`s from `rx`, coalesces a
/// burst of pending requests into a single shell-out, runs `query` /
/// `query_changes` off the UI thread, and ships the result back via
/// `tx`. Terminates cleanly when either channel closes.
///
/// Croft was rewritten to Rust for raw input latency. `git status` on a
/// fresh shell adds 15-50 ms (process spawn + scanning the tree) — fine
/// off-thread, never on the hot path of a sidebar-icon click. The same
/// channel gate is used by both the click path (`refresh_source_control`)
/// and the FS-watcher tick (`refresh_git_status_debounced`).
pub fn git_worker_loop(
    initial_root: PathBuf,
    rx: std::sync::mpsc::Receiver<GitRequest>,
    tx: std::sync::mpsc::Sender<GitResponse>,
) {
    let mut root = initial_root;
    while let Ok(first) = rx.recv() {
        let mut pending: Option<GitRequest> = match first {
            GitRequest::SetRoot(p) => {
                root = p;
                if tx.send(GitResponse::RootSet).is_err() {
                    return;
                }
                None
            }
            other => Some(other),
        };
        while let Ok(newer) = rx.try_recv() {
            match newer {
                GitRequest::SetRoot(p) => {
                    root = p;
                    if tx.send(GitResponse::RootSet).is_err() {
                        return;
                    }
                }
                other => {
                    pending = Some(match pending {
                        Some(prev) => prev.merge(other),
                        None => other,
                    });
                }
            }
        }
        let Some(req) = pending else {
            continue;
        };
        let resp = match req {
            GitRequest::Status => GitResponse::Status(query(&root)),
            GitRequest::StatusAndChanges => {
                let s = query(&root);
                let c = query_changes(&root);
                GitResponse::StatusAndChanges(s, c)
            }
            GitRequest::SetRoot(_) => unreachable!("SetRoot was drained inline"),
        };
        if tx.send(resp).is_err() {
            return;
        }
    }
}
