//! The explorer: the file tree, its marks, drag and drop, file operations and
//! explorer views.

use super::*;

#[test]
fn explorer_boots_with_gradient_focus_under_black_theme() {
    // Regression: the explorer auto-focuses at startup, but the Black-theme
    // gradient border used to arm only on the first focus change because
    // `sync_focus_flags` never ran during construction. `App::new` now syncs
    // once before returning, so the initial focus already carries the gradient.
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    assert_eq!(app.theme, crate::theme::Theme::BLACK);
    assert!(app.tree.focused, "explorer should boot focused");
    assert!(
        app.tree.focus_gradient,
        "Black-theme gradient border must arm at startup, not just after a focus change"
    );
}

/// #147 + #539: the PRIMARY instance's poll walks EVERY root's expanded rows,
/// so a multi-root workspace's secondary folders reach the noise classifier
/// matching neither of the primary's own roots. Measured whole, a secondary
/// folder at `<tmp>/build/second` had its `build` ancestor make every change
/// under it read as noise: the poll refreshed the tree but reported
/// `finder_relevant = false`, so the Cmd+P index stayed stale for that folder
/// for as long as it was open.
#[test]
fn poll_classifies_a_secondary_root_against_the_root_that_owns_it() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    let secondary = tmp.path().join("build/second");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&secondary).unwrap();
    let mut app = App::new(primary.clone()).unwrap();
    // The watcher is off: this is the POLL's question, and leaving events on
    // would let them answer it instead.
    app.fs_watch.disable();
    app.tree.add_root(secondary.clone());

    // Baseline every expanded dir, so the next poll sees ONLY what moves
    // after it. Without this the primary's own row is "changed" too and
    // would set `finder_relevant` on its own, hiding the secondary entirely.
    app.fs_watch.force_poll_due();
    let _ = app.fs_watch.poll(&mut app.tree, &[]);

    std::fs::write(secondary.join("a.rs"), b"fn main() {}").unwrap();
    app.fs_watch.force_poll_due();
    let poll = app.fs_watch.poll(&mut app.tree, &[]);

    assert!(
        poll.dirs_changed,
        "precondition: the write moved the secondary root's mtime"
    );
    assert!(
        poll.finder_relevant,
        "a secondary workspace folder is workspace content, whatever its own \
         path is spelled under"
    );
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
    assert_eq!(app.workspace_root(), mid);
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
    assert_eq!(app.workspace_root(), tmp.path());
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
    assert_eq!(app.workspace_root(), project);
    // The tree's root node must reflect the new path.
    assert_eq!(app.tree.nodes[0].path, project);
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

/// #1174: Cut then Paste with the selection still in the cut file's folder
/// leaves the file alone and keeps the cut, so pasting on the folder it was
/// meant for still moves it there.
#[test]
fn cut_then_paste_in_the_same_folder_leaves_the_file_alone() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "notes\n").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Explorer);
    app.focus_pane(Pane::Tree);
    let select = |app: &mut App, name: &str| {
        app.tree.selected = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.file_name().is_some_and(|f| f == name))
            .unwrap_or_else(|| panic!("{name} in the tree"));
    };
    select(&mut app, "notes.txt");
    let notes = app.tree.nodes[app.tree.selected].path.clone();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::CONTROL))
        .unwrap();
    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(notes.exists(), "the file keeps its name");
    assert!(!notes.with_file_name("notes copy.txt").exists());
    assert_eq!(app.status, "Already in this folder: nothing to move");
    assert!(app.tree_clipboard.is_some(), "the cut is kept");

    select(&mut app, "sub");
    let sub = app.tree.nodes[app.tree.selected].path.clone();
    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(sub.join("notes.txt").exists(), "{}", app.status);
    assert!(!notes.exists());
    assert!(app.tree_clipboard.is_none());
}

/// Negative (#1174): a cut pasted into another folder still moves the
/// file there, and a copy pasted into its own folder still makes
/// "<name> copy", as before.
#[test]
fn cut_to_another_folder_moves_and_copy_in_place_still_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "notes\n").unwrap();
    std::fs::write(tmp.path().join("todo.txt"), "todo\n").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Explorer);
    app.focus_pane(Pane::Tree);
    let select = |app: &mut App, name: &str| {
        app.tree.selected = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.file_name().is_some_and(|f| f == name))
            .unwrap_or_else(|| panic!("{name} in the tree"));
    };
    select(&mut app, "notes.txt");
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::CONTROL))
        .unwrap();
    select(&mut app, "sub");
    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(tmp.path().join("sub/notes.txt").exists(), "{}", app.status);
    assert!(!tmp.path().join("notes.txt").exists());
    assert!(app.tree_clipboard.is_none(), "a done move clears the cut");
    select(&mut app, "todo.txt");
    app.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .unwrap();
    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(tmp.path().join("todo copy.txt").exists(), "{}", app.status);
    assert!(tmp.path().join("todo.txt").exists());
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
    // must surface New File / New Folder, and the resolved
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
        "nested-folder right-click must offer New File; labels = {labels:?}",
    );
    assert!(
        labels.contains(&"New Folder"),
        "nested-folder right-click must offer New Folder; labels = {labels:?}",
    );
}

/// #1294: `Ctrl`+`Z` in the Explorer opens the zoxide jump, as Cmd+Z does,
/// so a terminal that never sends Super reaches it.
#[test]
fn ctrl_z_in_the_explorer_opens_the_zoxide_jump_like_cmd_z() {
    for mods in [KeyModifiers::CONTROL, KeyModifiers::SUPER] {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Tree);
        app.handle_key(key(KeyCode::Char('z'), mods)).unwrap();
        assert!(app.zoxide_jump.is_some(), "{mods:?}+Z opens it");
    }
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
fn tree_context_menu_on_workspace_root_offers_new_file_and_new_folder_only() {
    // Right-clicking the workspace root row must NOT offer Rename/Delete
    // (the root cannot be renamed or moved to trash from inside croft).
    let tmp = tempfile::tempdir().unwrap();
    let n = dir_node(tmp.path());
    let items = build_tree_context_menu_items(Some(&n), tmp.path(), &[], tmp.path(), None, None);
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(labels, ["New File", "New Folder"]);
}

/// Issue #217: the Explorer's file names were white-on-white under Croft
/// Light (a bare `Color::White` that bypassed the theme adapter). Render
/// the tree under the light theme and inspect the actual name cell: the
/// text must be dark, and under a dark theme it must stay white.
#[test]
fn explorer_names_are_legible_on_the_light_theme() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    for (theme, want_dark) in [
        (crate::theme::Theme::DARK_BLUE, false),
        (crate::theme::Theme::from_id("light"), true),
    ] {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.apply_theme(theme);
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|frame| app.render(frame)).unwrap();
        // Find the cell where "alpha.rs" is painted in the tree column.
        let buf = term.backend().buffer();
        let mut found = None;
        'rows: for y in 0..40u16 {
            for x in 0..40u16 {
                if buf[(x, y)].symbol() == "a"
                    && buf[(x + 1, y)].symbol() == "l"
                    && buf[(x + 2, y)].symbol() == "p"
                    && buf[(x + 3, y)].symbol() == "h"
                {
                    found = Some((x, y));
                    break 'rows;
                }
            }
        }
        let (x, y) = found.expect("alpha.rs must be visible in the Explorer");
        let fg = buf[(x, y)].fg;
        if want_dark {
            let ratatui::style::Color::Rgb(r, g, b) = fg else {
                panic!("light tree names must resolve to Rgb, got {fg:?}")
            };
            let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
            assert!(luma < 128.0, "tree name must be dark on white, got {fg:?}");
        } else {
            assert_eq!(
                fg,
                ratatui::style::Color::White,
                "dark themes keep the historical white tree names"
            );
        }
    }
}

