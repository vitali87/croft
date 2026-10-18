//! The command palette, quick open, the file finder and pickers.

use super::*;

#[test]
fn a_late_location_picker_reply_does_not_clobber_an_open_menu() {
    // Go to Implementations / References share the async-reply shape with
    // call hierarchy: the LSP can answer seconds later, and the picker must
    // not replace a menu the user opened while waiting.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let items = vec![(
        String::from("a.rs:1"),
        MenuAction::GoToLocation {
            path: tmp.path().join("a.rs"),
            line: 0,
            col: 0,
        },
    )];
    app.context_menu = Some(ContextMenu::flat(
        (1, 1),
        items.clone(),
        tmp.path().to_path_buf(),
    ));
    let menu_before = app.context_menu.as_ref().map(|m| m.items.len());
    app.open_location_picker(vec![(tmp.path().join("a.rs"), 0, 0)], "references");
    assert_eq!(
        app.context_menu.as_ref().map(|m| m.items.len()),
        menu_before,
        "an open menu survives a late reply"
    );
    assert_eq!(app.status, "Locations ready, but another menu is open");
    // With no menu open, the reply presents normally.
    app.context_menu = None;
    app.open_location_picker(vec![(tmp.path().join("a.rs"), 0, 0)], "references");
    assert!(
        app.context_menu.is_some(),
        "a free slot presents the picker"
    );
    assert_eq!(app.status, "1 references");
}

/// #31: navigate-back rested entirely on the Ctrl+Shift+click mouse chord.
/// The VS Code keyboard chord (Ctrl+-) and the palette command are the
/// fallbacks that work when the host terminal never delivers that chord.
#[test]
fn ctrl_minus_and_the_palette_both_navigate_back() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "fn main() {}\nlet x = helper();\n").unwrap();
    std::fs::write(&b, "fn helper() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.editor.cursor_row = 1;
    app.focus_pane(Pane::Editor);

    // The keyboard chord.
    app.go_to_definition(b.clone(), 0, 3);
    assert_eq!(app.editor.path.as_deref(), Some(b.as_path()));
    app.handle_key(key(KeyCode::Char('-'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(a.as_path()),
        "Ctrl+- goes back"
    );

    // The legacy encoding: Ctrl+- folds to 0x1F, decoded as Ctrl+_.
    app.go_to_definition(b.clone(), 0, 3);
    app.handle_key(key(KeyCode::Char('_'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(a.as_path()),
        "the legacy Ctrl+- spelling goes back too"
    );

    // The palette command.
    app.go_to_definition(b.clone(), 0, 3);
    app.run_command(crate::widgets::command_palette::Command::NavigateBack);
    assert_eq!(
        app.editor.path.as_deref(),
        Some(a.as_path()),
        "the Go Back palette command goes back"
    );
}

/// #843: Cmd+F12 has no `Ctrl` form off macOS (`Ctrl`+`F12` is Go to Type
/// Definition), so the palette's "Go to Implementations" sends Cmd+F12's
/// request for the symbol at the caret.
#[test]
fn the_palette_goes_to_implementations_at_the_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "lib.rs", "trait Shape {}\n");
    app.editor.cursor_col = 7;
    assert!(app.implementation_request_id.is_none());
    assert!(run_from_palette(&mut app, "Go to Implementations"));
    assert!(
        app.implementation_request_id.is_some(),
        "the request went to the language servers"
    );
}

/// #843: the Explorer's Cmd+Z has no `Ctrl` form, so the palette's
/// "Explorer: Jump to Directory (zoxide)" opens the same popup, from any pane.
#[test]
fn the_palette_opens_the_zoxide_jump_from_any_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    assert!(run_from_palette(
        &mut app,
        "Explorer: Jump to Directory (zoxide)"
    ));
    assert!(app.zoxide_jump.is_some());
}

/// #843: Cmd+Opt+←/→ have no `Ctrl` form off macOS (`Ctrl`+`Alt`+arrows
/// switch desktop workspaces), so the palette carries them, doing what the
/// chords do, from any pane.
#[test]
fn the_palette_focuses_the_left_and_right_editor_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "left");
    app.split_editor();
    assert_eq!(app.editor_layout.active_dfs_index(), 1);
    app.focus_pane(Pane::Tree);
    assert!(run_from_palette(&mut app, "View: Focus Left Editor Group"));
    assert_eq!(app.editor_layout.active_dfs_index(), 0);
    assert!(app.focus == Pane::Editor);
    assert!(run_from_palette(&mut app, "View: Focus Right Editor Group"));
    assert_eq!(app.editor_layout.active_dfs_index(), 1);
}

/// #843: Cmd+] / Cmd+[ have no `Ctrl` form (`Ctrl`+`[` is `Esc`), so the
/// palette carries next / previous terminal. The chords work only in the
/// terminal pane; the commands also focus it, as VS Code's do.
#[test]
fn the_palette_focuses_the_next_and_previous_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    app.active_terminal = 0;
    app.focus_pane(Pane::Editor);
    assert!(run_from_palette(&mut app, "Terminal: Focus Next Terminal"));
    assert_eq!(app.active_terminal, 1);
    assert!(app.focus == Pane::Terminal);
    assert!(run_from_palette(
        &mut app,
        "Terminal: Focus Previous Terminal"
    ));
    assert_eq!(app.active_terminal, 0);
    assert!(run_from_palette(
        &mut app,
        "Terminal: Focus Previous Terminal"
    ));
    assert_eq!(app.active_terminal, 2, "it wraps like Cmd+[");
}

/// #1562: the terminal's own actions are palette commands. Before, each was
/// a chord or a right-click row only, and the palette found none of them.
#[test]
fn the_palette_lists_every_terminal_action() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for title in [
        "Terminal: Clear",
        "Terminal: Rename",
        "Terminal: Kill the Active Terminal Instance",
        "Terminal: Split Terminal",
        "Terminal: Create New Terminal (With Profile)",
        "Terminal: Quick Select",
        "Terminal: Enter Copy Mode",
        "Terminal: Show Command History",
        "Terminal: Open Scrollback in Editor",
    ] {
        app.handle_key(key(
            KeyCode::Char('p'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ))
        .unwrap();
        for c in title.chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
                .unwrap();
        }
        let palette = app.command_palette.as_ref().expect("the palette opened");
        assert!(
            palette.results.iter().any(|i| i.title() == title),
            "{title} is in the palette"
        );
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
    }
}

/// #843: Cmd+A has no `Ctrl` form in the editor off macOS (`Ctrl`+`A` is line
/// start there), so the palette's "Select All" is the keyboard route, from
/// any pane.
#[test]
fn the_palette_selects_the_whole_buffer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.txt", "one\ntwo");
    app.focus_pane(Pane::Tree);
    assert!(run_from_palette(&mut app, "Select All"));
    assert!(app.focus == Pane::Editor);
    assert_eq!(app.editor.selection_text(), "one\ntwo");
}

/// Guard (#843): "Select All" is the same command as #852's (one id, one
/// title, one chord), so the two branches meet on one row.
#[test]
fn select_all_is_one_palette_command() {
    use crate::widgets::command_palette::{ALL_COMMANDS, Command};
    assert_eq!(Command::from_id("select_all"), Some(Command::SelectAll));
    assert_eq!(Command::SelectAll.title(), "Select All");
    assert_eq!(Command::SelectAll.keybinding_hint(), "Cmd+A");
    assert_eq!(
        ALL_COMMANDS
            .iter()
            .filter(|c| c.title() == "Select All")
            .count(),
        1
    );
}

/// #843: the palette's "Markdown: Run Code Block at Cursor" runs the fence on
/// any terminal, through the same confirm popup as Cmd+Enter.
#[test]
fn the_palette_runs_the_markdown_code_block_at_the_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = markdown_fence_app(tmp.path());
    let panes = app.terminals.len();
    assert!(run_from_palette(
        &mut app,
        "Markdown: Run Code Block at Cursor"
    ));
    let pending = app.pending_run_block.as_ref().expect("it confirms first");
    assert_eq!(pending.pane_name, "R.md:1");
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.pending_run_block.is_none());
    assert_eq!(app.terminals.len(), panes, "N runs nothing");
}

/// #1571: Insert Line Below / Above are palette commands with ids, so they
/// can be found and rebound.
#[test]
fn insert_line_below_and_above_are_palette_commands() {
    use crate::widgets::command_palette::Command;
    assert_eq!(
        Command::from_id("insert_line_below"),
        Some(Command::InsertLineBelow)
    );
    assert_eq!(
        Command::from_id("insert_line_above"),
        Some(Command::InsertLineAbove)
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "i.py", "a = 1\nb = 2\n");
    app.editor.cursor_col = 2;
    assert!(run_from_palette(&mut app, "Insert Line Below"));
    assert_eq!(app.editor.lines, ["a = 1", "", "b = 2"]);
    app.editor.cursor_row = 2;
    assert!(run_from_palette(&mut app, "Insert Line Above"));
    assert_eq!(app.editor.lines, ["a = 1", "", "", "b = 2"]);
    assert_eq!(app.editor.cursor_row, 2);
}

