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
mod tabs;
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
pub use multi_cursor::CaretMotion;
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
pub use tabs::Crumb;
use tabs::*;
pub use transforms::CaseTransform;
#[cfg(test)]
use transforms::*;
use typing::*;
use undo::*;
pub(crate) use viewers::ansi_color_to_tui;
pub use viewers::delete_own_gunzip_copies;
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
        self.mark_dirty();
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

    /// Mark the buffer as differing from disk, and pin its tab. A tab with
    /// unsaved changes is never the replaceable preview slot: VS Code pins a
    /// preview editor once it is dirty, so a later single-click on another
    /// file opens beside it instead of replacing it and dropping the edit.
    /// Every path that makes the buffer dirty comes through here, so a new
    /// command cannot forget to pin (#2073).
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
        self.preview = false;
    }

    /// Set whether the buffer differs from disk, pinning the tab when it
    /// does ([`Self::mark_dirty`]); for paths that work the answer out, such
    /// as an undo back to the saved text or a hex edit undone.
    pub(crate) fn set_dirty(&mut self, dirty: bool) {
        if dirty {
            self.mark_dirty();
        } else {
            self.dirty = false;
        }
    }

    /// The typing paths call this before they edit: it pins the tab at once,
    /// as `mark_dirty` does after any edit, and reveals the caret's fold so no
    /// keystroke lands on a line hidden inside a fold. Commands that move
    /// lines through folds handle them themselves (#1611).
    fn pin_on_edit(&mut self) {
        self.preview = false;
        self.reveal_cursor_fold();
    }

    /// One undoable edit at the caret, its steps in the order every such
    /// command takes them: pin the tab and open the caret's fold
    /// ([`Self::pin_on_edit`]), record the undo step `kind`, make the change
    /// (`edit`), then mark the buffer changed and colour it again. A command
    /// that leaves folds shut, records its step only sometimes, or marks the
    /// buffer changed only when the edit applied calls the steps itself.
    fn undoable_edit(&mut self, kind: EditKind, edit: impl FnOnce(&mut Self)) {
        self.pin_on_edit();
        self.push_undo(kind);
        edit(self);
        self.mark_buffer_changed();
        self.recompute_highlights();
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

#[cfg(test)]
mod tests;
