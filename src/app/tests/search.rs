//! Workspace search, find and replace, and the editor's find overlay.

use super::*;

#[test]
fn opening_a_search_hit_moves_focus_to_the_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hit.txt");
    std::fs::write(&f, "needle here\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("needle");
    app.search.run_query();
    assert!(
        matches!(app.focus, Pane::Tree),
        "precondition: search holds focus"
    );
    let hit = app.search.selected_hit().cloned().expect("a hit");
    app.open_search_hit(&hit);
    assert!(
        matches!(app.focus, Pane::Editor),
        "opening a search result must hand keyboard focus to the editor so typing no longer leaks into the search box"
    );
    assert!(!app.search.focused);
}

#[test]
fn opening_a_search_hit_marks_the_active_match_so_the_renderer_paints_it_orange() {
    // Global search (Cmd+Shift+F) result navigation must mark the occurrence
    // it lands on as the *active* match, the same way the in-file Cmd+F path
    // does, so the editor paints that one occurrence orange while the sibling
    // matches stay yellow. Without active_search_match every match renders the
    // same uniform yellow and the user can't tell which result they are on.
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hit.rs");
    // Line 2 (index 1) has two "alpha" matches: "let " is 4 chars, so the
    // first "alpha" starts at char col 4, and the second is later on the line.
    std::fs::write(&f, "fn main() {}\nlet alpha = alpha + 1;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("alpha");
    app.search.run_query();
    let hit = app.search.selected_hit().cloned().expect("a hit");
    app.open_search_hit(&hit);
    assert_eq!(
        app.editor.active_search_match,
        Some((1, 4, 5)),
        "opening a global-search hit must record the first match on the line as the active match (row 1, col 4, len 5) so the renderer paints exactly those cells orange; the second 'alpha' stays yellow"
    );
}

#[test]
fn typing_after_reactivating_search_replaces_instead_of_appending() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("oldterm");
    app.search.clear_selection();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(
        KeyCode::Char('F'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.search.query, "x",
        "the previous query was selected, so the first keystroke replaces it"
    );
}

#[test]
fn ctrl_shift_f_jumps_to_search() {
    let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    assert!(is_search_jump_key(key(KeyCode::Char('F'), mods)));
}

#[test]
fn cmd_shift_f_jumps_to_search() {
    let mods = KeyModifiers::SUPER | KeyModifiers::SHIFT;
    assert!(is_search_jump_key(key(KeyCode::Char('F'), mods)));
}

#[test]
fn plain_f_does_not_jump_to_search() {
    assert!(!is_search_jump_key(key(
        KeyCode::Char('f'),
        KeyModifiers::NONE
    )));
}

#[test]
fn editor_find_in_side_by_side_diff_counts_matches_in_diff_content() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    std::fs::write(&f1, "alpha\nbravo\ncharlie\n").unwrap();
    std::fs::write(&f2, "alpha\nbravo\ncharlie\nlogger.info here\n").unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    app.focus = Pane::Editor;
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    app.open_editor_find();
    app.editor_find_set_query(String::from("logger"));

    let count = app.editor_find.as_ref().map(|s| s.match_count).unwrap_or(0);
    assert!(
        count >= 1,
        "Find must search the content rendered in a diff tab: 'logger' is visible in the right column yet match_count={count}"
    );
    let active = app
        .editor
        .diff
        .as_ref()
        .and_then(|d| d.find.active)
        .expect("a match in a diff must set an active find position for highlighting");
    assert_eq!(
        active.side,
        crate::widgets::diff::DiffSide::Right,
        "the 'logger' match lives on the added (right) side and must be selected as active"
    );
}

#[test]
fn editor_find_in_diff_next_prev_cycle_through_both_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f1 = tmp.path().join("a.txt");
    let f2 = tmp.path().join("b.txt");
    std::fs::write(&f1, "needle one\nplain\n").unwrap();
    std::fs::write(&f2, "needle one\nplain\nneedle two\n").unwrap();
    app.editor.open_diff(&f1, &f2).unwrap();
    app.focus = Pane::Editor;
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    app.open_editor_find();
    app.editor_find_set_query(String::from("needle"));
    let count = app.editor_find.as_ref().map(|s| s.match_count).unwrap_or(0);
    assert!(
        count >= 2,
        "an equal line counts on both columns plus the added line: expected >=2, got {count}"
    );
    let first = app.editor_find.as_ref().and_then(|s| s.match_index);
    assert_eq!(first, Some(1), "opening find selects the first match");
    app.editor_find_jump_next();
    let second = app.editor_find.as_ref().and_then(|s| s.match_index);
    assert_eq!(second, Some(2), "Enter advances to the second match");
    app.editor_find_jump_prev();
    let back = app.editor_find.as_ref().and_then(|s| s.match_index);
    assert_eq!(back, Some(1), "Shift+Enter walks back to the first match");
}

#[test]
fn changing_workspace_root_retargets_search_at_the_new_explorer_root() {
    let tmp = tempfile::tempdir().unwrap();
    let inner = tmp.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.search.hits.push(crate::widgets::search::SearchHit {
        path: tmp.path().join("stale.txt"),
        line_no: 1,
        line_text: String::from("stale"),
        matches: 1,
    });

    app.change_workspace_root(inner.clone());

    assert_eq!(
        app.search.root, inner,
        "the search panel must point at the Explorer's new root so its target and the path-stripping match"
    );
    assert!(
        app.search.hits.is_empty(),
        "hits from the previous root must be dropped when the Explorer root changes"
    );
}

#[test]
fn mouse_wheel_over_search_panel_scrolls_the_results_list() {
    // User report: search shows thousands of matches piling up but
    // the panel can't be scrolled with the wheel - the user is stuck
    // at the top. The result list must respond to ScrollUp/ScrollDown
    // when the cursor is over the side panel and Search is the
    // active view.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::Search;
    app.show_tree = true;
    // Pretend a render captured the search panel at this rect.
    app.search.last_area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    app.search.last_inner = Rect {
        x: 1,
        y: 1,
        width: 58,
        height: 28,
    };
    // 1000 hits, well past anything that can fit in 30 rows.
    app.search.hits = (0..1000)
        .map(|i| crate::widgets::search::SearchHit {
            path: tmp.path().join("a.txt"),
            line_no: i + 1,
            line_text: format!("line {i}"),
            matches: 1,
        })
        .collect();
    app.search.scroll = 0;
    app.search.selected = 0;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollDown,
        column: 30,
        row: 15,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.search.scroll > 0,
        "wheel-down on the search panel must advance the result list scroll"
    );
    let after_down = app.search.scroll;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollUp,
        column: 30,
        row: 15,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.search.scroll < after_down,
        "wheel-up must reduce the scroll back toward the top"
    );
}

/// A background history refresh must not replace a lane diff (#348).
///
/// The reply tag carries the revspec as well as the root, because both
/// views are for the SAME root and the root alone cannot tell them apart.
/// Pinned against a positive value: the panel starts holding the lane
/// diff's own row, so a dropped reply and an accepted one differ in what
/// is on screen, not merely in timing.
#[test]
fn a_history_reply_does_not_replace_the_lane_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while app.graph_fetch_inflight {
        app.sync_explorer_panels();
        assert!(
            std::time::Instant::now() < deadline,
            "startup graph fetch must settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let row = |hash: &str, summary: &str| crate::git::GraphCommit {
        hash: String::from(hash),
        short_hash: hash[..7.min(hash.len())].to_string(),
        parents: Vec::new(),
        refs: Vec::new(),
        summary: String::from(summary),
        author: String::from("t"),
        age_secs: 0,
    };
    app.sidebar_view = SidebarView::SourceControl;
    app.show_tree = true;
    app.source_control.status.in_repo = true;

    // The lane diff is up and showing its own commit.
    app.graph_revspec = Some(String::from("main..lane"));
    app.commit_graph
        .set_rows(crate::widgets::commit_graph::layout_graph(vec![row(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "the lane commit",
        )]));

    // `commit_at` is a click hit-test and needs rendered geometry, so read
    // the top row the way a click would - after a draw.
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    let shown = |app: &mut App, term: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
        term.draw(|frame| app.render(frame)).unwrap();
        let area = app.commit_graph.last_area;
        app.commit_graph
            .commit_at(area.y + 2)
            .map(|c| c.hash.clone())
    };
    let lane_hash = shown(&mut app, &mut term);
    assert_eq!(
        lane_hash,
        Some(String::from("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")),
        "the lane diff must be on screen first"
    );

    // A plain history reply for the SAME root arrives (revspec None) carrying
    // a DIFFERENT commit. Same root, different view: the tag must drop it.
    app.graph_tx
        .send((
            app.active_scm_root.clone(),
            None,
            crate::widgets::commit_graph::layout_graph(vec![row(
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "a background history refresh",
            )]),
        ))
        .unwrap();
    app.sync_explorer_panels();
    assert_eq!(
        shown(&mut app, &mut term),
        lane_hash,
        "a history reply must not replace the lane diff the user asked for"
    );

    // The matching reply IS taken, so the drop above is the tag working
    // rather than the channel being dead.
    app.graph_tx
        .send((
            app.active_scm_root.clone(),
            Some(String::from("main..lane")),
            crate::widgets::commit_graph::layout_graph(vec![row(
                "cccccccccccccccccccccccccccccccccccccccc",
                "the refreshed lane diff",
            )]),
        ))
        .unwrap();
    app.sync_explorer_panels();
    assert_eq!(
        shown(&mut app, &mut term),
        Some(String::from("cccccccccccccccccccccccccccccccccccccccc")),
        "a reply for the view that IS up must land"
    );
}

