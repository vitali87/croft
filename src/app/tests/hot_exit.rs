//! Unsaved work: hot exit, backups, untitled buffers, quit prompts and
//! session restore.

use super::*;

/// #1427: a middle-click on a tab with unsaved edits asks first, as Ctrl+W
/// does, and keeps the tab until answered.
#[test]
fn a_middle_click_on_an_unsaved_tab_asks_before_closing() {
    use crossterm::event::MouseButton;
    let (mut app, _tmp) = three_tab_app("[]");
    app.editor.editors[0].lines[0] = String::from("edited");
    app.editor.editors[0].dirty = true;
    let a = tab_cell(&app, 0);
    press(&mut app, MouseButton::Middle, a);
    assert!(app.pending_unsaved.is_some(), "the unsaved prompt is up");
    assert_eq!(tab_names(&app), ["a.py", "b.txt", "c.txt"]);
}

/// #862: a quit with nothing unsaved is immediate, as it always was.
#[test]
fn ctrl_q_with_nothing_unsaved_quits_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "alpha\n");
    press_ctrl_q(&mut app);
    assert!(app.quit);
    assert_eq!(app.pending_unsaved, None, "nothing to ask about");
}

/// #862: Ctrl+Q with an unsaved buffer used to exit on the spot and lose
/// it. It asks now, naming the file, and does not quit until answered.
#[test]
fn ctrl_q_with_an_unsaved_tab_asks_before_quitting() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_unsaved_file(tmp.path());
    press_ctrl_q(&mut app);
    assert!(!app.quit, "an unsaved buffer must not be dropped by Ctrl+Q");
    assert_eq!(app.pending_unsaved, Some(UnsavedExit::Quit));
    let screen = draw_screen(&mut app);
    assert!(screen.contains("UNSAVED CHANGES"), "{screen}");
    assert!(screen.contains("1 file has unsaved changes"), "{screen}");
    assert!(
        screen.contains("a.txt"),
        "the prompt names the file:\n{screen}"
    );
    // The prompt owns the keyboard: typing does not reach the buffer, and a
    // second Ctrl+Q does not force the quit.
    let before = app.editor.lines.clone();
    type_into_editor(&mut app, "zz");
    press_ctrl_q(&mut app);
    assert_eq!(app.editor.lines, before);
    assert!(!app.quit);
}

/// #862: S saves every unsaved buffer, in every split, and then quits.
#[test]
fn s_at_the_quit_prompt_saves_every_split_and_quits() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "bravo\n").unwrap();
    app.split_editor();
    app.editor.open_pinned(&b).unwrap();
    type_into_editor(&mut app, "y");
    assert!(app.editor_layout.is_split(), "setup: two groups");
    press_ctrl_q(&mut app);
    assert!(!app.quit);
    let screen = draw_screen(&mut app);
    assert!(screen.contains("2 files have unsaved changes"), "{screen}");
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.quit, "saved everything, so it quits: {}", app.status);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "xalpha\n");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "ybravo\n");
}

/// #862: D quits without writing anything.
#[test]
fn d_at_the_quit_prompt_quits_without_saving() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    press_ctrl_q(&mut app);
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.quit);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha\n");
}

/// Draw one frame of `app` and return the terminal and where the real
/// cursor was left, which is what a screen reader follows.
fn draw_and_cursor(
    app: &mut App,
) -> (
    ratatui::Terminal<ratatui::backend::TestBackend>,
    ratatui::layout::Position,
) {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let pos = term.get_cursor_position().unwrap();
    (term, pos)
}

/// #1584: Ctrl+W on an edited tab opens the unsaved-changes prompt, and
/// screen reader mode reads its title, the file and what S, D and Escape
/// do, once, with the cursor on that line rather than behind the dialog.
#[test]
fn the_unsaved_changes_prompt_is_announced_with_its_file_and_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_unsaved_file(tmp.path());
    app.screen_reader = true;
    draw_and_cursor(&mut app);
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.pending_unsaved.is_some(), "setup: the prompt opened");
    let (_term, cursor) = draw_and_cursor(&mut app);
    let said = app.announcer.line.clone();
    for part in [
        "Unsaved changes",
        "This tab has unsaved changes: a.txt",
        "S, Save and close",
        "D, Don't save",
        "Escape, cancel",
    ] {
        assert!(said.contains(part), "{part:?} missing from {said:?}");
    }
    let (x, y) = app.announce_pos.expect("the status bar shows the line");
    assert_eq!((cursor.x, cursor.y), (x, y), "the cursor sits on the line");
    // Said once: the next frame, with nothing changed, says nothing new.
    let snap = app.a11y_snapshot();
    assert!(!app.announcer.update(snap, None));
}

/// #1584: the quit prompt names the files and its own verbs.
#[test]
fn the_quit_prompt_is_announced_with_its_files_and_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_unsaved_file(tmp.path());
    press_ctrl_q(&mut app);
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Unsaved changes");
    let item = snap.item.unwrap_or_default();
    for part in [
        "1 file has unsaved changes: a.txt",
        "S, Save all and quit",
        "D, Discard and quit",
    ] {
        assert!(item.contains(part), "{part:?} missing from {item:?}");
    }
}

/// #1584: the Source Control and Replace All confirmations say what they
/// will do and which key does it.
#[test]
fn discard_and_replace_all_confirmations_are_announced() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pending_discard = Some(PendingDiscard {
        rel_path: String::from("src/seed.txt"),
        untracked: false,
        staged: false,
    });
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Discard changes?");
    assert_eq!(
        snap.item.as_deref(),
        Some(
            "This will overwrite your local changes from HEAD. src/seed.txt. \
             Y, yes, discard. N or Escape, no"
        )
    );
    app.pending_discard = None;
    app.request_discard_all_source_control();
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Discard all changes?");
    assert!(
        snap.item
            .as_deref()
            .unwrap_or_default()
            .ends_with("Y, yes, discard all. N or Escape, no"),
        "{:?}",
        snap.item
    );
    app.pending_discard_all = false;
    app.search.replace = String::from("total");
    app.pending_replace_all = Some((3, 2, 1));
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Replace all?");
    // Longer than an announcement holds: the middle is cut, not the keys.
    let item = snap.item.unwrap_or_default();
    assert!(
        item.starts_with(
            "Replace 3 occurrence(s) across 2 file(s) with \"total\"? \
             Files are rewritten on disk. \
             1 open file(s) with unsaved changes"
        ),
        "{item}"
    );
    assert!(
        item.ends_with("… Enter or Y, replace. Escape, cancel"),
        "{item}"
    );
    assert!(item.chars().count() <= crate::a11y::MAX_LINE, "{item}");
}

/// #1584 (comment): the Settings editor reads its target layer and the
/// selected row, and a one-line prompt reads its title and keys; typing
/// into the prompt is left to the reader's own echo, not re-read.
#[test]
fn the_settings_editor_and_an_input_prompt_are_announced() {
    use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_settings_editor();
    let ed = app.settings_editor.as_mut().unwrap();
    ed.set_query(String::from("auto save"));
    let key_name = ed.selected_row().unwrap().key.replace('_', " ");
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Settings, writing to user");
    let item = snap.item.unwrap_or_default();
    assert!(item.starts_with(&format!("{key_name}, ")), "{item}");
    // The layer that set the value closes the line: the default here, or
    // the user layer when the machine running the suite sets it.
    assert!(
        [", default", ", user"].iter().any(|l| item.ends_with(l)),
        "{item}"
    );
    app.settings_editor.as_mut().unwrap().target = crate::config_layers::LayerKind::Workspace;
    assert_eq!(app.a11y_snapshot().focus, "Settings, writing to workspace");
    app.settings_editor = None;
    app.input_prompt = Some(InputPrompt::new(
        InputPurpose::PullRequestNumber,
        "Review Pull Request",
        "number or URL",
    ));
    let before = app.a11y_snapshot();
    assert_eq!(before.focus, "Review Pull Request");
    assert_eq!(
        before.item.as_deref(),
        Some("number or URL. Enter to confirm, Escape to cancel")
    );
    app.input_prompt.as_mut().unwrap().value = String::from("12");
    assert_eq!(app.a11y_snapshot(), before, "typing is not re-read");
}

/// #1584 (comment): Add Remote asks for the name, then replaces that
/// prompt with one for the URL under the same title and keys. The field's
/// instruction tells the two apart, so the second step is announced; the
/// typed name is not part of either.
#[test]
fn a_prompt_replaced_by_the_next_step_under_the_same_title_is_announced() {
    use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.screen_reader = true;
    app.open_input_prompt(InputPrompt::new(
        InputPurpose::AddRemoteName,
        "Add Remote",
        "Remote name (e.g. origin)",
    ));
    draw_and_cursor(&mut app);
    let first = app.announcer.line.clone();
    assert!(first.contains("Remote name (e.g. origin)"), "{first}");
    app.input_prompt.as_mut().unwrap().value = String::from("upstream");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let p = app.input_prompt.as_ref().expect("the URL step opened");
    assert_eq!(p.title, "Add Remote");
    let snap = app.a11y_snapshot();
    assert_eq!(
        snap.item.as_deref(),
        Some("Remote URL. Enter to confirm, Escape to cancel")
    );
    draw_and_cursor(&mut app);
    let second = app.announcer.line.clone();
    assert_ne!(second, first);
    assert!(second.contains("Remote URL"), "{second}");
    assert!(!second.contains("upstream"), "the typed value is not read");
}

