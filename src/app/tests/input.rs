//! Mouse and keyboard handling no single feature owns: clicks, drags, focus
//! and modifiers.

use super::*;

#[test]
fn shift_alt_drag_starts_a_column_selection() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("t.rs");
    std::fs::write(&f, "aaaaaaaa\nbbbbbbbb\ncccccccc\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    // Clear of the gutter: the sign margin / line-number columns are not
    // buffer positions, so begin_box_select would refuse there.
    let col = app.editor.last_inner.x + 8;
    let row = app.editor.last_inner.y;
    let mut down = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    down.modifiers = KeyModifiers::ALT | KeyModifiers::SHIFT;
    app.handle_mouse(down);
    assert!(
        app.editor.box_selecting(),
        "Shift+Alt press must anchor a column selection"
    );
    // Drag DOWN AND ACROSS. A drag that only moves vertically produces
    // zero-width selections on every row, which an implementation that ignored
    // the anchor column entirely would also satisfy.
    let mut drag = mouse(MouseEventKind::Drag(MouseButton::Left), col + 3, row + 2);
    drag.modifiers = KeyModifiers::ALT | KeyModifiers::SHIFT;
    app.handle_mouse(drag);
    assert!(
        app.editor.carets.len() >= 2,
        "Shift+Alt+drag must build a column of carets, got {}",
        app.editor.carets.len()
    );
    // The head row's span lives in `selection`; the rows above it are carets.
    // Every one must cover the SAME three columns — that is what makes it a
    // column selection rather than a stack of bare cursors.
    let head = app
        .editor
        .selection
        .expect("the head row must span columns");
    assert_eq!(
        head.head.1 - head.anchor.1,
        3,
        "head row must span 3 columns"
    );
    for c in &app.editor.carets {
        assert_eq!(
            (c.anchor.1, c.head.1),
            (head.anchor.1, head.head.1),
            "every row of a column selection covers the same columns"
        );
    }
}

#[test]
fn the_at_click_commands_all_refuse_a_non_text_view() {
    // All three At Click commands guard on `has_non_text_view`, because
    // `buffer_pos_at` maps cells from the TEXT layout and still returns a
    // position on a diff/sheet/hex/log view — so without the guard they would
    // act on coordinates belonging to a buffer the user is not looking at.
    //
    // Short-circuiting all three guards to false leaves the full suite green,
    // so the guards shipped unpinned. Each arm asserts its OWN status, since
    // a shared assertion would pass with two of the three guards deleted.
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("a.bin");
    std::fs::write(&bin, [0u8, 1, 2, 3, 4, 5, 6, 7]).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.hex = Some(crate::hex::HexView::open(&bin).unwrap());

    // Positive control: without this the test passes vacuously if the fixture
    // ever stops producing a non-text view.
    assert!(
        app.editor.has_non_text_view(),
        "precondition: the fixture must actually open a non-text view, or \
         every assertion below is about the ordinary text path"
    );
    // And the commands must have a click to act on, or they take the
    // keyboard-invocation arm and refuse for an unrelated reason.
    app.last_mouse_pos = Some((10, 10));

    for (cmd, want) in [
        (
            crate::widgets::command_palette::Command::MouseAddCursorAtClick,
            "No cursors in this view",
        ),
        (
            crate::widgets::command_palette::Command::MouseGoToDefinitionAtClick,
            "No definitions in this view",
        ),
        (
            crate::widgets::command_palette::Command::MouseOpenLinkAtClick,
            "No links in this view",
        ),
    ] {
        app.status = String::from("untouched");
        app.run_command(cmd);
        assert_eq!(
            app.status, want,
            "{cmd:?} must refuse on a non-text view rather than acting on a \
             buffer position the user cannot see"
        );
    }
}

