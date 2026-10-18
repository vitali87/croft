//! Opening, saving and reloading files: encodings, external changes, the
//! filesystem watcher and `croft view`.

use super::*;

/// #1157: `croft notes.md` for a file not written yet opens a tab for it
/// that says the save creates it; the save does.
#[test]
fn a_file_not_written_yet_opens_at_launch_and_its_save_creates_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.md");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_at_launch(&path);
    assert_eq!(app.editor.path.as_deref(), Some(path.as_path()));
    assert!(app.status.contains("saving creates it"), "{}", app.status);
    assert!(!path.exists());
    app.editor.insert_str("# Notes");
    app.write_current_to_disk();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().trim_end(),
        "# Notes"
    );

    // Negative: a missing folder opens nothing and says why.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_at_launch(&tmp.path().join("gone/new.md"));
    assert!(app.editor.path.is_none());
    assert!(app.status.contains("Couldn't open"), "{}", app.status);
}

#[test]
fn fs_watcher_does_not_descend_into_protected_top_level_dirs() {
    // Regression for the macOS App Management TCC prompt: when the
    // workspace contains a `Library` subdir at the top level (as $HOME
    // does), the watcher must not call WalkDir+stat into it. Asserted
    // here by making `Library` unreadable (mode 000): a recursive
    // watch would fail spawning because notify_debouncer_full would
    // hit a permission error inside the walk; our split-watch path
    // skips Library entirely and starts cleanly.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src").join("a.txt"), "hi").unwrap();
    let library = tmp.path().join("Library");
    std::fs::create_dir(&library).unwrap();
    std::fs::create_dir(library.join("Containers")).unwrap();
    std::fs::write(library.join("Containers").join("payload"), "x").unwrap();
    // Mode 000 makes any descent fail; if the watcher were to descend,
    // the WalkDir/stat would error during cache init.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&library, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let result = super::fs_watch::FsWatch::spawn_watcher(tmp.path());
    // Restore perms so tempdir cleanup works.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&library, std::fs::Permissions::from_mode(0o755));
    }
    assert!(
        result.is_ok(),
        "watcher must skip protected dirs and start cleanly"
    );
}