/// #465: the Explorer menu offers the viewer for a single file it handles,
/// after Rename and before the compare entries, and for nothing else.
#[test]
fn the_explorer_menu_offers_a_viewer_for_a_single_matching_file_only() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = tmp.path().join("data.csv");
    let md = tmp.path().join("notes.md");
    std::fs::write(&csv, "a,b\n").unwrap();
    std::fs::write(&md, "# hi\n").unwrap();
    let viewer_for = |file: &Path| {
        (file.extension().and_then(|e| e.to_str()) == Some("csv"))
            .then(|| (String::from("Open in csvlens"), String::from("csvlens")))
    };
    let target = tmp.path().to_path_buf();

    let n = file_node(&csv);
    let items = build_tree_context_menu_items_with(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&csv),
        &target,
        None,
        None,
        viewer_for,
    );
    let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
    let at = labels
        .iter()
        .position(|l| *l == "Open in csvlens")
        .expect("a csv gets the viewer entry");
    assert_eq!(labels[at - 1], "Rename", "it follows Rename");
    assert!(
        matches!(&items[at].1, MenuAction::OpenInViewer(id, p) if id == "csvlens" && p == &csv)
    );

    let n = file_node(&md);
    let items = build_tree_context_menu_items_with(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&md),
        &target,
        None,
        None,
        viewer_for,
    );
    assert!(
        !items.iter().any(|(l, _)| l == "Open in csvlens"),
        "a kind no viewer handles gets no entry"
    );

    // A multi-selection is not one file, so no viewer entry either.
    let n = file_node(&csv);
    let both = vec![csv.clone(), md.clone()];
    let items = build_tree_context_menu_items_with(
        Some(&n),
        tmp.path(),
        &both,
        &target,
        None,
        None,
        viewer_for,
    );
    assert!(
        !items.iter().any(|(l, _)| l == "Open in csvlens"),
        "a multi-selection gets no viewer entry"
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
    // Delete now asks first: it opens a confirmation popup, nothing is trashed yet.
    app.handle_tree_key(key(KeyCode::Delete, KeyModifiers::NONE));
    assert!(
        app.input_prompt.is_some(),
        "Delete must open a confirm popup"
    );
    assert!(
        tmp.path().join("a.txt").exists(),
        "not trashed until confirmed"
    );
    // Enter confirms; now both selected entries move to Trash.
    app.submit_input_prompt();
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

/// On a terminal that never sends Super, Ctrl+Z with the Explorer focused
/// opens the jump through the real key path (#1294).
#[test]
fn ctrl_z_in_the_explorer_opens_the_zoxide_jump() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus = Pane::Tree;
    app.sidebar_view = SidebarView::Explorer;
    app.handle_key(key(KeyCode::Char('z'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.zoxide_jump.is_some(), "status: {}", app.status);
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

/// #926: on a host without zoxide the jump popup asks before installing
/// anything. `n` closes it with nothing run; `y` starts the install, once.
#[test]
fn the_jump_popup_installs_zoxide_only_when_the_user_says_y() {
    use crate::zoxide::{InstallState, install_requests};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let open_without_zoxide = |app: &mut App| {
        app.handle_explorer_shortcut(key(KeyCode::Char('z'), KeyModifiers::SUPER));
        // Whatever this machine has: the popup as a host without zoxide sees it.
        let jump = app.zoxide_jump.as_mut().expect("Cmd+Z opens the popup");
        jump.set_results(None);
        jump.install_state = InstallState::Idle;
    };
    let before = install_requests();

    open_without_zoxide(&mut app);
    app.handle_zoxide_jump_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(app.zoxide_jump.is_none(), "n declines and closes the popup");
    assert_eq!(install_requests(), before, "n installs nothing");

    open_without_zoxide(&mut app);
    app.handle_zoxide_jump_key(key(KeyCode::Char('y'), KeyModifiers::NONE));
    assert_eq!(install_requests(), before + 1, "y starts the install");
    let jump = app.zoxide_jump.as_ref().expect("the popup stays open");
    assert_eq!(jump.install_state, InstallState::Running);
    assert!(
        jump.query.is_empty(),
        "the answer is not typed into the query"
    );
    assert!(
        app.status.to_lowercase().contains("installing"),
        "status: {}",
        app.status
    );
    app.handle_zoxide_jump_key(key(KeyCode::Char('y'), KeyModifiers::NONE));
    assert_eq!(
        install_requests(),
        before + 1,
        "a second y while it runs starts nothing"
    );
}

/// #926: with zoxide installed but no hook in the rc, launching croft no
/// longer wires one, so the empty popup offers the hook alone: `y` adds it,
/// installs nothing, and a typed query is a query, not an answer.
#[test]
fn the_jump_popup_offers_the_hook_alone_when_zoxide_is_installed() {
    use crate::zoxide::{hook_requests, install_requests};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let open_with_unhooked_zoxide = |app: &mut App| {
        app.handle_explorer_shortcut(key(KeyCode::Char('z'), KeyModifiers::SUPER));
        let jump = app.zoxide_jump.as_mut().expect("Cmd+Z opens the popup");
        jump.set_results(Some(Vec::new()));
        jump.hook_offer = Some(String::from("~/.bashrc"));
    };
    let (hooks, installs) = (hook_requests(), install_requests());

    open_with_unhooked_zoxide(&mut app);
    assert!(app.zoxide_jump.as_ref().unwrap().hook_offer_shown());
    app.handle_zoxide_jump_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(app.zoxide_jump.is_none(), "n declines and closes the popup");
    assert_eq!(hook_requests(), hooks, "n adds nothing");

    open_with_unhooked_zoxide(&mut app);
    app.handle_zoxide_jump_key(key(KeyCode::Char('y'), KeyModifiers::NONE));
    assert_eq!(hook_requests(), hooks + 1, "y adds the hook");
    assert_eq!(install_requests(), installs, "zoxide is there: no install");
    let jump = app.zoxide_jump.as_ref().expect("the popup stays open");
    assert!(jump.hook_offer.is_none(), "offered once");
    assert!(
        jump.query.is_empty(),
        "the answer is not typed into the query"
    );
    app.close_zoxide_jump();

    open_with_unhooked_zoxide(&mut app);
    app.zoxide_jump.as_mut().unwrap().push_char('p');
    app.handle_zoxide_jump_key(key(KeyCode::Char('y'), KeyModifiers::NONE));
    assert_eq!(hook_requests(), hooks + 1, "a query's `y` is a letter");
    assert_eq!(app.zoxide_jump.as_ref().unwrap().query, "py");
}

/// #1345: the Explorer hides a `files.exclude` folder, and only that:
/// `search.exclude` leaves the fixtures listed, as in VS Code.
#[test]
fn the_explorer_hides_files_exclude_folders_only() {
    let (app, t) = app_with_vscode_excludes();
    let listed: Vec<_> = app.tree.nodes.iter().map(|n| n.path.clone()).collect();
    assert!(!listed.contains(&t.path().join("generated")), "{listed:?}");
    assert!(listed.contains(&t.path().join("tests")), "{listed:?}");
    assert!(listed.contains(&t.path().join("src")), "{listed:?}");
}

/// #360: the built-in secret rules sit in the trigger set by default,
/// leave it when the setting is switched off, and come back on; a reveal
/// window lasts ten seconds and asks for a redraw when it closes.
#[test]
fn secret_redaction_toggles_the_builtin_rules_and_reveal_expires() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.secret_redaction, "on by default");
    assert!(
        app.triggers.has_redactions(),
        "the built-in rules are loaded"
    );
    assert_eq!(
        crate::triggers::mask_text("AKIAIOSFODNN7EXAMPLE", &app.triggers, false),
        "\u{2022}".repeat(20)
    );
    app.toggle_secret_redaction();
    assert!(!app.secret_redaction);
    assert!(
        !app.triggers.has_redactions(),
        "off means no built-in rules"
    );
    app.toggle_secret_redaction();
    assert!(app.triggers.has_redactions());

    assert!(!app.redactions_revealed());
    app.reveal_redacted_secrets();
    assert!(app.redactions_revealed());
    assert!(
        !app.tick_redaction_reveal(),
        "still inside the window: no change"
    );
    app.redaction_reveal_until = Some(std::time::Instant::now());
    assert!(
        app.tick_redaction_reveal(),
        "the window closing is a redraw"
    );
    assert!(!app.redactions_revealed());
    assert!(app.status.contains("masked again"));

    // The chip reads what was PAINTED: a hidden panel paints nothing, so a
    // stale count from the last visible frame must not survive it.
    if let Some(t) = app.terminals.first_mut() {
        t.redacted_on_screen = 3;
    }
    assert_eq!(app.redacted_on_screen(), 3);
    app.show_terminal = false;
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(app.redacted_on_screen(), 0, "a hidden pane counts nothing");
    app.show_terminal = true;

    // The clipboard keeps the real value unless a rule says otherwise; the
    // scrollback dump always masks.
    assert_eq!(
        app.terminal_text_for_copy(String::from("k=AKIAIOSFODNN7EXAMPLE")),
        "k=AKIAIOSFODNN7EXAMPLE"
    );
    app.triggers = std::sync::Arc::new(crate::triggers::TriggerSet::from_json(
        r#"[{ "regex": "AKIA[0-9A-Z]{16}", "action": "redact", "copy": "masked" }]"#,
    ));
    assert_eq!(
        app.terminal_text_for_copy(String::from("k=AKIAIOSFODNN7EXAMPLE")),
        format!("k={}", "\u{2022}".repeat(20))
    );
}

#[test]
fn explorer_views_menu_omits_dependencies_in_a_folder_with_no_manifest() {
    // A plain folder carries no package manifest, so the DEPENDENCIES view is
    // not offered at all — mirroring how rust-analyzer's "Rust Dependencies"
    // view simply does not exist outside a Rust project. The other four views
    // are always available, each with a checkmark reflecting its visibility.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_explorer_views_menu();
    let menu = app.context_menu.as_ref().expect("menu must open");
    assert_eq!(
        menu.items.len(),
        ExplorerView::ALL.len() - 1,
        "the Dependencies row is dropped when no manifest is present"
    );
    assert!(
        !menu
            .items
            .iter()
            .any(|e| menu_label(e).contains("Dependencies")),
        "no dependency toggle in a manifest-less folder: {:?}",
        menu.items
    );
    for entry in &menu.items {
        let label = menu_label(entry);
        let checked = label.starts_with('\u{2713}');
        // Every listed row names an always-available view and its check glyph
        // matches that view's current visibility.
        let view = ExplorerView::ALL
            .iter()
            .find(|v| label.contains(v.label()))
            .unwrap_or_else(|| panic!("unrecognized menu row {label:?}"));
        assert_eq!(checked, app.explorer_views.is_visible(*view));
    }
}

#[test]
fn explorer_views_menu_labels_dependencies_for_the_detected_ecosystem() {
    // A Cargo workspace makes the DEPENDENCIES view appear, labeled for the
    // detected ecosystem ("Rust Dependencies"), so the menu is language-aware
    // rather than hardcoded.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_explorer_views_menu();
    let menu = app.context_menu.as_ref().expect("menu must open");
    assert_eq!(menu.items.len(), ExplorerView::ALL.len());
    assert!(
        menu.items
            .iter()
            .any(|e| menu_label(e).contains("Rust Dependencies")),
        "a Cargo root labels the view 'Rust Dependencies': {:?}",
        menu.items
    );
}

#[test]
fn toggling_an_explorer_view_flips_and_persists_its_visibility() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let before = app.explorer_views.is_visible(ExplorerView::Outline);
    app.toggle_explorer_view(ExplorerView::Outline);
    assert_eq!(
        app.explorer_views.is_visible(ExplorerView::Outline),
        !before,
        "toggling Outline must flip its visibility"
    );
}

