use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

fn dummy_activity_images() -> ActivityBarImages {
    let s = || String::from("\x1b]1337;File=inline=1:Zm9v\x07");
    ActivityBarImages {
        explorer_active: s(),
        explorer_inactive: s(),
        search_active: s(),
        search_inactive: s(),
        source_control_active: s(),
        source_control_inactive: s(),
        remote_active: s(),
        remote_inactive: s(),
        run_debug_active: s(),
        run_debug_inactive: s(),
        settings_active: s(),
        settings_inactive: s(),
    }
}

#[test]
fn opening_a_search_hit_moves_focus_to_the_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hit.txt");
    std::fs::write(&f, "needle here\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("needle");
    app.search.run_query();
    assert!(
        matches!(app.focus, Pane::Tree),
        "precondition: search holds focus"
    );
    let hit = app.search.selected_hit().cloned().expect("a hit");
    app.open_search_hit(&hit);
    assert!(
        matches!(app.focus, Pane::Editor),
        "opening a search result must hand keyboard focus to the editor so typing no longer leaks into the search box"
    );
    assert!(!app.search.focused);
}

#[test]
fn opening_a_search_hit_marks_the_active_match_so_the_renderer_paints_it_orange() {
    // Global search (Cmd+Shift+F) result navigation must mark the occurrence
    // it lands on as the *active* match, the same way the in-file Cmd+F path
    // does, so the editor paints that one occurrence orange while the sibling
    // matches stay yellow. Without active_search_match every match renders the
    // same uniform yellow and the user can't tell which result they are on.
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hit.rs");
    // Line 2 (index 1) has two "alpha" matches: "let " is 4 chars, so the
    // first "alpha" starts at char col 4, and the second is later on the line.
    std::fs::write(&f, "fn main() {}\nlet alpha = alpha + 1;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("alpha");
    app.search.run_query();
    let hit = app.search.selected_hit().cloned().expect("a hit");
    app.open_search_hit(&hit);
    assert_eq!(
        app.editor.active_search_match,
        Some((1, 4, 5)),
        "opening a global-search hit must record the first match on the line as the active match (row 1, col 4, len 5) so the renderer paints exactly those cells orange; the second 'alpha' stays yellow"
    );
}

#[test]
fn go_to_definition_opens_the_target_file_and_moves_the_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("target.rs");
    std::fs::write(&target, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.go_to_definition(target.clone(), 2, 3);
    assert_eq!(
        app.editor.path.as_deref(),
        Some(target.as_path()),
        "the definition's file is opened in the editor"
    );
    assert_eq!(
        app.editor.cursor_row, 2,
        "caret lands on the definition line"
    );
    assert_eq!(
        app.editor.cursor_col, 3,
        "caret lands at the definition column"
    );
    assert!(
        matches!(app.focus, Pane::Editor),
        "focus moves to the editor"
    );
}

#[test]
fn go_back_returns_to_the_location_before_a_definition_jump() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "fn main() {}\nlet x = helper();\n").unwrap();
    std::fs::write(&b, "fn helper() {}\nfn other() {}\nfn third() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 8;

    app.go_to_definition(b.clone(), 0, 3);
    assert_eq!(
        app.editor.path.as_deref(),
        Some(b.as_path()),
        "the definition jump opens B"
    );
    assert_eq!(
        app.editor.cursor_row, 0,
        "caret lands on the definition line"
    );

    app.nav_back();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(a.as_path()),
        "go back returns to file A"
    );
    assert_eq!(
        app.editor.cursor_row, 1,
        "caret returns to the line we jumped from"
    );
    assert_eq!(
        app.editor.cursor_col, 8,
        "caret returns to the column we jumped from"
    );
}

#[test]
fn go_back_with_empty_history_stays_put() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    std::fs::write(&a, "fn main() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.editor.cursor_row = 0;
    app.nav_back();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(a.as_path()),
        "with nothing to go back to, go back is a no-op"
    );
}

#[test]
fn cmd_shift_s_jumps_to_source_control_from_explorer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(
        KeyCode::Char('S'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.sidebar_view,
        SidebarView::SourceControl,
        "Cmd+Shift+S must jump to Source Control, not Search"
    );
    assert!(
        matches!(app.focus, Pane::Tree),
        "Cmd+Shift+S focuses the Source Control panel"
    );
}

#[test]
fn typing_after_reactivating_search_replaces_instead_of_appending() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("oldterm");
    app.search.clear_selection();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(
        KeyCode::Char('F'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.search.query, "x",
        "the previous query was selected, so the first keystroke replaces it"
    );
}

#[test]
fn paste_while_terminal_focused_does_not_leak_into_search_box() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("hello");
    app.search.clear_selection();
    app.focus_pane(Pane::Terminal);
    app.handle_paste("machi");
    assert_eq!(
        app.search.query, "hello",
        "a bracketed paste must NOT append to the search box when the terminal is focused, even though the Search panel is visible"
    );
    assert!(
        matches!(app.focus, Pane::Terminal),
        "focus must stay on the terminal"
    );
}

#[test]
fn paste_while_terminal_focused_does_not_leak_into_editor_find_bar() {
    // Sibling of paste_while_terminal_focused_does_not_leak_into_search_box,
    // but for the in-file Find bar (Cmd+F). The find bar is pane-scoped, not a
    // modal: once the user clicks into the terminal, a paste meant for the
    // shell must reach the terminal, not get swallowed by the still-open find
    // query. Focus is the deciding factor, not whether editor_find is Some.
    let mut app = editor_app_with_lines(&["alpha beta", "alpha gamma"]);
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.editor_find.is_some(),
        "precondition: Cmd+F opens the inline editor Find bar"
    );
    let query_before = app
        .editor_find
        .as_ref()
        .map(|s| s.query.clone())
        .unwrap_or_default();
    app.focus_pane(Pane::Terminal);
    app.handle_paste("ls -la");
    assert_eq!(
        app.editor_find.as_ref().map(|s| s.query.clone()),
        Some(query_before),
        "a paste must NOT append to the editor Find query when the terminal is focused, even though the find bar is still open"
    );
    assert!(
        matches!(app.focus, Pane::Terminal),
        "focus must stay on the terminal"
    );
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
    let started = std::time::Instant::now();
    let mut saw = false;
    let mut saw_tree = false;
    for _ in 0..150 {
        if app.drain_fs_events() {
            saw = true;
        }
        if app.tree.nodes.iter().any(|n| n.path == new_file) {
            saw_tree = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(saw, "workspace write should propagate as a dirty signal");
    assert!(
        saw_tree,
        "workspace write should refresh the tree with the created file"
    );
    assert!(
        started.elapsed() <= std::time::Duration::from_millis(200),
        "created file should appear in Explorer within 200ms"
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
    let started = std::time::Instant::now();
    let mut saw_tree = false;
    for _ in 0..150 {
        let _ = app.drain_fs_events();
        if !app.tree.nodes.iter().any(|n| n.path == doomed) {
            saw_tree = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(saw_tree, "deleted file should disappear from the tree");
    assert!(
        started.elapsed() <= std::time::Duration::from_millis(200),
        "deleted file should disappear from Explorer within 200ms"
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
fn rect_contains_basic() {
    let r = Rect {
        x: 5,
        y: 5,
        width: 10,
        height: 10,
    };
    assert!(rect_contains(r, 5, 5));
    assert!(rect_contains(r, 14, 14));
    assert!(!rect_contains(r, 4, 5));
    assert!(!rect_contains(r, 15, 5));
    assert!(!rect_contains(r, 5, 15));
}

#[test]
fn rect_contains_zero_sized_is_empty() {
    let r = Rect {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };
    assert!(!rect_contains(r, 0, 0));
}

fn commit_fixture(subject: &str, when: &str) -> crate::git::CommitInfo {
    crate::git::CommitInfo {
        hash: "abc1234".to_string(),
        full_hash: "abc1234deadbeef".to_string(),
        when: when.to_string(),
        subject: subject.to_string(),
    }
}

#[test]
fn welcome_commit_wrapping_reserves_timestamp_column() {
    let c = commit_fixture(
        "feat(search): clickable paste button in input row reads pbpaste and keeps going",
        "3 hours ago",
    );
    let (first, rest) = welcome_commit_widths(&c, 42);
    assert!(
        first < rest,
        "first line should reserve room for the timestamp"
    );
    let lines = wrapped_welcome_commit_subject(&c, 42);
    assert!(lines.len() > 1, "long commit subjects should wrap");
    assert!(lines[0].chars().count() <= first as usize);
    assert!(lines[1].chars().count() <= rest as usize);
}

#[test]
fn welcome_recents_height_accounts_for_wrapped_commit_rows() {
    let c = commit_fixture(
        "feat(search): clickable paste button in input row reads pbpaste and keeps going",
        "3 hours ago",
    );
    let compact = welcome_recents_height(Some("https://bitbucket.org/a/b"), &[c], 42);
    assert!(
        compact > 1 + 1 + 1,
        "height must include wrapped commit continuation rows"
    );
}

#[test]
fn welcome_provider_badge_uses_repo_provider() {
    assert!(welcome_provider_badge("https://bitbucket.org/a/b").contains("Bitbucket"));
    assert!(welcome_provider_badge("https://github.com/a/b").contains("GitHub"));
}

/// Codeberg's badge must show the literal name without a Nerd Font glyph
/// in front of it: most installed Nerd Fonts (including the one croft
/// ships with via `setup-iterm2`) do not have a Codeberg codepoint, so
/// the previous `\u{ea60}` placeholder rendered as a stray symbol or
/// tofu box. The actual Codeberg logo is composited as an OSC-1337
/// image overlay at the badge cell when iTerm2 is detected (handled
/// elsewhere); the text path stays glyph-free so non-iTerm2 terminals
/// also display correctly.
#[test]
fn welcome_provider_badge_for_codeberg_has_no_unicode_glyph() {
    let badge = welcome_provider_badge("ssh://git@codeberg.org/vitali87/croft.git");
    assert!(badge.contains("Codeberg"), "badge: {badge:?}");
    assert!(
        badge.chars().all(|c| c.is_ascii() || c == ' '),
        "badge must be glyph-free for Codeberg until/unless we render the icon as an image overlay; current badge: {badge:?}"
    );
}

/// The Codeberg icon is overlaid as a 2-cell-wide OSC-1337 image at the
/// badge's anchor; the text after it must start at least one cell later
/// so the logo doesn't visually butt against the "C". That means three
/// leading spaces total: two reserved for the icon, one for the gap.
#[test]
fn welcome_provider_badge_for_codeberg_leaves_a_gap_between_icon_and_text() {
    let badge = welcome_provider_badge("https://codeberg.org/vitali87/croft");
    assert!(
        badge.starts_with("   Codeberg"),
        "badge must start with three spaces (2 for the icon overlay + 1 visual gap) followed by 'Codeberg'; got: {badge:?}"
    );
}

#[test]
fn lerp_rgb_hits_endpoints_exactly() {
    let a = (10u8, 20, 30);
    let b = (100u8, 200, 250);
    assert_eq!(lerp_rgb(a, b, 0.0), a);
    assert_eq!(lerp_rgb(a, b, 1.0), b);
    let mid = lerp_rgb(a, b, 0.5);
    assert!(mid.0 > a.0 && mid.0 < b.0);
}

#[test]
fn paint_gradient_box_skips_when_rect_runs_off_buffer() {
    // The panic that motivated the bounds clip: an 80x25 buffer asked
    // to render a tall recents box that extended past row 25.
    let buf_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    let mut buf = ratatui::buffer::Buffer::empty(buf_area);
    let oversized = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 60,
    };
    // Must not panic; the no-op clip is the contract.
    paint_gradient_box(&mut buf, oversized);
    let off_top_left = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    // Sanity: a fitting rect still draws.
    paint_gradient_box(&mut buf, off_top_left);
    assert_eq!(buf[(0, 0)].symbol(), "\u{256d}");
}

#[test]
fn clicking_an_scm_entry_below_a_stale_tree_last_area_still_selects_and_opens() {
    // User report: after the Source Control list got long enough to
    // reach below where the file tree had last rendered, the rows in
    // that lower band became unclickable. Root cause: the mouse
    // handler gated all sidebar-view click branches on `rect_contains
    // (self.tree.last_area, ...)`, but `tree.last_area` is only
    // refreshed when the Explorer view renders — switching to
    // Source Control freezes that rectangle, and any click below
    // its bottom edge fell through even though it was inside the
    // SCM panel.
    use crate::git::{ChangeEntry, ChangeKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Pretend the Explorer view rendered into a tall pane; then the
    // user opened the terminal which shrank the side area; then they
    // switched to Source Control. Set tree.last_area to the OLD,
    // taller rectangle so we can prove the SCM click-handler is no
    // longer dependent on it.
    app.tree.last_area = Rect {
        x: 4,
        y: 1,
        width: 30,
        height: 6,
    };
    app.set_sidebar_view(SidebarView::SourceControl);
    // Set entries AFTER set_sidebar_view because that path calls
    // refresh_source_control which overwrites entries with the live
    // git query (empty in a non-repo temp dir).
    app.source_control.status.in_repo = true;
    app.source_control.status.branch = Some("main".into());
    app.source_control.entries = (0..15)
        .map(|i| ChangeEntry {
            path: format!("file{i}.py"),
            kind: ChangeKind::Untracked,
            ..Default::default()
        })
        .collect();

    let backend = ratatui::backend::TestBackend::new(80, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    // Pick the y of a row that lives BELOW tree.last_area's bottom
    // but inside the SCM list. Walk the rendered list area to find
    // one.
    let stale_bottom = app.tree.last_area.y + app.tree.last_area.height;
    let list_area = app.source_control.last_list_area;
    assert!(
        list_area.y + list_area.height > stale_bottom,
        "test setup must put SCM rows below the stale tree.last_area"
    );
    // Walk from stale_bottom down looking for a row that maps to an
    // entry (skip section-header rows).
    let target_row = (stale_bottom..list_area.y + list_area.height)
        .find(|y| app.source_control.entry_at_y(*y).is_some())
        .expect("at least one entry must render below the stale tree band");

    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: list_area.x + 4,
        row: target_row,
        modifiers: KeyModifiers::NONE,
    });

    assert!(
        app.source_control.selected_change.is_some(),
        "click on an SCM entry below the stale tree.last_area must still select it"
    );
}

#[test]
fn click_on_a_deleted_source_control_entry_opens_unified_diff_with_head_lines_all_removed() {
    // A file that's tracked in HEAD but missing from the working
    // tree (or staged for deletion) used to leave the editor empty
    // when clicked, because `open_source_control_entry` fell into
    // the generic open-from-disk branch and the path doesn't exist
    // any more. New behaviour: clicking the row opens a one-pane
    // unified diff that paints every line of the HEAD blob as a
    // removed line — visually identical to `git diff` for a deletion.
    use crate::git::{ChangeEntry, ChangeKind};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(init.success());
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "t@t"])
        .current_dir(root)
        .status();
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(root)
        .status();
    let f = root.join("doomed.txt");
    std::fs::write(&f, b"alpha\nbeta\ngamma\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["add", "doomed.txt"])
        .current_dir(root)
        .status();
    let _ = std::process::Command::new("git")
        .args(["commit", "-q", "-m", "seed"])
        .current_dir(root)
        .status();
    // Delete in the working tree but leave it tracked in HEAD/index
    // — that's exactly the `Deleted` (unstaged) state.
    std::fs::remove_file(&f).unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.source_control.entries = vec![ChangeEntry {
        path: "doomed.txt".into(),
        kind: ChangeKind::Deleted,
        ..Default::default()
    }];
    app.set_sidebar_view(SidebarView::SourceControl);
    app.open_source_control_entry(0);
    let diff = app
        .editor
        .diff
        .as_ref()
        .expect("clicking a Deleted entry must open a diff view, not a blank tab");
    assert!(
        diff.unified,
        "the deletion view must be the single-column unified flavour, not side-by-side"
    );
    assert_eq!(diff.left_lines, vec!["alpha", "beta", "gamma"]);
    assert!(
        diff.rows
            .iter()
            .all(|r| matches!(r, crate::widgets::diff::DiffRow::Removed { .. })),
        "every visible row must be Removed so the whole pane paints red"
    );
}

#[test]
fn click_on_a_modified_source_control_entry_opens_a_diff_view_against_head() {
    // VS Code surfaces the diff between the working-tree file and
    // the HEAD version when the user clicks a Modified entry in the
    // Source Control panel. Croft used to just open the working
    // file as plain text — assert the new behaviour: the active
    // editor tab carries a `diff: Some(...)` payload.
    use crate::git::{ChangeEntry, ChangeKind};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Init a real repo so git show HEAD:<path> returns content.
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(init.success());
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "t@t"])
        .current_dir(root)
        .status();
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(root)
        .status();
    let f = root.join("a.py");
    std::fs::write(&f, b"print(1)\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["add", "a.py"])
        .current_dir(root)
        .status();
    let _ = std::process::Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(root)
        .status();
    std::fs::write(&f, b"print(2)\n").unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.source_control.entries = vec![ChangeEntry {
        path: "a.py".into(),
        kind: ChangeKind::Modified,
        ..Default::default()
    }];
    app.set_sidebar_view(SidebarView::SourceControl);
    app.open_source_control_entry(0);
    assert!(
        app.editor.diff.is_some(),
        "clicking a Modified entry must open the editor in diff-vs-HEAD mode, not as plain text"
    );
}

#[test]
fn flush_welcome_codeberg_badge_overlay_tracks_last_emitted_cell_across_resizes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Stub a baked OSC so the flush has something to "emit". The
    // bytes never reach a real terminal in tests — we're checking
    // the bookkeeping, not the wire format.
    app.overlays
        .badge
        .set_image("\x1b]1337;File=...\x07".to_string());

    // Initial render of welcome panel: badge anchored at (10, 5).
    app.overlays.badge.set_target(Some((10, 5)));
    app.flush_welcome_codeberg_badge_overlay();
    assert_eq!(
        app.overlays.badge.last_emitted(),
        Some((10, 5)),
        "after the first flush the last-emitted cell must match the target cell"
    );

    // Simulate a window resize that shifts the welcome panel right.
    app.overlays.badge.set_target(Some((20, 8)));
    app.flush_welcome_codeberg_badge_overlay();
    assert_eq!(
        app.overlays.badge.last_emitted(),
        Some((20, 8)),
        "after a resize the last-emitted cell must update to the new target so the next resize knows where to wipe"
    );

    // Repeated flush at the same cell is a no-op for tracking.
    app.flush_welcome_codeberg_badge_overlay();
    assert_eq!(
        app.overlays.badge.last_emitted(),
        Some((20, 8)),
        "re-emitting at the same cell must not change tracking"
    );

    // User opens a file: welcome panel hides, badge cell cleared,
    // and the next flush must drop the last-emitted record so the
    // image cells are wiped and not re-emitted.
    let file = tmp.path().join("README.md");
    std::fs::write(&file, "hi").unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.overlays.badge.set_target(None);
    app.flush_welcome_codeberg_badge_overlay();
    assert_eq!(
        app.overlays.badge.last_emitted(),
        None,
        "leaving the welcome panel must clear the last-emitted record so a future welcome render starts fresh"
    );
}

#[test]
fn render_welcome_clears_codeberg_badge_cell_when_recents_box_collapses() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome
        .set_test_remote(Some("https://codeberg.org/vitali87/croft".to_string()));
    app.overlays
        .badge
        .set_image("\x1b]1337;File=...\x07".to_string());

    let tall = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let backend = ratatui::backend::TestBackend::new(tall.width, tall.height);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_welcome(f, tall)).unwrap();
    assert!(
        app.overlays.badge.target().is_some(),
        "with a tall welcome area the recents box must fit and the badge cell must be set so the flush emits the OSC-1337 image"
    );

    let tight = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 10,
    };
    let backend = ratatui::backend::TestBackend::new(tight.width, tight.height);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_welcome(f, tight)).unwrap();
    assert_eq!(
        app.overlays.badge.target(),
        None,
        "when the recents box can't fit, the badge cell must reset to None so the post-draw flush wipes the stale image instead of re-emitting at the prior anchor"
    );
}

/// The user dislikes underlined links in the welcome panel; the blue
/// foreground colour already signals "clickable", and the underline
/// adds visual noise (especially under the Codeberg logo overlay).
/// Assert the rendered buffer contains zero cells with the UNDERLINED
/// modifier so this preference is enforced by the test suite.
#[test]
fn welcome_panel_renders_without_any_underlined_cells() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome
        .set_test_remote(Some("https://codeberg.org/vitali87/croft".to_string()));
    app.welcome.set_test_commits(vec![crate::git::CommitInfo {
        hash: "abc1234".to_string(),
        full_hash: "abc1234fffffffffffffffffffffffffffffffffff".to_string(),
        when: "1 hour ago".to_string(),
        subject: "feat: do thing".to_string(),
    }]);
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let backend = ratatui::backend::TestBackend::new(area.width, area.height);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_welcome(f, area)).unwrap();
    let buffer = term.backend().buffer();
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            assert!(
                !cell.modifier.contains(Modifier::UNDERLINED),
                "cell at ({x}, {y}) symbol={:?} has UNDERLINED set; the welcome panel must render no underlines",
                cell.symbol()
            );
        }
    }
}

#[test]
fn welcome_panel_does_not_paint_the_blazingly_fast_slogan_footer() {
    // The user asked to drop the chevron-bracketed
    // "Blazingly fast by design. Secure by default. Loved by developers."
    // line. Render the welcome panel into a buffer and confirm none
    // of the slogan fragments appear anywhere on the canvas.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 40,
    };
    let backend = ratatui::backend::TestBackend::new(area.width, area.height);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_welcome(f, area)).unwrap();
    let buffer = term.backend().buffer();
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            row.push_str(buffer[(x, y)].symbol());
        }
        for needle in [
            "Blazingly fast by design",
            "Secure by default",
            "Loved by developers",
        ] {
            assert!(
                !row.contains(needle),
                "welcome row {y} still carries removed slogan {needle:?}: {row:?}"
            );
        }
    }
}

#[test]
fn render_welcome_does_not_panic_in_default_80x25_with_many_commits() {
    // Repro for the index-(41,70) panic at startup: ratatui's initial
    // backend size is 80x25 before the alt-screen reflow. With a long
    // recents list the previous code computed a box taller than 25,
    // ran past the buffer, and panicked inside set_string.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome
        .set_test_remote(Some("https://bitbucket.org/u/repo".to_string()));
    app.welcome.set_test_commits((0..40)
            .map(|i| crate::git::CommitInfo {
                hash: format!("hash{i:04}"),
                full_hash: format!("fullhash{i:040}"),
                when: "1 hour ago".to_string(),
                subject:
                    "this is a long subject line that will wrap to multiple lines on a narrow recents column"
                        .to_string(),
            })
            .collect());
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    let backend = ratatui::backend::TestBackend::new(area.width, area.height);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_welcome(f, area)).unwrap();
}

#[test]
fn paint_gradient_box_draws_rounded_corners_with_corner_colours() {
    let rect = Rect {
        x: 0,
        y: 0,
        width: 8,
        height: 4,
    };
    let mut buf = ratatui::buffer::Buffer::empty(rect);
    paint_gradient_box(&mut buf, rect);
    assert_eq!(buf[(0, 0)].symbol(), "\u{256d}", "top-left rounded");
    assert_eq!(buf[(7, 0)].symbol(), "\u{256e}", "top-right rounded");
    assert_eq!(buf[(0, 3)].symbol(), "\u{2570}", "bottom-left rounded");
    assert_eq!(buf[(7, 3)].symbol(), "\u{256f}", "bottom-right rounded");
    assert_eq!(buf[(3, 0)].symbol(), "\u{2500}", "top edge horizontal");
    assert_eq!(buf[(0, 1)].symbol(), "\u{2502}", "left edge vertical");
    // Top-left cell carries the GRAD_TL fg colour exactly.
    let tl_fg = buf[(0, 0)].fg;
    assert_eq!(tl_fg, rgb_color(GRAD_TL));
    let tr_fg = buf[(7, 0)].fg;
    assert_eq!(tr_fg, rgb_color(GRAD_TR));
}

#[test]
fn welcome_tagline_constant_is_present() {
    assert!(WELCOME_TAGLINE.contains("LIGHTWEIGHT"));
    assert!(WELCOME_TAGLINE.contains("BLAZINGLY FAST"));
    assert!(WELCOME_TAGLINE.contains("DEVELOPERS"));
}

#[test]
fn key_to_bytes_arrows() {
    assert_eq!(
        key_to_bytes(key(KeyCode::Up, KeyModifiers::NONE)),
        b"\x1b[A"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Down, KeyModifiers::NONE)),
        b"\x1b[B"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Right, KeyModifiers::NONE)),
        b"\x1b[C"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Left, KeyModifiers::NONE)),
        b"\x1b[D"
    );
}

#[test]
fn key_to_bytes_enter_tab_backspace() {
    assert_eq!(key_to_bytes(key(KeyCode::Enter, KeyModifiers::NONE)), b"\r");
    assert_eq!(key_to_bytes(key(KeyCode::Tab, KeyModifiers::NONE)), b"\t");
    assert_eq!(
        key_to_bytes(key(KeyCode::Backspace, KeyModifiers::NONE)),
        &[0x7f]
    );
}

#[test]
fn key_to_bytes_ctrl_letter_maps_to_control_byte() {
    let bytes = key_to_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(bytes, vec![0x03]);
    let bytes = key_to_bytes(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    assert_eq!(bytes, vec![0x01]);
    let bytes = key_to_bytes(key(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(bytes, vec![0x1a]);
}

#[test]
fn key_to_bytes_alt_letter_prefixes_esc() {
    let bytes = key_to_bytes(key(KeyCode::Char('x'), KeyModifiers::ALT));
    assert_eq!(bytes, vec![0x1b, b'x']);
}

#[test]
fn key_to_bytes_alt_left_emits_esc_b_for_readline_backward_word() {
    let bytes = key_to_bytes(key(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(
        bytes, b"\x1bb",
        "Option+Left in the terminal must send ESC b so zsh/bash readline runs backward-word"
    );
}

#[test]
fn key_to_bytes_alt_right_emits_esc_f_for_readline_forward_word() {
    let bytes = key_to_bytes(key(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(
        bytes, b"\x1bf",
        "Option+Right in the terminal must send ESC f so zsh/bash readline runs forward-word"
    );
}

#[test]
fn key_to_bytes_plain_arrows_unchanged_when_alt_not_held() {
    assert_eq!(
        key_to_bytes(key(KeyCode::Left, KeyModifiers::NONE)),
        b"\x1b[D"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Right, KeyModifiers::NONE)),
        b"\x1b[C"
    );
}

#[test]
fn key_to_bytes_plain_char_utf8() {
    assert_eq!(
        key_to_bytes(key(KeyCode::Char('a'), KeyModifiers::NONE)),
        b"a"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Char('é'), KeyModifiers::NONE)),
        "é".as_bytes()
    );
}

#[test]
fn key_to_bytes_function_keys() {
    assert_eq!(
        key_to_bytes(key(KeyCode::F(1), KeyModifiers::NONE)),
        b"\x1bOP"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::F(5), KeyModifiers::NONE)),
        b"\x1b[15~"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::F(12), KeyModifiers::NONE)),
        b"\x1b[24~"
    );
}

#[test]
fn key_to_bytes_unknown_returns_empty() {
    assert!(key_to_bytes(key(KeyCode::CapsLock, KeyModifiers::NONE)).is_empty());
}

#[test]
fn ctrl_s_is_save_key() {
    assert!(is_save_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL)));
}

#[test]
fn cmd_s_is_save_key() {
    assert!(is_save_key(key(KeyCode::Char('s'), KeyModifiers::SUPER)));
}

