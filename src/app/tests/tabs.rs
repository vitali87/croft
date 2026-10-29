//! Editor tabs and groups: the tab strip, preview and pinned tabs, splits and
//! open editors.

use super::*;

#[test]
fn a_tab_strip_binding_fires_only_on_the_strip() {
    // `gesture_for` deliberately shares `editor_click` between the strip and
    // the editor body. That is only safe because a double needs both clicks
    // at the SAME cell and the two regions never overlap, so pin it.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp) = tree_app_with_keymap(
        r#"[{"key": "ctrl+click", "command": "quick_open", "when": "tab_strip"}]"#,
    );
    let strip_y = app.editor.tab_strip_y_for_test();
    let tab = app
        .editor
        .tab_at(app.editor.last_inner.x + 1, strip_y)
        .map(|_| (app.editor.last_inner.x + 1, strip_y))
        .expect("a rendered tab cell to aim at");

    let mut on_strip = mouse(MouseEventKind::Down(MouseButton::Left), tab.0, tab.1);
    on_strip.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(on_strip);
    assert!(
        app.file_finder.is_some(),
        "the strip row fires on the strip"
    );

    // The body is a different region, so the same spelling must fall through
    // to the built-in rather than firing the tab_strip row.
    app.file_finder = None;
    let body_row = app.editor.last_inner.y + 1;
    assert_ne!(body_row, strip_y, "the strip and the body do not overlap");
    let mut in_body = mouse(
        MouseEventKind::Down(MouseButton::Left),
        app.editor.last_inner.x + 4,
        body_row,
    );
    in_body.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(in_body);
    assert!(
        app.file_finder.is_none(),
        "a tab_strip binding must not fire in the editor body"
    );
}

fn active_tab_name(app: &App) -> String {
    tab_names(app)[app.editor.active_index()].clone()
}

/// #1427: a middle-click on a background tab closes that tab, as in VS
/// Code and browsers, and the tab being worked in stays active.
#[test]
fn a_middle_click_on_a_background_tab_closes_that_tab() {
    use crossterm::event::MouseButton;
    let (mut app, _tmp) = three_tab_app("[]");
    assert_eq!(active_tab_name(&app), "c.txt");
    let a = tab_cell(&app, 0);
    press(&mut app, MouseButton::Middle, a);
    assert_eq!(tab_names(&app), ["b.txt", "c.txt"]);
    assert_eq!(active_tab_name(&app), "c.txt");
}

/// #1427: a tab command bound to a tab-strip gesture acts on the clicked
/// tab. `close_editor` on a middle-click used to close the active tab.
#[test]
fn a_tab_strip_close_binding_closes_the_clicked_tab() {
    use crossterm::event::MouseButton;
    let (mut app, _tmp) = three_tab_app(
        r#"[{"key": "middle_click", "command": "close_editor", "when": "tab_strip"}]"#,
    );
    let a = tab_cell(&app, 0);
    press(&mut app, MouseButton::Middle, a);
    assert_eq!(tab_names(&app), ["b.txt", "c.txt"]);
    assert_eq!(active_tab_name(&app), "c.txt");

    let (mut app, _tmp) = three_tab_app(
        r#"[{"key": "ctrl+click", "command": "close_other_editors", "when": "tab_strip"}]"#,
    );
    let (x, y) = tab_cell(&app, 1);
    let mut ev = mouse(
        crossterm::event::MouseEventKind::Down(MouseButton::Left),
        x,
        y,
    );
    ev.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ev);
    assert_eq!(
        tab_names(&app),
        ["b.txt"],
        "the clicked tab is the one kept"
    );
}

/// #1427: only the command a tab-strip gesture is bound to acts on the
/// clicked tab. `rebase_abort` closes its own plan tab through Close Editor,
/// and that nested close used to take the clicked background tab instead.
#[test]
fn a_tab_strip_binding_leaves_nested_tab_commands_on_the_active_tab() {
    use crossterm::event::MouseButton;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for (name, text) in [
        ("a.py", "a\n"),
        ("b.txt", "b\n"),
        ("git-rebase-todo", "pick a b\n"),
    ] {
        let f = tmp.path().join(name);
        std::fs::write(&f, text).unwrap();
        app.editor.open_pinned(&f).unwrap();
    }
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "middle_click", "command": "rebase_abort", "when": "tab_strip"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let a = tab_cell(&app, 0);
    press(&mut app, MouseButton::Middle, a);
    assert_eq!(tab_names(&app), ["a.py", "b.txt"], "the plan tab closed");
    assert_eq!(app.tab_click_target, None);
}

/// #1427 negative: outside the strip, the tab commands keep acting on the
/// active tab, from a binding in the editor or from the keyboard, and a
/// middle-click in the text closes nothing.
#[test]
fn off_the_strip_tab_commands_still_act_on_the_active_tab() {
    use crossterm::event::MouseButton;
    let (mut app, _tmp) =
        three_tab_app(r#"[{"key": "ctrl+click", "command": "close_editor", "when": "editor"}]"#);
    let body = (app.editor.last_inner.x + 6, app.editor.last_inner.y);
    press(&mut app, MouseButton::Middle, body);
    assert_eq!(tab_names(&app).len(), 3, "a middle-click in the text");
    let mut ev = mouse(
        crossterm::event::MouseEventKind::Down(MouseButton::Left),
        body.0,
        body.1,
    );
    ev.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ev);
    assert_eq!(
        tab_names(&app),
        ["a.py", "b.txt"],
        "the active c.txt closed"
    );
    app.run_command(crate::widgets::command_palette::Command::CloseEditor);
    assert_eq!(tab_names(&app), ["a.py"], "from the keyboard too");
}

/// #1635 negative: moving focus to another editor group showing another
/// file is not a file switch. A history entry holds no group, so Go Back
/// would reopen the left group's file in the focused one.
#[test]
fn focusing_another_editor_group_records_no_go_back_location() {
    let (_tmp, mut app, a, b) = nav_switch_fixture();
    app.split_editor(); // right group (focused) duplicates a.txt
    app.editor.open_pinned(&b).unwrap();
    assert_eq!(app.editor.path.as_deref(), Some(b.as_path()));
    let changes = app.nav.changes();
    app.handle_key(key(KeyCode::Left, KeyModifiers::SUPER | KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(a.as_path()),
        "{}",
        app.status
    );
    assert_eq!(app.nav.changes(), changes, "no history entry recorded");
}

/// A boundary dir (any repo root: it holds `.git`) gets NO FSEvents watch on
/// macOS, by the iron law that keeps the build-churn storm dead. An in-place
/// write does not move the parent dir's mtime either, so for a file sitting
/// directly in one, the poll's per-file stat is the ONLY thing that can notice
/// a change — and it tracked the ACTIVE tab alone, leaving a background tab on
/// stale text indefinitely.
#[test]
fn the_poll_notices_a_background_tab_whose_file_changed_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let bg = tmp.path().join("bg.txt");
    let front = tmp.path().join("front.txt");
    std::fs::write(&bg, "old\n").unwrap();
    std::fs::write(&front, "front\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&bg).unwrap();
    app.editor.open_pinned(&front).unwrap(); // front.txt is now the active tab
    app.sync_open_file_poll_mtime();
    app.fs_watch.disable();
    // Baseline the dir mtimes so the sweep cannot be driven by `dirs_changed`.
    app.fs_watch.force_poll_due();
    let _ = app.drain_fs_events();

    // Rewrite the BACKGROUND file in place: same directory entry, so the dir's
    // own mtime does not move.
    std::fs::write(&bg, "new content\n").unwrap();
    app.fs_watch.force_poll_due();
    let _ = app.drain_fs_events();

    let reloaded = app
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(bg.as_path()))
        .expect("the background tab is still open")
        .lines
        .clone();
    assert_eq!(
        reloaded,
        vec!["new content"],
        "a background tab must pick up an in-place external write"
    );
}

/// Issue #209: VS Code's Reopen Closed Editor. Closing a tab records it;
/// Cmd+K Shift+W restores the most recently closed tab with its cursor
/// position, LIFO across multiple closes.
#[test]
fn cmd_k_shift_w_reopens_the_last_closed_tab_with_its_cursor() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "fn a() {}\nfn a2() {}\n").unwrap();
    std::fs::write(&b, "fn b() {}\nfn b2() {}\nfn b3() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.editor.open_pinned(&b).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 2;
    // Close b.rs (active), then a.rs, both through the keyboard path.
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.path.as_deref(), Some(a.as_path()));
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    // Reopen walks the closes in reverse order: a.rs first, then b.rs
    // with its remembered cursor row.
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('W'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(a.as_path()),
        "the first reopen restores the most recent close"
    );
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('W'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(b.as_path()),
        "the second reopen walks back to the earlier close"
    );
    assert_eq!(
        app.editor.cursor_row, 2,
        "the reopened tab must restore its cursor row"
    );
}

/// A closed tab whose file has since vanished is skipped, falling through
/// to the next reopenable entry rather than failing.
#[test]
fn reopen_closed_tab_skips_files_deleted_since_the_close() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "fn a() {}\n").unwrap();
    std::fs::write(&b, "fn b() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.editor.open_pinned(&b).unwrap();
    app.focus_pane(Pane::Editor);
    // Close b.rs then a.rs; both are recorded (a.rs closes down to the
    // blank buffer, which still counts as closing the tab).
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    // The stack top is a.rs (the last close); deleting IT forces the
    // missing-file skip down to b.rs.
    std::fs::remove_file(&a).unwrap();
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('W'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(b.as_path()),
        "a vanished top entry is skipped and the next closed tab reopens"
    );
}

