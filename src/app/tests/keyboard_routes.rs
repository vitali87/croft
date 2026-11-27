//! Every right-click item and every Source Control control has a way in
//! from the keyboard (#871 step 7). Croft's tenet is that everything has a
//! shortcut; this is the test that keeps a new menu row or panel button
//! from shipping mouse-only.
//!
//! Every right-click menu is one of the #2742 ledgers, and [`menus::MENUS`]
//! says for each how the keyboard reaches its rows. Show Context Menu
//! (`Shift+F10`, #2744) opens seven of them, so their rows are reached with
//! the menu's own keys. The other four are opened by their scenes in
//! `right_click.rs`, and every row there must name a key the keyboard
//! reaches it by: the hint `shortcut_for` draws beside it (the key ledger
//! presses every advertised hint), the palette command
//! `menu_action_command` says it runs, or one of [`menus::COMMAND_ROUTES`].
//!
//! Source Control's controls are the panel's hit areas: every field of
//! `SourceControlPanel` that records a `Rect` a click lands on. Each is in
//! [`SCM_CONTROLS`] with its route, or in [`SCM_NOT_CONTROLS`] with why it
//! takes no click of its own.
//!
//! The Source Control half runs everywhere. The menu half runs on Linux,
//! where the right-click ledgers' scenes do and where CI runs the tests;
//! rows only a Mac shows are built there too, by the flag their builders
//! take.
//!
//! A row or control with no keyboard route fails: it ships with one.

use super::*;
use crate::command_registry::Command;
use crate::widgets::source_control::KeyStop;

/// How the keyboard reaches a Source Control control.
#[derive(Clone, Copy, Debug)]
enum Route {
    /// A palette command, by id, does the same to what the keys are on.
    Command(&'static str),
    /// One of Source Control's Tab stops, where the arrows and `Enter`
    /// reach the item.
    Stop(KeyStop),
}

/// Source Control's controls, by the panel field that records where a
/// click lands, each with the keyboard's way to it. A field may be listed
/// more than once when it holds several controls.
const SCM_CONTROLS: &[(&str, Route)] = &[
    ("last_input_area", Route::Stop(KeyStop::Message)),
    ("last_button_area", Route::Command("git_commit")),
    (
        "last_commit_caret_area",
        Route::Command("git_show_commit_actions"),
    ),
    ("last_list_area", Route::Stop(KeyStop::List)),
    (
        "last_init_repo_button_area",
        Route::Command("git_initialize_repository"),
    ),
    ("nested_repo_button_areas", Route::Stop(KeyStop::Card)),
    ("repo_row_areas", Route::Stop(KeyStop::Repositories)),
    ("last_row_actions", Route::Command("git_stage_changes")),
    ("last_row_actions", Route::Command("git_unstage_changes")),
    ("last_row_actions", Route::Command("git_discard_changes")),
    ("header_refresh_btn", Route::Command("git_refresh")),
    ("last_branch_area", Route::Command("git_checkout_to")),
    (
        "header_more_btn",
        Route::Command("git_show_source_control_actions"),
    ),
];

/// Hit areas the panel records that are not controls of their own.
const SCM_NOT_CONTROLS: &[(&str, &str)] = &[
    ("last_area", "the whole panel"),
    ("last_inner", "the panel inside its border"),
    (
        "last_scrollbar",
        "the change list's scrollbar; the list's arrows and PageUp / PageDown scroll it",
    ),
    ("last_hero_area", "the no-repository card's picture"),
];

/// The route as a failure names it.
fn describe(route: Route) -> String {
    match route {
        Route::Command(id) => format!("command `{id}`"),
        Route::Stop(stop) => format!("the {stop:?} Tab stop"),
    }
}

/// A route names something that exists: a registered command.
fn check_route(route: Route, item: &str, wrong: &mut Vec<String>) {
    if let Route::Command(id) = route
        && Command::from_id(id).is_none()
    {
        wrong.push(format!("{item}: no command `{id}`"));
    }
}

/// The hit-area fields of `SourceControlPanel`, read from its source: those
/// whose type records a `Rect` a click lands on.
fn scm_hit_fields() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/widgets/source_control.rs");
    let text = std::fs::read_to_string(path).unwrap();
    let start = text
        .find("pub struct SourceControlPanel {")
        .expect("SourceControlPanel is in source_control.rs");
    let body = &text[start..];
    let end = body.find("\n}\n").expect("the struct closes");
    body[..end]
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (name, ty) = line.trim().strip_prefix("pub ")?.split_once(':')?;
            (ty.contains("Rect") || ty.contains("RowActionAreas")).then(|| name.trim().to_string())
        })
        .collect()
}

