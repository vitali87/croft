//! Every right-click item and every Source Control control has a way in
//! from the keyboard (#871 step 7). Croft's tenet is that everything has a
//! shortcut; this is the test that keeps a new menu row or panel button
//! from shipping mouse-only.
//!
//! Every right-click menu is one of the #2742 ledgers, and [`MENUS`] says
//! for each how the keyboard reaches its rows. Show Context Menu
//! (`Shift+F10`, #2744) opens six of them, so their rows are reached with
//! the menu's own keys. The other five are opened by their scenes in
//! `right_click.rs`, and every row there must name a key the keyboard
//! reaches it by: the hint `shortcut_for` draws beside it (the key ledger
//! presses every advertised hint), or the palette command
//! `menu_action_command` says it runs.
//!
//! Source Control's controls are the panel's hit areas: every field of
//! `SourceControlPanel` that records a `Rect` a click lands on. Each is in
//! [`SCM_CONTROLS`] with its route, or in [`SCM_NOT_CONTROLS`] with why it
//! takes no click of its own.
//!
//! What has no route yet is in [`NO_ROUTE_YET`] with the step or issue that
//! gives it one. That list can only shrink: an entry no menu holds any more
//! fails, so a route that lands takes its entry with it.

use super::*;
use crate::command_registry::Command;
use crate::widgets::source_control::KeyStop;
use std::collections::BTreeSet;

/// How the keyboard reaches a Source Control control.
#[derive(Clone, Copy, Debug)]
enum Route {
    /// A palette command, by id, does the same to what the keys are on.
    Command(&'static str),
    /// One of Source Control's Tab stops, where the arrows and `Enter`
    /// reach the item.
    Stop(KeyStop),
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
    (
        "right-click-debug-row",
        Some(right_click::the_debug_row_menus),
    ),
    ("right-click-outline", Some(right_click::the_outline_menu)),
    ("right-click-tab", Some(right_click::the_tab_menus)),
    ("right-click-gutter", Some(right_click::the_gutter_menus)),
];

/// Right-click actions the keyboard cannot reach yet, by variant, with what
/// gives them a route. Only shrinks.
const NO_ROUTE_YET: &[(&str, &str)] = &[
    ("SplitEditorLeft", "#871 step 7b: View: Split Editor Left"),
    ("SplitEditorDown", "#871 step 7b: View: Split Editor Down"),
    (
        "MoveEditorUp",
        "#871 step 7b: View: Move Editor into Group Above",
    ),
    (
        "MoveEditorDown",
        "#871 step 7b: View: Move Editor into Group Below",
    ),
    (
        "MoveEditorLeft",
        "#871 step 7b: View: Move Editor into Left Group",
    ),
    (
        "MoveEditorRight",
        "#871 step 7b: View: Move Editor into Right Group",
    ),
    ("SetMinimapSide", "#871 step 7c: View: Toggle Minimap Side"),
    (
        "CopyDebugValue",
        "#871 step 7d: Run and Debug rows from the keyboard",
    ),
    (
        "CopyDebugExpression",
        "#871 step 7d: Run and Debug rows from the keyboard",
    ),
    (
        "AddDebugWatch",
        "#871 step 7d: Run and Debug rows from the keyboard",
    ),
    (
        "RemoveDebugWatch",
        "#871 step 7d: Run and Debug rows from the keyboard",
    ),
    (
        "CopyCallStack",
        "#871 step 7d: Run and Debug rows from the keyboard",
    ),
];

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

#[test]
fn every_right_click_row_has_a_keyboard_route() {
    let classified: BTreeSet<&str> = MENUS.iter().map(|(name, _)| *name).collect();
    let ledgers: BTreeSet<&str> = right_click::RIGHT_CLICK_GOLDENS.into_iter().collect();
    assert_eq!(
        classified, ledgers,
        "every right-click menu says how the keyboard reaches it"
    );
    let mut wrong = Vec::new();
    let mut held = BTreeSet::new();
    for (name, scene) in MENUS {
        let Some(scene) = scene else { continue };
        let menus = right_click::menus_seen_in(scene);
        assert!(!menus.is_empty(), "{name}'s scene opens a menu");
        for (spot, entries) in menus {
            let mut items = Vec::new();
            items_of(&entries, &mut items);
            for (label, action) in items {
                let name = variant(&action);
                held.insert(name.clone());
                let routed =
                    shortcut_for(&action).is_some() || menu_action_command(&action).is_some();
                let waiting = NO_ROUTE_YET.iter().any(|(a, _)| *a == name);
                if routed && waiting {
                    wrong.push(format!(
                        "`{name}` has a key now: remove it from NO_ROUTE_YET"
                    ));
                }
                if !routed && !waiting {
                    wrong.push(format!(
                        "{spot}: \"{label}\" (`{name}`) has no keyboard route; give \
                         it a key `shortcut_for` shows, or open its menu with Shift+F10"
                    ));
                }
            }
        }
    }
    for (name, why) in NO_ROUTE_YET {
        if !held.contains(*name) {
            wrong.push(format!(
                "`{name}` ({why}) is listed as unreachable but no such menu holds it \
                 any more: remove it from NO_ROUTE_YET"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
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
