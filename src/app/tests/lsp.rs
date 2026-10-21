//! Language-server features: completion, hover, go to definition, rename,
//! code actions, formatting and diagnostics.

use super::*;

#[test]
fn caret_move_clears_lsp_occurrence_tints_and_edits_already_did() {
    // The occurrences tick keeps a resting caret's tints and drops them the
    // moment the caret moves; a reply for the old spot must also be dropped
    // (the in-flight request is cancelled on move).
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("occ.txt");
    std::fs::write(&file, "one two\nthree\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_at(&file, 0, 0).unwrap();
    app.tick_occurrences(); // observe the caret's resting spot
    app.editor
        .apply_occurrences(vec![crate::lsp::manager::OccurrenceItem {
            start_line: 0,
            start_char: 0,
            end_line: 0,
            end_char: 3,
            write: false,
        }]);
    app.tick_occurrences();
    assert_eq!(
        app.editor.occurrence_count_for_test(),
        1,
        "a resting caret must keep its tints across ticks"
    );
    app.editor.cursor_col = 4;
    let changed = app.tick_occurrences();
    assert!(changed, "the clearing tick must report a redraw");
    assert_eq!(
        app.editor.occurrence_count_for_test(),
        0,
        "moving the caret must drop the old symbol's tints"
    );
}

#[test]
fn cmd_k_shift_u_and_d_fire_type_hierarchy_requests() {
    // #613: Shift+U asks for supertypes, Shift+D for subtypes, and neither
    // falls through to the plain U / D arms (close saved tabs, etc.).
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.py");
    std::fs::write(&f, "class A:\n    pass\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('U'), KeyModifiers::SHIFT)));
    let first = app.call_hierarchy_request_id.expect("supertypes armed");
    // CSI-u hosts report Shift+D as a lowercase char with SHIFT.
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('d'), KeyModifiers::SHIFT)));
    let second = app.call_hierarchy_request_id.expect("subtypes armed");
    assert!(second > first);
    assert_eq!(
        app.editor.tab_count(),
        1,
        "the tab survived: no close arm ran"
    );
}

#[test]
fn a_late_call_hierarchy_reply_does_not_clobber_an_open_menu() {
    // rust-analyzer can answer seconds later; if the user opened another
    // menu while waiting, the reply must be dropped, not replace what they
    // are looking at (and yank focus to the editor).
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let items = vec![(
        String::from("caller — a.rs:1"),
        MenuAction::GoToLocation {
            path: tmp.path().join("a.rs"),
            line: 0,
            col: 0,
        },
    )];
    let other = ContextMenu::flat((1, 1), items.clone(), tmp.path().to_path_buf());
    app.context_menu = Some(other);
    let menu_before = app.context_menu.as_ref().map(|m| m.items.len());
    app.present_call_hierarchy_menu(items, "incoming calls");
    assert_eq!(
        app.context_menu.as_ref().map(|m| m.items.len()),
        menu_before,
        "an open menu survives a late reply"
    );
    assert_eq!(app.status, "Call hierarchy ready, but another menu is open");
    // With no menu open, the reply presents normally.
    app.context_menu = None;
    app.present_call_hierarchy_menu(
        vec![(
            String::from("caller — a.rs:1"),
            MenuAction::GoToLocation {
                path: tmp.path().join("a.rs"),
                line: 0,
                col: 0,
            },
        )],
        "incoming calls",
    );
    assert!(
        app.context_menu.is_some(),
        "a free slot presents the picker"
    );
    assert_eq!(app.status, "1 incoming calls");
}

#[test]
fn format_diagnostics_labels_each_message_and_sorts_errors_first() {
    use crate::lsp::manager::DiagnosticSeverity;
    let diags = vec![
        (DiagnosticSeverity::Warning, String::from("unused variable")),
        (DiagnosticSeverity::Error, String::from("type mismatch")),
    ];
    assert_eq!(
        format_diagnostics(&diags),
        Some(String::from(
            "Error: type mismatch\n\nWarning: unused variable"
        )),
        "errors must come before warnings and each line is labelled by severity"
    );
    assert_eq!(
        format_diagnostics(&[]),
        None,
        "no diagnostics must produce no block"
    );
}

#[test]
fn compose_hover_puts_the_diagnostic_above_the_type_info() {
    // VS Code (PR microsoft/vscode#166560) renders marker hovers on top.
    assert_eq!(
        compose_hover(Some("Error: type mismatch"), Some("const x: number")),
        Some(String::from(
            "Error: type mismatch\n────────\nconst x: number"
        )),
        "the diagnostic block sits above the language hover, divided by a rule"
    );
    assert_eq!(
        compose_hover(Some("Error: type mismatch"), None),
        Some(String::from("Error: type mismatch")),
        "a diagnostic with no type info shows on its own"
    );
    assert_eq!(
        compose_hover(None, Some("const x: number")),
        Some(String::from("const x: number")),
        "type info with no diagnostic shows on its own"
    );
    assert_eq!(
        compose_hover(None, None),
        None,
        "nothing to show means no popup"
    );
}

#[test]
fn hovering_a_diagnostic_on_punctuation_shows_its_message_synchronously() {
    use crate::lsp::manager::{Diagnostic, DiagnosticSeverity};
    use ratatui::layout::Rect;
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("dot.ts");
    std::fs::write(&f, "a.b\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Stand the editor up directly: the `.` at char 1 is not a word, so this
    // path never depends on a language server being attached.
    app.editor.path = Some(f.clone());
    app.editor.lines = vec![String::from("a.b")];
    app.editor.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    app.editor.last_gutter_width = 2;
    app.editor.apply_diagnostics(
        f,
        vec![Diagnostic {
            start_line: 0,
            start_char: 1,
            end_line: 0,
            end_char: 2,
            severity: DiagnosticSeverity::Error,
            message: String::from("unexpected token"),
            tags: Default::default(),
            code: None,
        }],
    );
    // text_x = x(0) + gutter(2) + 1 = 3, so cell col 4 is char 1 (the `.`).
    app.hover_at_cell(4, 0);
    let popup = app
        .hover_popup
        .as_ref()
        .expect("hovering a squiggle must surface a popup even without an LSP reply");
    assert_eq!(
        popup.lines,
        vec![String::from("Error: unexpected token")],
        "the popup must carry the diagnostic's severity and message"
    );
}

#[test]
fn the_swallowed_prefix_click_dismisses_the_hover_and_tab_tooltip() {
    // The sibling test above binds `ctrl+click` to a real command, so it takes
    // the MATCHED branch. The swallow branch needs a gesture that matches
    // NOTHING but is the first half of a bound double — a different arm, with
    // its own early return, which for a while did none of the teardown.
    //
    // Deleting `dismiss_click_overlays` from the swallow branch leaves the
    // whole suite green without this test, so the code was correct today and
    // unguarded tomorrow.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp) = tree_app_with_keymap(
        r#"[{"key": "ctrl+double_click", "command": "quick_open", "when": "editor"}]"#,
    );
    app.hover_popup = Some(crate::widgets::hover_popup::HoverPopup::new(
        String::from("stale hover"),
        (1, 1),
    ));
    app.tab_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new(
        String::from("stale tab tooltip"),
        (1, 1),
    ));

    let mut ev = mouse(
        MouseEventKind::Down(MouseButton::Left),
        app.editor.last_inner.x + 4,
        app.editor.last_inner.y + 1,
    );
    ev.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ev);

    // Proves this took the SWALLOW path, not the matched one: only
    // `ctrl+double_click` is bound, so a single click matches no command.
    // Without this the test could pass via the sibling branch's teardown and
    // say nothing about the swallow.
    assert!(
        app.file_finder.is_none(),
        "the prefix click must NOT fire the bound double's command — if it \
         did, this exercises the matched branch and not the swallow"
    );
    assert!(
        app.hover_popup.is_none(),
        "a swallowed prefix click must dismiss the hover it painted over, \
         exactly as the matched branch does: it re-focuses the pane underneath"
    );
    assert!(app.tab_tooltip.is_none(), "and the tab tooltip with it");
}

#[test]
fn tap_in_the_editor_arms_the_hover_dwell_and_release_keeps_it_armed() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, col, row) = app_with_open_file_and_editor_cell();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert_eq!(
        app.hover.cell(),
        Some((col, row)),
        "a tap is the touch equivalent of the pointer arriving: it must arm \
         the dwell at the tapped cell (a touchscreen never emits Moved events)"
    );
    assert!(
        app.hover
            .due(std::time::Instant::now() + HOVER_DELAY, HOVER_DELAY),
        "the tap-armed dwell must come due HOVER_DELAY after the press"
    );
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), col, row));
    assert_eq!(
        app.hover.cell(),
        Some((col, row)),
        "lifting the finger must not cancel the pending dwell, or a quick \
         tap could never fire"
    );
}

#[test]
fn drag_and_scroll_cancel_a_tap_armed_hover_dwell() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, col, row) = app_with_open_file_and_editor_cell();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), col + 3, row));
    assert_eq!(
        app.hover.cell(),
        None,
        "a drag is a selection, not a rest: it must cancel the pending dwell"
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    app.handle_mouse(mouse(MouseEventKind::ScrollDown, col, row));
    assert_eq!(
        app.hover.cell(),
        None,
        "scrolling moves the text under the tapped cell; the dwell must cancel"
    );
}

#[test]
fn press_and_hold_hover_popup_survives_the_finger_release() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, col, row) = app_with_open_file_and_editor_cell();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    // The dwell fired while the finger was still down (press-and-hold).
    app.hover_popup = Some(crate::widgets::hover_popup::HoverPopup::new(
        String::from("int"),
        (col, row),
    ));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), col, row));
    assert!(
        app.hover_popup.is_some(),
        "releasing the press must not dismiss the popup it just summoned"
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert!(
        app.hover_popup.is_none(),
        "the next tap is a new gesture and dismisses the previous popup"
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

/// #1561: Go to Definition from a preview tab opened the target in that
/// same preview slot, closing the file being read, and Go Back swapped them
/// again. A code navigation now keeps the tab it starts from open (VS
/// Code's default), so after F12 the call and the definition are both tabs.
#[test]
fn go_to_definition_from_a_preview_tab_keeps_that_file_open() {
    let (_tmp, mut app, mc, main, mh) = preview_nav_fixture();
    app.editor.open_preview(&main).unwrap();
    assert!(app.editor.is_preview(app.editor.active_index()));
    app.editor.cursor_row = 1;

    app.go_to_definition(mh.clone(), 1, 4);
    assert_eq!(app.editor.path.as_deref(), Some(mh.as_path()));
    assert_eq!(
        open_tab_paths(&app),
        vec![mc.clone(), main.clone(), mh.clone()],
        "the file the jump started from is still open"
    );
    let main_idx = open_tab_paths(&app)
        .iter()
        .position(|p| *p == main)
        .unwrap();
    assert!(
        !app.editor.is_preview(main_idx),
        "and no longer the preview a later open would replace"
    );

    app.nav_back();
    assert_eq!(app.editor.path.as_deref(), Some(main.as_path()));
    assert_eq!(
        app.editor.cursor_row, 1,
        "back lands where the jump started"
    );
    assert_eq!(
        open_tab_paths(&app),
        vec![mc, main, mh],
        "and Go Back does not close the definition"
    );
}

/// A definition whose target cannot be opened (a stale language-server
/// answer) leaves the user where they were, so the tab the jump started
/// from stays the preview it was rather than being pinned for nothing.
#[test]
fn a_failed_definition_jump_leaves_the_preview_tab_a_preview() {
    let (tmp, mut app, mc, main, _mh) = preview_nav_fixture();
    app.editor.open_preview(&main).unwrap();
    let gone = tmp.path().join("gone_dir");
    std::fs::create_dir(&gone).unwrap();
    app.go_to_definition(gone, 0, 0);
    assert_eq!(app.editor.path.as_deref(), Some(main.as_path()));
    assert_eq!(open_tab_paths(&app), vec![mc, main]);
    assert!(
        app.editor.is_preview(app.editor.active_index()),
        "no navigation happened, so the tab is still the preview"
    );
}

/// A tab opened through a symlink and a language server's canonical path
/// for the same file are one file: the jump stays inside the tab, so it
/// stays a preview.
#[cfg(unix)]
#[test]
fn a_definition_at_the_canonical_path_of_a_symlinked_preview_stays_a_preview() {
    let (tmp, mut app, mc, main, _mh) = preview_nav_fixture();
    let link = tmp.path().join("link.c");
    std::os::unix::fs::symlink(&main, &link).unwrap();
    app.editor.open_preview(&link).unwrap();
    app.go_to_definition(main.canonicalize().unwrap(), 1, 0);
    assert_eq!(app.editor.cursor_row, 1);
    assert_eq!(open_tab_paths(&app), vec![mc, link]);
    assert!(app.editor.is_preview(app.editor.active_index()));
}

/// Negative: a jump inside the same file (an outline entry, a local
/// definition) moves the caret without promoting the preview tab, so the
/// next single-click open still replaces it.
#[test]
fn a_definition_in_the_same_file_leaves_the_preview_tab_a_preview() {
    let (_tmp, mut app, mc, main, mh) = preview_nav_fixture();
    app.editor.open_preview(&main).unwrap();
    app.go_to_definition(main.clone(), 1, 0);
    assert_eq!(app.editor.cursor_row, 1);
    assert!(app.editor.is_preview(app.editor.active_index()));
    app.editor.open_preview(&mh).unwrap();
    assert_eq!(
        open_tab_paths(&app),
        vec![mc, mh],
        "the preview slot is still reused"
    );
}

#[test]
fn cmd_dot_and_ctrl_dot_are_quick_fix_key() {
    // VS Code binds Quick Fix to Cmd+. on macOS (arrives as Char('.')+SUPER via
    // the iTerm2/Ghostty CSI-u forwarder) and Ctrl+. on Linux/Termux.
    assert!(is_quick_fix_key(key(
        KeyCode::Char('.'),
        KeyModifiers::SUPER
    )));
    assert!(is_quick_fix_key(key(
        KeyCode::Char('.'),
        KeyModifiers::CONTROL
    )));
}

#[test]
fn plain_dot_and_modified_dot_are_not_quick_fix_key() {
    assert!(!is_quick_fix_key(key(
        KeyCode::Char('.'),
        KeyModifiers::NONE
    )));
    // Shift+. is '>' territory and Alt+. is the shell's last-arg; neither is
    // Quick Fix.
    assert!(!is_quick_fix_key(key(
        KeyCode::Char('.'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
    assert!(!is_quick_fix_key(key(
        KeyCode::Char('.'),
        KeyModifiers::ALT
    )));
}

#[test]
fn changing_workspace_root_rebinds_the_lsp_manager_to_the_new_root() {
    let tmp = tempfile::tempdir().unwrap();
    let inner = tmp.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Pretend a file from the old root is already open with the LSP so we can
    // prove the seen-map is reset (otherwise sync_lsp would never re-open it
    // against the new server).
    app.lsp_last_seen.insert(tmp.path().join("stale.rs"), 7);

    app.change_workspace_root(inner.clone());

    assert_eq!(
        app.lsp.as_ref().unwrap().workspace_root(),
        inner,
        "rust-analyzer's workspace folder must follow the Explorer root; left at the launch dir (e.g. ~/Documents, which has no Cargo.toml) every opened .rs comes back as an unlinked-file with no IDE services"
    );
    assert!(
        app.lsp_last_seen.is_empty(),
        "the LSP seen-map must be cleared on re-root so the next sync_lsp re-opens every live editor tab against the freshly-rooted server"
    );
}

/// #1618 negative: in a text buffer F2 is still Rename Symbol, and
/// Ctrl+F2 in the grid still does not open the cell editor.
#[test]
fn f2_outside_the_grid_is_still_rename_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.txt", "Lee\n");
    app.status.clear();
    app.handle_editor_key(key(KeyCode::F(2), KeyModifiers::NONE));
    assert!(
        app.status == "No language server for this file" || app.status == "Preparing rename…",
        "F2 went to Rename Symbol: {:?}",
        app.status
    );
    assert_eq!(app.editor.lines[0], "Lee");

    let p = tmp.path().join("people.csv");
    std::fs::write(&p, "id,name\n2,Lee\n").unwrap();
    app.editor.open(&p).unwrap();
    app.handle_editor_key(key(KeyCode::F(2), KeyModifiers::CONTROL));
    assert!(
        app.editor.sheet.as_ref().unwrap().editing.is_none(),
        "Ctrl+F2 is not the grid's edit key"
    );
}

/// Guard (#843): "Go to Implementations" with no file open sends nothing,
/// as Cmd+F12 would not.
#[test]
fn go_to_implementations_without_a_file_sends_nothing() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(Command::GoToImplementations);
    assert!(app.implementation_request_id.is_none());
}

/// The accept shifts the server's ranges by what was typed or deleted since
/// the request, applies the auto-import in the request's coordinates, is one
/// undo step, and credits the text to the server (#349).
#[test]
fn a_completion_accept_tracks_the_caret_and_is_one_generated_undo_step() {
    use crate::widgets::editor::TextSpanEdit;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Requested at `abc|)`, then one Backspace: `ab|)`.
    app.editor.lines = vec![String::from("ab)")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 2;
    app.completion_origin = Some((0, 3));
    let item = crate::lsp::CompletionItem {
        label: String::from("abcdef"),
        text_edit: Some(TextSpanEdit {
            start: (0, 0),
            end: (0, 3),
            new_text: String::from("abcdef"),
            utf16: true,
        }),
        ..Default::default()
    };
    assert!(app.accept_completion_edit(&item));
    assert_eq!(app.editor.lines, vec![String::from("abcdef)")]);
    assert_eq!(app.editor.cursor_col, 6);
    // A same-row additional edit lands where the server meant it, and the
    // whole accept is one undo step credited to the server.
    app.editor.lines = vec![String::from("x = fo; y"), String::from("z")];
    app.editor.provenance = crate::provenance::Provenance::new();
    app.editor.cursor_col = 6;
    app.completion_origin = Some((0, 6));
    let before = app.editor.lines.clone();
    let item = crate::lsp::CompletionItem {
        label: String::from("foobar"),
        text_edit: Some(TextSpanEdit {
            start: (0, 4),
            end: (0, 6),
            new_text: String::from("foobar"),
            utf16: true,
        }),
        additional_edits: vec![TextSpanEdit {
            start: (0, 9),
            end: (0, 9),
            new_text: String::from("Z"),
            utf16: true,
        }],
        ..Default::default()
    };
    assert!(app.accept_completion_edit(&item));
    assert_eq!(app.editor.lines[0], "x = foobar; yZ");
    assert_eq!(app.editor.cursor_col, 10);
    assert_eq!(
        app.editor.provenance.seat(0),
        Some(&crate::provenance::Seat::Generated)
    );
    app.editor.undo();
    assert_eq!(app.editor.lines, before, "one accept is one undo step");
}

/// A completion reply opens only while the caret is still in the word it was
/// asked for. One that landed after Enter opened on the next line with an
/// empty prefix, and the next Enter inserted its first item (#842).
#[test]
fn a_late_completion_reply_opens_only_while_the_caret_is_in_its_word() {
    use crate::lsp::manager::CompletionResult;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.txt");
    std::fs::write(&path, "import lo").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&path).unwrap();
    app.focus_pane(Pane::Editor);
    let reply = |request_id| CompletionResult {
        request_id,
        is_incomplete: false,
        path: path.clone(),
        items: vec![crate::lsp::CompletionItem {
            label: String::from("load"),
            ..Default::default()
        }],
    };
    let ask_at = |app: &mut App, id, col| {
        app.editor.cursor_row = 0;
        app.editor.cursor_col = col;
        app.completion_request_id = Some(id);
        app.completion_origin = Some((0, col));
    };
    let press = |app: &mut App, code| app.handle_key(key(code, KeyModifiers::NONE)).unwrap();

    // Asked at `import lo|`, then Enter before the reply landed.
    ask_at(&mut app, 1, 9);
    press(&mut app, KeyCode::Enter);
    draw(&mut app, 100, 30);
    assert!(!app.apply_completion_result(reply(1)));
    assert!(app.completion_popup.is_none(), "no popup on the next line");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.editor.lines,
        ["import lo", "", ""],
        "Enter breaks the line"
    );

    // A letter typed since keeps the caret in the word: the reply opens.
    app.editor.lines = vec![String::from("import lo")];
    ask_at(&mut app, 2, 9);
    press(&mut app, KeyCode::Char('a'));
    draw(&mut app, 100, 30);
    assert!(app.apply_completion_result(reply(2)));
    assert_eq!(
        app.completion_popup.take().map(|p| p.prefix),
        Some(String::from("loa"))
    );

    // Backspaced left of the request point, or a non-word character typed
    // since: the caret left the word, and the reply is dropped.
    ask_at(&mut app, 3, 10);
    press(&mut app, KeyCode::Backspace);
    draw(&mut app, 100, 30);
    assert!(!app.apply_completion_result(reply(3)));
    ask_at(&mut app, 4, 9);
    press(&mut app, KeyCode::Char(' '));
    draw(&mut app, 100, 30);
    assert!(!app.apply_completion_result(reply(4)));
    assert!(app.completion_popup.is_none());

    // A reply for another buffer never opens in this one.
    ask_at(&mut app, 5, 9);
    assert!(!app.apply_completion_result(CompletionResult {
        path: tmp.path().join("other.txt"),
        ..reply(5)
    }));
    assert!(app.completion_popup.is_none());
}

/// TypeScript on a possibly-undefined object: vtsls's member items replace
/// the `.` with `?.`, so their filter text starts at the dot. Typing a
/// letter filtered them on the bare word and hid every member (#1472).
#[test]
fn optional_members_survive_the_first_typed_letter() {
    use crate::lsp::manager::CompletionResult;
    use crate::widgets::editor::TextSpanEdit;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("opt.ts");
    std::fs::write(&path, "user.n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&path).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 6;
    app.completion_request_id = Some(1);
    app.completion_origin = Some((0, 6));
    draw(&mut app, 100, 30);
    let member = |name: &str| crate::lsp::CompletionItem {
        label: name.to_string(),
        filter_text: Some(format!(".?.{name}")),
        text_edit: Some(TextSpanEdit {
            start: (0, 4),
            end: (0, 6),
            new_text: format!("?.{name}"),
            utf16: true,
        }),
        ..Default::default()
    };
    assert!(app.apply_completion_result(CompletionResult {
        request_id: 1,
        is_incomplete: false,
        path: path.clone(),
        items: vec![member("email"), member("name")],
    }));
    let popup = app.completion_popup.as_ref().expect("the popup opens");
    assert_eq!(
        popup.selected_item().map(|i| i.label.as_str()),
        Some("name"),
        "status: {}",
        app.status
    );
    // Typing on narrows it the same way, and Backspace widens it again.
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    let popup = app
        .completion_popup
        .as_ref()
        .expect("still open after `na`");
    assert_eq!(popup.visible_indices().len(), 1);
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    let popup = app.completion_popup.as_ref().expect("still open after `.`");
    assert_eq!(popup.visible_indices().len(), 2);
    // Enter takes the server's edit: the `.` becomes `?.`.
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, ["user?.name"]);
}

/// A buffer holding `import lo` with the caret at its end, and a sender that
/// lands completion replies where a server's do, so a test can deliver one
/// at the moment a slow server would: after the keys typed since the ask.
fn completion_reply_app() -> (
    tempfile::TempDir,
    PathBuf,
    App,
    std::sync::mpsc::Sender<crate::lsp::manager::CompletionResult>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.txt");
    std::fs::write(&path, "import lo").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&path).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 9;
    let tx = app
        .lsp
        .as_mut()
        .expect("a test-built App runs a manager")
        .completion_reply_sender_for_test();
    (tmp, path, app, tx)
}

/// #1529: a reply the server marked incomplete, offering `labels`.
fn incomplete_reply(
    request_id: u64,
    path: &Path,
    labels: &[&str],
) -> crate::lsp::manager::CompletionResult {
    crate::lsp::manager::CompletionResult {
        is_incomplete: true,
        ..completion_reply(request_id, path, labels)
    }
}

/// #1529: clangd caps its list at 100 and marks it incomplete. Typing on
/// past the first list's names must ask the server again, not close the
/// popup: `lo` gave `load` only, `lox` has to reach the server's `loxodrome`.
#[test]
fn typing_into_an_incomplete_list_asks_the_server_again() {
    let (_tmp, path, mut app, tx) = completion_reply_app();
    app.trigger_completion();
    let first = app.completion_request_id.expect("the ask went out");
    draw(&mut app, 100, 30);
    tx.send(incomplete_reply(first, &path, &["load"])).unwrap();
    assert!(app.drain_lsp_completion());
    assert!(app.completion_popup.is_some());
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    sync_then_reask(&mut app);
    let again = app
        .completion_request_id
        .expect("an incomplete list keeps a request in flight");
    assert_ne!(again, first, "typing into an incomplete list re-asks");
    draw(&mut app, 100, 30);
    tx.send(incomplete_reply(again, &path, &["loxodrome"]))
        .unwrap();
    assert!(app.drain_lsp_completion());
    let popup = app.completion_popup.as_ref().expect("the new list opens");
    assert_eq!(popup.prefix, "lox");
    let labels: Vec<&str> = popup
        .visible_indices()
        .into_iter()
        .map(|i| popup.items[i].label.as_str())
        .collect();
    assert_eq!(labels, ["loxodrome"]);
}

/// #1529: while the re-ask is out, the old list's matches stay on screen,
/// and a stale reply to the first ask can't replace the newer one.
#[test]
fn an_incomplete_list_keeps_its_matches_until_the_new_reply_lands() {
    let (_tmp, path, mut app, tx) = completion_reply_app();
    app.trigger_completion();
    let first = app.completion_request_id.unwrap();
    draw(&mut app, 100, 30);
    tx.send(incomplete_reply(first, &path, &["load", "local"]))
        .unwrap();
    app.drain_lsp_completion();
    app.handle_key(key(KeyCode::Char('c'), KeyModifiers::NONE))
        .unwrap();
    let popup = app
        .completion_popup
        .as_ref()
        .expect("`local` still matches");
    assert_eq!(popup.visible_indices().len(), 1);
    sync_then_reask(&mut app);
    // The first ask answering late is dropped by id.
    tx.send(incomplete_reply(first, &path, &["stale"])).unwrap();
    assert!(!app.drain_lsp_completion());
    let again = app.completion_request_id.unwrap();
    assert_ne!(again, first);
}

/// #1529 negative: a complete list is only filtered. No second request
/// goes out, and when nothing matches the popup closes, as before.
#[test]
fn typing_into_a_complete_list_only_filters_it() {
    let (_tmp, path, mut app, tx) = completion_reply_app();
    app.trigger_completion();
    let first = app.completion_request_id.unwrap();
    draw(&mut app, 100, 30);
    tx.send(completion_reply(first, &path, &["load", "local"]))
        .unwrap();
    app.drain_lsp_completion();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    sync_then_reask(&mut app);
    assert_eq!(app.completion_request_id, Some(first), "no re-ask");
    assert!(app.completion_popup.is_some());
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.completion_popup.is_none(), "nothing matches `loax`");
    assert_eq!(app.completion_request_id, None);
}

/// #1529 negative: leaving the word (a space) ends an incomplete list's
/// session too; no re-ask follows the caret out of the word.
#[test]
fn leaving_the_word_ends_an_incomplete_list() {
    let (_tmp, path, mut app, tx) = completion_reply_app();
    app.trigger_completion();
    let first = app.completion_request_id.unwrap();
    draw(&mut app, 100, 30);
    tx.send(incomplete_reply(first, &path, &["load"])).unwrap();
    app.drain_lsp_completion();
    app.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE))
        .unwrap();
    sync_then_reask(&mut app);
    assert!(app.completion_popup.is_none());
    assert_eq!(app.completion_request_id, None);
}