#[test]
fn search_panel_paints_a_scrollbar_when_hits_exceed_visible_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::Search;
    app.show_tree = true;
    app.search.hits = (0..500)
        .map(|i| crate::widgets::search::SearchHit {
            path: tmp.path().join("a.txt"),
            line_no: i + 1,
            line_text: format!("line {i}"),
            matches: 1,
        })
        .collect();
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut app.search, area, &mut buf);
    assert!(
        app.search.last_scrollbar.width > 0 && app.search.last_scrollbar.height > 0,
        "scrollbar rect must be tracked when results overflow the viewport"
    );
}

#[test]
fn bracketed_paste_into_search_input_appends_to_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("foo");
    app.handle_paste("bar");
    assert_eq!(app.search.query, "foobar");
}

#[test]
fn bracketed_paste_into_search_strips_newlines() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.handle_paste("hello\nworld\r\n");
    assert_eq!(app.search.query, "helloworld");
}

#[test]
fn search_view_focuses_search_input_not_terminal_or_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);
    app.handle_key(key(
        KeyCode::Char('F'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(app.focus == Pane::Tree);
    assert!(app.search.focused);
    assert!(!app.tree.focused);
    assert!(!app.terminal().focused);
}

#[test]
fn cycling_focus_away_from_search_clears_search_focus_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.cycle_focus();
    assert!(!app.search.focused);
}

#[test]
fn search_editing_shortcuts_route_to_search_only_when_the_search_input_is_focused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    assert!(
        app.focus == Pane::Tree,
        "Search view focuses the side panel"
    );
    app.search.query = String::from("alpha");
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.search.selection_range(), Some((0, 5)));
}

#[test]
fn search_visible_does_not_steal_editing_shortcuts_from_a_focused_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);
    app.search.query = String::from("alpha");
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.search.selection_range(),
        None,
        "Cmd+A must act on the focused terminal, not the search query"
    );
}

#[test]
fn search_visible_routes_cmd_v_to_search_only_when_search_input_focused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.clipboard_reader = || Some(String::from("needle"));
    app.set_sidebar_view(SidebarView::Search);

    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
        .unwrap();

    assert_eq!(app.search.query, "needle");
}

#[test]
fn search_visible_routes_cmd_v_to_terminal_not_search_when_terminal_focused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.clipboard_reader = || Some(String::from("needle"));
    app.set_sidebar_view(SidebarView::Search);
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);

    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
        .unwrap();

    assert_eq!(
        app.search.query, "",
        "Cmd+V must paste into the focused terminal, not leak into the search query"
    );
}

#[test]
fn cmd_f_in_editor_opens_the_inline_find_overlay_not_the_new_file_prompt() {
    let mut app = editor_app_with_lines(&["alpha beta", "alpha gamma"]);
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.editor_find.is_some(),
        "Cmd+F with the editor focused must open the VS Code-style inline Find bar over the editor pane, not the Explorer 'new file' prompt"
    );
    assert!(
        app.prompt.is_none(),
        "Cmd+F in editor must NOT open the new-file Prompt — that is reserved for the Explorer pane"
    );
}

#[test]
fn cmd_shift_f_global_search_closes_the_local_editor_find_bar() {
    // With the in-file Find bar (Cmd+F) open, jumping to global Search
    // (Cmd+Shift+F) must dismiss the local bar. render_editor_find paints
    // whenever editor_find is Some regardless of focus, so a lingering bar
    // shadows the global search input on the left pane.
    let mut app = editor_app_with_lines(&["alpha beta", "alpha gamma"]);
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.editor_find.is_some(),
        "precondition: Cmd+F opens the inline editor Find bar"
    );
    app.handle_key(key(
        KeyCode::Char('f'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.sidebar_view,
        SidebarView::Search,
        "Cmd+Shift+F must switch the sidebar to global Search"
    );
    assert!(
        app.editor_find.is_none(),
        "Cmd+Shift+F must close the local editor Find bar so it cannot shadow the global search input"
    );
}

#[test]
fn typing_in_editor_find_jumps_the_cursor_to_the_next_match() {
    let mut app = editor_app_with_lines(&["alpha beta", "alpha gamma"]);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('t'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        (app.editor.cursor_row, app.editor.cursor_col),
        (0, 6),
        "typing 'beta' must jump the cursor to the first 'beta' occurrence — got ({}, {})",
        app.editor.cursor_row,
        app.editor.cursor_col
    );
}

#[test]
fn enter_in_editor_find_jumps_to_the_next_match_and_shift_enter_walks_back() {
    let mut app = editor_app_with_lines(&["alpha", "beta alpha", "alpha"]);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('l'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('h'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.cursor_row, 1,
        "Enter must jump to the next 'alpha' match on row 1"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.cursor_row, 2,
        "Enter again must walk forward to the row 2 match"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.editor.cursor_row, 1,
        "Shift+Enter must walk back one match to row 1"
    );
}

#[test]
fn jumping_to_a_match_records_an_active_search_match_so_the_renderer_can_paint_it_orange() {
    let mut app = editor_app_with_lines(&["alpha beta alpha"]);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('l'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('h'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    let first = app.editor.active_search_match;
    assert_eq!(
        first,
        Some((0, 0, 5)),
        "typing 'alpha' lands the cursor on the first match (col 0, len 5); active_search_match must record exactly that so the renderer can paint just those 5 cells orange while the other 'alpha' at col 11 keeps the regular yellow — got {first:?}"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let second = app.editor.active_search_match;
    assert_eq!(
        second,
        Some((0, 11, 5)),
        "Enter walks to the next match at col 11; the active marker must follow so the previously-orange match resets to yellow and the new cursor row gets the orange band"
    );
}

#[test]
fn esc_closes_the_editor_find_overlay_and_clears_the_highlight() {
    let mut app = editor_app_with_lines(&["alpha"]);
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.search_highlight.is_some());
    assert!(
        app.editor.active_search_match.is_some(),
        "typing should land the cursor on a match and arm the active-match marker"
    );
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor_find.is_none(), "Esc must close the find bar");
    assert!(
        app.editor.search_highlight.is_none(),
        "Esc must clear the editor's match highlight so leftover dim cells don't survive into normal typing"
    );
    assert!(
        app.editor.active_search_match.is_none(),
        "Esc must also clear the active-match marker so the orange band does not survive the overlay closing"
    );
}

#[test]
fn editor_find_pre_fills_the_query_from_word_under_cursor_on_open() {
    let mut app = editor_app_with_lines(&["alphabet"]);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.editor_find.as_ref().unwrap().query,
        "alphabet",
        "opening Cmd+F with the cursor mid-word pre-fills the whole word under it, as VS Code does (#1278)"
    );
    assert!(
        app.editor_find.as_ref().unwrap().query_selected,
        "and selects it, so typing replaces it"
    );
}

#[test]
fn editor_find_paste_appends_clipboard_to_the_query() {
    let mut app = editor_app_with_lines(&["alpha needle beta"]);
    app.clipboard_reader = || Some(String::from("needle"));
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.editor_find.as_ref().unwrap().query,
        "needle",
        "Cmd+V inside the find bar must paste from the system clipboard"
    );
}

#[test]
fn search_visible_does_not_steal_cmd_x_from_focused_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.focus_pane(Pane::Editor);
    app.search.query = String::from("needle");
    app.editor.lines = vec![String::from("alpha")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 5;
    app.editor.select_all();

    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::SUPER))
        .unwrap();

    assert_eq!(app.editor.lines, [String::new()]);
    assert_eq!(app.search.query, "needle");
}

#[test]
fn search_visible_does_not_steal_bracketed_paste_from_focused_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.focus_pane(Pane::Editor);
    app.search.query = String::from("needle");
    app.editor.lines = vec![String::from("alpha")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 5;

    app.handle_paste(" beta");

    assert_eq!(app.editor.lines, [String::from("alpha beta")]);
    assert_eq!(app.search.query, "needle");
}

#[test]
fn cmd_v_pastes_clipboard_into_focused_editor_even_when_search_sidebar_visible() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.clipboard_reader = || Some(String::from(" beta"));
    app.set_sidebar_view(SidebarView::Search);
    app.focus_pane(Pane::Editor);
    app.search.query = String::from("needle");
    app.editor.lines = vec![String::from("alpha")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 5;

    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
        .unwrap();

    assert_eq!(app.editor.lines, [String::from("alpha beta")]);
    assert_eq!(app.search.query, "needle");
}

#[test]
fn cmd_a_in_search_selects_entire_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("alpha");
    app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
    assert_eq!(
        app.search.selection_range(),
        Some((0, "alpha".len())),
        "Cmd+A in search input must select the whole query"
    );
}

#[test]
fn cmd_c_in_search_with_full_selection_clears_no_text() {
    // After Cmd+A then Cmd+C, the query stays put (copy is non-destructive).
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("alpha");
    app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
    app.handle_search_key(key(KeyCode::Char('c'), KeyModifiers::SUPER));
    assert_eq!(app.search.query, "alpha");
}

#[test]
fn cmd_x_in_search_with_full_selection_clears_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("alpha");
    app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
    app.handle_search_key(key(KeyCode::Char('x'), KeyModifiers::SUPER));
    assert_eq!(app.search.query, "");
    assert_eq!(app.search.selection_range(), None);
}

#[test]
fn paste_clipboard_into_search_with_injected_text_appends_to_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("foo");
    app.paste_clipboard_into_search(Some("BAR"));
    assert_eq!(app.search.query, "fooBAR");
}

