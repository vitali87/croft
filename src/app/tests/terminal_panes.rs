//! Terminal panes as a set: splitting, closing, maximizing, folding,
//! reordering, labels and restoring them across launches.

use super::*;

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
fn terminal_rename_split_and_kill_act_on_the_active_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    assert!(run_from_palette(&mut app, "Terminal: Split Terminal"));
    assert_eq!(app.terminals.len(), 2);
    assert!(app.show_terminal);
    app.active_terminal = 1;
    assert!(run_from_palette(&mut app, "Terminal: Rename"));
    assert!(matches!(
        app.prompt.as_ref().map(|p| &p.kind),
        Some(PromptKind::RenameTerminal(1))
    ));
    app.prompt = None;
    assert!(run_from_palette(
        &mut app,
        "Terminal: Kill the Active Terminal Instance"
    ));
    assert_eq!(app.terminals.len(), 1);
}

/// #1562 negative: Kill on the last pane keeps it, as Cmd+W does.
#[test]
fn terminal_kill_keeps_the_last_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::TerminalKill);
    assert_eq!(app.terminals.len(), 1);
    assert!(app.status.starts_with("Cannot close the last terminal"));
}

/// Guard (#843): with one terminal, next / previous stay on it, and they
/// skip a folded pane as Cmd+] does, never landing on one.
#[test]
fn the_terminal_focus_commands_stay_put_alone_and_skip_folded_panes() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(Command::FocusNextTerminal);
    assert_eq!(app.active_terminal, 0);
    app.run_command(Command::FocusPreviousTerminal);
    assert_eq!(app.active_terminal, 0);

    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    app.active_terminal = 0;
    app.terminals[1].collapsed = true;
    app.run_command(Command::FocusNextTerminal);
    assert_eq!(app.active_terminal, 2, "the folded pane is skipped");
    app.run_command(Command::FocusPreviousTerminal);
    assert_eq!(app.active_terminal, 0);
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

/// #912: `exit` in a pane left it open, dead and swallowing input. It now
/// closes, as VS Code closes a terminal whose shell exits, and is not
/// parked for undo-close: there is no shell to bring back.
#[test]
fn a_pane_whose_shell_exits_closes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    let survivor = app.terminals[0].uid();
    app.terminals[1].write_input(b"exit 2\n");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1500),
        "the exited pane to close",
        || {
            app.drain_terminal_bells();
            app.terminals.len() == 1
        },
    );
    assert_eq!(app.terminals[0].uid(), survivor);
    assert_eq!(app.active_terminal, 0);
    assert!(app.closed_terminals.is_empty(), "parked for undo-close");
    assert!(app.status.ends_with("exited with code 2"), "{}", app.status);
}

/// #912: the last pane cannot close, so when its shell exits it stays,
/// takes no input, and is not counted active; a key starts a new shell in
/// its place.
#[test]
fn the_last_pane_whose_shell_exits_waits_for_a_key_to_start_a_new_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let dead = app.terminals[0].uid();
    app.terminals[0].write_input(b"exit\n");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1500),
        "the last pane's shell to exit",
        || {
            app.drain_terminal_bells();
            app.terminals[0].has_exited()
        },
    );
    assert_eq!(app.terminals.len(), 1);
    assert_eq!(app.live_terminal_count(), 0);
    assert!(app.status.ends_with("start a new shell"), "{}", app.status);
    app.focus_pane(Pane::Terminal);
    app.handle_key(key(KeyCode::Char('l'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.terminals.len(), 1);
    assert_ne!(app.terminals[0].uid(), dead, "a new shell's pane");
    assert!(!app.terminals[0].has_exited());
    assert!(app.closed_terminals.is_empty());
    assert_eq!(app.status, "Started a new shell");
}

/// #912: a paste is input like a key, so into the last pane after its shell
/// exited it starts a new shell, as a typed key does, instead of going to
/// the shell that is gone.
#[test]
fn a_paste_into_the_last_exited_pane_starts_a_new_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let dead = app.terminals[0].uid();
    app.terminals[0].write_input(b"exit\n");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1500),
        "the last pane's shell to exit",
        || {
            app.drain_terminal_bells();
            app.terminals[0].has_exited()
        },
    );
    app.focus_pane(Pane::Terminal);
    app.paste_terminal_input(b"ls\n");
    assert_eq!(app.terminals.len(), 1);
    assert_ne!(app.terminals[0].uid(), dead, "a new shell's pane");
    assert!(
        app.pending_terminal_paste.is_none(),
        "no paste prompt for a dead shell"
    );
    assert_eq!(app.status, "Started a new shell");
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
fn clicking_terminal_maximize_button_maximizes_that_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    assert_eq!(app.active_terminal, 2);
    app.terminal_max_buttons = vec![
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
    assert!(
        app.terminal_pane_maximized,
        "the ⛶ click enters maximize mode"
    );
    assert_eq!(
        app.active_terminal, 0,
        "the pane whose ⛶ was clicked becomes the maximized one"
    );
}

/// #510: a pane the frame does not paint must drop `last_inner` as well as
/// `last_area`.
///
/// `last_area` guards clicks and is cleared at six terminal-pane sites.
/// `last_inner` was assigned in exactly one place - inside `render` - and
/// cleared nowhere, so a hidden pane kept the rect it held under the PREVIOUS
/// layout. That rect anchors the terminal image overlay, which is emitted
/// after `terminal.draw()` with absolute cursor positioning and is bounded by
/// nothing else: `update_terminal_image_overlay` checks the payload against
/// the same stale rect, so every check passes and the write lands wherever
/// the old origin pointed - over a sibling pane, the sidebar, or the status
/// bar. Being a raw write rather than a buffer index it cannot panic, and
/// ratatui's diff has no record of the cells it touched, so nothing repairs
/// them on the next frame.
///
/// Maximize is the reachable route: it hands every non-active pane
/// `Rect::default()`, so the render loop skips them while they keep the split
/// column they occupied a frame earlier.
#[test]
fn a_hidden_pane_drops_last_inner_not_just_last_area() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    let backend = ratatui::backend::TestBackend::new(180, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();

    // Paint the split first, so every pane holds a real inner rect.
    term.draw(|f| app.render(f)).unwrap();
    let active = app.active_terminal;
    let hidden = (0..app.terminals.len()).find(|i| *i != active).unwrap();
    assert!(
        app.terminals[hidden].last_inner.width > 0,
        "precondition: the pane painted a real inner rect while split"
    );

    // Maximize, which stops painting it.
    app.terminal_pane_maximized = true;
    term.draw(|f| app.render(f)).unwrap();

    assert_eq!(
        app.terminals[hidden].last_inner,
        Rect::default(),
        "a pane the frame skipped must not keep an inner rect the overlay \
         would anchor a raw write to"
    );
}

/// The collapse arm of the same condition. `t.collapsed && !maximized` is the
/// other route into that clear, and the maximize tests never reach it.
#[test]
fn a_collapsed_pane_drops_last_inner_too() {
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    term.draw(|f| app.render(f)).unwrap();
    assert!(app.terminals[1].last_inner.width > 0, "precondition");

    app.toggle_terminal_collapse(1);
    term.draw(|f| app.render(f)).unwrap();

    assert_eq!(
        app.terminals[1].last_inner,
        Rect::default(),
        "a folded pane keeps no rect for the overlay to anchor to"
    );
}

#[test]
fn maximized_terminal_fills_width_and_lists_others_in_rail() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    app.terminal_pane_maximized = true;
    let backend = ratatui::backend::TestBackend::new(180, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let active = app.active_terminal;
    assert!(
        app.terminals[active].last_area.width >= 100,
        "the maximized pane must span (almost) the whole panel width, got {}",
        app.terminals[active].last_area.width
    );
    for (i, t) in app.terminals.iter().enumerate() {
        if i != active {
            assert_eq!(
                t.last_area,
                Rect::default(),
                "hidden pane {i} must drop its hit rect so clicks can't phantom-match"
            );
        }
    }
    assert_eq!(
        app.terminal_rail_rects.len(),
        3,
        "the right-side rail lists every terminal"
    );
    let xs: std::collections::HashSet<u16> = app.terminal_rail_rects.iter().map(|r| r.x).collect();
    assert_eq!(xs.len(), 1, "rail rows stack vertically in one column");
    let ys: std::collections::HashSet<u16> = app.terminal_rail_rects.iter().map(|r| r.y).collect();
    assert_eq!(ys.len(), 3, "each rail row sits on its own line");
    // Index alignment: in maximize mode only the visible pane carries live
    // button rects; the hidden panes hold empty placeholders so a button's
    // position in the vec still names its terminal index.
    assert_eq!(app.terminal_max_buttons.len(), 3);
    let live: Vec<usize> = app
        .terminal_max_buttons
        .iter()
        .enumerate()
        .filter(|(_, r)| r.width > 0)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        live,
        vec![active],
        "only the maximized pane paints a restore button"
    );
}