#[test]
fn cmd_w_and_ctrl_shift_w_close_terminal_but_plain_ctrl_w_does_not() {
    // Cmd+W is the primary terminal-close chord; Ctrl+Shift+W stays as a
    // legacy fallback.
    assert!(is_terminal_close_key(key(
        KeyCode::Char('w'),
        KeyModifiers::SUPER
    )));
    assert!(is_terminal_close_key(key(
        KeyCode::Char('w'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
    // Plain Ctrl+W must NOT close the terminal: it is the shell's
    // delete-word-backward, and hijacking it would break readline editing.
    assert!(!is_terminal_close_key(key(
        KeyCode::Char('w'),
        KeyModifiers::CONTROL
    )));
}

#[test]
fn shift_ctrl_s_is_save_key() {
    // Some terminals report capital S with Ctrl pressed.
    let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    assert!(is_save_key(key(KeyCode::Char('S'), mods)));
}

#[test]
fn plain_s_is_not_save_key() {
    assert!(!is_save_key(key(KeyCode::Char('s'), KeyModifiers::NONE)));
}

#[test]
fn ctrl_q_is_not_save_key() {
    assert!(!is_save_key(key(KeyCode::Char('q'), KeyModifiers::CONTROL)));
}

#[test]
fn alt_s_is_not_save_key() {
    assert!(!is_save_key(key(KeyCode::Char('s'), KeyModifiers::ALT)));
}

#[test]
fn git_status_spans_empty_when_not_in_repo() {
    let st = crate::git::GitStatus::default();
    let spans = git_status_spans(&st);
    assert!(spans.is_empty(), "no git pill outside a git repo");
}

#[test]
fn git_status_spans_clean_branch_is_green() {
    // Agnoster convention: clean working tree → green pill.
    let st = crate::git::GitStatus {
        in_repo: true,
        branch: Some("main".into()),
        detached_hash: None,
        dirty: false,
        ahead: 0,
        behind: 0,
    };
    let spans = git_status_spans(&st);
    let main_span = spans
        .iter()
        .find(|s| s.content.contains("main"))
        .expect("branch span");
    assert_eq!(main_span.style.fg, Some(GIT_CLEAN_COLOR));
}

#[test]
fn git_status_spans_dirty_branch_is_yellow_not_red() {
    // Agnoster convention: dirty working tree → yellow/orange pill.
    // No red bullet either — colour alone carries the state.
    let st = crate::git::GitStatus {
        in_repo: true,
        branch: Some("main".into()),
        detached_hash: None,
        dirty: true,
        ahead: 0,
        behind: 0,
    };
    let spans = git_status_spans(&st);
    let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        !joined.contains('●'),
        "no red bullet — colour signals dirtiness"
    );
    let main_span = spans
        .iter()
        .find(|s| s.content.contains("main"))
        .expect("branch span");
    assert_eq!(main_span.style.fg, Some(GIT_DIRTY_COLOR));
}

#[test]
fn git_status_spans_renders_detached_hash_when_no_branch() {
    let st = crate::git::GitStatus {
        in_repo: true,
        branch: None,
        detached_hash: Some("abc1234".into()),
        dirty: false,
        ahead: 0,
        behind: 0,
    };
    let spans = git_status_spans(&st);
    let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(joined.contains("abc1234"));
}

#[test]
fn git_status_spans_renders_ahead_behind_counts() {
    let st = crate::git::GitStatus {
        in_repo: true,
        branch: Some("main".into()),
        detached_hash: None,
        dirty: false,
        ahead: 2,
        behind: 1,
    };
    let spans = git_status_spans(&st);
    let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(joined.contains("↑2"));
    assert!(joined.contains("↓1"));
}

#[test]
fn ctrl_shift_f_jumps_to_search() {
    let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    assert!(is_search_jump_key(key(KeyCode::Char('F'), mods)));
}

#[test]
fn cmd_shift_f_jumps_to_search() {
    let mods = KeyModifiers::SUPER | KeyModifiers::SHIFT;
    assert!(is_search_jump_key(key(KeyCode::Char('F'), mods)));
}

#[test]
fn plain_f_does_not_jump_to_search() {
    assert!(!is_search_jump_key(key(
        KeyCode::Char('f'),
        KeyModifiers::NONE
    )));
}

#[test]
fn ctrl_c_is_editor_copy_key() {
    assert!(is_editor_copy_key(key(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL
    )));
}

#[test]
fn cmd_c_is_editor_copy_key() {
    assert!(is_editor_copy_key(key(
        KeyCode::Char('c'),
        KeyModifiers::SUPER
    )));
}

#[test]
fn plain_c_is_not_editor_copy_key() {
    assert!(!is_editor_copy_key(key(
        KeyCode::Char('c'),
        KeyModifiers::NONE
    )));
}

#[test]
fn ctrl_x_is_editor_cut_key() {
    assert!(is_editor_cut_key(key(
        KeyCode::Char('x'),
        KeyModifiers::CONTROL
    )));
    assert!(is_editor_cut_key(key(
        KeyCode::Char('x'),
        KeyModifiers::SUPER
    )));
    assert!(!is_editor_cut_key(key(
        KeyCode::Char('x'),
        KeyModifiers::NONE
    )));
}

#[test]
fn ctrl_a_is_editor_select_all_key() {
    assert!(is_editor_select_all_key(key(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL
    )));
    assert!(is_editor_select_all_key(key(
        KeyCode::Char('a'),
        KeyModifiers::SUPER
    )));
    assert!(!is_editor_select_all_key(key(
        KeyCode::Char('a'),
        KeyModifiers::NONE
    )));
}

#[test]
fn ctrl_z_is_editor_undo_key() {
    assert!(is_editor_undo_key(key(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL
    )));
    assert!(is_editor_undo_key(key(
        KeyCode::Char('z'),
        KeyModifiers::SUPER
    )));
}

#[test]
fn plain_z_is_not_editor_undo_key() {
    assert!(!is_editor_undo_key(key(
        KeyCode::Char('z'),
        KeyModifiers::NONE
    )));
}

#[test]
fn shift_cmd_z_reserved_for_redo_not_undo() {
    let mods = KeyModifiers::SUPER | KeyModifiers::SHIFT;
    assert!(!is_editor_undo_key(key(KeyCode::Char('z'), mods)));
}

#[test]
fn ctrl_shift_c_is_recognized_as_terminal_copy() {
    let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    assert!(is_terminal_copy_key(key(KeyCode::Char('C'), mods)));
}

#[test]
fn cmd_c_is_recognized_as_terminal_copy() {
    // For terminals that pass Cmd via the kitty protocol; iTerm2 users
    // can also remap ⌘C → Send Hex Code 0x03 for terminal-pane copies,
    // but for keyboard-protocol-aware terminals we accept Super here.
    assert!(is_terminal_copy_key(key(
        KeyCode::Char('c'),
        KeyModifiers::SUPER
    )));
}

#[test]
fn plain_ctrl_c_is_not_terminal_copy() {
    // Ctrl+C must remain SIGINT in the embedded shell; it must not be
    // intercepted as the croft copy gesture.
    assert!(!is_terminal_copy_key(key(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL
    )));
}

/// Cmd+C from the croft terminal pane must land the selection on the
/// real macOS clipboard. This is the user-visible regression test
/// for the long-standing "I copied from croft and pbpaste shows the
/// previous value" bug, where the old OSC 52 path was silently
/// dropped by the host terminal. The contract: writing through
/// `copy_to_clipboard` puts the text on the system pasteboard such
/// that `clipboard::read_string` (which is what Cmd+V uses) returns
/// the same bytes. If this ever fails again, copy/paste between
/// croft and other apps is broken and the test must catch it before
/// the binary ships.
#[cfg(target_os = "macos")]
#[test]
fn copy_to_clipboard_writes_text_to_the_macos_system_pasteboard() {
    let _g = crate::clipboard::lock_clipboard_for_test();
    let sentinel = format!(
        "croft-copy-helper-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let used_native = copy_to_clipboard(&sentinel);
    assert!(
        used_native,
        "on macOS, copy_to_clipboard must succeed via NSPasteboard and return true so the call site can skip the OSC 52 fallback (and the host terminal's quirks can't silently drop the write)"
    );
    let got = crate::clipboard::read_string().expect("read_string must see what we just wrote");
    assert_eq!(
        got, sentinel,
        "round-trip through the system pasteboard must be byte-exact - if this ever drifts, every Cmd+C from croft is silently lossy"
    );
}

/// Tighter regression: a real `App` with a real `PtyTerminal`, a
/// terminal-pane selection, and `App::copy_terminal_selection` must
/// land the text on the macOS system clipboard. Exercises the full
/// wire from PTY bytes → alacritty grid → `selection_text` →
/// `copy_to_clipboard` → `NSPasteboard.setString:forType:` → what
/// `pbpaste` / `NSPasteboard.stringForType:` returns. A refactor
/// that reintroduces an OSC-52-only path in the terminal copy
/// handler would fail this test even if `copy_to_clipboard` itself
/// still resolved to the native path elsewhere.
#[cfg(target_os = "macos")]
#[test]
fn cmd_c_on_terminal_selection_lands_text_on_macos_clipboard() {
    let _g = crate::clipboard::lock_clipboard_for_test();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Render once so `terminal.last_inner` is populated and the
    // `start_selection_at` / `extend_selection_to` chord can map
    // viewport coordinates to cell coordinates the way the live
    // input handler does on a real keystroke.
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    app.terminal_mut().focused = true;
    term.draw(|f| app.render(f)).unwrap();

    let probe = format!("croft-terminal-cmdc-{}", std::process::id());
    app.terminal_mut()
        .write_input(format!("printf '{probe}\\n'\r").as_bytes());

    // Wait for the shell to push bytes through the PTY and the
    // background reader thread to drain them into the grid.
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_millis(3000) {
        if app.terminal().visible_text().contains(&probe) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Re-render to refresh `last_inner` after the grid has grown.
    term.draw(|f| app.render(f)).unwrap();

    let snapshot = app.terminal().visible_text();
    assert!(
        snapshot.contains(&probe),
        "shell must echo the probe within 3s; got:\n{snapshot}"
    );

    let row_idx = snapshot
        .lines()
        .position(|l| l.contains(&probe))
        .expect("probe row must exist");
    let line = snapshot.lines().nth(row_idx).unwrap();
    let col_start = line.find(&probe).unwrap();
    let col_end = col_start + probe.len() - 1;

    let inner = app.terminal().last_inner;
    let screen_y = inner.y + row_idx as u16;
    let screen_x_a = inner.x + col_start as u16;
    let screen_x_b = inner.x + col_end as u16;
    app.terminal_mut().start_selection_at(screen_x_a, screen_y);
    app.terminal_mut().extend_selection_to(screen_x_b, screen_y);
    assert_eq!(
        app.terminal().selection_text(),
        probe,
        "selection_text must reproduce the probe exactly so a Cmd+C copy is byte-faithful"
    );

    app.copy_terminal_selection();
    let got = crate::clipboard::read_string()
        .expect("read_string must surface what copy_terminal_selection just wrote");
    assert_eq!(
        got, probe,
        "after a Cmd+C on a terminal selection, the system clipboard must contain the exact selection text - this is the non-negotiable round-trip the user relies on for every copy out of croft"
    );
}

#[test]
fn cmd_c_on_side_by_side_diff_selection_lands_text_on_macos_clipboard() {
    let _g = crate::clipboard::lock_clipboard_for_test();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    std::fs::write(&f1, "alpha\nbravo\ncharlie\n").unwrap();
    std::fs::write(&f2, "alpha\nBRAVO\ncharlie\n").unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    app.focus = Pane::Editor;
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let diff = app.editor.diff.as_ref().unwrap();
    let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
    let l_text_x = app.editor.last_inner.x + l_gutter + 2;
    let body_top = app.editor.last_inner.y + 1;
    // Row 1 in the diff (after the @@/header rows) holds "bravo" on the
    // left. Click at col 0, drag to col 5 to select all five chars.
    let click_y = body_top + 1;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
        column: l_text_x,
        row: click_y,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(MouseButton::Left),
        column: l_text_x + 5,
        row: click_y,
        modifiers: KeyModifiers::NONE,
    });
    let sel = app
        .editor
        .diff
        .as_ref()
        .unwrap()
        .selection
        .expect("drag must have produced a diff selection");
    assert!(
        sel.has_area(),
        "drag must produce a non-zero-area selection"
    );

    // Drive the actual Cmd+C keystroke through handle_key. This is the
    // path the live app takes when the user presses Cmd+C; the
    // editor's main key handler short-circuits to handle_diff_key when
    // a diff is open, so a copy keybinding must be wired explicitly
    // INSIDE handle_diff_key. Without that branch, this assertion
    // fails — which is exactly the regression the user just reported:
    // selecting text in a diff and pressing Cmd+C did nothing,
    // leaving stale test sentinels on the system clipboard for the
    // next paste to surface.
    let _ = app.handle_key(key(KeyCode::Char('c'), KeyModifiers::SUPER));
    let got = crate::clipboard::read_string()
        .expect("Cmd+C on a diff selection must put text on the clipboard");
    assert_eq!(
        got, "bravo",
        "selecting cols 0..5 of the left column of row 1 and pressing Cmd+C must copy the exact left-side text via handle_diff_key's copy branch"
    );
}

#[test]
fn cmd_w_closes_a_diff_tab() {
    // Repro: Cmd+W did nothing on a diff view. handle_editor_key
    // short-circuits to handle_diff_key when a diff is open and returns
    // before the close-tab branch, so the close-tab key was swallowed.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    std::fs::write(&f1, "alpha\nbravo\n").unwrap();
    std::fs::write(&f2, "alpha\nBRAVO\n").unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    app.focus = Pane::Editor;
    assert!(app.editor.diff.is_some(), "setup: diff must be open");

    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();

    assert!(
        app.editor.diff.is_none(),
        "Cmd+W on a diff view must close the tab (resetting the last tab to blank), not be swallowed by handle_diff_key"
    );
}

#[test]
fn cmd_w_closes_a_sheet_tab() {
    // Same swallow bug, sheet branch: handle_sheet_key returned before
    // the close-tab handler so Cmd+W never reached close_active.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let csv = tmp.path().join("data.csv");
    std::fs::write(&csv, "name,age\nAlice,30\n").unwrap();
    app.editor.open(&csv).unwrap();
    app.focus = Pane::Editor;
    assert!(app.editor.sheet.is_some(), "setup: sheet must be open");

    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();

    assert!(
        app.editor.sheet.is_none(),
        "Cmd+W on a sheet view must close the tab, not be swallowed by handle_sheet_key"
    );
}

#[test]
fn cmd_w_closes_an_image_tab() {
    // Same swallow bug, image branch: the read-only image guard returned
    // before the close-tab handler so Cmd+W could never close a PNG tab.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let png = tmp.path().join("pic.png");
    image::RgbaImage::new(2, 2).save(&png).unwrap();
    app.editor.open(&png).unwrap();
    app.focus = Pane::Editor;
    assert!(app.editor.image.is_some(), "setup: image must be open");

    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();

    assert!(
        app.editor.image.is_none(),
        "Cmd+W on an image view must close the tab, not be swallowed by the read-only image guard"
    );
}

#[test]
fn enter_in_diff_view_opens_the_file_under_the_caret_at_its_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    std::fs::write(&f1, "alpha\nbravo\n").unwrap();
    std::fs::write(&f2, "alpha\nBRAVO\ncharlie\n").unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    app.focus = Pane::Editor;
    // Row 2 is the Added "charlie" row on the right side: new-file line 3.
    app.editor
        .diff
        .as_mut()
        .unwrap()
        .start_selection(crate::widgets::diff::DiffSide::Right, 2, 0);

    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.editor.diff.is_none(),
        "Enter must leave the diff and land in a real editable file tab"
    );
    assert_eq!(
        app.editor.path.as_deref(),
        Some(f2.as_path()),
        "the caret was on the right (working-tree) side, so Enter opens b.txt"
    );
    assert_eq!(
        app.editor.cursor_row, 2,
        "goto_line(3) puts the zero-based cursor on row 2 (the 'charlie' line)"
    );
}

#[test]
fn enter_on_a_removed_line_in_a_source_control_diff_opens_the_working_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let foo = tmp.path().join("foo.txt");
    // Working tree dropped "bravo"; HEAD still has it.
    std::fs::write(&foo, "alpha\ncharlie\n").unwrap();
    // Mirror open_source_control_entry's Modified branch: synthetic HEAD
    // label on the left, the real working file on the right.
    let label = std::path::PathBuf::from("foo.txt (HEAD)");
    app.editor
        .open_head_diff_with_text(label, "alpha\nbravo\ncharlie\n", &foo)
        .unwrap();
    app.focus = Pane::Editor;
    // Rows: 0 Equal alpha, 1 Removed bravo, 2 Equal charlie. Click row 1.
    app.editor
        .diff
        .as_mut()
        .unwrap()
        .start_selection(crate::widgets::diff::DiffSide::Left, 1, 0);

    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.editor.diff.is_none(),
        "Enter on a removed line must leave the diff for the real working \
         file, not stay parked on the diff tab; status was {:?}",
        app.status
    );
    assert_eq!(app.editor.path.as_deref(), Some(foo.as_path()));
}

#[test]
fn enter_in_multifile_git_diff_opens_the_real_file_under_the_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let foo = src_dir.join("foo.rs");
    std::fs::write(&foo, "keep1\nold\nkeep2\n").unwrap();
    let raw = concat!(
        "diff --git a/src/foo.rs b/src/foo.rs\n",
        "index 1111111..2222222 100644\n",
        "--- a/src/foo.rs\n",
        "+++ b/src/foo.rs\n",
        "@@ -1,3 +1,4 @@\n",
        " keep1\n",
        "-old\n",
        "+new1\n",
        "+new2\n",
        " keep2\n",
    );
    let label = std::path::PathBuf::from("git diff --staged");
    app.editor.open_git_diff_side_by_side(&label, raw).unwrap();
    app.focus = Pane::Editor;
    // Row 3 is "+new1" on the right side (new-file line 2).
    app.editor
        .diff
        .as_mut()
        .unwrap()
        .start_selection(crate::widgets::diff::DiffSide::Right, 3, 0);

    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    assert_eq!(
        app.editor.path.as_deref(),
        Some(foo.as_path()),
        "Enter on a row in the multi-file git diff must open the real file the \
         `diff --git` header names, resolved against the workspace root; status was {:?}",
        app.status
    );
    assert_eq!(app.editor.cursor_row, 1, "+new1 is new-file line 2");
}

#[test]
fn editor_find_in_side_by_side_diff_counts_matches_in_diff_content() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    std::fs::write(&f1, "alpha\nbravo\ncharlie\n").unwrap();
    std::fs::write(&f2, "alpha\nbravo\ncharlie\nlogger.info here\n").unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    app.focus = Pane::Editor;
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    app.open_editor_find();
    app.editor_find_set_query(String::from("logger"));

    let count = app.editor_find.as_ref().map(|s| s.match_count).unwrap_or(0);
    assert!(
        count >= 1,
        "Find must search the content rendered in a diff tab: 'logger' is visible in the right column yet match_count={count}"
    );
    let active = app
        .editor
        .diff
        .as_ref()
        .and_then(|d| d.find.active)
        .expect("a match in a diff must set an active find position for highlighting");
    assert_eq!(
        active.side,
        crate::widgets::diff::DiffSide::Right,
        "the 'logger' match lives on the added (right) side and must be selected as active"
    );
}

#[test]
fn editor_find_in_diff_next_prev_cycle_through_both_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    std::fs::write(&f1, "needle one\nplain\n").unwrap();
    std::fs::write(&f2, "needle one\nplain\nneedle two\n").unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    app.focus = Pane::Editor;
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    app.open_editor_find();
    app.editor_find_set_query(String::from("needle"));
    let count = app.editor_find.as_ref().map(|s| s.match_count).unwrap_or(0);
    assert!(
        count >= 2,
        "an equal line counts on both columns plus the added line: expected >=2, got {count}"
    );
    let first = app.editor_find.as_ref().and_then(|s| s.match_index);
    assert_eq!(first, Some(1), "opening find selects the first match");
    app.editor_find_jump_next();
    let second = app.editor_find.as_ref().and_then(|s| s.match_index);
    assert_eq!(second, Some(2), "Enter advances to the second match");
    app.editor_find_jump_prev();
    let back = app.editor_find.as_ref().and_then(|s| s.match_index);
    assert_eq!(back, Some(1), "Shift+Enter walks back to the first match");
}

#[test]
fn double_click_in_diff_selects_word_and_cmd_c_copies_it() {
    // End-to-end: two left-clicks in quick succession on a word in
    // the left column must snap a word-bounded selection (same UX
    // as VS Code / standard text editors), and the very next Cmd+C
    // keystroke must put exactly that word on the clipboard.
    let _g = crate::clipboard::lock_clipboard_for_test();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    std::fs::write(&f1, "alpha bravo charlie\nsecond\n").unwrap();
    std::fs::write(&f2, "alpha BRAVO charlie\nsecond\n").unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    app.focus = Pane::Editor;
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let diff = app.editor.diff.as_ref().unwrap();
    let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
    let l_text_x = app.editor.last_inner.x + l_gutter + 2;
    let body_top = app.editor.last_inner.y + 1;
    // "alpha bravo charlie" — "bravo" starts at char col 6.
    let bravo_x = l_text_x + 8;
    let click_y = body_top;
    let first = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
        column: bravo_x,
        row: click_y,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(first);
    let second = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
        column: bravo_x,
        row: click_y,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(second);
    let sel = app
        .editor
        .diff
        .as_ref()
        .unwrap()
        .selection
        .expect("double-click must produce a word selection");
    assert!(sel.has_area(), "word selection must span >0 chars");

    let _ = app.handle_key(key(KeyCode::Char('c'), KeyModifiers::SUPER));
    let got = crate::clipboard::read_string()
        .expect("Cmd+C after double-click must put the word on the clipboard");
    assert_eq!(
        got, "bravo",
        "double-click word selection + Cmd+C must copy exactly the clicked word"
    );
}

#[test]
fn lock_clipboard_for_test_restores_prior_clipboard_text_on_drop() {
    // Tests that hit the real NSPasteboard must never leave their
    // sentinels behind. This test simulates a user already having
    // text on the clipboard, runs a guarded section that writes a
    // sentinel, drops the guard, and asserts the clipboard now
    // reads back the simulated "user" text rather than the test
    // sentinel.
    //
    // To avoid leaking the simulated text into the developer's real
    // clipboard, capture the genuine pre-test contents first and
    // restore them at the very end via a Drop guard so even a panic
    // in the middle leaves the host clipboard untouched.
    struct RestoreClip(Option<String>);
    impl Drop for RestoreClip {
        fn drop(&mut self) {
            match self.0.take() {
                Some(s) => {
                    crate::clipboard::macos::write_string_native(&s);
                }
                None => crate::clipboard::macos::clear_native(),
            }
        }
    }
    let _restore = RestoreClip(crate::clipboard::macos::read_string_native());

    let pre = format!("user-preexisting-clip-{}", std::process::id());
    crate::clipboard::macos::write_string_native(&pre);
    {
        let _g = crate::clipboard::lock_clipboard_for_test();
        let sentinel = "test-sentinel-should-not-leak";
        assert!(
            crate::clipboard::write_string(sentinel),
            "guard-held write must hit the real NSPasteboard"
        );
        assert_eq!(
            crate::clipboard::read_string().as_deref(),
            Some(sentinel),
            "while the guard is held the test reads what it just wrote"
        );
    }
    let after = crate::clipboard::macos::read_string_native();
    assert_eq!(
        after.as_deref(),
        Some(pre.as_str()),
        "dropping the guard MUST restore the pre-test clipboard so a cargo test run never leaves a test sentinel on the developer's system clipboard for the next paste to surface"
    );
}

#[test]
fn ctrl_j_is_recognized_as_terminal_toggle() {
    assert!(is_terminal_toggle_key(key(
        KeyCode::Char('j'),
        KeyModifiers::CONTROL
    )));
}

#[test]
fn ctrl_shift_j_is_not_terminal_toggle_anymore() {
    // Ctrl+Shift+J is now reserved for the terminal-maximize chord, so
    // the terminal-toggle matcher must reject it. Ctrl+J alone (with no
    // shift) still toggles.
    let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    assert!(!is_terminal_toggle_key(key(KeyCode::Char('J'), mods)));
    assert!(!is_terminal_toggle_key(key(KeyCode::Char('j'), mods)));
}

#[test]
fn plain_j_is_not_terminal_toggle() {
    assert!(!is_terminal_toggle_key(key(
        KeyCode::Char('j'),
        KeyModifiers::NONE
    )));
}

#[test]
fn ctrl_shift_j_is_terminal_maximize_key() {
    let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    assert!(is_terminal_maximize_key(key(KeyCode::Char('J'), mods)));
    assert!(is_terminal_maximize_key(key(KeyCode::Char('j'), mods)));
}

#[test]
fn plain_ctrl_j_is_not_terminal_maximize_key() {
    assert!(!is_terminal_maximize_key(key(
        KeyCode::Char('j'),
        KeyModifiers::CONTROL
    )));
}

#[test]
fn plain_shift_j_is_not_terminal_maximize_key() {
    assert!(!is_terminal_maximize_key(key(
        KeyCode::Char('J'),
        KeyModifiers::SHIFT
    )));
}

#[test]
fn alt_or_super_blocks_terminal_maximize_key() {
    let with_alt = KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::ALT;
    let with_super = KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::SUPER;
    assert!(!is_terminal_maximize_key(key(KeyCode::Char('J'), with_alt)));
    assert!(!is_terminal_maximize_key(key(
        KeyCode::Char('J'),
        with_super
    )));
}

#[test]
fn ctrl_shift_j_toggles_terminal_maximize_state_via_handle_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(!app.terminal_maximized, "fresh app starts un-maximized");
    let chord = key(
        KeyCode::Char('J'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    app.handle_key(chord).unwrap();
    assert!(app.terminal_maximized, "first chord maximizes");
    assert!(app.show_terminal, "maximize forces terminal visible");
    assert!(
        matches!(app.focus, Pane::Terminal),
        "maximize focuses the terminal"
    );
    app.handle_key(chord).unwrap();
    assert!(!app.terminal_maximized, "second chord restores split");
}

/// User-visible regression: with the terminal maximised via
/// Ctrl+Shift+J, a mouse drag inside the (now fullscreen) terminal
/// pane must reach `PtyTerminal::extend_selection_to` so the user
/// can highlight terminal text. Before this fix `EditorTabs::render`
/// returned early on `area.height == 0` without zeroing the active
/// editor's `last_area`, so `handle_mouse`'s `in_editor` hit-test
/// (`rect_contains(self.editor.last_area, ...)`) still matched the
/// pre-maximise rectangle and the dispatch at app.rs:7229 / 7336
/// routed the click into `editor.mouse_down` / `editor.mouse_drag`.
/// The user saw the file caret jump instead of a selection
/// appearing in the terminal — symptom: "the cursor is jumping up
/// and down as I go back and forth."
#[test]
fn maximised_terminal_owns_mouse_drag_for_selection_not_the_collapsed_editor() {
    let tmp = tempfile::tempdir().unwrap();
    // Seed a real file and open it so the editor actually claims a
    // non-empty rectangle on the first render (the blank-initial
    // branch zeroes `last_area.height` to skip welcome-pane
    // overdraw, which would mask the bug we're probing).
    let f = tmp.path().join("probe.txt");
    std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();

    // Render once in split layout so the editor's `last_area` ends
    // up pointing at the right-hand pane — the exact stale
    // rectangle the bug would later read from.
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let editor_area_before_maximise = app.editor.last_area;
    assert!(
        editor_area_before_maximise.width > 0 && editor_area_before_maximise.height > 0,
        "precondition: in split layout with an open file the editor must claim a real rectangle, otherwise the bug we're testing for can't manifest"
    );

    // Trigger the maximize chord.
    let chord = key(
        KeyCode::Char('J'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    app.handle_key(chord).unwrap();
    assert!(app.terminal_maximized);

    // Re-render in the maximised layout — this is the frame that
    // used to leave `editor.last_area` stale.
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(
        app.editor.last_area.height, 0,
        "after maximise, the editor's hit-test rectangle must have height 0 so `in_editor` cannot match a click that lands on the maximised terminal"
    );

    // Drive a real Left-Drag through the production key/mouse
    // dispatch: anywhere inside the maximised terminal pane. With
    // the fix in place the drag must end up extending the terminal
    // selection, not poking the editor caret.
    let term_area = app.terminal().last_area;
    assert!(
        term_area.width > 4 && term_area.height > 4,
        "precondition: the maximised terminal must own a real rect for the drag to land in"
    );
    let click_col = term_area.x + 2;
    let click_row = term_area.y + 2;
    let drag_col = click_col + 5;
    let drag_row = click_row;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: click_col,
        row: click_row,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: drag_col,
        row: drag_row,
        modifiers: KeyModifiers::NONE,
    });
    let sel = app.terminal().selection();
    assert!(
        sel.is_some_and(|s| s.has_area()),
        "drag inside the maximised terminal must seed a non-empty selection on the PtyTerminal - if this is None or zero-area the click was eaten by the stale editor hit-test"
    );
}

/// Cross-pane paste matrix. In production, Cmd+V on macOS does NOT
/// arrive as a key event — `setup-iterm2` deliberately leaves Cmd+V
/// unbound (src/iterm2.rs:225-235) so iTerm2's native Paste action
/// fires and emits a bracketed-paste sequence carrying the OS
/// clipboard into the PTY. crossterm surfaces that as
/// `Event::Paste(s)`, dispatched by `main_loop` to
/// `App::handle_paste(&s)` (src/app.rs:17131).
///
/// This matrix therefore drives `handle_paste` directly with the
/// payload that iTerm2 would have delivered. Every destination
/// (editor, editor-find, search query, file-finder query,
/// terminal, source-control commit box) must consume the payload
/// in BOTH terminal layouts (split + maximised). If a case
/// silently fails here, that's the exact production symptom the
/// user described: "I copied from one place, pasted in another,
/// nothing happened."
#[test]
fn cross_pane_paste_via_bracketed_paste_works_in_every_destination_and_terminal_layout() {
    fn seed_app() -> (tempfile::TempDir, App) {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("probe.txt");
        std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_pinned(&f).unwrap();
        (tmp, app)
    }

    fn render(app: &mut App) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let backend = ratatui::backend::TestBackend::new(140, 50);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        term
    }

    // Drive a bracketed paste from a non-path token, which is what
    // iTerm2 emits when the user presses Cmd+V with terminal text
    // on the clipboard. Non-path so the drop-handler branches in
    // handle_paste don't divert us into "import into Explorer / scp
    // to Remote / fetch via drop-relay".
    let payload_editor_split = "clip-into-editor-split";
    let payload_editor_max = "clip-into-editor-maximised";
    let payload_find_split = "clip-into-find-split";
    let payload_find_max = "clip-into-find-maximised";
    let payload_search_split = "clip-into-search-split";
    let payload_search_max = "clip-into-search-maximised";
    let payload_finder = "clip-into-file-finder";
    let payload_sc = "clip-into-source-control";

    // -- Case 1: paste into the editor while terminal is split.
    {
        let (_tmp, mut app) = seed_app();
        let _draw = render(&mut app);
        app.focus_pane(Pane::Editor);
        app.handle_paste(payload_editor_split);
        assert!(
            app.editor
                .lines
                .iter()
                .any(|l| l.contains(payload_editor_split)),
            "bracketed paste with editor focus + terminal split must insert into the editor buffer; lines={:?}",
            app.editor.lines
        );
    }

    // -- Case 2: paste into the editor while terminal is maximised.
    {
        let (_tmp, mut app) = seed_app();
        let _draw = render(&mut app);
        app.toggle_terminal_maximize();
        let _draw = render(&mut app);
        app.focus_pane(Pane::Editor);
        app.handle_paste(payload_editor_max);
        assert!(
            app.editor
                .lines
                .iter()
                .any(|l| l.contains(payload_editor_max)),
            "bracketed paste with editor focus + terminal maximised must still insert into the editor; lines={:?}",
            app.editor.lines
        );
    }

    // -- Case 3: paste into the editor-find bar while terminal is
    //    split. The user explicitly named this as broken.
    {
        let (_tmp, mut app) = seed_app();
        let _draw = render(&mut app);
        app.focus_pane(Pane::Editor);
        app.open_editor_find();
        app.handle_paste(payload_find_split);
        let q = app
            .editor_find
            .as_ref()
            .map(|s| s.query.clone())
            .unwrap_or_default();
        assert!(
            q.contains(payload_find_split),
            "bracketed paste with editor-find open + terminal split must append to the query; query={q:?}"
        );
    }

    // -- Case 4: paste into the editor-find bar while terminal is
    //    maximised.
    {
        let (_tmp, mut app) = seed_app();
        let _draw = render(&mut app);
        app.toggle_terminal_maximize();
        let _draw = render(&mut app);
        app.focus_pane(Pane::Editor);
        app.open_editor_find();
        app.handle_paste(payload_find_max);
        let q = app
            .editor_find
            .as_ref()
            .map(|s| s.query.clone())
            .unwrap_or_default();
        assert!(
            q.contains(payload_find_max),
            "bracketed paste with editor-find open + terminal maximised must append to the query - this is the path the user named explicitly; query={q:?}"
        );
    }

    // -- Case 5: paste into the Search sidebar query while
    //    terminal is split.
    {
        let (_tmp, mut app) = seed_app();
        let _draw = render(&mut app);
        app.set_sidebar_view(SidebarView::Search);
        let _draw = render(&mut app);
        app.handle_paste(payload_search_split);
        assert!(
            app.search.query.contains(payload_search_split),
            "bracketed paste in Search sidebar + terminal split must append to the search query; query={:?}",
            app.search.query
        );
    }

    // -- Case 6: paste into Search sidebar while terminal is
    //    maximised. The user explicitly named the split layout as
    //    failing while maximised worked; both must succeed.
    {
        let (_tmp, mut app) = seed_app();
        let _draw = render(&mut app);
        app.toggle_terminal_maximize();
        let _draw = render(&mut app);
        app.set_sidebar_view(SidebarView::Search);
        let _draw = render(&mut app);
        app.handle_paste(payload_search_max);
        assert!(
            app.search.query.contains(payload_search_max),
            "bracketed paste in Search sidebar + terminal maximised must also append; query={:?}",
            app.search.query
        );
    }

    // -- Case 7: paste into the embedded terminal while it is
    //    split. The terminal pane consumes the bracketed paste by
    //    writing the bytes back into the PTY.
    {
        let (_tmp, mut app) = seed_app();
        let _draw = render(&mut app);
        app.focus_pane(Pane::Terminal);
        // No panic + handler completes is enough. Verifying the
        // PTY received the bytes requires polling the shell which
        // is too timing-fragile for CI.
        app.handle_paste("clip-into-terminal-split");
    }

    // -- Case 8: paste into the embedded terminal while
    //    maximised.
    {
        let (_tmp, mut app) = seed_app();
        let _draw = render(&mut app);
        app.toggle_terminal_maximize();
        let _draw = render(&mut app);
        app.focus_pane(Pane::Terminal);
        app.handle_paste("clip-into-terminal-maximised");
    }

    // -- Case 9: paste into the Source Control commit message
    //    box while terminal split. NOTE: at the time of writing,
    //    handle_paste has no SourceControl branch — this case
    //    will FAIL until handle_paste learns to route into the
    //    commit message when sidebar_view == SourceControl.
    {
        let (_tmp, mut app) = seed_app();
        let _draw = render(&mut app);
        app.set_sidebar_view(SidebarView::SourceControl);
        let _draw = render(&mut app);
        app.handle_paste(payload_sc);
        assert!(
            app.source_control.message.contains(payload_sc),
            "bracketed paste in the Source Control commit box must append the clipboard; got: {:?}",
            app.source_control.message
        );
    }

    // -- Case 10: paste into the file-finder (Cmd+P quick-open).
    {
        let (_tmp, mut app) = seed_app();
        let _draw = render(&mut app);
        app.open_file_finder();
        app.handle_paste(payload_finder);
        let q = app
            .file_finder
            .as_ref()
            .map(|f| f.query.clone())
            .unwrap_or_default();
        assert!(
            q.contains(payload_finder),
            "bracketed paste in the file finder (Cmd+P) must append to the query; query={q:?}"
        );
    }
}

/// Double-clicking a word in the terminal pane must word-select
/// in BOTH terminal layouts. The user reported that double-click
/// "doesn't auto-select" in some state. The detection logic in
/// `handle_mouse` keys off `last_terminal_left_down` + window;
/// the test sends two consecutive Down events on the same cell
/// within DOUBLE_CLICK_WINDOW and asserts a non-empty word
/// selection appears.
#[test]
fn double_click_in_terminal_word_selects_in_split_and_maximised_layout() {
    for maximise in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let backend = ratatui::backend::TestBackend::new(140, 50);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        if maximise {
            app.toggle_terminal_maximize();
            term.draw(|f| app.render(f)).unwrap();
        }
        app.focus_pane(Pane::Terminal);
        let probe = format!("croft-dclick-{}", std::process::id());
        app.terminal_mut()
            .write_input(format!("printf '{probe}\\n'\r").as_bytes());
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_millis(3000) {
            if app.terminal().visible_text().contains(&probe) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        term.draw(|f| app.render(f)).unwrap();
        let snap = app.terminal().visible_text();
        let row_idx = snap
            .lines()
            .position(|l| l.contains(&probe))
            .expect("probe must render");
        let line = snap.lines().nth(row_idx).unwrap();
        let mid_col = line.find(&probe).unwrap() + probe.len() / 2;
        let inner = app.terminal().last_inner;
        let screen_y = inner.y + row_idx as u16;
        let screen_x = inner.x + mid_col as u16;

        // Two clicks within the DOUBLE_CLICK_WINDOW on the same
        // cell — same path a real macOS double-click takes.
        for _ in 0..2 {
            app.handle_mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: screen_x,
                row: screen_y,
                modifiers: KeyModifiers::NONE,
            });
        }

        let sel_text = app.terminal().selection_text();
        assert!(
            sel_text.contains(&probe) || probe.contains(&sel_text),
            "double-click on the middle of the probe word must word-select the run that contains it in {} layout; got selection={sel_text:?}",
            if maximise { "maximised" } else { "split" }
        );
    }
}