/// Close Others records every unpinned tab it drops, so each can be
/// reopened in most-recent-first order.
#[test]
fn close_others_records_the_dropped_tabs_for_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    let c = tmp.path().join("c.rs");
    for f in [&a, &b, &c] {
        std::fs::write(f, "x\n").unwrap();
    }
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.editor.open_pinned(&b).unwrap();
    app.editor.open_pinned(&c).unwrap();
    let keep = app.editor.active_index();
    app.dispatch_menu_action(MenuAction::CloseOtherTabs(keep), tmp.path().to_path_buf());
    assert_eq!(app.editor.tab_count(), 1, "only c.rs survives");
    app.reopen_closed_tab();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(b.as_path()),
        "the last-recorded dropped tab reopens first"
    );
    app.reopen_closed_tab();
    assert_eq!(app.editor.path.as_deref(), Some(a.as_path()));
}

/// Tab context "Close All" must close the tabs in EVERY split group, not
/// just the clicked one. With files open side by side (e.g. `cgr
/// duplicates`), it used to empty only the focused group, whose blank pane
/// then collapsed away and promoted the other group — so it looked like a
/// single tab closed.
#[test]
fn close_all_closes_every_split_group() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    for f in [&a, &b] {
        std::fs::write(f, "x\n").unwrap();
    }
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.split_editor(); // right group (focused) duplicates a.rs
    app.editor.open_pinned(&b).unwrap();
    assert!(app.editor_layout.is_split(), "two groups before Close All");
    let total: usize = std::iter::once(&app.editor)
        .chain(app.editor_layout.inactive_groups())
        .map(|g| g.editors.len())
        .sum();
    assert!(total >= 2, "tabs are spread over both groups");
    app.dispatch_menu_action(MenuAction::CloseAllTabs, tmp.path().to_path_buf());
    assert!(
        !app.editor_layout.is_split(),
        "Close All collapses the split back to a single group"
    );
    assert_eq!(app.editor.tab_count(), 1, "a single blank pane remains");
    assert_eq!(app.editor.path, None, "no file stays open in any group");
    assert_eq!(app.status, format!("Closed {total} tabs"));
}

/// #862: a clean tab closes at once, exactly as before.
#[test]
fn closing_a_clean_tab_does_not_ask() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "alpha\n");
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.pending_unsaved, None);
    assert_eq!(app.editor.path, None);
    assert_eq!(app.status, "Closed tab");
}

/// #862: the tab context menu's Close asks about an unsaved tab, and its
/// Close Others / Close All keep one open rather than dropping it.
#[test]
fn tab_menu_closes_keep_unsaved_tabs() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "bravo\n").unwrap();
    app.editor.open_pinned(&b).unwrap();
    let root = tmp.path().to_path_buf();

    app.dispatch_menu_action(MenuAction::CloseTab(0), root.clone());
    assert!(
        app.pending_unsaved.is_some(),
        "Close on the unsaved tab asks"
    );
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    app.dispatch_menu_action(MenuAction::CloseOtherTabs(1), root.clone());
    assert_eq!(
        app.editor.tab_count(),
        2,
        "Close Others keeps the unsaved a.txt"
    );

    app.dispatch_menu_action(MenuAction::CloseAllTabs, root);
    assert_eq!(app.editor.tab_count(), 1);
    assert_eq!(app.editor.path.as_deref(), Some(a.as_path()));
    assert!(app.editor.dirty);
    assert_eq!(app.status, "Closed 1 tab; kept 1 with unsaved changes open");
}

/// #862: Close All keeps a split whose other group holds unsaved edits, and
/// folds away only the groups it emptied.
#[test]
fn close_all_keeps_the_split_group_holding_unsaved_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    app.split_editor();
    assert!(app.editor_layout.is_split(), "setup: two groups");
    app.dispatch_menu_action(MenuAction::CloseAllTabs, tmp.path().to_path_buf());
    assert!(
        !app.editor_layout.is_split(),
        "the emptied focused group folded away"
    );
    assert_eq!(app.editor.path.as_deref(), Some(a.as_path()));
    assert!(app.editor.dirty, "the unsaved group survives, promoted");
}

/// #862: a tab whose unsaved text another open tab holds too (a symbol tab
/// and its file's tab mirror one buffer) closes without asking: nothing is
/// lost while the other keeps it.
#[test]
fn a_tab_whose_edits_another_tab_holds_closes_without_asking() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_unsaved_file(tmp.path());
    let edited = app.editor.lines.clone();
    app.split_editor();
    app.editor.lines = edited.clone();
    app.editor.dirty = true;
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.pending_unsaved, None, "no prompt");
    assert!(!app.editor_layout.is_split(), "the tab closed");
    assert_eq!(app.editor.lines, edited, "the other copy kept the edits");
    assert!(app.editor.dirty);
}

/// #862 negative: S at a close prompt writes the tab it asked about and no
/// other; a second unsaved tab stays unsaved and open.
#[test]
fn s_at_the_close_prompt_saves_only_that_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "bravo\n").unwrap();
    app.editor.open_pinned(&b).unwrap();
    type_into_editor(&mut app, "y");
    app.editor.select(0);
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "xalpha\n");
    assert_eq!(
        std::fs::read_to_string(&b).unwrap(),
        "bravo\n",
        "the other unsaved tab is not written"
    );
    assert_eq!(app.editor.tab_count(), 1);
    assert_eq!(app.editor.path.as_deref(), Some(b.as_path()));
    assert!(app.editor.dirty, "and keeps its edits");
}

/// #862 negative: an answer never lands on a tab other than the one asked
/// about. When the tab at that index is another file by the time the answer
/// comes, D closes nothing and discards nothing.
#[test]
fn an_answer_for_a_tab_that_moved_closes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "bravo\n").unwrap();
    app.editor.open_pinned(&b).unwrap();
    app.editor.select(0);
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    assert!(matches!(
        app.pending_unsaved,
        Some(UnsavedExit::CloseTab { idx: 0, .. })
    ));
    // Something other than the user reorders the tabs meanwhile.
    app.editor.editors.swap(0, 1);
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.tab_count(), 2, "nothing closed");
    assert!(app.status.contains("moved"), "{}", app.status);
    let a_tab = app
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(a.as_path()))
        .unwrap();
    assert!(a_tab.dirty && a_tab.lines[0] == "xalpha", "edits kept");
}

/// #862 review (CodeRabbit, security): a tab closed without saving had its
/// text left in the hot-exit backup until a later tick rewrote it, so a
/// crash in between brought back text the user had chosen to discard. The
/// close rewrites the backup at once.
#[test]
fn discarding_a_tab_takes_its_text_out_of_the_backup_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, a) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    assert_eq!(
        hot_exit_texts_of(cache.path(), tmp.path(), &a).len(),
        1,
        "setup: backed up"
    );
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.status.contains("without saving"), "{}", app.status);
    assert_eq!(
        hot_exit_texts_of(cache.path(), tmp.path(), &a),
        Vec::<String>::new(),
        "no tick has run, and the discarded text is gone from disk"
    );
}

/// #862 review, the guard: discarding one tab leaves the others' unsaved
/// text in the backup.
#[test]
fn discarding_a_tab_keeps_the_other_unsaved_tabs_backed_up() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, a) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "bravo\n").unwrap();
    app.editor.open_pinned(&b).unwrap();
    type_into_editor(&mut app, "z");
    settle_hot_exit(&mut app);
    assert_eq!(app.editor.path.as_deref(), Some(b.as_path()), "setup");
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        hot_exit_texts_of(cache.path(), tmp.path(), &b),
        Vec::<String>::new()
    );
    assert_eq!(hot_exit_texts_of(cache.path(), tmp.path(), &a).len(), 1);
}

/// Guard (#843): with no split, "View: Focus Left/Right Editor Group" change
/// nothing, as the chords do: focus stays where it was.
#[test]
fn focusing_an_editor_group_without_a_split_changes_nothing() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "only");
    app.focus_pane(Pane::Tree);
    for cmd in [
        Command::FocusLeftEditorGroup,
        Command::FocusRightEditorGroup,
    ] {
        app.run_command(cmd);
        assert!(app.focus == Pane::Tree, "{cmd:?}");
        assert!(!app.editor_layout.is_split());
    }
}

