//! The integrated terminal inside a pane: keys, the clipboard, scrollback,
//! find, links, recording and shell integration.

use super::*;

#[test]
fn terminal_warning_renders_inside_a_narrow_frame_without_panicking() {
    // #192: the modal's width clamp had a lower bound of 50, so any
    // terminal narrower than that produced a rect wider than the buffer
    // and the paint panicked at launch.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pending_terminal_warning = true;
    let backend = ratatui::backend::TestBackend::new(46, 12);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // And pathologically small: still no panic.
    let backend = ratatui::backend::TestBackend::new(8, 3);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
}

/// Overlays on a terminal smaller than their minimum size: `clamp(40, w)`
/// panics when the width is under 40, and a confirm dialog's 50-column
/// floor built a rect outside the buffer. A phone in portrait or a small
/// tmux split is enough.
#[test]
fn overlays_render_on_a_tiny_terminal_without_panicking() {
    let tmp = tempfile::tempdir().unwrap();
    let draw = |app: &mut App, w: u16, h: u16| {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    };
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_command_palette();
    draw(&mut app, 36, 20);
    draw(&mut app, 80, 9);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_finder();
    draw(&mut app, 36, 20);
    draw(&mut app, 80, 9);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pending_discard = Some(PendingDiscard {
        rel_path: String::from("a.txt"),
        untracked: false,
        staged: false,
    });
    draw(&mut app, 45, 12);
    draw(&mut app, 45, 5);
    draw(&mut app, 8, 3);
}

/// #1133: at one row the bottom panel's band is zero rows high just below
/// the frame, and its tab strip was still painted there.
#[test]
fn a_one_row_terminal_renders_without_panicking() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("data.csv"), "a,b\n1,2\n").unwrap();
    let draw = |app: &mut App, w: u16, h: u16| {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    };
    let mut welcome = App::new(tmp.path().to_path_buf()).unwrap();
    let mut csv = App::new(tmp.path().to_path_buf()).unwrap();
    csv.editor.open(&tmp.path().join("data.csv")).unwrap();
    for app in [&mut welcome, &mut csv] {
        for w in [140, 100, 80, 40, 20] {
            draw(app, w, 1);
        }
    }
}

#[test]
fn terminal_warning_swallows_a_key_and_dismisses_for_the_session() {
    // While the unsupported-terminal nudge is up, any non-D key dismisses it
    // for this session and is swallowed so it never reaches the editor below.
    // Uses a plain letter so nothing is persisted to the real prefs file.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pending_terminal_warning = true;
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        !app.pending_terminal_warning,
        "any key should dismiss the nudge for this session"
    );
}

/// #1355: a URL longer than the pane soft-wraps, and Cmd/Ctrl+click read
/// only the clicked row: the first row opened a cut-down URL, the others
/// opened nothing. Every row of it must resolve to the whole URL.
#[test]
fn a_url_that_wraps_the_pane_edge_resolves_whole_from_any_of_its_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let inner = app.terminals[0].last_inner;
    let w = inner.width as usize;
    let url = format!("https://example.com/oauth?state={}END", "y".repeat(w));
    let chunk = format!("\r\nsee{} {url}\r\nnext line\r\n", "x".repeat(w - 10));
    app.terminals[0].feed_bytes_for_test(chunk.as_bytes());
    term.draw(|f| app.render(f)).unwrap();

    let pane = &app.terminals[0];
    let (_, head_row) = cell_carrying(pane, "https:").expect("the URL's first row");
    // The URL's head sits in the last six cells of its first row, then fills
    // the next row and ends on the one after.
    let clicks = [
        (inner.x + inner.width - 3, head_row),
        (inner.x + 5, head_row + 1),
        (inner.x + 1, head_row + 2),
    ];
    for (col, row) in clicks {
        let (text, idx) = pane.line_text_at(col, row).expect("inside the pane");
        assert_eq!(
            crate::port_detect::url_at(&text, idx).as_deref(),
            Some(url.as_str()),
            "click at ({col}, {row}) on {text:?}"
        );
    }
    // The row after it does not wrap into anything: it reads as itself.
    let (text, idx) = pane.line_text_at(inner.x + 1, head_row + 3).unwrap();
    assert!(text.starts_with("next line"), "{text:?}");
    assert_eq!(idx, 1);
    assert!(crate::port_detect::url_at(&text, idx).is_none());
}

/// `cell_carrying` returns a cell INSIDE the needle, not the row's first cell.
///
/// The refinement has exactly one red state in the suite, and it is a
/// downstream word-selection assertion in another test, so it does not
/// depend on that one test surviving: the helper's own doc records that the
/// column derivation in it was wrong once.
///
/// Placed beside the two helpers rather than in the block it was first
/// written in, which was some 35,000 lines away and made "beside the
/// helper" a claim the file did not support. Below `cell_with_link`
/// rather than between the two: inserting above a `///` block hands that
/// block to the newcomer, which this file has been bitten by twice.
#[test]
fn cell_carrying_lands_inside_the_needle_not_at_the_start_of_its_row() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let area = app.terminals[0].last_area;
    assert!(
        area.width > 4 && area.height > 2,
        "the terminal must be laid out, or the search finds nothing and the \
         failure below blames a missing token rather than a missing pane"
    );

    // The needle is NOT at column 0: a shell-shaped prefix in front of it is
    // what separates a span predicate from a row-level `contains`, which
    // would resolve the `b` of `bash-5.2$` instead.
    app.terminals[0].feed_bytes_for_test(b"a prompt got here first\r\nbash-5.2$ tok_here\r\n");
    term.draw(|f| app.render(f)).unwrap();

    let (col, row) =
        cell_carrying(&app.terminals[0], "tok_here").expect("the pane must show this test's token");
    let (text, idx) = app.terminals[0]
        .line_text_at(col, row)
        .expect("the returned cell must map to a row and a char index");
    let start = text
        .find("tok_here")
        .map(|b| text[..b].chars().count())
        .expect("the row the search returned must carry the needle");
    assert!(
        (start..start + "tok_here".chars().count()).contains(&idx),
        "the cell must fall inside the needle: row {text:?} put the cell at \
         char {idx} and the needle spans {start}..{}",
        start + "tok_here".chars().count()
    );
    assert!(
        idx > 0,
        "and this fixture must keep the needle off column 0, or a row \
         predicate and a span predicate pick the same cell and the assertion \
         above cannot fail"
    );
}

#[test]
fn a_terminal_miss_does_not_blame_a_view_it_never_searched() {
    // The refusal was scoped to the editor lookup, but the STATUS derived from
    // the same predicate stayed arm-wide: a click in a TERMINAL pane with no
    // link reported "No links in this view", naming the editor's view — which
    // `editor_blocked` had already short-circuited, so it was never searched.
    //
    // "a non-text view is up" and "the editor lookup is the one that missed"
    // are different questions, and the status must answer the second.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("a.bin");
    std::fs::write(&bin, [0u8, 1, 2, 3, 4, 5, 6, 7]).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+click", "command": "mouse_open_link_at_click", "when": "terminal"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    app.editor.hex = Some(crate::hex::HexView::open(&bin).unwrap());
    assert!(
        app.editor.has_non_text_view(),
        "precondition: a non-text view must be up, or the arm-wide status and \
         the scoped one agree and this test cannot tell them apart"
    );

    // Plain text, no link anywhere on the row.
    app.terminals[0].feed_bytes_for_test(b"just some words\r\n");
    term.draw(|f| app.render(f)).unwrap();
    let area = app.terminals[0].last_area;
    let (col, row) = (area.x + 2, area.y + 1);
    assert!(
        app.terminals[0].hyperlink_at_screen(col, row).is_none(),
        "precondition: the cell must hold NO link, or this tests the hit path"
    );
    assert_eq!(
        app.terminal_at_pos(col, row),
        Some(0),
        "precondition: and must be inside a terminal pane, or the editor \
         lookup is legitimately the one that missed"
    );

    app.focus_pane(Pane::Terminal);
    let mut ev = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ev.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ev);

    assert_eq!(
        app.status, "No link there",
        "a miss in a TERMINAL pane must not name the editor's view: that \
         lookup was short-circuited and never searched"
    );
}

#[test]
fn a_terminal_link_binding_is_not_refused_for_an_editor_side_reason() {
    // `MouseOpenLinkAtClick` spans two panes: it tries the editor's document
    // link, then falls through to the terminal helpers. The `has_non_text_view`
    // guard was placed on the whole ARM, but its justification — `buffer_pos_at`
    // maps cells from the TEXT layout, so a diff/hex view yields a position
    // into a buffer the user is not looking at — is about the EDITOR lookup
    // only. `terminal_url_click`/`terminal_file_click` read `self.terminal()`
    // and are independent of editor view state.
    //
    // The built-in this command replaces guards neither, so an arm-wide guard
    // made the bound command diverge from the built-in in the opposite
    // direction to the one the guard was added to fix.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("a.bin");
    std::fs::write(&bin, [0u8, 1, 2, 3, 4, 5, 6, 7]).unwrap();
    // `App::new` + direct keymap assignment, matching the sibling OSC 8 tests:
    // `tree_app_with_keymap` focuses the TREE, which lays the panes out
    // differently and moves the terminal's `last_area`.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+click", "command": "mouse_open_link_at_click", "when": "terminal"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    // The editor shows a hex preview of an UNRELATED file — the state the
    // guard exists for, and which has nothing to do with the terminal.
    app.editor.hex = Some(crate::hex::HexView::open(&bin).unwrap());
    assert!(
        app.editor.has_non_text_view(),
        "precondition: the editor must be on a non-text view, or the guard \
         never fires and this test is about the ordinary path"
    );

    // A non-web OSC 8 link in the terminal: resolves via `hyperlink_at_screen`,
    // then `open_detected_url` refuses the scheme INERTLY. A web URL would
    // reach `open_url` and launch a real browser.
    // The prompt of the real shell in this pane races the fed bytes, so the
    // line it lands on is not fixed (#397). This puts a line in front of the
    // link deliberately, making the order that used to break this test the
    // one it always runs, and the cell is then searched for rather than
    // assumed.
    app.terminals[0].feed_bytes_for_test(b"a prompt got here first\r\n");
    // A DECOY link on an earlier row, which is what gives `cell_with_link`'s
    // exact-URI predicate a red state here: weaken it to `.is_some()` and the
    // row-major search stops on the decoy, so the click resolves the wrong
    // link and the status assertion below rejects it by name. Without this
    // the predicate could be reverted and the test would still pass, since
    // nothing else in this pane emits OSC 8.
    //
    // `mailto:` rather than `file://` deliberately: `file://` is intercepted
    // by `editor_file_uri` and would exercise a different arm.
    app.terminals[0]
        .feed_bytes_for_test(b"\x1b]8;;mailto:decoy@example.com\x1b\\decoy\x1b]8;;\x1b\\\r\n");
    // Trailing newline parks the cursor on the NEXT row: without it any late
    // shell output writes onto the link's own row.
    app.terminals[0]
        .feed_bytes_for_test(b"\x1b]8;;mailto:t@example.com\x1b\\link\x1b]8;;\x1b\\\r\n");
    term.draw(|f| app.render(f)).unwrap();
    let (col, row) = cell_with_link(&app.terminals[0], "mailto:t@example.com").expect(
        "precondition: the click must land on the terminal link, or a refusal \
             proves nothing about which branch refused it",
    );

    app.focus_pane(Pane::Terminal);
    let mut ev = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ev.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ev);

    assert_eq!(
        app.status, "Refused to open non-web link: mailto:t@example.com",
        "a TERMINAL-region link binding must reach the terminal helpers even \
         while the EDITOR shows a hex preview of an unrelated file: the guard's \
         reason does not apply to a pane it does not read"
    );
}

#[test]
fn a_mouse_tracking_child_keeps_the_pointer_from_a_bound_wheel() {
    // A tracking child owns the gestures croft actually FORWARDS: the wheel
    // (both wheel arms) and, since #474, the left button. So a bound wheel
    // must decline for the child. The left click is covered by
    // `a_bound_click_defers_to_a_mouse_tracking_child`, which asserts the
    // same rule for the same reason.
    //
    // SHIFT is the documented override.
    use crossterm::event::MouseEventKind;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+wheel_up", "command": "quick_open", "when": "terminal"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // The child enables mouse tracking.
    app.terminals[0].feed_bytes_for_test(b"\x1b[?1000h");
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.terminals[0].mouse_reporting(),
        "the child is tracking the mouse"
    );

    let area = app.terminals[0].last_area;
    let (col, row) = (area.x + 2, area.y + 1);
    // Precondition: the cell must be inside `last_inner`, or the guard
    // declines for the BORDER reason and this proves nothing about tracking.
    assert!(
        app.terminals[0].cell_at(col, row).is_some(),
        "the wheel must land on a real grid cell"
    );
    let mut ev = mouse(MouseEventKind::ScrollUp, col, row);
    ev.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ev);
    assert!(
        app.file_finder.is_none(),
        "croft FORWARDS the wheel to a tracking child (all three `report_mouse` \
         call sites construct WheelUp/WheelDown), so the child really does own \
         this gesture and the binding must not fire"
    );
}

/// The Shift half of the same rule, on its own app: the first click of the
/// pair above is forwarded to the child, which starts a selection drag, and
/// a drag in flight legitimately swallows the next press. Sequencing the two
/// halves would test that interaction rather than the bypass.
#[test]
fn holding_shift_takes_the_pointer_back_from_a_tracking_child() {
    // Shift is ALSO part of a gesture's identity, so the bypass selects a
    // different row rather than rescuing the `ctrl+click` one: over a
    // tracking TUI you must bind `ctrl+shift+click` for a gesture that fires.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+shift+click", "command": "quick_open", "when": "terminal"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.terminals[0].feed_bytes_for_test(b"\x1b[?1000h");
    term.draw(|f| app.render(f)).unwrap();
    assert!(app.terminals[0].mouse_reporting(), "the child is tracking");

    let area = app.terminals[0].last_area;
    let mut shifted = mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.x + 2,
        area.y + 1,
    );
    shifted.modifiers = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    app.handle_mouse(shifted);
    assert!(
        app.file_finder.is_some(),
        "Shift takes the pointer back and the ctrl+shift+click row fires"
    );
}

#[test]
fn the_swallow_guard_reads_the_clicked_terminal_not_the_active_one() {
    // `terminal_would_open_something_at` decides whether swallowing a
    // double-click prefix would cost the user a built-in link-open. The
    // built-in calls `activate_terminal_pane(idx)` BEFORE it looks for a
    // link, so it reads the CLICKED pane. A predicate reading
    // `self.terminal()` reads the ACTIVE pane, and in any split layout where
    // the click lands outside the focused pane those are different content —
    // so the swallow decision disagrees with what the built-in then does.
    //
    // Observable is a REFUSED link (`open_detected_url` rejects a non-web
    // scheme inertly). A printed https URL would reach `open_url`, which is
    // `Command::new("open")` with no test guard, and the suite would launch a
    // real browser on every run.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    // Only the DOUBLE is bound, so a single ctrl+click is a prefix and the
    // swallow branch is the one under test.
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+double_click", "command": "toggle_word_wrap", "when": "terminal"}]"#,
    );
    app.split_terminal().unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(app.terminals.len(), 2, "two panes are visible");

    // Link in pane 0; pane 1 is ACTIVE. The two panes must differ, or the
    // bug is unreachable and this test proves nothing.
    app.terminals[0]
        // A DECOY link on an earlier row. Under the `.is_some()` this branch
        // replaced, the row-major search hits the decoy first and the click
        // resolves the wrong link, so the exact-status assertion fails with
        // `decoy@` in the message. That is the red state the tightening
        // shipped without: nothing else in these panes emits OSC 8, so
        // reverting it left the suite green.
        //
        // `mailto:` rather than `file://` deliberately: `file://` is
        // intercepted by `editor_file_uri` and would exercise a different arm.
        .feed_bytes_for_test(b"\x1b]8;;mailto:decoy@example.com\x1b\\decoy\x1b]8;;\x1b\\\r\n");
    app.terminals[0]
        .feed_bytes_for_test(b"\x1b]8;;mailto:split@example.com\x1b\\split-link\x1b]8;;\x1b\\\r\n");
    app.active_terminal = 1;
    term.draw(|f| app.render(f)).unwrap();

    // FIND the link's cell rather than assuming which row it landed on
    // (#397). Pane 0 has a real shell, and its prompt arrives from the
    // reader thread whenever the OS gets round to it - before the injected
    // bytes on a loaded machine, after them on a quiet one. Either way the
    // link is in the pane; only its row moves, and hard-coding
    // `area.y + 1` made the test a race against shell startup rather than a
    // check of the swallow guard it is named for.
    //
    // The search IS the click coordinate here, not a separate check ahead of
    // one, and it is anchored on this test's own URI rather than on "a link":
    // the decoy above is what makes that distinction fail loudly. Every cell
    // `cells_of` yields is inside `last_area`, which `hyperlink_at_screen`
    // hit-tests, so a cell found is a cell a click reaches - the claim is not
    // weakened by not naming the row.
    let (col, row) = cell_with_link(&app.terminals[0], "mailto:split@example.com")
        .expect("pane 0 must carry the link somewhere, or the built-in has nothing to act on");
    assert_eq!(
        app.active_terminal, 1,
        "pane 1 must be ACTIVE while the click lands in pane 0, or the \
         clicked-vs-active distinction this test exists for does not arise"
    );

    let mut ctrl = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ctrl.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ctrl);

    assert_eq!(
        app.status, "Refused to open non-web link: mailto:split@example.com",
        "the prefix click must defer to the built-in, which opens the link in \
         the CLICKED pane. Reading the ACTIVE pane finds nothing there, \
         swallows the click, and the link never opens"
    );
}

#[test]
fn a_double_click_prefix_over_a_mouse_tracking_child_leaves_the_builtin_alone() {
    // The swallow branch exists so the FIRST click of a bound `ctrl+double_click`
    // does not fire the built-in that owns the same modifier. Over a
    // mouse-tracking child it must still fall through, because swallowing
    // would disable the built-in Ctrl+click over every full-screen TUI while
    // binding `ctrl+click` leaves it working -- backwards.
    //
    // Since #474 the child DOES own a left click here (`child_owns_pointer`
    // covers Click/DoubleClick), so the swallow steps aside for that reason
    // too. The fall-through is right for a narrower reason that survives it:
    // the Cmd/Ctrl link-open runs BEFORE the forward in the Down(Left) arm,
    // so swallowing this click costs the user the link and buys nobody
    // anything -- the bound double is already unreachable over a tracker.
    //
    // The observable is deliberately a REFUSED link: `open_detected_url`
    // rejects a non-web scheme and sets a status without spawning anything.
    // Asserting on a successful open would shell out to `open`/`xdg-open` for
    // real, creating exactly the flaky spawning test #307 is about.
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
    // A real OSC 8 hyperlink with a non-web scheme: the built-in finds it via
    // `hyperlink_at_screen`, and `open_detected_url` then refuses it inertly.
    // The leading line forces the shell-prompt race (#397) to resolve the way
    // that used to break this test, so the cell is searched for instead of
    // computed from the border row.
    app.terminals[0].feed_bytes_for_test(b"a prompt got here first\r\n");
    // Two links in one feed, in the order the bytes carry them: the DECOY
    // first, then the payload. The `a prompt got here first` line above is a
    // SEPARATE feed, and that is fine here for a reason worth stating, since
    // a sibling test was just rewritten to avoid exactly that: this search is
    // anchored on a URI, not on a column, so a prompt arriving between the
    // two feeds moves the link's row and changes nothing the test asserts.
    // The wrap test cannot say that, because a column is precisely its
    // claim.
    //
    // The decoy is what gives the exact-URI SEARCH a
    // red state: revert the predicate to `.is_some()` and the row-major scan
    // stops on the decoy, so the status assertion rejects it by name. It does
    // NOT pin the assertion's FORM - with the search intact, `contains`
    // accepts the same correct answer, and an earlier version of this comment
    // claimed otherwise.
    //
    // The trailing newline after the payload parks the cursor on the NEXT
    // row, so late shell output cannot write onto the link's own row.
    app.terminals[0].feed_bytes_for_test(
        b"\x1b]8;;mailto:decoy@example.com\x1b\\decoy\x1b]8;;\x1b\\\r\n\x1b]8;;mailto:x@example.com\x1b\\link\x1b]8;;\x1b\\\r\n",
    );
    let (col, row) = cell_with_link(&app.terminals[0], "mailto:x@example.com").expect(
        "the cell must carry THIS test's OSC 8 link, or the built-in has nothing \
             to act on and this test cannot distinguish the two branches",
    );

    app.terminals[0].feed_bytes_for_test(b"\x1b[?1000h");
    assert!(
        app.terminals[0].mouse_reporting(),
        "the child must be tracking, or `child_owns_pointer` is false and this \
         test proves nothing about the guard"
    );

    let mut ctrl = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ctrl.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ctrl);

    assert_eq!(
        app.status, "Refused to open non-web link: mailto:x@example.com",
        "a double-click PREFIX over a mouse-tracking child must fall through to \
         the built-in: the Cmd/Ctrl link-open runs BEFORE the press is \
         forwarded, so swallowing the click would cost the user the link. \
         Expected the refusal from the built-in link handler"
    );
}

#[test]
fn terminal_decoration_dot_click_is_not_hijacked_by_the_sidebar_splitter() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A pane whose script emits the OSC 133 protocol, so a decoration dot
    // lands on its left border.
    let script = "printf '\\033]133;A\\007$ \\033]133;B\\007cmd\\n\\033]133;C\\007out\\n\\033]133;D;0\\007'; sleep 30";
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), script.into()],
        tmp.path(),
    )
    .unwrap();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the shell to emit a decoration mark",
        || !app.terminals[0].command_decorations().is_empty(),
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let deco = app.terminals[0].command_decorations()[0];
    let pane = app.terminals[0].last_area;
    let inner_y = app.terminals[0].last_inner.y;
    let (col, row) = (pane.x, inner_y + u16::try_from(deco.line).unwrap());
    assert_eq!(
        app.terminals[0].decoration_at_screen(col, row),
        Some(deco),
        "the dot cell must hit-test to the decoration"
    );
    // The regression: a lone pane's left border sits exactly in the sidebar
    // splitter's two-column grab zone, which used to swallow the click as a
    // resize drag before the decoration handler could see it.
    let x = app
        .sidebar_splitter_x
        .expect("sidebar shown, so the splitter seam exists");
    assert!(
        col == x || col == x.saturating_sub(1),
        "the dot column ({col}) must lie in the splitter grab zone ({x})"
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert!(
        app.splitter_drag.is_none(),
        "pressing a decoration dot must not start a sidebar resize"
    );
    assert!(
        app.context_menu.is_some(),
        "pressing a decoration dot must open the command's action menu"
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
fn cmd_f_opens_terminal_find_and_captures_typing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    assert!(app.terminal_find.is_none(), "find starts closed");

    // Cmd+F opens the find bar rather than being forwarded to the shell.
    app.handle_terminal_key(key(KeyCode::Char('f'), KeyModifiers::SUPER));
    assert!(app.terminal_find.is_some(), "Cmd+F must open the find bar");

    // Typing while the bar is open builds the query, it is not sent to the PTY.
    app.handle_terminal_key(key(KeyCode::Char('l'), KeyModifiers::NONE));
    app.handle_terminal_key(key(KeyCode::Char('s'), KeyModifiers::NONE));
    assert_eq!(
        app.terminal_find.as_ref().unwrap().query,
        "ls",
        "printable keys must append to the find query"
    );

    // Backspace edits the query.
    app.handle_terminal_key(key(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(app.terminal_find.as_ref().unwrap().query, "l");

    // Esc closes it.
    app.handle_terminal_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.terminal_find.is_none(), "Esc must close the find bar");
}

/// VS Code and iTerm2 on macOS reserve Cmd+F for terminal find and let
/// Ctrl+F through to the child — it is a control byte programs bind
/// (vim's page-forward, the shell's emacs forward-char). Stealing both
/// chords left no way to send 0x06 to a pane at all. Linux keeps Ctrl+F
/// opening find, the VS Code default there.
#[cfg(target_os = "macos")]
#[test]
fn ctrl_f_reaches_the_terminal_child_on_macos() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.handle_terminal_key(key(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert!(
        app.terminal_find.is_none(),
        "Ctrl+F must not open find on macOS"
    );
    let written = app.terminals[0].written_bytes_for_test();
    assert!(
        written.ends_with(&[0x06]),
        "Ctrl+F must be encoded to the child, wrote {written:?}"
    );
}

/// Run Terminal: Run Selected Text in Active Terminal the way the palette
/// does, and return what the active pane was sent.
fn run_selected_text(app: &mut App) -> String {
    let cmd = crate::widgets::command_palette::Command::from_id("run_selected_text")
        .expect("there is a Run Selected Text command");
    let before = app.terminals[app.active_terminal]
        .written_bytes_for_test()
        .len();
    app.run_command(cmd);
    let written = app.terminals[app.active_terminal].written_bytes_for_test();
    String::from_utf8_lossy(&written[before..]).into_owned()
}

/// What a pane was sent, without bracketed-paste marks: the test pane's
/// shell turns bracketed paste on by itself, at a time the test can't pin.
fn unbracketed(sent: &str) -> String {
    sent.replace("\x1b[200~", "").replace("\x1b[201~", "")
}

/// #1292: the selection goes to the terminal with Enter after it, and
/// focus stays in the editor so the next line can be picked.
#[test]
fn run_selected_text_sends_the_selection_and_enter_to_the_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(
        tmp.path(),
        "explore.py",
        "import csv\nrows = [1, 2]\ntotal = sum(rows)\nprint(total)\n",
    );
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (2, 0),
        head: (3, 12),
    });
    assert_eq!(
        unbracketed(&run_selected_text(&mut app)),
        "total = sum(rows)\nprint(total)\r"
    );
    assert!(app.focus == Pane::Editor, "focus stays in the editor");
    assert!(app.show_terminal, "the terminal is shown");
}

/// #1274: Run Selected Text is not a paste to confirm, even with the
/// multi-line warning always on; the lines and their Enter go together.
#[test]
fn run_selected_text_skips_the_multiline_paste_confirm() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "explore.py", "a = 1\nb = 2\n");
    app.multiline_paste_warning = crate::prefs::MultilinePasteWarning::Always;
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (0, 0),
        head: (1, 5),
    });
    assert_eq!(unbracketed(&run_selected_text(&mut app)), "a = 1\nb = 2\r");
    assert!(app.pending_terminal_paste.is_none(), "nothing held");
}

/// #1292: a multi-line block arrives as one bracketed paste when the
/// program in the pane asked for it, and Enter follows outside it.
#[test]
fn run_selected_text_brackets_the_block_when_the_pane_asked_for_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "a = 1\nb = 2\n");
    app.terminals[app.active_terminal].feed_bytes_for_test(b"\x1b[?2004h");
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (0, 0),
        head: (1, 5),
    });
    assert_eq!(
        run_selected_text(&mut app),
        "\x1b[200~a = 1\nb = 2\x1b[201~\r"
    );
}

/// #1292: with no selection the caret's line is sent and the caret steps
/// to the next non-blank line, so repeated runs walk through a script.
#[test]
fn run_selected_text_without_a_selection_sends_the_line_and_steps_on() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "a = 1\n\n\nb = 2\n");
    assert_eq!(unbracketed(&run_selected_text(&mut app)), "a = 1\r");
    assert_eq!(app.editor.cursor_row, 3, "blank lines are stepped over");
    assert_eq!(unbracketed(&run_selected_text(&mut app)), "b = 2\r");
}

/// #1292 negative: with nothing to send (a blank line at the end of the
/// file) the terminal gets nothing, not even a bare Enter.
#[test]
fn run_selected_text_sends_nothing_for_a_blank_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "a = 1\n");
    app.editor.cursor_row = 1;
    assert_eq!(unbracketed(&run_selected_text(&mut app)), "");
}

