use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};

use crate::widgets::editor::EditorTabs;
use crate::widgets::file_finder::{is_noise_dir, is_path_under_noise_dir};
use crate::widgets::file_tree::{FileTree, affected_dir_for_event};

const FS_POLL_INTERVAL: Duration = Duration::from_millis(50);

const FS_WATCH_PROTECTED_NAMES: &[&str] = &["Library", ".Trash"];

/// Max directories the macOS watch-target walk may visit before it stops and
/// leaves the remainder to the adaptive poll. Bounds the startup/rebind scan on
/// a parent-of-repos root (~/Documents measured at ~140k dirs / 7.6s) to a
/// fraction of a second, while comfortably covering any normal single repo in
/// full. See `collect_macos_watch_targets`.
///
/// macOS-only, like its one use site: on Linux the inotify backend needs no
/// such walk, and an unconditional const is a `dead_code` error there under
/// `-D warnings`.
#[cfg(target_os = "macos")]
const WATCH_WALK_DIR_BUDGET: usize = 8_192;

type FsWatcherInit = (
    notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
    Receiver<notify_debouncer_full::DebounceEventResult>,
);

#[derive(Default)]
pub struct FsPoll {
    pub open_file_changed: bool,
    pub dirs_changed: bool,
    pub finder_relevant: bool,
}

#[derive(Default)]
pub struct FsDrain {
    pub got_any: bool,
    pub touched_open_file: bool,
    pub dirs_changed: bool,
    pub finder_relevant: bool,
}

pub struct FsWatch {
    _watcher: Option<
        notify_debouncer_full::Debouncer<
            notify::RecommendedWatcher,
            notify_debouncer_full::RecommendedCache,
        >,
    >,
    rx: Option<Receiver<notify_debouncer_full::DebounceEventResult>>,
    init_rx: Option<Receiver<FsWatcherInit>>,
    poll_last_check: Instant,
    poll_interval: Duration,
    poll_dir_mtimes: BTreeMap<PathBuf, Option<SystemTime>>,
    /// Per-file stamps for EVERY open tab, not just the focused one. A file
    /// sitting directly in a boundary dir gets no watch at all (that is the
    /// iron law that keeps the FSEvents storm dead) and an in-place write does
    /// not move its parent dir's mtime, so this stat is the only thing that can
    /// see it change. Bounded by the number of open tabs, once per poll tick.
    poll_open_files: BTreeMap<PathBuf, Option<(SystemTime, u64)>>,
    /// The root this instance watches. Multi-root workspaces (#147) run
    /// one `FsWatch` per workspace root, and `drain` must attribute each
    /// event against ITS root — attributing against the shared tree's
    /// primary root silently dropped every event from a secondary root.
    watch_root: PathBuf,
}

impl FsWatch {
    pub fn spawn(root: &Path, tree: &FileTree) -> Self {
        Self::spawn_sharing(root, tree, 1)
    }

    /// [`spawn`] with the per-OS watch caps divided by `shares` — the
    /// number of workspace roots (#147). The caps exist to bound kernel
    /// watch descriptors and stream setup per PROCESS, so N concurrent
    /// instances must split one budget rather than multiply it.
    pub fn spawn_sharing(root: &Path, tree: &FileTree, shares: usize) -> Self {
        Self {
            _watcher: None,
            rx: None,
            init_rx: Some(Self::start_watcher_thread(root, shares)),
            poll_last_check: Instant::now(),
            poll_interval: FS_POLL_INTERVAL,
            poll_dir_mtimes: Self::snapshot_expanded_dir_mtimes(tree),
            poll_open_files: BTreeMap::new(),
            watch_root: root.to_path_buf(),
        }
    }