/// One event-loop tick's LSP steps: the edit's `didChange`, then the re-ask
/// an incomplete list left due (#1529).
fn sync_then_reask(app: &mut App) {
    app.sync_lsp();
    app.send_due_completion_reask();
}

/// #1529: when a keystroke filters out every item of an incomplete list,
/// the popup closes but the session stays: the next keystroke, typed before
/// the re-ask answers, asks again for the newer word.
#[test]
fn an_emptied_incomplete_list_still_re_asks_on_the_next_keystroke() {
    let (_tmp, path, mut app, tx) = completion_reply_app();
    app.trigger_completion();
    let first = app.completion_request_id.unwrap();
    draw(&mut app, 100, 30);
    tx.send(incomplete_reply(first, &path, &["load", "local"]))
        .unwrap();
    app.drain_lsp_completion();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.completion_popup.is_none(), "nothing matches `lox`");
    sync_then_reask(&mut app);
    let second = app.completion_request_id.expect("re-asked for `lox`");
    assert_ne!(second, first);
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::NONE))
        .unwrap();
    sync_then_reask(&mut app);
    let third = app.completion_request_id.expect("re-asked for `loxo`");
    assert_ne!(third, second, "a keystroke with no popup still re-asks");
    draw(&mut app, 100, 30);
    tx.send(incomplete_reply(third, &path, &["loxodrome"]))
        .unwrap();
    assert!(app.drain_lsp_completion());
    let popup = app.completion_popup.as_ref().expect("the new list opens");
    assert_eq!(popup.prefix, "loxo");
}

/// #1529: the re-ask waits for the tick's `sync_lsp`, so the server has the
/// edit's `didChange` before it is asked about the new caret position.
#[test]
fn an_incomplete_re_ask_waits_until_the_edit_is_synced() {
    let (_tmp, path, mut app, tx) = completion_reply_app();
    app.trigger_completion();
    let first = app.completion_request_id.unwrap();
    draw(&mut app, 100, 30);
    tx.send(incomplete_reply(first, &path, &["load"])).unwrap();
    app.drain_lsp_completion();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.completion_request_id,
        Some(first),
        "no request goes out from the key handler"
    );
    app.send_due_completion_reask();
    let again = app.completion_request_id.unwrap();
    assert_ne!(again, first, "one re-ask covers both keystrokes");
    app.send_due_completion_reask();
    assert_eq!(
        app.completion_request_id,
        Some(again),
        "nothing more is due"
    );
}

/// One completion reply for `path`, offering each of `labels`.
fn completion_reply(
    request_id: u64,
    path: &Path,
    labels: &[&str],
) -> crate::lsp::manager::CompletionResult {
    crate::lsp::manager::CompletionResult {
        request_id,
        is_incomplete: false,
        path: path.to_path_buf(),
        items: labels
            .iter()
            .map(|label| crate::lsp::CompletionItem {
                label: (*label).to_string(),
                ..Default::default()
            })
            .collect(),
    }
}

/// #1566: an app with `files` written and opened in order (the last one
/// active), the caret at the end of the active file's last line, drawn once
/// so a popup has somewhere to open.
fn word_completion_app(files: &[(&str, &str)]) -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for (name, text) in files {
        let path = tmp.path().join(name);
        std::fs::write(&path, text).unwrap();
        app.editor.open_pinned(&path).unwrap();
    }
    app.focus_pane(Pane::Editor);
    let row = app.editor.lines.len() - 1;
    app.editor.cursor_row = row;
    app.editor.cursor_col = app.editor.lines[row].chars().count();
    draw(&mut app, 100, 30);
    (tmp, app)
}

fn popup_labels(app: &App) -> Vec<String> {
    app.completion_popup.as_ref().map_or_else(Vec::new, |p| {
        p.visible_indices()
            .into_iter()
            .map(|i| p.items[i].label.clone())
            .collect()
    })
}

