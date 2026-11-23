//! The pop-ups drawn over the middle of the screen, and the pictures that
//! wait for them (#2659).

use super::*;
use crate::app::popups::Popup;

/// An app set up as iTerm2 with a 400×400 PNG open in the editor.
fn picture_app(root: &Path) -> App {
    let path = root.join("pic.png");
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(400, 400, image::Rgba([10, 200, 30, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    std::fs::write(&path, &png).unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::ITerm2;
    app.cell_pixel = Some((10, 20));
    app.editor.open(&path).unwrap();
    app
}

/// Open `p` with the state its painter draws from. The match has no
/// catch-all, so a pop-up cannot join the list without a way to open it
/// here. An agent's proposal is a real one, sent through the edit hook;
/// the returned stream is the hook's end, kept open while the test runs.
pub(super) fn open(app: &mut App, p: Popup) -> Option<std::os::unix::net::UnixStream> {
    match p {
        Popup::Prompt => {
            app.prompt = Some(Prompt {
                label: String::from("New File"),
                buffer: String::new(),
                caret: usize::MAX,
                kind: PromptKind::Create(CreateKind::File),
                target_dir: app.workspace_root().to_path_buf(),
                error: None,
            });
        }
        Popup::LocalOpenConfirm => {
            app.pending_local_open = Some(String::from("http://localhost:3000"));
        }
        Popup::DiscardConfirm => {
            app.pending_discard = Some(PendingDiscard {
                rel_path: String::from("a.rs"),
                untracked: false,
                staged: false,
            });
        }
        Popup::RevertHunkConfirm => {
            app.pending_revert_hunk = Some(PendingRevertHunk {
                rel_path: String::from("a.rs"),
                patch: String::new(),
            });
        }
        Popup::DiscardAllConfirm => app.pending_discard_all = true,
        Popup::StashDropConfirm => {
            app.pending_stash_drop = Some(PendingStashDrop {
                root: app.workspace_root().to_path_buf(),
                oid: String::from("0123abcd"),
                label: String::from("stash@{0}: WIP on main"),
            });
        }
        Popup::UnsavedScmConfirm => {
            app.pending_unsaved_scm = Some(PendingUnsavedScm {
                op: ScmWrite::Commit,
                root: app.workspace_root().to_path_buf(),
                files: vec![PathBuf::from("a.rs")],
            });
        }
        Popup::ReplaceAllConfirm => app.pending_replace_all = Some((3, 2, 0)),
        Popup::BroadcastConfirm => app.pending_broadcast_enable = true,
        Popup::TerminalPasteConfirm => {
            app.pending_terminal_paste = Some(b"echo a\necho b\n".to_vec());
        }
        Popup::RunBlockConfirm => {
            app.pending_run_block = Some(PendingRunBlock {
                pane_name: String::from("zsh"),
                cwd: app.workspace_root().to_path_buf(),
                command: String::from("ls"),
                code: String::from("ls"),
                destructive: false,
                doc: None,
                block_line: 0,
                persist: false,
                capture_timeout: None,
            });
        }
        Popup::UnsavedConfirm => app.pending_unsaved = Some(UnsavedExit::Quit),
        Popup::LossySaveConfirm => {
            app.pending_lossy_save = Some(LossySave {
                path: None,
                encoding: "windows-1252",
                chars: vec!['\u{1F600}'],
            });
        }
        Popup::FileFinder => {
            app.file_finder = Some(crate::widgets::file_finder::FileFinder::new(
                std::sync::Arc::new(Vec::new()),
            ));
        }
        Popup::CommandPalette => app.open_command_palette(),
        Popup::GoToSymbol => {
            app.go_to_symbol = Some(crate::widgets::symbol_picker::SymbolPicker::new(
                PathBuf::from("a.rs"),
                Vec::new(),
            ));
        }
        Popup::WorkspaceSymbols => {
            app.workspace_symbols = Some(
                crate::widgets::workspace_symbols::WorkspaceSymbolPicker::new(
                    app.workspace_root().to_path_buf(),
                    "",
                ),
            );
        }
        Popup::ProcessPicker => {
            app.process_picker = Some(crate::widgets::process_picker::ProcessPicker::new(
                Vec::new(),
            ));
        }
        Popup::ZoxideJump => {
            app.zoxide_jump = Some(crate::widgets::zoxide_jump::ZoxideJump::new());
        }
        Popup::CommandHistory => {
            app.command_history_popup = Some(crate::widgets::history_popup::HistoryPopup::new());
        }
        Popup::BranchPicker => {
            app.branch_picker = Some(crate::widgets::branch_picker::BranchPicker::new(Vec::new()));
        }
        Popup::SettingsEditor => app.open_settings_editor(),
        Popup::InputPrompt => {
            app.input_prompt = Some(crate::widgets::input_prompt::InputPrompt::new(
                crate::widgets::input_prompt::InputPurpose::GoToLine,
                "Go to Line",
                "line number",
            ));
        }
        Popup::ListPicker => {
            app.list_picker = Some(crate::widgets::list_picker::ListPicker::new(
                crate::widgets::list_picker::ListPurpose::ReviewPullRequest,
                "Review Pull Request",
                vec![crate::widgets::list_picker::ListRow {
                    id: String::from("1"),
                    label: String::from("#1 A change"),
                }],
            ));
        }
        Popup::ShortcutsModal => {
            app.shortcuts_modal = Some(crate::widgets::shortcuts::ShortcutsModal::default());
        }
        Popup::ConnectDialog => {
            app.connect_dialog = Some(crate::widgets::connect_dialog::ConnectDialog::new(
                String::from("devbox"),
            ));
        }
        Popup::Approval => {
            use std::io::Write;
            let root = app.workspace_root().to_path_buf();
            let file = root.join("a.rs");
            std::fs::write(&file, "let x = 1;\n").unwrap();
            let sock = root.join("hook.sock");
            let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
            listener.set_nonblocking(true).unwrap();
            app.hook_listener = Some(listener);
            let mut hook = std::os::unix::net::UnixStream::connect(&sock).unwrap();
            let req = crate::agent_hook::EditRequest {
                agent: "claude-code".into(),
                tool: "Edit".into(),
                input: serde_json::json!({"file_path": file, "old_string": "1", "new_string": "2"}),
                cwd: root,
            };
            writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
            assert!(app.drain_hook_requests(), "the proposal must arrive");
            return Some(hook);
        }
        Popup::TerminalWarning => app.pending_terminal_warning = true,
    }
    None
}

/// Close `p` again, the state [`open`] set taken away.
pub(super) fn close(app: &mut App, p: Popup) {
    match p {
        Popup::Prompt => app.prompt = None,
        Popup::LocalOpenConfirm => app.pending_local_open = None,
        Popup::DiscardConfirm => app.pending_discard = None,
        Popup::RevertHunkConfirm => app.pending_revert_hunk = None,
        Popup::DiscardAllConfirm => app.pending_discard_all = false,
        Popup::StashDropConfirm => app.pending_stash_drop = None,
        Popup::UnsavedScmConfirm => app.pending_unsaved_scm = None,
        Popup::ReplaceAllConfirm => app.pending_replace_all = None,
        Popup::BroadcastConfirm => app.pending_broadcast_enable = false,
        Popup::TerminalPasteConfirm => app.pending_terminal_paste = None,
        Popup::RunBlockConfirm => app.pending_run_block = None,
        Popup::UnsavedConfirm => app.pending_unsaved = None,
        Popup::LossySaveConfirm => app.pending_lossy_save = None,
        Popup::FileFinder => app.file_finder = None,
        Popup::CommandPalette => app.command_palette = None,
        Popup::GoToSymbol => app.go_to_symbol = None,
        Popup::WorkspaceSymbols => app.workspace_symbols = None,
        Popup::ProcessPicker => app.process_picker = None,
        Popup::ZoxideJump => app.zoxide_jump = None,
        Popup::CommandHistory => app.command_history_popup = None,
        Popup::BranchPicker => app.branch_picker = None,
        Popup::SettingsEditor => app.settings_editor = None,
        Popup::InputPrompt => app.input_prompt = None,
        Popup::ListPicker => app.list_picker = None,
        Popup::ShortcutsModal => app.shortcuts_modal = None,
        Popup::ConnectDialog => app.connect_dialog = None,
        Popup::Approval => {
            app.approval_ui = None;
            app.approvals.clear();
        }
        Popup::TerminalWarning => app.pending_terminal_warning = false,
    }
}

/// The bug (#2659): iTerm2 and Sixel have no layer for pictures, so croft
/// sends a picture after drawing the frame, and it wipes whatever it
/// overlaps. Opening any pop-up over a picture must hold the picture back.
/// Before the fix, 16 pop-ups (the approval dialog, every confirm, the file
/// prompt, the connect dialog and Settings) and the startup warning let the
/// picture be sent again over them.
#[test]
fn every_popup_holds_an_editor_picture_back() {
    let mut sent_over = Vec::new();
    for p in Popup::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = picture_app(tmp.path());
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 45)).unwrap();
        let before = term.draw(|f| app.render(f)).unwrap().buffer.clone();
        let l = app
            .editor_image_payload(0)
            .map(|(_, l)| l.clone())
            .expect("the picture must bake with no pop-up open");
        // What the main loop does next: send it, and remember the cells.
        let under = app.image_underlays(&before).editor[0];
        app.mark_editor_image_displayed(0);
        app.overlays.editor[0].mark_sent_over(under);

        let _hook = open(&mut app, p);
        assert!(p.is_open(&app), "{p:?} must count as open");
        let after = term.draw(|f| app.render(f)).unwrap().buffer.clone();
        let covered = (l.cell_y..l.cell_y + l.cell_h)
            .flat_map(|y| (l.cell_x..l.cell_x + l.cell_w).map(move |x| (x, y)))
            .filter(|&xy| before[xy] != after[xy])
            .count();
        assert!(
            covered > 0,
            "{p:?} must paint over the picture, or this test proves nothing for it"
        );
        if app.editor_image_payload(0).is_some() {
            sent_over.push(format!("{p:?} ({covered} cells)"));
        }
    }
    assert!(
        sent_over.is_empty(),
        "these pop-ups cover the picture, and croft would send the picture again over them: {}",
        sent_over.join(", ")
    );
}

