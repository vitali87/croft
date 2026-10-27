//! The window chrome: side bars, the activity bar, panels, the status bar,
//! Zen Mode, splitters and Customize Layout.

use super::*;

/// Negative for #1133: two rows, and a normal size after one row, still
/// draw the bottom panel's tab strip where it was.
#[test]
fn the_panel_tab_strip_still_paints_once_there_is_room() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 2)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let rows: Vec<String> = (0..30)
        .map(|y| (0..100).map(|x| buf[(x, y)].symbol().to_string()).collect())
        .collect();
    assert!(
        rows.iter()
            .any(|r| r.contains("PROBLEMS") && r.contains("TERMINAL")),
        "{rows:#?}"
    );
}

/// #1132: editor groups that can't all sit at their minimum width ran past
/// the frame, and the tab strip painted the first column outside it.
#[test]
fn splitting_the_editor_in_a_narrow_window_renders_without_panicking() {
    let tmp = tempfile::tempdir().unwrap();
    let draw = |app: &mut App, w: u16, h: u16| {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    };
    let mut app = app_with_open_file(tmp.path(), "api.py", "def f():\n    pass\n");
    draw(&mut app, 80, 24);
    for _ in 0..4 {
        app.split_editor();
        draw(&mut app, 80, 24);
    }
    assert_eq!(app.editor_layout.leaf_count(), 5);
    draw(&mut app, 60, 20);
    draw(&mut app, 22, 8);

    let mut two = app_with_open_file(tmp.path(), "b.py", "x = 1\n");
    two.split_editor();
    draw(&mut two, 22, 8);
    draw(&mut two, 20, 8);
}

/// #1132 (comment): the secondary side bar's OUTLINE scrollbar at 30x9.
#[test]
fn a_secondary_side_bar_in_a_narrow_window_renders_without_panicking() {
    let tmp = tempfile::tempdir().unwrap();
    let src: String = (0..80)
        .map(|i| format!("def f{i}():\n    pass\n"))
        .collect();
    let mut app = app_with_open_file(tmp.path(), "sessions.py", &src);
    app.toggle_secondary_side_bar();
    for (w, h) in [(120, 35), (30, 9), (30, 24), (12, 9)] {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    }
}

#[test]
fn resting_on_an_activity_icon_arms_its_button_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Render once so the activity-bar icon rectangles are populated.
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let icon = app.sidebar_areas.search_icon;
    let (cx, cy) = (icon.x + icon.width / 2, icon.y);
    app.on_mouse_moved(cx, cy, false);
    assert_eq!(
        app.ui_tooltip_label,
        Some("Search"),
        "hovering the Search activity-bar icon arms its hint label"
    );
    // Moving onto inert space clears the armed hint.
    app.on_mouse_moved(cx, icon.y + 200, false);
    assert_eq!(app.ui_tooltip_label, None);
}

#[test]
fn chrome_tooltip_paints_in_the_sidebar_not_clamped_into_the_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A compact hint anchored over an activity-bar cell (far-left column).
    app.ui_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new_compact(
        "Search".into(),
        (2, 6),
    ));
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    // The popup's own background must appear in the far-left region near the
    // anchor. The regression: it rendered against the editor's focused_area,
    // which starts well to the right, so nothing painted over the sidebar.
    let popup_bg = Color::Rgb(0x1e, 0x21, 0x2a);
    let painted = (0..20).any(|x| (0..12).any(|y| buf[(x, y)].bg == popup_bg));
    assert!(
        painted,
        "the chrome tooltip must paint near its sidebar anchor, not be clamped into the editor"
    );
    // ...and it must NOT paint over the activity-bar columns, whose inline-image
    // icons composite above text and would hide the tooltip's left edge.
    let over_bar = (0..ACTIVITY_BAR_WIDTH).any(|x| (0..50).any(|y| buf[(x, y)].bg == popup_bg));
    assert!(
        !over_bar,
        "the tooltip must be shifted clear of the activity-bar image column"
    );
}

#[test]
fn sidebar_header_actions_are_tooltipped_per_view() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let cell = |x| Rect {
        x,
        y: 1,
        width: 2,
        height: 1,
    };

    // Explorer toolbar (the icons in Image #1: new file / new folder / refresh /
    // collapse, plus the ⋯ views menu).
    app.sidebar_view = SidebarView::Explorer;
    app.tree.header_new_file_btn = cell(20);
    app.tree.header_new_folder_btn = cell(23);
    app.tree.header_refresh_btn = cell(26);
    app.tree.header_collapse_btn = cell(29);
    app.tree.header_views_btn = cell(32);
    assert_eq!(app.ui_tooltip_at(20, 1), Some("New File"));
    assert_eq!(app.ui_tooltip_at(23, 1), Some("New Folder"));
    assert_eq!(app.ui_tooltip_at(26, 1), Some("Refresh Explorer"));
    assert_eq!(
        app.ui_tooltip_at(29, 1),
        Some("Collapse Folders in Explorer")
    );
    assert_eq!(app.ui_tooltip_at(32, 1), Some("Views and More Actions"));

    // Remote Explorer header (the + / refresh in Image #2/#3).
    app.sidebar_view = SidebarView::Remote;
    app.remote.header_add_btn = cell(20);
    app.remote.header_refresh_btn = cell(23);
    assert_eq!(
        app.ui_tooltip_at(20, 1),
        Some("Open SSH Configuration File")
    );
    assert_eq!(app.ui_tooltip_at(23, 1), Some("Refresh"));

    // Source Control header refresh.
    app.sidebar_view = SidebarView::SourceControl;
    app.source_control.header_refresh_btn = cell(20);
    assert_eq!(app.ui_tooltip_at(20, 1), Some("Refresh"));

    // A view's stored rects go inert once another view is showing, so the
    // Remote add button at col 20 must not leak its hint into Explorer.
    app.sidebar_view = SidebarView::Explorer;
    assert_eq!(app.ui_tooltip_at(23, 1), Some("New Folder"));
}

#[test]
fn a_second_tap_on_the_same_cell_rearms_the_dwell() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, col, row) = app_with_open_file_and_editor_cell();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    app.hover.mark_fired();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert!(
        app.hover
            .due(std::time::Instant::now() + HOVER_DELAY, HOVER_DELAY),
        "a fresh tap must restart the dwell even on the same cell, else a \
         dismissed popup could never be re-summoned by tapping again"
    );
}

/// Negative: opening a location from a panel (a search hit, a problem) is
/// not a code navigation from the tab, so a preview tab is still replaced
/// as before.
#[test]
fn a_panel_open_still_replaces_the_preview_tab() {
    let (_tmp, mut app, mc, main, mh) = preview_nav_fixture();
    app.editor.open_preview(&main).unwrap();
    app.open_at(&mh, 1, 0).unwrap();
    assert_eq!(open_tab_paths(&app), vec![mc, mh]);
}

#[test]
fn ctrl_j_clears_maximize_when_hiding_terminal() {
    // Mental contract: Ctrl+J means "give me my normal layout back."
    // Hiding the terminal must clear the maximize flag so the next
    // Ctrl+J reopen restores the editor+terminal split, not a stuck
    // maximized terminal.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let max_chord = key(
        KeyCode::Char('J'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    let toggle_chord = key(KeyCode::Char('j'), KeyModifiers::CONTROL);
    app.handle_key(max_chord).unwrap();
    assert!(app.terminal_maximized);
    app.handle_key(toggle_chord).unwrap();
    assert!(!app.show_terminal, "Ctrl+J hides the terminal");
    assert!(
        !app.terminal_maximized,
        "hiding the terminal also clears maximize"
    );
    app.handle_key(toggle_chord).unwrap();
    assert!(app.show_terminal, "Ctrl+J reshows the terminal");
    assert!(
        !app.terminal_maximized,
        "reopened terminal stays in normal split, not maximized"
    );
}

#[test]
fn status_bar_shows_document_facts_not_a_keyboard_cheat_sheet() {
    // The redesigned bar drops the `^q Quit / ^j Term / …` cheat-sheet for a
    // VS Code-style right cluster of document facts.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.path = Some(std::path::PathBuf::from("x.rs"));
    app.editor
        .set_language(Some(crate::highlight::LangKind::Rust));
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let last_y = buf.area.height - 1;
    let row: String = (0..buf.area.width)
        .map(|x| buf[(x, last_y)].symbol())
        .collect();
    assert!(
        row.contains("Ln 1, Col 1"),
        "status bar shows cursor position: {row:?}"
    );
    assert!(
        row.contains("Rust"),
        "status bar shows language mode: {row:?}"
    );
    assert!(
        !row.contains("Cycle pane") && !row.contains("Quit"),
        "the keyboard cheat-sheet must be gone: {row:?}"
    );
}

#[test]
fn status_bar_indent_menu_action_pins_the_editor_indent_style() {
    use crate::widgets::editor::IndentStyle;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Default is spaces; the pill click routes through SetIndentStyle to tabs.
    app.dispatch_menu_action(
        MenuAction::SetIndentStyle(IndentStyle {
            width: 4,
            use_spaces: false,
        }),
        tmp.path().to_path_buf(),
    );
    assert_eq!(
        app.editor.indent_style(),
        IndentStyle {
            width: 4,
            use_spaces: false,
        }
    );
    assert_eq!(app.editor.indent_style().label(), "Tab Size: 4");
}

/// Guard (#843): the shell keeps `Ctrl`+`K` only while it has the keyboard.
/// With the bottom panel on PROBLEMS the focused panel is not a shell, so
/// `Ctrl`+`K` leads there as in any other pane.
#[cfg(not(target_os = "macos"))]
#[test]
fn ctrl_k_leads_from_the_panel_while_it_shows_problems() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.bottom_panel_tab = BottomPanelTab::Problems;
    app.focus_pane(Pane::Terminal);
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.cmd_k_leader.is_some());
}

#[test]
fn resize_arms_a_one_shot_terminal_clear_to_evict_stale_activity_icons() {
    // Regression for the activity-bar icon ghosting after a pane reshape.
    // iTerm2 keeps OSC-1337 image cells in a layer that survives plain SGR
    // overwrites, so on resize the previously-painted icon bitmaps linger and
    // the re-emit stamps a second copy on top (the doubled Run/Debug icon).
    // A resize must arm the same one-shot `terminal.clear()` the view-change
    // path uses, so iTerm2 drops the stale layer before the icons re-emit.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Pretend init_graphics baked the activity-bar icon images — images mode
    // is the only state in which the stale layer can exist.
    app.overlays.activity.set_images(ActivityBarImages {
        explorer_active: String::new(),
        explorer_inactive: String::new(),
        explorer_hovered: String::new(),
        search_active: String::new(),
        search_inactive: String::new(),
        search_hovered: String::new(),
        source_control_active: String::new(),
        source_control_inactive: String::new(),
        source_control_hovered: String::new(),
        remote_active: String::new(),
        remote_inactive: String::new(),
        remote_hovered: String::new(),
        run_debug_active: String::new(),
        run_debug_inactive: String::new(),
        run_debug_hovered: String::new(),
        extensions_active: String::new(),
        extensions_inactive: String::new(),
        extensions_hovered: String::new(),
        testing_active: String::new(),
        testing_inactive: String::new(),
        testing_hovered: String::new(),
        codeql_active: String::new(),
        codeql_inactive: String::new(),
        codeql_hovered: String::new(),
        sponsor_inactive: String::new(),
        sponsor_hovered: String::new(),
        settings_active: String::new(),
        settings_inactive: String::new(),
        settings_hovered: String::new(),
        layout_sidebar_on: String::new(),
        layout_sidebar_on_hover: String::new(),
        layout_sidebar_off: String::new(),
        layout_sidebar_off_hover: String::new(),
        layout_panel_on: String::new(),
        layout_panel_on_hover: String::new(),
        layout_panel_off: String::new(),
        layout_panel_off_hover: String::new(),
        layout_customize: String::new(),
        layout_customize_hover: String::new(),
    });
    app.on_resize();
    assert!(
        app.consume_activity_image_clear(),
        "a pane resize must arm exactly one terminal.clear() so iTerm2 evicts the stale OSC-1337 activity-bar image layer",
    );
    assert!(
        !app.consume_activity_image_clear(),
        "the clear request must be one-shot — consuming twice in a row returns false",
    );
    assert!(
        app.overlays.activity.dirty(),
        "resize must also re-mark the icons dirty so the post-draw flush re-emits them after the clear",
    );
}

#[test]
fn activity_bar_icons_moving_within_the_flush_arms_a_one_shot_terminal_clear() {
    // Same emit-time move-detection the run-debug headline icon uses, applied
    // to the activity bar: when the icon cells shift since the last emit, the
    // post-draw flush must arm one terminal.clear() to evict iTerm2's stale
    // OSC-1337 image layer instead of stacking a fresh copy on top (the
    // doubled-icon ghost). This is trigger-agnostic — it fires for any layout
    // shift, not only the Event::Resize path that on_resize covers.
    use ratatui::layout::Rect;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(ActivityBarImages {
        explorer_active: String::new(),
        explorer_inactive: String::new(),
        explorer_hovered: String::new(),
        search_active: String::new(),
        search_inactive: String::new(),
        search_hovered: String::new(),
        source_control_active: String::new(),
        source_control_inactive: String::new(),
        source_control_hovered: String::new(),
        remote_active: String::new(),
        remote_inactive: String::new(),
        remote_hovered: String::new(),
        run_debug_active: String::new(),
        run_debug_inactive: String::new(),
        run_debug_hovered: String::new(),
        extensions_active: String::new(),
        extensions_inactive: String::new(),
        extensions_hovered: String::new(),
        testing_active: String::new(),
        testing_inactive: String::new(),
        testing_hovered: String::new(),
        codeql_active: String::new(),
        codeql_inactive: String::new(),
        codeql_hovered: String::new(),
        sponsor_inactive: String::new(),
        sponsor_hovered: String::new(),
        settings_active: String::new(),
        settings_inactive: String::new(),
        settings_hovered: String::new(),
        layout_sidebar_on: String::new(),
        layout_sidebar_on_hover: String::new(),
        layout_sidebar_off: String::new(),
        layout_sidebar_off_hover: String::new(),
        layout_panel_on: String::new(),
        layout_panel_on_hover: String::new(),
        layout_panel_off: String::new(),
        layout_panel_off_hover: String::new(),
        layout_customize: String::new(),
        layout_customize_hover: String::new(),
    });
    let place = |app: &mut App, base: u16| {
        app.sidebar_areas.explorer_icon = Rect::new(0, base, 2, 1);
        app.sidebar_areas.search_icon = Rect::new(0, base + 2, 2, 1);
        app.sidebar_areas.source_control_icon = Rect::new(0, base + 4, 2, 1);
        app.sidebar_areas.remote_icon = Rect::new(0, base + 6, 2, 1);
        app.sidebar_areas.run_debug_icon = Rect::new(0, base + 8, 2, 1);
        app.sidebar_areas.extensions_icon = Rect::new(0, base + 10, 2, 1);
        app.sidebar_areas.testing_icon = Rect::new(0, base + 12, 2, 1);
    };
    // First emit at one layout: records the positions, arms no clear.
    place(&mut app, 2);
    app.overlays.activity.mark_dirty();
    app.flush_activity_image_overlays(&[]);
    assert!(
        !app.consume_activity_image_clear(),
        "the first emit has no prior positions to compare, so it must not arm a clear",
    );
    // A resize recenters the bar — every icon row shifts by one.
    place(&mut app, 3);
    app.overlays.activity.mark_dirty();
    app.flush_activity_image_overlays(&[]);
    assert!(
        app.consume_activity_image_clear(),
        "icons that moved since the last emit must arm one terminal.clear() to evict the stale image layer",
    );
    assert!(
        !app.consume_activity_image_clear(),
        "the clear request must be one-shot",
    );
}

/// #1356: the status bar and the git status name the operation and, for
/// a rebase, the step.
#[test]
fn the_status_bar_names_the_operation_in_progress() {
    let tmp = diverged_repo();
    let p = tmp.path();
    assert!(!git_succeeds(p, &["merge", "feat"]));
    let status = crate::git::query(p);
    let text: String = git_status_spans(&status)
        .iter()
        .map(|s| s.content.to_string())
        .collect();
    assert!(text.contains("main") && text.contains("MERGING"), "{text}");
    assert!(git_succeeds(p, &["merge", "--abort"]));
    assert!(git_succeeds(p, &["checkout", "-q", "feat"]));
    assert!(!git_succeeds(p, &["rebase", "main"]));
    let status = crate::git::query(p);
    let text: String = git_status_spans(&status)
        .iter()
        .map(|s| s.content.to_string())
        .collect();
    assert!(text.contains("REBASING 1/1"), "{text}");
}

#[test]
fn activity_icons_stay_visible_beside_the_centered_shortcuts_modal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(dummy_activity_images());
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let before = app.pending_activity_image_overlays().len();
    assert_eq!(
        before, 12,
        "precondition: seven view icons (CodeQL's is added, not built in) + the sponsor heart + the settings gear + the three layout toolbar icons emit"
    );
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let during = app.pending_activity_image_overlays().len();
    assert_eq!(
        during, before,
        "the shortcuts modal is centered over the editor column, so the far-left activity-bar icons it does not cover must keep re-emitting — otherwise the terminal.clear() armed by F1 wipes iTerm2's cached cells and the icons vanish for the whole time the modal is up; saw {before} icons before opening"
    );
}