/// #1608: clicking the `src` crumb lists the folder, folders first, and
/// Enter on `f3.rs` opens it.
#[test]
fn clicking_a_folder_crumb_lists_it_and_enter_opens_a_file() {
    let (mut app, tmp) = app_on_src_f2("// f2.rs\n");
    click_crumb(&mut app, 0);
    let menu = app.context_menu.as_ref().expect("the src picker");
    assert_eq!(
        menu_labels(&menu.items),
        vec!["sub/", "f1.rs", "f2.rs", "f3.rs"]
    );
    assert_eq!(menu.selected, 0);
    for _ in 0..3 {
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
    }
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.context_menu.is_none());
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("src/f3.rs").as_path())
    );
}

#[test]
fn opening_a_workspace_file_installs_its_exact_folder_set() {
    // #163: an explicitly opened .code-workspace defines the arrangement
    // exactly — the automatic per-primary store is not merged in.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("alpha");
    let b = tmp.path().join("beta");
    let c = tmp.path().join("gamma");
    for d in [&a, &b, &c] {
        std::fs::create_dir(d).unwrap();
    }
    let file = tmp.path().join("proj.code-workspace");
    crate::workspace::write_workspace_file(&file, &[b.clone(), c.clone()]).unwrap();

    let mut app = App::new(a.clone()).unwrap();
    // Poison the auto-store for primary B with a DIFFERENT folder: the
    // file must win, so alpha never joins.
    let b_canon = b.canonicalize().unwrap();
    crate::workspace::save_folders_for_root(
        &app.workspace_folders_path.clone(),
        &b_canon.display().to_string(),
        vec![a.clone()],
    )
    .unwrap();

    app.open_workspace_file(&file);

    let roots: Vec<PathBuf> = app.roots.iter().map(Path::to_path_buf).collect();
    assert_eq!(
        app.workspace_root(),
        b_canon,
        "first folder becomes primary"
    );
    assert!(
        roots.contains(&c.canonicalize().unwrap()),
        "the file's second folder joins: {roots:?}"
    );
    assert!(
        !roots.contains(&a.canonicalize().unwrap()),
        "the auto-store's folder does NOT merge in: {roots:?}"
    );

    // Save Workspace As round-trips the live set through the prompt path.
    let dest = tmp.path().join("saved.code-workspace");
    use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
    app.open_input_prompt(
        InputPrompt::new(
            InputPurpose::SaveWorkspaceAs,
            String::from("Save Workspace As"),
            "",
        )
        .with_value(dest.display().to_string()),
    );
    app.submit_input_prompt();
    let saved = crate::workspace::parse_workspace_file(&dest).unwrap();
    assert_eq!(saved, roots, "the saved file names the live folder set");

    // A malformed file refuses with the error in the status.
    let bad = tmp.path().join("bad.code-workspace");
    std::fs::write(&bad, "{ nope").unwrap();
    let before: Vec<PathBuf> = app.roots.iter().map(Path::to_path_buf).collect();
    app.open_workspace_file(&bad);
    assert!(app.status.starts_with("Workspace file:"), "{}", app.status);
    let after: Vec<PathBuf> = app.roots.iter().map(Path::to_path_buf).collect();
    assert_eq!(before, after, "a bad file changes nothing");
}