#[test]
fn a_modified_double_click_can_fire_at_all() {
    // The dispatch deliberately refuses to record a MODIFIED click into the
    // built-in trackers: they are shared with the editor's plain select-word,
    // so arming them with a ctrl+click would make the user's next ORDINARY
    // click select a word they never asked for. But `double_click` is only
    // classified by asking a tracker whether it just saw a click here — so
    // with nothing recording modified clicks, `ctrl+double_click` could never
    // become true and the binding was unreachable. A separate tracker keeps
    // both rules: the built-in one stays unarmed, the modified pair is seen.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Bound in `terminal`, not `editor`: with no file open the editor is never
    // laid out (`last_area` comes back height 0), so no click coordinate can
    // land in it and the binding's `when` could never match. The terminal is
    // always present, and this test is about whether a modified double-click
    // can fire AT ALL — the region it fires in is incidental.
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+double_click", "command": "toggle_word_wrap", "when": "terminal"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let before = app.editor.wrap_enabled();
    let area = app.terminals[0].last_area;
    assert!(
        area.width > 4 && area.height > 2,
        "the terminal must be laid out, or the click lands nowhere and this \
         test passes or fails for reasons unrelated to double-click tracking"
    );
    let (col, row) = (area.x + 2, area.y + 1);

    // First ctrl+click: arms the modified tracker, fires nothing (no `click`
    // row is bound), and must NOT arm the plain tracker.
    let mut first = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    first.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(first);
    assert_eq!(
        app.editor.wrap_enabled(),
        before,
        "the first click of the pair is not yet a double"
    );

    // Second ctrl+click at the same cell: now it is a double, so it fires.
    let mut second = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    second.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(second);
    assert_ne!(
        app.editor.wrap_enabled(),
        before,
        "ctrl+double_click must be reachable, not permanently dead"
    );
}

#[test]
fn the_at_click_commands_refuse_a_keyboard_invocation() {
    // Each reads the click through `last_mouse_pos`, which a palette or
    // chord invocation leaves None. They must say so rather than guess at
    // the caret and act somewhere the user never pointed.
    use crate::widgets::command_palette::Command;
    let (mut app, _tmp) = tree_app_with_keymap("[]");
    for (cmd, want) in [
        (
            Command::MouseAddCursorAtClick,
            "Mouse: Add Cursor at Click needs a mouse binding",
        ),
        (
            Command::MouseGoToDefinitionAtClick,
            "Mouse: Go to Definition at Click needs a mouse binding",
        ),
        (
            Command::MouseOpenLinkAtClick,
            "Mouse: Open Link at Click needs a mouse binding",
        ),
    ] {
        app.last_mouse_pos = None;
        app.status.clear();
        app.run_command(cmd);
        assert_eq!(app.status, want, "{cmd:?} must refuse a keyboard call");
    }
}