#[test]
fn activity_icons_behind_a_modal_that_reaches_the_left_edge_are_suppressed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(dummy_activity_images());
    let backend = ratatui::backend::TestBackend::new(40, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let before = app.pending_activity_image_overlays().len();
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let during = app.pending_activity_image_overlays().len();
    assert!(
        during < before || before == 0,
        "on a terminal narrow enough that the centered modal reaches column 0, icons whose blocks intersect the modal must be suppressed so they don't ghost over the modal text; saw {before} icons before, {during} during"
    );
}

/// Issue #225: the selected activity-bar icon must stay visible on the light
/// theme, and stay distinguishable from the unselected ones. Renders the bar
/// (glyph fallback path) under Croft Light and reads the real cells: the
/// selected Explorer glyph carries VS Code's `activityBar.foreground`, an
/// unselected glyph the lighter `inactiveForeground`, and neither is the bar
/// background.
#[test]
fn the_selected_activity_icon_is_visible_on_the_light_theme() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let light = crate::theme::Theme::from_id("light");
    app.apply_theme(light);
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let buf = term.backend().buffer();
    let cell_of = |glyph: char| {
        (0..40u16)
            .flat_map(|y| (0..4u16).map(move |x| (x, y)))
            .find(|&(x, y)| buf[(x, y)].symbol() == glyph.to_string())
            .map(|(x, y)| buf[(x, y)].clone())
            .unwrap_or_else(|| panic!("the activity bar must paint {glyph:?}"))
    };
    // Explorer is the sidebar view every workspace opens on, so it is the
    // selected icon here; Search is not.
    let selected = cell_of(crate::icons::ACTIVITY_EXPLORER);
    let resting = cell_of(crate::icons::ACTIVITY_SEARCH);
    assert_eq!(
        selected.fg,
        light.activity_icon_active(),
        "the selected icon wears activityBar.foreground, not the white that vanishes on a light bar"
    );
    assert_eq!(resting.fg, light.activity_icon_inactive());
    assert_ne!(
        selected.fg, resting.fg,
        "selection must be readable from the icon itself, not only from the pill"
    );
    assert_ne!(
        selected.fg, selected.bg,
        "the selected glyph must not be painted in the bar's own background"
    );
}

/// Croft Loch paints the workbench as surfaces on exactly the layout every
/// other theme gets: the same side bar and tree rects as Croft Black, no
/// line box in the side bar or the panel, the side bar and status bar on
/// chrome, and a hairline seam where the side bar meets the editor column.
#[test]
fn croft_loch_paints_surfaces_on_croft_blacks_layout() {
    let render = |id: &str| {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.apply_theme(crate::theme::Theme::from_id(id));
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|frame| app.render(frame)).unwrap();
        let buf = term.backend().buffer().clone();
        let side = app.last_sidebar_area.expect("the side bar is shown");
        let panel = app.terminals[0].last_area;
        (side, app.tree.last_inner, panel, buf)
    };
    let (side, tree, panel, black) = render("black");
    let (loch_side, loch_tree, loch_panel, buf) = render("loch");
    assert_eq!((loch_side, loch_tree, loch_panel), (side, tree, panel));
    assert!(!panel.is_empty(), "the terminal panel is painted");
    let boxed = |r: ratatui::layout::Rect, b: &ratatui::buffer::Buffer| {
        (r.top()..r.bottom())
            .flat_map(|y| (r.left()..r.right()).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                "\u{250c}\u{2510}\u{2500}\u{2502}\u{2501}\u{2503}\u{256d}\u{256e}\u{2570}\u{256f}"
                    .contains(b[(x, y)].symbol())
            })
            .count()
    };
    assert!(boxed(side, &black) > 0 && boxed(panel, &black) > 0);
    assert_eq!(boxed(side, &buf), 0, "no box in Croft Loch's side bar");
    assert_eq!(boxed(panel, &buf), 0, "no box around Croft Loch's terminal");
    let s = crate::theme::Theme::from_id("loch").surfaces().unwrap();
    let seam = (side.top()..side.bottom())
        .map(|y| &buf[(side.right() - 1, y)])
        .filter(|c| c.symbol() == "\u{2595}" && c.fg == s.hairline())
        .count();
    assert!(
        seam > side.height as usize / 2,
        "a hairline down the side bar's edge"
    );
    assert_eq!(buf[(side.x + 2, side.bottom() - 2)].bg, s.chrome());
    assert_eq!(buf[(60, 39)].bg, s.chrome(), "the status bar is chrome");
}

#[test]
fn cmd_b_and_ctrl_b_toggle_the_primary_side_bar() {
    // VS Code's "Toggle Primary Side Bar" is Cmd+B on macOS (Ctrl+B on
    // Linux); croft accepts both so the chord behaves identically whether
    // iTerm2 forwards Cmd+B as a Super CSI-u sequence or a raw Ctrl+B byte
    // arrives over SSH. The sidebar starts visible.
    let mut app = editor_app_with_lines(&["x"]);
    assert!(app.show_tree, "sidebar starts visible");
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::SUPER))
        .unwrap();
    assert!(!app.show_tree, "Cmd+B hides the side bar");
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.show_tree, "Cmd+B again shows it");
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(!app.show_tree, "Ctrl+B toggles the same state");
}

#[test]
fn dragging_sidebar_splitter_resizes_sidebar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Simulate a render so splitter coords are populated.
    app.sidebar_splitter_x = Some(36); // activity(4) + sidebar(32) = 36
    app.last_content_width = 60;
    app.last_content_height = 20;
    app.sidebar_width = 32;
    // Mouse-down on the splitter column.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 36,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.splitter_drag, Some(SplitterDrag::Sidebar));
    // Drag right by 10 cells: column 46 -> sidebar should grow to 42.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 46,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.sidebar_width, 42);
    // Mouse-up clears the drag state.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 46,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert!(app.splitter_drag.is_none());
}

#[test]
fn sidebar_drag_grabs_one_column_left_of_seam() {
    // The seam is a single column wide. To make it easier to grab (and
    // to mirror the visible pair of borders the user sees), a click on
    // the cell immediately to its left also starts the drag.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_splitter_x = Some(36);
    app.last_content_width = 60;
    app.last_content_height = 20;
    app.sidebar_width = 32;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 35,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.splitter_drag, Some(SplitterDrag::Sidebar));
}

#[test]
fn sidebar_drag_clamps_to_minimum() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_splitter_x = Some(36);
    app.last_content_width = 60;
    app.last_content_height = 20;
    app.sidebar_width = 32;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 36,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    // Drag far to the left — sidebar should clamp to its min, not collapse.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.sidebar_width, SIDEBAR_WIDTH_MIN);
}

/// The same invariant through the PUBLIC gesture rather than the flag, so a
/// regression that stops `toggle_terminal_pane_maximize` reaching this state
/// is caught too. Setting `terminal_pane_maximized` directly pins the render
/// loop; this pins the command.
#[test]
fn the_maximize_gesture_also_drops_a_hidden_panes_last_inner() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    let backend = ratatui::backend::TestBackend::new(180, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let active = app.active_terminal;
    let hidden = (0..app.terminals.len()).find(|i| *i != active).unwrap();
    assert!(app.terminals[hidden].last_inner.width > 0, "precondition");

    app.toggle_terminal_pane_maximize();
    term.draw(|f| app.render(f)).unwrap();

    assert_eq!(
        app.terminals[hidden].last_inner,
        Rect::default(),
        "the gesture, not just the flag, must drop the hidden pane's rect"
    );
}

#[test]
fn reordering_a_maximized_pane_keeps_its_rect_with_the_terminal() {
    // `move_terminal` re-seats `last_area` in SLOT order, which is right for
    // the even split, where a slot's geometry is positional. Maximize mode has
    // exactly ONE live rect and it belongs to whichever pane was active at the
    // last PAINT — not to the dragged terminal, since the press and the drag
    // are drained together — so seating by slot left the pane the drag makes
    // active holding an empty rect for the rest of the burst.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    for (i, t) in app.terminals.iter_mut().enumerate() {
        t.set_manual_name(Some(format!("pane{i}")));
    }
    app.terminal_pane_maximized = true;
    let backend = ratatui::backend::TestBackend::new(180, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(app.active_terminal, 2);
    let pane = app.terminals[2].last_area;
    assert!(
        pane.width > 100,
        "the maximized pane owns the panel's width"
    );

    // The real gesture: press rail row 0, drag onto row 2, with NO redraw in
    // between — the main loop drains the whole burst before painting, so the
    // live pane rect still belongs to whichever pane was active at the last
    // paint, not to the one being dragged.
    let press = app.terminal_rail_rects[0];
    let target = app.terminal_rail_rects[2];
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: press.x + 1,
        row: press.y,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: target.x + 1,
        row: target.y,
        modifiers: KeyModifiers::NONE,
    });

    // The reorder must actually have happened: without this every assertion
    // below also holds when the gesture is a no-op, since slot 2 already owned
    // the pane before the drag.
    assert_eq!(
        app.terminals[2].label(),
        "pane0",
        "the dragged terminal must have moved from slot 0 to slot 2"
    );
    assert_eq!(app.active_terminal, 2, "the dragged terminal stays active");
    assert_eq!(
        app.terminals[2].last_area, pane,
        "the live pane rect follows the pane the drag leaves active"
    );
    assert!(
        app.terminals[..2].iter().all(|t| t.last_area.width == 0),
        "the hidden panes keep empty rects"
    );
    // The whole point: the next motion report in this same burst must resolve
    // to the pane that owns the panel, not to a hidden one.
    assert_eq!(
        app.terminal_at_pos(pane.x + pane.width / 2, pane.y + pane.height / 2),
        Some(2),
        "a hit inside the maximized pane names the active terminal"
    );
}

#[test]
fn activity_bar_render_lays_out_run_debug_between_source_control_and_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let bar = Rect {
        x: 0,
        y: 0,
        width: ACTIVITY_BAR_WIDTH,
        height: 30,
    };
    let backend = ratatui::backend::TestBackend::new(40, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_activity_bar(f, bar)).unwrap();
    let scm = app.sidebar_areas.source_control_icon;
    let run = app.sidebar_areas.run_debug_icon;
    let rem = app.sidebar_areas.remote_icon;
    assert!(run.height > 0, "run-debug icon must claim a non-zero block");
    assert!(
        run.y > scm.y,
        "run-debug must sit BELOW source-control (scm.y={}, run.y={})",
        scm.y,
        run.y
    );
    assert!(
        run.y < rem.y,
        "run-debug must sit ABOVE remote (run.y={}, rem.y={}) — VS Code's activity-bar order",
        run.y,
        rem.y
    );
}

// #865: the update pills in the status bar are a click target too, and say
// which chord the updater is on.
#[test]
fn clicking_the_status_bar_update_pill_starts_the_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.local_drift = Some(String::from("def456"));
    let backend = ratatui::backend::TestBackend::new(200, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let pill = app.status_update_rect;
    assert!(pill.width > 0, "the drift pill is a click target");
    let bar: String = (0..200)
        .map(|x| term.backend().buffer()[(x, pill.y)].symbol().to_string())
        .collect();
    assert!(
        bar.contains(&format!("{} to rebuild", super::UPDATE_CROFT_CHORD)),
        "{bar}"
    );
    left_click(&mut app, pill.x + 1, pill.y);
    assert!(app.self_install.is_some(), "a click starts the rebuild");
}

#[test]
fn switching_sidebar_view_closes_the_open_commit_dropdown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    app.commit_menu_open = true;
    // Leaving Source Control must close the dropdown so it can't paint
    // over the Run-Debug (or any other) sidebar view.
    app.set_sidebar_view(SidebarView::RunDebug);
    assert!(
        !app.commit_menu_open,
        "the commit dropdown must close when the sidebar view changes"
    );
}

// Move/Copy into New Window spawn a REAL Ghostty window via `open`, so the
// happy path can't be exercised in a test without popping windows on the dev
// machine. These tests cover everything up to (and the shape of) the spawn:
// the refusal gates, the argv builder, and the `--open-file` launch open.

#[test]
fn new_window_is_refused_for_an_unsaved_buffer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A fresh app has a single blank, path-less buffer — nothing to hand off.
    app.copy_into_new_window(0);
    assert!(
        app.status.contains("save the file first"),
        "the refusal is explained: {}",
        app.status
    );
    assert_eq!(app.editor.tab_count(), 1, "nothing was spawned or removed");
}

#[test]
fn new_window_is_refused_over_ssh_and_leaves_the_tab_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    // Over SSH croft runs on the headless host: there is no local window server.
    app.is_remote = true;
    let before = app.editor.tab_count();

    app.move_into_new_window(app.editor.active_index());

    assert!(
        app.status.contains("not available over SSH"),
        "the refusal is explained: {}",
        app.status
    );
    assert_eq!(
        app.editor.tab_count(),
        before,
        "Move is a no-op when it can't spawn — the file stays here"
    );
    assert!(app.editor.path.is_some());
}

#[cfg(target_os = "macos")]
#[test]
fn ghostty_open_argv_suppresses_the_stray_default_window() {
    use std::ffi::OsStr;
    let exe = std::path::Path::new("/usr/local/bin/croft");
    let root = std::path::Path::new("/home/me/project");
    let file = std::path::Path::new("/home/me/project/src/main.rs");
    let argv = ghostty_open_argv(exe, root, file);
    let argv: Vec<&OsStr> = argv.iter().map(|s| s.as_os_str()).collect();
    assert_eq!(
        argv,
        [
            OsStr::new("-na"),
            OsStr::new("Ghostty"),
            OsStr::new("--args"),
            // Empirically: without this a fresh instance opens a 2nd window.
            OsStr::new("--initial-window=false"),
            OsStr::new("--quit-after-last-window-closed=true"),
            OsStr::new("-e"),
            OsStr::new("/usr/local/bin/croft"),
            OsStr::new("/home/me/project"),
            OsStr::new("--open-file"),
            OsStr::new("/home/me/project/src/main.rs"),
            // Opens focused on the file.
            OsStr::new("--zen"),
        ],
        "one Ghostty window, focused on the file, no stray default window"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn new_window_targets_the_host_terminal() {
    // The new window matches whatever terminal croft runs in, not always Ghostty.
    assert_eq!(
        detect_host_terminal(Some("iTerm.app"), None),
        HostTerminal::ITerm2
    );
    assert_eq!(
        detect_host_terminal(Some("ghostty"), None),
        HostTerminal::Ghostty
    );
    assert_eq!(
        detect_host_terminal(Some("Apple_Terminal"), None),
        HostTerminal::AppleTerminal
    );
    // Ghostty sometimes only exports TERM=xterm-ghostty.
    assert_eq!(
        detect_host_terminal(None, Some("xterm-ghostty")),
        HostTerminal::Ghostty
    );
    // An unrecognised terminal is reported by name, not silently mishandled.
    assert_eq!(
        detect_host_terminal(Some("WezTerm"), None),
        HostTerminal::Unknown("WezTerm".to_string())
    );
}

#[test]
fn zen_layout_collapses_explorer_and_terminal_and_survives_opening_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hi.rs");
    std::fs::write(&file, "fn main() {}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Baseline: a normal launch shows both panes.
    assert!(app.show_tree, "explorer visible by default");
    assert!(app.show_terminal, "terminal visible by default");

    // This is exactly what `run(.., zen = true)` does.
    app.show_tree = false;
    app.show_terminal = false;
    app.open_file_at_launch(&file);

    // Opening the file (which focuses the editor) must NOT re-show the chrome.
    assert!(!app.show_tree, "--zen keeps the explorer collapsed");
    assert!(!app.show_terminal, "--zen keeps the terminal collapsed");
    assert_eq!(app.editor.path.as_deref(), Some(file.as_path()));
}

#[test]
fn cmd_active_treats_ctrl_as_command_only_on_termux() {
    // Termux (Android) has no Cmd key, so Ctrl stands in for it there
    // (VS Code's Linux keymap). Off Termux, Ctrl is NOT the command key.
    assert!(!cmd_active(KeyModifiers::CONTROL, false));
    assert!(cmd_active(KeyModifiers::CONTROL, true));
}

#[test]
fn the_output_view_renders_a_pushed_channel_line() {
    use crate::output::OutputLevel;
    let ch = "app-output-render-test";
    crate::output::push(ch, OutputLevel::Error, "boom-from-server");
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.bottom_panel_tab = BottomPanelTab::Output;
    // Select our channel so the assertion is independent of other channels.
    app.output.sync();
    assert!(
        app.output.select_by_name(ch),
        "the pushed channel is present in the bus",
    );
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let mut text = String::new();
    for y in 0..50 {
        for x in 0..140 {
            text.push_str(buf[(x, y)].symbol());
        }
    }
    assert!(
        text.contains("boom-from-server"),
        "the OUTPUT view renders the channel's line",
    );
}

#[test]
fn the_active_panel_tab_is_underlined() {
    use ratatui::style::Modifier;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // TERMINAL is active by default: its label underlines like a VS Code tab,
    // but the underline hugs the word, not the padded hit rect.
    let t = app.terminal_tab_rect;
    let buf = term.backend().buffer();
    let underlined = (t.x..t.x + t.width)
        .filter(|&x| buf[(x, t.y)].modifier.contains(Modifier::UNDERLINED))
        .count();
    assert_eq!(
        underlined,
        "TERMINAL".len(),
        "underline spans only the word"
    );
    assert!(
        !buf[(t.x, t.y)].modifier.contains(Modifier::UNDERLINED),
        "the leading pad space is not underlined",
    );
    assert!(
        !buf[(t.x + t.width - 1, t.y)]
            .modifier
            .contains(Modifier::UNDERLINED),
        "the trailing pad space is not underlined",
    );
}

#[test]
fn osk_auto_shows_on_terminal_tap_and_only_when_armed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.osk_auto = true;
    draw(&mut app, 80, 40);
    let t = app.terminals[0].last_area;
    assert!(t.width > 0 && t.height > 0, "terminal pane must be visible");
    left_click(&mut app, t.x + t.width / 2, t.y + t.height / 2);
    assert!(
        app.osk.is_some(),
        "a tap in the terminal must raise the on-screen keyboard on Termux"
    );

    // Desktop terminals (auto flag off) keep the band hidden on the same tap.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.osk_auto = false;
    draw(&mut app, 80, 40);
    let t = app.terminals[0].last_area;
    left_click(&mut app, t.x + t.width / 2, t.y + t.height / 2);
    assert!(app.osk.is_none());
}

#[test]
fn osk_auto_shows_on_editor_tap() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.txt");
    std::fs::write(&path, "hello\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.osk_auto = true;
    app.editor.open_preview(&path).unwrap();
    app.focus_pane(Pane::Editor);
    draw(&mut app, 80, 40);
    let e = app.editor.last_area;
    left_click(&mut app, e.x + e.width / 2, e.y + e.height / 2);
    assert!(
        app.osk.is_some(),
        "a tap in the editor text area must raise the on-screen keyboard"
    );
}

#[test]
fn osk_reserves_bottom_band_above_status_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.osk = Some(crate::widgets::osk::Osk::new());
    draw(&mut app, 80, 40);
    let band = app.osk.as_ref().unwrap().last_area;
    assert_eq!(
        band.height,
        crate::widgets::osk::band_height(40),
        "band height scales with the frame for thumb-sized keys"
    );
    assert_eq!(
        band.y + band.height,
        40 - 1,
        "keyboard docks directly above the status bar"
    );
    assert_eq!(band.width, 80, "keyboard spans the full frame width");
    // Default focus is the sidebar, not the terminal, so the terminal pane
    // folds away entirely and its space goes to the keyboard.
    assert_eq!(
        app.terminals[0].last_area,
        Rect::default(),
        "non-focused terminal must fold away (and not phantom-hit-test) while the keyboard is up"
    );
}