/// The terminal, Markdown and minimap pictures, the SSH illustration and the
/// welcome logo wait for every pop-up too: each asks the same question as
/// the editor's. The welcome logo had a list of its own that missed
/// Settings, the confirms, the prompt and an agent's proposal (Codex, #2660).
#[test]
fn every_picture_waits_for_every_popup() {
    fn app_with_pictures(root: &Path) -> App {
        let mut app = App::new(root.to_path_buf()).unwrap();
        app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::ITerm2;
        app.cell_pixel = Some((8, 16));
        app.overlays.terminal_image.set(
            String::from("terminal picture"),
            TerminalImageLayout {
                cell_x: 2,
                cell_y: 30,
                cell_w: 10,
                cell_h: 5,
                seq: 1,
                pane: 0,
            },
        );
        app.overlays.markdown_image.set(
            String::from("markdown picture"),
            MarkdownImageLayout {
                cell_x: 60,
                cell_y: 5,
                cell_w: 10,
                cell_h: 5,
                path: root.join("a.png"),
                mtime_ms: 0,
            },
        );
        app.editor.lines = (0..80).map(|i| format!("line {i}")).collect();
        app.update_minimap_overlay(Rect {
            x: 100,
            y: 1,
            width: 6,
            height: 40,
        });
        app.set_sidebar_view(SidebarView::Remote);
        app.overlays.ssh.set_displayed(true);
        app
    }
    fn held_back(app: &mut App) -> [bool; 4] {
        app.flush_ssh_empty_state_overlay(&ratatui::buffer::Buffer::empty(app.last_frame_area));
        [
            app.terminal_image_payload().is_none(),
            app.markdown_image_payload().is_none(),
            app.minimap_image_payload().is_none(),
            app.consume_ssh_empty_state_image_clear(),
        ]
    }

    /// A blank workspace showing the welcome screen, its logo baked and due.
    fn app_with_welcome_logo(root: &Path) -> App {
        let mut app = App::new(root.to_path_buf()).unwrap();
        app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::ITerm2;
        app.cell_pixel = Some((8, 16));
        app.overlays.welcome.set_layout(WelcomeLayout {
            cell_x: 40,
            cell_y: 4,
            cell_w: 20,
            cell_h: 6,
        });
        app.overlays.welcome.set_image(String::from("welcome logo"));
        app.overlays.welcome.mark_dirty();
        app
    }

    let tmp = tempfile::tempdir().unwrap();
    let welcome = app_with_welcome_logo(tmp.path());
    assert!(
        welcome.welcome_image_emit_payload().is_some(),
        "with no pop-up open the welcome logo is sent"
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_pictures(tmp.path());
    assert!(!app.pictures_held_back());
    assert_eq!(
        held_back(&mut app),
        [false; 4],
        "with no pop-up open every picture shows: terminal, Markdown, minimap, SSH"
    );
    for p in Popup::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_pictures(tmp.path());
        let _hook = open(&mut app, p);
        assert!(app.pictures_held_back(), "{p:?}");
        assert_eq!(
            held_back(&mut app),
            [true; 4],
            "{p:?} must hold back the terminal, Markdown and minimap pictures and the SSH illustration"
        );
        let tmp = tempfile::tempdir().unwrap();
        let mut welcome = app_with_welcome_logo(tmp.path());
        let _hook = open(&mut welcome, p);
        assert!(
            welcome.editor.is_blank_initial() && !welcome.terminal_maximized,
            "{p:?}: opening it must leave the welcome screen up, or this proves nothing"
        );
        assert!(
            welcome.welcome_image_emit_payload().is_none(),
            "{p:?} must hold back the welcome logo"
        );
    }
}