/// #1438: with every repository tab saved, Commit asks nothing.
#[test]
fn a_saved_tab_does_not_hold_the_commit() {
    let (tmp, mut app) = scm_ready_to_commit();
    app.editor
        .open_pinned(&tmp.path().join("seed.txt"))
        .unwrap();
    app.source_control.message = "bump seed".to_string();
    app.source_control.message_cursor = app.source_control.message.chars().count();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_git_net(&mut app);
    assert!(app.pending_unsaved_scm.is_none());
    assert_eq!(last_commit_message(tmp.path()), "bump seed\n\n");
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
        state.tabs[state.active_tab].path.as_deref(),
        Some(file_a.as_path())
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
fn build_tab_context_menu_items_includes_all_four_close_actions_in_the_middle_of_a_strip() {
    // Right-click on tab 1 of 3 (i.e. a middle tab) should surface
    // every close action — there's at least one tab on each side.
    let items = build_tab_context_menu_items(1, 3, false, None, false, false, false);
    assert_eq!(
        menu_labels(&items),
        [
            "Close",
            "Close Others",
            "Close to the Right",
            "Close Saved",
            "Close All",
            "Pin",
            "Split Right",
            "Split & Move",
            "Move into New Window",
        ],
        "all close actions must appear and in this order — mirrors VS Code's tab strip"
    );
    assert!(matches!(
        menu_action_of(&items, "Close"),
        Some(MenuAction::CloseTab(1))
    ));
    assert!(matches!(
        menu_action_of(&items, "Close Others"),
        Some(MenuAction::CloseOtherTabs(1))
    ));
    assert!(matches!(
        menu_action_of(&items, "Close to the Right"),
        Some(MenuAction::CloseTabsToRight(1))
    ));
    assert!(matches!(
        menu_action_of(&items, "Close Saved"),
        Some(MenuAction::CloseSavedTabs)
    ));
    assert!(matches!(
        menu_action_of(&items, "Close All"),
        Some(MenuAction::CloseAllTabs)
    ));
}

#[test]
fn build_tab_context_menu_items_hides_close_to_the_right_on_the_last_tab() {
    // Last tab → "Close to the Right" would close zero tabs, so it
    // must be suppressed (matches VS Code).
    let items = build_tab_context_menu_items(2, 3, false, None, false, false, false);
    assert_eq!(
        menu_labels(&items),
        [
            "Close",
            "Close Others",
            "Close Saved",
            "Close All",
            "Pin",
            "Split Right",
            "Split & Move",
            "Move into New Window",
        ],
        "Close to the Right must be suppressed on the rightmost tab"
    );
}

#[test]
fn build_tab_context_menu_items_hides_close_others_when_only_one_tab_is_open() {
    let items = build_tab_context_menu_items(0, 1, false, None, false, false, false);
    assert_eq!(
        menu_labels(&items),
        [
            "Close",
            "Close All",
            "Pin",
            "Split Right",
            "Split & Move",
            "Move into New Window",
        ],
        "with a single tab, Close Others / Close to the Right / Close Saved are all no-ops and must be suppressed"
    );
}

#[test]
fn build_tab_context_menu_items_includes_reveal_in_finder_only_with_a_path_and_enabled() {
    let p = std::path::Path::new("/tmp/demo.rs");
    // Local macOS with a real path: Reveal in Finder sits after Close All,
    // before the split group — mirroring VS Code's lower-middle slot.
    let items = build_tab_context_menu_items(0, 1, false, Some(p), true, false, false);
    assert_eq!(
        menu_labels(&items),
        [
            "Close",
            "Close All",
            "Copy Path",
            "Copy Relative Path",
            "Reveal in Finder",
            "Reveal in Explorer View",
            "Pin",
            "Split Right",
            "Split & Move",
            "Move into New Window",
            "Copy into New Window",
        ],
    );
    assert!(matches!(
        menu_action_of(&items, "Reveal in Finder"),
        Some(MenuAction::RevealInFinder(rp)) if rp == p
    ));
    // Remote (or non-macOS) session: entry withheld even with a path.
    let remote = build_tab_context_menu_items(0, 1, false, Some(p), false, false, false);
    assert!(!menu_labels(&remote).contains(&"Reveal in Finder"));
    // A blank/untitled buffer (no path) never offers it, even when enabled.
    let blank = build_tab_context_menu_items(0, 1, false, None, true, false, false);
    assert!(!menu_labels(&blank).contains(&"Reveal in Finder"));
}

#[test]
fn build_tab_context_menu_items_offers_reveal_in_explorer_for_any_path() {
    let p = std::path::Path::new("/tmp/demo.rs");
    // Reveal in Explorer View targets croft's own tree, present local and
    // remote, so it shows for any path-bearing tab even when Finder is withheld.
    let remote = build_tab_context_menu_items(0, 1, false, Some(p), false, false, false);
    assert_eq!(
        menu_labels(&remote),
        [
            "Close",
            "Close All",
            "Copy Path",
            "Copy Relative Path",
            "Reveal in Explorer View",
            "Pin",
            "Split Right",
            "Split & Move",
            "Move into New Window",
            "Copy into New Window",
        ]
    );
    assert!(matches!(
        menu_action_of(&remote, "Reveal in Explorer View"),
        Some(MenuAction::RevealInExplorer(rp)) if rp == p
    ));
    // A blank/untitled buffer (no path) has nothing to reveal.
    let blank = build_tab_context_menu_items(0, 1, false, None, true, false, false);
    assert!(!menu_labels(&blank).contains(&"Reveal in Explorer View"));
}

#[test]
fn build_tab_context_menu_items_offers_copy_path_for_any_path_local_or_remote() {
    let p = std::path::Path::new("/tmp/demo.rs");
    // Copy Path is a clipboard write, so it shows for a path-bearing tab even
    // on a remote session where Reveal in Finder is withheld.
    let remote = build_tab_context_menu_items(0, 1, false, Some(p), false, false, false);
    assert_eq!(
        menu_labels(&remote),
        [
            "Close",
            "Close All",
            "Copy Path",
            "Copy Relative Path",
            "Reveal in Explorer View",
            "Pin",
            "Split Right",
            "Split & Move",
            "Move into New Window",
            "Copy into New Window",
        ]
    );
    assert!(matches!(
        menu_action_of(&remote, "Copy Path"),
        Some(MenuAction::CopyTabPath(cp)) if cp == p
    ));
    assert!(matches!(
        menu_action_of(&remote, "Copy Relative Path"),
        Some(MenuAction::CopyTabRelativePath(cp)) if cp == p
    ));
    // A blank/untitled buffer (no path) has nothing to copy.
    let blank = build_tab_context_menu_items(0, 1, false, None, true, false, false);
    assert!(!menu_labels(&blank).contains(&"Copy Path"));
    assert!(!menu_labels(&blank).contains(&"Copy Relative Path"));
}

#[test]
fn build_tab_context_menu_items_offers_keep_open_only_for_a_preview_tab() {
    let p = std::path::Path::new("/tmp/demo.rs");
    // Preview tab (italic): "Keep Open" appears, carrying this tab's index, and
    // sits just before the Pin / split group.
    let preview = build_tab_context_menu_items(0, 1, false, Some(p), false, true, false);
    assert!(matches!(
        menu_action_of(&preview, "Keep Open"),
        Some(MenuAction::KeepTabOpen(0))
    ));
    let labels = menu_labels(&preview);
    assert!(
        labels.iter().position(|&s| s == "Keep Open") < labels.iter().position(|&s| s == "Pin")
    );
    // A permanent (non-preview) tab never offers it.
    let permanent = build_tab_context_menu_items(0, 1, false, Some(p), false, false, false);
    assert!(!menu_labels(&permanent).contains(&"Keep Open"));
}

#[test]
fn build_tab_context_menu_items_offers_pin_or_unpin_carrying_the_tab_index() {
    let p = std::path::Path::new("/tmp/demo.rs");
    // Unpinned tab reads "Pin"; pinned tab reads "Unpin". Either way the entry
    // carries this tab's index and sits between Keep Open and the split group.
    let unpinned = build_tab_context_menu_items(0, 1, false, Some(p), true, false, false);
    assert!(matches!(
        menu_action_of(&unpinned, "Pin"),
        Some(MenuAction::ToggleTabPin(0))
    ));
    let labels = menu_labels(&unpinned);
    assert!(!labels.contains(&"Unpin"));
    let keep = labels.iter().position(|&s| s == "Keep Open");
    let pin_pos = labels.iter().position(|&s| s == "Pin");
    let split = labels.iter().position(|&s| s == "Split Right");
    assert!(keep < pin_pos && pin_pos < split);

    let pinned = build_tab_context_menu_items(0, 1, false, Some(p), false, false, true);
    assert!(matches!(
        menu_action_of(&pinned, "Unpin"),
        Some(MenuAction::ToggleTabPin(0))
    ));
    assert!(!menu_labels(&pinned).contains(&"Pin"));
}

#[test]
fn tab_menu_groups_split_and_move_into_a_submenu() {
    let items = build_tab_context_menu_items(0, 1, false, None, false, false, false);
    // A flat top-level "Split Right" plus a "Split & Move" submenu.
    assert!(items.iter().any(|e| matches!(
        e,
        MenuEntry::Item { label, action: MenuAction::SplitEditor } if label == "Split Right"
    )));
    let sub = items
        .iter()
        .find_map(|e| match e {
            MenuEntry::Submenu { label, items } if label == "Split & Move" => Some(items),
            _ => None,
        })
        .expect("a Split & Move submenu");
    let labels: Vec<&str> = sub
        .iter()
        .map(|e| match e {
            MenuEntry::Item { label, .. } => label.as_str(),
            MenuEntry::Submenu { label, .. } => label.as_str(),
            MenuEntry::Header(label) => label.as_str(),
            MenuEntry::Separator => "---",
        })
        .collect();
    assert_eq!(
        labels,
        [
            "Split Up",
            "Split Down",
            "Split Left",
            "Split Right",
            "---",
            "Move Above",
            "Move Below",
            "Move Left",
            "Move Right",
            "---",
            "Split in Group",
        ]
    );
    // VS Code's vertical labels: Above = Up, Below = Down.
    assert!(matches!(
        &sub[5],
        MenuEntry::Item {
            action: MenuAction::MoveEditorUp,
            ..
        }
    ));
    assert!(matches!(
        &sub[6],
        MenuEntry::Item {
            action: MenuAction::MoveEditorDown,
            ..
        }
    ));
}

/// Open the editor tab menu into `app.context_menu` with the cursor on the
/// "Split & Move" submenu parent.
fn open_tab_menu_on_split_and_move(app: &mut App) {
    let items = build_tab_context_menu_items(0, 1, false, None, false, false, false);
    let sub_idx = items
        .iter()
        .position(|e| menu_label(e) == "Split & Move")
        .unwrap();
    app.context_menu = Some(ContextMenu {
        crumb: None,
        scroll: Default::default(),
        origin: (0, 0),
        items,
        selected: sub_idx,
        open_submenu: None,
        submenu_selected: 0,
        target_dir: std::path::PathBuf::from("/"),
    });
}

#[test]
fn split_and_move_submenu_opens_and_down_skips_the_divider() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    open_tab_menu_on_split_and_move(&mut app);
    // Right opens the submenu, selecting its first child (Split Up).
    app.handle_menu_key(key(KeyCode::Right, KeyModifiers::NONE));
    {
        let m = app.context_menu.as_ref().unwrap();
        assert!(m.open_submenu.is_some(), "Right opened the submenu");
        assert_eq!(menu_label(m.cursor_entry().unwrap()), "Split Up");
    }
    // Down three times reaches "Split Right" (the last split before the
    // divider); one more Down SKIPS the divider onto "Move Above".
    for _ in 0..3 {
        app.handle_menu_key(key(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(
        menu_label(app.context_menu.as_ref().unwrap().cursor_entry().unwrap()),
        "Split Right"
    );
    app.handle_menu_key(key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(
        menu_label(app.context_menu.as_ref().unwrap().cursor_entry().unwrap()),
        "Move Above",
        "Down must step over the divider, never land on it"
    );
    // Enter on a child dispatches its action and closes the whole menu.
    app.handle_menu_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        app.context_menu.is_none(),
        "Enter on a child closes the menu"
    );
}

#[test]
fn split_and_move_submenu_left_and_esc_back_out_before_dismissing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    open_tab_menu_on_split_and_move(&mut app);
    app.handle_menu_key(key(KeyCode::Right, KeyModifiers::NONE));
    // Left closes the submenu but keeps the menu open at the top level.
    app.handle_menu_key(key(KeyCode::Left, KeyModifiers::NONE));
    assert!(app.context_menu.as_ref().unwrap().open_submenu.is_none());
    assert!(app.context_menu.is_some());
    // Re-open; Esc backs out of the submenu first...
    app.handle_menu_key(key(KeyCode::Right, KeyModifiers::NONE));
    app.handle_menu_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.context_menu.as_ref().unwrap().open_submenu.is_none());
    assert!(
        app.context_menu.is_some(),
        "first Esc only closes the submenu"
    );
    // ...and a second Esc dismisses the whole menu.
    app.handle_menu_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.context_menu.is_none(), "second Esc dismisses the menu");
}