#[test]
fn osk_with_editor_focus_hides_terminal_and_keeps_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.txt");
    std::fs::write(&path, "hello\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_preview(&path).unwrap();
    app.focus_pane(Pane::Editor);
    app.osk = Some(crate::widgets::osk::Osk::new());
    draw(&mut app, 80, 40);
    let band = app.osk.as_ref().unwrap().last_area;
    assert!(band.height > 0);
    assert_eq!(app.terminals[0].last_area, Rect::default());
    let e = app.editor.last_full_area;
    assert!(
        e.height > 0,
        "the editor must stay visible while typing into it"
    );
    assert!(
        e.y + e.height <= band.y,
        "editor must end above the keyboard band"
    );
}

#[test]
fn osk_with_terminal_focus_bumps_terminal_above_keyboard() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.txt");
    std::fs::write(&path, "hello\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_preview(&path).unwrap();
    app.focus_pane(Pane::Terminal);
    app.osk = Some(crate::widgets::osk::Osk::new());
    draw(&mut app, 80, 40);
    let band = app.osk.as_ref().unwrap().last_area;
    let t = app.terminals[0].last_area;
    assert!(
        t.height > 0,
        "the terminal must stay visible while typing into it"
    );
    assert_eq!(
        t.y + t.height,
        band.y,
        "terminal rides directly on top of the keyboard"
    );
    assert_eq!(
        t.y, 1,
        "the panel group claims the editor's space from the top; the terminal \
         body sits one row down, under the panel tab strip (PROBLEMS / TERMINAL)",
    );
    assert_eq!(
        app.editor.last_area.height, 0,
        "the open editor folds away while typing into the terminal"
    );
}

#[test]
fn osk_tap_types_into_focused_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.txt");
    std::fs::write(&path, "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.osk_auto = true;
    app.editor.open_preview(&path).unwrap();
    app.focus_pane(Pane::Editor);
    draw(&mut app, 80, 40);
    let e = app.editor.last_area;
    left_click(&mut app, e.x + 1, e.y + 1);
    assert!(app.osk.is_some());
    // Second frame lays the keyboard band out so its keys are tappable.
    draw(&mut app, 80, 40);
    let r = app
        .osk
        .as_ref()
        .unwrap()
        .rect_for(crate::widgets::osk::OskKey::Char('a'))
        .expect("lower layer exposes the letter a");
    left_click(&mut app, r.x + r.width / 2, r.y);
    assert_eq!(
        app.editor.lines[0], "a",
        "an OSK letter tap must reach the editor buffer like a keystroke"
    );
}

#[test]
fn osk_hide_key_dismisses_the_band() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.osk = Some(crate::widgets::osk::Osk::new());
    draw(&mut app, 80, 40);
    let r = app
        .osk
        .as_ref()
        .unwrap()
        .rect_for(crate::widgets::osk::OskKey::Hide)
        .expect("hide key is always present");
    left_click(&mut app, r.x + r.width / 2, r.y);
    assert!(app.osk.is_none(), "the hide key must dismiss the keyboard");
}

#[test]
fn osk_tap_types_into_file_finder_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.osk = Some(crate::widgets::osk::Osk::new());
    app.open_file_finder();
    draw(&mut app, 80, 40);
    let r = app
        .osk
        .as_ref()
        .unwrap()
        .rect_for(crate::widgets::osk::OskKey::Char('a'))
        .unwrap();
    left_click(&mut app, r.x + r.width / 2, r.y);
    assert_eq!(
        app.file_finder.as_ref().unwrap().query,
        "a",
        "OSK taps must route into open modals, not be swallowed by their mouse gates"
    );
}

#[test]
fn hovering_a_non_selected_activity_icon_emits_its_hovered_variant() {
    use ratatui::layout::Rect;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Distinguishable payloads so the emitted set reveals which variant won.
    app.overlays.activity.set_images(ActivityBarImages {
        explorer_active: "EA".into(),
        explorer_inactive: "EI".into(),
        explorer_hovered: "EH".into(),
        search_active: "SA".into(),
        search_inactive: "SI".into(),
        search_hovered: "SH".into(),
        source_control_active: "CA".into(),
        source_control_inactive: "CI".into(),
        source_control_hovered: "CH".into(),
        remote_active: "RA".into(),
        remote_inactive: "RI".into(),
        remote_hovered: "RH".into(),
        run_debug_active: "DA".into(),
        run_debug_inactive: "DI".into(),
        run_debug_hovered: "DH".into(),
        extensions_active: "XA".into(),
        extensions_inactive: "XI".into(),
        extensions_hovered: "XH".into(),
        testing_active: "TA".into(),
        testing_inactive: "TI".into(),
        testing_hovered: "TH".into(),
        codeql_active: "CA".into(),
        codeql_inactive: "CI".into(),
        codeql_hovered: "CH".into(),
        sponsor_inactive: "HI".into(),
        sponsor_hovered: "HH".into(),
        settings_active: "GA".into(),
        settings_inactive: "GI".into(),
        settings_hovered: "GH".into(),
        layout_sidebar_on: "LS".into(),
        layout_sidebar_on_hover: "LSh".into(),
        layout_sidebar_off: "LF".into(),
        layout_sidebar_off_hover: "LFh".into(),
        layout_panel_on: "LP".into(),
        layout_panel_on_hover: "LPh".into(),
        layout_panel_off: "LQ".into(),
        layout_panel_off_hover: "LQh".into(),
        layout_customize: "LC".into(),
        layout_customize_hover: "LCh".into(),
    });
    // Every view icon needs a non-empty block or the emit early-returns.
    app.sidebar_areas.explorer_icon = Rect::new(0, 0, 2, 1);
    app.sidebar_areas.search_icon = Rect::new(0, 2, 2, 1);
    app.sidebar_areas.source_control_icon = Rect::new(0, 4, 2, 1);
    app.sidebar_areas.remote_icon = Rect::new(0, 6, 2, 1);
    app.sidebar_areas.run_debug_icon = Rect::new(0, 8, 2, 1);
    app.sidebar_areas.extensions_icon = Rect::new(0, 10, 2, 1);
    app.sidebar_areas.testing_icon = Rect::new(0, 12, 2, 1);
    app.sidebar_view = SidebarView::Explorer;
    // Hover Search, a non-selected icon.
    app.hovered_activity_icon = Some(ActivityIcon::Search);
    let emitted: Vec<&str> = app
        .pending_activity_image_overlays()
        .into_iter()
        .map(|(_, s)| s)
        .collect();
    assert!(
        emitted.contains(&"SH"),
        "the hovered search icon emits its hovered variant"
    );
    assert!(
        !emitted.contains(&"SI"),
        "search drops its resting variant while hovered"
    );
    assert!(
        emitted.contains(&"EA"),
        "the selected explorer view still wins with its active variant"
    );
}

/// #1940: the sponsor heart emits its resting bake, and swaps to its
/// hovered one while the pointer is on it.
#[test]
fn hovering_the_sponsor_heart_emits_its_hovered_variant() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(ActivityBarImages {
        sponsor_inactive: "HI".into(),
        sponsor_hovered: "HH".into(),
        ..Default::default()
    });
    let emitted = |app: &App| -> Vec<String> {
        app.pending_activity_image_overlays()
            .into_iter()
            .map(|(_, s)| s.to_string())
            .collect()
    };
    draw(&mut app, 120, 40);
    assert!(emitted(&app).contains(&"HI".to_string()));
    let heart = app.sidebar_areas.sponsor_icon;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column: heart.x,
        row: heart.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.hovered_activity_icon, Some(ActivityIcon::Sponsor));
    let hovered = emitted(&app);
    assert!(hovered.contains(&"HH".to_string()), "{hovered:?}");
    assert!(!hovered.contains(&"HI".to_string()), "{hovered:?}");
}

/// #1845: a collapsed side bar selects no view, as in VS Code, so its view's
/// icon drops the pill, and the flip re-sends the baked icons whichever
/// route made it.
#[test]
fn a_collapsed_side_bar_unpills_its_views_activity_icon() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(ActivityBarImages {
        explorer_active: "EA".into(),
        explorer_inactive: "EI".into(),
        explorer_hovered: "EH".into(),
        ..Default::default()
    });
    let emitted = |app: &App| -> Vec<String> {
        app.pending_activity_image_overlays()
            .into_iter()
            .map(|(_, s)| s.to_string())
            .collect()
    };
    draw(&mut app, 120, 40);
    assert!(emitted(&app).contains(&"EA".to_string()));
    app.overlays.activity.mark_emitted();
    draw(&mut app, 120, 40);
    assert!(
        !app.overlays.activity.dirty(),
        "an unchanged frame re-sends nothing"
    );
    // Cmd+B's route, which marks no overlay dirty itself.
    app.toggle_side_bar();
    draw(&mut app, 120, 40);
    assert!(
        app.overlays.activity.dirty(),
        "closing the side bar re-sends the icons"
    );
    let shown = emitted(&app);
    assert!(shown.contains(&"EI".to_string()), "{shown:?}");
    assert!(!shown.contains(&"EA".to_string()), "{shown:?}");
}

#[test]
fn moving_over_an_activity_icon_marks_it_hovered_and_redirties_the_overlay() {
    use ratatui::layout::Rect;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_areas.search_icon = Rect::new(0, 4, 2, 2);
    // Baseline: no hover, icons already emitted (clean).
    app.hovered_activity_icon = None;
    app.overlays.activity.mark_emitted();
    assert!(!app.overlays.activity.dirty());
    let moved = |col: u16, row: u16| crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    };
    // Pointer enters the search icon (rows 4..6).
    app.handle_mouse(moved(0, 5));
    assert_eq!(app.hovered_activity_icon, Some(ActivityIcon::Search));
    assert!(
        app.overlays.activity.dirty(),
        "entering an icon re-emits the swapped image"
    );
    // Drifting within the same icon costs nothing: no re-dirty.
    app.overlays.activity.mark_emitted();
    app.handle_mouse(moved(1, 4));
    assert_eq!(app.hovered_activity_icon, Some(ActivityIcon::Search));
    assert!(
        !app.overlays.activity.dirty(),
        "drifting within one icon does not re-emit"
    );
    // Leaving the bar clears the hover and re-emits the resting icon.
    app.handle_mouse(moved(40, 20));
    assert_eq!(app.hovered_activity_icon, None);
    assert!(
        app.overlays.activity.dirty(),
        "leaving the icon re-emits its resting variant"
    );
}

#[test]
fn status_transient_keeps_its_tail_when_longer_than_the_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A transient whose informative part is the tail, longer than the room
    // left of the right cluster at 140 cols. The right cluster paints over
    // the left paragraph, so an unelided message loses exactly its tail —
    // for "Opened <deep absolute path>" that was the filename (#100).
    let deep: String = "directory-segment/".repeat(7);
    app.status = format!("Opened /{deep}main.rs");
    let backend = ratatui::backend::TestBackend::new(140, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer().clone();
    let a = buf.area;
    let last_row: String = (a.x..a.x + a.width)
        .map(|x| buf[(x, a.y + a.height - 1)].symbol().to_string())
        .collect();
    assert!(
        last_row.contains("main.rs"),
        "an over-long status transient must middle-elide so its tail stays \
         visible; the bar showed: {last_row:?}"
    );
    // Once a key has been pressed the right cluster is back (#1609), and the
    // message still keeps its tail beside it.
    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    app.status = format!("Opened /{deep}main.rs");
    app.status_logged = app.status.clone();
    term.draw(|f| app.render(f)).unwrap();
    let last_row = status_row(&term);
    assert!(last_row.contains("main.rs"), "{last_row:?}");
    assert!(
        last_row.contains("Ln 1, Col 1"),
        "the right cluster must still paint: {last_row:?}"
    );
}

/// #1609: a message is measured in terminal cells, so a path of
/// double-width names is elided to fit beside the right cluster instead of
/// counting half its width and having its tail painted over.
#[test]
fn status_message_with_wide_chars_keeps_its_tail_beside_the_cluster() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    let deep = "\u{76ee}\u{5f55}/".repeat(15);
    app.status = format!("Opened /{deep}main.rs");
    app.status_logged = app.status.clone();
    app.status_wide = false;
    term.draw(|f| app.render(f)).unwrap();
    let row = status_row(&term);
    assert!(
        row.contains("Ln 1, Col 1"),
        "the right cluster paints: {row:?}"
    );
    assert!(
        row.contains("main.rs"),
        "the wide-char message keeps its tail: {row:?}"
    );
}

/// #925: a message of several lines (git's text, a tool's error) is drawn
/// with a space where each line break or tab was; drawn as is, they were
/// dropped and the words either side ran together.
#[test]
fn a_multi_line_status_message_keeps_its_words_apart() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    app.status =
        String::from("Branch: overwritten by checkout:\n\ta.txt\nswitch branches.\nAborting");
    term.draw(|f| app.render(f)).unwrap();
    let row = status_row(&term);
    assert!(
        row.contains("Branch: overwritten by checkout: a.txt switch branches. Aborting"),
        "{row:?}"
    );
}

/// The recorder's prompt from Preferences: Open Keyboard Shortcuts, which
/// is longer than the room the right-hand cluster leaves at 140 columns.
fn recorder_prompt() -> String {
    String::from(
        "Press the new shortcut for \u{201c}Sort Lines Ascending\u{201d} (a function key or a chord with Cmd/Ctrl/Alt; Esc cancels)",
    )
}

/// #1609: a new message too long for the room beside Ln/Col, encoding and
/// language takes the whole bar, so its middle is not cut away; the next key
/// hands the room back.
#[test]
fn a_new_long_status_message_takes_the_whole_bar_until_a_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.status = recorder_prompt();
    term.draw(|f| app.render(f)).unwrap();
    let row = status_row(&term);
    assert!(
        row.contains(&recorder_prompt()),
        "the whole message shows at 140 columns: {row:?}"
    );
    assert!(
        !row.contains("Ln 1, Col 1"),
        "the cluster made room: {row:?}"
    );
    // Redrawing without input keeps it whole.
    term.draw(|f| app.render(f)).unwrap();
    assert!(status_row(&term).contains(&recorder_prompt()));

    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 60,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    app.status = recorder_prompt();
    term.draw(|f| app.render(f)).unwrap();
    let row = status_row(&term);
    assert!(
        row.contains("Ln 1, Col 1"),
        "a click brings the cluster back: {row:?}"
    );
    assert!(
        row.contains('\u{2026}'),
        "and the old message elides beside it: {row:?}"
    );
}

/// #1609: every message shown is kept, in full, in the Messages output
/// channel, and a click on the message opens it there.
#[test]
fn status_messages_are_kept_in_the_messages_channel() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    let first = format!("{} #{}", recorder_prompt(), std::process::id());
    let second = format!("Fleet: no host named web1 #{}", std::process::id());
    app.status = first.clone();
    term.draw(|f| app.render(f)).unwrap();
    app.status = second.clone();
    term.draw(|f| app.render(f)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let lines: Vec<String> = crate::output::snapshot(crate::output::CHANNEL_MESSAGES)
        .unwrap_or_default()
        .into_iter()
        .map(|l| l.text)
        .collect();
    assert_eq!(
        lines.iter().filter(|l| **l == first).count(),
        1,
        "{lines:?}"
    );
    assert_eq!(
        lines.iter().filter(|l| **l == second).count(),
        1,
        "logged once"
    );

    let r = app.status_message_rect;
    assert!(r.width > 0, "the message has a click target");
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: r.x + 1,
        row: r.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Output);
    assert_eq!(
        app.output.selected_name().as_deref(),
        Some(crate::output::CHANNEL_MESSAGES)
    );
}