/// End-to-end variant of the matrix that exercises the user's
/// exact gesture chain: text on screen in the embedded terminal →
/// real Cmd+C through the terminal pane handler → switch to the
/// destination pane → paste. The clipboard is the per-thread mock
/// (see `crate::clipboard::test_clip`) so both ends ride the same
/// transport every croft Cmd+C / Cmd+V hits in production. Every
/// pair below must succeed in BOTH terminal layouts because the
/// user explicitly demands identical behaviour ("the terminal
/// size should not matter").
#[test]
fn terminal_copy_paste_into_other_panes_works_in_split_and_maximised_layout() {
    // Seed a PtyTerminal grid with a known string, set a selection
    // over it, and trigger the production Cmd+C path so the mock
    // clipboard ends up with the payload. Returns the payload that
    // landed on the clipboard.
    fn copy_terminal_word(app: &mut App) -> String {
        let probe = format!("croft-e2e-{}", std::process::id());
        app.terminal_mut()
            .write_input(format!("printf '{probe}\\n'\r").as_bytes());
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_millis(3000) {
            if app.terminal().visible_text().contains(&probe) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let snap = app.terminal().visible_text();
        let row_idx = snap.lines().position(|l| l.contains(&probe)).unwrap();
        let line = snap.lines().nth(row_idx).unwrap();
        let col_start = line.find(&probe).unwrap();
        let col_end = col_start + probe.len() - 1;
        let inner = app.terminal().last_inner;
        let screen_y = inner.y + row_idx as u16;
        let screen_x_a = inner.x + col_start as u16;
        let screen_x_b = inner.x + col_end as u16;
        app.terminal_mut().start_selection_at(screen_x_a, screen_y);
        app.terminal_mut().extend_selection_to(screen_x_b, screen_y);
        assert_eq!(
            app.terminal().selection_text(),
            probe,
            "terminal selection_text must reproduce the probe before we copy"
        );
        app.copy_terminal_selection();
        probe
    }

    // Drive every destination/layout pair. The clipboard lives on
    // the test thread, so cross-pane wiring is the only thing
    // that can fail here.
    let layouts: &[bool] = &[false, true]; // false = split, true = maximised
    for &maximise in layouts {
        // ---- Destination: Search sidebar ----
        {
            let _g = crate::clipboard::lock_clipboard_for_test();
            let tmp = tempfile::tempdir().unwrap();
            let mut app = App::new(tmp.path().to_path_buf()).unwrap();
            let backend = ratatui::backend::TestBackend::new(140, 50);
            let mut term = ratatui::Terminal::new(backend).unwrap();
            term.draw(|f| app.render(f)).unwrap();
            if maximise {
                app.toggle_terminal_maximize();
                term.draw(|f| app.render(f)).unwrap();
            }
            app.focus_pane(Pane::Terminal);
            let probe = copy_terminal_word(&mut app);
            // Switch to Search like a real user would (Cmd+Shift+F
            // through the production key path).
            let cmd_shift_f = key(
                KeyCode::Char('F'),
                KeyModifiers::SUPER | KeyModifiers::SHIFT,
            );
            app.handle_key(cmd_shift_f).unwrap();
            term.draw(|f| app.render(f)).unwrap();
            // Simulate the bracketed paste iTerm2 would send on
            // Cmd+V (production path with setup-iterm2 applied).
            let clip = crate::clipboard::read_string()
                .expect("clipboard must hold what copy_terminal_selection just wrote");
            assert_eq!(clip, probe);
            app.handle_paste(&clip);
            assert!(
                app.search.query.contains(&probe),
                "terminal copy -> Search paste must work in {} layout; got query={:?}",
                if maximise { "maximised" } else { "split" },
                app.search.query
            );
        }

        // ---- Destination: editor-find ----
        {
            let _g = crate::clipboard::lock_clipboard_for_test();
            let tmp = tempfile::tempdir().unwrap();
            let f = tmp.path().join("probe.txt");
            std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
            let mut app = App::new(tmp.path().to_path_buf()).unwrap();
            app.editor.open_pinned(&f).unwrap();
            let backend = ratatui::backend::TestBackend::new(140, 50);
            let mut term = ratatui::Terminal::new(backend).unwrap();
            term.draw(|f| app.render(f)).unwrap();
            if maximise {
                app.toggle_terminal_maximize();
                term.draw(|f| app.render(f)).unwrap();
            }
            app.focus_pane(Pane::Terminal);
            let probe = copy_terminal_word(&mut app);
            // Switch back to editor + open find.
            app.focus_pane(Pane::Editor);
            app.open_editor_find();
            term.draw(|f| app.render(f)).unwrap();
            let clip = crate::clipboard::read_string().unwrap();
            assert_eq!(clip, probe);
            app.handle_paste(&clip);
            let q = app
                .editor_find
                .as_ref()
                .map(|s| s.query.clone())
                .unwrap_or_default();
            assert!(
                q.contains(&probe),
                "terminal copy -> editor-find paste must work in {} layout; got query={q:?}",
                if maximise { "maximised" } else { "split" }
            );
        }

        // ---- Destination: editor body ----
        {
            let _g = crate::clipboard::lock_clipboard_for_test();
            let tmp = tempfile::tempdir().unwrap();
            let f = tmp.path().join("probe.txt");
            std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
            let mut app = App::new(tmp.path().to_path_buf()).unwrap();
            app.editor.open_pinned(&f).unwrap();
            let backend = ratatui::backend::TestBackend::new(140, 50);
            let mut term = ratatui::Terminal::new(backend).unwrap();
            term.draw(|f| app.render(f)).unwrap();
            if maximise {
                app.toggle_terminal_maximize();
                term.draw(|f| app.render(f)).unwrap();
            }
            app.focus_pane(Pane::Terminal);
            let probe = copy_terminal_word(&mut app);
            app.focus_pane(Pane::Editor);
            term.draw(|f| app.render(f)).unwrap();
            let clip = crate::clipboard::read_string().unwrap();
            assert_eq!(clip, probe);
            app.handle_paste(&clip);
            assert!(
                app.editor.lines.iter().any(|l| l.contains(&probe)),
                "terminal copy -> editor body paste must work in {} layout; lines={:?}",
                if maximise { "maximised" } else { "split" },
                app.editor.lines
            );
        }

        // ---- Destination: source-control commit box ----
        {
            let _g = crate::clipboard::lock_clipboard_for_test();
            let tmp = tempfile::tempdir().unwrap();
            let mut app = App::new(tmp.path().to_path_buf()).unwrap();
            let backend = ratatui::backend::TestBackend::new(140, 50);
            let mut term = ratatui::Terminal::new(backend).unwrap();
            term.draw(|f| app.render(f)).unwrap();
            if maximise {
                app.toggle_terminal_maximize();
                term.draw(|f| app.render(f)).unwrap();
            }
            app.focus_pane(Pane::Terminal);
            let probe = copy_terminal_word(&mut app);
            app.set_sidebar_view(SidebarView::SourceControl);
            term.draw(|f| app.render(f)).unwrap();
            let clip = crate::clipboard::read_string().unwrap();
            assert_eq!(clip, probe);
            app.handle_paste(&clip);
            assert!(
                app.source_control.message.contains(&probe),
                "terminal copy -> Source Control paste must work in {} layout; msg={:?}",
                if maximise { "maximised" } else { "split" },
                app.source_control.message
            );
        }
    }
}

/// User-reported root cause: with the Search sidebar open, Cmd+C /
/// Cmd+V in the terminal silently no-op because the search-editing
/// shortcut guard fired on `focus != Editor`, swallowing the keys
/// into the (empty) search query instead of the focused terminal.
/// Copying from the terminal must work regardless of which sidebar
/// view is showing.
#[test]
fn terminal_cmd_c_copies_even_when_search_sidebar_is_open() {
    let _g = crate::clipboard::lock_clipboard_for_test();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    term.draw(|f| app.render(f)).unwrap();

    let probe = format!("croft-search-open-{}", std::process::id());
    app.terminal_mut()
        .write_input(format!("printf '{probe}\\n'\r").as_bytes());
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_millis(3000) {
        if app.terminal().visible_text().contains(&probe) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    term.draw(|f| app.render(f)).unwrap();
    let snap = app.terminal().visible_text();
    let row_idx = snap.lines().position(|l| l.contains(&probe)).unwrap();
    let line = snap.lines().nth(row_idx).unwrap();
    let col_start = line.find(&probe).unwrap();
    let col_end = col_start + probe.len() - 1;
    let inner = app.terminal().last_inner;
    let screen_y = inner.y + row_idx as u16;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
        column: inner.x + col_start as u16,
        row: screen_y,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(MouseButton::Left),
        column: inner.x + col_end as u16,
        row: screen_y,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(MouseButton::Left),
        column: inner.x + col_end as u16,
        row: screen_y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.focus == Pane::Terminal,
        "terminal click must focus terminal"
    );
    app.handle_key(key(KeyCode::Char('c'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        crate::clipboard::read_string().as_deref(),
        Some(probe.as_str()),
        "Cmd+C in the terminal must copy the terminal selection even when the Search sidebar is the active view"
    );
    assert!(
        app.search.query.is_empty(),
        "Cmd+C in the terminal must NOT leak into the search query; query={:?}",
        app.search.query
    );
}

#[test]
fn ctrl_j_clears_maximize_when_hiding_terminal() {
    // Mental contract: Ctrl+J means "give me my normal layout back."
    // Hiding the terminal must clear the maximize flag so the next
    // Ctrl+J reopen restores the editor+terminal split, not a stuck
    // maximized terminal.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let max_chord = key(
        KeyCode::Char('J'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    let toggle_chord = key(KeyCode::Char('j'), KeyModifiers::CONTROL);
    app.handle_key(max_chord).unwrap();
    assert!(app.terminal_maximized);
    app.handle_key(toggle_chord).unwrap();
    assert!(!app.show_terminal, "Ctrl+J hides the terminal");
    assert!(
        !app.terminal_maximized,
        "hiding the terminal also clears maximize"
    );
    app.handle_key(toggle_chord).unwrap();
    assert!(app.show_terminal, "Ctrl+J reshows the terminal");
    assert!(
        !app.terminal_maximized,
        "reopened terminal stays in normal split, not maximized"
    );
}

#[test]
fn maximized_terminal_collapses_editor_in_render_layout() {
    // Drive an actual frame and assert the editor pane received zero
    // height while the terminal occupies the full right column. This
    // pins the user-visible behavior, not just the flag.
    use ratatui::{Terminal, backend::TestBackend};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let max_chord = key(
        KeyCode::Char('J'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    app.handle_key(max_chord).unwrap();
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(
        app.editor.last_area.height, 0,
        "maximized terminal must collapse editor to zero rows"
    );
    assert!(
        app.editor.last_full_area.width > 0,
        "editor still tracks its column for hit-testing"
    );
}

#[test]
fn maximizing_terminal_requests_welcome_image_clear_when_image_was_displayed() {
    // Repro for "wordmark + icon bleed through the maximized terminal":
    // the welcome OSC-1337 image was painted at the editor pane's cells
    // before maximize. Maximizing collapses the editor to zero height
    // and the terminal pane now covers those cells, but iTerm2 still
    // has the image cached at the old cell positions. ratatui's text
    // writes do NOT evict that cache - only a one-shot terminal.clear()
    // does, gated by consume_welcome_image_clear().
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.welcome.set_displayed(true);
    let chord = key(
        KeyCode::Char('J'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    app.handle_key(chord).unwrap();
    assert!(app.terminal_maximized, "first chord maximizes");
    assert!(
        app.consume_welcome_image_clear(),
        "maximize must arm a one-shot welcome image clear"
    );
}

#[test]
fn maximizing_terminal_is_noop_for_welcome_clear_when_image_was_not_displayed() {
    // No prior emit means iTerm has no cached cells to evict; the clear
    // chain must stay silent so the main loop doesn't pointlessly
    // CSI-2J the screen every time the user toggles maximize before
    // they ever land on the welcome panel.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.welcome.set_displayed(false);
    let chord = key(
        KeyCode::Char('J'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    app.handle_key(chord).unwrap();
    assert!(app.terminal_maximized);
    assert!(
        !app.consume_welcome_image_clear(),
        "maximize must NOT arm a clear when no image was ever emitted"
    );
}

#[test]
fn restoring_terminal_from_maximize_rearms_welcome_overlay_dirty() {
    // After a clear, the post-draw emitter re-paints the welcome only
    // when welcome_overlay_dirty is true. render_welcome only sets it
    // when the layout *changes*, so if the restored editor area is
    // bit-identical to the pre-maximize area the dirty flag would
    // stay false and the wordmark would never come back. Exit must
    // arm it explicitly.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let chord = key(
        KeyCode::Char('J'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    app.handle_key(chord).unwrap();
    assert!(app.terminal_maximized);
    app.overlays.welcome.set_dirty(false);
    app.handle_key(chord).unwrap();
    assert!(!app.terminal_maximized);
    assert!(
        app.overlays.welcome.is_dirty(),
        "exiting maximize must re-arm welcome_overlay_dirty so the wordmark re-paints"
    );
}

#[test]
fn delete_key_is_recognized_as_delete_node() {
    assert!(is_delete_node_key(key(KeyCode::Delete, KeyModifiers::NONE)));
}

#[test]
fn cmd_backspace_is_recognized_as_delete_node() {
    // macOS Finder convention: ⌘⌫ moves the selection to the Trash.
    assert!(is_delete_node_key(key(
        KeyCode::Backspace,
        KeyModifiers::SUPER
    )));
}

#[test]
fn plain_backspace_is_delete_node_on_mac_layouts() {
    // On Mac keyboards the key labeled "delete" reports as Backspace; in
    // the tree pane (the only context this helper is called from) it must
    // trigger deletion to match the user's expectation.
    assert!(is_delete_node_key(key(
        KeyCode::Backspace,
        KeyModifiers::NONE
    )));
}

#[test]
fn ctrl_d_is_not_delete_node() {
    assert!(!is_delete_node_key(key(
        KeyCode::Char('d'),
        KeyModifiers::CONTROL
    )));
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
fn set_title_seq_wraps_with_osc0_and_bel() {
    let bytes = set_title_seq("croft");
    assert_eq!(bytes, b"\x1b]0;croft\x07");
}

#[test]
fn set_title_seq_handles_empty_title() {
    let bytes = set_title_seq("");
    assert_eq!(bytes, b"\x1b]0;\x07");
}

#[test]
fn set_title_seq_passes_through_unicode() {
    let bytes = set_title_seq("croft  README.md");
    assert_eq!(bytes, "\x1b]0;croft  README.md\x07".as_bytes());
}

#[test]
fn set_title_seq_strips_control_bytes_that_would_break_the_escape() {
    // BEL terminates the OSC, ESC starts a new sequence — both must be filtered.
    let bytes = set_title_seq("evil\x07file");
    assert_eq!(bytes, b"\x1b]0;evilfile\x07");
    let bytes = set_title_seq("evil\x1bfile");
    assert_eq!(bytes, b"\x1b]0;evilfile\x07");
    let bytes = set_title_seq("evil\nfile");
    assert_eq!(bytes, b"\x1b]0;evilfile\x07");
}

#[test]
fn app_name_constant_is_croft() {
    assert_eq!(APP_NAME, "croft");
}

#[test]
fn set_session_bg_srgb_seq_uses_iterm_osc1337_and_srgb_prefix() {
    // iTerm2 must read the bg as sRGB so the OSC-1337 PNG canvas (also
    // sRGB) matches pixel-for-pixel. Locking in the exact bytes catches
    // any future refactor that drops the `srgb:` prefix or the iTerm2
    // OSC introducer.
    let seq = set_session_bg_srgb_seq(crate::theme::Theme::DarkBlue.editor_bg_rgb());
    assert_eq!(seq, "\x1b]1337;SetColors=bg=srgb:1e222e\x07");
}

#[test]
fn reset_session_bg_seq_restores_profile_default() {
    // On exit we must revert iTerm2's session bg so the user's shell
    // doesn't keep croft's forced bg colour.
    assert_eq!(reset_session_bg_seq(), "\x1b]1337;SetColors=bg=default\x07");
}

#[test]
fn welcome_logo_png_asset_is_present_and_decodable() {
    // Regression guard for the welcome screen: the bundled logo asset
    // must remain a valid PNG so the OSC-1337 path can bake it. If
    // someone replaces it with a half-block fallback or empties the
    // asset, this catches it on every CI run.
    let bytes = crate::iterm2_inline::WELCOME_LOGO_PNG;
    assert!(bytes.len() > 1024, "welcome logo asset suspiciously small");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG file");
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .expect("welcome logo PNG must decode");
    assert!(img.width() > 0 && img.height() > 0);
}

#[test]
fn welcome_image_bake_produces_osc1337_carrying_logo_pixels() {
    // End-to-end regression for the welcome render path: feeding the
    // bundled logo PNG through the same `fit_image` + `build_inline_image_osc`
    // pipeline `render_welcome` uses must yield an OSC-1337 sequence
    // that (a) starts with the iTerm2 introducer, (b) advertises the
    // requested cell dimensions, and (c) carries an opaque canvas
    // filled with `EDITOR_BG_RGB`. If anyone swaps OSC-1337 for a
    // half-block or text fallback, this test will refuse to compile
    // or fail the byte assertions.
    let canvas_w = 48u32 * 8; // approx welcome cell w * cell pixel w
    let canvas_h = 14u32 * 16;
    let bg_rgb = crate::theme::Theme::DarkBlue.editor_bg_rgb();
    let bg = image::Rgba([bg_rgb.0, bg_rgb.1, bg_rgb.2, 0xff]);
    let baked = crate::iterm2_inline::fit_image(
        crate::iterm2_inline::WELCOME_LOGO_PNG,
        canvas_w,
        canvas_h,
        bg,
    )
    .expect("baked welcome PNG");
    assert_eq!(
        &baked[..8],
        b"\x89PNG\r\n\x1a\n",
        "fit_image must emit a PNG"
    );
    let osc = crate::iterm2_inline::build_inline_image_osc(&baked, 48, 14, false);
    assert!(osc.starts_with("\x1b]1337;File=inline=1"));
    assert!(osc.ends_with('\x07'));
    assert!(osc.contains("width=48"));
    assert!(osc.contains("height=14"));

    // Verify the canvas corner pixels are exactly the dark-blue theme bg so
    // the welcome image bg matches the sRGB-decoded session bg.
    let img = image::load_from_memory_with_format(&baked, image::ImageFormat::Png)
        .unwrap()
        .to_rgba8();
    for &(x, y) in &[
        (0u32, 0u32),
        (canvas_w - 1, 0),
        (0, canvas_h - 1),
        (canvas_w - 1, canvas_h - 1),
    ] {
        let p = img.get_pixel(x, y);
        assert_eq!(
            (p.0[0], p.0[1], p.0[2], p.0[3]),
            (bg_rgb.0, bg_rgb.1, bg_rgb.2, 0xff),
            "canvas corner ({x}, {y}) must be opaque editor bg"
        );
    }
}

#[test]
fn brand_pill_uses_app_name_constant() {
    assert_eq!(brand_pill_text(), " croft ");
}

#[test]
fn keyboard_enhancement_flags_request_disambiguate_escape_codes() {
    let f = keyboard_enhancement_flags();
    assert!(
        f.contains(crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        "Cmd / modifier delivery requires DISAMBIGUATE_ESCAPE_CODES"
    );
}

#[test]
fn keyboard_enhancement_flags_do_not_force_all_keys_as_escapes() {
    // REPORT_ALL_KEYS_AS_ESCAPE_CODES rewrites plain printable input.
    // We only need the disambiguation bit, not the all-keys bit.
    let f = keyboard_enhancement_flags();
    assert!(
        !f.contains(crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES),
        "we should not opt into REPORT_ALL_KEYS_AS_ESCAPE_CODES"
    );
}

#[test]
fn status_bar_advertises_terminal_toggle_shortcut() {
    let src = include_str!("mod.rs");
    assert!(
        src.contains("\"^j\""),
        "status bar should advertise ^j as the terminal-toggle shortcut"
    );
    assert!(
        src.contains("Span::raw(\" Term"),
        "status bar should label the ^j shortcut 'Term' (matched a `Span::raw(\" Term` prefix so cosmetic trailing-whitespace tweaks do not break the assertion)"
    );
}

#[test]
fn no_stale_tcode_pill_literal_in_source() {
    let src = include_str!("mod.rs");
    assert!(
        !src.contains("\" tcode \""),
        "stale ` tcode ` pill literal still present in src/app/mod.rs"
    );
}

#[test]
fn build_title_uses_basename_and_app_name() {
    let p = std::path::Path::new("/Users/somebody/projects/croft");
    assert_eq!(build_title(p), "croft  croft");
    let p = std::path::Path::new("/");
    assert_eq!(build_title(p), "croft  /");
}

fn file_node(path: &Path) -> crate::widgets::file_tree::Node {
    crate::widgets::file_tree::Node {
        path: path.to_path_buf(),
        depth: 1,
        is_dir: false,
        expanded: false,
        loaded: false,
    }
}

fn dir_node(path: &Path) -> crate::widgets::file_tree::Node {
    crate::widgets::file_tree::Node {
        path: path.to_path_buf(),
        depth: 1,
        is_dir: true,
        expanded: false,
        loaded: false,
    }
}

#[test]
fn tree_context_menu_on_file_offers_new_file_new_folder_then_cut_copy_rename_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hello.txt");
    std::fs::write(&f, "hi").unwrap();
    let n = file_node(&f);
    let target = f.parent().unwrap().to_path_buf();
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&f),
        &target,
        None,
        None,
    );
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        labels,
        [
            "New File",
            "New Folder",
            "Cut",
            "Copy",
            "Rename",
            "Select for Compare",
            "Delete",
        ],
    );
    assert!(matches!(&items[0].1, MenuAction::Create(CreateKind::File)));
    assert!(matches!(
        &items[1].1,
        MenuAction::Create(CreateKind::Folder)
    ));
    assert!(matches!(&items[2].1, MenuAction::Cut(ps) if ps == &vec![f.clone()]));
    assert!(matches!(&items[3].1, MenuAction::Copy(ps) if ps == &vec![f.clone()]));
    assert!(matches!(&items[4].1, MenuAction::Rename(p) if p == &f));
    assert!(matches!(&items[5].1, MenuAction::SelectForCompare(p) if p == &f));
    assert!(matches!(&items[6].1, MenuAction::Delete { paths } if paths == &vec![f.clone()]));
}

#[test]
fn tree_context_menu_adds_reveal_in_finder_when_local_macos() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hello.txt");
    std::fs::write(&f, "hi").unwrap();
    let n = file_node(&f);
    let target = f.parent().unwrap().to_path_buf();
    let mut items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&f),
        &target,
        None,
        None,
    );
    maybe_add_reveal_in_finder(&mut items, Some(f.as_path()), true);
    // Lands directly beneath the New File / New Folder block, mirroring VS
    // Code's top "reveal" placement.
    assert!(matches!(
        &items[2].1,
        MenuAction::RevealInFinder(p) if p == &f
    ));
    assert_eq!(items[2].0, "Reveal in Finder");
}

#[test]
fn tree_context_menu_omits_reveal_in_finder_when_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hello.txt");
    std::fs::write(&f, "hi").unwrap();
    let n = file_node(&f);
    let target = f.parent().unwrap().to_path_buf();
    let mut items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&f),
        &target,
        None,
        None,
    );
    // A remote SSH session disables Finder: the entry must not appear at all,
    // not even as a disabled no-op.
    maybe_add_reveal_in_finder(&mut items, Some(f.as_path()), false);
    assert!(
        !items
            .iter()
            .any(|(_, a)| matches!(a, MenuAction::RevealInFinder(_)))
    );
}

#[test]
fn tree_context_menu_omits_reveal_in_finder_when_no_entry_clicked() {
    let tmp = tempfile::tempdir().unwrap();
    let mut items = build_tree_context_menu_items(None, tmp.path(), &[], tmp.path(), None, None);
    // Right-clicking empty tree space has no path to reveal even on local macOS.
    maybe_add_reveal_in_finder(&mut items, None, true);
    assert!(
        !items
            .iter()
            .any(|(_, a)| matches!(a, MenuAction::RevealInFinder(_)))
    );
}

#[test]
fn reveal_in_finder_shortcut_hint_is_opt_cmd_r() {
    let action = MenuAction::RevealInFinder(PathBuf::from("/tmp/x"));
    assert_eq!(shortcut_for(&action), Some("⌥⌘R"));
}

#[test]
fn reveal_in_finder_key_is_opt_cmd_r_and_disjoint_from_rename() {
    // Cmd+Opt+R (and Ctrl+Opt+R) reveal in Finder.
    assert!(is_tree_reveal_in_finder_key(key(
        KeyCode::Char('r'),
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
    assert!(is_tree_reveal_in_finder_key(key(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL | KeyModifiers::ALT
    )));
    // Plain Cmd+R must NOT trigger reveal - that chord is Rename.
    assert!(!is_tree_reveal_in_finder_key(key(
        KeyCode::Char('r'),
        KeyModifiers::SUPER
    )));
    // And Cmd+Opt+R must NOT trigger Rename, so the two R chords never collide.
    assert!(!is_tree_rename_key(key(
        KeyCode::Char('r'),
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
}

#[test]
fn explorer_shortcut_predicates_recognise_expected_keys() {
    assert!(is_tree_new_file_key(key(
        KeyCode::Char('f'),
        KeyModifiers::SUPER
    )));
    assert!(is_tree_new_file_key(key(
        KeyCode::Char('F'),
        KeyModifiers::CONTROL
    )));
    assert!(!is_tree_new_file_key(key(
        KeyCode::Char('f'),
        KeyModifiers::NONE
    )));
    // Shift+Super disqualifies New File so Cmd+Shift+F can fall through to
    // the global Search jump (New Folder now lives on Cmd+Shift+N).
    assert!(!is_tree_new_file_key(key(
        KeyCode::Char('F'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));

    assert!(is_tree_new_folder_key(key(
        KeyCode::Char('N'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
    assert!(is_tree_new_folder_key(key(
        KeyCode::Char('N'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
    assert!(!is_tree_new_folder_key(key(
        KeyCode::Char('n'),
        KeyModifiers::SUPER
    )));
    assert!(
        !is_tree_new_folder_key(key(
            KeyCode::Char('F'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT
        )),
        "Cmd+Shift+F is no longer the New Folder chord — it must fall through to the global Search jump"
    );

    assert!(is_tree_rename_key(key(
        KeyCode::Char('r'),
        KeyModifiers::SUPER
    )));
    assert!(is_tree_rename_key(key(KeyCode::F(2), KeyModifiers::NONE)));
    assert!(!is_tree_rename_key(key(
        KeyCode::Char('r'),
        KeyModifiers::NONE
    )));

    assert!(is_tree_make_root_key(key(
        KeyCode::Char('/'),
        KeyModifiers::SUPER
    )));
    assert!(is_tree_make_root_key(key(
        KeyCode::Char('/'),
        KeyModifiers::CONTROL
    )));
    assert!(!is_tree_make_root_key(key(
        KeyCode::Char('/'),
        KeyModifiers::NONE
    )));

    let cmd_shift = KeyModifiers::SUPER | KeyModifiers::SHIFT;
    let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    assert!(is_tree_make_parent_root_key(key(
        KeyCode::Char('/'),
        cmd_shift
    )));
    assert!(is_tree_make_parent_root_key(key(
        KeyCode::Char('/'),
        ctrl_shift
    )));
    assert!(is_tree_make_parent_root_key(key(
        KeyCode::Char('?'),
        KeyModifiers::SUPER
    )));
    assert!(is_tree_make_parent_root_key(key(
        KeyCode::Char('?'),
        KeyModifiers::CONTROL
    )));
    assert!(is_tree_make_parent_root_key(key(
        KeyCode::Char('?'),
        cmd_shift
    )));
    assert!(!is_tree_make_parent_root_key(key(
        KeyCode::Char('/'),
        KeyModifiers::SUPER
    )));
    assert!(!is_tree_make_parent_root_key(key(
        KeyCode::Char('/'),
        KeyModifiers::NONE
    )));
    assert!(!is_tree_make_parent_root_key(key(
        KeyCode::Char('?'),
        KeyModifiers::NONE
    )));
    assert!(!is_tree_make_root_key(key(KeyCode::Char('/'), cmd_shift)));
}

#[test]
fn tree_typeahead_first_keystroke_jumps_to_first_matching_visible_node() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("apple")).unwrap();
    std::fs::write(tmp.path().join("banana.txt"), "").unwrap();
    std::fs::write(tmp.path().join("cherry.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.apply_tree_typeahead('b', std::time::Instant::now());
    let picked = app.tree.nodes[app.tree.selected]
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(picked, "banana.txt", "typing 'b' must select banana.txt");
}

#[test]
fn tree_typeahead_accumulates_prefix_within_window() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("test")).unwrap();
    std::fs::create_dir(tmp.path().join("toolbox")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let t0 = std::time::Instant::now();
    app.apply_tree_typeahead('t', t0);
    app.apply_tree_typeahead('e', t0 + std::time::Duration::from_millis(100));
    let name = app.tree.nodes[app.tree.selected]
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        name, "test",
        "'te' typed within the window must land on 'test', not 'toolbox'"
    );
}

#[test]
fn tree_typeahead_resets_after_window_expires_and_cycles() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("test")).unwrap();
    std::fs::create_dir(tmp.path().join("toolbox")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let t0 = std::time::Instant::now();
    app.apply_tree_typeahead('t', t0);
    let first = app.tree.selected;
    let first_name = app.tree.nodes[first]
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    app.apply_tree_typeahead('t', t0 + std::time::Duration::from_millis(1000));
    let second = app.tree.selected;
    let second_name = app.tree.nodes[second]
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_ne!(
        first, second,
        "the second 't' after the window expires must cycle to the next 't' match"
    );
    assert!(
        first_name.starts_with('t') && second_name.starts_with('t'),
        "both cycle stops must start with 't'"
    );
}

#[test]
fn tree_typeahead_is_case_insensitive() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("README.md"), "").unwrap();
    std::fs::write(tmp.path().join("zzz.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.apply_tree_typeahead('r', std::time::Instant::now());
    let picked = app.tree.nodes[app.tree.selected]
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(picked, "README.md");
}

#[test]
fn tree_typeahead_via_handle_tree_key_jumps_selection() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("apple")).unwrap();
    std::fs::write(tmp.path().join("banana.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Tree);
    app.sidebar_view = SidebarView::Explorer;
    app.handle_tree_key(key(KeyCode::Char('b'), KeyModifiers::NONE));
    let picked = app.tree.nodes[app.tree.selected]
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        picked, "banana.txt",
        "a plain letter routed through handle_tree_key must drive typeahead"
    );
}

#[test]
fn tree_typeahead_does_not_fire_when_modifier_held() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("banana.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let before = app.tree.selected;
    app.focus_pane(Pane::Tree);
    app.sidebar_view = SidebarView::Explorer;
    app.handle_tree_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert_eq!(
        app.tree.selected, before,
        "Ctrl+letter must not be consumed as typeahead"
    );
    assert!(
        app.tree_typeahead.is_none(),
        "Ctrl-modified keys must not seed the typeahead buffer"
    );
}

#[test]
fn cmd_shift_n_in_explorer_pane_creates_a_new_folder() {
    // With Explorer focused, Cmd+Shift+N opens the New Folder prompt
    // (the user-requested binding, moved off Cmd+Shift+F so that chord
    // can always reach the global Search jump).
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Tree);
    app.sidebar_view = SidebarView::Explorer;
    app.handle_key(key(
        KeyCode::Char('N'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        matches!(
            app.prompt.as_ref(),
            Some(Prompt {
                kind: PromptKind::Create(CreateKind::Folder),
                ..
            })
        ),
        "Explorer-focused Cmd+Shift+N must open a New Folder prompt"
    );
    assert_eq!(
        app.sidebar_view,
        SidebarView::Explorer,
        "must NOT jump to Search"
    );
}

#[test]
fn cmd_shift_f_in_explorer_pane_jumps_to_search_not_new_folder() {
    // Cmd+Shift+F is no longer the Explorer's New Folder chord — it
    // now always jumps to the Search sidebar, even when Explorer is
    // focused. New Folder lives on Cmd+Shift+N.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Tree);
    app.sidebar_view = SidebarView::Explorer;
    app.handle_key(key(
        KeyCode::Char('F'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        app.prompt.is_none(),
        "Cmd+Shift+F must NOT open the New Folder prompt anymore"
    );
    assert_eq!(app.sidebar_view, SidebarView::Search);
}

#[test]
fn cmd_shift_f_outside_explorer_still_jumps_to_search() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.sidebar_view = SidebarView::Explorer;
    app.handle_key(key(
        KeyCode::Char('F'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.sidebar_view, SidebarView::Search);
}

#[test]
fn shortcut_for_returns_expected_shortcuts_for_each_explorer_action() {
    let p = std::path::PathBuf::from("/x");
    assert_eq!(
        shortcut_for(&MenuAction::Create(CreateKind::File)),
        Some("⌘F")
    );
    assert_eq!(
        shortcut_for(&MenuAction::Create(CreateKind::Folder)),
        Some("⌘⇧N")
    );
    assert_eq!(shortcut_for(&MenuAction::Cut(vec![p.clone()])), Some("⌘X"));
    assert_eq!(shortcut_for(&MenuAction::Copy(vec![p.clone()])), Some("⌘C"));
    assert_eq!(shortcut_for(&MenuAction::Paste(p.clone())), Some("⌘V"));
    assert_eq!(shortcut_for(&MenuAction::Rename(p.clone())), Some("⌘R"));
    assert_eq!(shortcut_for(&MenuAction::MakeRoot(p.clone())), Some("⌘/"));
    assert_eq!(
        shortcut_for(&MenuAction::Delete {
            paths: vec![p.clone()]
        }),
        Some("⌫")
    );
    // Compare actions have no keybinding (no shortcut shown).
    assert_eq!(shortcut_for(&MenuAction::SelectForCompare(p)), None);
}

#[test]
fn tree_context_menu_on_subfolder_offers_make_root_between_rename_and_delete() {
    // "Make root" should appear on folder right-click so the user can
    // re-root the workspace into that folder (e.g., a nested git repo).
    // It must NOT appear on file right-clicks - files can't be a
    // workspace root.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("project_a");
    std::fs::create_dir(&d).unwrap();
    let n = dir_node(&d);
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&d),
        &d,
        None,
        None,
    );
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert!(
        labels.contains(&"Make root"),
        "folder right-click must include Make root, got {labels:?}"
    );
    let mr = items
        .iter()
        .find_map(|(_, a)| match a {
            MenuAction::MakeRoot(p) => Some(p),
            _ => None,
        })
        .expect("Make root action must carry the folder path");
    assert_eq!(mr, &d);
}

#[test]
fn tree_context_menu_on_file_does_not_offer_make_root() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hello.txt");
    std::fs::write(&f, "hi").unwrap();
    let n = file_node(&f);
    let target = f.parent().unwrap().to_path_buf();
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&f),
        &target,
        None,
        None,
    );
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert!(
        !labels.contains(&"Make root"),
        "files cannot be roots; Make root must be hidden for files: {labels:?}"
    );
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
    assert_eq!(app.workspace_root, inner);
    assert_eq!(app.tree.root, inner);
}

#[test]
fn changing_workspace_root_retargets_search_at_the_new_explorer_root() {
    let tmp = tempfile::tempdir().unwrap();
    let inner = tmp.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.search.hits.push(crate::widgets::search::SearchHit {
        path: tmp.path().join("stale.txt"),
        line_no: 1,
        line_text: String::from("stale"),
    });

    app.change_workspace_root(inner.clone());

    assert_eq!(
        app.search.root, inner,
        "the search panel must point at the Explorer's new root so its target and the path-stripping match"
    );
    assert!(
        app.search.hits.is_empty(),
        "hits from the previous root must be dropped when the Explorer root changes"
    );
}

#[test]
fn cmd_shift_slash_on_a_folder_changes_workspace_root_to_that_folder_s_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let mid = tmp.path().join("mid");
    let leaf = mid.join("leaf");
    std::fs::create_dir_all(&leaf).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mid_idx = app.tree.nodes.iter().position(|n| n.path == mid).unwrap();
    app.tree.selected = mid_idx;
    app.tree.expand_selected();
    let leaf_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path == leaf)
        .expect("leaf must be visible after expanding mid/");
    app.tree.selected = leaf_idx;
    app.focus = Pane::Tree;
    app.sidebar_view = SidebarView::Explorer;

    let handled = app.handle_explorer_shortcut(key(KeyCode::Char('?'), KeyModifiers::SUPER));

    assert!(handled);
    assert_eq!(app.workspace_root, mid);
    assert_eq!(app.tree.root, mid);
}

#[test]
fn cmd_shift_slash_on_the_tree_root_walks_up_one_filesystem_level() {
    let tmp = tempfile::tempdir().unwrap();
    let inner = tmp.path().join("inner");
    std::fs::create_dir(&inner).unwrap();
    let mut app = App::new(inner.clone()).unwrap();
    app.tree.selected = 0;
    app.focus = Pane::Tree;
    app.sidebar_view = SidebarView::Explorer;

    let handled = app.handle_explorer_shortcut(key(
        KeyCode::Char('/'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ));

    assert!(handled);
    assert_eq!(app.workspace_root, tmp.path());
    assert_eq!(app.tree.root, tmp.path());
}

#[test]
fn dispatch_make_root_changes_tree_root_and_workspace_root() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project_b");
    std::fs::create_dir(&project).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let original_root = app.tree.root.clone();
    app.dispatch_menu_action(MenuAction::MakeRoot(project.clone()), project.clone());
    assert_ne!(app.tree.root, original_root);
    assert_eq!(app.tree.root, project);
    assert_eq!(app.workspace_root, project);
    // The tree's root node must reflect the new path.
    assert_eq!(app.tree.nodes[0].path, project);
}

#[test]
fn change_workspace_root_writes_cd_into_active_terminal_pty() {
    let tmp = tempfile::tempdir().unwrap();
    let target_dir = tmp.path().join("seeded");
    std::fs::create_dir(&target_dir).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Clear the freshly-spawned shell's "first paint" dirty bit so
    // the next take_dirty() exclusively reflects bytes written by
    // change_workspace_root itself. change_workspace_root performs
    // no other write_input on the terminal, so a dirty flag after
    // the call can only have come from the change_cwd seed.
    let _ = app.terminal_mut().take_dirty();
    app.change_workspace_root(target_dir.clone());

    assert!(
        app.terminal_mut().take_dirty(),
        "change_workspace_root must seed the active terminal's PTY with a cd command so the shell follows the Explorer; nothing was written"
    );
    assert_eq!(
        app.workspace_root, target_dir,
        "workspace_root must still flip to the new path"
    );
}

#[test]
fn change_workspace_root_skips_cd_when_terminal_runs_a_foreground_app() {
    let tmp = tempfile::tempdir().unwrap();
    let target_dir = tmp.path().join("seeded");
    std::fs::create_dir(&target_dir).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Put the active terminal into a long-lived foreground command so its
    // foreground process group is the command, not the shell.
    app.terminal_mut().write_input(b"sleep 10\n");
    let mut waited = 0u32;
    while waited < 4000 && app.terminal_mut().foreground_is_shell() {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert!(
        !app.terminal_mut().foreground_is_shell(),
        "precondition: the terminal must be running a foreground command"
    );

    app.change_workspace_root(target_dir.clone());

    // The "terminal busy" status is set by the very branch that skips
    // change_cwd, so it is a deterministic proxy for "no cd was injected"
    // - far more reliable than the async PTY dirty bit, which the running
    // sleep and residual echo can flip independently of any cd seed.
    assert!(
        app.status.contains("terminal busy"),
        "a busy terminal must take the suppress-cd branch and say so; injecting a cd would type the path into the running app. status was: {}",
        app.status
    );
    assert_eq!(
        app.workspace_root, target_dir,
        "the Explorer root must still flip even when the shell cannot be synced"
    );
}

#[test]
fn tree_context_menu_on_subfolder_offers_new_file_new_folder_then_cut_copy_rename_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("sub");
    std::fs::create_dir(&d).unwrap();
    let n = dir_node(&d);
    let target = d.clone();
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&d),
        &target,
        None,
        None,
    );
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        labels,
        [
            "New File",
            "New Folder",
            "Cut",
            "Copy",
            "Rename",
            "Make root",
            "Delete"
        ],
    );
}

#[test]
fn tree_context_menu_with_clipboard_offers_paste_on_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("sub");
    std::fs::create_dir(&d).unwrap();
    let n = dir_node(&d);
    let clip = ExplorerClipboard {
        mode: ExplorerClipMode::Cut,
        paths: vec![tmp.path().join("a.txt")],
    };
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&d),
        &d,
        Some(&clip),
        None,
    );
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        labels,
        [
            "New File",
            "New Folder",
            "Cut",
            "Copy",
            "Paste",
            "Rename",
            "Make root",
            "Delete"
        ],
    );
}

#[test]
fn tree_context_menu_on_a_deeply_nested_folder_creates_inside_that_folder() {
    // Regression for the user-reported "right-click on nested
    // folders only shows Cut/Copy/Rename/Delete; I cannot create a
    // new file or folder inside the folder I clicked". The menu
    // must surface New File… / New Folder…, and the resolved
    // target_dir must be the right-clicked folder itself so the
    // create lands inside it (not at the workspace root).
    let tmp = tempfile::tempdir().unwrap();
    let nested = tmp.path().join("a").join("b").join("c");
    std::fs::create_dir_all(&nested).unwrap();
    let n = dir_node(&nested);
    let resolved_target = crate::widgets::file_tree::create_target_dir_for(Some(&n), tmp.path());
    assert_eq!(
        resolved_target, nested,
        "right-click on a nested folder must target the folder itself, not the workspace root",
    );
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&nested),
        &resolved_target,
        None,
        None,
    );
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert!(
        labels.contains(&"New File"),
        "nested-folder right-click must offer New File…; labels = {labels:?}",
    );
    assert!(
        labels.contains(&"New Folder"),
        "nested-folder right-click must offer New Folder…; labels = {labels:?}",
    );
}

#[test]
fn arrow_and_pageup_pagedown_scroll_the_diff_view() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    // 30 distinct lines so the diff has plenty of rows to scroll.
    let left: String = (0..30).map(|i| format!("left-{i}\n")).collect();
    let right: String = (0..30).map(|i| format!("right-{i}\n")).collect();
    std::fs::write(&f1, left).unwrap();
    std::fs::write(&f2, right).unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    // Pretend the editor inner is 12 rows tall (page = 10 after the
    // header + footer reservation).
    app.editor.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 12,
    };
    app.focus_pane(Pane::Editor);

    // Down once → +1.
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 1);
    // PageDown → +page (10).
    app.handle_key(key(KeyCode::PageDown, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 11);
    // Up once → -1.
    app.handle_key(key(KeyCode::Up, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 10);
    // PageUp → -page.
    app.handle_key(key(KeyCode::PageUp, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 0);
    // Home / End.
    app.handle_key(key(KeyCode::End, KeyModifiers::NONE))
        .unwrap();
    let total = app.editor.diff.as_ref().unwrap().rows.len();
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll, total);
    app.handle_key(key(KeyCode::Home, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 0);
}

#[test]
fn ctrl_d_with_no_anchor_stashes_selected_file() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "v1").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path == f)
        .expect("file must appear in tree");
    app.tree.selected = idx;
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.compare_anchor.as_deref(), Some(f.as_path()));
}