/// #1584 (comment): a long command, path or file list is cut so the keys
/// at the end of a confirmation still fit the announcement.
#[test]
fn a_long_confirmation_still_announces_its_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.screen_reader = true;
    app.pending_discard = Some(PendingDiscard {
        rel_path: format!("src/{}/seed.txt", "deeply/nested".repeat(30)),
        untracked: false,
        staged: false,
    });
    draw_and_cursor(&mut app);
    let said = app.announcer.line.clone();
    assert!(said.starts_with("Discard changes?"), "{said}");
    assert!(
        said.contains("… Y, yes, discard. N or Escape, no"),
        "the keys survive: {said}"
    );
    assert!(said.contains('…'), "the path is what was cut: {said}");
    app.pending_discard = None;
    app.search.replace = "x".repeat(400);
    app.pending_replace_all = Some((3, 2, 0));
    let item = app.a11y_snapshot().item.unwrap_or_default();
    assert!(item.chars().count() <= crate::a11y::MAX_LINE, "{item}");
    assert!(
        item.ends_with("Enter or Y, replace. Escape, cancel"),
        "{item}"
    );
}

/// #1584 (comment): an agent's edit proposal takes every key, so screen
/// reader mode reads it, not the editor behind it: who wants to change
/// which file, and the popup's keys in its current state.
#[test]
fn an_incoming_approval_is_announced_with_its_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    std::fs::write(&file, "let x = 1;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let _hook = queue_edit_proposal(&mut app, tmp.path(), &file, "1", "2");
    assert!(app.approval_ui.is_some(), "setup: the popup opened");
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "claude-code wants to edit a.rs");
    assert_eq!(snap.line, None, "not the editor under the popup");
    let item = snap.item.unwrap_or_default();
    for part in ["Enter, approve", "Escape, deny", "R, deny with a reason"] {
        assert!(item.contains(part), "{part:?} missing from {item:?}");
    }
    app.approval_ui.as_mut().unwrap().reason = Some(String::from("no"));
    assert_eq!(
        app.a11y_snapshot().item.as_deref(),
        Some("Type the reason. Enter, send. Escape, back")
    );
}

/// Negative (#1584): with no dialog open the snapshot is the focused
/// pane's as before, and screen reader mode off still says nothing and
/// leaves the caret where it was.
#[test]
fn without_a_dialog_the_editor_is_announced_and_mode_off_stays_quiet() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_unsaved_file(tmp.path());
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Editor a.txt");
    assert_eq!(snap.line.map(|(n, _)| n), Some(1));
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.pending_unsaved.is_some());
    draw_and_cursor(&mut app);
    assert!(app.announcer.line.is_empty(), "mode off announces nothing");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.pending_unsaved.is_none());
    assert_eq!(app.a11y_snapshot().focus, "Editor a.txt");
}

/// #1584 (review): with Cmd+K I's broadcast confirm open behind a
/// run-block confirm, the key answers the run-block one, which is drawn on
/// top and announced; the broadcast question is then announced and answered.
#[test]
fn a_run_block_confirm_over_the_broadcast_one_takes_the_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = markdown_fence_app(tmp.path());
    assert!(run_from_palette(
        &mut app,
        "Markdown: Run Code Block at Cursor"
    ));
    assert!(app.pending_run_block.is_some(), "setup: the block confirms");
    app.pending_broadcast_enable = true;
    assert_eq!(app.a11y_snapshot().focus, "Run this block?");
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.pending_run_block.is_none(), "N answered the block");
    assert!(app.pending_broadcast_enable, "the broadcast question stays");
    assert_eq!(app.a11y_snapshot().focus, "Broadcast input to all panes?");
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE))
        .unwrap();
    assert!(!app.pending_broadcast_enable);
    assert!(!app.broadcast_input);
}

/// #862: Esc (or N, which never means "don't save") cancels: croft stays,
/// the edits stay, and the disk is untouched. A modified D does not discard.
#[test]
fn esc_or_n_at_the_quit_prompt_cancels() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    for cancel in [
        key(KeyCode::Esc, KeyModifiers::NONE),
        key(KeyCode::Char('n'), KeyModifiers::NONE),
    ] {
        press_ctrl_q(&mut app);
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(!app.quit, "Ctrl+D is not the D answer");
        app.handle_key(cancel).unwrap();
        assert!(!app.quit);
        assert_eq!(app.pending_unsaved, None, "the prompt closed");
        assert!(app.editor.dirty, "the edits are still there");
        assert_eq!(app.editor.lines[0], "xalpha");
    }
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha\n");
}

/// #862: an untitled buffer that holds nothing is not worth a prompt.
#[test]
fn an_emptied_untitled_buffer_does_not_hold_up_a_quit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    type_into_editor(&mut app, "x");
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, vec![String::new()], "setup");
    press_ctrl_q(&mut app);
    assert!(app.quit);
}

/// #862: Cmd+W on an unsaved tab used to close it, and Reopen Closed
/// Editor brought back only the file on disk. It asks now; Esc keeps it.
#[test]
fn closing_an_unsaved_tab_asks_rather_than_dropping_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_unsaved_file(tmp.path());
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "bravo\n").unwrap();
    app.editor.open_pinned(&b).unwrap();
    app.editor.select(0);
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.tab_count(), 2, "nothing closed yet");
    assert!(matches!(
        app.pending_unsaved,
        Some(UnsavedExit::CloseTab { idx: 0, .. })
    ));
    assert_ne!(app.status, "Closed tab");
    let screen = draw_screen(&mut app);
    assert!(screen.contains("This tab has unsaved changes"), "{screen}");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.tab_count(), 2);
    assert_eq!(app.editor.lines[0], "xalpha", "the edits are kept");
}

/// #862 negative: while the prompt is up a click behind it does nothing,
/// even on another tab's close button, which closes that tab once the
/// prompt is gone.
#[test]
fn a_click_behind_the_unsaved_prompt_closes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_unsaved_file(tmp.path());
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "bravo\n").unwrap();
    app.editor.open_pinned(&b).unwrap();
    app.editor.select(0);
    draw_screen(&mut app);
    let row = app.editor.tab_strip_y_for_test();
    let col = app.editor.close_screen_x(1).expect("b.txt's close button");
    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();
    draw_screen(&mut app);
    left_click(&mut app, col, row);
    assert_eq!(app.editor.tab_count(), 2, "the click went nowhere");
    assert!(app.pending_unsaved.is_some(), "the prompt is still asking");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    draw_screen(&mut app);
    left_click(&mut app, col, row);
    assert_eq!(app.editor.tab_count(), 1, "with no prompt it closes b.txt");
}

/// What the one hot-exit backup of `root` holds.
fn the_hot_exit_backup(cache: &std::path::Path, root: &std::path::Path) -> crate::hot_exit::Backup {
    let files = hot_exit_files(cache, root);
    assert_eq!(files.len(), 1, "one backup: {files:?}");
    crate::hot_exit::Backup::load(&files[0]).unwrap()
}

/// #862 hot exit: an unsaved buffer is backed up (path, text, cursor) once
/// its edits settle, owner-only, and never while the edit is fresh, so
/// typing never waits on a write. The file itself is not touched.
#[test]
fn hot_exit_backs_up_an_unsaved_buffer_once_it_settles() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, a) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    app.tick_hot_exit(std::time::Instant::now());
    assert!(
        hot_exit_files(cache.path(), tmp.path()).is_empty(),
        "not while the edit is fresh"
    );
    settle_hot_exit(&mut app);
    let backup = the_hot_exit_backup(cache.path(), tmp.path());
    assert_eq!(backup.buffers.len(), 1);
    let tab = &backup.buffers[0].tab;
    assert_eq!(tab.path.as_deref(), Some(a.as_path()));
    assert!(tab.dirty);
    assert_eq!(
        tab.unsaved_text.as_deref().unwrap().lines().next(),
        Some("xalpha")
    );
    assert_eq!((tab.cursor_row, tab.cursor_col), (0, 1));
    use std::os::unix::fs::PermissionsExt;
    let file = &hot_exit_files(cache.path(), tmp.path())[0];
    let mode = std::fs::metadata(file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "it holds unsaved text: owner-only");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha\n");
    assert!(!app.quit);
}

/// #1578: hot-exit backups lived under `~/.cache/croft`, which cache
/// cleaners delete, taking the only copy of unsaved edits with them. They
/// now live in the state directory; a backup an earlier croft left in the
/// cache moves there on launch and is restored from it, and so do sticky
/// notes.
#[test]
fn a_backup_left_in_the_cache_is_restored_from_the_state_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (app, a) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, &cache.path().join("hot-exit"));
    settle_hot_exit(&mut app);
    drop(app);
    let notes = cache.path().join("notes").join("0123456789abcdef.json");
    std::fs::create_dir_all(notes.parent().unwrap()).unwrap();
    std::fs::write(&notes, "[]").unwrap();

    let data = user_data_dir(cache.path(), state.path());
    assert_eq!(data, state.path());
    assert!(
        !cache.path().join("hot-exit").exists(),
        "nothing is left in the cache"
    );
    assert!(
        state
            .path()
            .join("notes")
            .join("0123456789abcdef.json")
            .is_file()
    );

    let mut next = with_hot_exit(
        App::new(tmp.path().to_path_buf()).unwrap(),
        &data.join("hot-exit"),
    );
    next.restore_hot_exit();
    let ed = next
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(a.as_path()))
        .expect("the unsaved tab is back");
    assert!(ed.dirty);
    assert_eq!(ed.lines[0], "xalpha");
}