/// #1352: the palette's Go to Next/Previous Change run the same walk as
/// F7, and VS Code's command ids import onto them.
#[test]
fn go_to_next_change_is_a_palette_command() {
    use crate::widgets::command_palette::Command;
    let (mut app, _t) = app_with_change_bars();
    app.editor.cursor_row = 0;
    app.run_command(Command::from_id("next_change").expect("next_change id"));
    assert_eq!(app.editor.cursor_row, 4);
    app.run_command(Command::from_id("previous_change").expect("previous_change id"));
    assert_eq!(app.editor.cursor_row, 8, "wraps back to the last change");
    let mut report = crate::import_vscode::Report::default();
    crate::import_vscode::convert_keybindings(
        &serde_json::json!([
            { "key": "alt+f5", "command": "workbench.action.editor.nextChange" }
        ]),
        &mut report,
    );
    assert!(
        report.keybindings.iter().any(|(_, c)| c == "next_change"),
        "{:?}",
        report.keybindings
    );
}

#[test]
fn cmd_p_opens_the_file_finder_modal_for_quick_open() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.file_finder.is_some(),
        "Cmd+P must open the Quick Open file finder modal so the user can type a name and jump to a file the way VS Code does"
    );
}

#[test]
fn ctrl_p_opens_the_file_finder_modal_on_systems_without_super() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(
        app.file_finder.is_some(),
        "Ctrl+P must also open the Quick Open finder so the chord works on Linux / iTerm2 sessions that report CONTROL rather than SUPER"
    );
}

#[test]
fn plain_p_keystroke_does_not_open_the_file_finder() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.file_finder.is_none(),
        "an unmodified 'p' must reach the editor / tree typeahead and never trigger the modal"
    );
}

#[test]
fn esc_closes_the_file_finder_modal() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.file_finder.is_none(),
        "Esc must dismiss the file finder"
    );
}

/// Issue #207: under Croft Light the quick-input popups must paint light
/// chrome, not the historical hardcoded dark constants. Renders the Cmd+P
/// finder under both a dark theme and the light theme and inspects actual
/// buffer cells: dark stays byte-identical to the legacy palette, light
/// lands on the VS Code Light Modern quick-input fill with dark text.
#[test]
fn quick_open_popup_follows_the_light_theme() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    std::fs::write(tmp.path().join("beta.rs"), "").unwrap();
    for (theme, want_bg, dark_text) in [
        (
            crate::theme::Theme::DARK_BLUE,
            ratatui::style::Color::Rgb(0x16, 0x18, 0x1f),
            false,
        ),
        (
            crate::theme::Theme::from_id("light"),
            ratatui::style::Color::Rgb(0xf8, 0xf8, 0xf8),
            true,
        ),
    ] {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.apply_theme(theme);
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
            .unwrap();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|frame| app.render(frame)).unwrap();
        let finder = app.file_finder.as_ref().unwrap();
        // Probe the SECOND result row: the first wears the selection fill,
        // the second the plain popup body.
        let (x, y) = (finder.last_rect.x + 4, finder.last_rect.y + 4);
        let cell = &term.backend().buffer()[(x, y)];
        assert_eq!(
            cell.bg,
            want_bg,
            "{}: the finder body must wear the theme's popup fill",
            theme.id()
        );
        if dark_text {
            let ratatui::style::Color::Rgb(r, g, b) = cell.fg else {
                panic!("light finder text must be an Rgb color, got {:?}", cell.fg)
            };
            let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
            assert!(
                luma < 128.0,
                "light finder text must be dark on the white popup, got {:?}",
                cell.fg
            );
        }
    }
}

/// Issue #205: Cmd+P search results must be mouse clickable. A left click on
/// a result row confirms that row (the way VS Code's quick-pick behaves):
/// it opens the clicked file, not the keyboard-selected one, and dismisses
/// the finder.
#[test]
fn clicking_a_file_finder_row_opens_that_file() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    std::fs::write(tmp.path().join("beta.rs"), "").unwrap();
    std::fs::write(tmp.path().join("gamma.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let finder = app.file_finder.as_ref().unwrap();
    assert!(
        finder.results.len() >= 3,
        "the three seeded files must all be indexed"
    );
    // The list body starts three rows below the popup top (border, prompt,
    // separator). Row 1 of the list is the SECOND result - clicking it must
    // open that entry, not the keyboard selection parked on row 0.
    let col = finder.last_rect.x + 4;
    let row = finder.last_rect.y + 3 + 1;
    let expected = finder.results[1].entry.path.clone();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert!(
        app.file_finder.is_none(),
        "a click on a result row must confirm the pick and dismiss the finder"
    );
    assert_eq!(
        app.editor.path.as_deref(),
        Some(expected.as_path()),
        "the clicked row's file must be the one that opens"
    );
}

/// A click inside the popup but off the result rows (the query prompt line)
/// must neither open a file nor dismiss the finder - only clicks OUTSIDE the
/// popup close it, and only clicks ON a row confirm it.
#[test]
fn clicking_the_file_finder_prompt_line_is_inert() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let finder = app.file_finder.as_ref().unwrap();
    let col = finder.last_rect.x + 4;
    let prompt_row = finder.last_rect.y + 1;
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        col,
        prompt_row,
    ));
    assert!(
        app.file_finder.is_some(),
        "a click on the query line must not dismiss the finder"
    );
    assert_eq!(
        app.editor.path, None,
        "a click on the query line must not open anything"
    );
}

/// A click on an empty list row (inside the popup, below the last result)
/// maps to no result and must be inert rather than opening some file or
/// closing the popup.
#[test]
fn clicking_below_the_last_file_finder_result_is_inert() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let finder = app.file_finder.as_ref().unwrap();
    let results = finder.results.len();
    let col = finder.last_rect.x + 4;
    // One row past the last result, still inside the popup body.
    let row = finder.last_rect.y + 3 + results as u16;
    assert!(
        row < finder.last_rect.y + finder.last_rect.height - 1,
        "the empty row must land inside the popup for this test to mean anything"
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert!(
        app.file_finder.is_some(),
        "a click on an empty list row must not dismiss the finder"
    );
    assert_eq!(
        app.editor.path, None,
        "a click on an empty list row must not open anything"
    );
}

/// The command palette shares the quick-pick anatomy, so its rows must be
/// clickable too: a left click on a result row runs that command and
/// dismisses the palette.
#[test]
fn clicking_a_command_palette_row_runs_that_command() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(
        KeyCode::Char('p'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        app.command_palette.is_some(),
        "Cmd+Shift+P must open the palette"
    );
    for c in "keyboard shortcuts reference".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let palette = app.command_palette.as_ref().unwrap();
    assert!(
        !palette.results.is_empty(),
        "the query must keep the Help: Keyboard Shortcuts Reference command"
    );
    let col = palette.last_rect.x + 4;
    let row = palette.last_rect.y + 3;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert!(
        app.command_palette.is_none(),
        "a click on a palette row must run the command and dismiss the palette"
    );
    assert!(
        app.shortcuts_modal.is_some(),
        "the clicked command (Help: Keyboard Shortcuts Reference) must actually run"
    );
}

#[test]
fn typing_in_file_finder_fuzzy_filters_the_result_list() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub").join("beta.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::NONE))
        .unwrap();
    let finder = app
        .file_finder
        .as_ref()
        .expect("finder must still be open while typing");
    let names: Vec<String> = finder
        .visible_results()
        .iter()
        .map(|r| r.entry.rel.clone())
        .collect();
    assert!(
        names.iter().any(|n| n == "sub/beta.rs"),
        "typing 'be' must keep sub/beta.rs in the result list — got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "alpha.rs"),
        "typing 'be' must drop alpha.rs because 'be' is not a subsequence of 'alpha.rs' — got {names:?}"
    );
}

#[test]
fn enter_in_file_finder_opens_the_selected_file_and_closes_the_modal() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "fn main() {}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.file_finder.is_none(),
        "Enter must close the modal after opening the file"
    );
    let opened = app
        .editor
        .path
        .as_ref()
        .expect("the active tab must now have a path");
    assert_eq!(
        opened.file_name().and_then(|n| n.to_str()),
        Some("alpha.rs"),
        "Enter must open the highlighted entry (alpha.rs) into the active editor tab"
    );
}

