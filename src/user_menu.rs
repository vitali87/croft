//! The user's own menu: a short list of palette commands kept in
//! `~/.config/croft/menu.json`, so the ones used every day are one key away
//! instead of a search in the Command Palette.
//!
//! The same list is shown two ways: as a drop-down (`User Menu: Open`,
//! `Cmd+K Shift+M`) and as a starred block at the top of the Command Palette.
//! Only built-in commands are accepted, named by the ids `keybindings.json`
//! uses; tasks and shell commands are deliberately not part of it.
//!
//! The file is JSONC like `keybindings.json`. A row is a command, a group of
//! rows (a submenu, one level: the menu opens no submenu inside a submenu) or
//! a divider; a row that does not parse, names an unknown command or is a
//! group inside a group, is skipped and counted, never fatal.

use std::path::PathBuf;

use crate::widgets::command_palette::Command;

pub fn menu_path() -> PathBuf {
    crate::paths::config_dir().join("menu.json")
}

/// One entry of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserMenuItem {
    /// A command, under its own title unless the row gives a `label`.
    Command {
        label: Option<String>,
        command: Command,
        /// Whether the Command Palette lists it first (default yes).
        pinned: bool,
    },
    /// A submenu.
    Group {
        label: String,
        items: Vec<UserMenuItem>,
    },
    Separator,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserMenu {
    pub items: Vec<UserMenuItem>,
    /// Rows skipped because they did not parse or named no command.
    pub skipped: usize,
    /// Set when the whole file did not parse.
    pub problem: Option<String>,
}

#[derive(serde::Deserialize)]
struct Row {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    items: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    separator: bool,
    #[serde(default = "yes")]
    pinned: bool,
}

fn yes() -> bool {
    true
}

impl UserMenu {
    /// The menu in `menu.json`, or the starter set when the file is absent.
    pub fn load() -> Self {
        // Tests must not depend on whatever menu.json the machine has, nor on
        // the starter set shaping the palette's order.
        if cfg!(test) {
            return Self::default();
        }
        match std::fs::read_to_string(menu_path()) {
            Ok(text) => Self::from_json(&text),
            Err(_) => Self::from_json(TEMPLATE),
        }
    }

    pub fn from_json(json: &str) -> Self {
        let rows: Vec<serde_json::Value> =
            match serde_json::from_str(&crate::jsonc::strip_jsonc(json)) {
                Ok(rows) => rows,
                Err(e) => {
                    return Self {
                        problem: Some(format!("menu.json does not parse ({e})")),
                        ..Self::default()
                    };
                }
            };
        let mut skipped = 0;
        let items = parse_rows(rows, &mut skipped, false);
        Self {
            items,
            skipped,
            problem: None,
        }
    }

