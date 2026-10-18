//! Diff views, the merge editor and disk-conflict prompts.

use super::*;

/// A `diff://` link opens two files but is ONE jump for the user, so Back
/// must return to where the click happened — not to the left half of the
/// diff on the way there.
#[test]
fn side_by_side_diff_link_records_one_go_back_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin.rs");
    let left = tmp.path().join("left.rs");
    let right = tmp.path().join("right.rs");
    std::fs::write(&origin, "fn main() {}\nlet x = 1;\n").unwrap();
    std::fs::write(&left, "fn left() {}\n").unwrap();
    std::fs::write(&right, "fn right() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&origin).unwrap();
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 4;

    let lref = crate::file_ref::FileRef {
        path: left.to_string_lossy().into_owned(),
        line: 1,
        column: None,
        end_line: None,
    };
    let rref = crate::file_ref::FileRef {
        path: right.to_string_lossy().into_owned(),
        line: 1,
        column: None,
        end_line: None,
    };
    assert!(app.open_side_by_side(&lref, &rref), "both sides open");

    app.nav_back();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(origin.as_path()),
        "one Back returns to the clicked location, not the left half"
    );
    assert_eq!(app.editor.cursor_row, 1, "caret returns to the click row");
    assert_eq!(
        app.editor.cursor_col, 4,
        "caret returns to the click column"
    );
}

// Cross-platform: asserts via `clipboard::read_string()`, so the per-thread
// test mock covers the diff-selection → Cmd+C → clipboard wiring on every OS.
// The macOS NSPasteboard FFI is verified separately by the cfg(macos) tests in
// `clipboard.rs` and `cmd_c_on_terminal_selection_lands_text_on_macos_clipboard`.
#[test]
fn cmd_c_on_side_by_side_diff_selection_copies_selected_text() {
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
fn diff_word_selection_highlights_every_other_occurrence() {
    // Highlight-all parity with the plain editor: selecting a word in the
    // diff must paint a muted-blue box over EVERY other occurrence of that
    // word on screen (VS Code's `editor.selectionHighlight`), exactly like
    // the regular editor does. The diff renderer used to only paint the
    // active selection band, leaving the other occurrences un-highlighted.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    // "alpha" appears on every equal row's left AND right column, so a single
    // word selection has many other occurrences visible at once.
    std::fs::write(&f1, "alpha one\nalpha two\nalpha three\n").unwrap();
    std::fs::write(&f2, "alpha one\nalpha two\nalpha three\nalpha four\n").unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    app.focus = Pane::Editor;
    let _ = render_buf(&mut app);

    // Select the word "alpha" (cols 0..5) on the left column of the first row.
    {
        let diff = app.editor.diff.as_mut().unwrap();
        diff.select_word_at(crate::widgets::diff::DiffSide::Left, 0, 0);
        assert_eq!(
            diff.selection_text().as_deref(),
            Some("alpha"),
            "fixture: word click must select exactly 'alpha'"
        );
    }

    let buf = render_buf(&mut app);
    let occurrence = Color::Rgb(0x37, 0x61, 0x8e);
    let occ_cells = count_bg(&buf, occurrence);
    // The selected word itself is overpainted by the brighter selection band,
    // so every occurrence we count here is a *different* one. Two equal rows
    // still on screen carry "alpha" on both columns -> at least four more
    // 5-char spans = 20 cells, well over this floor.
    assert!(
        occ_cells >= 10,
        "selecting 'alpha' in the diff must highlight all other occurrences in \
         the muted-blue selection-highlight colour, mirroring the plain editor; \
         got only {occ_cells} highlighted cells"
    );
}

#[test]
fn double_click_in_diff_selects_word_and_cmd_c_copies_it() {
    // End-to-end: two left-clicks in quick succession on a word in
    // the left column must snap a word-bounded selection (same UX
    // as VS Code / standard text editors), and the very next Cmd+C
    // keystroke must put exactly that word on the clipboard.
    // Cross-platform: asserts via `clipboard::read_string()`, so the
    // per-thread test mock exercises this on every OS (no real-clipboard
    // opt-in needed — the OS pasteboard path is covered elsewhere).
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
fn cmd_k_then_s_selects_active_file_for_compare() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "v1").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.path = Some(f.clone());
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.cmd_k_leader.is_none(),
        "the chord consumes the armed leader"
    );
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
fn merge_picker_accept_current_replaces_the_block_with_ours() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", CONFLICTED_BODY);
    app.editor.cursor_row = 4; // inside the theirs section, still the same block
    app.start_code_action();
    assert!(app.list_picker.is_some(), "the merge picker must open");
    app.confirm_list_picker(); // first row = Accept Current
    assert_eq!(
        app.editor.lines,
        vec!["fn main() {", "    ours();", "}"],
        "the whole marker block collapses to the current section"
    );
    assert!(app.editor.dirty, "resolving a conflict dirties the buffer");
    assert!(app.editor.undo(), "resolution is one undo step");
    assert_eq!(app.editor.lines.len(), 7, "undo restores the marker block");
}