/// #862 hot exit: croft killed with an unsaved buffer (no quit ran, the
/// App is simply dropped) comes back on the next launch of the workspace:
/// the tab returns unsaved, with its text and cursor, the file untouched,
/// and the backup is the new croft's own at once, so a crash straight
/// after the launch cannot lose the text either.
#[test]
fn hot_exit_restores_the_unsaved_tab_in_the_next_croft_on_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, a) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    drop(app);

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    let ed = next
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(a.as_path()))
        .expect("the unsaved tab is back");
    assert!(ed.dirty, "restored as unsaved");
    assert_eq!(ed.lines[0], "xalpha");
    assert_eq!((ed.cursor_row, ed.cursor_col), (0, 1));
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "alpha\n",
        "nothing is written to the file"
    );
    assert!(
        next.status.contains("restored 1 unsaved tab"),
        "{}",
        next.status
    );
    let kept = the_hot_exit_backup(cache.path(), tmp.path());
    assert_eq!(kept.buffers[0].tab.path.as_deref(), Some(a.as_path()));
}

/// #862 hot exit: every quit that keeps or knowingly drops the edits
/// removes the backup: a clean quit (all saved), S once the saves land, D,
/// and vim's `:qa!`.
#[test]
fn hot_exit_backup_goes_with_a_clean_quit_save_all_or_discard() {
    type Quit = fn(&mut App);
    let quits: [(&str, Quit); 4] = [
        ("clean quit", |app| {
            app.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL))
                .unwrap();
            press_ctrl_q(app);
        }),
        ("S", |app| {
            press_ctrl_q(app);
            app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
                .unwrap();
        }),
        ("D", |app| {
            press_ctrl_q(app);
            app.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE))
                .unwrap();
        }),
        (":qa!", |app| app.vim_run_ex("qa!")),
    ];
    for (name, quit) in quits {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let (app, _) = app_with_unsaved_file(tmp.path());
        let mut app = with_hot_exit(app, cache.path());
        settle_hot_exit(&mut app);
        assert_eq!(hot_exit_files(cache.path(), tmp.path()).len(), 1, "setup");
        quit(&mut app);
        assert!(app.quit, "{name}: {}", app.status);
        assert!(
            hot_exit_files(cache.path(), tmp.path()).is_empty(),
            "{name} removes the backup"
        );
    }
}

/// #862 hot exit negative: what is not a clean quit leaves the backup:
/// cancelling the quit prompt, and the update relaunch (its handoff carries
/// the same text across the exec, and the next croft backs it up again).
#[test]
fn hot_exit_backup_stays_when_croft_does_not_quit_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    press_ctrl_q(&mut app);
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(hot_exit_files(cache.path(), tmp.path()).len(), 1, "Esc");
    app.update_status = UpdateStatus::Ready;
    app.handle_key(key(
        KeyCode::F(9),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(app.pending_reexec && app.quit, "setup: the relaunch armed");
    assert_eq!(
        hot_exit_files(cache.path(), tmp.path()).len(),
        1,
        "relaunch"
    );
}

/// #862 hot exit: a file changed on disk after the backup still comes back
/// unsaved with the backed-up text, the status line says it changed, and
/// the first save refuses to overwrite the other change.
#[test]
fn hot_exit_restores_over_a_file_changed_since_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, a) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    drop(app);
    std::fs::write(&a, "changed elsewhere after the backup\n").unwrap();

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    next.focus_pane(Pane::Editor);
    assert_eq!(next.editor.path.as_deref(), Some(a.as_path()));
    assert!(next.editor.dirty);
    assert_eq!(next.editor.lines[0], "xalpha");
    assert!(
        next.status.contains("a.txt changed on disk"),
        "{}",
        next.status
    );
    next.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "changed elsewhere after the backup\n",
        "the first save does not clobber the other change"
    );
}

/// #862 hot exit: a backed-up file deleted since comes back as an unsaved
/// tab for that path (a save recreates it), and an untitled buffer holding
/// text comes back untitled.
#[test]
fn hot_exit_restores_deleted_and_untitled_buffers() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, a) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    let untitled = app.editor.open_unreadable_tab(None);
    untitled.lines = vec![String::from("scratch note")];
    untitled.dirty = true;
    settle_hot_exit(&mut app);
    assert_eq!(
        the_hot_exit_backup(cache.path(), tmp.path()).buffers.len(),
        2
    );
    drop(app);
    std::fs::remove_file(&a).unwrap();

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    let deleted = next
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(a.as_path()))
        .expect("the deleted file's tab is back");
    assert!(deleted.dirty && deleted.lines[0] == "xalpha");
    assert!(!a.exists(), "not recreated until saved");
    let scratch = next
        .editor
        .editors
        .iter()
        .find(|e| e.path.is_none() && e.dirty)
        .expect("the untitled buffer is back");
    assert_eq!(scratch.lines, vec![String::from("scratch note")]);
}

/// A CSV of widgets open in the grid, with hot exit backing up to `cache`.
fn csv_grid_app(root: &std::path::Path, cache: &std::path::Path) -> (App, PathBuf) {
    let p = root.join("d.csv");
    std::fs::write(&p, "name,qty\nwidget,3\ngadget,5\n").unwrap();
    let mut app = with_hot_exit(App::new(root.to_path_buf()).unwrap(), cache);
    app.editor.open(&p).unwrap();
    assert!(app.editor.sheet.is_some(), "the CSV opens in the grid");
    (app, p)
}

/// Type `value` into the grid's selected cell and commit it with Enter.
fn type_into_cell(app: &mut App, value: &str) {
    for c in value.chars() {
        app.handle_sheet_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_sheet_key(key(KeyCode::Enter, KeyModifiers::NONE));
}

/// The tab holding `path` after a relaunch, by path.
fn tab_for<'a>(app: &'a App, path: &std::path::Path) -> &'a Editor {
    app.editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(path))
        .expect("the file's tab is back")
}

/// #1593: an unsaved cell edit in a CSV grid survives a kill: the next
/// croft reopens the grid with the edited value, dirty, and the file is
/// untouched until saved.
#[test]
fn hot_exit_restores_a_csv_grid_s_unsaved_cell() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (mut app, p) = csv_grid_app(tmp.path(), cache.path());
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_cell(&mut app, "99");
    assert!(app.editor.dirty);
    settle_hot_exit(&mut app);
    assert_eq!(
        hot_exit_files(cache.path(), tmp.path()).len(),
        1,
        "backed up"
    );
    drop(app);

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    let ed = tab_for(&next, &p);
    let view = ed.sheet.as_ref().expect("reopened in the grid");
    assert_eq!(view.sheets[0].cell(0, 1), "99");
    assert_eq!(view.sheets[0].cell(1, 1), "5");
    assert!(ed.dirty && view.dirty, "restored as unsaved");
    assert!(
        next.status.contains("restored 1 unsaved tab"),
        "{}",
        next.status
    );
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "name,qty\nwidget,3\ngadget,5\n",
        "nothing is written to the file"
    );
    let idx = next
        .editor
        .editors
        .iter()
        .position(|e| e.path.as_deref() == Some(p.as_path()))
        .unwrap();
    next.editor.select(idx);
    next.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "name,qty\nwidget,99\ngadget,5\n",
        "{}",
        next.status
    );
}

/// #1593: a later edit to the same grid rewrites the backup, though the
/// grid's edits never touch its text.
#[test]
fn a_second_grid_edit_rewrites_the_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (mut app, p) = csv_grid_app(tmp.path(), cache.path());
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_cell(&mut app, "99");
    settle_hot_exit(&mut app);
    type_into_cell(&mut app, "7");
    settle_hot_exit(&mut app);
    drop(app);

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    let view = tab_for(&next, &p).sheet.as_ref().unwrap();
    assert_eq!(
        (view.sheets[0].cell(0, 1), view.sheets[0].cell(1, 1)),
        ("99", "7")
    );
}

/// #1593: an xlsx grid keeps its edited cells, applied over the workbook
/// as it reopens and saved through the workbook's own save.
#[test]
fn hot_exit_restores_an_xlsx_grid_s_unsaved_cell() {
    let (tmp, p, app) = prices_xlsx_app();
    let cache = tempfile::tempdir().unwrap();
    let mut app = with_hot_exit(app, cache.path());
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_cell(&mut app, "5");
    settle_hot_exit(&mut app);
    drop(app);

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    let ed = tab_for(&next, &p);
    let view = ed.sheet.as_ref().expect("reopened in the grid");
    assert_eq!(view.sheets[0].cell(0, 1), "5");
    assert!(ed.dirty && view.dirty);
    assert_eq!(view.cell_edits, vec![(0, 0, 1)], "queued for the save");
    assert_eq!(xlsx_value(&p, 2, 2), "4", "the workbook is untouched");
    let idx = next
        .editor
        .editors
        .iter()
        .position(|e| e.path.as_deref() == Some(p.as_path()))
        .unwrap();
    next.editor.select(idx);
    next.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(xlsx_value(&p, 2, 2), "5", "{}", next.status);
}

