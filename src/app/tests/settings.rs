//! Settings, configuration files, themes, colours and locale.

use super::*;

#[test]
fn popup_gradient_tracks_black_theme() {
    // Popups/menus/tooltips wear the gradient + muted selection only under the
    // Black theme; Croft Dark keeps the legacy bright-blue accent so it stays
    // coherent with that theme's blue focus border.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.theme = crate::theme::Theme::BLACK;
    assert!(app.popup_gradient());
    app.theme = crate::theme::Theme::DARK_BLUE;
    assert!(!app.popup_gradient());
}

#[test]
fn black_theme_context_menu_uses_gradient_border_and_muted_selection() {
    use crate::gradient::{GRAD_TL, POPUP_SEL_BG, rgb_color};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.theme = crate::theme::Theme::BLACK;
    app.context_menu = Some(ContextMenu::flat(
        (10, 10),
        vec![
            (
                String::from("New File"),
                MenuAction::Create(CreateKind::File),
            ),
            (
                String::from("New Folder"),
                MenuAction::Create(CreateKind::Folder),
            ),
        ],
        tmp.path().to_path_buf(),
    ));
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    // The menu's top-left corner carries the gradient's TL colour, not blue.
    assert_eq!(buf[(10, 10)].fg, rgb_color(GRAD_TL));
    // The selected (first) row sits on the muted dark-teal fill.
    assert_eq!(buf[(11, 11)].bg, rgb_color(POPUP_SEL_BG));
}

#[test]
fn black_theme_status_bar_is_near_black_not_navy() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.theme = crate::theme::Theme::BLACK;
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let last_y = buf.area.height - 1;
    let near_black = Color::Rgb(0x18, 0x18, 0x18);
    let navy = Color::Rgb(0x1e, 0x3a, 0x6e);
    assert!(
        (0..buf.area.width).any(|x| buf[(x, last_y)].bg == near_black),
        "status bar should paint the near-black seam under the Black theme"
    );
    assert!(
        !(0..buf.area.width).any(|x| buf[(x, last_y)].bg == navy),
        "status bar must not keep the legacy navy under the Black theme"
    );
}

#[test]
fn paint_gradient_box_skips_when_rect_runs_off_buffer() {
    // The panic that motivated the bounds clip: an 80x25 buffer asked
    // to render a tall recents box that extended past row 25.
    let buf_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    let mut buf = ratatui::buffer::Buffer::empty(buf_area);
    let oversized = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 60,
    };
    // Must not panic; the no-op clip is the contract.
    paint_gradient_box(&mut buf, oversized);
    let off_top_left = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    // Sanity: a fitting rect still draws.
    paint_gradient_box(&mut buf, off_top_left);
    assert_eq!(buf[(0, 0)].symbol(), "\u{256d}");
}

/// The focused pane's box is drawn in HEAVY glyphs and an unfocused one
/// in light, so which pane owns the keyboard reads from weight and not
/// only from colour (#470). Both weights keep the rounded corners: there
/// is no heavy arc glyph, and the corners are the house look every other
/// caller of the gradient box shares.
#[test]
fn the_gradient_box_comes_in_two_weights() {
    use ratatui::buffer::Buffer;
    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 8,
        height: 4,
    };
    let sym = |b: &Buffer, x: u16, y: u16| b[(x, y)].symbol().to_string();

    let mut light = Buffer::empty(area);
    crate::gradient::paint_gradient_box(&mut light, area);
    let mut heavy = Buffer::empty(area);
    crate::gradient::paint_gradient_box_heavy(&mut heavy, area);

    // All four edges, not just two: the bug this was written for replaced
    // one horizontal edge and left the other light, so a test that reads
    // only the top would miss its own inverse.
    for (x, y, what) in [(3u16, 0u16, "top"), (3, 3, "bottom")] {
        assert_eq!(sym(&light, x, y), "\u{2500}", "light {what} edge");
        assert_eq!(sym(&heavy, x, y), "\u{2501}", "heavy {what} edge");
    }
    for (x, y, what) in [(0u16, 1u16, "left"), (7, 1, "right")] {
        assert_eq!(sym(&light, x, y), "\u{2502}", "light {what} edge");
        assert_eq!(sym(&heavy, x, y), "\u{2503}", "heavy {what} edge");
    }
    // The corners are shared, which is what keeps the two boxes the same
    // shape rather than two different visual languages.
    for (x, y) in [(0, 0), (7, 0), (0, 3), (7, 3)] {
        assert_eq!(
            sym(&light, x, y),
            sym(&heavy, x, y),
            "corner ({x},{y}) is the same arc in both weights"
        );
    }
}

#[test]
fn paint_gradient_box_draws_rounded_corners_with_corner_colours() {
    let rect = Rect {
        x: 0,
        y: 0,
        width: 8,
        height: 4,
    };
    let mut buf = ratatui::buffer::Buffer::empty(rect);
    paint_gradient_box(&mut buf, rect);
    assert_eq!(buf[(0, 0)].symbol(), "\u{256d}", "top-left rounded");
    assert_eq!(buf[(7, 0)].symbol(), "\u{256e}", "top-right rounded");
    assert_eq!(buf[(0, 3)].symbol(), "\u{2570}", "bottom-left rounded");
    assert_eq!(buf[(7, 3)].symbol(), "\u{256f}", "bottom-right rounded");
    assert_eq!(buf[(3, 0)].symbol(), "\u{2500}", "top edge horizontal");
    assert_eq!(buf[(0, 1)].symbol(), "\u{2502}", "left edge vertical");
    // Top-left cell carries the GRAD_TL fg colour exactly.
    let tl_fg = buf[(0, 0)].fg;
    assert_eq!(tl_fg, rgb_color(GRAD_TL));
    let tr_fg = buf[(7, 0)].fg;
    assert_eq!(tr_fg, rgb_color(GRAD_TR));
}

#[test]
fn set_session_bg_srgb_seq_uses_iterm_osc1337_and_srgb_prefix() {
    // iTerm2 must read the bg as sRGB so the OSC-1337 PNG canvas (also
    // sRGB) matches pixel-for-pixel. Locking in the exact bytes catches
    // any future refactor that drops the `srgb:` prefix or the iTerm2
    // OSC introducer.
    let seq = set_session_bg_srgb_seq(crate::theme::Theme::DARK_BLUE.editor_bg_rgb());
    assert_eq!(seq, "\x1b]1337;SetColors=bg=srgb:1e222e\x07");
}

/// Opening a config file must not reload the active tab in place: it held
/// unsaved edits to another file, which were silently discarded.
#[test]
fn opening_a_config_file_keeps_the_active_tabs_unsaved_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    std::fs::write(&a, "saved\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.editor.lines = vec![String::from("unsaved")];
    app.editor.dirty = true;
    let layer = tmp.path().join(".croft").join("config.json");
    std::fs::create_dir_all(layer.parent().unwrap()).unwrap();
    app.open_config_file_in_editor(layer.clone(), ConfigFileSeed::SettingsLayer);
    assert_eq!(app.editor.path.as_deref(), Some(layer.as_path()));
    let kept = app
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(a.as_path()))
        .expect("a.txt still has its tab");
    assert!(kept.dirty);
    assert_eq!(kept.lines, vec![String::from("unsaved")]);
}

/// #1617: Open Settings (JSON) on a fresh install creates a `config.json`
/// with no settings in it. Seeding every default made each one a user
/// setting, over an `extends` base and over later default changes.
#[test]
fn open_settings_json_seeds_a_config_with_no_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let path = tmp.path().join("user").join("config.json");
    app.open_config_file_in_editor(path.clone(), ConfigFileSeed::Settings);
    assert_eq!(app.editor.path.as_deref(), Some(path.as_path()));
    let written = std::fs::read_to_string(&path).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&crate::tasks::strip_jsonc(&written)).unwrap();
    assert_eq!(doc, serde_json::json!({}), "{written}");
    assert!(written.contains("docs/SETTINGS.md"), "{written}");
    assert_eq!(
        crate::prefs::Prefs::load(&path).unwrap(),
        crate::prefs::Prefs::default()
    );
}

/// #1617 negative: an existing `config.json` opens as it is.
#[test]
fn open_settings_json_leaves_an_existing_config_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let path = tmp.path().join("config.json");
    std::fs::write(&path, "{ \"vim_mode\": true }\n").unwrap();
    app.open_config_file_in_editor(path.clone(), ConfigFileSeed::Settings);
    assert_eq!(app.editor.path.as_deref(), Some(path.as_path()));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{ \"vim_mode\": true }\n"
    );
}

