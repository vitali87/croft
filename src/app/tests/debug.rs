//! Run and Debug: launch configurations, breakpoints, debug sessions and the
//! adapters behind them.

use super::*;

#[test]
fn cmd_k_shift_enter_debugs_instead_of_running_the_test_at_cursor() {
    // The Shift arm must precede the plain-Enter run arm, or the chord
    // silently RUNS the test it was asked to DEBUG.
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("t.rs");
    std::fs::write(&f, "#[test]\nfn my_case() { assert!(true); }\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.editor.cursor_row = 1;
    assert!(app.handle_cmd_k_chord(key(KeyCode::Enter, KeyModifiers::SHIFT)));
    // The tmp root has no manifest, so the debug path reports the missing
    // runner — the run path would have said "Running test my_case".
    assert_eq!(app.status, "No test runner detected in this workspace");
    assert!(
        !app.testing.is_busy(),
        "the debug chord must not start a run"
    );
}

#[test]
fn debug_stop_immediately_deactivates_the_run_debug_panel() {
    // Repro: Shift+F5 tore down the session but left the panel showing the
    // stale "Paused" tree, because poll_dap early-returns with no session and
    // never refreshes. debug_stop must refresh the panel itself.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Simulate a paused panel.
    app.run_debug.debug_active = true;
    app.run_debug.feedback = Some("Paused (breakpoint)".into());
    app.run_debug.debug_rows = vec![crate::widgets::run_debug::DebugRow {
        indent: 0,
        kind: crate::widgets::run_debug::DebugRowKind::Header {
            title: "CALL STACK".into(),
        },
    }];
    app.debug_stop();
    assert!(
        !app.run_debug.debug_active,
        "panel must deactivate the instant the session stops"
    );
    assert!(
        app.run_debug.debug_rows.is_empty(),
        "stale call-stack/variables rows must be cleared on stop"
    );
    assert!(
        app.run_debug.feedback.is_none(),
        "stale 'Paused' feedback must be cleared so the empty state is clean"
    );
}

#[test]
fn lldb_dap_rank_prefers_unversioned_then_highest_version() {
    // The Linux bug: lldb-dap ships under versioned names (`lldb-dap-18`) with
    // no unversioned symlink, so resolution must rank candidates rather than
    // only matching the bare `lldb-dap` name.
    // Non-matches return None.
    assert_eq!(lldb_dap_rank("lldb"), None);
    assert_eq!(lldb_dap_rank("lldb-dap-"), None);
    assert_eq!(lldb_dap_rank("lldb-dapper"), None);
    assert_eq!(lldb_dap_rank("clangd"), None);

    // Every real adapter name ranks.
    let unversioned = lldb_dap_rank("lldb-dap").expect("bare name must rank");
    let v18 = lldb_dap_rank("lldb-dap-18").expect("versioned name must rank");
    let v17 = lldb_dap_rank("lldb-dap-17").expect("versioned name must rank");
    let legacy_v18 = lldb_dap_rank("lldb-vscode-18").expect("legacy name must rank");

    // The unversioned binary wins outright (it tracks the system default).
    assert!(
        unversioned > v18,
        "unversioned lldb-dap outranks any versioned"
    );
    // Among versioned binaries, the newer LLVM release wins.
    assert!(v18 > v17, "lldb-dap-18 outranks lldb-dap-17");
    // At equal version, the modern `lldb-dap` name beats legacy `lldb-vscode`.
    assert!(
        v18 > legacy_v18,
        "lldb-dap-18 is preferred over the legacy lldb-vscode-18 name"
    );
}

#[test]
fn lldb_dap_missing_message_is_platform_appropriate() {
    // Regression: Linux users saw "install Xcode Command Line Tools", which is
    // nonsense off macOS. The hint must match the host's package manager.
    let msg = lldb_dap_missing_message();
    assert!(
        msg.contains("lldb-dap not found"),
        "message must name the missing adapter: {msg}"
    );
    if cfg!(target_os = "macos") {
        assert!(
            msg.contains("Xcode"),
            "macOS hint should mention Xcode: {msg}"
        );
    } else {
        assert!(
            !msg.contains("Xcode"),
            "non-macOS hint must not mention Xcode: {msg}"
        );
    }
}

#[test]
fn f5_failure_reaches_the_status_bar_when_run_debug_panel_is_hidden() {
    // Repro (remote Linux with no lldb-dap installed): pressing F5 on a file
    // croft can't debug wrote the reason ONLY into the Run/Debug panel's
    // feedback line, which renders solely while that sidebar is the active
    // view. With the Explorer focused the message landed nowhere, so F5 looked
    // like it did nothing. Every debug-start failure must also reach the
    // always-visible status bar. Exercised here via the platform-independent
    // "no adapter for this file type" branch (the lldb-missing branch shares
    // the same surfacing path but can't be reproduced on a dev Mac, where the
    // Xcode lldb-dap is always present).
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, b"hi\n").unwrap();
    app.editor.path = Some(file);
    app.set_sidebar_view(SidebarView::Explorer);

    app.debug_start_or_continue();

    assert!(
        app.status.contains("No debugger"),
        "F5 failure must explain itself in the status bar, not just the hidden panel: {}",
        app.status
    );
}

#[test]
fn run_debug_icon_clears_when_a_paused_session_takes_over_the_panel() {
    // Repro (Run/Debug panel already open when F5 starts a session, as seen on
    // a remote): the empty-state debug icon is an OSC-1337 inline image. When
    // the panel flips to the paused call-stack tree the widget stops re-emitting
    // it (last_icon_cell = None), but text drawn over an inline image does NOT
    // evict it, so the icon ghosted on top of the call stack. The flush must arm
    // a one-shot terminal clear whenever a previously-emitted icon is no longer
    // shown — the same eviction the welcome/hero overlays use.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.show_tree = true;
    app.set_sidebar_view(SidebarView::RunDebug);
    app.overlays
        .run_debug
        .set_image("\x1b]1337;File=inline=1:AAAA\x07".to_string());
    app.overlays.run_debug.mark_emitted_at((5, 5));
    // A paused session now owns the panel: render draws the tree, not the icon.
    app.run_debug.last_icon_cell = None;

    app.flush_run_debug_icon_overlay();

    assert!(
        app.consume_run_debug_image_clear(),
        "a one-shot terminal clear must be armed to evict the ghosted debug icon"
    );
    assert!(
        !app.overlays.run_debug.was_emitted(),
        "emitted-position tracking must reset so the clear fires exactly once"
    );
}

/// #1544: a left click in the breakpoint lane sets a breakpoint on the
/// clicked line, a second click removes it, and the caret stays put.
#[test]
fn a_left_click_in_the_breakpoint_lane_toggles_a_breakpoint_on_that_line() {
    let (mut app, _tmp, f) = app_with_painted_file("l1\nl2\nl3\nl4\nl5\n");
    let lane = app.editor.last_inner.x;
    let row = app.editor.last_inner.y + 2;
    click_and_release(&mut app, lane, row);
    assert_eq!(app.editor.breakpoint_lines(&f), vec![3u32]);
    assert_eq!(app.status, "Breakpoint set at line 3");
    assert_eq!(app.editor.cursor_row, 0, "the caret stays on line 1");
    // Past the double-click window, so the second click is its own.
    app.editor_click.clear();
    click_and_release(&mut app, lane, row);
    assert!(app.editor.breakpoint_lines(&f).is_empty());
    assert_eq!(app.status, "Breakpoint removed from line 3");
    assert_eq!(app.editor.cursor_row, 0);
}

/// #1544: drags after a breakpoint-lane press belong to the lane, so the
/// caret stays put and no selection is drawn from it to the clicked line,
/// even when the terminal sends a drag at the press cell. The next gesture
/// in the text selects as usual.
#[test]
fn drags_after_a_breakpoint_lane_press_neither_move_the_caret_nor_select() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, f) = app_with_painted_file("l1\nl2\nl3\nl4\nl5\n");
    let lane = app.editor.last_inner.x;
    let y = app.editor.last_inner.y;
    let text_x = lane + app.editor.last_gutter_width + 1;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), lane, y + 2));
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), lane, y + 2));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        text_x,
        y + 3,
    ));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), text_x, y + 3));
    assert_eq!(app.editor.breakpoint_lines(&f), vec![3u32]);
    assert_eq!(app.editor.cursor_row, 0, "the caret stays on line 1");
    assert!(
        app.editor.selection.is_none_or(|s| !s.has_area()),
        "no selection from the caret to the clicked line"
    );
    app.editor_click.clear();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        text_x,
        y + 1,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        text_x + 1,
        y + 1,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        text_x + 1,
        y + 1,
    ));
    assert!(
        app.editor.selection.is_some_and(|s| s.has_area()),
        "a text drag after the lane gesture still selects"
    );
}

/// "Add Conditional Breakpoint" must open a visible popup (the centered
/// `Prompt` overlay), NOT capture keystrokes on the bottom status line. The
/// status line is reserved for read-only info.
#[test]
fn add_conditional_breakpoint_opens_a_popup_not_the_status_line() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("prog.py");
    std::fs::write(&f, "l1\nl2\nl3\nl4\nl5\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    let gutter_col = app.editor.last_inner.x;
    let row = app.editor.last_inner.y + 2; // 1-based line 3

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        gutter_col,
        row,
    ));
    let idx = app
        .context_menu
        .as_ref()
        .unwrap()
        .items
        .iter()
        .position(|e| menu_label(e) == "Add Conditional Breakpoint\u{2026}")
        .expect("menu must offer Add Conditional Breakpoint");
    app.context_menu.as_mut().unwrap().selected = idx;
    app.handle_menu_key(key(KeyCode::Enter, KeyModifiers::NONE));

    // A visible popup must open on the clicked line; nothing should be armed
    // on the status line.
    let prompt = app
        .prompt
        .as_ref()
        .expect("conditional breakpoint must open a popup prompt, not a status-line input");
    assert!(
        matches!(prompt.kind, PromptKind::BreakpointCondition { line: 3, .. }),
        "popup must target the clicked line (3)"
    );

    // Type a condition into the popup and commit it via the real key path.
    for c in "x > 1".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    assert!(app.prompt.is_none(), "committing must close the popup");
    assert_eq!(
        app.editor.breakpoint_lines(&f),
        vec![3u32],
        "committing a condition must create the breakpoint on line 3"
    );
    let lines = app.editor.breakpoints.get(&f).unwrap();
    let specs = app.editor.source_breakpoints(&f, lines);
    assert_eq!(
        specs[0].condition.as_deref(),
        Some("x > 1"),
        "the typed expression must be attached as the breakpoint condition"
    );
}

/// The gutter menu's "Add Logpoint" opens the popup prompt; committing a
/// message creates the breakpoint with a `logMessage` attached, so the
/// adapter prints instead of pausing (VS Code's logpoint).
#[test]
fn add_logpoint_attaches_a_log_message_to_the_line() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("prog.py");
    std::fs::write(&f, "l1\nl2\nl3\nl4\nl5\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    let gutter_col = app.editor.last_inner.x;
    let row = app.editor.last_inner.y + 2; // 1-based line 3
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        gutter_col,
        row,
    ));
    let idx = app
        .context_menu
        .as_ref()
        .unwrap()
        .items
        .iter()
        .position(|e| menu_label(e) == "Add Logpoint")
        .expect("menu must offer Add Logpoint");
    app.context_menu.as_mut().unwrap().selected = idx;
    app.handle_menu_key(key(KeyCode::Enter, KeyModifiers::NONE));

    let prompt = app.prompt.as_ref().expect("logpoint must open a popup");
    assert!(
        matches!(prompt.kind, PromptKind::Logpoint { line: 3, .. }),
        "popup must target the clicked line (3)"
    );
    for c in "x is {x}".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    assert!(app.prompt.is_none(), "committing must close the popup");
    assert_eq!(
        app.editor.breakpoint_lines(&f),
        vec![3u32],
        "committing a logpoint must create the breakpoint on line 3"
    );
    let lines = app.editor.breakpoints.get(&f).unwrap();
    let specs = app.editor.source_breakpoints(&f, lines);
    assert_eq!(
        specs[0].log_message.as_deref(),
        Some("x is {x}"),
        "the typed message must ride the breakpoint as logMessage"
    );
    assert_eq!(
        specs[0].condition, None,
        "a logpoint sets no condition of its own"
    );
}

#[test]
fn leaving_run_debug_after_emitting_the_icon_arms_a_one_shot_terminal_clear() {
    // Regression for the bug+play headline icon ghosting on top of
    // whichever sidebar replaced Run-Debug. iTerm2's OSC-1337 image
    // cells survive plain SGR overwrites, so the welcome / editor
    // pipelines arm a `terminal.clear()` on the next render to evict
    // them. Run-Debug needs the same gate.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Pretend init_graphics succeeded and the panel emitted the icon
    // while the user was on Run-Debug.
    app.overlays
        .run_debug
        .set_image("\x1b]1337;File=...\x07".to_string());
    app.set_sidebar_view(SidebarView::RunDebug);
    app.run_debug.last_icon_cell = Some((10, 5));
    app.flush_run_debug_icon_overlay();
    assert!(
        app.overlays.run_debug.was_emitted(),
        "test setup: the flush must record the emit position"
    );
    // Switch away. The clear flag must be armed so the main loop's
    // OR chain fires `terminal.clear()` once before the next draw.
    app.set_sidebar_view(SidebarView::Explorer);
    assert!(
        app.consume_run_debug_image_clear(),
        "leaving Run-Debug after emitting the icon must arm exactly one terminal.clear() so iTerm2's image cells are evicted",
    );
    assert!(
        !app.consume_run_debug_image_clear(),
        "the clear request must be one-shot — consuming twice in a row returns false",
    );
}

#[test]
fn flush_run_debug_icon_overlay_is_silent_when_panel_is_not_visible() {
    // Critical invariant: when the user has switched away from
    // Run-Debug, the post-draw flush MUST NOT write any wipe payload
    // (no spaces, no replacement OSC). The prior space-stamp wipe
    // fired AFTER the redraw and erased the freshly-painted next
    // sidebar in those cells. Eviction is now handled exclusively
    // by `terminal.clear()` via `consume_run_debug_image_clear`.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays
        .run_debug
        .set_image("\x1b]1337;File=...\x07".to_string());
    app.set_sidebar_view(SidebarView::RunDebug);
    app.run_debug.last_icon_cell = Some((10, 5));
    app.flush_run_debug_icon_overlay();
    assert_eq!(app.overlays.run_debug.last_emitted(), Some((10, 5)));
    // Switch away.
    app.set_sidebar_view(SidebarView::Explorer);
    // The flush in this state must clear last_emitted but emit
    // nothing (we can only assert the bookkeeping side effect here;
    // the absence of stdout writes is asserted indirectly by the
    // function returning early before the write_all call).
    app.flush_run_debug_icon_overlay();
    assert_eq!(
        app.overlays.run_debug.last_emitted(),
        None,
        "flush on a hidden panel must reset last_emitted so a re-entry primes a fresh emit",
    );
}

#[test]
fn run_debug_icon_moving_within_a_visible_panel_arms_a_one_shot_terminal_clear() {
    // Regression for the doubled/stacked headline-icon ghost: when the
    // panel stays on Run-Debug but a layout change (terminal resize OR an
    // internal pane-drag, which never fires Event::Resize) recenters the
    // icon, the next flush would emit a fresh OSC-1337 image at the new
    // cell while iTerm2 still caches the old one — two overlapping icons.
    // The flush must instead detect the move, arm exactly one
    // terminal.clear() to evict the stale cells, and skip re-emitting until
    // the clear has run (last_emitted reset to None primes a fresh emit).
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays
        .run_debug
        .set_image("\x1b]1337;File=...\x07".to_string());
    app.set_sidebar_view(SidebarView::RunDebug);
    app.run_debug.last_icon_cell = Some((10, 5));
    app.flush_run_debug_icon_overlay();
    assert_eq!(app.overlays.run_debug.last_emitted(), Some((10, 5)));
    // A resize / pane-drag recenters the icon to a new cell while the panel
    // is still the active, visible Run-Debug view.
    app.run_debug.last_icon_cell = Some((3, 2));
    app.flush_run_debug_icon_overlay();
    assert!(
        app.consume_run_debug_image_clear(),
        "an icon that moved within the visible panel must arm one terminal.clear() to evict the stale image cells",
    );
    assert!(
        !app.consume_run_debug_image_clear(),
        "the clear request must be one-shot",
    );
    assert_eq!(
        app.overlays.run_debug.last_emitted(),
        None,
        "the move branch must reset last_emitted so the post-clear frame re-emits a single clean icon",
    );
}

#[test]
fn toggle_inline_values_clears_trailers_on_disable() {
    let mut app = editor_app_with_lines(&["def f():", "    x = 1"]);
    assert!(
        app.inline_values_enabled,
        "inline values are on by default (VS Code debug.inlineValues: auto)"
    );
    // Simulate a stop having populated trailers, then disable: every editor
    // must drop them immediately, not on the next stop.
    app.editor.inline_values.insert(1, String::from("x = 1"));
    app.run_command(crate::widgets::command_palette::Command::ToggleInlineValues);
    assert!(!app.inline_values_enabled);
    assert!(
        app.editor
            .editors
            .iter()
            .all(|e| e.inline_values.is_empty()),
        "disabling clears the trailers in every editor"
    );
    app.run_command(crate::widgets::command_palette::Command::ToggleInlineValues);
    assert!(app.inline_values_enabled, "round trip");
}

#[test]
fn cmd_shift_d_jumps_to_run_and_debug_from_any_pane() {
    let mut app = editor_app_with_lines(&["x"]);
    app.focus_pane(Pane::Editor);
    app.handle_key(key(
        KeyCode::Char('d'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.sidebar_view, SidebarView::RunDebug);
}

#[test]
fn click_on_run_debug_icon_switches_sidebar_to_run_debug() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(80, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let run = app.sidebar_areas.run_debug_icon;
    assert!(run.height > 0, "run-debug icon must be laid out");
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: run.x,
        row: run.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.sidebar_view, SidebarView::RunDebug);
}

/// #864: F5 on a script ran it under croft's private debug venv, where the
/// project's packages are missing, so `import requests` stopped the run on
/// line 1. The program runs under the interpreter Run picks: the nearest
/// venv inside the workspace, else python3.
#[test]
fn a_debugged_script_runs_under_the_interpreter_run_uses() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let pkg = root.join("src").join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    let file = pkg.join("main.py");
    std::fs::write(&file, b"import requests\n").unwrap();

    let bare = super::project_python_for(&pkg, root);
    assert_eq!(
        bare.file_name().and_then(|n| n.to_str()),
        Some("python3"),
        "no venv: python3, got {}",
        bare.display()
    );
    assert!(!bare.to_string_lossy().contains("debug-venv"));

    let venv = make_venv(root, ".venv");
    assert_eq!(super::project_python_for(&pkg, root), venv);
    assert_eq!(
        super::debuggee_python(&pkg, root, std::env::var_os("PATH").as_deref()),
        Some(venv.clone()),
        "F5's debuggee is Run's interpreter"
    );
    let req =
        crate::dap::session::launch_request(&file, &super::project_python_for(&pkg, root), false);
    assert_eq!(
        req["arguments"]["python"][0],
        &*venv.to_string_lossy(),
        "the launch names the project's venv"
    );
    // The same interpreter Run spawns for this file.
    let spec = App::run_spec_for(&file, root).unwrap();
    assert_eq!(spec.program, venv.to_string_lossy());
}