#[test]
fn cmd_opt_up_parks_the_previous_prompt_mark_at_the_viewport_top() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Swap in a pane that emits an OSC 133 prompt mark and then scrolls it
    // into history with filler output.
    let script = "printf '\\033]133;A\\007PROMPT\\n'; i=0; while [ $i -lt 40 ]; do echo filler-$i; i=$((i+1)); done";
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), script.into()],
        tmp.path(),
    )
    .unwrap();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the prompt mark and filler output",
        || {
            !app.terminals[0].prompt_lines().is_empty()
                && app.terminals[0]
                    .grid_lines()
                    .0
                    .iter()
                    .any(|l| l.contains("filler-39"))
        },
    );
    app.focus_pane(Pane::Terminal);
    assert_eq!(
        app.terminal().viewport_top_line(),
        0,
        "starts at the live bottom"
    );

    app.handle_terminal_key(key(KeyCode::Up, KeyModifiers::SUPER | KeyModifiers::ALT));
    let mark = app.terminal().prompt_lines()[0];
    assert!(mark < 0, "the mark scrolled into history");
    assert_eq!(
        app.terminal().viewport_top_line(),
        mark,
        "Cmd+Opt+Up must park the previous prompt mark at the viewport top"
    );

    // Only one mark: jumping forward finds nothing past it.
    app.handle_terminal_key(key(KeyCode::Down, KeyModifiers::SUPER | KeyModifiers::ALT));
    assert!(
        app.status.contains("No next command"),
        "status was: {}",
        app.status
    );
}

#[test]
fn ctrl_shift_space_quick_select_labels_matches_and_a_label_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A pane that prints two matchable strings, bottom-most last, and stays
    // alive so the grid does not vanish under the test.
    let script = "echo see-https://example.com/quick; echo sha de40bdf12ab34; sleep 60";
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from(script)],
        tmp.path(),
    )
    .unwrap();
    // The run-label header (`▶ /bin/sh -c echo …`) also contains both
    // sentinels, so wait until the SHA shows up on its own echoed row too
    // (a second occurrence); otherwise quick-select can open on a
    // header-only viewport and the hint set is timing-dependent.
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the hash on its own echoed row as well as in the run-label header",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .filter(|l| l.contains("de40bdf12ab34"))
                .count()
                >= 2
        },
    );
    app.focus_pane(Pane::Terminal);
    assert!(app.terminal_quick_select.is_none(), "starts off");

    app.handle_terminal_key(key(
        KeyCode::Char(' '),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    let state = app
        .terminal_quick_select
        .as_ref()
        .expect("Ctrl+Shift+Space must enter quick-select");
    assert!(
        state
            .hints
            .iter()
            .any(|h| h.text == "https://example.com/quick"),
        "the URL must be labelled"
    );
    // The echoed SHA is the bottom-most match on screen, so it gets the
    // cheapest label (WezTerm / tmux-thumbs assignment order).
    let sha = state.hints.last().expect("hints must not be empty");
    assert_eq!(
        (sha.text.as_str(), sha.label.as_str()),
        ("de40bdf12ab34", "a"),
        "the bottom-most match gets the cheapest label; hints were: {:?}",
        state
            .hints
            .iter()
            .map(|h| (h.line, h.text.as_str(), h.label.as_str()))
            .collect::<Vec<_>>()
    );

    // Esc cancels without touching anything.
    app.handle_terminal_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.terminal_quick_select.is_none(), "Esc must cancel");

    // Re-enter and commit the SHA's label: copies and closes the mode.
    app.handle_terminal_key(key(
        KeyCode::Char(' '),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    let label = app
        .terminal_quick_select
        .as_ref()
        .unwrap()
        .hints
        .iter()
        .find(|h| h.text == "de40bdf12ab34")
        .unwrap()
        .label
        .clone();
    for c in label.chars() {
        app.handle_terminal_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(
        app.terminal_quick_select.is_none(),
        "committing a label must close quick-select"
    );
    assert!(
        app.status.contains("Copied"),
        "commit must report the copy, status was: {}",
        app.status
    );
}

#[test]
fn shift_pageup_pages_the_scrollback_and_shift_end_snaps_back() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let script = "i=0; while [ $i -lt 80 ]; do echo page-$i; i=$((i+1)); done; sleep 60";
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from(script)],
        tmp.path(),
    )
    .unwrap();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the filler output",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("page-79"))
        },
    );
    app.focus_pane(Pane::Terminal);
    let at_bottom = app.terminal().viewport_top_line();

    app.handle_terminal_key(key(KeyCode::PageUp, KeyModifiers::SHIFT));
    let paged = app.terminal().viewport_top_line();
    assert!(
        paged < at_bottom,
        "Shift+PageUp must scroll into history: {paged} vs {at_bottom}"
    );

    app.handle_terminal_key(key(KeyCode::Home, KeyModifiers::SHIFT));
    let top = app.terminal().viewport_top_line();
    assert!(
        top < paged,
        "Shift+Home must jump to the oldest line: {top} vs {paged}"
    );

    app.handle_terminal_key(key(KeyCode::End, KeyModifiers::SHIFT));
    assert_eq!(
        app.terminal().viewport_top_line(),
        at_bottom,
        "Shift+End must snap back to the live bottom"
    );
}

#[test]
fn copy_on_select_puts_a_finished_terminal_selection_on_the_clipboard() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A run-label pane writes its `▶ …` header synchronously at spawn, so
    // the first row has deterministic text to select.
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("sleep 60")],
        tmp.path(),
    )
    .unwrap();
    app.copy_on_select = true;
    app.focus_pane(Pane::Terminal);
    // Render so the pane has a real on-screen area to select inside.
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let inner = app.terminal().last_inner;
    app.terminal_mut().start_selection_at(inner.x, inner.y);
    app.terminal_mut().extend_selection_to(inner.x + 8, inner.y);
    let expected = app.terminal().selection_text();
    assert!(!expected.is_empty(), "selection must cover some pane text");

    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        inner.x + 8,
        inner.y,
    ));
    let clip = crate::clipboard::read_string().unwrap_or_default();
    assert_eq!(
        clip, expected,
        "finishing the drag must copy the selection when copy_on_select is on"
    );
}

#[test]
fn pane_inline_image_overlay_anchors_hides_when_scrolled_off_and_returns() {
    use base64::Engine;
    let mut png_buf = Vec::new();
    image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]))
        .write_to(
            &mut std::io::Cursor::new(&mut png_buf),
            image::ImageFormat::Png,
        )
        .expect("encode test png");
    let png: &[u8] = &png_buf;
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A real protocol + cell geometry: tests run headless, so the values
    // init_graphics would have probed are injected directly.
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::ITerm2;
    app.cell_pixel = Some((10, 20));
    // The image prints first, then enough filler to scroll it into history.
    let script = format!(
        "printf '\\033]1337;File=inline=1:{b64}\\007\\n'; i=0; while [ $i -lt 80 ]; do echo fill-$i; i=$((i+1)); done; sleep 60"
    );
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), script],
        tmp.path(),
    )
    .unwrap();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the image and its filler",
        || {
            !app.terminals[0].pane_images().is_empty()
                && app.terminals[0]
                    .grid_lines()
                    .0
                    .iter()
                    .any(|l| l.contains("fill-79"))
        },
    );
    app.focus_pane(Pane::Terminal);
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();

    // At the live bottom the anchor sits in scrollback: no overlay.
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.terminal_image_payload().is_none(),
        "an off-screen anchor must hide the picture"
    );

    // Scrolling to the top brings the anchor into view: overlay appears,
    // anchored inside the pane at the image's viewport row.
    app.terminal_mut().scroll_to_top();
    term.draw(|f| app.render(f)).unwrap();
    let (osc, layout) = app
        .terminal_image_payload()
        .expect("a visible anchor must produce an overlay payload");
    assert!(
        osc.contains("1337;File=inline=1"),
        "the payload must be a re-baked OSC 1337 image"
    );
    let inner = app.terminal().last_inner;
    assert_eq!(layout.pane, 0);
    assert!(
        layout.cell_y >= inner.y && layout.cell_y < inner.y + inner.height,
        "the picture must anchor inside the pane: {layout:?} vs {inner:?}"
    );
    assert!(layout.cell_w >= 1 && layout.cell_h >= 1);
    // The main loop would flush the escape and mark it displayed.
    app.mark_terminal_image_displayed();

    // Snapping back to the live bottom hides it again and arms the clear.
    app.terminal_mut().reset_scrollback();
    term.draw(|f| app.render(f)).unwrap();
    assert!(app.terminal_image_payload().is_none());
    assert!(
        app.consume_terminal_image_clear(),
        "hiding a displayed picture must arm the clear latch"
    );
}

#[test]
fn quick_select_paints_gold_labels_and_broadcast_paints_modal_and_pills() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let script = "echo grab https://example.com/paint; sleep 60";
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from(script)],
        tmp.path(),
    )
    .unwrap();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the URL on its own echoed row as well as in the run-label header",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .filter(|l| l.contains("example.com/paint"))
                .count()
                >= 2
        },
    );
    app.focus_pane(Pane::Terminal);
    // Render once BEFORE opening quick select: the first render resizes the
    // pane to its layout size (reflowing the grid), and hints must be
    // computed against the geometry they will be painted in.
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.handle_terminal_key(key(
        KeyCode::Char(' '),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert!(app.terminal_quick_select.is_some());
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let gold = Color::Rgb(0xff, 0xd7, 0x4a);
    let label_cell = (0..buf.area.height)
        .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
        .find(|&(x, y)| buf[(x, y)].bg == gold && buf[(x, y)].fg == Color::Black);
    assert!(
        label_cell.is_some(),
        "a quick-select label must paint black-on-gold somewhere in the pane"
    );
    app.handle_terminal_key(key(KeyCode::Esc, KeyModifiers::NONE));

    // Broadcast: the enable confirm modal paints its red title, and once
    // confirmed every receiving pane's name pill carries the ⇶ marker.
    app.terminals.push(
        crate::widgets::terminal::PtyTerminal::new_running(
            "/bin/sh",
            &[String::from("-c"), String::from("sleep 60")],
            tmp.path(),
        )
        .unwrap(),
    );
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('i'), KeyModifiers::NONE)));
    fn screen_rows(term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect()
            })
            .collect()
    }
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        screen_rows(&term)
            .iter()
            .any(|r| r.contains("BROADCAST INPUT TO ALL PANES?")),
        "the confirm modal must be painted while pending"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let marks = screen_rows(&term)
        .iter()
        .map(|r| r.matches('\u{21f6}').count())
        .sum::<usize>();
    assert_eq!(
        marks, 2,
        "both receiving panes must wear the ⇶ broadcast pill"
    );
}

#[test]
fn cmd_k_i_broadcasts_input_to_every_pane_with_confirm_and_auto_off() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mk = || {
        crate::widgets::terminal::PtyTerminal::new_running(
            "/bin/sh",
            &[
                String::from("-c"),
                String::from("read x; echo got-$x; sleep 30"),
            ],
            tmp.path(),
        )
        .unwrap()
    };
    app.terminals[0] = mk();
    app.terminals.push(mk());
    app.active_terminal = 0;
    app.focus_pane(Pane::Terminal);

    // Cmd+K I asks for confirmation first (the classic multi-pane footgun
    // is typing a destructive command into panes you forgot were listening).
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('i'), KeyModifiers::NONE)));
    assert!(
        app.pending_broadcast_enable,
        "enabling broadcast must be gated on a confirm popup"
    );
    assert!(!app.broadcast_input);
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.broadcast_input, "Enter must confirm");
    assert!(!app.pending_broadcast_enable);

    // Typed keys now reach every pane: both shells read the same line.
    for c in ['h', 'i'] {
        app.handle_terminal_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_terminal_key(key(KeyCode::Enter, KeyModifiers::NONE));
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the broadcast input to reach every pane",
        || {
            app.terminals
                .iter()
                .all(|t| t.grid_lines().0.iter().any(|l| l.contains("got-hi")))
        },
    );

    // Cmd+K Shift+I toggles the active pane out of the broadcast set.
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('I'), KeyModifiers::SHIFT)));
    assert!(app.terminals[0].broadcast_excluded);

    // Closing down to one pane drops broadcast (safety: never keep
    // broadcasting silently to a shrunk set).
    assert!(app.close_active_terminal());
    assert!(
        !app.broadcast_input,
        "broadcast must switch off when only one pane remains"
    );
}

#[test]
fn trigger_hits_surface_in_the_status_bar_via_the_drain_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.triggers = std::sync::Arc::new(crate::triggers::TriggerSet::from_json(
        r#"[ { "regex": "BUILD (\\w+)", "action": "notify", "message": "build: \\1" } ]"#,
    ));
    // The shell waits for input so the drain tick has pushed the trigger set
    // into the pane before the matching line is produced.
    let script = "read x; echo BUILD OK; sleep 30";
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from(script)],
        tmp.path(),
    )
    .unwrap();
    app.drain_terminal_bells();
    app.terminals[0].write_input(b"\n");
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the trigger to surface in the status line",
        || {
            app.drain_terminal_bells();
            app.status.starts_with("Trigger in")
        },
    );
    assert!(
        app.status.contains("build: OK"),
        "interpolated message expected, status: {}",
        app.status
    );
}

#[test]
fn leaving_the_terminal_closes_its_find_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.handle_terminal_key(key(KeyCode::Char('f'), KeyModifiers::SUPER));
    assert!(app.terminal_find.is_some());
    app.focus_pane(Pane::Editor);
    assert!(
        app.terminal_find.is_none(),
        "moving focus off the terminal must close the find bar so no stale highlight lingers"
    );
}

#[test]
fn paste_into_input_prompt_fills_it_not_the_focused_pane() {
    // Regression: opening an MCP command's URL prompt then pasting leaked the
    // URL into the focused terminal pane (the popup stayed empty). A bracketed
    // paste while a modal input prompt is open must fill the prompt.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.input_prompt = Some(crate::widgets::input_prompt::InputPrompt::new(
        crate::widgets::input_prompt::InputPurpose::McpArg {
            command_id: String::from("fetch.url"),
        },
        "Web: Fetch URL as Markdown",
        "URL to fetch",
    ));
    app.handle_paste("https://example.com/x");
    assert_eq!(
        app.input_prompt.as_ref().unwrap().value,
        "https://example.com/x",
        "a paste while the input prompt is open must fill the prompt, not leak to the focused pane"
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
fn a_cell_pixel_size_needs_the_terminal_to_report_pixels() {
    assert_eq!(cell_pixel_of(100, 40, 800, 640), Some((8, 16)));
    assert_eq!(cell_pixel_of(100, 40, 0, 0), None);
    assert_eq!(cell_pixel_of(0, 40, 800, 640), None);
}

#[test]
fn modified_terminal_keys_keep_their_modifiers() {
    let k = |c, m| key_to_bytes(key(c, m), false);
    assert_eq!(k(KeyCode::Backspace, KeyModifiers::ALT), b"\x1b\x7f");
    assert_eq!(k(KeyCode::Char(' '), KeyModifiers::CONTROL), vec![0x00]);
    assert_eq!(k(KeyCode::Char('/'), KeyModifiers::CONTROL), vec![0x1f]);
    assert_eq!(
        k(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        ),
        b"\x1b\x01"
    );
    assert_eq!(k(KeyCode::Left, KeyModifiers::CONTROL), b"\x1b[1;5D");
    assert_eq!(k(KeyCode::Right, KeyModifiers::SHIFT), b"\x1b[1;2C");
    assert_eq!(k(KeyCode::Home, KeyModifiers::CONTROL), b"\x1b[1;5H");
    assert_eq!(k(KeyCode::Delete, KeyModifiers::CONTROL), b"\x1b[3;5~");
    // Alt alone on Left/Right stays readline's word motion.
    assert_eq!(k(KeyCode::Left, KeyModifiers::ALT), b"\x1bb");
}

#[test]
fn key_to_bytes_arrows() {
    assert_eq!(
        key_to_bytes(key(KeyCode::Up, KeyModifiers::NONE), false),
        b"\x1b[A"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Down, KeyModifiers::NONE), false),
        b"\x1b[B"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Right, KeyModifiers::NONE), false),
        b"\x1b[C"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Left, KeyModifiers::NONE), false),
        b"\x1b[D"
    );
}

/// DECCKM on: cursor keys switch to the SS3 form; keys outside the cursor
/// group are unaffected.
#[test]
fn key_to_bytes_arrows_in_app_cursor_mode_use_ss3() {
    for (code, seq) in [
        (KeyCode::Up, b"\x1bOA"),
        (KeyCode::Down, b"\x1bOB"),
        (KeyCode::Right, b"\x1bOC"),
        (KeyCode::Left, b"\x1bOD"),
        (KeyCode::Home, b"\x1bOH"),
        (KeyCode::End, b"\x1bOF"),
    ] {
        assert_eq!(key_to_bytes(key(code, KeyModifiers::NONE), true), seq);
    }
    assert_eq!(
        key_to_bytes(key(KeyCode::PageUp, KeyModifiers::NONE), true),
        b"\x1b[5~"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Enter, KeyModifiers::NONE), true),
        b"\r"
    );
}

#[test]
fn key_to_bytes_enter_tab_backspace() {
    assert_eq!(
        key_to_bytes(key(KeyCode::Enter, KeyModifiers::NONE), false),
        b"\r"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Tab, KeyModifiers::NONE), false),
        b"\t"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Backspace, KeyModifiers::NONE), false),
        &[0x7f]
    );
}

#[test]
fn key_to_bytes_ctrl_letter_maps_to_control_byte() {
    let bytes = key_to_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false);
    assert_eq!(bytes, vec![0x03]);
    let bytes = key_to_bytes(key(KeyCode::Char('a'), KeyModifiers::CONTROL), false);
    assert_eq!(bytes, vec![0x01]);
    let bytes = key_to_bytes(key(KeyCode::Char('z'), KeyModifiers::CONTROL), false);
    assert_eq!(bytes, vec![0x1a]);
}

#[test]
fn key_to_bytes_alt_letter_prefixes_esc() {
    let bytes = key_to_bytes(key(KeyCode::Char('x'), KeyModifiers::ALT), false);
    assert_eq!(bytes, vec![0x1b, b'x']);
}

#[test]
fn key_to_bytes_alt_left_emits_esc_b_for_readline_backward_word() {
    let bytes = key_to_bytes(key(KeyCode::Left, KeyModifiers::ALT), false);
    assert_eq!(
        bytes, b"\x1bb",
        "Option+Left in the terminal must send ESC b so zsh/bash readline runs backward-word"
    );
}

#[test]
fn key_to_bytes_alt_right_emits_esc_f_for_readline_forward_word() {
    let bytes = key_to_bytes(key(KeyCode::Right, KeyModifiers::ALT), false);
    assert_eq!(
        bytes, b"\x1bf",
        "Option+Right in the terminal must send ESC f so zsh/bash readline runs forward-word"
    );
}

#[test]
fn key_to_bytes_plain_arrows_unchanged_when_alt_not_held() {
    assert_eq!(
        key_to_bytes(key(KeyCode::Left, KeyModifiers::NONE), false),
        b"\x1b[D"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Right, KeyModifiers::NONE), false),
        b"\x1b[C"
    );
}

#[test]
fn key_to_bytes_plain_char_utf8() {
    assert_eq!(
        key_to_bytes(key(KeyCode::Char('a'), KeyModifiers::NONE), false),
        b"a"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::Char('é'), KeyModifiers::NONE), false),
        "é".as_bytes()
    );
}

#[test]
fn key_to_bytes_function_keys() {
    assert_eq!(
        key_to_bytes(key(KeyCode::F(1), KeyModifiers::NONE), false),
        b"\x1bOP"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::F(5), KeyModifiers::NONE), false),
        b"\x1b[15~"
    );
    assert_eq!(
        key_to_bytes(key(KeyCode::F(12), KeyModifiers::NONE), false),
        b"\x1b[24~"
    );
}

#[test]
fn key_to_bytes_unknown_returns_empty() {
    assert!(key_to_bytes(key(KeyCode::CapsLock, KeyModifiers::NONE), false).is_empty());
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
    await_terminal_probe(&mut app, &probe);
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

/// User-visible regression (issue #23): on mobile (Termux) a
/// multiline drag-selection that begins inside the terminal pane and
/// then travels *up* past the terminal's top edge into the editor
/// region must stay local to the terminal. The terminal sits below the
/// editor in the split layout, so a drag headed upward crosses into the
/// editor's rectangle. Before the fix, the editor pane reacted to the
/// out-of-bounds drag and started its own selection, so the user saw
/// text highlighted in BOTH the terminal and the left editor pane. The
/// drag must only ever extend the terminal's own selection (clamped to
/// the terminal grid) and never seed an editor selection.
#[test]
fn multiline_terminal_drag_above_the_pane_does_not_bleed_into_the_editor() {
    let tmp = tempfile::tempdir().unwrap();
    // Seed and open a real file so the editor claims a non-empty rect
    // directly above the terminal — the region the upward drag enters.
    let f = tmp.path().join("probe.txt");
    std::fs::write(&f, "alpha\nbeta\ngamma\ndelta\nepsilon\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();

    // Render once in the split layout (editor on top, terminal on the
    // bottom, sharing the same column band).
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let term_inner = app.terminal().last_inner;
    let editor_area = app.editor.last_area;
    assert!(
        term_inner.width > 4 && term_inner.height > 2,
        "precondition: the split terminal must own a real rect"
    );
    assert!(
        editor_area.height > 0 && editor_area.y < term_inner.y,
        "precondition: the editor pane must sit above the terminal so the upward drag crosses into it"
    );

    // The user first has an active selection in the editor pane (e.g. left
    // over from editing). Seed it so the editor owns a real selection band
    // that would render alongside any new terminal selection.
    app.focus_pane(Pane::Editor);
    app.editor.start_selection_at_cursor();
    app.editor.move_right();
    app.editor.move_right();
    app.editor.extend_selection_to_cursor();
    assert!(
        app.editor.selection.is_some_and(|s| s.has_area()),
        "precondition: the editor must hold an active selection before the terminal gesture"
    );

    // Down inside the terminal, a couple of rows below its top edge so
    // there is vertical room to drag upward out of the pane.
    let click_col = term_inner.x + 3;
    let click_row = term_inner.y + 2;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: click_col,
        row: click_row,
        modifiers: KeyModifiers::NONE,
    });
    // Drag UP into the editor region (row above the terminal's top edge)
    // and toward the left — the gesture that used to overflow the pane.
    let drag_col = term_inner.x + 1;
    let drag_row = editor_area.y + 1; // well inside the editor's rectangle
    assert!(
        drag_row < term_inner.y,
        "precondition: the drag end-point must land above the terminal pane"
    );
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: drag_col,
        row: drag_row,
        modifiers: KeyModifiers::NONE,
    });

    // The editor must NOT have started a selection of its own.
    assert!(
        app.editor.selection.is_none_or(|s| !s.has_area()),
        "a terminal drag that overflowed upward must not seed an editor selection - that is the cross-pane bleed reported in issue #23"
    );

    // The terminal's selection head must be clamped to the terminal's
    // own grid: with no scrollback the topmost reachable viewport line
    // is 0 and the column stays within the inner width.
    let sel = app
        .terminal()
        .selection()
        .expect("the terminal Down must have seeded a selection");
    let (head_line, head_col) = sel.head;
    assert!(
        head_line >= 0 && (head_line as u16) < term_inner.height,
        "terminal selection head line {head_line} must clamp inside the terminal viewport (0..{})",
        term_inner.height
    );
    assert!(
        head_col < term_inner.width,
        "terminal selection head col {head_col} must clamp inside the terminal inner width {}",
        term_inner.width
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
        await_terminal_probe(&mut app, &probe);
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
        await_terminal_probe(app, &probe);
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

/// Issue #226: the probe command must not spell the probe out, or the
/// shell's echo of it becomes a second row that matches the grid search
/// (and wins the race under load). Pins both halves of `probe_command`:
/// the echoed text can't contain the probe, and running it still puts the
/// probe on screen.
#[test]
fn the_terminal_probe_command_prints_the_probe_without_echoing_it() {
    let probe = format!("croft-probe-echo-{}", std::process::id());
    let cmd = probe_command(&probe);
    assert!(
        !cmd.contains(&probe),
        "the typed command line must not contain the probe verbatim, or the shell echo matches the grid search alongside the output; got {cmd:?}"
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    app.terminal_mut().focused = true;
    term.draw(|f| app.render(f)).unwrap();
    // Runs the very command asserted on above, on the same budget every other
    // probe site uses.
    await_terminal_probe(&mut app, &probe);
    let snap = app.terminal().visible_text();
    let rows: Vec<&str> = snap.lines().filter(|l| l.contains(&probe)).collect();
    assert_eq!(
        rows.len(),
        1,
        "exactly one row may carry the probe (the command's output); got {rows:?}"
    );
    assert_eq!(
        rows[0].trim(),
        probe,
        "the matching row must be the printed probe alone, not a command line quoting it"
    );
}

/// User-reported root cause: with the Search sidebar open, Cmd+C /
/// Cmd+V in the terminal silently no-op because the search-editing
/// shortcut guard fired on `focus != Editor`, swallowing the keys
/// into the (empty) search query instead of the focused terminal.
/// Copying from the terminal must work regardless of which sidebar
/// view is showing.
#[test]
fn terminal_cmd_c_copies_even_when_search_sidebar_is_open() {
    // Cross-platform: the assertion reads back via `clipboard::read_string()`,
    // so the per-thread mock validates the Cmd+C-routing logic on every OS.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    term.draw(|f| app.render(f)).unwrap();

    let probe = format!("croft-search-open-{}", std::process::id());
    await_terminal_probe(&mut app, &probe);
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
fn dtach_reattach_mode_reassert_reenables_mouse_tracking() {
    // dtach `-r winch` delivers only a SIGWINCH on reattach: the newly
    // attached terminal never received the startup DECSETs, so without a
    // re-assert the mouse is dead after every reconnect to a persisted
    // session (0.1.610 bug on `various`). The Resize-time sequence must
    // re-enable the alt screen, mouse tracking with SGR encoding, and
    // bracketed paste, and must SET (not push) the kitty keyboard flags so
    // repeated WINCHes don't leak stack entries the single exit pop can't
    // unwind.
    let s = String::from_utf8(mode_reassert_seq()).unwrap();
    for m in ["\x1b[?1049h", "\x1b[?1000h", "\x1b[?1006h", "\x1b[?2004h"] {
        assert!(s.contains(m), "reattach re-assert missing {m:?}");
    }
    // Structural guarantee, not just spot checks: the re-assert embeds the
    // startup takeover block verbatim, so a mode added at startup (via
    // takeover_mode_seq) can never be missing from the reattach path.
    let takeover = String::from_utf8(takeover_mode_seq()).unwrap();
    assert!(
        s.starts_with(&takeover),
        "mode_reassert_seq must embed takeover_mode_seq verbatim"
    );
    let set_kbd = format!("\x1b[={};1u", keyboard_enhancement_flags().bits());
    assert!(s.contains(&set_kbd), "kitty keyboard flags must be re-SET");
    assert!(
        !s.contains("\x1b[>"),
        "a push (CSI > flags u) per WINCH would leak stack entries"
    );
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
        app.workspace_root(),
        target_dir,
        "workspace_root must still flip to the new path"
    );
}

#[test]
fn change_workspace_root_skips_cd_when_terminal_runs_a_foreground_app() {
    let tmp = tempfile::tempdir().unwrap();
    let target_dir = tmp.path().join("seeded");
    std::fs::create_dir(&target_dir).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Put the active terminal into a long-lived foreground command.
    // 60s, not 10 (#169): under a loaded suite this test thread can be
    // descheduled >10s between the busy precondition and the re-root —
    // the sleep then exits and the shell GENUINELY reclaims the tty, so
    // the seed was correct and the assert wrong. Pane Drop kills the
    // shell tree, so nothing outlives the test.
    //
    // Proving the sleep is REALLY the foreground command needs more than
    // pgrp samples (#186): rc-startup children also own the tty, so a
    // resolved non-shell reading can be rc — the input above then sits as
    // type-ahead and the re-root races the first prompt. The marker's
    // OUTPUT is the proof: quote-split so the echoed command line cannot
    // match, it can only be printed by the shell's MAIN loop executing
    // the line — rc is done, and the very next resolved non-shell sample
    // can only be the sleep.
    app.terminal_mut()
        .write_input(b"echo GO''-MARKER; sleep 60\n");
    // Load-scaled (#397): the hand-rolled 8000ms loop this replaced did not
    // stretch under load at all, and its failure said only "marker output
    // never arrived" — true of a starved shell and a broken one alike.
    // `await_spawned` names what it waited for and how long it actually had.
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the shell's main loop to print the marker",
        || {
            let (lines, _) = app.terminal().grid_lines();
            lines
                .iter()
                .any(|l| l.contains("GO-MARKER") && !l.contains("echo"))
        },
    );
    // `Some(false)` is a kernel-confirmed foreign foreground group, and
    // with rc ruled out above it can only be the sleep. The fail-closed
    // retry inside `foreground_is_shell` can answer false on an
    // UNRESOLVED sample, so poll the strict accessor, not the policy one.
    // Retain the loop's own sample for the assert: re-sampling could see
    // a transient `None` (a failed tcgetpgrp) after the loop already
    // observed the confirmed reading.
    // Load-scaled like the marker wait above (#397): a shell that wins the
    // marker race inside a stretched budget could still lose a fixed 4000ms
    // here, and this one does not assert on timeout — it falls through to
    // the precondition assert below, which then reads as a broken
    // foreground-group check rather than a starved box. Captured from
    // inside the closure so the assert keeps the sample the wait observed.
    let mut foreground = None;
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the kernel to report a foreign foreground group (the sleep)",
        || {
            foreground = app.terminal().foreground_resolved_for_test();
            foreground == Some(false)
        },
    );
    // `await_spawned` panics rather than returning on timeout, so this can
    // only be `Some(false)`; it pins the sample the wait observed rather
    // than guarding the wait. Same shape as `wait_for_conflicted_entry`.
    assert_eq!(
        foreground,
        Some(false),
        "await_spawned returns only once the kernel reports the foreign group"
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
        app.workspace_root(),
        target_dir,
        "the Explorer root must still flip even when the shell cannot be synced"
    );
}