#[test]
fn settings_gear_is_bottom_anchored_below_the_view_icons() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(dummy_activity_images());
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let gear = app.sidebar_areas.settings_icon;
    let run_debug = app.sidebar_areas.run_debug_icon;
    assert!(gear.width > 0, "gear must lay out on a tall bar");
    assert!(
        gear.y > run_debug.y,
        "the gear is anchored at the bottom, below the Run-and-Debug view icon"
    );
    assert_eq!(
        app.pending_activity_image_overlays().len(),
        12,
        "eight view icons + the settings gear + the three layout toolbar icons emit"
    );
}

#[test]
fn settings_gear_bottom_inset_matches_the_view_icon_top_inset() {
    // The view icons start one cell below the bar's top edge
    // (`activity_explorer_y` = `bar.y + 1`). The gear must leave the same
    // one-cell gap above the status bar instead of hugging the bottom row,
    // so the bar reads as vertically balanced rather than bottom-heavy.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(dummy_activity_images());
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let explorer = app.sidebar_areas.explorer_icon;
    let gear = app.sidebar_areas.settings_icon;
    assert!(gear.width > 0, "gear must lay out on a tall bar");
    // The activity bar spans every row except the single status-bar row.
    let bar_bottom = app.last_frame_area.height - 1 - 1;
    let top_inset = explorer.y - app.last_frame_area.y;
    let bottom_inset = bar_bottom - (gear.y + gear.height - 1);
    assert_eq!(
        bottom_inset, top_inset,
        "the gap below the gear must equal the gap above the first view icon"
    );
}

#[test]
fn clicking_the_gear_opens_the_color_theme_menu_then_the_picker() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.activity.set_images(dummy_activity_images());
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let gear = app.sidebar_areas.settings_icon;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
        column: gear.x,
        row: gear.y,
        modifiers: KeyModifiers::NONE,
    });
    let menu = app.context_menu.as_ref().expect("gear opens a menu");
    assert!(
        matches!(
            menu.items.as_slice(),
            [
                MenuEntry::Item {
                    action: MenuAction::OpenThemePicker,
                    ..
                },
                MenuEntry::Item {
                    action: MenuAction::OpenCustomizeLayout,
                    ..
                }
            ]
        ),
        "the gear menu holds Color Theme then Customize Layout"
    );
    // Selecting Color Theme replaces the menu with the theme picker.
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let picker = app.context_menu.as_ref().expect("picker opens");
    assert_eq!(picker.items.len(), crate::theme::Theme::all().len());
    assert!(
        picker
            .items
            .iter()
            .all(|e| matches!(e.action(), Some(MenuAction::SetTheme(_)))),
        "every picker row switches a theme"
    );
}

#[test]
fn applying_a_theme_switches_the_active_theme_and_arms_a_repaint() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert_eq!(
        app.theme,
        crate::theme::Theme::BLACK,
        "fresh install defaults to the black theme"
    );
    app.apply_theme(crate::theme::Theme::DARK_BLUE);
    assert_eq!(app.theme, crate::theme::Theme::DARK_BLUE);
    // A theme switch arms a one-shot clear so iTerm2 evicts the stale-bg
    // image layer and the whole screen repaints on the new background.
    assert!(app.consume_activity_image_clear());
}

/// Issue #207: the light theme must recolor the HOST terminal too. The
/// portable OSC 10/11 pair carries the theme's fg/bg to Ghostty and every
/// other host (iTerm2 additionally gets its sRGB override), and the
/// panic/exit restore must hand the user's own colors back.
#[test]
fn host_dynamic_colors_follow_the_theme() {
    let light = crate::theme::Theme::from_id("light");
    let seq = super::host_colors_seq(light);
    assert!(
        seq.contains("\x1b]11;rgb:ff/ff/ff"),
        "light background must reach the host via OSC 11"
    );
    assert!(
        seq.contains("\x1b]10;rgb:3b/3b/3b"),
        "light foreground must reach the host via OSC 10"
    );
    let black = super::host_colors_seq(crate::theme::Theme::BLACK);
    assert!(black.contains("\x1b]11;rgb:00/00/00"));
    assert!(
        String::from_utf8_lossy(super::TERMINAL_RESTORE_SEQ).contains("\x1b]110\x07\x1b]111\x07"),
        "the panic/exit restore must reset the host's own colors"
    );
}

/// #1286: Settings has an Editor: Vim Mode row that shows and flips it.
#[test]
fn the_settings_picker_has_a_vim_mode_row() {
    let (mut app, _t) = vim_app("abc");
    app.vim.enabled = false;
    app.open_settings_view();
    let picker = app.list_picker.as_mut().expect("settings open");
    let row = picker
        .rows
        .iter()
        .position(|r| r.label.starts_with("Editor: Vim Mode: off"))
        .expect("a Vim Mode row");
    picker.selected = row;
    app.confirm_list_picker();
    assert!(app.vim.enabled, "{}", app.status);
}

/// #1286 negative: a settings reload that leaves `vim_mode` as it was
/// (a theme change, say) does not reset the mode the user is in.
#[test]
fn a_settings_reload_keeps_the_current_vim_mode() {
    let (mut app, _t) = vim_app("abc");
    app.vim.enabled = false;
    let on = crate::prefs::Prefs {
        vim_mode: true,
        ..Default::default()
    };
    app.apply_merged_settings(&on);
    vim_feed(&mut app, 'i');
    assert_eq!(app.vim.mode(), crate::vim::VimMode::Insert);
    app.apply_merged_settings(&on);
    assert!(app.vim.enabled);
    assert_eq!(app.vim.mode(), crate::vim::VimMode::Insert);
}

/// #1479: the issue's repo. `.vscode/settings.json` hides `build`, meaning
/// the root's output folder; `scripts/build/release.py` is source.
fn app_excluding_vscode_build() -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for dir in [".vscode", "build", "scripts/build", "src"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(
        root.join(".vscode/settings.json"),
        r#"{ "files.exclude": { "build": true } }"#,
    )
    .unwrap();
    std::fs::write(root.join("build/out.js"), "release_step()\n").unwrap();
    std::fs::write(
        root.join("scripts/build/release.py"),
        "def release_step(): ...\n",
    )
    .unwrap();
    std::fs::write(root.join("src/main.py"), "release_step()\n").unwrap();
    let app = App::new(root.to_path_buf()).unwrap();
    (app, tmp)
}

/// #1479: Search skips the root's `build/` and still finds the definition
/// in `scripts/build/`.
#[test]
fn a_vscode_exclude_of_build_keeps_scripts_build_in_search() {
    let (mut app, t) = app_excluding_vscode_build();
    assert_eq!(
        search_hit_paths(&mut app, t.path(), "release_step"),
        vec![
            String::from("scripts/build/release.py"),
            String::from("src/main.py")
        ]
    );
}

/// #1479: Go to File keeps a nested folder that shares a bare VS Code
/// exclude's name. (`gen` here: Go to File skips every `build` folder on
/// its own, as noise, whatever the settings say.)
#[test]
fn a_vscode_exclude_of_a_bare_name_keeps_nested_folders_in_go_to_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for dir in [".vscode", "gen", "scripts/gen"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(
        root.join(".vscode/settings.json"),
        r#"{ "files.exclude": { "gen": true } }"#,
    )
    .unwrap();
    std::fs::write(root.join("gen/out.py"), "\n").unwrap();
    std::fs::write(root.join("scripts/gen/release.py"), "\n").unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.open_file_finder();
    let rels: Vec<String> = app
        .file_finder
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .map(|e| e.rel.clone())
        .collect();
    assert!(
        rels.contains(&String::from("scripts/gen/release.py")),
        "{rels:?}"
    );
    assert!(!rels.iter().any(|r| r.starts_with("gen/")), "{rels:?}");
}

/// #1479: the Explorer hides the root's `build/` and lists
/// `scripts/build/` once `scripts/` is expanded.
#[test]
fn a_vscode_exclude_of_build_keeps_scripts_build_in_the_explorer() {
    let (mut app, t) = app_excluding_vscode_build();
    let release = t.path().join("scripts/build/release.py");
    assert!(
        app.tree.reveal_path(&release),
        "scripts/build/release.py is listed"
    );
    let listed: Vec<_> = app.tree.nodes.iter().map(|n| n.path.clone()).collect();
    assert!(
        listed.contains(&t.path().join("scripts/build")),
        "{listed:?}"
    );
    assert!(!listed.contains(&t.path().join("build")), "{listed:?}");
}

/// Render once and return (title 'T' cell coords, add-button rect).
fn terminal_header_probe(app: &mut App) -> Rect {
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let area = app.terminal().last_area;
    assert!(
        area.width > 20 && area.height > 2,
        "terminal pane must render"
    );
    app.terminal_add_buttons[0]
}

#[test]
fn black_theme_terminal_buttons_are_teal_icons_not_navy_chips() {
    use crate::gradient::{INNER_ACCENT, rgb_color};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let add = terminal_header_probe(&mut app);
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let plus = (add.x + 1, add.y);
    assert_eq!(buf[plus].symbol(), "+");
    assert_eq!(buf[plus].fg, rgb_color(INNER_ACCENT));
    assert_ne!(
        buf[plus].bg,
        Color::Rgb(0x1e, 0x3a, 0x6e),
        "Black theme must not paint the navy chip behind the + button"
    );
}