#[test]
fn an_unmatched_modified_click_does_not_arm_the_tracker() {
    // The shape that actually reaches the built-in `terminal_click.record`,
    // which has no modifier guard: a modified click that CLASSIFIES, matches
    // no binding, and is NOT the first half of a bound double-click. Binding
    // `alt+double_click` and clicking with CTRL gives exactly that — the
    // swallow branch declines (the bound double is ALT, the click is CTRL, so
    // `is_double_click_prefix` is false for this gesture), and the dispatch
    // falls through to the built-in for real.
    //
    // If `record` armed on it, the user's next ordinary click would select a
    // word they never gestured for.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "alt+double_click", "command": "toggle_word_wrap", "when": "terminal"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let area = app.terminals[0].last_area;
    assert!(
        area.width > 4 && area.height > 2,
        "terminal must be laid out"
    );
    // A line of output IN FRONT of the token, so the ordering that used to
    // break these tests is the one this test always runs (#397), and the cell
    // is SEARCHED rather than assumed.
    //
    // Assuming it made this test conditionally vacuous, which is worse than
    // plainly vacuous: measured both ways, with `ClickTracker::record`'s
    // modifier guard deliberately broken, it FAILED with the token on the
    // assumed row and PASSED with the token one row down. So it discriminated
    // whenever it was checked by hand and went blind precisely under load,
    // which is the only condition it exists to cover. The negative assertion
    // at the end is satisfied by a click that lands on blank and selects
    // nothing, whether or not the tracker armed.
    // One call rather than two, which is fewer moving parts and nothing more:
    // `cell_carrying` searches every cell of every row, so a prompt landing
    // between two feeds and prefixing the payload row is tolerated either way.
    // The sibling tests still feed their decoy and payload separately.
    //
    // The payload row OPENS with a shell-shaped prefix on purpose. It is what
    // gives the span predicate a red state: under a row-level `contains` the
    // search resolves the `b` of `bash-5.2$`, `select_word_at` takes the word
    // there instead of the token, and the positive control below fails by
    // name. Every other fixture in this file prints its needle at column 0,
    // where a row test and a span test pick the same cell and the refinement
    // cannot go red.
    app.terminals[0].feed_bytes_for_test(
        b"a prompt got here first\r\nbash-5.2$ hello_world_token some other text\r\n",
    );
    term.draw(|f| app.render(f)).unwrap();
    let (col, row) = cell_carrying(&app.terminals[0], "hello_world_token")
        .expect("the pane must show THIS test's token, or there is no word to mis-select");

    // Precondition: the CTRL click must match nothing, or it returns early
    // from the matched dispatch and never reaches the built-in.
    let ctrl_gesture = crate::keymap::Gesture {
        kind: crate::keymap::GestureKind::Click,
        mods: KeyModifiers::CONTROL,
    };
    assert!(
        app.keymap
            .command_for_mouse(ctrl_gesture, crate::keymap::MouseContext::Terminal)
            .is_none(),
        "ctrl+click must match nothing, or this never reaches the built-in"
    );
    assert!(
        !app.keymap
            .is_double_click_prefix(ctrl_gesture, crate::keymap::MouseContext::Terminal),
        "ctrl+click must NOT be a double-click prefix here, or the swallow \
         branch returns first and the built-in is never reached"
    );

    let mut ctrl = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ctrl.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ctrl);

    // A following PLAIN click must be a fresh single click, not the second
    // half of a double the modified click armed.
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col + 1, row));
    let selected = app.terminal().selection_text();
    assert!(
        !selected.contains("hello_world_token"),
        "a plain click after an unmatched ctrl+click must not select a WORD: \
         the ctrl+click must not have armed the double-click tracker, but \
         selection was {selected:?}"
    );

    // The POSITIVE half, over the same cell. Without it, "the tracker stayed
    // unarmed" and "word selection could never have happened here" are the
    // same green: a click landing somewhere unselectable satisfies the
    // negative whatever the tracker did.
    //
    // ONE more click, not two. The plain click above DID arm the tracker -
    // that is the behaviour being contrasted with the ctrl+click, which must
    // not - so this one pairs with it (`is_double` allows a one-column drift)
    // and selects the word. A second click would then start a FRESH
    // single-cell selection and wipe it, which is what the first draft of this
    // control did and why it failed.
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    // The WORD, exactly. `select_word_at` brackets the alphanumeric run
    // around the pivot and `selection_text` extracts that span, so a correct
    // select yields the token and nothing else; `contains` would also accept
    // a selection wider than the word, which is the shape a scroll-clock
    // rebase can produce. This PR tightened two status assertions for the
    // same reason, so its own new control should not be looser.
    assert_eq!(
        app.terminal().selection_text(),
        "hello_world_token",
        "a PLAIN click pairing with the plain click above must select the \
         word: the tracker arms on plain clicks and refuses only modified \
         ones, and without this the negative above proves nothing"
    );
}

#[test]
fn a_modified_click_that_drags_away_does_not_pair_with_the_click_it_returns_to() {
    // The built-in trackers get `clear_if_moved` on every drag, so click,
    // drag away, click back is not a double. `ModifiedClickTracker` has no
    // such call anywhere, so the modified pair fires where the identical
    // unmodified sequence correctly does not.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+double_click", "command": "toggle_word_wrap", "when": "terminal"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let area = app.terminals[0].last_area;
    assert!(
        area.width > 4 && area.height > 2,
        "terminal must be laid out"
    );
    let (col, row) = (area.x + 2, area.y + 1);

    let before = app.editor.wrap_enabled();
    let mut ctrl = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ctrl.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ctrl);

    // Drag well away, then release, then click the origin again.
    let mut drag = mouse(MouseEventKind::Drag(MouseButton::Left), col + 20, row + 3);
    drag.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(drag);
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        col + 20,
        row + 3,
    ));

    let mut back = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    back.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(back);

    assert_eq!(
        app.editor.wrap_enabled(),
        before,
        "a modified click that dragged away must not pair with the click it \
         returns to, but the double fired"
    );
}

