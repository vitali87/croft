//! Getting around: go back and forward, breadcrumbs, the outline, symbols
//! and file links.

use super::*;

/// SGR mouse reporting carries only shift/meta/ctrl — there is no super bit
/// (crossterm `parse_cb`), so Cmd+click and Option+click are the same event and
/// Go to Definition cannot share ALT with VS Code's multi-cursor. ALT is the
/// caret modifier; CTRL is go-to, matching the terminal pane's file/URL click.
#[test]
fn alt_click_adds_a_caret_and_ctrl_click_goes_to_definition() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("t.rs");
    std::fs::write(&f, "fn main() {}\nlet x = 1;\nlet y = 2;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    // A body column clear of the gutter, on a different row from the caret.
    let col = app.editor.last_inner.x + 8;
    let row = app.editor.last_inner.y + 2;

    let mut alt = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    alt.modifiers = KeyModifiers::ALT;
    app.handle_mouse(alt);
    assert!(
        !app.editor.carets.is_empty(),
        "Alt+click must add a secondary caret, not go to definition"
    );

    // Ctrl+click is go-to-definition. With no language server attached the
    // jump cannot land, so what is pinned is that it does NOT add a caret.
    let before = app.editor.carets.len();
    let mut ctrl = mouse(MouseEventKind::Down(MouseButton::Left), col, row + 1);
    ctrl.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ctrl);
    assert_eq!(
        app.editor.carets.len(),
        before,
        "Ctrl+click is Go to Definition and must never add a caret"
    );
}

#[test]
fn ctrl_click_on_a_line_range_selects_the_range() {
    // #804: `path:62-69` opens with lines 62 to 69 selected, the view on 62.
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("t.py");
    std::fs::write(&file, "l1\nl2\nl3\nline four\nl5\nl6\n").unwrap();
    let log = tmp.path().join("agent.log");
    // An escape makes it a rendered log, where Ctrl+click follows references.
    std::fs::write(&log, "\x1b[1mt.py:2-4\x1b[0m has the case\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    assert!(
        app.editor.log.is_some(),
        "the log opens in the rendered view"
    );
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let body = app.editor.log.as_ref().unwrap().last_body;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: body.x + 6,
        row: body.y,
        modifiers: KeyModifiers::CONTROL,
    });
    assert_eq!(app.editor.path.as_deref(), Some(file.as_path()));
    let sel = app.editor.selection.expect("the range is selected");
    assert_eq!(sel.normalised(), ((1, 0), (3, 9)), "lines 2 to 4, whole");
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 0));
    assert!(app.status.ends_with(":2-4"), "{}", app.status);
}

/// A `path:line` printed in a RENDERED ANSI log is Ctrl+clickable, the
/// same as one printed in a terminal pane (#257).
///
/// The reference is scanned against the text the user SEES. The fixture is
/// built so that matters: the second path sits after a 36-character SGR run
/// with no printed width, so its visible column falls INSIDE an escape
/// sequence in the raw bytes. A raw-bytes scan finds no reference at that
/// column at all, which is what makes this test turn on the mapping rather
/// than merely coexist with it.
///
/// Ctrl is what separates it from a selection drag: `begin_log_selection`
/// runs before the editor's Ctrl handling, so without the guard the modifier
/// click would just start a selection.
#[test]
fn a_file_reference_in_a_rendered_log_is_ctrl_clickable() {
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("first.rs");
    let second = tmp.path().join("second.rs");
    std::fs::write(&first, "a\nb\nc\n").unwrap();
    std::fs::write(&second, "one\ntwo\nthree\nfour\nfive\nsix\nseven\n").unwrap();

    // An SGR run with zero printed width, placed BETWEEN the two references
    // so the second one's screen column and byte offset diverge. It must be
    // LONGER than the path itself, or the shifted column still lands inside
    // the raw path token and both implementations agree - tempdir paths are
    // ~50 characters and vary per platform, so scale it rather than picking
    // a constant that silently stops discriminating. The assertion below
    // proves the fixture actually discriminates.
    let esc = "\x1b[1;3;4;38;2;255;0;0m".repeat(second.display().to_string().len() / 10 + 4);
    let log = tmp.path().join("build.log");
    std::fs::write(
        &log,
        format!(
            "{}:1 {esc}then {}:7 end\nconnecting to host.com:443\n",
            first.display(),
            second.display()
        ),
    )
    .unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    assert!(
        app.editor.log.is_some(),
        "the log must open in the rendered view, or this tests nothing"
    );

    let backend = ratatui::backend::TestBackend::new(200, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let body = app.editor.log.as_ref().unwrap().last_body;

    let visible = |app: &App, line: usize| -> String {
        app.editor
            .log
            .as_ref()
            .unwrap()
            .visible_text(line)
            .expect("line is parsed")
            .to_string()
    };

    // The second reference: visible column lands inside an escape in the raw
    // bytes, so only the escape-free scan can resolve it.
    let text = visible(&app, 0);
    let at = text.find("second.rs").expect("visible after stripping");
    let raw = std::fs::read_to_string(&log).unwrap();
    let raw_line = raw.lines().next().unwrap();
    assert!(
        crate::file_ref::file_ref_at(raw_line, at)
            .is_none_or(|fr| fr.path != second.display().to_string()),
        "fixture is wrong: a raw-bytes scan at column {at} already finds this \
         reference, so the test would pass without the escape-free mapping"
    );

    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: body.x + u16::try_from(at).unwrap(),
        row: body.y,
        modifiers: KeyModifiers::CONTROL,
    });
    assert_eq!(
        app.editor.path.as_deref(),
        Some(second.as_path()),
        "Ctrl+click on a path after an escape run opens that file"
    );
    assert_eq!(
        app.editor.cursor_row, 6,
        "and lands on the referenced line (`:7`, 0-based)"
    );

    // A lookalike must NOT be opened: only `is_file()` separates
    // `host.com:443` from a real reference, and nothing else asserts it.
    app.editor.open(&log).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let host_line = visible(&app, 1);
    let hat = host_line
        .find("host.com")
        .expect("the lookalike is on line 1");
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: body.x + u16::try_from(hat).unwrap(),
        row: body.y + 1,
        modifiers: KeyModifiers::CONTROL,
    });
    assert_eq!(
        app.editor.path.as_deref(),
        Some(log.as_path()),
        "`host.com:443` resolves to no file, so the click must not navigate"
    );
    assert!(
        app.editor.log.as_ref().is_some_and(|l| l.dragging),
        "and it falls through to a selection drag rather than being eaten"
    );

    // Without Ctrl the same cell still starts a selection drag. `has_selection`
    // needs a != b, so a press alone is not yet a selection: the anchor IS the
    // head until a drag moves it.
    app.editor.open(&log).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: body.x + u16::try_from(at).unwrap(),
        row: body.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        app.editor.path.as_deref(),
        Some(log.as_path()),
        "a plain click stays in the log"
    );
    assert!(
        app.editor.log.as_ref().is_some_and(|l| l.dragging),
        "a plain click still begins a selection drag"
    );
}

#[test]
fn a_prefix_click_defers_to_the_builtin_for_a_file_reference_too() {
    // The built-in Ctrl+click tries `terminal_url_click` OR
    // `terminal_file_click`. A swallow guard that consults only the URL half
    // still eats the file jump -- the same defect one branch over, which is
    // exactly the shape that produced this PR's major 2. Both arms or neither.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("target_file.rs"), "fn main() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(
        r#"[{"key": "ctrl+double_click", "command": "toggle_word_wrap", "when": "terminal"}]"#,
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let area = app.terminals[0].last_area;
    assert!(
        area.width > 12 && area.height > 2,
        "terminal must be laid out"
    );
    // The same class as the OSC 8 tests, reached through `terminal_file_click`
    // rather than `terminal_url_click`. The audit that found the others was
    // scoped to "tests that feed an OSC 8 link", which excludes a bare file
    // reference BY CONSTRUCTION - so the scope, not the code, is what hid it.
    app.terminals[0].feed_bytes_for_test(b"a prompt got here first\r\ntarget_file.rs:1:1\r\n");
    term.draw(|f| app.render(f)).unwrap();

    let (col, row) = cell_carrying(&app.terminals[0], "target_file.rs:1:1")
        .expect("the pane must carry THIS test's file reference");
    // Precondition: a file REFERENCE must be under the cursor, or the guard
    // declines for "nothing there" and the test says nothing about the file
    // half specifically.
    let has_ref = app.terminals[0]
        .line_text_at(col, row)
        .is_some_and(|(t, c)| crate::file_ref::file_ref_at(&t, c).is_some());
    assert!(
        has_ref,
        "the cell must carry a file reference, or this test cannot distinguish \
         the file arm from the URL arm"
    );

    let mut ctrl = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ctrl.modifiers = KeyModifiers::CONTROL;
    app.handle_mouse(ctrl);

    // The built-in ran: it opened the file rather than the click being
    // swallowed as a double-click prefix.
    assert!(
        app.editor
            .path
            .as_deref()
            .is_some_and(|p| p.ends_with("target_file.rs")),
        "the prefix click must fall through to the built-in file jump, but the \
         editor holds {:?}",
        app.editor.path.as_deref()
    );
}

#[test]
fn outline_scrollbar_drag_is_not_hijacked_by_the_sidebar_splitter() {
    use crate::lsp::manager::{OutlineKind, OutlineSymbol};
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "fn main() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Hermetic: App::new reads the user's real config; force the documented
    // default views so this test never depends on a personal Outline-off pref.
    app.explorer_views = Default::default();
    app.editor.open_pinned(&f).unwrap();
    // Enough symbols to overflow the OUTLINE's capped half-height so a scrollbar
    // is drawn; expand the panel so the body (and its bar) render.
    let syms: Vec<_> = (0..200u32)
        .map(|i| OutlineSymbol {
            name: format!("f{i}"),
            detail: None,
            kind: OutlineKind::Function,
            depth: 0,
            line: i,
            character: 0,
            range_start_line: i,
            range_end_line: i,
            range_start_character: 0,
            range_end_character: u32::MAX,
        })
        .collect();
    app.outline.set_symbols(f.clone(), syms);
    app.outline.collapsed = false;

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    let bar = app.outline.last_scrollbar;
    assert!(
        bar.width > 0 && bar.height > 0,
        "the overflowing outline must draw a scrollbar"
    );
    // The outline panel has no right border, so its scrollbar lands on the
    // sidebar's rightmost column, inside the splitter's two-cell grab zone. A
    // press there must drive the outline's bar, not start a sidebar resize.
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        bar.x,
        bar.y + bar.height - 1,
    ));
    assert!(
        app.splitter_drag.is_none(),
        "pressing the outline scrollbar must not start a sidebar splitter drag"
    );
    assert!(
        app.outline_scrollbar_drag,
        "pressing the outline scrollbar must arm the outline scrollbar drag"
    );
}

/// #1561: Go Forward after Go Back keeps both files open the same way.
#[test]
fn go_forward_after_go_back_keeps_both_files_open() {
    let (_tmp, mut app, mc, main, mh) = preview_nav_fixture();
    app.editor.open_preview(&main).unwrap();
    app.go_to_definition(mh.clone(), 1, 4);
    app.nav_back();
    app.nav_forward();
    assert_eq!(app.editor.path.as_deref(), Some(mh.as_path()));
    assert_eq!(open_tab_paths(&app), vec![mc, main, mh]);
}

/// Go Back to a location whose file has since become unopenable keeps the
/// preview tab a preview too.
#[test]
fn a_failed_go_back_leaves_the_preview_tab_a_preview() {
    let (tmp, mut app, _mc, main, _mh) = preview_nav_fixture();
    let doomed = tmp.path().join("doomed");
    std::fs::create_dir(&doomed).unwrap();
    app.nav.record(NavLoc {
        path: doomed,
        row: 0,
        col: 0,
    });
    app.editor.open_preview(&main).unwrap();
    app.nav_back();
    assert_eq!(app.editor.path.as_deref(), Some(main.as_path()));
    assert!(app.editor.is_preview(app.editor.active_index()));
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

/// Go Back, as Ctrl+- types it.
fn press_go_back(app: &mut App) {
    app.handle_key(key(KeyCode::Char('-'), KeyModifiers::CONTROL))
        .unwrap();
}

/// #1635: a file opened from Quick Open, a Search result or the Explorer
/// records where the user left, so Go Back returns there.
#[test]
fn go_back_returns_to_the_file_left_by_any_open() {
    let at = |app: &App| (app.editor.path.clone(), app.editor.cursor_row);
    // Quick Open.
    let (_tmp, mut app, a, b) = nav_switch_fixture();
    quick_open_typing(&mut app, "b.txt");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.path.as_deref(), Some(b.as_path()));
    press_go_back(&mut app);
    assert_eq!(at(&app), (Some(a.clone()), 39), "{}", app.status);

    // A Search result.
    let (_tmp, mut app, a, b) = nav_switch_fixture();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("needle");
    app.search.run_query();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.path.as_deref(), Some(b.as_path()));
    press_go_back(&mut app);
    assert_eq!(at(&app), (Some(a.clone()), 39), "{}", app.status);

    // The Explorer.
    let (_tmp, mut app, a, b) = nav_switch_fixture();
    app.set_sidebar_view(SidebarView::Explorer);
    app.focus_pane(Pane::Tree);
    app.tree.selected = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path == b)
        .expect("b.txt is listed");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.path.as_deref(), Some(b.as_path()));
    press_go_back(&mut app);
    assert_eq!(at(&app), (Some(a), 39), "{}", app.status);
}