/// #864 for launch.json: a Python config that names no interpreter runs
/// under the venv nearest its program (or its cwd, for a `module`), and one
/// that names its own keeps it.
#[test]
fn a_launch_json_python_config_runs_under_the_project_venv_unless_it_names_one() {
    use crate::dap::configs::{SubstCtx, debugpy_request, parse_launch_json, resolve};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let ctx = SubstCtx {
        workspace_folder: root.to_path_buf(),
        file: None,
        folders: Vec::new(),
    };
    let python_of = |json: &str| {
        let cfg = parse_launch_json(json, ".vscode/launch.json").remove(0);
        let rc = resolve(&cfg, &ctx).unwrap();
        let cwd = rc.cwd.clone().unwrap_or_else(|| root.to_path_buf());
        let adapter = Path::new("/home/u/.croft/debug-venv/bin/python");
        let req = debugpy_request(&rc, &super::config_project_python(&rc, &cwd, root, adapter));
        req["arguments"]["python"].clone()
    };
    let program = r#"[{"name":"P","type":"python","program":"${workspaceFolder}/app/main.py"}]"#;

    let fallback = python_of(program);
    let fallback = fallback[0].as_str().unwrap();
    assert!(
        fallback.ends_with("python3") && !fallback.contains("debug-venv"),
        "no venv: python3, got {fallback}"
    );

    let venv = make_venv(root, ".venv");
    assert_eq!(python_of(program)[0], &*venv.to_string_lossy());

    // A module launch has no program path: its cwd's venv is the one.
    let svc = root.join("svc");
    std::fs::create_dir_all(&svc).unwrap();
    let svc_venv = make_venv(&svc, "venv");
    assert_eq!(
        python_of(r#"[{"name":"M","type":"python","module":"svc.main","cwd":"svc"}]"#)[0],
        &*svc_venv.to_string_lossy()
    );

    assert_eq!(
        python_of(
            r#"[{"name":"E","type":"python","program":"app/main.py","python":"/opt/py/bin/python3.9"}]"#
        ),
        "/opt/py/bin/python3.9",
        "an explicit python wins"
    );
}

/// #864 review: a config naming both `module` and `program` launches the
/// module (debugpy refuses a request naming both, so the program is dropped),
/// so its interpreter comes from its `cwd`, never from beside the program
/// the request no longer carries. A program launch still uses the venv
/// beside its program.
#[test]
fn a_module_launch_that_also_names_a_program_runs_under_its_cwds_venv() {
    use crate::dap::configs::{SubstCtx, debugpy_request, parse_launch_json, resolve};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let ctx = SubstCtx {
        workspace_folder: root.to_path_buf(),
        file: None,
        folders: Vec::new(),
    };
    let request_of = |json: &str| {
        let cfg = parse_launch_json(json, ".vscode/launch.json").remove(0);
        let rc = resolve(&cfg, &ctx).unwrap();
        let cwd = rc.cwd.clone().unwrap_or_else(|| root.to_path_buf());
        let adapter = Path::new("/home/u/.croft/debug-venv/bin/python");
        debugpy_request(&rc, &super::config_project_python(&rc, &cwd, root, adapter))
    };
    let root_venv = make_venv(root, ".venv");
    let tools = root.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let tools_venv = make_venv(&tools, ".venv");

    let both = request_of(
        r#"[{"name":"T","type":"python","module":"pytest","program":"tools/helper.py"}]"#,
    );
    assert_eq!(
        both["arguments"]["module"], "pytest",
        "the module is launched"
    );
    assert!(
        both["arguments"].get("program").is_none(),
        "the program is dropped"
    );
    assert_eq!(
        both["arguments"]["python"][0],
        &*root_venv.to_string_lossy(),
        "the module runs under its cwd's venv, not the dropped program's"
    );

    let program = request_of(r#"[{"name":"P","type":"python","program":"tools/helper.py"}]"#);
    assert_eq!(
        program["arguments"]["python"][0],
        &*tools_venv.to_string_lossy(),
        "a program launch keeps the venv beside its program"
    );
}

/// #864 guard: the debuggee's interpreter is looked up no higher than the
/// workspace root, as Run's is. A venv in the folder above the workspace
/// belongs to some other project and is not picked up.
#[test]
fn a_venv_above_the_workspace_is_not_the_debuggee_interpreter() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = make_venv(tmp.path(), ".venv");
    let root = tmp.path().join("proj");
    let pkg = root.join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();

    assert_eq!(
        super::debuggee_python(&pkg, &root, None),
        None,
        "a venv above the workspace is not the project's"
    );
    let py = super::project_python_for(&pkg, &root);
    assert_ne!(py, outside, "nor Run's");
    assert!(!py.starts_with(tmp.path()), "got {}", py.display());
}

/// #864 guard: a nearer venv beats the workspace root's, for the debuggee
/// exactly as for Run, so a sub-project debugs against its own packages.
#[test]
fn a_nearer_venv_beats_the_workspace_roots_for_the_debuggee() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let root_venv = make_venv(root, ".venv");
    let svc = root.join("svc");
    std::fs::create_dir_all(&svc).unwrap();
    let svc_venv = make_venv(&svc, "venv");

    assert_eq!(
        super::debuggee_python(&svc, root, None),
        Some(svc_venv.clone())
    );
    assert_eq!(super::project_python_for(&svc, root), svc_venv);
    assert_eq!(
        super::debuggee_python(root, root, None),
        Some(root_venv.clone())
    );
    assert_eq!(super::project_python_for(root, root), root_venv);
}

/// A stand-in venv interpreter ([`make_venv`]) whose `pyvenv.cfg` says it
/// is Python `version`.
fn make_venv_of(dir: &Path, venv_name: &str, version: &str) -> PathBuf {
    let py = make_venv(dir, venv_name);
    std::fs::write(
        dir.join(venv_name).join("pyvenv.cfg"),
        format!("home = /usr/bin\ninclude-system-site-packages = false\nversion = {version}\n"),
    )
    .unwrap();
    py
}

/// #864: a debuggee interpreter older than 3.14 is named, since 3.14 is
/// attach's floor (PEP 768) and not launch's.
#[test]
fn a_debuggee_older_than_3_14_is_named_with_the_attach_floor() {
    let tmp = tempfile::tempdir().unwrap();
    let py = make_venv_of(tmp.path(), ".venv", "3.12.4");
    assert_eq!(
        super::debuggee_python_note(&py),
        Ok(Some(String::from(
            "under Python 3.12 (.venv); attach needs 3.14+"
        )))
    );
}

/// #864 guard: 3.14 and newer say nothing, and neither does an interpreter
/// whose version cannot be read (the launch then speaks for itself).
#[test]
fn a_3_14_debuggee_or_an_unreadable_one_gets_no_note() {
    let tmp = tempfile::tempdir().unwrap();
    let py314 = make_venv_of(tmp.path(), "py314", "3.14.0");
    assert_eq!(super::debuggee_python_note(&py314), Ok(None));
    let py315 = make_venv_of(tmp.path(), "py315", "3.15.1");
    assert_eq!(super::debuggee_python_note(&py315), Ok(None));
    let unknown = make_venv(tmp.path(), "unknown");
    assert_eq!(super::debuggee_python_note(&unknown), Ok(None));
}

/// #864: an interpreter below debugpy's own floor (3.10) cannot host the
/// debuggee at all, so the launch refuses it by name. 3.10 itself is fine.
#[test]
fn a_debuggee_debugpy_cannot_run_under_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let old = make_venv_of(tmp.path(), ".venv", "3.9.18");
    assert_eq!(
        super::debuggee_python_note(&old),
        Err(String::from(
            "Python 3.9 (.venv) is too old to debug: debugpy needs 3.10+"
        ))
    );
    let floor = make_venv_of(tmp.path(), "venv", "3.10.0");
    assert_eq!(
        super::debuggee_python_note(&floor),
        Ok(Some(String::from(
            "under Python 3.10 (venv); attach needs 3.14+"
        )))
    );
}

/// #1398 review: Debug Test's version check reads the interpreter a
/// debug-test config names, not the project `.venv` it replaced, and
/// checks nothing when the debug venv only stands in for a project that
/// has no interpreter of its own.
#[test]
fn a_debug_test_configs_interpreter_is_the_one_checked() {
    use crate::dap::configs::{SubstCtx, parse_launch_json, resolve, with_debug_test_config};
    let root = Path::new("/work/proj");
    let ctx = SubstCtx {
        workspace_folder: root.to_path_buf(),
        file: None,
        folders: Vec::new(),
    };
    let base = |py: &str| {
        crate::dap::session::pytest_debug_launch_request(Path::new(py), root, "test_mode")
    };
    let cfgs = parse_launch_json(
        r#"[{ "name": "T", "type": "debugpy", "purpose": ["debug-test"],
              "python": "/opt/py314/bin/python" }]"#,
        ".vscode/launch.json",
    );
    let rc = resolve(&cfgs[0], &ctx).unwrap();
    let adapter = Path::new("/debug-venv/bin/python");
    let req = with_debug_test_config(base("/work/proj/.venv/bin/python"), &rc);
    assert_eq!(
        super::debug_test_python(&req, root, adapter, true),
        Some(PathBuf::from("/opt/py314/bin/python"))
    );
    let req = with_debug_test_config(base("/debug-venv/bin/python"), &rc);
    assert_eq!(
        super::debug_test_python(&req, root, adapter, false),
        Some(PathBuf::from("/opt/py314/bin/python")),
        "a config's python is checked even when no project one was found"
    );
    assert_eq!(
        super::debug_test_python(&base("/debug-venv/bin/python"), root, adapter, false),
        None
    );
    assert_eq!(
        super::debug_test_python(&base("/work/proj/.venv/bin/python"), root, adapter, true),
        Some(PathBuf::from("/work/proj/.venv/bin/python"))
    );
}

/// #864: a launch.json config's version check reads the interpreter the
/// request actually runs: its `python` (a path or a command line) or the
/// legacy `pythonPath`, and nothing for a request that names neither.
#[test]
fn a_launch_requests_own_interpreter_is_the_one_checked() {
    use serde_json::json;
    let python = |args: serde_json::Value| {
        super::launch_request_python(&json!({ "arguments": args }), Path::new("/work/proj"))
    };
    assert_eq!(
        python(json!({ "python": "/opt/py/bin/python3.9" })),
        Some(PathBuf::from("/opt/py/bin/python3.9"))
    );
    assert_eq!(
        python(json!({ "python": ["/p/.venv/bin/python", "-X", "dev"] })),
        Some(PathBuf::from("/p/.venv/bin/python"))
    );
    assert_eq!(
        python(json!({ "pythonPath": "/legacy/python" })),
        Some(PathBuf::from("/legacy/python"))
    );
    assert_eq!(python(json!({ "program": "a.py" })), None);
}

/// #864 review: a relative interpreter in launch.json is found where
/// debugpy starts it: the request's `cwd`, else a program's folder, else
/// the folder the adapter runs in. The version check used to run it from
/// croft's own folder, which can hold a different file of that name.
#[test]
fn a_relative_launch_python_is_checked_where_debugpy_runs_it() {
    use serde_json::json;
    let python = |args: serde_json::Value| {
        super::launch_request_python(&json!({ "arguments": args }), Path::new("/work/proj"))
    };
    assert_eq!(
        python(json!({ "python": ".venv/bin/python", "cwd": "/work/proj/api" })),
        Some(PathBuf::from("/work/proj/api/.venv/bin/python")),
        "from the request's cwd"
    );
    assert_eq!(
        python(json!({
            "python": ["venv/bin/python", "-X", "dev"],
            "program": "/work/proj/tools/run.py"
        })),
        Some(PathBuf::from("/work/proj/tools/venv/bin/python")),
        "from the program's folder when the request names no cwd"
    );
    assert_eq!(
        python(json!({ "python": "./py/bin/python", "module": "pytest" })),
        Some(PathBuf::from("/work/proj/py/bin/python")),
        "from the adapter's folder for a module launch"
    );
}

/// #864 review guard: only a relative path with a folder in it moves. A
/// bare name is a PATH lookup for debugpy too, and an absolute path is
/// already where it is.
#[test]
fn a_bare_or_absolute_launch_python_is_checked_as_written() {
    use serde_json::json;
    let python = |args: serde_json::Value| {
        super::launch_request_python(&json!({ "arguments": args }), Path::new("/work/proj"))
    };
    assert_eq!(
        python(json!({ "python": "python3", "cwd": "/work/proj/api" })),
        Some(PathBuf::from("python3"))
    );
    assert_eq!(
        python(json!({ "python": "/opt/py/bin/python3.12", "cwd": "/work/proj/api" })),
        Some(PathBuf::from("/opt/py/bin/python3.12"))
    );
}

/// #864, through F5: a project venv debugpy cannot run under is refused
/// before any adapter starts, on the status line and the panel.
#[test]
fn f5_refuses_a_project_venv_too_old_for_debugpy() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    make_venv_of(&root, ".venv", "3.9.18");
    let file = root.join("main.py");
    std::fs::write(&file, "print('hi')\n").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open(&file).unwrap();
    app.debug_start_or_continue();
    let started = !app.debug_sessions.is_empty();
    if started {
        app.debug_stop();
    }
    assert_eq!(
        app.status, "Python 3.9 (.venv) is too old to debug: debugpy needs 3.10+",
        "a session started: {started}"
    );
    assert!(!started);
    assert!(app.run_debug.feedback_is_error);
}

/// #864 regression guard: with no venv and no `python3` on PATH, a bare
/// `python3` cannot start, where the launch used to run under the debug
/// venv. Nothing is found, so the launch keeps the debug venv's python; a
/// `python3` on PATH is still preferred, as for Run.
#[test]
fn a_debuggee_with_no_venv_and_no_python3_is_left_to_the_debug_venv() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    let pkg = root.join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    let empty = tmp.path().join("empty-bin");
    std::fs::create_dir_all(&empty).unwrap();
    let no_python = std::env::join_paths([&empty]).unwrap();
    assert_eq!(super::debuggee_python(&pkg, &root, Some(&no_python)), None);
    assert_eq!(super::debuggee_python(&pkg, &root, None), None, "no PATH");

    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("python3"), b"#!/bin/sh\n").unwrap();
    let with_python = std::env::join_paths([&empty, &bin]).unwrap();
    assert_eq!(
        super::debuggee_python(&pkg, &root, Some(&with_python)),
        Some(bin.join("python3"))
    );
}

/// #864 guard: the fallback never overrides a venv the project has, even
/// with nothing on PATH.
#[test]
fn a_project_venv_is_the_debuggee_whatever_path_holds() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let venv = make_venv(root, ".venv");
    let pkg = root.join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    let empty = std::env::join_paths([root.join("empty-bin")]).unwrap();
    assert_eq!(super::debuggee_python(&pkg, root, Some(&empty)), Some(venv));
}

/// #1400: F5 writes the unsaved tab before the launch, so the debuggee
/// runs the text the breakpoints were set against.
#[test]
fn f5_saves_the_unsaved_buffer_before_launching() {
    let (_tmp, mut app, file) = app_with_unsaved_pinned_file("notes.txt");
    app.debug_start_or_continue();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\nold\n");
    assert!(!app.editor.dirty);
    assert!(
        app.status.starts_with("No debugger for .txt files"),
        "the launch itself still runs and reports: {}",
        app.status
    );
}

/// #1400: every dirty tab is saved, not only the one being debugged: the
/// debuggee imports the others.
#[test]
fn f5_saves_every_unsaved_tab_not_only_the_active_one() {
    let (tmp, mut app, helper) = app_with_unsaved_pinned_file("helper.txt");
    let main = tmp.path().canonicalize().unwrap().join("main.txt");
    std::fs::write(&main, "main\n").unwrap();
    app.editor.open_pinned(&main).unwrap();
    app.debug_start_or_continue();
    assert_eq!(std::fs::read_to_string(&helper).unwrap(), "new\nold\n");
    assert_eq!(app.unsaved_count(), 0);
}

/// #1400: Debug Test saves too: the test runs from the files on disk.
#[test]
fn debug_test_saves_the_unsaved_buffer_before_launching() {
    let (_tmp, mut app, file) = app_with_unsaved_pinned_file("test_x.txt");
    app.debug_named_test(String::from("test_x"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\nold\n");
    assert_eq!(app.status, "No test runner detected in this workspace");
}

/// #1400 negative: with `debug.saveBeforeStart: "none"` the file on disk
/// is left alone, and the status says the debuggee runs the saved file.
#[test]
fn with_save_before_debug_off_f5_leaves_the_disk_and_warns() {
    let (_tmp, mut app, file) = app_with_unsaved_pinned_file("notes.txt");
    app.save_before_debug = false;
    app.debug_start_or_continue();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old\n");
    assert!(app.editor.dirty);
    assert!(
        app.status
            .starts_with("notes.txt has unsaved changes: the launch uses the file on disk"),
        "{}",
        app.status
    );
    assert!(
        app.status.contains("No debugger for .txt files"),
        "{}",
        app.status
    );
}

/// #1400 negative: a tab whose file changed on disk is never overwritten
/// blind to start a launch; it stays unsaved and the status says so.
#[test]
fn f5_never_overwrites_a_file_changed_on_disk() {
    let (_tmp, mut app, file) = app_with_unsaved_pinned_file("notes.txt");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&file, "theirs, changed elsewhere\n").unwrap();
    app.debug_start_or_continue();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "theirs, changed elsewhere\n"
    );
    assert!(app.editor.dirty);
    assert!(
        app.status.contains("notes.txt has unsaved changes"),
        "{}",
        app.status
    );
}

/// #1400 negative: with nothing unsaved, a launch writes nothing and says
/// nothing about saving.
#[test]
fn f5_with_nothing_unsaved_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let file = root.join("notes.txt");
    std::fs::write(&file, "old\n").unwrap();
    let stamp = std::fs::metadata(&file).unwrap().modified().unwrap();
    let mut app = App::new(root).unwrap();
    app.editor.open_pinned(&file).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    app.debug_start_or_continue();
    assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), stamp);
    assert!(!app.status.contains("unsaved"), "{}", app.status);
}

// #865: with croft older than its checkout, the status bar offered a
// rebuild on F9, and F9 then started `cargo install` instead of toggling a
// breakpoint - and was swallowed while the build ran, and re-exec'd croft
// once it landed. Bare F9 is Toggle Breakpoint in every one of those states.
#[test]
fn f9_toggles_a_breakpoint_whatever_the_updater_is_doing() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("app.py");
    std::fs::write(&file, "x = 1\ny = 2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();
    let marked = |app: &App| {
        app.editor
            .breakpoints
            .get(&file)
            .is_some_and(|lines| lines.contains(&1))
    };

    // A rebuild on offer.
    app.local_drift = Some(String::from("def456"));
    assert!(app.update_armed());
    let _ = app.handle_key(key(KeyCode::F(9), KeyModifiers::NONE));
    assert!(marked(&app), "F9 set a breakpoint: {}", app.status);
    assert!(app.self_install.is_none(), "and started no rebuild");

    // A rebuild running: F9 is not swallowed.
    app.self_install = Some(crate::update_watch::SelfInstall::preloaded(&[]));
    let _ = app.handle_key(key(KeyCode::F(9), KeyModifiers::NONE));
    assert!(!marked(&app), "F9 removed it again: {}", app.status);

    // The rebuild landed: F9 still does not relaunch.
    app.self_install = None;
    app.update_status = UpdateStatus::Ready;
    let _ = app.handle_key(key(KeyCode::F(9), KeyModifiers::NONE));
    assert!(marked(&app), "{}", app.status);
    assert!(
        !app.pending_reexec && !app.quit,
        "F9 never relaunches croft"
    );
}

#[test]
fn bare_f10_reaches_the_shell_when_terminal_is_focused_with_no_debug_session() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A pane that prints back the raw bytes it reads: F10 must arrive as
    // \x1b[21~, which `cat -v` renders as ^[[21~. That sentinel never appears
    // in the script text, so the echoed run-label header cannot fake a pass.
    let script = "stty raw -echo; dd bs=1 count=5 2>/dev/null | cat -v; sleep 60";
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from(script)],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    let _ = app.handle_key(key(KeyCode::F(10), KeyModifiers::NONE));
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "F10 to reach the PTY rather than being swallowed",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("^[[21~"))
        },
    );
}

#[test]
fn debug_a_cargo_test_builds_off_the_ui_thread() {
    // A cold `cargo test --no-run` takes minutes; running it on the UI
    // thread froze the whole TUI (no keys, no redraw, no PTY pumping, no
    // cancel) and the "Building…" status could never even paint before the
    // launch overwrote it. The request must return immediately and the
    // result arrive through the tick drain.
    let tmp = tempfile::tempdir().unwrap();
    // A Cargo project cargo rejects instantly (no src/), so the background
    // build fails fast and deterministically.
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.start_test_binary_build(tmp.path().to_path_buf(), String::from("my_case"), None);
    assert_eq!(
        app.status, "Building test binary for my_case",
        "the request returns with the progress status showing"
    );
    // A second request while one is pending is refused, not stacked.
    app.start_test_binary_build(tmp.path().to_path_buf(), String::from("other"), None);
    assert_eq!(app.status, "A test binary is already building");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !app.drain_pending_test_debug() {
        assert!(
            std::time::Instant::now() < deadline,
            "the build result never arrived through the drain"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        app.status.starts_with("Test build failed"),
        "a broken crate surfaces the build error, got: {}",
        app.status
    );
    assert!(
        app.pending_test_debug.is_none(),
        "the pending slot frees for the next request"
    );
}

/// When a debug session ends after breakpoints were set but none was ever hit,
/// the feedback says so explicitly rather than the opaque "Debug session ended".
#[test]
fn debug_end_message_flags_a_program_that_exited_without_hitting_a_breakpoint() {
    assert_eq!(
        debug_end_message(true, false),
        "Program exited without hitting a breakpoint"
    );
    // A breakpoint was hit (ever_stopped), or none were set: plain end message.
    assert_eq!(debug_end_message(true, true), "Debug session ended");
    assert_eq!(debug_end_message(false, false), "Debug session ended");
}

/// The debug console (program output) stays visible after the session ends, so a
/// fast-exiting program's output/errors aren't wiped before the user can read
/// them. A fresh idle panel (no ended session) shows no console.
#[test]
fn debug_console_is_retained_after_the_session_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_console = vec![
        crate::ansi_text::AnsiLine::plain("hello from program"),
        crate::ansi_text::AnsiLine::plain("all done"),
    ];
    app.run_debug.session_ended = true;
    app.refresh_debug_panel();
    assert!(
        app.run_debug
            .console_tail
            .contains(&crate::ansi_text::AnsiLine::plain("all done")),
        "the ended session's console output must stay visible"
    );

    // A fresh idle panel (never ran, or a new run reset the flag) shows nothing.
    app.run_debug.session_ended = false;
    app.refresh_debug_panel();
    assert!(
        app.run_debug.console_tail.is_empty(),
        "the idle Run panel must not show stale console output"
    );
}