#[test]
fn ctrl_space_in_a_file_no_server_takes_offers_the_words_already_in_it() {
    let (_tmp, mut app) = word_completion_app(&[("notes.txt", "configuration loader\nconfigu")]);
    app.handle_key(key(KeyCode::Char(' '), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(
        popup_labels(&app),
        ["configuration"],
        "not `configu` itself"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, ["configuration loader", "configuration"]);
}

#[test]
fn a_makefile_variable_completes_from_its_definition() {
    let (_tmp, mut app) = word_completion_app(&[(
        "Makefile",
        "BUILD_DIRECTORY := out\n\nall:\n\tmkdir -p $(BUILD_DIRECTORY)\n\techo $(BUI",
    )]);
    app.trigger_completion();
    assert_eq!(popup_labels(&app), ["BUILD_DIRECTORY"]);
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines[4], "\techo $(BUILD_DIRECTORY");
}

#[test]
fn word_suggestions_draw_on_the_open_documents_the_setting_names() {
    use crate::word_suggest::WordSuggestions;
    let (_tmp, mut app) = word_completion_app(&[
        ("animals.txt", "elephant"),
        ("lift.py", "elevator = 1\n"),
        ("todo.txt", "elegant\nele"),
    ]);
    let mut offered = |mode| {
        app.word_suggestions = mode;
        app.completion_popup = None;
        app.trigger_completion();
        popup_labels(&app)
    };
    assert_eq!(
        offered(WordSuggestions::MatchingDocuments),
        ["elegant", "elephant"],
        "the other plain-text file, not the Python one"
    );
    assert_eq!(offered(WordSuggestions::CurrentDocument), ["elegant"]);
    assert_eq!(
        offered(WordSuggestions::AllDocuments),
        ["elegant", "elephant", "elevator"]
    );
    assert!(offered(WordSuggestions::Off).is_empty());
}

#[test]
fn word_suggestions_draw_on_documents_open_in_other_split_groups() {
    let (tmp, mut app) = word_completion_app(&[("animals.txt", "elephant")]);
    app.split_editor();
    let todo = tmp.path().join("todo.txt");
    std::fs::write(&todo, "elegant\nele").unwrap();
    app.editor.open_pinned(&todo).unwrap();
    let active = app.editor.active_index();
    app.editor.close_others(active);
    assert!(
        app.editor
            .editors
            .iter()
            .all(|e| e.path.as_deref() == Some(todo.as_path())),
        "animals.txt is open only in the other group"
    );
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 3;
    app.focus_pane(Pane::Editor);
    draw(&mut app, 100, 30);
    app.trigger_completion();
    assert_eq!(popup_labels(&app), ["elegant", "elephant"]);
}

#[test]
fn with_word_suggestions_off_ctrl_space_offers_nothing() {
    let (_tmp, mut app) = word_completion_app(&[("notes.txt", "configuration loader\nconfigu")]);
    app.word_suggestions = crate::word_suggest::WordSuggestions::Off;
    app.trigger_completion();
    assert!(app.completion_popup.is_none());
    assert_ne!(app.status, "No suggestions");
}

#[test]
fn ctrl_space_with_no_word_to_offer_says_so() {
    let (_tmp, mut app) = word_completion_app(&[("notes.txt", "alpha\nzz")]);
    app.trigger_completion();
    assert!(app.completion_popup.is_none());
    assert_eq!(app.status, "No suggestions");
}

#[test]
fn a_typed_full_stop_does_not_open_the_word_list() {
    // `.` asks a server for members; with none, a word list on every full
    // stop of a note would only get in the way.
    let (_tmp, mut app) = word_completion_app(&[("notes.txt", "configuration\nconfig")]);
    app.handle_key(key(KeyCode::Char('.'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.completion_popup.is_none());
    assert_eq!(app.editor.lines[1], "config.");
}

#[test]
fn a_server_s_own_items_are_not_mixed_with_words() {
    let (_tmp, mut app) = word_completion_app(&[("notes.txt", "load_words\nlo")]);
    let path = app.editor.path.clone().unwrap();
    let lsp = app.lsp.as_mut().unwrap();
    let tx = lsp.completion_reply_sender_for_test();
    lsp.set_document_servers_for_test(&path, &["srv"]);
    app.trigger_completion();
    assert!(
        app.completion_popup.is_none(),
        "a file a server has waits for the server's answer"
    );
    let id = app.completion_request_id.expect("the server was asked");
    tx.send(completion_reply(id, &path, &["load_from_server"]))
        .unwrap();
    assert!(app.drain_lsp_completion());
    assert_eq!(popup_labels(&app), ["load_from_server"]);
}

#[test]
fn a_server_with_nothing_to_offer_leaves_the_words() {
    let (_tmp, mut app) = word_completion_app(&[("notes.txt", "load_words\nlo")]);
    let path = app.editor.path.clone().unwrap();
    let lsp = app.lsp.as_mut().unwrap();
    let tx = lsp.completion_reply_sender_for_test();
    lsp.set_document_servers_for_test(&path, &["srv"]);
    app.trigger_completion();
    let id = app.completion_request_id.unwrap();
    tx.send(completion_reply(id, &path, &[])).unwrap();
    assert!(app.drain_lsp_completion());
    assert_eq!(popup_labels(&app), ["load_words"]);
}

#[test]
fn a_late_server_list_replaces_the_words_and_an_empty_one_keeps_them() {
    // No server had the file when Ctrl+Space was pressed (one still
    // starting, say): the words open at once. An empty reply then keeps the
    // user's place in them; a real list replaces them.
    let (_tmp, mut app) = word_completion_app(&[("notes.txt", "load_a load_b\nlo")]);
    let path = app.editor.path.clone().unwrap();
    let tx = app.lsp.as_mut().unwrap().completion_reply_sender_for_test();
    app.trigger_completion();
    assert_eq!(popup_labels(&app), ["load_a", "load_b"]);
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    let id = app.completion_request_id.unwrap();
    tx.send(completion_reply(id, &path, &[])).unwrap();
    app.drain_lsp_completion();
    assert_eq!(app.completion_popup.as_ref().unwrap().selected, 1);
    tx.send(completion_reply(id, &path, &["load_server"]))
        .unwrap();
    assert!(app.drain_lsp_completion());
    assert_eq!(popup_labels(&app), ["load_server"]);
}

/// #842 through the path a real reply takes: asked at `import lo|`, Enter
/// pressed before the reply landed. The reply must not open a popup on the
/// new line, where the next Enter accepted its first item (`load`) instead
/// of breaking the line.
#[test]
fn a_completion_reply_landing_after_enter_leaves_the_next_enter_to_break_the_line() {
    let (_tmp, path, mut app, tx) = completion_reply_app();
    app.trigger_completion();
    let id = app.completion_request_id.expect("the ask went out");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    draw(&mut app, 100, 30);
    tx.send(completion_reply(id, &path, &["load"])).unwrap();
    app.drain_lsp_completion();
    assert!(
        app.completion_popup.is_none(),
        "a reply for the word left behind opened on the next line"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.lines,
        ["import lo", "", ""],
        "the second Enter breaks the line, it does not accept `load`"
    );
}

/// Guard (#842): the common case, a reply that lands with the caret exactly
/// where it was asked (nothing typed since), still opens. The fix drops
/// replies once the caret LEFT the word; it must not drop this boundary.
#[test]
fn a_completion_reply_with_the_caret_where_it_was_asked_still_opens() {
    let (_tmp, path, mut app, tx) = completion_reply_app();
    app.trigger_completion();
    let id = app.completion_request_id.expect("the ask went out");
    draw(&mut app, 100, 30);
    tx.send(completion_reply(id, &path, &["load"])).unwrap();
    assert!(app.drain_lsp_completion());
    assert_eq!(
        app.completion_popup.as_ref().map(|p| p.prefix.as_str()),
        Some("lo")
    );
}

/// Guard (#842): non-ASCII letters and `_` typed since the ask keep the caret
/// in the word, so the reply still opens. The caret is a char index, and a
/// byte-based check (or an ASCII-only word test) would drop it.
#[test]
fn a_completion_reply_still_opens_after_non_ascii_word_chars_typed_since() {
    let (_tmp, path, mut app, tx) = completion_reply_app();
    app.trigger_completion();
    let id = app.completion_request_id.expect("the ask went out");
    for c in ['ö', '_'] {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    draw(&mut app, 100, 30);
    tx.send(completion_reply(id, &path, &["loö_value"]))
        .unwrap();
    assert!(app.drain_lsp_completion());
    assert_eq!(
        app.completion_popup.as_ref().map(|p| p.prefix.as_str()),
        Some("loö_")
    );
}

/// Guard (#842): a stale reply (an earlier request's id) is skipped without
/// cancelling the request still pending, so the fresh reply behind it opens.
/// Dropping a reply clears the pending id only when the caret left the word
/// it was asked for; a stale id must not reach that path first.
#[test]
fn a_stale_completion_reply_does_not_cancel_the_pending_one() {
    let (_tmp, path, mut app, tx) = completion_reply_app();
    app.trigger_completion();
    let stale = app.completion_request_id.expect("the first ask went out");
    app.trigger_completion();
    let fresh = app.completion_request_id.expect("the second ask went out");
    assert_ne!(stale, fresh);
    draw(&mut app, 100, 30);
    tx.send(completion_reply(stale, &path, &["lost"])).unwrap();
    tx.send(completion_reply(fresh, &path, &["load"])).unwrap();
    assert!(app.drain_lsp_completion());
    let popup = app
        .completion_popup
        .as_ref()
        .expect("the fresh reply opens");
    assert_eq!(popup.request_id, fresh);
    assert_eq!(
        popup
            .items
            .iter()
            .map(|i| i.label.as_str())
            .collect::<Vec<_>>(),
        ["load"]
    );
}

/// A completion is accepted by the server's own edit: TypeScript's `?.foo`
/// replaces the `.` before the word (croft produced `a.?.foo`), letters
/// typed since the request go too, and an auto-import lands with it.
#[test]
fn a_completion_applies_its_text_edit_and_additional_edits() {
    use crate::widgets::editor::TextSpanEdit;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.lines = vec![String::from("a.f")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 3;
    let item = crate::lsp::CompletionItem {
        label: String::from("foo"),
        // Requested at `a.` (col 2); `f` was typed since.
        text_edit: Some(TextSpanEdit {
            start: (0, 1),
            end: (0, 2),
            new_text: String::from("?.foo"),
            utf16: true,
        }),
        additional_edits: vec![TextSpanEdit {
            start: (0, 0),
            end: (0, 0),
            new_text: String::from("import { x } from 'x';\n"),
            utf16: true,
        }],
        ..Default::default()
    };
    assert!(app.accept_completion_edit(&item));
    assert_eq!(
        app.editor.lines,
        vec![
            String::from("import { x } from 'x';"),
            String::from("a?.foo")
        ]
    );
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 6));
    // No text edit: the prefix path takes over.
    assert!(!app.accept_completion_edit(&crate::lsp::CompletionItem::default()));
    // A range past the caret replaces the suffix it covers.
    app.editor.lines = vec![String::from("fooBar")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 3;
    let item = crate::lsp::CompletionItem {
        label: String::from("baz"),
        text_edit: Some(TextSpanEdit {
            start: (0, 0),
            end: (0, 6),
            new_text: String::from("baz"),
            utf16: true,
        }),
        ..Default::default()
    };
    assert!(app.accept_completion_edit(&item));
    assert_eq!(app.editor.lines, vec![String::from("baz")]);
    // A snippet takes the same range and imports, and still expands: the
    // `.` its range covers goes, and the caret sits on the first tab stop.
    app.editor.lines = vec![String::from("a.f")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 3;
    let item = crate::lsp::CompletionItem {
        label: String::from("foo"),
        is_snippet: true,
        text_edit: Some(TextSpanEdit {
            start: (0, 1),
            end: (0, 3),
            new_text: String::from("?.foo(${1:x})$0"),
            utf16: true,
        }),
        additional_edits: vec![TextSpanEdit {
            start: (0, 0),
            end: (0, 0),
            new_text: String::from("use x;\n"),
            utf16: true,
        }],
        ..Default::default()
    };
    assert!(app.accept_completion_edit(&item));
    assert_eq!(
        app.editor.lines,
        vec![String::from("use x;"), String::from("a?.foo(x)")]
    );
    assert_eq!(app.editor.cursor_row, 1);
    assert!(app.editor.snippet_active(), "tab stops are live");
}

/// #1639: a rust-analyzer "Run Test" code lens is a task too, and saves
/// first: cargo builds the file on disk.
#[test]
fn a_code_lens_test_run_saves_the_unsaved_buffer_first() {
    let (tmp, mut app, file) = app_with_unsaved_pinned_file("lib.rs");
    let mut lens = lens_at(0, "new", "Run Test", "rust-analyzer.runSingle");
    lens.arguments = vec![serde_json::json!({
        "label": "test tests::it_works",
        "kind": "cargo",
        "args": {
            "cwd": tmp.path().canonicalize().unwrap(),
            "cargoArgs": ["test", "--lib"],
            "executableArgs": ["tests::it_works", "--exact"]
        }
    })];
    app.editor.code_lenses = vec![lens];
    app.run_code_lens(0);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\nold\n");
    assert_eq!(app.status, "Running test tests::it_works");
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
    assert_eq!(menu_label(&menu.items[0]), "foo.rs:5");
    assert!(matches!(
        menu.items[0].action(),
        Some(MenuAction::GoToLocation { path, line: 4, col: 2 }) if path == &a
    ));
    assert_eq!(
        menu_label(&menu.items[1]),
        format!("sub{}bar.rs:1", std::path::MAIN_SEPARATOR)
    );
    assert!(matches!(
        menu.items[1].action(),
        Some(MenuAction::GoToLocation { path, line: 0, col: 0 }) if path == &b
    ));
}

#[test]
fn a_syntax_parse_landing_after_the_language_servers_outline_does_not_replace_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "big.py", &python_module(20_000));
    let path = app.editor.path.clone().unwrap();
    note_edit_for_outline(&mut app);
    app.sync_outline();
    assert!(app.outline_parse.is_some(), "the parse is running");
    let mut from_server = app_outline_symbol_named("FromServer");
    from_server.range_end_line = 40_000;
    assert!(app.apply_outline_symbols(path, app.editor.edit_seq, vec![from_server]));
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(60),
        "the off-thread outline parse",
        || {
            app.sync_outline();
            app.outline_parse.is_none()
        },
    );
    let names: Vec<_> = app
        .outline
        .symbols()
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, vec!["FromServer"]);
}

fn app_outline_symbol_named(name: &str) -> crate::lsp::manager::OutlineSymbol {
    let tmp = tempfile::tempdir().unwrap();
    let mut probe = app_with_open_file(tmp.path(), "one.py", "def one(x):\n    return x\n");
    probe.sync_outline();
    let mut symbol = probe.outline.symbols()[0].clone();
    symbol.name = name.to_string();
    symbol
}

#[test]
fn empty_lsp_reply_keeps_the_tree_sitter_outline() {
    // A server that lacks documentSymbol (or replies before indexing) sends an
    // empty outline. That must not blank symbols tree-sitter already painted.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "demo.rs", "fn only() {}\n");
    let active = app.editor.path.clone().unwrap();
    app.sync_outline();
    assert!(!app.outline.is_empty(), "tree-sitter painted the outline");
    assert!(
        !app.apply_outline_symbols(active, 0, Vec::new()),
        "an empty LSP reply is ignored when an outline already exists"
    );
    assert!(
        !app.outline.is_empty(),
        "the tree-sitter outline survives the empty reply"
    );
}

#[test]
fn diagnostics_store_applies_to_the_active_editor_on_drain() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.ts");
    std::fs::write(&file, "const x = 1;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    // Seed the store as if vtsls had published a diagnostic for this file.
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("vtsls"),
        vec![diag(6, 7, DiagnosticSeverity::Error)],
    );
    app.lsp_diagnostics.insert(file.clone(), by_server);
    // Nothing arrives on the channel, but the active file's diagnostics are
    // unapplied (a "tab switch"), so the drain must push the stored set in.
    let changed = app.drain_lsp_diagnostics();
    assert!(
        changed,
        "applying the stored diagnostics must mark a redraw"
    );
    assert_eq!(app.editor.diagnostics_path(), Some(file.as_path()));
    assert_eq!(
        app.editor.diagnostic_spans_for_test()[0],
        vec![(6, 7, DiagnosticSeverity::Error)],
    );
}

#[test]
fn merged_diagnostics_layers_every_server_for_a_file() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let file = tmp.path().join("a.py");
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("ty"),
        vec![diag(0, 1, DiagnosticSeverity::Error)],
    );
    by_server.insert(
        String::from("ruff"),
        vec![diag(2, 3, DiagnosticSeverity::Warning)],
    );
    app.lsp_diagnostics.insert(file.clone(), by_server);
    let merged = app.merged_diagnostics(&file);
    assert_eq!(
        merged.len(),
        2,
        "both servers' diagnostics must survive the merge, not clobber each other"
    );
}

#[test]
fn toggle_format_on_save_command_flips_the_pref_and_reports_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.format_on_save = false;
    app.run_command(crate::widgets::command_palette::Command::ToggleFormatOnSave);
    assert!(app.format_on_save, "the command turns format-on-save on");
    assert!(app.status.contains("on") || app.status.contains("On"));
    app.run_command(crate::widgets::command_palette::Command::ToggleFormatOnSave);
    assert!(!app.format_on_save, "the command toggles it back off");
}

#[test]
fn format_on_save_eligible_requires_pref_and_formatter_and_code_file() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("m.py");
    std::fs::write(&f, "x=1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    // Pref off: never eligible, even when a formatter is available.
    app.format_on_save = false;
    assert!(!app.format_on_save_eligible(true));
    // Pref on but no formatter advertised: not eligible (would hang the save).
    app.format_on_save = true;
    assert!(!app.format_on_save_eligible(false));
    // Pref on + formatter + a real code file: eligible.
    assert!(app.format_on_save_eligible(true));
}

/// #1440: a repo's `"[c]": { "editor.formatOnSave": true }` formats a C
/// file on save, and only a C file.
#[test]
fn a_language_block_turns_format_on_save_on_for_that_language_only() {
    let (tmp, mut app) = app_with_vscode_settings(r#"{ "[c]": { "editor.formatOnSave": true } }"#);
    assert!(!app.format_on_save, "the global setting stays off");

    app.editor.open(&tmp.path().join("m.c")).unwrap();
    assert!(
        app.format_on_save_eligible(true),
        "a .c tab formats on save"
    );

    app.editor.open(&tmp.path().join("m.py")).unwrap();
    assert!(
        !app.format_on_save_eligible(true),
        "a .py tab keeps the global off"
    );
}

/// #1440 negative: a language block turning format on save off wins over
/// the global on, for that language alone.
#[test]
fn a_language_block_turns_format_on_save_off_over_the_global_on() {
    let (tmp, mut app) = app_with_vscode_settings(
        r#"{ "editor.formatOnSave": true, "[markdown]": { "editor.formatOnSave": false } }"#,
    );
    app.editor.open(&tmp.path().join("README.md")).unwrap();
    assert!(!app.format_on_save_eligible(true), "Markdown is excluded");

    app.editor.open(&tmp.path().join("m.py")).unwrap();
    assert!(
        app.format_on_save_eligible(true),
        "every other language formats"
    );
}

/// #1440: a settings reload that changes only a language block's inlay
/// hints clears that language's cached hints, as the global toggle would.
#[test]
fn a_reload_disabling_a_language_s_inlay_hints_clears_its_cached_hints() {
    let (tmp, mut app) = app_with_vscode_settings("{}");
    app.editor.open(&tmp.path().join("m.py")).unwrap();
    let py = app.editor.path.clone().unwrap();
    app.editor.apply_inlay_hints(
        py,
        vec![crate::lsp::manager::InlayHintItem {
            line: 0,
            character: 1,
            label: String::from(": int"),
        }],
    );
    assert!(!app.editor.inlay_spans_for_test(0).is_empty());

    let mut prefs = app.merged_settings_here().prefs;
    prefs
        .languages
        .entry(String::from("python"))
        .or_default()
        .disable_inlay_hints = Some(true);
    app.apply_merged_settings(&prefs);
    assert!(app.inlay_hints_enabled, "the global setting stays on");
    assert!(
        app.editor.inlay_spans_for_test(0).is_empty(),
        "the Python tab's hints go with its language's setting"
    );
}

#[test]
fn a_second_save_while_a_format_is_in_flight_still_writes_the_first_file() {
    // The latch holds ONE path and the manager tracks one format request id,
    // so arming a second file abandons the first reply. Overwriting the latch
    // therefore lost the first save outright: a.py stayed dirty and unwritten,
    // with no error and no status.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.py");
    let b = tmp.path().join("b.py");
    std::fs::write(&a, "a=1\n").unwrap();
    std::fs::write(&b, "b=1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.editor.insert_char('X');
    app.arm_deferred_save(); // Cmd+S on a.py

    // The user moves on and saves the other file before the first reply lands.
    app.editor.open_pinned(&b).unwrap();
    app.editor.insert_char('Y');
    app.arm_deferred_save(); // Cmd+S on b.py

    assert!(
        std::fs::read_to_string(&a).unwrap().starts_with('X'),
        "a.py's save must survive the second Cmd+S, got {:?}",
        std::fs::read_to_string(&a).unwrap()
    );
    // b.py is still deferred, waiting on its own format reply.
    assert_eq!(app.save_after_format.as_deref(), Some(b.as_path()));
    app.complete_pending_save();
    assert!(
        std::fs::read_to_string(&b).unwrap().starts_with('Y'),
        "and b.py lands when its reply arrives"
    );
}

#[test]
fn a_deferred_format_save_writes_the_file_it_was_asked_to_save() {
    // `save()` armed a bare bool and returned; the write happened later, from
    // the format reply, through `write_current_to_disk` — which saves the
    // ACTIVE tab. Switching tabs while the formatter thought left the file the
    // user pressed Cmd+S on unwritten and still dirty, and wrote the other tab
    // instead, snapshot and didSave included.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.py");
    let b = tmp.path().join("b.py");
    std::fs::write(&a, "a=1\n").unwrap();
    std::fs::write(&b, "b=1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.editor.insert_char('X');
    app.save_after_format = Some(a.clone());
    // The formatter is still thinking; the user moves to the other file.
    app.editor.open_pinned(&b).unwrap();
    app.editor.insert_char('Y');
    assert_eq!(
        app.editor.path.as_deref(),
        Some(b.as_path()),
        "b.py is active"
    );

    app.complete_pending_save();

    assert!(
        std::fs::read_to_string(&a).unwrap().starts_with('X'),
        "the file the save was asked for must be written, got {:?}",
        std::fs::read_to_string(&a).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(&b).unwrap(),
        "b=1\n",
        "the tab that merely happened to be active must NOT be written"
    );
}

#[test]
fn complete_pending_save_writes_formatted_buffer_to_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("m.py");
    std::fs::write(&f, "x=1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    // Simulate a format edit landing in the buffer, then the deferred save
    // firing when the format response arrives.
    app.editor.insert_char('y');
    app.save_after_format = Some(f.clone());
    app.complete_pending_save();
    assert!(
        app.save_after_format.is_none(),
        "the deferred-save latch is consumed"
    );
    let on_disk = std::fs::read_to_string(&f).unwrap();
    assert!(
        on_disk.starts_with('y'),
        "the buffer was written to disk, got {on_disk:?}"
    );
    // With the latch clear, a second call is a no-op (no re-save).
    app.editor.insert_char('z');
    app.complete_pending_save();
    let unchanged = std::fs::read_to_string(&f).unwrap();
    assert_eq!(unchanged, on_disk, "no write without the latch set");
}

#[test]
fn a_semantic_token_reply_older_than_the_applied_one_is_dropped() {
    // Two full requests can be in flight for one path at the SAME seq
    // (sync_lsp plus a server-driven refresh), and the request retry loop
    // emits an empty batch once its backoff runs out. Ordering by request
    // generation is what stops that late empty loser from blanking a good
    // overlay and being persisted to the content-keyed cache.
    assert!(
        crate::app::semantic_reply_is_current(None, 7),
        "the first reply for a path always applies"
    );
    assert!(crate::app::semantic_reply_is_current(Some(7), 8));
    assert!(
        crate::app::semantic_reply_is_current(Some(7), 7),
        "the same request re-delivered is not stale"
    );
    assert!(
        !crate::app::semantic_reply_is_current(Some(8), 7),
        "a reply from an older request must never overwrite a newer one"
    );
}

#[test]
fn quick_fix_inside_a_conflict_opens_the_merge_picker() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", CONFLICTED_BODY);
    app.editor.cursor_row = 2; // inside the ours section
    app.start_code_action();
    let picker = app
        .list_picker
        .as_ref()
        .expect("Quick Fix inside a conflict must open the merge picker without an LSP round-trip");
    assert_eq!(
        picker.purpose,
        crate::widgets::list_picker::ListPurpose::MergeConflict
    );
    assert_eq!(
        picker.rows.len(),
        3,
        "Accept Current / Accept Incoming / Accept Both"
    );
}

#[test]
fn accepting_a_snippet_completion_expands_it_with_tab_stops() {
    // A snippet item in the completion popup carries the body as is_snippet
    // insertion text; Enter must expand it through the editor engine, not
    // insert the literal "$1".
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.insert_str("log");
    let item = super::snippet_completion_item(&crate::snippets::Snippet {
        prefix: "log".into(),
        body: "console.log($1)".into(),
        scope: vec![],
    });
    app.completion_popup = Some(crate::widgets::completion_popup::CompletionPopup::new(
        vec![item],
        "log".into(),
        (0, 0),
        std::path::PathBuf::new(),
        1,
    ));
    app.handle_completion_popup_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.editor.lines[app.editor.cursor_row], "console.log()");
    assert_eq!(
        app.editor.cursor_col, 12,
        "the typed prefix is replaced and the caret lands at $1"
    );
}

#[test]
fn peek_excerpt_windows_clamp_to_the_file() {
    let lines: Vec<String> = (1..=20).map(|i| format!("line-{i}")).collect();
    let (start, ex) = peek_excerpt(&lines, 10, 4, 12);
    assert_eq!(
        (start, ex.len()),
        (6, 12),
        "centered window: 4 above, 7 below"
    );
    let (start, ex) = peek_excerpt(&lines, 1, 4, 12);
    assert_eq!(start, 0, "top of file clamps the above-context");
    assert_eq!(ex.len(), 12);
    let (start, ex) = peek_excerpt(&lines, 19, 4, 12);
    assert_eq!(
        (start, ex.len()),
        (8, 12),
        "bottom of file pulls the window up to keep it full"
    );
    let short: Vec<String> = (1..=3).map(|i| format!("s{i}")).collect();
    let (start, ex) = peek_excerpt(&short, 1, 4, 12);
    assert_eq!((start, ex.len()), (0, 3), "short files return whole");
    let (_, empty) = peek_excerpt(&[], 0, 4, 12);
    assert!(empty.is_empty());
}

#[test]
fn peek_popup_shows_the_target_then_enter_jumps_and_esc_closes() {
    let tmp = tempfile::tempdir().unwrap();
    let other = tmp.path().join("lib.rs");
    let body: String = (1..=30)
        .map(|i| {
            if i == 15 {
                String::from("fn the_answer() -> u32 {\n")
            } else {
                format!("// filler {i}\n")
            }
        })
        .collect();
    std::fs::write(&other, &body).unwrap();
    let here = tmp.path().join("main.rs");
    std::fs::write(&here, "fn main() { the_answer(); }\n// next line\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&here).unwrap();
    app.focus_pane(Pane::Editor);

    // The drain's consumer, driven directly (a live server is not needed to
    // pin the popup contract).
    app.open_peek_popup(other.clone(), 14, 3);
    let popup = app.peek_popup.as_ref().expect("the peek popup must open");
    let text = popup.lines.join("\n");
    assert!(
        text.starts_with("lib.rs:15"),
        "header names the workspace-relative target; was {text:?}"
    );
    assert!(
        text.contains("\u{25b6}15  fn the_answer"),
        "the definition line is numbered and marked; was {text:?}"
    );
    assert!(text.contains("Enter jumps"), "the footer teaches the keys");

    // Esc closes without moving.
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    ))
    .unwrap();
    assert!(app.peek_popup.is_none() && app.peek_target.is_none());
    assert_eq!(app.editor.path.as_deref(), Some(here.as_path()));

    // Enter jumps to the target and records it as a definition jump.
    app.open_peek_popup(other.clone(), 14, 3);
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ))
    .unwrap();
    assert!(app.peek_popup.is_none());
    assert_eq!(
        app.editor.path.as_deref(),
        Some(other.as_path()),
        "Enter converts the peek into the real jump"
    );
    assert_eq!(app.editor.cursor_row, 14);

    // Any other key closes the popup AND keeps its meaning.
    app.editor.open_pinned(&here).unwrap();
    app.editor.cursor_row = 0;
    app.open_peek_popup(other.clone(), 14, 3);
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Down,
        KeyModifiers::NONE,
    ))
    .unwrap();
    assert!(app.peek_popup.is_none(), "a plain key dismisses the glance");
    assert_eq!(
        app.editor.cursor_row, 1,
        "the dismissing key keeps its normal meaning — Down still moves"
    );
}