#[test]
fn cmd_k_shift_p_keeps_the_preview_tab_open_and_the_menu_says_so() {
    // Keep Open lost its Cmd+K Enter chord to run-test-at-cursor, but the
    // tab menu kept advertising "⌘K ⏎" — a hint that now starts a test run.
    // The action lives on Cmd+K ⇧P (sibling of Cmd+K P pin), and the menu
    // label must say exactly that.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "x").unwrap();
    app.editor.open_preview(&f).unwrap();
    let idx = app.editor.active_index();
    assert!(app.editor.is_preview(idx), "opened as a preview tab");
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('P'), KeyModifiers::SHIFT))
        .unwrap();
    assert!(
        !app.editor.is_preview(app.editor.active_index()),
        "Cmd+K Shift+P promotes the preview to a kept-open tab"
    );
    assert_eq!(app.status, "Kept tab open");
    assert_eq!(
        shortcut_for(&MenuAction::KeepTabOpen(0)),
        Some("⌘K ⇧P"),
        "the tab menu must advertise the chord that actually works"
    );
}

#[test]
fn cmd_k_p_pins_then_unpins_the_active_tab() {
    // Pin moved to Cmd+K P: its old Shift+Enter chord now debugs the test
    // at the caret.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "x").unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.editor.is_pinned(app.editor.active_index()),
        "Cmd+K P pins the active tab"
    );
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        !app.editor.is_pinned(app.editor.active_index()),
        "a second Cmd+K P unpins it"
    );
}

#[test]
fn split_editor_duplicates_active_file_into_a_focused_right_group() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello\nworld\nthere");
    let path = app.editor.path.clone().unwrap();
    app.editor.cursor_row = 2;

    app.split_editor();

    assert!(
        app.editor_layout.is_split(),
        "split must create a second group"
    );
    assert_eq!(
        app.editor_layout.active_dfs_index(),
        1,
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
    let groups = app.editor_layout.inactive_groups();
    assert_eq!(groups[0].path.as_deref(), Some(path.as_path()));
}

#[test]
fn split_down_makes_a_vertical_split_with_the_new_group_active_below() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello\nworld");
    let path = app.editor.path.clone().unwrap();

    app.split_editor_dir(editor_layout::SplitDir::Vertical, true);

    assert!(app.editor_layout.is_split());
    assert_eq!(
        app.editor_layout.root_split_dir(),
        Some(editor_layout::SplitDir::Vertical),
        "Split Down stacks the groups vertically"
    );
    assert_eq!(
        app.editor_layout.active_dfs_index(),
        1,
        "the new (focused) group is the LOWER leaf"
    );
    assert_eq!(app.editor.path.as_deref(), Some(path.as_path()));
    let groups = app.editor_layout.inactive_groups();
    assert_eq!(groups[0].path.as_deref(), Some(path.as_path()));
}

#[test]
fn tab_menu_window_actions_carry_the_clicked_tab_index() {
    let p = std::path::Path::new("/tmp/demo.rs");
    let items = build_tab_context_menu_items(2, 3, false, Some(p), false, false, false);
    assert!(matches!(
        menu_action_of(&items, "Move into New Window"),
        Some(MenuAction::MoveIntoNewWindow(2))
    ));
    assert!(matches!(
        menu_action_of(&items, "Copy into New Window"),
        Some(MenuAction::CopyIntoNewWindow(2))
    ));
    // Copy needs a real path; a blank buffer omits it but still offers Move.
    let blank = build_tab_context_menu_items(0, 1, false, None, false, false, false);
    assert!(menu_labels(&blank).contains(&"Move into New Window"));
    assert!(!menu_labels(&blank).contains(&"Copy into New Window"));
}

#[test]
fn split_right_then_split_down_yields_three_tiled_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.split_editor_dir(editor_layout::SplitDir::Horizontal, true); // Split Right
    app.split_editor_dir(editor_layout::SplitDir::Vertical, true); // Split Down on the right group
    assert_eq!(
        app.editor_layout.leaf_count(),
        3,
        "repeated splitting builds a real grid, not a no-op"
    );
    // Left column full height; the right column is split top/bottom.
    let rects = app
        .editor_layout
        .leaf_rects(ratatui::layout::Rect::new(0, 0, 100, 40), 10);
    assert_eq!(rects.len(), 3);
    assert_eq!(rects[0], ratatui::layout::Rect::new(0, 0, 50, 40));
    assert_eq!(rects[1], ratatui::layout::Rect::new(50, 0, 50, 20));
    assert_eq!(rects[2], ratatui::layout::Rect::new(50, 20, 50, 20));
    // The newest (focused) group is the lower-right leaf.
    assert_eq!(app.editor_layout.active_dfs_index(), 2);
}

/// In a grid of three or more groups the render path bakes the ACTIVE group
/// into overlay slot 0 (`update_editor_image_overlay(0, active_area)`), so
/// slot 0's image gate, layout key and baked pixels must all resolve to the
/// active group. They used to resolve to the depth-first-0 group instead:
/// a focused PDF in the third group either got no preview at all (when the
/// dfs-0 group held a text file) or was keyed under the dfs-0 group's path
/// and froze on its first baked page.
#[test]
fn slot_zero_of_a_grid_resolves_to_the_active_group() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.split_editor_dir(editor_layout::SplitDir::Horizontal, true);
    app.split_editor_dir(editor_layout::SplitDir::Vertical, true);
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "B").unwrap();
    app.editor.open_pinned(&b).unwrap();
    assert_eq!(app.editor_layout.active_dfs_index(), 2);
    assert_eq!(
        app.group_on_side(0).and_then(|g| g.path.as_deref()),
        Some(b.as_path()),
        "slot 0 must be the active group in a 3+ grid"
    );
    assert!(
        app.group_on_side(1).is_none(),
        "slot 1 is dark in a 3+ grid; nothing may bake into it"
    );
}

#[test]
fn move_right_with_no_neighbor_creates_a_new_group_with_the_moved_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "A");
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "B").unwrap();
    app.editor.open_pinned(&b).unwrap(); // active group: a.txt + b.txt, active = b
    assert_eq!(app.editor.tab_count(), 2);
    app.move_active_editor(editor_layout::Dir::Right);
    assert_eq!(
        app.editor_layout.leaf_count(),
        2,
        "no neighbour to the right → a new group is created for the moved tab"
    );
    assert_eq!(
        app.editor.path.as_deref(),
        Some(b.as_path()),
        "the moved tab is active in the new group"
    );
    let groups = app.editor_layout.inactive_groups();
    assert_eq!(groups.len(), 1);
    assert!(
        groups[0]
            .iter_tabs()
            .all(|e| e.path.as_deref() != Some(b.as_path())),
        "the source group no longer holds the moved tab"
    );
}