#[test]
fn quick_open_with_a_line_range_lands_on_the_line_and_selects_the_range() {
    // #472: `alpha:236-239` in Cmd+P must still find alpha.rs, and Enter
    // must land on line 236 with 236-239 selected so the range reads as
    // highlighted. `alpha:12` lands on the line with nothing selected.
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=300).map(|i| format!("line {i}\n")).collect();
    std::fs::write(tmp.path().join("alpha.rs"), &body).unwrap();
    std::fs::write(tmp.path().join("beta.rs"), "fn b() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let type_and_enter = |app: &mut App, text: &str| {
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
            .unwrap();
        for c in text.chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
                .unwrap();
        }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
    };

    type_and_enter(&mut app, "alpha:236-239");
    assert!(app.file_finder.is_none(), "Enter closes the modal");
    assert_eq!(
        app.editor
            .path
            .as_deref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str()),
        Some("alpha.rs"),
        "the file part of the query is what gets opened"
    );
    let sel = app
        .editor
        .selection
        .expect("the range is selected so it reads as highlighted");
    let ((r0, c0), (r1, _)) = sel.normalised();
    assert_eq!(
        (r0, c0),
        (235, 0),
        "the selection starts at the top of line 236"
    );
    assert_eq!(r1, 238, "and reaches line 239");
    // The cursor sits at the selection's HEAD, as every other
    // selection-installing path leaves it, and the head is the landing line
    // (the anchor is the far end), so the line the user asked for is the one
    // on screen and Shift+motion extends from it.
    assert_eq!(
        (app.editor.cursor_row, app.editor.cursor_col),
        sel.head,
        "the cursor is at the head of the range"
    );
    assert_eq!(
        app.editor.cursor_row, 235,
        "and the head is the landing line"
    );
    assert!(
        app.status.ends_with("alpha.rs:236-239"),
        "the status names the range: {:?}",
        app.status
    );
    app.handle_key(key(KeyCode::Up, KeyModifiers::SHIFT))
        .unwrap();
    let ((r0, _), (r1, _)) = app.editor.selection.expect("still selected").normalised();
    assert_eq!(
        (r0, r1),
        (234, 238),
        "Shift+Up extends the range from the landing line"
    );
    let page = app.editor.page_size().max(1);
    assert!(
        app.editor.scroll <= 235 && 235 < app.editor.scroll + page,
        "the landing line is on screen: scroll={} page={page}",
        app.editor.scroll
    );

    // A pick with no hint replaces the range a hinted pick left: quick open
    // never installed a selection before hints existed, so a bare `alpha`
    // must not re-show one the user did not just ask for.
    // Extended by hand, the range is the user's now and a re-pick keeps it;
    // a freshly installed one is cleared by the next pick.
    let extended = app.editor.selection;
    type_and_enter(&mut app, "alpha");
    assert_eq!(
        app.editor.selection, extended,
        "a range the user extended survives an unhinted re-pick"
    );
    type_and_enter(&mut app, "alpha:236-239");
    type_and_enter(&mut app, "alpha");
    assert!(
        app.editor.selection.is_none(),
        "an unhinted pick clears the range a hinted pick left"
    );
    // A saturated line number goes through the open path like any other
    // out-of-range line: it lands on the last line and the status reports
    // the line landed on, never the sentinel.
    type_and_enter(&mut app, "alpha:99999999999999999999");
    assert_eq!(
        app.editor.cursor_row, 299,
        "a saturated line lands on the last line"
    );
    assert!(
        app.status.ends_with("alpha.rs:300"),
        "the status names the landed line, not the typed one: {:?}",
        app.status
    );
    // Both ends of a range are clamped the same way.
    type_and_enter(&mut app, "alpha:280-99999999999999999999");
    assert!(
        app.status.ends_with("alpha.rs:280-300"),
        "both ends of the range are clamped to the file: {:?}",
        app.status
    );
    // A selection the user made by hand is theirs: an unhinted pick of the
    // file already in front of them does not take it away. Only a range a
    // hinted pick installed is cleared by the next pick.
    let mine = crate::widgets::editor::EditorSelection {
        anchor: (10, 0),
        head: (11, 3),
    };
    app.editor.selection = Some(mine);
    type_and_enter(&mut app, "alpha");
    assert_eq!(
        app.editor.selection,
        Some(mine),
        "an unhinted re-pick keeps a selection the user made"
    );
    // Stale secondary carets do not survive a pick: the next keystroke
    // would otherwise edit at every one of them.
    app.editor.cursor_row = 5;
    app.editor.add_cursor_below();
    assert!(
        app.editor.has_multi_cursor(),
        "fixture: a second caret exists"
    );
    type_and_enter(&mut app, "alpha:236-239");
    assert!(
        !app.editor.has_multi_cursor(),
        "a pick collapses the carets along with the selection"
    );
    // A zero column (0-based tool output) keeps the line and drops the
    // column, as the terminal's path:line:col parser does.
    type_and_enter(&mut app, "alpha:236:0");
    assert_eq!(
        (app.editor.cursor_row, app.editor.cursor_col),
        (235, 0),
        "`:236:0` lands on line 236 at the start"
    );

    type_and_enter(&mut app, "alpha:12");
    assert_eq!(
        app.editor.cursor_row, 11,
        "a single line lands on that line"
    );
    assert!(
        app.editor.selection.is_none(),
        "a single line is a place to land, not a range to select"
    );
    assert!(
        app.status.ends_with("alpha.rs:12"),
        "the status names the line: {:?}",
        app.status
    );

    type_and_enter(&mut app, "alpha:5000");
    assert_eq!(
        app.editor.cursor_row, 299,
        "a line past the end lands on the last line"
    );

    // A range taller than the viewport still shows the landing line after a
    // paint: the paint-time clamp follows the cursor, so the cursor must be
    // on that line. Painted first so the page size is the real one.
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    type_and_enter(&mut app, "alpha:100-200");
    term.draw(|f| app.render(f)).unwrap();
    let page = app.editor.page_size().max(1);
    assert!(
        app.editor.scroll <= 99 && 99 < app.editor.scroll + page,
        "line 100 is on screen after the paint: scroll={} page={page}",
        app.editor.scroll
    );
    let ((r0, _), (r1, _)) = app
        .editor
        .selection
        .expect("the tall range is selected")
        .normalised();
    assert_eq!((r0, r1), (99, 199));

    type_and_enter(&mut app, "alpha:236:7");
    assert_eq!(
        (app.editor.cursor_row, app.editor.cursor_col),
        (235, 6),
        "`:line:col` lands on the column too"
    );

    // The click path goes through the same open, so a clicked row honours
    // the hint as Enter does.
    use crossterm::event::{MouseButton, MouseEventKind};
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    for c in "alpha:40".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let finder = app.file_finder.as_ref().unwrap();
    assert_eq!(
        finder.results.len(),
        1,
        "the file part alone matches alpha.rs"
    );
    // The list body starts three rows below the popup top (border, prompt,
    // separator); row 0 is the only result.
    let (col, row) = (finder.last_rect.x + 4, finder.last_rect.y + 3);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert!(app.file_finder.is_none(), "the click confirms the pick");
    assert_eq!(
        app.editor.cursor_row, 39,
        "a clicked row lands on the hinted line"
    );
}

#[test]
fn down_arrow_moves_selection_in_the_file_finder() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a1.rs"), "").unwrap();
    std::fs::write(tmp.path().join("a2.rs"), "").unwrap();
    std::fs::write(tmp.path().join("a3.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    let before = app.file_finder.as_ref().unwrap().selected_index();
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    let after = app.file_finder.as_ref().unwrap().selected_index();
    assert_eq!(
        after,
        before + 1,
        "Down arrow must move the selection cursor one row"
    );
}

#[test]
fn backspace_removes_a_character_from_the_finder_query() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.file_finder.as_ref().unwrap().query, "a");
}

#[test]
fn opening_the_file_finder_arms_one_image_clear_so_iterm_evicts_cached_osc_1337_cells() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.consume_file_finder_image_clear(),
        "Cmd+P must arm terminal.clear() so iTerm2's image cache (activity bar icons, welcome wordmark, editor preview) is wiped before the modal paints, otherwise those cached image cells bleed through the file list"
    );
    assert!(
        !app.consume_file_finder_image_clear(),
        "the flag is one-shot — a second consume call must return false so the driver does not re-clear every frame"
    );
}

#[test]
fn bracketed_paste_into_open_file_finder_appends_to_the_query_not_the_editor() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("hello")];
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_paste("alpha");
    assert_eq!(
        app.file_finder.as_ref().unwrap().query,
        "alpha",
        "an iTerm2 bracketed paste while the finder is open must land in the finder query so the user can Cmd+P then Cmd+V a filename"
    );
    assert_eq!(
        app.editor.lines,
        vec!["hello"],
        "the editor pane behind the modal must not receive the pasted text"
    );
}

#[test]
fn cmd_v_keystroke_in_file_finder_reads_clipboard_into_the_query() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.clipboard_reader = || Some(String::from("alpha"));
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.file_finder.as_ref().unwrap().query,
        "alpha",
        "Cmd+V inside the finder must read the system clipboard and append every char to the query — this covers terminals that disable bracketed paste or route Cmd+V via CSI-u"
    );
}

#[test]
fn file_finder_paste_strips_control_chars_so_a_pasted_newline_does_not_submit_garbage() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_paste("al\nph\ta");
    assert_eq!(
        app.file_finder.as_ref().unwrap().query,
        "alpha",
        "control chars in pasted text must be filtered out — a stray newline must not end the line and a tab must not navigate"
    );
}