#[test]
fn a_clamped_rail_never_paints_past_its_own_column() {
    // The rail width is clamped at the call site to
    // `content.width.saturating_sub(20)`, but the label was always rendered as
    // a fixed 7 cells (space + icon + space + 4 chars). `Buffer::set_string`
    // clips at the FRAME edge, not at the rail rect, so on a narrow panel the
    // tail of the label lands on whatever is drawn to the right of the panel.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.terminal_pane_maximized = true;
    // A label character no other widget can paint, so "this cell holds a label
    // char" is proof of a bleed and not a coincidence with the panel's text.
    app.terminals[0].set_manual_name(Some(String::from("§§§§")));
    app.terminals[1].set_manual_name(Some(String::from("§§§§")));

    // Wide enough that the buffer edge cannot mask the bleed, narrow enough
    // that the clamp bites: the assertion is the rail's own right edge.
    // The bleed is only observable when something is DRAWN to the right of the
    // panel, so the secondary side bar must be up: the panel clips at the frame
    // edge otherwise and the overrun is silently truncated.
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    app.secondary_side_bar_visible = true;
    app.sidebar_width = 21;
    term.draw(|f| app.render(f)).unwrap();

    assert!(
        app.terminal_rail_rects
            .iter()
            .any(|r| r.width < TERMINAL_RAIL_W),
        "this layout must actually clamp the rail, or the test proves nothing"
    );
    let cell = |x: u16, y: u16| {
        term.backend()
            .buffer()
            .cell(ratatui::layout::Position::new(x, y))
            .unwrap()
            .symbol()
            .to_string()
    };
    for rail in &app.terminal_rail_rects {
        let past = rail.x + rail.width;
        // Inside the rail the label must actually be painted, or a future
        // layout change could shrink the rail until nothing bleeds and the
        // assertion below passes while the bug is fully restored.
        let inside: String = (rail.x..past).map(|x| cell(x, rail.y)).collect();
        assert!(
            inside.contains('§'),
            "the rail must paint some of its label, got {inside:?}"
        );
        // Every cell from the rail's right edge to the frame edge: not a fixed
        // peephole, so the check survives any change to the pane constants.
        let bleed: String = (past..80).map(|x| cell(x, rail.y)).collect();
        assert!(
            !bleed.contains('§'),
            "the rail label bled past its own column into {bleed:?}"
        );
    }
}

#[test]
fn rail_is_a_fixed_narrow_column_showing_four_label_chars() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.terminal_pane_maximized = true;
    let backend = ratatui::backend::TestBackend::new(180, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();

    // The rail is a fixed, very narrow column: icon + exactly 4 label
    // characters, regardless of how short or long the names are.
    app.terminals[0].set_manual_name(Some(String::from("zsh")));
    app.terminals[1].set_manual_name(Some(String::from("a-very-long-terminal-pane-name")));
    term.draw(|f| app.render(f)).unwrap();
    for r in &app.terminal_rail_rects {
        assert_eq!(
            r.width, TERMINAL_RAIL_W,
            "the rail must always be exactly {TERMINAL_RAIL_W} cells wide"
        );
    }
    // The long name is cut to its first 4 characters in the rail row.
    let rail = app.terminal_rail_rects[1];
    let cells: String = (rail.x..rail.x + rail.width)
        .map(|x| {
            term.backend()
                .buffer()
                .cell(ratatui::layout::Position::new(x, rail.y))
                .unwrap()
                .symbol()
                .chars()
                .next()
                .unwrap()
        })
        .collect();
    assert!(
        cells.contains("a-ve") && !cells.contains("a-ver"),
        "rail row must show exactly the first 4 label chars, got {cells:?}"
    );
}

#[test]
fn clicking_rail_row_switches_terminal_and_stays_maximized() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    app.terminal_pane_maximized = true;
    let backend = ratatui::backend::TestBackend::new(180, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let r = app.terminal_rail_rects[0];
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: r.x + 1,
        row: r.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.active_terminal, 0, "the rail row picks its terminal");
    assert!(
        app.terminal_pane_maximized,
        "shuffling via the rail stays in maximize mode"
    );
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.terminals[0].last_area.width >= 100,
        "the newly picked terminal now owns the maximized pane"
    );
}

#[test]
fn a_long_pane_label_never_overpaints_the_maximize_button() {
    // The header buttons lay out right-to-left, and the `⛶` added by this span
    // is the leftmost of four, at `col.width - 13`. The pane pill's width
    // budget still reserved room for three buttons, so a label that fills its
    // budget painted straight over the glyph — worst with the broadcast `⇶`
    // or a host accent, whose prefixes widen the pill by two more cells and
    // erase it entirely. The button's hit rect stays live underneath, so those
    // cells silently maximize instead of starting the drag-reorder the pill
    // advertises.
    //
    // Both label characters are deliberate: `§` is one cell, so it pins the
    // char budget, and `項` is two, so it pins that the CLIP is measured in
    // display cells. A budget counted in chars passes the `§` case while a
    // wide-character name still runs over the button.
    for name in ["§".repeat(30), "項".repeat(30)] {
        assert_pane_pill_clears_its_buttons(&name);
    }
}

fn assert_pane_pill_clears_its_buttons(name: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for _ in 0..3 {
        app.split_terminal().unwrap();
    }
    for t in app.terminals.iter_mut() {
        t.set_manual_name(Some(String::from(name)));
    }
    // Broadcast is on so the pill wears its widest form.
    app.broadcast_input = true;
    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let glyph = name.chars().next().unwrap();

    let cell = |x: u16, y: u16| {
        term.backend()
            .buffer()
            .cell(ratatui::layout::Position::new(x, y))
            .unwrap()
            .symbol()
            .to_string()
    };
    let mut checked = 0;
    // Every button in the strip, not only `⛶`. Since #313 the leftmost is the
    // `‹` collapse button, and checking the old leftmost alone would leave the
    // new one free to be overpainted - the same bug this test exists for,
    // three columns over.
    let families: [(&str, &Vec<Rect>, char); 2] = [
        ("collapse", &app.terminal_collapse_buttons, '\u{eab5}'),
        ("maximize", &app.terminal_max_buttons, '\u{eb4c}'),
    ];
    for (i, pill) in app.terminal_label_rects.iter().enumerate() {
        let painted: Vec<(&str, Rect, char)> = families
            .iter()
            .filter(|(_, rects, _)| rects[i].width > 0)
            .map(|(f, rects, want)| (*f, rects[i], *want))
            .collect();
        if painted.is_empty() {
            continue;
        }
        // The pill has to run right up to the button strip, or a layout that
        // simply left it short would pass this while the bug stands. Measured
        // against the LEFTMOST button: the pill is clipped at the strip's
        // edge, so asking it to also reach the rightmost would be asking it to
        // be in two places at once.
        //
        // One cell of slack, and only one: a two-cell grapheme that would
        // straddle the clip boundary is dropped whole rather than half-painted,
        // so a `項` name legitimately stops one column short where a `§` name
        // lands exactly on it. Two cells of slack would start admitting pills
        // that were never long enough to threaten the buttons at all.
        let leftmost = painted.iter().map(|(_, r, _)| r.x).min().unwrap();
        assert!(
            pill.width > 0 && pill.x + pill.width + 1 >= leftmost,
            "pane {i}'s pill must reach the button strip for this to prove \
             anything, pill={pill:?} strip starts at {leftmost}"
        );
        for (family, btn, want) in painted {
            let cells: String = (btn.x..btn.x + btn.width).map(|x| cell(x, btn.y)).collect();
            assert!(
                !cells.contains(glyph),
                "pane {i}'s {glyph:?} label painted over its {family} button: {cells:?}"
            );
            assert!(
                cells.contains(want),
                "pane {i}'s {family} glyph must survive a {glyph:?} label, got {cells:?}"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 4,
        "this layout must paint both button families to test"
    );
}

#[test]
fn a_rail_taller_than_the_panel_scrolls_instead_of_dropping_terminals() {
    // The rail painted one row per terminal and stopped at the panel's bottom
    // edge, so with more terminals than rows the tail simply vanished: no row,
    // no hit rect, and if the active terminal was among them nothing in the
    // rail was highlighted at all. The panel floor is 3 rows, so four panes
    // are enough to reach this.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for _ in 0..5 {
        app.split_terminal().unwrap();
    }
    app.terminal_pane_maximized = true;
    app.terminal_height = Some(TERMINAL_HEIGHT_MIN);
    let backend = ratatui::backend::TestBackend::new(180, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let live = |app: &App| -> usize {
        app.terminal_rail_rects
            .iter()
            .filter(|r| r.width > 0)
            .count()
    };
    assert!(
        live(&app) < app.terminals.len(),
        "this layout must actually overflow the rail, or the test proves nothing"
    );
    assert_eq!(
        app.terminal_rail_rects.len(),
        app.terminals.len(),
        "off-window rows still hold a placeholder so a rect's position names \
         its terminal"
    );
    // The active terminal is the last one, past the window's bottom edge.
    assert!(
        app.terminal_rail_rects[app.active_terminal].width > 0,
        "the rail must scroll to keep the active terminal visible"
    );

    // The wheel reaches the terminals the window has scrolled past.
    let rail = app.terminal_rail_area;
    assert!(
        rail.width > 0,
        "the rail records its own area for the wheel"
    );
    for _ in 0..app.terminals.len() {
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollUp,
            column: rail.x,
            row: rail.y,
            modifiers: KeyModifiers::NONE,
        });
    }
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.terminal_rail_rects[0].width > 0,
        "scrolling the rail up must reach the first terminal"
    );
}

#[test]
fn cmd_k_m_toggles_terminal_maximize() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('m'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.terminal_pane_maximized,
        "Cmd+K M maximizes the active pane"
    );
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('m'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        !app.terminal_pane_maximized,
        "Cmd+K M again restores the split"
    );
}