#[test]
fn move_left_into_an_existing_neighbor_relocates_and_collapses_the_emptied_source() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "A");
    let a = tmp.path().join("a.txt");
    app.split_editor_dir(editor_layout::SplitDir::Horizontal, true); // right group active, dup of a
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "B").unwrap();
    app.editor.open_preview(&b).unwrap(); // right group now shows b (single tab)
    assert_eq!(app.editor_layout.leaf_count(), 2);
    app.move_active_editor(editor_layout::Dir::Left); // move b into the left (a) group
    assert_eq!(
        app.editor_layout.leaf_count(),
        1,
        "the emptied right group collapses away"
    );
    let paths: Vec<_> = app
        .editor
        .iter_tabs()
        .filter_map(|e| e.path.clone())
        .collect();
    assert!(
        paths.iter().any(|p| p == &a) && paths.iter().any(|p| p == &b),
        "both a.txt and the moved b.txt now live in the surviving group: {paths:?}"
    );
}

#[test]
fn split_in_group_opens_a_second_view_of_the_active_file_beside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "A");
    let a = tmp.path().join("a.txt");
    app.split_in_group();
    assert_eq!(app.editor_layout.leaf_count(), 2, "a second group appears");
    assert_eq!(app.editor.path.as_deref(), Some(a.as_path()));
    let groups = app.editor_layout.inactive_groups();
    assert_eq!(
        groups[0].path.as_deref(),
        Some(a.as_path()),
        "both groups show the same file"
    );
}

#[test]
fn split_up_makes_a_vertical_split_with_the_new_group_active_above() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hi");
    app.split_editor_dir(editor_layout::SplitDir::Vertical, false);
    assert_eq!(
        app.editor_layout.root_split_dir(),
        Some(editor_layout::SplitDir::Vertical)
    );
    assert_eq!(
        app.editor_layout.active_dfs_index(),
        0,
        "Split Up puts the new focused group in the UPPER leaf"
    );
}

#[test]
fn split_left_makes_a_horizontal_split_with_the_new_group_active_left() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hi");
    app.split_editor_dir(editor_layout::SplitDir::Horizontal, false);
    assert_eq!(
        app.editor_layout.root_split_dir(),
        Some(editor_layout::SplitDir::Horizontal)
    );
    assert_eq!(
        app.editor_layout.active_dfs_index(),
        0,
        "Split Left puts the new focused group in the LEFT leaf"
    );
}

#[test]
fn split_down_lays_the_groups_out_stacked_with_a_horizontal_seam() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hi");
    app.split_editor_dir(editor_layout::SplitDir::Vertical, true);
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let rects = app.editor_layout.leaf_rects(area, 10);
    assert_eq!(rects.len(), 2);
    assert_eq!(
        rects[0].x, rects[1].x,
        "stacked groups share the same column"
    );
    assert!(
        rects[0].y < rects[1].y,
        "the two groups stack top over bottom"
    );
    assert_eq!(
        rects[0].height + rects[1].height,
        area.height,
        "they tile the full height with the seam carrying no dedicated row"
    );
}

#[test]
fn split_editor_refuses_a_blank_unsaved_buffer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.split_editor();
    assert!(
        !app.editor_layout.is_split(),
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
    let groups = app.editor_layout.inactive_groups();
    let left = groups[0];
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
    assert_eq!(app.editor_layout.active_dfs_index(), 1);

    // Move focus to the LEFT group (the original).
    app.focus_editor_group(true);
    assert_eq!(
        app.editor_layout.active_dfs_index(),
        0,
        "focusing left makes the focused group the left column"
    );
    assert_eq!(
        app.editor.path.as_deref(),
        Some(original.as_path()),
        "after the swap `editor` is the original left group"
    );
    assert!(
        app.editor.focused && !app.editor_layout.inactive_groups()[0].focused,
        "only the focused group's border lights up"
    );

    // Move focus back to the RIGHT group.
    app.focus_editor_group(false);
    assert_eq!(app.editor_layout.active_dfs_index(), 1);
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
    assert!(app.editor_layout.is_split());

    // Cmd+W closes the focused group's only tab -> the group empties and
    // the split collapses back to the surviving column.
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();

    assert!(
        !app.editor_layout.is_split(),
        "emptying the focused group collapses the split"
    );
    assert_eq!(
        app.editor_layout.active_dfs_index(),
        0,
        "the collapsed state resets to the single-column default"
    );
    assert!(app.editor_seams.is_empty());
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
fn clicking_the_output_tab_switches_the_panel_view() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let r = app.output_tab_rect;
    assert!(r.width > 0, "the OUTPUT tab must have a hit rect");
    left_click(&mut app, r.x, r.y);
    assert_eq!(
        app.bottom_panel_tab,
        BottomPanelTab::Output,
        "a click on the OUTPUT tab activates that view",
    );
}

#[test]
fn tab_strip_renders_disambiguated_titles_for_colliding_names() {
    // #167 end-to-end: two same-named files from different folders must
    // paint distinguishable tab titles in the real strip.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("alpha");
    let b = tmp.path().join("beta");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::write(a.join("main.rs"), "a\n").unwrap();
    std::fs::write(b.join("main.rs"), "b\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // The QUICK OPEN path (preview tab, pinned by an edit, then a second
    // preview) — the live gesture, which must disambiguate exactly like
    // two pinned opens.
    app.editor.open_preview(&a.join("main.rs")).unwrap();
    app.editor.insert_char('x');
    app.editor.open_preview(&b.join("main.rs")).unwrap();
    assert_eq!(app.editor.iter_tabs().count(), 2, "two tabs");

    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let buf = term.backend().buffer().clone();
    let strip_y = app.editor.tab_strip_y_for_test();
    let row: String = (0..buf.area.width)
        .map(|x| buf[(x, strip_y)].symbol().to_string())
        .collect();
    assert!(
        row.contains("main.rs — alpha") && row.contains("main.rs — beta"),
        "the strip paints the disambiguated titles: {row}"
    );
}

/// A stale tab from an earlier dump can live in ANY editor group, not only
/// the focused one, and every copy must go before the file is rewritten:
/// a tab left in another group would read the new bytes through its old
/// index and paint garbage, with nothing to heal it.
#[test]
fn a_second_dump_closes_the_stale_tab_in_every_editor_group() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.scrollback_dir = tmp.path().join("dumps");
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[32m${s}GRP\\033[0m\\n\"; read line; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the coloured row",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("QQGRP"))
        },
    );
    app.open_scrollback_in_editor();
    let path = app.editor.path.clone().expect("the first dump has a path");
    // Split: the dump tab now sits in the left group; the right is focused.
    app.split_editor();
    app.focus_pane(Pane::Terminal);
    app.open_scrollback_in_editor();
    let stale_elsewhere = app
        .editor_layout
        .inactive_groups()
        .iter()
        .any(|g| g.find_tab_with_path(&path).is_some());
    assert!(
        !stale_elsewhere,
        "no other group keeps a tab over the rewritten dump file"
    );
    assert!(
        app.editor.find_tab_with_path(&path).is_some(),
        "the focused group holds the rebuilt dump"
    );
    // The left group held nothing but the stale dump: closing its only tab
    // leaves a blank group, which every other close path prunes; a dead
    // blank pane the user never opened must not survive here either.
    assert!(
        app.editor_layout
            .inactive_groups()
            .iter()
            .all(|g| !g.is_blank_initial()),
        "an inactive group emptied by the close is pruned, not left blank"
    );
}