#[test]
fn paste_clipboard_into_search_with_no_text_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("foo");
    app.paste_clipboard_into_search(None);
    assert_eq!(app.search.query, "foo");
}

#[test]
fn paste_clipboard_into_search_with_no_text_sets_diagnostic_status() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.paste_clipboard_into_search(None);
    assert!(
        app.status.to_lowercase().contains("clipboard"),
        "expected diagnostic status mentioning the clipboard, got: {:?}",
        app.status
    );
}

#[test]
fn paste_clipboard_into_search_with_empty_text_sets_diagnostic_status() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.paste_clipboard_into_search(Some(""));
    assert!(
        app.status.to_lowercase().contains("clipboard"),
        "expected diagnostic status mentioning the clipboard, got: {:?}",
        app.status
    );
}

#[test]
fn unmatched_cmd_key_in_search_logs_diagnostic_status() {
    // If iTerm or the kitty CSI u path delivers a Cmd+<letter> the search
    // handler doesn't recognise, leave a breadcrumb so the user can tell
    // us exactly what crossterm saw — this is how we'll diagnose Cmd+V
    // delivery problems without an interactive debugger.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.handle_search_key(key(KeyCode::Char('q'), KeyModifiers::SUPER));
    assert!(
        app.status.contains("Cmd+"),
        "expected unhandled-cmd diagnostic, got: {:?}",
        app.status
    );
}

#[test]
fn cmd_v_is_recognised_as_search_paste_key() {
    assert!(is_search_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::SUPER
    )));
    assert!(is_search_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::CONTROL
    )));
    assert!(is_search_paste_key(key(
        KeyCode::Char('\u{16}'),
        KeyModifiers::NONE
    )));
    assert!(!is_search_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::NONE
    )));
    assert!(!is_search_paste_key(key(
        KeyCode::Char('a'),
        KeyModifiers::SUPER
    )));
}

#[test]
fn left_click_on_paste_button_triggers_clipboard_paste_into_search() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.tree.last_area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    app.search.last_area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    app.search.last_inner = Rect {
        x: 1,
        y: 1,
        width: 58,
        height: 10,
    };
    app.search.paste_button_x = 40;
    app.search.paste_button_y = 1;
    app.search.paste_button_w = 5;
    let original_status = app.status.clone();
    let m = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 42,
        row: 1,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(m);
    assert_ne!(
        app.status, original_status,
        "click on Paste button must invoke the clipboard paste path (status updates whether clipboard read succeeds, fails, or returns empty)"
    );
}

#[test]
fn double_clicking_a_search_hit_pins_the_tab_so_the_next_hit_opens_beside_it() {
    // Search results must mirror the Explorer's preview/pin gesture: a
    // single click opens the hit in the replaceable preview slot, a
    // double-click pins it so navigating to the next result opens a NEW
    // preview tab beside it instead of replacing the file under review.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "needle a\n").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "needle b\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("needle");
    app.search.run_query();
    assert_eq!(app.search.hits.len(), 2, "precondition: one hit per file");
    app.search.last_area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    app.search.last_inner = Rect {
        x: 1,
        y: 1,
        width: 58,
        height: 18,
    };
    // Result rows start 5 rows below the inner top (header + blank + single-row
    // query field + separator + caption), matching SearchPanel::hit_at_y.
    let first_hit_row = app.search.last_inner.y + 5;
    let click = |row: u16| crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 5,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let first_path = app.search.hits[0].path.clone();
    let second_path = app.search.hits[1].path.clone();

    app.handle_mouse(click(first_hit_row));
    assert!(
        app.editor.editors[app.editor.active_index()].preview,
        "a single click on a search hit opens it as a preview tab"
    );
    app.handle_mouse(click(first_hit_row));
    assert!(
        !app.editor.editors[app.editor.active_index()].preview,
        "a second click on the same hit within the double-click window must pin the tab, exactly like double-clicking a file in the Explorer"
    );

    app.handle_mouse(click(first_hit_row + 1));
    let paths: Vec<_> = app
        .editor
        .editors
        .iter()
        .filter_map(|e| e.path.clone())
        .collect();
    assert!(
        paths.contains(&first_path) && paths.contains(&second_path),
        "after pinning the first hit, opening the second must keep both files open (got {paths:?}); the pinned tab must not be replaced by the next preview"
    );
    assert!(
        app.editor.editors[app.editor.active_index()].preview,
        "the second hit lands in a fresh preview tab beside the pinned one"
    );
}

// --- Split editor (side-by-side panes) -----------------------------------

/// Seeding the Search panel from a terminal command must describe THAT
/// command: filter globs left over from an earlier manual search would
/// silently narrow the seeded results (the terminal scanned everything), and
/// the status line would still claim a faithful seed — feeding Replace All a
/// wrong subset.
#[test]
fn seeding_search_replaces_stale_include_and_exclude_filters() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.search.include = String::from("*.md");
    app.search.exclude = String::from("vendor");
    assert!(seed_at_root(&mut app, "rg TODO"));
    assert_eq!(
        app.search.include, "",
        "a bare rg scanned everything; a stale include must not filter the seeded search"
    );
    assert_eq!(app.search.exclude, "");
    assert!(seed_at_root(&mut app, "rg -g '*.rs' -g '!target' TODO"));
    assert_eq!(app.search.include, "*.rs");
    assert_eq!(app.search.exclude, "target");
}

/// Seed Search as if the terminal ran `input` at the workspace root.
fn seed_at_root(app: &mut App, input: &str) -> bool {
    let root = app.workspace_root().to_path_buf();
    app.seed_search_from_command_in(input, &root)
}

/// Negative: with no terminal whose directory croft can read, a search is
/// refused rather than guessed to have run at the workspace root, which
/// would widen `rg color` run from src/ to every file (#1201).
#[test]
fn a_search_with_no_known_terminal_directory_is_not_seeded() {
    let (_tmp, mut app) = grep_scope_app();
    // No pane to read a directory from: the app's own shell would answer
    // with the workspace root.
    app.active_terminal = app.terminals.len();
    app.search.query = String::from("untouched");
    assert!(app.seed_search_from_command("rg color"), "still a search");
    assert_eq!(app.search.query, "untouched");
    assert!(app.status.contains("Search not seeded"), "{}", app.status);
    assert!(!app.seed_search_from_command("ls -la"), "not a search");
}

/// The workspace of #1201: one `color` in src/, one in docs/ and one in a
/// vendored file that must never be rewritten.
fn grep_scope_app() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    for (rel, body) in [
        ("src/style.py", "color = \"red\"\n"),
        ("docs/guide.md", "Set the color in style.py.\n"),
        (
            "vendor/lib.py",
            "color = \"keep\"  # third-party, do not touch\n",
        ),
    ] {
        let f = tmp.path().join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, body).unwrap();
    }
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    (tmp, app)
}

/// The files of [`grep_scope_app`] the seeded Search (and so Replace All)
/// would cover.
fn seeded_scope(app: &App, root: &std::path::Path) -> Vec<&'static str> {
    let filter = crate::widgets::search::PathFilter::new(&app.search.include, &app.search.exclude);
    ["docs/guide.md", "src/style.py", "vendor/lib.py"]
        .into_iter()
        .filter(|rel| filter.allows(root, &root.join(rel)))
        .collect()
}

/// `rg -n color src/` searched one directory; the seeded Search must too,
/// or Replace All rewrites docs/ and the vendored file (#1201).
#[test]
fn seeded_search_covers_only_the_paths_the_grep_searched() {
    let (tmp, mut app) = grep_scope_app();
    let root = tmp.path();
    assert!(seed_at_root(&mut app, "rg -n color src/"));
    assert_eq!(app.search.query, "color");
    assert_eq!(
        seeded_scope(&app, root),
        ["src/style.py"],
        "include: {:?}",
        app.search.include
    );
    assert!(seed_at_root(&mut app, "grep -rn color src/style.py docs"));
    assert_eq!(seeded_scope(&app, root), ["docs/guide.md", "src/style.py"]);
}

/// `rg -t py` searched Python files only, so the markdown guide stays out.
#[test]
fn seeded_search_keeps_the_rg_type_filter() {
    let (tmp, mut app) = grep_scope_app();
    let root = tmp.path();
    assert!(seed_at_root(&mut app, "rg -t py color"));
    assert_eq!(
        seeded_scope(&app, root),
        ["src/style.py", "vendor/lib.py"],
        "include: {:?}",
        app.search.include
    );
    assert!(seed_at_root(&mut app, "rg --type-not py color"));
    assert_eq!(seeded_scope(&app, root), ["docs/guide.md"]);
    assert!(seed_at_root(&mut app, "rg -t py color src"));
    assert_eq!(seeded_scope(&app, root), ["src/style.py"]);
}

/// After `cd src`, a bare `rg color` searched src/ only.
#[test]
fn seeded_search_runs_from_the_panes_directory() {
    let (tmp, mut app) = grep_scope_app();
    let root = tmp.path();
    assert!(app.seed_search_from_command_in("rg color", &root.join("src")));
    assert_eq!(seeded_scope(&app, root), ["src/style.py"]);
    assert!(app.seed_search_from_command_in("rg color ../vendor", &root.join("src")));
    assert_eq!(seeded_scope(&app, root), ["vendor/lib.py"]);
}