#[test]
fn change_workspace_root_seeds_cd_while_shell_startup_owns_the_tty() {
    let tmp = tempfile::tempdir().unwrap();
    let target_dir = tmp.path().join("seeded");
    std::fs::create_dir(&target_dir).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Reproduce the #94 window deterministically: a shell whose rc startup
    // holds the tty's foreground process group before the first prompt.
    // `set -m` turns job control on, so the foreground `sleep` gets its own
    // process group and the tty — exactly the state an rc-startup child
    // leaves the pane in under full-suite load.
    let script = tmp.path().join("slow-startup.sh");
    std::fs::write(&script, "#!/bin/bash\nset -m\nsleep 30\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    app.terminals[app.active_terminal] = crate::widgets::terminal::PtyTerminal::new_running(
        script.to_str().unwrap(),
        &[],
        tmp.path(),
    )
    .unwrap();

    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the startup child to take the foreground group from the shell",
        || !app.terminal().foreground_is_shell(),
    );
    // await_spawned panics on timeout, so this pins the state the rest of
    // the test reads rather than guarding the wait, the same shape as
    // wait_for_conflicted_entry.
    assert!(
        !app.terminal().foreground_is_shell(),
        "precondition: the startup child must own the foreground group"
    );

    let _ = app.terminal_mut().take_dirty();
    app.change_workspace_root(target_dir.clone());

    assert!(
        app.terminal_mut().take_dirty(),
        "a Make Root during shell startup must still seed the cd: nothing has ever been \
         written into the pane, so a non-shell foreground group can only be shell startup \
         and the seed is type-ahead for the first prompt"
    );
    assert!(
        !app.status.contains("terminal busy"),
        "the startup window must not be reported as a busy terminal; status was: {}",
        app.status
    );
}

#[test]
fn ctrl_k_leads_off_macos_except_in_the_shell_or_the_vim_editor() {
    // Super reaches croft only over the kitty keyboard protocol, so off macOS
    // Ctrl+K arms the leader too, as in VS Code's Linux keymap (#843). The
    // shell keeps it for readline's kill-line and vim mode for the editor's.
    assert!(ctrl_k_leads(false, false, false));
    assert!(
        !ctrl_k_leads(false, true, false),
        "a focused shell keeps Ctrl+K"
    );
    assert!(
        !ctrl_k_leads(false, false, true),
        "the vim editor keeps kill-line"
    );
    assert!(
        !ctrl_k_leads(true, false, false),
        "macOS: Ctrl+K kills, Cmd leads"
    );
    let ctrl_k = key(KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert!(is_cmd_k_leader_key(ctrl_k, true));
    // Off Termux (the test process), a Ctrl+K that does not lead falls
    // through to the editor's kill-to-end-of-line.
    assert!(!is_cmd_k_leader_key(ctrl_k, false));
    for ctrl_leads in [false, true] {
        let super_k = key(KeyCode::Char('k'), KeyModifiers::SUPER);
        assert!(
            is_cmd_k_leader_key(super_k, ctrl_leads),
            "Cmd+K always leads"
        );
        let ctrl_shift_k = key(
            KeyCode::Char('K'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert!(
            !is_cmd_k_leader_key(ctrl_shift_k, ctrl_leads),
            "Ctrl+Shift+K stays Delete Line"
        );
    }
}

#[test]
fn terminal_clear_from_the_palette_empties_the_active_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.active_terminal = 1;
    await_terminal_probe(&mut app, "cleared-1562");
    app.focus_pane(Pane::Editor);
    assert!(run_from_palette(&mut app, "Terminal: Clear"));
    assert!(
        !app.terminals[1].visible_text().contains("cleared-1562"),
        "{}",
        app.terminals[1].visible_text()
    );
    assert_eq!(app.status, "Cleared terminal");
}

#[test]
fn terminal_modes_from_the_palette_focus_the_terminal() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    draw(&mut app, 100, 30);
    app.focus_pane(Pane::Editor);
    app.run_command(Command::TerminalCopyMode);
    assert!(app.terminal_copy_mode.is_some());
    assert!(app.focus == Pane::Terminal);
    app.terminal_copy_mode = None;
    app.focus_pane(Pane::Editor);
    app.run_command(Command::TerminalCommandHistory);
    assert!(app.command_history_popup.is_some());
    assert!(app.focus == Pane::Terminal);
}

/// #1562: a terminal mode opened from the palette while another
/// bottom-panel tab is in front brings the Terminal tab forward, or the
/// mode would be hidden and its keys would go to that tab.
#[test]
fn terminal_modes_from_the_palette_bring_the_terminal_tab_forward() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    draw(&mut app, 100, 30);
    for cmd in [
        Command::TerminalCopyMode,
        Command::TerminalQuickSelect,
        Command::TerminalCommandHistory,
    ] {
        app.terminal_copy_mode = None;
        app.command_history_popup = None;
        app.set_bottom_panel_tab(BottomPanelTab::Output);
        app.focus_pane(Pane::Editor);
        app.run_command(cmd);
        assert!(app.bottom_panel_tab == BottomPanelTab::Terminal, "{cmd:?}");
        assert!(app.focus == Pane::Terminal, "{cmd:?}");
    }
}

/// #1562: a terminal menu row shows the chord the user bound to its
/// command, and its built-in one otherwise.
#[test]
fn terminal_menu_rows_show_the_user_s_chord() {
    // The built-in chords as this platform spells them (#885).
    let (clear, rename) = if cfg!(target_os = "macos") {
        ("⌘K K", "⌘K R")
    } else {
        ("Ctrl+K K", "Ctrl+K R")
    };
    let none = crate::keymap::Keymap::from_json("[]");
    assert_eq!(
        menu_hint(&MenuAction::ClearTerminal(0), &none).as_deref(),
        Some(clear)
    );
    let bound = crate::keymap::Keymap::from_json(
        r#"[{ "key": "ctrl+alt+k", "command": "terminal_clear" }]"#,
    );
    assert_eq!(
        menu_hint(&MenuAction::ClearTerminal(0), &bound).as_deref(),
        Some("ctrl+alt+k")
    );
    assert_eq!(
        menu_hint(&MenuAction::RenameTerminal(0), &bound).as_deref(),
        Some(rename),
        "only the rebound row changes"
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

/// #538, half one: Cmd+C inside a program that tracks the mouse.
///
/// Claude Code turns mouse tracking on, so a left-drag inside it is FORWARDED
/// to the child and croft deliberately clears its own selection (#474). Cmd+C
/// then found no selection and returned in SILENCE — the user pressed copy,
/// nothing reached the clipboard, and nothing said why or what would work.
#[test]
fn cmd_c_inside_a_mouse_tracking_program_says_why_nothing_was_copied() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);
    // The child explicitly REQUESTS click and drag tracking (?1002 button-drag,
    // ?1006 SGR), which is what makes the drag below the child's rather than
    // croft's. A program that never asks still selects on a plain drag.
    app.terminals[0].feed_bytes_for_test(b"\x1b[?1002h\x1b[?1006h");
    assert!(
        app.terminals[0].mouse_reporting(),
        "precondition: the child is tracking the mouse"
    );
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let inner = app.terminals[0].last_inner;

    // The user drags across the TUI's output, which is the child's gesture.
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        inner.x + 2,
        inner.y + 1,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        inner.x + 12,
        inner.y + 1,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        inner.x + 12,
        inner.y + 1,
    ));
    assert!(
        !app.terminals[0].selection().is_some_and(|s| s.has_area()),
        "precondition: the drag belonged to the child, so croft holds no selection"
    );

    app.status.clear();
    app.handle_key(key(KeyCode::Char('c'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        !app.status.is_empty(),
        "Cmd+C that copies nothing must say so rather than fail silently"
    );
    assert!(
        app.status.contains("Shift"),
        "and must name the gesture that DOES select inside a tracking program, got {:?}",
        app.status
    );
}

/// Run `copy` on a relay session and return what it sent home to the local
/// clipboard, or `None` when nothing went over the relay.
fn relay_copied(app: &mut App, copy: impl FnOnce(&mut App)) -> Option<String> {
    let home = tempfile::tempdir().unwrap();
    app.is_relay_session = true;
    let (log, inbox) = with_relay_home(home.path(), || {
        let log = app.relay_log_path().expect("relay log path derivable");
        let inbox = app.relay_inbox_path().expect("relay inbox path derivable");
        copy(app);
        (log, inbox)
    });
    let written = std::fs::read_to_string(&log).ok()?;
    let id = written.lines().last()?.strip_prefix("copy\t")?.to_string();
    std::fs::read_to_string(inbox.join(format!("{id}.txt"))).ok()
}

/// A pane whose script emits one OSC 133 command printing `out-1667`, and
/// that command's decoration.
fn app_with_one_finished_command(
    tmp: &std::path::Path,
) -> (App, crate::widgets::terminal::CommandDecoration) {
    let mut app = App::new(tmp.to_path_buf()).unwrap();
    let script = "x=out; printf '\\033]133;A\\007$ \\033]133;B\\007cmd\\n\\033]133;C\\007'\"$x\"'-1667\\n\\033]133;D;0\\007'; sleep 30";
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), script.into()],
        tmp,
    )
    .unwrap();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the shell to finish its command",
        || !app.terminals[0].command_decorations().is_empty(),
    );
    let deco = app.terminals[0].command_decorations()[0];
    (app, deco)
}

/// #1667: Copy Output on a relay session sends the output home to the
/// clipboard the user pastes from, like a selection copy does (#538). It
/// wrote the remote box's clipboard, which nothing on the user's machine can
/// paste from.
#[test]
fn copy_output_on_a_relay_session_reaches_the_local_clipboard() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, deco) = app_with_one_finished_command(tmp.path());
    let sent = relay_copied(&mut app, |app| app.copy_command_output(0, deco));
    assert_eq!(sent.as_deref().map(str::trim_end), Some("out-1667"));
    assert_eq!(
        app.status,
        "Sent command output (1 line) to the local clipboard"
    );
}

/// #1667: copy mode, quick select and copy on select go home on a relay
/// session too, so no terminal copy is left writing the remote box.
#[test]
fn every_terminal_copy_on_a_relay_session_reaches_the_local_clipboard() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let script = "x=de40; printf \"sha ${x}bdf12ab34\\n\"; sleep 60";
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from(script)],
        tmp.path(),
    )
    .unwrap();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the pane output \"de40bdf12ab34\"",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.starts_with("sha de40bdf12ab34"))
        },
    );
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    // Quick select: commit the hash's label.
    let sent = relay_copied(&mut app, |app| {
        app.handle_terminal_key(key(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        let label = app
            .terminal_quick_select
            .as_ref()
            .expect("Ctrl+Shift+Space enters quick select")
            .hints
            .iter()
            .find(|h| h.text == "de40bdf12ab34")
            .expect("the hash is labelled")
            .label
            .clone();
        for c in label.chars() {
            app.handle_terminal_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
    });
    assert_eq!(sent.as_deref(), Some("de40bdf12ab34"), "quick select");
    assert!(
        app.status.ends_with("to the local clipboard"),
        "{}",
        app.status
    );

    // Copy mode: a line selection on the hash's row, then y.
    let (lines, _) = app.terminals[0].grid_lines();
    let row = lines
        .iter()
        .position(|l| l.starts_with("sha de40bdf12ab34"))
        .unwrap();
    let sent = relay_copied(&mut app, |app| {
        app.handle_terminal_key(key(
            KeyCode::Char('y'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert!(
            app.terminal_copy_mode.is_some(),
            "Ctrl+Shift+Y enters copy mode"
        );
        app.handle_terminal_key(key(KeyCode::Char('g'), KeyModifiers::NONE));
        for _ in 0..row {
            app.handle_terminal_key(key(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        app.handle_terminal_key(key(KeyCode::Char('V'), KeyModifiers::NONE));
        app.handle_terminal_key(key(KeyCode::Char('y'), KeyModifiers::NONE));
    });
    assert_eq!(
        sent.as_deref().map(str::trim_end),
        Some("sha de40bdf12ab34"),
        "copy mode"
    );
    assert!(
        app.status.ends_with("to the local clipboard"),
        "{}",
        app.status
    );

    // Copy on select: a finished drag over the hash's row.
    app.copy_on_select = true;
    let inner = app.terminals[0].last_inner;
    let (lines, top) = app.terminals[0].grid_lines();
    let vrow = (top
        + lines
            .iter()
            .position(|l| l.starts_with("sha de40bdf12ab34"))
            .unwrap() as i32) as u16;
    app.terminals[0].start_selection_at(inner.x, inner.y + vrow);
    app.terminals[0].extend_selection_to(inner.x + 2, inner.y + vrow);
    let expected = app.terminals[0].selection_text();
    assert!(expected.starts_with("sh"), "precondition: {expected:?}");
    let sent = relay_copied(&mut app, |app| {
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            inner.x + 2,
            inner.y + vrow,
        ));
    });
    assert_eq!(sent, Some(expected), "copy on select");
}

/// #1667 negative: off a relay session Copy Output still writes this
/// machine's clipboard and queues nothing for a relay.
#[test]
fn copy_output_off_a_relay_session_writes_this_machines_clipboard() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (mut app, deco) = app_with_one_finished_command(tmp.path());
    assert!(!app.is_relay_session);
    let log = with_relay_home(home.path(), || {
        app.copy_command_output(0, deco);
        app.relay_log_path().expect("relay log path derivable")
    });
    assert!(!log.exists(), "nothing goes over a relay");
    let clip = crate::clipboard::read_string().unwrap_or_default();
    assert_eq!(clip.trim_end(), "out-1667");
    assert!(
        app.status.starts_with("Copied command output (1 line)"),
        "{}",
        app.status
    );
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
fn clicking_restore_button_returns_to_split_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.terminal_pane_maximized = true;
    let backend = ratatui::backend::TestBackend::new(180, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let r = app.terminal_max_buttons[app.active_terminal];
    assert!(r.width > 0, "the maximized pane paints a restore button");
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: r.x + 1,
        row: r.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        !app.terminal_pane_maximized,
        "clicking the restore button leaves maximize mode"
    );
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.terminals.iter().all(|t| t.last_area.width > 0),
        "all panes are visible again after restore"
    );
    assert!(
        app.terminal_rail_rects.is_empty(),
        "the rail disappears with the split restored"
    );
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

// #865 guard: a bare F9 in a focused terminal with no debug session belongs
// to the shell (htop's F9 kill) even with an update ready, where it used to
// relaunch croft; it toggles no breakpoint either.
#[test]
fn bare_f9_in_a_focused_terminal_is_the_shells_even_with_an_update_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.update_status = UpdateStatus::Ready;
    let _ = app.handle_key(key(KeyCode::F(9), KeyModifiers::NONE));
    assert!(!app.pending_reexec && !app.quit, "croft did not relaunch");
    assert!(
        app.editor.breakpoints.is_empty(),
        "nor toggled a breakpoint"
    );
}

/// Negative (#1630): the block still runs from the pane's directory, so a
/// module beside the document imports as it did through the heredoc.
#[test]
fn a_python_block_still_imports_from_the_pane_s_directory() {
    if !(crate::markdown::runnable_interpreter("python") == Some("python3")) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("helper_1630.py"), "X = 'local'\n").unwrap();
    let scripts = tempfile::tempdir().unwrap();
    let cmd = super::fence_command(
        "python3",
        "import helper_1630\nprint(helper_1630.X)\n",
        scripts.path(),
        "/bin/sh",
    );
    assert_eq!(run_typed_line(&cmd, tmp.path()), "local\n");
}

/// #360: quick-select offers no hint inside a masked span (a hex run or a
/// number there is a fragment of the secret), while hints elsewhere on
/// the screen survive. The masked token here is itself a hex run at a
/// word boundary, so without the filter it WOULD be a hint.
#[test]
fn quick_select_skips_hints_inside_masked_spans() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.triggers = std::sync::Arc::new(crate::triggers::TriggerSet::from_json(
        r#"[{ "regex": "token \\S+", "action": "redact" }]"#,
    ));
    let term = crate::widgets::terminal::PtyTerminal::new_with_transcript(
        tmp.path(),
        &[
            String::from("token deadbeefcafe0123 end"),
            String::from("commit cafebabe0123456 fixed it"),
        ],
    )
    .unwrap();
    app.insert_terminal(term);
    app.focus_pane(Pane::Terminal);
    app.open_terminal_quick_select();
    let state = app
        .terminal_quick_select
        .as_ref()
        .expect("quick select opened");
    let texts: Vec<&str> = state.hints.iter().map(|h| h.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("cafebabe0123456")),
        "the ordinary hash is still a hint: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("deadbeefcafe0123")),
        "the hex run inside the masked token is not a hint: {texts:?}"
    );
}

#[test]
fn undo_close_restores_a_closed_terminal_pane_with_its_process_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mk = || {
        crate::widgets::terminal::PtyTerminal::new_running(
            "/bin/sh",
            &[
                String::from("-c"),
                String::from("read x; echo revived-$x; sleep 30"),
            ],
            tmp.path(),
        )
        .unwrap()
    };
    app.terminals[0] = mk();
    app.terminals.push(mk());
    app.terminals[1].set_manual_name(Some(String::from("keepme")));
    app.active_terminal = 1;
    app.focus_pane(Pane::Terminal);

    assert!(app.close_active_terminal());
    assert_eq!(app.terminals.len(), 1);

    // Cmd+K Shift+T brings the pane back: same slot, same name, and the
    // child process never died in the grace window.
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('T'), KeyModifiers::SHIFT)));
    assert_eq!(app.terminals.len(), 2, "undo close must restore the pane");
    assert_eq!(app.terminals[1].label(), "keepme");
    assert_eq!(app.active_terminal, 1, "the restored pane takes focus");
    app.terminals[1].write_input(b"back\n");
    // The same operation and the same old constant as the wait in
    // `terminal_session_restores_pane_layout_names_and_focus_across_restarts`:
    // a shell echoing one line, waited on under whatever load the suite is
    // running. Same 2s base, so the same 8s floor.
    crate::test_budget::await_spawned(
        crate::test_budget::tests::RESTORED_SHELL_BASE,
        "the reopened pane's shell to answer",
        || {
            app.terminals[1]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("revived-back"))
        },
    );

    // Past the grace window the parked pane is dropped for real: the tick
    // reaps it and undo has nothing left to restore.
    assert!(app.close_active_terminal());
    for parked in &mut app.closed_terminals {
        parked.closed_at =
            std::time::Instant::now() - UNDO_CLOSE_GRACE - std::time::Duration::from_secs(1);
    }
    app.drain_terminal_bells();
    assert!(
        app.closed_terminals.is_empty(),
        "expired parked panes must be reaped by the tick"
    );
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('T'), KeyModifiers::SHIFT)));
    assert_eq!(app.terminals.len(), 1, "nothing to restore after expiry");
}

#[test]
fn copy_mode_navigates_scrollback_with_vi_keys_and_copies_char_line_and_block() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Row 0 is the deterministic `▶ …` run-label header; the printf rows
    // land on rows 1..=3. Lines are short so nothing wraps at 80 cols.
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            // Sentinels are assembled at runtime so the pane's `▶ …` header
            // (which echoes the command text) can never match the waits.
            String::from("x=a; y=b; z=c; printf \"${x}aaa1\\n${y}bbb2\\n${z}cc3\\n\"; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the pane output \"ccc3\"",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("ccc3"))
        },
    );
    let enter_copy_mode = key(
        KeyCode::Char('y'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    let press = |app: &mut App, code: KeyCode| {
        app.handle_terminal_key(key(code, KeyModifiers::NONE));
    };
    // The header may wrap, so locate "aaaa1" by content: its distance from
    // the grid top is the number of j presses after g.
    let (lines, top) = app.terminals[0].grid_lines();
    let hops = lines
        .iter()
        .position(|l| l.starts_with("aaaa1"))
        .expect("printf output must be on the grid");
    assert!(top <= 0, "no scrollback expected in this tiny session");
    let down = |app: &mut App, n: usize| {
        for _ in 0..n {
            app.handle_terminal_key(key(KeyCode::Char('j'), KeyModifiers::NONE));
        }
    };

    // Char selection: g to the grid top, j down to "aaaa1", v at col 0,
    // $ to the line's last glyph, y copies and leaves copy mode.
    app.handle_terminal_key(enter_copy_mode);
    assert!(
        app.terminal_copy_mode.is_some(),
        "Ctrl+Shift+Y must enter copy mode"
    );
    press(&mut app, KeyCode::Char('g'));
    down(&mut app, hops);
    press(&mut app, KeyCode::Char('0'));
    press(&mut app, KeyCode::Char('v'));
    press(&mut app, KeyCode::Char('$'));
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(
        crate::clipboard::read_string().unwrap_or_default(),
        "aaaa1",
        "v + $ + y must copy the whole first output line"
    );
    assert!(app.terminal_copy_mode.is_none(), "y exits copy mode");

    // Line selection: V spans full rows regardless of cursor column.
    app.handle_terminal_key(enter_copy_mode);
    press(&mut app, KeyCode::Char('g'));
    down(&mut app, hops);
    press(&mut app, KeyCode::Char('l'));
    press(&mut app, KeyCode::Char('l'));
    press(&mut app, KeyCode::Char('V'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(
        crate::clipboard::read_string().unwrap_or_default(),
        "aaaa1\nbbbb2",
        "V + j + y must copy both full lines"
    );

    // Block selection: a 2-wide, 3-tall rectangle from the output rows.
    app.handle_terminal_key(enter_copy_mode);
    press(&mut app, KeyCode::Char('g'));
    down(&mut app, hops);
    press(&mut app, KeyCode::Char('0'));
    app.handle_terminal_key(key(KeyCode::Char('v'), KeyModifiers::CONTROL));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('l'));
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(
        crate::clipboard::read_string().unwrap_or_default(),
        "aa\nbb\ncc",
        "Ctrl+V must cut a rectangular column slice"
    );

    // Esc exits and drops the highlight.
    app.handle_terminal_key(enter_copy_mode);
    press(&mut app, KeyCode::Char('v'));
    press(&mut app, KeyCode::Esc);
    assert!(app.terminal_copy_mode.is_none(), "Esc exits copy mode");
    assert!(
        app.terminals[0].selection().is_none(),
        "leaving copy mode must clear the selection highlight"
    );
}

#[test]
fn capture_triggers_collect_into_the_captures_panel_and_jump_to_the_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.triggers = std::sync::Arc::new(crate::triggers::TriggerSet::from_json(
        r#"[ { "regex": "err-(\\d+)", "action": "capture", "message": "boom \\1" } ]"#,
    ));
    // Read-gated so the drain tick pushes the trigger set into the pane
    // before the matching line prints; the sentinel is assembled at runtime
    // so the `▶ …` header can never fire the trigger itself.
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("read x; e=err; echo ${e}-7 broke; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    app.drain_terminal_bells();
    app.terminals[0].write_input(b"\n");
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the error line to be captured",
        || {
            app.drain_terminal_bells();
            !app.captures.is_empty()
        },
    );
    let entry = app.captures.selected_entry().expect("one capture");
    assert_eq!(entry.message, "boom 7");
    assert!(
        entry.line.contains("err-7 broke"),
        "the whole line is kept: {:?}",
        entry.line
    );

    // Enter on the row jumps back to the pane and selects the matched line.
    app.captures_open_selected();
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Terminal);
    let sel = app.terminals[0].selection_text();
    assert!(
        sel.contains("err-7 broke"),
        "jump must land the selection on the captured line, got {sel:?}"
    );

    // `n` asks the Navigator about the line (#372); with none seated it
    // says so rather than doing nothing, and the palette carries the same
    // command.
    app.ask_navigator_about_capture();
    assert!(
        app.status.to_lowercase().contains("not active"),
        "{}",
        app.status
    );
    assert!(
        crate::widgets::command_palette::ALL_COMMANDS
            .contains(&crate::widgets::command_palette::Command::AskNavigatorAboutCapture)
    );
    assert_eq!(
        crate::widgets::command_palette::Command::AskNavigatorAboutCapture.id(),
        "captures_ask_navigator"
    );
}

#[test]
fn change_workspace_root_restores_the_incoming_workspaces_layout_when_the_panel_is_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    let b_sub = b.join("sub");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::create_dir(&b_sub).unwrap();
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
                    cwd: b_sub.display().to_string(),
                    name: Some(String::from("srv")),
                    transcript: Vec::new(),
                    lane: None,
                },
            ],
            active: 1,
        },
    )
    .unwrap();

    let mut app = App::new(a.clone()).unwrap();
    app.terminal_session_path = session_path;
    // As over SSH, where plain panes come back (#1458); locally they stay
    // closed, which `a_local_re_root_keeps_one_terminal_for_ended_panes`
    // covers.
    app.terminal_restore_respawns = true;
    // The default pane is untouched: no input, no name, no launched
    // program — exactly the startup state restore replaces at launch.
    app.change_workspace_root(b.clone());

    assert_eq!(
        app.terminals.len(),
        2,
        "re-rooting an untouched panel must bring back the incoming workspace's saved layout"
    );
    assert_eq!(app.terminals[1].label(), "srv", "names come back with it");
    assert_eq!(app.active_terminal, 1, "focus lands on the recorded pane");
}

#[test]
fn restoring_a_workspaces_layout_drops_pane_bound_state_from_the_outgoing_panel() {
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
    // As over SSH, where plain panes come back (#1458).
    app.terminal_restore_respawns = true;
    // Park an undo-close entry (the split writes no input into pane 0, so
    // the panel stays pristine) and open copy mode on the surviving pane:
    // both are bound to the OUTGOING panel and keyed by pane index, which
    // the restored panel's identical indexes would wrongly satisfy.
    app.split_terminal().unwrap();
    assert!(app.close_terminal_at(1));
    assert!(!app.closed_terminals.is_empty());
    app.open_terminal_copy_mode();
    assert!(app.terminal_copy_mode.is_some());

    app.change_workspace_root(b.clone());

    assert_eq!(app.terminals.len(), 2, "precondition: the restore ran");
    assert!(
        app.terminal_copy_mode.is_none(),
        "copy mode was bound to a replaced pane; its coordinates mean nothing on the restored grid"
    );
    assert!(
        app.closed_terminals.is_empty(),
        "undo-close must not resurrect a PTY from the outgoing workspace into the restored panel"
    );
}