/// #1593: a value still being typed into a cell, never committed, is
/// backed up too: a kill mid-typing does not lose it.
#[test]
fn hot_exit_keeps_a_cell_still_being_typed_into() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (mut app, p) = csv_grid_app(tmp.path(), cache.path());
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    for c in "99".chars() {
        app.handle_sheet_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(app.editor.sheet.as_ref().unwrap().editing.is_some());
    settle_hot_exit(&mut app);
    drop(app);

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    let ed = tab_for(&next, &p);
    let view = ed.sheet.as_ref().expect("reopened in the grid");
    assert_eq!(view.sheets[0].cell(0, 1), "99");
    assert!(ed.dirty);
}

/// All the backed-up sheet cells across the hot-exit backups of `root`.
fn backed_up_sheet_cells(
    cache: &std::path::Path,
    root: &std::path::Path,
) -> Vec<crate::widgets::editor::SheetCellEdit> {
    hot_exit_files(cache, root)
        .iter()
        .filter_map(|f| crate::hot_exit::Backup::load(f).ok())
        .flat_map(|b| b.buffers)
        .flat_map(|b| b.tab.sheet_cells)
        .collect()
}

/// #1593: an xlsx's backed-up cells stay backed up while the workbook
/// cannot be opened, rather than going with a backup thought empty.
#[test]
fn hot_exit_keeps_xlsx_cells_while_the_workbook_is_gone() {
    let (tmp, p, app) = prices_xlsx_app();
    let cache = tempfile::tempdir().unwrap();
    let mut app = with_hot_exit(app, cache.path());
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_cell(&mut app, "5");
    settle_hot_exit(&mut app);
    drop(app);
    std::fs::remove_file(&p).unwrap();

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    settle_hot_exit(&mut next);
    assert_eq!(
        backed_up_sheet_cells(cache.path(), tmp.path()),
        vec![(String::from("Sheet1"), 0, 1, String::from("5"))]
    );
}

/// #1593: a backed-up xlsx cell whose row the workbook no longer has is
/// kept in the backup; the cells that do fit are restored.
#[test]
fn hot_exit_keeps_xlsx_cells_the_workbook_no_longer_has_room_for() {
    let (tmp, p, app) = prices_xlsx_app();
    let cache = tempfile::tempdir().unwrap();
    let mut app = with_hot_exit(app, cache.path());
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_cell(&mut app, "5");
    settle_hot_exit(&mut app);
    drop(app);
    let file = hot_exit_files(cache.path(), tmp.path()).remove(0);
    let mut backup = crate::hot_exit::Backup::load(&file).unwrap();
    backup.buffers[0]
        .tab
        .sheet_cells
        .push((String::from("Sheet1"), 99, 1, String::from("7")));
    backup.save(&file).unwrap();

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    let view = tab_for(&next, &p).sheet.as_ref().unwrap();
    assert_eq!(view.sheets[0].cell(0, 1), "5");
    assert_eq!(view.cell_edits, vec![(0, 0, 1)], "only the cell that fit");
    let cells = backed_up_sheet_cells(cache.path(), tmp.path());
    assert!(
        cells.contains(&(String::from("Sheet1"), 99, 1, String::from("7"))),
        "the unplaced cell stays backed up: {cells:?}"
    );
}

/// #1593: a backed-up xlsx cell goes back on the worksheet it was typed
/// into, found by name: one naming a worksheet the workbook no longer has
/// (renamed, or the sheets reordered and that one removed) is kept, not
/// put on whichever worksheet now sits at its old position.
#[test]
fn hot_exit_matches_xlsx_cells_to_their_worksheet_by_name() {
    let (tmp, p, app) = prices_xlsx_app();
    let cache = tempfile::tempdir().unwrap();
    let mut app = with_hot_exit(app, cache.path());
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_cell(&mut app, "5");
    settle_hot_exit(&mut app);
    drop(app);
    let file = hot_exit_files(cache.path(), tmp.path()).remove(0);
    let mut backup = crate::hot_exit::Backup::load(&file).unwrap();
    assert_eq!(
        backup.buffers[0].tab.sheet_cells,
        vec![(String::from("Sheet1"), 0, 1, String::from("5"))]
    );
    backup.buffers[0]
        .tab
        .sheet_cells
        .push((String::from("Archive"), 1, 1, String::from("7")));
    backup.save(&file).unwrap();

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    let view = tab_for(&next, &p).sheet.as_ref().unwrap();
    assert_eq!(view.sheets[0].cell(0, 1), "5");
    assert_eq!(view.sheets[0].cell(1, 1), "9", "not put on Sheet1");
    assert_eq!(view.cell_edits, vec![(0, 0, 1)]);
    let cells = backed_up_sheet_cells(cache.path(), tmp.path());
    assert!(
        cells.contains(&(String::from("Archive"), 1, 1, String::from("7"))),
        "{cells:?}"
    );
}

/// #1593: an xlsx no longer readable as a workbook opens as text or hex,
/// with none of its cells put back; they stay backed up rather than go
/// with a tab counted as restored.
#[test]
fn hot_exit_keeps_xlsx_cells_when_the_file_opens_outside_the_grid() {
    let (tmp, p, app) = prices_xlsx_app();
    let cache = tempfile::tempdir().unwrap();
    let mut app = with_hot_exit(app, cache.path());
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_cell(&mut app, "5");
    settle_hot_exit(&mut app);
    drop(app);
    std::fs::write(&p, "not a workbook\n").unwrap();

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    assert!(
        tab_for(&next, &p).sheet.is_none(),
        "setup: opened outside the grid"
    );
    assert!(!next.status.contains("restored 1"), "{}", next.status);
    settle_hot_exit(&mut next);
    assert_eq!(
        backed_up_sheet_cells(cache.path(), tmp.path()),
        vec![(String::from("Sheet1"), 0, 1, String::from("5"))]
    );
}

/// #1593: a value still being typed into an xlsx cell, with the tab not
/// yet dirty, is backed up with the workbook's disk stamp, so a change to
/// the workbook before the restore is still told.
#[test]
fn a_pending_xlsx_cell_s_backup_keeps_the_disk_stamp() {
    let (_tmp, _p, mut app) = prices_xlsx_app();
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Char('5'), KeyModifiers::NONE));
    assert!(!app.editor.dirty, "setup: only the pending cell");
    let backup = app.capture_hot_exit();
    assert_eq!(backup.buffers.len(), 1);
    assert!(backup.buffers[0].disk_stamp.is_some());
}

/// #1593: a grid's unsaved edits in one split are kept over a clean tab
/// of the same file in the focused split, here reopened as text.
#[test]
fn a_split_s_dirty_grid_wins_over_a_clean_tab_of_the_same_file() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (mut app, p) = csv_grid_app(tmp.path(), cache.path());
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_cell(&mut app, "99");
    app.split_editor();
    app.editor.force_text = true;
    app.editor.open(&p).unwrap();
    assert!(app.editor.sheet.is_none() && !app.editor.dirty, "setup");
    let state = app.capture_session_state();
    let tabs: Vec<_> = state
        .tabs
        .iter()
        .filter(|t| t.path.as_deref() == Some(p.as_path()))
        .collect();
    assert_eq!(tabs.len(), 1, "{:?}", state.tabs);
    assert!(tabs[0].dirty);
    assert!(
        tabs[0]
            .unsaved_text
            .as_deref()
            .unwrap()
            .contains("widget,99"),
        "{:?}",
        tabs[0]
    );
}

/// #1593: xlsx cells an update relaunch cannot put back (the workbook is
/// gone) are kept in a hot-exit backup the next launch restores from,
/// rather than lost with the deleted handoff.
#[test]
fn a_relaunch_keeps_xlsx_cells_it_could_not_restore() {
    let (tmp, p, mut app) = prices_xlsx_app();
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_cell(&mut app, "5");
    let state = app.capture_session_state();
    drop(app);
    let bytes = std::fs::read(&p).unwrap();
    std::fs::remove_file(&p).unwrap();

    let cache = tempfile::tempdir().unwrap();
    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    let left = next.apply_session_state(&state);
    assert_eq!(left.len(), 1);
    assert!(next.hold_unrestored_tabs(left));
    assert_eq!(
        backed_up_sheet_cells(cache.path(), tmp.path()),
        vec![(String::from("Sheet1"), 0, 1, String::from("5"))]
    );
    // Once the workbook is back, a later launch puts the cell back.
    drop(next);
    std::fs::write(&p, bytes).unwrap();
    let mut later = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    later.restore_hot_exit();
    let ed = tab_for(&later, &p);
    assert_eq!(ed.sheet.as_ref().unwrap().sheets[0].cell(0, 1), "5");
    assert!(ed.dirty, "{}", later.status);
}

/// #1593: the update relaunch carries a TSV grid's edits the same way.
#[test]
fn a_relaunch_keeps_a_tsv_grid_s_unsaved_cell() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("t.tsv");
    std::fs::write(&p, "name\tqty\nwidget\t3\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_cell(&mut app, "12");
    let state = app.capture_session_state();

    let mut next = App::new(tmp.path().to_path_buf()).unwrap();
    next.apply_session_state(&state);
    let ed = tab_for(&next, &p);
    let view = ed.sheet.as_ref().expect("reopened in the grid");
    assert_eq!(view.sheets[0].cell(0, 1), "12");
    assert!(ed.dirty);
}