#[test]
fn add_watch_via_the_palette_prompt_then_clear_all() {
    use crate::widgets::input_prompt::InputPurpose;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::DebugAddWatch);
    let prompt = app
        .input_prompt
        .as_ref()
        .expect("Debug: Add Watch Expression must open an input prompt");
    assert_eq!(prompt.purpose, InputPurpose::AddWatch);
    for c in "len(cart)".chars() {
        app.input_prompt.as_mut().unwrap().push_char(c);
    }
    app.submit_input_prompt();
    assert!(app.input_prompt.is_none(), "submitting closes the prompt");
    assert_eq!(app.watch_exprs, vec!["len(cart)"]);
    assert!(app.status.contains("Watching len(cart)"));

    // Duplicates are refused silently, whitespace trimmed.
    app.add_watch_expression(String::from("  len(cart)  "));
    assert_eq!(app.watch_exprs.len(), 1, "no duplicate watches");

    app.add_watch_expression(String::from("t"));
    app.watch_vals
        .insert(String::from("t"), (String::from("9.99"), true));
    app.remove_watch_expression(0);
    assert_eq!(
        app.watch_exprs,
        vec!["t"],
        "removal is by display index, later rows keep their values"
    );
    assert!(app.watch_vals.contains_key("t"));

    app.run_command(crate::widgets::command_palette::Command::DebugClearWatch);
    assert!(app.watch_exprs.is_empty() && app.watch_vals.is_empty());
    assert!(app.status.contains("Removed 1 watch expression"));
}

#[test]
fn lldb_program_resolves_against_the_session_cwd_not_crofts() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("proj");
    std::fs::create_dir_all(cwd.join("target/debug")).unwrap();
    std::fs::write(cwd.join("target/debug/app"), b"bin").unwrap();
    let resolved = resolve_lldb_program("target/debug/app", &cwd).expect("exists under cwd");
    assert_eq!(resolved, cwd.join("target/debug/app"));
    assert!(resolved.is_absolute());
    // A missing relative program errors with the RESOLVED path, so the
    // message says where croft actually looked.
    let err = resolve_lldb_program("target/release/app", &cwd).unwrap_err();
    assert!(
        err.contains(&cwd.join("target/release/app").display().to_string()),
        "{err}"
    );
    // Absolute programs are taken as written.
    let abs = cwd.join("target/debug/app");
    let abs_str = abs.to_string_lossy().into_owned();
    assert_eq!(
        resolve_lldb_program(&abs_str, Path::new("/somewhere/else")).unwrap(),
        abs
    );
}

#[test]
fn debug_config_picker_lists_configs_and_selection_drives_f5() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "name": "API", "type": "python", "request": "launch", "program": "api.py" },
            { "name": "Javan", "type": "java", "program": "Main.java" }
        ]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_debug_config_picker();
    let picker = app.list_picker.as_ref().expect("picker open");
    assert_eq!(
        picker.rows.len(),
        4,
        "zero-config entry + the two configs + Add Configuration"
    );
    assert!(picker.rows[0].label.contains("active file"));
    assert!(picker.rows[1].label.contains("API"));

    // Confirming a config remembers it as the F5 target. The unsupported
    // type must surface a named error, not vanish from the picker silently.
    app.list_picker.as_mut().unwrap().selected = 2;
    app.confirm_list_picker();
    assert_eq!(app.selected_debug_config.as_deref(), Some("Javan"));
    assert!(
        app.run_debug.feedback_is_error,
        "unsupported type is an error: {:?}",
        app.run_debug.feedback
    );
    assert!(
        app.status.contains("unsupported type \"java\""),
        "{}",
        app.status
    );

    // The zero-config entry clears the selection.
    app.open_debug_config_picker();
    app.list_picker.as_mut().unwrap().selected = 0;
    app.confirm_list_picker();
    assert_eq!(app.selected_debug_config, None);
}

/// #250: the Run and Debug panel's config row is the documented way to reach
/// the picker — both KEYBINDINGS.md and LAYOUT.md say so, and LAYOUT.md says
/// compounds "are listed too". But the row is gated on a count that only
/// covered `discover_configs`, so a workspace declaring ONLY compounds had no
/// row at all: `config_row_label`'s `(None, 0)` arm suppresses it, the render
/// never runs, `last_config_area` stays zero-sized, and the click guard on
/// `width > 0` refuses. The picker lists the compounds; nothing opens it.
#[test]
fn the_config_row_counts_compounds_so_a_compounds_only_workspace_still_has_one() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/launch.json"),
        r#"{"configurations":[{"name":"Server","type":"python","program":"s.py"}],
            "compounds":[{"name":"Full Stack","configurations":["Server"]}]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.refresh_run_debug();
    assert_eq!(
        app.run_debug.config_count, 2,
        "one configuration plus one compound — the row counts what the picker lists"
    );

    // The case that loses the row entirely rather than merely undercounting.
    let only = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(only.path().join(".croft")).unwrap();
    std::fs::write(
        only.path().join(".croft/launch.json"),
        r#"{"configurations":[],
            "compounds":[{"name":"Orphan","configurations":["Nothing"]}]}"#,
    )
    .unwrap();
    let mut app2 = App::new(only.path().to_path_buf()).unwrap();
    app2.refresh_run_debug();
    assert_eq!(
        app2.run_debug.config_count, 1,
        "a compounds-only workspace must still get a row, or the documented \
         route to the picker does not exist for it"
    );
    // `config_row_label` is private to the widget module; the count is the
    // input its `(None, 0)` suppression arm reads, so a non-zero count here is
    // what makes the row exist. run_debug.rs's own unit test covers the arm.
}

#[test]
fn refresh_run_debug_syncs_the_config_row() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/launch.json"),
        r#"[ { "name": "One", "type": "python", "program": "a.py" } ]"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.selected_debug_config = Some(String::from("One"));
    app.refresh_run_debug();
    assert_eq!(app.run_debug.config_count, 1);
    assert_eq!(app.run_debug.selected_config.as_deref(), Some("One"));
}

/// "N of M started" must count sessions, not call sites.
///
/// `launch_resolved_into_set` returns `()` on every validation, spawn and
/// attach failure, so the member launcher reported `Launched` unconditionally
/// and the summary counted members that never started. CI caught the extreme
/// form of it on a runner with no Python adapter: "2 session(s), showing ?"
/// over an entirely empty set.
///
/// The assertion is the INVARIANT, not a number: a session exists for each
/// one the report claims. A literal count would only restate whichever host
/// ran it - this Mac starts the good member, CI starts nothing.
#[test]
fn a_member_that_fails_to_start_is_not_counted_as_started() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    // `Broken` names an adapter croft cannot resolve, so it can never start
    // on any host; `Server` starts wherever debugpy exists.
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "name": "Server", "type": "python", "request": "launch", "program": "s.py" },
            { "name": "Broken", "type": "no-such-debugger", "request": "launch", "program": "b.py" }
        ],
        "compounds": [
            { "name": "Mixed", "configurations": ["Server", "Broken"] }
        ]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    app.open_debug_config_picker();
    let row = app
        .list_picker
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .position(|r| r.label.starts_with("Mixed"))
        .expect("the compound is listed");
    app.list_picker.as_mut().unwrap().selected = row;
    app.confirm_list_picker();

    let live = app.debug_sessions.names();
    assert!(
        !live.contains(&"Broken".to_string()),
        "the unresolvable member never joins the set: {live:?}"
    );
    let reported = app
        .run_debug
        .feedback
        .clone()
        .unwrap_or_else(|| app.status.clone());
    if live.is_empty() {
        assert!(
            reported.contains("started no sessions"),
            "an empty set is reported as empty, never as live sessions: {reported}"
        );
    } else {
        assert!(
            reported.contains(&format!("{} of 2", live.len())),
            "the count names the sessions that exist ({}), not the members attempted: {reported}",
            live.len()
        );
    }
    assert!(
        !reported.contains("showing ?"),
        "no session is described that cannot be named: {reported}"
    );
}

/// A member croft declines to start must be NAMED where the user will see it.
///
/// `launch_compound_members` builds that line into `run_debug.feedback`, and
/// it is the only place the skipped member's name appears - `status` carries
/// the compound and a count, never which member is missing. `reveal_debug_view`
/// runs `refresh_run_debug`, which clears `feedback`, so calling it AFTER the
/// assignment wiped the line before it could be drawn and the compound ran
/// fewer sessions than asked for with nothing said about it.
///
/// The assertion is on the member's NAME in the surviving feedback. Asserting
/// `feedback.is_some()` would pass on the all-started line too, which names no
/// skipped member and is exactly the case this must distinguish from.
#[test]
fn a_compound_member_skipped_for_its_own_task_is_named_in_the_feedback() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    // `Client` declares its own preLaunchTask, so croft refuses to debug it
    // against whatever the last build left behind. `Server` has none and runs.
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "name": "Server", "type": "python", "request": "launch", "program": "s.py" },
            { "name": "Client", "type": "python", "request": "launch", "program": "c.py",
              "preLaunchTask": "build-client" }
        ],
        "compounds": [
            { "name": "Pair", "configurations": ["Server", "Client"] }
        ]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    app.open_debug_config_picker();
    let row = app
        .list_picker
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .position(|r| r.label.starts_with("Pair"))
        .expect("the compound is listed");
    app.list_picker.as_mut().unwrap().selected = row;
    app.confirm_list_picker();

    // Whether `Server` can START depends on the host's adapters - CI has none
    // and this Mac does - but the SKIP is decided at parse time and happens
    // either way. So the naming is what this pins, and it reads whichever
    // field carries the report on this path.
    let reported = app
        .run_debug
        .feedback
        .clone()
        .unwrap_or_else(|| app.status.clone());
    assert!(
        reported.contains("Client"),
        "the skipped member is named: {reported}"
    );
    assert!(
        reported.contains("preLaunchTask"),
        "and why it was skipped: {reported}"
    );
    // The control for the assertions above: `Client` is never in the running
    // set, so `contains("Client")` above is not passing on a line that merely
    // lists every configuration in the file.
    assert!(
        !app.debug_sessions.names().contains(&"Client".to_string()),
        "the skipped member is not running: {:?}",
        app.debug_sessions.names()
    );
}

/// #310 defers compounds that need SEVERAL debug sessions at once. A compound
/// naming ONE configuration needs exactly one session, which croft has run
/// since #250 — so refusing it cites a limitation that does not apply and
/// tells the user something false about their own launch.json.
///
/// `parse_compounds` rejects only an EMPTY member list, so a one-member
/// compound parses and reaches the launch site, where the `Ok(_)` arm reports
/// the #310 message without ever consulting `configurations.len()`. This is a
/// relocated-decision leftover: the member list is resolved, then the arm that
/// consumes it answers a question about session count without reading it.
#[test]
fn a_single_member_compound_launches_rather_than_citing_the_multi_session_limit() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "name": "Server", "type": "python", "request": "launch", "program": "s.py" },
            { "name": "Client", "type": "python", "request": "launch", "program": "c.py" }
        ],
        "compounds": [
            { "name": "Just Server", "configurations": ["Server"] },
            { "name": "Both", "configurations": ["Server", "Client"] },
            { "name": "Tasked", "configurations": ["Server"], "preLaunchTask": "build" }
        ]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    app.open_debug_config_picker();
    let idx = app
        .list_picker
        .as_ref()
        .expect("picker open")
        .rows
        .iter()
        .position(|r| r.label.starts_with("Just Server"))
        .expect("the one-member compound is listed");
    app.list_picker.as_mut().unwrap().selected = idx;
    app.confirm_list_picker();

    assert!(
        !app.status.contains("#310"),
        "a one-member compound needs ONE session, so the multi-session \
         deferral does not apply to it: {}",
        app.status
    );
    // #567: it selects the COMPOUND, not its member. Selecting the member
    // made F5 relaunch the bare configuration, skipping anything the compound
    // itself declares (its preLaunchTask).
    assert_eq!(app.selected_debug_compound.as_deref(), Some("Just Server"));
    assert_eq!(app.selected_debug_config, None);
    // The selection is assigned BEFORE the launch, which has several
    // early-return error paths — so the assertion above proves only that the
    // launch BRANCH was reached. Assert the launch was not refused.
    // The launch reached a real adapter, so what it does next depends on the
    // machine: the GitHub runner has no `uv`, and "Debugger setup failed:
    // running `uv venv`" is a true report about that box, not this fix
    // regressing. Assert what the fix actually claims - that the refusal is
    // not the #310 deferral - rather than that nothing went wrong at all,
    // which made the test a statement about the environment.
    if let Some(feedback) = app.run_debug.feedback.as_deref() {
        assert!(
            !feedback.contains("#310") && !feedback.contains("several debug sessions"),
            "a one-member compound must not be refused for needing several \
             sessions; any other environment-dependent failure is not this \
             test's business: {feedback}"
        );
    }

    // The guard that used to hold this line: a multi-member compound was
    // DEFERRED and named #310. It launches now, which is what #310 built, so
    // the guard becomes the positive one - the same picker row starts every
    // member rather than one of them.
    app.open_debug_config_picker();
    let multi = app
        .list_picker
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .position(|r| r.label.starts_with("Both"))
        .expect("the two-member compound is listed");
    app.list_picker.as_mut().unwrap().selected = multi;
    app.confirm_list_picker();
    // Asserting the ABSENCE of the old message proves nothing - any refusal
    // omits it, and so does an empty status. Pin the OUTCOME: both members
    // are running, in file order, with the first focused.
    //
    // Not a disjunction over "started no sessions". That arm is satisfied by
    // the zero-session early return, so it passed without ever constraining
    // the count - and the premise behind it ("no adapter is installed, so
    // every member fails to start") is simply false here: the compound really
    // does launch two sessions on this host.
    // The REPORT must agree with the SET, whatever the host can start. CI has
    // no Python adapter and starts zero; this Mac has debugpy and starts two.
    // Pinning either number makes the test a statement about the runner, so
    // pin the invariant that was actually broken: the count came from the
    // number of members attempted rather than from the sessions that exist,
    // and said "2 session(s), showing ?" over an empty set.
    let live = app.debug_sessions.names();
    if live.is_empty() {
        assert!(
            app.status.contains("started no sessions"),
            "no adapter: the compound says so rather than claiming sessions: {}",
            app.status
        );
    } else {
        assert_eq!(
            live,
            vec!["Server", "Client"],
            "with an adapter, both members run in launch.json order: {}",
            app.status
        );
        assert_eq!(
            app.debug_sessions.focused_name(),
            Some("Server"),
            "the FIRST member takes focus"
        );
        assert!(
            app.status.contains(&format!("{} session(s)", live.len())),
            "the count names the sessions that exist: {}",
            app.status
        );
        assert!(
            !app.status.contains("showing ?"),
            "and the focused session is identified: {}",
            app.status
        );
    }
    assert!(
        !app.status.contains("#310"),
        "and no longer defers to the session limit: {}",
        app.status
    );

    // #318: a compound's own `preLaunchTask` is now RUN rather than refused.
    // This workspace declares no tasks at all, so the compound naming one is
    // an error that says which task is missing — the same contract a config's
    // own task gets, because the user cannot tell the two apart from outside.
    //
    // On a FRESH app: the launches above leave a live session, and
    // `debug_error` does not clear one, so asserting `is_none()` on this app
    // would be answered by the earlier launch rather than by this path.
    let mut fresh = App::new(tmp.path().to_path_buf()).unwrap();
    fresh.open_debug_config_picker();
    let tasked = fresh
        .list_picker
        .as_ref()
        .expect("picker open")
        .rows
        .iter()
        .position(|r| r.label.starts_with("Tasked"))
        .expect("the task-carrying compound is listed");
    fresh.list_picker.as_mut().unwrap().selected = tasked;
    fresh.confirm_list_picker();
    assert!(
        fresh.status.contains("\"build\" not found"),
        "the error names the task the file asked for, so the user can declare \
         it rather than guessing: {}",
        fresh.status
    );
    assert!(
        !fresh.status.contains("#318"),
        "and it no longer cites the unbuilt-feature issue: croft honours this \
         key now, so naming #318 would report a limitation it does not have: {}",
        fresh.status
    );
    assert!(
        fresh.pending_debug_launch.is_none(),
        "nothing parked behind a task that does not exist"
    );
    assert!(
        fresh.debug_sessions.is_empty(),
        "and nothing launched — starting the member anyway would debug stale \
         artifacts, which is what running the task exists to prevent"
    );
}

/// #318: a compound's own `preLaunchTask` runs before its member starts, and
/// the launch waits for it — the same contract a configuration's own task has.
///
/// The distinguishing assertion is the PARK. A compound that simply launched
/// its member would leave `pending_debug_launch` empty and a session starting;
/// one that refused would report an error and do nothing. Parked-with-no-error
/// is the state that only "the task is running, the debugger is waiting for
/// it" produces.
#[test]
fn a_compounds_own_pre_launch_task_runs_before_its_member_starts() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/tasks.json"),
        r#"{ "version": "2.0.0", "tasks": [
            { "label": "build", "type": "shell", "command": "true" }
        ]}"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "name": "Server", "type": "python", "request": "launch", "program": "s.py" },
            { "name": "Client", "type": "python", "request": "launch", "program": "c.py" }
        ],
        "compounds": [
            { "name": "Tasked", "configurations": ["Server"], "preLaunchTask": "build" },
            { "name": "Presented", "configurations": ["Server"],
              "presentation": { "hidden": "true" } },
            { "name": "PresentedPair", "configurations": ["Server", "Client"],
              "presentation": { "hidden": "true" } }
        ]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    let select = |app: &mut App, label: &str| {
        app.open_debug_config_picker();
        let idx = app
            .list_picker
            .as_ref()
            .expect("picker open")
            .rows
            .iter()
            .position(|r| r.label.starts_with(label))
            .unwrap_or_else(|| panic!("{label} is listed"));
        app.list_picker.as_mut().unwrap().selected = idx;
        app.confirm_list_picker();
    };

    select(&mut app, "Tasked");
    assert!(
        app.pending_debug_launch.is_some(),
        "the launch waits behind the compound's task rather than starting \
         alongside it: status was {:?}",
        app.status
    );
    assert!(
        !app.run_debug.feedback_is_error,
        "and this is progress, not a refusal: {:?}",
        app.run_debug.feedback
    );
    assert!(
        app.status.contains("build") && app.status.contains("exits 0"),
        "the status names the task and the condition the launch waits on: {}",
        app.status
    );
    assert!(
        app.debug_sessions.is_empty(),
        "nothing has been debugged yet — that is the whole point of the wait"
    );

    // The guard that must stay GREEN: honouring one key must not be
    // satisfiable by launching everything. Since #318 finished, `hidden`,
    // `group` and `order` are all honoured, so the only shapes left to carry
    // this guard are the MALFORMED ones. `{"hidden": "true"}` is the likely
    // typo: a quoted bool croft cannot read, which must refuse rather than
    // launch unhidden in silence.
    let mut fresh = App::new(tmp.path().to_path_buf()).unwrap();
    select(&mut fresh, "Presented");
    assert!(
        fresh.run_debug.feedback_is_error,
        "presentation is still not honoured, so it still refuses: {:?}",
        fresh.run_debug.feedback
    );
    // The message names the SUB-KEY, not `presentation` (#318). That is the
    // point of the split: a compound may declare a working `group` beside a
    // malformed `hidden`, so telling the user that "presentation" is
    // unsupported would send them deleting a line that works.
    assert!(
        fresh.status.contains("presentation.hidden"),
        "and names the key it cannot read: {}",
        fresh.status
    );
    // The BARE parent would overstate it, for the same reason. The qualified
    // path is what they can find in the file, so the check is that
    // `presentation` never appears WITHOUT its sub-key.
    assert!(
        !fresh
            .status
            .replace("presentation.hidden", "")
            .contains("presentation"),
        "the parent is named only as part of the qualified path: {}",
        fresh.status
    );
    assert!(
        !fresh.status.contains("preLaunchTask"),
        "without naming the one it now can — reporting a limitation croft no \
         longer has is the false claim this whole path exists to avoid: {}",
        fresh.status
    );
    assert!(
        fresh.pending_debug_launch.is_none() && fresh.debug_sessions.is_empty(),
        "and nothing ran"
    );

    // The same refusal on a MULTI-member compound must not tell the user to
    // run one member directly. The guard covers both arities now, and naming
    // only the first is advice to launch a SUBSET - the very outcome this arm
    // refuses to produce itself. Both names, or the remedy is worse than the
    // refusal.
    let mut pair = App::new(tmp.path().to_path_buf()).unwrap();
    select(&mut pair, "PresentedPair");
    assert!(
        pair.run_debug.feedback_is_error,
        "a malformed key still refuses at two members: {:?}",
        pair.run_debug.feedback
    );
    assert!(
        pair.status.contains("Server") && pair.status.contains("Client"),
        "and the remedy names EVERY member, not just the first: {}",
        pair.status
    );
    assert!(pair.debug_sessions.is_empty(), "and still nothing ran");
}