#[test]
fn ctrl_d_again_on_same_file_clears_the_anchor() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "v1").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let idx = app.tree.nodes.iter().position(|n| n.path == f).unwrap();
    app.tree.selected = idx;
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .unwrap();
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.compare_anchor.is_none(), "second press toggles off");
}

#[test]
fn ctrl_d_with_anchor_on_other_file_opens_a_diff_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "alpha\nbravo\n").unwrap();
    std::fs::write(&b, "alpha\nBRAVO\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let a_idx = app.tree.nodes.iter().position(|n| n.path == a).unwrap();
    let b_idx = app.tree.nodes.iter().position(|n| n.path == b).unwrap();
    // Anchor a.txt.
    app.tree.selected = a_idx;
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .unwrap();
    // Move to b.txt and press again.
    app.tree.selected = b_idx;
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.compare_anchor.is_none(), "anchor consumed by compare");
    assert!(app.editor.diff.is_some(), "editor must now hold a diff tab");
}

#[test]
fn left_right_arrows_pan_diff_horizontally() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    let long_line: String = std::iter::repeat_n('x', 200).collect();
    std::fs::write(&f1, format!("{long_line}\n")).unwrap();
    std::fs::write(&f2, format!("{long_line}y\n")).unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    app.editor.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 12,
    };
    app.focus_pane(Pane::Editor);

    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll_x, 4);
    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll_x, 8);
    app.handle_key(key(KeyCode::Left, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll_x, 4);
    // Vertical Up/Down still go to diff.scroll, not scroll_x.
    let before_y = app.editor.diff.as_ref().unwrap().scroll;
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.diff.as_ref().unwrap().scroll >= before_y);
}

#[test]
fn mouse_horizontal_wheel_over_diff_pans_horizontally() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    let long: String = std::iter::repeat_n('z', 120).collect();
    std::fs::write(&f1, format!("{long}\n")).unwrap();
    std::fs::write(&f2, format!("{long}\n")).unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    app.editor.last_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 20,
    };
    app.editor.last_full_area = app.editor.last_area;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollRight,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll_x, 4);
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollLeft,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll_x, 0);
}

#[test]
fn dragging_editor_horizontal_scrollbar_pans_the_buffer() {
    // The editor paints a horizontal scrollbar on its bottom row when the
    // longest line overflows the text column. Clicking and dragging its thumb
    // must move scroll_col, and releasing must end the drag.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f = tmp.path().join("long.txt");
    let long: String = std::iter::repeat_n('z', 500).collect();
    std::fs::write(&f, format!("{long}\n")).unwrap();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);

    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let bar = app.editor.last_hscrollbar;
    assert!(
        bar.width > 0 && bar.height == 1,
        "an overflowing line must paint a horizontal scrollbar"
    );

    // Click-drag the thumb to the far-right end of the track.
    let right_x = bar.x + bar.width - 1;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: right_x,
        row: bar.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.editor.scroll_col > 0,
        "clicking the right of the track must pan right"
    );
    let after_down = app.editor.scroll_col;

    // Dragging back to the left edge pans back to column 0.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: bar.x,
        row: bar.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        app.editor.scroll_col, 0,
        "dragging to the left edge pans home"
    );
    assert_ne!(after_down, app.editor.scroll_col);

    // Release ends the drag so later moves don't keep panning.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: bar.x,
        row: bar.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        !app.editor_hscrollbar_drag,
        "releasing must clear the drag flag"
    );
}

#[test]
fn mouse_wheel_over_search_panel_scrolls_the_results_list() {
    // User report: search shows thousands of matches piling up but
    // the panel can't be scrolled with the wheel - the user is stuck
    // at the top. The result list must respond to ScrollUp/ScrollDown
    // when the cursor is over the side panel and Search is the
    // active view.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::Search;
    app.show_tree = true;
    // Pretend a render captured the search panel at this rect.
    app.search.last_area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    app.search.last_inner = Rect {
        x: 1,
        y: 1,
        width: 58,
        height: 28,
    };
    // 1000 hits, well past anything that can fit in 30 rows.
    app.search.hits = (0..1000)
        .map(|i| crate::widgets::search::SearchHit {
            path: tmp.path().join("a.txt"),
            line_no: i + 1,
            line_text: format!("line {i}"),
        })
        .collect();
    app.search.scroll = 0;
    app.search.selected = 0;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollDown,
        column: 30,
        row: 15,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.search.scroll > 0,
        "wheel-down on the search panel must advance the result list scroll"
    );
    let after_down = app.search.scroll;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollUp,
        column: 30,
        row: 15,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.search.scroll < after_down,
        "wheel-up must reduce the scroll back toward the top"
    );
}

#[test]
fn search_panel_paints_a_scrollbar_when_hits_exceed_visible_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::Search;
    app.show_tree = true;
    app.search.hits = (0..500)
        .map(|i| crate::widgets::search::SearchHit {
            path: tmp.path().join("a.txt"),
            line_no: i + 1,
            line_text: format!("line {i}"),
        })
        .collect();
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut app.search, area, &mut buf);
    assert!(
        app.search.last_scrollbar.width > 0 && app.search.last_scrollbar.height > 0,
        "scrollbar rect must be tracked when results overflow the viewport"
    );
}

#[test]
fn mouse_wheel_over_diff_scrolls_the_diff_not_the_text_buffer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    std::fs::write(&f1, "1\n2\n3\n4\n5\n6\n").unwrap();
    std::fs::write(&f2, "1\n2\n3\n4\n5\n6\n").unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    // Place the editor pane somewhere the mouse can hit it.
    app.editor.last_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 20,
    };
    app.editor.last_full_area = app.editor.last_area;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollDown,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 3);
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollUp,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 0);
}

#[test]
fn menu_item_at_handles_clipped_menu_so_clicks_dispatch_the_visible_row() {
    // Repro: with a 5-item menu (Cut, Copy, Rename…, Select for
    // Compare, Delete), if the user right-clicks low enough that
    // the menu must shift up by 1 to fit on screen, a click on the
    // visible "Select for Compare" row used to map to "Rename"
    // because hit-testing used the unclipped rect while rendering
    // used the clipped one.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Frame is 60 wide, 12 tall. Menu has 7 items + 2 borders = 9
    // rows. Origin at y=6 would make the menu run from row 6 to row
    // 14 (off-screen). The renderer (and hit-test) clamp y to
    // 12 - 9 = 3.
    app.last_frame_area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let f = tmp.path().join("file.txt");
    std::fs::write(&f, "x").unwrap();
    let n = crate::widgets::file_tree::Node {
        path: f.clone(),
        depth: 1,
        is_dir: false,
        expanded: false,
        loaded: true,
    };
    let target = f.parent().unwrap().to_path_buf();
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&f),
        &target,
        None,
        None,
    );
    // Sanity: items[5] is "Select for Compare" after the New
    // File… / New Folder… prefix lifted everything down by two.
    assert!(matches!(&items[5].1, MenuAction::SelectForCompare(_)));
    app.context_menu = Some(ContextMenu {
        origin: (10, 6),
        items,
        selected: 0,
        target_dir: target,
    });
    // The visible "Select for Compare" row sits at clipped.y + 1
    // (top border) + 5 (item index) = 3 + 1 + 5 = 9.
    let idx = app
        .menu_item_at(11, 9)
        .expect("hit must land inside the menu");
    assert_eq!(
        idx, 5,
        "click on visible row 9 must map to item 5 (Select for Compare)"
    );
}

#[test]
fn tree_context_menu_offers_compare_with_selected_when_anchor_is_present() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    std::fs::write(&a, "v1").unwrap();
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "v2").unwrap();
    let n = file_node(&b);
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&b),
        tmp.path(),
        None,
        Some(a.as_path()),
    );
    let kinds: Vec<&MenuAction> = items.iter().map(|(_, a)| a).collect();
    assert!(
        kinds
            .iter()
            .any(|a| matches!(a, MenuAction::CompareWithSelected { .. })),
        "Compare with Selected must be offered when an anchor is set"
    );
    assert!(
        kinds
            .iter()
            .any(|a| matches!(a, MenuAction::SelectForCompare(_))),
        "Select for Compare must always be offered for single files"
    );
}

#[test]
fn tree_context_menu_hides_compare_when_anchor_is_the_same_file() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    std::fs::write(&a, "v1").unwrap();
    let n = file_node(&a);
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&a),
        tmp.path(),
        None,
        Some(a.as_path()),
    );
    let kinds: Vec<&MenuAction> = items.iter().map(|(_, a)| a).collect();
    assert!(
        !kinds
            .iter()
            .any(|a| matches!(a, MenuAction::CompareWithSelected { .. })),
        "Compare with Selected must not appear against itself"
    );
}

#[test]
fn tree_context_menu_with_two_files_offers_compare_selected() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    std::fs::write(&a, "v1").unwrap();
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "v2").unwrap();
    let n = file_node(&a);
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        &[a.clone(), b.clone()],
        tmp.path(),
        None,
        None,
    );
    let compare = items
        .iter()
        .find(|(s, _)| s == "Compare Selected")
        .expect("Compare Selected must be offered for a two-file selection");
    assert!(matches!(
        compare.1,
        MenuAction::CompareWithSelected { ref anchor, ref other }
            if anchor == &a && other == &b
    ));
}

#[test]
fn tree_context_menu_hides_compare_selected_when_a_folder_is_selected() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    std::fs::write(&a, "v1").unwrap();
    let d = tmp.path().join("dir");
    std::fs::create_dir(&d).unwrap();
    let n = file_node(&a);
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        &[a.clone(), d.clone()],
        tmp.path(),
        None,
        None,
    );
    assert!(
        !items.iter().any(|(s, _)| s == "Compare Selected"),
        "Compare Selected must not appear when the selection includes a folder"
    );
}

#[test]
fn tree_context_menu_with_multi_selection_promotes_delete_count() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hello.txt");
    std::fs::write(&f, "hi").unwrap();
    let f2 = tmp.path().join("bye.txt");
    std::fs::write(&f2, "bye").unwrap();
    let n = file_node(&f);
    let target = f.parent().unwrap().to_path_buf();
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        &[f.clone(), f2.clone()],
        &target,
        None,
        None,
    );
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        labels,
        [
            "New File",
            "New Folder",
            "Cut",
            "Copy",
            "Compare Selected",
            "Delete 2 items"
        ],
    );
}

#[test]
fn tree_context_menu_on_empty_space_offers_new_file_and_new_folder_only() {
    let tmp = tempfile::tempdir().unwrap();
    let items = build_tree_context_menu_items(None, tmp.path(), &[], tmp.path(), None, None);
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(labels, ["New File", "New Folder"]);
    assert!(matches!(items[0].1, MenuAction::Create(CreateKind::File)));
    assert!(matches!(items[1].1, MenuAction::Create(CreateKind::Folder)));
}

#[test]
fn tree_context_menu_on_empty_space_includes_paste_when_clipboard_set() {
    let tmp = tempfile::tempdir().unwrap();
    let clip = ExplorerClipboard {
        mode: ExplorerClipMode::Copy,
        paths: vec![tmp.path().join("x.txt")],
    };
    let items = build_tree_context_menu_items(None, tmp.path(), &[], tmp.path(), Some(&clip), None);
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(labels, ["New File", "New Folder", "Paste"]);
}

#[test]
fn consume_welcome_image_clear_fires_once_when_editor_opens_a_file() {
    // Repro for the "logo bleeds through under an open file" bug:
    // after the welcome OSC-1337 has been written to iTerm, opening a
    // file must signal a one-shot screen clear so ratatui repaints
    // every cell on the next draw and iTerm's image cache for those
    // cells is wiped.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.welcome.set_displayed(true);
    let f = tmp.path().join("hi.txt");
    std::fs::write(&f, "hi").unwrap();
    app.editor.open_pinned(&f).unwrap();
    assert!(app.consume_welcome_image_clear(), "first call must fire");
    assert!(
        !app.overlays.welcome.is_displayed(),
        "flag must reset after consume"
    );
    assert!(
        !app.consume_welcome_image_clear(),
        "second call must be a no-op until welcome is re-shown"
    );
}

#[test]
fn consume_welcome_image_clear_is_noop_while_editor_pane_is_blank() {
    // The image is still meant to be visible, so don't clear.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.welcome.set_displayed(true);
    assert!(!app.consume_welcome_image_clear());
    assert!(
        app.overlays.welcome.is_displayed(),
        "flag must NOT reset while image is still meant to show"
    );
}

#[test]
fn consume_welcome_image_clear_handles_reposition_request_while_blank() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.welcome.set_displayed(true);
    app.overlays.welcome.request_clear();

    assert!(app.consume_welcome_image_clear());
    assert!(!app.overlays.welcome.is_displayed());
    assert!(!app.overlays.welcome.clear_requested());
}

#[test]
fn consume_welcome_image_clear_is_noop_when_image_was_never_shown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.welcome.set_displayed(false);
    let f = tmp.path().join("hi.txt");
    std::fs::write(&f, "hi").unwrap();
    app.editor.open_pinned(&f).unwrap();
    assert!(!app.consume_welcome_image_clear());
}

#[test]
fn clicking_source_control_does_not_block_on_git_and_posts_a_changes_request() {
    // Regression for the user-reported "Source Control click feels
    // sluggish vs the other sidebar icons" bug. The cause was a
    // synchronous `git status --porcelain` shell-out from
    // `refresh_source_control` running on the UI thread, costing
    // 15-50 ms per click on a small repo and 100-300 ms on a large
    // one. The fix routes every git status query through a
    // background worker; the click path now does a single
    // non-blocking `mpsc::Sender::send` and returns within the
    // microsecond range.
    let tmp = tempfile::tempdir().unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["init", "-q", "-b", "main"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["config", "user.email", "a@b"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["config", "user.name", "a"])
        .status();
    std::fs::write(tmp.path().join("dirty.txt"), "x").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Pretend the worker is idle by draining its initial Status
    // response (App::new posts one to seed the badge). This isn't
    // strictly required for the assertion but keeps the test from
    // racing with that startup reply.
    std::thread::sleep(std::time::Duration::from_millis(150));
    let _ = app.drain_git_responses();
    let started = std::time::Instant::now();
    app.set_sidebar_view(SidebarView::SourceControl);
    let elapsed = started.elapsed();
    // The synchronous shell-out used to take 15-50 ms minimum on
    // any real machine; an order of magnitude headroom catches the
    // regression even on a fast CI box.
    assert!(
        elapsed < std::time::Duration::from_millis(5),
        "Source Control click must not block on git; took {elapsed:?}",
    );
    // The Changes request must have landed on the worker — give it
    // up to 2 s to respond, then drain. The response carries the
    // single untracked file we wrote above.
    let mut got_changes = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if app.drain_git_responses() && !app.source_control.entries.is_empty() {
            got_changes = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        got_changes,
        "the worker must reply with the dirty file's Changes entry; entries = {:?}",
        app.source_control.entries,
    );
}

#[test]
fn resize_arms_a_one_shot_terminal_clear_to_evict_stale_activity_icons() {
    // Regression for the activity-bar icon ghosting after a pane reshape.
    // iTerm2 keeps OSC-1337 image cells in a layer that survives plain SGR
    // overwrites, so on resize the previously-painted icon bitmaps linger and
    // the re-emit stamps a second copy on top (the doubled Run/Debug icon).
    // A resize must arm the same one-shot `terminal.clear()` the view-change
    // path uses, so iTerm2 drops the stale layer before the icons re-emit.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Pretend init_graphics baked the activity-bar icon images — images mode
    // is the only state in which the stale layer can exist.
    app.overlays.activity.set_images(ActivityBarImages {
        explorer_active: String::new(),
        explorer_inactive: String::new(),
        search_active: String::new(),
        search_inactive: String::new(),
        source_control_active: String::new(),
        source_control_inactive: String::new(),
        remote_active: String::new(),
        remote_inactive: String::new(),
        run_debug_active: String::new(),
        run_debug_inactive: String::new(),
        settings_active: String::new(),
        settings_inactive: String::new(),
    });
    app.on_resize();
    assert!(
        app.consume_activity_image_clear(),
        "a pane resize must arm exactly one terminal.clear() so iTerm2 evicts the stale OSC-1337 activity-bar image layer",
    );
    assert!(
        !app.consume_activity_image_clear(),
        "the clear request must be one-shot — consuming twice in a row returns false",
    );
    assert!(
        app.overlays.activity.is_dirty(),
        "resize must also re-mark the icons dirty so the post-draw flush re-emits them after the clear",
    );
}

#[test]
fn activity_bar_icons_moving_within_the_flush_arms_a_one_shot_terminal_clear() {
    // Same emit-time move-detection the run-debug headline icon uses, applied
    // to the activity bar: when the icon cells shift since the last emit, the
    // post-draw flush must arm one terminal.clear() to evict iTerm2's stale
    // OSC-1337 image layer instead of stacking a fresh copy on top (the
    // doubled-icon ghost). This is trigger-agnostic — it fires for any layout
    // shift, not only the Event::Resize path that on_resize covers.
    use ratatui::layout::Rect;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(ActivityBarImages {
        explorer_active: String::new(),
        explorer_inactive: String::new(),
        search_active: String::new(),
        search_inactive: String::new(),
        source_control_active: String::new(),
        source_control_inactive: String::new(),
        remote_active: String::new(),
        remote_inactive: String::new(),
        run_debug_active: String::new(),
        run_debug_inactive: String::new(),
        settings_active: String::new(),
        settings_inactive: String::new(),
    });
    let place = |app: &mut App, base: u16| {
        app.sidebar_areas.explorer_icon = Rect::new(0, base, 2, 1);
        app.sidebar_areas.search_icon = Rect::new(0, base + 2, 2, 1);
        app.sidebar_areas.source_control_icon = Rect::new(0, base + 4, 2, 1);
        app.sidebar_areas.remote_icon = Rect::new(0, base + 6, 2, 1);
        app.sidebar_areas.run_debug_icon = Rect::new(0, base + 8, 2, 1);
    };
    // First emit at one layout: records the positions, arms no clear.
    place(&mut app, 2);
    app.overlays.activity.mark_dirty();
    app.flush_activity_image_overlays();
    assert!(
        !app.consume_activity_image_clear(),
        "the first emit has no prior positions to compare, so it must not arm a clear",
    );
    // A resize recenters the bar — every icon row shifts by one.
    place(&mut app, 3);
    app.overlays.activity.mark_dirty();
    app.flush_activity_image_overlays();
    assert!(
        app.consume_activity_image_clear(),
        "icons that moved since the last emit must arm one terminal.clear() to evict the stale image layer",
    );
    assert!(
        !app.consume_activity_image_clear(),
        "the clear request must be one-shot",
    );
}

#[test]
fn leaving_run_debug_after_emitting_the_icon_arms_a_one_shot_terminal_clear() {
    // Regression for the bug+play headline icon ghosting on top of
    // whichever sidebar replaced Run-Debug. iTerm2's OSC-1337 image
    // cells survive plain SGR overwrites, so the welcome / editor
    // pipelines arm a `terminal.clear()` on the next render to evict
    // them. Run-Debug needs the same gate.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Pretend init_graphics succeeded and the panel emitted the icon
    // while the user was on Run-Debug.
    app.overlays
        .run_debug
        .set_image("\x1b]1337;File=...\x07".to_string());
    app.set_sidebar_view(SidebarView::RunDebug);
    app.run_debug.last_icon_cell = Some((10, 5));
    app.flush_run_debug_icon_overlay();
    assert!(
        app.overlays.run_debug.was_emitted(),
        "test setup: the flush must record the emit position"
    );
    // Switch away. The clear flag must be armed so the main loop's
    // OR chain fires `terminal.clear()` once before the next draw.
    app.set_sidebar_view(SidebarView::Explorer);
    assert!(
        app.consume_run_debug_image_clear(),
        "leaving Run-Debug after emitting the icon must arm exactly one terminal.clear() so iTerm2's image cells are evicted",
    );
    assert!(
        !app.consume_run_debug_image_clear(),
        "the clear request must be one-shot — consuming twice in a row returns false",
    );
}

#[test]
fn flush_run_debug_icon_overlay_is_silent_when_panel_is_not_visible() {
    // Critical invariant: when the user has switched away from
    // Run-Debug, the post-draw flush MUST NOT write any wipe payload
    // (no spaces, no replacement OSC). The prior space-stamp wipe
    // fired AFTER the redraw and erased the freshly-painted next
    // sidebar in those cells. Eviction is now handled exclusively
    // by `terminal.clear()` via `consume_run_debug_image_clear`.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays
        .run_debug
        .set_image("\x1b]1337;File=...\x07".to_string());
    app.set_sidebar_view(SidebarView::RunDebug);
    app.run_debug.last_icon_cell = Some((10, 5));
    app.flush_run_debug_icon_overlay();
    assert_eq!(app.overlays.run_debug.last_emitted(), Some((10, 5)));
    // Switch away.
    app.set_sidebar_view(SidebarView::Explorer);
    // The flush in this state must clear last_emitted but emit
    // nothing (we can only assert the bookkeeping side effect here;
    // the absence of stdout writes is asserted indirectly by the
    // function returning early before the write_all call).
    app.flush_run_debug_icon_overlay();
    assert_eq!(
        app.overlays.run_debug.last_emitted(),
        None,
        "flush on a hidden panel must reset last_emitted so a re-entry primes a fresh emit",
    );
}

#[test]
fn run_debug_icon_moving_within_a_visible_panel_arms_a_one_shot_terminal_clear() {
    // Regression for the doubled/stacked headline-icon ghost: when the
    // panel stays on Run-Debug but a layout change (terminal resize OR an
    // internal pane-drag, which never fires Event::Resize) recenters the
    // icon, the next flush would emit a fresh OSC-1337 image at the new
    // cell while iTerm2 still caches the old one — two overlapping icons.
    // The flush must instead detect the move, arm exactly one
    // terminal.clear() to evict the stale cells, and skip re-emitting until
    // the clear has run (last_emitted reset to None primes a fresh emit).
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays
        .run_debug
        .set_image("\x1b]1337;File=...\x07".to_string());
    app.set_sidebar_view(SidebarView::RunDebug);
    app.run_debug.last_icon_cell = Some((10, 5));
    app.flush_run_debug_icon_overlay();
    assert_eq!(app.overlays.run_debug.last_emitted(), Some((10, 5)));
    // A resize / pane-drag recenters the icon to a new cell while the panel
    // is still the active, visible Run-Debug view.
    app.run_debug.last_icon_cell = Some((3, 2));
    app.flush_run_debug_icon_overlay();
    assert!(
        app.consume_run_debug_image_clear(),
        "an icon that moved within the visible panel must arm one terminal.clear() to evict the stale image cells",
    );
    assert!(
        !app.consume_run_debug_image_clear(),
        "the clear request must be one-shot",
    );
    assert_eq!(
        app.overlays.run_debug.last_emitted(),
        None,
        "the move branch must reset last_emitted so the post-clear frame re-emits a single clean icon",
    );
}

#[test]
fn tree_context_menu_on_workspace_root_offers_new_file_and_new_folder_only() {
    // Right-clicking the workspace root row must NOT offer Rename/Delete
    // (the root cannot be renamed or moved to trash from inside croft).
    let tmp = tempfile::tempdir().unwrap();
    let n = dir_node(tmp.path());
    let items = build_tree_context_menu_items(Some(&n), tmp.path(), &[], tmp.path(), None, None);
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(labels, ["New File", "New Folder"]);
}

#[test]
fn bracketed_paste_into_search_input_appends_to_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("foo");
    app.handle_paste("bar");
    assert_eq!(app.search.query, "foobar");
}

#[test]
fn bracketed_paste_into_search_strips_newlines() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.handle_paste("hello\nworld\r\n");
    assert_eq!(app.search.query, "helloworld");
}

#[test]
fn search_view_focuses_search_input_not_terminal_or_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);
    app.handle_key(key(
        KeyCode::Char('F'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(app.focus == Pane::Tree);
    assert!(app.search.focused);
    assert!(!app.tree.focused);
    assert!(!app.terminal().focused);
}

#[test]
fn cycling_focus_away_from_search_clears_search_focus_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.cycle_focus();
    assert!(!app.search.focused);
}

#[test]
fn search_editing_shortcuts_route_to_search_only_when_the_search_input_is_focused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    assert!(
        app.focus == Pane::Tree,
        "Search view focuses the side panel"
    );
    app.search.query = String::from("alpha");
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.search.selection_range(), Some((0, 5)));
}

#[test]
fn search_visible_does_not_steal_editing_shortcuts_from_a_focused_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);
    app.search.query = String::from("alpha");
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.search.selection_range(),
        None,
        "Cmd+A must act on the focused terminal, not the search query"
    );
}

#[test]
fn search_visible_routes_cmd_v_to_search_only_when_search_input_focused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.clipboard_reader = || Some(String::from("needle"));
    app.set_sidebar_view(SidebarView::Search);

    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
        .unwrap();

    assert_eq!(app.search.query, "needle");
}

#[test]
fn search_visible_routes_cmd_v_to_terminal_not_search_when_terminal_focused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.clipboard_reader = || Some(String::from("needle"));
    app.set_sidebar_view(SidebarView::Search);
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);

    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
        .unwrap();

    assert_eq!(
        app.search.query, "",
        "Cmd+V must paste into the focused terminal, not leak into the search query"
    );
}