/// An OSC 8 hyperlink's URI is invisible: the pane shows "CLICK-ME" while
/// the cell carries whatever the printing program chose — including a
/// file:// or custom-scheme URI that `open`/`xdg-open` would hand to an
/// arbitrary application. Only web links may open; anything else is
/// refused with the REAL destination surfaced in the status line.
#[test]
fn cmd_clicking_a_non_web_hyperlink_refuses_instead_of_opening() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    app.terminal_mut().focused = true;
    term.draw(|f| app.render(f)).unwrap();

    let script = "printf 'ref \\033]8;;file:///nonexistent-croft-probe\\033\\\\CLICK-ME\\033]8;;\\033\\\\\\n'";
    app.terminal_mut()
        .write_input(format!("{script}\r").as_bytes());
    // Locate the linked cell in the click's own coordinate space
    // (`hyperlink_at` viewport rows), not via `visible_text`, whose
    // logical-line joining shifts row indices past any soft wrap.
    let linked_cell = |app: &App| -> Option<(usize, usize)> {
        (0..40usize).find_map(|r| {
            (0..120usize)
                .find(|&c| {
                    app.terminal().hyperlink_at(r, c).as_deref()
                        == Some("file:///nonexistent-croft-probe")
                })
                .map(|c| (r, c))
        })
    };
    // Load-scaled rather than a fresh constant (#307): this budget was raised
    // 5s -> 8s once already, which is the shape of a number that cannot be
    // guessed from inside one test. The 1000ms literal that first replaced
    // it was under-calibrated against its own siblings and still lost on a
    // loaded box, so it now shares `SHELL_PAINT_BASE` with the other
    // wait-for-the-shell-to-paint test (#397).
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the shell to paint the linked cell",
        || linked_cell(&app).is_some(),
    );
    term.draw(|f| app.render(f)).unwrap();
    let (row_idx, col) = linked_cell(&app).expect("shell must print the linked text");

    let inner = app.terminal().last_inner;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: inner.x + col as u16,
        row: inner.y + row_idx as u16,
        modifiers: KeyModifiers::SUPER,
    });

    assert!(
        !app.status.starts_with("Opened"),
        "a file:// hyperlink must never reach the system opener (status: {})",
        app.status
    );
    assert!(
        app.status.contains("file:///nonexistent-croft-probe"),
        "the refusal must surface the real destination (status: {})",
        app.status
    );
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

/// A click on the cell being edited keeps the edit open: nothing commits.
#[test]
fn a_click_on_the_cell_being_edited_keeps_editing() {
    let (_tmp, _p, mut app) = prices_xlsx_app();
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_sheet(&mut app, "5");
    let (x, y) = sheet_cell_on_screen(&app, 0, 1);
    left_click(&mut app, x, y);
    let view = app.editor.sheet.as_ref().unwrap();
    assert!(view.editing.is_some());
    assert!(view.cell_edits.is_empty() && !app.editor.dirty);
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
fn finder_drop_into_terminal_pastes_path_instead_of_scp_in_remote_view() {
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
    // Focus sits in the embedded terminal (e.g. a Claude Code session)
    // while the Remote sidebar happens to be the active view. A Finder
    // drag-drop here must forward the path into the terminal so the
    // running program receives it, NOT hijack it into an scp upload to
    // the auto-selected host. Focus wins, mirroring the Explorer branch.
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);
    let editor_before = app.editor.lines.clone();
    app.handle_paste(&format!("{}\n", src.display()));
    assert!(
        app.take_pending_scp_uploads().is_empty(),
        "terminal-focused drop must not queue an scp upload",
    );
    assert_eq!(
        app.editor.lines, editor_before,
        "terminal-focused drop must not leak the path into the editor",
    );
    assert!(src.exists(), "the drop must not move or delete the source");
}

