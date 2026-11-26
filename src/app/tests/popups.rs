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
                root: std::path::PathBuf::new(),
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
            // Waiting for a password, so it has a field to type into.
            let mut dialog =
                crate::widgets::connect_dialog::ConnectDialog::new(String::from("devbox"));
            dialog.set_prompt(
                crate::remote_connect::PromptKind::Password,
                String::from("devbox's password:"),
            );
            app.connect_dialog = Some(dialog);
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
const BESIDE_THEIR_OWNER: [&str; 7] = [
    "render_port_toast",
    "render_update_toast",
    "render_zen_hint",
    "render_context_menu",
    "render_editor_find",
    "render_terminal_find",
    "render_tour_caption",
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

/// An app with `notes.txt` open in the editor and a terminal running a
/// program that prints nothing, so every byte written toward it is one the
/// test sent. `behind` is the pane that has the focus under the pop-ups.
fn paste_app(root: &Path, behind: Pane) -> App {
    let path = root.join("notes.txt");
    std::fs::write(&path, "keep\n").unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("sleep 30")],
        root,
    )
    .unwrap();
    app.active_terminal = 0;
    app.editor.open(&path).unwrap();
    app.focus_pane(behind);
    app
}

/// Every place typed or pasted text can land: the file, the shell, and the
/// field of each pop-up that has one.
fn typed_text(app: &App) -> Vec<(&'static str, Option<String>)> {
    vec![
        ("the file", Some(app.editor.lines.join("\n"))),
        (
            "the shell",
            Some(String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()).into_owned()),
        ),
        (
            "New File box",
            app.prompt.as_ref().map(|p| p.buffer.clone()),
        ),
        (
            "Quick Open",
            app.file_finder.as_ref().map(|f| f.query.clone()),
        ),
        (
            "palette",
            app.command_palette.as_ref().map(|p| p.query.clone()),
        ),
        (
            "Go to Symbol",
            app.go_to_symbol.as_ref().map(|p| p.query.clone()),
        ),
        (
            "workspace symbols",
            app.workspace_symbols.as_ref().map(|p| p.query.clone()),
        ),
        (
            "Jump to Directory",
            app.zoxide_jump.as_ref().map(|p| p.query.clone()),
        ),
        (
            "command history",
            app.command_history_popup.as_ref().map(|p| p.query.clone()),
        ),
        (
            "branch picker",
            app.branch_picker.as_ref().map(|p| p.query.clone()),
        ),
        (
            "Settings search",
            app.settings_editor.as_ref().map(|e| e.query.clone()),
        ),
        (
            "input box",
            app.input_prompt.as_ref().map(|p| p.value.clone()),
        ),
        (
            "list picker",
            app.list_picker.as_ref().map(|p| p.query.clone()),
        ),
        (
            "password box",
            app.connect_dialog.as_ref().map(|d| d.input.clone()),
        ),
        (
            "refusal reason",
            app.approval_ui.as_ref().and_then(|u| u.reason.clone()),
        ),
    ]
}

/// Open `popups` in turn over a fresh app, then either type `text` or paste
/// it, and say where it landed.
fn land(
    popups: &[Popup],
    behind: Pane,
    paste: bool,
    text: &str,
) -> Vec<(&'static str, Option<String>)> {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = paste_app(tmp.path(), behind);
    let _hooks: Vec<_> = popups.iter().map(|&p| open(&mut app, p)).collect();
    for &p in popups {
        assert!(p.is_open(&app), "{p:?} must count as open");
    }
    if paste {
        app.handle_paste(text);
    } else {
        for c in text.chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
                .unwrap();
        }
    }
    typed_text(&app)
}

/// The places where pasting `zx` and typing it end up differently. The
/// startup warning, which takes the first key to close itself, drops a paste
/// whole: there, a paste must change nothing, as typing nothing does.
fn paste_differs_from_typing(popups: &[Popup], behind: Pane) -> Vec<String> {
    let typing = if popups.last() == Some(&Popup::TerminalWarning) {
        ""
    } else {
        "zx"
    };
    let typed = land(popups, behind, false, typing);
    let pasted = land(popups, behind, true, "zx");
    typed
        .iter()
        .zip(&pasted)
        .filter(|(t, p)| t != p)
        .map(|((place, t), (_, p))| format!("{place}: typed {t:?}, pasted {p:?}"))
        .collect()
}