    /// A SECONDARY root's watcher (#147): events only. The PRIMARY
    /// instance owns the adaptive poll for the whole tree — its dir-mtime
    /// snapshot walks every root's expanded rows, covering secondary
    /// roots wherever their events fall past the caps — so an extra
    /// instance snapshotting the tree again would only duplicate stat
    /// work if its `poll` were ever called (#148 review).
    pub fn spawn_event_only(root: &Path, shares: usize) -> Self {
        Self {
            _watcher: None,
            rx: None,
            init_rx: Some(Self::start_watcher_thread(root, shares)),
            poll_last_check: Instant::now(),
            poll_interval: FS_POLL_INTERVAL,
            poll_dir_mtimes: BTreeMap::new(),
            poll_open_files: BTreeMap::new(),
            watch_root: root.to_path_buf(),
        }
    }

    pub fn rebind(&mut self, root: &Path, tree: &FileTree) {
        self.rebind_sharing(root, tree, 1)
    }

    /// [`rebind`] with the shared-budget divisor, like [`spawn_sharing`].
    pub fn rebind_sharing(&mut self, root: &Path, tree: &FileTree, shares: usize) {
        offload_drop(self._watcher.take());
        self.rx = None;
        self.init_rx = Some(Self::start_watcher_thread(root, shares));
        self.poll_dir_mtimes = Self::snapshot_expanded_dir_mtimes(tree);
        self.watch_root = root.to_path_buf();
    }

    pub fn disable(&mut self) {
        offload_drop(self._watcher.take());
        self.rx = None;
        self.init_rx = None;
    }