/// Negative: a message that fits beside the cluster leaves it in place, and
/// an empty status logs nothing.
#[test]
fn a_status_message_that_fits_keeps_the_cluster() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    app.status = String::from("Saved");
    term.draw(|f| app.render(f)).unwrap();
    let row = status_row(&term);
    assert!(
        row.contains("Saved") && row.contains("Ln 1, Col 1"),
        "{row:?}"
    );
    app.status.clear();
    term.draw(|f| app.render(f)).unwrap();
    let lines = crate::output::snapshot(crate::output::CHANNEL_MESSAGES).unwrap_or_default();
    assert!(
        lines.iter().all(|l| !l.text.is_empty()),
        "an empty status is not logged"
    );
    assert_eq!(
        app.status_message_rect.width, 0,
        "no message, no click target"
    );
}

// --- Customize Layout: pure chrome geometry -------------------------------

fn main_band() -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 30,
    }
}

#[test]
fn chrome_left_position_orders_activity_primary_editor_secondary() {
    let l = compute_chrome_layout(main_band(), true, true, true, SideBarPosition::Left, 20, 30);
    let a = l.activity.expect("activity shown");
    let p = l.primary_side.expect("primary shown");
    let s = l.secondary_side.expect("secondary shown");
    assert_eq!(a.x, 0, "activity bar hugs the left edge");
    assert_eq!(a.width, ACTIVITY_BAR_WIDTH);
    assert_eq!(
        p.x,
        a.x + a.width,
        "primary side bar follows the activity bar"
    );
    assert_eq!(p.width, 20);
    assert_eq!(
        l.right.x,
        p.x + p.width,
        "editor follows the primary side bar"
    );
    assert_eq!(
        s.x,
        l.right.x + l.right.width,
        "secondary is on the far right"
    );
    assert_eq!(
        l.sidebar_splitter_x,
        Some(l.right.x),
        "seam is the editor's left edge"
    );
    // Every column tiles the band exactly, no gaps or overlaps.
    assert_eq!(a.width + p.width + l.right.width + s.width, 100);
}

#[test]
fn chrome_right_position_mirrors_to_secondary_editor_primary_activity() {
    let l = compute_chrome_layout(
        main_band(),
        true,
        true,
        true,
        SideBarPosition::Right,
        20,
        30,
    );
    let s = l.secondary_side.expect("secondary shown");
    let p = l.primary_side.expect("primary shown");
    let a = l.activity.expect("activity shown");
    assert_eq!(
        s.x, 0,
        "secondary hugs the left when the primary is on the right"
    );
    assert_eq!(l.right.x, s.x + s.width, "editor follows the secondary");
    assert_eq!(
        p.x,
        l.right.x + l.right.width,
        "primary side bar follows the editor"
    );
    assert_eq!(a.x, p.x + p.width, "activity bar is on the far right");
    assert_eq!(
        l.sidebar_splitter_x,
        Some(p.x),
        "seam is the editor's right edge"
    );
    assert_eq!(s.width + l.right.width + p.width + a.width, 100);
}

#[test]
fn chrome_hidden_regions_consume_no_width() {
    let l = compute_chrome_layout(
        main_band(),
        false,
        false,
        false,
        SideBarPosition::Left,
        20,
        30,
    );
    assert!(l.activity.is_none());
    assert!(l.primary_side.is_none());
    assert!(l.secondary_side.is_none());
    assert_eq!(l.sidebar_splitter_x, None, "no seam without a side bar");
    assert_eq!(l.right.x, 0, "editor reclaims the whole band");
    assert_eq!(l.right.width, 100);
}

#[test]
fn chrome_narrow_frame_shrinks_primary_to_keep_editor_min() {
    // activity(4) + primary wants 90 + editor needs RIGHT_PANE_MIN, in 100 wide.
    let l = compute_chrome_layout(main_band(), true, true, false, SideBarPosition::Left, 90, 0);
    assert_eq!(l.right.width, RIGHT_PANE_MIN, "editor keeps its minimum");
    let p = l.primary_side.expect("primary still shown, just narrower");
    assert_eq!(p.width, 100 - ACTIVITY_BAR_WIDTH - RIGHT_PANE_MIN);
}

#[test]
fn chrome_drops_a_secondary_side_bar_the_window_cannot_fit() {
    // #1132 (comment): at 30 columns the 30-column secondary bar was placed
    // past the right edge of the frame, and its scrollbar wrote outside it.
    let narrow = Rect::new(0, 0, 30, 9);
    for pos in [SideBarPosition::Left, SideBarPosition::Right] {
        let l = compute_chrome_layout(narrow, true, true, true, pos, 20, 30);
        assert!(
            l.secondary_side.is_none(),
            "{pos:?}: {:?}",
            l.secondary_side
        );
        for r in [l.activity, l.primary_side, Some(l.right)]
            .into_iter()
            .flatten()
        {
            assert!(r.right() <= narrow.right(), "{pos:?}: {r:?}");
        }
    }
}

#[test]
fn chrome_keeps_the_secondary_side_bar_when_it_fits() {
    // Negative: with room for it next to the editor's minimum, the secondary
    // bar keeps its full width.
    let l = compute_chrome_layout(
        main_band(),
        true,
        false,
        true,
        SideBarPosition::Left,
        20,
        30,
    );
    assert_eq!(l.secondary_side.map(|s| s.width), Some(30));
}

#[test]
fn panel_band_center_and_justify_fill_the_band() {
    let band = Rect {
        x: 10,
        y: 5,
        width: 40,
        height: 8,
    };
    assert_eq!(panel_band_rect(band, PanelAlignment::Center), band);
    assert_eq!(panel_band_rect(band, PanelAlignment::Justify), band);
}

#[test]
fn panel_band_left_hugs_left_right_hugs_right() {
    let band = Rect {
        x: 10,
        y: 5,
        width: 60,
        height: 8,
    };
    let left = panel_band_rect(band, PanelAlignment::Left);
    assert_eq!(left.x, 10);
    assert_eq!(left.width, 30);
    let right = panel_band_rect(band, PanelAlignment::Right);
    assert_eq!(
        right.x,
        10 + 60 - 30,
        "right-aligned panel ends at the band's right edge"
    );
    assert_eq!(right.width, 30);
}

// --- Zen Mode snapshot / restore ------------------------------------------

#[test]
fn zen_mode_hides_all_chrome_then_restores_the_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A non-default starting layout so restore is meaningful.
    app.show_tree = true;
    app.show_terminal = false;
    app.secondary_side_bar_visible = true;
    app.activity_bar_visible = true;
    app.status_bar_visible = true;

    app.toggle_zen_mode();
    assert!(app.zen_mode, "zen on");
    assert!(!app.activity_bar_visible);
    assert!(!app.status_bar_visible);
    assert!(!app.show_tree);
    assert!(!app.secondary_side_bar_visible);
    assert!(!app.show_terminal);

    app.toggle_zen_mode();
    assert!(!app.zen_mode, "zen off");
    assert!(app.show_tree, "restored prior side bar");
    assert!(!app.show_terminal, "restored prior (hidden) panel");
    assert!(
        app.secondary_side_bar_visible,
        "restored prior secondary bar"
    );
    assert!(app.activity_bar_visible);
    assert!(app.status_bar_visible);
}

/// A 200-column frame with `app` drawn into it, and its text (#1585).
fn draw_wide(app: &mut App, w: u16) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    screen_text(&term)
}

/// The editor as last drawn, `(x, width)` (#1585): its text and the
/// minimap strip carved from its right edge. Centered Layout centres the
/// two together, as VS Code centres an editor with its minimap.
fn editor_column(app: &App) -> (u16, u16) {
    let text = app.editor.last_full_area;
    let strip = if app.minimap_visible && app.minimap_room {
        MINIMAP_WIDTH_CELLS
    } else {
        0
    };
    (text.x, text.width + strip)
}

/// #1585: Zen Mode centres the editor: in 200 columns it is 120 wide and
/// starts at column 40, like VS Code's `zenMode.centerLayout`. The
/// minimap is part of those 120 columns; without it the text takes them.
#[test]
fn zen_mode_centres_the_editor_in_a_wide_window() {
    let (_tmp, mut app) = zen_app();
    app.toggle_zen_mode();
    draw_wide(&mut app, 200);
    assert!(app.minimap_visible && app.minimap_room, "the minimap shows");
    assert_eq!(
        editor_column(&app),
        (40, 120),
        "{:?}",
        app.editor.last_full_area
    );
    app.toggle_minimap();
    draw_wide(&mut app, 200);
    let area = app.editor.last_full_area;
    assert_eq!((area.x, area.width), (40, 120), "{area:?}");
    app.toggle_minimap();
    app.toggle_zen_mode();
    draw_wide(&mut app, 200);
    assert!(
        editor_column(&app).1 > 120,
        "out of Zen the editor fills its column again: {:?}",
        app.editor.last_full_area
    );
}

/// #1585: View: Toggle Centered Layout centres the editor outside Zen and
/// persists in the layout; inside a centred Zen it turns the centring off.
#[test]
fn toggle_centered_layout_centres_outside_zen_and_uncentres_zen() {
    let (_tmp, mut app) = zen_app();
    app.show_tree = false;
    app.activity_bar_visible = false;
    app.show_terminal = false;
    app.run_command(crate::widgets::command_palette::Command::ToggleCenteredLayout);
    assert!(app.centered_layout);
    assert_eq!(app.status, "Centered Layout on");
    draw_wide(&mut app, 200);
    assert_eq!(editor_column(&app), (40, 120));
    app.run_command(crate::widgets::command_palette::Command::ToggleCenteredLayout);
    assert!(!app.centered_layout);

    app.toggle_zen_mode();
    assert!(app.centered_layout_active(), "Zen starts centred");
    app.run_command(crate::widgets::command_palette::Command::ToggleCenteredLayout);
    assert!(!app.centered_layout_active() && !app.centered_layout);
    draw_wide(&mut app, 200);
    assert_eq!(editor_column(&app), (0, 200));
    let rows = app.customize_layout_items();
    assert!(
        rows.iter().any(|r| matches!(r, MenuEntry::Item { label, action: MenuAction::ToggleCenteredLayout, .. } if label.contains("Centered Layout"))),
        "the Customize Layout popup has the row"
    );
}

/// #1585, negative: a window no wider than the centred width, or Zen with
/// `zen_center_layout` off, leaves the editor its whole column.
#[test]
fn a_narrow_window_or_zen_without_centring_keeps_the_full_width() {
    let (_tmp, mut app) = zen_app();
    app.toggle_zen_mode();
    draw_wide(&mut app, 110);
    assert_eq!(editor_column(&app), (0, 110));
    app.toggle_zen_mode();
    app.zen_center_layout = false;
    app.toggle_zen_mode();
    draw_wide(&mut app, 200);
    assert_eq!(editor_column(&app), (0, 200));
    assert_eq!(
        centered_editor_rect(Rect::new(5, 1, 50, 9), 0),
        Rect::new(5, 1, 50, 9),
        "a zero width centres nothing"
    );
    assert_eq!(
        centered_editor_rect(Rect::new(5, 1, 51, 9), 20),
        Rect::new(20, 1, 20, 9)
    );
}

/// #1585: Esc Esc leaves Zen Mode and restores the chrome from before it.
#[test]
fn esc_esc_leaves_zen_mode_and_restores_the_chrome() {
    let (_tmp, mut app) = zen_app();
    app.show_tree = true;
    app.status_bar_visible = true;
    app.toggle_zen_mode();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.zen_mode, "one Esc is not enough");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(!app.zen_mode, "Esc Esc leaves Zen");
    assert!(app.show_tree && app.status_bar_visible);
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(!app.zen_mode, "Esc Esc never enters Zen");
}

/// #1585: Zen hides the status bar, so entering it shows how to leave for
/// a few seconds; leaving takes the hint with it.
#[test]
fn entering_zen_says_how_to_leave_it_for_a_while() {
    let (_tmp, mut app) = zen_app();
    assert!(!draw_wide(&mut app, 120).contains("Esc Esc to exit"));
    app.toggle_zen_mode();
    assert!(draw_wide(&mut app, 120).contains(zen_hint_text()));
    assert!(!app.expire_zen_hint(std::time::Instant::now()), "not yet");
    assert!(app.expire_zen_hint(std::time::Instant::now() + ZEN_HINT_FOR));
    assert!(!draw_wide(&mut app, 120).contains("Esc Esc to exit"));
    app.toggle_zen_mode();
    app.toggle_zen_mode();
    app.toggle_zen_mode();
    assert!(
        app.zen_hint_until.is_none(),
        "leaving Zen drops a hint still showing"
    );
}

#[test]
fn secondary_side_bar_toggle_flips_visibility() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let before = app.secondary_side_bar_visible;
    app.toggle_secondary_side_bar();
    assert_eq!(app.secondary_side_bar_visible, !before);
}

#[test]
fn customize_layout_items_reflect_current_state_with_checks() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.activity_bar_visible = true;
    app.status_bar_visible = false;
    app.side_bar_position = SideBarPosition::Right;
    app.panel_alignment = PanelAlignment::Justify;
    let items = app.customize_layout_items();
    // Activity Bar (on) gets a check; Status Bar (off) does not.
    let activity = menu_label(&items[0]);
    assert!(
        activity.starts_with('\u{2713}'),
        "Activity Bar checked: {activity}"
    );
    let status = menu_label(&items[4]);
    assert!(
        !status.starts_with('\u{2713}'),
        "Status Bar unchecked: {status}"
    );
    // The active radio option carries the check.
    let labels = menu_labels(&items);
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with('\u{2713}') && l.contains("Right")),
        "side-bar Right is checked"
    );
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with('\u{2713}') && l.contains("Justify")),
        "panel Justify is checked"
    );
}

#[test]
fn render_survives_every_customize_layout_combination() {
    // Exercises the real render() path (not just the pure geometry) across the
    // layout permutations so an out-of-bounds buffer write in any branch
    // (side bar right, secondary bar, hidden chrome, every panel alignment)
    // panics here rather than in front of the user.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let aligns = [
        PanelAlignment::Left,
        PanelAlignment::Center,
        PanelAlignment::Right,
        PanelAlignment::Justify,
    ];
    for &pos in &[SideBarPosition::Left, SideBarPosition::Right] {
        for &activity in &[true, false] {
            for &secondary in &[true, false] {
                for &status in &[true, false] {
                    for &align in &aligns {
                        app.side_bar_position = pos;
                        app.activity_bar_visible = activity;
                        app.secondary_side_bar_visible = secondary;
                        app.status_bar_visible = status;
                        app.panel_alignment = align;
                        for size in [(120u16, 40u16), (40, 12)] {
                            let backend = ratatui::backend::TestBackend::new(size.0, size.1);
                            let mut term = ratatui::Terminal::new(backend).unwrap();
                            term.draw(|f| app.render(f)).expect("render must not panic");
                        }
                    }
                }
            }
        }
    }
    // And in Zen Mode (all chrome hidden) at a tiny size.
    app.toggle_zen_mode();
    let backend = ratatui::backend::TestBackend::new(30, 8);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f))
        .expect("zen render must not panic");
}

#[test]
fn layout_toolbar_emits_2x_inline_images_reflecting_on_off_state() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut imgs = dummy_activity_images();
    imgs.layout_sidebar_on = "SIDE_ON".into();
    imgs.layout_sidebar_on_hover = "SIDE_ON_H".into();
    imgs.layout_sidebar_off = "SIDE_OFF".into();
    imgs.layout_panel_on = "PANEL_ON".into();
    imgs.layout_panel_off = "PANEL_OFF".into();
    imgs.layout_customize = "CUSTOMIZE".into();
    imgs.layout_customize_hover = "CUSTOMIZE_H".into();
    app.overlays.activity.set_images(imgs);
    // Place the toolbar (4 cells wide each), zero the activity bar so only the
    // layout icons survive the width filter.
    app.sidebar_areas = SidebarAreas::default();
    app.layout_icon_areas = LayoutIconAreas {
        toggle_side_bar: Rect::new(50, 0, LAYOUT_ICON_CELLS_W, LAYOUT_ICON_CELLS_H),
        toggle_panel: Rect::new(55, 0, LAYOUT_ICON_CELLS_W, LAYOUT_ICON_CELLS_H),
        customize: Rect::new(60, 0, LAYOUT_ICON_CELLS_W, LAYOUT_ICON_CELLS_H),
    };
    app.show_tree = true;
    app.show_terminal = false;
    app.pointer_cell = None;
    let overlays = app.pending_activity_image_overlays();
    // Side bar shown -> the filled (ON) image; panel hidden -> the hollow (OFF).
    assert!(
        overlays
            .iter()
            .any(|(pos, s)| *pos == (50, 0) && *s == "SIDE_ON"),
        "side-bar icon emits the ON image when shown"
    );
    assert!(
        overlays
            .iter()
            .any(|(pos, s)| *pos == (55, 0) && *s == "PANEL_OFF"),
        "panel icon emits the OFF image when hidden"
    );
    assert!(
        overlays
            .iter()
            .any(|(pos, s)| *pos == (60, 0) && *s == "CUSTOMIZE"),
        "customize icon always emits its image"
    );
    // Pointer over the customize icon -> the brighter hover variant.
    app.pointer_cell = Some((61, 0));
    let overlays = app.pending_activity_image_overlays();
    assert!(
        overlays
            .iter()
            .any(|(pos, s)| *pos == (60, 0) && *s == "CUSTOMIZE_H"),
        "customize icon emits the hover image under the pointer"
    );
    assert!(
        overlays
            .iter()
            .any(|(pos, s)| *pos == (50, 0) && *s == "SIDE_ON"),
        "a non-hovered icon keeps its resting image"
    );
    // Pointer over the side-bar icon, side bar shown -> the ON hover variant.
    app.pointer_cell = Some((51, 0));
    let overlays = app.pending_activity_image_overlays();
    assert!(
        overlays
            .iter()
            .any(|(pos, s)| *pos == (50, 0) && *s == "SIDE_ON_H"),
        "side-bar icon emits the ON hover image under the pointer"
    );
    // Flip both visibilities: the images swap to match.
    app.pointer_cell = None;
    app.show_tree = false;
    app.show_terminal = true;
    let overlays = app.pending_activity_image_overlays();
    assert!(
        overlays
            .iter()
            .any(|(p, s)| *p == (50, 0) && *s == "SIDE_OFF")
    );
    assert!(
        overlays
            .iter()
            .any(|(p, s)| *p == (55, 0) && *s == "PANEL_ON")
    );
}