/// A scope croft can't reproduce is refused with the reason, leaving the
/// panel untouched, instead of seeding a wider search: an inverted match
/// (`-v` lists the lines that don't match), an unknown rg type, a path
/// outside the workspace or one that doesn't exist.
#[test]
fn a_grep_whose_scope_cant_be_reproduced_refuses_to_seed() {
    let (tmp, mut app) = grep_scope_app();
    let root = tmp.path();
    for cmd in [
        "rg -v color src/",
        "grep -rL color .",
        "rg -t nosuchtype color",
        "rg color ../",
        "rg color missing/",
    ] {
        app.status.clear();
        assert!(
            app.seed_search_from_command_in(cmd, root),
            "{cmd} is a search"
        );
        assert!(
            app.status.starts_with("Search not seeded: "),
            "{cmd}: status {:?}",
            app.status
        );
        assert_eq!(app.search.query, "", "{cmd} must not seed the panel");
    }
}

/// Negative: a search that covered the whole workspace still seeds the
/// whole workspace, and an rg `-L` (follow symlinks) is not an inversion.
#[test]
fn a_grep_over_the_whole_workspace_still_seeds_every_file() {
    let (tmp, mut app) = grep_scope_app();
    let root = tmp.path();
    for cmd in [
        "rg color",
        "grep -rn color .",
        "rg -L color",
        "git grep color -- .",
    ] {
        assert!(app.seed_search_from_command_in(cmd, root));
        assert_eq!(app.search.query, "color", "{cmd}");
        assert_eq!(
            seeded_scope(&app, root),
            ["docs/guide.md", "src/style.py", "vendor/lib.py"],
            "{cmd}: include {:?}",
            app.search.include
        );
    }
    assert!(app.seed_search_from_command_in("rg -g '*.md' color", root));
    assert_eq!(app.search.include, "*.md");
}

/// A byte-range selection made in the Include field before the seed must not
/// survive into the (shorter) seeded text: the next keystroke used to
/// replace_range the stale span and panic out of bounds.
#[test]
fn seeding_search_cannot_leave_a_stale_field_selection() {
    use crate::widgets::search::SearchField;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.search.include = String::from("**/*.rs,**/*.toml");
    app.search.focus_field(SearchField::Include);
    app.search.select_all_active();
    assert!(seed_at_root(&mut app, "rg -g '*.md' TODO"));
    assert_eq!(
        app.search.field,
        SearchField::Query,
        "seeding hands focus to the query field it just filled"
    );
    app.search.type_char('x');
    assert_eq!(app.search.include, "*.md", "the seeded include is intact");
}

/// #1345: `.vscode/settings.json`'s `files.exclude` and `search.exclude`
/// keep their folders out of Search, so the one real hit is all there is.
#[test]
fn search_honours_the_vscode_exclude_settings() {
    let (mut app, t) = app_with_vscode_excludes();
    assert_eq!(
        search_hit_paths(&mut app, t.path(), "parse_order"),
        vec![String::from("src/orders.py")]
    );
}

/// #1345: the exclude box adds to the project's exclusions rather than
/// replacing them.
#[test]
fn the_exclude_box_adds_to_the_project_excludes() {
    let (mut app, t) = app_with_vscode_excludes();
    app.search.exclude = String::from("src");
    assert!(
        search_hit_paths(&mut app, t.path(), "parse_order").is_empty(),
        "src is excluded by the box and the rest by the project"
    );
}

/// #1345 negative: turning the project exclusions off (VS Code's "Use
/// Exclude Settings and Ignore Files") searches everything again.
#[test]
fn project_excludes_can_be_switched_off_for_one_search() {
    let (mut app, t) = app_with_vscode_excludes();
    app.toggle_project_excludes();
    assert!(!app.search.use_exclude_settings);
    assert_eq!(search_hit_paths(&mut app, t.path(), "parse_order").len(), 8);
}

/// #1558: a hand-made selection matches as plain text, so `oo` is found in
/// every `foo` and in `food`, where whole-word matching finds none.
#[test]
fn cmd_shift_l_matches_a_hand_made_selection_as_text() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = occurrences_app(tmp.path());
    select(&mut app, (0, 1), (0, 3));
    cmd_shift_l(&mut app);
    assert_eq!(app.editor.carets.len() + 1, 4);
    let primary = app.editor.selection.unwrap();
    assert_eq!(
        primary.normalised(),
        ((0, 1), (0, 3)),
        "the user's stays primary"
    );
    type_char(&mut app, 'Q');
    assert_eq!(
        app.editor.lines,
        ["fQ = 1", "bar = fQ", "fQ += 2", "fQd = 3"]
    );
}

#[test]
fn replace_chord_opens_the_find_bar_with_the_replace_row() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "alpha beta");
    app.handle_key(replace_chord()).unwrap();
    let s = app
        .editor_find
        .as_ref()
        .expect("Cmd+Opt+F must open the find bar");
    assert!(s.replace_visible, "the replace row must be shown");
}

#[test]
fn replace_chord_toggles_the_replace_row_on_an_open_find_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "alpha beta");
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        !app.editor_find.as_ref().unwrap().replace_visible,
        "plain Cmd+F opens find without the replace row"
    );
    app.handle_key(replace_chord()).unwrap();
    assert!(
        app.editor_find.as_ref().unwrap().replace_visible,
        "Cmd+Opt+F on an open find bar expands the replace row"
    );
    app.handle_key(replace_chord()).unwrap();
    assert!(
        !app.editor_find.as_ref().unwrap().replace_visible,
        "Cmd+Opt+F again collapses the replace row"
    );
}

#[test]
fn enter_in_the_replace_field_replaces_the_current_match_and_advances() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "- alpha beta\ngamma alpha");
    // The caret opens off any word, so nothing is seeded (#1278) and the
    // query is typed.
    app.handle_key(replace_chord()).unwrap();
    for c in "alpha".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.lines[0], "- x beta",
        "the current match is replaced"
    );
    assert_eq!(
        app.editor.lines[1], "gamma alpha",
        "only the current match is replaced"
    );
    assert_eq!(
        app.editor.cursor_row, 1,
        "the editor advances to the next match"
    );
    assert!(app.editor.dirty, "a replace dirties the buffer");
    assert!(app.editor.undo(), "the replace is undoable");
    assert_eq!(app.editor.lines[0], "- alpha beta");
}

#[test]
fn replace_all_chord_replaces_every_match_in_one_undo_step() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "- alpha beta\ngamma alpha");
    // The caret opens off any word, so nothing is seeded (#1278) and the
    // query is typed.
    app.handle_key(replace_chord()).unwrap();
    for c in "alpha".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::ALT | KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines[0], "- x beta");
    assert_eq!(app.editor.lines[1], "gamma x");
    assert!(app.editor.dirty, "replace all dirties the buffer");
    assert!(app.editor.undo(), "replace all is one undo step");
    assert_eq!(app.editor.lines[0], "- alpha beta");
    assert_eq!(app.editor.lines[1], "gamma alpha");
}

/// #1577: with Find in Selection on, the count and Replace All cover the
/// selected function only. Replace All used to rewrite all three.
#[test]
fn find_in_selection_limits_the_count_and_replace_all() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = find_in_selection_app(tmp.path());
    assert_eq!(find_count(&app), 6, "the whole file before the toggle");
    alt_l(&mut app);
    assert_eq!(find_count(&app), 2);
    assert_eq!(app.editor.find_range, Some(((3, 0), (5, 0))));
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::ALT | KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.editor.lines,
        [
            "def a(x):",
            "    return x + 1",
            "",
            "def b(n):",
            "    return n * 2",
            "",
            "def c(x):",
            "    return x - 3",
        ]
    );
    assert_eq!(app.status, "Replaced 2 occurrences");
}

/// #1577: Enter steps through the selection's matches only, wrapping
/// inside it, and the bar says which of its two it is on.
#[test]
fn find_in_selection_steps_only_through_the_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = find_in_selection_app(tmp.path());
    alt_l(&mut app);
    let mut seen = Vec::new();
    for _ in 0..4 {
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        seen.push(app.editor.active_search_match);
        let index = app.editor_find.as_ref().unwrap().match_index;
        assert!(matches!(index, Some(1 | 2)), "{index:?}");
    }
    assert_eq!(
        seen,
        [
            Some((4, 11, 1)),
            Some((3, 6, 1)),
            Some((4, 11, 1)),
            Some((3, 6, 1)),
        ]
    );
}

/// #1577: with no selection, Find in Selection takes the caret's line; the
/// palette has the command, and it opens the bar.
#[test]
fn find_in_selection_without_a_selection_takes_the_caret_line() {
    use crate::widgets::command_palette::Command;
    assert_eq!(
        Command::from_id("toggle_find_in_selection"),
        Some(Command::ToggleFindInSelection)
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "f.py", FIND_IN_SELECTION_SRC);
    (app.editor.cursor_row, app.editor.cursor_col) = (7, 0);
    assert!(run_from_palette(&mut app, "Find: Toggle Find in Selection"));
    let state = app.editor_find.as_ref().expect("the bar opened");
    assert_eq!(state.in_selection, Some(((7, 0), (7, 16))));
    for c in "x".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(find_count(&app), 1);
}

/// Negatives (#1577): toggling off counts the whole file again, closing
/// the bar drops the range, and the next bar starts without it.
#[test]
fn find_in_selection_off_and_closed_is_the_whole_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = find_in_selection_app(tmp.path());
    alt_l(&mut app);
    assert_eq!(find_count(&app), 2);
    alt_l(&mut app);
    assert_eq!(find_count(&app), 6);
    assert_eq!(app.editor.find_range, None);
    alt_l(&mut app);
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor_find.is_none());
    assert_eq!(app.editor.find_range, None);
    app.handle_key(replace_chord()).unwrap();
    assert_eq!(app.editor_find.as_ref().unwrap().in_selection, None);
    // A typed `l` with no modifier is text, not the toggle.
    app.handle_key(key(KeyCode::Char('l'), KeyModifiers::NONE))
        .unwrap();
    let state = app.editor_find.as_ref().unwrap();
    assert!(state.query.ends_with('l') || state.replace.ends_with('l'));
    assert_eq!(state.in_selection, None);
}