/// #1602: `~/` in a typed path is the home directory, and a relative path
/// is under the workspace.
#[test]
fn a_typed_path_expands_the_home_and_sits_under_the_workspace() {
    let home = std::path::Path::new("/home/h");
    let root = std::path::Path::new("/work/ws");
    assert_eq!(
        resolve_typed_path("~/team.code-workspace", Some(home), root),
        Some(home.join("team.code-workspace"))
    );
    assert_eq!(
        resolve_typed_path(" sub/x.code-workspace ", Some(home), root),
        Some(root.join("sub/x.code-workspace"))
    );
    assert_eq!(
        resolve_typed_path("/abs/x.code-workspace", Some(home), root),
        Some(std::path::Path::new("/abs/x.code-workspace").to_path_buf())
    );
    assert_eq!(
        resolve_typed_path("~user/x", Some(home), root),
        Some(root.join("~user/x")),
        "only `~/` is the home directory"
    );
    assert_eq!(
        resolve_typed_path("~/team.code-workspace", None, root),
        None,
        "with no home, `~/` is refused, not put under the workspace"
    );
}

/// Removes a path made relative to the process directory, if a run made it:
/// a path that was already there when the guard was made is never touched.
struct RemoveOnDrop(Option<PathBuf>);

impl RemoveOnDrop {
    fn new(path: PathBuf) -> Self {
        Self((!path.exists()).then_some(path))
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// #1602: Save Workspace As with a relative path writes under the workspace
/// root, not the directory croft was started from, and the status line
/// names the file as written.
#[test]
fn save_workspace_as_writes_a_relative_path_under_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let rel = format!("croft-1602-{}", std::process::id());
    let _cleanup = RemoveOnDrop::new(std::env::current_dir().unwrap().join(&rel));
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::SaveWorkspaceAs);
    app.input_prompt.as_mut().unwrap().value = format!("{rel}/team.code-workspace");
    app.submit_input_prompt();
    let dest = tmp.path().join(&rel).join("team.code-workspace");
    assert!(dest.is_file(), "{}", app.status);
    assert!(
        !std::env::current_dir().unwrap().join(&rel).exists(),
        "nothing under the launch directory"
    );
    assert!(
        app.status.contains(&dest.display().to_string()),
        "{}",
        app.status
    );
    assert!(app.status.contains("created folder"), "{}", app.status);
}

/// #1602: a bare name gets the `.code-workspace` extension.
#[test]
fn save_workspace_as_adds_the_extension_to_a_bare_name() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::SaveWorkspaceAs);
    app.input_prompt.as_mut().unwrap().value = tmp.path().join("team").display().to_string();
    app.submit_input_prompt();
    assert!(
        tmp.path().join("team.code-workspace").is_file(),
        "{}",
        app.status
    );
    assert!(!tmp.path().join("team").exists());
}

/// Negative (#1602): the suggested absolute path is written where it says,
/// with no "created folder" note for a folder that exists.
#[test]
fn save_workspace_as_keeps_the_suggested_path() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::SaveWorkspaceAs);
    let suggested = PathBuf::from(&app.input_prompt.as_ref().unwrap().value);
    app.submit_input_prompt();
    assert!(suggested.is_file(), "{}", app.status);
    assert!(!app.status.contains("created folder"), "{}", app.status);
}

/// #1012: Save Workspace As suggests the team's committed file. Listing the
/// same folders, it is left alone; with another folder, saving asks first,
/// and a yes changes only `folders`, keeping names, configs and comments.
#[test]
fn save_workspace_as_asks_before_changing_a_team_file_and_keeps_the_rest_of_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for d in ["wsx", "wsx_lib", "extra"] {
        std::fs::create_dir(root.join(d)).unwrap();
    }
    let file = root.join("wsx/wsx.code-workspace");
    let team = "{\n  \"folders\": [ { \"path\": \".\" }, { \"path\": \"../wsx_lib\", \"name\": \"shared lib\" } ],\n  // F5 runs the app\n  \"launch\": { \"configurations\": [ { \"name\": \"Run app\" } ] }\n}\n";
    std::fs::write(&file, team).unwrap();
    let mut app = App::new(root.join("wsx")).unwrap();
    app.open_workspace_file(&file);
    let save = |app: &mut App| {
        app.run_command(crate::widgets::command_palette::Command::SaveWorkspaceAs);
        assert_eq!(
            app.input_prompt.as_ref().unwrap().value,
            file.display().to_string()
        );
        app.submit_input_prompt();
    };
    save(&mut app);
    assert!(app.input_prompt.is_none(), "nothing to ask: {}", app.status);
    assert!(app.status.contains("already up to date"), "{}", app.status);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), team);

    app.add_workspace_folder(root.join("extra"));
    save(&mut app);
    assert!(app.input_prompt.as_ref().is_some_and(|p| p.confirm));
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.input_prompt.is_none());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        team,
        "Esc keeps it"
    );

    save(&mut app);
    app.submit_input_prompt();
    let out = std::fs::read_to_string(&file).unwrap();
    assert!(
        app.status.starts_with("Saved workspace (3 folders)"),
        "{}",
        app.status
    );
    assert_eq!(
        out,
        team.replace(
            "\"shared lib\" }",
            "\"shared lib\" }, {\"path\":\"../extra\"}"
        )
    );
}

#[test]
fn workspace_folder_set_persists_across_sessions_and_reroots() {
    // #153: the folder set lived only in memory — reopening the same
    // workspace came back single-folder.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    let c = tmp.path().join("c");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::create_dir(&c).unwrap();
    let store = tmp.path().join("folders.json");

    let mut app = App::new(a.clone()).unwrap();
    app.workspace_folders_path = store.clone();
    app.add_workspace_folder(b.clone());
    app.add_workspace_folder(c.clone());

    // A fresh session on the same primary restores the arrangement.
    let mut app2 = App::new(a.clone()).unwrap();
    app2.workspace_folders_path = store.clone();
    app2.restore_workspace_folders();
    let b_canon = b.canonicalize().unwrap();
    let c_canon = c.canonicalize().unwrap();
    let roots: Vec<PathBuf> = app2.roots.iter().map(Path::to_path_buf).collect();
    assert!(
        roots.contains(&b_canon) && roots.contains(&c_canon),
        "both folders come back: {roots:?}"
    );

    // A vanished folder is dropped from the record, not restored.
    std::fs::remove_dir_all(&c).unwrap();
    let mut app3 = App::new(a.clone()).unwrap();
    app3.workspace_folders_path = store.clone();
    app3.restore_workspace_folders();
    let roots: Vec<PathBuf> = app3.roots.iter().map(Path::to_path_buf).collect();
    assert!(roots.contains(&b_canon) && !roots.contains(&c_canon));
    let map = crate::workspace::load_folders(&store);
    assert_eq!(
        map.get(&a.display().to_string()),
        Some(&vec![b_canon.clone()]),
        "the record rewrites to what survived"
    );

    // Removing the last secondary folder prunes the key.
    app3.remove_workspace_folder(b_canon.clone());
    assert!(
        !crate::workspace::load_folders(&store).contains_key(&a.display().to_string()),
        "a single-folder workspace never lingers in the store"
    );

    // Re-rooting into a workspace with a saved set restores it.
    crate::workspace::save_folders_for_root(
        &store,
        &b_canon.display().to_string(),
        vec![a.clone()],
    )
    .unwrap();
    app3.change_workspace_root(b_canon.clone());
    let roots: Vec<PathBuf> = app3.roots.iter().map(Path::to_path_buf).collect();
    assert!(
        roots.contains(&a.canonicalize().unwrap()),
        "the new primary's saved folders come back with the re-root: {roots:?}"
    );
}