/// Which painter draws each pop-up. No catch-all: a new pop-up names its
/// painter.
fn painter(p: Popup) -> &'static str {
    match p {
        Popup::Prompt => "render_prompt",
        Popup::LocalOpenConfirm => "render_local_open_confirm",
        Popup::DiscardConfirm => "render_discard_confirm",
        Popup::RevertHunkConfirm => "render_revert_hunk_confirm",
        Popup::DiscardAllConfirm => "render_discard_all_confirm",
        Popup::StashDropConfirm => "render_stash_drop_confirm",
        Popup::UnsavedScmConfirm => "render_unsaved_scm_confirm",
        Popup::ReplaceAllConfirm => "render_replace_all_confirm",
        Popup::BroadcastConfirm => "render_broadcast_confirm",
        Popup::TerminalPasteConfirm => "render_terminal_paste_confirm",
        Popup::RunBlockConfirm => "render_run_block_confirm",
        Popup::UnsavedConfirm => "render_unsaved_confirm",
        Popup::LossySaveConfirm => "render_lossy_save_confirm",
        Popup::FileFinder => "render_file_finder",
        Popup::CommandPalette => "render_command_palette",
        Popup::GoToSymbol => "render_go_to_symbol",
        Popup::WorkspaceSymbols => "render_workspace_symbols",
        Popup::ProcessPicker => "render_process_picker",
        Popup::ZoxideJump => "render_zoxide_jump",
        Popup::CommandHistory => "render_command_history_popup",
        Popup::BranchPicker => "render_branch_picker",
        Popup::SettingsEditor => "render_settings_editor",
        Popup::InputPrompt => "render_input_prompt",
        Popup::ListPicker => "render_list_picker",
        Popup::ShortcutsModal => "render_shortcuts_modal",
        Popup::ConnectDialog => "render_connect_dialog",
        Popup::Approval => "render_approval_popup",
        Popup::TerminalWarning => "render_terminal_warning",
    }
}