#[test]
fn merge_commands_outside_a_conflict_report_and_leave_the_buffer_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", CONFLICTED_BODY);
    app.editor.cursor_row = 0; // outside the block
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptCurrent);
    assert_eq!(app.status, "No merge conflict at the cursor");
    assert_eq!(app.editor.lines.len(), 7, "the buffer is untouched");
}

#[test]
fn conflict_regions_render_with_tinted_backgrounds() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", CONFLICTED_BODY);
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let x = app.editor.last_inner.x + app.editor.last_gutter_width as u16 + 1;
    let y0 = app.editor.last_inner.y; // row 0: "fn main() {"
    let buf = term.backend().buffer();
    let bg_at = |dy: u16| buf[(x, y0 + dy)].style().bg;
    assert_ne!(
        bg_at(2),
        bg_at(0),
        "the ours content row must be tinted differently from a plain row"
    );
    assert_ne!(
        bg_at(4),
        bg_at(0),
        "the theirs content row must be tinted differently from a plain row"
    );
    assert_ne!(
        bg_at(2),
        bg_at(4),
        "ours and theirs tint differently, like VS Code's current vs incoming"
    );
}

#[test]
fn merge_accept_refuses_a_non_text_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let png = tmp.path().join("p.png");
    image::RgbaImage::new(1, 1).save(&png).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&png).unwrap();
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptCurrent);
    assert_eq!(
        app.status, "Merge conflict actions need a text file",
        "a non-text tab must refuse, never consult a possibly-stale conflict cache"
    );
}

#[test]
fn f7_cycles_conflicts_in_a_plain_editor_tab() {
    let tmp = tempfile::tempdir().unwrap();
    conflicted_repo(tmp.path());
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .open_pinned(&tmp.path().join("shared.txt"))
        .unwrap();
    app.focus_pane(Pane::Editor);
    let headers: Vec<usize> = app
        .editor
        .conflicts()
        .iter()
        .map(|b| b.ours_start)
        .collect();
    assert_eq!(headers.len(), 2, "precondition: two conflict blocks");
    app.editor.cursor_row = 0;
    let f7 = |app: &mut App, shift: bool| {
        let mut key = crossterm::event::KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE);
        if shift {
            key.modifiers = KeyModifiers::SHIFT;
        }
        app.handle_key(key).unwrap();
    };
    f7(&mut app, false);
    assert_eq!(
        app.editor.cursor_row, headers[0],
        "first F7 reaches conflict 1"
    );
    assert!(
        app.status.contains("Conflict 1 of 2"),
        "status names the position; was {:?}",
        app.status
    );
    f7(&mut app, false);
    assert_eq!(
        app.editor.cursor_row, headers[1],
        "second F7 reaches conflict 2"
    );
    f7(&mut app, false);
    assert_eq!(app.editor.cursor_row, headers[0], "F7 wraps to conflict 1");
    f7(&mut app, true);
    assert_eq!(
        app.editor.cursor_row, headers[1],
        "Shift+F7 walks backwards with wrap"
    );
}

#[test]
fn taking_the_incoming_deletion_removes_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = open_modify_delete(tmp.path(), true);
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptAllIncoming);
    app.complete_merge();
    assert!(!tmp.path().join("gone.txt").exists(), "{}", app.status);
    assert_eq!(git_out(tmp.path(), &["ls-files", "gone.txt"]), "");
    assert_eq!(
        git_out(tmp.path(), &["status", "--porcelain", "gone.txt"]),
        "D  gone.txt\n"
    );
    assert!(app.status.contains("removed gone.txt"), "{}", app.status);
    assert!(
        app.editor
            .find_any_tab_with_path(&tmp.path().join("gone.txt"))
            .is_none(),
        "the tab for the removed file closes"
    );
}

#[test]
fn taking_the_current_deletion_removes_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = open_modify_delete(tmp.path(), false);
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptAllCurrent);
    app.complete_merge();
    assert!(!tmp.path().join("gone.txt").exists(), "{}", app.status);
    assert_eq!(git_out(tmp.path(), &["ls-files", "gone.txt"]), "");
}