#[test]
fn closing_down_to_one_terminal_exits_maximize() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.terminal_pane_maximized = true;
    assert!(app.close_terminal_at(0));
    assert!(
        !app.terminal_pane_maximized,
        "a lone terminal has nothing to maximize against; the flag resets"
    );
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

#[test]
fn renaming_a_terminal_pane_sets_and_clears_its_label() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.begin_rename_terminal(0);
    match app.prompt.as_mut() {
        Some(p) => {
            assert!(matches!(p.kind, super::PromptKind::RenameTerminal(0)));
            p.buffer = "logs".into();
        }
        None => panic!("rename prompt did not open"),
    }
    app.commit_prompt();
    assert_eq!(
        app.terminals[0].label(),
        "logs",
        "rename sets the manual name"
    );
    // A blank rename clears the manual name (falls back to the auto label).
    app.begin_rename_terminal(0);
    app.prompt.as_mut().unwrap().buffer = "   ".into();
    app.commit_prompt();
    assert_ne!(
        app.terminals[0].label(),
        "logs",
        "blank rename clears the name"
    );
}

#[test]
fn a_second_pane_shows_its_label() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.bottom_panel_tab = BottomPanelTab::Terminal;
    app.show_terminal = true;
    // One pane => no label clutter; split to two, name the first.
    app.split_terminal().unwrap();
    app.terminals[0].set_manual_name(Some("server".into()));
    draw(&mut app, 160, 50);
    let buf_text: String = {
        let backend = ratatui::backend::TestBackend::new(160, 50);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        term.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    };
    assert!(
        buf_text.contains("server"),
        "the named pane shows its label"
    );
}

#[test]
fn clicking_the_profile_caret_opens_an_anchored_shell_menu() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.bottom_panel_tab = BottomPanelTab::Terminal;
    app.show_terminal = true;
    draw(&mut app, 140, 50);
    let caret = *app
        .terminal_profile_buttons
        .first()
        .expect("the profile caret is rendered on the pane header");
    assert!(app.context_menu.is_none());
    left_click(&mut app, caret.x, caret.y);
    let menu = app
        .context_menu
        .as_ref()
        .expect("a context menu opens on caret click");
    // Anchored at the caret (drops one row below it), not a centered modal.
    assert_eq!(
        menu.origin,
        (caret.x, caret.y + 1),
        "the menu anchors under the caret",
    );
    // Every row launches a shell profile.
    assert!(!menu.items.is_empty(), "the menu lists shells");
    assert!(
        menu.items.iter().all(|e| matches!(
            e,
            MenuEntry::Item {
                action: MenuAction::NewTerminalWithProfile(_),
                ..
            }
        )),
        "every row opens a new terminal with a shell profile",
    );
}

#[test]
fn drag_reorders_terminal_panes_and_active_follows_its_terminal() {
    // Dragging a pane label (side-by-side) or a rail row (maximize mode)
    // live-moves that terminal to the hovered slot. The reorder must keep
    // `active_terminal` pinned to the same underlying terminal: the dragged
    // pane stays active while it moves, and an unrelated active pane must not
    // silently switch because its index shifted.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    for (i, name) in ["one", "two", "three"].iter().enumerate() {
        app.terminals[i].set_manual_name(Some(name.to_string()));
    }

    app.active_terminal = 0;
    app.move_terminal(0, 2);
    let order: Vec<&str> = app.terminals.iter().map(|t| t.label()).collect();
    assert_eq!(order, ["two", "three", "one"], "pane moved to the far slot");
    assert_eq!(app.active_terminal, 2, "the dragged pane stays active");

    app.active_terminal = 1; // "three"
    app.move_terminal(2, 0); // drag "one" back to the front
    let order: Vec<&str> = app.terminals.iter().map(|t| t.label()).collect();
    assert_eq!(order, ["one", "two", "three"]);
    assert_eq!(
        app.terminals[app.active_terminal].label(),
        "three",
        "an unrelated active pane keeps its terminal when indices shift"
    );

    // Out-of-range and no-op moves must not disturb anything.
    app.move_terminal(1, 1);
    app.move_terminal(9, 0);
    app.move_terminal(0, 9);
    let order: Vec<&str> = app.terminals.iter().map(|t| t.label()).collect();
    assert_eq!(order, ["one", "two", "three"]);
}

#[test]
fn reordering_panes_defers_the_session_write_off_the_input_path() {
    // move_terminal runs once per motion report during a drag. A full
    // read-modify-write of the session file — plus a cwd syscall and a stat
    // per pane — on each one violates the input-latency invariant, so the
    // write is marked and flushed once per tick instead.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // The test-mode session path is per PROCESS, shared by every test in
    // this binary: a concurrent test's save would recreate the file between
    // the remove below and the non-existence assert. Absence is only
    // provable on a path this test owns alone.
    app.terminal_session_path = tmp.path().join("terminal-sessions.json");
    app.split_terminal().unwrap();
    let _ = std::fs::remove_file(&app.terminal_session_path);
    app.move_terminal(0, 1);
    assert!(
        !app.terminal_session_path.exists(),
        "the session write must not sit on the input path"
    );
    assert!(
        app.terminal_session_dirty,
        "the move is recorded as pending"
    );
    app.flush_terminal_session();
    assert!(
        app.terminal_session_path.exists(),
        "the deferred write lands on the flush"
    );
    assert!(!app.terminal_session_dirty);
}

#[test]
fn a_drag_held_over_one_pane_does_not_flip_flop_between_frames() {
    // The main loop drains the whole queued mouse burst before drawing, so
    // several Drag events land with no redraw between them. Hit-testing
    // must stay POSITIONAL across a move: `last_area` travels with the
    // terminal object, so if the rects are not re-seated per slot, the
    // second event swaps the panes straight back and the final order is
    // the parity of how many motion reports the terminal happened to emit.
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    for (i, name) in ["one", "two", "three"].iter().enumerate() {
        app.terminals[i].set_manual_name(Some(name.to_string()));
    }
    let backend = ratatui::backend::TestBackend::new(180, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let mouse = |kind, x: u16, y: u16| MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    let grab = app.terminal_label_rects[0];
    let dest = app.terminals[1].last_area;
    let (dx, dy) = (dest.x + dest.width / 2, dest.y + dest.height / 2);
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        grab.x + 1,
        grab.y,
    ));
    // Four motion reports over the same cell, no redraw in between.
    for _ in 0..4 {
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), dx, dy));
    }
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), dx, dy));
    let order: Vec<&str> = app
        .terminals
        .iter()
        .map(|t| t.manual_name().unwrap_or(""))
        .collect();
    assert_eq!(
        order,
        vec!["two", "one", "three"],
        "the pane must settle where the pointer is, whatever the event count"
    );
}

#[test]
fn dragging_a_pane_label_and_a_rail_row_reorders_terminals_end_to_end() {
    // Full wiring pass over real hit rects: render a frame, press the first
    // pane's name pill, drag into the third pane, release - the terminal
    // must land in the far slot. Then the same through the maximize rail.
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    for (i, name) in ["one", "two", "three"].iter().enumerate() {
        app.terminals[i].set_manual_name(Some(name.to_string()));
    }
    let backend = ratatui::backend::TestBackend::new(180, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let mouse = |kind, x: u16, y: u16| MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    let grab = app.terminal_label_rects[0];
    assert!(grab.width > 0, "pane label pill must record a drag handle");
    let dest = app.terminals[2].last_area;
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        grab.x + 1,
        grab.y,
    ));
    let (dx, dy) = (dest.x + dest.width / 2, dest.y + 2);
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), dx, dy));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), dx, dy));
    let order: Vec<&str> = app.terminals.iter().map(|t| t.label()).collect();
    assert_eq!(order, ["two", "three", "one"], "label drag moves the pane");
    assert_eq!(app.active_terminal, 2, "the dragged pane lands focused");

    // Maximize mode: drag the top rail row onto the bottom one.
    app.terminal_pane_maximized = true;
    term.draw(|f| app.render(f)).unwrap();
    let from_row = app.terminal_rail_rects[0];
    let to_row = app.terminal_rail_rects[2];
    assert!(from_row.width > 0 && to_row.width > 0, "rail rows painted");
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        from_row.x + 1,
        from_row.y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        to_row.x + 1,
        to_row.y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        to_row.x + 1,
        to_row.y,
    ));
    let order: Vec<&str> = app.terminals.iter().map(|t| t.label()).collect();
    assert_eq!(order, ["three", "one", "two"], "rail drag moves the row");
}