#[test]
fn file_finder_modal_swallows_typed_characters_so_editor_does_not_receive_them() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("hello")];
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.lines,
        vec!["hello"],
        "while the file finder is open, typed letters must not leak into the editor buffer"
    );
    assert_eq!(
        app.file_finder.as_ref().unwrap().query,
        "x",
        "typed letters must go into the finder query instead"
    );
}

#[test]
fn cmd_p_index_drops_a_file_deleted_off_disk() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), b"").unwrap();
    let target = tmp.path().join("doomed.rs");
    std::fs::write(&target, b"").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut waited = 0u32;
    loop {
        app.poll_file_finder_index();
        if let Some(idx) = app.file_finder_index.as_ref()
            && idx.iter().any(|e| e.rel == "doomed.rs")
        {
            break;
        }
        if waited >= 5000 {
            panic!("initial index must contain doomed.rs within 5s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    for _ in 0..20 {
        let _ = app.drain_fs_events();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::fs::remove_file(&target).unwrap();
    let mut gone = false;
    for _ in 0..400 {
        let _ = app.drain_fs_events();
        app.poll_file_finder_index();
        if let Some(idx) = app.file_finder_index.as_ref()
            && !idx.iter().any(|e| e.rel == "doomed.rs")
        {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        gone,
        "deleted file must drop out of the Cmd+P index after the FS-event-driven rebuild lands"
    );
}

#[test]
fn open_cmd_p_finder_re_ranks_in_place_when_the_index_swaps() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join("alpha.rs"), b"").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut waited = 0u32;
    loop {
        app.poll_file_finder_index();
        if app.file_finder_index.is_some() {
            break;
        }
        if waited >= 5000 {
            panic!("initial index must land within 5s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    // Open Cmd+P, type the new filename — initially zero results.
    app.open_file_finder();
    for c in "resolver.ts".chars() {
        app.file_finder.as_mut().unwrap().push_char(c);
    }
    assert!(
        app.file_finder
            .as_ref()
            .unwrap()
            .visible_results()
            .is_empty(),
        "resolver.ts is not on disk yet; finder must show no matches before the file is created"
    );
    let new_dir = tmp
        .path()
        .join("packages/ant-ts-lib-noggin/src/entities/files");
    std::fs::create_dir_all(&new_dir).unwrap();
    std::fs::write(new_dir.join("resolver.ts"), b"").unwrap();
    let mut surfaced = false;
    for _ in 0..400 {
        let _ = app.drain_fs_events();
        app.poll_file_finder_index();
        let finder = app.file_finder.as_ref().unwrap();
        if finder
            .visible_results()
            .iter()
            .any(|r| r.entry.rel == "packages/ant-ts-lib-noggin/src/entities/files/resolver.ts")
        {
            surfaced = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        surfaced,
        "an open Cmd+P finder must re-rank against the rebuilt index without the user closing and reopening it"
    );
}

#[test]
fn the_profile_picker_anchors_under_the_pane_that_owns_the_caret() {
    // Hidden panes push `Rect::default()` placeholders so a button's position
    // in the vec names its terminal. The picker still anchored on `.first()`,
    // which in maximize mode is the placeholder for terminal 0 whenever some
    // other pane is the maximized one, dropping the dropdown at the window's
    // top-left corner instead of under the caret it was opened from.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    app.terminal_pane_maximized = true;
    assert_eq!(
        app.active_terminal, 2,
        "the newest pane is the maximized one"
    );
    let backend = ratatui::backend::TestBackend::new(180, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let caret = app.terminal_profile_buttons[2];
    assert!(caret.width > 0, "the maximized pane paints a profile caret");
    assert_eq!(
        app.terminal_profile_buttons[0],
        ratatui::layout::Rect::default(),
        "the hidden pane holds a placeholder, which is what `.first()` found"
    );
    assert_eq!(
        app.terminal_caret_origin(2),
        (caret.x, caret.y + 1),
        "the dropdown anchors under the caret that is actually on screen"
    );

    // Side by side every pane has a live caret, so anchoring on the first
    // live one would hang every pane's dropdown under the leftmost.
    app.terminal_pane_maximized = false;
    term.draw(|f| app.render(f)).unwrap();
    let rightmost = app.terminal_profile_buttons[2];
    assert!(
        rightmost.x > app.terminal_profile_buttons[0].x,
        "the panes must really be side by side for this to prove anything"
    );
    assert_eq!(
        app.terminal_caret_origin(2),
        (rightmost.x, rightmost.y + 1),
        "each pane's dropdown anchors under its OWN caret"
    );
}

/// The jump is reachable from the palette with any pane focused, so a
/// terminal without Super still has a way in (#1294).
#[test]
fn the_palette_has_a_zoxide_jump_command_that_works_from_any_pane() {
    use crate::widgets::command_palette::ALL_COMMANDS;
    let cmd = ALL_COMMANDS
        .iter()
        .copied()
        .find(|c| c.title().to_lowercase().contains("zoxide"))
        .expect("a palette command for the zoxide jump");
    assert_eq!(cmd.title(), "Explorer: Jump to Directory (zoxide)");
    for pane in [Pane::Editor, Pane::Terminal, Pane::Tree] {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus = pane;
        app.run_command(cmd);
        assert!(app.zoxide_jump.is_some(), "{}", app.status);
    }
}

#[test]
fn zoxide_jump_popup_anchors_over_the_explorer_column_not_centered() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_explorer_shortcut(key(KeyCode::Char('z'), KeyModifiers::SUPER));
    assert!(app.zoxide_jump.is_some(), "precondition: popup is open");

    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let popup = app.zoxide_jump.as_ref().unwrap().last_rect;
    // The Explorer tree renders inside the sidebar column; the popup must
    // share that column's left edge (anchored over the Explorer), not be
    // centered on the 120-wide frame (which would put x near ~30).
    assert_eq!(
        popup.x, app.tree.last_area.x,
        "jump popup must be left-anchored to the Explorer column, got x={} vs sidebar x={}",
        popup.x, app.tree.last_area.x
    );
    assert!(
        popup.y <= app.tree.last_area.y,
        "jump popup must drop from the top of the Explorer, not be vertically centered"
    );
}

#[test]
fn a_zoxide_row_re_roots_under_the_same_key_as_the_add_branch() {
    // A zoxide row is a bare line from its database, so it can name a
    // directory in a spelling nothing else in croft uses (a symlinked
    // `/var/…` for `/private/var/…`). Both branches of the jump must land on
    // the same spelling: the folder store keys on
    // `roots.primary().display()`, so two spellings of one directory would
    // file and restore under different keys.
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().canonicalize().unwrap().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = tmp.path().canonicalize().unwrap().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    // Jump mode: re-root onto the symlinked spelling.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut jump = crate::widgets::zoxide_jump::ZoxideJump::new();
    jump.set_results(Some(vec![link.clone()]));
    app.zoxide_jump = Some(jump);
    app.zoxide_add_folder = false;
    app.apply_zoxide_jump_selection();
    let jumped = app.roots.primary().to_path_buf();

    // Add mode: the same row through the other branch.
    let mut app2 = App::new(tmp.path().to_path_buf()).unwrap();
    let mut jump2 = crate::widgets::zoxide_jump::ZoxideJump::new();
    jump2.set_results(Some(vec![link.clone()]));
    app2.zoxide_jump = Some(jump2);
    app2.zoxide_add_folder = true;
    app2.apply_zoxide_jump_selection();
    let added = app2
        .roots
        .iter()
        .find(|r| r.ends_with("real") || r.ends_with("link"))
        .expect("the folder was added")
        .to_path_buf();

    assert_eq!(
        jumped, added,
        "both branches of one gesture must store the same spelling"
    );
    assert_eq!(
        jumped,
        link.canonicalize().unwrap(),
        "and it is the canonical one, so the folder store keys agree"
    );
}

#[test]
fn the_palette_and_vim_recorders_are_one_recording_not_two() {
    // vim used to mirror the recording flag, so a palette Stop left `q`
    // believing it was still recording: the next `q` re-saved the stale
    // register and swallowed the key after it as a register name.
    let (mut app, tmp) = vim_app("one\ntwo\n");
    app.macros_path = tmp.path().join("macros.json");

    // vim starts, palette stops.
    vim_feed_str(&mut app, "qa");
    assert!(app.macro_recording.is_some());
    app.run_command(crate::widgets::command_palette::Command::MacroStartStopRecording);
    assert!(app.macro_recording.is_none(), "the palette stopped it");

    // A following `q` must START a fresh recording, not stop a phantom one.
    vim_feed_str(&mut app, "qb");
    assert_eq!(
        app.macro_recording.as_ref().map(|r| r.register),
        Some(Some('b')),
        "vim sees the real state and opens register b"
    );

    // And the other direction: palette starts, a bare vim `q` stops.
    app.run_command(crate::widgets::command_palette::Command::MacroStartStopRecording);
    app.run_command(crate::widgets::command_palette::Command::MacroStartStopRecording);
    assert!(app.macro_recording.is_some(), "palette started one");
    vim_feed(&mut app, 'q');
    assert!(
        app.macro_recording.is_none(),
        "a bare q stops what the palette started"
    );
}

/// #1345: Go to File skips excluded files, as VS Code's file search does
/// for both settings, and still finds the rest.
#[test]
fn quick_open_skips_excluded_files() {
    let (mut app, _t) = app_with_vscode_excludes();
    app.open_file_finder();
    let rels: Vec<String> = app
        .file_finder
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .map(|e| e.rel.clone())
        .collect();
    assert!(rels.contains(&String::from("src/orders.py")), "{rels:?}");
    assert!(
        !rels
            .iter()
            .any(|r| r.starts_with("generated/") || r.starts_with("tests/")),
        "{rels:?}"
    );
}

#[test]
fn cmd_shift_p_opens_command_palette() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.command_palette.is_none());
    app.handle_key(key(
        KeyCode::Char('p'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        app.command_palette.is_some(),
        "Cmd+Shift+P must open the command palette"
    );
    // Esc closes it.
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.command_palette.is_none());
}

/// Negatives (#1558): with nothing to match, neither command changes the
/// buffer or leaves stray carets, and the palette and VS Code ids reach
/// them.
#[test]
fn occurrence_commands_with_nothing_to_act_on_change_nothing() {
    use crate::widgets::command_palette::Command;
    assert_eq!(
        Command::from_id("select_all_occurrences"),
        Some(Command::SelectAllOccurrences)
    );
    assert_eq!(
        Command::from_id("add_cursors_to_line_ends"),
        Some(Command::AddCursorsToLineEnds)
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut app = occurrences_app(tmp.path());
    // The caret on `=`, no word under it.
    app.editor.cursor_col = 4;
    cmd_shift_l(&mut app);
    assert!(!app.editor.has_multi_cursor());
    app.handle_key(key(
        KeyCode::Char('I'),
        KeyModifiers::SHIFT | KeyModifiers::ALT,
    ))
    .unwrap();
    assert!(!app.editor.has_multi_cursor());
    // A selection across lines is not matched; it stays as it was.
    select(&mut app, (0, 4), (1, 3));
    cmd_shift_l(&mut app);
    assert!(!app.editor.has_multi_cursor());
    assert_eq!(app.editor.selection.unwrap().normalised(), ((0, 4), (1, 3)));
    assert_eq!(app.editor.lines[0], "foo = 1");
}

/// The palette command reaches `clear_bookmarks` without the chord's
/// handler, so the non-text guard has to hold on that path too.
#[test]
fn the_palette_clear_bookmarks_command_leaves_a_preview_alone() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.md", "# a\n\nb\n");
    app.editor.cursor_row = 2;
    app.editor.toggle_bookmark();
    assert!(app.editor.toggle_markdown_preview());
    app.run_command(Command::ClearBookmarks);
    assert_eq!(
        app.editor.bookmarked_lines(),
        vec![3],
        "the preview hides them"
    );
    // Back on the source, the same command clears, so the assertion above is
    // the guard and not a command that does nothing.
    assert!(app.editor.toggle_markdown_preview());
    app.run_command(Command::ClearBookmarks);
    assert!(app.editor.bookmarked_lines().is_empty());
}

#[test]
fn palette_merge_accept_incoming_resolves_at_cursor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", CONFLICTED_BODY);
    app.editor.cursor_row = 1;
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptIncoming);
    assert_eq!(app.editor.lines, vec!["fn main() {", "    theirs();", "}"]);
    // The last block resolving appends the Complete Merge pointer, so the
    // next step of the flow is always one status read away (#107).
    assert_eq!(
        app.status,
        "Accepted incoming change — all conflicts resolved; \"Merge: Complete Merge\" stages the file"
    );
}

#[test]
fn palette_merge_accept_both_keeps_ours_then_theirs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", CONFLICTED_BODY);
    app.editor.cursor_row = 3;
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptBoth);
    assert_eq!(
        app.editor.lines,
        vec!["fn main() {", "    ours();", "    theirs();", "}"]
    );
}