/// Negative (#1593): a grid with no unsaved edits leaves no backup, and
/// one saved after its backup was written takes the backup away.
#[test]
fn a_clean_or_saved_grid_leaves_no_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (mut app, p) = csv_grid_app(tmp.path(), cache.path());
    settle_hot_exit(&mut app);
    assert!(hot_exit_files(cache.path(), tmp.path()).is_empty());
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_cell(&mut app, "99");
    settle_hot_exit(&mut app);
    assert_eq!(hot_exit_files(cache.path(), tmp.path()).len(), 1);
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert!(!app.editor.dirty, "{}", app.status);
    settle_hot_exit(&mut app);
    assert!(hot_exit_files(cache.path(), tmp.path()).is_empty());
    assert!(std::fs::read_to_string(&p).unwrap().contains("widget,99"));
}

/// Negative (#1593): a text tab's backup is unchanged: its text, no cells.
#[test]
fn a_text_tab_s_backup_carries_no_sheet_cells() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _a) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    let backup = the_hot_exit_backup(cache.path(), tmp.path());
    assert!(backup.buffers[0].tab.sheet_cells.is_empty());
    let raw = std::fs::read_to_string(&hot_exit_files(cache.path(), tmp.path())[0]).unwrap();
    assert!(!raw.contains("sheet_cells"), "{raw}");
}

/// #862 review (CodeRabbit): a tab restored for a file deleted since keeps
/// the stamp its edits were made against. It came back with none, so once
/// something made the file again, the first save wrote over that without
/// asking, as it would not for a file merely changed since.
#[test]
fn a_restored_tab_of_a_deleted_file_does_not_overwrite_one_made_since() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, a) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    drop(app);
    std::fs::remove_file(&a).unwrap();
    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    assert!(next.status.contains("no longer on disk"), "{}", next.status);
    assert_eq!(next.editor.path.as_deref(), Some(a.as_path()), "setup");
    std::fs::write(&a, "made elsewhere\n").unwrap();
    next.focus_pane(Pane::Editor);
    next.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "made elsewhere\n");
    assert!(next.status.contains("changed on disk"), "{}", next.status);
    assert!(next.editor.dirty, "the restored text is still there");
}

/// #862 review, the guard: a deleted file's restored tab can still be
/// saved. The first save asks, as for any file changed since; the second
/// writes it.
#[test]
fn a_restored_tab_of_a_deleted_file_is_saved_when_asked_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, a) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    drop(app);
    std::fs::remove_file(&a).unwrap();
    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    next.focus_pane(Pane::Editor);
    for _ in 0..2 {
        next.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
            .unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&a).unwrap().lines().next(),
        Some("xalpha")
    );
    assert!(!next.editor.dirty);
}

/// #862 review (CodeRabbit): a consumed backup that cannot be removed once
/// this croft's own backup lands is not forgotten. It was dropped without a
/// word, so text the user later discarded could come back at a launch. It
/// is said, and the quit tries it again and says so too.
#[test]
fn a_consumed_backup_that_cannot_be_removed_is_reported_and_tried_at_quit() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    drop(app);
    let old = as_a_dead_crofts_backup(cache.path(), tmp.path());
    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    let own = block_hot_exit_path(&next);
    next.restore_hot_exit();
    assert!(old.exists(), "setup: kept while its own write fails");
    // Neither emptied nor unlinked: a non-empty directory in its place.
    std::fs::remove_file(&old).unwrap();
    std::fs::create_dir_all(old.join("blocked")).unwrap();
    std::fs::remove_dir_all(&own).unwrap();
    settle_hot_exit(&mut next);
    assert!(own.is_file(), "setup: its own backup landed");
    let old_name = old.display().to_string();
    assert!(
        next.status.contains("Could not remove") && next.status.contains(&old_name),
        "{}",
        next.status
    );
    press_ctrl_q(&mut next);
    next.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE))
        .unwrap();
    assert!(next.quit);
    assert!(
        next.exit_notes.iter().any(|n| n.contains(&old_name)),
        "{:?}",
        next.exit_notes
    );
}

/// #862 review (CodeRabbit): a dead croft's backup can sit at this croft's
/// own backup path, where the process start time cannot be read (the name
/// is then `<pid>.json`) and the pid was recycled. Restoring from it, then
/// writing this croft's own backup, then cutting it down to a held copy
/// all hit one file: the cut wrote over the backup of what was restored.
/// It is moved aside before anything is restored from it.
#[test]
fn a_backup_at_this_crofts_own_path_is_moved_aside_before_restoring() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "bravo\n").unwrap();
    let b_buffer = crate::hot_exit::Buffer {
        tab: crate::session_state::OpenTabState {
            path: Some(b.clone()),
            dirty: true,
            cursor_row: 0,
            cursor_col: 0,
            scroll: 0,
            scroll_col: 0,
            unsaved_text: Some(String::from("zbravo\n")),
            sheet_cells: Vec::new(),
        },
        disk_stamp: None,
    };
    two_dead_backups_of_a(
        tmp.path(),
        cache.path(),
        ["first\n", "second\n"],
        vec![b_buffer],
    );
    // The older backup, holding a.txt and b.txt, sits at this croft's own
    // path; its write time comes with it.
    let older = hot_exit_files(cache.path(), tmp.path())
        .into_iter()
        .find(|f| f.ends_with("1-1.json"))
        .unwrap();
    let own = crate::hot_exit::own_path(cache.path(), tmp.path());
    std::fs::rename(&older, &own).unwrap();

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    assert!(
        next.status.contains("restored 2 unsaved tabs"),
        "{}",
        next.status
    );
    assert_eq!(
        hot_exit_texts_of(cache.path(), tmp.path(), &b),
        ["zbravo\n"],
        "b.txt's restored text is backed up"
    );
    assert_eq!(
        hot_exit_texts_of(cache.path(), tmp.path(), &a),
        ["first\n", "second\n"],
        "and so are both copies of a.txt"
    );
}

/// #862 hot exit negative: only the launched workspace's backup comes back;
/// another workspace's stays where it is for that workspace's next launch.
#[test]
fn hot_exit_does_not_restore_another_workspaces_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    drop(app);

    let mut elsewhere = with_hot_exit(App::new(other.path().to_path_buf()).unwrap(), cache.path());
    elsewhere.restore_hot_exit();
    assert!(
        elsewhere.editor.editors.iter().all(|e| !e.dirty),
        "nothing restored into the other workspace"
    );
    assert_eq!(hot_exit_files(cache.path(), tmp.path()).len(), 1, "kept");
}

/// #862 hot exit negative: a backup another running croft owns (a second
/// window on the workspace) is neither restored nor removed by this one.
/// This one is named by its pid alone, as backups were before hot exit
/// recorded start times: a live pid is then taken to be its croft.
#[test]
fn hot_exit_leaves_a_running_crofts_backup_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    drop(app);
    let mine = hot_exit_files(cache.path(), tmp.path()).remove(0);
    // pid 1 is always running.
    let live = mine.with_file_name("1.json");
    std::fs::rename(&mine, &live).unwrap();

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    assert!(
        next.editor.editors.iter().all(|e| !e.dirty),
        "not taken over"
    );
    assert!(live.exists(), "and not removed");
}

/// This test's hot-exit backup of `root`, renamed as a croft that is gone
/// left it (a pid that does not exist, in the pid-only name): a backup at a
/// path other than the restoring croft's own.
fn as_a_dead_crofts_backup(cache: &std::path::Path, root: &std::path::Path) -> PathBuf {
    let mine = hot_exit_files(cache, root).remove(0);
    let dead = mine.with_file_name("999999999.json");
    std::fs::rename(&mine, &dead).unwrap();
    dead
}

/// Put a non-empty directory where `app`'s own hot-exit backup goes, so
/// writing it fails even for root (a rename cannot replace a directory
/// that holds something). Returns the path, to clear the way again.
fn block_hot_exit_path(app: &App) -> PathBuf {
    let own = app.hot_exit_path().unwrap();
    std::fs::create_dir_all(own.join("blocked")).unwrap();
    own
}

/// #862 hot exit (review): the backup a restore consumed stays on disk
/// until this croft's own backup of the restored tabs is written. The
/// restore removed it first, so a failed write (a full disk) left the text
/// in memory only; the next tick then writes it and lets the old one go.
#[test]
fn a_failed_rewrite_after_a_restore_keeps_the_old_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    drop(app);
    let old = as_a_dead_crofts_backup(cache.path(), tmp.path());

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    let own = block_hot_exit_path(&next);
    next.restore_hot_exit();
    assert!(
        next.editor.dirty && next.editor.lines[0] == "xalpha",
        "setup: restored"
    );
    assert!(
        old.exists(),
        "the old backup stays while it is the only copy on disk"
    );
    assert!(next.status.contains("Could not back up"), "{}", next.status);
    std::fs::remove_dir_all(&own).unwrap();
    settle_hot_exit(&mut next);
    assert!(own.is_file(), "a later tick writes it");
    assert!(!old.exists(), "and only then the old backup goes");
}

/// #862 hot exit (review): a backup write that failed is tried again by a
/// later tick, with no further edit. It counted as written, so nothing was
/// backed up until the user typed again.
#[test]
fn a_failed_hot_exit_write_is_retried_by_a_later_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    let own = block_hot_exit_path(&app);
    settle_hot_exit(&mut app);
    assert!(!own.is_file(), "setup: the write failed");
    std::fs::remove_dir_all(&own).unwrap();
    settle_hot_exit(&mut app);
    assert!(own.is_file(), "retried with no new edit");
    let backup = crate::hot_exit::Backup::load(&own).unwrap();
    assert_eq!(backup.buffers.len(), 1);
}