#[test]
fn peek_references_steps_through_every_reference_with_up_and_down() {
    // #616: the reply's references in the peek popup, "N of M" in the title,
    // Up/Down wrapping through them, Enter jumping to the one shown.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "fn f() {}\nfn g() { f(); }\n").unwrap();
    std::fs::write(&b, "fn h() {\n    f();\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    app.peek_refs = Some((vec![(a.clone(), 1, 9), (b.clone(), 1, 4)], 0));
    app.show_peeked_reference();
    let text = |app: &App| app.peek_popup.as_ref().expect("popup").lines.join("\n");
    assert!(text(&app).starts_with("Reference 1 of 2"), "{}", text(&app));
    assert!(text(&app).contains("a.rs:2"));

    let press = |app: &mut App, code| {
        app.handle_key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE))
            .unwrap()
    };
    press(&mut app, KeyCode::Down);
    assert!(
        text(&app).starts_with("Reference 2 of 2"),
        "Down steps, not dismisses"
    );
    assert!(text(&app).contains("b.rs:2"));
    press(&mut app, KeyCode::Down);
    assert!(text(&app).starts_with("Reference 1 of 2"), "and wraps");
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    assert!(app.peek_popup.is_none() && app.peek_refs.is_none());
    assert_eq!(app.editor.path.as_deref(), Some(b.as_path()));
    assert_eq!(app.editor.cursor_row, 1);
}

#[test]
fn alt_shift_f12_is_peek_references_not_go_to_references() {
    let key_of = |m| crossterm::event::KeyEvent::new(KeyCode::F(12), m);
    let peek = key_of(KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert!(is_peek_references_key(peek));
    assert!(!is_peek_references_key(key_of(KeyModifiers::SHIFT)));
    assert!(is_go_to_references_key(key_of(KeyModifiers::SHIFT)));
}

#[test]
fn the_f_keys_still_go_to_definition_and_rename_without_a_keymap() {
    let (mut app, _tmp) = lsp_nav_app("[]");
    app.handle_key(key(KeyCode::F(12), KeyModifiers::NONE))
        .unwrap();
    assert!(app.definition_request_id.is_some(), "F12");
    app.handle_key(key(KeyCode::F(12), KeyModifiers::SHIFT))
        .unwrap();
    assert!(app.references_request_id.is_some(), "Shift+F12");
    app.handle_key(key(KeyCode::F(2), KeyModifiers::NONE))
        .unwrap();
    assert!(app.prepare_rename_request.is_some(), "F2");
}

#[test]
fn a_rebound_go_to_definition_on_a_non_text_view_says_so_and_sends_nothing() {
    let (mut app, tmp) = lsp_nav_app(
        r#"[
            { "key": "ctrl+alt+d", "command": "go_to_definition" },
            { "key": "ctrl+alt+r", "command": "rename_symbol" }
        ]"#,
    );
    let bin = tmp.path().join("a.bin");
    std::fs::write(&bin, [0u8, 1, 2, 3, 4, 5, 6, 7]).unwrap();
    app.editor.hex = Some(crate::hex::HexView::open(&bin).unwrap());
    assert!(app.editor.has_non_text_view(), "precondition");
    let chord = |c| key(KeyCode::Char(c), KeyModifiers::CONTROL | KeyModifiers::ALT);

    app.handle_key(chord('d')).unwrap();
    assert!(app.definition_request_id.is_none());
    assert_eq!(app.status, "Go to Definition needs a text file");
    app.handle_key(chord('r')).unwrap();
    assert!(app.prepare_rename_request.is_none());
    assert_eq!(app.status, "Rename Symbol needs a text file");
}

#[test]
fn rename_symbol_defers_the_prompt_until_a_verdict_arrives() {
    // #254 item 6: F2 no longer opens the prompt straight away — it fires
    // prepareRename and parks, so the prompt can pre-fill the server's own
    // validated name. (The document here was never opened through the LSP
    // layer, so the worker's answer is the unknown-document `unsupported`
    // verdict; the verdict shapes themselves are covered by
    // `a_prepare_rename_verdict_routes_to_the_prompt_status_or_fallback`.)
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("lib.rs"),
        "fn main() {\n    let value = 1;\n}\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("lib.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 9; // inside `value`
    app.start_rename_symbol();
    assert!(app.prompt.is_none(), "the prompt waits on the verdict");
    assert_eq!(app.status, "Preparing rename…");
    assert!(
        app.prepare_rename_request.is_some(),
        "the request is armed for the verdict to match against"
    );
}

#[test]
fn a_prepare_rename_verdict_routes_to_the_prompt_status_or_fallback() {
    // Every shape the server can answer with (#254 item 6). The reply
    // channel's sender lives inside the LSP worker, so these drive the
    // verdict handler the drain loop delegates to.
    use crate::lsp::manager::PrepareRenameResult;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("lib.rs");
    std::fs::write(&path, "fn main() {\n    let value = 1;\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&path).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 9; // inside `value`

    let verdict = |target, error, unsupported| PrepareRenameResult {
        request_id: 4,
        unsupported,
        error,
        target,
    };
    let arm = |app: &mut App| {
        app.prepare_rename_request = Some((4, path.clone(), 1, 9, app.editor.edit_seq));
        app.prompt = None;
        app.status = String::from("Preparing rename…");
    };

    // A placeholder wins over the range text.
    arm(&mut app);
    assert!(app.apply_prepare_rename_verdict(verdict(
        Some(((1, 8, 1, 13), Some(String::from("renamed")))),
        None,
        false
    )));
    assert_eq!(
        app.prompt.as_ref().expect("prompt opened").buffer,
        "renamed",
        "the server's placeholder pre-fills the prompt"
    );
    assert_eq!(
        app.status, "",
        "\"Preparing rename…\" goes once the prompt opens"
    );

    // No placeholder: the validated range's own text.
    arm(&mut app);
    assert!(app.apply_prepare_rename_verdict(verdict(Some(((1, 8, 1, 13), None)), None, false)));
    assert_eq!(
        app.prompt.as_ref().expect("prompt opened").buffer,
        "value",
        "the range text pre-fills when no placeholder is offered"
    );

    // The server refused the position.
    arm(&mut app);
    assert!(app.apply_prepare_rename_verdict(verdict(
        None,
        Some(String::from("cannot rename a keyword")),
        false
    )));
    assert_eq!(app.status, "Rename rejected: cannot rename a keyword");
    assert!(app.prompt.is_none(), "a refusal opens no prompt");

    // The server answered "nothing here".
    arm(&mut app);
    assert!(app.apply_prepare_rename_verdict(verdict(None, None, false)));
    assert_eq!(app.status, "Nothing renameable at the cursor");
    assert!(app.prompt.is_none());

    // No prepare support: the word-under-cursor fallback.
    arm(&mut app);
    assert!(app.apply_prepare_rename_verdict(verdict(None, None, true)));
    assert_eq!(
        app.prompt.as_ref().expect("fallback prompt opened").buffer,
        "value"
    );
    assert_eq!(app.status, "", "and when the fallback prompt opens");

    // A status something else set after F2 is not the verdict's to clear.
    arm(&mut app);
    app.status = String::from("Saved lib.rs");
    assert!(app.apply_prepare_rename_verdict(verdict(None, None, true)));
    assert_eq!(app.status, "Saved lib.rs");

    // The buffer moved under the in-flight request: say so rather than
    // leaving "Preparing rename…" wedged on screen forever.
    arm(&mut app);
    app.editor.edit_seq += 1;
    assert!(app.apply_prepare_rename_verdict(verdict(
        Some(((1, 8, 1, 13), Some(String::from("renamed")))),
        None,
        false
    )));
    assert_eq!(app.status, "Rename cancelled");
    assert!(app.prompt.is_none(), "a stale verdict opens no prompt");

    // A superseded reply must not disarm the newer in-flight request.
    arm(&mut app);
    let stale = PrepareRenameResult {
        request_id: 3,
        unsupported: false,
        error: None,
        target: None,
    };
    assert!(!app.apply_prepare_rename_verdict(stale));
    assert!(
        app.prepare_rename_request.is_some(),
        "the armed request survives an older reply"
    );
}

#[test]
fn a_cancelled_rename_disarms_the_in_flight_prepare_request() {
    // Esc on the prompt abandons the caret. A verdict landing afterwards must
    // not reopen the prompt — it would carry the stale (row, col) into the
    // rename request and rename the wrong symbol.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("lib.rs");
    std::fs::write(&path, "fn main() {\n    let value = 1;\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&path).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 9;
    app.start_rename_symbol();
    assert!(
        app.prepare_rename_request.is_some(),
        "the request is armed while it is in flight"
    );
    app.prompt = Some(Prompt {
        label: String::from("Rename Symbol 'value'"),
        buffer: String::from("value"),
        kind: PromptKind::RenameSymbol {
            path: path.clone(),
            row: 1,
            col: 9,
        },
        target_dir: std::path::PathBuf::new(),
        error: None,
    });
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.prompt.is_none(), "Esc closes the prompt");
    assert!(
        app.prepare_rename_request.is_none(),
        "and disarms the in-flight prepare so a late verdict cannot reopen it"
    );
}

#[test]
fn format_selection_needs_a_selection_and_reports_unsupported_via_the_reply() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn main( ) {\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("lib.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    // No selection: refused up front with guidance.
    app.run_command(crate::widgets::command_palette::Command::FormatSelection);
    assert_eq!(app.status, "Select something to format");
    // With a selection: the request fires; with no server spawned in
    // tests, the worker's always-answer contract reports unsupported
    // through the SAME drain as whole-document formatting, with
    // selection-specific wording.
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (0, 0),
        head: (1, 1),
    });
    app.run_command(crate::widgets::command_palette::Command::FormatSelection);
    assert_eq!(app.status, "Formatting selection");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.format_request_id.is_some() && std::time::Instant::now() < deadline {
        app.drain_lsp_format();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(app.status, "No range formatter available for this file");
}

#[test]
fn linked_editing_set_clears_when_the_caret_leaves_and_mirrors_through_handle_key() {
    // #254 item 3: keystrokes inside a linked range mirror to the pair
    // on the tick; moving the caret out of every range drops the set.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("page.html"), "<title>t</title>\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .open_pinned(&tmp.path().join("page.html"))
        .unwrap();
    app.focus_pane(Pane::Editor);
    app.editor
        .set_linked_ranges(&[(0, 1, 0, 6), (0, 10, 0, 15)]);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 6;
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.tick_linked_editing(), "the keystroke mirrored");
    assert_eq!(app.editor.lines[0], "<titles>t</titles>");
    // Caret leaves the set: the next tick clears it.
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 9; // inside the body text
    app.tick_linked_editing();
    assert!(!app.editor.has_linked_ranges());
    // Typing there no longer mirrors.
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert!(!app.tick_linked_editing());
    assert_eq!(app.editor.lines[0], "<titles>tx</titles>");
}

/// A `null` linkedEditingRange answer (anything outside a tag pair) was
/// asked again on every UI wake while the caret stayed put: about 45
/// requests per keypress and 11k a session against vtsls (#1299).
#[test]
fn a_null_linked_editing_answer_is_not_asked_again_at_the_same_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = linked_ts_app(tmp.path());
    app.tick_linked_editing(); // observes the caret
    let sent: Vec<u64> = (0..100).filter_map(|_| linked_wake(&mut app)).collect();
    assert_eq!(
        sent.len(),
        1,
        "one request for an unmoved caret over 100 wakes, not {}",
        sent.len()
    );
}

#[test]
fn a_caret_move_between_selection_range_request_and_reply_drops_the_stale_chains() {
    // #272 review: a click moves the caret WITHOUT bumping edit_seq, so
    // the pending gate must also compare the cursor positions the
    // request was fired for — the old caret's chains must not apply at
    // the new spot.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("lib.rs"),
        "fn main() {\n    let value = 1;\n}\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("lib.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 9;
    app.run_command(crate::widgets::command_palette::Command::ExpandSelection);
    assert!(app.selection_range_request.is_some(), "request in flight");
    // A caret move (no edit) before the reply lands.
    app.editor.clear_selection();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.selection_range_request.is_some() && std::time::Instant::now() < deadline {
        app.drain_lsp_selection_ranges();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        app.selection_range_request.is_none(),
        "the stale reply cleared the pending slot"
    );
    assert!(
        app.editor.selection.is_none(),
        "the old caret's chains were dropped, not applied at the new caret"
    );
}

#[test]
fn change_color_presentation_needs_a_color_and_defers_the_picker_on_the_reply() {
    use crate::lsp::manager::ColorItem;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("style.css"), "a { color: #ff0000; }\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .open_pinned(&tmp.path().join("style.css"))
        .unwrap();
    app.focus_pane(Pane::Editor);
    // No color at the cursor: refused up front.
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 2;
    app.run_command(crate::widgets::command_palette::Command::ChangeColorPresentation);
    assert_eq!(app.status, "No color value at the cursor");
    // Inject a color set (as a documentColor reply would) and ask on it.
    let path = tmp.path().join("style.css");
    app.editor.apply_document_colors(
        path.clone(),
        vec![ColorItem {
            line: 0,
            character: 11,
            end_line: 0,
            end_character: 18,
            red: 255,
            green: 0,
            blue: 0,
            raw: (1.0, 0.0, 0.0, 1.0),
        }],
    );
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 12;
    app.run_command(crate::widgets::command_palette::Command::ChangeColorPresentation);
    assert_eq!(app.status, "Fetching color presentations…");
    // No server in tests: the worker's always-answer contract resolves
    // the deferred picker to the empty-offering status.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.color_presentations_request.is_some() && std::time::Instant::now() < deadline {
        app.drain_lsp_color_presentations();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(app.status, "No color presentations offered here");
}

#[test]
fn a_stale_color_presentation_pick_is_refused_after_a_buffer_change() {
    use crate::widgets::editor::TextSpanEdit;
    use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
    // #277 review: an external reload while the picker is open bumps
    // edit_seq; the pick must refuse rather than splice stale offsets.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("style.css"), "a { color: #ff0000; }\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let path = tmp.path().join("style.css");
    app.editor.open_pinned(&path).unwrap();
    app.focus_pane(Pane::Editor);
    app.pending_color_presentations = vec![(
        String::from("rgb(255, 0, 0)"),
        vec![TextSpanEdit {
            start: (0, 11),
            end: (0, 18),
            new_text: String::from("rgb(255, 0, 0)"),
            utf16: false,
        }],
    )];
    app.pending_color_context = Some((path.clone(), app.editor.edit_seq));
    app.open_list_picker(
        ListPicker::new(
            ListPurpose::ColorPresentation,
            "Change Color Presentation",
            vec![ListRow {
                id: String::from("0"),
                label: String::from("rgb(255, 0, 0)"),
            }],
        ),
        "empty",
    );
    // The buffer changes while the picker is open.
    app.editor.insert_char('x');
    let before = app.editor.lines[0].clone();
    app.confirm_list_picker();
    assert_eq!(app.editor.lines[0], before, "no stale edit applied");
    assert_eq!(
        app.status,
        "The buffer changed — re-run Change Color Presentation"
    );
    assert!(app.pending_color_presentations.is_empty());
}

#[test]
fn on_type_formatting_stays_inert_when_disabled_or_unadvertised() {
    // #254 item 4: gated behind editor.format_on_type (default OFF,
    // matching VS Code), and even when on, nothing fires unless a
    // server advertised the trigger set for the language.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn main() {\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("lib.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    assert!(!app.format_on_type, "off by default");
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char(';'), KeyModifiers::NONE))
        .unwrap();
    app.tick_on_type_formatting();
    assert!(app.on_type_request.is_none(), "disabled: nothing fires");
    assert!(
        app.editor.last_typed.is_none(),
        "the tick consumes the record even while disabled, so enabling \
         the setting later cannot replay a stale trigger"
    );
    // Enable via the settings toggle; still inert because no server has
    // advertised any trigger characters for Rust in this test.
    app.toggle_format_on_type();
    assert!(app.format_on_type);
    assert_eq!(app.status, "Format on Type: on");
    app.handle_key(key(KeyCode::Char(';'), KeyModifiers::NONE))
        .unwrap();
    app.tick_on_type_formatting();
    assert!(
        app.on_type_request.is_none(),
        "no advertised trigger set: the request is never sent"
    );
    assert!(
        app.editor.last_typed.is_none(),
        "each keystroke is examined exactly once"
    );
}

