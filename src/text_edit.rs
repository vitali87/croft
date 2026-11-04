//! The text-edit value that language servers, formatters and the editor pass
//! between them. A leaf, so the LSP layer can produce edits without depending
//! on the editor widget that applies them.

/// A single text replacement in char-indexed `(row, col)` coordinates, as
/// produced by an LSP rename `WorkspaceEdit`. Applied by
/// `widgets::editor::apply_span_edits_to_lines` and `Editor::apply_span_edits`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSpanEdit {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub new_text: String,
    /// The columns are UTF-16 code units, as a language server sends them,
    /// not chars. Converted against the lines at apply time: read as char
    /// columns, every edit after an emoji on its line landed off by one per
    /// astral character, and a rename wrote `xrenamed+ 1` over `x + 1`.
    pub utf16: bool,
}
