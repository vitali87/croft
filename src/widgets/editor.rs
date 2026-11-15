use anyhow::{Context, Result};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use unicode_segmentation::UnicodeSegmentation;

use crate::highlight::{
    HiSpan, LangKind, LangRegistry, compute_line_starts, decode_semantic_tokens, language_label,
};
use crate::packed_lines::PackedLines;
pub use crate::sheet::SheetCellEdit;
pub use crate::text_edit::TextSpanEdit;
use crate::widgets::scrollbar;

mod brackets;
mod comments;
mod diff_view;
mod disk;
mod folding;
mod gutter;
mod highlighting;
mod language;
mod line_commands;
mod lsp_overlays;
mod mouse;
mod multi_cursor;
mod navigation;
mod on_enter;
mod render;
mod selection;
mod snippets;
mod span_edits;
mod symbol_tabs;
mod transforms;
mod typing;
mod undo;
mod viewers;
mod wrap;
use brackets::*;
use comments::*;
pub use diff_view::diff_hit_test;
use diff_view::*;
pub use disk::LineEnding;
pub use disk::SCRATCH_SAVE_REFUSAL;
pub use disk::SaveCleanup;
pub use disk::SaveOutcome;
pub use disk::decode_blob;
pub use disk::is_permission_denied;
pub(crate) use disk::normalize_newlines;
use disk::*;
pub use gutter::GitMark;
pub use gutter::LineBlame;
pub use gutter::LineNumbers;
pub use gutter::carry_marks;
use gutter::*;
use highlighting::*;
pub use language::IndentStyle;
pub use language::SELECTABLE_LANGUAGES;
pub use language::WhitespaceMode;
pub use language::language_scope_id;
pub use language::normalize_rulers;
use language::*;
use line_commands::*;
use multi_cursor::*;
pub(crate) use navigation::is_word_char;
#[cfg(test)]
pub(crate) use navigation::word_char_at;
use navigation::*;
pub(crate) use on_enter::split_into_lines;
use on_enter::*;
use render::*;
pub use selection::EditorSelection;
use selection::*;
pub use span_edits::apply_span_edits_to_lines;
use span_edits::*;
pub use transforms::CaseTransform;
#[cfg(test)]
use transforms::*;
use typing::*;
use undo::*;
pub(crate) use viewers::ansi_color_to_tui;
pub(crate) use viewers::format_bytes_human;
pub use viewers::sqlite_wal_path;
use viewers::*;
pub use wrap::CommentBox;
pub use wrap::CommentFocus;
pub use wrap::CommentHit;
pub(crate) use wrap::NAVIGATOR_ACCENT;
use wrap::*;

/// An image drawn with half blocks (#1591): `rows` × `cols` cells, each a
/// `▀` whose foreground is the upper pixel and background the lower one.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CellImage {
    /// `(image generation, area width, area height, background)`.
    key: (u64, u16, u16, Color),
    cols: u16,
    rows: u16,
    cells: Vec<(Color, Color)>,
}