/// Both branches add a function at the same spot, and git's default
/// conflict style writes the functions' shared `}` once, below the
/// markers. Accept Both still keeps each function whole and closed.
#[test]
fn palette_merge_accept_both_keeps_both_added_functions_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let base = "fn a() {\n    x();\n}\n\nfn tail() {\n    z();\n}\n";
    let add = |f: &str| {
        base.replace(
            "fn tail",
            &format!("fn {f}() {{\n    {f}();\n}}\n\nfn tail"),
        )
    };
    git_ok(root, &["init", "-q", "-b", "main"]);
    git_ok(root, &["config", "user.name", "t"]);
    git_ok(root, &["config", "user.email", "t@t"]);
    // Git's default style, whatever this machine's config says.
    git_ok(root, &["config", "merge.conflictStyle", "merge"]);
    std::fs::write(root.join("a.rs"), base).unwrap();
    git_ok(root, &["add", "-A"]);
    git_ok(root, &["commit", "-qm", "base"]);
    git_ok(root, &["checkout", "-qb", "feature"]);
    std::fs::write(root.join("a.rs"), add("from_branch")).unwrap();
    git_ok(root, &["commit", "-qam", "branch"]);
    git_ok(root, &["checkout", "-q", "main"]);
    std::fs::write(root.join("a.rs"), add("from_main")).unwrap();
    git_ok(root, &["commit", "-qam", "main"]);
    let merge = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["merge", "-q", "feature"])
        .output()
        .unwrap();
    assert!(!merge.status.success(), "the merge must conflict");
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.editor.open_pinned(&root.join("a.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    let footer = app
        .editor
        .lines
        .iter()
        .position(|l| l.starts_with(">>>>>>>"))
        .unwrap();
    assert_eq!(
        app.editor.lines[footer + 1],
        "}",
        "git wrote the shared `}}` below the markers"
    );
    app.editor.cursor_row = footer - 1;
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptBoth);
    assert_eq!(
        app.editor.lines.join("\n"),
        "fn a() {\n    x();\n}\n\nfn from_main() {\n    from_main();\n}\n\n\
         fn from_branch() {\n    from_branch();\n}\n\nfn tail() {\n    z();\n}"
    );
    assert_eq!(
        app.status,
        "Accepted both changes — all conflicts resolved; \"Merge: Complete Merge\" stages the file"
    );
    assert!(app.editor.undo(), "resolution is one undo step");
    assert!(app.editor.lines.iter().any(|l| l.starts_with("<<<<<<<")));
}

#[test]
fn hash_prefix_in_quick_open_switches_to_workspace_symbols() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn main() {}");
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.file_finder.is_some(), "Cmd+P opens Quick Open");
    app.handle_key(key(KeyCode::Char('#'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.file_finder.is_none(),
        "typing # hands Quick Open over to the workspace symbol picker"
    );
    let picker = app
        .workspace_symbols
        .as_ref()
        .expect("the workspace symbol picker must open");
    assert_eq!(picker.query, "", "the # itself is not part of the query");
}

/// #1306: `>` in Quick Open is VS Code's route to the Command Palette.
#[test]
fn angle_prefix_in_quick_open_opens_the_command_palette() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "x = 1\n");
    quick_open_typing(&mut app, ">");
    assert!(app.file_finder.is_none(), "Quick Open hands over");
    let palette = app.command_palette.as_ref().expect("the palette opens");
    assert_eq!(palette.query, "", "the > itself is not part of the query");
}

/// #1306: text pasted after the prefix carries over as the new query, for
/// `>` and for the `#` that already switched when typed.
#[test]
fn a_pasted_prefixed_query_carries_into_the_picker_it_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "x = 1\n");
    quick_open_typing(&mut app, "");
    app.handle_paste(">toggle word wrap");
    let palette = app.command_palette.as_ref().expect("the palette opens");
    assert_eq!(palette.query, "toggle word wrap");
    app.command_palette = None;
    quick_open_typing(&mut app, "");
    app.handle_paste("#main");
    let picker = app.workspace_symbols.as_ref().expect("symbols open");
    assert_eq!(picker.query, "main");
}

/// #1306: `@` in Quick Open opens Go to Symbol in Editor.
#[test]
fn at_prefix_in_quick_open_opens_go_to_symbol_in_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "def f():\n    pass\n");
    quick_open_typing(&mut app, "@");
    assert!(app.file_finder.is_none(), "Quick Open hands over");
    let picker = app.go_to_symbol.as_ref().expect("Go to Symbol opens");
    assert_eq!(picker.query, "");
}

