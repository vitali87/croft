//! Context menus and tooltips.

use super::*;

/// A context menu longer than the terminal is tall, or on a terminal under
/// 18 columns, renders clipped instead of panicking.
#[test]
fn a_context_menu_bigger_than_the_terminal_renders_without_panicking() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let items: Vec<(String, MenuAction)> = (0..30)
        .map(|i| (format!("item {i}"), MenuAction::Create(CreateKind::File)))
        .collect();
    app.context_menu = Some(ContextMenu::flat((2, 2), items, tmp.path().to_path_buf()));
    for (w, h) in [(80, 12), (12, 30), (8, 3)] {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let r = app.menu_rect().unwrap();
        assert!(r.width <= w && r.height <= h, "{r:?} in {w}x{h}");
    }
}

#[test]
fn tooltip_repaints_over_the_hero_without_hiding_it() {
    // The Run-and-Debug activity-icon hint flips up onto the centered no-repo
    // hero illustration. The illustration must STAY (it's an OSC-1337 image),
    // and the tooltip is re-painted on top of it post-draw. The fix must never
    // clear/hide the hero just because a tooltip overlaps it.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::SourceControl;
    app.cell_pixel = Some((8, 16));
    let backend = ratatui::backend::TestBackend::new(63, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert!(!app.source_control.status.in_repo, "tempdir is not a repo");
    let hero = app.source_control.last_hero_area;
    assert!(hero.width > 0 && hero.height > 0, "hero must be laid out");
    assert_eq!(
        app.active_sidebar_image_rect(),
        Some(hero),
        "the no-repo hero is the active sidebar illustration"
    );

    // A tooltip box squarely over the hero: the hero must keep painting (no
    // clear requested), and the overlapping repaint path must engage.
    app.ui_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new_compact(
        "Run and Debug".into(),
        (hero.x, hero.y),
    ));
    app.ui_tooltip_area = Some(hero);
    app.flush_no_repo_hero_overlay();
    app.flush_tooltip_over_image();
    assert!(
        !app.consume_no_repo_hero_image_clear(),
        "the hero must NOT be hidden/cleared when a tooltip overlaps it"
    );

    // No active illustration in a repo, so nothing to repaint over.
    app.source_control.status.in_repo = true;
    assert_eq!(
        app.active_sidebar_image_rect(),
        None,
        "a repo workspace shows no hero, so there is no image to overlay"
    );
}

/// Right-clicking the editor gutter (glyph margin / line-number column)
/// opens the breakpoint context menu and toggles the breakpoint on the
/// CLICKED line, not the cursor line, mirroring VS Code's glyph-margin
/// menu. The cursor must not move.
#[test]
fn right_click_on_the_gutter_opens_the_breakpoint_menu_and_toggles_the_clicked_line() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("prog.py");
    std::fs::write(&f, "l1\nl2\nl3\nl4\nl5\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    // Glyph-margin column (left of text_x) on the 3rd content row => 1-based
    // line 3. Cursor stays on line 1 throughout.
    let gutter_col = app.editor.last_inner.x;
    let row = app.editor.last_inner.y + 2;
    assert_eq!(app.editor.cursor_row, 0, "precondition: cursor on line 1");

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        gutter_col,
        row,
    ));
    let menu = app
        .context_menu
        .as_ref()
        .expect("a gutter right-click must open a context menu");
    let labels = menu_labels(&menu.items);
    assert!(
        labels.contains(&"Add Breakpoint"),
        "menu must offer Add Breakpoint when none exists; got {labels:?}"
    );
    assert!(
        labels.contains(&"Add Conditional Breakpoint\u{2026}"),
        "menu must offer Add Conditional Breakpoint; got {labels:?}"
    );

    // Select "Add Breakpoint" and commit it through the real menu key path.
    let idx = menu
        .items
        .iter()
        .position(|e| menu_label(e) == "Add Breakpoint")
        .unwrap();
    app.context_menu.as_mut().unwrap().selected = idx;
    app.handle_menu_key(key(KeyCode::Enter, KeyModifiers::NONE));

    assert!(
        app.context_menu.is_none(),
        "committing a menu item must close the menu"
    );
    assert_eq!(
        app.editor.breakpoint_lines(&f),
        vec![3u32],
        "the breakpoint must land on the clicked line (3), not the cursor line (1)"
    );
    assert_eq!(
        app.editor.cursor_row, 0,
        "the gutter click must not move the cursor"
    );

    // Right-clicking the same gutter line now offers Remove Breakpoint.
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        gutter_col,
        row,
    ));
    let labels = menu_labels(&app.context_menu.as_ref().unwrap().items);
    assert!(
        labels.contains(&"Remove Breakpoint"),
        "an existing breakpoint must offer Remove Breakpoint; got {labels:?}"
    );
}

