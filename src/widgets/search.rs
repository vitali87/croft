use globset::{Glob, GlobSet, GlobSetBuilder};
use grep_matcher::{LineMatchKind, LineTerminator, Match, Matcher, NoCaptures, NoError};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, MmapChoice, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{WalkBuilder, WalkState};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use unicode_width::UnicodeWidthStr;

use crate::glob_list::split_globs;

mod fields;
mod highlight;
mod hit_test;
mod matcher;
mod path_filter;
mod render;
mod replace;
mod results;
mod walk;
mod worker;
pub use highlight::split_for_highlight;
pub(crate) use matcher::Pattern;
use matcher::*;
pub use path_filter::PathFilter;
use path_filter::*;
pub use replace::preserve_case;
use replace::*;
pub(crate) use walk::as_typed;
pub use walk::line_may_match;
pub use walk::search_workspace_streaming_filtered;
use walk::*;
pub use worker::SearchEvent;
pub use worker::SearchRequest;
pub use worker::search_worker_loop;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    /// 1-indexed line number, the way humans / editors talk about lines.
    pub line_no: usize,
    /// The matched line without its line break, length capped, and with
    /// any indent before the first match trimmed.
    pub line_text: String,
    /// How many times the query matches on this line, counted as Replace All
    /// replaces them. The header counts these, as VS Code does, not lines
    /// (#860).
    pub matches: usize,
}

/// Mode toggles that drive `search_workspace`. Mirror VS Code's three
/// search input toggles, in the same left-to-right order:
///   - `case_sensitive` (Aa)
///   - `whole_word` (ab with underline)
///   - `use_regex` (.*)
///   - `preserve_case` (AB, on the replace row): each replacement takes the
///     case of the text it replaces (#1612)
///
/// All-false matches the original case-insensitive substring behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SearchOpts {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
    pub preserve_case: bool,
}

/// How far the Search panel's run got (#2608).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchProgress {
    /// Its hits are still streaming in.
    Running,
    /// It looked at every file: the list is the whole answer.
    Finished,
    /// It was stopped before it looked at every file, when the panel left
    /// Search, or the workspace's folders changed after it ran: the list is
    /// not the whole answer, and coming back to Search runs it again.
    Cancelled,
    /// Its scan crashed before it looked at every file: the list is not the
    /// whole answer, and Refresh runs it again (#2611).
    Crashed,
}

/// Side-panel widget shown when the active sidebar view is "Search".
pub struct SearchPanel {
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub selected: usize,
    pub scroll: usize,
    pub focused: bool,
    /// True under the Black theme: overpaint the focused outer border with the
    /// orange→green brand gradient instead of the legacy solid blue. Set by the
    /// app's focus/theme sync.
    pub focus_gradient: bool,
    /// Active color theme; drives the scrollbar track/thumb colors. Set by the
    /// app's theme sync.
    pub theme: crate::theme::Theme,
    pub last_inner: Rect,
    pub last_area: Rect,
    pub root: PathBuf,
    /// Every workspace root (#147); `root` stays the primary. Display
    /// strips a hit against its OWNING root, prefixed with the root's
    /// name when more than one folder is open.
    pub roots: Vec<PathBuf>,
    pub opts: SearchOpts,
    /// `(query, opts, error)` behind `query_error`, so the caption doesn't
    /// rebuild the regex every frame.
    query_error_memo: Option<(String, SearchOpts, Option<String>)>,
    /// Per-toggle absolute screen column captured by the most recent render.
    /// Used by `App::handle_mouse` to map clicks on `Aa`, `ab`, `.*` into
    /// flag flips. Zero means the row was too narrow to render that toggle.
    pub toggle_case_x: u16,
    pub toggle_word_x: u16,
    pub toggle_regex_x: u16,
    /// The replace row's `AB` (Preserve Case) chip, 2 cells from
    /// `toggle_preserve_x` on `toggle_preserve_y`; x 0 while the row is hidden.
    pub toggle_preserve_x: u16,
    pub toggle_preserve_y: u16,
    pub toggle_y: u16,
    pub paste_button_x: u16,
    pub paste_button_y: u16,
    pub paste_button_w: u16,
    pub selection: Option<(usize, usize)>,
    /// Hit-test rect captured during the last render of the result-list
    /// scrollbar. Empty when there's no overflow. Used by the mouse
    /// handler to route track clicks / drags to `scroll_to_bar_y`.
    pub last_scrollbar: Rect,
    /// Render-time pointer cell, fed from `App::pointer_cell` each frame so a
    /// hovered (but unselected) result row can lift to the hover background.
    pub hover_pointer: Option<(u16, u16)>,

    // --- Replace + files-to-include/exclude (VS Code Search sidebar) ---
    /// Replacement text. Editable only while the Replace row is expanded.
    pub replace: String,
    /// "files to include" glob list (comma-separated), shown when the details
    /// (`...`) row is expanded.
    pub include: String,
    /// "files to exclude" glob list (comma-separated), shown when details is
    /// expanded.
    pub exclude: String,
    /// Whether the Replace row is expanded, toggled by the left chevron.
    pub replace_open: bool,
    /// Whether the include/exclude detail rows are expanded, toggled by `...`.
    pub details_open: bool,
    /// Which input field currently receives typed characters. Tab cycles
    /// through the visible fields; clicking an input focuses it directly.
    pub field: SearchField,
    /// The number of the search run the panel shows: the worker's events
    /// for any other run are dropped (#2608).
    pub run: u64,
    /// How far that run got. Replace All goes ahead only once it is
    /// `Finished`: while hits still stream, or after the run stopped early,
    /// files the scan never reached would be silently missed (#124 review).
    pub progress: SearchProgress,
    /// Selection range within the currently focused field's text.
    pub field_selection: Option<(usize, usize)>,
    /// Caret of each field as a char index (#1729), indexed by
    /// [`SearchField::index`]. `None` is "at the end of the text, wherever
    /// that is now": a field nobody moved a caret in follows its text when
    /// the text is assigned from outside. Reads clamp, so a mid-text caret
    /// left past a shortened text falls back to the end too.
    carets: [Option<usize>; 4],
    /// Where each field's text starts on screen and the first char it shows,
    /// captured each render so a click can place the caret.
    field_origin: [(u16, usize); 4],

