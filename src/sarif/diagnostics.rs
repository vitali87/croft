//! SARIF results as editor diagnostics (#577).
//!
//! Every open log publishes its results into the same per-file diagnostics
//! store the language servers and Markdown Lint use, under the source
//! `SARIF: <tool>`. That one step gives results squiggles, hover text and
//! Problems entries with no SARIF-specific drawing code.

use super::model::{Run, SarifResult};
use super::region::{ColumnKind, column_kind};
use super::resolve::Resolver;
use super::semantics::{self as sem, Level};
use crate::lsp::manager::{Diagnostic, DiagnosticSeverity};
use std::path::{Path, PathBuf};

/// VS Code's mapping: error and warning keep their names; note and none are
/// informational (it never uses Hint).
pub fn severity(level: Level) -> DiagnosticSeverity {
    match level {
        Level::Error => DiagnosticSeverity::Error,
        Level::Warning => DiagnosticSeverity::Warning,
        Level::Note | Level::None => DiagnosticSeverity::Information,
    }
}

/// The diagnostics store key for a run's tool.
pub fn source_key(run: &Run) -> String {
    let name = run.tool.driver.name.trim();
    if name.is_empty() {
        String::from("SARIF")
    } else {
        format!("SARIF: {name}")
    }
}

/// Convert a 0-based column in `kind` units on `line` to UTF-16 code units,
/// which is what a `Diagnostic` carries.
fn to_utf16(line: &str, col: usize, kind: ColumnKind) -> u32 {
    match kind {
        ColumnKind::Utf16CodeUnits => col as u32,
        ColumnKind::UnicodeCodePoints => {
            line.chars().take(col).map(char::len_utf16).sum::<usize>() as u32
        }
    }
}

/// One result's primary location as a diagnostic in a local file, or `None`
/// when it has no file location or the file is not on this machine.
/// `line_of(path, n)` returns line `n` (0-based) of a file, for placing a
/// region with no end column at the end of its line.
pub fn diagnostic_for(
    run: &Run,
    result: &SarifResult,
    resolver: &Resolver,
    line_of: &mut dyn FnMut(&Path, usize) -> Option<String>,
) -> Option<(PathBuf, Diagnostic)> {
    let physical = result.locations.first()?.physical_location.as_ref()?;
    let artifact = physical.artifact_location.as_ref()?;
    let path = resolver.resolve(run, artifact, &|p| p.is_file())?;
    let region = physical.region.as_ref();
    let kind = column_kind(run);
    let start_line = region.and_then(|r| r.start_line).unwrap_or(1).max(1) as usize - 1;
    let end_line = region
        .and_then(|r| r.end_line)
        .map(|l| (l.max(1) as usize - 1).max(start_line))
        .unwrap_or(start_line);
    let start_text = line_of(&path, start_line).unwrap_or_default();
    let start_col = region.and_then(|r| r.start_column).unwrap_or(1).max(1) as usize - 1;
    let end_text = if end_line == start_line {
        start_text.clone()
    } else {
        line_of(&path, end_line).unwrap_or_default()
    };
    let end_char = match region.and_then(|r| r.end_column) {
        Some(c) => to_utf16(&end_text, c.max(1) as usize - 1, kind),
        None => end_text.encode_utf16().count() as u32,
    };
    let rule = sem::rule_for(run, result);
    let component = sem::rule_component(run, result);
    let text: String = sem::segments(&sem::message_text(&result.message, rule, Some(component)))
        .into_iter()
        .map(|s| match s {
            sem::Segment::Text(t) => t,
            sem::Segment::LocationLink { text, .. } | sem::Segment::UriLink { text, .. } => text,
        })
        .collect();
    let message = match sem::rule_id(run, result) {
        Some(id) => format!("[{id}] {text}"),
        None => text,
    };
    Some((
        path,
        Diagnostic {
            start_line: start_line as u32,
            start_char: to_utf16(&start_text, start_col, kind),
            end_line: end_line as u32,
            end_char,
            severity: severity(sem::effective_level(result, rule)),
            message,
        },
    ))
}