#[test]
fn merge_editor_accepts_toggles_and_manual_edits_update_the_counter() {
    let tmp = tempfile::tempdir().unwrap();
    conflicted_repo(tmp.path());
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let idx = wait_for_conflicted_entry(&mut app);
    app.open_source_control_entry(idx);
    assert!(app.editor.merge.is_some());

    // Accept Current on conflict 1 (cursor already there).
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptCurrent);
    assert_eq!(app.editor.lines[1], "main-one");
    // F7 lands on conflict 2; Accept Combination (Incoming First).
    app.merge_jump(false);
    assert!(app.status.contains("Conflict 2 of 2"), "{:?}", app.status);
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptBothReverse);
    assert_eq!(
        app.editor.lines.join("\n"),
        "alpha\nmain-one\nm1\nm2\nm3\nm4\nm5\nfeature-two\nmain-two\nomega",
        "incoming-first combination splices theirs then ours"
    );
    assert_eq!(app.editor.merge.as_ref().unwrap().resolved_count(), 2);

    // Checkbox toggling: unchecking the only accepted side reverts the
    // region to base and the conflict back to unresolved.
    use crate::merge_editor::{CheckSide, ConflictState};
    app.merge_toggle_side(0, CheckSide::Incoming); // Current -> Both
    assert_eq!(
        app.editor.merge.as_ref().unwrap().conflicts[0].state,
        ConflictState::Both
    );
    assert_eq!(app.editor.lines[1], "main-one");
    assert_eq!(app.editor.lines[2], "feature-one");
    app.merge_toggle_side(0, CheckSide::Current); // Both -> Incoming
    app.merge_toggle_side(0, CheckSide::Incoming); // Incoming -> Unresolved
    let mv = app.editor.merge.as_ref().unwrap();
    assert_eq!(mv.conflicts[0].state, ConflictState::Unresolved);
    assert_eq!(app.editor.lines[1], "base-one", "back to base text");
    assert_eq!(mv.resolved_count(), 1);

    // A manual edit inside the region marks it manually resolved.
    let row = app.editor.merge.as_ref().unwrap().conflicts[0].result_start;
    app.editor.cursor_row = row;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app.merge_view_sync();
    let mv = app.editor.merge.as_ref().unwrap();
    assert_eq!(mv.conflicts[0].state, ConflictState::Manual);
    assert_eq!(mv.resolved_count(), 2, "manual edits count as resolved");

    // Base pane toggle.
    assert!(!app.editor.merge.as_ref().unwrap().show_base);
    app.run_command(crate::widgets::command_palette::Command::MergeToggleBase);
    assert!(app.editor.merge.as_ref().unwrap().show_base);
    assert_eq!(app.status, "Base pane shown");
}

#[test]
fn merge_editor_works_on_a_marker_only_file_outside_any_merge() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("plain.txt"),
        "top\n<<<<<<< HEAD\nmine\n=======\nyours\n>>>>>>> other\nbottom\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .open_pinned(&tmp.path().join("plain.txt"))
        .unwrap();
    app.focus_pane(Pane::Editor);
    app.run_command(crate::widgets::command_palette::Command::MergeOpenEditor);
    let mv = app.editor.merge.as_ref().expect("marker synthesis worked");
    assert!(mv.from_markers);
    assert_eq!(mv.conflicts.len(), 1);
    assert_eq!(mv.conflicts[0].ours, vec!["mine".to_string()]);
    assert_eq!(mv.conflicts[0].theirs, vec!["yours".to_string()]);
    assert!(
        !app.editor.lines.iter().any(|l| l.starts_with("<<<<<<<")),
        "the Result holds no markers"
    );
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptBoth);
    assert_eq!(
        app.editor.lines.join("\n"),
        "top\nmine\nyours\nbottom",
        "Accept Both keeps ours then theirs"
    );
    // Undo steps back through the accept and the initial transform to
    // the marker text.
    app.editor.undo();
    app.editor.undo();
    assert!(
        app.editor.lines.iter().any(|l| l.starts_with("<<<<<<<")),
        "the marker buffer is one undo chain away"
    );
}

