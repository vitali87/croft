//! Language servers: sync, messages and progress, decorations, go-to and peek,
//! hierarchies, rename, code actions, formatting, linked editing, smart select,
//! highlights, symbol pickers, code lens, outline, breadcrumbs, sticky scroll, restart.

use super::*;

mod breadcrumbs;
mod capabilities;
mod code_actions;
mod decorations;
mod format;
mod go_to_symbol;
mod hierarchy;
mod inlay_hints;
mod messages;
mod navigation;
mod outline;
mod peek;
mod rename;
mod rust_analyzer;
mod selection;
mod semantic;
mod sync;
mod typing;
mod workspace_edits;
mod workspace_symbols;
#[cfg(test)]
pub(super) use breadcrumbs::breadcrumb_symbol_chain;
#[cfg(test)]
pub(super) use breadcrumbs::sibling_symbols;
pub(super) use outline::syntax_kind_of;
#[cfg(test)]
pub(super) use peek::peek_excerpt;
#[cfg(test)]
pub(super) use semantic::semantic_reply_is_current;
pub(super) use sync::just_installed_covers_path;
pub(super) use sync::store_diagnostics_update;
pub(super) use sync::tool_on_path;
use sync::*;
pub(super) use typing::is_signature_help_trigger;
pub(super) use typing::snippet_completion_item;

/// What to do with one on-type formatting reply (#254). `NotOurs` leaves the
/// armed request alone — a superseded reply must not disarm the newer one
/// still in flight — while `Stale` and `Apply` both consume it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum OnTypeReply {
    NotOurs,
    Stale,
    Apply,
}

/// What a breadcrumb crumb stands for (#1608).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CrumbTarget {
    Folder(PathBuf),
    File(PathBuf),
    /// An index into the outline's symbols.
    Symbol(usize),
}

/// The file the OUTLINE shows and the edit seq it was synced at.
pub(super) type OutlineKey = (PathBuf, Option<u64>);

/// Where an off-thread outline parse delivers its symbols (#1150).
pub(super) type OutlineParse = std::sync::mpsc::Receiver<Vec<crate::lsp::manager::OutlineSymbol>>;

/// A place Peek References can show: path, 0-based line, UTF-16 column.
pub(super) type PeekLocation = (PathBuf, u32, u32);

/// How long a buffer must sit unedited before its code lenses are asked
/// for (#608).
pub(super) const CODE_LENS_SETTLE: std::time::Duration = std::time::Duration::from_millis(600);

/// Maximum pinned sticky-scroll header rows (VS Code's default is 5).
pub(super) const STICKY_SCROLL_MAX: usize = 5;

impl App {
    /// Ask for the active file's code lenses once its edits have settled
    /// (#608). Lenses cost a server real work (rust-analyzer resolves each
    /// one), so this waits out [`CODE_LENS_SETTLE`] rather than riding every
    /// edit the way inlay hints do.
    pub fn tick_code_lens(&mut self) {
        if !self.code_lens_enabled {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(&seq) = self.lsp_last_seen.get(&path) else {
            return;
        };
        if self.code_lens_requested.get(&path) == Some(&seq)
            || self
                .editor
                .last_edit_at
                .is_some_and(|t| t.elapsed() < CODE_LENS_SETTLE)
        {
            return;
        }
        if let Some(lsp) = self.lsp.as_ref() {
            lsp.request_code_lens(path.clone(), seq);
            self.code_lens_requested.insert(path, seq);
        }
    }

    /// Land code-lens replies on every tab showing the file (#608). A reply
    /// for an older seq is dropped; the tab keeps its lenses, each shown only
    /// while its own line is unchanged.
    pub fn drain_lsp_code_lens(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_code_lens() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            if !self.code_lens_enabled || self.lsp_last_seen.get(&u.path) != Some(&u.seq) {
                continue;
            }
            self.for_each_tab_of(&u.path, |tab| {
                tab.code_lenses = u
                    .lenses
                    .iter()
                    .filter_map(|l| {
                        let line = l.line as usize;
                        Some(crate::code_lens::EditorLens {
                            line,
                            line_text: tab.lines.get(line)?.clone(),
                            title: l.title.clone(),
                            command: l.command.clone(),
                            arguments: l.arguments.clone(),
                            server_side: l.server_side,
                        })
                    })
                    .collect();
            });
            changed = true;
        }
        changed
    }

