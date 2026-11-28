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

/// The language servers' own state on croft's side: the manager that runs
/// them, what each open buffer last sent them, the diagnostics and
/// progress they reported, the questions they asked, and whether a croft
/// build has them stopped.
pub(super) struct LspState {
    /// Optional LSP orchestrator. None when LspManager::new failed at
    /// startup (e.g. the background tokio runtime couldn't spawn); the
    /// rest of the editor stays fully functional without it.
    pub(super) manager: Option<crate::lsp::LspManager>,
    /// Per-path snapshot of the last edit_seq we forwarded to the LSP
    /// manager. sync_lsp diffs every tick against the current tabs to
    /// emit did_open / did_change / did_close.
    pub(super) last_seen: std::collections::HashMap<PathBuf, u64>,
    /// Newest semantic-token request generation already applied per path.
    /// `seq` alone is not enough: two full requests can be in flight at the
    /// same seq (`sync_lsp` plus a server-driven refresh), and the request
    /// retry loop emits an EMPTY batch once its backoff is exhausted, so a
    /// late empty loser would otherwise blank a good overlay and be written
    /// to the content-keyed disk cache.
    pub(super) semantic_generation_seen: std::collections::HashMap<PathBuf, u64>,
    /// Diagnostics store: per file, the latest set from each server. Servers
    /// push complete sets via `textDocument/publishDiagnostics`, so the inner
    /// map is keyed by server name (ty + ruff can both publish for one file)
    /// and a server's entry is replaced wholesale on each batch (an empty
    /// batch removes it). The merged value for the visible file is handed to
    /// the editor; the store outlives tab switches so re-focusing a file shows
    /// its squiggles again without waiting for the next server push.
    pub(super) diagnostics: std::collections::HashMap<
        PathBuf,
        std::collections::HashMap<String, Vec<crate::lsp::manager::Diagnostic>>,
    >,
    /// Live LSP work-done progress, keyed by server name (e.g. "rust-analyzer"
    /// -> "Indexing 112/340 33%"). An entry exists only while that server has
    /// an active task; the status bar surfaces it so a busy-priming server is
    /// never mistaken for a dead one. Cleared on the server's `End`.
    pub(super) progress: std::collections::HashMap<String, String>,
    /// Language-server questions (`window/showMessageRequest`, #1599)
    /// waiting for the list picker, oldest first, and the one it shows.
    /// Dropping one answers the server "no action".
    pub(super) questions: std::collections::VecDeque<LspQuestion>,
    pub(super) question: Option<LspQuestion>,
    /// True while this croft has stopped its language servers because a
    /// source build of croft is compiling on the same host (#694), so the
    /// servers are restarted once it ends. Only a remote-launched croft
    /// ever sets it.
    pub(super) paused_for_build: bool,
}

impl LspState {
    /// With `manager` (`None` when it could not start), and nothing synced or
    /// reported yet.
    pub(super) fn new(manager: Option<crate::lsp::LspManager>) -> Self {
        Self {
            manager,
            last_seen: std::collections::HashMap::new(),
            semantic_generation_seen: std::collections::HashMap::new(),
            diagnostics: std::collections::HashMap::new(),
            progress: std::collections::HashMap::new(),
            questions: std::collections::VecDeque::new(),
            question: None,
            paused_for_build: false,
        }
    }
}

/// Where the caret rests in which buffer (#2062): the file, the caret's row
/// and column, and the buffer's edit seq.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CaretSpot {
    pub(super) path: PathBuf,
    pub(super) row: usize,
    pub(super) col: usize,
    pub(super) seq: u64,
}

/// A language-server request still waited on, with the file and the edit
/// seq it was asked for (#2062): a reply for another file, or for a buffer
/// edited since, is dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileRequest {
    pub(super) id: u64,
    pub(super) path: PathBuf,
    pub(super) seq: u64,
}

/// A language-server request still waited on, with the caret spot it was
/// asked at (#2062).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CaretRequest {
    pub(super) id: u64,
    pub(super) at: CaretSpot,
}

/// The answer to the request still waited on, from every reply waiting on
/// one channel (#2062). `next` yields the replies oldest first; one whose
/// `id` is not `pending` answers an older request and is dropped, and the
/// last that matches wins. Taking an answer ends the wait, so `pending` is
/// cleared; with none, it is left as it was. Declaration, type definition,
/// rename, formatting and code actions read their replies this way.
pub(super) fn take_reply<T>(
    pending: &mut Option<u64>,
    mut next: impl FnMut() -> Option<T>,
    id: impl Fn(&T) -> u64,
) -> Option<T> {
    let mut reply = None;
    while let Some(candidate) = next() {
        if Some(id(&candidate)) == *pending {
            reply = Some(candidate);
        }
    }
    if reply.is_some() {
        *pending = None;
    }
    reply
}

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