#[test]
fn merge_editor_renders_source_panes_and_result_at_100_columns() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("plain.txt"),
        "top\n<<<<<<< HEAD\nmine\n=======\nyours\n>>>>>>> other\nbottom\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .open_pinned(&tmp.path().join("plain.txt"))
        .unwrap();
    app.focus_pane(Pane::Editor);
    app.run_command(crate::widgets::command_palette::Command::MergeOpenEditor);
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer().clone();
    let a = buf.area;
    let text: String = (a.y..a.y + a.height)
        .map(|y| {
            (a.x..a.x + a.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("MERGE"), "header row painted");
    assert!(text.contains("0/1 conflict resolved"), "counter painted");
    assert!(text.contains("CURRENT (yours)"), "current pane titled");
    assert!(text.contains("INCOMING (theirs)"), "incoming pane titled");
    assert!(
        text.contains("RESULT (editable)"),
        "result separator painted"
    );
    assert!(
        text.contains("[ ]"),
        "the unresolved conflict shows unchecked boxes"
    );
    let spans = app.editor.merge.as_ref().unwrap().check_spans.clone();
    assert_eq!(spans.len(), 2, "one checkbox per source pane");
    // Clicking the Current checkbox accepts that side.
    let (y, xs, idx, _) = &spans[0];
    let m = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: xs.start,
        row: *y,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(m);
    let mv = app.editor.merge.as_ref().unwrap();
    assert_eq!(
        mv.conflicts[*idx].state,
        crate::merge_editor::ConflictState::Current,
        "the checkbox click accepted Current"
    );
    assert_eq!(app.editor.lines[1], "mine");
}

/// #1169: the merge editor's side panes carry labelled accept buttons
/// above each conflict, not only the `[ ]` checkboxes, and a click on one
/// resolves the conflict that way and ticks the button.
#[test]
fn merge_editor_accept_buttons_resolve_the_conflict_above_which_they_sit() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("plain.txt"),
        "top\n<<<<<<< HEAD\nmine\n=======\nyours\n>>>>>>> other\nbottom\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .open_pinned(&tmp.path().join("plain.txt"))
        .unwrap();
    app.focus_pane(Pane::Editor);
    app.run_command(crate::widgets::command_palette::Command::MergeOpenEditor);
    let backend = ratatui::backend::TestBackend::new(160, 60);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    let screen = |term: &ratatui::Terminal<ratatui::backend::TestBackend>| {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    term.draw(|f| app.render(f)).unwrap();
    let text = screen(&term);
    for label in [" Accept Current ", " Accept Incoming ", " Accept Both "] {
        assert!(text.contains(label), "{label:?} must be painted:\n{text}");
    }
    assert!(
        text.contains("[ ]"),
        "the checkboxes stay beside the buttons"
    );
    let mv = app.editor.merge.as_ref().unwrap();
    let (y, xs, idx, _) = mv
        .action_spans
        .iter()
        .find(|(_, _, _, s)| *s == crate::merge_editor::ConflictState::Incoming)
        .cloned()
        .expect("the incoming pane records its Accept Incoming button");
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: xs.start,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
    let mv = app.editor.merge.as_ref().unwrap();
    assert_eq!(
        mv.conflicts[idx].state,
        crate::merge_editor::ConflictState::Incoming
    );
    assert_eq!(app.editor.lines[1], "yours", "the Result took incoming");
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        screen(&term).contains("\u{2713} Accept Incoming"),
        "the button for the state the conflict is in is ticked"
    );
}

#[test]
fn clicking_an_accept_action_on_the_header_row_resolves_that_block() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    conflicted_repo(tmp.path());
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .open_pinned(&tmp.path().join("shared.txt"))
        .unwrap();
    app.focus_pane(Pane::Editor);
    let backend = ratatui::backend::TestBackend::new(160, 44);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer().clone();
    let a = buf.area;
    let text: String = (a.y..a.y + a.height)
        .map(|y| {
            (a.x..a.x + a.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains(" Accept Current ") && text.contains(" Accept Incoming "),
        "the header row must carry the accept buttons"
    );
    let span = app
        .editor
        .merge_action_spans
        .iter()
        .find(|(_, _, _, res)| *res == crate::merge::Resolution::Incoming)
        .cloned()
        .expect("an incoming action span must be recorded");
    let before = app.editor.conflicts().len();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        span.1.start,
        span.0,
    ));
    assert_eq!(
        app.editor.conflicts().len(),
        before - 1,
        "the clicked block must resolve; status: {:?}",
        app.status
    );
    assert!(
        app.editor.lines.contains(&String::from("feature-one")),
        "the incoming side must be kept"
    );
    assert!(
        app.status.contains("Accepted incoming change"),
        "status must confirm; was {:?}",
        app.status
    );
}

