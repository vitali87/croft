//! Language servers: sync, messages and progress, decorations, go-to and peek,
//! hierarchies, rename, code actions, formatting, linked editing, smart select,
//! highlights, symbol pickers, code lens, outline, breadcrumbs, sticky scroll, restart.

use super::*;

mod breadcrumbs;
mod capabilities;
mod code_actions;
mod code_lens;
mod decorations;
mod format;
mod go_to_symbol;
mod hierarchy;
mod hover_text;
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
pub(super) use hover_text::compose_hover;
pub(super) use hover_text::format_diagnostics;
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

/// An off-thread outline parse (#1150), the `outline-parse` job: a parse
/// whose thread crashed reads as dead (#2587).
pub(super) type OutlineParse = crate::jobs::Job<Vec<crate::lsp::manager::OutlineSymbol>>;

/// A place Peek References can show: path, 0-based line, UTF-16 column.
pub(super) type PeekLocation = (PathBuf, u32, u32);

/// How long a buffer must sit unedited before its code lenses are asked
/// for (#608).
pub(super) const CODE_LENS_SETTLE: std::time::Duration = std::time::Duration::from_millis(600);

/// Maximum pinned sticky-scroll header rows (VS Code's default is 5).
pub(super) const STICKY_SCROLL_MAX: usize = 5;

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