/// #1577: the range belongs to the tab it was taken in. Another tab under
/// the same bar searches and replaces whole instead of by the first tab's
/// coordinates, and closing the bar from it still drops the first tab's
/// range.
#[test]
fn find_in_selection_stays_with_its_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = find_in_selection_app(tmp.path());
    alt_l(&mut app);
    assert_eq!(find_count(&app), 2);
    let g = tmp.path().join("g.py");
    std::fs::write(&g, FIND_IN_SELECTION_SRC).unwrap();
    app.editor.open_pinned(&g).unwrap();
    assert_eq!(app.editor.find_range, None);
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::ALT | KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.status, "Replaced 6 occurrences");
    assert!(app.editor.lines.iter().all(|l| !l.contains('x')));
    assert_eq!(app.editor.editors[0].find_range, Some(((3, 0), (5, 0))));
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor_find.is_none());
    assert_eq!(app.editor.editors[0].find_range, None);
}

/// #1577: `Alt+L` on a query seeded from a one-line selection keeps the
/// query, rather than clearing it as a typed character would.
#[test]
fn alt_l_keeps_a_seeded_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "f.py", FIND_IN_SELECTION_SRC);
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (1, 4),
        head: (1, 10),
    });
    (app.editor.cursor_row, app.editor.cursor_col) = (1, 10);
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.editor_find.as_ref().unwrap().query_selected);
    alt_l(&mut app);
    let state = app.editor_find.as_ref().unwrap();
    assert_eq!(state.query, "return");
    assert_eq!(state.in_selection, Some(((1, 4), (1, 10))));
    assert_eq!(find_count(&app), 1);
}

#[test]
fn replace_all_recounts_instead_of_reporting_zero() {
    // Replacing `a` with `aa` doubles the matches; the title must recount,
    // not read "No results" over a body still full of highlights.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "a");
    app.open_editor_replace();
    if let Some(s) = app.editor_find.as_mut() {
        s.query = "a".to_string();
        s.replace = "aa".to_string();
    }
    app.editor_find_replace_all();
    assert_eq!(
        app.editor_find.as_ref().unwrap().match_count,
        2,
        "the count reflects the buffer after the replace"
    );
}

#[test]
fn replace_all_requires_confirmation_and_esc_cancels() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "beta one\nbeta two\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.search.root = tmp.path().to_path_buf();
    app.search.query = String::from("beta");
    app.search.replace = String::from("delta");
    app.search.run_query();
    assert_eq!(app.search.hits.len(), 2, "precondition: two hits");

    // A still-streaming search refuses: hits arriving after confirmation
    // would be silently missed.
    app.search.complete = false;
    app.run_search_replace_all();
    assert!(app.pending_replace_all.is_none());
    assert!(app.status.contains("still running"));

    app.search.complete = true;
    app.run_search_replace_all();
    assert_eq!(
        app.pending_replace_all,
        Some((2, 1, 0)),
        "Enter must raise the modal with counts, not write"
    );
    let disk = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert!(
        disk.contains("beta"),
        "nothing is written before confirmation"
    );

    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    ))
    .unwrap();
    assert!(app.pending_replace_all.is_none());
    assert!(app.status.contains("cancelled"));
    let disk = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert!(disk.contains("beta"), "Esc leaves the files alone");

    app.run_search_replace_all();
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ))
    .unwrap();
    let disk = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert_eq!(disk, "delta one\ndelta two\n", "Enter commits the rewrite");
    assert!(app.status.contains("replaced 2 occurrence"));
}

#[test]
fn replace_all_skips_files_open_with_unsaved_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let clean = tmp.path().join("clean.txt");
    let dirty = tmp.path().join("dirty.txt");
    std::fs::write(&clean, "beta here\n").unwrap();
    std::fs::write(&dirty, "beta there\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&dirty).unwrap();
    app.editor.insert_char('x'); // unsaved edit
    assert!(app.editor.dirty);

    app.search.root = tmp.path().to_path_buf();
    app.search.query = String::from("beta");
    app.search.replace = String::from("delta");
    app.search.run_query();
    app.search.complete = true;
    app.run_search_replace_all();
    assert_eq!(
        app.pending_replace_all,
        Some((1, 1, 1)),
        "the modal counts exclude the dirty file and name the skip"
    );
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ))
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(&clean).unwrap(),
        "delta here\n",
        "the clean file is rewritten"
    );
    assert_eq!(
        std::fs::read_to_string(&dirty).unwrap(),
        "beta there\n",
        "the dirty-open file is untouched on disk"
    );
    assert!(
        app.status.contains("skipped dirty.txt") && app.status.contains("unsaved"),
        "the skip is named; was {:?}",
        app.status
    );
}

/// #257: Cmd+F in a rendered colour log must search what the user SEES.
///
/// The log view keeps the file windowed, so a log tab's `lines` is a stub
/// and the ordinary find path reads an empty buffer: the bar answers "No
/// results" over a file full of matches. `LogView::visible_text` was
/// written for exactly these consumers and had none.
#[test]
fn find_in_a_rendered_color_log_matches_the_visible_text() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("build.log");
    let mut body = String::new();
    for i in 0..200 {
        body.push_str(&format!("\u{1b}[32mINFO\u{1b}[0m step {i} ok\n"));
    }
    body.push_str("\u{1b}[31mERROR\u{1b}[0m disk full\n");
    std::fs::write(&p, &body).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    assert!(
        app.editor.log.is_some(),
        "the fixture must open as a rendered log"
    );

    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    for ch in "disk".chars() {
        app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }

    let state = app
        .editor_find
        .as_ref()
        .expect("Cmd+F opens the find bar over a log");
    assert_eq!(
        state.match_count, 1,
        "the one ERROR line must be found through the stripped text"
    );
    assert_eq!(
        app.editor.active_search_match.map(|(r, _, _)| r),
        Some(200),
        "the match is the last line of the file"
    );
    assert!(
        app.editor.scroll > 0,
        "the view must scroll to reveal the match, not sit at the top"
    );
}

/// #257 review finding: the find bar's `N+` belonged to the log tab that
/// earned it and leaked onto the next search. A text buffer is counted
/// exhaustively, so showing `N+` there claims a limit that does not exist.
#[test]
fn a_logs_budgeted_count_does_not_leak_onto_the_next_text_search() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("huge.log");
    // Past FIND_SCAN_BYTES, so the count genuinely runs out of budget.
    let line = "\u{1b}[31mERROR\u{1b}[0m ".to_string() + &"pad ".repeat(40) + "\n";
    let body = line.repeat((crate::log_view::FIND_SCAN_BYTES / line.len()) + 2000);
    std::fs::write(&log, &body).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&log).unwrap();
    assert!(app.editor.log.is_some(), "the fixture must open rendered");
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    for ch in "ERROR".chars() {
        app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }
    assert!(
        app.editor_find
            .as_ref()
            .is_some_and(|s| s.count_truncated()),
        "a log too large to search whole must report its count as partial"
    );

    // Now search an ordinary file, which IS counted exhaustively. The find
    // bar deliberately stays OPEN across the tab switch: closing it builds a
    // fresh EditorFind on the next Cmd+F, and this test then passes against
    // the very bug it exists to catch (round-2 review finding).
    let text = tmp.path().join("small.txt");
    std::fs::write(&text, "ERROR one\nplain\nERROR two\n").unwrap();
    app.editor.open(&text).unwrap();
    assert!(app.editor.log.is_none(), "a plain file is a text tab");
    assert!(
        app.editor_find.is_some(),
        "the find bar must still be the one the log search left behind"
    );
    // Retype the query into the SAME bar rather than opening a new one.
    app.editor_find.as_mut().unwrap().query.clear();
    for ch in "ERROR".chars() {
        app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }
    let state = app.editor_find.as_ref().expect("the find bar is open");
    assert_eq!(state.match_count, 2);
    assert!(
        !state.count_truncated(),
        "an exhaustive count must not inherit the log's N+"
    );
}

/// #257 round-4 review finding: a CHANGED query must not leave the previous
/// query's highlight painted. The early return that keeps a match across an
/// out-of-reach STEP was also firing on the retype path, where the old
/// position belongs to a query the user has already replaced.
#[test]
fn retyping_into_an_out_of_reach_query_clears_the_old_highlight() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("far.log");
    let mut body = String::from("\u{1b}[31mERROR\u{1b}[0m first and only\n");
    let filler = "quiet line with nothing of interest ".repeat(4);
    for _ in 0..(crate::log_view::FIND_SCAN_BYTES / filler.len() + 3000) {
        body.push_str(&filler);
        body.push('\n');
    }
    std::fs::write(&p, &body).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    for ch in "ERROR".chars() {
        app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        app.editor.active_search_match.map(|(r, _, _)| r),
        Some(0),
        "the match is found while the query still matches"
    );

    // Extend the query into something that matches nothing reachable.
    for ch in "ZZQX".chars() {
        app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        app.editor.active_search_match, None,
        "the previous query's highlight must not survive under a new query \
         that found nothing: the bar would say no match while a match stayed \
         painted"
    );
}