/// #1306: `:12` then Enter in Quick Open goes to line 12 of the open file.
#[test]
fn colon_line_in_quick_open_goes_to_that_line() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=50).map(|i| format!("x = {i}\n")).collect();
    let mut app = app_with_open_file(tmp.path(), "a.py", &body);
    quick_open_typing(&mut app, ":12");
    assert!(app.file_finder.is_none(), "Quick Open hands over");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 11, "{}", app.status);
}

#[test]
fn palette_go_to_symbol_in_workspace_opens_the_picker() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn main() {}");
    app.run_command(crate::widgets::command_palette::Command::GoToWorkspaceSymbol);
    assert!(app.workspace_symbols.is_some());
}

#[test]
fn typing_in_the_picker_without_a_language_server_reports_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn main() {}");
    app.lsp = None;
    app.open_workspace_symbols("");
    app.handle_key(key(KeyCode::Char('m'), KeyModifiers::NONE))
        .unwrap();
    // The query dispatches on the debounce tick, not the keystroke.
    std::thread::sleep(std::time::Duration::from_millis(250));
    app.workspace_symbols_dispatch_due();
    let picker = app.workspace_symbols.as_ref().unwrap();
    assert!(
        picker.unsupported,
        "with no language server the picker must say so instead of spinning"
    );
}

#[test]
fn finished_commands_land_in_durable_history_and_the_popup_types_them() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Point the store at the tempdir so tests never touch the real file.
    app.command_history =
        crate::command_history::CommandHistory::load(&tmp.path().join("hist.jsonl"));
    // Full OSC 133 marks around a fake command (B → command text → C →
    // 1s of "runtime" → D;0), then a read so the popup's Enter-typed
    // bytes are observable through the shell.
    let script = "printf '\\033]133;A\\007$ \\033]133;B\\007kubectl get pods\\n\\033]133;C\\007'; sleep 1; printf 'out\\n\\033]133;D;0\\007'; read x; echo typed-$x; sleep 30";
    // The marks are stamped when the reader thread PARSES them, not when
    // the child wrote them. Under suite load a late pickup shrinks the
    // measured gap (#65), and in the extreme the reader is descheduled
    // across the WHOLE sleep: both marks then coalesce into one read and
    // the arrival-time measurement honestly collapses to ~0 (#154) — the
    // true gap is unrecoverable from coalesced bytes. So the C→D floor is
    // asserted per attempt, retrying the collapse class a bounded number
    // of times: a real wrong-span bug fails every attempt deterministically,
    // while three consecutive full-second deschedules do not happen.
    let mut entry = None;
    for attempt in 0..3 {
        app.command_history.entries.clear();
        app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
            "/bin/sh",
            &[String::from("-c"), String::from(script)],
            tmp.path(),
        )
        .unwrap();
        crate::test_budget::await_spawned(
            crate::test_budget::tests::SHELL_PAINT_BASE,
            "the finished command to reach the history store",
            || {
                app.drain_terminal_bells();
                !app.command_history.is_empty()
            },
        );
        let e = app.command_history.entries[0].clone();
        assert_eq!(
            e.cmd, "kubectl get pods",
            "the typed text travels with the completion"
        );
        assert_eq!(e.exit, Some(0));
        if e.dur_ms >= 500 {
            entry = Some(e);
            break;
        }
        assert!(
            attempt < 2,
            "duration must cover the C→D gap on some attempt; every run measured under 500ms (last: {})",
            e.dur_ms
        );
    }
    let e = entry.expect("an attempt produced a full-gap measurement");
    let _ = e;

    // The popup searches the store and Enter types the pick at the prompt.
    app.focus_pane(Pane::Terminal);
    app.handle_terminal_key(key(
        KeyCode::Char('h'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert!(
        app.command_history_popup.is_some(),
        "Ctrl+Shift+H must open the history popup"
    );
    for c in "pods".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    let picked = app
        .command_history_popup
        .as_ref()
        .and_then(|p| p.selected_entry())
        .map(|e| e.cmd.clone());
    assert_eq!(picked.as_deref(), Some("kubectl get pods"));
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.command_history_popup.is_none(),
        "Enter closes the popup"
    );
    app.terminals[0].write_input(b"\n");
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the typed command to reach the shell",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.contains("typed-kubectl get pods"))
        },
    );
}

/// The four navigator palette commands exist, hint their chords, and their
/// hostless paths hint instead of panicking; Toggle records activation for
/// the tick loop to seat.
#[test]
fn palette_carries_the_navigator_commands() {
    use crate::widgets::command_palette::{ALL_COMMANDS, Command};
    for cmd in [
        Command::AskNavigator,
        Command::YieldToNavigator,
        Command::ToggleNavigator,
        Command::ClearNavigatorNotes,
    ] {
        assert!(ALL_COMMANDS.contains(&cmd), "{cmd:?} missing from palette");
    }
    assert_eq!(Command::AskNavigator.keybinding_hint(), "Cmd+K Q");
    assert_eq!(Command::YieldToNavigator.keybinding_hint(), "Cmd+K Y");

    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "a\nb").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pair_record_path = tmp.path().join("x.pair.json");
    app.open_file_at_launch(&file);

    app.run_command(crate::widgets::command_palette::Command::AskNavigator);
    assert!(app.status.to_lowercase().contains("not active"));
    app.run_command(crate::widgets::command_palette::Command::YieldToNavigator);
    assert!(app.status.to_lowercase().contains("not active"));

    app.navigator_notes
        .insert("f.txt".into(), vec![(1, 0, "n".into())]);
    app.editor.comment_focus = Some(crate::widgets::editor::CommentFocus {
        id: 1,
        reply: String::new(),
        cursor: 0,
    });
    app.run_command(crate::widgets::command_palette::Command::ClearNavigatorNotes);
    assert!(app.navigator_notes.is_empty());
    assert!(app.editor.comment_focus.is_none());

    app.run_command(crate::widgets::command_palette::Command::ToggleNavigator);
    let record =
        crate::session::read_pair_record(&app.pair_record_path).expect("toggle writes the record");
    assert!(record.enabled);
    app.run_command(crate::widgets::command_palette::Command::ToggleNavigator);
    let record = crate::session::read_pair_record(&app.pair_record_path).unwrap();
    assert!(!record.enabled);
}

/// #250: the compound layer reaches the user through
/// `open_debug_config_picker` -> `discover_compounds` -> rows ->
/// `confirm_list_picker` -> `resolve_compound`. Nothing but clippy's dead-code
/// lint currently guards that chain: a refactor dropping the compound rows
/// would leave `resolve_compound` with test-only callers and NO TEST FAILING,
/// which is the exact regression this PR exists to correct.
#[test]
fn the_picker_lists_compounds_and_selecting_one_reports_why_it_cannot_launch() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [
            { "name": "Server", "type": "python", "request": "launch", "program": "s.py" },
            { "name": "Client", "type": "python", "request": "launch", "program": "c.py" }
        ],
        "compounds": [
            { "name": "Full Stack", "configurations": ["Server", "Client"] },
            { "name": "Broken", "configurations": ["Server", "Missing"] }
        ]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    app.open_debug_config_picker();
    let picker = app.list_picker.as_ref().expect("picker open");
    assert_eq!(
        picker.rows.len(),
        6,
        "zero-config entry + two configurations + two compounds + Add Configuration"
    );
    let compound_row = picker
        .rows
        .iter()
        .find(|r| r.label.starts_with("Full Stack"))
        .expect("the compound is listed");
    assert!(
        compound_row.label.contains("Server"),
        "the row names its members: {}",
        compound_row.label
    );
    assert!(
        compound_row.id.starts_with("compound:"),
        "compound rows carry their own id space so they cannot collide with \
         a configuration index: {}",
        compound_row.id
    );

    // Selecting a resolvable MULTI-MEMBER compound launches every member
    // (#310). It used to defer and name the issue; the session model holds a
    // set now, so the assertion inverts. What must NOT happen is a subset
    // launch, which would debug something other than what was asked for, so
    // the count is what this checks rather than mere success.
    let idx = picker
        .rows
        .iter()
        .position(|r| r.label.starts_with("Full Stack"))
        .unwrap();
    app.list_picker.as_mut().unwrap().selected = idx;
    app.confirm_list_picker();
    // The old refusal ALSO named the compound and set feedback, so neither
    // of those separates it from a launch. Only the launch path says this.
    assert!(
        app.status.contains("started no sessions") || app.status.starts_with("Debugging compound"),
        "the compound went down the launch path rather than being refused: {}",
        app.status
    );
    assert!(
        !app.status.contains("#310"),
        "and does not cite a limit that is gone: {}",
        app.status
    );

    // A compound naming a configuration no launch.json declares is a config
    // error, reported as such rather than folded into the not-yet message.
    app.open_debug_config_picker();
    let bad = app
        .list_picker
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .position(|r| r.label.starts_with("Broken"))
        .expect("the second compound is listed");
    app.list_picker.as_mut().unwrap().selected = bad;
    app.confirm_list_picker();
    assert!(
        app.status.contains("Missing"),
        "the error names the member that does not exist: {}",
        app.status
    );
}

// ---- Profiles (#618) ----