/// #1635: Go to Line, then Quick Open, then Back twice visits the line the
/// file was left on, then the line before the Go to Line; Forward returns.
#[test]
fn go_back_walks_a_go_to_line_then_a_file_switch_in_order() {
    let (_tmp, mut app, a, b) = nav_switch_fixture();
    quick_open_typing(&mut app, ":80");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 79, "{}", app.status);
    quick_open_typing(&mut app, "b.txt");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    press_go_back(&mut app);
    assert_eq!(
        (app.editor.path.clone(), app.editor.cursor_row),
        (Some(a.clone()), 79)
    );
    press_go_back(&mut app);
    assert_eq!(
        (app.editor.path.clone(), app.editor.cursor_row),
        (Some(a), 39)
    );
    app.run_command(crate::widgets::command_palette::Command::NavigateForward);
    app.run_command(crate::widgets::command_palette::Command::NavigateForward);
    assert_eq!(
        app.editor.path.as_deref(),
        Some(b.as_path()),
        "{}",
        app.status
    );
}

/// #1635 negative: moving through a file with the keys records nothing,
/// and Go Back itself is not recorded as a switch.
#[test]
fn moving_within_a_file_records_no_go_back_location() {
    let (_tmp, mut app, a, _b) = nav_switch_fixture();
    for code in [
        KeyCode::PageDown,
        KeyCode::PageDown,
        KeyCode::Down,
        KeyCode::Up,
    ] {
        app.handle_key(key(code, KeyModifiers::NONE)).unwrap();
    }
    app.handle_key(key(KeyCode::End, KeyModifiers::CONTROL))
        .unwrap();
    press_go_back(&mut app);
    assert_eq!(app.status, "No previous location");
    assert_eq!(app.editor.path.as_deref(), Some(a.as_path()));
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
fn workspace_symbol_queries_debounce_instead_of_firing_per_keystroke() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_workspace_symbols("");
    if let Some(p) = app.workspace_symbols.as_mut() {
        p.query.push('a');
    }
    app.workspace_symbols_requery();
    assert!(
        app.ws_symbols_request_id.is_none(),
        "a keystroke must not dispatch workspace/symbol synchronously"
    );
    std::thread::sleep(std::time::Duration::from_millis(250));
    app.workspace_symbols_dispatch_due();
    assert!(
        app.ws_symbols_request_id.is_some() || app.workspace_symbols.as_ref().unwrap().unsupported,
        "the settled query must dispatch (or surface no-LSP) after the window"
    );
}

#[test]
fn a_reply_to_a_superseded_workspace_symbol_query_is_dropped() {
    // Type, let the request dispatch, then type again: the in-flight
    // reply answers a query the user has edited away from and must not
    // render (workspace/symbol on a big project outlives the debounce).
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_workspace_symbols("");
    if let Some(p) = app.workspace_symbols.as_mut() {
        p.query.push('a');
    }
    app.workspace_symbols_requery();
    std::thread::sleep(std::time::Duration::from_millis(250));
    app.workspace_symbols_dispatch_due();
    let id = app
        .ws_symbols_request_id
        .expect("the settled query dispatched");
    if let Some(p) = app.workspace_symbols.as_mut() {
        p.query.push('b');
    }
    app.workspace_symbols_requery();
    assert!(
        !app.apply_workspace_symbols(id, Vec::new(), false),
        "a reply for the superseded query must be dropped, not rendered"
    );
}

#[test]
fn clearing_the_workspace_symbol_query_clears_the_unsupported_notice() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_workspace_symbols("");
    if let Some(p) = app.workspace_symbols.as_mut() {
        p.unsupported = true;
        p.query.clear();
    }
    app.workspace_symbols_requery();
    assert!(
        !app.workspace_symbols.as_ref().unwrap().unsupported,
        "an emptied query shows the type-to-search hint, not a stale notice"
    );
}

#[test]
fn cmd_click_on_a_file_line_reference_in_the_terminal_opens_the_file_there() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/probe.rs"), "one\ntwo\nthree\nfour\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Render once so `terminal.last_inner` / `last_area` are populated and
    // mouse coordinates map to grid cells like on a live click.
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    app.terminal_mut().focused = true;
    term.draw(|f| app.render(f)).unwrap();

    let reference = "src/probe.rs:3:2";
    app.terminal_mut()
        .write_input(format!("printf 'ref {reference}\\n'\r").as_bytes());
    // The echoed command line also contains the reference; wait for the
    // OUTPUT line (the one starting with "ref "), then click that one.
    let is_output_line = |l: &str| l.trim_start().starts_with("ref src/probe.rs");
    // A pure liveness backstop, generous on purpose (#44): the shell does
    // its full interactive startup on a box the parallel suite saturates,
    // and a tight ceiling here measured the scheduler, not croft — the
    // one observed failure had the echoed command on screen but the
    // output line not yet flushed. A genuine delivery failure still trips
    // this, just later.
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(30) {
        if app.terminal().visible_text().lines().any(is_output_line) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    term.draw(|f| app.render(f)).unwrap();
    let snapshot = app.terminal().visible_text();
    let row_idx = snapshot
        .lines()
        .position(is_output_line)
        .unwrap_or_else(|| panic!("shell must print the reference; got:\n{snapshot}"));
    let line = snapshot.lines().nth(row_idx).unwrap();
    let col = line.find("probe.rs").unwrap() + 2;

    let inner = app.terminal().last_inner;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: inner.x + col as u16,
        row: inner.y + row_idx as u16,
        modifiers: KeyModifiers::SUPER,
    });

    // Canonicalize both sides: on macOS the pane cwd resolves through the
    // /var → /private/var symlink while tempdir reports the alias.
    let opened = app
        .editor
        .path
        .as_deref()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()));
    assert_eq!(
        opened,
        Some(
            tmp.path()
                .join("src/probe.rs")
                .canonicalize()
                .expect("probe file exists")
        ),
        "Cmd+click on path:line:col must open the file (status: {})",
        app.status
    );
    assert_eq!(app.editor.cursor_row, 2, "line 3 is 0-based row 2");
    assert_eq!(app.editor.cursor_col, 1, "col 2 is 0-based col 1");
    assert!(
        matches!(app.focus, Pane::Editor),
        "the jump must focus the editor"
    );
}

/// #317, the reported gesture end to end: a ctrl+click that matches no link
/// falls past the URL and file arms into the built-in selection path. That
/// path used to arm the double-click tracker regardless of modifiers, so the
/// user's NEXT ordinary click at the same cell classified as a double and
/// word-selected text they never double-clicked.
///
/// The probe word is deliberately link-free and path-free, so the ctrl+click
/// genuinely reaches the fall-through arm rather than being consumed above it
/// - a click that IS consumed would make this test pass without the fix.
#[test]
fn a_ctrl_click_that_opens_nothing_does_not_arm_the_terminal_double_click() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.focus_pane(Pane::Terminal);
    let probe = format!("croftmodclick{}", std::process::id());
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

    let click = |app: &mut App, modifiers| {
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: screen_x,
            row: screen_y,
            modifiers,
        });
    };

    // The ctrl+click must actually fall through: nothing on this word is a
    // URL or an existing file, so no link opens and no selection is made yet.
    click(&mut app, KeyModifiers::CONTROL);
    // The user's next ordinary click, at the same cell.
    click(&mut app, KeyModifiers::NONE);

    let sel = app.terminal().selection_text();
    assert!(
        !sel.contains(&probe),
        "a plain click after an unmatched ctrl+click must not word-select: the \
         ctrl+click must not have armed the double-click tracker, but selection was {sel:?}"
    );

    // The guard must not have cost the real gesture: two plain clicks still
    // word-select at the same cell.
    click(&mut app, KeyModifiers::NONE);
    let sel = app.terminal().selection_text();
    assert!(
        sel.contains(&probe) || probe.contains(&sel),
        "two ordinary clicks must still word-select; got {sel:?}"
    );
}