#[test]
fn osc_9_4_progress_paints_a_border_gauge_and_pill_percent() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Pane 0 reports 46% (state 1), then a read gates the state-0 clear so
    // the test controls both phases. A second pane makes the name pill paint.
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from(
                "printf '\\033]9;4;1;46\\007'; read x; printf '\\033]9;4;0\\007'; sleep 30",
            ),
        ],
        tmp.path(),
    )
    .unwrap();
    app.terminals[0].set_manual_name(Some(String::from("bld")));
    app.terminals.push(
        crate::widgets::terminal::PtyTerminal::new_running(
            "/bin/sh",
            &[String::from("-c"), String::from("sleep 30")],
            tmp.path(),
        )
        .unwrap(),
    );
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the 1-of-46 progress report",
        || app.terminals[0].progress() == Some((1, 46)),
    );

    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    fn screen_rows(term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect()
            })
            .collect()
    }
    let area = app.terminals[0].last_area;
    let inner = app.terminals[0].last_inner;
    let rows = screen_rows(&term);
    let border_row = &rows[(area.y + area.height - 1) as usize];
    // The gauge's fill glyph contrasts with the pane's own border weight
    // (#470): a focused pane draws a heavy border, so its fill is `═`
    // against `━`; an unfocused one draws light, so its fill is `━`
    // against `─`. Counting the glyph that marks FILL keeps this test
    // about the gauge rather than about which border weight is in force.
    let fill_glyph = if app.terminals[0].focused {
        '═'
    } else {
        '━'
    };
    let fill: usize = border_row
        .chars()
        .skip(inner.x as usize)
        .take(inner.width as usize)
        .filter(|c| *c == fill_glyph)
        .count();
    let expected = (inner.width as u32 * 46 / 100) as usize;
    assert!(
        expected > 0,
        "pane too narrow for the test to mean anything"
    );
    assert_eq!(
        fill, expected,
        "the bottom border must fill to 46% with heavy strokes: {border_row:?}"
    );
    let top_row = &rows[area.y as usize];
    assert!(
        top_row.contains("bld \u{b7} 46%"),
        "the name pill carries the percent: {top_row:?}"
    );

    // State 0 clears: the gauge and the pill percent both vanish.
    app.terminals[0].write_input(b"\n");
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "state 0 to clear the progress",
        || app.terminals[0].progress().is_none(),
    );
    term.draw(|f| app.render(f)).unwrap();
    let rows = screen_rows(&term);
    let border_row = &rows[(area.y + area.height - 1) as usize];
    // No FILL glyph remains. `━` is the border's own glyph on a focused
    // pane (#470), so a literal check here would fail on an intact border
    // rather than on a leftover gauge.
    assert!(
        !border_row.contains(fill_glyph),
        "a cleared gauge restores the plain border: {border_row:?}"
    );
    assert!(
        !rows[area.y as usize].contains("46%"),
        "the pill percent must clear too"
    );
}

#[test]
fn cmd_k_d_dumps_the_pane_scrollback_into_a_rendered_log_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=scroll; echo ${s}back-payload-7; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    app.terminals[0].set_manual_name(Some(String::from("bldlog")));
    app.scrollback_dir = tmp.path().join("dumps");
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the pane output \"scrollback-payload-7\"",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("scrollback-payload-7"))
        },
    );

    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('d'), KeyModifiers::NONE)));
    assert!(
        matches!(app.focus, Pane::Editor),
        "the dump lands in the editor, so focus follows"
    );
    let label = app
        .editor
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    assert!(
        label.contains("bldlog") && label.contains("scrollback"),
        "the scratch tab is named after the pane: {label:?}"
    );
    // A pane's output carries colour (croft's own run header does, and so
    // does any shell prompt), so the dump lands in the rendered log view
    // (#257) rather than a plain scratch buffer; the text is read from the
    // view once its index is complete and its window loaded.
    let text: Vec<String> = {
        let log = app
            .editor
            .log
            .as_mut()
            .expect("the dump opens as a rendered log");
        log.finish_index();
        let n = log.len();
        log.ensure(0, n).unwrap();
        (0..n)
            .map(|i| log.visible_text(i).unwrap_or("").to_string())
            .collect()
    };
    assert!(
        text.iter().any(|l| l.contains("scrollback-payload-7")),
        "the pane's output is in the buffer: {text:?}"
    );
    let blank_tail = text
        .iter()
        .rev()
        .take_while(|l| l.trim().is_empty())
        .count();
    assert!(
        blank_tail <= 1,
        "the unused live-screen rows must not pad the buffer: {blank_tail} blank tail lines"
    );
}

/// #257: Cmd+K D lands the pane's scrollback in the rendered log view, so
/// the colours the pane showed survive into the tab, written to a real file
/// under the scrollback dir because the log view indexes from disk.
#[test]
fn cmd_k_d_opens_the_scrollback_as_a_rendered_log_with_its_colours() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.scrollback_dir = tmp.path().join("dumps");
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[32m${s}GREEN\\033[0m rest\\n\"; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the pane to print the coloured row",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("QQGREEN rest"))
        },
    );
    app.open_scrollback_in_editor();
    {
        // The view indexes in the background and parses a window on demand:
        // read only what it has actually loaded.
        let log = app
            .editor
            .log
            .as_mut()
            .expect("the scrollback opens in the rendered log view");
        log.finish_index();
        let n = log.len();
        log.ensure(0, n).unwrap();
    }
    let log = app.editor.log.as_ref().unwrap();
    let row = (0..log.len())
        .find(|&i| {
            log.visible_text(i)
                .is_some_and(|t| t.contains("QQGREEN rest"))
        })
        .expect("the coloured row is in the view, escapes stripped");
    let line = log.line(row).expect("the row is parsed");
    let green = line
        .spans
        .iter()
        .find(|s| line.text[s.start..s.end].contains("QQGREEN"))
        .unwrap_or_else(|| panic!("a span colours the green run: {:?}", line.spans));
    assert_eq!(
        green.style.fg,
        Some(crate::ansi_text::AnsiColor::Indexed(2)),
        "the pane's green reaches the tab as the symbolic green slot"
    );
    let path = app.editor.path.clone().expect("the tab has a path");
    assert!(
        path.starts_with(&app.scrollback_dir) && path.extension().is_some_and(|e| e == "log"),
        "the dump is a .log under the scrollback dir: {path:?}"
    );
    assert!(
        path.is_file(),
        "the dump exists on disk for the view to index"
    );
}

/// The dump file is one per pane and overwritten on each Cmd+K D, so the
/// second dump must show the pane's NEW output, not the first dump's rows
/// through a stale index, and must not claim a fresh open it did not make.
#[test]
fn a_second_dump_of_the_same_pane_shows_its_new_output() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.scrollback_dir = tmp.path().join("dumps");
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from(
                "s=QQ; printf \"\\033[32m${s}ONE\\033[0m\\n\"; read line; \
                 printf \"\\033[32m${s}TWO\\033[0m\\n\"; sleep 30",
            ),
        ],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the first coloured row",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("QQONE"))
        },
    );
    app.open_scrollback_in_editor();
    assert!(app.editor.log.is_some(), "the first dump is a rendered log");

    app.terminals[0].write_input(b"\n");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the second coloured row",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("QQTWO"))
        },
    );
    app.open_scrollback_in_editor();
    let text: Vec<String> = {
        let log = app
            .editor
            .log
            .as_mut()
            .expect("the second dump is a rendered log too");
        log.finish_index();
        let n = log.len();
        log.ensure(0, n).unwrap();
        (0..n)
            .map(|i| log.visible_text(i).unwrap_or("").to_string())
            .collect()
    };
    assert!(
        text.iter().any(|l| l.contains("QQTWO")),
        "the second dump shows the new row, not the stale first dump: {text:?}"
    );
}

/// The route gate and the open path's sniff must agree: the sniff reads only
/// the first 8 KiB, so colour that first appears beyond it must route to the
/// plain buffer, or the user gets an editable file full of raw escapes.
#[test]
fn colour_beyond_the_sniff_prefix_keeps_the_dump_plain() {
    let plain: Vec<String> = (0..400)
        .map(|i| format!("plain row {i:04} with padding text"))
        .collect();
    let mut late = plain.clone();
    late.push(String::from("\x1b[32mlate colour\x1b[0m"));
    assert!(
        late.iter().map(|r| r.len() + 1).take(400).sum::<usize>() > 8192,
        "fixture: the colour sits past the 8 KiB sniff prefix"
    );
    assert!(
        !scrollback_opens_as_log(&late),
        "colour past the prefix is not a log to the sniff"
    );
    let mut early = vec![String::from("\x1b[32mearly colour\x1b[0m")];
    early.extend(plain.iter().cloned());
    assert!(
        scrollback_opens_as_log(&early),
        "colour inside the prefix is"
    );
    assert!(!scrollback_opens_as_log(&plain), "no colour at all is not");
}

/// A dump is a pinned tab of its own, as the scratch buffer was: a later
/// single-click file peek must not replace it, and two panes' dumps must
/// not share one preview slot.
#[test]
fn a_scrollback_dump_is_a_pinned_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.scrollback_dir = tmp.path().join("dumps");
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[32m${s}PIN\\033[0m\\n\"; sleep 30"),
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
                .any(|l| l.contains("QQPIN"))
        },
    );
    app.open_scrollback_in_editor();
    let path = app.editor.path.clone().expect("the dump tab has a path");
    let idx = app
        .editor
        .find_tab_with_path(&path)
        .expect("the dump tab is findable by its path");
    assert!(
        !app.editor.is_preview(idx),
        "the dump is pinned, not a replaceable preview"
    );
}

/// Two panes may carry the same label (two unnamed shells both read
/// `terminal`), so the dump file is keyed on the pane's stable uid too: the
/// second pane's dump must not overwrite and close the first pane's.
#[test]
fn two_panes_with_one_label_dump_to_separate_files() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.scrollback_dir = tmp.path().join("dumps");
    let spawn = |tag: &str| {
        crate::widgets::terminal::PtyTerminal::new_running(
            "/bin/sh",
            &[
                String::from("-c"),
                format!("s=QQ; printf \"\\033[32m${{s}}{tag}\\033[0m\\n\"; sleep 30"),
            ],
            tmp.path(),
        )
        .unwrap()
    };
    app.terminals = vec![spawn("ONE"), spawn("TWO")];
    for (i, tag) in ["QQONE", "QQTWO"].iter().enumerate() {
        app.terminals[i].set_manual_name(Some(String::from("same")));
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(500),
            "the pane to print its row",
            || {
                app.terminals[i]
                    .grid_lines()
                    .0
                    .iter()
                    .any(|l| l.contains(tag))
            },
        );
    }
    app.active_terminal = 0;
    app.focus_pane(Pane::Terminal);
    app.open_scrollback_in_editor();
    let first = app.editor.path.clone().expect("first dump has a path");
    app.active_terminal = 1;
    app.focus_pane(Pane::Terminal);
    app.open_scrollback_in_editor();
    let second = app.editor.path.clone().expect("second dump has a path");
    assert_ne!(first, second, "one file per pane, not per label");
    assert!(
        first.is_file() && second.is_file(),
        "both dumps are still on disk"
    );
    assert!(
        app.editor.find_tab_with_path(&first).is_some(),
        "the first pane's tab survives the second pane's dump"
    );
}

/// A dump holds terminal output, so it is written owner-only: the dir is
/// 0700 and the file 0600 whatever the umask, so another local account
/// cannot read it.
#[cfg(unix)]
#[test]
fn a_scrollback_dump_is_owner_only_on_disk() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.scrollback_dir = tmp.path().join("dumps");
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[32m${s}SEC\\033[0m\\n\"; sleep 30"),
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
                .any(|l| l.contains("QQSEC"))
        },
    );
    app.open_scrollback_in_editor();
    let path = app.editor.path.clone().expect("the dump has a path");
    let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    let dir_mode = std::fs::metadata(&app.scrollback_dir)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600, "the dump is readable by its owner alone");
    assert_eq!(dir_mode, 0o700, "and so is the dir that holds it");
}

/// A dump file must not be shared across croft processes: the pane uid is
/// a per-process counter that restarts at 1, so the file name carries the
/// pid too, as the session handoff file does.
#[test]
fn a_scrollback_dump_file_is_scoped_to_this_process() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.scrollback_dir = tmp.path().join("dumps");
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[32m${s}PID\\033[0m\\n\"; sleep 30"),
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
                .any(|l| l.contains("QQPID"))
        },
    );
    app.open_scrollback_in_editor();
    let name = app
        .editor
        .path
        .clone()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .expect("the dump has a file name");
    assert!(
        name.contains(&format!("-{}-", std::process::id())),
        "the file name carries this process's pid: {name}"
    );
}

/// The prune after a close is scoped to the group the close emptied: a
/// group the user left blank on purpose (a split with one side cleared) is
/// not this command's to fold away.
#[test]
fn a_scrollback_dump_leaves_a_blank_group_it_did_not_empty_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.scrollback_dir = tmp.path().join("dumps");
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[32m${s}KEEP\\033[0m\\n\"; read line; sleep 30"),
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
                .any(|l| l.contains("QQKEEP"))
        },
    );
    app.open_scrollback_in_editor();
    // Split, blank the new (focused) group, then focus the group that holds
    // the dump: the blank group is now an inactive leaf with no dump in it.
    app.split_editor();
    assert!(app.editor_layout.is_split(), "fixture: the editor is split");
    app.editor.close_tab(0);
    assert!(
        app.editor.is_blank_initial(),
        "fixture: the focused group is blank"
    );
    app.focus_editor_group(true);
    assert!(
        app.editor.path.is_some(),
        "fixture: the dump's group is focused"
    );
    assert!(
        app.editor_layout
            .inactive_groups()
            .iter()
            .any(|g| g.is_blank_initial()),
        "fixture: the blank group is inactive"
    );
    app.focus_pane(Pane::Terminal);
    app.open_scrollback_in_editor();
    assert!(
        app.editor_layout.is_split(),
        "a blank group this dump did not empty survives it"
    );
}

/// With two inactive groups, one emptied by the close and one the user
/// blanked, only the emptied one is pruned: the sweep must be scoped to the
/// group the close emptied, not to blankness.
#[test]
fn a_scrollback_dump_prunes_only_the_group_it_emptied() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.scrollback_dir = tmp.path().join("dumps");
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[32m${s}THREE\\033[0m\\n\"; read line; sleep 30"),
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
                .any(|l| l.contains("QQTHREE"))
        },
    );
    app.open_scrollback_in_editor();
    // [A(dump) | B(focused)] -> blank B -> focus A -> split A again:
    // [[A(dump) | C(focused)] | B(blank)], so A and B are both inactive.
    app.split_editor();
    app.editor.close_tab(0);
    app.focus_editor_group(true);
    assert!(
        app.editor.path.is_some(),
        "fixture: the dump's group is focused"
    );
    app.split_editor();
    let inactive = app.editor_layout.inactive_groups();
    assert_eq!(inactive.len(), 2, "fixture: two inactive groups");
    assert!(
        inactive.iter().any(|g| g.is_blank_initial())
            && inactive.iter().any(|g| g
                .find_tab_with_path(app.editor.path.as_ref().unwrap())
                .is_some()),
        "fixture: one blank, one holding the dump"
    );
    app.focus_pane(Pane::Terminal);
    app.open_scrollback_in_editor();
    let inactive = app.editor_layout.inactive_groups();
    assert!(
        app.editor_layout.is_split(),
        "the user's blank group keeps its split"
    );
    assert_eq!(
        inactive.len(),
        1,
        "exactly the emptied group is gone: {} left",
        inactive.len()
    );
    assert!(
        inactive[0].is_blank_initial(),
        "and the survivor is the user's blank group"
    );
}

/// A dump that cannot reach the disk still leaves the user the text: the
/// plain rows land in the scratch buffer and the status says why.
#[test]
fn a_scrollback_dump_that_cannot_be_written_falls_back_to_the_plain_buffer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A file where the dir's parent should be: nothing can be created under it.
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, "not a dir").unwrap();
    app.scrollback_dir = blocker.join("dumps");
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[32m${s}FALL\\033[0m\\n\"; sleep 30"),
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
                .any(|l| l.contains("QQFALL"))
        },
    );
    app.open_scrollback_in_editor();
    assert!(app.editor.log.is_none(), "no rendered log view opened");
    assert!(
        app.editor.lines.iter().any(|l| l.contains("QQFALL")),
        "the plain text is in the buffer: {:?}",
        app.editor.lines
    );
    assert!(
        app.status.starts_with("Rendered scrollback unavailable"),
        "the status says why: {:?}",
        app.status
    );
}

/// Dumps are named by pid, so a croft that exits leaves its dumps behind
/// with no process that will ever overwrite them; the next dump reaps the
/// files of pids that are gone and leaves live processes' files alone.
#[cfg(unix)]
#[test]
fn a_scrollback_dump_reaps_the_dumps_of_dead_processes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.scrollback_dir = tmp.path().join("dumps");
    std::fs::create_dir_all(&app.scrollback_dir).unwrap();
    // A pid that is certainly gone: a child that has already been reaped.
    let mut child = std::process::Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    let dead = app
        .scrollback_dir
        .join(format!("shell-{}-1-scrollback.log", child.id()));
    std::fs::write(&dead, "gone").unwrap();
    let live = app
        .scrollback_dir
        .join(format!("shell-{}-99-scrollback.log", std::process::id()));
    std::fs::write(&live, "mine").unwrap();
    let other = app.scrollback_dir.join("notes.txt");
    std::fs::write(&other, "not a dump").unwrap();
    // A pid no `pid_t` can hold reads as alive: a false "alive" only leaks
    // a file, a false "dead" would delete one.
    let huge = app.scrollback_dir.join("shell-4294967295-1-scrollback.log");
    std::fs::write(&huge, "?").unwrap();
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[32m${s}REAP\\033[0m\\n\"; sleep 30"),
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
                .any(|l| l.contains("QQREAP"))
        },
    );
    app.open_scrollback_in_editor();
    assert!(app.editor.path.is_some(), "the dump itself landed");
    assert!(!dead.exists(), "a dead process's dump is reaped");
    assert!(live.exists(), "this process's other dump stays");
    assert!(other.exists(), "a file that is not a dump is not touched");
    assert!(huge.exists(), "a pid outside pid_t is left alone");
}

/// The dump is a new on-disk copy of pane output, so the redact rules must
/// hold on disk too: the matching row is written masked (and gives up its
/// colour), while a neighbouring row keeps its colour.
#[test]
fn a_scrollback_dump_is_redacted_on_disk_and_only_the_masked_row_loses_colour() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.scrollback_dir = tmp.path().join("dumps");
    app.triggers = std::sync::Arc::new(crate::triggers::TriggerSet::from_json(
        r#"[{ "regex": "token \\S+", "action": "redact" }]"#,
    ));
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from(
                "s=QQ; printf \"\\033[32m${s}SAFE\\033[0m\\n\"; \
                 printf \"\\033[31mtoken ${s}SECRET99\\033[0m\\n\"; sleep 30",
            ),
        ],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(std::time::Duration::from_millis(500), "both rows", || {
        app.terminals[0]
            .grid_lines()
            .0
            .iter()
            .any(|l| l.contains("QQSECRET99"))
    });
    app.open_scrollback_in_editor();
    let path = app.editor.path.clone().expect("the dump has a path");
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(
        !on_disk.contains("QQSECRET99"),
        "the secret never reaches the disk: {on_disk:?}"
    );
    let masked_row = on_disk
        .lines()
        .find(|l| l.contains(crate::triggers::MASK))
        .expect("the matching row is written masked");
    assert!(
        !masked_row.contains('\x1b'),
        "the masked row gives up its colour: {masked_row:?}"
    );
    let safe_row = on_disk
        .lines()
        .find(|l| l.contains("QQSAFE"))
        .expect("the neighbouring row is there");
    assert!(
        safe_row.contains("\x1b[32m"),
        "and keeps its colour: {safe_row:?}"
    );
}

/// Copy-mode coordinates live in the App while the pane re-anchors its
/// selection to streaming content; each keystroke used to re-push the
/// stale absolute lines, teleporting the highlight off the text the user
/// was looking at.
#[test]
fn copy_mode_keys_follow_content_that_streamed_between_presses() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // The pane runs a real shell whose prompt arrives on its own schedule:
    // under load it landed between the fed rows and the key press and moved
    // the selection. Wait for the shell's output to settle first.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut last = app.terminals[0].grid_lines().0;
    let mut quiet_since = std::time::Instant::now();
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let now = app.terminals[0].grid_lines().0;
        if now != last {
            last = now;
            quiet_since = std::time::Instant::now();
        } else if last.iter().any(|l| !l.trim().is_empty())
            && quiet_since.elapsed() > std::time::Duration::from_millis(300)
        {
            break;
        }
    }
    let mut fill = String::new();
    for i in 0..40 {
        fill.push_str(&format!("row-{i}\r\n"));
    }
    fill.push_str("row-40");
    app.terminals[0].feed_bytes_for_test(fill.as_bytes());
    app.open_terminal_copy_mode();
    app.handle_terminal_copy_mode_key(key(KeyCode::Char('v'), KeyModifiers::NONE));
    // Five rows stream while the user pauses between keys; the pane
    // re-anchors the selection to keep it on row-40.
    let mut more = String::new();
    for i in 0..5 {
        more.push_str(&format!("\r\nx-{i}"));
    }
    app.terminals[0].feed_bytes_for_test(more.as_bytes());
    app.handle_terminal_copy_mode_key(key(KeyCode::Char('k'), KeyModifiers::NONE));
    let text = app.terminals[0].selection_text();
    assert!(
        text.contains("row-40"),
        "the selection must stay on the content the user was on, got: {text:?}"
    );
}

#[test]
fn a_plain_click_on_the_prompt_row_moves_the_shell_cursor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let sink = tmp.path().join("clicks.bin");
    // The shell's `dd` captures exactly what the click writes to the PTY.
    let script = format!(
        "h=hello; printf '\\033]133;A\\007$ \\033]133;B\\007'; printf \"${{h}}-world\"; stty raw -echo 2>/dev/null; dd bs=1 count=33 of={} 2>/dev/null; sleep 30",
        sink.display()
    );
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), script],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the prompt text",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("hello-world"))
        },
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let inner = app.terminals[0].last_inner;
    let (lines, top) = app.terminals[0].grid_lines();
    let vrow = (lines
        .iter()
        .position(|l| l.contains("hello-world"))
        .unwrap() as i32
        + top) as u16;
    let (x, y) = (inner.x + 2, inner.y + vrow);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), x, y));
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the click's arrow keys to reach the shell (33 bytes)",
        || std::fs::read(&sink).unwrap_or_default().len() == 33,
    );
    assert_eq!(
        std::fs::read(&sink).unwrap(),
        b"\x1b[D".repeat(11),
        "eleven left-arrows bring the cursor from after 'd' to 'h'"
    );
}

/// An app that enables application cursor keys (DECCKM, `\e[?1h`) — Python's
/// REPL binds arrows from terminfo's application-mode entries — must receive
/// arrows and Home/End as SS3 (`\eOA`…), not the normal-mode CSI form the
/// REPL does not recognize (vitali87/croft#1).
#[test]
fn arrow_keys_reach_an_app_cursor_mode_child_as_ss3() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let sink = tmp.path().join("keys.bin");
    // The child turns on DECCKM, then `dd` captures exactly what the six
    // key presses write to the PTY (six 3-byte sequences).
    // The sentinel is assembled at runtime ("${s}DY_MARK") because the pane
    // header echoes this script text verbatim; a literal sentinel would match
    // before the child has run at all.
    let script = format!(
        "s=REA; printf '\\033[?1h'; printf \"${{s}}DY_MARK\"; stty raw -echo 2>/dev/null; dd bs=1 count=18 of={} 2>/dev/null; sleep 30",
        sink.display()
    );
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), script],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the child to turn on app cursor mode",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("READY_MARK"))
        },
    );
    for code in [
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Right,
        KeyCode::Left,
        KeyCode::Home,
        KeyCode::End,
    ] {
        app.handle_terminal_key(key(code, KeyModifiers::NONE));
    }
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the key presses to reach the child (18 bytes)",
        || std::fs::read(&sink).unwrap_or_default().len() == 18,
    );
    assert_eq!(
        std::fs::read(&sink).unwrap(),
        b"\x1bOA\x1bOB\x1bOC\x1bOD\x1bOH\x1bOF",
        "DECCKM is on: keys must arrive in the SS3 application form"
    );
}

/// Broadcast input fans one keystroke out to panes whose children disagree
/// about DECCKM. Each pane must receive the form its own child asked for
/// (bash's readline leaves DECCKM off while zsh turns it on, so mixed panes
/// are the normal case): pane 0 stays in normal mode and must get CSI, pane
/// 1 turns application cursor keys on and must get SS3.
#[test]
fn broadcast_arrows_encode_per_pane_cursor_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let sink_a = tmp.path().join("a.bin");
    let sink_b = tmp.path().join("b.bin");
    // Runtime-assembled sentinel: the pane header echoes the script text.
    let script_a = format!(
        "s=REA; printf \"${{s}}DY_MARK\"; stty raw -echo 2>/dev/null; dd bs=1 count=3 of={} 2>/dev/null; sleep 30",
        sink_a.display()
    );
    let script_b = format!(
        "s=REA; printf '\\033[?1h'; printf \"${{s}}DY_MARK\"; stty raw -echo 2>/dev/null; dd bs=1 count=3 of={} 2>/dev/null; sleep 30",
        sink_b.display()
    );
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), script_a],
        tmp.path(),
    )
    .unwrap();
    app.terminals.push(
        crate::widgets::terminal::PtyTerminal::new_running(
            "/bin/sh",
            &[String::from("-c"), script_b],
            tmp.path(),
        )
        .unwrap(),
    );
    app.active_terminal = 0;
    app.broadcast_input = true;
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the children to reach their prompts",
        || {
            app.terminals
                .iter()
                .all(|t| t.grid_lines().0.iter().any(|l| l.contains("READY_MARK")))
        },
    );
    app.handle_terminal_key(key(KeyCode::Up, KeyModifiers::NONE));
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the broadcast to reach both panes (3 bytes each)",
        || {
            std::fs::read(&sink_a).unwrap_or_default().len() == 3
                && std::fs::read(&sink_b).unwrap_or_default().len() == 3
        },
    );
    assert_eq!(
        std::fs::read(&sink_a).unwrap(),
        b"\x1b[A",
        "normal-mode pane must get the CSI form"
    );
    assert_eq!(
        std::fs::read(&sink_b).unwrap(),
        b"\x1bOA",
        "DECCKM pane must get the SS3 form"
    );
}

#[test]
fn host_accent_rules_dress_matching_panes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_host_accents(&[crate::prefs::HostAccentRule {
        pattern: String::from("prod-*"),
        accent: None, // defaults to the danger red
        badge: Some(String::from("PROD")),
    }]);
    // Pane 0 reports a production hostname over OSC 7; pane 1 stays plain.
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("printf '\\033]7;file://prod-db-1/tmp\\007'; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    app.terminals[0].set_manual_name(Some(String::from("db")));
    app.terminals.push(
        crate::widgets::terminal::PtyTerminal::new_running(
            "/bin/sh",
            &[String::from("-c"), String::from("sleep 30")],
            tmp.path(),
        )
        .unwrap(),
    );
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the OSC 7 host",
        || app.terminals[0].shell_host().as_deref() == Some("prod-db-1"),
    );
    let backend = ratatui::backend::TestBackend::new(140, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let area = app.terminals[0].last_area;
    let red = Some(ratatui::style::Color::Rgb(0xf1, 0x4c, 0x4c));
    let border = term.backend().buffer()[(area.x, area.y + 1)].style();
    assert_eq!(
        border.fg, red,
        "a matching pane's border wears the accent color"
    );
    let inner = app.terminals[0].last_inner;
    let top_row: String = (inner.x..inner.x + inner.width)
        .map(|x| {
            term.backend().buffer()[(x, inner.y)]
                .symbol()
                .chars()
                .next()
                .unwrap_or(' ')
        })
        .collect();
    assert!(
        top_row.contains("PROD"),
        "the badge watermark sits on the pane's top row: {top_row:?}"
    );
    let pill_row: String = (area.x..area.x + area.width)
        .map(|x| {
            term.backend().buffer()[(x, area.y)]
                .symbol()
                .chars()
                .next()
                .unwrap_or(' ')
        })
        .collect();
    assert!(
        pill_row.contains("\u{26a0} db"),
        "the name pill carries the warning mark: {pill_row:?}"
    );

    // The plain pane keeps croft's normal border color.
    let other = app.terminals[1].last_area;
    let other_border = term.backend().buffer()[(other.x, other.y + 1)].style();
    assert_ne!(
        other_border.fg, red,
        "a non-matching pane must not wear the accent"
    );
}