#[test]
fn clicking_the_customize_icon_opens_the_customize_layout_popup() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let cust = app.layout_icon_areas.customize;
    assert!(cust.width > 0, "the customize icon must be laid out");
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
        column: cust.x,
        row: cust.y,
        modifiers: KeyModifiers::NONE,
    });
    let menu = app.context_menu.as_ref().expect("customize popup opens");
    assert!(
        matches!(
            menu.items.first(),
            Some(MenuEntry::Item {
                action: MenuAction::ToggleActivityBar,
                ..
            })
        ),
        "the popup is the Customize Layout menu"
    );
    // And the icon advertises itself on hover.
    assert_eq!(app.ui_tooltip_at(cust.x, cust.y), Some("Customize Layout"));
}

#[test]
fn clicking_the_side_bar_icon_toggles_the_primary_side_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let side = app.layout_icon_areas.toggle_side_bar;
    assert!(side.width > 0);
    let before = app.show_tree;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
        column: side.x,
        row: side.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.show_tree, !before, "the side-bar icon flips show_tree");
}

/// #1845: as in VS Code, a click on the icon of the view the side bar is
/// showing collapses the side bar, and the next click on it expands it again.
#[test]
fn clicking_the_open_views_activity_icon_collapses_then_reopens_the_side_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    draw(&mut app, 120, 40);
    let explorer = app.sidebar_areas.explorer_icon;
    assert!(app.show_tree && app.sidebar_view == SidebarView::Explorer);
    app.focus_pane(Pane::Tree);
    left_click(&mut app, explorer.x, explorer.y);
    assert!(
        !app.show_tree,
        "the open view's icon collapses the side bar"
    );
    assert!(
        app.focus == Pane::Editor,
        "focus leaves the hidden side bar"
    );
    draw(&mut app, 120, 40);
    left_click(&mut app, explorer.x, explorer.y);
    assert!(app.show_tree, "the next click expands it again");
    assert_eq!(app.sidebar_view, SidebarView::Explorer);
    assert!(app.focus == Pane::Tree);
}

#[test]
fn clicking_another_views_activity_icon_switches_and_expands_a_collapsed_side_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    draw(&mut app, 120, 40);
    let explorer = app.sidebar_areas.explorer_icon;
    let search = app.sidebar_areas.search_icon;
    left_click(&mut app, search.x, search.y);
    assert!(
        app.show_tree,
        "another view's icon switches, never collapses"
    );
    assert_eq!(app.sidebar_view, SidebarView::Search);
    left_click(&mut app, search.x, search.y);
    assert!(!app.show_tree);
    left_click(&mut app, explorer.x, explorer.y);
    assert!(
        app.show_tree,
        "any view's icon expands a collapsed side bar"
    );
    assert_eq!(app.sidebar_view, SidebarView::Explorer);
}

/// Collapsing from a maximized terminal's side bar hands focus to the
/// terminal: the editor taking it would un-maximize the terminal.
#[test]
fn collapsing_the_side_bar_from_its_icon_keeps_a_maximized_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    draw(&mut app, 120, 40);
    let explorer = app.sidebar_areas.explorer_icon;
    app.show_terminal = true;
    app.terminal_maximized = true;
    app.focus_pane(Pane::Tree);
    left_click(&mut app, explorer.x, explorer.y);
    assert!(!app.show_tree);
    assert!(app.focus == Pane::Terminal);
    assert!(app.terminal_maximized, "the terminal stays maximized");
}

#[test]
fn secondary_sidebar_chord_is_alt_cmd_b_not_plain_cmd_b() {
    assert!(is_secondary_sidebar_toggle_key(key(
        KeyCode::Char('b'),
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
    assert!(!is_secondary_sidebar_toggle_key(key(
        KeyCode::Char('b'),
        KeyModifiers::SUPER
    )));
    // Plain Cmd+B stays the PRIMARY side-bar toggle, never the secondary.
    assert!(!is_sidebar_toggle_key(key(
        KeyCode::Char('b'),
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
}

#[test]
fn resize_must_force_a_full_text_repaint_on_every_protocol() {
    // dtach reattaches with `-r winch` precisely so the app repaints: after a
    // reattach the physical screen is blank while ratatui's back buffer still
    // says every cell is painted, so the diff alone emits nothing and only
    // later-changing cells (status bar, cursor) ever appear - the "remote
    // croft comes back all black" bug. The image-eviction wipe used to cover
    // this by accident; Kitty skips that wipe (the typing-blink fix), so a
    // WINCH must arm its own unconditional repaint.
    use crate::iterm2_inline::InlineImageProtocol;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.inline_protocol = InlineImageProtocol::Kitty;
    assert!(
        !app.consume_resize_repaint(),
        "no repaint owed before any resize"
    );
    app.on_resize();
    assert!(
        app.consume_resize_repaint(),
        "a WINCH must force the full text repaint even where image eviction skips the wipe"
    );
    assert!(
        !app.consume_resize_repaint(),
        "the resize repaint latch is one-shot"
    );
}

/// A window whose spawn FAILED must not keep the workspace's single-host
/// lock: it cannot host, so holding the flock only blocks the window that
/// can. The failed spawner un-appoints itself and releases; the next window
/// acquires and seats.
#[test]
fn a_failed_spawner_releases_the_lock_so_another_window_can_host() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("collab.sock");
    let lock_path = tmp.path().join("host.lock");
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
    let record = crate::session::PairRecord {
        model: None,
        name: "nav".into(),
        enabled: true,
        task: None,
        provider: None,
        base_url: None,
    };

    let mut a = App::new(tmp.path().to_path_buf()).unwrap();
    a.pair_record_path = tmp.path().join("a.pair.json");
    a.pair_socket = socket.clone();
    a.pair_host_lock_path = lock_path.clone();
    a.pair_spawn_override = Some(Box::new(|_| anyhow::bail!("claude is broken")));
    crate::session::write_pair_record(&a.pair_record_path, &record).unwrap();
    a.last_pair_check = None;
    a.maybe_seat_navigator();
    assert!(a.pair_host.is_none());
    assert!(a.navigator_down, "the failure latches, no 1s retry storm");
    assert!(
        a.pair_host_lock.is_none(),
        "a window that cannot host must release the lock"
    );
    assert!(
        a.collab_config.is_none(),
        "the same-tick self-appointment is undone with it"
    );

    let mut b = App::new(tmp.path().to_path_buf()).unwrap();
    b.pair_record_path = tmp.path().join("b.pair.json");
    b.pair_socket = socket.clone();
    b.pair_host_lock_path = lock_path.clone();
    b.pair_spawn_override = Some(Box::new(local_test_spawn));
    crate::session::write_pair_record(&b.pair_record_path, &record).unwrap();
    b.last_pair_check = None;
    b.maybe_seat_navigator();
    assert!(
        b.pair_host.is_some(),
        "the working window must be able to host: {}",
        b.status
    );
}

#[test]
fn status_elision_never_fires_on_a_message_that_fits() {
    let tmp = tempfile::tempdir().unwrap();
    // A git repo puts the branch segment in the bar, like any real session.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("a.txt"), "x\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "init"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let _ = app.drain_git_responses();
        if app.git.status().branch.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    app.status = String::from("Quick select: type a label to copy, UPPERCASE pastes, Esc cancels");
    // 153 cols: prefix (with the branch segment and zone dividers) + message
    // + right cluster (with its │ seams) total 152, so the message fits and
    // must paint verbatim — elision may only fire when the arithmetic says
    // the message truly cannot fit.
    let backend = ratatui::backend::TestBackend::new(153, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer().clone();
    let a = buf.area;
    let last_row: String = (a.x..a.x + a.width)
        .map(|x| buf[(x, a.y + a.height - 1)].symbol().to_string())
        .collect();
    // The WHOLE message must be present: middle-elision keeps both ends, so a
    // substring from one end would pass even after a regression elided the
    // other.
    assert!(
        last_row.contains(app.status.as_str()),
        "a transient that fits beside the right cluster must paint verbatim, \
         not elide; the bar showed: {last_row:?}"
    );
}

/// #260: with auto-hide on, moving focus to the editor collapses the sidebar,
/// and a deliberate sidebar action brings it back and HOLDS it — the reveal
/// must not be undone by the focus change it performs itself.
#[test]
fn auto_hide_collapses_on_editor_focus_and_deliberate_actions_restore_it() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "fn main() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;

    // #302: the collapse is now delivered by the dwell tick rather than by the
    // focus move itself. What this test protects — that focusing the editor
    // DOES collapse, and that deliberate sidebar actions bring it back and
    // hold it — is unchanged; only the moment of the collapse moved.
    app.focus_pane(Pane::Editor);
    assert!(
        app.tick_sidebar_auto_hide_at(
            std::time::Instant::now() + crate::app::sidebar_dwell::AUTO_HIDE_DWELL
        ),
        "the grace delay elapses and the collapse fires"
    );
    assert!(!app.show_tree, "focusing the editor collapses the sidebar");

    // An activity-bar view switch is a deliberate sidebar action: it reveals
    // the sidebar AND lands focus there, so the pin it takes is released by
    // that same focus move — the sidebar stays open because focus is in it,
    // not because it is pinned.
    app.set_sidebar_view(SidebarView::Search);
    assert!(app.show_tree, "choosing a view reveals the sidebar");
    assert!(app.focus == Pane::Tree, "and puts focus in it");
    assert!(
        !app.sidebar_pinned_open,
        "focus landed in the sidebar, so the pin has done its job"
    );

    // Leaving again collapses.
    app.focus_pane(Pane::Editor);
    assert!(
        app.tick_sidebar_auto_hide_at(
            std::time::Instant::now() + crate::app::sidebar_dwell::AUTO_HIDE_DWELL
        ),
        "the collapse fires after the delay"
    );
    assert!(!app.show_tree, "leaving collapses it again");

    // Terminal focus collapses too, not just the editor.
    app.set_sidebar_view(SidebarView::Explorer);
    app.focus_pane(Pane::Tree);
    app.focus_pane(Pane::Terminal);
    assert!(
        app.tick_sidebar_auto_hide_at(
            std::time::Instant::now() + crate::app::sidebar_dwell::AUTO_HIDE_DWELL
        ),
        "and for a terminal too"
    );
    assert!(!app.show_tree, "a terminal is also 'not the sidebar'");
}

/// The flapping cases from #260: a seam drag, an open prompt or menu, and Zen
/// Mode must all suppress the collapse. Each is a state where the sidebar is
/// either being manipulated or deliberately borrowed.
#[test]
fn auto_hide_is_suppressed_while_dragging_prompting_or_in_zen_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;

    // Baseline: it would collapse.
    app.show_tree = true;
    assert!(app.sidebar_auto_hide_allowed());

    // Mid seam-drag the pointer is over the editor by definition; collapsing
    // under the cursor is exactly the flapping the issue calls out.
    app.splitter_drag = Some(SplitterDrag::Sidebar);
    assert!(!app.sidebar_auto_hide_allowed(), "a drag suppresses it");
    app.focus_pane(Pane::Editor);
    // Tick past the delay: since #302 defers the collapse, asserting
    // `show_tree` right after the focus move is true for any deferring
    // implementation and says nothing about the drag suppression.
    assert!(
        !app.tick_sidebar_auto_hide_at(std::time::Instant::now() + AUTO_HIDE_DWELL),
        "the drag suppresses the collapse even once the window elapses"
    );
    assert!(app.show_tree, "and the sidebar survives the drag");
    app.splitter_drag = None;

    // Zen Mode already owns chrome visibility and wins, as it does today.
    app.zen_mode = true;
    assert!(!app.sidebar_auto_hide_allowed(), "zen mode wins");
    app.zen_mode = false;

    // Off by default: a user who never opted in never loses their sidebar.
    app.sidebar_auto_hide = false;
    assert!(!app.sidebar_auto_hide_allowed());
    app.focus_pane(Pane::Editor);
    assert!(app.show_tree, "opt-in only");
}

/// The Customize Layout rows carry hardcoded re-open indices (each toggle
/// reopens the menu with itself selected), so inserting a row in the middle
/// silently mis-selects every row after it. #260's Auto-Hide row is appended
/// after Minimap for that reason; this pins the positions so a future insert
/// fails here rather than in someone's hands.
#[test]
fn customize_layout_toggle_rows_keep_their_reopen_indices() {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    let items = app.customize_layout_items();
    let label = |i: usize| match &items[i] {
        MenuEntry::Item { label, .. } => label.clone(),
        _ => panic!("row {i} is not an item"),
    };
    assert!(label(0).contains("Activity Bar"));
    assert!(label(1).contains("Primary Side Bar"));
    assert!(label(2).contains("Secondary Side Bar"));
    assert!(label(3).contains("Panel"));
    assert!(label(4).contains("Status Bar"));
    assert!(label(5).contains("Minimap"));
    assert!(
        label(6).contains("Auto-Hide Side Bar"),
        "the new row is appended, not inserted"
    );
}

/// #294 review: the palette and menu toggles must not drift. Both funnel
/// through one method, so they set the same state and report the same status;
/// previously the menu route set neither the status nor any persistence.
#[test]
fn both_auto_hide_toggle_routes_agree() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(!app.sidebar_auto_hide, "off by default");

    app.toggle_sidebar_auto_hide();
    assert!(app.sidebar_auto_hide);
    assert!(
        app.status.contains("on"),
        "the toggle reports its new state: {}",
        app.status
    );
    app.toggle_sidebar_auto_hide();
    assert!(!app.sidebar_auto_hide);
    assert!(
        app.status.contains("off"),
        "and reports off: {}",
        app.status
    );
}

/// #294 re-review found the blocker this pins: the pin was set only on the
/// palette route, while a real Cmd+B keypress is handled earlier and returns
/// before `Cmd` dispatch — so the headline reveal gesture revealed the panel
/// and the very next click into the editor collapsed it again. Driving the
/// key event is the whole point; every other test here sets the flag by hand
/// and so could not see it.
#[test]
fn the_real_cmd_b_keypress_reveals_and_holds_the_sidebar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = false;

    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.show_tree, "Cmd+B reveals the sidebar");

    // #302: each collapse is now delivered by the dwell tick. The guarantee
    // under test — that the reveal survives the focus move it performs, and is
    // one-shot rather than sticky — is unchanged.
    let dwell = crate::app::sidebar_dwell::AUTO_HIDE_DWELL;
    app.focus_pane(Pane::Editor);
    app.tick_sidebar_auto_hide_at(std::time::Instant::now() + dwell);
    assert!(
        app.show_tree,
        "and the reveal survives the next move to the editor"
    );

    // Still one-shot: the exemption is spent, so the following move collapses.
    app.focus_pane(Pane::Tree);
    app.focus_pane(Pane::Editor);
    app.tick_sidebar_auto_hide_at(std::time::Instant::now() + dwell);
    assert!(!app.show_tree, "the exemption was consumed, not sticky");

    // Ctrl+B is the same gesture on the other platform convention.
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.show_tree);
    app.focus_pane(Pane::Editor);
    app.tick_sidebar_auto_hide_at(std::time::Instant::now() + dwell);
    assert!(app.show_tree, "Ctrl+B pins exactly as Cmd+B does");
}

/// A pin taken while the feature is OFF must not be banked: it would sit
/// unconsumed and silently eat the first collapse after the user turns
/// auto-hide on, which reads as "the setting did nothing".
#[test]
fn a_reveal_while_auto_hide_is_off_does_not_bank_an_exemption() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = false;
    app.show_tree = false;

    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.show_tree);
    assert!(
        !app.sidebar_pinned_open,
        "no pin is banked while the feature is off"
    );

    app.sidebar_auto_hide = true;
    app.focus_pane(Pane::Editor);
    // #302: delivered by the tick, not the focus move. The guarantee under
    // test — that no exemption was banked while the feature was off, so the
    // first collapse after enabling it is not silently eaten — is unchanged.
    assert!(
        app.tick_sidebar_auto_hide_at(
            std::time::Instant::now() + crate::app::sidebar_dwell::AUTO_HIDE_DWELL
        ),
        "the collapse fires"
    );
    assert!(
        !app.show_tree,
        "so the first collapse after enabling it actually happens"
    );
}