/// The bug (#2665): a paste went to one of six pop-ups or the New File box,
/// and otherwise to the pane behind. Under the other 22 pop-ups, the SSH
/// password box, the branch picker, Settings, an agent's proposal and every
/// confirm among them, it was typed into the file or the shell the pop-up
/// hides. A paste now lands where typing the same text does: in the field
/// of a pop-up that has one, and nowhere under one that has none.
#[test]
fn a_paste_lands_where_typing_does_under_every_popup() {
    let mut wrong = Vec::new();
    for (behind, name) in [(Pane::Editor, "editor"), (Pane::Terminal, "terminal")] {
        for p in Popup::ALL {
            let differs = paste_differs_from_typing(&[p], behind);
            if !differs.is_empty() {
                wrong.push(format!("{p:?} over the {name}: {}", differs.join("; ")));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a paste went somewhere typing does not:\n{}",
        wrong.join("\n")
    );
}

/// An agent's proposal can arrive over any pop-up and takes the keys from
/// it; a paste must not go into the pop-up underneath either (#2665).
#[test]
fn a_proposal_arriving_over_a_popup_takes_the_paste_as_it_takes_the_keys() {
    let mut wrong = Vec::new();
    for p in Popup::ALL.into_iter().filter(|&p| p != Popup::Approval) {
        let differs = paste_differs_from_typing(&[p, Popup::Approval], Pane::Editor);
        if !differs.is_empty() {
            wrong.push(format!("{p:?}: {}", differs.join("; ")));
        }
    }
    assert!(
        wrong.is_empty(),
        "under a proposal, a paste went somewhere typing does not:\n{}",
        wrong.join("\n")
    );
}

/// The reason for refusing an agent's edit, once `r` has opened it, takes a
/// paste as it takes typing, without the line break that would send it.
#[test]
fn a_paste_goes_into_the_reason_for_refusing_an_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = paste_app(tmp.path(), Pane::Editor);
    let _hook = open(&mut app, Popup::Approval);
    app.approval_ui.as_mut().unwrap().reason = Some(String::from("not "));
    app.handle_paste("this way\n");
    assert_eq!(
        app.approval_ui.as_ref().unwrap().reason.as_deref(),
        Some("not this way")
    );
    assert_eq!(
        app.editor.lines.join("\n"),
        "keep",
        "nothing reaches the file"
    );
}

/// A right-click menu is not one of the pop-ups drawn over the middle of
/// the screen, but it takes every key while it is open, so a paste goes
/// nowhere either: not into the file or the shell beside it (Codex, #2665).
#[test]
fn a_paste_with_a_right_click_menu_open_goes_nowhere() {
    for behind in [Pane::Editor, Pane::Terminal] {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = paste_app(tmp.path(), behind);
        let items = vec![(
            String::from("New File"),
            MenuAction::Create(CreateKind::File),
        )];
        app.context_menu = Some(ContextMenu::flat((2, 2), items, tmp.path().to_path_buf()));
        app.handle_paste("zx");
        assert_eq!(
            app.editor.lines.join("\n"),
            "keep",
            "nothing reaches the file"
        );
        assert_eq!(
            app.terminals[0].written_bytes_for_test(),
            b"",
            "nothing reaches the shell"
        );
        assert!(app.context_menu.is_some(), "the menu stays open");
    }
}

/// #2678 review (Codex): Settings opens an input prompt over itself to edit
/// a value, and the prompt has the keys while it is up. It has the mouse
/// too: a click outside it closes it, as anywhere else, and Settings stays.
#[test]
fn a_click_outside_a_setting_value_prompt_closes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let _hook = open(&mut app, Popup::SettingsEditor);
    app.input_prompt = Some(crate::widgets::input_prompt::InputPrompt::new(
        crate::widgets::input_prompt::InputPurpose::SettingValue {
            key: String::from("tab_size"),
        },
        "tab size",
        "new value",
    ));
    draw(&mut app, 160, 40);
    left_click(&mut app, 0, 0);
    assert!(
        app.input_prompt.is_none(),
        "the click outside closed the prompt"
    );
    assert!(app.settings_editor.is_some(), "Settings stays under it");
}