#[test]
fn black_theme_terminal_button_hover_paints_teal_pill() {
    use crate::gradient::{POPUP_SEL_BG, rgb_color};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let add = terminal_header_probe(&mut app);
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column: add.x + 1,
        row: add.y,
        modifiers: KeyModifiers::NONE,
    });
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let plus = (add.x + 1, add.y);
    assert_eq!(
        buf[plus].bg,
        rgb_color(POPUP_SEL_BG),
        "hovered button wears the teal pill"
    );
    assert_eq!(buf[plus].fg, Color::White);
}

#[test]
fn dark_blue_theme_terminal_button_keeps_navy_chip() {
    // Croft Dark keeps its coherent all-blue look: navy chip on the pane's
    // add/close buttons, exactly as before the Black-theme restyle. (The pane
    // no longer carries a TERMINAL title — the panel tab strip labels it.)
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.theme = crate::theme::Theme::DARK_BLUE;
    app.sync_focus_flags();
    let add = terminal_header_probe(&mut app);
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let navy = Color::Rgb(0x1e, 0x3a, 0x6e);
    assert_eq!(buf[(add.x + 1, add.y)].bg, navy);
}

#[test]
fn black_theme_paints_no_legacy_navy_or_button_blue_in_any_sidebar_view() {
    // The Black theme replaced the old blue accent with the brand teal, so
    // no frame may keep the legacy navy chips (#1e3a6e) or the VS Code
    // button blue (#0967b8): sidebar titles, remote header buttons, the
    // active editor tab, and the Initialize / Run and Debug buttons all
    // restyle together.
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("probe.txt");
    std::fs::write(&f, "alpha\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    let navy = Color::Rgb(0x1e, 0x3a, 0x6e);
    let button_blue = Color::Rgb(0x09, 0x67, 0xb8);
    for view in [
        SidebarView::Explorer,
        SidebarView::Search,
        SidebarView::SourceControl,
        SidebarView::Remote,
        SidebarView::RunDebug,
    ] {
        app.sidebar_view = view;
        let buf = render_buf(&mut app);
        assert_eq!(
            count_bg(&buf, navy),
            0,
            "legacy navy chip leaked into {view:?} under the Black theme"
        );
        assert_eq!(
            count_bg(&buf, button_blue),
            0,
            "legacy button blue leaked into {view:?} under the Black theme"
        );
    }
}

#[test]
fn black_theme_sidebar_titles_are_chipless_brand_headers() {
    use crate::gradient::{PANEL_TITLE_FG, rgb_color};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let buf = render_buf(&mut app);
    let tree = app.tree.last_area;
    assert_eq!(buf[(tree.x + 2, tree.y)].symbol(), "E");
    assert_eq!(buf[(tree.x + 2, tree.y)].fg, rgb_color(PANEL_TITLE_FG));
    app.sidebar_view = SidebarView::Remote;
    let buf = render_buf(&mut app);
    let remote = app.remote.last_area;
    assert_eq!(buf[(remote.x + 2, remote.y)].symbol(), "R");
    assert_eq!(buf[(remote.x + 2, remote.y)].fg, rgb_color(PANEL_TITLE_FG));
}

/// #1583: a German catalog for the frame's own titles, as a translator
/// would write it.
const CHROME_DE: &str = r#"{
    "EXPLORER": "ERKUNDER",
    "OUTLINE": "GLIEDERUNG",
    "TIMELINE": "ZEITACHSE",
    "SOURCE CONTROL": "QUELLCODEVERWALTUNG",
    "PROBLEMS": "PROBLEME",
    "OUTPUT": "AUSGABE",
    "TERMINAL": "KONSOLE",
    "Command Palette — Esc to close, ↑/↓ to navigate, Enter to run": "Befehlspalette — Esc schließt",
    "UNSAVED CHANGES": "NICHT GESPEICHERT",
    "This tab has unsaved changes:": "Dieser Tab ist nicht gespeichert:",
    "DISCARD CHANGES?": "ÄNDERUNGEN VERWERFEN?",
    "This will overwrite your local changes from HEAD.": "Lokale Änderungen werden überschrieben."
}"#;

/// Render `app` at 140x50 with `catalog` active and return the screen.
fn screen_with_catalog(app: &mut App, catalog: &str) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 50)).unwrap();
    crate::i18n::with_test_catalog(catalog, || term.draw(|f| app.render(f)).unwrap());
    screen_text(&term)
}

/// #1583: the side bar and panel titles were literals drawn without `tr`,
/// so a catalog entry for them changed nothing.
#[test]
fn the_side_bar_and_panel_titles_come_from_the_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    // A file is open, so the editor area is not the welcome page, whose
    // release highlights can name these titles in English.
    std::fs::write(tmp.path().join("a.txt"), "x\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("a.txt")).unwrap();
    for view in [ExplorerView::Outline, ExplorerView::Timeline] {
        if !app.explorer_views.is_visible(view) {
            app.toggle_explorer_view(view);
        }
    }
    let screen = screen_with_catalog(&mut app, CHROME_DE);
    for want in [
        " ERKUNDER ",
        "GLIEDERUNG",
        "ZEITACHSE",
        " PROBLEME ",
        " AUSGABE ",
        " KONSOLE ",
    ] {
        assert!(screen.contains(want), "{want:?} missing:\n{screen}");
    }
    for gone in [" EXPLORER ", " PROBLEMS ", " OUTPUT ", " TERMINAL "] {
        assert!(!screen.contains(gone), "{gone:?} still drawn:\n{screen}");
    }
}

/// #1583: the Source Control view's title.
#[test]
fn the_source_control_title_comes_from_the_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::SourceControl;
    let screen = screen_with_catalog(&mut app, CHROME_DE);
    assert!(screen.contains("QUELLCODEVERWALTUNG"), "{screen}");
    assert!(!screen.contains("SOURCE CONTROL"), "{screen}");
}

/// #1583: the Command Palette's title line.
#[test]
fn the_palette_title_comes_from_the_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_command_palette();
    let screen = screen_with_catalog(&mut app, CHROME_DE);
    assert!(
        screen.contains(" Befehlspalette — Esc schließt "),
        "{screen}"
    );
    assert!(
        !screen.contains("Command Palette — Esc to close"),
        "{screen}"
    );
}

/// #1583: the unsaved-changes and discard prompts' titles and text.
#[test]
fn the_unsaved_and_discard_prompts_come_from_the_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "x = 1\n");
    app.pending_unsaved = Some(UnsavedExit::CloseTab { idx: 0, path: None });
    let screen = screen_with_catalog(&mut app, CHROME_DE);
    assert!(screen.contains(" NICHT GESPEICHERT "), "{screen}");
    assert!(
        screen.contains("Dieser Tab ist nicht gespeichert:"),
        "{screen}"
    );
    assert!(!screen.contains("UNSAVED CHANGES"), "{screen}");

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pending_discard = Some(PendingDiscard {
        rel_path: String::from("a.txt"),
        untracked: false,
        staged: false,
    });
    let screen = screen_with_catalog(&mut app, CHROME_DE);
    assert!(screen.contains(" ÄNDERUNGEN VERWERFEN? "), "{screen}");
    assert!(
        screen.contains("Lokale Änderungen werden überschrieben."),
        "{screen}"
    );
    assert!(!screen.contains("DISCARD CHANGES?"), "{screen}");
}

/// #1583: a panel tab's click target is as wide as its translated label,
/// measured in cells: a CJK label is twice as wide as its char count, and
/// the next tab starts after it rather than on top of it.
#[test]
fn a_translated_panel_tab_is_clickable_across_its_whole_label() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    screen_with_catalog(
        &mut app,
        r#"{"PROBLEMS": "問題", "OUTPUT": "AUSGABE-UND-PROTOKOLL"}"#,
    );
    let problems = app.problems_tab_rect;
    let output = app.output_tab_rect;
    assert_eq!(problems.width, 6, "' 問題 ' is six cells wide");
    assert_eq!(output.x, problems.x + problems.width + 1);
    assert_eq!(output.width, " AUSGABE-UND-PROTOKOLL ".len() as u16);
    left_click(&mut app, output.x + output.width - 2, output.y);
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Output);
}

/// #1583 negative: with no entry for them the titles stay English, and a
/// tab's click target keeps its English width.
#[test]
fn titles_without_a_catalog_entry_stay_english() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let screen = screen_with_catalog(&mut app, r#"{"Move Line Up": "Zeile nach oben"}"#);
    for want in [" EXPLORER ", " PROBLEMS ", " OUTPUT ", " TERMINAL "] {
        assert!(screen.contains(want), "{want:?} missing:\n{screen}");
    }
    assert_eq!(app.output_tab_rect.width, " OUTPUT ".len() as u16);
}