/// Opening a file whose diff is open (Quick Open, an Explorer click, a
/// problem row) opens the file beside the diff instead of landing on it.
/// The left side of an Explorer compare is the case that used to strand.
#[test]
fn opening_a_file_whose_diff_is_open_opens_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let a = tmp.path().join("a.yaml");
    let b = tmp.path().join("b.yaml");
    std::fs::write(&a, "k: 1\n").unwrap();
    std::fs::write(&b, "k: 2\n").unwrap();
    app.editor.open_diff(&a, &b).unwrap();
    let diff_tab = app.editor.active_index();

    app.editor.open_preview(&a).unwrap();

    assert!(app.editor.diff.is_none(), "landed on the diff");
    assert_eq!(app.editor.path.as_deref(), Some(a.as_path()));
    assert_eq!(app.editor.lines[0], "k: 1");
    assert_eq!(app.editor.editors.len(), 2, "the diff stays open beside it");
    assert!(app.editor.editors[diff_tab].diff.is_some());
    // Opening it again reuses the file's tab, not a third one.
    app.editor.open_preview(&a).unwrap();
    assert_eq!(app.editor.editors.len(), 2);
}

#[test]
fn an_open_head_diff_follows_the_working_file_and_head() {
    // The Source Control diff used to be a snapshot: an agent rewriting the
    // file, or a save from a sibling tab, left the rows stale until the tab
    // was reopened (#471). Now the view is rebuilt in place -- from disk on
    // a filesystem trigger, from HEAD too on a git-status trigger -- with
    // the reader's viewport kept.
    let (tmp, f) = repo_with_seed("one\ntwo\n");
    let root = tmp.path().to_path_buf();
    let mut app = App::new(root.clone()).unwrap();
    // Open exactly what open_source_control_entry opens for a Modified row.
    std::fs::write(&f, "one\ntwo\nthree\n").unwrap();
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
    assert_eq!(app.editor.diff.as_ref().unwrap().right_lines.len(), 3);
    assert!(
        !app.refresh_open_diff_views(false, &[], &no_paths()),
        "nothing moved yet: the refresh is a no-op"
    );

    // The working file grows under the open view.
    rewrite_later(&f, "one\ntwo\nthree\nfour\nfive\n");
    app.editor.diff.as_mut().unwrap().scroll = 1;
    assert!(
        app.refresh_open_diff_views(false, &[], &no_paths()),
        "a filesystem trigger rebuilds the view from the changed file"
    );
    let d = app.editor.diff.as_ref().unwrap();
    assert_eq!(d.right_lines, vec!["one", "two", "three", "four", "five"]);
    assert_eq!(d.left_lines, vec!["one", "two"], "HEAD side untouched");
    assert_eq!(
        d.scroll, 1,
        "the reader's viewport is kept, not reset to the first hunk"
    );
    assert!(
        d.left_is_git_head,
        "the rebuilt view still gates stage/revert like the original"
    );
    assert!(
        matches!(
            d.source,
            crate::widgets::diff::DiffSource::HeadVsWorking { .. }
        ),
        "the source survives the rebuild"
    );
    assert!(
        !app.refresh_open_diff_views(false, &[], &no_paths()),
        "the stamps were taken: a second tick does not rebuild again"
    );

    // A commit moves HEAD: the git-status trigger re-reads the left side.
    git_ok(&root, &["add", "."]);
    git_ok(&root, &["commit", "-m", "grow", "--quiet"]);
    assert!(
        !app.refresh_open_diff_views(false, &[], &no_paths()),
        "a filesystem-only trigger does not re-read HEAD"
    );
    assert!(
        !app.refresh_open_diff_views(true, &[], &no_paths()),
        "nor does a git-status drain in which THIS root's HEAD did not move"
    );
    assert!(
        app.refresh_open_diff_views(true, std::slice::from_ref(&root), &no_paths()),
        "the drain that saw this root's HEAD move does"
    );
    let d = app.editor.diff.as_ref().unwrap();
    assert_eq!(
        d.left_lines, d.right_lines,
        "after the commit both sides agree"
    );
}

#[test]
fn a_staged_diff_view_reruns_git_on_a_status_trigger_only() {
    let (tmp, f) = repo_with_seed("alpha\n");
    let root = tmp.path().to_path_buf();
    let mut app = App::new(root.clone()).unwrap();
    let raw = crate::git::diff_staged(&root).unwrap();
    app.editor
        .open_git_diff_side_by_side(std::path::Path::new("git diff --staged"), &raw)
        .unwrap();
    app.tag_open_diff(crate::widgets::diff::DiffSource::GitCommand {
        root: root.clone(),
        kind: crate::widgets::diff::GitDiffKind::Staged,
    });
    assert!(
        app.editor
            .diff
            .as_ref()
            .unwrap()
            .right_lines
            .contains(&String::from("(no changes)")),
        "nothing is staged yet"
    );

    std::fs::write(&f, "alpha\nbeta\n").unwrap();
    git_ok(&root, &["add", "seed.txt"]);
    assert!(
        !app.refresh_open_diff_views(false, &[], &no_paths()),
        "a git-command view is not re-run on a filesystem tick: the index is not a file it watches"
    );
    assert!(
        app.refresh_open_diff_views(true, &[], &no_paths()),
        "the git-status drain re-runs it"
    );
    let d = app.editor.diff.as_ref().unwrap();
    assert!(
        d.right_lines.iter().any(|l| l == "beta"),
        "the staged line now shows: {:?}",
        d.right_lines
    );
    assert!(
        !app.refresh_open_diff_views(true, &[], &no_paths()),
        "identical output changes nothing"
    );
}