/// #257 follow-up: the find bar's title must FIT the bar.
///
/// The truncated-count arm shipped at 51 characters in a bar capped at 48
/// and painted as "...in the part that could be searc". Nothing rendered the
/// title, so nothing could catch it, and the replacement is protected only
/// by arithmetic no one re-runs. This renders every arm at the real width.
#[test]
fn every_find_bar_title_fits_the_bar_it_is_painted_in() {
    use crate::widgets::editor_find::{EditorFind, render_editor_find};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let opts = crate::widgets::search::SearchOpts::default();

    // Every arm of the title match, with the WHOLE title it must paint, and
    // how to reach it. `match_index` matters as much as the count: the
    // "N of M" arm is reached ONLY when it is Some, and `set_match_count`
    // does not touch it, so the first version of this test never rendered
    // that arm at all while claiming in its name to cover every one. It is
    // also the arm whose width grows with TWO runtime numbers, which makes
    // it the most able to overrun the bar.
    //
    // The expected string is the ENTIRE title, never a fragment of it. A
    // fragment cannot detect an overrun: text added BEFORE the fragment
    // pushes the tail off the end while leaving the fragment itself intact,
    // so `contains` still passes. Measured rather than reasoned: against the
    // fragment "9876543 of 9876543+", widening that arm to
    // " Find - match {idx} of {total} found in this whole file " (59 cells
    // painted into a 48-cell bar) left this test green. Only the full title
    // makes the clip observable, because the clip removes its end.
    //
    // Written out rather than rebuilt from the code's own format strings on
    // purpose: sharing the construction with the code under test would make
    // any widening agree with itself and pass.
    /// A title arm: the whole title it must paint, and how to reach it.
    type TitleCase = (&'static str, Box<dyn Fn(&mut EditorFind)>);

    let cases: Vec<TitleCase> = vec![
        // The empty-query arm is the BARE title, and it is asserted whole
        // for the same reason as the rest: every other arm also contains
        // "Find", so a fragment could not tell this arm from a wrong one.
        (" Find ", Box::new(|s: &mut EditorFind| s.query.clear())),
        (
            " Find — No results ",
            Box::new(|s: &mut EditorFind| s.set_match_count(0, false)),
        ),
        (
            " Find: no match in the part searched ",
            Box::new(|s: &mut EditorFind| s.set_match_count(0, true)),
        ),
        (
            " Find — 1234 matches ",
            Box::new(|s: &mut EditorFind| s.set_match_count(1234, false)),
        ),
        (
            " Find — 1234+ matches ",
            Box::new(|s: &mut EditorFind| s.set_match_count(1234, true)),
        ),
        (
            " Find — 7 of 1234 ",
            Box::new(|s: &mut EditorFind| {
                s.set_match_count(1234, false);
                s.match_index = Some(7);
            }),
        ),
        (
            " Find — 9876543 of 9876543+ ",
            Box::new(|s: &mut EditorFind| {
                // Both numbers as wide as they plausibly get on a log, since
                // this arm's length grows with each independently.
                s.set_match_count(9_876_543, true);
                s.match_index = Some(9_876_543);
            }),
        ),
    ];

    for (expected, prepare) in cases {
        let mut state = EditorFind::new(String::from("needle"), opts);
        prepare(&mut state);
        let mut buf = Buffer::empty(area);
        render_editor_find(&mut state, area, &mut buf, crate::theme::Theme::BLACK);

        let rect = state.last_rect;
        assert!(rect.width > 0, "the bar must have been painted");
        let row: String = (rect.x..rect.x + rect.width)
            .map(|x| buf[(x, rect.y)].symbol().to_string())
            .collect();
        // THE BAR'S TITLE AREA, not the buffer. Two renderers paint this
        // title and they clip differently: `Block` clips to its own title
        // area (`rect.width - 2`), while the gradient re-stamp in
        // `render_editor_find` uses `set_span`, whose clamp is the BUFFER's
        // right edge. `Theme::BLACK.gradient()` is true, so the second one
        // runs, and a title between 38 and 47 cells overruns the block
        // silently: it writes over the bar's own top-right corner and out
        // past the inner area, while a comparison anchored to the painted
        // row still matches, because the overrun is exactly the text the
        // comparison wanted. The width assertion is what makes that visible.
        assert!(
            expected.chars().count() <= (rect.width as usize).saturating_sub(2),
            "the {}-cell title does not fit a {}-cell bar's title area",
            expected.chars().count(),
            rect.width
        );
        // Exactly, at the position the block paints it: one cell in from
        // the left corner. `contains` would be weaker in both directions -
        // it cannot see a clipped tail when the wanted text is a fragment,
        // and for the bare " Find " arm every other title contains it too,
        // so a completely wrong title would satisfy it.
        let painted: String = row.chars().skip(1).take(expected.chars().count()).collect();
        assert_eq!(
            painted, expected,
            "the title did not fit whole in a {}-cell bar, the row painted {row:?}",
            rect.width
        );
        // And the bar is still a bar. The corner is the first thing an
        // overrunning title destroys, so it is the cheapest witness that the
        // title stayed inside the box it was painted in.
        assert_eq!(
            buf[(rect.x + rect.width - 1, rect.y)].symbol(),
            "\u{256e}",
            "the title overran the bar and ate its top-right corner: {row:?}"
        );
    }
}

/// #257 follow-up: the count and the highlight must agree at every step of
/// a search over a budgeted log.
///
/// Rounds 3, 4 and 5 of #393 each found one instance of a single class: the
/// bar's count and the painted highlight disagreeing. This walks the whole
/// gesture sequence and asserts the exact state after each one: type, step
/// forward into the out-of-reach branch (where round 3's defect lived),
/// step back, retype into a query that matches nothing, delete back.
///
/// NO SEPARATE INVARIANT HELPER, deliberately. An earlier version carried a
/// closure asserting `match_count == 0 => active_search_match.is_none()` at
/// each checkpoint, which reads as the general property behind the three
/// defects. It could not fail: `match_count == 0` holds at exactly one
/// checkpoint, and there the concrete assertion already fixes the highlight
/// to `None`, which is strictly stronger. Neutering the closure entirely
/// left the test green. Extending it over a prefix walk did not help either,
/// because the code has no per-length behaviour, so every state the walk
/// visits is behaviourally identical to one already enumerated.
///
/// A dead assertion whose name announces a property is worse than no
/// assertion: it makes a reader believe the class is covered. The concrete
/// assertions ARE the property, made checkable one state at a time.
///
/// HALF A PAIR. These pin `count == 0` with nothing painted. The other
/// direction, a count that survives while the highlight is thrown away, is
/// pinned by `a_step_that_runs_out_of_reach_keeps_the_match_it_already_had`.
///
/// WHICH CHECKPOINTS ARE NEW. Three of them re-assert what
/// `a_step_that_runs_out_of_reach_keeps_the_match_it_already_had` and
/// `retyping_into_an_out_of_reach_query_clears_the_old_highlight` already
/// cover; they are carried along because this test's subject is the
/// SEQUENCE, and a state is only reached by passing through them. The two
/// that no sibling covers are the backward step and the final backspace
/// into a query that matches again. A reader counting five independent
/// facts here would be counting wrong.
///
/// TWO REACHABLE MATCHES, not one. With a single match the Shift+Enter
/// checkpoint asserted nothing: `find_prev`'s leg 1 rejects the anchor's own
/// match, leg 2 walks back from EOF and runs out of budget before reaching
/// row 0, and `scroll_log_to_match` returns on `out_of_reach` WITHOUT
/// touching `active_search_match`. The assertion passed because the field
/// was never written. Measured: making `find_prev` return
/// `Found(row 0, col 999, len 5)` unconditionally left the test green. A
/// second match on row 1 puts a real backward step within budget, so the
/// checkpoint now fails if `find_prev`'s anchor filter or its column
/// arithmetic regresses.
#[test]
fn the_count_and_the_highlight_agree_at_every_step_of_a_log_search() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("budgeted.log");
    // TWO matches at the head, both inside the budget, so a backward step
    // has somewhere to land. Row 1 puts ERROR at character column 3, which
    // makes a wrong column visible rather than coincidentally right.
    let mut body = String::from("\u{1b}[31mERROR\u{1b}[0m first\n");
    body.push_str("no \u{1b}[31mERROR\u{1b}[0m second\n");
    let filler = "quiet line with nothing of interest ".repeat(4);
    // Comfortably past the budget, matching the margin the sibling tests use
    // (+3000 lines, ~11%). The earlier +200 was 1.4% over, and while the
    // budget is a pure byte counter with no timing in it, a change to window
    // accounting or to the stripped length of the colour header could flip
    // the sweep from OutOfReach to Absent and quietly change what four
    // assertions mean without any of them failing.
    for _ in 0..(crate::log_view::FIND_SCAN_BYTES / filler.len() + 3000) {
        body.push_str(&filler);
        body.push('\n');
    }
    std::fs::write(&p, &body).unwrap();
    assert!(
        body.len() > crate::log_view::FIND_SCAN_BYTES,
        "the fixture must outrun the sweep budget or the out-of-reach \
         checkpoints test nothing"
    );

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    assert!(app.editor.log.is_some(), "the fixture opens rendered");

    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    for ch in "ERROR".chars() {
        app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }
    // The WHOLE tuple, not just the row. A wrong column or length paints the
    // band in the wrong place, which is the defect class this test is for,
    // and comparing only the row cannot see it.
    assert_eq!(
        app.editor.active_search_match,
        Some((0, 0, 5)),
        "typing lands on the first match, at its own column"
    );
    assert_eq!(
        app.editor_find.as_ref().unwrap().match_count,
        2,
        "both head matches are counted, not merely some"
    );

    // Forward to the SECOND match, which is within budget, so this leg
    // asserts a real step rather than a refusal.
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.active_search_match,
        Some((1, 3, 5)),
        "the next match is row 1 at column 3, past the \"no \" prefix"
    );

    // Forward again: nothing further is reachable, so this exercises the
    // out-of-reach branch, which is where round 3's defect lived.
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.active_search_match,
        Some((1, 3, 5)),
        "a step that cannot reach further keeps the match it had"
    );
    assert!(
        app.editor_find.as_ref().unwrap().count_truncated(),
        "and the bar reports that the search did not cover the file"
    );

    // Backward from row 1 to row 0: within budget, so `find_prev`'s leg 1
    // answers and its result is actually consumed. With one match this leg
    // ran out of budget and `scroll_log_to_match` returned without writing
    // anything, so the assertion passed on an untouched field.
    app.handle_key(key(KeyCode::Enter, KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.editor.active_search_match,
        Some((0, 0, 5)),
        "stepping back lands on the previous match, not on the current one"
    );

    // Only now retype into something unmatchable, so the transition from a
    // painted match to none is itself a checkpoint.
    for ch in "ZZQX".chars() {
        app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        app.editor_find.as_ref().unwrap().match_count,
        0,
        "the new query matches nothing"
    );
    assert_eq!(
        app.editor.active_search_match, None,
        "so nothing may stay painted"
    );

    // And back: the highlight must return rather than staying cleared.
    for _ in 0..4 {
        app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        app.editor.active_search_match,
        Some((0, 0, 5)),
        "a query that matches again must paint its match, whole"
    );
}

