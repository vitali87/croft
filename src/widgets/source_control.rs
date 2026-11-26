use crate::git::{ChangeEntry, ChangeKind, ChangeSection, GitStatus};
use crate::icons;
use crate::widgets::scrollbar;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Widget, Wrap},
};
use std::collections::BTreeSet;

mod buttons;
mod change_list;
mod clicks;
mod commit_menu;
mod empty_state;
mod message;
mod palette;
mod render;
mod repositories;
pub use buttons::primary_button_colors;
pub use buttons::render_button;
use buttons::*;
use change_list::*;
use clicks::*;
pub use commit_menu::CommitMenuItem;
use message::*;
use palette::*;
#[cfg(test)]
use render::*;
pub use repositories::RepoRow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowActionAreas {
    pub entry_idx: usize,
    /// Discard / stage are painted on a selected *unstaged* row; `unstage`
    /// on a selected *staged* row. The irrelevant rects are left default
    /// (zero-size, so they never hit-test) for the row's section.
    pub discard: Rect,
    pub stage: Rect,
    pub unstage: Rect,
}

pub struct SourceControlPanel {
    pub focused: bool,
    /// True under the Black theme: overpaint the focused outer border with the
    /// orange→green brand gradient instead of the legacy solid blue. Set by the
    /// app's focus/theme sync.
    pub focus_gradient: bool,
    /// Active color theme; drives the scrollbar track/thumb colors. Set by the
    /// app's theme sync.
    pub theme: crate::theme::Theme,
    pub message: String,
    pub message_cursor: usize,
    /// Leading characters scrolled out of view on the left of the
    /// commit-message input so the caret stays visible when the message is
    /// wider than the box. Recomputed every render from the cursor and the
    /// box width.
    pub message_scroll: usize,
    /// Message lines scrolled out of view above the commit-message input
    /// once the message has more lines than the box shows. Recomputed every
    /// render, like `message_scroll`, to keep the caret's line visible.
    pub message_line_scroll: usize,
    pub status: GitStatus,
    /// The prepared merge message last put in the box (#1282), whole
    /// (#1336), to tell an untouched seed from the user's own text.
    seeded_message: Option<String>,
    pub entries: Vec<ChangeEntry>,
    /// The multi-root repositories overview (#161), pushed by the App per
    /// frame from its per-folder git workers. Length below two hides the
    /// section (single-folder panels render exactly as before).
    pub repositories: Vec<RepoRow>,
    /// Frame-truth hit rects for the overview rows (cleared and
    /// re-recorded every render, #103): click switches the panel to that
    /// repository.
    pub repo_row_areas: Vec<(Rect, std::path::PathBuf)>,
    pub last_area: Rect,
    pub last_inner: Rect,
    pub last_input_area: Rect,
    pub last_button_area: Rect,
    /// Hit-test rect for the chevron split-button that opens the Commit
    /// dropdown (Commit & Push, Push, View …). Empty when the panel is in
    /// the no-repo state or the button row was clipped.
    pub last_commit_caret_area: Rect,
    pub last_list_area: Rect,
    pub last_scrollbar: Rect,
    /// Hit-test rect for the empty-state "Initialize Repository" button.
    /// Empty when the panel is in a git repo or when the panel is too
    /// small to draw the empty-state card.
    pub last_init_repo_button_area: Rect,
    /// Repositories in the workspace's immediate subfolders, as (label,
    /// absolute path), set by `App` while the workspace itself is in no
    /// repo (#1227). Non-empty, the empty-state card lists them with an
    /// Open button each in place of Initialize.
    pub nested_repos: Vec<(String, std::path::PathBuf)>,
    /// Hit-test rects for those Open buttons; empty whenever none paint.
    pub nested_repo_button_areas: Vec<(Rect, std::path::PathBuf)>,
    /// Cell rect reserved for the no-repo empty-state hero illustration.
    /// `App` paints an OSC-1337 inline PNG here on iTerm2-class terminals;
    /// the widget paints an ASCII Y-fork as fallback so the same rect
    /// reads as a logo on plain terminals too. Empty when the panel is
    /// in a git repo or the empty-state card is too small to allocate.
    pub last_hero_area: Rect,
    /// Set by `App` after probing the host terminal for OSC-1337 inline
    /// image support. When true, the empty-state render leaves the hero
    /// rect blank so the PNG owns those cells without competing with an
    /// ASCII fallback that would briefly flash through during resize.
    pub inline_hero_image_active: bool,
    pub scroll: usize,
    /// Index into `entries` of the currently-selected change, if any.
    /// Drives the row-highlight bg so the user can tell at a glance
    /// which entry the editor is showing the diff for.
    pub selected_change: Option<usize>,
    /// Indices of all rows currently selected (multi-select). Always
    /// includes `selected_change` when that is `Some`. Cmd+A populates this
    /// with every entry; a plain click resets it to the single clicked row.
    /// The Stage shortcut (Cmd+S) acts on this set; click on an action icon
    /// still acts on just that row.
    pub multi_selection: BTreeSet<usize>,
    /// The change list holds the keyboard instead of the message box
    /// (#871), as in VS Code: arrows move `selected_change` and Enter opens
    /// its diff. Tab, or Down on the message's last line, moves here; Tab,
    /// or Up on the first row, goes back.
    pub list_focused: bool,
    /// Status / error line painted below the button after a commit attempt.
    /// Cleared on the next refresh.
    pub commit_feedback: Option<String>,
    pub commit_feedback_is_error: bool,
    /// Hit-test rects for the inline action icons (Discard, Stage) painted
    /// on the selected unstaged row. `None` when no row is selected or the
    /// selected row is already staged. Cleared at the top of each render
    /// and re-populated for the visible selected row.
    pub last_row_actions: Option<RowActionAreas>,
    /// Render-time pointer cell, fed from `App::pointer_cell` each frame so a
    /// hovered (but unselected) change row can lift to the hover background.
    pub hover_pointer: Option<(u16, u16)>,
    /// Hit-test rect for the refresh pill at the right of the SOURCE CONTROL
    /// header row, mirroring the Remote Explorer refresh affordance. Clicking
    /// it forces an immediate, un-debounced git re-scan. Empty when the panel
    /// is in the no-repo state or the header row was clipped.
    pub header_refresh_btn: Rect,
    /// Hit-test rect for the branch name on the branch row. Clicking it
    /// opens the Checkout / Create Branch picker, mirroring VS Code's
    /// clickable status-bar branch indicator. Empty in the no-repo state.
    pub last_branch_area: Rect,
    /// Hit-test rect for the "⋯" actions pill in the header (left of the
    /// refresh pill). Opens the full Source Control actions menu. Empty in
    /// the no-repo state or when the header row is too narrow.
    pub header_more_btn: Rect,
}

impl SourceControlPanel {
    pub fn new() -> Self {
        Self {
            focused: false,
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            message: String::new(),
            message_cursor: 0,
            message_scroll: 0,
            message_line_scroll: 0,
            status: GitStatus::default(),
            seeded_message: None,
            entries: Vec::new(),
            repositories: Vec::new(),
            repo_row_areas: Vec::new(),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            last_input_area: Rect::default(),
            last_button_area: Rect::default(),
            last_commit_caret_area: Rect::default(),
            last_list_area: Rect::default(),
            last_scrollbar: Rect::default(),
            last_init_repo_button_area: Rect::default(),
            nested_repos: Vec::new(),
            nested_repo_button_areas: Vec::new(),
            last_hero_area: Rect::default(),
            inline_hero_image_active: false,
            scroll: 0,
            selected_change: None,
            multi_selection: BTreeSet::new(),
            list_focused: false,
            commit_feedback: None,
            commit_feedback_is_error: false,
            last_row_actions: None,
            hover_pointer: None,
            header_refresh_btn: Rect::default(),
            last_branch_area: Rect::default(),
            header_more_btn: Rect::default(),
        }
    }
}

impl Default for SourceControlPanel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