#[test]
fn black_theme_remote_header_buttons_are_near_white_icons() {
    use crate::gradient::{PANEL_TITLE_FG, rgb_color};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::Remote;
    let buf = render_buf(&mut app);
    let add = app.remote.header_add_btn;
    assert!(add.width > 0, "remote header add button must render");
    // The snug pill paints the glyph at the rect origin (the highlight wraps
    // exactly that one cell), so probe there rather than the rect centre.
    // 863cc5c moved header view-action icons off the brand teal to the
    // near-white PANEL_TITLE_FG, matching VS Code's light `icon.foreground`.
    assert_eq!(buf[(add.x, add.y)].fg, rgb_color(PANEL_TITLE_FG));
}

#[test]
fn black_theme_active_editor_tab_wears_the_teal_selection_fill() {
    use crate::gradient::{POPUP_SEL_BG, rgb_color};
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("probe.txt");
    std::fs::write(&f, "alpha\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    let buf = render_buf(&mut app);
    assert!(
        count_bg(&buf, rgb_color(POPUP_SEL_BG)) > 0,
        "active tab must wear the muted teal fill under the Black theme"
    );
}

#[test]
fn black_theme_action_buttons_wear_the_brand_teal_fill() {
    use crate::gradient::{PRIMARY_BTN_BG, rgb_color};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::SourceControl;
    let buf = render_buf(&mut app);
    let init = app.source_control.last_init_repo_button_area;
    assert!(
        init.width > 0,
        "Initialize button must render outside a repo"
    );
    assert_eq!(
        buf[(init.x + init.width / 2, init.y + init.height / 2)].bg,
        rgb_color(PRIMARY_BTN_BG)
    );
    app.sidebar_view = SidebarView::RunDebug;
    let buf = render_buf(&mut app);
    let run = app.run_debug.last_button_area;
    assert!(run.width > 0, "Run and Debug button must render");
    assert_eq!(
        buf[(run.x + run.width / 2, run.y + run.height / 2)].bg,
        rgb_color(PRIMARY_BTN_BG)
    );
}

#[test]
fn dark_blue_theme_keeps_the_legacy_blue_chrome() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("probe.txt");
    std::fs::write(&f, "alpha\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.theme = crate::theme::Theme::DARK_BLUE;
    app.sync_focus_flags();
    app.editor.open_pinned(&f).unwrap();
    let buf = render_buf(&mut app);
    let navy = Color::Rgb(0x1e, 0x3a, 0x6e);
    let tree = app.tree.last_area;
    assert_eq!(
        buf[(tree.x + 2, tree.y)].bg,
        navy,
        "EXPLORER chip stays navy"
    );
    assert!(count_bg(&buf, navy) > 0, "active tab stays navy");
    app.sidebar_view = SidebarView::RunDebug;
    let buf = render_buf(&mut app);
    let run = app.run_debug.last_button_area;
    assert_eq!(
        buf[(run.x + run.width / 2, run.y + run.height / 2)].bg,
        Color::Rgb(0x09, 0x67, 0xb8),
        "Run and Debug button stays VS Code blue under Croft Dark"
    );
}

/// #1440: the other per-language settings follow the tab's language too:
/// format on type, auto-closing pairs, whitespace rendering, inlay hints.
#[test]
fn per_language_editor_settings_follow_the_tab_s_language() {
    use crate::widgets::editor::WhitespaceMode;
    let (_tmp, app) = app_with_vscode_settings(
        r#"{
            "[python]": {
                "editor.formatOnType": true,
                "editor.autoClosingBrackets": "never",
                "editor.renderWhitespace": "all",
                "editor.inlayHints.enabled": "off"
            }
        }"#,
    );
    assert!(app.format_on_type_for("python"));
    assert!(!app.format_on_type_for("c"));
    assert!(!app.auto_close_pairs_for("python"));
    assert!(app.auto_close_pairs_for("c"));
    assert_eq!(app.whitespace_mode_for("python"), WhitespaceMode::All);
    assert_eq!(app.whitespace_mode_for("c"), app.whitespace_mode);
    assert!(!app.inlay_hints_enabled_for("python"));
    assert!(app.inlay_hints_enabled_for("c"));
}

/// #1440: the active tab's editor gets its language's auto-closing pairs
/// and every editor its language's whitespace rendering when drawn.
#[test]
fn the_active_editor_takes_its_language_s_settings() {
    use crate::widgets::editor::WhitespaceMode;
    let (tmp, mut app) = app_with_vscode_settings(
        r#"{ "[python]": { "editor.autoClosingBrackets": "never", "editor.renderWhitespace": "all" } }"#,
    );
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();

    app.editor.open(&tmp.path().join("m.py")).unwrap();
    app.sync_blame();
    term.draw(|frame| app.render(frame)).unwrap();
    assert!(!app.editor.auto_close_pairs);
    assert_eq!(app.editor.whitespace_mode, WhitespaceMode::All);

    app.editor.open(&tmp.path().join("m.c")).unwrap();
    app.sync_blame();
    term.draw(|frame| app.render(frame)).unwrap();
    assert!(app.editor.auto_close_pairs, "C keeps the global on");
    assert_eq!(app.editor.whitespace_mode, app.whitespace_mode);
}

/// #1440: files whose grammar is shared take their own VS Code language
/// id's block: `[javascriptreact]` for `.jsx`, `[jsonc]` for `.jsonc`,
/// `[scss]` for `.scss`, not the `javascript`/`json`/`css` they highlight as.
#[test]
fn a_language_block_applies_to_files_that_share_another_language_s_grammar() {
    let (tmp, mut app) = app_with_vscode_settings(
        r#"{
            "[javascriptreact]": { "editor.formatOnSave": true },
            "[jsonc]": { "editor.formatOnSave": true },
            "[scss]": { "editor.formatOnSave": true }
        }"#,
    );
    for name in ["a.jsx", "a.jsonc", "a.scss"] {
        std::fs::write(tmp.path().join(name), "x\n").unwrap();
        app.editor.open(&tmp.path().join(name)).unwrap();
        assert!(
            app.format_on_save_eligible(true),
            "{name} takes its own block"
        );
    }
    // Negative: the plain-grammar files keep the global off.
    for name in ["a.js", "a.json", "a.css"] {
        std::fs::write(tmp.path().join(name), "x\n").unwrap();
        app.editor.open(&tmp.path().join(name)).unwrap();
        assert!(
            !app.format_on_save_eligible(true),
            "{name} keeps the global"
        );
    }
}

/// #1574: a workspace's `files_associations` open `card.tpl` as HTML, and
/// again after the tab is closed and the file reopened.
#[test]
fn an_associated_template_opens_in_its_language_every_time() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/config.json"),
        r#"{ "files_associations": { "*.tpl": "html" } }"#,
    )
    .unwrap();
    let mut app = app_with_open_file(tmp.path(), "card.tpl", "<h1>{{ title }}</h1>\n");
    assert_eq!(
        app.editor.language(),
        Some(crate::highlight::LangKind::Html)
    );
    assert_eq!(app.editor.language_label(), "HTML");
    app.editor.close_active();
    app.editor
        .open_pinned(&tmp.path().join("card.tpl"))
        .unwrap();
    assert_eq!(
        app.editor.language(),
        Some(crate::highlight::LangKind::Html)
    );
}

/// #1574: a settings change re-languages the open tabs croft chose a
/// language for, and leaves one whose language was picked by hand.
#[test]
fn a_new_association_relanguages_open_tabs_but_not_a_hand_picked_one() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.tpl", "<b>x</b>\n");
    assert_eq!(app.editor.language(), None, "no association yet");
    std::fs::write(tmp.path().join("b.tpl"), "<i>y</i>\n").unwrap();
    app.editor.open_pinned(&tmp.path().join("b.tpl")).unwrap();
    app.editor
        .pick_language(Some(crate::highlight::LangKind::Xml));
    let mut prefs = crate::prefs::Prefs::default();
    prefs
        .files_associations
        .insert(String::from("*.tpl"), String::from("html"));
    app.apply_file_associations(&prefs);
    let langs: Vec<_> = app
        .editor
        .editors
        .iter()
        .map(|e| (e.path.clone().unwrap(), e.language()))
        .collect();
    assert!(
        langs.contains(&(
            tmp.path().join("a.tpl"),
            Some(crate::highlight::LangKind::Html)
        )),
        "{langs:?}"
    );
    assert!(
        langs.contains(&(
            tmp.path().join("b.tpl"),
            Some(crate::highlight::LangKind::Xml)
        )),
        "{langs:?}"
    );
}