#[test]
fn change_workspace_root_into_a_git_subdir_flips_source_control_into_repo_state() {
    // Regression for the user's "Make Root into a .git-bearing folder
    // doesn't update Source Control" bug: the git worker thread was
    // spawned ONCE with the initial root captured by value. After
    // change_workspace_root, the worker kept shelling git status
    // against the OLD root, so the Source Control panel stayed in
    // its no-repo empty state even though the new root was a real
    // git tree.
    let outer = tempfile::tempdir().unwrap();
    let repo = outer.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(&repo)
        .args(["init", "-q", "-b", "main"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(&repo)
        .args(["config", "user.email", "a@b"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(&repo)
        .args(["config", "user.name", "a"])
        .status();
    let mut app = App::new(outer.path().to_path_buf()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(150));
    let _ = app.drain_git_responses();
    assert!(
        !app.source_control.status.in_repo,
        "preconditions: outer dir must NOT be a git repo"
    );

    app.change_workspace_root(repo.clone());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut flipped = false;
    while std::time::Instant::now() < deadline {
        if app.drain_git_responses() && app.source_control.status.in_repo {
            flipped = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        flipped,
        "after Make Root into a real git tree the worker must re-query against the new root and the Source Control panel must report in_repo=true; saw status={:?}",
        app.source_control.status,
    );
    assert_eq!(app.source_control.status.branch.as_deref(), Some("main"));
}

fn make_committed_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(p)
        .args(["init", "-q", "-b", "main"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(p)
        .args(["config", "user.email", "a@b"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(p)
        .args(["config", "user.name", "a"])
        .status();
    std::fs::write(p.join("seed.txt"), b"seed\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(p)
        .args(["add", "seed.txt"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(p)
        .args(["commit", "-q", "-m", "init"])
        .status();
    tmp
}

fn wait_for_changes(app: &mut App, predicate: impl Fn(&App) -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut next_kick = std::time::Instant::now();
    while std::time::Instant::now() < deadline {
        if std::time::Instant::now() >= next_kick {
            app.refresh_source_control();
            next_kick = std::time::Instant::now() + std::time::Duration::from_millis(200);
        }
        app.drain_git_responses();
        if predicate(app) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    predicate(app)
}

#[test]
fn stage_source_control_entry_runs_git_add_and_flips_kind_to_staged() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(
                |e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::Modified
            )),
        "preconditions: seed.txt must appear as Modified"
    );
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.stage_source_control_entry(idx);
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(
                |e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::StagedModified
            )),
        "after stage_source_control_entry the file must read as StagedModified; saw {:?}",
        app.source_control.entries,
    );
}

#[test]
fn request_discard_source_control_entry_only_opens_a_confirm_modal_without_touching_disk() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.request_discard_source_control_entry(idx);
    assert!(
        app.pending_discard.is_some(),
        "Discard click must request user confirmation rather than discarding immediately"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "changed\n",
        "request alone must not touch the working tree"
    );
}

#[test]
fn confirming_discard_for_modified_file_restores_head_content() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.request_discard_source_control_entry(idx);
    app.confirm_pending_discard();
    assert!(
        app.pending_discard.is_none(),
        "modal must close after confirm"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "seed\n",
        "discard must restore HEAD content of a Modified file"
    );
}

#[test]
fn confirming_discard_for_untracked_file_deletes_it_from_disk() {
    let tmp = make_committed_repo();
    let new_file = tmp.path().join("scratch.txt");
    std::fs::write(&new_file, b"untracked\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "scratch.txt" && e.kind == crate::git::ChangeKind::Untracked)
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "scratch.txt")
        .unwrap();
    app.request_discard_source_control_entry(idx);
    assert!(matches!(
        app.pending_discard.as_ref(),
        Some(pd) if pd.untracked
    ));
    app.confirm_pending_discard();
    assert!(
        !new_file.exists(),
        "discard on an Untracked entry must delete the file from disk"
    );
}

#[test]
fn cancel_pending_discard_clears_modal_without_acting() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.request_discard_source_control_entry(idx);
    app.cancel_pending_discard();
    assert!(app.pending_discard.is_none());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "changed\n",
        "cancel must NOT touch the working tree"
    );
}

#[test]
fn clicking_the_stage_icon_on_a_selected_unstaged_row_stages_that_entry() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    app.set_sidebar_view(SidebarView::SourceControl);
    let backend = ratatui::backend::TestBackend::new(80, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.source_control.selected_change = Some(idx);
    term.draw(|f| app.render(f)).unwrap();
    let actions = app
        .source_control
        .last_row_actions
        .expect("selected unstaged row must expose action rects");
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: actions.stage.x,
        row: actions.stage.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(
                |e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::StagedModified
            )),
        "clicking the stage cell must run `git add` against that entry"
    );
}

#[test]
fn clicking_the_discard_icon_on_a_selected_unstaged_row_opens_the_confirm_modal() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    app.set_sidebar_view(SidebarView::SourceControl);
    let backend = ratatui::backend::TestBackend::new(80, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.source_control.selected_change = Some(idx);
    term.draw(|f| app.render(f)).unwrap();
    let actions = app
        .source_control
        .last_row_actions
        .expect("selected unstaged row must expose action rects");
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: actions.discard.x,
        row: actions.discard.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.pending_discard.is_some(),
        "discard click must arm the confirm modal (NEVER discard silently)"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "changed\n",
        "discard click alone must NOT touch the working tree until the user confirms"
    );
}

#[test]
fn cmd_a_in_source_control_pane_selects_every_change_row() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    std::fs::write(tmp.path().join("scratch.txt"), b"new\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2);
    app.set_sidebar_view(SidebarView::SourceControl);
    let n = app.source_control.entries.len();
    app.handle_source_control_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
    assert_eq!(
        app.source_control.multi_selection.len(),
        n,
        "Cmd+A in SC must populate multi_selection with every entry"
    );
}

#[test]
fn cmd_s_through_handle_key_dispatches_to_source_control_not_to_editor_save() {
    // Regression for the user's "Cmd+S didn't react" report: the
    // global `is_save_key` intercept used to fire BEFORE the SC
    // handler, so Cmd+S called `App::save` (a no-op when no editor
    // file was open) and the user's selected change never moved to
    // the index. Route through `handle_key` (not `handle_source_control_key`)
    // to exercise the full dispatcher.
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    app.set_sidebar_view(SidebarView::SourceControl);
    app.source_control.select_all_changes();
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(
                |e| e.path == "seed.txt" && e.kind.section() == crate::git::ChangeSection::Staged
            )),
        "Cmd+S routed through handle_key must reach the SC stage handler, not the global save key intercept"
    );
}

#[test]
fn staging_a_filename_with_a_space_works_after_porcelain_dequoting() {
    // Regression for "fatal: pathspec '\"linked_list copy.py\"' did
    // not match any files": porcelain output wraps filenames with
    // spaces in double quotes, and we used to feed those quotes
    // straight into `git add`. The parser now dequotes; the staging
    // path must end at the real filename.
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("linked_list copy.py"), b"x\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "linked_list copy.py")
    });
    assert!(
        !app.source_control
            .entries
            .iter()
            .any(|e| e.path.starts_with('"') || e.path.ends_with('"')),
        "no entry's path may still carry the porcelain quoting wrapper"
    );
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "linked_list copy.py")
        .unwrap();
    app.stage_source_control_entry(idx);
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(|e| e.path == "linked_list copy.py"
                && e.kind.section() == crate::git::ChangeSection::Staged)),
        "Staging a file whose name contains a space must succeed; saw status={:?}",
        app.status,
    );
}

#[test]
fn cmd_s_in_source_control_pane_stages_every_multi_selected_entry() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    std::fs::write(tmp.path().join("scratch.txt"), b"new\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2);
    app.set_sidebar_view(SidebarView::SourceControl);
    app.source_control.select_all_changes();
    app.handle_source_control_key(key(KeyCode::Char('s'), KeyModifiers::SUPER));
    assert!(
        wait_for_changes(&mut app, |a| {
            let kinds: Vec<_> = a
                .source_control
                .entries
                .iter()
                .map(|e| e.kind.section())
                .collect();
            !kinds.is_empty()
                && kinds
                    .iter()
                    .all(|s| *s == crate::git::ChangeSection::Staged)
        }),
        "Cmd+S with multi-selection must stage every selected entry; saw {:?}",
        app.source_control.entries,
    );
}

#[test]
fn cmd_s_with_single_selection_stages_only_that_entry() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    std::fs::write(tmp.path().join("scratch.txt"), b"new\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2);
    app.set_sidebar_view(SidebarView::SourceControl);
    let scratch_idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "scratch.txt")
        .unwrap();
    // Single click resets multi_selection to just this row.
    app.source_control.multi_selection.clear();
    app.source_control.multi_selection.insert(scratch_idx);
    app.source_control.selected_change = Some(scratch_idx);
    app.handle_source_control_key(key(KeyCode::Char('s'), KeyModifiers::SUPER));
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(|e| e.path == "scratch.txt"
                && e.kind.section() == crate::git::ChangeSection::Staged)),
        "scratch.txt must have been staged"
    );
    assert!(
        app.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::Modified),
        "seed.txt must remain unstaged (single-row stage scope)"
    );
}

#[test]
fn cmd_enter_in_source_control_falls_back_to_plain_commit_without_pushing() {
    // iTerm2 swallows Cmd+Enter as Toggle Fullscreen so the chord never
    // reaches croft. Terminals that DO deliver it should treat Cmd as
    // a no-op modifier — Enter alone commits, Ctrl+Enter pushes. The
    // SC handler's plain `KeyCode::Enter` match arm catches the
    // residual Cmd+Enter and calls `commit_source_control` (no push).
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    app.set_sidebar_view(SidebarView::SourceControl);
    app.source_control.message = "fix: plain cmd-enter".to_string();
    app.source_control.message_cursor = app.source_control.message.chars().count();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::SUPER));
    let log = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["log", "--oneline"])
        .output()
        .unwrap();
    let log_out = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_out.contains("fix: plain cmd-enter"),
        "Cmd+Enter must still commit locally (push is bound to Ctrl+Enter only): log={log_out:?}"
    );
}

#[test]
fn ctrl_enter_in_source_control_commits_and_pushes() {
    let tmp = make_committed_repo();
    // Wire a bare remote and set it as origin so `git push` has
    // somewhere to send.
    let remote = tempfile::tempdir().unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(remote.path())
        .args(["init", "--bare", "-q", "-b", "main"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["remote", "add", "origin"])
        .arg(remote.path())
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["push", "-u", "origin", "main"])
        .status();
    // Make a tracked change and stage it so commit-all-tracked has
    // something to commit.
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    app.set_sidebar_view(SidebarView::SourceControl);
    app.source_control.message = "fix: bump seed".to_string();
    app.source_control.message_cursor = app.source_control.message.chars().count();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::CONTROL));
    // Verify the remote received the commit (its log now reports two
    // commits — the initial push + our new one).
    let log = std::process::Command::new("git")
        .args(["-C"])
        .arg(remote.path())
        .args(["log", "--oneline"])
        .output()
        .unwrap();
    let log_out = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_out.lines().count() >= 2,
        "Cmd+Enter must have committed AND pushed (remote log: {log_out:?})"
    );
    assert!(
        log_out.contains("fix: bump seed"),
        "remote's log must include the new commit's subject"
    );
}

#[test]
fn opening_a_modified_file_diff_parks_scroll_on_the_first_change_hunk() {
    let tmp = make_committed_repo();
    let big = (0..50).map(|i| format!("line {i}\n")).collect::<String>();
    std::fs::write(tmp.path().join("seed.txt"), &big).unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["add", "seed.txt"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["commit", "-q", "-m", "big seed"])
        .status();
    // Now change a line deep in the file so the first hunk lives well
    // past row 0.
    let mut lines: Vec<String> = big.lines().map(str::to_string).collect();
    lines[30] = "edited-30".to_string();
    std::fs::write(tmp.path().join("seed.txt"), lines.join("\n") + "\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.open_source_control_entry(idx);
    let diff = app
        .editor
        .diff
        .as_ref()
        .expect("opening a Modified entry must build a diff");
    assert!(
        diff.scroll >= 25 && diff.scroll <= 30,
        "diff must auto-scroll near the change (row ~30, with 2-row context above); saw scroll={}",
        diff.scroll,
    );
}

#[test]
fn f7_in_the_diff_view_jumps_to_the_next_change_hunk() {
    let tmp = make_committed_repo();
    // Build a multi-hunk file: edits at lines 10 and 40.
    let original: String = (0..50).map(|i| format!("line {i}\n")).collect();
    std::fs::write(tmp.path().join("seed.txt"), &original).unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["add", "seed.txt"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["commit", "-q", "-m", "seed"])
        .status();
    let mut edited: Vec<String> = original.lines().map(str::to_string).collect();
    edited[10] = "edited-10".to_string();
    edited[40] = "edited-40".to_string();
    std::fs::write(tmp.path().join("seed.txt"), edited.join("\n") + "\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.open_source_control_entry(idx);
    let first_scroll = app.editor.diff.as_ref().unwrap().scroll;
    // F7 jumps to the next hunk.
    app.handle_editor_key(key(KeyCode::F(7), KeyModifiers::NONE));
    let next_scroll = app.editor.diff.as_ref().unwrap().scroll;
    assert!(
        next_scroll > first_scroll,
        "F7 must advance scroll past the current hunk (saw first={first_scroll}, next={next_scroll})"
    );
    // Shift+F7 walks back.
    app.handle_editor_key(key(KeyCode::F(7), KeyModifiers::SHIFT));
    let prev_scroll = app.editor.diff.as_ref().unwrap().scroll;
    assert!(
        prev_scroll < next_scroll,
        "Shift+F7 must walk back to the prior hunk (saw next={next_scroll}, prev={prev_scroll})"
    );
}

#[test]
fn forward_diff_jump_wraps_from_the_last_hunk_back_to_the_first() {
    // Regression: the › arrow / F7 used to stick on the last hunk when
    // that hunk lived in the bottom-clamped region of the diff. The cause
    // was that `render_diff` clamps `diff.scroll` to `max_scroll` and
    // writes it back, so the next forward jump re-derived an anchor BELOW
    // the last hunk and kept re-selecting it instead of wrapping. Backward
    // (‹) was unaffected because a too-low anchor still has earlier hunks
    // to step through. This test drives forward jumps with a render between
    // each (to trigger the clamp) and asserts the forward arrow eventually
    // wraps to the first hunk.
    let tmp = make_committed_repo();
    let original: String = (0..60).map(|i| format!("line {i}\n")).collect();
    std::fs::write(tmp.path().join("seed.txt"), &original).unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["add", "seed.txt"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["commit", "-q", "-m", "seed"])
        .status();
    // Two hunks: one near the top, one near the very bottom so the bottom
    // hunk falls inside the clamped viewport region.
    let mut edited: Vec<String> = original.lines().map(str::to_string).collect();
    edited[5] = "edited-5".to_string();
    edited[58] = "edited-58".to_string();
    std::fs::write(tmp.path().join("seed.txt"), edited.join("\n") + "\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.open_source_control_entry(idx);

    // A small viewport forces render_diff to clamp scroll at the bottom.
    let backend = ratatui::backend::TestBackend::new(120, 24);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let first_scroll = app.editor.diff.as_ref().unwrap().scroll;
    // Forward to the bottom hunk; render to trigger the clamp writeback.
    app.handle_editor_key(key(KeyCode::F(7), KeyModifiers::NONE));
    term.draw(|f| app.render(f)).unwrap();
    let bottom_scroll = app.editor.diff.as_ref().unwrap().scroll;
    assert!(
        bottom_scroll > first_scroll,
        "F7 must advance to the bottom hunk (first={first_scroll}, bottom={bottom_scroll})"
    );
    // Forward again from the last hunk MUST wrap back to the first, not
    // stick on the bottom hunk.
    app.handle_editor_key(key(KeyCode::F(7), KeyModifiers::NONE));
    term.draw(|f| app.render(f)).unwrap();
    let wrapped_scroll = app.editor.diff.as_ref().unwrap().scroll;
    assert_eq!(
        wrapped_scroll, first_scroll,
        "forward from the last hunk must wrap to the first (saw {wrapped_scroll}, expected {first_scroll})"
    );
}

#[test]
fn focused_source_control_input_drives_a_visible_caret_at_the_message_cursor() {
    // Regression for the user's "I don't see the caret blinking in the
    // Source Control message box, so I have no clue where it is at any
    // point in time" report. After focusing the SC panel in a real
    // repo, `cursor_should_be_visible()` must report true and the
    // caret cell must point inside the rendered input box.
    let tmp = tempfile::tempdir().unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["init", "-q", "-b", "main"])
        .output();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        app.drain_git_responses();
        if app.source_control.status.in_repo {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        app.source_control.status.in_repo,
        "preconditions: tempdir must register as a git repo"
    );

    app.set_sidebar_view(SidebarView::SourceControl);
    let backend = ratatui::backend::TestBackend::new(80, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.handle_source_control_key(key(KeyCode::Char('f'), KeyModifiers::NONE));
    app.handle_source_control_key(key(KeyCode::Char('i'), KeyModifiers::NONE));
    app.handle_source_control_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
    term.draw(|f| app.render(f)).unwrap();

    assert!(
        app.cursor_should_be_visible(),
        "caret must be visible when the Source Control panel owns focus and the input is laid out"
    );
    let (cx, cy) = app
        .source_control
        .cursor_screen_pos()
        .expect("focused SC panel must expose a caret cell");
    let r = app.source_control.last_input_area;
    assert!(
        cx >= r.x + 2 && cx < r.x + r.width - 1 && cy == r.y + 1,
        "caret cell ({cx},{cy}) must sit inside the input's text row (input rect {r:?})"
    );
    assert_eq!(
        cx,
        r.x + 2 + 3,
        "caret must advance one cell per typed char (3 chars typed → +3 cells past the first text cell)"
    );
}

#[test]
fn editor_shift_alt_down_duplicates_the_current_line_via_handle_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("alpha"), String::from("beta")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 3;
    app.handle_key(key(KeyCode::Down, KeyModifiers::ALT | KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["alpha", "alpha", "beta"]);
    assert_eq!(
        (app.editor.cursor_row, app.editor.cursor_col),
        (1, 3),
        "cursor must follow onto the duplicated line"
    );
}

#[test]
fn editor_shift_alt_up_duplicates_the_current_line_via_handle_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("alpha"), String::from("beta")];
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 2;
    app.handle_key(key(KeyCode::Up, KeyModifiers::ALT | KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["alpha", "beta", "beta"]);
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 2));
}

#[test]
fn editor_alt_right_jumps_one_word_forward() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("hello world rest")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Right, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 5);
    app.handle_key(key(KeyCode::Right, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 11);
}

fn editor_app_with_lines(lines: &[&str]) -> App {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = lines.iter().map(|s| (*s).to_string()).collect();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app
}

#[test]
fn editor_cmd_gg_jumps_to_top_of_file() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
    app.editor.cursor_row = 3;
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 0));
}

#[test]
fn editor_cmd_g_then_plain_g_also_jumps_to_top() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
    app.editor.cursor_row = 3;
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 0));
}

#[test]
fn editor_cmd_g_count_g_jumps_to_specific_line() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('3'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (2, 0));
}

#[test]
fn editor_cmd_g_multidigit_count_g_jumps_to_specific_line() {
    let mut app = editor_app_with_lines(&(0..30).map(|_| "x").collect::<Vec<_>>());
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('1'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('2'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 11);
}

#[test]
fn editor_cmd_shift_g_jumps_to_bottom_of_file() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
    app.handle_key(key(
        KeyCode::Char('G'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.editor.cursor_row, 3);
    assert_ne!(
        app.sidebar_view,
        SidebarView::SourceControl,
        "cmd+shift+g in editor pane must NOT hijack to source control"
    );
}

#[test]
fn editor_cmd_dd_deletes_current_line() {
    let mut app = editor_app_with_lines(&["alpha", "beta", "gamma"]);
    app.editor.cursor_row = 1;
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["alpha", "gamma"]);
}

#[test]
fn editor_cmd_d_count_d_deletes_n_lines() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
    app.editor.cursor_row = 1;
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('3'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["a", "e"]);
}

#[test]
fn editor_cmd_yy_does_not_modify_buffer() {
    let mut app = editor_app_with_lines(&["alpha", "beta", "gamma"]);
    app.editor.cursor_row = 1;
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["alpha", "beta", "gamma"]);
    assert!(app.status.starts_with("Yanked"));
}

#[test]
fn editor_chord_cancels_on_unrelated_key() {
    let mut app = editor_app_with_lines(&["alpha"]);
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["alphax"]);
}

#[test]
fn editor_ctrl_a_jumps_to_start_of_line() {
    let mut app = editor_app_with_lines(&["hello world"]);
    app.editor.cursor_col = 7;
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 0);
}

#[test]
fn editor_ctrl_e_jumps_to_end_of_line() {
    let mut app = editor_app_with_lines(&["hello world"]);
    app.editor.cursor_col = 3;
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 11);
}

#[test]
fn editor_ctrl_k_kills_to_end_of_line() {
    let mut app = editor_app_with_lines(&["hello world", "next"]);
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["hello", "next"]);
}

#[test]
fn editor_cmd_count_gg_jumps_to_line_with_leading_count() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
    app.handle_key(key(KeyCode::Char('3'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 2);
}

#[test]
fn editor_cmd_count_dd_deletes_n_lines_with_leading_count() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
    app.editor.cursor_row = 1;
    app.handle_key(key(KeyCode::Char('2'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["a", "d", "e"]);
}

#[test]
fn editor_cmd_count_yy_yanks_n_lines_with_leading_count() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
    app.editor.cursor_row = 0;
    app.handle_key(key(KeyCode::Char('2'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["a", "b", "c", "d"]);
    assert!(app.status.contains("Yanked 2"));
}

#[test]
fn editor_cmd_count_then_shift_g_jumps_to_specific_line() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
    app.handle_key(key(KeyCode::Char('4'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(
        KeyCode::Char('G'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.editor.cursor_row, 3);
}

#[test]
fn editor_ctrl_u_kills_to_start_of_line() {
    let mut app = editor_app_with_lines(&["hello world"]);
    app.editor.cursor_col = 6;
    app.handle_key(key(KeyCode::Char('u'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["world"]);
    assert_eq!(app.editor.cursor_col, 0);
}

#[test]
fn editor_cmd_o_opens_line_below_with_indent() {
    let mut app = editor_app_with_lines(&["    foo", "bar"]);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 4;
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["    foo", "    ", "bar"]);
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 4));
}

#[test]
fn editor_cmd_shift_o_opens_line_above_with_indent() {
    let mut app = editor_app_with_lines(&["foo", "    bar"]);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 6;
    app.handle_key(key(
        KeyCode::Char('O'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.editor.lines, vec!["foo", "    ", "    bar"]);
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 4));
}

#[test]
fn f1_opens_the_shortcuts_modal_from_any_focus() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.shortcuts_modal.is_some(),
        "F1 must open the shortcuts panel"
    );
}

#[test]
fn esc_closes_the_shortcuts_modal_and_restores_normal_input() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.shortcuts_modal.is_none(),
        "Esc must dismiss the shortcuts panel"
    );
}

#[test]
fn shortcuts_modal_swallows_keys_so_editor_does_not_receive_them() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("hello")];
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.lines,
        vec!["hello"],
        "while the shortcuts modal is open, typed letters must not leak into the editor buffer"
    );
}

#[test]
fn opening_the_shortcuts_modal_arms_one_image_clear_so_iterm_evicts_cached_osc_1337_cells() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.consume_shortcuts_image_clear(),
        "F1 must arm terminal.clear() so iTerm2's image cache (activity bar icons, welcome wordmark, editor preview) is wiped before the modal paints, otherwise those cached image cells bleed through the modal text"
    );
    assert!(
        !app.consume_shortcuts_image_clear(),
        "the flag is one-shot — a second consume call must return false so the driver does not re-clear every frame"
    );
}

#[test]
fn closing_the_shortcuts_modal_arms_image_clear_and_re_dirties_overlays() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    let _ = app.consume_shortcuts_image_clear();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.consume_shortcuts_image_clear(),
        "Esc must also arm terminal.clear() so the modal's text cells get wiped and the activity bar / welcome wordmark / hero icons can be re-emitted cleanly"
    );
    assert!(app.overlays.activity.is_dirty());
    assert!(app.overlays.welcome.is_dirty());
    assert!(app.overlays.hero.is_dirty());
}

#[test]
fn ssh_empty_state_overlay_arms_image_clear_when_user_switches_off_remote_view() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Simulate the post-emit state without actually writing OSC bytes.
    app.overlays.ssh.set_displayed(true);
    app.set_sidebar_view(SidebarView::Explorer);
    app.flush_ssh_empty_state_overlay();
    assert!(
        app.consume_ssh_empty_state_image_clear(),
        "leaving the Remote view after the illustration was painted must arm one terminal.clear() so iTerm2 evicts the cached image cells — otherwise the server-rack PNG ghosts onto the Explorer tree the user just clicked to"
    );
    assert!(!app.overlays.ssh.is_displayed());
}

#[test]
fn ssh_empty_state_overlay_arms_image_clear_when_modal_opens_on_top() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Remote);
    app.overlays.ssh.set_displayed(true);
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    app.flush_ssh_empty_state_overlay();
    assert!(
        app.consume_ssh_empty_state_image_clear(),
        "F1 opening the shortcuts modal over the SSH empty state must arm an image-clear so the illustration doesn't bleed through the modal text"
    );
}

#[test]
fn activity_icons_stay_visible_beside_the_centered_shortcuts_modal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(dummy_activity_images());
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let before = app.pending_activity_image_overlays().len();
    assert_eq!(
        before, 6,
        "precondition: all five view icons plus the bottom settings gear emit"
    );
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let during = app.pending_activity_image_overlays().len();
    assert_eq!(
        during, before,
        "the shortcuts modal is centered over the editor column, so the far-left activity-bar icons it does not cover must keep re-emitting — otherwise the terminal.clear() armed by F1 wipes iTerm2's cached cells and the icons vanish for the whole time the modal is up; saw {before} icons before opening"
    );
}

#[test]
fn activity_icons_behind_a_modal_that_reaches_the_left_edge_are_suppressed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(dummy_activity_images());
    let backend = ratatui::backend::TestBackend::new(40, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let before = app.pending_activity_image_overlays().len();
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let during = app.pending_activity_image_overlays().len();
    assert!(
        during < before || before == 0,
        "on a terminal narrow enough that the centered modal reaches column 0, icons whose blocks intersect the modal must be suppressed so they don't ghost over the modal text; saw {before} icons before, {during} during"
    );
}

#[test]
fn settings_gear_is_bottom_anchored_below_the_view_icons() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(dummy_activity_images());
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let gear = app.sidebar_areas.settings_icon;
    let run_debug = app.sidebar_areas.run_debug_icon;
    assert!(gear.width > 0, "gear must lay out on a tall bar");
    assert!(
        gear.y > run_debug.y,
        "the gear is anchored at the bottom, below the Run-and-Debug view icon"
    );
    assert_eq!(
        app.pending_activity_image_overlays().len(),
        6,
        "five view icons plus the bottom settings gear emit"
    );
}

#[test]
fn clicking_the_gear_opens_the_color_theme_menu_then_the_picker() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(dummy_activity_images());
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let gear = app.sidebar_areas.settings_icon;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
        column: gear.x,
        row: gear.y,
        modifiers: KeyModifiers::NONE,
    });
    let menu = app.context_menu.as_ref().expect("gear opens a menu");
    assert!(
        matches!(menu.items.as_slice(), [(_, MenuAction::OpenThemePicker)]),
        "the gear menu holds a single Color Theme entry"
    );
    // Selecting Color Theme replaces the menu with the theme picker.
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let picker = app.context_menu.as_ref().expect("picker opens");
    assert_eq!(picker.items.len(), crate::theme::Theme::ALL.len());
    assert!(
        picker
            .items
            .iter()
            .all(|(_, a)| matches!(a, MenuAction::SetTheme(_))),
        "every picker row switches a theme"
    );
}

#[test]
fn selecting_the_black_theme_switches_the_active_theme() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert_eq!(
        app.theme,
        crate::theme::Theme::DarkBlue,
        "default is dark blue"
    );
    app.apply_theme(crate::theme::Theme::Black);
    assert_eq!(app.theme, crate::theme::Theme::Black);
    // A theme switch arms a one-shot clear so iTerm2 evicts the stale-bg
    // image layer and the whole screen repaints on the new background.
    assert!(app.consume_activity_image_clear());
}

#[test]
fn shortcuts_modal_scrolls_down_then_back_to_top_via_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    let modal = app.shortcuts_modal.as_mut().unwrap();
    modal.last_inner_height = 5;
    modal.last_content_height = 50;
    app.handle_key(key(KeyCode::PageDown, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.shortcuts_modal.as_ref().unwrap().scroll, 10);
    app.handle_key(key(KeyCode::Home, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.shortcuts_modal.as_ref().unwrap().scroll, 0);
}

#[test]
fn cmd_p_opens_the_file_finder_modal_for_quick_open() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.file_finder.is_some(),
        "Cmd+P must open the Quick Open file finder modal so the user can type a name and jump to a file the way VS Code does"
    );
}

#[test]
fn ctrl_p_opens_the_file_finder_modal_on_systems_without_super() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(
        app.file_finder.is_some(),
        "Ctrl+P must also open the Quick Open finder so the chord works on Linux / iTerm2 sessions that report CONTROL rather than SUPER"
    );
}

#[test]
fn plain_p_keystroke_does_not_open_the_file_finder() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.file_finder.is_none(),
        "an unmodified 'p' must reach the editor / tree typeahead and never trigger the modal"
    );
}

#[test]
fn esc_closes_the_file_finder_modal() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.file_finder.is_none(),
        "Esc must dismiss the file finder"
    );
}

#[test]
fn typing_in_file_finder_fuzzy_filters_the_result_list() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub").join("beta.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::NONE))
        .unwrap();
    let finder = app
        .file_finder
        .as_ref()
        .expect("finder must still be open while typing");
    let names: Vec<String> = finder
        .visible_results()
        .iter()
        .map(|r| r.entry.rel.clone())
        .collect();
    assert!(
        names.iter().any(|n| n == "sub/beta.rs"),
        "typing 'be' must keep sub/beta.rs in the result list — got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "alpha.rs"),
        "typing 'be' must drop alpha.rs because 'be' is not a subsequence of 'alpha.rs' — got {names:?}"
    );
}

#[test]
fn enter_in_file_finder_opens_the_selected_file_and_closes_the_modal() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "fn main() {}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.file_finder.is_none(),
        "Enter must close the modal after opening the file"
    );
    let opened = app
        .editor
        .path
        .as_ref()
        .expect("the active tab must now have a path");
    assert_eq!(
        opened.file_name().and_then(|n| n.to_str()),
        Some("alpha.rs"),
        "Enter must open the highlighted entry (alpha.rs) into the active editor tab"
    );
}

#[test]
fn down_arrow_moves_selection_in_the_file_finder() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a1.rs"), "").unwrap();
    std::fs::write(tmp.path().join("a2.rs"), "").unwrap();
    std::fs::write(tmp.path().join("a3.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    let before = app.file_finder.as_ref().unwrap().selected_index();
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    let after = app.file_finder.as_ref().unwrap().selected_index();
    assert_eq!(
        after,
        before + 1,
        "Down arrow must move the selection cursor one row"
    );
}

#[test]
fn backspace_removes_a_character_from_the_finder_query() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.file_finder.as_ref().unwrap().query, "a");
}

#[test]
fn opening_the_file_finder_arms_one_image_clear_so_iterm_evicts_cached_osc_1337_cells() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.consume_file_finder_image_clear(),
        "Cmd+P must arm terminal.clear() so iTerm2's image cache (activity bar icons, welcome wordmark, editor preview) is wiped before the modal paints, otherwise those cached image cells bleed through the file list"
    );
    assert!(
        !app.consume_file_finder_image_clear(),
        "the flag is one-shot — a second consume call must return false so the driver does not re-clear every frame"
    );
}

#[test]
fn cmd_shift_e_jumps_to_explorer_from_any_pane() {
    let mut app = editor_app_with_lines(&["x"]);
    app.set_sidebar_view(SidebarView::Search);
    app.show_tree = false;
    app.handle_key(key(
        KeyCode::Char('e'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.sidebar_view, SidebarView::Explorer);
    assert!(
        app.show_tree,
        "Cmd+Shift+E must un-collapse the sidebar if it was hidden"
    );
}

#[test]
fn cmd_b_and_ctrl_b_toggle_the_primary_side_bar() {
    // VS Code's "Toggle Primary Side Bar" is Cmd+B on macOS (Ctrl+B on
    // Linux); croft accepts both so the chord behaves identically whether
    // iTerm2 forwards Cmd+B as a Super CSI-u sequence or a raw Ctrl+B byte
    // arrives over SSH. The sidebar starts visible.
    let mut app = editor_app_with_lines(&["x"]);
    assert!(app.show_tree, "sidebar starts visible");
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::SUPER))
        .unwrap();
    assert!(!app.show_tree, "Cmd+B hides the side bar");
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.show_tree, "Cmd+B again shows it");
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(!app.show_tree, "Ctrl+B toggles the same state");
}