    pub fn try_install_watcher(&mut self) -> bool {
        let Some(rx) = self.init_rx.as_ref() else {
            return false;
        };
        match rx.try_recv() {
            Ok((w, evrx)) => {
                self._watcher = Some(w);
                self.rx = Some(evrx);
                self.init_rx = None;
                true
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.init_rx = None;
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
        }
    }

    pub fn sync_open_file_mtime(&mut self, open_paths: &[PathBuf]) {
        self.poll_open_files = Self::stamp_all(open_paths);
    }

    fn stamp_all(paths: &[PathBuf]) -> BTreeMap<PathBuf, Option<(SystemTime, u64)>> {
        paths
            .iter()
            .map(|p| (p.clone(), Self::file_stamp(p)))
            .collect()
    }

    pub fn drain(&mut self, tree: &mut FileTree, editor: &EditorTabs) -> FsDrain {
        let mut out = FsDrain::default();
        let Some(rx) = self.rx.as_ref() else {
            return out;
        };
        let mut affected: BTreeSet<PathBuf> = BTreeSet::new();
        while let Ok(result) = rx.try_recv() {
            out.got_any = true;
            let events = match result {
                Ok(evs) => evs,
                Err(_) => continue,
            };
            for ev in events {
                let mutates_content = event_mutates_content(&ev.event.kind);
                for path in &ev.event.paths {
                    if mutates_content && editor.matches_open_path(path) {
                        out.touched_open_file = true;
                    }
                    if let Some(dir) = affected_dir_for_event(path, &self.watch_root) {
                        affected.insert(dir);
                    } else if path == &self.watch_root
                        || path.canonicalize().ok().as_deref()
                            == self.watch_root.canonicalize().ok().as_deref()
                    {
                        affected.insert(self.watch_root.clone());
                    }
                }
            }
        }
        if !affected.is_empty() {
            for dir in affected.iter().rev() {
                if let Some(idx) = tree.index_of_dir(dir) {
                    tree.refresh_children(idx);
                } else if let Ok(c) = dir.canonicalize()
                    && let Some(idx) = tree.index_of_dir(&c)
                {
                    tree.refresh_children(idx);
                }
            }
            self.poll_dir_mtimes = Self::snapshot_expanded_dir_mtimes(tree);
            out.dirs_changed = true;
            out.finder_relevant = affected.iter().any(|p| !is_path_under_noise_dir(p));
        }
        out
    }

    /// Whether the next `poll` would do any work. `drain_fs_events` runs every
    /// frame while the poll fires at most every `FS_POLL_INTERVAL`, so the
    /// caller checks this before collecting the open-tab paths — otherwise the
    /// frame pays a Vec of PathBuf clones just to have `poll` drop it.
    pub fn poll_due(&self) -> bool {
        self.poll_last_check.elapsed() >= self.poll_interval
    }

    pub fn poll(&mut self, tree: &mut FileTree, open_paths: &[PathBuf]) -> FsPoll {
        let mut out = FsPoll::default();
        if !self.poll_due() {
            return out;
        }
        let poll_start = Instant::now();
        self.poll_last_check = poll_start;
        out.open_file_changed = self.poll_open_file_change(open_paths);
        let current = Self::snapshot_expanded_dir_mtimes(tree);
        let changed_dirs: Vec<PathBuf> = current
            .iter()
            .filter_map(|(path, stamp)| {
                if self.poll_dir_mtimes.get(path) == Some(stamp) {
                    None
                } else {
                    Some(path.clone())
                }
            })
            .collect();
        if changed_dirs.is_empty() {
            self.poll_dir_mtimes = current;
            self.poll_interval = Self::back_off(poll_start);
            return out;
        }
        for dir in changed_dirs.iter().rev() {
            if let Some(idx) = tree.index_of_dir(dir) {
                tree.refresh_children(idx);
            }
        }
        self.poll_dir_mtimes = Self::snapshot_expanded_dir_mtimes(tree);
        out.dirs_changed = true;
        out.finder_relevant = changed_dirs.iter().any(|p| !is_path_under_noise_dir(p));
        self.poll_interval = Self::back_off(poll_start);
        out
    }

    /// True when any file backing an open tab has a different stamp than the
    /// last poll saw. A path we have no previous stamp for (a tab just opened)
    /// is only baselined — it is not a change — and a path that has gone away
    /// with its tab simply drops out.
    fn poll_open_file_change(&mut self, open_paths: &[PathBuf]) -> bool {
        let current = Self::stamp_all(open_paths);
        let changed = current.iter().any(
            |(path, stamp)| matches!(self.poll_open_files.get(path), Some(old) if old != stamp),
        );
        self.poll_open_files = current;
        changed
    }

    fn back_off(poll_start: Instant) -> Duration {
        poll_start
            .elapsed()
            .saturating_mul(10)
            .clamp(FS_POLL_INTERVAL, Duration::from_secs(10))
    }

    fn snapshot_expanded_dir_mtimes(tree: &FileTree) -> BTreeMap<PathBuf, Option<SystemTime>> {
        tree.nodes
            .iter()
            .filter(|n| n.is_dir && n.expanded)
            .map(|n| (n.path.clone(), Self::dir_modified(&n.path)))
            .collect()
    }

    fn dir_modified(path: &Path) -> Option<SystemTime> {
        std::fs::metadata(path).and_then(|m| m.modified()).ok()
    }

    fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
        let meta = std::fs::metadata(path).ok()?;
        let modified = meta.modified().ok()?;
        Some((modified, meta.len()))
    }