/// #294 review: the reveal exemption is ONE-SHOT. Cmd+B reveals without
/// moving focus, so a sticky flag would never be cleared (focus never lands
/// on the tree) and auto-hide would be silently dead for the session.
///
/// #302 moved WHEN the exemption is spent — from the focus move itself to the
/// collapse the dwell later fires — but not WHETHER it is spent. The
/// one-shot guarantee this test exists to protect is unchanged, so the test
/// keeps its assertions and gains the tick that now delivers the collapse.
#[test]
fn the_reveal_exemption_is_consumed_rather_than_sticky() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;

    // Cmd+B-style reveal: exempt the next collapse.
    app.sidebar_pinned_open = true;
    app.focus_pane(Pane::Editor);
    let t0 = std::time::Instant::now();
    let fired = app.tick_sidebar_auto_hide_at(t0 + crate::app::sidebar_dwell::AUTO_HIDE_DWELL);
    assert!(!fired, "the exemption protects this collapse");
    assert!(app.show_tree, "so the sidebar is still up");
    assert!(
        !app.sidebar_pinned_open,
        "and is consumed rather than latching auto-hide off"
    );

    // The very next move collapses, proving the feature re-armed itself.
    app.focus_pane(Pane::Editor);
    let t1 = std::time::Instant::now();
    assert!(
        app.tick_sidebar_auto_hide_at(t1 + crate::app::sidebar_dwell::AUTO_HIDE_DWELL),
        "auto-hide is live again"
    );
    assert!(!app.show_tree, "and the sidebar went down");
}

/// #294 review: the suppression list must cover the modal states the codebase
/// already treats as modal — several are sidebar-targeting flows that are
/// their own fields rather than `Prompt`s, so checking `prompt` alone missed
/// exactly the ones the comment claimed were covered.
#[test]
fn auto_hide_yields_to_real_modal_overlays_and_explorer_drags() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;
    assert!(app.sidebar_auto_hide_allowed(), "baseline collapses");

    // A command palette is modal and may sit over sidebar space.
    app.command_palette = Some(crate::widgets::command_palette::CommandPalette::new());
    assert!(
        !app.sidebar_auto_hide_allowed(),
        "a modal overlay suppresses the collapse"
    );
    app.command_palette = None;

    // With no activity bar there is no on-screen way back to a collapsed
    // sidebar, so a mouse-only user would be stranded.
    app.activity_bar_visible = false;
    assert!(
        !app.sidebar_auto_hide_allowed(),
        "no reveal affordance means no auto-hide"
    );
    app.activity_bar_visible = true;

    // A focus move the user did not ask for is not a reason to hide chrome.
    // Tick PAST the grace delay: with the collapse deferred (#302), asserting
    // `show_tree` right after the focus move is trivially true for any
    // implementation that defers, suppressed or not. The suppression is only
    // observable once the window has elapsed.
    app.without_auto_hide(|a| a.focus_pane(Pane::Editor));
    assert!(
        !app.tick_sidebar_auto_hide_at(std::time::Instant::now() + AUTO_HIDE_DWELL),
        "a programmatic focus move must not collapse, even after the delay"
    );
    assert!(app.show_tree, "programmatic focus leaves the sidebar alone");
}

/// #302: a click that PASSES THROUGH the panel on its way to the editor must
/// not collapse it under the pointer. #294 collapsed on the focus change
/// itself, so the panel flapped; the collapse now waits out a grace delay, and
/// a return to the sidebar inside that window cancels it outright.
///
/// Written against `focus_pane` and `show_tree` alone, so it fails on the
/// missing BEHAVIOUR rather than on a missing method. The clock-injected
/// assertions live in the tests below.
#[test]
fn a_click_passing_through_the_sidebar_does_not_collapse_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;

    // Precondition: the collapse must be POSSIBLE, or "it stayed up" passes
    // vacuously against a sidebar that could never have gone down.
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: nothing is suppressing auto-hide, so a collapse is live"
    );

    // Focus crosses into the editor. Under #294 this collapsed immediately;
    // under #302 the collapse is only ARMED and the panel is still up.
    app.focus_pane(Pane::Editor);
    assert!(
        app.show_tree,
        "the sidebar stays up for the length of the grace delay"
    );

    // Focus comes back inside the window — the click was passing through.
    app.focus_pane(Pane::Tree);
    assert!(
        app.show_tree,
        "a click through the panel leaves it exactly where it was"
    );

    // Assert the DISARM directly. Ticking alone does not pin it: with focus
    // parked on `Pane::Tree` the tick declines via its own `focus == Pane::Tree`
    // early return, so this test passes even with the disarm deleted. The
    // dwell's own state is the only thing that distinguishes "cancelled" from
    // "declined for an unrelated reason".
    assert!(
        !app.sidebar_dwell.armed(),
        "the return to the sidebar cancelled the pending collapse outright"
    );

    // And nothing fires once the window elapses.
    assert!(
        !app.tick_sidebar_auto_hide_at(std::time::Instant::now() + AUTO_HIDE_DWELL),
        "no collapse after the deadline"
    );
    assert!(
        app.show_tree,
        "so the panel is still up after the window closes"
    );
}

/// #302: a focus move that stays away must still collapse — the grace delay is
/// a delay, not a reprieve. Public-surface half: after the move the panel is
/// still up but a collapse is PENDING, which `tick_sidebar_auto_hide` will
/// deliver. Timing precision is covered by the unit tests on the timer itself.
#[test]
fn leaving_the_sidebar_arms_a_collapse_rather_than_doing_it_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: a collapse is possible here"
    );

    app.focus_pane(Pane::Editor);
    assert!(app.show_tree, "not collapsed on the focus change itself");

    // Several more focus moves land inside the grace window. None of them may
    // collapse it, and — the trap the issue names — none may push the
    // deadline out either, which the timer's own unit tests pin.
    //
    // Stamped AFTER the arming move on purpose. `due` is `>=`, so an exact
    // deadline would be fine, but a stamp taken BEFORE `focus_pane` is strictly
    // earlier than the anchor `focus_pane` records — `armed_at + DWELL` would
    // then land before the real deadline and the tick would correctly decline.
    // Taken here it is a hair LATER than the anchor, so the tick is genuinely
    // due; the re-arming bug this guards against would push the deadline a full
    // AUTO_HIDE_DWELL out, far beyond that sub-microsecond slack.
    let armed_at = std::time::Instant::now();
    app.focus_pane(Pane::Terminal);
    app.focus_pane(Pane::Editor);
    assert!(app.show_tree, "still up while the window is open");

    // The deadline is measured from the FIRST arm, not the last focus move, so
    // a tick at the original deadline still fires. Without this the pane hops
    // above prove nothing: `show_tree` is set either way before any tick, and
    // a re-arming bug that pushed the deadline out would pass unnoticed.
    assert!(
        app.tick_sidebar_auto_hide_at(armed_at + AUTO_HIDE_DWELL),
        "the hops did not push the deadline out — it fires on the original one"
    );
    assert!(!app.show_tree, "and the collapse actually happened");
}

// Sidebar auto-hide tests, a trap worth knowing before you write one: routing
// focus through `Pane::Tree` CLEARS the one-shot reveal pin by design (moving
// into the sidebar means you are using it, so the exemption is released). A
// Tree->Editor round trip is the obvious way to re-arm a collapse, and it
// silently wipes the flag under test. Re-arm via `Pane::Terminal` instead when
// the pin has to survive. This is invisible from the test side and cost one
// misdiagnosed green.

/// #302, the part that is new work rather than a port: #294's one-shot pin is
/// consumed by "the next collapse attempt", but with a timer that stops being
/// a single well-defined event. A deliberate reveal must not lose its
/// protection to an attempt the dwell then CANCELS.
#[test]
fn arming_a_collapse_does_not_consume_the_reveal_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;

    // Precondition: a collapse is otherwise live here, so the exemption is
    // protecting against something real rather than a case that would never
    // have fired.
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: without the pin, this focus move would collapse"
    );

    // A deliberate reveal (Cmd+B) banks the one-shot exemption.
    app.sidebar_pinned_open = true;

    // Focus leaves: this ARMS a collapse but must not spend the exemption,
    // because the collapse has not happened and may yet be cancelled.
    app.focus_pane(Pane::Editor);
    assert!(
        app.sidebar_pinned_open,
        "arming a collapse must not spend the exemption — only firing one does"
    );
    assert!(app.show_tree, "and the sidebar is still up");
}

/// #302: the pin must survive a tick that declines to collapse for a reason
/// that has nothing to do with the pin. Every EARLY decline in
/// `tick_sidebar_auto_hide_at` returns before spending it; the LATE decline —
/// `sidebar_auto_hide_allowed()` being false — sits after the take, so a
/// transient suppression (a seam drag, an open palette, a modal) burns a pin
/// the user never got the benefit of. The suppression clears a moment later
/// and the next collapse proceeds unpinned.
#[test]
fn a_suppressed_tick_does_not_spend_the_reveal_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;

    // Precondition, checked before either the pin or the suppression is set:
    // a collapse is otherwise live here, so both are suppressing something
    // real rather than a case that was never going to fire.
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: without pin or drag, this focus move would collapse"
    );

    // A deliberate reveal (Cmd+B) banks the one-shot exemption.
    app.sidebar_pinned_open = true;

    // Focus leaves: arms the collapse.
    app.focus_pane(Pane::Editor);

    // The user starts dragging the sidebar seam. This suppresses the collapse
    // on its own — independently of the pin — and is transient: it ends when
    // they release the mouse.
    app.splitter_drag = Some(SplitterDrag::Sidebar);

    // Precondition: the drag alone is enough to stop this collapse, so the
    // tick below declines for a reason that is not the pin.
    assert!(
        !app.sidebar_auto_hide_allowed(),
        "precondition: the seam drag suppresses the collapse by itself"
    );

    let now = std::time::Instant::now();
    let collapsed = app.tick_sidebar_auto_hide_at(now + AUTO_HIDE_DWELL);

    assert!(!collapsed, "the drag suppressed this collapse");
    assert!(app.show_tree, "so the sidebar is still up");
    assert!(
        app.sidebar_pinned_open,
        "a collapse stopped by an unrelated suppression must not spend the \
         one-shot exemption — the user never saw the collapse it was banked for"
    );

    // The pin surviving is only half the contract: it must still be SPENDABLE.
    // A fix that keeps the pin by making the collapse permanently unreachable
    // satisfies the assertion above and is strictly worse than the bug. So
    // release the drag and let the collapse the pin was banked for happen.
    //
    // Re-arm WITHOUT routing focus through `Pane::Tree`: moving into the
    // sidebar deliberately releases the pin (mod.rs, the `Pane::Tree` arm), so
    // a Tree->Editor round trip would clear the very flag under test. Focus is
    // already on Editor here; the dwell is what needs re-arming.
    app.splitter_drag = None;
    app.focus_pane(Pane::Terminal);
    let pinned_collapse = app.tick_sidebar_auto_hide_at(now + AUTO_HIDE_DWELL * 2);
    assert!(!pinned_collapse, "the pin exempts this one");
    assert!(app.show_tree, "so the sidebar survives it");
    assert!(
        !app.sidebar_pinned_open,
        "and NOW the exemption is spent — it was banked for exactly this"
    );

    app.focus_pane(Pane::Editor);
    let unpinned_collapse = app.tick_sidebar_auto_hide_at(now + AUTO_HIDE_DWELL * 3);
    assert!(
        unpinned_collapse,
        "with the exemption spent, the next collapse fires — a pin that can \
         never be spent is a sidebar that can never auto-hide"
    );
    assert!(!app.show_tree, "and the sidebar is finally down");
}

/// #302: a reveal under Zen Mode must not bank a pin. No collapse can fire
/// while Zen owns the chrome, so the pin would sit unconsumed and then eat the
/// first legitimate collapse after Zen exits — the exact failure
/// `toggle_side_bar` already guards against when auto-hide is off.
#[test]
fn a_reveal_under_zen_mode_banks_no_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = false;

    // Precondition: with Zen off, this same reveal DOES bank a pin — so the
    // assertion below is about Zen, not about the reveal failing generally.
    app.toggle_side_bar();
    assert!(
        app.sidebar_pinned_open,
        "precondition: a normal reveal banks the one-shot pin"
    );

    // Back to a hidden sidebar, now under Zen Mode.
    app.toggle_side_bar();
    app.zen_mode = true;
    app.activity_bar_visible = false;

    app.toggle_side_bar();
    assert!(app.show_tree, "the reveal still shows the sidebar");
    assert!(
        !app.sidebar_pinned_open,
        "but banks no pin: nothing can spend it until Zen exits, and a pin \
         held that long eats a collapse the user never connected to it"
    );
}

/// #302: a collapse blocked by a TRANSIENT suppression must be retried once
/// that suppression lifts, not abandoned. The tick disarming before it asks
/// `sidebar_auto_hide_allowed()` drops the pending collapse on the floor, and
/// nothing re-arms it but a fresh focus move — so a user whose palette was open
/// during the grace window keeps a sidebar that never auto-hides again.
#[test]
fn a_transiently_suppressed_collapse_is_retried_once_the_suppression_lifts() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;

    // Precondition: a collapse is genuinely live here.
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: nothing is suppressing auto-hide yet"
    );

    // NOTE: `maybe_auto_hide_sidebar` stamps the anchor with its OWN
    // `Instant::now()`, so a deadline computed from a timestamp taken HERE is
    // fractionally too early and the tick declines as not-yet-due — which
    // looks exactly like the suppression working. Anchor the deadlines to a
    // point comfortably past any real arm instead.
    app.focus_pane(Pane::Editor);
    let armed_at = std::time::Instant::now();

    // A transient suppression appears DURING the grace window.
    app.command_palette = Some(crate::widgets::command_palette::CommandPalette::new());
    assert!(
        !app.sidebar_auto_hide_allowed(),
        "precondition: the open palette suppresses the collapse"
    );

    // The window elapses while it is still open: correctly declines.
    // Precondition: the deadline really has elapsed, so a decline here is the
    // palette and not the timer. Proven by the far-future tick at the end,
    // which fires with the identical deadline once the palette is gone.
    assert!(
        !app.tick_sidebar_auto_hide_at(armed_at + AUTO_HIDE_DWELL * 5),
        "the palette suppresses this collapse"
    );
    assert!(app.show_tree, "so the sidebar is still up");

    // The user closes the palette. NO new focus move — focus never left the
    // editor, so nothing re-arms; the pending collapse must survive on its own.
    app.command_palette = None;
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: with the palette closed the collapse is live again"
    );

    assert!(
        app.tick_sidebar_auto_hide_at(armed_at + AUTO_HIDE_DWELL * 5),
        "the collapse the palette deferred must still fire once it closes — \
         a transient suppression delays the collapse, it does not cancel it"
    );
    assert!(!app.show_tree, "and the sidebar is finally down");
}

/// #302: toggling the auto-hide SETTING drops any pending collapse. The
/// function's own comment promises turning it on "takes effect at the next
/// focus move rather than retroactively yanking it away" — a dwell left armed
/// across an off/on round trip breaks that promise on the next idle tick, with
/// no focus move of the user's own.
#[test]
fn toggling_the_auto_hide_setting_drops_a_pending_collapse() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: collapse is live"
    );

    let armed_at = std::time::Instant::now();
    app.focus_pane(Pane::Editor);
    assert!(
        app.sidebar_dwell.armed(),
        "precondition: a collapse is pending"
    );

    // The user turns the feature off, then changes their mind.
    app.toggle_sidebar_auto_hide();
    app.toggle_sidebar_auto_hide();
    assert!(app.sidebar_auto_hide, "precondition: back on");

    assert!(
        !app.tick_sidebar_auto_hide_at(armed_at + AUTO_HIDE_DWELL * 1000),
        "the collapse armed before the setting was touched must not fire"
    );
    assert!(app.show_tree, "so the sidebar is still up");
}