#[test]
fn cmd_shift_s_jumps_to_source_control_even_when_editor_is_focused() {
    let mut app = editor_app_with_lines(&["x"]);
    app.focus_pane(Pane::Editor);
    app.handle_key(key(
        KeyCode::Char('s'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.sidebar_view,
        SidebarView::SourceControl,
        "Cmd+Shift+S must jump to Source Control even from the editor (no Save-As to collide with); Search stays on Cmd+Shift+F"
    );
    assert!(
        matches!(app.focus, Pane::Tree),
        "Cmd+Shift+S must move keyboard focus into the Source Control panel"
    );
}

#[test]
fn cmd_shift_d_jumps_to_run_and_debug_from_any_pane() {
    let mut app = editor_app_with_lines(&["x"]);
    app.focus_pane(Pane::Editor);
    app.handle_key(key(
        KeyCode::Char('d'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.sidebar_view, SidebarView::RunDebug);
}

#[test]
fn cmd_shift_r_jumps_to_remote_from_any_pane() {
    let mut app = editor_app_with_lines(&["x"]);
    app.focus_pane(Pane::Editor);
    app.handle_key(key(
        KeyCode::Char('r'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.sidebar_view, SidebarView::Remote);
}

#[test]
fn is_drop_to_local_key_matches_only_cmd_shift_l() {
    assert!(is_drop_to_local_key(key(
        KeyCode::Char('l'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
    assert!(is_drop_to_local_key(key(
        KeyCode::Char('L'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
    assert!(!is_drop_to_local_key(key(
        KeyCode::Char('l'),
        KeyModifiers::SUPER
    )));
    assert!(!is_drop_to_local_key(key(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL
    )));
    assert!(!is_drop_to_local_key(key(
        KeyCode::Char('r'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
}

#[test]
fn cmd_shift_l_on_remote_requests_drop_to_local() {
    let mut app = editor_app_with_lines(&["x"]);
    app.is_remote = true;
    app.handle_key(key(
        KeyCode::Char('l'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        app.drop_to_local && app.quit,
        "Cmd+Shift+L on a remote-launched croft must request a clean hand-back to the local croft"
    );
}

#[test]
fn cmd_shift_l_on_local_is_inert() {
    let mut app = editor_app_with_lines(&["x"]);
    app.is_remote = false;
    app.handle_key(key(
        KeyCode::Char('l'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        !app.drop_to_local && !app.quit,
        "Cmd+Shift+L on a local croft must do nothing - there is no remote session to leave"
    );
}

#[test]
fn cmd_shift_t_focuses_the_terminal_pane_from_the_editor() {
    let mut app = editor_app_with_lines(&["x"]);
    app.focus_pane(Pane::Editor);
    app.handle_key(key(
        KeyCode::Char('t'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        matches!(app.focus, Pane::Terminal),
        "Cmd+Shift+T must move focus to the Terminal pane from the editor so the user can start typing in the shell without reaching for the mouse"
    );
}

#[test]
fn cmd_shift_t_focuses_the_terminal_pane_from_the_tree() {
    let mut app = editor_app_with_lines(&["x"]);
    app.focus_pane(Pane::Tree);
    app.handle_key(key(
        KeyCode::Char('t'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        matches!(app.focus, Pane::Terminal),
        "Cmd+Shift+T must move focus to the Terminal pane even when the Explorer is focused"
    );
}

#[test]
fn cmd_shift_t_unhides_the_terminal_if_it_was_collapsed() {
    let mut app = editor_app_with_lines(&["x"]);
    app.focus_pane(Pane::Editor);
    app.show_terminal = false;
    app.handle_key(key(
        KeyCode::Char('T'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        app.show_terminal,
        "Cmd+Shift+T must un-hide the terminal if it was collapsed (Ctrl+J) — otherwise the focus jump lands in a zero-row pane"
    );
    assert!(matches!(app.focus, Pane::Terminal));
}

#[test]
fn cmd_t_globally_splits_the_terminal_and_does_not_double_fire_as_focus() {
    let mut app = editor_app_with_lines(&["x"]);
    app.focus_pane(Pane::Editor);
    let before = app.terminals.len();
    app.handle_key(key(KeyCode::Char('t'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.terminals.len(),
        before + 1,
        "Cmd+T must split the terminal from any pane; Cmd+Shift+T (focus) must not steal the bare Cmd+T chord"
    );
    assert!(
        !is_terminal_split_key(key(
            KeyCode::Char('t'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT
        )),
        "Cmd+Shift+T is focus, not split"
    );
    assert!(
        !is_terminal_split_key(key(
            KeyCode::Char('t'),
            KeyModifiers::SUPER | KeyModifiers::ALT
        )),
        "Cmd+Opt+T is iTerm2's relocated New Tab, not split"
    );
}

#[test]
fn ctrl_shift_t_still_splits_the_terminal_and_does_not_double_fire_as_focus() {
    let mut app = editor_app_with_lines(&["x"]);
    app.focus_pane(Pane::Editor);
    let before = app.terminals.len();
    app.handle_key(key(
        KeyCode::Char('t'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.terminals.len(),
        before + 1,
        "Ctrl+Shift+T must remain bound to split_terminal — the new Cmd+Shift+T focus chord must not steal it"
    );
}

#[test]
fn terminal_focus_key_predicate_accepts_cmd_shift_t_and_rejects_neighbours() {
    let cmd_shift = KeyModifiers::SUPER | KeyModifiers::SHIFT;
    assert!(is_terminal_focus_key(key(KeyCode::Char('t'), cmd_shift)));
    assert!(is_terminal_focus_key(key(KeyCode::Char('T'), cmd_shift)));
    assert!(
        !is_terminal_focus_key(key(KeyCode::Char('t'), KeyModifiers::SUPER)),
        "Cmd+T (no Shift) is the split/new-terminal chord, not focus; the focus chord requires Shift"
    );
    assert!(
        !is_terminal_focus_key(key(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )),
        "Ctrl+Shift+T must stay routed to is_terminal_split_key, not focus"
    );
    assert!(
        !is_terminal_focus_key(key(
            KeyCode::Char('t'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT | KeyModifiers::ALT
        )),
        "Cmd+Opt+Shift+T must not focus — that chord is reserved (we relocate iTerm2's Restore Closed Session onto it)"
    );
}

#[test]
fn iterm2_image_drag_artefact_does_not_paste_its_path_into_the_editor() {
    // Root cause of the user's "Pasted 67 chars" surprise: when iTerm2
    // hijacks a click-drag on an OSC-1337 activity-bar icon, it writes
    // the decoded PNG to $TMPDIR/iTerm2.<rand>.png and re-injects the
    // path as a bracketed paste. parse_dropped_paths filters those
    // paths out of the IMPORT list, but without an early-return in
    // handle_paste the fallthrough would still insert the raw path
    // text into the focused editor.
    let mut app = editor_app_with_lines(&[""]);
    app.focus_pane(Pane::Editor);
    let tmp = tempfile::tempdir().unwrap();
    let temp_dir = tmp.path().join("T").join("com.googlecode.iterm2");
    std::fs::create_dir_all(&temp_dir).unwrap();
    let fake = temp_dir.join("iTerm2.5AO7K5.png");
    std::fs::write(&fake, b"\x89PNG\r\n\x1a\n").unwrap();
    let payload = format!("{}\n", fake.display());
    app.handle_paste(&payload);
    assert_eq!(
        app.editor.lines,
        vec![""],
        "iTerm2 inline-image drag artefact must be silently dropped at every level — the path string must NOT be pasted as text into the editor body (the user reported a CLAUDE.local.md tab opening with 'Pasted 67 chars' after trying to resize the sidebar near the Explorer icon)"
    );
}

#[test]
fn cmd_f_in_editor_opens_the_inline_find_overlay_not_the_new_file_prompt() {
    let mut app = editor_app_with_lines(&["alpha beta", "alpha gamma"]);
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.editor_find.is_some(),
        "Cmd+F with the editor focused must open the VS Code-style inline Find bar over the editor pane, not the Explorer 'new file' prompt"
    );
    assert!(
        app.prompt.is_none(),
        "Cmd+F in editor must NOT open the new-file Prompt — that is reserved for the Explorer pane"
    );
}

#[test]
fn cmd_f_in_explorer_still_opens_the_new_file_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Tree);
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.prompt.is_some(),
        "Cmd+F in the Explorer pane must keep creating a new file — the existing behaviour the user asked us to preserve"
    );
    assert!(
        app.editor_find.is_none(),
        "Cmd+F in the Explorer must NOT open the editor Find overlay; the chord is overloaded by pane"
    );
}

#[test]
fn cmd_shift_f_global_search_closes_the_local_editor_find_bar() {
    // With the in-file Find bar (Cmd+F) open, jumping to global Search
    // (Cmd+Shift+F) must dismiss the local bar. render_editor_find paints
    // whenever editor_find is Some regardless of focus, so a lingering bar
    // shadows the global search input on the left pane.
    let mut app = editor_app_with_lines(&["alpha beta", "alpha gamma"]);
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.editor_find.is_some(),
        "precondition: Cmd+F opens the inline editor Find bar"
    );
    app.handle_key(key(
        KeyCode::Char('f'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.sidebar_view,
        SidebarView::Search,
        "Cmd+Shift+F must switch the sidebar to global Search"
    );
    assert!(
        app.editor_find.is_none(),
        "Cmd+Shift+F must close the local editor Find bar so it cannot shadow the global search input"
    );
}

#[test]
fn typing_in_editor_find_jumps_the_cursor_to_the_next_match() {
    let mut app = editor_app_with_lines(&["alpha beta", "alpha gamma"]);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('t'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        (app.editor.cursor_row, app.editor.cursor_col),
        (0, 6),
        "typing 'beta' must jump the cursor to the first 'beta' occurrence — got ({}, {})",
        app.editor.cursor_row,
        app.editor.cursor_col
    );
}

#[test]
fn enter_in_editor_find_jumps_to_the_next_match_and_shift_enter_walks_back() {
    let mut app = editor_app_with_lines(&["alpha", "beta alpha", "alpha"]);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('l'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('h'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.cursor_row, 1,
        "Enter must jump to the next 'alpha' match on row 1"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.cursor_row, 2,
        "Enter again must walk forward to the row 2 match"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.editor.cursor_row, 1,
        "Shift+Enter must walk back one match to row 1"
    );
}

#[test]
fn jumping_to_a_match_records_an_active_search_match_so_the_renderer_can_paint_it_orange() {
    let mut app = editor_app_with_lines(&["alpha beta alpha"]);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('l'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('h'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    let first = app.editor.active_search_match;
    assert_eq!(
        first,
        Some((0, 0, 5)),
        "typing 'alpha' lands the cursor on the first match (col 0, len 5); active_search_match must record exactly that so the renderer can paint just those 5 cells orange while the other 'alpha' at col 11 keeps the regular yellow — got {first:?}"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let second = app.editor.active_search_match;
    assert_eq!(
        second,
        Some((0, 11, 5)),
        "Enter walks to the next match at col 11; the active marker must follow so the previously-orange match resets to yellow and the new cursor row gets the orange band"
    );
}

#[test]
fn esc_closes_the_editor_find_overlay_and_clears_the_highlight() {
    let mut app = editor_app_with_lines(&["alpha"]);
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.search_highlight.is_some());
    assert!(
        app.editor.active_search_match.is_some(),
        "typing should land the cursor on a match and arm the active-match marker"
    );
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor_find.is_none(), "Esc must close the find bar");
    assert!(
        app.editor.search_highlight.is_none(),
        "Esc must clear the editor's match highlight so leftover dim cells don't survive into normal typing"
    );
    assert!(
        app.editor.active_search_match.is_none(),
        "Esc must also clear the active-match marker so the orange band does not survive the overlay closing"
    );
}

#[test]
fn editor_find_pre_fills_the_query_from_word_under_cursor_on_open() {
    let mut app = editor_app_with_lines(&["alphabet"]);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.editor_find.as_ref().unwrap().query,
        "alpha",
        "opening Cmd+F with the cursor mid-word must pre-fill the query with the identifier chars to the left of the cursor, matching VS Code"
    );
}

#[test]
fn editor_find_paste_appends_clipboard_to_the_query() {
    let mut app = editor_app_with_lines(&["alpha needle beta"]);
    app.clipboard_reader = || Some(String::from("needle"));
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.editor_find.as_ref().unwrap().query,
        "needle",
        "Cmd+V inside the find bar must paste from the system clipboard"
    );
}

#[test]
fn picking_a_nested_file_via_cmd_p_expands_every_parent_dir_in_the_explorer_and_selects_the_row() {
    let tmp = tempfile::tempdir().unwrap();
    let nested_dir = tmp.path().join("packages").join("inner").join("citations");
    std::fs::create_dir_all(&nested_dir).unwrap();
    let target = nested_dir.join("storage.py");
    std::fs::write(&target, "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.tree.last_inner.height = 20;
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('t'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let selected = app
        .tree
        .selected_path()
        .map(|p| p.to_path_buf())
        .expect("a row must be selected after Enter on the finder");
    assert_eq!(
        selected, target,
        "the explorer cursor must land on the picked file, not stay on whichever row was selected before — got {selected:?}"
    );
    let visible_paths: Vec<String> = app
        .tree
        .nodes
        .iter()
        .map(|n| n.path.display().to_string())
        .collect();
    for expected_parent in [
        tmp.path().join("packages"),
        tmp.path().join("packages").join("inner"),
        tmp.path().join("packages").join("inner").join("citations"),
    ] {
        let s = expected_parent.display().to_string();
        assert!(
            visible_paths.contains(&s),
            "parent dir {s} must appear in the flattened node list — i.e. every ancestor of the picked file must be expanded so the row is reachable: got {visible_paths:?}"
        );
    }
}

#[test]
fn bracketed_paste_into_open_file_finder_appends_to_the_query_not_the_editor() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("hello")];
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_paste("alpha");
    assert_eq!(
        app.file_finder.as_ref().unwrap().query,
        "alpha",
        "an iTerm2 bracketed paste while the finder is open must land in the finder query so the user can Cmd+P then Cmd+V a filename"
    );
    assert_eq!(
        app.editor.lines,
        vec!["hello"],
        "the editor pane behind the modal must not receive the pasted text"
    );
}

#[test]
fn cmd_v_keystroke_in_file_finder_reads_clipboard_into_the_query() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.clipboard_reader = || Some(String::from("alpha"));
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.file_finder.as_ref().unwrap().query,
        "alpha",
        "Cmd+V inside the finder must read the system clipboard and append every char to the query — this covers terminals that disable bracketed paste or route Cmd+V via CSI-u"
    );
}

#[test]
fn file_finder_paste_strips_control_chars_so_a_pasted_newline_does_not_submit_garbage() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_paste("al\nph\ta");
    assert_eq!(
        app.file_finder.as_ref().unwrap().query,
        "alpha",
        "control chars in pasted text must be filtered out — a stray newline must not end the line and a tab must not navigate"
    );
}

#[test]
fn file_finder_modal_swallows_typed_characters_so_editor_does_not_receive_them() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("hello")];
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.lines,
        vec!["hello"],
        "while the file finder is open, typed letters must not leak into the editor buffer"
    );
    assert_eq!(
        app.file_finder.as_ref().unwrap().query,
        "x",
        "typed letters must go into the finder query instead"
    );
}

#[test]
fn editor_cmd_a_still_selects_all() {
    let mut app = editor_app_with_lines(&["hello", "world"]);
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.selection_text(), "hello\nworld");
}

#[test]
fn editor_alt_left_jumps_one_word_backward() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("hello world rest")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 16;
    app.handle_key(key(KeyCode::Left, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 12);
    app.handle_key(key(KeyCode::Left, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 6);
}

#[test]
fn editor_shift_alt_right_extends_selection_by_word() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("hello world")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Right, KeyModifiers::ALT | KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.editor.selection_text(),
        "hello",
        "Shift+Option+Right should select from cursor to the next word boundary"
    );
}

#[test]
fn search_visible_does_not_steal_cmd_x_from_focused_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.focus_pane(Pane::Editor);
    app.search.query = String::from("needle");
    app.editor.lines = vec![String::from("alpha")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 5;
    app.editor.select_all();

    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::SUPER))
        .unwrap();

    assert_eq!(app.editor.lines, [String::new()]);
    assert_eq!(app.search.query, "needle");
}

#[test]
fn search_visible_does_not_steal_bracketed_paste_from_focused_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.focus_pane(Pane::Editor);
    app.search.query = String::from("needle");
    app.editor.lines = vec![String::from("alpha")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 5;

    app.handle_paste(" beta");

    assert_eq!(app.editor.lines, [String::from("alpha beta")]);
    assert_eq!(app.search.query, "needle");
}

#[test]
fn cmd_v_pastes_clipboard_into_focused_editor_even_when_search_sidebar_visible() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.clipboard_reader = || Some(String::from(" beta"));
    app.set_sidebar_view(SidebarView::Search);
    app.focus_pane(Pane::Editor);
    app.search.query = String::from("needle");
    app.editor.lines = vec![String::from("alpha")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 5;

    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
        .unwrap();

    assert_eq!(app.editor.lines, [String::from("alpha beta")]);
    assert_eq!(app.search.query, "needle");
}

#[test]
fn cmd_a_in_search_selects_entire_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("alpha");
    app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
    assert_eq!(
        app.search.selection_range(),
        Some((0, "alpha".len())),
        "Cmd+A in search input must select the whole query"
    );
}

#[test]
fn cmd_c_in_search_with_full_selection_clears_no_text() {
    // After Cmd+A then Cmd+C, the query stays put (copy is non-destructive).
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("alpha");
    app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
    app.handle_search_key(key(KeyCode::Char('c'), KeyModifiers::SUPER));
    assert_eq!(app.search.query, "alpha");
}

#[test]
fn cmd_x_in_search_with_full_selection_clears_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("alpha");
    app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
    app.handle_search_key(key(KeyCode::Char('x'), KeyModifiers::SUPER));
    assert_eq!(app.search.query, "");
    assert_eq!(app.search.selection_range(), None);
}

#[test]
fn paste_clipboard_into_search_with_injected_text_appends_to_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("foo");
    app.paste_clipboard_into_search(Some("BAR"));
    assert_eq!(app.search.query, "fooBAR");
}

#[test]
fn paste_clipboard_into_search_with_no_text_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("foo");
    app.paste_clipboard_into_search(None);
    assert_eq!(app.search.query, "foo");
}

#[test]
fn paste_clipboard_into_search_with_no_text_sets_diagnostic_status() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.paste_clipboard_into_search(None);
    assert!(
        app.status.to_lowercase().contains("clipboard"),
        "expected diagnostic status mentioning the clipboard, got: {:?}",
        app.status
    );
}

#[test]
fn paste_clipboard_into_search_with_empty_text_sets_diagnostic_status() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.paste_clipboard_into_search(Some(""));
    assert!(
        app.status.to_lowercase().contains("clipboard"),
        "expected diagnostic status mentioning the clipboard, got: {:?}",
        app.status
    );
}

#[test]
fn unmatched_cmd_key_in_search_logs_diagnostic_status() {
    // If iTerm or the kitty CSI u path delivers a Cmd+<letter> the search
    // handler doesn't recognise, leave a breadcrumb so the user can tell
    // us exactly what crossterm saw — this is how we'll diagnose Cmd+V
    // delivery problems without an interactive debugger.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.handle_search_key(key(KeyCode::Char('q'), KeyModifiers::SUPER));
    assert!(
        app.status.contains("Cmd+"),
        "expected unhandled-cmd diagnostic, got: {:?}",
        app.status
    );
}

#[test]
fn cmd_v_is_recognised_as_search_paste_key() {
    assert!(is_search_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::SUPER
    )));
    assert!(is_search_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::CONTROL
    )));
    assert!(is_search_paste_key(key(
        KeyCode::Char('\u{16}'),
        KeyModifiers::NONE
    )));
    assert!(!is_search_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::NONE
    )));
    assert!(!is_search_paste_key(key(
        KeyCode::Char('a'),
        KeyModifiers::SUPER
    )));
}

#[test]
fn cmd_v_is_recognised_as_editor_paste_key() {
    assert!(is_editor_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::SUPER
    )));
    assert!(is_editor_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::CONTROL
    )));
    assert!(is_editor_paste_key(key(
        KeyCode::Char('\u{16}'),
        KeyModifiers::NONE
    )));
    assert!(!is_editor_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::NONE
    )));
    assert!(!is_editor_paste_key(key(
        KeyCode::Char('a'),
        KeyModifiers::SUPER
    )));
}

#[test]
fn left_click_on_paste_button_triggers_clipboard_paste_into_search() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.tree.last_area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    app.search.last_area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    app.search.last_inner = Rect {
        x: 1,
        y: 1,
        width: 58,
        height: 10,
    };
    app.search.paste_button_x = 40;
    app.search.paste_button_y = 1;
    app.search.paste_button_w = 5;
    let original_status = app.status.clone();
    let m = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 42,
        row: 1,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(m);
    assert_ne!(
        app.status, original_status,
        "click on Paste button must invoke the clipboard paste path (status updates whether clipboard read succeeds, fails, or returns empty)"
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
fn cmd_p_index_refreshes_after_a_new_file_lands_on_disk() {
    // Regression: the Cmd+P index was built once at App::new and
    // never refreshed, so files created later (by another process,
    // by a git checkout that swapped the working tree, or by an
    // in-app create) stayed invisible to Cmd+P until croft was
    // restarted. drain_fs_events now triggers kick_file_finder_index_rebuild
    // so creates and deletes propagate into the haystack the
    // finder ranks against.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), b"").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Wait for the initial background index to land.
    let mut waited = 0u32;
    loop {
        app.poll_file_finder_index();
        if app.file_finder_index.is_some() {
            break;
        }
        if waited >= 5000 {
            panic!("initial index must land within 5s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    // Drain any startup FS-watcher noise so the next loop reacts
    // only to the file we are about to create.
    for _ in 0..20 {
        let _ = app.drain_fs_events();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let new_dir = tmp
        .path()
        .join("packages/ant-ts-lib-noggin/src/entities/files");
    std::fs::create_dir_all(&new_dir).unwrap();
    std::fs::write(new_dir.join("resolver.ts"), b"export {};\n").unwrap();
    let started = std::time::Instant::now();
    let mut saw = false;
    for _ in 0..400 {
        let _ = app.drain_fs_events();
        app.poll_file_finder_index();
        if let Some(idx) = app.file_finder_index.as_ref()
            && idx
                .iter()
                .any(|e| e.rel == "packages/ant-ts-lib-noggin/src/entities/files/resolver.ts")
        {
            saw = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        saw,
        "Cmd+P index must include resolver.ts within a few seconds of it being written to disk; without this, branch-new files stay invisible to Cmd+P until croft restarts"
    );
    assert!(
        started.elapsed() <= std::time::Duration::from_secs(5),
        "index refresh must finish within 5s of the FS event on a 2-file workspace"
    );
}

#[test]
fn cmd_p_index_drops_a_file_deleted_off_disk() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), b"").unwrap();
    let target = tmp.path().join("doomed.rs");
    std::fs::write(&target, b"").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut waited = 0u32;
    loop {
        app.poll_file_finder_index();
        if let Some(idx) = app.file_finder_index.as_ref()
            && idx.iter().any(|e| e.rel == "doomed.rs")
        {
            break;
        }
        if waited >= 5000 {
            panic!("initial index must contain doomed.rs within 5s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    for _ in 0..20 {
        let _ = app.drain_fs_events();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::fs::remove_file(&target).unwrap();
    let mut gone = false;
    for _ in 0..400 {
        let _ = app.drain_fs_events();
        app.poll_file_finder_index();
        if let Some(idx) = app.file_finder_index.as_ref()
            && !idx.iter().any(|e| e.rel == "doomed.rs")
        {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        gone,
        "deleted file must drop out of the Cmd+P index after the FS-event-driven rebuild lands"
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

#[test]
fn open_cmd_p_finder_re_ranks_in_place_when_the_index_swaps() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), b"").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut waited = 0u32;
    loop {
        app.poll_file_finder_index();
        if app.file_finder_index.is_some() {
            break;
        }
        if waited >= 5000 {
            panic!("initial index must land within 5s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    // Open Cmd+P, type the new filename — initially zero results.
    app.open_file_finder();
    for c in "resolver.ts".chars() {
        app.file_finder.as_mut().unwrap().push_char(c);
    }
    assert!(
        app.file_finder
            .as_ref()
            .unwrap()
            .visible_results()
            .is_empty(),
        "resolver.ts is not on disk yet; finder must show no matches before the file is created"
    );
    let new_dir = tmp
        .path()
        .join("packages/ant-ts-lib-noggin/src/entities/files");
    std::fs::create_dir_all(&new_dir).unwrap();
    std::fs::write(new_dir.join("resolver.ts"), b"").unwrap();
    let mut surfaced = false;
    for _ in 0..400 {
        let _ = app.drain_fs_events();
        app.poll_file_finder_index();
        let finder = app.file_finder.as_ref().unwrap();
        if finder
            .visible_results()
            .iter()
            .any(|r| r.entry.rel == "packages/ant-ts-lib-noggin/src/entities/files/resolver.ts")
        {
            surfaced = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        surfaced,
        "an open Cmd+P finder must re-rank against the rebuilt index without the user closing and reopening it"
    );
}

#[test]
fn click_on_tree_row_aligned_with_terminal_splitter_y_selects_the_row_not_the_splitter() {
    // Regression for the user's "I can't click on the last two files,
    // but arrow+Enter opens them" report. Root cause: the editor /
    // terminal splitter handler at handle_mouse checks `m.row == y
    // || m.row == y.saturating_sub(1)` WITHOUT gating by column, so
    // a click on a tree row in the LEFT column whose y happens to
    // line up with the editor/terminal splitter (which only lives
    // in the RIGHT column) was hijacked into a splitter-drag and
    // never reached the Explorer's `node_at_y` branch.
    let tmp = tempfile::tempdir().unwrap();
    for name in [
        "alpha.txt",
        "beta.txt",
        "gamma.txt",
        "delta.txt",
        "epsilon.txt",
        "zeta.txt",
    ] {
        std::fs::write(tmp.path().join(name), b"").unwrap();
    }
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Layout: activity-bar(4) + tree-pane on the left, editor +
    // terminal stacked on the right. Pick a splitter row that
    // intersects one of the rendered tree rows.
    app.tree.last_area = Rect {
        x: 4,
        y: 0,
        width: 32,
        height: 12,
    };
    app.tree.last_inner = Rect {
        x: 5,
        y: 1,
        width: 30,
        height: 10,
    };
    app.sidebar_splitter_x = Some(36);
    // Splitter sits inside the tree's rendered row band.
    app.terminal_splitter_y = Some(6);
    // The clicked tree row's y MUST equal terminal_splitter_y to
    // trigger the bug; column stays well inside tree.last_inner.x..
    let click_row = 6u16;
    let click_col = app.tree.last_inner.x + 2;
    let target_idx = (click_row - app.tree.last_inner.y) as usize;
    assert!(
        target_idx < app.tree.nodes.len(),
        "test setup must put a real tree row at the splitter y"
    );
    let click = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: click_col,
        row: click_row,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click);
    assert!(
        app.splitter_drag.is_none(),
        "click in the LEFT (tree) column must never arm a splitter drag, even when the row y lines up with the editor/terminal splitter that lives in the RIGHT column"
    );
    assert_eq!(
        app.tree.selected, target_idx,
        "the tree row under the click must become selected; the user reported the bottom rows of the tree being unclickable even though they were plainly visible"
    );
}

#[test]
fn double_clicking_directory_toggles_only_once() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub").join("child.txt"), "hi").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.tree.last_area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    app.tree.last_inner = Rect {
        x: 1,
        y: 1,
        width: 38,
        height: 6,
    };
    let sub_idx = app
        .tree
        .nodes
        .iter()
        .position(|node| node.path.ends_with("sub"))
        .unwrap();
    let row = app.tree.last_inner.y + sub_idx as u16;
    let click = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 6,
        row,
        modifiers: KeyModifiers::NONE,
    };

    app.handle_mouse(click);
    let sub_idx = app
        .tree
        .nodes
        .iter()
        .position(|node| node.path.ends_with("sub"))
        .unwrap();
    assert!(app.tree.nodes[sub_idx].expanded);
    let expanded_len = app.tree.nodes.len();

    app.handle_mouse(click);
    let sub_idx = app
        .tree
        .nodes
        .iter()
        .position(|node| node.path.ends_with("sub"))
        .unwrap();
    assert!(app.tree.nodes[sub_idx].expanded);
    assert_eq!(app.tree.nodes.len(), expanded_len);
}

#[test]
fn typing_after_select_all_replaces_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("alpha");
    app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
    app.handle_search_key(key(KeyCode::Char('z'), KeyModifiers::NONE));
    assert_eq!(app.search.query, "z");
}

#[test]
fn shift_arrow_extends_tree_selection() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "").unwrap();
    std::fs::write(tmp.path().join("c.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.tree.select_replace(1);
    app.handle_tree_key(key(KeyCode::Down, KeyModifiers::SHIFT));
    app.handle_tree_key(key(KeyCode::Down, KeyModifiers::SHIFT));
    assert_eq!(app.tree.action_paths().len(), 3);
}

#[test]
fn cmd_a_in_tree_marks_all_visible() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_tree_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
    assert_eq!(app.tree.marked.len(), app.tree.nodes.len());
}

#[test]
fn esc_in_tree_clears_marks() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.tree.toggle_mark(1);
    assert!(!app.tree.marked.is_empty());
    app.handle_tree_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.tree.marked.is_empty());
}

#[test]
fn cmd_c_then_cmd_v_copies_explorer_paths_into_target() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("src.txt"), "hello").unwrap();
    std::fs::create_dir(tmp.path().join("dest")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let src_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("src.txt"))
        .unwrap();
    app.tree.select_replace(src_idx);
    app.handle_tree_key(key(KeyCode::Char('c'), KeyModifiers::SUPER));
    let dest_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("dest"))
        .unwrap();
    app.tree.select_replace(dest_idx);
    app.handle_tree_key(key(KeyCode::Char('v'), KeyModifiers::SUPER));
    let dest_file = tmp.path().join("dest/src.txt");
    assert!(dest_file.exists(), "copy must place file in target dir");
    assert!(
        tmp.path().join("src.txt").exists(),
        "copy must keep the source"
    );
}

#[test]
fn cmd_x_then_cmd_v_moves_explorer_paths_into_target() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("src.txt"), "hello").unwrap();
    std::fs::create_dir(tmp.path().join("dest")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let src_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("src.txt"))
        .unwrap();
    app.tree.select_replace(src_idx);
    app.handle_tree_key(key(KeyCode::Char('x'), KeyModifiers::SUPER));
    let dest_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("dest"))
        .unwrap();
    app.tree.select_replace(dest_idx);
    app.handle_tree_key(key(KeyCode::Char('v'), KeyModifiers::SUPER));
    assert!(tmp.path().join("dest/src.txt").exists());
    assert!(
        !tmp.path().join("src.txt").exists(),
        "cut must remove source"
    );
    assert!(
        app.tree_clipboard.is_none(),
        "cut clipboard must be consumed on paste"
    );
}

#[test]
fn delete_key_trashes_every_marked_path() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let a_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("a.txt"))
        .unwrap();
    let b_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("b.txt"))
        .unwrap();
    app.tree.select_replace(a_idx);
    app.tree.toggle_mark(b_idx);
    app.handle_tree_key(key(KeyCode::Delete, KeyModifiers::NONE));
    assert!(!tmp.path().join("a.txt").exists());
    assert!(!tmp.path().join("b.txt").exists());
}

#[test]
fn drag_drop_moves_marked_paths_to_target_dir() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("src.txt"), "x").unwrap();
    std::fs::create_dir(tmp.path().join("dest")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.tree.last_area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    app.tree.last_inner = Rect {
        x: 1,
        y: 1,
        width: 38,
        height: 6,
    };
    let src_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("src.txt"))
        .unwrap();
    let dest_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("dest"))
        .unwrap();
    let src_row = app.tree.last_inner.y + src_idx as u16;
    let dest_row = app.tree.last_inner.y + dest_idx as u16;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 6,
        row: src_row,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 6,
        row: dest_row,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 6,
        row: dest_row,
        modifiers: KeyModifiers::NONE,
    });
    assert!(tmp.path().join("dest/src.txt").exists());
    assert!(!tmp.path().join("src.txt").exists());
}

#[test]
fn alt_drag_copies_instead_of_moves() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("src.txt"), "x").unwrap();
    std::fs::create_dir(tmp.path().join("dest")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.tree.last_area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    app.tree.last_inner = Rect {
        x: 1,
        y: 1,
        width: 38,
        height: 6,
    };
    let src_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("src.txt"))
        .unwrap();
    let dest_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("dest"))
        .unwrap();
    let src_row = app.tree.last_inner.y + src_idx as u16;
    let dest_row = app.tree.last_inner.y + dest_idx as u16;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 6,
        row: src_row,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 6,
        row: dest_row,
        modifiers: KeyModifiers::ALT,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 6,
        row: dest_row,
        modifiers: KeyModifiers::ALT,
    });
    assert!(tmp.path().join("dest/src.txt").exists());
    assert!(
        tmp.path().join("src.txt").exists(),
        "Alt-drag must keep the source"
    );
}

#[test]
fn shift_click_extends_marks_from_anchor() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "").unwrap();
    std::fs::write(tmp.path().join("c.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.tree.last_area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    app.tree.last_inner = Rect {
        x: 1,
        y: 1,
        width: 38,
        height: 6,
    };
    let first_idx = 1usize;
    let last_idx = app.tree.nodes.len() - 1;
    let first_row = app.tree.last_inner.y + first_idx as u16;
    let last_row = app.tree.last_inner.y + last_idx as u16;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 6,
        row: first_row,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 6,
        row: first_row,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 6,
        row: last_row,
        modifiers: KeyModifiers::SHIFT,
    });
    let span = last_idx - first_idx + 1;
    assert_eq!(app.tree.action_paths().len(), span);
}

#[test]
fn alt_click_toggles_individual_mark_on_release() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.tree.last_area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    app.tree.last_inner = Rect {
        x: 1,
        y: 1,
        width: 38,
        height: 6,
    };
    let a_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("a.txt"))
        .unwrap();
    let b_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("b.txt"))
        .unwrap();
    let a_row = app.tree.last_inner.y + a_idx as u16;
    let b_row = app.tree.last_inner.y + b_idx as u16;
    for row in [a_row, b_row] {
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 6,
            row,
            modifiers: KeyModifiers::ALT,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 6,
            row,
            modifiers: KeyModifiers::ALT,
        });
    }
    let marked: BTreeSet<PathBuf> = app.tree.marked.clone();
    assert!(marked.contains(&tmp.path().join("a.txt")));
    assert!(marked.contains(&tmp.path().join("b.txt")));
}

#[test]
fn ctrl_click_also_toggles_individual_mark() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.tree.last_area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    app.tree.last_inner = Rect {
        x: 1,
        y: 1,
        width: 38,
        height: 6,
    };
    let a_idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("a.txt"))
        .unwrap();
    let row = app.tree.last_inner.y + a_idx as u16;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 6,
        row,
        modifiers: KeyModifiers::CONTROL,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 6,
        row,
        modifiers: KeyModifiers::CONTROL,
    });
    assert!(app.tree.marked.contains(&tmp.path().join("a.txt")));
}

#[test]
fn alt_click_does_not_open_the_file() {
    // Toggle is meant to mark — never to open. The deferred-toggle flow
    // must not reach `editor.open_preview` when Alt/Ctrl is held.
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "hi").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.tree.last_area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    app.tree.last_inner = Rect {
        x: 1,
        y: 1,
        width: 38,
        height: 6,
    };
    let idx = app.tree.nodes.iter().position(|n| n.path == f).unwrap();
    let row = app.tree.last_inner.y + idx as u16;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 6,
        row,
        modifiers: KeyModifiers::ALT,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 6,
        row,
        modifiers: KeyModifiers::ALT,
    });
    assert!(
        app.editor.is_blank_initial(),
        "alt-click must not open the file"
    );
}

#[test]
fn parse_dropped_paths_accepts_a_single_existing_absolute_path() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("dropped.txt");
    std::fs::write(&f, "hi").unwrap();
    let parsed = parse_dropped_paths(&format!("{}\n", f.display()));
    assert_eq!(parsed, vec![f]);
}