#[test]
fn a_tab_switch_disarms_the_background_tabs_on_type_trigger() {
    // A keystroke and a tab switch can drain in the same input burst, so
    // the tick can run with a different tab active than the one that
    // typed. The record on the now-background tab must be swept, or
    // returning to it minutes later would fire a request for a long-dead
    // keystroke — its per-tab edit_seq still matches, nothing having been
    // edited in between.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "fn a() {\n}\n").unwrap();
    std::fs::write(tmp.path().join("b.rs"), "fn b() {\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("a.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char(';'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.editor.last_typed.is_some(),
        "keystroke recorded on tab A"
    );
    // Switch to tab B before any tick runs, then tick with B active.
    app.editor.open_pinned(&tmp.path().join("b.rs")).unwrap();
    app.tick_on_type_formatting();
    // Return to tab A: the armed record must be gone.
    app.editor.open_pinned(&tmp.path().join("a.rs")).unwrap();
    assert!(
        app.editor.last_typed.is_none(),
        "the sweep disarms background tabs — returning to one must not \
         replay the pre-switch keystroke"
    );

    // Same hazard one level out: the typing tab is in a split group that
    // loses focus before the tick runs.
    app.handle_key(key(KeyCode::Char(';'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.last_typed.is_some(), "keystroke recorded again");
    app.split_editor_dir(editor_layout::SplitDir::Horizontal, true);
    assert_eq!(app.editor_layout.leaf_count(), 2, "really split");
    app.tick_on_type_formatting();
    let armed = app
        .editor_layout
        .inactive_groups_mut()
        .iter()
        .any(|g| g.editors.iter().any(|e| e.last_typed.is_some()));
    assert!(
        !armed,
        "the sweep reaches editors in inactive split groups too"
    );
}

/// #369: a file and its symbol tab count edits apart, but one of them
/// speaks for the file to the language server: the active tab. Judged
/// each against the one record, they took turns resending the whole text
/// on every tick.
#[test]
fn one_tab_speaks_for_a_file_to_the_language_server() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("two.rs");
    std::fs::write(&file, "fn a() {\n    1\n}\nfn b() {\n    2\n}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 5;
    for _ in 0..3 {
        app.editor.insert_char('1');
    }
    app.editor.cursor_row = 4;
    app.run_command(crate::widgets::command_palette::Command::OpenAsSymbolTab);
    app.sync_symbol_views();
    assert!(app.editor.symbol_view.is_some(), "the symbol tab is active");
    let path = app.editor.path.clone().unwrap();
    assert_ne!(
        app.editor.editors[0].edit_seq, app.editor.edit_seq,
        "the tabs' counters differ"
    );
    let mut seen = Vec::new();
    for _ in 0..3 {
        app.sync_lsp();
        seen.push(app.lsp_last_seen.get(&path).copied());
    }
    let active = Some(app.editor.edit_seq);
    assert_eq!(seen, vec![active, active, active], "no tick resends");
}

/// With the file open as well as its diff, the rename edits the file's tab
/// (unsaved, one undo step), whichever of the two was opened first, and
/// leaves the disk to the save.
#[test]
fn a_rename_edits_the_files_tab_and_not_its_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let main = tmp.path().join("main.ts");
    std::fs::write(&main, "greet(1);\ngreet(2);\n").unwrap();
    app.editor
        .open_head_diff_with_text(PathBuf::from("main.ts (HEAD)"), "greet(1);\n", &main, true)
        .unwrap();
    app.editor.open_pinned(&main).unwrap();
    let file_tab = app.editor.active_index();
    assert_eq!(app.editor.editors.len(), 2);

    let (_, occurrences) = app.apply_rename_edits(&greet_to_welcome(&main)).unwrap();

    assert_eq!(occurrences, 2);
    let tab = &app.editor.editors[file_tab];
    assert!(tab.diff.is_none());
    assert_eq!(tab.lines[..2], ["welcome(1);", "welcome(2);"]);
    assert!(tab.dirty);
    assert_eq!(
        std::fs::read_to_string(&main).unwrap(),
        "greet(1);\ngreet(2);\n",
        "an open tab's rename waits for the save"
    );
}

/// A split whose group shows only the file's diff while another group holds
/// the file itself, unsaved: the rename edits that group's tab rather than
/// rewriting the disk under it, and the stale check watches the same tab.
#[test]
fn a_rename_reaches_the_files_tab_in_another_split_group() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let main = tmp.path().join("main.ts");
    std::fs::write(&main, "greet(1);\ngreet(2);\n// todo\n").unwrap();
    app.editor.open_pinned(&main).unwrap();
    app.editor.cursor_row = 2;
    app.editor.cursor_col = 0;
    app.editor.insert_str("x");
    assert!(app.editor.dirty);
    app.split_editor_dir(editor_layout::SplitDir::Horizontal, true);
    app.editor
        .open_head_diff_with_text(PathBuf::from("main.ts (HEAD)"), "greet(1);\n", &main, true)
        .unwrap();
    assert_eq!(
        app.editor.find_buffer_tab(&main),
        None,
        "the diff took the split's preview slot: only the diff here"
    );
    let edits = greet_to_welcome(&main);
    let baseline = app.edit_seqs_in_every_group();
    assert_eq!(app.stale_edit_target(&baseline, &edits), None);

    let (files, occurrences) = app.apply_rename_edits(&edits).unwrap();

    assert_eq!((files, occurrences), (1, 2));
    assert_eq!(
        std::fs::read_to_string(&main).unwrap(),
        "greet(1);\ngreet(2);\n// todo\n",
        "the open tab takes the rename; the disk waits for its save"
    );
    let other = app.editor_layout.inactive_groups()[0];
    let tab = &other.editors[other.find_buffer_tab(&main).unwrap()];
    assert_eq!(tab.lines[..3], ["welcome(1);", "welcome(2);", "x// todo"]);
    assert!(tab.dirty);
    assert!(
        app.stale_edit_target(&baseline, &edits).is_some(),
        "a later edit to that tab is seen from the other group"
    );
}

/// #915: a file held unsaved only in the other split group stays open on
/// the language server with its unsaved text, so a rename computed from that
/// text lands on the buffer's own lines, beside the unsaved edit, as one
/// undo step there, and the disk keeps the saved file.
#[test]
fn a_rename_keeps_the_unsaved_edits_of_a_file_open_in_the_other_split_group() {
    const SAVED: &str = "import json\n\n\ndef load_items(path):\n    return json.load(path)\n";
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let store = root.join("store.py");
    std::fs::write(&store, SAVED).unwrap();
    let mut app = app_with_open_file(
        &root,
        "app.py",
        "from store import load_items\n\nload_items(\"a\")\n",
    );
    let main_py = app.editor.path.clone().unwrap();
    app.split_editor_dir(editor_layout::SplitDir::Horizontal, true);
    app.editor.open(&store).unwrap();
    for c in "import os".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let unsaved = format!("import os\n{SAVED}");
    assert_eq!(app.editor.document_text(), unsaved);
    app.sync_lsp();
    app.focus_editor_group(true);
    app.sync_lsp();

    assert_eq!(app.editor.path.as_deref(), Some(main_py.as_path()));
    let seq = app.editor_layout.inactive_groups()[0].edit_seq;
    assert_eq!(
        app.lsp_last_seen.get(&store),
        Some(&seq),
        "the other group's file stays open on the server"
    );
    let held = app.lsp.as_ref().unwrap().synced_text_for_test(&store);
    assert_eq!(
        held.as_deref(),
        Some(unsaved.as_str()),
        "with its unsaved text"
    );

    let span = |row, col| crate::widgets::editor::TextSpanEdit {
        start: (row, col),
        end: (row, col + 10),
        new_text: String::from("read_items"),
        utf16: false,
    };
    app.rename_baseline = app.edit_seqs_in_every_group();
    app.land_rename(Some(vec![
        (main_py.clone(), vec![span(0, 18), span(2, 0)]),
        (store.clone(), vec![span(4, 4)]),
    ]));

    assert_eq!(app.status, "Renamed 3 occurrence(s) across 2 file(s)");
    assert_eq!(app.editor.lines[0], "from store import read_items");
    let other = app.editor_layout.inactive_groups()[0];
    assert_eq!(
        other.document_text(),
        unsaved.replace("load_items", "read_items"),
        "the rename and the unsaved line, both"
    );
    assert!(other.dirty);
    assert_eq!(std::fs::read_to_string(&store).unwrap(), SAVED);

    app.focus_editor_group(false);
    app.handle_key(key(KeyCode::Char('z'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(
        app.editor.document_text(),
        unsaved,
        "undo takes back the rename alone"
    );
    assert_eq!(std::fs::read_to_string(&store).unwrap(), SAVED);
}

/// #369: saving the file's tab leaves no disk conflict on its symbol tab.
///
/// The symbol tab holds the same text that was just written, so the next
/// external-change check must see it as in sync with disk instead of
/// offering to reload over the saved edit.
#[test]
fn saving_the_files_tab_leaves_no_conflict_on_its_symbol_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('7'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.editors[1].dirty);

    app.editor.editors[0].save_to_disk().unwrap();
    app.sync_symbol_views();
    assert!(
        !app.reload_open_file_after_external_change(),
        "the symbol tab already holds the saved text"
    );
    assert!(app.input_prompt.is_none(), "no reload prompt");
    assert_eq!(app.editor.editors[1].lines[4], "    27");
}

/// #369: two symbol tabs of one file with no file tab restore as ONE tab.
#[test]
fn session_capture_keeps_one_tab_for_orphaned_symbol_tabs_of_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.editor.close_tab(0);
    app.open_symbol_tab_for("a".into(), 0, 2);
    assert_eq!(app.editor.editors.len(), 2);
    let state = app.capture_session_state();
    assert_eq!(state.tabs.len(), 1);
    assert_eq!(state.tabs[0].path.as_ref(), Some(&file));
}

/// A move re-points the file's tab in the OTHER split too, not only in the
/// focused group.
#[test]
fn moving_a_file_repoints_its_tab_in_every_split() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("dest")).unwrap();
    std::fs::write(root.join("a.txt"), "a").unwrap();
    std::fs::write(root.join("c.txt"), "c").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open_pinned(&root.join("a.txt")).unwrap();
    app.focus_pane(Pane::Editor);
    app.split_editor();
    // The focused (right) group moves on to another file; a.txt stays open
    // in the left one.
    app.editor.open_pinned(&root.join("c.txt")).unwrap();
    app.editor.close_tab(0);
    assert!(app.apply_paste_or_drop(
        &root.join("dest"),
        &[root.join("a.txt")],
        ExplorerClipMode::Cut
    ));
    let groups = app.editor_layout.inactive_groups_mut();
    let paths: Vec<_> = groups[0]
        .editors
        .iter()
        .filter_map(|e| e.path.clone())
        .collect();
    assert!(paths.contains(&root.join("dest/a.txt")), "{paths:?}");
}

// ---- Markdown preview scroll sync across a split (#619) ----

#[test]
fn a_split_preview_follows_the_source_pane_scroll() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("doc.md");
    let mut body = String::from("# Top\n\n");
    for i in 0..30 {
        body.push_str(&format!("para {i}\n\n"));
    }
    std::fs::write(&f, &body).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus_pane(Pane::Editor);
    assert!(
        app.editor.toggle_markdown_preview(),
        "left pane shows the preview"
    );
    app.split_editor();
    assert!(
        app.editor.markdown_preview.is_none(),
        "the new pane shows the source"
    );
    app.editor.scroll = 20;
    assert!(
        app.sync_markdown_scroll(),
        "the preview pane is told to follow"
    );
    let preview_pane = app
        .editor_layout
        .inactive_groups()
        .into_iter()
        .find_map(|g| g.editors.get(g.active_index()))
        .and_then(|t| t.markdown_preview.as_ref())
        .expect("the other pane still shows the preview");
    assert_eq!(preview_pane.scroll_to_source, Some(20));
    assert!(
        !app.sync_markdown_scroll(),
        "a still viewport costs nothing"
    );
}

