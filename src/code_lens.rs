//! What clicking a code lens does (#608).
//!
//! A lens is a title plus a command id with opaque arguments. Some commands
//! the producing server runs itself (it lists them in
//! `executeCommandProvider`); the rest are *client* commands that VS Code
//! implements, which croft has to recognise and map onto its own features.
//! A command croft cannot map is shown but inert, and says so when clicked,
//! rather than being sent to a server that will only reject it.

use std::path::PathBuf;

use serde_json::Value;

/// The editor-side record of one lens: where it sits, the text of that line
/// when the lens arrived (so a lens never lingers on a line that changed),
/// and what it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorLens {
    pub line: usize,
    pub line_text: String,
    pub title: String,
    pub command: Option<String>,
    pub arguments: Vec<Value>,
    pub server_side: bool,
}

/// What a lens click resolves to.
#[derive(Debug, Clone, PartialEq)]
pub enum LensAction {
    /// Show these locations (path, 0-based line, UTF-16 column) in the
    /// location picker, e.g. "3 references".
    ShowLocations(Vec<(PathBuf, u32, u32)>),
    /// Run this shell command in a terminal pane named `label`.
    RunInTerminal { label: String, command: String },
    /// Debug the test at this 0-based line through croft's debugger.
    DebugAt(usize),
    /// Hand the command back to the server (`workspace/executeCommand`).
    ServerCommand {
        command: String,
        arguments: Vec<Value>,
    },
    /// Nothing croft can do; the string says why.
    Unsupported(String),
}

/// Map a lens to the action it performs.
pub fn action_for(lens: &EditorLens) -> LensAction {
    let Some(command) = lens.command.as_deref() else {
        return LensAction::Unsupported(format!("\"{}\" has no command", lens.title));
    };
    if lens.server_side {
        return LensAction::ServerCommand {
            command: command.to_string(),
            arguments: lens.arguments.clone(),
        };
    }
    // `editor.action.showReferences(uri, position, locations)` and the
    // servers' own aliases of it take the same arguments.
    if command.ends_with("showReferences") || command == "editor.action.goToLocations" {
        return match lens.arguments.get(2).map(locations) {
            Some(locs) if !locs.is_empty() => LensAction::ShowLocations(locs),
            _ => LensAction::Unsupported(String::from("no locations to show")),
        };
    }
    match command {
        "rust-analyzer.runSingle" => match lens.arguments.first().and_then(cargo_command) {
            Some((label, command)) => LensAction::RunInTerminal { label, command },
            None => LensAction::Unsupported(String::from("not a cargo runnable")),
        },
        "rust-analyzer.debugSingle" => LensAction::DebugAt(lens.line),
        other => LensAction::Unsupported(format!("{other} is not a command croft runs")),
    }
}

/// LSP `Location`s (or `LocationLink`s) as picker targets.
fn locations(v: &Value) -> Vec<(PathBuf, u32, u32)> {
    let Some(items) = v.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|loc| {
            let uri = loc.get("uri").or_else(|| loc.get("targetUri"))?.as_str()?;
            let range = loc
                .get("range")
                .or_else(|| loc.get("targetSelectionRange"))?;
            let start = range.get("start")?;
            let path = url::Url::parse(uri).ok()?.to_file_path().ok()?;
            Some((
                path,
                start.get("line")?.as_u64()? as u32,
                start.get("character")?.as_u64()? as u32,
            ))
        })
        .collect()
}

/// A rust-analyzer cargo runnable as a shell command: `cd` into its
/// directory, then `cargo <args> -- <test args>`.
fn cargo_command(runnable: &Value) -> Option<(String, String)> {
    if runnable.get("kind")?.as_str()? != "cargo" {
        return None;
    }
    let args = runnable.get("args")?;
    let strings = |key: &str| -> Vec<String> {
        args.get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|s| s.as_str().map(sh_quote)).collect())
            .unwrap_or_default()
    };
    let cargo_args = strings("cargoArgs");
    if cargo_args.is_empty() {
        return None;
    }
    let mut command = String::new();
    if let Some(dir) = args
        .get("cwd")
        .or_else(|| args.get("workspaceRoot"))
        .and_then(Value::as_str)
    {
        command.push_str(&format!("cd {} && ", sh_quote(dir)));
    }
    command.push_str("cargo ");
    command.push_str(&cargo_args.join(" "));
    let exec_args = strings("executableArgs");
    if !exec_args.is_empty() {
        command.push_str(" -- ");
        command.push_str(&exec_args.join(" "));
    }
    let label = runnable
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("cargo")
        .to_string();
    Some((label, command))
}

/// Quote `s` for a POSIX shell, leaving plain words bare so the command a
/// pane shows stays readable.
fn sh_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=+@%,".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lens(command: &str, arguments: Vec<Value>, server_side: bool) -> EditorLens {
        EditorLens {
            line: 4,
            line_text: String::from("fn it_works() {"),
            title: String::from("t"),
            command: Some(command.to_string()),
            arguments,
            server_side,
        }
    }

    #[test]
    fn a_references_lens_shows_its_locations() {
        let l = lens(
            "rust-analyzer.showReferences",
            vec![
                json!("file:///p/a.rs"),
                json!({"line": 0, "character": 3}),
                json!([{"uri": "file:///p/b.rs", "range": {"start": {"line": 7, "character": 2}, "end": {"line": 7, "character": 5}}}]),
            ],
            false,
        );
        assert_eq!(
            action_for(&l),
            LensAction::ShowLocations(vec![(PathBuf::from("/p/b.rs"), 7, 2)])
        );
    }

    #[test]
    fn a_rust_analyzer_run_lens_becomes_a_cargo_command() {
        let l = lens(
            "rust-analyzer.runSingle",
            vec![json!({
                "label": "test tests::it_works",
                "kind": "cargo",
                "args": {
                    "cwd": "/p/my crate",
                    "cargoArgs": ["test", "--package", "demo", "--lib"],
                    "executableArgs": ["tests::it_works", "--exact", "--nocapture"]
                }
            })],
            false,
        );
        assert_eq!(
            action_for(&l),
            LensAction::RunInTerminal {
                label: String::from("test tests::it_works"),
                command: String::from(
                    "cd '/p/my crate' && cargo test --package demo --lib -- tests::it_works --exact --nocapture"
                ),
            }
        );
    }

    #[test]
    fn a_debug_lens_debugs_the_test_on_its_line() {
        assert_eq!(
            action_for(&lens("rust-analyzer.debugSingle", vec![], false)),
            LensAction::DebugAt(4)
        );
    }

    #[test]
    fn a_command_the_server_advertises_goes_back_to_it() {
        let l = lens(
            "gopls.run_tests",
            vec![json!({"URI": "file:///p/a_test.go"})],
            true,
        );
        assert!(matches!(action_for(&l), LensAction::ServerCommand { .. }));
    }

    #[test]
    fn an_unknown_client_command_is_inert_and_says_why() {
        let l = lens("editor.action.somethingElse", vec![], false);
        let LensAction::Unsupported(why) = action_for(&l) else {
            panic!("expected Unsupported");
        };
        assert!(why.contains("editor.action.somethingElse"));
    }

    #[test]
    fn shell_quoting_leaves_plain_words_bare() {
        assert_eq!(sh_quote("--exact"), "--exact");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }
}