#[test]
fn parse_dropped_paths_drops_iterm2_inline_image_drag_artefact() {
    // iTerm2 lets the user drag any OSC-1337 inline image (which is
    // how croft paints activity-bar icons / welcome wordmark / SSH
    // hero) out of the terminal. On mouse-up iTerm2 writes the bytes
    // to $TMPDIR/iTerm2.<rand>.png and re-injects the path as a
    // bracketed paste, which croft would otherwise see as "the user
    // dragged a file into the editor" and open as an image preview.
    // That has happened to the user accidentally when they tried to
    // click a sidebar icon and the click drifted into a drag — the
    // file `iTerm2.5AO7K5.png` opened as a tab showing the Explorer
    // glyph. The drop must be silently filtered.
    let tmp = tempfile::tempdir().unwrap();
    let temp_dir = tmp.path().join("T").join("com.googlecode.iterm2");
    std::fs::create_dir_all(&temp_dir).unwrap();
    let fake = temp_dir.join("iTerm2.5AO7K5.png");
    std::fs::write(&fake, b"\x89PNG\r\n\x1a\n").unwrap();
    let parsed = parse_dropped_paths(&format!("{}\n", fake.display()));
    assert!(
        parsed.is_empty(),
        "iTerm2's inline-image drag temp file must be filtered out so a stray click-drag on an activity-bar icon does not open a phantom PNG tab — got {parsed:?}"
    );
}

#[test]
fn parse_dropped_paths_keeps_user_pngs_that_happen_to_share_a_temp_dir() {
    // The filter must key on the `iTerm2.<rand>.png` filename
    // pattern, not just "anything under /tmp". A real user-authored
    // PNG that happens to live under a temp directory must still be
    // accepted, otherwise the heuristic over-fires.
    let tmp = tempfile::tempdir().unwrap();
    let temp_dir = tmp.path().join("T");
    std::fs::create_dir_all(&temp_dir).unwrap();
    let user_png = temp_dir.join("my_screenshot.png");
    std::fs::write(&user_png, b"\x89PNG\r\n\x1a\n").unwrap();
    let parsed = parse_dropped_paths(&format!("{}\n", user_png.display()));
    assert_eq!(
        parsed,
        vec![user_png.clone()],
        "a user-named PNG under a temp dir must still be droppable — only the literal iTerm2.<rand>.png pattern is the artefact"
    );
}

#[test]
fn parse_dropped_paths_strips_file_url_prefix_and_decodes() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("with space.txt");
    std::fs::write(&f, "hi").unwrap();
    let url = format!("file://{}/with%20space.txt", tmp.path().to_string_lossy());
    let parsed = parse_dropped_paths(&url);
    assert_eq!(parsed, vec![f]);
}

#[test]
fn parse_dropped_paths_handles_backslash_escaped_spaces() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("with space.txt");
    std::fs::write(&f, "hi").unwrap();
    let escaped = format!("{}/with\\ space.txt", tmp.path().to_string_lossy());
    let parsed = parse_dropped_paths(&escaped);
    assert_eq!(parsed, vec![f]);
}

#[test]
fn parse_dropped_paths_supports_multi_file_drop_via_newlines() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "1").unwrap();
    std::fs::write(&b, "2").unwrap();
    let payload = format!("{}\n{}\n", a.display(), b.display());
    let parsed = parse_dropped_paths(&payload);
    assert_eq!(parsed.len(), 2);
    assert!(parsed.contains(&a));
    assert!(parsed.contains(&b));
}

#[test]
fn parse_dropped_paths_supports_multi_file_drop_via_unescaped_spaces() {
    // iTerm2's Finder-drop format for two files with spaces in their
    // names is `<path1> <path2>`, where spaces inside a path are
    // backslash-escaped and the separator between paths is a literal
    // space. Make sure both paths come through.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("02 Аленький цветочек.mp3");
    let b = tmp.path().join("03 Буратино.mp3");
    std::fs::write(&a, "x").unwrap();
    std::fs::write(&b, "y").unwrap();
    let payload = format!(
        "{}/02\\ Аленький\\ цветочек.mp3 {}/03\\ Буратино.mp3",
        tmp.path().to_string_lossy(),
        tmp.path().to_string_lossy(),
    );
    let parsed = parse_dropped_paths(&payload);
    assert_eq!(parsed.len(), 2, "expected two paths, got {parsed:?}");
    assert!(parsed.contains(&a));
    assert!(parsed.contains(&b));
}

#[test]
fn parse_dropped_paths_keeps_quoted_path_with_space_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("with space.txt");
    std::fs::write(&a, "x").unwrap();
    let payload = format!("\"{}\"", a.display());
    let parsed = parse_dropped_paths(&payload);
    assert_eq!(parsed, vec![a]);
}

#[test]
fn parse_dropped_paths_handles_mix_of_quoted_and_escaped_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("plain.txt");
    let b = tmp.path().join("with space.txt");
    std::fs::write(&a, "x").unwrap();
    std::fs::write(&b, "y").unwrap();
    let payload = format!("{} \"{}\"", a.display(), b.display(),);
    let parsed = parse_dropped_paths(&payload);
    assert_eq!(parsed.len(), 2, "got {parsed:?}");
    assert!(parsed.contains(&a));
    assert!(parsed.contains(&b));
}

#[test]
fn parse_dropped_paths_returns_empty_for_plain_text() {
    // Plain typed text must not be hijacked as a drop import.
    let parsed = parse_dropped_paths("hello world this is some text");
    assert!(parsed.is_empty());
}

#[test]
fn parse_dropped_paths_skips_nonexistent_paths() {
    let parsed = parse_dropped_paths("/this/path/definitely/does/not/exist");
    assert!(parsed.is_empty());
}

#[test]
fn finder_drop_into_explorer_moves_file_into_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let src = outside.path().join("dragged.txt");
    std::fs::write(&src, "hello").unwrap();
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Tree);
    app.set_sidebar_view(SidebarView::Explorer);
    app.handle_paste(&format!("{}\n", src.display()));
    let dest = workspace.path().join("dragged.txt");
    assert!(dest.exists(), "file should land in the workspace");
    assert!(!src.exists(), "Finder drop must move, not copy");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello");
}

#[test]
fn finder_drop_on_remote_view_queues_scp_even_when_terminal_focused() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let src = outside.path().join("dragged.txt");
    std::fs::write(&src, "hello").unwrap();
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    app.remote.targets = vec![crate::remote::RemoteTarget {
        alias: String::from("alpha"),
        host_name: Some(String::from("alpha.example.com")),
        user: Some(String::from("vitali")),
    }];
    app.remote.selected = 0;
    app.set_sidebar_view(SidebarView::Remote);
    // The user clicked into the embedded terminal and never returned
    // focus to the sidebar before dragging from Finder. iTerm2's
    // drag-drop arrives as a bracketed paste; mouse focus does not
    // shift on drop.
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);
    app.handle_paste(&format!("{}\n", src.display()));
    let queued = app.take_pending_scp_uploads();
    assert_eq!(queued.len(), 1, "one scp upload should be queued");
    assert_eq!(queued[0].alias, "alpha");
    assert_eq!(queued[0].src, src);
    assert!(src.exists(), "scp must copy, not move");
}

#[test]
fn finder_drop_on_remote_view_queues_scp_even_when_editor_focused() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let src = outside.path().join("e.txt");
    std::fs::write(&src, "x").unwrap();
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    app.remote.targets = vec![crate::remote::RemoteTarget {
        alias: String::from("beta"),
        host_name: None,
        user: None,
    }];
    app.remote.selected = 0;
    app.set_sidebar_view(SidebarView::Remote);
    app.focus_pane(Pane::Editor);
    let editor_before = app.editor.lines.clone();
    app.handle_paste(&format!("{}\n", src.display()));
    assert_eq!(
        app.editor.lines, editor_before,
        "remote drop must not leak the path text into the editor",
    );
    let queued = app.take_pending_scp_uploads();
    assert_eq!(queued.len(), 1);
}

#[test]
fn finder_drop_on_remote_view_with_no_target_reports_clear_status() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let src = outside.path().join("dragged.txt");
    std::fs::write(&src, "hi").unwrap();
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    app.remote.targets.clear();
    app.remote.selected = 0;
    app.set_sidebar_view(SidebarView::Remote);
    app.focus_pane(Pane::Tree);
    app.handle_paste(&format!("{}\n", src.display()));
    assert!(app.take_pending_scp_uploads().is_empty());
    assert!(
        app.status.contains("no Remote Explorer host"),
        "status must explain the failure, was: {:?}",
        app.status,
    );
    assert!(
        src.exists(),
        "with no host the source file must be left intact",
    );
}

fn relay_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[test]
fn remote_launched_drop_queues_pull_request_via_relay_log() {
    let _guard = relay_test_lock().lock().unwrap();
    // Simulates the case where the user dragged a Finder file onto a
    // remote-launched croft. The Mac path doesn't exist on the remote
    // box, but the parent local-croft has plumbed the relay env, so
    // the drop is recorded in the request log instead of being
    // pasted into the editor.
    let workspace = tempfile::tempdir().unwrap();
    let relay = tempfile::tempdir().unwrap();
    let log = relay.path().join("requests.log");
    let inbox = relay.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    // SAFETY: tests are single-threaded for env var manipulation.
    unsafe {
        std::env::set_var("CROFT_DROP_RELAY_LOG", &log);
        std::env::set_var("CROFT_DROP_RELAY_INBOX", &inbox);
    }
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Explorer);
    app.focus_pane(Pane::Tree);
    app.handle_paste("/Users/vitali/Documents/foo.txt\n");
    unsafe {
        std::env::remove_var("CROFT_DROP_RELAY_LOG");
        std::env::remove_var("CROFT_DROP_RELAY_INBOX");
    }
    let written = std::fs::read_to_string(&log).expect("relay log was written");
    assert!(
        written.starts_with("pull\t"),
        "relay log entry must be a pull request, got: {written:?}",
    );
    assert!(
        written.contains("/Users/vitali/Documents/foo.txt"),
        "relay log must carry the foreign path, got: {written:?}",
    );
    assert_eq!(app.pending_remote_pulls.len(), 1);
}

#[test]
fn drain_remote_pulls_imports_file_when_relay_signals_ok() {
    let _guard = relay_test_lock().lock().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let relay = tempfile::tempdir().unwrap();
    let inbox = relay.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    unsafe {
        std::env::set_var("CROFT_DROP_RELAY_INBOX", &inbox);
    }
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Explorer);
    app.focus_pane(Pane::Tree);
    let request_id = String::from("test-req-1");
    let request_dir = inbox.join(&request_id);
    std::fs::create_dir_all(&request_dir).unwrap();
    let staged = request_dir.join("foo.txt");
    std::fs::write(&staged, "payload").unwrap();
    std::fs::write(request_dir.join(".ok"), b"").unwrap();
    app.pending_remote_pulls.push(PendingRemotePull {
        request_id: request_id.clone(),
        src_display: String::from("/Users/v/Docs/foo.txt"),
        basename: String::from("foo.txt"),
        dest_dir: workspace.path().to_path_buf(),
        started_at: std::time::Instant::now(),
        kind: RemotePullKind::File,
    });
    let changed = app.drain_remote_pulls();
    unsafe {
        std::env::remove_var("CROFT_DROP_RELAY_INBOX");
    }
    assert!(changed);
    assert!(app.pending_remote_pulls.is_empty());
    let landed = workspace.path().join("foo.txt");
    assert!(landed.exists(), "file should land in workspace");
    assert_eq!(std::fs::read_to_string(&landed).unwrap(), "payload");
    assert!(
        !request_dir.exists(),
        "relay request dir must be cleaned up after import",
    );
    assert!(app.status.contains("Pulled"));
}

#[test]
fn drain_remote_pulls_surfaces_relay_error_message() {
    let _guard = relay_test_lock().lock().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let relay = tempfile::tempdir().unwrap();
    let inbox = relay.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    unsafe {
        std::env::set_var("CROFT_DROP_RELAY_INBOX", &inbox);
    }
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    let request_dir = inbox.join("req-2");
    std::fs::create_dir_all(&request_dir).unwrap();
    std::fs::write(request_dir.join(".err"), b"scp exited with 1").unwrap();
    app.pending_remote_pulls.push(PendingRemotePull {
        request_id: String::from("req-2"),
        src_display: String::from("/Users/v/missing.txt"),
        basename: String::from("missing.txt"),
        dest_dir: workspace.path().to_path_buf(),
        started_at: std::time::Instant::now(),
        kind: RemotePullKind::File,
    });
    let changed = app.drain_remote_pulls();
    unsafe {
        std::env::remove_var("CROFT_DROP_RELAY_INBOX");
    }
    assert!(changed);
    assert!(app.status.contains("scp exited with 1"));
}

#[test]
fn explorer_drop_on_terminal_focus_does_not_hijack_text_paste() {
    // Regression guard: when sidebar is Explorer and focus is on the
    // embedded terminal, pasting still goes to the terminal so the
    // user can type a path into a shell command.
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let src = outside.path().join("typed.txt");
    std::fs::write(&src, "x").unwrap();
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Explorer);
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);
    app.handle_paste(&format!("{}\n", src.display()));
    assert!(
        src.exists(),
        "explorer-with-terminal-focus drop must NOT move the source",
    );
    assert!(!workspace.path().join("typed.txt").exists());
}

#[test]
fn closing_image_tab_requests_overlay_clear_on_next_render() {
    // Repro for: opening a PNG/PDF then closing the tab leaves the
    // OSC-1337 pixels bleeding through the welcome screen. The render
    // path that swaps to welcome must mark the editor image overlay
    // for clearing so the main loop wipes the screen.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.editor[0].set_test_state(
        Some(String::from("dummy-osc")),
        Some(EditorImageLayout {
            cell_x: 0,
            cell_y: 0,
            cell_w: 10,
            cell_h: 10,
            path: tmp.path().join("doomed.png"),
        }),
        true,
    );
    // Editor is blank-initial right after construction, so calling
    // disable_editor_image directly mimics what render() does on
    // the welcome branch.
    app.disable_editor_image(0);
    assert!(app.overlays.editor[0].image_is_none());
    assert!(app.overlays.editor[0].layout_is_none());
    assert!(
        app.consume_editor_image_clear(),
        "first call must report a pending clear"
    );
    assert!(
        !app.consume_editor_image_clear(),
        "second call must be a no-op"
    );
}

#[test]
fn dragging_sidebar_splitter_resizes_sidebar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Simulate a render so splitter coords are populated.
    app.sidebar_splitter_x = Some(36); // activity(4) + sidebar(32) = 36
    app.last_content_width = 60;
    app.last_content_height = 20;
    app.sidebar_width = 32;
    // Mouse-down on the splitter column.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 36,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.splitter_drag, Some(SplitterDrag::Sidebar));
    // Drag right by 10 cells: column 46 -> sidebar should grow to 42.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 46,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.sidebar_width, 42);
    // Mouse-up clears the drag state.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 46,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert!(app.splitter_drag.is_none());
}

#[test]
fn sidebar_drag_grabs_one_column_left_of_seam() {
    // The seam is a single column wide. To make it easier to grab (and
    // to mirror the visible pair of borders the user sees), a click on
    // the cell immediately to its left also starts the drag.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_splitter_x = Some(36);
    app.last_content_width = 60;
    app.last_content_height = 20;
    app.sidebar_width = 32;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 35,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.splitter_drag, Some(SplitterDrag::Sidebar));
}

#[test]
fn welcome_paints_a_full_bordered_box_around_the_editor_area() {
    // Without a visible envelope, users on the welcome page can't
    // perceive that the explorer is resizable and can't see where the
    // editor pane ends. Regression guard: the welcome render must
    // paint a full Borders::ALL frame around `area` so all four edges
    // are perceptible.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    let area = ratatui::layout::Rect::new(40, 0, 80, 40);
    term.draw(|f| app.render_welcome(f, area)).unwrap();
    let buf = term.backend().buffer();
    let left = area.x;
    let right = area.x + area.width - 1;
    let top = area.y;
    let bot = area.y + area.height - 1;
    // Vertical edges: left and right columns must be vertical bars on
    // most rows (corners use ─/└/┌/┘).
    let mut left_bars = 0;
    let mut right_bars = 0;
    for y in (top + 1)..bot {
        if buf[(left, y)].symbol() == "│" {
            left_bars += 1;
        }
        if buf[(right, y)].symbol() == "│" {
            right_bars += 1;
        }
    }
    let inner_h = (bot - top - 1) as usize;
    assert!(
        left_bars >= inner_h / 2,
        "left edge underpainted: {left_bars}"
    );
    assert!(
        right_bars >= inner_h / 2,
        "right edge underpainted: {right_bars}"
    );
    // Horizontal edges: top and bottom rows must be horizontal bars on
    // most columns.
    let mut top_bars = 0;
    let mut bot_bars = 0;
    for x in (left + 1)..right {
        if buf[(x, top)].symbol() == "─" {
            top_bars += 1;
        }
        if buf[(x, bot)].symbol() == "─" {
            bot_bars += 1;
        }
    }
    let inner_w = (right - left - 1) as usize;
    assert!(top_bars >= inner_w / 2, "top edge underpainted: {top_bars}");
    assert!(
        bot_bars >= inner_w / 2,
        "bottom edge underpainted: {bot_bars}"
    );
    // Corners.
    assert_eq!(buf[(left, top)].symbol(), "┌");
    assert_eq!(buf[(right, top)].symbol(), "┐");
    assert_eq!(buf[(left, bot)].symbol(), "└");
    assert_eq!(buf[(right, bot)].symbol(), "┘");
}

#[test]
fn sidebar_drag_clamps_to_minimum() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_splitter_x = Some(36);
    app.last_content_width = 60;
    app.last_content_height = 20;
    app.sidebar_width = 32;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 36,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    // Drag far to the left — sidebar should clamp to its min, not collapse.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.sidebar_width, SIDEBAR_WIDTH_MIN);
}

#[test]
fn dragging_terminal_splitter_resizes_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminal_splitter_y = Some(15);
    app.last_content_height = 20;
    app.last_content_width = 60;
    app.terminal_height = Some(5); // bottom = 15 + 5 = 20
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 50,
        row: 15,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.splitter_drag, Some(SplitterDrag::Terminal));
    // Drag the splitter up to row 10: terminal height = 20 - 10 = 10.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 50,
        row: 10,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.terminal_height, Some(10));
}

#[test]
fn terminal_drag_grabs_one_row_above_seam() {
    // The terminal splitter sits on the terminal's top border row, but
    // the editor / welcome bottom border row (one row above) is also a
    // grabbable handle — symmetric with the sidebar's two-column zone.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminal_splitter_y = Some(15);
    app.last_content_height = 20;
    app.last_content_width = 60;
    app.terminal_height = Some(5);
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 50,
        row: 14,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.splitter_drag, Some(SplitterDrag::Terminal));
}

#[test]
fn fresh_app_has_one_active_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    assert_eq!(app.terminals.len(), 1);
    assert_eq!(app.active_terminal, 0);
}

#[test]
fn split_terminal_appends_and_activates_new_one() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    assert_eq!(app.terminals.len(), 2);
    assert_eq!(app.active_terminal, 1);
    app.split_terminal().unwrap();
    assert_eq!(app.terminals.len(), 3);
    assert_eq!(app.active_terminal, 2);
}

#[test]
fn split_terminal_inserts_to_the_right_of_the_active_terminal_not_at_the_end() {
    // Open three terminals so the strip is [T0, T1, T2] with T2
    // active. Focus T0 (the left-most), split. The new terminal must
    // land at index 1 — immediately to the right of where the user
    // was looking — and T1/T2 must shift right to indices 2 and 3,
    // matching the user's mental model that a new pane appears next
    // to the one they're working in, not at the far right edge.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    let pid_t0 = app.terminals[0].pid();
    let pid_t1 = app.terminals[1].pid();
    let pid_t2 = app.terminals[2].pid();
    app.active_terminal = 0;
    app.split_terminal().unwrap();
    assert_eq!(app.terminals.len(), 4);
    assert_eq!(
        app.active_terminal, 1,
        "a split from the left-most terminal must focus the new one at index 1, not at the far right"
    );
    assert_eq!(
        app.terminals[0].pid(),
        pid_t0,
        "the originally-active terminal must keep its slot"
    );
    assert_eq!(
        app.terminals[2].pid(),
        pid_t1,
        "the old neighbour to the right must shift one slot right, not stay put"
    );
    assert_eq!(
        app.terminals[3].pid(),
        pid_t2,
        "the right-most terminal must shift to the new right edge"
    );
}

#[test]
fn close_active_terminal_drops_the_active_one() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    assert_eq!(app.active_terminal, 2);
    assert!(app.close_active_terminal());
    assert_eq!(app.terminals.len(), 2);
    assert_eq!(app.active_terminal, 1);
    assert!(app.close_active_terminal());
    assert_eq!(app.terminals.len(), 1);
    assert_eq!(app.active_terminal, 0);
}

#[test]
fn close_active_terminal_refuses_to_drop_the_last_one() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(!app.close_active_terminal());
    assert_eq!(app.terminals.len(), 1);
    assert_eq!(app.active_terminal, 0);
}

#[test]
fn cycle_terminal_wraps_around() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    app.active_terminal = 0;
    app.cycle_terminal();
    assert_eq!(app.active_terminal, 1);
    app.cycle_terminal();
    assert_eq!(app.active_terminal, 2);
    app.cycle_terminal();
    assert_eq!(app.active_terminal, 0);
}

#[test]
fn cycle_terminal_back_wraps_around_to_the_left() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    app.active_terminal = 0;
    app.cycle_terminal_back();
    assert_eq!(
        app.active_terminal, 2,
        "cycle-back from the left edge must wrap to the right-most slot"
    );
    app.cycle_terminal_back();
    assert_eq!(app.active_terminal, 1);
    app.cycle_terminal_back();
    assert_eq!(app.active_terminal, 0);
}

#[test]
fn cmd_left_bracket_cycles_to_the_previous_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    app.focus = Pane::Terminal;
    app.active_terminal = 2;
    let _ = app.handle_key(key(KeyCode::Char('['), KeyModifiers::SUPER));
    assert_eq!(
        app.active_terminal, 1,
        "Cmd+[ must move focus one terminal to the left"
    );
    let _ = app.handle_key(key(KeyCode::Char('{'), KeyModifiers::SUPER));
    assert_eq!(
        app.active_terminal, 0,
        "Cmd+{{ (shift-applied glyph) must also cycle back"
    );
}

#[test]
fn cmd_right_bracket_cycles_to_the_next_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    app.focus = Pane::Terminal;
    app.active_terminal = 0;
    let _ = app.handle_key(key(KeyCode::Char(']'), KeyModifiers::SUPER));
    assert_eq!(
        app.active_terminal, 1,
        "Cmd+] must move focus one terminal to the right"
    );
    let _ = app.handle_key(key(KeyCode::Char('}'), KeyModifiers::SUPER));
    assert_eq!(
        app.active_terminal, 2,
        "Cmd+}} (shift-applied glyph) must also cycle forward"
    );
}

#[test]
fn ctrl_shift_t_globally_splits_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert_eq!(app.terminals.len(), 1);
    app.handle_key(key(
        KeyCode::Char('T'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.terminals.len(), 2);
    assert_eq!(app.active_terminal, 1);
}

#[test]
fn ctrl_shift_w_in_terminal_pane_closes_active() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.focus_pane(Pane::Terminal);
    app.handle_key(key(
        KeyCode::Char('W'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.terminals.len(), 1);
}

#[test]
fn clicking_terminal_add_button_splits() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminal_add_buttons = vec![Rect {
        x: 50,
        y: 30,
        width: 3,
        height: 1,
    }];
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 51,
        row: 30,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.terminals.len(), 2);
    assert_eq!(app.active_terminal, 1);
}

#[test]
fn clicking_terminal_close_button_drops_active_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    assert_eq!(app.terminals.len(), 2);
    assert_eq!(app.active_terminal, 1);
    app.terminal_close_buttons = vec![
        Rect {
            x: 10,
            y: 30,
            width: 3,
            height: 1,
        },
        Rect {
            x: 50,
            y: 30,
            width: 3,
            height: 1,
        },
    ];
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 51,
        row: 30,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.terminals.len(), 1);
}

#[test]
fn clicking_close_button_on_non_active_terminal_drops_that_specific_terminal() {
    // The user has three terminals open with the rightmost (idx 2) active;
    // clicking the [-] on the leftmost terminal must close idx 0, not the
    // active one. Regression for "I would like to kill any of them, not
    // only the right one."
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    assert_eq!(app.terminals.len(), 3);
    assert_eq!(app.active_terminal, 2);
    let pid_left = app.terminals[0].pid();
    let pid_right = app.terminals[2].pid();
    app.terminal_close_buttons = vec![
        Rect {
            x: 10,
            y: 30,
            width: 3,
            height: 1,
        },
        Rect {
            x: 30,
            y: 30,
            width: 3,
            height: 1,
        },
        Rect {
            x: 50,
            y: 30,
            width: 3,
            height: 1,
        },
    ];
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 11,
        row: 30,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.terminals.len(), 2);
    assert!(
        app.terminals.iter().all(|t| t.pid() != pid_left),
        "the leftmost terminal (the one whose [-] was clicked) must be gone"
    );
    assert_eq!(
        app.terminals[1].pid(),
        pid_right,
        "the formerly-rightmost terminal must remain"
    );
    assert_eq!(
        app.active_terminal, 1,
        "active terminal index must shift down so the same terminal stays active"
    );
}

#[test]
fn each_terminal_paints_its_own_add_close_buttons_when_multiple_open() {
    // Per-pane chrome: with N terminals visible there should be N [+] hit
    // rects (one per pane) and, when N > 1, N [-] hit rects so the user
    // can kill any one of them.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    assert_eq!(app.terminals.len(), 3);
    let backend = ratatui::backend::TestBackend::new(180, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(
        app.terminal_add_buttons.len(),
        3,
        "each terminal pane must record its own [+] hit rect"
    );
    assert_eq!(
        app.terminal_close_buttons.len(),
        3,
        "each terminal pane must record its own [-] hit rect when more than one is open"
    );
    // Distinct columns: the buttons must sit on different panes.
    let xs: std::collections::HashSet<u16> = app.terminal_add_buttons.iter().map(|r| r.x).collect();
    assert_eq!(
        xs.len(),
        3,
        "add-button rects must be on three distinct columns"
    );
}

#[test]
fn cwd_of_pid_on_self_returns_a_directory_under_tempdir() {
    // The harness runs each test from the crate root by default, but
    // we don't depend on that — we just need cwd_of_pid to round-trip
    // *some* directory for the current process. lsof / /proc gives us
    // the live cwd of the shell that started the test runner.
    let pid = std::process::id();
    match cwd_of_pid(pid) {
        Some(p) => assert!(
            p.is_dir(),
            "cwd lookup must return a real directory, got {p:?}"
        ),
        None => {
            // Acceptable on platforms where neither /proc nor lsof
            // resolve the cwd in the sandbox. The fallback in
            // split_terminal handles that case; nothing to assert here.
        }
    }
}

#[test]
fn add_button_wins_over_terminal_splitter_on_same_row() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Splitter and button share the row, as they do in real layout.
    app.terminal_splitter_y = Some(30);
    app.terminal_add_buttons = vec![Rect {
        x: 50,
        y: 30,
        width: 3,
        height: 1,
    }];
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 51,
        row: 30,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.splitter_drag.is_none(),
        "click on [+] must not start a splitter drag"
    );
    assert_eq!(app.terminals.len(), 2);
}

#[test]
fn peek_terminals_dirty_does_not_clear_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Fresh App starts dirty so the first frame paints.
    assert!(app.peek_terminals_dirty(), "fresh app should be dirty");
    // Two consecutive peeks must both see the flag - peek must not clear.
    assert!(
        app.peek_terminals_dirty(),
        "peek must not consume the dirty flag"
    );
    // Drain consumes it.
    let drained = app.drain_terminals_dirty();
    assert!(drained, "drain after peek should still report dirty once");
    assert!(
        !app.peek_terminals_dirty(),
        "after drain, peek must be clean"
    );
}

#[test]
fn cmd_shift_g_no_longer_jumps_to_source_control() {
    // Source Control is reached via Cmd+Shift+S; the old Cmd+Shift+G alias
    // was removed so the chord stays free for the editor's vim-style
    // "go to bottom of file".
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(
        KeyCode::Char('G'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_ne!(
        app.sidebar_view,
        SidebarView::SourceControl,
        "Cmd+Shift+G must no longer route to Source Control"
    );
}

#[test]
fn activity_bar_render_lays_out_run_debug_between_source_control_and_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let bar = Rect {
        x: 0,
        y: 0,
        width: ACTIVITY_BAR_WIDTH,
        height: 30,
    };
    let backend = ratatui::backend::TestBackend::new(40, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_activity_bar(f, bar)).unwrap();
    let scm = app.sidebar_areas.source_control_icon;
    let run = app.sidebar_areas.run_debug_icon;
    let rem = app.sidebar_areas.remote_icon;
    assert!(run.height > 0, "run-debug icon must claim a non-zero block");
    assert!(
        run.y > scm.y,
        "run-debug must sit BELOW source-control (scm.y={}, run.y={})",
        scm.y,
        run.y
    );
    assert!(
        run.y < rem.y,
        "run-debug must sit ABOVE remote (run.y={}, rem.y={}) — VS Code's activity-bar order",
        run.y,
        rem.y
    );
}

#[test]
fn click_on_run_debug_icon_switches_sidebar_to_run_debug() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(80, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let run = app.sidebar_areas.run_debug_icon;
    assert!(run.height > 0, "run-debug icon must be laid out");
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: run.x,
        row: run.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.sidebar_view, SidebarView::RunDebug);
}

#[test]
fn run_spec_for_python_with_no_venv_falls_back_to_system_python3() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("script.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert_eq!(spec.program, "python3");
    assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
    assert_eq!(spec.cwd, tmp.path());
}

#[test]
fn run_spec_for_node_picks_node_for_dot_js() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("server.js");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert_eq!(spec.program, "node");
    assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
}

#[test]
fn run_spec_for_bash_picks_bash_for_dot_sh() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("build.sh");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert_eq!(spec.program, "bash");
    assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
}

#[test]
fn run_spec_for_unknown_extension_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("notes.txt");
    std::fs::write(&f, b"").unwrap();
    assert!(App::run_spec_for(&f, tmp.path()).is_none());
}

#[test]
fn run_spec_passes_apostrophe_paths_through_argv_unescaped() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("O'Reilly.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert_eq!(
        spec.args,
        vec![f.to_string_lossy().into_owned()],
        "argv carries the path verbatim — no shell quoting because no shell is invoked"
    );
}

/// Drop a fake `bin/python` shim in `dir/.venv/bin/python` so the venv
/// detector finds an executable file. Returns the full path to the shim
/// for assertions.
fn make_venv(dir: &Path, venv_name: &str) -> PathBuf {
    let bin = dir.join(venv_name).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let py = bin.join("python");
    std::fs::write(&py, b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&py).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&py, perms).unwrap();
    }
    py
}

#[test]
fn run_spec_picks_dot_venv_python_in_the_file_directory_over_system_python3() {
    // Reproduces the user-reported case: croft is opened on a parent
    // directory, the file lives in a subfolder that owns a .venv, and
    // running the file with system python3 misses the project's deps.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let venv_python = make_venv(&project, ".venv");
    let f = project.join("main.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert_eq!(
        spec.program,
        venv_python.to_string_lossy().into_owned(),
        "venv interpreter must be the spawned program (looked for {})",
        venv_python.display()
    );
    assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
    assert_eq!(
        spec.cwd, project,
        "cwd must be the directory that owns the venv so relative imports / .env files resolve"
    );
}

#[test]
fn run_spec_walks_up_from_file_dir_to_find_venv_in_an_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    let nested = project.join("src").join("pkg");
    std::fs::create_dir_all(&nested).unwrap();
    make_venv(&project, ".venv");
    let f = nested.join("main.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert!(
        spec.program.ends_with(".venv/bin/python"),
        "expected venv python program, got: {}",
        spec.program
    );
    assert_eq!(spec.cwd, project);
}

#[test]
fn run_spec_also_picks_a_plain_venv_directory() {
    let tmp = tempfile::tempdir().unwrap();
    make_venv(tmp.path(), "venv");
    let f = tmp.path().join("main.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert!(
        spec.program.ends_with("venv/bin/python"),
        "expected plain-venv python program, got: {}",
        spec.program
    );
}

#[test]
fn run_spec_does_not_walk_above_workspace_root() {
    let tmp = tempfile::tempdir().unwrap();
    // venv lives ABOVE the workspace root — must be ignored.
    make_venv(tmp.path(), ".venv");
    let workspace = tmp.path().join("inner");
    std::fs::create_dir_all(&workspace).unwrap();
    let f = workspace.join("main.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, &workspace).unwrap();
    assert_eq!(
        spec.program, "python3",
        "must NOT use a venv that sits above the workspace root, got: {}",
        spec.program
    );
}

#[test]
fn run_active_file_with_no_open_file_records_feedback_and_does_not_spawn_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let term_count_before = app.terminals.len();
    app.run_active_file();
    assert_eq!(
        app.terminals.len(),
        term_count_before,
        "no file open: must NOT spawn an extra terminal"
    );
    assert!(app.run_debug.feedback_is_error);
    assert!(app.run_debug.feedback.is_some());
}

#[test]
fn run_active_file_with_unknown_extension_records_feedback_and_does_not_spawn_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("notes.txt");
    std::fs::write(&f, b"hi").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    let term_count_before = app.terminals.len();
    app.run_active_file();
    assert_eq!(app.terminals.len(), term_count_before);
    assert!(app.run_debug.feedback_is_error);
}

#[test]
fn run_active_file_with_python_file_spawns_a_new_terminal_and_focuses_it() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hello.py");
    std::fs::write(&f, b"print('hi')\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    let term_count_before = app.terminals.len();
    app.run_active_file();
    assert_eq!(
        app.terminals.len(),
        term_count_before + 1,
        "running a known extension must spawn a fresh terminal so the previous one's history isn't clobbered"
    );
    assert!(app.show_terminal, "terminal pane must become visible");
    assert!(matches!(app.focus, Pane::Terminal));
    assert!(!app.run_debug.feedback_is_error);
}

#[test]
fn focus_flag_only_set_on_active_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.focus_pane(Pane::Terminal);
    let active = app.active_terminal;
    for (i, t) in app.terminals.iter().enumerate() {
        assert_eq!(t.focused, i == active);
    }
    app.focus_pane(Pane::Editor);
    for t in &app.terminals {
        assert!(!t.focused);
    }
}