#[test]
fn fs_watcher_does_not_deliver_events_from_target_or_node_modules_subtrees() {
    // Regression for the M4 CPU storm: when the workspace contains
    // `target/` or `node_modules/` at the top level, the FSEvents
    // thread used to spend ~60% of its samples inside
    // notify_debouncer_full's FileIdMap memcmp loop processing cargo
    // and npm writes that we then discarded post-event. The fix is to
    // never subscribe to those subtrees in the first place (same
    // split-watch pattern already used for Library/.Trash, extended
    // to NOISE_DIR_NAMES). This test asserts the contract by writing
    // a burst of files into `target/` and verifying the debouncer
    // channel receives zero events for those paths, while writes
    // under a normal sibling like `src/` still come through.
    let tmp = tempfile::tempdir().unwrap();
    let src_raw = tmp.path().join("src");
    let target_raw = tmp.path().join("target");
    let node_modules_raw = tmp.path().join("node_modules");
    std::fs::create_dir(&src_raw).unwrap();
    std::fs::create_dir(&target_raw).unwrap();
    std::fs::create_dir(&node_modules_raw).unwrap();
    let src = src_raw.canonicalize().unwrap();
    let target = target_raw.canonicalize().unwrap();
    let node_modules = node_modules_raw.canonicalize().unwrap();

    let (_debouncer, rx) = super::fs_watch::FsWatch::spawn_watcher(tmp.path())
        .expect("watcher must start cleanly with noise dirs present");

    for _ in 0..30 {
        while rx.try_recv().is_ok() {}
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    for i in 0..50 {
        std::fs::write(target.join(format!("cargo_{i}.o")), b"x").unwrap();
        std::fs::write(node_modules.join(format!("pkg_{i}.js")), b"x").unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mut noise_events: Vec<std::path::PathBuf> = Vec::new();
    while let Ok(batch) = rx.try_recv() {
        for ev in batch.unwrap_or_default() {
            for path in &ev.event.paths {
                if path.starts_with(&target) || path.starts_with(&node_modules) {
                    noise_events.push(path.clone());
                }
            }
        }
    }
    assert!(
        noise_events.is_empty(),
        "writes under target/ and node_modules/ must not produce debouncer events; got {noise_events:?}"
    );

    std::fs::write(src.join("a.rs"), b"fn main() {}").unwrap();
    let started = std::time::Instant::now();
    let mut signal_events: Vec<std::path::PathBuf> = Vec::new();
    while started.elapsed() < std::time::Duration::from_millis(1500) {
        while let Ok(batch) = rx.try_recv() {
            for ev in batch.unwrap_or_default() {
                for path in &ev.event.paths {
                    if path.starts_with(&src) {
                        signal_events.push(path.clone());
                    }
                }
            }
        }
        if !signal_events.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        !signal_events.is_empty(),
        "writes under src/ must still produce debouncer events after the noise-dir split"
    );
}

#[test]
fn fs_watcher_prunes_a_go_module_cache_below_the_workspace_root() {
    // The whole reason the watcher's skip predicate takes a &Path rather
    // than a bare file name: `go` cannot be blacklisted by name, so
    // go/pkg/mod is matched as a component run, and that needs the path.
    // Without it the module cache is watched like any other subdir.
    let tmp = tempfile::tempdir().unwrap();
    let cache_raw = tmp.path().join("go/pkg/mod/golang.org/x");
    let src_raw = tmp.path().join("go/cmd");
    std::fs::create_dir_all(&cache_raw).unwrap();
    std::fs::create_dir_all(&src_raw).unwrap();
    let cache = cache_raw.canonicalize().unwrap();
    let src = src_raw.canonicalize().unwrap();

    let (_debouncer, rx) = super::fs_watch::FsWatch::spawn_watcher(tmp.path())
        .expect("watcher must start cleanly with a go module cache present");

    for _ in 0..30 {
        while rx.try_recv().is_ok() {}
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    for i in 0..50 {
        std::fs::write(cache.join(format!("mod_{i}.go")), b"x").unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mut noise_events: Vec<std::path::PathBuf> = Vec::new();
    while let Ok(batch) = rx.try_recv() {
        for ev in batch.unwrap_or_default() {
            for path in &ev.event.paths {
                if path.starts_with(&cache) {
                    noise_events.push(path.clone());
                }
            }
        }
    }
    assert!(
        noise_events.is_empty(),
        "writes under go/pkg/mod must not produce debouncer events; got {noise_events:?}"
    );

    // The sibling go/ source tree is NOT noise and must still deliver.
    std::fs::write(src.join("main.go"), b"package main").unwrap();
    let started = std::time::Instant::now();
    let mut signal_events: Vec<std::path::PathBuf> = Vec::new();
    while started.elapsed() < std::time::Duration::from_millis(1500) {
        while let Ok(batch) = rx.try_recv() {
            for ev in batch.unwrap_or_default() {
                for path in &ev.event.paths {
                    if path.starts_with(&src) {
                        signal_events.push(path.clone());
                    }
                }
            }
        }
        if !signal_events.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        !signal_events.is_empty(),
        "writes under a real go/cmd source dir must still produce events"
    );
}

#[test]
fn fs_watcher_still_watches_a_workspace_whose_own_path_is_noise_named() {
    // The noise rules describe what is INSIDE a workspace. The root's own
    // ancestors are not part of it, so a checkout that happens to live
    // under a `build/` (or `go/pkg/mod/`) ancestor must be watched
    // normally. Matching the absolute path pruned every child and left the
    // workspace with no watches at all.
    let tmp = tempfile::tempdir().unwrap();
    let root_raw = tmp.path().join("build/myapp");
    let src_raw = root_raw.join("src");
    std::fs::create_dir_all(&src_raw).unwrap();
    let root = root_raw.canonicalize().unwrap();
    let src = src_raw.canonicalize().unwrap();

    let (_debouncer, rx) = super::fs_watch::FsWatch::spawn_watcher(&root)
        .expect("watcher must start for a root under a noise-named ancestor");

    for _ in 0..30 {
        while rx.try_recv().is_ok() {}
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    std::fs::write(src.join("a.rs"), b"fn main() {}").unwrap();
    let started = std::time::Instant::now();
    let mut signal_events: Vec<std::path::PathBuf> = Vec::new();
    while started.elapsed() < std::time::Duration::from_millis(1500) {
        while let Ok(batch) = rx.try_recv() {
            for ev in batch.unwrap_or_default() {
                for path in &ev.event.paths {
                    if path.starts_with(&src) {
                        signal_events.push(path.clone());
                    }
                }
            }
        }
        if !signal_events.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        !signal_events.is_empty(),
        "a workspace under a build/ ancestor must still deliver its own events"
    );
}

/// #539 review: the WATCHER stopped rooting streams at or above a
/// noise-named ancestor, but `drain` still classified EVENT paths whole. A
/// workspace living under `build/` therefore had every one of its own events
/// read as noise: `changed_files` never filled, so the agent-lane ledger saw
/// nothing, and `finder_relevant` never went true, so the Cmd+P index never
/// rebuilt. That is the same freeze the branch fixes, reached from the other
/// side.
#[test]
fn drain_reports_writes_in_a_workspace_under_a_noise_named_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    let src_raw = tmp.path().join("build/myapp/src");
    std::fs::create_dir_all(&src_raw).unwrap();
    let root = tmp
        .path()
        .join("build/myapp")
        .canonicalize()
        .expect("the workspace root exists");
    let src = src_raw.canonicalize().unwrap();

    let mut app = App::new(root.clone()).unwrap();
    // Let the watcher thread hand its debouncer over, then swallow whatever
    // the install itself produced, so the only events left are the write's.
    for _ in 1..=FS_SYNC_TICKS {
        app.try_install_pending_init();
        let _ = app.fs_watch.drain(&mut app.tree, &app.editor);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let signal = src.join("a.rs");
    std::fs::write(&signal, b"fn main() {}").unwrap();

    let mut changed: std::collections::BTreeSet<std::path::PathBuf> =
        std::collections::BTreeSet::new();
    let mut finder_relevant = false;
    for _ in 1..=FS_SYNC_TICKS {
        let d = app.fs_watch.drain(&mut app.tree, &app.editor);
        changed.extend(d.changed_files);
        finder_relevant |= d.finder_relevant;
        if changed.contains(&signal) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        changed.contains(&signal),
        "a write inside a workspace under a build/ ancestor is the workspace's \
         own content, not noise; got {changed:?}"
    );
    assert!(
        finder_relevant,
        "and it is worth a finder index rebuild, since the finder indexes it"
    );
}

/// A directory made after the watcher started is watched too: on Linux
/// every directory needs its own watch, installed once at startup, so a
/// write inside a new folder used to go unreported.
#[test]
fn writes_inside_a_directory_made_after_startup_are_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let mut app = App::new(root.clone()).unwrap();
    for _ in 1..=FS_SYNC_TICKS {
        app.try_install_pending_init();
        let _ = app.fs_watch.drain(&mut app.tree, &app.editor);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let dir = root.join("newdir");
    std::fs::create_dir(&dir).unwrap();
    let early = dir.join("early.rs");
    std::fs::write(&early, b"1").unwrap();
    let mut changed: std::collections::BTreeSet<std::path::PathBuf> =
        std::collections::BTreeSet::new();
    for _ in 1..=FS_SYNC_TICKS {
        changed.extend(app.fs_watch.drain(&mut app.tree, &app.editor).changed_files);
        if changed.contains(&early) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(changed.contains(&early), "{changed:?}");
    let late = dir.join("late.rs");
    std::fs::write(&late, b"2").unwrap();
    for _ in 1..=FS_SYNC_TICKS {
        changed.extend(app.fs_watch.drain(&mut app.tree, &app.editor).changed_files);
        if changed.contains(&late) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        changed.contains(&late),
        "the new folder is watched: {changed:?}"
    );
}

#[test]
fn fs_watcher_prunes_noise_dirs_nested_below_the_workspace_root() {
    // Regression for the freeze when the workspace root is a *parent of
    // repos* (e.g. ~/Documents): there the noise dirs (node_modules/, .git/,
    // target/) are NOT direct children of the root, so the old depth-1
    // pruning missed them and installed one recursive FSEvents watch over
    // the whole tree, re-subscribing every repo's node_modules write storm
    // (confirmed by `sample`: five fsevents loops + a mutex-bound debouncer
    // at ~45% CPU). Pruning must therefore be depth-aware: a noise dir
    // nested under a normal subdir must still never deliver events.
    let tmp = tempfile::tempdir().unwrap();
    let repo_raw = tmp.path().join("repo");
    let src_raw = repo_raw.join("src");
    let node_modules_raw = repo_raw.join("node_modules");
    let git_raw = repo_raw.join(".git");
    std::fs::create_dir_all(&src_raw).unwrap();
    std::fs::create_dir_all(&node_modules_raw).unwrap();
    std::fs::create_dir_all(&git_raw).unwrap();
    let src = src_raw.canonicalize().unwrap();
    let node_modules = node_modules_raw.canonicalize().unwrap();
    let git = git_raw.canonicalize().unwrap();

    let (_debouncer, rx) = super::fs_watch::FsWatch::spawn_watcher(tmp.path())
        .expect("watcher must start cleanly with nested noise dirs present");

    for _ in 0..30 {
        while rx.try_recv().is_ok() {}
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    for i in 0..50 {
        std::fs::write(node_modules.join(format!("pkg_{i}.js")), b"x").unwrap();
        std::fs::write(git.join(format!("obj_{i}")), b"x").unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mut noise_events: Vec<std::path::PathBuf> = Vec::new();
    while let Ok(batch) = rx.try_recv() {
        for ev in batch.unwrap_or_default() {
            for path in &ev.event.paths {
                if path.starts_with(&node_modules) || path.starts_with(&git) {
                    noise_events.push(path.clone());
                }
            }
        }
    }
    assert!(
        noise_events.is_empty(),
        "writes under a repo's nested node_modules/ and .git/ must not produce \
         debouncer events even when the repo is itself a subdir of the root; got {noise_events:?}"
    );

    std::fs::write(src.join("a.rs"), b"fn main() {}").unwrap();
    let started = std::time::Instant::now();
    let mut signal_events: Vec<std::path::PathBuf> = Vec::new();
    while started.elapsed() < std::time::Duration::from_millis(1500) {
        while let Ok(batch) = rx.try_recv() {
            for ev in batch.unwrap_or_default() {
                for path in &ev.event.paths {
                    if path.starts_with(&src) {
                        signal_events.push(path.clone());
                    }
                }
            }
        }
        if !signal_events.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        !signal_events.is_empty(),
        "writes under a nested repo/src/ must still produce events after the depth-aware split"
    );
}

// Regression for the two-cores-pegged fan storm: on macOS a "non-recursive"
// FSEvents watch is still a fully recursive stream rooted at the path, so any
// watch target that is an ancestor of a noise dir (target/, node_modules/,
// .git/) re-streams that dir's entire build churn into the fsevents callback
// even though notify discards it before the debouncer. The earlier
// boundary-watch fix left exactly such ancestor watches (on the workspace root
// and on each repo dir) and its tests only checked debouncer events, never the
// callback cost — so the burn shipped. The invariant the watcher MUST hold:
// no installed watch target is an ancestor of any noise dir.
// A parent-of-repos root (e.g. ~/Documents with ~40 repos) holds 100k+
// non-noise dirs. Walking all of them to prove they're noise-free costs
// seconds on the watcher thread and yields thousands of FSEvents watches. The
// walk MUST stop once it has spent its dir-visit budget and let the adaptive
// poll cover the rest, rather than scan the whole tree.
#[cfg(target_os = "macos")]
#[test]
fn macos_watch_walk_stops_when_the_dir_budget_is_spent() {
    let tmp = tempfile::tempdir().unwrap();
    // A dirty root (has .git) so its clean children are collected individually
    // instead of the whole root being watched as one clean subtree.
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    for i in 0..300 {
        std::fs::create_dir_all(tmp.path().join(format!("d{i}/sub"))).unwrap();
    }

    let mut targets: Vec<(std::path::PathBuf, notify::RecursiveMode)> = Vec::new();
    let mut budget = 50usize; // far fewer than the ~600 dirs present
    super::fs_watch::collect_macos_watch_targets(tmp.path(), tmp.path(), &mut targets, &mut budget);

    assert_eq!(
        budget, 0,
        "the walk must spend its whole budget and stop, not visit every dir"
    );
    assert!(
        targets.len() < 300,
        "once the budget caps, not every clean subtree is watched (the poll covers the rest); got {}",
        targets.len()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_watch_targets_never_root_a_stream_above_a_noise_dir() {
    use crate::widgets::file_finder::is_noise_dir;
    // A parent-of-repos layout (~/Documents-shaped): root has no top-level
    // noise, but each repo nested under it carries target/, node_modules/, .git.
    let tmp = tempfile::tempdir().unwrap();
    for repo in ["alpha", "beta"] {
        let r = tmp.path().join(repo);
        std::fs::create_dir_all(r.join("src/inner")).unwrap();
        std::fs::create_dir_all(r.join("target/debug/incremental")).unwrap();
        std::fs::create_dir_all(r.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(r.join(".git/objects")).unwrap();
    }

    let mut targets: Vec<(std::path::PathBuf, notify::RecursiveMode)> = Vec::new();
    let mut budget = usize::MAX; // generous: this small tree is walked in full
    super::fs_watch::collect_macos_watch_targets(tmp.path(), tmp.path(), &mut targets, &mut budget);

    // No target may contain a noise dir anywhere beneath it (which on macOS
    // would put that noise dir inside the target's recursive FSEvents stream).
    for (path, _mode) in &targets {
        for entry in walkdir_noise(path) {
            if let Some(name) = entry.file_name() {
                assert!(
                    !is_noise_dir(name),
                    "watch target {path:?} roots an FSEvents stream over noise dir {entry:?}; \
                     cargo/npm build churn there would peg the fsevents callback"
                );
            }
        }
    }
    // And the clean leaves must still be covered, or external edits go unseen.
    assert!(
        targets.iter().any(|(p, _)| p.ends_with("alpha/src")
            || p.canonicalize()
                .map(|c| c.ends_with("alpha/src"))
                .unwrap_or(false)),
        "the clean repo subtree (alpha/src) must still be watched; got {targets:?}"
    );
}

#[cfg(target_os = "macos")]
fn walkdir_noise(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.filter_map(Result::ok) {
            let p = entry.path();
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                out.push(p.clone());
                stack.push(p);
            }
        }
    }
    out
}

#[test]
fn drain_fs_events_returns_false_when_nothing_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.fs_watch.disable();
    for _ in 0..20 {
        let _ = app.drain_fs_events();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!app.drain_fs_events(), "no fs events ⇒ no redraw needed");
}

/// How many drain ticks an external filesystem change may take to reach the
/// Explorer: the 200 ms fs-sync invariant, expressed in croft's own work
/// rather than in wall clock (#483). Each tick is one `drain_fs_events` and
/// a 10 ms pause, so twenty ticks is 200 ms of croft's own opportunities to
/// notice the change; a suite that starves this thread for 300 ms between
/// two ticks stretches the clock but not the count. On a quiet machine the
/// change lands in one or two ticks, and the poll fallback alone would land
/// it in about five at `FS_POLL_INTERVAL`'s floor (more once `back_off`
/// widens the interval), so this bound catches a gross regression (a
/// watcher and a poll that both stopped delivering), not drift; the
/// wall-clock figure itself is measured by the `#[ignore]`d serial test
/// below.
const FS_SYNC_TICKS: usize = 20;

#[test]
fn drain_fs_events_returns_true_after_workspace_write() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "*.txt\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for _ in 0..20 {
        let _ = app.drain_fs_events();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let new_file = tmp.path().join("new.txt");
    std::fs::write(&new_file, "hi").unwrap();
    let mut saw = false;
    let mut landed = false;
    // Bounded at the invariant itself: every tick past it would give the
    // same verdict, so the loop stops where the claim does.
    for _ in 1..=FS_SYNC_TICKS {
        if app.drain_fs_events() {
            saw = true;
        }
        if app.tree.nodes.iter().any(|n| n.path == new_file) {
            landed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(saw, "workspace write should propagate as a dirty signal");
    assert!(
        landed,
        "created file should appear in Explorer within {FS_SYNC_TICKS} drain ticks (never landed)"
    );
}

#[test]
fn drain_fs_events_removes_deleted_root_file_from_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let doomed = tmp.path().join("doomed.txt");
    std::fs::write(&doomed, "bye").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(
        app.tree.nodes.iter().any(|n| n.path == doomed),
        "precondition: initial tree contains the file"
    );

    std::fs::remove_file(&doomed).unwrap();
    let mut gone = false;
    for _ in 1..=FS_SYNC_TICKS {
        let _ = app.drain_fs_events();
        if !app.tree.nodes.iter().any(|n| n.path == doomed) {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        gone,
        "deleted file should disappear from Explorer within {FS_SYNC_TICKS} drain ticks (never left)"
    );
}

/// The 200 ms invariant as wall clock, for a serial run on a quiet machine
/// (pass `--ignored` with the filter `fs_sync_reflects`). Ignored by default
/// because a full parallel suite is itself the load (#483): the scheduler,
/// not croft, decides whether this thread runs again inside 200 ms while
/// thousands of neighbours spawn shells, so a failure here in that setting
/// says nothing about the watcher. The tick-counted pair above carries the
/// check in the suite; this one measures the invariant itself.
#[test]
#[ignore = "wall clock: run serially on a quiet machine"]
fn fs_sync_reflects_external_changes_within_200ms_wall_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let doomed = tmp.path().join("doomed.txt");
    std::fs::write(&doomed, "bye").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for _ in 0..20 {
        let _ = app.drain_fs_events();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let budget = std::time::Duration::from_millis(200);

    let new_file = tmp.path().join("new.txt");
    std::fs::write(&new_file, "hi").unwrap();
    // The budget is checked after every drain as well as before: a drain
    // that lands the change past the deadline must still fail, not end the
    // loop quietly on the next condition check.
    let started = std::time::Instant::now();
    while !app.tree.nodes.iter().any(|n| n.path == new_file) {
        let _ = app.drain_fs_events();
        assert!(
            started.elapsed() <= budget,
            "created file should appear in Explorer within {budget:?} (deadline passed mid-drain)"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        started.elapsed() <= budget,
        "created file should appear in Explorer within {budget:?} (deadline passed at the last check)"
    );

    std::fs::remove_file(&doomed).unwrap();
    let started = std::time::Instant::now();
    while app.tree.nodes.iter().any(|n| n.path == doomed) {
        let _ = app.drain_fs_events();
        assert!(
            started.elapsed() <= budget,
            "deleted file should disappear from Explorer within {budget:?} (deadline passed mid-drain)"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        started.elapsed() <= budget,
        "deleted file should disappear from Explorer within {budget:?} (deadline passed at the last check)"
    );
}

#[test]
fn drain_fs_events_polling_fallback_refreshes_tree_without_watcher_event() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.fs_watch.disable();
    app.fs_watch.clear_dir_mtimes();
    app.fs_watch.force_poll_due();

    let new_file = tmp.path().join("new.txt");
    std::fs::write(&new_file, "hi").unwrap();

    assert!(app.drain_fs_events());
    assert!(
        app.tree.nodes.iter().any(|n| n.path == new_file),
        "polling fallback should refresh the tree when no watcher event arrives"
    );
}

#[test]
fn drain_fs_events_polling_fallback_reloads_clean_open_file_without_watcher_event() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("open.txt");
    std::fs::write(&file, "old\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.sync_open_file_poll_mtime();
    app.fs_watch.disable();
    app.fs_watch.force_poll_due();

    std::fs::write(&file, "new content\n").unwrap();

    assert!(app.drain_fs_events());
    assert_eq!(app.editor.lines, vec!["new content"]);
    assert!(!app.editor.dirty);
}

/// "Reopen with Encoding" re-reads the file from disk over the buffer AND
/// clears both history stacks, so unsaved work was gone with no confirmation
/// and no way back through Cmd+Z. The status-bar encoding segment is a newly
/// clickable target sitting a few cells from the Ln/Col readout, so this is
/// easy to hit by accident — including by picking the encoding already in use,
/// which otherwise looks like a no-op.
#[test]
fn reopen_with_encoding_refuses_to_discard_unsaved_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("notes.txt");
    std::fs::write(&f, "keep me\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.editor.cursor_col = 0;
    app.editor.insert_char('X');
    assert!(app.editor.dirty, "the buffer has unsaved work");

    app.dispatch_menu_action(
        MenuAction::SetEncoding(encoding_rs::UTF_8),
        tmp.path().to_path_buf(),
    );

    assert!(
        app.editor.lines[0].starts_with('X'),
        "the unsaved edit must survive, got {:?}",
        app.editor.lines[0]
    );
    assert!(app.editor.dirty, "and the buffer is still dirty");
    let msg = app.status.to_lowercase();
    assert!(
        msg.contains("undo"),
        "the refusal must point at the non-destructive route, got {:?}",
        app.status
    );
    // Never "save first": the buffer may be showing mojibake from a wrong
    // decode, and saving writes those replacement chars over the real bytes.
    assert!(
        !msg.contains("save the file first") && !msg.contains("save first"),
        "must not advise the destructive route, got {:?}",
        app.status
    );
}

/// The external-change sweep has to reach every editor GROUP, not just the
/// focused one. The poll stats each open tab exactly once and re-baselines its
/// stamp, so a change it reports and the sweep then misses is burned: no later
/// tick sees it again and the split pane keeps stale text forever.
#[test]
fn an_external_write_reloads_a_tab_in_an_inactive_split_group() {
    let tmp = tempfile::tempdir().unwrap();
    let side = tmp.path().join("side.txt");
    let front = tmp.path().join("front.txt");
    std::fs::write(&side, "old\n").unwrap();
    std::fs::write(&front, "front\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&side).unwrap();
    app.split_editor(); // side.txt is now open in BOTH groups; right is focused
    app.editor.open_pinned(&front).unwrap(); // focused group moves to front.txt
    app.sync_open_file_poll_mtime();
    app.fs_watch.disable();
    app.fs_watch.force_poll_due();
    let _ = app.drain_fs_events();

    std::fs::write(&side, "new content\n").unwrap();
    app.fs_watch.force_poll_due();
    let _ = app.drain_fs_events();

    let stale: Vec<String> = app
        .editor_layout
        .inactive_groups()
        .into_iter()
        .flat_map(|g| g.editors.iter())
        .filter(|e| e.path.as_deref() == Some(side.as_path()))
        .map(|e| e.lines.join("\n"))
        .collect();
    assert!(
        !stale.is_empty(),
        "the inactive group still holds a tab on side.txt"
    );
    for text in stale {
        assert_eq!(
            text, "new content",
            "a tab in an inactive split group must reload too"
        );
    }
}

/// Watcher backends on Linux fire `Access(...)` events when croft (or any
/// other process) reads the open file — the inotify subsystem does not
/// distinguish a benign read from a real content change. Treating those
/// as "external change" reloads the editor, which clears `selection` and
/// kills any in-flight Cmd+A / mouse-drag / Shift+Right gesture. The
/// reload must only fire for events that mutate content.
#[test]
fn drain_fs_events_preserves_editor_selection_on_access_only_event() {
    use notify::Event as NotifyEvent;
    use notify::event::{AccessKind, AccessMode, EventKind};
    use notify_debouncer_full::DebouncedEvent;
    use std::sync::mpsc;

    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("readme.md");
    std::fs::write(&file, "hello world\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.editor.select_all();
    assert!(
        app.editor.selection.is_some(),
        "precondition: selection set after select_all"
    );

    let (tx, rx) = mpsc::channel();
    app.fs_watch.set_test_events(rx);
    app.sync_open_file_poll_mtime();

    let access_event = NotifyEvent::new(EventKind::Access(AccessKind::Open(AccessMode::Read)))
        .add_path(file.clone());
    tx.send(Ok(vec![DebouncedEvent::new(
        access_event,
        std::time::Instant::now(),
    )]))
    .unwrap();

    app.drain_fs_events();
    assert!(
        app.editor.selection.is_some(),
        "Access(Read) on the open file must not clobber editor selection"
    );
}

/// Same class of spurious wake-up as the Access event, via the other
/// notify branch: `chmod`, `touch -a`, ownership changes, and xattr
/// updates surface as `Modify(Metadata(_))`. None mutate file content,
/// so none should trigger an editor reload that wipes selection.
#[test]
fn drain_fs_events_preserves_editor_selection_on_metadata_event() {
    use notify::Event as NotifyEvent;
    use notify::event::{EventKind, MetadataKind, ModifyKind};
    use notify_debouncer_full::DebouncedEvent;
    use std::sync::mpsc;

    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("readme.md");
    std::fs::write(&file, "hello world\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.editor.select_all();

    let (tx, rx) = mpsc::channel();
    app.fs_watch.set_test_events(rx);
    app.sync_open_file_poll_mtime();

    let metadata_event = NotifyEvent::new(EventKind::Modify(ModifyKind::Metadata(
        MetadataKind::AccessTime,
    )))
    .add_path(file.clone());
    tx.send(Ok(vec![DebouncedEvent::new(
        metadata_event,
        std::time::Instant::now(),
    )]))
    .unwrap();

    app.drain_fs_events();
    assert!(
        app.editor.selection.is_some(),
        "Modify(Metadata(AccessTime)) on the open file must not clobber editor selection"
    );
}

/// Companion to the access-event test: a real content change still has to
/// trigger the reload, otherwise external edits would never refresh the
/// buffer. Guards against an over-broad fix that drops legitimate events.
#[test]
fn drain_fs_events_reloads_on_modify_data_event() {
    use notify::Event as NotifyEvent;
    use notify::event::{DataChange, EventKind, ModifyKind};
    use notify_debouncer_full::DebouncedEvent;
    use std::sync::mpsc;

    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("readme.md");
    std::fs::write(&file, "old\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();

    let (tx, rx) = mpsc::channel();
    app.fs_watch.set_test_events(rx);
    app.sync_open_file_poll_mtime();

    std::fs::write(&file, "new content\n").unwrap();
    let modify_event = NotifyEvent::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
        .add_path(file.clone());
    tx.send(Ok(vec![DebouncedEvent::new(
        modify_event,
        std::time::Instant::now(),
    )]))
    .unwrap();

    app.drain_fs_events();
    assert_eq!(app.editor.lines, vec!["new content"]);
}

/// The reported bug: a file open in a NON-active tab changed on disk and the
/// tab never reloaded, so it looked like another project's content had leaked
/// in. FS sync must cover every open tab, not just the focused one.
#[test]
fn drain_fs_events_reloads_background_tab_on_external_change() {
    use notify::Event as NotifyEvent;
    use notify::event::{DataChange, EventKind, ModifyKind};
    use notify_debouncer_full::DebouncedEvent;
    use std::sync::mpsc;

    let tmp = tempfile::tempdir().unwrap();
    let bg = tmp.path().join("background.txt");
    let fg = tmp.path().join("foreground.txt");
    std::fs::write(&bg, "bg old\n").unwrap();
    std::fs::write(&fg, "fg old\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&bg).unwrap(); // background tab
    app.editor.open_pinned(&fg).unwrap(); // active tab

    let (tx, rx) = mpsc::channel();
    app.fs_watch.set_test_events(rx);
    app.sync_open_file_poll_mtime();

    std::fs::write(&bg, "bg NEW\n").unwrap();
    let ev = NotifyEvent::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
        .add_path(bg.clone());
    tx.send(Ok(vec![DebouncedEvent::new(ev, std::time::Instant::now())]))
        .unwrap();

    app.drain_fs_events();

    let bg_tab = app
        .editor
        .iter_tabs()
        .find(|e| e.path.as_deref() == Some(bg.as_path()))
        .unwrap();
    assert_eq!(
        bg_tab.lines,
        vec!["bg NEW"],
        "a background tab must reload when its file changes on disk"
    );
    assert_eq!(app.editor.lines, vec!["fg old"], "active tab untouched");
}

#[test]
fn ctrl_s_is_save_key() {
    for kitty in [false, true] {
        assert!(is_save_key(
            key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            kitty
        ));
    }
}

#[test]
fn cmd_s_is_save_key() {
    for kitty in [false, true] {
        assert!(is_save_key(
            key(KeyCode::Char('s'), KeyModifiers::SUPER),
            kitty
        ));
    }
}

#[test]
fn shift_ctrl_s_is_save_key() {
    // Without the kitty keyboard protocol a terminal may capitalise the
    // letter of a plain Ctrl+S, so Ctrl+Shift+S saves there.
    let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    assert!(is_save_key(key(KeyCode::Char('S'), mods), false));
    // An uppercase S with Ctrl but no Shift reported saves everywhere.
    for kitty in [false, true] {
        assert!(is_save_key(
            key(KeyCode::Char('S'), KeyModifiers::CONTROL),
            kitty
        ));
    }
}

/// #857 negative: the jump needs the Shift *reported*. Where the terminal
/// (or tmux 3.5+, or tmux with `extended-keys on`) sends no Shift flag,
/// Ctrl+S and a Ctrl+uppercase-S still save and the side bar stays put,
/// and without the kitty keyboard protocol even a reported Shift saves.
#[test]
fn ctrl_s_without_a_reported_shift_still_saves_and_stays_put() {
    let cases = [
        (true, KeyCode::Char('s'), KeyModifiers::CONTROL),
        (true, KeyCode::Char('S'), KeyModifiers::CONTROL),
        (
            false,
            KeyCode::Char('s'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
    ];
    for (kitty, code, mods) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("notes.txt");
        std::fs::write(&path, "on disk").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.kitty_keys = kitty;
        app.editor.open_pinned(&path).unwrap();
        app.focus_pane(Pane::Editor);
        let before = app.sidebar_view;
        app.editor.insert_str("edited ");
        app.handle_key(key(code, mods)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim_end(),
            "edited on disk",
            "{code:?} {mods:?} kitty={kitty} saves"
        );
        assert_eq!(app.sidebar_view, before, "{code:?} {mods:?}: no jump");
        assert_ne!(app.sidebar_view, SidebarView::SourceControl);
    }
}

#[test]
fn plain_s_is_not_save_key() {
    assert!(!is_save_key(
        key(KeyCode::Char('s'), KeyModifiers::NONE),
        true
    ));
}

#[test]
fn ctrl_q_is_not_save_key() {
    assert!(!is_save_key(
        key(KeyCode::Char('q'), KeyModifiers::CONTROL),
        true
    ));
}

#[test]
fn alt_s_is_not_save_key() {
    assert!(!is_save_key(
        key(KeyCode::Char('s'), KeyModifiers::ALT),
        true
    ));
}

#[test]
fn open_create_prompt_populates_state() {
    // We can't easily build a full App in a test (PtyTerminal spawns a real
    // shell), so we exercise the Prompt struct directly via the public
    // helpers it relies on.  This guards the data the prompt carries.
    use crate::widgets::file_tree::{create_file_in, create_folder_in, validate_new_name};
    use tempfile::TempDir;
    let tmp = TempDir::new().unwrap();
    // commit_prompt path: validate → create → returns path on success.
    assert!(validate_new_name("hello.rs").is_ok());
    let p = create_file_in(tmp.path(), "hello.rs").unwrap();
    assert!(p.is_file());
    let d = create_folder_in(tmp.path(), "newdir").unwrap();
    assert!(d.is_dir());
}

#[test]
fn cmd_shift_slash_on_a_file_changes_workspace_root_to_the_file_s_parent_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let inner = tmp.path().join("inner");
    std::fs::create_dir(&inner).unwrap();
    let file = inner.join("note.txt");
    std::fs::write(&file, "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let target_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path == file)
        .or_else(|| {
            let i = app.tree.nodes.iter().position(|n| n.path == inner).unwrap();
            app.tree.selected = i;
            app.tree.expand_selected();
            app.tree.nodes.iter().position(|n| n.path == file)
        })
        .expect("file node must be visible after expanding inner/");
    app.tree.selected = target_idx;
    app.focus = Pane::Tree;
    app.sidebar_view = SidebarView::Explorer;

    let handled = app.handle_explorer_shortcut(key(
        KeyCode::Char('/'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ));

    assert!(
        handled,
        "Cmd+Shift+/ must be handled by the Explorer dispatcher"
    );
    assert_eq!(app.workspace_root(), inner);
    assert_eq!(app.tree.root, inner);
}

/// #862: an untitled buffer has no file to save to, so S cannot keep its
/// text and does not quit; D is the explicit way out.
#[test]
fn save_all_does_not_quit_while_an_untitled_buffer_holds_text() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    type_into_editor(&mut app, "note");
    assert!(app.editor.path.is_none() && app.editor.dirty, "setup");
    press_ctrl_q(&mut app);
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(!app.quit, "the untitled text would be lost");
    assert!(app.status.contains("untitled"), "{}", app.status);
    assert_eq!(app.editor.lines, vec![String::from("note")]);
    press_ctrl_q(&mut app);
    app.handle_key(key(KeyCode::Char('D'), KeyModifiers::SHIFT))
        .unwrap();
    assert!(app.quit);
}

/// #862: a save that fails keeps croft open. Here the file changed on disk
/// under the edits: Save All must neither overwrite that nor quit.
#[test]
fn save_all_does_not_quit_when_a_save_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    std::fs::write(&a, "changed elsewhere\n").unwrap();
    press_ctrl_q(&mut app);
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert!(!app.quit);
    assert!(
        app.status.contains("a.txt (changed on disk)"),
        "{}",
        app.status
    );
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "changed elsewhere\n",
        "the other writer's change is not overwritten"
    );
    assert!(app.editor.dirty, "the edits are still in the buffer");
}

/// #862 review: a file open in two editor groups is two buffers. With the
/// same unsaved text in both, Save All writes it once and quits: before,
/// the second write read the first as a change on disk, refused, and kept
/// croft open over a file that already held the edits.
#[test]
fn save_all_writes_a_file_open_in_two_groups_with_the_same_edits_once_and_quits() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    let edited = app.editor.lines.clone();
    app.split_editor();
    app.editor.lines = edited.clone();
    app.editor.dirty = true;
    press_ctrl_q(&mut app);
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.quit, "{}", app.status);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "xalpha\n");
}

/// #862 review, the guard: two groups holding different unsaved text for
/// one file are not settled by writing whichever comes first. Neither is
/// written, both stay, and croft stays open naming the file.
#[test]
fn save_all_writes_neither_of_two_different_unsaved_copies_of_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    app.split_editor();
    app.editor.lines = vec![String::from("yalpha"), String::new()];
    app.editor.dirty = true;
    press_ctrl_q(&mut app);
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert!(!app.quit);
    assert!(
        app.status
            .contains("a.txt (open in 2 editor groups with different unsaved text)"),
        "{}",
        app.status
    );
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha\n");
    let mut texts: Vec<String> = std::iter::once(&app.editor)
        .chain(app.editor_layout.inactive_groups())
        .flat_map(|g| g.editors.iter())
        .filter(|e| e.path.as_deref() == Some(a.as_path()))
        .inspect(|e| assert!(e.dirty, "both copies stay unsaved"))
        .map(|e| e.lines[0].clone())
        .collect();
    texts.sort();
    assert_eq!(texts, ["xalpha", "yalpha"]);
}

/// #862: S at the close prompt writes the tab, then closes it.
#[test]
fn s_at_the_close_prompt_saves_then_closes() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "xalpha\n");
    assert_eq!(app.editor.path, None, "the tab closed");
    assert_eq!(app.status, "Saved and closed tab");
    assert!(!app.quit, "closing a tab never quits");
}

/// #862: D at the close prompt closes without writing, and the tab can
/// still be reopened from disk.
#[test]
fn d_at_the_close_prompt_closes_without_saving() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    app.run_command(crate::widgets::command_palette::Command::CloseEditor);
    assert!(app.pending_unsaved.is_some(), "View: Close Editor asks too");
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.path, None, "the tab closed");
    assert_eq!(app.status, "Closed tab without saving");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha\n");
    app.reopen_closed_tab();
    assert_eq!(app.editor.path.as_deref(), Some(a.as_path()));
    assert_eq!(app.editor.lines[0], "alpha", "reopened from disk");
}

/// #1438: Save All & Commit writes the tab, then commits the editor text.
#[test]
fn save_all_and_commit_commits_the_text_in_the_tab() {
    let (tmp, mut app) = scm_with_unsaved_tab();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    wait_for_git_net(&mut app);
    assert!(app.pending_unsaved_scm.is_none());
    assert!(!app.editor.dirty, "the tab was saved");
    assert_eq!(
        git_out(tmp.path(), &["show", "HEAD:seed.txt"]),
        "edited changed\n"
    );
    assert_eq!(last_commit_message(tmp.path()), "bump seed\n\n");
}

/// #1438: Stash asks too, and Save All & Stash stashes the editor text,
/// leaving the tab saved.
#[test]
fn save_all_and_stash_stashes_the_text_in_the_tab() {
    let (tmp, mut app) = scm_with_unsaved_tab();
    app.stash_source_control();
    let pending = app.pending_unsaved_scm.as_ref().expect("the prompt opens");
    assert_eq!(pending.op, ScmWrite::Stash);
    assert_eq!(
        git_out(tmp.path(), &["stash", "list"]),
        "",
        "nothing stashed yet"
    );
    let rendered = screen_text_863(&mut app, 120, 40);
    assert!(rendered.contains("Save All & Stash"), "{rendered}");
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.pending_unsaved_scm.is_none());
    assert!(!app.editor.dirty, "the tab was saved");
    // #1581: the stash asks for its name next, seeded with the message box.
    app.submit_input_prompt();
    assert_eq!(
        git_out(tmp.path(), &["stash", "list", "--format=%gs"]),
        "On main: bump seed\n"
    );
    assert_eq!(
        git_out(tmp.path(), &["show", "stash@{0}:seed.txt"]),
        "edited changed\n"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "seed\n",
        "the stash put the work aside"
    );
}

/// Local snapshots are path-keyed and repo-free, so a file outside the
/// workspace root must still list (and restore) its versions; the panel used
/// to sit on "Loading" forever while snapshots accumulated invisibly.
#[test]
fn a_file_outside_the_workspace_still_lists_its_local_snapshots() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = outside.path().join("note.txt");
    std::fs::write(&f, "v2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    crate::history::record_in(&app.history_root, &f, b"v1\n", 1000).unwrap();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let target = loop {
        app.sync_explorer_panels();
        if let Some(t) = app.timeline.diff_target(0) {
            break Some(t);
        }
        if std::time::Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(
        target,
        Some((f.clone(), "local:1000".to_string())),
        "the out-of-root file's snapshot never reached the TIMELINE"
    );
}

/// A Windows-1252 buffer (via Reopen with Encoding) saves non-UTF-8 bytes, so
/// its history must be recorded byte-exact — the old text-only path silently
/// skipped every save of such a file.
#[test]
fn saving_a_windows_1252_file_records_a_byte_exact_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("latin.txt");
    // 0xE9 is 'é' in Windows-1252 and invalid UTF-8.
    std::fs::write(&f, b"caf\xE9\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.editor
        .reopen_with_encoding(encoding_rs::WINDOWS_1252)
        .unwrap();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('X'), KeyModifiers::NONE))
        .unwrap();
    app.save();
    let snaps = wait_for_snapshots(&app.history_root, &f);
    assert_eq!(
        snaps.len(),
        1,
        "a non-UTF-8 file's save must record history"
    );
    let disk = std::fs::read(&f).unwrap();
    assert!(disk.contains(&0xE9), "staging: the save re-encoded to 1252");
    assert_eq!(
        std::fs::read(&snaps[0].file).unwrap(),
        disk,
        "the snapshot must hold the exact on-disk bytes"
    );
}

#[test]
fn cmd_p_index_is_built_in_the_background_so_open_does_not_walk_synchronously() {
    // Regression for the user's "Cmd+P is so slow" report. The old
    // open_file_finder ran build_file_index synchronously on the
    // UI thread on first invocation; for a sizeable workspace
    // that stalled the TUI for the entire walk. The fix kicks
    // off the walk in App::new and delivers the index via
    // channel - by the time the user reaches for Cmd+P, the
    // index is already cached.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), b"").unwrap();
    std::fs::create_dir(tmp.path().join(".croft")).unwrap();
    std::fs::write(tmp.path().join(".croft").join("lsp.log"), b"").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Poll for the background-built index to land. With this tiny
    // workspace it should arrive within a couple of hundred ms.
    let mut waited_ms = 0u32;
    loop {
        app.poll_file_finder_index();
        if app.file_finder_index.is_some() {
            break;
        }
        if waited_ms >= 5000 {
            panic!(
                "background walker thread must deliver the file index within 5s of App::new on a 2-file workspace"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited_ms += 20;
    }
    // Once the index has landed, open_file_finder must consume it
    // without falling back to the synchronous walk - and the index
    // MUST contain the .croft/lsp.log entry the user expected to
    // find via Cmd+P typing 'lsp.log'.
    let rels: Vec<String> = app
        .file_finder_index
        .as_ref()
        .unwrap()
        .iter()
        .map(|e| e.rel.clone())
        .collect();
    assert!(
        rels.iter().any(|r| r == ".croft/lsp.log"),
        "background index must contain .croft/lsp.log; got {rels:?}"
    );
}

#[test]
fn change_workspace_root_rebuilds_cmd_p_index_against_the_new_root() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("root_a");
    let b = tmp.path().join("root_b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("only_in_a.rs"), b"").unwrap();
    std::fs::write(b.join("only_in_b.rs"), b"").unwrap();
    let mut app = App::new(a.clone()).unwrap();
    let mut waited = 0u32;
    loop {
        app.poll_file_finder_index();
        if let Some(idx) = app.file_finder_index.as_ref()
            && idx.iter().any(|e| e.rel == "only_in_a.rs")
        {
            break;
        }
        if waited >= 5000 {
            panic!("initial index against root_a must land within 5s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    app.change_workspace_root(b.clone());
    let mut saw_b = false;
    for _ in 0..400 {
        app.poll_file_finder_index();
        if let Some(idx) = app.file_finder_index.as_ref() {
            let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
            if rels.contains(&"only_in_b.rs") && !rels.contains(&"only_in_a.rs") {
                saw_b = true;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        saw_b,
        "after change_workspace_root(b), the Cmd+P index must reflect root_b's files (only_in_b.rs present, only_in_a.rs gone); without this, Cmd+/ leaves Cmd+P searching the previous project"
    );
}

/// A relative launch path opens the file under the workspace root by its
/// absolute path, whatever directory the process runs in (#1193).
#[test]
fn a_relative_launch_path_opens_the_absolute_file_under_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("pkg")).unwrap();
    std::fs::write(root.join("pkg/util.py"), "x = 1\n").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.open_file_at_launch(std::path::Path::new("pkg/util.py"));
    assert_eq!(
        app.editor.path.as_deref(),
        Some(root.join("pkg/util.py").as_path())
    );
    assert!(app.focus == Pane::Editor);
}

/// A launch file that can't be opened says so instead of leaving "Ready".
#[test]
fn a_launch_file_that_cannot_be_opened_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_at_launch(std::path::Path::new("pkg/missing.py"));
    assert!(app.editor.path.is_none());
    assert!(
        app.status.starts_with("Couldn't open ") && app.status.contains("pkg/missing.py"),
        "status: {:?}",
        app.status
    );
}

#[test]
fn open_file_at_launch_opens_the_file_and_focuses_the_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hi.rs");
    std::fs::write(&file, "fn main() {}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    app.open_file_at_launch(&file);

    assert_eq!(
        app.editor.path.as_deref(),
        Some(file.as_path()),
        "`croft --open-file <path>` opens that file on launch"
    );
    assert!(
        app.focus == Pane::Editor,
        "the editor takes focus on launch open"
    );
}

#[test]
fn status_path_speaks_workspace_relative_inside_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    let inside = tmp.path().join("src").join("main.rs");
    assert_eq!(
        app.status_path(&inside),
        format!("src{}main.rs", std::path::MAIN_SEPARATOR),
        "paths under the workspace root must render root-relative, like tabs \
         and breadcrumbs"
    );
    let outside = std::path::Path::new("/etc/hosts");
    assert_eq!(
        app.status_path(outside),
        "/etc/hosts",
        "paths outside the root keep their absolute form"
    );
}

/// Issue #213: files.autoSave onFocusChange. With the mode on, moving
/// focus off the editor writes the dirty buffer immediately, without
/// waiting for the afterDelay timer (which stays off here).
#[test]
fn auto_save_on_focus_change_writes_when_the_editor_loses_focus() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.auto_save = false;
    app.auto_save_on_focus_change = true;
    app.focus_pane(Pane::Editor);
    app.tick_auto_save(); // settle the focus signature
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.dirty);
    assert!(
        !app.tick_auto_save(),
        "no focus change yet, and the delay mode is off"
    );
    assert!(app.editor.dirty, "still dirty while focus has not moved");
    // Focus leaves the editor: the buffer must be written on the next tick.
    app.focus_pane(Pane::Terminal);
    assert!(app.tick_auto_save(), "losing focus saves the buffer");
    assert!(!app.editor.dirty, "saving clears the dirty flag");
    let on_disk = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert!(
        on_disk.contains('x'),
        "the edit must be on disk: {on_disk:?}"
    );
}

/// Switching tabs saves the tab that lost focus, and leaves the newly
/// focused (untouched) tab alone.
#[test]
fn auto_save_on_focus_change_writes_the_tab_that_lost_focus() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "aaa").unwrap();
    std::fs::write(&b, "bbb").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.auto_save = false;
    app.auto_save_on_focus_change = true;
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    app.tick_auto_save();
    app.handle_key(key(KeyCode::Char('Z'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.dirty, "a.txt is dirty");
    // Opening b.txt takes focus away from a.txt.
    app.editor.open_pinned(&b).unwrap();
    assert!(app.tick_auto_save(), "the tab that lost focus is saved");
    let on_disk = std::fs::read_to_string(&a).unwrap();
    assert!(
        on_disk.contains('Z'),
        "a.txt must be written when it lost focus: {on_disk:?}"
    );
}

/// The mode is off by default, so a focus change alone never writes.
#[test]
fn focus_changes_do_not_save_when_the_mode_is_off() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    // Pin both modes off explicitly: App::new loads the developer's real
    // prefs, so asserting a DEFAULT here would depend on machine state.
    app.auto_save = false;
    app.auto_save_on_focus_change = false;
    app.focus_pane(Pane::Editor);
    app.tick_auto_save();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app.focus_pane(Pane::Terminal);
    assert!(!app.tick_auto_save(), "the mode is off");
    assert!(app.editor.dirty, "the buffer stays dirty");
}

#[test]
fn auto_save_writes_a_dirty_buffer_after_the_delay() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.auto_save = true;
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.dirty, "typing dirties the buffer");
    assert!(
        !app.tick_auto_save(),
        "a buffer edited a moment ago must not save yet"
    );
    assert!(app.editor.dirty, "still dirty before the delay elapses");
    age_last_edit(&mut app.editor);
    assert!(app.tick_auto_save(), "the aged buffer must save");
    assert!(!app.editor.dirty, "saving clears the dirty flag");
    let on_disk = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert!(
        on_disk.contains('x'),
        "the edit must be on disk, got: {on_disk:?}"
    );
}

#[test]
fn auto_save_surfaces_a_disk_conflict_instead_of_latching_silently() {
    // A dirty buffer collides with an external write: save_to_disk latches
    // disk_conflict, due() then excludes the tab forever, and without a
    // prompt the user learns nothing while auto save silently stops.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.auto_save = true;
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    std::fs::write(tmp.path().join("a.txt"), "external change wins\n").unwrap();
    age_last_edit(&mut app.editor);
    assert!(
        app.tick_auto_save(),
        "a conflict-only tick must signal a redraw so the prompt shows now, not on the next blink"
    );
    assert!(app.editor.disk_conflict, "the conflict latches");
    assert!(
        app.input_prompt.is_some() || app.status.to_lowercase().contains("conflict"),
        "the collision must be surfaced, not swallowed; status: {:?}",
        app.status
    );
}

#[test]
fn explicit_save_prompts_before_a_lossy_encoding_write_then_second_press_consents() {
    // Issue #41: encoding_rs substitutes HTML numeric character references
    // (not `?`) for unmappable characters, so an unguarded save silently
    // rewrites 日本語 as `&#26085;…` on disk while the buffer looks fine.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "plain");
    app.editor
        .reopen_with_encoding(encoding_rs::WINDOWS_1252)
        .unwrap();
    app.editor.lines = vec![String::from("日本語 costs 5€")];
    app.editor.dirty = true;
    app.save();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "plain",
        "the first Cmd+S must refuse, leaving the file untouched"
    );
    assert!(
        app.status.contains("windows-1252") && app.status.contains('日'),
        "the refusal must name the encoding and the doomed characters; status: {:?}",
        app.status
    );
    assert!(
        app.editor.lossy_save_armed,
        "the refusal arms the second press"
    );
    app.save();
    assert_eq!(
        std::fs::read(tmp.path().join("a.txt")).unwrap(),
        b"&#26085;&#26412;&#35486; costs 5\x80",
        "the second Cmd+S is explicit consent to the lossy write"
    );
    assert!(!app.editor.dirty, "the consented save completes normally");
}