/// #852 regression, driven by keys alone: `Ctrl+Alt+S` writes every dirty
/// tab to disk, the inactive one included, not only the active tab.
#[test]
fn ctrl_alt_s_writes_the_inactive_dirty_tab_too() {
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
    app.handle_key(key(
        KeyCode::Char('s'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert!(
        std::fs::read_to_string(&a).unwrap().contains('X'),
        "the inactive tab a.txt must reach disk"
    );
    assert!(std::fs::read_to_string(&b).unwrap().contains('Y'));
    assert!(app.editor.editors.iter().all(|e| !e.dirty));
}

/// #852 negative: plain `Ctrl+S` is still File: Save, not Save All; the
/// inactive dirty tab stays unsaved on disk.
#[test]
fn plain_ctrl_s_still_saves_only_the_active_tab() {
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
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(std::fs::read_to_string(&b).unwrap().contains('Y'));
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "aaa");
    assert_eq!(app.unsaved_count(), 1);
}

/// #852 negative: a collapsed OPEN EDITORS hides its Save All action, as
/// VS Code hides a collapsed view's actions, and a click on the header
/// title still just toggles the section and saves nothing.
#[test]
fn open_editors_save_all_hides_while_collapsed_and_the_title_click_still_toggles() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    std::fs::write(&a, "aaa").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.auto_save = false;
    app.auto_save_on_focus_change = false;
    app.format_on_save = false;
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('X'), KeyModifiers::NONE))
        .unwrap();
    app.explorer_views = Default::default();
    app.explorer_views.toggle(ExplorerView::OpenEditors);
    app.open_editors.collapsed = true;
    app.sidebar_view = SidebarView::Explorer;
    app.show_tree = true;
    let buf = render_buffer_852(&mut app, 120, 40);
    let hdr = app.open_editors.last_area;
    assert!(hdr.height > 0, "OPEN EDITORS is on screen");
    assert!(
        !(hdr.x..hdr.x + hdr.width).any(|x| buf[(x, hdr.y)].symbol() == "\u{eb49}"),
        "a collapsed section paints no Save All"
    );
    assert_eq!(app.open_editors.save_all_btn, Rect::default());
    // The rightmost header cells, where the button sits when expanded.
    left_click(&mut app, hdr.x + hdr.width - 2, hdr.y);
    assert!(!app.open_editors.collapsed, "the header click expands it");
    let _ = render_buffer_852(&mut app, 120, 40);
    left_click(&mut app, hdr.x + 3, hdr.y);
    assert!(app.open_editors.collapsed, "the title click collapses it");
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "aaa",
        "no header click saved anything"
    );
    assert!(app.editor.dirty);
}

/// #852 (comment 3, its GIF): the tab menu's words work in the palette too.
/// "keep open" found no command at all; it now runs View: Keep Editor.
#[test]
fn typing_the_tab_menus_keep_open_in_the_palette_keeps_the_preview_tab() {
    let (mut app, tmp) = tabs_app_852(&["a.rs"]);
    let p = tmp.path().join("p.rs");
    std::fs::write(&p, "x\n").unwrap();
    app.editor.open_preview(&p).unwrap();
    let ran = run_in_palette_852(&mut app, "keep open");
    assert_eq!(ran.as_deref(), Some("View: Keep Editor"));
    assert!(!app.editor.preview, "the preview tab is kept");
}

/// #852 (comment 3) guard: each close / pin / keep row of the tab menu
/// names the palette command that does its job on the active tab, and the
/// command leaves every group's tabs, the status line and the reopen stack
/// exactly as a click on the row does, whether the tab is plain, pinned or
/// a preview, beside an unsaved tab and another split group.
#[test]
fn every_tab_menu_close_pin_and_keep_row_has_a_palette_command_that_does_the_same() {
    use crate::widgets::command_palette::Command;
    fn command_id(label: &str, action: &MenuAction) -> Option<&'static str> {
        match action {
            MenuAction::CloseOtherTabs(_) => Some("close_other_editors"),
            MenuAction::CloseTabsToRight(_) => Some("close_editors_to_the_right"),
            MenuAction::CloseSavedTabs => Some("close_saved_editors"),
            MenuAction::CloseAllTabs => Some("close_all_editors"),
            MenuAction::KeepTabOpen(_) => Some("keep_editor"),
            MenuAction::ToggleTabPin(_) if label == "Pin" => Some("pin_editor"),
            MenuAction::ToggleTabPin(_) => Some("unpin_editor"),
            _ => None,
        }
    }
    let mut covered: Vec<&str> = Vec::new();
    for active in ["c.rs", "b.rs", "p.rs"] {
        let (app, _tmp) = split_tabs_app_852(active);
        let idx = app.editor.active_index();
        let path = app.editor.tab_path(idx);
        let items = build_tab_context_menu_items(
            idx,
            app.editor.tab_count(),
            app.editor_layout.is_split(),
            path.as_deref(),
            false,
            app.editor.is_preview(idx),
            app.editor.is_pinned(idx),
        );
        for (label, action) in items.into_iter().filter_map(|e| match e {
            MenuEntry::Item { label, action } => Some((label, action)),
            _ => None,
        }) {
            let Some(id) = command_id(&label, &action) else {
                continue;
            };
            let cmd = Command::from_id(id).unwrap_or_else(|| {
                panic!("the tab menu's {label:?} (on {active}) has no palette command {id:?}")
            });
            let (mut by_click, tmp_click) = split_tabs_app_852(active);
            let (mut by_palette, _tmp_palette) = split_tabs_app_852(active);
            by_click.dispatch_menu_action(action.clone(), tmp_click.path().to_path_buf());
            by_palette.run_command(cmd);
            assert_eq!(
                tabs_snapshot_852(&by_palette),
                tabs_snapshot_852(&by_click),
                "{cmd:?} on {active} must do what the tab menu's {label:?} does"
            );
            covered.push(id);
        }
    }
    for id in [
        "close_other_editors",
        "close_editors_to_the_right",
        "close_saved_editors",
        "close_all_editors",
        "pin_editor",
        "unpin_editor",
        "keep_editor",
    ] {
        assert!(
            covered.contains(&id),
            "{id} was never compared: {covered:?}"
        );
    }
}

/// #852 (comment 3) negative: with no file open, Pin, Unpin and Keep only
/// say so. The welcome screen's placeholder is not pinned (Cmd+K P used to
/// pin it) and Close Others / Close to the Right have nothing to close.
#[test]
fn tab_commands_with_no_editor_open_only_say_so() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap =
        crate::keymap::Keymap::from_json(r#"[{ "key": "ctrl+alt+u", "command": "unpin_editor" }]"#);
    assert!(app.editor.is_blank_initial(), "fixture: the welcome screen");
    for (title, status) in [
        ("View: Pin Editor", "No editor is open"),
        ("View: Keep Editor", "No editor is open"),
        (
            "View: Close Other Editors in Group",
            "No other tabs to close",
        ),
        (
            "View: Close Editors to the Right in Group",
            "No tabs to the right to close",
        ),
    ] {
        let ran = run_in_palette_852(&mut app, title);
        assert_eq!(ran.as_deref(), Some(title));
        assert_eq!(app.status, status, "{title}");
        assert!(app.editor.is_blank_initial(), "{title}");
        assert!(!app.editor.pinned, "{title}");
    }
    app.handle_key(key(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_eq!(app.status, "No editor is open", "a key bound to Unpin");
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE))
        .unwrap();
    assert!(!app.editor.pinned, "Cmd+K P pins no placeholder");
    assert_eq!(app.status, "No editor is open");
}

/// #852 (comment 3) negative: Pin Editor and Unpin Editor are not toggles.
/// Bound to their own keys, each leaves a tab already in its state alone,
/// as VS Code's `when` clauses do; Keep Editor on a kept tab changes nothing.
#[test]
fn pin_editor_never_unpins_and_unpin_editor_never_pins() {
    let (mut app, _tmp) = tabs_app_852(&["a.rs", "b.rs", "c.rs"]);
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[
            { "key": "ctrl+alt+p", "command": "pin_editor" },
            { "key": "ctrl+alt+u", "command": "unpin_editor" },
            { "key": "ctrl+alt+k", "command": "keep_editor" }
        ]"#,
    );
    let press = |app: &mut App, c: char| {
        app.handle_key(key(
            KeyCode::Char(c),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ))
        .unwrap();
    };
    press(&mut app, 'u');
    assert!(!app.editor.pinned, "Unpin does not pin");
    assert_eq!(tab_names_852(&app.editor), ["a.rs", "b.rs", "c.rs"]);
    assert_eq!(app.status, "Tab is not pinned");
    press(&mut app, 'p');
    assert!(app.editor.pinned);
    press(&mut app, 'p');
    assert!(app.editor.pinned, "a second Pin does not unpin");
    assert_eq!(tab_names_852(&app.editor), ["c.rs", "a.rs", "b.rs"]);
    assert_eq!(app.status, "Tab is already pinned");
    press(&mut app, 'k');
    assert!(
        !app.editor.preview && app.editor.pinned,
        "Keep changes nothing"
    );
    assert_eq!(tab_names_852(&app.editor), ["c.rs", "a.rs", "b.rs"]);
    assert_eq!(app.status, "Tab is already kept open");
}