    fn start_watcher_thread(root: &Path, shares: usize) -> Receiver<FsWatcherInit> {
        let (init_tx, init_rx) = std::sync::mpsc::channel();
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            if let Ok(pair) = Self::spawn_watcher_sharing(&root, shares) {
                let _ = init_tx.send(pair);
            }
        });
        init_rx
    }

    #[cfg(test)]
    pub(super) fn spawn_watcher(root: &Path) -> Result<FsWatcherInit> {
        Self::spawn_watcher_sharing(root, 1)
    }

    pub(super) fn spawn_watcher_sharing(root: &Path, shares: usize) -> Result<FsWatcherInit> {
        use notify::RecursiveMode;
        use notify_debouncer_full::new_debouncer;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debouncer = new_debouncer(Duration::from_millis(100), None, tx)
            .context("creating filesystem watcher")?;
        // The watcher backend is fundamentally different on the two
        // first-class targets, so the install strategy MUST differ too -
        // a single uniform strategy is pathological on one of them:
        //
        //  * macOS / FSEvents is per-TREE. One recursive watch covers an
        //    entire subtree in a single stream with no kernel watch limit,
        //    and every `watch()` call tears down and rebuilds that stream.
        //    Calling `watch()` once per directory therefore fires thousands
        //    of FSEventStreamCreate calls and pins a core at ~100% forever
        //    (confirmed by `sample` 2026-05: the FSEvents thread sits in
        //    FSEventStreamCreate). So on macOS we make as FEW calls as
        //    possible.
        //
        //  * Linux / inotify is per-DIRECTORY. A recursive watch installs
        //    one watch per dir in the subtree, and a real repo's
        //    node_modules/target/.git push past
        //    /proc/sys/fs/inotify/max_user_watches (often 8192 on a VPS),
        //    returning ENOSPC so NO watcher installs and the main loop
        //    falls back to a blocking stat-walk that freezes typing over
        //    SSH. Each inotify_add_watch is cheap and independent (no
        //    stream rebuild), so on Linux we add MANY non-recursive watches.
        //
        // Both branches prune the same protected + noise dirs (target/,
        // node_modules/, .git/, ~/Library Containers) which generate
        // cargo/npm/git write storms the debouncer's FileIdMap memcmp loop
        // can't keep up with, and which on macOS also trip Sonoma's App
        // Management TCC class.
        #[cfg(target_os = "macos")]
        {
            // Depth-aware pruning. We install a recursive watch only at the
            // root of each maximal noise-free subtree, and NEVER root a stream
            // at or above a noise dir (target/, node_modules/, .git/).
            //
            // Why "never above" and not "non-recursive on the boundary": on
            // macOS there is no such thing as a non-recursive FSEvents watch.
            // notify implements RecursiveMode::NonRecursive as a fully
            // recursive FSEventStream rooted at the path, then DISCARDS events
            // whose parent isn't the path — after the kernel has already
            // delivered them and the callback has paid to filter them
            // (notify 8.2.0 src/fsevent.rs: the `recursive_info`
            // `path.starts_with` loop runs for every event in the subtree).
            // So a non-recursive watch on a boundary dir like ~/Documents or
            // a repo root still streams every target/ and node_modules/ write
            // beneath it into the fsevents-loop thread, pinning a core during
            // any cargo/npm build even though zero of those events reach the
            // debouncer. The old code's boundary watches did exactly this; the
            // regression hid because the tests only asserted zero *debouncer*
            // events, never the callback CPU.
            //
            // The boundary dirs' own direct entries (a new top-level file, a
            // Cargo.toml edit) are instead covered by the adaptive-backoff
            // poll, which stats only the expanded/visible dirs — cheap, and
            // already the fallback for everything not under a watch.
            let mut targets: Vec<(PathBuf, RecursiveMode)> = Vec::new();
            // Cap the discovery walk so opening a parent-of-repos root (e.g.
            // ~/Documents, ~140k non-noise dirs → a 7.6s scan) can't turn the
            // watcher thread into a multi-second readdir burst. A typical single
            // repo is far under this, so its behaviour is unchanged; a tree that
            // exceeds it gets watches on the subtrees discovered before the cap
            // and the adaptive poll covers the rest.
            let mut budget = WATCH_WALK_DIR_BUDGET / shares.max(1);
            if collect_macos_watch_targets(root, &mut targets, &mut budget) {
                // Whole tree is noise-free (e.g. a small repo with no
                // node_modules): one recursive watch covers it in a single
                // stream, exactly as before.
                targets.push((root.to_path_buf(), RecursiveMode::Recursive));
            }
            // Safety net: if a pathological tree still produces a huge number
            // of streams, keep the shallowest (broadest-coverage) ones and let
            // the adaptive-backoff poll fallback cover the rest, rather than
            // spending forever in FSEventStreamCreate.
            const MAX_WATCHES: usize = 2_000;
            let max_watches = MAX_WATCHES / shares.max(1);
            if targets.len() > max_watches {
                targets.sort_by_key(|(p, _)| p.components().count());
                targets.truncate(max_watches);
            }
            // An empty target list here means `collect` returned `false`: the
            // root is a boundary that contains noise but has no noise-free
            // subtree to anchor a stream on (e.g. a repo that is only .git +
            // node_modules + loose files). The OLD code fell back to a
            // non-recursive watch on the root — which on macOS is a recursive
            // stream over exactly that noise, the storm we're killing. So in
            // that case we install NOTHING and let the adaptive poll cover the
            // root's (few, visible) entries. Watching nothing is correct, not a
            // failure: the poll is the floor for everything outside a watch.
            for (path, mode) in targets {
                let _ = debouncer.watch(&path, mode);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let is_skippable = |name: &std::ffi::OsStr| -> bool {
                FS_WATCH_PROTECTED_NAMES.iter().any(|n| name == *n) || is_noise_dir(name)
            };
            // Hard ceiling so a pathological tree can't spend forever
            // issuing inotify_add_watch syscalls on the init thread;
            // anything beyond is covered by the adaptive-backoff poll
            // fallback. Per-dir failures are tolerated (we keep going), so
            // a tree that still exceeds the limit installs with partial
            // coverage instead of nothing.
            const MAX_WATCHES: usize = 50_000;
            let max_watches = MAX_WATCHES / shares.max(1);
            // A zero share (pathologically many roots) installs NOTHING:
            // the loop below tests the cap only after a watch call, so
            // entering it would grant every instance one descriptor past
            // the shared budget (#148 review). The adaptive poll is the
            // floor, exactly as for a boundary dir.
            if max_watches == 0 {
                return Ok((debouncer, rx));
            }
            let mut stack = vec![root.to_path_buf()];
            let mut watched = 0usize;
            while let Some(dir) = stack.pop() {
                if debouncer.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
                    watched += 1;
                }
                if watched >= max_watches {
                    break;
                }
                let Ok(rd) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in rd.filter_map(Result::ok) {
                    if is_skippable(&entry.file_name()) {
                        continue;
                    }
                    // `file_type()` does not follow symlinks, so a symlinked
                    // directory reports `is_dir() == false` and we never
                    // descend into it - that also rules out symlink cycles.
                    if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                        stack.push(entry.path());
                    }
                }
            }
        }
        Ok((debouncer, rx))
    }

    #[cfg(test)]
    pub fn set_test_events(&mut self, rx: Receiver<notify_debouncer_full::DebounceEventResult>) {
        self._watcher = None;
        self.init_rx = None;
        self.rx = Some(rx);
    }

    #[cfg(test)]
    pub fn force_poll_due(&mut self) {
        self.poll_last_check = Instant::now()
            .checked_sub(self.poll_interval)
            .unwrap_or_else(Instant::now);
    }

    #[cfg(test)]
    pub fn clear_dir_mtimes(&mut self) {
        self.poll_dir_mtimes.clear();
    }
}