#[test]
fn auto_save_surfaces_an_encoding_refusal_instead_of_latching_silently() {
    // The same latch-and-tell contract as the disk-conflict arm: the refusal
    // removes the tab from due(), so the transition tick must both redraw
    // and say why auto save stopped — and must never write the mangled bytes.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "plain");
    app.auto_save = true;
    app.editor
        .reopen_with_encoding(encoding_rs::WINDOWS_1252)
        .unwrap();
    app.editor.lines = vec![String::from("日本語")];
    app.editor.dirty = true;
    age_last_edit(&mut app.editor);
    assert!(
        app.tick_auto_save(),
        "a refusal-only tick must signal a redraw so the message shows now"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "plain",
        "auto save must never write the substituted bytes"
    );
    assert!(app.editor.encoding_loss, "the refusal latches");
    assert!(
        !app.editor.lossy_save_armed,
        "auto save must never consent to the lossy write on the user's behalf"
    );
    assert!(
        app.status.contains("a.txt") && app.status.contains("windows-1252"),
        "the stall must be surfaced with the tab and its encoding; status: {:?}",
        app.status
    );
    assert!(
        !app.tick_auto_save(),
        "the latch keeps later ticks quiet instead of re-refusing every second"
    );
}

#[test]
fn auto_save_stays_off_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    assert!(!app.auto_save, "auto save defaults off, like VS Code");
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    age_last_edit(&mut app.editor);
    assert!(!app.tick_auto_save());
    assert!(app.editor.dirty, "nothing saves while the pref is off");
}

