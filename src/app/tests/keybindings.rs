//! Keybindings: the keymap, default bindings, chords, recorded shortcuts and
//! macros.

use super::*;

#[test]
fn cmd_k_h_chords_fire_call_hierarchy_requests() {
    // Cmd+K H asks for incoming calls, Cmd+K Shift+H for outgoing; both
    // must arm the request id so the drain can match the reply.
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.py");
    std::fs::write(&f, "def target():\n    pass\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('h'), KeyModifiers::NONE)));
    let first = app
        .call_hierarchy_request_id
        .expect("incoming request armed");
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('H'), KeyModifiers::SHIFT)));
    let second = app
        .call_hierarchy_request_id
        .expect("outgoing request armed");
    assert!(second > first, "each chord fires a fresh request");
}

#[test]
fn a_bound_gesture_runs_its_command() {
    // The headline claim of #259, driven end to end: a gesture in
    // keybindings.json reaches `run_command`. Everything else in this file
    // tested the parser or the tracker in isolation.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp) = tree_app_with_keymap(
        r#"[{"key": "ctrl+click", "command": "quick_open", "when": "editor"}]"#,
    );
    assert!(app.file_finder.is_none(), "nothing open yet");

    let col = app.editor.last_inner.x + 4;
    let row = app.editor.last_inner.y + 1;
    let mut ev = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ev.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ev);

    assert!(
        app.file_finder.is_some(),
        "the bound command must run, taking the gesture over from its built-in"
    );
}

#[test]
fn a_gesture_runs_the_binding_for_the_region_it_was_clicked_in() {
    // The same spelling bound in two regions must resolve by where the click
    // landed, which is what `mouse_context_for` exists for.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp) = tree_app_with_keymap(
        r#"[{"key": "ctrl+click", "command": "quick_open", "when": "file_tree"},
            {"key": "ctrl+click", "command": "toggle_side_bar", "when": "editor"}]"#,
    );

    let mut in_tree = mouse(
        MouseEventKind::Down(MouseButton::Left),
        app.tree.last_area.x + 2,
        app.tree.last_area.y + 1,
    );
    in_tree.modifiers = KeyModifiers::CONTROL;
    let tree_was = app.show_tree;
    app.handle_mouse(in_tree);
    assert!(
        app.file_finder.is_some(),
        "a click in the tree runs the file_tree row"
    );
    assert_eq!(
        app.show_tree, tree_was,
        "and must not run the editor row bound to the same spelling"
    );
}

#[test]
fn a_click_binding_does_not_disable_the_double_click_binding_beside_it() {
    // The dispatch returns early, skipping the built-in branch that would
    // have recorded the click. Without recording it itself, a firing `click`
    // binding leaves `is_double` false forever and the `double_click` binding
    // beside it never fires again.
    //
    // Driven through `handle_mouse`: the previous version of this test called
    // `tree_click.record` itself and asserted `is_double`, which is a
    // tautology about `ClickTracker` that passes with the fix reverted.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp) = tree_app_with_keymap(
        r#"[{"key": "click", "command": "save_file", "when": "file_tree"},
            {"key": "double_click", "command": "quick_open", "when": "file_tree"}]"#,
    );
    let col = app.tree.last_area.x + 2;
    let row = app.tree.last_area.y + 1;

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert!(
        app.file_finder.is_none(),
        "the first click is a single and must run the `click` row"
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert!(
        app.file_finder.is_some(),
        "the second must still read as a double — the click binding has to \
         keep the tracker the built-in would have kept"
    );
}

#[test]
fn a_bound_gesture_dismisses_the_hover_and_tab_tooltip() {
    // The dispatch returns early, above the teardown every other left-press
    // path runs. Without doing it itself, a fired binding leaves a hover or
    // tab tooltip painted over state the command just changed — describing
    // what was there before it ran.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp) = tree_app_with_keymap(
        r#"[{"key": "ctrl+click", "command": "quick_open", "when": "editor"}]"#,
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

    assert!(app.file_finder.is_some(), "the binding fired");
    assert!(
        app.hover_popup.is_none(),
        "a fired binding must dismiss the hover it painted over"
    );
    assert!(app.tab_tooltip.is_none(), "and the tab tooltip with it");
}

#[test]
fn a_binding_on_the_terminal_border_fires_because_the_child_cannot_receive_it() {
    // `child_owns_pointer` declines a user binding when the child has asked
    // for mouse tracking. But `terminal_at_pos` hit-tests `last_area`, which
    // INCLUDES the border, while delivery goes through `cell_at`, which
    // hit-tests `last_inner` and excludes it. So a click on the border was
    // suppressed for the child's benefit and then never reached the child:
    // lost by both parties.
    //
    // The built-in wheel path already has this rule and says so
    // ("the wheel sat on the pane BORDER, outside the inner grid ... falls
    // through like any non-tracking pane instead of swallowing the notch"),
    // but it requires `report_mouse` to actually return true. This predicate
    // checked only `mouse_reporting()`.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp) = tree_app_with_keymap(
        r#"[{"key": "ctrl+click", "command": "toggle_word_wrap", "when": "terminal"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.focus_pane(Pane::Terminal);

    let area = app.terminals[0].last_area;
    assert!(
        area.width > 4 && area.height > 2,
        "terminal must be laid out"
    );
    let (col, row) = (area.x + 2, area.y);

    // The two facts that make this a BORDER click rather than a grid click.
    // Without both, the test would be about something else entirely.
    assert!(
        app.terminals[0].cell_at(col, row).is_none(),
        "precondition: this cell must be OUTSIDE the inner grid, or the child \
         could receive it and suppression would be correct"
    );
    assert_eq!(
        app.terminal_at_pos(col, row),
        Some(0),
        "precondition: but INSIDE last_area, or `child_owns_pointer` never \
         considers this terminal and the test proves nothing"
    );

    // Child asks for tracking: the real predicate `mouse_reporting` consults.
    app.terminals[0].feed_bytes_for_test(b"\x1b[?1000h");
    assert!(
        app.terminals[0].mouse_reporting(),
        "precondition: the child must be tracking, or there is nothing to \
         decline the binding for"
    );

    let before = app.editor.wrap_enabled();
    let mut ev = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ev.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ev);

    assert_ne!(
        app.editor.wrap_enabled(),
        before,
        "a binding on the BORDER must fire: the child cannot own a cell it \
         will never be told about, so declining here loses the gesture for \
         both parties. The wheel path already falls through on this exact case"
    );
}

#[test]
fn every_mouse_binding_the_docs_show_actually_loads() {
    // Round 7 found `paste` and `page_up` in this example, neither of which is
    // a real command id: `Keymap::from_json` skipped both rows with a warning,
    // so a user copying the documented snippet got a keymap that silently did
    // less than it said. A doc example that does not load is worse than none,
    // and the only way that stays true is to load the real file here rather
    // than a copy that can drift from it.
    let doc = include_str!("../../../docs/KEYBINDINGS.md");
    let block = doc
        .split("## Mouse bindings in `keybindings.json`")
        .nth(1)
        .expect("the mouse bindings section is still in KEYBINDINGS.md")
        .split("```jsonc")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("the section still carries a jsonc example block");
    assert!(
        block.contains("\"command\""),
        "the extracted block must be the binding example, not an empty match"
    );

    let (_km, warnings) = crate::keymap::Keymap::resolve(block);
    assert!(
        warnings.is_empty(),
        "the documented example must load cleanly, but got: {warnings:?}"
    );
}

#[test]
fn a_bound_gesture_moves_keyboard_focus_to_the_pane_it_clicked() {
    // Round 7: the dispatch made the clicked terminal pane ACTIVE (so commands
    // read the right grid) but never called `focus_pane`, which is a different
    // thing — it decides where the next KEYSTROKE goes. So a bound gesture
    // clicked into the terminal ran against the terminal while the user's
    // typing kept going to the editor they had left.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+click", "command": "mouse_open_link_at_click", "when": "terminal"}]"#,
    );
    app.focus_pane(Pane::Editor);
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let area = app.terminals[0].last_area;
    assert!(area.width > 4 && area.height > 2, "the terminal is visible");
    let mut ev = mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.x + 2,
        area.y + 1,
    );
    ev.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ev);

    // `matches!` rather than `assert_eq!`: `Pane` has no `Debug`, and deriving
    // one on a production type just to format a test failure is the wrong
    // trade.
    assert!(
        matches!(app.focus, Pane::Terminal),
        "clicking a pane with a bound gesture must focus it, or the next \
         keystroke goes to the pane the user just clicked away from"
    );
}

#[test]
fn a_bound_wheel_gesture_fires_and_a_modified_wheel_does_not_match_the_bare_row() {
    // The wheel is the one gesture family whose dispatch no other test drives
    // end to end — the rest of the coverage stops at the parser. It is also
    // the family where a regression is invisible: a wheel that silently falls
    // through to the built-in scroll looks like ordinary scrolling.
    //
    // Modifiers are part of a gesture's identity, so a bare `wheel_up` row
    // must not answer a ctrl+wheel_up. That is the same rule the shift test
    // above pins for clicks, checked here for the wheel.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp) =
        tree_app_with_keymap(r#"[{"key": "wheel_up", "command": "quick_open", "when": "editor"}]"#);
    let _ = MouseButton::Left;

    let (col, row) = (app.editor.last_inner.x + 4, app.editor.last_inner.y + 1);
    let mut modified = mouse(MouseEventKind::ScrollUp, col, row);
    modified.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(modified);
    assert!(
        app.file_finder.is_none(),
        "ctrl+wheel_up must not match a bare `wheel_up` row"
    );

    app.handle_mouse(mouse(MouseEventKind::ScrollUp, col, row));
    assert!(
        app.file_finder.is_some(),
        "a bare wheel_up binding must fire through the dispatch"
    );
}

#[test]
fn a_bound_gesture_resolves_the_link_in_the_pane_it_clicked_not_the_active_one() {
    // Splits make several terminals visible at once, and every terminal
    // command reads the grid through `self.terminal()`, i.e. the ACTIVE
    // pane. The built-in ctrl+click makes the clicked pane active before
    // resolving the link; the binding dispatch returns above that point, so
    // without its own activation a bound `mouse_open_link_at_click` inspects
    // whichever pane happened to be focused and reports "No link there" over
    // a URL the user can plainly see.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+click", "command": "mouse_open_link_at_click", "when": "terminal"}]"#,
    );
    app.split_terminal().unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(app.terminals.len(), 2, "two panes are visible");

    // The URL is printed into pane 0 only, while pane 1 holds focus.
    // An OSC 8 hyperlink with a NON-WEB scheme, not a printed http(s) URL.
    // `terminal_url_click` checks `hyperlink_at_screen` before text sniffing,
    // so this resolves; and `open_detected_url` then refuses a non-web scheme
    // inertly. A printed https URL would reach `open_url`, which is
    // `Command::new("open")` with no test guard — the suite would launch a
    // real browser on every run (#307's spawning class).
    app.terminals[0].feed_bytes_for_test(b"a prompt got here first\r\n");
    // A DECOY link on an earlier row. It pins the exact-URI predicate the
    // same way it is pinned in the split-feed siblings: under `.is_some()`
    // the row-major search hits the decoy, the click resolves it, and the
    // `zero@` status assertion below fails by name. The decoy goes into pane
    // 0 alongside the payload, so it cannot weaken the "WHICH pane resolved"
    // claim: pane 1 still carries no link at all.
    app.terminals[0]
        .feed_bytes_for_test(b"\x1b]8;;mailto:decoy@example.com\x1b\\decoy\x1b]8;;\x1b\\\r\n");
    app.terminals[0]
        .feed_bytes_for_test(b"\x1b]8;;mailto:zero@example.com\x1b\\zero-link\x1b]8;;\x1b\\\r\n");
    app.active_terminal = 1;
    term.draw(|f| app.render(f)).unwrap();

    // Click directly on the URL, in pane 0, the pane that is NOT active.
    //
    // The cell is SEARCHED FOR rather than computed as "first row after the
    // border". Pane 0 runs a real shell, so its prompt races the fed bytes:
    // when the prompt lands first the printed line is pushed down a row and
    // the assumed cell holds nothing, which is a flake under full-suite load
    // and passes every time in isolation (#397). The `a prompt got here
    // first` line above makes that order permanent instead of occasional, so
    // this test now exercises the case that used to break it.
    //
    // Searching costs nothing here: the claim is "clicking the link in pane 0
    // resolves against pane 0", and any cell carrying that link is a fair
    // place to click. The precondition survives, because a pane with no such
    // cell fails below rather than clicking somewhere arbitrary.
    let (col, row) = cell_with_link(&app.terminals[0], "mailto:zero@example.com").expect(
        "pane 0 must carry the OSC 8 link somewhere in its area, or a refusal proves nothing",
    );
    let mut ev = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ev.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ev);

    // Positive form, not `assert_ne!("No link there")`: excluding one string
    // cannot tell "resolved pane 0's link" from "resolved nothing and said so
    // differently". The `zero@` address also pins WHICH pane resolved — pane 1
    // holds focus and carries no link.
    assert_eq!(
        app.status, "Refused to open non-web link: mailto:zero@example.com",
        "the bound command must resolve against the CLICKED pane; it read the active one"
    );
    assert_eq!(
        app.active_terminal, 0,
        "clicking a pane through a binding makes it active, as the built-in does"
    );
}