/// Issue #210: Go Forward. After Go Back (Ctrl+-), Ctrl+Shift+- must
/// return to where the user came from, at the same cursor position.
#[test]
fn ctrl_shift_minus_goes_forward_after_back() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "fn a() {}\n").unwrap();
    std::fs::write(
        &b,
        "fn b() {}\nfn b2() {}\nfn b3() {}\nfn b4() {}\nfn b5() {}\nfn b6() {}\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    // Jump (records a.rs:0 on the back stack, like go-to-definition).
    app.go_to_definition(b.clone(), 5, 0);
    assert_eq!(app.editor.path.as_deref(), Some(b.as_path()));
    assert_eq!(app.editor.cursor_row, 5);
    // Back returns to a.rs.
    app.handle_key(key(KeyCode::Char('-'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.path.as_deref(), Some(a.as_path()));
    // Forward must return to b.rs:5.
    app.handle_key(key(
        KeyCode::Char('-'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(b.as_path()),
        "Ctrl+Shift+- must go forward to the jump target"
    );
    assert_eq!(app.editor.cursor_row, 5, "forward restores the position");
}

/// Legacy encodings fold Ctrl+- into 0x1F (`_` + CONTROL, no SHIFT); that
/// spelling must stay Go BACK, never forward.
#[test]
fn legacy_ctrl_underscore_without_shift_stays_go_back() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "fn a() {}\n").unwrap();
    std::fs::write(&b, "fn b() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    app.go_to_definition(b.clone(), 0, 0);
    app.handle_key(key(KeyCode::Char('_'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(a.as_path()),
        "bare Ctrl+_ (legacy Ctrl+-) must still go back"
    );
}

/// Issue #210: Go to Last Edit Location (Cmd+K Cmd+Q). After editing one
/// file and navigating elsewhere, the chord returns to the edit point.
#[test]
fn cmd_k_cmd_q_jumps_to_the_last_edit_location() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "fn a() {}\nfn a2() {}\nfn a3() {}\n").unwrap();
    std::fs::write(&b, "fn b() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    // Edit a.rs at row 2.
    app.editor.cursor_row = 2;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    // Navigate away.
    app.editor.open_pinned(&b).unwrap();
    assert_eq!(app.editor.path.as_deref(), Some(b.as_path()));
    // Cmd+K Cmd+Q returns to the edit point in a.rs.
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('q'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(a.as_path()),
        "Cmd+K Cmd+Q must return to the last edited file"
    );
    assert_eq!(app.editor.cursor_row, 2, "and to the edited row");
}

#[test]
fn editor_cmd_shift_o_opens_go_to_symbol() {
    let mut app = editor_app_with_lines(&["fn foo() {}", "fn bar() {}"]);
    app.editor.path = Some(std::path::PathBuf::from("lib.rs"));
    app.focus_pane(Pane::Editor);
    app.handle_key(key(
        KeyCode::Char('O'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        app.go_to_symbol.is_some(),
        "Cmd+Shift+O must open the Go to Symbol picker"
    );
}

fn outline_sym(
    name: &str,
    kind: crate::lsp::manager::OutlineKind,
    start: u32,
    end: u32,
) -> crate::lsp::manager::OutlineSymbol {
    crate::lsp::manager::OutlineSymbol {
        name: name.to_string(),
        detail: None,
        kind,
        depth: 0,
        line: start,
        character: 0,
        range_start_line: start,
        range_end_line: end,
        range_start_character: 0,
        range_end_character: u32::MAX,
    }
}

#[test]
fn outline_symbols_apply_only_for_the_active_file() {
    use crate::lsp::manager::OutlineKind;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn a() {}\n");
    let active = app.editor.path.clone().unwrap();
    let syms = vec![outline_sym("a", OutlineKind::Function, 0, 0)];

    // A reply for a file the user already navigated away from is dropped.
    let other = tmp.path().join("b.rs");
    assert!(
        !app.apply_outline_symbols(other, 0, syms.clone()),
        "a reply whose path is not the active editor file must be ignored"
    );
    assert!(app.outline.path.is_none(), "stale reply must not populate");

    // A reply for the active file populates the outline.
    assert!(app.apply_outline_symbols(active.clone(), 0, syms));
    assert_eq!(app.outline.path.as_ref(), Some(&active));
    assert!(!app.outline.is_empty());
}

#[test]
fn an_outline_reply_is_matched_by_request_not_by_the_larger_seq() {
    use crate::lsp::manager::OutlineKind;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn a() {}\n");
    let active = app.editor.path.clone().unwrap();

    // `seq` is a per-Editor counter, and a file landing in a FRESH editor group
    // (a Cmd+\ split, or close-and-reopen) restarts it at zero. So the request
    // actually outstanding can carry a far smaller number than a reply still in
    // flight from the group the user left, and "the highest seq is the newest"
    // is simply false. Anything that pre-selects replies by size before this
    // guard sees them keeps the stale one; the guard then rejects it on the
    // exact-match rule, and sync_outline never re-requests because its
    // (path, seq) key has not moved — the outline stays tree-sitter-only until
    // the next edit.
    app.outline_synced = Some((active.clone(), Some(1)));

    let stale = vec![outline_sym(
        "from_the_old_group",
        OutlineKind::Function,
        0,
        0,
    )];
    assert!(
        !app.apply_outline_symbols(active.clone(), 40, stale),
        "a bigger seq is a reply from a previous editor group, not a newer one"
    );
    assert!(app.outline.is_empty(), "the stale reply must not populate");

    let fresh = vec![outline_sym("outstanding", OutlineKind::Function, 0, 0)];
    assert!(
        app.apply_outline_symbols(active.clone(), 1, fresh),
        "the reply matching the outstanding request applies however small its seq"
    );
    assert!(!app.outline.is_empty());
}

#[test]
fn sync_outline_settles_empty_for_a_file_without_a_language_server() {
    // A plain text file has no language server, so no documentSymbol reply will
    // ever arrive. sync_outline must settle the panel to a loaded, empty state
    // (rendering "No symbols") rather than leaving it spinning on "Loading".
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.txt", "hello\nworld\n");
    let active = app.editor.path.clone().unwrap();
    app.sync_outline();
    assert_eq!(app.outline.path.as_ref(), Some(&active));
    assert!(app.outline.is_empty(), "no symbols for a non-served file");
    assert!(
        app.outline.loaded(),
        "outline must be marked loaded so it shows 'No symbols', not 'Loading'"
    );
}

#[test]
fn sync_outline_populates_from_tree_sitter_without_a_language_server() {
    // Even with no LSP attached, opening a source file must fill the OUTLINE
    // instantly from the buffer's own syntax tree (tree-sitter-first), so the
    // panel never spins on "Loading" waiting for a cold server.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(
        tmp.path(),
        "demo.rs",
        "struct S;\n\nfn alpha() {}\n\nfn beta() {}\n",
    );
    app.sync_outline();
    assert!(
        app.outline.loaded(),
        "syntactic outline settles immediately"
    );
    assert!(
        !app.outline.is_empty(),
        "tree-sitter must surface the struct and two functions"
    );
}

/// Tick `sync_outline` until the outline holds a symbol named `name`.
fn sync_outline_until_symbol(app: &mut App, name: &str) {
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(60),
        "the off-thread outline parse",
        || {
            app.sync_outline();
            app.outline.symbols().iter().any(|s| s.name == name)
        },
    );
}

#[test]
fn typing_in_a_large_file_does_not_reparse_its_outline_on_the_ui_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "big.py", &python_module(60_000));
    note_edit_for_outline(&mut app);
    sync_outline_until_symbol(&mut app, "f59999");
    app.editor.cursor_row = app.editor.lines.len() - 1;
    app.editor.cursor_col = 0;
    for c in "def added(): pass".chars() {
        app.editor.insert_char(c);
    }
    note_edit_for_outline(&mut app);
    let started = std::time::Instant::now();
    app.sync_outline();
    let took = started.elapsed();
    assert!(
        took < std::time::Duration::from_millis(250),
        "a keystroke's outline sync took {took:?}"
    );
    assert!(
        app.outline.symbols().iter().any(|s| s.name == "f59999"),
        "the previous outline stays until the new one lands"
    );
    sync_outline_until_symbol(&mut app, "added");
}

#[test]
fn a_burst_of_typing_in_a_large_file_ends_on_the_outline_of_its_last_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "big.py", &python_module(20_000));
    note_edit_for_outline(&mut app);
    sync_outline_until_symbol(&mut app, "f19999");
    app.editor.cursor_row = app.editor.lines.len() - 1;
    for c in "def burst(): pass".chars() {
        app.editor.insert_char(c);
        note_edit_for_outline(&mut app);
        app.sync_outline();
    }
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(60),
        "the parse of the last keystroke",
        || {
            app.sync_outline();
            app.outline_parse.is_none() && app.outline_parsed_for == app.outline_synced
        },
    );
    assert!(app.outline.symbols().iter().any(|s| s.name == "burst"));
    app.sync_outline();
    assert!(
        app.outline_parse.is_none(),
        "with no new edit, no new parse"
    );
}

#[test]
fn a_small_file_outline_still_lands_in_the_same_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "small.py", &python_module(50));
    note_edit_for_outline(&mut app);
    app.sync_outline();
    assert_eq!(app.outline.symbols().len(), 50);
    assert!(app.outline_parse.is_none(), "no worker for a small file");
}

#[test]
fn clicking_the_outline_header_toggles_collapse() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn a() {}\n");
    // Hermetic: ignore the user's real Outline-view pref (see scrollbar test).
    app.explorer_views = Default::default();
    let active = app.editor.path.clone().unwrap();
    app.apply_outline_symbols(
        active,
        0,
        vec![outline_sym(
            "a",
            crate::lsp::manager::OutlineKind::Function,
            0,
            0,
        )],
    );
    assert!(app.outline.collapsed, "starts collapsed");

    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let hdr = app.outline.last_area;
    assert!(hdr.height > 0, "outline must have rendered");
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        hdr.x + 2,
        hdr.y,
    ));
    assert!(
        !app.outline.collapsed,
        "clicking the header expands the section"
    );
}

#[test]
fn clicking_an_outline_row_jumps_the_editor_to_that_symbol() {
    use crate::lsp::manager::OutlineKind;
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");
    // Hermetic: ignore the user's real Outline-view pref (see scrollbar test).
    app.explorer_views = Default::default();
    let active = app.editor.path.clone().unwrap();
    app.outline.toggle_collapse(); // expand so rows render
    app.apply_outline_symbols(
        active,
        0,
        vec![
            outline_sym("a", OutlineKind::Function, 0, 0),
            outline_sym("b", OutlineKind::Function, 1, 1),
            outline_sym("c", OutlineKind::Function, 2, 2),
        ],
    );
    app.editor.cursor_row = 0;

    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let area = app.outline.last_area;
    // Header occupies the first inner row; symbol rows follow. Click the
    // second symbol ("b", line 1), which sits two rows below the header.
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.x + 2,
        area.y + 2,
    ));
    assert_eq!(
        app.editor.cursor_row, 1,
        "clicking the second outline row must move the editor caret to line 1"
    );
}

/// A workspace with `files` (empty) and `open` open in the editor.
fn c_workspace(files: &[&str], open: &str) -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    for f in files {
        let p = tmp.path().join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!("// {f}\n")).unwrap();
    }
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&tmp.path().join(open)).unwrap();
    app.focus_pane(Pane::Editor);
    (tmp, app)
}

/// #1360: Alt+O goes from `src/geom.cpp` to `include/geom.hpp` and back.
#[test]
fn alt_o_switches_between_a_source_and_its_header() {
    let (tmp, mut app) = c_workspace(&["src/geom.cpp", "include/geom.hpp"], "src/geom.cpp");
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("include/geom.hpp").as_path()),
        "{}",
        app.status
    );
    assert_eq!(app.editor.lines[0], "// include/geom.hpp");
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("src/geom.cpp").as_path())
    );
}

/// #1360: the palette command says when there is no counterpart.
#[test]
fn switch_header_source_says_when_there_is_no_counterpart() {
    let (tmp, mut app) = c_workspace(&["main.cpp"], "main.cpp");
    app.run_command(crate::widgets::command_palette::Command::SwitchHeaderSource);
    assert_eq!(app.status, "No header/source counterpart for main.cpp");
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("main.cpp").as_path())
    );
}

/// #1360: switching to the counterpart keeps unsaved edits in the source,
/// which is still there, dirty, when Alt+O comes back to it.
#[test]
fn alt_o_keeps_unsaved_edits_in_the_current_buffer() {
    let (tmp, mut app) = c_workspace(&["src/geom.cpp", "include/geom.hpp"], "src/geom.cpp");
    app.editor.lines[0] = String::from("int unsaved;");
    app.editor.dirty = true;
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("include/geom.hpp").as_path()),
        "{}",
        app.status
    );
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("src/geom.cpp").as_path())
    );
    assert_eq!(app.editor.lines[0], "int unsaved;");
    assert!(app.editor.dirty);
}

/// #1360: a file in a secondary root of a multi-root workspace finds its
/// counterpart across that root's `include/` and `src/` split.
#[test]
fn alt_o_resolves_against_the_root_that_owns_the_file() {
    let primary = tempfile::tempdir().unwrap();
    let secondary = tempfile::tempdir().unwrap();
    for f in ["src/foo.cpp", "include/foo.hpp"] {
        let p = secondary.path().join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!("// {f}\n")).unwrap();
    }
    let mut app = App::new(primary.path().to_path_buf()).unwrap();
    app.roots.add(secondary.path().to_path_buf());
    app.file_finder_index = Some(std::sync::Arc::new(Vec::new()));
    app.file_finder_index_rx = None;
    app.editor
        .open(&secondary.path().join("src/foo.cpp"))
        .unwrap();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(secondary.path().join("include/foo.hpp").as_path()),
        "{}",
        app.status
    );
}

/// #1360: Alt+O before the background index lands still finds a
/// counterpart that only the anywhere-in-the-workspace search reaches.
#[test]
fn alt_o_builds_the_index_when_it_has_not_landed() {
    let (tmp, mut app) = c_workspace(&["lib/geom.cpp", "api/geom.hpp"], "lib/geom.cpp");
    app.file_finder_index = None;
    app.file_finder_index_rx = None;
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("api/geom.hpp").as_path()),
        "{}",
        app.status
    );
}

/// #1360, negative: in a file that is not C-family, Alt+O switches
/// nothing, and the palette command says what it works on.
#[test]
fn switch_header_source_leaves_other_files_alone() {
    let (tmp, mut app) = c_workspace(&["geom.py", "geom.h"], "geom.py");
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("geom.py").as_path())
    );
    app.run_command(crate::widgets::command_palette::Command::SwitchHeaderSource);
    assert_eq!(
        app.status,
        "Switch Header/Source works on C, C++ and Objective-C files"
    );
}

#[test]
fn breadcrumb_symbol_chain_returns_enclosing_symbols_outermost_first() {
    use crate::lsp::manager::OutlineKind;
    let symbols = vec![
        sym("Widget", OutlineKind::Class, 0, 0, 20),
        sym("build", OutlineKind::Method, 1, 2, 8),
        sym("paint", OutlineKind::Method, 1, 10, 18),
    ];
    // Caret on line 4 sits inside Widget::build.
    let chain = breadcrumb_symbol_chain(&symbols, 4, 0);
    assert_eq!(chain, vec![0, 1], "outermost (Widget) then inner (build)");
    // Caret on line 15 sits inside Widget::paint.
    assert_eq!(breadcrumb_symbol_chain(&symbols, 15, 0), vec![0, 2]);
    // Caret on line 0 (the class header) is inside the class only.
    assert_eq!(breadcrumb_symbol_chain(&symbols, 0, 0), vec![0]);
    // Caret past every symbol has no scope chain.
    assert!(breadcrumb_symbol_chain(&symbols, 30, 0).is_empty());
}

/// An outline symbol spanning `(line, col)` to `(line, col)`.
fn sym_span(
    name: &str,
    depth: u16,
    start: (u32, u32),
    end: (u32, u32),
) -> crate::lsp::manager::OutlineSymbol {
    crate::lsp::manager::OutlineSymbol {
        range_start_character: start.1,
        range_end_character: end.1,
        character: start.1,
        ..sym(
            name,
            crate::lsp::manager::OutlineKind::Field,
            depth,
            start.0,
            end.0,
        )
    }
}