#[test]
fn finder_drop_into_editor_inserts_path_instead_of_scp_in_remote_view() {
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
    app.handle_paste(&format!("{}\n", src.display()));
    assert!(
        app.take_pending_scp_uploads().is_empty(),
        "editor-focused drop must not queue an scp upload",
    );
    assert!(
        app.editor.lines.iter().any(|l| l.contains("e.txt")),
        "editor-focused drop should insert the dropped path, was: {:?}",
        app.editor.lines,
    );
}

#[test]
fn finder_drop_onto_remote_tree_still_queues_scp() {
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
    // Dropping ONTO the Remote sidebar (tree focused) is the deliberate
    // upload gesture and must still queue an scp copy to the host.
    app.focus_pane(Pane::Tree);
    app.handle_paste(&format!("{}\n", src.display()));
    let queued = app.take_pending_scp_uploads();
    assert_eq!(queued.len(), 1, "one scp upload should be queued");
    assert_eq!(queued[0].alias, "alpha");
    assert_eq!(queued[0].src, src);
    assert!(src.exists(), "scp must copy, not move");
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

#[test]
fn pure_path_payload_distinguishes_finder_drag_from_text_paste() {
    // A genuine Finder drag carries only absolute path tokens.
    assert!(super::is_pure_path_payload("/Users/v/Documents/foo.txt"));
    assert!(super::is_pure_path_payload("/Users/v/a.txt /Users/v/b.txt"));
    assert!(super::is_pure_path_payload("'/Users/v/with space.txt'"));
    // Text pastes that merely contain a path always carry a non-path token, so
    // they must NOT be hijacked into a relay pull.
    assert!(!super::is_pure_path_payload("cat /etc/hosts"));
    assert!(!super::is_pure_path_payload("see /Users/v/foo.txt please"));
    assert!(!super::is_pure_path_payload("relative/path.txt"));
    assert!(!super::is_pure_path_payload(""));
}

/// #1585, negative: an Esc that clears a selection, a key between the two,
/// two Escapes too far apart, or Escapes typed into a terminal do not
/// leave Zen.
#[test]
fn an_esc_that_did_something_is_not_half_of_esc_esc() {
    let (_tmp, mut app) = zen_app();
    app.toggle_zen_mode();
    app.handle_key(key(KeyCode::Right, KeyModifiers::SHIFT))
        .unwrap();
    assert!(app.editor.selection.is_some(), "precondition: a selection");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.selection.is_none(), "the first Esc cleared it");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.zen_mode,
        "the Esc that cleared the selection did not count"
    );

    app.zen_esc_at = None;
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.zen_mode, "a key between the Escapes breaks the pair");

    app.zen_esc_at = None;
    let t = std::time::Instant::now();
    app.note_idle_zen_esc(t);
    app.note_idle_zen_esc(t + ZEN_ESC_WINDOW + std::time::Duration::from_millis(1));
    assert!(app.zen_mode, "too far apart");

    app.zen_esc_at = None;
    app.focus = Pane::Terminal;
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.zen_mode, "Escapes for a terminal program stay its own");
}

/// Clicking an annotated span pops its note, but that branch used to
/// return before the mouse-up cleanup: the zero-area selection the
/// mouse-down planted stayed painted as a one-cell highlight and the
/// pane-drag latch stayed armed.
#[test]
fn clicking_an_annotated_span_still_clears_the_click_selection() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A SILENT pane (#477). `App::new` spawns the user's interactive shell,
    // whose prompt lands whenever the machine gets round to it; a prompt
    // arriving between the anchor below and the click scrolled the
    // annotation off the row the click was aimed at, and the popup
    // assertion read as "annotation hover broke" on a merely busy host.
    // A child that prints nothing cannot move the grid, so the only bytes
    // on it are the ones this test feeds.
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("sleep 30")],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.terminals[0].feed_bytes_for_test(b"annotated words here\r\n");
    // Anchor the note to the grid line the text actually landed on and aim
    // the click at that same line, rather than assuming both are row 0.
    let line = app.terminals[0]
        .find_captured_line("annotated words here")
        .expect("the fed text is on the grid");
    assert!(
        line >= 0,
        "with no scrollback the text sits on a viewport row, got line {line}"
    );
    let clock = app.terminals[0].scroll_clock();
    app.terminals[0].add_annotation(line, clock, 0, 15, String::from("the note"));
    let inner = app.terminals[0].last_inner;
    let (cx, cy) = (inner.x + 2, inner.y + line as u16);
    assert!(
        app.terminals[0]
            .line_text_at(cx, cy)
            .is_some_and(|(text, idx)| text.starts_with("annotated words here") && idx == 2),
        "the click must land ON the annotated text, or the popup assertion is vacuous"
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), cx, cy));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), cx, cy));
    assert!(app.hover_popup.is_some(), "the click must pop the note");
    assert!(
        app.terminals[0].selection().is_none(),
        "the zero-area click selection must not stay painted"
    );
}