/// #318, the half a parked-launch assertion cannot see: what happens when the
/// compound's task FINISHES.
///
/// A compound's member is resumed through the ordinary launch path, so a
/// member carrying its own `preLaunchTask` parks again rather than starting.
/// That chaining is what makes croft run both tasks, as VS Code does, and
/// nothing about the initial park proves it happens.
#[test]
fn a_finished_compound_task_starts_its_member_and_chains_the_members_own_task() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/tasks.json"),
        r#"{ "version": "2.0.0", "tasks": [
            { "label": "compound-build", "type": "shell", "command": "true 1" },
            { "label": "member-build", "type": "shell", "command": "true 2" }
        ]}"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "name": "Server", "type": "python", "request": "launch",
              "program": "s.py", "preLaunchTask": "member-build" }
        ],
        "compounds": [
            { "name": "Tasked", "configurations": ["Server"], "preLaunchTask": "compound-build" }
        ]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    app.open_debug_config_picker();
    let idx = app
        .list_picker
        .as_ref()
        .expect("picker open")
        .rows
        .iter()
        .position(|r| r.label.starts_with("Tasked"))
        .expect("the compound is listed");
    app.list_picker.as_mut().unwrap().selected = idx;
    app.confirm_list_picker();
    let parked = app
        .pending_debug_launch
        .take()
        .expect("the compound's task parked the launch");
    assert!(
        app.status.contains("compound-build"),
        "staging: it is the COMPOUND's task that ran first: {}",
        app.status
    );

    // The compound's task exits 0.
    app.resume_pending_launch(Some(0), parked.step);

    // The member does not go straight to the adapter: it declares a task of
    // its own, so it parks in turn. Both tasks run, which is the behaviour
    // being chained rather than merged buys.
    let second = app
        .pending_debug_launch
        .as_ref()
        .expect("the member's own preLaunchTask parks in turn rather than being skipped");
    assert!(
        app.status.contains("member-build"),
        "and it is the MEMBER's task waiting now: {}",
        app.status
    );
    assert!(
        !app.run_debug.feedback_is_error,
        "still progress, not a refusal: {:?}",
        app.run_debug.feedback
    );
    assert!(
        matches!(second.step, crate::app::PendingLaunchStep::Resolved(_)),
        "the second park is the member's own launch, so the chain ends here \
         rather than looping"
    );

    // A task that fails aborts instead of debugging what the build left.
    let second = app.pending_debug_launch.take().expect("still parked");
    app.resume_pending_launch(Some(1), second.step);
    assert!(
        app.run_debug.feedback_is_error && app.status.contains("exited 1"),
        "a non-zero task aborts the launch and says so: {}",
        app.status
    );
    assert!(app.debug_sessions.is_empty(), "and nothing was debugged");
}

/// #373: "Debug This Test" breaks where the test last failed, without the
/// user setting a breakpoint — and takes that breakpoint back out when the
/// session ends, so it never outlives the run that asked for it.
#[test]
fn debugging_a_test_breaks_at_its_last_failure_and_cleans_up_after() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    let src = tmp.path().join("src/lib.rs");
    std::fs::write(&src, "fn a() {}\nfn b() {}\nassert!(false);\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // The runner's output for a failing test, as it reaches the channel.
    crate::output::clear(crate::output::CHANNEL_TESTS);
    for line in [
        "---- tests::adds stdout ----",
        "thread 'tests::adds' panicked at src/lib.rs:3:5:",
        "assertion failed: false",
    ] {
        crate::output::push(
            crate::output::CHANNEL_TESTS,
            crate::output::OutputLevel::Info,
            line,
        );
    }

    let site = app
        .failure_site_of("tests::adds")
        .expect("the panic line names a place in the user's own code");
    assert_eq!(site.line, 3);
    assert_eq!(site.resolve(tmp.path()), Some((src.clone(), 3)));

    // Arming sets the breakpoint and says where.
    assert!(app.editor.breakpoints.is_empty());
    app.arm_failure_breakpoint("tests::adds", tmp.path());
    assert_eq!(
        app.editor.breakpoints.get(&src).map(|l| l.contains(&3)),
        Some(true),
        "the assertion's line carries a breakpoint the user never set"
    );
    // The note rides on whichever launch message the user actually sees:
    // setting it as the status here would be overwritten moments later.
    assert!(
        app.with_failure_note(String::from("Building"))
            .contains("breaking at lib.rs:3 where it last failed"),
        "{}",
        app.with_failure_note(String::from("Building"))
    );

    // Disarming takes back exactly that one, leaving no empty entry, and
    // drops the note with it.
    app.disarm_failure_breakpoint();
    assert_eq!(app.with_failure_note(String::from("Building")), "Building");
    assert!(
        app.editor.breakpoints.is_empty(),
        "the temporary breakpoint does not outlive its session"
    );

    // A breakpoint the USER set at the same line is theirs: arming must not
    // adopt it, and disarming must not delete their work.
    app.editor
        .breakpoints
        .entry(src.clone())
        .or_default()
        .insert(3);
    app.arm_failure_breakpoint("tests::adds", tmp.path());
    app.disarm_failure_breakpoint();
    assert_eq!(
        app.editor.breakpoints.get(&src).map(|l| l.contains(&3)),
        Some(true),
        "the user's own breakpoint survives a debug session that shared its line"
    );
    app.editor.breakpoints.clear();

    // A failure in a dependency names nothing actionable, so nothing is set.
    crate::output::clear(crate::output::CHANNEL_TESTS);
    for line in [
        "---- tests::deps stdout ----",
        "thread 'tests::deps' panicked at /Users/me/.cargo/registry/src/x-1.0/lib.rs:9:1:",
    ] {
        crate::output::push(
            crate::output::CHANNEL_TESTS,
            crate::output::OutputLevel::Info,
            line,
        );
    }
    assert!(app.failure_site_of("tests::deps").is_none());
    app.arm_failure_breakpoint("tests::deps", tmp.path());
    assert!(
        app.editor.breakpoints.is_empty(),
        "a location inside a dependency is not offered as a breakpoint"
    );

    // The palette carries the command that reaches all of this.
    assert!(
        crate::widgets::command_palette::ALL_COMMANDS
            .contains(&crate::widgets::command_palette::Command::DebugTestAtCursor)
    );
}

/// The config row and the picker must agree about hidden compounds (#318).
///
/// The row is the documented route INTO the picker, so a count that
/// includes what the picker drops advertises an entry that is not there:
/// a workspace whose only compound is hidden would render "1 debug entry"
/// over a picker offering nothing to debug.
#[test]
fn the_config_row_does_not_count_a_hidden_compound() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/launch.json"),
        r#"{
          "configurations": [
            { "name": "A", "type": "lldb", "request": "launch", "program": "/bin/ls" }
          ],
          "compounds": [
            { "name": "OnlyHidden", "configurations": ["A"],
              "presentation": { "hidden": true } }
          ]
        }"#,
    )
    .unwrap();

    // `A` is declared, so the compound RESOLVES and the only thing keeping it
    // out of the count is `hidden`. With an empty `configurations` the count
    // could equally come from an unresolvable member, and the assertion would
    // hold for a reason it does not claim.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.refresh_run_debug();
    // Pin the fixture's discriminating property rather than leaving it in a
    // comment: the counts below only mean "the compound was excluded" while
    // exactly one configuration is declared. A later edit adding a second
    // config would make 1 and 2 wrong for a reason that has nothing to do
    // with hiding, and the obvious repair -- bumping the numbers -- would
    // leave a test that no longer measures what it claims.
    assert_eq!(
        crate::dap::configs::discover_configs(tmp.path()).len(),
        1,
        "fixture: exactly one configuration, so the counts isolate the compound"
    );
    assert_eq!(
        app.run_debug.config_count, 1,
        "the visible configuration counts; the hidden compound does not"
    );

    // And the picker agrees: nothing but the always-present rows (the active
    // file, and Add Configuration).
    app.open_debug_config_picker();
    let picker = app.list_picker.as_ref().expect("the picker opened");
    assert!(
        !picker.rows.iter().any(|r| r.id.starts_with("compound:")),
        "no compound rows: {:?}",
        picker.rows.iter().map(|r| &r.id).collect::<Vec<_>>()
    );
    app.list_picker = None;

    // The POSITIVE leg. Everything above asserts an absence, and an absence
    // is what a broken parser, an unreadable launch.json and a working
    // filter all produce. Unhiding the same compound in the same workspace
    // must make it appear -- without this the test passes with the filter
    // deleted and the file never parsed.
    std::fs::write(
        tmp.path().join(".croft/launch.json"),
        r#"{
          "configurations": [
            { "name": "A", "type": "lldb", "request": "launch", "program": "/bin/ls" }
          ],
          "compounds": [
            { "name": "OnlyHidden", "configurations": ["A"],
              "presentation": { "hidden": false } }
          ]
        }"#,
    )
    .unwrap();
    let mut shown = App::new(tmp.path().to_path_buf()).unwrap();
    shown.refresh_run_debug();
    assert_eq!(
        shown.run_debug.config_count, 2,
        "the same compound, unhidden, IS an entry the row offers alongside A"
    );
    shown.open_debug_config_picker();
    let picker = shown.list_picker.as_ref().expect("the picker opened");
    assert!(
        picker.rows.iter().any(|r| r.id.starts_with("compound:")),
        "and the picker lists it: {:?}",
        picker.rows.iter().map(|r| &r.id).collect::<Vec<_>>()
    );
}

/// #318: a CONFIGURATION can ask to be hidden too, and the picker and its
/// badge must agree about that the way they already do for a compound.
/// Deleting the configuration half of the filter left the whole suite green.
#[test]
fn a_hidden_configuration_leaves_both_the_picker_and_the_count() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/launch.json"),
        r#"{
          "configurations": [
            { "name": "Shown", "type": "lldb", "request": "launch", "program": "/bin/ls" },
            { "name": "Hid", "type": "lldb", "request": "launch", "program": "/bin/ls",
              "presentation": { "hidden": true } }
          ]
        }"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.refresh_run_debug();
    assert_eq!(
        crate::dap::configs::discover_configs(tmp.path()).len(),
        2,
        "fixture: both configurations parse, so the count below isolates hiding"
    );
    assert_eq!(
        app.run_debug.config_count, 1,
        "the hidden configuration is not an entry the row offers"
    );
    app.open_debug_config_picker();
    let labels: Vec<String> = app
        .list_picker
        .as_ref()
        .expect("the picker opened")
        .rows
        .iter()
        .map(|r| r.label.clone())
        .collect();
    assert!(
        labels.iter().any(|l| l.starts_with("Shown")),
        "the visible one is listed: {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.starts_with("Hid")),
        "and the hidden one is not: {labels:?}"
    );
}

/// #318: the row ids are built from the UNSORTED lists, so a row keeps naming
/// the entry it says even after `presentation` reorders the picker. Indexing
/// the sorted sequence instead would launch a different configuration than
/// the row names, which is the trap the module doc calls out and which no
/// test covered while only the hidden filter was exercised.
#[test]
fn a_reordering_presentation_leaves_every_row_naming_its_own_entry() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    // "Last" is declared third and asks to be first, so a sorted-index bug
    // cannot hide behind the entries happening to already be in order.
    std::fs::write(
        tmp.path().join(".croft/launch.json"),
        r#"{
          "configurations": [
            { "name": "First", "type": "lldb", "request": "launch", "program": "/bin/ls" },
            { "name": "Second", "type": "lldb", "request": "launch", "program": "/bin/ls" },
            { "name": "Last", "type": "lldb", "request": "launch", "program": "/bin/ls",
              "presentation": { "order": 1 } }
          ]
        }"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_debug_config_picker();
    let rows: Vec<(String, String)> = app
        .list_picker
        .as_ref()
        .expect("the picker opened")
        .rows
        .iter()
        .map(|r| (r.id.clone(), r.label.clone()))
        .collect();
    let configs = crate::dap::configs::discover_configs(tmp.path());
    let ordered: Vec<&str> = rows
        .iter()
        .filter(|(id, _)| id != "active" && id != "add")
        .map(|(_, label)| label.split(' ').next().unwrap_or_default())
        .collect();
    assert_eq!(
        ordered,
        vec!["Last", "First", "Second"],
        "precondition: the presentation actually moved a row"
    );
    // The claim: every id still indexes the entry whose name the row shows.
    for (id, label) in rows.iter().filter(|(id, _)| id != "active" && id != "add") {
        let idx: usize = id.parse().expect("a configuration row's id is its index");
        assert!(
            label.starts_with(&configs[idx].name),
            "row {id:?} says {label:?} but index {idx} is {:?}",
            configs[idx].name
        );
    }
}

/// A compound asking `presentation.hidden` is not listed, and hiding one
/// does not misdirect the rows that remain (#318).
///
/// The second half is the subtle one: row ids are `compound:{i}` where `i`
/// indexes `debug_compounds`. Filtering AFTER `enumerate` keeps those ids
/// naming the same compound; filtering before would make every row after a
/// hidden one launch its neighbour.
#[test]
fn a_hidden_compound_is_not_listed_and_the_rest_keep_their_ids() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/launch.json"),
        r#"{
          "configurations": [
            { "name": "A", "type": "lldb", "request": "launch", "program": "/bin/ls" }
          ],
          "compounds": [
            { "name": "First",  "configurations": ["A"] },
            { "name": "Hidden", "configurations": ["A"], "presentation": { "hidden": true } },
            { "name": "Third",  "configurations": ["A"] }
          ]
        }"#,
    )
    .unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_debug_config_picker();
    let picker = app.list_picker.as_ref().expect("the picker opened");
    let rows: Vec<(String, String)> = picker
        .rows
        .iter()
        .filter(|r| r.id.starts_with("compound:"))
        .map(|r| (r.id.clone(), r.label.clone()))
        .collect();

    assert_eq!(rows.len(), 2, "the hidden compound is not listed: {rows:?}");
    assert!(
        !rows.iter().any(|(_, l)| l.contains("Hidden")),
        "and it is the hidden one that went: {rows:?}"
    );

    // Third was at index 2 before the filter and must still say so, or
    // selecting it would launch the compound the id points at instead.
    assert_eq!(rows[0].0, "compound:0", "First keeps its index");
    assert!(rows[0].1.contains("First"));
    assert_eq!(
        rows[1].0, "compound:2",
        "Third keeps index 2 across the hidden row, not 1"
    );
    assert!(rows[1].1.contains("Third"));
}

/// Pick the row of the open list picker whose id is `id`.
fn pick_row_id(app: &mut App, id: &str) {
    let picker = app.list_picker.as_mut().expect("a picker is open");
    picker.selected = picker
        .rows
        .iter()
        .position(|r| r.id == id)
        .unwrap_or_else(|| panic!("no row {id}"));
    app.confirm_list_picker();
}

/// Type `value` over whatever the open prompt holds and press Enter.
fn answer_prompt(app: &mut App, value: &str) {
    app.input_prompt.as_mut().expect("a prompt is open").value = value.to_string();
    app.submit_input_prompt();
}

/// #250: Debug: Add Configuration… walks the fields a launch needs, refuses
/// a value that does not fit without losing the rest, and writes the entry
/// to `.croft/launch.json`, selected for F5 and listed in the picker.
#[test]
fn add_debug_configuration_writes_a_launch_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Reachable from the picker, not only the palette.
    app.open_debug_config_picker();
    pick_row_id(&mut app, "add");
    pick_row_id(&mut app, "python");
    pick_row_id(&mut app, "launch");
    let prompt = app.input_prompt.as_ref().expect("the name is asked");
    assert_eq!(prompt.value, "Launch Python", "seeded with a name");
    answer_prompt(&mut app, "Serve");
    assert_eq!(app.input_prompt.as_ref().unwrap().value, "${file}");
    answer_prompt(&mut app, "${workspaceFolder}/api.py");
    answer_prompt(&mut app, "--port \"80 00");
    assert!(app.status.contains("quote"), "{}", app.status);
    assert_eq!(
        app.input_prompt.as_ref().unwrap().value,
        "--port \"80 00",
        "the refused value is offered back to fix"
    );
    answer_prompt(&mut app, "--port 8000");
    // Optional fields take a blank Enter.
    answer_prompt(&mut app, "");
    answer_prompt(&mut app, "DEBUG=1");
    answer_prompt(&mut app, "");
    assert!(app.input_prompt.is_none(), "the draft is complete");
    assert!(app.status.contains("Added \"Serve\""), "{}", app.status);
    assert_eq!(app.selected_debug_config.as_deref(), Some("Serve"));
    let cfgs = crate::dap::configs::discover_configs(tmp.path());
    assert_eq!(cfgs.len(), 1);
    assert_eq!(cfgs[0].source, ".croft/launch.json");
    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".croft/launch.json")).unwrap(),
    )
    .unwrap();
    let entry = &written["configurations"][0];
    assert_eq!(entry["program"], "${workspaceFolder}/api.py");
    assert_eq!(entry["args"], serde_json::json!(["--port", "8000"]));
    assert_eq!(entry["env"]["DEBUG"], "1");
    assert!(entry.get("cwd").is_none() && entry.get("preLaunchTask").is_none());
    app.open_debug_config_picker();
    assert!(
        app.list_picker
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .any(|r| r.label.starts_with("Serve")),
        "and the picker lists it"
    );
}

/// #250: an attach asks only where to attach, and the palette starts it.
#[test]
fn add_debug_configuration_attaches_by_port() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::AddDebugConfig);
    pick_row_id(&mut app, "node");
    pick_row_id(&mut app, "attach");
    answer_prompt(&mut app, "Web");
    answer_prompt(&mut app, "nope");
    assert!(app.status.contains("Not a port"), "{}", app.status);
    answer_prompt(&mut app, "9229");
    assert!(app.input_prompt.is_none());
    let cfgs = crate::dap::configs::discover_configs(tmp.path());
    assert_eq!(
        (cfgs.len(), cfgs[0].request),
        (1, crate::dap::configs::RequestKind::Attach)
    );
}

/// #1425: in a multi-root workspace a compound member from another folder
/// resolves in that folder: its preLaunchTask is found in web's tasks.json
/// and its program is web's. Before, the compound failed with "which no
/// launch.json declares".
#[test]
fn a_compound_member_from_another_folder_launches_from_that_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let api = tmp.path().join("api");
    let web = tmp.path().join("web");
    for d in [&api, &web] {
        std::fs::create_dir_all(d.join(".vscode")).unwrap();
    }
    std::fs::write(
        api.join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "name": "Run api", "type": "python", "request": "launch", "program": "${workspaceFolder}/main.py" }
        ],
        "compounds": [
            { "name": "Web only", "configurations": [{ "folder": "web", "name": "Run web" }] }
        ]}"#,
    )
    .unwrap();
    std::fs::write(
        web.join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "name": "Run web", "type": "python", "request": "launch", "program": "${workspaceFolder}/main.py", "preLaunchTask": "serve" }
        ]}"#,
    )
    .unwrap();
    std::fs::write(
        web.join(".vscode/tasks.json"),
        r#"{ "version": "2.0.0", "tasks": [
            { "label": "serve", "type": "shell", "command": "sleep 30" }
        ]}"#,
    )
    .unwrap();
    let mut app = App::new(api.clone()).unwrap();
    app.roots.add(web.clone());
    pick_debug_row(&mut app, "Web only");
    assert!(
        !app.status.contains("no launch.json declares"),
        "status: {}",
        app.status
    );
    let pending = app
        .pending_debug_launch
        .as_ref()
        .expect("web's preLaunchTask parks the launch");
    let PendingLaunchStep::Resolved(rc) = &pending.step else {
        panic!("the member itself is parked behind its task");
    };
    assert_eq!(rc.name, "Run web");
    assert_eq!(
        rc.program.as_deref(),
        Some(web.join("main.py").to_string_lossy().as_ref())
    );
}

/// Open the debug picker and confirm the row whose label starts with `label`.
fn pick_debug_row(app: &mut App, label: &str) {
    app.open_debug_config_picker();
    let idx = app
        .list_picker
        .as_ref()
        .expect("picker open")
        .rows
        .iter()
        .position(|r| r.label.starts_with(label))
        .unwrap_or_else(|| panic!("{label} is listed"));
    app.list_picker.as_mut().unwrap().selected = idx;
    app.confirm_list_picker();
}

/// A workspace with two configs and compounds with and without a task. The
/// task never finishes on its own, so a parked launch stays parked.
fn compound_workspace() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/tasks.json"),
        r#"{ "version": "2.0.0", "tasks": [
            { "label": "build", "type": "shell", "command": "true" }
        ]}"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "name": "Server", "type": "python", "request": "launch", "program": "s.py" },
            { "name": "Client", "type": "python", "request": "launch", "program": "c.py" }
        ],
        "compounds": [
            { "name": "Tasked", "configurations": ["Server"], "preLaunchTask": "build" },
            { "name": "Both", "configurations": ["Server", "Client"] }
        ]}"#,
    )
    .unwrap();
    tmp
}