/// #1221: symbols that share a line are siblings, not a nesting chain. On
/// `export interface Item { id: number; cents: number }` the caret at
/// `export` is in `Item` only, and on `cents` in `Item` then `cents`.
#[test]
fn breadcrumb_chain_tells_siblings_on_one_line_apart() {
    // export interface Item { id: number; cents: number }
    // 0      7         17   22 24         36             51
    let symbols = vec![
        sym_span("Item", 0, (0, 0), (0, 51)),
        sym_span("id", 1, (0, 24), (0, 35)),
        sym_span("cents", 1, (0, 36), (0, 50)),
    ];
    assert_eq!(breadcrumb_symbol_chain(&symbols, 0, 0), vec![0]);
    assert_eq!(breadcrumb_symbol_chain(&symbols, 0, 40), vec![0, 2]);
    assert_eq!(breadcrumb_symbol_chain(&symbols, 0, 26), vec![0, 1]);
    // A minified bundle: three functions on one line, the caret in the
    // second, is in the second only.
    let symbols = vec![
        sym_span("f0", 0, (0, 0), (0, 30)),
        sym_span("f1", 0, (0, 31), (0, 61)),
        sym_span("f2", 0, (0, 62), (0, 92)),
    ];
    assert_eq!(breadcrumb_symbol_chain(&symbols, 0, 40), vec![1]);
    assert!(breadcrumb_symbol_chain(&symbols, 1, 0).is_empty());
}

/// Negative (#1221): multi-line scopes still chain by line. Inside a
/// method's body, at any column, the chain is the class then the method;
/// on the method's first line before it starts, only the class.
#[test]
fn breadcrumb_chain_still_nests_multi_line_scopes() {
    let symbols = vec![
        sym_span("Cart", 0, (11, 0), (20, 1)),
        sym_span("total", 1, (16, 2), (18, 3)),
    ];
    assert_eq!(breadcrumb_symbol_chain(&symbols, 17, 0), vec![0, 1]);
    assert_eq!(breadcrumb_symbol_chain(&symbols, 17, 40), vec![0, 1]);
    assert_eq!(breadcrumb_symbol_chain(&symbols, 16, 0), vec![0]);
    assert_eq!(breadcrumb_symbol_chain(&symbols, 16, 2), vec![0, 1]);
    assert!(breadcrumb_symbol_chain(&symbols, 21, 0).is_empty());
}

#[test]
fn build_breadcrumbs_lists_path_segments_then_the_symbol_chain() {
    use crate::lsp::manager::OutlineKind;
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("src");
    std::fs::create_dir_all(&sub).unwrap();
    let f = sub.join("m.rs");
    std::fs::write(&f, "fn a() {}\nfn b() {\n    x\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.outline
        .set_symbols(f.clone(), vec![sym("b", OutlineKind::Function, 0, 1, 3)]);
    app.editor.cursor_row = 2; // inside fn b
    let crumbs = app.build_breadcrumbs();
    let labels: Vec<&str> = crumbs.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["src", "m.rs", "b"]);
    assert_eq!(crumbs[0].target, None, "path crumbs are informational");
    assert_eq!(
        crumbs[2].target,
        Some((1, 0)),
        "symbol crumb jumps to itself"
    );
}

/// #1608: the file crumb lists its folder with the file selected; a folder
/// row drills in, and an empty folder says so instead of an empty picker.
#[test]
fn the_file_crumb_lists_its_folder_and_folders_drill_in() {
    let (mut app, _tmp) = app_on_src_f2("// f2.rs\n");
    click_crumb(&mut app, 1);
    let menu = app.context_menu.as_ref().expect("the f2.rs picker");
    assert_eq!(menu_labels(&menu.items)[menu.selected], "f2.rs");
    let menu = app.context_menu.as_mut().unwrap();
    menu.selected = 0;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.context_menu.is_none(),
        "an empty folder opens no picker"
    );
    assert!(app.status.ends_with("sub is empty"), "{}", app.status);
    assert!(app.editor.path.as_deref().unwrap().ends_with("src/f2.rs"));
}

/// #1608: a symbol crumb lists the symbols beside it, itself selected, and
/// not those of another parent.
#[test]
fn a_symbol_crumb_lists_its_siblings() {
    use crate::lsp::manager::OutlineKind;
    let (mut app, tmp) = app_on_src_f2(
        "struct S;\nimpl S {\n    fn a() {}\n    fn b() {\n    }\n    fn c() {}\n}\nfn free() {}\n",
    );
    let f = tmp.path().join("src/f2.rs");
    app.outline.set_symbols(
        f.clone(),
        vec![
            sym("S", OutlineKind::Struct, 0, 1, 6),
            sym("a", OutlineKind::Method, 1, 2, 2),
            sym("b", OutlineKind::Method, 1, 3, 4),
            sym("c", OutlineKind::Method, 1, 5, 5),
            sym("free", OutlineKind::Function, 0, 7, 7),
        ],
    );
    app.editor.cursor_row = 3;
    draw(&mut app, 100, 30);
    let labels: Vec<String> = app
        .editor
        .breadcrumbs
        .iter()
        .map(|c| c.label.clone())
        .collect();
    assert_eq!(labels, vec!["src", "f2.rs", "S", "b"]);
    click_crumb(&mut app, 3);
    let menu = app.context_menu.as_ref().expect("the b picker");
    assert_eq!(menu_labels(&menu.items), vec!["a", "b", "c"]);
    assert_eq!(menu.selected, 1);
    // A click outside an open menu only closes it.
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    click_crumb(&mut app, 2);
    let menu = app.context_menu.as_ref().expect("the S picker");
    assert_eq!(menu_labels(&menu.items), vec!["S", "free"]);
    assert_eq!(menu.selected, 0);
}

