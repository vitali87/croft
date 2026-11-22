//! The background search worker: the requests the panel sends it, the events it answers
//! with, and the pause in typing it waits for before a search starts.

use super::*;
use crate::jobs::{Job, Poll, Start};

/// One unit of streamed output from the search worker, for the search run
/// whose number it carries. The panel keeps only the events of the run it is
/// showing, so nothing from an earlier run lands in it, the same query's
/// included (#2608).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchEvent {
    /// A batch of hits, one batch per file as the parallel walker finds them.
    Hits(u64, Vec<SearchHit>),
    /// The run looked at every file (or its query was empty): its hits are
    /// all of them, so Replace All may go ahead.
    Done(u64),
    /// The run was stopped before it looked at every file, by a request
    /// that came after it: the hits so far are not all of them.
    Cancelled(u64),
    /// The run's scan crashed before it looked at every file. The crash
    /// notice has said so; the hits so far are not all of them (#2611).
    Crashed(u64),
}

/// Work submitted to the search worker. `Query` carries the query string and
/// the toggle state at the moment of submission, so the user can flip a toggle
/// and re-fire the same string with new opts. `SetRoot` rebinds the directory
/// the worker scans, so search always targets the Explorer's current root and
/// its subdirectories after a Make-root / open-folder, never the root captured
/// when the worker was spawned.
#[derive(Clone, Debug)]
pub enum SearchRequest {
    /// Search run `run` for `query` with `opts`. Every event of the run
    /// carries `run`.
    Query {
        run: u64,
        query: String,
        opts: SearchOpts,
    },
    /// Stop the run in flight, or the one waiting for the pause in typing,
    /// which then ends with `Cancelled`: the panel left Search, and the scan
    /// should stop using every core.
    Cancel,
    SetRoot(PathBuf),
    /// Replace the whole root set (multi-root workspaces, #147): the scan
    /// walks every root, so hits come from all workspace folders.
    SetRoots(Vec<PathBuf>),
    /// Snapshot of files open in unsaved (dirty) editors as `(path, content)`
    /// where `content` is the buffer's current text. The worker searches these
    /// in-memory copies and skips their stale disk versions, so an unsaved edit
    /// (e.g. a Rename Symbol) is findable immediately. Like `SetRoot`, this is
    /// state, not a query: it updates the snapshot used by the next scan.
    SetDirtyBuffers(Vec<(PathBuf, String)>),
    /// VS Code's "files to include" / "files to exclude" glob inputs, as raw
    /// comma-separated strings. Like `SetRoot`, this is state, not a query: it
    /// updates the filter the next scan applies. Empty strings clear the
    /// filter.
    SetFilter(String, String),
}

/// How long the user has to pause typing before the search fires. Each
/// fresh keystroke restarts the timer, so a fast typist running through
/// "cla" never triggers searches for "c" or "cl" - only the trailing
/// query gets scanned. This is the standard search-as-you-type pattern
/// (VS Code, GitHub, etc.). 200 ms is in the sweet spot: short enough
/// to feel responsive, long enough that bursts of typing are silent.
pub(super) const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);

/// Background worker loop. Implements trailing-edge debounce: blocks on
/// `recv()` for the first input, then loops on `recv_timeout(200 ms)` -
/// each fresh keystroke restarts the quiet window, so the search fires
/// only after the user has actually paused. The scan runs on a dedicated
/// thread so a new keystroke arriving DURING a scan can cancel it via
/// the shared `Arc<AtomicBool>` and start fresh. Each scan emits per-file
/// `Hits` batches followed by exactly one `Done`, or `Cancelled` when a
/// later request stopped it first, so the receiver always sees a balanced
/// event stream. A run replaced by a newer query before its scan started
/// sends nothing; one stopped by `Cancel` before its scan started sends
/// `Cancelled`. The thread exits cleanly when the channel closes.
pub fn search_worker_loop(
    root: PathBuf,
    rx: std::sync::mpsc::Receiver<SearchRequest>,
    tx: std::sync::mpsc::Sender<SearchEvent>,
) {
    search_worker_loop_with(root, rx, tx, scan_root);
}