#[test]
fn restart_relaunches_the_selected_compound_not_a_stale_config() {
    // #567 item 6: a compound launch never became the F5 target, so restart
    // (and F5 after the sessions end) ran whatever was selected before.
    let tmp = compound_workspace();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.selected_debug_config = Some(String::from("Client"));

    pick_debug_row(&mut app, "Tasked");
    assert_eq!(app.selected_debug_compound.as_deref(), Some("Tasked"));
    assert_eq!(
        app.selected_debug_config, None,
        "the compound replaces the old target"
    );
    app.refresh_run_debug();
    assert_eq!(app.run_debug.selected_config.as_deref(), Some("Tasked"));

    // Restart goes back through the compound, so its task parks the launch
    // again. Launching "Client" (the stale target) or bare "Server" (its
    // member) would clear the parking instead.
    app.pending_debug_launch = None;
    app.debug_restart();
    assert!(
        app.pending_debug_launch.is_some() && app.status.contains("compound preLaunchTask"),
        "restart re-runs the compound behind its own task: {}",
        app.status
    );

    // Choosing a configuration afterwards drops the compound selection.
    pick_debug_row(&mut app, "Server");
    assert_eq!(app.selected_debug_compound, None);
    assert_eq!(app.selected_debug_config.as_deref(), Some("Server"));
}

#[test]
fn a_direct_compound_launch_drops_an_older_parked_task() {
    // #567 item 5: the parked launch outlived a newer compound launch, and
    // its task's exit would later start its config and stop the compound.
    let tmp = compound_workspace();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    pick_debug_row(&mut app, "Tasked");
    assert!(
        app.pending_debug_launch.is_some(),
        "the task-gated launch is parked"
    );

    pick_debug_row(&mut app, "Both");
    assert!(
        app.pending_debug_launch.is_none(),
        "the newer compound launch supersedes the parked one"
    );
}

#[test]
fn a_zero_config_launch_drops_an_older_parked_task() {
    let tmp = compound_workspace();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    pick_debug_row(&mut app, "Tasked");
    assert!(app.pending_debug_launch.is_some());

    pick_debug_row(&mut app, "Debug active file");
    assert!(app.pending_debug_launch.is_none());
}

/// A stand-in adapter that sends each JSON DAP message in `messages`, framed
/// with its byte length, and then idles.
fn stub_emitting(messages: &[&str]) -> crate::dap::session::DapSession {
    let mut script = String::new();
    for m in messages {
        script.push_str(&format!(
            "printf 'Content-Length: %d\\r\\n\\r\\n%s' {} '{}'; ",
            m.len(),
            m
        ));
    }
    script.push_str("sleep 30");
    crate::dap::session::DapSession::launch_with(
        "sh",
        &[String::from("-c"), script],
        std::path::Path::new("."),
        serde_json::json!({"seq": 2, "type": "request", "command": "launch", "arguments": {}}),
        std::collections::BTreeMap::new(),
    )
    .expect("sh spawns")
}

/// Poll until `done` holds or two seconds pass.
fn poll_until(app: &mut App, done: impl Fn(&App) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !done(app) && std::time::Instant::now() < deadline {
        app.poll_dap();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// #1426: Alt+F9 sends js-debug's `all` filter and says it is on; with an
/// adapter that offers no raised-exceptions filter it says so instead of
/// claiming exceptions will break.
#[test]
fn the_raised_exceptions_toggle_follows_the_adapters_filters() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push(
        "Node",
        stub_emitting(&[
            r#"{"seq":1,"type":"response","request_seq":1,"success":true,"command":"initialize","body":{"exceptionBreakpointFilters":[{"filter":"all","label":"Caught Exceptions"},{"filter":"uncaught","label":"Uncaught Exceptions"}]}}"#,
        ]),
    );
    poll_until(&mut app, |a| {
        a.debug_sessions
            .focused()
            .is_some_and(|s| !s.capabilities.is_null())
    });
    app.debug_toggle_raised_exceptions();
    assert_eq!(app.status, "Breaking on raised exceptions (and uncaught)");
    assert_eq!(
        app.debug_sessions.focused().unwrap().exception_filters,
        vec![String::from("uncaught"), String::from("all")]
    );
    app.debug_toggle_raised_exceptions();
    assert_eq!(app.status, "Breaking on uncaught exceptions only");

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push(
        "Go",
        stub_emitting(&[
            r#"{"seq":1,"type":"response","request_seq":1,"success":true,"command":"initialize","body":{"supportsConditionalBreakpoints":true}}"#,
        ]),
    );
    poll_until(&mut app, |a| {
        a.debug_sessions
            .focused()
            .is_some_and(|s| !s.capabilities.is_null())
    });
    app.debug_toggle_raised_exceptions();
    assert_eq!(
        app.status,
        "This debugger has no filter for raised exceptions; exception breakpoints are unchanged"
    );
    assert_eq!(
        app.debug_sessions.focused().unwrap().exception_filters,
        vec![String::from("uncaught")]
    );
}

/// #1426: Alt+F9 pressed before the adapter answers `initialize` is kept
/// and translated into that adapter's own ids once it does, instead of
/// sending debugpy's `raised`, which lldb-dap would then drop.
#[test]
fn the_raised_exceptions_toggle_before_initialize_uses_the_adapters_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push(
        "LLDB",
        stub_emitting(&[
            r#"{"seq":1,"type":"response","request_seq":1,"success":true,"command":"initialize","body":{"exceptionBreakpointFilters":[{"filter":"cpp_catch","label":"C++ Catch"},{"filter":"cpp_throw","label":"C++ Throw"},{"filter":"objc_catch","label":"Objective-C Catch"},{"filter":"objc_throw","label":"Objective-C Throw"}]}}"#,
        ]),
    );
    app.debug_toggle_raised_exceptions();
    assert!(app.status.starts_with("Breaking on raised exceptions"));
    poll_until(&mut app, |a| {
        a.debug_sessions
            .focused()
            .is_some_and(|s| !s.capabilities.is_null())
    });
    assert_eq!(
        app.debug_sessions.focused().unwrap().exception_filters,
        vec![String::from("cpp_throw"), String::from("objc_throw")]
    );
}

/// #1421: an expensive scope (js-debug's Global) shows collapsed with a
/// chevron and no variables; a click opens it and fetches it. A cheap
/// scope stays open with no chevron.
#[test]
fn an_expensive_scope_shows_collapsed_and_opens_on_click() {
    use crate::widgets::run_debug::DebugRowKind;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push(
        "Node",
        stub_emitting(&[
            r#"{"seq":1,"type":"event","event":"stopped","body":{"reason":"breakpoint","threadId":1}}"#,
            r#"{"seq":2,"type":"response","request_seq":9,"success":true,"command":"scopes","body":{"scopes":[{"name":"Local: work","variablesReference":10,"expensive":false},{"name":"Global","variablesReference":20,"expensive":true}]}}"#,
        ]),
    );
    poll_until(&mut app, |a| {
        a.debug_sessions
            .focused()
            .is_some_and(|s| s.scopes.len() == 2)
    });
    app.refresh_debug_panel();
    let scope_rows = |app: &App| -> Vec<(String, Option<bool>)> {
        app.run_debug
            .debug_rows
            .iter()
            .filter_map(|r| match &r.kind {
                DebugRowKind::Scope { name, open, .. } => Some((name.clone(), *open)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        scope_rows(&app),
        vec![
            (String::from("Local: work"), None),
            (String::from("Global"), Some(false)),
        ]
    );
    let global = app
        .run_debug
        .debug_rows
        .iter()
        .position(|r| matches!(&r.kind, DebugRowKind::Scope { name, .. } if name == "Global"))
        .unwrap();
    app.debug_panel_click(global);
    assert_eq!(
        scope_rows(&app),
        vec![
            (String::from("Local: work"), None),
            (String::from("Global"), Some(true)),
        ]
    );
    assert!(
        app.debug_sessions
            .focused()
            .unwrap()
            .open_expensive_scopes
            .contains(&(String::from("Global"), 0))
    );
    // A second click closes it again; a click on the cheap scope does nothing.
    app.debug_panel_click(global);
    assert_eq!(scope_rows(&app)[1], (String::from("Global"), Some(false)));
    let local = app
        .run_debug
        .debug_rows
        .iter()
        .position(|r| matches!(&r.kind, DebugRowKind::Scope { name, .. } if name == "Local: work"))
        .unwrap();
    app.debug_panel_click(local);
    assert_eq!(scope_rows(&app)[0], (String::from("Local: work"), None));
}

/// #1421: two expensive scopes sharing a name open separately; opening
/// the second leaves the first collapsed.
#[test]
fn expensive_scopes_sharing_a_name_open_separately() {
    use crate::widgets::run_debug::DebugRowKind;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push(
        "Node",
        stub_emitting(&[
            r#"{"seq":1,"type":"event","event":"stopped","body":{"reason":"breakpoint","threadId":1}}"#,
            r#"{"seq":2,"type":"response","request_seq":9,"success":true,"command":"scopes","body":{"scopes":[{"name":"Global","variablesReference":20,"expensive":true},{"name":"Global","variablesReference":30,"expensive":true}]}}"#,
        ]),
    );
    poll_until(&mut app, |a| {
        a.debug_sessions
            .focused()
            .is_some_and(|s| s.scopes.len() == 2)
    });
    app.refresh_debug_panel();
    let scope_rows = |app: &App| -> Vec<Option<bool>> {
        app.run_debug
            .debug_rows
            .iter()
            .filter_map(|r| match &r.kind {
                DebugRowKind::Scope { open, .. } => Some(*open),
                _ => None,
            })
            .collect()
    };
    assert_eq!(scope_rows(&app), vec![Some(false), Some(false)]);
    let second = app
        .run_debug
        .debug_rows
        .iter()
        .position(|r| matches!(&r.kind, DebugRowKind::Scope { index: 1, .. }))
        .unwrap();
    app.debug_panel_click(second);
    assert_eq!(scope_rows(&app), vec![Some(false), Some(true)]);
}

/// #567 item 1: a background member's program output used to be drained and
/// dropped. It reaches the debug console, tagged with the member's name.
#[test]
fn a_background_members_output_reaches_the_console_with_its_name() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push(
        "A",
        stub_emitting(&[
            r#"{"seq":1,"type":"event","event":"output","body":{"category":"stdout","output":"hello from A\n"}}"#,
        ]),
    );
    app.debug_sessions.push("B", stub_member(false));
    app.debug_sessions.focus(1);
    poll_until(&mut app, |a| {
        a.debug_console
            .iter()
            .any(|l| l.text.contains("hello from A"))
    });
    assert!(
        app.debug_console
            .iter()
            .any(|l| l.text == "[A] hello from A"),
        "console: {:?}",
        app.debug_console
    );
    assert_eq!(
        app.debug_sessions.focused_name(),
        Some("B"),
        "output alone does not steal focus"
    );
    app.debug_stop();
}

/// #567 item 1: a background member hitting a breakpoint looked hung, its
/// `stopped` drained and dropped. With the focused member still running, the
/// view moves to the stopped one and says so.
#[test]
fn a_background_stop_moves_the_view_to_the_stopped_member() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push(
        "A",
        stub_emitting(&[
            r#"{"seq":1,"type":"event","event":"stopped","body":{"reason":"breakpoint","threadId":1}}"#,
        ]),
    );
    app.debug_sessions.push("B", stub_member(false));
    app.debug_sessions.focus(1);
    poll_until(&mut app, |a| a.debug_sessions.focused_name() == Some("A"));
    assert_eq!(app.debug_sessions.focused_name(), Some("A"));
    assert!(app.status.contains("A stopped"), "status: {}", app.status);
    assert_eq!(
        app.run_debug.feedback.as_deref(),
        Some("Paused (breakpoint)"),
        "the stop replays through the normal handler once A is focused"
    );
    app.debug_stop();
}

const STOPPED_EVENT: &str =
    r#"{"seq":1,"type":"event","event":"stopped","body":{"reason":"breakpoint","threadId":1}}"#;

/// A compound can list one configuration twice, so two members can share a
/// name: the member that stopped is the one shown, not the first of that name.
#[test]
fn a_stop_shows_the_member_that_stopped_even_when_names_repeat() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("A", stub_member(false));
    app.debug_sessions
        .push("A", stub_emitting(&[STOPPED_EVENT]));
    app.debug_sessions.push("B", stub_member(false));
    app.debug_sessions.focus(2);
    poll_until(&mut app, |a| a.debug_sessions.focused_index() != 2);
    assert_eq!(
        app.debug_sessions.focused_index(),
        1,
        "the second A stopped"
    );
    app.debug_stop();
}

/// A member ending in the same poll shifts the stopped member's position; the
/// view still lands on the member that stopped.
#[test]
fn a_stop_lands_on_the_right_member_when_an_earlier_one_ends_alongside() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push(
        "Gone",
        stub_emitting(&[r#"{"seq":1,"type":"event","event":"terminated"}"#]),
    );
    app.debug_sessions
        .push("Stops", stub_emitting(&[STOPPED_EVENT]));
    app.debug_sessions.push("Watched", stub_member(false));
    app.debug_sessions.focus(2);
    poll_until(&mut app, |a| {
        a.debug_sessions.len() == 2 && a.debug_sessions.focused_name() == Some("Stops")
    });
    assert_eq!(app.debug_sessions.names(), vec!["Stops", "Watched"]);
    assert_eq!(app.debug_sessions.focused_name(), Some("Stops"));
    app.debug_stop();
}

/// The switch command cycles the members, and says so when there is only one.
#[test]
fn switch_debug_session_cycles_the_members() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("A", stub_member(false));
    app.switch_debug_session();
    assert_eq!(app.status, "Only one debug session is running");
    app.debug_sessions.push("B", stub_member(false));
    assert_eq!(app.debug_sessions.focused_name(), Some("B"));
    app.switch_debug_session();
    assert_eq!(app.debug_sessions.focused_name(), Some("A"));
    assert!(
        app.status.starts_with("Debugging A"),
        "status: {}",
        app.status
    );
    app.switch_debug_session();
    assert_eq!(app.debug_sessions.focused_name(), Some("B"));
    assert!(is_switch_debug_session_key(key(
        KeyCode::Char('g'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT,
    )));
    assert!(!is_switch_debug_session_key(key(
        KeyCode::Char('g'),
        KeyModifiers::SUPER | KeyModifiers::ALT,
    )));
    app.debug_stop();
}

/// What a member reported in the background replays when it is switched to:
/// a queued `stopped` drives the same handler it would have when focused.
#[test]
fn a_switched_to_member_replays_its_queued_stop() {
    use crate::dap::session::DapEvent;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("A", stub_member(false));
    app.debug_sessions.push("B", stub_member(false));
    let (_, a) = app
        .debug_sessions
        .iter_named_mut_indexed()
        .find(|(i, _)| *i == 0)
        .unwrap();
    a.backlog.push(DapEvent::Stopped {
        thread_id: 1,
        reason: String::from("exception"),
    });
    app.poll_dap();
    assert_ne!(
        app.run_debug.feedback.as_deref(),
        Some("Paused (exception)"),
        "not replayed while A is in the background"
    );
    app.switch_debug_session();
    app.poll_dap();
    assert_eq!(
        app.run_debug.feedback.as_deref(),
        Some("Paused (exception)")
    );
    app.debug_stop();
}

/// #867: the Debug Console lived only in the sidebar, cut at its edge.
/// Program output and REPL results are mirrored, whole, to OUTPUT's
/// "Debug Console" channel, where they read at full width and can be
/// searched and copied.
#[test]
fn the_debug_console_is_mirrored_to_an_output_channel() {
    use crate::dap::session::DapEvent;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let tag = format!("mirror-{}", std::process::id());
    let printed = format!("{tag} {}", "wide ".repeat(40));
    let event = format!(
        r#"{{"seq":1,"type":"event","event":"output","body":{{"category":"stdout","output":"{printed}\n"}}}}"#
    );
    app.debug_sessions.push("P", stub_emitting(&[&event]));
    let mirrored = |text: &str| {
        crate::output::snapshot(crate::output::CHANNEL_DEBUG_CONSOLE)
            .unwrap_or_default()
            .iter()
            .any(|l| l.text == text)
    };
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the program's output to reach the Debug Console channel",
        || {
            app.poll_dap();
            mirrored(&printed)
        },
    );

    let (_, p) = app
        .debug_sessions
        .iter_named_mut_indexed()
        .find(|(i, _)| *i == 0)
        .unwrap();
    p.backlog.push(DapEvent::Evaluated {
        context: String::from("repl"),
        expression: format!("describe('{tag}')"),
        result: format!("'{tag} described'"),
        success: true,
    });
    app.poll_dap();
    assert!(mirrored(&format!("❯ describe('{tag}')")), "the REPL echo");
    assert!(mirrored(&format!("'{tag} described'")), "and its result");
    app.debug_stop();
}

/// #867 guard: the mirror carries what the console shows and nothing more.
/// debugpy's `telemetry` banner, which the console drops, stays out of the
/// channel too, and a printed line lands there once, not once per poll.
#[test]
fn the_debug_console_mirror_skips_telemetry_and_copies_each_line_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let tag = format!("mirror-once-{}", std::process::id());
    let telemetry = format!(
        r#"{{"seq":1,"type":"event","event":"output","body":{{"category":"telemetry","output":"{tag} ptvsd"}}}}"#
    );
    let printed = format!(
        r#"{{"seq":2,"type":"event","event":"output","body":{{"category":"stdout","output":"{tag} printed\n"}}}}"#
    );
    app.debug_sessions
        .push("P", stub_emitting(&[&telemetry, &printed]));
    let count = |text: &str| {
        crate::output::snapshot(crate::output::CHANNEL_DEBUG_CONSOLE)
            .unwrap_or_default()
            .iter()
            .filter(|l| l.text == text)
            .count()
    };
    let line = format!("{tag} printed");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the printed line to reach the Debug Console channel",
        || {
            app.poll_dap();
            count(&line) > 0
        },
    );
    for _ in 0..5 {
        app.poll_dap();
    }
    let channel = crate::output::snapshot(crate::output::CHANNEL_DEBUG_CONSOLE).unwrap_or_default();
    let in_panel = app.debug_console.iter().filter(|l| l.text == line).count();
    app.debug_stop();
    assert_eq!(count(&line), 1, "once");
    assert!(
        !channel
            .iter()
            .any(|l| l.text.contains(&format!("{tag} ptvsd"))),
        "telemetry is not mirrored"
    );
    assert_eq!(in_panel, 1, "and the panel keeps its own copy");
}

/// A stand-in debug adapter (#864) that stops on an exception as soon as it
/// is configured, advertising `exceptionInfo`. With `answer_at_once` it
/// answers that request as it comes; otherwise it holds the answer until
/// `continue`, then prints `MARKER`, after which the late answer has been
/// read.
fn exception_stop_adapter(dir: &Path, answer_at_once: bool) -> crate::dap::session::DapSession {
    let script = format!(
        r#"
import json, sys
AT_ONCE = {at_once}
seq = [1000]
held = []
def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    return json.loads(sys.stdin.buffer.read(length))
def send(msg):
    seq[0] += 1
    msg["seq"] = seq[0]
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()
def reply(req, body=None):
    send({{"type": "response", "request_seq": req["seq"], "success": True,
          "command": req["command"], "body": body or {{}}}})
def answer(req):
    reply(req, {{"exceptionId": "KeyError", "description": "'x'"}})
while True:
    req = read()
    if req is None:
        break
    c = req["command"]
    if c == "initialize":
        reply(req, {{"supportsExceptionInfoRequest": True}})
        send({{"type": "event", "event": "initialized"}})
    elif c == "configurationDone":
        reply(req)
        send({{"type": "event", "event": "stopped", "body": {{"reason": "exception", "threadId": 1}}}})
    elif c == "exceptionInfo":
        if AT_ONCE:
            answer(req)
        else:
            held.append(req)
    elif c == "continue":
        reply(req)
        for h in held:
            answer(h)
        send({{"type": "event", "event": "output", "body": {{"category": "stdout", "output": "MARKER\n"}}}})
    elif c == "disconnect":
        reply(req)
        break
    else:
        reply(req)
"#,
        at_once = if answer_at_once { "True" } else { "False" }
    );
    let path = dir.join("fake_dap.py");
    std::fs::write(&path, script).unwrap();
    crate::dap::session::DapSession::launch_with(
        "python3",
        &[path.display().to_string()],
        dir,
        serde_json::json!({"type": "request", "command": "launch", "arguments": {}}),
        std::collections::BTreeMap::new(),
    )
    .expect("python3 spawns")
}