#[test]
fn every_source_control_control_has_a_keyboard_route() {
    let fields = scm_hit_fields();
    assert!(
        fields.iter().any(|f| f == "header_more_btn"),
        "the panel's fields are read: {fields:?}"
    );
    let mut wrong = Vec::new();
    for field in &fields {
        let control = SCM_CONTROLS.iter().any(|(f, _)| f == field);
        let layout = SCM_NOT_CONTROLS.iter().any(|(f, _)| f == field);
        if !control && !layout {
            wrong.push(format!(
                "SourceControlPanel::{field} takes clicks but has no keyboard route"
            ));
        }
    }
    for (field, route) in SCM_CONTROLS {
        check_route(*route, field, &mut wrong);
        if !fields.contains(&field.to_string()) {
            wrong.push(format!(
                "SCM_CONTROLS names `{field}` ({}), which the panel no longer has",
                describe(*route)
            ));
        }
    }
    for (field, _) in SCM_NOT_CONTROLS {
        if !fields.contains(&field.to_string()) {
            wrong.push(format!(
                "SCM_NOT_CONTROLS names `{field}`, which the panel no longer has"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The right-click half, and the tab rows' commands: its menus come from
/// the right-click ledgers' scenes, which run on Linux, where the CI
/// tests run.
#[cfg(target_os = "linux")]
mod menus {
    use super::*;
    use std::collections::BTreeSet;

    /// The menus opened by a scene, built the way a local macOS build builds
    /// them: the tab menu then carries Reveal in Finder. Its builder takes
    /// that row as a flag, so the row is checked here, where the tests run,
    /// though only a Mac shows it. (The Explorer's Finder row is in a menu
    /// Show Context Menu opens.)
    fn macos_rows() -> Vec<(String, Vec<MenuEntry>)> {
        let path = PathBuf::from("/w/a.rs");
        vec![(
            String::from("the tab menu on macOS"),
            build_tab_context_menu_items(0, 2, true, Some(&path), true, false, false),
        )]
    }

    /// A right-click menu's ledger, with how the keyboard reaches its rows:
    /// `None` when Show Context Menu opens the menu itself (the `shift_f10_*`
    /// tests in `right_click.rs` prove each), else the scene that opens it,
    /// whose every row must carry a key of its own.
    type MenuRoute = (&'static str, Option<fn()>);

    /// Every right-click menu, by its ledger: see [`MenuRoute`].
    const MENUS: [MenuRoute; 11] = [
        ("right-click-editor", None),
        ("right-click-explorer", None),
        ("right-click-commit", None),
        ("right-click-terminal", None),
        ("right-click-capture", None),
        ("right-click-problem", None),
        ("right-click-minimap", Some(right_click::the_minimap_menu)),
        ("right-click-debug-row", None),
        ("right-click-outline", Some(right_click::the_outline_menu)),
        ("right-click-tab", Some(right_click::the_tab_menus)),
        ("right-click-gutter", Some(right_click::the_gutter_menus)),
    ];

    /// Right-click actions whose row runs no command, but which a palette
    /// command reaches all the same, by variant: (variant, command id, why).
    const COMMAND_ROUTES: &[(&str, &str, &str)] = &[(
        "SetMinimapSide",
        "toggle_minimap_side",
        "the Side rows pick an edge; the toggle moves the strip to the other one, \
     and picking the edge it is on changes nothing",
    )];

    /// An action's variant name, as its `Debug` form starts.
    fn variant(action: &MenuAction) -> String {
        format!("{action:?}")
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// Every item of `items`, submenus' included, as (label, action).
    fn items_of(items: &[MenuEntry], out: &mut Vec<(String, MenuAction)>) {
        for entry in items {
            match entry {
                MenuEntry::Item { label, action } => out.push((label.clone(), action.clone())),
                MenuEntry::Submenu { items, .. } => items_of(items, out),
                MenuEntry::Separator | MenuEntry::Header(_) => {}
            }
        }
    }

    #[test]
    fn every_right_click_row_has_a_keyboard_route() {
        let classified: BTreeSet<&str> = MENUS.iter().map(|(name, _)| *name).collect();
        let ledgers: BTreeSet<&str> = right_click::RIGHT_CLICK_GOLDENS.into_iter().collect();
        assert_eq!(
            classified, ledgers,
            "every right-click menu says how the keyboard reaches it"
        );
        let mut menus = Vec::new();
        for (name, scene) in MENUS {
            let Some(scene) = scene else { continue };
            let seen = right_click::menus_seen_in(scene);
            assert!(!seen.is_empty(), "{name}'s scene opens a menu");
            menus.extend(seen);
        }
        menus.extend(macos_rows());
        let mut wrong = Vec::new();
        let mut held = BTreeSet::new();
        for (spot, entries) in menus {
            let mut items = Vec::new();
            items_of(&entries, &mut items);
            for (label, action) in items {
                let name = variant(&action);
                held.insert(name.clone());
                let routed = shortcut_for(&action).is_some()
                    || menu_action_command(&action).is_some()
                    || COMMAND_ROUTES.iter().any(|(v, _, _)| *v == name);
                if !routed {
                    wrong.push(format!(
                        "{spot}: \"{label}\" (`{name}`) has no keyboard route; give \
                     it a key `shortcut_for` shows, or open its menu with Shift+F10"
                    ));
                }
            }
        }
        for (name, id, _) in COMMAND_ROUTES {
            check_route(Route::Command(id), name, &mut wrong);
            if !held.contains(*name) {
                wrong.push(format!(
                    "COMMAND_ROUTES names `{name}`, which no such menu holds"
                ));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// Three saved tabs in one group, the last active, and, with `split`, the
    /// group split to the right first, with the three tabs' group active, so
    /// the moves have a neighbour. Two apps over one folder draw the same tabs.
    fn three_tabs(root: &Path, split: bool) -> App {
        let names = ["a.rs", "b.rs", "c.rs"];
        for name in names {
            std::fs::write(root.join(name), "fn main() {}\n").unwrap();
        }
        let mut app = App::new(root.to_path_buf()).unwrap();
        for name in names {
            app.editor.open_pinned(&root.join(name)).unwrap();
        }
        if split {
            // Back to the left group, which keeps all three tabs.
            app.split_editor();
            app.focus_editor_group(true);
        }
        app
    }

    /// The tab menu's Split and Move rows and the palette commands routed for
    /// them (#871 step 7b) leave the same editor groups, the same focus and the
    /// same status, from one group and from a split: the command is the row,
    /// reached from the keyboard.
    #[test]
    fn a_tab_row_and_its_command_leave_the_same_groups() {
        let rows = [
            MenuAction::SplitEditorUp,
            MenuAction::SplitEditorDown,
            MenuAction::SplitEditorLeft,
            MenuAction::SplitInGroup,
            MenuAction::MoveEditorUp,
            MenuAction::MoveEditorDown,
            MenuAction::MoveEditorLeft,
            MenuAction::MoveEditorRight,
        ];
        for split in [false, true] {
            for action in &rows {
                let cmd = menu_action_command(action).expect("the row runs a command");
                let tmp = tempfile::tempdir().unwrap();
                let mut by_row = three_tabs(tmp.path(), split);
                let mut by_command = three_tabs(tmp.path(), split);
                let before = key_ledger::groups(&by_row);
                by_row.dispatch_menu_action(action.clone(), tmp.path().to_path_buf());
                by_command.run_command(cmd);
                let after = key_ledger::groups(&by_row);
                let at = format!("{action:?} as {cmd:?}, split first: {split}");
                assert_ne!(after, before, "{at}: the row changes the groups");
                assert_eq!(key_ledger::groups(&by_command), after, "{at}");
                assert!(by_command.focus == by_row.focus, "{at}: focus");
                assert_eq!(by_command.status, by_row.status, "{at}: status");
            }
        }
    }

    /// File: Reveal Active File in Explorer View does what the tab menu's
    /// Reveal in Explorer View row does on the active tab: the Explorer shows,
    /// with the file's row selected and the tree focused.
    #[test]
    fn reveal_active_file_in_explorer_is_the_tab_row() {
        let tmp = tempfile::tempdir().unwrap();
        let mut by_row = three_tabs(tmp.path(), false);
        let mut by_command = three_tabs(tmp.path(), false);
        for app in [&mut by_row, &mut by_command] {
            app.set_sidebar_view(SidebarView::Search);
            app.focus_pane(Pane::Editor);
        }
        let path = by_row.editor.path.clone().expect("c.rs is active");
        by_row.dispatch_menu_action(MenuAction::RevealInExplorer(path), tmp.path().to_path_buf());
        by_command
            .run_command(crate::widgets::command_palette::Command::RevealActiveFileInExplorer);
        let picked = |app: &App| {
            app.tree
                .selected_path()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        };
        assert_eq!(
            picked(&by_row).as_deref(),
            Some("c.rs"),
            "the row reveals c.rs"
        );
        assert_eq!(picked(&by_command), picked(&by_row));
        assert!(by_command.sidebar_view == SidebarView::Explorer);
        assert!(by_command.focus == by_row.focus && by_row.focus == Pane::Tree);
    }

    /// With no file to reveal, the command says so rather than doing nothing.
    #[test]
    fn reveal_active_file_in_explorer_without_a_file_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.run_command(crate::widgets::command_palette::Command::RevealActiveFileInExplorer);
        assert_eq!(app.status, "Reveal in Explorer View: no active file");
    }

    /// View: Move / Copy Editor into New Window run the tab rows' code on the
    /// active tab: an unsaved buffer gets the rows' own refusal (no window is
    /// opened from a test).
    #[test]
    fn the_new_window_commands_are_the_tab_rows() {
        use crate::widgets::command_palette::Command as Cmd;
        for (action, cmd) in [
            (MenuAction::MoveIntoNewWindow(0), Cmd::MoveEditorToNewWindow),
            (MenuAction::CopyIntoNewWindow(0), Cmd::CopyEditorToNewWindow),
        ] {
            assert_eq!(menu_action_command(&action), Some(cmd));
            let tmp = tempfile::tempdir().unwrap();
            let mut by_row = App::new(tmp.path().to_path_buf()).unwrap();
            let mut by_command = App::new(tmp.path().to_path_buf()).unwrap();
            by_row.dispatch_menu_action(action.clone(), tmp.path().to_path_buf());
            by_command.run_command(cmd);
            assert!(
                by_row.status.contains("save the file first"),
                "{action:?}: {}",
                by_row.status
            );
            assert_eq!(by_command.status, by_row.status, "{cmd:?}");
        }
    }

    /// View: Toggle Minimap Side moves the strip to the edge it is not on, as
    /// picking that edge in the minimap menu's Side rows does.
    #[test]
    fn toggle_minimap_side_picks_the_other_edge() {
        let tmp = tempfile::tempdir().unwrap();
        let mut by_row = App::new(tmp.path().to_path_buf()).unwrap();
        let mut by_command = App::new(tmp.path().to_path_buf()).unwrap();
        for side in [MinimapSide::Left, MinimapSide::Right] {
            assert!(
                by_command.minimap_side != side,
                "setup: {side:?} is the other edge"
            );
            by_row.dispatch_menu_action(MenuAction::SetMinimapSide(side), tmp.path().to_path_buf());
            by_command.run_command(crate::widgets::command_palette::Command::ToggleMinimapSide);
            assert!(
                by_command.minimap_side == side,
                "the toggle lands on {side:?}"
            );
            assert!(by_row.minimap_side == side);
        }
    }
}