/// #1574: a language picked by hand stays picked even when it is the one
/// the file's name gave it, so a later association does not replace it.
#[test]
fn a_hand_picked_language_matching_the_inferred_one_survives_an_association() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.tpl", "<b>x</b>\n");
    assert_eq!(app.editor.language(), None);
    app.dispatch_menu_action(MenuAction::SetLanguage(None), tmp.path().to_path_buf());
    let mut prefs = crate::prefs::Prefs::default();
    prefs
        .files_associations
        .insert(String::from("*.tpl"), String::from("html"));
    app.apply_file_associations(&prefs);
    assert_eq!(app.editor.language(), None, "Plain Text was picked by hand");
}

/// #1574: an association that moves an open file to another language
/// server closes it on the old one, so the next sync opens it on the new.
#[test]
fn an_association_change_reopens_the_file_on_its_new_server() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn main() {}\n");
    let rs = app.editor.path.clone().unwrap();
    std::fs::write(tmp.path().join("b.go"), "package b\n").unwrap();
    let go = tmp.path().join("b.go");
    app.lsp_last_seen.insert(rs.clone(), 3);
    app.lsp_last_seen.insert(go.clone(), 4);
    let mut prefs = crate::prefs::Prefs::default();
    prefs
        .files_associations
        .insert(String::from("*.rs"), String::from("python"));
    app.apply_file_associations(&prefs);
    assert!(
        !app.lsp_last_seen.contains_key(&rs),
        "rerouted file is resent"
    );
    assert_eq!(app.lsp_last_seen.get(&go), Some(&4), "untouched file stays");
}

/// #1574: a folder added to the workspace gets the associations, and one
/// removed loses them, though the settings themselves did not change.
#[test]
fn associations_follow_workspace_folders_in_and_out() {
    let tmp = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    let extra_root = extra.path().canonicalize().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "x\n");
    let mut prefs = crate::prefs::Prefs::default();
    prefs
        .files_associations
        .insert(String::from("*.tpl"), String::from("html"));
    app.apply_file_associations(&prefs);
    let card = extra_root.join("card.tpl");
    assert_eq!(crate::highlight::associated_language_id(&card), None);
    app.add_workspace_folder(extra_root.clone());
    assert_eq!(
        crate::highlight::associated_language_id(&card).as_deref(),
        Some("html")
    );
    app.remove_workspace_folder(extra_root.clone());
    assert_eq!(crate::highlight::associated_language_id(&card), None);
}

/// #1574 negative: with no association, an unknown extension stays Plain
/// Text and a known one keeps its language.
#[test]
fn without_an_association_files_keep_the_built_in_languages() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app_with_open_file(tmp.path(), "card.tpl", "<h1>x</h1>\n");
    assert_eq!(app.editor.language(), None);
    let app = app_with_open_file(tmp.path(), "main.rs", "fn main() {}\n");
    assert_eq!(
        app.editor.language(),
        Some(crate::highlight::LangKind::Rust)
    );
}

#[test]
fn settings_view_toggle_flips_the_setting_and_stays_open() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let before = app.auto_close_pairs;
    app.open_settings_view();
    // The first row is "Auto Closing Pairs"; confirming it flips the value.
    app.confirm_list_picker();
    assert_eq!(
        app.auto_close_pairs, !before,
        "the toggle row flips the setting"
    );
    assert!(
        app.list_picker.is_some(),
        "a toggle keeps the settings hub open so more can be flipped"
    );
}

#[test]
fn workspace_settings_layer_applies_at_startup_and_reloads_live() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/config.json"),
        r#"{"format_on_save":true,"disable_indent_guides":true}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // The workspace layer is later in the chain than any user layer, and
    // test builds read no user layer at all (`config_layers::user_layers_dir`).
    assert!(app.format_on_save, "committed workspace setting applies");
    assert!(!app.indent_guides_enabled);
    assert_eq!(
        crate::config_layers::layer_of(&app.settings_provenance, "format_on_save"),
        crate::config_layers::LayerKind::Workspace
    );
    // Saving an edited layer file re-merges and applies live, no relaunch.
    std::fs::write(
        tmp.path().join(".croft/config.json"),
        r#"{"format_on_save":false,"disable_indent_guides":true}"#,
    )
    .unwrap();
    let layer = crate::config_layers::workspace_config_path(tmp.path());
    app.reload_config_for_path(&layer);
    assert!(!app.format_on_save, "the edit applied without a restart");
    assert!(app.status.contains("Settings reloaded"), "{}", app.status);
}

#[test]
fn settings_hub_shows_provenance_and_the_layer_rows() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/config.json"),
        r#"{"format_on_save":true}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_settings_view();
    let rows = app.list_picker.as_ref().expect("hub open").rows.clone();
    let fos = rows
        .iter()
        .find(|r| r.id == "toggle:format_on_save")
        .unwrap();
    assert!(
        fos.label.contains("· workspace"),
        "a workspace-won value says so: {}",
        fos.label
    );
    for id in [
        "cmd:open_settings_json",
        "cmd:open_user_settings_local_json",
        "cmd:open_workspace_settings_json",
        "cmd:open_workspace_settings_local_json",
    ] {
        assert!(rows.iter().any(|r| r.id == id), "missing hub row {id}");
    }
}

#[test]
fn opening_workspace_local_settings_gitignores_it_first() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_workspace_settings(true);
    let gi = std::fs::read_to_string(tmp.path().join(".croft/.gitignore")).unwrap();
    assert!(gi.contains("config.local.json"), "{gi}");
    let local = crate::config_layers::workspace_local_config_path(tmp.path());
    assert_eq!(app.editor.path.as_deref(), Some(local.as_path()));
    let seeded = std::fs::read_to_string(&local).unwrap();
    assert!(seeded.contains("settings overlay"), "{seeded}");
}

#[test]
fn a_settings_reload_applies_format_on_type_live() {
    // The live-reload path (save a config file inside croft →
    // remerge_settings → apply_merged_settings) must carry the key like
    // every other allowlisted preference; startup-only application would
    // leave the Settings row showing a provenance whose value is not in
    // force until the next launch.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(!app.format_on_type, "off by default");
    let p = crate::prefs::Prefs {
        format_on_type: true,
        ..Default::default()
    };
    app.apply_merged_settings(&p);
    assert!(app.format_on_type, "a reloaded merge turns it on");
    app.apply_merged_settings(&crate::prefs::Prefs::default());
    assert!(!app.format_on_type, "and back off");
}