#[test]
fn a_snapshot_and_a_two_file_diff_follow_their_files() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "x\n").unwrap();
    std::fs::write(&b, "x\ny\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Timeline snapshot: fixed left text against the working file.
    app.editor
        .open_head_diff_with_text(
            std::path::PathBuf::from("b.txt (local snapshot)"),
            "x\n",
            &b,
            false,
        )
        .unwrap();
    app.tag_open_diff(crate::widgets::diff::DiffSource::FixedLeft {
        left_text: String::from("x\n"),
    });
    rewrite_later(&b, "x\ny\nz\n");
    assert!(app.refresh_open_diff_views(false, &[], &no_paths()));
    let d = app.editor.diff.as_ref().unwrap();
    assert_eq!(d.right_lines, vec!["x", "y", "z"]);
    assert_eq!(d.left_lines, vec!["x"], "the snapshot side never changes");
    assert!(!d.left_is_git_head, "a snapshot view still cannot stage");

    // Two real files: either side moving rebuilds.
    app.editor.open_diff(&a, &b).unwrap();
    rewrite_later(&a, "x\nw\n");
    assert!(app.refresh_open_diff_views(false, &[], &no_paths()));
    let d = app.editor.diff.as_ref().unwrap();
    assert_eq!(d.left_lines, vec!["x", "w"]);
    assert!(
        d.left_is_real_file,
        "Enter on a Removed row still opens the left file"
    );
    assert!(!app.refresh_open_diff_views(false, &[], &no_paths()));
}

#[test]
fn a_diff_view_in_an_inactive_split_group_is_refreshed_too() {
    // Every group is swept, not just the focused one: the diff the user
    // parked in the other split is exactly the one they are watching while
    // they type in this one.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "1\n").unwrap();
    std::fs::write(&b, "1\n2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_diff(&a, &b).unwrap();
    app.split_editor();
    app.editor.open_pinned(&a).unwrap();
    assert!(
        app.editor.diff.is_none(),
        "the focused group shows a plain buffer"
    );

    rewrite_later(&b, "1\n2\n3\n");
    assert!(app.refresh_open_diff_views(false, &[], &no_paths()));
    let stale = app
        .editor_layout
        .inactive_groups_mut()
        .into_iter()
        .flat_map(|g| g.editors.iter())
        .filter_map(|e| e.diff.as_ref())
        .any(|d| d.right_lines.len() != 3);
    assert!(!stale, "the inactive group's diff shows the third line");
}

#[test]
fn a_head_diff_tagged_with_the_repo_toplevel_refreshes_from_a_subdirectory_workspace() {
    // The Source Control opener tags a view with `scm_root()`, the git
    // TOPLEVEL. From a subdirectory workspace that is not the workspace
    // root the drain keys its HEAD-oid bookkeeping on, so `heads_moved`
    // has to record the toplevel (and compare canonically), or a commit
    // never re-reads the HEAD side. This drives the real drain.
    let (tmp, f) = repo_with_seed("one\n");
    let root = tmp.path().to_path_buf();
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let mut app = App::new(sub.clone()).unwrap();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::RESTORED_SHELL_BASE,
        "the git worker's first status",
        || {
            app.try_install_pending_init();
            app.git.status().in_repo
        },
    );
    let scm = app.scm_root();
    assert_ne!(
        scm, sub,
        "precondition: the toplevel is not the workspace root"
    );

    std::fs::write(&f, "one\ntwo\n").unwrap();
    app.editor
        .open_head_diff_with_text(
            std::path::PathBuf::from("seed.txt (HEAD)"),
            "one\n",
            &f,
            true,
        )
        .unwrap();
    app.tag_open_diff(crate::widgets::diff::DiffSource::HeadVsWorking {
        root: scm.clone(),
        rel: String::from("seed.txt"),
    });
    git_ok(&root, &["add", "."]);
    git_ok(&root, &["commit", "-m", "two", "--quiet"]);
    app.active_git_bypass_debounce();
    app.refresh_git_status_debounced();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::RESTORED_SHELL_BASE,
        "the HEAD side to follow the commit through the status drain",
        || {
            app.try_install_pending_init();
            app.editor
                .diff
                .as_ref()
                .is_some_and(|d| d.left_lines == d.right_lines && d.right_lines.len() == 2)
        },
    );
}