#[test]
fn terminal_session_restores_pane_layout_names_and_focus_across_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let session_path = tmp.path().join("sessions.json");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminal_session_path = session_path.clone();
    app.split_terminal().unwrap();
    app.terminals[1].set_manual_name(Some(String::from("srv")));
    app.active_terminal = 1;
    app.save_terminal_session();
    let raw = std::fs::read_to_string(&session_path).unwrap();
    assert!(
        raw.contains("srv"),
        "the session file records pane names: {raw}"
    );

    // A fresh app on the same workspace resurrects the layout.
    let mut app2 = App::new(tmp.path().to_path_buf()).unwrap();
    app2.terminal_session_path = session_path.clone();
    app2.restore_terminal_session_with(true);
    assert_eq!(app2.terminals.len(), 2, "both panes come back");
    assert_eq!(app2.terminals[1].label(), "srv", "names survive");
    assert_eq!(
        app2.active_terminal, 1,
        "focus lands on the pane that had it"
    );
    // The restored pane runs a live shell.
    app2.terminals[1].write_input(b"s=ali; echo ${s}ve-42\n");
    // Load-scaled, not a fixed 8000ms: what blows a wait on a spawned shell
    // is contention, which is a property of what else the suite is doing
    // rather than of this test, and `test_budget` is where that reasoning
    // already lives (#307/#422).
    //
    // `await_spawned` multiplies the base by `BASE_CALIBRATION * load_scale`,
    // which that module's own test pins at 4 at MIN_SCALE and 8 at the cap.
    // A 2s base is therefore 8s at the floor and 16s at the cap: the 8000ms
    // this replaces is what the wait KEEPS on a quiet machine, and load buys
    // more on top. The floor is the point - `MIN_SCALE`'s own doc says it may
    // never shrink below the budgets these tests already had, since every one
    // of them was observed failing at 1x.
    crate::test_budget::await_spawned(
        crate::test_budget::tests::RESTORED_SHELL_BASE,
        "the restored pane's shell to answer",
        || {
            app2.terminals[1]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("alive-42"))
        },
    );

    // Closing back down to one default pane prunes the record, so a plain
    // single-shell workspace never grows the file.
    app2.terminals[1].set_manual_name(None);
    assert!(app2.close_terminal_at(1));
    app2.save_terminal_session();
    let map = crate::terminal_session::load(&session_path);
    assert!(
        !map.contains_key(&tmp.path().display().to_string()),
        "a trivial session (one unnamed pane at the root) is pruned"
    );
}

#[test]
fn a_local_launch_opens_one_terminal_not_the_saved_empty_panes() {
    // #1458: on this machine the saved panes' shells ended with the last
    // croft, so bringing them back only gave empty shells cluttering the
    // panel. A local launch keeps the one default terminal instead.
    let tmp = tempfile::tempdir().unwrap();
    let session_path = tmp.path().join("sessions.json");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminal_session_path = session_path.clone();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    app.terminals[1].set_manual_name(Some(String::from("srv")));
    app.active_terminal = 1;
    app.save_terminal_session();
    drop(app);

    let mut app2 = App::new(tmp.path().to_path_buf()).unwrap();
    app2.terminal_session_path = session_path.clone();
    let default_pane = app2.terminals[0].uid();
    app2.restore_terminal_session_with(false);
    assert_eq!(app2.terminals.len(), 1, "one terminal, not three");
    assert_eq!(
        app2.terminals[0].uid(),
        default_pane,
        "the default terminal is the one kept"
    );
    assert_ne!(app2.terminals[0].label(), "srv");
    assert_eq!(app2.active_terminal, 0);
    assert!(
        !app2.status.contains("Restored terminal session"),
        "nothing was restored: {}",
        app2.status
    );
}

#[test]
fn a_local_launch_focuses_the_first_pane_when_the_active_one_is_left_out() {
    // The saved active pane is a plain pane a local launch drops. Focus
    // falls back to the first pane brought back, not to the lane that
    // happened to follow the dropped one.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let session = tmp.path().join("sessions.json");
    let root_key = repo.display().to_string();
    let lane = |name: &str| {
        let dir = tmp.path().join(format!("repo-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        crate::terminal_session::PaneRecord {
            host: None,
            cwd: dir.display().to_string(),
            name: Some(format!("Lane: {name}")),
            transcript: Vec::new(),
            lane: Some(crate::terminal_session::LaneRecord {
                path: dir.display().to_string(),
                branch: format!("agent/{name}"),
                agent: None,
                base: None,
            }),
        }
    };
    let rec = crate::terminal_session::SessionRecord {
        panes: vec![
            lane("one"),
            crate::terminal_session::PaneRecord {
                host: None,
                cwd: root_key.clone(),
                name: Some(String::from("plain")),
                transcript: Vec::new(),
                lane: None,
            },
            lane("two"),
        ],
        active: 1,
    };
    crate::terminal_session::save_for_root(&session, &root_key, rec).unwrap();
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = session;
    app.restore_terminal_session_with(false);
    assert_eq!(app.terminals.len(), 2, "both lanes come back");
    assert_eq!(app.terminals[0].manual_name(), Some("Lane: one"));
    assert_eq!(app.active_terminal, 0, "focus falls back to the first pane");
}

#[cfg(target_os = "linux")]
#[test]
fn a_local_launch_reattaches_a_shell_still_running_in_its_pane_host() {
    // croft was killed, not quit: the shell is still running in its pane
    // host, with whatever it was doing on screen. That is not an empty
    // pane, so a local launch reattaches it, and drops the pane whose host
    // is gone instead of respawning it.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    let spec = crate::pane_host::PaneSpec {
        socket: tmp.path().join("pane.sock"),
        cwd: Some(root.clone()),
        env: Vec::new(),
        cols: 80,
        rows: 24,
        argv: ["sh", "-c", "echo still-$((6*7)); exec sh"]
            .iter()
            .map(|a| a.to_string())
            .collect(),
    };
    let socket = spec.socket.clone();
    let host = std::thread::spawn(move || crate::pane_host::serve(&spec));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !crate::session::is_alive(&socket) {
        assert!(
            std::time::Instant::now() < deadline,
            "the host never listened"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let root_key = root.display().to_string();
    let pane = |name: &str, host: Option<String>| crate::terminal_session::PaneRecord {
        cwd: root_key.clone(),
        name: Some(String::from(name)),
        lane: None,
        transcript: Vec::new(),
        host,
    };
    let session = tmp.path().join("sessions.json");
    let rec = crate::terminal_session::SessionRecord {
        panes: vec![
            pane(
                "gone",
                Some(tmp.path().join("dead.sock").display().to_string()),
            ),
            pane("kept", Some(socket.display().to_string())),
        ],
        active: 1,
    };
    crate::terminal_session::save_for_root(&session, &root_key, rec).unwrap();

    let mut app = App::new(root.clone()).unwrap();
    app.terminal_session_path = session;
    app.restore_terminal_session_with(false);
    assert_eq!(app.terminals.len(), 1, "the dead pane is not respawned");
    assert_eq!(app.terminals[0].host_socket(), Some(socket.as_path()));
    assert_eq!(app.terminals[0].manual_name(), Some("kept"));
    assert_eq!(app.active_terminal, 0);

    drop(app);
    host.join().unwrap().unwrap();
}

#[test]
fn change_workspace_root_saves_the_outgoing_workspaces_terminal_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let mut app = App::new(a.clone()).unwrap();
    app.terminal_session_path = tmp.path().join("sessions.json");
    // Name the pane directly (the rename UI would flush a save itself, and
    // this test must prove the RE-ROOT saves): a named pane is a
    // non-trivial layout the store keeps.
    app.terminals[0].set_manual_name(Some(String::from("srv")));
    assert!(
        !app.terminal_session_path.exists(),
        "precondition: nothing has saved the session yet"
    );

    app.change_workspace_root(b.clone());

    let map = crate::terminal_session::load(&app.terminal_session_path);
    let rec = map.get(&a.display().to_string()).expect(
        "re-rooting away must persist the outgoing workspace's pane layout under ITS key; \
         the layout was silently discarded",
    );
    assert_eq!(
        rec.panes[0].name.as_deref(),
        Some("srv"),
        "the saved record carries the outgoing panes"
    );
    assert!(
        !map.contains_key(&b.display().to_string()),
        "nothing may be filed under the incoming root's key by the re-root itself"
    );
}

/// #1458, negative: locally, re-rooting an untouched panel into a workspace
/// whose saved panes have all ended keeps the one default terminal rather
/// than bringing them back as empty shells.
#[test]
fn a_local_re_root_keeps_one_terminal_for_ended_panes() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let session_path = tmp.path().join("sessions.json");
    let pane = |name: Option<&str>| crate::terminal_session::PaneRecord {
        host: None,
        cwd: b.display().to_string(),
        name: name.map(String::from),
        transcript: Vec::new(),
        lane: None,
    };
    crate::terminal_session::save_for_root(
        &session_path,
        &b.display().to_string(),
        crate::terminal_session::SessionRecord {
            panes: vec![pane(None), pane(Some("srv")), pane(None)],
            active: 1,
        },
    )
    .unwrap();
    let mut app = App::new(a.clone()).unwrap();
    app.terminal_session_path = session_path;
    app.terminal_restore_respawns = false;
    app.change_workspace_root(b.clone());
    assert_eq!(app.terminals.len(), 1);
    assert_eq!(app.active_terminal, 0);
}