/// #1538: a find-bar jump while a reloaded large log is still indexing is
/// the reader's place; the next index batch does not snap the view back to
/// the place the reload was settling.
#[test]
fn a_log_search_jump_during_a_reload_index_is_not_undone() {
    use ratatui::widgets::Widget as _;
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("huge.log");
    let mut body = Vec::new();
    body.extend_from_slice(b"\x1b[32mINFO\x1b[0m head\n");
    let mut lines = 1usize;
    while (body.len() as u64) < crate::log_view::HEAD_INDEX_BYTES + 1024 * 1024 {
        body.extend_from_slice(format!("line{lines} some padding text\n").as_bytes());
        lines += 1;
    }
    std::fs::write(&p, &body).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut *app.editor).render(area, &mut buf);
    app.editor.log.as_mut().unwrap().finish_index();
    app.editor.scroll_down(5);
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "\x1b[31mERROR\x1b[0m disk full").unwrap();
    }
    app.editor.reload_or_flag_conflict();
    assert!(app.editor.log.as_ref().unwrap().indexing());

    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    for ch in "line400 ".chars() {
        app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        app.editor.active_search_match.map(|(row, ..)| row),
        Some(400),
        "the search lands on its match"
    );
    let scroll = app.editor.scroll;
    assert!(scroll > 5 && scroll <= 400, "and the view moved to it");
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the background log index",
        || {
            app.poll_log_index();
            !app.editor.log.as_ref().unwrap().indexing()
        },
    );
    assert_eq!(app.editor.scroll, scroll, "the index did not move the view");
}

/// #369: Join Lines, a counted line delete, find and Replace All stay
/// inside the symbol.
#[test]
fn line_joins_counted_deletes_and_find_stay_in_the_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("two.rs");
    std::fs::write(&file, "fn a() {\n    1\n}\nfn b() {\n    2\n}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.editor.cursor_row = 1;
    app.run_command(crate::widgets::command_palette::Command::OpenAsSymbolTab);
    assert_eq!(app.editor.symbol_clip(), Some((0, 3)));
    let before = app.editor.lines.clone();

    app.editor.cursor_row = 2;
    app.editor.join_lines();
    assert_eq!(app.editor.lines, before, "the last line joins nothing");

    // Find wraps within the symbol: from `fn a`'s `}`, the next `}` is
    // that same one, not `fn b`'s.
    let opts = crate::widgets::search::SearchOpts::default();
    let m = app.editor_find_step(true, "}", opts, 2, 0, true).unwrap();
    assert_eq!(m.row, 2);

    let mut find = crate::widgets::editor_find::EditorFind::new("fn".into(), opts);
    find.replace_visible = true;
    find.replace = "FN".into();
    app.editor_find = Some(find);
    app.editor_find_replace_all();
    assert_eq!(app.editor.lines[0], "FN a() {");
    assert_eq!(app.editor.lines[3], "fn b() {", "outside the symbol");

    app.editor.cursor_row = 1;
    app.editor.delete_lines(10);
    assert_eq!(
        app.editor.lines,
        ["FN a() {", "fn b() {", "    2", "}"],
        "the count stops at the symbol's end"
    );
}

#[test]
fn a_rebuild_recomputes_the_find_match_against_the_new_rows() {
    // The active find match names a row of the OLD rows. After a rebuild it
    // is dropped and, with the find bar open, recomputed: gone when the new
    // rows no longer hold the needle, back when they do again.
    let (tmp, f) = repo_with_seed("alpha\n");
    let root = tmp.path().to_path_buf();
    let mut app = App::new(root.clone()).unwrap();
    std::fs::write(&f, "alpha\nneedle here\n").unwrap();
    app.editor
        .open_head_diff_with_text(
            std::path::PathBuf::from("seed.txt (HEAD)"),
            "alpha\n",
            &f,
            true,
        )
        .unwrap();
    app.tag_open_diff(crate::widgets::diff::DiffSource::HeadVsWorking {
        root: root.clone(),
        rel: String::from("seed.txt"),
    });
    app.open_editor_find();
    app.diff_find_set_query(String::from("needle"));
    assert!(
        app.editor.diff.as_ref().unwrap().find.active.is_some(),
        "precondition: the needle is found"
    );

    rewrite_later(&f, "alpha\nnothing to see\n");
    assert!(app.refresh_open_diff_views(false, &[], &no_paths()));
    let d = app.editor.diff.as_ref().unwrap();
    assert!(d.find.active.is_none(), "no row holds the needle now");
    assert_eq!(
        d.find.needle.as_deref(),
        Some("needle"),
        "the query itself survives"
    );
    assert_eq!(
        app.editor_find.as_ref().and_then(|s| s.match_index),
        None,
        "the find bar agrees: no current match"
    );

    rewrite_later(&f, "alpha\nthe needle is back\nneedle twice\n");
    assert!(app.refresh_open_diff_views(false, &[], &no_paths()));
    let d = app.editor.diff.as_ref().unwrap();
    assert!(
        d.find.active.is_some(),
        "the match is recomputed on the new rows"
    );
    assert_eq!(
        app.editor_find.as_ref().and_then(|s| s.match_index),
        Some(1),
        "and the find bar counts from the first"
    );
}

// ---- Search Editor (#615) ----

#[test]
fn a_search_editor_reruns_its_header_and_opens_results() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::write(root.join("a.txt"), "one\nthe needle is here\nthree\n").unwrap();
    std::fs::write(root.join("b.txt"), "no match\n").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::OpenSearchEditor);
    assert!(crate::search_editor::is_search_editor(&app.editor.lines));
    app.editor.lines[0] = String::from("# Query: needle");
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('R'), KeyModifiers::SHIFT)));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !app.drain_search_editor() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let text = app.editor.lines.join("\n");
    assert!(text.contains("1 result - 1 file"), "{text}");
    assert!(text.contains("a.txt:"), "{text}");
    let row = app
        .editor
        .lines
        .iter()
        .position(|l| l.contains("the needle is here"))
        .expect("result row");
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = row;
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ))
    .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(root.join("a.txt").as_path())
    );
    assert_eq!(app.editor.cursor_row, 1, "the match's line, 0-based");
}

/// #860: VS Code's `Alt+C` / `Alt+W` / `Alt+R` flip the Search side bar's
/// `Aa` / `ab` / `.*` toggles from any of its inputs, and `Alt+D` opens and
/// closes the include / exclude rows, all of which were click-only. Plain
/// letters still type.
#[test]
fn search_toggles_and_details_answer_the_keyboard() {
    use crate::widgets::search::{SearchField, SearchOpts};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    let alt = |c| key(KeyCode::Char(c), KeyModifiers::ALT);
    assert_eq!(app.search.opts, SearchOpts::default());
    app.handle_key(alt('c')).unwrap();
    assert!(app.search.opts.case_sensitive);
    assert_eq!(app.status, "Search: Match Case on");
    app.handle_key(alt('w')).unwrap();
    assert!(app.search.opts.whole_word);
    app.handle_key(alt('r')).unwrap();
    assert!(app.search.opts.use_regex);
    assert!(
        app.search.query.is_empty(),
        "the keys toggle, they do not type"
    );
    // Alt+D reveals include / exclude, and the toggles work from there too.
    app.handle_key(alt('d')).unwrap();
    assert!(app.search.details_open);
    app.search.focus_field(SearchField::Include);
    app.handle_key(alt('c')).unwrap();
    assert!(!app.search.opts.case_sensitive);
    assert_eq!(app.status, "Search: Match Case off");
    assert!(app.search.include.is_empty());
    app.handle_key(alt('d')).unwrap();
    assert!(!app.search.details_open);
    assert_eq!(
        app.search.field,
        SearchField::Query,
        "closing the rows hands focus back to the query"
    );
    app.handle_key(key(KeyCode::Char('c'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.search.query, "c");
}

/// #1612: `Alt+P` in the Search side bar flips Preserve Case, names it on
/// the status line and types nothing; the palette carries it too.
#[test]
fn alt_p_toggles_preserve_case_in_the_search_side_bar() {
    use crate::widgets::command_palette::{ALL_COMMANDS, Command};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.open_replace();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::ALT))
        .unwrap();
    assert!(app.search.opts.preserve_case);
    assert_eq!(app.status, "Search: Preserve Case on");
    assert!(
        app.search.replace.is_empty(),
        "the key toggles, it does not type"
    );
    // Negative: the other toggles are untouched.
    assert!(!app.search.opts.case_sensitive && !app.search.opts.use_regex);
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::ALT))
        .unwrap();
    assert!(!app.search.opts.preserve_case);
    assert_eq!(app.status, "Search: Preserve Case off");
    assert!(ALL_COMMANDS.contains(&Command::SearchTogglePreserveCase));
    app.show_tree = false;
    app.run_command(Command::SearchTogglePreserveCase);
    assert!(app.show_tree);
    assert!(app.search.opts.preserve_case);
}