    /// The commands the Command Palette pins, in menu order.
    pub fn pinned(&self) -> Vec<Command> {
        fn walk(items: &[UserMenuItem], out: &mut Vec<Command>) {
            for item in items {
                match item {
                    UserMenuItem::Command {
                        command,
                        pinned: true,
                        ..
                    } if !out.contains(command) => out.push(*command),
                    UserMenuItem::Group { items, .. } => walk(items, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.items, &mut out);
        out
    }

    /// Whether the menu has no command to show.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

fn parse_rows(
    rows: Vec<serde_json::Value>,
    skipped: &mut usize,
    in_group: bool,
) -> Vec<UserMenuItem> {
    let mut out = Vec::new();
    for value in rows {
        let Ok(row) = serde_json::from_value::<Row>(value) else {
            *skipped += 1;
            continue;
        };
        if row.separator {
            // Never first, never twice in a row.
            if !matches!(out.last(), None | Some(UserMenuItem::Separator)) {
                out.push(UserMenuItem::Separator);
            }
        } else if row.items.is_some() && in_group {
            // The menu opens one submenu level; a group inside a group would
            // show but never open, its commands out of reach.
            *skipped += 1;
        } else if let Some(items) = row.items {
            let items = parse_rows(items, skipped, true);
            match row.label {
                Some(label) if !items.is_empty() => out.push(UserMenuItem::Group { label, items }),
                _ => *skipped += 1,
            }
        } else if let Some(id) = row.command {
            match Command::from_id(&id) {
                Some(command) => out.push(UserMenuItem::Command {
                    label: row.label,
                    command,
                    pinned: row.pinned,
                }),
                None => *skipped += 1,
            }
        } else {
            *skipped += 1;
        }
    }
    while matches!(out.last(), Some(UserMenuItem::Separator)) {
        out.pop();
    }
    out
}

/// The starter file, also the menu until the file exists.
pub const TEMPLATE: &str = r##"// croft user menu: the commands you use every day.
//
// Open it with "User Menu: Open" (Cmd+K Shift+M); the same commands also head
// the Command Palette, marked with a star. A row is one of:
//   { "command": "format_document" }                    a command, by its id
//   { "command": "toggle_bookmark", "label": "Mark" }   ... under your own label
//   { "command": "fold_all", "pinned": false }          ... kept out of the palette's star block
//   { "label": "Folding", "items": [ ... ] }            a submenu (one level)
//   { "separator": true }                               a divider
// Command ids are the ones keybindings.json uses (palette: Preferences: Open
// Keyboard Shortcuts lists them). Save the file; it is read each time the menu
// or the palette opens.
[
  { "command": "format_document" },
  { "command": "toggle_line_comment" },
  { "command": "go_to_symbol" },
  { "separator": true },
  { "label": "Bookmarks", "items": [
    { "command": "toggle_bookmark" },
    { "command": "next_bookmark" },
    { "command": "previous_bookmark" },
    { "command": "view_bookmarks" }
  ] },
  { "label": "Folding", "items": [
    { "command": "toggle_fold", "pinned": false },
    { "command": "fold_all", "pinned": false },
    { "command": "unfold_all", "pinned": false }
  ] },
  { "label": "Lines", "items": [
    { "command": "move_line_up", "pinned": false },
    { "command": "move_line_down", "pinned": false },
    { "command": "copy_line_down", "pinned": false },
    { "command": "delete_line", "pinned": false },
    { "command": "sort_lines_ascending", "pinned": false }
  ] },
  { "separator": true },
  { "command": "open_user_menu_json", "label": "Edit this menu…", "pinned": false }
]
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starter_menu_parses_with_nothing_skipped() {
        let menu = UserMenu::from_json(TEMPLATE);
        assert_eq!(menu.problem, None);
        assert_eq!(menu.skipped, 0, "every id in the template is a command");
        assert!(!menu.is_empty());
    }

    #[test]
    fn a_group_a_label_and_a_divider_parse() {
        let menu = UserMenu::from_json(
            r#"[{"command":"fold_all","label":"Fold"},{"separator":true},
                {"label":"G","items":[{"command":"toggle_fold"}]}]"#,
        );
        assert_eq!(menu.skipped, 0);
        assert_eq!(menu.items.len(), 3);
        assert!(matches!(
            &menu.items[0],
            UserMenuItem::Command { label: Some(l), command: Command::FoldAll, pinned: true } if l == "Fold"
        ));
        assert_eq!(menu.items[1], UserMenuItem::Separator);
        assert!(
            matches!(&menu.items[2], UserMenuItem::Group { label, items } if label == "G" && items.len() == 1)
        );
    }

    #[test]
    fn a_group_inside_a_group_is_skipped_and_counted() {
        let menu = UserMenu::from_json(
            r#"[{"label":"Outer","items":[{"command":"toggle_fold"},
                  {"label":"Inner","items":[{"command":"fold_all"}]}]},
                {"command":"fold_all"}]"#,
        );
        assert_eq!(menu.skipped, 1, "the inner group, once");
        assert_eq!(menu.items.len(), 2);
        assert!(matches!(
            &menu.items[0],
            UserMenuItem::Group { items, .. }
                if items.len() == 1 && matches!(items[0], UserMenuItem::Command { .. })
        ));
    }

    #[test]
    fn an_unknown_command_or_a_bad_row_is_skipped_and_counted() {
        let menu = UserMenu::from_json(
            r#"[{"command":"no_such_command"},{"command":7},{"label":"empty","items":[]},
                {"command":"fold_all"}]"#,
        );
        assert_eq!(menu.skipped, 3);
        assert_eq!(menu.items.len(), 1);
    }

    #[test]
    fn dividers_never_lead_trail_or_double_up() {
        let menu = UserMenu::from_json(
            r#"[{"separator":true},{"separator":true},{"command":"fold_all"},
                {"separator":true},{"separator":true}]"#,
        );
        assert_eq!(menu.items.len(), 1);
    }

    #[test]
    fn a_file_that_does_not_parse_reports_it_and_has_no_items() {
        let menu = UserMenu::from_json("{ not json");
        assert!(menu.problem.is_some());
        assert!(menu.is_empty());
    }

    #[test]
    fn the_palette_pins_every_command_unless_the_row_opts_out() {
        let menu = UserMenu::from_json(
            r#"[{"command":"fold_all"},{"command":"toggle_fold","pinned":false},
                {"label":"G","items":[{"command":"unfold_all"},{"command":"fold_all"}]}]"#,
        );
        assert_eq!(menu.pinned(), vec![Command::FoldAll, Command::UnfoldAll]);
    }
}