/// #1608: Ctrl+Shift+. opens the last crumb's picker; Left and Right walk
/// the bar, stopping at its ends. Ctrl+. (no Shift) is still Quick Fix.
#[test]
fn focus_and_select_breadcrumbs_walks_the_bar_from_the_keyboard() {
    use crate::widgets::command_palette::Command;
    let (mut app, _tmp) = app_on_src_f2("// f2.rs\n");
    app.handle_key(key(KeyCode::Char('.'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(
        app.context_menu.as_ref().is_none_or(|m| m.crumb.is_none()),
        "Ctrl+. is not the breadcrumbs chord"
    );
    app.context_menu = None;
    app.handle_key(key(
        KeyCode::Char('.'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    let crumb = |app: &App| app.context_menu.as_ref().and_then(|m| m.crumb);
    assert_eq!(crumb(&app), Some(1), "the last crumb, f2.rs");
    app.handle_key(key(KeyCode::Left, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(crumb(&app), Some(0));
    app.handle_key(key(KeyCode::Left, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(crumb(&app), Some(0), "Left stops at the first crumb");
    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(crumb(&app), Some(1), "Right stops at the last crumb");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.context_menu.is_none());
    assert_eq!(
        Command::from_id("breadcrumbs_focus_and_select"),
        Some(Command::FocusBreadcrumbs)
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut empty = App::new(tmp.path().to_path_buf()).unwrap();
    empty.run_command(Command::FocusBreadcrumbs);
    assert!(empty.context_menu.is_none());
    assert_eq!(empty.status, "No breadcrumbs to focus");
}

/// #1608: the breadcrumb chord works from any pane, not only the editor.
#[test]
fn focus_and_select_breadcrumbs_works_from_the_explorer() {
    let (mut app, _tmp) = app_on_src_f2("// f2.rs\n");
    app.focus_pane(Pane::Tree);
    app.handle_key(key(
        KeyCode::Char('.'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.context_menu.as_ref().and_then(|m| m.crumb), Some(1));
    assert!(app.focus == Pane::Editor);
}

/// #1608: on a bar too narrow for the whole chain the chord focuses the
/// last crumb that fit instead of reporting no breadcrumbs.
#[test]
fn focus_and_select_breadcrumbs_takes_the_last_crumb_that_fit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut dir = tmp.path().to_path_buf();
    for name in [
        "alpha_folder",
        "beta_folder",
        "gamma_folder",
        "delta_folder",
    ] {
        dir.push(name);
    }
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("the_file_at_the_end.rs");
    std::fs::write(&file, "// end\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.show_tree = false;
    app.editor.open(&file).unwrap();
    app.focus_pane(Pane::Editor);
    draw(&mut app, 50, 20);
    let n = app.editor.breadcrumbs.len();
    assert!(
        app.editor.breadcrumb_anchor(n - 1).is_none(),
        "last crumb clipped"
    );
    let last = (0..n)
        .rev()
        .find(|&i| app.editor.breadcrumb_anchor(i).is_some())
        .expect("some crumb on screen");
    app.handle_key(key(
        KeyCode::Char('.'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.context_menu.as_ref().and_then(|m| m.crumb), Some(last));
}

/// #1608: a folder picker taller than the terminal scrolls with its
/// highlight, and clicks hit the row drawn under them.
#[test]
fn a_long_crumb_picker_keeps_its_highlight_on_screen() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for k in 0..40 {
        std::fs::write(src.join(format!("f{k:02}.rs")), "\n").unwrap();
    }
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&src.join("f35.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    draw(&mut app, 100, 20);
    click_crumb(&mut app, 1);
    // The row the highlight is drawn on, with its label there.
    let shown = |app: &mut App| {
        let screen = screen_text_863(app, 100, 20);
        let r = app.menu_rect().unwrap();
        let sel = app.context_menu.as_ref().unwrap().selected;
        let y = (r.y + 1..r.y + r.height - 1)
            .find(|&y| app.menu_item_at(r.x + 2, y) == Some(sel))
            .expect("highlight on screen");
        let line = screen.lines().nth(y as usize).unwrap().to_string();
        assert!(line.contains(&format!("f{sel:02}.rs")), "{line}");
        y
    };
    assert_eq!(app.context_menu.as_ref().unwrap().selected, 35);
    shown(&mut app);
    for _ in 0..10 {
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(app.context_menu.as_ref().unwrap().selected, 39);
    let bottom = shown(&mut app);
    app.handle_key(key(KeyCode::Up, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(shown(&mut app), bottom - 1, "Up moves within the window");
    for _ in 0..40 {
        app.handle_key(key(KeyCode::Up, KeyModifiers::NONE))
            .unwrap();
    }
    let top = shown(&mut app);
    assert_eq!(top, app.menu_rect().unwrap().y + 1);
}

#[test]
fn sibling_symbols_stay_under_one_parent() {
    use crate::lsp::manager::OutlineKind::{Function, Method, Struct};
    let symbols = vec![
        sym("S", Struct, 0, 0, 9),
        sym("a", Method, 1, 1, 1),
        sym("b", Method, 1, 2, 2),
        sym("T", Struct, 0, 10, 19),
        sym("c", Method, 1, 11, 11),
        sym("free", Function, 0, 20, 20),
    ];
    assert_eq!(sibling_symbols(&symbols, 2), vec![1, 2]);
    assert_eq!(sibling_symbols(&symbols, 4), vec![4]);
    assert_eq!(sibling_symbols(&symbols, 0), vec![0, 3, 5]);
    assert!(sibling_symbols(&symbols, 9).is_empty());
}

#[test]
fn stale_document_symbol_reply_must_not_replace_the_fresher_outline() {
    // While typing on a slow language server, `documentSymbol` replies land
    // several edits behind the buffer. Applying one replaces the fresh
    // tree-sitter outline with ranges that no longer contain the caret, the
    // breadcrumb's symbol crumb drops off, and the next keystroke restores it:
    // the "hello_there blinks in the breadcrumb" bug. A reply is only valid
    // when it answers the NEWEST request (same path, same edit seq).
    use crate::lsp::manager::{OutlineKind, OutlineSymbol};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("demo.rs");
    std::fs::write(
        &file,
        "fn hello_there() {\n    let a = 1;\n    let b = 2;\n}\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_at_launch(&file);
    // The LSP manager tracks this file at edit seq 7; the newest outline
    // request was issued for that seq.
    app.lsp_last_seen.insert(file.clone(), 7);
    app.sync_outline();
    assert!(
        app.outline
            .symbols()
            .iter()
            .any(|s| s.name == "hello_there"),
        "tree-sitter must have painted the fresh outline"
    );

    let stale_symbol = OutlineSymbol {
        name: String::from("hello_there"),
        detail: None,
        kind: OutlineKind::Function,
        depth: 0,
        line: 0,
        character: 3,
        range_start_line: 0,
        range_end_line: 0, // ranges from an older buffer: caret now outside
        range_start_character: 0,
        range_end_character: u32::MAX,
    };
    let applied = app.apply_outline_symbols(file.clone(), 3, vec![stale_symbol.clone()]);
    assert!(
        !applied,
        "a documentSymbol reply for an older edit seq must be dropped, not applied"
    );
    assert!(
        app.outline
            .symbols()
            .iter()
            .any(|s| s.name == "hello_there" && s.range_end_line >= 3),
        "the fresh tree-sitter ranges must survive the stale reply"
    );

    // The reply answering the newest request (seq 7) refines the outline.
    let applied = app.apply_outline_symbols(file.clone(), 7, vec![stale_symbol]);
    assert!(applied, "the reply matching the newest request must apply");
}

#[test]
fn the_navigation_chords_move_the_cursor_between_marks() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.rs", "1\n2\n3\n4\n5\n6\n");
    for row in [1, 4] {
        app.editor.cursor_row = row;
        app.editor.toggle_bookmark();
    }
    app.editor.cursor_row = 0;
    app.handle_editor_key(key(
        KeyCode::Char('.'),
        KeyModifiers::SUPER | KeyModifiers::ALT,
    ));
    assert_eq!(app.editor.cursor_row, 1);
    app.handle_editor_key(key(
        KeyCode::Char('.'),
        KeyModifiers::SUPER | KeyModifiers::ALT,
    ));
    assert_eq!(app.editor.cursor_row, 4);
    app.handle_editor_key(key(
        KeyCode::Char(','),
        KeyModifiers::SUPER | KeyModifiers::ALT,
    ));
    assert_eq!(app.editor.cursor_row, 1);
}

#[test]
fn bookmark_status_counts_marks_and_names_the_line_separately() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.rs", "1\n2\n3\n4\n5\n6\n");
    for row in [1, 4] {
        app.editor.cursor_row = row;
        app.editor.toggle_bookmark();
    }
    app.editor.cursor_row = 0;
    let next = key(KeyCode::Char('.'), KeyModifiers::SUPER | KeyModifiers::ALT);
    app.handle_editor_key(next);
    assert_eq!(app.status, "Bookmark 1 of 2 (line 2)");
    app.handle_editor_key(next);
    assert_eq!(app.status, "Bookmark 2 of 2 (line 5)");
}

#[test]
fn navigating_an_unmarked_file_explains_itself_rather_than_doing_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.rs", "1\n2\n3\n");
    app.editor.cursor_row = 1;
    app.handle_editor_key(key(
        KeyCode::Char('.'),
        KeyModifiers::SUPER | KeyModifiers::ALT,
    ));
    assert_eq!(app.editor.cursor_row, 1);
    assert!(app.status.contains("No bookmarks"), "was {:?}", app.status);
}

// --- Workspace symbol search (# in Quick Open) ------------------------------

fn ws_item(
    name: &str,
    path: &std::path::Path,
    line: u32,
) -> crate::lsp::manager::WorkspaceSymbolItem {
    crate::lsp::manager::WorkspaceSymbolItem {
        name: name.to_string(),
        kind: crate::lsp::manager::OutlineKind::Function,
        path: path.to_path_buf(),
        line,
        character: 0,
        container: None,
    }
}

#[test]
fn stale_workspace_symbol_replies_are_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn main() {}");
    app.open_workspace_symbols("ma");
    app.ws_symbols_request_id = Some(7);
    let f = tmp.path().join("a.rs");
    assert!(
        !app.apply_workspace_symbols(3, vec![ws_item("old", &f, 0)], false),
        "a reply for a superseded request must be dropped"
    );
    assert!(app.workspace_symbols.as_ref().unwrap().results.is_empty());
    assert!(
        app.apply_workspace_symbols(7, vec![ws_item("main", &f, 0)], false),
        "the newest request's reply applies"
    );
    assert_eq!(app.workspace_symbols.as_ref().unwrap().results.len(), 1);
}

#[test]
fn enter_opens_the_selected_symbols_file_at_its_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn main() {}");
    let other = tmp.path().join("b.rs");
    std::fs::write(&other, "line0\nline1\nfn target() {}\n").unwrap();
    app.open_workspace_symbols("target");
    app.ws_symbols_request_id = Some(1);
    assert!(app.apply_workspace_symbols(1, vec![ws_item("target", &other, 2)], false));
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.workspace_symbols.is_none(), "Enter closes the picker");
    assert_eq!(
        app.editor.path.as_deref(),
        Some(other.as_path()),
        "Enter opens the symbol's file"
    );
    assert_eq!(
        app.editor.cursor_row, 2,
        "the caret lands on the symbol's line"
    );
}

/// Ctrl+Shift+H on a tiny host window: the popup clamps inverted
/// (min > max) below 40 columns or 10 rows and `Ord::clamp` panics,
/// taking the raw-mode TUI down.
#[test]
fn the_history_popup_survives_a_tiny_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.open_command_history();
    let backend = ratatui::backend::TestBackend::new(30, 8);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
}

#[test]
fn ctrl_click_follows_a_document_link_before_go_to_definition() {
    use crate::lsp::manager::DocumentLinkItem;
    // #254 item 8: a file:// link opens the target in the editor; the
    // definition request is never fired for a linked range.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "// link here\n").unwrap();
    std::fs::write(tmp.path().join("b.rs"), "fn target() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("a.rs")).unwrap();
    app.focus_pane(Pane::Editor);
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
    app.open_document_link(target.as_ref());
    assert_eq!(
        app.editor.path.as_deref().and_then(|p| p.file_name()),
        Some(std::ffi::OsStr::new("b.rs")),
        "the file link opened in the editor"
    );
    // And the editor-side lookup that the click dispatch consults.
    app.editor.open_pinned(&tmp.path().join("a.rs")).unwrap();
    app.editor.apply_document_links(
        tmp.path().join("a.rs"),
        vec![DocumentLinkItem {
            line: 0,
            character: 3,
            end_line: 0,
            end_character: 12,
            target: String::from("https://example.com"),
        }],
    );
    assert_eq!(
        app.editor.document_link_at(0, 5),
        Some("https://example.com")
    );
}

#[test]
fn a_document_link_refuses_a_non_web_scheme_and_records_nav_history() {
    use crate::lsp::manager::DocumentLinkItem;
    // The target comes from a language server and the text it decorates says
    // nothing about where it points, so anything but file://, http and https
    // is refused rather than handed to the system opener — the same rule
    // `open_detected_url` applies to terminal OSC 8 links.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("a.rs"),
        "// link here
",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("b.rs"),
        "fn target() {}
",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("a.rs")).unwrap();
    app.focus_pane(Pane::Editor);

    for hostile in [
        "vscode://file/etc/passwd",
        "VSCode://file/etc/passwd",
        "cursor://file/etc/passwd",
        "javascript:alert(1)",
        "mailto:a@b.c",
        "ssh://host/x",
        "  HTTP-ish-but-not://x",
    ] {
        app.open_document_link(hostile);
        assert!(
            app.status.starts_with("Refused to open non-web link:"),
            "{hostile} must be refused, got {:?}",
            app.status
        );
    }

    // A control character never belongs in a URI, and the relay's own
    // validator rejects one; refuse before anything downstream sees it.
    for malformed in [
        "https://example.com\nopen\tevil",
        "https://exa\u{0}mple.com",
        // The check runs before the branch, so a file:// target carrying one
        // is refused too rather than reaching Url::parse.
        "file:///etc/hosts\nopen\tevil",
    ] {
        app.open_document_link(malformed);
        assert!(
            app.status.starts_with("Refused to open malformed link:"),
            "{malformed:?} must be refused, got {:?}",
            app.status
        );
    }

    // A file:// link still opens, and records where the user was so Back
    // returns to it.
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 4;
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
    app.open_document_link(target.as_ref());
    assert_eq!(
        app.editor.path.as_deref().and_then(|p| p.file_name()),
        Some(std::ffi::OsStr::new("b.rs")),
        "the file link opened"
    );
    let back = app
        .nav
        .back(None)
        .expect("the jump recorded a Back location, like go-to-definition");
    assert_eq!(
        back.path.file_name(),
        Some(std::ffi::OsStr::new("a.rs")),
        "Back returns to the file the link was clicked in"
    );
    assert_eq!((back.row, back.col), (0, 4), "and to the click's caret");

    // URI schemes are case-insensitive (RFC 3986) and `file_ref` already
    // matches them that way; a server spelling it `FILE://` must open, not
    // fall through to the web guard and get refused.
    app.editor.open_pinned(&tmp.path().join("a.rs")).unwrap();
    let upper = target.as_ref().replacen("file://", "FILE://", 1);
    app.open_document_link(&upper);
    assert_eq!(
        app.editor.path.as_deref().and_then(|p| p.file_name()),
        Some(std::ffi::OsStr::new("b.rs")),
        "an upper-case file scheme still opens in the editor"
    );

    // Surrounding whitespace is trimmed rather than smuggled downstream.
    app.editor.open_pinned(&tmp.path().join("a.rs")).unwrap();
    app.open_document_link(&format!("  {}  ", target.as_ref()));
    assert_eq!(
        app.editor.path.as_deref().and_then(|p| p.file_name()),
        Some(std::ffi::OsStr::new("b.rs")),
        "a padded target is normalised, not refused"
    );
}

/// An LSP edit applied to the file's tab while its symbol tab is active
/// survives the next keystroke in the symbol tab: both tabs changed, and
/// the keystroke's tab used to be copied over the rename.
#[test]
fn a_rename_landing_before_a_keystroke_in_a_symbol_tab_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    app.focus = Pane::Editor;
    let edits = vec![(
        file.clone(),
        vec![crate::widgets::editor::TextSpanEdit {
            start: (0, 3),
            end: (0, 4),
            new_text: String::from("renamed"),
            utf16: false,
        }],
    )];
    app.apply_rename_edits(&edits).unwrap();
    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::NONE,
    ))
    .unwrap();
    app.sync_symbol_views();
    let file_tab = app
        .editor
        .editors
        .iter()
        .find(|t| t.symbol_view.is_none() && t.path.as_deref() == Some(file.as_path()))
        .expect("the file's own tab");
    assert!(
        file_tab.lines[0].contains("renamed"),
        "{:?}",
        file_tab.lines
    );
    assert!(
        file_tab.lines.iter().any(|l| l.contains('x')),
        "{:?}",
        file_tab.lines
    );
}

/// #369: two symbols that start on one line get a tab each, opening either
/// again focuses its own tab, and each follows its own symbol: renaming the
/// inner one leaves the outer tab's title alone.
#[test]
fn symbols_starting_on_one_line_get_a_tab_each() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("one.rs");
    std::fs::write(&file, "impl S { fn m() {} }\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.open_symbol_tab_for("S".into(), 0, 0);
    app.editor.select(0);
    app.open_symbol_tab_for("m".into(), 0, 0);
    let names = |app: &App| -> Vec<String> {
        let mut names: Vec<String> = app
            .editor
            .editors
            .iter()
            .filter_map(|e| e.symbol_view.as_ref().map(|v| v.name.clone()))
            .collect();
        names.sort();
        names
    };
    assert_eq!(names(&app), vec!["S", "m"]);
    app.editor.select(0);
    app.open_symbol_tab_for("S".into(), 0, 0);
    assert_eq!(names(&app).len(), 2, "reopening S focused its tab");
    assert_eq!(
        app.editor.symbol_view.as_ref().map(|v| v.name.as_str()),
        Some("S")
    );
    // Rename m to n from the file's tab.
    app.editor.select(0);
    let col = app.editor.lines[0].find("m()").unwrap();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = col;
    app.editor.delete_forward();
    app.editor.insert_char('n');
    app.sync_symbol_views();
    assert_eq!(names(&app), vec!["S", "n"]);
}

/// #369: pickers spell some symbols differently (the LSP outline's
/// `impl Foo` is tree-sitter's `Foo`); asking for the same symbol under
/// either name focuses the one tab.
#[test]
fn one_symbol_under_two_spellings_gets_one_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("foo.rs");
    std::fs::write(&file, "impl Foo {\n    fn a() {}\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.open_symbol_tab_for("Foo".into(), 0, 2);
    app.editor.select(0);
    app.open_symbol_tab_for("impl Foo".into(), 0, 2);
    let tabs = app
        .editor
        .editors
        .iter()
        .filter(|e| e.symbol_view.is_some())
        .count();
    assert_eq!(tabs, 1);
}

/// #369: a symbol tab's minimap draws only its symbol, and a click on it
/// lands on the symbol's lines, not the file's.
#[test]
fn a_symbol_tab_minimap_draws_and_maps_only_its_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("mm.rs");
    std::fs::write(&file, "fn a() {\n    1\n}\n  fn b() {\n    2\n  }").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    let (bg, fg) = ((0, 0, 0), (255, 255, 255));
    // Row 0, column 0: `f` of `fn a` in the file's tab.
    let lit = |buf: &[u8]| buf[0] != 0;
    assert!(lit(&app.editor.minimap_rgba(4, 3, 3, bg, fg)));
    app.editor.cursor_row = 4;
    app.run_command(crate::widgets::command_palette::Command::OpenAsSymbolTab);
    assert_eq!(app.editor.minimap_span(), (3, 6));
    // `  fn b`: column 0 is indentation.
    assert!(!lit(&app.editor.minimap_rgba(4, 3, 3, bg, fg)));
    app.minimap_img_rect = Rect::new(0, 0, 2, 3);
    app.cell_pixel = Some((1, 1));
    app.minimap_content_h = 3;
    app.minimap_scroll_to_row(0);
    assert_eq!(
        app.editor.cursor_row, 3,
        "the top of the strip is b's first line"
    );
}

/// #369: a documented symbol starts on its doc comment in the LSP outline
/// and on its item in the tree-sitter one; opened from both it gets one tab.
#[test]
fn a_documented_symbol_opened_from_both_outlines_gets_one_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("doc.rs");
    std::fs::write(&file, "/// doc\nfn foo() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.open_symbol_tab_for("foo".into(), 1, 1);
    app.editor.select(0);
    app.open_symbol_tab_for("foo".into(), 0, 1);
    let tabs = app
        .editor
        .editors
        .iter()
        .filter(|e| e.symbol_view.is_some())
        .count();
    assert_eq!(tabs, 1);
    assert!(
        app.editor.symbol_view.is_some(),
        "the existing tab is focused"
    );
}

/// #369: two same-named symbols of one kind and depth (a `fn new` in each
/// of two impls) are told apart by the line their item starts on.
#[test]
fn same_named_symbols_on_different_lines_get_a_tab_each() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("news.rs");
    std::fs::write(
        &file,
        "impl A {\n    fn new() {}\n}\nimpl B {\n    fn new() {}\n}\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.open_symbol_tab_for("new".into(), 1, 1);
    app.editor.select(0);
    app.open_symbol_tab_for("new".into(), 4, 4);
    let tabs = app
        .editor
        .editors
        .iter()
        .filter(|e| e.symbol_view.is_some())
        .count();
    assert_eq!(tabs, 2);
}

/// #369: switching between two symbol tabs of the same length repaints the
/// minimap (their other layout fields agree), and its viewport and
/// selection count from the symbol's first line.
#[test]
fn switching_between_same_length_symbol_tabs_repaints_the_minimap() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("two.rs");
    std::fs::write(&file, "fn a() {\n    1\n}\nfn b() {\n    2\n}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.cell_pixel = Some((8, 16));
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
    app.editor.open_pinned(&file).unwrap();
    app.open_symbol_tab_for("a".into(), 0, 2);
    app.editor.select(0);
    app.open_symbol_tab_for("b".into(), 3, 5);
    let tab = |app: &App, name: &str| {
        app.editor
            .editors
            .iter()
            .position(|e| e.symbol_view.as_ref().is_some_and(|v| v.name == name))
            .unwrap()
    };
    let (a, b) = (tab(&app, "a"), tab(&app, "b"));
    let strip = Rect::new(100, 1, 6, 20);
    app.editor.select(a);
    app.update_minimap_overlay(strip);
    assert_eq!(app.minimap_base.as_ref().map(|m| m.sig.5), Some((0, 3)));
    app.editor.select(b);
    app.update_minimap_overlay(strip);
    assert_eq!(app.minimap_base.as_ref().map(|m| m.sig.5), Some((3, 6)));
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (3, 0),
        head: (4, 2),
    });
    app.update_minimap_overlay(strip);
    let layout = app.overlays.minimap.layout().expect("laid out");
    assert_eq!((layout.top, layout.selection), (0, Some((0, 1))));
}

/// #369: a symbol tab is a tab of its own that shows only its symbol.
///
/// Opened beside the file's tab, titled by the symbol, holding the whole
/// file (so LSP, save and line numbers stay in file coordinates) while it
/// paints only the symbol's lines. `fn b` is the last symbol in the file:
/// its range must stop at the buffer's end, not a separator byte past it.
#[test]
fn a_symbol_tab_opens_beside_its_file_showing_only_the_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);

    assert_eq!(app.editor.editors.len(), 2, "a NEW tab, not a replaced one");
    assert_eq!(app.editor.active_index(), 1, "placed after its file's tab");
    let tab = &app.editor.editors[1];
    assert_eq!(tab.path.as_deref(), Some(file.as_path()));
    let view = tab.symbol_view.as_ref().expect("the tab is a symbol tab");
    assert_eq!((view.name.as_str(), view.first, view.last), ("b", 3, 5));
    let text = tab.lines.join("\n");
    assert_eq!(
        &text[view.range.start..view.range.end],
        "fn b() {\n    2\n}",
        "the range slices exactly the symbol, up to the buffer's end"
    );
    assert_eq!(tab.cursor_row, 4, "the caret stays where it was");

    // Scrolled to the top, the clip, not the scroll, keeps `fn a` out.
    app.editor.scroll = 0;
    let screen = painted(&mut app);
    assert!(
        screen.contains("b \u{b7} two.rs"),
        "titled by its symbol:\n{screen}"
    );
    assert!(
        screen.contains("fn b() {"),
        "the symbol is shown:\n{screen}"
    );
    assert!(
        !screen.contains("fn a() {"),
        "nothing outside the symbol is painted:\n{screen}"
    );

    // Opening it again focuses the existing tab rather than stacking one.
    app.editor.select(0);
    app.editor.cursor_row = 5;
    app.run_command(crate::widgets::command_palette::Command::OpenAsSymbolTab);
    assert_eq!(app.editor.editors.len(), 2);
    assert_eq!(app.editor.active_index(), 1);
}

/// #369: the caret and every edit stay inside the symbol.
///
/// Motions stop at its edges, and the edits that would join its first or
/// last line to a line outside it (Backspace at its start, Delete at its
/// end, moving a line across its edge) do nothing, rather than pulling text
/// the tab cannot show into or out of it.
#[test]
fn a_symbol_tab_keeps_the_caret_and_edits_inside_its_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_symbol_tab_on_b(&tmp);
    let before = app.editor.lines.clone();

    app.editor.cursor_row = 3;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Up, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.cursor_row, 3,
        "Up stops at the symbol's first line"
    );
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.lines, before,
        "Backspace at its start joins nothing"
    );
    app.handle_key(key(KeyCode::Up, KeyModifiers::ALT)).unwrap();
    assert_eq!(
        app.editor.lines, before,
        "its first line cannot move above it"
    );

    app.editor.cursor_row = 5;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::End, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        (app.editor.cursor_row, app.editor.cursor_col),
        (5, 1),
        "the caret parks at the symbol's end"
    );
    app.handle_key(key(KeyCode::Delete, KeyModifiers::NONE))
        .unwrap();
    app.editor.cursor_row = 5;
    app.handle_key(key(KeyCode::Down, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(
        app.editor.lines, before,
        "Delete and Alt+Down stop at its end"
    );

    // An ordinary edit inside still works.
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('0'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines[4], "    20");
}

/// #369: a symbol tab and its file's tab are one document.
///
/// Typing in either shows up in the other, an edit above the symbol moves
/// the clip with it, saving either clears both dirty dots, and a symbol
/// that is deleted closes its tab with a note saying why.
#[test]
fn a_symbol_tab_mirrors_its_file_and_follows_the_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();

    // Typing in the symbol tab reaches the file's tab.
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('7'), KeyModifiers::NONE))
        .unwrap();
    // The key itself mirrors the edit, so nothing is left for the tick.
    assert!(!app.sync_symbol_views());
    assert_eq!(app.editor.editors[0].lines[4], "    27");
    assert!(
        app.editor.editors[0].dirty,
        "the file's tab has the unsaved edit"
    );

    // Lines added above the symbol in the file's tab move the clip down.
    app.editor.select(0);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    app.sync_symbol_views();
    let tab = &app.editor.editors[1];
    assert_eq!(tab.lines, app.editor.editors[0].lines);
    let view = tab.symbol_view.as_ref().unwrap();
    assert_eq!(
        (view.first, view.last),
        (4, 6),
        "the clip moved with its symbol"
    );

    // Saving one clears both.
    app.editor.editors[0].save_to_disk().unwrap();
    app.sync_symbol_views();
    assert!(
        !app.editor.editors[1].dirty,
        "the saved text is the symbol tab's text too"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "\nfn a() {\n    1\n}\nfn b() {\n    27\n}"
    );

    // Renaming the symbol retitles the tab.
    app.editor.select(1);
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 4;
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app.sync_symbol_views();
    assert_eq!(
        app.editor.editors[1].symbol_view.as_ref().unwrap().name,
        "bx"
    );

    // Deleting it closes the tab.
    app.editor.select(0);
    let kept: Vec<String> = app.editor.lines[..4].to_vec();
    app.editor.replace_all_lines(kept);
    app.sync_symbol_views();
    assert_eq!(app.editor.editors.len(), 1, "the symbol tab closed");
    assert!(app.editor.editors[0].symbol_view.is_none());
    assert!(
        app.status.contains("bx") && app.status.contains("gone"),
        "the user is told which tab closed and why: {}",
        app.status
    );
}