/// #864: an `exceptionInfo` answer that arrives after Continue describes a
/// stop that is over. It must not put "Paused on exception" on the status
/// line of a program that is running again.
#[test]
fn an_exception_answer_after_continue_does_not_say_paused() {
    use crate::dap::session::SessionPhase;
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions
        .push("P", exception_stop_adapter(tmp.path(), false));
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the exception stop",
        || {
            app.poll_dap();
            app.debug_sessions
                .focused()
                .is_some_and(|s| s.phase == SessionPhase::Stopped)
        },
    );
    app.debug_start_or_continue();
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the late answer and the marker after it",
        || {
            app.poll_dap();
            app.debug_console.iter().any(|l| l.text == "MARKER")
        },
    );
    let status = app.status.clone();
    let feedback = app.run_debug.feedback.clone().unwrap_or_default();
    app.debug_stop();
    assert!(!status.contains("Paused"), "status: {status}");
    assert!(!feedback.contains("KeyError"), "feedback: {feedback}");
}

/// #1543: Continue, Step Into, Step Out and Step Into Target are commands:
/// the palette lists them, `keybindings.json` names them by id, and VS
/// Code's own chords for them carry over in `croft import-vscode`.
#[test]
fn the_step_commands_are_in_the_palette_and_bindable() {
    use crate::widgets::command_palette::{ALL_COMMANDS, Command};
    for (title, id) in [
        ("Debug: Continue", "continue"),
        ("Debug: Step Into", "step_into"),
        ("Debug: Step Out", "step_out"),
        ("Debug: Step Into Target", "step_into_target"),
    ] {
        let cmd = Command::from_id(id).unwrap_or_else(|| panic!("no command {id}"));
        assert_eq!(cmd.title(), title);
        assert!(ALL_COMMANDS.contains(&cmd));
    }
    assert_eq!(
        Command::from_id("step_into").unwrap().keybinding_hint(),
        "F11"
    );
    assert_eq!(
        Command::from_id("step_out").unwrap().keybinding_hint(),
        "Shift+F11"
    );
}

/// #1543: with a session paused on thread 1, the palette's Step Into and
/// Step Out send `stepIn` / `stepOut`, and Step Into Target's picker sends
/// `stepIn` with the chosen call's `targetId`.
#[test]
fn step_into_target_steps_into_the_picked_call() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let log = tmp.path().join("requests.log");
    let mut session = crate::dap::session::DapSession::launch_with(
        "sh",
        &[String::from("-c"), format!("cat > '{}'", log.display())],
        std::path::Path::new("."),
        serde_json::json!({"seq": 2, "type": "request", "command": "launch", "arguments": {}}),
        std::collections::BTreeMap::new(),
    )
    .expect("sh spawns");
    session.phase = crate::dap::session::SessionPhase::Stopped;
    session.stopped_thread = Some(1);
    app.debug_sessions.push("app.py", session);
    let sent = |needle: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if std::fs::read_to_string(&log).is_ok_and(|s| s.contains(needle)) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    };
    assert!(run_from_palette(&mut app, "Debug: Step Out"));
    assert!(sent("\"command\":\"stepOut\""));

    app.offer_step_in_targets(vec![
        (1, String::from("load('data.csv')")),
        (2, String::from("scale(2)")),
        (3, String::from("transform(...)")),
    ]);
    let picker = app
        .list_picker
        .as_ref()
        .expect("several calls open a picker");
    assert_eq!(picker.title, "Step Into Target");
    let labels: Vec<&str> = picker.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["load('data.csv')", "scale(2)", "transform(...)"]);
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(
        sent("\"targetId\":2"),
        "{:?}",
        std::fs::read_to_string(&log)
    );
}

/// #1543 negative: a line with one call steps straight into it and a line
/// with none steps in plainly; neither opens a picker.
#[test]
fn one_or_no_step_in_target_needs_no_picker() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let log = tmp.path().join("requests.log");
    let mut session = crate::dap::session::DapSession::launch_with(
        "sh",
        &[String::from("-c"), format!("cat > '{}'", log.display())],
        std::path::Path::new("."),
        serde_json::json!({"seq": 2, "type": "request", "command": "launch", "arguments": {}}),
        std::collections::BTreeMap::new(),
    )
    .expect("sh spawns");
    session.phase = crate::dap::session::SessionPhase::Stopped;
    session.stopped_thread = Some(1);
    app.debug_sessions.push("app.py", session);
    app.offer_step_in_targets(vec![(9, String::from("only()"))]);
    assert!(app.list_picker.is_none());
    app.offer_step_in_targets(Vec::new());
    assert!(app.list_picker.is_none());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut text = String::new();
    while std::time::Instant::now() < deadline {
        text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.matches("\"command\":\"stepIn\"").count() >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(text.contains("\"targetId\":9"), "{text}");
    assert_eq!(text.matches("\"command\":\"stepIn\"").count(), 2, "{text}");
}

/// #1543 negative: Debug: Continue resumes only a paused session; with no
/// session it says so instead of starting one (F5 still starts).
#[test]
fn continue_with_no_session_starts_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(run_from_palette(&mut app, "Debug: Continue"));
    assert!(app.debug_sessions.is_empty());
    assert_eq!(app.status, "No debug session to continue");
}

/// #864 guard: an answer that arrives while the program is still stopped
/// names the exception, on the status line and in the panel.
#[test]
fn an_exception_answer_while_stopped_names_the_exception() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions
        .push("P", exception_stop_adapter(tmp.path(), true));
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the exception to be named",
        || {
            app.poll_dap();
            app.status.contains("KeyError")
        },
    );
    let feedback = app.run_debug.feedback.clone();
    let status = app.status.clone();
    app.debug_stop();
    assert_eq!(status, "Paused on exception: KeyError: 'x'");
    assert_eq!(
        feedback.as_deref(),
        Some("Paused (exception): KeyError: 'x'")
    );
}

#[test]
fn breakpoint_items_list_every_breakpoint_by_file_then_line() {
    let (_tmp, app, _a, _b) = app_with_breakpoints();
    let items = app.breakpoint_items();
    let shown: Vec<(String, Option<String>)> = items
        .iter()
        .map(|i| (i.label.clone(), i.detail.clone()))
        .collect();
    assert_eq!(
        shown,
        vec![
            (String::from("a.rs:1"), None),
            (String::from("a.rs:3"), Some(String::from("if n > 2"))),
            (String::from("b.rs:4"), Some(String::from("log at {i}"))),
        ]
    );
}

/// The ✕ removes the breakpoint and the condition that went with it, so a
/// breakpoint set again later on that line starts plain.
#[test]
fn removing_from_the_list_drops_the_breakpoint_and_its_condition() {
    let (_tmp, mut app, a, _b) = app_with_breakpoints();
    app.sync_breakpoint_list();
    let idx = app
        .run_debug
        .breakpoints
        .iter()
        .position(|i| i.label == "a.rs:3")
        .unwrap();
    app.breakpoint_list_click(idx, true);
    assert!(!app.editor.breakpoints[&a].contains(&3));
    assert!(
        app.editor.breakpoints[&a].contains(&1),
        "only that one goes"
    );
    assert!(!app.editor.breakpoint_conditions[&a].contains_key(&3));
    assert_eq!(app.run_debug.breakpoints.len(), 2, "the list follows");
}

#[test]
fn clicking_a_breakpoint_opens_its_file_at_the_line() {
    // Line 1 of a three-line file: a jump one row off lands on a different
    // line, where the file's LAST line would be clamped back and hide it.
    let (_tmp, mut app, a, _b) = app_with_breakpoints();
    app.sync_breakpoint_list();
    let idx = app
        .run_debug
        .breakpoints
        .iter()
        .position(|i| i.label == "a.rs:1")
        .unwrap();
    app.breakpoint_list_click(idx, false);
    assert_eq!(app.editor.path.as_deref(), Some(a.as_path()));
    assert_eq!(app.editor.cursor_row, 0, "line 1, 0-based");
}

/// #1418: a Go "Connect to server" config connects to the headless delve at
/// its port instead of being refused for lacking a processId; a Go attach
/// with neither says what it needs.
#[test]
fn a_go_connect_to_server_attach_reaches_the_headless_delve() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let rc = resolved_attach(
        r#"{ "configurations": [ { "name": "Bare", "type": "go", "request": "attach" } ] }"#,
    );
    app.launch_resolved_config(rc);
    assert!(
        app.status.contains(
            r#"config "Bare": a go attach needs a "processId", or "mode": "remote" with a "port""#
        ),
        "{}",
        app.status
    );
    let rc = resolved_attach(
        r#"{ "configurations": [ { "name": "No port", "type": "go", "request": "attach", "mode": "remote" } ] }"#,
    );
    app.launch_resolved_config(rc);
    assert!(
        app.status
            .contains(r#"config "No port": a go remote attach needs a "port""#),
        "{}",
        app.status
    );
    assert!(app.debug_sessions.is_empty());

    let (port, server) = crate::dap::session::tests::headless_delve(2);
    let rc = resolved_attach(&format!(
        r#"{{ "configurations": [ {{ "name": "Connect to server", "type": "go", "request": "attach",
             "mode": "remote", "host": "127.0.0.1", "port": {port} }} ] }}"#
    ));
    app.launch_resolved_config(rc);
    let got = server.join().unwrap();
    assert_eq!(got[1]["command"], "attach");
    assert_eq!(got[1]["arguments"]["mode"], "remote");
    assert!(!app.debug_sessions.is_empty(), "{}", app.status);
}

fn resolved_attach(json: &str) -> crate::dap::configs::ResolvedConfig {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(tmp.path().join(".vscode/launch.json"), json).unwrap();
    let cfg = crate::dap::configs::discover_configs(tmp.path())
        .into_iter()
        .next()
        .expect("one config");
    let ctx = crate::dap::configs::SubstCtx {
        workspace_folder: tmp.path().to_path_buf(),
        file: None,
        folders: Vec::new(),
    };
    crate::dap::configs::resolve(&cfg, &ctx).expect("resolves")
}

/// #1419: an lldb attach with only a `program` (or `attachCommands`) is
/// started, not refused for lacking a pid; one with none of the three
/// names all of them.
#[test]
fn an_lldb_attach_needs_a_pid_a_program_or_attach_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let rc = resolved_attach(
        r#"{ "configurations": [ { "name": "Nothing", "type": "lldb", "request": "attach" } ] }"#,
    );
    app.launch_resolved_config(rc);
    if lldb_dap_path().is_none() {
        assert!(app.debug_sessions.is_empty());
        return;
    }
    assert!(
        app.status.contains(
            r#"config "Nothing": an lldb attach needs a numeric "processId", a "program" or "attachCommands""#
        ),
        "{}",
        app.status
    );
    assert!(app.debug_sessions.is_empty());

    for config in [
        r#"{ "name": "By name", "type": "lldb", "request": "attach", "program": "/no/such/app", "waitFor": true }"#,
        r#"{ "name": "Remote", "type": "lldb", "request": "attach", "attachCommands": ["gdb-remote 127.0.0.1:9"] }"#,
    ] {
        let rc = resolved_attach(&format!(r#"{{ "configurations": [ {config} ] }}"#));
        app.launch_resolved_config(rc);
        assert!(!app.status.contains("needs a numeric"), "{}", app.status);
        assert!(!app.debug_sessions.is_empty(), "{config} starts a session");
        app.debug_stop();
    }
}

/// #250: an lldb attach whose `processId` is `${command:pickProcess}` opens
/// a process picker instead of failing, and the chosen pid resumes it.
#[test]
fn a_pick_process_attach_opens_the_process_picker() {
    use crate::widgets::list_picker::ListPurpose;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let rc = resolved_attach(
        r#"{ "configurations": [ { "name": "Attach", "type": "lldb", "request": "attach",
             "processId": "${command:pickProcess}" } ] }"#,
    );
    app.launch_resolved_config(rc);
    let picker = app.list_picker.as_ref().expect("the picker opens");
    assert!(matches!(picker.purpose, ListPurpose::AttachProcess));
    assert!(
        !picker.rows.is_empty(),
        "this test process's siblings are listed"
    );
    assert!(
        picker.rows.iter().all(|r| r.id.parse::<i64>().is_ok()),
        "every row id is a pid"
    );
    assert!(
        !picker
            .rows
            .iter()
            .any(|r| r.id == std::process::id().to_string()),
        "croft does not offer itself"
    );
    assert!(
        app.debug_sessions.is_empty(),
        "nothing launched before the pick"
    );

    let (rc, _) = app.take_pending_attach(4242).expect("the attach is parked");
    assert_eq!(rc.process_id, Some(4242));
    assert!(
        app.take_pending_attach(4242).is_none(),
        "taken once, not twice"
    );
}

/// `pickProcess` on an adapter that does not attach by pid is refused with
/// a message, not launched with the key silently dropped.
#[test]
fn pick_process_on_a_non_lldb_config_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let rc = resolved_attach(
        r#"{ "configurations": [ { "name": "Py", "type": "python", "request": "attach",
             "processId": "${command:pickProcess}" } ] }"#,
    );
    app.launch_resolved_config(rc);
    assert!(app.list_picker.is_none());
    assert!(
        app.status.contains("pickProcess")
            || app
                .run_debug
                .feedback
                .as_deref()
                .unwrap_or("")
                .contains("pickProcess"),
        "status: {} / feedback: {:?}",
        app.status,
        app.run_debug.feedback
    );
}

/// Replacing the set for a new launch is not the end of a debug run, so
/// the internal stop leaves the task alone (VS Code runs it on stop and on
/// termination, not on relaunch).
#[test]
fn the_internal_stop_before_a_relaunch_runs_no_post_debug_task() {
    let (_tmp, mut app) = app_with_cleanup_task();
    app.debug_sessions.push("A", stub_member(false));
    app.debug_post_tasks = vec![String::from("cleanup")];
    app.debug_stop();
    assert_eq!(last_task_label(&app), None);
}

#[test]
fn stop_all_true_stops_the_set_when_a_background_member_ends() {
    // #567 item 4: `stopAll: true` was refused; it is honoured now.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("A", stub_member(true));
    app.debug_sessions.push("B", stub_member(false));
    app.debug_sessions.focus(1);
    app.debug_stop_all = true;

    poll_until_shrinks(&mut app, 2);
    assert!(
        app.debug_sessions.is_empty(),
        "A ending takes B with it: {:?}",
        app.debug_sessions.names()
    );
    assert!(
        app.status.contains("stopAll"),
        "and says why: {}",
        app.status
    );
}

#[test]
fn stop_all_false_leaves_the_siblings_running() {
    // The control: without stopAll only the member that ended goes.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("A", stub_member(true));
    app.debug_sessions.push("B", stub_member(false));
    app.debug_sessions.focus(1);

    poll_until_shrinks(&mut app, 2);
    assert_eq!(app.debug_sessions.names(), vec!["B"]);
    app.debug_stop();
}

/// #867: the issue's recording shows "Debug session ended" in the panel
/// while the status bar still read "Debugging out.p… · F10 step over ·
/// Shift+F5 stop". A session that ends on its own says so on the status
/// line as well.
#[test]
fn a_session_ending_on_its_own_says_so_on_the_status_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("out.py", stub_member(true));
    app.status = String::from("Debugging out.py — F5 continue · F10 step over · Shift+F5 stop");
    poll_until_shrinks(&mut app, 1);
    assert!(app.debug_sessions.is_empty(), "the stub terminated");
    assert_eq!(
        app.run_debug.feedback.as_deref(),
        Some("Debug session ended")
    );
    assert_eq!(app.status, "Debug session ended");
}

/// #1547: a stand-in adapter for a program paused in `app.py`: the first
/// stop is a breakpoint on line 3, and every `next` or `continue` stops on
/// line 3 again, as a loop body does.
fn loop_stop_adapter(dir: &Path) -> crate::dap::session::DapSession {
    stop_adapter(dir, false)
}

/// [`loop_stop_adapter`], where with `sourceless_step` a `next` stops in a
/// frame with no source, as native code does.
fn stop_adapter(dir: &Path, sourceless_step: bool) -> crate::dap::session::DapSession {
    let app_py = dir.join("app.py");
    std::fs::write(
        &app_py,
        "total = 0\nfor n in range(3):\n    total += n\nprint(total)\n",
    )
    .unwrap();
    let script = format!(
        r#"
import json, sys
seq = [1000]
stepped = [False]
sourceless = {sourceless}
def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    return json.loads(sys.stdin.buffer.read(length))
def send(msg):
    seq[0] += 1
    msg["seq"] = seq[0]
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()
def reply(req, body=None):
    send({{"type": "response", "request_seq": req["seq"], "success": True,
          "command": req["command"], "body": body or {{}}}})
def stop(reason):
    send({{"type": "event", "event": "stopped", "body": {{"reason": reason, "threadId": 1}}}})
while True:
    req = read()
    if req is None:
        break
    c = req["command"]
    if c == "initialize":
        reply(req)
        send({{"type": "event", "event": "initialized"}})
    elif c == "configurationDone":
        reply(req)
        stop("breakpoint")
    elif c == "threads":
        reply(req, {{"threads": [{{"id": 1, "name": "main"}}]}})
    elif c == "stackTrace":
        if sourceless and stepped[0]:
            reply(req, {{"stackFrames": [{{"id": 2, "name": "memcpy", "line": 0, "column": 0}}],
                        "totalFrames": 1}})
        else:
            reply(req, {{"stackFrames": [{{"id": 1, "name": "<module>", "line": 3, "column": 1,
                        "source": {{"path": {path}}}}}], "totalFrames": 1}})
    elif c == "next":
        stepped[0] = True
        reply(req)
        stop("step")
    elif c == "continue":
        reply(req)
        stop("breakpoint")
    elif c == "disconnect":
        reply(req)
        break
    else:
        reply(req)
"#,
        path = serde_json::to_string(&app_py.display().to_string()).unwrap(),
        sourceless = if sourceless_step { "True" } else { "False" },
    );
    let path = dir.join("loop_dap.py");
    std::fs::write(&path, script).unwrap();
    crate::dap::session::DapSession::launch_with(
        "python3",
        &[path.display().to_string()],
        dir,
        serde_json::json!({"type": "request", "command": "launch", "arguments": {}}),
        std::collections::BTreeMap::new(),
    )
    .expect("python3 spawns")
}

fn has_python3() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok()
}

/// Poll the adapter until screen reader mode has a new event past `seen`,
/// and return it.
fn await_a11y_event(app: &mut App, seen: Option<u64>) -> String {
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the pause's announcement",
        || {
            app.poll_dap();
            app.a11y_event.as_ref().map(|(n, _)| *n) > seen
        },
    );
    app.update_announcer();
    app.announcer.line.clone()
}

/// #1547: hitting a breakpoint was silent in screen reader mode: a stop
/// set only the panel's pill. It now reads why, where and the line.
#[test]
fn a_breakpoint_pause_is_read_out_with_its_file_line_and_text() {
    if !has_python3() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.screen_reader = true;
    app.debug_sessions.push("P", loop_stop_adapter(tmp.path()));
    let said = await_a11y_event(&mut app, None);
    app.debug_stop();
    assert!(
        said.contains("Paused on breakpoint, app.py line 3: total += n"),
        "said: {said}"
    );
}

/// #1547: a step that lands on the line it left, and a continue back to
/// the same breakpoint, are each read again, and Continue says so.
#[test]
fn every_stop_is_read_even_on_the_same_line() {
    if !has_python3() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.screen_reader = true;
    app.debug_sessions.push("P", loop_stop_adapter(tmp.path()));
    await_a11y_event(&mut app, None);
    let first = app.a11y_event.as_ref().map(|(n, _)| *n);

    app.debug_step("next");
    let said = await_a11y_event(&mut app, first);
    assert!(
        said.contains("Paused on step, app.py line 3"),
        "said: {said}"
    );
    let stepped = app.a11y_event.as_ref().map(|(n, _)| *n);

    app.debug_start_or_continue();
    assert_eq!(
        app.a11y_event.as_ref().map(|(_, e)| e.as_str()),
        Some("Continued")
    );
    app.update_announcer();
    assert!(
        app.announcer.line.contains("Continued"),
        "{}",
        app.announcer.line
    );
    let continued = app.a11y_event.as_ref().map(|(n, _)| *n);
    assert!(continued > stepped);

    let said = await_a11y_event(&mut app, continued);
    app.debug_stop();
    assert!(
        said.contains("Paused on breakpoint, app.py line 3: total += n"),
        "said: {said}"
    );
}

/// #1547: a stop whose top frame has no source keeps the last stop's
/// location in the session; the pause is read with the frame's name, not
/// that stale file and line.
#[test]
fn a_sourceless_stop_is_not_read_with_the_last_stops_location() {
    if !has_python3() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.screen_reader = true;
    app.debug_sessions.push("P", stop_adapter(tmp.path(), true));
    await_a11y_event(&mut app, None);
    let first = app.a11y_event.as_ref().map(|(n, _)| *n);

    app.debug_step("next");
    let said = await_a11y_event(&mut app, first);
    app.debug_stop();
    assert!(said.contains("Paused on step, in memcpy"), "said: {said}");
    assert!(!said.contains("app.py"), "said: {said}");
}

/// #1547 negative: with screen reader mode off a pause records nothing,
/// so turning the mode on later never reads out a stale stop.
#[test]
fn a_pause_with_screen_reader_mode_off_is_not_kept() {
    if !has_python3() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.screen_reader = false;
    app.debug_sessions.push("P", loop_stop_adapter(tmp.path()));
    crate::test_budget::await_spawned(std::time::Duration::from_millis(500), "the stop", || {
        app.poll_dap();
        app.editor.stop_line.is_some()
    });
    app.debug_stop();
    assert_eq!(app.a11y_event, None);
}