/// #302: the activity bar is a STRUCTURAL suppression that flips on its own,
/// independently of Zen Mode. A collapse armed while it is hidden must not sit
/// armed and then fire — spending a banked reveal pin — on the first idle tick
/// after the bar comes back.
#[test]
fn hiding_the_activity_bar_drops_a_pending_collapse_and_its_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;

    // A deliberate reveal banks the one-shot pin while the bar is visible.
    app.sidebar_pinned_open = true;
    let armed_at = std::time::Instant::now();
    app.focus_pane(Pane::Editor);
    assert!(
        app.sidebar_dwell.armed(),
        "precondition: a collapse is pending"
    );

    // The user hides the activity bar, then shows it again.
    let root = app.active_workspace_root();
    app.dispatch_menu_action(MenuAction::ToggleActivityBar, root.clone());
    assert!(!app.activity_bar_visible, "precondition: the bar is hidden");
    app.dispatch_menu_action(MenuAction::ToggleActivityBar, root);
    assert!(app.activity_bar_visible, "precondition: the bar is back");

    // Two things mask this test if left alone, and either alone is enough.
    // `dispatch_menu_action` ends by opening the Customize Layout popup, and
    // `sidebar_auto_hide_allowed()` bails on `context_menu.is_some()` — so the
    // tick declines before reaching anything this test is named for. And the
    // pin banked above would absorb the collapse anyway, leaving `!fired` and
    // `show_tree` both true while the pin is silently spent, which IS the
    // failure the hunk exists to prevent. Assert the dwell and the pin
    // directly, with the popup cleared.
    app.context_menu = None;
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: nothing else is suppressing the collapse now"
    );
    assert!(
        !app.sidebar_dwell.armed(),
        "the activity-bar toggle cancelled the pending collapse"
    );
    assert!(
        !app.tick_sidebar_auto_hide_at(armed_at + AUTO_HIDE_DWELL * 1000),
        "an idle tick must not collapse after the bar returns"
    );
    assert!(app.show_tree, "so the sidebar is still up");
    assert!(
        !app.sidebar_pinned_open,
        "and hiding the bar dropped the pin rather than holding an \
         unconsumable one across a period where no collapse can fire — the \
         same reason `toggle_side_bar` refuses to bank one in that state"
    );

    // The pin was dropped, so the next deliberate focus move collapses as the
    // user expects rather than being silently exempted by a stale reveal.
    let t1 = std::time::Instant::now();
    app.focus_pane(Pane::Tree);
    app.focus_pane(Pane::Editor);
    assert!(
        app.tick_sidebar_auto_hide_at(t1 + AUTO_HIDE_DWELL * 2),
        "a fresh focus move after the bar returns collapses — no stale pin ate it"
    );
}

/// #302's actual payload: the collapse WAITS. Every other app-level test ticks
/// at the deadline or beyond, so all of them pass against a zero-length window
/// — i.e. against #294's immediate collapse delivered through a tick. Deleting
/// the `due()` check leaves them all green. This is the only test that fails
/// when the grace delay itself is removed, which is what makes it the
/// acceptance test for the feature rather than for its plumbing.
#[test]
fn a_collapse_does_not_fire_inside_the_grace_window() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: a collapse is live here"
    );

    // Stamp BEFORE the arm. `maybe_auto_hide_sidebar` anchors on its own
    // `Instant::now()`, so a deadline derived from a later stamp could land
    // outside the real window and decline as not-yet-due for the wrong reason.
    let before = std::time::Instant::now();
    app.focus_pane(Pane::Editor);

    // Half a window in: the collapse is pending and must not have fired.
    assert!(
        !app.tick_sidebar_auto_hide_at(before + AUTO_HIDE_DWELL / 2),
        "a tick INSIDE the grace window must not collapse — this is the delay"
    );
    assert!(app.show_tree, "so the sidebar is still up mid-window");
    assert!(
        app.sidebar_dwell.armed(),
        "and the collapse is still pending rather than cancelled"
    );

    // Past the window, with focus still away: now it fires.
    assert!(
        app.tick_sidebar_auto_hide_at(before + AUTO_HIDE_DWELL * 3),
        "once the window elapses the collapse delivers"
    );
    assert!(!app.show_tree, "and the sidebar is down");
}

/// #302: a deliberate Cmd+B reveal supersedes a pending automatic collapse.
/// Once a declined tick stays ARMED (so a transient suppression defers rather
/// than cancels), the dwell can outlive the sidebar state it was armed
/// against: the user hides and re-reveals by hand, the reveal banks its
/// one-shot pin, and the very next idle frame tick finds the stale dwell due
/// and spends that pin on a collapse nobody ever saw. The next focus move then
/// takes the sidebar down — the #294 bug, reached by a different door.
#[test]
fn a_manual_reveal_cancels_a_pending_collapse() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: a collapse is live here"
    );

    app.focus_pane(Pane::Editor);
    let armed_at = std::time::Instant::now();

    // A modal defers the collapse. It stays armed by design (that is the
    // transient-suppression retry this PR added).
    app.command_palette = Some(crate::widgets::command_palette::CommandPalette::new());
    assert!(
        !app.tick_sidebar_auto_hide_at(armed_at + AUTO_HIDE_DWELL * 5),
        "precondition: the palette defers this collapse"
    );
    app.command_palette = None;

    // The user hides and re-reveals the sidebar by hand.
    app.toggle_side_bar();
    app.toggle_side_bar();
    assert!(
        app.show_tree && app.sidebar_pinned_open,
        "precondition: the manual reveal banked a one-shot pin"
    );

    // An IDLE frame tick — no focus move, no gesture at all.
    assert!(
        !app.tick_sidebar_auto_hide_at(armed_at + AUTO_HIDE_DWELL * 6),
        "an idle tick must not collapse a sidebar the user just revealed"
    );
    assert!(
        app.sidebar_pinned_open,
        "and must not spend the pin on a collapse the user never saw"
    );

    // So the deliberate reveal still holds across the next move out.
    app.focus_pane(Pane::Editor);
    assert!(
        !app.tick_sidebar_auto_hide_at(armed_at + AUTO_HIDE_DWELL * 20),
        "the reveal survives the next focus move, as #294 guarantees"
    );
    assert!(app.show_tree, "the sidebar is still up");
}

/// #302: disabling and re-enabling auto-hide drops a pin banked beforehand.
/// `toggle_side_bar` refuses to bank one while the feature is off — "a pin set
/// while the feature is off would sit unconsumed and silently eat the first
/// collapse after they turn it on" — and a pin banked BEFORE the user disables
/// it reaches that same stranded state by the other route. The existing
/// `a_reveal_while_auto_hide_is_off_does_not_bank_an_exemption` covers
/// off -> reveal -> enable; this is reveal -> disable -> enable.
#[test]
fn disabling_auto_hide_drops_a_pin_banked_before_it_was_turned_off() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = false;

    app.toggle_side_bar();
    assert!(
        app.show_tree && app.sidebar_pinned_open,
        "precondition: the reveal banked a pin while the feature was on"
    );

    app.toggle_sidebar_auto_hide();
    assert!(!app.sidebar_auto_hide, "precondition: the feature is off");
    assert!(
        !app.sidebar_pinned_open,
        "turning the feature off drops the pin: nothing can consume it while \
         off, so it would eat the first collapse after re-enabling"
    );

    app.toggle_sidebar_auto_hide();
    assert!(app.sidebar_auto_hide, "precondition: back on");
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: a collapse is live again"
    );
    let t0 = std::time::Instant::now();
    app.focus_pane(Pane::Editor);
    assert!(
        app.tick_sidebar_auto_hide_at(t0 + AUTO_HIDE_DWELL * 2),
        "and the first collapse after re-enabling fires — no stranded pin ate it"
    );
}

/// #302: Zen entry must drop the reveal pin as well as the dwell. Zen sets
/// BOTH structural conditions — `activity_bar_visible = false` and
/// `zen_mode = true` — so `toggle_side_bar`'s reason for refusing to bank a
/// pin there ("no collapse can fire for as long as it lasts") applies in full
/// to a pin already held.
///
/// This is a REGRESSION guard, not just an edge case: on main
/// `maybe_auto_hide_sidebar` did an unconditional `mem::take` of the pin on
/// every focus move into the editor or terminal, so any move during or after
/// Zen cleared it. Moving consumption to fire time removed that incidental
/// cleanup, and nothing replaced it at the Zen boundary.
#[test]
fn entering_zen_mode_drops_the_reveal_pin_as_well_as_the_dwell() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = false;

    // A deliberate Cmd+B reveal banks the one-shot exemption.
    app.toggle_side_bar();
    assert!(
        app.show_tree && app.sidebar_pinned_open,
        "precondition: the reveal banked a pin"
    );

    app.toggle_zen_mode();
    assert!(app.zen_mode, "precondition: Zen is on");
    assert!(
        !app.sidebar_pinned_open,
        "Zen entry drops the pin: no collapse can fire while Zen owns the \
         chrome, so a pin held across it is unconsumable and eats the first \
         real collapse afterwards"
    );

    // The user-visible consequence: after Zen, the next deliberate focus move
    // collapses as expected rather than being silently exempted.
    app.toggle_zen_mode();
    assert!(
        !app.zen_mode && app.show_tree,
        "precondition: Zen off, sidebar back"
    );
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: a collapse is live again"
    );
    let t0 = std::time::Instant::now();
    app.focus_pane(Pane::Editor);
    assert!(
        app.tick_sidebar_auto_hide_at(t0 + AUTO_HIDE_DWELL * 2),
        "and it fires — no stale pin from before Zen absorbed it"
    );
}

/// #302: Zen Mode is a STRUCTURAL suppression, not a transient one. The tick
/// stays armed when a drag or a palette defers a collapse, because those end in
/// moments — but Zen can last the session, and a dwell left armed across it
/// fires on the very frame Zen exits, collapsing the sidebar for a focus move
/// the user made an hour earlier.
#[test]
fn entering_zen_mode_cancels_a_pending_collapse_rather_than_deferring_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: a collapse is live here"
    );

    app.focus_pane(Pane::Editor);
    let armed_at = std::time::Instant::now();

    // Zen takes over the chrome while the collapse is still pending.
    app.toggle_zen_mode();
    assert!(app.zen_mode, "precondition: Zen is on");
    assert!(!app.show_tree, "precondition: Zen hid the sidebar");

    // The user works in Zen for a long time, then leaves.
    app.toggle_zen_mode();
    assert!(!app.zen_mode, "precondition: Zen is off");
    assert!(
        app.show_tree,
        "precondition: leaving Zen restored the sidebar"
    );

    // A tick far past the original deadline must not fire: that collapse
    // belonged to a focus move from before Zen.
    assert!(
        !app.tick_sidebar_auto_hide_at(armed_at + AUTO_HIDE_DWELL * 1000),
        "a collapse armed before Zen must not fire on the frame Zen exits"
    );
    assert!(app.show_tree, "so the restored sidebar stays up");
}

/// #302: a dwell must not be ARMED while a structural suppression is already
/// in force. This is the one instance of the class that no entry-site disarm
/// can catch, and it is worth being explicit about why.
///
/// `entering_zen_mode_cancels_a_pending_collapse_rather_than_deferring_it`
/// covers arming BEFORE Zen: there is a transition, Zen entry sees it, and the
/// entry hook disarms. Here the dwell is armed DURING Zen — Cmd+B still works
/// inside Zen, and a focus move afterwards arms as usual — so the suppression
/// is never entered while the dwell is live. There is no transition, so there
/// is no boundary to hook, and every existing disarm site is structurally
/// blind to it.
///
/// The consequence is user-visible and delayed: the arm declines at fire time
/// via `!sidebar_auto_hide_allowed()` and stays armed BY DESIGN (transient
/// suppressions must retry). Zen is not transient. An hour later the user
/// leaves Zen and the next 8ms tick collapses the sidebar for a focus move
/// they made an hour earlier, silently spending any pin with it.
///
/// The fix is therefore not a sixth boundary: `maybe_auto_hide_sidebar` must
/// refuse to arm under a structural suppression at all, mirroring what
/// `toggle_side_bar` already does for the pin at the same four conditions.
#[test]
fn arming_inside_zen_mode_does_not_collapse_the_sidebar_when_zen_exits() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = false;

    // Into Zen FIRST, so there is no pending dwell for Zen entry to cancel.
    app.toggle_zen_mode();
    assert!(app.zen_mode, "precondition: Zen is on");
    assert!(
        !app.sidebar_auto_hide_allowed(),
        "precondition: Zen structurally suppresses the collapse"
    );

    // Cmd+B works inside Zen, and the user opens the sidebar to look at a file.
    app.toggle_side_bar();
    assert!(app.show_tree, "precondition: the sidebar is up inside Zen");
    assert!(
        !app.sidebar_pinned_open,
        "precondition: toggle_side_bar already refuses to bank a pin under a \
         structural suppression — this test is about the DWELL, which does not"
    );

    // Focus moves away, exactly as it would outside Zen. This is the arm that
    // no entry site can see: the suppression was already in force.
    let t0 = std::time::Instant::now();
    app.focus_pane(Pane::Editor);
    // The main loop, faithfully: ticks throughout Zen, well past the grace
    // delay. None may collapse, and — the part two previous fixes got wrong —
    // none may destroy the pending collapse either, because nothing would
    // re-arm it afterwards.
    for i in 0..50u32 {
        assert!(
            !app.tick_sidebar_auto_hide_at(
                t0 + AUTO_HIDE_DWELL * 2 + std::time::Duration::from_millis(8 * i as u64)
            ),
            "no collapse fires inside Zen: Zen owns the chrome (tick {i})"
        );
    }
    assert!(
        app.sidebar_dwell.armed(),
        "and the dwell SURVIVES those ticks. Cancelling on a suppressed tick \
         is indistinguishable from refusing to arm — production ticks every \
         8ms, so the cancel lands 8ms after the arm, and nothing re-arms once \
         a structural suppression lifts"
    );

    // The user-visible failure, spelled out. Zen exit RESTORES the pre-Zen
    // snapshot, so the sidebar this test opened inside Zen is closed again on
    // the way out — that part is by design and not what is under test. Reopen
    // it, then check that no stale collapse from the in-Zen focus move is still
    // pending to fire on the next tick.
    app.toggle_zen_mode();
    assert!(!app.zen_mode, "precondition: Zen is off again");
    app.show_tree = true;
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: a collapse is structurally possible again"
    );
    // The claim this test exists for: no STALE collapse on the frame Zen exits.
    // The dwell is still armed, but each suppressed tick re-stamped it, so its
    // deadline is measured from the last moment a collapse was impossible —
    // not from the focus move made before Zen. The user gets the full grace
    // period from a moment they can perceive: the chrome coming back.
    assert!(
        !app.tick_sidebar_auto_hide_at(t0 + AUTO_HIDE_DWELL * 2),
        "and no stale collapse fires after Zen exits, for a focus move made \
         while Zen was still on"
    );
    assert!(
        app.show_tree,
        "the sidebar is still up: the user asked for it and never asked otherwise"
    );
    // But the collapse is not LOST either — that was the round-one and
    // round-two defect. Once the re-stamped grace period elapses it fires.
    let after_zen = t0 + AUTO_HIDE_DWELL * 2 + std::time::Duration::from_millis(8 * 49);
    assert!(
        app.tick_sidebar_auto_hide_at(after_zen + AUTO_HIDE_DWELL * 2),
        "and once the grace period from Zen exit elapses, it does fire: \
         retained, not discarded"
    );
}

/// #302 REGRESSION GUARD, round three. The two previous fixes each destroyed
/// the state the recovery needs, and the guard that was supposed to catch that
/// COULD NOT, because it never ticked during the suppression.
///
/// `main_loop` calls `tick_sidebar_auto_hide()` unconditionally on an 8ms
/// cadence (mod.rs, `event::poll(Duration::from_millis(8))`). So in production
/// a tick lands within 8ms of arming — and again, and again, for as long as the
/// suppression lasts. Any design that cancels the dwell on a suppressed tick
/// has cancelled it 8ms after it was armed, which is indistinguishable from
/// refusing to arm at all.
///
/// This test therefore TICKS DURING THE SUPPRESSED PERIOD, many times, exactly
/// as the main loop would. That single property is what separates it from the
/// vacuous guard it replaces: the previous version set the flag, cleared it,
/// and ticked once afterwards — a sequence production never produces.
#[test]
fn auto_hide_recovers_after_the_activity_bar_comes_back() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_auto_hide = true;
    app.show_tree = true;
    app.activity_bar_visible = false;
    app.focus_pane(Pane::Tree);

    assert!(
        !app.sidebar_auto_hide_allowed(),
        "precondition: a hidden activity bar structurally suppresses the collapse"
    );

    let t0 = std::time::Instant::now();
    app.focus_pane(Pane::Editor);

    // The main loop, faithfully: ticks every 8ms while the bar stays hidden,
    // and — the part that makes this a real guard — PAST the grace delay.
    //
    // Ticking only within AUTO_HIDE_DWELL proves nothing: the tick returns at
    // its `due` check before it ever reaches the structural branch, so a
    // cancel-on-suppressed-tick defect is unreachable and the test passes
    // against the very design it exists to reject. That was the third version
    // of this guard and it had no teeth; teeth-proving it against the round-two
    // mutation is what exposed that. Production ticks for as long as the
    // suppression lasts, which is minutes or hours, so the ticks that matter
    // are the ones AFTER the deadline has elapsed.
    for i in 0..50u32 {
        let t = t0 + AUTO_HIDE_DWELL * 2 + std::time::Duration::from_millis(8 * i as u64);
        assert!(
            !app.tick_sidebar_auto_hide_at(t),
            "no collapse may fire while the bar is hidden (tick {i})"
        );
    }
    assert!(
        app.sidebar_dwell.armed(),
        "and the dwell SURVIVES them: cancelling on a suppressed tick is what \
         strands the feature, because nothing re-arms once the bar returns"
    );
    assert!(
        app.show_tree,
        "precondition: still open after 50 suppressed ticks"
    );

    // An hour later the user brings the activity bar back. Focus never moved —
    // it is still the editor — so no gesture will arm anything, and production
    // guards `if self.focus != Pane::Editor` before calling `focus_pane`.
    app.activity_bar_visible = true;
    assert!(
        app.focus == Pane::Editor,
        "precondition: focus never moved, so nothing re-arms by gesture"
    );
    assert!(
        app.sidebar_auto_hide_allowed(),
        "precondition: the suppression has lifted"
    );

    let last_tick = t0 + AUTO_HIDE_DWELL * 2 + std::time::Duration::from_millis(8 * 49);
    assert!(
        app.tick_sidebar_auto_hide_at(last_tick + AUTO_HIDE_DWELL * 2),
        "auto-hide recovers once the bar returns: the pending collapse fires \
         rather than the feature staying dead for the session"
    );
    assert!(!app.show_tree, "and the sidebar actually collapsed");
}