#[test]
fn auto_save_never_overwrites_an_external_disk_change() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.auto_save = true;
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    // The file changes under croft before the delay elapses.
    std::fs::write(tmp.path().join("a.txt"), "external edit").unwrap();
    age_last_edit(&mut app.editor);
    app.tick_auto_save();
    let on_disk = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert_eq!(
        on_disk, "external edit",
        "auto save must refuse the write and keep the external content"
    );
}

#[test]
fn auto_save_sweeps_dirty_background_tabs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "first");
    app.auto_save = true;
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    // Open a second file; the dirty first tab moves to the background.
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "second").unwrap();
    app.editor.open_pinned(&b).unwrap();
    for e in app.editor.editors.iter_mut() {
        age_last_edit(e);
    }
    assert!(app.tick_auto_save(), "the background tab must save");
    let on_disk = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert!(
        on_disk.contains('x'),
        "the background tab's edit must be on disk, got: {on_disk:?}"
    );
}

#[test]
fn toggle_auto_save_command_flips_the_pref_and_reports_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.run_command(crate::widgets::command_palette::Command::ToggleAutoSave);
    assert!(app.auto_save);
    assert_eq!(app.status, "Auto Save: on");
    app.run_command(crate::widgets::command_palette::Command::ToggleAutoSave);
    assert!(!app.auto_save);
    assert_eq!(app.status, "Auto Save: off");
}

/// #1306 negative: a file name with a line after it is still a file
/// search, so `b.py:12` opens b.py at line 12, and an `@` or `>` later in
/// the query is just text.
#[test]
fn a_file_name_with_a_line_still_opens_that_file_at_the_line() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=50).map(|i| format!("y = {i}\n")).collect();
    std::fs::write(tmp.path().join("b.py"), body).unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "x = 1\n");
    quick_open_typing(&mut app, "b@>");
    assert!(
        app.file_finder.is_some(),
        "a later @ or > is part of the name"
    );
    assert!(app.command_palette.is_none() && app.go_to_symbol.is_none());
    app.file_finder = None;
    quick_open_typing(&mut app, "b.py:12");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("b.py").as_path())
    );
    assert_eq!(app.editor.cursor_row, 11);
}

/// The private write must never re-mode something it did not create through
/// a link: `set_permissions` follows symlinks, so a scrollback dir that is a
/// link is refused and its target keeps its mode. A real dir left loose by
/// an older run is tightened, and so is a stale dump file.
#[cfg(unix)]
#[test]
fn write_private_refuses_a_symlinked_dir_and_tightens_a_loose_one() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let loose = tmp.path().join("loose");
    std::fs::create_dir(&loose).unwrap();
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o755)).unwrap();
    let stale = loose.join("old.log");
    std::fs::write(&stale, "old").unwrap();
    std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_private(&loose, &stale, "new").expect("a real dir is written");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode(&loose),
        0o700,
        "a loose dir from an older run is tightened"
    );
    assert_eq!(mode(&stale), 0o600, "and so is a stale dump file");
    assert_eq!(std::fs::read_to_string(&stale).unwrap(), "new");

    let target = tmp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let outcome = write_private(&link, &link.join("x.log"), "x");
    assert!(
        outcome.is_err(),
        "a symlinked dir is refused, not written through"
    );
    assert_eq!(mode(&target), 0o755, "the link's target keeps its mode");
    assert!(!target.join("x.log").exists(), "and gains no file");

    // The same for the dump FILE: a link planted where the dump goes must
    // not have its target truncated, overwritten and re-moded.
    let outside = tmp.path().join("outside.txt");
    std::fs::write(&outside, "secret").unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o644)).unwrap();
    let planted = loose.join("planted.log");
    std::os::unix::fs::symlink(&outside, &planted).unwrap();
    let outcome = write_private(&loose, &planted, "new");
    assert!(outcome.is_err(), "a symlinked dump file is refused");
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "secret",
        "its target is untouched"
    );
    assert_eq!(mode(&outside), 0o644, "and keeps its mode");
}

/// Lexical containment is not enough: a normal-looking wire key can name a
/// symlink inside the workspace whose target lies outside it, and the
/// owner-side open would follow the link — disclosing (and editing) files
/// beyond the share. Symlinked keys must resolve inside the workspace or be
/// declined; intra-workspace symlinks keep working.
#[test]
fn a_symlinked_wire_file_key_cannot_escape_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(tmp.path().join("secret.txt"), "SECRET").unwrap();
    std::fs::write(root.join("inner.txt"), "inner").unwrap();
    std::os::unix::fs::symlink(tmp.path().join("secret.txt"), root.join("leak.txt")).unwrap();
    std::fs::create_dir(tmp.path().join("outside")).unwrap();
    std::fs::write(tmp.path().join("outside").join("deep.txt"), "DEEP").unwrap();
    std::os::unix::fs::symlink(tmp.path().join("outside"), root.join("sub")).unwrap();
    std::os::unix::fs::symlink(root.join("inner.txt"), root.join("alias.txt")).unwrap();
    let mut app = App::new(root.clone()).unwrap();
    assert_eq!(
        app.owner_buffer_text(&root, "leak.txt"),
        None,
        "a symlink to a file outside the workspace must be declined"
    );
    assert_eq!(
        app.owner_buffer_text(&root, "sub/deep.txt"),
        None,
        "a path through a symlinked directory must not escape the workspace"
    );
    assert_eq!(
        app.owner_buffer_text(&root, "alias.txt").as_deref(),
        Some("inner"),
        "a symlink resolving inside the workspace still works"
    );
}

/// Cmd+K Y yields the active file to the navigator (a COMMENT-ONLY turn
/// carrying the numbered buffer). Commentary normally lands as a comment
/// box; here no owner serves the file (the host was seated by hand, no
/// self-appointment), so the words fall back to the Navigator OUTPUT
/// channel rather than being lost.
#[test]
fn cmd_k_y_yields_the_active_file_and_commentary_reaches_output() {
    use std::time::{Duration, Instant};
    if !crate::lsp::manager::is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "a\nb\nc").unwrap();
    let socket = tmp.path().join("collab.sock");
    {
        let s = socket.clone();
        std::thread::spawn(move || {
            let _ = crate::collab::relay_serve(&s);
        });
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !crate::session::is_alive(&socket) {
        assert!(Instant::now() < deadline, "relay never came up");
        std::thread::sleep(Duration::from_millis(10));
    }
    let script = tmp.path().join("fake_claude.py");
    std::fs::write(&script, crate::pair::FAKE_CLAUDE).unwrap();
    let log = tmp.path().join("stdin.log");

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_at_launch(&file);

    // Without a host the chord hints instead of panicking.
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('y'), KeyModifiers::NONE)));
    assert!(
        app.status.to_lowercase().contains("not active"),
        "got: {}",
        app.status
    );

    let mut cmd = std::process::Command::new("python3");
    cmd.arg(&script).arg(&log).arg("notes");
    app.pair_host =
        Some(crate::pair_host::PairHost::spawn_cmd(&socket, "navigator", None, cmd).unwrap());
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('y'), KeyModifiers::NONE)));
    assert!(
        app.status.to_lowercase().contains("yielded"),
        "got: {}",
        app.status
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let sent = std::fs::read_to_string(&log).unwrap_or_default();
        if sent.contains("COMMENT-ONLY") && sent.contains("0|a") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "yield turn never reached claude; log: {sent}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The fake's "Reviewing." commentary flows to the Navigator channel.
    // OUTPUT is process-global, so only lines past this test's own baseline
    // count: a sibling test's residue on the same channel must not be able
    // to satisfy the wait.
    let baseline = crate::output::snapshot("Navigator")
        .unwrap_or_default()
        .len();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.poll_pair();
        let lines = crate::output::snapshot("Navigator").unwrap_or_default();
        if lines
            .iter()
            .skip(baseline)
            .any(|l| l.text.contains("Reviewing."))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "commentary never reached OUTPUT; got {lines:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// #37: a failed external reload was silently discarded — the sweep tried,
/// the re-read died (file replaced by a directory), and nothing said so.
/// The tab keeps its last good view and the status line reports it.
#[test]
fn a_failed_external_reload_reports_in_the_status_line() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("f.txt");
    std::fs::write(&f, "one\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    std::fs::remove_file(&f).unwrap();
    std::fs::create_dir(&f).unwrap();
    assert!(
        app.reload_open_file_after_external_change(),
        "a failed reload is redraw-worthy, not an empty sweep"
    );
    assert!(
        app.status.contains("Reload failed"),
        "the status line must name the failure; got: {}",
        app.status
    );
    assert_eq!(
        app.editor.lines[0], "one",
        "the last good buffer survives the failed reload"
    );
}

#[test]
fn create_prompt_labels_speak_workspace_relative() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("src")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_create_prompt(CreateKind::File, tmp.path().join("src"));
    let label = app.prompt.as_ref().unwrap().label.clone();
    assert_eq!(
        label, "New File in src",
        "the prompt title's one job is naming the target directory; a deep \
         absolute workspace path truncates it out of the popup (#105)"
    );
    app.prompt = None;
    app.open_create_prompt(CreateKind::Folder, tmp.path().to_path_buf());
    let label = app.prompt.as_ref().unwrap().label.clone();
    assert_eq!(
        label, "New Folder in .",
        "the workspace root itself reads as '.'"
    );
}

/// The sweep's auto-enable is capped by workspace size, and the setting
/// overrides the cap in both directions (#256, criterion 5).
///
/// `auto` is a size question, so it has to be answered by counting - but
/// counting a giant tree to decide not to walk a giant tree is the cost the
/// cap exists to avoid. `workspace_exceeds` stops at the threshold rather
/// than completing the walk, so the answer costs at most `limit + 1`
/// entries however large the root is.
#[test]
fn the_sweep_auto_enables_only_under_the_file_threshold() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..12 {
        std::fs::write(tmp.path().join(format!("f{i}.ts")), "export const x = 1;\n").unwrap();
    }

    // Under the cap: auto enables.
    assert!(
        !App::workspace_exceeds(tmp.path(), 100),
        "12 files is under a cap of 100"
    );

    // The exact boundary, both sides. "Exceeds" means strictly more, so a
    // root holding exactly `limit` files does NOT exceed it - without this
    // pair, `>` and `>=` are indistinguishable and the doc comment's claim
    // is unverified.
    assert!(
        !App::workspace_exceeds(tmp.path(), 12),
        "exactly 12 files does not exceed a cap of 12"
    );
    assert!(
        App::workspace_exceeds(tmp.path(), 11),
        "12 files exceeds a cap of 11"
    );
    assert!(
        App::project_check_auto_enabled(tmp.path(), "auto", 100),
        "auto enables under the cap"
    );

    // Over the cap: auto declines.
    assert!(
        App::workspace_exceeds(tmp.path(), 5),
        "12 files exceeds a cap of 5"
    );
    assert!(
        !App::project_check_auto_enabled(tmp.path(), "auto", 5),
        "auto declines over the cap"
    );

    // The setting overrides the count in BOTH directions - a cap that only
    // ever said no would make `on` meaningless on the roots that need it.
    assert!(
        App::project_check_auto_enabled(tmp.path(), "on", 5),
        "`on` runs on a root the cap would have declined"
    );
    assert!(
        !App::project_check_auto_enabled(tmp.path(), "off", 100),
        "`off` declines a root the cap would have allowed"
    );

    // An unrecognised value reads as `auto` rather than silently enabling a
    // sweep on a monorepo: a typo must not cost more than it saves.
    assert!(
        !App::project_check_auto_enabled(tmp.path(), "definitely-not-a-mode", 5),
        "an unknown setting falls back to auto, which declines here"
    );
    assert!(
        App::project_check_auto_enabled(tmp.path(), "definitely-not-a-mode", 100),
        "and to auto's yes under the cap, not a blanket no"
    );
}

