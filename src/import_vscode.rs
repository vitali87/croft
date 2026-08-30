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

fn as_scrollback(v: &Value) -> Option<Value> {
    v.as_u64().map(Value::from)
}

const SETTINGS: &[SettingMap] = &[
    SettingMap {
        vscode: "editor.formatOnSave",
        croft: "format_on_save",
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
        vscode: "editor.renderWhitespace",
        croft: "render_whitespace",
        convert: as_whitespace_mode,
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
];

/// VS Code command id to croft palette command id.
///
/// Deliberately partial. Every entry here is a command whose croft equivalent
/// does the same thing; a VS Code command with no true counterpart is left
/// out so the import reports it rather than binding the user's chord to
/// something that merely sounds similar.
/// Two VS Code commands are deliberately ABSENT, having been mapped and
/// then removed: `editor.action.revealDefinition` (F12) has no caret-driven
/// croft equivalent, only `mouse_go_to_definition_at_click`, which reads the
/// last POINTER position and would send F12 somewhere unrelated to the
/// cursor; and `editor.action.showHover` is not `peek_definition`, which
/// opens a different thing entirely.
const COMMANDS: &[(&str, &str)] = &[
    ("workbench.action.files.save", "save_file"),
    ("workbench.action.quickOpen", "quick_open"),
    ("workbench.action.gotoSymbol", "go_to_symbol"),
    ("workbench.action.showAllSymbols", "go_to_workspace_symbol"),
    ("workbench.action.closeActiveEditor", "close_editor"),
    (
        "workbench.action.reopenClosedEditor",
        "reopen_closed_editor",
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
    ("workbench.action.toggleZenMode", "toggle_zen_mode"),
    ("workbench.view.explorer", "show_explorer"),
    ("workbench.view.search", "show_search"),
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
    ("editor.action.startFindReplaceAction", "replace_in_file"),
    ("editor.action.moveLinesUpAction", "move_line_up"),
    ("editor.action.moveLinesDownAction", "move_line_down"),
    ("editor.action.deleteLines", "delete_line"),
    ("editor.action.joinLines", "join_lines"),
    ("editor.action.transformToUppercase", "transform_upper"),
    ("editor.action.transformToLowercase", "transform_lower"),
    ("editor.action.transformToTitlecase", "transform_title"),
    (
        "editor.action.trimTrailingWhitespace",
        "trim_trailing_whitespace",
    ),
    ("editor.action.indentationToSpaces", "indentation_to_spaces"),
    ("editor.action.indentationToTabs", "indentation_to_tabs"),
    ("editor.action.insertCursorAbove", "add_cursor_above"),
    ("editor.action.insertCursorBelow", "add_cursor_below"),
    (
        "editor.action.addSelectionToNextFindMatch",
        "add_selection_to_next_match",
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
    ("workbench.action.debug.start", "start_debugging"),
    ("workbench.action.debug.stop", "stop_debugging"),
    ("workbench.action.debug.restart", "restart_debugging"),
    ("workbench.action.debug.pause", "pause_debugging"),
    ("workbench.action.debug.stepOver", "step_over"),
    ("editor.debug.action.toggleBreakpoint", "toggle_breakpoint"),
    ("undo", "undo"),
    ("redo", "redo"),
    ("references-view.showCallHierarchy", "show_incoming_calls"),
    ("workbench.action.navigateBack", "navigate_back"),
    ("workbench.action.navigateForward", "navigate_forward"),
];

/// What an import would do, or did.
#[derive(Debug, Default)]
pub struct Report {
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
    (settings, unmapped, warnings)
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
        report.snippets.insert(name.clone(), Value::Object(out));
    }
}

/// Read a whole VS Code user directory into a report.
pub fn scan_profile(dir: &Path) -> Result<Report> {
    let mut report = Report::default();
    if let Some(doc) = read_jsonc(&dir.join("settings.json"))? {
        convert_settings(&doc, &mut report);
    }
    if let Some(doc) = read_jsonc(&dir.join("keybindings.json"))? {
        convert_keybindings(&doc, &mut report);
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
    apply_into(&crate::prefs::config_dir(), report)
}

/// [`apply`] into an explicit config directory.
///
/// Split out so a test can drive the REAL write path. The previous test
/// asserted on `carries_comments`, the predicate, and never called `apply`:
/// unwiring the guard at all three call sites left every test passing while
/// the import destroyed croft's own commented templates. Nothing here
/// touches the environment, because the test binary runs its tests on
/// threads of one process.
pub fn apply_into(dir: &Path, report: &mut Report) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut written = Vec::new();

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
                if let Some(current) = existing.get(key) {
                    if current != value {
                        report.conflicts.push(format!(
                            "config.json {key}: kept {current}, VS Code had {value}"
                        ));
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

    /// A missing profile is empty, not an error: someone may have only ever
    /// set keybindings.
    #[test]
    fn an_empty_profile_directory_is_not_a_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        let report = scan_profile(dir.path()).expect("an empty profile is fine");
        assert!(report.is_empty());
    }
}