#[test]
fn menu_item_at_handles_clipped_menu_so_clicks_dispatch_the_visible_row() {
    // Repro: with a 5-item menu (Cut, Copy, Rename…, Select for
    // Compare, Delete), if the user right-clicks low enough that
    // the menu must shift up by 1 to fit on screen, a click on the
    // visible "Select for Compare" row used to map to "Rename"
    // because hit-testing used the unclipped rect while rendering
    // used the clipped one.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Frame is 60 wide, 12 tall. Menu has 7 items + 2 borders = 9
    // rows. Origin at y=6 would make the menu run from row 6 to row
    // 14 (off-screen). The renderer (and hit-test) clamp y to
    // 12 - 9 = 3.
    app.last_frame_area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let f = tmp.path().join("file.txt");
    std::fs::write(&f, "x").unwrap();
    let n = crate::widgets::file_tree::Node {
        path: f.clone(),
        depth: 1,
        is_dir: false,
        expanded: false,
        loaded: true,
    };
    let target = f.parent().unwrap().to_path_buf();
    let items = build_tree_context_menu_items(
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&f),
        &target,
        None,
        None,
    );
    // Sanity: items[5] is "Select for Compare" after the New
    // File… / New Folder prefix lifted everything down by two.
    assert!(matches!(&items[5].1, MenuAction::SelectForCompare(_)));
    app.context_menu = Some(ContextMenu::flat((10, 6), items, target));
    // The visible "Select for Compare" row sits at clipped.y + 1
    // (top border) + 5 (item index) = 3 + 1 + 5 = 9.
    let idx = app
        .menu_item_at(11, 9)
        .expect("hit must land inside the menu");
    assert_eq!(
        idx, 5,
        "click on visible row 9 must map to item 5 (Select for Compare)"
    );
}

#[test]
fn opening_the_profile_menu_keeps_the_welcome_logo() {
    // Regression guard for the welcome-logo bug: opening the terminal profile
    // dropdown must NOT arm the welcome image clear — clearing it blanked the
    // logo (the reported bug). The menu's own Clear paints over its cells, so
    // the cached logo stays intact behind/around the dropdown.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.welcome.mark_displayed();
    app.open_terminal_profile_picker(0);
    assert!(app.context_menu.is_some(), "the menu opens");
    assert!(
        !app.consume_welcome_image_clear(),
        "opening the profile menu must NOT evict the welcome logo",
    );
}

#[test]
fn an_open_context_menu_hides_the_minimap_raster() {
    // The raster is emitted at Kitty z=0 (above text) AFTER ratatui's diff, on
    // every frame. The minimap's own right-click menu is anchored on the strip,
    // so without a gate the raster paints back over the menu the user just
    // opened. `cb0481b` fixed the same collision for the welcome logo. The gate
    // has to be rect-precise, not "any menu is open": arming the clear latch
    // for a menu elsewhere wipes and repaints the whole screen on the
    // cell-buffer protocols.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.cell_pixel = Some((8, 16));
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
    app.editor.lines = (0..80).map(|i| format!("line {i}")).collect();
    let strip = ratatui::layout::Rect {
        x: 100,
        y: 1,
        width: 6,
        height: 40,
    };
    app.update_minimap_overlay(strip);
    assert!(app.minimap_image_payload().is_some(), "minimap must bake");
    let _ = app.consume_minimap_image_clear();
    app.mark_minimap_image_displayed();

    app.open_minimap_menu(102, 5);
    app.update_minimap_overlay(strip);
    assert!(
        app.minimap_image_payload().is_none(),
        "no re-emit while a menu covers the strip, or it repaints over the menu"
    );
    assert!(
        app.consume_minimap_image_clear(),
        "and the placed image must be evicted, not left on the Kitty layer"
    );

    app.context_menu = None;
    app.update_minimap_overlay(strip);
    assert!(
        app.minimap_image_payload().is_some(),
        "the minimap comes back once the menu closes"
    );

    // A menu is clamped to the frame, not snapped to its right edge, so a
    // right-click far from the strip never reaches it.
    app.mark_minimap_image_displayed();
    app.open_minimap_menu(2, 5);
    app.update_minimap_overlay(strip);
    assert!(
        app.minimap_image_payload().is_some(),
        "a menu nowhere near the strip must leave the minimap alone"
    );
    assert!(
        !app.consume_minimap_image_clear(),
        "and must not arm the screen-wiping clear latch"
    );
}