/// #414: a dropped `file://` URL naming a non-ASCII path must decode to that
/// path, not to one char per byte.
///
/// The decoder pushed each `%xx` byte, and each literal byte, `as char`,
/// which maps a byte to the Latin-1 code point of the same value, so every
/// multi-byte UTF-8 sequence came out as mojibake and the drop opened a
/// path that does not exist. The same defect class as #396, in the drop
/// path.
#[test]
fn a_dropped_file_url_with_a_non_ascii_path_decodes_to_utf8() {
    use std::path::PathBuf;
    assert_eq!(
        super::normalise_dropped_token("file:///tmp/caf%C3%A9/%E4%B8%AD%E6%96%87.txt"),
        Some(PathBuf::from("/tmp/caf\u{e9}/\u{4e2d}\u{6587}.txt")),
        "percent-encoded UTF-8 reassembles into the characters it encodes"
    );
    assert_eq!(
        super::normalise_dropped_token("file:///tmp/caf\u{e9}"),
        Some(PathBuf::from("/tmp/caf\u{e9}")),
        "a literal non-ASCII character in the URL survives too"
    );
    // The ASCII cases the decoder always handled are unchanged.
    assert_eq!(
        super::normalise_dropped_token("file:///tmp/a%20b"),
        Some(PathBuf::from("/tmp/a b"))
    );
    assert_eq!(
        super::normalise_dropped_token("'/tmp/x y'"),
        Some(PathBuf::from("/tmp/x y"))
    );
    // `+` is not a space in a file URL (form encoding does not apply), and a
    // `%` that is not an escape passes through literally, wherever it sits.
    for (url, want) in [
        ("file:///tmp/a+b", "/tmp/a+b"),
        ("file:///tmp/a%zzb", "/tmp/a%zzb"),
        ("file:///tmp/a%2", "/tmp/a%2"),
        ("file:///tmp/a%", "/tmp/a%"),
    ] {
        assert_eq!(
            super::normalise_dropped_token(url),
            Some(PathBuf::from(want)),
            "{url}"
        );
    }
}

/// A `file://` URL whose escapes do not form valid UTF-8 names no path this
/// process can open. The decoder used to read each byte as Latin-1 and open
/// the wrong path; a lossy decode would instead open a path with U+FFFD in
/// it, which is just as wrong and passes every "is this a path" screen. The
/// function's contract is to drop what is not a plausible path, so it does.
#[test]
fn a_dropped_file_url_with_invalid_utf8_is_dropped() {
    assert_eq!(super::normalise_dropped_token("file:///tmp/%FF"), None);
    assert_eq!(
        super::normalise_dropped_token("file:///tmp/%C3"),
        None,
        "a truncated multi-byte lead is not a path either"
    );
}

/// The terminal only reports focus in and out once asked to (mode 1004),
/// so the takeover turns it on and every restore path turns it back off.
#[test]
fn focus_reports_are_turned_on_at_startup_and_off_on_exit() {
    let takeover = String::from_utf8(takeover_mode_seq()).unwrap();
    assert!(
        takeover.contains("\x1b[?1004h"),
        "takeover must enable focus reports: {takeover:?}"
    );
    let restore = String::from_utf8_lossy(TERMINAL_RESTORE_SEQ);
    assert!(
        restore.contains("\x1b[?1004l"),
        "the restore sequence must disable focus reports"
    );
    assert!(
        !restore.contains("\x1b[?1004h"),
        "restoring must never re-enable them"
    );
}