#[test]
fn alt_screen_upward_drag_through_the_real_mouse_pipeline_selects_upward() {
    // Field report: mouse-down on a word mid-pane inside Claude Code,
    // drag UP, and the highlight ran DOWNWARD instead — the drift math
    // measured against the selection's top, which mid-drag is the pointer,
    // so every event "corrected" a phantom shift and walked the anchor
    // down. Reproduce through the production dispatch: an alt-screen
    // mouse-tracking app, a press mid-pane, upward drags, with app
    // repaints (animated status row) and renders between every event.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    // Few enough rows that nothing scrolls in the pane's alt grid. The
    // modes are Claude Code's own pair: click-only 1000 with SGR 1006. Since
    // #474 the press is forwarded to such a child, but a drag is croft's
    // selection because the child never asked for motion -- under 1002 this
    // same plain drag would be the app's and Shift would be needed.
    let mut screen = String::from("\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[H\x1b[2J");
    for i in 0..6 {
        screen.push_str(&format!("transcript line number {i:02}\r\n"));
    }
    screen.push_str("tokens 100");
    app.terminal().feed_bytes_for_test(screen.as_bytes());

    let area = app.terminal().last_area;
    assert!(area.height > 9, "terminal pane must be real: {area:?}");
    let (ix, iy) = (area.x + 1, area.y + 1);
    // Press on grid row 4, then drag up to rows 3 and 2, the app
    // repainting its animated row and croft rendering between events.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: ix,
        row: iy + 4,
        modifiers: KeyModifiers::NONE,
    });
    term.draw(|f| app.render(f)).unwrap();
    for (step, drag_row) in [iy + 3, iy + 2].into_iter().enumerate() {
        app.terminal()
            .feed_bytes_for_test(format!("\x1b[7;1Htokens 10{step}\x1b[K").as_bytes());
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: ix,
            row: drag_row,
            modifiers: KeyModifiers::NONE,
        });
        term.draw(|f| app.render(f)).unwrap();
    }
    let sel = app.terminal().selection().expect("drag built a selection");
    let (sr, _, er, _) = sel.normalised();
    assert_eq!(
        (sr, er),
        (2, 4),
        "dragging up two rows from row 4 must select rows 2..=4, never run downward: {sel:?}"
    );
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: ix,
        row: iy + 2,
        modifiers: KeyModifiers::NONE,
    });
    let sel = app
        .terminal()
        .selection()
        .expect("selection survives release");
    let (sr, _, er, _) = sel.normalised();
    assert_eq!(
        (sr, er),
        (2, 4),
        "release must not move the selection: {sel:?}"
    );
    assert_eq!(
        app.terminal().selection_text(),
        "transcript line number 02\ntranscript line number 03\nt",
        "the extracted text names the rows the pointer actually crossed"
    );
}

/// A keystroke into a fresh, not-yet-attached pane of a live file (a split
/// or reopen holding stale disk text) used to land in the stale buffer and
/// be silently wiped when the attach pass seeded the pane one tick later.
/// The bootstrap input gate must cover this window too: refuse the key
/// with a hint instead of losing it.
#[test]
fn typing_into_an_unattached_pane_of_a_live_file_is_gated_not_wiped() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "hello world").unwrap();
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
    let mut owner = App::new(tmp.path().to_path_buf()).unwrap();
    owner.collab_config = Some((socket.clone(), crate::collab::CollabRole::Owner));
    let mut guest = App::new(tmp.path().to_path_buf()).unwrap();
    guest.collab_config = Some((socket.clone(), crate::collab::CollabRole::Guest));
    owner.open_file_at_launch(&file);
    guest.open_file_at_launch(&file);
    let pump_until =
        |owner: &mut App, guest: &mut App, what: &str, done: &dyn Fn(&App, &App) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done(owner, guest) {
                assert!(Instant::now() < deadline, "never settled: {what}");
                owner.poll_collab();
                guest.poll_collab();
                std::thread::sleep(Duration::from_millis(5));
            }
        };
    pump_until(&mut owner, &mut guest, "bootstrap", &|_, g| {
        g.collab.as_ref().is_some_and(|s| s.is_live("f.txt"))
    });
    guest.editor.insert_str("SESSION ");
    pump_until(&mut owner, &mut guest, "session edit converges", &|o, g| {
        o.editor.lines == g.editor.lines && o.editor.lines[0].contains("SESSION")
    });

    // The split duplicate holds stale disk text and has not attached yet;
    // a keystroke arriving before the next tick must be refused, not
    // absorbed into text the attach pass is about to replace.
    guest.split_editor_dir(crate::app::editor_layout::SplitDir::Horizontal, true);
    guest
        .handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        !guest.editor.lines.join("\n").contains('x'),
        "the keystroke went into a stale unattached pane and will be wiped"
    );
    pump_until(&mut owner, &mut guest, "the split pane seeds", &|o, g| {
        g.editor.lines == o.editor.lines && g.editor.lines[0].contains("SESSION")
    });
    assert!(
        !owner.editor.lines.join("\n").contains('x'),
        "the gated keystroke must not reach the session"
    );
}

/// F6 must give the same guarantee as every other route into the editor:
/// it used to assign focus directly, bypassing `focus_pane`, so cycling
/// into the editor under a maximized terminal focused a zero-height pane.
#[test]
fn cycling_focus_into_the_editor_also_restores_a_maximized_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminal_maximized = true;
    for _ in 0..3 {
        app.cycle_focus();
        if matches!(app.focus, Pane::Editor) {
            break;
        }
    }
    assert!(
        matches!(app.focus, Pane::Editor),
        "F6 must reach the editor"
    );
    assert!(
        !app.terminal_maximized,
        "cycling into the editor must restore it, same as any other route"
    );
}

/// vim's `e` at the last word of a row hops to the next row's first word
/// end, exactly like `w` and `b` already wrap; it used to be a dead key
/// there because the scan ran into the row's trailing padding.
#[test]
fn copy_mode_e_wraps_to_the_next_rows_word_end() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    // A fresh row first: whatever the spawned shell printed (a prompt, a
    // banner — it varies by environment) must not shift the fed text, and
    // the navigation below is cursor-relative rather than jumping to the
    // scrollback top, so the test is hermetic against prior output.
    app.terminals[0].feed_bytes_for_test(b"\r\nalpha beta\r\nnext word");
    app.open_terminal_copy_mode();
    app.handle_terminal_copy_mode_key(key(KeyCode::Char('k'), KeyModifiers::NONE));
    app.handle_terminal_copy_mode_key(key(KeyCode::Char('0'), KeyModifiers::NONE));
    app.handle_terminal_copy_mode_key(key(KeyCode::Char('e'), KeyModifiers::NONE));
    let st = app.terminal_copy_mode.as_ref().unwrap();
    let first_row = st.line;
    assert_eq!(st.col, 4, "first e lands on the end of 'alpha'");
    app.handle_terminal_copy_mode_key(key(KeyCode::Char('e'), KeyModifiers::NONE));
    assert_eq!(
        app.terminal_copy_mode.as_ref().unwrap().col,
        9,
        "second e lands on the end of 'beta'"
    );
    app.handle_terminal_copy_mode_key(key(KeyCode::Char('e'), KeyModifiers::NONE));
    let st = app.terminal_copy_mode.as_ref().unwrap();
    assert_eq!(
        st.line,
        first_row + 1,
        "e at the row's last word must wrap to the next row"
    );
    assert_eq!(st.col, 3, "and land on the end of 'next'");
}

/// vim's `e` keeps advancing across word-less lines: from the last word
/// before a run of blank rows it lands on the end of the next word below
/// them, not at column 0 of the first blank row.
#[test]
fn copy_mode_e_skips_blank_rows_to_the_next_word_end() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.terminals[0].feed_bytes_for_test(b"\r\nalpha\r\n\r\n\r\nnext word");
    app.open_terminal_copy_mode();
    for _ in 0..3 {
        app.handle_terminal_copy_mode_key(key(KeyCode::Char('k'), KeyModifiers::NONE));
    }
    app.handle_terminal_copy_mode_key(key(KeyCode::Char('0'), KeyModifiers::NONE));
    app.handle_terminal_copy_mode_key(key(KeyCode::Char('e'), KeyModifiers::NONE));
    let st = app.terminal_copy_mode.as_ref().unwrap();
    let alpha_row = st.line;
    assert_eq!(st.col, 4, "first e lands on the end of 'alpha'");
    app.handle_terminal_copy_mode_key(key(KeyCode::Char('e'), KeyModifiers::NONE));
    let st = app.terminal_copy_mode.as_ref().unwrap();
    assert_eq!(
        st.line,
        alpha_row + 3,
        "e skips the two blank rows to the row with the next word"
    );
    assert_eq!(st.col, 3, "and lands on the end of 'next'");
}

/// On iTerm2/Sixel the image blit happens after ratatui draws the frame:
/// with the pane's right-click menu open over the picture, the blit erased
/// the overlapped menu rows. The editor preview already suppresses its
/// image under an open context menu; the terminal overlay must too.
#[test]
fn the_terminal_image_yields_to_an_open_context_menu() {
    let (_tmp, mut app) =
        app_with_baked_terminal_image(crate::iterm2_inline::InlineImageProtocol::ITerm2);
    assert!(app.terminal_image_payload().is_some());
    let l = app.overlays.terminal_image.layout().unwrap().clone();
    // The pane's right-click menu, opened on the picture itself.
    app.context_menu = Some(ContextMenu {
        crumb: None,
        scroll: Default::default(),
        origin: (l.cell_x, l.cell_y),
        items: vec![
            MenuEntry::Item {
                label: String::from("Rename Terminal"),
                action: MenuAction::RenameTerminal(0),
            },
            MenuEntry::Item {
                label: String::from("Clear"),
                action: MenuAction::ClearTerminal(0),
            },
        ],
        selected: 0,
        open_submenu: None,
        submenu_selected: 0,
        target_dir: PathBuf::new(),
    });
    app.update_terminal_image_overlay();
    assert!(
        app.terminal_image_payload().is_none(),
        "the menu must win over the picture while it is open"
    );
}

/// A wheel notch on the pane BORDER while the child tracks the mouse used
/// to vanish: the report was undeliverable (the border sits outside the
/// inner grid) and the handler had no fall-through, so nothing scrolled —
/// yet the same cell scrolls croft's scrollback fine on a non-tracking
/// pane. An undeliverable report now falls back the same way.
#[test]
fn a_wheel_on_the_pane_border_scrolls_even_when_the_child_tracks_the_mouse() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // Fill scrollback so there is history to scroll into, then have the
    // child enable mouse tracking.
    let mut feed = Vec::new();
    for i in 0..80 {
        feed.extend_from_slice(format!("row-{i}\r\n").as_bytes());
    }
    feed.extend_from_slice(b"\x1b[?1000h");
    app.terminals[0].feed_bytes_for_test(&feed);
    term.draw(|f| app.render(f)).unwrap();
    let area = app.terminals[0].last_area;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollUp,
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.terminals[0].scroll_display_offset() > 0,
        "the border notch must scroll the scrollback, not vanish"
    );
}

/// In the alternate screen there is no scrollback, so Shift+End must fall
/// through to the program like its comment promises and like Shift+Home /
/// Shift+PageUp/PageDown already do; it used to be swallowed because the
/// bottom-reset arm had no alt-screen guard.
#[test]
fn shift_end_reaches_an_alt_screen_program() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.terminals[0].feed_bytes_for_test(b"\x1b[?1049h");
    app.handle_terminal_key(key(KeyCode::End, KeyModifiers::SHIFT));
    let expected = key_to_bytes(
        key(KeyCode::End, KeyModifiers::SHIFT),
        app.terminals[0].app_cursor_keys(),
    );
    assert!(!expected.is_empty());
    let written = app.terminals[0].written_bytes_for_test();
    assert!(
        written.ends_with(&expected),
        "Shift+End must be encoded to the program in the alt screen, wrote {written:?}"
    );
}

#[test]
fn a_wrapped_prompt_does_not_push_the_url_off_a_maximized_pane() {
    // The deterministic half of #397's quick-select entry. Its sibling below
    // was measured at 3 failures in 20 runs under load, because a REAL
    // shell's prompt wraps to several rows and the pane had one row of slack;
    // a rate is the best evidence a race allows, and it is not a test. Both
    // now run on a silent pane.
    //
    // This one feeds the prompt itself, so the geometry is the whole claim
    // and the machine has no say. Four rows of it, then the same park,
    // stream and assert. On the unmaximized pane, which is five rows here,
    // this fails on every machine on every run.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // The pane's child writes nothing, so the grid holds only the bytes fed
    // below: a real shell's startup prompt landing between them adds rows
    // the geometry does not count (#641).
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "sleep",
        &[String::from("30")],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    app.toggle_terminal_maximize();
    term.draw(|f| app.render(f)).unwrap();
    let inner = app.terminals[0].last_inner;
    let h = inner.height as usize;
    // The same tripwire the sibling carries. This fixture's own band is
    // wider - it needs eight rows, not sixteen - but a panel that shrank
    // below the sibling's threshold should fail in both, not silently pass
    // here while failing there.
    assert!(
        h >= 16,
        "the maximized panel must leave slack for the wrapped prompt; got {h} rows"
    );

    app.terminals[0].feed_bytes_for_test("\r\n".repeat(h * 2).as_bytes());
    app.terminals[0].feed_bytes_for_test(b"\r\nhttp://drift.io");
    app.open_terminal_quick_select();
    // The prompt arrives AFTER the URL is on screen, which is the race: one
    // that lands before the flood is scrolled away by it and costs nothing.
    // Four rows of it, the shape a hostname-and-path prompt takes when it
    // wraps at this width, and then the stream that scrolls the URL.
    app.terminals[0]
        .feed_bytes_for_test(format!("\r\n{}", "p".repeat(inner.width as usize * 4)).as_bytes());
    app.terminals[0].feed_bytes_for_test(b"\r\nx1\r\nx2\r\nx3");
    term.draw(|f| app.render(f)).unwrap();

    let buf = term.backend().buffer().clone();
    let joined: String = (inner.y..inner.y + inner.height)
        .flat_map(|y| {
            (inner.x..inner.x + inner.width)
                .map(move |x| (x, y))
                .map(|(x, y)| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
        })
        .collect();
    // The label clause is what makes `open_terminal_quick_select` above
    // load-bearing: without it the bare URL satisfies this assertion just as
    // well as a labelled one, and deleting the call left the test green.
    assert!(
        joined.contains("ttp://drift.io") && !joined.contains("http://drift.io"),
        "a four-row prompt must not cost the URL its place on screen, and a \
         label must still cover its first cell: {joined:?}"
    );
}

/// Quick-select labels ride the scroll clock like every other overlay:
/// output streaming under an open label set used to leave the gold labels
/// at fixed viewport rows while their matches scrolled away, so the user
/// typed the label next to one thing and copied another.
#[test]
fn quick_select_labels_follow_content_that_streams_below_them() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A SILENT pane (#397). The row slack below was this test's first fix,
    // and it held until a full-suite run captured the pane holding ONLY the
    // shell's wrapped prompt and the trailing x1 x2 x3, the URL gone: the
    // shell's prompt repaint after the maximize's SIGWINCH landed between
    // the feeds and rewrote the rows the URL sat on. Slack absorbs a prompt
    // that appends; it cannot absorb one that repaints. A child that prints
    // nothing removes the writer entirely, and the subject here -- croft's
    // label overlay following content the pane streams -- needs a grid, not
    // a shell. The slack assertion stays: the scroll this test drives still
    // needs the rows.
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("sleep 30")],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    // The panel over the editor, so the pane has ROWS. Unmaximized it is a
    // few rows tall here, and this test parks its URL four rows from the
    // bottom. When the pane still ran the workspace shell that was almost
    // no slack against a wrapped prompt (this test's first entry in #397,
    // 3 failures in 20 runs under load); with a silent child the rows are
    // simply what the scroll below needs, and the maximize is how it gets
    // them.
    app.toggle_terminal_maximize();
    term.draw(|f| app.render(f)).unwrap();
    // Park the cursor at the pane bottom so every further row SCROLLS.
    let h = app.terminals[0].last_inner.height as usize;
    // The precondition this fix IS, checked in the same run as the claim it
    // supports: if a future panel change shrank the pane back toward five
    // rows, the test would quietly return to flaking instead of failing.
    assert!(
        h >= 16,
        "the maximized panel must leave rows for the scroll this test drives; got {h} rows"
    );
    app.terminals[0].feed_bytes_for_test("\r\n".repeat(h * 2).as_bytes());
    // One atomic feed, opening with its own newline, so "\r\n" + URL lands
    // under one grid lock with the URL at column 0. #62 saw the match wrap
    // the pane edge when another writer got between two feeds; the pane is
    // silent now, and the shape is kept because it is the one that cannot
    // wrap whatever else is on screen.
    app.terminals[0].feed_bytes_for_test(b"\r\nhttp://drift.io");
    app.open_terminal_quick_select();
    // The URL by name, not `is_some()`: quick select stages every match on
    // screen, and naming the one this test planted keeps the assertion
    // about it rather than about whatever else happens to match.
    assert!(
        app.terminal_quick_select
            .as_ref()
            .is_some_and(|s| s.hints.iter().any(|h| h.text.contains("drift.io"))),
        "staging: the URL is hinted"
    );
    app.terminals[0].feed_bytes_for_test(b"\r\nx1\r\nx2\r\nx3");
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer().clone();
    // Join the pane's inner rows into one stream before searching, so pane
    // geometry can't split the needle across a row boundary (#62), and
    // don't pin WHICH gold label the URL drew: label assignment is
    // bottom-priority over every match on screen, and the letter is not the
    // claim. The invariant is that SOME label still covers the URL's first
    // cell after the scroll — the 'h' is overlaid, the rest is intact.
    let inner = app.terminals[0].last_inner;
    let joined: String = (inner.y..inner.y + inner.height)
        .flat_map(|y| {
            (inner.x..inner.x + inner.width)
                .map(move |x| (x, y))
                .map(|(x, y)| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
        })
        .collect();
    assert!(
        joined.contains("ttp://drift.io") && !joined.contains("http://drift.io"),
        "a label must still overlay the URL's first cell after the content\n\
         scrolled (the 'h' covered, the tail intact); pane text: {joined:?}"
    );
}

/// The #64 defect end-to-end: a URL wrapping the pane edge was invisible as
/// a URL — the scanner saw display rows independently, so the head fragment
/// matched nothing and the tail was labelled as a filesystem path, and
/// typing the label copied `/drift.io` instead of what the user sees.
/// Stitched scanning must stage the WHOLE URL, anchored on its head.
#[test]
fn quick_select_sees_a_match_that_wraps_the_pane_edge_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // One atomic feed (#62): its own newline pins the pad to column 0, the
    // pad parks the URL six cells before the pane edge, so "http:/" fills
    // the row (setting WRAPLINE) and "/drift.io" continues on the next.
    let w = app.terminals[0].last_inner.width as usize;
    let mut chunk = String::from("\r\n");
    chunk.push_str(&"x".repeat(w - 6));
    chunk.push_str("http://drift.io");
    app.terminals[0].feed_bytes_for_test(chunk.as_bytes());
    app.open_terminal_quick_select();
    let state = app
        .terminal_quick_select
        .as_ref()
        .expect("staging: the pane holds at least the wrapped URL");
    // `starts_with`, not equality: the live child shell's prompt can land
    // right after the fed chunk, gluing itself onto the URL's tail within
    // the same logical line — the head is the invariant.
    let url = state
        .hints
        .iter()
        .find(|h| h.text.starts_with("http://drift.io"))
        .unwrap_or_else(|| {
            panic!(
                "the wrapped URL must be staged whole; hints were: {:?}",
                state
                    .hints
                    .iter()
                    .map(|h| (h.line, h.start, h.text.as_str()))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(
        url.start,
        w - 6,
        "the label anchors on the URL's first cell, not the wrap row"
    );
    assert!(
        !url.tail.is_empty(),
        "the continuation row rides along as a tail segment"
    );
    assert!(
        !state.hints.iter().any(|h| h.text == "/drift.io"),
        "the wrapped tail must not be labelled as a path fragment; hints: {:?}",
        state
            .hints
            .iter()
            .map(|h| h.text.as_str())
            .collect::<Vec<_>>()
    );
    // Committing the URL's label copies the whole URL, not the fragment.
    let label = url.label.clone();
    for c in label.chars() {
        app.handle_terminal_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(
        app.terminal_quick_select.is_none(),
        "committing a label must close quick-select"
    );
    assert!(
        app.status.contains("Copied"),
        "commit must report the copy, status was: {}",
        app.status
    );
}

/// Quick-select is bound to the pane it opened on, exactly like copy mode:
/// after a pane switch its labels must close, never be pushed onto (or
/// pasted into) the newly active pane at the old pane's coordinates.
#[test]
fn quick_select_closes_instead_of_hijacking_another_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    // Three URLs per row across ten rows: >26 matches forces two-char
    // labels, so the first typed char takes the PUSH path (a live re-push
    // of the hint set) rather than committing and closing.
    let mut feed = Vec::new();
    for i in 0..10 {
        feed.extend_from_slice(
            format!("\r\nhttp://a{i}.io http://b{i}.io http://c{i}.io").as_bytes(),
        );
    }
    feed.extend_from_slice(b"\r\n");
    app.terminals[0].feed_bytes_for_test(&feed);
    app.open_terminal_quick_select();
    let st = app.terminal_quick_select.as_ref().expect("staging: hints");
    assert!(st.hints.len() > 26, "staging: two-char labels required");
    // A label PREFIX char (the first char of some two-char label).
    let prefix = st
        .hints
        .iter()
        .find(|h| h.label.len() == 2)
        .expect("staging: a two-char label")
        .label
        .chars()
        .next()
        .unwrap();
    app.terminals.push(
        crate::widgets::terminal::PtyTerminal::new_running(
            "/bin/sleep",
            &[String::from("30")],
            tmp.path(),
        )
        .unwrap(),
    );
    app.active_terminal = 1;
    app.handle_terminal_quick_select_key(key(KeyCode::Char(prefix), KeyModifiers::NONE));
    assert!(
        app.terminal_quick_select.is_none(),
        "a pane switch closes quick select instead of hijacking the new pane"
    );
    assert!(
        !app.terminals[1].has_hints_for_test(),
        "the sibling pane must not inherit the old pane's hint labels"
    );
}

/// The find anchor rides the scroll clock like every other line-anchored
/// overlay: output streaming while the bar is open used to leave the
/// stored match line pointing N rows below its text, so the next
/// Enter/Shift+Enter walked from a phantom position — re-finding the same
/// occurrence or skipping others — and the bright active highlight
/// detached from its row.
#[test]
fn terminal_find_navigation_survives_streaming_output() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    // Fill the screen first so later output rotates rows into scrollback
    // (line indices only drift once the grid is full).
    let mut feed = Vec::new();
    for i in 0..40 {
        feed.extend_from_slice(format!("pad-{i}\r\n").as_bytes());
    }
    feed.extend_from_slice(b"match-one\r\nmatch-two\r\nmatch-three\r\n");
    app.terminals[0].feed_bytes_for_test(&feed);
    app.handle_terminal_key(key(KeyCode::Char('f'), KeyModifiers::SUPER));
    for c in "match".chars() {
        app.handle_terminal_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let anchored_line = |app: &App| -> String {
        let (lines, now) = app.terminals[0].find_lines_and_clock();
        let (clock, abs, _c, _l) = app.terminal_find_match.expect("an anchored match");
        let abs = abs - (now - clock) as i32;
        let row = lines.iter().rposition(|l| l.start <= abs).unwrap_or(0);
        lines[row].text.clone()
    };
    assert!(
        anchored_line(&app).contains("match-three"),
        "staging: the first anchor is the bottom-most occurrence"
    );
    // Output streams while the bar is open: every stored line drifts.
    app.terminals[0].feed_bytes_for_test(b"filler\r\nfiller\r\nfiller\r\nfiller\r\nfiller\r\n");
    app.handle_terminal_key(key(KeyCode::Enter, KeyModifiers::SHIFT));
    assert!(
        anchored_line(&app).contains("match-two"),
        "prev from the drifted anchor must land on the occurrence above it, got {:?}",
        anchored_line(&app)
    );
}

/// Clearing a pane invalidates find's whole world: the anchored match
/// names content that no longer exists (and the clear can kill the scroll
/// clock's tracer), so the anchor and active highlight must drop and the
/// match count must honestly read zero instead of navigation walking from
/// a phantom position over the emptied grid.
#[test]
fn clearing_a_pane_resets_its_find_anchor_and_count() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.terminals[0].feed_bytes_for_test(b"\r\nmatch-a\r\nmatch-b\r\n");
    app.handle_terminal_key(key(KeyCode::Char('f'), KeyModifiers::SUPER));
    for c in "match".chars() {
        app.handle_terminal_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(
        app.terminal_find_match.is_some(),
        "staging: an anchored match exists"
    );
    assert!(
        app.terminal_find.as_ref().unwrap().match_count > 0,
        "staging: matches counted"
    );
    app.clear_terminal_at(0);
    assert!(
        app.terminal_find_match.is_none(),
        "the cleared grid must drop the stale anchor"
    );
    assert_eq!(
        app.terminal_find.as_ref().unwrap().match_count,
        0,
        "the match count must reflect the emptied grid"
    );
}

/// Terminal find is bound to the pane it opened on: a gesture that
/// activates a sibling pane must close it AND clear the highlight on the
/// pane that owned it. Closing against the newly active pane instead left
/// the owning pane's needle painted for the rest of the session and
/// applied the stored jump anchor to the wrong pane's grid.
#[test]
fn terminal_find_closes_instead_of_straying_to_another_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.terminals[0].feed_bytes_for_test(b"\r\nerror one\r\nerror two\r\n");
    app.handle_terminal_key(key(KeyCode::Char('f'), KeyModifiers::SUPER));
    app.handle_terminal_key(key(KeyCode::Char('e'), KeyModifiers::NONE));
    assert!(
        app.terminals[0].has_search_for_test(),
        "staging: the owning pane is highlighted"
    );
    app.terminals.push(
        crate::widgets::terminal::PtyTerminal::new_running(
            "/bin/sleep",
            &[String::from("30")],
            tmp.path(),
        )
        .unwrap(),
    );
    app.active_terminal = 1;
    app.sync_focus_flags();
    assert!(
        app.terminal_find.is_none(),
        "a pane switch closes find instead of retargeting the new pane"
    );
    assert!(
        !app.terminals[0].has_search_for_test(),
        "the owning pane's highlight must be cleared on close"
    );
}

/// Copy mode is bound to the pane it opened on: a gesture that activates a
/// sibling pane closes the mode (tmux-style) instead of folding the other
/// pane's independent scroll clock into the stored coordinates and
/// painting a garbage selection there.
#[test]
fn copy_mode_closes_instead_of_hijacking_another_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.open_terminal_copy_mode();
    assert!(app.terminal_copy_mode.is_some());
    app.terminals.push(
        crate::widgets::terminal::PtyTerminal::new_running(
            "/bin/sleep",
            &[String::from("30")],
            tmp.path(),
        )
        .unwrap(),
    );
    app.active_terminal = 1;
    app.handle_terminal_copy_mode_key(key(KeyCode::Char('j'), KeyModifiers::NONE));
    assert!(
        app.terminal_copy_mode.is_none(),
        "a pane switch closes copy mode instead of hijacking the new pane"
    );
    assert!(
        app.terminals[1].selection().is_none(),
        "the sibling pane must not inherit a painted selection"
    );
}

#[test]
fn the_deleted_side_says_so_in_its_pane_title() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = open_modify_delete(tmp.path(), true);
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
    assert!(text.contains("INCOMING (theirs): deleted"), "{text}");
    assert!(text.contains("CURRENT (yours)") && !text.contains("CURRENT (yours): deleted"));
}

#[test]
fn finished_settles_pending_matches_pane_and_command() {
    assert!(finished_settles_pending(
        7,
        " cargo build\n",
        7,
        "cargo build"
    ));
    assert!(
        !finished_settles_pending(8, "cargo build", 7, "cargo build"),
        "wrong pane"
    );
    assert!(
        !finished_settles_pending(7, "cargo test", 7, "cargo build"),
        "different command"
    );
}

#[test]
fn a_parked_launch_aborts_when_its_pane_dies_or_never_reports_a_mark() {
    use std::time::Duration;
    let quick = Duration::from_secs(1);
    let over = PRELAUNCH_MARK_GRACE + Duration::from_secs(1);
    assert!(
        pending_abort_reason(false, false, quick).is_some(),
        "a closed pane aborts immediately"
    );
    assert_eq!(
        pending_abort_reason(true, false, over),
        None,
        "a busy pane (long build) waits indefinitely"
    );
    assert_eq!(
        pending_abort_reason(true, true, quick),
        None,
        "an idle pane inside the grace window keeps waiting for the mark"
    );
    assert!(
        pending_abort_reason(true, true, over).is_some(),
        "idle past the grace with no mark = shell integration never reported; abort"
    );
}