#[test]
fn focusing_a_secondary_roots_file_rebinds_the_test_runner_and_tasks() {
    // #151: the runner gate and the worker resolved against the primary
    // only — from a secondary folder's file, Run All refused with "no
    // test runner" even when that folder is a cargo project.
    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("plain");
    let proj = tmp.path().join("proj");
    std::fs::create_dir(&plain).unwrap();
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname = \"proj\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/main.rs"),
        "fn main() {}\n#[test]\nfn proj_truth() {}\n",
    )
    .unwrap();
    std::fs::write(proj.join("Makefile"), "build:\n\techo hi\n").unwrap();

    let mut app = App::new(plain.clone()).unwrap();
    app.add_workspace_folder(proj.clone());
    let proj_canon = proj.canonicalize().unwrap();

    // Primary focused: no runner, the gate refuses.
    app.status.clear();
    app.run_all_tests();
    assert_eq!(app.status, crate::testing::NO_RUNNER_STATUS);

    // Focus the cargo project's file: the follow tick rebinds the
    // Testing stack and the gate passes.
    app.editor
        .open_pinned(&proj_canon.join("src/main.rs"))
        .unwrap();
    let _ = app.drain_git_responses();
    assert_eq!(
        app.active_test_root, proj_canon,
        "the Testing stack follows"
    );
    app.status.clear();
    app.run_all_tests();
    assert_ne!(
        app.status,
        crate::testing::NO_RUNNER_STATUS,
        "the secondary folder's cargo runner is found"
    );

    // The open-Testing-view path discovers against the active folder:
    // this is the exact live sequence, and the discovery gate had its own
    // primary-root check the entry-point gates didn't cover.
    app.open_testing_view();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let _ = app.test_worker.drain(&mut app.testing);
        if app
            .testing
            .cases_for_test()
            .iter()
            .any(|(name, _)| name.contains("proj_truth"))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "discovery never returned the secondary project's tests; cases: {:?}",
            app.testing.cases_for_test()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Drain the discovery's Finished before running: breaking out on the
    // first case leaves the panel busy, and run_all_tests refuses while a
    // discovery is in flight.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while app.testing.is_busy() {
        let _ = app.test_worker.drain(&mut app.testing);
        assert!(
            std::time::Instant::now() < deadline,
            "discovery never reported Finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // And a full RUN executes in that folder: the case must go green,
    // which only happens if cargo built and ran the secondary project.
    app.run_all_tests();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let _ = app.test_worker.drain(&mut app.testing);
        if app.testing.cases_for_test().iter().any(|(name, status)| {
            name.contains("proj_truth") && *status == crate::testing::model::TestStatus::Passed
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the secondary project's test never passed; cases: {:?}",
            app.testing.cases_for_test()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Tasks discover against the active folder too.
    let tasks = crate::tasks::discover_tasks(&app.active_workspace_root());
    assert!(
        tasks.iter().any(|t| t.command.contains("make")),
        "the secondary folder's Makefile tasks are offered: {:?}",
        tasks.iter().map(|t| t.command.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn focusing_a_secondary_roots_file_flips_source_control_to_its_repo() {
    // #149: git state was singular (one worker at the primary), so a
    // secondary root's repository was invisible — no branch, no panel
    // entries, no scm_root for its operations.
    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("plain");
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&plain).unwrap();
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-q", "-b", "trunk"],
        vec!["config", "user.email", "a@b"],
        vec!["config", "user.name", "a"],
    ] {
        let ok = std::process::Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(&args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git setup step failed: {args:?}");
    }
    std::fs::write(repo.join("seed.txt"), "one\n").unwrap();
    for args in [vec!["add", "."], vec!["commit", "-qm", "init"]] {
        let ok = std::process::Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(&args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git setup step failed: {args:?}");
    }
    std::fs::write(repo.join("seed.txt"), "one\ntwo\n").unwrap();

    // Primary is NOT a repo; the repo joins as a secondary folder.
    let mut app = App::new(plain.clone()).unwrap();
    app.add_workspace_folder(repo.clone());
    let repo_canon = repo.canonicalize().unwrap();

    // Focus the secondary root's file: the SCM surfaces must follow.
    app.editor
        .open_pinned(&repo_canon.join("seed.txt"))
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut flipped = false;
    while std::time::Instant::now() < deadline {
        let _ = app.drain_git_responses();
        if app.source_control.status.branch.as_deref() == Some("trunk")
            && app
                .source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt")
        {
            flipped = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        flipped,
        "the panel must show the secondary root's repo (branch + changes); saw {:?} / {:?}",
        app.source_control.status.branch,
        app.source_control
            .entries
            .iter()
            .map(|e| e.path.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        app.scm_root().canonicalize().unwrap(),
        repo_canon,
        "panel operations address the active repo's toplevel"
    );

    // The mutation path must hit the SAME repository the panel shows
    // (#150 review: staging addressed the active repo while commit still
    // ran against the primary — stage in B, commit in A).
    app.stage_all_source_control();
    app.source_control.message = String::from("from the panel");
    app.commit_staged_source_control();
    wait_for_git_net(&mut app);
    let log = std::process::Command::new("git")
        .args(["-C"])
        .arg(&repo)
        .args(["log", "--oneline"])
        .output()
        .unwrap();
    assert!(log.status.success());
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("from the panel"),
        "the commit lands in the ACTIVE repo, the one the panel shows"
    );
}

#[test]
fn add_and_remove_workspace_folder_fan_out_across_tree_index_and_search() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("alpha");
    let b = tmp.path().join("beta");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::write(a.join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(b.join("util.rs"), "pub fn u() {}\n").unwrap();
    let mut app = App::new(a.clone()).unwrap();

    app.add_workspace_folder(b.clone());

    let b_canon = b.canonicalize().unwrap();
    assert!(app.roots.is_multi(), "the model grew");
    assert!(
        app.tree.root_paths().any(|r| r == b_canon),
        "the Explorer gained the section"
    );
    assert_eq!(app.search.roots.len(), 2, "the search panel spans both");
    assert_eq!(
        app.fs_watch_extra.len(),
        1,
        "the added root gets its own watcher share"
    );
    // The Cmd+P index rebuild runs on a walker thread: poll it in, then
    // both roots' files must appear under root-name prefixes.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.try_install_pending_init();
        if let Some(idx) = app.file_finder_index.as_ref() {
            let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
            if rels.contains(&"alpha/main.rs") && rels.contains(&"beta/util.rs") {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the multi-root Cmd+P index never landed; entries: {:?}",
            app.file_finder_index
                .as_ref()
                .map(|i| i.iter().map(|e| e.rel.clone()).collect::<Vec<_>>())
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // Adding it again is a no-op with an explanation.
    app.add_workspace_folder(b.clone());
    assert_eq!(app.status, "Folder is already in the workspace");

    // The primary is not removable; the secondary is, and everything
    // collapses back to single-root.
    let a_canon = a.canonicalize().unwrap();
    app.remove_workspace_folder(a_canon);
    assert!(app.roots.is_multi(), "removing the primary is refused");
    app.remove_workspace_folder(b_canon.clone());
    assert!(!app.roots.is_multi());
    assert!(
        !app.tree.root_paths().any(|r| r == b_canon),
        "the section left the Explorer"
    );
    assert_eq!(app.search.roots.len(), 1);
    assert!(app.fs_watch_extra.is_empty());
}

#[test]
fn delete_filters_workspace_roots_out_of_a_mixed_selection() {
    // #146 review: the context menu forwards a mixed multi-selection
    // wholesale into MenuAction::Delete, and Select All marks the root
    // rows too — so the batch path must refuse root rows at the
    // delete_paths choke point, not rely on the per-node guard.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::write(a.join("f.txt"), "x\n").unwrap();
    let mut app = App::new(a.clone()).unwrap();
    app.tree.add_root(b.clone());

    // Mixed batch: a real file plus BOTH root rows (what Select All +
    // right-click Delete assembles).
    app.delete_paths(vec![a.join("f.txt"), a.clone(), b.clone()]);
    let prompt = app.input_prompt.as_ref().expect("the confirm opens");
    match &prompt.purpose {
        crate::widgets::input_prompt::InputPurpose::TreeDelete { paths } => {
            assert_eq!(
                paths,
                &vec![a.join("f.txt")],
                "both workspace roots are filtered; only the file survives to the confirm"
            );
        }
        other => panic!("wrong prompt purpose: {other:?}"),
    }
    app.input_prompt = None;

    // A batch of ONLY roots opens no prompt at all.
    app.delete_paths(vec![a.clone(), b.clone()]);
    assert!(
        app.input_prompt.is_none(),
        "a roots-only batch is refused outright"
    );
    assert_eq!(app.status, "Workspace roots cannot be deleted");
}

#[test]
fn answering_no_to_the_trash_confirm_keeps_the_file() {
    // #855: the confirm was a text field prefilled with `trash`. Clearing it
    // and typing `no` read as an answer, yet Enter trashed the file anyway.
    // It is a yes/no question now: typing and pasting go nowhere, and N
    // keeps the file.
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("scratch.py");
    std::fs::write(&file, "x\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Tree);

    app.delete_paths(vec![file.clone()]);
    for _ in 0..5 {
        app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
    }
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app.handle_paste("no");
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::CONTROL))
        .unwrap();
    let prompt = app
        .input_prompt
        .as_ref()
        .expect("keys that do not answer leave the question up");
    assert_eq!(prompt.value, "", "nothing typed or pasted lands in it");

    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.input_prompt.is_none(), "N answers no");
    assert!(file.exists(), "the file stays where it was");
}

#[test]
fn a_markdown_file_opened_from_the_explorer_scrolls_its_preview() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (0..200).map(|i| format!("line {i}\n")).collect();
    std::fs::write(tmp.path().join("notes.md"), body).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    open_from_explorer(&mut app, "notes.md");
    assert!(matches!(app.focus, Pane::Editor));
    app.toggle_markdown_preview();
    assert!(app.editor.markdown_preview.is_some());

    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    let scrolled = app
        .editor
        .markdown_preview
        .as_ref()
        .map(|p| p.scroll)
        .unwrap();
    assert!(scrolled > 0, "Down must scroll the rendered Markdown");
}

#[test]
fn a_text_file_opened_from_the_explorer_takes_the_arrow_keys() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "one\ntwo\nthree\n").unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    open_from_explorer(&mut app, "main.rs");
    let before = app.tree.selected;
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 1, "Down moves the caret in the file");
    assert_eq!(
        app.tree.selected, before,
        "the Explorer selection must not move once the file is open"
    );
}

#[test]
fn right_arrow_in_the_explorer_previews_a_file_without_stealing_focus() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("peek.txt"), "hello\n").unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.file_name().is_some_and(|f| f == "peek.txt"))
        .expect("peek.txt in the tree");
    app.tree.selected = idx;
    app.focus_pane(Pane::Tree);
    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();

    assert_eq!(app.editor.lines[0], "hello");
    assert!(
        matches!(app.focus, Pane::Tree),
        "Right is an Explorer navigation gesture: it previews but keeps the keyboard in the tree"
    );
}