/// #862 hot exit negative: a restore whose rewrite lands removes the backup
/// it consumed; this croft's own backup is never removed, even when the
/// consumed file carries its name (a dead croft whose pid it reuses).
#[test]
fn a_restore_replaces_the_old_backup_and_keeps_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    drop(app);
    let old = as_a_dead_crofts_backup(cache.path(), tmp.path());
    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    assert!(!old.exists(), "the consumed backup goes");
    let own = next.hot_exit_path().unwrap();
    assert!(own.is_file(), "its own holds the text now");
    drop(next);

    // The consumed file is this croft's own name: it is rewritten, never
    // removed.
    let mut again = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    again.restore_hot_exit();
    assert!(again.editor.dirty, "setup: restored again");
    assert!(own.is_file(), "its own backup is kept");
    let backup = crate::hot_exit::Backup::load(&own).unwrap();
    assert!(
        backup.buffers[0]
            .tab
            .unsaved_text
            .as_deref()
            .unwrap()
            .starts_with("xalpha")
    );
}

/// #862 hot exit negative: a consumed backup that restores nothing (no
/// buffers in it) is removed at once; there is no text to wait on a write
/// for, and no backup of this croft's is written.
#[test]
fn a_backup_that_restores_nothing_is_removed_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let dir = crate::hot_exit::workspace_dir(cache.path(), tmp.path());
    let empty = dir.join("999999999.json");
    crate::hot_exit::Backup {
        workspace_root: tmp.path().to_path_buf(),
        buffers: Vec::new(),
    }
    .save(&empty)
    .unwrap();
    let mut app = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    app.restore_hot_exit();
    assert!(!empty.exists());
    assert!(hot_exit_files(cache.path(), tmp.path()).is_empty());
}

/// #862 hot exit (review): a backup whose pid is alive again, but as a
/// process that started at another time than the croft that wrote it, is
/// from a croft that is gone: its pid was recycled. It is restored. Judged
/// by the pid alone it stayed untouched as long as the newcomer ran.
#[test]
fn hot_exit_restores_a_backup_whose_pid_was_recycled() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, a) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    drop(app);
    let mine = hot_exit_files(cache.path(), tmp.path()).remove(0);
    // pid 1 is always running, and did not start one second into 1970.
    let recycled = mine.with_file_name("1-1.json");
    std::fs::rename(&mine, &recycled).unwrap();

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    let ed = next
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(a.as_path()))
        .expect("restored");
    assert!(ed.dirty && ed.lines[0] == "xalpha");
    assert!(!recycled.exists(), "consumed");
}

/// Two dead crofts' backups (`1-1.json`, `1-2.json`: pid 1 never started
/// in 1970), each holding `a.txt` with the unsaved text `texts` gives it,
/// the second written later, and `extra` buffers added to the first.
fn two_dead_backups_of_a(
    tmp: &std::path::Path,
    cache: &std::path::Path,
    texts: [&str; 2],
    extra: Vec<crate::hot_exit::Buffer>,
) {
    let (app, _) = app_with_unsaved_file(tmp);
    let mut app = with_hot_exit(app, cache);
    settle_hot_exit(&mut app);
    drop(app);
    let mine = hot_exit_files(cache, tmp).remove(0);
    let mut backup = crate::hot_exit::Backup::load(&mine).unwrap();
    std::fs::remove_file(&mine).unwrap();
    for (i, text) in texts.iter().enumerate() {
        backup.buffers.truncate(1);
        backup.buffers[0].tab.unsaved_text = Some((*text).to_string());
        if i == 0 {
            backup.buffers.extend(extra.iter().cloned());
        }
        let file = mine.with_file_name(format!("1-{}.json", i + 1));
        backup.save(&file).unwrap();
        let written =
            std::time::SystemTime::now() - std::time::Duration::from_secs(60 - 30 * i as u64);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(written)
            .unwrap();
    }
}

/// #862 review: two crofts on one workspace, both killed holding unsaved
/// edits to one file. Restoring the second backup into the tab the first
/// filled overwrote that text, and both backups were then deleted, so one
/// set of edits was gone while the status line counted two tabs restored.
/// The second copy now stays in its backup, and comes back once the first
/// has been saved or closed.
#[test]
fn hot_exit_keeps_a_second_backup_of_a_file_it_already_restored() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    two_dead_backups_of_a(
        tmp.path(),
        cache.path(),
        ["first\n", "second\n"],
        Vec::new(),
    );

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    assert!(
        next.status.contains("restored 1 unsaved tab"),
        "{}",
        next.status
    );
    assert!(
        next.status.contains("a.txt has a second unsaved copy"),
        "{}",
        next.status
    );
    let shown = next
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(a.as_path()))
        .expect("restored");
    assert_eq!(shown.lines[0], "second", "the newer copy fills the tab");
    assert_eq!(
        hot_exit_texts_of(cache.path(), tmp.path(), &a),
        ["first\n", "second\n"],
        "both texts are still on disk"
    );

    // Once this croft's copy is gone (a clean quit that discards it), the
    // next launch brings back the other one.
    next.clear_hot_exit();
    drop(next);
    let mut third = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    third.restore_hot_exit();
    let other = third
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(a.as_path()))
        .expect("the second copy is restored");
    assert!(other.dirty);
    assert_eq!(
        other.lines[0], "first",
        "the copy the first launch held back"
    );
}

/// #862 review, the guard: the backup that held the second copy keeps only
/// that copy. Its other file is restored and not left in it to come back a
/// second time.
#[test]
fn hot_exit_restores_the_rest_of_a_backup_that_also_held_a_second_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "bravo\n").unwrap();
    let b_buffer = crate::hot_exit::Buffer {
        tab: crate::session_state::OpenTabState {
            path: Some(b.clone()),
            dirty: true,
            cursor_row: 0,
            cursor_col: 0,
            scroll: 0,
            scroll_col: 0,
            unsaved_text: Some(String::from("zbravo\n")),
            sheet_cells: Vec::new(),
        },
        disk_stamp: None,
    };
    two_dead_backups_of_a(
        tmp.path(),
        cache.path(),
        ["first\n", "second\n"],
        vec![b_buffer],
    );

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    assert!(
        next.status.contains("restored 2 unsaved tabs"),
        "{}",
        next.status
    );
    let bed = next
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(b.as_path()))
        .expect("b.txt restored");
    assert!(bed.dirty && bed.lines[0] == "zbravo");
    assert_eq!(
        hot_exit_texts_of(cache.path(), tmp.path(), &b),
        ["zbravo\n"],
        "b.txt is in this croft's backup alone"
    );
    assert_eq!(
        hot_exit_texts_of(cache.path(), tmp.path(), &a),
        ["first\n", "second\n"]
    );
}

/// #862 hot exit negative: a backup whose pid and start time are both
/// those of a running process is that croft's live copy: neither restored
/// nor removed.
#[test]
fn hot_exit_leaves_the_backup_of_a_process_still_running_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    drop(app);
    let mine = hot_exit_files(cache.path(), tmp.path()).remove(0);
    let init = crate::hot_exit::start_time_of(1).expect("pid 1 runs");
    let live = mine.with_file_name(format!("1-{init}.json"));
    std::fs::rename(&mine, &live).unwrap();

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    assert!(
        next.editor.editors.iter().all(|e| !e.dirty),
        "not taken over"
    );
    assert!(live.exists(), "and not removed");
}

/// #862 hot exit (review): a backup the quit could not remove is not
/// dropped silently. It holds text the user chose to discard (D), so the
/// failure is said, with the path, rather than left to be restored as
/// unsaved at the next launch without a word.
#[test]
fn a_backup_a_quit_cannot_remove_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    // A directory where the backup goes: neither written nor removable.
    let own = block_hot_exit_path(&app);
    press_ctrl_q(&mut app);
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.quit, "the quit still goes ahead");
    assert!(
        app.status.contains("Could not remove") && app.status.contains(&own.display().to_string()),
        "{}",
        app.status
    );
    assert_eq!(
        app.exit_notes,
        vec![app.status.clone()],
        "and it is said again on stderr once croft has gone"
    );
}

/// #862 hot exit (review): a backup that could not be read is not kept
/// forever. Reported and kept for a week, in case it can be recovered by
/// hand, it is removed once it is older than that.
#[test]
fn an_unreadable_backup_older_than_a_week_is_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let dir = crate::hot_exit::workspace_dir(cache.path(), tmp.path());
    std::fs::create_dir_all(&dir).unwrap();
    let corrupt = dir.join("999999999.json");
    std::fs::write(&corrupt, "{\"workspace_root\": \"/tm").unwrap();
    let eight_days = std::time::Duration::from_secs(8 * 24 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(&corrupt)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - eight_days)
        .unwrap();
    let mut app = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    app.restore_hot_exit();
    assert!(!corrupt.exists(), "expired");
    assert!(app.editor.editors.iter().all(|e| !e.dirty));
    assert!(app.status.contains("removed"), "{}", app.status);
}