/// #1612: flipping Preserve Case while a search runs keeps its results:
/// the worker's events carry the old toggle, and the search still
/// completes so Replace All is not left blocked.
#[test]
fn toggling_preserve_case_mid_search_keeps_the_search_live() {
    use crate::widgets::search::SearchEvent;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("user");
    let before = app.search.opts;
    app.toggle_search_opt(crate::widgets::search::SearchToggle::PreserveCase);
    assert!(app.search.opts.preserve_case);
    // Drain whatever the real worker sent, then hand in events stamped
    // with the options from before the toggle.
    app.drain_search_results();
    app.search.complete = false;
    let (tx, rx) = std::sync::mpsc::channel();
    app.search_results_rx = rx;
    tx.send(SearchEvent::Done(String::from("user"), before))
        .unwrap();
    assert!(app.drain_search_results());
    assert!(app.search.complete, "the search must finish");
    // Negative: a result under a different Match Case is still dropped.
    app.search.complete = false;
    let mut other = before;
    other.case_sensitive = !other.case_sensitive;
    tx.send(SearchEvent::Done(String::from("user"), other))
        .unwrap();
    assert!(!app.drain_search_results());
    assert!(!app.search.complete);
}

/// #860: `Ctrl/Cmd+Shift+H` is Replace in Files: the Search side bar opens
/// with its Replace row expanded and focused, from the editor, and a second
/// press never collapses it. In the live terminal `Ctrl+Shift+H` stays
/// command history.
#[test]
fn replace_in_files_opens_the_replace_row_from_anywhere() {
    use crate::widgets::search::SearchField;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "foo");
    let ctrl_shift_h = key(
        KeyCode::Char('H'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    app.handle_key(ctrl_shift_h).unwrap();
    assert!(app.sidebar_view == SidebarView::Search);
    assert!(app.focus == Pane::Tree);
    assert!(app.search.replace_open);
    assert_eq!(app.search.field, SearchField::Replace);
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.search.replace, "x", "typing lands in Replace");
    assert!(app.search.query.is_empty());
    app.search.focus_field(SearchField::Query);
    app.handle_key(key(
        KeyCode::Char('h'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(app.search.replace_open, "the chord only ever opens the row");
    assert_eq!(app.search.field, SearchField::Replace);
    // The live shell keeps its command history on Ctrl+Shift+H.
    app.search.replace_open = false;
    app.set_bottom_panel_tab(BottomPanelTab::Terminal);
    app.handle_key(ctrl_shift_h).unwrap();
    assert!(app.command_history_popup.is_some());
    assert!(!app.search.replace_open);
}

/// #860: the palette carries Replace in Files and the four side bar
/// toggles; run from there (the side bar hidden) they reveal it, so the flip
/// is seen.
#[test]
fn search_commands_run_from_the_palette_reveal_the_side_bar() {
    use crate::widgets::command_palette::{ALL_COMMANDS, Command};
    use crate::widgets::search::SearchField;
    for cmd in [
        Command::ReplaceInFiles,
        Command::SearchToggleMatchCase,
        Command::SearchToggleWholeWord,
        Command::SearchToggleRegex,
        Command::SearchToggleDetails,
    ] {
        assert!(ALL_COMMANDS.contains(&cmd), "{cmd:?} missing from palette");
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "foo");
    app.show_tree = false;
    app.run_command(Command::SearchToggleWholeWord);
    assert!(app.show_tree);
    assert!(app.sidebar_view == SidebarView::Search);
    assert!(app.search.opts.whole_word);
    app.run_command(Command::SearchToggleDetails);
    assert!(app.search.details_open);
    app.run_command(Command::ReplaceInFiles);
    assert!(app.search.replace_open);
    assert_eq!(app.search.field, SearchField::Replace);
}

/// #860 negative: the Search side bar's `Alt` toggles belong to its inputs.
/// From the editor, `Alt+C` / `W` / `R` / `D` leave the toggles and the
/// include / exclude rows alone, and inside the inputs only a bare `Alt`
/// flips one (`Ctrl+Alt+C` is not Match Case).
#[test]
fn search_alt_toggles_stay_inside_the_search_inputs() {
    use crate::widgets::search::SearchOpts;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "foo");
    app.set_sidebar_view(SidebarView::Search);
    app.focus_pane(Pane::Editor);
    for c in ['c', 'w', 'r', 'd'] {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::ALT))
            .unwrap();
    }
    assert_eq!(app.search.opts, SearchOpts::default());
    assert!(!app.search.details_open);
    app.set_sidebar_view(SidebarView::Search);
    assert!(
        app.focus == Pane::Tree,
        "fixture: the Search input has focus"
    );
    app.handle_key(key(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert!(!app.search.opts.case_sensitive, "only a bare Alt toggles");
}

/// #860 negative: Replace in Files only ever opens the row; with it open
/// and text in both fields, the chord keeps the query, the replacement and
/// the toggles as they were.
#[test]
fn replace_in_files_keeps_what_the_search_already_holds() {
    use crate::widgets::search::SearchField;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "foo");
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("foo");
    app.search.replace = String::from("bar");
    app.search.opts.whole_word = true;
    app.focus_pane(Pane::Editor);
    app.handle_key(key(
        KeyCode::Char('H'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(app.search.replace_open);
    assert_eq!(app.search.field, SearchField::Replace);
    assert_eq!(app.search.query, "foo");
    assert_eq!(app.search.replace, "bar");
    assert!(app.search.opts.whole_word);
}

#[test]
fn find_seeds_the_whole_word_and_typing_replaces_it() {
    // Caret after `def loadOr`, mid-word.
    let (mut app, _tmp) = find_seed_app(0, 10);
    app.handle_key(ctrl('f')).unwrap();
    assert_eq!(find_query(&app), "loadOrders");
    type_str(&mut app, "save");
    assert_eq!(find_query(&app), "save");
    assert!(app.editor_find.as_ref().unwrap().match_count >= 1);
}

#[test]
fn find_seeds_the_word_the_caret_just_left() {
    // Caret right after `loadOrders`, where it sits after typing a word.
    let (mut app, _tmp) = find_seed_app(0, 14);
    app.handle_key(ctrl('f')).unwrap();
    assert_eq!(find_query(&app), "loadOrders");
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(find_query(&app), "", "Backspace clears a selected seed");
}

#[test]
fn ctrl_f_on_an_open_bar_selects_its_query_again() {
    let (mut app, _tmp) = find_seed_app(0, 10);
    app.handle_key(ctrl('f')).unwrap();
    type_str(&mut app, "save");
    app.handle_key(ctrl('f')).unwrap();
    type_str(&mut app, "write");
    assert_eq!(find_query(&app), "write");
}

#[test]
fn an_unseeded_or_deselected_query_still_appends() {
    // Negative: with nothing to seed, typing appends as before; after a
    // caret key the seed is no longer selected and typing extends it.
    let (mut app, _tmp) = find_seed_app(2, 0);
    app.handle_key(ctrl('f')).unwrap();
    assert_eq!(find_query(&app), "");
    type_str(&mut app, "sa");
    type_str(&mut app, "ve");
    assert_eq!(find_query(&app), "save");
    let (mut app, _tmp) = find_seed_app(0, 10);
    app.handle_key(ctrl('f')).unwrap();
    app.handle_key(key(KeyCode::End, KeyModifiers::NONE))
        .unwrap();
    type_str(&mut app, "X");
    assert_eq!(find_query(&app), "loadOrdersX");
}

#[test]
fn search_results_are_announced_with_the_count_and_the_selected_match() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus = Pane::Tree;
    app.sidebar_view = SidebarView::Search;
    assert_eq!(app.a11y_snapshot().focus, "Search");
    assert_eq!(
        app.a11y_snapshot().item,
        None,
        "no results, nothing selected"
    );
    let hit = |file: &str, line_no: usize, text: &str| crate::widgets::search::SearchHit {
        path: tmp.path().join(file),
        line_no,
        line_text: text.into(),
        matches: 1,
    };
    app.search.hits = vec![
        hit("app.py", 1, "def total(xs):"),
        hit("b.py", 1, "total = 0"),
        hit("b.py", 3, "total += i"),
    ];
    app.search.selected = 2;
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Search, 3 matches in 2 files");
    assert_eq!(snap.item.as_deref(), Some("b.py line 3: total += i"));
}

/// #1295: the announced total counts matches, not matching lines, the same
/// as the Search header: a line with two matches is two.
#[test]
fn search_announces_every_match_on_a_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus = Pane::Tree;
    app.sidebar_view = SidebarView::Search;
    app.search.hits = vec![crate::widgets::search::SearchHit {
        path: tmp.path().join("b.py"),
        line_no: 3,
        line_text: "total += total".into(),
        matches: 2,
    }];
    assert_eq!(app.a11y_snapshot().focus, "Search, 2 matches in 1 file");
}