#[test]
fn an_on_type_reply_applies_only_while_its_buffer_and_tab_are_unmoved() {
    // The four dispositions of a reply. The channel's sender lives in the
    // LSP worker, so the decision is tested through the pure helper the
    // drain loop delegates to.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "fn a() {\n}\n").unwrap();
    std::fs::write(&b, "fn b() {\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    let reply = |id: u64, path: &std::path::Path| crate::lsp::manager::FormatResult {
        request_id: id,
        path: path.to_path_buf(),
        edits: Some(Vec::new()),
        unsupported: false,
        server: None,
    };

    // Nothing armed: any reply is a leftover.
    assert_eq!(
        app.on_type_reply_disposition(&reply(1, &a)),
        OnTypeReply::NotOurs
    );

    // Armed and everything matches.
    app.on_type_request = Some((7, app.editor.edit_seq, a.clone()));
    assert_eq!(
        app.on_type_reply_disposition(&reply(7, &a)),
        OnTypeReply::Apply
    );

    // A superseded id must NOT disarm the newer in-flight request.
    assert_eq!(
        app.on_type_reply_disposition(&reply(6, &a)),
        OnTypeReply::NotOurs,
        "an older reply is ignored without consuming the armed request"
    );
    assert!(
        app.on_type_request.is_some(),
        "the helper never clears; the newer request stays armed"
    );

    // Buffer moved since the request.
    app.on_type_request = Some((7, app.editor.edit_seq.wrapping_sub(1), a.clone()));
    assert_eq!(
        app.on_type_reply_disposition(&reply(7, &a)),
        OnTypeReply::Stale,
        "edits computed against text that has since changed are dropped"
    );

    // Reply is for a different file than the one requested.
    app.on_type_request = Some((7, app.editor.edit_seq, a.clone()));
    assert_eq!(
        app.on_type_reply_disposition(&reply(7, &b)),
        OnTypeReply::Stale,
        "a reply for another path never applies to this tab"
    );

    // The requested tab is no longer active: edit_seq is per-tab, so the
    // counter alone would compare two unrelated buffers.
    app.editor.open_pinned(&b).unwrap();
    app.on_type_request = Some((7, app.editor.edit_seq, a.clone()));
    assert_eq!(
        app.on_type_reply_disposition(&reply(7, &a)),
        OnTypeReply::Stale,
        "the tab that typed must still be the active one"
    );
}

/// vtsls 0.3.0's on-type reply after typing `if(a>1){return  0;` right
/// after the `{` of `export function f(a: number) {` (#1633), captured with
/// a minimal LSP client. Pairs of edits share a position.
fn vtsls_on_type_reply() -> Vec<crate::widgets::editor::TextSpanEdit> {
    let edit = |col: usize, end: usize, text: &str| crate::widgets::editor::TextSpanEdit {
        start: (0, col),
        end: (0, end),
        new_text: text.to_string(),
        utf16: true,
    };
    vec![
        edit(30, 30, "\n"),
        edit(30, 30, "  "),
        edit(32, 32, " "),
        edit(34, 34, " "),
        edit(35, 35, " "),
        edit(37, 37, " "),
        edit(38, 38, "\n"),
        edit(38, 38, "    "),
        edit(44, 46, " "),
    ]
}

/// #1633: format on type indents the lines it breaks and leaves the caret
/// right after the `;` that was typed, on the line the text moved to.
#[test]
fn format_on_type_keeps_the_caret_after_the_typed_text() {
    let tmp = tempfile::tempdir().unwrap();
    let ts = tmp.path().join("fmt2.ts");
    std::fs::write(
        &ts,
        "export function f(a: number) {if(a>1){return  0;\n  return a;\n}\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&ts).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 48;
    assert!(app.apply_on_type_edits(&ts, &vtsls_on_type_reply()));
    assert_eq!(
        app.editor.lines[..4],
        [
            "export function f(a: number) {",
            "  if (a > 1) {",
            "    return 0;",
            "  return a;"
        ]
        .map(String::from)
    );
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (2, 13));
}

/// #1633 negative: edits that all sit after the caret leave it where it is,
/// and an edit on the caret's line before it moves it by the text added.
#[test]
fn format_on_type_moves_the_caret_only_for_edits_before_it() {
    let tmp = tempfile::tempdir().unwrap();
    let ts = tmp.path().join("a.ts");
    std::fs::write(&ts, "let a=1;\nlet  b = 2;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&ts).unwrap();
    app.focus_pane(Pane::Editor);
    let edit =
        |row: usize, col: usize, end: usize, text: &str| crate::widgets::editor::TextSpanEdit {
            start: (row, col),
            end: (row, end),
            new_text: text.to_string(),
            utf16: true,
        };
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 3;
    assert!(app.apply_on_type_edits(&ts, &[edit(1, 3, 5, " ")]));
    assert_eq!(app.editor.lines[1], "let b = 2;");
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 3));
    app.editor.cursor_col = 8;
    assert!(app.apply_on_type_edits(&ts, &[edit(0, 5, 5, " "), edit(0, 6, 6, " ")]));
    assert_eq!(app.editor.lines[0], "let a = 1;");
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 10));
}

/// #1603: an Add import quick fix grows the import block above the caret;
/// typing straight after it lands on the line the caret was on, not in the
/// import block that took its line number.
#[test]
fn typing_after_an_add_import_lands_on_the_caret_s_own_text() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("main.go");
    std::fs::write(
        &file,
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tname := strings.ToUpper(\"croft\")\n\tfmt.Println(name)\n}\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();
    app.focus = Pane::Editor;
    app.editor.cursor_row = 5;
    app.editor.cursor_col = 9;
    let edits = vec![(
        file.clone(),
        vec![crate::widgets::editor::TextSpanEdit {
            start: (2, 0),
            end: (2, 12),
            new_text: String::from("import (\n\t\"fmt\"\n\t\"strings\"\n)"),
            utf16: true,
        }],
    )];
    app.apply_rename_edits(&edits).unwrap();
    for c in ['m', 'y'] {
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    }
    assert_eq!(app.editor.lines[5], ")", "the import block is intact");
    assert_eq!(
        app.editor.lines[8], "\tname := mystrings.ToUpper(\"croft\")",
        "{:#?}",
        app.editor.lines
    );
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (8, 11));
}

/// Negative (#1603): an edit to another file leaves this tab's caret alone.
#[test]
fn an_edit_to_another_file_leaves_the_caret_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.go");
    let other = tmp.path().join("b.go");
    std::fs::write(&file, "package a\n\nvar x = 1\n").unwrap();
    std::fs::write(&other, "package a\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();
    app.editor.cursor_row = 2;
    app.editor.cursor_col = 4;
    let edits = vec![(
        other.clone(),
        vec![crate::widgets::editor::TextSpanEdit {
            start: (0, 0),
            end: (0, 0),
            new_text: String::from("// one\n// two\n"),
            utf16: false,
        }],
    )];
    app.apply_rename_edits(&edits).unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (2, 4));
    assert!(
        std::fs::read_to_string(&other)
            .unwrap()
            .starts_with("// one")
    );
}

#[test]
fn a_rename_reply_for_a_buffer_typed_into_since_the_request_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_calc_open(&tmp);
    app.rename_baseline = app.editor.edit_seqs();
    type_a_header_line(&mut app);
    app.land_rename(Some(rename_total_to_sum(&file)));
    assert_eq!(
        app.editor.lines.join("\n"),
        "# header\ntotal = 1\nprint(total)\ntotal += 2",
        "no edit lands at the stale positions"
    );
    assert_eq!(
        app.status,
        "Rename cancelled: calc.srn changed while the server was working; press F2 again"
    );
}

#[test]
fn a_rename_reply_for_an_untouched_buffer_still_renames() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_calc_open(&tmp);
    app.rename_baseline = app.editor.edit_seqs();
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    app.land_rename(Some(rename_total_to_sum(&file)));
    assert_eq!(
        app.editor.lines.join("\n"),
        "sum = 1\nprint(sum)\nsum += 2",
        "a caret move is not an edit"
    );
    assert_eq!(app.status, "Renamed 3 occurrence(s) across 1 file(s)");
}

#[test]
fn a_rename_still_rewrites_a_closed_file_when_another_buffer_was_typed_into() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_calc_open(&tmp);
    let closed = app.tree.root.join("other.srn");
    std::fs::write(&closed, "total = 1\nprint(total)\ntotal += 2\n").unwrap();
    app.rename_baseline = app.editor.edit_seqs();
    type_a_header_line(&mut app);
    app.land_rename(Some(rename_total_to_sum(&closed)));
    assert_eq!(
        std::fs::read_to_string(&closed).unwrap(),
        "sum = 1\nprint(sum)\nsum += 2\n",
        "the typing was in a file the rename leaves alone"
    );
    assert_eq!(app.status, "Renamed 3 occurrence(s) across 1 file(s)");
}

#[test]
fn a_rename_reply_with_no_baseline_is_applied_as_before() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_calc_open(&tmp);
    app.land_rename(Some(rename_total_to_sum(&file)));
    assert_eq!(app.editor.lines[0], "sum = 1");
    assert!(app.rename_baseline.is_empty());
}

fn quick_fix_renaming_total(file: &Path) -> crate::lsp::manager::CodeActionItem {
    crate::lsp::manager::CodeActionItem {
        title: String::from("Rename to sum"),
        server: String::from("slowrename"),
        edits: rename_total_to_sum(file),
        command: None,
        needs_resolve: false,
        resolve_action: None,
        is_preferred: false,
    }
}

#[test]
fn a_quick_fix_for_a_buffer_typed_into_since_the_request_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_calc_open(&tmp);
    app.code_action_baseline = app.editor.edit_seqs();
    type_a_header_line(&mut app);
    app.apply_code_action(quick_fix_renaming_total(&file));
    assert_eq!(
        app.editor.lines.join("\n"),
        "# header\ntotal = 1\nprint(total)\ntotal += 2"
    );
    assert_eq!(
        app.status,
        "Quick fix cancelled: calc.srn changed while the server was working; try it again"
    );
}

#[test]
fn a_quick_fix_for_an_untouched_buffer_still_applies() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_calc_open(&tmp);
    app.code_action_baseline = app.editor.edit_seqs();
    app.apply_code_action(quick_fix_renaming_total(&file));
    assert_eq!(app.editor.lines.join("\n"), "sum = 1\nprint(sum)\nsum += 2");
    assert_eq!(
        app.status,
        "Rename to sum: changed 3 edit(s) across 1 file(s)"
    );
}

#[test]
fn peek_references_with_nothing_to_ask_does_not_arm_on_a_stale_request() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.editor.path.is_none());
    app.references_request_id = Some(7);
    app.peek_references_at_cursor();
    assert!(!app.references_want_peek);
    assert_eq!(app.references_request_id, None);
}

/// #369: a format-on-save write lands after `save` returned, so it settles
/// the file's other tab itself rather than leaving it dirty.
#[test]
fn a_deferred_format_on_save_write_cleans_the_symbol_tabs_sibling() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('7'), KeyModifiers::NONE))
        .unwrap();
    app.sync_symbol_views();
    app.save_after_format = Some(file.clone());
    app.complete_pending_save();
    assert!(std::fs::read_to_string(&file).unwrap().contains("27"));
    assert!(!app.editor.editors[1].dirty, "the symbol tab is saved");
    assert!(!app.editor.editors[0].dirty, "so is the file's tab");
    assert!(!app.reload_open_file_after_external_change());
}

/// #369: a format-on-save reply that lands after the user moved to another
/// file writes the requested file's tab and still settles its symbol tab.
#[test]
fn a_late_format_on_save_write_cleans_the_symbol_tabs_sibling_from_another_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('7'), KeyModifiers::NONE))
        .unwrap();
    app.sync_symbol_views();
    let other = tmp.path().join("other.rs");
    std::fs::write(&other, "fn c() {}").unwrap();
    app.editor.open_pinned(&other).unwrap();
    assert_eq!(app.editor.path.as_deref(), Some(other.as_path()));
    app.save_after_format = Some(file.clone());
    app.complete_pending_save();
    assert!(std::fs::read_to_string(&file).unwrap().contains("27"));
    let tabs: Vec<_> = app
        .editor
        .editors
        .iter()
        .filter(|e| e.path.as_deref() == Some(file.as_path()))
        .collect();
    assert_eq!(tabs.len(), 2, "fixture: the file and its symbol tab");
    assert!(tabs.iter().all(|e| !e.dirty), "both tabs are saved");
}

#[test]
fn a_failed_move_leaves_the_importers_alone_and_edits_follow_a_moved_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("pkg/sub")).unwrap();
    std::fs::write(root.join("main.rs"), "use pkg;\n").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    let edit = |path: PathBuf| {
        vec![(
            path,
            vec![crate::widgets::editor::TextSpanEdit {
                start: (0, 4),
                end: (0, 7),
                new_text: String::from("pkg::sub::pkg"),
                utf16: false,
            }],
        )]
    };
    // A folder into its own child: refused, and main.rs must not change.
    let pending = PendingFileMove {
        request_id: 1,
        op: FileMove::Paste {
            dest_dir: root.join("pkg/sub"),
            paths: vec![root.join("pkg")],
        },
        renames: vec![crate::lsp::manager::FileRenameOp {
            old: root.join("pkg"),
            new: root.join("pkg/sub/pkg"),
            is_dir: true,
        }],
        deadline: std::time::Instant::now(),
        baseline: Vec::new(),
    };
    app.finish_file_move(pending, edit(root.join("main.rs")));
    assert_eq!(
        std::fs::read_to_string(root.join("main.rs")).unwrap(),
        "use pkg;\n"
    );
    assert!(root.join("pkg/sub").is_dir());

    // An edit to a file inside the moved folder lands at its new place.
    std::fs::write(root.join("pkg/lib.rs"), "use pkg;\n").unwrap();
    std::fs::create_dir_all(root.join("dest")).unwrap();
    let pending = PendingFileMove {
        request_id: 2,
        op: FileMove::Paste {
            dest_dir: root.join("dest"),
            paths: vec![root.join("pkg")],
        },
        renames: vec![crate::lsp::manager::FileRenameOp {
            old: root.join("pkg"),
            new: root.join("dest/pkg"),
            is_dir: true,
        }],
        deadline: std::time::Instant::now(),
        baseline: Vec::new(),
    };
    app.finish_file_move(pending, edit(root.join("pkg/lib.rs")));
    assert_eq!(
        std::fs::read_to_string(root.join("dest/pkg/lib.rs")).unwrap(),
        "use pkg::sub::pkg;\n"
    );
}

#[test]
fn rename_target_validates_without_touching_the_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("a.txt");
    std::fs::write(&old, "x").unwrap();
    std::fs::write(tmp.path().join("taken.txt"), "y").unwrap();
    let target = crate::widgets::file_tree::rename_target(tmp.path(), &old, "b.txt").unwrap();
    assert_eq!(target, tmp.path().join("b.txt"));
    assert!(old.exists(), "nothing moved yet");
    assert!(crate::widgets::file_tree::rename_target(tmp.path(), &old, "taken.txt").is_err());
}

#[test]
fn clicking_a_painted_code_lens_runs_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn main() {}\n");
    app.editor.code_lenses = vec![lens_at(
        0,
        "fn main() {}",
        "Frobnicate",
        "vendor.frobnicate",
    )];
    let area = ratatui::layout::Rect::new(0, 0, 80, 10);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(
        &mut *app.editor as &mut crate::widgets::editor::Editor,
        area,
        &mut buf,
    );
    let (y, xs, idx) = app
        .editor
        .code_lens_spans
        .first()
        .cloned()
        .expect("lens painted");
    assert_eq!(idx, 0);
    let painted: String = xs
        .clone()
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect();
    assert_eq!(painted, "Frobnicate");
    assert_eq!(app.editor.code_lens_at(xs.start, y), Some(0));
    app.run_code_lens(0);
    assert!(
        app.status
            .contains("vendor.frobnicate is not a command croft runs"),
        "an unmappable lens says why: {}",
        app.status
    );
}

#[test]
fn a_code_lens_hides_once_its_line_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn main() {}\n");
    app.editor.code_lenses = vec![lens_at(0, "fn main() {}", "Run", "x")];
    assert_eq!(app.editor.lenses_on_line(0), vec![0]);
    app.editor.lines[0] = String::from("fn main() { edited }");
    assert!(
        app.editor.lenses_on_line(0).is_empty(),
        "stale lens is not shown"
    );
}

/// The `Rust:` palette rows for the active file, with "Rust:" typed.
fn rust_palette_rows(app: &mut App) -> Vec<String> {
    app.open_command_palette();
    let palette = app.command_palette.as_mut().expect("the palette opens");
    palette.set_query("Rust:");
    let mut rows: Vec<String> = palette
        .results
        .iter()
        .map(|i| i.title().to_string())
        .filter(|t| t.starts_with("Rust: "))
        .collect();
    app.command_palette = None;
    // Which rows, not the palette's ranking of equally good matches.
    rows.sort();
    rows
}

/// #1626: with rust-analyzer serving a Rust file, the palette offers its
/// Expand Macro, Locate Parent Module, Open Cargo.toml, Rebuild Proc Macros
/// and Reload Workspace, where it offered none.
#[test]
fn rust_analyzer_commands_are_offered_where_it_serves_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "main.rs", "fn main() {}\n");
    let path = app.editor.path.clone().unwrap();
    app.lsp
        .as_ref()
        .expect("a test App has an LSP manager")
        .set_document_clients_for_test(&path, &["rust-analyzer"]);
    assert_eq!(
        rust_palette_rows(&mut app),
        [
            "Rust: Expand Macro Recursively at Caret",
            "Rust: Locate Parent Module",
            "Rust: Open Cargo.toml",
            "Rust: Rebuild Proc Macros and Build Scripts",
            "Rust: Reload Workspace",
        ]
    );
}

/// #1626 negative: a file rust-analyzer does not serve (a Python file, or
/// a Rust file before it opens on it) lists none of them.
#[test]
fn rust_analyzer_commands_are_hidden_where_it_does_not_serve_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "m.py", "x = 1\n");
    let path = app.editor.path.clone().unwrap();
    app.lsp
        .as_ref()
        .unwrap()
        .set_document_clients_for_test(&path, &["basedpyright"]);
    assert!(rust_palette_rows(&mut app).is_empty());
    let mut app = app_with_open_file(tmp.path(), "main.rs", "fn main() {}\n");
    assert!(rust_palette_rows(&mut app).is_empty());
}

/// #1626: rust-analyzer retired from diagnostic pulls (one pull timed out)
/// no longer reports on the file, but its client still runs, so its
/// commands stay offered and a keybinding still sends the request.
#[test]
fn rust_analyzer_commands_stay_offered_after_it_stops_reporting_diagnostics() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "main.rs", "fn main() {}\n");
    let path = app.editor.path.clone().unwrap();
    let lsp = app.lsp.as_ref().unwrap();
    lsp.set_document_clients_for_test(&path, &["rust-analyzer"]);
    lsp.set_document_servers_for_test(&path, &[]);
    assert_eq!(rust_palette_rows(&mut app).len(), 5);
    app.request_rust_analyzer(crate::lsp::rust_analyzer::RaRequest::ParentModule);
    assert!(app.rust_analyzer_request.is_some(), "{}", app.status);
}

/// A Rust file rust-analyzer is open on, and an answer to `request` under
/// the id the app now waits on.
fn app_awaiting_rust_analyzer(
    tmp: &std::path::Path,
    request: crate::lsp::rust_analyzer::RaRequest,
) -> (App, u64) {
    let mut app = app_with_open_file(
        tmp,
        "main.rs",
        "macro_rules! sq { ($e:expr) => { $e * $e }; }\nfn main() { let _ = sq!(2 + 1); }\n",
    );
    let path = app.editor.path.clone().unwrap();
    app.lsp
        .as_ref()
        .unwrap()
        .set_document_clients_for_test(&path, &["rust-analyzer"]);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 21;
    app.request_rust_analyzer(request);
    let (id, sent) = app.rust_analyzer_request.expect("the request is awaited");
    assert_eq!(sent, request);
    (app, id)
}

/// #1626: Expand Macro opens the expansion as a Rust tab named after the
/// macro.
#[test]
fn an_expand_macro_answer_opens_the_expansion_in_a_rust_tab() {
    use crate::lsp::rust_analyzer::RaRequest;
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, id) = app_awaiting_rust_analyzer(tmp.path(), RaRequest::ExpandMacro);
    assert!(
        app.apply_rust_analyzer_result(crate::lsp::manager::RustAnalyzerResult {
            request_id: id,
            request: RaRequest::ExpandMacro,
            outcome: Ok(serde_json::json!({ "name": "sq", "expansion": "(2 + 1) * (2 + 1)" })),
        })
    );
    assert!(
        app.editor
            .path
            .as_ref()
            .is_some_and(|p| p.ends_with("Expansion of sq!")),
        "{:?}",
        app.editor.path
    );
    assert_eq!(app.editor.lines[0], "(2 + 1) * (2 + 1)");
    assert_eq!(app.editor.language_label(), "Rust");
    assert!(app.rust_analyzer_request.is_none());
}

