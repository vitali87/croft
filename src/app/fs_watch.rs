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
    poll_open_file_mtime: Option<(PathBuf, Option<(SystemTime, u64)>)>,
}

impl FsWatch {
    pub fn spawn(root: &Path, tree: &FileTree) -> Self {
        Self {
            _watcher: None,
            rx: None,
            init_rx: Some(Self::start_watcher_thread(root)),
            poll_last_check: Instant::now(),
            poll_interval: FS_POLL_INTERVAL,
            poll_dir_mtimes: Self::snapshot_expanded_dir_mtimes(tree),
            poll_open_file_mtime: None,
        }
    }

    pub fn rebind(&mut self, root: &Path, tree: &FileTree) {
        self._watcher = None;
        self.rx = None;
        self.init_rx = Some(Self::start_watcher_thread(root));
        self.poll_dir_mtimes = Self::snapshot_expanded_dir_mtimes(tree);
    }

    pub fn disable(&mut self) {
        self._watcher = None;
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

    pub fn sync_open_file_mtime(&mut self, open_path: Option<&Path>) {
        self.poll_open_file_mtime = open_path.map(|p| (p.to_path_buf(), Self::file_stamp(p)));
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
                    if let Some(dir) = affected_dir_for_event(path, &tree.root) {
                        affected.insert(dir);
                    } else if path == &tree.root
                        || path.canonicalize().ok().as_deref()
                            == tree.root.canonicalize().ok().as_deref()
                    {
                        affected.insert(tree.root.clone());
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

    pub fn poll(&mut self, tree: &mut FileTree, open_path: Option<&Path>) -> FsPoll {
        let mut out = FsPoll::default();
        if self.poll_last_check.elapsed() < self.poll_interval {
            return out;
        }
        let poll_start = Instant::now();
        self.poll_last_check = poll_start;
        out.open_file_changed = self.poll_open_file_change(open_path);
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

    fn poll_open_file_change(&mut self, open_path: Option<&Path>) -> bool {
        let current = open_path.map(|p| (p.to_path_buf(), Self::file_stamp(p)));
        let changed = match (&self.poll_open_file_mtime, &current) {
            (Some((old_path, old_stamp)), Some((path, stamp))) if old_path == path => {
                old_stamp != stamp
            }
            _ => {
                self.poll_open_file_mtime = current;
                return false;
            }
        };
        self.poll_open_file_mtime = current;
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

    fn start_watcher_thread(root: &Path) -> Receiver<FsWatcherInit> {
        let (init_tx, init_rx) = std::sync::mpsc::channel();
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            if let Ok(pair) = Self::spawn_watcher(&root) {
                let _ = init_tx.send(pair);
            }
        });
        init_rx
    }

    pub(super) fn spawn_watcher(root: &Path) -> Result<FsWatcherInit> {
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
            // Depth-aware pruning. The old code only checked the root's
            // *immediate* children for noise, so opening a directory that is
            // a PARENT of repos (e.g. ~/Documents) found no top-level
            // node_modules/.git/target, fell through to a single recursive
            // watch over the whole tree, and re-subscribed every repo's write
            // storm one level down (confirmed by `sample`: five fsevents
            // loops + a mutex-bound debouncer at ~45% CPU, UI frozen).
            //
            // Instead, walk the tree (descent stops at noise dirs, so their
            // subtrees are never read) and install a recursive watch only at
            // the root of each maximal noise-free subtree, plus a
            // non-recursive watch on every "boundary" directory that contains
            // noise so its own direct entries are still observed. A noise dir
            // is therefore never covered by any FSEvents stream at any depth,
            // and the stream count stays in the low hundreds instead of one
            // pathological recursive watch — keeping FSEventStreamCreate well
            // under the per-tree count that pins a core.
            let mut targets: Vec<(PathBuf, RecursiveMode)> = Vec::new();
            if collect_macos_watch_targets(root, &mut targets) {
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
            if targets.len() > MAX_WATCHES {
                targets.sort_by_key(|(p, _)| p.components().count());
                targets.truncate(MAX_WATCHES);
            }
            // The root itself must always be watchable, or `spawn_watcher`
            // would succeed having subscribed to nothing on a clean-but-empty
            // root; mirror the previous contract of failing loudly if even
            // the root cannot be watched.
            if targets.is_empty() {
                debouncer
                    .watch(root, RecursiveMode::NonRecursive)
                    .context("starting non-recursive watch on workspace root")?;
            } else {
                for (path, mode) in targets {
                    let _ = debouncer.watch(&path, mode);
                }
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
            let mut stack = vec![root.to_path_buf()];
            let mut watched = 0usize;
            while let Some(dir) = stack.pop() {
                if debouncer.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
                    watched += 1;
                }
                if watched >= MAX_WATCHES {
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
/// non-noise file beneath it without ever subscribing to a noise subtree at
/// any depth (descent stops at noise dirs, so their subtrees are never even
/// read). Returns `true` when `dir`'s entire subtree is noise-free, deferring
/// to the caller to install one recursive watch at the subtree's highest
/// point (a single stream); returns `false` when `dir` is a boundary, having
/// already pushed a non-recursive watch on `dir` itself plus a recursive
/// watch on each maximal noise-free child subtree.
#[cfg(target_os = "macos")]
fn collect_macos_watch_targets(
    dir: &Path,
    targets: &mut Vec<(PathBuf, notify::RecursiveMode)>,
) -> bool {
    use notify::RecursiveMode;
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
            let clean = collect_macos_watch_targets(&sd, targets);
            (sd, clean)
        })
        .collect();
    if !has_noise && child_clean.iter().all(|(_, clean)| *clean) {
        return true;
    }
    targets.push((dir.to_path_buf(), RecursiveMode::NonRecursive));
    for (sd, clean) in child_clean {
        if clean {
            targets.push((sd, RecursiveMode::Recursive));
        }
    }
    false
}

fn event_mutates_content(kind: &notify::EventKind) -> bool {
    use notify::EventKind;
    use notify::event::ModifyKind;
    !matches!(
        kind,
        EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(_))
    )
}