#[test]
fn change_workspace_root_keeps_live_panes_when_the_terminal_was_touched() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let session_path = tmp.path().join("sessions.json");
    crate::terminal_session::save_for_root(
        &session_path,
        &b.display().to_string(),
        crate::terminal_session::SessionRecord {
            panes: vec![
                crate::terminal_session::PaneRecord {
                    host: None,
                    cwd: b.display().to_string(),
                    name: None,
                    transcript: Vec::new(),
                    lane: None,
                },
                crate::terminal_session::PaneRecord {
                    host: None,
                    cwd: b.display().to_string(),
                    name: Some(String::from("srv")),
                    transcript: Vec::new(),
                    lane: None,
                },
            ],
            active: 0,
        },
    )
    .unwrap();

    let mut app = App::new(a.clone()).unwrap();
    app.terminal_session_path = session_path;
    // Any input byte marks the pane as the user's: its shell (history,
    // scrollback, possibly a job) must survive the re-root untouched even
    // though the incoming workspace has a saved layout.
    app.terminal_mut().write_input(b"echo touched\n");
    let uid = app.terminals[0].uid();

    app.change_workspace_root(b.clone());

    assert_eq!(
        app.terminals.len(),
        1,
        "a touched panel is never replaced by the incoming workspace's saved layout"
    );
    assert_eq!(
        app.terminals[0].uid(),
        uid,
        "the surviving pane is the same live shell, not a respawn"
    );
}

/// Opening a file while the terminal is maximized (Ctrl+Shift+J) must
/// restore the editor - the same convention VS Code follows when a file is
/// revealed over a maximized panel. Before this, focus silently moved into
/// an editor the layout gave zero rows: arrows did nothing visible and
/// typing dirtied a hidden buffer.
#[test]
fn opening_a_file_over_a_maximized_terminal_restores_the_editor() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminal_maximized = true;
    open_from_explorer(&mut app, "a.txt");
    assert!(matches!(app.focus, Pane::Editor));
    assert!(
        !app.terminal_maximized,
        "focusing the editor must restore it from under a maximized terminal"
    );
}

/// A collapsed pane records no geometry, and does not poison the next one.
///
/// `record_active_screen`'s guard is `size != self.recorded_size && size.0 > 0
/// && size.1 > 0`, and the two size conjuncts had never run: every fixture in
/// the suite holds a non-zero rect. They carry two claims. A pane whose
/// `last_inner` has collapsed must not write a degenerate `r` announcing a
/// zero-column window, and it must not overwrite `recorded_size` with that
/// zero, which would make the NEXT record emit a resize back to a geometry
/// the player already had.
///
/// Drop EITHER size conjunct and the sequence gains two `r` events instead
/// of none: `size.0 > 0` from the width fold, `size.1 > 0` from the height
/// fold. The first version of this doc named only the width half, which is
/// the shape an earlier round was pulled up for: the doc is what the next
/// editor reads before the assert message.
#[test]
fn a_collapsed_pane_records_no_resize_and_leaves_the_last_one_standing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0].last_inner = ratatui::layout::Rect {
        x: 1,
        y: 1,
        width: 40,
        height: 20,
    };
    app.terminals[0].resize(40, 20);

    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    app.record_active_screen();
    // Collapsed: the rect a pane gets when its split is folded to nothing.
    // `last_inner` only, deliberately: production also calls `resize`, which
    // floors the GRID at two columns, and the guard under test reads
    // `last_inner` rather than the grid.
    app.terminals[0].last_inner.width = 0;
    app.record_active_screen();
    // And back to the geometry the recording was opened at. If the collapsed
    // record had written its zero into `recorded_size`, THIS record would
    // announce a resize to 40x20, which the player already has.
    app.terminals[0].last_inner.width = 40;
    app.record_active_screen();
    // The HEIGHT conjunct, which the first version of this test left as dead
    // as it found it: with only `size.0 > 0` deleted the collapsed record
    // still has a zero width and the sequence does not move, so that mutation
    // was invisible. A zero-height fold exercises the other half.
    app.terminals[0].last_inner.height = 0;
    app.record_active_screen();
    app.terminals[0].last_inner.height = 20;
    app.record_active_screen();
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);

    let path = std::fs::read_dir(app.workspace_root())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "cast"))
        .expect("a .cast file was written");
    let text = std::fs::read_to_string(&path).unwrap();
    let kinds: Vec<String> = text
        .lines()
        .skip(1)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|ev| ev[1].as_str().map(String::from))
        .collect();

    assert_eq!(
        kinds,
        vec!["o", "o", "o", "o", "o"],
        "five frames and NO resize: a collapsed pane has no geometry worth \
         announcing, and must not make the pane after it look like a change. \
         Dropping either conjunct of the guard adds two `r` events, the width \
         half from the first fold and the height half from the second, \
         got {kinds:?}"
    );
}

#[test]
fn collapsed_panes_keep_one_column_and_the_rest_divide_what_is_left() {
    // The apportioning is the whole of the layout risk, so it is measured
    // against real solved widths rather than eyeballed off a constraint list.
    let solve = |flags: &[bool], width: u16| -> Vec<u16> {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints(terminal_pane_constraints(flags))
            .split(Rect {
                x: 0,
                y: 0,
                width,
                height: 10,
            })
            .iter()
            .map(|r| r.width)
            .collect()
    };

    // Nothing collapsed: the even split croft had before the feature existed.
    assert_eq!(solve(&[false; 4], 100), vec![25, 25, 25, 25]);

    // Two collapsed: one column each, and the two still open divide the 98
    // that leaves. This is the assertion `Ratio(1, k)` fails - it would ask
    // for 50 columns apiece out of the FULL 100 while the strips also want
    // theirs, and the solver settles the overflow by shaving the panes.
    let widths = solve(&[false, true, true, false], 100);
    assert_eq!(
        widths,
        vec![49, 1, 1, 49],
        "collapsed panes cost one column"
    );
    assert_eq!(widths.iter().sum::<u16>(), 100, "the row is fully spent");

    // The gain is the point: 25 columns each becomes 49.
    assert!(
        widths[0] > solve(&[false; 4], 100)[0],
        "collapsing must actually hand width to the panes still open"
    );
}

#[test]
fn clicking_a_collapsed_strip_gives_the_pane_its_width_back() {
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.toggle_terminal_collapse(2);
    term.draw(|f| app.render(f)).unwrap();

    let strip = app.terminal_strip_rects[2];
    assert_eq!(strip.width, 1, "a collapsed pane is one column wide");
    assert!(strip.height > 1, "the strip runs the height of the panel");
    // Anywhere down the strip, not only the chevron: one column is a small
    // enough target without also demanding the right row.
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        strip.x,
        strip.y + strip.height / 2,
    ));
    assert!(!app.terminals[2].collapsed, "the strip is the way back out");
}

#[test]
fn a_collapsed_pane_is_never_reflowed_into_its_strip() {
    // `resize` clamps to two columns, so rendering a collapsed pane would not
    // crash - it would quietly rewrap the running shell to a two-column grid
    // and destroy everything on its screen. Skipping the render is what keeps
    // the pane's geometry, and this is the assertion that says so.
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    let before = app.terminals[1].grid_cols();
    assert!(
        before > 2,
        "the pane must start at a real width to prove this"
    );

    app.toggle_terminal_collapse(1);
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(
        app.terminals[1].grid_cols(),
        before,
        "a collapsed pane's shell keeps the geometry it had"
    );
    assert_eq!(
        app.terminals[1].last_area,
        Rect::default(),
        "and its grid hit rect is cleared, so a strip click cannot land in it"
    );
}

#[test]
fn the_last_expanded_pane_refuses_to_collapse() {
    let (_tmp, mut app, _term) = app_with_terminal_panes(2);
    app.toggle_terminal_collapse(0);
    assert!(app.terminals[0].collapsed);

    app.toggle_terminal_collapse(1);
    assert!(
        !app.terminals[1].collapsed,
        "folding every pane away would leave a row of strips with nothing in it"
    );
    assert!(
        app.status.contains("last expanded"),
        "and it says so rather than swallowing the click: {:?}",
        app.status
    );
}

#[test]
fn closing_the_only_expanded_pane_never_leaves_a_row_of_strips() {
    // The refusal above guards one route into an all-collapsed row; closing
    // is the other, and it arrives from the opposite direction.
    let (_tmp, mut app, _term) = app_with_terminal_panes(2);
    app.toggle_terminal_collapse(0);
    assert!(app.close_terminal_at(1), "close the pane that was expanded");
    assert_eq!(app.terminals.len(), 1);
    assert!(
        !app.terminals[0].collapsed,
        "the survivor must be given its width back"
    );
}

#[test]
fn reopening_a_closed_pane_brings_it_back_expanded() {
    let (_tmp, mut app, _term) = app_with_terminal_panes(3);
    app.toggle_terminal_collapse(2);
    assert!(app.close_terminal_at(2));
    app.undo_close_terminal();
    // The count first. Without it a reopen that did nothing at all leaves two
    // uncollapsed panes and satisfies the flag check while proving the pane
    // never came back: the predicate is fine, the set it ranges over is wrong.
    assert_eq!(app.terminals.len(), 3, "the pane must actually be back");
    assert!(
        !app.terminals.iter().any(|t| t.collapsed),
        "a reopen that put a strip on screen would read as having failed"
    );
}

#[test]
fn cmd_k_brackets_fold_the_active_pane_and_restore_every_folded_one() {
    let (_tmp, mut app, _term) = app_with_terminal_panes(4);
    app.active_terminal = 3;
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('['), KeyModifiers::NONE)));
    app.active_terminal = 2;
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('['), KeyModifiers::NONE)));
    assert_eq!(
        app.terminals.iter().filter(|t| t.collapsed).count(),
        2,
        "each Cmd+K [ folds the pane that is active"
    );

    assert!(app.handle_cmd_k_chord(key(KeyCode::Char(']'), KeyModifiers::NONE)));
    assert!(
        !app.terminals.iter().any(|t| t.collapsed),
        "Cmd+K ] is the issue's one gesture back to equal-sized panes"
    );
    assert!(app.status.contains("Restored 2"), "{:?}", app.status);
}