/// #369: Alt+Enter in the workspace-symbol picker opens the symbol tab even
/// when the server names the file through a symlink.
///
/// The jump lands on the tab already open under the real path, so the
/// landed check has to compare the two spellings by their canonical form.
#[cfg(unix)]
#[test]
fn alt_enter_on_a_symlinked_symbol_still_opens_its_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let file = root.join("two.rs");
    std::fs::write(&file, "fn a() {\n    1\n}\nfn b() {\n    2\n}").unwrap();
    let link = root.join("link.c");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.open_workspace_symbols("b");
    app.ws_symbols_request_id = Some(1);
    app.apply_workspace_symbols(1, vec![ws_item("b", &link, 3)], false);
    app.handle_key(key(KeyCode::Enter, KeyModifiers::ALT))
        .unwrap();
    let view = app
        .editor
        .symbol_view
        .as_ref()
        .expect("the symbol tab opened");
    assert_eq!(view.name, "b");
}

/// #369: duplicating the symbol's last line keeps the copy inside it.
///
/// The copy lands below the closing brace, so the clip has to grow by the
/// one line rather than leave the new line outside the tab.
#[test]
fn duplicating_a_symbols_last_line_keeps_the_copy_inside_the_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("two.rs");
    std::fs::write(&file, "fn a() {\n    1\n}\nfn b() {\n    2\n}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.editor.cursor_row = 1;
    app.run_command(crate::widgets::command_palette::Command::OpenAsSymbolTab);
    app.sync_symbol_views();
    let view = app.editor.symbol_view.as_ref().expect("symbol tab on a");
    assert_eq!((view.first, view.last), (0, 2));

    app.editor.cursor_row = 2;
    app.editor.cursor_col = 0;
    app.editor.duplicate_lines_down();
    app.sync_symbol_views();
    assert_eq!(
        app.editor.lines[..5],
        ["fn a() {", "    1", "}", "}", "fn b() {"]
    );
    let view = app.editor.symbol_view.as_ref().expect("still a symbol tab");
    assert_eq!((view.first, view.last), (0, 3), "the clip grew by the copy");
    assert_eq!(app.editor.cursor_row, 3, "the caret sits on the copy");
}

/// #369: Cmd+S in the symbol tab writes the file and leaves both tabs clean,
/// with no conflict for the file's tab, which holds the same text.
#[test]
fn cmd_s_in_a_symbol_tab_saves_the_file_and_cleans_both_tabs() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('7'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.editors[0].dirty, "the file's tab has the edit");
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn a() {\n    1\n}\nfn b() {\n    27\n}"
    );
    assert!(!app.editor.editors[1].dirty, "the symbol tab is saved");
    assert!(!app.editor.editors[0].dirty, "so is the file's tab");
    assert!(
        !app.reload_open_file_after_external_change(),
        "the file's tab already holds the saved text"
    );
    assert!(app.input_prompt.is_none(), "no reload prompt");
}

/// #369: the focus-change sweep, which waits for no delay, also writes a
/// file with a symbol tab once and cleans both when focus leaves the editor.
#[test]
fn a_focus_change_save_writes_a_file_with_a_symbol_tab_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('7'), KeyModifiers::NONE))
        .unwrap();
    app.sync_symbol_views();
    assert!(app.editor.editors.iter().all(|e| e.dirty), "fixture");
    // Focus left the editor, so both tabs are due at once.
    app.focus = Pane::Tree;
    assert!(app.sweep_auto_save(false));
    assert!(std::fs::read_to_string(&file).unwrap().contains("27"));
    for e in &app.editor.editors {
        assert!(!e.dirty && !e.disk_conflict, "both tabs are saved");
    }
    assert!(app.input_prompt.is_none(), "no conflict prompt");
}

