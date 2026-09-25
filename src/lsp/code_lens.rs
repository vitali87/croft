//! LSP CodeLens (#608): the server's lenses for a document, and what croft
//! does when one is clicked.
//!
//! Servers name their lens commands in their own vocabulary
//! (`rust-analyzer.runSingle`, `editor.action.showReferences`, ...), so a
//! lens is classified into the few actions croft can carry out itself:
//! show references, run a test, debug a test. Anything else is drawn as
//! plain text, never sent back as a command croft cannot vouch for.

use serde_json::Value;

/// What clicking a lens does in croft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LensAction {
    /// Open the references picker at this 0-based position.
    References { line: usize, col: usize },
    /// Run the named test.
    RunTest(String),
    /// Debug the named test.
    DebugTest(String),
    /// Shown, not clickable.
    None,
}

/// One lens: the 0-based line it sits above, its label once resolved, and
/// what it does. `data` is kept for `codeLens/resolve`.
#[derive(Debug, Clone, PartialEq)]
pub struct Lens {
    pub line: usize,
    pub col: usize,
    pub title: Option<String>,
    pub action: LensAction,
    pub data: Option<Value>,
    /// The lens as the server sent it, for `codeLens/resolve`.
    pub raw: Value,
}

/// Parse a `textDocument/codeLens` result (`CodeLens[] | null`).
pub fn parse_lenses(result: &Value) -> Vec<Lens> {
    result
        .as_array()
        .map(|lenses| lenses.iter().filter_map(lens_from).collect())
        .unwrap_or_default()
}

fn lens_from(raw: &Value) -> Option<Lens> {
    let start = &raw["range"]["start"];
    let line = start["line"].as_u64()? as usize;
    let col = start["character"].as_u64().unwrap_or(0) as usize;
    let command = &raw["command"];
    let title = command["title"].as_str().map(str::to_string);
    let action = match command["command"].as_str() {
        Some(name) => classify(
            name,
            command["arguments"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            line,
            col,
        ),
        None => LensAction::None,
    };
    Some(Lens {
        line,
        col,
        title,
        action,
        data: raw.get("data").cloned(),
        raw: raw.clone(),
    })
}

/// A lens after `codeLens/resolve` filled in its command.
pub fn resolved(lens: &Lens, result: &Value) -> Lens {
    lens_from(result).unwrap_or_else(|| lens.clone())
}

/// Map a server command to croft's own action.
pub fn classify(command: &str, arguments: &[Value], line: usize, col: usize) -> LensAction {
    // rust-analyzer names the test as the first `executableArgs` entry;
    // gopls lists its test functions under `Tests`.
    let test_name = || {
        arguments.first().and_then(|a| {
            a["args"]["executableArgs"][0]
                .as_str()
                .or_else(|| a["Tests"][0].as_str())
                .map(str::to_string)
        })
    };
    match command {
        "editor.action.showReferences" | "rust-analyzer.showReferences" => {
            LensAction::References { line, col }
        }
        "rust-analyzer.runSingle" | "gopls.run_tests" | "gopls.test" => {
            test_name().map_or(LensAction::None, LensAction::RunTest)
        }
        "rust-analyzer.debugSingle" => test_name().map_or(LensAction::None, LensAction::DebugTest),
        _ => LensAction::None,
    }
}

/// A lens title as shown: codicon references (`$(play)`) dropped, since
/// the terminal has no icon font to resolve them against.
pub fn display_title(title: &str) -> String {
    let mut out = String::new();
    let mut rest = title;
    while let Some(i) = rest.find("$(") {
        out.push_str(&rest[..i]);
        match rest[i..].find(')') {
            Some(j) => rest = &rest[i + j + 1..],
            None => {
                rest = &rest[i..];
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lenses_parse_with_or_without_a_command() {
        let lenses = parse_lenses(&json!([
            {"range": {"start": {"line": 4, "character": 3}, "end": {"line": 4, "character": 9}},
             "command": {"title": "3 references", "command": "editor.action.showReferences",
                         "arguments": ["file:///w/a.ts", {"line": 4, "character": 3}, []]}},
            {"range": {"start": {"line": 10, "character": 0}, "end": {"line": 10, "character": 5}},
             "data": {"k": 1}}
        ]));
        assert_eq!(lenses.len(), 2);
        assert_eq!(lenses[0].line, 4);
        assert_eq!(lenses[0].title.as_deref(), Some("3 references"));
        assert_eq!(lenses[0].action, LensAction::References { line: 4, col: 3 });
        assert_eq!(lenses[1].title, None, "unresolved until codeLens/resolve");
        assert_eq!(lenses[1].data, Some(json!({"k": 1})));
        assert!(parse_lenses(&Value::Null).is_empty());
    }

    #[test]
    fn resolve_fills_the_title_and_action() {
        let lens = parse_lenses(&json!([{"range": {"start": {"line": 2, "character": 0},
            "end": {"line": 2, "character": 1}}, "data": 7}]))
        .remove(0);
        let r = resolved(
            &lens,
            &json!({"range": {"start": {"line": 2, "character": 0},
            "end": {"line": 2, "character": 1}}, "command": {"title": "▶ Run Test",
            "command": "rust-analyzer.runSingle",
            "arguments": [{"label": "test parse::a", "kind": "cargo",
                           "args": {"executableArgs": ["parse::a", "--exact"]}}]}}),
        );
        assert_eq!(r.title.as_deref(), Some("▶ Run Test"));
        assert_eq!(r.action, LensAction::RunTest("parse::a".into()));
        assert_eq!(r.line, 2);
    }

    #[test]
    fn commands_croft_can_carry_out_are_mapped_and_the_rest_are_text() {
        let ra = |cmd: &str| {
            classify(
                cmd,
                &[json!({"args": {"executableArgs": ["m::t", "--exact", "--nocapture"]}})],
                1,
                2,
            )
        };
        assert_eq!(
            ra("rust-analyzer.runSingle"),
            LensAction::RunTest("m::t".into())
        );
        assert_eq!(
            ra("rust-analyzer.debugSingle"),
            LensAction::DebugTest("m::t".into())
        );
        assert_eq!(
            classify("rust-analyzer.showReferences", &[], 5, 6),
            LensAction::References { line: 5, col: 6 }
        );
        assert_eq!(
            classify("gopls.run_tests", &[json!({"Tests": ["TestX"]})], 0, 0),
            LensAction::RunTest("TestX".into())
        );
        assert_eq!(classify("some.server.thing", &[], 0, 0), LensAction::None);
        assert_eq!(
            classify("rust-analyzer.runSingle", &[json!({"args": {}})], 0, 0),
            LensAction::None,
            "no test name, nothing croft can run"
        );
    }

    #[test]
    fn titles_lose_codicon_references() {
        assert_eq!(display_title("$(play) Run Test"), "Run Test");
        assert_eq!(display_title("3 references"), "3 references");
        assert_eq!(display_title("a $(x) b $(unclosed"), "a  b $(unclosed");
    }
}
