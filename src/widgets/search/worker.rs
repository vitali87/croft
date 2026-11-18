//! The background search worker: the requests the panel sends it, the events it answers
//! with, and the pause in typing it waits for before a search starts.

use super::*;

/// One unit of streamed output from the search worker. The query and opts
/// are echoed back on every event so the receiver can drop stale events
/// when the user has typed past or flipped a toggle since the scan started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchEvent {
    /// A batch of hits, one batch per file as the parallel walker finds them.
    Hits(String, SearchOpts, Vec<SearchHit>),
    /// Sentinel emitted exactly once per submitted request when the scan
    /// finishes (or short-circuits on an empty query). UI uses this to flip
    /// the "(searching…)" caption off.
    Done(String, SearchOpts),
}

/// Work submitted to the search worker. `Query` carries the query string and
/// the toggle state at the moment of submission, so the user can flip a toggle
/// and re-fire the same string with new opts. `SetRoot` rebinds the directory
/// the worker scans, so search always targets the Explorer's current root and
/// its subdirectories after a Make-root / open-folder, never the root captured
/// when the worker was spawned.
#[derive(Clone, Debug)]
pub enum SearchRequest {
    Query(String, SearchOpts),
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
/// `Hits` batches followed by exactly one `Done`, even when canceled,
/// so the receiver always sees a balanced event stream. The thread exits
/// cleanly when the channel closes.
pub fn search_worker_loop(
    root: PathBuf,
    rx: std::sync::mpsc::Receiver<SearchRequest>,
    tx: std::sync::mpsc::Sender<SearchEvent>,
) {
    let mut roots: Vec<PathBuf> = vec![root];
    use std::sync::mpsc::RecvTimeoutError;
    let mut current: Option<(std::thread::JoinHandle<()>, Arc<AtomicBool>)> = None;
    // Latest snapshot of unsaved editor buffers, refreshed by SetDirtyBuffers.
    let mut dirty: Vec<(PathBuf, String)> = Vec::new();
    // Latest include/exclude glob filter, refreshed by SetFilter.
    let mut filter = PathFilter::default();
    while let Ok(mut latest) = rx.recv() {
        // A new message landed: cancel the in-flight scan (if any) so its
        // worker thread bails out before the user pauses long enough to
        // trigger the next scan.
        if let Some((handle, cancel)) = current.take() {
            cancel.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
        // State updates (root rebind, dirty-buffer snapshot) are not queries
        // and do not debounce: apply and wait for the next message.
        match latest {
            SearchRequest::SetRoot(new_root) => {
                roots = vec![new_root];
                continue;
            }
            SearchRequest::SetRoots(new_roots) if !new_roots.is_empty() => {
                roots = new_roots;
                continue;
            }
            SearchRequest::SetRoots(_) => continue,
            SearchRequest::SetDirtyBuffers(buffers) => {
                dirty = buffers;
                continue;
            }
            SearchRequest::SetFilter(ref include, ref exclude) => {
                filter = PathFilter::new(include, exclude);
                continue;
            }
            SearchRequest::Query(..) => {}
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
                Ok(newer) => latest = newer,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let SearchRequest::Query(query, opts) = latest else {
            continue;
        };
        if as_typed(&query).is_empty() {
            if tx.send(SearchEvent::Done(query, opts)).is_err() {
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
        let handle = std::thread::spawn(move || {
            let q_for_emit = query_for_thread.clone();
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
                let _ = tx_for_emit.send(SearchEvent::Hits(q_for_emit.clone(), opts, buf_hits));
            }
            // Every workspace root in order: one balanced Hits stream, one
            // Done after the last root (#147).
            for scan_root in &roots_for_thread {
                if cancel_for_thread.load(Ordering::Relaxed) {
                    break;
                }
                let tx_for_root = tx_for_emit.clone();
                let q_for_root = q_for_emit.clone();
                search_workspace_streaming_filtered(
                    scan_root,
                    &query_for_thread,
                    opts,
                    &cancel_for_thread,
                    &skip,
                    &filter_for_thread,
                    move |batch| {
                        let _ =
                            tx_for_root.send(SearchEvent::Hits(q_for_root.clone(), opts, batch));
                    },
                );
            }
            let _ = tx_for_thread.send(SearchEvent::Done(query_for_thread, opts));
        });
        current = Some((handle, cancel));
    }
    if let Some((handle, cancel)) = current.take() {
        cancel.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }
}