    /// Run the active editor's code lens at `idx` (#608).
    pub(crate) fn run_code_lens(&mut self, idx: usize) {
        let Some(lens) = self.editor.code_lenses.get(idx).cloned() else {
            return;
        };
        match crate::code_lens::action_for(&lens) {
            crate::code_lens::LensAction::ShowLocations(targets) => {
                if targets.len() == 1 {
                    let (path, line, col) = targets.into_iter().next().expect("len 1");
                    self.go_to_definition(path, line, col);
                } else {
                    self.open_location_picker(targets, "references");
                }
            }
            crate::code_lens::LensAction::RunInTerminal { label, command } => {
                self.start_project_task(crate::tasks::Task {
                    label,
                    command,
                    source: String::from("code lens"),
                    is_build: false,
                    is_default: false,
                    problem_matcher: None,
                    is_background: false,
                    vscode: None,
                });
            }
            crate::code_lens::LensAction::DebugAt(line) => {
                self.editor.cursor_row = line.min(self.editor.lines.len().saturating_sub(1));
                self.editor.cursor_col = 0;
                self.debug_test_at_cursor();
            }
            crate::code_lens::LensAction::ServerCommand { command, arguments } => {
                if let (Some(lsp), Some(path)) = (self.lsp.as_mut(), self.editor.path.clone()) {
                    lsp.execute_command(path, command, arguments);
                    self.status = format!("Ran \u{201c}{}\u{201d}", lens.title);
                }
            }
            crate::code_lens::LensAction::Unsupported(why) => {
                self.status = format!("Code lens: {why}");
            }
        }
    }

    /// The caret line's code lenses from the keyboard (Cmd+K Shift+E): one
    /// runs at once, several open a menu to pick from (#608).
    pub(super) fn run_code_lens_at_cursor(&mut self) {
        let lenses = self.editor.lenses_on_line(self.editor.cursor_row);
        match lenses.as_slice() {
            [] => self.status = String::from("No code lens on this line"),
            [one] => self.run_code_lens(*one),
            many => {
                let items: Vec<(String, MenuAction)> = many
                    .iter()
                    .map(|&i| {
                        (
                            self.editor.code_lenses[i].title.clone(),
                            MenuAction::RunCodeLens(i),
                        )
                    })
                    .collect();
                let origin = self.editor.cursor_screen_pos().unwrap_or((
                    self.editor.last_full_area.x + 1,
                    self.editor.last_full_area.y + 1,
                ));
                let root = self.tree.root.clone();
                self.context_menu = Some(ContextMenu::flat(origin, items, root));
            }
        }
    }

    /// Editor: Toggle CodeLens (#608).
    pub(super) fn toggle_code_lens(&mut self) {
        self.code_lens_enabled = !self.code_lens_enabled;
        self.code_lens_requested.clear();
        if !self.code_lens_enabled {
            for tab in self.editor.editors.iter_mut() {
                tab.code_lenses.clear();
            }
            for group in self.editor_layout.inactive_groups_mut() {
                for tab in group.editors.iter_mut() {
                    tab.code_lenses.clear();
                }
            }
        }
        self.status = format!(
            "CodeLens: {}",
            if self.code_lens_enabled { "on" } else { "off" }
        );
    }
}

/// The bundled shell language server, the one that needs shfmt and
/// shellcheck (#1511).
pub(super) const BASH_SERVER: &str = "bash-language-server";

/// What Format Document says for a shell script when shfmt is missing (#1511).
pub(super) const SHFMT_MISSING: &str =
    "Not formatted: bash-language-server formats shell scripts with shfmt, which is not on PATH";

/// The one-time notice when a shell script opens and shellcheck is missing
/// (#1511).
pub(super) const SHELLCHECK_MISSING: &str = "Shell scripts are not linted: bash-language-server lints with shellcheck, which is not on PATH";

/// What the restart commands say while an update build has the language
/// servers stopped (#1621): the build's end starts them again.
pub(super) const LSP_PAUSED_FOR_BUILD: &str =
    "Language servers are stopped while croft builds its update; they restart when it finishes";

/// The largest buffer whose syntax outline is parsed on the UI thread
/// (#1150). A from-scratch parse costs about a quarter of a millisecond per
/// kilobyte, so this keeps it within a frame; larger buffers go to
/// [`App::start_outline_parse`].
pub(super) const OUTLINE_INLINE_MAX_BYTES: usize = 64 * 1024;

/// Divider drawn between the diagnostic block and the language hover content,
/// mirroring the horizontal rule VS Code renders between hover sections.
pub(super) const HOVER_DIVIDER: &str = "────────";

/// What Show or Focus Hover says when the caret has nothing to describe
/// (#1518): a keyboard command that does nothing must say why.
pub(super) const NO_HOVER_AT_CARET: &str = "No hover information at the caret";

/// Render the diagnostics under the hovered point into a single labelled text
/// block, errors first (so the most severe message leads), each line prefixed
/// with its severity. `None` when there are no diagnostics. The severity word
/// answers the user's "what type of error is it" directly, since a TUI can't
/// show VS Code's coloured marker icon.
pub(super) fn format_diagnostics(
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
pub(super) fn compose_hover(diagnostic: Option<&str>, hover: Option<&str>) -> Option<String> {
    match (diagnostic, hover) {
        (Some(d), Some(h)) => Some(format!("{d}\n{HOVER_DIVIDER}\n{h}")),
        (Some(d), None) => Some(d.to_string()),
        (None, Some(h)) => Some(h.to_string()),
        (None, None) => None,
    }
}