/// #1626: Locate Parent Module and Open Cargo.toml jump to the place the
/// server names.
#[test]
fn parent_module_and_cargo_toml_answers_jump_to_the_location() {
    use crate::lsp::rust_analyzer::RaRequest;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"rmod\"\n").unwrap();
    std::fs::write(root.join("lib.rs"), "\n\nmod main;\n").unwrap();
    let (mut app, id) = app_awaiting_rust_analyzer(&root, RaRequest::ParentModule);
    let at = |path: &std::path::Path, line: u32, col: u32| {
        serde_json::json!({
            "uri": url::Url::from_file_path(path).unwrap().as_str(),
            "range": { "start": { "line": line, "character": col }, "end": { "line": line, "character": col + 4 } },
        })
    };
    app.apply_rust_analyzer_result(crate::lsp::manager::RustAnalyzerResult {
        request_id: id,
        request: RaRequest::ParentModule,
        outcome: Ok(serde_json::json!([at(&root.join("lib.rs"), 2, 4)])),
    });
    assert!(
        app.editor
            .path
            .as_ref()
            .is_some_and(|p| p.ends_with("lib.rs"))
    );
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (2, 4));

    app.editor.open_pinned(&root.join("main.rs")).unwrap();
    app.request_rust_analyzer(RaRequest::OpenCargoToml);
    let (id, _) = app.rust_analyzer_request.unwrap();
    app.apply_rust_analyzer_result(crate::lsp::manager::RustAnalyzerResult {
        request_id: id,
        request: RaRequest::OpenCargoToml,
        outcome: Ok(at(&root.join("Cargo.toml"), 0, 0)),
    });
    assert!(
        app.editor
            .path
            .as_ref()
            .is_some_and(|p| p.ends_with("Cargo.toml"))
    );
}

/// #1626 negative: an answer to an older request is dropped, and an error
/// or a `null` expansion reports itself and opens nothing.
#[test]
fn stale_failed_or_empty_rust_analyzer_answers_open_nothing() {
    use crate::lsp::rust_analyzer::RaRequest;
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, id) = app_awaiting_rust_analyzer(tmp.path(), RaRequest::ExpandMacro);
    let file = app.editor.path.clone();
    let answer = |request_id, outcome| crate::lsp::manager::RustAnalyzerResult {
        request_id,
        request: RaRequest::ExpandMacro,
        outcome,
    };
    assert!(!app.apply_rust_analyzer_result(answer(
        id + 100,
        Ok(serde_json::json!({ "name": "sq", "expansion": "x" }))
    )));
    assert_eq!(app.editor.path, file);
    assert!(app.apply_rust_analyzer_result(answer(id, Ok(serde_json::Value::Null))));
    assert_eq!(app.status, "No macro call at the caret");
    assert_eq!(app.editor.path, file);
    app.request_rust_analyzer(RaRequest::ExpandMacro);
    let (id, _) = app.rust_analyzer_request.unwrap();
    app.apply_rust_analyzer_result(answer(id, Err(String::from("content modified"))));
    assert!(app.status.contains("content modified"), "{}", app.status);
    assert_eq!(app.editor.path, file);
}

/// #1626 negative: run on a file rust-analyzer does not serve, a `Rust:`
/// command says so and sends nothing.
#[test]
fn a_rust_command_without_rust_analyzer_says_so_and_sends_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "main.rs", "fn main() {}\n");
    app.run_command(crate::widgets::command_palette::Command::RustExpandMacro);
    assert!(app.rust_analyzer_request.is_none());
    assert_eq!(app.status, "rust-analyzer is not running for this file");
}

/// #694: a source build of croft on this host stops the language servers
/// for its duration and restarts them after, since a compile next to
/// rust-analyzer is what exhausted memory on small remotes.
#[test]
fn a_source_build_pauses_the_language_servers_and_its_end_restarts_them() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.lsp.is_some(), "precondition: a manager is running");

    assert!(app.apply_source_build_state(true));
    assert!(app.lsp.is_none(), "the servers must stop for the build");
    assert!(
        !app.apply_source_build_state(true),
        "a build still running changes nothing"
    );

    assert!(app.apply_source_build_state(false));
    assert!(
        app.lsp.is_some(),
        "the servers must come back once the build ends"
    );
    assert!(!app.apply_source_build_state(false));

    // Nothing running is nothing to pause, and nothing to start later: a
    // manager that failed to start is not brought up by an unrelated build.
    app.lsp = None;
    assert!(!app.apply_source_build_state(true));
    assert!(!app.apply_source_build_state(false));
    assert!(app.lsp.is_none());
}

/// #694: re-rooting while a build has the language servers stopped must not
/// start them beside the compile; the build's end starts them at the new
/// root instead.
#[test]
fn a_reroot_during_a_source_build_leaves_the_servers_stopped() {
    let tmp = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.apply_source_build_state(true));
    assert!(app.lsp.is_none());

    app.change_workspace_root(other.path().to_path_buf());
    assert!(
        app.lsp.is_none(),
        "a re-root started the servers while the build still runs"
    );

    assert!(app.apply_source_build_state(false));
    let lsp = app
        .lsp
        .as_ref()
        .expect("the build's end restarts the servers");
    assert_eq!(
        lsp.workspace_root(),
        app.workspace_root(),
        "the restarted servers must serve the NEW root"
    );
}

/// #1621: "Language Servers: Restart All" starts a fresh manager and drops
/// what the old servers left: diagnostics, progress and the per-file sync
/// marks, so the next sync re-opens every tab against the new servers.
#[test]
fn restart_all_language_servers_starts_fresh_servers() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.py");
    std::fs::write(&file, "x = 1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();
    assert!(app.lsp.is_some());
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("ty"),
        vec![diag(0, 1, DiagnosticSeverity::Error)],
    );
    app.lsp_diagnostics.insert(file.clone(), by_server);
    app.rebuild_problems();
    app.lsp_last_seen.insert(file.clone(), 7);
    app.run_command(crate::widgets::command_palette::Command::RestartLanguageServers);
    assert!(app.lsp.is_some(), "a new manager serves the workspace");
    assert!(app.lsp_last_seen.is_empty(), "every tab is re-opened");
    assert!(app.lsp_diagnostics.is_empty(), "stale diagnostics are gone");
    assert_eq!(app.status, "Restarted all language servers");
    let logged = crate::output::snapshot("Language Servers").unwrap_or_default();
    assert!(
        logged
            .iter()
            .any(|l| l.text == "Restarted all language servers")
    );
}

/// #1621: "Restart for This File" asks the manager to restart the active
/// file's servers and says so.
#[test]
fn restart_for_this_file_names_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();
    app.run_command(crate::widgets::command_palette::Command::RestartLanguageServersForFile);
    assert_eq!(app.status, "Restarting the language servers for main.rs");
}

/// Negative (#1621): a file no language maps to has no servers to restart,
/// so the status says so instead of claiming a restart.
#[test]
fn restart_for_this_file_reports_a_file_no_server_serves() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, "hello\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();
    app.run_command(crate::widgets::command_palette::Command::RestartLanguageServersForFile);
    assert_eq!(app.status, "No language server serves notes.txt");
}

/// Negative (#1621): with no file open, or while an update build has the
/// servers stopped, the restarts change nothing and say why.
#[test]
fn the_restarts_refuse_without_a_file_or_during_a_build() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::RestartLanguageServersForFile);
    assert_eq!(
        app.status,
        "Restart for This File needs a file open in the editor"
    );
    app.lsp_last_seen.insert(tmp.path().join("kept.py"), 3);
    app.lsp = None;
    app.lsp_paused_for_build = true;
    app.run_command(crate::widgets::command_palette::Command::RestartLanguageServers);
    assert!(app.lsp.is_none(), "nothing starts beside the build");
    assert_eq!(app.lsp_last_seen.len(), 1);
    assert!(app.status.contains("builds its update"), "{}", app.status);
}

/// #694 review: a resumed manager numbers semantic replies from 0 again, so
/// the old manager's per-file high-water marks must go with it, or every
/// reply for those files is rejected until the new count catches up.
#[test]
fn resuming_after_a_build_forgets_the_old_semantic_generations() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let p = tmp.path().join("a.rs");
    app.semantic_generation_seen.insert(p.clone(), 500);
    assert!(app.apply_source_build_state(true));
    assert!(app.apply_source_build_state(false));
    assert!(
        semantic_reply_is_current(app.semantic_generation_seen.get(&p).copied(), 0),
        "the restarted server's first reply must be accepted"
    );
}

/// #694 review: diagnostics of a file with no open buffer do not outlive a
/// pause; nothing else would ever drop them.
#[test]
fn resuming_after_a_build_drops_diagnostics_no_open_tab_owns() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "open.rs", "fn main() {}");
    let open = app.editor.path.clone().unwrap();
    let closed = tmp.path().join("closed.rs");
    app.lsp_diagnostics.insert(open.clone(), Default::default());
    app.lsp_diagnostics
        .insert(closed.clone(), Default::default());
    assert!(app.apply_source_build_state(true));
    assert!(app.apply_source_build_state(false));
    assert!(
        !app.lsp_diagnostics.contains_key(&closed),
        "a tab closed during the build kept its diagnostics"
    );
    assert!(
        app.lsp_diagnostics.contains_key(&open),
        "an open tab's diagnostics stay until the new servers publish"
    );
}

// ---- A late formatter reply never lands on text typed since (#1127) ----

/// The issue's `week.notes`, with `bread` added after `milk`: the buffer a
/// format request is sent for.
fn app_with_week_notes(tmp: &tempfile::TempDir) -> (App, PathBuf) {
    let mut app = app_with_open_file(
        tmp.path(),
        "week.notes",
        "# shopping list\nmilk\neggs\n# todo\ncall bob\npay rent\n",
    );
    let file = app.editor.path.clone().unwrap();
    type_at_line_end(&mut app, 1, "\nbread");
    (app, file)
}

fn type_at_line_end(app: &mut App, row: usize, text: &str) {
    app.editor.cursor_row = row;
    app.editor.cursor_col = app.editor.lines[row].chars().count();
    for c in text.chars() {
        if c == '\n' {
            app.editor.insert_newline();
        } else {
            app.editor.insert_char(c);
        }
    }
}

/// What the request path records when it sends `textDocument/formatting`.
fn send_format_request(app: &mut App, file: &Path, on_save: bool) {
    app.format_request_id = Some(41);
    app.format_request_seq = app.format_target_seq(file);
    if on_save {
        app.save_after_format = Some(file.to_path_buf());
    }
}

/// The formatter's answer for the text it was sent: upper-case the two
/// `# ` headings, on lines 0 and 4.
fn uppercase_headings() -> Vec<crate::widgets::editor::TextSpanEdit> {
    [(0, "# shopping list"), (4, "# todo")]
        .into_iter()
        .map(|(row, line)| crate::widgets::editor::TextSpanEdit {
            start: (row, 0),
            end: (row, line.len()),
            new_text: line.to_uppercase(),
            utf16: false,
        })
        .collect()
}

#[test]
fn a_format_on_save_reply_for_a_buffer_typed_into_since_saves_it_unformatted() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_week_notes(&tmp);
    send_format_request(&mut app, &file, true);
    type_at_line_end(&mut app, 2, "\nbutter");
    app.land_format(Some(uppercase_headings()), false, Some(file.clone()), None);
    let typed = "# shopping list\nmilk\nbread\nbutter\neggs\n# todo\ncall bob\npay rent";
    assert_eq!(app.editor.lines.join("\n"), typed, "eggs survives");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("{typed}\n"),
        "and reaches disk"
    );
    assert!(app.save_after_format.is_none());
    assert_eq!(
        app.status,
        "Saved week.notes without formatting: it changed while the formatter was running"
    );
}

#[test]
fn a_format_document_reply_for_a_buffer_typed_into_since_is_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_week_notes(&tmp);
    send_format_request(&mut app, &file, false);
    type_at_line_end(&mut app, 2, "\nbutter");
    let before = app.editor.lines.clone();
    app.land_format(Some(uppercase_headings()), false, Some(file.clone()), None);
    assert_eq!(app.editor.lines, before);
    assert_eq!(
        app.status,
        "Format skipped: week.notes changed while the formatter was running; format again"
    );
}

#[test]
fn a_format_reply_for_an_untouched_buffer_is_still_applied() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_week_notes(&tmp);
    send_format_request(&mut app, &file, false);
    app.editor.cursor_row = 5;
    app.editor.cursor_col = 2;
    app.land_format(Some(uppercase_headings()), false, Some(file.clone()), None);
    assert_eq!(
        app.editor.lines.join("\n"),
        "# SHOPPING LIST\nmilk\nbread\neggs\n# TODO\ncall bob\npay rent",
        "a caret move is not an edit"
    );
    assert_eq!(app.status, "Formatted document");
}

#[test]
fn a_format_on_save_reply_for_an_untouched_buffer_saves_the_formatted_text() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_week_notes(&tmp);
    send_format_request(&mut app, &file, true);
    app.land_format(Some(uppercase_headings()), false, Some(file.clone()), None);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "# SHOPPING LIST\nmilk\nbread\neggs\n# TODO\ncall bob\npay rent\n"
    );
    assert!(app.save_after_format.is_none());
}

// ---- The servers read the file's final newline (#911) ----

const SHOP_PY: &str = "def total(prices):\n    return sum(prices)\n";

/// #911: the servers get the file with its final newline, on open and on
/// every change, so a formatter has none to add to a formatted file.
#[test]
fn the_language_servers_get_the_files_final_newline() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "shop.py", SHOP_PY);
    let file = app.editor.path.clone().unwrap();
    let synced = |app: &App| app.lsp.as_ref().unwrap().synced_text_for_test(&file);
    app.sync_lsp();
    assert_eq!(synced(&app).as_deref(), Some(SHOP_PY));
    type_at_line_end(&mut app, 1, "  # all");
    app.sync_lsp();
    assert_eq!(
        synced(&app).as_deref(),
        Some("def total(prices):\n    return sum(prices)  # all\n")
    );
}

/// #911: a formatter that rewrites the whole file replaces up to the start
/// of the empty row after its final newline. The edit lands, and the save
/// writes one newline, not a blank last line and a second one.
#[test]
fn a_whole_file_format_reply_saves_one_final_newline() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(
        tmp.path(),
        "shop.py",
        "def total(prices):\n  return sum(prices)\n",
    );
    let file = app.editor.path.clone().unwrap();
    send_format_request(&mut app, &file, true);
    let whole_file = crate::widgets::editor::TextSpanEdit {
        start: (0, 0),
        end: (2, 0),
        new_text: SHOP_PY.to_string(),
        utf16: false,
    };
    app.land_format(Some(vec![whole_file]), false, Some(file.clone()), None);
    assert_eq!(
        app.editor.lines,
        vec!["def total(prices):", "    return sum(prices)"]
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), SHOP_PY);
    assert!(!app.editor.dirty);
}

/// #694 review: a format-on-save in flight when the servers stop is saved at
/// once, unformatted, rather than left armed for an unrelated reply.
#[test]
fn pausing_for_a_build_completes_a_pending_format_on_save() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn main() {}");
    let file = app.editor.path.clone().unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app.format_request_id = Some(7);
    app.save_after_format = Some(file.clone());
    assert!(app.apply_source_build_state(true));
    assert!(app.format_request_id.is_none());
    assert!(app.save_after_format.is_none(), "the save was left armed");
    assert!(
        std::fs::read_to_string(&file).unwrap().starts_with('x'),
        "the edit must reach disk"
    );
}

/// #737: the poll's positive path. A marker naming a live build pauses the
/// servers of a remote-launched croft; the marker going resumes them.
#[test]
fn a_live_build_marker_pauses_the_servers_and_its_removal_resumes_them() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.update_watch = Some(crate::update_watch::UpdateWatch::start(
            tmp.path().to_path_buf(),
            String::new(),
        ));
        let cache = croft_cache_dir();
        std::fs::create_dir_all(&cache).unwrap();
        let marker = cache.join(format!(
            "{}{}",
            crate::update_watch::BUILD_MARKER_PREFIX,
            std::process::id()
        ));
        // This test process stands in for the compile: alive, and ours.
        std::fs::write(&marker, std::process::id().to_string()).unwrap();
        if let Some(boot) = crate::update_watch::boot_id() {
            let sidecar = format!(
                "{}{}",
                marker.display(),
                crate::update_watch::BOOT_SIDECAR_SUFFIX
            );
            std::fs::write(sidecar, boot).unwrap();
        }
        assert!(app.poll_source_build_marker());
        assert!(app.lsp.is_none() && app.lsp_paused_for_build);

        std::fs::remove_file(&marker).unwrap();
        app.build_marker_checked = None; // past the once-a-second gate
        assert!(app.poll_source_build_marker());
        assert!(app.lsp.is_some() && !app.lsp_paused_for_build);
    });
}

// --- #1599: language-server messages, questions and documents ---------------

fn server_update(
    server: &str,
    event: crate::lsp::manager::ServerEvent,
) -> crate::lsp::manager::ServerUpdate {
    crate::lsp::manager::ServerUpdate {
        server: String::from(server),
        event,
    }
}

type ServerAnswer = tokio::sync::oneshot::Receiver<Option<lsp_types::MessageActionItem>>;

/// gopls asking whether to update go.mod, with two actions.
fn go_mod_question(app: &mut App) -> ServerAnswer {
    let (reply, answer) = tokio::sync::oneshot::channel();
    let actions = ["Update", "Ignore"]
        .map(|t| lsp_types::MessageActionItem {
            title: String::from(t),
            properties: Default::default(),
        })
        .to_vec();
    app.apply_server_update(server_update(
        "gopls",
        crate::lsp::manager::ServerEvent::Question {
            text: String::from("go.mod needs updating"),
            actions,
            reply,
        },
    ));
    answer
}

#[test]
fn a_server_error_shows_on_the_status_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.status = String::from("Ready");
    let changed = app.apply_server_update(server_update(
        "rust-analyzer",
        crate::lsp::manager::ServerEvent::Message {
            level: lsp_types::MessageType::ERROR,
            text: String::from(
                "\n  Failed to load workspaces.\ncargo metadata exited with code 101",
            ),
        },
    ));
    assert!(changed, "the bar repaints");
    assert_eq!(app.status, "rust-analyzer: Failed to load workspaces.");
}

#[test]
fn progress_updates_still_set_and_clear_the_server_s_status() {
    use crate::lsp::manager::ServerEvent;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.apply_server_update(server_update(
        "rust-analyzer",
        ServerEvent::Progress(Some(String::from("Indexing 3/9"))),
    ));
    assert_eq!(
        app.lsp_progress_status().as_deref(),
        Some("rust-analyzer: Indexing 3/9")
    );
    assert!(app.apply_server_update(server_update("rust-analyzer", ServerEvent::Progress(None))));
    assert_eq!(app.lsp_progress_status(), None);
    assert!(
        !app.apply_server_update(server_update("rust-analyzer", ServerEvent::Progress(None))),
        "a second End changes nothing"
    );
}