/// The invariant behind every test above, applied to one file of each kind
/// croft previews: opening a file must leave the keyboard where that file's
/// navigation keys are handled. Add a new preview type, add a row here.
#[test]
fn opening_any_file_type_from_the_explorer_lands_the_keyboard_in_the_editor() {
    require_pdf_backend();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("plain.txt"), "alpha\nbeta\n").unwrap();
    std::fs::write(tmp.path().join("code.rs"), "fn main() {}\n").unwrap();
    std::fs::write(tmp.path().join("notes.md"), "# Title\n\nbody\n").unwrap();
    std::fs::write(tmp.path().join("data.csv"), csv_body(5)).unwrap();
    std::fs::write(tmp.path().join("data.tsv"), "a\tb\n1\t2\n").unwrap();
    std::fs::write(tmp.path().join("conf.json"), "{\"a\": 1}\n").unwrap();
    write_test_png(&tmp.path().join("shot.png"));
    write_three_page_pdf(&tmp.path().join("doc.pdf"));

    for name in [
        "plain.txt",
        "code.rs",
        "notes.md",
        "data.csv",
        "data.tsv",
        "conf.json",
        "shot.png",
        "doc.pdf",
    ] {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        open_from_explorer(&mut app, name);
        assert!(
            matches!(app.focus, Pane::Editor),
            "{name}: opening it must hand the keyboard to the editor, or its navigation keys are dead"
        );
    }
}

#[test]
fn clicking_any_file_type_in_the_explorer_lands_the_keyboard_in_the_editor() {
    require_pdf_backend();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("plain.txt"), "alpha\nbeta\n").unwrap();
    std::fs::write(tmp.path().join("data.csv"), csv_body(5)).unwrap();
    std::fs::write(tmp.path().join("notes.md"), "# Title\n").unwrap();
    write_test_png(&tmp.path().join("shot.png"));
    write_three_page_pdf(&tmp.path().join("doc.pdf"));

    for name in ["plain.txt", "data.csv", "notes.md", "shot.png", "doc.pdf"] {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        click_open_in_explorer(&mut app, name);
        assert!(
            matches!(app.focus, Pane::Editor),
            "{name}: one click must hand the keyboard to the editor"
        );
    }
}

/// Selecting a folder row must not move focus: the click is a tree gesture and
/// the user is still browsing.
#[test]
fn clicking_a_folder_in_the_explorer_keeps_the_keyboard_in_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/lib.rs"), "fn main() {}\n").unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    click_open_in_explorer(&mut app, "src");
    assert!(
        matches!(app.focus, Pane::Tree),
        "expanding a folder is a tree gesture: the keyboard stays in the Explorer"
    );
}