/// The cap hints ONCE per session, and only under `auto` (#256).
///
/// The hint is the cap's user-visible half: an explicit sweep still runs on
/// a huge root, but the user is told once that automatic runs will not
/// start. Repeating it every sweep would train them to ignore the status
/// line, and showing it under `on` would contradict the setting they chose.
#[test]
fn the_size_hint_fires_once_and_only_under_auto() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // `on` means run regardless of size, so a size hint would contradict it.
    app.problems_project_scope = String::from("on");
    assert!(
        App::project_check_auto_enabled(tmp.path(), &app.problems_project_scope, 0),
        "`on` runs even against a zero-file cap"
    );

    // `off` never auto-runs, so the size is not the reason - no hint either.
    app.problems_project_scope = String::from("off");
    assert!(!App::project_check_auto_enabled(
        tmp.path(),
        &app.problems_project_scope,
        usize::MAX
    ));

    // Only `auto` makes size the deciding factor. The root needs a file for
    // a cap of 0 to be EXCEEDED - an empty tree does not exceed zero, and
    // asserting against one would have passed for the wrong reason.
    std::fs::write(tmp.path().join("a.ts"), "export const a = 1;\n").unwrap();
    app.problems_project_scope = String::from("auto");
    assert!(
        !App::project_check_auto_enabled(tmp.path(), "auto", 0),
        "auto declines when the root exceeds the cap"
    );
    assert!(
        App::project_check_auto_enabled(tmp.path(), "auto", usize::MAX),
        "and enables under it - the paired case, so the line above cannot \
         pass by the predicate always saying no"
    );

    // Config values are normalised like the sibling pref: a hand-edited
    // "Off " must mean off, not a size-dependent maybe.
    assert_eq!(App::project_scope_mode(" Off "), "off");
    assert_eq!(App::project_scope_mode("ON"), "on");
    assert_eq!(App::project_scope_mode("always"), "on");
    assert_eq!(App::project_scope_mode("never"), "off");
    assert_eq!(
        App::project_scope_mode("wat"),
        "auto",
        "an unknown value is auto, never on"
    );
    assert!(
        !App::project_check_auto_enabled(tmp.path(), " Off ", usize::MAX),
        "a padded, capitalised off is honoured rather than read as auto"
    );
}

/// Counting stops at the threshold instead of walking the whole tree
/// (#256, criterion 5).
///
/// Pinned by making the tree far larger than the cap and asserting the
/// answer arrives anyway: a full walk of a deep tree is the thing the cap
/// is protecting against, so a cap that walks it has already lost.
#[test]
fn the_threshold_check_stops_counting_at_the_limit() {
    let tmp = tempfile::tempdir().unwrap();
    // 500 files against a cap of 3.
    for d in 0..10 {
        let dir = tmp.path().join(format!("d{d}"));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..50 {
            std::fs::write(dir.join(format!("f{i}.ts")), "x\n").unwrap();
        }
    }
    assert!(App::workspace_exceeds(tmp.path(), 3), "500 files exceeds 3");

    // Ignored directories do not count toward the cap: node_modules alone
    // would put every JS project over it, which would disable the sweep for
    // exactly the projects the feature is for.
    let clean = tempfile::tempdir().unwrap();
    let heavy = clean.path().join("node_modules").join("pkg");
    std::fs::create_dir_all(&heavy).unwrap();
    for i in 0..200 {
        std::fs::write(heavy.join(format!("m{i}.js")), "x\n").unwrap();
    }
    std::fs::write(clean.path().join("a.ts"), "export const a = 1;\n").unwrap();
    assert!(
        !App::workspace_exceeds(clean.path(), 10),
        "node_modules is not the user's code and must not trip the cap"
    );
}

/// An Explorer rename of `calc.srn` to `sums.srn` whose server answer also
/// renames `total` inside the moved file, which is open in a tab.
fn move_calc_to_sums(app: &App, file: &Path) -> PendingFileMove {
    let root = app.tree.root.clone();
    PendingFileMove {
        request_id: 9,
        op: FileMove::Rename {
            parent: root.clone(),
            old: file.to_path_buf(),
            new_name: String::from("sums.srn"),
        },
        renames: vec![crate::lsp::manager::FileRenameOp {
            old: file.to_path_buf(),
            new: root.join("sums.srn"),
            is_dir: false,
        }],
        deadline: std::time::Instant::now(),
        baseline: app.editor.edit_seqs(),
    }
}

#[test]
fn a_file_move_moves_but_leaves_a_buffer_typed_into_since_the_request_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_calc_open(&tmp);
    let pending = move_calc_to_sums(&app, &file);
    type_a_header_line(&mut app);
    app.finish_file_move(pending, rename_total_to_sum(&file));
    assert!(
        app.tree.root.join("sums.srn").exists(),
        "the move still ran"
    );
    assert_eq!(
        app.editor.lines.join("\n"),
        "# header\ntotal = 1\nprint(total)\ntotal += 2"
    );
    assert!(
        app.status
            .ends_with("; references not updated: sums.srn changed while the server was working"),
        "{}",
        app.status
    );
}

#[test]
fn a_file_move_still_updates_an_untouched_buffer_it_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_calc_open(&tmp);
    let pending = move_calc_to_sums(&app, &file);
    app.finish_file_move(pending, rename_total_to_sum(&file));
    assert_eq!(
        app.editor.path.as_deref(),
        Some(app.tree.root.join("sums.srn").as_path())
    );
    assert_eq!(app.editor.lines.join("\n"), "sum = 1\nprint(sum)\nsum += 2");
    assert!(
        app.status.contains("references updated in 1 file"),
        "{}",
        app.status
    );
}

/// #369: auto save writes a file with a symbol tab once. Both tabs hold the
/// edit and are due together; writing the second would read the first
/// write as an external change and raise a conflict prompt.
#[test]
fn auto_save_writes_a_file_with_a_symbol_tab_once_and_cleans_both() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.auto_save = true;
    app.sync_symbol_views();
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('7'), KeyModifiers::NONE))
        .unwrap();
    app.sync_symbol_views();
    let past = std::time::Instant::now() - std::time::Duration::from_secs(5);
    for e in app.editor.editors.iter_mut() {
        assert!(e.dirty, "fixture: both tabs hold the edit");
        e.last_edit_at = Some(past);
    }
    assert!(app.tick_auto_save());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn a() {\n    1\n}\nfn b() {\n    27\n}"
    );
    for e in &app.editor.editors {
        assert!(!e.dirty && !e.disk_conflict, "both tabs are saved");
    }
    assert!(app.input_prompt.is_none(), "no conflict prompt");
    assert!(!app.reload_open_file_after_external_change());
}

/// #369: both tabs of a file with a symbol tab meet a disk conflict
/// together, and the prompt names the file once.
#[test]
fn auto_save_reports_a_symbol_tabs_file_conflict_once() {
    use crate::widgets::input_prompt::InputPurpose;
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.auto_save = true;
    app.sync_symbol_views();
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('7'), KeyModifiers::NONE))
        .unwrap();
    app.sync_symbol_views();
    std::fs::write(
        &file,
        "fn a() {\n    1\n}\nfn b() {\n    changed elsewhere\n}",
    )
    .unwrap();
    let past = std::time::Instant::now() - std::time::Duration::from_secs(5);
    for e in app.editor.editors.iter_mut() {
        e.last_edit_at = Some(past);
    }
    assert!(app.tick_auto_save());
    assert!(
        app.editor.editors.iter().all(|e| e.disk_conflict),
        "both tabs latch the conflict"
    );
    match app.input_prompt.as_ref().map(|p| &p.purpose) {
        Some(InputPurpose::ReloadConflict { paths }) => assert_eq!(paths, &vec![file]),
        _ => panic!("a conflict prompt"),
    }
}

/// #369: an encoding refusal on a file with a symbol tab names the file
/// once, not once per tab.
#[test]
fn auto_save_names_a_symbol_tabs_file_once_when_its_encoding_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _file) = app_with_symbol_tab_on_b(&tmp);
    app.auto_save = true;
    app.sync_symbol_views();
    let past = std::time::Instant::now() - std::time::Duration::from_secs(5);
    for e in app.editor.editors.iter_mut() {
        e.encoding = encoding_rs::WINDOWS_1252;
        e.lines[4] = String::from("    \u{65e5}\u{672c}");
        e.dirty = true;
        e.last_edit_at = Some(past);
    }
    assert!(app.tick_auto_save());
    assert!(app.editor.editors.iter().all(|e| e.encoding_loss));
    assert_eq!(app.status.matches("two.rs").count(), 1, "{}", app.status);
}

/// #345: the watcher's three ledger-facing behaviours, driven through the
/// real drain rather than asserted about by reading it.
///
/// A rescan means events were DROPPED, so the queue is a lower bound and
/// the ledger must record that durably; a removed directory is not a file
/// and must not enter a file-only ledger; and a removed FILE must still be
/// reported, so the row can leave the queue.
#[test]
fn the_watcher_reports_rescans_and_keeps_directories_out_of_the_ledger() {
    use notify::Event as NotifyEvent;
    use notify::event::{EventKind, ModifyKind, RemoveKind};
    use notify_debouncer_full::DebouncedEvent;
    use std::sync::mpsc;

    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("touched.rs");
    std::fs::write(&file, "fn a() {}\n").unwrap();
    let doomed_dir = tmp.path().join("subdir");
    std::fs::create_dir(&doomed_dir).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0].set_agent(Some(crate::agents::AgentLane {
        name: String::from("claude"),
        status: crate::agents::AgentStatus::Working,
    }));

    let (tx, rx) = mpsc::channel();
    app.fs_watch.set_test_events(rx);

    // A removed directory, a removed file, and an ordinary write.
    let mut dir_gone = NotifyEvent::new(EventKind::Remove(RemoveKind::Folder));
    dir_gone = dir_gone.add_path(doomed_dir.clone());
    let mut written = NotifyEvent::new(EventKind::Modify(ModifyKind::Any));
    written = written.add_path(file.clone());
    let now = std::time::Instant::now();
    tx.send(Ok(vec![
        DebouncedEvent::new(dir_gone, now),
        DebouncedEvent::new(written, now),
    ]))
    .unwrap();
    app.drain_fs_events();

    assert!(
        app.agent_ledger.is_unreviewed(&file),
        "the written file is attributed to the working agent"
    );
    assert!(
        !app.agent_ledger.is_unreviewed(&doomed_dir),
        "a removed DIRECTORY must not enter a file-only ledger"
    );
    assert!(
        !app.agent_ledger.may_be_incomplete(),
        "no rescan yet, so the queue is not a lower bound"
    );

    // A removed FILE that IS in a lane leaves it the moment the removal is
    // seen: a deleted file generates no later write, so a guard that
    // excluded its event would strand the row in the queue forever.
    let doomed = tmp.path().join("doomed.rs");
    std::fs::write(&doomed, "fn gone() {}\n").unwrap();
    let mut doomed_set = std::collections::BTreeSet::new();
    doomed_set.insert(doomed.clone());
    assert!(app.attribute_writes_to_working_agents(&doomed_set));
    assert!(app.agent_ledger.is_unreviewed(&doomed));
    std::fs::remove_file(&doomed).unwrap();
    let (tx3, rx3) = mpsc::channel();
    app.fs_watch.set_test_events(rx3);
    // `Any` is what kqueue, the Windows watcher and the poll fallback emit
    // for every removal, so this is the shape three of five backends send.
    let removed = NotifyEvent::new(EventKind::Remove(RemoveKind::Any)).add_path(doomed.clone());
    tx3.send(Ok(vec![DebouncedEvent::new(
        removed,
        std::time::Instant::now(),
    )]))
    .unwrap();
    app.drain_fs_events();
    assert!(
        !app.agent_ledger.is_unreviewed(&doomed),
        "the removed file's row leaves the queue rather than stranding"
    );

    // Now a rescan: the watcher overflowed and dropped events.
    let (tx2, rx2) = mpsc::channel();
    app.fs_watch.set_test_events(rx2);
    let rescan = NotifyEvent::new(EventKind::Modify(ModifyKind::Any))
        .add_path(file.clone())
        .set_flag(notify::event::Flag::Rescan);
    tx2.send(Ok(vec![DebouncedEvent::new(
        rescan,
        std::time::Instant::now(),
    )]))
    .unwrap();
    app.drain_fs_events();
    assert!(
        app.agent_ledger.may_be_incomplete(),
        "a rescan makes every count a lower bound, durably"
    );

    // And the doubt survives an unrelated status change, which is the
    // point of holding it in the ledger rather than the status line.
    app.status = String::from("something else entirely");
    assert!(app.agent_ledger.may_be_incomplete());
}

/// A post-draw overlay whose rect does not fit the screen is not emitted
/// (#513). The flush block writes with absolute CUP straight to stdout,
/// so an out-of-range origin cannot panic and ratatui cannot repair it --
/// the payload has to be refused before it is written.
#[test]
fn an_overlay_past_the_screen_edge_is_refused_before_it_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    // Inside the screen: emittable.
    assert!(
        app.overlay_fits_on_screen(0, 0, 80, 24),
        "a payload filling the screen exactly must be allowed"
    );
    assert!(app.overlay_fits_on_screen(10, 5, 20, 10), "well inside");

    // Past either edge: refused. These are the shapes a stale rect makes --
    // the pre-maximize column of a pane that is now narrower, or a row
    // index from a taller layout.
    assert!(
        !app.overlay_fits_on_screen(70, 0, 20, 5),
        "past the right edge (70+20 > 80)"
    );
    assert!(
        !app.overlay_fits_on_screen(0, 20, 10, 10),
        "past the bottom edge (20+10 > 24)"
    );
    assert!(
        !app.overlay_fits_on_screen(u16::MAX - 1, 0, 4, 4),
        "a wild origin must saturate rather than wrap into range"
    );

    // A zero-sized payload has nothing to place. Refused rather than
    // trivially in bounds, so a cleared rect cannot slip a bare cursor
    // park through.
    assert!(!app.overlay_fits_on_screen(0, 0, 0, 5), "zero width");
    assert!(!app.overlay_fits_on_screen(0, 0, 5, 0), "zero height");
}

#[test]
fn a_same_length_rewrite_with_a_preserved_mtime_refreshes_on_the_watcher_path() {
    // `(mtime, len)` cannot see a rewrite that keeps both, and the poll has
    // nothing else to go on. The watcher does: it names the path, and a
    // named path bypasses the stamp gate.
    let (tmp, f) = repo_with_seed("one\ntwo\n");
    let root = tmp.path().to_path_buf();
    let mut app = App::new(root.clone()).unwrap();
    std::fs::write(&f, "one\ntwo\nAAA\n").unwrap();
    app.editor
        .open_head_diff_with_text(
            std::path::PathBuf::from("seed.txt (HEAD)"),
            "one\ntwo\n",
            &f,
            true,
        )
        .unwrap();
    app.tag_open_diff(crate::widgets::diff::DiffSource::HeadVsWorking {
        root: root.clone(),
        rel: String::from("seed.txt"),
    });
    let mtime = std::fs::metadata(&f).unwrap().modified().unwrap();
    std::fs::write(&f, "one\ntwo\nBBB\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(
        std::fs::metadata(&f).unwrap().modified().unwrap(),
        mtime,
        "precondition: the rewrite preserved the mtime"
    );
    assert!(
        !app.refresh_open_diff_views(false, &[], &no_paths()),
        "the poll's stamp gate cannot see this rewrite"
    );
    let mut written = std::collections::BTreeSet::new();
    written.insert(f.clone());
    assert!(
        app.refresh_open_diff_views(false, &[], &written),
        "the watcher naming the path rebuilds the view regardless of the stamp"
    );
    assert!(
        app.editor
            .diff
            .as_ref()
            .unwrap()
            .right_lines
            .iter()
            .any(|l| l == "BBB"),
        "the view shows the rewritten bytes"
    );
}

#[test]
fn a_view_request_for_a_directory_is_refused_with_a_reason_the_client_prints() {
    // The server knows why; the client only relays. A silent refusal would
    // exit the user's shell 0 having opened nothing.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("subdir");
    std::fs::create_dir(&dir).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let sock = seat_view_listener(&mut app, tmp.path());

    let client = view_from_a_pane(&sock, &dir);
    // Load-scaled, and it still cannot hang: `await_spawned` panics naming
    // what it waited for, so the `join` below is only reached once the client
    // thread has finished (CONTRIBUTING.md, "Waiting on a spawned process in
    // a test"). A fixed two seconds decided pass or fail here before.
    drain_until_answered(&mut app, &client);
    match client.join().unwrap() {
        crate::view_ipc::ViewReply::Err { message } => {
            assert!(
                message.contains("is a directory"),
                "the reason must name what went wrong, got {message:?}"
            );
        }
        other => panic!("a directory must not open as a file: {other:?}"),
    }
}