#[test]
fn a_modified_click_binding_does_not_arm_the_plain_double_click() {
    // `GestureKind::Click` covers `ctrl+click` too, and the built-ins read
    // their trackers without consulting modifiers. Recording a modified click
    // armed the editor's plain double-click, so the user's next ordinary
    // click selected a word they never gestured for.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("t.rs");
    std::fs::write(&file, "let name = value;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+click", "command": "save_file", "when": "editor"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    let col = app.editor.last_inner.x + 12;
    let row = app.editor.last_inner.y;
    let mut ctrl = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ctrl.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ctrl);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));

    // A plain click always anchors a collapsed selection (anchor == head);
    // that is true with no binding loaded too. The defect is a non-empty one,
    // i.e. a WORD the user never gestured for.
    let selected_a_word = app.editor.selection.is_some_and(|s| s.anchor != s.head);
    assert!(
        !selected_a_word,
        "a plain click after a bound ctrl+click must not select a word, got {:?}",
        app.editor.selection
    );
}

#[test]
fn the_first_click_of_a_bound_modified_pair_does_not_fire_the_builtin() {
    // The dispatch guard is `Some((ctx, gesture, Some(cmd)))`. When a gesture
    // classifies but matches no command — exactly the first click of a
    // `ctrl+double_click` pair, which by construction can only be a
    // `ctrl+click` — the pattern fails, nothing returns early, and control
    // reaches the built-in Ctrl+click arm. So binding ONLY the double gave a
    // spurious Go to Definition on every first click: the gesture was
    // recognised as half of a bound pair and then leaked into the built-in it
    // was supposed to be part of.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("t.rs");
    std::fs::write(&file, "let name = value;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+double_click", "command": "save_file", "when": "editor"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    let (col, row) = (app.editor.last_inner.x + 12, app.editor.last_inner.y);
    let (before_row, before_col) = (app.editor.cursor_row, app.editor.cursor_col);
    let mut ctrl = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ctrl.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ctrl);

    // `trigger_definition_at` moves the caret to the clicked position before
    // asking the server, so an unmoved caret is the observable proof the
    // built-in did not run. Asserting on the LSP request itself would need a
    // live server; the caret is the same signal without one.
    assert_eq!(
        (app.editor.cursor_row, app.editor.cursor_col),
        (before_row, before_col),
        "the first click of a bound ctrl+double_click must not reach the \
         built-in Ctrl+click Go to Definition, but the caret moved as though \
         it had"
    );
}

#[test]
fn a_modified_click_binding_does_not_arm_the_plain_double_click_in_the_terminal() {
    // `terminal_click.record` has no modifier guard, so a ctrl+click that
    // REACHES it arms the plain double-click tracker and the user's next
    // ordinary click selects a word they never gestured for.
    //
    // What this test ACTUALLY covers is the swallow branch, not the built-in.
    // With only `ctrl+double_click` bound, the pair's first click matches no
    // command, `is_double_click_prefix` is therefore true, and the SWALLOW
    // branch returns before the built-in path runs — so `terminal_click.record`
    // is never reached here. Deleting the swallow branch does fail this test,
    // which is what it guards: the prefix click must not arm the tracker.
    //
    // The unguarded `record` itself is covered separately by
    // `an_unmatched_modified_click_does_not_arm_the_tracker`, which uses a
    // gesture that classifies, matches nothing, and is NOT a double-click
    // prefix — the only shape that actually reaches the built-in.
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
        "the terminal must be laid out, or the clicks land nowhere and this \
         test passes for reasons unrelated to tracker arming"
    );

    // There must be a WORD under the pointer. `select_word_at` is what a
    // spuriously-armed tracker triggers, and it selects nothing on a blank
    // cell — so clicking an empty terminal makes this test pass vacuously
    // whether the tracker armed or not. Print text first, and assert it
    // arrived, so the click has a real word to wrongly select.
    // A line of output IN FRONT of the token, so the ordering that used to
    // break these tests is the one this test always runs (#397).
    app.terminals[0].feed_bytes_for_test(b"a prompt got here first\r\nhello_world_token\r\n");
    term.draw(|f| app.render(f)).unwrap();
    // SEARCHED, not assumed. This test already carried a positive control, so
    // a displaced token failed it outright rather than passing vacuously - the
    // flaking half of the same defect its sibling had the vacuous half of.
    let (col, row) = cell_carrying(&app.terminals[0], "hello_world_token")
        .expect("the pane must show THIS test's token, or there is no word to mis-select");
    // No re-derivation of `cell_carrying`'s own predicate here. It ran that
    // exact span test on this pane and this cell to produce `(col, row)`, so
    // repeating it can only fail if the live shell scrolled the grid between
    // the search and the re-read - a different fault from the one such an
    // assertion would name.
    //
    // The span property is not pinned HERE. This test's payload prints at
    // column 0, where a row predicate and a span predicate select the same
    // cell, and both assertions below are negatives. Its red state lives in
    // `an_unmatched_modified_click_does_not_arm_the_tracker`, whose payload
    // row opens with a shell prefix and whose positive control fails by name
    // under a row predicate.

    let mut ctrl = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ctrl.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ctrl);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));

    // A plain click ALWAYS anchors a collapsed one-cell selection
    // (`start_selection_at`), so the correct outcome here is the single
    // character under the pointer — not an empty string. The defect is the
    // tracker firing `select_word_at`, which yields the whole WORD.
    //
    // Asserting emptiness would fail on correct behaviour, so the assertion
    // is against the word: anything longer than the one anchored cell means a
    // double-click fired off a click the user never paired.
    let selected = app.terminal().selection_text();
    assert!(
        !selected.contains("hello_world_token"),
        "a plain click after a bound ctrl+click must not select a WORD in the \
         terminal — the ctrl+click armed the plain double-click tracker. \
         Expected the one anchored cell, got {selected:?}"
    );
    assert!(
        selected.chars().count() <= 1,
        "a plain click anchors exactly one cell; a longer selection means the \
         tracker fired a double-click, got {selected:?}"
    );
}

#[test]
fn a_bound_click_defers_to_a_mouse_tracking_child() {
    // `child_owns_pointer` covers a bound CLICK the same way it covers the
    // wheel, because since #474 the left button is a gesture croft actually
    // forwards: the Down(Left) arm sends the press to a tracking child. So a
    // `ctrl+click` binding must decline over a tracking child -- the child
    // asked for the pointer -- and the press must reach it with the Ctrl bit
    // set, exactly as it would under a bare terminal emulator. SHIFT remains
    // the bypass (`holding_shift_takes_the_pointer_back_from_a_tracking_child`).
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+click", "command": "toggle_word_wrap", "when": "terminal"}]"#,
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

    app.terminals[0].feed_bytes_for_test(b"\x1b[?1000h\x1b[?1006h");
    assert!(
        app.terminals[0].mouse_reporting(),
        "the child must be tracking, or this test says nothing about the guard"
    );
    let Some((lr, lc)) = app.terminals[0].cell_at(col, row) else {
        panic!(
            "the click must land on a real grid cell, or the guard declines for the \
             border reason instead of the tracking one and the test is vacuous"
        );
    };

    let before = app.editor.wrap_enabled();
    let mut ctrl = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ctrl.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ctrl);

    assert_eq!(
        app.editor.wrap_enabled(),
        before,
        "a bound ctrl+click over a tracking child must NOT run: the child asked \
         for the pointer and croft forwards the press to it"
    );
    let expected = format!("\x1b[<16;{};{}M", lc + 1, lr + 1);
    assert_eq!(
        String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()),
        expected,
        "the press reaches the child as an SGR report carrying the Ctrl bit"
    );
}

#[test]
fn a_reloaded_keymap_with_a_refused_row_reports_its_warning_count() {
    // Asserts the summary the reload sets, against a keymap built in memory.
    // Writing the real `~/.config/croft/keybindings.json` — which is what the
    // previous version of this test did — leaks into every concurrently
    // running test that builds an `App`, because `App::new` loads that path
    // unconditionally.
    let (_refused, warns) = crate::keymap::Keymap::resolve(
        r#"[{"key": "cmd+click", "command": "save_file", "when": "editor"}]"#,
    );
    assert_eq!(warns.len(), 1, "the Cmd row is refused with a reason");

    let status = super::keybindings_reload_status(&warns);
    assert!(
        status.contains("1 warning") && status.contains("OUTPUT"),
        "the count must reach the status bar and point at the channel, got {status:?}"
    );
    assert!(
        !super::keybindings_reload_status(&[]).contains("warning"),
        "a clean reload says nothing about warnings"
    );
}

#[test]
fn a_cmd_or_triple_gesture_is_refused_rather_than_bound_to_a_dead_entry() {
    // Both parse but can never match, so binding them would make
    // `has_mouse_bindings()` true for entries nothing can look up — and
    // would let cmd+click dodge the reserved bare-click check.
    let (km, warnings) = crate::keymap::Keymap::resolve(
        r#"[{"key": "cmd+click", "command": "save_file"},
            {"key": "triple_click", "command": "save_file"}]"#,
    );
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        !km.has_mouse_bindings(),
        "neither is bound, so the per-event dispatch stays off"
    );
    assert!(
        warnings.iter().any(|w| w.contains("second click")),
        "triple_click says the real reason, not 'not a gesture': {warnings:?}"
    );
}

#[test]
fn delete_key_is_recognized_as_delete_node() {
    assert!(is_delete_node_key(key(KeyCode::Delete, KeyModifiers::NONE)));
}