/// #249: after a restart the panes came back in the right directories with
/// the right names — and completely BLANK, which is what the report means by
/// "opens all old terminals empty". The pane's own output is now carried in
/// the session store and repainted above the fresh prompt.
///
/// A real panel (two panes) is used deliberately: a lone unnamed pane at the
/// workspace root is still pruned as the default layout, so that the store
/// does not grow for every directory a shell was ever run in.
#[test]
fn a_restored_terminal_pane_shows_the_output_it_had_before() {
    let tmp = tempfile::tempdir().unwrap();
    let session_path = tmp.path().join("sessions.json");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminal_session_path = session_path.clone();
    app.split_terminal().unwrap();

    // Pin an explicit geometry so the assertions below do not depend on the
    // spawn default. (A pane is born 80x24, so this is not waking a
    // zero-column grid — an earlier comment here claimed that and was wrong.)
    app.terminals[0].resize(80, 24);
    // Put a recognisable line on the pane, as the child would have printed.
    app.terminals[0].feed_bytes_for_test(b"unique-marker-249\r\n");
    app.save_terminal_session();
    let raw = std::fs::read_to_string(&session_path).unwrap();
    assert!(
        raw.contains("unique-marker-249"),
        "the store carries the pane's output: {raw}"
    );

    // A fresh app restores the panel; the marker must be on screen.
    let mut app2 = App::new(tmp.path().to_path_buf()).unwrap();
    app2.terminal_session_path = session_path.clone();
    app2.restore_terminal_session_with(true);
    app2.terminals[0].resize(80, 24);
    let (lines, _) = app2.terminals[0].grid_lines();
    assert!(
        lines.iter().any(|l| l.contains("unique-marker-249")),
        "the restored pane repaints its previous output: {lines:?}"
    );
}

/// #298 review: the restored output must sit ABOVE the new shell's prompt.
/// Asserting only that the marker is somewhere on screen passes whether the
/// transcript landed above the prompt, below it, or interleaved — which is
/// exactly what the replay-after-spawn race produced. Painting the preamble
/// before the reader thread starts is what makes the ordering deterministic.
#[test]
fn restored_output_is_painted_above_the_new_prompt() {
    use crate::widgets::terminal::PtyTerminal;
    let tmp = tempfile::tempdir().unwrap();
    let transcript: Vec<String> = (1..=3).map(|i| format!("restored-line-{i}")).collect();
    let t = PtyTerminal::new_with_transcript(tmp.path(), &transcript).unwrap();

    // The preamble is in the grid before the child can write a byte, so the
    // rows are readable immediately — no waiting on the shell.
    let (lines, _) = t.grid_lines();
    let row_of = |needle: &str| lines.iter().position(|l| l.contains(needle));
    let first = row_of("restored-line-1").expect("first restored line is on screen");
    let last = row_of("restored-line-3").expect("last restored line is on screen");
    assert!(
        first < last,
        "the transcript keeps its order: {first} then {last} in {lines:?}"
    );

    // Whatever the shell prints lands on a later row than the restored text.
    let after: Vec<&String> = lines.iter().skip(last + 1).collect();
    assert!(
        after.iter().all(|l| !l.contains("restored-line")),
        "nothing restored appears below the last restored line: {after:?}"
    );
}

/// #298 review: a full-screen program's frame is not scrollback. Quitting
/// with vim or htop open used to persist that frame and replay it as inert
/// text above the next prompt — the alternate screen exists precisely so its
/// contents vanish on exit.
#[test]
fn an_alt_screen_frame_is_not_persisted_as_scrollback() {
    let tmp = tempfile::tempdir().unwrap();
    let session_path = tmp.path().join("sessions.json");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminal_session_path = session_path.clone();
    app.split_terminal().unwrap();
    app.terminals[0].resize(80, 24);

    // Real scrollback first, then a full-screen program takes over.
    app.terminals[0].feed_bytes_for_test(b"real-scrollback-line\r\n");
    app.terminals[0].feed_bytes_for_test(b"\x1b[?1049h");
    app.terminals[0].feed_bytes_for_test(b"FULLSCREEN-TUI-FRAME\r\n");
    assert!(
        app.terminals[0].alt_screen(),
        "the pane is on the alternate screen"
    );

    app.save_terminal_session();
    let raw = std::fs::read_to_string(&session_path).unwrap();
    assert!(
        !raw.contains("FULLSCREEN-TUI-FRAME"),
        "the TUI's frame must not reach the store: {raw}"
    );
}

/// A store written before transcripts existed must still load: `serde(default)`
/// is the mechanism, and nothing else pins it.
#[test]
fn a_pane_record_without_a_transcript_still_parses() {
    let rec: crate::terminal_session::PaneRecord =
        serde_json::from_str(r#"{"cwd":"/tmp","name":null}"#).unwrap();
    assert!(rec.transcript.is_empty());
}

/// #356: a recorded frame carries the VISIBLE screen, not the scrollback.
///
/// `grid_lines` starts at `topmost_line()`, which is negative scrollback —
/// up to 5000 rows. Writing all of it after a clear scrolls the live screen
/// straight off the top, so the cast shows the tail of the history rather
/// than what the user was looking at, at roughly 400 KB per frame. A test
/// that only counts event kinds cannot see either half of that.
#[test]
fn a_recorded_frame_is_the_visible_screen_not_the_scrollback() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0].last_inner = ratatui::layout::Rect {
        x: 1,
        y: 1,
        width: 40,
        height: 20,
    };
    // `resize`, not a `CSI 8 ; rows ; cols t` feed: the emulator leaves that
    // sequence unhandled, so the grid kept the 80x24 it was spawned with while
    // `last_inner` said 40x6, and the recorded frame carried 24 rows under a
    // header announcing 6. Every assertion below still passed, because they
    // read the same `grid_lines` the recorder does: a fixture can be wrong
    // about itself while everything asserted on it holds.
    //
    // 20 rows, and the number answers two failures at once rather than
    // trading one for the other.
    //
    // `line-199` sits one row above the bottom at EVERY height, because the
    // flood ends on a newline. What the height sets is the eviction budget:
    // how many rows the pane's live shell may print before the frame is
    // recorded. `resize` writes through to the pty master, so that shell
    // takes a SIGWINCH the old escape-sequence feed never sent, and it is a
    // login shell running the developer's own rc - a wrapped prompt and a
    // banner are ordinary. At 6 the budget was five rows, which is inside
    // that range; at 12 it was eleven, which is nearer than it needs to be.
    //
    // The other constraint is only that the height DIFFER from the 24 a pane
    // is spawned at, or the header assertion below cannot fail: restore the
    // unhandled escape sequence at 24 and every assertion here passes. 20
    // satisfies both - 19 rows of budget, and the control still fires.
    //
    // A third way the height matters, worth writing down because it is not
    // obvious: the scrollback guard rejects a last-N-rows recorder by
    // arithmetic on this number, not structurally. At `resize(40, 64)` a
    // recorder writing the last 64 rows would satisfy every assertion here,
    // because 64 would then BE the screen. The row-order assertion below is
    // the one check that does not depend on the height.
    app.terminals[0].resize(40, 20);

    // Far more output than the screen holds, so most of it is scrollback.
    let mut flood = String::new();
    for i in 0..200 {
        flood.push_str(&format!("line-{i}\r\n"));
    }
    app.terminals[0].feed_bytes_for_test(flood.as_bytes());

    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    app.record_active_screen();
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);

    let path = std::fs::read_dir(app.workspace_root())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "cast"))
        .expect("a .cast file was written");
    let text = std::fs::read_to_string(&path).unwrap();

    let frame = text
        .lines()
        .skip(1)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|ev| ev[1] == "o")
        .and_then(|ev| ev[2].as_str().map(String::from))
        .expect("an output event");

    // The geometry a player is handed, taken from the FILE rather than from
    // `grid_lines`. The recorder takes the header from the pane's rect and
    // the frame body from the grid, and nothing tied the two together: a
    // fixture whose rect and grid disagreed wrote a 24-row frame under a
    // header announcing 6, and every assertion here still passed because they
    // all read the grid. A player sizes its window from this header, so the
    // frame that follows has to fit it.
    let header: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("a header")).expect("the header is JSON");

    let rows = frame.split("\r\n").count();
    // The claim is the VISIBLE screen, so the bound is the screen's own
    // height, taken from the pane rather than guessed: `grid_lines` returns
    // the scrollback first, and the visible rows are what remains after its
    // negative `top`. The bound of 64 this replaces admitted the regression
    // it existed to reject - a recorder that wrote the last 64 rows carries
    // 40 rows of history AND `line-199`, so every assertion here passed on
    // it (verified by making `record_active_screen` do exactly that).
    let (all, top) = app.terminals[0].grid_lines();
    let visible = all.len() - (-top).max(0) as usize;
    assert_eq!(
        rows, visible,
        "the frame must be the {visible}-row screen, not {rows} rows of history"
    );
    assert_eq!(
        (header["width"].as_u64(), header["height"].as_u64()),
        (Some(40), Some(rows as u64)),
        "the geometry a player reads must describe the frame that follows it"
    );
    // And a check that does not go through `grid_lines` at all, so a wrong
    // slice cannot agree with a wrong expectation: any frame wider than the
    // screen reaches back into the flood, and `line-100` is far outside a
    // screen-sized window at the recent end of 200 lines.
    assert!(
        !frame.contains("line-100"),
        "the frame reaches back into scrollback: {frame:?}"
    );
    // Order and adjacency, in one literal that does not go through
    // `grid_lines`. Everything above constrains the frame's ENDS and its row
    // count, so a recorder that emitted the visible rows reversed, or
    // shuffled, satisfies all of it.
    assert!(
        frame.contains("line-198\r\nline-199"),
        "the frame must carry the screen's rows in order: {frame:?}"
    );
    // And it is the RECENT end of the output, not the oldest.
    // `line-199` alone. The `|| contains("line-19")` this replaces was both
    // redundant and weakening: "line-199" already contains "line-19", so the
    // disjunct could only ever ADD acceptance - of a frame showing an old
    // scrollback slice around line-19 through line-24, which is precisely the
    // regression the assertion exists to reject. An OR whose second arm is a
    // substring of its first can only make a check weaker.
    assert!(
        frame.contains("line-199"),
        "the frame must show what was last on screen: {:?}",
        &frame[..frame.len().min(120)]
    );
    assert!(
        !frame.contains("line-0\r\n"),
        "the frame reaches back to the first line ever printed"
    );
}

/// The recorded frame's rows wrap at the width the header declares (#397).
///
/// The header takes its geometry from the pane's `last_inner` and the body
/// comes from the grid, and the sibling test above ties them together in one
/// dimension only: it compares `header["height"]` against a row count derived
/// from the frame, but `header["width"]` against the literal 40 the fixture
/// itself set. A grid wrapped at any other column satisfies that. It is not a
/// hypothetical shape either: that fixture shipped an 80-column grid under a
/// 40-column header and every assertion in it passed.
///
/// A line longer than the screen is the discriminator. It can only wrap at
/// the grid's REAL width, so a frame whose rows run past the header's width
/// fails here, and the digits make the wrap column readable from the text
/// rather than inferred from a length.
#[test]
fn a_recorded_frames_rows_wrap_at_the_width_its_header_declares() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0].last_inner = ratatui::layout::Rect {
        x: 1,
        y: 1,
        width: 40,
        height: 20,
    };
    // `resize`, so the GRID is 40 columns too. Setting `last_inner` alone
    // leaves the grid at the 80 it was spawned with, which is exactly the
    // disagreement this test exists to reject.
    app.terminals[0].resize(40, 20);

    // Digits repeating every SEVEN columns, so the wrap point is READ off the
    // text. The period matters twice over. A uniform fill would look
    // correctly wrapped at every width; and a period of ten, which this
    // replaces, divides 40, so `long[..40]` and `long[40..80]` were the SAME
    // string and the adjacency needle below was also satisfied at grid widths
    // 50 and 60. Seven is coprime with 40, so the two halves differ and
    // adjacency pins the column by itself.
    let long: String = (0..100u8).map(|i| char::from(b'0' + (i % 7))).collect();
    // A LEADING CRLF, in the SAME feed as the payload. The pane runs a real
    // shell whose prompt races these bytes (#397), and a prompt landing first
    // would push the payload off column 0, at which point the line wraps
    // somewhere else and this test reports "the long line must wrap at column
    // 40" against a grid that really is 40 - the same misdiagnosis its own
    // first draft produced for a different reason. `feed_bytes_for_test`
    // holds the term lock for the whole buffer, so the reader thread cannot
    // interleave INSIDE this call; a separate leading feed would leave the
    // window between the two calls open.
    app.terminals[0].feed_bytes_for_test(format!("\r\n{long}\r\n").as_bytes());

    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    app.record_active_screen();
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);

    let path = std::fs::read_dir(app.workspace_root())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "cast"))
        .expect("a .cast file was written");
    let text = std::fs::read_to_string(&path).unwrap();
    let header: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("a header")).expect("the header is JSON");
    let frame = text
        .lines()
        .skip(1)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|ev| ev[1] == "o")
        .and_then(|ev| ev[2].as_str().map(String::from))
        .expect("an output event");

    // The recorder writes a clear-and-home BEFORE the rows, so the first
    // segment of a naive split carries seven escape characters on top of its
    // row. Measuring that as a row's width is how the first version of this
    // test read 47 columns off a grid that really was 40.
    let body = frame
        .strip_prefix("\u{1b}[H\u{1b}[2J")
        .expect("a recorded frame opens with clear-and-home, then the rows");
    let declared = header["width"]
        .as_u64()
        .expect("the header declares a width");
    assert_eq!(
        declared, 40,
        "the fixture's own premise: the header must declare the 40 columns the pane was sized to"
    );
    // The wrap, pinned as two WHOLE rows rather than as a substring. The
    // substring form this replaces did not deliver the uniqueness its comment
    // claimed: `contains` only needs the first half to END some row and the
    // second to BEGIN the next, so with a period-P fill it matches at every
    // width w in 40..=60 with w congruent to 40 mod P. At the period of 10
    // this started with that was widths 50 and 60; at 7 it is 47 and 54, and
    // 47 is the very number this test's own first draft misread off a
    // 40-column grid. Changing the period could never fix it.
    //
    // Row EQUALITY can only hold at width 40: at 47 the row is `long[..47]`,
    // which is not `long[..40]`. So adjacency now rejects every other width on
    // its own, and the bound below is a genuinely independent second check
    // rather than the only one doing the work.
    let rows: Vec<&str> = body.split("\r\n").collect();
    // Precondition, so a missing payload cannot be reported as a bad wrap.
    // `record_active_screen` splits scrollback off, so shell startup output
    // pushing the payload above `topmost_line()` would fail the adjacency
    // check below while naming the column, which is the exact misdiagnosis
    // this test's first draft produced for a different reason.
    assert!(
        rows.iter().any(|r| r.starts_with(&long[..8])),
        "the payload must still be on the visible screen: {body:?}"
    );
    assert!(
        rows.windows(2)
            .any(|w| w[0] == &long[..40] && w[1] == &long[40..80]),
        "the long line must wrap at column {declared}, the width the header promises: {body:?}"
    );
    // And no row may exceed it. This is the half that catches the 80-column
    // grid: there the line wraps at 80, so the widest row is twice what a
    // player sizes its window to.
    let widest = body.split("\r\n").map(visible_width).max().unwrap_or(0);
    assert!(
        widest as u64 <= declared,
        "the widest row is {widest} columns and the header promises {declared}: \
         a player sizes its window from the header, so the frame has to fit it"
    );
}

/// #356's playback criterion with `agg`, the GIF converter: a recording
/// with colour, a resize and a wrapped line converts without complaint.
/// Set `CROFT_TEST_AGG` to an agg binary (`cargo install --git
/// https://github.com/asciinema/agg`).
#[test]
#[ignore = "needs agg; set CROFT_TEST_AGG"]
fn a_recorded_cast_converts_to_a_gif_with_agg() {
    let agg = std::env::var("CROFT_TEST_AGG").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let size = |app: &mut App, w: u16, h: u16| {
        app.terminals[0].last_inner = ratatui::layout::Rect {
            x: 1,
            y: 1,
            width: w,
            height: h,
        };
        app.terminals[0].resize(w, h);
    };
    size(&mut app, 60, 10);
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    app.terminals[0].feed_bytes_for_test(
        b"\x1b[1;31mbold red\x1b[0m \x1b[30;42mblack on green\x1b[0m \x1b[4;34munderlined\x1b[0m\r\n",
    );
    app.record_active_screen();
    size(&mut app, 40, 8);
    app.terminals[0].feed_bytes_for_test(
        "a line long enough to wrap at forty columns, with \u{4e00} wide text\r\n".as_bytes(),
    );
    app.record_active_screen();
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);

    let cast = std::fs::read_dir(app.workspace_root())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "cast"))
        .expect("a .cast file was written");
    let gif = tmp.path().join("out.gif");
    let out = std::process::Command::new(&agg)
        .arg(&cast)
        .arg(&gif)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "agg refused the cast: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&gif).unwrap();
    assert!(bytes.starts_with(b"GIF89a"), "a GIF was written");
}

/// A geometry change reaches the cast BEFORE the frame drawn at it (#397).
///
/// `record_active_screen` emits the resize first so a player has the new
/// geometry before the frame that was wrapped for it; the other order renders
/// one frame at the old width. Nothing pinned that order. In every other
/// fixture `recorded_size` already equals the pane's size at record time, so
/// the branch never runs at all, and the one other test that produces an `r`
/// event calls `record_resize` directly, which bypasses the ordering logic
/// rather than exercising it.
///
/// `last_inner` alone moves here, without a `resize`: it is what the branch
/// reads, and the claim under test is the ORDER of two events, not the
/// content of the frame between them.
#[test]
fn a_recorded_resize_precedes_the_frame_it_was_drawn_for() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0].last_inner = ratatui::layout::Rect {
        x: 1,
        y: 1,
        width: 40,
        height: 20,
    };
    app.terminals[0].resize(40, 20);

    // Toggling on seeds `recorded_size` from the pane, so this first frame
    // must NOT carry a resize: a test that saw one here would be reading the
    // seeding, not the ordering.
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    app.record_active_screen();
    app.terminals[0].last_inner = ratatui::layout::Rect {
        x: 1,
        y: 1,
        width: 60,
        height: 10,
    };
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

    // Swap the two statements in `record_active_screen` and this reads
    // `["o", "o", "r"]`; drop the branch and it reads `["o", "o"]`. The
    // sequence is asserted whole rather than as "an r exists somewhere",
    // which both of those would satisfy under a looser check.
    assert_eq!(
        kinds,
        vec!["o", "r", "o"],
        "the cast must carry the new geometry before the frame drawn at it, got {kinds:?}"
    );
}

/// A fold mid-recording narrows the frame, and says nothing about it (#397).
///
/// The sibling above moves `last_inner` alone, because the guard it tests
/// reads `last_inner`. Production folds differently: `render` assigns
/// `last_inner = inner` and then calls `resize(inner.width, inner.height)`,
/// so a real fold leaves `last_inner.width` at 0 while the GRID floors at two
/// columns. The zero then suppresses the resize event, and the cast carries a
/// 2-column frame under a 40-column header with nothing announcing it.
///
/// The first version of this test asserted `widest <= declared` and could not
/// fail. Both numbers are arithmetic: no row can exceed the grid, and the
/// grid is only ever 40 or the floor of 2, so 2 <= 40 held whatever the fold
/// did. Deleting both fold lines left every assertion green, which is a test
/// named for a gesture it never exercised. It now asserts what the fold
/// actually moves.
#[test]
fn a_fold_mid_recording_narrows_the_frame_and_announces_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0].last_inner = ratatui::layout::Rect {
        x: 1,
        y: 1,
        width: 40,
        height: 20,
    };
    app.terminals[0].resize(40, 20);
    app.terminals[0].feed_bytes_for_test(b"\r\nbefore the fold\r\n");

    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    app.record_active_screen();
    // Exactly what `render` does on a fold: `last_inner` takes the zero, and
    // `resize` floors the grid at two columns. Setting `last_inner.width = 2`
    // instead, as the first version did, is a pairing production never
    // produces, and it makes the recorder emit an `r` the real gesture
    // suppresses, so the fixture avoided the case it was written for.
    app.terminals[0].last_inner.width = 0;
    app.terminals[0].resize(0, 20);
    app.record_active_screen();
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);

    let path = std::fs::read_dir(app.workspace_root())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "cast"))
        .expect("a .cast file was written");
    let text = std::fs::read_to_string(&path).unwrap();
    // No header parse here: the "frames fit the header" claim belongs to
    // `a_recorded_frames_rows_wrap_at_the_width_its_header_declares`, which
    // pins it with a 100-character payload. Asserting it HERE was arithmetic,
    // since `after` is pinned to 2 two lines below and no row can exceed the
    // grid, so it survived the fold being deleted. That is the same defect
    // this test was rewritten to remove, kept one assertion further down.
    let events: Vec<(String, String)> = text
        .lines()
        .skip(1)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|ev| {
            Some((
                ev[1].as_str()?.to_string(),
                ev[2].as_str().unwrap_or_default().to_string(),
            ))
        })
        .collect();
    let widths: Vec<usize> = events
        .iter()
        .filter(|(kind, _)| kind == "o")
        .map(|(_, frame)| {
            frame
                .strip_prefix("\u{1b}[H\u{1b}[2J")
                .expect("a recorded frame opens with clear-and-home")
                .split("\r\n")
                .map(visible_width)
                .max()
                .unwrap_or(0)
        })
        .collect();
    assert_eq!(widths.len(), 2, "one frame each side of the fold");

    // The claim the fold moves. Remove the fold and `after` EQUALS `before`
    // (whatever that is: the pane's real shell may print a prompt wider than
    // the payload), so this is the assertion the previous version was
    // missing. No literal here, because the fixture does not control it.
    let (before, after) = (widths[0], widths[1]);
    assert!(
        after < before,
        "the fold must narrow the recorded rows, got {before} then {after}"
    );
    assert_eq!(
        after, 2,
        "and the folded frame carries the GRID's two-column floor (`cols.max(2)` \
         in src/widgets/terminal.rs) rather than a snapshot taken before the \
         resize landed"
    );
    // And the suppression is part of the claim rather than a side effect the
    // filter above discarded: `last_inner.width` of 0 fails the guard's
    // `size.0 > 0`, so the narrowing reaches the player with no `r` in front
    // of it. Set `last_inner.width = 2` and an `r` appears here.
    assert!(
        events.iter().all(|(kind, _)| kind == "o"),
        "a fold announces no geometry, got {:?}",
        events.iter().map(|(k, _)| k).collect::<Vec<_>>()
    );
}

/// A pane that GROWS mid-recording announces the new width first (#397).
///
/// The fold above narrows, which a player renders as a small image in a large
/// window and cannot corrupt anything. Widening is the direction that can:
/// the frame really is wider than the header the recording opened at, and
/// correctness rests entirely on the `r` event arriving before the frame
/// drawn at it. Nothing in the suite recorded a pane that grows.
#[test]
fn a_widened_pane_announces_its_new_geometry_before_the_wider_frame() {
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
    app.terminals[0].last_inner.width = 80;
    app.terminals[0].resize(80, 20);
    // Longer than the header's 40 and shorter than the new 80, so the row
    // that lands is one the OLD geometry could not have held.
    let wide: String = std::iter::repeat_n('w', 60).collect();
    app.terminals[0].feed_bytes_for_test(format!("\r\n{wide}\r\n").as_bytes());
    app.record_active_screen();
    // A third frame with NOTHING moved, which pins the recorder REMEMBERING
    // the widening. Delete `self.recorded_size = size` from
    // `record_active_screen` and this reads ["o","r","o","r","o"]: the stale
    // size makes every later frame re-announce a geometry the player has.
    // Nothing in the suite caught that assignment's removal.
    app.record_active_screen();
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);

    let path = std::fs::read_dir(app.workspace_root())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "cast"))
        .expect("a .cast file was written");
    let text = std::fs::read_to_string(&path).unwrap();
    let header: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("a header")).expect("the header is JSON");
    let declared = header["width"]
        .as_u64()
        .expect("the header declares a width");
    let events: Vec<serde_json::Value> = text
        .lines()
        .skip(1)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect();
    let kinds: Vec<&str> = events.iter().filter_map(|ev| ev[1].as_str()).collect();
    assert_eq!(
        kinds,
        vec!["o", "r", "o", "o"],
        "the widened geometry must reach the player BEFORE the frame wrapped \
         for it, and exactly once, got {kinds:?}"
    );

    let widest = events[2][2]
        .as_str()
        .expect("the second frame is text")
        .strip_prefix("\u{1b}[H\u{1b}[2J")
        .expect("a recorded frame opens with clear-and-home")
        .split("\r\n")
        .map(visible_width)
        .max()
        .unwrap_or(0);
    assert!(
        widest as u64 > declared,
        "the fixture must produce a frame the HEADER cannot hold, or the \
         resize event is not load-bearing: widest {widest}, header {declared}"
    );
    // Unconditional, and on BOTH dimensions. The first version read
    // `events[1][3]`, which an asciicast resize never has (it is
    // `[time, "r", "COLSxROWS"]`, three elements), so the primary extraction
    // always yielded None and an `or_else` did the work behind an `if let`.
    // The day that fallback was tidied away the assertion would have stopped
    // running with nothing failing, and the rows dimension was read by
    // nothing at all: `record_resize(size.1, size.0)` survived the suite.
    assert_eq!(
        events[1][2].as_str(),
        Some("80x20"),
        "the `r` must name the pane's new geometry, got {:?}",
        events[1]
    );
}

/// #356: a recording produces a file that is asciicast v2 all the way
/// through, header and every event.
///
/// Asserted by PARSING the file rather than by matching strings: the format
/// exists to be read by `asciinema play` and `agg`, and a cast that looks
/// right but does not parse fails at the moment someone tries to share it.
/// A terminal stream is full of quotes and escape bytes, so this is the
/// failure mode that actually happens.
#[test]
fn a_recording_writes_a_parseable_asciicast() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0].last_area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };

    // The PTY grid is sized from the pane's INNER rect, so the header must
    // be too — `Borders::ALL` takes a cell on each side. Recording the outer
    // size declares a terminal two columns wider than the content was
    // wrapped for, and the player re-wraps every long line at the wrong
    // column.
    app.terminals[0].last_inner = ratatui::layout::Rect {
        x: 1,
        y: 1,
        width: 78,
        height: 22,
    };

    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    assert!(app.recording.is_some(), "recording started: {}", app.status);

    // Output carrying the characters that break a hand-rolled encoder.
    app.record_terminal_output("\u{1b}[32m\"quoted\" \\ back\u{1b}[0m");
    app.record_resize(132, 50);
    app.record_terminal_output("after the resize");

    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    assert!(app.recording.is_none(), "recording stopped: {}", app.status);

    // Found by LISTING rather than by rebuilding the name. A test that
    // recomputes the path asserts against its own copy of the naming rule,
    // so it would keep passing if production's rule changed — the same
    // stand-in problem as asserting against a helper you wrote yourself.
    let path = std::fs::read_dir(app.workspace_root())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "cast"))
        .expect("a .cast file was written");

    let text = std::fs::read_to_string(&path).expect("the cast file exists");
    let mut lines = text.lines();

    let header: serde_json::Value =
        serde_json::from_str(lines.next().expect("a header line")).expect("header is JSON");
    assert_eq!(header["version"], 2);
    assert_eq!(
        header["width"], 78,
        "the header must carry the GRID size, not the bordered pane's"
    );
    assert_eq!(header["height"], 22);

    let mut kinds = Vec::new();
    let mut last_time = -1.0f64;
    for line in lines {
        let ev: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("event is not JSON: {line:?} — {e}"));
        let t = ev[0].as_f64().expect("a numeric timestamp");
        assert!(
            t >= last_time,
            "time went backwards: {t} after {last_time} — a player would hang"
        );
        last_time = t;
        kinds.push(ev[1].as_str().unwrap_or("?").to_string());
    }
    assert!(
        kinds.iter().any(|k| k == "o") && kinds.iter().any(|k| k == "r"),
        "both output and resize events must reach the file: {kinds:?}"
    );
}