// Decodes `halfblock_cells` has started, so a test can see that a refusal
// came before one. Per thread, as `FIT_IMAGE_BAKES` is.
#[cfg(test)]
thread_local! {
    static HALFBLOCK_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The inks the side-by-side and deleted-file diff views paint their rows
/// with. A boxed theme keeps the saturated red and green rows it always
/// had. A `surfaces` theme (Croft Loch) tints a changed row toward red or
/// green over the canvas, firmly enough that a change is seen at a glance
/// and calmly enough that the code's own colours still lead; marks the
/// changed characters in a clearly brighter tint of the same hue; lifts a
/// syntax colour that would not read on either tint; puts the `+`/`-`
/// signs in the hue, a changed row's line number in the muted ink (the
/// faint one sinks into the tint) and the rest faint; and hatches the
/// filler opposite an insertion or a deletion instead of filling it solid.
#[derive(Debug, Clone, Copy)]
struct DiffInks {
    removed_bg: Color,
    added_bg: Color,
    /// Text on a removed / added row before its syntax colours land (all
    /// of it in a file with no grammar).
    removed_fg: Color,
    added_fg: Color,
    equal_fg: Color,
    /// The `-` and `+` signs, and the `‹`/`›` that flag a change past an
    /// edge.
    removed_sign: Color,
    added_sign: Color,
    sign_modifier: Modifier,
    gutter_fg: Color,
    /// The line number of a removed or added row: the gutter ink on a
    /// boxed theme, a step up under `surfaces` so it keeps 3:1 on the
    /// row's tint.
    changed_gutter_fg: Color,
    intraline_removed: Color,
    intraline_added: Color,
    /// The background of the blank side of an inserted or deleted line:
    /// the row's tint on a boxed theme, the canvas under `surfaces`.
    filler_removed: Color,
    filler_added: Color,
    /// The glyph and ink the filler is hatched with, under `surfaces`.
    hatch: Option<(&'static str, Color)>,
    /// The glyph and ink of the line between the two columns.
    seam: (&'static str, Color),
    /// Under `surfaces`, the tones a syntax colour on a changed row is
    /// lifted toward when it would fall under 4.5:1 on the row's tint or
    /// on its changed characters' ([`crate::widgets::surface::legible_ink`]).
    /// `None` on a boxed theme, whose inks are drawn exactly as they were.
    legible: Option<crate::widgets::surface::Tones>,
}

/// Coarse classification of the most recent edit, used so consecutive
/// `InsertChar` (or `MultiInsertChar`) ops coalesce into a single undo step
/// (typing burst) but any other edit kind always opens a new step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    InsertChar,
    /// A character typed at every multi-cursor caret. Coalesces like
    /// `InsertChar` while the carets stay where the last keystroke left
    /// them, so a multi-cursor rename undoes in one step (#913).
    MultiInsertChar,
    Newline,
    Backspace,
    DeleteForward,
    Paste,
    DeleteSelection,
    DuplicateLines,
    DeleteLines,
    KillToEol,
    KillToBol,
    OpenLine,
    /// Block indent / outdent (Tab / Shift+Tab over a line range). Never
    /// coalesces, so each Tab press is its own undo step like VS Code.
    Indent,
    /// A Backspace or Delete applied simultaneously across every
    /// multi-cursor caret. Never coalesces, so each is its own undo step.
    MultiEdit,
    /// Move a line / block up or down (Alt+Up / Alt+Down). Its own step.
    MoveLines,
    /// Toggle line or block comment (Cmd+/ / Shift+Alt+A). Its own step.
    ToggleComment,
    /// Join selected lines into one. Its own step.
    JoinLines,
    /// Upper / lower / title-case a selection. Its own step.
    TransformCase,
    /// Sort the selected lines. Its own step.
    SortLines,
    /// Trim trailing whitespace across the buffer. Its own step.
    TrimWhitespace,
    /// Transpose the two characters around the cursor (Ctrl+T). Its own step.
    Transpose,
    /// Convert leading indentation between tabs and spaces. Its own step.
    IndentConvert,
    /// Remove trailing blank lines at end of file. Its own step.
    TrimFinalNewlines,
    /// Terminate the last line on save (`.editorconfig`
    /// `insert_final_newline`). Its own step, so it never coalesces into
    /// the typing burst that preceded the save.
    InsertFinalNewline,
    /// Find-bar Replace / Replace All. Never coalesces, so each replace is
    /// its own undo step like VS Code.
    Replace,
    /// Increment / decrement the number under the cursor (vim's Ctrl-A /
    /// Ctrl-X). Its own step, so a run of bumps undoes one at a time.
    BumpNumber,
    /// Expand an Emmet abbreviation into markup. Its own step, so one undo
    /// puts the abbreviation back.
    EmmetExpand,
}

#[derive(Clone, Debug)]
struct Snapshot {
    /// Packed (#1707): a step held as a `Vec<String>` cost twice its text.
    lines: PackedLines,
    /// Whether that text ended in a newline: a language server's edit can
    /// add or remove it (#911), and an undo brings it back with the text.
    final_newline: bool,
    cursor_row: usize,
    cursor_col: usize,
    selection: Option<EditorSelection>,
    carets: Vec<EditorSelection>,
    dirty: bool,
    /// The editor's `save_seq` when this snapshot was taken. A restore that
    /// crosses a save point re-dirties the buffer: its content no longer
    /// matches disk even though the snapshot predates the edit.
    save_seq: u64,
    /// Who typed each line at the time of the snapshot (#349): an undo
    /// brings back text, and the map that described that text comes back
    /// with it, so a cycle never credits lines to whoever wrote a different
    /// buffer.
    provenance: crate::provenance::Provenance,
    /// The server's fold spans as they stood over this text, with the line
    /// count they were checked against: a line move rewrites them, so an
    /// undo must bring back the ones that described the restored text.
    lsp_folds: Option<Vec<crate::lsp::manager::FoldingRangeItem>>,
    lsp_folds_lines: usize,
    /// The bytes this step's text holds, measured once when it is taken
    /// (see [`UNDO_BYTES_LIMIT`]).
    bytes: usize,
}

thread_local! {
    static CLOSED_HISTORY: std::cell::RefCell<Vec<ClosedHistory>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// An in-progress snippet expansion: the caret is cycling the tab stops of a
/// just-expanded snippet. See [`Editor::expand_snippet`].
#[derive(Debug, Clone)]
struct SnippetSession {
    /// Start `(row, col)` of the current stop's placeholder.
    anchor: (usize, usize),
    /// Placeholder length (chars) of the current stop, selected on landing.
    cur_len: usize,
    /// The placeholder's text on landing, to tell an edit from a plain Tab
    /// when the replacement happens to be the same length.
    cur_text: String,
    /// Remaining stops to visit, in order: `(row, col, placeholder_len)`.
    stops: std::collections::VecDeque<(usize, usize, usize)>,
}

// Counts `rebuild_hidden_ranges` calls so a test can pin that the fold lookup
// is derived once per fold change and not per rendered row. Plain comment, not
// a doc comment: those cannot attach to a `thread_local!`.
#[cfg(test)]
thread_local! {
    static FOLD_RANGE_REBUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A tab closing drops its editor: its history is kept for the file
/// (#1568), whichever of the many close paths dropped it.
impl Drop for Editor {
    fn drop(&mut self) {
        self.stash_history();
    }
}

pub struct Editor {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
    /// Which seat wrote each line (#349), for the gutter overlay and the
    /// inline blame annotation.
    ///
    /// Only edits croft can attribute are recorded — a line it did not watch
    /// being written stays unknown rather than inheriting from its
    /// neighbour, because a provenance overlay that is right most of the
    /// time gets read as fact and the line it gets wrong is the one someone
    /// is arguing about.
    pub provenance: crate::provenance::Provenance,
    /// Why the viewer the file's kind called for was declined, when it was
    /// declined by a deliberate cap rather than by failing to parse (#493).
    /// Consumed by the fallback viewer's status line: a browser silently
    /// replaced by a hex dump reads as a bug rather than as a policy.
    /// Keyed on the path it describes and never cleared on entry: an open
    /// can set it and then fail before reaching any viewer, and `open_hex`
    /// is also reached directly by "Reopen as Hex" without passing through
    /// `open`. Keying makes a surviving note explain only its own file,
    /// which the
    /// clear-after-open the sibling `pdf_restore_page` uses cannot do on
    /// its own. It is not sufficient alone, though: a note can outlive its
    /// open and go stale against the SAME path, so the two places that
    /// reach no fresh status line clear it too - `open_hex`'s same-path
    /// refresh, and a listing that succeeds and thereby disproves it.
    route_note: Option<(std::path::PathBuf, String)>,
    /// The `edit_seq` at which `App::sync_provenance` last considered THIS
    /// buffer for a persisted map (#349), so the history read happens once
    /// per buffer generation rather than per tick. Per tab, because
    /// `edit_seq` is: two tabs on one file both sit at seq 1 after their
    /// opens, and one marker shared across them would skip the second for
    /// the session. Every opener bumps the seq, so every whole-buffer swap
    /// is reconsidered by construction.
    pub provenance_synced_seq: Option<u64>,
    /// Debugger breakpoints, keyed by file path, as 1-based line numbers.
    /// Rendered as red dots in the gutter and pushed to the DAP adapter on
    /// launch. Keyed by path (not just the active file) so switching the buffer
    /// to another file and back keeps its breakpoints.
    pub breakpoints: std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    /// Where the debugger is currently paused (path, 1-based line), or None when
    /// not stopped. Drawn as a highlighted row with a gutter arrow.
    pub stop_line: Option<(PathBuf, usize)>,
    /// Breakpoint lines the adapter reported as NOT verified (could not bind,
    /// e.g. a blank/comment line). Rendered as a hollow ○ instead of a solid ●
    /// so the user sees the breakpoint is inert. Keyed by path.
    pub unverified_breakpoints:
        std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    /// Optional condition expression per breakpoint line (path -> line ->
    /// condition). A conditional breakpoint only pauses when the expression is
    /// true. Rendered with a distinct gutter glyph.
    pub breakpoint_conditions:
        std::collections::HashMap<PathBuf, std::collections::HashMap<usize, String>>,
    /// Optional log message per breakpoint line (path -> line -> message): a
    /// logpoint. The adapter interpolates `{expr}` holes and prints instead
    /// of pausing. Rendered as an amber diamond in the gutter.
    pub breakpoint_logs:
        std::collections::HashMap<PathBuf, std::collections::HashMap<usize, String>>,
    /// Optional hit count per breakpoint line (path -> line -> DAP
    /// `hitCondition`, #611): pause only once the line has been hit that
    /// many times.
    pub breakpoint_hit_conditions:
        std::collections::HashMap<PathBuf, std::collections::HashMap<usize, String>>,
    /// Reader bookmarks (VS Code's Bookmarks extension, nvim's `m` marks),
    /// keyed by file path as 1-based line numbers — the same shape as
    /// [`breakpoints`](Self::breakpoints), so switching tabs and coming back
    /// keeps a file's marks. They are navigation aids only: nothing in the
    /// debugger or the LSP reads them.
    pub bookmarks: std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    /// The text the open file's [`bookmarks`](Self::bookmarks) currently
    /// describe, keyed by the path it was taken for (`None` for an untitled
    /// buffer, whose folds follow edits too). Every buffer change is
    /// diffed against it so marks follow their lines through inserts,
    /// deletes, undo and reloads (see [`shift_bookmark_lines`]). Held only
    /// while the open buffer has marks, so an unmarked file pays nothing.
    bookmark_shadow: Option<(Option<PathBuf>, Vec<String>)>,
    /// Set while a multi-character insert (a paste) runs its per-character
    /// helpers, so the bookmark sync runs once at the end instead of once per
    /// character.
    bookmark_sync_paused: bool,
    /// Monotonic counter that bumps on every buffer mutation. The App's
    /// per-tick sync_lsp diff reads this to know when to forward a
    /// did_change to the LSP server, so building lines.join("\n") only
    /// happens on actual changes, not every frame.
    pub edit_seq: u64,
    /// Overview ruler: the lines the search highlight matches, keyed by
    /// `(edit_seq, needle, options)`. The ruler renders every frame, and
    /// re-running the search (a regex compile included) over the whole buffer
    /// per frame is input lag on a large file.
    ruler_search_cache: Option<(u64, String, crate::widgets::search::SearchOpts, Vec<usize>)>,
    /// Overview ruler: each line's first visual row in wrap mode, text rows
    /// only, keyed by `(edit_seq, wrap width)`. Comment-box rows are few and
    /// change without an edit, so they are added at mapping time instead.
    ruler_starts_cache: Option<(u64, usize, Vec<usize>)>,
    /// The caret's bracket-match highlight, keyed by `(edit_seq,
    /// highlight_generation, caret, protected_current)`: see
    /// `cached_bracket_match_pair`.
    bracket_pair_cache: Option<BracketPairCache>,
    /// The `edit_seq` the collab session last synced this buffer at (Phase D
    /// independent viewports, docs/MULTIPLAYER.md). The tick diff extracts
    /// ops only when `edit_seq` has moved past this, and it is re-pinned
    /// after a remote edit is applied so the diff never rebroadcasts one
    /// (same lazy-recompute pattern as `git_marks_seq`).
    pub collab_synced_seq: u64,
    /// The [`CollabDoc::text_gen`](crate::collab::CollabDoc::text_gen) this
    /// buffer last synced at, 0 = never attached to the live doc. A buffer
    /// created after its file went live (a split duplicate, a reopen) holds
    /// stale disk text: it must never extract (its diff would revert every
    /// peer) and is instead seeded from the replica when this lags.
    pub collab_doc_gen: u64,
    /// HEAD baseline for the git gutter: the committed version's lines. Set by
    /// the app (read off the workspace git root once per file / HEAD change).
    /// `None` when the file is untracked, outside a repo, or not yet fetched.
    pub git_head_lines: Option<PackedLines>,
    /// The path `git_head_lines` was fetched for. The app refetches when this
    /// stops matching `path` (the tab switched files) so the gutter can never
    /// show another file's diff.
    pub git_baseline_for: Option<PathBuf>,
    /// Per 0-based buffer line, how it differs from HEAD. Recomputed from
    /// `git_head_lines` vs `lines` whenever `edit_seq` moves (the diff is a
    /// whole-file Myers pass, so it runs once per edit-batch, not per frame).
    git_marks: std::collections::HashMap<usize, GitMark>,
    /// The `edit_seq` `git_marks` was computed at; `u64::MAX` forces a first
    /// recompute once a baseline arrives.
    git_marks_seq: u64,
    /// Auto-closing pairs (#121): typing an opener/quote inserts the pair,
    /// closers type over, selections surround, and backspace eats an empty
    /// pair. Synced from the app's persisted preference like `blame_enabled`.
    pub auto_close_pairs: bool,
    /// Overwrite (Replace) mode, toggled by the Insert key: a typed character
    /// replaces the one under the caret instead of pushing it along. Mirrored
    /// from the app's single global switch, like `auto_close_pairs`.
    pub overwrite: bool,
    /// The global on-save clean-ups (#1657), set by the app before each save.
    pub save_cleanup: SaveCleanup,
    /// Auto-closing TAGS (VS Code's `html.autoClosingTags` /
    /// `javascript.autoClosingTags`, filling the "no tag auto-close" gap
    /// in `src/vscode_extensions.rs`): typing `>` at the end of an opening
    /// HTML/XML/JSX tag inserts the matching `</tag>` after the caret.
    /// Separate from `auto_close_pairs` because `>` is not a bracket pair
    /// here — it only closes a tag when the text before it parses as one.
    pub auto_close_tags: bool,
    /// The caret position and `edit_seq` recorded by the LAST auto-close
    /// insertion. Pair-backspace fires only while the caret still sits
    /// there with no edits since — a pre-existing `()` in the file must
    /// never lose both sides to one backspace (#122 review).
    auto_pair_at: Option<(usize, usize, u64)>,
    /// The `edit_seq` and caret places after the last multi-cursor
    /// keystroke that inserted pairs (#1597): while nothing has edited or
    /// moved since, a multi-cursor Backspace inside an empty pair takes both
    /// sides, at every caret that inserted one (the last list). A
    /// pre-existing `()` keeps its closer, as with one caret (#122 review),
    /// even when another caret of the same keystroke paired.
    multi_auto_pair: Option<(u64, CaretPlaces, CaretPlaces)>,
    /// The `edit_seq` and every caret's selection, the primary first, after
    /// the last multi-cursor keystroke that typed a character and left no
    /// selection (#913): the next one joins its undo step only while nothing
    /// has edited or moved since, as one caret's typing burst does. Every
    /// command that edits the caret set clears it, so the burst ends even
    /// when the carets come back to the same places (collapse, then add).
    multi_typing: Option<(u64, Vec<EditorSelection>)>,
    /// Merge-conflict blocks in the buffer, lazily recomputed whenever
    /// `edit_seq` moves (same pattern as the git-gutter marks).
    conflicts: Vec<crate::merge::ConflictBlock>,
    /// The `edit_seq` `conflicts` was computed at; `u64::MAX` forces the
    /// first scan.
    conflicts_seq: u64,
    /// Clickable accept-action spans painted on conflict header rows this
    /// frame: `(screen y, x range, header row, resolution)`. Cleared at
    /// render start so the hit test always describes the painted frame.
    pub merge_action_spans: Vec<(u16, std::ops::Range<u16>, usize, crate::merge::Resolution)>,
    /// The language server's code lenses for this buffer (#608), painted at
    /// the end of their line while it still reads as it did when they came.
    pub code_lenses: Vec<crate::code_lens::EditorLens>,
    /// Clickable lens spans painted this frame: `(screen y, x range, index
    /// into code_lenses)`. Cleared at render start, like the merge spans.
    pub code_lens_spans: Vec<(u16, std::ops::Range<u16>, usize)>,
    /// An inline suggestion (#607), painted as ghost text at the caret while
    /// the caret, the buffer and the row are all still where it was asked
    /// for. One that replaces part of the line strikes that part through.
    pub ghost: Option<crate::inline_complete::Ghost>,
    /// Per-source-line git blame for the current file, index 0 = line 1. Set
    /// by the app off-thread once per (file, HEAD); `None` until fetched or
    /// when blame is disabled. Drives the GitLens-style current-line inline
    /// annotation.
    pub blame_lines: Option<crate::git::Blame>,
    /// The path `blame_lines` was fetched for; the app refetches when this
    /// stops matching `path` so blame can never show another file's authors.
    pub blame_for: Option<PathBuf>,
    /// Whether to paint the current-line blame annotation (user pref, default
    /// on). The blame data is still fetched so toggling is instant.
    pub blame_enabled: bool,
    /// The blame annotation painted this frame when it names a commit:
    /// `(screen y, x range, the commit's hash)`, which the mouse hovers for
    /// the commit's card and clicks to open its changes (#1556). Cleared at
    /// render start, like the lens spans.
    blame_span: Option<(u16, std::ops::Range<u16>, String)>,
    /// Paint who typed each line in the gutter and name the seat in the
    /// inline-blame annotation (#349). Off by default: it is a review lens,
    /// not a resting state, and `Editor: Toggle Provenance` flips it for the
    /// session.
    pub provenance_overlay: bool,
    pub scroll: usize,
    /// In soft-wrap mode, the index of the first visible visual segment within
    /// the top logical line (`self.scroll`). Lets the viewport start partway
    /// through a paragraph that wraps to more rows than the pane is tall.
    /// Always 0 outside wrap mode.
    scroll_sub: usize,
    /// Cached total visual-row counts for soft-wrap, one `(width, total)` entry
    /// per width probed this frame (the renderer tests both the wide and the
    /// vbar-narrowed width, so a single slot would thrash). Cleared when the
    /// buffer changes (alongside `hscroll_content_cols`). Drives the vertical
    /// scrollbar's extent in wrap mode without an O(total chars) sweep/frame.
    wrap_total_cache: Vec<(usize, usize)>,
    /// Horizontal scroll offset in CHARACTERS. Long lines (e.g. minified
    /// JS, base64 blobs in HTML) need to scroll right so search hits and
    /// cursor positions deep in a line are reachable. The render slices
    /// each line at this column and `move_right`/`move_left` keep the
    /// cursor visible.
    pub scroll_col: usize,
    /// Cursor column as a CHARACTER index (not bytes), for the current line.
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub focused: bool,
    /// A modal quick input (Command Palette, Go to File, a list picker, any
    /// modal overlay) holds the keyboard above this editor. `focused` keeps
    /// saying where the keyboard returns; this only takes the accent marks
    /// (the top rule's lip, the active tab's underline) off a `surfaces`
    /// theme's editor while the overlay has it, so one thing at a time wears
    /// the accent. Set by the app each frame.
    pub overlay_has_keyboard: bool,
    /// When focused, draw the orange→green gradient border (Black theme)
    /// instead of the solid blue one. Set by the app's focus/theme sync.
    pub focus_gradient: bool,
    /// Active color theme; drives the scrollbar track/thumb colors. Set by the
    /// app's theme sync.
    pub theme: crate::theme::Theme,
    /// Whether the bundled PDF and CSV viewer extensions are enabled. Set by the
    /// app from its disabled-extensions set; when off, `open` skips the inline
    /// viewer and falls through to opening the file as plain text. Default true
    /// so a freshly constructed editor (and every test) keeps the viewers on.
    pub pdf_viewer_enabled: bool,
    pub csv_viewer_enabled: bool,
    pub dirty: bool,
    /// Bumped on every SAVE (`mark_synced_with_disk`, reached only from
    /// `write_buffer_to_disk`). Undo snapshots record it so a restore across
    /// a save point re-dirties. A load clears the undo stacks instead, so no
    /// stale snapshot can span it; a reload keeps them and bumps this
    /// (`carry_history_across_reload`).
    save_seq: u64,
    pub status: String,
    pub last_area: Rect,
    pub last_inner: Rect,
    pub last_scrollbar: Rect,
    /// Hit-test rect for the horizontal scrollbar painted on the bottom inner
    /// row when the longest line overflows the text column. `Rect::default()`
    /// when no line overflows (the row is given back to text). `App` consults
    /// this on click/drag to map an X coordinate onto `scroll_col`.
    pub last_hscrollbar: Rect,
    /// Number of rows actually used for text in the last render, i.e.
    /// `inner.height` minus the horizontal scrollbar row when present. The
    /// vertical viewport math (page size, vertical thumb) reads this so the
    /// cursor never hides behind the horizontal bar.
    last_text_rows: u16,
    /// Cached longest-line width in CHARACTERS. Recomputing it every frame is
    /// O(total chars), so it is invalidated (set `None`) only when the buffer
    /// content changes (`open`/`mark_buffer_changed`) and recomputed lazily.
    hscroll_content_cols: Option<(usize, usize, usize)>,
    /// Visual-row layout captured by the most recent render: one entry per
    /// painted screen row, each `(logical_line, char_start, char_end)`. In
    /// soft-wrap mode a logical line spans several entries; otherwise there is
    /// one entry per visible line with the range `[scroll_col, scroll_col +
    /// text_width)`. `cursor_screen_pos`/`buffer_pos_at` invert it so the
    /// cursor and clicks land on the right cell regardless of wrapping.
    /// Comment-box rows appear inline as `VisRow::Box` entries.
    last_wrap_rows: Vec<VisRow>,
    pub last_gutter_width: u16,
    pub selection: Option<EditorSelection>,
    /// Secondary carets for multi-cursor editing (VS Code "Change All
    /// Occurrences"). The primary caret stays in `cursor_row`/`cursor_col`
    /// plus `selection`; these are the *extra* carets. Empty in the common
    /// single-cursor case, which keeps every existing edit path unchanged.
    /// Each entry is a full selection (anchor and head); head is that caret's
    /// cursor. Edits apply to the primary and all of these as one undo step.
    pub carets: Vec<EditorSelection>,
    /// Linked editing ranges (#254): equal-length single-line spans the
    /// server says rename together (paired HTML/JSX tags). While the
    /// caret edits inside one, `mirror_linked_edit` replays the change
    /// onto the others; anything structurally surprising (multi-row
    /// edit, invalid tag character, missed edits) drops the set — the
    /// next caret-idle round trip re-establishes it.
    linked_ranges: Vec<LinkedRange>,
    /// Char-length snapshot of every row a linked range sits on, taken
    /// when the set was installed/re-synced; the single changed row is
    /// how the mirror finds the edit and its length delta.
    linked_rows: Vec<(usize, usize)>,
    /// The `edit_seq` the stored positions describe. The mirror only
    /// trusts a single-step advance; anything else clears the set.
    linked_seq: u64,
    /// Cursor position at the last undo-push, i.e. where the last edit
    /// began (pre-edit coordinates) — the mirror's edit locator.
    pub last_edit_origin: (usize, usize),
    /// Expand/Shrink Selection stacks (#254): one per cursor (primary
    /// first, then `carets` in order), each recording the selections the
    /// gesture stepped through so shrink retraces exactly. Valid only
    /// while `edit_seq` matches and every cursor's CURRENT selection
    /// still equals its stack's current step — any edit, click, or
    /// caret reshuffle fails the check and the next gesture rebuilds
    /// from scratch (the `tick_occurrences` invalidation posture).
    select_expand: Option<SelectExpandStacks>,
    /// The last typed character and the `edit_seq` it produced — the
    /// on-type formatting trigger detector (#254). Stale once any other
    /// edit bumps the seq.
    pub last_typed: Option<(char, u64)>,
    /// The whitespace-only line Enter just indented (#956): its row, how
    /// many characters of indentation it got, and the `edit_seq` that left
    /// it so. The indentation is cleared again, as VS Code's
    /// `trimAutoWhitespace` does, once the caret leaves the line with
    /// nothing typed on it.
    auto_indent: Option<(usize, usize, u64)>,
    /// Collaborators' caret positions in this file (multiplayer sessions,
    /// docs/MULTIPLAYER.md): (row, char col, participant color). Painted as
    /// colored block cells like secondary carets; the App rebuilds this
    /// before each render from the session roster.
    pub ghost_carets: Vec<(usize, usize, Color)>,
    /// Name tags for ghost carets still inside their fade window (VS Code
    /// Live Share style): (caret row, caret char col, name, participant
    /// color). Painted on the visual row above the caret, falling back
    /// below when the caret sits on the viewport's top row. The App
    /// rebuilds this before each render alongside `ghost_carets`.
    pub ghost_caret_labels: Vec<(usize, usize, String, Color)>,
    /// The 0-based buffer line wearing the AI-stream stop button in the
    /// sign margin (`croft pair`): clicking it — or Cmd+K X — cancels the
    /// stream and reverts the streamed text. The App rebuilds this before
    /// each render from the stream state and the pilot's ghost caret.
    pub stream_stop_line: Option<usize>,
    /// The AI pair programmer's comment boxes for this buffer: each renders
    /// as an unnumbered block between its anchor line and the next line
    /// (title, body, reply field + Ignore button). The App rebuilds this per
    /// tick from the pilot host's anchored-note snapshot for the active
    /// file; boxes belong to no buffer line and never touch the text.
    pub comment_boxes: Vec<CommentBox>,
    /// The reply draft riding the focused box's footer field. While set,
    /// the App routes typing here instead of the buffer.
    pub comment_focus: Option<CommentFocus>,
    /// Enclosing scope header lines to pin at the top of the viewport (VS Code
    /// "Sticky Scroll"), outermost first, set by `App` from the outline scope
    /// chain of the top visible line. Empty disables the feature (e.g. wrap
    /// mode, no symbols, or nothing scrolled off).
    pub sticky_lines: Vec<u32>,
    /// Screen `(row, logical line)` pairs the sticky bar painted last render, so
    /// `App` can map a click on a pinned header to a jump.
    sticky_click_rows: Vec<(u16, u32)>,
    /// Anchor of an in-progress column (box) selection (VS Code's Shift+Alt+drag),
    /// as a `(row, col)` buffer position. `Some` while the box drag is live; each
    /// drag rebuilds `carets` from the rectangle between it and the pointer.
    box_anchor: Option<(usize, usize)>,
    /// The keyboard column selection (#1560) the carets were last built
    /// from: the box's anchor and head, and the primary caret and caret
    /// count it left. The next column-select key grows this box while the
    /// carets are still the ones it made, and starts a new one at the caret
    /// once anything else moved them.
    column_box: Option<ColumnBox>,
    /// VS Code's Column Selection Mode (#1560): while on, Shift+arrows grow
    /// a column selection instead of a stream one.
    pub column_selection_mode: bool,
    /// Active snippet tab-stop session (VS Code snippet expansion). `Some` while
    /// the caret is cycling through `$1`, `$2`, … stops of a just-expanded
    /// snippet; Tab advances, and any structural key cancels it. See
    /// [`Editor::expand_snippet`].
    snippet: Option<SnippetSession>,
    /// Currently-collapsed fold headers, as 0-based logical line indexes. A
    /// header's body (the more-indented lines below it, up to `fold_range`'s
    /// end) is hidden from the render while its index is in this set. Purely a
    /// view: the buffer is untouched, so every edit/undo path ignores it.
    folded: std::collections::BTreeSet<usize>,
    /// Merged, ascending, disjoint spans of lines hidden by `folded`, rebuilt
    /// only when the fold set or the buffer changes. `is_line_hidden` runs on
    /// every rendered row of every frame, and deriving it from `folded` there
    /// meant re-scanning each folded region per row — Fold All on a large file
    /// turned one frame into millions of iterations.
    hidden_ranges: Vec<(usize, usize)>,
    /// `self.lines.len()` at the moment `folded` was last set. Fold headers are
    /// line indexes, so an insert/delete anywhere shifts them; when the count
    /// no longer matches, the render drops every fold rather than hide the
    /// wrong lines. ponytail: whole-buffer invalidation, not per-fold anchor
    /// tracking; upgrade to sticky anchors if folds-survive-edits is wanted.
    fold_epoch_lines: usize,
    /// Server fold spans (#254), authoritative for `fold_range` while
    /// present AND measured against the current line count. `None` (no
    /// capable server, or none answered yet) keeps the indentation /
    /// marker fallback in charge — the two-phase posture of the outline.
    lsp_folds: Option<Vec<crate::lsp::manager::FoldingRangeItem>>,
    /// `self.lines.len()` when `lsp_folds` was applied; a mismatch means
    /// the spans predate an edit and are ignored until the next reply.
    lsp_folds_lines: usize,
    /// Fallback fold table beyond plain indentation: `#region` /
    /// `// region` marker pairs (kind Region) and runs of full-line
    /// comments (kind Comment). Rebuilt lazily per `edit_seq` by
    /// `refresh_fold_tables`, read by `fold_range` / `fold_kind_at`.
    fallback_kind_folds: Vec<(usize, usize, crate::lsp::manager::FoldRangeKind)>,
    /// The `edit_seq` `fallback_kind_folds` was built for (`None` = never).
    fallback_folds_seq: Option<u64>,
    /// True when this tab is the single replaceable "preview" slot. Single-
    /// click / plain-Enter opens replace the preview tab's contents in place;
    /// double-click / Ctrl+Enter / typing into the buffer pin the tab
    /// (preview = false) so subsequent previews don't overwrite it.
    pub preview: bool,
    /// `Some(n)` for a tab made by File: New Untitled Text File (#1580),
    /// shown as `Untitled-n` until Save As gives it a path.
    pub untitled: Option<usize>,
    /// Set on a scratch tab (`open_text_buffer`): text croft made, such as
    /// a past version, a Search Editor or a commit, whose `path` is only
    /// its label (#951). No save writes it: written, the label became a
    /// junk file in croft's working directory. Save As gives it a path.
    pub scratch: bool,
    /// The selection Add Selection to Next Find Match last made from the
    /// word at the caret (#1558). While the primary selection is that one,
    /// further matches are whole words, as in VS Code; a selection made by
    /// hand matches as plain text, so `oo` is found inside every `foo`.
    /// Paired with the `edit_seq` it was made at; dropped as soon as the
    /// selection is seen to differ (see `sync_occurrence_word_sel`), so a
    /// later hand-made selection at the same place is not mistaken for it.
    occurrence_word_sel: Option<(EditorSelection, u64)>,
    /// True when the user has pinned this tab (VS Code "Pin"). Pinned tabs are
    /// kept leftmost in the strip, survive Close Others / Close to the Right,
    /// and show a thumb-tack glyph in place of the close `\u{2715}`. A pinned
    /// tab is never the replaceable preview slot (pinning clears `preview`).
    pub pinned: bool,
    /// Set on a symbol tab (#369): the tab holds the whole file, as a split
    /// does, but shows, scrolls and edits only this symbol's lines.
    pub symbol_view: Option<crate::symbol_range::SymbolView>,
    /// The `edit_seq` at which the App last mirrored this buffer against its
    /// symbol-tab siblings. `None` = not (yet) mirrored with them, so this
    /// buffer is a copy that may predate them and never a mirror source.
    pub mirror_seq: Option<u64>,
    /// Id of this buffer's latest undo step (or undo/redo), 0 before any.
    /// A mirrored sibling takes one step per source step, so undo walks it
    /// back as far as the source's own undo would.
    pub undo_step_id: u64,
    /// The source step the last mirrored edit was folded into.
    mirrored_step: Option<u64>,
    /// Where the source's caret sat after the edit last mirrored in: the
    /// symbol view settles an ambiguous edit against it (#369).
    pub mirror_caret: Option<(usize, usize)>,
    undo_stack: Vec<Snapshot>,
    /// States popped off `undo_stack` by `undo`, awaiting `redo`. Cleared by any
    /// fresh edit (`push_undo`) so a new edit branches history like VS Code.
    redo_stack: Vec<Snapshot>,
    last_edit_kind: Option<EditKind>,
    /// The group of the remote edit that opened the undo step on top of
    /// the stack (#1207): further remote spans of that group join the step
    /// instead of opening their own. Any other step, undo or redo ends it; a
    /// peer's edit, which opens no step (#919), does not.
    remote_undo_group: Option<u64>,
    /// Open from [`Self::begin_undo_group`] to [`Self::end_undo_group`]
    /// (#934), holding the [`Self::undo_step_id`] of the step its edits are
    /// filling (`None` before its first edit).
    undo_group: Option<Option<u64>>,
    lang: Option<LangKind>,
    /// The language came from "Change Language Mode" (#1574), not from the
    /// file's name, so a `files_associations` change leaves it alone.
    language_by_hand: bool,
    /// Explicit indentation preference set from the status-bar pill. `None`
    /// falls back to the language default (2 spaces for YAML, 4 otherwise);
    /// `Some` pins spaces-vs-tabs and width for newly typed indentation.
    indent_override: Option<IndentStyle>,
    /// Indentation detected from the buffer's own content on open (VS
    /// Code's `editor.detectIndentation`): `Some` when the file showed a
    /// clear preference, `None` when it gave no signal. The manual
    /// status-bar override always wins over this.
    detected_indent: Option<IndentStyle>,
    /// Properties from the `.editorconfig` files covering this buffer's
    /// path, resolved on open. They outrank `detected_indent` (a project
    /// that states its style must beat a guess at it) but not the manual
    /// status-bar `indent_override`, which is the user overruling both.
    editorconfig: crate::editorconfig::Props,
    /// Line-ending style, detected on open and applied on save. Surfaced in the
    /// status bar; the user can switch it there.
    pub eol: LineEnding,
    /// Text encoding the buffer was decoded from and is re-encoded to on save.
    /// Defaults to UTF-8; the status bar's "Reopen with Encoding" switches it.
    pub encoding: &'static encoding_rs::Encoding,
    /// Whether the file on disk began with a byte-order mark. `decode` strips
    /// it, so without remembering it every save silently dropped it.
    bom: bool,
    /// Whether the file on disk ended with a line break. The buffer keeps
    /// no line for it (a trailing newline is not a line of its own), so
    /// without remembering it every save stripped it: `a\nb\n` came back
    /// as `a\nb`.
    final_newline: bool,
    /// Whether decoding the file met bytes invalid in its encoding, now
    /// shown as U+FFFD. Saving writes those as `EF BF BD`, destroying the
    /// original bytes, so it takes the same explicit consent as an encoding
    /// that cannot represent the text.
    pub decode_lossy: bool,
    /// Indentation guides (VS Code `editor.guides.indentation`): dim vertical
    /// lines at each indent level in a line's leading whitespace, with the
    /// cursor's block highlighted. App-synced from prefs; on by default.
    pub show_indent_guides: bool,
    /// Home walks back one wrapped row per press on a continuation row's
    /// start (`home_walks_wrapped_rows`, #1569). Off is VS Code's behaviour.
    pub home_walks_rows: bool,
    /// Ruler columns (#1524, VS Code `editor.rulers`). App-synced from
    /// prefs; empty by default.
    pub rulers: Vec<u16>,
    /// Bracket-pair colorization (#131, VS Code on-by-default since 1.67):
    /// per-line `(char column, colour index)` pairs for every `()[]{}` outside
    /// strings and comments, colour cycling by nesting depth
    /// (`UNEXPECTED_BRACKET` marks an unmatched closer). Rebuilt with the
    /// syntax spans by every syntax pass and carried through edits between
    /// passes, never per frame. App-synced from prefs; on by default.
    pub show_bracket_colors: bool,
    bracket_colors: BracketColors,
    /// Byte ranges of every string and comment in the buffer (lines joined
    /// by `\n`), from the same highlight pass as `bracket_colors`. Quote
    /// auto-pairing reads it to see a string or block comment an earlier
    /// line opened, which a scan of the caret's line alone cannot (#844).
    protected_ranges: Vec<(usize, usize)>,
    /// Whether `protected_ranges` describes the text as it is now. Byte
    /// ranges are not carried through edits the way spans are (#850), so an
    /// edit leaves them stale until the next pass over the edited text lands.
    protected_current: bool,
    /// Inline color-literal decorations (`naumovs.color-highlight`'s row in
    /// `vscode_extensions.rs`):
    /// per line, the `(start char col, end char col, background, foreground)`
    /// of every recognized `#hex` / `rgb()` / `hsl()` color literal, painted
    /// as a background tint over the literal's own text. Language-agnostic
    /// (a plain scan, not gated on any grammar) so it also lights up color
    /// constants in Rust/Python/JSON/Markdown, unlike the LSP-driven
    /// document-color `■` swatch (#254), which only fires where a language
    /// server resolves one. Rebuilt in `recompute_highlights`, on by default.
    show_color_swatches: bool,
    color_swatches: Vec<Vec<(usize, usize, Color, Color)>>,
    /// Draw an image tab with half-block cells (#1591): set by the app on a
    /// terminal with no inline-image protocol (tmux, plain SSH, GNOME
    /// Terminal), where the header line was all the tab showed.
    pub cell_images: bool,
    /// The last half-block rendering, kept until the image, the area or
    /// the background changes, so a redraw does not decode the file again.
    cell_image: Option<CellImage>,
    /// The pointer's screen cell, handed in by [`EditorTabs`] each frame so
    /// the Markdown preview can lift the link under it (#1939). `None` when
    /// the pointer is off-screen or unknown.
    pub pointer: Option<(u16, u16)>,
    /// Whether the overview ruler paints its whole-file ticks over the
    /// vertical scrollbar track. On by default, like VS Code's; the palette's
    /// `View: Toggle Overview Ruler` turns it off for a plain bar.
    pub overview_ruler: bool,
    /// Whitespace glyph rendering (#133); app-synced from prefs.
    pub whitespace_mode: WhitespaceMode,
    /// What the gutter numbers each line with (#1576, VS Code's
    /// `editor.lineNumbers`); app-synced from prefs.
    pub line_numbers: LineNumbers,
    /// Starting ignore-whitespace mode for diffs opened in this editor
    /// (#258, `diff.ignore_whitespace`); app-synced from prefs. Note this is
    /// unrelated to `whitespace_mode` above, which paints glyphs.
    pub diff_ws_default: crate::widgets::diff::DiffWhitespace,
    /// Debugger inline values (#135, VS Code `debug.inlineValues`): 0-based
    /// line → composed "name = value" trailer, rebuilt by the app on every
    /// stop from the VARIABLES data and cleared on resume/step/terminate.
    /// Painted like the blame trailer; the cursor-line blame yields to it.
    pub inline_values: std::collections::BTreeMap<usize, String>,
    /// Live Run's latest report for this buffer (`crate::live_run`): value
    /// trailers and line coverage, set by the app when a run lands. Each
    /// line paints only while it still matches the text that ran.
    pub live_run: Option<crate::live_run::View>,
    /// Per-tab override for soft-wrap (VS Code "View: Toggle Word Wrap",
    /// Alt+Z). `None` means follow the language default (`wrap_enabled`
    /// wraps Markdown only); `Some(true)`/`Some(false)` force it on/off for
    /// this buffer. Reset to `None` on every `open` so each file starts at
    /// its language default.
    wrap_override: Option<bool>,
    highlights: Vec<Vec<HiSpan>>,
    /// tailspin's spans for each distinct line of a plain `.log` buffer
    /// (#1537), keyed by the line's text so an edit re-highlights only the
    /// lines it touched. Theme-free: colours resolve on every recompute.
    log_spans: std::collections::HashMap<String, Vec<crate::ansi_text::AnsiSpan>>,
    /// The background syntax pass in flight, if any (#850). An edit never
    /// waits on tree-sitter for a large file: `recompute_highlights` carries
    /// the current spans through the edit and hands the text to a worker
    /// thread, and `poll_highlights` swaps the result in when it lands. One
    /// pass at a time: edits made while it runs are picked up by the pass
    /// that follows it, so a burst of typing costs a pass or two, not one
    /// per key.
    highlight_job: Option<HighlightJob>,
    /// Counts `recompute_highlights` calls, so a landing pass knows whether
    /// the buffer was edited after it started.
    highlight_requests: u64,
    /// The text `highlights` and `bracket_colors` describe, line for line:
    /// what an edit is diffed against to carry them over. Packed (#1707):
    /// a second `Vec<String>` of a large file cost twice its text.
    highlight_base: PackedLines,
    /// How long the last pass took. An edit waits for its pass only when the
    /// last one fit the budget, so a small file still colors each keystroke
    /// in the same frame and a large one never makes it wait.
    highlight_cost: std::time::Duration,
    /// How long an edit may wait for its pass before painting the carried
    /// spans. `None` waits for it to finish: tests, which assert colors right
    /// after an edit, and views built off the UI thread, which no tick polls.
    highlight_budget: Option<std::time::Duration>,
    /// Bumped whenever a pass lands, so a cache keyed on the text (the
    /// minimap's baked strip) repaints in the new colors.
    highlight_generation: u64,
    /// LSP semantic-token overlay decoded per line (byte offsets within
    /// the line), painted over `highlights` at render so a parameter (and
    /// other resolved symbols) keep their color everywhere they appear,
    /// not just at the declaration. Empty until the server replies. This
    /// is the editor half of the VS Code / Zed "combined" model.
    semantic_overlay: Vec<Vec<HiSpan>>,
    /// The raw last semantic-token batch (relative-encoded data + the
    /// server legend), retained so the overlay can be re-decoded against
    /// the buffer after each edit. `semantic_path` records which file the
    /// batch is for, so a stale batch is ignored after a tab switch.
    semantic_data: Vec<u32>,
    semantic_legend: Option<std::sync::Arc<Vec<String>>>,
    semantic_path: Option<PathBuf>,
    /// Whether the retained batch covers the WHOLE document (a
    /// `semanticTokens/full` reply) rather than just the opening viewport (a
    /// `semanticTokens/range` reply). Once a full batch lands for a file, a
    /// late range reply for that same file is dropped so it cannot blank the
    /// off-screen colour the full batch painted.
    semantic_is_full: bool,
    /// Diagnostics for the loaded file, retained with their raw LSP UTF-16
    /// positions so `diagnostic_spans` can be re-decoded against the buffer.
    /// `diagnostics_path` guards a stale batch from underlining the wrong file
    /// after a tab switch, exactly as `semantic_path` does for tokens.
    diagnostics: Vec<crate::lsp::manager::Diagnostic>,
    diagnostics_path: Option<PathBuf>,
    /// `edit_seq` when `diagnostics` were applied. After a later edit their
    /// positions and `data` describe text that has changed (#1030).
    diagnostics_seq: u64,
    /// Decoded per-logical-line underline runs `(start_char, end_char,
    /// severity)`, recomputed from `diagnostics` whenever they or the buffer
    /// change. The render loop paints a coloured underline over each run.
    diagnostic_spans: Vec<Vec<(usize, usize, crate::lsp::manager::DiagnosticSeverity)>>,
    /// Per-logical-line runs of tagged diagnostics `(start_char, end_char,
    /// tags)` (#1522): Unnecessary code is drawn faded and Deprecated code
    /// struck through, on top of (or, for a hint, instead of) an underline.
    diagnostic_tag_spans: Vec<Vec<(usize, usize, crate::lsp::manager::DiagnosticTags)>>,
    /// Inlay hints for the loaded file, retained with their raw LSP UTF-16
    /// positions so `inlay_spans` can be re-decoded against the buffer after
    /// each edit. `inlay_path` guards a stale batch from annotating the wrong
    /// file after a tab switch, exactly as `diagnostics_path` does.
    inlay_hints: Vec<crate::lsp::manager::InlayHintItem>,
    inlay_path: Option<PathBuf>,
    /// The selected SARIF result's analysis steps in this file (#577), as
    /// `(0-based line, label)`, drawn after their lines' text. `step_path`
    /// guards them like `inlay_path`: steps belong to one file only.
    step_marks: Vec<(usize, String)>,
    step_path: Option<PathBuf>,
    /// Occurrences of the symbol under the caret (LSP documentHighlight),
    /// already converted to `(row, start_char, end_char, write)` against the
    /// current buffer. Cleared on every edit — the server's columns are
    /// stale the moment the text changes — and replaced when the app's next
    /// idle-caret request answers.
    occurrences: Vec<(usize, usize, usize, bool)>,
    /// Decoded per-logical-line virtual-cell runs `(char_col, label,
    /// swatch)`, sorted by column: LSP inlay hints (swatch `None`, dim
    /// italic) and document-color swatches (#254: a `■` whose fg IS the
    /// color). The render loop splices each label into the row at its
    /// anchor; every overlay painter, the caret, and mouse mapping
    /// translate buffer columns past them.
    inlay_spans: Vec<Vec<(usize, String, Option<Color>)>>,
    /// The server's resolved document links (#254): raw UTF-16 ranges +
    /// target URIs, path-stamped like `inlay_path`. Consulted by the
    /// Ctrl+click dispatch BEFORE firing Go to Definition.
    doc_links: Vec<crate::lsp::manager::DocumentLinkItem>,
    doc_links_path: Option<PathBuf>,
    /// The document's color values (#254): raw UTF-16 positions + RGB,
    /// re-decoded like `inlay_hints`; feeds the `■` swatch spans and the
    /// Change Color Presentation picker's range lookup.
    color_infos: Vec<crate::lsp::manager::ColorItem>,
    /// The file `color_infos` describes (same contract as `inlay_path`).
    color_path: Option<PathBuf>,
    registry: LangRegistry,
    /// When set, every occurrence of this string in the visible portion of
    /// the buffer is overpainted with the search-match style after the
    /// syntax-highlighted line is laid down. Mirrors the highlight in the
    /// Search panel so the user sees their query lit up in the file too.
    pub search_highlight: Option<String>,
    /// Toggle state that drives `search_highlight` matching: case
    /// sensitivity, whole-word boundaries, regex. Mirrors `SearchPanel.opts`
    /// so the editor's yellow highlight stays consistent with what the
    /// search panel claims is a match.
    pub search_highlight_opts: crate::widgets::search::SearchOpts,
    /// When the inline editor Find overlay (Cmd+F) is open and the cursor
    /// has landed on a match, this records (row, col_chars, len_chars) so
    /// the renderer can paint the active match in a stronger orange while
    /// every other match keeps the regular yellow. Mirrors VS Code's
    /// "current match" highlight. Cleared when the find bar closes or when
    /// the highlight term is cleared.
    pub active_search_match: Option<(usize, usize, usize)>,
    /// Find in Selection (#1577): while the find bar is limited to a range,
    /// only matches inside it are highlighted.
    pub find_range: Option<crate::widgets::editor_find::FindRange>,
    /// Some when this tab is a read-only image preview rather than a text
    /// buffer. The text fields (`lines`, undo, highlights, …) are left in
    /// their default empty state and the renderer paints metadata only;
    /// the actual pixels are emitted as an OSC-1337 inline image overlay
    /// by `App` after each frame.
    /// Rendered Markdown preview (Cmd/Ctrl+Shift+V) replacing the source view
    /// while set; rebuilt lazily when `built_seq` falls behind `edit_seq`.
    pub markdown_preview: Option<crate::markdown::MarkdownPreview>,
    /// Captured runs to show under their fences in the preview (#354).
    /// Owned by the app and copied in before a rebuild, because the editor
    /// has no view of which panes ran what.
    pub md_outputs: crate::markdown::BlockOutputs,
    pub image: Option<ImageView>,
    /// A reload's request that `open_pdf` come up on this page instead of
    /// page 1, so the reader's place survives an external rebuild in ONE
    /// rasterisation. Rendering page 1 first and re-rendering afterwards
    /// opened a window where a transient failure silently snapped the
    /// reader back to page 1 (#72). Set by `reload_from_disk`, consumed
    /// (or cleared) by the `open` it wraps.
    pdf_restore_page: Option<u32>,
    /// Read-only spreadsheet preview for `.csv` / `.tsv` / `.xlsx` / etc.
    /// Mutually exclusive with `image` and the text path; none of the
    /// editor's text fields are populated when this is `Some`.
    pub sheet: Option<crate::sheet::SheetView>,
    /// Read-only side-by-side diff view. Mutually exclusive with the text
    /// path, `image`, and `sheet` — when set the renderer paints two
    /// columns based on `diff.rows` and ignores `lines`.
    pub diff: Option<crate::widgets::diff::DiffData>,
    /// Read-only hex viewer (#172): the routing fallback for every file
    /// the text heuristic rejects, and the explicit "Reopen as Hex"
    /// target. Mutually exclusive with the text path and the other
    /// preview kinds; windowed IO, so the file is never loaded whole.
    pub hex: Option<crate::hex::HexView>,
    /// Rendered ANSI log view (#257): colour-bearing logs paint with the
    /// theme's ANSI palette instead of showing raw escapes. Windowed like
    /// [`crate::hex`], so a multi-gigabyte log opens instantly. Read-only;
    /// "Reopen as Text" (`force_text`) shows the raw bytes.
    pub log: Option<crate::log_view::LogView>,
    /// The top line a reload of the log is restoring (#1538), `usize::MAX`
    /// for "the end", while the reloaded view is still indexing: the line
    /// may lie past what the head pass reached, so it is settled again as
    /// the index grows. Dropped when the reader scrolls.
    log_reload_top: Option<usize>,
    /// Archive browser (#179): the member list of a zip/jar/whl/tar
    /// archive; Enter extracts one member to scratch and opens it
    /// through the normal dispatch. Read-only.
    pub archive: Option<crate::archive::ArchiveView>,
    /// A pull request under review (#365): its files, checks and viewed
    /// marks. Read-only; the tab has no file behind it.
    pub pr_review: Option<crate::widgets::pr_review::PrReviewView>,
    /// A fleet run's results as tiles (#363).
    pub fleet: Option<crate::widgets::fleet::FleetView>,
    /// SARIF results viewer (#577): every result of a `.sarif` log, grouped
    /// and filterable; Enter opens a result's location. Read-only; "Reopen
    /// as Text" shows the JSON.
    pub sarif: Option<crate::sarif::view::SarifView>,
    /// Three-way merge editor (#253). UNLIKE the other view kinds this is
    /// not read-only and not in `has_non_text_view`: `lines` holds the
    /// editable Result and keeps the whole text path (LSP, undo, save);
    /// the renderer only carves the source panes off the top of the area.
    pub merge: Option<crate::merge_editor::MergeView>,
    /// Cursor row at the last undo-push (i.e. where the last edit began);
    /// the merge editor's region tracker keys its shift heuristic off it.
    pub merge_edit_row: usize,
    /// Per-tab "Reopen as Text" override (#175): when set, `open` skips
    /// every preview route (extension and sniffed alike) and lands in
    /// the text editor — an SVG's XML source, a workbook's bytes (which
    /// the binary heuristic then sends to hex). It STICKS across
    /// same-path reloads, so the FS-sync sweep cannot flip the tab back
    /// to a preview, and clears when the tab opens a different file.
    pub force_text: bool,
    /// Per-tab "Reopen as Preview" on a plain text file (#1537): `open`
    /// shows it in the rendered log view, tailspin's colours and all, even
    /// though it carries no escapes. Sticks across same-path reloads and
    /// clears with a different file, like `force_text`, which wins.
    pub force_log: bool,
    /// Notebook code cells (indices into `cells`) running or queued in the
    /// notebook's kernel: the preview shows them as `In [*]` (#355).
    pub notebook_running: Vec<usize>,
    /// Coverage marks for this file from the last coverage run (#263),
    /// painted in the git bar's lane while shown.
    pub coverage: Option<crate::testing::coverage::CoverageLens>,
    /// Hit-test rect for the "previous change" arrow painted in the diff
    /// header. Empty when the tab isn't a diff or the header was clipped.
    /// `App` consults this on left-click to jump to the previous hunk.
    pub diff_prev_arrow: Rect,
    /// Hit-test rect for the "next change" arrow painted in the diff
    /// header. Mirror of `diff_prev_arrow`.
    pub diff_next_arrow: Rect,
    /// Disk identity (mtime, len) captured the last time this tab was in
    /// sync with disk — i.e. at open and after a successful save. The FS
    /// sync sweep compares the file's current stamp against this to decide
    /// whether an external process changed the file underneath us. `None`
    /// for a tab with no file (the blank initial buffer).
    disk_stamp: Option<(SystemTime, u64)>,
    /// Set for a new file opened before it exists (#1157): its `None`
    /// stamp means "expected absent", so the file appearing on disk is an
    /// external change rather than a phantom one.
    disk_expect_absent: bool,
    /// Set when an external on-disk change is detected while this buffer has
    /// unsaved edits. Reloading would discard the user's edits and saving
    /// would clobber the external change, so neither happens automatically:
    /// the flag drives the conflict warning and makes `save_to_disk` refuse
    /// to overwrite until the user forces it.
    pub disk_conflict: bool,
    /// The sweep's last report of a failed reload or a deletion (#900), so
    /// it is not repeated while disk stays where it was reported.
    disk_report: Option<DiskReport>,
    /// Set when a save was refused because `encoding` cannot represent the
    /// buffer. Latches like `disk_conflict` so auto save stops retrying (and
    /// stops re-reporting) a file only an explicit Cmd+S can resolve; cleared
    /// by a successful write and by any reopen, which may change `encoding`.
    pub encoding_loss: bool,
    /// One-shot consent to write characters `encoding` cannot represent,
    /// answering an `EncodingLoss` refusal. Set by the explicit-save path
    /// only (never by auto save) and consumed by the write it authorises.
    pub lossy_save_armed: bool,
    /// The file refuses this user's writes (#1379): the tab wears a lock and
    /// auto save leaves it alone. Read at open and after each save.
    pub write_protected: bool,
    /// One-shot consent to overwrite the user's own write-protected file,
    /// answering a `WriteProtected` refusal. Set by the explicit-save path
    /// only and consumed by the write it authorises.
    pub overwrite_protected_armed: bool,
    /// When the buffer last changed, driving the auto-save delay. `None`
    /// until the first edit.
    pub last_edit_at: Option<std::time::Instant>,
    /// The caret position of the last buffer edit, for "Go to Last Edit
    /// Location" (Cmd+K Cmd+Q). Set beside `last_edit_at` in
    /// `mark_buffer_changed`.
    pub last_edit_pos: Option<(usize, usize)>,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            path: None,
            lines: Vec::new(),
            provenance: crate::provenance::Provenance::new(),
            route_note: None,
            provenance_synced_seq: None,
            breakpoints: std::collections::HashMap::new(),
            stop_line: None,
            unverified_breakpoints: std::collections::HashMap::new(),
            breakpoint_conditions: std::collections::HashMap::new(),
            breakpoint_logs: std::collections::HashMap::new(),
            breakpoint_hit_conditions: std::collections::HashMap::new(),
            bookmarks: std::collections::HashMap::new(),
            bookmark_shadow: None,
            bookmark_sync_paused: false,
            edit_seq: 0,
            symbol_view: None,
            mirror_seq: None,
            undo_step_id: 0,
            mirrored_step: None,
            mirror_caret: None,
            ruler_search_cache: None,
            ruler_starts_cache: None,
            bracket_pair_cache: None,
            collab_synced_seq: 0,
            collab_doc_gen: 0,
            git_head_lines: None,
            git_baseline_for: None,
            git_marks: std::collections::HashMap::new(),
            git_marks_seq: u64::MAX,
            auto_close_pairs: true,
            overwrite: false,
            save_cleanup: SaveCleanup::default(),
            auto_close_tags: true,
            auto_pair_at: None,
            multi_auto_pair: None,
            multi_typing: None,
            conflicts: Vec::new(),
            merge_action_spans: Vec::new(),
            code_lenses: Vec::new(),
            code_lens_spans: Vec::new(),
            ghost: None,
            conflicts_seq: u64::MAX,
            blame_lines: None,
            blame_for: None,
            blame_enabled: true,
            blame_span: None,
            provenance_overlay: false,
            scroll: 0,
            scroll_sub: 0,
            wrap_total_cache: Vec::new(),
            scroll_col: 0,
            cursor_row: 0,
            cursor_col: 0,
            focused: false,
            overlay_has_keyboard: false,
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            pdf_viewer_enabled: true,
            csv_viewer_enabled: true,
            dirty: false,
            save_seq: 0,
            status: String::from("No file open"),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            last_scrollbar: Rect::default(),
            last_hscrollbar: Rect::default(),
            last_text_rows: 0,
            hscroll_content_cols: None,
            last_wrap_rows: Vec::new(),
            last_gutter_width: 0,
            selection: None,
            carets: Vec::new(),
            linked_ranges: Vec::new(),
            linked_rows: Vec::new(),
            linked_seq: 0,
            last_edit_origin: (0, 0),
            select_expand: None,
            last_typed: None,
            auto_indent: None,
            ghost_carets: Vec::new(),
            ghost_caret_labels: Vec::new(),
            stream_stop_line: None,
            comment_boxes: Vec::new(),
            comment_focus: None,
            sticky_lines: Vec::new(),
            sticky_click_rows: Vec::new(),
            box_anchor: None,
            column_box: None,
            column_selection_mode: false,
            snippet: None,
            folded: std::collections::BTreeSet::new(),
            hidden_ranges: Vec::new(),
            fold_epoch_lines: 0,
            lsp_folds: None,
            lsp_folds_lines: 0,
            fallback_kind_folds: Vec::new(),
            fallback_folds_seq: None,
            preview: false,
            untitled: None,
            scratch: false,
            occurrence_word_sel: None,
            pinned: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit_kind: None,
            remote_undo_group: None,
            undo_group: None,
            lang: None,
            language_by_hand: false,
            indent_override: None,
            detected_indent: None,
            editorconfig: crate::editorconfig::Props::default(),
            eol: LineEnding::Lf,
            encoding: encoding_rs::UTF_8,
            bom: false,
            final_newline: false,
            decode_lossy: false,
            show_indent_guides: true,
            home_walks_rows: false,
            rulers: Vec::new(),
            show_bracket_colors: true,
            bracket_colors: Vec::new(),
            protected_ranges: Vec::new(),
            protected_current: false,
            show_color_swatches: true,
            color_swatches: Vec::new(),
            cell_images: false,
            cell_image: None,
            pointer: None,
            whitespace_mode: WhitespaceMode::default(),
            line_numbers: LineNumbers::default(),
            diff_ws_default: crate::widgets::diff::DiffWhitespace::default(),
            inline_values: std::collections::BTreeMap::new(),
            live_run: None,
            wrap_override: None,
            highlights: Vec::new(),
            log_spans: std::collections::HashMap::new(),
            highlight_job: None,
            highlight_requests: 0,
            highlight_base: PackedLines::default(),
            highlight_cost: std::time::Duration::ZERO,
            highlight_budget: (!cfg!(test)).then_some(HIGHLIGHT_INLINE_BUDGET),
            highlight_generation: 0,
            semantic_overlay: Vec::new(),
            semantic_data: Vec::new(),
            semantic_legend: None,
            semantic_path: None,
            semantic_is_full: false,
            diagnostics: Vec::new(),
            diagnostics_path: None,
            diagnostics_seq: 0,
            diagnostic_spans: Vec::new(),
            diagnostic_tag_spans: Vec::new(),
            inlay_hints: Vec::new(),
            inlay_path: None,
            step_marks: Vec::new(),
            step_path: None,
            occurrences: Vec::new(),
            inlay_spans: Vec::new(),
            doc_links: Vec::new(),
            doc_links_path: None,
            color_infos: Vec::new(),
            color_path: None,
            registry: LangRegistry::new(),
            overview_ruler: true,
            search_highlight: None,
            search_highlight_opts: crate::widgets::search::SearchOpts::default(),
            active_search_match: None,
            find_range: None,
            markdown_preview: None,
            md_outputs: crate::markdown::BlockOutputs::new(),
            image: None,
            pdf_restore_page: None,
            sheet: None,
            diff: None,
            hex: None,
            log: None,
            log_reload_top: None,
            archive: None,
            pr_review: None,
            fleet: None,
            sarif: None,
            merge: None,
            merge_edit_row: 0,
            force_text: false,
            force_log: false,
            notebook_running: Vec::new(),
            coverage: None,
            diff_prev_arrow: Rect::default(),
            diff_next_arrow: Rect::default(),
            disk_stamp: None,
            disk_expect_absent: false,
            disk_conflict: false,
            disk_report: None,
            encoding_loss: false,
            lossy_save_armed: false,
            write_protected: false,
            overwrite_protected_armed: false,
            last_edit_at: None,
            last_edit_pos: None,
        }
    }

    pub fn set_search_highlight(&mut self, term: Option<String>) {
        self.search_highlight = term.filter(|s| !s.is_empty());
    }

    fn mark_buffer_changed(&mut self) {
        self.dirty = true;
        self.last_edit_at = Some(std::time::Instant::now());
        self.last_edit_pos = Some((self.cursor_row, self.cursor_col));
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.hscroll_content_cols = None;
        self.wrap_total_cache.clear();
        self.occurrences.clear();
        // Link ranges were measured against the pre-edit text; an edit shifts
        // the spans they point at, so a Ctrl+click would follow the wrong one
        // until the next server batch lands.
        self.doc_links.clear();
        self.doc_links_path = None;
        // Lossy-save consent named the unmappable characters the buffer held
        // when the prompt fired; an edit changes what a write would destroy,
        // so stale consent must not carry over (unlike `force_save_armed`,
        // whose overwrite consent is about the DISK state, which typing does
        // not touch). Clearing the latch also lets auto save retry a buffer
        // the user fixed by deleting the offending characters.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        // Marks follow their lines through every edit (inserts, deletes,
        // pastes, joins); see `sync_bookmarks_to_buffer`.
        self.sync_bookmarks_to_buffer();
    }

    #[cfg(test)]
    pub(crate) fn lines_for_test(&self) -> &[String] {
        &self.lines
    }

    fn line_char_len(&self, row: usize) -> usize {
        self.lines.get(row).map(|s| s.chars().count()).unwrap_or(0)
    }

    fn byte_index(&self, row: usize, col: usize) -> usize {
        let line = &self.lines[row];
        line.char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(line.len())
    }

    /// Any user-driven mutation pins the buffer (matches VS Code: typing in
    /// a preview tab promotes it to a regular tab so a subsequent
    /// single-click on another file can't silently replace your edits).
    fn pin_on_edit(&mut self) {
        self.preview = false;
        // Every mutating entry point calls this, so it is the one place that
        // can guarantee no edit ever lands on a line hidden inside a fold.
        self.reveal_cursor_fold();
    }

    /// Whether the primary caret or an extra one is on `line`.
    fn is_caret_line(&self, line: usize) -> bool {
        line == self.cursor_row || self.carets.iter().any(|c| c.head.0 == line)
    }
}

/// What a background syntax pass computed, and for which text.
struct HighlightPass {
    kind: LangKind,
    /// The text highlighted, line for line: the base the result is carried
    /// from when edits landed while it ran.
    lines: PackedLines,
    spans: Vec<Vec<HiSpan>>,
    brackets: BracketColors,
    /// Byte ranges of the strings and comments the grammar found (#844).
    protected: Vec<(usize, usize)>,
    cost: std::time::Duration,
}

/// Build a Vec<Span> from a line and its byte-range highlight spans.
/// Byte index of the character at character index `chars_in`, or the line's
/// byte length when `chars_in` falls past the end. Used to slice a line at
/// a horizontal scroll offset measured in characters.
fn byte_index_of_char(line: &str, chars_in: usize) -> usize {
    line.char_indices()
        .nth(chars_in)
        .map(|(b, _)| b)
        .unwrap_or(line.len())
}

/// Character column for the LSP UTF-16 offset `u16_off` within `line`. LSP
/// positions count UTF-16 code units; the editor lays out by character, so a
/// line containing astral-plane characters (which take two UTF-16 units) needs
/// this conversion. An offset past the line clamps to the line's char length.
pub(crate) fn utf16_to_char_col(line: &str, u16_off: u32) -> usize {
    let mut units: u32 = 0;
    for (char_idx, ch) in line.chars().enumerate() {
        if units >= u16_off {
            return char_idx;
        }
        units += ch.len_utf16() as u32;
    }
    line.chars().count()
}

thread_local! {
    /// Cells one `\t` is drawn as: the buffer's "Tab Size" (status bar), set
    /// by [`Editor::sync_tab_cells`] before anything measures or paints text.
    static TAB_CELLS: std::cell::Cell<usize> = const { std::cell::Cell::new(1) };
}

use crate::widgets::editor_find::find_window;

/// A merge-conflict accept button (#1169): its label, the shorter one a row
/// too narrow for every full label gets, and its fill.
struct AcceptChip {
    label: String,
    short: String,
    fill: Color,
}

/// Multi-buffer editor: a stack of `Editor` instances with a single active
/// one, plus a 1-row clickable tab strip rendered above the active editor.
/// `Deref`/`DerefMut` aim at the active editor so existing call sites that
/// were written for a single `Editor` continue to work without rewrites.
/// One segment of the breadcrumb bar (VS Code's editor breadcrumbs): a path
/// folder, the file name, or an enclosing symbol. `target` is the caret
/// position a click jumps to; `None` for the informational path/file crumbs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub target: Option<(u32, u32)>,
    /// The glyph and colour a `surfaces` theme draws before the label: the
    /// file's type icon on the file crumb, the symbol kind's on a symbol
    /// crumb, none on a folder. Not part of the crumb's hit range.
    pub icon: Option<(char, Color)>,
}

/// A rendered breadcrumb crumb's hit-test span: `(x_start, width, jump target)`.
type BreadcrumbRange = (u16, u16, Option<(u32, u32)>);

pub struct EditorTabs {
    /// The active editor's rect below the tab strip and breadcrumbs, from
    /// the last frame: where a view standing in for it (the history
    /// scrubber's, #371) paints.
    pub last_body: Rect,
    pub editors: Vec<Editor>,
    active: usize,
    /// Breadcrumb segments for the active file, set by `App` each frame from
    /// the workspace-relative path and the outline scope chain at the caret.
    /// Empty hides the bar (e.g. the welcome screen or an unsaved buffer).
    pub breadcrumbs: Vec<Crumb>,
    /// Per-crumb `(x_start, width, target)` recorded by the most recent render,
    /// so `App::handle_mouse` can map a click on the breadcrumb row to a jump.
    breadcrumb_ranges: Vec<BreadcrumbRange>,
    /// Screen row the breadcrumb bar rendered on, or `None` when it was hidden.
    breadcrumb_y: Option<u16>,
    /// Per-tab on-screen `(x_start, width)` recorded by the most recent
    /// render. `tab_at(col, row)` reads this to map mouse clicks to tab
    /// indices.
    tab_screen_ranges: Vec<(u16, u16)>,
    /// Per-tab absolute column where the close `\u{2715}` glyph lives.
    /// `0` means "no close button rendered for this tab" (e.g. when the
    /// tab is the only one — closing it isn't allowed so we hide the X).
    tab_close_x: Vec<u16>,
    tab_strip_y: u16,
    /// Scroll and button state of the tab strip, boxed to keep this large
    /// struct (a layout-tree leaf) under clippy's size-difference limit.
    tab_strip: Box<TabStripState>,
    /// The full pane area (tab strip + body) from the most recent render.
    /// Used by `App::handle_mouse` for hit-testing — the active editor's
    /// own `last_area` only covers the body below the strip.
    pub last_full_area: Rect,
    /// Source of truth for the search-match term that's currently being
    /// painted in every tab's body. Each `Editor.search_highlight` is a
    /// copy kept in sync with this; storing it here lets a freshly-created
    /// editor (e.g. after closing the previous tab and opening a new file
    /// from a search hit) inherit the term without the App needing to
    /// re-call `set_search_highlight` after every open.
    search_highlight_term: Option<String>,
    /// Toggle state matching `search_highlight_term`. Same propagation
    /// strategy: every newly-created editor inherits these.
    search_highlight_opts: crate::widgets::search::SearchOpts,
    /// Last pointer cell, fed in by `App` before each render so the tab
    /// strip can paint a hover lift on the tab body under the pointer and a
    /// pill behind the close `\u{2715}` cell when the pointer rests on it.
    /// `None` when the pointer is off-screen or unknown.
    pub hover_pointer: Option<(u16, u16)>,
    /// Handed to the active editor each frame: see [`Editor::cell_images`].
    pub cell_images: bool,
    /// The bookmarks of the tabs closed since the App last took them
    /// ([`Self::take_released_bookmarks`]), on the files' saved lines, so a
    /// file's marks outlive the last tab that held them.
    released_bookmarks: Vec<std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>>,
}

impl EditorTabs {
    pub fn new() -> Self {
        Self {
            last_body: Rect::default(),
            editors: vec![Editor::new()],
            active: 0,
            breadcrumbs: Vec::new(),
            breadcrumb_ranges: Vec::new(),
            breadcrumb_y: None,
            cell_images: false,
            tab_screen_ranges: Vec::new(),
            tab_close_x: Vec::new(),
            tab_strip_y: 0,
            tab_strip: Box::default(),
            last_full_area: Rect::default(),
            search_highlight_term: None,
            search_highlight_opts: crate::widgets::search::SearchOpts::default(),
            hover_pointer: None,
            released_bookmarks: Vec::new(),
        }
    }

    pub fn tab_count(&self) -> usize {
        self.editors.len()
    }

    /// Set (or clear) the search-match highlight term + opts for every
    /// open tab, so opening another file from search keeps the same query
    /// lit, clearing the search box wipes the highlights, and toggling a
    /// search mode (case / whole-word / regex) re-paints the file with
    /// the matching rule. Also persists term + opts so editors created
    /// after this call (e.g. after a close + reopen) inherit them.
    pub fn set_search_highlight(
        &mut self,
        term: Option<String>,
        opts: crate::widgets::search::SearchOpts,
    ) {
        let normalised = term.filter(|s| !s.is_empty());
        let cleared = normalised.is_none();
        self.search_highlight_term = normalised.clone();
        self.search_highlight_opts = opts;
        for ed in &mut self.editors {
            ed.search_highlight = normalised.clone();
            ed.search_highlight_opts = opts;
            if cleared {
                ed.active_search_match = None;
            }
        }
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The group's active tab, for callers that must act on the group without
    /// hoisting it into App::editor (e.g. a wheel scroll over an inactive
    /// split pane). editors is never empty, so this cannot fail.
    pub fn active_editor_mut(&mut self) -> &mut Editor {
        let active = self.active.min(self.editors.len().saturating_sub(1));
        &mut self.editors[active]
    }

    /// Wheel-scroll this group's active tab by notches (positive = down),
    /// mirroring what App does for the focused tab: a diff scrolls its own
    /// pane, a PDF preview steps pages (clamped to the document), a hex view
    /// scrolls by data rows, anything else scrolls text by notches lines.
    /// Keeping the modes here means a background split group behaves like the
    /// focused one instead of scrolling a buffer that isn't on screen.
    pub fn wheel_scroll_active(&mut self, notches: i32) {
        let ed = self.active_editor_mut();
        if let Some(diff) = ed.diff.as_mut() {
            if notches >= 0 {
                diff.scroll_down_by(notches.unsigned_abs() as usize);
            } else {
                diff.scroll_up_by(notches.unsigned_abs() as usize);
            }
            return;
        }
        if let Some(page) = ed.pdf_page() {
            // One page per wheel event, like the focused preview: the
            // caller's notches are text lines (3 a tick), not pages.
            let target = if notches >= 0 {
                page.saturating_add(1)
            } else {
                page.saturating_sub(1).max(1)
            };
            ed.set_pdf_page(target);
            return;
        }
        if let Some(view) = ed.hex.as_mut() {
            let rows = (view.layout.data_rows as usize).max(1);
            view.scroll_by(notches as i64, rows);
            return;
        }
        if notches >= 0 {
            ed.scroll_down(notches.unsigned_abs() as usize);
        } else {
            ed.scroll_up(notches.unsigned_abs() as usize);
        }
    }

    /// The tab strip label for the tab at `idx` (file name, diff pair, or
    /// "untitled"), for status messages. Empty string for an out-of-range idx.
    pub fn tab_display_label(&self, idx: usize) -> String {
        self.editors.get(idx).map(tab_label).unwrap_or_default()
    }

    /// The on-disk path backing the tab at `idx`, if any. `None` for an
    /// out-of-range index or a blank/untitled buffer. Used to seed the tab
    /// context-menu's path-bearing actions (Reveal in Finder, Copy Path).
    pub fn tab_path(&self, idx: usize) -> Option<PathBuf> {
        self.editors.get(idx).and_then(|e| e.path.clone())
    }

    /// The strip's display titles, disambiguated (#167); index-aligned
    /// with `iter_tabs` so projections (OPEN EDITORS) match the strip.
    pub fn tab_display_labels(&self) -> Vec<String> {
        disambiguated_tab_labels(&self.editors)
    }

    #[cfg(test)]
    pub fn tab_strip_y_for_test(&self) -> u16 {
        self.tab_strip_y
    }

    pub fn iter_tabs(&self) -> impl Iterator<Item = &Editor> {
        self.editors.iter()
    }

    /// Close the undo group of every tab that still has one open (#934): a
    /// vim change entered on one tab can be left with `Esc` on another, and
    /// an edit back on its tab that skips vim, such as a paste, would
    /// otherwise fold into the change. A tab with no group open is left as
    /// it is.
    pub fn end_open_undo_groups(&mut self) {
        for editor in &mut self.editors {
            if editor.undo_group.is_some() {
                editor.end_undo_group();
            }
        }
    }

    pub fn select(&mut self, idx: usize) -> bool {
        if idx >= self.editors.len() {
            return false;
        }
        self.editors[self.active].focused = false;
        self.active = idx;
        self.editors[self.active].focused = true;
        true
    }

    /// [`Editor::trim_auto_indent`] over every tab (#956): the active one
    /// keeps the line its caret is still on when `focused_group`, and every
    /// other tab has been switched away from, so its line was left.
    pub fn trim_auto_indents(&mut self, focused_group: bool) {
        let active = self.active;
        for (i, editor) in self.editors.iter_mut().enumerate() {
            editor.trim_auto_indent(!(focused_group && i == active));
        }
    }

    /// Sweep EVERY open tab (not just the active one) for external on-disk
    /// changes. Clean tabs are silently reloaded; dirty tabs are flagged as
    /// conflicts. This is the core of the FS-sync invariant: a file open in
    /// a background tab must reflect disk reality just like the focused one.
    /// `skip` exempts paths from the sweep: a collab guest never reloads
    /// shared files from disk — its replica is authoritative, and the
    /// owner's own reload reaches it as ops (docs/MULTIPLAYER.md, Phase D).
    pub fn reload_externally_changed_tabs(
        &mut self,
        skip: &dyn Fn(&Path) -> bool,
    ) -> ExternalReloadReport {
        let mut report = ExternalReloadReport::default();
        for ed in &mut self.editors {
            // Diff/image/sheet views don't carry an editable text buffer to
            // reload, and a path-less blank tab has nothing to sync.
            if ed.path.is_none() || ed.diff.is_some() {
                continue;
            }
            if ed.path.as_deref().is_some_and(skip) {
                continue;
            }
            let path = ed.path.clone();
            match ed.reload_or_flag_conflict() {
                ExternalChange::Reloaded => {
                    if let Some(p) = path {
                        report.reloaded.push(p);
                    }
                }
                ExternalChange::Conflict => {
                    if let Some(p) = path {
                        report.conflicts.push(p);
                    }
                }
                ExternalChange::ReloadFailed => {
                    if let Some(p) = path {
                        report.failed.push(p);
                    }
                }
                ExternalChange::Deleted => {
                    if let Some(p) = path {
                        report.deleted.push(p);
                    }
                }
                ExternalChange::Unchanged => {}
            }
        }
        report
    }

    /// Discard unsaved edits and reload the given paths from disk — the
    /// "Reload" resolution of an external-change conflict. Returns the paths
    /// actually reverted (a tab must still be open on each).
    pub fn revert_paths_to_disk(&mut self, paths: &[PathBuf]) -> Vec<PathBuf> {
        let mut reverted = Vec::new();
        for ed in &mut self.editors {
            let Some(p) = ed.path.clone() else { continue };
            if paths.iter().any(|q| q == &p) && ed.revert_to_disk().is_ok() {
                reverted.push(p);
            }
        }
        reverted
    }

    /// If any tab currently points at `old`, repoint it to `new`. The on-
    /// disk file has already been moved; this only updates the in-memory
    /// path so subsequent saves and the tab label track the new name.
    pub fn rename_open_path(&mut self, old: &Path, new: &Path) {
        for e in &mut self.editors {
            // Under a moved folder too: a tab of `dir/a.rs` follows `dir` to
            // its new place, or its next save recreates the old path.
            let moved = e
                .path
                .as_deref()
                .and_then(|p| p.strip_prefix(old).ok())
                .map(|rest| {
                    if rest.as_os_str().is_empty() {
                        new.to_path_buf()
                    } else {
                        new.join(rest)
                    }
                });
            if let Some(moved) = moved {
                e.path = Some(moved);
                // Re-anchor the disk stamp to the new path so the rename
                // isn't mistaken for an external content change on the next
                // FS-sync sweep.
                e.mark_synced_with_disk();
            }
        }
    }

    /// Open `path` in a brand-new tab inserted directly after the active
    /// one, then make that new tab active. Returns the result of the
    /// underlying `Editor::open` so the caller can surface errors.
    pub fn open_in_new_tab(&mut self, path: &Path) -> Result<()> {
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.open(path)?;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Insert a pinned tab for `path` without reading it (None: untitled),
    /// reusing the blank initial tab. The caller fills the buffer: this is
    /// for text whose only copy is in memory, like a session handoff's
    /// unsaved edits to a file that can no longer be opened.
    pub fn open_unreadable_tab(&mut self, path: Option<PathBuf>) -> &mut Editor {
        if !self.is_blank_initial() {
            let mut e = Editor::new();
            e.focused = self.editors[self.active].focused;
            let pos = self.active + 1;
            self.editors.insert(pos, e);
            self.editors[self.active].focused = false;
            self.active = pos;
        }
        let e = &mut self.editors[self.active];
        e.path = path;
        e.preview = false;
        e
    }

    /// Test-only / disk-less helper: insert a tab whose path is set but
    /// whose contents are empty. Production code should call
    /// `open_in_new_tab` so the file is actually loaded from disk.
    pub fn add_tab_with_path(&mut self, path: PathBuf) {
        let mut e = Editor::new();
        e.path = Some(path);
        e.focused = self.editors[self.active].focused;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Close the currently active tab. Refuses (returns false) when only one
    /// tab remains — closing the last would leave the editor pane empty.
    pub fn close_active(&mut self) -> bool {
        self.close_tab(self.active)
    }

    /// Close every tab showing `path` or a file under it (a deleted file or
    /// folder), except tabs with unsaved edits: those stay open, dirty, so
    /// the edits can still be saved (recreating the file) or discarded. A
    /// sheet cell still being typed into counts as an unsaved edit.
    /// Returns how many dirty tabs were kept.
    pub fn close_clean_tabs_under(&mut self, path: &Path) -> usize {
        let mut kept = 0;
        let mut i = self.editors.len();
        while i > 0 {
            i -= 1;
            let e = &self.editors[i];
            if !e.path.as_deref().is_some_and(|p| p.starts_with(path)) {
                continue;
            }
            if e.dirty || e.sheet_pending_cell().is_some() {
                kept += 1;
            } else {
                self.close_tab(i);
            }
        }
        kept
    }

    /// Close the tab at `idx`. When more than one tab is open the tab is
    /// removed and `self.active` is shifted so it still points at a valid
    /// tab. When this is the last remaining tab it is reset to the blank
    /// just-launched state instead of being removed (so the editor pane
    /// always has at least one buffer to render). Returns false only on an
    /// out-of-range index.
    pub fn close_tab(&mut self, idx: usize) -> bool {
        if idx >= self.editors.len() {
            return false;
        }
        if self.editors.len() == 1 {
            let was_focused = self.editors[0].focused;
            let mut fresh = Editor::new();
            fresh.focused = was_focused;
            let closed = std::mem::replace(&mut self.editors[0], fresh);
            self.release_bookmarks(&closed);
            self.active = 0;
            return true;
        }
        let closed = self.editors.remove(idx);
        self.release_bookmarks(&closed);
        if self.active > idx {
            self.active -= 1;
        } else if self.active >= self.editors.len() {
            self.active = self.editors.len() - 1;
        }
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        true
    }

    /// Keep a closing tab's bookmarks for the App, which holds a file's
    /// marks once no tab does.
    fn release_bookmarks(&mut self, closed: &Editor) {
        if !closed.bookmarks.is_empty() {
            self.released_bookmarks.push(closed.bookmarks_on_disk());
        }
    }

    /// The bookmarks of the tabs closed since the last call, oldest first.
    pub fn take_released_bookmarks(
        &mut self,
    ) -> Vec<std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>> {
        std::mem::take(&mut self.released_bookmarks)
    }

    /// Close every tab whose index ≠ `keep_idx`, except pinned tabs, which
    /// always survive (VS Code "Close Others" never closes a pinned tab), and
    /// tabs with unsaved changes, which a sweep never drops (#862). The
    /// kept tab stays active. Returns how many tabs were actually removed (0
    /// when `keep_idx` is out of range or nothing else is closeable). Mirrors
    /// VS Code's "Close Others" context-menu action.
    pub fn close_others(&mut self, keep_idx: usize) -> usize {
        if keep_idx >= self.editors.len() || self.editors.len() <= 1 {
            return 0;
        }
        let before = self.editors.len();
        let mut new_active = 0;
        let mut kept: Vec<Editor> = Vec::with_capacity(before);
        for (i, ed) in std::mem::take(&mut self.editors).into_iter().enumerate() {
            if i == keep_idx {
                new_active = kept.len();
                kept.push(ed);
            } else if ed.pinned || ed.dirty {
                kept.push(ed);
            } else {
                self.release_bookmarks(&ed);
            }
        }
        let removed = before - kept.len();
        self.editors = kept;
        self.active = new_active;
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        removed
    }

    /// Close every tab whose index > `from_idx`, except pinned tabs, which
    /// always survive (VS Code "Close to the Right" never closes a pinned
    /// tab), and tabs with unsaved changes (#862). The tab at `from_idx`
    /// stays active; tabs to the left are untouched. Returns the number of
    /// tabs removed. Matches VS Code's "Close to the Right".
    pub fn close_to_right(&mut self, from_idx: usize) -> usize {
        if from_idx >= self.editors.len() {
            return 0;
        }
        let before = self.editors.len();
        let old_active = self.active;
        let mut new_active: Option<usize> = None;
        let mut pivot_pos = 0;
        let mut kept: Vec<Editor> = Vec::with_capacity(before);
        for (i, ed) in std::mem::take(&mut self.editors).into_iter().enumerate() {
            if i <= from_idx || ed.pinned || ed.dirty {
                if i == from_idx {
                    pivot_pos = kept.len();
                }
                if i == old_active {
                    new_active = Some(kept.len());
                }
                kept.push(ed);
            } else {
                self.release_bookmarks(&ed);
            }
        }
        let removed = before - kept.len();
        self.editors = kept;
        // Keep the previously-active tab active when it survived; otherwise
        // fall back to the pivot tab's new position.
        self.active = new_active.unwrap_or(pivot_pos);
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        removed
    }

    /// Close every tab, resetting the editor pane to the single blank
    /// just-launched state — mirrors `close_tab` on the last remaining
    /// tab. Returns how many tabs were collapsed away (always ≥ 1 when
    /// the editor had any content). Matches VS Code's "Close All": pinned
    /// tabs stay open (#941). Tabs with unsaved changes stay open too
    /// (#862): VS Code asks about each, croft leaves them for a Cmd+W that
    /// asks. That makes it the same sweep as [`Self::close_saved`].
    pub fn close_all(&mut self) -> usize {
        self.close_saved()
    }

    /// Close every saved (non-dirty) tab, keeping any with unsaved changes
    /// and every pinned tab (#941). When nothing is kept the pane resets to
    /// a single blank buffer, mirroring `close_all`; otherwise the kept tabs
    /// stay in order, the active one staying active when it is among them
    /// and the first becoming active when not. Returns how many tabs were
    /// removed (0 when every open tab is dirty or pinned). Matches VS
    /// Code's "Close Saved" (`workbench.action.closeUnmodifiedEditors`).
    pub fn close_saved(&mut self) -> usize {
        let before = self.editors.len();
        let old_active = self.active;
        let was_focused = self.editors[old_active].focused;
        let mut new_active = 0;
        let mut kept: Vec<Editor> = Vec::with_capacity(before);
        for (i, ed) in std::mem::take(&mut self.editors).into_iter().enumerate() {
            if ed.dirty || ed.pinned {
                if i == old_active {
                    new_active = kept.len();
                }
                kept.push(ed);
            } else {
                self.release_bookmarks(&ed);
            }
        }
        let removed = before - kept.len();
        if kept.is_empty() {
            let mut fresh = Editor::new();
            fresh.focused = was_focused;
            kept.push(fresh);
        }
        self.editors = kept;
        self.active = new_active;
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        removed
    }

    /// Map a mouse cell `(col, row)` to a tab index, or `None` if the click
    /// missed every tab. Uses the on-screen ranges captured during the most
    /// recent render.
    pub fn tab_at(&self, col: u16, row: u16) -> Option<usize> {
        if row != self.tab_strip_y {
            return None;
        }
        for (i, &(x, w)) in self.tab_screen_ranges.iter().enumerate() {
            if col >= x && col < x.saturating_add(w) {
                return Some(i);
            }
        }
        None
    }

    /// Whether `(col, row)` is on the tab strip row of this group, tabs or the
    /// gap after them.
    pub fn tab_strip_contains(&self, col: u16, row: u16) -> bool {
        let r = self.last_full_area;
        row == self.tab_strip_y && col >= r.x && col < r.x + r.width
    }

    /// Reserve `cells` at the strip's right end for what the app paints there.
    pub fn set_tab_reserved_right(&mut self, cells: u16) {
        self.tab_strip.reserved_right = cells;
    }

    /// Whether `(col, row)` is on the "all open tabs" button.
    pub fn tab_list_button_at(&self, col: u16, row: u16) -> bool {
        let r = self.tab_strip.list_button;
        r.width > 0 && row == r.y && col >= r.x && col < r.x + r.width
    }

    /// A wheel notch over the strip: [`Self::scroll_tabs`], but at most one tab
    /// per [`TAB_WHEEL_GAP`] in a direction. A touchpad sends dozens of events
    /// per swipe, which would otherwise fling the strip to its end.
    pub fn scroll_tabs_wheel(&mut self, delta: isize) {
        let now = std::time::Instant::now();
        if let Some((at, dir)) = self.tab_strip.wheel_at
            && dir == delta
            && now.duration_since(at) < TAB_WHEEL_GAP
        {
            return;
        }
        self.tab_strip.wheel_at = Some((now, delta));
        self.scroll_tabs(delta);
    }

    /// Scroll the tab strip by `delta` tabs (the wheel over it). The render
    /// clamps the result to what still fills the strip.
    pub fn scroll_tabs(&mut self, delta: isize) {
        let last = self.editors.len().saturating_sub(1);
        self.tab_strip.scroll = self.tab_strip.scroll.saturating_add_signed(delta).min(last);
        self.tab_strip.revealed = Some((self.active, self.editors.len()));
    }

    /// Full filesystem path for a tab, used as the hover tooltip that
    /// disambiguates same-named files (the tab label only shows the bare
    /// `file_name()`). Returns `None` for an unsaved "untitled" buffer
    /// since there is nothing to disambiguate. Diff tabs report both
    /// sides so the viewer can tell which revisions are being compared.
    pub fn tab_full_path(&self, idx: usize) -> Option<String> {
        let e = self.editors.get(idx)?;
        if let Some(diff) = e.diff.as_ref() {
            let l = diff.left_path.to_string_lossy().into_owned();
            let r_is_real = diff.right_path != Path::new("/dev/null")
                && !diff.right_path.as_os_str().is_empty();
            if r_is_real {
                let r = diff.right_path.to_string_lossy().into_owned();
                return Some(format!("{l} \u{2194} {r}"));
            }
            return Some(l);
        }
        e.path.as_ref().map(|p| p.to_string_lossy().into_owned())
    }

    pub fn tab_screen_x(&self, idx: usize) -> Option<(u16, u16)> {
        self.tab_screen_ranges.get(idx).copied()
    }

    pub fn close_screen_x(&self, idx: usize) -> Option<u16> {
        self.tab_close_x.get(idx).copied().filter(|&x| x != 0)
    }

    /// Map a mouse cell to the tab whose close `\u{2715}` glyph occupies it,
    /// or `None` if the click missed every close button. Used by the App's
    /// mouse handler to short-circuit ahead of `tab_at` so a click on the X
    /// closes the tab instead of selecting it.
    pub fn close_at(&self, col: u16, row: u16) -> Option<usize> {
        if row != self.tab_strip_y {
            return None;
        }
        self.tab_close_x.iter().position(|&x| x != 0 && x == col)
    }

    /// Index of the first tab whose `path` matches `target` either by
    /// literal equality or by canonicalised equality (so symlink + relative
    /// path aliases dedupe to the same tab). Returns `None` if no tab is
    /// currently holding that file.
    ///
    /// A symbol tab (#369) is not "the file's tab": opening, previewing or
    /// selecting a file must land on a tab showing all of it, so this skips
    /// them. [`Self::find_any_tab_with_path`] includes them.
    pub fn find_tab_with_path(&self, target: &Path) -> Option<usize> {
        self.find_tab_matching(target, |e| e.symbol_view.is_none())
    }

    /// Like [`Self::find_tab_with_path`], symbol tabs included: for closing
    /// every tab of a file.
    pub fn find_any_tab_with_path(&self, target: &Path) -> Option<usize> {
        self.find_tab_matching(target, |_| true)
    }

    /// The tab whose buffer holds `target`'s text: its own tab first, else a
    /// symbol tab over it. A diff tab carries the file's `path` but shows a
    /// comparison, so an edit sent to it changes nothing (#1113); it never
    /// counts.
    pub fn find_buffer_tab(&self, target: &Path) -> Option<usize> {
        self.find_tab_matching(target, |e| e.diff.is_none() && e.symbol_view.is_none())
            .or_else(|| self.find_tab_matching(target, |e| e.diff.is_none()))
    }

    /// If `path` is open in a tab, apply LSP rename `edits` to that buffer
    /// in-memory (one undo step, marked dirty) and return the count applied.
    /// `None` when no tab holds the file, so the caller can edit it on disk.
    pub fn apply_rename_to_open_tab(
        &mut self,
        path: &Path,
        edits: &[TextSpanEdit],
    ) -> Option<usize> {
        let idx = self.find_buffer_tab(path)?;
        Some(self.editors[idx].apply_span_edits(edits))
    }

    /// Every open tab's file and `edit_seq`: the buffers as they stand when
    /// a language server is asked for a workspace edit (#1151).
    pub fn edit_seqs(&self) -> Vec<(PathBuf, u64)> {
        self.editors
            .iter()
            .filter_map(|e| Some((e.path.clone()?, e.edit_seq)))
            .collect()
    }

    /// True when the tab a workspace edit to `path` would land in (as
    /// [`Self::apply_rename_to_open_tab`] picks it) was edited after `seqs`
    /// was taken: the edit's positions describe text no longer there. A
    /// file no tab held when `seqs` was taken is never called edited.
    pub fn edited_since(&self, seqs: &[(PathBuf, u64)], path: &Path) -> bool {
        let Some(idx) = self.find_buffer_tab(path) else {
            return false;
        };
        let e = &self.editors[idx];
        let Some(p) = e.path.as_ref() else {
            return false;
        };
        let mut then = seqs.iter().filter(|(q, _)| q == p).peekable();
        then.peek().is_some() && !then.any(|(_, seq)| *seq == e.edit_seq)
    }

    fn find_tab_matching(&self, target: &Path, extra: impl Fn(&Editor) -> bool) -> Option<usize> {
        let canon_target = target.canonicalize().ok();
        self.editors.iter().position(|e| {
            if !extra(e) {
                return false;
            }
            let Some(p) = e.path.as_ref() else {
                return false;
            };
            if p == target {
                return true;
            }
            match (canon_target.as_ref(), p.canonicalize().ok()) {
                (Some(a), Some(b)) => *a == b,
                _ => false,
            }
        })
    }

    pub fn preview_index(&self) -> Option<usize> {
        self.editors.iter().position(|e| e.preview)
    }

    /// Mark the active tab as pinned (no longer the preview slot). Called
    /// when the user double-clicks a file, hits Ctrl+Enter, or starts typing
    /// inside a preview tab.
    pub fn pin_active(&mut self) {
        self.editors[self.active].preview = false;
    }

    /// True when the tab at `idx` is the replaceable preview slot. Drives the
    /// tab menu's "Keep Open" entry, which only makes sense for a preview tab.
    pub fn is_preview(&self, idx: usize) -> bool {
        self.editors.get(idx).is_some_and(|e| e.preview)
    }

    /// VS Code "Keep Open": promote the tab at `idx` out of the preview slot
    /// so a subsequent single-click open no longer replaces it. Returns true
    /// when a preview tab was actually promoted (false if it was already
    /// permanent or the index is out of range).
    pub fn keep_open(&mut self, idx: usize) -> bool {
        match self.editors.get_mut(idx) {
            Some(e) if e.preview => {
                e.preview = false;
                true
            }
            _ => false,
        }
    }

    /// True when the tab at `idx` is pinned. Drives the tab menu's
    /// "Pin"/"Unpin" label and the close-cell glyph (pin vs `\u{2715}`).
    pub fn is_pinned(&self, idx: usize) -> bool {
        self.editors.get(idx).is_some_and(|e| e.pinned)
    }

    /// VS Code "Pin"/"Unpin" the tab at `idx`. Returns the tab's new pinned
    /// state. Pinning clears the preview flag (a pinned tab is never the
    /// replaceable preview slot) and reorders the strip so pinned tabs stay
    /// leftmost: a stable partition lands a newly-pinned tab at the end of the
    /// pinned block and a newly-unpinned tab at the front of the unpinned
    /// block, matching VS Code. The active tab follows its editor across the
    /// reorder.
    pub fn toggle_pin(&mut self, idx: usize) -> bool {
        if idx >= self.editors.len() {
            return false;
        }
        let now_pinned = !self.editors[idx].pinned;
        self.editors[idx].pinned = now_pinned;
        if now_pinned {
            self.editors[idx].preview = false;
        }
        // Stable partition: pinned first (keeping their relative order), then
        // unpinned (keeping theirs). `sort_by_key` is stable, so the two blocks
        // preserve order and only the toggled tab crosses the boundary.
        let n = self.editors.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| !self.editors[i].pinned);
        let new_active = order.iter().position(|&i| i == self.active).unwrap_or(0);
        let mut slots: Vec<Option<Editor>> = self.editors.drain(..).map(Some).collect();
        self.editors = order.iter().map(|&i| slots[i].take().unwrap()).collect();
        self.active = new_active;
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        now_pinned
    }

    /// Move the tab at `from` to position `to` in this group (#1565), as VS
    /// Code's tab drag and Move Editor Left / Right do. Pinned tabs stay ahead
    /// of the rest, so `to` is clamped into the moved tab's own block. The
    /// active tab stays active. Returns the tab's new index, or `None` when
    /// `from` is out of range.
    pub fn move_tab(&mut self, from: usize, to: usize) -> Option<usize> {
        if from >= self.editors.len() {
            return None;
        }
        let pinned = self.editors.iter().filter(|e| e.pinned).count();
        let to = if self.editors[from].pinned {
            to.min(pinned - 1)
        } else {
            to.clamp(pinned, self.editors.len() - 1)
        };
        if to == from {
            return Some(from);
        }
        let active = self.active;
        let ed = self.editors.remove(from);
        self.editors.insert(to, ed);
        self.active = if active == from {
            to
        } else if from < active && active <= to {
            active - 1
        } else if to <= active && active < from {
            active + 1
        } else {
            active
        };
        Some(to)
    }

    /// View: Move Editor Left / Right (#1565): shift the active tab one place
    /// within its group. False when it is already at that end (of its pinned
    /// or unpinned block), so nothing moved.
    pub fn move_active_tab(&mut self, right: bool) -> bool {
        let from = self.active;
        let to = if right {
            from + 1
        } else {
            match from.checked_sub(1) {
                Some(to) => to,
                None => return false,
            }
        };
        self.move_tab(from, to).is_some_and(|at| at != from)
    }

    /// Remove and return the active editor (for a Move to another group). When
    /// it was the only tab the group falls back to a single blank tab (like
    /// closing the last tab), so the group stays renderable; the caller can
    /// detect that empty state via [`Self::is_blank_initial`] and prune it.
    pub fn take_active_editor(&mut self) -> Editor {
        let ed = self.editors.remove(self.active);
        if self.editors.is_empty() {
            self.editors.push(Editor::new());
            self.active = 0;
        } else if self.active >= self.editors.len() {
            self.active = self.editors.len() - 1;
        }
        for (i, e) in self.editors.iter_mut().enumerate() {
            e.focused = i == self.active;
        }
        ed
    }

    /// Add `ed` (moved from another group) and make it active. A blank-initial
    /// group is replaced in place so a Move never leaves a stray blank tab
    /// beside the moved editor. A pinned editor lands at the end of the
    /// pinned block, so pinned tabs stay ahead of the rest.
    pub fn push_editor(&mut self, mut ed: Editor) {
        if self.is_blank_initial() {
            ed.focused = true;
            self.editors[0] = ed;
            self.active = 0;
            return;
        }
        let at = if ed.pinned {
            self.editors.iter().take_while(|e| e.pinned).count()
        } else {
            self.editors.len()
        };
        self.editors.insert(at, ed);
        self.active = at;
        for (i, e) in self.editors.iter_mut().enumerate() {
            e.focused = i == self.active;
        }
    }

    /// VS Code "preview tab" semantics: open `path` in the single
    /// replaceable preview slot. If the file is already in some tab, just
    /// switch to it. Otherwise reuse the existing preview slot, or create a
    /// fresh preview tab next to the active one when none exists.
    pub fn open_preview(&mut self, path: &Path) -> Result<()> {
        // A diff of the file is not the file (#1113): opening it lands on
        // its own tab, or a new one beside the diff.
        if let Some(idx) =
            self.find_tab_matching(path, |e| e.diff.is_none() && e.symbol_view.is_none())
        {
            // Switching to an already-open file: if a stale preview tab
            // exists for some OTHER file, drop it so the user doesn't
            // accumulate ghost tabs from quick "peek" navigations.
            if let Some(prev) = self.preview_index()
                && prev != idx
            {
                let dropped = self.editors.remove(prev);
                self.release_bookmarks(&dropped);
                let new_idx = if idx > prev { idx - 1 } else { idx };
                // Bypass `select` here because the removal may have left
                // `self.active` pointing past the end (when the active
                // tab was the preview we just dropped).
                self.active = new_idx;
                for (i, ed) in self.editors.iter_mut().enumerate() {
                    ed.focused = i == new_idx;
                }
                return Ok(());
            }
            self.select(idx);
            return Ok(());
        }
        if let Some(idx) = self.preview_index() {
            self.select(idx);
            self.editors[idx].open(path)?;
            self.editors[idx].preview = true;
            return Ok(());
        }
        if self.is_blank_initial() {
            let active = self.active;
            self.editors[active].open(path)?;
            self.editors[active].preview = true;
            self.editors[active].search_highlight = self.search_highlight_term.clone();
            self.editors[active].search_highlight_opts = self.search_highlight_opts;
            return Ok(());
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.open(path)?;
        e.preview = true;
        e.search_highlight = self.search_highlight_term.clone();
        e.search_highlight_opts = self.search_highlight_opts;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// The text a diff should show for `path` (#1572): an open tab's unsaved
    /// edits, as a save would write them, since that is the version the user
    /// is looking at; the file on disk when no tab has unsaved edits to it.
    fn text_to_diff(unsaved: Option<String>, path: &Path) -> Result<String> {
        match unsaved {
            Some(text) => Ok(text),
            None => {
                std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
            }
        }
    }

    /// The text a save of `path`'s open tab would write, when that tab has
    /// unsaved edits; `None` otherwise. Searches this group only: the app
    /// resolves it across every split group before calling the `_resolved`
    /// openers.
    pub fn unsaved_text_of(&self, path: &Path) -> Option<String> {
        let idx = self.find_tab_matching(path, |e| {
            e.dirty && e.symbol_view.is_none() && !e.has_non_text_view()
        })?;
        Some(self.editors[idx].content_for_disk())
    }

    /// Open a side-by-side diff of two files in a fresh pinned tab next
    /// to the active one, each side read the way [`Self::text_to_diff`]
    /// says. The new tab is read-only: edits, save, and the
    /// text-rendering path are all bypassed via the `diff: Some(...)` flag
    /// on the underlying Editor.
    #[cfg(test)]
    pub fn open_diff(&mut self, left: &Path, right: &Path) -> Result<()> {
        let left_unsaved = self.unsaved_text_of(left);
        let right_unsaved = self.unsaved_text_of(right);
        self.open_diff_resolved(left, right, left_unsaved, right_unsaved)
    }

    /// [`Self::open_diff`] with each side's unsaved text already resolved by
    /// the caller (across every split group); `None` reads the disk.
    pub fn open_diff_resolved(
        &mut self,
        left: &Path,
        right: &Path,
        left_unsaved: Option<String>,
        right_unsaved: Option<String>,
    ) -> Result<()> {
        let left_text = Self::text_to_diff(left_unsaved, left)?;
        let right_text = Self::text_to_diff(right_unsaved, right)?;
        let left_lines: Vec<String> = left_text.lines().map(str::to_string).collect();
        let right_lines: Vec<String> = right_text.lines().map(str::to_string).collect();
        let mut data = crate::widgets::diff::DiffData::build(
            left.to_path_buf(),
            right.to_path_buf(),
            left_lines,
            right_lines,
        );
        data.left_is_real_file = true;
        data.source = crate::widgets::diff::DiffSource::TwoFiles;
        data.stamp_sides();
        data.set_whitespace_mode(self.diff_ws_default);

        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        // The path is set so close-by-path lookups work; `diff` being Some
        // is what diverts the renderer onto the side-by-side path.
        e.path = Some(left.to_path_buf());
        e.diff = Some(data);
        // Reuse the blank-initial slot if the editor pane was empty;
        // otherwise insert a new tab next to the active one.
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Open a side-by-side diff between an in-memory `left_text` (e.g.
    /// the HEAD version of a file) and `right` as the user sees it: its open
    /// tab's unsaved edits, else the file on disk. The left "path" is purely
    /// a label; nothing reads from it on disk.
    /// Used by the Source Control panel to show working-tree-vs-HEAD
    /// diffs when the user clicks a Modified entry.
    #[cfg(test)]
    pub fn open_head_diff_with_text(
        &mut self,
        left_label: PathBuf,
        left_text: &str,
        right: &Path,
        left_is_git_head: bool,
    ) -> Result<()> {
        let unsaved = self.unsaved_text_of(right);
        self.open_head_diff_with_text_resolved(
            left_label,
            left_text,
            right,
            left_is_git_head,
            unsaved,
        )
    }

    /// [`Self::open_head_diff_with_text`] with `right`'s unsaved text already
    /// resolved by the caller (across every split group); `None` reads the
    /// disk.
    pub fn open_head_diff_with_text_resolved(
        &mut self,
        left_label: PathBuf,
        left_text: &str,
        right: &Path,
        left_is_git_head: bool,
        unsaved: Option<String>,
    ) -> Result<()> {
        let right_is_unsaved = unsaved.is_some();
        let right_text = match unsaved {
            Some(text) => text,
            None => std::fs::read_to_string(right)
                .with_context(|| format!("reading {}", right.display()))?,
        };
        self.open_head_diff_with_texts(
            left_label,
            left_text,
            &right_text,
            right,
            left_is_git_head,
            right_is_unsaved,
        )
    }

    /// [`Self::open_head_diff_with_text`] with the working side already read
    /// and decoded, for a file that isn't UTF-8 (#1242). `right_is_unsaved`
    /// says `right_text` is the open tab's unsaved buffer, not the disk.
    pub fn open_head_diff_with_texts(
        &mut self,
        left_label: PathBuf,
        left_text: &str,
        right_text: &str,
        right: &Path,
        left_is_git_head: bool,
        right_is_unsaved: bool,
    ) -> Result<()> {
        let left_lines: Vec<String> = left_text.lines().map(str::to_string).collect();
        let right_lines: Vec<String> = right_text.lines().map(str::to_string).collect();
        let mut data = crate::widgets::diff::DiffData::build_with_byte_check(
            left_label,
            right.to_path_buf(),
            left_lines,
            right_lines,
            Some(left_text),
            Some(right_text),
        );
        data.set_whitespace_mode(self.diff_ws_default);
        // Park the viewport on the first change hunk so the user lands on
        // the first edit instead of reading through unchanged leading
        // lines. Identical files stay at scroll 0.
        if let Some(row) = data.first_change_row() {
            data.scroll_to_row(row);
        }
        data.left_is_git_head = left_is_git_head;
        data.right_is_unsaved = right_is_unsaved;
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(right.to_path_buf());
        e.diff = Some(data);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        // If a tab is already showing this path (as a plain file or a
        // diff), reuse it so we don't pile up duplicate tabs as the
        // user clicks through the change list — unless it holds unsaved
        // edits, which replacing wholesale would silently destroy. A diff
        // tab of the path comes first: with the file's own tab dirty, it
        // is the only one to reuse (#1572).
        if let Some(idx) = self
            .find_tab_matching(right, |e| e.diff.is_some())
            .or_else(|| self.find_tab_with_path(right))
            && !self.editors[idx].dirty
        {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Open a single-pane unified diff that paints every line of
    /// `head_text` as a removed line. `path` is the workspace-relative
    /// path that no longer exists on disk; it doubles as the display
    /// label in the diff header and as the tab dedup key so repeated
    /// clicks on the same Source-Control row reuse the tab rather than
    /// stacking new ones.
    pub fn open_deleted_diff_with_text(&mut self, path: &Path, head_text: &str) -> Result<()> {
        let mut data =
            crate::widgets::diff::DiffData::build_unified_deletion(path.to_path_buf(), head_text);
        data.set_whitespace_mode(self.diff_ws_default);
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(path.to_path_buf());
        e.diff = Some(data);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        // Same dirty guard as `open_head_diff_with_text`: a deleted-on-disk
        // file can still hold unsaved edits in its tab.
        if let Some(idx) = self.find_tab_with_path(path)
            && !self.editors[idx].dirty
        {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Open a side-by-side view of raw `git diff` text (e.g. the stdout
    /// of `git diff --staged` or `git diff <branch>`). `label` is a
    /// synthetic path used as the tab title and as the dedup key so a
    /// repeat invocation reuses the existing tab instead of stacking new
    /// ones. The text is parsed into separate left/right streams so the
    /// standard two-column renderer takes over — every `+`/`-` pair in a
    /// hunk lines up horizontally instead of zigzagging vertically.
    pub fn open_git_diff_side_by_side(&mut self, label: &Path, raw_diff: &str) -> Result<()> {
        let mut data = crate::widgets::diff::DiffData::build_side_by_side_from_git_text(
            label.to_path_buf(),
            raw_diff,
        );
        data.set_whitespace_mode(self.diff_ws_default);
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(label.to_path_buf());
        e.diff = Some(data);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        if let Some(idx) = self.find_tab_with_path(label) {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Show a fleet run's results in the `Fleet` tab (#363), replacing the
    /// previous run's, so one tab always holds the latest comparison.
    pub fn open_fleet(&mut self, view: crate::widgets::fleet::FleetView) {
        let label = PathBuf::from("Fleet");
        if let Some(idx) = self.find_tab_with_path(&label) {
            self.editors[idx].fleet = Some(view);
            self.select(idx);
            return;
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(label);
        e.fleet = Some(view);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return;
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Open a pull request for review in a fresh tab labelled `PR #n`
    /// (#365), or re-select the tab already showing it.
    pub fn open_pr_review(&mut self, view: crate::widgets::pr_review::PrReviewView) {
        let label = PathBuf::from(format!("PR #{}", view.pr.number));
        if let Some(idx) = self.find_tab_with_path(&label) {
            self.editors[idx].pr_review = Some(view);
            self.select(idx);
            return;
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(label);
        e.pr_review = Some(view);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return;
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Open arbitrary text in a scratch tab labelled `label` (no file on
    /// disk). Used by "Show Git Output" to surface the git command log. The
    /// tab has no `disk_stamp`, so the FS-sync layer never tries to reload
    /// or overwrite it, and is `scratch`, so no save writes it (#951).
    pub fn open_text_buffer(&mut self, label: &Path, text: &str) -> Result<()> {
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(label.to_path_buf());
        e.scratch = true;
        e.lines = split_into_lines(text);
        if e.lines.is_empty() {
            e.lines.push(String::new());
        }
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        if let Some(idx) = self.find_tab_with_path(label) {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Pinned-tab open: if the file is already in some tab, pin and switch
    /// to it. Otherwise create a fresh pinned tab next to the active one.
    /// Used by double-click in the explorer and Ctrl+Enter on a tree row.
    pub fn open_pinned(&mut self, path: &Path) -> Result<()> {
        self.open_pinned_with(path, Editor::open)
    }

    /// [`Self::open_pinned`] for a file that may not exist yet: a missing
    /// one opens as an empty buffer that its first save creates (#1157).
    pub fn open_new_pinned(&mut self, path: &Path) -> Result<()> {
        self.open_pinned_with(path, Editor::open_new_file)
    }

    fn open_pinned_with(
        &mut self,
        path: &Path,
        open: fn(&mut Editor, &Path) -> Result<()>,
    ) -> Result<()> {
        // A diff tab can carry the working file's `path` yet is not an
        // editable view of it, so skip diffs here: Enter on a diff (or a
        // double-click) opens the real file beside the diff rather than just
        // re-selecting the diff tab.
        if let Some(idx) =
            self.find_tab_matching(path, |e| e.diff.is_none() && e.symbol_view.is_none())
        {
            self.editors[idx].preview = false;
            self.select(idx);
            return Ok(());
        }
        if self.is_blank_initial() {
            let active = self.active;
            open(&mut self.editors[active], path)?;
            self.editors[active].preview = false;
            self.editors[active].search_highlight = self.search_highlight_term.clone();
            self.editors[active].search_highlight_opts = self.search_highlight_opts;
            return Ok(());
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        open(&mut e, path)?;
        e.preview = false;
        e.search_highlight = self.search_highlight_term.clone();
        e.search_highlight_opts = self.search_highlight_opts;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// True iff the editor is in its just-launched state: a single tab with
    /// no file loaded and no edits. `App::render` uses this to swap the
    /// editor pane for the welcome screen, and `open_preview` /
    /// `open_pinned` use it to reuse the placeholder tab rather than stack
    /// a new one on top.
    pub fn is_blank_initial(&self) -> bool {
        self.editors.len() == 1
            && self.editors[0].path.is_none()
            && self.editors[0].untitled.is_none()
            && !self.editors[0].dirty
            && self.editors[0].lines.iter().all(|l| l.is_empty())
    }

    /// File: New Untitled Text File (#1580): a pinned, empty tab with no
    /// path next to the active one, shown as `Untitled-n`. The caller picks
    /// `n` across every split group, which this group cannot see. The blank
    /// startup tab becomes it rather than staying behind.
    pub fn new_untitled(&mut self, n: usize) {
        if self.is_blank_initial() {
            let active = self.active;
            self.editors[active].untitled = Some(n);
            self.editors[active].preview = false;
            return;
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.untitled = Some(n);
        e.search_highlight = self.search_highlight_term.clone();
        e.search_highlight_opts = self.search_highlight_opts;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Test-only helper that mirrors `open_preview` without going through
    /// the disk: insert a preview tab whose path is set but whose buffer is
    /// empty. Used by unit tests to exercise preview-slot bookkeeping in
    /// isolation from filesystem I/O.
    pub fn add_preview_tab_with_path(&mut self, path: PathBuf) {
        let mut e = Editor::new();
        e.path = Some(path);
        e.preview = true;
        e.focused = self.editors[self.active].focused;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Test-only helper for the "single-click on a different file when a
    /// preview tab already exists" path: rewrite the existing preview tab's
    /// path without changing tab count.
    pub fn repoint_preview_to(&mut self, path: PathBuf) {
        if let Some(idx) = self.preview_index() {
            self.editors[idx].path = Some(path);
        }
    }
}

impl Default for EditorTabs {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for EditorTabs {
    type Target = Editor;
    fn deref(&self) -> &Editor {
        &self.editors[self.active]
    }
}

impl DerefMut for EditorTabs {
    fn deref_mut(&mut self) -> &mut Editor {
        &mut self.editors[self.active]
    }
}

// Tab-strip chrome (strip, tab bodies, hover lift, close pill) is per-theme
// data: `Theme::tab_*` carries the two built-ins' historical constants and
// derives colors for every manifest theme, so the strip follows the selected
// theme instead of staying VS-Code navy. Only the label foregrounds stay
// fixed: this muted grey and white are legible on all dark themes.
/// The tab strip's scroll position and button, see `EditorTabs::tab_strip`.
#[derive(Default)]
struct TabStripState {
    /// Index of the first tab drawn in the strip: it scrolls by tab when
    /// there are more of them than fit (wheel over the strip, or the active
    /// tab changing).
    scroll: usize,
    /// The `(active, tab count)` the strip last scrolled itself to reveal, so
    /// a wheel scroll away from the active tab is not undone on the next
    /// frame.
    revealed: Option<(usize, usize)>,
    /// Cells at the strip's right end the app paints over (the Customize
    /// Layout icons); tabs stop short of them. Set before each render.
    reserved_right: u16,
    /// The "all open tabs" button at the right end of the tab area, drawn
    /// only when the tabs do not all fit (otherwise empty).
    list_button: Rect,
    /// When and which way the wheel last moved the strip, so a touchpad's
    /// flood of events moves it a tab at a time rather than all at once.
    wheel_at: Option<(std::time::Instant, isize)>,
}

/// Cells the "all open tabs" button takes at the right end of a crowded strip.
const TAB_LIST_W: u16 = 3;
/// The least time between two tabs of wheel scrolling in one direction.
const TAB_WHEEL_GAP: std::time::Duration = std::time::Duration::from_millis(90);
const TAB_INACTIVE_FG: Color = Color::Rgb(0x9d, 0xa5, 0xb4);
const TAB_ACTIVE_FG: Color = Color::White;

/// Ink for the tab close/pin glyph on its hover pill: white, except that a
/// high contrast theme (#1548) takes whichever of black or white reads
/// better on its pill (black on High Contrast's orange).
fn close_pill_fg(theme: crate::theme::Theme, pill: Color) -> Color {
    match pill {
        Color::Rgb(r, g, b)
            if theme.high_contrast()
                && crate::theme::contrast_ratio((r, g, b), (0, 0, 0))
                    > crate::theme::contrast_ratio((r, g, b), (0xff, 0xff, 0xff)) =>
        {
            Color::Black
        }
        _ => Color::White,
    }
}

/// The file-type icon a tab leads with on a `surfaces` theme: the
/// Explorer's icon for its file (a diff tab's left side), the plain file
/// icon for an untitled buffer.
fn tab_icon(ed: &Editor) -> crate::icons::Icon {
    let path = ed
        .path
        .as_deref()
        .or_else(|| ed.diff.as_ref().map(|d| d.left_path.as_path()));
    match path.and_then(|p| p.file_name().map(|n| (p, n))) {
        Some((p, name)) => {
            let suffix = p
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            crate::icons::for_path(&name.to_string_lossy(), &suffix)
        }
        None => crate::icons::DEFAULT_FILE,
    }
}

/// One visible tab of the strip on a `surfaces` theme, for
/// [`paint_surface_tab`]: where it sits and the states its look follows.
struct SurfaceTab<'a> {
    /// The tab's first cell (its leading pad).
    x: u16,
    y: u16,
    label: &'a str,
    /// Where the label starts, after the pad, the icon and its gap.
    label_x: u16,
    /// The close / pin / dirty-dot cell, the tab's close hit target.
    close_x: u16,
    ed: &'a Editor,
    theme: crate::theme::Theme,
    t: crate::widgets::surface::Tones,
    /// The tab body's fill: canvas when active, the hover lift, or chrome.
    bg: Color,
    is_active: bool,
    /// The tab's group is the focused editor group.
    group_focused: bool,
    /// That group has the keyboard (focused, no modal over it).
    has_keyboard: bool,
    tab_hovered: bool,
    on_close: bool,
}

/// Paint a tab as ` {icon} {label} {close} ` on a `surfaces` theme. Three
/// inks and no bold: an inactive label is muted and lifts to body text
/// under the pointer; the active tab is the region's one strong label in
/// the focused group and body text in another, and only in the group with
/// the keyboard does it wear the accent, as an underline under its label.
/// The close cross is muted on the active or hovered tab and faint on the
/// rest; a dirty file shows a dot in that slot until the pointer is on the
/// tab, so the tab keeps one width whether its file is dirty or not.
fn paint_surface_tab(buf: &mut Buffer, tab: SurfaceTab<'_>) {
    let SurfaceTab {
        x,
        y,
        label,
        label_x,
        close_x,
        ed,
        theme,
        t,
        bg,
        is_active,
        group_focused,
        has_keyboard,
        tab_hovered,
        on_close,
    } = tab;
    let fill = Style::default().bg(bg);
    let label_fg = if is_active {
        if group_focused { t.strong() } else { t.text() }
    } else if tab_hovered {
        t.text()
    } else {
        t.muted()
    };
    // The pads and gaps first, so every cell of the tab wears its fill.
    for cx in [x, x + 2, close_x - 1, close_x + 1] {
        buf.set_string(cx, y, " ", fill);
    }
    // The icon in its own colour on the active or hovered tab, eased most
    // of the way into the tab's fill on the others.
    let icon = tab_icon(ed);
    let icon_fg = match (theme.ui(icon.color), bg) {
        (Color::Rgb(r, g, b), Color::Rgb(br, bgc, bb)) if !(is_active || tab_hovered) => {
            crate::gradient::rgb_color(crate::gradient::lerp_rgb((r, g, b), (br, bgc, bb), 0.45))
        }
        (c, _) => c,
    };
    buf.set_string(
        x + 1,
        y,
        icon.glyph.to_string(),
        Style::default().fg(icon_fg).bg(bg),
    );
    let mut style = Style::default().fg(label_fg).bg(bg);
    if ed.preview {
        style = style.add_modifier(Modifier::ITALIC);
    }
    buf.set_string(label_x, y, label, style);
    if is_active && has_keyboard {
        let width = Span::raw(label).width() as u16;
        for ux in label_x..label_x + width {
            let cell = &mut buf[(ux, y)];
            cell.modifier.insert(Modifier::UNDERLINED);
            cell.underline_color = t.accent();
        }
    }
    // The close slot: a thumb-tack on a pinned tab (clicking it unpins), a
    // dot on a dirty one while the pointer is elsewhere, else the cross.
    let (glyph, ink) = if ed.pinned {
        ("\u{f08d}", t.muted())
    } else if ed.dirty && !tab_hovered {
        ("\u{25cf}", if is_active { t.text() } else { t.muted() })
    } else if is_active || tab_hovered {
        ("\u{2715}", t.muted())
    } else {
        ("\u{2715}", t.faint())
    };
    let close_style = if on_close {
        // The pill names exactly the glyph the click will land on.
        Style::default()
            .fg(t.strong())
            .bg(theme.tab_close_pill_bg())
    } else {
        Style::default().fg(ink).bg(bg)
    };
    buf.set_string(close_x, y, glyph, close_style);
}

impl Widget for &mut EditorTabs {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_full_area = area;
        if area.height == 0 || area.width == 0 {
            // Ctrl+Shift+J maximises the terminal pane, which collapses
            // the editor area to height 0. The inner editor's `render`
            // is never reached on this branch, so its `last_area` would
            // otherwise keep the rectangle from the pre-maximise frame
            // and `App::handle_mouse`'s `in_editor` hit-test
            // (rect_contains(self.editor.last_area, ...)) would still
            // win against the terminal in the dispatch chain at
            // app.rs:7229 / 7336 — clicks meant to begin a terminal
            // selection get routed to the editor's mouse_down and the
            // file caret jumps instead. Zero the inner editor's
            // hit-test rectangle here so the terminal pane wins
            // unambiguously while the editor is collapsed.
            for ed in self.editors.iter_mut() {
                ed.last_area = Rect {
                    x: area.x,
                    y: area.y,
                    width: area.width,
                    height: 0,
                };
                ed.last_inner = ed.last_area;
                ed.last_scrollbar = Rect::default();
            }
            return;
        }
        let strip_h: u16 = 1;
        let strip = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: strip_h,
        };
        let mut body = Rect {
            x: area.x,
            y: area.y + strip_h,
            width: area.width,
            height: area.height - strip_h,
        };

        // The tab chrome follows the theme. Like the brand flag below, the
        // theme propagates from a synced sibling: a tab opened since the last
        // sync still carries the default theme and must not flash the wrong
        // chrome for a frame.
        let theme = self
            .editors
            .iter()
            .map(|e| e.theme)
            .find(|t| *t != crate::theme::Theme::default())
            .unwrap_or_default();
        // Paint strip background first so the gap to the right of the last
        // tab still reads as the tab-strip colour rather than terminal default.
        let strip_bg_style = Style::default().bg(theme.tab_strip_bg());
        for x in strip.x..strip.x + strip.width {
            buf[(x, strip.y)].set_style(strip_bg_style);
            buf[(x, strip.y)].set_symbol(" ");
        }

        self.tab_strip_y = strip.y;
        self.tab_screen_ranges.clear();
        self.tab_close_x.clear();
        let active = self.active;
        // Black theme (`focus_gradient` doubles as the theme flag): the
        // active tab wears the muted teal selection fill instead of the
        // legacy navy chip; Croft Dark keeps the navy. Sync pushes the flag
        // to every editor, but one freshly opened since the last sync still
        // defaults to false — propagate from its siblings so it can't flash
        // the navy chip (or a blue body border) for a frame.
        let brand = self.editors.iter().any(|e| e.focus_gradient);
        if brand {
            for ed in self.editors.iter_mut() {
                ed.focus_gradient = true;
            }
        }
        let active_tab_bg = theme.tab_active_bg();
        let pointer = self.hover_pointer;
        // A `surfaces` theme (Croft Loch) lays a tab out as
        // ` {icon} {label} {close} `: a file-type icon leads the label, the
        // dirty dot moves out of the label into the close slot (so a tab
        // never changes width as its file goes dirty), and only the active
        // tab of the group with the keyboard wears the accent, as an
        // underline. Every other theme keeps ` {label} {close} `.
        let tones = crate::widgets::surface::tones(theme);
        let group_focused = self.editors[active].focused;
        let has_keyboard = group_focused && !self.editors[active].overlay_has_keyboard;
        let icon_w: u16 = if tones.is_some() { 2 } else { 0 };
        let display_labels: Vec<String> = disambiguated_tab_labels(&self.editors)
            .into_iter()
            .zip(&self.editors)
            .map(|(label, ed)| {
                // The menus, status line and screen reader keep the `● `
                // prefix (`tab_display_label`); only the strip drops it, and
                // only where the close slot shows the dot instead: a pinned
                // tab's slot holds the pin, so its label keeps the prefix.
                match label.strip_prefix("\u{25cf} ") {
                    Some(rest) if tones.is_some() && ed.dirty && !ed.pinned => rest.to_string(),
                    _ => label,
                }
            })
            .collect();
        // Tabs never run under what the app paints at the strip's right end,
        // and the strip scrolls by tab when they do not all fit.
        let mut strip_right = strip.x
            + strip
                .width
                .saturating_sub(self.tab_strip.reserved_right.min(strip.width));
        let avail_full = usize::from(strip_right.saturating_sub(strip.x));
        let widths: Vec<usize> = display_labels
            .iter()
            .map(|l| {
                // Display CELLS, not chars (#168 review): a CJK file or
                // directory name is double-width, and a char count shifted
                // the close button and hit ranges left of the painted text.
                // A pad either side, the close cell and a gap, plus the icon
                // and its gap on a `surfaces` theme.
                usize::from(Span::raw(l.as_str()).width() as u16) + 4 + usize::from(icon_w)
            })
            .collect();
        let n = widths.len();
        // Tabs that do not all fit get an "all open tabs" button in the last
        // cells of the tab area, so the hidden ones stay reachable.
        self.tab_strip.list_button = Rect::default();
        if widths.iter().sum::<usize>() > avail_full && avail_full > usize::from(TAB_LIST_W) {
            strip_right -= TAB_LIST_W;
            self.tab_strip.list_button = Rect {
                x: strip_right,
                y: strip.y,
                width: TAB_LIST_W,
                height: 1,
            };
            let hovered = self.hover_pointer.is_some_and(|(px, py)| {
                py == strip.y && px >= strip_right && px < strip_right + TAB_LIST_W
            });
            // As the layout icons beside it: the glyph goes from grey to the
            // accent on hover, the cell keeps the strip's own background. On
            // a `surfaces` theme it is a header action: muted at rest, the
            // strong ink on a faint chip under the pointer.
            let fg = if hovered {
                theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))
            } else {
                theme.ui(Color::Rgb(0xcc, 0xcc, 0xcc))
            };
            let mut style = Style::default().fg(fg).bg(theme.tab_strip_bg());
            if tones.is_some() {
                style = style.patch(crate::widgets::header_pill::action_style(theme, hovered));
            }
            buf.set_string(strip_right, strip.y, " \u{2261} ", style);
        }
        let avail = usize::from(strip_right.saturating_sub(strip.x));
        // Last scroll that still fills the strip: past it the tail would leave
        // blank room on the right while earlier tabs stay hidden.
        let max_scroll = {
            let mut first = n.saturating_sub(1);
            let mut used = widths.get(first).copied().unwrap_or(0);
            while first > 0 && used + widths[first - 1] <= avail {
                first -= 1;
                used += widths[first];
            }
            first
        };
        if self.tab_strip.revealed != Some((active, n)) {
            self.tab_strip.revealed = Some((active, n));
            if active < self.tab_strip.scroll {
                self.tab_strip.scroll = active;
            } else {
                // Scroll right just far enough that the active tab fits whole.
                while self.tab_strip.scroll < active
                    && widths[self.tab_strip.scroll..=active].iter().sum::<usize>() > avail
                {
                    self.tab_strip.scroll += 1;
                }
            }
        }
        self.tab_strip.scroll = self.tab_strip.scroll.min(max_scroll);
        let mut cursor_x = strip.x;
        let mut hidden_rest = false;
        for (i, ed) in self.editors.iter().enumerate() {
            let label_text = display_labels[i].clone();
            let label_chars = Span::raw(label_text.as_str()).width() as u16;
            let width = widths[i] as u16;
            if i < self.tab_strip.scroll
                || hidden_rest
                || usize::from(cursor_x.saturating_add(width)) > usize::from(strip_right)
            {
                // Scrolled off, or past the room: no cells, no hit range.
                hidden_rest |= i >= self.tab_strip.scroll;
                self.tab_screen_ranges.push((cursor_x, 0));
                self.tab_close_x.push(0);
                continue;
            }
            let is_active = i == active;
            let label_x = cursor_x + 1 + icon_w;
            let close_x = label_x + label_chars + 1;
            // Treatment C: the pointer resting anywhere on a tab lifts an
            // inactive tab's body (the active tab is already prominent), and
            // resting on the close cross gives that single cell a pill.
            let tab_hovered = pointer.is_some_and(|(px, py)| {
                py == strip.y && px >= cursor_x && px < cursor_x.saturating_add(width)
            });
            let on_close = pointer == Some((close_x, strip.y));
            let hover_lift = tab_hovered && !is_active;
            let bg = if hover_lift {
                theme.tab_hover_bg()
            } else if is_active {
                active_tab_bg
            } else {
                theme.tab_inactive_bg()
            };
            if let Some(t) = tones {
                paint_surface_tab(
                    buf,
                    SurfaceTab {
                        x: cursor_x,
                        y: strip.y,
                        label: &label_text,
                        label_x,
                        close_x,
                        ed,
                        theme,
                        t,
                        bg,
                        is_active,
                        group_focused,
                        has_keyboard,
                        tab_hovered,
                        on_close,
                    },
                );
                self.tab_screen_ranges.push((cursor_x, width));
                self.tab_close_x.push(close_x);
                cursor_x = cursor_x.saturating_add(width);
                continue;
            }
            let fg = if is_active || hover_lift {
                TAB_ACTIVE_FG
            } else {
                TAB_INACTIVE_FG
            };
            // A high contrast theme resolves the label greys to its pure
            // foreground (#1548): white on High Contrast Light's pale active
            // tab would be unreadable.
            let fg = if theme.high_contrast() {
                theme.ui(fg)
            } else {
                fg
            };
            let mut modifiers = Modifier::empty();
            if is_active {
                modifiers |= Modifier::BOLD;
            }
            if ed.preview {
                modifiers |= Modifier::ITALIC;
            }
            let style = Style::default().fg(fg).bg(bg).add_modifier(modifiers);
            // The close cell shows a thumb-tack on a pinned tab (clicking it
            // unpins) and the close cross otherwise (clicking it closes).
            let close_glyph = if ed.pinned { "\u{f08d}" } else { "\u{2715}" };
            // Layout: " " + label + " " + ✕/pin + " "
            let padded = format!(" {label_text} {close_glyph} ");
            buf.set_string(cursor_x, strip.y, &padded, style);
            // Overpaint the close/pin cell with its hover pill so the user sees
            // exactly which glyph their click will land on.
            if on_close {
                let pill_bg = theme.tab_close_pill_bg();
                buf.set_string(
                    close_x,
                    strip.y,
                    close_glyph,
                    Style::default()
                        .fg(close_pill_fg(theme, pill_bg))
                        .bg(pill_bg)
                        .add_modifier(Modifier::BOLD),
                );
            }
            self.tab_screen_ranges.push((cursor_x, width));
            self.tab_close_x.push(close_x);
            cursor_x = cursor_x.saturating_add(width);
        }

        // Breadcrumb bar (VS Code editor breadcrumbs): one row below the tab
        // strip showing the file path + enclosing symbol chain. Carve it off
        // the body so the editor's own coordinate math shrinks with it.
        self.breadcrumb_ranges.clear();
        self.breadcrumb_y = None;
        if !self.breadcrumbs.is_empty() && body.height > 1 {
            let crumb_y = body.y;
            self.breadcrumb_y = Some(crumb_y);
            let theme = self.editors[active].theme;
            let bg = theme.editor_bg();
            for x in area.x..area.x + area.width {
                buf[(x, crumb_y)].set_symbol(" ");
                buf[(x, crumb_y)].set_style(Style::default().bg(bg));
            }
            let max_x = area.x + area.width;
            let mut x = area.x + 1;
            // On a `surfaces` theme the trail is in the theme's inks: faint
            // separators, muted crumbs leading to the one you are in (the
            // last) in body text, the file and symbol crumbs led by their
            // icons.
            let tones = crate::widgets::surface::tones(theme);
            let last = self.breadcrumbs.len() - 1;
            for (i, crumb) in self.breadcrumbs.iter().enumerate() {
                if i > 0 {
                    let sep = " \u{203A} "; // " › "
                    let sw = sep.chars().count() as u16;
                    if x + sw >= max_x {
                        break;
                    }
                    let sep_fg = tones.map_or(Color::DarkGray, |t| t.faint());
                    buf.set_string(x, crumb_y, sep, Style::default().fg(sep_fg).bg(bg));
                    x += sw;
                }
                if x >= max_x {
                    break;
                }
                // The icon and its gap go before the label, outside its hit
                // range, and only where a cell of the label still fits.
                if tones.is_some()
                    && let Some((glyph, color)) = crumb.icon
                    && x + 2 < max_x
                {
                    let icon_style = Style::default().fg(theme.ui(color)).bg(bg);
                    buf.set_string(x, crumb_y, glyph.to_string(), icon_style);
                    x += 2;
                }
                let avail = (max_x - x) as usize;
                // Symbol crumbs (clickable) read brighter than the informational
                // path crumbs, mirroring VS Code's active/inactive breadcrumbs.
                let fg = match tones {
                    Some(t) if i == last => t.text(),
                    Some(t) => t.muted(),
                    None if crumb.target.is_some() => self.theme.ui(Color::Gray),
                    None => Color::DarkGray,
                };
                // Budget and advance in display CELLS: `set_stringn` clips by
                // grapheme width and reports where it stopped, so a CJK or
                // emoji crumb neither overruns the bar nor gets painted over by
                // the next crumb, and its hit rect covers what was drawn.
                let (end_x, _) = buf.set_stringn(
                    x,
                    crumb_y,
                    &crumb.label,
                    avail,
                    Style::default().fg(fg).bg(bg),
                );
                let sw = end_x.saturating_sub(x);
                self.breadcrumb_ranges.push((x, sw, crumb.target));
                x = end_x;
            }
            body = Rect {
                y: body.y + 1,
                height: body.height - 1,
                ..body
            };
        }

        self.last_body = body;
        let active_editor = &mut self.editors[active];
        active_editor.cell_images = self.cell_images;
        active_editor.pointer = self.hover_pointer;
        Widget::render(active_editor, body, buf);
    }
}

impl EditorTabs {
    /// If `(col, row)` lands on a breadcrumb crumb with a jump target, return
    /// the caret position to navigate to. Path/file crumbs return `None`.
    pub fn breadcrumb_target_at(&self, col: u16, row: u16) -> Option<(u32, u32)> {
        if self.breadcrumb_y != Some(row) {
            return None;
        }
        self.breadcrumb_ranges
            .iter()
            .find(|&&(x, w, _)| col >= x && col < x + w)
            .and_then(|&(_, _, target)| target)
    }

    /// The index in `breadcrumbs` of the crumb drawn under `(col, row)`,
    /// path or symbol alike (#1608).
    pub fn breadcrumb_index_at(&self, col: u16, row: u16) -> Option<usize> {
        if self.breadcrumb_y != Some(row) {
            return None;
        }
        self.breadcrumb_ranges
            .iter()
            .position(|&(x, w, _)| col >= x && col < x + w)
    }

    /// Where crumb `i`'s picker opens: the row under the bar, at the
    /// crumb's first cell. `None` while the bar is hidden or the crumb did
    /// not fit on it.
    pub fn breadcrumb_anchor(&self, i: usize) -> Option<(u16, u16)> {
        let y = self.breadcrumb_y?;
        let &(x, _, _) = self.breadcrumb_ranges.get(i)?;
        Some((x, y + 1))
    }
}

/// Tab titles for a strip, with VS Code's labelFormat disambiguation
/// (#167): colliding file names gain the SHORTEST distinguishing
/// trailing directory context (`main.rs — alpha` beside
/// `main.rs — beta`; deeper only when the parents collide too), and
/// unique titles stay bare. Diff/preview tabs keep their own labels.
pub(crate) fn disambiguated_tab_labels(editors: &[Editor]) -> Vec<String> {
    let base: Vec<String> = editors.iter().map(tab_label).collect();
    let mut out = base.clone();
    // Group by FILE NAME, not the rendered label: decorations (the dirty
    // dot) are part of the base string, and keying on it let a dirty tab
    // escape its collision group.
    let mut groups: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, e) in editors.iter().enumerate() {
        // A symbol tab's label already differs from its file's tab, and the
        // two share a path, so no parent suffix could tell them apart.
        // A proposal merge's scratch copy is labelled as one, and its
        // parent is a cache token that would only add noise (#1353).
        if e.diff.is_none()
            && e.symbol_view.is_none()
            && !e.merge.as_ref().is_some_and(|m| m.proposal_from.is_some())
            && let Some(name) = e.path.as_deref().and_then(|p| p.file_name())
        {
            groups
                .entry(name.to_string_lossy().into_owned())
                .or_default()
                .push(i);
        }
    }
    for idxs in groups.into_values() {
        if idxs.len() < 2 {
            continue;
        }
        // Walk as deep as the longest parent in the group: two DISTINCT
        // paths sharing a filename must differ somewhere in their
        // parents, so the full-parent suffixes are always distinct — a
        // fixed cap could leave deep-shared tails identical (#168
        // review).
        let max_depth = idxs
            .iter()
            .filter_map(|&i| editors[i].path.as_deref())
            .filter_map(|p| p.parent())
            .map(|p| p.components().count())
            .max()
            .unwrap_or(1)
            .max(1);
        for depth in 1..=max_depth {
            let sufs: Vec<String> = idxs
                .iter()
                .map(|&i| {
                    dir_suffix(
                        editors[i]
                            .path
                            .as_deref()
                            .unwrap_or(std::path::Path::new("")),
                        depth,
                    )
                })
                .collect();
            let mut seen = std::collections::HashSet::new();
            let all_unique = sufs.iter().all(|s| seen.insert(s.clone()));
            if all_unique || depth == max_depth {
                for (k, &i) in idxs.iter().enumerate() {
                    if !sufs[k].is_empty() {
                        out[i] = format!("{} — {}", base[i], sufs[k]);
                    }
                }
                break;
            }
        }
    }
    out
}

/// The last `depth` directory components of `path`'s parent, joined
/// with `/` — the trailing context the disambiguated label shows.
fn dir_suffix(path: &std::path::Path, depth: usize) -> String {
    let Some(parent) = path.parent() else {
        return String::new();
    };
    let comps: Vec<String> = parent
        .components()
        .rev()
        .take(depth)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    comps.into_iter().rev().collect::<Vec<_>>().join("/")
}

/// Whether `path` is gone from disk (#900): not there at all, as opposed to
/// there but unreadable, which a deleted tab's save must not treat alike.
fn gone_from_disk(path: &Path) -> bool {
    matches!(std::fs::metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

fn tab_label(e: &Editor) -> String {
    if let Some(diff) = e.diff.as_ref() {
        let l = diff
            .left_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| diff.left_path.to_string_lossy().into_owned());
        // Synthetic single-sided diffs (deletion view, raw `git diff`
        // text view) leave `right_path` empty / `/dev/null` so the tab
        // label collapses to just the left label instead of trailing a
        // misleading "↔ null".
        let r_is_real = diff.right_path != std::path::Path::new("/dev/null")
            && !diff.right_path.as_os_str().is_empty();
        if r_is_real {
            let r = diff
                .right_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| diff.right_path.to_string_lossy().into_owned());
            return format!("{l} \u{2194} {r}");
        }
        return l;
    }
    let name = match &e.path {
        Some(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("untitled")),
        None => match e.untitled {
            Some(n) => format!("Untitled-{n}"),
            None => String::from("untitled"),
        },
    };
    // The merge editor is text underneath (dirty dot still applies), but
    // the tab says which flavour of the file it is showing.
    let name = if e.merge.as_ref().is_some_and(|m| m.proposal_from.is_some()) {
        format!("{name} (proposal merge)")
    } else if e.merge.is_some() {
        format!("{name} (merge)")
    } else if let Some(view) = e.symbol_view.as_ref() {
        // A symbol tab (#369) leads with its symbol; the file says where.
        format!("{} \u{b7} {name}", view.name)
    } else {
        name
    };
    // The lock VS Code shows on a file the user can't write (#1379).
    let name = if e.write_protected {
        format!("{name} \u{f023}")
    } else {
        name
    };
    // VS Code's mark on a tab whose file is gone from disk (#900).
    let name = if e.deleted_on_disk() {
        format!("{name} (deleted)")
    } else {
        name
    };
    if e.dirty {
        format!("\u{25cf} {name}")
    } else {
        name
    }
}

#[cfg(test)]
mod tests;