#[test]
fn reveal_in_finder_key_is_opt_cmd_r_and_disjoint_from_rename() {
    // Cmd+Opt+R (and Ctrl+Opt+R) reveal in Finder.
    assert!(is_reveal_in_finder_key(key(
        KeyCode::Char('r'),
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
    assert!(is_reveal_in_finder_key(key(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL | KeyModifiers::ALT
    )));
    // Plain Cmd+R must NOT trigger reveal - that chord is Rename.
    assert!(!is_reveal_in_finder_key(key(
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
    // Every menu row carries an accelerator (croft tenet: no blank column).
    // Color Theme, Close to the Right, and the compare actions are Cmd+K chords.
    assert_eq!(shortcut_for(&MenuAction::OpenThemePicker), Some("⌘K ⌘T"));
    assert_eq!(shortcut_for(&MenuAction::CloseTabsToRight(0)), Some("⌘K →"));
    assert_eq!(
        shortcut_for(&MenuAction::SelectForCompare(p.clone())),
        Some("⌘K S")
    );
    assert_eq!(
        shortcut_for(&MenuAction::CompareWithSelected {
            anchor: p.clone(),
            other: p.clone()
        }),
        Some("⌘K C")
    );
    assert_eq!(
        shortcut_for(&MenuAction::EditBreakpointConditionAt { line: 0 }),
        Some("⇧F9")
    );
    // Editor tab-menu actions: Close Saved and Reveal in Explorer View are
    // Cmd+K chords; Copy Path is a standalone Cmd+Opt chord.
    assert_eq!(shortcut_for(&MenuAction::CloseSavedTabs), Some("⌘K U"));
    assert_eq!(
        shortcut_for(&MenuAction::CopyTabPath(p.clone())),
        Some("⌥⌘C")
    );
    assert_eq!(
        shortcut_for(&MenuAction::RevealInExplorer(p.clone())),
        Some("⌘K E")
    );
}

#[test]
fn cmd_k_arms_leader_then_unmatched_second_key_clears_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.cmd_k_leader.is_some(), "Cmd+K arms the chord leader");
    // A key that completes no Cmd+K chord clears the leader and falls through
    // to its normal meaning.
    app.handle_key(key(KeyCode::Char('q'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.cmd_k_leader.is_none(),
        "an unmatched second key clears the leader"
    );
}

/// #843: on desktop Linux `Ctrl+K` `B` opens Testing and leaves the buffer
/// alone, while a focused shell and the vim-mode editor keep `Ctrl+K`, and
/// the palette still kills to the end of the line.
#[cfg(not(target_os = "macos"))]
#[test]
fn ctrl_k_chords_reach_croft_off_macos_but_not_from_the_shell_or_vim() {
    use crate::widgets::command_palette::Command;
    use crate::widgets::list_picker::ListPurpose;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.txt");
    std::fs::write(&path, "keep this line").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&path).unwrap();
    let ctrl = |c| key(KeyCode::Char(c), KeyModifiers::CONTROL);
    let at_col_5 = |app: &mut App| {
        app.focus_pane(Pane::Editor);
        app.editor.lines = vec![String::from("keep this line")];
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 5;
    };

    at_col_5(&mut app);
    app.handle_key(ctrl('k')).unwrap();
    assert!(app.cmd_k_leader.is_some(), "Ctrl+K arms the leader");
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.sidebar_view, SidebarView::Testing, "Ctrl+K B: Testing");
    assert_eq!(
        app.editor.lines,
        ["keep this line"],
        "nothing killed or typed"
    );

    // Ctrl held on the second key counts as Cmd: VS Code's Linux Ctrl+K
    // Ctrl+S opens the Keyboard Shortcuts editor.
    at_col_5(&mut app);
    app.handle_key(ctrl('k')).unwrap();
    app.handle_key(ctrl('s')).unwrap();
    assert!(
        app.list_picker
            .as_ref()
            .is_some_and(|p| p.purpose == ListPurpose::KeyboardShortcuts)
    );
    app.list_picker = None;

    // The palette still kills to the end of the line.
    at_col_5(&mut app);
    app.run_command(Command::KillToEndOfLine);
    assert_eq!(app.editor.lines, ["keep "]);

    // Vim mode keeps Ctrl+K as the editor's kill-line.
    at_col_5(&mut app);
    app.vim.enabled = true;
    app.handle_key(ctrl('k')).unwrap();
    assert!(app.cmd_k_leader.is_none(), "vim mode: Ctrl+K does not lead");
    assert_eq!(app.editor.lines, ["keep "], "vim mode: Ctrl+K kills");
    app.vim.enabled = false;

    // A focused shell gets Ctrl+K for itself.
    app.bottom_panel_tab = BottomPanelTab::Terminal;
    app.focus_pane(Pane::Terminal);
    app.handle_key(ctrl('k')).unwrap();
    assert!(
        app.cmd_k_leader.is_none(),
        "the shell's Ctrl+K is not captured"
    );
}

/// #843: LINUX.md promises that off macOS a `Cmd` chord "works as the same
/// chord with `Ctrl`", except the chords its table lists. This walks every
/// `is_*_key` predicate over every `Cmd` chord it takes and checks the `Ctrl`
/// spelling: taken too, unless the table lists the chord, in which case it
/// must really not be `Ctrl` (the negative half). The table is read from
/// LINUX.md itself, so a new Super-only chord fails here until it gets a
/// `Ctrl` form or a row, and a row fails once its chord gains a `Ctrl` form.
#[cfg(not(target_os = "macos"))]
#[test]
fn every_cmd_chord_is_ctrl_off_macos_unless_linux_md_lists_it() {
    use std::collections::BTreeSet;
    type Pred = fn(KeyEvent) -> bool;
    macro_rules! preds {
        ($($p:ident),* $(,)?) => { vec![$((stringify!($p), $p as Pred)),*] };
    }
    // The leader asked from the editor and from a focused shell.
    fn leader_in_editor(key: KeyEvent) -> bool {
        is_cmd_k_leader_key(key, ctrl_k_leads(false, false, false))
    }
    fn leader_in_shell(key: KeyEvent) -> bool {
        is_cmd_k_leader_key(key, ctrl_k_leads(false, true, false))
    }
    // `is_save_key` also takes whether Shift is reported with Ctrl.
    fn is_save_key_reporting_shift(key: KeyEvent) -> bool {
        is_save_key(key, true)
    }
    let mut predicates: Vec<(&str, Pred)> = preds![
        is_terminal_copy_key,
        is_compare_key,
        is_search_jump_key,
        is_editor_find_key,
        is_terminal_find_key,
        is_editor_replace_key,
        is_file_finder_key,
        is_command_palette_key,
        is_go_to_symbol_key,
        is_focus_breadcrumbs_key,
        is_toggle_line_comment_key,
        is_toggle_block_comment_key,
        is_toggle_wrap_key,
        is_switch_header_source_key,
        is_tree_zoxide_jump_key,
        is_tree_new_file_key,
        is_tree_new_folder_key,
        is_tree_rename_key,
        is_reveal_in_finder_key,
        is_copy_path_key,
        is_tree_make_root_key,
        is_tree_make_parent_root_key,
        is_explorer_jump_key,
        is_source_control_jump_key,
        is_run_debug_jump_key,
        is_remote_jump_key,
        is_extensions_jump_key,
        is_drop_to_local_key,
        is_run_build_task_key,
        is_markdown_preview_key,
        is_run_fence_key,
        is_sidebar_toggle_key,
        is_secondary_sidebar_toggle_key,
        is_minimap_toggle_key,
        is_terminal_toggle_key,
        is_terminal_maximize_key,
        is_quick_select_key,
        is_copy_mode_key,
        is_command_history_key,
        is_terminal_split_key,
        is_editor_split_key,
        is_goto_bracket_key,
        is_select_to_bracket_key,
        is_navigate_back_key,
        is_navigate_forward_key,
        is_transpose_key,
        is_indentation_to_spaces_key,
        is_indentation_to_tabs_key,
        is_trim_final_newlines_key,
        is_toggle_bookmark_key,
        is_switch_debug_session_key,
        is_clear_bookmarks_key,
        is_next_bookmark_key,
        is_prev_bookmark_key,
        is_join_lines_key,
        is_transform_upper_key,
        is_transform_lower_key,
        is_copy_relative_path_key,
        is_sort_lines_asc_key,
        is_sort_lines_desc_key,
        is_trim_trailing_whitespace_key,
        is_increment_number_key,
        is_decrement_number_key,
        is_format_document_key,
        is_emmet_expand_key,
        is_quick_fix_key,
        is_focus_group_left_key,
        is_focus_group_right_key,
        is_terminal_focus_key,
        is_terminal_close_key,
        is_terminal_cycle_key,
        is_terminal_cycle_back_key,
        is_delete_node_key,
        is_completion_trigger_key,
        is_save_all_key,
        is_new_untitled_file_key,
        is_replace_in_files_key,
        is_search_details_key,
        is_show_output_key,
        is_show_problems_key,
        is_update_croft_key,
        is_editor_copy_key,
        is_editor_cut_key,
        is_search_paste_key,
        is_editor_paste_key,
        is_clipboard_paste_key,
        is_editor_select_all_key,
        is_editor_undo_key,
        is_editor_redo_key,
        is_rename_symbol_key,
        is_change_all_occurrences_key,
        is_select_all_occurrences_key,
        is_add_cursors_to_line_ends_key,
        is_select_next_occurrence_key,
        is_delete_line_key,
        is_go_to_definition_key,
        is_peek_definition_key,
        is_go_to_references_key,
        is_run_to_cursor_key,
        is_peek_references_key,
        is_go_to_declaration_key,
        is_go_to_type_definition_key,
        is_go_to_implementation_key,
        is_vim_toggle_key,
        is_editor_line_home_key,
        is_editor_line_end_key,
        is_editor_kill_to_eol_key,
        is_editor_kill_to_bol_key,
        is_editor_open_line_below_key,
        is_editor_open_line_above_key,
        is_close_tab_key,
    ];
    predicates.push(("is_save_key", is_save_key_reporting_shift));
    predicates.push(("is_cmd_k_leader_key", leader_in_editor));
    predicates.push(("is_cmd_k_leader_key@shell", leader_in_shell));

    // Every predicate the key router defines is walked (the scan F1's own
    // coverage test uses), so a new one cannot slip past unexamined.
    const APP_SRC: &str = include_str!("../mod.rs");
    let defined: BTreeSet<&str> = APP_SRC
        .match_indices("\nfn is_")
        .filter_map(|(i, _)| {
            let rest = &APP_SRC[i + 4..];
            let name = &rest[..rest.find('(')?];
            name.ends_with("_key").then_some(name)
        })
        .collect();
    let walked: BTreeSet<&str> = predicates
        .iter()
        .map(|(n, _)| n.split('@').next().unwrap())
        .collect();
    assert_eq!(defined, walked, "walk every is_*_key predicate");

    // How a LINUX.md row's chord lacks a `Ctrl` form.
    #[derive(Clone, Copy)]
    enum NoCtrl {
        /// The predicate refuses the `Ctrl` spelling.
        Refused,
        /// The predicate would take it, but this one takes it first there.
        TakenBy(&'static str, Pred),
        /// croft takes `Ctrl`, but a legacy terminal sends the bare key.
        LegacySendsBareKey,
    }
    struct Row {
        /// The row's first cell in LINUX.md's table.
        doc: &'static str,
        pred: &'static str,
        /// The chord's key, and its modifiers besides `Cmd` (every
        /// combination the predicate takes when empty).
        code: &'static [KeyCode],
        mods: &'static [KeyModifiers],
        how: NoCtrl,
    }
    let any: &[KeyModifiers] = &[];
    let rows = [
        Row {
            doc: "`Cmd`+`\\` split editor",
            pred: "is_editor_split_key",
            code: &[KeyCode::Char('\\')],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`Shift`+`\\` / `Cmd`+`Opt`+`\\` go to / select to bracket",
            pred: "is_goto_bracket_key",
            code: &[KeyCode::Char('\\')],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`Shift`+`\\` / `Cmd`+`Opt`+`\\` go to / select to bracket",
            pred: "is_select_to_bracket_key",
            code: &[KeyCode::Char('\\')],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`Opt`+`←` / `→` focus the left / right editor group",
            pred: "is_focus_group_left_key",
            code: &[KeyCode::Left],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`Opt`+`←` / `→` focus the left / right editor group",
            pred: "is_focus_group_right_key",
            code: &[KeyCode::Right],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`]` / `Cmd`+`[` next / previous terminal",
            pred: "is_terminal_cycle_key",
            code: &[KeyCode::Char(']'), KeyCode::Char('}')],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`]` / `Cmd`+`[` next / previous terminal",
            pred: "is_terminal_cycle_back_key",
            code: &[KeyCode::Char('['), KeyCode::Char('{')],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`T` split terminal",
            pred: "is_terminal_split_key",
            code: &[KeyCode::Char('t')],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd+K` chords typed in the terminal pane",
            pred: "is_cmd_k_leader_key@shell",
            code: &[KeyCode::Char('k')],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`E` toggle vim mode",
            pred: "is_vim_toggle_key",
            code: &[KeyCode::Char('e')],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`A` select all, in the editor",
            pred: "is_editor_select_all_key",
            code: &[KeyCode::Char('a')],
            mods: &[KeyModifiers::NONE, KeyModifiers::SHIFT],
            how: NoCtrl::TakenBy("is_editor_line_home_key", is_editor_line_home_key),
        },
        Row {
            doc: "`Cmd`+`Shift`+`T` focus the terminal",
            pred: "is_terminal_focus_key",
            code: &[KeyCode::Char('t')],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`C` / `Cmd`+`W` copy the selection / close the active terminal, in the terminal pane",
            pred: "is_terminal_copy_key",
            code: &[KeyCode::Char('c')],
            mods: &[KeyModifiers::NONE, KeyModifiers::ALT],
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`C` / `Cmd`+`W` copy the selection / close the active terminal, in the terminal pane",
            pred: "is_terminal_close_key",
            code: &[KeyCode::Char('w')],
            mods: &[KeyModifiers::NONE, KeyModifiers::ALT],
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`F12` go to implementations",
            pred: "is_go_to_implementation_key",
            code: &[KeyCode::F(12)],
            mods: any,
            how: NoCtrl::Refused,
        },
        Row {
            doc: "`Cmd`+`Enter` run the Markdown code block under the caret",
            pred: "is_run_fence_key",
            code: &[KeyCode::Enter],
            mods: any,
            how: NoCtrl::LegacySendsBareKey,
        },
    ];

    // Every key a chord is spelled with, and every modifier set besides Cmd.
    let mut codes: Vec<KeyCode> = ('a'..='z')
        .chain('A'..='Z')
        .chain('0'..='9')
        .chain("`-=[]\\;',./~!@#$%^&*()_+{}|:\"<>?".chars())
        .map(KeyCode::Char)
        .collect();
    codes.extend([
        KeyCode::Enter,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Esc,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Insert,
    ]);
    codes.extend((1..=12).map(KeyCode::F));
    let extras = [
        KeyModifiers::NONE,
        KeyModifiers::SHIFT,
        KeyModifiers::ALT,
        KeyModifiers::SHIFT | KeyModifiers::ALT,
    ];
    let same_key = |a: KeyCode, b: KeyCode| match (a, b) {
        (KeyCode::Char(a), KeyCode::Char(b)) => a.eq_ignore_ascii_case(&b),
        _ => a == b,
    };
    let row_for = |pred: &str, code: KeyCode, extra: KeyModifiers| {
        rows.iter().find(|r| {
            r.pred == pred
                && r.code.iter().any(|c| same_key(*c, code))
                && (r.mods.is_empty() || r.mods.contains(&extra))
        })
    };
    let spell = |k: KeyEvent| format!("{:?}+{:?}", k.modifiers, k.code);

    let mut broken = Vec::new();
    let mut listed_seen = BTreeSet::new();
    for (name, pred) in &predicates {
        for &code in &codes {
            for extra in extras {
                let cmd = key(code, KeyModifiers::SUPER | extra);
                if !pred(cmd) {
                    continue;
                }
                let ctrl = key(code, KeyModifiers::CONTROL | extra);
                let Some(row) = row_for(name, code, extra) else {
                    if !pred(ctrl) {
                        broken.push(format!(
                            "{name}: takes {} but not {} (give it a Ctrl form, or a LINUX.md row)",
                            spell(cmd),
                            spell(ctrl)
                        ));
                    }
                    continue;
                };
                listed_seen.insert((row.doc, row.pred));
                // The negative half: a listed chord really has no Ctrl form.
                let holds = match row.how {
                    NoCtrl::Refused => !pred(ctrl),
                    NoCtrl::TakenBy(_, first) => first(ctrl),
                    NoCtrl::LegacySendsBareKey => pred(ctrl) && !pred(key(code, extra)),
                };
                if !holds {
                    broken.push(format!(
                        "{name}: LINUX.md lists {:?} without a Ctrl form, but {} {}",
                        row.doc,
                        spell(ctrl),
                        match row.how {
                            NoCtrl::Refused => String::from("matches"),
                            NoCtrl::TakenBy(owner, _) => format!("is not taken first by {owner}"),
                            NoCtrl::LegacySendsBareKey =>
                                String::from("is refused, or the bare key matches too"),
                        }
                    ));
                }
            }
        }
    }
    assert!(broken.is_empty(), "{broken:#?}");
    for row in &rows {
        assert!(
            listed_seen.contains(&(row.doc, row.pred)),
            "{} does not take the Cmd chord of {:?}",
            row.pred,
            row.doc
        );
    }

    // LINUX.md's table has exactly these rows, and each palette command its
    // right-hand column names exists.
    let doc = include_str!("../../../docs/LINUX.md");
    let table = doc
        .split("| Chord | Why not `Ctrl` | Without `Super` |")
        .nth(1)
        .expect("LINUX.md still has the no-Ctrl table");
    let cells: Vec<Vec<&str>> = table
        .lines()
        .skip(2)
        .take_while(|l| l.starts_with('|'))
        .map(|l| l.trim_matches('|').split(" | ").map(str::trim).collect())
        .collect();
    let documented: BTreeSet<&str> = cells.iter().map(|c| c[0]).collect();
    let expected: BTreeSet<&str> = rows.iter().map(|r| r.doc).collect();
    assert_eq!(documented, expected, "LINUX.md's no-Ctrl table");
    let titles: BTreeSet<&str> = crate::widgets::command_palette::ALL_COMMANDS
        .iter()
        .map(|c| c.title())
        .collect();
    for route in cells.iter().map(|c| c[2]) {
        for quoted in route.split('"').skip(1).step_by(2) {
            assert!(
                titles.contains(quoted),
                "LINUX.md names the palette command {quoted:?}, which does not exist"
            );
        }
    }
}

/// #843: the Keyboard Shortcuts view (`Ctrl`+`K` `Ctrl`+`S`) spells each
/// chord for Linux: `Cmd` is `Ctrl` there, as LINUX.md says, and a chord
/// without a `Ctrl` form reads `Super` rather than a `Ctrl` that does
/// something else (`Ctrl`+`E` is end of line). Hints without `Cmd` are left
/// as written.
#[cfg(not(target_os = "macos"))]
#[test]
fn the_keyboard_shortcuts_view_spells_chords_for_linux() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_keyboard_shortcuts();
    let picker = app.list_picker.as_ref().expect("the view opened");
    let shown = |id: &str| {
        let row = picker
            .rows
            .iter()
            .find(|r| r.id == format!("kb:{id}"))
            .unwrap_or_else(|| panic!("no row for {id}"));
        row.label.rsplit("  \u{00b7}  ").next().unwrap().to_string()
    };
    assert_eq!(shown("toggle_line_comment"), "Ctrl+/");
    assert_eq!(shown("join_lines"), "Ctrl+Alt+Shift+J");
    assert_eq!(shown("toggle_vim_mode"), "Super+E");
    assert_eq!(shown("toggle_terminal"), "Ctrl+J");
    assert_eq!(shown("move_line_up"), "Alt+\u{2191}");
    assert_eq!(shown("copy_line_down"), "Shift+Alt+\u{2193}");
}

/// #1562 negative: on a bottom-panel tab other than Terminal there is no
/// shell, so a chord for a terminal command is not taken for it there.
#[test]
fn a_user_chord_for_a_terminal_command_is_not_taken_on_another_panel_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{ "key": "ctrl+alt+k", "command": "terminal_clear" }]"#,
    );
    app.set_bottom_panel_tab(BottomPanelTab::Output);
    app.status.clear();
    app.handle_key(key(
        KeyCode::Char('k'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_ne!(app.status, "Cleared terminal");
}

/// #1562: a `keybindings.json` chord for a terminal command works with the
/// terminal focused, where the shell would otherwise get it; a chord for
/// any other command still goes to the shell there.
#[test]
fn a_user_chord_for_a_terminal_command_works_in_the_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[
          { "key": "ctrl+alt+k", "command": "terminal_clear" },
          { "key": "ctrl+alt+b", "command": "toggle_side_bar" }
        ]"#,
    );
    await_terminal_probe(&mut app, "bound-1562");
    app.focus_pane(Pane::Terminal);
    let side_bar = app.show_tree;
    app.handle_key(key(
        KeyCode::Char('b'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_eq!(
        app.show_tree, side_bar,
        "not a terminal command: the shell's"
    );
    app.handle_key(key(
        KeyCode::Char('k'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_eq!(app.status, "Cleared terminal");
    assert!(!app.terminal().visible_text().contains("bound-1562"));
}

/// Ctrl+Alt+J, the chord the #1436 key tests press.
fn ctrl_alt_j() -> KeyEvent {
    key(
        KeyCode::Char('j'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    )
}

/// #1436: a chord row with `"when": "editor"` fires only while the editor
/// has focus. The `when` was once ignored on key chords, so the same press
/// from the Explorer joined lines in the open file behind the user's back.
#[test]
fn an_editor_scoped_chord_does_nothing_from_the_file_tree() {
    let (mut app, _tmp) = tree_app_with_keymap(
        r#"[{"key": "ctrl+alt+j", "command": "join_lines", "when": "editor"}]"#,
    );
    let before = app.editor.lines.clone();
    app.focus_pane(Pane::Tree);
    app.handle_key(ctrl_alt_j()).unwrap();
    assert_eq!(
        app.editor.lines, before,
        "the Explorer has focus, so the editor-only row must not edit the file"
    );

    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 0;
    app.handle_key(ctrl_alt_j()).unwrap();
    assert_eq!(
        app.editor.lines[0], "let name = value; fn main() {}",
        "with the editor focused the row runs"
    );
}

/// #1436: a scoped row wins in its pane, and an unscoped row for the same
/// chord still answers everywhere else. Once the later row simply replaced
/// the earlier one, so the editor's binding ran from the Explorer too.
#[test]
fn a_scoped_chord_and_an_unscoped_one_share_a_key() {
    let (mut app, _tmp) = tree_app_with_keymap(
        r#"[{"key": "ctrl+alt+j", "command": "quick_open"},
            {"key": "ctrl+alt+j", "command": "join_lines", "when": "editor"}]"#,
    );
    let before = app.editor.lines.clone();
    app.focus_pane(Pane::Tree);
    app.handle_key(ctrl_alt_j()).unwrap();
    assert!(
        app.file_finder.is_some(),
        "the Explorer runs the unscoped row"
    );
    assert_eq!(app.editor.lines, before, "and not the editor's");

    app.file_finder = None;
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 0;
    app.handle_key(ctrl_alt_j()).unwrap();
    assert!(
        app.file_finder.is_none(),
        "the editor runs its own row instead"
    );
    assert_eq!(app.editor.lines[0], "let name = value; fn main() {}");
}

/// #1436 negative: a row with no `when` keeps firing from every pane but the
/// terminal, as before.
#[test]
fn an_unscoped_chord_still_fires_from_the_editor_and_the_tree() {
    for (pane, name) in [(Pane::Editor, "editor"), (Pane::Tree, "tree")] {
        let (mut app, _tmp) =
            tree_app_with_keymap(r#"[{"key": "ctrl+alt+j", "command": "quick_open"}]"#);
        app.focus_pane(pane);
        app.handle_key(ctrl_alt_j()).unwrap();
        assert!(app.file_finder.is_some(), "{name}");
    }
}

/// #916: Copy Line Up/Down have command ids, so a `keybindings.json` chord
/// (what `import-vscode` writes for VS Code's `copyLinesUpAction` /
/// `copyLinesDownAction`) runs them, and Shift+Alt+Up/Down still copy.
#[test]
fn copy_line_up_and_down_run_from_a_user_chord_and_their_default_one() {
    let (mut app, _tmp) = tree_app_with_keymap(
        r#"[{"key": "ctrl+alt+d", "command": "copy_line_down"},
            {"key": "ctrl+alt+u", "command": "copy_line_up"}]"#,
    );
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 0;
    app.handle_key(key(
        KeyCode::Char('d'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_eq!(
        app.editor.lines[..3],
        ["let name = value;", "let name = value;", "fn main() {}"]
    );
    assert_eq!(app.editor.cursor_row, 1, "the caret follows the copy down");

    app.editor.cursor_row = 2;
    app.handle_key(key(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_eq!(app.editor.lines[2..4], ["fn main() {}", "fn main() {}"]);
    assert_eq!(
        app.editor.cursor_row, 2,
        "the caret stays on the upper copy"
    );

    app.handle_key(key(KeyCode::Down, KeyModifiers::SHIFT | KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.editor
            .lines
            .iter()
            .filter(|l| *l == "fn main() {}")
            .count(),
        3
    );
}

/// #1436 negative: a `file_tree` row fires from the Explorer, and not from
/// another sidebar view, which has focus as the same pane but is not the
/// file tree (the rule mouse bindings already follow).
#[test]
fn a_file_tree_scoped_chord_fires_only_over_the_explorer() {
    let (mut app, _tmp) = tree_app_with_keymap(
        r#"[{"key": "ctrl+alt+j", "command": "quick_open", "when": "file_tree"}]"#,
    );
    app.focus_pane(Pane::Tree);
    app.sidebar_view = SidebarView::Search;
    app.handle_key(ctrl_alt_j()).unwrap();
    assert!(app.file_finder.is_none(), "Search is not the file tree");

    // The unbound press fell through to the built-in terminal toggle,
    // which may have moved focus.
    app.focus_pane(Pane::Tree);
    app.sidebar_view = SidebarView::Explorer;
    app.handle_key(ctrl_alt_j()).unwrap();
    assert!(app.file_finder.is_some(), "the Explorer is");

    app.file_finder = None;
    app.focus_pane(Pane::Editor);
    app.handle_key(ctrl_alt_j()).unwrap();
    assert!(app.file_finder.is_none(), "and the editor is not");
}

/// Guard (#843): a `Ctrl`+`K` that leads, then a key that completes no chord,
/// drops the leader and the key keeps its meaning: `!` is typed where the
/// caret was, and nothing is killed.
#[cfg(not(target_os = "macos"))]
#[test]
fn ctrl_k_then_a_key_that_completes_no_chord_types_it_and_kills_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.txt", "keep this");
    app.editor.cursor_col = 4;
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::CONTROL))
        .unwrap();
    app.handle_key(key(KeyCode::Char('!'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.cmd_k_leader.is_none());
    assert_eq!(app.editor.lines[0], "keep! this");
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
    assert!(app.overlays.activity.dirty());
    assert!(app.overlays.welcome.is_dirty());
    assert!(app.overlays.hero.is_dirty());
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
fn copy_path_key_is_opt_cmd_c_and_disjoint_from_copy_and_relative_path() {
    // Cmd+Opt+C (and Ctrl+Opt+C) copy the absolute path.
    assert!(is_copy_path_key(key(
        KeyCode::Char('c'),
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
    assert!(is_copy_path_key(key(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL | KeyModifiers::ALT
    )));
    // Plain Cmd+C is Copy-selection, not Copy Path.
    assert!(!is_copy_path_key(key(
        KeyCode::Char('c'),
        KeyModifiers::SUPER
    )));
    // Cmd+Opt+Shift+C is Copy Relative Path: the Shift variant must route there,
    // not collide with the absolute Copy Path.
    let shift_variant = key(
        KeyCode::Char('c'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT,
    );
    assert!(!is_copy_path_key(shift_variant));
    assert!(is_copy_relative_path_key(shift_variant));
    // ...and Copy Relative Path must NOT fire on the no-Shift Copy Path chord.
    assert!(!is_copy_relative_path_key(key(
        KeyCode::Char('c'),
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
}

#[test]
fn zoxide_jump_key_is_cmd_or_ctrl_z_and_never_redo() {
    // Cmd+Z and Ctrl+Z open the Explorer jump popup (#1294): most Linux
    // terminals never deliver Super, and this predicate runs only while the
    // Explorer is focused, where Ctrl+Z is no shell suspend or editor undo.
    // Cmd+Shift+Z is the reserved redo chord. Cmd+J stays free for terminal
    // manipulation.
    assert!(is_tree_zoxide_jump_key(key(
        KeyCode::Char('z'),
        KeyModifiers::SUPER
    )));
    assert!(
        is_tree_zoxide_jump_key(key(KeyCode::Char('Z'), KeyModifiers::SUPER)),
        "letter match must be case-insensitive"
    );
    assert!(
        is_tree_zoxide_jump_key(key(KeyCode::Char('z'), KeyModifiers::CONTROL)),
        "Ctrl+Z is the jump on terminals that never send Super"
    );
    for (mods, why) in [
        (
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
            "Cmd+Shift+Z is redo",
        ),
        (
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            "Ctrl+Shift+Z is redo",
        ),
        (KeyModifiers::ALT, "Alt+Z is not the jump"),
        (
            KeyModifiers::CONTROL | KeyModifiers::ALT,
            "Ctrl+Alt+Z is not the jump",
        ),
        (KeyModifiers::NONE, "a bare z is type-to-find"),
    ] {
        assert!(
            !is_tree_zoxide_jump_key(key(KeyCode::Char('z'), mods)),
            "{why}"
        );
    }
    assert!(
        !is_tree_zoxide_jump_key(key(KeyCode::Char('j'), KeyModifiers::SUPER)),
        "Cmd+J must stay free for terminal manipulation"
    );
}

#[test]
fn a_replayed_key_cannot_start_a_recording() {
    // The re-entrancy flag gates CAPTURE, not dispatch, so a replayed `q`
    // used to reach the vim handler and leave the user silently recording
    // after the replay returned — every later keystroke captured with no
    // gesture having asked for it.
    let (mut app, tmp) = vim_app("one\ntwo\n");
    app.macros_path = tmp.path().join("macros.json");
    // A register whose contents are themselves a recording gesture.
    let mut hostile = crate::macros::Macro::default();
    hostile.push_key(key(KeyCode::Char('q'), KeyModifiers::NONE));
    hostile.push_key(key(KeyCode::Char('c'), KeyModifiers::NONE));
    app.macro_registers.insert("b".into(), hostile);

    vim_feed_str(&mut app, "@b");
    assert!(
        app.macro_recording.is_none(),
        "replaying a recording gesture must not leave the editor recording"
    );
}

#[test]
fn saving_one_register_preserves_every_field_of_a_newer_crofts_steps() {
    // `update_json_store` re-serialises the ENTIRE map on every save, so a
    // fieldless catch-all would rewrite a newer croft's steps as
    // `{"kind":"Unknown"}` the first time any unrelated register was
    // stored — silently destroying them. Worse than refusing to save,
    // because it is unrecoverable.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("macros.json");
    std::fs::write(
        &path,
        r#"{"b":{"steps":[{"kind":"Mouse","button":"left","x":3}]}}"#,
    )
    .unwrap();

    let mut mine = crate::macros::Macro::default();
    mine.push_key(key(KeyCode::Char('z'), KeyModifiers::NONE));
    crate::macros::save_register(&path, "c", mine).unwrap();

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("\"button\"") && raw.contains("\"left\""),
        "the future step's own fields survive an unrelated save: {raw}"
    );
    assert!(
        raw.contains("\"x\""),
        "including every field, not just the tag: {raw}"
    );
}

#[test]
fn a_replayed_keybinding_cannot_open_the_count_prompt() {
    // `macro_replay_times` is a bindable id, so a replayed chord reaches it
    // through keymap -> run_command. A prompt opening mid-replay swallows
    // the macro's remaining keys into the count field, and `input_prompt`
    // is checked ABOVE the macro tap, so a later Enter would submit the
    // count and re-enter replay.
    let (mut app, tmp) = vim_app("one\n");
    app.macros_path = tmp.path().join("macros.json");
    let mut m = crate::macros::Macro::default();
    m.push_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
    app.macro_last = Some(m);

    app.macro_replaying = true;
    app.run_command(crate::widgets::command_palette::Command::MacroReplayTimes);
    assert!(
        app.input_prompt.is_none(),
        "the count prompt must not open while a macro is replaying"
    );

    app.macro_replaying = false;
    app.run_command(crate::widgets::command_palette::Command::MacroReplayTimes);
    assert!(
        app.input_prompt.is_some(),
        "but it opens normally when nothing is replaying"
    );
}

#[test]
fn a_macro_of_only_unknown_steps_reports_that_it_ran_nothing() {
    // `is_empty()` tests the step list; a macro whose steps are all
    // unreplayable is not empty but yields no keys, and reporting
    // "Macro replayed" would tell the user a newer croft's macro ran.
    let (mut app, tmp) = vim_app("one\n");
    app.macros_path = tmp.path().join("macros.json");
    let path = app.macros_path.clone();
    std::fs::write(
        &path,
        r#"{"u":{"steps":[{"kind":"Mouse","button":"left"}]}}"#,
    )
    .unwrap();
    app.macro_registers = crate::macros::load(&path);

    vim_feed_str(&mut app, "@u");
    assert!(
        app.status.contains("no steps this version can replay"),
        "an all-unknown macro says so, got {:?}",
        app.status
    );
    assert_eq!(app.editor.lines[0], "one", "and nothing ran");
}

#[test]
fn an_unknown_step_kind_does_not_brick_the_whole_store() {
    // A macros.json written by a newer croft must lose the step it names,
    // not every register in the file — serde's tagged enums fail the WHOLE
    // parse on an unknown tag without a catch-all, which would also block
    // every future save (the store refuses to overwrite what it cannot read).
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("macros.json");
    std::fs::write(
        &path,
        r#"{"a":{"steps":[{"kind":"Text","text":"hi"}]},
            "b":{"steps":[{"kind":"Mouse","button":"left"}]}}"#,
    )
    .unwrap();

    let loaded = crate::macros::load(&path);
    assert!(
        loaded.contains_key("a"),
        "the readable register survives: {loaded:?}"
    );
    assert_eq!(
        loaded.get("a").map(|m| m.len()),
        Some(2),
        "and still replays its own keys"
    );
    // The unknown step is inert rather than fatal.
    assert_eq!(
        loaded.get("b").map(|m| m.key_events().len()),
        Some(0),
        "an unknown step kind replays as nothing"
    );
    // Saves still work, so the store is not permanently locked.
    assert!(
        crate::macros::save_register(&path, "c", loaded.get("a").cloned().unwrap()).is_ok(),
        "an unknown kind elsewhere in the file must not block writes"
    );
}

#[test]
fn a_replay_aborts_visibly_and_keeps_earlier_iterations() {
    // The issue requires an abort to leave prior iterations intact. Budget
    // refusal is the reachable abort in a test: assert it refuses without
    // running anything rather than spinning.
    let (mut app, tmp) = vim_app("one\n");
    app.macros_path = tmp.path().join("macros.json");
    let mut big = crate::macros::Macro::default();
    for _ in 0..10 {
        big.push_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
    }
    app.macro_registers.insert("z".into(), big);
    let before = app.editor.lines[0].clone();

    vim_feed_str(&mut app, "99999@z");
    assert!(
        app.status.contains("refused"),
        "a replay past the budget is refused visibly, got {:?}",
        app.status
    );
    assert_eq!(
        app.editor.lines[0], before,
        "and nothing ran, rather than half of it"
    );
}

/// #1464: a paste is one macro step but inserts all of its text, so its
/// characters count against the replay budget: a 10 KB paste times 100 is
/// refused, not a megabyte typed into the buffer.
#[test]
fn a_macro_paste_counts_its_characters_against_the_replay_budget() {
    let (mut app, tmp) = vim_app("one\n");
    app.macros_path = tmp.path().join("macros.json");
    let mut big = crate::macros::Macro::default();
    big.push_paste(&"x".repeat(10_000));
    app.macro_registers.insert("z".into(), big);
    let before = app.editor.lines[0].clone();
    vim_feed_str(&mut app, "100@z");
    assert!(
        app.status.contains("refused"),
        "a replay past the budget is refused visibly, got {:?}",
        app.status
    );
    assert_eq!(app.editor.lines[0], before, "and nothing ran");
}

#[test]
fn a_non_register_key_after_q_aborts_the_gesture() {
    // The per-character handlers only see plain chars, so an arrow key after
    // `q` used to return at the motion arms with `pending_record` still set —
    // and the next letter typed would silently start a recording nobody
    // asked for. Vim aborts the gesture on a non-register key.
    let (mut app, tmp) = vim_app("one\ntwo\nthree\n");
    app.macros_path = tmp.path().join("macros.json");
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;

    vim_feed(&mut app, 'q');
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.macro_recording.is_none(),
        "the arrow key aborts the pending register gesture"
    );

    // The following letter must be an ordinary motion, not a register name.
    vim_feed(&mut app, 'j');
    assert!(
        app.macro_recording.is_none(),
        "and does not retroactively start a recording"
    );
}

#[test]
fn a_replayed_key_is_never_recorded_into_the_macro_producing_it() {
    // Replay feeds keys back through handle_key, so without the re-entrancy
    // guard a macro would grow while replaying and never terminate.
    let (mut app, tmp) = vim_app("one\ntwo\n");
    app.macros_path = tmp.path().join("macros.json");
    vim_feed_str(&mut app, "qa");
    vim_feed_str(&mut app, "IX");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    vim_feed(&mut app, 'q');
    let recorded_len = app.macro_registers.get("a").unwrap().len();

    vim_feed_str(&mut app, "@a");
    assert_eq!(
        app.macro_registers.get("a").unwrap().len(),
        recorded_len,
        "replaying must not append to the register it is replaying"
    );
}

/// Record the issue's macro on line one of `lines`: End, `: `, a terminal
/// paste of `paste`, ` ok`, Down. Then replay it once on the next line.
fn record_and_replay_a_macro_with_a_paste(lines: &[&str], paste: &str) -> App {
    use crate::widgets::command_palette::Command;
    let mut app = editor_app_with_lines(lines);
    app.run_command(Command::MacroStartStopRecording);
    app.handle_key(key(KeyCode::End, KeyModifiers::NONE))
        .unwrap();
    type_str(&mut app, ": ");
    app.handle_paste(paste);
    type_str(&mut app, " ok");
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    app.run_command(Command::MacroStartStopRecording);
    app.run_command(Command::MacroReplayLast);
    app
}

/// #1464: a terminal paste made while recording is part of the macro, and
/// replay pastes the same text at the new caret.
#[test]
fn a_paste_while_recording_a_macro_replays_at_the_new_caret() {
    let app = record_and_replay_a_macro_with_a_paste(&["alpha", "beta", "gamma"], "PASTED");
    assert_eq!(app.editor.lines[0], "alpha: PASTED ok");
    assert_eq!(
        app.editor.lines[1], "beta: PASTED ok",
        "the replay pastes too"
    );
    assert_eq!(app.editor.lines[2], "gamma");
}

/// #1464: a multi-line paste replays as pasted. Typed back key by key, its
/// newlines would pick up the editor's auto-indent line by line.
#[test]
fn a_multi_line_paste_in_a_macro_replays_with_its_indentation() {
    let paste = "{\n  x: 1,\n}";
    let app = record_and_replay_a_macro_with_a_paste(&["  a", "  b", "  c"], paste);
    let first: Vec<&str> = app.editor.lines[0..3].iter().map(String::as_str).collect();
    assert_eq!(
        first,
        ["  a: {", "  x: 1,", "} ok"],
        "the recording pasted as is"
    );
    let replayed: Vec<&str> = app.editor.lines[3..6].iter().map(String::as_str).collect();
    assert_eq!(
        replayed,
        ["  b: {", "  x: 1,", "} ok"],
        "and so does the replay"
    );
}

/// #1464: the paste is one step of its own, between the typing around it.
#[test]
fn a_paste_is_recorded_as_one_step_of_its_own() {
    use crate::macros::Step;
    use crate::widgets::command_palette::Command;
    let mut app = editor_app_with_lines(&["alpha"]);
    app.run_command(Command::MacroStartStopRecording);
    type_str(&mut app, "ab");
    app.handle_paste("P\nQ");
    type_str(&mut app, "c");
    app.run_command(Command::MacroStartStopRecording);
    let steps = app.macro_last.as_ref().expect("recorded").steps.clone();
    assert_eq!(
        steps,
        vec![
            Step::Text { text: "ab".into() },
            Step::Paste {
                text: "P\nQ".into()
            },
            Step::Text { text: "c".into() },
        ]
    );
}

/// #1464 negative: a paste into another pane while recording is not part
/// of the macro, which captures the editor only, and the Macros channel
/// says it was skipped, as it does for keys: by size, never its text.
#[test]
fn a_paste_into_another_pane_while_recording_is_skipped_and_logged() {
    use crate::widgets::command_palette::Command;
    let mut app = editor_app_with_lines(&["alpha"]);
    app.run_command(Command::MacroStartStopRecording);
    type_str(&mut app, "x");
    app.focus_pane(Pane::Tree);
    app.handle_paste("SKIPPED-PASTE-1464");
    app.focus_pane(Pane::Editor);
    app.run_command(Command::MacroStartStopRecording);
    let steps = app.macro_last.as_ref().expect("recorded").steps.clone();
    assert_eq!(steps, vec![crate::macros::Step::Text { text: "x".into() }]);
    let logged = crate::output::snapshot("Macros").unwrap_or_default();
    assert!(
        logged
            .iter()
            .any(|l| l.text.contains("skipped a paste of 18 chars")),
        "{logged:?}"
    );
    assert!(
        !logged.iter().any(|l| l.text.contains("SKIPPED-PASTE-1464")),
        "the pasted text itself stays out of the log"
    );
}

/// #1464 negative: a paste the editor's find bar takes while recording never
/// reached the document, so the macro does not record it and its replay
/// does not insert it into the text.
#[test]
fn a_paste_into_the_find_bar_is_not_recorded_into_the_macro() {
    use crate::widgets::command_palette::Command;
    let mut app = editor_app_with_lines(&["a", "b"]);
    app.run_command(Command::MacroStartStopRecording);
    app.open_editor_find();
    assert!(app.focus == Pane::Editor);
    app.handle_paste("QUERY");
    app.editor_find = None;
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    app.run_command(Command::MacroStartStopRecording);
    app.run_command(Command::MacroReplayLast);
    assert!(
        app.editor.lines.iter().all(|l| !l.contains("QUERY")),
        "{:?}",
        app.editor.lines
    );
}

/// #1464 negative: a replayed paste is not recorded again into the macro
/// replaying it, and pasting with no recording records nothing.
#[test]
fn a_replayed_paste_is_never_recorded_again() {
    let mut app = record_and_replay_a_macro_with_a_paste(&["a", "b"], "P");
    let len = app.macro_last.as_ref().unwrap().steps.len();
    app.handle_paste("Z");
    assert_eq!(app.macro_last.as_ref().unwrap().steps.len(), len);
    assert!(app.macro_recording.is_none());
}

#[test]
fn an_empty_recording_is_discarded_rather_than_clobbering_a_good_macro() {
    let (mut app, tmp) = vim_app("x\n");
    app.macros_path = tmp.path().join("macros.json");
    vim_feed_str(&mut app, "qa");
    vim_feed_str(&mut app, "IZ");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    vim_feed(&mut app, 'q');
    let good = app.macro_registers.get("a").cloned().unwrap();

    // Start and immediately stop: nothing recorded.
    vim_feed_str(&mut app, "qa");
    vim_feed(&mut app, 'q');
    assert_eq!(
        app.macro_registers.get("a"),
        Some(&good),
        "an empty recording must not replace a useful register"
    );
}

#[test]
fn a_macro_recorded_in_vim_mode_replays_from_the_palette() {
    // The registers and the palette's "last macro" are one store, so the two
    // entry points are interchangeable — the issue calls this out explicitly.
    let (mut app, tmp) = vim_app("one\ntwo\n");
    app.macros_path = tmp.path().join("macros.json");
    vim_feed_str(&mut app, "qa");
    vim_feed_str(&mut app, "IX");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    vim_feed(&mut app, 'j');
    vim_feed(&mut app, 'q');

    app.run_command(crate::widgets::command_palette::Command::MacroReplayLast);
    assert_eq!(
        app.editor.lines[1], "Xtwo",
        "the palette replays what vim recorded"
    );
}

#[test]
fn a_register_survives_a_restart_through_macros_json() {
    let (mut app, tmp) = vim_app("one\n");
    let path = tmp.path().join("macros.json");
    app.macros_path = path.clone();
    vim_feed_str(&mut app, "qa");
    vim_feed_str(&mut app, "IX");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    vim_feed(&mut app, 'q');

    // A fresh load of the store sees the register, which is what App::new does.
    let reloaded = crate::macros::load(&path);
    assert_eq!(
        reloaded.get("a"),
        app.macro_registers.get("a"),
        "the register was persisted, not just held in memory"
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
fn format_document_key_is_cmd_opt_shift_f() {
    // VS Code "Format Document" is Shift+Alt+F; croft forwards it as the
    // Cmd+Opt+Shift+F chord (modifier byte 12) so it reaches the editor over
    // iTerm2's GlobalKeyMap and Ghostty's csi: keybind alike.
    assert!(is_format_document_key(key(
        KeyCode::Char('F'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT
    )));
    // Linux / Termux fold the command modifier onto Ctrl.
    assert!(is_format_document_key(key(
        KeyCode::Char('f'),
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT
    )));
    // Any missing modifier, or a different letter, is not the chord.
    assert!(!is_format_document_key(key(
        KeyCode::Char('F'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
    assert!(!is_format_document_key(key(
        KeyCode::Char('g'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT
    )));
}

#[test]
fn go_to_definition_key_is_f12_without_shift() {
    assert!(is_go_to_definition_key(key(
        KeyCode::F(12),
        KeyModifiers::NONE
    )));
    // Alt+F12 is Peek Definition now (#115): the fold-noise tolerance the
    // plain jump used to extend to a stray ALT bit moved onto the peek,
    // which is the strictly less disruptive consumer.
    assert!(!is_go_to_definition_key(key(
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
fn cmd_k_o_chords_route_to_the_new_window_actions() {
    let tmp = tempfile::tempdir().unwrap();
    // Blank/unsaved buffer: the chord is still handled, but the action refuses
    // (no path), so no real window is spawned during the test.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Cmd+K O -> Copy into New Window.
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('o'), KeyModifiers::NONE)));
    assert!(
        app.status.contains("save the file first"),
        "Cmd+K O routes to Copy into New Window: {}",
        app.status
    );

    // Cmd+K Shift+O -> Move into New Window.
    app.status.clear();
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('o'), KeyModifiers::SHIFT)));
    assert!(
        app.status.contains("save the file first"),
        "Cmd+K Shift+O routes to Move into New Window: {}",
        app.status
    );
}

#[test]
fn cmd_k_backslash_chord_splits_the_editor_up() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hi");
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.cmd_k_leader.is_some(), "Cmd+K arms the chord leader");
    app.handle_key(key(KeyCode::Char('\\'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.editor_layout.root_split_dir(),
        Some(editor_layout::SplitDir::Vertical),
        "Cmd+K Cmd+\\ splits the editor vertically"
    );
    assert_eq!(
        app.editor_layout.active_dfs_index(),
        0,
        "Cmd+K Cmd+\\ = Split Up (new group above); plain Cmd+\\ stays Split Right"
    );
}

#[test]
fn cmd_k_shift_backslash_chord_splits_in_group() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hi");
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(
        KeyCode::Char('\\'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.editor_layout.leaf_count(),
        2,
        "Cmd+K Shift+Cmd+\\ (Split in Group) opens a second view beside the file"
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
fn cmd_active_rejects_bare_modifiers() {
    assert!(!cmd_active(KeyModifiers::NONE, true));
    assert!(!cmd_active(KeyModifiers::ALT, true));
    assert!(!cmd_active(KeyModifiers::SHIFT, true));
}

/// #360, byte level: the terminal session store is written to disk and
/// replayed next launch, so a key that was on screen must not be in it.
#[test]
fn the_session_store_holds_the_mask_not_the_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminal_session_path = tmp.path().join("terminal-sessions.json");
    let term = crate::widgets::terminal::PtyTerminal::new_with_transcript(
        tmp.path(),
        &[String::from("key AKIAIOSFODNN7EXAMPLE end")],
    )
    .unwrap();
    // The transcript is painted into the grid during the spawn, before
    // the child can write, so the save sees it at once.
    app.insert_terminal(term);
    app.save_terminal_session();
    let store = std::fs::read_to_string(&app.terminal_session_path).unwrap();
    assert!(
        !store.contains("AKIAIOSFODNN7EXAMPLE"),
        "the key leaked to disk: {store}"
    );
    assert!(
        store.contains("key \u{2022}"),
        "the mask was stored in its place: {store}"
    );
}

// ---- New VS Code feature keybinding predicates ----

#[test]
fn command_palette_key_is_cmd_or_ctrl_shift_p() {
    assert!(is_command_palette_key(key(
        KeyCode::Char('p'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
    assert!(is_command_palette_key(key(
        KeyCode::Char('p'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
    // Cmd+P (no Shift) is Quick Open, not the palette.
    assert!(!is_command_palette_key(key(
        KeyCode::Char('p'),
        KeyModifiers::SUPER
    )));
    // Alt disqualifies it.
    assert!(!is_command_palette_key(key(
        KeyCode::Char('p'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT | KeyModifiers::ALT
    )));
}

#[test]
fn go_to_symbol_key_is_cmd_or_ctrl_shift_o() {
    assert!(is_go_to_symbol_key(key(
        KeyCode::Char('o'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
    assert!(is_go_to_symbol_key(key(
        KeyCode::Char('o'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
    // Cmd+O (no Shift) is open-line-below, not Go to Symbol.
    assert!(!is_go_to_symbol_key(key(
        KeyCode::Char('o'),
        KeyModifiers::SUPER
    )));
}

#[test]
fn toggle_line_comment_key_is_cmd_or_ctrl_slash() {
    assert!(is_toggle_line_comment_key(key(
        KeyCode::Char('/'),
        KeyModifiers::SUPER
    )));
    assert!(is_toggle_line_comment_key(key(
        KeyCode::Char('/'),
        KeyModifiers::CONTROL
    )));
    assert!(!is_toggle_line_comment_key(key(
        KeyCode::Char('/'),
        KeyModifiers::NONE
    )));
    assert!(!is_toggle_line_comment_key(key(
        KeyCode::Char('/'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
}

#[test]
fn toggle_block_comment_key_is_shift_alt_a() {
    assert!(is_toggle_block_comment_key(key(
        KeyCode::Char('a'),
        KeyModifiers::SHIFT | KeyModifiers::ALT
    )));
    assert!(is_toggle_block_comment_key(key(
        KeyCode::Char('A'),
        KeyModifiers::SHIFT | KeyModifiers::ALT
    )));
    // Alt alone (no Shift) is not the chord.
    assert!(!is_toggle_block_comment_key(key(
        KeyCode::Char('a'),
        KeyModifiers::ALT
    )));
    // The command modifier disqualifies it.
    assert!(!is_toggle_block_comment_key(key(
        KeyCode::Char('a'),
        KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::SUPER
    )));
}

#[test]
fn toggle_wrap_key_is_alt_z() {
    assert!(is_toggle_wrap_key(key(
        KeyCode::Char('z'),
        KeyModifiers::ALT
    )));
    assert!(!is_toggle_wrap_key(key(
        KeyCode::Char('z'),
        KeyModifiers::ALT | KeyModifiers::SHIFT
    )));
    assert!(!is_toggle_wrap_key(key(
        KeyCode::Char('z'),
        KeyModifiers::NONE
    )));
}

#[test]
fn switch_header_source_key_is_alt_o() {
    assert!(is_switch_header_source_key(key(
        KeyCode::Char('o'),
        KeyModifiers::ALT
    )));
    assert!(!is_switch_header_source_key(key(
        KeyCode::Char('o'),
        KeyModifiers::ALT | KeyModifiers::SHIFT
    )));
    assert!(!is_switch_header_source_key(key(
        KeyCode::Char('o'),
        KeyModifiers::NONE
    )));
}

// --- Bookmarks ------------------------------------------------------------

#[test]
fn bookmark_chords_are_disjoint_from_the_chords_they_sit_beside() {
    // Toggle is Cmd+Opt+Shift+K; plain Cmd+Shift+K is still Delete Line.
    assert!(is_toggle_bookmark_key(key(
        KeyCode::Char('k'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT
    )));
    assert!(!is_delete_line_key(key(
        KeyCode::Char('k'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT
    )));
    assert!(!is_toggle_bookmark_key(key(
        KeyCode::Char('k'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));

    // Next/previous are Cmd+Opt+. and Cmd+Opt+, — Quick Fix (Cmd+.) excludes
    // Alt, so the two dot chords never fire together.
    assert!(is_next_bookmark_key(key(
        KeyCode::Char('.'),
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
    assert!(!is_quick_fix_key(key(
        KeyCode::Char('.'),
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
    assert!(!is_next_bookmark_key(key(
        KeyCode::Char('.'),
        KeyModifiers::SUPER
    )));
    assert!(is_prev_bookmark_key(key(
        KeyCode::Char(','),
        KeyModifiers::CONTROL | KeyModifiers::ALT
    )));
    assert!(!is_prev_bookmark_key(key(
        KeyCode::Char(','),
        KeyModifiers::ALT
    )));
}

// --- Emmet: Expand Abbreviation -------------------------------------------

#[test]
fn cmd_opt_shift_e_is_the_emmet_expand_chord() {
    assert!(is_emmet_expand_key(key(
        KeyCode::Char('e'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT
    )));
    // Termux / Linux forward SUPER as CONTROL.
    assert!(is_emmet_expand_key(key(
        KeyCode::Char('e'),
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT
    )));
    // Each modifier is load-bearing: dropping any one must not expand.
    assert!(!is_emmet_expand_key(key(
        KeyCode::Char('e'),
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
    assert!(!is_emmet_expand_key(key(
        KeyCode::Char('e'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
    assert!(!is_emmet_expand_key(key(
        KeyCode::Char('e'),
        KeyModifiers::NONE
    )));
    // And it is the E chord specifically, not any Cmd+Opt+Shift letter.
    assert!(!is_emmet_expand_key(key(
        KeyCode::Char('w'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT
    )));
}

#[test]
fn the_toggle_chord_marks_the_cursor_line_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.rs", "a\nb\nc\nd\n");
    app.editor.cursor_row = 2;
    app.handle_editor_key(key(
        KeyCode::Char('k'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(app.editor.bookmarked_lines(), vec![3]);
    assert!(
        app.status.contains("line 3"),
        "the status must name the line; was {:?}",
        app.status
    );
}

#[test]
fn bookmark_clear_chord_is_cmd_opt_shift_b_and_clears_the_file() {
    assert!(is_clear_bookmarks_key(key(
        KeyCode::Char('b'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT,
    )));
    assert!(is_clear_bookmarks_key(key(
        KeyCode::Char('B'),
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
    )));
    assert!(!is_clear_bookmarks_key(key(
        KeyCode::Char('b'),
        KeyModifiers::SUPER | KeyModifiers::ALT,
    )));
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.rs", "1\n2\n3\n");
    for row in [0, 2] {
        app.editor.cursor_row = row;
        app.editor.toggle_bookmark();
    }
    app.handle_editor_key(key(
        KeyCode::Char('b'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert!(app.editor.bookmarked_lines().is_empty());
    assert_eq!(app.status, "Cleared 2 bookmarks");
}

#[test]
fn a_user_keybinding_overrides_and_runs_the_bound_command() {
    // A chord in keybindings.json is consulted ahead of the built-in handlers
    // and dispatches the palette command it names.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap =
        crate::keymap::Keymap::from_json(r#"[{ "key": "ctrl+alt+g", "command": "show_search" }]"#);
    assert_ne!(app.sidebar_view, SidebarView::Search);
    app.handle_key(key(
        KeyCode::Char('g'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_eq!(
        app.sidebar_view,
        SidebarView::Search,
        "the bound chord must run Show Search via run_command"
    );
}

#[test]
fn a_user_keybinding_is_ignored_while_the_terminal_is_focused() {
    // The terminal needs its raw control keys, so the keymap must not fire there.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap =
        crate::keymap::Keymap::from_json(r#"[{ "key": "ctrl+alt+g", "command": "show_search" }]"#);
    app.focus = Pane::Terminal;
    app.handle_key(key(
        KeyCode::Char('g'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_ne!(
        app.sidebar_view,
        SidebarView::Search,
        "a bound chord must be inert while the terminal is focused"
    );
}

/// The `file` key in a snapshot request comes straight off the wire: a
/// traversing or absolute key must never resolve outside the workspace, or
/// any guest (the MCP collab agent included) could make the owner read,
/// broadcast, and even edit arbitrary files it can reach.
#[test]
fn a_traversing_wire_file_key_is_declined() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(tmp.path().join("secret.txt"), "SECRET").unwrap();
    std::fs::write(root.join("inner.txt"), "inner").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    assert_eq!(
        app.owner_buffer_text(&root, "../secret.txt"),
        None,
        "a traversing wire key must not escape the workspace root"
    );
    assert_eq!(
        app.owner_buffer_text(&root, "sub/../../secret.txt"),
        None,
        "a mid-path traversal must not escape the workspace root"
    );
    assert_eq!(
        app.owner_buffer_text(&root, "/etc/hosts"),
        None,
        "an absolute wire key must not leave the workspace root"
    );
    assert_eq!(
        app.owner_buffer_text(&root, "inner.txt").as_deref(),
        Some("inner"),
        "a plain relative key still resolves"
    );
}

#[test]
fn f12_family_chords_route_alt_to_peek() {
    let plain = crossterm::event::KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE);
    let alt = crossterm::event::KeyEvent::new(KeyCode::F(12), KeyModifiers::ALT);
    assert!(is_go_to_definition_key(plain) && !is_peek_definition_key(plain));
    assert!(is_peek_definition_key(alt) && !is_go_to_definition_key(alt));
    let shift_alt =
        crossterm::event::KeyEvent::new(KeyCode::F(12), KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert!(
        !is_peek_definition_key(shift_alt),
        "peek requires ALT alone in the F12 family"
    );
}

#[test]
fn a_rebound_chord_runs_go_to_definition_and_rename_symbol() {
    // #1212: keybindings.json rows naming the caret-driven LSP actions were
    // rejected as unknown ids, so the only way in was the F-key.
    let (mut app, _tmp) = lsp_nav_app(
        r#"[
            { "key": "ctrl+alt+d", "command": "go_to_definition" },
            { "key": "ctrl+alt+r", "command": "rename_symbol" },
            { "key": "ctrl+alt+u", "command": "go_to_references" },
            { "key": "ctrl+alt+t", "command": "go_to_type_definition" },
            { "key": "ctrl+alt+i", "command": "go_to_implementations" },
            { "key": "ctrl+alt+l", "command": "go_to_declaration" }
        ]"#,
    );
    let chord = |c| key(KeyCode::Char(c), KeyModifiers::CONTROL | KeyModifiers::ALT);

    app.handle_key(chord('d')).unwrap();
    assert!(
        app.definition_request_id.is_some(),
        "the bound chord sends the definition request at the caret"
    );
    app.handle_key(chord('u')).unwrap();
    assert!(app.references_request_id.is_some(), "references");
    app.handle_key(chord('t')).unwrap();
    assert!(app.type_definition_request_id.is_some(), "type definition");
    app.handle_key(chord('i')).unwrap();
    assert!(app.implementation_request_id.is_some(), "implementations");
    app.handle_key(chord('l')).unwrap();
    assert!(app.declaration_request_id.is_some(), "declaration");
    app.handle_key(chord('r')).unwrap();
    assert!(
        app.prepare_rename_request.is_some(),
        "the bound chord starts a rename at the caret"
    );
    assert_eq!(app.status, "Preparing rename…");
}

#[test]
fn the_bound_go_to_definition_follows_a_document_link_like_the_builtin() {
    use crate::lsp::manager::DocumentLinkItem;
    // A bound gesture is meant to be able to REPLACE the built-in it mirrors,
    // so it must keep the built-in's #254 precedence: a server-resolved
    // document link outranks Go to Definition at that spot, because a URL in
    // a comment has no definition at all. `trigger_definition_at` checks the
    // link first; the bound command jumped straight to the LSP request, so
    // rebinding ctrl+click silently regressed link-following.
    //
    // Observable is a file:// link, which opens the target in the editor --
    // fully inert. A web link would reach `open_url` and launch a browser.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "// link here\n").unwrap();
    std::fs::write(tmp.path().join("b.rs"), "fn target() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("a.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let target = lsp_types::Url::from_file_path(tmp.path().join("b.rs")).unwrap();
    app.editor.apply_document_links(
        tmp.path().join("a.rs"),
        vec![DocumentLinkItem {
            line: 0,
            character: 3,
            end_line: 0,
            end_character: 12,
            target: target.to_string(),
        }],
    );

    let area = app.editor.last_inner;
    assert!(area.width > 6 && area.height > 1, "editor must be laid out");
    // Text starts past the line-number gutter: `buffer_pos_at` computes
    // `last_inner.x + last_gutter_width + 1`, so a column measured from
    // `area.x` alone lands in the gutter and resolves to the wrong char.
    let text_x = area.x + app.editor.last_gutter_width + 1;
    let (col, row) = (text_x + 5, area.y);
    assert!(
        app.editor.buffer_pos_at(col, row).is_some(),
        "the click must resolve to a buffer position, or nothing downstream runs"
    );
    let (bl, bc) = app.editor.buffer_pos_at(col, row).unwrap();
    assert_eq!(
        app.editor.document_link_at(bl, bc),
        Some(target.as_str()),
        "the clicked cell must sit INSIDE the link range, or this test cannot \
         tell link-following from its absence"
    );

    app.last_mouse_pos = Some((col, row));
    app.run_command(crate::widgets::command_palette::Command::MouseGoToDefinitionAtClick);

    assert_eq!(
        app.editor.path.as_deref().and_then(|p| p.file_name()),
        Some(std::ffi::OsStr::new("b.rs")),
        "the bound Go to Definition must follow the document link first, as \
         `trigger_definition_at` does -- a bound gesture that drops the \
         built-in's link precedence is not a replacement for it"
    );
}

#[test]
fn the_bound_open_link_works_in_the_editor_not_only_the_terminal() {
    use crate::lsp::manager::DocumentLinkItem;
    // `editor` is the DEFAULT `when` region per docs/KEYBINDINGS.md, so the
    // most natural binding a user writes for this command lands there. It
    // consulted only the terminal grid (`terminal_url_click` ->
    // `terminal_file_click`), both of which hit-test `last_inner` of the
    // TERMINAL, so an editor click could never resolve: it reported
    // "No link there" over a link that was there.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "// link here\n").unwrap();
    std::fs::write(tmp.path().join("b.rs"), "fn target() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("a.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let target = lsp_types::Url::from_file_path(tmp.path().join("b.rs")).unwrap();
    app.editor.apply_document_links(
        tmp.path().join("a.rs"),
        vec![DocumentLinkItem {
            line: 0,
            character: 3,
            end_line: 0,
            end_character: 12,
            target: target.to_string(),
        }],
    );

    let area = app.editor.last_inner;
    assert!(area.width > 6 && area.height > 1, "editor must be laid out");
    // Text starts past the line-number gutter: `buffer_pos_at` computes
    // `last_inner.x + last_gutter_width + 1`, so a column measured from
    // `area.x` alone lands in the gutter and resolves to the wrong char.
    let text_x = area.x + app.editor.last_gutter_width + 1;
    let (col, row) = (text_x + 5, area.y);
    let (bl, bc) = app
        .editor
        .buffer_pos_at(col, row)
        .expect("the click must resolve to a buffer position");
    assert_eq!(
        app.editor.document_link_at(bl, bc),
        Some(target.as_str()),
        "the clicked cell must sit inside the link range, or a failure here \
         would mean 'no link' rather than 'the command ignored the editor'"
    );

    app.last_mouse_pos = Some((col, row));
    app.run_command(crate::widgets::command_palette::Command::MouseOpenLinkAtClick);

    assert_eq!(
        app.editor.path.as_deref().and_then(|p| p.file_name()),
        Some(std::ffi::OsStr::new("b.rs")),
        "Open Link at Click must follow an editor document link. It consulted \
         only the terminal, so in the DEFAULT `editor` region it was a \
         permanent no-op that reported {:?}",
        app.status
    );
}

/// #369: every way in opens the same symbol tab.
///
/// `Cmd+K Shift+V` takes the symbol at the caret, and an OUTLINE row's right-click
/// menu opens the row's symbol even when the caret sits elsewhere.
#[test]
fn the_chord_and_the_outline_menu_open_a_symbol_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("two.rs");
    std::fs::write(&file, "fn a() {\n    1\n}\nfn b() {\n    2\n}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.editor.cursor_row = 1;
    // Plain Cmd+K V is Live Run's.
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('v'), KeyModifiers::NONE)));
    assert!(
        app.editor.symbol_view.is_none(),
        "Cmd+K V is not the symbol tab"
    );
    // Shift arrives lowercase with SHIFT under CSI-u.
    let mut csi_u = App::new(tmp.path().to_path_buf()).unwrap();
    csi_u.editor.open_pinned(&file).unwrap();
    csi_u.editor.cursor_row = 1;
    assert!(csi_u.handle_cmd_k_chord(key(KeyCode::Char('v'), KeyModifiers::SHIFT)));
    assert!(csi_u.editor.symbol_view.is_some(), "Shift+v opens it too");
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('V'), KeyModifiers::SHIFT)));
    let view = app
        .editor
        .symbol_view
        .as_ref()
        .expect("Cmd+K Shift+V opened a symbol tab");
    assert_eq!((view.name.as_str(), view.first, view.last), ("a", 0, 2));

    app.editor.select(0);
    app.dispatch_menu_action(
        MenuAction::OpenSymbolTab {
            path: file.clone(),
            name: "b".into(),
            first: 3,
            last: 5,
        },
        tmp.path().to_path_buf(),
    );
    let view = app
        .editor
        .symbol_view
        .as_ref()
        .expect("the menu opened a symbol tab");
    assert_eq!((view.name.as_str(), view.first, view.last), ("b", 3, 5));
    assert_eq!(app.editor.editors.len(), 3);
}

#[test]
fn the_expand_entry_advertises_no_chord_it_cannot_perform() {
    // `Cmd+K [` acts on `active_terminal`, which the focus invariant keeps off
    // folded panes, so the chord can only ever COLLAPSE. The menu label
    // alternates, so a folded pane's menu read `Expand Terminal   ⌘K [` -
    // advertising a shortcut that does the opposite of the label beside it.
    // Contrast `ToggleMaximizeTerminal`, which carries `⌘K M` honestly because
    // that chord genuinely toggles both ways.
    assert_eq!(
        shortcut_for(&MenuAction::ToggleCollapseTerminal(0)),
        None,
        "a one-way chord must not be advertised beside a two-way label"
    );
    assert_eq!(
        shortcut_for(&MenuAction::ToggleMaximizeTerminal(0)),
        Some("⌘K M"),
        "and the genuinely two-way sibling keeps its hint, so this is not just \
         the hint lookup being broken"
    );
}

// ---- Keyboard Shortcuts editor (#612) ----

#[test]
fn recording_a_shortcut_writes_it_and_takes_effect_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keybindings_file = tmp.path().join("keybindings.json");
    app.open_keyboard_shortcuts();
    let cmd = crate::widgets::command_palette::Command::ToggleLiveRun;
    app.recording_shortcut = Some((cmd, std::time::Instant::now()));
    // Plain typing is refused and ends the recording.
    app.handle_key(key(KeyCode::Char('j'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.status.contains("needs a function key"),
        "{}",
        app.status
    );
    assert!(!app.keybindings_file.exists());
    app.recording_shortcut = Some((cmd, std::time::Instant::now()));
    app.handle_key(key(
        KeyCode::Char('j'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert!(
        app.status.starts_with("ctrl+alt+j now runs"),
        "{}",
        app.status
    );
    let written = std::fs::read_to_string(&app.keybindings_file).unwrap();
    assert!(written.contains("\"toggle_live_run\""), "{written}");
    assert_eq!(app.keymap.chord_for(cmd).as_deref(), Some("ctrl+alt+j"));
}

#[test]
fn escape_leaves_the_shortcut_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keybindings_file = tmp.path().join("keybindings.json");
    app.recording_shortcut = Some((
        crate::widgets::command_palette::Command::ToggleLiveRun,
        std::time::Instant::now(),
    ));
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.status, "Shortcut unchanged");
    assert!(!app.keybindings_file.exists());
}

#[test]
fn a_stale_or_clicked_away_shortcut_prompt_records_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keybindings_file = tmp.path().join("keybindings.json");
    let cmd = crate::widgets::command_palette::Command::ToggleLiveRun;
    let long_ago = std::time::Instant::now() - std::time::Duration::from_secs(60);
    app.recording_shortcut = Some((cmd, long_ago));
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(
        !app.keybindings_file.exists(),
        "a late chord is not recorded"
    );
    app.recording_shortcut = Some((cmd, std::time::Instant::now()));
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.recording_shortcut.is_none(),
        "a click cancels the prompt"
    );
}

/// The platform's command modifier: Cmd on macOS, Ctrl elsewhere.
fn mod_key_1482() -> KeyModifiers {
    if cfg!(target_os = "macos") {
        KeyModifiers::SUPER
    } else {
        KeyModifiers::CONTROL
    }
}

/// An app recording `cmd`'s shortcut into a keybindings.json in `tmp`.
fn recording_1482(tmp: &std::path::Path, cmd: crate::widgets::command_palette::Command) -> App {
    let mut app = App::new(tmp.to_path_buf()).unwrap();
    app.keybindings_file = tmp.join("keybindings.json");
    app.recording_shortcut = Some((cmd, std::time::Instant::now()));
    app
}

/// #1482: a chord a built-in command owns (Ctrl+/ is Toggle Line Comment)
/// is not taken on the first press: croft names the owner and waits, and
/// Esc writes nothing.
#[test]
fn recording_a_built_in_chord_asks_first_and_esc_writes_nothing() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = recording_1482(tmp.path(), Command::TransformUpper);
    let slash = key(KeyCode::Char('/'), mod_key_1482());
    app.handle_key(slash).unwrap();
    assert!(
        app.status
            .contains("\u{201c}Toggle Line Comment\u{201d}'s shortcut")
            && app
                .status
                .contains("Enter to give it to \u{201c}Transform to Uppercase\u{201d}"),
        "{}",
        app.status
    );
    assert!(!app.keybindings_file.exists(), "nothing is written yet");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.status, "Shortcut unchanged");
    assert!(!app.keybindings_file.exists());
    assert_eq!(app.keymap.command_for(slash), None);
    // Any other key cancels too, and is not typed or run.
    let mut app = recording_1482(tmp.path(), Command::TransformUpper);
    app.handle_key(slash).unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.status, "Shortcut unchanged");
    assert!(!app.keybindings_file.exists());
}

/// #1482: Enter gives the chord to the new command, says what it ran, and
/// the palette and Keyboard Shortcuts stop showing it on the old one.
#[test]
fn confirming_a_taken_chord_moves_it_and_its_hint() {
    use crate::widgets::command_palette::{Command, PaletteItem};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = recording_1482(tmp.path(), Command::TransformUpper);
    let slash = key(KeyCode::Char('/'), mod_key_1482());
    app.handle_key(slash).unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.status
            .ends_with("(it ran \u{201c}Toggle Line Comment\u{201d})"),
        "{}",
        app.status
    );
    assert_eq!(app.keymap.command_for(slash), Some(Command::TransformUpper));
    let written = std::fs::read_to_string(&app.keybindings_file).unwrap();
    assert!(written.contains("\"transform_upper\""), "{written}");

    app.open_command_palette();
    let palette = app.command_palette.as_ref().unwrap();
    assert_eq!(
        palette.hint_for(&PaletteItem::Builtin(Command::ToggleLineComment)),
        ""
    );
    let modifier = if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    };
    assert_eq!(
        palette.hint_for(&PaletteItem::Builtin(Command::TransformUpper)),
        format!("{modifier}+/")
    );
    assert_eq!(
        palette.hint_for(&PaletteItem::Builtin(Command::ToggleBlockComment)),
        "Shift+Alt+A",
        "untouched shortcuts keep their hint"
    );
    app.command_palette = None;

    app.open_keyboard_shortcuts();
    let rows = &app.list_picker.as_ref().unwrap().rows;
    let row = |id: &str| {
        rows.iter()
            .find(|r| r.id == format!("kb:{id}"))
            .unwrap()
            .label
            .clone()
    };
    assert!(
        row("toggle_line_comment").ends_with("\u{2014}"),
        "{}",
        row("toggle_line_comment")
    );
    assert!(
        row("transform_upper").ends_with("(yours)"),
        "{}",
        row("transform_upper")
    );
}

/// #1482 negative: a free chord, or the command's own built-in chord, is
/// still written on the first press, with no question asked.
#[test]
fn a_free_chord_or_the_commands_own_chord_applies_at_once() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = recording_1482(tmp.path(), Command::TransformUpper);
    app.handle_key(key(
        KeyCode::Char('j'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert!(app.confirming_shortcut.is_none());
    assert_eq!(
        app.status,
        "ctrl+alt+j now runs \u{201c}Transform to Uppercase\u{201d}"
    );
    let comment = recording_1482(tmp.path(), Command::ToggleLineComment);
    let mut app = comment;
    app.handle_key(key(KeyCode::Char('/'), mod_key_1482()))
        .unwrap();
    assert!(app.confirming_shortcut.is_none(), "{}", app.status);
    assert_eq!(
        app.keymap
            .command_for(key(KeyCode::Char('/'), mod_key_1482())),
        Some(Command::ToggleLineComment)
    );
}

/// #1482 negative: a chord the user's own keybindings.json gave another
/// command asks too, and a click away cancels the question.
#[test]
fn a_users_own_chord_asks_too_and_a_click_cancels() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = recording_1482(tmp.path(), Command::ToggleLiveRun);
    let chord = key(
        KeyCode::Char('j'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    app.handle_key(chord).unwrap();
    let before = std::fs::read_to_string(&app.keybindings_file).unwrap();
    app.recording_shortcut = Some((Command::TransformUpper, std::time::Instant::now()));
    app.handle_key(chord).unwrap();
    assert!(
        app.status
            .contains("\u{201c}Python: Toggle Live Run\u{201d}'s shortcut"),
        "{}",
        app.status
    );
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    assert!(app.confirming_shortcut.is_none());
    assert_eq!(app.status, "Shortcut unchanged");
    assert_eq!(
        std::fs::read_to_string(&app.keybindings_file).unwrap(),
        before
    );
    assert_eq!(app.keymap.command_for(chord), Some(Command::ToggleLiveRun));
}

/// #1482: holding the recorded chord long enough to auto-repeat does not
/// answer the question; Enter afterwards still reassigns it.
#[test]
fn a_repeat_of_the_recorded_chord_keeps_the_question_open() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = recording_1482(tmp.path(), Command::TransformUpper);
    let slash = key(KeyCode::Char('/'), mod_key_1482());
    app.handle_key(slash).unwrap();
    let mut repeat = slash;
    repeat.kind = KeyEventKind::Repeat;
    app.handle_key(repeat).unwrap();
    app.handle_key(repeat).unwrap();
    assert!(app.confirming_shortcut.is_some(), "{}", app.status);
    assert!(!app.keybindings_file.exists());
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.keymap.command_for(slash), Some(Command::TransformUpper));
}

/// #1482: a built-in chord several commands share names all of them, since
/// the new binding takes it from each; a command that is one of them is
/// asked about the others.
#[test]
fn a_shared_built_in_chord_names_every_owner() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let f7 = key(KeyCode::F(7), KeyModifiers::NONE);
    let mut app = recording_1482(tmp.path(), Command::TransformUpper);
    app.handle_key(f7).unwrap();
    assert!(
        app.status.contains(
            "\u{201c}Merge Conflict: Next Conflict\u{201d} and \u{201c}Go to Next Change\u{201d}'s shortcut"
        ),
        "{}",
        app.status
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.status.ends_with(
            "(it ran \u{201c}Merge Conflict: Next Conflict\u{201d} and \u{201c}Go to Next Change\u{201d})"
        ),
        "{}",
        app.status
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut app = recording_1482(tmp.path(), Command::GoToNextChange);
    app.handle_key(f7).unwrap();
    assert!(
        app.status
            .contains(" is \u{201c}Merge Conflict: Next Conflict\u{201d}'s shortcut"),
        "{}",
        app.status
    );
}

/// #262: a config file croft did not write itself (one that arrived by
/// config sync) is applied within the watch interval, not on next launch;
/// a macro croft records itself is not reloaded behind its back.
#[test]
fn a_macros_file_written_outside_croft_is_reloaded_live() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let store = tmp.path().join("macros.json");
    app.macros_path = store.clone();
    app.config_watch = crate::config_sync::ConfigWatch::new(vec![store.clone()]);
    app.macro_registers.clear();
    let mut mac = crate::macros::Macro::default();
    mac.push_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::NONE,
    ));
    crate::macros::save_register(&store, "q", mac).unwrap();
    let t0 = std::time::Instant::now();
    assert!(!app.tick_config_watch_at(t0), "not before the interval");
    assert!(app.tick_config_watch_at(t0 + crate::config_sync::ConfigWatch::INTERVAL * 2));
    assert!(
        app.macro_registers.contains_key("q"),
        "{:?}",
        app.macro_registers.keys()
    );
    assert!(app.status.starts_with("Macros reloaded"), "{}", app.status);
    // Nothing more changed: the next check reloads nothing.
    assert!(!app.tick_config_watch_at(t0 + crate::config_sync::ConfigWatch::INTERVAL * 4));
}

/// #852 (comment 3): the tab commands bind from keybindings.json like any
/// palette command, which gives Close Others (no chord of its own) and Pin
/// a key on Linux without Super.
#[test]
fn keybindings_json_chords_run_pin_editor_and_close_other_editors() {
    let (mut app, _tmp) = tabs_app_852(&["a.rs", "b.rs", "c.rs"]);
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[
            { "key": "ctrl+alt+p", "command": "pin_editor" },
            { "key": "ctrl+alt+o", "command": "close_other_editors" }
        ]"#,
    );
    app.handle_key(key(
        KeyCode::Char('p'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert!(app.editor.pinned, "ctrl+alt+p pins the active c.rs");
    assert_eq!(tab_names_852(&app.editor), ["c.rs", "a.rs", "b.rs"]);
    app.editor.select(2);
    app.handle_key(key(
        KeyCode::Char('o'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert_eq!(
        tab_names_852(&app.editor),
        ["c.rs", "b.rs"],
        "ctrl+alt+o closes the others, the pinned tab surviving as in the tab menu"
    );
}

/// #852 (comment 3): the Keyboard Shortcuts editor lists each tab command
/// with the chord croft has for it, like every other palette command.
#[test]
fn the_keyboard_shortcuts_editor_lists_the_editor_tab_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_keyboard_shortcuts();
    let picker = app.list_picker.as_ref().expect("the editor opens");
    for (id, title, chord) in [
        (
            "close_other_editors",
            "View: Close Other Editors in Group",
            "\u{2014}",
        ),
        (
            "close_editors_to_the_right",
            "View: Close Editors to the Right in Group",
            "Cmd+K →",
        ),
        (
            "close_saved_editors",
            "View: Close Saved Editors in Group",
            "Cmd+K U",
        ),
        ("close_all_editors", "View: Close All Editors", "Cmd+K W"),
        ("pin_editor", "View: Pin Editor", "Cmd+K P"),
        ("unpin_editor", "View: Unpin Editor", "Cmd+K P"),
        ("keep_editor", "View: Keep Editor", "Cmd+K Shift+P"),
    ] {
        let row = picker
            .rows
            .iter()
            .find(|r| r.id == format!("kb:{id}"))
            .unwrap_or_else(|| panic!("no Keyboard Shortcuts row for {id}"));
        // Off macOS the row shows the Ctrl+K leader croft takes there (#843).
        let chord = if cfg!(target_os = "macos") {
            chord.to_string()
        } else {
            chord.replace("Cmd+K", "Ctrl+K")
        };
        assert_eq!(row.label, format!("{title}  \u{00b7}  {chord}"));
    }
}

/// The confirm popup disarms a pending Cmd+K chord: its next key answers the
/// popup, it does not finish the chord (Cmd+K I is the broadcast toggle).
#[test]
fn a_multiline_paste_confirm_disarms_a_pending_cmd_k_chord() {
    let (mut app, _tmp) = plain_paste_app();
    app.cmd_k_leader = Some(std::time::Instant::now());
    app.handle_paste(TWO_LINES);
    assert!(app.pending_terminal_paste.is_some());
    assert!(app.cmd_k_leader.is_none(), "the chord is disarmed");
    app.handle_key(key(KeyCode::Char('i'), KeyModifiers::NONE))
        .unwrap();
    assert!(!app.pending_broadcast_enable, "no broadcast prompt");
    assert!(!app.broadcast_input);
}