/// #862 hot exit negative: clean buffers are never backed up, and once the
/// last unsaved edit is saved the backup goes.
#[test]
fn hot_exit_does_not_back_up_clean_buffers() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let app = app_with_open_file(tmp.path(), "a.txt", "alpha\n");
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    assert!(hot_exit_files(cache.path(), tmp.path()).is_empty());
    assert!(
        !crate::hot_exit::workspace_dir(cache.path(), tmp.path()).exists(),
        "not even the directory"
    );
    type_into_editor(&mut app, "x");
    settle_hot_exit(&mut app);
    assert_eq!(hot_exit_files(cache.path(), tmp.path()).len(), 1, "setup");
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap();
    settle_hot_exit(&mut app);
    assert!(hot_exit_files(cache.path(), tmp.path()).is_empty(), "saved");
}

/// #862 hot exit negative: an unsaved buffer that has not changed since
/// the last backup is not written again, however often the tick runs or
/// the caret moves; the next edit is.
#[test]
fn hot_exit_does_not_rewrite_an_unchanged_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    let file = hot_exit_files(cache.path(), tmp.path()).remove(0);
    std::fs::write(&file, "sentinel").unwrap();
    settle_hot_exit(&mut app);
    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    settle_hot_exit(&mut app);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "sentinel",
        "not rewritten"
    );
    type_into_editor(&mut app, "y");
    settle_hot_exit(&mut app);
    let backup = crate::hot_exit::Backup::load(&file).unwrap();
    assert!(
        backup.buffers[0]
            .tab
            .unsaved_text
            .as_deref()
            .unwrap()
            .contains("xaylpha")
    );
}

/// #862 hot exit: re-rooting the workspace takes this croft's backup along
/// to the new root. Left under the old one, the old workspace's next launch
/// would restore buffers this croft still holds and later saves.
#[test]
fn hot_exit_backup_follows_a_change_of_workspace_root() {
    let tmp = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, _) = app_with_unsaved_file(tmp.path());
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    assert_eq!(hot_exit_files(cache.path(), tmp.path()).len(), 1, "setup");
    app.change_workspace_root(other.path().to_path_buf());
    settle_hot_exit(&mut app);
    assert!(
        hot_exit_files(cache.path(), tmp.path()).is_empty(),
        "nothing left under the old root"
    );
    assert_eq!(hot_exit_files(cache.path(), other.path()).len(), 1);
}

/// #862 hot exit negative: a backup that cannot be read (truncated, not
/// JSON) restores nothing and does not panic; it is kept, since it may be
/// the only copy of the text, and the status line says so.
#[test]
fn hot_exit_ignores_a_corrupt_backup_without_a_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let dir = crate::hot_exit::workspace_dir(cache.path(), tmp.path());
    std::fs::create_dir_all(&dir).unwrap();
    let corrupt = dir.join("999999999.json");
    std::fs::write(&corrupt, "{\"workspace_root\": \"/tm").unwrap();
    let mut app = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    app.restore_hot_exit();
    assert!(app.editor.editors.iter().all(|e| !e.dirty));
    assert!(corrupt.exists(), "kept");
    assert!(app.status.contains("could not read"), "{}", app.status);
}

/// A comment longer than the line on disk it replaces (#862): a restore
/// that kept the disk file's syntax spans would colour only the first few
/// characters of it, or none.
const RESTORED_COMMENT: &str = "# a comment that is longer than the disk line";

/// The syntax spans `text` gets as line 0 of a `.py` file opened from disk,
/// the reference a restored buffer holding the same text must match.
fn spans_of_python_line_opened_from_disk(text: &str) -> Vec<(usize, usize, ratatui::style::Style)> {
    let tmp = tempfile::tempdir().unwrap();
    let app = app_with_open_file(tmp.path(), "ref.py", &format!("{text}\n"));
    app.editor.line_spans_for_test(0)
}

/// #862 hot exit: a restored buffer is highlighted as the text it holds,
/// not as the file on disk it was opened over. It kept the disk text's
/// spans: here the restored comment was coloured only for the length of
/// the disk line `x = 1`, and the rest stayed plain until the next edit.
/// The edit counter moves too, so the LSP and everything else that follows
/// it resync to the restored text.
#[test]
fn hot_exit_restored_text_is_highlighted_as_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut app = with_hot_exit(
        app_with_open_file(tmp.path(), "a.py", "x = 1\n"),
        cache.path(),
    );
    app.editor.lines = vec![String::from(RESTORED_COMMENT), String::new()];
    app.editor.dirty = true;
    settle_hot_exit(&mut app);
    drop(app);

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    assert_eq!(next.editor.lines[0], RESTORED_COMMENT, "setup: restored");
    let want = spans_of_python_line_opened_from_disk(RESTORED_COMMENT);
    assert!(
        want.iter()
            .any(|&(s, e, _)| s == 0 && e == RESTORED_COMMENT.len()),
        "setup: the comment is one span over the whole line: {want:?}"
    );
    assert_eq!(
        next.editor.line_spans_for_test(0),
        want,
        "the whole restored comment carries the comment style"
    );
    let mut opened = crate::widgets::editor::Editor::new();
    opened.open(&tmp.path().join("a.py")).unwrap();
    assert_ne!(
        next.editor.edit_seq, opened.edit_seq,
        "the edit counter moved past the disk text's"
    );
}

/// #862: the update relaunch restores through the same path and had the
/// same gap: its unsaved text kept the disk file's syntax spans.
#[test]
fn relaunch_restored_text_is_highlighted_as_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "x = 1\n");
    app.editor.lines = vec![String::from(RESTORED_COMMENT), String::new()];
    app.editor.dirty = true;
    let state = app.capture_session_state();

    let mut next = App::new(tmp.path().to_path_buf()).unwrap();
    next.apply_session_state(&state);
    let a = tmp.path().join("a.py");
    let ed = next
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(a.as_path()))
        .unwrap();
    assert_eq!(ed.lines[0], RESTORED_COMMENT, "setup: restored");
    assert_eq!(
        ed.line_spans_for_test(0),
        spans_of_python_line_opened_from_disk(RESTORED_COMMENT)
    );
}

/// #862 negative: an untitled buffer has no language, so its restored text
/// gets no syntax spans at all (and no panic); a clean tab open beside the
/// restored ones keeps its spans, its text and its edit counter.
#[test]
fn restoring_leaves_untitled_text_plain_and_clean_tabs_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut app = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    let scratch = app.editor.open_unreadable_tab(None);
    scratch.lines = vec![String::from(RESTORED_COMMENT)];
    scratch.dirty = true;
    settle_hot_exit(&mut app);
    drop(app);

    let clean = tmp.path().join("clean.py");
    std::fs::write(&clean, "def f():\n    return 1\n").unwrap();
    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.editor.open_pinned(&clean).unwrap();
    let before = (
        next.editor.line_spans_for_test(0),
        next.editor.lines.clone(),
        next.editor.edit_seq,
    );
    assert!(!before.0.is_empty(), "setup: the clean tab is highlighted");
    next.restore_hot_exit();
    let untitled = next
        .editor
        .editors
        .iter()
        .find(|e| e.path.is_none() && e.dirty)
        .expect("the untitled buffer is back");
    assert_eq!(untitled.lines, vec![String::from(RESTORED_COMMENT)]);
    assert!(
        untitled.line_spans_for_test(0).is_empty(),
        "no language, no spans: {:?}",
        untitled.line_spans_for_test(0)
    );
    let kept = next
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(clean.as_path()))
        .unwrap();
    assert!(!kept.dirty);
    assert_eq!(
        (
            kept.line_spans_for_test(0),
            kept.lines.clone(),
            kept.edit_seq
        ),
        before,
        "the clean tab is untouched"
    );
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
    let tab = state
        .tabs
        .iter()
        .find(|t| t.path.as_ref() == Some(&file))
        .unwrap();
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

/// A relaunch carries the unsaved text of every split, not just the
/// focused one.
#[test]
fn session_state_keeps_unsaved_edits_from_every_split() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let c = tmp.path().join("c.txt");
    std::fs::write(&a, "a\n").unwrap();
    std::fs::write(&c, "c\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.editor.lines = vec![String::from("edited a")];
    app.editor.dirty = true;
    app.focus_pane(Pane::Editor);
    app.split_editor();
    // The focused (right) group moves to another file and closes its copy.
    app.editor.open_pinned(&c).unwrap();
    app.editor.close_tab(0);
    let state = app.capture_session_state();
    let tab = state
        .tabs
        .iter()
        .find(|t| t.path.as_ref() == Some(&a))
        .expect("the left split's dirty tab is carried");
    assert_eq!(tab.unsaved_text.as_deref(), Some("edited a"));
}

/// A self-update must not drop the only copy of unsaved text: neither an
/// untitled buffer's nor that of a file deleted while the update ran.
#[test]
fn session_state_keeps_unsaved_untitled_and_unreadable_buffers() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("gone.txt");
    std::fs::write(&file, "saved\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.lines = vec![String::from("scratch notes")];
    app.editor.dirty = true;
    app.editor.open_pinned(&file).unwrap();
    app.editor.lines = vec![String::from("edited gone")];
    app.editor.dirty = true;
    let state = app.capture_session_state();
    assert_eq!(state.tabs.len(), 2);
    std::fs::remove_file(&file).unwrap();

    let mut restored = App::new(tmp.path().to_path_buf()).unwrap();
    restored.apply_session_state(&state);
    let eds = &restored.editor.editors;
    let untitled = eds.iter().find(|e| e.path.is_none()).unwrap();
    assert!(untitled.dirty);
    assert_eq!(untitled.lines, vec![String::from("scratch notes")]);
    let gone = eds
        .iter()
        .find(|e| e.path.as_deref() == Some(file.as_path()))
        .unwrap();
    assert!(gone.dirty);
    assert_eq!(gone.lines, vec![String::from("edited gone")]);
    assert_eq!(eds.len(), 2, "the blank initial tab is reused");

    // A blank untitled buffer is not carried.
    let blank = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(blank.capture_session_state().tabs.is_empty());
}