#[test]
fn a_workspace_layer_setting_disable_log_highlight_applies_on_remerge() {
    // #466: `disable_log_highlight` is a workspace-allowed key, so a repo's
    // `.croft/config.json` can set it, and a settings remerge (a save of
    // that file) must apply it live: to the field, to the default for logs
    // opened later, and to every open log view. Read at startup too.
    let _exclusive = crate::log_view::DEFAULT_HIGHLIGHT_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = crate::log_view::default_highlight();
    let tmp = tempfile::tempdir().unwrap();
    let layer = crate::config_layers::workspace_config_path(tmp.path());
    std::fs::create_dir_all(layer.parent().unwrap()).unwrap();
    std::fs::write(&layer, r#"{"disable_log_highlight": true}"#).unwrap();
    let log = tmp.path().join("build.log");
    std::fs::write(&log, b"\x1b[32mok\x1b[0m 2024-01-02 step 1\nplain step 2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(!app.log_highlight, "the workspace layer is read at startup");
    app.editor.open_pinned(&log).unwrap();
    assert!(
        app.editor.log.is_some(),
        "a coloured .log opens as a rendered log"
    );
    // Force the open view on, as a stale default would have left it, so the
    // remerge below has something visible to fix.
    app.editor.log.as_mut().unwrap().set_highlight(true);

    // A second log parked in another split group, so the fan-out over
    // inactive groups is exercised, not just the active tab.
    let other = tmp.path().join("other.log");
    std::fs::write(&other, b"\x1b[31merr\x1b[0m 2024-01-02 step 9\n").unwrap();
    app.split_editor();
    app.editor.open_pinned(&other).unwrap();
    assert!(app.editor.log.is_some());
    app.editor.log.as_mut().unwrap().set_highlight(true);
    fn parked_log_flags(app: &mut App) -> Vec<bool> {
        app.editor_layout
            .inactive_groups_mut()
            .into_iter()
            .flat_map(|g| g.editors.iter())
            .filter_map(|e| e.log.as_ref().map(|l| l.highlight()))
            .collect()
    }

    std::fs::write(&layer, r#"{"disable_log_highlight": false}"#).unwrap();
    app.reload_config_for_path(&layer);
    assert!(
        app.log_highlight,
        "the remerge applied the workspace layer's new value"
    );
    assert!(
        app.editor.log.as_ref().unwrap().highlight(),
        "and pushed it into the open log view"
    );
    let parked = parked_log_flags(&mut app);
    assert!(
        !parked.is_empty() && parked.iter().all(|&h| h),
        "and into the log parked in the other split group: {parked:?}"
    );
    assert!(
        crate::log_view::default_highlight(),
        "and into the default for later logs"
    );

    std::fs::write(&layer, r#"{"disable_log_highlight": true}"#).unwrap();
    app.reload_config_for_path(&layer);
    assert!(!app.log_highlight);
    assert!(
        !app.editor.log.as_ref().unwrap().highlight(),
        "the opt-out reaches the open view"
    );
    assert!(
        parked_log_flags(&mut app).iter().all(|&h| !h),
        "and the parked one"
    );
    crate::log_view::set_default_highlight(before);
}

#[test]
fn the_offer_switches_apply_on_a_settings_remerge() {
    // The two prefs re-apply on a config save like every other boolean pref,
    // and switching the offer off takes down one already on screen.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // The refusal memory is read from the real cache directory at startup;
    // this machine's must not decide the test.
    app.remote_offer_refused.clear();
    let pane = app.terminals[0].uid();
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(app.ssh_offer.is_some());
    let mut prefs = crate::prefs::Prefs {
        disable_remote_offer: true,
        remote_offer_excluded_hosts: vec![String::from("jump")],
        ..Default::default()
    };
    app.apply_merged_settings(&prefs);
    assert!(app.remote_offer_disabled && app.remote_offer_excluded == vec![String::from("jump")]);
    assert!(
        app.ssh_offer.is_none(),
        "the live offer went down with the switch"
    );
    prefs.disable_remote_offer = false;
    app.apply_merged_settings(&prefs);
    assert!(!app.remote_offer_disabled);
    // An offer on screen for a host that just joined the exclusion list
    // goes down too, not only on the global switch.
    app.consider_ssh_offer(pane, None);
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(app.ssh_offer.is_some());
    prefs.remote_offer_excluded_hosts = vec![String::from("db-1")];
    app.apply_merged_settings(&prefs);
    assert!(
        app.ssh_offer.is_none(),
        "the newly excluded host's offer went down"
    );
}

/// One check of the config watch after `t0`, past its interval.
fn tick_after(app: &mut App, t0: std::time::Instant, n: u32) -> bool {
    app.tick_config_watch_at(t0 + crate::config_sync::ConfigWatch::INTERVAL * n)
}

/// #1435: `.croft/config.json` written from croft's own terminal (or by a
/// `git pull`) applies within the watch interval, the way a save from
/// inside croft does, and only once.
#[test]
fn a_workspace_settings_file_written_outside_croft_applies_live() {
    use crate::widgets::editor::WhitespaceMode;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert_eq!(app.whitespace_mode, WhitespaceMode::Selection);
    let t0 = std::time::Instant::now();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/config.json"),
        r#"{ "render_whitespace": "all" }"#,
    )
    .unwrap();
    assert!(tick_after(&mut app, t0, 2));
    assert_eq!(app.whitespace_mode, WhitespaceMode::All);
    assert_eq!(app.editor.whitespace_mode, WhitespaceMode::All);
    assert_eq!(app.status, "Settings reloaded");
    assert!(!tick_after(&mut app, t0, 4), "applied once");
}

/// #1435: an `extends` target is part of the chain too, watched even
/// before it exists, so creating it applies its settings.
#[test]
fn an_extends_target_written_outside_croft_applies_live() {
    use crate::widgets::editor::WhitespaceMode;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/config.json"),
        r#"{ "extends": "team.json" }"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let t0 = std::time::Instant::now();
    std::fs::write(
        tmp.path().join(".croft/team.json"),
        r#"{ "render_whitespace": "none" }"#,
    )
    .unwrap();
    assert!(tick_after(&mut app, t0, 2));
    assert_eq!(app.whitespace_mode, WhitespaceMode::None);
    assert_eq!(app.status, "Settings reloaded");
}

/// #1435: a settings file saved from inside croft reloads on the save, and
/// the watch does not reload it a second time.
#[test]
fn a_settings_file_saved_in_croft_reloads_once() {
    use crate::widgets::editor::WhitespaceMode;
    let tmp = tempfile::tempdir().unwrap();
    let layer = crate::config_layers::workspace_config_path(tmp.path());
    std::fs::create_dir_all(layer.parent().unwrap()).unwrap();
    std::fs::write(&layer, "{}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let t0 = std::time::Instant::now();
    app.editor.open(&layer).unwrap();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 1;
    app.editor.insert_str(r#" "render_whitespace": "all" "#);
    app.save();
    assert_eq!(app.whitespace_mode, WhitespaceMode::All);
    assert_eq!(app.status, "Settings reloaded");
    app.status = String::from("after the save");
    assert!(!tick_after(&mut app, t0, 2), "the save already applied it");
    assert_eq!(app.status, "after the save");
}

/// #1435: the Settings editor writes its layer and applies it itself, so
/// the watch leaves that write alone and the editor's own message stays.
#[test]
fn a_settings_editor_write_is_not_reloaded_again_by_the_watch() {
    use crate::widgets::editor::WhitespaceMode;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let t0 = std::time::Instant::now();
    app.open_settings_editor();
    app.settings_editor.as_mut().unwrap().target = crate::config_layers::LayerKind::Workspace;
    app.write_setting("render_whitespace", serde_json::json!("all"));
    assert_eq!(app.whitespace_mode, WhitespaceMode::All);
    let saved = app.status.clone();
    assert!(saved.starts_with("render_whitespace saved to"), "{saved}");
    assert!(!tick_after(&mut app, t0, 2));
    assert_eq!(app.status, saved);
}

/// #1435: after a re-root the new root's settings are watched and the old
/// root's are not: editing the repo croft has left changes nothing.
#[test]
fn after_a_re_root_only_the_new_roots_settings_are_watched() {
    use crate::widgets::editor::WhitespaceMode;
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    for root in [old.path(), new.path()] {
        std::fs::create_dir_all(root.join(".croft")).unwrap();
    }
    let mut app = App::new(old.path().to_path_buf()).unwrap();
    app.change_workspace_root(new.path().to_path_buf());
    let old_layer = crate::config_layers::workspace_config_path(old.path());
    let new_layer = crate::config_layers::workspace_config_path(new.path());
    assert!(app.config_watch.watches(&new_layer));
    assert!(!app.config_watch.watches(&old_layer));
    let t0 = std::time::Instant::now();
    std::fs::write(&old_layer, r#"{ "render_whitespace": "all" }"#).unwrap();
    assert!(!tick_after(&mut app, t0, 2), "the old root is not watched");
    assert_eq!(app.whitespace_mode, WhitespaceMode::Selection);
    std::fs::write(&new_layer, r#"{ "render_whitespace": "all" }"#).unwrap();
    assert!(tick_after(&mut app, t0, 4));
    assert_eq!(app.whitespace_mode, WhitespaceMode::All);
}

/// #1435: the user's own settings layers are watched from startup, the
/// files croft already watched still are, and a file that is no settings
/// layer is not.
#[test]
fn the_config_watch_covers_every_settings_layer_and_only_those() {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    for layer in &app.settings_chain {
        assert!(app.config_watch.watches(layer), "{}", layer.display());
    }
    assert!(app.config_watch.watches(&crate::keymap::keybindings_path()));
    assert!(
        app.config_watch
            .watches(&crate::config_layers::synced_config_path())
    );
    assert!(!app.config_watch.watches(&tmp.path().join("notes.json")));
    assert!(
        !app.config_watch
            .watches(&tmp.path().join(".croft/tasks.json"))
    );
}

#[test]
fn sync_config_now_runs_the_cli_in_a_pane_for_a_plain_host_only() {
    // #262: the palette command types `croft sync-config <host>` into a
    // new terminal, so the host must be an alias, never shell text.
    use crate::widgets::command_palette::Command;
    assert_eq!(
        Command::from_id("remote_sync_config_now"),
        Some(Command::RemoteSyncConfigNow)
    );
    assert_eq!(
        Command::RemoteSyncConfigNow.title(),
        "Remote: Sync Config Now"
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for bad in ["dev box", "x;rm -rf ~", "-oProxyCommand=x", ""] {
        app.sync_config_to(bad);
        assert!(
            app.status.contains("not an ssh host alias"),
            "{bad:?}: {}",
            app.status
        );
    }
    app.sync_config_to("user@dev-box.lan");
    assert_eq!(app.status, "Syncing config to user@dev-box.lan");
}

/// #262: a `config.synced.json` that arrives by config sync is applied on
/// arrival: it is in the settings chain, so the config watch's reload of it
/// re-runs the merge.
#[test]
fn the_synced_settings_layer_is_in_the_reload_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    let synced = crate::config_layers::synced_config_path();
    assert!(
        app.settings_chain.contains(&synced),
        "{synced:?} in {:?}",
        app.settings_chain
    );
}

#[test]
fn the_multiline_paste_warning_follows_its_setting() {
    use crate::prefs::MultilinePasteWarning;
    let (mut app, _tmp) = plain_paste_app();
    app.multiline_paste_warning = MultilinePasteWarning::Never;
    app.handle_paste(TWO_LINES);
    assert_eq!(pasted(&app), TWO_LINES, "never: straight in");
    let (mut app, _tmp) = plain_paste_app();
    app.multiline_paste_warning = MultilinePasteWarning::Always;
    app.terminals[0].feed_bytes_for_test(b"\x1b[?2004h");
    app.handle_paste(TWO_LINES);
    assert!(
        app.pending_terminal_paste.is_some(),
        "always: asks even when bracketed"
    );
    assert_eq!(pasted(&app), "");
}

#[test]
fn the_multiline_paste_warning_setting_parses() {
    use crate::prefs::{MultilinePasteWarning, Prefs};
    let p: Prefs = serde_json::from_str(r#"{"terminal_multiline_paste_warning":"never"}"#).unwrap();
    assert_eq!(
        p.terminal_multiline_paste_warning,
        MultilinePasteWarning::Never
    );
    let p: Prefs = serde_json::from_str("{}").unwrap();
    assert_eq!(
        p.terminal_multiline_paste_warning,
        MultilinePasteWarning::Auto
    );
}

/// #1433: the bytes croft writes for one frame, through the real crossterm
/// backend, at `depth`.
fn frame_bytes_at(depth: crate::color_depth::ColorDepth) -> String {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.py"), "print(1)\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&tmp.path().join("a.py")).unwrap();
    app.color_depth = depth;
    struct Sink(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let bytes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let backend = ratatui::backend::CrosstermBackend::new(Sink(bytes.clone()));
    let mut term = ratatui::Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 100, 30)),
        },
    )
    .unwrap();
    term.draw(|f| app.render_for_terminal(f)).unwrap();
    drop(term);
    String::from_utf8_lossy(&bytes.borrow()).into_owned()
}

/// #1433: on a 256-colour terminal the frame carries no 24-bit colour
/// sequence, only palette indexes.
#[test]
fn a_256_colour_terminal_gets_no_24_bit_colour() {
    let out = frame_bytes_at(crate::color_depth::ColorDepth::Ansi256);
    assert_eq!(out.matches(";2;").count(), 0, "{out:?}");
    assert!(out.contains("48;5;"), "background indexes are written");
}

/// #1433: the Linux console gets the 16 ANSI colours only (crossterm
/// writes them as palette entries 0-15).
#[test]
fn a_16_colour_terminal_gets_only_the_16_ansi_colours() {
    let out = frame_bytes_at(crate::color_depth::ColorDepth::Ansi16);
    assert_eq!(out.matches(";2;").count(), 0, "{out:?}");
    let indexes: Vec<u32> = out
        .split("8;5;")
        .skip(1)
        .map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().unwrap()
        })
        .collect();
    assert!(!indexes.is_empty());
    assert!(indexes.iter().all(|&i| i < 16), "{indexes:?}");
}

/// #1433 negative: a truecolor terminal still gets the theme's exact
/// 24-bit colours.
#[test]
fn a_truecolor_terminal_keeps_24_bit_colour() {
    let out = frame_bytes_at(crate::color_depth::ColorDepth::TrueColor);
    assert!(out.matches(";2;").count() > 50, "{out:?}");
}

/// An App over `tmp` whose user config lives in `cfg`, never the real one.
fn settings_editor_app(cfg: &std::path::Path, tmp: &std::path::Path) -> App {
    let mut app = App::new(tmp.to_path_buf()).unwrap();
    app.config_dir = cfg.to_path_buf();
    app.run_command(crate::widgets::command_palette::Command::OpenSettingsEditor);
    assert!(app.settings_editor.is_some(), "the editor opens");
    app
}

fn type_query(app: &mut App, q: &str) {
    for c in q.chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
}

#[test]
fn the_settings_editor_flips_a_setting_into_the_user_layer_and_applies_it() {
    // #612: search a setting, Enter edits it in place, the file and the live
    // session both change, and the row names the layer that set it.
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.json"),
        "{\n  \"auto_save\": false\n}\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/config.json"),
        "{ \"copy_on_select\": true }",
    )
    .unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    let copy = app
        .settings_editor
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .find(|r| r.key == "copy_on_select")
        .cloned()
        .unwrap();
    assert_eq!(copy.layer, crate::config_layers::LayerKind::Workspace);
    type_query(&mut app, "auto save");
    let row = app
        .settings_editor
        .as_ref()
        .unwrap()
        .selected_row()
        .cloned()
        .unwrap();
    assert_eq!(row.key, "auto_save");
    assert_eq!(row.value, serde_json::json!(false));
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.path().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written["auto_save"], serde_json::json!(true));
    assert!(app.auto_save, "applied to the live session");
    let row = app
        .settings_editor
        .as_ref()
        .unwrap()
        .selected_row()
        .cloned()
        .unwrap();
    assert_eq!(row.value, serde_json::json!(true));
    assert_eq!(row.layer, crate::config_layers::LayerKind::User);
}

