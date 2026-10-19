//! One-shot import of a VS Code user profile (#351): settings, keybindings
//! and snippets.
//!
//! Most of what a VS Code user misses on day one is their own configuration,
//! and croft already reads VS Code's formats in several places (the
//! `.vscode/settings.json` subset, `launch.json`, `tasks.json`,
//! `.code-workspace`). This finishes the job for the USER-level files.
//!
//! # Three conversions of very different difficulty
//!
//! **Snippets** are nearly free: croft's `snippets.json` already mirrors
//! VS Code's global snippets file, tab-stop syntax included. The only real
//! work is that VS Code splits them per language by FILENAME
//! (`snippets/python.json`), which croft carries as a `scope` field.
//!
//! **Keybindings** share a file shape and disagree on every command id, so
//! they convert through a table. An unmapped id is dropped and named, never
//! guessed at: a chord silently bound to the wrong action is worse than one
//! that did not come across.
//!
//! **Settings** is where the two editors genuinely differ. Only keys croft has
//! a real equivalent of are mapped; everything else is listed as unmapped
//! rather than dropped silently, because "my settings imported" and "my
//! settings are gone" must not look the same.
//!
//! # Merge, never overwrite
//!
//! An existing croft value always wins, and the conflict is reported. Import
//! is therefore idempotent: running it twice changes nothing the second time.
//! Someone who has already tuned croft must not lose that by curiosity.

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A VS Code-family user directory, by product name. Cursor and VSCodium are
/// the same format under a different folder, and someone arriving from one of
/// them is exactly the user this exists for.
const USER_DIRS: &[(&str, &str)] = &[
    ("VS Code", "Code"),
    ("VS Code Insiders", "Code - Insiders"),
    ("VSCodium", "VSCodium"),
    ("Cursor", "Cursor"),
    ("Windsurf", "Windsurf"),
];

/// Candidate user directories that exist on this machine, most standard first.
pub fn discover_profiles() -> Vec<(String, PathBuf)> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        // macOS keeps them under Application Support; Linux under the XDG
        // config dir. Probing both is cheaper than deciding by target_os and
        // works when a home directory is shared between them.
        roots.push(home.join("Library").join("Application Support"));
        roots.push(home.join(".config"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        roots.push(appdata);
    }
    let mut found = Vec::new();
    for (label, dir) in USER_DIRS {
        for root in &roots {
            let candidate = root.join(dir).join("User");
            if candidate.is_dir()
                && !found
                    .iter()
                    .any(|(_, p): &(String, PathBuf)| p == &candidate)
            {
                found.push(((*label).to_string(), candidate));
            }
        }
    }
    found
}

/// One VS Code settings key croft understands, and how it lands.
struct SettingMap {
    vscode: &'static str,
    /// Croft pref key, or `None` when the source key sets more than one.
    croft: &'static str,
    convert: fn(&Value) -> Option<Value>,
}

fn as_bool(v: &Value) -> Option<Value> {
    v.as_bool().map(Value::from)
}

fn negated(v: &Value) -> Option<Value> {
    v.as_bool().map(|b| Value::from(!b))
}

/// VS Code writes this one as a bool OR as `"on"` / `"off"` / `"auto"`
/// depending on the setting's age.
fn off_means_disabled(v: &Value) -> Option<Value> {
    match v {
        Value::Bool(b) => Some(Value::from(!*b)),
        Value::String(s) => Some(Value::from(s == "off")),
        _ => None,
    }
}

fn never_means_disabled(v: &Value) -> Option<Value> {
    v.as_str().map(|s| Value::from(s == "never"))
}

fn none_means_disabled(v: &Value) -> Option<Value> {
    v.as_str().map(|s| Value::from(s == "none"))
}

fn as_word_suggestions(v: &Value) -> Option<Value> {
    // VS Code took a boolean before 1.84 (`true` was matching documents) and
    // takes one of four ids since; croft keeps the ids.
    let mapped = match v {
        Value::Bool(true) => "matchingDocuments",
        Value::Bool(false) => "off",
        Value::String(s) => match s.as_str() {
            "off" | "currentDocument" | "matchingDocuments" | "allDocuments" => s.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    Some(Value::String(mapped.to_string()))
}

fn as_whitespace_mode(v: &Value) -> Option<Value> {
    // VS Code has five whitespace modes; croft has three (WhitespaceMode:
    // none, selection, all). Only ids croft can actually represent are
    // emitted.
    //
    // An earlier version mapped `selection` and `trailing` onto "boundary",
    // which croft has NO variant for. `selection` still behaved correctly by
    // ACCIDENT: `WhitespaceMode::from_pref` sends anything unrecognised to
    // Selection, so a wrong id landed on the right mode and would have broken
    // the moment that fallback changed.
    let s = v.as_str()?;
    let mapped = match s {
        "none" | "selection" | "all" => s,
        // Neither has a croft mode: `boundary` marks whitespace BETWEEN words
        // and `trailing` only at line ends, so rendering either as `all`
        // would mark whitespace the user deliberately kept. The honest
        // nearest is none, and the caller reports the loss.
        "boundary" | "trailing" => "none",
        _ => return None,
    };
    Some(Value::from(mapped))
}

/// VS Code's `editor.rulers` (#1524): each ruler is a column number or an
/// object with a `column` (and a `color` croft does not carry). An entry
/// that is neither, or not a usable column, is left out.
fn as_rulers(v: &Value) -> Option<Value> {
    let columns: Vec<u16> = v
        .as_array()?
        .iter()
        .filter_map(|r| r.as_u64().or_else(|| r.get("column")?.as_u64()))
        .filter_map(|c| u16::try_from(c).ok())
        .filter(|&c| c > 0)
        .collect();
    Some(Value::from(columns))
}

/// `editor.lineNumbers` (#1576): VS Code's four modes are croft's.
fn as_line_numbers(v: &Value) -> Option<Value> {
    v.as_str()
        .filter(|s| matches!(*s, "on" | "off" | "relative" | "interval"))
        .map(Value::from)
}

fn as_scrollback(v: &Value) -> Option<Value> {
    v.as_u64().map(Value::from)
}

/// VS Code's `files.exclude` / `search.exclude` object (#1345): the globs
/// set to `true`. A `when` clause hides a file only next to a sibling croft
/// does not look for, so it is left out: a dropped entry shows a file, a
/// misread one would hide it.
///
/// VS Code matches these globs against the workspace-relative path, so a
/// bare `build` is the root's `build` only, where croft's own bare glob
/// matches at any depth and hid `scripts/build/` too (#1479). Such a glob
/// crosses anchored, as `/build`. A glob that already starts with `/`
/// matches no workspace-relative path in VS Code, so it is left out rather
/// than read as croft's root anchor, which would hide a file VS Code shows.
///
/// A glob set to `false` takes back the same glob from a lower layer, as in
/// VS Code, so it crosses as `!glob` (#1494). A glob that itself starts
/// with `!` is a literal one in VS Code, so it crosses escaped, as `\!glob`.
fn as_enabled_globs(v: &Value) -> Option<Value> {
    let obj = v.as_object()?;
    Some(Value::from(
        obj.iter()
            .filter(|(glob, on)| on.as_bool().is_some() && !glob.starts_with('/'))
            .map(|(glob, on)| {
                let glob = if glob.starts_with('!') && glob.contains('/') {
                    format!("\\{glob}")
                } else if glob.contains('/') || glob.starts_with("**") {
                    glob.clone()
                } else {
                    format!("/{glob}")
                };
                if on.as_bool() == Some(true) {
                    glob
                } else {
                    format!("!{glob}")
                }
            })
            .collect::<Vec<String>>(),
    ))
}

/// A non-empty string, or an explicit `null`, which VS Code accepts for
/// `editor.defaultFormatter` to clear one named in a lower layer.
fn as_string(v: &Value) -> Option<Value> {
    if v.is_null() {
        return Some(Value::Null);
    }
    v.as_str().filter(|s| !s.is_empty()).map(Value::from)
}

/// The croft settings a VS Code `settings.json` key maps to, for telling a
/// user who wrote the VS Code name in croft's config which keys to use
/// (#1607). `files.autoSave` sets two: its focus and window modes are
/// `auto_save_on_focus_change`, not a delayed `auto_save`.
pub fn croft_keys_for(vscode_key: &str) -> Option<&'static [&'static str]> {
    match vscode_key {
        "files.autoSave" => Some(&["auto_save", "auto_save_on_focus_change"]),
        "workbench.colorTheme" => Some(&["theme"]),
        _ => SETTINGS
            .iter()
            .find(|m| m.vscode == vscode_key)
            .map(|m| std::slice::from_ref(&m.croft)),
    }
}

const SETTINGS: &[SettingMap] = &[
    SettingMap {
        vscode: "editor.formatOnSave",
        croft: "format_on_save",
        convert: as_bool,
    },
    SettingMap {
        vscode: "editor.defaultFormatter",
        croft: "default_formatter",
        convert: as_string,
    },
    SettingMap {
        vscode: "files.insertFinalNewline",
        croft: "insert_final_newline",
        convert: as_bool,
    },
    SettingMap {
        vscode: "files.trimTrailingWhitespace",
        croft: "trim_trailing_whitespace",
        convert: as_bool,
    },
    SettingMap {
        vscode: "files.trimFinalNewlines",
        croft: "trim_final_newlines",
        convert: as_bool,
    },
    SettingMap {
        vscode: "editor.formatOnType",
        croft: "format_on_type",
        convert: as_bool,
    },
    SettingMap {
        vscode: "editor.bracketPairColorization.enabled",
        croft: "disable_bracket_colors",
        convert: negated,
    },
    SettingMap {
        vscode: "editor.guides.indentation",
        croft: "disable_indent_guides",
        convert: negated,
    },
    SettingMap {
        vscode: "editor.lineNumbers",
        croft: "line_numbers",
        convert: as_line_numbers,
    },
    SettingMap {
        vscode: "editor.inlayHints.enabled",
        croft: "disable_inlay_hints",
        convert: off_means_disabled,
    },
    SettingMap {
        vscode: "editor.autoClosingBrackets",
        croft: "disable_auto_close_pairs",
        convert: never_means_disabled,
    },
    SettingMap {
        vscode: "debug.inlineValues",
        croft: "disable_inline_values",
        convert: off_means_disabled,
    },
    SettingMap {
        vscode: "debug.saveBeforeStart",
        croft: "disable_save_before_debug",
        convert: none_means_disabled,
    },
    SettingMap {
        vscode: "editor.renderWhitespace",
        croft: "render_whitespace",
        convert: as_whitespace_mode,
    },
    SettingMap {
        vscode: "editor.rulers",
        croft: "rulers",
        convert: as_rulers,
    },
    SettingMap {
        vscode: "editor.wordBasedSuggestions",
        croft: "word_based_suggestions",
        convert: as_word_suggestions,
    },
    SettingMap {
        vscode: "terminal.integrated.scrollback",
        croft: "terminal_scrollback",
        convert: as_scrollback,
    },
    SettingMap {
        vscode: "terminal.integrated.copyOnSelection",
        croft: "copy_on_select",
        convert: as_bool,
    },
    SettingMap {
        vscode: "files.exclude",
        croft: "files_exclude",
        convert: as_enabled_globs,
    },
    SettingMap {
        vscode: "search.exclude",
        croft: "search_exclude",
        convert: as_enabled_globs,
    },
];

/// VS Code command id to croft palette command id.
///
/// Deliberately partial. Every entry here is a command whose croft equivalent
/// does the same thing; a VS Code command with no true counterpart is left
/// out so the import reports it rather than binding the user's chord to
/// something that merely sounds similar.
/// `editor.action.showHover` is deliberately ABSENT, having been mapped and
/// then removed: it is not `peek_definition`, which opens a different thing
/// entirely. `editor.action.revealDefinition` was once mapped to
/// `mouse_go_to_definition_at_click`, which reads the last POINTER position;
/// it now maps to the caret-driven `go_to_definition` (#1212).
const COMMANDS: &[(&str, &str)] = &[
    ("workbench.action.files.save", "save_file"),
    // The GitHub Pull Requests extension's share commands, and the built-in
    // GitHub extension's Open on GitHub (#1610).
    ("issue.copyGithubPermalink", "git_copy_permalink"),
    ("issue.copyGithubHeadLink", "git_copy_head_link"),
    ("issue.openGithubPermalink", "git_open_on_remote"),
    ("github.openOnGitHub", "git_open_on_remote"),
    ("git.createTag", "git_create_tag"),
    ("git.pushTags", "git_push_tags"),
    ("git.deleteRemoteTag", "git_delete_remote_tag"),
    ("workbench.action.files.saveAs", "save_as"),
    ("workbench.action.files.revert", "revert_file"),
    ("workbench.action.files.saveAll", "save_all"),
    ("explorer.newFile", "new_file"),
    (
        "workbench.action.files.newUntitledFile",
        "new_untitled_file",
    ),
    ("explorer.newFolder", "new_folder"),
    ("git.viewStash", "git_view_stash"),
    ("editor.action.selectAll", "select_all"),
    ("workbench.action.quickOpen", "quick_open"),
    ("workbench.action.editor.nextChange", "next_change"),
    ("workbench.action.editor.previousChange", "previous_change"),
    ("workbench.action.gotoSymbol", "go_to_symbol"),
    ("workbench.action.showAllSymbols", "go_to_workspace_symbol"),
    ("workbench.action.closeActiveEditor", "close_editor"),
    (
        "workbench.action.reopenClosedEditor",
        "reopen_closed_editor",
    ),
    ("workbench.action.closeOtherEditors", "close_other_editors"),
    (
        "workbench.action.closeEditorsToTheRight",
        "close_editors_to_the_right",
    ),
    (
        "workbench.action.closeUnmodifiedEditors",
        "close_saved_editors",
    ),
    ("workbench.action.closeAllEditors", "close_all_editors"),
    ("workbench.action.pinEditor", "pin_editor"),
    ("workbench.action.unpinEditor", "unpin_editor"),
    ("workbench.action.keepEditor", "keep_editor"),
    (
        "workbench.action.moveEditorLeftInGroup",
        "move_editor_left_in_group",
    ),
    (
        "workbench.action.moveEditorRightInGroup",
        "move_editor_right_in_group",
    ),
    (
        "workbench.action.moveEditorToNextGroup",
        "move_editor_to_next_group",
    ),
    (
        "workbench.action.moveEditorToPreviousGroup",
        "move_editor_to_previous_group",
    ),
    ("workbench.action.splitEditor", "split_editor"),
    (
        "workbench.action.toggleSidebarVisibility",
        "toggle_side_bar",
    ),
    (
        "workbench.action.toggleAuxiliaryBar",
        "toggle_secondary_side_bar",
    ),
    (
        "workbench.action.terminal.toggleTerminal",
        "toggle_terminal",
    ),
    ("workbench.action.terminal.new", "new_terminal"),
    ("workbench.action.terminal.focusNext", "focus_next_terminal"),
    (
        "workbench.action.terminal.focusPrevious",
        "focus_previous_terminal",
    ),
    ("workbench.action.focusLeftGroup", "focus_left_editor_group"),
    (
        "workbench.action.focusRightGroup",
        "focus_right_editor_group",
    ),
    ("workbench.action.terminal.focus", "focus_terminal"),
    ("workbench.action.terminal.clear", "terminal_clear"),
    ("workbench.action.terminal.rename", "terminal_rename"),
    ("workbench.action.terminal.kill", "terminal_kill"),
    ("workbench.action.terminal.split", "terminal_split"),
    (
        "workbench.action.terminal.newWithProfile",
        "terminal_new_with_profile",
    ),
    (
        "workbench.action.terminal.runSelectedText",
        "run_selected_text",
    ),
    ("workbench.actions.view.problems", "show_problems"),
    ("workbench.action.toggleZenMode", "toggle_zen_mode"),
    (
        "workbench.action.toggleActivityBarVisibility",
        "toggle_activity_bar",
    ),
    (
        "workbench.action.toggleStatusbarVisibility",
        "toggle_status_bar",
    ),
    (
        "workbench.action.toggleSidebarPosition",
        "toggle_side_bar_position",
    ),
    ("workbench.action.customizeLayout", "customize_layout"),
    ("workbench.view.explorer", "show_explorer"),
    ("breadcrumbs.focusAndSelect", "breadcrumbs_focus_and_select"),
    ("workbench.view.search", "show_search"),
    ("workbench.action.replaceInFiles", "replace_in_files"),
    ("toggleSearchCaseSensitive", "search_toggle_match_case"),
    ("toggleSearchWholeWord", "search_toggle_whole_word"),
    ("toggleSearchRegex", "search_toggle_regex"),
    (
        "workbench.action.search.toggleQueryDetails",
        "search_toggle_details",
    ),
    ("workbench.view.scm", "show_source_control"),
    ("workbench.view.debug", "show_run_debug"),
    ("workbench.view.testing", "show_testing"),
    ("workbench.view.extensions", "show_extensions"),
    ("workbench.action.openSettings", "open_settings"),
    ("workbench.action.openSettingsJson", "open_settings_json"),
    (
        "workbench.action.openGlobalKeybindingsFile",
        "open_keybindings_json",
    ),
    (
        "workbench.action.openGlobalKeybindings",
        "keyboard_shortcuts",
    ),
    ("editor.action.formatDocument", "format_document"),
    ("editor.action.formatSelection", "format_selection"),
    ("editor.action.commentLine", "toggle_line_comment"),
    ("editor.action.blockComment", "toggle_block_comment"),
    ("editor.action.quickFix", "quick_fix"),
    ("editor.action.peekDefinition", "peek_definition"),
    ("editor.action.revealDefinition", "go_to_definition"),
    ("editor.action.goToReferences", "go_to_references"),
    ("editor.action.revealDeclaration", "go_to_declaration"),
    ("editor.action.goToTypeDefinition", "go_to_type_definition"),
    ("editor.action.goToImplementation", "go_to_implementations"),
    ("editor.action.rename", "rename_symbol"),
    ("editor.action.startFindReplaceAction", "replace_in_file"),
    ("toggleFindInSelection", "toggle_find_in_selection"),
    ("editor.action.moveLinesUpAction", "move_line_up"),
    ("editor.action.moveLinesDownAction", "move_line_down"),
    ("editor.action.deleteLines", "delete_line"),
    ("deleteAllRight", "kill_to_end_of_line"),
    ("editor.action.joinLines", "join_lines"),
    ("editor.action.insertLineAfter", "insert_line_below"),
    ("editor.action.insertLineBefore", "insert_line_above"),
    ("editor.action.transformToUppercase", "transform_upper"),
    ("editor.action.transformToLowercase", "transform_lower"),
    ("editor.action.transformToTitlecase", "transform_title"),
    (
        "editor.action.trimTrailingWhitespace",
        "trim_trailing_whitespace",
    ),
    ("editor.action.indentationToSpaces", "indentation_to_spaces"),
    ("editor.action.indentationToTabs", "indentation_to_tabs"),
    (
        "editor.emmet.action.wrapWithAbbreviation",
        "emmet_wrap_with_abbreviation",
    ),
    ("editor.emmet.action.removeTag", "emmet_remove_tag"),
    ("editor.emmet.action.updateTag", "emmet_update_tag"),
    ("editor.emmet.action.balanceOut", "emmet_balance_outward"),
    ("editor.emmet.action.balanceIn", "emmet_balance_inward"),
    ("editor.emmet.action.matchTag", "emmet_match_tag"),
    ("editor.action.insertCursorAbove", "add_cursor_above"),
    ("editor.action.insertCursorBelow", "add_cursor_below"),
    (
        "editor.action.addSelectionToNextFindMatch",
        "add_selection_to_next_match",
    ),
    ("editor.action.selectHighlights", "select_all_occurrences"),
    (
        "editor.action.insertCursorAtEndOfEachLineSelected",
        "add_cursors_to_line_ends",
    ),
    ("editor.action.smartSelect.expand", "expand_selection"),
    ("editor.action.smartSelect.shrink", "shrink_selection"),
    ("editor.action.jumpToBracket", "jump_to_bracket"),
    ("editor.action.selectToBracket", "select_to_bracket"),
    ("editor.foldAll", "fold_all"),
    ("editor.unfoldAll", "unfold_all"),
    ("editor.toggleFold", "toggle_fold"),
    ("markdown.showPreview", "toggle_markdown_preview"),
    ("workbench.action.tasks.build", "run_build_task"),
    ("workbench.action.tasks.runTask", "run_task"),
    ("workbench.action.tasks.reRunTask", "rerun_last_task"),
    ("testing.runAll", "testing_run_all"),
    ("testing.runCurrentFile", "testing_run_file"),
    ("testing.reRunFailTests", "testing_rerun_failed"),
    ("testing.reRunLastRun", "testing_rerun_last"),
    ("workbench.action.debug.start", "start_debugging"),
    ("workbench.action.debug.stop", "stop_debugging"),
    ("workbench.action.debug.restart", "restart_debugging"),
    ("workbench.action.debug.pause", "pause_debugging"),
    ("workbench.action.debug.stepOver", "step_over"),
    ("workbench.action.debug.continue", "continue"),
    ("workbench.action.debug.stepInto", "step_into"),
    ("workbench.action.debug.stepOut", "step_out"),
    ("editor.debug.action.stepIntoTargets", "step_into_target"),
    ("editor.debug.action.toggleBreakpoint", "toggle_breakpoint"),
    ("undo", "undo"),
    ("redo", "redo"),
    ("references-view.showCallHierarchy", "show_incoming_calls"),
    ("workbench.action.navigateBack", "navigate_back"),
    ("workbench.action.navigateForward", "navigate_forward"),
];

/// Where each VS Code-family product keeps its installed extensions: beside
/// its own dot-directory in the home, not under the user directory being
/// imported. Keyed by the product folder that holds `User`.
const EXTENSION_DIRS: &[(&str, &str)] = &[
    ("Code", ".vscode"),
    ("Code - Insiders", ".vscode-insiders"),
    ("VSCodium", ".vscode-oss"),
    ("Cursor", ".cursor"),
    ("Windsurf", ".windsurf"),
];

/// The extensions directories to search for the profile's colour theme.
///
/// A user directory that belongs to a known product yields that product's
/// directory alone, so a Cursor import does not pick up a same-named theme
/// from a stale VS Code install. Anything else (a `--from` pointing at a
/// copy) probes every product, most standard first.
pub fn theme_extension_dirs_under(home: &Path, user_dir: &Path) -> Vec<PathBuf> {
    let product = user_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str());
    if let Some(dot) = product.and_then(|p| {
        EXTENSION_DIRS
            .iter()
            .find(|(name, _)| *name == p)
            .map(|(_, dot)| *dot)
    }) {
        return vec![home.join(dot).join("extensions")];
    }
    EXTENSION_DIRS
        .iter()
        .map(|(_, dot)| home.join(dot).join("extensions"))
        .collect()
}