#[test]
fn the_socket_sweep_removes_a_dead_crofts_file_and_spares_a_live_one() {
    // Sockets are not auto-unlinked, so a crashed croft leaves its file for
    // the next one to clear. Getting this wrong in the other direction would
    // be worse than litter: deleting a LIVE croft's socket silently breaks
    // `croft view` in every pane it owns.
    let tmp = tempfile::tempdir().unwrap();
    // A pid that has certainly exited: spawn, reap, and its number is free.
    let dead = {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    };
    // A live pid that is NOT this process. Naming the live socket for
    // `process::id()` made this half unfalsifiable: production reads
    // `pid != me && kill(pid, 0) != 0`, so for our own pid the first term
    // decides it and the liveness probe never runs. Measured - replacing the
    // whole condition with `pid != me` left all three assertions passing,
    // while production would then unlink the socket of every OTHER running
    // croft, which is the outcome the comment above calls worse than litter.
    let mut peer = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let live = peer.id();
    // `view-<pid>-<nonce>.sock` is the ONLY shape production writes now, so
    // the fixture uses it. The pre-nonce spelling is kept for the dead pid as
    // well, because the sweep still has to clear what an older croft left.
    let dead_sock = tmp.path().join(format!("view-{dead}.sock"));
    let dead_nonce_sock = tmp.path().join(format!("view-{dead}-a1b2c3d4.sock"));
    let live_sock = tmp.path().join(format!("view-{live}-e5f6a7b8.sock"));
    let stranger = tmp.path().join("not-a-view-socket");
    // The lock `bind_socket_0600` writes beside each socket, which the sweep
    // is supposed to take with it.
    let dead_lock = tmp.path().join(format!("view-{dead}-a1b2c3d4.bind.lock"));
    let live_lock = tmp.path().join(format!("view-{live}-e5f6a7b8.bind.lock"));
    // A lock whose socket NEVER APPEARED: a croft that died between the
    // lock's creation and the rename leaves this, and nothing keyed on the
    // `.sock` suffix can ever reach it.
    let orphan_lock = tmp.path().join(format!("view-{dead}-99887766.bind.lock"));
    // `bind_socket_0600` binds inside `.s<pid>-<n>` and removes it on both
    // paths; a croft killed inside that window strands one, and nothing else
    // ever looks at it again.
    let dead_stage = tmp.path().join(format!(".s{dead}-0"));
    let live_stage = tmp.path().join(format!(".s{live}-0"));
    // Same prefix, no counter: not ours, so the sweep must leave it.
    let stage_lookalike = tmp.path().join(format!(".s{dead}-notanumber"));
    for d in [&dead_stage, &live_stage, &stage_lookalike] {
        std::fs::create_dir(d).unwrap();
        std::fs::write(d.join("s"), b"").unwrap();
    }
    for p in [
        &dead_sock,
        &dead_nonce_sock,
        &live_sock,
        &stranger,
        &dead_lock,
        &live_lock,
        &orphan_lock,
    ] {
        std::fs::write(p, b"").unwrap();
    }

    crate::app::sweep_dead_view_sockets(tmp.path());
    let live_survived = live_sock.exists();
    let live_lock_survived = live_lock.exists();
    let live_stage_survived = live_stage.exists();
    let _ = peer.kill();
    let _ = peer.wait();

    assert!(!dead_sock.exists(), "a dead croft's socket must be cleared");
    assert!(
        !dead_nonce_sock.exists(),
        "and the nonce-shaped name, which is the only one production writes"
    );
    assert!(
        !dead_lock.exists(),
        "the lock beside a dead croft's socket goes with it"
    );
    assert!(
        !orphan_lock.exists(),
        "a lock whose socket never appeared must be swept too, or it leaks \
         forever: nothing else in croft ever looks at it again"
    );
    assert!(
        live_survived,
        "another croft is alive, so its socket must survive the sweep"
    );
    assert!(
        live_lock_survived,
        "and so must its lock: removing it lets a second croft bind the \
         socket this one is serving"
    );
    assert!(
        !dead_stage.exists(),
        "a dead croft's bind staging directory leaks forever otherwise: \
         nothing but this sweep ever reads that name again"
    );
    assert!(
        live_stage_survived,
        "and a LIVE croft's staging directory must survive: removing one \
         mid-bind takes the socket it is about to rename out with it"
    );
    assert!(
        stage_lookalike.exists(),
        "the counter must parse as a number, or the sweep is matching on a \
         prefix and will eat directories it did not create"
    );
    assert!(
        stranger.exists(),
        "the sweep must only touch files it recognises by name"
    );
}

/// One drain serves EVERY client waiting, not just the first (#362).
///
/// `drain_view_requests` loops on accept under a shared 20ms deadline
/// precisely so a frame that finds three `croft view` calls queued answers
/// all three. Nothing pinned that: every other fixture hands it exactly one
/// connection, so replacing the loop body's tail with `return changed` after
/// the first client passes the whole suite.
///
/// Both requests are written SYNCHRONOUSLY from this thread before the drain
/// runs, so "both were pending in the same frame" is a property of the
/// fixture rather than of the scheduler.
#[test]
fn one_drain_answers_every_client_already_waiting() {
    use std::io::{BufRead, Write};
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("first.txt");
    let second = tmp.path().join("second.txt");
    std::fs::write(&first, "one").unwrap();
    std::fs::write(&second, "two").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Production's 20ms is shorter than one file open under a loaded full
    // suite, and then the drain rightly defers client 1 to the next frame.
    // Stretched, a drain that stops after the first client still fails.
    app.view_drain_budget =
        crate::test_budget::spawn_budget(crate::test_budget::tests::VIEW_DRAIN_BASE);
    let sock = seat_view_listener(&mut app, tmp.path());

    let mut clients = Vec::new();
    for target in [&first, &second] {
        let mut c = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        // A read timeout, so losing FAILS rather than hangs: a drain that
        // serves only client 0 still passes the assert below, because
        // something opened, and an untimed `read_line` on client 1 would then
        // block forever. That is a CI timeout with no message, which two
        // other tests in this file go out of their way to avoid.
        c.set_read_timeout(Some(crate::test_budget::spawn_budget(
            crate::test_budget::tests::VIEW_DRAIN_BASE,
        )))
        .unwrap();
        // Through `ViewRequest::new`, so a change to the byte encoding is
        // exercised here rather than bypassed by hand-rolling the field.
        let req = crate::view_ipc::ViewRequest::new(target);
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        c.write_all(line.as_bytes()).unwrap();
        c.flush().unwrap();
        clients.push(c);
    }

    assert!(
        app.drain_view_requests(),
        "one drain, with two complete requests already on the socket"
    );

    for (i, c) in clients.iter().enumerate() {
        let mut reply = String::new();
        std::io::BufReader::new(c)
            .read_line(&mut reply)
            .unwrap_or_else(|e| {
                panic!(
                    "client {i} was never answered ({e}): one drain must serve \
                     every client already waiting, not just the first"
                )
            });
        let reply: crate::view_ipc::ViewReply = serde_json::from_str(reply.trim())
            .unwrap_or_else(|e| panic!("client {i} got no parseable reply: {e}"));
        assert_eq!(
            reply,
            crate::view_ipc::ViewReply::Ok,
            "client {i} must be answered by the SAME drain as client 0: a loop \
             that serves one connection and returns leaves this one waiting a \
             whole frame, and every other test hands the drain a single client"
        );
    }
    let open: Vec<_> = app
        .editor
        .editors
        .iter()
        .filter_map(|e| e.path.clone())
        .collect();
    assert!(
        open.contains(&first) && open.contains(&second),
        "both files must be open, got {open:?}"
    );
}

/// A relative path is refused rather than resolved against croft's cwd (#362).
///
/// `view_ipc`'s header states that the server only ever sees absolute paths,
/// because the client resolves against ITS cwd before sending. Nothing
/// enforced it, so a relative path from anything that is not `croft view`
/// opened whatever that name meant from croft's own working directory, which
/// is a different file than the sender named. Same-uid, so this is an
/// invariant rather than a boundary, but an unchecked invariant is a comment.
#[test]
fn a_relative_path_in_a_request_is_refused_rather_than_resolved_here() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    let reply = app.apply_view_request(std::path::Path::new("relative.txt"));

    match reply {
        crate::view_ipc::ViewReply::Err { message } => assert!(
            message.contains("absolute"),
            "the refusal must name the invariant it is enforcing, got {message:?}"
        ),
        other => panic!("a relative path must not be resolved here: {other:?}"),
    }
}

/// The status arm that no test could reach (#362).
///
/// `bind_view_socket` returns early under `cfg(test)` so a test cannot leave
/// a live socket in the real cache dir, which also means nothing in the suite
/// ever latches `VIEW_BIND_ERROR`: the arm that tells the user their bind
/// failed had never once run. `view_status` is the pure half, split out so it
/// can. Short-circuit it to always answer "Ready" and this goes red.
#[test]
fn a_failed_view_bind_reaches_the_status_line_with_its_cause() {
    assert_eq!(
        crate::app::view_status(None),
        "Ready",
        "a croft whose bind succeeded says nothing about it"
    );
    let said = crate::app::view_status(Some("Permission denied (os error 13)"));
    assert!(
        said.contains("croft view"),
        "the message must name the feature that is off, or the user cannot \
         connect it to the command that fails: {said:?}"
    );
    assert!(
        said.contains("Permission denied (os error 13)"),
        "and it must carry the CAUSE: a bare 'unavailable' sends the user \
         looking in the wrong place, which is what the latch exists to \
         prevent: {said:?}"
    );
}

#[test]
fn a_client_that_never_sends_a_newline_is_refused_by_the_deadline() {
    // The socket's read timeout does NOT bound `read_request` on its own:
    // SO_RCVTIMEO applies per `recv`, while the read loops until it sees a
    // newline. A client writing inside every window re-arms it forever, and
    // the drain runs on the frame loop, so that freezes the editor.
    //
    // Asserted on the REPLY, not on elapsed time. The wall-clock version of
    // this test was load-sensitive to the point of being a flake in waiting:
    // measured, the fixed path took 20-90ms and the broken path 147-495ms, so
    // the two bands nearly touch and any threshold between them is a coin
    // toss on a busy machine. The deadline path produces a message no other
    // path produces, which is load-independent and says WHICH bound fired.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let sock = seat_view_listener(&mut app, tmp.path());

    let sock2 = sock.clone();
    // The dribbler stops when the SERVER is done with it, not after a fixed
    // count. It used to run 240 x 5ms, a flat 1.2s, while the accept it races
    // is load-scaled to as much as sixteen seconds: on a machine slow enough
    // to need that budget the dribbler had already exited, the server read
    // EOF on `{"path":[47,47`, and the test failed on a serde parse error
    // while asserting about a deadline. That is the same class this fixture
    // was converted to tolerate, left in the fixture instead of the budget.
    let served = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dribbling = std::sync::Arc::clone(&served);
    let dribbler = std::thread::spawn(move || {
        let Ok(mut c) = std::os::unix::net::UnixStream::connect(&sock2) else {
            return;
        };
        // Continuously, and INSIDE the budget. Writing once and going quiet
        // does not reproduce the defect: the per-read timeout fires and the
        // drain moves on, so a fixture shaped that way passes against the
        // unbounded read too. The hang needs a client that keeps re-arming.
        let _ = std::io::Write::write_all(&mut c, br#"{"path":[47"#);
        let _ = std::io::Write::flush(&mut c);
        while !dribbling.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(5));
            if std::io::Write::write_all(&mut c, b",47").is_err() {
                return;
            }
            let _ = std::io::Write::flush(&mut c);
        }
    });

    let listener = app.view_listener.as_ref().unwrap();
    let stream = accept_when_ready(listener);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
    let (reply, opened) = app.answer_view_client(stream, deadline);
    served.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = dribbler.join();

    assert!(
        !opened,
        "nothing was ever named, so nothing can have opened"
    );
    match reply {
        crate::view_ipc::ViewReply::Err { message } => assert!(
            message.contains("did not arrive within the frame budget"),
            "the DEADLINE must be what refuses this client. Any other error \
             means some incidental bound fired instead and the deadline is \
             still unenforced: {message}"
        ),
        other => panic!("a request with no newline cannot succeed: {other:?}"),
    }
}

#[test]
fn a_non_blocking_accepted_stream_is_still_answered() {
    // The PREVIOUS version of this test could not fail: it accepted a
    // connection, called `set_nonblocking(false)` itself, and then asserted
    // the flag it had just set. Deleting the production call left it green on
    // every platform, and its own comment claimed the opposite.
    //
    // The version after that could not fail either, for a NEW reason, and the
    // fix that broke it was in the same round: `read_line_by_deadline` now
    // swallows `WouldBlock`/`TimedOut` and loops, precisely so a per-recv
    // errno cannot end a read early. A non-blocking stream therefore busy
    // spins until the bytes arrive and the reply is `Ok` either way, so
    // asserting the reply says nothing about the flag. Measured: with
    // `set_nonblocking(false)` deleted from production, that version passed.
    //
    // The flag itself is the claim, so the flag is what this asserts. The
    // probe is a DUP rather than a second connection: a dup shares the open
    // file description, so it reads back the same `O_NONBLOCK` the server
    // cleared on the original. The reply assertion stays alongside it, since
    // a cleared flag on an unanswered request would not be a pass either.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let sock = seat_view_listener(&mut app, tmp.path());
    let target = tmp.path().join("seen.txt");
    std::fs::write(&target, b"hello").unwrap();

    let sock2 = sock.clone();
    let target2 = target.clone();
    let client = std::thread::spawn(move || {
        let mut c = std::os::unix::net::UnixStream::connect(&sock2).unwrap();
        // Connect, PAUSE, then write. The pause is what makes the blocking
        // mode observable at all: a client that writes before the server
        // accepts leaves the bytes sitting in the socket buffer, so the first
        // read succeeds immediately whether the socket blocks or not, and the
        // test passes with the flag cleared or set. The read has to WAIT for
        // the mode to matter, which is exactly the case a real `croft view`
        // hits whenever the shell is a little slower than the frame loop.
        std::thread::sleep(std::time::Duration::from_millis(120));
        // `ViewRequest::new` rather than the field by hand, so the byte
        // encoding is exercised rather than reimplemented beside it.
        let req = crate::view_ipc::ViewRequest::new(&target2);
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        let _ = std::io::Write::write_all(&mut c, line.as_bytes());
        let _ = std::io::Write::flush(&mut c);
        std::thread::sleep(std::time::Duration::from_millis(300));
    });
    // Long enough for the CONNECT to land, short enough that the request has
    // not been written yet.
    std::thread::sleep(std::time::Duration::from_millis(30));

    let listener = app.view_listener.as_ref().unwrap();
    let stream = accept_when_ready(listener);
    // The state macOS hands production, forced here so Linux exercises it too.
    stream.set_nonblocking(true).unwrap();
    let probe = stream.try_clone().expect("a dup of the accepted stream");

    // The client thread sleeps before it writes, so this deadline is racing a
    // scheduler, not the server. Load-scaled for the same reason every other
    // spawn wait here is (CONTRIBUTING.md).
    let deadline = std::time::Instant::now()
        + crate::test_budget::spawn_budget(crate::test_budget::tests::VIEW_SERVER_DEADLINE_BASE);
    let (reply, opened) = app.answer_view_client(stream, deadline);
    let _ = client.join();

    let flags = unsafe { libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(&probe), libc::F_GETFL) };
    assert!(
        flags >= 0,
        "F_GETFL on the dup failed: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(
        flags & libc::O_NONBLOCK,
        0,
        "production must clear O_NONBLOCK on an accepted stream: macOS inherits \
         the listener's flags across accept, and a non-blocking read there \
         returns before the client has written"
    );

    assert_eq!(
        reply,
        crate::view_ipc::ViewReply::Ok,
        "and the request is still answered: a cleared flag with no reply \
         behind it would not be a pass"
    );
    assert!(opened, "and the file must actually have been opened");
}

#[test]
fn the_sweep_spares_staged_stdin_a_croft_may_still_be_showing() {
    // The previous version of this test modelled the "live" file as
    // `stdin-{own_pid}-0.csv` and passed. Production never produces that name:
    // `stage_stdin` runs inside `view_ipc::run`, the short-lived `croft view`
    // CLIENT, which exits milliseconds later. So every staged file carries a
    // pid that is dead by construction, the pid-liveness sweep matched all of
    // them, and launching a second croft deleted a file the first one had open
    // in a tab. `sqlite_view::table_page` re-opens by path on every page turn,
    // so that breaks the tab rather than merely littering.
    //
    // Staged through the real function, so the name is the shape production
    // makes rather than the shape the old fixture assumed.
    let tmp = tempfile::tempdir().unwrap();
    let fresh = crate::view_ipc::stage_stdin(tmp.path(), b"a,b\n1,2\n", None).unwrap();
    assert!(fresh.exists(), "precondition: the pipe was staged");

    crate::app::sweep_dead_view_sockets(tmp.path());
    assert!(
        fresh.exists(),
        "a just-staged pipe must survive: a pane may be showing it right now"
    );
}