#[test]
fn a_background_diff_refresh_leaves_the_active_diff_find_and_anchor_alone() {
    // Only a rebuild of the ACTIVE tab recomputes the find bar; a diff
    // refreshing in another split must not scroll or re-anchor the view the
    // reader is on.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    let c = tmp.path().join("c.txt");
    let d = tmp.path().join("d.txt");
    std::fs::write(&a, "1\n").unwrap();
    std::fs::write(&b, "1\n2\n").unwrap();
    std::fs::write(&c, "x\n").unwrap();
    std::fs::write(&d, "x\nneedle\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_diff(&a, &b).unwrap();
    app.split_editor();
    app.editor.open_diff(&c, &d).unwrap();
    app.open_editor_find();
    app.diff_find_set_query(String::from("needle"));
    // Anchored AFTER the query: `diff_find_apply` scrolls to its match and
    // clears the hunk anchor, which is exactly why a spurious recompute
    // from a background refresh would be visible here.
    app.editor.diff.as_mut().unwrap().nav_anchor = Some(0);
    let before = app.editor.diff.as_ref().unwrap().clone();
    assert!(
        before.find.active.is_some(),
        "precondition: the active diff has a match"
    );

    rewrite_later(&b, "1\n2\n3\n");
    assert!(
        app.refresh_open_diff_views(false, &[], &no_paths()),
        "the background split's diff rebuilt"
    );
    let after = app.editor.diff.as_ref().unwrap();
    assert_eq!(
        after.nav_anchor,
        Some(0),
        "the active diff's hunk anchor is untouched"
    );
    assert_eq!(after.scroll, before.scroll, "and its viewport");
    assert_eq!(
        after.find.active, before.find.active,
        "and its active match"
    );
}

#[test]
fn a_background_diff_keeps_its_find_band_across_a_rebuild() {
    // The active match is recomputed inside `carry_view_from`, so a view
    // rebuilt in a background split keeps its band without the app's
    // find-bar recompute, which only runs for the active tab.
    let tmp = tempfile::tempdir().unwrap();
    let c = tmp.path().join("c.txt");
    let d = tmp.path().join("d.txt");
    std::fs::write(&c, "x\n").unwrap();
    std::fs::write(&d, "x\nneedle\n").unwrap();
    let plain = tmp.path().join("plain.txt");
    std::fs::write(&plain, "hi\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_diff(&c, &d).unwrap();
    app.open_editor_find();
    app.diff_find_set_query(String::from("needle"));
    assert!(app.editor.diff.as_ref().unwrap().find.active.is_some());
    // The diff moves to the background: a split, and a plain file on top.
    app.split_editor();
    app.editor.open_pinned(&plain).unwrap();
    assert!(
        app.editor.diff.is_none(),
        "the active tab is the plain file"
    );

    rewrite_later(&d, "x\nstill a needle\nmore\n");
    assert!(app.refresh_open_diff_views(false, &[], &no_paths()));
    let mut seen = 0;
    for group in app.editor_layout.inactive_groups_mut() {
        for ed in &group.editors {
            if let Some(dd) = ed.diff.as_ref().filter(|dd| dd.right_path == d) {
                seen += 1;
                assert_eq!(dd.find.needle.as_deref(), Some("needle"));
                assert!(
                    dd.find.active.is_some(),
                    "the background view's active match was recomputed in place"
                );
            }
        }
    }
    assert!(seen >= 1, "the background split still holds the diff");
}

#[test]
fn enter_opens_the_files_diff_and_esc_leaves_review() {
    // The cache-dir override is process-global; serialize with the
    // other tests that redirect it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.open_pr_review(crate::widgets::pr_review::tests::sample(), "o/r#579".into());
        let patch = "diff --git a/src/sarif/view.rs b/src/sarif/view.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/sarif/view.rs\n@@ -0,0 +1,2 @@\n+fn a() {}\n+fn b() {}\n";
        app.editor.pr_review.as_mut().unwrap().diff =
            Some(crate::pr_review::split_diff_by_file(patch));
        let pr_tab = app.editor.active_index();
        app.handle_pr_review_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            app.editor.diff.is_some(),
            "a diff tab opened: {}",
            app.status
        );
        assert_ne!(app.editor.active_index(), pr_tab, "in its own tab");
        app.editor.select(pr_tab);
        app.handle_pr_review_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            (0..app.editor.tab_count()).all(|i| {
                app.editor.select(i);
                app.editor.pr_review.is_none()
            }),
            "Esc leaves review: the PR tab is closed"
        );
    });
}