/// [`theme_extension_dirs_under`] for this machine's home.
pub fn theme_extension_dirs(user_dir: &Path) -> Vec<PathBuf> {
    // `discover_profiles` finds a Windows profile through APPDATA, where
    // HOME is usually unset; the extensions live under the user profile.
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match home.map(PathBuf::from) {
        Some(home) => theme_extension_dirs_under(&home, user_dir),
        None => Vec::new(),
    }
}

/// The `(name, version)` of an extension directory, `publisher.name-1.2.3`,
/// for ordering: name ascending, version descending, so the first match is
/// the newest install. VS Code prunes old versions lazily, so two side by
/// side is the ordinary state, and byte order would put `1.10.0` before
/// `1.9.0` and `2.0.0`.
fn extension_order(dir: &Path) -> (String, std::cmp::Reverse<Vec<u64>>) {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let (stem, version) = name.rsplit_once('-').unwrap_or((name, ""));
    let parts: Vec<u64> = version
        .split('.')
        .map(|p| {
            p.trim_end_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .unwrap_or(0)
        })
        .collect();
    (stem.to_string(), std::cmp::Reverse(parts))
}

/// Every theme JSON an installed extension contributes under `label`,
/// newest install of each extension first.
///
/// `workbench.colorTheme` holds the theme's picker label, or its `id` when
/// the extension declares one, so both are matched. A `path` that is
/// absolute or climbs out of its extension directory is skipped: the
/// manifest is trusted to describe its own files, not to name others.
pub fn find_installed_themes(label: &str, extension_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in extension_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut extensions: Vec<PathBuf> =
            entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        extensions.sort_by_key(|p| extension_order(p));
        for ext in extensions {
            let Ok(raw) = std::fs::read_to_string(ext.join("package.json")) else {
                continue;
            };
            let Ok(pkg) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let Some(themes) = pkg["contributes"]["themes"].as_array() else {
                continue;
            };
            for theme in themes {
                let named =
                    theme["id"].as_str() == Some(label) || theme["label"].as_str() == Some(label);
                let Some(rel) = theme["path"].as_str().filter(|_| named) else {
                    continue;
                };
                let rel = Path::new(rel);
                let escapes = rel.is_absolute()
                    || rel
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir));
                if !escapes {
                    found.push(ext.join(rel));
                }
            }
        }
    }
    found
}

/// The VS Code colour theme an import resolved: where it came from and what
/// `croft theme-import` made of it.
#[derive(Debug, Clone)]
pub struct ThemeSource {
    /// The `workbench.colorTheme` value.
    pub label: String,
    /// The theme JSON inside the installed extension.
    pub path: PathBuf,
    /// The croft theme, ready to install.
    pub converted: crate::vscode_theme::Converted,
}

/// What an import would do, or did.
#[derive(Debug, Default)]
pub struct Report {
    /// The colour theme, when `workbench.colorTheme` named an installed
    /// extension theme. Its id is also in `settings` under `theme`.
    pub theme: Option<ThemeSource>,
    /// Croft settings this import would write, as `key = value` lines.
    pub settings: BTreeMap<String, Value>,
    /// VS Code settings keys with no croft equivalent.
    pub unmapped_settings: Vec<String>,
    /// Croft keybinding rows, ready to serialise.
    pub keybindings: Vec<(String, String)>,
    /// VS Code keybinding rows dropped, with the reason.
    pub dropped_keybindings: Vec<String>,
    /// Snippet name to its croft entry.
    pub snippets: Map<String, Value>,
    /// Values croft already has, which the import left alone.
    pub conflicts: Vec<String>,
    /// Files croft DECLINED to write, and why. Separate from `conflicts`
    /// because they are a different fact: a conflict means croft kept what
    /// was already there, a refusal means croft wrote nothing at all. Folding
    /// them together let one heading serve two meanings, and let the closing
    /// line tell a user "nothing changed because nothing needed to" when the
    /// truth was "croft declined to write".
    pub refusals: Vec<String>,
    /// Anything that went wrong short of failing the import.
    pub warnings: Vec<String>,
    /// For each key in `snippets`, the VS Code name and scope it came from.
    snippet_origins: BTreeMap<String, (String, String)>,
}

impl Report {
    pub fn is_empty(&self) -> bool {
        self.settings.is_empty() && self.keybindings.is_empty() && self.snippets.is_empty()
    }
}

fn read_jsonc(path: &Path) -> Result<Option<Value>> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    // `crate::tasks::strip_jsonc` walks CHARS. The copy that used to live in
    // `workspace.rs` walked bytes and mangled any non-ASCII value (#396),
    // which a settings file carries routinely in paths and snippet bodies;
    // it was deleted in favour of this one rather than repaired.
    let stripped = crate::tasks::strip_jsonc(&raw);
    if stripped.trim().is_empty() {
        return Ok(None);
    }
    let value =
        serde_json::from_str(&stripped).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(value))
}