/// #369: right-clicking an OUTLINE row offers its symbol as a tab.
///
/// The row, not the caret, picks the symbol: the caret sits in `a` while
/// the click lands on `b`'s row.
#[test]
fn an_outline_row_right_click_opens_that_symbol_as_a_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("two.rs");
    std::fs::write(&file, "fn a() {\n    1\n}\nfn b() {\n    2\n}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    if !app.explorer_views.is_visible(ExplorerView::Outline) {
        app.toggle_explorer_view(ExplorerView::Outline);
    }
    app.outline.collapsed = false;
    let symbols = app.compute_syntax_outline(&file);
    app.outline.set_symbols(file.clone(), symbols);
    painted(&mut app);
    let area = app.outline.last_area;
    let row = (area.y..area.y + area.height)
        .find(|&y| {
            app.outline
                .row_at(y)
                .is_some_and(|i| app.outline.symbols()[i].name == "b")
        })
        .expect("b has an OUTLINE row");

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        area.x + 2,
        row,
    ));
    let menu = app.context_menu.as_mut().expect("the row has a menu");
    let idx = menu
        .items
        .iter()
        .position(|e| menu_label(e) == "Open Symbol in Its Own Tab")
        .expect("the menu offers the symbol tab");
    menu.selected = idx;
    app.handle_menu_key(key(KeyCode::Enter, KeyModifiers::NONE));

    let view = app
        .editor
        .symbol_view
        .as_ref()
        .expect("a symbol tab opened");
    assert_eq!((view.name.as_str(), view.first, view.last), ("b", 3, 5));
}

/// #369: a restart restores the file, not a duplicate of it.
///
/// The symbol tab mirrors its file's tab, so session capture drops it while
/// that tab is open, and keeps it (as a plain tab, with its unsaved text)
/// when it is the file's only tab.
#[test]
fn session_capture_keeps_a_symbol_tab_only_when_it_is_the_files_last_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    let state = app.capture_session_state();
    assert_eq!(state.tabs.len(), 1, "the file's tab alone");
    assert_eq!(state.active_tab, 0, "focus falls to the file's tab");

    app.editor.close_tab(0);
    app.editor.lines[4] = "    9".into();
    app.editor.dirty = true;
    let state = app.capture_session_state();
    assert_eq!(state.tabs.len(), 1);
    assert_eq!(state.tabs[0].path.as_ref(), Some(&file));
    assert!(
        state.tabs[0]
            .unsaved_text
            .as_deref()
            .unwrap()
            .contains("    9")
    );
}

/// #369: scrolling a symbol tab stops once its last line reaches the pane's
/// bottom; a short symbol never leaves a lone line above blank rows.
#[test]
fn a_symbol_tab_never_scrolls_its_tail_off_a_pane_it_fits() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    app.editor.cursor_row = 5;
    app.editor.cursor_col = 0;
    app.editor.scroll = 5;
    painted(&mut app);
    assert_eq!(
        app.editor.scroll, 3,
        "fn b's three lines fill the pane top down"
    );
}

/// #369: multi-cursor commands in a symbol tab stay inside the symbol:
/// Change All Occurrences matches only there, Add Cursor Above stops at its
/// first line, and a caret that lands outside is dropped rather than
/// stacked on the edge.
#[test]
fn multi_cursor_commands_stay_in_the_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    app.editor.cursor_row = 3;
    app.editor.cursor_col = 0;
    assert_eq!(
        app.editor.select_all_occurrences_of_word_at_cursor(),
        1,
        "fn a's `fn` is outside"
    );
    assert!(app.editor.carets.is_empty());
    app.editor.selection = None;
    app.editor.add_cursor_above();
    assert!(app.editor.carets.is_empty(), "no caret above the symbol");
    for row in [0, 1] {
        app.editor
            .carets
            .push(crate::widgets::editor::EditorSelection::new(row, 0));
    }
    app.editor.clamp_to_symbol_view();
    assert!(app.editor.carets.is_empty(), "outside carets are dropped");
    // Two copies of one inside caret, and one on the primary, leave one.
    let (row, col) = (app.editor.cursor_row, app.editor.cursor_col);
    for (r, c) in [(4, 0), (4, 0), (row, col)] {
        app.editor
            .carets
            .push(crate::widgets::editor::EditorSelection::new(r, c));
    }
    app.editor.clamp_to_symbol_view();
    assert_eq!(app.editor.carets.len(), 1, "duplicates are removed");
    app.editor.carets.clear();
    // Add Selection to Next Match finds no `fn` outside the symbol.
    app.editor.cursor_row = 3;
    app.editor.cursor_col = 0;
    assert_eq!(app.editor.select_next_occurrence(), 1, "selects the word");
    assert_eq!(
        app.editor.select_next_occurrence(),
        1,
        "fn a's `fn` is outside"
    );
    assert!(app.editor.carets.is_empty());
}

/// #369: Add Cursor Below in a symbol tab stops at the symbol's last line,
/// with the rest of the file below it.
#[test]
fn add_cursor_below_stops_at_the_symbols_end() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("two.rs");
    std::fs::write(&file, "fn a() {\n    1\n}\nfn b() {\n    2\n}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.editor.cursor_row = 1;
    app.run_command(crate::widgets::command_palette::Command::OpenAsSymbolTab);
    app.sync_symbol_views();
    assert_eq!(app.editor.symbol_clip(), Some((0, 3)));
    app.editor.cursor_row = 2;
    app.editor.cursor_col = 0;
    app.editor.add_cursor_below();
    assert!(app.editor.carets.is_empty(), "fn b is not the tab's");
}

/// #369: a collaborator's edit is not the symbol tab's own. A line they
/// open at the start of the symbol's first line stays above the symbol.
#[test]
fn a_collaborators_edit_on_a_symbols_edge_is_not_the_tabs_own() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    for ed in app.editor.editors.iter_mut() {
        ed.apply_span_edits(&[crate::widgets::editor::TextSpanEdit {
            start: (3, 0),
            end: (3, 0),
            new_text: String::from("\n"),
            utf16: false,
        }]);
        // The local caret sits just past the new line, where a caret that
        // had typed it would be.
        ed.cursor_row = 4;
        ed.cursor_col = 0;
        ed.collab_doc_gen = 1;
        ed.collab_synced_seq = ed.edit_seq;
    }
    app.sync_symbol_views();
    let view = app.editor.editors[1].symbol_view.as_ref().unwrap();
    assert_eq!(
        (view.first, view.last),
        (4, 6),
        "the new line is above fn b"
    );
}

/// #369: an edit on a symbol's edge lands on the side its caret is on.
///
/// Enter at the end of the symbol, typed in its own tab, opens a line the
/// tab keeps showing, so the caret stays on it; Enter at the start of the
/// symbol's first line, typed in the file's tab, opens a line above it that
/// the clip leaves out.
#[test]
fn a_line_opened_on_a_symbols_edge_goes_where_the_caret_is() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();

    app.editor.cursor_row = 5;
    app.editor.cursor_col = 1;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('z'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines[6], "z", "the caret stayed on the new line");
    assert_eq!(app.editor.cursor_row, 6);
    let view = app.editor.editors[1].symbol_view.as_ref().unwrap();
    assert_eq!((view.first, view.last), (3, 6), "the symbol grew");

    app.editor.select(0);
    app.editor.cursor_row = 3;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let view = app.editor.editors[1].symbol_view.as_ref().unwrap();
    assert_eq!(
        (view.first, view.last),
        (4, 7),
        "the opened line is above the symbol"
    );
}

/// #369: deleting a symbol tab's whole symbol closes the tab while a tab of
/// the whole file still holds the text.
#[test]
fn a_symbol_tab_whose_symbol_is_gone_closes() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    assert_eq!(app.editor.editors.len(), 2);
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    app.sync_symbol_views();
    assert_eq!(app.editor.editors.len(), 1, "{}", app.status);
    assert!(app.status.contains("that symbol is gone"), "{}", app.status);
    assert!(app.editor.symbol_view.is_none());
}

/// #369: a symbol tab left as the only tab of its file keeps its unsaved
/// edits when its symbol is deleted: it becomes a tab of the whole file
/// instead of closing and taking them with it.
#[test]
fn an_orphaned_symbol_tab_with_unsaved_edits_survives_its_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    // An edit outside the symbol, made in the file tab and mirrored in.
    app.editor.select(0);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('9'), KeyModifiers::NONE))
        .unwrap();
    app.sync_symbol_views();
    app.editor.close_tab(0);
    assert!(
        app.editor.symbol_view.is_some(),
        "only the symbol tab is left"
    );
    assert!(app.editor.dirty);
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    app.sync_symbol_views();
    assert_eq!(app.editor.editors.len(), 1, "the tab stays: {}", app.status);
    assert!(
        app.editor.symbol_view.is_none(),
        "it shows the whole file now"
    );
    assert_eq!(app.editor.path.as_deref(), Some(file.as_path()));
    assert_eq!(
        app.editor.lines[1], "    19",
        "the out-of-clip edit survives"
    );
    assert!(app.editor.dirty);
    assert!(app.status.contains("whole file"), "{}", app.status);
}

/// #369: a tab that turned into a whole-file tab keeps the keys typed
/// right after, in the same burst, while another symbol tab of its file is
/// still open to mirror against.
#[test]
fn a_kept_orphan_keeps_the_next_keys_beside_a_sibling_symbol_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    app.editor.select(0);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.run_command(crate::widgets::command_palette::Command::OpenAsSymbolTab);
    app.sync_symbol_views();
    let whole = |app: &App| {
        app.editor
            .editors
            .iter()
            .position(|e| e.symbol_view.is_none())
    };
    app.editor.close_tab(whole(&app).expect("the file tab"));
    assert_eq!(app.editor.editors.len(), 2, "two symbol tabs are left");
    let b = app
        .editor
        .editors
        .iter()
        .position(|e| e.symbol_view.as_ref().is_some_and(|v| v.name == "b"))
        .expect("the b tab");
    app.editor.select(b);
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    let kept = whole(&app).expect("the b tab became a file tab");
    assert_eq!(
        app.editor.editors[kept].path.as_deref(),
        Some(file.as_path())
    );
    assert!(
        app.editor.editors[kept].lines.iter().any(|l| l == "x"),
        "{:?}",
        app.editor.editors[kept].lines
    );
}

/// #369: one symbol's tabs in two groups, with no file tab left in either:
/// deleting the symbol keeps one of them as the file's tab, and the other,
/// whose edits that tab now holds, closes and folds its group away.
#[test]
fn one_symbols_tabs_in_two_groups_keep_a_single_file_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    app.split_editor();
    app.editor.cursor_row = 4;
    app.run_command(crate::widgets::command_palette::Command::OpenAsSymbolTab);
    app.sync_symbol_views();
    let file_tab = |tabs: &crate::widgets::editor::EditorTabs| {
        tabs.editors
            .iter()
            .position(|e| e.symbol_view.is_none())
            .expect("the file tab")
    };
    let i = file_tab(&app.editor);
    app.editor.close_tab(i);
    let other = &mut app.editor_layout.inactive_groups_mut()[0];
    let i = file_tab(other);
    other.close_tab(i);
    assert_eq!(app.editor.editors.len(), 1);
    assert_eq!(app.editor_layout.inactive_groups()[0].editors.len(), 1);
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.editor_layout.inactive_groups().is_empty(),
        "the other copy closed and its group folded away"
    );
    assert_eq!(app.editor.editors.len(), 1);
    let kept = &app.editor.editors[0];
    assert!(kept.symbol_view.is_none() && kept.dirty);
    assert_eq!(kept.path.as_deref(), Some(file.as_path()));
    assert!(app.status.contains("whole file"), "{}", app.status);
}