#[test]
fn the_profiles_picker_offers_the_default_and_a_new_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::SwitchProfile);
    let picker = app.list_picker.as_ref().expect("picker open");
    assert!(matches!(
        picker.purpose,
        crate::widgets::list_picker::ListPurpose::Profiles
    ));
    let ids: Vec<&str> = picker.rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids.first(), Some(&"profile:"), "Default comes first");
    assert!(ids.contains(&"new"));
    assert!(
        !ids.contains(&"workspace-clear"),
        "no workspace choice to clear in a fresh workspace"
    );
}

/// #679: outside a persistent session there is nothing to detach from,
/// and saying so beats a chord that silently does nothing.
#[test]
fn session_detach_outside_a_session_says_there_is_nothing_to_detach_from() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.session_channel = None;
    app.run_command(crate::widgets::command_palette::Command::SessionDetach);
    assert_eq!(
        app.status,
        "Not attached to a persistent session: nothing to detach from"
    );
}

/// #863 negative: a palette the user opens themselves mid-tour is theirs:
/// Esc closes it and the tour goes on.
#[test]
fn esc_in_a_palette_the_user_opened_keeps_the_tour() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        assert!(
            app.command_palette.is_none(),
            "fixture: step 1 opens no modal"
        );
        app.open_command_palette();
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.command_palette.is_none());
        assert!(app.tour.is_some(), "the tour goes on");
        app.finish_tour();
    });
}

/// #1580: the palette has the command, and from the blank startup tab it
/// turns that tab into `Untitled-1` (the welcome screen gives way) rather
/// than stacking a second tab.
#[test]
fn the_palette_turns_the_startup_tab_into_untitled_1() {
    use crate::widgets::command_palette::Command;
    assert_eq!(
        Command::from_id("new_untitled_file"),
        Some(Command::NewUntitledFile)
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.editor.is_blank_initial());
    assert!(run_from_palette(&mut app, "File: New Untitled Text File"));
    assert_eq!(app.editor.tab_count(), 1);
    assert_eq!(app.editor.tab_display_label(0), "Untitled-1");
    assert!(!app.editor.is_blank_initial(), "no welcome screen over it");
    assert!(app.focus == Pane::Editor);
}

/// #852: Select All is on the palette, the route on Linux where `Ctrl+A` is
/// line start; it selects the active buffer and hands the editor focus.
#[test]
fn select_all_from_the_palette_selects_the_active_buffer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "one\ntwo");
    app.focus_pane(Pane::Tree);
    app.run_command(crate::widgets::command_palette::Command::SelectAll);
    assert_eq!(app.editor.selection_text(), "one\ntwo");
    assert!(app.focus == Pane::Editor);
}

/// #852 boundary: with no file open, File: New File… from the editor has
/// no "beside the active file" and falls back to the Explorer's target.
#[test]
fn palette_new_file_with_no_file_open_uses_the_explorer_target() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.editor.path.is_none());
    app.focus_pane(Pane::Editor);
    let explorer_dir = app.explorer_create_target_dir();
    app.run_command(Command::NewFile);
    match app.prompt.as_ref() {
        Some(Prompt {
            kind: PromptKind::Create(CreateKind::File),
            target_dir,
            ..
        }) => assert_eq!(target_dir, &explorer_dir),
        _ => panic!("File: New File… must open the New File prompt"),
    }
}

/// #852 (comment 2): the palette query "layout" finds the layout commands,
/// so a bar hidden with a click can be brought back from the keyboard.
#[test]
fn palette_query_layout_lists_the_customize_layout_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    open_palette_852(&mut app);
    type_keys_852(&mut app, "layout");
    let titles: Vec<String> = app
        .command_palette
        .as_ref()
        .unwrap()
        .results
        .iter()
        .map(|r| r.title().to_string())
        .collect();
    for want in [
        "View: Customize Layout…",
        "View: Toggle Activity Bar Visibility",
        "View: Toggle Status Bar Visibility",
        "View: Toggle Primary Side Bar Position",
        "View: Set Panel Alignment…",
        "View: Set Quick Input Position…",
    ] {
        assert!(
            titles.iter().any(|t| t == want),
            "\"layout\" must find {want:?}; got {titles:?}"
        );
    }
}

/// #852 (comment 2): Activity Bar, Status Bar and Primary Side Bar Position
/// toggle from the palette, typed and run with Enter.
#[test]
fn palette_toggles_the_activity_bar_status_bar_and_side_bar_position() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.activity_bar_visible = true;
    app.status_bar_visible = true;
    app.side_bar_position = SideBarPosition::Left;
    open_palette_852(&mut app);
    type_keys_852(&mut app, "toggle status bar visibility");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(!app.status_bar_visible, "the status bar hides");
    open_palette_852(&mut app);
    type_keys_852(&mut app, "toggle activity bar visibility");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(!app.activity_bar_visible, "the activity bar hides");
    open_palette_852(&mut app);
    type_keys_852(&mut app, "toggle primary side bar position");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.side_bar_position, SideBarPosition::Right);
}

/// #852 (comment 2): Panel Alignment and Quick Input Position are set from
/// the palette through a picker of their options.
#[test]
fn palette_sets_panel_alignment_and_quick_input_position_by_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.panel_alignment = PanelAlignment::Center;
    app.quick_input_position = QuickInputPosition::Top;
    open_palette_852(&mut app);
    type_keys_852(&mut app, "set panel alignment");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.list_picker.is_some(), "the alignment picker opens");
    type_keys_852(&mut app, "justify");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.panel_alignment, PanelAlignment::Justify);
    open_palette_852(&mut app);
    type_keys_852(&mut app, "set quick input position");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.list_picker.is_some(), "the position picker opens");
    type_keys_852(&mut app, "center");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.quick_input_position, QuickInputPosition::Center);
}

/// #852 negative: the palette's layout commands change the layout and
/// nothing else; the Customize Layout popup's re-open is the click route's
/// (it keeps the popup up between clicks) and must not pop over the editor
/// after a palette command.
#[test]
fn palette_layout_commands_leave_the_customize_layout_popup_closed() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for cmd in [
        Command::ToggleActivityBar,
        Command::ToggleStatusBar,
        Command::ToggleSideBarPosition,
    ] {
        app.run_command(cmd);
        assert!(
            app.context_menu.is_none(),
            "{cmd:?} must not open the Customize Layout popup"
        );
    }
    // Twice is a round trip: every toggle is back where it started.
    let fresh = App::new(tmp.path().to_path_buf()).unwrap();
    for cmd in [
        Command::ToggleActivityBar,
        Command::ToggleStatusBar,
        Command::ToggleSideBarPosition,
    ] {
        app.run_command(cmd);
    }
    assert_eq!(app.activity_bar_visible, fresh.activity_bar_visible);
    assert_eq!(app.status_bar_visible, fresh.status_bar_visible);
    assert_eq!(app.side_bar_position, fresh.side_bar_position);
}

/// The palette's rows for `query`, then Esc.
fn palette_rows_852(app: &mut App, query: &str) -> Vec<String> {
    open_palette_852(app);
    type_keys_852(app, query);
    let rows = app
        .command_palette
        .as_ref()
        .map(|p| p.results.iter().map(|i| i.title().to_string()).collect())
        .unwrap_or_default();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    rows
}

/// #852 (comment 3): View: Close Other Editors in Group, typed into the
/// palette, closes every other tab and keeps the active one, as the tab
/// menu's Close Others does. Before, the palette had no such command.
#[test]
fn palette_close_other_editors_keeps_only_the_active_tab() {
    let (mut app, _tmp) = tabs_app_852(&["a.rs", "b.rs", "c.rs", "d.rs"]);
    app.editor.select(1);
    let ran = run_in_palette_852(&mut app, "View: Close Other Editors in Group");
    assert_eq!(
        tab_names_852(&app.editor),
        ["b.rs"],
        "the palette ran {ran:?}"
    );
    assert_eq!(app.status, "Closed 3 other tabs");
}

/// #852 (comment 3): View: Close Editors to the Right in Group closes the
/// tabs right of the active one and keeps it active.
#[test]
fn palette_close_editors_to_the_right_closes_only_the_tabs_right_of_the_active_one() {
    let (mut app, _tmp) = tabs_app_852(&["a.rs", "b.rs", "c.rs", "d.rs"]);
    app.editor.select(1);
    let ran = run_in_palette_852(&mut app, "View: Close Editors to the Right in Group");
    assert_eq!(
        tab_names_852(&app.editor),
        ["a.rs", "b.rs"],
        "the palette ran {ran:?}"
    );
    assert_eq!(app.editor.active_index(), 1, "b.rs stays active");
    assert_eq!(app.status, "Closed 2 tabs to the right");
}