/// Reveal-in-explorer targets the sidebar, so it must work even when
/// auto-hide has collapsed it — otherwise the command silently does nothing
/// visible.
#[test]
fn reveal_in_explorer_shows_a_collapsed_sidebar() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "fn main() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;
    app.focus_pane(Pane::Editor);
    // #302: the collapse arrives on the dwell tick rather than the focus move.
    // What this test protects — that a sidebar-targeting command reveals an
    // ALREADY-COLLAPSED sidebar — is unchanged; it just has to reach that
    // collapsed state the way the app now does.
    app.tick_sidebar_auto_hide_at(
        std::time::Instant::now() + crate::app::sidebar_dwell::AUTO_HIDE_DWELL,
    );
    assert!(!app.show_tree, "collapsed first");

    app.reveal_in_explorer(f);
    assert!(
        app.show_tree,
        "a sidebar-targeting command reveals the sidebar"
    );
}

/// #345: the Explorer's unreviewed dots track the ledger, on every path
/// that changes it.
///
/// The predicate and the painting are tested in `file_tree`; this is the
/// wiring between them, which is where a decoration feature actually breaks.
/// A dot that lags the ledger is worse than no dot at all, because "no dot"
/// is precisely what a reviewed file looks like — so a stale one tells the
/// user they have reviewed something they have not.
///
/// **The drain is driven for real, not stood in for.** An earlier version of
/// this test called `attribute_writes_to_working_agents` and then
/// `sync_agent_lane_decorations` by hand, which tests the helper and not the
/// wiring: deleting the drain's own sync left every assertion passing. Since
/// the drain is the path that fires on every agent write, that was the one
/// call site the feature cannot afford to have untested.
#[test]
fn the_explorer_dots_follow_the_agent_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let touched = tmp.path().join("touched.rs");
    std::fs::write(&touched, "fn a() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(
        app.tree.agent_touched.is_empty(),
        "staging: nothing decorated before an agent writes"
    );

    app.terminals[0].set_agent(Some(crate::agents::AgentLane {
        name: String::from("claude"),
        status: crate::agents::AgentStatus::Working,
    }));

    // Let the watcher establish itself first, as the other fs-watch tests
    // do: a write racing the initial registration is simply not delivered,
    // which would make this test flaky rather than wrong.
    for _ in 0..20 {
        let _ = app.drain_fs_events();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Through the real drain: write the file, let the watcher deliver it,
    // and call the function the event loop calls. No explicit sync here —
    // that is the property under test.
    std::fs::write(&touched, "fn a() { todo!() }\n").unwrap();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the watcher event to reach the tree",
        || {
            app.drain_fs_events();
            app.tree.agent_touched.contains(&touched)
        },
    );
    assert!(
        app.tree.agent_touched.contains(&touched),
        "the drain must light the row it attributed, without a nudge \
         (ledger has {} unreviewed)",
        app.agent_ledger.unreviewed_files()
    );
    assert!(app.tree.is_agent_touched(&touched));
    // The chip counts the set the tree draws. Asserted HERE, where both
    // sides are 1: after the lane is cleared below they are both 0, and an
    // equality that only ever compares 0 to 0 cannot fail.
    assert_eq!(
        app.agent_ledger.unreviewed_files(),
        app.tree.agent_touched.len(),
        "the badge and the dots must be the same number"
    );

    // Reviewing goes through the public entry point, which syncs itself.
    assert!(app.mark_agent_file_reviewed("claude", &touched));
    assert!(
        !app.tree.agent_touched.contains(&touched),
        "marking reviewed must clear the dot without a further nudge"
    );

    // Mark-the-whole-lane reviewed is the other syncing entry point.
    let mut changed = std::collections::BTreeSet::new();
    changed.insert(touched.clone());
    std::fs::write(&touched, "fn a() { 1 }\n").unwrap();
    assert!(app.attribute_writes_to_working_agents(&changed));
    app.sync_agent_lane_decorations();
    assert!(
        app.tree.agent_touched.contains(&touched),
        "staging: lit again"
    );
    app.mark_agent_lane_reviewed("claude");
    assert!(
        app.tree.agent_touched.is_empty(),
        "marking the lane reviewed must clear every dot"
    );
}

/// #345: removing a workspace folder takes its review queue with it.
///
/// The rows leave the tree on their own, so no stale dot renders — but the
/// status chip counts the same set, and a count that outlives the folder it
/// describes sends the user hunting for rows that are not there.
#[test]
fn removing_a_folder_drops_its_rows_from_the_review_queue() {
    let primary = tempfile::tempdir().unwrap();
    let secondary = tempfile::tempdir().unwrap();
    let in_second = secondary.path().join("s.rs");
    std::fs::write(&in_second, "fn s() {}\n").unwrap();
    let mut app = App::new(primary.path().to_path_buf()).unwrap();
    app.roots.add(secondary.path().to_path_buf());

    app.terminals[0].set_agent(Some(crate::agents::AgentLane {
        name: String::from("claude"),
        status: crate::agents::AgentStatus::Working,
    }));
    let mut changed = std::collections::BTreeSet::new();
    changed.insert(in_second.clone());
    assert!(app.attribute_writes_to_working_agents(&changed));
    app.sync_agent_lane_decorations();
    assert_eq!(
        app.tree.agent_touched.len(),
        1,
        "staging: the secondary root's file is queued"
    );

    app.remove_workspace_folder(secondary.path().to_path_buf());
    assert!(
        app.tree.agent_touched.is_empty(),
        "the removed folder's rows outlived it: {:?}",
        app.tree.agent_touched
    );
    assert_eq!(app.agent_ledger.unreviewed_files(), 0);
}

#[test]
fn cmd_k_e_still_reveals_in_the_explorer() {
    // The design proposal had picked Cmd+K E for restore-all. It is croft's
    // reveal-in-Explorer, and an `e` arm added below the existing one would
    // never have been reached - a chord that silently does nothing. This
    // pins the collision so the mistake cannot be made a second time.
    let (_tmp, mut app, _term) = app_with_terminal_panes(2);
    app.toggle_terminal_collapse(1);
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('e'), KeyModifiers::NONE)));
    // BOTH halves, in this test. "Did not collapse" alone passes against a
    // build where `e` reaches nothing at all, while the test's NAME goes on
    // claiming it revealed. With no file open the reveal arm says so, and
    // that is the presence half over the same state.
    assert_eq!(
        app.status, "Reveal in Explorer View: no active file",
        "Cmd+K E must still reach reveal-in-Explorer"
    );
    assert!(
        app.terminals[1].collapsed,
        "and must not have been quietly taken over by collapse"
    );
}

/// While a session is shown the tree carries the list as its last section;
/// a change swaps that section rather than stacking a second one.
#[test]
fn the_tree_breakpoint_section_follows_changes_without_duplicating() {
    use crate::widgets::run_debug::{DebugRow, DebugRowKind};
    let (_tmp, mut app, a, _b) = app_with_breakpoints();
    app.run_debug.debug_active = true;
    app.run_debug.debug_rows = vec![DebugRow {
        indent: 0,
        kind: DebugRowKind::Header {
            title: String::from("CALL STACK"),
        },
    }];
    app.sync_breakpoint_list();
    let count = |app: &App| {
        (
            app.run_debug
                .debug_rows
                .iter()
                .filter(
                    |r| matches!(&r.kind, DebugRowKind::Header { title } if title == "BREAKPOINTS"),
                )
                .count(),
            app.run_debug
                .debug_rows
                .iter()
                .filter(|r| matches!(r.kind, DebugRowKind::Breakpoint { .. }))
                .count(),
        )
    };
    assert_eq!(count(&app), (1, 3));
    app.editor.breakpoints.get_mut(&a).unwrap().insert(2);
    app.sync_breakpoint_list();
    assert_eq!(count(&app), (1, 4), "one header, one more row");
    assert!(matches!(
        &app.run_debug.debug_rows[0].kind,
        DebugRowKind::Header { title } if title == "CALL STACK"
    ));
}