/// #1655: the user `config.json` the Settings editor test app reads.
fn user_config(cfg: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(cfg.join("config.json")).unwrap()).unwrap()
}

/// #1655: the selected Settings editor row.
fn selected_setting(app: &App) -> crate::settings_editor::Row {
    app.settings_editor
        .as_ref()
        .unwrap()
        .selected_row()
        .cloned()
        .unwrap()
}

/// #1655: Del removes a toggled setting from `config.json`, where toggling it
/// back only wrote the default value explicitly, and the row returns to
/// `default`. Other keys in the file stay.
#[test]
fn del_resets_a_toggled_setting_and_drops_its_key() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.json"),
        "{\n  \"auto_save_on_focus_change\": false,\n  \"copy_on_select\": true\n}\n",
    )
    .unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    type_query(&mut app, "auto save on focus change");
    assert_eq!(
        selected_setting(&app).layer,
        crate::config_layers::LayerKind::User
    );
    app.handle_key(key(KeyCode::Delete, KeyModifiers::NONE))
        .unwrap();
    let written = user_config(cfg.path());
    assert!(
        written.get("auto_save_on_focus_change").is_none(),
        "{written}"
    );
    assert_eq!(written["copy_on_select"], serde_json::json!(true));
    let row = selected_setting(&app);
    assert_eq!(row.key, "auto_save_on_focus_change");
    assert_eq!(row.layer, crate::config_layers::LayerKind::Default);
    assert!(app.status.contains("reset"), "{}", app.status);
}

/// #1655: a number setting, which Enter could only overwrite, resets too,
/// and the live session takes the default again.
#[test]
fn del_resets_a_number_setting_to_its_default() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.json"),
        "{ \"terminal_scrollback\": 5000, \"auto_save\": true }",
    )
    .unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    type_query(&mut app, "terminal scrollback");
    app.handle_key(key(KeyCode::Delete, KeyModifiers::NONE))
        .unwrap();
    let written = user_config(cfg.path());
    assert!(written.get("terminal_scrollback").is_none(), "{written}");
    let default = serde_json::to_value(crate::prefs::Prefs::default()).unwrap();
    assert_eq!(selected_setting(&app).value, default["terminal_scrollback"]);
    assert!(app.auto_save, "the other key still applies");
}

/// #1655: resetting the user value of a key the workspace also sets leaves
/// the workspace value in force, with the row naming the workspace.
#[test]
fn resetting_the_user_value_leaves_the_workspace_one_in_force() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.json"),
        "{ \"copy_on_select\": false }",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/config.json"),
        "{ \"copy_on_select\": true }",
    )
    .unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    type_query(&mut app, "copy on select");
    app.handle_key(key(KeyCode::Delete, KeyModifiers::NONE))
        .unwrap();
    assert!(user_config(cfg.path()).get("copy_on_select").is_none());
    let row = selected_setting(&app);
    assert_eq!(row.layer, crate::config_layers::LayerKind::Workspace);
    assert_eq!(row.value, serde_json::json!(true));
    let ws = std::fs::read_to_string(tmp.path().join(".croft/config.json")).unwrap();
    assert!(
        ws.contains("copy_on_select"),
        "the workspace layer is untouched: {ws}"
    );
}

/// #1655 negative: Del on a setting the target layer does not set writes
/// nothing (no file is created, another layer's file is untouched) and says
/// so, rather than failing silently.
#[test]
fn del_on_a_setting_the_layer_does_not_set_writes_nothing() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    type_query(&mut app, "copy on select");
    app.handle_key(key(KeyCode::Delete, KeyModifiers::NONE))
        .unwrap();
    assert!(!cfg.path().join("config.json").exists());
    assert!(app.status.contains("not set"), "{}", app.status);

    std::fs::write(
        cfg.path().join("config.json"),
        "{ \"copy_on_select\": true }",
    )
    .unwrap();
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Delete, KeyModifiers::NONE))
        .unwrap();
    assert!(
        !tmp.path().join(".croft/config.json").exists(),
        "the workspace layer has nothing to reset"
    );
    assert_eq!(
        user_config(cfg.path())["copy_on_select"],
        serde_json::json!(true),
        "and the user layer is not the target"
    );
}