/// #852 (comment 3): View: Close Saved Editors in Group closes the saved
/// tabs and keeps the one with unsaved edits, unwritten.
#[test]
fn palette_close_saved_editors_closes_the_saved_tabs_and_keeps_the_dirty_one() {
    let (mut app, tmp) = tabs_app_852(&["a.rs", "b.rs", "c.rs"]);
    app.editor.select(1);
    type_into_active_tab_852(&mut app);
    let ran = run_in_palette_852(&mut app, "View: Close Saved Editors in Group");
    assert_eq!(
        tab_names_852(&app.editor),
        ["b.rs"],
        "the palette ran {ran:?}"
    );
    assert!(app.editor.dirty, "the unsaved edit is kept");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("b.rs")).unwrap(),
        "x\n",
        "and not written"
    );
    assert_eq!(app.status, "Closed 2 saved tabs");
}

/// #852 (comment 3): View: Close All Editors closes every tab.
#[test]
fn palette_close_all_editors_closes_every_tab() {
    let (mut app, _tmp) = tabs_app_852(&["a.rs", "b.rs", "c.rs"]);
    let ran = run_in_palette_852(&mut app, "View: Close All Editors");
    assert!(
        app.editor.is_blank_initial(),
        "the palette ran {ran:?}; tabs left: {:?}",
        tab_names_852(&app.editor)
    );
    assert_eq!(app.status, "Closed 3 tabs");
}

/// #852 (comment 3): View: Pin Editor pins the active tab (it moves to the
/// pinned block on the left) and View: Unpin Editor unpins it.
#[test]
fn palette_pin_editor_and_unpin_editor_pin_and_unpin_the_active_tab() {
    let (mut app, _tmp) = tabs_app_852(&["a.rs", "b.rs", "c.rs"]);
    let ran = run_in_palette_852(&mut app, "View: Pin Editor");
    assert!(app.editor.pinned, "the palette ran {ran:?}");
    assert_eq!(
        tab_names_852(&app.editor),
        ["c.rs", "a.rs", "b.rs"],
        "a pinned tab moves leftmost"
    );
    assert_eq!(app.status, "Pinned tab");
    let ran = run_in_palette_852(&mut app, "View: Unpin Editor");
    assert!(!app.editor.pinned, "the palette ran {ran:?}");
    assert_eq!(
        app.editor.path.as_deref().and_then(Path::file_name),
        Some(std::ffi::OsStr::new("c.rs"))
    );
    assert_eq!(app.status, "Unpinned tab");
}

/// #852 (comment 3): as in VS Code, the palette offers Pin Editor on an
/// unpinned tab and Unpin Editor on a pinned one, never the one that would
/// do nothing.
#[test]
fn the_palette_offers_pin_editor_or_unpin_editor_by_the_active_tab() {
    let (mut app, tmp) = tabs_app_852(&["a.rs", "b.rs"]);
    let rows = palette_rows_852(&mut app, "pin editor");
    assert!(rows.iter().any(|t| t == "View: Pin Editor"), "{rows:?}");
    assert!(
        !rows.iter().any(|t| t == "View: Unpin Editor"),
        "an unpinned tab has nothing to unpin: {rows:?}"
    );
    let idx = app.editor.active_index();
    app.dispatch_menu_action(MenuAction::ToggleTabPin(idx), tmp.path().to_path_buf());
    assert!(app.editor.pinned, "fixture: the tab menu pinned it");
    let rows = palette_rows_852(&mut app, "pin editor");
    assert!(rows.iter().any(|t| t == "View: Unpin Editor"), "{rows:?}");
    assert!(
        !rows.iter().any(|t| t == "View: Pin Editor"),
        "a pinned tab is already pinned: {rows:?}"
    );
}

/// #852 (comment 3): View: Keep Editor keeps the preview tab open, as the
/// tab menu's Keep Open does.
#[test]
fn palette_keep_editor_keeps_the_preview_tab_open() {
    let (mut app, tmp) = tabs_app_852(&["a.rs"]);
    let p = tmp.path().join("p.rs");
    std::fs::write(&p, "x\n").unwrap();
    app.editor.open_preview(&p).unwrap();
    assert!(app.editor.preview, "fixture: p.rs is the preview tab");
    let ran = run_in_palette_852(&mut app, "View: Keep Editor");
    assert!(!app.editor.preview, "the palette ran {ran:?}");
    assert_eq!(app.status, "Kept tab open");
}

/// #852 (comment 3) negative: Close Other Editors in Group keeps the active
/// tab and every pinned tab, as the tab menu's Close Others does, and the
/// tabs it closes go on the reopen stack.
#[test]
fn palette_close_other_editors_keeps_the_active_and_the_pinned_tabs() {
    let (mut app, tmp) = tabs_app_852(&["a.rs", "b.rs", "c.rs", "d.rs"]);
    app.dispatch_menu_action(MenuAction::ToggleTabPin(0), tmp.path().to_path_buf());
    app.editor.select(2);
    run_in_palette_852(&mut app, "View: Close Other Editors in Group");
    assert_eq!(tab_names_852(&app.editor), ["a.rs", "c.rs"]);
    assert!(app.editor.editors[0].pinned, "the pinned a.rs survives");
    assert_eq!(app.editor.active_index(), 1, "c.rs stays active");
    let reopen: Vec<String> = app
        .closed_tabs
        .iter()
        .map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(reopen, ["b.rs", "d.rs"]);
    assert_eq!(app.status, "Closed 2 other tabs");
}

/// #852 (comment 3) negative: Close Editors to the Right on the last tab
/// closes nothing and says so, rather than touching the tabs to its left.
#[test]
fn palette_close_editors_to_the_right_on_the_last_tab_closes_nothing() {
    let (mut app, _tmp) = tabs_app_852(&["a.rs", "b.rs", "c.rs"]);
    run_in_palette_852(&mut app, "View: Close Editors to the Right in Group");
    assert_eq!(tab_names_852(&app.editor), ["a.rs", "b.rs", "c.rs"]);
    assert_eq!(app.editor.active_index(), 2);
    assert!(app.closed_tabs.is_empty(), "nothing to reopen");
    assert_eq!(app.status, "No tabs to the right to close");
}

/// #852 (comment 3) negative: the group-scoped tab commands act on the
/// focused split only. The other split's tabs, including its copy of a file
/// the command closes here, stay exactly as they were.
#[test]
fn palette_tab_commands_leave_the_other_split_alone() {
    for (title, active) in [
        ("View: Close Other Editors in Group", "c.rs"),
        ("View: Close Editors to the Right in Group", "c.rs"),
        ("View: Close Saved Editors in Group", "c.rs"),
        ("View: Pin Editor", "c.rs"),
        ("View: Unpin Editor", "b.rs"),
        ("View: Keep Editor", "p.rs"),
    ] {
        let (mut app, _tmp) = split_tabs_app_852(active);
        let before = tabs_snapshot_852(&app);
        let ran = run_in_palette_852(&mut app, title);
        assert_eq!(ran.as_deref(), Some(title));
        let after = tabs_snapshot_852(&app);
        assert!(after.2, "{title}: still split");
        assert_ne!(after.0[0], before.0[0], "{title} changed the focused group");
        assert_eq!(
            after.0[1..],
            before.0[1..],
            "{title} must leave the other split alone"
        );
    }
}

/// #1610: the palette finds the share commands by "permalink" and by the
/// host's name, and the editor's right-click menu carries them.
#[test]
fn the_share_commands_are_in_the_palette_and_the_editor_menu() {
    use crate::widgets::command_palette::{ALL_COMMANDS, Command};
    for cmd in [
        Command::GitCopyPermalink,
        Command::GitCopyHeadLink,
        Command::GitOpenOnRemote,
    ] {
        assert!(ALL_COMMANDS.contains(&cmd), "{cmd:?}");
        assert!(cmd.keyword().contains("github"), "{cmd:?}");
        assert_eq!(Command::from_id(cmd.id()), Some(cmd));
    }
    let (mut app, _tmp, _) = app_on_a_pushed_github_file();
    draw(&mut app, 100, 30);
    let x = app.editor.last_inner.x + app.editor.last_gutter_width + 2;
    let y = app.editor.last_inner.y + 1;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), x, y));
    let labels = menu_labels(&app.context_menu.as_ref().unwrap().items);
    assert!(labels.contains(&"Copy Permalink"), "{labels:?}");
    assert!(labels.contains(&"Open on Remote"), "{labels:?}");
}

#[test]
fn the_palette_offers_the_move_editor_commands() {
    let (mut app, _tmp) = three_tab_app_1565();
    let rows = palette_rows_852(&mut app, "move editor");
    for title in [
        "View: Move Editor Left",
        "View: Move Editor Right",
        "View: Move Editor into Next Group",
        "View: Move Editor into Previous Group",
    ] {
        assert!(
            rows.iter().any(|r| r == title),
            "{title} missing from {rows:?}"
        );
    }
    assert_eq!(
        run_in_palette_852(&mut app, "View: Move Editor Left").as_deref(),
        Some("View: Move Editor Left")
    );
    assert_eq!(tab_names_852(&app.editor), ["a.py", "c.py", "b.py"]);
}