/// #2678 review (Greptile): a pop-up that opens mid-drag takes the release.
/// The drag ends there without its drop. Left live, the next release after
/// the pop-up closed finished it and moved the file into the folder.
#[test]
fn a_drag_a_popup_cuts_short_drops_nothing_later() {
    let mut dropped = Vec::new();
    for p in Popup::ALL.into_iter().filter(|p| !p.handles_mouse()) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("src.txt"), "x").unwrap();
        std::fs::create_dir(tmp.path().join("dest")).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.tree.last_area = Rect::new(0, 0, 40, 8);
        app.tree.last_inner = Rect::new(1, 1, 38, 6);
        let row_of = |app: &App, name: &str| {
            let i = app.tree.nodes.iter().position(|n| n.path.ends_with(name));
            app.tree.last_inner.y + i.expect("the tree lists it") as u16
        };
        let (src_row, dest_row) = (row_of(&app, "src.txt"), row_of(&app, "dest"));
        let mouse = |app: &mut App, kind, row| {
            app.handle_mouse(crossterm::event::MouseEvent {
                kind,
                column: 6,
                row,
                modifiers: KeyModifiers::NONE,
            });
        };
        use crossterm::event::{MouseButton::Left, MouseEventKind::*};
        mouse(&mut app, Down(Left), src_row);
        mouse(&mut app, Drag(Left), dest_row);
        let hook = open(&mut app, p);
        mouse(&mut app, Up(Left), dest_row);
        close(&mut app, p);
        drop(hook);
        mouse(&mut app, Up(Left), dest_row);
        if tmp.path().join("dest/src.txt").exists() {
            dropped.push(format!("{p:?}"));
        }
    }
    assert!(
        dropped.is_empty(),
        "a drag cut short by these pop-ups dropped the file later: {}",
        dropped.join(", ")
    );
}

/// #2678 review (Greptile): the port toast is drawn under the pop-ups, so a
/// click on one of its buttons while a pop-up is open lands on a button the
/// pop-up may hide. It must not press it.
#[test]
fn a_toast_button_under_a_popup_is_not_pressed() {
    let mut pressed = Vec::new();
    for p in Popup::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.show_port_toast(3000, Some("node".into()));
        draw(&mut app, 140, 50);
        let dismiss = app
            .port_toast
            .as_ref()
            .expect("the toast shows")
            .buttons
            .iter()
            .find(|(_, a)| matches!(a, super::PortToastAction::Dismiss))
            .map(|(r, _)| *r)
            .expect("the toast has a dismiss button");
        let _hook = open(&mut app, p);
        left_click(&mut app, dismiss.x, dismiss.y);
        if app.port_toast.is_none() {
            pressed.push(format!("{p:?}"));
        }
    }
    assert!(
        pressed.is_empty(),
        "a click pressed the toast's button under these pop-ups: {}",
        pressed.join(", ")
    );
}

/// #2678 review (CodeRabbit): the status bar and the layout icons are
/// handled before the panes, and the New File box's rule to ignore the mouse
/// came after them: a click on a layout icon under it toggled the panel. No
/// pop-up lets a click reach them.
#[test]
fn a_layout_icon_under_a_popup_is_not_pressed() {
    let mut pressed = Vec::new();
    for p in Popup::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        draw(&mut app, 160, 40);
        let icon = app.layout_icon_areas.toggle_panel;
        assert!(icon.width > 0, "setup: the layout icons are drawn");
        let shown = app.show_terminal;
        let _hook = open(&mut app, p);
        left_click(&mut app, icon.x, icon.y);
        if app.show_terminal != shown {
            pressed.push(format!("{p:?}"));
        }
    }
    assert!(
        pressed.is_empty(),
        "a click toggled the panel under these pop-ups: {}",
        pressed.join(", ")
    );
}

/// The bug (#2674): only an agent's proposal, the two unsaved-changes
/// prompts and the terminal paste question took the mouse. Under the other
/// nine confirms, Settings and the startup warning, a click went to what
/// they cover: it moved the editor's cursor and closed a tab. A pop-up
/// either handles the mouse itself (a picker closes on a click outside it)
/// or takes every mouse event; none lets one through.
#[test]
fn a_click_behind_a_popup_never_reaches_what_it_covers() {
    // Two files open, the first shown, then `p` opened over them. A picker
    // closes on a click outside it, so each click gets a fresh setup: both
    // land while the pop-up is open.
    fn setup(
        root: &Path,
        p: Popup,
    ) -> (
        App,
        Option<std::os::unix::net::UnixStream>,
        (u16, u16),
        (u16, u16),
    ) {
        let a = root.join("a.txt");
        let b = root.join("b.txt");
        std::fs::write(&a, "alpha\nbeta\ngamma\n").unwrap();
        std::fs::write(&b, "bravo\n").unwrap();
        let mut app = App::new(root.to_path_buf()).unwrap();
        app.editor.open_pinned(&a).unwrap();
        app.editor.open_pinned(&b).unwrap();
        app.editor.select(0);
        app.focus_pane(Pane::Editor);
        draw(&mut app, 160, 40);
        let tab_row = app.editor.tab_strip_y_for_test();
        let close_b = app.editor.close_screen_x(1).expect("b.txt's close button");
        let caret = app.editor.cursor_screen_pos().expect("the editor's caret");
        let hook = open(&mut app, p);
        draw(&mut app, 160, 40);
        (app, hook, (close_b, tab_row), caret)
    }
    let mut reached = Vec::new();
    for p in Popup::ALL {
        let mut what = Vec::new();
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _hook, _, (x, y)) = setup(tmp.path(), p);
        // Two lines down and three along from the caret: "gamma", column 3.
        left_click(&mut app, x + 3, y + 2);
        if (app.editor.cursor_row, app.editor.cursor_col) != (0, 0) {
            what.push("moved the cursor");
        }
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, _hook, (x, y), _) = setup(tmp.path(), p);
        // The other tab's close button.
        left_click(&mut app, x, y);
        if app.editor.tab_count() != 2 {
            what.push("closed a tab");
        }
        if !what.is_empty() {
            reached.push(format!("{p:?} ({})", what.join(", ")));
        }
    }
    assert!(
        reached.is_empty(),
        "a click went through these pop-ups to the editor behind: {}",
        reached.join(", ")
    );
}