/// [`search_worker_loop`], scanning each root with `scan_root_fn`.
pub(super) fn search_worker_loop_with(
    root: PathBuf,
    rx: std::sync::mpsc::Receiver<SearchRequest>,
    tx: std::sync::mpsc::Sender<SearchEvent>,
    scan_root_fn: ScanRoot,
) {
    let mut roots: Vec<PathBuf> = vec![root];
    use std::sync::mpsc::RecvTimeoutError;
    let mut current: Option<Scan> = None;
    // Latest snapshot of unsaved editor buffers, refreshed by SetDirtyBuffers.
    let mut dirty: Vec<(PathBuf, String)> = Vec::new();
    // Latest include/exclude glob filter, refreshed by SetFilter.
    let mut filter = PathFilter::default();
    'requests: while let Some(mut latest) = next_request(&rx, &mut current, &tx) {
        // A new message landed: cancel the in-flight scan (if any) so its
        // worker thread bails out before the user pauses long enough to
        // trigger the next scan.
        if let Some(scan) = current.take() {
            scan.stop(&tx);
        }
        // State updates (root rebind, dirty-buffer snapshot) are not queries
        // and do not debounce: apply and wait for the next message. A
        // `Cancel` has done its work above.
        match latest {
            SearchRequest::SetRoot(new_root) => {
                roots = vec![new_root];
                continue;
            }
            SearchRequest::SetRoots(new_roots) if !new_roots.is_empty() => {
                roots = new_roots;
                continue;
            }
            SearchRequest::SetRoots(_) | SearchRequest::Cancel => continue,
            SearchRequest::SetDirtyBuffers(buffers) => {
                dirty = buffers;
                continue;
            }
            SearchRequest::SetFilter(ref include, ref exclude) => {
                filter = PathFilter::new(include, exclude);
                continue;
            }
            SearchRequest::Query { .. } => {}
        }
        // Trailing-edge debounce: each new keystroke resets the timer.
        // The search fires only after `SEARCH_DEBOUNCE` of silence. A
        // root rebind or dirty-buffer snapshot arriving mid-debounce updates
        // the state without displacing the pending query.
        loop {
            match rx.recv_timeout(SEARCH_DEBOUNCE) {
                Ok(SearchRequest::SetRoot(new_root)) => roots = vec![new_root],
                Ok(SearchRequest::SetRoots(new_roots)) if !new_roots.is_empty() => {
                    roots = new_roots
                }
                Ok(SearchRequest::SetRoots(_)) => {}
                Ok(SearchRequest::SetDirtyBuffers(buffers)) => dirty = buffers,
                Ok(SearchRequest::SetFilter(include, exclude)) => {
                    filter = PathFilter::new(&include, &exclude)
                }
                // The waiting run never starts, and says so.
                Ok(SearchRequest::Cancel) => {
                    if let SearchRequest::Query { run, .. } = latest
                        && tx.send(SearchEvent::Cancelled(run)).is_err()
                    {
                        return;
                    }
                    continue 'requests;
                }
                Ok(newer) => latest = newer,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let SearchRequest::Query { run, query, opts } = latest else {
            continue;
        };
        if as_typed(&query).is_empty() {
            if tx.send(SearchEvent::Done(run)).is_err() {
                return;
            }
            continue;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_thread = cancel.clone();
        let tx_for_thread = tx.clone();
        let roots_for_thread = roots.clone();
        let query_for_thread = query.clone();
        let dirty_for_thread = dirty.clone();
        let filter_for_thread = filter.clone();
        let spawned = Job::spawn("search-scan", Start::Now, move || {
            let tx_for_emit = tx_for_thread.clone();
            // First, search the unsaved buffers in-memory and collect the set
            // of their canonical paths so the disk walk skips the stale copies.
            // Dirty buffers must respect the include/exclude filter too, so a
            // glob that excludes a file hides its unsaved hits as well.
            let mut skip: HashSet<PathBuf> = HashSet::new();
            let mut buf_hits: Vec<SearchHit> = Vec::new();
            for (path, content) in &dirty_for_thread {
                if let Ok(canon) = path.canonicalize() {
                    skip.insert(canon);
                }
                // Filter globs are root-relative: judge against the root
                // that owns the buffer's path (the first containing one;
                // roots rarely nest and the glob answer matches VS Code's
                // folder-relative include semantics either way).
                let owner = roots_for_thread
                    .iter()
                    .find(|r| path.starts_with(r))
                    .unwrap_or(&roots_for_thread[0]);
                if !filter_for_thread.is_empty() && !filter_for_thread.allows(owner, path) {
                    continue;
                }
                collect_matches_in_text(path, content, &query_for_thread, opts, &mut buf_hits);
            }
            if !buf_hits.is_empty() {
                let _ = tx_for_emit.send(SearchEvent::Hits(run, buf_hits));
            }
            // Every workspace root in order: one balanced Hits stream, one
            // Done after the last root (#147).
            for scan_root in &roots_for_thread {
                if cancel_for_thread.load(Ordering::Relaxed) {
                    break;
                }
                scan_root_fn(
                    scan_root,
                    &query_for_thread,
                    opts,
                    &cancel_for_thread,
                    &skip,
                    &filter_for_thread,
                    Emit {
                        tx: tx_for_emit.clone(),
                        run,
                    },
                );
            }
            // A scan stopped part-way has not looked at every file.
            let _ = tx_for_thread.send(if cancel_for_thread.load(Ordering::Relaxed) {
                SearchEvent::Cancelled(run)
            } else {
                SearchEvent::Done(run)
            });
        });
        match spawned {
            Ok(job) => current = Some(Scan { job, cancel, run }),
            // No thread for the scan: the run ends here, as one that crashed.
            Err(_) => {
                if tx.send(SearchEvent::Crashed(run)).is_err() {
                    return;
                }
            }
        }
    }
    if let Some(scan) = current.take() {
        scan.stop(&tx);
    }
}

/// How often the worker looks at the scan in flight while it waits for a
/// request, so a scan that crashed is reported within a moment.
const SCAN_WATCH: std::time::Duration = std::time::Duration::from_millis(100);

/// A scan in flight: its job, the flag that stops it, and its run.
struct Scan {
    job: Job<()>,
    cancel: Arc<AtomicBool>,
    run: u64,
}

impl Scan {
    /// Stop it, and wait until it has ended. One whose thread crashed says
    /// so for its run: the panel may still be waiting on it, as after a
    /// `Cancel` (#2611).
    fn stop(self, tx: &std::sync::mpsc::Sender<SearchEvent>) {
        self.cancel.store(true, Ordering::Relaxed);
        if self.job.wait().is_none() {
            let _ = tx.send(SearchEvent::Crashed(self.run));
        }
    }
}

/// The next request, or `None` once the panel has gone. While it waits it
/// watches the scan in flight, and reports one whose thread crashed as
/// `Crashed` for its run, so the panel never waits on it for good (#2611).
fn next_request(
    rx: &std::sync::mpsc::Receiver<SearchRequest>,
    current: &mut Option<Scan>,
    tx: &std::sync::mpsc::Sender<SearchEvent>,
) -> Option<SearchRequest> {
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        let Some(scan) = current.as_ref() else {
            return rx.recv().ok();
        };
        match rx.recv_timeout(SCAN_WATCH) {
            Ok(request) => return Some(request),
            Err(RecvTimeoutError::Disconnected) => return None,
            Err(RecvTimeoutError::Timeout) => {
                let run = scan.run;
                match scan.job.poll() {
                    Poll::Pending => {}
                    Poll::Done(()) => *current = None,
                    Poll::Died => {
                        *current = None;
                        if tx.send(SearchEvent::Crashed(run)).is_err() {
                            return None;
                        }
                    }
                }
            }
        }
    }
}

/// Where a scan sends one root's hits: the run they belong to.
#[derive(Clone)]
pub(super) struct Emit {
    tx: std::sync::mpsc::Sender<SearchEvent>,
    run: u64,
}

impl Emit {
    fn send(&self, batch: Vec<SearchHit>) {
        let _ = self.tx.send(SearchEvent::Hits(self.run, batch));
    }
}

/// Scan one root, sending each file's hits as they are found: the walker
/// below, or a test's stand-in.
pub(super) type ScanRoot =
    fn(&Path, &str, SearchOpts, &Arc<AtomicBool>, &HashSet<PathBuf>, &PathFilter, Emit);

/// The walker behind [`ScanRoot`].
fn scan_root(
    root: &Path,
    query: &str,
    opts: SearchOpts,
    cancel: &Arc<AtomicBool>,
    skip: &HashSet<PathBuf>,
    filter: &PathFilter,
    emit: Emit,
) {
    search_workspace_streaming_filtered(root, query, opts, cancel, skip, filter, move |batch| {
        emit.send(batch)
    });
}