/// The server stamps its own pid onto a staged file when it opens it (#362).
///
/// The rename is what gives the sweep a live holder to see. It must happen
/// BEFORE the open, so the editor holds the name the sweep will spare rather
/// than one that is about to move under it.
#[test]
fn opening_a_staged_pipe_claims_it_for_this_croft() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = crate::view_ipc::stage_stdin(tmp.path(), b"a,b\n1,2\n", None).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    let reply = app.apply_view_request(&staged);
    assert_eq!(reply, crate::view_ipc::ViewReply::Ok, "the pipe must open");

    assert!(
        !staged.exists(),
        "the staged name must be gone: it is the name the sweep judges by age"
    );
    let open = app.editor.editors[app.editor.active_index()]
        .path
        .clone()
        .expect("a file is open");
    let name = open.file_name().unwrap().to_str().unwrap();
    assert_eq!(
        crate::view_ipc::holder_of_staged(name),
        Some(std::process::id()),
        "the EDITOR must hold the claimed path, or a page turn re-opens a \
         name that no longer exists: {name}"
    );
    assert!(
        open.exists(),
        "and the claimed file must be on disk under its new name"
    );
}

/// An ordinary file is never renamed by the claim (#362).
#[test]
fn opening_a_named_file_does_not_rename_it() {
    let tmp = tempfile::tempdir().unwrap();
    // Deliberately named like a staged file, but NOT in the staging dir: the
    // directory check is what stops croft renaming a user's own file.
    let decoy = tmp.path().join("stdin-1-0.txt");
    std::fs::write(&decoy, b"mine").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    let reply = app.apply_view_request(&decoy);
    assert_eq!(reply, crate::view_ipc::ViewReply::Ok, "it must still open");
    assert!(
        decoy.exists(),
        "a file the user named must keep its name, whatever it looks like"
    );
    assert_eq!(
        app.editor.editors[app.editor.active_index()]
            .path
            .as_deref(),
        Some(decoy.as_path()),
        "and the editor holds the path the user asked for"
    );
}
#[test]
fn the_sweep_removes_staged_stdin_old_enough_that_nothing_can_be_showing_it() {
    // The other half. Age is the honest key here: the pid in the name belongs
    // to a process that is always gone, so liveness answers a question the
    // name cannot pose. Anything older than the retention window predates any
    // running croft's session.
    let tmp = tempfile::tempdir().unwrap();
    let stale = crate::view_ipc::stage_stdin(tmp.path(), b"secret", Some("txt")).unwrap();
    // Backdate past the window without waiting for it.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 * 30);
    let f = std::fs::File::options().write(true).open(&stale).unwrap();
    f.set_modified(old).unwrap();
    drop(f);

    crate::app::sweep_dead_view_sockets(tmp.path());
    assert!(
        !stale.exists(),
        "a staged pipe older than the window must not outlive it: the module's \
         own rationale names `vault read | croft view -`"
    );
}

// NOT a test: `a_client_that_never_reads_its_reply` was written for the
// missing write timeout and removed rather than kept green. Measured, the
// drain returned in 13ms because the reply is bounded by MAX_REQUEST_BYTES
// (~16KB of path) while Linux's default unix-socket buffer is an order of
// magnitude larger, so `write_all` never blocks here whatever the timeout is.
// The fix (`set_write_timeout` in `answer_view_client`) is correct on macOS,
// whose buffers are small enough to hit it and which is croft's primary
// platform - but a test that passes identically with and without it is not
// evidence, and keeping one would misrepresent this as covered.

#[test]
fn the_view_socket_is_bound_before_the_first_pane_is_spawned() {
    // The blocker this PR exists to fix, pinned. `croft view` failed in the
    // STARTUP pane because `App::new` built that pane before binding the
    // socket, and portable-pty snapshots the environment inside
    // `CommandBuilder::new`. Every other test on this branch seats a listener
    // by hand and skips the bind entirely, so reverting the ordering left the
    // whole suite green - the one defect this branch is about had no guard.
    //
    // Checked against the SOURCE rather than at runtime, deliberately.
    // `view_ipc::SOCK_PATH` is a set-once process global: a test that ran the
    // real bind would publish a temp path into every other test in this
    // binary, which is the hazard `CACHE_DIR_OVERRIDE_FOR_TEST` exists to
    // avoid for the cache dir. The repo already checks source-level properties
    // this way (`scripts/check_doc_ownership.py`). It is brittle to a rename,
    // and it fails loudly and says why, which is the trade being made.
    let src = include_str!("../mod.rs");
    let ctor = src
        .find("pub fn new(root: PathBuf)")
        .expect("App::new must exist for this guard to mean anything");
    // Bounded to App::new's own body. Slicing to the end of the file let two
    // plausible edits satisfy the guard while inverting the runtime order:
    // extracting the pane spawn into a helper defined BELOW App::new (the
    // marker's offset then jumps past the bind's), or a doc comment naming
    // `Self::bind_view_socket()` somewhere above a later spawn.
    let end = src[ctor..]
        .find("\n    pub fn ")
        .map(|i| ctor + i)
        .unwrap_or(src.len());
    let body = &src[ctor..end];
    // And each marker must be unique in the file, so a second occurrence
    // cannot silently be the one measured.
    for marker in ["Self::bind_view_socket()", "PtyTerminal::new(&root)"] {
        assert_eq!(
            src.matches(marker).count(),
            1,
            "{marker} must appear exactly once, or this guard measures whichever \
             copy comes first rather than the one that runs"
        );
    }
    let bind = body
        .find("Self::bind_view_socket()")
        .expect("App::new must bind the view socket");
    let pane = body
        .find("PtyTerminal::new(&root)")
        .expect("App::new must spawn the first pane");
    assert!(
        bind < pane,
        "the view socket must be bound BEFORE the first pane is spawned: \
         portable-pty snapshots the environment when a pane's CommandBuilder \
         is built, so a pane constructed first never learns the socket and \
         `croft view` fails in it while working in every pane opened later"
    );
}

#[test]
fn a_view_request_for_a_fifo_is_refused_rather_than_opened() {
    // `apply_view_request` refused only directories, so a FIFO passed the
    // check and reached `Editor::open`, whose `File::open` BLOCKS until a
    // writer appears. `mkfifo /tmp/f && croft view /tmp/f` from any pane froze
    // the whole editor permanently, recoverable only by SIGKILL, and
    // `/dev/zero` took the same route and read until OOM.
    //
    // The 20ms drain deadline does not help: it bounds the READ of the
    // request, and this is the step after it. Bounding one stage of a pipeline
    // says nothing about the next.
    //
    // Run on a thread with a join deadline: against the old behaviour this
    // hangs forever, and a hang reads as a CI timeout rather than as this
    // defect.
    let tmp = tempfile::tempdir().unwrap();
    let fifo = tmp.path().join("pipe");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(
        unsafe { libc::mkfifo(c.as_ptr(), 0o600) },
        0,
        "precondition: the fixture must actually create a FIFO"
    );
    assert!(
        fifo.exists(),
        "and `exists()` must say yes, as it did in production"
    );

    let root = tmp.path().to_path_buf();
    let target = fifo.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut app = App::new(root).unwrap();
        let _ = tx.send(app.apply_view_request(&target));
    });
    let reply = rx
        .recv_timeout(crate::test_budget::spawn_budget(
            crate::test_budget::tests::VIEW_FIFO_RECV_BASE,
        ))
        .expect("opening a FIFO must not block the frame loop forever");
    assert!(
        matches!(reply, crate::view_ipc::ViewReply::Err { .. }),
        "a FIFO is not a regular file and must be refused with a reason the \
         client prints, got {reply:?}"
    );
}

/// A copy is a new file: the original keeps its history, the copy starts
/// empty.
#[test]
fn copying_a_file_leaves_its_history_with_the_original() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    let (mut app, _hist) = app_with_history(&root);
    crate::history::record_in(&app.history_root, &root.join("a.txt"), b"a0\n", 1_000).unwrap();
    assert!(app.apply_paste_or_drop(
        &root.join("sub"),
        &[root.join("a.txt")],
        ExplorerClipMode::Copy
    ));
    assert!(root.join("sub").join("a.txt").exists());
    assert_eq!(
        crate::history::entries_in(&app.history_root, &root.join("a.txt")).len(),
        1
    );
    assert!(
        crate::history::entries_in(&app.history_root, &root.join("sub").join("a.txt")).is_empty()
    );
}

// ---- Explorer moves ask the servers first (#610) ----

#[test]
fn a_file_move_applies_the_servers_edits_before_renaming() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::write(root.join("util.py"), "def f(): pass\n").unwrap();
    std::fs::write(root.join("main.py"), "import util\n").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    let pending = PendingFileMove {
        request_id: 7,
        op: FileMove::Rename {
            parent: root.clone(),
            old: root.join("util.py"),
            new_name: String::from("helpers.py"),
        },
        renames: vec![crate::lsp::manager::FileRenameOp {
            old: root.join("util.py"),
            new: root.join("helpers.py"),
            is_dir: false,
        }],
        deadline: std::time::Instant::now(),
        baseline: Vec::new(),
    };
    let edits = vec![(
        root.join("main.py"),
        vec![crate::widgets::editor::TextSpanEdit {
            start: (0, 7),
            end: (0, 11),
            new_text: String::from("helpers"),
            utf16: false,
        }],
    )];
    app.finish_file_move(pending, edits);
    assert!(root.join("helpers.py").exists(), "the file moved");
    assert!(!root.join("util.py").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("main.py")).unwrap(),
        "import helpers\n",
        "the import followed it"
    );
    assert!(
        app.status.contains("references updated in 1 file"),
        "{}",
        app.status
    );
}

#[test]
fn a_pending_move_goes_ahead_at_its_deadline_without_an_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::write(root.join("a.txt"), "x").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.pending_file_move = Some(PendingFileMove {
        request_id: u64::MAX,
        op: FileMove::Rename {
            parent: root.clone(),
            old: root.join("a.txt"),
            new_name: String::from("b.txt"),
        },
        renames: Vec::new(),
        deadline: std::time::Instant::now() - std::time::Duration::from_millis(1),
        baseline: Vec::new(),
    });
    assert!(app.tick_file_moves());
    assert!(app.pending_file_move.is_none());
    assert!(
        root.join("b.txt").exists(),
        "a silent server cannot block the move"
    );
}

#[test]
fn a_probe_reports_whether_the_file_is_still_open() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "git-rebase-todo", "pick a b\n");
    let path = app.editor.path.clone().unwrap();
    assert_eq!(
        app.probe_view_path(&path),
        crate::view_ipc::ViewReply::Err {
            message: String::from(crate::view_ipc::PROBE_OPEN)
        }
    );
    app.run_command(crate::widgets::command_palette::Command::CloseEditor);
    assert!(matches!(
        app.probe_view_path(&path),
        crate::view_ipc::ViewReply::Err { .. }
    ));
}

#[test]
fn a_plain_view_request_has_no_probe_field_on_the_wire() {
    // An older croft must read this build's `croft view` unchanged.
    let json = serde_json::to_string(&crate::view_ipc::ViewRequest::new(std::path::Path::new(
        "/a",
    )))
    .unwrap();
    assert!(!json.contains("probe"), "{json}");
    let probe = serde_json::to_string(&crate::view_ipc::ViewRequest::probe(std::path::Path::new(
        "/a",
    )))
    .unwrap();
    assert!(probe.contains("\"probe\":true"));
}

#[test]
fn a_file_that_did_not_exist_yet_says_so() {
    let repo = scrub_repo();
    std::fs::write(repo.path().join("new.txt"), "fresh\n").unwrap();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.editor.open(&repo.path().join("new.txt")).unwrap();
    app.scrub_history();
    app.handle_scrubber_key(KeyCode::Left);
    let view = app
        .scrub_view
        .as_ref()
        .expect("a view even for a missing file");
    assert!(view.lines[0].contains("did not exist"), "{:?}", view.lines);
}

#[test]
fn a_missing_file_asks_to_locate_it_and_the_mapping_is_learned_and_kept() {
    // #577: VS Code's Locate… prompt, with the learned prefix remembered.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("lib")).unwrap();
        std::fs::write(tmp.path().join("src/a.rs"), "a1\na2\n").unwrap();
        std::fs::write(tmp.path().join("lib/b.rs"), "b1\nb2\nb3\n").unwrap();
        let log = tmp.path().join("ci.sarif");
        std::fs::write(
            &log,
            r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"ci"}},"results":[
                {"message":{"text":"in a"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"file:///home/runner/work/app/src/a.rs"},"region":{"startLine":2}}}]},
                {"message":{"text":"in b"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"file:///home/runner/work/app/lib/b.rs"},"region":{"startLine":3}}}]}
            ]}]}"#,
        )
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_preview(&log).unwrap();
        // The first result's file is not here: Enter asks where it is.
        app.handle_sarif_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(
                app.input_prompt.as_ref().map(|p| &p.purpose),
                Some(crate::widgets::input_prompt::InputPurpose::SarifLocate { .. })
            ),
            "a Locate prompt: {}",
            app.status
        );
        // Groups of equal size sort by path, so `lib/b.rs` is selected first.
        assert_eq!(
            app.editor
                .sarif
                .as_ref()
                .unwrap()
                .selected_entry()
                .map(|e| e.message.clone())
                .as_deref(),
            Some("in b")
        );
        // A file with another name is refused.
        app.submit_sarif_locate(&tmp.path().join("src/a.rs").display().to_string());
        assert!(app.status.contains("names must match"), "{}", app.status);
        // The right file opens, and the mapping is learned.
        app.submit_sarif_locate(&tmp.path().join("lib/b.rs").display().to_string());
        assert_eq!(
            app.editor.path.as_deref(),
            Some(tmp.path().join("lib/b.rs").as_path())
        );
        assert_eq!(app.editor.cursor_row, 2);
        // The other result now resolves on its own, in the next session too.
        let mut again = App::new(tmp.path().to_path_buf()).unwrap();
        again.editor.open_preview(&log).unwrap();
        let rows = again.editor.sarif.as_ref().unwrap().rows();
        let a_row = rows
            .iter()
            .position(|r| match r {
                crate::sarif::view::Row::Item { entry } => {
                    again.editor.sarif.as_ref().unwrap().entries[*entry].message == "in a"
                }
                _ => false,
            })
            .unwrap();
        again.editor.sarif.as_mut().unwrap().selected = a_row;
        again.handle_sarif_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            again.editor.path.as_deref(),
            Some(tmp.path().join("src/a.rs").as_path()),
            "resolved through the saved prefix: {}",
            again.status
        );
        assert_eq!(again.editor.cursor_row, 1);
    });
}

/// #365: a viewed-state read that began while a write was in flight may
/// have seen GitHub before the write landed. Its answer must not undo the
/// mark, even after the write finished and a refresh replaced the view.
#[test]
fn a_read_that_began_before_a_write_settled_does_not_undo_it() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let (gh, _log) = viewed_sync_gh(tmp.path(), "  *FileAsViewed*) echo '{}' ;;");
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.review_gh = gh.display().to_string();
        let pr = || {
            crate::pr_review::parse_pr(
                r#"{"number": 42, "url": "https://github.com/x/y/pull/42", "files": [{"path": "a.rs"}]}"#,
            )
            .unwrap()
        };
        app.open_pr_review(pr(), String::from("x/y#42"));
        app.editor.pr_review.as_mut().unwrap().github_id = Some(String::from("PR_kw42"));
        // Tick a.rs: its mutation is queued, then sent.
        app.handle_pr_review_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        app.poll_pr_viewed();
        assert!(app.pr_viewed_write.is_some(), "in flight");
        // A refresh replaces the view and starts a read while it is.
        app.open_pr_review(pr(), String::from("x/y#42"));
        let (tx, rx) = std::sync::mpsc::channel();
        app.pr_viewed_fetch = Some((String::from("x/y#42"), app.pr_viewed_seq, rx));
        // The write lands and settles before the read answers ...
        settle_viewed_writes(&mut app);
        assert!(app.pr_viewed_queue.is_empty() && app.pr_viewed_write.is_none());
        // ... with what GitHub said before it: unviewed.
        let mut states = std::collections::BTreeMap::new();
        states.insert(
            String::from("a.rs"),
            crate::pr_review::ViewedState::Unviewed,
        );
        tx.send(Ok((String::from("PR_kw42"), states))).unwrap();
        app.poll_pr_viewed();
        let view = app.editor.pr_review.as_ref().unwrap();
        assert!(view.viewed.contains("a.rs"), "the stale answer is ignored");
        assert!(
            crate::pr_review::ViewedStore::load(&App::pr_viewed_path())
                .viewed("x/y#42")
                .contains("a.rs"),
            "and does not reach the local store"
        );
        // A read that begins after the write settled is trusted again.
        let (tx, rx) = std::sync::mpsc::channel();
        app.pr_viewed_fetch = Some((String::from("x/y#42"), app.pr_viewed_seq, rx));
        let mut states = std::collections::BTreeMap::new();
        states.insert(
            String::from("a.rs"),
            crate::pr_review::ViewedState::Unviewed,
        );
        tx.send(Ok((String::from("PR_kw42"), states))).unwrap();
        app.poll_pr_viewed();
        assert!(
            !app.editor
                .pr_review
                .as_ref()
                .unwrap()
                .viewed
                .contains("a.rs")
        );
    });
}