/// True for a directory name croft must never watch: a protected macOS dir
/// (`Library`/`.Trash`, which trip Sonoma's App Management TCC class) or a
/// build/VCS noise dir (`node_modules`/`target`/`.git`/…) whose cargo/npm/git
/// write storms the debouncer's FileIdMap loop cannot keep up with.
#[cfg(target_os = "macos")]
fn is_skippable_name(name: &std::ffi::OsStr) -> bool {
    FS_WATCH_PROTECTED_NAMES.iter().any(|n| name == *n) || is_noise_dir(name)
}

/// Walk `dir` and append the FSEvents watch targets that cover every
/// non-noise file beneath it without ever rooting a stream at or above a noise
/// subtree at any depth (descent stops at noise dirs, so their subtrees are
/// never even read). Returns `true` when `dir`'s entire subtree is noise-free,
/// deferring to the caller to install one recursive watch at the subtree's
/// highest point (a single stream); returns `false` when `dir` is a boundary
/// (it contains noise somewhere beneath it), having pushed a recursive watch
/// on each maximal noise-free child subtree. A boundary dir is deliberately
/// NOT watched itself: on macOS a non-recursive watch is still a recursive
/// FSEventStream (see `spawn_watcher`), so watching a boundary would re-stream
/// the very noise we descended past. Its direct entries fall to the poll.
#[cfg(target_os = "macos")]
pub(super) fn collect_macos_watch_targets(
    dir: &Path,
    targets: &mut Vec<(PathBuf, notify::RecursiveMode)>,
    budget: &mut usize,
) -> bool {
    use notify::RecursiveMode;
    // Bound the discovery walk. A parent-of-repos root (e.g. ~/Documents) can
    // hold 100k+ non-noise dirs; walking all of them to confirm they are
    // noise-free costs seconds on the watcher thread. When the budget is spent
    // we report the subtree as NOT clean, so no recursive stream is ever rooted
    // over a tree we didn't finish verifying (the iron law: never watch above
    // unseen noise). The adaptive poll, which already stats every expanded dir
    // each cycle, is the floor for whatever we stop short of watching.
    // ponytail: a flat dir-visit cap; a huge *single* clean repo past the cap
    // falls back to poll-only instead of one recursive watch. Raise the cap if
    // that ever matters; typical single repos are far under it.
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    let mut has_noise = false;
    let mut subdirs: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.filter_map(Result::ok) {
            if is_skippable_name(&entry.file_name()) {
                has_noise = true;
                continue;
            }
            // `file_type()` does not follow symlinks, so a symlinked dir
            // reports `is_dir() == false` and we never descend — that also
            // rules out symlink cycles (e.g. the ~/Library group-container
            // loops that flooded the basedpyright crawl).
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                subdirs.push(entry.path());
            }
        }
    }
    let child_clean: Vec<(PathBuf, bool)> = subdirs
        .into_iter()
        .map(|sd| {
            let clean = collect_macos_watch_targets(&sd, targets, budget);
            (sd, clean)
        })
        .collect();
    if !has_noise && child_clean.iter().all(|(_, clean)| *clean) {
        return true;
    }
    for (sd, clean) in child_clean {
        if clean {
            targets.push((sd, RecursiveMode::Recursive));
        }
    }
    false
}