    // --- Hit-test geometry captured during render (absolute screen coords) ---
    /// The left expand chevron (toggles the Replace row).
    pub chevron_x: u16,
    pub chevron_y: u16,
    /// The replace-all action icon at the right of the Replace row.
    pub replace_all_x: u16,
    pub replace_all_y: u16,
    /// The `...` details toggle (expands include/exclude).
    pub ellipsis_x: u16,
    pub ellipsis_y: u16,
    /// The SEARCH header action icons (top-right): refresh re-runs the query,
    /// clear wipes the query + results. Single-cell hit targets.
    pub refresh_x: u16,
    pub refresh_y: u16,
    pub clear_x: u16,
    pub clear_y: u16,
    /// Content rects of each input box's editable area, for click-to-focus.
    pub query_input_rect: Rect,
    pub replace_input_rect: Rect,
    pub include_input_rect: Rect,
    pub exclude_input_rect: Rect,
    /// The "use the project's exclusions" checkbox row under the exclude box
    /// (#1345); zero-sized when the project excludes nothing.
    pub project_exclude_rect: Rect,
    /// The project's exclusion globs (`files_exclude` then
    /// `search_exclude`, #1345), applied on top of the exclude box while
    /// `use_exclude_settings` is on.
    pub project_exclude: Vec<String>,
    /// VS Code's "Use Exclude Settings and Ignore Files" toggle: off, a
    /// search covers the folders the project hides.
    pub use_exclude_settings: bool,
    /// Row offset (from `inner.y`) where the results list starts. Recomputed
    /// each render because expanding Replace / details rows pushes results
    /// down. `results_viewport_height` and `hit_at_y` read this so scrolling
    /// and click mapping stay aligned with the rendered layout.
    pub results_start_offset: u16,
    /// The scroll the rows were drawn with (#2751). A click is matched
    /// against it: the screen shows it until the next frame, while a key or
    /// wheel step read in the same burst has already moved `scroll`.
    pub last_scroll: usize,
    /// Screen cell of the focused field's text caret, captured each render so
    /// the app can drive the host's blinking hardware caret there (matching
    /// the Source Control commit box). `None` when no field is focused or the
    /// panel has not laid out yet.
    caret_cell: Option<(u16, u16)>,
}

/// Which input field of the Search panel currently has the cursor. Drives
/// where typed characters land and which box draws the focus accent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SearchField {
    #[default]
    Query,
    Replace,
    Include,
    Exclude,
}

impl SearchField {
    fn index(self) -> usize {
        match self {
            SearchField::Query => 0,
            SearchField::Replace => 1,
            SearchField::Include => 2,
            SearchField::Exclude => 3,
        }
    }
}

impl SearchPanel {
    pub fn new(root: PathBuf) -> Self {
        Self {
            roots: vec![root.clone()],
            query: String::new(),
            hits: Vec::new(),
            selected: 0,
            scroll: 0,
            focused: false,
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            last_inner: Rect::default(),
            last_area: Rect::default(),
            root,
            opts: SearchOpts::default(),
            query_error_memo: None,
            toggle_case_x: 0,
            toggle_word_x: 0,
            toggle_regex_x: 0,
            toggle_preserve_x: 0,
            toggle_preserve_y: 0,
            toggle_y: 0,
            paste_button_x: 0,
            paste_button_y: 0,
            paste_button_w: 0,
            selection: None,
            last_scrollbar: Rect::default(),
            hover_pointer: None,
            replace: String::new(),
            include: String::new(),
            exclude: String::new(),
            replace_open: false,
            details_open: false,
            field: SearchField::Query,
            run: 0,
            progress: SearchProgress::Running,
            field_selection: None,
            carets: [None; 4],
            field_origin: [(0, 0); 4],
            chevron_x: 0,
            chevron_y: 0,
            replace_all_x: 0,
            replace_all_y: 0,
            ellipsis_x: 0,
            ellipsis_y: 0,
            refresh_x: 0,
            refresh_y: 0,
            clear_x: 0,
            clear_y: 0,
            query_input_rect: Rect::default(),
            replace_input_rect: Rect::default(),
            include_input_rect: Rect::default(),
            exclude_input_rect: Rect::default(),
            project_exclude_rect: Rect::default(),
            project_exclude: Vec::new(),
            use_exclude_settings: true,
            results_start_offset: 5,
            last_scroll: 0,
            caret_cell: None,
        }
    }
}

/// Identifies which of the search-mode toggles a click landed on, or
/// an `Alt` key named (#860).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchToggle {
    CaseSensitive,
    WholeWord,
    UseRegex,
    /// `AB` on the replace row (#1612).
    PreserveCase,
}

#[cfg(test)]
mod tests;
