//! rust-analyzer's own requests (#1626): the commands its VS Code extension
//! offers that are not part of LSP, so `lsp-types` has no typed form for
//! them. Each is sent raw (params and result as JSON) the way `initialize`
//! is in `client.rs`; this module builds the params and reads the answers.

use std::path::PathBuf;

use lsp_types::Url;
use serde_json::{Value, json};

/// The name rust-analyzer registers under; its requests go only to it.
pub const SERVER: &str = "rust-analyzer";

/// One of the rust-analyzer requests the palette's `Rust:` commands send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaRequest {
    /// `rust-analyzer/expandMacro`: the macro call at the caret, expanded
    /// recursively, as `{ name, expansion }`.
    ExpandMacro,
    /// `experimental/parentModule`: where the caret's module is declared.
    ParentModule,
    /// `experimental/openCargoToml`: the manifest of the caret's crate.
    OpenCargoToml,
    /// `rust-analyzer/rebuildProcMacros`: rebuild build scripts and proc
    /// macros, for output gone stale.
    RebuildProcMacros,
    /// `rust-analyzer/reloadWorkspace`: re-read `Cargo.toml` and the
    /// project layout.
    ReloadWorkspace,
}

impl RaRequest {
    pub fn method(self) -> &'static str {
        match self {
            RaRequest::ExpandMacro => "rust-analyzer/expandMacro",
            RaRequest::ParentModule => "experimental/parentModule",
            RaRequest::OpenCargoToml => "experimental/openCargoToml",
            RaRequest::RebuildProcMacros => "rust-analyzer/rebuildProcMacros",
            RaRequest::ReloadWorkspace => "rust-analyzer/reloadWorkspace",
        }
    }

    /// The request's params for a caret at UTF-16 `(line, character)` in
    /// `uri`. The workspace-wide two take none.
    pub fn params(self, uri: &Url, line: u32, character: u32) -> Value {
        let doc = json!({ "uri": uri.as_str() });
        match self {
            RaRequest::ExpandMacro | RaRequest::ParentModule => json!({
                "textDocument": doc,
                "position": { "line": line, "character": character },
            }),
            RaRequest::OpenCargoToml => json!({ "textDocument": doc }),
            RaRequest::RebuildProcMacros | RaRequest::ReloadWorkspace => Value::Null,
        }
    }
}

/// An `expandMacro` answer as `(macro name, expansion)`; `None` when the
/// caret is on no macro call (rust-analyzer answers `null`). The name is
/// bare: rust-analyzer sends `sq!` for a bang macro, and the caller adds
/// the `!` itself, which read `sq!!`.
pub fn expansion(result: &Value) -> Option<(String, String)> {
    let name = result.get("name")?.as_str()?.trim_end_matches('!');
    let text = result.get("expansion")?.as_str()?;
    Some((name.to_string(), text.to_string()))
}

/// The places a location answer points at, as `(file, line, UTF-16
/// column)`: a `Location`, a list of them, or a list of `LocationLink`s
/// (their selection range, the name rather than the whole item). Empty for
/// `null` or anything outside a file.
pub fn locations(result: &Value) -> Vec<(PathBuf, u32, u32)> {
    let one = |v: &Value| -> Option<(PathBuf, u32, u32)> {
        let (uri, range) = match v.get("targetUri") {
            Some(uri) => (uri, v.get("targetSelectionRange")?),
            None => (v.get("uri")?, v.get("range")?),
        };
        let path = Url::parse(uri.as_str()?).ok()?.to_file_path().ok()?;
        let start = range.get("start")?;
        let line = u32::try_from(start.get("line")?.as_u64()?).ok()?;
        let col = u32::try_from(start.get("character")?.as_u64()?).ok()?;
        Some((path, line, col))
    };
    match result {
        Value::Array(items) => items.iter().filter_map(one).collect(),
        Value::Null => Vec::new(),
        v => one(v).into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri() -> Url {
        Url::from_file_path("/w/src/main.rs").unwrap()
    }

    /// #1626: the caret requests carry the document and position, the
    /// manifest one only the document, the workspace ones nothing.
    #[test]
    fn each_request_carries_the_params_rust_analyzer_reads() {
        let p = RaRequest::ExpandMacro.params(&uri(), 4, 9);
        assert_eq!(p["textDocument"]["uri"], "file:///w/src/main.rs");
        assert_eq!(p["position"]["line"], 4);
        assert_eq!(p["position"]["character"], 9);
        assert_eq!(
            RaRequest::ParentModule.params(&uri(), 1, 2)["position"]["line"],
            1
        );
        let cargo = RaRequest::OpenCargoToml.params(&uri(), 1, 2);
        assert!(cargo.get("position").is_none());
        assert_eq!(cargo["textDocument"]["uri"], "file:///w/src/main.rs");
        assert!(RaRequest::ReloadWorkspace.params(&uri(), 1, 2).is_null());
        assert_eq!(RaRequest::ExpandMacro.method(), "rust-analyzer/expandMacro");
    }

    #[test]
    fn an_expansion_answer_reads_as_name_and_text() {
        let v = json!({ "name": "sq", "expansion": "(p.x + 1) * (p.x + 1)" });
        assert_eq!(
            expansion(&v),
            Some((String::from("sq"), String::from("(p.x + 1) * (p.x + 1)")))
        );
    }

    #[test]
    fn a_bang_in_the_name_is_dropped_so_it_is_not_doubled() {
        let v = json!({ "name": "sq!", "expansion": "x" });
        assert_eq!(expansion(&v).map(|(n, _)| n).as_deref(), Some("sq"));
    }

    /// #1626 negative: no macro at the caret is `null`, not an empty tab.
    #[test]
    fn a_null_expansion_is_none() {
        assert_eq!(expansion(&Value::Null), None);
        assert_eq!(expansion(&json!({ "name": "sq" })), None);
    }

    #[test]
    fn locations_read_a_location_a_list_and_location_links() {
        let loc = json!({
            "uri": "file:///w/src/lib.rs",
            "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 9 } },
        });
        assert_eq!(
            locations(&loc),
            vec![(PathBuf::from("/w/src/lib.rs"), 3, 4)]
        );
        assert_eq!(locations(&json!([loc, loc])).len(), 2);
        let link = json!([{
            "targetUri": "file:///w/Cargo.toml",
            "targetRange": { "start": { "line": 0, "character": 0 }, "end": { "line": 9, "character": 0 } },
            "targetSelectionRange": { "start": { "line": 1, "character": 7 }, "end": { "line": 1, "character": 12 } },
        }]);
        assert_eq!(
            locations(&link),
            vec![(PathBuf::from("/w/Cargo.toml"), 1, 7)]
        );
    }

    /// #1626 negative: `null`, or a location outside any file, jumps
    /// nowhere.
    #[test]
    fn null_or_non_file_locations_are_empty() {
        assert!(locations(&Value::Null).is_empty());
        let remote = json!({
            "uri": "https://example.com/x.rs",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
        });
        assert!(locations(&remote).is_empty());
    }
}