/// The painters the frame calls for things that open next to what they
/// belong to. Pictures yield to these only where they overlap, so they are
/// not pop-ups in this sense.
const BESIDE_THEIR_OWNER: [&str; 9] = [
    "render_port_toast",
    "render_update_toast",
    "render_zen_hint",
    "render_context_menu",
    "render_commit_dropdown",
    "render_editor_find",
    "render_terminal_find",
    "render_tour_caption",
    "render_scm_menu",
];

/// Every painter the frame calls in its overlay pass draws a listed pop-up
/// or something that opens beside its owner, and every listed pop-up is
/// painted there. A new pop-up painted without joining the list fails here,
/// so pictures cannot be sent over it again (#2659).
#[test]
fn every_painter_in_the_overlay_pass_is_a_listed_popup_or_opens_beside_its_owner() {
    let frame =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/frame.rs"))
            .unwrap();
    let start = frame
        .find("// Overlays render last")
        .expect("the overlay pass starts with its comment");
    let end = start
        + frame[start..]
            .find("self.drop_covered_update_controls(")
            .expect("the overlay pass ends by dropping covered update controls");
    let called: Vec<&str> = frame[start..end]
        .match_indices("self.render_")
        .map(|(i, _)| {
            let name = &frame[start + i + "self.".len()..];
            &name[..name.find('(').unwrap()]
        })
        .collect();
    let listed: Vec<&str> = Popup::ALL.into_iter().map(painter).collect();
    let unknown: Vec<&&str> = called
        .iter()
        .filter(|n| !listed.contains(n) && !BESIDE_THEIR_OWNER.contains(n))
        .collect();
    assert!(
        unknown.is_empty(),
        "add these to `Popup` (src/app/popups.rs) if they open over the middle \
         of the screen, or to BESIDE_THEIR_OWNER if they open beside what they \
         belong to: {unknown:?}"
    );
    let unpainted: Vec<&&str> = listed
        .iter()
        .chain(BESIDE_THEIR_OWNER.iter())
        .filter(|n| !called.contains(n))
        .collect();
    assert!(
        unpainted.is_empty(),
        "listed but not painted in the overlay pass: {unpainted:?}"
    );
}