/// #1655: a row whose value differs from croft's default carries a dot,
/// whichever layer set it, and the footer names Del reset. A value set
/// explicitly to its default carries none (negative).
#[test]
fn the_settings_editor_marks_changed_rows_and_shows_del_reset() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.json"),
        "{ \"auto_save\": true, \"auto_save_on_focus_change\": false }",
    )
    .unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    type_query(&mut app, "auto save");
    let buf = render_buf(&mut app);
    let lines: Vec<String> = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect();
    let line_of = |needle: &str| {
        lines
            .iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not drawn:\n{}", lines.join("\n")))
            .clone()
    };
    assert!(line_of("Esc close").contains("Del reset"));
    let changed = lines
        .iter()
        .find(|l| l.contains("auto save ") && !l.contains("focus") && !l.contains("> auto"))
        .cloned()
        .unwrap_or_default();
    assert!(changed.contains("\u{2022} auto save "), "{changed}");
    let same = line_of("auto save on focus change");
    assert!(!same.contains('\u{2022}'), "{same}");
}

#[test]
fn the_settings_editor_writes_the_workspace_layer_only_for_allowed_keys() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.settings_editor.as_ref().unwrap().target,
        crate::config_layers::LayerKind::Workspace
    );
    type_query(&mut app, "format on save");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let ws: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".croft/config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ws["format_on_save"], serde_json::json!(true));
    assert!(
        !cfg.path().join("config.json").exists(),
        "the user layer is untouched"
    );
    // A key only the user may set is refused for the workspace.
    for _ in 0.."format on save".len() {
        app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
    }
    type_query(&mut app, "sidebar auto hide");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.status.contains("user"), "{}", app.status);
    let ws = std::fs::read_to_string(tmp.path().join(".croft/config.json")).unwrap();
    assert!(!ws.contains("sidebar_auto_hide"), "{ws}");
}

#[test]
fn the_settings_editor_asks_for_a_number_and_refuses_one_that_is_not() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    type_query(&mut app, "terminal scrollback");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(matches!(
        app.input_prompt.as_ref().map(|p| &p.purpose),
        Some(crate::widgets::input_prompt::InputPurpose::SettingValue { key }) if key == "terminal_scrollback"
    ));
    app.close_input_prompt();
    app.submit_setting_value("terminal_scrollback", "lots");
    assert!(app.status.contains("number"), "{}", app.status);
    assert!(!cfg.path().join("config.json").exists());
    app.submit_setting_value("terminal_scrollback", " 5000 ");
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.path().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written["terminal_scrollback"], serde_json::json!(5000));
}

fn written_settings(cfg: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(cfg.join("config.json")).unwrap()).unwrap()
}

/// #1555: Enter on the unset UI language picks a language in place; it used
/// to close the editor and open `config.json` at line 1.
#[test]
fn enter_on_the_unset_language_picks_one_in_the_editor() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    type_query(&mut app, "locale");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.settings_editor.is_some(), "still in the editor");
    let lang = written_settings(cfg.path())["locale"].clone();
    assert!(lang.is_string(), "a language was set: {lang}");
    let row = app
        .settings_editor
        .as_ref()
        .unwrap()
        .selected_row()
        .cloned()
        .unwrap();
    assert_eq!(row.value, lang, "and the row shows it");
}

/// #1555: an unset number setting asks for a number, saves it as one, and
/// an empty entry unsets it again.
#[test]
fn an_unset_number_setting_takes_a_number_and_an_empty_entry_unsets_it() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    type_query(&mut app, "terminal rewind");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(matches!(
        app.input_prompt.as_ref().map(|p| &p.purpose),
        Some(crate::widgets::input_prompt::InputPurpose::SettingValue { key }) if key == "terminal_rewind_mb"
    ));
    assert_eq!(
        app.input_prompt.as_ref().unwrap().value,
        "",
        "an unset setting's prompt opens empty, not on (default)"
    );
    app.close_input_prompt();
    app.submit_setting_value("terminal_rewind_mb", "500");
    assert_eq!(
        written_settings(cfg.path())["terminal_rewind_mb"],
        serde_json::json!(500)
    );
    app.submit_setting_value("terminal_rewind_mb", "");
    let written = written_settings(cfg.path());
    assert!(
        written.get("terminal_rewind_mb").is_none(),
        "unset, so its default applies: {written}"
    );
}

/// #1555: a value the setting can't hold is refused and nothing is
/// written; `code_scanning` took `enabled`, said it was saved, and
/// ignored it.
#[test]
fn a_value_the_setting_cannot_hold_is_refused() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    app.write_setting("code_scanning", serde_json::json!("enabled"));
    assert!(app.status.contains("can't be enabled"), "{}", app.status);
    assert!(!cfg.path().join("config.json").exists());
    type_query(&mut app, "code scanning");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.input_prompt.is_none(), "a choice: no free text to type");
    assert_eq!(
        written_settings(cfg.path())["code_scanning"],
        serde_json::json!("on")
    );
}

/// #1555: a list setting opens the chosen layer's file with the caret on
/// its key, the workspace's when Tab has chosen it.
#[test]
fn a_list_setting_opens_its_layer_file_at_the_key() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    let ws_file = tmp.path().join(".croft/config.json");
    std::fs::write(
        &ws_file,
        "{\n  \"auto_save\": true,\n  \"files_exclude\": [\"target\"]\n}\n",
    )
    .unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    type_query(&mut app, "files_exclude");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.settings_editor.is_none());
    let opened = app.editor.path.clone().unwrap();
    assert!(
        opened.ends_with(".croft/config.json"),
        "{}",
        opened.display()
    );
    assert_eq!(app.editor.cursor_row, 2, "on the key's line");
    assert!(app.status.contains("files exclude"), "{}", app.status);
}

/// #1555 negative: with the user layer chosen, a list setting opens the
/// user's file, at its key too.
#[test]
fn a_list_setting_in_the_user_layer_opens_the_user_file_at_the_key() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.json"),
        "{\n  \"theme\": \"dark\",\n  \"auto_save\": true,\n  \"search_exclude\": []\n}\n",
    )
    .unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    type_query(&mut app, "search exclude");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let opened = app.editor.path.clone().unwrap();
    assert_eq!(opened.file_name().unwrap(), "config.json");
    assert!(
        opened.starts_with(cfg.path()) || opened.starts_with(cfg.path().canonicalize().unwrap())
    );
    assert_eq!(app.editor.cursor_row, 3);
}

/// #1555: an explicitly empty `review_ai_prefix` (no marker) opens its
/// prompt empty, so confirming it unchanged keeps it empty instead of
/// unsetting it back to the default marker.
#[test]
fn confirming_an_empty_review_prefix_keeps_it_empty() {
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.json"),
        "{\n  \"review_ai_prefix\": \"\"\n}\n",
    )
    .unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    type_query(&mut app, "review_ai_prefix");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let prompt = app.input_prompt.as_ref().expect("a text prompt");
    assert_eq!(prompt.value, "", "opens empty, not on (default)");
    let value = prompt.value.clone();
    app.close_input_prompt();
    app.submit_setting_value("review_ai_prefix", &value);
    assert_eq!(
        written_settings(cfg.path())["review_ai_prefix"],
        serde_json::json!("")
    );
}

#[test]
fn the_settings_editor_draws_its_value_prompt_on_top() {
    // The prompt Enter opens for a number sits over the editor. Drawn under
    // it, the user typed blind.
    let cfg = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = settings_editor_app(cfg.path(), tmp.path());
    type_query(&mut app, "terminal scrollback");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.input_prompt.is_some(), "Enter asks for the number");
    type_query(&mut app, "424242");
    let backend = ratatui::backend::TestBackend::new(120, 36);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(screen.contains("424242"), "the typed value is visible");
}

/// #1576: a project's `.vscode/settings.json` `editor.lineNumbers` sets
/// the gutter; a value croft has no mode for is ignored.
#[test]
fn vscode_line_numbers_setting_applies() {
    use crate::widgets::editor::LineNumbers;
    for (value, want) in [
        ("relative", LineNumbers::Relative),
        ("off", LineNumbers::Off),
        ("sideways", LineNumbers::On),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
        std::fs::write(
            tmp.path().join(".vscode/settings.json"),
            format!(r#"{{ "editor.lineNumbers": "{value}" }}"#),
        )
        .unwrap();
        let app = App::new(tmp.path().to_path_buf()).unwrap();
        assert_eq!(app.line_numbers, want, "{value}");
    }
}