/// The eight pickers answer the mouse through one rule
/// (`widgets::picker_rows::answer_mouse`), and each says only how it closes
/// and what taking a row does. A left or a right press outside closes every
/// one of them, and a left press on its top border, inside it but on no
/// row, leaves it open.
#[test]
fn every_picker_closes_on_a_press_outside_and_stays_open_on_its_border() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    const PICKERS: [Popup; 8] = [
        Popup::FileFinder,
        Popup::CommandPalette,
        Popup::GoToSymbol,
        Popup::WorkspaceSymbols,
        Popup::ProcessPicker,
        Popup::ZoxideJump,
        Popup::BranchPicker,
        Popup::ListPicker,
    ];
    fn last_rect(app: &App, p: Popup) -> Rect {
        match p {
            Popup::FileFinder => app.file_finder.as_ref().unwrap().last_rect,
            Popup::CommandPalette => app.command_palette.as_ref().unwrap().last_rect,
            Popup::GoToSymbol => app.go_to_symbol.as_ref().unwrap().last_rect,
            Popup::WorkspaceSymbols => app.workspace_symbols.as_ref().unwrap().last_rect,
            Popup::ProcessPicker => app.process_picker.as_ref().unwrap().last_rect,
            Popup::ZoxideJump => app.zoxide_jump.as_ref().unwrap().last_rect,
            Popup::BranchPicker => app.branch_picker.as_ref().unwrap().last_rect,
            Popup::ListPicker => app.list_picker.as_ref().unwrap().last_rect,
            _ => unreachable!("{p:?} is no picker"),
        }
    }
    // `p` open over a fresh app, drawn once so it knows where it is.
    fn opened(root: &Path, p: Popup) -> (App, Rect) {
        let mut app = App::new(root.to_path_buf()).unwrap();
        open(&mut app, p);
        draw(&mut app, 160, 40);
        assert!(p.is_open(&app), "{p:?} opens");
        let rect = last_rect(&app, p);
        assert!(rect.width > 0 && rect.height > 0, "{p:?} was drawn");
        (app, rect)
    }
    fn press(app: &mut App, button: MouseButton, column: u16, row: u16) {
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(button),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        });
    }
    let mut wrong = Vec::new();
    for p in PICKERS {
        for button in [MouseButton::Left, MouseButton::Right] {
            let tmp = tempfile::tempdir().unwrap();
            let (mut app, rect) = opened(tmp.path(), p);
            // The bottom-right corner, which no picker reaches.
            let (x, y) = (159, 39);
            assert!(
                !rect.contains(ratatui::layout::Position::new(x, y)),
                "{p:?} reaches the corner"
            );
            press(&mut app, button, x, y);
            if p.is_open(&app) {
                wrong.push(format!("{p:?} stayed open on a {button:?} press outside"));
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let (mut app, rect) = opened(tmp.path(), p);
        press(&mut app, MouseButton::Left, rect.x + 1, rect.y);
        if !p.is_open(&app) {
            wrong.push(format!("{p:?} closed on a press on its own border"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// #2679: in Zen Mode, an Esc that closed one of the pop-ups the old short
/// list left out (a confirm, Settings, the connect dialog, an agent's
/// proposal, the startup warning) counted as an Esc that did nothing, so one
/// more Esc left Zen Mode. Closing a pop-up is never half of Esc Esc.
#[test]
fn closing_a_popup_with_esc_is_not_half_of_zen_esc_esc() {
    let mut left = Vec::new();
    for p in Popup::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "one\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open(&file).unwrap();
        app.focus_pane(Pane::Editor);
        app.toggle_zen_mode();
        assert!(app.zen_mode, "setup: Zen Mode is on");
        let _hook = open(&mut app, p);
        app.focus = Pane::Editor;
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        if p.is_open(&app) {
            // Esc does not close this one; nothing to count.
            continue;
        }
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        if !app.zen_mode {
            left.push(format!("{p:?}"));
        }
    }
    assert!(
        left.is_empty(),
        "Esc closing these pop-ups counted toward Esc Esc and left Zen: {}",
        left.join(", ")
    );
}

/// #2679: Screencast Mode hides what is typed while the terminal has focus,
/// since a shell may be reading a password. Typing goes to a pop-up open
/// over the terminal instead, and it was hidden for the pop-ups the old
/// short list left out. Only a field that is secret stays hidden.
#[test]
fn screencast_shows_typing_into_a_popup_over_the_terminal() {
    let mut hidden = Vec::new();
    for p in Popup::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.show_terminal = true;
        app.focus_pane(Pane::Terminal);
        assert!(
            app.screencast_hides_typing(),
            "setup: the shell's typing hides"
        );
        let _hook = open(&mut app, p);
        app.focus = Pane::Terminal;
        let secret = app
            .connect_dialog
            .as_ref()
            .is_some_and(|d| d.input_is_secret());
        if app.screencast_hides_typing() != secret {
            hidden.push(format!("{p:?}"));
        }
    }
    assert!(
        hidden.is_empty(),
        "typing into these pop-ups was hidden as if it went to the shell: {}",
        hidden.join(", ")
    );
}

/// #2679: auto-hide may collapse the sidebar only while no pop-up is open:
/// many pop-ups are anchored over it or ask about something in it, such as
/// Source Control's Discard Changes. It checked the old short list and the
/// file and connect boxes, and collapsed under the rest.
#[test]
fn the_sidebar_never_auto_hides_under_a_popup() {
    let mut collapsed = Vec::new();
    for p in Popup::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.sidebar_auto_hide = true;
        app.show_tree = true;
        assert!(app.sidebar_auto_hide_allowed(), "setup: it would collapse");
        let _hook = open(&mut app, p);
        if app.sidebar_auto_hide_allowed() {
            collapsed.push(format!("{p:?}"));
        }
    }
    assert!(
        collapsed.is_empty(),
        "auto-hide could collapse the sidebar under these pop-ups: {}",
        collapsed.join(", ")
    );
}

/// #2679: while a pop-up has the keys, the editor drops the marks that say
/// it has them (its tab's accent). It learned that from the old short list,
/// so under a confirm, Settings or an agent's proposal it still looked as if
/// typing would land in it.
#[test]
fn the_editor_never_looks_focused_under_a_popup() {
    let mut focused = Vec::new();
    for p in Popup::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "one\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open(&file).unwrap();
        app.focus_pane(Pane::Editor);
        draw(&mut app, 120, 40);
        assert!(
            !app.editor.overlay_has_keyboard,
            "setup: the editor has the keys"
        );
        let _hook = open(&mut app, p);
        draw(&mut app, 120, 40);
        if !app.editor.overlay_has_keyboard {
            focused.push(format!("{p:?}"));
        }
    }
    assert!(
        focused.is_empty(),
        "the editor kept its focused look under these pop-ups: {}",
        focused.join(", ")
    );
}

/// #2679: the history scrubber's slider took a click under the file prompt
/// and the connect dialog, which handle the mouse further down: a click
/// there moved the file to another commit behind the pop-up.
#[test]
fn the_scrubber_never_seeks_under_a_popup() {
    let mut sought = Vec::new();
    for p in Popup::ALL {
        let repo = super::scrub_repo();
        let mut app = App::new(repo.path().to_path_buf()).unwrap();
        app.editor.open(&repo.path().join("a.txt")).unwrap();
        app.scrub_history();
        draw(&mut app, 120, 30);
        let track = app.scrub_slider;
        assert!(track.width > 10, "setup: the slider lays out");
        let before = app.scrub_view.as_ref().map(|v| v.lines.clone());
        let _hook = open(&mut app, p);
        left_click(&mut app, track.x, track.y);
        if app.scrub_view.as_ref().map(|v| v.lines.clone()) != before {
            sought.push(format!("{p:?}"));
        }
    }
    assert!(
        sought.is_empty(),
        "a click on the slider moved the scrubber under these pop-ups: {}",
        sought.join(", ")
    );
}