/// #858: View Changes vs previous on a one-commit repository reported git's
/// three-line error; it now reports its `fatal:` line.
#[test]
fn a_failed_diff_view_names_git_s_fatal_line() {
    let tmp = make_committed_repo();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.view_previous_commit_diff_source_control();
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(
        app.status,
        "View Changes vs previous failed: fatal: ambiguous argument 'HEAD~1': unknown revision or path not in the working tree."
    );
    assert_one_line_858(&app);
}

/// A binary (not UTF-8, no tab picking an encoding) file still opens
/// without a diff, as before.
#[test]
fn a_modified_binary_file_still_opens_without_a_diff() {
    let (tmp, _) = encoded_repo("blob.bin", b"\x00\xff\x01bin\n", b"\x00\xfemore\n");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "blob.bin")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "blob.bin")
        .unwrap();
    app.open_source_control_entry(idx);
    assert!(app.editor.diff.is_none());
}

#[test]
fn typing_on_into_the_disk_conflict_prompt_keeps_the_edits() {
    // The prompt opens mid-typing and takes the keys; Enter on whatever was
    // typed used to reload and throw the unsaved function away.
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_a_disk_conflict(tmp.path());
    type_keys(&mut app, "    print(tax(100))");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.input_prompt.is_none(), "the prompt is dismissed");
    assert!(
        app.editor.lines.join("\n").contains("def tax(amount):"),
        "the unsaved edits were discarded: {:?}",
        app.editor.lines
    );
    assert!(app.editor.dirty);
    assert!(
        app.status.contains("Kept your unsaved edits"),
        "{}",
        app.status
    );
}

#[test]
fn the_disk_conflict_prompt_starts_empty_and_says_what_reload_costs() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_a_disk_conflict(tmp.path());
    let prompt = app.input_prompt.as_ref().unwrap();
    assert_eq!(
        prompt.value, "",
        "nothing pre-filled for a stray Enter to confirm"
    );
    assert!(prompt.title.starts_with("tax.py "), "{}", prompt.title);
    let said = format!(
        "{} {}",
        prompt.placeholder,
        prompt.hint.clone().unwrap_or_default()
    );
    assert!(said.contains("discard"), "{said}");
    // Negative: Enter on the empty field does nothing at all.
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.input_prompt.is_some());
    assert!(app.editor.dirty);
}

#[test]
fn escape_on_the_disk_conflict_prompt_keeps_the_edits() {
    // Negative: the other way out still keeps them.
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_a_disk_conflict(tmp.path());
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.input_prompt.is_none());
    assert!(app.editor.lines.join("\n").contains("def tax(amount):"));
}

#[test]
fn compare_with_selected_diffs_an_unsaved_buffer_as_it_stands() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "x\n").unwrap();
    std::fs::write(&b, "y\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    type_str(&mut app, "z");
    app.dispatch_menu_action(
        MenuAction::CompareWithSelected {
            anchor: a.clone(),
            other: b.clone(),
        },
        tmp.path().to_path_buf(),
    );
    let diff = app.editor.diff.as_ref().expect("a diff view is open");
    assert_eq!(
        diff.left_lines,
        vec!["zx".to_string()],
        "the unsaved buffer, not the disk"
    );
    assert_eq!(
        diff.right_lines,
        vec!["y".to_string()],
        "a closed file reads the disk"
    );
}

/// #1588: the same for a merge editor in another editor group.
#[test]
fn a_merge_editor_in_another_group_is_not_backed_up_either() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_merge_editor_open(tmp.path());
    let other = tmp.path().join("other.txt");
    std::fs::write(&other, "x\n").unwrap();
    app.run_command(crate::widgets::command_palette::Command::SplitEditor);
    app.editor.open_pinned(&other).unwrap();
    assert!(
        app.editor_layout
            .inactive_groups()
            .iter()
            .any(|g| g.editors.iter().any(|e| e.merge.is_some())),
        "setup: the merge editor is in the other group"
    );
    assert!(
        !app.capture_hot_exit()
            .buffers
            .iter()
            .any(|b| b.tab.path.as_deref() == Some(file.as_path()))
    );
}