/// #1568: closing a tab threw its undo history away, so after Reopen Closed
/// Editor Ctrl+Z said "Nothing to undo". The history now comes back with
/// the file, and undoing past the save leaves the buffer modified.
#[test]
fn reopen_closed_editor_brings_back_the_undo_history() {
    let (_tmp, _a, mut app) = app_with_a_saved_edit();
    app.run_command(crate::widgets::command_palette::Command::CloseEditor);
    assert!(app.editor.path.is_none(), "the tab closed");
    app.run_command(crate::widgets::command_palette::Command::ReopenClosedEditor);
    assert_eq!(app.editor.lines[0], "value = 100");
    app.handle_key(ctrl('z')).unwrap();
    assert_eq!(app.status, "Undo");
    assert_eq!(app.editor.lines[0], "value = 1");
    assert!(app.editor.dirty, "the undone text is not what is on disk");
    app.handle_key(key(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.editor.lines[0], "value = 100", "and redo still works");
}

/// #1568 negative: a file changed on disk after its tab closed opens with
/// no history, which describes text the file no longer has.
#[test]
fn a_file_changed_after_its_tab_closed_reopens_with_no_history() {
    let (_tmp, a, mut app) = app_with_a_saved_edit();
    app.run_command(crate::widgets::command_palette::Command::CloseEditor);
    std::fs::write(&a, "value = 2\n").unwrap();
    app.run_command(crate::widgets::command_palette::Command::ReopenClosedEditor);
    assert_eq!(app.editor.lines[0], "value = 2");
    app.handle_key(ctrl('z')).unwrap();
    assert_eq!(app.status, "Nothing to undo");
    assert_eq!(app.editor.lines[0], "value = 2");
}

/// #1568: a tab moving on to another file (a reused preview slot, or any
/// open into the same tab) keeps the history of the file it leaves, which
/// comes back when that file opens there again.
#[test]
fn a_tab_keeps_the_history_of_the_file_it_moves_off() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.py");
    let b = tmp.path().join("b.py");
    std::fs::write(&a, "value = 1\n").unwrap();
    std::fs::write(&b, "other\n").unwrap();
    let mut e = crate::widgets::editor::Editor::new();
    e.open(&a).unwrap();
    e.end_line();
    e.insert_str("00");
    e.save_to_disk().unwrap();
    e.open(&b).unwrap();
    assert!(!e.undo(), "b.py has no history of its own");
    e.open(&a).unwrap();
    assert!(e.undo());
    assert_eq!(e.lines[0], "value = 1");
}

/// #1568: the kept histories are capped; past the cap the oldest closed
/// file's goes first and the newest stay.
#[test]
fn closed_tab_histories_are_capped_oldest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let files: Vec<_> = (0..31)
        .map(|i| {
            let f = tmp.path().join(format!("f{i}.txt"));
            std::fs::write(&f, "x\n").unwrap();
            f
        })
        .collect();
    for f in &files {
        let mut e = crate::widgets::editor::Editor::new();
        e.open(f).unwrap();
        e.insert_str("y");
        e.save_to_disk().unwrap();
    }
    let undoes = |f: &std::path::Path| {
        let mut e = crate::widgets::editor::Editor::new();
        e.open(f).unwrap();
        e.undo()
    };
    assert!(!undoes(&files[0]), "the oldest of 31 is dropped");
    assert!(undoes(&files[1]));
    assert!(undoes(&files[30]));
}

fn active_name_1565(app: &App) -> String {
    tab_names_852(&app.editor)[app.editor.active_index()].clone()
}

#[test]
fn ctrl_shift_page_up_and_down_move_the_active_tab_along_the_bar() {
    let (mut app, _tmp) = three_tab_app_1565();
    let up = key(KeyCode::PageUp, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    let down = key(
        KeyCode::PageDown,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    app.handle_key(up).unwrap();
    assert_eq!(tab_names_852(&app.editor), ["a.py", "c.py", "b.py"]);
    assert_eq!(active_name_1565(&app), "c.py", "the moved tab stays active");
    app.handle_key(up).unwrap();
    assert_eq!(tab_names_852(&app.editor), ["c.py", "a.py", "b.py"]);
    assert!(
        app.editor.selection.is_none(),
        "the chord moves the tab, not a Shift+PageUp page selection"
    );
    app.handle_key(down).unwrap();
    assert_eq!(tab_names_852(&app.editor), ["a.py", "c.py", "b.py"]);
    assert_eq!(active_name_1565(&app), "c.py");
}

#[test]
fn move_editor_left_on_the_first_tab_moves_nothing() {
    let (mut app, _tmp) = three_tab_app_1565();
    app.editor.select(0);
    app.run_command(crate::widgets::command_palette::Command::MoveEditorLeftInGroup);
    assert_eq!(tab_names_852(&app.editor), ["a.py", "b.py", "c.py"]);
    assert_eq!(app.status, "Move: the editor is already the first tab");
    app.editor.select(2);
    app.run_command(crate::widgets::command_palette::Command::MoveEditorRightInGroup);
    assert_eq!(tab_names_852(&app.editor), ["a.py", "b.py", "c.py"]);
    assert_eq!(app.status, "Move: the editor is already the last tab");
}

#[test]
fn move_tab_keeps_the_active_tab_and_pinned_tabs_first() {
    let (mut app, _tmp) = three_tab_app_1565();
    let tabs = &mut app.editor;
    assert_eq!(tabs.move_tab(2, 0), Some(0));
    assert_eq!(tab_names_852(tabs), ["c.py", "a.py", "b.py"]);
    assert_eq!(
        tabs.active_index(),
        0,
        "c.py, the active tab, moved with it"
    );
    tabs.select(1); // a.py
    assert_eq!(tabs.move_tab(2, 0), Some(0));
    assert_eq!(tab_names_852(tabs), ["b.py", "c.py", "a.py"]);
    assert_eq!(
        tabs.active_index(),
        2,
        "another tab moving keeps a.py active"
    );
    assert_eq!(tabs.move_tab(7, 0), None, "out of range moves nothing");
    // Pin a.py: pinned tabs lead, and no move crosses that boundary.
    assert!(tabs.toggle_pin(2));
    assert_eq!(tab_names_852(tabs), ["a.py", "b.py", "c.py"]);
    assert_eq!(
        tabs.move_tab(2, 0),
        Some(1),
        "an unpinned tab stops after the pins"
    );
    assert_eq!(tab_names_852(tabs), ["a.py", "c.py", "b.py"]);
    assert_eq!(
        tabs.move_tab(0, 2),
        Some(0),
        "a pinned tab stays among the pins"
    );
    assert_eq!(tab_names_852(tabs), ["a.py", "c.py", "b.py"]);
}

/// The column of the tab at `idx` on the rendered tab strip.
fn tab_col_1565(app: &App, idx: usize) -> (u16, u16) {
    let y = app.editor.tab_strip_y_for_test();
    let x = (0..200)
        .find(|&x| app.editor.tab_at(x, y) == Some(idx))
        .expect("tab on the strip");
    (x + 1, y)
}

#[test]
fn dragging_a_tab_onto_another_reorders_the_bar() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp) = three_tab_app_1565();
    draw(&mut app, 120, 30);
    let (cx, y) = tab_col_1565(&app, 2);
    let (ax, _) = tab_col_1565(&app, 0);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), cx, y));
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), ax, y));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), ax, y));
    assert_eq!(tab_names_852(&app.editor), ["c.py", "a.py", "b.py"]);
    assert_eq!(active_name_1565(&app), "c.py");
}

#[test]
fn a_click_or_a_drag_off_the_bar_leaves_the_tab_order_alone() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp) = three_tab_app_1565();
    draw(&mut app, 120, 30);
    let (bx, y) = tab_col_1565(&app, 1);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), bx, y));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), bx, y));
    assert_eq!(tab_names_852(&app.editor), ["a.py", "b.py", "c.py"]);
    assert_eq!(
        active_name_1565(&app),
        "b.py",
        "a click still selects the tab"
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), bx, y));
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), bx, y + 6));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), bx, y + 6));
    assert_eq!(tab_names_852(&app.editor), ["a.py", "b.py", "c.py"]);
    // After the release, a drag in the editor body is not a tab drag.
    let (cx, _) = tab_col_1565(&app, 2);
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), cx, y));
    assert_eq!(tab_names_852(&app.editor), ["a.py", "b.py", "c.py"]);
}

#[test]
fn move_editor_into_next_group_splits_then_previous_brings_it_back() {
    let (mut app, tmp) = three_tab_app_1565();
    let c = tmp.path().join("c.py");
    app.run_command(crate::widgets::command_palette::Command::MoveEditorToNextGroup);
    assert_eq!(
        app.editor_layout.leaf_count(),
        2,
        "one group: a new one opens"
    );
    assert_eq!(app.editor_layout.active_dfs_index(), 1, "to the right");
    assert_eq!(app.editor.path.as_deref(), Some(c.as_path()));
    let left = &app.editor_layout.inactive_groups()[0];
    assert_eq!(
        tab_names_852(left),
        ["a.py", "b.py"],
        "the source lost c.py"
    );
    app.run_command(crate::widgets::command_palette::Command::MoveEditorToPreviousGroup);
    assert_eq!(
        app.editor_layout.leaf_count(),
        1,
        "the emptied group closes"
    );
    assert_eq!(tab_names_852(&app.editor), ["a.py", "b.py", "c.py"]);
    assert_eq!(active_name_1565(&app), "c.py");
}