#[test]
fn the_minus_button_still_closes_and_the_chevron_collapses() {
    // The ruling on #313: `-` keeps meaning close, and collapse is its own
    // button. These are one test because the whole risk is the two being
    // confused - a build where `-` folded instead of closing would pass any
    // test that only exercised one of them.
    let (_tmp, mut app, _term) = app_with_terminal_panes(3);

    let fold = app.terminal_collapse_buttons[1];
    assert!(fold.width > 0, "the collapse button must be painted");
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        fold.x + 1,
        fold.y,
    ));
    assert_eq!(app.terminals.len(), 3, "collapsing must never close a pane");
    assert!(app.terminals[1].collapsed, "the clicked pane folded");

    let close = app.terminal_close_buttons[0];
    assert!(close.width > 0, "the close button must be painted");
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        close.x + 1,
        close.y,
    ));
    assert_eq!(
        app.terminals.len(),
        2,
        "`-` still closes the pane - it was NOT repurposed to collapse"
    );
}

/// Ctrl+B reaches the app running in a focused terminal instead of
/// toggling the side bar (#304). Claude Code backgrounds a running
/// command with it and has no other route to that gesture; croft's
/// sidebar toggle keeps three.
#[test]
fn ctrl_b_belongs_to_the_app_in_a_focused_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.bottom_panel_tab = BottomPanelTab::Terminal;
    let before = app.show_tree;

    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(
        app.show_tree, before,
        "Ctrl+B in a focused terminal must not move the side bar"
    );
    // The byte has to ARRIVE, not merely fail to toggle. A guard that
    // declined the toggle and then swallowed the key would satisfy the
    // assertion above while delivering nothing -- which is the whole
    // point of the change, so it is the thing to pin.
    assert!(
        app.terminals[app.active_terminal]
            .written_bytes_for_test()
            .contains(&0x02),
        "Ctrl+B must reach the PTY as 0x02"
    );

    // Cmd+B is untouched, which is what keeps the side bar reachable from
    // the terminal -- releasing the Ctrl form only costs a chord that has
    // three other routes.
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::SUPER))
        .unwrap();
    assert_ne!(
        app.show_tree, before,
        "Cmd+B still toggles the side bar from the terminal"
    );
}

fn pty_bytes(app: &App) -> Vec<u8> {
    app.terminals[app.active_terminal].written_bytes_for_test()
}

/// Ctrl+Q inside vim or nano in a focused terminal is the app's key
/// (visual block, search backwards, XON), not a quit that kills the
/// shell and every unsaved tab with it (#1290).
#[test]
fn ctrl_q_in_a_focused_terminal_reaches_the_app_instead_of_quitting() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = focused_terminal_app(tmp.path());
    app.handle_key(key(KeyCode::Char('q'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(!app.quit, "Ctrl+Q in the terminal must not quit croft");
    assert!(
        pty_bytes(&app).contains(&0x11),
        "Ctrl+Q must reach the PTY as 0x11"
    );
}

/// Ctrl+V is vim's visual block and nano's page down, so in a focused
/// terminal it goes to the app as 0x16 (#1290). Pasting keeps
/// Ctrl+Shift+V and Cmd+V, as in VS Code on Linux.
#[test]
fn ctrl_v_in_a_focused_terminal_reaches_the_app_and_shifted_paste_still_pastes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = focused_terminal_app(tmp.path());
    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(
        pty_bytes(&app),
        vec![0x16],
        "Ctrl+V must reach the PTY as 0x16"
    );
    assert!(!app.status.starts_with("Paste"), "Ctrl+V must not paste");

    // A paste either lands in the PTY or, with no clipboard tool on the
    // box, reports why on the status bar; it never sends the raw byte.
    for chord in [
        key(
            KeyCode::Char('V'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        key(KeyCode::Char('v'), KeyModifiers::SUPER),
    ] {
        app.status.clear();
        let before = pty_bytes(&app).len();
        app.handle_key(chord).unwrap();
        let sent = pty_bytes(&app)[before..].to_vec();
        assert!(
            !sent.contains(&0x16),
            "{chord:?} pastes rather than sending 0x16"
        );
        assert!(
            !sent.is_empty() || app.status.starts_with("Paste"),
            "{chord:?} still pastes the clipboard"
        );
    }
}

#[test]
fn right_clicking_a_collapsed_strip_offers_to_expand_it() {
    // `terminal_at_pos` hit-tests `last_area`, which a collapsed pane clears,
    // so the right-click never resolved to a folded pane: the menu's
    // "Expand Terminal" branch was unreachable while KEYBINDINGS.md advertised
    // it, and a strip had exactly one gesture.
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.toggle_terminal_collapse(1);
    term.draw(|f| app.render(f)).unwrap();
    let strip = app.terminal_strip_rects[1];
    assert_eq!(strip.width, 1, "precondition: the strip is painted");

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
        "the folded pane offers the way back out: {labels:?}"
    );
}

#[test]
fn restore_all_is_inert_while_a_pane_is_maximized() {
    // The mirror of `collapse_is_inert_while_a_pane_is_maximized`. Collapse
    // got the maximize guard in round 1 and restore did not, so Cmd+K ]
    // cleared every flag and announced it, for a change invisible until the
    // user left maximize - the same silent-state-change shape, reversed.
    let (_tmp, mut app, _term) = app_with_terminal_panes(3);
    app.toggle_terminal_collapse(2);
    app.active_terminal = 0;
    app.toggle_terminal_pane_maximize();
    assert!(app.terminal_pane_maximized, "precondition");

    assert!(app.handle_cmd_k_chord(key(KeyCode::Char(']'), KeyModifiers::NONE)));
    assert!(
        app.terminals[2].collapsed,
        "restore must not silently clear folding the user cannot see"
    );
}

#[test]
fn jumping_to_a_captured_line_reveals_the_folded_pane_that_holds_it() {
    // Round 2 moved the "focus never rests on a folded pane" invariant into
    // `sync_focus_flags`. That closed three routes and broke a fourth class:
    // callers which deliberately TARGET a pane by index and then focus it.
    // `sync_focus_flags` sees the target folded and walks focus away, so the
    // caller's chosen pane is silently swapped for a neighbour - here the
    // capture is applied to a pane the user cannot see while focus lands
    // somewhere unrelated. `run_project_task` and `run_block_in_pane` have the
    // same shape and write a COMMAND into the invisible pane.
    //
    // The invariant is right; what was missing is that reaching for a pane by
    // index is a request to SEE it, which is the rationale `undo_close_terminal`
    // already applies when it force-expands a reopened pane.
    let (_tmp, mut app, _term) = app_with_terminal_panes(3);
    let pid = app.terminals[1]
        .shell_pid()
        .expect("a real shell to key on");
    app.terminals[1].feed_bytes_for_test(b"a captured line worth jumping to\r\n");
    app.toggle_terminal_collapse(1);
    assert!(
        app.terminals[1].collapsed,
        "precondition: the target is folded"
    );
    assert_ne!(app.active_terminal, 1, "precondition: focus left it");

    app.captures.push(crate::widgets::captures::CapturedLine {
        pane: String::from("build"),
        shell_pid: Some(pid),
        message: String::from("captured"),
        line: String::from("a captured line worth jumping to"),
    });
    app.captures_open_selected();

    assert_eq!(
        app.active_terminal, 1,
        "the jump must land on the pane that HOLDS the captured line"
    );
    assert!(
        !app.terminals[1].collapsed,
        "and that pane must be visible: applying a selection to a folded pane \
         shows the user nothing"
    );
}

#[test]
fn maximizing_from_a_folded_strip_shows_the_pane_the_menu_named() {
    // `ToggleMaximizeTerminal` carries its index and assigns `active_terminal`
    // directly rather than through `focus_pane`, which makes it the one path
    // that deliberately puts focus ON a folded pane. That is correct here:
    // maximize ignores the collapse flags by design and paints the active
    // pane across the panel, so the pane the menu names is the pane that
    // appears. The strip's right-click made this reachable, and nothing
    // pinned it.
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.toggle_terminal_collapse(1);
    term.draw(|f| app.render(f)).unwrap();
    let strip = app.terminal_strip_rects[1];
    let folded_width = app.terminals[1].last_area.width;

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        strip.x,
        strip.y + 1,
    ));
    let action = app
        .context_menu
        .as_ref()
        .expect("a strip right-click opens the pane menu")
        .items
        .iter()
        .find_map(|e| match e {
            MenuEntry::Item { label, action } if label == "Maximize Terminal" => {
                Some(action.clone())
            }
            _ => None,
        })
        .expect("a folded pane still offers maximize: the entry carries its own index");
    app.context_menu = None;
    let root = app.workspace_root().to_path_buf();
    app.dispatch_menu_action(action, root);
    term.draw(|f| app.render(f)).unwrap();

    assert_eq!(
        app.active_terminal, 1,
        "the maximized pane is the one the menu named"
    );
    assert!(
        app.terminals[1].last_area.width > folded_width,
        "the folded pane is painted across the panel, not left as a strip: \
         {folded_width} then {}",
        app.terminals[1].last_area.width
    );
    assert!(
        app.terminal_strip_rects.iter().all(|r| r.width == 0),
        "no strip survives maximize, or a click could land on a pane that is not painted"
    );
}

/// #474: a rendered app whose only pane is tracking the mouse with SGR
/// reports, exactly the pair Claude Code sends (`?1000h` + `?1006h`), plus
/// a cell inside the grid and that cell's local coordinates.
fn tracking_terminal_app(modes: &[u8]) -> (App, tempfile::TempDir, u16, u16, u16, u16) {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.terminals[0].feed_bytes_for_test(modes);
    let inner = app.terminals[0].last_inner;
    assert!(
        inner.width > 6 && inner.height > 3,
        "terminal grid must be laid out, got {inner:?}"
    );
    let (col, row) = (inner.x + 3, inner.y + 2);
    let (lr, lc) = app.terminals[0]
        .cell_at(col, row)
        .expect("the test cell lies inside the grid");
    (app, tmp, col, row, lc, lr)
}

#[test]
fn a_click_over_a_mouse_tracking_child_is_forwarded_as_press_and_release() {
    // Claude Code's diff panel closes from a click on its ×; htop, lazygit
    // and vim's `mouse=a` route clicks the same way. All of them see the
    // click only if croft sends the press AND the release: a TUI that got a
    // press with no release believes the button is still down. Before #474
    // neither was sent -- the click became a one-cell croft selection.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, col, row, lc, lr) = tracking_terminal_app(b"\x1b[?1000h\x1b[?1006h");
    assert!(app.terminals[0].mouse_reporting());

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert_eq!(
        String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()),
        format!("\x1b[<0;{};{}M", lc + 1, lr + 1),
        "the press is an SGR button-0 report at the 1-based grid cell"
    );
    assert!(
        app.terminals[0].selection().is_none(),
        "a forwarded press plants no croft selection: the child owns the click"
    );
    let uid = app.terminals[0].uid();
    assert!(
        app.terminal_pointer_forwarded
            .is_some_and(|f| f.pane == uid && f.press == (col, row) && !f.selecting),
        "the pane (by uid) and cell that got the press are remembered for the release"
    );

    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), col, row));
    assert_eq!(
        String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()),
        format!("\x1b[<0;{c};{r}M\x1b[<0;{c};{r}m", c = lc + 1, r = lr + 1),
        "the release follows as the lower-case SGR terminator"
    );
    assert!(
        app.terminal_pointer_forwarded.is_none(),
        "the release ends the forwarded gesture"
    );
    assert!(
        app.terminals[0].selection().is_none(),
        "nor does the release leave a highlight behind"
    );
}

#[test]
fn a_click_over_a_child_that_is_not_tracking_stays_a_croft_selection() {
    // The forward is gated on the child having asked (`report_mouse` declines
    // without a tracking mode), so a shell prompt or a plain pager keeps
    // croft's click-to-select and nothing reaches the child's stdin.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, col, row, _, _) = tracking_terminal_app(b"");
    assert!(!app.terminals[0].mouse_reporting());

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert!(
        app.terminals[0].written_bytes_for_test().is_empty(),
        "no report is written to a child that never asked for the mouse"
    );
    assert!(
        app.terminals[0].selection().is_some(),
        "the click anchors croft's own selection as before"
    );
    assert!(app.terminal_pointer_forwarded.is_none());
}

#[test]
fn shift_click_over_a_tracking_child_keeps_the_click_for_croft() {
    // Shift is the bypass for the whole forwarded family, the same one the
    // wheel has and the one xterm, iTerm2 and VS Code use: with it held the
    // click anchors croft's selection so text can still be copied out of a
    // full-screen app that otherwise owns the pointer.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, col, row, _, _) = tracking_terminal_app(b"\x1b[?1000h\x1b[?1006h");

    let mut shift = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    shift.modifiers = KeyModifiers::SHIFT;
    app.handle_mouse(shift);
    assert!(
        app.terminals[0].written_bytes_for_test().is_empty(),
        "Shift+click is croft's: nothing is reported to the child"
    );
    assert!(
        app.terminals[0].selection().is_some(),
        "Shift+click anchors croft's own selection over a tracking child"
    );
    assert!(app.terminal_pointer_forwarded.is_none());
}

#[test]
fn a_click_on_the_pane_border_is_not_forwarded_to_a_tracking_child() {
    // `terminal_at_pos` hit-tests the pane INCLUDING its border, while a
    // report needs a grid cell. A border click over a tracking child must
    // fall through to croft (where it is inert) rather than be reported at a
    // cell the child does not have.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, col, _, _, _) = tracking_terminal_app(b"\x1b[?1000h\x1b[?1006h");
    let border_row = app.terminals[0].last_area.y;
    assert!(
        app.terminals[0].cell_at(col, border_row).is_none(),
        "the top border is not a grid cell"
    );
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        col,
        border_row,
    ));
    assert!(
        app.terminals[0].written_bytes_for_test().is_empty(),
        "no report for a cell outside the child's grid"
    );
    assert!(app.terminal_pointer_forwarded.is_none());
}

#[test]
fn a_forwarded_drag_reports_motion_only_when_the_child_asked_for_it() {
    // DECSET 1000 is click-only: the child wants presses and releases and
    // nothing in between. 1002 adds button-held motion. The Drag arm hands
    // the event to `report_mouse`, whose motion guard already encodes that
    // distinction, so the same drag is silent under 1000 and a `+32` report
    // under 1002 -- and croft's own drag-selection never starts either way.
    use crossterm::event::{MouseButton, MouseEventKind};

    let (mut app, _tmp, col, row, lc, lr) = tracking_terminal_app(b"\x1b[?1000h\x1b[?1006h");
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        col + 2,
        row + 1,
    ));
    assert_eq!(
        String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()),
        format!("\x1b[<0;{};{}M", lc + 1, lr + 1),
        "under 1000 the drag adds nothing after the press: the child asked for clicks"
    );
    // A click-only child cannot use the drag, so croft keeps it: the
    // selection is anchored at the PRESS cell, not where the first motion
    // event happened to land, and extends to the pointer.
    let sel = app.terminals[0]
        .selection()
        .expect("under 1000 the drag is croft's text selection");
    let (sr, sc, er, ec) = sel.normalised();
    assert_eq!(
        (sr, sc, er, ec),
        (lr as i32, lc, lr as i32 + 1, lc + 2),
        "the selection runs from the press cell to the drag cell"
    );
    assert!(
        app.terminal_pointer_forwarded.is_some_and(|f| f.selecting),
        "the gesture is still open and marked as croft's selection"
    );
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        col + 2,
        row + 1,
    ));
    assert_eq!(
        String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()),
        format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            lc + 1,
            lr + 1,
            lc + 3,
            lr + 2
        ),
        "the child that got the press still gets the release"
    );
    assert!(
        app.terminals[0].selection().is_some_and(|s| s.has_area()),
        "and croft's selection survives the release for copying"
    );
    assert!(app.terminal_pointer_forwarded.is_none());

    let (mut app, _tmp, col, row, lc, lr) = tracking_terminal_app(b"\x1b[?1002h\x1b[?1006h");
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        col + 2,
        row + 1,
    ));
    assert_eq!(
        String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()),
        format!(
            "\x1b[<0;{};{}M\x1b[<32;{};{}M",
            lc + 1,
            lr + 1,
            lc + 3,
            lr + 2
        ),
        "under 1002 the drag is a button-0 motion report at the new cell"
    );
    assert!(app.terminals[0].selection().is_none());
}

#[test]
fn a_forwarded_release_outside_the_pane_is_clamped_to_its_grid() {
    // A press the child got must be followed by a release the child gets,
    // even when the pointer has wandered off the pane by then; otherwise the
    // child keeps a button held that the user let go. xterm reports such a
    // release at the nearest cell, and so does croft: the coordinates are
    // clamped to the pane's grid rather than the report being dropped.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, col, row, lc, lr) = tracking_terminal_app(b"\x1b[?1000h\x1b[?1006h");
    let inner = app.terminals[0].last_inner;
    assert!(
        app.terminals[0].cell_at(0, 0).is_none(),
        "(0,0) must be outside the pane, or nothing is clamped"
    );

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 0, 0));
    assert_eq!(
        String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()),
        format!("\x1b[<0;{};{}M\x1b[<0;1;1m", lc + 1, lr + 1),
        "the release lands on the grid's top-left cell, the nearest to (0,0)"
    );
    assert_eq!(
        app.terminals[0].clamp_to_grid(u16::MAX, u16::MAX),
        (inner.x + inner.width - 1, inner.y + inner.height - 1),
        "the far corner clamps to the last cell, never one past it"
    );
    assert!(app.terminal_pointer_forwarded.is_none());
}

#[test]
fn a_forwarded_release_over_a_sibling_pane_goes_to_the_pane_that_got_the_press() {
    // The point of remembering the pane in `terminal_pointer_forwarded`
    // rather than re-hit-testing at release: a drag that ends over the
    // NEIGHBOUR pane still owes its release to the child that got the press,
    // at that child's nearest edge cell. Re-resolving by pointer would hand
    // the release to a pane that never saw a press (or drop it, if that pane
    // is not tracking) and leave the first child with a held button.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(app.terminals.len(), 2, "two panes are visible");
    // Only pane 0 tracks the mouse; pane 1 is a plain shell.
    app.terminals[0].feed_bytes_for_test(b"\x1b[?1000h\x1b[?1006h");
    assert!(app.terminals[0].mouse_reporting());
    assert!(!app.terminals[1].mouse_reporting());

    let inner0 = app.terminals[0].last_inner;
    let inner1 = app.terminals[1].last_inner;
    assert!(
        inner0.width > 4 && inner0.height > 3 && inner1.width > 4 && inner1.height > 3,
        "both panes need a real grid: {inner0:?} {inner1:?}"
    );
    let (col, row) = (inner0.x + 2, inner0.y + 1);
    let (lr, lc) = app.terminals[0].cell_at(col, row).unwrap();
    // A cell well inside pane 1, on the same row.
    let (far_col, far_row) = (inner1.x + 2, row);
    assert!(
        app.terminals[1].cell_at(far_col, far_row).is_some()
            && app.terminals[0].cell_at(far_col, far_row).is_none(),
        "the release cell must belong to pane 1 only"
    );

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        far_col,
        far_row,
    ));

    let (clamped_col, clamped_row) = app.terminals[0].clamp_to_grid(far_col, far_row);
    let (er, ec) = app.terminals[0].cell_at(clamped_col, clamped_row).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()),
        format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            lc + 1,
            lr + 1,
            ec + 1,
            er + 1
        ),
        "pane 0 gets the press and the release, the latter at its nearest edge cell"
    );
    assert!(
        app.terminals[1].written_bytes_for_test().is_empty(),
        "pane 1 never saw a press, so it gets no release either"
    );
    assert!(app.terminal_pointer_forwarded.is_none());
}

#[test]
fn a_forwarded_release_finds_its_pane_after_an_earlier_pane_closes() {
    // `ForwardedPointer` names the pane by uid, not by index. Close the pane
    // BEFORE the pressed one while the button is held: the pressed pane
    // slides from index 1 to index 0, and the release must still reach it.
    // An index would have delivered the release to whatever now sits at
    // the old slot (nothing, here) and left the pressed child holding a
    // button.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(app.terminals.len(), 2);
    app.terminals[1].feed_bytes_for_test(b"\x1b[?1000h\x1b[?1006h");
    let pressed_uid = app.terminals[1].uid();
    let inner = app.terminals[1].last_inner;
    let (col, row) = (inner.x + 2, inner.y + 1);
    let (lr, lc) = app.terminals[1].cell_at(col, row).unwrap();

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert!(
        app.terminal_pointer_forwarded
            .is_some_and(|f| f.pane == pressed_uid)
    );

    assert!(app.close_terminal_at(0), "the EARLIER pane closes");
    assert_eq!(app.terminals.len(), 1);
    assert_eq!(
        app.terminals[0].uid(),
        pressed_uid,
        "the pressed pane now sits at index 0"
    );
    // The pane was re-laid out by the close; release wherever the pointer
    // is, the report is clamped to the pane's current grid.
    term.draw(|f| app.render(f)).unwrap();
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), col, row));

    let written = String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()).into_owned();
    assert!(
        written.starts_with(&format!("\x1b[<0;{};{}M", lc + 1, lr + 1)),
        "the press went to the pane before the close: {written:?}"
    );
    assert!(
        written.ends_with('m') && written.matches("\x1b[<0;").count() == 2,
        "the release still reaches the same pane after it moved to index 0: {written:?}"
    );
    assert!(app.terminal_pointer_forwarded.is_none());
}

#[test]
fn a_click_only_drag_keeps_extending_the_origin_pane_after_the_active_pane_moves() {
    // Under click-only 1000 the drag is croft's selection on the pane that
    // got the press. `Cmd+]` (cycle_terminal) can move the ACTIVE pane while
    // the button is held; the drag must keep extending the origin's
    // selection, not the new active pane's, and the release must finalise
    // it there. Routing through `terminal_mut()` would have left the origin
    // at its one-cell anchor and started nothing on the other pane either.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.terminals[0].feed_bytes_for_test(b"\x1b[?1000h\x1b[?1006h");
    let inner = app.terminals[0].last_inner;
    assert!(
        inner.width > 6 && inner.height > 3,
        "pane 0 needs a grid: {inner:?}"
    );
    let (col, row) = (inner.x + 2, inner.y + 1);
    let (lr, lc) = app.terminals[0].cell_at(col, row).unwrap();

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert_eq!(
        app.active_terminal, 0,
        "the press activates the pressed pane"
    );
    // The keyboard moves the active pane mid-gesture.
    app.cycle_terminal();
    assert_eq!(app.active_terminal, 1, "Cmd+] moved the active pane");

    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        col + 3,
        row + 1,
    ));
    let sel = app.terminals[0]
        .selection()
        .expect("the origin pane carries the drag-selection");
    let (sr, sc, er, ec) = sel.normalised();
    assert_eq!(
        (sr, sc, er, ec),
        (lr as i32, lc, lr as i32 + 1, lc + 3),
        "the origin's selection runs from the press cell to the drag cell"
    );
    assert!(
        app.terminals[1].selection().is_none(),
        "the pane that merely became active gets no selection"
    );

    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        col + 3,
        row + 1,
    ));
    assert!(
        app.terminals[0].selection().is_some_and(|s| s.has_area()),
        "the release finalises the origin's selection"
    );
    assert!(
        String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()).ends_with('m'),
        "and the origin child gets the release"
    );
    assert!(
        app.terminals[1].written_bytes_for_test().is_empty(),
        "the active pane's child sees nothing"
    );
    assert!(app.terminal_pointer_forwarded.is_none());
}

#[test]
fn a_double_click_over_a_tracking_child_is_two_forwarded_clicks_not_a_word_select() {
    // Both clicks of the pair are the child's, and neither arms croft's
    // double-click tracker, so no word is selected. This is the documented
    // trade: word-select is unavailable over a tracking app.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, col, row, lc, lr) = tracking_terminal_app(b"\x1b[?1000h\x1b[?1006h");
    // The token is put ON the click row (the helper's cell is grid row 2),
    // and the precondition checks it: a word-select on a blank cell selects
    // nothing, so without this the "no word" assertion could not fail.
    app.terminals[0].feed_bytes_for_test(b"\x1b[3;1Hhello_world_token\r\n");
    let (text, idx) = app.terminals[0]
        .line_text_at(col, row)
        .expect("the click must resolve to a grid cell");
    assert!(
        text.contains("hello_world_token") && idx < text.len(),
        "the click must land ON the token: {text:?} at {idx}"
    );
    for _ in 0..2 {
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), col, row));
    }
    let written = String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()).into_owned();
    let pair = format!("\x1b[<0;{c};{r}M\x1b[<0;{c};{r}m", c = lc + 1, r = lr + 1);
    assert_eq!(
        written,
        format!("{pair}{pair}"),
        "two press/release pairs reach the child"
    );
    assert!(
        app.terminals[0].selection().is_none(),
        "no word is selected: the pair was never croft's"
    );
}

#[test]
fn shift_drag_over_a_1002_child_selects_with_croft_and_reports_nothing() {
    // Under 1002 the unshifted drag is the child's; Shift+drag is the way
    // left to select text in a `mouse=a` vim or lazygit, and it must never
    // leak a report.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, col, row, _, _) = tracking_terminal_app(b"\x1b[?1002h\x1b[?1006h");
    let mut ev = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ev.modifiers = KeyModifiers::SHIFT;
    app.handle_mouse(ev);
    let mut ev = mouse(MouseEventKind::Drag(MouseButton::Left), col + 3, row + 1);
    ev.modifiers = KeyModifiers::SHIFT;
    app.handle_mouse(ev);
    let mut ev = mouse(MouseEventKind::Up(MouseButton::Left), col + 3, row + 1);
    ev.modifiers = KeyModifiers::SHIFT;
    app.handle_mouse(ev);
    assert!(
        app.terminals[0].written_bytes_for_test().is_empty(),
        "Shift keeps every event of the gesture from the child"
    );
    assert!(
        app.terminals[0].selection().is_some_and(|s| s.has_area()),
        "and croft's selection has the dragged area"
    );
}

#[test]
fn copy_on_select_fires_for_a_click_only_forwarded_drag() {
    // The forwarded-release tail carries its own copy-on-select, aimed at the
    // origin pane; it must put the origin's selection on the clipboard the
    // way the ordinary mouse-up does.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, _, _, _, _) = tracking_terminal_app(b"\x1b[?1000h\x1b[?1006h");
    app.copy_on_select = true;
    app.terminals[0].feed_bytes_for_test(b"\x1b[H");
    app.terminals[0].feed_bytes_for_test(b"copy-me-please\r\n");
    let inner = app.terminals[0].last_inner;
    let (x0, y0) = (inner.x, inner.y);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x0, y0));
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), x0 + 13, y0));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), x0 + 13, y0));
    assert_eq!(
        crate::clipboard::read_string().as_deref(),
        Some("copy-me-please"),
        "the origin pane's selection landed on the clipboard"
    );
}

#[test]
fn edge_autoscroll_stands_down_while_the_forwarded_pane_is_not_active_and_resumes() {
    // The tick scrolls the ACTIVE pane. With a click-only forwarded drag
    // parked past pane 0's top edge, Cmd+] must not have the tick walk pane
    // 1's scrollback, and Cmd+[ back must let it resume without a new Drag
    // event, since a parked pointer sends none.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.terminals[0].feed_bytes_for_test(b"\x1b[?1000h\x1b[?1006h");
    // Enough history in pane 1 that a stray tick would visibly move it.
    app.terminals[1].feed_bytes_for_test("row\r\n".repeat(200).as_bytes());
    let inner = app.terminals[0].last_inner;
    let (col, row) = (inner.x + 2, inner.y + 1);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert_eq!(app.active_terminal, 0);
    // Park the pointer above the top edge: the drag arms auto-scroll.
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        col,
        inner.y.saturating_sub(1),
    ));
    assert!(
        app.terminal_select_autoscroll.is_some(),
        "precondition: auto-scroll armed"
    );

    app.cycle_terminal();
    assert_eq!(app.active_terminal, 1);
    let pane1_before = app.terminals[1].viewport_top_line();
    std::thread::sleep(std::time::Duration::from_millis(40));
    assert!(
        !app.tick_terminal_autoscroll(),
        "the tick stands down for a non-origin active pane"
    );
    assert_eq!(
        app.terminals[1].viewport_top_line(),
        pane1_before,
        "pane 1's viewport did not move"
    );
    assert!(
        app.terminal_select_autoscroll.is_some(),
        "the armed state is held, not dropped"
    );

    app.cycle_terminal_back();
    assert_eq!(app.active_terminal, 0);
    std::thread::sleep(std::time::Duration::from_millis(40));
    assert!(
        app.tick_terminal_autoscroll(),
        "back on the origin pane the tick resumes without a new Drag"
    );
}