#[test]
fn unsaved_count_tracks_dirty_editors() {
    // The Explorer activity badge mirrors VS Code: the count of open editors
    // with unsaved edits. A freshly-opened blank buffer is clean; one edit
    // makes it count as a single unsaved file.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert_eq!(app.unsaved_count(), 0, "a fresh blank buffer is clean");
    app.editor.insert_char('x');
    assert_eq!(
        app.unsaved_count(),
        1,
        "an edited buffer counts as one unsaved file"
    );
}

#[test]
fn a_restored_dirty_tab_gets_an_auto_save_stamp() {
    // A session-restored dirty tab has no last_edit_at; due() then never
    // fires and the restored unsaved content sits unwritten forever.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    let state = app.capture_session_state();
    let mut restored = App::new(tmp.path().to_path_buf()).unwrap();
    restored.apply_session_state(&state);
    let dirty = restored
        .editor
        .editors
        .iter()
        .find(|e| e.dirty)
        .expect("the dirty tab survives the restore");
    assert!(
        dirty.last_edit_at.is_some(),
        "a restored dirty tab must be eligible for auto save"
    );
}

/// The release is about the focused terminal only: from the editor,
/// Ctrl+Q still quits (#1290).
#[test]
fn ctrl_q_still_quits_outside_the_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('q'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.quit, "with the editor focused, Ctrl+Q still quits");
}

/// The PROBLEMS tab owns the panel pane while it is shown, so there is no
/// app to hand Ctrl+Q to: it still quits (#1290).
#[test]
fn ctrl_q_still_quits_from_the_problems_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = focused_terminal_app(tmp.path());
    app.bottom_panel_tab = BottomPanelTab::Problems;
    app.handle_key(key(KeyCode::Char('q'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.quit, "Ctrl+Q quits when the panel shows PROBLEMS");
}

/// #1580: `Ctrl`+`N` (`Cmd`+`N`) opens a focused `Untitled-1` tab with no
/// path beside the open file, and the next one is `Untitled-2`. It used to
/// do nothing.
#[test]
fn ctrl_n_opens_numbered_untitled_tabs() {
    for mods in new_untitled_chords() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_open_file(tmp.path(), "b.txt", "keep\n");
        app.handle_key(key(KeyCode::Char('n'), mods)).unwrap();
        assert_eq!(app.editor.tab_count(), 2, "{mods:?}");
        assert_eq!(app.editor.path, None);
        assert!(app.focus == Pane::Editor);
        let first = app.editor.active_index();
        assert_eq!(app.editor.tab_display_label(first), "Untitled-1");
        type_str(&mut app, "trace");
        assert_eq!(app.editor.lines[0], "trace");
        app.handle_key(key(KeyCode::Char('n'), mods)).unwrap();
        assert_eq!(app.editor.tab_count(), 3);
        assert_eq!(
            app.editor.tab_display_label(app.editor.active_index()),
            "Untitled-2"
        );
        assert_eq!(
            app.editor.tab_display_label(first),
            "● Untitled-1",
            "typed into"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("b.txt")).unwrap(),
            "keep\n"
        );
        assert_eq!(
            std::fs::read_dir(tmp.path()).unwrap().count(),
            1,
            "no file is created on disk"
        );
    }
}

/// #1580: text typed into an untitled tab is unsaved work: quitting asks,
/// naming the tab.
#[test]
fn quitting_with_text_in_an_untitled_tab_asks_first() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "b.txt", "keep\n");
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::SUPER))
        .unwrap();
    type_str(&mut app, "draft");
    press_ctrl_q(&mut app);
    assert!(!app.quit);
    assert_eq!(app.pending_unsaved, Some(UnsavedExit::Quit));
    let screen = draw_screen(&mut app);
    assert!(screen.contains("Untitled-1"), "{screen}");
}

/// #1580: saving an untitled tab asks where (Save As); the tab then takes
/// the path and its language, and its number is free for the next one.
#[test]
fn saving_an_untitled_tab_asks_where_and_names_the_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "b.txt", "keep\n");
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::SUPER))
        .unwrap();
    type_str(&mut app, "x = 1");
    app.save();
    let prompt = app.prompt.as_mut().expect("Save asks where");
    assert!(matches!(prompt.kind, PromptKind::SaveAs));
    assert_eq!(prompt.buffer, "");
    prompt.buffer = String::from("scratch.py");
    app.commit_prompt();
    let saved = tmp.path().join("scratch.py");
    assert_eq!(app.editor.path.as_deref(), Some(saved.as_path()));
    assert!(
        std::fs::read_to_string(&saved)
            .unwrap()
            .starts_with("x = 1")
    );
    assert!(!app.editor.dirty);
    assert!(app.editor.language().is_some(), "a .py file is Python");
    let idx = app.editor.active_index();
    assert_eq!(app.editor.tab_display_label(idx), "scratch.py");
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.editor.tab_display_label(app.editor.active_index()),
        "Untitled-1"
    );
}

/// #1580: untitled numbers are unique across split groups. With
/// `Untitled-1` in one group, a new one in the other is `Untitled-2`.
#[test]
fn untitled_numbers_stay_unique_across_split_groups() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "b.txt", "keep\n");
    app.run_command(Command::NewUntitledFile);
    assert_eq!(app.editor.tab_display_label(1), "Untitled-1");
    app.editor.select(0);
    app.split_editor_dir(editor_layout::SplitDir::Horizontal, true);
    assert!(app.editor_layout.is_split());

    app.run_command(Command::NewUntitledFile);

    let label = app.editor.tab_display_label(app.editor.active_index());
    assert_eq!(label, "Untitled-2");
}

#[test]
fn reloading_over_unsaved_edits_keeps_them_in_local_history() {
    // Typing `reload` is the explicit consent; what it discards stays
    // recoverable from TIMELINE.
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, f) = app_with_a_disk_conflict(tmp.path());
    type_keys(&mut app, "reload");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.lines.join("\n"),
        "RATE = 0.2  # from the other branch"
    );
    assert!(!app.editor.dirty);
    let held: Vec<String> = crate::history::entries_in(&app.history_root, &f)
        .iter()
        .map(|s| std::fs::read_to_string(&s.file).unwrap())
        .collect();
    assert!(
        held.iter().any(|h| h.contains("def tax(amount):")),
        "the discarded buffer is in Local History: {held:?}"
    );
}

/// #1568 negative: edits discarded at close ("Don't Save") are not on disk,
/// so their history does not apply to the file that reopens.
#[test]
fn discarded_edits_leave_no_history_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.py");
    std::fs::write(&a, "value = 1\n").unwrap();
    let mut tabs = crate::widgets::editor::EditorTabs::new();
    tabs.open_pinned(&a).unwrap();
    tabs.end_line();
    tabs.insert_str("00");
    assert!(tabs.dirty);
    tabs.close_tab(tabs.active_index());
    tabs.open_pinned(&a).unwrap();
    assert_eq!(tabs.lines[0], "value = 1");
    assert!(!tabs.undo(), "nothing to undo in the file as it is on disk");
}

/// #1588: hot exit never backs up a merge editor's Result as an unsaved
/// edit of the file, so a relaunch after the terminal closed brings the
/// file back clean, as it is on disk, not as a dirty tab whose Ctrl+S
/// writes the Result over it.
#[test]
fn hot_exit_does_not_keep_a_merge_result_as_the_files_unsaved_text() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (app, file) = app_with_merge_editor_open(tmp.path());
    let disk = std::fs::read_to_string(&file).unwrap();
    let mut app = with_hot_exit(app, cache.path());
    settle_hot_exit(&mut app);
    assert!(
        !app.capture_hot_exit()
            .buffers
            .iter()
            .any(|b| b.tab.path.as_deref() == Some(file.as_path())),
        "no unsaved buffer for the merged file"
    );
    drop(app);

    let mut next = with_hot_exit(App::new(tmp.path().to_path_buf()).unwrap(), cache.path());
    next.restore_hot_exit();
    assert!(
        next.editor
            .editors
            .iter()
            .all(|e| e.path.as_deref() != Some(file.as_path()) || !e.dirty),
        "the file does not come back dirty"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), disk);
}

/// #1588 negative: a plain unsaved edit of the same file, once the merge
/// editor is closed with Reopen as Text, is still backed up.
#[test]
fn an_unsaved_edit_after_reopen_as_text_is_still_backed_up() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_merge_editor_open(tmp.path());
    app.run_command(crate::widgets::command_palette::Command::ReopenAsText);
    assert!(app.editor.merge.is_none(), "setup: a plain text tab");
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.editor.insert_char('x');
    let backup = app.capture_hot_exit();
    let kept = backup
        .buffers
        .iter()
        .find(|b| b.tab.path.as_deref() == Some(file.as_path()))
        .expect("the unsaved edit is backed up");
    assert!(
        kept.tab
            .unsaved_text
            .as_deref()
            .unwrap()
            .starts_with("xtop")
    );
}