/// #1547 (comment): a Run All that finished was silent; it sets no
/// "Running …" status for the outcome to replace. Its counts and first
/// failure are now read out.
#[test]
fn a_finished_run_all_is_read_out_with_its_first_failure() {
    use crate::testing::model::{Activity, TestCase, TestStatus};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.screen_reader = true;
    app.testing.on_busy_started(Activity::Running);
    for (name, status) in [
        ("m::a", TestStatus::Passed),
        ("m::b", TestStatus::Failed),
        ("m::c", TestStatus::Skipped),
    ] {
        app.testing.apply_case(TestCase {
            name: name.to_string(),
            status,
        });
    }
    app.testing.on_finished(Some(false));
    app.tick_test_watch();
    app.update_announcer();
    assert!(
        app.announcer
            .line
            .contains("Tests finished: 1 failed, 1 passed, 1 skipped. First failure: m::b"),
        "said: {}",
        app.announcer.line
    );
}

/// #1547 negative: a run whose outcome already went to the status line
/// is not read twice; only its first failure is added.
#[test]
fn a_run_with_a_status_outcome_adds_only_its_first_failure() {
    use crate::testing::model::{TestCase, TestStatus};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.screen_reader = true;
    app.testing.start_filter("m::");
    app.status = String::from("Running suite m");
    app.await_test_run_outcome(String::from("suite m"));
    app.testing.apply_case(TestCase {
        name: String::from("m::b"),
        status: TestStatus::Failed,
    });
    app.testing.on_finished(Some(false));
    app.tick_test_watch();
    assert!(app.status.starts_with("suite m failed"), "{}", app.status);
    assert_eq!(
        app.a11y_event.as_ref().map(|(_, e)| e.as_str()),
        Some("First failure: m::b")
    );
}

/// #867: the run that never reached its breakpoint says that on the
/// status line too, as the panel does.
#[test]
fn a_run_that_never_hit_its_breakpoint_says_so_on_the_status_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .breakpoints
        .entry(tmp.path().join("out.py"))
        .or_default()
        .insert(1);
    app.debug_sessions.push("out.py", stub_member(true));
    app.status = String::from("Debugging out.py — F5 continue · F10 step over · Shift+F5 stop");
    poll_until_shrinks(&mut app, 1);
    assert!(app.debug_sessions.is_empty(), "the stub terminated");
    assert_eq!(app.status, "Program exited without hitting a breakpoint");
}

/// #867 guard: the end message is written once, when the session ends. A
/// status set after that (the user's next action) survives later polls.
#[test]
fn a_status_set_after_the_debug_session_ended_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("out.py", stub_member(true));
    poll_until_shrinks(&mut app, 1);
    assert!(app.debug_sessions.is_empty(), "the stub terminated");
    app.status = String::from("Saved out.py");
    for _ in 0..5 {
        app.poll_dap();
    }
    assert_eq!(app.status, "Saved out.py");
}

/// #867 guard: when the focused member of a `stopAll` compound ends, the
/// status keeps saying why the rest stopped rather than the plain end
/// message.
#[test]
fn stop_all_keeps_its_explanation_when_the_focused_member_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("A", stub_member(true));
    app.debug_sessions.push("B", stub_member(false));
    app.debug_sessions.focus(0);
    app.debug_stop_all = true;

    poll_until_shrinks(&mut app, 2);
    assert!(
        app.debug_sessions.is_empty(),
        "{:?}",
        app.debug_sessions.names()
    );
    assert_eq!(
        app.status,
        "A ended — stopAll stopped the rest of the compound"
    );
}

/// A compound that is REFUSED (here, it names a configuration no
/// launch.json declares) leaves the running set alone, so it must not leave
/// its `stopAll` behind either: that set never asked for it.
#[test]
fn a_refused_compound_does_not_hand_its_stop_all_to_the_running_set() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{"version":"0.2.0","configurations":[],
            "compounds":[{"name":"Bad","configurations":["Missing"],"stopAll":true}]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("A", stub_member(true));
    app.debug_sessions.push("B", stub_member(false));
    app.debug_sessions.focus(1);
    let compound = crate::dap::configs::discover_compounds(tmp.path())
        .into_iter()
        .find(|c| c.name == "Bad")
        .expect("the compound parses");
    assert!(compound.stop_all);
    app.launch_compound(&compound);
    assert_eq!(
        app.debug_sessions.len(),
        2,
        "the refusal leaves the set running"
    );

    poll_until_shrinks(&mut app, 2);
    assert_eq!(
        app.debug_sessions.names(),
        vec!["B"],
        "A ending must not take B: the running set never asked for stopAll"
    );
    app.debug_stop();
}

#[test]
fn a_background_member_ending_rebuilds_the_report_from_the_live_set() {
    // #567 items 2 and 3: the status and panel line kept naming the ended
    // session. Both now say what ended, what is shown, and what still runs.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("A", stub_member(true));
    app.debug_sessions.push("B", stub_member(false));
    app.debug_sessions.focus(1);
    app.debug_compound = Some(String::from("Stack"));
    app.run_debug.feedback = Some(String::from("Debugging compound Stack: A, B"));

    poll_until_shrinks(&mut app, 2);
    assert_eq!(app.debug_sessions.names(), vec!["B"]);
    assert_eq!(
        app.run_debug.feedback.as_deref(),
        Some("Debugging compound Stack: B (A ended)")
    );
    assert!(
        app.status.starts_with("A ended — showing B; running: B"),
        "{}",
        app.status
    );
    app.debug_stop();
    assert_eq!(app.debug_compound, None, "a full stop forgets the compound");
}

#[test]
fn the_focused_member_ending_names_the_session_now_shown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("A", stub_member(true));
    app.debug_sessions.push("B", stub_member(false));
    app.debug_sessions.focus(0);
    app.debug_compound = Some(String::from("Stack"));

    poll_until_shrinks(&mut app, 2);
    assert_eq!(app.debug_sessions.names(), vec!["B"]);
    assert!(
        app.status.starts_with("A ended — showing B; running: B"),
        "{}",
        app.status
    );
    app.debug_stop();
}

// ---- #611: hit counts, function breakpoints, Run to Cursor ----

#[test]
fn a_hit_count_prompt_sets_the_breakpoints_hit_condition() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "x = 1\ny = 2\n");
    app.editor.cursor_row = 1;
    app.run_command(crate::widgets::command_palette::Command::DebugAddHitCountBreakpoint);
    let path = app.editor.path.clone().unwrap();
    assert!(matches!(
        app.prompt.as_ref().map(|p| &p.kind),
        Some(PromptKind::HitCondition { line: 2, .. })
    ));
    app.commit_hit_condition(path.clone(), 2, " 5 ");
    let lines = app.editor.breakpoints.get(&path).cloned().unwrap();
    assert!(lines.contains(&2), "the breakpoint exists");
    let specs = app.editor.source_breakpoints(&path, &lines);
    assert_eq!(specs[0].hit_condition.as_deref(), Some("5"));
    app.commit_hit_condition(path.clone(), 2, "");
    let specs = app.editor.source_breakpoints(&path, &lines);
    assert_eq!(specs[0].hit_condition, None, "blank clears it");
}

#[test]
fn function_breakpoints_are_kept_for_the_next_session() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.commit_function_breakpoint(String::from("main"));
    app.commit_function_breakpoint(String::from("main"));
    assert_eq!(
        app.function_breakpoints,
        vec![String::from("main")],
        "no duplicates"
    );
    app.debug_clear_function_breakpoints();
    assert!(app.function_breakpoints.is_empty());
}

#[test]
fn run_to_cursor_without_a_session_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "x = 1\n");
    let ctrl_f10 = crossterm::event::KeyEvent::new(KeyCode::F(10), KeyModifiers::CONTROL);
    assert!(is_run_to_cursor_key(ctrl_f10));
    assert!(!is_run_to_cursor_key(crossterm::event::KeyEvent::new(
        KeyCode::F(10),
        KeyModifiers::NONE
    )));
    app.debug_run_to_cursor();
    assert_eq!(app.status, "Run to Cursor: no debug session");
}

const EXITED_EVENT: &str = r#"{"seq":1,"type":"event","event":"exited","body":{"exitCode":0}}"#;
const TERMINATED_EVENT: &str = r#"{"seq":2,"type":"event","event":"terminated"}"#;

/// Adapters send `exited` and then `terminated`, both of which end a member.
/// Arriving in one poll they named the member twice, and the second removal
/// took a live sibling with it.
#[test]
fn a_member_that_exits_and_terminates_in_one_poll_is_removed_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions
        .push("A", stub_emitting(&[EXITED_EVENT, TERMINATED_EVENT]));
    app.debug_sessions.push("B", stub_member(false));
    app.debug_sessions.push("C", stub_member(false));
    app.debug_sessions.focus(2);
    // Both messages are written before the first poll reads them.
    std::thread::sleep(std::time::Duration::from_millis(300));
    poll_until(&mut app, |a| {
        !a.debug_sessions.names().contains(&String::from("A"))
    });
    assert_eq!(
        app.debug_sessions.names(),
        vec![String::from("B"), String::from("C")]
    );
    app.debug_stop();
}

/// The focused member ending in the same poll a background member stops: the
/// view used to move to the stopped member first, carrying the ending with it
/// into the old member's backlog, so the ended member was never removed.
#[test]
fn a_focused_member_that_ends_as_another_stops_is_still_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions
        .push("S", stub_emitting(&[STOPPED_EVENT]));
    app.debug_sessions
        .push("F", stub_emitting(&[TERMINATED_EVENT]));
    app.debug_sessions.focus(1);
    std::thread::sleep(std::time::Duration::from_millis(300));
    poll_until(&mut app, |a| {
        a.debug_sessions.names() == vec![String::from("S")]
    });
    assert_eq!(
        app.debug_sessions.names(),
        vec![String::from("S")],
        "F ended and must go"
    );
    app.debug_stop();
}

#[cfg(unix)]
#[test]
fn debugging_a_codeql_query_stops_at_its_first_breakpoint() {
    // #578: Debug Query quick-evaluates the predicate at the first
    // breakpoint; with none it runs the whole query.
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(
            tmp.path(),
            "import python\n\npredicate isCall(Call c) { exists(c) }\n\nfrom Call c\nwhere isCall(c)\nselect c\n",
        );
        app.codeql_program = fake_query_server(
            bin.path(),
            r#"{"resultType": 0, "message": "", "evaluationTime": 1}"#,
            "\"c\"\n",
        );
        let query = tmp.path().join("q.ql");
        // A breakpoint on a line with no predicate is refused with why.
        app.editor
            .breakpoints
            .entry(query.clone())
            .or_default()
            .insert(7);
        app.run_command(Command::CodeqlDebugQuery);
        assert!(
            app.status
                .starts_with("No predicate at the breakpoint on line 7"),
            "{}",
            app.status
        );
        // On the `where` line: the called predicate.
        app.editor
            .breakpoints
            .entry(query.clone())
            .or_default()
            .insert(6);
        app.run_command(Command::CodeqlDebugQuery);
        assert_eq!(
            app.status,
            "Running Quick evaluation of isCall (breakpoint on line 6) on app\u{2026}"
        );
        wait_for_codeql(&mut app);
        let requests = std::fs::read_to_string(bin.path().join("requests.jsonl")).unwrap();
        let run: serde_json::Value =
            serde_json::from_str(requests.lines().last().unwrap()).unwrap();
        assert_eq!(
            run["params"]["body"]["target"]["quickEval"]["quickEvalPos"]["line"],
            6
        );
        assert_eq!(
            run["params"]["body"]["target"]["quickEval"]["quickEvalPos"]["column"],
            7
        );
        // Without breakpoints the whole query runs, as Run Query does.
        app.editor.open(&query).unwrap();
        app.editor.breakpoints.clear();
        app.run_command(Command::CodeqlDebugQuery);
        assert_eq!(app.status, "Running q.ql on app\u{2026}");
        wait_for_codeql(&mut app);
    });
}

/// #264: an adapter that refuses to start the program (delve and a Go too
/// old for it) ends the debug session with its reason in the status and
/// the Run and Debug feedback, not "Debug session ended", and not a
/// session left Initializing.
#[test]
fn a_refused_debug_launch_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let refused = r#"{"seq":1,"type":"response","request_seq":2,"success":false,"command":"launch","message":"Failed to launch","body":{"error":{"id":3000,"format":"Failed to launch /w: Go version go1.24.7 is too old for this version of Delve"}}}"#;
    app.debug_sessions.push("go", stub_emitting(&[refused]));
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the refusal to end the session",
        || {
            app.poll_dap();
            app.debug_sessions.is_empty()
        },
    );
    assert!(
        app.status
            .starts_with("Could not start debugging: Failed to launch /w: Go version"),
        "{}",
        app.status
    );
    assert!(app.run_debug.feedback_is_error);
    assert!(
        app.run_debug
            .feedback
            .as_deref()
            .is_some_and(|f| f.contains("too old for this version of Delve"))
    );
}

/// Pick the launch.json configuration `label` in the Run and Debug picker,
/// which starts it, as the user's click does.
fn start_debug_config(app: &mut App, label: &str) {
    app.open_debug_config_picker();
    let idx = app
        .list_picker
        .as_ref()
        .expect("picker open")
        .rows
        .iter()
        .position(|r| r.label.starts_with(label))
        .unwrap_or_else(|| panic!("{label} is listed"));
    app.list_picker.as_mut().unwrap().selected = idx;
    app.confirm_list_picker();
}

/// Drive the app's debug machinery (the preLaunchTask pane and the adapter)
/// until the focused session is stopped with `local` loaded, or `secs` pass.
fn debug_until_local(app: &mut App, local: &str, secs: u64) -> Option<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        app.drain_terminal_bells();
        app.poll_dap();
        if let Some(v) = app
            .debug_sessions
            .focused()
            .and_then(|s| s.lookup_local(local))
        {
            return Some(v.value.clone());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

/// #250, against a real debugpy: a VS Code launch.json config with `args`
/// and an `envFile` launches the program with both, so a breakpoint after
/// they are read sees their values. Needs `~/.croft/debug-venv`.
#[test]
#[ignore = "requires ~/.croft/debug-venv (uv + debugpy)"]
fn a_launch_json_python_config_passes_args_and_env_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::write(
        root.join("app.py"),
        "import os, sys\nwho = sys.argv[1]\ngreeting = os.environ[\"GREETING\"]\nprint(f\"{greeting}, {who}\")\n",
    )
    .unwrap();
    std::fs::write(root.join(".env"), "GREETING=hi from envFile\n").unwrap();
    std::fs::create_dir_all(root.join(".vscode")).unwrap();
    std::fs::write(
        root.join(".vscode/launch.json"),
        r#"{ "version": "0.2.0", "configurations": [
            // Written for VS Code: comments, and keys croft does not map.
            { "name": "Py", "type": "debugpy", "request": "launch",
              "program": "${workspaceFolder}/app.py", "args": ["croft"],
              "envFile": "${workspaceFolder}/.env", "console": "integratedTerminal",
              "justMyCode": true }
        ]}"#,
    )
    .unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor
        .breakpoints
        .entry(root.join("app.py"))
        .or_default()
        .insert(4);
    start_debug_config(&mut app, "Py");
    let greeting = debug_until_local(&mut app, "greeting", 90);
    let who = app
        .debug_sessions
        .focused()
        .and_then(|s| s.lookup_local("who"))
        .map(|v| v.value.clone());
    app.debug_stop();
    assert_eq!(
        greeting.as_deref(),
        Some("'hi from envFile'"),
        "status {:?}, feedback {:?}",
        app.status,
        app.run_debug.feedback
    );
    assert_eq!(who.as_deref(), Some("'croft'"));
}

/// #864 against a real debugpy: F5 on a script importing a module only the
/// project's `.venv` has stopped with "Paused (exception)" on line 1, since
/// the program ran under croft's debug venv. It now reaches its breakpoint
/// with the module's value, and the exception it raises next is named.
/// Needs `~/.croft/debug-venv` and a `python3` with `venv`.
#[test]
#[ignore = "requires ~/.croft/debug-venv (uv + debugpy)"]
fn f5_debugs_a_script_against_the_project_venv_and_names_its_exception() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let made = std::process::Command::new("python3")
        .args(["-m", "venv", "--without-pip"])
        .arg(root.join(".venv"))
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let site = std::process::Command::new(root.join(".venv/bin/python"))
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_path('purelib'))",
        ])
        .output()
        .unwrap();
    let site = PathBuf::from(String::from_utf8(site.stdout).unwrap().trim());
    std::fs::write(site.join("croftdep.py"), "VALUE = 42\n").unwrap();
    let file = root.join("main.py");
    std::fs::write(
        &file,
        "import croftdep\nvalue = croftdep.VALUE\nraise ValueError(f\"boom {value}\")\n",
    )
    .unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open(&file).unwrap();
    app.editor
        .breakpoints
        .entry(file.clone())
        .or_default()
        .insert(3);
    app.debug_start_or_continue();
    let value = debug_until_local(&mut app, "value", 90);
    assert_eq!(
        value.as_deref(),
        Some("42"),
        "status {:?}, feedback {:?}",
        app.status,
        app.run_debug.feedback
    );
    app.debug_start_or_continue();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline
        && !app
            .run_debug
            .feedback
            .as_deref()
            .is_some_and(|f| f.contains("ValueError"))
    {
        app.poll_dap();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let feedback = app.run_debug.feedback.clone();
    let status = app.status.clone();
    app.debug_stop();
    assert_eq!(
        feedback.as_deref(),
        Some("Paused (exception): ValueError: boom 42"),
        "status {status:?}"
    );
    assert!(status.contains("ValueError: boom 42"), "{status}");
}

/// A real `python3 -m venv --without-pip` at `dir`, and its purelib
/// site-packages directory.
fn real_venv(dir: &Path) -> PathBuf {
    let made = std::process::Command::new("python3")
        .args(["-m", "venv", "--without-pip"])
        .arg(dir)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let site = std::process::Command::new(dir.join("bin/python"))
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_path('purelib'))",
        ])
        .output()
        .unwrap();
    PathBuf::from(String::from_utf8(site.stdout).unwrap().trim())
}

/// #864 against a real debugpy: F5 under a project venv older than 3.14
/// names its version, on the status line and as the Debug Console's first
/// line, and still debugs. Skipped when `python3` is already 3.14+.
/// Needs `~/.croft/debug-venv` and a `python3` with `venv`.
#[test]
#[ignore = "requires ~/.croft/debug-venv (uv + debugpy)"]
fn f5_names_a_project_python_older_than_3_14() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    real_venv(&root.join(".venv"));
    let version = std::process::Command::new(root.join(".venv/bin/python"))
        .args(["-c", "import sys; print('%d.%d' % sys.version_info[:2])"])
        .output()
        .unwrap();
    let version = String::from_utf8(version.stdout)
        .unwrap()
        .trim()
        .to_string();
    let (major, minor) = version.split_once('.').unwrap();
    let minor: u32 = minor.parse().unwrap();
    if major != "3" || !(10..14).contains(&minor) {
        eprintln!("python3 is {version}: nothing to name");
        return;
    }
    let file = root.join("main.py");
    std::fs::write(&file, "x = 1\n").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open(&file).unwrap();
    app.debug_start_or_continue();
    let started = !app.debug_sessions.is_empty();
    let status = app.status.clone();
    let console = app.debug_console.clone();
    app.debug_stop();
    let want = format!("Debugging main.py under Python {version} (.venv); attach needs 3.14+");
    assert!(started, "{status}");
    assert!(status.starts_with(&want), "{status}");
    assert!(
        status.contains("F10 step over"),
        "the key hints stay: {status}"
    );
    assert_eq!(
        console.first().map(|l| l.text.as_str()),
        Some(want.as_str()),
        "{console:?}"
    );
}

/// #864 against a real debugpy: Debug Test at Cursor runs pytest under
/// the venv nearest the test file, as F5 and Live Run pick it, not only a
/// workspace-root `.venv`. The only venv here is `svc/venv`, holding a
/// stand-in `pytest` module that records `sys.prefix`; the root-only lookup
/// ran a `python3` without it.
/// Needs `~/.croft/debug-venv` and a `python3` with `venv`.
#[test]
#[ignore = "requires ~/.croft/debug-venv (uv + debugpy)"]
fn debug_test_at_cursor_runs_pytest_under_the_nearest_venv() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::write(root.join("pytest.ini"), "[pytest]\n").unwrap();
    let svc = root.join("svc");
    std::fs::create_dir_all(&svc).unwrap();
    let site = real_venv(&svc.join("venv"));
    let pytest = site.join("pytest");
    std::fs::create_dir_all(&pytest).unwrap();
    std::fs::write(pytest.join("__init__.py"), "").unwrap();
    let main = pytest.join("__main__.py");
    std::fs::write(&main, "import sys\nprefix = sys.prefix\nprint(prefix)\n").unwrap();
    let test = svc.join("test_app.py");
    std::fs::write(&test, "def test_prefix():\n    assert True\n").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open(&test).unwrap();
    app.editor.cursor_row = 1;
    app.editor
        .breakpoints
        .entry(main.clone())
        .or_default()
        .insert(3);
    app.debug_test_at_cursor();
    let prefix = debug_until_local(&mut app, "prefix", 90);
    let status = app.status.clone();
    app.debug_stop();
    let prefix = prefix.unwrap_or_else(|| panic!("no stop in pytest; status {status:?}"));
    assert!(prefix.contains("svc/venv"), "sys.prefix {prefix}");
}