#[test]
fn folding_the_focused_pane_moves_focus_somewhere_visible() {
    // A one-column strip shows no cursor, so keystrokes would land where the
    // user cannot see them arrive.
    let (_tmp, mut app, _term) = app_with_terminal_panes(3);
    app.active_terminal = 1;
    app.toggle_terminal_collapse(1);
    assert!(app.terminals[1].collapsed);
    // The SPECIFIC pane, not merely "not the folded one": naming it is what
    // tells a deliberate hand-off from focus landing somewhere by accident.
    // Pane 1's neighbours are equidistant, so this pins the left tie-break.
    assert_eq!(app.active_terminal, 0, "ties go to the pane on the left");
    assert!(
        !app.terminals[app.active_terminal].collapsed,
        "and that pane is actually on screen"
    );
}

#[test]
fn folding_a_pane_hands_focus_to_its_neighbour_not_to_the_front_of_the_row() {
    // The case above cannot tell "nearest" from "first": pane 1 of three is
    // one step from both its neighbours, so the two rules agree there. Four
    // panes with the LAST one folded separates them - nearest says 2, first
    // says 0 - which is the whole of the difference between handing focus to
    // a neighbour and throwing the user back across the panel.
    let (_tmp, mut app, _term) = app_with_terminal_panes(4);
    app.active_terminal = 3;
    app.toggle_terminal_collapse(3);
    assert!(app.terminals[3].collapsed);
    assert_eq!(
        app.active_terminal, 2,
        "focus goes to the neighbour, not to the first expanded pane"
    );
}

/// Closing the last EXPANDED pane leaves a panel of nothing but strips,
/// and something must bring one back (part of #468).
///
/// `toggle_terminal_collapse` refuses to fold the last expanded pane,
/// but closing reaches that state from the other side, which is a route
/// the fold guard cannot see. `ensure_a_terminal_pane_is_expanded` is
/// the answer and has exactly one caller; this pins that the caller is
/// on the path that needs it.
#[test]
fn closing_the_last_expanded_pane_leaves_one_expanded_behind() {
    let (_tmp, mut app, mut term) = app_with_terminal_panes(2);
    term.draw(|f| app.render(f)).unwrap();

    app.toggle_terminal_collapse(1);
    assert!(app.terminals[1].collapsed, "premise: pane 1 is folded");
    assert!(
        !app.terminals[0].collapsed,
        "premise: pane 0 is the only expanded one"
    );

    // Close the expanded one. The fold guard never fires -- nothing is
    // being folded -- so the panel would be all strips without the
    // close path's own guard.
    app.close_terminal_at(0);
    term.draw(|f| app.render(f)).unwrap();

    assert_eq!(app.terminals.len(), 1, "one pane left");
    assert!(
        !app.terminals[0].collapsed,
        "the surviving pane must be expanded: a panel of nothing but strips \
         has no cursor and no way back that does not depend on the strip \
         click still working"
    );
}

/// EXHAUSTIVE recovery sweep for #468: every pane index, both side bar
/// positions, and with a resize or a maximize cycle interleaved, must be
/// recoverable from its strip.
///
/// Four theories about this bug died against the tree by inspection --
/// the editor/panel splitter's grab zone, the maximize/collapse flag
/// interaction, index 0 being special, and the last-expanded guard. This
/// stops arguing and drives the combinations instead, because "I could
/// not construct the path" is a much weaker claim than "the paths were
/// enumerated and all of them recover".
///
/// If this ever goes red it names the reproduction directly.
#[test]
fn every_collapsed_pane_recovers_from_its_strip_in_every_layout() {
    for panes in [2usize, 3] {
        for right_bar in [false, true] {
            for perturb in ["none", "resize", "maximize"] {
                for idx in 0..panes {
                    let (_tmp, mut app, mut term) = app_with_terminal_panes(panes);
                    app.side_bar_position = if right_bar {
                        crate::app::SideBarPosition::Right
                    } else {
                        crate::app::SideBarPosition::Left
                    };
                    term.draw(|f| app.render(f)).unwrap();

                    app.toggle_terminal_collapse(idx);
                    assert!(
                        app.terminals[idx].collapsed,
                        "premise: pane {idx} folds ({panes} panes, right_bar={right_bar})"
                    );

                    // The perturbations #475 and #478 make plausible: a
                    // resize leaves stale geometry behind, and maximize
                    // bypasses the pane-constraint path entirely.
                    match perturb {
                        "resize" => {
                            term =
                                ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24))
                                    .unwrap();
                        }
                        "maximize" => {
                            app.toggle_terminal_pane_maximize();
                            term.draw(|f| app.render(f)).unwrap();
                            app.toggle_terminal_pane_maximize();
                        }
                        _ => {}
                    }
                    term.draw(|f| app.render(f)).unwrap();

                    // The perturbation must PRESERVE the fold. A `continue`
                    // here would let a maximize cycle that clears `collapsed`
                    // pass the case without ever clicking a strip, so the
                    // sweep would report paths it never walked -- the exact
                    // shape of green this test exists to refuse.
                    assert!(
                        app.terminals[idx].collapsed,
                        "pane {idx} must still be folded after {perturb} \
                         ({panes} panes, right_bar={right_bar}); if this \
                         perturbation legitimately unfolds panes the case \
                         needs its own assertion, not a skip"
                    );
                    let strip = app.terminal_strip_rects[idx];
                    assert_eq!(
                        strip.width, 1,
                        "pane {idx} keeps a one-column strip after {perturb} \
                         ({panes} panes, right_bar={right_bar})"
                    );

                    app.handle_mouse(mouse(
                        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                        strip.x,
                        strip.y,
                    ));
                    assert!(
                        !app.terminals[idx].collapsed,
                        "pane {idx} must unfold from its strip at ({}, {}) after \
                         {perturb} ({panes} panes, right_bar={right_bar})",
                        strip.x, strip.y
                    );
                }
            }
        }
    }
}

#[test]
fn a_collapsed_strip_carries_the_pane_name_down_its_column() {
    // The strip is what makes the gesture obviously reversible, so it has to
    // be identifiable: the chevron out, then the name.
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.terminals[1].set_manual_name(Some(String::from("build")));
    app.toggle_terminal_collapse(1);
    term.draw(|f| app.render(f)).unwrap();

    let strip = app.terminal_strip_rects[1];
    let column: String = (strip.y..strip.y + strip.height)
        .map(|y| {
            term.backend()
                .buffer()
                .cell(ratatui::layout::Position::new(strip.x, y))
                .unwrap()
                .symbol()
                .to_string()
        })
        .collect();
    assert!(
        column.starts_with('\u{eab6}'),
        "the expand chevron heads the strip: {column:?}"
    );
    assert!(
        column.contains("build"),
        "the pane's name runs down the strip: {column:?}"
    );
}

/// #468 reports the LEFTMOST pane specifically: "works fine for
/// right-side-opened terminals but the default terminal that is on the left
/// side, which fold also on the left side, cannot be recovered after
/// collapse."
///
/// Every other strip test in this file uses index 1 or 2, so index 0 has
/// never been exercised - the shape in which a bug survives a green suite.
/// Two theories (the splitter grab zone, the sidebar seam) already died
/// against the tree, so this asserts the user's gesture end to end rather
/// than any mechanism: fold pane 0, click its strip, and require it back.
#[test]
fn the_leftmost_pane_comes_back_from_its_strip() {
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.toggle_terminal_collapse(0);
    term.draw(|f| app.render(f)).unwrap();

    let strip = app.terminal_strip_rects[0];
    assert_eq!(strip.width, 1, "precondition: pane 0's strip is painted");
    assert!(app.terminals[0].collapsed, "precondition: pane 0 is folded");

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        strip.x,
        strip.y + 1,
    ));
    assert!(
        !app.terminals[0].collapsed,
        "a left-click on the leftmost strip unfolds it, the same as any other"
    );
}