/// A picture wider than any pane, so it spans the terminal's whole width.
fn wide_png() -> Vec<u8> {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(2400, 300, image::Rgba([0, 0, 255, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

/// #2663: the terminal's Find bar opens at the top right of the pane, over
/// a picture the terminal shows (`imgcat`). On iTerm2 croft sent the
/// picture again over the bar; it now waits while the bar is drawn on it.
#[test]
fn a_terminal_picture_waits_for_the_find_bar_drawn_over_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::ITerm2;
    app.cell_pixel = Some((8, 16));
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 45)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // Clear the pane and print the picture at its third row, so its top
    // rows are where the Find bar opens.
    app.terminals[0].feed_bytes_for_test(b"\x1b[2J\x1b[3;1H");
    app.terminals[0].push_image_for_test(wide_png());
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.terminal_image_payload().is_some(),
        "staging: with nothing over it the picture is sent"
    );
    app.open_terminal_find();
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.terminal_image_payload().is_none(),
        "the Find bar is drawn over the picture, so the picture waits"
    );
    app.terminal_find = None;
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.terminal_image_payload().is_some(),
        "and comes back once the bar closes"
    );
}

/// #2663: a tooltip under the editor's title-bar buttons lands on an image
/// open in the editor; the picture waits. One elsewhere leaves it alone.
#[test]
fn an_editor_picture_waits_only_for_a_tooltip_drawn_over_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = picture_app(tmp.path());
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 45)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let l = app
        .editor_image_payload(0)
        .map(|(_, l)| l.clone())
        .expect("staging: the picture bakes");
    // A tooltip in the side bar, nowhere near the picture.
    app.ui_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new_compact(
        String::from("New File"),
        (8, 2),
    ));
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.editor_image_payload(0).is_some(),
        "a tooltip that misses the picture must not hold it back"
    );
    // A tooltip under the title bar's buttons, over the picture's top rows.
    app.ui_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new_compact(
        String::from("Split Editor Right"),
        (l.cell_x + l.cell_w - 20, l.cell_y.saturating_sub(2)),
    ));
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.editor_image_payload(0).is_none(),
        "the tooltip is drawn over the picture, so the picture waits"
    );
}