/// Map a VS Code settings object onto croft's keys.
///
/// THE mapping, used by both callers: this importer and the
/// `.vscode/settings.json` workspace layer in [`crate::config_layers`].
/// They had a table each, and the tables had already drifted (one treated
/// `files.autoSave: "onWindowChange"` as a save-on-focus-change, the other
/// ignored it), so the same file meant different things depending on which
/// path read it.
///
/// Returns the croft settings, the VS Code keys with no croft equivalent,
/// and any lossy conversions worth reporting.
pub fn map_settings(
    obj: &Map<String, Value>,
) -> (BTreeMap<String, Value>, Vec<String>, Vec<String>) {
    let mut settings: BTreeMap<String, Value> = BTreeMap::new();
    let mut unmapped: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for (key, value) in obj {
        if let Some(ids) = language_block_ids(key) {
            // `"[typescript]": { "editor.defaultFormatter": … }`, also as
            // `"[javascript][typescript]"` (#1499): the formatter each
            // language names. A grouped block keeps its whole key
            // (`javascript][typescript`) so a layer's grouped block does
            // not overwrite another layer's single-language one, which
            // VS Code ranks above it. An explicit `null` is kept as "" so
            // it still shadows the formatter named for every file. The
            // block's other keys map per language (#1440).
            let mut block = value.clone();
            if let Some(formatter) = block.get("editor.defaultFormatter").and_then(as_string) {
                if let Some(b) = block.as_object_mut() {
                    b.remove("editor.defaultFormatter");
                }
                let formatter = if formatter.is_null() {
                    Value::from("")
                } else {
                    formatter
                };
                let map = settings
                    .entry(String::from("language_formatters"))
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Some(map) = map.as_object_mut() {
                    map.insert(ids.join("]["), formatter);
                }
            }
            map_language_block(
                key,
                &ids,
                &block,
                &mut settings,
                &mut unmapped,
                &mut warnings,
            );
            continue;
        }
        match SETTINGS.iter().find(|m| m.vscode == key) {
            Some(map) => match (map.convert)(value) {
                Some(converted) => {
                    if key == "editor.renderWhitespace" && converted != *value {
                        warnings.push(format!(
                            "editor.renderWhitespace {value} has no croft mode; using {converted}"
                        ));
                    }
                    settings.insert(map.croft.to_string(), converted);
                }
                None => unmapped.push(format!(
                    "{key} (croft has {}, but not for this value: {value})",
                    map.croft
                )),
            },
            None => {
                // `files.autoSave` sets two croft prefs, so it is handled
                // here rather than in the one-to-one table.
                if let Some(mode) = value.as_str().filter(|_| key == "files.autoSave") {
                    // Only the values VS Code actually defines are
                    // authoritative. Treating any string as one meant a TYPO
                    // ("afterDelayy") emitted `false, false` and silently
                    // turned the user's auto-save off through the workspace
                    // layer; the mapper this replaced matched three values and
                    // emitted nothing for anything else.
                    let (delay, focus) = match mode {
                        "afterDelay" => (true, false),
                        "onFocusChange" | "onWindowChange" => (false, true),
                        "off" => (false, false),
                        _ => {
                            unmapped.push(format!("{key} (unrecognised value: {value})"));
                            continue;
                        }
                    };
                    settings.insert(String::from("auto_save"), Value::from(delay));
                    settings.insert(
                        String::from("auto_save_on_focus_change"),
                        Value::from(focus),
                    );
                    continue;
                }
                unmapped.push(key.clone());
            }
        }
    }
    unmapped.sort();
    unmapped.dedup();
    (settings, unmapped, warnings)
}

/// The language ids of a VS Code language block key (#1440): `[python]` is
/// `python`, `[javascript][typescript]` both. `None` for any other key.
fn language_block_ids(key: &str) -> Option<Vec<String>> {
    let inner = key.strip_prefix('[')?.strip_suffix(']')?;
    let ids: Vec<String> = inner.split("][").map(str::trim).map(String::from).collect();
    ids.iter()
        .all(|id| !id.is_empty() && !id.contains(['[', ']']))
        .then_some(ids)
}