#[test]
fn f9_relaunches_only_when_update_is_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Not ready: F9 must not arm the re-exec.
    let _ = app.handle_key(key(KeyCode::F(9), KeyModifiers::empty()));
    assert!(!app.pending_reexec);
    assert!(!app.quit);

    // Ready: F9 arms the re-exec and quits the loop so `run` can exec.
    app.update_status = UpdateStatus::Ready;
    let _ = app.handle_key(key(KeyCode::F(9), KeyModifiers::empty()));
    assert!(app.pending_reexec);
    assert!(app.quit);
}

#[test]
fn session_state_round_trip_reopens_tabs_at_saved_cursor_and_active() {
    let tmp = tempfile::tempdir().unwrap();
    let file_a = tmp.path().join("a.txt");
    let file_b = tmp.path().join("b.txt");
    std::fs::write(&file_a, "line0\nline1\nline2\n").unwrap();
    std::fs::write(&file_b, "only\n").unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file_a).unwrap();
    app.editor.open_pinned(&file_b).unwrap();
    // Park the cursor on a.txt and make it the active tab.
    let idx_a = app
        .editor
        .editors
        .iter()
        .position(|e| e.path.as_deref() == Some(file_a.as_path()))
        .unwrap();
    app.editor.editors[idx_a].cursor_row = 2;
    app.editor.editors[idx_a].cursor_col = 3;
    app.editor.select(idx_a);

    let state = app.capture_session_state();
    assert_eq!(state.tabs.len(), 2);
    assert_eq!(
        state.tabs[state.active_tab].path.as_path(),
        file_a.as_path()
    );

    let mut restored = App::new(tmp.path().to_path_buf()).unwrap();
    restored.apply_session_state(&state);
    let restored_paths: Vec<_> = restored
        .editor
        .editors
        .iter()
        .filter_map(|e| e.path.clone())
        .collect();
    assert!(restored_paths.contains(&file_a));
    assert!(restored_paths.contains(&file_b));
    let active = &restored.editor.editors[restored.editor.active_index()];
    assert_eq!(active.path.as_deref(), Some(file_a.as_path()));
    assert_eq!(active.cursor_row, 2);
    assert_eq!(active.cursor_col, 3);
}

#[test]
fn session_state_preserves_unsaved_buffer_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("dirty.txt");
    std::fs::write(&file, "saved\n").unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    let idx = app.editor.active_index();
    app.editor.editors[idx].lines = vec![String::from("edited unsaved")];
    app.editor.editors[idx].dirty = true;

    let state = app.capture_session_state();
    let tab = state.tabs.iter().find(|t| t.path == file).unwrap();
    assert!(tab.dirty);
    assert_eq!(tab.unsaved_text.as_deref(), Some("edited unsaved"));

    let mut restored = App::new(tmp.path().to_path_buf()).unwrap();
    restored.apply_session_state(&state);
    let ed = restored
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(file.as_path()))
        .unwrap();
    assert!(ed.dirty);
    assert_eq!(ed.lines, vec![String::from("edited unsaved")]);
    // On-disk file must be untouched by the capture/restore.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "saved\n");
}

#[test]
fn build_tab_context_menu_items_includes_all_four_close_actions_in_the_middle_of_a_strip() {
    // Right-click on tab 1 of 3 (i.e. a middle tab) should surface
    // every close action — there's at least one tab on each side.
    let items = build_tab_context_menu_items(1, 3, false);
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Close",
            "Close Others",
            "Close to the Right",
            "Close All",
            "Split Editor Right"
        ],
        "all four close actions must appear and in this order — mirrors VS Code's tab strip"
    );
    assert!(matches!(&items[0].1, MenuAction::CloseTab(1)));
    assert!(matches!(&items[1].1, MenuAction::CloseOtherTabs(1)));
    assert!(matches!(&items[2].1, MenuAction::CloseTabsToRight(1)));
    assert!(matches!(&items[3].1, MenuAction::CloseAllTabs));
}

#[test]
fn build_tab_context_menu_items_hides_close_to_the_right_on_the_last_tab() {
    // Last tab → "Close to the Right" would close zero tabs, so it
    // must be suppressed (matches VS Code).
    let items = build_tab_context_menu_items(2, 3, false);
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        labels,
        ["Close", "Close Others", "Close All", "Split Editor Right"],
        "Close to the Right must be suppressed on the rightmost tab"
    );
}

#[test]
fn build_tab_context_menu_items_hides_close_others_when_only_one_tab_is_open() {
    let items = build_tab_context_menu_items(0, 1, false);
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        labels,
        ["Close", "Close All", "Split Editor Right"],
        "with a single tab, Close Others and Close to the Right are both no-ops and must be suppressed"
    );
}

#[test]
fn zoxide_jump_key_is_cmd_z_only_and_never_ctrl_z_or_redo() {
    // Cmd+Z (SUPER) opens the Explorer jump popup. Ctrl+Z must NOT — it
    // is the shell suspend in the terminal, and the editor's undo lives on
    // Ctrl/Cmd+Z in its own (editor-focused) path. Cmd+Shift+Z is the
    // reserved redo chord. Cmd+J stays free for terminal manipulation.
    assert!(is_tree_zoxide_jump_key(key(
        KeyCode::Char('z'),
        KeyModifiers::SUPER
    )));
    assert!(
        is_tree_zoxide_jump_key(key(KeyCode::Char('Z'), KeyModifiers::SUPER)),
        "letter match must be case-insensitive"
    );
    assert!(
        !is_tree_zoxide_jump_key(key(KeyCode::Char('z'), KeyModifiers::CONTROL)),
        "Ctrl+Z must not open the popup; it is shell suspend / editor undo"
    );
    assert!(
        !is_tree_zoxide_jump_key(key(
            KeyCode::Char('z'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT
        )),
        "Cmd+Shift+Z is reserved for redo and must not open the popup"
    );
    assert!(
        !is_tree_zoxide_jump_key(key(KeyCode::Char('j'), KeyModifiers::SUPER)),
        "Cmd+J must stay free for terminal manipulation"
    );
}

#[test]
fn zoxide_jump_popup_anchors_over_the_explorer_column_not_centered() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_explorer_shortcut(key(KeyCode::Char('z'), KeyModifiers::SUPER));
    assert!(app.zoxide_jump.is_some(), "precondition: popup is open");

    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let popup = app.zoxide_jump.as_ref().unwrap().last_rect;
    // The Explorer tree renders inside the sidebar column; the popup must
    // share that column's left edge (anchored over the Explorer), not be
    // centered on the 120-wide frame (which would put x near ~30).
    assert_eq!(
        popup.x, app.tree.last_area.x,
        "jump popup must be left-anchored to the Explorer column, got x={} vs sidebar x={}",
        popup.x, app.tree.last_area.x
    );
    assert!(
        popup.y <= app.tree.last_area.y,
        "jump popup must drop from the top of the Explorer, not be vertically centered"
    );
}

#[test]
fn cmd_z_in_explorer_opens_the_zoxide_jump_popup() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(
        app.is_explorer_focused(),
        "precondition: a fresh app is focused on the Explorer pane"
    );
    assert!(app.zoxide_jump.is_none(), "popup starts closed");
    let handled = app.handle_explorer_shortcut(key(KeyCode::Char('z'), KeyModifiers::SUPER));
    assert!(
        handled,
        "Cmd+Z must be consumed by the Explorer shortcut layer"
    );
    assert!(
        app.zoxide_jump.is_some(),
        "Cmd+Z in the Explorer must open the zoxide jump popup"
    );
    // Esc closes it again.
    app.handle_zoxide_jump_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.zoxide_jump.is_none(), "Esc must close the jump popup");
}

// --- Native modal (vim) editing -------------------------------------------

fn vim_app(content: &str) -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("buf.txt");
    std::fs::write(&f, content).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus = Pane::Editor;
    app.vim.enabled = true;
    (app, tmp)
}

fn vim_feed(app: &mut App, c: char) {
    app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
        .unwrap();
}

fn vim_feed_str(app: &mut App, s: &str) {
    for c in s.chars() {
        vim_feed(app, c);
    }
}

#[test]
fn vim_dd_deletes_the_current_line() {
    let (mut app, _t) = vim_app("alpha\nbeta\ngamma");
    vim_feed_str(&mut app, "dd");
    assert_eq!(app.editor.lines, vec!["beta", "gamma"]);
}

#[test]
fn vim_count_dd_deletes_multiple_lines() {
    let (mut app, _t) = vim_app("a\nb\nc\nd");
    vim_feed_str(&mut app, "2dd");
    assert_eq!(app.editor.lines, vec!["c", "d"]);
}

#[test]
fn vim_x_deletes_char_under_cursor() {
    let (mut app, _t) = vim_app("hello");
    vim_feed(&mut app, 'x');
    assert_eq!(app.editor.lines, vec!["ello"]);
}

#[test]
fn vim_dw_deletes_word_and_trailing_space() {
    let (mut app, _t) = vim_app("foo bar");
    vim_feed_str(&mut app, "dw");
    assert_eq!(app.editor.lines, vec!["bar"]);
}

#[test]
fn vim_de_deletes_to_word_end_inclusive() {
    let (mut app, _t) = vim_app("foo bar");
    vim_feed_str(&mut app, "de");
    assert_eq!(app.editor.lines, vec![" bar"]);
}

#[test]
fn vim_diw_deletes_inner_word() {
    let (mut app, _t) = vim_app("foo bar baz");
    // cursor on "foo"
    vim_feed_str(&mut app, "diw");
    assert_eq!(app.editor.lines, vec![" bar baz"]);
}

#[test]
fn vim_dollar_then_d_deletes_to_end_of_line() {
    let (mut app, _t) = vim_app("hello world");
    vim_feed_str(&mut app, "wd$");
    assert_eq!(app.editor.lines, vec!["hello "]);
}

#[test]
fn vim_insert_types_text_then_esc_returns_to_normal() {
    let (mut app, _t) = vim_app("bc");
    vim_feed(&mut app, 'i');
    vim_feed(&mut app, 'a');
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["abc"]);
    assert_eq!(app.vim.mode(), crate::vim::VimMode::Normal);
}

#[test]
fn vim_append_inserts_after_cursor() {
    let (mut app, _t) = vim_app("ac");
    vim_feed(&mut app, 'a'); // insert after the 'a'
    vim_feed(&mut app, 'b');
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["abc"]);
}

#[test]
fn vim_open_line_below_enters_insert_on_fresh_line() {
    let (mut app, _t) = vim_app("first");
    vim_feed(&mut app, 'o');
    vim_feed_str(&mut app, "second");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["first", "second"]);
}

#[test]
fn vim_yank_line_then_paste_duplicates_it() {
    let (mut app, _t) = vim_app("dup\ntail");
    vim_feed_str(&mut app, "yy");
    vim_feed(&mut app, 'p');
    assert_eq!(app.editor.lines, vec!["dup", "dup", "tail"]);
}

#[test]
fn vim_visual_select_then_delete_removes_selection() {
    let (mut app, _t) = vim_app("abcdef");
    vim_feed(&mut app, 'v'); // visual at col 0 (covers 'a')
    vim_feed(&mut app, 'l'); // extend to 'b'
    vim_feed(&mut app, 'l'); // extend to 'c'
    vim_feed(&mut app, 'd'); // delete a,b,c inclusive
    assert_eq!(app.editor.lines, vec!["def"]);
}

#[test]
fn vim_normal_mode_swallows_plain_letters() {
    let (mut app, _t) = vim_app("text");
    vim_feed(&mut app, 'z'); // unmapped in Normal: must not be inserted
    assert_eq!(app.editor.lines, vec!["text"]);
}

#[test]
fn vim_search_moves_cursor_to_match() {
    let (mut app, _t) = vim_app("alpha needle omega");
    vim_feed(&mut app, '/');
    vim_feed_str(&mut app, "needle");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 0);
    assert_eq!(
        app.editor.cursor_col, 6,
        "cursor lands on the 'n' of needle"
    );
}

#[test]
fn update_spinner_frame_cycles_through_all_frames_and_wraps() {
    // Frame 0 at the start, advances one frame per UPDATE_SPINNER_FRAME_MS.
    assert_eq!(update_spinner_frame(0), UPDATE_SPINNER_FRAMES[0]);
    assert_eq!(
        update_spinner_frame(UPDATE_SPINNER_FRAME_MS),
        UPDATE_SPINNER_FRAMES[1]
    );
    assert_eq!(
        update_spinner_frame(UPDATE_SPINNER_FRAME_MS * 5 + 50),
        UPDATE_SPINNER_FRAMES[5],
        "mid-frame elapsed still maps to the floor frame"
    );
    // Wraps back to frame 0 after a full cycle of all six frames.
    let cycle = UPDATE_SPINNER_FRAME_MS * UPDATE_SPINNER_FRAMES.len() as u128;
    assert_eq!(update_spinner_frame(cycle), UPDATE_SPINNER_FRAMES[0]);
    assert_eq!(
        update_spinner_frame(cycle + UPDATE_SPINNER_FRAME_MS),
        UPDATE_SPINNER_FRAMES[1]
    );
}

#[test]
fn vim_search_records_active_match_and_advances_with_n() {
    // Two matches on one line: "x needle y needle z" -> cols 2 and 11.
    let (mut app, _t) = vim_app("x needle y needle z");
    vim_feed(&mut app, '/');
    vim_feed_str(&mut app, "needle");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.active_search_match,
        Some((0, 2, 6)),
        "the match the cursor jumped to must be recorded as (row, col_chars, len_chars) so the renderer paints it orange while the other 'needle' stays yellow"
    );
    vim_feed(&mut app, 'n');
    assert_eq!(
        app.editor.active_search_match,
        Some((0, 11, 6)),
        "`n` advances the active match to the next occurrence"
    );
}

#[test]
fn vim_gg_and_capital_g_jump_between_ends() {
    let (mut app, _t) = vim_app("one\ntwo\nthree");
    app.handle_key(key(KeyCode::Char('G'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 2);
    vim_feed_str(&mut app, "gg");
    assert_eq!(app.editor.cursor_row, 0);
}

#[test]
fn vim_toggle_works_from_any_pane_even_with_no_editor_focus() {
    let (mut app, _t) = vim_app("abc");
    app.vim.enabled = false;
    // Focus the file tree, not the editor.
    app.focus = Pane::Tree;
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.vim.enabled,
        "Cmd+E must toggle vim mode globally, from any pane, so it can be pre-armed before opening or focusing a file"
    );
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        !app.vim.enabled,
        "a second Cmd+E from the tree toggles it back off"
    );
}

#[test]
fn vim_toggle_off_clears_search_highlights() {
    let (mut app, _t) = vim_app("x needle y needle z");
    vim_feed(&mut app, '/');
    vim_feed_str(&mut app, "needle");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.editor.search_highlight.is_some(),
        "precondition: a vim search leaves the match-highlight term set"
    );
    assert!(app.editor.active_search_match.is_some());
    // Toggling vim mode off with Cmd+E is the only way to dismiss vim search
    // highlights, so it must clear both the highlight term and the active match.
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(!app.vim.enabled);
    assert!(
        app.editor.search_highlight.is_none(),
        "toggling vim off must clear the search-match highlight term so no matches stay painted"
    );
    assert!(
        app.editor.active_search_match.is_none(),
        "toggling vim off must clear the active-match marker too"
    );
}

#[test]
fn vim_toggle_off_restores_plain_typing() {
    let (mut app, _t) = vim_app("");
    // Cmd+E turns modal editing off.
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(!app.vim.enabled);
    vim_feed_str(&mut app, "hi");
    assert_eq!(app.editor.lines, vec!["hi"]);
}

#[test]
fn vim_toggle_on_with_cmd_e_enables_modal_editing() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("buf.txt");
    std::fs::write(&f, "abc").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus = Pane::Editor;
    // Cmd+E from a plain editor turns modal editing on.
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.vim.enabled);
    assert_eq!(app.vim.mode(), crate::vim::VimMode::Normal);
    // A plain letter is now a Normal-mode command, not inserted text.
    vim_feed(&mut app, 'x');
    assert_eq!(app.editor.lines, vec!["bc"]);
}

#[test]
fn vim_ex_goto_line_number() {
    let (mut app, _t) = vim_app("l1\nl2\nl3\nl4");
    vim_feed(&mut app, ':');
    vim_feed(&mut app, '3');
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 2);
}

#[test]
fn vim_cmd_save_shortcut_still_works_in_normal_mode() {
    // A Cmd-modified key must pass through to the editor's own handling
    // rather than being swallowed by Normal mode.
    let (mut app, _t) = vim_app("data");
    vim_feed(&mut app, 'x'); // edit so the buffer is dirty: "ata"
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["ata"]);
    assert!(
        !app.editor.dirty,
        "Cmd+S should have saved through Normal mode"
    );
}

#[test]
fn rename_symbol_key_is_plain_f2_only() {
    assert!(is_rename_symbol_key(key(KeyCode::F(2), KeyModifiers::NONE)));
    // Modified F2 is Change All Occurrences, not Rename Symbol.
    assert!(!is_rename_symbol_key(key(
        KeyCode::F(2),
        KeyModifiers::SUPER
    )));
    assert!(!is_rename_symbol_key(key(
        KeyCode::F(2),
        KeyModifiers::CONTROL
    )));
    assert!(!is_rename_symbol_key(key(
        KeyCode::F(3),
        KeyModifiers::NONE
    )));
}

#[test]
fn change_all_occurrences_key_accepts_cmd_ctrl_or_alt_f2() {
    // macOS Cmd, Linux Ctrl, and iTerm2's Cmd-folded-to-Meta (ALT) all fire.
    assert!(is_change_all_occurrences_key(key(
        KeyCode::F(2),
        KeyModifiers::SUPER
    )));
    assert!(is_change_all_occurrences_key(key(
        KeyCode::F(2),
        KeyModifiers::CONTROL
    )));
    assert!(is_change_all_occurrences_key(key(
        KeyCode::F(2),
        KeyModifiers::ALT
    )));
    // Plain F2 stays Rename Symbol.
    assert!(!is_change_all_occurrences_key(key(
        KeyCode::F(2),
        KeyModifiers::NONE
    )));
}

#[test]
fn go_to_definition_key_is_f12_without_shift() {
    assert!(is_go_to_definition_key(key(
        KeyCode::F(12),
        KeyModifiers::NONE
    )));
    // Alt+F12 still means Definition (iTerm2 can fold a held Cmd onto the
    // Meta/Alt bit, and Option+F12 is otherwise unused).
    assert!(is_go_to_definition_key(key(
        KeyCode::F(12),
        KeyModifiers::ALT
    )));
    // Shift+F12 is Declaration, Ctrl+F12 is Type Definition, Cmd+F12 (SUPER) is
    // Implementations; none of the three is Definition.
    assert!(!is_go_to_definition_key(key(
        KeyCode::F(12),
        KeyModifiers::SHIFT
    )));
    assert!(!is_go_to_definition_key(key(
        KeyCode::F(12),
        KeyModifiers::CONTROL
    )));
    assert!(!is_go_to_definition_key(key(
        KeyCode::F(12),
        KeyModifiers::SUPER
    )));
    assert!(!is_go_to_definition_key(key(
        KeyCode::F(11),
        KeyModifiers::NONE
    )));
}

#[test]
fn go_to_references_key_is_shift_f12() {
    // VS Code's real binding: Shift+F12 = Go to References.
    assert!(is_go_to_references_key(key(
        KeyCode::F(12),
        KeyModifiers::SHIFT
    )));
    // Plain F12 is Definition, not References.
    assert!(!is_go_to_references_key(key(
        KeyCode::F(12),
        KeyModifiers::NONE
    )));
    // Ctrl+Shift+F12 is Declaration, not References; the Ctrl bit must split
    // them when both Shift and Ctrl are reported.
    assert!(!is_go_to_references_key(key(
        KeyCode::F(12),
        KeyModifiers::SHIFT | KeyModifiers::CONTROL
    )));
    // Cmd+F12 is Implementations.
    assert!(!is_go_to_references_key(key(
        KeyCode::F(12),
        KeyModifiers::SHIFT | KeyModifiers::SUPER
    )));
    assert!(!is_go_to_references_key(key(
        KeyCode::F(2),
        KeyModifiers::SHIFT
    )));
}

#[test]
fn go_to_declaration_key_is_ctrl_shift_f12() {
    // Declaration moved off bare Shift+F12 (now References) to Ctrl+Shift+F12.
    assert!(is_go_to_declaration_key(key(
        KeyCode::F(12),
        KeyModifiers::SHIFT | KeyModifiers::CONTROL
    )));
    // Plain F12 is Definition.
    assert!(!is_go_to_declaration_key(key(
        KeyCode::F(12),
        KeyModifiers::NONE
    )));
    // Bare Shift+F12 is now References, not Declaration.
    assert!(!is_go_to_declaration_key(key(
        KeyCode::F(12),
        KeyModifiers::SHIFT
    )));
    // Ctrl+F12 (no Shift) is Type Definition, not Declaration.
    assert!(!is_go_to_declaration_key(key(
        KeyCode::F(12),
        KeyModifiers::CONTROL
    )));
    assert!(!is_go_to_declaration_key(key(
        KeyCode::F(2),
        KeyModifiers::SHIFT | KeyModifiers::CONTROL
    )));
}

#[test]
fn go_to_type_definition_key_is_ctrl_f12() {
    assert!(is_go_to_type_definition_key(key(
        KeyCode::F(12),
        KeyModifiers::CONTROL
    )));
    // Plain F12 (Definition) and Shift+F12 (References) are not Type Definition.
    assert!(!is_go_to_type_definition_key(key(
        KeyCode::F(12),
        KeyModifiers::NONE
    )));
    assert!(!is_go_to_type_definition_key(key(
        KeyCode::F(12),
        KeyModifiers::SHIFT
    )));
    // Ctrl+Shift+F12 is Declaration, not Type Definition.
    assert!(!is_go_to_type_definition_key(key(
        KeyCode::F(12),
        KeyModifiers::SHIFT | KeyModifiers::CONTROL
    )));
    assert!(!is_go_to_type_definition_key(key(
        KeyCode::F(11),
        KeyModifiers::CONTROL
    )));
}

#[test]
fn go_to_implementation_key_is_cmd_f12() {
    assert!(is_go_to_implementation_key(key(
        KeyCode::F(12),
        KeyModifiers::SUPER
    )));
    // Plain F12 (Definition), Shift+F12 (References) and Ctrl+F12 (Type
    // Definition) are not Implementations.
    assert!(!is_go_to_implementation_key(key(
        KeyCode::F(12),
        KeyModifiers::NONE
    )));
    assert!(!is_go_to_implementation_key(key(
        KeyCode::F(12),
        KeyModifiers::SHIFT
    )));
    assert!(!is_go_to_implementation_key(key(
        KeyCode::F(12),
        KeyModifiers::CONTROL
    )));
    assert!(!is_go_to_implementation_key(key(
        KeyCode::F(11),
        KeyModifiers::SUPER
    )));
}

#[test]
fn implementation_picker_lists_every_target_as_a_jump_item() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let a = tmp.path().join("foo.rs");
    let b = tmp.path().join("sub").join("bar.rs");
    app.open_location_picker(
        vec![(a.clone(), 4, 2), (b.clone(), 0, 0)],
        "implementations",
    );

    let menu = app
        .context_menu
        .as_ref()
        .expect("multiple implementations must open a picker, not silently pick the first");
    assert_eq!(
        menu.items.len(),
        2,
        "every implementor must get its own selectable row"
    );
    // Labels are workspace-relative path + 1-based line; actions jump straight
    // to the resolved location.
    assert_eq!(menu.items[0].0, "foo.rs:5");
    assert_eq!(
        menu.items[0].1,
        MenuAction::GoToLocation {
            path: a,
            line: 4,
            col: 2
        }
    );
    assert_eq!(
        menu.items[1].0,
        format!("sub{}bar.rs:1", std::path::MAIN_SEPARATOR)
    );
    assert_eq!(
        menu.items[1].1,
        MenuAction::GoToLocation {
            path: b,
            line: 0,
            col: 0
        }
    );
}

#[test]
fn shortcut_for_returns_expected_shortcuts_for_editor_symbol_actions() {
    // Every editor symbol-menu row must advertise a working accelerator.
    assert_eq!(
        shortcut_for(&MenuAction::GoToDefinitionAt { row: 0, col: 0 }),
        Some("F12")
    );
    assert_eq!(
        shortcut_for(&MenuAction::GoToReferencesAt { row: 0, col: 0 }),
        Some("⇧F12")
    );
    assert_eq!(
        shortcut_for(&MenuAction::GoToDeclarationAt { row: 0, col: 0 }),
        Some("⌃⇧F12")
    );
    assert_eq!(
        shortcut_for(&MenuAction::GoToTypeDefinitionAt { row: 0, col: 0 }),
        Some("⌃F12")
    );
    assert_eq!(
        shortcut_for(&MenuAction::GoToImplementationAt { row: 0, col: 0 }),
        Some("⌘F12")
    );
    assert_eq!(
        shortcut_for(&MenuAction::RenameSymbolAt { row: 0, col: 0 }),
        Some("F2")
    );
    assert_eq!(
        shortcut_for(&MenuAction::ChangeAllOccurrencesAt { row: 0, col: 0 }),
        Some("⌘F2")
    );
}

#[test]
fn switching_sidebar_view_closes_the_open_commit_dropdown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    app.commit_menu_open = true;
    // Leaving Source Control must close the dropdown so it can't paint
    // over the Run-Debug (or any other) sidebar view.
    app.set_sidebar_view(SidebarView::RunDebug);
    assert!(
        !app.commit_menu_open,
        "the commit dropdown must close when the sidebar view changes"
    );
}

// --- Split editor (side-by-side panes) -----------------------------------

/// Open a file into the focused editor group of a fresh App.
fn app_with_open_file(tmp: &std::path::Path, name: &str, body: &str) -> App {
    let f = tmp.join(name);
    std::fs::write(&f, body).unwrap();
    let mut app = App::new(tmp.to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app
}

#[test]
fn shift_tab_in_editor_dedents_the_current_python_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "snippet.py", "def f():\n        pass\n");
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 8;
    // Shift+Tab reaches the editor as BackTab.
    app.handle_editor_key(key(KeyCode::BackTab, KeyModifiers::SHIFT));
    assert_eq!(
        app.editor.lines[1], "    pass",
        "Shift+Tab must outdent the line one level"
    );
    assert_eq!(app.editor.cursor_col, 4);
}

#[test]
fn tab_in_editor_indents_a_multiline_python_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "block.py", "a = 1\nb = 2\n");
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 5;
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (0, 0),
        head: (1, 5),
    });
    app.handle_editor_key(key(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.editor.lines[0], "    a = 1");
    assert_eq!(app.editor.lines[1], "    b = 2");
}

#[test]
fn split_editor_duplicates_active_file_into_a_focused_right_group() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello\nworld\nthere");
    let path = app.editor.path.clone().unwrap();
    app.editor.cursor_row = 2;

    app.split_editor();

    assert!(
        app.editor_split.is_some(),
        "split must create a second group"
    );
    assert!(
        !app.split_focus_left,
        "the new (focused) group lands in the RIGHT column, mirroring VS Code"
    );
    // `editor` is always the focused group: it is the new right duplicate.
    assert_eq!(
        app.editor.path.as_deref(),
        Some(path.as_path()),
        "the duplicate shows the same file"
    );
    assert_eq!(
        app.editor.cursor_row, 2,
        "the duplicate opens at the source cursor position, not the top"
    );
    assert_eq!(
        app.editor.tab_count(),
        1,
        "the new group holds exactly the file, with no stray blank tab"
    );
    // The original group survives in the left column.
    assert_eq!(
        app.editor_split.as_ref().unwrap().path.as_deref(),
        Some(path.as_path())
    );
}

#[test]
fn split_editor_refuses_a_blank_unsaved_buffer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.split_editor();
    assert!(
        app.editor_split.is_none(),
        "a blank buffer has no path to mirror, so split is refused"
    );
}

#[test]
fn opening_a_file_in_the_split_replaces_the_duplicate_instead_of_stacking() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "first");
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "second").unwrap();

    app.split_editor(); // right group = preview duplicate of a.txt
    assert_eq!(app.editor.tab_count(), 1);

    // Opening another file in the focused (right) column reuses the
    // preview slot rather than leaving the duplicate pinned beside it.
    app.editor.open_preview(&b).unwrap();
    assert_eq!(
        app.editor.tab_count(),
        1,
        "the right column swaps to the new file in place; the duplicate must not linger as a second tab"
    );
    assert_eq!(app.editor.path.as_deref(), Some(b.as_path()));
    // The left column is untouched, giving a true side-by-side of two files.
    let left = app.editor_split.as_ref().unwrap();
    assert_eq!(left.tab_count(), 1);
    assert_eq!(
        left.path.as_deref(),
        Some(tmp.path().join("a.txt").as_path())
    );
}

#[test]
fn focus_editor_group_swaps_focus_but_keeps_physical_columns_fixed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "left-original");
    let original = app.editor.path.clone().unwrap();
    app.split_editor(); // focus now on the right duplicate
    assert!(!app.split_focus_left);

    // Move focus to the LEFT group (the original).
    app.focus_editor_group(true);
    assert!(
        app.split_focus_left,
        "focusing left makes the focused group the left column"
    );
    assert_eq!(
        app.editor.path.as_deref(),
        Some(original.as_path()),
        "after the swap `editor` is the original left group"
    );
    assert!(
        app.editor.focused && !app.editor_split.as_ref().unwrap().focused,
        "only the focused group's border lights up"
    );

    // Move focus back to the RIGHT group.
    app.focus_editor_group(false);
    assert!(!app.split_focus_left);
    assert_eq!(
        app.editor.path.as_deref(),
        Some(original.as_path()),
        "both columns hold the same duplicated file, so the path is unchanged; \
         the point is the swap is symmetric and never loses a group"
    );
}

#[test]
fn closing_last_tab_in_focused_group_collapses_the_split() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "body");
    app.split_editor();
    assert!(app.editor_split.is_some());

    // Cmd+W closes the focused group's only tab -> the group empties and
    // the split collapses back to the surviving column.
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();

    assert!(
        app.editor_split.is_none(),
        "emptying the focused group collapses the split"
    );
    assert!(
        app.split_focus_left,
        "the collapsed state resets to the single-column default"
    );
    assert_eq!(app.editor_splitter_x, None);
}

#[test]
fn editor_image_overlay_is_dual_slot_and_clear_ors_both_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let layout = |name: &str| EditorImageLayout {
        cell_x: 0,
        cell_y: 0,
        cell_w: 10,
        cell_h: 10,
        path: tmp.path().join(name),
    };
    // Both physical columns can hold an inline image at once.
    app.overlays.editor[0].set_test_state(Some("left-osc".into()), Some(layout("l.png")), true);
    app.overlays.editor[1].set_test_state(Some("right-osc".into()), Some(layout("r.png")), true);
    assert_eq!(
        app.editor_image_payload(0).map(|(o, _)| o),
        Some("left-osc")
    );
    assert_eq!(
        app.editor_image_payload(1).map(|(o, _)| o),
        Some("right-osc")
    );

    // Disabling the right slot arms a clear; `consume_editor_image_clear`
    // must report it even though the left slot is quiet.
    app.disable_editor_image(1);
    assert!(
        app.consume_editor_image_clear(),
        "a clear armed on either column must surface from the ORed consume"
    );
    assert!(
        !app.consume_editor_image_clear(),
        "the latch is one-shot and both slots were consumed"
    );
}

#[test]
fn editor_split_key_is_cmd_backslash_only() {
    assert!(is_editor_split_key(key(
        KeyCode::Char('\\'),
        KeyModifiers::SUPER
    )));
    // Bare backslash is a literal character, not a split.
    assert!(!is_editor_split_key(key(
        KeyCode::Char('\\'),
        KeyModifiers::NONE
    )));
    // Modified variants are reserved for other (future) chords.
    assert!(!is_editor_split_key(key(
        KeyCode::Char('\\'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
}

#[test]
fn focus_group_keys_are_cmd_opt_arrows_disjoint_from_word_motion() {
    assert!(is_focus_group_left_key(key(
        KeyCode::Left,
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
    assert!(is_focus_group_right_key(key(
        KeyCode::Right,
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
    // Plain Opt+Arrow is editor word-motion, NOT group focus.
    assert!(!is_focus_group_left_key(key(
        KeyCode::Left,
        KeyModifiers::ALT
    )));
    assert!(!is_focus_group_right_key(key(
        KeyCode::Right,
        KeyModifiers::ALT
    )));
}

#[test]
fn cmd_active_treats_super_as_command_everywhere() {
    // Super is the command modifier on every platform, Termux or not.
    assert!(cmd_active(KeyModifiers::SUPER, false));
    assert!(cmd_active(KeyModifiers::SUPER, true));
}

#[test]
fn cmd_active_treats_ctrl_as_command_only_on_termux() {
    // Termux (Android) has no Cmd key, so Ctrl stands in for it there
    // (VS Code's Linux keymap). Off Termux, Ctrl is NOT the command key.
    assert!(!cmd_active(KeyModifiers::CONTROL, false));
    assert!(cmd_active(KeyModifiers::CONTROL, true));
}

#[test]
fn cmd_active_rejects_bare_modifiers() {
    assert!(!cmd_active(KeyModifiers::NONE, true));
    assert!(!cmd_active(KeyModifiers::ALT, true));
    assert!(!cmd_active(KeyModifiers::SHIFT, true));
}

#[test]
fn editor_split_still_requires_super_off_termux() {
    // Regression guard: with no Termux env present (the test process),
    // Cmd+\ splits but Ctrl+\ does not — Ctrl+\ stays SIGQUIT for the shell.
    assert!(is_editor_split_key(key(
        KeyCode::Char('\\'),
        KeyModifiers::SUPER
    )));
    assert!(!is_editor_split_key(key(
        KeyCode::Char('\\'),
        KeyModifiers::CONTROL
    )));
}

#[test]
fn terminal_cycle_still_requires_super_off_termux() {
    assert!(is_terminal_cycle_key(key(
        KeyCode::Char(']'),
        KeyModifiers::SUPER
    )));
    assert!(!is_terminal_cycle_key(key(
        KeyCode::Char(']'),
        KeyModifiers::CONTROL
    )));
    assert!(is_terminal_cycle_back_key(key(
        KeyCode::Char('['),
        KeyModifiers::SUPER
    )));
}