/// #250, against a real lldb-dap: a Rust binary that needs a CLI argument
/// and an env var is debugged from a config whose `cargo build`
/// preLaunchTask builds it first. Needs cargo and lldb-dap.
#[test]
#[ignore = "requires cargo and lldb-dap"]
fn a_launch_json_rust_config_builds_first_and_passes_args_and_env() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    let n: i32 = std::env::args().nth(1).unwrap().parse().unwrap();\n    \
         let k: i32 = std::env::var(\"K\").unwrap().parse().unwrap();\n    \
         println!(\"{}\", n + k);\n}\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".vscode")).unwrap();
    std::fs::write(
        root.join(".vscode/tasks.json"),
        r#"{ "version": "2.0.0", "tasks": [
            { "label": "cargo build", "type": "shell", "command": "cargo build" }
        ]}"#,
    )
    .unwrap();
    std::fs::write(
        root.join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "name": "Rs", "type": "lldb", "request": "launch",
              "program": "${workspaceFolder}/target/debug/t", "args": ["35"],
              "env": { "K": "7" }, "cwd": "${workspaceFolder}",
              "preLaunchTask": "cargo build", "sourceLanguages": ["rust"] }
        ]}"#,
    )
    .unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor
        .breakpoints
        .entry(root.join("src/main.rs"))
        .or_default()
        .insert(4);
    start_debug_config(&mut app, "Rs");
    assert!(
        app.pending_debug_launch.is_some(),
        "the launch waits for the build: {}",
        app.status
    );
    let k = debug_until_local(&mut app, "k", 240);
    let n = app
        .debug_sessions
        .focused()
        .and_then(|s| s.lookup_local("n"))
        .map(|v| v.value.clone());
    app.debug_stop();
    assert!(
        root.join("target/debug/t").is_file(),
        "the preLaunchTask built the binary"
    );
    assert_eq!(
        k.as_deref(),
        Some("7"),
        "status {:?}, feedback {:?}",
        app.status,
        app.run_debug.feedback
    );
    assert_eq!(n.as_deref(), Some("35"));
}

/// #250, against a real vscode-js-debug: a `request: attach` config with a
/// `port` attaches to a running Node server and stops at a breakpoint in
/// it. Needs node and `~/.croft/js-debug`.
#[test]
#[ignore = "requires node and ~/.croft/js-debug"]
fn a_launch_json_node_attach_config_attaches_by_port() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::write(
        root.join("server.js"),
        "let n = 0;\nsetInterval(() => {\n  n += 1;\n  const label = `tick ${n}`;\n  if (label === '') console.log(label);\n}, 100);\n",
    )
    .unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut node = std::process::Command::new("node")
        .arg(format!("--inspect=127.0.0.1:{port}"))
        .arg(root.join("server.js"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("node runs");
    std::fs::create_dir_all(root.join(".vscode")).unwrap();
    std::fs::write(
        root.join(".vscode/launch.json"),
        format!(
            r#"{{ "configurations": [
                {{ "name": "Attach", "type": "node", "request": "attach",
                  "port": {port}, "skipFiles": ["<node_internals>/**"] }}
            ]}}"#
        ),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let mut app = App::new(root.clone()).unwrap();
    app.editor
        .breakpoints
        .entry(root.join("server.js"))
        .or_default()
        .insert(5);
    start_debug_config(&mut app, "Attach");
    let label = debug_until_local(&mut app, "label", 90);
    app.debug_stop();
    let _ = node.kill();
    let _ = node.wait();
    let label = label.unwrap_or_else(|| {
        panic!(
            "no stop: status {:?}, feedback {:?}",
            app.status, app.run_debug.feedback
        )
    });
    assert!(label.contains("tick "), "{label}");
}

/// Feed `events` (DAP `output` bodies' `output` strings, JSON-escaped) to a
/// stub debug session and poll until `done` holds for the Debug Console.
fn debug_console_after(outputs: &[&str], done: impl Fn(&App) -> bool) -> App {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let events: Vec<String> = outputs
        .iter()
        .enumerate()
        .map(|(i, out)| {
            format!(
                r#"{{"seq":{},"type":"event","event":"output","body":{{"category":"stdout","output":"{out}"}}}}"#,
                i + 1
            )
        })
        .collect();
    let refs: Vec<&str> = events.iter().map(String::as_str).collect();
    app.debug_sessions.push("P", stub_emitting(&refs));
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the program's output to reach the Debug Console",
        || {
            app.poll_dap();
            done(&app)
        },
    );
    app.debug_stop();
    app
}

fn console_line<'a>(app: &'a App, text: &str) -> &'a crate::ansi_text::AnsiLine {
    app.debug_console
        .iter()
        .find(|l| l.text == text)
        .unwrap_or_else(|| panic!("no {text:?} in {:?}", app.debug_console))
}

/// The colour at byte `at` of a console line.
fn console_fg(line: &crate::ansi_text::AnsiLine, at: usize) -> Option<crate::ansi_text::AnsiColor> {
    line.spans
        .iter()
        .find(|s| s.start <= at && at < s.end)
        .and_then(|s| s.style.fg)
}

/// #1501: pytest `--color=yes`, chalk and coloured logging printed their SGR
/// sequences into the Debug Console as `[31mFAILED[0m`. The console keeps the
/// colours as spans over the plain text, and OUTPUT's channel gets the text.
#[test]
fn the_debug_console_shows_colour_codes_as_colours() {
    let tag = format!("ansi-{}", std::process::id());
    let printed = format!("FAILED tests/test_a.py::test_x - {tag}");
    let app = debug_console_after(
        &[&format!(
            r"\u001b[31mFAILED\u001b[0m tests/test_a.py::\u001b[1mtest_x\u001b[0m - {tag}\n"
        )],
        |a| a.debug_console.iter().any(|l| l.text.contains(&tag)),
    );
    let line = console_line(&app, &printed);
    use crate::ansi_text::AnsiColor;
    assert_eq!(
        console_fg(line, 0),
        Some(AnsiColor::Indexed(1)),
        "FAILED is red"
    );
    assert_eq!(console_fg(line, 7), None, "the reset ends it");
    let bold = line
        .spans
        .iter()
        .find(|s| &line.text[s.start..s.end] == "test_x")
        .expect("test_x is its own run");
    assert!(bold.style.bold, "{:?}", line.spans);
    assert!(
        crate::output::snapshot(crate::output::CHANNEL_DEBUG_CONSOLE)
            .unwrap_or_default()
            .iter()
            .any(|l| l.text == printed),
        "OUTPUT's Debug Console channel gets the plain text"
    );
}

/// #1501: a colour set on one line stays on until reset, as in a terminal,
/// and a sequence cut between two output events is joined, not printed.
#[test]
fn debug_console_colour_carries_across_lines_and_cut_events() {
    let tag = format!("carry-{}", std::process::id());
    let app = debug_console_after(
        &[r"\u001b[3", &format!(r"2m{tag} ok\nstill\u001b[0m plain\n")],
        |a| a.debug_console.iter().any(|l| l.text.contains("plain")),
    );
    use crate::ansi_text::AnsiColor;
    let first = console_line(&app, &format!("{tag} ok"));
    assert_eq!(console_fg(first, 0), Some(AnsiColor::Indexed(2)));
    let second = console_line(&app, "still plain");
    assert_eq!(
        console_fg(second, 0),
        Some(AnsiColor::Indexed(2)),
        "carried"
    );
    assert_eq!(console_fg(second, 6), None, "until the reset");
    assert!(
        !app.debug_console
            .iter()
            .any(|l| l.text.contains('\u{1b}') || l.text.contains("[3")),
        "{:?}",
        app.debug_console
    );
}

/// #1501 negative: text without escapes is untouched, brackets included, and
/// croft's own console lines never inherit a colour the program left on.
#[test]
fn debug_console_plain_output_and_croft_lines_stay_plain() {
    use crate::dap::session::DapEvent;
    let tag = format!("plain-{}", std::process::id());
    let mut app = debug_console_after(
        &[&format!(r"[31m is just text {tag}\n\u001b[35mleft on\n")],
        |a| a.debug_console.iter().any(|l| l.text.ends_with("left on")),
    );
    let plain = console_line(&app, &format!("[31m is just text {tag}"));
    assert_eq!(
        plain.spans.iter().map(|s| s.style).collect::<Vec<_>>(),
        vec![crate::ansi_text::AnsiStyle::default()]
    );
    app.debug_sessions.push("Q", stub_emitting(&[]));
    let (_, q) = app
        .debug_sessions
        .iter_named_mut_indexed()
        .find(|(i, _)| *i == 0)
        .unwrap();
    q.backlog.push(DapEvent::Evaluated {
        context: String::from("repl"),
        expression: String::from("1 + 1"),
        result: String::from("2"),
        success: true,
    });
    app.poll_dap();
    app.debug_stop();
    let echo = console_line(&app, "❯ 1 + 1");
    assert_eq!(console_fg(echo, 0), None, "{:?}", echo.spans);
    assert_eq!(console_fg(console_line(&app, "2"), 0), None);
}

/// #1501: each debug session's output is its own ANSI stream. In a compound,
/// session A ends an event mid-escape (`ESC[3`) with red left on; session B's
/// next line must neither lose its first letter to A's escape nor turn red.
#[test]
fn debug_console_sessions_keep_their_own_colour_and_cut_escape() {
    let tag = format!("streams-{}", std::process::id());
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let a_event = format!(
        r#"{{"seq":1,"type":"event","event":"output","body":{{"category":"stdout","output":"\u001b[31mred from A {tag}\n\u001b[3"}}}}"#
    );
    app.debug_sessions.push("A", stub_emitting(&[&a_event]));
    let a_line = format!("red from A {tag}");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "session A's output to reach the Debug Console",
        || {
            app.poll_dap();
            app.debug_console.iter().any(|l| l.text == a_line)
        },
    );
    // A is now in the background with red on and `ESC[3` held back.
    let b_event = format!(
        r#"{{"seq":1,"type":"event","event":"output","body":{{"category":"stdout","output":"hello from B {tag}\n"}}}}"#
    );
    app.debug_sessions.push("B", stub_emitting(&[&b_event]));
    let b_line = format!("hello from B {tag}");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "session B's output to reach the Debug Console",
        || {
            app.poll_dap();
            app.debug_console
                .iter()
                .any(|l| l.text.contains(&format!("from B {tag}")))
        },
    );
    app.debug_stop();
    let b = console_line(&app, &b_line);
    assert_eq!(console_fg(b, 0), None, "A's red stays in A: {:?}", b.spans);
    use crate::ansi_text::AnsiColor;
    assert_eq!(
        console_fg(console_line(&app, &a_line), 0),
        Some(AnsiColor::Indexed(1)),
        "A's own line is red"
    );
}

/// #1391: a node launch with nothing to run is refused naming every field
/// that would have given it something.
#[test]
fn a_node_launch_with_nothing_to_run_names_what_it_needs() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [ { "name": "Bare", "type": "node", "request": "launch" } ] }"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let cfg = crate::dap::configs::discover_configs(tmp.path()).remove(0);
    app.launch_debug_config(&cfg);
    assert!(app.run_debug.feedback_is_error);
    assert!(
        app.status.contains(
            "config \"Bare\": a node launch needs a \"program\", a \"runtimeExecutable\" or a \"command\""
        ),
        "{}",
        app.status
    );
    assert!(app.debug_sessions.is_empty());
}

/// #1410: a browser croft launches has a screen to open on only locally
/// with a display; over SSH, or on Linux with no X or Wayland display, it
/// runs headless.
#[test]
fn a_browser_runs_headless_only_where_there_is_no_display() {
    let none = |_: &str| false;
    let x11 = |k: &str| k == "DISPLAY";
    let wayland = |k: &str| k == "WAYLAND_DISPLAY";
    assert!(crate::app::no_display(true, true, x11), "over SSH");
    assert!(
        crate::app::no_display(false, true, none),
        "Linux, no display"
    );
    assert!(!crate::app::no_display(false, true, x11));
    assert!(!crate::app::no_display(false, true, wayland));
    assert!(
        !crate::app::no_display(false, false, none),
        "macOS has a display with no DISPLAY set"
    );
}

/// #1392: a CodeLLDB `cargo` launch with no `program` builds with cargo
/// first instead of being refused; an lldb launch with neither is still
/// refused.
#[test]
fn a_cargo_launch_builds_first_and_a_bare_one_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "type": "lldb", "request": "launch", "name": "Debug executable 'rc'",
              "cargo": { "args": ["build", "--bin=rc", "--package=rc"],
                         "filter": { "name": "rc", "kind": "bin" } },
              "cwd": "${workspaceFolder}" },
            { "type": "lldb", "request": "launch", "name": "Bare" }
        ]}"#,
    )
    .unwrap();
    let launch = |name: &str| {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let cfg = crate::dap::configs::discover_configs(tmp.path())
            .into_iter()
            .find(|c| c.name == name)
            .unwrap();
        app.launch_debug_config(&cfg);
        app
    };
    let app = launch("Debug executable 'rc'");
    assert!(
        !app.pending_cargo_launches.is_empty() && !app.run_debug.feedback_is_error,
        "{}",
        app.status
    );
    assert!(
        app.status
            .contains("building with cargo build --bin=rc --package=rc"),
        "{}",
        app.status
    );
    let app = launch("Bare");
    assert!(app.pending_cargo_launches.is_empty());
    assert!(app.run_debug.feedback_is_error);
    assert!(
        app.status.contains("an lldb launch needs a \"program\""),
        "{}",
        app.status
    );
}

/// A CMake project whose `main.cpp` needs its include folder and a second
/// translation unit (#1359), as the issue's fixture.
fn cmake_debug_project(root: &Path) -> PathBuf {
    for (rel, text) in [
        (
            "include/geom.hpp",
            "#pragma once\nnamespace geom { double twice(double); }\n",
        ),
        (
            "src/geom.cpp",
            "#include \"geom.hpp\"\nnamespace geom { double twice(double x) { return 2*x; } }\n",
        ),
        (
            "src/main.cpp",
            "#include <cstdio>\n#include \"geom.hpp\"\nint main() { std::printf(\"%f\\n\", geom::twice(2)); }\n",
        ),
        (
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.10)\nproject(cp CXX)\nset(CMAKE_EXPORT_COMPILE_COMMANDS ON)\nadd_executable(app src/main.cpp src/geom.cpp)\ntarget_include_directories(app PRIVATE include)\n",
        ),
    ] {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    root.join("src/main.cpp")
}

/// Wait out F5's C/C++ build (#1359).
fn finished_debug_build(
    app: &mut App,
) -> (
    PathBuf,
    Result<crate::cpp_build::Built, crate::cpp_build::Failed>,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    loop {
        if let Some(done) = app.take_debug_build() {
            return done;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the debug build never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// #1359: F5 on `src/main.cpp` in a configured CMake project builds the
/// project's target, off the UI thread, and debugs that program; it no
/// longer compiles `main.cpp` alone (which cannot find `geom.hpp`) or
/// leaves a `main.dbg` in `src/`.
#[test]
fn f5_in_a_cmake_project_debugs_the_target_that_builds_the_file() {
    use crate::lsp::manager::is_on_path;
    if !(is_on_path("cmake") && is_on_path("c++")) {
        return;
    }
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    let main = cmake_debug_project(&root);
    let configured = std::process::Command::new("cmake")
        .arg("-S")
        .arg(&root)
        .arg("-B")
        .arg(root.join("build"))
        .output()
        .unwrap();
    assert!(configured.status.success(), "{configured:?}");
    with_relay_home(tmp.path(), || {
        let mut app = App::new(root.clone()).unwrap();
        app.begin_debug_build(&main);
        assert_eq!(app.status, "Building main.cpp for debugging…");
        let (source, built) = finished_debug_build(&mut app);
        assert_eq!(source, main);
        let built = built.unwrap_or_else(|f| panic!("{}\n{}", f.summary, f.output));
        assert_eq!(
            built.binary.canonicalize().unwrap(),
            root.join("build/app").canonicalize().unwrap()
        );
        assert!(built.how.starts_with("cmake --build"), "{}", built.how);
        assert!(built.how.ends_with("(target app)"), "{}", built.how);
        let ran = std::process::Command::new(&built.binary).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&ran.stdout).trim(), "4.000000");
        assert!(!root.join("src/main.dbg").exists(), "nothing lands in src/");
    });
}

/// #1359, negative: a build that fails says so on the status line and keeps
/// the compiler's whole output in OUTPUT, and a new F5 drops a build still
/// running rather than launching it later.
#[test]
fn a_failed_debug_build_says_why_and_keeps_its_output() {
    use crate::lsp::manager::is_on_path;
    if !is_on_path("c++") {
        return;
    }
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    // Not configured: no build folder, so main.cpp builds alone and
    // cannot find geom.hpp.
    let main = cmake_debug_project(&root);
    with_relay_home(tmp.path(), || {
        let mut app = App::new(root.clone()).unwrap();
        app.begin_debug_build(&main);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while !app.poll_debug_build() {
            assert!(
                std::time::Instant::now() < deadline,
                "the build never ended"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            app.status.starts_with("Build for debug failed: c++ failed"),
            "{}",
            app.status
        );
        assert!(app.status.contains("geom.hpp"), "{}", app.status);
        assert!(
            app.status
                .ends_with("(full output in OUTPUT › Debug Build)")
        );
        assert!(app.run_debug.feedback_is_error);
        let lines = crate::output::snapshot("Debug Build").expect("the build's output is kept");
        assert!(lines.iter().any(|l| l.text.contains("geom.hpp")));
        assert!(!root.join("src/main.dbg").exists());
        assert!(!app.poll_debug_build(), "a finished build reports once");

        app.begin_debug_build(&main);
        app.editor.path = None;
        app.start_debug_session();
        assert!(
            app.debug_build.is_none(),
            "a new F5 drops the running build"
        );
        assert_eq!(app.status, "Open a file to debug");

        // Stop while the build runs: it never launches.
        app.begin_debug_build(&main);
        app.debug_stop_by_user();
        assert!(app.debug_build.is_none(), "Stop drops the running build");
        assert!(!app.poll_debug_build());
    });
}

/// #1359: F5 on a C++ file in an added workspace folder builds that
/// folder's CMake project, not the primary root's.
#[test]
fn f5_builds_the_project_of_the_files_own_workspace_folder() {
    use crate::lsp::manager::is_on_path;
    if !(is_on_path("cmake") && is_on_path("c++")) {
        return;
    }
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    std::fs::create_dir_all(&primary).unwrap();
    let second = tmp.path().join("second");
    let main = cmake_debug_project(&second);
    let configured = std::process::Command::new("cmake")
        .arg("-S")
        .arg(&second)
        .arg("-B")
        .arg(second.join("build"))
        .output()
        .unwrap();
    assert!(configured.status.success(), "{configured:?}");
    with_relay_home(tmp.path(), || {
        let mut app = App::new(primary.clone()).unwrap();
        app.roots.add(second.clone());
        app.begin_debug_build(&main);
        let (_, built) = finished_debug_build(&mut app);
        let built = built.unwrap_or_else(|f| panic!("{}\n{}", f.summary, f.output));
        assert_eq!(
            built.binary.canonicalize().unwrap(),
            second.join("build/app").canonicalize().unwrap()
        );
    });
}

/// #1392: Stop Debugging while cargo is still building drops the pending
/// launch, so the debuggee does not start after the user stopped; and a
/// compound with two cargo members queues both builds instead of keeping
/// only the last and calling the set a failure.
#[test]
fn cargo_launches_queue_per_compound_member_and_stop_drops_them() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "type": "lldb", "request": "launch", "name": "A",
              "cargo": { "args": ["build", "--bin=a"] } },
            { "type": "lldb", "request": "launch", "name": "B",
              "cargo": { "args": ["build", "--bin=b"] } }
        ],
        "compounds": [ { "name": "Both", "configurations": ["A", "B"] } ] }"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let compound = crate::dap::configs::discover_compounds(tmp.path())
        .into_iter()
        .find(|c| c.name == "Both")
        .unwrap();
    app.launch_compound(&compound);
    assert_eq!(app.pending_cargo_launches.len(), 2, "{}", app.status);
    assert!(!app.run_debug.feedback_is_error, "{}", app.status);
    assert!(app.status.contains("building A, B"), "{}", app.status);
    app.debug_stop_by_user();
    assert!(app.pending_cargo_launches.is_empty());
}