#[test]
fn a_bare_forwarded_click_never_reaches_the_clipboard() {
    // The forwarded-release tail runs copy-on-select only for a gesture the
    // Drag arm turned into croft's selection. A plain click that stayed the
    // child's leaves the clipboard alone even with copy-on-select on.
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, _, _, _, _) = tracking_terminal_app(b"\x1b[?1000h\x1b[?1006h");
    app.copy_on_select = true;
    app.terminals[0].feed_bytes_for_test(b"\x1b[H");
    app.terminals[0].feed_bytes_for_test(b"not-for-the-clipboard\r\n");
    let inner = app.terminals[0].last_inner;
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        inner.x + 2,
        inner.y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        inner.x + 2,
        inner.y,
    ));
    assert_eq!(
        crate::clipboard::read_string().as_deref().unwrap_or(""),
        "",
        "a click that was the child's copies nothing"
    );
    assert!(app.terminals[0].selection().is_none());
}

#[test]
fn edge_autoscroll_is_dropped_when_the_forwarded_pane_closes_mid_gesture() {
    // A parked, armed forwarded drag whose origin pane is then closed has
    // nothing to resume on: the tick drops the gesture instead of standing
    // down until a release that may never come.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.terminals[0].feed_bytes_for_test(b"\x1b[?1000h\x1b[?1006h");
    let inner = app.terminals[0].last_inner;
    let (col, row) = (inner.x + 2, inner.y + 1);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        col,
        inner.y.saturating_sub(1),
    ));
    assert!(
        app.terminal_select_autoscroll.is_some(),
        "precondition: armed"
    );
    assert!(
        app.close_terminal_at(0),
        "the origin pane closes under the held button"
    );
    std::thread::sleep(std::time::Duration::from_millis(40));
    assert!(!app.tick_terminal_autoscroll(), "nothing to scroll");
    assert!(
        app.terminal_select_autoscroll.is_none() && app.terminal_pointer_forwarded.is_none(),
        "the gesture is dropped, not held"
    );
}

#[test]
fn an_untouched_offer_expires_and_a_closed_pane_takes_its_offer_along() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // The refusal memory is read from the real cache directory at startup;
    // this machine's must not decide the test.
    app.remote_offer_refused.clear();
    let pane = app.terminals[0].uid();
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    app.ssh_offer.as_mut().unwrap().since =
        std::time::Instant::now() - SSH_OFFER_TTL - std::time::Duration::from_secs(1);
    app.expire_ssh_offer();
    assert!(
        app.ssh_offer.is_none(),
        "an offer past its TTL clears itself"
    );

    app.split_terminal().unwrap();
    let second = app.terminals[1].uid();
    app.consider_ssh_offer(second, Some(String::from("db-1")));
    assert!(app.ssh_offer.as_ref().is_some_and(|o| o.pane == second));
    assert!(app.close_terminal_at(1));
    app.expire_ssh_offer();
    assert!(
        app.ssh_offer.is_none(),
        "the pane that owned the offer is gone"
    );
    assert!(
        !app.ssh_offer_seen.contains_key(&second),
        "and its per-pane memory went with it"
    );
}

#[test]
fn a_pane_that_moves_to_another_host_loses_the_old_offer_even_if_the_new_one_is_excluded() {
    // `ssh a cmd; ssh b cmd` inside one sample gap: the offer for A must not
    // stay up (and be accepted with Cmd+K G) once the pane is on B, whether
    // or not B itself is offerable.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.remote_offer_refused.clear();
    let pane = app.terminals[0].uid();
    app.remote_offer_excluded = vec![String::from("b")];
    app.consider_ssh_offer(pane, Some(String::from("a")));
    assert!(app.ssh_offer.as_ref().is_some_and(|o| o.host == "a"));
    app.consider_ssh_offer(pane, Some(String::from("b")));
    assert!(
        app.ssh_offer.is_none(),
        "the offer for A fell when the pane moved to B"
    );
    assert!(app.status.is_empty());
}

#[test]
fn switching_the_offer_off_and_on_forgets_what_each_pane_was_last_seen_on() {
    // While the offer is off the label thread ships no samples, so the
    // per-pane memory stops tracking reality. Session A on db-1, offer off,
    // session A ends and session B to db-1 starts, offer on: B is a new
    // session and must be offered, not mistaken for a continuation of A.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.remote_offer_refused.clear();
    let pane = app.terminals[0].uid();
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(app.ssh_offer.is_some());
    let mut prefs = crate::prefs::Prefs {
        disable_remote_offer: true,
        ..Default::default()
    };
    app.apply_merged_settings(&prefs);
    assert!(app.ssh_offer.is_none(), "the switch took the offer down");
    // No `None` sample in between: nothing observed the gap.
    prefs.disable_remote_offer = false;
    app.apply_merged_settings(&prefs);
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(
        app.ssh_offer.as_ref().is_some_and(|o| o.host == "db-1"),
        "the first session seen after a re-enable is offered"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_restore_reattaches_a_shell_still_running_in_its_pane_host() {
    // #694: croft was killed, not quit, so the pane's host still holds its
    // shell. Restoring reattaches to it (same shell, its screen redrawn)
    // instead of starting a fresh one; a pane whose host is gone falls back
    // to a fresh shell as before. The record keeps naming the live host, so
    // a second crash finds it again.
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
        transcript: vec![String::from("old transcript")],
        host,
    };
    let session = tmp.path().join("sessions.json");
    let rec = crate::terminal_session::SessionRecord {
        panes: vec![
            pane("kept", Some(socket.display().to_string())),
            pane(
                "gone",
                Some(tmp.path().join("dead.sock").display().to_string()),
            ),
        ],
        active: 0,
    };
    crate::terminal_session::save_for_root(&session, &root_key, rec).unwrap();

    let mut app = App::new(root.clone()).unwrap();
    app.terminal_session_path = session.clone();
    app.restore_terminal_session_with(true);
    assert_eq!(app.terminals.len(), 2);
    let kept = &app.terminals[0];
    assert_eq!(kept.host_socket(), Some(socket.as_path()), "reattached");
    assert_eq!(kept.manual_name(), Some("kept"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    loop {
        let (lines, _) = kept.grid_lines();
        if lines.iter().any(|l| l.contains("still-42")) {
            assert!(
                !lines.iter().any(|l| l.contains("old transcript")),
                "the live screen, not the saved transcript: {lines:?}"
            );
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{lines:?}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let gone = &app.terminals[1];
    assert_eq!(
        gone.host_socket(),
        None,
        "a dead host falls back to a fresh shell"
    );
    assert_eq!(gone.manual_name(), Some("gone"));

    let saved = crate::terminal_session::load(&session);
    let saved = &saved[&root_key];
    assert_eq!(
        saved.panes[0].host.as_deref(),
        Some(socket.to_str().unwrap())
    );
    assert_eq!(saved.panes[1].host, None);

    // Quitting closes the panes, which ends the reattached shell too.
    drop(app);
    host.join().unwrap().unwrap();
}

#[test]
fn a_view_request_from_a_pane_opens_the_file_in_the_editor() {
    // #362: the point of the whole channel. A shell prompt asks, and the file
    // arrives as an editor tab exactly as an Explorer click would leave it.
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("report.txt");
    std::fs::write(&target, "hello from a pane").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    let sock = seat_view_listener(&mut app, tmp.path());

    let client = view_from_a_pane(&sock, &target);
    drain_until_answered(&mut app, &client);
    assert_eq!(client.join().unwrap(), crate::view_ipc::ViewReply::Ok);

    let open = app.editor.editors[app.editor.active_index()].path.clone();
    assert_eq!(
        open.as_deref(),
        Some(target.as_path()),
        "the requested file must be the active tab"
    );
    assert!(
        app.focus == Pane::Editor,
        "the user asked to look at it, so focus follows"
    );
}

// --- #362 fallback-review blocker: the startup pane's environment ----------

#[test]
fn a_pane_command_carries_the_view_socket_when_one_is_bound() {
    // The blocker this replaces: the socket was published with `set_var`
    // AFTER `App::new` had already built the first pane, and portable-pty
    // snapshots `std::env::vars_os()` inside `CommandBuilder::new` and then
    // clears the child's environment before applying only that snapshot. So
    // `croft view` failed in the startup pane and worked in every pane opened
    // later. No test could see it: `bind_view_socket` returns early under
    // `cfg!(test)` and the server-side helper seats a listener by hand, so
    // nothing exercised the publish path at all.
    //
    // Passing the path IN keeps this pure: a process-global set once inside
    // one test would leak into every other test in the binary.
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    crate::widgets::terminal::apply_pane_env(
        &mut cmd,
        Some(std::path::Path::new("/tmp/croft-view-probe.sock")),
    );
    let carried: Vec<(String, String)> = cmd
        .iter_extra_env_as_str()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert!(
        carried
            .iter()
            .any(|(k, v)| k == crate::view_ipc::SOCK_ENV && v == "/tmp/croft-view-probe.sock"),
        "a pane's command must carry the view socket: {carried:?}"
    );
    // Presence half over the same state: the terminal identity still travels,
    // so this cannot pass by the env application having been removed.
    assert!(
        carried
            .iter()
            .any(|(k, v)| k == "TERM" && v == "xterm-256color"),
        "TERM must still be applied: {carried:?}"
    );
}

#[test]
fn a_pane_command_clears_an_inherited_view_socket_when_none_is_bound() {
    // A croft launched FROM a croft pane inherits the outer croft's
    // `CROFT_VIEW_SOCK` through `CommandBuilder::new`, which seeds itself from
    // `std::env::vars_os()`. If the inner croft's own bind fails, adding
    // nothing is not the same as clearing: its panes keep the PARENT's socket,
    // and `croft view report.pdf` opens the file in the wrong window and exits
    // 0. The field doc promises the opposite ("panes then see no
    // CROFT_VIEW_SOCK and the client says so plainly").
    //
    // Asserted through `iter_full_env_as_str`, which is what the CHILD
    // receives. The sibling test below reads `iter_extra_env_as_str`, which
    // filters out every inherited entry and is therefore structurally blind to
    // this: it can only ever prove nothing was ADDED.
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    cmd.env(crate::view_ipc::SOCK_ENV, "/tmp/outer-croft.sock");
    crate::widgets::terminal::apply_pane_env(&mut cmd, None);
    let carried: Vec<(String, String)> = cmd
        .iter_full_env_as_str()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert!(
        !carried.iter().any(|(k, _)| k == crate::view_ipc::SOCK_ENV),
        "an inherited socket must be CLEARED, not merely not-added, or a \
         nested croft routes `croft view` to its parent: {carried:?}"
    );
    // Presence half over the same view: the terminal identity still reaches
    // the child, so this cannot pass by the whole env having been dropped.
    assert!(
        carried
            .iter()
            .any(|(k, v)| k == "TERM" && v == "xterm-256color"),
        "TERM must still reach the child: {carried:?}"
    );
}

#[test]
fn a_pane_command_omits_the_view_socket_when_none_is_bound() {
    // A croft that failed to bind must not hand its panes a stale or empty
    // path: `croft view` then says the socket is unset, which is true and
    // actionable, rather than failing to connect to a name that is not there.
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    crate::widgets::terminal::apply_pane_env(&mut cmd, None);
    let carried: Vec<(String, String)> = cmd
        .iter_extra_env_as_str()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert!(
        !carried.iter().any(|(k, _)| k == crate::view_ipc::SOCK_ENV),
        "no socket bound means no variable: {carried:?}"
    );
    assert!(
        carried.iter().any(|(k, _)| k == "TERM"),
        "and the pane is still given its terminal identity: {carried:?}"
    );
}

#[test]
fn a_pane_names_croft_as_its_terminal_not_the_host() {
    // #1453: croft in Ghostty handed every pane `TERM_PROGRAM=ghostty`, so
    // `croft plot` there wrote Kitty graphics that croft's emulator drops.
    // Read through `iter_full_env_as_str`, what the child actually gets.
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    cmd.env("TERM_PROGRAM", "ghostty");
    cmd.env("KITTY_WINDOW_ID", "1");
    cmd.env("GHOSTTY_RESOURCES_DIR", "/usr/share/ghostty");
    cmd.env("LC_TERMINAL", "iTerm2");
    cmd.env("PATH_PROBE_1453", "kept");
    crate::widgets::terminal::apply_pane_env(&mut cmd, None);
    let env: std::collections::HashMap<String, String> = cmd
        .iter_full_env_as_str()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(env.get("TERM_PROGRAM").map(String::as_str), Some("croft"));
    assert_eq!(
        env.get("TERM_PROGRAM_VERSION").map(String::as_str),
        Some(env!("CARGO_PKG_VERSION"))
    );
    for host_only in ["KITTY_WINDOW_ID", "GHOSTTY_RESOURCES_DIR", "LC_TERMINAL"] {
        assert!(!env.contains_key(host_only), "{host_only} leaked: {env:?}");
    }
    assert!(
        matches!(
            env.get("CROFT_INLINE_IMAGES").map(String::as_str),
            Some("0" | "1")
        ),
        "a pane says whether its images show: {env:?}"
    );
    // Only the terminal's identity goes: the rest of the environment stays.
    assert_eq!(env.get("PATH_PROBE_1453").map(String::as_str), Some("kept"));
    assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
}

/// #614: a key typed after a suggestion was painted makes it stale; Right
/// before the echo arrives moves the cursor instead of typing the old
/// suggestion's rest after the new key.
#[test]
fn right_does_not_accept_a_terminal_suggestion_the_typing_outran() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.term_suggestion = Some((0, String::from("g"), String::from("it status")));
    app.handle_terminal_key(key(KeyCode::Char('i'), KeyModifiers::NONE));
    app.term_suggestion = Some((0, String::from("g"), String::from("it status")));
    app.handle_terminal_key(key(KeyCode::Right, KeyModifiers::NONE));
    let written = String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()).into_owned();
    assert!(!written.contains("it status"), "{written:?}");
}

/// #678: Claude Code (or tmux, nvim) copying a selection through OSC 52 in
/// a croft pane must land where croft's own copy lands. Dropping it broke
/// copy on a remote box, where OSC 52 is the program's only clipboard.
#[test]
fn a_pane_program_osc52_copy_reaches_the_clipboard_on_the_drain_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    crate::clipboard::test_clip::mock_write("before");
    // "Y29waWVk" is "copied"; the empty store first is a program clearing
    // the clipboard at startup and must not wipe it.
    let script =
        "read x; printf '\\033]52;c;\\007'; read y; printf '\\033]52;c;Y29waWVk\\007'; sleep 30";
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from(script)],
        tmp.path(),
    )
    .unwrap();
    app.terminals[0].write_input(b"\n");
    // The empty store has no visible output to wait on, so give it the
    // same budget the real copy gets and require that nothing changed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
    while std::time::Instant::now() < deadline {
        app.drain_terminal_bells();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        crate::clipboard::test_clip::mock_read().as_deref(),
        Some("before"),
        "an empty OSC 52 store must not wipe the clipboard"
    );
    app.terminals[0].write_input(b"\n");
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the pane's OSC 52 copy to reach the clipboard",
        || {
            app.drain_terminal_bells();
            crate::clipboard::test_clip::mock_read().as_deref() == Some("copied")
        },
    );
    assert!(
        app.status.starts_with("Copied 6 chars"),
        "the copy is reported like croft's own, status: {}",
        app.status
    );
}

/// #356: a terminal recording has its own status-bar badge with the time
/// it has run, apart from the macro recorder's REC.
#[test]
fn a_terminal_recording_shows_its_own_badge_until_it_stops() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert!(!status_row(&term).contains("CAST"));
    app.toggle_session_recording();
    assert!(app.recording.is_some(), "{}", app.status);
    // The start message names the file; the badge must stand on its own.
    app.status.clear();
    term.draw(|f| app.render(f)).unwrap();
    let row = status_row(&term);
    assert!(row.contains("● CAST 0:00"), "{row}");
    assert!(!row.contains("REC "), "not the macro badge: {row}");
    app.toggle_session_recording();
    term.draw(|f| app.render(f)).unwrap();
    assert!(!status_row(&term).contains("CAST"));
}

/// A recorded row's width as a player draws it: its characters with the
/// SGR and cursor escapes (#356) taken out, since those occupy no column.
fn visible_width(row: &str) -> usize {
    crate::remote_connect::strip_ansi(row.as_bytes())
        .chars()
        .count()
}

/// The output frames of the one cast `app` wrote into its workspace.
fn recorded_frames(app: &App) -> Vec<String> {
    let path = std::fs::read_dir(app.workspace_root())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "cast"))
        .expect("a .cast file was written");
    std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .skip(1)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|ev| ev[1] == "o")
        .filter_map(|ev| ev[2].as_str().map(String::from))
        .collect()
}

/// #356: a recorded frame keeps the colours the pane showed, as SGR per
/// run, and ends by putting the player's cursor where the pane's was, or
/// hiding it, so each frame sets the cursor state rather than inheriting it.
#[test]
fn a_recorded_frame_keeps_the_panes_colours_and_cursor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0].last_inner = ratatui::layout::Rect {
        x: 1,
        y: 1,
        width: 60,
        height: 10,
    };
    app.terminals[0].resize(60, 10);
    // The pane runs a live shell: let its prompt land first, or a late one
    // moves the cursor between the recording and the read below.
    let mut last = app.terminals[0].screen_ansi_wrapped();
    let mut quiet_since = std::time::Instant::now();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while quiet_since.elapsed() < std::time::Duration::from_millis(500)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(25));
        let now = app.terminals[0].screen_ansi_wrapped();
        if now != last {
            last = now;
            quiet_since = std::time::Instant::now();
        }
    }
    app.terminals[0].feed_bytes_for_test(b"\r\n\x1b[1;31mQQRED\x1b[0m plain\r\n");

    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    app.record_active_screen();
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);

    let frames = recorded_frames(&app);
    let frame = frames.first().expect("an output event");
    assert!(
        frame.contains("\u{1b}[1;31mQQRED\u{1b}[0m plain"),
        "the red, bold run is recorded as SGR: {frame:?}"
    );
    let shown = frame.rsplit_once('\u{1b}').map(|(_, tail)| tail);
    assert!(
        matches!(shown, Some("[?25h" | "[?25l")),
        "the frame ends by setting the cursor: {frame:?}"
    );
    if shown == Some("[?25h") {
        let (row, col) = app.terminals[0].screen_ansi_wrapped().1.unwrap();
        assert!(
            frame.ends_with(&format!("\u{1b}[{};{}H\u{1b}[?25h", row + 1, col + 1)),
            "the cursor is put back where the pane has it: {frame:?}"
        );
    }
}

#[test]
fn a_hidden_cursor_is_recorded_hidden() {
    assert_eq!(super::cursor_suffix(None), "\u{1b}[?25l");
    assert_eq!(super::cursor_suffix(Some((0, 0))), "\u{1b}[1;1H\u{1b}[?25h");
    assert_eq!(super::cursor_suffix(Some((3, 6))), "\u{1b}[4;7H\u{1b}[?25h");
}

/// #356: a row a redact rule touches is written as its masked text and
/// gives up its colour, since a mask spliced between escapes could break
/// them; the rows around it keep theirs.
#[test]
fn a_masked_row_is_recorded_plain_and_its_neighbours_in_colour() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0].last_inner = ratatui::layout::Rect {
        x: 1,
        y: 1,
        width: 60,
        height: 10,
    };
    app.terminals[0].resize(60, 10);
    let key = "AKIAIOSFODNN7EXAMPLE";
    app.terminals[0].feed_bytes_for_test(
        format!("\r\n\x1b[32mexport KEY={key}\x1b[0m\r\n\x1b[34mQQBLUE\x1b[0m\r\n").as_bytes(),
    );

    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    app.record_active_screen();
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);

    let frames = recorded_frames(&app);
    let frame = frames.first().expect("an output event");
    assert!(!frame.contains(key), "the key reached the cast: {frame:?}");
    let masked = frame
        .split("\r\n")
        .find(|row| row.contains("export KEY="))
        .expect("the masked row is recorded");
    assert!(
        !masked.contains('\u{1b}'),
        "the masked row carries no escapes: {masked:?}"
    );
    assert!(
        frame.contains("\u{1b}[34mQQBLUE\u{1b}[0m"),
        "the next row keeps its colour: {frame:?}"
    );
}

/// #356: a run of one colour stays one SGR run across the soft-wrap column
/// and across a wide character. Both carry cell flags that are layout, not
/// style, and keyed on those the run was reset and set again around them.
#[test]
fn a_colour_run_is_not_split_by_the_wrap_column_or_a_wide_char() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0].resize(20, 6);
    let long = "y".repeat(30);
    app.terminals[0].feed_bytes_for_test(
        format!("\r\n\x1b[33m{long}\x1b[0m\r\n\x1b[32mab\u{4e2d}cd\x1b[0m\r\n").as_bytes(),
    );
    let (rows, _) = app.terminals[0].screen_ansi_wrapped();
    let ansi: Vec<&str> = rows.iter().map(|(_, a, _)| a.as_str()).collect();
    let first = format!("\x1b[33m{}\x1b[0m", "y".repeat(20));
    assert!(
        ansi.contains(&first.as_str()),
        "the wrapped row is one run: {ansi:?}"
    );
    assert!(
        ansi.contains(&"\x1b[32mab\u{4e2d}cd\x1b[0m"),
        "the wide char sits inside its run: {ansi:?}"
    );
}

/// Negatives (#1580): Esc on the Save As prompt keeps the tab untitled with
/// its text; `Ctrl`+`N` in a terminal is the shell's and in vim mode is
/// vim's, so neither opens a tab; `Ctrl`+`Shift`+`N` is still not it.
#[test]
fn ctrl_n_leaves_terminals_vim_and_shifted_chords_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "b.txt", "keep\n");
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::SUPER))
        .unwrap();
    type_str(&mut app, "note");
    app.save();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.prompt.is_none());
    assert_eq!(app.editor.path, None);
    assert_eq!(app.editor.lines[0], "note");
    assert!(app.editor.dirty);

    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "b.txt", "keep\n");
    app.handle_key(key(
        KeyCode::Char('n'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    app.handle_key(key(
        KeyCode::Char('n'),
        KeyModifiers::SUPER | KeyModifiers::ALT,
    ))
    .unwrap();
    app.focus = Pane::Terminal;
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::CONTROL))
        .unwrap();
    app.focus_pane(Pane::Editor);
    app.vim.enabled = true;
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.tab_count(), 1, "no untitled tab opened");
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("b.txt").as_path())
    );
}

#[test]
fn a_multiline_paste_into_a_plain_shell_waits_for_confirmation() {
    let (mut app, _tmp) = plain_paste_app();
    app.handle_paste(TWO_LINES);
    assert_eq!(pasted(&app), "", "nothing runs before the user agrees");
    assert!(app.pending_terminal_paste.is_some());
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(pasted(&app), TWO_LINES);
    assert!(app.pending_terminal_paste.is_none());
}

#[test]
fn a_click_while_asking_cannot_move_the_paste_to_another_pane() {
    // The paste was checked against the active pane; a click on another
    // pane's rail row must not send it there instead.
    let (mut app, _tmp) = two_pane_paste_app();
    app.handle_paste(TWO_LINES);
    let other = app.terminal_rail_rects[1];
    left_click(&mut app, other.x + 1, other.y);
    assert_eq!(app.active_terminal, 0, "the click is ignored while asking");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(pasted(&app), TWO_LINES, "the checked pane gets the paste");
}

/// A quiet 30-column pane holding `text`, with the find bar open on it.
fn wrapped_find_app(text: &str) -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sleep",
        &[String::from("60")],
        tmp.path(),
    )
    .unwrap();
    app.terminals[0].resize(30, 10);
    app.terminals[0].feed_bytes_for_test(format!("\x1b[2J\x1b[H{text}").as_bytes());
    app.focus_pane(Pane::Terminal);
    app.open_terminal_find();
    (tmp, app)
}

fn terminal_find_count(app: &mut App, query: &str) -> usize {
    app.terminal_find_set_query(query.to_string());
    app.terminal_find.as_ref().unwrap().match_count
}

/// The background the pane paints at `col` of the `row`th screen row of
/// the fed text (the pane's run-label header may sit above it).
fn terminal_cell_bg(app: &mut App, row: u16, col: u16) -> ratatui::style::Color {
    let area = ratatui::layout::Rect::new(0, 0, 32, 12);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut app.terminals[0], area, &mut buf);
    let first = (1..area.height - 1)
        .find(|&y| buf[(1, y)].symbol() == "x")
        .expect("the fed text is on screen");
    buf[(col + 1, first + row)].bg
}

/// A match the pane's soft wrap cut in two is found once, and both halves
/// are painted as the active match (#1275).
#[test]
fn terminal_find_matches_across_a_soft_wrap() {
    let line = format!("{}connection-refused port 5432\r\n", "x".repeat(24));
    let (_tmp, mut app) = wrapped_find_app(&line);
    assert_eq!(terminal_find_count(&mut app, "connection-refused"), 1);
    assert_eq!(
        app.terminal_find.as_ref().unwrap().match_index,
        Some(1),
        "1 of 1"
    );
    let reset = ratatui::style::Color::Reset;
    // `connec` ends row 0; `tion-refused` starts row 1.
    let first_half = terminal_cell_bg(&mut app, 0, 26);
    let second_half = terminal_cell_bg(&mut app, 1, 8);
    assert_ne!(first_half, reset, "the half before the wrap is painted");
    assert_eq!(first_half, second_half, "both halves are the active match");
    assert_eq!(terminal_cell_bg(&mut app, 0, 10), reset, "the x run is not");
    assert_eq!(terminal_cell_bg(&mut app, 1, 14), reset, "nor is `port`");
}

/// Negative: rows that merely sit next to each other, with no soft wrap
/// between them, never match across their boundary, and a word that ends
/// exactly at the wrap is counted once.
#[test]
fn terminal_find_never_joins_rows_without_a_wrap_and_counts_an_edge_match_once() {
    let text = format!("{}connec\r\ntion-refused\r\n", "x".repeat(24));
    let (_tmp, mut app) = wrapped_find_app(&text);
    assert_eq!(terminal_find_count(&mut app, "connection-refused"), 0);
    assert_eq!(
        terminal_cell_bg(&mut app, 0, 26),
        ratatui::style::Color::Reset
    );
    assert_eq!(terminal_find_count(&mut app, "connec"), 1);

    let edge = format!("{}abcdef{}\r\n", "x".repeat(24), "y".repeat(10));
    let (_tmp, mut app) = wrapped_find_app(&edge);
    assert_eq!(terminal_find_count(&mut app, "abcdef"), 1);
    assert_eq!(
        terminal_find_count(&mut app, "fyy"),
        1,
        "and across the edge"
    );
}

#[test]
fn the_terminal_tab_is_still_announced_as_its_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus = Pane::Terminal;
    app.bottom_panel_tab = BottomPanelTab::Terminal;
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Terminal 1");
    assert_eq!(snap.item, None);
}

/// #1488 negative: a command that prints nothing (`clear`, `cd`) leaves the
/// pane's problems as they were; only a scan of real output replaces them.
#[test]
fn a_command_with_no_output_keeps_the_panes_problems() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let pane = app.run_project_task(long_task("make")).unwrap();
    let cwd = tmp.path().to_path_buf();
    assert!(app.apply_build_scan(
        pane,
        Some(&cwd),
        "make",
        "main.c:7:3: error: expected ';'\n"
    ));
    finish_pane_command(&mut app, pane, Some(0), 10);
    assert_eq!(app.problems.groups().len(), 1, "still listed");
    // And the skip still covers the watcher's own finish when it does
    // report output.
    app.watch_published_panes.insert(pane);
    assert!(!app.apply_build_scan(pane, Some(&cwd), "tsc --watch", "a.c:1:1: error: old\n"));
    assert!(app.apply_build_scan(pane, Some(&cwd), "make", "b.c:1:1: error: new\n"));
}