/// The right-click half of the same gesture, for the same untested index.
#[test]
fn the_leftmost_panes_strip_offers_expand_terminal() {
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.toggle_terminal_collapse(0);
    term.draw(|f| app.render(f)).unwrap();
    let strip = app.terminal_strip_rects[0];
    assert_eq!(strip.width, 1, "precondition: pane 0's strip is painted");

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        strip.x,
        strip.y + 1,
    ));
    let menu = app
        .context_menu
        .as_ref()
        .expect("a strip right-click opens the pane menu");
    let labels: Vec<String> = menu
        .items
        .iter()
        .filter_map(|e| match e {
            MenuEntry::Item { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert!(
        labels.iter().any(|l| l == "Expand Terminal"),
        "the leftmost folded pane offers the way back out too: {labels:?}"
    );
}

#[test]
fn a_long_pane_name_never_paints_past_the_end_of_its_strip() {
    // A PROPERTY, not a case: this never states which row the name should end
    // on, only that nothing lands outside the strip. A case written to a
    // boundary already reasoned through would confirm the painting code's
    // arithmetic rather than probe it, since the same reasoning produced both.
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.toggle_terminal_collapse(1);
    term.draw(|f| app.render(f)).unwrap();
    let height = app.terminal_strip_rects[1].height;
    assert!(
        height > 2,
        "the strip must be tall enough for a name to overrun"
    );

    // Omega is one cell wide and appears nowhere else in the chrome, so any
    // sighting of it outside the strip is the name having escaped.
    const MARK: &str = "\u{3a9}";
    for len in 1..=(height as usize + 2) {
        app.terminals[1].set_manual_name(Some(MARK.repeat(len)));
        term.draw(|f| app.render(f)).unwrap();
        let strip = app.terminal_strip_rects[1];
        assert_eq!(strip.width, 1, "still one column at name length {len}");
        // Containment, counted over the WHOLE screen rather than probed at
        // the row below the strip: that probe reads `None` whenever the panel
        // reaches the last screen row, and an assertion the buffer's edge can
        // satisfy is not an assertion. Comparing totals cannot go vacuous, and
        // it states no arithmetic about which row is last.
        let buf = term.backend().buffer();
        let mark_at = |x: u16, y: u16| {
            buf.cell(ratatui::layout::Position::new(x, y))
                .is_some_and(|c| c.symbol() == MARK)
        };
        let area = buf.area();
        let on_screen = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| mark_at(x, y))
            .count();
        let in_strip = (strip.y..strip.y + strip.height)
            .filter(|&y| mark_at(strip.x, y))
            .count();
        assert!(
            in_strip > 0,
            "the name must actually be painted at length {len}, or nothing is being tested"
        );
        assert_eq!(
            on_screen, in_strip,
            "a {len}-character name painted outside its own strip"
        );
    }
}

// --- #313 fallback-review findings: red-first ------------------------------

#[test]
fn cycling_terminals_never_lands_on_a_collapsed_pane() {
    // Cmd+] / Cmd+[ walk `active_terminal` by raw modular arithmetic, which
    // predates the collapse flag. `toggle_terminal_collapse` documents the
    // invariant "focus must not sit on a pane the user cannot see" and
    // defends it on its own path only; cycling is the other way in.
    //
    // The FIXTURE has to be able to tell the two apart: four panes with index
    // 1 folded, cycling from 0. Broken lands on 1 (folded, no cursor painted);
    // correct skips to 2. A three-pane fixture would not discriminate.
    let (_tmp, mut app, _term) = app_with_terminal_panes(4);
    app.toggle_terminal_collapse(1);
    app.active_terminal = 0;
    app.cycle_terminal();
    assert_eq!(
        app.active_terminal, 2,
        "forward cycling steps over the strip"
    );
    assert!(!app.terminals[app.active_terminal].collapsed);

    app.cycle_terminal_back();
    assert_eq!(app.active_terminal, 0, "and so does backward cycling");
    assert!(!app.terminals[app.active_terminal].collapsed);

    // Presence half: with nothing folded the cycle still moves one slot, so
    // this cannot pass by the cycle being broken outright.
    app.restore_all_terminal_panes();
    app.active_terminal = 0;
    app.cycle_terminal();
    assert_eq!(app.active_terminal, 1, "an unfolded row still steps by one");
}

#[test]
fn collapse_is_inert_while_a_pane_is_maximized() {
    // The `‹` button is not painted while maximized and the menu entry is
    // suppressed, but the chord and the palette command called through
    // unconditionally. `toggle_terminal_collapse` reassigns `active_terminal`,
    // and the maximize branch hands the panel to whatever that is - so the
    // pane being watched was silently swapped for a neighbour.
    let (_tmp, mut app, _term) = app_with_terminal_panes(3);
    app.active_terminal = 1;
    app.toggle_terminal_pane_maximize();
    assert!(app.terminal_pane_maximized, "precondition");

    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('['), KeyModifiers::NONE)));
    assert_eq!(
        app.active_terminal, 1,
        "the maximized pane must not be swapped out from under the user"
    );
    assert!(
        !app.terminals.iter().any(|t| t.collapsed),
        "and nothing folds while maximize owns the panel"
    );
}

#[test]
fn a_reorder_never_hands_a_collapsed_pane_a_live_hit_rect() {
    // `move_terminal` re-seats `last_area` in SLOT order, because the rect is a
    // slot's geometry living on the terminal object. Before collapse existed
    // every visible slot had a real rect, so that was self-consistent. Now a
    // folded pane dragged into an expanded slot inherits a full-size rect it is
    // not painted in, and clicks over it resolve to a pane that is not there.
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.toggle_terminal_collapse(1);
    term.draw(|f| app.render(f)).unwrap();

    app.move_terminal(0, 2);
    let folded = app
        .terminals
        .iter()
        .position(|t| t.collapsed)
        .expect("the folded pane survived the reorder");
    assert_eq!(
        app.terminals[folded].last_area,
        Rect::default(),
        "a folded pane must not inherit an expanded slot's hit rect"
    );
    // Presence half: the panes that ARE painted keep real rects, so this
    // cannot pass by the re-seat having been removed altogether.
    assert!(
        app.terminals
            .iter()
            .filter(|t| !t.collapsed)
            .any(|t| t.last_area.width > 0),
        "expanded panes still carry their slot geometry"
    );
}

#[test]
fn closing_the_pane_beside_a_folded_one_never_leaves_focus_on_it() {
    // Round 1 fixed ONE route onto a folded pane (cycling) at the call site
    // rather than at `sync_focus_flags`, which documents itself as the single
    // place every gesture moving `active_terminal` funnels through.
    //
    // Here: fold the ACTIVE pane 1, which hands focus left to 0, then close 0.
    // `close_terminal_at` leaves `active_terminal` at 0, which is now the
    // folded pane. `ensure_a_terminal_pane_is_expanded` does not catch it - it
    // enforces "SOME pane is expanded", not "the ACTIVE pane is".
    let (_tmp, mut app, _term) = app_with_terminal_panes(3);
    app.active_terminal = 1;
    app.toggle_terminal_collapse(1);
    assert_eq!(app.active_terminal, 0, "precondition: focus went left");
    assert!(app.close_terminal_at(0), "close the pane holding focus");
    assert!(
        !app.terminals[app.active_terminal].collapsed,
        "closing a neighbour must not leave focus on a strip"
    );
}

#[test]
fn a_folded_strips_menu_offers_nothing_that_acts_on_the_active_pane() {
    // A strip right-click sets `active_terminal = idx` and focuses the pane,
    // and `sync_focus_flags` then walks focus straight off a folded pane to
    // its nearest expanded neighbour. Every menu entry that carries no index
    // acts on `active_terminal`, so the menu named one pane and acted on
    // another: "Open Scrollback in Editor" opened the NEIGHBOUR's scrollback
    // under the neighbour's label. The broadcast entry was worse — it read
    // its label from `terminals[idx]` and toggled `active_terminal`, so it
    // could say "Exclude from Broadcast" and include a different pane.
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.broadcast_input = true;
    app.toggle_terminal_collapse(1);
    term.draw(|f| app.render(f)).unwrap();
    let strip = app.terminal_strip_rects[1];
    assert_eq!(strip.width, 1, "precondition: the strip is painted");

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        strip.x,
        strip.y + 1,
    ));
    assert_ne!(
        app.active_terminal, 1,
        "precondition: focus cannot rest on a folded pane, which is what \
         makes an index-free entry act on the wrong one"
    );
    let labels = |app: &App| -> Vec<String> {
        app.context_menu
            .as_ref()
            .expect("a right-click opens the pane menu")
            .items
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect()
    };
    let folded = labels(&app);
    for entry in [
        "Copy",
        "Paste",
        "Quick Select",
        "Copy Mode",
        "Command History",
        "Open Scrollback in Editor",
        "Exclude from Broadcast",
        "Include in Broadcast",
        "Split Terminal",
    ] {
        assert!(
            !folded.iter().any(|l| l == entry),
            "{entry:?} acts on the active pane, which is not this one: {folded:?}"
        );
    }
    assert!(
        folded.iter().any(|l| l == "Rename Terminal")
            && folded.iter().any(|l| l == "Expand Terminal")
            && folded.iter().any(|l| l == "Kill Terminal"),
        "the entries that carry their own index stay: {folded:?}"
    );

    // The control, in the same run: an EXPANDED pane still offers all of
    // them, so this is a rule about folded panes and not a menu that lost
    // half its entries.
    app.context_menu = None;
    let area = app.terminals[0].last_area;
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        area.x + 1,
        area.y + 1,
    ));
    let expanded = labels(&app);
    for entry in [
        "Paste",
        "Split Terminal",
        "Quick Select",
        "Copy Mode",
        "Command History",
        "Open Scrollback in Editor",
    ] {
        assert!(
            expanded.iter().any(|l| l == entry),
            "{entry:?} belongs on an expanded pane's menu: {expanded:?}"
        );
    }
}