/// Opens `impl S` and `fn m` as symbol tabs, closes the file tab, deletes
/// m's line from the impl tab one Backspace at a time (m goes on the last
/// one), then presses undo once in the impl tab or in the m tab (by then
/// the file's tab); hands back the line undo left.
fn undo_once_after_m_goes(in_kept: bool) -> String {
    fn tab(app: &App, name: &str) -> Option<usize> {
        app.editor
            .editors
            .iter()
            .position(|e| e.symbol_view.as_ref().is_some_and(|v| v.name == name))
    }
    let m_line = "    fn m() {}";
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("s.rs");
    std::fs::write(&file, format!("impl S {{\n{m_line}\n}}\n")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    for row in [1, 0] {
        app.editor.select(0);
        app.editor.cursor_row = row;
        app.editor.cursor_col = 0;
        app.run_command(crate::widgets::command_palette::Command::OpenAsSymbolTab);
        app.sync_symbol_views();
    }
    let i = app
        .editor
        .editors
        .iter()
        .position(|e| e.symbol_view.is_none())
        .expect("the file tab");
    app.editor.close_tab(i);
    let imp = tab(&app, "S").expect("the impl tab");
    app.editor.select(imp);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = m_line.chars().count();
    app.focus_pane(Pane::Editor);
    for _ in 0..m_line.chars().count() {
        app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
    }
    assert!(tab(&app, "m").is_none(), "m is gone");
    let kept = app
        .editor
        .editors
        .iter()
        .position(|e| e.symbol_view.is_none())
        .expect("the m tab became the file tab");
    assert_eq!(
        app.editor.editors[kept].lines[1], "",
        "the kept tab mirrors the emptied line"
    );
    let at = if in_kept {
        kept
    } else {
        tab(&app, "S").expect("the impl tab")
    };
    app.editor.select(at);
    app.handle_key(key(KeyCode::Char('z'), KeyModifiers::SUPER))
        .unwrap();
    app.editor.lines[1].clone()
}

/// #369: a method's symbol tab kept as the file's tab while the impl tab's
/// Backspaces delete the method keeps taking one undo step per Backspace,
/// so one undo there walks back as far as it does in the impl tab. m goes
/// on the last Backspace, in the pass that last mirrored it: the kept tab
/// must stay a mirror member (its `mirror_seq` kept), or its undo is
/// overwritten by the impl tab's text.
#[test]
fn a_kept_orphan_undoes_the_backspaces_that_deleted_its_symbol_like_its_sibling() {
    let in_impl = undo_once_after_m_goes(false);
    assert_ne!(
        in_impl, "",
        "undo in the impl tab restores part of the line"
    );
    assert_eq!(undo_once_after_m_goes(true), in_impl);
}

/// #369: a clean symbol tab whose symbol goes closes even with no file
/// tab open: it holds nothing the disk does not.
#[test]
fn a_clean_orphaned_symbol_tab_closes_when_its_symbol_goes() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    app.editor.close_tab(0);
    assert!(app.editor.symbol_view.is_some() && !app.editor.dirty);
    std::fs::write(&file, "fn a() {\n    1\n}\n").unwrap();
    app.editor.reload_if_clean().unwrap().unwrap();
    app.sync_symbol_views();
    assert!(
        app.editor.editors.iter().all(|e| e.path.is_none()),
        "{}",
        app.status
    );
    assert!(app.status.contains("that symbol is gone"), "{}", app.status);
}

/// #369: going to a file lands on the tab that shows ALL of it.
///
/// A symbol tab has the file's path, and the tab lookups behind opening,
/// previewing and selecting a file would otherwise pick it and show a jump
/// target that sits outside its clip as the clip's nearest edge.
#[test]
fn opening_a_file_never_lands_in_its_symbol_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_symbol_tab_on_b(&tmp);
    app.editor.close_tab(0);
    assert!(
        app.editor.editors[0].symbol_view.is_some(),
        "only the symbol tab is left"
    );

    app.open_at_utf16(&file, 0, 0).unwrap();
    assert!(
        app.editor.symbol_view.is_none(),
        "the jump opened a tab of the whole file"
    );
    assert_eq!(app.editor.cursor_row, 0, "and landed where it was sent");
    assert_eq!(
        app.editor.editors.len(),
        2,
        "the symbol tab stays open beside it"
    );
}

/// Wait for the OUTLINE to show the scrubbed commit's own symbols (#371),
/// which the builder parses on its own thread with the finished view.
fn settle_scrub_outline(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        app.drain_scrub_views();
        app.sync_outline();
        let commit = app
            .scrubber
            .as_ref()
            .and_then(|s| s.commit())
            .map(|c| c.hash.clone());
        if commit.is_some() && app.outline_scrub_key.as_ref().map(|(_, c)| c.clone()) == commit {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the scrubbed commit's outline was never built"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn the_outline_and_breadcrumbs_follow_the_scrubbed_commit() {
    // #371: while scrubbing, the Outline lists the file as it was at the
    // commit on screen, the breadcrumbs follow the historical caret, a
    // symbol jump lands in the history, and leaving brings the live
    // outline back.
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "a@b"]);
    git(&["config", "user.name", "a"]);
    let file = tmp.path().join("lib.rs");
    std::fs::write(&file, "fn alpha() {\n    let a = 1;\n}\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "alpha"]);
    std::fs::write(
        &file,
        "fn beta() {\n}\n\nfn alpha() {\n    let a = 1;\n}\n\nfn gamma() {\n}\n",
    )
    .unwrap();
    git(&["commit", "-q", "-am", "beta and gamma"]);

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();
    let names = |app: &App| -> Vec<String> {
        app.outline
            .symbols()
            .iter()
            .map(|s| s.name.clone())
            .collect()
    };
    app.sync_outline();
    assert_eq!(
        names(&app),
        ["beta", "alpha", "gamma"],
        "control: live outline"
    );

    app.scrub_history();
    assert!(app.handle_scrubber_key(KeyCode::Left), "HEAD");
    assert!(app.handle_scrubber_key(KeyCode::Left), "the root commit");
    settle_scrub_outline(&mut app);
    assert_eq!(names(&app), ["alpha"], "the commit's outline");

    // A jump to `alpha` lands in the historical view, where it is line 0,
    // and the breadcrumbs name it from there.
    let target = app.outline.jump_target(0).unwrap().1;
    assert!(app.jump_in_scrub_view(target));
    assert_eq!(app.scrub_view.as_ref().unwrap().cursor_row, 0);
    app.editor.cursor_row = 7; // the live caret, inside `gamma`
    let crumbs: Vec<String> = app
        .build_breadcrumbs()
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert_eq!(
        crumbs.last().map(String::as_str),
        Some("alpha"),
        "{crumbs:?}"
    );

    // Switching to a file that did not exist at this commit: its outline
    // is its own (none), never the previous file's history under its name.
    let other = tmp.path().join("other.rs");
    std::fs::write(&other, "fn zeta() {}\n").unwrap();
    app.editor.open(&other).unwrap();
    app.sync_outline();
    assert!(names(&app).is_empty(), "{:?}", names(&app));
    app.editor.open(&file).unwrap();
    app.sync_outline();
    assert_eq!(names(&app), ["alpha"]);

    assert!(
        app.handle_scrubber_key(KeyCode::Home),
        "back to the working tree"
    );
    app.sync_outline();
    assert_eq!(
        names(&app),
        ["beta", "alpha", "gamma"],
        "the live outline is back"
    );
    assert!(!app.jump_in_scrub_view(0), "no history on screen");
}

#[test]
fn a_click_on_a_secondary_outline_row_jumps_to_that_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = secondary_outline_app(&tmp, 30);
    // The usual reason for the secondary Outline: the primary side bar is
    // hidden, so this is the only Outline on screen. The Explorer's OUTLINE
    // section is folded, as it is by default.
    app.toggle_side_bar();
    app.outline.collapsed = true;
    app.editor.cursor_row = 58;
    let backend = ratatui::backend::TestBackend::new(160, 45);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let (_, right) = outline_hosts(&app);
    let (x, y) = find_painted(&term, "sym_010", right).expect("sym_010 in the secondary bar");
    left_click(&mut app, x + 2, y);
    assert_eq!(
        app.editor.cursor_row, 20,
        "the caret moves to sym_010's line"
    );
}

#[test]
fn wheeling_one_outline_host_leaves_the_others_scroll_alone() {
    use crossterm::event::MouseEventKind;
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = secondary_outline_app(&tmp, 60);
    let backend = ratatui::backend::TestBackend::new(160, 45);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let (left, right) = outline_hosts(&app);
    assert!(find_painted(&term, "sym_000", left.clone()).is_some());
    let (x, y) = find_painted(&term, "sym_005", right.clone()).expect("secondary rows");
    wheel(&mut app, MouseEventKind::ScrollDown, x, y);
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        find_painted(&term, "sym_000", right.clone()).is_none(),
        "the secondary Outline scrolled"
    );
    assert!(
        find_painted(&term, "sym_000", left.clone()).is_some(),
        "the Explorer's OUTLINE did not move with it"
    );

    // And the other way round: the Explorer's wheel leaves the secondary be.
    let (x, y) = find_painted(&term, "sym_002", left.clone()).expect("explorer rows");
    wheel(&mut app, MouseEventKind::ScrollDown, x, y);
    wheel(&mut app, MouseEventKind::ScrollDown, x, y);
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        find_painted(&term, "sym_000", left).is_none(),
        "the Explorer scrolled"
    );
    assert!(
        find_painted(&term, "sym_003", right).is_some(),
        "the secondary Outline kept its own offset"
    );
}

#[test]
fn a_click_on_the_secondary_outline_header_or_blank_rows_moves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = secondary_outline_app(&tmp, 3);
    app.toggle_side_bar();
    app.outline.collapsed = true;
    app.editor.cursor_row = 3;
    let backend = ratatui::backend::TestBackend::new(160, 45);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let (_, right) = outline_hosts(&app);
    let (x, y) = find_painted(&term, "OUTLINE", right.clone()).expect("secondary header");
    left_click(&mut app, x + 1, y);
    let (sx, sy) = find_painted(&term, "sym_002", right.clone()).unwrap();
    // Well below the last symbol: inside the bar, on no row.
    left_click(&mut app, sx, sy + 10);
    assert_eq!(app.editor.cursor_row, 3, "neither click moved the caret");
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        find_painted(&term, "sym_000", right).is_some(),
        "the header click did not fold the secondary Outline away"
    );
}

// ---- An empty Go to Definition / Declaration / Type Definition reply (#1302) ----

/// `m.py` open with the caret on `undefined_name`, as in the issue.
fn jump_app(dir: &std::path::Path) -> App {
    std::fs::write(
        dir.join("m.py"),
        "total = 42\nprint(total + undefined_name)\n",
    )
    .unwrap();
    let mut app = App::new(dir.to_path_buf()).unwrap();
    app.editor.open_pinned(&dir.join("m.py")).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 16;
    app.status = String::from("Jumped to m.py line 2");
    app
}

#[test]
fn an_empty_jump_reply_says_nothing_was_found_and_clears_the_request() {
    use crate::lsp::manager::{DeclarationResult, DefinitionResult, TypeDefinitionResult};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = jump_app(tmp.path());

    app.request_definition_at_cursor();
    let id = app.definition_request_id.expect("F12 sent a request");
    app.lsp
        .as_ref()
        .unwrap()
        .push_definition_for_test(DefinitionResult {
            request_id: id,
            target: None,
        });
    let redrew = app.drain_lsp_definition();
    assert_eq!(app.status, "No definition found");
    assert!(redrew, "the empty reply redraws");
    assert_eq!(app.definition_request_id, None);

    app.request_declaration_at_cursor();
    let id = app.declaration_request_id.expect("a declaration request");
    app.lsp
        .as_ref()
        .unwrap()
        .push_declaration_for_test(DeclarationResult {
            request_id: id,
            target: None,
            unsupported: false,
        });
    assert!(app.drain_lsp_declaration());
    assert_eq!(app.status, "No declaration found");
    assert_eq!(app.declaration_request_id, None);

    app.request_type_definition_at_cursor();
    let id = app
        .type_definition_request_id
        .expect("a type definition request");
    app.lsp
        .as_ref()
        .unwrap()
        .push_type_definition_for_test(TypeDefinitionResult {
            request_id: id,
            target: None,
            unsupported: false,
        });
    assert!(app.drain_lsp_type_definition());
    assert_eq!(app.status, "No type definition found");
    assert_eq!(app.type_definition_request_id, None);
}

#[test]
fn a_stale_jump_reply_changes_nothing_and_a_found_target_still_jumps() {
    // Negative: only the outstanding request's reply speaks, and a reply
    // with a target jumps as before instead of reporting "not found".
    use crate::lsp::manager::DefinitionResult;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = jump_app(tmp.path());
    let path = app.editor.path.clone().unwrap();
    app.request_definition_at_cursor();
    let id = app.definition_request_id.expect("F12 sent a request");
    app.lsp
        .as_ref()
        .unwrap()
        .push_definition_for_test(DefinitionResult {
            request_id: id + 1000,
            target: None,
        });
    assert!(!app.drain_lsp_definition(), "a stale reply is dropped");
    assert_eq!(app.status, "Jumped to m.py line 2");
    assert_eq!(app.definition_request_id, Some(id), "still waiting");

    app.lsp
        .as_ref()
        .unwrap()
        .push_definition_for_test(DefinitionResult {
            request_id: id,
            target: Some((path, 0, 0)),
        });
    assert!(app.drain_lsp_definition());
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 0));
    assert_ne!(app.status, "No definition found");
}