/// iTerm2 OSC-1337 and Sixel have no image z-layer: the post-frame image
/// blit overwrites whatever text sits in those cells, so an open context
/// menu overlapping the editor preview must suppress the overlay (Kitty
/// instead layers images below text and needs nothing).
#[test]
fn a_context_menu_over_the_editor_image_suppresses_the_blit_on_no_z_protocols() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let img = ratatui::layout::Rect::new(2, 2, 30, 15);
    assert!(
        !app.context_menu_covers_rect(img),
        "no menu open: nothing to suppress"
    );
    app.context_menu = Some(ContextMenu::flat(
        (4, 4),
        vec![(
            String::from("Rename"),
            MenuAction::Rename(tmp.path().into()),
        )],
        tmp.path().into(),
    ));
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::ITerm2;
    assert!(
        app.context_menu_covers_rect(img),
        "an overlapping menu must suppress the image on iTerm2"
    );
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
    assert!(
        !app.context_menu_covers_rect(img),
        "Kitty layers images below text; the menu already wins"
    );
}

#[test]
fn dismissing_a_strips_menu_never_leaves_focus_on_the_strip() {
    // This route was OPENED by round 1's own fix: making a strip
    // right-clickable also made it focusable, because the same line that
    // resolves the pane also focuses it. A folded pane paints no cursor and no
    // focus border, so the user then types into a pane they cannot see.
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
    assert!(
        app.context_menu.is_some(),
        "precondition: the strip right-click opened the pane menu"
    );
    // Dismiss through the PRODUCTION path. Assigning `context_menu = None`
    // under a comment saying "the user presses Esc" made this test assert
    // focus after the RIGHT-CLICK, not after dismissal: the right-click branch
    // calls `focus_pane`, which runs the invariant, so the assertion below was
    // already satisfied before the line pretending to dismiss anything ran. It
    // passed, and it could not have failed for the reason it is named for.
    app.handle_menu_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        app.context_menu.is_none(),
        "Esc must actually close the menu, or the dismissal path is not what \
         this test exercised"
    );
    assert!(
        !app.terminals[app.active_terminal].collapsed,
        "dismissing a strip's menu must not leave focus on the strip"
    );
}

/// #1117 fixture: `a.rs` open and drawn, `name` selected when `select`,
/// then a right-click on the word `value`.
fn editor_menu_1117(select: bool) -> (tempfile::TempDir, App) {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "let name = value;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus_pane(Pane::Editor);
    if select {
        app.editor.cursor_col = 4;
        app.editor.start_selection_at_cursor();
        app.editor.cursor_col = 8;
        app.editor.extend_selection_to_cursor();
    }
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let buf = term.backend().buffer().clone();
    let (x, y) = (0..buf.area.height)
        .find_map(|y| {
            let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            let at = row.find("let name = value;")?;
            Some((row[..at].chars().count() as u16 + 12, y))
        })
        .expect("the line is on screen");
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), x, y));
    (tmp, app)
}

fn run_menu_item_1117(app: &mut App, label: &str) {
    let menu = app.context_menu.take().expect("a context menu");
    let action = menu_action_of(&menu.items, label)
        .unwrap_or_else(|| panic!("no {label}: {:?}", menu_labels(&menu.items)))
        .clone();
    app.dispatch_menu_action(action, menu.target_dir.clone());
}