/// Map one `"[id]": { … }` block (#1440) into `languages.<id>`, for each of
/// its ids. croft keys languages by VS Code's own ids, so they carry over
/// as they are. A key inside that croft maps but only globally, or does not
/// map at all, is reported with its block, as `[python] editor.rulers`.
fn map_language_block(
    key: &str,
    ids: &[String],
    value: &Value,
    settings: &mut BTreeMap<String, Value>,
    unmapped: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let Some(inner) = value.as_object() else {
        unmapped.push(format!("{key} (not an object: {value})"));
        return;
    };
    let (mapped, inner_unmapped, inner_warnings) = map_settings(inner);
    unmapped.extend(inner_unmapped.into_iter().map(|k| format!("{key} {k}")));
    warnings.extend(inner_warnings.into_iter().map(|w| format!("{key} {w}")));
    let mut block = Map::new();
    for (croft_key, v) in mapped {
        if crate::prefs::LANGUAGE_KEYS.contains(&croft_key.as_str()) {
            block.insert(croft_key, v);
        } else {
            let vscode = SETTINGS.iter().find(|m| m.croft == croft_key).map_or(
                if croft_key.starts_with("auto_save") {
                    "files.autoSave"
                } else {
                    croft_key.as_str()
                },
                |m| m.vscode,
            );
            unmapped.push(format!(
                "{key} {vscode} (croft has {croft_key} only for every language)"
            ));
        }
    }
    if block.is_empty() {
        return;
    }
    let languages = settings
        .entry(String::from("languages"))
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(languages) = languages {
        for id in ids {
            let entry = languages
                .entry(id.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(entry) = entry {
                entry.extend(block.clone());
            }
        }
    }
}

/// Add imported `languages` blocks to the ones a config.json already has
/// (#1440), a setting at a time: a language's setting the config already
/// holds is kept and reported, like a top-level one. True when any was
/// added.
fn merge_languages(
    current: &mut Map<String, Value>,
    imported: &Map<String, Value>,
    conflicts: &mut Vec<String>,
) -> bool {
    let mut changed = false;
    for (id, block) in imported {
        let Some(block) = block.as_object() else {
            continue;
        };
        let entry = current
            .entry(id.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        let Value::Object(entry) = entry else {
            conflicts.push(format!(
                "config.json languages.{id}: kept {entry}, VS Code had {}",
                Value::Object(block.clone())
            ));
            continue;
        };
        for (key, value) in block {
            match entry.get(key) {
                Some(kept) if kept != value => conflicts.push(format!(
                    "config.json languages.{id}.{key}: kept {kept}, VS Code had {value}"
                )),
                Some(_) => {}
                None => {
                    entry.insert(key.clone(), value.clone());
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Convert a VS Code `settings.json` document.
pub fn convert_settings(doc: &Value, report: &mut Report) {
    let Some(obj) = doc.as_object() else {
        report
            .warnings
            .push(String::from("settings.json is not a JSON object; skipped"));
        return;
    };
    let (settings, unmapped, warnings) = map_settings(obj);
    report.settings.extend(settings);
    report.unmapped_settings.extend(unmapped);
    report.warnings.extend(warnings);
    report.unmapped_settings.sort();
}

/// Convert a VS Code `keybindings.json` document.
pub fn convert_keybindings(doc: &Value, report: &mut Report) {
    let Some(rows) = doc.as_array() else {
        report.warnings.push(String::from(
            "keybindings.json is not a JSON array; skipped",
        ));
        return;
    };
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for row in rows {
        let Some(command) = row.get("command").and_then(Value::as_str) else {
            continue;
        };
        let Some(key) = row.get("key").and_then(Value::as_str) else {
            continue;
        };
        // A leading `-` is VS Code's "remove this default binding". croft's
        // defaults are its own, so there is nothing to remove.
        if let Some(removed) = command.strip_prefix('-') {
            report.dropped_keybindings.push(format!(
                "{key}: unbinding {removed} has no croft equivalent"
            ));
            continue;
        }
        // croft's `Chord::parse` splits on `+` only: a VS Code chord SEQUENCE
        // ("ctrl+k z") is not a chord croft can express, so writing one lands
        // a row croft skips with a warning at load. Reported here instead, so
        // every row this import writes is one croft can read.
        if key.split_whitespace().count() > 1 {
            report
                .dropped_keybindings
                .push(format!("{key}: croft has no multi-key chord sequences"));
            continue;
        }
        match COMMANDS.iter().find(|(vs, _)| *vs == command) {
            Some((_, croft)) => {
                // A later row wins in VS Code too.
                seen.insert(key.to_string(), (*croft).to_string());
            }
            None => report
                .dropped_keybindings
                .push(format!("{key}: no croft command matches {command}")),
        }
    }
    report.keybindings = seen.into_iter().collect();
    report.dropped_keybindings.sort();
}

/// Convert one VS Code snippets file. `language` comes from the file name for
/// `snippets/<language>.json`, and is `None` for a `.code-snippets` file,
/// whose entries carry their own scope.
pub fn convert_snippets(doc: &Value, language: Option<&str>, report: &mut Report) {
    let Some(obj) = doc.as_object() else {
        report.warnings.push(String::from(
            "a snippets file is not a JSON object; skipped",
        ));
        return;
    };
    for (name, body) in obj {
        let Some(entry) = body.as_object() else {
            continue;
        };
        if entry.get("prefix").is_none() || entry.get("body").is_none() {
            report
                .warnings
                .push(format!("snippet {name:?} has no prefix or body; skipped"));
            continue;
        }
        let mut out = entry.clone();
        // croft carries the language in a `scope` field; VS Code carries it
        // in the FILE NAME for per-language snippets, so it is recovered here
        // or the snippet would silently widen to every language.
        if let Some(lang) = language {
            out.entry("scope")
                .or_insert_with(|| Value::from(lang.to_string()));
        }
        let key = snippet_key(name, &out, report);
        report.snippets.insert(key, Value::Object(out));
    }
}

/// The scope a converted snippet is for, as its key names it: its `scope`
/// field, or every language.
fn snippet_scope(snippet: &serde_json::Map<String, Value>) -> String {
    snippet
        .get("scope")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("all languages")
        .to_string()
}

/// The key `snippet`, named `name` in its VS Code file, goes under in
/// croft's one snippets map (#1178). VS Code scopes a name to its file, so
/// `main` in python.json and in rust.json are two snippets: when another
/// scope already holds the name, both are keyed `name (scope)`. A second
/// one for the same scope replaces the first, and the report says so.
/// Which name and scope each key came from is tracked rather than read back
/// from the key's text, so a snippet literally named `main (python)` is never
/// taken for a generated key and overwritten.
fn snippet_key(
    name: &str,
    snippet: &serde_json::Map<String, Value>,
    report: &mut Report,
) -> String {
    let scope = snippet_scope(snippet);
    let origin = (name.to_string(), scope.clone());
    if let Some(key) = report
        .snippet_origins
        .iter()
        .find(|(_, o)| **o == origin)
        .map(|(k, _)| k.clone())
    {
        report.warnings.push(format!(
            "snippet {name:?} is defined twice for {scope}; kept the later one"
        ));
        return key;
    }
    let shared = report.snippet_origins.values().any(|(n, _)| n == name);
    let key = if shared {
        // The one still under the bare name moves to its own qualified key.
        if let Some((_, prev_scope)) = report.snippet_origins.remove(name) {
            let prev = report.snippets.remove(name).expect("origin tracks a key");
            let moved = free_snippet_key(report, format!("{name} ({prev_scope})"));
            report.snippets.insert(moved.clone(), prev);
            report
                .snippet_origins
                .insert(moved, (name.to_string(), prev_scope));
        }
        free_snippet_key(report, format!("{name} ({scope})"))
    } else {
        free_snippet_key(report, name.to_string())
    };
    report.snippet_origins.insert(key.clone(), origin);
    key
}

/// `base`, or `base 2`, `base 3`, ... when a snippet with another name or
/// scope already holds it.
fn free_snippet_key(report: &Report, base: String) -> String {
    if !report.snippets.contains_key(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|k| !report.snippets.contains_key(k))
        .expect("an unused key exists")
}

/// Turn `workbench.colorTheme` into a croft theme, when it can be.
///
/// The setting is the one the user sees all day, and listing it as "croft
/// has no equivalent" when the theme is sitting in their extensions
/// directory is the wrong answer. When the label names an installed
/// extension theme, it goes through the same conversion as `croft
/// theme-import` and croft's `theme` setting follows the converted id. VS
/// Code's built-in themes (Dark+, Light Modern, ...) live inside the app
/// bundle rather than the extensions directory, so they are not found; the
/// key stays unmapped and a warning says how to import the file by hand.
fn resolve_color_theme(label: &str, extension_dirs: &[PathBuf], report: &mut Report) {
    let mut matches = find_installed_themes(label, extension_dirs).into_iter();
    let Some(path) = matches.next() else {
        report.warnings.push(format!(
            "workbench.colorTheme {label:?} is not an installed extension theme (a built-in VS \
             Code theme lives in the app, not the extensions directory); to bring it over, run \
             `croft theme-import <theme.json>` on its file"
        ));
        return;
    };
    // A label is not unique across extensions (several ship a "Dark"), so
    // say which one was taken when there was a choice.
    let alternates: Vec<String> = matches.map(|p| p.display().to_string()).collect();
    if !alternates.is_empty() {
        report.warnings.push(format!(
            "workbench.colorTheme {label:?} matches more than one installed theme; took {} \
             and ignored {}",
            path.display(),
            alternates.join(", ")
        ));
    }
    match crate::vscode_theme::convert_file(&path, None) {
        Ok(converted) => {
            report
                .settings
                .insert(String::from("theme"), Value::from(converted.id.clone()));
            report
                .unmapped_settings
                .retain(|k| k != "workbench.colorTheme");
            report.theme = Some(ThemeSource {
                label: label.to_string(),
                path,
                converted,
            });
        }
        Err(err) => report.warnings.push(format!(
            "workbench.colorTheme {label:?} was found at {} but could not be converted: {err:#}",
            path.display()
        )),
    }
}

/// Read a whole VS Code user directory into a report.
pub fn scan_profile(dir: &Path) -> Result<Report> {
    scan_profile_with_extensions(dir, &theme_extension_dirs(dir))
}

/// [`scan_profile`] with explicit extension directories to resolve the
/// colour theme from.
pub fn scan_profile_with_extensions(dir: &Path, extension_dirs: &[PathBuf]) -> Result<Report> {
    let mut report = Report::default();
    if let Some(doc) = read_jsonc(&dir.join("settings.json"))? {
        convert_settings(&doc, &mut report);
        if let Some(label) = doc["workbench.colorTheme"].as_str() {
            resolve_color_theme(label, extension_dirs, &mut report);
        }
    }
    if let Some(doc) = read_jsonc(&dir.join("keybindings.json"))? {
        convert_keybindings(&doc, &mut report);
    }
    // VSCodeVim maps to croft's built-in Vim mode (#1286): a Vim user who
    // imports their profile starts croft in it rather than typing `dd`
    // into the first file they open.
    if vscodevim_installed(extension_dirs) {
        report
            .settings
            .insert(String::from("vim_mode"), Value::Bool(true));
    }
    let snippets_dir = dir.join("snippets");
    if snippets_dir.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&snippets_dir)
            .with_context(|| format!("reading {}", snippets_dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        // Deterministic order: two files defining the same snippet name must
        // resolve the same way on every run, or the import is not idempotent.
        entries.sort();
        for path in entries {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let language = match ext {
                "json" => path.file_stem().and_then(|s| s.to_str()),
                "code-snippets" => None,
                _ => continue,
            };
            if let Some(doc) = read_jsonc(&path)? {
                convert_snippets(&doc, language, &mut report);
            }
        }
    }
    Ok(report)
}

/// Whether VSCodeVim (`vscodevim.vim`) is installed in any of
/// `extension_dirs`, each holding `publisher.name-version` directories.
fn vscodevim_installed(extension_dirs: &[PathBuf]) -> bool {
    extension_dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(|e| e.ok())
        .any(|e| e.path().is_dir() && extension_order(&e.path()).0 == "vscodevim.vim")
}

/// Whether `path` carries JSONC extras croft would destroy by rewriting it:
/// comments, or the trailing commas of a hand-edited file.
///
/// Named for extras rather than comments because that is what it detects. It
/// compares the raw text against the stripped text, which differs for a
/// trailing comma as much as for a comment, and calling it `carries_comments`
/// produced a refusal telling the user their comments were at risk in a file
/// that had none.
///
/// A merge here parses JSONC and writes back strict JSON, which drops every
/// comment. That is not hypothetical: croft SEEDS `keybindings.json` and
/// `snippets.json` from commented templates (`keymap::TEMPLATE`,
/// `snippets::TEMPLATE`), so the common case is a file whose explanatory
/// header this import would silently delete. Such a file is left alone and
/// its additions reported instead, which is the same posture as a value
/// conflict: croft says what it did not do rather than doing it quietly.
fn carries_jsonc_extras(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    raw != crate::tasks::strip_jsonc(&raw)
}

/// Read a config file for merging, turning a PARSE failure into a per-file
/// refusal rather than an aborted run.
///
/// `read_jsonc`'s error propagated through `?`, so a single stray comma in
/// `config.json` stopped the whole import before it reached keybindings or
/// snippets. A stray comma is far likelier than the wrong JSON shape, so
/// the rarer half was handled and the common one was not.
///
/// `None` means "nothing usable here": either absent, which is ordinary, or
/// unreadable, which is reported. The caller distinguishes them by whether a
/// refusal was recorded.
fn read_or_refuse(path: &Path, report: &mut Report) -> (Option<Value>, bool) {
    match read_jsonc(path) {
        Ok(v) => (v, false),
        Err(e) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            report.refusals.push(format!(
                "{name} could not be read ({e}), so it was left untouched"
            ));
            // Unusable is NOT absent. Falling through to the absent path
            // started the merge from an empty map and wrote a fresh file over
            // the one croft could not read: the same data loss the
            // wrong-shape guard exists to prevent.
            (None, true)
        }
    }
}

/// Merge a report into croft's config files, leaving existing values alone.
///
/// Returns the files written. An existing value is never replaced: someone
/// who has already tuned croft must not lose it to a one-shot import, and
/// leaving it alone is also what makes a second run a no-op.
pub fn apply(report: &mut Report) -> Result<Vec<PathBuf>> {
    apply_into_dirs(
        &crate::prefs::config_dir(),
        &crate::lsp::manifest::user_extensions_dir(),
        report,
    )
}

/// [`apply_into_dirs`] with the theme installed under `<dir>/extensions`,
/// for tests.
#[cfg(test)]
pub fn apply_into(dir: &Path, report: &mut Report) -> Result<Vec<PathBuf>> {
    apply_into_dirs(dir, &dir.join("extensions"), report)
}

/// [`apply`] into an explicit config directory and extensions directory.
///
/// Split out so a test can drive the REAL write path. The previous test
/// asserted on `carries_comments`, the predicate, and never called `apply`:
/// unwiring the guard at all three call sites left every test passing while
/// the import destroyed croft's own commented templates. Nothing here
/// touches the environment, because the test binary runs its tests on
/// threads of one process.
pub fn apply_into_dirs(
    dir: &Path,
    extensions_dir: &Path,
    report: &mut Report,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut written = Vec::new();

    // The theme goes in first: `settings` already carries `theme = <id>`,
    // and a config that names a theme whose manifest is missing is worse
    // than one that names none.
    if let Some(theme) = &report.theme {
        let installed = crate::vscode_theme::install_into(extensions_dir, &theme.converted)
            .with_context(|| format!("installing theme {:?}", theme.label))?;
        written.push(installed.path);
    }

    let mut wrong_shape;
    if !report.settings.is_empty() {
        let path = dir.join("config.json");
        if carries_jsonc_extras(&path) {
            report.refusals.push(String::from(
                "config.json: left untouched, because rewriting it would drop its comments or hand formatting. Add the entries above by hand.",
            ));
        } else {
            // "Parsed, but not the expected shape" is not "absent". Treating
            // them alike OVERWROTE a config.json holding an array or a scalar
            // with a fresh object and discarded the user's content silently,
            // which is the opposite of this module's stated posture.
            let (doc, unusable) = read_or_refuse(&path, report);
            wrong_shape = unusable;
            let mut existing: Map<String, Value> = match doc {
                Some(Value::Object(m)) => m,
                Some(_) => {
                    report.refusals.push(String::from(
                        "config.json is not a JSON object, so it was left untouched",
                    ));
                    wrong_shape = true;
                    Map::new()
                }
                None => Map::new(),
            };
            let mut changed = false;
            for (key, value) in &report.settings {
                if key == "languages"
                    && let (Some(Value::Object(current)), Value::Object(imported)) =
                        (existing.get_mut(key), value)
                {
                    changed |= merge_languages(current, imported, &mut report.conflicts);
                    continue;
                }
                if let Some(current) = existing.get(key) {
                    if current != value {
                        // The converted theme is installed either way (it is
                        // selectable), but the config still names the old
                        // one; a plain "kept X" reads as if nothing happened.
                        report.conflicts.push(if key == "theme" && report.theme.is_some() {
                            format!(
                                "config.json theme: kept {current}; the converted theme {value} \
                                 is installed and selectable under the gear menu, set \
                                 theme = {value} to switch"
                            )
                        } else {
                            format!("config.json {key}: kept {current}, VS Code had {value}")
                        });
                    }
                    continue;
                }
                existing.insert(key.clone(), value.clone());
                changed = true;
            }
            if changed && !wrong_shape {
                write_json(&path, &Value::Object(existing))?;
                written.push(path);
            }
        }
    }

    if !report.keybindings.is_empty() {
        let path = dir.join("keybindings.json");
        if carries_jsonc_extras(&path) {
            report.refusals.push(String::from(
                "keybindings.json: left untouched, because rewriting it would drop its comments or hand formatting. Add the entries above by hand.",
            ));
        } else {
            let (doc, unusable) = read_or_refuse(&path, report);
            wrong_shape = unusable;
            let mut rows: Vec<Value> = match doc {
                Some(Value::Array(a)) => a,
                Some(_) => {
                    report.refusals.push(String::from(
                        "keybindings.json is not a JSON array, so it was left untouched",
                    ));
                    wrong_shape = true;
                    Vec::new()
                }
                None => Vec::new(),
            };
            let bound: Vec<String> = rows
                .iter()
                .filter_map(|r| r.get("key").and_then(Value::as_str).map(str::to_string))
                .collect();
            let mut changed = false;
            for (key, command) in &report.keybindings {
                if bound.iter().any(|k| k == key) {
                    report
                        .conflicts
                        .push(format!("keybindings.json {key}: already bound, left alone"));
                    continue;
                }
                let mut row = Map::new();
                row.insert(String::from("key"), Value::from(key.clone()));
                row.insert(String::from("command"), Value::from(command.clone()));
                rows.push(Value::Object(row));
                changed = true;
            }
            if changed && !wrong_shape {
                write_json(&path, &Value::Array(rows))?;
                written.push(path);
            }
        }
    }

    if !report.snippets.is_empty() {
        let path = dir.join("snippets.json");
        if carries_jsonc_extras(&path) {
            report.refusals.push(String::from(
                "snippets.json: left untouched, because rewriting it would drop its comments or hand formatting. Add the entries above by hand.",
            ));
        } else {
            let (doc, unusable) = read_or_refuse(&path, report);
            wrong_shape = unusable;
            let mut existing: Map<String, Value> = match doc {
                Some(Value::Object(m)) => m,
                Some(_) => {
                    report.refusals.push(String::from(
                        "snippets.json is not a JSON object, so it was left untouched",
                    ));
                    wrong_shape = true;
                    Map::new()
                }
                None => Map::new(),
            };
            let mut changed = false;
            for (name, entry) in &report.snippets {
                if existing.contains_key(name) {
                    report.conflicts.push(format!(
                        "snippets.json {name:?}: already defined, left alone"
                    ));
                    continue;
                }
                existing.insert(name.clone(), entry.clone());
                changed = true;
            }
            if changed && !wrong_shape {
                write_json(&path, &Value::Object(existing))?;
                written.push(path);
            }
        }
    }

    report.conflicts.sort();
    report.refusals.sort();
    Ok(written)
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(path, format!("{text}\n")).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// #1524: `editor.rulers` imports in both of VS Code's forms, plain
    /// columns and `{ "column", "color" }` objects; entries that are not a
    /// usable column are left out.
    #[test]
    fn editor_rulers_import_as_columns() {
        let doc = json!({
            "editor.rulers": [80, { "column": 100, "color": "#ff0000" }, "wide", -4, 0, 70000]
        });
        let mut report = Report::default();
        convert_settings(&doc, &mut report);
        assert_eq!(report.settings["rulers"], json!([80, 100]));
        let prefs: crate::prefs::Prefs =
            serde_json::from_value(Value::Object(report.settings.clone().into_iter().collect()))
                .unwrap();
        assert_eq!(prefs.rulers, vec![80, 100]);
    }

    #[test]
    fn word_based_suggestions_import_as_vs_code_s_ids() {
        // #1566: the four ids carry over; the boolean older VS Code wrote
        // means the default or off; an id croft does not know is left out.
        for (vscode, croft) in [
            (json!("off"), Some(json!("off"))),
            (json!("currentDocument"), Some(json!("currentDocument"))),
            (json!("allDocuments"), Some(json!("allDocuments"))),
            (json!(true), Some(json!("matchingDocuments"))),
            (json!(false), Some(json!("off"))),
            (json!("everywhere"), None),
        ] {
            let mut report = Report::default();
            convert_settings(
                &json!({ "editor.wordBasedSuggestions": vscode }),
                &mut report,
            );
            assert_eq!(
                report.settings.get("word_based_suggestions"),
                croft.as_ref(),
                "{vscode}"
            );
        }
        assert!(crate::config_layers::WORKSPACE_ALLOWED_KEYS.contains(&"word_based_suggestions"));
    }

    #[test]
    fn settings_map_only_where_croft_has_a_real_equivalent() {
        let doc = json!({
            "editor.formatOnSave": true,
            "editor.bracketPairColorization.enabled": false,
            "editor.autoClosingBrackets": "never",
            "editor.renderWhitespace": "all",
            "terminal.integrated.scrollback": 20000,
            "editor.fontFamily": "Fira Code",
            "workbench.iconTheme": "material-icon-theme"
        });
        let mut report = Report::default();
        convert_settings(&doc, &mut report);

        assert_eq!(report.settings["format_on_save"], json!(true));
        assert_eq!(
            report.settings["disable_bracket_colors"],
            json!(true),
            "VS Code enables the feature, croft names the negative"
        );
        assert_eq!(report.settings["disable_auto_close_pairs"], json!(true));
        assert_eq!(report.settings["render_whitespace"], json!("all"));
        assert_eq!(report.settings["terminal_scrollback"], json!(20000));
        // Keys croft cannot honour are LISTED, never dropped in silence:
        // "my settings imported" and "my settings are gone" must not look
        // the same to the user.
        assert!(
            report
                .unmapped_settings
                .contains(&String::from("editor.fontFamily"))
        );
        assert!(
            report
                .unmapped_settings
                .contains(&String::from("workbench.iconTheme"))
        );
    }

    /// #1440: a `"[python]": { … }` block maps its keys into
    /// `languages.python`, a `[javascript][typescript]` block into both,
    /// and the keys croft cannot scope to a language are listed with their
    /// block rather than the bare `[python]` the whole block used to be.
    #[test]
    fn language_blocks_map_into_languages() {
        let mut report = Report::default();
        convert_settings(
            &json!({
                "[python]": {
                    "editor.formatOnSave": true,
                    "editor.defaultFormatter": "charliermarshall.ruff",
                    "editor.rulers": [88]
                },
                "[javascript][typescript]": { "editor.renderWhitespace": "all" },
                "[markdown]": { "editor.inlayHints.enabled": "off", "terminal.integrated.scrollback": 5 },
                "[c]": { "files.autoSave": "afterDelay" }
            }),
            &mut report,
        );
        assert_eq!(
            report.settings["languages"],
            json!({
                "python": { "format_on_save": true },
                "javascript": { "render_whitespace": "all" },
                "typescript": { "render_whitespace": "all" },
                "markdown": { "disable_inlay_hints": true }
            })
        );
        assert_eq!(
            report.settings["language_formatters"],
            json!({ "python": "charliermarshall.ruff" })
        );
        assert!(!report.settings.contains_key("format_on_save"));
        assert!(!report.settings.contains_key("terminal_scrollback"));
        let unmapped = &report.unmapped_settings;
        assert!(
            !unmapped
                .iter()
                .any(|u| u.contains("editor.defaultFormatter")),
            "{unmapped:?}"
        );
        for want in [
            "[python] editor.rulers (croft has rulers only for every language)",
            "[markdown] terminal.integrated.scrollback (croft has terminal_scrollback only for every language)",
            "[c] files.autoSave (croft has auto_save only for every language)",
        ] {
            assert!(
                unmapped.contains(&String::from(want)),
                "{want} not in {unmapped:?}"
            );
        }
        assert!(
            !unmapped.iter().any(|u| u == "[python]"),
            "a block is never listed whole: {unmapped:?}"
        );
    }

    /// #1440 negative: keys that only look like a block, and a block that
    /// is not an object, stay unmapped and set nothing.
    #[test]
    fn malformed_language_blocks_set_nothing() {
        let mut report = Report::default();
        convert_settings(
            &json!({ "[]": { "editor.formatOnSave": true }, "[go]": true, "[a]b]": {} }),
            &mut report,
        );
        assert!(
            !report.settings.contains_key("languages"),
            "{:?}",
            report.settings
        );
        assert!(report.unmapped_settings.contains(&String::from("[]")));
        assert!(report.unmapped_settings.contains(&String::from("[a]b]")));
        assert!(
            report
                .unmapped_settings
                .iter()
                .any(|u| u.starts_with("[go] (not an object")),
            "{:?}",
            report.unmapped_settings
        );
    }

    /// #1440: importing language blocks into a config.json that has some
    /// adds the missing settings and keeps the ones already there.
    #[test]
    fn imported_language_blocks_merge_into_existing_ones() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{ "languages": { "python": { "format_on_save": false } } }"#,
        )
        .unwrap();
        let mut report = Report::default();
        convert_settings(
            &json!({
                "[python]": { "editor.formatOnSave": true, "editor.formatOnType": true },
                "[go]": { "editor.formatOnSave": true }
            }),
            &mut report,
        );
        apply_into(dir.path(), &mut report).expect("apply runs");

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            written["languages"],
            json!({
                "python": { "format_on_save": false, "format_on_type": true },
                "go": { "format_on_save": true }
            })
        );
        assert!(
            report.conflicts.contains(&String::from(
                "config.json languages.python.format_on_save: kept false, VS Code had true"
            )),
            "{:?}",
            report.conflicts
        );
    }

    /// One VS Code key drives two croft prefs, and every mode must land
    /// somewhere: "off" has to clear both rather than leaving whatever was
    /// there before.
    #[test]
    fn auto_save_modes_map_onto_both_croft_toggles() {
        for (mode, delay, focus) in [
            ("afterDelay", true, false),
            ("onFocusChange", false, true),
            ("onWindowChange", false, true),
            ("off", false, false),
        ] {
            let mut report = Report::default();
            convert_settings(&json!({ "files.autoSave": mode }), &mut report);
            assert_eq!(report.settings["auto_save"], json!(delay), "{mode}");
            assert_eq!(
                report.settings["auto_save_on_focus_change"],
                json!(focus),
                "{mode}"
            );
        }
    }

    /// Every croft key the mapping writes must be a real pref, or the
    /// import produces a config.json croft ignores. `Prefs` is the authority,
    /// so the test asks it rather than repeating a list that would drift.
    #[test]
    fn every_mapped_setting_is_a_real_croft_pref() {
        let defaults =
            serde_json::to_value(crate::prefs::Prefs::default()).expect("Prefs serialises");
        let known = defaults.as_object().expect("Prefs is a JSON object");
        let mut emitted: Vec<&str> = SETTINGS.iter().map(|m| m.croft).collect();
        // `files.autoSave` is handled outside the one-to-one table.
        emitted.push("auto_save");
        emitted.push("auto_save_on_focus_change");
        for key in emitted {
            assert!(
                known.contains_key(key),
                "{key} is not a field of Prefs, so croft would ignore it"
            );
        }
    }

    /// #1345: `files.exclude` / `search.exclude` carry the globs set to
    /// `true`; a `when` clause (sibling-file rule croft has no equivalent
    /// for) is left out rather than hiding files. A `false` entry crosses as
    /// a `!glob` removal (#1494), which hides nothing itself.
    #[test]
    fn exclude_settings_keep_the_true_globs_and_false_ones_as_removals() {
        let mut report = Report::default();
        convert_settings(
            &json!({
                "files.exclude": {
                    "**/generated": true,
                    "**/keep": false,
                    "**/*.js": { "when": "$(basename).ts" }
                },
                "search.exclude": { "**/tests/fixtures": true }
            }),
            &mut report,
        );
        assert_eq!(
            report.settings["files_exclude"],
            json!(["**/generated", "!**/keep"])
        );
        assert_eq!(
            report.settings["search_exclude"],
            json!(["**/tests/fixtures"])
        );
    }

    /// #1494: a VS Code glob that starts with `!` is literal there, so it
    /// crosses escaped and stays an exclusion instead of a removal.
    #[test]
    fn a_literal_bang_glob_crosses_escaped() {
        let mut report = Report::default();
        convert_settings(
            &json!({
                "files.exclude": { "!keep/out": true, "!drafts/old": false, "!bare": true }
            }),
            &mut report,
        );
        assert_eq!(
            report.settings["files_exclude"],
            json!(["/!bare", "!\\!drafts/old", "\\!keep/out"])
        );
    }

    /// #1479: VS Code matches an exclude glob against the workspace-relative
    /// path, so a bare `build` is the root's `build` only. It crosses over
    /// anchored (`/build`), where croft's own bare globs match at any depth;
    /// a glob that already says where it applies crosses unchanged, and one
    /// with its own leading `/` (which VS Code never matches) is left out.
    #[test]
    fn a_bare_vscode_exclude_glob_stays_anchored_to_the_root() {
        let mut report = Report::default();
        convert_settings(
            &json!({
                "files.exclude": {
                    "build": true,
                    "*.log": true,
                    "**/node_modules": true,
                    "src/gen": true,
                    "/dist": true
                },
                "search.exclude": { "coverage": true }
            }),
            &mut report,
        );
        let mut files: Vec<String> =
            serde_json::from_value(report.settings["files_exclude"].clone()).unwrap();
        files.sort();
        assert_eq!(
            files,
            vec!["**/node_modules", "/*.log", "/build", "src/gen"]
        );
        assert_eq!(report.settings["search_exclude"], json!(["/coverage"]));
    }

    /// #1345 negative: an exclude setting that is not an object maps to
    /// nothing and is reported, not read as "exclude everything".
    #[test]
    fn a_malformed_exclude_setting_maps_to_nothing() {
        let mut report = Report::default();
        convert_settings(&json!({ "files.exclude": "**/generated" }), &mut report);
        assert!(!report.settings.contains_key("files_exclude"));
        assert!(
            report
                .unmapped_settings
                .iter()
                .any(|k| k.starts_with("files.exclude")),
            "{:?}",
            report.unmapped_settings
        );
    }

    /// A converted value must ROUND-TRIP through the consumer that reads it.
    ///
    /// The previous version of this test compared `std::mem::discriminant`
    /// of two `serde_json::Value`s, which cannot tell `"boundary"` from
    /// `"selection"`: both are `Value::String`, one variant carrying the
    /// payload inside. So it asserted "a string went where a string goes",
    /// and it passed while the fixture fed it `"boundary"`, a mode croft has
    /// no variant for. The test named the bug's own input and went green.
    ///
    /// Round-tripping is the property that discriminates: a value croft
    /// cannot represent does not survive its own parse.
    #[test]
    fn a_converted_whitespace_mode_round_trips_through_croft() {
        use crate::widgets::editor::WhitespaceMode;

        // Every VS Code mode, and what croft should store for it.
        for (vscode, expected) in [
            ("none", "none"),
            ("all", "all"),
            ("selection", "selection"),
            // No croft mode marks only these, so the honest nearest is none.
            ("boundary", "none"),
            ("trailing", "none"),
        ] {
            let mut report = Report::default();
            convert_settings(&json!({ "editor.renderWhitespace": vscode }), &mut report);
            let stored = report.settings["render_whitespace"].as_str().unwrap();
            assert_eq!(stored, expected, "{vscode} converted to {stored}");
            assert_eq!(
                WhitespaceMode::from_pref(stored).pref_id(),
                stored,
                "{stored:?} does not survive croft's own parse: from_pref sends \
                 anything unrecognised to Selection, so a wrong id can look \
                 correct while depending on that fallback"
            );
        }
    }

    /// Every keybinding this import writes must be one croft can LOAD.
    ///
    /// The table maps command ids; nothing checked the chord side, and
    /// `Chord::parse` splits on `+` only, so a VS Code chord SEQUENCE
    /// ("ctrl+k z") was counted as mapped, written to disk, and skipped at
    /// load with a warning. Running the output through the real keymap
    /// parser is the assertion that could not miss it.
    #[test]
    fn every_written_keybinding_is_one_croft_can_load() {
        let doc = json!([
            { "key": "ctrl+shift+p", "command": "workbench.action.quickOpen" },
            { "key": "ctrl+k z", "command": "workbench.action.toggleZenMode" },
            { "key": "f12", "command": "workbench.action.tasks.build" }
        ]);
        let mut report = Report::default();
        convert_keybindings(&doc, &mut report);

        assert!(
            report
                .dropped_keybindings
                .iter()
                .any(|d| d.contains("ctrl+k z")),
            "a chord sequence croft cannot express must be reported, not \
             written: {:?}",
            report.dropped_keybindings
        );

        let rows: Vec<Value> = report
            .keybindings
            .iter()
            .map(|(key, command)| json!({ "key": key, "command": command }))
            .collect();
        let json = serde_json::to_string(&Value::Array(rows)).unwrap();
        let (_keymap, warnings) = crate::keymap::Keymap::resolve(&json);
        assert!(
            warnings.is_empty(),
            "croft rejected rows this import would have written: {warnings:?}"
        );
    }

    #[test]
    fn keybindings_convert_by_table_and_name_what_they_drop() {
        let doc = json!([
            { "key": "ctrl+shift+p", "command": "workbench.action.quickOpen" },
            { "key": "ctrl+k z", "command": "workbench.action.toggleZenMode" },
            { "key": "ctrl+alt+q", "command": "some.extension.command" },
            { "key": "ctrl+b", "command": "-workbench.action.toggleSidebarVisibility" }
        ]);
        let mut report = Report::default();
        convert_keybindings(&doc, &mut report);

        assert_eq!(
            report.keybindings,
            vec![(String::from("ctrl+shift+p"), String::from("quick_open"))],
            "only rows croft can both understand and PARSE are written: \
             `ctrl+k z` is a chord sequence croft cannot express"
        );
        assert_eq!(
            report.dropped_keybindings.len(),
            3,
            "an unknown command, an unbinding and a chord sequence are all \
             reported: {:?}",
            report.dropped_keybindings
        );
        assert!(
            report
                .dropped_keybindings
                .iter()
                .any(|d| d.contains("some.extension.command")),
            "a chord croft cannot honour must be NAMED, not bound to something \
             that merely sounds similar"
        );
    }

    /// #1212: a remapped Go to Definition / References / Implementation /
    /// Type Definition / Declaration / Rename chord carries over onto croft's
    /// caret-driven commands, and croft loads every row it writes.
    #[test]
    fn the_lsp_navigation_and_rename_chords_import_onto_caret_commands() {
        let pairs = [
            (
                "ctrl+alt+a",
                "editor.action.revealDefinition",
                "go_to_definition",
            ),
            (
                "ctrl+alt+b",
                "editor.action.goToReferences",
                "go_to_references",
            ),
            (
                "ctrl+alt+c",
                "editor.action.revealDeclaration",
                "go_to_declaration",
            ),
            (
                "ctrl+alt+d",
                "editor.action.goToTypeDefinition",
                "go_to_type_definition",
            ),
            (
                "ctrl+alt+e",
                "editor.action.goToImplementation",
                "go_to_implementations",
            ),
            ("ctrl+alt+f", "editor.action.rename", "rename_symbol"),
        ];
        let doc = Value::Array(
            pairs
                .iter()
                .map(|(key, vscode, _)| json!({ "key": key, "command": vscode }))
                .collect(),
        );
        let mut report = Report::default();
        convert_keybindings(&doc, &mut report);
        assert!(
            report.dropped_keybindings.is_empty(),
            "{:?}",
            report.dropped_keybindings
        );
        let expected: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, _, croft)| (key.to_string(), croft.to_string()))
            .collect();
        assert_eq!(report.keybindings, expected);
        let rows: Vec<Value> = report
            .keybindings
            .iter()
            .map(|(key, command)| json!({ "key": key, "command": command }))
            .collect();
        let json = serde_json::to_string(&Value::Array(rows)).unwrap();
        let (_keymap, warnings) = crate::keymap::Keymap::resolve(&json);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// The hover and peek commands still import as they did: `showHover` has
    /// no croft counterpart and is reported, `peekDefinition` stays peek.
    #[test]
    fn hover_is_still_dropped_and_peek_still_imports_as_peek() {
        let doc = json!([
            { "key": "ctrl+alt+h", "command": "editor.action.showHover" },
            { "key": "ctrl+alt+p", "command": "editor.action.peekDefinition" }
        ]);
        let mut report = Report::default();
        convert_keybindings(&doc, &mut report);
        assert_eq!(
            report.keybindings,
            vec![(String::from("ctrl+alt+p"), String::from("peek_definition"))]
        );
        assert_eq!(report.dropped_keybindings.len(), 1);
        assert!(report.dropped_keybindings[0].contains("editor.action.showHover"));
    }

    /// #852 (comment 3): VS Code's editor-tab commands import onto croft's
    /// palette commands of the same name, and croft loads every row.
    #[test]
    fn the_editor_tab_commands_import_under_their_vscode_ids() {
        let pairs = [
            (
                "ctrl+alt+1",
                "workbench.action.closeOtherEditors",
                "close_other_editors",
            ),
            (
                "ctrl+alt+2",
                "workbench.action.closeEditorsToTheRight",
                "close_editors_to_the_right",
            ),
            (
                "ctrl+alt+3",
                "workbench.action.closeUnmodifiedEditors",
                "close_saved_editors",
            ),
            (
                "ctrl+alt+4",
                "workbench.action.closeAllEditors",
                "close_all_editors",
            ),
            ("ctrl+alt+5", "workbench.action.pinEditor", "pin_editor"),
            ("ctrl+alt+6", "workbench.action.unpinEditor", "unpin_editor"),
            ("ctrl+alt+7", "workbench.action.keepEditor", "keep_editor"),
        ];
        let doc = Value::Array(
            pairs
                .iter()
                .map(|(key, vscode, _)| json!({ "key": key, "command": vscode }))
                .collect(),
        );
        let mut report = Report::default();
        convert_keybindings(&doc, &mut report);
        assert!(
            report.dropped_keybindings.is_empty(),
            "{:?}",
            report.dropped_keybindings
        );
        let expected: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, _, croft)| (key.to_string(), croft.to_string()))
            .collect();
        assert_eq!(report.keybindings, expected);
        let rows: Vec<Value> = report
            .keybindings
            .iter()
            .map(|(key, command)| json!({ "key": key, "command": command }))
            .collect();
        let json = serde_json::to_string(&Value::Array(rows)).unwrap();
        let (_keymap, warnings) = crate::keymap::Keymap::resolve(&json);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// #1565: VS Code's move-editor commands import, with VS Code's own
    /// default chords, and resolve to real croft commands.
    #[test]
    fn the_move_editor_commands_import_under_their_vscode_ids() {
        let pairs = [
            (
                "ctrl+shift+pageup",
                "workbench.action.moveEditorLeftInGroup",
                "move_editor_left_in_group",
            ),
            (
                "ctrl+shift+pagedown",
                "workbench.action.moveEditorRightInGroup",
                "move_editor_right_in_group",
            ),
            (
                "ctrl+alt+right",
                "workbench.action.moveEditorToNextGroup",
                "move_editor_to_next_group",
            ),
            (
                "ctrl+alt+left",
                "workbench.action.moveEditorToPreviousGroup",
                "move_editor_to_previous_group",
            ),
        ];
        let doc = Value::Array(
            pairs
                .iter()
                .map(|(key, vscode, _)| json!({ "key": key, "command": vscode }))
                .collect(),
        );
        let mut report = Report::default();
        convert_keybindings(&doc, &mut report);
        assert!(
            report.dropped_keybindings.is_empty(),
            "{:?}",
            report.dropped_keybindings
        );
        // The report lists bindings in key order.
        let mut expected: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, _, croft)| (key.to_string(), croft.to_string()))
            .collect();
        expected.sort();
        assert_eq!(report.keybindings, expected);
        let rows: Vec<Value> = report
            .keybindings
            .iter()
            .map(|(key, command)| json!({ "key": key, "command": command }))
            .collect();
        let json = serde_json::to_string(&Value::Array(rows)).unwrap();
        let (_keymap, warnings) = crate::keymap::Keymap::resolve(&json);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// Every croft command id in the table must actually exist. A typo here
    /// would produce a keybindings.json croft itself rejects at load.
    #[test]
    fn every_mapped_command_id_is_a_real_croft_command() {
        for (vscode, croft) in COMMANDS {
            assert!(
                crate::widgets::command_palette::Command::from_id(croft).is_some(),
                "{vscode} maps to {croft}, which is not a croft command id"
            );
        }
    }

    /// A sweep for the class the round-4 review found three of: a mapping
    /// that looks right and produces something the consumer cannot use.
    ///
    /// Every croft command in the table is checked for taking its target
    /// from the POINTER rather than the caret. A keyboard shortcut bound to
    /// a mouse-position command fires somewhere unrelated to the cursor,
    /// which is how `editor.action.revealDefinition` came to be mapped to
    /// `mouse_go_to_definition_at_click`.
    #[test]
    fn no_keyboard_shortcut_maps_to_a_pointer_driven_command() {
        for (vscode, croft) in COMMANDS {
            assert!(
                !croft.starts_with("mouse_"),
                "{vscode} maps to {croft}, which acts on the last POINTER \
                 position: a chord bound to it fires wherever the mouse \
                 happens to be, not at the caret"
            );
        }
    }

    /// #1400: `debug.saveBeforeStart: "none"` is the one value that turns
    /// off saving before a launch; VS Code's other values all save.
    #[test]
    fn debug_save_before_start_none_disables_saving_before_a_launch() {
        let none = json!({ "debug.saveBeforeStart": "none" });
        let (mapped, _, _) = map_settings(none.as_object().unwrap());
        assert_eq!(mapped["disable_save_before_debug"], json!(true));
        for value in ["allEditorsInActiveGroup", "nonUntitledEditorsInActiveGroup"] {
            let saves = json!({ "debug.saveBeforeStart": value });
            let (mapped, _, _) = map_settings(saves.as_object().unwrap());
            assert_eq!(mapped["disable_save_before_debug"], json!(false), "{value}");
        }
        let junk = json!({ "debug.saveBeforeStart": 3 });
        let (mapped, _, _) = map_settings(junk.as_object().unwrap());
        assert!(!mapped.contains_key("disable_save_before_debug"));
    }

    /// The workspace layer and this importer must read a VS Code settings
    /// file the SAME way. They had a table each, and the tables had already
    /// drifted: `files.autoSave: "onWindowChange"` was a save-on-focus-change
    /// to one and invisible to the other.
    #[test]
    fn the_workspace_layer_and_the_importer_agree_on_a_settings_file() {
        let doc = json!({
            "files.autoSave": "onWindowChange",
            "editor.formatOnSave": true,
            "editor.renderWhitespace": "boundary",
            "terminal.integrated.scrollback": 9000
        });
        let obj = doc.as_object().unwrap();
        let (shared, _unmapped, _warnings) = map_settings(obj);

        assert_eq!(
            shared["auto_save_on_focus_change"],
            json!(true),
            "onWindowChange saves when the window loses focus, which is what \
             croft's focus-change toggle means"
        );

        // The workspace layer sees the same values, minus the keys a
        // workspace may not set.
        for key in [
            "auto_save_on_focus_change",
            "format_on_save",
            "render_whitespace",
        ] {
            assert!(
                crate::config_layers::WORKSPACE_ALLOWED_KEYS.contains(&key),
                "{key} should reach a workspace layer"
            );
        }
        assert!(
            !crate::config_layers::WORKSPACE_ALLOWED_KEYS.contains(&"terminal_scrollback"),
            "a workspace must not set the scrollback, so the filter must drop it"
        );
    }

    /// croft SEEDS `keybindings.json` and `snippets.json` from commented
    /// templates, so the common case is a file whose header this import would
    /// destroy by parsing JSONC and writing strict JSON back.
    ///
    /// This drives `apply_into`, the real write path. The previous version
    /// asserted on `carries_comments` alone, so unwiring the guard at all
    /// three call sites left every test passing while the import ate croft's
    /// own template header: a surviving mutant on the defect the test is
    /// named for.
    #[test]
    fn a_commented_config_file_is_never_rewritten() {
        let dir = tempfile::TempDir::new().unwrap();
        let keybindings = dir.path().join("keybindings.json");
        std::fs::write(&keybindings, crate::keymap::TEMPLATE).unwrap();
        let before = std::fs::read_to_string(&keybindings).unwrap();
        assert!(
            before.contains("//"),
            "croft's own template carries comments, which is the whole point"
        );

        let mut report = Report::default();
        report
            .keybindings
            .push((String::from("ctrl+shift+p"), String::from("quick_open")));
        report
            .snippets
            .insert(String::from("S"), json!({ "prefix": "s", "body": "x" }));
        report
            .settings
            .insert(String::from("format_on_save"), json!(true));

        let written = apply_into(dir.path(), &mut report).expect("apply runs");

        assert_eq!(
            std::fs::read_to_string(&keybindings).unwrap(),
            before,
            "the commented file must be byte-for-byte untouched"
        );
        assert!(
            !written.contains(&keybindings),
            "and must not be reported as written"
        );
        assert!(
            report
                .refusals
                .iter()
                .any(|c| c.contains("keybindings.json") && c.contains("comments")),
            "the user must be told why, got {:?}",
            report.refusals
        );

        // A file with no comments is still written, so the guard is narrow.
        assert!(
            written.iter().any(|p| p.ends_with("config.json")),
            "an uncommented file must still be merged: {written:?}"
        );
    }

    /// A file that parses but is the WRONG SHAPE must be reported, not
    /// replaced. Treating "not an object" the same as "absent" started the
    /// merge from an empty map and overwrote the user's content silently,
    /// which is the opposite of this module's posture.
    #[test]
    fn a_wrong_shaped_config_file_is_reported_rather_than_overwritten() {
        let dir = tempfile::TempDir::new().unwrap();
        // Each file parses as JSON and is the wrong shape for its purpose.
        let cases = [
            ("config.json", "[1, 2, 3]\n"),
            ("keybindings.json", "{ \"not\": \"an array\" }\n"),
            ("snippets.json", "\"a bare string\"\n"),
        ];
        for (name, body) in cases {
            std::fs::write(dir.path().join(name), body).unwrap();
        }

        let mut report = Report::default();
        report
            .settings
            .insert(String::from("format_on_save"), json!(true));
        report
            .keybindings
            .push((String::from("ctrl+shift+p"), String::from("quick_open")));
        report
            .snippets
            .insert(String::from("S"), json!({ "prefix": "s", "body": "x" }));

        let written = apply_into(dir.path(), &mut report).expect("apply runs");

        assert!(
            written.is_empty(),
            "nothing may be written over a wrong-shaped file: {written:?}"
        );
        for (name, body) in cases {
            assert_eq!(
                std::fs::read_to_string(dir.path().join(name)).unwrap(),
                body,
                "{name} must be byte-for-byte untouched"
            );
            assert!(
                report.refusals.iter().any(|c| c.starts_with(name)),
                "{name} must be reported as a refusal, got {:?}",
                report.refusals
            );
        }
    }

    /// A file that does not PARSE is refused per file, not aborted per run.
    ///
    /// `read_jsonc`'s error propagated through `?`, so one stray comma in
    /// `config.json` stopped the whole import before it reached keybindings
    /// or snippets. The test that claimed to cover "per file, not per run"
    /// used a fixture that PARSES (`[1]`), so it covered the rarer half of
    /// its own property: a stray comma is far likelier than a JSON array
    /// where an object belongs.
    #[test]
    fn an_unparsable_file_is_refused_without_stopping_the_others() {
        let dir = tempfile::TempDir::new().unwrap();
        // The classic hand-edit: one trailing comma too many, and not
        // recoverable by the JSONC strip.
        // No comments and no trailing comma, so the JSONC-extras guard does
        // not claim it first: this file is simply not JSON.
        std::fs::write(dir.path().join("config.json"), "{ \"a\": }\n").unwrap();

        let mut report = Report::default();
        report
            .settings
            .insert(String::from("format_on_save"), json!(true));
        report
            .snippets
            .insert(String::from("S"), json!({ "prefix": "s", "body": "x" }));

        let written = apply_into(dir.path(), &mut report)
            .expect("an unreadable file must not fail the whole import");
        assert!(
            written.iter().any(|p| p.ends_with("snippets.json")),
            "the other files must still be written: {written:?}"
        );
        assert!(
            !written.iter().any(|p| p.ends_with("config.json")),
            "and the unreadable one must not be"
        );
        assert!(
            report
                .refusals
                .iter()
                .any(|r| r.starts_with("config.json") && r.contains("could not be read")),
            "the user must be told which file and why, got {:?}",
            report.refusals
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.json")).unwrap(),
            "{ \"a\": }\n",
            "and it must be left byte-for-byte alone"
        );
    }

    /// A refusal is not a conflict. Reporting them in one list let the CLI
    /// tell a user "nothing changed because nothing needed to" when the truth
    /// was that croft declined to write.
    #[test]
    fn a_refusal_is_reported_apart_from_a_conflict() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("keybindings.json"), crate::keymap::TEMPLATE).unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{ "format_on_save": false }"#,
        )
        .unwrap();

        let mut report = Report::default();
        report
            .settings
            .insert(String::from("format_on_save"), json!(true));
        report
            .keybindings
            .push((String::from("ctrl+shift+p"), String::from("quick_open")));
        apply_into(dir.path(), &mut report).expect("apply runs");

        assert!(
            report
                .conflicts
                .iter()
                .all(|c| !c.contains("left untouched")),
            "a refusal must not appear as a conflict: {:?}",
            report.conflicts
        );
        assert!(
            report
                .refusals
                .iter()
                .any(|r| r.contains("keybindings.json")),
            "the refusal belongs in its own list: {:?}",
            report.refusals
        );
        assert!(
            report
                .conflicts
                .iter()
                .any(|c| c.contains("format_on_save")),
            "and the real conflict stays a conflict: {:?}",
            report.conflicts
        );
    }

    /// One wrong-shaped file must not block the others: the flag is per
    /// file, not per run.
    #[test]
    fn a_wrong_shaped_file_does_not_stop_the_other_two() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("config.json"), "[1]\n").unwrap();

        let mut report = Report::default();
        report
            .settings
            .insert(String::from("format_on_save"), json!(true));
        report
            .snippets
            .insert(String::from("S"), json!({ "prefix": "s", "body": "x" }));

        let written = apply_into(dir.path(), &mut report).expect("apply runs");
        assert!(
            written.iter().any(|p| p.ends_with("snippets.json")),
            "the snippets file is fine and must still be written: {written:?}"
        );
        assert!(!written.iter().any(|p| p.ends_with("config.json")));
    }

    /// A conflict message must name the setting it kept. Two of these were
    /// `String::from` with `{}` placeholders, so the user saw literal braces
    /// where the key and both values belonged, in the report this module's
    /// own doc calls "the product, not a side effect".
    #[test]
    fn a_conflict_message_names_the_value_it_kept() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{ "format_on_save": false }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("snippets.json"),
            r#"{ "Main": { "prefix": "m", "body": "mine" } }"#,
        )
        .unwrap();

        let mut report = Report::default();
        report
            .settings
            .insert(String::from("format_on_save"), json!(true));
        report.snippets.insert(
            String::from("Main"),
            json!({ "prefix": "m", "body": "theirs" }),
        );
        apply_into(dir.path(), &mut report).expect("apply runs");

        let joined = report.conflicts.join("\n");
        assert!(
            joined.contains("format_on_save") && joined.contains("false"),
            "the settings conflict must name the key and the kept value: {joined}"
        );
        assert!(
            joined.contains("Main"),
            "the snippet conflict must name the snippet: {joined}"
        );
        assert!(
            !joined.contains('{') && !joined.contains('}'),
            "no message may print a literal placeholder: {joined}"
        );
    }

    /// #1178: VS Code scopes a snippet's name to its file, so `main` in
    /// python.json and in rust.json are two snippets. Both are imported,
    /// keyed by their language, and both reach snippets.json.
    #[test]
    fn the_same_snippet_name_in_two_languages_imports_both() {
        let dir = tempfile::TempDir::new().unwrap();
        let profile = dir.path().join("User");
        std::fs::create_dir_all(profile.join("snippets")).unwrap();
        std::fs::write(profile.join("settings.json"), "{}").unwrap();
        std::fs::write(
            profile.join("snippets/python.json"),
            r#"{ "main": { "prefix": "ifmain", "body": ["if __name__ == \"__main__\":", "\t$0"] } }"#,
        )
        .unwrap();
        std::fs::write(
            profile.join("snippets/rust.json"),
            r#"{ "main": { "prefix": "fnmain", "body": ["fn main() {", "\t$0", "}"] } }"#,
        )
        .unwrap();
        let mut report = scan_profile_with_extensions(&profile, &[]).unwrap();
        assert_eq!(report.snippets.len(), 2, "{:?}", report.snippets);
        assert_eq!(report.snippets["main (python)"]["prefix"], "ifmain");
        assert_eq!(report.snippets["main (python)"]["scope"], "python");
        assert_eq!(report.snippets["main (rust)"]["prefix"], "fnmain");
        assert_eq!(report.snippets["main (rust)"]["scope"], "rust");

        let out = dir.path().join("croft");
        apply_into(&out, &mut report).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(out.join("snippets.json")).unwrap())
                .unwrap();
        let prefixes: Vec<&str> = written
            .as_object()
            .unwrap()
            .values()
            .filter_map(|v| v["prefix"].as_str())
            .collect();
        assert!(
            prefixes.contains(&"ifmain") && prefixes.contains(&"fnmain"),
            "{written}"
        );

        // A third language joins them under its own key.
        let doc = json!({ "main": { "prefix": "gomain", "body": "func main() {}" } });
        convert_snippets(&doc, Some("go"), &mut report);
        assert_eq!(report.snippets.len(), 3);
        assert_eq!(report.snippets["main (go)"]["prefix"], "gomain");
        assert!(!report.snippets.contains_key("main"));
    }

    /// #1178, negative: a name used once keeps its bare key, and the same
    /// name twice for the same language keeps the later one and says so.
    #[test]
    fn a_snippet_name_used_once_keeps_its_key_and_a_same_language_repeat_warns() {
        let mut report = Report::default();
        let first = json!({ "main": { "prefix": "a", "body": "first" } });
        convert_snippets(&first, Some("python"), &mut report);
        assert_eq!(report.snippets.keys().collect::<Vec<_>>(), ["main"]);
        assert!(report.warnings.is_empty());
        let again = json!({ "main": { "prefix": "b", "body": "second" } });
        convert_snippets(&again, Some("python"), &mut report);
        assert_eq!(report.snippets.len(), 1);
        assert_eq!(report.snippets["main"]["body"], "second");
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("\"main\" is defined twice for python")),
            "{:?}",
            report.warnings
        );
    }

    /// #1178: a snippet literally named `foo (python)` is not a generated
    /// key, so a later `foo` in python.json must not overwrite it.
    #[test]
    fn a_literal_parenthesized_snippet_name_is_not_taken_for_a_generated_key() {
        let mut report = Report::default();
        let literal = json!({ "foo (python)": { "prefix": "lit", "body": "literal" } });
        convert_snippets(&literal, Some("python"), &mut report);
        let foo = json!({ "foo": { "prefix": "foo", "body": "foo" } });
        convert_snippets(&foo, Some("python"), &mut report);
        assert_eq!(report.snippets.len(), 2, "{:?}", report.snippets);
        assert_eq!(report.snippets["foo (python)"]["prefix"], "lit");
        assert_eq!(report.snippets["foo"]["prefix"], "foo");
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        // A `foo` for another language now qualifies both, and the python one
        // steps around the literal key instead of replacing it.
        let rust = json!({ "foo": { "prefix": "rs", "body": "rs" } });
        convert_snippets(&rust, Some("rust"), &mut report);
        assert_eq!(report.snippets.len(), 3, "{:?}", report.snippets);
        assert_eq!(report.snippets["foo (python)"]["prefix"], "lit");
        assert_eq!(report.snippets["foo (python) 2"]["prefix"], "foo");
        assert_eq!(report.snippets["foo (rust)"]["prefix"], "rs");

        // A repeat of python's `foo` still replaces its own snippet only.
        let again = json!({ "foo": { "prefix": "foo2", "body": "foo2" } });
        convert_snippets(&again, Some("python"), &mut report);
        assert_eq!(report.snippets.len(), 3);
        assert_eq!(report.snippets["foo (python)"]["prefix"], "lit");
        assert_eq!(report.snippets["foo (python) 2"]["prefix"], "foo2");
    }

    #[test]
    fn snippets_take_their_language_from_the_file_name() {
        let doc = json!({
            "Print": { "prefix": "log", "body": "print($1)$0" },
            "Scoped": { "prefix": "x", "body": "y", "scope": "rust" },
            "Broken": { "prefix": "no body" }
        });
        let mut report = Report::default();
        convert_snippets(&doc, Some("python"), &mut report);

        assert_eq!(report.snippets["Print"]["scope"], json!("python"));
        assert_eq!(
            report.snippets["Scoped"]["scope"],
            json!("rust"),
            "an explicit scope wins over the file name"
        );
        assert!(!report.snippets.contains_key("Broken"));
        assert_eq!(report.warnings.len(), 1);
    }

    /// A `.code-snippets` file carries its own scopes, so nothing is added.
    #[test]
    fn a_global_snippets_file_keeps_its_own_scope() {
        let doc = json!({ "Any": { "prefix": "a", "body": "b" } });
        let mut report = Report::default();
        convert_snippets(&doc, None, &mut report);
        assert!(report.snippets["Any"].get("scope").is_none());
    }

    /// Reading a whole profile: the three files, in one pass, with the
    /// snippets directory keyed by file name.
    #[test]
    fn a_profile_directory_converts_end_to_end() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{
                // VS Code writes JSONC here
                "editor.formatOnSave": true,
                "editor.fontSize": 13,
            }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("keybindings.json"),
            r#"[{ "key": "ctrl+s", "command": "workbench.action.files.save" }]"#,
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("snippets")).unwrap();
        std::fs::write(
            dir.path().join("snippets").join("rust.json"),
            r##"{ "Test": { "prefix": "tst", "body": "#[test]" } }"##,
        )
        .unwrap();

        let report = scan_profile(dir.path()).expect("the profile scans");
        assert_eq!(report.settings["format_on_save"], json!(true));
        assert_eq!(
            report.unmapped_settings,
            vec![String::from("editor.fontSize")]
        );
        assert_eq!(
            report.keybindings,
            vec![(String::from("ctrl+s"), String::from("save_file"))]
        );
        assert_eq!(report.snippets["Test"]["scope"], json!("rust"));
    }

    /// #1285: VS Code's Save As and Revert File keybindings import onto
    /// croft's commands instead of being dropped.
    #[test]
    fn save_as_and_revert_keybindings_import() {
        let mut report = Report::default();
        convert_keybindings(
            &json!([
                { "key": "ctrl+alt+s", "command": "workbench.action.files.saveAs" },
                { "key": "ctrl+alt+r", "command": "workbench.action.files.revert" },
            ]),
            &mut report,
        );
        assert_eq!(
            report.keybindings,
            vec![
                (String::from("ctrl+alt+r"), String::from("revert_file")),
                (String::from("ctrl+alt+s"), String::from("save_as")),
            ]
        );
        assert!(
            report.dropped_keybindings.is_empty(),
            "{:?}",
            report.dropped_keybindings
        );
    }

    /// #1562: VS Code's terminal clear / rename / kill / split / new with
    /// profile chords import, where all were reported as having no croft
    /// command; a terminal command croft lacks is still reported.
    #[test]
    fn terminal_action_keybindings_import() {
        let mut report = Report::default();
        convert_keybindings(
            &json!([
                { "key": "ctrl+alt+k", "command": "workbench.action.terminal.clear" },
                { "key": "ctrl+alt+r", "command": "workbench.action.terminal.rename" },
                { "key": "ctrl+alt+x", "command": "workbench.action.terminal.kill" },
                { "key": "ctrl+alt+s", "command": "workbench.action.terminal.split" },
                { "key": "ctrl+alt+p", "command": "workbench.action.terminal.newWithProfile" },
                { "key": "ctrl+alt+z", "command": "workbench.action.terminal.sizeToContentWidth" }
            ]),
            &mut report,
        );
        let mut got = report.keybindings.clone();
        got.sort();
        let mut want: Vec<(String, String)> = [
            ("ctrl+alt+k", "terminal_clear"),
            ("ctrl+alt+r", "terminal_rename"),
            ("ctrl+alt+x", "terminal_kill"),
            ("ctrl+alt+s", "terminal_split"),
            ("ctrl+alt+p", "terminal_new_with_profile"),
        ]
        .iter()
        .map(|(k, c)| (k.to_string(), c.to_string()))
        .collect();
        want.sort();
        assert_eq!(got, want);
        assert_eq!(report.dropped_keybindings.len(), 1, "sizeToContentWidth");
        for (_, id) in &want {
            assert!(
                crate::widgets::command_palette::Command::from_id(id).is_some(),
                "{id} is a croft command"
            );
        }
    }

    /// #1535: VS Code's Run All Tests / Run Tests in Current File / Rerun
    /// Failed Tests / Rerun Last Run chords import.
    #[test]
    fn testing_run_keybindings_import() {
        let mut report = Report::default();
        convert_keybindings(
            &json!([
                { "key": "ctrl+alt+a", "command": "testing.runAll" },
                { "key": "ctrl+alt+f", "command": "testing.runCurrentFile" },
                { "key": "ctrl+alt+e", "command": "testing.reRunFailTests" },
                { "key": "ctrl+alt+l", "command": "testing.reRunLastRun" }
            ]),
            &mut report,
        );
        let mut got: Vec<&str> = report.keybindings.iter().map(|(_, c)| c.as_str()).collect();
        got.sort();
        assert_eq!(
            got,
            [
                "testing_rerun_failed",
                "testing_rerun_last",
                "testing_run_all",
                "testing_run_file"
            ]
        );
        for id in got {
            assert!(crate::widgets::command_palette::Command::from_id(id).is_some());
        }
    }

    /// #1292: VS Code's Run Selected Text keybinding imports instead of
    /// being dropped.
    #[test]
    fn run_selected_text_keybinding_imports() {
        let mut report = Report::default();
        convert_keybindings(
            &json!([{ "key": "ctrl+alt+enter", "command": "workbench.action.terminal.runSelectedText" }]),
            &mut report,
        );
        assert_eq!(
            report.keybindings,
            vec![(
                String::from("ctrl+alt+enter"),
                String::from("run_selected_text")
            )]
        );
    }

    /// #1286: VSCodeVim installed in the profile's product turns croft's
    /// Vim mode on, so a Vim user's first `dd` after the import is a
    /// command rather than text.
    #[test]
    fn an_installed_vscodevim_turns_vim_mode_on() {
        let dir = tempfile::TempDir::new().unwrap();
        let profile = dir.path().join("User");
        std::fs::create_dir_all(&profile).unwrap();
        let extensions = dir.path().join("extensions");
        std::fs::create_dir_all(extensions.join("vscodevim.vim-1.30.1")).unwrap();
        let report = scan_profile_with_extensions(&profile, &[extensions]).unwrap();
        assert_eq!(report.settings.get("vim_mode"), Some(&json!(true)));
        assert!(!report.is_empty(), "the import has something to write");
    }

    /// #1286 negative: without VSCodeVim (another extension whose name
    /// merely starts the same way included), the import leaves Vim mode
    /// alone.
    #[test]
    fn without_vscodevim_the_import_leaves_vim_mode_alone() {
        let dir = tempfile::TempDir::new().unwrap();
        let profile = dir.path().join("User");
        std::fs::create_dir_all(&profile).unwrap();
        let extensions = dir.path().join("extensions");
        std::fs::create_dir_all(extensions.join("vscodevim.vimish-0.1.0")).unwrap();
        std::fs::create_dir_all(extensions.join("rust-lang.rust-analyzer-0.3.2")).unwrap();
        let report = scan_profile_with_extensions(&profile, &[extensions]).unwrap();
        assert_eq!(report.settings.get("vim_mode"), None);
    }

    /// Build a VS Code extensions directory holding one colour theme.
    fn fixture_extensions(root: &Path, label: &str) -> PathBuf {
        let ext = root.join("extensions").join("acme.fixture-theme-1.2.0");
        std::fs::create_dir_all(ext.join("themes")).unwrap();
        std::fs::write(
            ext.join("package.json"),
            format!(
                r##"{{ "name": "fixture-theme", "contributes": {{ "themes": [
                    {{ "label": "{label}", "uiTheme": "vs-dark", "path": "./themes/fixture.json" }}
                ] }} }}"##
            ),
        )
        .unwrap();
        std::fs::write(
            ext.join("themes").join("fixture.json"),
            format!(
                r##"{{ "name": "{label}", "type": "dark", "colors": {{ "editor.background": "#101820" }} }}"##
            ),
        )
        .unwrap();
        root.join("extensions")
    }

    /// `workbench.colorTheme` names a theme by its picker label. When that
    /// theme is an installed extension, the import converts it through
    /// `croft theme-import` and points croft's `theme` at the result, so
    /// the user arrives with the colours they had rather than a line saying
    /// croft has no equivalent for the one setting they see all day.
    #[test]
    fn an_installed_vscode_theme_is_converted_and_selected() {
        let dir = tempfile::TempDir::new().unwrap();
        let profile = dir.path().join("User");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(
            profile.join("settings.json"),
            r#"{ "workbench.colorTheme": "Fixture Dark", "editor.formatOnSave": true }"#,
        )
        .unwrap();
        let extensions = fixture_extensions(dir.path(), "Fixture Dark");

        let mut report =
            scan_profile_with_extensions(&profile, &[extensions]).expect("the profile scans");
        let theme = report.theme.as_ref().expect("the theme was resolved");
        assert_eq!(theme.label, "Fixture Dark");
        assert_eq!(theme.converted.id, "fixture-dark");
        assert_eq!(
            report.settings["theme"],
            json!("fixture-dark"),
            "croft's theme setting follows the converted id"
        );
        assert!(
            !report
                .unmapped_settings
                .iter()
                .any(|k| k == "workbench.colorTheme"),
            "a resolved theme is mapped, not listed as missing: {:?}",
            report.unmapped_settings
        );

        // Applying installs the manifest where croft's picker reads it, and
        // writes the theme key with the rest of the settings.
        let croft = tempfile::TempDir::new().unwrap();
        let croft_ext = croft.path().join("extensions");
        let written = apply_into_dirs(croft.path(), &croft_ext, &mut report).expect("apply runs");
        let manifest = croft_ext.join("theme-fixture-dark").join("extension.toml");
        assert!(manifest.is_file(), "the converted theme was installed");
        assert!(
            written.contains(&manifest),
            "and reported as written: {written:?}"
        );
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(croft.path().join("config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(config["theme"], json!("fixture-dark"));
    }

    /// A theme croft cannot find as an installed extension (VS Code's own
    /// built-ins live inside the app bundle, not the extensions dir) stays
    /// unmapped, and the report says why rather than leaving the user to
    /// wonder whether the key was read at all.
    #[test]
    fn a_built_in_or_absent_theme_stays_unmapped_with_the_reason() {
        let dir = tempfile::TempDir::new().unwrap();
        let profile = dir.path().join("User");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(
            profile.join("settings.json"),
            r#"{ "workbench.colorTheme": "Dark+" }"#,
        )
        .unwrap();
        let extensions = fixture_extensions(dir.path(), "Something Else");

        let report =
            scan_profile_with_extensions(&profile, &[extensions]).expect("the profile scans");
        assert!(report.theme.is_none());
        assert!(!report.settings.contains_key("theme"));
        assert_eq!(
            report.unmapped_settings,
            vec![String::from("workbench.colorTheme")]
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("Dark+") && w.contains("theme-import")),
            "the warning names the theme and the way to import it by hand: {:?}",
            report.warnings
        );
    }

    /// A theme that resolves but whose JSON does not convert degrades to a
    /// warning, not a failed import: the other settings still land.
    #[test]
    fn a_theme_that_does_not_convert_is_a_warning_not_a_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        let profile = dir.path().join("User");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(
            profile.join("settings.json"),
            r#"{ "workbench.colorTheme": "Broken", "editor.formatOnSave": true }"#,
        )
        .unwrap();
        let extensions = fixture_extensions(dir.path(), "Broken");
        std::fs::write(
            extensions
                .join("acme.fixture-theme-1.2.0")
                .join("themes")
                .join("fixture.json"),
            "{ not json",
        )
        .unwrap();
        let report =
            scan_profile_with_extensions(&profile, &[extensions]).expect("the profile scans");
        assert!(report.theme.is_none());
        assert_eq!(report.settings["format_on_save"], json!(true));
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("could not be converted")),
            "{:?}",
            report.warnings
        );
    }

    /// A config.json that already names a theme keeps it (merge, never
    /// overwrite), and the report says the converted one is installed and
    /// how to switch, rather than a bare "kept" beside a line that reads as
    /// if the theme changed.
    #[test]
    fn an_existing_theme_setting_is_kept_and_the_conflict_says_how_to_switch() {
        let dir = tempfile::TempDir::new().unwrap();
        let profile = dir.path().join("User");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(
            profile.join("settings.json"),
            r#"{ "workbench.colorTheme": "Fixture Dark" }"#,
        )
        .unwrap();
        let extensions = fixture_extensions(dir.path(), "Fixture Dark");
        let mut report =
            scan_profile_with_extensions(&profile, &[extensions]).expect("the profile scans");

        let croft = tempfile::TempDir::new().unwrap();
        std::fs::write(croft.path().join("config.json"), r#"{ "theme": "nord" }"#).unwrap();
        let croft_ext = croft.path().join("extensions");
        let written = apply_into_dirs(croft.path(), &croft_ext, &mut report).expect("apply runs");
        assert!(
            croft_ext
                .join("theme-fixture-dark")
                .join("extension.toml")
                .is_file()
        );
        assert!(
            !written.contains(&croft.path().join("config.json")),
            "config.json is untouched"
        );
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(croft.path().join("config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(config["theme"], json!("nord"));
        assert!(
            report
                .conflicts
                .iter()
                .any(|c| c.contains("installed and selectable") && c.contains("fixture-dark")),
            "{:?}",
            report.conflicts
        );
    }

    /// VS Code leaves old versions of an extension in place; the newest
    /// wins, by version, not by byte order (where `1.10.0` sorts before
    /// `1.9.0`). A theme path that climbs out of its extension is skipped.
    #[test]
    fn the_newest_installed_version_wins_and_an_escaping_path_is_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("extensions");
        for (version, file) in [
            ("1.9.0", "old.json"),
            ("1.10.0", "new.json"),
            ("1.2.0", "older.json"),
        ] {
            let ext = root.join(format!("acme.fixture-theme-{version}"));
            std::fs::create_dir_all(ext.join("themes")).unwrap();
            std::fs::write(
                ext.join("package.json"),
                format!(
                    r##"{{ "contributes": {{ "themes": [ {{ "label": "Fixture Dark", "uiTheme": "vs-dark", "path": "./themes/{file}" }} ] }} }}"##
                ),
            )
            .unwrap();
            std::fs::write(
                ext.join("themes").join(file),
                r##"{ "name": "Fixture Dark", "type": "dark", "colors": {} }"##,
            )
            .unwrap();
        }
        let found = find_installed_themes("Fixture Dark", std::slice::from_ref(&root));
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["new.json", "old.json", "older.json"]);

        let escaping = root.join("evil.theme-1.0.0");
        std::fs::create_dir_all(&escaping).unwrap();
        std::fs::write(
            escaping.join("package.json"),
            r##"{ "contributes": { "themes": [ { "label": "Evil", "path": "../../../etc/passwd" }, { "label": "Evil", "path": "/etc/passwd" } ] } }"##,
        )
        .unwrap();
        assert!(find_installed_themes("Evil", &[root]).is_empty());
    }

    /// Each VS Code-family product keeps its extensions beside its own
    /// dot-directory, not under the user directory being imported.
    #[test]
    fn extension_dirs_follow_the_product_of_the_user_directory() {
        let home = Path::new("/home/t");
        for (user_dir, expected) in [
            ("/home/t/.config/Code/User", ".vscode/extensions"),
            (
                "/home/t/.config/Code - Insiders/User",
                ".vscode-insiders/extensions",
            ),
            ("/home/t/.config/VSCodium/User", ".vscode-oss/extensions"),
            ("/home/t/.config/Cursor/User", ".cursor/extensions"),
            ("/home/t/.config/Windsurf/User", ".windsurf/extensions"),
        ] {
            assert_eq!(
                theme_extension_dirs_under(home, Path::new(user_dir)),
                vec![home.join(expected)],
                "{user_dir}"
            );
        }
        // An unrecognised directory (a --from pointing anywhere) probes every
        // product, most standard first.
        let all = theme_extension_dirs_under(home, Path::new("/tmp/somewhere"));
        assert_eq!(all[0], home.join(".vscode/extensions"));
        assert_eq!(all.len(), 5);
    }

    /// A missing profile is empty, not an error: someone may have only ever
    /// set keybindings.
    #[test]
    fn an_empty_profile_directory_is_not_a_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        let report = scan_profile(dir.path()).expect("an empty profile is fine");
        assert!(report.is_empty());
    }

    /// #1499: the importer maps a language block's formatter and reports
    /// the block's keys croft cannot scope to a language (#1440).
    #[test]
    fn a_language_block_maps_its_formatter_and_lists_the_rest() {
        let doc = serde_json::json!({
            "editor.defaultFormatter": "esbenp.prettier-vscode",
            "[python]": { "editor.defaultFormatter": "charliermarsh.ruff", "editor.tabSize": 4 },
            "[go]": { "editor.tabSize": 8 },
        });
        let (mapped, unmapped, _) = map_settings(doc.as_object().unwrap());
        assert_eq!(
            mapped.get("default_formatter"),
            Some(&Value::from("esbenp.prettier-vscode"))
        );
        assert_eq!(
            mapped.get("language_formatters"),
            Some(&serde_json::json!({ "python": "charliermarsh.ruff" }))
        );
        assert_eq!(
            unmapped,
            vec![
                String::from("[go] editor.tabSize"),
                String::from("[python] editor.tabSize"),
            ]
        );
    }

    /// #1571: VS Code's Insert Line Below / Above chords import onto croft's
    /// commands instead of being dropped.
    #[test]
    fn insert_line_after_and_before_import() {
        let doc = json!([
            { "key": "ctrl+enter", "command": "editor.action.insertLineAfter" },
            { "key": "ctrl+shift+enter", "command": "editor.action.insertLineBefore" },
            { "key": "ctrl+alt+enter", "command": "editor.action.insertLineAround" }
        ]);
        let mut report = Report::default();
        convert_keybindings(&doc, &mut report);
        assert_eq!(
            report.keybindings,
            [
                (
                    String::from("ctrl+enter"),
                    String::from("insert_line_below")
                ),
                (
                    String::from("ctrl+shift+enter"),
                    String::from("insert_line_above")
                ),
            ]
        );
        assert_eq!(
            report.dropped_keybindings,
            ["ctrl+alt+enter: no croft command matches editor.action.insertLineAround"]
        );
    }

    /// #1580: VS Code's New Untitled Text File chord imports onto croft's.
    #[test]
    fn new_untitled_file_imports() {
        let doc = json!([
            { "key": "ctrl+alt+n", "command": "workbench.action.files.newUntitledFile" }
        ]);
        let mut report = Report::default();
        convert_keybindings(&doc, &mut report);
        assert_eq!(
            report.keybindings,
            [(
                String::from("ctrl+alt+n"),
                String::from("new_untitled_file")
            )]
        );
        assert!(report.dropped_keybindings.is_empty());
    }

    /// #1576: `editor.lineNumbers` maps onto `line_numbers`; a value croft
    /// has no mode for is reported, not mapped.
    #[test]
    fn line_numbers_import_with_their_mode() {
        for mode in ["on", "off", "relative", "interval"] {
            let (settings, unmapped, _) =
                map_settings(json!({ "editor.lineNumbers": mode }).as_object().unwrap());
            assert_eq!(settings["line_numbers"], json!(mode));
            assert!(unmapped.is_empty());
        }
        let (settings, unmapped, _) = map_settings(
            json!({ "editor.lineNumbers": "sideways" })
                .as_object()
                .unwrap(),
        );
        assert!(settings.is_empty());
        assert_eq!(unmapped.len(), 1, "{unmapped:?}");
    }

    /// #1558: VS Code's Select All Occurrences and Add Cursors to Line Ends
    /// chords import onto croft's commands.
    #[test]
    fn select_highlights_and_line_end_cursors_import() {
        let doc = json!([
            { "key": "ctrl+alt+l", "command": "editor.action.selectHighlights" },
            { "key": "ctrl+alt+e", "command": "editor.action.insertCursorAtEndOfEachLineSelected" }
        ]);
        let mut report = Report::default();
        convert_keybindings(&doc, &mut report);
        assert_eq!(
            report.keybindings,
            [
                (
                    String::from("ctrl+alt+e"),
                    String::from("add_cursors_to_line_ends")
                ),
                (
                    String::from("ctrl+alt+l"),
                    String::from("select_all_occurrences")
                ),
            ]
        );
        assert!(report.dropped_keybindings.is_empty());
    }

    /// #1577: VS Code's Find in Selection toggle chord imports onto croft's.
    #[test]
    fn toggle_find_in_selection_imports() {
        let doc = json!([
            { "key": "ctrl+alt+i", "command": "toggleFindInSelection" }
        ]);
        let mut report = Report::default();
        convert_keybindings(&doc, &mut report);
        assert_eq!(
            report.keybindings,
            [(
                String::from("ctrl+alt+i"),
                String::from("toggle_find_in_selection")
            )]
        );
    }
}