/// #2663: the sidebar pictures wait the same way: a tooltip over the Source
/// Control illustration of a folder that is not a repository.
#[test]
fn the_source_control_illustration_waits_for_a_tooltip_drawn_over_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::ITerm2;
    app.cell_pixel = Some((10, 20));
    app.show_tree = true;
    app.sidebar_view = SidebarView::SourceControl;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let hero = app.source_control.last_hero_area;
    assert!(
        hero.width > 0 && hero.height > 0,
        "staging: the illustration has its place"
    );
    assert!(!app.picture_waits(crate::app::popups::PictureSpot::SourceControlHero));
    // A one-line tooltip just above the illustration touches none of it.
    app.ui_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new_compact(
        String::from("Initialize Repository"),
        (hero.x + 1, hero.y + 1),
    ));
    term.draw(|f| app.render(f)).unwrap();
    let above = app.ui_tooltip_area.expect("the tooltip is drawn");
    assert!(
        above.y + 1 < hero.y + 1
            || !app.picture_waits(crate::app::popups::PictureSpot::SourceControlHero),
        "a tooltip that misses the illustration must not hold it back"
    );
    // One whose line lands inside it does.
    app.ui_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new_compact(
        String::from("Initialize Repository"),
        (hero.x + 1, hero.y + hero.height - 1),
    ));
    term.draw(|f| app.render(f)).unwrap();
    let over = app.ui_tooltip_area.expect("the tooltip is drawn");
    assert!(
        over.intersects(hero),
        "staging: the tooltip {over:?} must land on the illustration {hero:?}"
    );
    assert!(app.picture_waits(crate::app::popups::PictureSpot::SourceControlHero));
}

/// The record itself: only a picture whose cells changed between the two
/// notes counts as covered, and a new frame starts with none.
#[test]
fn only_a_picture_whose_cells_changed_counts_as_covered() {
    use crate::app::popups::PictureSpot;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 40, 20));
    app.minimap_img_rect = Rect::new(30, 0, 4, 10);
    app.source_control.last_hero_area = Rect::new(2, 2, 10, 4);
    app.note_pictures_before_popups(&buf);
    buf[(31, 3)].set_symbol("x");
    app.note_pictures_after_popups(&buf);
    assert!(app.picture_waits(PictureSpot::Minimap));
    assert!(!app.picture_waits(PictureSpot::SourceControlHero));
    app.picture_cover.start_frame();
    assert!(!app.picture_waits(PictureSpot::Minimap));
}

/// Codex on #2664: the editor tab's tooltip was drawn with the editor,
/// before the pictures' cells were noted, so over an image it counted as
/// part of the picture and the picture was sent over it. It is now drawn
/// with the other pop-ups.
#[test]
fn an_editor_picture_waits_for_a_tab_tooltip_drawn_over_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = picture_app(tmp.path());
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 45)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let l = app
        .editor_image_payload(0)
        .map(|(_, l)| l.clone())
        .expect("staging: the picture bakes");
    app.tab_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new_compact(
        String::from("pic.png  ~/project"),
        (l.cell_x + 4, l.cell_y + 2),
    ));
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.editor_image_payload(0).is_none(),
        "the tab tooltip is drawn over the picture, so the picture waits"
    );
}