/// #540: the terminal pane menu carried every terminal gesture except the
/// two a right-click is most often reached for. Copy is offered only while
/// the pane has a selection - the menu model has no disabled state, so an
/// entry that would copy nothing is worse than no entry - and both entries
/// go through the same routes the Cmd+C / Cmd+V chords already use.
#[test]
fn the_pane_menu_copies_a_selection_and_pastes_the_clipboard() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A silent child: the grid then holds exactly what the test feeds it,
    // with no shell prompt racing the selection coordinates.
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("sleep 60")],
        tmp.path(),
    )
    .unwrap();
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let area = app.terminals[0].last_area;
    let inner = app.terminals[0].last_inner;
    let labels = |app: &App| -> Vec<String> {
        menu_labels(
            &app.context_menu
                .as_ref()
                .expect("a right-click opens the pane menu")
                .items,
        )
        .iter()
        .map(|l| l.to_string())
        .collect()
    };

    // Nothing selected: Paste is offered, Copy is not.
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        area.x + 1,
        area.y + 1,
    ));
    let empty = labels(&app);
    assert!(
        empty.iter().any(|l| l == "Paste"),
        "Paste needs no selection: {empty:?}"
    );
    assert!(
        !empty.iter().any(|l| l == "Copy"),
        "Copy must not be offered while there is nothing to copy: {empty:?}"
    );
    app.context_menu = None;

    // With a selection, Copy joins the menu and reaches the clipboard. The
    // row is located in the grid rather than assumed: the pane paints a
    // launch banner of its own above whatever the child prints.
    app.terminals[0].feed_bytes_for_test(b"copy-me-540\r\n");
    let (lines, top) = app.terminals[0].grid_lines();
    let vrow = (top
        + lines
            .iter()
            .position(|l| l.starts_with("copy-me-540"))
            .expect("the fed text must reach the grid") as i32) as u16;
    app.terminals[0].start_selection_at(inner.x, inner.y + vrow);
    app.terminals[0].extend_selection_to(inner.x + 20, inner.y + vrow);
    let expected = app.terminals[0].selection_text();
    assert!(
        expected.contains("copy-me-540"),
        "precondition: the selection covers the fed text, got {expected:?}"
    );
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        area.x + 1,
        area.y + 1,
    ));
    let selected = labels(&app);
    assert!(
        selected.iter().any(|l| l == "Copy"),
        "Copy belongs on a pane that has a selection: {selected:?}"
    );
    app.context_menu = None;
    app.dispatch_menu_action(MenuAction::TerminalCopySelection, tmp.path().to_path_buf());
    assert_eq!(
        crate::clipboard::read_string().as_deref(),
        Some(expected.as_str()),
        "the menu's Copy must put the selection on the clipboard"
    );

    // And Paste sends the clipboard on to the pane's child.
    let before = app.terminals[0].written_bytes_for_test().len();
    assert!(crate::clipboard::write_string("pasted-540"));
    app.dispatch_menu_action(MenuAction::TerminalPaste, tmp.path().to_path_buf());
    let written = app.terminals[0].written_bytes_for_test();
    let sent = String::from_utf8_lossy(&written[before..]).to_string();
    assert!(
        sent.contains("pasted-540"),
        "the menu's Paste must reach the shell, sent {sent:?}"
    );
}

#[test]
fn a_pane_named_in_wide_characters_still_marks_its_strip() {
    // Only single-width characters are painted, because a CJK glyph or an
    // emoji is two cells wide and the pane next door is one column away. But
    // FILTERING them out left a pane named entirely in them with a blank
    // strip: a folded pane that is not identifiable is the one thing the
    // strip exists to prevent.
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.terminals[1].set_manual_name(Some(String::from("\u{9805}\u{76ee}")));
    app.toggle_terminal_collapse(1);
    term.draw(|f| app.render(f)).unwrap();
    let strip = app.terminal_strip_rects[1];
    assert_eq!(strip.width, 1, "precondition: the strip is painted");
    assert!(strip.height > 2, "precondition: room below the chevron");

    let buf = term.backend().buffer();
    let at = |x: u16, y: u16| {
        buf.cell(ratatui::layout::Position::new(x, y))
            .map(|c| c.symbol().to_string())
            .unwrap_or_default()
    };
    let marked = (strip.y + 1..strip.y + strip.height)
        .filter(|&y| !at(strip.x, y).trim().is_empty())
        .count();
    assert!(
        marked > 0,
        "the strip must carry something under the chevron for a wide-character name"
    );
    // And the containment property the filter was there for: the stand-in is
    // one cell, so the pane one column away is untouched.
    assert_eq!(
        unicode_width::UnicodeWidthChar::width(TERMINAL_STRIP_WIDE_STANDIN),
        Some(1),
        "the stand-in has to fit the one column the strip owns"
    );
}

#[test]
fn a_new_profile_prompt_refuses_an_unsafe_name_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.prompt = Some(Prompt {
        label: String::from("New profile name"),
        buffer: String::from("../escape"),
        kind: PromptKind::NewProfile,
        target_dir: tmp.path().to_path_buf(),
        error: None,
    });
    app.commit_prompt();
    let p = app.prompt.as_ref().expect("the prompt stays open");
    assert!(
        p.error
            .as_deref()
            .is_some_and(|e| e.contains("profile name")),
        "{:?}",
        p.error
    );
}

/// #1598: GNOME Terminal and Konsole keep Ctrl+Shift+T / W for their own New
/// Tab and Close Tab, so the pane menu carries Split Terminal and Kill
/// Terminal, as VS Code's does. Kill closes the pane that was right-clicked,
/// not whichever one had focus.
#[test]
fn the_pane_menu_splits_and_kills_terminals() {
    let (_tmp, mut app, mut term) = app_with_terminal_panes(2);
    let right_click = |app: &mut App, term: &mut ratatui::Terminal<_>, idx: usize| {
        term.draw(|f| app.render(f)).unwrap();
        app.context_menu = None;
        let area = app.terminals[idx].last_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            area.x + 1,
            area.y + 1,
        ));
        app.context_menu
            .as_ref()
            .expect("a right-click opens the pane menu")
            .items
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item { label, action } => Some((label.clone(), action.clone())),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let pick = |items: &[(String, MenuAction)], label: &str| {
        items
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, a)| a.clone())
            .unwrap_or_else(|| panic!("no {label:?} in {items:?}"))
    };
    let tmp_root = app.tree.root.clone();

    let items = right_click(&mut app, &mut term, 1);
    assert_eq!(shortcut_for(&pick(&items, "Split Terminal")), Some("⌘T"));
    assert_eq!(shortcut_for(&pick(&items, "Kill Terminal")), Some("⌘W"));
    app.dispatch_menu_action(pick(&items, "Split Terminal"), tmp_root.clone());
    assert_eq!(app.terminals.len(), 3, "{}", app.status);
    assert!(
        app.status.contains("Split terminal: 3 active"),
        "{}",
        app.status
    );

    // Kill acts on the pane right-clicked even when another has focus.
    let doomed = app.terminals[0].uid();
    let items = right_click(&mut app, &mut term, 0);
    app.active_terminal = 2;
    app.dispatch_menu_action(pick(&items, "Kill Terminal"), tmp_root.clone());
    assert_eq!(app.terminals.len(), 2);
    assert!(
        app.terminals.iter().all(|t| t.uid() != doomed),
        "the right-clicked pane is the one that closed"
    );
    assert!(
        app.status.contains("Closed terminal: 2 remaining"),
        "{}",
        app.status
    );
}

/// #1598 negative: a lone pane cannot be closed (`Cmd+W` refuses the last
/// terminal), so its menu offers no Kill Terminal that would do nothing;
/// Split is still there.
#[test]
fn a_lone_terminals_menu_offers_split_but_no_kill() {
    let (_tmp, mut app, mut term) = app_with_terminal_panes(1);
    term.draw(|f| app.render(f)).unwrap();
    let area = app.terminals[0].last_area;
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        area.x + 1,
        area.y + 1,
    ));
    let labels = menu_labels(&app.context_menu.as_ref().expect("the pane menu").items);
    assert!(labels.contains(&"Split Terminal"), "{labels:?}");
    assert!(!labels.contains(&"Kill Terminal"), "{labels:?}");
}

/// #1598: LINUX.md's way to copy, close and split terminal panes without
/// `Super` must work in GNOME Terminal and Konsole, which keep
/// `Ctrl`+`Shift`+`C` / `W` / `T` for themselves: each row names the pane
/// menu entry, and the Ctrl+Shift chords are no longer the whole answer.
#[test]
fn linux_terminal_pane_fallbacks_name_the_pane_menu() {
    let doc = include_str!("../../../docs/LINUX.md");
    let row = |first: &str| -> &str {
        doc.lines()
            .find(|l| l.starts_with(&format!("| {first} |")))
            .unwrap_or_else(|| panic!("no LINUX.md row {first:?}"))
            .trim_matches('|')
            .split(" | ")
            .nth(2)
            .unwrap()
            .trim()
    };
    let copy_close = row(
        "`Cmd`+`C` / `Cmd`+`W` copy the selection / close the active terminal, in the terminal pane",
    );
    let split = row("`Cmd`+`T` split terminal");
    for (cell, entries) in [
        (copy_close, &["Copy", "Kill Terminal"][..]),
        (split, &["Split Terminal"][..]),
    ] {
        assert!(cell.contains("right-click menu"), "{cell}");
        for entry in entries {
            assert!(cell.contains(entry), "{entry} missing from {cell}");
        }
        assert!(
            !cell.starts_with("`Ctrl`+`Shift`"),
            "the host terminal's own chord is not the fallback: {cell}"
        );
    }
    assert!(
        doc.contains("GNOME Terminal and Konsole keep `Ctrl`+`Shift`+`C`, `W` and `T`"),
        "LINUX.md says why the Ctrl+Shift chords do not reach croft there"
    );
}