#[test]
fn a_server_question_is_answered_with_the_picked_action() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut answer = go_mod_question(&mut app);
    assert!(app.drain_lsp_progress(), "the picker opens");
    let picker = app.list_picker.as_ref().expect("the question is asked");
    assert_eq!(
        picker.purpose,
        crate::widgets::list_picker::ListPurpose::ServerQuestion
    );
    assert_eq!(picker.title, "gopls: go.mod needs updating");
    let labels: Vec<&str> = picker.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["Update", "Ignore"]);
    app.handle_list_picker_key(key(KeyCode::Down, KeyModifiers::NONE));
    app.handle_list_picker_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.list_picker.is_none());
    let picked = answer.try_recv().expect("answered").expect("an action");
    assert_eq!(picked.title, "Ignore");
    assert_eq!(app.status, "gopls: Ignore");
}

#[test]
fn escape_answers_a_server_question_with_no_action() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut answer = go_mod_question(&mut app);
    app.drain_lsp_progress();
    app.handle_list_picker_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.list_picker.is_none());
    assert!(
        matches!(
            answer.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ),
        "the server hears no action, rather than waiting forever"
    );
}

#[test]
fn a_server_question_waits_for_an_open_picker_and_questions_queue() {
    use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_list_picker(
        ListPicker::new(
            ListPurpose::StashApply,
            "Apply Stash",
            vec![ListRow {
                id: String::from("0"),
                label: String::from("stash@{0}"),
            }],
        ),
        "",
    );
    let mut first = go_mod_question(&mut app);
    let mut second = go_mod_question(&mut app);
    app.drain_lsp_progress();
    assert_eq!(
        app.list_picker.as_ref().map(|p| p.title.as_str()),
        Some("Apply Stash"),
        "the user's own picker is not replaced"
    );
    app.handle_list_picker_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(
            first.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "closing the stash picker does not answer the waiting question"
    );
    app.drain_lsp_progress();
    app.handle_list_picker_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(first.try_recv().unwrap().unwrap().title, "Update");
    app.drain_lsp_progress();
    assert!(app.list_picker.is_some(), "then the next question");
    app.handle_list_picker_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(second.try_recv().is_err());
}

#[test]
fn a_question_without_actions_is_only_a_message() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let (reply, mut answer) = tokio::sync::oneshot::channel();
    app.apply_server_update(server_update(
        "gopls",
        crate::lsp::manager::ServerEvent::Question {
            text: String::from("Gopls is busy"),
            actions: Vec::new(),
            reply,
        },
    ));
    app.drain_lsp_progress();
    assert!(app.list_picker.is_none(), "no empty picker");
    assert_eq!(app.status, "gopls: Gopls is busy");
    assert!(matches!(
        answer.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
}

#[test]
fn show_document_opens_the_file_with_the_range_selected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let file = root.join("main.go");
    std::fs::write(
        &file,
        "package main\n\nfunc main() {\n\tprintln(\"hi\")\n}\n",
    )
    .unwrap();
    let mut app = App::new(root).unwrap();
    let range = lsp_types::Range {
        start: lsp_types::Position::new(2, 5),
        end: lsp_types::Position::new(2, 9),
    };
    let (reply, mut shown) = tokio::sync::oneshot::channel();
    app.apply_server_update(server_update(
        "gopls",
        crate::lsp::manager::ServerEvent::ShowDocument {
            uri: lsp_types::Url::from_file_path(&file).unwrap(),
            external: false,
            take_focus: true,
            selection: Some(range),
            reply,
        },
    ));
    assert_eq!(shown.try_recv(), Ok(true), "the server hears it was shown");
    assert_eq!(app.editor.path.as_deref(), Some(file.as_path()));
    let sel = app
        .editor
        .selection
        .as_ref()
        .expect("the range is selected");
    assert_eq!((sel.anchor, sel.head), ((2, 5), (2, 9)), "`main`");
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (2, 9));
}

#[test]
fn show_document_sends_a_link_to_the_browser_and_not_to_a_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for external in [true, false] {
        let (reply, mut shown) = tokio::sync::oneshot::channel();
        app.apply_server_update(server_update(
            "gopls",
            crate::lsp::manager::ServerEvent::ShowDocument {
                uri: lsp_types::Url::parse("https://go.dev/gopls/features").unwrap(),
                external,
                take_focus: true,
                selection: None,
                reply,
            },
        ));
        assert_eq!(shown.try_recv(), Ok(true));
        assert_eq!(app.status, "Open https://go.dev/gopls/features");
        assert!(app.editor.path.is_none(), "no tab for a web page");
    }
}

#[test]
fn show_document_answers_false_for_a_file_that_will_not_open() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let mut app = App::new(root.clone()).unwrap();
    let (reply, mut shown) = tokio::sync::oneshot::channel();
    app.apply_server_update(server_update(
        "gopls",
        crate::lsp::manager::ServerEvent::ShowDocument {
            uri: lsp_types::Url::from_file_path(root.join("missing.go")).unwrap(),
            external: false,
            take_focus: true,
            selection: None,
            reply,
        },
    ));
    assert_eq!(shown.try_recv(), Ok(false), "not shown is not a success");
    assert!(app.editor.path.is_none());
}

#[test]
fn show_document_without_take_focus_leaves_focus_where_it_was() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let file = root.join("main.go");
    std::fs::write(&file, "package main\n").unwrap();
    let mut app = App::new(root).unwrap();
    app.focus_pane(Pane::Tree);
    let show = |app: &mut App, take_focus: bool| {
        let (reply, mut shown) = tokio::sync::oneshot::channel();
        app.apply_server_update(server_update(
            "gopls",
            crate::lsp::manager::ServerEvent::ShowDocument {
                uri: lsp_types::Url::from_file_path(&file).unwrap(),
                external: false,
                take_focus,
                selection: None,
                reply,
            },
        ));
        assert_eq!(shown.try_recv(), Ok(true));
    };
    show(&mut app, false);
    assert_eq!(app.editor.path.as_deref(), Some(file.as_path()));
    assert!(app.focus == Pane::Tree, "takeFocus: false keeps focus");
    show(&mut app, true);
    assert!(app.focus == Pane::Editor, "takeFocus: true moves it");
}

#[test]
fn a_command_only_quick_fix_says_it_ran_rather_than_applied() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _file) = app_with_calc_open(&tmp);
    app.apply_code_action(crate::lsp::manager::CodeActionItem {
        title: String::from("Browse gopls feature documentation"),
        server: String::from("gopls"),
        edits: Vec::new(),
        command: Some(crate::lsp::manager::CodeActionCommand {
            command: String::from("gopls.doc.features"),
            arguments: Vec::new(),
        }),
        needs_resolve: false,
        resolve_action: None,
        is_preferred: false,
    });
    assert_eq!(app.status, "Ran Browse gopls feature documentation");
}

#[test]
fn a_quick_fix_with_edits_and_a_command_reports_the_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_calc_open(&tmp);
    app.code_action_baseline = app.editor.edit_seqs();
    let mut item = quick_fix_renaming_total(&file);
    item.command = Some(crate::lsp::manager::CodeActionCommand {
        command: String::from("srn.reindex"),
        arguments: Vec::new(),
    });
    app.apply_code_action(item);
    assert!(
        app.status.starts_with("Rename to sum: changed "),
        "{}",
        app.status
    );
}

/// #737: a re-root starts a new manager, which numbers semantic replies from
/// 0 again, so the old high-water marks go with the old one.
#[test]
fn a_reroot_forgets_the_old_semantic_generations() {
    let tmp = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let p = tmp.path().join("a.rs");
    app.semantic_generation_seen.insert(p.clone(), 500);
    app.change_workspace_root(other.path().to_path_buf());
    assert!(
        semantic_reply_is_current(app.semantic_generation_seen.get(&p).copied(), 0),
        "the new server's first reply must be accepted"
    );
    assert!(app.lsp.is_some(), "the new root's servers started");
}

/// #694: an "all clear" from the last server reporting on a path drops the
/// path from the store instead of leaving an empty entry behind; another
/// server's findings on it survive.
#[test]
fn an_all_clear_from_every_server_drops_the_path_from_the_store() {
    use crate::lsp::manager::{DiagnosticSeverity, DiagnosticsUpdate};
    let mut store = std::collections::HashMap::new();
    let path = PathBuf::from("/w/gone.rs");
    let update = |server: &str, diagnostics| DiagnosticsUpdate {
        path: path.clone(),
        server: server.to_string(),
        diagnostics,
        pulled: false,
    };
    store_diagnostics_update(
        &mut store,
        update("ra", vec![diag(0, 1, DiagnosticSeverity::Error)]),
    );
    store_diagnostics_update(
        &mut store,
        update("clippy", vec![diag(2, 3, DiagnosticSeverity::Warning)]),
    );
    store_diagnostics_update(&mut store, update("ra", Vec::new()));
    assert_eq!(store[&path].len(), 1, "clippy's findings stay");
    store_diagnostics_update(&mut store, update("clippy", Vec::new()));
    assert!(store.is_empty(), "{store:?}");
    // An all-clear for a path never reported does not create one.
    store_diagnostics_update(&mut store, update("ra", Vec::new()));
    assert!(store.is_empty());
}

/// #1102: rust-analyzer answers a document pull with its own analysis and
/// pushes `cargo check`'s findings. Neither empties the other, so an open
/// file keeps its compile errors in PROBLEMS and under its text.
#[test]
fn a_pull_answer_keeps_what_the_same_server_pushed() {
    use crate::lsp::manager::{DiagnosticSeverity, DiagnosticsUpdate};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let path = tmp.path().join("names.rs");
    let update = |pulled, diagnostics| DiagnosticsUpdate {
        path: path.clone(),
        server: "rust-analyzer".into(),
        diagnostics,
        pulled,
    };
    let mut moved = diag(4, 9, DiagnosticSeverity::Error);
    moved.message = "borrow of moved value: `names`".into();
    app.apply_diagnostics_updates(vec![update(false, vec![moved.clone()])]);
    // The file opens and rust-analyzer's pull finds nothing of its own.
    app.apply_diagnostics_updates(vec![update(true, Vec::new())]);
    app.rebuild_problems();
    assert_eq!(app.problems.error_count(), 1, "{:?}", app.lsp_diagnostics);
    assert_eq!(app.merged_diagnostics(&path), vec![moved.clone()]);
    assert_eq!(app.problems.groups()[0].items[0].source, "rust-analyzer");
    // A save: cargo check pushes again, then the pull finds a warning.
    let mut unused = diag(0, 3, DiagnosticSeverity::Warning);
    unused.message = "unused variable: `total`".into();
    app.apply_diagnostics_updates(vec![
        update(false, vec![moved.clone()]),
        update(true, vec![unused.clone()]),
    ]);
    app.rebuild_problems();
    assert_eq!(app.problems.total_count(), 2);
    let merged = app.merged_diagnostics(&path);
    assert!(
        merged.contains(&moved) && merged.contains(&unused),
        "{merged:?}"
    );
    // And the push no longer replaces the pulled warning.
    app.apply_diagnostics_updates(vec![update(false, vec![moved.clone()])]);
    assert_eq!(app.merged_diagnostics(&path).len(), 2);
}

/// #1102: each kind of report still clears its own findings, and a path
/// leaves the store once neither has any.
#[test]
fn an_empty_pull_or_push_still_clears_its_own_findings() {
    use crate::lsp::manager::{DiagnosticSeverity, DiagnosticsUpdate};
    let mut store = std::collections::HashMap::new();
    let path = PathBuf::from("/w/names.rs");
    let update = |pulled, diagnostics| DiagnosticsUpdate {
        path: path.clone(),
        server: "rust-analyzer".into(),
        diagnostics,
        pulled,
    };
    let pushed = diag(0, 1, DiagnosticSeverity::Error);
    let pulled = diag(2, 3, DiagnosticSeverity::Warning);
    store_diagnostics_update(&mut store, update(false, vec![pushed.clone()]));
    store_diagnostics_update(&mut store, update(true, vec![pulled.clone()]));
    store_diagnostics_update(&mut store, update(true, Vec::new()));
    let left: Vec<_> = diagnostics_by_source(&store[&path]).collect();
    assert_eq!(left, vec![("rust-analyzer", &pushed)]);
    store_diagnostics_update(&mut store, update(true, vec![pulled.clone()]));
    store_diagnostics_update(&mut store, update(false, Vec::new()));
    let left: Vec<_> = diagnostics_by_source(&store[&path]).collect();
    assert_eq!(left, vec![("rust-analyzer", &pulled)]);
    store_diagnostics_update(&mut store, update(true, Vec::new()));
    assert!(store.is_empty(), "{store:?}");
}

/// #1102: a server that pushes a finding and also returns it to a pull
/// lists it once, under the server's own name.
#[test]
fn a_finding_both_pushed_and_pulled_is_listed_once() {
    use crate::lsp::manager::{DiagnosticSeverity, DiagnosticsUpdate};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let path = tmp.path().join("main.py");
    let undefined = diag(0, 4, DiagnosticSeverity::Error);
    app.apply_diagnostics_updates(
        [false, true]
            .into_iter()
            .map(|pulled| DiagnosticsUpdate {
                path: path.clone(),
                server: "pyright".into(),
                diagnostics: vec![undefined.clone()],
                pulled,
            })
            .collect(),
    );
    app.rebuild_problems();
    assert_eq!(app.problems.total_count(), 1);
    assert_eq!(app.problems.groups()[0].items[0].source, "pyright");
    assert_eq!(app.merged_diagnostics(&path), vec![undefined]);
}

#[test]
fn a_snippets_file_that_loads_nothing_says_where_to_look() {
    // #1191: a broken snippets.json used to reload as "Snippets reloaded"
    // with nothing loaded.
    let status = super::snippets_reload_status(true, &[]);
    assert!(status.contains("not loaded"), "{status}");
    assert!(status.contains("OUTPUT · Snippets"), "{status}");
    assert_eq!(
        super::snippets_reload_status(false, &[]),
        "Snippets reloaded"
    );
}

// --- #1255: Prettier projects format with Prettier ------------------------------

/// A Prettier project whose `node_modules/.bin/prettier` is a shell stand-in:
/// it records its arguments and turns double quotes single and drops a
/// trailing `;`, the way `{ semi: false, singleQuote: true }` would.
#[cfg(unix)]
fn prettier_project(config: bool, stub_body: &str) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    if config {
        std::fs::write(
            root.join(".prettierrc"),
            "{ \"semi\": false, \"singleQuote\": true }\n",
        )
        .unwrap();
    }
    let bin = root.join("node_modules/.bin/prettier");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, format!("#!/bin/sh\n{stub_body}\n")).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        root.join("app.ts"),
        "import { a } from \"./a\";\nexport const b = \"b\";\n",
    )
    .unwrap();
    tmp
}

#[cfg(unix)]
const PRETTIER_STUB: &str =
    "printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/../../args.txt\"\nsed \"s/\\\"/'/g; s/;\\$//\"";

#[cfg(unix)]
fn wait_for_prettier(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if app.drain_prettier_format() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn format_document_in_a_prettier_project_runs_its_prettier() {
    let tmp = prettier_project(true, PRETTIER_STUB);
    let file = tmp.path().join("app.ts");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.start_format_document();
    wait_for_prettier(&mut app);
    assert_eq!(
        app.editor.lines,
        vec!["import { a } from './a'", "export const b = 'b'"],
        "the buffer holds Prettier's output; status {:?}",
        app.status
    );
    assert_eq!(app.status, "Formatted document with Prettier");
    let args = std::fs::read_to_string(tmp.path().join("args.txt")).unwrap();
    assert_eq!(args, format!("--stdin-filepath\n{}\n", file.display()));
    assert!(app.editor.undo(), "the format is one undo step");
    assert_eq!(app.editor.lines[0], "import { a } from \"./a\";");
}

/// #1113: Prettier's result lands in the file's tab even when a diff of
/// the file was opened before it, rather than being compared against the
/// diff and dropped.
#[cfg(unix)]
#[test]
fn prettier_formats_a_file_whose_diff_is_open_too() {
    let tmp = prettier_project(true, PRETTIER_STUB);
    let file = tmp.path().join("app.ts");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .open_head_diff_with_text(PathBuf::from("app.ts (HEAD)"), "", &file, true)
        .unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.start_format_document();
    wait_for_prettier(&mut app);
    assert_eq!(app.status, "Formatted document with Prettier");
    assert_eq!(app.editor.lines[0], "import { a } from './a'");
}

#[cfg(unix)]
#[test]
fn format_on_save_in_a_prettier_project_writes_prettiers_output() {
    let tmp = prettier_project(true, PRETTIER_STUB);
    let file = tmp.path().join("app.ts");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.format_on_save = true;
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 0;
    app.editor.insert_char('/');
    app.editor.insert_char('/');
    app.editor.insert_char(' ');
    app.save();
    wait_for_prettier(&mut app);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "import { a } from './a'\n// export const b = 'b'\n",
        "status {:?}",
        app.status
    );
}

#[cfg(unix)]
#[test]
fn without_a_prettier_config_the_language_server_still_formats() {
    let tmp = prettier_project(false, PRETTIER_STUB);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("app.ts")).unwrap();
    app.start_format_document();
    assert!(app.prettier_job.is_none());
    assert!(!tmp.path().join("args.txt").exists(), "Prettier never ran");
    assert_ne!(app.status, "Formatting document with Prettier");
}

/// A directory holding executable stand-ins named `tools`, as a PATH.
#[cfg(unix)]
fn tool_dir(tmp: &Path, tools: &[&str]) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp.join(format!("bin-{}", tools.join("-")));
    std::fs::create_dir_all(&dir).unwrap();
    for tool in tools {
        let p = dir.join(tool);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir.into_os_string()
}

/// An App with `name` open, holding a badly indented script.
fn app_with_script(tmp: &tempfile::TempDir, name: &str) -> (App, PathBuf) {
    let file = tmp.path().join(name);
    std::fs::write(&file, "greet()   {\necho \"hello $1\"\n    }\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.focus_pane(Pane::Editor);
    (app, file)
}

/// #1511: bash-language-server answers a format request with no edits
/// when shfmt is missing; croft says why instead of "already formatted".
#[cfg(unix)]
#[test]
fn formatting_a_shell_script_without_shfmt_says_shfmt_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_script(&tmp, "run.sh");
    app.tool_search_path = Some(tool_dir(tmp.path(), &["shellcheck"]));
    send_format_request(&mut app, &file, false);
    app.land_format(
        Some(Vec::new()),
        false,
        Some(file.clone()),
        Some(BASH_SERVER),
    );
    assert_eq!(app.status, SHFMT_MISSING);
    assert!(app.status.contains("shfmt"), "{}", app.status);
}

/// #1511, negative: with shfmt installed an empty reply really means the
/// script is formatted, and other files keep "already formatted" too.
#[cfg(unix)]
#[test]
fn an_empty_format_reply_is_already_formatted_with_shfmt_or_for_other_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_script(&tmp, "run.sh");
    app.tool_search_path = Some(tool_dir(tmp.path(), &["shfmt"]));
    send_format_request(&mut app, &file, false);
    app.land_format(
        Some(Vec::new()),
        false,
        Some(file.clone()),
        Some(BASH_SERVER),
    );
    assert_eq!(app.status, "Document already formatted");

    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_script(&tmp, "notes.py");
    app.tool_search_path = Some(tool_dir(tmp.path(), &[]));
    send_format_request(&mut app, &file, false);
    app.land_format(
        Some(Vec::new()),
        false,
        Some(file.clone()),
        Some(BASH_SERVER),
    );
    assert_eq!(app.status, "Document already formatted");
}