#[test]
fn save_as_writes_the_buffer_to_a_new_path_and_moves_the_tab_there() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_plan_whose_folder_was_removed(tmp.path());
    let cmd = crate::widgets::command_palette::Command::from_id("save_as")
        .expect("there is a File: Save As… command");
    app.run_command(cmd);
    assert_eq!(
        app.prompt.as_ref().map(|p| p.buffer.as_str()),
        Some("notes/plan.md"),
        "the prompt starts from the tab's own path"
    );
    app.prompt = None;
    save_as(&mut app, "drafts/plan.txt");
    let saved = tmp.path().join("drafts/plan.txt");
    assert!(
        app.prompt.is_none(),
        "{:?}",
        app.prompt.as_ref().map(|p| &p.error)
    );
    assert_eq!(
        std::fs::read_to_string(&saved).unwrap(),
        "# Plan\n\n- step one\n- step two\n"
    );
    assert_eq!(app.editor.path.as_deref(), Some(saved.as_path()));
    assert!(!app.editor.dirty);
    assert!(
        app.editor.language().is_none(),
        "a .txt file has no grammar"
    );
    assert!(
        !tmp.path().join("notes").exists(),
        "the old path is not recreated"
    );
    // The tab now belongs to the new file: the next save goes there.
    app.editor.insert_str("!");
    app.save();
    assert!(!app.editor.dirty, "{}", app.status);
    assert!(
        std::fs::read_to_string(&saved)
            .unwrap()
            .contains("- step two!")
    );
}

#[test]
fn save_as_does_not_overwrite_another_file() {
    // Negative: an existing file at the answer is left as it was, and the
    // tab keeps its own path and its unsaved edits.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_plan_whose_folder_was_removed(tmp.path());
    std::fs::write(tmp.path().join("README.md"), "# app\n").unwrap();
    save_as(&mut app, "README.md");
    let error = app.prompt.as_ref().and_then(|p| p.error.clone());
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("already exists")),
        "{error:?}"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("README.md")).unwrap(),
        "# app\n"
    );
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("notes/plan.md").as_path())
    );
    assert!(app.editor.dirty);
    // Nor does a blank answer go anywhere.
    save_as(&mut app, "  ");
    assert!(app.prompt.as_ref().is_some_and(|p| p.error.is_some()));
    assert!(app.editor.dirty);
}

#[test]
fn a_save_as_that_fails_leaves_the_tab_where_it_was() {
    // Negative: a path that can't be written keeps the tab on its own path
    // with its edits, so nothing is lost and Save As can be tried again.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_plan_whose_folder_was_removed(tmp.path());
    std::fs::write(tmp.path().join("blocker"), "").unwrap();
    save_as(&mut app, "blocker/plan.md");
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("notes/plan.md").as_path())
    );
    assert!(app.editor.dirty);
    let error = app.prompt.as_ref().and_then(|p| p.error.clone());
    assert!(error.is_some(), "the prompt stays open with the reason");
}

/// #852: `Ctrl+Alt+S` is File: Save All: every dirty tab reaches disk, not
/// only the active one, and a second press says there is nothing left.
#[test]
fn save_all_writes_every_dirty_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "aaa").unwrap();
    std::fs::write(&b, "bbb").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // App::new loads the developer's real prefs: pin every mode that would
    // save on its own, or defer the write, off.
    app.auto_save = false;
    app.auto_save_on_focus_change = false;
    app.format_on_save = false;
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('X'), KeyModifiers::NONE))
        .unwrap();
    app.editor.open_pinned(&b).unwrap();
    app.handle_key(key(KeyCode::Char('Y'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.unsaved_count(), 2);
    app.handle_key(key(
        KeyCode::Char('s'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_eq!(app.unsaved_count(), 0, "{}", app.status);
    assert!(std::fs::read_to_string(&a).unwrap().contains('X'));
    assert!(std::fs::read_to_string(&b).unwrap().contains('Y'));
    assert_eq!(app.status, "Saved 2 editors");
    app.run_command(crate::widgets::command_palette::Command::SaveAll);
    assert_eq!(app.status, "Save All: nothing to save");
    // Shift is Convert Indentation to Spaces; plain Ctrl+S is Save.
    assert!(!is_save_all_key(key(
        KeyCode::Char('S'),
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT
    )));
    assert!(!is_save_all_key(key(
        KeyCode::Char('s'),
        KeyModifiers::CONTROL
    )));
}

/// #852 negative: Save All writes only what auto save may write blind. An
/// inactive tab whose file changed on disk keeps the external text, stays
/// dirty and is reported, while the other dirty tab still saves.
#[test]
fn save_all_never_overwrites_an_external_change_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "aaa").unwrap();
    std::fs::write(&b, "bbb").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.auto_save = false;
    app.auto_save_on_focus_change = false;
    app.format_on_save = false;
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('X'), KeyModifiers::NONE))
        .unwrap();
    std::fs::write(&a, "external change wins\n").unwrap();
    app.editor.open_pinned(&b).unwrap();
    app.handle_key(key(KeyCode::Char('Y'), KeyModifiers::NONE))
        .unwrap();
    app.run_command(crate::widgets::command_palette::Command::SaveAll);
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "external change wins\n",
        "Save All must not write over a file changed on disk"
    );
    assert!(std::fs::read_to_string(&b).unwrap().contains('Y'));
    assert_eq!(app.unsaved_count(), 1, "the conflicted tab stays dirty");
    assert!(
        app.status.contains("1 editor still unsaved"),
        "the leftover tab is reported, not a clean \"Saved\": {:?}",
        app.status
    );
}

/// #852 (comment 1): the OPEN EDITORS header carries VS Code's Save All
/// action; clicking it writes every dirty tab and leaves the section as it
/// was (the click is the button's, not the header's collapse toggle).
#[test]
fn clicking_save_all_on_the_open_editors_header_saves_every_dirty_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "aaa").unwrap();
    std::fs::write(&b, "bbb").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.auto_save = false;
    app.auto_save_on_focus_change = false;
    app.format_on_save = false;
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('X'), KeyModifiers::NONE))
        .unwrap();
    app.editor.open_pinned(&b).unwrap();
    app.handle_key(key(KeyCode::Char('Y'), KeyModifiers::NONE))
        .unwrap();
    app.explorer_views = Default::default();
    app.explorer_views.toggle(ExplorerView::OpenEditors);
    app.open_editors.collapsed = false;
    app.sidebar_view = SidebarView::Explorer;
    app.show_tree = true;
    let buf = render_buffer_852(&mut app, 120, 40);
    let hdr = app.open_editors.last_area;
    assert!(hdr.height > 0, "OPEN EDITORS is on screen");
    let save_all_x = (hdr.x..hdr.x + hdr.width)
        .find(|&x| buf[(x, hdr.y)].symbol() == "\u{eb49}")
        .expect("the OPEN EDITORS header paints a Save All (codicon save-all) action");
    left_click(&mut app, save_all_x, hdr.y);
    assert!(
        std::fs::read_to_string(&a).unwrap().contains('X'),
        "Save All writes the inactive tab"
    );
    assert!(std::fs::read_to_string(&b).unwrap().contains('Y'));
    assert!(
        !app.open_editors.collapsed,
        "the button's click does not also collapse the section"
    );
}

/// #852: a file dirty in two splits is one unsaved file, as the Explorer's
/// unsaved badge counts it, so Save All does not report it twice.
#[test]
fn save_all_counts_a_file_dirty_in_two_splits_once() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    std::fs::write(&a, "aaa").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.auto_save = false;
    app.auto_save_on_focus_change = false;
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    app.split_editor();
    app.handle_key(key(KeyCode::Char('X'), KeyModifiers::NONE))
        .unwrap();
    app.focus_editor_group(true);
    app.handle_key(key(KeyCode::Char('Y'), KeyModifiers::NONE))
        .unwrap();
    let dirty_buffers = app
        .editor
        .editors
        .iter()
        .chain(
            app.editor_layout
                .inactive_groups()
                .into_iter()
                .flat_map(|g| g.editors.iter()),
        )
        .filter(|e| e.dirty && e.path.as_deref() == Some(a.as_path()))
        .count();
    assert_eq!(dirty_buffers, 2, "fixture: a.txt is dirty in both splits");
    assert_eq!(app.unsaved_count(), 1, "one file, counted once");
}

/// #852: a file with a symbol tab is one file on disk; Save All writes it
/// once and says "Saved 1 editor", not 2.
#[test]
fn save_all_reports_a_file_with_a_symbol_tab_as_one_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.auto_save = false;
    app.auto_save_on_focus_change = false;
    app.format_on_save = false;
    app.sync_symbol_views();
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('7'), KeyModifiers::NONE))
        .unwrap();
    app.sync_symbol_views();
    assert!(
        app.editor.editors.iter().all(|e| e.dirty),
        "fixture: both tabs hold the edit"
    );
    app.run_command(crate::widgets::command_palette::Command::SaveAll);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn a() {\n    1\n}\nfn b() {\n    27\n}"
    );
    assert_eq!(app.status, "Saved 1 editor");
}

/// #1491: Save All said "Saved 1 editor" while the file still waited on
/// its formatter. It now says the file is being formatted.
#[test]
fn save_all_does_not_report_a_file_waiting_on_its_formatter_as_saved() {
    assert_eq!(
        save_all_summary(1, 0, Some("m.py"), String::new()),
        "Formatting m.py before saving it"
    );
    assert_eq!(
        save_all_summary(3, 0, Some("m.py"), String::new()),
        "Saved 2 editors; formatting m.py before saving it"
    );
    // Negative: with nothing waiting, the count stands as before.
    assert_eq!(
        save_all_summary(2, 0, None, String::new()),
        "Saved 2 editors"
    );
    assert_eq!(
        save_all_summary(1, 0, None, String::new()),
        "Saved 1 editor"
    );
}

/// #1568: opening the file again from the Explorer (a new tab, not Reopen
/// Closed Editor) finds the history too.
#[test]
fn opening_a_closed_file_again_brings_back_the_undo_history() {
    let (_tmp, a, mut app) = app_with_a_saved_edit();
    app.run_command(crate::widgets::command_palette::Command::CloseEditor);
    app.editor.open_preview(&a).unwrap();
    assert!(app.editor.undo());
    assert_eq!(app.editor.lines[0], "value = 1");
}

/// Negative (#1643): only a SQLite tab's stamp covers a `-wal` file. A plain
/// text file whose `<name>-wal` sibling changes is not changed on disk, and
/// the sweep neither reloads it nor flags a conflict.
#[test]
fn a_text_files_wal_named_sibling_is_not_part_of_its_stamp() {
    let tmp = tempfile::tempdir().unwrap();
    let txt = tmp.path().join("notes.txt");
    std::fs::write(&txt, "x\n").unwrap();
    let sibling = tmp.path().join("notes.txt-wal");
    std::fs::write(&sibling, "one").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&txt).unwrap();
    app.editor.insert_char('y');
    std::fs::write(&sibling, "a longer second write").unwrap();
    assert!(!app.editor.disk_changed_externally());
    assert!(!app.reload_open_file_after_external_change());
    assert!(!app.editor.disk_conflict);
    assert_eq!(app.editor.lines[0], "yx");
}

/// Serve one `croft edit` request for `path` at `at` through a socket pair.
fn serve_view_request_1606(
    app: &mut App,
    path: &std::path::Path,
    at: Option<(usize, usize)>,
) -> crate::view_ipc::ViewReply {
    let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut line = serde_json::to_vec(&crate::view_ipc::ViewRequest::at(path, at)).unwrap();
    line.push(b'\n');
    std::io::Write::write_all(&mut client, &line).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    app.answer_view_client(server, deadline).0
}

/// #1606: `croft edit big.txt:150:3` from a pane opens big.txt with the
/// caret on line 150, column 3, scrolled into view.
#[test]
fn a_view_request_with_a_location_puts_the_caret_there() {
    let tmp = tempfile::tempdir().unwrap();
    let big = tmp.path().join("big.txt");
    let text: String = (1..=300).map(|n| format!("line {n}\n")).collect();
    std::fs::write(&big, text).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let reply = serve_view_request_1606(&mut app, &big, Some((149, 2)));
    assert_eq!(reply, crate::view_ipc::ViewReply::Ok);
    assert_eq!(app.editor.path.as_deref(), Some(big.as_path()));
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (149, 2));
    assert!(app.editor.scroll > 0 && app.editor.scroll <= 149);
    assert!(app.focus == Pane::Editor);
}

/// Negative (#1606): a location past the end clamps to the text, and a
/// request without one leaves the caret at the top.
#[test]
fn a_view_request_location_clamps_and_none_leaves_the_caret_at_the_top() {
    let tmp = tempfile::tempdir().unwrap();
    let small = tmp.path().join("small.txt");
    std::fs::write(&small, "one\ntwo\n").unwrap();
    let other = tmp.path().join("other.txt");
    std::fs::write(&other, "alpha\nbeta\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    serve_view_request_1606(&mut app, &small, Some((99, 99)));
    let last = app.editor.lines.len() - 1;
    assert_eq!(app.editor.cursor_row, last);
    assert!(app.editor.cursor_col <= app.editor.lines[last].chars().count());
    serve_view_request_1606(&mut app, &other, None);
    assert_eq!(app.editor.path.as_deref(), Some(other.as_path()));
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 0));
}

/// Issue #1622: auto save "onWindowChange". The terminal window croft runs
/// in losing focus (alt-tab, another tmux window) writes every dirty buffer,
/// the one being typed into included: the editor still has croft's own
/// focus, but the whole window lost the user's.
#[test]
fn the_window_losing_focus_saves_the_buffer_being_edited() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.auto_save = false;
    app.auto_save_on_focus_change = true;
    app.focus_pane(Pane::Editor);
    app.tick_auto_save();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.dirty);
    assert!(!app.tick_auto_save(), "focus inside croft has not moved");
    assert!(app.on_window_focus_lost(), "the window lost focus");
    assert!(!app.editor.dirty, "the active buffer is saved");
    let on_disk = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert!(on_disk.contains('x'), "the edit is on disk: {on_disk:?}");
}

/// Every open buffer is written, not only the active one.
#[test]
fn the_window_losing_focus_saves_every_dirty_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "aaa").unwrap();
    std::fs::write(&b, "bbb").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.auto_save = false;
    app.auto_save_on_focus_change = false;
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('A'), KeyModifiers::NONE))
        .unwrap();
    app.editor.open_pinned(&b).unwrap();
    app.handle_key(key(KeyCode::Char('B'), KeyModifiers::NONE))
        .unwrap();
    app.tick_auto_save();
    app.auto_save_on_focus_change = true;
    assert!(app.on_window_focus_lost());
    let a_disk = std::fs::read_to_string(&a).unwrap();
    let b_disk = std::fs::read_to_string(&b).unwrap();
    assert!(
        a_disk.contains('A'),
        "the background tab is saved: {a_disk:?}"
    );
    assert!(b_disk.contains('B'), "the active tab is saved: {b_disk:?}");
}

/// Negative: with the focus-change mode off, the window losing focus
/// writes nothing, even with afterDelay on and a buffer old enough.
#[test]
fn the_window_losing_focus_saves_nothing_when_the_mode_is_off() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.auto_save = true;
    app.auto_save_on_focus_change = false;
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    age_last_edit(&mut app.editor);
    assert!(!app.on_window_focus_lost(), "the mode is off");
    assert!(app.editor.dirty, "the buffer stays dirty");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "hello"
    );
}

/// Negative: a clean buffer is not rewritten when the window loses focus.
#[test]
fn the_window_losing_focus_leaves_clean_buffers_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.auto_save = false;
    app.auto_save_on_focus_change = true;
    app.focus_pane(Pane::Editor);
    let before = std::fs::metadata(tmp.path().join("a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(!app.on_window_focus_lost(), "nothing to save");
    let after = std::fs::metadata(tmp.path().join("a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after, "a clean file is not rewritten");
}