#[test]
fn a_strip_on_the_sidebar_seam_still_unfolds_its_pane() {
    // #468: the default terminal is the LEFTMOST pane, and its strip sits on
    // the editor column's left edge, which is exactly the column the sidebar
    // seam claims for a resize drag. That hit-test runs before the strip's,
    // so a click on the left pane's strip started a drag and the pane could
    // never be brought back; a pane opened to the right sat clear of the seam
    // and unfolded fine. Both sidebar positions are covered because the seam
    // moves with the side bar: on the right it is the column past the editor,
    // and its two-column grab zone reaches the LAST pane's strip instead.
    for &pos in &[SideBarPosition::Left, SideBarPosition::Right] {
        let (_tmp, mut app, mut term) = app_with_terminal_panes(2);
        app.side_bar_position = pos;
        let idx = match pos {
            SideBarPosition::Left => 0,
            SideBarPosition::Right => 1,
        };
        app.toggle_terminal_collapse(idx);
        term.draw(|f| app.render(f)).unwrap();

        let strip = app.terminal_strip_rects[idx];
        assert_eq!(strip.width, 1, "{pos:?}: pane {idx} is folded to a strip");
        let seam = app
            .sidebar_splitter_x
            .expect("the side bar is shown, so its seam exists");
        // The whole point: the strip must lie inside the seam's grab zone
        // (the seam column and the one left of it), or this proves nothing.
        assert!(
            strip.x == seam || strip.x + 1 == seam,
            "{pos:?}: precondition - strip x={} must meet the seam at x={seam}",
            strip.x
        );

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            strip.x,
            strip.y + strip.height / 2,
        ));
        assert!(
            !app.terminals[idx].collapsed,
            "{pos:?}: the strip on the seam is still the way back out"
        );
        assert_eq!(
            app.splitter_drag, None,
            "{pos:?}: a click on a strip is not a sidebar resize"
        );
    }
}

/// The release is conditional on focus, not global (#304): the same
/// chord keeps its croft meaning everywhere else, so the rule is about
/// when the chord is unambiguous rather than a preference.
#[test]
fn ctrl_b_still_toggles_the_side_bar_outside_the_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    let before = app.show_tree;
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .unwrap();
    assert_ne!(
        app.show_tree, before,
        "with the editor focused, Ctrl+B is still croft's"
    );
}

#[test]
fn maximize_ignores_the_collapse_flags_and_gives_them_back_on_exit() {
    // The two gestures are orthogonal: entering maximize does not clear the
    // flags, and leaving it restores whatever folding was in force.
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.toggle_terminal_collapse(2);
    app.active_terminal = 0;
    app.toggle_terminal_pane_maximize();
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.terminals[2].collapsed,
        "maximize must not silently clear the folding"
    );
    assert_eq!(
        app.terminal_strip_rects
            .iter()
            .filter(|r| r.width > 0)
            .count(),
        0,
        "but no strip is painted while maximize owns the panel"
    );

    app.toggle_terminal_pane_maximize();
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(
        app.terminal_strip_rects[2].width, 1,
        "leaving maximize brings the strip back"
    );
}

#[test]
fn leaving_maximize_never_lands_focus_on_a_folded_pane() {
    // The rail lists every pane, folded ones included, and a rail click sets
    // `active_terminal` with no collapse check. While maximized that is
    // harmless - the flags are ignored and the pane is painted full width -
    // but leaving maximize drops focus onto a one-column strip.
    let (_tmp, mut app, mut term) = app_with_terminal_panes(3);
    app.toggle_terminal_collapse(2);
    app.active_terminal = 0;
    app.toggle_terminal_pane_maximize();
    term.draw(|f| app.render(f)).unwrap();

    // Hand the maximized pane to the folded terminal the way the rail does.
    app.active_terminal = 2;
    app.toggle_terminal_pane_maximize();
    assert!(
        !app.terminal_pane_maximized,
        "precondition: back to the split"
    );
    assert!(
        !app.terminals[app.active_terminal].collapsed,
        "leaving maximize must not leave focus on a strip"
    );
}

/// #682: small chrome images go out when new, changed, moved, back from
/// hiding, or (iTerm2) repainted around; not on every frame.
#[test]
fn a_chrome_image_is_sent_only_when_it_could_be_missing() {
    use crate::iterm2_inline::InlineImageProtocol;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let at = Rect::new(2, 2, 3, 1);
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
    app.drawn_buffer = Some(buf.clone());
    app.inline_protocol = InlineImageProtocol::ITerm2;
    assert!(app.chrome_image_due("k", "img", at), "new");
    app.end_chrome_flush();
    assert!(!app.chrome_image_due("k", "img", at), "unchanged frame");
    app.end_chrome_flush();
    assert!(app.chrome_image_due("k", "img2", at), "changed");
    assert!(
        app.chrome_image_due("k", "img2", Rect::new(3, 2, 3, 1)),
        "moved"
    );
    app.end_chrome_flush();
    buf[(2, 2)].set_symbol("x");
    app.drawn_buffer = Some(buf.clone());
    assert!(
        app.chrome_image_due("k", "img2", Rect::new(3, 2, 3, 1)),
        "a neighbour repainted on iTerm2"
    );
    app.end_chrome_flush();
    app.end_chrome_flush();
    assert!(
        app.chrome_image_due("k", "img2", Rect::new(3, 2, 3, 1)),
        "back after a frame hidden"
    );
    app.end_chrome_flush();
    app.inline_protocol = InlineImageProtocol::Kitty;
    assert!(app.chrome_image_due("k", "img2", Rect::new(3, 2, 3, 1)));
    app.end_chrome_flush();
    buf[(2, 3)].set_symbol("y");
    app.drawn_buffer = Some(buf);
    assert!(
        !app.chrome_image_due("k", "img2", Rect::new(3, 2, 3, 1)),
        "Kitty ignores neighbours"
    );
}

/// #852: every bottom-panel tab is reachable from the keyboard. Each tab
/// names the palette command that shows it (`show_command` has no
/// catch-all, so a new tab cannot compile without one, and the strip paints
/// from `ALL`, so a clickable tab is always in this loop); here each command
/// must be in the palette and must land on its tab, panel shown and
/// focused, from a hidden panel on another tab.
#[test]
fn every_bottom_panel_tab_is_reachable_from_the_palette() {
    use crate::widgets::command_palette::ALL_COMMANDS;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for tab in BottomPanelTab::ALL {
        let cmd = tab.show_command();
        assert!(
            ALL_COMMANDS.contains(&cmd),
            "{cmd:?} (shows {tab:?}) is missing from the palette"
        );
        app.show_terminal = false;
        app.focus_pane(Pane::Editor);
        app.bottom_panel_tab = if tab == BottomPanelTab::Terminal {
            BottomPanelTab::Problems
        } else {
            BottomPanelTab::Terminal
        };
        app.run_command(cmd);
        assert_eq!(app.bottom_panel_tab, tab, "{cmd:?}");
        assert!(app.show_terminal, "{cmd:?} must reveal the panel");
        assert!(
            app.focus == Pane::Terminal,
            "{cmd:?} must focus the panel so its keys work at once"
        );
    }
}

/// #852: VS Code's `Ctrl/Cmd+Shift+M` and `+U` show PROBLEMS and OUTPUT from
/// any pane, the live terminal included, and `Cmd+Shift+T` brings the
/// TERMINAL tab back rather than focusing whichever tab was last up.
#[test]
fn panel_tab_chords_switch_between_problems_output_and_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    let cmd_shift = KeyModifiers::SUPER | KeyModifiers::SHIFT;
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('M'), ctrl_shift)).unwrap();
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Problems);
    assert!(app.focus == Pane::Terminal);
    // From the PROBLEMS view itself.
    app.handle_key(key(KeyCode::Char('u'), cmd_shift)).unwrap();
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Output);
    app.handle_key(key(KeyCode::Char('T'), cmd_shift)).unwrap();
    assert_eq!(
        app.bottom_panel_tab,
        BottomPanelTab::Terminal,
        "Cmd+Shift+T must bring the TERMINAL tab forward"
    );
    assert!(app.focus == Pane::Terminal);
    // From the live shell: the chord is croft's, as the side bar jumps are.
    app.handle_key(key(KeyCode::Char('U'), ctrl_shift)).unwrap();
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Output);
    // Plain Ctrl+M / Ctrl+U stay the shell's (Enter, kill line).
    assert!(!is_show_problems_key(key(
        KeyCode::Char('m'),
        KeyModifiers::CONTROL
    )));
    assert!(!is_show_output_key(key(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL
    )));
    // Cmd+Opt+Shift+U is Transform to Uppercase, not OUTPUT.
    assert!(!is_show_output_key(key(
        KeyCode::Char('U'),
        cmd_shift | KeyModifiers::ALT
    )));
}

/// #852: Output: Select Channel… is the OUTPUT toolbar's click-only channel
/// dropdown as a keyboard picker; choosing a row shows that channel.
#[test]
fn output_select_channel_picks_a_channel_from_the_keyboard() {
    use crate::widgets::command_palette::Command;
    use crate::widgets::list_picker::ListPurpose;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // The bus is process-wide, so the channel carries a name no other test
    // uses and the picker is narrowed to it.
    let channel = "Keyboard Reach 852";
    crate::output::push(channel, crate::output::OutputLevel::Info, "hello");
    app.run_command(Command::OutputSelectChannel);
    let picker = app.list_picker.as_mut().expect("the channel picker opens");
    assert_eq!(picker.purpose, ListPurpose::OutputChannel);
    for c in "reach 852".chars() {
        picker.push_char(c);
    }
    assert_eq!(picker.visible_count(), 1);
    app.confirm_list_picker();
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Output);
    assert!(app.focus == Pane::Terminal);
    assert_eq!(app.output.selected_name().as_deref(), Some(channel));
}

/// #852 regression, driven by keys alone: from the editor with the panel
/// hidden, `Ctrl+Shift+M` shows PROBLEMS, `Ctrl+Shift+U` shows OUTPUT and
/// `Cmd+Shift+T` brings TERMINAL back over them, each time with the panel
/// shown and focused.
#[test]
fn panel_tab_chords_reach_problems_output_and_terminal_by_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    let cmd_shift = KeyModifiers::SUPER | KeyModifiers::SHIFT;
    app.show_terminal = false;
    app.bottom_panel_tab = BottomPanelTab::Terminal;
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('M'), ctrl_shift)).unwrap();
    assert_eq!(
        app.bottom_panel_tab,
        BottomPanelTab::Problems,
        "Ctrl+Shift+M must show PROBLEMS"
    );
    assert!(app.show_terminal, "Ctrl+Shift+M must reveal the panel");
    assert!(app.focus == Pane::Terminal);
    app.handle_key(key(KeyCode::Char('U'), ctrl_shift)).unwrap();
    assert_eq!(
        app.bottom_panel_tab,
        BottomPanelTab::Output,
        "Ctrl+Shift+U must show OUTPUT"
    );
    app.handle_key(key(KeyCode::Char('T'), cmd_shift)).unwrap();
    assert_eq!(
        app.bottom_panel_tab,
        BottomPanelTab::Terminal,
        "Cmd+Shift+T must bring TERMINAL forward"
    );
    assert!(app.focus == Pane::Terminal);
}

/// #852 (comment 2): the Customize Layout popup opens from the keyboard.
/// Typing its name in the palette and pressing Enter shows the popup with
/// its rows, where before the palette said "No commands match".
#[test]
fn typing_customize_layout_in_the_palette_opens_the_popup() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    open_palette_852(&mut app);
    type_keys_852(&mut app, "customize layout");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let menu = app
        .context_menu
        .as_ref()
        .expect("View: Customize Layout… opens the Customize Layout popup");
    assert!(
        menu.items.iter().any(|e| matches!(
            e,
            MenuEntry::Item {
                action: MenuAction::ToggleStatusBar,
                ..
            }
        )),
        "the popup is the Customize Layout one"
    );
}

/// #852 guard: no Customize Layout row ships mouse-only. Every row names
/// the palette command that reaches it (a new row fails here until it has
/// one), the command is in the palette and found by "layout", and running
/// it (choosing the row's option where it opens a picker) lands on the
/// state the row's click sets, from a state where that option is not yet
/// the active one.
#[test]
fn every_customize_layout_row_has_a_palette_command_that_does_the_same() {
    use crate::widgets::command_palette::{ALL_COMMANDS, Command, LAYOUT_COMMANDS};
    fn command_for(action: &MenuAction) -> Option<Command> {
        match action {
            MenuAction::ToggleActivityBar => Some(Command::ToggleActivityBar),
            MenuAction::ToggleSideBar => Some(Command::ToggleSideBar),
            MenuAction::ToggleSecondarySideBar => Some(Command::ToggleSecondarySideBar),
            MenuAction::TogglePanel => Some(Command::ToggleTerminal),
            MenuAction::ToggleStatusBar => Some(Command::ToggleStatusBar),
            MenuAction::ToggleMinimap => Some(Command::ToggleMinimap),
            MenuAction::ToggleAutoHideSideBar => Some(Command::ToggleAutoHideSideBar),
            MenuAction::SetSideBarPosition(_) => Some(Command::ToggleSideBarPosition),
            MenuAction::SetPanelAlignment(_) => Some(Command::SetPanelAlignment),
            MenuAction::SetQuickInputPosition(_) => Some(Command::SetQuickInputPosition),
            MenuAction::ToggleZenMode => Some(Command::ToggleZenMode),
            MenuAction::ToggleCenteredLayout => Some(Command::ToggleCenteredLayout),
            _ => None,
        }
    }
    // A radio row's setting moved off the row's option first.
    fn off_option(app: &mut App, action: &MenuAction) {
        match *action {
            MenuAction::SetSideBarPosition(p) => {
                app.side_bar_position = if p == SideBarPosition::Left {
                    SideBarPosition::Right
                } else {
                    SideBarPosition::Left
                };
            }
            MenuAction::SetPanelAlignment(al) => {
                app.panel_alignment = if al == PanelAlignment::Left {
                    PanelAlignment::Center
                } else {
                    PanelAlignment::Left
                };
            }
            MenuAction::SetQuickInputPosition(p) => {
                app.quick_input_position = if p == QuickInputPosition::Top {
                    QuickInputPosition::Center
                } else {
                    QuickInputPosition::Top
                };
            }
            _ => {}
        }
    }
    let snapshot = |app: &App| {
        (
            app.activity_bar_visible,
            app.show_tree,
            app.secondary_side_bar_visible,
            app.show_terminal,
            app.status_bar_visible,
            app.minimap_visible,
            app.sidebar_auto_hide,
            app.side_bar_position,
            app.panel_alignment,
            app.quick_input_position,
            app.zen_mode,
            app.centered_layout,
        )
    };
    let tmp = tempfile::tempdir().unwrap();
    let rows: Vec<(String, MenuAction)> = App::new(tmp.path().to_path_buf())
        .unwrap()
        .customize_layout_items()
        .into_iter()
        .filter_map(|e| match e {
            MenuEntry::Item { label, action } => Some((label, action)),
            _ => None,
        })
        .collect();
    assert!(rows.len() >= 12, "the popup's rows: {rows:?}");
    for (label, action) in rows {
        let cmd = command_for(&action)
            .unwrap_or_else(|| panic!("Customize Layout row {label:?} has no palette command"));
        assert!(ALL_COMMANDS.contains(&cmd), "{cmd:?} is not in the palette");
        assert!(
            LAYOUT_COMMANDS.contains(&cmd),
            "{cmd:?} is not found by the query \"layout\""
        );
        let mut by_click = App::new(tmp.path().to_path_buf()).unwrap();
        let mut by_palette = App::new(tmp.path().to_path_buf()).unwrap();
        off_option(&mut by_click, &action);
        off_option(&mut by_palette, &action);
        assert_eq!(snapshot(&by_click), snapshot(&by_palette), "fixture");
        by_click.dispatch_menu_action(action.clone(), tmp.path().to_path_buf());
        by_palette.run_command(cmd);
        if let Some(picker) = by_palette.list_picker.as_mut() {
            let option = label.trim_start_matches('\u{2713}').trim().to_lowercase();
            for c in option.chars() {
                picker.push_char(c);
            }
            assert_eq!(picker.visible_count(), 1, "{cmd:?} offers {option:?}");
            by_palette.confirm_list_picker();
        }
        assert_eq!(
            snapshot(&by_palette),
            snapshot(&by_click),
            "{cmd:?} must do what the row {label:?} does"
        );
    }
}

/// #852 negative: the alignment picker opens on the current alignment and
/// Esc leaves it unchanged; only a chosen row changes it.
#[test]
fn dismissing_the_panel_alignment_picker_keeps_the_alignment() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.panel_alignment = PanelAlignment::Right;
    app.run_command(Command::SetPanelAlignment);
    let picker = app.list_picker.as_ref().expect("the picker opens");
    assert_eq!(
        picker.selected_row().map(|r| r.id.as_str()),
        Some("right"),
        "the picker opens on the current alignment"
    );
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.list_picker.is_none());
    assert_eq!(app.panel_alignment, PanelAlignment::Right);
}