/// #1117: with text selected, the editor's right-click menu ends with Cut,
/// Copy, Paste and Command Palette, and each does what its key does.
#[test]
fn the_editor_menu_offers_cut_copy_paste_and_the_palette() {
    let (_tmp, mut app) = editor_menu_1117(true);
    let labels: Vec<String> = menu_labels(&app.context_menu.as_ref().unwrap().items)
        .into_iter()
        .map(str::to_string)
        .collect();
    let tail: Vec<&str> = labels
        .iter()
        .rev()
        .take(4)
        .rev()
        .map(String::as_str)
        .collect();
    assert_eq!(
        tail,
        ["Copy", "Paste", "---", "Command Palette"],
        "{labels:?}"
    );
    assert!(labels.contains(&String::from("Cut")), "{labels:?}");
    run_menu_item_1117(&mut app, "Copy");
    assert_eq!(app.status, "Copied 4 chars to clipboard");
    assert_eq!(app.editor.lines[0], "let name = value;");

    let (_tmp, mut app) = editor_menu_1117(true);
    run_menu_item_1117(&mut app, "Cut");
    assert_eq!(app.editor.lines[0], "let  = value;");
    assert_eq!(app.status, "Cut 4 chars to clipboard");

    let (_tmp, mut app) = editor_menu_1117(true);
    app.clipboard_reader = || Some(String::from("label"));
    run_menu_item_1117(&mut app, "Paste");
    assert_eq!(app.editor.lines[0], "let label = value;");

    let (_tmp, mut app) = editor_menu_1117(false);
    run_menu_item_1117(&mut app, "Command Palette");
    assert!(app.command_palette.is_some());
}

/// #1117: in a rendered Markdown preview the menu works on what the reader
/// sees. A drag over the rendered text offers Copy of that text; Cut and
/// Paste, which would edit the hidden source, are never offered, and a
/// selection left in the source brings none of them back.
#[test]
fn the_editor_menu_in_a_markdown_preview_copies_only_the_rendered_selection() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let md = tmp.path().join("doc.md");
    std::fs::write(&md, "# Heading\n\nSome rendered body text here.\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&md).unwrap();
    app.focus_pane(Pane::Editor);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    app.editor.cursor_row = 2;
    app.editor.cursor_col = 0;
    app.editor.start_selection_at_cursor();
    app.editor.cursor_col = 4;
    app.editor.extend_selection_to_cursor();
    app.toggle_markdown_preview();
    assert!(app.editor.markdown_preview.is_some());
    term.draw(|frame| app.render(frame)).unwrap();
    let (area, row, text) = {
        let md = app.editor.markdown_preview.as_ref().unwrap();
        let (i, t) = md
            .rows
            .iter()
            .enumerate()
            .map(|(i, cells)| (i as u16, cells.concat()))
            .find(|(_, r)| r.contains("rendered body"))
            .expect("the body text is rendered");
        (md.last_area, i, t)
    };
    let (x, y) = (area.x + text.find("rendered").unwrap() as u16, area.y + row);
    let clipboard_items = |app: &App| -> Vec<String> {
        menu_labels(&app.context_menu.as_ref().expect("a context menu").items)
            .into_iter()
            .filter(|l| ["Cut", "Copy", "Paste"].contains(l))
            .map(str::to_string)
            .collect()
    };

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), x, y));
    assert!(
        clipboard_items(&app).is_empty(),
        "{:?}",
        clipboard_items(&app)
    );
    app.context_menu = None;

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y));
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), x + 7, y));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), x + 7, y));
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), x, y));
    assert_eq!(clipboard_items(&app), ["Copy"]);
    run_menu_item_1117(&mut app, "Copy");
    assert_eq!(app.status, "Copied 8 chars to clipboard");
    assert_eq!(app.editor.lines[2], "Some rendered body text here.");
}

/// #1117 negative: with nothing selected there is nothing to cut or copy,
/// so only Paste is offered; the symbol actions stay where they were.
#[test]
fn the_editor_menu_without_a_selection_offers_only_paste() {
    let (_tmp, app) = editor_menu_1117(false);
    let labels = menu_labels(&app.context_menu.as_ref().unwrap().items);
    assert_eq!(labels.first(), Some(&"Go to Definition"), "{labels:?}");
    assert!(labels.contains(&"Paste"), "{labels:?}");
    assert!(
        !labels.contains(&"Cut") && !labels.contains(&"Copy"),
        "{labels:?}"
    );
}
