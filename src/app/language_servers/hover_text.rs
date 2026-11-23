//! What a hover says: the diagnostics under the pointer as one block, errors first, above
//! the server's hover text.

use super::*;

/// Render the diagnostics under the hovered point into a single labelled text
/// block, errors first (so the most severe message leads), each line prefixed
/// with its severity. `None` when there are no diagnostics. The severity word
/// answers the user's "what type of error is it" directly, since a TUI can't
/// show VS Code's coloured marker icon.
pub(in crate::app) fn format_diagnostics(
    diags: &[(crate::lsp::manager::DiagnosticSeverity, String)],
) -> Option<String> {
    use crate::lsp::manager::DiagnosticSeverity;
    if diags.is_empty() {
        return None;
    }
    let rank = |s: &DiagnosticSeverity| match s {
        DiagnosticSeverity::Error => 0,
        DiagnosticSeverity::Warning => 1,
        DiagnosticSeverity::Information => 2,
        DiagnosticSeverity::Hint => 3,
    };
    let label = |s: &DiagnosticSeverity| match s {
        DiagnosticSeverity::Error => "Error",
        DiagnosticSeverity::Warning => "Warning",
        DiagnosticSeverity::Information => "Info",
        DiagnosticSeverity::Hint => "Hint",
    };
    let mut ordered: Vec<&(DiagnosticSeverity, String)> = diags.iter().collect();
    ordered.sort_by_key(|(s, _)| rank(s));
    Some(
        ordered
            .iter()
            .map(|(s, m)| format!("{}: {}", label(s), m))
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

/// Stack the diagnostic block above the language hover content, divided by a
/// rule, matching VS Code (PR microsoft/vscode#166560 put marker hovers on
/// top). Either side may be absent; `None` when both are.
pub(in crate::app) fn compose_hover(
    diagnostic: Option<&str>,
    hover: Option<&str>,
) -> Option<String> {
    match (diagnostic, hover) {
        (Some(d), Some(h)) => Some(format!("{d}\n{HOVER_DIVIDER}\n{h}")),
        (Some(d), None) => Some(d.to_string()),
        (None, Some(h)) => Some(h.to_string()),
        (None, None) => None,
    }
}