impl Drop for FsWatch {
    // Dropping the whole watcher (e.g. on app teardown or a workspace swap that
    // replaces the `App`) must offload the notify `Debouncer` exactly like
    // `rebind`/`disable` do; otherwise a `_watcher` still `Some` at drop runs
    // `FsEventWatcher::stop()` inline and can busy-spin the UI thread (see
    // `offload_drop`). `take` leaves `None`, so the field's own drop is a no-op.
    fn drop(&mut self) {
        offload_drop(self._watcher.take());
    }
}

// VA 2026-06-23: Drop `value` on a detached background thread instead of
// inline. Dropping a notify FSEvents watcher runs FsEventWatcher::stop(),
// which busy-spins `while CFRunLoopIsWaiting(runloop) == 0 { thread::yield_now() }`
// (notify 8.2.0, src/fsevent.rs) until the watcher's CFRunLoop thread parks.
// When that thread is mid MustScanSubDirs rescan the spin never ends, so doing
// it on the UI thread (change_workspace_root -> rebind, or disable) freezes the
// whole app and pins a core. Offloading keeps the spin on a throwaway thread the
// UI never joins; `T: Send + 'static` lets the watcher move across the boundary.
pub(super) fn offload_drop<T: Send + 'static>(value: T) {
    std::thread::spawn(move || drop(value));
}

fn event_mutates_content(kind: &notify::EventKind) -> bool {
    use notify::EventKind;
    use notify::event::ModifyKind;
    !matches!(
        kind,
        EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(_))
    )
}