#[test]
fn renaming_a_file_in_the_explorer_keeps_its_local_history() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::write(root.join("notes.txt"), "v3\n").unwrap();
    let (mut app, _hist) = app_with_history(&root);
    crate::history::record_in(&app.history_root, &root.join("notes.txt"), b"v1\n", 1_000).unwrap();
    crate::history::record_in(&app.history_root, &root.join("notes.txt"), b"v2\n", 60_000).unwrap();
    assert!(app.run_file_move(FileMove::Rename {
        parent: root.clone(),
        old: root.join("notes.txt"),
        new_name: String::from("journal.txt"),
    }));
    let moved = crate::history::entries_in(&app.history_root, &root.join("journal.txt"));
    assert_eq!(
        moved.iter().map(|s| s.millis).collect::<Vec<_>>(),
        [60_000, 1_000]
    );
    assert!(crate::history::entries_in(&app.history_root, &root.join("notes.txt")).is_empty());
}

#[test]
fn cutting_a_folder_into_another_keeps_its_files_history() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(root.join("archive")).unwrap();
    std::fs::write(root.join("docs").join("a.md"), "a\n").unwrap();
    let (mut app, _hist) = app_with_history(&root);
    let old = root.join("docs").join("a.md");
    crate::history::record_in(&app.history_root, &old, b"a0\n", 1_000).unwrap();
    assert!(app.run_file_move(FileMove::Paste {
        dest_dir: root.join("archive"),
        paths: vec![root.join("docs")],
    }));
    let new = root.join("archive").join("docs").join("a.md");
    assert!(new.exists());
    assert_eq!(crate::history::entries_in(&app.history_root, &new).len(), 1);
    assert!(crate::history::entries_in(&app.history_root, &old).is_empty());
}

#[test]
fn moving_files_and_folders_carries_every_open_tab_along() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("dir")).unwrap();
    std::fs::create_dir_all(root.join("dest")).unwrap();
    std::fs::write(root.join("a.txt"), "a").unwrap();
    std::fs::write(root.join("dir/b.txt"), "b").unwrap();
    std::fs::write(root.join("c.txt"), "c").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open_pinned(&root.join("a.txt")).unwrap();
    app.editor.open_in_new_tab(&root.join("dir/b.txt")).unwrap();
    app.editor.open_in_new_tab(&root.join("c.txt")).unwrap();
    // c.txt is active; a.txt and dir/b.txt sit in background tabs.
    assert!(app.apply_paste_or_drop(
        &root.join("dest"),
        &[root.join("a.txt"), root.join("dir")],
        ExplorerClipMode::Cut
    ));
    let paths: Vec<_> = app
        .editor
        .editors
        .iter()
        .filter_map(|e| e.path.clone())
        .collect();
    assert!(paths.contains(&root.join("dest/a.txt")), "{paths:?}");
    assert!(paths.contains(&root.join("dest/dir/b.txt")), "{paths:?}");
}

#[test]
fn the_explorer_dims_files_missing_from_the_scrubbed_commit() {
    // #371: at a commit, a file its tree lacks dims; the working tree and
    // closing the scrubber clear it.
    let repo = scrub_repo();
    std::fs::write(repo.path().join("new.txt"), "fresh\n").unwrap();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let new = repo.path().join("new.txt");
    let old = repo.path().join("a.txt");
    app.scrub_history();
    assert!(app.tree.scrub_tree.is_none(), "opening parks at the tree");
    app.handle_scrubber_key(KeyCode::Left);
    assert!(app.tree.is_absent_at_scrub(&new), "new.txt is not in HEAD");
    assert!(!app.tree.is_absent_at_scrub(&old), "a.txt is");
    app.handle_scrubber_key(KeyCode::Home);
    assert!(
        !app.tree.is_absent_at_scrub(&new),
        "Home clears the dimming"
    );
    app.handle_scrubber_key(KeyCode::Left);
    assert!(app.tree.is_absent_at_scrub(&new));
    app.handle_scrubber_key(KeyCode::Right);
    assert!(
        !app.tree.is_absent_at_scrub(&new),
        "stepping to the working tree clears it"
    );
    app.handle_scrubber_key(KeyCode::Left);
    app.handle_scrubber_key(KeyCode::Esc);
    assert!(app.tree.scrub_tree.is_none(), "closing clears it");
}

#[test]
fn saving_a_file_whose_folder_was_removed_puts_the_folder_back() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_plan_whose_folder_was_removed(tmp.path());
    // The first press may stop to say the file changed on disk; the second
    // is the consent to write it anyway.
    app.save();
    if app.editor.dirty {
        app.save();
    }
    assert!(!app.editor.dirty, "still unsaved: {}", app.status);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("notes/plan.md")).unwrap(),
        "# Plan\n\n- step one\n- step two\n"
    );
}

#[test]
fn a_save_whose_folder_is_now_a_file_still_fails_and_leaves_that_file_alone() {
    // Negative: putting a missing folder back must not paper over a path
    // that can't be a folder. A file now sits where the folder was.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_plan_whose_folder_was_removed(tmp.path());
    std::fs::write(tmp.path().join("notes"), "a file, not a folder\n").unwrap();
    app.save();
    app.save();
    assert!(app.editor.dirty, "the edits are still only in the buffer");
    assert!(app.status.starts_with("Save failed"), "{}", app.status);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("notes")).unwrap(),
        "a file, not a folder\n"
    );
}

/// #852: File: New File… / New Folder… open the Explorer's create prompt
/// from any pane: beside the active file from the editor, in the Explorer
/// selection while the Explorer has focus (as its own chords do).
#[test]
fn new_file_and_folder_from_the_palette_open_the_create_prompt() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
    let mut app = app_with_open_file(tmp.path(), "sub/a.txt", "x");
    let file_dir = app
        .editor
        .path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap();
    assert!(file_dir.ends_with("sub"));
    app.run_command(Command::NewFile);
    match app.prompt.as_ref() {
        Some(Prompt {
            kind: PromptKind::Create(CreateKind::File),
            target_dir,
            ..
        }) => assert_eq!(target_dir, &file_dir),
        _ => panic!("File: New File… must open the New File prompt"),
    }
    app.prompt = None;
    app.focus_pane(Pane::Tree);
    app.sidebar_view = SidebarView::Explorer;
    app.run_command(Command::NewFolder);
    let explorer_dir = app.explorer_create_target_dir();
    match app.prompt.as_ref() {
        Some(Prompt {
            kind: PromptKind::Create(CreateKind::Folder),
            target_dir,
            ..
        }) => assert_eq!(target_dir, &explorer_dir),
        _ => panic!("File: New Folder… must open the New Folder prompt"),
    }
}

#[test]
fn a_click_on_the_explorer_outline_still_jumps_with_the_secondary_bar_up() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = secondary_outline_app(&tmp, 30);
    let backend = ratatui::backend::TestBackend::new(160, 45);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let (left, _) = outline_hosts(&app);
    let (x, y) = find_painted(&term, "sym_004", left).expect("sym_004 in the Explorer");
    left_click(&mut app, x + 2, y);
    assert_eq!(
        app.editor.cursor_row, 8,
        "the caret moves to sym_004's line"
    );
}
