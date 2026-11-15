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

mod diff_view;
mod disk;
mod gutter;
mod highlighting;
mod lsp_overlays;
mod span_edits;
mod typing;
mod undo;
mod viewers;
mod wrap;
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
pub use span_edits::apply_span_edits_to_lines;
use span_edits::*;
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

fn write_cell(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str, style: Style) {
    use unicode_segmentation::UnicodeSegmentation;
    // Fill the slot in screen columns, not chars (#1138): a CJK or emoji
    // glyph is two columns, so `w` chars drew up to twice the slot and ran
    // into the next column. Blank it first; then a wide glyph that would
    // straddle the edge is left out and its column stays blank. Overflow is
    // cut plainly with no trailing ellipsis marker (the user wants no
    // ellipsis anywhere in croft).
    let width = w as usize;
    buf.set_stringn(x, y, " ".repeat(width), width, style);
    let mut col = 0;
    for g in text.graphemes(true) {
        let gw = crate::sheet::grapheme_width(g);
        if gw == 0 {
            continue;
        }
        if col + gw > width {
            break;
        }
        if g.contains(char::is_control) {
            let shown: String = g.chars().map(visible_control).collect();
            buf.set_stringn(x + col as u16, y, &shown, gw, style);
        } else {
            buf.set_stringn(x + col as u16, y, g, gw, style);
        }
        col += gw;
    }
}

/// A control character as a glyph a cell can show (#1002): the buffer drops
/// control characters, so `line1\nline2\ttabbed` read `line1line2tabbed`.
/// One glyph per character, so a cell's width and an edit's caret still count
/// characters: `⏎` for a newline, `→` for a tab, a control picture (`␍`,
/// `␀`, `␡`) for the rest.
fn visible_control(c: char) -> char {
    match c {
        '\n' => '\u{23ce}',
        '\t' => '\u{2192}',
        '\u{0}'..='\u{1f}' => char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}'),
        '\u{7f}' => '\u{2421}',
        c if c.is_control() => '\u{fffd}',
        c => c,
    }
}

/// Inclusive char-indexed range `(row, col)` anchor and head, where head
/// follows the cursor as the user drags / shift-arrows.  `normalised()` returns
/// the pair in row-major order so callers don't have to care which end came
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorSelection {
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

impl EditorSelection {
    pub fn new(row: usize, col: usize) -> Self {
        Self {
            anchor: (row, col),
            head: (row, col),
        }
    }
    pub fn normalised(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
    pub fn has_area(&self) -> bool {
        self.anchor != self.head
    }
}

/// One linked-editing span (#254): a single-line char-column range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LinkedRange {
    row: usize,
    start: usize,
    len: usize,
}

/// The Expand/Shrink Selection state (#254): per-cursor step stacks.
/// See the `select_expand` field doc for the validity contract.
struct SelectExpandStacks {
    /// The `edit_seq` the stacks were built against.
    edit_seq: u64,
    /// One stack per cursor: primary first, then `carets` in order.
    stacks: Vec<ExpandStack>,
}

/// One cursor's selection-growth history, smallest step first.
struct ExpandStack {
    steps: Vec<EditorSelection>,
    /// Index of the step the buffer currently shows.
    pos: usize,
}

/// A keyboard column selection (#1560): its corners as `(row, col)`, the
/// head's column free to run past a short line's end as VS Code's does,
/// and the carets and selections it left, to tell whether they are still
/// its own.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ColumnBox {
    anchor: (usize, usize),
    head: (usize, usize),
    left: ColumnBoxCarets,
}

/// The primary caret, primary selection and secondary carets a keyboard
/// column selection left behind.
type ColumnBoxCarets = (
    (usize, usize),
    Option<EditorSelection>,
    Vec<EditorSelection>,
);

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

/// The case transforms VS Code exposes as
/// `editor.action.transformTo{Upper,Lower,Title,Snake,Camel,Pascal,Kebab}case`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseTransform {
    Upper,
    Lower,
    Title,
    /// `user_name` (#1573).
    Snake,
    /// `userName` (#1573).
    Camel,
    /// `UserName` (#1573).
    Pascal,
    /// `user-name` (#1573).
    Kebab,
}

/// A single keystroke to fan out across every multi-cursor caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaretEdit {
    Insert(char),
    Backspace,
    DeleteForward,
}

/// Caret places, `(row, col)` each.
type CaretPlaces = Vec<(usize, usize)>;

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

/// Max selected-text length that still drives occurrence highlighting, matching
/// VS Code's default `editor.selectionHighlightMaxLength`.
const SELECTION_HIGHLIGHT_MAX_LEN: usize = 200;

/// The buffer's indentation preference for newly typed indentation: the tab
/// width and whether Tab inserts spaces or a literal tab. Surfaced in the
/// status bar's "Spaces: N" / "Tab Size: N" pill, which the user can click to
/// change (VS Code's "Indent Using Spaces / Tabs" + "Change Tab Display Size").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IndentStyle {
    pub width: u32,
    pub use_spaces: bool,
}

/// Detect the dominant indentation of a buffer's lines (VS Code's
/// `editor.detectIndentation`). Tabs vs spaces is a straight majority of
/// indented lines; the space width is the most common positive step
/// between consecutive indentation depths (2..=8), falling back to the
/// smallest leading run when the file never nests. `None` when nothing is
/// indented, so the caller keeps the language default. Scans at most the
/// first 10k lines: enough signal for any real file, bounded for huge ones.
pub fn detect_indentation(lines: &[String]) -> Option<IndentStyle> {
    let mut tab_lines = 0usize;
    let mut space_lines = 0usize;
    let mut steps: [usize; 9] = [0; 9];
    let mut prev_depth = 0usize;
    let mut min_run = usize::MAX;
    for line in lines.iter().take(10_000) {
        if line.starts_with('\t') {
            // Whitespace-only lines carry no intent, same as the space
            // branch below: a blank line with leftover tab indentation
            // must not vote.
            if line.chars().any(|c| !c.is_whitespace()) {
                tab_lines += 1;
            }
            continue;
        }
        let depth = line.chars().take_while(|&c| c == ' ').count();
        if depth == 0 {
            if !line.is_empty() {
                prev_depth = 0;
            }
            continue;
        }
        if line.chars().nth(depth).is_none() {
            // Whitespace-only lines carry no intent.
            continue;
        }
        space_lines += 1;
        min_run = min_run.min(depth);
        let step = depth.abs_diff(prev_depth);
        if (2..=8).contains(&step) {
            steps[step] += 1;
        }
        prev_depth = depth;
    }
    if tab_lines == 0 && space_lines == 0 {
        return None;
    }
    if tab_lines >= space_lines {
        return Some(IndentStyle {
            width: 4,
            use_spaces: false,
        });
    }
    let width = steps
        .iter()
        .enumerate()
        .skip(2)
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(&a.0)))
        .filter(|&(_, &n)| n > 0)
        .map(|(w, _)| w)
        .or(((2..=8).contains(&min_run)).then_some(min_run))?;
    Some(IndentStyle {
        width: width as u32,
        use_spaces: true,
    })
}

impl IndentStyle {
    /// The indentation unit as text: `width` spaces, or a single tab.
    pub fn unit(self) -> String {
        if self.use_spaces {
            " ".repeat(self.width as usize)
        } else {
            "\t".to_string()
        }
    }

    /// Status-bar label, matching VS Code ("Spaces: 4" / "Tab Size: 4").
    pub fn label(self) -> String {
        let kind = if self.use_spaces {
            "Spaces"
        } else {
            "Tab Size"
        };
        format!("{kind}: {}", self.width)
    }
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

    /// Fill the whole-buffer caches a first paint would otherwise compute
    /// (fold tables, conflict markers, the widest line), so a view built on
    /// another thread (#371) costs the UI thread only its visible rows.
    fn warm_render_caches(&mut self) {
        self.refresh_fold_tables();
        self.conflicts();
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

    /// The buffer's active indentation style, in precedence order: the
    /// status-bar override if set, then `.editorconfig`, then the style
    /// detected from the file's content on open, then the language default
    /// (2 spaces for YAML, 4 spaces otherwise).
    ///
    /// `.editorconfig` outranks detection because a project that STATES its
    /// style must beat a guess at it — including on a file whose current
    /// content is inconsistent with the rule, which is the case the property
    /// exists to correct. The two properties are resolved independently: a
    /// config naming only `indent_style` keeps the detected width.
    pub fn indent_style(&self) -> IndentStyle {
        if let Some(pinned) = self.indent_override {
            return pinned;
        }
        let base = self.detected_indent.unwrap_or(IndentStyle {
            width: indent_unit_for(self.lang).chars().count() as u32,
            use_spaces: true,
        });
        IndentStyle {
            width: self.editorconfig.indent_width.unwrap_or(base.width),
            use_spaces: self.editorconfig.use_spaces.unwrap_or(base.use_spaces),
        }
    }

    /// Pin the buffer's indentation style (status-bar "Indent Using …" /
    /// "Change Tab Display Size"). Affects newly typed indentation only; use
    /// [`Editor::convert_indentation`] to rewrite existing lines.
    pub fn set_indent_style(&mut self, style: IndentStyle) {
        self.indent_override = Some(style);
        self.hscroll_content_cols = None;
    }

    /// The indentation unit as text for the active style: N spaces or one tab.
    fn indent_unit(&self) -> String {
        self.indent_style().unit()
    }

    /// The display cell a row showing `line` from char `start` begins at, for
    /// tab stops: a wrapped row counts from its own left edge, a flat one
    /// from the line's (the cells scrolled past).
    fn row_origin(&self, line: &str, start: usize) -> usize {
        if self.wrap_enabled() {
            0
        } else {
            cells_before(line, start)
        }
    }

    /// The caret's column as the status bar shows it, counted in display
    /// cells from 1: a tab moves it to the next tab stop, as in VS Code and
    /// vim, so it agrees with where the caret is drawn.
    pub fn cursor_visual_col(&self) -> usize {
        self.sync_tab_cells();
        // Only a tab is more than one column; any other character, a wide
        // one or each part of `👍🏽` included, counts one, exactly as VS
        // Code's status bar counts (`CursorColumns.toStatusbarColumn`).
        self.lines
            .get(self.cursor_row)
            .map_or(self.cursor_col, |l| {
                l.chars().take(self.cursor_col).fold(0, |col, c| {
                    col + if c == '\t' { tab_cells_at(col) } else { 1 }
                })
            })
            + 1
    }

    /// Make `\t` measure and paint as this buffer's Tab Size from here on.
    pub(crate) fn sync_tab_cells(&self) {
        set_tab_cells(self.indent_style().width as usize);
    }

    /// The editor's indentation preference as the LSP `FormattingOptions`
    /// fields (`tab_size`, `insert_spaces`).
    pub fn indent_preference(&self) -> (u32, bool) {
        let s = self.indent_style();
        (s.width, s.use_spaces)
    }

    /// Human label for the buffer's language mode, for the status bar.
    pub fn language_label(&self) -> &'static str {
        if self.is_plain_log() {
            return "Log";
        }

        // SCSS, Less and Sass share the CSS grammar but are their own
        // languages to the reader and to the CSS server (#1532), so the
        // status bar names them the way VS Code does.
        if self.lang == Some(LangKind::Css) {
            let ext = self
                .path
                .as_deref()
                .and_then(|p| p.extension())
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            match ext.as_deref() {
                Some("scss") => return "SCSS",
                Some("less") => return "Less",
                Some("sass") => return "Sass",
                _ => {}
            }
        }
        language_label(self.lang)
    }

    /// The active language, for the status-bar "Change Language Mode" picker
    /// to pre-select the current entry.
    pub fn language(&self) -> Option<LangKind> {
        self.lang
    }

    /// Override the buffer's language mode (status-bar "Change Language Mode").
    /// Re-runs tree-sitter highlighting under the new grammar; the app re-syncs
    /// the LSP separately off the changed language.
    pub fn set_language(&mut self, lang: Option<LangKind>) {
        self.lang = lang;
        self.language_by_hand = false;
        self.recompute_highlights();
    }

    /// [`Self::set_language`] for a language the user picked (#1574): the
    /// tab keeps it when `files_associations` change, even when it is the
    /// language the file's name would give it anyway.
    pub fn pick_language(&mut self, lang: Option<LangKind>) {
        self.set_language(lang);
        self.language_by_hand = true;
    }

    /// Whether the language was picked by hand ([`Self::pick_language`]).
    pub fn language_picked_by_hand(&self) -> bool {
        self.language_by_hand
    }

    /// Clamp the cursor back inside the buffer after an edit that may have
    /// removed rows or shortened the cursor's line (e.g. a whole-document
    /// reformat). Without this a stale `cursor_row`/`cursor_col` can index past
    /// the end of `lines` on the next keystroke.
    pub fn clamp_cursor(&mut self) {
        if self.lines.is_empty() {
            self.cursor_row = 0;
            self.cursor_col = 0;
            return;
        }
        self.cursor_row = self.cursor_row.min(self.lines.len() - 1);
        let max_col = self.lines[self.cursor_row].chars().count();
        self.cursor_col = self.cursor_col.min(max_col);
    }

    /// The language id (VS Code identifier) of this buffer, for snippet scoping.
    ///
    /// `.jsx` and `.jsonc` share a grammar with JavaScript and JSON but not
    /// VS Code's id (#1641): snippets imported from `javascriptreact.json`
    /// are scoped `javascriptreact`, and never applied while a `.jsx` file
    /// was called `javascript`.
    pub fn scope_id(&self) -> &'static str {
        let ext = self
            .path
            .as_deref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match (self.lang, ext.as_deref()) {
            (Some(LangKind::JavaScript), Some("jsx")) => "javascriptreact",
            (Some(LangKind::Json), Some("jsonc")) => "jsonc",
            _ => language_scope_id(self.lang),
        }
    }

    /// True while a snippet's tab stops are being cycled with Tab.
    pub fn snippet_active(&self) -> bool {
        self.snippet.is_some()
    }

    /// Abandon any active snippet session (a structural key moved the caret
    /// away, so Tab should return to its normal meaning).
    pub fn cancel_snippet(&mut self) {
        self.snippet = None;
    }

    /// [`Self::expand_snippet_with`] knowing only this buffer's variables.
    #[cfg(test)]
    pub fn expand_snippet(&mut self, body: &str, prefix_len: usize) {
        self.expand_snippet_with(body, prefix_len, &|_| None);
    }

    /// The value of snippet variable `name` this buffer can tell (#914): its
    /// file, the caret's line, its comment syntax, or the clock's. `None`
    /// for any other name.
    fn snippet_var(&self, name: &str) -> Option<String> {
        let path = self.path.as_deref();
        let lossy =
            |s: Option<&std::ffi::OsStr>| s.map_or(String::new(), |s| s.to_string_lossy().into());
        let block = block_comment_tokens(self.lang);
        Some(match name {
            "TM_FILENAME" => lossy(path.and_then(Path::file_name)),
            "TM_FILENAME_BASE" => lossy(path.and_then(Path::file_stem)),
            "TM_DIRECTORY" => lossy(path.and_then(Path::parent).map(Path::as_os_str)),
            "TM_FILEPATH" => lossy(path.map(Path::as_os_str)),
            "TM_SELECTED_TEXT" | "SELECTION" => self.selection_text(),
            "TM_CURRENT_LINE" => self.lines.get(self.cursor_row).cloned().unwrap_or_default(),
            "TM_LINE_INDEX" => self.cursor_row.to_string(),
            "TM_LINE_NUMBER" => (self.cursor_row + 1).to_string(),
            "LINE_COMMENT" => self.line_comment_marker().unwrap_or_default().to_string(),
            "BLOCK_COMMENT_START" => block.map_or("", |b| b.0).to_string(),
            "BLOCK_COMMENT_END" => block.map_or("", |b| b.1).to_string(),
            _ => return crate::snippets::clock_var(name),
        })
    }

    /// Expand snippet `body` at the caret. `prefix_len` chars of the already
    /// typed prefix are removed first, then the resolved text is inserted with
    /// continuation lines indented to the current line. The caret lands on the
    /// first tab stop (selecting its placeholder); when more stops remain a
    /// session starts so Tab cycles through them. Variables resolve from this
    /// buffer first, then from `vars`, the workspace's (#914).
    pub fn expand_snippet_with(
        &mut self,
        body: &str,
        prefix_len: usize,
        vars: &dyn Fn(&str) -> Option<String>,
    ) {
        self.expand_snippet_with_in(body, prefix_len, vars, None);
    }

    /// [`Self::expand_snippet_with`] in the undo step `group`: a completion
    /// that is a snippet expands in the step of its accept, which the
    /// auto-import its server resolves afterwards joins too (#928).
    pub fn expand_snippet_with_in(
        &mut self,
        body: &str,
        prefix_len: usize,
        vars: &dyn Fn(&str) -> Option<String>,
        group: Option<u64>,
    ) {
        self.snippet = None;
        for _ in 0..prefix_len {
            self.backspace_in(group);
        }
        let start = (self.cursor_row, self.cursor_col);
        let indent: String = self
            .lines
            .get(start.0)
            .map(|l| {
                l.chars()
                    .take(start.1)
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect()
            })
            .unwrap_or_default();
        let indent_len = indent.chars().count();
        // A value's line breaks are `\n`, as the body's: stops are counted in it.
        let resolve = |name: &str| {
            let value = self.snippet_var(name).or_else(|| vars(name));
            value.map(|v| normalize_newlines(&v))
        };
        let parsed = crate::snippets::parse_body(body, &resolve);
        // A body's leading `\t` is one indent level, VS Code's convention
        // (#1554): it becomes this buffer's indent unit, so a space-indented
        // file gets no tab after its spaces. Tabs past the indentation are
        // text and stay.
        let unit = self.indent_unit();
        let grow = unit.chars().count().saturating_sub(1);
        let body_lines: Vec<&str> = parsed.text.split('\n').collect();
        let leading_tabs = |line: &str, upto: usize| {
            line.chars()
                .take(upto)
                .take_while(|c| *c == ' ' || *c == '\t')
                .filter(|c| *c == '\t')
                .count()
        };
        let text = body_lines
            .iter()
            .map(|line| {
                let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
                format!("{}{}", line[..lead].replace('\t', &unit), &line[lead..])
            })
            .collect::<Vec<_>>()
            .join("\n");
        let inserted = if indent.is_empty() {
            text
        } else {
            text.replace('\n', &format!("\n{indent}"))
        };
        // A snippet body is text the user chose, not text they wrote (#349).
        self.insert_str_as_in(&inserted, crate::provenance::Seat::Generated, group);

        let mut abs: std::collections::VecDeque<(usize, usize, usize)> = parsed
            .stops
            .iter()
            .map(|s| {
                let (ld, col) = crate::snippets::offset_to_line_col(&parsed.text, s.offset);
                let col = col + grow * leading_tabs(body_lines[ld], col);
                let row = start.0 + ld;
                let c = if ld == 0 {
                    start.1 + col
                } else {
                    indent_len + col
                };
                (row, c, s.len)
            })
            .collect();
        let Some(first) = abs.pop_front() else {
            return; // no stops: leave the caret at the end of the insertion
        };
        self.place_at_snippet_stop(first);
        if !abs.is_empty() {
            self.snippet = Some(SnippetSession {
                anchor: (first.0, first.1),
                cur_len: first.2,
                cur_text: self.span_text(first.0, first.1, first.2),
                stops: abs,
            });
        }
    }

    /// Advance to the next snippet tab stop. Later stops on the current line are
    /// shifted by the net chars typed at the current stop first. Returns false
    /// when no session is active (so the caller falls back to indenting).
    pub fn snippet_next(&mut self) -> bool {
        let Some(mut sess) = self.snippet.take() else {
            return false;
        };
        // ponytail: only edits made at the current stop, on its own line, are
        // tracked. Typing newlines or editing elsewhere mid-session drifts the
        // later stops; upgrade to real markers if that workflow matters.
        if self.cursor_row == sess.anchor.0 {
            let typed = self.cursor_col as isize - sess.anchor.1 as isize;
            let shift = typed - sess.cur_len as isize;
            let now = self.span_text(sess.anchor.0, sess.anchor.1, typed.max(0) as usize);
            if now != sess.cur_text {
                // Stops nested inside the placeholder just replaced
                // (`${1:foo(${2:x})}`) name text that is gone: drop them, or
                // the next Tab selects whatever now sits at their offset,
                // a same-length replacement included.
                let after = sess.anchor.1 + sess.cur_len;
                let anchor = sess.anchor;
                sess.stops
                    .retain(|s| !(s.0 == anchor.0 && s.1 >= anchor.1 && s.1 < after));
            }
            if shift != 0 {
                let after = sess.anchor.1 + sess.cur_len;
                for s in sess.stops.iter_mut() {
                    if s.0 == sess.anchor.0 && s.1 >= after {
                        s.1 = (s.1 as isize + shift).max(0) as usize;
                    }
                }
            }
        }
        let Some(next) = sess.stops.pop_front() else {
            return false;
        };
        self.place_at_snippet_stop(next);
        if !sess.stops.is_empty() {
            sess.anchor = (next.0, next.1);
            sess.cur_len = next.2;
            sess.cur_text = self.span_text(next.0, next.1, next.2);
            self.snippet = Some(sess);
        }
        true
    }

    /// `len` chars of row `row` from char column `col`, clamped to the line.
    fn span_text(&self, row: usize, col: usize, len: usize) -> String {
        self.lines
            .get(row)
            .map(|l| l.chars().skip(col).take(len).collect())
            .unwrap_or_default()
    }

    /// Move the caret to a resolved tab stop, selecting its placeholder text so
    /// the next keystroke replaces it.
    fn place_at_snippet_stop(&mut self, stop: (usize, usize, usize)) {
        // A stop can outlive its row (a backspace join mid-session shrinks
        // the buffer): clamp to the live grid before planting anything —
        // clamping only the cursor left a selection whose row indexed past
        // the buffer end on the next edit.
        let (row, col, len) = stop;
        let row = row.min(self.lines.len().saturating_sub(1));
        let line_len = self.line_char_len(row);
        let col = col.min(line_len);
        let end = (col + len).min(line_len);
        self.cursor_row = row;
        self.cursor_col = end;
        self.selection = (end > col).then_some(EditorSelection {
            anchor: (row, col),
            head: (row, end),
        });
    }

    fn insert_newline_raw(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let byte = self.byte_index(row, col);
        let right = self.lines[row].split_off(byte);
        self.lines.insert(row + 1, right);
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.mark_buffer_changed();
    }

    pub fn insert_newline(&mut self) {
        // Enter again on a line Enter only indented leaves that line blank
        // (#956): the indentation moves down with the caret, and the clear
        // is part of this Enter's undo step.
        let blank = self
            .auto_indent
            .take()
            .filter(|&(row, len, seq)| {
                seq == self.edit_seq
                    && row == self.cursor_row
                    && self.selection.is_none()
                    && self.is_auto_indent_line(row, len)
            })
            .map(|(row, ..)| row);
        self.pin_on_edit();
        self.push_undo(EditKind::Newline);
        self.delete_selection_inner();
        self.smart_insert_newline_inner();
        if let Some(row) = blank
            && row < self.cursor_row
            && self.lines[row].chars().all(|c| c == ' ' || c == '\t')
            && !self.lines[row].is_empty()
        {
            self.lines[row].clear();
            self.remap_carets(|p| p);
            self.mark_buffer_changed();
        }
        self.note_auto_indent();
        self.recompute_highlights();
        // Newline is an on-type trigger for several servers (#254).
        self.last_typed = Some(('\n', self.edit_seq));
    }

    /// Remember the caret's line when Enter left it holding nothing but the
    /// indentation it inserted, caret at its end (#956).
    fn note_auto_indent(&mut self) {
        let row = self.cursor_row;
        self.auto_indent = self.lines.get(row).and_then(|line| {
            let len = line.chars().count();
            (len > 0 && self.cursor_col == len && line.chars().all(|c| c == ' ' || c == '\t'))
                .then_some((row, len, self.edit_seq))
        });
    }

    /// Whether `row` still holds exactly the `len` characters of
    /// indentation Enter gave it and nothing else.
    fn is_auto_indent_line(&self, row: usize, len: usize) -> bool {
        self.lines.get(row).is_some_and(|line| {
            line.chars().count() == len && line.chars().all(|c| c == ' ' || c == '\t')
        })
    }

    /// Clear the indentation Enter put on a line the caret has since left
    /// with nothing typed on it (#956), so a blank line between statements
    /// is saved blank rather than as `"    "` that every linter flags. Called
    /// after each key and click, with `tab_left` set for every tab but the
    /// one being typed in: switching away from a tab leaves its line too.
    /// It takes no undo step of its own: the step on top is still the one
    /// before the Enter, so one Ctrl+Z still undoes the Enter.
    pub fn trim_auto_indent(&mut self, tab_left: bool) {
        let Some((row, len, seq)) = self.auto_indent else {
            return;
        };
        if seq == self.edit_seq && row == self.cursor_row && !tab_left {
            return;
        }
        self.auto_indent = None;
        // Anything typed or edited since, here or elsewhere, keeps the line.
        if seq != self.edit_seq || !self.is_auto_indent_line(row, len) {
            return;
        }
        self.lines[row].clear();
        // A selection end or an extra caret on the line stays on it, at
        // its new end, not past it.
        self.remap_carets(|p| p);
        let pos = self.last_edit_pos;
        self.mark_buffer_changed();
        self.last_edit_pos = pos;
        self.recompute_highlights();
    }

    fn smart_insert_newline_inner(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        if let Some(rule) = on_enter_rule(self.lang, &self.lines, row, self.byte_index(row, col)) {
            self.apply_on_enter(rule);
            return;
        }
        let line = self.lines[row].clone();

        // Inside a doc comment the new line continues it (#1443).
        let below = self.lines.get(row + 1..).unwrap_or(&[]);
        if let Some(enter) = comment_on_enter(self.lang, &line, col, below) {
            self.continue_comment(enter);
            return;
        }

        let leading: String = line
            .chars()
            .take(col)
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();

        let prefix_chars: Vec<char> = line.chars().take(col).collect();
        let last_non_ws = prefix_chars
            .iter()
            .rev()
            .find(|c| !c.is_whitespace())
            .copied();
        let next_char = line.chars().nth(col);

        let unit = self.indent_unit();
        let extra = if extra_indent_triggered(self.lang, last_non_ws) {
            unit.as_str()
        } else {
            ""
        };
        let pair_split = is_bracket_pair_split(self.lang, last_non_ws, next_char);

        self.insert_newline_raw();

        let new_indent = if self.lang == Some(LangKind::Yaml) {
            let prefix: String = prefix_chars.iter().collect();
            yaml_newline_indent(&prefix, &unit)
        } else {
            format!("{leading}{extra}")
        };
        for c in new_indent.chars() {
            self.insert_char_raw(c);
        }

        if pair_split {
            self.insert_newline_raw();
            for c in leading.chars() {
                self.insert_char_raw(c);
            }
            self.cursor_row -= 1;
            self.cursor_col = new_indent.chars().count();
        }
    }

    /// Carry out a Markdown Enter rule (#1495). The text after
    /// the caret moves to the new line behind the continuation, without the
    /// blanks that separated it from the caret.
    fn apply_on_enter(&mut self, rule: OnEnter) {
        let row = self.cursor_row;
        match rule {
            OnEnter::EndList => {
                self.lines[row].clear();
                self.cursor_col = 0;
                self.mark_buffer_changed();
            }
            OnEnter::Continue(text) => {
                self.insert_newline_raw();
                let rest = self.lines[row + 1]
                    .trim_start_matches([' ', '\t'])
                    .to_string();
                self.lines[row + 1] = format!("{text}{rest}");
                self.cursor_col = text.chars().count();
                self.mark_buffer_changed();
            }
        }
    }

    /// Break the line at the caret into a comment's next line (#1443): the
    /// new line starts with `enter.start`, and a `/**` just opened is closed
    /// on a line below, the caret staying on the line between.
    fn continue_comment(&mut self, enter: CommentEnter) {
        let start_len = enter.start.chars().count();
        if let CommentBelow::MoveCloser(_) = &enter.below {
            // The `*/` right after the caret goes to the closing line, so
            // the space before it goes with the break.
            while self.lines[self.cursor_row]
                .chars()
                .nth(self.cursor_col)
                .is_some_and(|c| c == ' ' || c == '\t')
            {
                self.delete_forward_raw();
            }
        }
        self.insert_newline_raw();
        for c in enter.start.chars() {
            self.insert_char_raw(c);
        }
        match enter.below {
            CommentBelow::Nothing => {}
            CommentBelow::MoveCloser(indent) => {
                self.insert_newline_raw();
                for c in indent.chars() {
                    self.insert_char_raw(c);
                }
                self.cursor_row -= 1;
                self.cursor_col = start_len;
            }
            CommentBelow::AddCloser(closer) => {
                let (row, col) = (self.cursor_row, self.cursor_col);
                self.cursor_col = self.line_char_len(row);
                self.insert_newline_raw();
                for c in closer.chars() {
                    self.insert_char_raw(c);
                }
                self.cursor_row = row;
                self.cursor_col = col;
            }
        }
    }

    /// On a symbol tab, true when a bare Backspace (`forward` false) or
    /// Delete would join the symbol's first or last line with a line outside
    /// the tab: the caret sits on the clip's first character or after its
    /// last, with no selection to delete instead.
    fn at_symbol_edge(&self, forward: bool) -> bool {
        let Some((first, end)) = self.symbol_clip() else {
            return false;
        };
        if self.selection.is_some_and(|s| s.has_area()) {
            return false;
        }
        if forward {
            self.cursor_row + 1 == end && self.cursor_col >= self.line_char_len(end - 1)
        } else {
            self.cursor_row == first && self.cursor_col == 0
        }
    }

    /// True while multi-cursor mode is active (one or more secondary carets
    /// in addition to the primary cursor).
    pub fn has_multi_cursor(&self) -> bool {
        !self.carets.is_empty()
    }

    /// Drop every secondary caret, leaving just the primary cursor. Called on
    /// Esc, mouse clicks, and any movement that isn't a multi-cursor edit.
    pub fn collapse_carets(&mut self) {
        self.multi_typing = None;
        self.carets.clear();
    }

    /// VS Code "Change All Occurrences" (Cmd+F2): select every whole-word,
    /// case-sensitive textual match of the identifier under the cursor in the
    /// current file and turn each into a caret. The match at the cursor (or
    /// the first, if the cursor isn't on one) becomes the primary selection;
    /// the rest become secondary carets. Returns the number of occurrences
    /// selected (0 when the cursor isn't on a word, leaving state untouched).
    pub fn select_all_occurrences_of_word_at_cursor(&mut self) -> usize {
        self.multi_typing = None;
        let Some((start, end)) = self.word_at(self.cursor_row, self.cursor_col) else {
            return 0;
        };
        let word: Vec<char> = self.lines[self.cursor_row]
            .chars()
            .skip(start)
            .take(end - start)
            .collect();
        if word.is_empty() {
            return 0;
        }
        let mut occ: Vec<EditorSelection> = Vec::new();
        // A symbol tab matches only inside its symbol.
        let (lo, hi) = self.symbol_clip().unwrap_or((0, self.lines.len()));
        for (row, line) in self.lines.iter().enumerate().take(hi).skip(lo) {
            let chars: Vec<char> = line.chars().collect();
            for col in find_word_occurrences(&chars, &word) {
                occ.push(EditorSelection {
                    anchor: (row, col),
                    head: (row, col + word.len()),
                });
            }
        }
        if occ.is_empty() {
            return 0;
        }
        // The occurrence containing the cursor becomes primary so typing
        // continues where the user is looking; otherwise fall back to the
        // first match in the file.
        let primary_idx = occ
            .iter()
            .position(|s| {
                s.anchor.0 == self.cursor_row
                    && self.cursor_col >= s.anchor.1
                    && self.cursor_col <= s.head.1
            })
            .unwrap_or(0);
        let primary = occ[primary_idx];
        self.selection = Some(primary);
        self.cursor_row = primary.head.0;
        self.cursor_col = primary.head.1;
        self.carets = occ
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i != primary_idx)
            .map(|(_, s)| s)
            .collect();
        self.carets.len() + 1
    }

    /// The start of every occurrence of `word` in the buffer (a symbol tab's
    /// symbol only), in document order: whole words when `whole_word`, else
    /// every case-sensitive plain-text match, left to right without overlap.
    fn occurrence_starts(&self, word: &[char], whole_word: bool) -> Vec<(usize, usize)> {
        let (lo, hi) = self.symbol_clip().unwrap_or((0, self.lines.len()));
        let mut occ = Vec::new();
        for (row, line) in self.lines.iter().enumerate().take(hi).skip(lo) {
            let chars: Vec<char> = line.chars().collect();
            let cols = if whole_word {
                find_word_occurrences(&chars, word)
            } else {
                find_text_occurrences(&chars, word)
            };
            occ.extend(cols.into_iter().map(|col| (row, col)));
        }
        occ
    }

    /// VS Code "Select All Occurrences of Find Match" (`Cmd+Shift+L`,
    /// #1558): a selection on every match of the selected text, as plain
    /// text for a hand-made selection and as whole words for one Add
    /// Selection to Next Find Match took from the caret; with no selection,
    /// every whole-word match of the word at the caret (Change All
    /// Occurrences). The selection the caret is in stays primary. Returns
    /// the number of selections, 0 when there is nothing to match.
    pub fn select_all_occurrences(&mut self) -> usize {
        self.multi_typing = None;
        let Some(sel) = self.selection.filter(|s| s.anchor != s.head) else {
            return self.select_all_occurrences_of_word_at_cursor();
        };
        let (start, end) = sel.normalised();
        if start.0 != end.0 {
            // A match across lines is not looked for: the selection stays.
            return 0;
        }
        let word: Vec<char> = self.lines[start.0]
            .chars()
            .skip(start.1)
            .take(end.1 - start.1)
            .collect();
        self.sync_occurrence_word_sel();
        let whole_word = self.occurrence_word_sel.is_some();
        let occ = self.occurrence_starts(&word, whole_word);
        if occ.is_empty() {
            return 0;
        }
        let primary_idx = occ.iter().position(|&o| o == start).unwrap_or(0);
        let as_sel = |(row, col): (usize, usize)| EditorSelection {
            anchor: (row, col),
            head: (row, col + word.len()),
        };
        let primary = as_sel(occ[primary_idx]);
        self.selection = Some(primary);
        (self.cursor_row, self.cursor_col) = primary.head;
        self.carets = occ
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != primary_idx)
            .map(|(_, &o)| as_sel(o))
            .collect();
        self.ensure_cursor_col_visible();
        self.carets.len() + 1
    }

    /// VS Code "Add Cursors to Line Ends" (`Shift+Alt+I`, #1558): a caret at
    /// the end of every line the selection covers, the selection dropped.
    /// A last line the selection only reaches at column 0 is left out, as
    /// VS Code does. The last line's caret is the primary. Returns the
    /// number of carets, 0 with no selection.
    pub fn add_cursors_to_line_ends(&mut self) -> usize {
        self.multi_typing = None;
        let Some(sel) = self.selection.filter(|s| s.anchor != s.head) else {
            return 0;
        };
        let (start, end) = sel.normalised();
        let last = if end.1 == 0 && end.0 > start.0 {
            end.0 - 1
        } else {
            end.0
        };
        let ends: Vec<(usize, usize)> = (start.0..=last)
            .map(|row| (row, self.line_char_len(row)))
            .collect();
        let Some((&primary, rest)) = ends.split_last() else {
            return 0;
        };
        self.selection = None;
        (self.cursor_row, self.cursor_col) = primary;
        self.carets = rest
            .iter()
            .map(|&(row, col)| EditorSelection::new(row, col))
            .collect();
        self.occurrence_word_sel = None;
        self.ensure_cursor_col_visible();
        self.carets.len() + 1
    }

    /// VS Code "Add Selection to Next Find Match" (`Cmd+D`): the first press
    /// selects the word under the cursor; each further press finds the next
    /// occurrence of the selected text (document order, wrapping) that is not
    /// already a caret and adds it as a secondary caret, moving the primary to
    /// it so the view follows. Returns the number of cursors (0 when the first
    /// press lands off a word, leaving state untouched).
    pub fn select_next_occurrence(&mut self) -> usize {
        self.multi_typing = None;
        // First press with no selection: select the word under the cursor.
        let Some(sel) = self.selection else {
            let Some((s, e)) = self.word_at(self.cursor_row, self.cursor_col) else {
                return 0;
            };
            let word = EditorSelection {
                anchor: (self.cursor_row, s),
                head: (self.cursor_row, e),
            };
            self.selection = Some(word);
            self.occurrence_word_sel = Some((word, self.edit_seq));
            self.cursor_col = e;
            self.ensure_cursor_col_visible();
            return 1;
        };
        // Only single-line word selections drive the incremental match search.
        let (start, end) = sel.normalised();
        if start.0 != end.0 {
            return self.carets.len() + 1;
        }
        let word: Vec<char> = self.lines[start.0]
            .chars()
            .skip(start.1)
            .take(end.1 - start.1)
            .collect();
        if word.is_empty() {
            return self.carets.len() + 1;
        }
        // Every occurrence in document order, and the set already selected
        // (the primary plus each existing caret), keyed by its start.
        self.sync_occurrence_word_sel();
        let whole_word = self.occurrence_word_sel.is_some();
        let occ = self.occurrence_starts(&word, whole_word);
        let mut taken: Vec<(usize, usize)> = self.carets.iter().map(|c| c.normalised().0).collect();
        taken.push(start);
        // Scan forward from the primary, wrapping, for the first match not yet
        // taken.
        let from = occ.iter().position(|&o| o == start).unwrap_or(0);
        let n = occ.len();
        let next = (1..=n)
            .map(|k| occ[(from + k) % n])
            .find(|o| !taken.contains(o));
        let Some((row, col)) = next else {
            return self.carets.len() + 1;
        };
        // Demote the current primary to a caret, promote the new match.
        self.carets.push(sel);
        let head = (row, col + word.len());
        let found = EditorSelection {
            anchor: (row, col),
            head,
        };
        self.selection = Some(found);
        if whole_word {
            self.occurrence_word_sel = Some((found, self.edit_seq));
        }
        self.cursor_row = head.0;
        self.cursor_col = head.1;
        self.ensure_cursor_col_visible();
        self.carets.len() + 1
    }

    /// Insert a character at every caret. Consecutive characters typed
    /// with the carets left where they were form one undo step (#913).
    ///
    /// Deliberately does not set `last_typed`: an on-type formatting reply
    /// (#254) is computed at ONE position and would be wrong at the other
    /// carets, so multi-cursor typing never arms the trigger.
    pub fn multi_insert_char(&mut self, c: char) {
        self.multi_apply(CaretEdit::Insert(c));
    }

    /// Backspace at every caret. One undo step.
    pub fn multi_backspace(&mut self) {
        self.multi_apply(CaretEdit::Backspace);
    }

    /// Forward-delete at every caret. One undo step.
    pub fn multi_delete_forward(&mut self) {
        self.multi_apply(CaretEdit::DeleteForward);
    }

    /// Apply `edit` at the primary caret and every secondary caret as a
    /// single undo step, which a typed character extends when it continues
    /// a typing burst. Carets are processed bottom-to-top so an edit never
    /// shifts the coordinates of a caret still to be processed.
    fn multi_apply(&mut self, edit: CaretEdit) {
        // A Backspace right after a keystroke that paired at every caret
        // takes both sides of each empty pair, as one caret's does (#1597).
        let armed: Vec<(usize, usize)> = match self.multi_auto_pair.take() {
            Some((seq, places, paired))
                if self.auto_close_pairs
                    && edit == CaretEdit::Backspace
                    && seq == self.edit_seq
                    && places == self.caret_places() =>
            {
                paired
            }
            _ => Vec::new(),
        };
        // A typed character joins the open typing burst only while the
        // carets sit where its last keystroke left them, with no edit since.
        let burst = self.multi_typing.take() == Some((self.edit_seq, self.caret_selections()));
        // A closer or quote stepped over at every caret edits nothing, so it
        // records no undo step, as one caret's type-over does.
        if let CaretEdit::Insert(c) = edit
            && self.auto_close_pairs
            && self.types_over_at_every_caret(c)
        {
            self.selection = None;
            self.cursor_col += 1;
            for s in &mut self.carets {
                s.head.1 += 1;
                s.anchor = s.head;
            }
            self.ensure_cursor_col_visible();
            if burst {
                self.multi_typing = Some((self.edit_seq, self.caret_selections()));
            }
            return;
        }
        self.pin_on_edit();
        let kind = match edit {
            CaretEdit::Insert(_) => EditKind::MultiInsertChar,
            CaretEdit::Backspace | CaretEdit::DeleteForward => EditKind::MultiEdit,
        };
        if !burst {
            self.break_undo_coalescing();
        }
        self.push_undo(kind);

        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        let mut items: Vec<(bool, EditorSelection)> = Vec::with_capacity(self.carets.len() + 1);
        items.push((true, primary));
        for s in &self.carets {
            items.push((false, *s));
        }
        // Descending by start so the lowest caret is edited first.
        items.sort_by_key(|item| std::cmp::Reverse(item.1.normalised().0));
        // Clear so the per-caret raw ops can't see stale carets.
        self.carets.clear();

        // Each caret's place, kept as (rows from the bottom, chars from its
        // line's end): the later edits happen above or to the left of it, and
        // leave everything after it untouched, so those two stay true where a
        // plain (row, col) goes stale on its row (`abcd` with carets at 1 and
        // 3, typing X then Y, gave `aXYbcYXd`).
        // A selection a surround kept (#1597) has its anchor placed the same
        // way: it sits after the later edits too.
        type Place = (usize, usize);
        let mut placed: Vec<(bool, Place, Option<Place>, bool)> = Vec::new();
        for (is_primary, sel) in items {
            if sel.has_area() {
                self.selection = Some(sel);
            } else {
                self.selection = None;
            }
            let last = self.lines.len().saturating_sub(1);
            self.cursor_row = sel.head.0.min(last);
            self.cursor_col = sel.head.1.min(self.line_char_len(self.cursor_row));
            let paired = self.apply_caret_edit_raw(edit, armed.contains(&sel.head));
            let place = |this: &Self, (row, col): (usize, usize)| {
                (
                    this.lines.len().saturating_sub(1) - row,
                    this.line_char_len(row).saturating_sub(col),
                )
            };
            let anchor = self
                .selection
                .filter(|s| s.has_area())
                .map(|s| place(self, s.anchor));
            placed.push((
                is_primary,
                place(self, (self.cursor_row, self.cursor_col)),
                anchor,
                paired,
            ));
        }

        self.selection = None;
        let last = self.lines.len().saturating_sub(1);
        let resolve = |this: &Self, (from_bottom, from_end): (usize, usize)| {
            let row = last.saturating_sub(from_bottom);
            (row, this.line_char_len(row).saturating_sub(from_end))
        };
        let mut new_primary = EditorSelection::new(self.cursor_row, self.cursor_col);
        let mut primary_kept = false;
        let mut new_secondary: Vec<EditorSelection> = Vec::new();
        let mut paired_places: Vec<(usize, usize)> = Vec::new();
        for (is_primary, head, anchor, paired) in placed {
            let head = resolve(self, head);
            if paired {
                paired_places.push(head);
            }
            let sel = EditorSelection {
                anchor: anchor.map_or(head, |a| resolve(self, a)),
                head,
            };
            if is_primary {
                new_primary = sel;
                primary_kept = anchor.is_some();
            } else {
                new_secondary.push(sel);
            }
        }
        self.cursor_row = new_primary.head.0.min(last);
        self.cursor_col = new_primary.head.1.min(self.line_char_len(self.cursor_row));
        if primary_kept {
            self.selection = Some(new_primary);
        }
        // Carets an edit brought together are one caret from here on: kept
        // apart, the next key edited the same spot twice (one Backspace
        // deleting two line breaks).
        let primary = (self.cursor_row, self.cursor_col);
        new_secondary.sort_unstable_by_key(|s| (s.head, s.anchor));
        new_secondary.dedup();
        new_secondary.retain(|s| s.head != primary);
        self.carets = new_secondary;
        self.recompute_highlights();
        if !paired_places.is_empty() {
            self.multi_auto_pair = Some((self.edit_seq, self.caret_places(), paired_places));
        }
        // A surround keeps its selections and is its own step, as one
        // caret's is: the next keystroke opens a new one.
        let carets = self.caret_selections();
        if kind == EditKind::MultiInsertChar && !carets.iter().any(|s| s.has_area()) {
            self.multi_typing = Some((self.edit_seq, carets));
        }
    }

    /// True when `c` typed at every caret only steps over the same closer
    /// or quote: no caret has a selection and each sits right before `c`.
    fn types_over_at_every_caret(&self, c: char) -> bool {
        if !(is_pair_closer(c) || is_pair_quote(c)) {
            return false;
        }
        let primary_sel = self.selection.is_some_and(|s| s.has_area());
        !primary_sel
            && self.carets.iter().all(|s| !s.has_area())
            && self
                .caret_places()
                .into_iter()
                .all(|(row, col)| self.lines.get(row).and_then(|l| l.chars().nth(col)) == Some(c))
    }

    /// Every caret's place, the primary first, the extra carets in order.
    fn caret_places(&self) -> Vec<(usize, usize)> {
        std::iter::once((self.cursor_row, self.cursor_col))
            .chain(self.carets.iter().map(|c| c.head))
            .collect()
    }

    /// Every caret's selection, the primary first, the extra carets in
    /// order; the primary's is empty at the cursor when it has none.
    fn caret_selections(&self) -> Vec<EditorSelection> {
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        std::iter::once(primary)
            .chain(self.carets.iter().copied())
            .collect()
    }

    /// One caret's worth of a multi-edit: no snapshot, no re-highlight (the
    /// batching caller in `multi_apply` does both once).
    ///
    /// A typed character follows the same pair rules as one caret's
    /// (#1597): surround, type-over and auto-close; with them skipped, a
    /// quote replaced every Ctrl+D selection and `)` doubled the closer.
    /// `pairs_armed` lets Backspace take both sides of an empty pair the
    /// last keystroke inserted. Returns true when a pair was inserted.
    fn apply_caret_edit_raw(&mut self, edit: CaretEdit, pairs_armed: bool) -> bool {
        match edit {
            CaretEdit::Insert(c) => {
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                let action = match self.auto_close_pairs {
                    true => self.pair_action(c),
                    false => PairAction::Plain,
                };
                match action {
                    PairAction::Surround(close) => self.surround_selection_raw(c, close),
                    PairAction::TypeOver => self.cursor_col += 1,
                    PairAction::InsertPair(close) => {
                        self.insert_pair_raw(c, close);
                        return true;
                    }
                    PairAction::Plain => {
                        self.delete_selection_inner();
                        self.insert_char_raw(c);
                    }
                }
            }
            CaretEdit::Backspace => {
                if pairs_armed
                    && !self.selection.is_some_and(|s| s.has_area())
                    && self.cursor_col > 0
                {
                    let line = self.lines.get(self.cursor_row).cloned().unwrap_or_default();
                    let prev = line.chars().nth(self.cursor_col - 1);
                    let next = line.chars().nth(self.cursor_col);
                    if let (Some(p), Some(n)) = (prev, next)
                        && auto_close_partner(p) == Some(n)
                        && !is_pair_closer(p)
                    {
                        let byte = self.byte_index(self.cursor_row, self.cursor_col);
                        self.lines[self.cursor_row].remove(byte);
                    }
                }
                self.backspace_raw();
            }
            CaretEdit::DeleteForward => self.delete_forward_raw(),
        }
        false
    }

    pub fn start_selection_at_cursor(&mut self) {
        self.selection = Some(EditorSelection::new(self.cursor_row, self.cursor_col));
    }

    pub fn extend_selection_to_cursor(&mut self) {
        if let Some(sel) = self.selection.as_mut() {
            sel.head = (self.cursor_row, self.cursor_col);
        } else {
            self.start_selection_at_cursor();
        }
    }

    /// An editor `(row, char col)` as an LSP UTF-16 `(line, character)`.
    pub fn pos_to_utf16(&self, row: usize, col: usize) -> (u32, u32) {
        if self.lines.is_empty() {
            return (0, 0);
        }
        let row = row.min(self.lines.len() - 1);
        let line = &self.lines[row];
        let byte = char_byte(line, col.min(line.chars().count()));
        (row as u32, line[..byte].encode_utf16().count() as u32)
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.occurrence_word_sel = None;
    }

    /// Forget the Add Selection to Next Find Match word marker (#1558) once
    /// the selection or the buffer has moved on from it. Called before each
    /// input event and by the occurrence commands, so a selection that left
    /// the marked one and came back (Esc, then the same word selected by
    /// hand) matches as plain text again.
    pub fn sync_occurrence_word_sel(&mut self) {
        if self
            .occurrence_word_sel
            .is_some_and(|(sel, seq)| self.selection != Some(sel) || seq != self.edit_seq)
        {
            self.occurrence_word_sel = None;
        }
    }

    // ---- Linked editing (#254) ------------------------------------------

    /// Install the server's linked-editing set: UTF-16
    /// `(start_line, start_char, end_line, end_char)` spans. Multi-line
    /// spans (which the protocol forbids anyway) and sets smaller than a
    /// pair are dropped. Positions are snapshotted against the current
    /// buffer (`edit_seq` + per-row lengths).
    pub fn set_linked_ranges(&mut self, spans: &[(u32, u32, u32, u32)]) {
        let mut ranges: Vec<LinkedRange> = spans
            .iter()
            .filter(|&&(sl, _, el, _)| sl == el && (sl as usize) < self.lines.len())
            .map(|&(sl, sc, _, ec)| {
                let row = sl as usize;
                let start = utf16_to_char_col(&self.lines[row], sc);
                let end = utf16_to_char_col(&self.lines[row], ec);
                LinkedRange {
                    row,
                    start,
                    len: end.saturating_sub(start),
                }
            })
            .collect();
        ranges.sort_by_key(|r| (r.row, r.start));
        if ranges.len() < 2 {
            self.clear_linked_ranges();
            return;
        }
        self.linked_ranges = ranges;
        self.resnapshot_linked_rows();
        self.linked_seq = self.edit_seq;
    }

    /// The primary caret as an LSP UTF-16 `(line, character)` pair.
    pub fn cursor_position_utf16(&self) -> (u32, u32) {
        let row = self.cursor_row.min(self.lines.len().saturating_sub(1));
        let line = &self.lines[row];
        let byte = char_byte(line, self.cursor_col.min(line.chars().count()));
        (row as u32, line[..byte].encode_utf16().count() as u32)
    }

    pub fn clear_linked_ranges(&mut self) {
        self.linked_ranges.clear();
        self.linked_rows.clear();
    }

    pub fn has_linked_ranges(&self) -> bool {
        !self.linked_ranges.is_empty()
    }

    /// True when `(row, col)` sits inside (or at the end boundary of) a
    /// linked range — the caret positions where the set stays alive.
    pub fn linked_ranges_contain(&self, row: usize, col: usize) -> bool {
        self.linked_ranges
            .iter()
            .any(|r| r.row == row && col >= r.start && col <= r.start + r.len)
    }

    fn resnapshot_linked_rows(&mut self) {
        let mut rows: Vec<usize> = self.linked_ranges.iter().map(|r| r.row).collect();
        rows.dedup();
        self.linked_rows = rows
            .into_iter()
            .map(|row| (row, self.lines[row].chars().count()))
            .collect();
    }

    /// Replay the last edit across the sibling linked ranges (#254): the
    /// paired-tag auto-rename. Called once per tick after key handling;
    /// returns true when siblings were rewritten. Trusts only a clean
    /// single-step, single-row edit that began inside one range and
    /// keeps the content a plausible tag word — anything else clears
    /// the set rather than corrupt text far from the caret.
    pub fn mirror_linked_edit(&mut self) -> bool {
        if self.linked_ranges.len() < 2 || self.edit_seq == self.linked_seq {
            return false;
        }
        // A single edit advances the seq exactly once; a missed frame
        // (paste burst, undo, reload) is not safely replayable.
        if self.edit_seq != self.linked_seq.wrapping_add(1) {
            self.clear_linked_ranges();
            return false;
        }
        let (erow, ecol) = self.last_edit_origin;
        // Exactly one linked row may have changed, by the edit's delta.
        let changed: Vec<(usize, isize)> = self
            .linked_rows
            .iter()
            .filter_map(|&(row, old_len)| {
                let now = self.lines.get(row)?.chars().count() as isize;
                let delta = now - old_len as isize;
                (delta != 0).then_some((row, delta))
            })
            .collect();
        let delta = match changed.as_slice() {
            [] => 0isize,
            [(row, delta)] if *row == erow => *delta,
            _ => {
                self.clear_linked_ranges();
                return false;
            }
        };
        // The edit must have begun inside one range on that row.
        let Some(i) = self
            .linked_ranges
            .iter()
            .position(|r| r.row == erow && ecol >= r.start && ecol <= r.start + r.len)
        else {
            self.clear_linked_ranges();
            return false;
        };
        let new_len = self.linked_ranges[i].len as isize + delta;
        if new_len < 0 {
            self.clear_linked_ranges();
            return false;
        }
        let new_len = new_len as usize;
        // A boundary-adjacent backspace/delete-forward removes the
        // delimiter OUTSIDE the range, not the range's own content, and
        // leaves the caret outside the updated span. Reject that instead
        // of writing a truncated word into the siblings.
        if self.cursor_row != erow
            || self.cursor_col < self.linked_ranges[i].start
            || self.cursor_col > self.linked_ranges[i].start + new_len
        {
            self.clear_linked_ranges();
            return false;
        }
        let src = self.linked_ranges[i];
        let line = &self.lines[src.row];
        let from = char_byte(line, src.start);
        let to = char_byte(line, src.start + new_len);
        let text: String = line[from..to].to_string();
        // Keep it a plausible identifier/tag word; a delimiter means the
        // user is doing something the mirror shouldn't propagate.
        if text
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '<' | '>' | '/' | '"' | '\''))
        {
            self.clear_linked_ranges();
            return false;
        }
        // Rewrite every sibling, rightmost-first per row so earlier
        // starts stay valid while lengths change.
        let mut order: Vec<usize> = (0..self.linked_ranges.len()).filter(|&j| j != i).collect();
        order.sort_by_key(|&j| {
            let r = &self.linked_ranges[j];
            (r.row, std::cmp::Reverse(r.start))
        });
        for j in order {
            let r = self.linked_ranges[j];
            // Siblings after the edit on the edited row already shifted.
            let start = if r.row == erow && r.start > ecol {
                (r.start as isize + delta).max(0) as usize
            } else {
                r.start
            };
            let line = &mut self.lines[r.row];
            let from = char_byte(line, start);
            let to = char_byte(line, start + r.len);
            line.replace_range(from..to, &text);
        }
        // Recompute stored positions: every range now has `new_len`; on
        // each row, starts shift by the accumulated growth of preceding
        // ranges (the edited range's own delta included).
        let per = new_len as isize - src.len as isize;
        let mut shift: std::collections::HashMap<usize, isize> = std::collections::HashMap::new();
        for r in self.linked_ranges.iter_mut() {
            let acc = shift.entry(r.row).or_insert(0);
            r.start = (r.start as isize + *acc).max(0) as usize;
            r.len = new_len;
            *acc += per;
        }
        // The mirrors changed the buffer: bump the seq so highlights /
        // LSP resync, and adopt it as the new baseline. Deliberately no
        // undo push — the user's keystroke snapshot holds the whole
        // pre-edit buffer, so one Undo reverts the keystroke AND the
        // mirrors together (VS Code's model).
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.resnapshot_linked_rows();
        self.linked_seq = self.edit_seq;
        true
    }

    // ---- Expand / Shrink Selection (#254) -------------------------------

    /// Every cursor's current selection, primary first — a zero-area
    /// selection at the caret when nothing is selected.
    fn cursor_selections(&self) -> Vec<EditorSelection> {
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        std::iter::once(primary)
            .chain(self.carets.iter().copied())
            .collect()
    }

    /// Ensure the expand stacks describe the CURRENT cursors; rebuild
    /// from scratch on any mismatch (edit, click, caret reshuffle). See
    /// the `select_expand` field doc for why matching, not indexing, is
    /// the validity test.
    pub fn validate_expand_stacks(&mut self) {
        let cur = self.cursor_selections();
        let valid = self.select_expand.as_ref().is_some_and(|se| {
            se.edit_seq == self.edit_seq
                && se.stacks.len() == cur.len()
                && se.stacks.iter().zip(&cur).all(|(st, c)| {
                    st.steps
                        .get(st.pos)
                        .is_some_and(|s| s.normalised() == c.normalised())
                })
        });
        if !valid {
            self.select_expand = Some(SelectExpandStacks {
                edit_seq: self.edit_seq,
                stacks: cur
                    .into_iter()
                    .map(|c| ExpandStack {
                        steps: vec![c],
                        pos: 0,
                    })
                    .collect(),
            });
        }
    }

    /// Write every stack's current step back onto the cursors, keeping
    /// the cursor-follows-head convention of every other selection setter.
    fn apply_expand_steps(&mut self) {
        let Some(se) = self.select_expand.as_ref() else {
            return;
        };
        let sels: Vec<EditorSelection> = se.stacks.iter().map(|st| st.steps[st.pos]).collect();
        let primary = sels[0];
        self.selection = primary.has_area().then_some(primary);
        self.cursor_row = primary.head.0.min(self.lines.len().saturating_sub(1));
        self.cursor_col = primary.head.1.min(self.line_char_len(self.cursor_row));
        self.carets = sels[1..].to_vec();
        self.ensure_cursor_col_visible();
    }

    /// Step every cursor one level up its cached chain. False when no
    /// stack has a deeper step cached (the caller then computes one —
    /// LSP chain or syntax ancestry).
    pub fn expand_selection_from_stack(&mut self) -> bool {
        let Some(se) = self.select_expand.as_mut() else {
            return false;
        };
        let mut any = false;
        for st in &mut se.stacks {
            if st.pos + 1 < st.steps.len() {
                st.pos += 1;
                any = true;
            }
        }
        if any {
            self.apply_expand_steps();
        }
        any
    }

    /// Shrink Selection: retrace one step down the stack. False at the
    /// bottom (the gesture's starting point).
    pub fn shrink_selection_step(&mut self) -> bool {
        self.validate_expand_stacks();
        let Some(se) = self.select_expand.as_mut() else {
            return false;
        };
        let mut any = false;
        for st in &mut se.stacks {
            if st.pos > 0 {
                st.pos -= 1;
                any = true;
            }
        }
        if any {
            self.apply_expand_steps();
        }
        any
    }

    /// Install per-cursor LSP `selectionRange` chains (#254): UTF-16
    /// `(start_line, start_char, end_line, end_char)` spans, smallest
    /// first, index-aligned with the cursors. Each stack's history above
    /// its current step is replaced by the strictly-growing tail of its
    /// chain; the caller then calls [`Self::expand_selection_from_stack`].
    pub fn install_selection_chains(&mut self, chains: Vec<Vec<(u32, u32, u32, u32)>>) {
        self.validate_expand_stacks();
        // Convert before mutably borrowing the stacks.
        let converted: Vec<Vec<EditorSelection>> = chains
            .iter()
            .map(|chain| {
                chain
                    .iter()
                    .map(|&(sl, sc, el, ec)| {
                        let sr = (sl as usize).min(self.lines.len().saturating_sub(1));
                        let er = (el as usize).min(self.lines.len().saturating_sub(1));
                        EditorSelection {
                            anchor: (sr, utf16_to_char_col(&self.lines[sr], sc)),
                            head: (er, utf16_to_char_col(&self.lines[er], ec)),
                        }
                    })
                    .collect()
            })
            .collect();
        let Some(se) = self.select_expand.as_mut() else {
            return;
        };
        for (st, chain) in se.stacks.iter_mut().zip(converted) {
            let cur = st.steps[st.pos].normalised();
            st.steps.truncate(st.pos + 1);
            let mut last = cur;
            for sel in chain {
                let n = sel.normalised();
                // Keep only strictly-growing spans that still contain the
                // current selection, so a chain answered for a slightly
                // different anchor can't shrink or jump the gesture.
                if n.0 <= last.0 && n.1 >= last.1 && n != last {
                    st.steps.push(sel);
                    last = n;
                }
            }
        }
    }

    /// Grow every cursor's selection to the nearest strictly-larger
    /// tree-sitter node (#254's serverless fallback). Without a grammar,
    /// grows to the line span and then the whole buffer, so the command
    /// always answers. Returns false when nothing could grow.
    pub fn expand_selection_syntax(&mut self) -> bool {
        self.validate_expand_stacks();
        let tree = self.lang.map(crate::highlight::language_for).and_then(|l| {
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&l).ok()?;
            parser.parse(self.lines.join("\n"), None)
        });
        let Some(se) = self.select_expand.as_ref() else {
            return false;
        };
        let mut grown: Vec<Option<EditorSelection>> = Vec::with_capacity(se.stacks.len());
        for st in &se.stacks {
            let cur = st.steps[st.pos].normalised();
            let next = match &tree {
                Some(t) => self.syntax_enclosing_range(t, cur),
                None => self.textual_enclosing_range(cur),
            };
            grown.push(next);
        }
        let Some(se) = self.select_expand.as_mut() else {
            return false;
        };
        let mut any = false;
        for (st, next) in se.stacks.iter_mut().zip(grown) {
            if let Some(sel) = next {
                st.steps.truncate(st.pos + 1);
                st.steps.push(sel);
                st.pos += 1;
                any = true;
            }
        }
        if any {
            self.apply_expand_steps();
        }
        any
    }

    /// The smallest tree-sitter node range strictly containing `cur`
    /// (char coordinates), climbing `.parent()` past same-range wrappers.
    fn syntax_enclosing_range(
        &self,
        tree: &tree_sitter::Tree,
        cur: ((usize, usize), (usize, usize)),
    ) -> Option<EditorSelection> {
        let start_b = self.char_pos_to_byte(cur.0);
        let end_b = self.char_pos_to_byte(cur.1);
        let mut node = tree.root_node().descendant_for_byte_range(start_b, end_b)?;
        loop {
            let (ns, ne) = (node.start_byte(), node.end_byte());
            let contains = ns <= start_b && ne >= end_b;
            let strictly = contains && (ns < start_b || ne > end_b);
            if strictly {
                let anchor = self.ts_point_to_char(node.start_position());
                let head = self.ts_point_to_char(node.end_position());
                return Some(EditorSelection { anchor, head });
            }
            node = node.parent()?;
        }
    }

    /// Grammar-less growth: the current line span, then the whole buffer.
    fn textual_enclosing_range(
        &self,
        cur: ((usize, usize), (usize, usize)),
    ) -> Option<EditorSelection> {
        let (start, end) = cur;
        let line_sel = EditorSelection {
            anchor: (start.0, 0),
            head: (end.0, self.line_char_len(end.0)),
        };
        let n = line_sel.normalised();
        if n.0 < start || n.1 > end {
            return Some(line_sel);
        }
        let last = self.lines.len().saturating_sub(1);
        let all = EditorSelection {
            anchor: (0, 0),
            head: (last, self.line_char_len(last)),
        };
        let n = all.normalised();
        (n.0 < start || n.1 > end).then_some(all)
    }

    /// Byte offset of char position `(row, col)` in the `\n`-joined text.
    fn char_pos_to_byte(&self, pos: (usize, usize)) -> usize {
        let mut off = 0usize;
        for (i, l) in self.lines.iter().enumerate() {
            if i == pos.0 {
                return off + char_byte(l, pos.1.min(l.chars().count()));
            }
            off += l.len() + 1;
        }
        off.saturating_sub(1)
    }

    /// Tree-sitter `Point` (row + BYTE column) → editor `(row, char col)`.
    fn ts_point_to_char(&self, p: tree_sitter::Point) -> (usize, usize) {
        let row = p.row.min(self.lines.len().saturating_sub(1));
        let line = &self.lines[row];
        let col = line[..p.column.min(line.len())].chars().count();
        (row, col)
    }

    /// The cursors' positions as LSP UTF-16 `(line, character)` pairs,
    /// primary first — `textDocument/selectionRange`'s `positions` input.
    pub fn cursor_positions_utf16(&self) -> Vec<(u32, u32)> {
        self.cursor_selections()
            .iter()
            .map(|s| {
                let (row, col) = s.head;
                let row = row.min(self.lines.len().saturating_sub(1));
                let line = &self.lines[row];
                let byte = char_byte(line, col.min(line.chars().count()));
                let u16col = line[..byte].encode_utf16().count() as u32;
                (row as u32, u16col)
            })
            .collect()
    }

    pub fn select_all(&mut self) {
        if self.lines.is_empty() {
            self.selection = None;
            return;
        }
        let last_row = self.lines.len() - 1;
        let last_col = self.line_char_len(last_row);
        self.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (last_row, last_col),
        });
        self.cursor_row = last_row;
        self.cursor_col = last_col;
    }

    /// Extract the selection text (`\n`-joined across rows) using the
    /// editor's char-indexed coordinates.  Returns "" when there's no
    /// selection or the selection is zero-area.
    pub fn selection_text(&self) -> String {
        let Some(sel) = self.selection else {
            return String::new();
        };
        if !sel.has_area() {
            return String::new();
        }
        let ((sr, sc), (er, ec)) = sel.normalised();
        if sr == er {
            let line = &self.lines[sr];
            let from = char_byte(line, sc);
            let to = char_byte(line, ec);
            return line[from..to].to_string();
        }
        let mut out = String::new();
        // first row: from sc to end of line
        let first = &self.lines[sr];
        let from = char_byte(first, sc);
        out.push_str(&first[from..]);
        out.push('\n');
        // full middle rows
        for r in (sr + 1)..er {
            out.push_str(&self.lines[r]);
            out.push('\n');
        }
        // last row: from start to ec
        let last = &self.lines[er];
        let to = char_byte(last, ec);
        out.push_str(&last[..to]);
        out
    }

    /// The literal text to light up as "other occurrences" of the current
    /// selection (VS Code's selection highlight). Returns `None` when the
    /// selection shouldn't drive occurrence highlighting at all. Computed once
    /// per render and fed to `paint_selection_occurrences` for every visible
    /// line.
    fn selection_occurrence_needle(&self) -> Option<String> {
        let sel = self.selection?;
        if !sel.has_area() {
            return None;
        }
        let ((sr, sc), (er, ec)) = sel.normalised();
        if sr != er {
            // VS Code only highlights occurrences for single-line selections.
            return None;
        }
        let line = &self.lines[sr];
        let text = &line[char_byte(line, sc)..char_byte(line, ec)];
        // Mirror VS Code's non-empty-selection branch: skip whitespace-only
        // selections and anything past the 200-char ceiling
        // (`editor.selectionHighlightMaxLength`); otherwise highlight the
        // literal substring. No minimum length, no whole-word filter.
        if text.trim().is_empty() || text.chars().count() > SELECTION_HIGHLIGHT_MAX_LEN {
            return None;
        }
        Some(text.to_string())
    }

    /// Delete the current selection if it has area.  Returns true iff content
    /// was removed.  Cursor lands at the start of the deleted range and the
    /// selection is cleared.  Pushes an undo step.
    pub fn delete_selection(&mut self) -> bool {
        if !self.selection.map(|s| s.has_area()).unwrap_or(false) {
            self.selection = None;
            return false;
        }
        self.push_undo(EditKind::DeleteSelection);
        let removed = self.delete_selection_inner();
        if removed {
            self.recompute_highlights();
        }
        removed
    }

    /// Same as `delete_selection` but does NOT push an undo step or
    /// recompute highlights — used by other public mutators that have
    /// already snapshotted state and will recompute themselves.
    fn delete_selection_inner(&mut self) -> bool {
        let Some(sel) = self.selection else {
            return false;
        };
        if !sel.has_area() {
            self.selection = None;
            return false;
        }
        let ((sr, sc), (er, ec)) = sel.normalised();
        if sr == er {
            let line = &mut self.lines[sr];
            let from = char_byte(line, sc);
            let to = char_byte(line, ec);
            line.replace_range(from..to, "");
        } else {
            let last = self.lines.remove(er);
            for _ in (sr + 1)..er {
                self.lines.remove(sr + 1);
            }
            let first = &mut self.lines[sr];
            let from = char_byte(first, sc);
            first.truncate(from);
            let to = char_byte(&last, ec);
            first.push_str(&last[to..]);
        }
        self.cursor_row = sr;
        self.cursor_col = sc;
        self.selection = None;
        self.mark_buffer_changed();
        true
    }

    /// Leading-whitespace width of `line`, or `None` for a blank line (a blank
    /// line belongs to whatever block surrounds it, so it carries no indent of
    /// its own). Tabs and spaces each count as one column, which is all the
    /// fold heuristic needs: it only ever compares indents with `>`.
    fn indent_width(&self, line: usize) -> Option<usize> {
        let s = self.lines.get(line)?;
        (!s.trim().is_empty()).then(|| s.chars().take_while(|c| *c == ' ' || *c == '\t').count())
    }

    /// The foldable region headed by `line`, as `(line, last_line_inclusive)`,
    /// or `None` when nothing below `line` is more indented. Interior blank
    /// lines are absorbed; trailing blanks and the first line back at (or
    /// below) the header's indent are not. This is VS Code's default
    /// indentation-based folding: a pure function of the text, no LSP or
    /// tree-sitter round-trip.
    pub fn fold_range(&self, line: usize) -> Option<(usize, usize)> {
        // Server spans first (#254): while a capable server has answered
        // for this exact line count, its ranges REPLACE the heuristics
        // wholesale — VS Code's provider model. rust-analyzer and friends
        // emit region markers and comment spans themselves.
        if let Some(ranges) = self.lsp_folds_current() {
            return ranges
                .iter()
                .filter(|r| r.start_line == line)
                .map(|r| (line, r.end_line.min(self.lines.len().saturating_sub(1))))
                .max_by_key(|&(_, end)| end);
        }
        // Fallback #1: the marker/comment table (`#region` pairs, comment
        // runs) — folds indentation alone can't see.
        if let Some(&(s, e, _)) = self
            .fallback_kind_folds
            .iter()
            .find(|&&(s, _, _)| s == line)
        {
            return Some((s, e.min(self.lines.len().saturating_sub(1))));
        }
        // Fallback #2: the indentation scan.
        self.indent_fold_range(line)
    }

    /// The plain indentation-based fold heuristic, a pure function of the
    /// text — the last-resort scanner behind the server spans and the
    /// marker/comment table.
    fn indent_fold_range(&self, line: usize) -> Option<(usize, usize)> {
        let base = self.indent_width(line)?;
        let mut end = line;
        let mut i = line + 1;
        while i < self.lines.len() {
            match self.indent_width(i) {
                None => {} // blank: possibly interior, keep scanning
                Some(w) if w > base => end = i,
                Some(_) => break, // back to base indent or shallower
            }
            i += 1;
        }
        (end > line).then_some((line, end))
    }

    /// Whether `line` heads a collapsible region.
    pub fn is_foldable(&self, line: usize) -> bool {
        self.fold_range(line).is_some()
    }

    /// Server fold spans, but only while they still describe this buffer:
    /// an edit changes the line count and orphans them until the next
    /// reply (same whole-buffer posture as `fold_epoch_lines`).
    fn lsp_folds_current(&self) -> Option<&[crate::lsp::manager::FoldingRangeItem]> {
        match &self.lsp_folds {
            Some(ranges) if self.lsp_folds_lines == self.lines.len() => Some(ranges),
            _ => None,
        }
    }

    /// Install a server's fold-span set (#254). The caller (the app's LSP
    /// drain) already seq-gated the reply; the line count is recorded so
    /// later edits orphan the spans instead of folding the wrong lines.
    pub fn set_lsp_folds(&mut self, ranges: Vec<crate::lsp::manager::FoldingRangeItem>) {
        self.lsp_folds = Some(ranges);
        self.lsp_folds_lines = self.lines.len();
        // Spans under existing collapsed headers may have moved.
        if !self.folded.is_empty() {
            self.rebuild_hidden_ranges();
        }
    }

    /// Rebuild the fallback marker/comment fold table when the buffer
    /// changed. Cheap no-op per frame otherwise; called from the render
    /// and from every fold command so headless callers agree with paint.
    pub fn refresh_fold_tables(&mut self) {
        if self.fallback_folds_seq == Some(self.edit_seq) {
            return;
        }
        self.fallback_folds_seq = Some(self.edit_seq);
        self.fallback_kind_folds.clear();
        use crate::lsp::manager::FoldRangeKind;
        // Region markers: a stack pairs nested #region/#endregion.
        let mut stack: Vec<usize> = Vec::new();
        for (i, l) in self.lines.iter().enumerate() {
            match region_marker(l) {
                Some(true) => stack.push(i),
                Some(false) => {
                    if let Some(start) = stack.pop()
                        && i > start
                    {
                        self.fallback_kind_folds
                            .push((start, i, FoldRangeKind::Region));
                    }
                }
                None => {}
            }
        }
        // Comment runs: ≥2 consecutive full-line comments fold as one
        // block from the first line.
        let mut run_start: Option<usize> = None;
        for i in 0..=self.lines.len() {
            let is_comment = self
                .lines
                .get(i)
                .is_some_and(|l| comment_line(l) && region_marker(l).is_none());
            match (run_start, is_comment) {
                (None, true) => run_start = Some(i),
                (Some(s), false) => {
                    if i - s >= 2 {
                        self.fallback_kind_folds
                            .push((s, i - 1, FoldRangeKind::Comment));
                    }
                    run_start = None;
                }
                _ => {}
            }
        }
        self.fallback_kind_folds
            .sort_unstable_by_key(|&(s, e, _)| (s, e));
    }

    /// The fold kind at a header line, when one is known: the server's
    /// kind, else the fallback table's. Plain indentation folds have no
    /// kind — "Fold All Comments/Regions" skips them.
    pub fn fold_kind_at(&self, line: usize) -> Option<crate::lsp::manager::FoldRangeKind> {
        if let Some(ranges) = self.lsp_folds_current() {
            return ranges
                .iter()
                .filter(|r| r.start_line == line)
                .max_by_key(|r| r.end_line)
                .map(|r| r.kind);
        }
        self.fallback_kind_folds
            .iter()
            .find(|&&(s, _, _)| s == line)
            .map(|&(_, _, k)| k)
    }

    /// Collapse every fold of `kind` — "Fold All Comments" (Cmd+K Cmd+/) /
    /// "Fold All Regions" (Cmd+K Cmd+8).
    pub fn fold_all_of_kind(&mut self, kind: crate::lsp::manager::FoldRangeKind) {
        self.refresh_fold_tables();
        let headers: Vec<usize> = (0..self.lines.len())
            .filter(|&l| self.fold_kind_at(l) == Some(kind) && self.is_foldable(l))
            .collect();
        if headers.is_empty() {
            return;
        }
        self.folded.extend(headers);
        self.fold_epoch_lines = self.lines.len();
        self.rebuild_hidden_ranges();
        while self.is_line_hidden(self.cursor_row) {
            self.cursor_row = self.cursor_row.saturating_sub(1);
        }
    }

    /// Expand every collapsed fold of `kind` — "Unfold All Regions"
    /// (Cmd+K Cmd+9). The mirror of [`Self::fold_all_of_kind`].
    pub fn unfold_all_of_kind(&mut self, kind: crate::lsp::manager::FoldRangeKind) {
        self.refresh_fold_tables();
        let drop: Vec<usize> = self
            .folded
            .iter()
            .copied()
            .filter(|&l| self.fold_kind_at(l) == Some(kind))
            .collect();
        if drop.is_empty() {
            return;
        }
        for l in drop {
            self.folded.remove(&l);
        }
        self.rebuild_hidden_ranges();
    }

    /// The OUTERMOST fold header whose region covers `line` — the function's
    /// `def`/`fn` line rather than the nearest `if`/`for` block that
    /// [`Self::enclosing_fold_header`] stops at. Climbs header-by-header;
    /// each step strictly decreases the line, so it terminates.
    fn outermost_enclosing_header(&self, line: usize) -> Option<usize> {
        let mut head = self.enclosing_fold_header(line)?;
        loop {
            let above = (0..head)
                .rev()
                .find(|&h| matches!(self.fold_range(h), Some((_, end)) if head <= end));
            match above {
                Some(a) => head = a,
                None => return Some(head),
            }
        }
    }

    /// Column distance between indentation guides: the indent width for
    /// space-indented buffers, one column for tab indentation (a leading tab
    /// occupies a single cell, so each tab is one nesting level).
    fn guide_step(&self) -> usize {
        let s = self.indent_style();
        if s.use_spaces {
            s.width.max(1) as usize
        } else {
            1
        }
    }

    /// Indent width `line` contributes to guide painting. A non-blank line is
    /// its own leading-whitespace width; a blank line borrows the minimum of
    /// its nearest non-blank neighbours, so guides run through gaps inside a
    /// block but stop between top-level blocks (VS Code's rule). The
    /// neighbour scans are capped: past a 200-blank-line gap guides simply
    /// stop, which keeps the per-row render cost bounded.
    fn guide_indent_width(&self, line: usize) -> usize {
        self.guide_indent(line).0
    }

    /// [`Self::guide_indent_width`] plus the line whose leading whitespace
    /// that width counts: the line itself, or for a blank line the neighbour
    /// it borrowed from. The width is in characters, so where its guides sit
    /// on screen has to be measured on that line (a tab is several cells; a
    /// blank line has none of its own to measure).
    fn guide_indent(&self, line: usize) -> (usize, usize) {
        const BLANK_SCAN_CAP: usize = 200;
        if let Some(w) = self.indent_width(line) {
            return (w, line);
        }
        let above = (0..line)
            .rev()
            .take(BLANK_SCAN_CAP)
            .find_map(|i| self.indent_width(i).map(|w| (w, i)));
        let below = (line + 1..self.lines.len())
            .take(BLANK_SCAN_CAP)
            .find_map(|i| self.indent_width(i).map(|w| (w, i)));
        match (above, below) {
            (Some(a), Some(b)) => {
                if b.0 < a.0 {
                    b
                } else {
                    a
                }
            }
            _ => (0, line),
        }
    }

    /// The active indentation guide (VS Code
    /// `editor.guides.highlightActiveIndentation`): the innermost guide of the
    /// block containing the cursor, as `(guide column, first line, last line)`
    /// inclusive. A line whose next non-blank neighbour is deeper is a block
    /// header and activates the guide of the body it opens. `None` at top
    /// level. The block walk is capped so a pathological single block can't
    /// make a frame scan the whole file.
    fn active_indent_guide(&self) -> Option<(usize, usize, usize)> {
        const BLOCK_WALK_CAP: usize = 5_000;
        let step = self.guide_step();
        let cr = self.cursor_row.min(self.lines.len().saturating_sub(1));
        let own = self
            .indent_width(cr)
            .unwrap_or_else(|| self.guide_indent_width(cr));
        let below = (cr + 1..self.lines.len())
            .take(BLOCK_WALK_CAP)
            .find_map(|i| self.indent_width(i))
            .unwrap_or(0);
        let target = own.max(below);
        if target == 0 {
            return None;
        }
        // The innermost guide of a block indented `target`: the last guide
        // column strictly below it.
        let col = (target - 1) / step * step;
        // Anchor on a row that actually draws the guide: the cursor row for a
        // body line, the first body row for a header.
        let anchor = if own > col { cr } else { cr + 1 };
        if anchor >= self.lines.len() || self.guide_indent_width(anchor) <= col {
            return None;
        }
        let mut lo = anchor;
        while lo > 0 && anchor - (lo - 1) <= BLOCK_WALK_CAP && self.guide_indent_width(lo - 1) > col
        {
            lo -= 1;
        }
        let mut hi = anchor;
        while hi + 1 < self.lines.len()
            && (hi + 1) - anchor <= BLOCK_WALK_CAP
            && self.guide_indent_width(hi + 1) > col
        {
            hi += 1;
        }
        Some((col, lo, hi))
    }

    /// The nearest foldable header at or above `line` whose region contains
    /// `line`, so "fold at cursor" collapses the enclosing block when the
    /// cursor sits in a body line rather than on the header itself.
    fn enclosing_fold_header(&self, line: usize) -> Option<usize> {
        if self.is_foldable(line) {
            return Some(line);
        }
        (0..line)
            .rev()
            .find(|&h| matches!(self.fold_range(h), Some((_, end)) if line <= end))
    }

    /// How many lines apart `a` and `b` are on screen: the lines between
    /// them, less those a collapsed fold hides.
    fn visible_line_distance(&self, a: usize, b: usize) -> usize {
        let (lo, hi) = (a.min(b), a.max(b));
        let hidden: usize = self
            .hidden_ranges
            .iter()
            .map(|&(header, end)| {
                let (from, to) = ((header + 1).max(lo + 1), end.min(hi));
                (to + 1).saturating_sub(from)
            })
            .sum();
        (hi - lo).saturating_sub(hidden)
    }

    /// Whether the primary caret or an extra one is on `line`.
    fn is_caret_line(&self, line: usize) -> bool {
        line == self.cursor_row || self.carets.iter().any(|c| c.head.0 == line)
    }

    /// Whether `line` is hidden inside a collapsed region. A binary search over
    /// the merged spans in [`Self::hidden_ranges`], which the fold writes keep
    /// current — this is called for every rendered row of every frame.
    pub fn is_line_hidden(&self, line: usize) -> bool {
        self.hidden_ranges
            .binary_search_by(|&(header, end)| {
                if line <= header {
                    std::cmp::Ordering::Greater
                } else if line > end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }

    /// Recompute [`Self::hidden_ranges`] from `folded`. Every write to `folded`
    /// must call this. Headers are visited in ascending order and any header
    /// already inside the span being built is skipped: an inner fold's range is
    /// contained by its outer's, so it can add nothing.
    fn rebuild_hidden_ranges(&mut self) {
        // Hidden lines take no wrap rows, so the cached totals change too.
        self.wrap_total_cache.clear();
        #[cfg(test)]
        FOLD_RANGE_REBUILDS.with(|c| c.set(c.get() + 1));
        let mut out: Vec<(usize, usize)> = Vec::new();
        for &h in &self.folded {
            if let Some(&(_, end)) = out.last()
                && h <= end
            {
                continue;
            }
            if let Some(span) = self.fold_range(h) {
                out.push(span);
            }
        }
        self.hidden_ranges = out;
    }

    /// Drop every fold that touches rows `start..=end`, before an edit that
    /// reorders those rows without changing the line count. Folds are line
    /// numbers and are only reset when the count changes, so after Move Line
    /// or Sort Lines a fold kept hiding the same numbers over different text.
    fn forget_folds_in(&mut self, start: usize, end: usize) {
        if self.folded.is_empty() {
            return;
        }
        let touched: Vec<usize> = self
            .folded
            .iter()
            .copied()
            .filter(|&h| {
                (start..=end).contains(&h)
                    || self
                        .fold_range(h)
                        .is_some_and(|(s, e)| s <= end && e >= start)
            })
            .collect();
        if touched.is_empty() {
            return;
        }
        for h in touched {
            self.folded.remove(&h);
        }
        self.rebuild_hidden_ranges();
    }

    /// Unfold whatever collapsed region the cursor is sitting inside. Movement
    /// is not the only way in — search, go-to-definition and goto-line all set
    /// `cursor_row` directly — and a caret on a hidden line cannot be painted
    /// at all (`cursor_screen_pos` has no row for it), so an edit there would
    /// change content the user cannot see. Cheap when nothing is folded.
    pub fn reveal_cursor_fold(&mut self) {
        if self.folded.is_empty() {
            return;
        }
        let row = self.cursor_row;
        let covering: Vec<usize> = self
            .folded
            .iter()
            .copied()
            .filter(|&h| matches!(self.fold_range(h), Some((_, end)) if h < row && row <= end))
            .collect();
        if covering.is_empty() {
            // Nothing to reveal, so the spans are still current. `pin_on_edit`
            // calls this on every mutation: rebuilding here would rescan the
            // whole buffer once per keystroke for every folded header, which
            // after Fold All is the expensive case this cache exists to avoid.
            return;
        }
        for h in covering {
            self.folded.remove(&h);
        }
        self.rebuild_hidden_ranges();
    }

    /// Next visible line at or after `line`, walking out of any collapsed
    /// region. `None` past the end of the buffer.
    fn next_visible_line(&self, line: usize) -> Option<usize> {
        let mut l = line;
        while l < self.lines.len() {
            if !self.is_line_hidden(l) {
                return Some(l);
            }
            l += 1;
        }
        None
    }

    /// Previous visible line at or before `line`.
    fn prev_visible_line(&self, line: usize) -> usize {
        let mut l = line;
        while l > 0 && self.is_line_hidden(l) {
            l -= 1;
        }
        l
    }

    /// Toggle the fold owning `line` (VS Code `editor.toggleFold`). Collapsing a
    /// block that contains the cursor snaps the cursor up to the header so it
    /// never strands on a hidden line.
    pub fn toggle_fold(&mut self, line: usize) {
        self.refresh_fold_tables();
        let Some(header) = self.enclosing_fold_header(line) else {
            return;
        };
        let collapsing = !self.folded.remove(&header);
        if collapsing {
            self.folded.insert(header);
        }
        // Both branches changed the fold set, so both must refresh the spans.
        self.rebuild_hidden_ranges();
        if collapsing && self.is_line_hidden(self.cursor_row) {
            let len = self
                .lines
                .get(header)
                .map(|s| s.chars().count())
                .unwrap_or(0);
            self.cursor_row = header;
            self.cursor_col = self.cursor_col.min(len);
        }
        self.fold_epoch_lines = self.lines.len();
    }

    /// Collapse every foldable region (VS Code "Fold All", Cmd+K Cmd+0),
    /// snapping the cursor up to the nearest visible line.
    pub fn fold_all(&mut self) {
        self.refresh_fold_tables();
        self.folded = (0..self.lines.len())
            .filter(|&l| self.is_foldable(l))
            .collect();
        self.fold_epoch_lines = self.lines.len();
        self.rebuild_hidden_ranges();
        while self.cursor_row > 0 && self.is_line_hidden(self.cursor_row) {
            self.cursor_row -= 1;
        }
    }

    /// Every foldable header with its range's end and its nesting level, in
    /// line order (#1653): level 1 is outermost, and a header inside another
    /// header's range is one level deeper.
    fn fold_headers_with_levels(&self) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        // The ends of the ranges enclosing the line being scanned.
        let mut open: Vec<usize> = Vec::new();
        for header in 0..self.lines.len() {
            let Some((_, end)) = self.fold_range(header) else {
                continue;
            };
            while open.last().is_some_and(|&e| e < header) {
                open.pop();
            }
            out.push((header, end, open.len() + 1));
            open.push(end);
        }
        out
    }

    /// Collapse every fold exactly `level` deep and leave the others as they
    /// are (VS Code "Fold Level N", Cmd+K Cmd+N, #1653): a deeper fold is
    /// hidden inside its collapsed parent anyway, and keeping its own state
    /// means opening the parent again shows it as it was. Fold
    /// Level 2 on a module keeps its classes and their methods' headers in
    /// view with the bodies collapsed, the overview Fold All cannot give
    /// since it collapses the outermost block too. The cursor snaps up to the
    /// nearest visible line, as for Fold All.
    pub fn fold_level(&mut self, level: usize) {
        self.refresh_fold_tables();
        let headers: Vec<usize> = self
            .fold_headers_with_levels()
            .into_iter()
            .filter(|&(_, _, l)| l == level)
            .map(|(h, _, _)| h)
            .collect();
        if headers.is_empty() {
            return;
        }
        self.folded.extend(headers);
        self.fold_epoch_lines = self.lines.len();
        self.rebuild_hidden_ranges();
        while self.cursor_row > 0 && self.is_line_hidden(self.cursor_row) {
            self.cursor_row -= 1;
        }
        let len = self.line_char_len(self.cursor_row);
        self.cursor_col = self.cursor_col.min(len);
    }

    /// Collapse the fold owning `line` and every fold inside it (VS Code
    /// "Fold Recursively", #1653), so unfolding it later shows its inner
    /// blocks collapsed. The cursor snaps to the header when it was inside.
    pub fn fold_recursively(&mut self, line: usize) {
        self.refresh_fold_tables();
        let Some(header) = self.enclosing_fold_header(line) else {
            return;
        };
        let Some((start, end)) = self.fold_range(header) else {
            return;
        };
        let inner: Vec<usize> = (start..=end).filter(|&l| self.is_foldable(l)).collect();
        self.folded.extend(inner);
        self.fold_epoch_lines = self.lines.len();
        self.rebuild_hidden_ranges();
        if self.is_line_hidden(self.cursor_row) {
            self.cursor_row = header;
            let len = self.line_char_len(header);
            self.cursor_col = self.cursor_col.min(len);
        }
    }

    /// Expand the fold owning `line` and every fold inside it (VS Code
    /// "Unfold Recursively", #1653). Folds outside its range stay as they
    /// are.
    pub fn unfold_recursively(&mut self, line: usize) {
        self.refresh_fold_tables();
        let Some(header) = self.enclosing_fold_header(line) else {
            return;
        };
        let Some((start, end)) = self.fold_range(header) else {
            return;
        };
        let inner: Vec<usize> = self
            .folded
            .iter()
            .copied()
            .filter(|h| (start..=end).contains(h))
            .collect();
        if inner.is_empty() {
            return;
        }
        for h in inner {
            self.folded.remove(&h);
        }
        self.rebuild_hidden_ranges();
    }

    /// Expand every fold (VS Code "Unfold All", Cmd+K Cmd+J).
    pub fn unfold_all(&mut self) {
        self.folded.clear();
        self.hidden_ranges.clear();
    }

    /// Mouse-down: position the cursor at the click point and start a
    /// fresh zero-area selection there. A subsequent drag widens it.
    pub fn mouse_down(&mut self, col: u16, row: u16) {
        self.click(col, row);
        self.start_selection_at_cursor();
    }

    /// Mouse-drag: move the cursor to the drag point and extend the selection
    /// head to the new cursor.  Anchors at the current cursor if no prior
    /// selection exists.
    pub fn mouse_drag(&mut self, col: u16, row: u16) {
        if self.selection.is_none() {
            self.start_selection_at_cursor();
        }
        self.click(col, row);
        self.extend_selection_to_cursor();
    }

    pub fn move_up(&mut self) {
        if self.wrap_enabled() {
            self.wrap_move_vertical(-1);
        } else if self.cursor_row > 0 {
            // Step over a collapsed region, as `move_down` does.
            self.cursor_row = self.prev_visible_line(self.cursor_row - 1);
            self.cursor_col = floor_grapheme_col(&self.lines[self.cursor_row], self.cursor_col);
        }
        self.last_edit_kind = None;
    }

    pub fn move_down(&mut self) {
        if self.wrap_enabled() {
            self.wrap_move_vertical(1);
        } else if self.cursor_row + 1 < self.lines.len() {
            // Step OVER a collapsed region rather than into it, the way VS Code
            // does: the fold stays shut and the caret lands on the next line
            // the user can actually see.
            if let Some(next) = self.next_visible_line(self.cursor_row + 1) {
                self.cursor_row = next;
                self.cursor_col = floor_grapheme_col(&self.lines[self.cursor_row], self.cursor_col);
            }
        }
        self.last_edit_kind = None;
    }

    /// The Up arrow: [`Self::move_up`], and on the first line (or wrapped
    /// row) the caret goes to the start of the line, as in VS Code, so
    /// Shift+Up selects to the start of the file (#1563). Vim's `k` keeps
    /// `move_up`, which stops on the first line as Vim does.
    pub fn arrow_up(&mut self) {
        let before = (self.cursor_row, self.cursor_col);
        self.move_up();
        if (self.cursor_row, self.cursor_col) == before {
            self.cursor_col = 0;
            self.ensure_cursor_col_visible();
        }
    }

    /// The Down arrow: [`Self::move_down`], and on the last line (or
    /// wrapped row) the caret goes to the end of the line, so Shift+Down
    /// selects through it instead of stopping at its start (#1563).
    pub fn arrow_down(&mut self) {
        let before = (self.cursor_row, self.cursor_col);
        self.move_down();
        if (self.cursor_row, self.cursor_col) == before {
            self.cursor_col = self.line_char_len(self.cursor_row);
            self.ensure_cursor_col_visible();
        }
    }

    pub fn goto_top(&mut self) {
        self.clear_selection();
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    pub fn goto_bottom(&mut self) {
        self.clear_selection();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    /// `Ctrl+Home` (`Cmd+Home` on macOS), VS Code's `cursorTop`. Unlike
    /// `goto_top` it keeps the selection, so `Shift` extends it.
    pub fn cursor_top(&mut self) {
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    /// `Ctrl+End`, VS Code's `cursorBottom`: after the last line's last
    /// character, keeping the selection like `cursor_top`.
    pub fn cursor_bottom(&mut self) {
        self.cursor_row = self.lines.len().saturating_sub(1);
        self.cursor_col = self.line_char_len(self.cursor_row);
        self.last_edit_kind = None;
    }

    /// Copy the caret + scroll position from another editor onto this one,
    /// clamped to this buffer's bounds. Used when the editor is split so
    /// the duplicate opens exactly where the source was - including the
    /// wrap sub-line offset (`scroll_sub`), which is private to this
    /// module and can't be set from `App`.
    pub fn copy_view_position_from(&mut self, src: &Editor) {
        let max_row = self.lines.len().saturating_sub(1);
        self.cursor_row = src.cursor_row.min(max_row);
        let max_col = self
            .lines
            .get(self.cursor_row)
            .map_or(0, |l| l.chars().count());
        self.cursor_col = src.cursor_col.min(max_col);
        self.scroll = src.scroll.min(max_row);
        self.scroll_sub = src.scroll_sub;
        self.scroll_col = src.scroll_col;
    }

    pub fn goto_line(&mut self, one_based: usize) {
        self.clear_selection();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let max = self.lines.len() - 1;
        let target = one_based.saturating_sub(1).min(max);
        self.cursor_row = target;
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    pub fn yank_lines(&self, count: usize) -> String {
        let n = count.max(1);
        let start = self.cursor_row;
        self.lines_slice_text(start, n)
    }

    pub fn delete_lines(&mut self, count: usize) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.push_undo(EditKind::DeleteLines);
        self.clear_selection();
        let n = count.max(1);
        // On a symbol tab the lines deleted stay inside the symbol: a count
        // stops at its last line, and a caret above it starts at its first.
        let (first, limit) = self.symbol_clip().unwrap_or((0, self.lines.len()));
        let start = self.cursor_row.max(first);
        let end = (start + n).min(limit).max(start + 1).min(self.lines.len());
        let yanked = self.lines_slice_text(start, end - start);
        self.lines.drain(start..end);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = start.min(self.lines.len() - 1);
        self.cursor_col = 0;
        self.mark_buffer_changed();
        self.recompute_highlights();
        yanked
    }

    /// Delete every line touched by the primary cursor or selection and by
    /// each secondary caret, yanking the removed text. VS Code's
    /// `editor.action.deleteLines` works across all cursors, and `Cmd+D`
    /// exists to make them: deleting only the primary's line would strand the
    /// others, still painted on rows that moved out from under them.
    pub fn delete_caret_lines(&mut self) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.push_undo(EditKind::DeleteLines);
        let last = self.lines.len() - 1;
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        // Mark while collecting, then scan once: the rows come out sorted and
        // deduped without a sort, and Change All Occurrences (Cmd+F2) can put
        // a caret on every match in the file.
        let mut doomed = vec![false; self.lines.len()];
        for sel in std::iter::once(primary).chain(self.carets.iter().copied()) {
            let (a, h) = sel.normalised();
            for slot in &mut doomed[a.0.min(last)..=h.0.min(last)] {
                *slot = true;
            }
        }
        let rows: Vec<usize> = doomed
            .iter()
            .enumerate()
            .filter_map(|(r, &drop)| drop.then_some(r))
            .collect();
        let yanked = rows
            .iter()
            .map(|&r| self.lines[r].as_str())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        // One retain pass, not a `remove` per row: removing tens of thousands
        // of rows one at a time is quadratic — a multi-second freeze on a large
        // file, on the render thread.
        let mut i = 0;
        self.lines.retain(|_| {
            let keep = !doomed[i];
            i += 1;
            keep
        });
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.selection = None;
        self.carets.clear();
        self.cursor_row = rows[0].min(self.lines.len() - 1);
        self.cursor_col = 0;
        self.mark_buffer_changed();
        self.recompute_highlights();
        yanked
    }

    pub fn kill_to_bol(&mut self) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.clear_selection();
        let row = self.cursor_row;
        if self.cursor_col == 0 {
            return String::new();
        }
        self.push_undo(EditKind::KillToBol);
        let from = self.byte_index(row, 0);
        let to = self.byte_index(row, self.cursor_col);
        let killed = self.lines[row][from..to].to_string();
        self.lines[row].replace_range(from..to, "");
        self.cursor_col = 0;
        self.mark_buffer_changed();
        self.recompute_highlights();
        killed
    }

    pub fn open_line_below(&mut self) {
        self.pin_on_edit();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.push_undo(EditKind::OpenLine);
        self.clear_selection();
        let row = self.cursor_row;
        let indent: String = self.lines[row]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        self.lines.insert(row + 1, indent.clone());
        self.cursor_row = row + 1;
        self.cursor_col = indent.chars().count();
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    pub fn open_line_above(&mut self) {
        self.pin_on_edit();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.push_undo(EditKind::OpenLine);
        self.clear_selection();
        let row = self.cursor_row;
        let indent: String = self.lines[row]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        self.lines.insert(row, indent.clone());
        self.cursor_col = indent.chars().count();
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    pub fn kill_to_eol(&mut self) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.clear_selection();
        let row = self.cursor_row;
        let line_len = self.line_char_len(row);
        if self.cursor_col >= line_len {
            return String::new();
        }
        self.push_undo(EditKind::KillToEol);
        let from = self.byte_index(row, self.cursor_col);
        let to = self.byte_index(row, line_len);
        let killed = self.lines[row][from..to].to_string();
        self.lines[row].replace_range(from..to, "");
        self.mark_buffer_changed();
        self.recompute_highlights();
        killed
    }

    /// Splice `text` over the `len_chars` characters starting at
    /// `(row, col_chars)` — the find bar's Replace. One undo step; the
    /// cursor lands at the end of the inserted text so replace-and-advance
    /// continues from there.
    pub fn replace_find_match(
        &mut self,
        row: usize,
        col_chars: usize,
        len_chars: usize,
        text: &str,
    ) {
        if row >= self.lines.len() {
            return;
        }
        let line_len = self.line_char_len(row);
        if col_chars.saturating_add(len_chars) > line_len {
            return;
        }
        self.pin_on_edit();
        self.push_undo(EditKind::Replace);
        self.clear_selection();
        let from = self.byte_index(row, col_chars);
        let to = self.byte_index(row, col_chars + len_chars);
        let text = &normalize_newlines(text);
        let breaks = text.matches('\n').count();
        if breaks == 0 {
            self.lines[row].replace_range(from..to, text);
            self.cursor_row = row;
            self.cursor_col = col_chars + text.chars().count();
        } else {
            // A replacement newline (VS Code's regex `\n`) becomes a real
            // line break, never an embedded control char in one line.
            let line = std::mem::take(&mut self.lines[row]);
            let merged = format!("{}{}{}", &line[..from], text, &line[to..]);
            self.lines
                .splice(row..=row, merged.split('\n').map(str::to_string));
            self.cursor_row = row + breaks;
            self.cursor_col = text
                .rsplit('\n')
                .next()
                .map_or(0, |tail| tail.chars().count());
        }
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    /// Swap in a whole new buffer as one undo step — the find bar's
    /// Replace All. The cursor clamps to the new buffer's bounds.
    pub fn replace_all_lines(&mut self, new_lines: Vec<String>) {
        self.pin_on_edit();
        self.push_undo(EditKind::Replace);
        self.clear_selection();
        self.lines = new_lines;
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = self.cursor_row.min(self.lines.len() - 1);
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    /// Take a sibling's text, replacing only the lines that differ so a
    /// caret or selection below the change moves with its line rather than
    /// staying on a row number that now holds different text.
    ///
    /// `source_step` is the sibling's [`Self::undo_step_id`]: edits it made
    /// within one of its undo steps (a typing burst) fold into one step
    /// here, so undo in this tab walks back as far as it would there.
    /// `source_caret` is where the sibling's caret sits after the edit.
    /// Returns whether the text changed.
    pub fn mirror_lines_from(
        &mut self,
        new_lines: &[String],
        source_step: u64,
        source_caret: (usize, usize),
    ) -> bool {
        if self.lines == new_lines || new_lines.is_empty() {
            return false;
        }
        let prefix = self
            .lines
            .iter()
            .zip(new_lines)
            .take_while(|(a, b)| a == b)
            .count();
        let room = self.lines.len().min(new_lines.len()) - prefix;
        let suffix = self
            .lines
            .iter()
            .rev()
            .zip(new_lines.iter().rev())
            .take(room)
            .take_while(|(a, b)| a == b)
            .count();
        let old_end = self.lines.len() - suffix;
        let new_end = new_lines.len() - suffix;
        self.pin_on_edit();
        if self.mirrored_step == Some(source_step) {
            self.seed_bookmark_shadow();
            self.redo_stack.clear();
        } else {
            self.push_undo(EditKind::Replace);
            self.mirrored_step = Some(source_step);
        }
        // A selection clear of the changed lines survives, moving with its
        // lines; one touching them is cleared.
        let shift = |row: usize| row + new_end - old_end;
        if let Some(sel) = self.selection.as_mut() {
            let (lo, hi) = (sel.anchor.0.min(sel.head.0), sel.anchor.0.max(sel.head.0));
            if lo >= old_end {
                sel.anchor.0 = shift(sel.anchor.0);
                sel.head.0 = shift(sel.head.0);
            } else if hi >= prefix {
                self.selection = None;
            }
        }
        // Extra carets follow the same rule as the selection.
        self.carets.retain_mut(|c| {
            let (lo, hi) = (c.anchor.0.min(c.head.0), c.anchor.0.max(c.head.0));
            if lo >= old_end {
                c.anchor.0 = shift(c.anchor.0);
                c.head.0 = shift(c.head.0);
                true
            } else {
                hi < prefix
            }
        });
        self.mirror_caret = Some(source_caret);
        self.lines
            .splice(prefix..old_end, new_lines[prefix..new_end].iter().cloned());
        if self.cursor_row >= old_end {
            self.cursor_row = shift(self.cursor_row);
        }
        self.cursor_row = self.cursor_row.min(self.lines.len() - 1);
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        true
    }

    /// Start a fresh tab from a sibling's unsaved text, without an undo step:
    /// the tab never held the disk text as far as the user can tell, so
    /// undo must not walk back to it.
    pub fn seed_unsaved(&mut self, lines: Vec<String>, dirty: bool) {
        self.lines = lines;
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.mark_buffer_changed();
        self.dirty = dirty;
        self.recompute_highlights();
    }

    /// Record that this buffer matches the file on disk: a sibling holding
    /// the same text was just saved (or undone back to its saved state).
    pub fn mark_clean_like_sibling(&mut self) {
        self.dirty = false;
        self.mark_synced_with_disk();
    }

    fn lines_slice_text(&self, start: usize, count: usize) -> String {
        if count == 0 || start >= self.lines.len() {
            return String::new();
        }
        let end = (start + count).min(self.lines.len());
        let mut out = self.lines[start..end].join("\n");
        out.push('\n');
        out
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col = prev_grapheme_col(&self.lines[self.cursor_row], self.cursor_col);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.line_char_len(self.cursor_row);
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    pub fn move_right(&mut self) {
        if self.cursor_col < self.line_char_len(self.cursor_row) {
            self.cursor_col = next_grapheme_col(&self.lines[self.cursor_row], self.cursor_col);
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    /// Word-step right (Option+Right on macOS, Ctrl+Right elsewhere). Skips
    /// any non-word run starting at the cursor — across line boundaries —
    /// then skips the following word run on the line where it landed and
    /// stops. Word chars are ASCII alphanumerics + underscore; everything
    /// else (whitespace, punctuation, EOL) is non-word. Mirrors readline
    /// `forward-word` and VS Code's "Cursor Word Right" command.
    pub fn move_word_right(&mut self) {
        loop {
            let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
            while self.cursor_col < chars.len() && !word_char_at(&chars, self.cursor_col) {
                self.cursor_col += 1;
            }
            if self.cursor_col < chars.len() {
                break;
            }
            if self.cursor_row + 1 < self.lines.len() {
                self.cursor_row += 1;
                self.cursor_col = 0;
            } else {
                self.ensure_cursor_col_visible();
                self.last_edit_kind = None;
                return;
            }
        }
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        while self.cursor_col < chars.len() && word_char_at(&chars, self.cursor_col) {
            self.cursor_col += 1;
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    /// VS Code's "Copy Line Down" (Shift+Option+Down on macOS). Duplicates
    /// the current line — or, when a selection is active, every line the
    /// selection touches as one block — directly below the original, then
    /// moves the cursor (and the selection, if any) onto the duplicate so
    /// the next keystroke acts on the copy. With several carets every
    /// caret's lines are copied (#1467); carets on touching lines share one
    /// block. Recorded as a single undo step via the dedicated
    /// `EditKind::DuplicateLines`.
    pub fn duplicate_lines_down(&mut self) {
        self.push_undo(EditKind::DuplicateLines);
        let blocks = self.selected_row_blocks_over_folds();
        for &(start, end) in blocks.iter().rev() {
            let block: Vec<String> = self.lines[start..=end].to_vec();
            self.lines.splice(end + 1..end + 1, block);
        }
        // A caret moves down by every block copied at or above it, its own
        // block's copy included.
        self.remap_carets(|(r, c)| (r + rows_in_blocks(&blocks, |b| b.0 <= r), c));
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Copy Line Up" (Shift+Option+Up on macOS). Mirror of
    /// `duplicate_lines_down`: inserts the copy *above* the original
    /// block, leaving the cursor on the upper copy at the same row index
    /// it started at (the original is pushed down by `block_len`).
    pub fn duplicate_lines_up(&mut self) {
        self.push_undo(EditKind::DuplicateLines);
        let blocks = self.selected_row_blocks_over_folds();
        for &(start, end) in blocks.iter().rev() {
            let block: Vec<String> = self.lines[start..=end].to_vec();
            self.lines.splice(start..start, block);
        }
        // Only the blocks wholly above a caret push it down: its own copy
        // goes in above it, onto the row it already has.
        self.remap_carets(|(r, c)| (r + rows_in_blocks(&blocks, |b| b.1 < r), c));
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    fn selected_or_cursor_row_range(&self) -> (usize, usize) {
        match &self.selection {
            Some(sel) => {
                let (start, end) = sel.normalised();
                (start.0, end.0)
            }
            None => (self.cursor_row, self.cursor_row),
        }
    }

    /// The primary selection (a bare caret when there is none) followed by
    /// every extra caret.
    fn selections_with_primary(&self) -> Vec<EditorSelection> {
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        std::iter::once(primary)
            .chain(self.carets.iter().copied())
            .collect()
    }

    /// The rows every selection or caret touches, as sorted `(first, last)`
    /// blocks. Blocks that overlap or touch are one block, so a line
    /// command never edits a row twice or moves one block into another.
    fn selected_row_blocks(&self) -> Vec<(usize, usize)> {
        let last = self.lines.len().saturating_sub(1);
        let rows = self
            .selections_with_primary()
            .iter()
            .map(|s| {
                let (a, b) = s.normalised();
                (a.0.min(last), b.0.min(last))
            })
            .collect();
        merge_row_blocks(rows, true)
    }

    /// [`Self::selected_row_blocks`], with a block that ends on a folded
    /// header widened over the lines the fold hides, so Move Line and Copy
    /// Line take a folded region whole instead of its header alone (#1611).
    fn selected_row_blocks_over_folds(&self) -> Vec<(usize, usize)> {
        let rows = self
            .selected_row_blocks()
            .into_iter()
            .map(|(start, end)| (start, self.folded_end(end).unwrap_or(end)))
            .collect();
        merge_row_blocks(rows, true)
    }

    /// The last line `row`'s fold hides, when `row` is a folded header.
    fn folded_end(&self, row: usize) -> Option<usize> {
        self.hidden_ranges
            .iter()
            .find(|&&(header, _)| header == row)
            .map(|&(_, end)| end)
    }

    /// The header of the fold hiding `row`, when a fold hides it.
    fn folded_header_hiding(&self, row: usize) -> Option<usize> {
        self.hidden_ranges
            .iter()
            .find(|&&(header, end)| header < row && row <= end)
            .map(|&(header, _)| header)
    }

    /// Carry the folds through a line move that sent each row `r` to
    /// `map(r)`: a moved fold stays folded over the same text. `before` is
    /// the fold set ahead of the move. It replaces what the edit's own fold
    /// sync made of the move, which follows a header by its text and so
    /// loses one whose text repeats. The server's spans move too, since a
    /// move keeps the line count they are checked against; one the move
    /// split apart is dropped until the next reply.
    fn move_folds(
        &mut self,
        before: std::collections::BTreeSet<usize>,
        map: impl Fn(usize) -> usize,
    ) {
        self.folded = before.into_iter().map(&map).collect();
        if let Some(spans) = self.lsp_folds.as_mut() {
            for span in spans.iter_mut() {
                span.start_line = map(span.start_line);
                span.end_line = map(span.end_line);
            }
            spans.retain(|span| span.start_line < span.end_line);
        }
        // The marker and comment table is rebuilt from the moved text.
        self.refresh_fold_tables();
        self.rebuild_hidden_ranges();
    }

    /// Move the primary caret and selection and every extra caret through
    /// `f`, clamp each onto its line, and merge carets that landed together.
    fn remap_carets(&mut self, f: impl Fn((usize, usize)) -> (usize, usize)) {
        let last = self.lines.len().saturating_sub(1);
        let clamp = |this: &Self, p: (usize, usize)| {
            let (r, c) = f(p);
            let r = r.min(last);
            (r, c.min(this.line_char_len(r)))
        };
        let map = |this: &Self, s: EditorSelection| EditorSelection {
            anchor: clamp(this, s.anchor),
            head: clamp(this, s.head),
        };
        self.selection = self.selection.map(|s| map(self, s));
        (self.cursor_row, self.cursor_col) = clamp(self, (self.cursor_row, self.cursor_col));
        self.carets = self.carets.iter().map(|&s| map(self, s)).collect();
        self.dedup_carets();
    }

    /// Make `all[0]` the primary selection (or bare caret) and the rest
    /// extra carets.
    fn set_selections(&mut self, all: &[EditorSelection]) {
        let Some((&primary, rest)) = all.split_first() else {
            return;
        };
        self.selection = primary.has_area().then_some(primary);
        (self.cursor_row, self.cursor_col) = primary.head;
        self.carets = rest.to_vec();
        self.dedup_carets();
    }

    /// Drop extra carets that sit on the primary or on an earlier caret, and
    /// merge selections that overlap into one, as VS Code does (#1570):
    /// kept apart, the next key would edit the same spot twice, and the
    /// edit at the lower one left the upper one's end stale, so typing over
    /// two overlapping line selections panicked or deleted lines nobody
    /// selected. Selections that only touch stay apart, as does a caret at a
    /// selection's edge. A merged selection keeps the place in the order and
    /// the direction of its first member, so the primary stays the primary.
    fn dedup_carets(&mut self) {
        let all = self.selections_with_primary();
        let mut by_start: Vec<usize> = (0..all.len()).collect();
        by_start.sort_by_key(|&i| all[i].normalised());
        // Each run of overlapping selections: (first member, start, end).
        let mut runs: Vec<(usize, BPos, BPos)> = Vec::with_capacity(all.len());
        for i in by_start {
            let (start, end) = all[i].normalised();
            match runs.last_mut() {
                Some(run) if start < run.2 || (start, end) == (run.1, run.2) => {
                    run.0 = run.0.min(i);
                    run.2 = run.2.max(end);
                }
                _ => runs.push((i, start, end)),
            }
        }
        runs.sort_unstable_by_key(|run| run.0);
        let mut merged = runs.into_iter().map(|(i, start, end)| {
            let (anchor, head) = if all[i].anchor <= all[i].head {
                (start, end)
            } else {
                (end, start)
            };
            EditorSelection { anchor, head }
        });
        let primary = merged.next().expect("the primary is always first");
        if primary != all[0] {
            self.selection = primary.has_area().then_some(primary);
            (self.cursor_row, self.cursor_col) = primary.head;
        }
        self.carets = merged.collect();
    }

    /// A position kept as (rows from the last line, chars from its line's
    /// end). Edits above it, or left of it on its line, leave both true, so
    /// a command that edits from the bottom of the buffer up pins each
    /// caret as it passes and finds it again at the end.
    fn pin_from_end(&self, (row, col): (usize, usize)) -> (usize, usize) {
        let last = self.lines.len().saturating_sub(1);
        (
            last.saturating_sub(row),
            self.line_char_len(row).saturating_sub(col),
        )
    }

    /// The position `pin_from_end` kept, in the buffer as it is now.
    fn unpin_from_end(&self, (from_bottom, from_end): (usize, usize)) -> (usize, usize) {
        let row = self
            .lines
            .len()
            .saturating_sub(1)
            .saturating_sub(from_bottom);
        (row, self.line_char_len(row).saturating_sub(from_end))
    }

    /// Char-based slice of the buffer between two `(row, char_col)` points
    /// (`start <= end`), joining spanned rows with `\n`.
    fn char_range_text(&self, start: (usize, usize), end: (usize, usize)) -> String {
        if start.0 == end.0 {
            return self.lines[start.0]
                .chars()
                .skip(start.1)
                .take(end.1.saturating_sub(start.1))
                .collect();
        }
        let mut out: String = self.lines[start.0].chars().skip(start.1).collect();
        for line in &self.lines[start.0 + 1..end.0] {
            out.push('\n');
            out.push_str(line);
        }
        out.push('\n');
        let last: String = self.lines[end.0].chars().take(end.1).collect();
        out.push_str(&last);
        out
    }

    /// Replace the char-range `start..end` with `new`, re-splitting on `\n`
    /// so a replacement that adds or removes newlines reshapes `lines`.
    fn replace_char_range(&mut self, start: (usize, usize), end: (usize, usize), new: &str) {
        let prefix: String = self.lines[start.0].chars().take(start.1).collect();
        let suffix: String = self.lines[end.0].chars().skip(end.1).collect();
        let combined = format!("{prefix}{new}{suffix}");
        let replacement: Vec<String> = combined.split('\n').map(str::to_string).collect();
        self.lines.splice(start.0..=end.0, replacement);
    }

    /// VS Code "Move Line Down" (Alt+Down): swap the current line / selected
    /// block with the line below it, carrying the cursor and selection along.
    /// With several carets every caret's block moves. No-op when a block
    /// already touches the last line: all of them move or none does.
    ///
    /// A folded region moves as one line and stays folded (#1611): a block
    /// on a folded header takes the hidden lines with it, and a block above
    /// a folded header moves below its whole region, never into its body.
    pub fn move_lines_down(&mut self) {
        let blocks = self.selected_row_blocks_over_folds();
        // What each block swaps with: the line below it, or all of a folded
        // region that starts there.
        let below: Vec<(usize, usize)> = blocks
            .iter()
            .map(|&(_, end)| (end + 1, self.folded_end(end + 1).unwrap_or(end + 1)))
            .collect();
        // A symbol tab's block stops at the symbol's last line.
        let limit = self.symbol_clip().map_or(self.lines.len(), |(_, e)| e);
        if below.last().is_none_or(|&(_, last)| last >= limit)
            || below
                .iter()
                .zip(blocks.iter().skip(1))
                .any(|(b, next)| b.1 >= next.0)
        {
            return;
        }
        self.push_undo(EditKind::MoveLines);
        let folds = self.folded.clone();
        for (&(start, end), &(_, last)) in blocks.iter().zip(&below).rev() {
            self.lines[start..=last].rotate_right(last - end);
        }
        let map = |r: usize| {
            for (&(start, end), &(first, last)) in blocks.iter().zip(&below) {
                if (start..=end).contains(&r) {
                    return r + (last - end);
                }
                // The lines below a block swap up over it.
                if (first..=last).contains(&r) {
                    return r - (end - start + 1);
                }
            }
            r
        };
        self.remap_carets(|(r, c)| (map(r), c));
        self.mark_buffer_changed();
        self.move_folds(folds, map);
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Move Line Up" (Alt+Up): mirror of `move_lines_down`. No-op
    /// when a block already touches the first line. A block below a folded
    /// region moves above all of it (#1611).
    pub fn move_lines_up(&mut self) {
        let blocks = self.selected_row_blocks_over_folds();
        let first = self.symbol_clip().map_or(0, |(first, _)| first);
        if blocks.first().is_none_or(|&(start, _)| start <= first) {
            return;
        }
        // What each block swaps with: the line above it, or all of the
        // folded region whose hidden lines end there.
        let above: Vec<(usize, usize)> = blocks
            .iter()
            .map(|&(start, _)| {
                let row = start - 1;
                (self.folded_header_hiding(row).unwrap_or(row), row)
            })
            .collect();
        if above.first().is_some_and(|&(top, _)| top < first)
            || above
                .iter()
                .skip(1)
                .zip(&blocks)
                .any(|(a, prev)| a.0 <= prev.1)
        {
            return;
        }
        self.push_undo(EditKind::MoveLines);
        let folds = self.folded.clone();
        for (&(start, end), &(top, _)) in blocks.iter().zip(&above) {
            self.lines[top..=end].rotate_left(start - top);
        }
        let map = |r: usize| {
            for (&(start, end), &(top, bottom)) in blocks.iter().zip(&above) {
                if (start..=end).contains(&r) {
                    return r - (start - top);
                }
                // The lines above a block swap down under it.
                if (top..=bottom).contains(&r) {
                    return r + (end - start + 1);
                }
            }
            r
        };
        self.remap_carets(|(r, c)| (map(r), c));
        self.mark_buffer_changed();
        self.move_folds(folds, map);
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Toggle Line Comment" (Cmd+/): comment or uncomment every line
    /// the cursor, the selection or any extra caret touches using the
    /// language's line-comment marker. Comments are inserted at each block's
    /// common (minimum) indentation so the markers align; the lines
    /// uncomment only when every non-blank one is already commented. Each
    /// caret moves with the text on its line, so a selection still covers
    /// what it did. Returns false (a no-op) for languages with no line
    /// comment.
    pub fn toggle_line_comment(&mut self) -> bool {
        let Some(token) = self.line_comment_marker() else {
            // VS Code's fallback for a language with only block comments
            // (HTML, CSS, Markdown, XML): wrap the lines in one (#1181).
            return match block_comment_tokens(self.lang) {
                Some((open, close)) => self.toggle_lines_in_block_comment(open, close),
                None => false,
            };
        };
        let blocks = self.selected_row_blocks();
        let non_blank: Vec<usize> = blocks
            .iter()
            .flat_map(|&(start, end)| start..=end)
            .filter(|&r| !self.lines[r].trim().is_empty())
            .collect();
        if non_blank.is_empty() {
            return false;
        }
        self.push_undo(EditKind::ToggleComment);
        // Rows in JSX children take `{/* … */}`: there `//` is text (#1623).
        let jsx: std::collections::HashSet<usize> = self.jsx_child_rows(&non_blank);
        let all_commented = non_blank.iter().all(|&r| {
            if jsx.contains(&r) {
                is_jsx_comment(self.lines[r].trim())
            } else {
                line_comment_prefix_len(self.lines[r].trim_start(), token).is_some()
            }
        });
        // Each changed row with the char column of its edit and the chars it
        // added (negative: removed), to carry the carets on it along.
        let mut edits: Vec<(usize, usize, isize)> = Vec::with_capacity(non_blank.len());
        if all_commented {
            for &r in &non_blank {
                let line = self.lines[r].clone();
                let off = line.len() - line.trim_start().len();
                if jsx.contains(&r) {
                    let core = line[off..].trim_end();
                    let inner = &core[JSX_COMMENT_OPEN.len()..core.len() - JSX_COMMENT_CLOSE.len()];
                    let lead_space = usize::from(inner.starts_with(' '));
                    let inner = &inner[lead_space..];
                    let inner = inner.strip_suffix(' ').unwrap_or(inner);
                    self.lines[r] = format!("{}{inner}", &line[..off]);
                    let removed = JSX_COMMENT_OPEN.chars().count() + lead_space;
                    edits.push((r, line[..off].chars().count(), -(removed as isize)));
                    continue;
                }
                let mut rest = line[off..].to_string();
                let len = line_comment_prefix_len(&rest, token).unwrap_or(0);
                let mut removed = rest[..len].chars().count();
                rest.drain(..len);
                if rest.starts_with(' ') {
                    rest.drain(..1);
                    removed += 1;
                }
                self.lines[r] = format!("{}{}", &line[..off], rest);
                edits.push((r, line[..off].chars().count(), -(removed as isize)));
            }
        } else {
            let marker = format!("{token} ");
            for &(start, end) in &blocks {
                let rows: Vec<usize> = non_blank
                    .iter()
                    .copied()
                    .filter(|r| (start..=end).contains(r))
                    .collect();
                let Some(min_indent) = rows
                    .iter()
                    .map(|&r| {
                        let l = &self.lines[r];
                        l.len() - l.trim_start().len()
                    })
                    .min()
                else {
                    continue;
                };
                for &r in &rows {
                    if jsx.contains(&r) {
                        // Already `{/* … */}`: wrapping it again would nest
                        // the comment, and its `*/}` would end the outer one.
                        if is_jsx_comment(self.lines[r].trim()) {
                            continue;
                        }
                        let end = self.lines[r].trim_end().len();
                        self.lines[r].truncate(end);
                        self.lines[r].push_str(&format!(" {JSX_COMMENT_CLOSE}"));
                        let open = format!("{JSX_COMMENT_OPEN} ");
                        self.lines[r].insert_str(min_indent, &open);
                        let at = self.lines[r][..min_indent].chars().count();
                        edits.push((r, at, open.chars().count() as isize));
                        continue;
                    }
                    self.lines[r].insert_str(min_indent, &marker);
                    let at = self.lines[r][..min_indent].chars().count();
                    edits.push((r, at, marker.chars().count() as isize));
                }
            }
        }
        self.remap_carets(|(r, c)| match edits.iter().find(|e| e.0 == r) {
            Some(&(_, at, delta)) => (r, shift_col(c, at, delta)),
            None => (r, c),
        });
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// This buffer's line-comment marker: its language's, or for a file
    /// croft has no grammar for, the one its name or extension uses
    /// (#1181).
    fn line_comment_marker(&self) -> Option<&'static str> {
        match self.lang {
            // SCSS and Sass share the CSS grammar but, unlike CSS, have a
            // `//` line comment, so the file's extension decides.
            Some(LangKind::Css) | None => {
                self.path.as_deref().and_then(line_comment_token_for_path)
            }
            Some(_) => line_comment_token(self.lang),
        }
    }

    /// Whether Toggle Line Comment has any comment to use in this buffer: a
    /// line comment, or a block comment to wrap the lines in.
    pub fn has_line_comment_syntax(&self) -> bool {
        self.line_comment_marker().is_some() || block_comment_tokens(self.lang).is_some()
    }

    /// Toggle Line Comment where the language has only block comments: each
    /// block of lines is wrapped in one `open … close` pair, from its first
    /// non-blank character to its last, or unwrapped when every block
    /// already is one, as VS Code does (#1181).
    fn toggle_lines_in_block_comment(&mut self, open: &str, close: &str) -> bool {
        // Each block's first and last non-blank rows.
        let spans: Vec<(usize, usize)> = self
            .selected_row_blocks()
            .into_iter()
            .filter_map(|(start, end)| {
                let blank = |r: &usize| self.lines[*r].trim().is_empty();
                let first = (start..=end).find(|r| !blank(r))?;
                let last = (start..=end).rev().find(|r| !blank(r))?;
                Some((first, last))
            })
            .collect();
        if spans.is_empty() {
            return false;
        }
        let commented = spans.iter().all(|&(first, last)| {
            let (head, tail) = (self.lines[first].trim_start(), self.lines[last].trim_end());
            head.starts_with(open)
                && tail.ends_with(close)
                && (first != last || head.len() >= open.len() + close.len())
        });
        self.push_undo(EditKind::ToggleComment);
        // Each edit as (row, char column, chars added or, negative, removed),
        // in the order made: a row's closing edit comes before its opening
        // one, so each column is still true when its edit is applied.
        let mut edits: Vec<(usize, usize, isize)> = Vec::with_capacity(spans.len() * 2);
        for &(first, last) in &spans {
            let line = &self.lines[last];
            let end = line.trim_end().len();
            if commented {
                let mut cut = end - close.len();
                if line[..cut].ends_with(' ') {
                    cut -= 1;
                }
                let at = line[..cut].chars().count();
                let removed = line[cut..end].chars().count();
                self.lines[last].replace_range(cut..end, "");
                edits.push((last, at, -(removed as isize)));
            } else {
                let at = line[..end].chars().count();
                let marker = format!(" {close}");
                self.lines[last].insert_str(end, &marker);
                edits.push((last, at, marker.chars().count() as isize));
            }
            let line = &self.lines[first];
            let indent = line.len() - line.trim_start().len();
            let at = line[..indent].chars().count();
            if commented {
                let mut cut = indent + open.len();
                if line[cut..].starts_with(' ') {
                    cut += 1;
                }
                let removed = line[indent..cut].chars().count();
                self.lines[first].replace_range(indent..cut, "");
                edits.push((first, at, -(removed as isize)));
            } else {
                let marker = format!("{open} ");
                self.lines[first].insert_str(indent, &marker);
                edits.push((first, at, marker.chars().count() as isize));
            }
        }
        self.remap_carets(|(r, c)| {
            let c = edits
                .iter()
                .filter(|e| e.0 == r)
                .fold(c, |c, &(_, at, delta)| shift_col(c, at, delta));
            (r, c)
        });
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// VS Code "Toggle Block Comment" (Shift+Alt+A): wrap the selection (or,
    /// with no selection, the current line) in the language's block-comment
    /// delimiters, or strip them when the selection already is a block
    /// comment. Every extra caret's selection or line is done the same way,
    /// each on its own. Returns false for languages with no block comment
    /// (e.g. YAML).
    pub fn toggle_block_comment(&mut self) -> bool {
        let Some(tokens) = block_comment_tokens(self.lang) else {
            return false;
        };
        let last = self.lines.len().saturating_sub(1);
        let mut ranges: Vec<(BPos, BPos, bool)> = self
            .selections_with_primary()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let (start, end) = if s.has_area() {
                    s.normalised()
                } else {
                    let row = s.head.0.min(last);
                    ((row, 0), (row, self.line_char_len(row)))
                };
                (start, end, i == 0)
            })
            .collect();
        // Two carets on one line comment it once.
        ranges.sort_unstable();
        let mut merged: Vec<(BPos, BPos, bool)> = Vec::new();
        for (start, end, primary) in ranges {
            match merged.last_mut() {
                Some(m) if start < m.1 || start == m.0 => {
                    m.1 = m.1.max(end);
                    m.2 |= primary;
                }
                _ => merged.push((start, end, primary)),
            }
        }
        if merged
            .iter()
            .all(|&(start, end, _)| self.char_range_text(start, end).trim().is_empty())
        {
            return false;
        }
        self.push_undo(EditKind::ToggleComment);
        // A range starting in JSX children is wrapped `{/* … */}`, since a
        // bare `/* */` there is text (#1623). Read before any edit.
        let jsx_starts: Vec<(usize, usize)> = merged
            .iter()
            .map(|&(start, end, _)| {
                let text = self.char_range_text(start, end);
                let lead = text
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .collect::<String>();
                text_end(start, &lead)
            })
            .collect();
        let standalone: Vec<bool> = merged
            .iter()
            .map(|&(start, end, _)| is_jsx_comment(self.char_range_text(start, end).trim()))
            .collect();
        let jsx = self.jsx_children_at(&jsx_starts, &standalone);
        // From the bottom up, so an edit never moves a range still to come.
        let mut placed: Vec<(bool, (usize, usize))> = Vec::with_capacity(merged.len());
        for (i, &(start, end, primary)) in merged.iter().enumerate().rev() {
            let (open, close) = if jsx[i] {
                (JSX_COMMENT_OPEN, JSX_COMMENT_CLOSE)
            } else {
                tokens
            };
            let text = self.char_range_text(start, end);
            let trimmed = text.trim();
            let caret = if trimmed.is_empty() {
                end
            } else {
                let wrapped = trimmed.starts_with(open)
                    && trimmed.ends_with(close)
                    && trimmed.len() >= open.len() + close.len();
                let new = if wrapped {
                    trimmed[open.len()..trimmed.len() - close.len()]
                        .trim()
                        .to_string()
                } else {
                    format!("{open} {trimmed} {close}")
                };
                // Only the trimmed core is replaced: the whitespace around it
                // (the line's indent, and the line break a whole-line
                // selection ends with) stays as it was. Replacing the whole
                // range joined the next line into this one and dropped the
                // indent.
                let lead = &text[..text.len() - text.trim_start().len()];
                let trail = &text[text.trim_end().len()..];
                let before = format!("{lead}{new}");
                self.replace_char_range(start, end, &format!("{before}{trail}"));
                // The caret lands just after the (un)commented text.
                text_end(start, &before)
            };
            placed.push((primary, self.pin_from_end(caret)));
        }
        self.place_pinned_carets(&placed);
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// The rows among `rows` whose first non-blank character sits in JSX
    /// children (#1623).
    fn jsx_child_rows(&self, rows: &[usize]) -> std::collections::HashSet<usize> {
        let points: Vec<(usize, usize)> = rows
            .iter()
            .map(|&r| {
                let line = &self.lines[r];
                (r, line.chars().take_while(|c| c.is_whitespace()).count())
            })
            .collect();
        let standalone: Vec<bool> = rows
            .iter()
            .map(|&r| is_jsx_comment(self.lines[r].trim()))
            .collect();
        rows.iter()
            .zip(self.jsx_children_at(&points, &standalone))
            .filter_map(|(&r, jsx)| jsx.then_some(r))
            .collect()
    }

    /// Whether each of `points` (char positions) starts something in JSX
    /// children, an element, a closing tag, text or a `{…}`, where a
    /// comment must be written `{/* … */}` (#1623). Always false outside
    /// the JSX-capable grammars, and on a line of plain code or attributes.
    ///
    /// Where `standalone[i]` says the text at `points[i]` is one whole
    /// `{/* … */}`, that point also counts when it sits in JSX or in a parse
    /// error: commenting out a closing tag leaves the markup unparseable, and
    /// a second toggle must still take the comment off. In well-formed plain
    /// code such a line is a block holding a comment, and stays code.
    fn jsx_children_at(&self, points: &[(usize, usize)], standalone: &[bool]) -> Vec<bool> {
        let none = vec![false; points.len()];
        if !matches!(self.lang, Some(LangKind::JavaScript) | Some(LangKind::Tsx)) {
            return none;
        }
        let Some(lang) = self.lang.map(crate::highlight::language_for) else {
            return none;
        };
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&lang).is_err() {
            return none;
        }
        let Some(tree) = parser.parse(self.lines.join("\n"), None) else {
            return none;
        };
        points
            .iter()
            .zip(standalone)
            .map(|(&p, &standalone)| {
                let byte = self.char_pos_to_byte(p);
                jsx_child_starts_at(tree.root_node(), byte)
                    || standalone && in_jsx_or_error(tree.root_node(), byte)
            })
            .collect()
    }

    /// Put a bare caret at each pinned position, the one marked primary as
    /// the primary caret.
    fn place_pinned_carets(&mut self, placed: &[(bool, (usize, usize))]) {
        let mut all: Vec<EditorSelection> = placed
            .iter()
            .map(|&(_, pin)| {
                let (r, c) = self.unpin_from_end(pin);
                EditorSelection::new(r, c)
            })
            .collect();
        if let Some(i) = placed.iter().position(|&(primary, _)| primary) {
            all.swap(0, i);
        }
        self.set_selections(&all);
    }

    /// VS Code "Join Lines": collapse the selected lines (or the current line
    /// with the one below it) into one, trimming each joined line's leading
    /// whitespace and separating with a single space. Every extra caret
    /// joins its own lines too. No-op when no caret has a following line to
    /// join.
    pub fn join_lines(&mut self) {
        // A symbol tab's last line has nothing below it to join.
        let limit = self.symbol_clip().map_or(self.lines.len(), |(_, e)| e);
        // (first row, last row, primary, caret) for each selection.
        let mut items: Vec<(usize, usize, bool, (usize, usize))> = self
            .selections_with_primary()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let (a, b) = s.normalised();
                let last = if a.0 == b.0 { a.0 + 1 } else { b.0 };
                (a.0, last, i == 0, s.head)
            })
            .collect();
        items.sort_unstable();
        let mut merged: Vec<(usize, usize, bool, (usize, usize))> = Vec::new();
        for (start, last, primary, head) in items {
            match merged.last_mut() {
                Some(m) if start <= m.1 => {
                    m.1 = m.1.max(last);
                    m.2 |= primary;
                }
                _ => merged.push((start, last, primary, head)),
            }
        }
        if merged.iter().all(|&(_, last, _, _)| last >= limit) {
            return;
        }
        self.push_undo(EditKind::JoinLines);
        let mut placed: Vec<(bool, (usize, usize))> = Vec::with_capacity(merged.len());
        for &(start, last, primary, head) in merged.iter().rev() {
            // A caret with no line below it stays where it is.
            let caret = if last >= limit {
                head
            } else {
                let mut result = self.lines[start].trim_end().to_string();
                let cursor_col = result.chars().count();
                for line in &self.lines[start + 1..=last] {
                    let piece = line.trim_start();
                    if !result.is_empty()
                        && !result.ends_with(char::is_whitespace)
                        && !piece.is_empty()
                    {
                        result.push(' ');
                    }
                    result.push_str(piece);
                }
                self.lines.splice(start..=last, std::iter::once(result));
                (start, cursor_col)
            };
            placed.push((primary, self.pin_from_end(caret)));
        }
        self.place_pinned_carets(&placed);
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Transform to Upper/Lower/Title Case": rewrite the selection
    /// (or, with no selection, the word under the cursor) in the requested
    /// case, and the same for every extra caret, keeping each selection.
    /// No-op when there is nothing to transform.
    pub fn transform_selection_case(&mut self, kind: CaseTransform) {
        let sels = self.selections_with_primary();
        let ranges: Vec<Option<(BPos, BPos)>> = sels
            .iter()
            .map(|s| {
                if s.has_area() {
                    Some(s.normalised())
                } else if matches!(
                    kind,
                    CaseTransform::Snake
                        | CaseTransform::Camel
                        | CaseTransform::Pascal
                        | CaseTransform::Kebab
                ) {
                    // Naming styles take the whole identifier run, hyphens
                    // included, so `user-name` converts as one identifier.
                    self.identifier_run_at(s.head.0, s.head.1)
                        .map(|(a, b)| ((s.head.0, a), (s.head.0, b)))
                } else {
                    self.word_at(s.head.0, s.head.1)
                        .map(|(a, b)| ((s.head.0, a), (s.head.0, b)))
                }
            })
            .collect();
        if ranges
            .iter()
            .flatten()
            .all(|&(start, end)| self.char_range_text(start, end).is_empty())
        {
            return;
        }
        self.push_undo(EditKind::TransformCase);
        // Overlapping ranges (two carets in one word, nested selections)
        // merge into one span, so each character is rewritten exactly once.
        let mut by_start: Vec<usize> = (0..sels.len()).filter(|&i| ranges[i].is_some()).collect();
        by_start.sort_by_key(|&i| ranges[i].map(|r| r.0));
        let mut spans: Vec<(BPos, BPos, Vec<usize>)> = Vec::new();
        for i in by_start {
            let Some((start, end)) = ranges[i] else {
                continue;
            };
            match spans.last_mut() {
                Some(span) if start < span.1 || start == span.0 => {
                    span.1 = span.1.max(end);
                    span.2.push(i);
                }
                _ => spans.push((start, end, vec![i])),
            }
        }
        // From the bottom up, so an edit never moves a span still to come.
        // A caret with no word is a span with nothing to rewrite.
        for i in 0..sels.len() {
            if ranges[i].is_none() {
                spans.push((sels[i].head, sels[i].head, vec![i]));
            }
        }
        spans.sort_by_key(|span| std::cmp::Reverse(span.0));
        let mut pinned = vec![((0, 0), (0, 0)); sels.len()];
        for (start, end, members) in spans {
            let text = self.char_range_text(start, end);
            // Each endpoint as a char offset into the span, so it lands on
            // the same character once the span is rewritten.
            let offset = |e: &Self, pos: BPos| {
                e.char_range_text(start, pos.max(start).min(end))
                    .chars()
                    .count()
            };
            let offsets: Vec<(usize, usize)> = members
                .iter()
                .map(|&i| (offset(self, sels[i].anchor), offset(self, sels[i].head)))
                .collect();
            let new = match kind {
                CaseTransform::Upper => text.to_uppercase(),
                CaseTransform::Lower => text.to_lowercase(),
                CaseTransform::Title => title_case(&text),
                CaseTransform::Snake
                | CaseTransform::Camel
                | CaseTransform::Pascal
                | CaseTransform::Kebab => identifier_case(&text, kind),
            };
            if !text.is_empty() {
                self.replace_char_range(start, end, &new);
            }
            let at = |k: usize| text_end(start, &new.chars().take(k).collect::<String>());
            for (&i, &(anchor, head)) in members.iter().zip(&offsets) {
                let after = if text.is_empty() {
                    sels[i]
                } else {
                    EditorSelection {
                        anchor: at(anchor),
                        head: at(head),
                    }
                };
                pinned[i] = (
                    self.pin_from_end(after.anchor),
                    self.pin_from_end(after.head),
                );
            }
        }
        let all: Vec<EditorSelection> = pinned
            .iter()
            .map(|&(anchor, head)| EditorSelection {
                anchor: self.unpin_from_end(anchor),
                head: self.unpin_from_end(head),
            })
            .collect();
        self.set_selections(&all);
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Sort Lines Ascending/Descending": sort the selected lines
    /// lexicographically, or the whole buffer when nothing is selected. With
    /// several carets each selection's lines are sorted on their own. No-op
    /// on a single line.
    pub fn sort_lines(&mut self, ascending: bool) {
        let blocks = if self.carets.is_empty() {
            vec![match &self.selection {
                Some(sel) => {
                    let (s, e) = sel.normalised();
                    (s.0, e.0)
                }
                None => (0, self.lines.len().saturating_sub(1)),
            }]
        } else {
            let rows = self
                .selections_with_primary()
                .iter()
                .map(|s| {
                    let (a, b) = s.normalised();
                    (a.0, b.0)
                })
                .collect();
            merge_row_blocks(rows, false)
        };
        let blocks: Vec<(usize, usize)> = blocks.into_iter().filter(|&(s, e)| s < e).collect();
        if blocks.is_empty() {
            return;
        }
        self.push_undo(EditKind::SortLines);
        for &(start, end) in &blocks {
            let mut block: Vec<String> = self.lines[start..=end].to_vec();
            block.sort();
            if !ascending {
                block.reverse();
            }
            self.lines.splice(start..=end, block);
            self.forget_folds_in(start, end);
        }
        // The rows changed under the carets: a column past the new line's
        // end made the next word motion index out of bounds.
        self.remap_carets(|p| p);
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    /// VS Code "Trim Trailing Whitespace": strip trailing spaces and tabs from
    /// every line, clamping the cursor onto the new line end. Returns false
    /// (a no-op) when the buffer is already clean.
    pub fn trim_trailing_whitespace(&mut self) -> bool {
        let any = self
            .lines
            .iter()
            .any(|l| l.trim_end_matches([' ', '\t']).len() != l.len());
        if !any {
            return false;
        }
        self.push_undo(EditKind::TrimWhitespace);
        self.carets.clear();
        for line in &mut self.lines {
            let trimmed_len = line.trim_end_matches([' ', '\t']).len();
            line.truncate(trimmed_len);
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        true
    }

    /// The character at `(row, char_col)`, or None when out of range. Used by
    /// the bracket-matching helpers, which work in char indices.
    fn char_at(&self, row: usize, col: usize) -> Option<char> {
        self.lines.get(row)?.chars().nth(col)
    }

    /// Which string or comment each bracket lies in (#1634), from the last
    /// highlight pass. `None` while the pass is stale or the buffer has no
    /// grammar: every bracket then counts, as before.
    fn bracket_zones(&self) -> Option<BracketZones<'_>> {
        if !self.protected_current || self.protected_ranges.is_empty() {
            return None;
        }
        let mut starts = Vec::with_capacity(self.lines.len());
        let mut at = 0;
        for line in &self.lines {
            starts.push(at);
            at += line.len() + 1;
        }
        Some(BracketZones {
            lines: &self.lines,
            starts,
            ranges: &self.protected_ranges,
        })
    }

    /// Forward-scan from the opening bracket at `open` to its matching close,
    /// counting nesting of the SAME pair type. Returns the `(row, col)` of the
    /// closing bracket, or None when unbalanced. Brackets in strings and
    /// comments are text (#1634): they are passed over, and one of them as
    /// `open` matches nothing, even a balanced `"()"`.
    fn match_bracket_forward(&self, open: BPos, zones: Option<&BracketZones>) -> Option<BPos> {
        let oc = self.char_at(open.0, open.1)?;
        let cc = matching_close(oc)?;
        if zones.is_some_and(|z| z.zone(open).is_some()) {
            return None;
        }
        let mut depth = 0i32;
        // One pass along each line. `char_at` walks a line from its start,
        // so stepping a scan with it was quadratic in the line's length: a
        // minified line's opening bracket froze the first frame (#959).
        for (row, line) in self.lines.iter().enumerate().skip(open.0) {
            let from = if row == open.0 { open.1 } else { 0 };
            for (col, (byte, ch)) in line.char_indices().enumerate().skip(from) {
                if (ch != oc && ch != cc) || zones.is_some_and(|z| z.zone_at(row, byte).is_some()) {
                    continue;
                }
                if ch == oc {
                    depth += 1;
                } else {
                    depth -= 1;
                    if depth == 0 {
                        return Some((row, col));
                    }
                }
            }
        }
        None
    }

    /// Backward-scan from the closing bracket at `close` to its matching open,
    /// passing over brackets in strings and comments, as
    /// [`Self::match_bracket_forward`] does.
    fn match_bracket_backward(&self, close: BPos, zones: Option<&BracketZones>) -> Option<BPos> {
        let cc = self.char_at(close.0, close.1)?;
        let oc = matching_open(cc)?;
        if zones.is_some_and(|z| z.zone(close).is_some()) {
            return None;
        }
        let mut depth = 0i32;
        for row in (0..=close.0).rev() {
            let end = (row == close.0).then_some(close.1 + 1);
            for (col, byte, ch) in chars_back(&self.lines[row], end) {
                if (ch != oc && ch != cc) || zones.is_some_and(|z| z.zone_at(row, byte).is_some()) {
                    continue;
                }
                if ch == cc {
                    depth += 1;
                } else {
                    depth -= 1;
                    if depth == 0 {
                        return Some((row, col));
                    }
                }
            }
        }
        None
    }

    /// The innermost bracket pair enclosing the cursor, found by walking
    /// backwards for the first unmatched opening bracket and matching it
    /// forwards. Mirrors VS Code's `findEnclosingBrackets`. Brackets in
    /// strings and comments are passed over, so from inside a string this
    /// finds the pair in code around it.
    fn enclosing_brackets(&self, zones: Option<&BracketZones>) -> Option<(BPos, BPos)> {
        let mut expected: Vec<char> = Vec::new();
        for row in (0..=self.cursor_row).rev() {
            let end = (row == self.cursor_row).then_some(self.cursor_col);
            for (col, byte, ch) in chars_back(self.lines.get(row)?, end) {
                let pos = (row, col);
                let in_code = || zones.is_none_or(|z| z.zone_at(row, byte).is_none());
                if is_close_bracket(ch) && in_code() {
                    expected.push(matching_open(ch)?);
                } else if is_open_bracket(ch) && in_code() {
                    match expected.last() {
                        Some(&want) if want == ch => {
                            expected.pop();
                        }
                        _ => {
                            let close = self.match_bracket_forward(pos, zones)?;
                            return Some((pos, close));
                        }
                    }
                }
            }
        }
        None
    }

    /// The first bracket pair at or after the cursor (`findNextBracket`),
    /// passing over brackets in strings and comments.
    fn next_bracket_pair(&self, zones: Option<&BracketZones>) -> Option<(BPos, BPos)> {
        for (row, line) in self.lines.iter().enumerate().skip(self.cursor_row) {
            let from = if row == self.cursor_row {
                self.cursor_col
            } else {
                0
            };
            for (col, (byte, ch)) in line.char_indices().enumerate().skip(from) {
                let in_code = zones.is_none_or(|z| z.zone_at(row, byte).is_none());
                if is_open_bracket(ch) && in_code {
                    let open = (row, col);
                    return self.match_bracket_forward(open, zones).map(|c| (open, c));
                }
                if is_close_bracket(ch) && in_code {
                    let close = (row, col);
                    return self
                        .match_bracket_backward(close, zones)
                        .map(|o| (o, close));
                }
            }
        }
        None
    }

    /// Resolve the bracket pair relevant to the cursor, returning
    /// `(open, close, anchored_on_close)`. `anchored_on_close` is true when the
    /// cursor sits on the closing bracket, so a jump should target the open
    /// bracket instead. Adjacency priority mirrors VS Code's `matchBracket`:
    /// a close to the left wins, then an open to the right, then an open to the
    /// left, then a close to the right; failing all four it falls back to the
    /// enclosing pair, then the next bracket forward.
    fn find_bracket_pair(&self) -> Option<(BPos, BPos, bool)> {
        let zones = self.bracket_zones();
        if let Some(pair) = self.adjacent_bracket_pair(zones.as_ref()) {
            return Some(pair);
        }
        if let Some((open, close)) = self.enclosing_brackets(zones.as_ref()) {
            return Some((open, close, false));
        }
        self.next_bracket_pair(zones.as_ref())
            .map(|(o, c)| (o, c, false))
    }

    /// The bracket pair whose bracket sits immediately to the left or right of
    /// the caret, as `(open, close, anchored_on_close)`. `None` when the caret
    /// touches no bracket. Adjacency priority mirrors VS Code's `matchBracket`:
    /// a close to the left wins, then an open to the right, then an open to the
    /// left, then a close to the right. A bracket in a string or comment
    /// pairs with nothing.
    fn adjacent_bracket_pair(&self, zones: Option<&BracketZones>) -> Option<(BPos, BPos, bool)> {
        let row = self.cursor_row;
        let col = self.cursor_col;
        let left = col.checked_sub(1).and_then(|c| self.char_at(row, c));
        let right = self.char_at(row, col);
        if left.is_some_and(is_close_bracket) {
            let close = (row, col - 1);
            return self
                .match_bracket_backward(close, zones)
                .map(|o| (o, close, true));
        }
        if right.is_some_and(is_open_bracket) {
            let open = (row, col);
            return self
                .match_bracket_forward(open, zones)
                .map(|c| (open, c, false));
        }
        if left.is_some_and(is_open_bracket) {
            let open = (row, col - 1);
            return self
                .match_bracket_forward(open, zones)
                .map(|c| (open, c, false));
        }
        if right.is_some_and(is_close_bracket) {
            let close = (row, col);
            return self
                .match_bracket_backward(close, zones)
                .map(|o| (o, close, true));
        }
        None
    }

    /// The bracket pair to highlight (VS Code's `editorBracketMatch`): the
    /// bracket adjacent to the caret and its match, as `(open, close)`. Unlike
    /// [`Self::find_bracket_pair`] it never falls back to the enclosing or next
    /// pair, so the highlight appears only when the caret is beside a bracket.
    pub fn bracket_match_pair(&self) -> Option<(BPos, BPos)> {
        let zones = self.bracket_zones();
        self.adjacent_bracket_pair(zones.as_ref())
            .map(|(o, c, _)| (o, c))
    }

    /// [`Self::bracket_match_pair`], worked out once per edit, highlight
    /// pass and caret place. Every frame asks, and a partner far along a
    /// long line costs a scan of that line, which kept a core busy while
    /// croft sat idle on a minified file (#959). A pass can move the strings
    /// and comments without an edit (Change Language Mode), so it counts.
    fn cached_bracket_match_pair(&mut self) -> Option<(BPos, BPos)> {
        let key = (
            self.edit_seq,
            self.highlight_generation,
            (self.cursor_row, self.cursor_col),
            self.protected_current,
        );
        if let Some((at, pair)) = self.bracket_pair_cache
            && at == key
        {
            return pair;
        }
        let pair = self.bracket_match_pair();
        self.bracket_pair_cache = Some((key, pair));
        pair
    }

    /// VS Code "Go to Bracket" (`editor.action.jumpToBracket`, Cmd+Shift+\):
    /// move the cursor to the matching bracket. From an opening bracket it
    /// lands on the closing one and vice versa; from inside a pair it lands on
    /// the enclosing closing bracket. Returns false (a no-op) when no bracket
    /// is in play.
    pub fn jump_to_matching_bracket(&mut self) -> bool {
        let Some((open, close, anchored_on_close)) = self.find_bracket_pair() else {
            return false;
        };
        let target = if anchored_on_close { open } else { close };
        self.clear_selection();
        self.cursor_row = target.0;
        self.cursor_col = target.1;
        self.last_edit_kind = None;
        self.ensure_cursor_col_visible();
        true
    }

    /// VS Code "Select to Bracket" (`editor.action.selectToBracket`): select
    /// the region between the matching brackets, including the brackets
    /// themselves (from the opening bracket's start to the closing bracket's
    /// end). Returns false (a no-op) when no bracket pair is in play.
    pub fn select_to_matching_bracket(&mut self) -> bool {
        let Some((open, close, _)) = self.find_bracket_pair() else {
            return false;
        };
        let head = (close.0, close.1 + 1);
        self.selection = Some(EditorSelection { anchor: open, head });
        self.cursor_row = head.0;
        self.cursor_col = head.1;
        self.last_edit_kind = None;
        self.ensure_cursor_col_visible();
        true
    }

    /// vim's `Ctrl-A` / `Ctrl-X` (`delta` of `+1` / `-1`), which Zed and the
    /// VS Code increment extensions all copy: bump the number under the
    /// cursor. When the cursor is not inside a number, the next one to its
    /// right on the same line is used, so the caret rarely needs positioning
    /// precisely. With several carets every caret bumps its own number, in
    /// one undo step, and each caret ends on its number (#1637). Returns
    /// false when no caret's line holds a number at or after it, leaving the
    /// buffer and the undo stack untouched.
    ///
    /// Formatting is preserved rather than normalised, because the point is
    /// to edit a literal in place: zero padding keeps its width (`007` ->
    /// `008`), and a hex or binary literal keeps its prefix and digit case
    /// (`0xFF` -> `0x100`, `0Xff` -> `0X100`). Width only grows when the
    /// value needs the room.
    pub fn bump_number(&mut self, delta: i64) -> bool {
        // Every caret's number, read off the buffer before anything changes.
        // Carets that share a number bump it once, so two carets on `19`
        // give `20`, not `21`.
        let all = self.selections_with_primary();
        let mut owned: Vec<Option<(usize, usize)>> = Vec::with_capacity(all.len());
        let mut bumps: Vec<(usize, NumberToken, String)> = Vec::new();
        for s in &all {
            let (row, col) = s.head;
            let found = self.lines.get(row).and_then(|line| {
                let tok = number_token_at_or_after(line, self.byte_index(row, col))?;
                Some((tok, tok.bumped(line, delta)?))
            });
            let Some((tok, replacement)) = found else {
                owned.push(None);
                continue;
            };
            owned.push(Some((row, tok.start)));
            if !bumps
                .iter()
                .any(|(r, t, _)| *r == row && t.start == tok.start)
            {
                bumps.push((row, tok, replacement));
            }
        }
        if bumps.is_empty() {
            return false;
        }
        // A clamped bump (`0x00` down) finds a number but changes nothing;
        // recording it would dirty a clean buffer and leave an undo step
        // that undoes nothing.
        bumps.retain(|(row, tok, rep)| *rep != self.lines[*row][tok.start..tok.end]);
        if bumps.is_empty() {
            return true;
        }

        // A real edit from here on: pin a preview tab, or the next preview
        // open reuses it and the bump is lost with it.
        self.pin_on_edit();
        // One undo step for every caret's bump.
        self.push_undo(EditKind::BumpNumber);
        // Never coalesce: a run of bumps must undo one at a time, the way
        // holding Ctrl-A in vim does.
        self.last_edit_kind = None;

        // Where every caret lands, worked out on the text before the edits.
        // Literals are ASCII, so a bump's growth in bytes is its growth in
        // chars, and a point moves by what the bumps left of it on its row
        // added.
        let char_of =
            |this: &Self, row: usize, byte: usize| this.lines[row][..byte].chars().count();
        let growth_before = |this: &Self, row: usize, col: usize| -> isize {
            bumps
                .iter()
                .filter(|(r, t, _)| *r == row && char_of(this, row, t.end) <= col)
                .map(|(_, t, rep)| rep.len() as isize - (t.end - t.start) as isize)
                .sum()
        };
        let shift = |this: &Self, (row, col): (usize, usize)| {
            (
                row,
                col.saturating_add_signed(growth_before(this, row, col)),
            )
        };
        let moved: Vec<EditorSelection> = all
            .iter()
            .zip(&owned)
            .map(|(s, own)| {
                let bump = own.and_then(|(row, start)| {
                    bumps.iter().find(|(r, t, _)| *r == row && t.start == start)
                });
                match bump {
                    // vim leaves the caret on the number's last digit, which
                    // is what makes a repeated bump keep working on the same
                    // literal even as it grows a digit.
                    Some((row, tok, rep)) => {
                        let start = char_of(self, *row, tok.start);
                        let (_, start) = shift(self, (*row, start));
                        EditorSelection::new(*row, start + rep.len().saturating_sub(1))
                    }
                    None => EditorSelection {
                        anchor: shift(self, s.anchor),
                        head: shift(self, s.head),
                    },
                }
            })
            .collect();

        // Right to left along each row, so an edit never moves a literal
        // still waiting for its own.
        bumps.sort_by(|(ra, ta, _), (rb, tb, _)| (rb, tb.start).cmp(&(ra, ta.start)));
        for (row, tok, replacement) in &bumps {
            self.lines[*row].replace_range(tok.start..tok.end, replacement);
        }
        self.set_selections(&moved);
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// The Emmet dialect this buffer expands into, or `None` where the chord
    /// is not offered.
    fn emmet_profile(&self) -> Option<crate::emmet::Profile> {
        emmet_profile_for(self.lang, self.path.as_deref())
    }

    /// Whether Emmet expansion is offered in this buffer at all, so the
    /// caller can tell "wrong language" apart from "nothing to expand".
    pub fn emmet_available(&self) -> bool {
        self.emmet_profile().is_some()
    }

    /// VS Code "Emmet: Expand Abbreviation"
    /// (`editor.emmet.action.expandAbbreviation`): replace the abbreviation
    /// ending at the cursor with the markup it stands for, and leave the
    /// caret in the first empty element.
    ///
    /// Only runs in markup buffers, and only when what sits left of the
    /// cursor actually parses as an abbreviation — so the chord is inert in
    /// prose and in code rather than mangling either. Returns whether
    /// anything expanded, which the caller reports in the status bar.
    pub fn expand_emmet_abbreviation(&mut self) -> bool {
        let Some(profile) = self.emmet_profile() else {
            return false;
        };
        let row = self.cursor_row;
        let col = self.cursor_col;
        let line = match self.lines.get(row) {
            Some(l) => l.clone(),
            None => return false,
        };
        let (start_col, abbr) = match crate::emmet::abbreviation_before(&line, col) {
            Some(v) => v,
            None => return false,
        };
        let unit = self.indent_unit();
        let (markup, caret) = match crate::emmet::expand(&abbr, &unit, profile) {
            Some(v) => v,
            None => return false,
        };

        // The abbreviation's own indentation prefixes every line of the
        // expansion, so an abbreviation typed inside a nested block stays
        // inside it.
        let lead: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let lead = if start_col == lead.chars().count() {
            lead
        } else {
            String::new()
        };

        self.pin_on_edit();
        self.push_undo(EditKind::EmmetExpand);
        self.last_edit_kind = None;

        let chars: Vec<char> = line.chars().collect();
        let before: String = chars[..start_col].iter().collect();
        let after: String = chars[col.min(chars.len())..].iter().collect();

        // `markup` always ends in a newline; the trailing line merges with
        // whatever followed the abbreviation.
        let body = markup.strip_suffix('\n').unwrap_or(&markup);
        let mut out: Vec<String> = Vec::new();
        let mut caret_row = row;
        let mut caret_col = 0usize;
        let mut consumed = 0usize;
        for (i, seg) in body.split('\n').enumerate() {
            let mut l = String::new();
            if i == 0 {
                l.push_str(&before);
            } else {
                l.push_str(&lead);
            }
            let text_start = l.chars().count();
            l.push_str(seg);
            // `caret` is a byte offset into `markup`; find the line holding
            // it and convert to a char column on the rebuilt line.
            let seg_end = consumed + seg.len();
            if caret >= consumed && caret <= seg_end {
                caret_row = row + i;
                caret_col = text_start + seg[..caret - consumed].chars().count();
            }
            consumed = seg_end + 1; // the '\n' we split on
            out.push(l);
        }
        if let Some(last) = out.last_mut() {
            last.push_str(&after);
        }

        self.lines.splice(row..=row, out);
        self.cursor_row = caret_row;
        self.cursor_col = caret_col;
        self.clear_selection();
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// The buffer as one string, for the Emmet commands that edit markup
    /// already written (#1557), and `(row, col)` as a byte offset into it.
    fn emmet_text_and_offset(&self, row: usize, col: usize) -> (String, usize) {
        let text = self.lines.join("\n");
        let before: usize = self.lines[..row.min(self.lines.len())]
            .iter()
            .map(|l| l.len() + 1)
            .sum();
        let line = self.lines.get(row).map_or("", String::as_str);
        let in_line = line.char_indices().nth(col).map_or(line.len(), |(i, _)| i);
        (text, before + in_line)
    }

    /// Byte offset `at` of `text` as a (row, char column).
    fn emmet_position(text: &str, at: usize) -> (usize, usize) {
        let head = &text[..at.min(text.len())];
        let row = head.matches('\n').count();
        let start = head.rfind('\n').map_or(0, |i| i + 1);
        (row, head[start..].chars().count())
    }

    /// Make the buffer `text`, as one undo step, rewriting only the lines
    /// that changed.
    fn emmet_set_text(&mut self, text: &str) {
        let new: Vec<String> = text.split('\n').map(String::from).collect();
        let (head, tail) = {
            let old = &self.lines;
            let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
            let room = old.len().min(new.len()) - head;
            let tail = old
                .iter()
                .rev()
                .zip(new.iter().rev())
                .take(room)
                .take_while(|(a, b)| a == b)
                .count();
            (head, tail)
        };
        self.pin_on_edit();
        self.push_undo(EditKind::EmmetExpand);
        self.last_edit_kind = None;
        let old_end = self.lines.len() - tail;
        let added = new.len() - tail - head;
        self.lines
            .splice(head..old_end, new[head..new.len() - tail].iter().cloned());
        // Rows below the rewrite keep their seats; the rewritten rows are
        // markup croft generated.
        self.provenance.splice(head, old_end - head, added);
        self.provenance
            .record(head..head + added, crate::provenance::Seat::Generated);
        self.provenance.truncate(self.lines.len());
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    /// Emmet: Wrap with Abbreviation (#1557): wrap the selected lines in
    /// `abbr`, one level in, at their own indentation; a selection inside
    /// one line is wrapped where it stands. With nothing selected it wraps
    /// the element around the caret, as VS Code does, or the caret's line
    /// outside any element. Returns whether anything was wrapped.
    pub fn emmet_wrap(&mut self, abbr: &str) -> bool {
        use crate::emmet::WrapContent;
        let Some(profile) = self.emmet_profile() else {
            return false;
        };
        let unit = self.indent_unit();
        let sel = self
            .selection
            .filter(|s| s.has_area())
            .map(|s| s.normalised())
            .or_else(|| self.emmet_element_at_caret());
        if let Some(((r0, c0), (r1, c1))) = sel
            && r0 == r1
        {
            let chars: Vec<char> = self.lines[r0].chars().collect();
            let before: String = chars[..c0].iter().collect();
            let after: String = chars[c1.min(chars.len())..].iter().collect();
            if !before.trim().is_empty() || !after.trim().is_empty() {
                let picked: String = chars[c0..c1.min(chars.len())].iter().collect();
                let Some(markup) =
                    crate::emmet::wrap(abbr, WrapContent::Inline(&picked), &unit, profile)
                else {
                    return false;
                };
                let markup = markup.trim_end_matches('\n');
                let (text, at) = self.emmet_text_and_offset(r0, c0);
                let end = at + picked.len();
                let new = format!("{}{markup}{}", &text[..at], &text[end..]);
                self.emmet_set_text(&new);
                (self.cursor_row, self.cursor_col) = Self::emmet_position(&new, at);
                self.clear_selection();
                self.ensure_cursor_col_visible();
                return true;
            }
        }
        let (first, last) = match sel {
            // A selection ending at the start of a line leaves that line out.
            Some(((r0, _), (r1, 0))) if r1 > r0 => (r0, r1 - 1),
            Some(((r0, _), (r1, _))) => (r0, r1),
            None => (self.cursor_row, self.cursor_row),
        };
        let rows = &self.lines[first..=last.min(self.lines.len() - 1)];
        if rows.iter().all(|l| l.trim().is_empty()) {
            return false;
        }
        // The lines' shared indentation stays outside the new element.
        let base: String = rows
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                l.chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect::<String>()
            })
            .min_by_key(String::len)
            .unwrap_or_default();
        let content: Vec<String> = rows
            .iter()
            .map(|l| {
                l.strip_prefix(base.as_str())
                    .unwrap_or(l.trim_start())
                    .to_string()
            })
            .collect();
        let Some(markup) = crate::emmet::wrap(abbr, WrapContent::Lines(&content), &unit, profile)
        else {
            return false;
        };
        let wrapped: Vec<String> = markup
            .trim_end_matches('\n')
            .split('\n')
            .map(|l| {
                if l.is_empty() {
                    String::new()
                } else {
                    format!("{base}{l}")
                }
            })
            .collect();
        let mut lines = self.lines.clone();
        lines.splice(first..=last, wrapped);
        self.emmet_set_text(&lines.join("\n"));
        self.cursor_row = first;
        self.cursor_col = base.chars().count();
        self.clear_selection();
        self.ensure_cursor_col_visible();
        true
    }

    /// The span of the element around the caret, a caret just past its
    /// closing `>` included, for a wrap with nothing selected.
    fn emmet_element_at_caret(&self) -> Option<((usize, usize), (usize, usize))> {
        let (text, at) = self.emmet_text_and_offset(self.cursor_row, self.cursor_col);
        let element = crate::emmet::enclosing(&text, at)
            .or_else(|| crate::emmet::enclosing(&text, at.checked_sub(1)?))?;
        let span = element.outer();
        Some((
            Self::emmet_position(&text, span.start),
            Self::emmet_position(&text, span.end),
        ))
    }

    /// Emmet: Remove Tag (#1557): take away the tags of the element around
    /// the caret, keeping its content.
    pub fn emmet_remove_tag(&mut self) -> bool {
        if self.emmet_profile().is_none() {
            return false;
        }
        let unit = self.indent_unit();
        let (text, at) = self.emmet_text_and_offset(self.cursor_row, self.cursor_col);
        let Some((new, caret)) = crate::emmet::remove_tag(&text, at, &unit) else {
            return false;
        };
        self.emmet_set_text(&new);
        (self.cursor_row, self.cursor_col) = Self::emmet_position(&new, caret);
        self.clear_selection();
        self.ensure_cursor_col_visible();
        true
    }

    /// Emmet: Update Tag (#1557): rename the element around the caret.
    pub fn emmet_update_tag(&mut self, name: &str) -> bool {
        if self.emmet_profile().is_none() {
            return false;
        }
        let (text, at) = self.emmet_text_and_offset(self.cursor_row, self.cursor_col);
        let Some(new) = crate::emmet::rename_tag(&text, at, name) else {
            return false;
        };
        self.emmet_set_text(&new);
        true
    }

    /// Emmet: Go to Matching Pair (#1557): from an opening tag to its
    /// closing tag, and back.
    pub fn emmet_match_tag(&mut self) -> bool {
        if self.emmet_profile().is_none() {
            return false;
        }
        let (text, at) = self.emmet_text_and_offset(self.cursor_row, self.cursor_col);
        let Some(to) = crate::emmet::matching_tag(&text, at) else {
            return false;
        };
        (self.cursor_row, self.cursor_col) = Self::emmet_position(&text, to);
        self.clear_selection();
        self.ensure_cursor_col_visible();
        true
    }

    /// Emmet: Balance outward / inward (#1557): grow the selection to the
    /// enclosing element's content, then the element, and so on outward, or
    /// walk it back in.
    pub fn emmet_balance(&mut self, outward: bool) -> bool {
        if self.emmet_profile().is_none() {
            return false;
        }
        let ((r0, c0), (r1, c1)) = self.selection.map(|s| s.normalised()).unwrap_or((
            (self.cursor_row, self.cursor_col),
            (self.cursor_row, self.cursor_col),
        ));
        let (text, start) = self.emmet_text_and_offset(r0, c0);
        let end = self.emmet_text_and_offset(r1, c1).1;
        let range = if outward {
            crate::emmet::balance_out(&text, start..end)
        } else {
            crate::emmet::balance_in(&text, start..end)
        };
        let Some(range) = range else {
            return false;
        };
        let anchor = Self::emmet_position(&text, range.start);
        let head = Self::emmet_position(&text, range.end);
        self.selection = Some(EditorSelection { anchor, head });
        (self.cursor_row, self.cursor_col) = head;
        self.ensure_cursor_col_visible();
        true
    }

    /// VS Code "Transpose Characters around the Cursor"
    /// (`editor.action.transpose`): swap the character before the cursor with
    /// the one after it and advance the cursor (emacs `transpose-chars`). At
    /// the end of a non-final line the last character moves across the line
    /// break to the start of the next line. At the end of the final line it is
    /// a no-op. With several carets each one swaps around itself, in one undo
    /// step (#1637).
    pub fn transpose_chars(&mut self) {
        let all = self.selections_with_primary();
        let edits = all.iter().any(|s| {
            let (row, col) = s.head;
            if col >= self.line_char_len(row) {
                row + 1 < self.lines.len()
            } else {
                col > 0
            }
        });
        // One undo step for every caret's swap.
        if edits {
            self.push_undo(EditKind::Transpose);
        }
        // Bottom up and right to left: a swap only touches its own row's
        // chars at its caret, or moves a row's last char to the start of the
        // next, so every caret still waiting is where it was, and every caret
        // already done is pinned from its line's end.
        let mut order: Vec<usize> = (0..all.len()).collect();
        order.sort_by(|&a, &b| all[b].head.cmp(&all[a].head));
        // Two carets whose swaps share a character (`ab` with carets after
        // `a` and after `b`) would each act on text the other already moved:
        // the second of them, in this order, stays where it is instead.
        let mut taken: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
        let mut pinned = vec![(0, 0); all.len()];
        for i in order {
            let head = all[i].head;
            let cells = self.transpose_cells(head);
            if cells.iter().any(|c| taken.contains(c)) {
                pinned[i] = self.pin_from_end(head);
                continue;
            }
            taken.extend(cells);
            let at = self.transpose_at(head);
            pinned[i] = self.pin_from_end(at);
        }
        let moved: Vec<EditorSelection> = pinned
            .iter()
            .map(|&p| {
                let (row, col) = self.unpin_from_end(p);
                EditorSelection::new(row, col)
            })
            .collect();
        self.set_selections(&moved);
        if edits {
            self.mark_buffer_changed();
            self.recompute_highlights();
        }
        self.ensure_cursor_col_visible();
    }

    /// The `(row, char col)` cells one caret's Transpose Characters
    /// changes, in the buffer as it is: the two chars it swaps, or a line's
    /// last char and the line break it moves across (col = the line's
    /// length). Empty when it only steps the caret.
    fn transpose_cells(&self, (row, col): (usize, usize)) -> Vec<(usize, usize)> {
        let len = self.line_char_len(row);
        if col >= len {
            if row + 1 >= self.lines.len() || len == 0 {
                return Vec::new();
            }
            return vec![(row, len - 1), (row, len)];
        }
        if col == 0 {
            return Vec::new();
        }
        vec![(row, col - 1), (row, col)]
    }

    /// One caret's Transpose Characters, returning where that caret goes.
    fn transpose_at(&mut self, (row, col): (usize, usize)) -> (usize, usize) {
        let len = self.line_char_len(row);
        if col >= len {
            if row + 1 >= self.lines.len() {
                return (row, col);
            }
            if len == 0 {
                return (row + 1, 0);
            }
            let mut chars: Vec<char> = self.lines[row].chars().collect();
            let moved = chars.pop().unwrap();
            self.lines[row] = chars.into_iter().collect();
            self.lines[row + 1].insert(0, moved);
            return (row + 1, 1);
        }
        if col == 0 {
            // VS Code's single-character range at column 1: no swap, just step
            // the cursor right.
            return (row, 1);
        }
        let mut chars: Vec<char> = self.lines[row].chars().collect();
        chars.swap(col - 1, col);
        self.lines[row] = chars.into_iter().collect();
        (row, col + 1)
    }

    /// VS Code "Convert Indentation to Spaces" (`editor.action.indentationToSpaces`):
    /// replace every tab in each line's leading indentation with `tabSize`
    /// spaces. Only leading whitespace is touched.
    pub fn indentation_to_spaces(&mut self) {
        self.convert_indentation(true);
    }

    /// VS Code "Convert Indentation to Tabs" (`editor.action.indentationToTabs`):
    /// replace each run of `tabSize` spaces in the leading indentation with a
    /// tab, leaving any remainder. Only leading whitespace is touched.
    pub fn indentation_to_tabs(&mut self) {
        self.convert_indentation(false);
    }

    fn convert_indentation(&mut self, to_spaces: bool) {
        let tab_w = (self.indent_style().width as usize).max(1);
        let spaces = " ".repeat(tab_w);
        let mut new_lines = self.lines.clone();
        let mut changed = false;
        for line in new_lines.iter_mut() {
            let indent_len = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            let indent: String = line.chars().take(indent_len).collect();
            let converted = if to_spaces {
                indent.replace('\t', &spaces)
            } else {
                indent.replace(&spaces, "\t")
            };
            if converted != indent {
                let rest: String = line.chars().skip(indent_len).collect();
                *line = format!("{converted}{rest}");
                changed = true;
            }
        }
        if !changed {
            return;
        }
        self.push_undo(EditKind::IndentConvert);
        self.lines = new_lines;
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code's `files.trimFinalNewlines`, surfaced as a command: drop trailing
    /// empty lines at the end of the buffer, always keeping at least one line.
    /// Returns false (a no-op) when there are no trailing blank lines.
    pub fn trim_final_newlines(&mut self) -> bool {
        let mut end = self.lines.len();
        while end > 1 && self.lines[end - 1].is_empty() {
            end -= 1;
        }
        if end == self.lines.len() {
            return false;
        }
        self.push_undo(EditKind::TrimFinalNewlines);
        self.lines.truncate(end);
        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len() - 1;
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        true
    }

    /// VS Code "Add Cursor Below" (Cmd+Alt+Down): drop a secondary caret on
    /// the row below the lowest current caret, at the same column (clamped to
    /// that line's length). No-op at the last row.
    pub fn add_cursor_below(&mut self) {
        self.multi_typing = None;
        let max_row = self
            .carets
            .iter()
            .map(|c| c.head.0)
            .chain(std::iter::once(self.cursor_row))
            .max()
            .unwrap_or(self.cursor_row);
        let target = max_row + 1;
        if target >= self.symbol_clip().map_or(self.lines.len(), |(_, end)| end) {
            return;
        }
        let col = self.cursor_col.min(self.line_char_len(target));
        self.carets.push(EditorSelection {
            anchor: (target, col),
            head: (target, col),
        });
    }

    /// VS Code Alt+click: add a caret at the screen cell `(col, row)`, keeping
    /// the existing cursor(s). The old primary (with any selection) becomes a
    /// caret and the clicked point becomes the new primary. Clicking an
    /// existing zero-width caret removes it (toggle). Returns false when the
    /// click misses the text area.
    pub fn add_caret_at_screen(&mut self, col: u16, row: u16) -> bool {
        self.multi_typing = None;
        let Some((r, c)) = self.buffer_pos_at(col, row) else {
            return false;
        };
        // Clicking the current primary is a no-op (nothing to add).
        if (r, c) == (self.cursor_row, self.cursor_col) {
            return true;
        }
        // Clicking an existing zero-width caret removes it (toggle off).
        if let Some(idx) = self
            .carets
            .iter()
            .position(|s| !s.has_area() && s.head == (r, c))
        {
            self.carets.remove(idx);
            return true;
        }
        // Demote the current primary (preserving any selection) to a caret and
        // promote the clicked point.
        let primary = self.selection.unwrap_or(EditorSelection {
            anchor: (self.cursor_row, self.cursor_col),
            head: (self.cursor_row, self.cursor_col),
        });
        self.carets.push(primary);
        self.selection = None;
        self.cursor_row = r;
        self.cursor_col = c;
        self.ensure_cursor_col_visible();
        true
    }

    /// Begin a column (box) selection anchored at the screen cell `(col, row)`
    /// (VS Code's Shift+Alt+drag). Clears any existing selection/carets and
    /// parks the primary at the anchor. Returns false if the cell misses text.
    pub fn begin_box_select(&mut self, col: u16, row: u16) -> bool {
        self.multi_typing = None;
        let Some((r, c)) = self.buffer_pos_at(col, row) else {
            return false;
        };
        self.box_anchor = Some((r, c));
        self.carets.clear();
        self.selection = None;
        self.cursor_row = r;
        self.cursor_col = c;
        true
    }

    /// Whether a column (box) selection drag is in progress.
    pub fn box_selecting(&self) -> bool {
        self.box_anchor.is_some()
    }

    /// Extend the live column selection to the screen cell `(col, row)`,
    /// rebuilding one caret per spanned row. No-op if no box drag is active.
    pub fn box_drag_to_screen(&mut self, col: u16, row: u16) {
        if self.box_anchor.is_none() {
            return;
        }
        if let Some((r, c)) = self.buffer_pos_at(col, row) {
            self.box_select_to(r, c);
        }
    }

    /// Rebuild the column selection so it spans the rectangle from the box
    /// anchor to `(head_row, head_col)`: every row in the range gets a caret
    /// over the shared column span (clamped to that line's length); the head
    /// row is the primary. Zero-width columns become bare carets.
    fn box_select_to(&mut self, head_row: usize, head_col: usize) {
        self.multi_typing = None;
        let Some((ar, ac)) = self.box_anchor else {
            return;
        };
        let (r0, r1) = (ar.min(head_row), ar.max(head_row));
        let (c0, c1) = (ac.min(head_col), ac.max(head_col));
        self.carets.clear();
        self.selection = None;
        for r in r0..=r1 {
            let len = self.line_char_len(r);
            let s = c0.min(len);
            let e = c1.min(len);
            let sel = EditorSelection {
                anchor: (r, s),
                head: (r, e),
            };
            if r == head_row {
                self.selection = (e > s).then_some(sel);
                self.cursor_row = r;
                self.cursor_col = e;
            } else {
                self.carets.push(sel);
            }
        }
        self.ensure_cursor_col_visible();
    }

    /// End the column-selection drag, keeping the carets it produced.
    pub fn end_box_select(&mut self) {
        self.box_anchor = None;
    }

    /// Grow the column (box) selection from the keyboard (#1560), VS
    /// Code's Ctrl+Shift+Alt+arrows: move its head `rows` rows and `cols`
    /// columns, then rebuild one selection per spanned row, as the
    /// Shift+Alt+drag does. A box starts at the caret (or the one-line
    /// selection's anchor) unless the carets are still the ones the last
    /// column-select key made. The head stays on the buffer's rows and,
    /// like VS Code's, may sit past a short line's end, up to the longest
    /// line in the box.
    pub fn column_select(&mut self, rows: isize, cols: isize) {
        let current = self.column_box_carets();
        let (anchor, head) = match &self.column_box {
            Some(b) if b.left == current => (b.anchor, b.head),
            _ => {
                let caret = (self.cursor_row, self.cursor_col);
                let anchor = match self.selection {
                    Some(sel) if self.carets.is_empty() && sel.anchor.0 == sel.head.0 => sel.anchor,
                    _ => caret,
                };
                (anchor, caret)
            }
        };
        let last = self.lines.len().saturating_sub(1);
        let row = head.0.saturating_add_signed(rows).min(last);
        let (r0, r1) = (anchor.0.min(row), anchor.0.max(row));
        let widest = (r0..=r1).map(|r| self.line_char_len(r)).max().unwrap_or(0);
        let col = head.1.saturating_add_signed(cols).min(widest.max(anchor.1));
        self.box_anchor = Some(anchor);
        self.box_select_to(row, col);
        self.box_anchor = None;
        self.column_box = Some(ColumnBox {
            anchor,
            head: (row, col),
            left: self.column_box_carets(),
        });
    }

    /// The carets and selections a keyboard column selection compares
    /// against what it left, so any edit or move starts a fresh box.
    fn column_box_carets(&self) -> ColumnBoxCarets {
        (
            (self.cursor_row, self.cursor_col),
            self.selection,
            self.carets.clone(),
        )
    }

    /// Rows a column-select PageUp/PageDown moves: one screen of text rows,
    /// as the plain page motions use.
    pub fn column_select_page(&self) -> isize {
        self.page_size().max(1) as isize
    }

    /// VS Code "Add Cursor Above" (Cmd+Alt+Up): mirror of `add_cursor_below`.
    /// No-op at the first row.
    pub fn add_cursor_above(&mut self) {
        self.multi_typing = None;
        let min_row = self
            .carets
            .iter()
            .map(|c| c.head.0)
            .chain(std::iter::once(self.cursor_row))
            .min()
            .unwrap_or(self.cursor_row);
        if min_row <= self.symbol_clip().map_or(0, |(first, _)| first) {
            return;
        }
        let target = min_row - 1;
        let col = self.cursor_col.min(self.line_char_len(target));
        self.carets.push(EditorSelection {
            anchor: (target, col),
            head: (target, col),
        });
    }

    /// True when the active selection spans more than one row, so Tab should
    /// indent the whole block (VS Code) rather than replace the selection
    /// with a single indentation unit.
    pub fn selection_is_multiline(&self) -> bool {
        match &self.selection {
            Some(sel) => {
                let (start, end) = sel.normalised();
                start.0 != end.0
            }
            None => false,
        }
    }

    /// VS Code's plain-Tab indent (no multi-line selection): insert spaces up
    /// to the next tab stop from the cursor column, replacing any single-line
    /// selection first. With `editor.insertSpaces` + `editor.useTabStops`
    /// (both on by default) a Tab at column 2 with a width-4 unit adds 2
    /// spaces to reach column 4, not a flat 4.
    pub fn indent_at_cursor(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::Indent);
        if self.selection.is_some() {
            self.delete_selection_inner();
        }
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let style = self.indent_style();
        let row = self.cursor_row;
        let byte = self.byte_index(row, self.cursor_col);
        let (ins, advance): (String, usize) = if style.use_spaces {
            let unit_w = (style.width as usize).max(1);
            let pad = unit_w - (self.cursor_col % unit_w);
            (" ".repeat(pad), pad)
        } else {
            // A tab-indented buffer inserts one literal tab (useTabStops).
            ("\t".to_string(), 1)
        };
        self.lines[row].insert_str(byte, &ins);
        self.cursor_col += advance;
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Indent Lines" (Tab with a multi-line selection): prepend one
    /// indentation unit to every line the selection touches. Empty lines are
    /// left untouched, matching VS Code's "do not indent empty lines" rule
    /// for block indent. Cursor and selection columns shift right by the unit
    /// width on the lines that actually gained indentation so the same text
    /// stays highlighted.
    pub fn indent_lines(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::Indent);
        let unit = self.indent_unit();
        let unit_w = unit.chars().count();
        let (start_row, end_row) = self.selected_or_cursor_row_range();
        for row in start_row..=end_row {
            if !self.lines[row].is_empty() {
                self.lines[row].insert_str(0, &unit);
            }
        }
        // A row gained indentation iff it was non-empty (empty lines were
        // skipped and stay empty), so a row that is non-empty now is exactly
        // one that shifted right by `unit_w`.
        let shift = |row: usize, col: usize, lines: &[String]| -> usize {
            if row >= start_row && row <= end_row && !lines[row].is_empty() {
                col + unit_w
            } else {
                col
            }
        };
        self.cursor_col = shift(self.cursor_row, self.cursor_col, &self.lines);
        if let Some(mut sel) = self.selection {
            sel.anchor.1 = shift(sel.anchor.0, sel.anchor.1, &self.lines);
            sel.head.1 = shift(sel.head.0, sel.head.1, &self.lines);
            self.selection = Some(sel);
        }
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Outdent" (Shift+Tab): strip one indentation level from every
    /// line the selection touches, or just the current line when there is no
    /// selection. Removal is tab-stop aligned (the `useTabStops` default): a
    /// line indented 6 spaces drops to 4, a line indented 3 drops to 0. A
    /// single leading tab counts as one level. Cursor and selection columns
    /// follow the removal on their own row.
    pub fn dedent_lines(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::Indent);
        let unit_w = (self.indent_style().width as usize).max(1);
        let (start_row, end_row) = self.selected_or_cursor_row_range();
        let mut removed = vec![0usize; self.lines.len()];
        let mut any = false;
        for (row, slot) in removed
            .iter_mut()
            .enumerate()
            .take(end_row + 1)
            .skip(start_row)
        {
            let n = leading_outdent_chars(&self.lines[row], unit_w);
            if n > 0 {
                self.lines[row].replace_range(0..n, "");
                *slot = n;
                any = true;
            }
        }
        let pull = |row: usize, col: usize, removed: &[usize]| -> usize {
            if row < removed.len() {
                col.saturating_sub(removed[row])
            } else {
                col
            }
        };
        self.cursor_col = pull(self.cursor_row, self.cursor_col, &removed);
        if let Some(mut sel) = self.selection {
            sel.anchor.1 = pull(sel.anchor.0, sel.anchor.1, &removed);
            sel.head.1 = pull(sel.head.0, sel.head.1, &removed);
            self.selection = Some(sel);
        }
        if any {
            self.mark_buffer_changed();
            self.recompute_highlights();
        }
        self.ensure_cursor_col_visible();
    }

    /// Word-step left (Option+Left on macOS). Symmetric to `move_word_right`:
    /// skips non-word chars backwards across line boundaries, then walks
    /// back over the preceding word run, landing on its first char.
    pub fn move_word_left(&mut self) {
        loop {
            let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
            self.cursor_col = self.cursor_col.min(chars.len());
            while self.cursor_col > 0 && !word_char_at(&chars, self.cursor_col - 1) {
                self.cursor_col -= 1;
            }
            if self.cursor_col > 0 {
                break;
            }
            if self.cursor_row > 0 {
                self.cursor_row -= 1;
                self.cursor_col = self.line_char_len(self.cursor_row);
            } else {
                self.ensure_cursor_col_visible();
                self.last_edit_kind = None;
                return;
            }
        }
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        while self.cursor_col > 0 && word_char_at(&chars, self.cursor_col - 1) {
            self.cursor_col -= 1;
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    pub fn scroll_left_by(&mut self, n: usize) {
        self.scroll_col = self.scroll_col.saturating_sub(n);
    }

    pub fn scroll_right_by(&mut self, n: usize) {
        self.scroll_col = self.scroll_col.saturating_add(n);
    }

    /// Recompute the visible text width from `last_inner` / `last_gutter_width`
    /// (set during the most recent render) and use it to decide whether the
    /// current cursor column is on-screen. This is the same arithmetic the
    /// renderer uses, kept in one place so move-and-scroll stays in lock-step.
    fn visible_text_width(&self) -> usize {
        let scrollbar_w = u16::from(self.last_scrollbar.width > 0);
        self.last_inner
            .width
            .saturating_sub(self.last_gutter_width + 2 + scrollbar_w) as usize
    }

    /// Pull `scroll_col` so the cursor sits inside the current viewport.
    /// Mirrors `ensure_cursor_visible` for vertical scroll - keyboard nav
    /// follows; wheel scrolling does not call this so the user's wheel
    /// position is sacrosanct. `pub(crate)` so callers like
    /// `App::open_search_hit` can sync horizontal scroll after a jump.
    pub(crate) fn ensure_cursor_col_visible(&mut self) {
        if self.wrap_enabled() {
            return;
        }
        let width = self.visible_text_width();
        if width == 0 {
            return;
        }
        if self.cursor_col < self.scroll_col {
            self.scroll_col = self.cursor_col;
        } else {
            // Hint cells between the scroll origin and the caret consume
            // viewport width too. Scrolling right drops leading hints out of
            // the row, so re-measure until the caret fits (the loop is
            // bounded: `scroll_col` only grows, capped at `cursor_col`).
            self.sync_tab_cells();
            loop {
                let extra = self.inlay_cells_before_cursor(self.cursor_row, self.scroll_col);
                let row = RowCells::of_line(
                    self.lines.get(self.cursor_row).map_or("", String::as_str),
                    self.scroll_col,
                );
                let cells = row.text_before(self.cursor_col) + extra;
                if cells < width {
                    break;
                }
                // Scroll right by the cells the caret overflows by (a tab or
                // wide character is several), measured in columns.
                let overflow = cells + 1 - width;
                let mut target = self.scroll_col;
                while target < self.cursor_col && row.text_before(target) < overflow {
                    target += 1;
                }
                if target <= self.scroll_col {
                    break;
                }
                self.scroll_col = target.min(self.cursor_col);
            }
        }
    }

    /// One screen worth of rows, derived from the editor's last rendered
    /// inner height.  Falls back to a sensible default before the first
    /// render (when `last_inner.height` is still 0).
    pub fn page_size(&self) -> usize {
        let from_rows = self.text_rows();
        if from_rows > 0 { from_rows } else { 20 }
    }

    /// Move the viewport down by exactly one screen so the first
    /// previously-unseen row becomes the new top of the viewport, and place
    /// the cursor on that new top row.  Clamps at end of file.
    pub fn page_down_one_screen(&mut self) {
        let page = self.page_size();
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top + page);
            return;
        }
        let max_row = self.lines.len().saturating_sub(1);
        let new_top = (self.scroll + page).min(max_row);
        self.scroll = new_top;
        self.cursor_row = new_top;
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    /// Move the viewport up by exactly one screen so the new top is `page`
    /// rows above the previous top.  Cursor lands on the new top row.
    /// Clamps at the start of file.
    pub fn page_up_one_screen(&mut self) {
        let page = self.page_size();
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top.saturating_sub(page));
            return;
        }
        let new_top = self.scroll.saturating_sub(page);
        self.scroll = new_top;
        self.cursor_row = new_top;
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    /// Start of the logical line (`Ctrl+A`), before any indentation.
    pub fn home_line(&mut self) {
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    /// End of the logical line (`Ctrl+E`, and End's second step).
    pub fn end_line(&mut self) {
        self.cursor_col = self.line_char_len(self.cursor_row);
        self.last_edit_kind = None;
    }

    /// The Home key, VS Code's `cursorHome`: to the line's first
    /// non-whitespace character, or to its start when the caret is already
    /// there, so a second press toggles (#987). It used to always go to the
    /// start, before the indentation. With wrap on (#1569) it first goes to
    /// the start of the wrapped row the caret is on. With
    /// `home_walks_rows`, a caret already on a continuation row's start goes
    /// to the start of the row above, one row per press, down to the line's
    /// own stops.
    pub fn home_key(&mut self) {
        let line = &self.lines[self.cursor_row];
        let first = line
            .chars()
            .position(|c| c != ' ' && c != '\t')
            .unwrap_or_else(|| line.chars().count());
        let smart = if self.cursor_col == first { 0 } else { first };
        self.cursor_col = match self.wrapped_row_of_cursor() {
            // A row can start on a combining mark (the wrap breaks after the
            // space it sits on), so step back to its cluster's start.
            Some((start, _)) => match floor_grapheme_col(line, start) {
                row_start if self.cursor_col > row_start && row_start > first => row_start,
                _ if self.home_walks_rows && start > 0 && self.cursor_col == start => {
                    self.wrapped_row_above(start).unwrap_or(smart)
                }
                _ => smart,
            },
            None => smart,
        };
        self.last_edit_kind = None;
    }

    /// The End key. With wrap on (#1569) it first goes to the end of the
    /// wrapped row the caret is on, before the character the row breaks at
    /// (the space, usually), since the column after it is drawn on the next
    /// row; from there, to the line's end.
    pub fn end_key(&mut self) {
        let len = self.line_char_len(self.cursor_row);
        self.cursor_col = match self.wrapped_row_of_cursor() {
            Some((_, end)) if end < len => {
                let last = floor_grapheme_col(&self.lines[self.cursor_row], end - 1);
                if self.cursor_col < last { last } else { len }
            }
            _ => len,
        };
        self.last_edit_kind = None;
    }

    pub fn scroll_up(&mut self, n: usize) {
        // A rendered log scrolls by file line, ahead of the wrap/comment-box
        // paths that assume an editable buffer (#257).
        if self.log.is_some() {
            self.scroll_view_to(self.scroll.saturating_sub(n));
            return;
        }
        if self.scroll_markdown_preview(-(n as i32)) {
            return;
        }
        if self.scroll_sheet_rows(-(n as isize)) {
            return;
        }
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top.saturating_sub(n));
        } else if self.comment_boxes.is_empty() {
            self.scroll_view_to(self.scroll.saturating_sub(n));
        } else {
            self.nonwrap_set_top(self.nonwrap_top_content_row().saturating_sub(n));
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        if self.log.is_some() {
            self.scroll_view_to(self.scroll.saturating_add(n));
            return;
        }
        if self.scroll_markdown_preview(n as i32) {
            return;
        }
        if self.scroll_sheet_rows(n as isize) {
            return;
        }
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top.saturating_add(n));
        } else if self.comment_boxes.is_empty() {
            self.scroll_view_to(self.scroll.saturating_add(n));
        } else {
            self.nonwrap_set_top(self.nonwrap_top_content_row().saturating_add(n));
        }
    }

    /// Turn a symbol tab into an ordinary tab of its whole file: the
    /// buffer already holds the whole file, so only the clip and the
    /// mirror state go.
    pub fn leave_symbol_view(&mut self) {
        self.symbol_view = None;
        self.mirror_seq = None;
        self.mirrored_step = None;
        self.mirror_caret = None;
    }

    /// Turn a symbol tab into an ordinary tab of its whole file while its
    /// text stays in step with its siblings: only the clip and the pending
    /// caret go, and the mirror state is kept. Callers: only right after a
    /// mirror pass settled the tab, or on a file a collab session keeps in
    /// step; a tab reused for other text needs `leave_symbol_view`.
    pub fn drop_symbol_clip(&mut self) {
        self.symbol_view = None;
        self.mirror_caret = None;
    }

    /// A symbol tab's visible lines as `(first, end)`, `end` exclusive,
    /// kept inside the buffer. `None` on an ordinary tab.
    pub fn symbol_clip(&self) -> Option<(usize, usize)> {
        let v = self.symbol_view.as_ref()?;
        let end = (v.last + 1).min(self.lines.len()).max(1);
        Some((v.first.min(end - 1), end))
    }

    /// The lines find and replace work over, and the row of the first: a
    /// symbol tab's symbol, or the whole buffer.
    pub fn find_scope(&self) -> (&[String], usize) {
        match self.symbol_clip() {
            Some((first, end)) => (&self.lines[first..end], first),
            None => (&self.lines, 0),
        }
    }

    /// The clip in the scrollbar's units: content rows above it, and rows
    /// in it. Visual rows when wrapping, else lines plus comment-box rows.
    fn clip_content_rows(&self, width: usize, wrap: bool) -> Option<(usize, usize)> {
        let (first, end) = self.symbol_clip()?;
        let rows = |a: usize, b: usize| -> usize {
            if wrap {
                (a..b).map(|l| self.group_visual_rows(l, width)).sum()
            } else {
                (b - a) + self.box_rows_between(a, b, width)
            }
        };
        Some((rows(0, first), rows(first, end)))
    }

    /// Keep a symbol tab's caret, selections and viewport on its symbol's
    /// lines. Positions above the clip snap to its first character, below
    /// it to its last, so every motion that would leave the symbol stops at
    /// its edge instead. Secondary carets outside the clip are dropped
    /// rather than snapped, and duplicates removed.
    pub fn clamp_to_symbol_view(&mut self) {
        let Some((first, end)) = self.symbol_clip() else {
            return;
        };
        let last = end - 1;
        let last_len = self.line_char_len(last);
        let clamp = |p: (usize, usize)| -> (usize, usize) {
            if p.0 < first {
                (first, 0)
            } else if p.0 > last {
                (last, last_len)
            } else {
                p
            }
        };
        (self.cursor_row, self.cursor_col) = clamp((self.cursor_row, self.cursor_col));
        let clamp_sel = |sel: &mut EditorSelection| {
            sel.anchor = clamp(sel.anchor);
            sel.head = clamp(sel.head);
        };
        if let Some(sel) = self.selection.as_mut() {
            clamp_sel(sel);
        }
        // A secondary caret outside the symbol is dropped, not clamped:
        // clamped, several would stack on one edge and each type there.
        self.carets.retain(|c| (first..=last).contains(&c.head.0));
        self.carets.iter_mut().for_each(clamp_sel);
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        let mut seen = vec![primary];
        self.carets.retain(|c| {
            let fresh = !seen.contains(c);
            seen.push(*c);
            fresh
        });
        if self.scroll < first || self.scroll > last {
            self.scroll = self.scroll.clamp(first, last);
            self.scroll_sub = 0;
        }
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let viewport = self.text_rows();
        if self.wrap_enabled() {
            let width = self.visible_text_width();
            let (base, total) = match self.clip_content_rows(width, true) {
                Some(clip) => clip,
                None => (0, self.total_visual_rows(width)),
            };
            let Some(metrics) = scrollbar::vertical_metrics(
                self.last_scrollbar,
                total,
                viewport,
                self.top_visual_row(width).saturating_sub(base),
            ) else {
                return false;
            };
            self.wrap_set_top(base + scrollbar::scroll_for_y(metrics, y));
            return true;
        }
        // Same content length the render sized the bar with: lines plus
        // comment-box rows. Mapping through bare `lines.len()` made the bar
        // of a short file with a tall navigator comment dead (metrics said
        // "no overflow") and a long file's thumb run away from the pointer.
        let bw = self.visible_text_width();
        let (base, content) = self.clip_content_rows(bw, false).unwrap_or((
            0,
            self.lines.len() + self.box_rows_between(0, self.lines.len(), bw),
        ));
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            content,
            viewport,
            self.nonwrap_top_content_row().saturating_sub(base),
        ) else {
            return false;
        };
        self.nonwrap_set_top(base + scrollbar::scroll_for_y(metrics, y));
        true
    }

    /// Map a click/drag X coordinate on the horizontal scrollbar onto
    /// `scroll_col`. Returns false when there is no horizontal bar (no
    /// overflow), so the caller can fall through to other hit-tests.
    pub fn scroll_to_bar_x(&mut self, x: u16) -> bool {
        let width = self.visible_text_width();
        let Some(metrics) = scrollbar::horizontal_metrics(
            self.last_hscrollbar,
            self.content_cols(width),
            width,
            self.scroll_col,
        ) else {
            return false;
        };
        self.scroll_col = scrollbar::scroll_for_x(metrics, x);
        true
    }

    /// Rows used for text in the last render (inner height minus the
    /// horizontal scrollbar row when present). Falls back to the full inner
    /// height before the first render populates `last_text_rows`.
    pub(crate) fn text_rows(&self) -> usize {
        let rows = self.last_text_rows as usize;
        if rows > 0 {
            rows
        } else {
            self.last_inner.height as usize
        }
    }

    /// Number of text rows currently visible (the minimap viewport box height).
    pub fn visible_rows(&self) -> usize {
        self.text_rows()
    }

    /// Inclusive `(start_row, end_row)` of the active selection, or `None` when
    /// there's no selection with area. Drives the minimap selection band.
    pub fn selection_rows(&self) -> Option<(usize, usize)> {
        let sel = self.selection?;
        if !sel.has_area() {
            return None;
        }
        let (start, end) = sel.normalised();
        Some((start.0, end.0))
    }

    /// The lines the minimap draws, as `(first, end)` with `end` exclusive:
    /// a symbol tab's symbol (#369), else the whole buffer.
    pub fn minimap_span(&self) -> (usize, usize) {
        self.symbol_clip().unwrap_or((0, self.lines.len()))
    }

    /// Move the cursor to `line` and center the viewport on it (minimap click /
    /// drag navigation). The cursor moves too because the render's scroll-follow
    /// snaps the viewport back to keep the caret visible; centering the caret
    /// here means that follow logic leaves the chosen scroll position alone.
    pub fn goto_line_centered(&mut self, line: usize) {
        if self.lines.is_empty() {
            return;
        }
        let line = line.min(self.lines.len() - 1);
        self.cursor_row = line;
        self.cursor_col = self.cursor_col.min(self.line_char_len(line));
        let viewport = self.text_rows();
        if viewport == 0 {
            // Not rendered yet (a file opened for the first time has no
            // geometry), so there is no middle to find: leave a few lines of
            // context above the target and let the first render follow.
            self.scroll = line.saturating_sub(5);
        } else {
            self.scroll = line
                .saturating_sub(viewport / 2)
                .min(self.lines.len().saturating_sub(viewport));
        }
        self.last_edit_kind = None;
    }

    /// Rasterize the whole document into a `w`x`h` RGBA buffer for the minimap:
    /// one column per character (clipped to `w`), each non-whitespace char
    /// painted in its syntax color. Whitespace stays `bg`, so indentation reads
    /// as the file's shape, like VS Code's minimap. The lines are mapped into
    /// `content_h` rows (top-aligned) rather than the full `h`: a short file
    /// uses a fixed small per-line height and leaves the rest of the strip
    /// blank, instead of stretching six lines down the whole column. For a file
    /// long enough to fill the strip, `content_h == h` and the mapping is the
    /// plain whole-file fit.
    /// ponytail: 1px per char, clipped at strip width; sub-pixel char scaling
    /// only if a wider strip ever needs to show more columns.
    pub fn minimap_rgba(
        &self,
        w: u32,
        h: u32,
        content_h: u32,
        bg: (u8, u8, u8),
        fg: (u8, u8, u8),
    ) -> Vec<u8> {
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for px in buf.chunks_exact_mut(4) {
            px[0] = bg.0;
            px[1] = bg.1;
            px[2] = bg.2;
            px[3] = 0xff;
        }
        let (first, end) = self.minimap_span();
        let total = (end - first).max(1) as u64;
        for (i, line) in self.lines[first..end].iter().enumerate() {
            let y0 = (i as u64 * content_h as u64 / total) as u32;
            if y0 >= h {
                break;
            }
            let y1 = (((i as u64 + 1) * content_h as u64 / total) as u32).clamp(y0 + 1, h);
            let merged = merge_overlay(
                self.highlights
                    .get(first + i)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                self.semantic_overlay
                    .get(first + i)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            );
            let mut si = 0usize;
            for (col, (byte, ch)) in (0_u32..).zip(line.char_indices()) {
                if col >= w {
                    break;
                }
                while si < merged.len() && merged[si].range().end <= byte {
                    si += 1;
                }
                if !ch.is_whitespace() {
                    let (r, g, b) = if si < merged.len() && merged[si].range().start <= byte {
                        span_rgb(merged[si].style, fg)
                    } else {
                        fg
                    };
                    for y in y0..y1 {
                        let idx = ((y * w + col) * 4) as usize;
                        buf[idx] = r;
                        buf[idx + 1] = g;
                        buf[idx + 2] = b;
                    }
                }
            }
        }
        buf
    }

    /// The non-wrap viewport top in CONTENT rows: buffer lines plus the
    /// comment-box rows above (and inside, via `scroll_sub`) the top line.
    fn nonwrap_top_content_row(&self) -> usize {
        let bw = self.visible_text_width();
        self.scroll + self.box_rows_between(0, self.scroll, bw) + self.scroll_sub
    }

    /// Set the non-wrap viewport top to content row `row`, landing mid-box
    /// when it falls inside a comment box (`scroll_sub` counts rows past
    /// the top line's own text row). Mirrors `wrap_set_top` for the
    /// box-extended non-wrap layout; a tall box is unreadable and its
    /// Reply/Ignore footer unreachable with a line-granular top. Pulls the
    /// cursor along like `scroll_view_to`.
    fn nonwrap_set_top(&mut self, row: usize) {
        let viewport = self.text_rows();
        if viewport == 0 || self.lines.is_empty() {
            self.scroll = 0;
            self.scroll_sub = 0;
            return;
        }
        if self.comment_boxes.is_empty() {
            self.scroll_sub = 0;
            self.scroll_view_to(row);
            return;
        }
        let bw = self.visible_text_width();
        let total = self.lines.len() + self.box_rows_between(0, self.lines.len(), bw);
        let row = row.min(total.saturating_sub(viewport));
        let mut acc = 0usize;
        let mut line = 0usize;
        while line < self.lines.len() {
            let group = 1 + self.box_rows_between(line, line + 1, bw);
            if acc + group > row {
                break;
            }
            acc += group;
            line += 1;
        }
        self.scroll = line.min(self.lines.len().saturating_sub(1));
        self.scroll_sub = row.saturating_sub(acc);
        let last_visible = (self.scroll + viewport - 1).min(self.lines.len().saturating_sub(1));
        let floor = self.caret_floor_row().min(last_visible);
        if self.cursor_row < floor {
            self.cursor_row = floor;
        } else if self.cursor_row > last_visible {
            self.cursor_row = last_visible;
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    fn scroll_view_to(&mut self, top: usize) {
        let viewport = self.last_inner.height as usize;
        // A rendered log's text side is a one-line stub, so clamping against
        // `lines` would pin scroll at 0 and the view could never move past its
        // first screen (#257). Clamp against the log's own line count instead;
        // its body area is one row shorter than the viewport (the header).
        if let Some(log) = self.log.as_ref() {
            let rows = viewport.saturating_sub(1).max(1);
            self.scroll = top.min(log.len().saturating_sub(rows));
            // Any scroll is the reader's own place now, which a reload still
            // settling must not take back.
            self.log_reload_top = None;
            return;
        }
        if viewport == 0 || self.lines.is_empty() {
            self.scroll = 0;
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.last_edit_kind = None;
            return;
        }
        self.scroll = top.min(self.lines.len().saturating_sub(viewport));
        let last_visible = (self.scroll + viewport - 1).min(self.lines.len().saturating_sub(1));
        let floor = self.caret_floor_row().min(last_visible);
        if self.cursor_row < floor {
            self.cursor_row = floor;
        } else if self.cursor_row > last_visible {
            self.cursor_row = last_visible;
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    /// Topmost line the caret may be dragged to when the viewport scrolls out
    /// from under it: the first row the sticky band does not cover. Parking it
    /// on `scroll` itself puts it under the pinned headers, where it cannot be
    /// seen and where the band would have to yield to stay honest — which
    /// deleted the band for the whole of a wheel scroll.
    ///
    /// The band is measured from the previous frame's `sticky_lines` (the app
    /// layer rebuilds them from `scroll` on the next render). A burst of wheel
    /// events can therefore be one header out mid-gesture, which the render's
    /// own caret guard absorbs by trimming the band for that frame.
    fn caret_floor_row(&self) -> usize {
        if self.wrap_enabled() || self.sticky_lines.is_empty() {
            return self.scroll;
        }
        let rows_avail = (self.last_inner.height as usize).saturating_sub(1);
        self.scroll + self.sticky_lines.len().min(rows_avail)
    }

    /// Double-click word selection: place the cursor at the click point, then
    /// select the maximal run of word characters covering it and leave the
    /// caret at the right edge of that run (VS Code parity). A click on
    /// whitespace or past the end of an empty line clears any selection.
    pub fn select_word_at(&mut self, col: u16, row: u16) {
        self.click(col, row);
        if self.lines.is_empty() {
            self.selection = None;
            return;
        }
        let r = self.cursor_row;
        let chars: Vec<char> = self.lines[r].chars().collect();
        let c = self.cursor_col;
        let pivot = if c < chars.len() && is_word_char(chars[c]) {
            Some(c)
        } else if c == chars.len() && c > 0 && is_word_char(chars[c - 1]) {
            Some(c - 1)
        } else {
            None
        };
        let Some(p) = pivot else {
            self.selection = None;
            return;
        };
        let mut start = p;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = p + 1;
        while end < chars.len() && is_word_char(chars[end]) {
            end += 1;
        }
        self.cursor_col = end;
        self.selection = Some(EditorSelection {
            anchor: (r, start),
            head: (r, end),
        });
        self.last_edit_kind = None;
    }

    /// Lines `from` through `to` as one whole-line selection (#1570): from
    /// the start of the top line to the start of the line after the bottom
    /// one, or to the end of the buffer's last line, which has none. With
    /// `to` above `from` it runs upward, the caret at the top.
    fn whole_lines(&self, from: usize, to: usize) -> EditorSelection {
        let last = self.lines.len().saturating_sub(1);
        let (top, bottom) = (from.min(to).min(last), from.max(to).min(last));
        let start = (top, 0);
        let end = if bottom < last {
            (bottom + 1, 0)
        } else {
            (last, self.line_char_len(last))
        };
        let (anchor, head) = if to < from {
            (end, start)
        } else {
            (start, end)
        };
        EditorSelection { anchor, head }
    }

    /// Whole-line selection by mouse (#1570), as VS Code's triple-click and
    /// line-number click make: select from line `anchor` (the line under the
    /// pointer when `None`) through the line under screen `(col, row)`, and
    /// return the anchor so a drag can keep extending from it.
    pub fn select_lines_at(&mut self, anchor: Option<usize>, col: u16, row: u16) -> usize {
        self.click(col, row);
        let anchor = anchor.unwrap_or(self.cursor_row);
        self.set_selections(&[self.whole_lines(anchor, self.cursor_row)]);
        anchor
    }

    /// VS Code "Expand Line Selection" (#1570): every cursor's selection
    /// grows to the whole lines it touches, through the start of the next
    /// line. A selection that already ends at a line start reaches one line
    /// further, so each press adds a line.
    pub fn expand_line_selection(&mut self) {
        let all: Vec<EditorSelection> = self
            .selections_with_primary()
            .iter()
            .map(|s| {
                let (start, end) = s.normalised();
                self.whole_lines(start.0, end.0)
            })
            .collect();
        self.set_selections(&all);
        self.last_edit_kind = None;
    }

    /// Move the cursor to the screen coordinates (col, row). Used for mouse clicks.
    pub fn click(&mut self, col: u16, row: u16) {
        if self.lines.is_empty() || self.last_inner.height == 0 {
            return;
        }
        if row < self.last_inner.y || row >= self.last_inner.y + self.last_inner.height {
            return;
        }
        let row_idx = (row - self.last_inner.y) as usize;
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        // Gutter clicks land on the row's first column (saturates to 0).
        let visible = col.saturating_sub(text_x) as usize;
        // Map the screen row through the last render's visual-row layout (the
        // same map `buffer_pos_at` reads) so wrapped and folded lines resolve
        // to the line actually shown on that row; the linear map drifts by one
        // for every wrapped continuation row above the pointer. A press below
        // the last visual row clamps to it. The map is empty only before the
        // first paint, when nothing can be wrapped or folded and the linear
        // map is exact.
        // A press on a comment-box row leaves the caret alone (the App
        // intercepts box clicks; this is the belt-and-braces path). A press
        // below the last visual row clamps to the nearest text row above.
        let clamped = row_idx.min(self.last_wrap_rows.len().saturating_sub(1));
        let nearest_text = (0..=clamped)
            .rev()
            .find_map(|i| self.text_row(i).map(|t| (i, t)));
        if !self.last_wrap_rows.is_empty()
            && row_idx < self.last_wrap_rows.len()
            && self.text_row(row_idx).is_none()
        {
            return;
        }
        // `start + visible` is a DISPLAY column, so inlay hints spliced into
        // the row have to be mapped back out of it — the same inversion
        // `buffer_pos_at` (and therefore hover) applies. Without this the
        // caret lands `hint` cells right of the pointer.
        let (target_line, target_col) = match nearest_text {
            Some((_, (line, start, end))) => {
                // In wrap mode a drag past a segment's right edge stops at the
                // segment end; non-wrap keeps the click-to-line-end behaviour.
                let cap = if self.wrap_enabled() { end } else { usize::MAX };
                (
                    line,
                    self.buffer_col_at_display(line, start, visible).min(cap),
                )
            }
            None => {
                let line = (self.scroll + row_idx).min(self.lines.len().saturating_sub(1));
                (
                    line,
                    self.buffer_col_at_display(line, self.scroll_col, visible),
                )
            }
        };
        self.cursor_row = target_line;
        self.cursor_col = floor_grapheme_col(&self.lines[target_line], target_col);
        self.last_edit_kind = None;
        // A click is a deliberate reposition; any multi-cursor session ends.
        self.carets.clear();
    }

    pub fn buffer_pos_at(&self, col: u16, row: u16) -> Option<(usize, usize)> {
        if self.last_inner.height == 0 || self.lines.is_empty() || row < self.last_inner.y {
            return None;
        }
        self.sync_tab_cells();
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        if self.wrap_enabled() {
            // Map the screen row through the last render's visual-row layout so
            // a click lands on the right logical line/column even when wrapped.
            // The blank tail of a folded row maps to the segment end, letting
            // you click to end-of-visual-line.
            let (line, start, end) = self.text_row((row - self.last_inner.y) as usize)?;
            if col < text_x {
                return None;
            }
            let visible_col = (col - text_x) as usize;
            let char_col = self
                .buffer_col_at_display(line, start, visible_col)
                .min(end)
                .min(self.line_char_len(line));
            return Some((line, char_col));
        }
        // Non-wrap: map through the painted layout (comment boxes shift the
        // rows below them); before the first paint the linear map is exact.
        if row >= self.last_inner.y + self.last_inner.height {
            return None;
        }
        let line = if self.last_wrap_rows.is_empty() {
            self.scroll + (row - self.last_inner.y) as usize
        } else {
            self.text_row((row - self.last_inner.y) as usize)?.0
        };
        if line >= self.lines.len() || col < text_x {
            return None;
        }
        let text_width = self
            .last_inner
            .width
            .saturating_sub(self.last_gutter_width + 2 + u16::from(self.last_scrollbar.width > 0));
        let visible_col = (col - text_x) as usize;
        if visible_col >= text_width as usize {
            return None;
        }
        Some((
            line,
            self.buffer_col_at_display(line, self.scroll_col, visible_col),
        ))
    }

    /// Invert the row's display map: the first buffer column of `line`
    /// (counting from `base`, the column the row starts at) whose first cell
    /// reaches `visible_cells` cells right of the row's left edge. Cells are
    /// as painted: a tab takes its Tab Size, a wide character two, and a
    /// point landing inside an inlay hint's own cells resolves to its anchor
    /// column, so the caret snaps beside the code the hint annotates.
    fn buffer_col_at_display(&self, line: usize, base: usize, visible_cells: usize) -> usize {
        self.sync_tab_cells();
        let line_len = self.line_char_len(line);
        let base = base.min(line_len);
        let text = self.lines.get(line).map_or("", String::as_str);
        let mut row = RowCells::default();
        row.fill(
            &text[byte_index_of_char(text, base)..],
            base,
            self.row_origin(text, base),
            self.row_inlay_spans(line)
                .iter()
                .filter(|(hc, _, _)| *hc >= base)
                .map(|(hc, l, _)| (*hc, l.chars().count())),
        );
        let mut c = base;
        while c < line_len && row.cell(c) < visible_cells {
            c += 1;
        }
        c
    }

    pub fn word_at(&self, line: usize, col: usize) -> Option<(usize, usize)> {
        let chars: Vec<char> = self.lines.get(line)?.chars().collect();
        if col >= chars.len() || !is_word_char(chars[col]) {
            return None;
        }
        let mut start = col;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = col + 1;
        while end < chars.len() && is_word_char(chars[end]) {
            end += 1;
        }
        Some((start, end))
    }

    /// Like [`Self::word_at`], but over the identifier runs the naming-style
    /// transforms rewrite: letters, digits, `_` and `-` (#1573). A `-` joins
    /// the run only between a letter or digit and a letter, so the minus in
    /// `total-5` or `-count` stays an operator.
    fn identifier_run_at(&self, line: usize, col: usize) -> Option<(usize, usize)> {
        let chars: Vec<char> = self.lines.get(line)?.chars().collect();
        let ident = |i: usize| {
            let c = chars[i];
            c.is_alphanumeric()
                || c == '_'
                || (c == '-'
                    && i > 0
                    && chars[i - 1].is_alphanumeric()
                    && chars.get(i + 1).is_some_and(|n| n.is_alphabetic()))
        };
        if col >= chars.len() || !ident(col) {
            return None;
        }
        let mut start = col;
        while start > 0 && ident(start - 1) {
            start -= 1;
        }
        let mut end = col + 1;
        while end < chars.len() && ident(end) {
            end += 1;
        }
        Some((start, end))
    }

    /// The identifier text under `(line, col)`, or None over non-word
    /// characters. Used by debug hover-to-evaluate to name the variable.
    pub fn word_string_at(&self, line: usize, col: usize) -> Option<String> {
        let (start, end) = self.word_at(line, col)?;
        let chars: Vec<char> = self.lines.get(line)?.chars().collect();
        Some(chars[start..end].iter().collect())
    }

    /// Severities and messages of every diagnostic whose range covers the
    /// character position, each message followed by its rule as
    /// `source(code)` and the rule's link on a line of its own (#1540), then
    /// a line per related location (#1523), its file relative to `root`.
    /// The position is `(line, ch)`. Decoded on demand from the retained
    /// `diagnostics` (UTF-16 positions) rather than the per-line span cache so
    /// the messages don't have to be duplicated into the render-time cache.
    /// Only runs on a hover dwell, never per frame. A zero-width diagnostic is
    /// widened to one cell to match the underline, so a point diagnostic is
    /// still hoverable. Returns an empty vec when no squiggle is under the
    /// point (and when the retained batch belongs to another file).
    pub fn diagnostics_at(
        &self,
        line: usize,
        ch: usize,
        root: &Path,
    ) -> Vec<(crate::lsp::manager::DiagnosticSeverity, String)> {
        if self.diagnostics_path.as_deref() != self.path.as_deref() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for d in &self.diagnostics {
            let (start_line, start_char) = self.diagnostic_row(d.start_line, d.start_char);
            let (end_line, end_char) = self.diagnostic_row(d.end_line, d.end_char);
            if line < start_line || line > end_line {
                continue;
            }
            let Some(text) = self.lines.get(line) else {
                continue;
            };
            let from = if line == start_line {
                utf16_to_char_col(text, start_char)
            } else {
                0
            };
            let to = if line == end_line {
                utf16_to_char_col(text, end_char)
            } else {
                text.chars().count()
            };
            let to = to.max(from + 1);
            if ch >= from && ch < to {
                let mut text = message_with_code(d);
                for related in d.related() {
                    text.push('\n');
                    text.push_str(&related.label(root));
                }
                out.push((d.severity, text));
            }
        }
        out
    }

    /// The region a hover popup should be keyed to at `(line, ch)`: the word
    /// range when one is under the point, otherwise the span of the first
    /// diagnostic covering it (so a squiggle on punctuation, e.g. a `..`
    /// syntax error, still anchors and dismisses a hover). `None` when neither
    /// is present. Shared by `poll_hover`, `drain_lsp_hover`, and the
    /// mouse-move dismissal so all three agree on what the popup belongs to.
    pub fn hover_region_at(&self, line: usize, ch: usize) -> Option<(usize, usize, usize)> {
        if let Some((start, end)) = self.word_at(line, ch) {
            return Some((line, start, end));
        }
        if self.diagnostics_path.as_deref() != self.path.as_deref() {
            return None;
        }
        for d in &self.diagnostics {
            let (start_line, start_char) = self.diagnostic_row(d.start_line, d.start_char);
            let (end_line, end_char) = self.diagnostic_row(d.end_line, d.end_char);
            if line < start_line || line > end_line {
                continue;
            }
            let text = self.lines.get(line)?;
            let from = if line == start_line {
                utf16_to_char_col(text, start_char)
            } else {
                0
            };
            let to = if line == end_line {
                utf16_to_char_col(text, end_char)
            } else {
                text.chars().count()
            };
            let to = to.max(from + 1);
            if ch >= from && ch < to {
                return Some((line, from, to));
            }
        }
        None
    }
}

/// The string and comment byte ranges of a buffer, with each line's start
/// offset, so a bracket scan can tell code from text (#1634).
struct BracketZones<'a> {
    lines: &'a [String],
    starts: Vec<usize>,
    ranges: &'a [(usize, usize)],
}

impl BracketZones<'_> {
    /// The string or comment `(row, char col)` lies in, as an index into
    /// the ranges; `None` in code.
    fn zone(&self, (row, col): BPos) -> Option<usize> {
        self.zone_at(row, char_byte(self.lines.get(row)?, col))
    }

    /// [`Self::zone`] of the char at `byte` on `row`, for a scan that
    /// already has the byte offset.
    fn zone_at(&self, row: usize, byte: usize) -> Option<usize> {
        let byte = self.starts.get(row)? + byte;
        let i = self.ranges.partition_point(|&(_, end)| end <= byte);
        self.ranges
            .get(i)
            .filter(|&&(start, _)| start <= byte)
            .map(|_| i)
    }
}

/// `(char col, byte, char)` for `line`'s chars before char column `end`
/// (all of them when `None`), the last first: one pass for a backward
/// bracket scan, however long the line (#959).
fn chars_back(line: &str, end: Option<usize>) -> impl Iterator<Item = (usize, usize, char)> + '_ {
    let head = &line[..end.map_or(line.len(), |end| char_byte(line, end))];
    let count = head.chars().count();
    head.char_indices()
        .rev()
        .zip((0..count).rev())
        .map(|((byte, ch), col)| (col, byte, ch))
}

/// Convert a char index within `s` to a byte index, saturating at `s.len()`.
fn char_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Char column of the extended grapheme cluster boundary before `col` on
/// `line`, so one step never stops inside a ZWJ emoji, a flag or a letter with
/// its combining accent (#1196). Columns stay char-indexed: every cluster
/// boundary is a char boundary.
fn prev_grapheme_col(line: &str, col: usize) -> usize {
    if line.is_ascii() {
        return col.saturating_sub(1);
    }
    let mut start = 0;
    let mut prev = 0;
    for g in line.graphemes(true) {
        if start >= col {
            break;
        }
        prev = start;
        start += g.chars().count();
    }
    prev
}

/// Char column of the extended grapheme cluster boundary after `col` on
/// `line`, saturating at the line's end (#1196).
fn next_grapheme_col(line: &str, col: usize) -> usize {
    if line.is_ascii() {
        return (col + 1).min(line.len());
    }
    let mut end = 0;
    for g in line.graphemes(true) {
        end += g.chars().count();
        if end > col {
            return end;
        }
    }
    end
}

/// `col` moved back to the start of the grapheme cluster it falls inside, so
/// a vertical move or a click never leaves the caret mid-cluster (#1196).
fn floor_grapheme_col(line: &str, col: usize) -> usize {
    if line.is_ascii() {
        return col.min(line.len());
    }
    let mut start = 0;
    for g in line.graphemes(true) {
        let len = g.chars().count();
        if start + len > col {
            return start;
        }
        start += len;
    }
    start
}

pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `chars[i]` belongs to a word for the word motions and deletes
/// (#909): a word char, or a zero-width mark riding on one (a combining
/// accent in decomposed text, a variation selector), so `Ctrl+Right` never
/// stops between `e` and its accent for `Ctrl+Backspace` to split them.
pub(crate) fn word_char_at(chars: &[char], i: usize) -> bool {
    let joins = |c: char| !c.is_control() && unicode_width::UnicodeWidthChar::width(c) == Some(0);
    let base = (0..=i)
        .rev()
        .find(|&j| j == 0 || !joins(chars[j]))
        .unwrap_or(0);
    is_word_char(chars[base])
}

/// Where Delete Word Left stops on `line`, going back from char `col`: over
/// any non-word chars, then over the word before them (`move_word_left`'s
/// stop within a line), moved back to a grapheme boundary.
fn word_start_before(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let mut i = col.min(chars.len());
    while i > 0 && !word_char_at(&chars, i - 1) {
        i -= 1;
    }
    while i > 0 && word_char_at(&chars, i - 1) {
        i -= 1;
    }
    floor_grapheme_col(line, i)
}

/// Where Delete Word Right stops on `line`, going on from char `col`: over
/// any non-word chars, then to the end of the word after them
/// (`move_word_right`'s stop within a line), on to a grapheme boundary.
fn word_end_after(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let mut i = col.min(chars.len());
    while i < chars.len() && !word_char_at(&chars, i) {
        i += 1;
    }
    while i < chars.len() && word_char_at(&chars, i) {
        i += 1;
    }
    match floor_grapheme_col(line, i) {
        start if start < i => next_grapheme_col(line, start),
        _ => i,
    }
}

/// A `(row, char_col)` buffer position: a bracket character's in the
/// bracket matching helpers, a selection end's in the line commands.
type BPos = (usize, usize);

/// `Editor::bracket_pair_cache`: the key it was worked out at, and the pair.
type BracketPairCache = ((u64, u64, BPos, bool), Option<(BPos, BPos)>);

/// The bracket pairs croft matches, mirroring VS Code's default language
/// brackets `()`, `[]`, `{}`.
const BRACKET_PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

fn is_open_bracket(c: char) -> bool {
    BRACKET_PAIRS.iter().any(|&(o, _)| o == c)
}

fn is_close_bracket(c: char) -> bool {
    BRACKET_PAIRS.iter().any(|&(_, cl)| cl == c)
}

fn matching_close(c: char) -> Option<char> {
    BRACKET_PAIRS
        .iter()
        .find(|&&(o, _)| o == c)
        .map(|&(_, cl)| cl)
}

fn matching_open(c: char) -> Option<char> {
    BRACKET_PAIRS
        .iter()
        .find(|&&(_, cl)| cl == c)
        .map(|&(o, _)| o)
}

/// Start char-indices of every whole-word, case-sensitive occurrence of
/// `word` within `chars`. "Whole-word" means the char immediately before and
/// after the match is not a word char, so `foo` does not match inside
/// `foobar`. Mirrors VS Code's "Change All Occurrences" matching.
fn find_word_occurrences(chars: &[char], word: &[char]) -> Vec<usize> {
    let mut out = Vec::new();
    if word.is_empty() || word.len() > chars.len() {
        return out;
    }
    let last_start = chars.len() - word.len();
    let mut i = 0;
    while i <= last_start {
        if chars[i..i + word.len()] == *word {
            let before_ok = i == 0 || !is_word_char(chars[i - 1]);
            let after_idx = i + word.len();
            let after_ok = after_idx >= chars.len() || !is_word_char(chars[after_idx]);
            if before_ok && after_ok {
                out.push(i);
                i += word.len();
                continue;
            }
        }
        i += 1;
    }
    out
}

/// How a comment is written in JSX children (#1623), where `//` and a bare
/// `/* */` are text that React renders.
const JSX_COMMENT_OPEN: &str = "{/*";
/// The close of a JSX comment, matching [`JSX_COMMENT_OPEN`].
const JSX_COMMENT_CLOSE: &str = "*/}";

/// Whether the trimmed line is one `{/* … */}` comment.
fn is_jsx_comment(trimmed: &str) -> bool {
    trimmed.len() >= JSX_COMMENT_OPEN.len() + JSX_COMMENT_CLOSE.len()
        && trimmed.starts_with(JSX_COMMENT_OPEN)
        && trimmed.ends_with(JSX_COMMENT_CLOSE)
}

/// Whether the text at `byte` is JSX children: inside a JSX element's text,
/// or at the start of a child element, closing tag or `{…}` expression.
/// A position in an opening tag's attributes, or in the code inside a
/// `{…}`, is not.
fn jsx_child_starts_at(root: tree_sitter::Node, byte: usize) -> bool {
    let Some(mut node) = root.descendant_for_byte_range(byte, byte) else {
        return false;
    };
    loop {
        let Some(parent) = node.parent() else {
            return false;
        };
        if matches!(parent.kind(), "jsx_element" | "jsx_fragment") {
            match node.kind() {
                "jsx_text" => return true,
                "jsx_element"
                | "jsx_self_closing_element"
                | "jsx_expression"
                | "jsx_closing_element" => return node.start_byte() == byte,
                // The element's own opening tag: whether the element is
                // a child is a question for the element's parent.
                "jsx_opening_element" => {}
                _ => return false,
            }
        }
        node = parent;
    }
}

/// Whether the node at `byte`, or any node around it, is JSX or a parse
/// error.
fn in_jsx_or_error(root: tree_sitter::Node, byte: usize) -> bool {
    let mut node = root.descendant_for_byte_range(byte, byte);
    while let Some(n) = node {
        if n.is_error() || n.kind().starts_with("jsx") {
            return true;
        }
        node = n.parent();
    }
    false
}

/// Start char-indices of every case-sensitive occurrence of `text` within
/// `chars`, as plain text and without overlap (#1558): what VS Code matches
/// for a selection made by hand, so `oo` is found inside `foo`.
fn find_text_occurrences(chars: &[char], text: &[char]) -> Vec<usize> {
    let mut out = Vec::new();
    if text.is_empty() || text.len() > chars.len() {
        return out;
    }
    let mut i = 0;
    while i + text.len() <= chars.len() {
        if chars[i..i + text.len()] == *text {
            out.push(i);
            i += text.len();
        } else {
            i += 1;
        }
    }
    out
}

/// The line-comment marker for a language (VS Code `lineComment`), or `None`
/// for languages with no line comment, in which case Toggle Line Comment is a
/// no-op.
fn line_comment_token(lang: Option<LangKind>) -> Option<&'static str> {
    match lang {
        Some(LangKind::Rust)
        | Some(LangKind::JavaScript)
        | Some(LangKind::TypeScript)
        | Some(LangKind::Tsx)
        | Some(LangKind::Go)
        | Some(LangKind::C)
        | Some(LangKind::Cpp)
        | Some(LangKind::Ql)
        | Some(LangKind::Dbscheme) => Some("//"),
        Some(LangKind::Python)
        | Some(LangKind::Yaml)
        | Some(LangKind::Toml)
        | Some(LangKind::Bash)
        | Some(LangKind::Ruby)
        | Some(LangKind::Make)
        | Some(LangKind::Dockerfile) => Some("#"),
        Some(LangKind::Java) | Some(LangKind::CSharp) => Some("//"),
        Some(LangKind::Lua) | Some(LangKind::Sql) => Some("--"),
        // JSONC, which croft's and VS Code's own settings files are: VS
        // Code's JSON language comments with `//` too.
        Some(LangKind::Json) => Some("//"),
        _ => None,
    }
}

/// The line comment of a file croft has no grammar for, by its file name or
/// extension, as VS Code's language configurations give them (#1181).
fn line_comment_token_for_path(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    if name.starts_with(".env")
        || matches!(
            name,
            ".gitignore"
                | ".dockerignore"
                | ".npmignore"
                | ".prettierignore"
                | ".gitattributes"
                | ".editorconfig"
                | "CODEOWNERS"
        )
    {
        return Some("#");
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "kt" | "kts" | "swift" | "scala" | "sc" | "dart" | "php" | "groovy" | "gradle"
        | "jsonc" | "json5" | "proto" | "zig" | "v" | "sv" | "fs" | "fsx" | "scss" | "sass"
        | "less" => Some("//"),
        "pl" | "pm" | "r" | "ps1" | "psm1" | "mk" | "conf" | "cfg" | "properties" | "env"
        | "tf" | "hcl" | "nix" | "jl" | "ex" | "exs" | "cmake" | "gitignore" | "zsh" | "fish"
        | "awk" | "tcl" | "nim" | "cr" | "graphql" | "gql" => Some("#"),
        "hs" | "elm" | "ada" | "adb" | "ads" | "vhd" | "vhdl" => Some("--"),
        "ini" | "clj" | "cljs" | "edn" | "lisp" | "el" | "scm" | "rkt" | "asm" => Some(";"),
        "tex" | "sty" | "erl" | "hrl" => Some("%"),
        "vim" => Some("\""),
        "bat" | "cmd" => Some("@REM"),
        _ => None,
    }
}

/// The byte length of the line comment `token` that `text` (a line with its
/// indentation trimmed) starts with, or `None` when it is not commented. A
/// word token (batch's `@REM`) must end at whitespace or the line's end, so
/// `REMOTE.EXE` is not a comment, and batch's plain `REM` counts too.
fn line_comment_prefix_len(text: &str, token: &str) -> Option<usize> {
    let word = |t: &str| {
        text.starts_with(t)
            && text[t.len()..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace)
    };
    if !token.ends_with(|c: char| c.is_ascii_alphabetic()) {
        return text.starts_with(token).then_some(token.len());
    }
    if word(token) {
        return Some(token.len());
    }
    let plain = token.trim_start_matches('@');
    (plain != token && word(plain)).then_some(plain.len())
}

/// The block-comment delimiters for a language (VS Code `blockComment`), or
/// `None` for languages without one (e.g. YAML), in which case Toggle Block
/// Comment is a no-op. Python has no real block comment, but VS Code maps the
/// command onto a triple-quoted string, so it returns `""" """`.
fn block_comment_tokens(lang: Option<LangKind>) -> Option<(&'static str, &'static str)> {
    match lang {
        Some(LangKind::Rust)
        | Some(LangKind::JavaScript)
        | Some(LangKind::TypeScript)
        | Some(LangKind::Tsx)
        | Some(LangKind::Go)
        | Some(LangKind::C)
        | Some(LangKind::Cpp)
        | Some(LangKind::Css)
        | Some(LangKind::Ql)
        | Some(LangKind::Dbscheme) => Some(("/*", "*/")),
        Some(LangKind::Html)
        | Some(LangKind::Markdown)
        | Some(LangKind::Xml)
        | Some(LangKind::Vue)
        | Some(LangKind::Svelte) => Some(("<!--", "-->")),
        Some(LangKind::Lua) => Some(("--[[", "]]")),
        Some(LangKind::Java) | Some(LangKind::CSharp) | Some(LangKind::Sql) => Some(("/*", "*/")),
        // Python has no true block comment; VS Code's language config maps
        // Toggle Block Comment onto a triple-quoted string (`""" """`).
        Some(LangKind::Python) => Some(("\"\"\"", "\"\"\"")),
        _ => None,
    }
}

/// Sort `(first, last)` row blocks and merge the ones that overlap, and
/// with `touching` also the ones that meet end to end.
fn merge_row_blocks(mut blocks: Vec<(usize, usize)>, touching: bool) -> Vec<(usize, usize)> {
    blocks.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(blocks.len());
    for (start, end) in blocks {
        match out.last_mut() {
            Some(last) if start <= last.1 + usize::from(touching) => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// How many rows the blocks that `keep` picks span.
fn rows_in_blocks(blocks: &[(usize, usize)], keep: impl Fn(&(usize, usize)) -> bool) -> usize {
    blocks
        .iter()
        .filter(|b| keep(b))
        .map(|&(start, end)| end - start + 1)
        .sum()
}

/// Where a caret at char column `col` lands after `delta` chars are
/// inserted (positive) or removed (negative) at column `at` of its line.
fn shift_col(col: usize, at: usize, delta: isize) -> usize {
    let n = delta.unsigned_abs();
    if delta >= 0 {
        if col >= at { col + n } else { col }
    } else if col >= at + n {
        col - n
    } else {
        col.min(at)
    }
}

/// The position just after `text` when it is written at `start`.
fn text_end(start: (usize, usize), text: &str) -> (usize, usize) {
    match text.rsplit_once('\n') {
        Some((head, tail)) => (
            start.0 + head.matches('\n').count() + 1,
            tail.chars().count(),
        ),
        None => (start.0, start.1 + text.chars().count()),
    }
}

/// Title-case a string: the first alphanumeric of each run is upper-cased,
/// the rest lower-cased, separators preserved. Used by Transform to Title
/// Case. An apostrophe (`'` or `’`) right after a letter stays inside the
/// word, so `don't` and `journey's` keep a lower-case tail (#1016); one at
/// the start of a word (`'tis`) still lets the next letter start it, as in
/// VS Code.
fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_alnum = false;
    let mut prev_letter = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if prev_alnum {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_alnum = true;
            prev_letter = c.is_alphabetic();
        } else if matches!(c, '\'' | '\u{2019}') && prev_letter {
            // `prev_alnum` stays set: the letter after it continues the word.
            out.push(c);
            prev_letter = false;
        } else {
            out.push(c);
            prev_alnum = false;
            prev_letter = false;
        }
    }
    out
}

/// Rewrite every identifier in `s` in the naming style `kind` names, Snake,
/// Camel, Pascal or Kebab (#1573). An identifier is a run of letters,
/// digits, `_` and `-`; everything between runs (spaces, punctuation, line
/// breaks) is kept, as are a run's leading and trailing `_` and `-`
/// (`_private`). A run with no letter is left alone.
fn identifier_case(s: &str, kind: CaseTransform) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if !run.is_empty() {
            out.push_str(&case_run(run, kind));
            run.clear();
        }
    };
    for c in s.chars() {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// One identifier run in `kind`'s style; see [`identifier_case`].
fn case_run(run: &str, kind: CaseTransform) -> String {
    let edge = |c: char| c == '_' || c == '-';
    let core = run.trim_matches(edge);
    if !core.chars().any(char::is_alphabetic) {
        return run.to_string();
    }
    let lead = &run[..run.len() - run.trim_start_matches(edge).len()];
    let trail = &run[run.trim_end_matches(edge).len()..];
    let words = case_words(core);
    let cap = |w: &str| {
        let mut chars = w.chars();
        chars.next().map_or_else(String::new, |f| {
            f.to_uppercase()
                .chain(chars.flat_map(char::to_lowercase))
                .collect()
        })
    };
    let body = match kind {
        CaseTransform::Snake | CaseTransform::Kebab => {
            let sep = if kind == CaseTransform::Snake {
                "_"
            } else {
                "-"
            };
            words
                .iter()
                .map(|w| w.to_lowercase())
                .collect::<Vec<_>>()
                .join(sep)
        }
        CaseTransform::Camel => words
            .iter()
            .enumerate()
            .map(|(i, w)| if i == 0 { w.to_lowercase() } else { cap(w) })
            .collect(),
        _ => words.iter().map(|w| cap(w)).collect(),
    };
    format!("{lead}{body}{trail}")
}

/// The words of an identifier: split at `_` and `-`, and where the case
/// changes as VS Code's transforms read it: a lower-case letter before an
/// upper-case one (`foo|Bar`), and before the last capital of a run of
/// capitals or digits that a lower-case letter follows (`HTTP|Server`,
/// `Base64|Encoder`).
fn case_words(core: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in core.split(['_', '-']).filter(|p| !p.is_empty()) {
        let chars: Vec<char> = part.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let prev = i.checked_sub(1).map(|j| chars[j]);
            let next = chars.get(i + 1).copied();
            let boundary = c.is_uppercase()
                && prev.is_some_and(|p| {
                    p.is_lowercase()
                        || ((p.is_uppercase() || p.is_numeric())
                            && next.is_some_and(char::is_lowercase))
                });
            if boundary && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            word.push(c);
        }
        if !word.is_empty() {
            words.push(word);
        }
    }
    words
}

/// Most rulers kept (#1524). A repository's `.croft/config.json` may set
/// `rulers`, and the painter walks them for every visible row, so the list
/// is bounded before it reaches an editor.
pub const MAX_RULERS: usize = 16;

/// The `rulers` setting as an editor keeps it: ascending, without
/// duplicates, and at most `MAX_RULERS` columns.
pub fn normalize_rulers(rulers: &[u16]) -> Vec<u16> {
    let mut out = rulers.to_vec();
    out.sort_unstable();
    out.dedup();
    out.truncate(MAX_RULERS);
    out
}

/// Whitespace rendering (#133, VS Code `editor.renderWhitespace`): which
/// spaces/tabs paint a visible glyph (`·` / `→`). `Selection` — VS Code's
/// default — shows them only inside selected text; `All` across the whole
/// buffer; `None` never. The palette command cycles Selection → All → None.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhitespaceMode {
    None,
    #[default]
    Selection,
    All,
}

impl WhitespaceMode {
    /// The persisted config id (`render_whitespace` in config.json).
    pub fn pref_id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Selection => "selection",
            Self::All => "all",
        }
    }

    /// Parse the persisted id; anything unrecognised (including the empty
    /// string an older config deserializes to) is the VS Code default.
    pub fn from_pref(id: &str) -> Self {
        match id {
            "none" => Self::None,
            "all" => Self::All,
            _ => Self::Selection,
        }
    }

    /// Settings/status label.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Selection => "selection",
            Self::All => "all",
        }
    }

    /// The palette toggle's cycle order (VS Code default first).
    pub fn next(self) -> Self {
        match self {
            Self::Selection => Self::All,
            Self::All => Self::None,
            Self::None => Self::Selection,
        }
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

fn indent_unit_for(lang: Option<LangKind>) -> &'static str {
    match lang {
        Some(LangKind::Yaml) => "  ",
        _ => "    ",
    }
}

/// The base a numeric literal is written in. Octal is deliberately absent:
/// vim reads a bare leading zero as octal, which silently turns `019` into
/// an error and `010` into `011` meaning nine, and every modern editor that
/// copied Ctrl-A dropped that behaviour with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NumberRadix {
    Dec,
    Hex,
    Bin,
}

/// One numeric literal located on a line: the byte range it occupies and how
/// to read it. `start` includes a `-` sign and a `0x` / `0b` prefix, so the
/// range is exactly the text a bump replaces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct NumberToken {
    start: usize,
    end: usize,
    radix: NumberRadix,
}

impl NumberToken {
    /// The literal rewritten with `delta` added, preserving how it was
    /// written. `None` when the result cannot be represented — an overflow,
    /// or a negative hex/binary value, which is clamped rather than wrapped
    /// (vim turns `0x0` into `0xffffffffffffffff`, which is never what the
    /// keystroke meant).
    fn bumped(&self, line: &str, delta: i64) -> Option<String> {
        let text = &line[self.start..self.end];
        match self.radix {
            NumberRadix::Dec => {
                let value: i64 = text.parse().ok()?;
                let next = value.checked_add(delta)?;
                // Zero padding is a fact about the literal, not about its
                // value: `007` is a version segment or a port, and widening
                // it on every bump would be wrong.
                let digits = text.strip_prefix('-').unwrap_or(text).len();
                let body = next.unsigned_abs();
                let sign = if next < 0 { "-" } else { "" };
                Some(format!("{sign}{body:0digits$}"))
            }
            NumberRadix::Hex | NumberRadix::Bin => {
                let (prefix, digits) = text.split_at(2);
                let radix = if self.radix == NumberRadix::Hex {
                    16
                } else {
                    2
                };
                let value = u64::from_str_radix(digits, radix).ok()?;
                let next = match value.checked_add_signed(delta) {
                    Some(v) => v,
                    // Underflow clamps at zero rather than wrapping: vim
                    // turns `0x0` into `0xffffffffffffffff`, which is never
                    // what the keystroke meant.
                    None if delta < 0 => 0,
                    None => return None,
                };
                let width = digits.len();
                let body = match self.radix {
                    NumberRadix::Bin => format!("{next:0width$b}"),
                    // Digit case follows the literal: a file that writes
                    // `0xFF` keeps writing uppercase.
                    _ if digits.chars().any(|c| c.is_ascii_uppercase()) => {
                        format!("{next:0width$X}")
                    }
                    _ => format!("{next:0width$x}"),
                };
                Some(format!("{prefix}{body}"))
            }
        }
    }
}

/// The literal the cursor sits inside, or failing that the next one to its
/// right on the same line — vim's rule, and the reason Ctrl-A is usable
/// without placing the caret exactly.
fn number_token_at_or_after(line: &str, caret: usize) -> Option<NumberToken> {
    // Tokens come out left to right and never overlap, so the first one
    // ending at or after the caret is the one under it, or the next one
    // along when the caret is between literals.
    number_tokens(line).into_iter().find(|t| t.end >= caret)
}

/// Every numeric literal on the line, left to right and non-overlapping.
fn number_tokens(line: &str) -> Vec<NumberToken> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        // A digit that continues an identifier (`utf8`, `sha256`) is still a
        // number to vim, and bumping it is usually what was meant. What must
        // NOT happen is reading the `8` of `utf8` as the start of a fresh
        // token when we are already inside one, which the scan below avoids
        // by consuming each token whole.
        if let Some(tok) = radix_prefixed_token(b, i) {
            i = tok.end;
            out.push(tok);
            continue;
        }
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            // A `-` directly before the digits is the sign, as in vim, but
            // only when it is not itself preceded by a digit or an
            // identifier character: in `a-5` the `-` is a sign, in `x1-5`
            // it is a subtraction and `5` is positive.
            let signed = start > 0
                && b[start - 1] == b'-'
                && !line[..start - 1]
                    .chars()
                    .next_back()
                    .is_some_and(is_word_or_digit);
            out.push(NumberToken {
                start: if signed { start - 1 } else { start },
                end: i,
                radix: NumberRadix::Dec,
            });
            continue;
        }
        i += 1;
    }
    out
}

/// `0x…` / `0b…` starting exactly at `i`, or None.
fn radix_prefixed_token(b: &[u8], i: usize) -> Option<NumberToken> {
    if b[i] != b'0' || i + 2 >= b.len() {
        return None;
    }
    let (radix, valid): (NumberRadix, fn(u8) -> bool) = match b[i + 1] {
        b'x' | b'X' => (NumberRadix::Hex, |c: u8| c.is_ascii_hexdigit()),
        b'b' | b'B' => (NumberRadix::Bin, |c: u8| c == b'0' || c == b'1'),
        _ => return None,
    };
    let mut j = i + 2;
    while j < b.len() && valid(b[j]) {
        j += 1;
    }
    if j == i + 2 {
        return None;
    }
    Some(NumberToken {
        start: i,
        end: j,
        radix,
    })
}

/// Checked on the character, not the byte: before the `-` in `λ-5` sits a
/// UTF-8 continuation byte, which no byte test reads as a letter.
fn is_word_or_digit(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The Emmet dialect for a buffer, or `None` where expansion is not offered,
/// matching VS Code's default `emmet.includeLanguages`. `.jsx` shares
/// `LangKind::JavaScript` with plain `.js`, so the extension tells them
/// apart: an abbreviation in ordinary JavaScript is code, not markup.
fn emmet_profile_for(
    lang: Option<LangKind>,
    path: Option<&std::path::Path>,
) -> Option<crate::emmet::Profile> {
    use crate::emmet::Profile;
    match lang {
        Some(LangKind::Html | LangKind::Vue | LangKind::Svelte) => Some(Profile::Html),
        Some(LangKind::Xml) => Some(Profile::Xml),
        Some(LangKind::Tsx) => Some(Profile::Jsx),
        Some(LangKind::JavaScript)
            if path
                .and_then(|p| p.extension())
                .is_some_and(|e| e.eq_ignore_ascii_case("jsx")) =>
        {
            Some(Profile::Jsx)
        }
        _ => None,
    }
}

/// The VS Code language id for `lang`, used to scope user snippets (their
/// `scope` field names these). Lowercase, matching VS Code's identifiers so a
/// config copied from there just works.
pub fn language_scope_id(lang: Option<LangKind>) -> &'static str {
    match lang {
        None => "plaintext",
        Some(LangKind::Rust) => "rust",
        Some(LangKind::Python) => "python",
        Some(LangKind::JavaScript) => "javascript",
        Some(LangKind::TypeScript) => "typescript",
        Some(LangKind::Tsx) => "typescriptreact",
        Some(LangKind::Json) => "json",
        Some(LangKind::Xml) => "xml",
        Some(LangKind::Toml) => "toml",
        Some(LangKind::Yaml) => "yaml",
        Some(LangKind::Markdown) => "markdown",
        Some(LangKind::Go) => "go",
        Some(LangKind::Html) => "html",
        Some(LangKind::Css) => "css",
        Some(LangKind::Bash) => "shellscript",
        Some(LangKind::C) => "c",
        Some(LangKind::Cpp) => "cpp",
        Some(LangKind::Lua) => "lua",
        Some(LangKind::Ql) => "ql",
        Some(LangKind::Dbscheme) => "dbscheme",
        Some(LangKind::Java) => "java",
        Some(LangKind::Ruby) => "ruby",
        Some(LangKind::CSharp) => "csharp",
        Some(LangKind::Sql) => "sql",
        Some(LangKind::Make) => "makefile",
        Some(LangKind::Dockerfile) => "dockerfile",
        Some(LangKind::Vue) => "vue",
        Some(LangKind::Svelte) => "svelte",
    }
}

/// Every selectable language mode, in the order the status-bar picker lists
/// them. `None` (Plain Text) is offered separately by the picker.
pub const SELECTABLE_LANGUAGES: &[LangKind] = &[
    LangKind::Rust,
    LangKind::Python,
    LangKind::JavaScript,
    LangKind::TypeScript,
    LangKind::Tsx,
    LangKind::Json,
    LangKind::Toml,
    LangKind::Yaml,
    LangKind::Markdown,
    LangKind::Go,
    LangKind::Html,
    LangKind::Css,
    LangKind::Bash,
    LangKind::C,
    LangKind::Cpp,
    LangKind::Lua,
    LangKind::Ql,
    LangKind::Dbscheme,
    LangKind::Java,
    LangKind::Ruby,
    LangKind::CSharp,
    LangKind::Sql,
    LangKind::Make,
    LangKind::Dockerfile,
    LangKind::Vue,
    LangKind::Svelte,
];

/// Number of leading whitespace bytes to strip for one outdent step, matching
/// VS Code's tab-stop-aligned `unshiftIndent`. A leading tab counts as one
/// level (strip it). For spaces, strip back to the previous tab stop: with a
/// width-4 unit, 4 -> remove 4, 6 -> remove 2, 3 -> remove 3, 1 -> remove 1.
/// All leading whitespace here is ASCII (`' '` / `'\t'`), so byte count equals
/// char count.
fn leading_outdent_chars(line: &str, unit_w: usize) -> usize {
    if unit_w == 0 {
        return 0;
    }
    if line.starts_with('\t') {
        return 1;
    }
    let spaces = line.chars().take_while(|c| *c == ' ').count();
    if spaces == 0 {
        0
    } else {
        ((spaces - 1) % unit_w) + 1
    }
}

/// What Enter types inside a doc comment (#1443), after VS Code's
/// `onEnterRules`.
#[derive(Debug, PartialEq)]
struct CommentEnter {
    /// The new line's text, ahead of whatever followed the caret.
    start: String,
    /// What happens on the line below the new one.
    below: CommentBelow,
}

#[derive(Debug, PartialEq)]
enum CommentBelow {
    Nothing,
    /// A `/**` was just opened with nothing closing it: this line closes it.
    AddCloser(String),
    /// The caret sat right before the `*/` of `/** */`: that `*/` moves to
    /// a line of its own, indented by this.
    MoveCloser(String),
}

/// The languages whose doc comments are `/** … */` blocks.
fn has_block_doc_comments(lang: Option<LangKind>) -> bool {
    matches!(
        lang,
        Some(
            LangKind::JavaScript
                | LangKind::TypeScript
                | LangKind::Tsx
                | LangKind::C
                | LangKind::Cpp
                | LangKind::Java
                | LangKind::CSharp
                | LangKind::Rust
        )
    )
}

/// How Enter at char `col` of `line` continues a doc comment (#1443), or
/// None where it is a plain line break:
/// - `/**` opening a block gives ` * `, closing the block below when
///   nothing does yet;
/// - a ` * …` line inside the block gives the next ` * ` at the same
///   column;
/// - a ` */` line steps back to the column of its `/**`;
/// - in Rust, a `///` or `//!` line gives the next `/// ` or `//! `.
///
/// A plain `//` comment, as in VS Code, is not continued.
fn comment_on_enter(
    lang: Option<LangKind>,
    line: &str,
    col: usize,
    below: &[String],
) -> Option<CommentEnter> {
    let before: String = line.chars().take(col).collect();
    let after: String = line.chars().skip(col).collect();
    let indent: String = before
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let body = &before[indent.len()..];
    let nothing = |start: String| {
        Some(CommentEnter {
            start,
            below: CommentBelow::Nothing,
        })
    };
    if lang == Some(LangKind::Rust) && !body.starts_with("////") {
        for intro in ["///", "//!"] {
            if body.starts_with(intro) {
                return nothing(format!("{indent}{intro} "));
            }
        }
    }
    if !has_block_doc_comments(lang) {
        return None;
    }
    let closer_next = after.trim_start().starts_with("*/");
    if let Some(rest) = body.strip_prefix("/**")
        && !rest.starts_with('/')
        && !rest.contains("*/")
    {
        let below = if closer_next {
            CommentBelow::MoveCloser(format!("{indent} "))
        } else if after.contains("*/") || block_closed_below(below) {
            // Already closed, on this line or further down.
            CommentBelow::Nothing
        } else {
            CommentBelow::AddCloser(format!("{indent} */"))
        };
        return Some(CommentEnter {
            start: format!("{indent} * "),
            below,
        });
    }
    // Only a `*` set in from its line's start is a block's continuation, as
    // VS Code's rule has it.
    if !indent.ends_with(' ') {
        return None;
    }
    if let Some(rest) = body.strip_prefix('*')
        && (rest.is_empty() || rest.starts_with(' '))
        && !rest.contains("*/")
        && !closer_next
    {
        return nothing(format!("{indent}* "));
    }
    if body.trim_end() == "*/" && after.trim().is_empty() {
        return nothing(indent[..indent.len() - 1].to_string());
    }
    None
}

/// Whether the lines below an opened block comment close it: a `*/`
/// comes before any further `/*` would open another block.
fn block_closed_below(below: &[String]) -> bool {
    for line in below {
        match (line.find("/*"), line.find("*/")) {
            (open, Some(close)) if open.is_none_or(|open| close < open) => return true,
            (Some(_), _) => return false,
            _ => {}
        }
    }
    false
}

fn extra_indent_triggered(lang: Option<LangKind>, last_non_ws: Option<char>) -> bool {
    let last = match last_non_ws {
        Some(c) => c,
        None => return false,
    };
    match lang {
        Some(LangKind::Python) => matches!(last, ':' | '(' | '[' | '{'),
        Some(LangKind::Rust)
        | Some(LangKind::JavaScript)
        | Some(LangKind::TypeScript)
        | Some(LangKind::Tsx)
        | Some(LangKind::Json)
        | Some(LangKind::Go)
        | Some(LangKind::Css)
        | Some(LangKind::Lua)
        | Some(LangKind::Ql)
        | Some(LangKind::Java)
        | Some(LangKind::CSharp) => matches!(last, '(' | '[' | '{'),
        _ => false,
    }
}

/// What Enter does on a line a per-language rule recognises, in place of
/// copying the indent (#1495), as the Markdown editors people come from do.
/// Doc comments (#1443) go through `comment_on_enter`.
#[derive(Debug, PartialEq, Eq)]
enum OnEnter {
    /// Open a new line starting with this text: the indent plus the list
    /// marker or quote that continues the line.
    Continue(String),
    /// Enter on a list item or quote that holds only its marker: clear the
    /// marker, which ends the list.
    EndList,
}

/// The Enter rule for the caret at byte `col` of `lines[row]`, if any.
fn on_enter_rule(
    lang: Option<LangKind>,
    lines: &[String],
    row: usize,
    col: usize,
) -> Option<OnEnter> {
    let line = lines.get(row)?;
    match lang? {
        LangKind::Markdown if !in_markdown_fence(lines, row) => markdown_on_enter(line, col),
        _ => None,
    }
}

/// True when `row` sits inside a fenced code block opened above it. A fence
/// opens with three or more `` ` `` or `~` and closes only on a line of the
/// same character, at least as long, with nothing after it.
fn in_markdown_fence(lines: &[String], row: usize) -> bool {
    let mut open: Option<(char, usize)> = None;
    for l in &lines[..row] {
        let t = l.trim_start();
        let Some(ch) = t.chars().next().filter(|c| matches!(c, '`' | '~')) else {
            continue;
        };
        let width = t.len() - t.trim_start_matches(ch).len();
        if width < 3 {
            continue;
        }
        match open {
            None => open = Some((ch, width)),
            Some((c, w)) if c == ch && width >= w && t[width..].trim().is_empty() => open = None,
            Some(_) => {}
        }
    }
    open.is_some()
}

/// Enter on a Markdown list item, task item or blockquote (#1495): the next
/// line repeats the indent, the `>` quotes and the marker, with an ordered
/// number counted on and a task box left unticked. An item that is only its
/// marker ends the list instead. A caret inside the marker, a thematic break
/// (`* * *`) or emphasis (`**bold**`) is no list.
fn markdown_on_enter(line: &str, col: usize) -> Option<OnEnter> {
    let trimmed = line.trim();
    if trimmed.len() >= 3
        && let Some(first) = trimmed.chars().next()
        && matches!(first, '-' | '*' | '_')
        && trimmed.chars().all(|c| c == first || c == ' ' || c == '\t')
    {
        return None;
    }
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let mut i = indent;
    let mut quote = String::new();
    while line[i..].starts_with('>') {
        quote.push('>');
        i += 1;
        if line[i..].starts_with(' ') {
            quote.push(' ');
            i += 1;
        }
    }
    let spaced = |at: usize| line[at..].starts_with([' ', '\t']);
    let rest = &line[i..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let mut marker = None;
    if rest.starts_with(['-', '*', '+']) && spaced(i + 1) {
        marker = Some(rest[..1].to_string());
        i += 1;
    } else if (1..=9).contains(&digits)
        && rest[digits..].starts_with(['.', ')'])
        && spaced(i + digits + 1)
    {
        let n: u64 = rest[..digits].parse().ok()?;
        marker = Some(format!("{}{}", n + 1, &rest[digits..=digits]));
        i += digits + 1;
    }
    let mut next = format!("{}{quote}", &line[..indent]);
    if let Some(marker) = marker {
        let gap = line[i..].len() - line[i..].trim_start_matches([' ', '\t']).len();
        next.push_str(&marker);
        next.push_str(&line[i..i + gap]);
        i += gap;
        let after = &line[i..];
        if matches!(after.get(..3), Some("[ ]" | "[x]" | "[X]"))
            && (after.len() == 3 || spaced(i + 3))
        {
            next.push_str("[ ] ");
            i += 3;
            i += line[i..].len() - line[i..].trim_start_matches([' ', '\t']).len();
        }
    } else if quote.is_empty() {
        return None;
    }
    if col < i.min(line.trim_end().len()) {
        return None;
    }
    if line[i.min(line.len())..].trim().is_empty() {
        return Some(OnEnter::EndList);
    }
    Some(OnEnter::Continue(next))
}

/// The indent for the line Enter opens after `prefix` in YAML (#1309).
/// A key with no value (`services:`), a block scalar (`run: |`) or an open
/// flow collection (`args: [`) starts a nested block one `unit` deeper. In a
/// list item (`- name: x`) the base column is the item's content, so the
/// next key lines up with `name`; anything else keeps the line's indent.
fn yaml_newline_indent(prefix: &str, unit: &str) -> String {
    let leading: String = prefix
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let mut content = &prefix[leading.len()..];
    let mut base = leading.clone();
    while let Some(rest) = content.strip_prefix('-')
        && (rest.is_empty() || rest.starts_with(' '))
    {
        let after = rest.trim_start_matches(' ');
        base.push_str(&" ".repeat(content.len() - after.len()));
        content = after;
    }
    let content = yaml_strip_comment(content).trim_end();
    let Some(colon) = yaml_key_colon(content) else {
        return leading;
    };
    let mut value = content[colon + 1..].trim();
    // An anchor or tag (`&defaults`, `!!map`) comes before the node it names,
    // so `.defaults: &defaults` still opens the mapping below it, as shared
    // blocks in GitLab CI, compose and Helm values are written.
    while let Some(prop) = value.split_whitespace().next()
        && prop.starts_with(['&', '!'])
    {
        value = value[prop.len()..].trim_start();
    }
    let opens_block = value.is_empty()
        || value.ends_with('[')
        || value.ends_with('{')
        || (value.starts_with(['|', '>'])
            && value[1..]
                .chars()
                .all(|c| matches!(c, '-' | '+') || c.is_ascii_digit()));
    if opens_block {
        format!("{base}{unit}")
    } else {
        base
    }
}

/// `line` up to a `#` comment: a `#` at the start or after whitespace,
/// outside quotes.
fn yaml_strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut prev_ws = true;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' && prev_ws => return &line[..i],
            None => {}
        }
        prev_ws = c.is_whitespace();
    }
    line
}

/// Byte offset of the `:` that ends a mapping key in `content`: the first
/// one outside quotes followed by a space or the end of the line.
fn yaml_key_colon(content: &str) -> Option<usize> {
    let mut quote = None;
    let mut chars = content.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if (c == '\'' || c == '"') && i == 0 => quote = Some(c),
            None if c == ':' && chars.peek().is_none_or(|(_, n)| n.is_whitespace()) => {
                return Some(i);
            }
            None => {}
        }
    }
    None
}

fn is_bracket_pair_split(lang: Option<LangKind>, prev: Option<char>, next: Option<char>) -> bool {
    let bracket_aware = matches!(
        lang,
        Some(LangKind::Rust)
            | Some(LangKind::Python)
            | Some(LangKind::JavaScript)
            | Some(LangKind::TypeScript)
            | Some(LangKind::Tsx)
            | Some(LangKind::Json)
            | Some(LangKind::Go)
            | Some(LangKind::Css)
            | Some(LangKind::Lua)
            | Some(LangKind::Ql)
            | Some(LangKind::Java)
            | Some(LangKind::CSharp)
    );
    if !bracket_aware {
        return false;
    }
    matches!(
        (prev, next),
        (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')'))
    )
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

fn set_tab_cells(n: usize) {
    TAB_CELLS.with(|t| t.set(n.max(1)));
}

/// Screen cells a char takes in the editor: its display width, with a tab
/// drawn as the buffer's Tab Size blank cells.
fn char_cells(c: char) -> usize {
    if c == '\t' {
        TAB_CELLS.with(std::cell::Cell::get)
    } else {
        unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
    }
}

/// Cells a tab takes when it starts at display cell `col` of its row: up to
/// the next multiple of Tab Size, as in vim and VS Code, so text after a tab
/// lines up whatever came before it.
fn tab_cells_at(col: usize) -> usize {
    let t = TAB_CELLS.with(std::cell::Cell::get).max(1);
    t - col % t
}

/// Cells `c` takes when it starts at display cell `col`.
fn char_cells_at(c: char, col: usize) -> usize {
    if c == '\t' {
        tab_cells_at(col)
    } else {
        char_cells(c)
    }
}

/// Cells the grapheme cluster `cluster` takes when it starts at display cell
/// `col`. ratatui paints a cluster as one glyph sized by the whole cluster
/// (`crate::cell_map`): `👍🏽` is two cells, not four, and `❤️` two, not one.
/// A lone character keeps [`char_cells_at`], so a tab runs to the next stop
/// (a tab is always a cluster of its own).
fn cluster_cells_at(cluster: &str, col: usize) -> usize {
    let mut chars = cluster.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => char_cells_at(c, col),
        (Some(_), Some(_)) => unicode_width::UnicodeWidthStr::width(cluster),
        (None, _) => 0,
    }
}

/// Each character of `text` with the cells it takes when `text` starts at
/// display cell `origin` of its row: a cluster's first character takes the
/// whole cluster's cells ([`cluster_cells_at`]) and the rest none. Everything
/// that measures text for tab stops goes through here, so the drawing, the
/// caret and clicks agree on where a tab after an emoji ends.
fn char_cells_from(text: &str, origin: usize) -> impl Iterator<Item = (char, usize)> + '_ {
    use unicode_segmentation::UnicodeSegmentation;
    let mut col = origin;
    text.graphemes(true).flat_map(move |cluster| {
        let w = cluster_cells_at(cluster, col);
        col += w;
        cluster
            .chars()
            .enumerate()
            .map(move |(i, c)| (c, if i == 0 { w } else { 0 }))
    })
}

/// Cells `text` takes from display cell `origin` of its row on.
fn cells_from(text: &str, origin: usize) -> usize {
    char_cells_from(text, origin).map(|(_, w)| w).sum()
}

/// Display cells of the first `chars` characters of `line`, from the line's
/// left edge: where its character number `chars` starts.
fn cells_before(line: &str, chars: usize) -> usize {
    cells_from(&line[..byte_index_of_char(line, chars)], 0)
}

/// `text` as the editor paints it: each tab as the blank cells it takes
/// ([`char_cells`]), which the caret, selection, clicks and every overlay
/// count for it. ratatui drops control characters, so a raw tab took no
/// cell and the text after it slid left under all of them (#891).
fn blank_tabs(text: &str) -> std::borrow::Cow<'_, str> {
    blank_tabs_from(text, 0)
}

/// [`blank_tabs`] for text that starts at display cell `origin` of its row,
/// so each tab fills up to the next tab stop.
fn blank_tabs_from(text: &str, origin: usize) -> std::borrow::Cow<'_, str> {
    if !text.contains('\t') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for (c, w) in char_cells_from(text, origin) {
        if c == '\t' {
            out.extend(std::iter::repeat_n(' ', w));
        } else {
            out.push(c);
        }
    }
    out.into()
}

/// Shift highlight spans left by `byte_start`, dropping spans that fall
/// entirely before the cut and clamping spans straddling the cut.
/// Split file text into buffer lines the way the LSP spec / VS Code / Zed do:
/// `\r\n`, a lone `\r`, and `\n` are all line terminators. Rust's `str::lines`
/// only breaks on `\n` (stripping a trailing `\r`), so a stray lone `\r` would
/// leave croft's line count one short of the language server's. Every LSP
/// position past that `\r` (semantic tokens, diagnostics, hover, definition)
/// would then resolve one row off. Normalizing first keeps the two in lockstep
/// and is a no-op for clean `\n`-only files.
pub(crate) fn split_into_lines(text: &str) -> Vec<String> {
    // Most text has no `\r` at all; skip the two whole-text copies that
    // normalising would make (#371 splits a 35k-line file per scrub step).
    if !text.contains('\r') {
        return text.lines().map(str::to_string).collect();
    }
    normalize_newlines(text)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

/// The edits that re-indent a paste of `text` made at `(row, col)` (#1218):
/// every later line moves under `base` (the caret line's indentation) by its
/// indentation relative to the least-indented line, written in the buffer's
/// style. The first line counts toward that least indentation when the paste
/// starts in the caret line's leading whitespace, and then keeps only its
/// indentation beyond that least (the caret's column already indents it). Lines that are
/// only whitespace are emptied; a final segment that is only whitespace is
/// left alone, as the rest of the caret's line follows it.
fn paste_reindent_edits(
    text: &str,
    row: usize,
    col: usize,
    base: &str,
    at_indent: bool,
    tab: u32,
    spaces: bool,
) -> Vec<TextSpanEdit> {
    let tab = tab.max(1) as usize;
    let segs: Vec<&str> = text.split('\n').collect();
    let n = segs.len() - 1;
    let ws_of = |l: &str| -> (usize, usize) {
        let mut chars = 0;
        let mut width = 0;
        for c in l.chars() {
            match c {
                ' ' => width += 1,
                '\t' => width += tab - width % tab,
                _ => break,
            }
            chars += 1;
        }
        (chars, width)
    };
    let blank = |l: &str| l.trim().is_empty();
    let counted = segs
        .iter()
        .enumerate()
        .filter(|(i, l)| (*i > 0 || at_indent) && !blank(l))
        .map(|(_, l)| ws_of(l).1);
    let Some(least) = counted.min() else {
        return Vec::new();
    };
    let render = |width: usize| match spaces {
        true => " ".repeat(width),
        false => format!("{}{}", "\t".repeat(width / tab), " ".repeat(width % tab)),
    };
    let edit = |r: usize, from: usize, to: usize, new_text: String| TextSpanEdit {
        start: (r, from),
        end: (r, to),
        new_text,
        utf16: false,
    };
    let mut edits = Vec::new();
    let (first_chars, first_width) = ws_of(segs[0]);
    if at_indent && first_chars > 0 {
        let keep = match blank(segs[0]) {
            true => String::new(),
            false => render(first_width - least),
        };
        if segs[0].chars().take(first_chars).collect::<String>() != keep {
            edits.push(edit(row, col, col + first_chars, keep));
        }
    }
    for (i, seg) in segs.iter().enumerate().skip(1) {
        let (chars, width) = ws_of(seg);
        let new_ws = match blank(seg) {
            true if i == n => continue,
            true => String::new(),
            false => format!("{base}{}", render(width - least)),
        };
        let whole = if blank(seg) {
            seg.chars().count()
        } else {
            chars
        };
        if seg.chars().take(whole).collect::<String>() != new_ws {
            edits.push(edit(row + i, 0, whole, new_ws));
        }
    }
    edits
}

fn shift_spans_for_view(spans: &[HiSpan], byte_start: usize) -> Vec<HiSpan> {
    let mut out = Vec::with_capacity(spans.len());
    for sp in spans {
        let range = sp.range();
        if range.end <= byte_start {
            continue;
        }
        out.push(HiSpan::new(
            range.start.saturating_sub(byte_start),
            range.end - byte_start,
            sp.style,
        ));
    }
    out
}

/// Overlay `over` onto `base` (both sorted, non-overlapping, byte offsets
/// within one line) and return a sorted, non-overlapping span list. Where
/// the two cover the same bytes, `over` wins; `base` shows through in the
/// gaps. This is the per-line realization of the VS Code / Zed "combined"
/// rule: semantic tokens repaint the bytes they resolve, tree-sitter
/// syntax fills the rest.
/// Resolve a span's foreground to RGB for the minimap; non-RGB colors
/// (named/indexed/reset) fall back to the editor's default text color.
fn span_rgb(style: ratatui::style::Style, default: (u8, u8, u8)) -> (u8, u8, u8) {
    match style.fg {
        Some(ratatui::style::Color::Rgb(r, g, b)) => (r, g, b),
        _ => default,
    }
}

fn merge_overlay(base: &[HiSpan], over: &[HiSpan]) -> Vec<HiSpan> {
    if over.is_empty() {
        return base.to_vec();
    }
    let mut out: Vec<HiSpan> = Vec::with_capacity(base.len() + over.len());
    for b in base {
        // Emit the parts of this base span not covered by any overlay span.
        let mut cur = b.start;
        for o in over.iter().filter(|o| o.end > b.start && o.start < b.end) {
            if o.start > cur {
                out.push(HiSpan {
                    start: cur,
                    end: o.start,
                    style: b.style,
                });
            }
            cur = cur.max(o.end);
            if cur >= b.end {
                break;
            }
        }
        if cur < b.end {
            out.push(HiSpan {
                start: cur,
                end: b.end,
                style: b.style,
            });
        }
    }
    // The overlay normally wins outright, except over a comment tag (`TODO`,
    // `FIXME`, ...), which the tag pass deliberately marked so it stands out.
    // A server that reports the whole comment as one `comment` token would
    // otherwise repaint the tag in ordinary comment grey, so tag highlighting
    // would silently stop the moment an LSP attached. Carve those ranges out
    // of each overlay span instead. Keywords are BOLD as well, so the tag
    // test is the tag hue, not BOLD alone.
    let keep: Vec<&HiSpan> = base
        .iter()
        .filter(|b| crate::highlight::is_comment_tag_style(b.style))
        .collect();
    for o in over {
        let mut cur = o.start;
        for k in keep.iter().filter(|k| k.end > o.start && k.start < o.end) {
            if k.start > cur {
                out.push(HiSpan {
                    start: cur,
                    end: k.start,
                    style: o.style,
                });
            }
            out.push(HiSpan {
                start: k.start.max(o.start),
                end: k.end.min(o.end),
                style: k.style,
            });
            cur = cur.max(k.end);
            if cur >= o.end {
                break;
            }
        }
        if cur < o.end {
            out.push(HiSpan {
                start: cur,
                end: o.end,
                style: o.style,
            });
        }
    }
    out.sort_by_key(|s| s.start);
    out
}

/// `origin` is the display cell `line` starts at in its row (tab stops).
fn build_line_spans<'a>(line: &'a str, spans: &[HiSpan], origin: usize) -> Vec<Span<'a>> {
    if spans.is_empty() {
        return vec![Span::raw(blank_tabs_from(line, origin))];
    }
    let mut out: Vec<Span> = Vec::with_capacity(spans.len() * 2);
    let mut cursor = 0usize;
    // Display cells painted so far, from the row's left edge.
    let mut col = origin;
    let mut push = |out: &mut Vec<Span<'a>>, text: &'a str, style: Option<Style>| {
        let shown = blank_tabs_from(text, col);
        col += cells_from(text, col);
        out.push(match style {
            Some(st) => Span::styled(shown, st),
            None => Span::raw(shown),
        });
    };
    for sp in spans {
        let range = sp.range();
        if range.start > cursor && range.start <= line.len() {
            let slice = &line[cursor..range.start];
            if !slice.is_empty() {
                push(&mut out, slice, None);
            }
        }
        let s = range.start.min(line.len());
        let e = range.end.min(line.len());
        if e > s {
            push(&mut out, &line[s..e], Some(sp.style));
            cursor = e;
        }
    }
    if cursor < line.len() {
        push(&mut out, &line[cursor..], None);
    }
    out
}

impl Widget for &mut Editor {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.clamp_to_symbol_view();
        self.sync_tab_cells();
        let block_style = if self.focused {
            Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let surfaces = self.theme.surfaces().is_some();
        let block = if surfaces {
            crate::widgets::surface::pane_block(self.theme.editor_bg())
        } else {
            Block::default()
                .borders(Borders::ALL)
                .border_style(block_style)
        };
        let mut inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the orange→green
        // gradient that matches the welcome activity box. The editor has no
        // title, so nothing needs re-stamping over the gradient top edge.
        // Croft Loch always draws the seam under the header along the top
        // edge, and lights it with the accent lip only while the editor has
        // the keyboard: a modal quick input above it takes the lip away.
        if surfaces {
            crate::widgets::surface::paint_pane_top(
                buf,
                area,
                self.theme,
                self.focused && !self.overlay_has_keyboard,
            );
        } else if self.focused && self.focus_gradient {
            crate::gradient::paint_gradient_box(buf, area);
        }
        self.last_area = area;
        self.last_inner = inner;
        self.last_scrollbar = Rect::default();
        self.last_hscrollbar = Rect::default();
        self.merge_action_spans.clear();
        self.code_lens_spans.clear();
        self.blame_span = None;
        // Every rect this frame publishes is cleared up front, so a frame
        // that paints nothing leaves nothing behind for the mouse path to
        // hit-test against. `render_log` sets its own when it paints; the
        // early return below means it may never run at all, which is the
        // case a reset inside `render_log` cannot cover.
        if let Some(log) = self.log.as_mut() {
            log.last_body = Rect::default();
        }

        if inner.height == 0 {
            return;
        }
        let cbg = canvas_bg(
            crate::iterm2_inline::detect_iterm2_inline_support(),
            self.theme,
        );
        // Merge editor (#253): reconcile the tracked regions with any
        // manual edit since last frame, carve the source panes off the
        // top, then FALL THROUGH to the text path with the remaining
        // rect — the Result below is the ordinary text buffer.
        let (len, seq, row) = (self.lines.len(), self.edit_seq, self.merge_edit_row);
        let theme = self.theme;
        if let Some(mv) = self.merge.as_mut() {
            mv.sync_with_buffer(len, seq, row);
            inner = render_merge_panes(mv, inner, buf, theme);
            self.last_inner = inner;
        }
        // The Result height comes from the POST-carve rect: the merge
        // panes above just shrank `inner`, and every row / scrollbar /
        // cursor-visibility computation below must see what is left.
        let height = inner.height as usize;
        if height == 0 {
            return;
        }
        if let Some(image) = self.image.as_ref() {
            let ibg = image_canvas_bg(
                crate::iterm2_inline::detect_inline_image_protocol(),
                self.theme,
            );
            render_image_placeholder(image, self.path.as_deref(), inner, buf, ibg, self.theme);
            if self.cell_images {
                let area = Rect {
                    y: inner.y + 1,
                    height: inner.height.saturating_sub(1),
                    ..inner
                };
                let key = (image.generation, area.width, area.height, ibg);
                if self.cell_image.as_ref().map(|c| c.key) != Some(key) {
                    // A body that does not decode (a truncated file whose
                    // header still gave dimensions) is cached as an empty
                    // picture too, so each frame does not decode it again.
                    let (cols, rows, cells) =
                        halfblock_cells(&image.bytes, area.width, area.height, ibg)
                            .unwrap_or_default();
                    self.cell_image = Some(CellImage {
                        key,
                        cols,
                        rows,
                        cells,
                    });
                }
                if let Some(picture) = self.cell_image.as_ref() {
                    paint_cell_image(picture, area, buf);
                }
            }
            return;
        }
        if let Some(view) = self.sheet.as_mut() {
            render_sheet(view, self.path.as_deref(), inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.archive.as_mut() {
            render_archive(view, self.path.as_deref(), inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.pr_review.as_mut() {
            crate::widgets::pr_review::render(view, inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.fleet.as_mut() {
            crate::widgets::fleet::render(view, inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.sarif.as_mut() {
            crate::sarif::render::render(view, self.path.as_deref(), inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.log.as_mut() {
            let search = self
                .search_highlight
                .as_deref()
                .map(|t| (t, self.search_highlight_opts));
            let active = self.active_search_match;
            render_log(
                view,
                self.path.as_deref(),
                inner,
                buf,
                cbg,
                self.theme,
                self.scroll,
                search,
                active,
            );
            return;
        }
        if let Some(view) = self.hex.as_mut() {
            render_hex(view, self.path.as_deref(), inner, buf, cbg, self.theme);
            return;
        }
        if self.markdown_preview.is_some() {
            self.render_markdown_preview(inner, buf);
            return;
        }
        if let Some(diff) = self.diff.as_mut() {
            let (prev_arrow, next_arrow) =
                render_diff(diff, inner, buf, cbg, self.theme, self.pointer);
            self.diff_prev_arrow = prev_arrow;
            self.diff_next_arrow = next_arrow;
            return;
        }
        // Non-diff tabs: clear the hit rects so a stale arrow click on a
        // tab the user just switched away from can't fire.
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();

        // Gutter = a left glyph margin (the breakpoint / stop column, VS Code
        // style) + the right-aligned line number + its trailing space. Folding
        // the margin into `gutter_width` shifts the numbers and code right
        // together, so the number-to-code gap is unchanged and click/cursor
        // mapping (which derives from `gutter_width`) stays correct for free.
        const SIGN_MARGIN: u16 = 2;
        // `off` drops the number column only; the sign margin with its
        // breakpoints and fold chevrons, and the change bar, stay (#1576).
        let digits = match self.line_numbers {
            LineNumbers::Off => 0,
            _ => (self.lines.len() + 1).to_string().len() as u16,
        };
        let gutter_width = digits + 1 + SIGN_MARGIN;
        self.last_gutter_width = gutter_width;
        // Rebuild the git-gutter marks if the buffer moved since last frame
        // (cheap no-op otherwise), so the bars below diff against HEAD.
        self.refresh_git_marks();
        // Same cadence for the fallback fold table (#254): region markers
        // and comment runs the gutter chevrons below consult.
        self.refresh_fold_tables();
        // Rescan merge-conflict blocks on the same cadence so the region
        // tints below always match the buffer.
        self.conflicts();
        // Fold headers are line indexes; an insert/delete anywhere shifts them.
        // If the line count changed since folds were set, drop them wholesale
        // rather than hide the wrong lines (see `fold_epoch_lines`).
        if !self.folded.is_empty() && self.lines.len() != self.fold_epoch_lines {
            self.folded.clear();
            self.hidden_ranges.clear();
        }
        // A caret on a hidden line has no painted row, so nothing is drawn and
        // the user cannot see where they are. Every direct `cursor_row` setter
        // can land there — go-to-definition, search, Ctrl+G, a restored session
        // — and enumerating them is how one gets missed, so normalise here,
        // where all of them converge before anything is painted. One binary
        // search against `hidden_ranges` when a fold exists, nothing when none
        // does.
        if !self.folded.is_empty() && self.is_line_hidden(self.cursor_row) {
            self.reveal_cursor_fold();
        }
        let wrap = self.wrap_enabled();
        if wrap {
            // Wrapped text folds onto extra rows instead of scrolling sideways.
            self.scroll_col = 0;
        }
        let text_x = inner.x + gutter_width + 1;

        // Layout has a small cycle: the horizontal bar takes the bottom row
        // only when a line overflows the text column, but the text column's
        // width depends on whether the vertical bar takes a column. Break it
        // in one pass: provisionally reserve the vertical bar's column from
        // the FULL height to decide the horizontal bar, then recompute the
        // vertical thumb against the real (possibly shortened) viewport.
        // Reserving the horizontal row can only ever narrow the text column,
        // never widen it, so a line that overflows under the provisional
        // width still overflows under the final one - the decision is stable.
        // The horizontal bar never appears in wrap mode (lines fold instead).
        let provisional_text_width = inner
            .width
            .saturating_sub(gutter_width + 2 + u16::from(self.lines.len() > height))
            as usize;
        let content_cols = self.content_cols(provisional_text_width);
        let hbar_present =
            !wrap && provisional_text_width > 0 && content_cols > provisional_text_width;
        let text_height = height - usize::from(hbar_present);
        self.last_text_rows = text_height as u16;

        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: inner.y,
            width: u16::from(inner.width > 0),
            height: text_height as u16,
        };

        // Decide the vertical scrollbar and final text width, then clamp the
        // scroll so the cursor stays visible. In wrap mode the content extent
        // and scroll position are measured in VISUAL rows (a logical line may
        // span several); otherwise in logical lines, one per row.
        let (text_width, scrollbar_metrics) = if wrap {
            let wide = inner.width.saturating_sub(gutter_width + 2) as usize;
            // Overflow at the wide width forces the vbar, which narrows the
            // column and only ever adds rows, so the decision is stable.
            let vbar = match self.clip_content_rows(wide, true) {
                Some((_, rows)) => rows > text_height,
                None => self.total_visual_rows(wide) > text_height,
            };
            let tw = if vbar { wide.saturating_sub(1) } else { wide };
            // A symbol tab measures only its clip: `base` rows sit above it.
            let (base, content_rows) = match self.clip_content_rows(tw, true) {
                Some(clip) => clip,
                None => (0, self.total_visual_rows(tw)),
            };

            // Keep the cursor's visual row inside the viewport.
            if text_height > 0 {
                let cursor_vrow = self.cursor_visual_row(tw);
                let mut top = self.top_visual_row(tw);
                if cursor_vrow < top {
                    top = cursor_vrow;
                } else if cursor_vrow >= top + text_height {
                    top = cursor_vrow + 1 - text_height;
                }
                top = top
                    .min(base + content_rows.saturating_sub(text_height))
                    .max(base);
                self.set_top_to_visual_row(top, tw);
            }
            let metrics = scrollbar::vertical_metrics(
                scrollbar_area,
                content_rows,
                text_height,
                self.top_visual_row(tw).saturating_sub(base),
            );
            (tw as u16, metrics)
        } else {
            // `scroll_sub` counts rows INTO the top line's comment box (set
            // by the content-row scroller); clamp it to the group and drop
            // it entirely when a cursor clamp below moves the top line.
            if self.comment_boxes.is_empty() {
                self.scroll_sub = 0;
            } else {
                let bw = inner.width.saturating_sub(gutter_width + 3) as usize;
                let group = 1 + self.box_rows_between(self.scroll, self.scroll + 1, bw);
                self.scroll_sub = self.scroll_sub.min(group - 1);
            }
            let scroll_before = self.scroll;
            if text_height > 0 {
                if self.cursor_row < self.scroll {
                    self.scroll = self.cursor_row;
                } else if self.cursor_row >= self.scroll + text_height {
                    self.scroll = self.cursor_row + 1 - text_height;
                }
                // Comment boxes between the top and the cursor consume rows;
                // scroll further until the cursor's painted row fits. Boxes
                // are measured at the pre-scrollbar width, one column
                // narrower than the final text column, which can only
                // overestimate their height (the cursor stays visible).
                if !self.comment_boxes.is_empty() {
                    let bw = inner.width.saturating_sub(gutter_width + 3) as usize;
                    while self.cursor_row + self.box_rows_between(self.scroll, self.cursor_row, bw)
                        >= self.scroll + text_height
                    {
                        self.scroll += 1;
                    }
                }
            }
            if let Some((first, end)) = self.symbol_clip() {
                // The lowest top that still fills the pane with the clip's
                // tail, so scrolling never strands one clip line at the top.
                let bw = inner.width.saturating_sub(gutter_width + 3) as usize;
                let (mut top, mut rows) = (end, 0);
                while top > first {
                    let line = top - 1;
                    let need = if self.is_line_hidden(line) {
                        0
                    } else {
                        1 + self.box_rows_between(line, top, bw)
                    };
                    if rows + need > text_height {
                        break;
                    }
                    rows += need;
                    top = line;
                }
                self.scroll = self.scroll.clamp(first, top.min(end - 1));
            }
            if self.scroll != scroll_before {
                self.scroll_sub = 0;
            }
            let bw = inner.width.saturating_sub(gutter_width + 3) as usize;
            let (base, content) = self.clip_content_rows(bw, false).unwrap_or((
                0,
                self.lines.len() + self.box_rows_between(0, self.lines.len(), bw),
            ));
            let metrics = scrollbar::vertical_metrics(
                scrollbar_area,
                content,
                text_height,
                // Content-row top: lines above plus box rows above plus the
                // rows scrolled into the top line's own box.
                (self.scroll + self.box_rows_between(0, self.scroll, bw) + self.scroll_sub)
                    .saturating_sub(base),
            );
            let sw = u16::from(metrics.is_some());
            let tw = inner.width.saturating_sub(gutter_width + 2 + sw);
            (tw, metrics)
        };
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }

        // Clamp horizontal scroll (non-wrap only) so a wheel swipe can't
        // strand the buffer off-screen, then lay out the horizontal bar.
        // The extent depends on the width it is measured against, and the
        // final text column may be narrower than the provisional one.
        let content_cols = self.content_cols(text_width as usize);
        let max_scroll_col = content_cols.saturating_sub(text_width as usize);
        self.scroll_col = self.scroll_col.min(max_scroll_col);
        let hbar_metrics = scrollbar::horizontal_metrics(
            Rect {
                x: text_x,
                y: inner.y + text_height as u16,
                width: text_width,
                height: u16::from(hbar_present),
            },
            content_cols,
            text_width as usize,
            self.scroll_col,
        );
        self.last_hscrollbar = hbar_metrics.map(|m| m.area).unwrap_or_default();

        // Build the visual-row layout the loop paints and the inverse maps
        // (cursor/click) read back. Each entry is (logical_line, char_start,
        // char_end). Non-wrap: one row per visible line, range [scroll_col,
        // scroll_col + text_width). Wrap: the wrapped segments from
        // (scroll, scroll_sub) down, capped at the viewport height.
        let mut visual_rows: Vec<VisRow> = Vec::with_capacity(text_height);
        // The boxes hanging under a line, emitted right after its last
        // segment. `group_pos` is the row's index within the line's whole
        // group (segments first, then box rows) so a wrap viewport top that
        // decomposed into a box region resumes mid-box.
        let push_box_rows = |rows: &mut Vec<VisRow>,
                             ed: &Editor,
                             line: usize,
                             skip: usize,
                             seg_count: usize,
                             tw: usize| {
            if ed.comment_boxes.is_empty() {
                return;
            }
            let mut group_pos = seg_count;
            for idx in 0..ed.comment_boxes.len() {
                if ed.comment_boxes[idx].line != line {
                    continue;
                }
                for box_row in 0..ed.comment_box_height(idx, tw) {
                    if group_pos >= skip && rows.len() < text_height {
                        rows.push(VisRow::Box {
                            box_idx: idx,
                            box_row,
                        });
                    }
                    group_pos += 1;
                }
            }
        };
        // The lines of a live inline suggestion after its first (#1625),
        // hung under the caret's line as their own rows: only the first fits
        // at the caret, and a "(+2 lines)" count let Tab insert a function
        // body nobody had read. The caret row says how many did not fit.
        let ghost_tail = if self.focused {
            self.live_ghost()
                .map_or(0, |t| t.split('\n').count().saturating_sub(1))
        } else {
            0
        };
        let push_ghost_rows = |rows: &mut Vec<VisRow>| {
            for index in 1..=ghost_tail {
                if rows.len() >= text_height {
                    break;
                }
                rows.push(VisRow::Ghost { index });
            }
        };
        // A symbol tab stops painting at its symbol's last line.
        let clip_end = self.symbol_clip().map_or(self.lines.len(), |(_, end)| end);
        if wrap {
            let tw = text_width as usize;
            let mut line = self.scroll;
            let mut skip = self.scroll_sub;
            while line < clip_end && visual_rows.len() < text_height {
                if self.is_line_hidden(line) {
                    line += 1;
                    continue;
                }
                let segs = self.line_segments(line, tw);
                for (i, &(s, e)) in segs.iter().enumerate() {
                    if i < skip {
                        continue;
                    }
                    if visual_rows.len() >= text_height {
                        break;
                    }
                    visual_rows.push(VisRow::Text {
                        line,
                        start: s,
                        end: e,
                    });
                }
                if line == self.cursor_row && skip < segs.len() {
                    push_ghost_rows(&mut visual_rows);
                }
                push_box_rows(&mut visual_rows, self, line, skip, segs.len(), tw);
                skip = 0;
                line += 1;
            }
        } else {
            // Collect up to `text_height` VISIBLE logical lines from `scroll`,
            // skipping any hidden inside a collapsed fold.
            let tw = text_width as usize;
            let mut line = self.scroll;
            // Group-position skip on the top line: 0 = its text row, 1..
            // = rows into its comment box, so the viewport can start
            // mid-box exactly like the wrap path.
            let mut skip = self.scroll_sub;
            while line < clip_end && visual_rows.len() < text_height {
                if !self.is_line_hidden(line) {
                    if skip == 0 && visual_rows.len() < text_height {
                        visual_rows.push(VisRow::Text {
                            line,
                            start: self.scroll_col,
                            end: self.scroll_col + text_width as usize,
                        });
                        if line == self.cursor_row {
                            push_ghost_rows(&mut visual_rows);
                        }
                    }
                    push_box_rows(&mut visual_rows, self, line, skip, 1, tw);
                    skip = 0;
                }
                line += 1;
            }
        }
        self.last_wrap_rows = visual_rows.clone();

        let sel_norm = self
            .selection
            .filter(|s| s.has_area())
            .map(|s| s.normalised());
        let occ_needle = self.selection_occurrence_needle();
        // Bracket-match highlight (VS Code `editorBracketMatch`): only in the
        // focused editor, and only when the caret is beside a bracket.
        let bracket_pair = if self.focused {
            self.cached_bracket_match_pair()
        } else {
            None
        };
        // Indentation guides: the cursor block's active guide (focused editor
        // only) and the paint colours, resolved once per frame — the active
        // walk is O(block), not per-row work.
        let guide_step = self.guide_step();
        let active_guide = (self.show_indent_guides && self.focused)
            .then(|| self.active_indent_guide())
            .flatten();
        let guide_fg = self.theme.indent_guide();
        let guide_active_fg = self.theme.indent_guide_active();
        // A `surfaces` theme keeps the gutter quiet: an expanded fold's
        // chevron shows on the caret's line, or on every foldable line while
        // the pointer is over the gutter (VS Code's `showFoldingControls:
        // mouseover`); the change bar hugs the code edge as a quarter block.
        let surfaces = self.theme.surfaces().is_some();
        let gutter_hovered = self.pointer.is_some_and(|(px, py)| {
            px >= inner.x
                && px <= inner.x + gutter_width
                && py >= inner.y
                && py < inner.y + inner.height
        });
        // Each text row's geometry, refilled per row (#873).
        let mut row_cells = RowCells::default();

        for (row_idx, vis_row) in visual_rows.iter().enumerate() {
            let y = inner.y + row_idx as u16;
            let (line_idx, row_start, row_end) = match *vis_row {
                VisRow::Box { box_idx, box_row } => {
                    self.paint_comment_box_row(
                        buf,
                        inner,
                        y,
                        gutter_width,
                        text_x,
                        text_width,
                        box_idx,
                        box_row,
                    );
                    continue;
                }
                VisRow::Ghost { index } => {
                    self.paint_ghost_row(
                        buf,
                        inner,
                        y,
                        gutter_width,
                        text_x,
                        text_width,
                        index,
                        wrap,
                    );
                    continue;
                }
                VisRow::Text { line, start, end } => (line, start, end),
            };
            // Current-line highlight (VS Code `editor.lineHighlightBackground`,
            // #248): a whisper of the accent behind every visual row of the
            // cursor's line, gutter through code. Focused editor only, and
            // hidden while a selection is active so the two washes never stack
            // — VS Code's `renderLineHighlight` behaves the same. Painted
            // first: spans that carry their own background (selection, search,
            // occurrences) patch over it, spans that don't inherit it.
            if self.focused && line_idx == self.cursor_row && sel_norm.is_none() {
                buf.set_style(
                    Rect::new(inner.x, y, gutter_width + 1 + text_width, 1),
                    Style::default().bg(self.theme.current_line_bg()),
                );
            }
            // The line number shows once per logical line - on its first visual
            // row; wrapped continuation rows get a blank gutter, like VS Code.
            if !wrap || row_start == 0 {
                let line_no = format!(
                    "{:>width$} ",
                    self.gutter_number(line_idx)
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                    width = gutter_width as usize - 1
                );
                // Live Run coverage tints the number: green for a statement
                // that ran, a muted red for one that never did. Otherwise a
                // caret's line is brighter than the rest (VS Code's
                // `editorLineNumber.activeForeground`, #1576).
                let number_fg = match self
                    .live_run
                    .as_ref()
                    .and_then(|v| v.coverage(line_idx, &self.lines[line_idx]))
                {
                    Some(crate::live_run::Coverage::Ran) => self.theme.git_added(),
                    Some(crate::live_run::Coverage::NeverRan) => {
                        self.theme.ui(Color::Rgb(0xa0, 0x55, 0x55))
                    }
                    None if self.is_caret_line(line_idx) => self.theme.line_number_active(),
                    None => self.theme.line_number_fg(),
                };
                let gutter = Line::from(Span::styled(line_no, Style::default().fg(number_fg)));
                buf.set_line(inner.x, y, &gutter, gutter_width);
            } else {
                buf.set_line(
                    inner.x,
                    y,
                    &Line::from(" ".repeat(gutter_width as usize)),
                    gutter_width,
                );
            }

            // Debugger gutter glyphs, on a logical line's first visual row only:
            // a yellow stop arrow (▶) takes priority over a red breakpoint dot
            // (●). Painted in the dedicated glyph margin at the far left
            // (`inner.x`), VS Code-style, so it never crowds the line number or
            // the code and the number-to-code gap is untouched.
            let sign_x = inner.x;
            let mut sign_taken = false;
            if (!wrap || row_start == 0)
                && let Some(path) = self.path.as_deref()
            {
                let here = line_idx + 1; // gutter is 1-based
                let is_stop = self
                    .stop_line
                    .as_ref()
                    .is_some_and(|(p, l)| p == path && *l == here);
                let is_bp = self
                    .breakpoints
                    .get(path)
                    .is_some_and(|s| s.contains(&here));
                let is_unverified = self
                    .unverified_breakpoints
                    .get(path)
                    .is_some_and(|s| s.contains(&here));
                // A hit count is a condition on the pause (#611), so it wears
                // the conditional diamond too, as in VS Code.
                let is_conditional = self
                    .breakpoint_conditions
                    .get(path)
                    .is_some_and(|c| c.contains_key(&here))
                    || self
                        .breakpoint_hit_conditions
                        .get(path)
                        .is_some_and(|h| h.contains_key(&here));
                let is_logpoint = self
                    .breakpoint_logs
                    .get(path)
                    .is_some_and(|m| m.contains_key(&here));
                if is_stop {
                    buf.set_string(
                        sign_x,
                        y,
                        "▶",
                        Style::default().fg(self.theme.ui(Color::Rgb(0xff, 0xcc, 0x00))),
                    );
                    sign_taken = true;
                } else if is_bp {
                    // Hollow dimmed ring when the adapter could not bind it; a
                    // red diamond for a conditional breakpoint; an amber one
                    // for a logpoint; a solid red dot for a plain, live one.
                    let (glyph, color) = if is_unverified {
                        ("○", self.theme.ui(Color::Rgb(0x99, 0x99, 0x99)))
                    } else if is_conditional {
                        ("◆", self.theme.ui(Color::Rgb(0xe5, 0x1c, 0x23)))
                    } else if is_logpoint {
                        ("◆", self.theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b)))
                    } else {
                        ("●", self.theme.ui(Color::Rgb(0xe5, 0x1c, 0x23)))
                    };
                    buf.set_string(sign_x, y, glyph, Style::default().fg(color));
                    sign_taken = true;
                }
            }

            // AI-stream stop button (croft pair): while a pilot streams into
            // this file, the row under its caret wears a stop square in the
            // badge's orange; clicking it (or Cmd+K X) cancels and reverts
            // the stream. Outranks the test play glyph; the debugger's
            // glyphs above still win the shared cell.
            if (!wrap || row_start == 0) && !sign_taken && self.stream_stop_line == Some(line_idx) {
                buf.set_string(
                    sign_x,
                    y,
                    "■",
                    Style::default().fg(self.theme.ui(Color::Rgb(0xff, 0x9d, 0x2f))),
                );
                sign_taken = true;
            }

            // Bookmark flag: a reader's own mark, ranked below every debugger
            // glyph and the AI-stream square (those report machine state and
            // must never be hidden) but above the test play bead.
            if (!wrap || row_start == 0) && !sign_taken && self.is_bookmarked(line_idx) {
                buf.set_string(
                    sign_x,
                    y,
                    "\u{2691}", // ⚑, a plain-Unicode flag: no Nerd Font needed
                    Style::default().fg(self.theme.ui(Color::Rgb(0x4f, 0xc1, 0xff))),
                );
                sign_taken = true;
            }

            // Testing gutter glyph: the play button beside a test fn's
            // definition (VS Code's run bead, in its testing green), sharing
            // the sign cell with the debugger — whose stop arrow and
            // breakpoint dot both outrank it.
            if (!wrap || row_start == 0)
                && !sign_taken
                && crate::testing::locate::test_fn_on_line(
                    self.path.as_deref(),
                    &self.lines,
                    line_idx,
                )
                .is_some()
            {
                buf.set_string(
                    sign_x,
                    y,
                    "\u{eb2c}", // cod-play, the Testing panel's run glyph
                    Style::default().fg(self.theme.ui(Color::Rgb(0x73, 0xc9, 0x91))),
                );
            }

            // Fold chevron: on a foldable header's first visual row, in the
            // second sign-margin column (`sign_x + 1`) so it never collides with
            // a breakpoint dot at `sign_x`. ▾ = expanded, ▸ = collapsed; the
            // collapsed one is brighter so hidden content is noticeable. Matches
            // the gutter's own line numbers. On a `surfaces` theme the
            // expanded one waits for the caret or the pointer, and the
            // collapsed one, always shown, wears the muted ink.
            if (!wrap || row_start == 0) && self.is_foldable(line_idx) {
                let collapsed = self.folded.contains(&line_idx);
                let glyph = if collapsed {
                    let ink = crate::widgets::surface::tones(self.theme)
                        .map_or_else(|| self.theme.ui(Color::Gray), |t| t.muted());
                    Some(("▸", ink))
                } else if !surfaces || gutter_hovered || self.is_caret_line(line_idx) {
                    Some(("▾", self.theme.line_number_fg()))
                } else {
                    None
                };
                if let Some((glyph, color)) = glyph {
                    buf.set_string(sign_x + 1, y, glyph, Style::default().fg(color));
                }
            }

            // Git gutter: a thin coloured bar in the spacer cell between the
            // line number and the code (VS Code's dirty-diff lane), on a
            // logical line's first visual row only. Colour carries add/mod/del.
            // ponytail: one heavy bar in three colours; the bar's column also
            // shows deletions, which VS Code renders as a small triangle — fine
            // as a first cut, the colour already disambiguates.
            if (!wrap || row_start == 0)
                && let Some(mark) = self.git_marks.get(&line_idx)
            {
                let color = match mark {
                    GitMark::Added => self.theme.git_added(),
                    GitMark::Modified => self.theme.git_modified(),
                    GitMark::Deleted => self.theme.git_deleted(),
                };
                // ┃ heavy vertical; ▎ left quarter block on a `surfaces`
                // theme, a crisp stripe on the cell edge rather than a line
                // through its middle.
                let bar = if surfaces { "\u{258e}" } else { "\u{2503}" };
                buf.set_string(inner.x + gutter_width, y, bar, Style::default().fg(color));
            }
            // Coverage lens (#263): the same lane, painted over the git bar
            // while a coverage report is shown. Green run, red never run,
            // amber run with a branch missed; dimmed once the file changed
            // after the run. `Coverage: Clear` gives the lane back to git.
            if (!wrap || row_start == 0)
                && let Some(lens) = self.coverage.as_ref()
                && let Some(cov) = lens.lines.get(&line_idx)
            {
                use crate::testing::coverage::LineCov;
                let (rgb, glyph) = match cov {
                    LineCov::Covered => ((0x4e, 0xc9, 0x7a), "\u{258c}"),
                    LineCov::Uncovered => ((0xe0, 0x55, 0x55), "\u{258c}"),
                    LineCov::Partial => ((0xe0, 0xb0, 0x40), "\u{2596}"),
                };
                let (r, g, b) = if lens.stale {
                    (rgb.0 / 2, rgb.1 / 2, rgb.2 / 2)
                } else {
                    rgb
                };
                buf.set_string(
                    inner.x + gutter_width,
                    y,
                    glyph,
                    Style::default().fg(self.theme.ui(Color::Rgb(r, g, b))),
                );
            }
            // Provenance overlay (#349): the same lane, a thinner bar in the
            // seat's hue, painted over the git bar while the lens is on
            // because the user asked who typed the line, not whether it
            // differs from HEAD. A line with no record paints nothing: an
            // unknown line is never guessed at.
            if self.provenance_overlay
                && (!wrap || row_start == 0)
                && let Some(seat) = self.provenance.seat(line_idx)
            {
                let (r, g, b) = seat.hue();
                // ▎ left one-quarter block; ▏ one-eighth on a `surfaces`
                // theme, whose change bar already takes the quarter block.
                let bar = if surfaces { "\u{258f}" } else { "\u{258e}" };
                buf.set_string(
                    inner.x + gutter_width,
                    y,
                    bar,
                    Style::default().fg(self.theme.ui(Color::Rgb(r, g, b))),
                );
            }

            // Per-row window: the segment [row_start, row_end). For non-wrap
            // this is exactly the horizontally-scrolled view; for wrap it is
            // one folded segment. The overlay painters treat `row_start` as
            // the scroll offset and `row_width` as the text width, so the
            // same code paints both modes.
            let raw = &self.lines[line_idx];
            let line_len = self.line_char_len(line_idx);
            let byte_start = byte_index_of_char(raw, row_start);
            let byte_end = byte_index_of_char(raw, row_end);
            // A wrapped row's width in CELLS, which is what the painters clip
            // by: its char count cut a row of wide characters in half.
            let row_origin = self.row_origin(raw, row_start);
            let row_width = {
                let cells: usize = cells_from(&raw[byte_start..byte_end], row_origin);
                cells.max(row_end - row_start).min(text_width as usize) as u16
            };
            let visible_raw = &raw[byte_start..byte_end];
            let seg_bytes = byte_end - byte_start;
            let empty: Vec<HiSpan> = Vec::new();
            let line_spans = self.highlights.get(line_idx).unwrap_or(&empty);
            // Paint the LSP semantic overlay over the tree-sitter base so
            // resolved symbols (parameters, etc.) win wherever the server
            // has an opinion, syntax fills the gaps (the "combined" model).
            let sem_spans = self.semantic_overlay.get(line_idx).unwrap_or(&empty);
            let merged = merge_overlay(line_spans, sem_spans);
            // The row's geometry: its text by display width (#873) and the
            // inlay hints anchored inside its window, as (anchor col,
            // display cells) pairs. A hint's label is spliced into the row
            // BEFORE the character at its anchor; every buffer column at or
            // past an anchor therefore paints `inlay_cells_before` cells
            // further right, and each overlay painter below translates its
            // columns through `row_cells` so highlights, underlines, and
            // carets stay glued to their glyphs.
            let hint_cap = row_end.min(line_len);
            row_cells.fill(
                visible_raw,
                row_start,
                row_origin,
                self.row_inlay_spans(line_idx)
                    .iter()
                    .filter(|(hc, _, _)| *hc >= row_start && *hc <= hint_cap)
                    .map(|(hc, l, _)| (*hc, l.chars().count())),
            );
            if row_cells.hints.is_empty() {
                // Shift highlight spans to the row origin and clip them to the
                // segment so build_line_spans never slices past `visible_raw`.
                let shifted: Vec<HiSpan> = shift_spans_for_view(&merged, byte_start)
                    .into_iter()
                    .filter_map(|sp| sp.clipped(seg_bytes))
                    .collect();
                let spans = build_line_spans(visible_raw, &shifted, row_origin);
                buf.set_line(text_x, y, &Line::from(spans), row_width);
            } else {
                // Splice each hint label between the text segments it splits.
                // Style A (Zed's look): dim italic text, one shade quieter
                // than comments, no chip background.
                let hint_style = Style::default()
                    .fg(self.theme.ignored_fg())
                    .add_modifier(Modifier::ITALIC);
                let mut out: Vec<Span> = Vec::new();
                let mut from = row_start;
                for (hcol, label, swatch) in self
                    .row_inlay_spans(line_idx)
                    .iter()
                    .filter(|(hc, _, _)| *hc >= row_start && *hc <= hint_cap)
                {
                    if *hcol > from {
                        out.extend(inlay_text_segment(
                            raw,
                            &merged,
                            (row_start, row_origin),
                            from,
                            *hcol,
                        ));
                    }
                    // A color swatch paints in ITS color (#254); plain
                    // hints keep the dim italic Zed look.
                    let style = match swatch {
                        Some(c) => Style::default().fg(*c),
                        None => hint_style,
                    };
                    out.push(Span::styled(label.clone(), style));
                    from = from.max(*hcol);
                }
                if row_end > from {
                    out.extend(inlay_text_segment(
                        raw,
                        &merged,
                        (row_start, row_origin),
                        from,
                        row_end,
                    ));
                }
                buf.set_line(text_x, y, &Line::from(out), row_width);
            }

            // Bracket-pair colorization (#131): recolour each bracket cell by
            // its nesting depth (red for an unmatched closer). A foreground
            // override on the already-painted glyph, so selection / search /
            // occurrence layers below still lay their backgrounds over it —
            // and the find layer, painted later, wins outright like it does
            // over syntax colours. Columns translate through inlay cells and
            // the row window like every painter; wrap continuation segments
            // participate (brackets live anywhere in the line, unlike the
            // leading-whitespace guides).
            if self.show_bracket_colors
                && let Some(cols) = self.bracket_colors.get(line_idx)
            {
                for &(c, ci) in cols {
                    let c = c as usize;
                    if c < row_start {
                        continue;
                    }
                    let fg = if ci == UNEXPECTED_BRACKET {
                        self.theme.bracket_unexpected_fg()
                    } else {
                        self.theme.bracket_pair_color(usize::from(ci))
                    };
                    if !paint_row_cells(buf, text_x, y, row_width, &row_cells, c, |s| s.fg(fg)) {
                        break;
                    }
                }
            }

            // Inline color-literal decorations (VS Code's
            // `naumovs.color-highlight`, filling the "no colour-swatch
            // decorations" gap): tint each recognized `#hex` / `rgb()` /
            // `hsl()` literal's own cells with its parsed color, choosing a
            // black or white foreground for legibility. Painted after
            // bracket colorization so a color literal's own brackets (e.g.
            // `rgb(...)`) still read against the tint, and before the
            // overlays below (selection, search) so those still win over it.
            if self.show_color_swatches
                && let Some(hits) = self.color_swatches.get(line_idx)
            {
                for &(start_c, end_c, bg, fg) in hits {
                    if end_c <= row_start {
                        continue;
                    }
                    let from = start_c.max(row_start);
                    for c in from..end_c {
                        let tint = |s: Style| s.bg(bg).fg(fg);
                        if !paint_row_cells(buf, text_x, y, row_width, &row_cells, c, tint) {
                            break;
                        }
                    }
                }
            }

            // Indentation guides (VS Code `editor.guides.indentation`): a dim
            // │ at each indent-unit column of the line's leading whitespace,
            // with the cursor block's guide highlighted. Painted into blank
            // cells only, right after the text, so text always wins and the
            // overlays below (selection band, conflict tints) lay their
            // backgrounds over the glyph. A wrap continuation row carries no
            // leading whitespace, so only a line's first segment draws guides;
            // in the flat path `row_start` is the horizontal scroll and
            // translates the columns like every other painter here.
            if self.show_indent_guides && (!wrap || row_start == 0) {
                let (guide_width, guide_src) = self.guide_indent(line_idx);
                // A blank line's guides sit where its neighbour's indentation
                // is drawn, tabs and all, not one cell per level.
                let src_cells;
                let geometry = if guide_src == line_idx {
                    &row_cells
                } else {
                    src_cells = RowCells::of_line_at(
                        &self.lines[guide_src],
                        row_start,
                        self.row_origin(&self.lines[guide_src], row_start),
                    );
                    &src_cells
                };
                for c in (0..guide_width).step_by(guide_step) {
                    if c < row_start {
                        continue;
                    }
                    let col = geometry.cell(c);
                    if col >= usize::from(row_width) {
                        break;
                    }
                    let cell = &mut buf[(text_x + col as u16, y)];
                    if cell.symbol() != " " {
                        continue;
                    }
                    let fg = match active_guide {
                        Some((ac, lo, hi)) if ac == c && (lo..=hi).contains(&line_idx) => {
                            guide_active_fg
                        }
                        _ => guide_fg,
                    };
                    cell.set_symbol("\u{2502}");
                    // Not italic even over a comment: a slanted guide looks
                    // broken, and the text's modifier is not the guide's.
                    let style = cell.style().fg(fg).remove_modifier(Modifier::ITALIC);
                    cell.set_style(style);
                }
            }

            // Render whitespace (#133): swap space/tab cells to `·`/`→` in a
            // dim theme colour — across the whole row in All mode, inside the
            // primary and secondary-caret selections in Selection mode (the
            // VS Code default). Painted after the indent guides so the glyph
            // wins the cell: it reports a real character where the guide only
            // decorates, and the selection band below touches backgrounds
            // only, so the glyph rides the band like text does.
            if self.whitespace_mode != WhitespaceMode::None {
                let ws_fg = self.theme.whitespace_fg();
                let mut ws_spans: Vec<(usize, usize)> = Vec::new();
                match self.whitespace_mode {
                    WhitespaceMode::All => ws_spans.push((0, line_len)),
                    WhitespaceMode::Selection => {
                        if let Some(((sr, sc), (er, ec))) = sel_norm
                            && line_idx >= sr
                            && line_idx <= er
                        {
                            let s = if line_idx == sr { sc } else { 0 };
                            let e = if line_idx == er { ec } else { line_len };
                            if e > s {
                                ws_spans.push((s, e));
                            }
                        }
                        for caret in &self.carets {
                            let ((cr0, cc0), (cr1, cc1)) = caret.normalised();
                            if line_idx >= cr0 && line_idx <= cr1 {
                                let s = if line_idx == cr0 { cc0 } else { 0 };
                                let e = if line_idx == cr1 { cc1 } else { line_len };
                                if e > s {
                                    ws_spans.push((s, e));
                                }
                            }
                        }
                    }
                    WhitespaceMode::None => {}
                }
                for (s, e) in ws_spans {
                    let from = s.max(row_start);
                    let to = e.min(row_end).min(line_len);
                    if to <= from {
                        continue;
                    }
                    for (ci, ch) in raw.chars().enumerate().skip(from).take(to - from) {
                        let glyph = match ch {
                            ' ' => "\u{b7}",
                            '\t' => "\u{2192}",
                            _ => continue,
                        };
                        let col = row_cells.cell(ci);
                        if col >= usize::from(row_width) {
                            break;
                        }
                        let cell = &mut buf[(text_x + col as u16, y)];
                        cell.set_symbol(glyph);
                        // The glyph marks whitespace, not comment text.
                        let style = cell.style().fg(ws_fg).remove_modifier(Modifier::ITALIC);
                        cell.set_style(style);
                    }
                }
            }

            // Merge-conflict region tints (VS Code's current/incoming
            // backgrounds), painted right after the text so diagnostics,
            // search highlights, and the selection all win over them.
            if let Some(tint) = conflict_row_tint(&self.conflicts, line_idx, self.theme) {
                paint_full_row_bg(buf, text_x, y, row_width, tint);
            }
            // Clickable accept buttons on the conflict header row — VS Code's
            // merge-conflict CodeLens, drawn after the marker text where the
            // `<<<<<<<` line is otherwise dead space (#1169). A narrow row
            // gets the short labels rather than losing buttons. Hit spans are
            // recorded per frame (cleared at render start) so the rects always
            // describe the painted frame (#103's invariant).
            if let Some(block_idx) = self.conflicts.iter().position(|b| b.ours_start == line_idx) {
                let block = self.conflicts[block_idx];
                let marker_len = row_cells.text_before(line_len) as u16;
                // A header that wraps gets them after its last segment only.
                if marker_len <= row_width {
                    use crate::merge::Resolution;
                    let chips = [
                        AcceptChip::new("Accept Current", "Current", ACCEPT_CURRENT_FILL),
                        AcceptChip::new("Accept Incoming", "Incoming", ACCEPT_INCOMING_FILL),
                        AcceptChip::new("Accept Both", "Both", ACCEPT_BOTH_FILL),
                    ];
                    let res = [Resolution::Current, Resolution::Incoming, Resolution::Both];
                    let x0 = text_x + marker_len + 2;
                    for (i, xs) in
                        paint_accept_chips(buf, &chips, x0, text_x + row_width, y, self.theme)
                    {
                        self.merge_action_spans
                            .push((y, xs, block.ours_start, res[i]));
                    }
                }
            }

            // LSP diagnostics: underline each problem span in its severity
            // colour, clipped to the visible row window. VS Code draws a wavy
            // underline; the crossterm backend exposes only a straight one, so
            // a coloured straight underline carries the severity instead. The
            // underline colour is independent of the glyph's foreground, so the
            // syntax/semantic colour underneath is preserved.
            let empty_diag: Vec<(usize, usize, crate::lsp::manager::DiagnosticSeverity)> =
                Vec::new();
            for &(sc, ec, severity) in self.diagnostic_spans.get(line_idx).unwrap_or(&empty_diag) {
                let vs = row_cells.cell(sc);
                // The exclusive end shifts by the hints before its LAST char,
                // so an underline never swallows a hint anchored right at it.
                let ve = row_cells.end(ec);
                if ve > vs {
                    paint_diagnostic_underline(
                        buf, text_x, y, row_width, vs, ve, severity, self.theme,
                    );
                }
            }
            let empty_tags: Vec<(usize, usize, crate::lsp::manager::DiagnosticTags)> = Vec::new();
            for &(sc, ec, tags) in self
                .diagnostic_tag_spans
                .get(line_idx)
                .unwrap_or(&empty_tags)
            {
                let vs = row_cells.cell(sc);
                let ve = row_cells.end(ec);
                if ve > vs {
                    paint_diagnostic_tags(buf, text_x, y, row_width, vs, ve, tags);
                }
            }

            // LSP occurrences of the symbol under the caret (word highlight):
            // painted before the find layer, the selection band, and the
            // bracket match so all three stay visible on top. The find layer
            // in particular paints black-on-gold; an occurrence bg over it
            // left black text on a dark grey (the caret parked on a find
            // match makes the two layers cover the same cells).
            for &(occ_row, occ_start, occ_end, occ_write) in &self.occurrences {
                if occ_row != line_idx {
                    continue;
                }
                let bg = if occ_write {
                    self.theme.occurrence_write_bg()
                } else {
                    self.theme.occurrence_bg()
                };
                for c in occ_start.max(row_start)..occ_end {
                    if !paint_row_cells(buf, text_x, y, row_width, &row_cells, c, |s| s.bg(bg)) {
                        break;
                    }
                }
            }

            if let Some(term) = self
                .search_highlight
                .as_deref()
                .filter(|_| find_window(self.find_range, line_idx) != Some((0, 0)))
            {
                let active_on_line = self
                    .active_search_match
                    .filter(|(r, _, _)| *r == line_idx)
                    .map(|(_, c, l)| (c, l));
                paint_search_highlight(
                    buf,
                    text_x,
                    y,
                    row_width,
                    raw,
                    term,
                    self.search_highlight_opts,
                    &row_cells,
                    active_on_line,
                    None,
                    find_window(self.find_range, line_idx),
                    self.theme,
                );
            }

            if let Some(needle) = occ_needle.as_deref() {
                paint_selection_occurrences(
                    buf, text_x, y, row_width, raw, needle, &row_cells, self.theme,
                );
            }

            if let Some(((sr, sc), (er, ec))) = sel_norm
                && line_idx >= sr
                && line_idx <= er
            {
                let sel_start = if line_idx == sr { sc } else { 0 };
                // For non-final selected rows, paint past the content by one
                // cell to make the trailing newline visible.
                let sel_end = if line_idx == er { ec } else { line_len + 1 };
                let visible_start = row_cells.cell(sel_start);
                let visible_end = row_cells.end(sel_end);
                if visible_end > visible_start {
                    paint_selection_band(
                        buf,
                        text_x,
                        y,
                        row_width,
                        visible_start,
                        visible_end,
                        self.theme,
                    );
                }
            }

            // Bracket-match highlight: tint the two matched bracket cells that
            // fall on this visual row, honouring horizontal scroll.
            if let Some((open, close)) = bracket_pair {
                for pos in [open, close] {
                    if pos.0 == line_idx && pos.1 >= row_start {
                        paint_bracket_match(
                            buf,
                            text_x,
                            y,
                            row_width,
                            row_cells.cell(pos.1),
                            self.theme,
                        );
                    }
                }
            }

            // Secondary multi-cursor carets ("Change All Occurrences"): paint
            // each one's selection band and a software block cursor at its
            // head. The primary cursor is the hardware caret; these blocks
            // mark the extra cursors so the user sees every edit point.
            for caret in &self.carets {
                let ((cr0, cc0), (cr1, cc1)) = caret.normalised();
                if line_idx >= cr0 && line_idx <= cr1 {
                    let cs = if line_idx == cr0 { cc0 } else { 0 };
                    let ce = if line_idx == cr1 { cc1 } else { line_len + 1 };
                    let vs = row_cells.cell(cs);
                    let ve = row_cells.end(ce);
                    if ve > vs {
                        paint_selection_band(buf, text_x, y, row_width, vs, ve, self.theme);
                    }
                }
                // Only paint the block on the visual row that holds the caret.
                let c = caret.head.1;
                let on_row = caret.head.0 == line_idx
                    && c >= row_start
                    && (c < row_end || (c == row_end && row_end == line_len));
                if on_row {
                    // `caret`, not `cell`: a caret AT a hint's anchor sits
                    // left of the hint, inside its own band.
                    paint_block_cursor(buf, text_x, y, row_width, row_cells.caret(c), self.theme);
                }
            }

            // Collaborators' ghost carets (multiplayer): the same block shape
            // as secondary carets, in each participant's color. The local
            // hardware caret is painted by the terminal after drawing, so it
            // always sits above these.
            for &(gr, gc, color) in &self.ghost_carets {
                let on_row = gr == line_idx
                    && gc >= row_start
                    && (gc < row_end || (gc == row_end && row_end == line_len));
                if on_row {
                    paint_ghost_caret(buf, text_x, y, row_width, row_cells.caret(gc), color);
                }
            }

            // GitLens-style inline blame: a dim italic annotation trailing the
            // cursor's line, on its last visual segment, in the focused editor
            // only. Painted last so no overlay covers it, and clipped to the
            // pane's right edge.
            if self.focused
                && line_idx == self.cursor_row
                && row_end >= line_len
                && !self.inline_values.contains_key(&line_idx)
                && self.live_note(line_idx).is_none()
                && self.lenses_on_line(line_idx).is_empty()
                && self.live_ghost().is_none()
                && let Some(note) = self.current_line_blame_annotation()
            {
                let text_cols = row_cells.cell(line_len);
                let start_x = text_x + text_cols as u16 + 2;
                let right = inner.x + inner.width;
                if start_x < right {
                    let avail = (right - start_x) as usize;
                    let shown: String = note.chars().take(avail).collect();
                    buf.set_string(
                        start_x,
                        y,
                        &shown,
                        Style::default()
                            .fg(self.theme.ignored_fg())
                            .add_modifier(Modifier::ITALIC),
                    );
                    // Hover and click reach the commit it names (#1556);
                    // `Uncommitted changes` has none to show.
                    if let Some(LineBlame::Commit(c)) = self.cursor_line_blame() {
                        let hash = c.hash.clone();
                        let w = unicode_width::UnicodeWidthStr::width(shown.as_str()) as u16;
                        self.blame_span = Some((y, start_x..(start_x + w).min(right), hash));
                    }
                }
            }

            // Debugger inline values (#135): the "name = value" trailer for a
            // stopped session, on the line's last visual segment like the
            // blame above (which yields to it on the cursor line — while
            // stepping, state beats history). Clipped to the pane edge.
            if row_end >= line_len
                && let Some(note) = self.inline_values.get(&line_idx)
            {
                let text_cols = row_cells.cell(line_len);
                let start_x = text_x + text_cols as u16 + 2;
                let right = inner.x + inner.width;
                if start_x < right {
                    let avail = (right - start_x) as usize;
                    let shown: String = note.chars().take(avail).collect();
                    buf.set_string(
                        start_x,
                        y,
                        &shown,
                        Style::default()
                            .fg(self.theme.ignored_fg())
                            .add_modifier(Modifier::ITALIC),
                    );
                }
            }

            // A language server's replacement strikes through the columns it
            // takes the place of, and its text stands one cell clear of them.
            let replaces = self
                .live_ghost_full()
                .and_then(|g| g.replaces.clone())
                .filter(|_| self.focused && line_idx == self.cursor_row);
            if let Some(r) = &replaces {
                let (vs, ve) = (row_cells.cell(r.start), row_cells.end(r.end));
                if ve > vs {
                    let struck = crate::lsp::manager::DiagnosticTags {
                        unnecessary: true,
                        deprecated: true,
                    };
                    paint_diagnostic_tags(buf, text_x, y, row_width, vs, ve, struck);
                }
            }

            // Inline suggestion (#607): the first line as ghost text right at
            // the caret (suggestions are only asked for at a line's end); the
            // rest hang below as `VisRow::Ghost` rows (#1625), and only lines
            // that found no row are counted here. It owns the line's tail, so
            // the trailers below stand aside.
            let ghost_here = self.focused
                && line_idx == self.cursor_row
                && row_end >= line_len
                && self.live_ghost().is_some();
            if ghost_here && let Some(text) = self.live_ghost() {
                let text_cols = row_cells.cell(line_len) + usize::from(replaces.is_some());
                let x = text_x + text_cols as u16;
                let right = inner.x + inner.width;
                if x < right {
                    let first = text.split('\n').next().unwrap_or("");
                    let shown_rows = visual_rows
                        .iter()
                        .filter(|r| matches!(r, VisRow::Ghost { .. }))
                        .count();
                    let more = text.split('\n').count() - 1 - shown_rows;
                    let shown = if more > 0 {
                        format!(
                            "{first}  (+{more} line{})",
                            if more == 1 { "" } else { "s" }
                        )
                    } else {
                        first.to_string()
                    };
                    let clipped: String = shown.chars().take((right - x) as usize).collect();
                    buf.set_string(
                        x,
                        y,
                        &clipped,
                        Style::default()
                            .fg(self.theme.ignored_fg())
                            .add_modifier(Modifier::ITALIC),
                    );
                }
            }

            // Code lenses (#608): the server's "3 references | Run" at the end
            // of the symbol's line, clickable, yielding to the debugger's and
            // Live Run's trailers, which describe this very run.
            if row_end >= line_len
                && !ghost_here
                && !self.inline_values.contains_key(&line_idx)
                && self.live_note(line_idx).is_none()
            {
                let lenses = self.lenses_on_line(line_idx);
                if !lenses.is_empty() {
                    let text_cols = row_cells.cell(line_len);
                    let mut x = text_x + text_cols as u16 + 2;
                    let right = inner.x + inner.width;
                    let style = Style::default()
                        .fg(self.theme.ignored_fg())
                        .add_modifier(Modifier::UNDERLINED);
                    for (n, idx) in lenses.into_iter().enumerate() {
                        if n > 0 && x + 3 < right {
                            buf.set_string(
                                x,
                                y,
                                " | ",
                                Style::default().fg(self.theme.ignored_fg()),
                            );
                            x += 3;
                        }
                        if x >= right {
                            break;
                        }
                        let title = &self.code_lenses[idx].title;
                        let shown: String = title.chars().take((right - x) as usize).collect();
                        let w = unicode_width::UnicodeWidthStr::width(shown.as_str()) as u16;
                        buf.set_string(x, y, &shown, style);
                        self.code_lens_spans.push((y, x..x + w, idx));
                        x += w;
                    }
                }
            }

            // Live Run trailer: what this line did on the last run, painted
            // where the debugger's values go and yielding to them, since a
            // paused session is the more specific answer.
            if row_end >= line_len
                && !ghost_here
                && !self.inline_values.contains_key(&line_idx)
                && let Some(note) = self.live_note(line_idx)
            {
                let text_cols = row_cells.cell(line_len);
                let mut x = text_x + text_cols as u16 + 2;
                let right = inner.x + inner.width;
                for (text, kind) in note {
                    if x >= right {
                        break;
                    }
                    let color = match kind {
                        crate::live_run::Kind::Value => self.theme.ignored_fg(),
                        crate::live_run::Kind::Output => {
                            self.theme.ui(Color::Rgb(0x3b, 0x9e, 0xff))
                        }
                        crate::live_run::Kind::Error => self.theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c)),
                        crate::live_run::Kind::Timeout => {
                            self.theme.ui(Color::Rgb(0xcc, 0xa7, 0x00))
                        }
                    };
                    let avail = (right - x) as usize;
                    let shown: String = text.chars().take(avail).collect();
                    let width = unicode_width::UnicodeWidthStr::width(shown.as_str()) as u16;
                    buf.set_string(
                        x,
                        y,
                        &shown,
                        Style::default().fg(color).add_modifier(Modifier::ITALIC),
                    );
                    x = x.saturating_add(width + 2);
                }
            }

            // The cursor itself is drawn by the host terminal as a hardware
            // caret (DECSCUSR `BlinkingBar`); App calls
            // `frame.set_cursor_position(...)` so the blink/overlay never
            // hides the underlying character.
        }

        // Rulers (#1524, VS Code `editor.rulers`): a dim │ at each ruler
        // column down the whole text area, past the end of the file too.
        // Blank cells only, so text always wins, as with the indent guides;
        // a long line runs over its ruler, which still shows the limit. The
        // column is fixed on screen: the flat path shifts it by the
        // horizontal scroll, and in wrap mode every visual row carries it.
        if !self.rulers.is_empty() {
            let ruler_fg = self.theme.ruler();
            let shift = if wrap { 0 } else { self.scroll_col };
            for row in 0..text_height {
                if matches!(visual_rows.get(row), Some(VisRow::Box { .. })) {
                    continue;
                }
                let y = inner.y + row as u16;
                for &ruler in &self.rulers {
                    let Some(col) = usize::from(ruler).checked_sub(shift) else {
                        continue;
                    };
                    if col >= text_width as usize {
                        continue;
                    }
                    let cell = &mut buf[(text_x + col as u16, y)];
                    if cell.symbol() != " " {
                        continue;
                    }
                    cell.set_symbol("\u{2502}");
                    let style = cell.style().fg(ruler_fg).remove_modifier(Modifier::ITALIC);
                    cell.set_style(style);
                }
            }
        }

        // Ghost caret name tags (VS Code Live Share): while a peer's caret is
        // inside its fade window the App feeds a label here, painted on the
        // visual row above the caret — below when the caret sits on the
        // viewport's top row — at the caret's on-screen column. Painted after
        // the row loop (a tag overlays a neighbouring row's content) but
        // before sticky scroll, which floats above everything.
        for (cl, cc, name, color) in &self.ghost_caret_labels {
            let Some(caret_vidx) = visual_rows.iter().position(|r| {
                matches!(r, VisRow::Text { line: li, start: s, end: e }
                    if *li == *cl && *cc >= *s
                        && (*cc < *e || (*cc == *e && *e == self.line_char_len(*li))))
            }) else {
                continue;
            };
            let label_vidx = if caret_vidx > 0 {
                caret_vidx - 1
            } else {
                caret_vidx + 1
            };
            if label_vidx >= visual_rows.len() {
                continue;
            }
            // The caret's on-screen column: translate through the caret row's
            // own inlay map, exactly as its ghost cell was painted.
            let VisRow::Text {
                start: caret_start,
                end: caret_end,
                ..
            } = visual_rows[caret_vidx]
            else {
                continue;
            };
            if *cc < caret_start {
                continue;
            }
            let caret_raw = &self.lines[*cl];
            let hint_cap = caret_end.min(self.line_char_len(*cl));
            row_cells.fill(
                &caret_raw[byte_index_of_char(caret_raw, caret_start)
                    ..byte_index_of_char(caret_raw, caret_end)],
                caret_start,
                self.row_origin(caret_raw, caret_start),
                self.row_inlay_spans(*cl)
                    .iter()
                    .filter(|(hc, _, _)| *hc >= caret_start && *hc <= hint_cap)
                    .map(|(hc, l, _)| (*hc, l.chars().count())),
            );
            let x0 = row_cells.caret(*cc) as u16;
            let y = inner.y + label_vidx as u16;
            let style = Style::default()
                .fg(Color::Black)
                .bg(*color)
                .add_modifier(Modifier::BOLD);
            for (i, ch) in name.chars().enumerate() {
                let x = x0 + i as u16;
                if x >= text_width {
                    break;
                }
                buf[(text_x + x, y)].set_char(ch).set_style(style);
            }
        }

        // Sticky scroll: pin the enclosing scope headers to the top of the
        // viewport, overpainting the topmost content rows (VS Code's sticky
        // widget floats over the content the same way). Non-wrap only; the app
        // supplies the header lines outermost-first, already filtered to those
        // scrolled above the viewport.
        self.sticky_click_rows.clear();
        if !wrap && !self.sticky_lines.is_empty() && inner.height > 1 {
            let bg = self.theme.sticky_scroll_bg();
            let text_w = (inner.x + inner.width).saturating_sub(text_x);
            // Never paint over the caret's own row: a caret under the band
            // cannot be seen at all. `caret_floor_row` keeps a scroll from
            // parking it there, so this normally trims nothing; it still has to
            // hold for the frames where the caret arrived some other way.
            //
            // The caret's PAINTED row, from the layout built above — counting
            // buffer lines instead would overstate it whenever a collapsed fold
            // or a comment box sits between the top of the viewport and the
            // caret, and the band would paint straight over it. `None` means
            // the caret's line is not painted at all (scrolled off, or hidden
            // inside a fold), so nothing needs to be kept clear.
            let rows_avail = (inner.height - 1) as usize;
            let caret_row = self
                .last_wrap_rows
                .iter()
                .position(|r| matches!(r, VisRow::Text { line, .. } if *line == self.cursor_row));
            let max_rows = match caret_row {
                Some(row) => rows_avail.min(row),
                None => rows_avail,
            };
            let sticky = self.sticky_lines.clone();
            for (i, &line) in sticky.iter().take(max_rows).enumerate() {
                let y = inner.y + i as u16;
                for x in inner.x..inner.x + inner.width {
                    buf[(x, y)].set_symbol(" ");
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
                let line_no = format!(
                    "{:>width$} ",
                    self.gutter_number(line as usize)
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                    width = gutter_width.saturating_sub(1) as usize
                );
                buf.set_string(
                    inner.x,
                    y,
                    &line_no,
                    Style::default().fg(self.theme.line_number_fg()).bg(bg),
                );
                self.paint_highlighted_line(buf, text_x, y, text_w, line as usize);
                self.sticky_click_rows.push((y, line));
            }
            // The band just overpainted those rows, so any [Accept …] span
            // recorded there no longer matches what is on screen — a click
            // on a sticky header must never resolve an unseen conflict.
            let band_end = inner.y + sticky.len().min(max_rows) as u16;
            self.merge_action_spans
                .retain(|(y, _, _, _)| *y >= band_end);
        }

        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
            // Overview ruler: ticks for the whole file painted over the track
            // the thumb rides on, so problems and search hits off screen are
            // still visible. Computed first so `self` is free while painting.
            if self.overview_ruler {
                let ticks = self.overview_ruler_rows_wrapped(
                    metrics.area.height,
                    wrap.then_some(text_width as usize),
                );
                paint_overview_ruler(buf, metrics.area, &ticks, self.theme);
            }
        }
        if let Some(metrics) = hbar_metrics {
            scrollbar::render_horizontal(buf, metrics, self.focused, self.theme);
        }
    }
}

impl Editor {
    /// Paint logical line `line_idx`'s syntax/semantic-highlighted text at row
    /// `y` from column 0 (no horizontal scroll), clipped to `width`. Used by
    /// the sticky-scroll bar to render pinned scope headers. Assumes the row
    /// background is already filled by the caller.
    fn paint_highlighted_line(
        &self,
        buf: &mut Buffer,
        text_x: u16,
        y: u16,
        width: u16,
        line_idx: usize,
    ) {
        let Some(raw) = self.lines.get(line_idx) else {
            return;
        };
        let empty: Vec<HiSpan> = Vec::new();
        let line_spans = self.highlights.get(line_idx).unwrap_or(&empty);
        let sem_spans = self.semantic_overlay.get(line_idx).unwrap_or(&empty);
        let merged = merge_overlay(line_spans, sem_spans);
        let raw_bytes = raw.len();
        let clamped: Vec<HiSpan> = merged
            .into_iter()
            .filter_map(|sp| sp.clipped(raw_bytes))
            .collect();
        let spans = build_line_spans(raw, &clamped, 0);
        buf.set_line(text_x, y, &Line::from(spans), width);
    }

    /// If `(col, row)` lands on a pinned sticky-scroll header, return the
    /// logical line to jump to. `None` when the row is not a sticky header.
    pub fn sticky_line_at(&self, col: u16, row: u16) -> Option<u32> {
        if col < self.last_inner.x || col >= self.last_inner.x + self.last_inner.width {
            return None;
        }
        self.sticky_click_rows
            .iter()
            .find(|&&(y, _)| y == row)
            .map(|&(_, line)| line)
    }

    /// Absolute (column, row) of the editor's cursor in screen coordinates,
    /// or `None` if the cursor is outside the visible viewport. Used by
    /// `App::render` to position the host terminal's hardware caret.
    pub fn cursor_screen_pos(&self) -> Option<(u16, u16)> {
        if self.last_inner.height == 0 {
            return None;
        }
        self.sync_tab_cells();
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        if self.wrap_enabled() {
            // Find the visual row holding the cursor in the layout the last
            // render captured (a logical line spans several wrapped rows). A
            // column on a wrap boundary belongs to the next row's start.
            let idx = self.last_wrap_rows.iter().position(|r| {
                matches!(r, VisRow::Text { line, start, end }
                    if *line == self.cursor_row
                        && self.cursor_col >= *start
                        && (self.cursor_col < *end
                            || (self.cursor_col == *end
                                && *end == self.line_char_len(*line))))
            })?;
            let (_, start, _) = self.text_row(idx)?;
            // The caret is valid on every cell of the segment AND one past
            // its last cell (end of line / blank line — where the user is
            // about to type). Bail only when the cell would fall outside the
            // text column: past the pane edge or onto the vertical scrollbar.
            // In cells, as the row was painted: wide characters before the
            // caret push it right by two each.
            let visible_col = RowCells::of_line_at(&self.lines[self.cursor_row], start, 0)
                .text_before(self.cursor_col);
            let x = text_x as usize + visible_col;
            let right = (self.last_inner.x + self.last_inner.width)
                .saturating_sub(u16::from(self.last_scrollbar.width > 0))
                as usize;
            if x >= right {
                return None;
            }
            return Some((x as u16, self.last_inner.y + idx as u16));
        }
        // Non-wrap: one row per line, horizontally scrolled by `scroll_col`;
        // comment boxes shift the rows below them, so map through the
        // painted layout when one exists.
        if self.cursor_row < self.scroll {
            return None;
        }
        let row_in_view = if self.last_wrap_rows.is_empty() {
            self.cursor_row - self.scroll
        } else {
            self.last_wrap_rows
                .iter()
                .position(|r| matches!(r, VisRow::Text { line, .. } if *line == self.cursor_row))?
        };
        if (row_in_view as u16) >= self.last_inner.height {
            return None;
        }
        let text_width = self
            .last_inner
            .width
            .saturating_sub(self.last_gutter_width + 2 + u16::from(self.last_scrollbar.width > 0));
        if text_width == 0 || self.cursor_col < self.scroll_col {
            return None;
        }
        // Inlay-hint cells before the caret shift it right. Strictly-before
        // (`hc < cursor_col`, not `<=`): a caret AT a hint's anchor sits left
        // of the hint, like VS Code, so typing there pushes the hint along.
        let extra = self.inlay_cells_before_cursor(self.cursor_row, self.scroll_col);
        let visible_col = self
            .lines
            .get(self.cursor_row)
            .map_or(self.cursor_col - self.scroll_col, |l| {
                RowCells::of_line(l, self.scroll_col).text_before(self.cursor_col)
            })
            + extra;
        if visible_col >= text_width as usize {
            return None;
        }
        Some((
            text_x + visible_col as u16,
            self.last_inner.y + row_in_view as u16,
        ))
    }

    /// Hint cells spliced between `scroll_col` and the caret on `line`
    /// (anchors strictly before `cursor_col`, at or past `scroll_col`).
    fn inlay_cells_before_cursor(&self, line: usize, scroll_col: usize) -> usize {
        self.row_inlay_spans(line)
            .iter()
            .filter(|(hc, _, _)| *hc >= scroll_col && *hc < self.cursor_col)
            .map(|(_, l, _)| l.chars().count())
            .sum()
    }
}

/// Overpaint every match of `needle` in `raw_line` with the search-match
/// style, honouring `opts` (case-sensitive / whole-word / regex). Delegates
/// to `split_for_highlight` so the highlight rule stays 1:1 with the
/// search-engine matcher; column conversion uses `chars().count()` over
/// the byte prefix to stay correct for Unicode.
/// Hint cells inserted at or before buffer column `c` on this row: the
/// display-column shift every overlay painter applies past inlay hints.
/// `hints` is the row's visible (anchor col, label cells) set.
fn inlay_cells_before(hints: &[(usize, usize)], c: usize) -> usize {
    hints
        .iter()
        .filter(|(hc, _)| *hc <= c)
        .map(|(_, n)| n)
        .sum()
}

/// Hint cells strictly before buffer column `c`: the shift for a CARET at
/// `c`, which sits left of a hint anchored exactly there (a character at
/// `c` sits right of it — that is [`inlay_cells_before`]).
fn inlay_cells_strictly_before(hints: &[(usize, usize)], c: usize) -> usize {
    hints.iter().filter(|(hc, _)| *hc < c).map(|(_, n)| n).sum()
}

/// Where buffer columns land on one painted text row (#873): the text is
/// drawn by display width, a CJK character or most emoji taking two cells
/// ([`char_cells`]), with inlay-hint labels spliced between characters.
/// Every overlay laid over the text maps its columns through this, or it
/// lands one cell left of its glyph per wide character before it.
#[derive(Debug, Default)]
struct RowCells {
    /// Buffer column of the row's first character: the horizontal scroll, or
    /// a wrapped segment's start.
    start: usize,
    /// Cells of the row's text before each of its characters, plus one entry
    /// for where that text ends. Empty means one cell per character.
    before: Vec<usize>,
    /// The row's inlay hints as (anchor column, label cells).
    hints: Vec<(usize, usize)>,
}

impl RowCells {
    /// The geometry of a row showing `line` from buffer column `start`, with
    /// no inlay hints.
    fn of_line(line: &str, start: usize) -> Self {
        Self::of_line_at(line, start, cells_before(line, start))
    }

    /// [`Self::of_line`] for a row whose first cell is display cell `origin`
    /// of its tab stops: 0 for a wrapped row, the cells scrolled past for a
    /// horizontally scrolled one.
    fn of_line_at(line: &str, start: usize, origin: usize) -> Self {
        let mut row = Self::default();
        row.fill(&line[byte_index_of_char(line, start)..], start, origin, []);
        row
    }

    /// Refill for a row painting `text` from buffer column `start` with
    /// `hints` spliced in, reusing the allocations.
    fn fill(
        &mut self,
        text: &str,
        start: usize,
        origin: usize,
        hints: impl IntoIterator<Item = (usize, usize)>,
    ) {
        self.start = start;
        self.before.clear();
        self.before.push(0);
        let mut cells = 0;
        // Tab stops count from the row's left edge, `origin` cells before its
        // first character. A grapheme cluster's first character takes the
        // whole cluster's cells and the rest none ([`char_cells_from`]).
        for (_, w) in char_cells_from(text, origin) {
            cells += w;
            self.before.push(cells);
        }
        self.hints.clear();
        self.hints.extend(hints);
    }

    /// Cells of the row's own text before buffer column `col`: none at or
    /// before the row's start, and one per column past the end of its text,
    /// where the selection's newline cell sits.
    fn text_before(&self, col: usize) -> usize {
        let i = col.saturating_sub(self.start);
        match self.before.get(i) {
            Some(&cells) => cells,
            None => {
                let end = self.before.len().saturating_sub(1);
                self.before.last().copied().unwrap_or(0) + (i - end)
            }
        }
    }

    /// The first cell of the character at `col`: right of a hint anchored at
    /// or before it.
    fn cell(&self, col: usize) -> usize {
        self.text_before(col) + inlay_cells_before(&self.hints, col)
    }

    /// The cell a caret at `col` sits on: left of a hint anchored exactly
    /// there, like the primary caret (`cursor_screen_pos`), marking the cell
    /// its typed character will land on.
    fn caret(&self, col: usize) -> usize {
        self.text_before(col) + inlay_cells_strictly_before(&self.hints, col)
    }

    /// The cell just past a range ending before `col`: shifted by the hints
    /// before its LAST character, so a hint anchored at `col` stays outside.
    fn end(&self, col: usize) -> usize {
        self.text_before(col) + inlay_cells_before(&self.hints, col.saturating_sub(1))
    }

    /// Cells the character at `col` takes: two for a wide one, none for a
    /// combining mark, one past the end of the text.
    fn width(&self, col: usize) -> usize {
        self.text_before(col + 1) - self.text_before(col)
    }
}

/// Restyle every cell the character at buffer column `col` occupies on a
/// text row: [`paint_log_cells`] for the editor's row geometry. Returns false
/// once the row's right edge is reached, so a caller walking columns can stop.
fn paint_row_cells(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    row: &RowCells,
    col: usize,
    restyle: impl Fn(Style) -> Style,
) -> bool {
    let first = row.cell(col);
    let width = row.width(col);
    // A character that does not fit WHOLE was never painted (`set_line`
    // stops before it), so its band would colour an empty cell.
    if first >= usize::from(text_width) || first + width > usize::from(text_width) {
        return false;
    }
    for x in first..first + width {
        let cell = &mut buf[(text_x + x as u16, y)];
        cell.set_style(restyle(cell.style()));
    }
    true
}

/// The syntax-highlighted spans of `raw`'s character range `[from, to)`,
/// clipped out of the line's merged highlight spans. Paints the text
/// segments between spliced inlay-hint labels.
fn inlay_text_segment<'a>(
    raw: &'a str,
    merged: &[HiSpan],
    row: (usize, usize),
    from: usize,
    to: usize,
) -> Vec<Span<'a>> {
    let byte_from = byte_index_of_char(raw, from);
    let byte_to = byte_index_of_char(raw, to);
    let seg = &raw[byte_from..byte_to];
    let seg_bytes = byte_to - byte_from;
    let shifted: Vec<HiSpan> = shift_spans_for_view(merged, byte_from)
        .into_iter()
        .filter_map(|sp| sp.clipped(seg_bytes))
        .collect();
    // Tab stops count from the row's left edge: its first char is `row.0`,
    // which sits at display cell `row.1` (hint labels do not count).
    let row_start = byte_index_of_char(raw, row.0).min(byte_from);
    let origin = row.1 + cells_from(&raw[row_start..byte_from], row.1);
    build_line_spans(seg, &shifted, origin)
}

/// The (inactive, active) find-highlight styles, shared by the editor's
/// highlighter and the rendered log's so the two tabs read the same. A
/// boxed theme's solid blocks; Croft Loch tints each cell instead
/// ([`crate::widgets::surface::match_style`]).
fn search_highlight_styles(theme: crate::theme::Theme) -> (Style, Style) {
    let inactive = Style::default()
        .fg(Color::Black)
        .bg(theme.ui(Color::Rgb(0xff, 0xd7, 0x4a)))
        .add_modifier(Modifier::BOLD);
    let active = Style::default()
        .fg(Color::Black)
        .bg(theme.ui(Color::Rgb(0xff, 0x8c, 0x2a)))
        .add_modifier(Modifier::BOLD);
    (inactive, active)
}

// Render helper: each argument is an independent painting input (buffer,
// geometry, text, styling); bundling them into a struct would add indirection
// without improving clarity.
#[allow(clippy::too_many_arguments)]
fn paint_search_highlight(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    raw_line: &str,
    needle: &str,
    opts: crate::widgets::search::SearchOpts,
    row: &RowCells,
    active_match_on_line: Option<(usize, usize)>,
    cells: Option<&crate::cell_map::CellMap>,
    window: Option<(usize, usize)>,
    theme: crate::theme::Theme,
) {
    if needle.is_empty() {
        return;
    }
    let (inactive_style, active_style) = search_highlight_styles(theme);
    // Croft Loch tints each matched cell's own background amber and keeps
    // its syntax ink where it stays legible ([`surface::match_style`]); a
    // boxed theme stamps the solid find style over it.
    let tones = crate::widgets::surface::tones(theme);
    let restyle = |active: bool| {
        move |s: Style| match tones {
            Some(t) => crate::widgets::surface::match_style(s, t, theme, active),
            None if active => active_style,
            None => inactive_style,
        }
    };
    let segments = crate::widgets::search::split_for_highlight(raw_line, needle, opts);
    // `abs_col` tracks the absolute character index in the original line.
    // Columns before the row's start are scrolled out; the rest map
    // through the row's geometry and are painted inside `text_width`.
    let mut abs_col: usize = 0;
    for (chunk, is_match) in segments {
        let chunk_cols = chunk.chars().count();
        // Find in Selection (#1577): only matches wholly inside the window.
        let in_window = window.is_none_or(|(lo, hi)| abs_col >= lo && abs_col + chunk_cols <= hi);
        if is_match && in_window {
            let is_active =
                active_match_on_line.is_some_and(|(c, l)| c == abs_col && l == chunk_cols);
            let style = restyle(is_active);
            for c in 0..chunk_cols {
                let absolute = abs_col + c;
                if absolute < row.start {
                    continue;
                }
                // A rendered log paints by ratatui's grapheme rules (#404),
                // so its band goes through the same cell map the text did;
                // the editor's text path goes through its row geometry.
                if let Some(cells) = cells {
                    if !paint_log_cells(buf, text_x, text_x + text_width, y, cells, absolute, style)
                    {
                        break;
                    }
                    continue;
                }
                if !paint_row_cells(buf, text_x, y, text_width, row, absolute, style) {
                    break;
                }
            }
        }
        abs_col = abs_col.saturating_add(chunk_cols);
        if row.cell(abs_col) >= usize::from(text_width) {
            break;
        }
    }
}

use crate::widgets::editor_find::find_window;

/// Paint VS Code-style "selection highlight": a muted blue box over every
/// occurrence of `needle` on this line. Matches the literal selected text
/// case-sensitively (like VS Code's `editor.selectionHighlight`). Only the
/// background is repainted, so syntax foreground colours show through. The
/// active selection itself is overpainted afterwards by
/// `paint_selection_band` with a brighter blue, giving the two-tone look.
// Render helper: same independent-inputs shape as paint_search_highlight.
#[allow(clippy::too_many_arguments)]
fn paint_selection_occurrences(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    raw_line: &str,
    needle: &str,
    row: &RowCells,
    theme: crate::theme::Theme,
) {
    if needle.is_empty() {
        return;
    }
    let bg = theme.ui(Color::Rgb(0x37, 0x61, 0x8e));
    let needle_cols = needle.chars().count();
    let mut search_from = 0usize;
    while let Some(rel) = raw_line[search_from..].find(needle) {
        let byte_start = search_from + rel;
        let char_start = raw_line[..byte_start].chars().count();
        for absolute in char_start.max(row.start)..char_start + needle_cols {
            if !paint_row_cells(buf, text_x, y, text_width, row, absolute, |s| s.bg(bg)) {
                break;
            }
        }
        search_from = byte_start + needle.len();
    }
}

/// Paint a software block cursor for a secondary multi-cursor caret on cell
/// `col` of row `y`'s text. Uses a reversed bar so it reads as a caret
/// distinct from the host terminal's hardware caret on the primary cursor.
fn paint_block_cursor(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    col: usize,
    theme: crate::theme::Theme,
) {
    if col >= usize::from(text_width) {
        return;
    }
    let cell = &mut buf[(text_x + col as u16, y)];
    // The glyph under the block takes the canvas on a `surfaces` theme
    // rather than the host palette's named black.
    let fg = crate::widgets::surface::tones(theme).map_or(Color::Black, |t| t.canvas());
    cell.set_style(
        Style::default()
            .fg(fg)
            .bg(theme.secondary_caret_bg())
            .add_modifier(Modifier::BOLD),
    );
}

/// Paint one collaborator's caret cell in their color (multiplayer ghost
/// caret): same shape and clipping as `paint_block_cursor`, but tinted per
/// participant so everyone can see where the others are.
fn paint_ghost_caret(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    col: usize,
    color: Color,
) {
    if col >= usize::from(text_width) {
        return;
    }
    let cell = &mut buf[(text_x + col as u16, y)];
    cell.set_style(
        Style::default()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    );
}

/// Tint the single bracket cell on cell `col` of row `y`'s text with the
/// theme's bracket-match background, leaving the glyph's foreground intact so
/// the bracket stays readable. No-op when the cell is past the row's edge.
fn paint_bracket_match(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    col: usize,
    theme: crate::theme::Theme,
) {
    if col >= usize::from(text_width) {
        return;
    }
    let cell = &mut buf[(text_x + col as u16, y)];
    cell.set_style(cell.style().bg(theme.bracket_match_bg()));
}

/// A merge-conflict accept button (#1169): its label, the shorter one a row
/// too narrow for every full label gets, and its fill.
struct AcceptChip {
    label: String,
    short: String,
    fill: Color,
}

/// Tint every cell of one text row, keeping the glyphs and their colours.
fn paint_full_row_bg(buf: &mut Buffer, text_x: u16, y: u16, text_width: u16, bg: Color) {
    for col in 0..text_width {
        let x = text_x + col;
        let cell = &mut buf[(x, y)];
        cell.set_style(cell.style().bg(bg));
    }
}

fn paint_selection_band(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    start_char: usize,
    end_char: usize,
    theme: crate::theme::Theme,
) {
    let bg = theme.ui(Color::Rgb(0x26, 0x4f, 0x78));
    let s = start_char.min(text_width as usize);
    let e = end_char.min(text_width as usize);
    if e <= s {
        return;
    }
    for col in s..e {
        let x = text_x + col as u16;
        let cell = &mut buf[(x, y)];
        cell.set_style(cell.style().bg(bg));
    }
}

/// Paint a coloured underline across `[start_char, end_char)` of a rendered
/// row to mark an LSP diagnostic. The colour encodes severity, matching VS
/// Code's palette (red error, yellow warning, blue info, faint grey for a
/// hint's short marker). Uses the
/// terminal's separate underline colour so the glyph's foreground (its syntax
/// or semantic colour) is left untouched, the way VS Code's squiggle sits
/// under unchanged text.
#[allow(clippy::too_many_arguments)]
fn paint_diagnostic_underline(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    start_char: usize,
    end_char: usize,
    severity: crate::lsp::manager::DiagnosticSeverity,
    theme: crate::theme::Theme,
) {
    use crate::lsp::manager::DiagnosticSeverity;
    let color = match severity {
        DiagnosticSeverity::Error => theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c)),
        DiagnosticSeverity::Warning => theme.ui(Color::Rgb(0xcc, 0xa7, 0x00)),
        DiagnosticSeverity::Information => theme.ui(Color::Rgb(0x3b, 0x9e, 0xff)),
        DiagnosticSeverity::Hint => theme.ui(Color::Rgb(0x80, 0x80, 0x80)),
    };
    let s = start_char.min(text_width as usize);
    let e = end_char.min(text_width as usize);
    if e <= s {
        return;
    }
    for col in s..e {
        let x = text_x + col as u16;
        let cell = &mut buf[(x, y)];
        cell.set_style(
            cell.style()
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(color),
        );
    }
}

/// A diagnostic's message for its hover (#1540): VS Code's `source(code)`
/// after it, and the rule's documentation link below, when the server sent
/// them.
fn message_with_code(d: &crate::lsp::manager::Diagnostic) -> String {
    let Some(code) = &d.code else {
        return d.message.clone();
    };
    let mut text = format!("{}  {}", d.message, code.label(""));
    if let Some(href) = &code.href {
        text.push('\n');
        text.push_str(href);
    }
    text
}

/// Draw a tagged diagnostic's look across `[start_char, end_char)` of a
/// rendered row (#1522): Unnecessary code (unused, unreachable) faded, as VS
/// Code draws it at reduced opacity, and Deprecated code struck through. The
/// glyphs keep their syntax colour.
fn paint_diagnostic_tags(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    start_char: usize,
    end_char: usize,
    tags: crate::lsp::manager::DiagnosticTags,
) {
    let mut look = Modifier::empty();
    if tags.unnecessary {
        look |= Modifier::DIM;
    }
    if tags.deprecated {
        look |= Modifier::CROSSED_OUT;
    }
    let s = start_char.min(text_width as usize);
    let e = end_char.min(text_width as usize);
    for col in s..e {
        let cell = &mut buf[(text_x + col as u16, y)];
        cell.set_style(cell.style().add_modifier(look));
    }
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

/// Classify a line as a fold-region marker (#254): after leading
/// whitespace and a comment introducer (`//`, `#`, `--`, `;`, `/*`, with
/// an optional extra `#` as in `// #region`), the word `region`
/// (Some(true)) or `endregion` (Some(false)) at a word boundary — the
/// language-agnostic `#region` family VS Code folds.
fn region_marker(line: &str) -> Option<bool> {
    let trimmed = line.trim_start();
    let mut rest = None;
    for intro in ["//", "#", "--", ";", "/*"] {
        if let Some(r) = trimmed.strip_prefix(intro) {
            rest = Some(r);
            break;
        }
    }
    let s = rest?.trim_start();
    let s = s.strip_prefix('#').unwrap_or(s);
    let boundary = |r: &str| !r.starts_with(|c: char| c.is_alphanumeric() || c == '_');
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("endregion") {
        return boundary(&s[9..]).then_some(false);
    }
    if lower.starts_with("region") {
        return boundary(&s[6..]).then_some(true);
    }
    None
}

/// Whether a line is a full-line comment for the fallback comment-run
/// scanner. Rust/C attributes (`#[...]`, `#![...]`) are code, not
/// comments, despite the leading `#`.
fn comment_line(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("#[") || t.starts_with("#![") {
        return false;
    }
    ["//", "#", "--", ";", "/*", "*/", "* "]
        .iter()
        .any(|intro| t.starts_with(intro))
        || t == "*"
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