/// #1511: an empty reply from another formatter (a user extension's, say)
/// is that formatter's answer; shfmt has nothing to do with it.
#[cfg(unix)]
#[test]
fn an_empty_format_reply_from_another_server_does_not_blame_shfmt() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_script(&tmp, "run.sh");
    app.tool_search_path = Some(tool_dir(tmp.path(), &[]));
    send_format_request(&mut app, &file, false);
    app.land_format(
        Some(Vec::new()),
        false,
        Some(file.clone()),
        Some("my-shell-fmt"),
    );
    assert_eq!(app.status, "Document already formatted");
}

/// Register (or, with no names, unregister) bash-language-server for
/// shell scripts, as the `lsp-bash` extension being enabled or disabled
/// would.
fn register_bash_servers(app: &mut App, names: &[&'static str]) {
    app.lsp
        .as_mut()
        .expect("a test App has an LSP manager")
        .set_registered_servers_for_test(crate::lsp::Language::BASH, names);
}

/// #1511: opening a shell script without shellcheck says, once, that it
/// will not be linted, since PROBLEMS stays empty either way.
#[cfg(unix)]
#[test]
fn opening_a_shell_script_without_shellcheck_says_so_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_script(&tmp, "run.sh");
    register_bash_servers(&mut app, &[BASH_SERVER]);
    app.tool_search_path = Some(tool_dir(tmp.path(), &["shfmt"]));
    app.sync_lsp();
    assert_eq!(app.status, SHELLCHECK_MISSING);
    app.status = String::from("Saved run.sh");
    let other = tmp.path().join("deploy.sh");
    std::fs::write(&other, "echo hi\n").unwrap();
    app.editor.open_pinned(&other).unwrap();
    app.sync_lsp();
    assert_eq!(app.status, "Saved run.sh", "the notice is shown once");
}

/// #1511, negative: no notice with shellcheck installed, or for a file
/// that is not a shell script.
#[cfg(unix)]
#[test]
fn no_lint_notice_with_shellcheck_or_for_other_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_script(&tmp, "run.sh");
    app.tool_search_path = Some(tool_dir(tmp.path(), &["shellcheck"]));
    app.status.clear();
    app.sync_lsp();
    assert_eq!(app.status, "");

    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_script(&tmp, "notes.py");
    app.tool_search_path = Some(tool_dir(tmp.path(), &[]));
    app.status.clear();
    app.sync_lsp();
    assert_eq!(app.status, "");

    // A disabled `lsp-bash` runs no bash-language-server, so there is
    // nothing shellcheck would lint for.
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_script(&tmp, "run.sh");
    register_bash_servers(&mut app, &[]);
    app.tool_search_path = Some(tool_dir(tmp.path(), &[]));
    app.status.clear();
    app.sync_lsp();
    assert_eq!(app.status, "");
}

/// #1511: only an executable file counts as the tool being installed.
#[cfg(unix)]
#[test]
fn a_tool_is_on_path_only_as_an_executable_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tool_dir(tmp.path(), &["shfmt"]);
    assert!(tool_on_path("shfmt", Some(&path)));
    assert!(!tool_on_path("shellcheck", Some(&path)));
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(plain.join("shellcheck")).unwrap();
    std::fs::write(plain.join("shfmt"), "not executable").unwrap();
    assert!(!tool_on_path("shfmt", Some(plain.as_os_str())));
    assert!(!tool_on_path("shellcheck", Some(plain.as_os_str())));
    assert!(!tool_on_path("shfmt", None));
}

#[cfg(unix)]
#[test]
fn a_failing_prettier_says_why_and_still_saves() {
    let tmp = prettier_project(
        true,
        "cat > /dev/null\necho '[error] app.ts: SyntaxError: Unexpected token (1:5)' >&2\nexit 2",
    );
    let file = tmp.path().join("app.ts");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.format_on_save = true;
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.editor.insert_char('X');
    app.save();
    wait_for_prettier(&mut app);
    assert_eq!(
        app.status,
        "Prettier failed: app.ts: SyntaxError: Unexpected token (1:5); saved without formatting"
    );
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .starts_with("Ximport"),
        "the save still happens, unformatted"
    );
}

#[cfg(unix)]
#[test]
fn an_edit_made_while_prettier_runs_is_kept() {
    let tmp = prettier_project(true, PRETTIER_STUB);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("app.ts")).unwrap();
    app.start_format_document();
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 0;
    app.editor.insert_char('Z');
    wait_for_prettier(&mut app);
    assert_eq!(app.editor.lines[1], "Zexport const b = \"b\";");
    assert_eq!(
        app.status,
        "Prettier result dropped: the file changed while it ran"
    );
}

#[cfg(unix)]
#[test]
fn a_final_newline_prettier_adds_reaches_the_disk() {
    // Prettier always ends a file with a newline; `awk 1` does the same here.
    let stub = format!("{PRETTIER_STUB} | awk 1");
    let tmp = prettier_project(true, &stub);
    let file = tmp.path().join("app.ts");
    std::fs::write(&file, "export const b = \"b\";").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.format_on_save = true;
    app.editor.insert_char(' ');
    app.editor.undo();
    app.editor.insert_char('/');
    app.editor.insert_char('/');
    app.save();
    wait_for_prettier(&mut app);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "//export const b = 'b'\n",
        "status {:?}",
        app.status
    );
}

// --- #1254: long hovers scroll ---------------------------------------------------

fn app_with_long_hover() -> (
    App,
    tempfile::TempDir,
    ratatui::Terminal<ratatui::backend::TestBackend>,
) {
    let (mut app, tmp, col, row) = app_with_open_file_and_editor_cell();
    let doc = (1..=40)
        .map(|i| format!("doc line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.hover_popup = Some(crate::widgets::hover_popup::HoverPopup::new(
        doc,
        (col, row),
    ));
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    (app, tmp, term)
}

#[test]
fn the_wheel_over_a_hover_popup_scrolls_it_instead_of_closing_it() {
    use crossterm::event::MouseEventKind;
    let (mut app, _tmp, mut term) = app_with_long_hover();
    let area = app.hover_popup.as_ref().unwrap().last_area;
    assert!(area.height > 2, "the popup painted");
    let (x, y) = (area.x + 3, area.y + 2);
    app.handle_mouse(mouse(MouseEventKind::ScrollDown, x, y));
    let popup = app
        .hover_popup
        .as_ref()
        .expect("the wheel keeps the popup open");
    assert!(popup.scroll > 0, "and scrolls it");
    let after_down = popup.scroll;
    app.handle_mouse(mouse(MouseEventKind::ScrollUp, x, y));
    assert!(app.hover_popup.as_ref().unwrap().scroll < after_down);
    term.draw(|frame| app.render(frame)).unwrap();
    assert!(app.hover_popup.is_some());
}

#[test]
fn moving_the_pointer_onto_the_hover_popup_keeps_it_open() {
    use crossterm::event::MouseEventKind;
    let (mut app, _tmp, _term) = app_with_long_hover();
    let area = app.hover_popup.as_ref().unwrap().last_area;
    app.handle_mouse(mouse(MouseEventKind::Moved, area.x + 3, area.y + 2));
    assert!(
        app.hover_popup.is_some(),
        "the pointer has to cross onto the popup to scroll it"
    );
}

#[test]
fn the_wheel_away_from_the_hover_popup_still_closes_it() {
    use crossterm::event::MouseEventKind;
    let (mut app, _tmp, _term) = app_with_long_hover();
    let area = app.hover_popup.as_ref().unwrap().last_area;
    let x = if area.x > 2 {
        area.x - 2
    } else {
        area.right() + 2
    };
    app.handle_mouse(mouse(MouseEventKind::ScrollDown, x, area.y + 1));
    assert!(
        app.hover_popup.is_none(),
        "scrolling the editor dismisses the hover"
    );
}

// --- #1252: Quick Fix sends the selection --------------------------------------

fn diag_on_line(line: u32, message: &str) -> crate::lsp::manager::Diagnostic {
    crate::lsp::manager::Diagnostic {
        start_line: line,
        start_char: 0,
        end_line: line,
        end_char: 1,
        severity: crate::lsp::manager::DiagnosticSeverity::Warning,
        message: String::from(message),
        tags: Default::default(),
        code: None,
    }
}

const QUICK_FIX_BODY: &str = "def f():\n    a = 1\n    b = 2\n    c = 3\n    return a\n";

#[test]
fn quick_fix_with_a_selection_asks_about_the_whole_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "f.py", QUICK_FIX_BODY);
    // Select lines 2-3 top to bottom: the caret ends at the start of line 4.
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (1, 0),
        head: (3, 0),
    });
    app.editor.cursor_row = 3;
    app.editor.cursor_col = 0;
    let (start, end, _) = app.code_action_request();
    assert_eq!(
        (start, end),
        ((1, 0), (3, 0)),
        "the code action range must be the selection, not the caret point"
    );
}

#[test]
fn quick_fix_with_a_backwards_selection_sends_it_start_first() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "f.py", QUICK_FIX_BODY);
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (2, 9),
        head: (1, 4),
    });
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 4;
    let (start, end, _) = app.code_action_request();
    assert_eq!((start, end), ((1, 4), (2, 9)));
}

#[test]
fn quick_fix_with_a_selection_sends_the_diagnostics_of_every_selected_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "f.py", QUICK_FIX_BODY);
    let path = app.editor.path.clone().unwrap();
    app.editor.apply_diagnostics(
        path,
        vec![
            diag_on_line(1, "a unused"),
            diag_on_line(2, "b unused"),
            diag_on_line(4, "outside"),
        ],
    );
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (1, 0),
        head: (2, 9),
    });
    app.editor.cursor_row = 2;
    app.editor.cursor_col = 9;
    let (_, _, diagnostics) = app.code_action_request();
    let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages, vec!["a unused", "b unused"]);
}

#[test]
fn quick_fix_without_a_selection_still_asks_about_the_caret_point() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "f.py", QUICK_FIX_BODY);
    let path = app.editor.path.clone().unwrap();
    app.editor.apply_diagnostics(
        path,
        vec![diag_on_line(1, "a unused"), diag_on_line(2, "b unused")],
    );
    app.editor.selection = None;
    app.editor.cursor_row = 2;
    app.editor.cursor_col = 6;
    let (start, end, diagnostics) = app.code_action_request();
    assert_eq!((start, end), ((2, 6), (2, 6)));
    let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        vec!["b unused"],
        "only the caret line's diagnostics"
    );
}

#[test]
fn quick_fix_with_an_empty_selection_falls_back_to_the_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "f.py", QUICK_FIX_BODY);
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (3, 4),
        head: (3, 4),
    });
    app.editor.cursor_row = 3;
    app.editor.cursor_col = 4;
    let (start, end, _) = app.code_action_request();
    assert_eq!((start, end), ((3, 4), (3, 4)));
}

#[test]
fn quick_fix_selection_columns_are_utf16() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "f.py", "s = \"🙂🙂\"\n");
    // Select the two emoji: chars 5..7, UTF-16 units 5..9.
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (0, 5),
        head: (0, 7),
    });
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 7;
    let (start, end, _) = app.code_action_request();
    assert_eq!((start, end), ((0, 5), (0, 9)));
}

// ── #1490: inlay hints are asked for up to the document's real end ──

#[test]
fn inlay_hints_are_requested_up_to_the_last_line_not_one_past_it() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app_with_open_file(
        tmp.path(),
        "main.rs",
        "fn main() {\n    let v = 40 + 2;\n}\n",
    );
    let targets = app.inlay_hint_targets();
    assert_eq!(targets.len(), 1);
    // The server holds the buffer's lines joined by newlines; its last
    // position is the end of the closing brace's line, not a line past it.
    assert_eq!(targets[0].1, lsp_types::Position::new(2, 1));
    assert_eq!(
        targets[0].1,
        crate::lsp::manager::document_end(&app.editor.lines.join("\n"))
    );
}

#[test]
fn inlay_hints_end_after_the_last_line_in_utf16_units() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app_with_open_file(tmp.path(), "main.py", "x = 1\ns = \"é😀\"");
    // `s = "é😀"` is 9 UTF-16 units: `é` is one and `😀` is two.
    assert_eq!(
        app.inlay_hint_targets()[0].1,
        lsp_types::Position::new(1, 9)
    );
}

/// Negative: an editor with no file asks nothing.
#[test]
fn inlay_hints_target_no_file_when_none_is_open() {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.inlay_hint_targets().is_empty());
}

#[test]
fn a_snippet_reload_with_a_skipped_entry_says_so() {
    // Built in memory: writing the real snippets.json would leak into every
    // concurrently running test that builds an `App`.
    let set = crate::snippets::SnippetSet::from_json(
        r#"{ "Broken": { "prefix": 42, "body": "x" }, "Log": { "prefix": "log", "body": "y" } }"#,
    );
    let status = super::snippets_reload_status(set.is_broken(), set.warnings());
    assert!(
        status.contains("1 warning") && status.contains("OUTPUT · Snippets"),
        "{status:?}"
    );
}

/// Negative: a clean reload says nothing about warnings.
#[test]
fn a_clean_snippet_reload_has_no_warning() {
    let set =
        crate::snippets::SnippetSet::from_json(r#"{ "Log": { "prefix": "log", "body": "y" } }"#);
    assert_eq!(
        super::snippets_reload_status(set.is_broken(), set.warnings()),
        "Snippets reloaded"
    );
}

/// A format-on-save in flight for `m.py`, with `x` typed and not yet on
/// disk: the state Ctrl+S leaves while it waits on the formatter.
fn app_waiting_on_format_on_save(tmp: &Path) -> (App, PathBuf) {
    let mut app = app_with_open_file(tmp, "m.py", "x = 1\n");
    let file = app.editor.path.clone().unwrap();
    app.editor.insert_char('y');
    app.arm_deferred_save();
    app.format_request_id = Some(41);
    app.format_request_seq = app.format_target_seq(&file);
    (app, file)
}

/// #1491: with Format on Save on, a server that never answered
/// `textDocument/formatting` left the file unwritten forever (the tab dirty,
/// "Formatting document" in the status bar). After a bounded wait the file
/// is written unformatted, as VS Code does, and the late reply is dropped.
#[test]
fn a_format_on_save_with_no_reply_writes_the_file_unformatted_after_a_bounded_wait() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_waiting_on_format_on_save(tmp.path());
    let start = std::time::Instant::now();

    // Within the wait nothing is written: the reply may still come.
    assert!(!app.drain_lsp_format_at(start));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "x = 1\n");
    assert!(app.save_after_format.is_some());

    let after = start + FORMAT_ON_SAVE_TIMEOUT + std::time::Duration::from_millis(50);
    assert!(app.drain_lsp_format_at(after), "redraw for the save");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "yx = 1\n");
    assert!(!app.editor.dirty, "the tab is saved");
    assert!(app.save_after_format.is_none());
    assert!(
        app.format_request_id.is_none(),
        "a reply that turns up later is not applied to the saved text"
    );
    assert_eq!(
        app.status,
        "Saved m.py without formatting: the formatter did not answer in 2 s"
    );
}

/// #1491: a tab closed while its format-on-save waited is not written, so
/// the timeout keeps "Save skipped" rather than claiming it saved the file.
#[test]
fn a_format_on_save_timeout_after_the_tab_closed_does_not_claim_a_save() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_waiting_on_format_on_save(tmp.path());
    app.editor.close_tab(0);
    let after = std::time::Instant::now() + FORMAT_ON_SAVE_TIMEOUT * 2;
    assert!(app.drain_lsp_format_at(after));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "x = 1\n");
    assert!(app.save_after_format.is_none());
    assert_eq!(app.status, "Save skipped: the tab was closed");
}

/// #1491: pressing Ctrl+S again while the formatter is silent writes the
/// file at once instead of changing nothing.
#[test]
fn a_second_save_while_the_formatter_is_silent_writes_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_waiting_on_format_on_save(tmp.path());
    app.save();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "yx = 1\n");
    assert!(!app.editor.dirty);
    assert!(app.save_after_format.is_none());
    assert!(app.format_request_id.is_none());
    assert_eq!(
        app.status,
        "Saved m.py without formatting: saved again before the formatter answered"
    );
}

/// #1491 negative: a reply inside the wait still formats, then saves, and
/// the wait does not fire afterwards.
#[test]
fn a_format_reply_inside_the_wait_still_formats_before_saving() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_waiting_on_format_on_save(tmp.path());
    let formatted = vec![crate::widgets::editor::TextSpanEdit {
        start: (0, 0),
        end: (0, 6),
        new_text: String::from("yx = 2"),
        utf16: false,
    }];
    app.format_request_id = None;
    app.land_format(Some(formatted), false, Some(file.clone()), None);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "yx = 2\n");
    let saved = app.status.clone();
    let later = std::time::Instant::now() + FORMAT_ON_SAVE_TIMEOUT * 2;
    assert!(!app.drain_lsp_format_at(later), "nothing left to time out");
    assert_eq!(app.status, saved);
}

/// #1491 negative: a plain Format Document (no save waiting) is never cut
/// short; a slow formatter may still answer it.
#[test]
fn a_slow_format_document_without_a_save_is_left_to_finish() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "m.py", "x = 1\n");
    app.format_request_id = Some(41);
    let later = std::time::Instant::now() + FORMAT_ON_SAVE_TIMEOUT * 2;
    assert!(!app.drain_lsp_format_at(later));
    assert_eq!(app.format_request_id, Some(41));
}

/// #1499: zod's `.vscode/settings.json` turns format on save on and names
/// Biome. croft runs no Biome, so a save writes the file as typed instead
/// of letting tsserver reformat it, and the first such save says why.
#[test]
fn a_project_formatter_croft_does_not_run_saves_the_file_as_typed() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/settings.json"),
        r#"{ "editor.formatOnSave": true, "editor.defaultFormatter": "biomejs.biome" }"#,
    )
    .unwrap();
    let mut app = app_with_open_file(tmp.path(), "api.ts", "type A =\n  string;\n");
    app.apply_merged_settings(&crate::config_layers::load_merged(Some(tmp.path())).prefs);
    assert!(app.format_on_save, "the project's formatOnSave is honoured");
    assert_eq!(app.default_formatter.as_deref(), Some("biomejs.biome"));
    let file = app.editor.path.clone().unwrap();
    app.editor.insert_char('x');
    app.format_and_save(true);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "xtype A =\n  string;\n"
    );
    assert!(!app.editor.dirty);
    assert!(
        app.save_after_format.is_none(),
        "no format request is waited on"
    );
    assert!(app.format_request_id.is_none());
    assert_eq!(
        app.status,
        "Saved api.ts without formatting: this project formats with biomejs.biome, which croft does not run"
    );

    app.editor.insert_char('y');
    app.format_and_save(true);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "xytype A =\n  string;\n"
    );
    assert!(
        !app.status.contains("biomejs.biome"),
        "said once: {}",
        app.status
    );
}

/// #1499, negative: with no formatter named, or croft's own server named
/// for the language, format on save still formats; a language entry for
/// another language leaves this one alone.
#[test]
fn format_on_save_still_formats_when_croft_runs_the_named_formatter() {
    let tmp = tempfile::tempdir().unwrap();
    for (global, per_language) in [
        (None, vec![]),
        (Some("vscode.typescript-language-features"), vec![]),
        (None, vec![("python", "charliermarsh.ruff")]),
        (
            Some("biomejs.biome"),
            vec![("typescript", "vscode.typescript-language-features")],
        ),
    ] {
        let mut app = app_with_open_file(tmp.path(), "api.ts", "type A = string;\n");
        app.format_on_save = true;
        app.default_formatter = global.map(String::from);
        app.language_formatters = per_language
            .iter()
            .map(|(l, f)| (l.to_string(), f.to_string()))
            .collect();
        app.editor.insert_char('x');
        app.format_and_save(true);
        assert_eq!(
            app.save_after_format, app.editor.path,
            "{global:?} {per_language:?}: the save waits on the formatter"
        );
    }
}
