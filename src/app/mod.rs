use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::collections::BTreeSet;
use std::io::{Stdout, stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;

mod agents_collab;
mod bottom_panel;
mod click;
mod code_scanning;
mod codeql;
mod commands;
mod completion;
mod cursor_blink;
mod debugger;
mod diff_merge;
mod editor_input;
mod editor_layout;
mod editor_tabs;
mod explorer;
mod frame;
mod fs_watch;
mod git_worker;
mod host_integration;
mod hover;
mod inline_images;
mod key_input;
mod language_servers;
mod memory_report;
mod menus;
mod mouse_input;
mod nav;
mod overlay;
mod perf_hud;
mod prompts;
mod remote_sessions;
mod review;
mod runners;
mod save_restore;
mod scm;
mod screencast;
mod search;
mod settings;
pub(crate) mod sidebar_dwell;
mod terminal_panes;
mod terminal_tools;
mod viewers;
mod welcome;
mod welcome_screen;
mod workbench;
use agents_collab::*;
use bottom_panel::*;
use click::{ClickTracker, ModifiedClickTracker};
use code_scanning::*;
use codeql::*;
#[cfg(test)]
use commands::*;
use completion::*;
use cursor_blink::CursorBlink;
use debugger::*;
use editor_input::*;
use editor_tabs::*;
use explorer::*;
use frame::*;
use fs_watch::FsWatch;
use git_worker::GitWorker;
pub(crate) use host_integration::reattach_reassert_seq;
pub(crate) use host_integration::reset_host_colors_seq;
pub(crate) use host_integration::reset_session_bg_seq;
pub(crate) use host_integration::takeover_mode_seq;
use host_integration::*;
use hover::{HOVER_DELAY, HoverDwell};
use inline_images::*;
use key_input::*;
use language_servers::*;
use menus::*;
use nav::{NavHistory, NavLoc};
use overlay::OverlayManager;
use perf_hud::PerfHud;
use prompts::*;
use remote_sessions::*;
use review::*;
use runners::*;
use save_restore::*;
use scm::*;
use screencast::Screencast;
use search::*;
use settings::*;
use sidebar_dwell::{AUTO_HIDE_DWELL, SidebarDwell};
use terminal_panes::*;
#[cfg(test)]
use terminal_tools::*;
use viewers::*;
use welcome::WelcomeState;
#[cfg(test)]
use welcome_screen::*;
pub(crate) use workbench::PanelAlignment;
pub(crate) use workbench::QuickInputPosition;
pub(crate) use workbench::SideBarPosition;
use workbench::*;

use crate::lsp::manager::CompletionTrigger;
use crate::lsp::manager::{diagnostics_by_source, diagnostics_source, diagnostics_store_key};
use crate::paths::croft_cache_dir;
use crate::theme::BRAND_ORANGE;
use crate::widgets::terminal::cwd_of_pid;

use crate::widgets::{
    dependencies::{DependenciesPanel, Dependency, Ecosystem},
    editor::{Editor, EditorTabs},
    file_tree::FileTree,
    open_editors::{OpenEditorItem, OpenEditorsPanel},
    outline::OutlinePanel,
    remote::RemotePanel,
    run_debug::RunDebugPanel,
    search::{SearchPanel, SearchRequest},
    source_control::SourceControlPanel,
    terminal::{MouseAction, MouseButtonKind, MouseMods, PtyTerminal},
    timeline::TimelinePanel,
};

/// Which sidebar view is active in the left side panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarView {
    Explorer,
    Search,
    SourceControl,
    Remote,
    RunDebug,
    Extensions,
    Testing,
    /// CodeQL (#578): databases, queries, query history, AST viewer.
    CodeQL,
}

/// Which Explorer sub-views are shown, toggled from the ⋯ menu. Wraps the
/// persisted [`crate::prefs::ExplorerViewsPrefs`] so the app can query/flip a
/// view by its [`ExplorerView`] without touching the serde struct's fields
/// directly.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ExplorerViewVisibility {
    flags: crate::prefs::ExplorerViewsPrefs,
}

/// One in-flight `textDocument/selectionRange` request (#254): the
/// Expand Selection gesture waiting on its chains. `positions` snapshots
/// the exact cursors the request was fired for, so a reply (or a reuse
/// attempt) after ANY caret movement is recognised as stale even though
/// `edit_seq` did not move.
struct PendingSelectionRanges {
    id: u64,
    path: PathBuf,
    seq: u64,
    presses: usize,
    positions: Vec<(u32, u32)>,
}

/// Where a row of Bookmarks: View Bookmarks points: the tab holding the
/// mark (its editor group by depth-first index, then its index in that
/// group), the file it shows and the 1-based line.
#[derive(Clone, Debug)]
struct BookmarkTarget {
    group: usize,
    tab: usize,
    path: PathBuf,
    line: usize,
}

/// Pre-Zen visibility snapshot, restored verbatim when Zen Mode is toggled
/// off so the workspace returns to exactly the chrome it had before.
#[derive(Clone, Copy, Debug)]
struct PreZenLayout {
    activity_bar: bool,
    status_bar: bool,
    side_bar: bool,
    secondary_side_bar: bool,
    panel: bool,
}

/// Hit-test rects for the three layout-control icons (Toggle Primary Side
/// Bar, Toggle Panel, Customize Layout) rendered at the top-right of the
/// editor tab strip and the welcome screen — croft's analog of VS Code's
/// title-bar layout controls.
#[derive(Clone, Copy, Default)]
struct LayoutIconAreas {
    toggle_side_bar: Rect,
    toggle_panel: Rect,
    customize: Rect,
}

/// Result of [`compute_chrome_layout`]: the rectangles for each workspace
/// region within the horizontal `main` band. Hidden regions are `None` and
/// consume no width.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct ChromeLayout {
    activity: Option<Rect>,
    primary_side: Option<Rect>,
    secondary_side: Option<Rect>,
    /// The editor + terminal column (always present).
    right: Rect,
    /// Column of the seam between the editor and the primary side bar, where a
    /// resize drag starts. `None` when the primary side bar is hidden.
    sidebar_splitter_x: Option<u16>,
}

/// Centered popup geometry for frames of any size. Min bounds cap at the
/// frame because Ord::clamp panics on min > max, and a frame with no
/// cells on an axis gets no popup at all.
fn fallback_popup_rect(full: Rect, max_w_cap: u16) -> Rect {
    if full.width == 0 || full.height == 0 {
        return Rect {
            width: 0,
            height: 0,
            ..full
        };
    }
    let max_w = max_w_cap.min(full.width);
    let max_h = full.height;
    let width = (full.width.saturating_mul(7) / 10).clamp(40.min(max_w), max_w);
    let height = (full.height.saturating_mul(6) / 10).clamp(10.min(max_h), max_h);
    Rect {
        x: full.x + (full.width.saturating_sub(width)) / 2,
        y: full.y + (full.height.saturating_sub(height)) / 4,
        width,
        height,
    }
}

#[derive(Default, Clone, Copy)]
struct SidebarAreas {
    /// Block occupied by the Explorer activity-bar icon, in absolute coords.
    /// Multi-row when image rendering is active; the hit-test still uses the
    /// whole block.
    explorer_icon: Rect,
    /// Block occupied by the Search activity-bar icon, in absolute coords.
    search_icon: Rect,
    /// Block occupied by the Source Control activity-bar icon, in absolute coords.
    source_control_icon: Rect,
    /// Block occupied by the Remote Explorer activity-bar icon, in absolute coords.
    remote_icon: Rect,
    /// Block occupied by the Run and Debug activity-bar icon, in absolute coords.
    run_debug_icon: Rect,
    /// Block occupied by the Extensions activity-bar icon, in absolute coords.
    extensions_icon: Rect,
    /// Block occupied by the Testing (beaker) activity-bar icon, in absolute coords.
    testing_icon: Rect,
    /// Block occupied by the CodeQL (QL) activity-bar icon; empty while the
    /// built-in CodeQL extension is disabled.
    codeql_icon: Rect,
    /// Block occupied by the sponsor heart directly above the settings gear;
    /// empty when the bar is too short for it. Clicking it opens croft's
    /// GitHub Sponsors page.
    sponsor_icon: Rect,
    /// Block occupied by the settings gear, bottom-anchored on the activity
    /// bar (VS Code's "Manage" button). Asymmetric to the view icons up top;
    /// clicking it opens the settings menu (Color Theme picker).
    settings_icon: Rect,
}

/// Pre-encoded iTerm2 OSC-1337 inline-image escape sequences for each icon
/// state. Encoded once in `App::init_graphics` (no PNG re-encoding per
/// frame) and rewritten under the activity-bar block after every ratatui
/// frame draw, since ratatui's bg-clear overdraws the image.
#[cfg_attr(test, derive(Default))]
pub struct ActivityBarImages {
    explorer_active: String,
    explorer_inactive: String,
    explorer_hovered: String,
    search_active: String,
    search_inactive: String,
    search_hovered: String,
    source_control_active: String,
    source_control_inactive: String,
    source_control_hovered: String,
    remote_active: String,
    remote_inactive: String,
    remote_hovered: String,
    run_debug_active: String,
    run_debug_inactive: String,
    run_debug_hovered: String,
    extensions_active: String,
    extensions_inactive: String,
    extensions_hovered: String,
    testing_active: String,
    testing_inactive: String,
    testing_hovered: String,
    codeql_active: String,
    codeql_inactive: String,
    codeql_hovered: String,
    /// The sponsor heart. It opens a web page rather than a view or a menu,
    /// so it is never selected and has no active variant.
    sponsor_inactive: String,
    sponsor_hovered: String,
    /// The settings gear. `active` is shown while its menu is open so the
    /// button reads as pressed, mirroring the view icons' selected state;
    /// `hovered` brightens it while the pointer rests on it.
    settings_active: String,
    settings_inactive: String,
    settings_hovered: String,
    /// Customize Layout toolbar icons (top-right of the editor / welcome),
    /// baked OPAQUE at the 4×2-cell toolbar box so they paint in front of the
    /// container's top line. The side-bar / panel icons carry an on/off pair
    /// (filled vs. hollow) matching their visibility; every icon also has a
    /// brighter `_hover` variant emitted while the pointer rests on it, exactly
    /// like the activity-bar icons.
    layout_sidebar_on: String,
    layout_sidebar_on_hover: String,
    layout_sidebar_off: String,
    layout_sidebar_off_hover: String,
    layout_panel_on: String,
    layout_panel_on_hover: String,
    layout_panel_off: String,
    layout_panel_off_hover: String,
    layout_customize: String,
    layout_customize_hover: String,
}

/// Everything [`App::bake_chrome_images`] needs, resolved on the UI thread so
/// the bake can run on one of its own.
#[derive(Clone, Copy)]
struct ChromeBakeInput {
    protocol: crate::iterm2_inline::InlineImageProtocol,
    cell: (u32, u32),
    is_tmux: bool,
    icon_bg: ::image::Rgba<u8>,
    icon_ink: crate::iterm2_inline::IconInk,
    sponsor_ink: crate::iterm2_inline::IconInk,
}

/// The pictures [`App::init_graphics`] bakes off the UI thread; `None` for
/// any that failed to bake.
struct ChromeImages {
    activity: Option<ActivityBarImages>,
    run_debug: Option<String>,
    ssh: Option<String>,
}

/// What the welcome wordmark is baked for. A picture baked for any other (a
/// relayout, a theme switch, a font zoom since) is stale.
#[derive(Clone, Copy, PartialEq)]
struct WelcomeBakeKey {
    layout: WelcomeLayout,
    cell: (u32, u32),
    bg: ::image::Rgba<u8>,
    light: bool,
    protocol: crate::iterm2_inline::InlineImageProtocol,
}

/// Single source of truth for the application's user-facing name.
pub const APP_NAME: &str = "croft";

/// The status bar's three text tones, so hierarchy comes from brightness
/// rather than from dividers or a colour per item: `text` for the values you
/// read (branch, line and column, language), `muted` for settings and
/// commentary, and `dim` for labels and for counts that are zero.
#[derive(Clone, Copy)]
struct StatusTones {
    text: Color,
    muted: Color,
    dim: Color,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Tree,
    Editor,
    Terminal,
}

/// Which tab the bottom panel group shows. The panel is the "terminal group"
/// VS Code-style: a tab strip over a shared region. `Terminal` shows the live
/// shell(s); `Problems` shows aggregated workspace diagnostics; `Output` is the
/// read-only per-channel log viewer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BottomPanelTab {
    Terminal,
    Problems,
    Output,
    Ports,
    Captures,
}

impl BottomPanelTab {
    /// Every tab, in the strip's left-to-right order (VS Code's panel-group
    /// order). The strip paints from this list, so a tab cannot be
    /// clickable without being in it.
    const ALL: [BottomPanelTab; 5] = [
        BottomPanelTab::Problems,
        BottomPanelTab::Output,
        BottomPanelTab::Terminal,
        BottomPanelTab::Ports,
        BottomPanelTab::Captures,
    ];

    /// The tab's strip label in English, the catalog key it is drawn
    /// through (#1583).
    fn label(self) -> &'static str {
        match self {
            BottomPanelTab::Problems => "PROBLEMS",
            BottomPanelTab::Output => "OUTPUT",
            BottomPanelTab::Terminal => "TERMINAL",
            BottomPanelTab::Ports => "PORTS",
            BottomPanelTab::Captures => "CAPTURES",
        }
    }

    /// The palette command that shows this tab (#852). No catch-all: a new
    /// tab fails to compile until it names one, so no tab ships reachable
    /// only by a click on the strip.
    fn show_command(self) -> crate::widgets::command_palette::Command {
        use crate::widgets::command_palette::Command;
        match self {
            BottomPanelTab::Terminal => Command::FocusTerminal,
            BottomPanelTab::Problems => Command::ShowProblems,
            BottomPanelTab::Output => Command::ShowOutput,
            BottomPanelTab::Ports => Command::ShowPorts,
            BottomPanelTab::Captures => Command::ShowCaptures,
        }
    }
}

/// A transient, click-only notification that a browsable port just appeared in
/// the terminal. It never captures keystrokes — the terminal owns those — so
/// the user clicks an action or lets it auto-dismiss. The forward actions only
/// apply on a remote session (a local port is already reachable).
struct PortToast {
    port: u16,
    process: Option<String>,
    origin_remote: bool,
    shown_at: std::time::Instant,
    /// Action button hit rects, recomputed each render.
    buttons: Vec<(Rect, PortToastAction)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CreateKind {
    File,
    Folder,
}

/// The bottom-left "a newer croft is available" popup (#333), a card in
/// the manner of Ghostty's (`widgets/update_popup.rs`) over its status-bar
/// pill. Click-only like the port toast: it never takes a keystroke from
/// the editor or a terminal. It is down while the staged build runs (the
/// status bar shows that, #1852) and comes back with `ready` set, offering
/// Relaunch instead of Update, once the build has landed.
struct UpdateToast {
    release: crate::update_check::Release,
    ready: bool,
    /// Why the last Update did not stage the version (#1844): said in the
    /// popup, which then offers Retry.
    failed: Option<&'static str>,
    /// False once Later folded the card into its status-bar pill, which
    /// opens it again.
    open: bool,
    /// Action hit rects, recomputed each render.
    buttons: Vec<(Rect, UpdateToastAction)>,
    /// What the card painted in each of `buttons`, to tell whether a later
    /// overlay covered it before the frame went out.
    painted: Vec<Vec<ratatui::buffer::Cell>>,
}

/// A fenced block about to run (#353), parked behind the confirm popup.
#[derive(Clone, Debug)]
struct PendingRunBlock {
    pane_name: String,
    cwd: PathBuf,
    command: String,
    /// What the block runs, shown whole in the popup: the block as written,
    /// or a transcript's commands without their prompts (#1473). The fence
    /// was refused if it carried control characters, so it renders as typed.
    code: String,
    /// Turns the popup red: the block looks destructive or said `{confirm}`.
    destructive: bool,
    /// The document this block came from, for `{persist}`'s write-back.
    doc: Option<PathBuf>,
    /// The fence opener's SOURCE line, which keys the captured output.
    block_line: usize,
    /// `{persist}`: write the capture back into the document.
    persist: bool,
    /// `{timeout=N}`: stop CAPTURING after N seconds. Never stops the
    /// process; the block keeps running in its pane.
    capture_timeout: Option<std::time::Duration>,
}

/// A block whose output croft is waiting for (#354).
///
/// Armed when the block is typed into its pane and settled by
/// [`App::settle_block_captures`] when the shell reports the command
/// finished.
///
/// IDENTITY, NOT POSITION (#440, landed as #441). `after` is the id of the
/// last command the pane had already finished when the block was typed, so
/// this block's command is the first decoration whose id is greater. Ids
/// are monotonic and the collector never renumbers them, so an evicted
/// command reads as GONE rather than as its neighbour - which is what makes
/// the `{persist}` write safe rather than merely guarded.
///
/// The previous shape indexed the decoration list, which is a sliding
/// window: an eviction shifted every index down and index `seen` came to
/// name a stranger. That was narrowed with a high-water sample and could
/// not be closed by one, because any length-derived signal samples a value
/// that can move twice between observations. An id cannot move at all.
struct PendingCapture {
    pane_name: String,
    doc: Option<PathBuf>,
    block_line: usize,
    /// Ids at or below this belong to commands that finished BEFORE the
    /// block was typed.
    ///
    /// `None` is a sentinel, not a special case: ids start at 1, so `None`
    /// and `Some(0)` select identically and a fresh pane needs no separate
    /// handling.
    ///
    /// THE RESIDUAL IS IN THE SELECTION, NOT HERE. Settling takes the
    /// EARLIEST qualifying id, which is this block's command only if
    /// nothing else finished in the gap between arming and the keystrokes
    /// reaching the shell. A command completing in that gap is taken
    /// instead, on a pane with a full history exactly as on a fresh one -
    /// `after` does not enter into it. The window is much smaller than the
    /// eviction one it replaced, since a foreign command must now COMPLETE
    /// in the gap rather than merely exist, and the outcome self-corrects
    /// on the next run. Closing it properly means arming after the
    /// keystrokes are known to have landed, which is a different change.
    after: Option<u64>,
    started: std::time::Instant,
    timeout: Option<std::time::Duration>,
    persist: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum MenuAction {
    Create(CreateKind),
    /// Move every path in `paths` to the OS trash (recoverable). One entry
    /// for a single right-click, more when a multi-selection is active.
    Delete {
        paths: Vec<PathBuf>,
    },
    /// Open the rename prompt pre-filled with the entry's current name.
    Rename(PathBuf),
    /// Cut the listed paths to the explorer clipboard for later Paste.
    Cut(Vec<PathBuf>),
    /// Copy the listed paths to the explorer clipboard for later Paste.
    Copy(Vec<PathBuf>),
    /// Paste the explorer clipboard's payload into `target_dir`.
    Paste(PathBuf),
    /// Stash this path as the "compare anchor" so the next file the user
    /// right-clicks can be diffed against it.
    SelectForCompare(PathBuf),
    /// Open a side-by-side diff between the previously-selected anchor and
    /// the file the user just right-clicked.
    CompareWithSelected {
        anchor: PathBuf,
        other: PathBuf,
    },
    /// Re-root the workspace at the right-clicked folder. Folder-only -
    /// files can't be roots. Used to dive into a nested git repo without
    /// relaunching croft.
    MakeRoot(PathBuf),
    /// Add another folder to the workspace (multi-root, #147): opens the
    /// fuzzy directory picker in add-folder mode.
    AddWorkspaceFolder,
    /// Drop a SECONDARY workspace root from the folder set (#147).
    RemoveWorkspaceFolder(PathBuf),
    /// Copy the absolute path of the right-clicked editor tab to the system
    /// clipboard (VS Code's "Copy Path"). Available wherever the tab is backed
    /// by a real on-disk path, local or remote.
    CopyTabPath(PathBuf),
    /// Copy the right-clicked editor tab's path relative to the workspace root
    /// (VS Code's "Copy Relative Path"). Falls back to the absolute path when
    /// the file lives outside the workspace root. Local or remote.
    CopyTabRelativePath(PathBuf),
    /// Reveal the right-clicked editor tab's file in the Explorer tree:
    /// expand its parent folders, select its row, and focus the Explorer.
    /// Mirrors VS Code's "Reveal in Explorer View". Works local and remote.
    RevealInExplorer(PathBuf),
    /// Reveal the path in macOS Finder via `open -R`. Local-macOS only:
    /// on a remote SSH session croft runs on the headless host where no
    /// Finder exists, so the menu omits this entry entirely rather than
    /// offering a no-op.
    RevealInFinder(PathBuf),
    /// Open the file in an extension-contributed viewer (#465): the viewer's
    /// id and the file.
    OpenInViewer(String, PathBuf),
    /// Close the editor tab at `idx`. Mirrors clicking the tab's `×`
    /// glyph; surfaces in the tab-strip right-click menu so the four
    /// close actions (Close / Others / Right / All) live in one place.
    CloseTab(usize),
    /// Close every editor tab whose index ≠ `keep_idx`.
    CloseOtherTabs(usize),
    /// Close every editor tab whose index > `from_idx`.
    CloseTabsToRight(usize),
    /// Close every editor tab and reset the pane to a blank state.
    CloseAllTabs,
    /// Close every saved (non-dirty) editor tab, keeping any with unsaved
    /// changes. Mirrors VS Code's "Close Saved".
    CloseSavedTabs,
    /// Promote the preview tab at `idx` to a permanent tab so a later
    /// single-click open won't replace it (VS Code's "Keep Open"). Only
    /// surfaced for a tab that is currently the replaceable preview slot.
    KeepTabOpen(usize),
    /// Toggle the pinned state of the tab at `idx` (VS Code's "Pin" / "Unpin").
    /// Pinned tabs stay leftmost and survive Close Others / Close to the Right.
    ToggleTabPin(usize),
    /// Split the editor side by side, the new (focused) duplicate to the RIGHT
    /// (VS Code "Split Right"). The default split.
    SplitEditor,
    /// Split side by side, the new focused duplicate to the LEFT ("Split Left").
    SplitEditorLeft,
    /// Split stacked, the new focused duplicate ABOVE ("Split Up").
    SplitEditorUp,
    /// Split stacked, the new focused duplicate BELOW ("Split Down").
    SplitEditorDown,
    /// Move the active editor into the group above ("Move Above").
    MoveEditorUp,
    /// Move the active editor into the group below ("Move Below").
    MoveEditorDown,
    /// Move the active editor into the group to the left ("Move Left").
    MoveEditorLeft,
    /// Move the active editor into the group to the right ("Move Right").
    MoveEditorRight,
    /// Open a second view of the active file beside it ("Split in Group").
    SplitInGroup,
    /// Move keyboard focus to the left editor group (while split).
    FocusGroupLeft,
    /// Move keyboard focus to the right editor group (while split).
    FocusGroupRight,
    /// VS Code "Move into New Window": detach the tab at `idx` into a
    /// full-screen, chrome-free editor window (Esc returns it). The tab leaves
    /// its group, which collapses if it was the group's last tab.
    MoveIntoNewWindow(usize),
    /// VS Code "Copy into New Window" (⌘K O): open a duplicate view of the tab
    /// at `idx` in a full-screen editor window, leaving the original in place.
    CopyIntoNewWindow(usize),
    /// Editor body: LSP "Rename Symbol" of the identifier at buffer `(row,
    /// col)`. Captured at right-click so the later dispatch targets the same
    /// symbol the user clicked.
    RenameSymbolAt {
        row: usize,
        col: usize,
    },
    /// Editor body: "Copy Permalink" / "Open on Remote" (#1610) for the
    /// selected lines, else the caret's.
    CopyPermalink,
    OpenOnRemote,
    /// Editor body: "Change All Occurrences" of the word at buffer `(row,
    /// col)` (multi-cursor select within the current file).
    ChangeAllOccurrencesAt {
        row: usize,
        col: usize,
    },
    /// Editor body: LSP "Go to Definition" of the identifier at buffer
    /// `(row, col)`.
    GoToDefinitionAt {
        row: usize,
        col: usize,
    },
    /// Editor body: LSP "Go to Declaration" of the identifier at buffer
    /// `(row, col)`. Distinct from definition for languages that separate the
    /// two (C/C++ header prototype vs `.c` body, TypeScript `.d.ts` vs source).
    GoToDeclarationAt {
        row: usize,
        col: usize,
    },
    /// Editor body: LSP "Go to Type Definition" of the identifier at buffer
    /// `(row, col)`. Jumps to where the *type* of the expression under the
    /// cursor is defined (e.g. the `User` struct for `let u = get_user()`),
    /// as opposed to where the symbol itself is defined.
    GoToTypeDefinitionAt {
        row: usize,
        col: usize,
    },
    /// Editor body: LSP "Go to Implementations" of the abstraction at buffer
    /// `(row, col)`. Walks down from a trait / interface / abstract method to
    /// the concrete implementors; the server often returns many, in which case
    /// the results are offered as a picker built from `GoToLocation` items.
    GoToImplementationAt {
        row: usize,
        col: usize,
    },
    /// Editor body: LSP "Go to References" of the symbol at buffer `(row, col)`.
    /// Finds every use of the symbol project-wide (declaration included, like VS
    /// Code); the server almost always returns many, which are offered as a
    /// picker built from `GoToLocation` items.
    GoToReferencesAt {
        row: usize,
        col: usize,
    },
    /// Return to the location before the last navigation jump (#31).
    NavigateBack,
    NavigateForward,
    /// Editor body: LSP "Quick Fix" of the position at buffer `(row, col)`.
    QuickFixAt {
        row: usize,
        col: usize,
    },
    /// A breadcrumb picker's file (#1608): open it as Quick Open would.
    OpenCrumbFile(PathBuf),
    /// A breadcrumb picker's folder (#1608): list it in the picker of
    /// `crumb`, where the listing started.
    BrowseCrumbFolder {
        dir: PathBuf,
        crumb: usize,
    },
    /// Jump straight to an already-resolved location (LSP line/character). Used
    /// for the entries of the multi-result "Go to Implementations" / "Go to
    /// References" pickers, where each row is one location; selecting it records
    /// nav history and opens the file at that position.
    GoToLocation {
        path: PathBuf,
        line: u32,
        col: u32,
    },
    /// Editor gutter: toggle a bookmark on 1-based `line`. The gutter menu has
    /// no cursor to act on, so the line rides the action; the label reads
    /// "Toggle Bookmark" either way, as the Bookmarks extension's does.
    ToggleBookmarkAt {
        line: usize,
    },
    /// Editor gutter: jump to the next bookmark in the open file, wrapping at
    /// the end. Not line-scoped, so it rides the menu as a command.
    NextBookmark,
    /// Editor gutter: mirror of [`MenuAction::NextBookmark`] walking upwards.
    PreviousBookmark,
    /// Editor gutter: toggle a breakpoint on 1-based `line`. Built from a
    /// glyph-margin right-click; the label reads "Add Breakpoint" or "Remove
    /// Breakpoint" depending on whether one already sits on that line, but both
    /// resolve to the same toggle. Mirrors VS Code's glyph-margin menu.
    ToggleBreakpointAt {
        line: usize,
    },
    /// Editor gutter: open the breakpoint-condition editor for 1-based `line`,
    /// creating the breakpoint if absent. VS Code's "Add Conditional
    /// Breakpoint" / "Edit Condition".
    EditBreakpointConditionAt {
        line: usize,
    },
    /// Editor gutter: open the logpoint editor for 1-based `line`, creating
    /// the breakpoint if absent. VS Code's "Add Logpoint" / "Edit Logpoint".
    EditLogpointAt {
        line: usize,
    },
    /// Editor gutter: ask the resident navigator about 0-based `line`
    /// (opens the ask box scoped to that line).
    AskNavigatorAt {
        line: usize,
    },
    /// Editor body: ask the navigator about the selected 0-based inclusive
    /// line span (the selection's text rides the turn).
    AskNavigatorSelection {
        start: usize,
        end: usize,
    },
    /// CAPTURES row: ask the navigator about the selected captured line,
    /// with the pane rows around it (#372).
    AskNavigatorAboutCapture,
    /// VARIABLES or WATCH row: copy its value (#1546). With `expression`
    /// and an adapter that offers it, the full value comes from an
    /// `evaluate` in the `clipboard` context, which is not cut as the
    /// row's `value` may be; otherwise `value` itself is copied.
    CopyDebugValue {
        expression: Option<String>,
        value: String,
    },
    /// VARIABLES row: copy its `evaluateName`; WATCH row: its expression.
    CopyDebugExpression(String),
    /// VARIABLES row: watch its `evaluateName`.
    AddDebugWatch(String),
    /// WATCH row: stop watching expression `index`.
    RemoveDebugWatch(usize),
    /// CALL STACK row: copy the whole stack, a frame a line.
    CopyCallStack,
    /// PROBLEMS row: have the navigator fix this diagnostic as a streamed,
    /// cancellable edit (#374).
    FixProblemWithNavigator {
        path: PathBuf,
        item: crate::widgets::problems::ProblemItem,
    },
    /// OUTLINE row: open lines `first..=last` of the Outline's file as a
    /// symbol tab named `name` (#369).
    OpenSymbolTab {
        path: PathBuf,
        name: String,
        first: usize,
        last: usize,
    },
    /// OUTLINE row: jump to the symbol, as a left click does.
    GoToOutlineSymbol {
        path: PathBuf,
        line: u32,
        col: u32,
    },
    /// Editor body: one level of the call hierarchy for the symbol at buffer
    /// `(row, col)` — callers when `incoming`, callees otherwise.
    ShowCallsAt {
        row: usize,
        col: usize,
        incoming: bool,
    },
    /// Settings gear → "Color Theme": replace the gear menu with the theme
    /// picker (the list of themes with a check on the active one).
    OpenThemePicker,
    /// Theme picker entry: switch the IDE color theme to `theme`, persist it,
    /// re-emit the session background, and re-bake the icon images.
    SetTheme(crate::theme::Theme),
    /// Explorer "Views and More Actions" (⋯) entry: flip the visibility of one
    /// stacked sub-view (Open Editors / Folders / Outline / Timeline / Rust
    /// Dependencies) and persist the choice.
    ToggleExplorerView(ExplorerView),
    /// Customize Layout: re-open the Customize Layout popup (so a toggle the
    /// user just flipped stays visible for the next one, like VS Code).
    OpenCustomizeLayout,
    /// Customize Layout: show/hide the left activity bar.
    ToggleActivityBar,
    /// Customize Layout: show/hide the primary side bar (same as ⌘B).
    ToggleSideBar,
    ToggleAutoHideSideBar,
    /// Customize Layout: show/hide the secondary side bar (the Outline).
    ToggleSecondarySideBar,
    /// Customize Layout: show/hide the bottom panel (the terminal, same as ⌃J).
    TogglePanel,
    /// Customize Layout: show/hide the bottom status bar.
    ToggleStatusBar,
    /// Customize Layout: dock the primary side bar to `position`.
    SetSideBarPosition(SideBarPosition),
    /// Customize Layout: set the bottom panel's horizontal alignment.
    SetPanelAlignment(PanelAlignment),
    /// Customize Layout: anchor the quick input at `position`.
    SetQuickInputPosition(QuickInputPosition),
    /// Customize Layout: toggle Zen Mode (hide all chrome / restore).
    ToggleZenMode,
    /// Customize Layout: centre the editor (#1585).
    ToggleCenteredLayout,
    /// Run the active editor's code lens at this index (#608).
    RunCodeLens(usize),
    /// Terminal pane right-click: rename the pane at `idx`.
    RenameTerminal(usize),
    /// Terminal pane right-click: clear the pane at `idx`'s screen + scrollback.
    ClearTerminal(usize),
    /// Terminal pane right-click: maximize the pane at `idx` across the
    /// panel (or restore the even split when already maximized).
    ToggleMaximizeTerminal(usize),
    /// Terminal pane right-click: collapse the pane at `idx` to a strip, or
    /// expand it again (#313).
    ToggleCollapseTerminal(usize),
    /// Terminal pane right-click: copy the pane's selection to the clipboard.
    /// Offered only while the pane HAS one, so the entry never copies nothing.
    TerminalCopySelection,
    /// Terminal pane right-click: paste the host clipboard into the pane.
    TerminalPaste,
    /// Editor right-click (#1117): the editor's own Cut, Copy and Paste, as
    /// its keys do them. Cut and Copy are offered only with a selection.
    EditorCut,
    EditorCopy,
    EditorPaste,
    /// Editor right-click (#1117): open the Command Palette.
    ShowCommandPalette,
    /// Terminal pane right-click: enter quick-select hint mode on the pane.
    TerminalQuickSelect,
    /// Terminal pane right-click: enter copy mode (keyboard selection with
    /// vi motions) on the pane.
    TerminalCopyMode,
    /// Terminal pane right-click: open the durable command-history search.
    TerminalCommandHistory,
    /// Terminal pane right-click: dump the pane's scrollback into a scratch
    /// editor tab.
    OpenScrollbackInEditor,
    /// Terminal pane right-click: toggle broadcast input (keystrokes go to
    /// every pane at once; enabling routes through the confirm popup).
    ToggleBroadcastInput,
    /// Terminal pane right-click while broadcasting: toggle whether the
    /// active pane receives the mirrored input.
    ToggleBroadcastExclusion,
    /// Terminal pane right-click: restore the most recently closed pane
    /// while it is still inside the undo-close grace window.
    UndoCloseTerminal,
    /// Terminal pane right-click: open another terminal next to the active
    /// one, as `Cmd+T` does (#1598).
    SplitTerminal,
    /// Terminal pane right-click: close the pane at `idx`, as `Cmd+W` does
    /// for the active one (#1598).
    KillTerminal(usize),
    /// Command decoration menu: copy the command's output span to the
    /// clipboard (pane `idx`, the clicked decoration embedded).
    CopyCommandOutput(usize, crate::widgets::terminal::CommandDecoration),
    /// Command decoration menu: select the command's output span in the pane.
    SelectCommandOutput(usize, crate::widgets::terminal::CommandDecoration),
    /// Command decoration menu: type the command at the shell again.
    RerunCommand(usize, crate::widgets::terminal::CommandDecoration),
    /// Terminal profile dropdown (`⌄`): open a new pane running this shell path.
    NewTerminalWithProfile(String),
    /// Minimap right-click menu: show/hide the editor minimap.
    ToggleMinimap,
    /// Minimap right-click menu: move the strip to the given editor edge.
    SetMinimapSide(MinimapSide),
    /// Status-bar language picker: set the active buffer's language mode.
    SetLanguage(Option<crate::highlight::LangKind>),
    /// Status-bar EOL picker: switch the buffer's line-ending style.
    SetLineEnding(crate::widgets::editor::LineEnding),
    /// Status-bar encoding picker: reopen the file decoded with this encoding.
    SetEncoding(&'static encoding_rs::Encoding),
    /// Status-bar indentation picker: pin spaces-vs-tabs and width for newly
    /// typed indentation.
    SetIndentStyle(crate::widgets::editor::IndentStyle),
    /// Status-bar indentation picker: rewrite existing leading indentation.
    /// `true` converts tabs to spaces, `false` converts spaces to tabs.
    ConvertIndentation(bool),
    /// A COMMITS graph row's right-click menu (#1627): act on the clicked
    /// commit, by full hash, with its short hash for the status line.
    CommitAction {
        action: CommitAction,
        hash: String,
        short: String,
    },
}

/// A compound's members and the name to report them under (#310).
struct CompoundLaunch {
    members: Vec<crate::dap::configs::DebugConfig>,
    compound: String,
    /// The compound's `stopAll`, carried to the moment the set is replaced.
    stop_all: bool,
}

/// One viewed mark waiting to reach GitHub (#365).
struct PrViewedWrite {
    /// The review tab's `owner/repo#n`, so a later tab's failure is not
    /// reported against this one.
    key: String,
    pr_id: String,
    path: String,
    viewed: bool,
}

/// A code scanning lookup in flight (#577).
struct CodeScanList {
    branch: String,
    /// Load what it finds (`on`, or the palette command), rather than only
    /// saying it is there (`prompt`).
    load: bool,
    rx: std::sync::mpsc::Receiver<Result<Vec<crate::sarif::github::Analysis>, String>>,
}

/// A pull request head checked out into a sibling worktree (#365).
struct PrCheckout {
    number: u64,
    path: PathBuf,
    /// The commit checked out: a HEAD anywhere else means new commits.
    oid: String,
    /// Whether review mode created the worktree; one that was already there
    /// is never removed by leaving.
    created: bool,
}

/// `${input:…}` questions waiting on the user (#1424): asked one at a
/// time, then `then` runs with the answers.
struct InputRun {
    ask: Vec<crate::dap::configs::LaunchInput>,
    answers: std::collections::BTreeMap<String, String>,
    then: InputThen,
}

/// A launch.json config launch parked behind its `preLaunchTask` (#250): the
/// task runs in a terminal pane, and the FinishedCommand sweep decides.
struct PendingDebugLaunch {
    /// Uid of the pane the task command was written to.
    pane: u64,
    /// The task's command line, matched against the pane's next finished
    /// command (OSC 133 reports what actually ran).
    command: String,
    /// What to do once the task exits 0.
    step: PendingLaunchStep,
    /// When the task was written, for the no-command-mark fallback below.
    started: std::time::Instant,
    /// A background task (#1541): settled by the end of its first build
    /// cycle, which its watch matcher reports, since it never exits.
    watch: bool,
}

#[derive(Clone, Debug)]
struct ExplorerClipboard {
    mode: ExplorerClipMode,
    paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct PendingFileMove {
    request_id: u64,
    op: FileMove,
    renames: Vec<crate::lsp::manager::FileRenameOp>,
    deadline: std::time::Instant,
    /// The open buffers' `edit_seq`s when the request went out (#1151).
    baseline: Vec<(PathBuf, u64)>,
}

/// Numbers each App's scratch keybindings file under test (#612), so tests
/// running in parallel never share one.
static TEST_KEYBINDINGS_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Accumulated type-to-jump state for the Explorer. `prefix` lowercase
/// only; `last` is the instant of the most recent keystroke.
#[derive(Clone, Debug)]
struct TreeTypeahead {
    prefix: String,
    last: std::time::Instant,
}

/// State for an in-progress drag inside the explorer pane. The drag is only
/// "armed" once the pointer has actually moved off the original cell, so a
/// stationary mouse-down → mouse-up still behaves like a click.
#[derive(Clone, Debug)]
struct ExplorerDrag {
    /// Paths the drag is moving (or copying, when Alt is held).
    paths: Vec<PathBuf>,
    /// Tree row of the drop target under the pointer. `None` means the
    /// pointer is over empty space or outside the tree.
    target_idx: Option<usize>,
    /// True once we've seen a Drag event with a different cell from the
    /// initial Down. Until then we treat the gesture as a still-pending click.
    armed: bool,
    /// (instant, x, y) of the initiating mouse-down.
    started_at: (std::time::Instant, u16, u16),
    /// Index of the row the user pressed on. Used as the toggle target if
    /// the gesture turns out to be a stationary Alt-click rather than a drag.
    start_idx: usize,
    /// True when the initiating mouse-down was an Alt/Ctrl click. A
    /// non-armed release toggles the start row's mark; an armed release
    /// performs a copy-drop instead of the default move-drop.
    toggle_on_release: bool,
}

struct ContextMenu {
    /// Top-left of the menu in absolute terminal coordinates.
    origin: (u16, u16),
    /// Items, in display order.
    items: Vec<MenuEntry>,
    /// Highlighted top-level row.
    selected: usize,
    /// Index into `items` of the currently open submenu (a `Submenu` entry), or
    /// `None` when no submenu is open.
    open_submenu: Option<usize>,
    /// Highlighted row WITHIN the open submenu (only meaningful when
    /// `open_submenu` is `Some`).
    submenu_selected: usize,
    /// Where any New File / New Folder should be created.
    target_dir: PathBuf,
    /// The breadcrumb whose picker this is (#1608): Left / Right step to
    /// the neighbouring crumb's picker.
    crumb: Option<usize>,
    /// The first top-level row on screen: a menu taller than the terminal
    /// (a big folder's breadcrumb picker) scrolls to keep the highlight in
    /// view. Kept by `scroll_for`.
    scroll: std::cell::Cell<usize>,
    /// The first child row on screen inside the open submenu, driven by the
    /// wheel (`wheel_active`) and kept in range by
    /// render_context_menu.
    submenu_scroll: std::cell::Cell<usize>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum PromptKind {
    Create(CreateKind),
    /// Rename the entry at `path`. The prompt's `target_dir` holds the
    /// entry's parent; `buffer` is pre-filled with the current basename.
    Rename(PathBuf),
    /// File: Save As… (#1203): a path for the active tab's buffer, relative
    /// to `target_dir` (the workspace root) unless absolute. `buffer` starts
    /// as the tab's own path.
    SaveAs,
    /// LSP "Rename Symbol" (F2 in the editor): rename the identifier at
    /// `(row, col)` in `path`. `buffer` is pre-filled with the symbol; on
    /// commit the new name drives a `textDocument/rename` request.
    RenameSymbol {
        path: PathBuf,
        row: usize,
        col: usize,
    },
    /// Debugger "Add Conditional Breakpoint" / "Edit Condition": edit the
    /// boolean condition for the breakpoint on 1-based `line` of `path`. The
    /// buffer is pre-filled with any existing condition; on commit the
    /// breakpoint is created (if absent) and the condition attached/cleared,
    /// then pushed to a live session. A popup, never the status line.
    BreakpointCondition {
        path: PathBuf,
        line: usize,
    },
    /// Debugger "Add Logpoint" / "Edit Logpoint": edit the log message for
    /// the breakpoint on 1-based `line` of `path` (`{expr}` holes are
    /// interpolated by the adapter, which prints instead of pausing). Same
    /// commit shape as [`PromptKind::BreakpointCondition`].
    Logpoint {
        path: PathBuf,
        line: usize,
    },
    /// Debugger "Add Hit Count Breakpoint" (#611): the DAP `hitCondition`
    /// for the breakpoint on 1-based `line` of `path`. Same commit shape as
    /// [`PromptKind::BreakpointCondition`].
    HitCondition {
        path: PathBuf,
        line: usize,
    },
    /// Debugger "Add Function Breakpoint" (#611): a function name to pause
    /// on entry to.
    FunctionBreakpoint,
    /// Debugger "Break When Value Changes" (#611): a variable name from the
    /// paused frame's scopes.
    DataBreakpoint,
    /// Profiles: New Profile (#618): the new profile's name.
    NewProfile,
    ReviewComment,
    ReviewSummary,
    StickyNote,
    /// Rename the terminal pane at `idx`. The buffer is pre-filled with its
    /// current label; on commit the name overrides the auto label.
    RenameTerminal(usize),
    /// Annotate a span of terminal output on pane `pane` (Cmd+K N with a
    /// selection). `existing` edits that annotation in place; otherwise
    /// `line`/`start`/`len` pin a new one. A blank commit cancels (or, for
    /// an existing note, deletes it).
    AnnotateTerminal {
        pane: usize,
        line: i32,
        /// Scroll-clock reading `line` was captured against when the prompt
        /// OPENED. The commit anchors with this pair: sampling a fresh
        /// baseline at commit would mis-pin the note by every row that
        /// streamed while the user typed it.
        clock: i64,
        start: u16,
        len: u16,
        existing: Option<usize>,
    },
}

struct Prompt {
    label: String,
    buffer: String,
    /// Caret as a char index into `buffer`; `usize::MAX` is the end, which is
    /// where every prompt opens (read it through [`Prompt::caret`]).
    caret: usize,
    kind: PromptKind,
    target_dir: PathBuf,
    error: Option<String>,
}

/// The unsaved-changes prompt in front of a commit or stash (#1438): `op`
/// waits while `files` (dirty tabs under the repository) are unsaved.
/// `root` is the repository the request was made against: the mouse stays
/// live while the prompt is open, so focus can move Source Control to
/// another root before the choice, and the write must not follow it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingUnsavedScm {
    op: ScmWrite,
    root: PathBuf,
    files: Vec<PathBuf>,
}

/// A Drop Stash pick waiting on its confirm popup (#1128): a stash is
/// usually work kept nowhere else, and `git stash drop` has no undo. `root`
/// is the repository it was picked in, held in case Source Control moves
/// to another root before the answer. `oid` is the stash's commit, not its
/// index: a stash pushed or dropped while the question is up shifts every
/// `stash@{N}`, and the drop must still hit the stash that was named.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingStashDrop {
    root: PathBuf,
    oid: String,
    label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingDiscard {
    rel_path: String,
    untracked: bool,
    staged: bool,
}

/// A revert-hunk request waiting on its confirm popup. Holds the
/// single-hunk patch built when the user pressed R, so the confirm
/// reverse-applies exactly what they were looking at.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingRevertHunk {
    rel_path: String,
    patch: String,
}

/// A save refused because the file's encoding cannot hold the buffer
/// (#1053), waiting on the user's answer: what the write would destroy.
/// Asked in a modal, not the status bar, which trims a long message from the
/// middle and used to cut exactly the warning out of it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LossySave {
    /// The tab's file when asked, so the answer never writes another tab.
    path: Option<PathBuf>,
    encoding: &'static str,
    /// The characters `encoding` cannot represent. Empty when the loss is
    /// bytes that were not valid when the file was read, shown as U+FFFD.
    chars: Vec<char>,
}

/// A tour in progress (#377).
pub struct TourRun {
    pub tour: crate::tour::Tour,
    pub scratch: PathBuf,
    pub previous_root: PathBuf,
    /// The current step opened a palette, Quick Open or a picker, whose Esc
    /// then leaves the tour rather than only closing it (#863).
    pub step_opened_modal: bool,
}

/// The live ssh-pane workspace offer (#364): which pane's foreground became
/// `ssh <host>`, the resolved config alias, and when the offer appeared (it
/// expires on its own if neither accepted nor dismissed).
#[derive(Clone, Debug, PartialEq, Eq)]
struct SshOffer {
    pane: u64,
    host: String,
    since: std::time::Instant,
    /// The pane's OSC 7 report when the offer appeared, so accepting can
    /// tell the remote shell's directory from one left by an earlier shell.
    cwd_at_offer: Option<crate::remote::CwdReport>,
}

/// A left press forwarded to a mouse-tracking child (#474): which pane got
/// it, the host cell it landed on, and whether the drag since has become
/// croft's own text selection (a child that asked only for clicks cannot use
/// a drag, so croft keeps it). See `App::terminal_pointer_forwarded`.
///
/// The pane is held by its stable `uid`, NOT its index in `App::terminals`:
/// a pane closed while the button is held (the close chord, the header
/// button) shifts every later pane one slot left, and an index would then
/// deliver the release to the neighbour while the child that got the press
/// keeps a button it thinks is still down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ForwardedPointer {
    pane: u64,
    press: (u16, u16),
    selecting: bool,
}

/// State for edge auto-scroll during a terminal drag-selection. `dir` is
/// -1 (pointer above the top edge, scroll into history) or +1 (below the
/// bottom edge, scroll toward live); `col` is the last drag column so the
/// re-pinned selection head tracks the pointer horizontally; `last` rate-
/// limits the scroll so it advances at a readable speed rather than once
/// per main-loop wakeup.
#[derive(Clone, Copy)]
struct TerminalSelectAutoScroll {
    dir: i32,
    col: u16,
    last: std::time::Instant,
}

/// Active quick-select session on the focused terminal pane: the labelled
/// matches overlaid on the viewport, the label prefix typed so far, and
/// whether any typed char was UPPERCASE (commit then pastes the match into
/// the shell as well as copying it, WezTerm's convention).
pub struct QuickSelectState {
    /// Index into `App::terminals` of the pane the labels were scraped
    /// from: hints, pastes and re-pushes are bound to it, and any gesture
    /// that activates a sibling pane closes the mode (the copy-mode rule).
    pane: usize,
    /// Scroll-clock reading the hint lines were captured at (one snapshot
    /// with the lines themselves); every re-push reuses it so the pane's
    /// render-time translation keeps labels on their scrolled matches.
    clock: i64,
    hints: Vec<QuickHint>,
    typed: String,
    shifted: bool,
}

/// One labelled quick-select match: where it sits (absolute grid line plus
/// char span in the spacer-skipped row text), the text a committed label
/// copies, and the label itself.
#[derive(Clone)]
struct QuickHint {
    line: i32,
    start: usize,
    len: usize,
    text: String,
    label: String,
    /// Continuation segments `(absolute line, start, len)` when the match
    /// wraps the pane edge: highlighted alongside the head span, but the
    /// label glyph only ever paints on the head.
    tail: Vec<(i32, usize, usize)>,
}

/// Active copy-mode session on the focused terminal pane (WezTerm / tmux
/// copy mode): a keyboard cursor over the grid plus the selection anchor
/// and kind once v / V / Ctrl+V arms one. The pane paints the cursor and
/// the selection; this state owns the motions.
pub struct CopyModeState {
    /// Pane index the mode opened on. The coordinates and clock below are
    /// meaningless against any other pane (each pane owns an independent
    /// scroll clock), so a gesture that moves `active_terminal` while the
    /// mode is up closes it instead of hijacking the new pane.
    pane: usize,
    /// Cursor cell: absolute grid line + grid column.
    line: i32,
    col: u16,
    /// Selection anchor cell; `None` until v / V / Ctrl+V arms a selection.
    anchor: Option<(i32, u16)>,
    kind: CopySelKind,
    /// Scroll-clock reading the coordinates above are valid against. The
    /// pane re-anchors its painted selection to streaming content between
    /// keypresses; every key handler folds the clock movement into these
    /// coordinates first, or the keystroke would teleport the highlight
    /// back to the stale absolute lines.
    clock: i64,
}

/// A closed editor tab, reopenable via Cmd+K ⇧W (VS Code "Reopen Closed
/// Editor"): the file plus the view state needed to restore the tab as the
/// user left it. Unlike [`ClosedTerminal`] there is no live process to
/// park, so entries never expire; the stack is just bounded.
pub struct ClosedTab {
    pub path: PathBuf,
    pub pinned: bool,
    pub cursor: (usize, usize),
    pub scroll: usize,
}

/// A terminal pane parked by close for the undo-close grace window: the
/// live pane, the slot it occupied (so restore puts it back where it was),
/// and when it was closed (for expiry).
pub struct ClosedTerminal {
    pub term: crate::widgets::terminal::PtyTerminal,
    pub idx: usize,
    pub closed_at: std::time::Instant,
}

/// A "Running …" status an operation put in the status bar (#856), kept
/// until the operation ends so the bar can say how it went instead of
/// claiming it still runs: a test run, a task, a runnable fenced block.
/// All of them end through [`RunningStatus::settle`].
struct RunningStatus {
    /// The status text the operation set; replaced only while the bar
    /// still shows it, so a newer message is never clobbered.
    shown: String,
    /// What ran, as the outcome names it: a test's own name, `suite X`,
    /// `CodeQL tests`, a task's label, a fenced block's pane.
    label: String,
    started: std::time::Instant,
}

pub struct App {
    pub tree: FileTree,
    pub search: SearchPanel,
    pub remote: RemotePanel,
    pub source_control: SourceControlPanel,
    /// The Source Control COMMITS section: the repo-wide commit graph with
    /// lane rails, stacked under the change list. See [`CommitGraphPanel`].
    pub commit_graph: crate::widgets::commit_graph::CommitGraphPanel,
    /// Background `git log --topo-order` + lane layout for the COMMITS
    /// section, tagged by the root it describes so a stale reply after a
    /// Make Root is ignored on drain.
    graph_rx: GraphReplyRx,
    graph_tx: GraphReplyTx,
    /// What the COMMITS panel is currently showing: `None` for the root's
    /// own history, `Some("base..lane")` for a worktree lane's diff (#348).
    ///
    /// The reply tag carries this too, so a history refresh landing while a
    /// lane diff is up is discarded rather than replacing it - the panel
    /// already discarded replies for a since-left ROOT, and this is the same
    /// rule at the resolution the lane diff needs.
    graph_revspec: Option<String>,
    /// A graph fetch is running; suppresses duplicate spawns when a burst of
    /// StatusAndChanges responses lands. Cleared when the reply drains.
    graph_fetch_inflight: bool,
    /// A refresh was requested while a fetch was in flight (Make Root, a
    /// newer HEAD): re-fire once the stale reply drains instead of dropping
    /// the request (see [`Self::refresh_commit_graph`]).
    graph_refetch_queued: bool,
    pub run_debug: RunDebugPanel,
    /// The Testing side panel: the suite tree with live pass/fail status.
    pub testing: crate::widgets::testing::TestingPanel,
    /// The CodeQL side bar (#578).
    pub codeql: crate::widgets::codeql::CodeqlPanel,
    /// Background `cargo test` worker feeding the Testing panel.
    test_worker: crate::testing::worker::TestWorker,
    /// The run whose "Running test …" status awaits its outcome (#856).
    test_run_status: Option<RunningStatus>,
    /// The last test run started, for Testing: Rerun Last Run (#1535).
    last_test_run: Option<LastTestRun>,
    /// Tasks and fenced blocks whose "Running …" status awaits their pane's
    /// command-finished report (#856), by pane uid.
    pane_run_statuses: Vec<(u64, RunningStatus)>,
    /// The Extensions side panel: bundled + installed extensions with toggles.
    pub extensions: crate::widgets::extensions::ExtensionsPanel,
    /// Extension ids VS Code reported installed, from the last
    /// "Extensions: Compare with VS Code" (#352). Empty until the user asks,
    /// since listing them spawns `code`. Workspace recommendations are read
    /// from `.vscode/extensions.json` on every refresh instead.
    pub vscode_listed: Vec<String>,
    /// The Explorer's OPEN EDITORS section: the open editor tabs, projected
    /// from the active `editor` group each frame. See [`OpenEditorsPanel`].
    pub open_editors: OpenEditorsPanel,
    /// The Explorer's OUTLINE section, stacked below the file tree: the active
    /// editor's `documentSymbol` tree. See [`OutlinePanel`].
    pub outline: OutlinePanel,
    /// The Explorer's TIMELINE section: the active file's git history, fetched
    /// off-thread via [`crate::git::file_history`]. See [`TimelinePanel`].
    pub timeline: TimelinePanel,
    /// The Explorer's AGENT LANE section (#345).
    pub agent_lane_panel: crate::widgets::agent_lane::AgentLanePanel,
    /// Per lane file, the `+n −m` its row shows and what it was counted
    /// from: (reviewed snapshot, last agent write, disk mtime).
    lane_change_counts: std::collections::HashMap<PathBuf, (LaneCountKey, Option<(usize, usize)>)>,
    /// The Explorer's DEPENDENCIES section: the workspace's packages, resolved
    /// off-thread per detected ecosystem. See [`DependenciesPanel`].
    pub dependencies: DependenciesPanel,
    /// Package ecosystems detected at the workspace root (by manifest file),
    /// recomputed on every re-root. Empty means no recognised manifest, which
    /// hides the DEPENDENCIES section entirely and drops it from the ⋯ menu —
    /// the rule every reference editor follows. Drives the section's header
    /// label too. Cached so the cheap detection never runs on the render path.
    dep_ecosystems: Vec<Ecosystem>,
    /// Which Explorer sub-views are shown, toggled from the ⋯ menu. Persisted.
    pub explorer_views: ExplorerViewVisibility,
    /// Every file of the layered-settings chain (#251), for hot-reload
    /// matching on save; rebuilt by `remerge_settings`.
    settings_chain: Vec<PathBuf>,
    /// Which layer set each top-level settings key, for the hub's
    /// provenance suffix ("· workspace").
    settings_provenance: std::collections::BTreeMap<String, crate::config_layers::LayerKind>,
    /// Extension ids the user has disabled in the Extensions panel. Loaded once
    /// from prefs at startup and persisted on every toggle, mirroring how
    /// `explorer_views` is held in memory rather than re-read each access. The
    /// gates for the bundled features (PDF/CSV/Vim/LSPs) consult this set.
    disabled_extensions: std::collections::BTreeSet<String>,
    /// Ids of the installed extensions: the bundled ones plus those in the
    /// user extensions dir, read at launch and again whenever the Extensions
    /// panel refreshes. An add-on's feature, such as CodeQL's side bar
    /// (#1842), stays off until it is added.
    installed_extensions: std::collections::BTreeSet<String>,
    /// User key bindings from `~/.config/croft/keybindings.json`, consulted
    /// ahead of the built-in chords so a rebind wins over the default.
    keymap: crate::keymap::Keymap,
    /// Live macro recording (#255), `None` when not recording. Carries the
    /// vim register it will be stored under, or `None` for the palette
    /// recorder, whose macro is the unnamed "last recording".
    macro_recording: Option<crate::macros::Recording>,
    /// The most recent finished recording: what "Replay Last" and vim's `@@`
    /// run, and what a `q` with no register stores into.
    macro_last: Option<crate::macros::Macro>,
    /// Named registers (`qa` … `q`), loaded from macros.json at startup.
    macro_registers: std::collections::HashMap<String, crate::macros::Macro>,
    /// True while a replay is feeding keys back through `handle_key`, so a
    /// replayed key is never recorded and a macro cannot invoke itself.
    macro_replaying: bool,
    /// The store path, held so tests can point saves at a tempdir.
    macros_path: std::path::PathBuf,
    /// croft's own config files, checked for changes made outside croft,
    /// such as a file arriving by config sync (#262).
    config_watch: crate::config_sync::ConfigWatch,
    /// The click a user mouse binding is running for (#259), set only for
    /// the duration of that dispatch. Position-carrying commands read it;
    /// `None` everywhere else, so a keyboard invocation of the same command
    /// can never act on a stale click.
    last_mouse_pos: Option<(u16, u16)>,
    /// The tab a tab-strip mouse binding was clicked on (#1427), set only
    /// for that dispatch and taken by `run_command` as it starts, so only
    /// the directly bound command sees it: a command it runs in turn (an
    /// aborted rebase closing its tab, a replayed macro) acts on the active
    /// tab as it would from the keyboard.
    tab_click_target: Option<usize>,
    /// User snippets from `~/.config/croft/snippets.json`. Typing a snippet's
    /// prefix and pressing Tab expands it with tab-stop navigation.
    snippets: crate::snippets::SnippetSet,
    /// VS Code `editor.formatOnSave`: when true, `Cmd+S` first asks the language
    /// server to format the buffer, then writes once the reply lands. Loaded
    /// from prefs at startup, toggled from the palette / `Cmd+K F`.
    format_on_save: bool,
    /// The formatter the settings name for every file and per language
    /// (VS Code's `editor.defaultFormatter`, #1499). Format on save skips a
    /// file whose named formatter croft does not run.
    default_formatter: Option<String>,
    language_formatters: std::collections::BTreeMap<String, String>,
    /// Named formatters whose skipped save has been explained once this
    /// session (#1499); later saves say only "Saved".
    foreign_formatters_noted: std::collections::BTreeSet<String>,
    /// Per-language overrides (#1440), VS Code's `"[python]": { … }`, keyed
    /// by VS Code's language id. Read through the `*_for` helpers, which
    /// fall back to the global setting.
    language_prefs: std::collections::BTreeMap<String, crate::prefs::LanguagePrefs>,
    /// VS Code `terminal.integrated.copyOnSelection`: a finished terminal
    /// mouse selection lands on the clipboard without an explicit Cmd+C.
    /// Loaded from prefs at startup, toggled in the Settings hub.
    copy_on_select: bool,
    /// The project's `files_exclude` globs (#1345), as last applied to the
    /// Explorer, so a settings reload re-lists the tree only on a change.
    files_exclude: Vec<String>,
    /// The `files_associations` in effect (#1574), to tell a change.
    files_associations: std::collections::BTreeMap<String, String>,
    /// The workspace roots `files_associations` were installed for, so a
    /// root added, removed or replaced gets them or loses them (#1574).
    files_associations_roots: Vec<PathBuf>,
    /// The `files_encoding` default (#1432) as last applied, for a folder
    /// added to the workspace later.
    files_encoding: Option<crate::file_encoding::Charset>,
    /// Tailspin highlighting in rendered log views (#466): the inverse of
    /// the `disable_log_highlight` preference. Pushed into `log_view`'s
    /// default for views opened later and into every open view on toggle.
    log_highlight: bool,
    /// Built-in secret redaction rules are in the trigger set (#360).
    /// Settings "Terminal: Redact Secrets"; stored inverted in config.json
    /// as `disable_secret_redaction` so the default is on.
    secret_redaction: bool,
    /// "Terminal: Reveal Redacted Secrets": masks paint as their real text
    /// until this instant, then mask again on the next frame.
    redaction_reveal_until: Option<std::time::Instant>,
    /// VS Code's `files.autoSave: afterDelay`: dirty buffers write
    /// themselves to disk once the delay elapses after the last edit.
    auto_save: bool,
    /// VS Code's `files.autoSave: onFocusChange`: a dirty buffer writes
    /// itself the moment it loses focus. Independent of `auto_save`.
    pub auto_save_on_focus_change: bool,
    /// The focus signature (pane, group, active path) seen by the last
    /// auto-save tick. A change means something lost focus, which is the
    /// only trigger `onFocusChange` fires on. `None` until the first tick
    /// so opening croft never writes anything on its own.
    last_focus_signature: Option<(usize, usize, Option<PathBuf>)>,
    /// Latch set while a save is waiting on a format reply. `drain_lsp_format`
    /// consumes it to write the (now formatted) buffer to disk.
    save_after_format: Option<PathBuf>,
    /// The project's Prettier formatting the active file (#1255), whose
    /// reply completes a deferred save like an LSP format reply does.
    prettier_job: Option<crate::prettier::Job>,

    /// When a format-on-save stops waiting on its formatter and writes the
    /// file unformatted (#1491). Set with `save_after_format`.
    format_save_deadline: Option<std::time::Instant>,
    /// Background `git log` results for the TIMELINE, keyed by the file they
    /// describe so a stale reply for a since-closed file is ignored on drain.
    timeline_rx: std::sync::mpsc::Receiver<(PathBuf, Vec<crate::git::FileHistoryEntry>)>,
    timeline_tx: std::sync::mpsc::Sender<(PathBuf, Vec<crate::git::FileHistoryEntry>)>,
    /// Background folder-size scans for the CodeQL database card (#578): a
    /// large database is many files, so the walk stays off the input path.
    codeql_size_rx: std::sync::mpsc::Receiver<(PathBuf, Option<u64>)>,
    codeql_size_tx: std::sync::mpsc::Sender<(PathBuf, Option<u64>)>,
    /// Databases whose size scan is in flight, so a refresh does not start
    /// a second walk of the same folder.
    codeql_size_pending: std::collections::HashSet<PathBuf>,
    /// The file the TIMELINE has been fetched (or is being fetched) for, so the
    /// fetch fires exactly once per active-file change.
    timeline_fetched: Option<PathBuf>,
    /// When and for which folder the subfolder-repo scan behind the
    /// Source Control empty state last ran (#1227); rescanned at most
    /// every couple of seconds while that card shows.
    nested_repo_scan: Option<(PathBuf, std::time::Instant)>,
    /// Off-thread per-line git blame for the active editor (GitLens-style
    /// inline annotation). Fetched once per (file, HEAD) like the gutter.
    blame_rx: std::sync::mpsc::Receiver<(PathBuf, crate::git::Blame)>,
    blame_tx: std::sync::mpsc::Sender<(PathBuf, crate::git::Blame)>,
    /// The file blame has been fetched (or is in flight) for; reset to `None`
    /// on HEAD move so the annotation refreshes after a commit / checkout.
    blame_fetched: Option<PathBuf>,
    /// The commit whose card the pointer, resting on the inline blame, asked
    /// for (#1556), and where `git log` sends it. Kept once the card shows,
    /// so the card stays while the pointer moves along the annotation.
    blame_hover: Option<(String, std::sync::mpsc::Receiver<Result<String, String>>)>,
    /// The network git operation in flight (push, pull, fetch, clone...),
    /// named for the status line, and where its finisher arrives.
    git_net_job: Option<(String, std::sync::mpsc::Receiver<GitNetDone>)>,
    /// A read-only git view being computed off the UI thread (a TIMELINE
    /// commit diff, a COMMITS patch): a large repo's `git show` or rename
    /// walk froze the editor when run inline. A newer click replaces it.
    git_view_job: Option<std::sync::mpsc::Receiver<GitNetDone>>,
    /// The `vim_mode` pref as last applied (#1286). A settings reload
    /// changes the live mode only when the pref itself changed, so an
    /// unrelated edit to config.json does not reset the mode in use.
    vim_mode_pref: bool,
    /// User pref: show the current-line inline blame annotation (default on).
    inline_blame_enabled: bool,
    /// The provenance lens (#349): who typed each line, in the gutter and
    /// the blame annotation. Session-only for now; the persisted half of
    /// #349 (the map beside local history) is a later slice. Pushed onto
    /// every editor in every group by `sync_focus_flags`, since the gutter
    /// bar paints in every pane, not only the focused one.
    provenance_overlay: bool,
    /// Indentation guides (#129), VS Code `editor.guides.indentation`: on by
    /// default, pushed to every editor by the per-frame focus sync.
    indent_guides_enabled: bool,
    /// Home walks back one wrapped row per press (`home_walks_wrapped_rows`).
    home_walks_rows: bool,
    /// Bracket-pair colorization (#131): on by default, pushed to every
    /// editor by the per-frame focus sync like the indent guides.
    bracket_colors_enabled: bool,
    /// Whitespace rendering mode (#133), frame-synced to every editor;
    /// "selection" by default, the palette command cycles it.
    whitespace_mode: crate::widgets::editor::WhitespaceMode,
    /// Ruler columns (#1524) from the `rulers` setting, synced onto every
    /// editor each frame like `whitespace_mode`.
    rulers: Vec<u16>,
    /// Gutter line numbers (#1576): on, relative, interval or off.
    line_numbers: crate::widgets::editor::LineNumbers,
    /// Default ignore-whitespace mode applied to newly opened diffs (#258).
    diff_ws_default: crate::widgets::diff::DiffWhitespace,
    /// Debugger inline values (#135): on by default; rebuilt on every
    /// `InspectionUpdated`, cleared wherever the stop arrow clears.
    inline_values_enabled: bool,
    /// Save unsaved editors before every debug or run launch (#1400), so
    /// the debuggee runs the text its breakpoints were set against. Off
    /// only by `disable_save_before_debug` (`debug.saveBeforeStart: none`).
    save_before_debug: bool,
    /// Auto-closing pairs (#121), persisted; synced onto the active editor
    /// beside the blame flag.
    auto_close_pairs: bool,
    /// Re-indent a multi-line paste under the caret line (#1218), on unless
    /// `disable_paste_reindent` is set.
    paste_reindent: bool,
    /// The global on-save clean-ups (#1657), handed to each editor as it
    /// saves. A `.editorconfig` property still wins for its file.
    save_cleanup: crate::widgets::editor::SaveCleanup,
    /// LSP inlay hints (inline type / parameter annotations), on by default
    /// like VS Code's `editor.inlayHints.enabled`; toggled from the palette
    /// and persisted as `disable_inlay_hints` in config.json.
    inlay_hints_enabled: bool,
    /// Local-history root (`~/.config/croft/history`), cached so tests can
    /// redirect snapshots to a tempdir.
    history_root: PathBuf,
    /// The snapshot currently shown in a local-history diff, `(live file,
    /// snapshot millis, snapshot contents)`, so "Local History: Restore
    /// Snapshot" can write it back and carry its seats (#349). Cleared when a
    /// non-snapshot diff opens.
    history_restore: Option<(PathBuf, u64, Vec<u8>)>,
    /// Completed off-thread history recordings, drained to refresh the
    /// TIMELINE once the snapshot actually exists on disk.
    history_done_rx: std::sync::mpsc::Receiver<PathBuf>,
    history_done_tx: std::sync::mpsc::Sender<PathBuf>,
    /// The whole-project check's result: where it ran, and what the process
    /// did (#256). The `io::Result` carries a spawn failure separately from
    /// a run that produced output, because those need different messages.
    #[allow(clippy::type_complexity)]
    project_check_rx: std::sync::mpsc::Receiver<(PathBuf, std::io::Result<std::process::Output>)>,
    #[allow(clippy::type_complexity)]
    project_check_tx: std::sync::mpsc::Sender<(PathBuf, std::io::Result<std::process::Output>)>,
    /// Whether a sweep is in flight, so a second invocation says so rather
    /// than stacking compilers.
    project_check_running: bool,
    /// How many rows the last sweep produced (#256), counted before its
    /// items join a store shared with every other producer.
    project_check_rows: usize,
    /// Whether the sweep may run unasked (#256): `auto`, `on` or `off`.
    /// Read at startup; an unrecognised value behaves as `auto`.
    problems_project_scope: String,
    /// What gcc/clang-shaped build messages count their column in
    /// (`build_column_unit`), applied when a scan installs its rows.
    build_column_unit: crate::build_matchers::ColumnUnit,
    /// The PROBLEMS entry Go to Next/Previous Problem last opened, as (its
    /// file, line, column, place in the walked list) plus where that left
    /// the caret (row, column). Repeating the command from that caret steps
    /// from this entry, not from the caret: an entry past its line's end
    /// leaves the caret clamped short of it, which compared as still ahead
    /// and was picked again, and entries sharing a position were skipped.
    last_problem_jump: Option<(PathBuf, u32, u32, usize, usize, usize)>,
    /// Signals that this workspace is too large for `auto` to run the sweep
    /// unasked (#256). Sent from the sweep's worker, since deciding it is a
    /// whole-workspace walk.
    project_hint_rx: std::sync::mpsc::Receiver<()>,
    project_hint_tx: std::sync::mpsc::Sender<()>,
    /// One hint per session, not per run: repeating it every sweep would
    /// train the user to ignore the status line.
    project_check_hinted: bool,
    /// Replies from the background test-source locator (the workspace grep is
    /// too slow for the render thread): the root the search ran under, the
    /// test name, and the hit. A reply from before a re-root is dropped.
    #[allow(clippy::type_complexity)]
    test_jump_rx: std::sync::mpsc::Receiver<(PathBuf, String, Option<(PathBuf, u32)>)>,
    #[allow(clippy::type_complexity)]
    test_jump_tx: std::sync::mpsc::Sender<(PathBuf, String, Option<(PathBuf, u32)>)>,
    /// Background per-ecosystem dependency-resolution result for DEPENDENCIES.
    deps_rx: std::sync::mpsc::Receiver<Vec<Dependency>>,
    deps_tx: std::sync::mpsc::Sender<Vec<Dependency>>,
    /// The workspace root DEPENDENCIES was detected/fetched for, so detection
    /// and the fetch fire once per root (re-fired by Make Root / re-root).
    deps_fetched: Option<PathBuf>,
    /// Push-to-talk voice input (Termux OSK mic). The worker streams its
    /// transcript over this channel; `voice_handle` lets the mic release kill
    /// the in-flight recognizer.
    voice_rx: std::sync::mpsc::Receiver<crate::voice::VoiceMsg>,
    voice_tx: std::sync::mpsc::Sender<crate::voice::VoiceMsg>,
    voice_handle: Option<crate::voice::VoiceHandle>,
    /// Set when a mic tap cancels a live session, so the resulting empty result
    /// is reported as "canceled" rather than "no speech detected".
    voice_canceling: bool,
    pub editor: EditorTabs,
    /// The editor group layout tree. `editor` (above) is ALWAYS the active
    /// (focused) group, hoisted out of the tree's single active leaf, so the
    /// 430-odd `self.editor` call sites keep operating on whichever group has
    /// focus. The tree holds every OTHER group plus the spatial split
    /// structure; a single unsplit leaf is the just-launched state. Splitting,
    /// moving focus, and collapsing all go through [`editor_layout`].
    pub editor_layout: editor_layout::EditorLayout,
    /// One-shot latch armed when a save is refused because the file changed
    /// on disk. The next `save()` overwrites anyway, giving the user a
    /// "press Cmd+S again to overwrite" escape hatch without a modal.
    force_save_armed: bool,
    /// One-shot latch armed when a save failed for want of permission
    /// (#1379): the next Cmd+S of that file saves it with sudo instead.
    sudo_save_armed: Option<PathBuf>,
    /// Saves handed to `sudo tee` in a terminal pane (#1379): each file and
    /// the bytes it should end up holding, settled once it holds them.
    pending_sudo_saves: Vec<(PathBuf, Vec<u8>, Option<i32>)>,
    pub terminals: Vec<PtyTerminal>,
    pub active_terminal: usize,
    perf: PerfHud,
    /// Developer: Toggle Screencast Mode: the keys pressed, shown on screen
    /// for a recording. Off at every launch, as VS Code's is.
    screencast: Screencast,
    /// The workspace's root-folder set (#143). One entry today; the
    /// multi-root slices grow it. Read the primary via `workspace_root()`.
    roots: crate::workspace::WorkspaceRoots,
    sidebar_view: SidebarView,
    sidebar_areas: SidebarAreas,
    /// Active IDE color theme. Drives every explicit-background surface
    /// (baked icon PNGs, editor image/sheet/diff canvases, the `SetColors`
    /// session fill); `Color::Reset` surfaces follow the session bg for free.
    /// Toggled from the activity-bar gear menu; persisted via `crate::prefs`.
    theme: crate::theme::Theme,
    focus: Pane,
    show_tree: bool,
    show_terminal: bool,
    /// `Ctrl+Shift+J` maximizes the terminal pane: the editor / welcome
    /// collapses to zero height and the terminal fills the entire right
    /// column next to the Explorer. Pressing the chord again, or hiding
    /// the terminal entirely with `Ctrl+J`, clears the flag.
    terminal_maximized: bool,
    /// Layout (Customize Layout popup): whether the left activity bar is
    /// shown. Persisted via `crate::prefs`.
    activity_bar_visible: bool,
    /// Layout: whether the bottom status bar is shown. Persisted.
    status_bar_visible: bool,
    /// Layout: which edge the primary side bar (+ activity bar) docks to.
    /// Persisted.
    side_bar_position: SideBarPosition,
    /// Layout: whether the secondary side bar (the active file's Outline) is
    /// shown, on the edge opposite the primary side bar (⌥⌘B). Persisted.
    secondary_side_bar_visible: bool,
    /// Layout: horizontal span/alignment of the bottom panel. Persisted.
    panel_alignment: PanelAlignment,
    /// Layout: where the quick input (command palette / Go to File) anchors.
    /// Persisted.
    quick_input_position: QuickInputPosition,
    /// Layout: Zen Mode hides the activity bar, both side bars, the panel,
    /// and the status bar; toggling off restores `pre_zen`.
    zen_mode: bool,
    /// View: Toggle Centered Layout (#1585), persisted: the editor keeps
    /// `centered_layout_width` columns in the middle of its column.
    centered_layout: bool,
    /// Zen Mode starts centred (`zen_center_layout`, persisted, on by
    /// default), as VS Code's `zenMode.centerLayout`.
    zen_center_layout: bool,
    /// Whether this Zen session is centred: `zen_center_layout` on entry,
    /// until Toggle Centered Layout turns it off.
    zen_centered: bool,
    /// The centred editor's width in columns (#1585).
    centered_layout_width: u16,
    /// When the last Esc in Zen Mode that did nothing else landed: a
    /// second such Esc inside [`ZEN_ESC_WINDOW`] leaves Zen (#1585).
    zen_esc_at: Option<std::time::Instant>,
    /// Until when the "how to leave Zen" hint shows (#1585). Zen hides the
    /// status bar, so the hint draws on its own.
    zen_hint_until: Option<std::time::Instant>,
    /// Auto-hide the sidebar when focus moves to the editor or a terminal
    /// (#260, `sidebar_auto_hide`). Off by default. The sidebar comes back
    /// on any deliberate sidebar action — Cmd+B, an activity-bar icon, or a
    /// sidebar-targeting command like reveal-in-explorer.
    sidebar_auto_hide: bool,
    /// Set while a focus move is happening for a reason other than the user
    /// asking for it (async results, launch, sidebar-internal activation).
    sidebar_auto_hide_suspended: bool,
    /// One-shot exemption: the next auto-hide collapse is skipped and the
    /// flag consumed. Cmd+B reveals the sidebar WITHOUT moving focus, so a
    /// sticky flag would stay set (focus never lands on the tree to clear
    /// it) and silently disable auto-hide for the rest of the session.
    /// Consuming it on the next attempt keeps the reveal usable while
    /// guaranteeing the feature re-arms itself.
    sidebar_pinned_open: bool,
    /// Grace delay between focus leaving the sidebar and auto-hide collapsing
    /// it (#302), so a click passing through the panel does not take it away
    /// under the pointer.
    sidebar_dwell: SidebarDwell,
    /// Pre-Zen visibility snapshot; `None` whenever not in Zen Mode.
    pre_zen: Option<PreZenLayout>,
    /// Hit-test rects for the top-right layout-control icons.
    layout_icon_areas: LayoutIconAreas,
    status: String,
    /// Orange status-bar badge shown for the whole session when this is a
    /// remote (SSH) croft not running under dtach, so it can't survive a
    /// transport drop. Set once at startup; rendered separately from `status`
    /// so transient messages never clobber it. `None` otherwise.
    persistence_warning: Option<&'static str>,
    quit: bool,
    drop_to_local: bool,
    is_remote: bool,
    context_menu: Option<ContextMenu>,
    /// The type-to-filter state of an open breadcrumb picker: the typed text
    /// and the picker's full, unfiltered rows. Reset whenever a picker opens
    /// (`show_crumb_picker`) and read only while `context_menu.crumb` is set.
    crumb_filter: (String, Vec<MenuEntry>),
    prompt: Option<Prompt>,
    fs_watch: FsWatch,
    /// One extra watcher per SECONDARY workspace root (#147), sharing the
    /// primary's watch-cap budget. The primary `fs_watch` also owns the
    /// adaptive poll (its dir-mtime snapshot walks the whole tree, all
    /// roots included) and the open-file stats; extras are event-only.
    fs_watch_extra: Vec<FsWatch>,
    /// One git worker per SECONDARY workspace root (#149), keyed by the
    /// root path. `git` stays the primary's worker. Each discovers its
    /// own repo toplevel; replies are root-scoped by construction.
    git_extra: Vec<(PathBuf, GitWorker)>,
    /// The workspace root whose repository the Source Control panel and
    /// the status line follow: the root owning the focused editor's
    /// file, falling back to the primary. Recomputed each drain tick; a
    /// change re-seeds the panel from that worker and re-requests its
    /// change list.
    active_scm_root: PathBuf,
    /// A repositories-overview click pins the panel to that folder's repo
    /// (#161); the pin clears when the FOCUS-derived folder changes, so
    /// the panel resumes following the file the user works in.
    scm_pin: Option<PathBuf>,
    /// The last focus-derived folder, for detecting the change that
    /// releases `scm_pin`.
    scm_follow_root: PathBuf,
    /// The workspace root the Testing panel and worker are bound to —
    /// follows the focused file's folder like the SCM surfaces (#151).
    active_test_root: PathBuf,
    /// The last `followed_test_root` answer and the workspace folder and
    /// focused file it was for (#1520).
    test_root_follow: Option<((PathBuf, Option<PathBuf>), PathBuf)>,
    /// Folder set from an explicitly opened `.code-workspace` (#163):
    /// consumed by `change_workspace_root` INSTEAD of the automatic
    /// per-primary restore, so the file defines the arrangement exactly.
    pending_workspace_folders: Option<Vec<PathBuf>>,
    /// Per-workspace-root last-seen HEAD oid, for gutter-baseline and
    /// commit-graph invalidation per repository (#149).
    git_head_oids: std::collections::BTreeMap<PathBuf, Option<String>>,
    git: GitWorker,
    cursor_blink: CursorBlink,
    /// Overwrite (Replace) mode, toggled by the Insert key. One switch for the
    /// whole app, as in VS Code: it follows you from tab to tab.
    overwrite_mode: bool,
    /// Whether the terminal's caret was last set to the overwrite block, so
    /// the style escape is sent only when the wanted shape changes.
    caret_block_sent: bool,
    editor_click: ClickTracker,
    tree_click: ClickTracker,
    terminal_click: ClickTracker,
    /// Modified clicks, so `ctrl+double_click` and friends can be recognised
    /// without arming the unmodified trackers above (see the type's docs).
    /// One tracker, not one per region: it is only ever read by the binding
    /// dispatch, and a double needs both clicks at the same cell, so two
    /// regions cannot be confused for one another.
    modified_click: ModifiedClickTracker,
    /// Double-click tracking for PORTS rows: a second click on the same row
    /// opens that port, the same as pressing Enter on it.
    ports_click: ClickTracker,
    /// Active edge auto-scroll while drag-selecting in the terminal: set
    /// when the drag pointer is pinned past the top/bottom edge, serviced
    /// each main-loop tick to keep growing the selection through
    /// scrollback. `None` whenever the pointer is inside the pane or the
    /// button is released.
    terminal_select_autoscroll: Option<TerminalSelectAutoScroll>,
    /// Pane whose scrollbar is currently being dragged with the left mouse button.
    scrollbar_drag: Option<Pane>,
    /// True while the editor's horizontal scrollbar thumb is being dragged.
    /// Only the editor has a horizontal bar, so a bool suffices.
    editor_hscrollbar_drag: bool,
    /// A left press in the editor's breakpoint lane toggled a breakpoint
    /// (#1544), so the rest of that gesture belongs to the lane: drags are
    /// dropped until the release. Without it the first drag (some terminals
    /// send one at the press cell) would anchor a selection at the caret and
    /// walk the caret to the clicked line, undoing the caret-preserving
    /// click. Cleared on the release and on the next press.
    breakpoint_lane_press: bool,
    /// The line a triple-click or a line-number press selected (#1570): a
    /// drag from it selects whole lines, from this one to the line under
    /// the pointer. Cleared on the release and on the next press.
    editor_line_drag: Option<usize>,
    /// A left-drag is moving the OUTLINE panel's scrollbar thumb. The outline
    /// is not a focusable `Pane`, so it gets its own drag latch rather than
    /// riding `scrollbar_drag`.
    outline_scrollbar_drag: bool,
    /// The secondary side bar's own view of the Outline (scroll and hit
    /// rects), swapped into `outline` around that bar's render and mouse
    /// handling so the two hosts scroll and click independently (#1326).
    secondary_outline: crate::widgets::outline::OutlineView,
    /// The secondary Outline's scrollbar thumb is being dragged.
    secondary_outline_scrollbar_drag: bool,
    /// Scrollbar-thumb drag latches for the other stacked Explorer sub-views,
    /// matching `outline_scrollbar_drag` (none is a focusable `Pane`).
    open_editors_scrollbar_drag: bool,
    timeline_scrollbar_drag: bool,
    deps_scrollbar_drag: bool,
    testing_scrollbar_drag: bool,
    graph_scrollbar_drag: bool,
    welcome: WelcomeState,
    /// One-shot background refresh of the remote extension index. Polled in the
    /// main loop; rebuilds the Extensions panel once if the verified cache
    /// changed. `None` once consumed (or on a disarmed build, immediately).
    ext_index_refresh: Option<std::sync::mpsc::Receiver<bool>>,
    /// The `codeql` executable; tests point it at a stand-in (#578).
    codeql_program: PathBuf,
    /// The query run in flight: its outcome arrives here, and when it
    /// started (#578).
    codeql_run: Option<(
        std::sync::mpsc::Receiver<crate::codeql_query::RunStatus>,
        std::time::Instant,
    )>,
    /// Raised to stop the query run in flight: its `codeql` is killed and
    /// the run recorded as cancelled (#578).
    codeql_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The file name of the query in flight, for its status-bar chip.
    codeql_run_name: String,
    /// Queries "Run Queries in Pack" has yet to start, in order (#578).
    codeql_run_queue: std::collections::VecDeque<crate::codeql_query::QueuedRun>,
    /// "Run Query on Multiple Databases" being set up (#578): the query,
    /// its text, and the databases checked so far.
    codeql_multi_db: Option<(PathBuf, String, Vec<PathBuf>)>,
    /// How many queries the pack run holds and how many have failed so
    /// far, for the status line; the total is 0 when no pack run is on.
    codeql_batch: (usize, usize),
    /// The database upgrade or cache cleanup in flight (#578).
    codeql_upgrade: Option<CodeqlUpgrade>,
    /// The `codeql version` call behind "CodeQL: Copy Version Information"
    /// (#578): its bare version number, or why it could not be read.
    codeql_version: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// The evaluator log summary being generated (#578): where the outcome
    /// arrives, and the summary file to open once it lands.
    codeql_log_summary: Option<(std::sync::mpsc::Receiver<Result<(), String>>, PathBuf)>,
    /// The evaluator log viewer being prepared (#578): where the rendered
    /// tree (or why there is none) arrives, and the tab label to show it in.
    codeql_log_viewer: Option<CodeqlLogViewerJob>,
    /// The Model Editor's endpoints being read (#578): the endpoints or why
    /// not, and the database and language they are for.
    codeql_model_job: Option<CodeqlModelJob>,
    /// The performance comparison being prepared (#578): where the rendered
    /// table (or why there is none) arrives, and the tab label to show it in.
    codeql_perf_compare: Option<(std::sync::mpsc::Receiver<Result<String, String>>, String)>,
    /// The query help being rendered (#578): where the outcome arrives,
    /// the Markdown file to preview once it lands, and the query's name.
    codeql_query_help: Option<CodeqlQueryHelp>,
    /// The pack install or download in flight (#578): where its outcome
    /// arrives, and the status lines for success and failure.
    codeql_pack_job: Option<CodeqlPackJob>,
    /// A database being downloaded or extracted (#1155): what is being
    /// added, for the status line, and where its folder arrives.
    codeql_db_job: Option<(String, std::sync::mpsc::Receiver<Result<PathBuf, String>>)>,
    /// A CodeQL document being made (#578: View CFG, the alert and result
    /// views): the file it writes or why not, and the status lines for
    /// each.
    codeql_doc_job: Option<CodeqlDocJob>,
    /// The CodeQL results table last walked (#578), so the walk goes on
    /// once the code it opened has the focus.
    codeql_results_walk: Option<PathBuf>,
    /// The AST being read for the AST Viewer (#578): the tree or why not,
    /// and the view it fills in.
    codeql_ast_job: Option<(
        std::sync::mpsc::Receiver<Result<crate::codeql_ast::AstTree, String>>,
        crate::codeql_ast::AstView,
    )>,
    /// `codeql_cli_path` from settings (#578), kept to notice a reload
    /// changing it.
    codeql_cli_setting: Option<String>,
    /// Where [`Self::codeql_program`] came from (#578).
    codeql_cli_source: crate::codeql_cli::Source,
    /// The newest CLI release "CodeQL: Check for CLI Updates" found, which
    /// "CodeQL: Download CLI" then installs (#578).
    codeql_cli_latest: Option<String>,
    /// The CLI download in flight (#578).
    codeql_cli_download: Option<std::sync::mpsc::Receiver<crate::codeql_cli::JobUpdate>>,
    /// The CLI update check in flight (#578): the latest release and the
    /// version in use, if it could be read.
    codeql_cli_check: Option<std::sync::mpsc::Receiver<CodeqlCliCheck>>,
    /// The network calls behind those two, replaced in tests.
    codeql_cli_net: crate::codeql_cli::Net,
    /// The GitHub Code Search in flight: the list its repositories go
    /// into, and its outcome (#578).
    codeql_code_search: Option<CodeqlCodeSearch>,
    /// The variant analysis being bundled and submitted (#578).
    codeql_variant_submit:
        Option<std::sync::mpsc::Receiver<Result<crate::codeql_submit::Submitted, String>>>,
    /// The read of unfinished variant analyses' progress in flight, and
    /// when the last one started (#578).
    codeql_variant_poll: Option<std::sync::mpsc::Receiver<Vec<CodeqlRunPoll>>>,
    codeql_variant_polled: Option<std::time::Instant>,
    /// The fetch of a variant analysis's results in flight (#578).
    codeql_variant_results: Option<std::sync::mpsc::Receiver<VariantResults>>,
    /// The export of a variant analysis's results in flight (#578): where
    /// it went (a folder's summary file or a gist's URL), how many
    /// repositories it holds, and those whose results failed.
    codeql_variant_export: Option<std::sync::mpsc::Receiver<Result<VariantExport, String>>>,
    /// The walk for the workspace's queries in flight (#840): opening the
    /// CodeQL view starts it off the UI thread and the packs land here. It is
    /// tagged with the root it walks, so a root the workspace has left since
    /// never lists its queries.
    codeql_queries_job: Option<(
        PathBuf,
        std::sync::mpsc::Receiver<Vec<crate::codeql_query::QueryPack>>,
    )>,
    /// True when the in-flight refresh was triggered by the user (the ⟳ button),
    /// so its completion reports a status line; the silent startup refresh
    /// doesn't, to avoid clobbering more useful startup messages.
    ext_index_manual_refresh: bool,
    /// Channel to the background search worker. Each keystroke or toggle
    /// flip pushes a `(query, opts)` request here; the worker debounces
    /// and runs `search_workspace` off the UI thread.
    search_query_tx: std::sync::mpsc::Sender<crate::widgets::search::SearchRequest>,
    /// Results coming back from the search worker: `(query, hits)`. The
    /// query is echoed so we can drop stale results when the user has
    /// typed past the query that produced them.
    search_results_rx: std::sync::mpsc::Receiver<crate::widgets::search::SearchEvent>,
    /// Pixel size of one terminal cell, captured in `init_graphics`.
    /// Required to bake OSC-1337 images at exact viewport pixel size so
    /// iTerm draws them with no stretching or letterboxing.
    cell_pixel: Option<(u32, u32)>,
    /// The activity-bar icons and panel illustrations `init_graphics` is
    /// baking off the UI thread (#1973); `None` once they are up.
    chrome_bake: Option<std::sync::mpsc::Receiver<(ChromeBakeInput, ChromeImages)>>,
    /// What the activity-bar icons and panel illustrations on screen were
    /// baked for; `None` while none are up.
    chrome_shown: Option<ChromeBakeInput>,
    /// The bake asked for while `chrome_bake` was still running: it runs
    /// when that one lands, in its place.
    chrome_bake_next: Option<ChromeBakeInput>,
    /// The welcome wordmark being baked off the UI thread (#1973), and what
    /// for; `None` once it is up.
    welcome_bake: Option<(WelcomeBakeKey, std::sync::mpsc::Receiver<Option<String>>)>,
    /// Last wheel step applied to a PDF preview (when, in which direction,
    /// and on which document), so a momentum flick's same-direction burst
    /// coalesces instead of queueing one blocking rasteriser run per event.
    /// Keyed by document: switching to another PDF starts fresh - a reader's
    /// first gesture there is not a repeat.
    last_pdf_wheel: Option<(std::time::Instant, i32, Option<PathBuf>)>,
    /// One-shot consent to overwrite FORMULA cells on the next xlsx
    /// save (#178/#194): armed by the save that held them back, distinct
    /// from `force_save_armed` (the disk-conflict consent).
    formula_overwrite_armed: bool,
    /// Pre-encoded inline-image for the source-control change-count badge (an
    /// accent pill + count, VS Code's `activityBarBadge`), emitted at z=1 over
    /// the SCM activity icon's bottom-right. `None` when there are no changes,
    /// not in a repo, or image rendering is off (the glyph path draws a text
    /// badge instead). Rebuilt by `refresh_scm_change_badge` when the change
    /// set, theme, or cell size shifts.
    scm_change_badge: Option<String>,
    /// Change count the cached `scm_change_badge` was built for; drives the
    /// dirty check and the badge's on-screen cell width.
    scm_change_badge_count: usize,
    /// Pre-encoded inline-image for the Explorer change badge: the count of
    /// open editors with unsaved edits (VS Code's Explorer activity badge),
    /// emitted at z=1 over the Explorer icon's bottom-right. `None` when nothing
    /// is unsaved or image rendering is off. Mirrors the SCM badge plumbing;
    /// rebuilt by `refresh_explorer_unsaved_badge`.
    explorer_unsaved_badge: Option<String>,
    /// Unsaved-file count the cached `explorer_unsaved_badge` was built for.
    explorer_unsaved_count: usize,
    /// Pre-encoded inline-image for the Remote icon's forwarded-ports badge
    /// (VS Code's Ports view count), emitted at z=1 over the Remote icon. `None`
    /// when no port is forwarded or image rendering is off. Same plumbing as the
    /// other count badges; rebuilt by `refresh_remote_ports_badge`.
    remote_ports_badge: Option<String>,
    /// Forwarded-port count the cached `remote_ports_badge` was built for.
    remote_ports_count: usize,
    /// Pre-encoded inline-image for the Testing icon's failing-test count badge
    /// (red pill, VS Code's Testing badge), emitted at z=1 over the beaker icon.
    /// `None` when nothing is failing or image rendering is off. Same plumbing as
    /// the other count badges; rebuilt by `refresh_testing_badge`.
    testing_badge: Option<String>,
    /// Failing-test count the cached `testing_badge` was built for.
    testing_badge_count: usize,
    /// Problem count the cached `overlays.problems_badge` circle was built for;
    /// gates the per-frame rebuild in `refresh_problems_badge`.
    problems_badge_count: usize,
    /// Anchor cell (top-left) where the PROBLEMS-tab circle badge should be
    /// emitted this frame, recorded by `paint_panel_tabs` in images mode. `None`
    /// when the panel strip isn't drawn or the count is zero; the post-frame
    /// `flush_problems_badge_overlay` clears the image when it's `None`.
    problems_badge_cell: Option<(u16, u16)>,
    /// Which inline-image protocol the host terminal speaks, resolved once at
    /// startup (env-fixed). Drives the [`crate::iterm2_inline::build_inline_image`]
    /// dispatch (iTerm2 OSC-1337 vs Kitty graphics) and the Kitty-only
    /// delete-all eviction on the clear chain.
    inline_protocol: crate::iterm2_inline::InlineImageProtocol,
    /// Set once by the runtime DA1 probe (`run`) when the host terminal lacks an
    /// env-advertised protocol (iTerm2/Kitty) but answers DA1 with sixel
    /// support. `init_graphics` promotes `inline_protocol` to `Sixel` when this
    /// is true, and preserves it across the theme-switch re-detect that would
    /// otherwise reset to `None` (sixel has no env signal to re-derive from).
    sixel_supported: bool,
    /// One-shot: a WINCH arrived, so the next frame must wipe and repaint every
    /// cell on EVERY protocol. A dtach reattach fires WINCH (`-r winch`) against
    /// a blank physical screen while ratatui's buffer still says everything is
    /// painted, so the diff alone emits nothing; the Kitty path skips the
    /// image-eviction wipe (the typing-blink fix), so resize owns its own.
    resize_repaint_needed: bool,
    /// VS Code-style editor minimap (rasterized document preview in a right/left
    /// strip). On by default; toggled and re-sided from the strip's right-click
    /// menu.
    minimap_visible: bool,
    minimap_side: MinimapSide,
    /// Cached whole-file raster; recomposited (not rebaked) on scroll.
    minimap_base: Option<MinimapBase>,
    /// Hit-test rect of the on-screen minimap image (text-aligned rows), for
    /// click-to-scroll and the right-click menu. Empty when the minimap is off.
    minimap_img_rect: Rect,
    /// Pixel height the lines are mapped into within the image (top-aligned;
    /// `<=` the image's pixel height for short files). Click mapping divides by
    /// this, not the full strip height, so clicks land on the right line.
    minimap_content_h: u32,
    /// When the minimap image was last baked, and whether an edit since then
    /// is waiting for the next bake (see [`MINIMAP_EDIT_REBAKE`]).
    minimap_baked_at: Option<std::time::Instant>,
    minimap_edit_pending: bool,
    /// The strip was last drawn as braille text, where the terminal cannot
    /// draw images (#1231); `minimap_content_h` then counts dot rows.
    minimap_text: bool,
    /// The braille cells for the text strip, rebuilt only when the same
    /// signature the image bake keys on changes.
    minimap_text_cache: Option<(MinimapSig, Vec<BrailleCell>)>,
    /// Whether the editor was wide enough for the strip on the last frame,
    /// so turning the minimap on can say why it does not show.
    minimap_room: bool,
    /// The buffer ratatui drew last, and what each small chrome image (the
    /// PROBLEMS badge, Run and Debug icon, sidebar illustrations) was sent
    /// over, keyed by overlay, for [`App::chrome_image_due`]. Keys not
    /// checked in a frame (the image was hidden) are dropped.
    drawn_buffer: Option<ratatui::buffer::Buffer>,
    chrome_sent: std::collections::HashMap<&'static str, u64>,
    chrome_seen: std::collections::HashSet<&'static str>,
    /// A left-drag is panning the view via the minimap strip.
    minimap_drag: bool,
    /// Clipboard read entrypoint. Production uses the host clipboard; tests
    /// can swap in a deterministic reader for Cmd+V routing assertions.
    clipboard_reader: fn() -> Option<String>,
    /// Whether this croft is a relay session: a `croft remote` child whose
    /// LOCAL launcher runs the drop pump, so the clipboard the user pastes
    /// from lives on the other machine (#538).
    ///
    /// Resolved ONCE here rather than read per copy. `CROFT_RELAY_KEY` is
    /// process-global, and `deliver_copied_text` asks this question on every
    /// copy: a relay test that sets the variable would otherwise reroute the
    /// copy of every test running beside it, which is exactly what it did
    /// (three suites broke). Inert under `cfg(test)` for the same reason the
    /// collab channel's read is — a test opts in by setting the field — so no
    /// test can be steered by another's environment.
    is_relay_session: bool,
    remote_launch: Option<RemoteLaunch>,
    pending_remote_launch_host: Option<String>,
    pending_remote_launch_path: Option<String>,
    /// The ssh-pane offer currently shown in the status bar (#364), if any.
    /// Accepted with Cmd+K G, dismissed with Esc, dropped when the pane's
    /// ssh ends or after `SSH_OFFER_TTL`.
    ssh_offer: Option<SshOffer>,
    /// What each pane (by uid) was last seen connected to, so an ssh session
    /// is offered once when it starts rather than on every sample.
    ssh_offer_seen: std::collections::BTreeMap<u64, Option<String>>,
    /// The label thread's ssh samples (#364): `(shell pid, resolved host)`
    /// per pane it looked at, shipped beside the names it already resolves
    /// so the argv read and the `~/.ssh/config` parse stay off the loop.
    ssh_rx: std::sync::mpsc::Receiver<Vec<(i32, Option<String>)>>,
    ssh_tx: std::sync::mpsc::Sender<Vec<(i32, Option<String>)>>,
    /// `disable_remote_offer` from prefs (#364).
    remote_offer_disabled: bool,
    /// `remote_offer_excluded_hosts` from prefs (#364).
    remote_offer_excluded: Vec<String>,
    /// `fleet_groups` from prefs (#363): named host sets a fleet run can
    /// expand, so a fleet is named once rather than retyped per run.
    fleet_groups: std::collections::BTreeMap<String, Vec<String>>,
    /// The `code_scanning` setting (#577).
    code_scanning: crate::sarif::github::CodeScanningMode,
    /// The branch (or detached HEAD) code scanning last looked at, so a
    /// checkout of another one is noticed.
    code_scan_seen: Option<String>,
    /// A code scanning lookup in flight: the branch it is for, whether to
    /// load what it finds, and where the chosen analyses arrive.
    code_scan_list: Option<CodeScanList>,
    /// Chosen analyses fetching as SARIF: where their saved files arrive.
    code_scan_fetch: Option<std::sync::mpsc::Receiver<Vec<Result<PathBuf, String>>>>,
    /// Hosts whose provisioning failed recently (#364), loaded from the
    /// cache file at startup and extended when an install fails.
    remote_offer_refused: crate::remote::RefusedHosts,
    /// The `agents.json` row a new worktree lane starts in its pane (#348);
    /// `None` opens a plain shell. From `lane_agent` in settings.json.
    lane_agent: Option<String>,
    /// The colour depth each frame is fitted to before it is written
    /// (#1433): the `color_depth` setting, else what the terminal's
    /// environment advertises.
    pub(crate) color_depth: crate::color_depth::ColorDepth,
    /// Which panes are worktree lanes' (#348), by pane uid: saved with the
    /// terminal session so a relaunch seats the agent again, and consulted
    /// when a lane closes so its pane goes with it.
    lane_panes: std::collections::BTreeMap<u64, crate::terminal_session::LaneRecord>,
    /// Explorer-scoped Cut/Copy buffer. Independent from the OS clipboard
    /// (which carries text), this stores filesystem paths and the intent
    /// (move vs. copy) until the next Paste consumes it.
    tree_clipboard: Option<ExplorerClipboard>,
    /// Explorer type-to-jump buffer. Each printable keystroke within
    /// `TREE_TYPEAHEAD_WINDOW` of the previous one accumulates; a
    /// keystroke after the gap resets the buffer and cycles past the
    /// current selection. Matches VS Code's tree typeahead.
    tree_typeahead: Option<TreeTypeahead>,
    /// File the user picked via the explorer's "Select for Compare" menu
    /// action. None until they pick one; cleared once they invoke
    /// "Compare with Selected" against another file.
    compare_anchor: Option<PathBuf>,
    /// Whole-screen rect captured at the start of every `render`. The
    /// context-menu hit-test reads this to clamp the menu's bounds the
    /// same way the renderer does, so a menu that gets shifted up to fit
    /// on screen still maps clicks to the right item.
    last_frame_area: Rect,
    /// The Explorer/sidebar column rect from the last render (`None` when
    /// the sidebar is collapsed). The zoxide jump popup anchors over this
    /// so it drops from the Explorer rather than centering on the frame.
    last_sidebar_area: Option<Rect>,
    /// Active explorer drag-and-drop, if any.
    tree_drag: Option<ExplorerDrag>,
    /// Pending SCP uploads queued by a Finder drag-drop onto the Remote
    /// Explorer. These are intentionally NOT run inline — the main loop
    /// drains the queue after suspending the alt-screen so scp can use
    /// the host shell for password / FIDO / known_hosts prompts and the
    /// user actually sees what's happening.
    pending_scp_uploads: Vec<ScpUpload>,
    /// Drops awaiting reverse-pull from the user's local Mac via the
    /// drop-relay launched by the local croft parent. Polled each frame.
    pending_remote_pulls: Vec<PendingRemotePull>,
    /// This croft's `croft view` listener (#362).
    ///
    /// `None` when the bind failed: a croft that cannot offer `croft view`
    /// must still start, so the failure costs the feature and nothing else.
    /// Panes then see no `CROFT_VIEW_SOCK` and the client says so plainly,
    /// which is the same message a pane outside croft gets.
    ///
    /// No path is kept alongside it. The doc used to promise "the socket path
    /// to unlink when it goes away" and nothing unlinked it: there is no
    /// `Drop for App`, both readers discarded the second element, and the
    /// launch-time sweep is what actually reclaims a dead croft's socket -
    /// deliberately, since an unclean exit leaves no `Drop` to run either.
    /// A field carrying a promise the code does not keep is worse than no
    /// field.
    view_listener: Option<std::os::unix::net::UnixListener>,
    /// Agent edit approvals (#347): this workspace's hook socket, the
    /// proposals waiting on the user in arrival order, and the popup's
    /// state for the one at the head (`Some` exactly while one waits).
    hook_listener: Option<std::os::unix::net::UnixListener>,
    /// The workspace the hook socket was bound for: an approval
    /// notification's links name it, so `croft decide` finds this croft
    /// (#359).
    hook_root: PathBuf,
    approvals: std::collections::VecDeque<crate::agent_approval::Pending>,
    approval_ui: Option<crate::agent_approval::ApprovalUi>,
    /// The language server's check of the proposal at the head of the
    /// queue (#347): its diagnostics show in the popup before approval.
    approval_check: Option<crate::agent_approval::ProposalCheck>,
    /// A proposal being edited before approval (#347): its token, and the
    /// scratch file whose save approves the edited text. The popup stays
    /// down while it lasts.
    approval_edit: Option<(String, PathBuf)>,
    /// When that edit is a three-way merge with a dirty tab's unsaved
    /// edits, the tab's file: once approved, the tab takes the merged text.
    approval_merge_into: Option<PathBuf>,
    /// One Jupyter kernel per notebook that has run a cell (#355), by path.
    /// Dropping one (the map entry going) shuts its kernel down.
    notebook_kernels: std::collections::HashMap<PathBuf, crate::notebook_kernel::NotebookRun>,
    /// Notebook: Select Kernel asking for the installed kernels (#1377):
    /// the notebook it is for and the bridge listing them.
    kernel_listing: Option<(PathBuf, crate::notebook_kernel::KernelSession)>,
    /// The kernel picker's notebook, the kernelspecs its rows name, and
    /// the kernel the notebook asked for that is not installed, if that is
    /// why it opened.
    kernel_choice: Option<(
        PathBuf,
        Vec<crate::notebook_kernel::KernelSpec>,
        Option<String>,
    )>,
    /// When the shown coverage report was taken, the file its editor lens
    /// was built for, and the install command a missing coverage tool
    /// needs (#263).
    coverage_at: Option<std::time::SystemTime>,
    coverage_lens_path: Option<PathBuf>,
    coverage_install: Option<String>,
    /// How long one frame may spend serving `croft view` clients (see
    /// `drain_view_requests`). A field so a test on a loaded machine can
    /// stretch it: the fixed 20ms is shorter than one file open there.
    view_drain_budget: std::time::Duration,
    /// URL awaiting the user's local-browser confirmation (remote-
    /// launched croft only). When `Some`, a modal asks Y/A/N and all
    /// other keys are swallowed.
    pending_local_open: Option<String>,
    /// Source Control entry awaiting Y/N confirmation for destructive
    /// `Discard Changes`. Holds the relative path and whether it was an
    /// Untracked entry (Untracked discard deletes the file from disk;
    /// tracked discard runs `git checkout HEAD -- <path>`). Both paths
    /// destroy uncommitted work, so the modal must always run first.
    pending_discard: Option<PendingDiscard>,
    /// A commit or stash asked for while tabs under the repository held
    /// unsaved edits (#1438). The write records the files on disk, so the
    /// modal asks first: save them all, go ahead without them, or cancel.
    pending_unsaved_scm: Option<PendingUnsavedScm>,
    /// The repository the open Delete Remote Tag picker listed (#1582),
    /// so the deletion stays bound to it if Source Control moves.
    remote_tag_root: Option<PathBuf>,
    /// Set for the one call that runs a write the prompt already cleared.
    scm_unsaved_cleared: bool,
    /// A write waiting on a format-on-save reply: Save All armed the
    /// active tab's deferred write, and the commit runs once it lands.
    scm_after_format_save: Option<(ScmWrite, PathBuf)>,
    /// Armed by R in a Source-Control diff; reverting a hunk destroys
    /// uncommitted work, so the confirm modal always runs first.
    pending_revert_hunk: Option<PendingRevertHunk>,
    /// An unsaved-changes prompt (#862) waiting on S / D / Esc, holding what
    /// it goes on to do. While `Some`, the modal eats every key and click.
    pending_unsaved: Option<UnsavedExit>,
    /// A save that would lose characters or bytes to the file's encoding
    /// (#1053), waiting on S / Esc. While `Some`, the modal eats every key
    /// and click, so a second Cmd+S can no longer write it.
    pending_lossy_save: Option<LossySave>,
    /// The unsaved state (see [`App::unsaved_stamp`]) the user chose to
    /// leave behind when asked about a remote connect (#862): the handoff
    /// that ends this croft later goes without asking again only while
    /// nothing has changed since.
    remote_handoff_ok: Option<u64>,
    /// Hot exit (#862): the directory this croft backs its unsaved buffers
    /// up under (`~/.local/state/croft/hot-exit`, see [`user_data_dir`]).
    /// `None` keeps no backups, as
    /// under test unless a test points it at a tempdir.
    pub hot_exit_dir: Option<PathBuf>,
    /// The [`App::unsaved_stamp`] this croft's backup on disk was written
    /// at, `None` while it has none: the tick writes only when they differ.
    /// A write that fails leaves it alone, so a later tick tries again.
    hot_exit_written: Option<u64>,
    /// When a failed hot-exit write may be tried again (#862): a full disk
    /// is retried every [`App::HOT_EXIT_DELAY`], not every frame. `None`
    /// once a write lands.
    hot_exit_retry_at: Option<std::time::Instant>,
    /// Backups of crofts that are gone whose text this croft restored
    /// (#862). Removed once its own backup holds that text, so a write that
    /// fails never leaves the text in memory only.
    hot_exit_consumed: Vec<PathBuf>,
    /// Consumed backups that could not be removed once this croft's own
    /// backup landed (#862 review). They may still hold text, so the quit
    /// tries them again and says which are left.
    hot_exit_unremoved: Vec<PathBuf>,
    /// Lines for stderr once the terminal is the shell's again, about what
    /// the user must still hear after croft has gone: a hot-exit backup a
    /// quit could not remove (#862).
    exit_notes: Vec<String>,
    /// Where each clean exit saves the session for the next launch, one
    /// file per workspace (`~/.local/state/croft/sessions`, see
    /// [`crate::session_state::saved_path`]). `None` saves none, as under
    /// test unless a test points it at a tempdir.
    pub session_dir: Option<PathBuf>,
    /// The saved session being read off the UI thread once the first frame
    /// is up, and whether its tabs may come back (hot exit brought none) or
    /// only its bookmarks.
    saved_session: Option<(
        std::sync::mpsc::Receiver<crate::session_state::SessionState>,
        bool,
    )>,
    /// The bookmarks of files no tab holds marks for any more, on their
    /// saved lines: what the last tab holding them left when it closed
    /// (see [`Self::settle_bookmarks`]).
    closed_bookmarks: std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    /// The tasks backing the open Run Task picker; the picker row `id`
    /// indexes into this list.
    run_tasks: Vec<crate::tasks::Task>,
    /// The most recently run task, for "Tasks: Rerun Last Task".
    last_task: Option<crate::tasks::Task>,
    /// Set on startup (after `init_graphics` resolves the inline-image
    /// protocol) when the host terminal can't render croft's icons/images
    /// and the user hasn't silenced the nudge. Drawn as a dismissible modal
    /// recommending iTerm2 (macOS) / Ghostty (macOS/Linux).
    pending_terminal_warning: bool,
    /// True while the Source Control commit dropdown (▾ caret) is open.
    /// The caret button toggles this; clicking a menu row, the commit
    /// button, or anywhere outside the menu closes it.
    commit_menu_open: bool,
    /// Cached default-branch name (e.g. "main" / "master"), refreshed
    /// when the workspace root changes. Surfaced in the commit dropdown's
    /// "View Changes vs <name>" label so users know which branch the
    /// diff compares against. `None` until the resolver succeeds.
    default_branch_label: Option<String>,
    /// The resolver running off the UI thread for the repository at the
    /// path (#1973): it runs up to five `git` processes, and on macOS each
    /// goes through the Xcode shim.
    default_branch_ask: Option<(PathBuf, std::sync::mpsc::Receiver<Option<String>>)>,
    /// Last HEAD oid the git-gutter baselines were synced against. When the
    /// git worker reports a different oid (commit, checkout, pull), the cached
    /// per-tab HEAD snapshots are invalidated so the gutter refetches.
    /// True after the user has chosen "Always for this session" on the
    /// local-browser confirmation. Subsequent link clicks dispatch to
    /// the relay silently.
    trust_local_browser: bool,
    /// Width in cells of the sidebar (Explorer / Search / Remote pane).
    /// Defaults to 32 cells; user can drag the splitter between sidebar
    /// and editor to widen or narrow.
    sidebar_width: u16,
    /// Height in cells of the bottom terminal pane. `None` = use the
    /// default 35% split; Some = a user-specified pinned height. Stored
    /// in cells (not percent) so it doesn't drift on window resize.
    terminal_height: Option<u16>,
    /// Active splitter drag, if any. Cleared on mouse-up.
    splitter_drag: Option<SplitterDrag>,
    /// Last-rendered geometry of the vertical splitter column (between
    /// sidebar and editor) and the horizontal splitter row (between
    /// editor and terminal). Used by the mouse handler to hit-test cleanly
    /// without recomputing layout outside of `render`.
    sidebar_splitter_x: Option<u16>,
    terminal_splitter_y: Option<u16>,
    /// Every draggable seam between editor groups, as laid out by the most
    /// recent render. Empty when unsplit. Drives both the focus-on-click guard
    /// (a press on a seam must not steal focus) and the seam-drag hit-test, for
    /// a grid of any depth.
    editor_seams: Vec<editor_layout::Seam>,
    /// The whole editor area (every group) as the most recent render laid
    /// it out: what `leaf_rects` splits into the groups' rects. The active
    /// group's own `last_full_area` is only its share once the editor is
    /// split, so a geometric hit-test must start from this instead.
    editor_layout_area: Rect,
    /// Hit-test rectangles of the "[+]" buttons - one per terminal pane,
    /// indexed in lock-step with `terminals`. Empty when the pane is hidden
    /// or every pane is too narrow for the label.
    terminal_add_buttons: Vec<Rect>,
    /// Hit-test rectangles of the profile-caret (`⌄`) buttons - one per pane,
    /// in lock-step with `terminals`. Clicking one opens the profile picker.
    terminal_profile_buttons: Vec<Rect>,
    /// Hit-test rectangles of the "[-]" buttons - one per terminal pane,
    /// indexed in lock-step with `terminals`. Empty when only one terminal
    /// is open (closing the last one would leave the pane empty, which we
    /// explicitly forbid) or the pane is hidden.
    terminal_close_buttons: Vec<Rect>,
    /// Hit rect of the focus-mode (`⛶`) button at the right of the panel's
    /// tab row (#1980): it maximizes the ACTIVE terminal across the panel, or
    /// restores the split. One button for the panel rather than one per pane,
    /// because the mode rearranges every pane at once. Empty with fewer than
    /// two terminals, off the TERMINAL tab, or while the panel is hidden.
    terminal_focus_button: Rect,
    /// Hit rect of the panel's maximize / restore button (#1980), the click
    /// form of `Ctrl+Shift+J`: grow the panel over the editor, or give the
    /// editor its rows back. Rightmost on the panel's tab row, on every tab,
    /// since it sizes the whole panel. Empty while the panel is hidden.
    panel_maximize_button: Rect,
    /// Hit-test rectangles of the maximize-mode rail rows down the panel's
    /// right edge, in lock-step with `terminals`. Clicking one hands that
    /// terminal the maximized pane. Empty outside maximize mode.
    terminal_rail_rects: Vec<Rect>,
    /// The maximize rail's own column, so the wheel can scroll it. Empty
    /// whenever the rail is not on screen.
    terminal_rail_area: Rect,
    /// First terminal shown in the rail when there are more terminals than
    /// rows. Clamped during paint, so nothing else has to keep it honest.
    terminal_rail_scroll: usize,
    /// Which terminal the rail last scrolled into view, so the reveal fires
    /// when the active pane changes and never fights a deliberate wheel.
    terminal_rail_revealed: Option<usize>,
    /// Hit-test rectangles of the pane name pills at each pane's top-left, in
    /// lock-step with `terminals`. A press on one starts a drag-reorder;
    /// hidden or unlabeled panes hold an empty rect. Empty with one terminal.
    terminal_label_rects: Vec<Rect>,
    /// Hit-test rectangles of the collapse (`‹`) buttons, in lock-step with
    /// `terminals` (#313). A pane that is hidden, already collapsed, too
    /// narrow for the glyph, or maximized holds an empty rect.
    terminal_collapse_buttons: Vec<Rect>,
    /// Extensions whose programs the user has allowed to run (the MCP
    /// consent set, loaded once from prefs and kept current when a consent
    /// is granted here), so a viewer's gate is a set lookup rather than a
    /// prefs read per click, and a test can seed it without a prefs file.
    consented_extensions: std::collections::BTreeSet<String>,
    /// The config dir this app reads extensions and consent from. Carried
    /// rather than re-derived so a test can point one app at a scratch dir
    /// instead of mutating the process-wide environment, which this repo
    /// has twice seen take an unrelated test down.
    config_dir: PathBuf,
    /// The profile whose settings this croft started with (#1587), whose
    /// `config.json` under `config_dir` is the user layer the Settings
    /// editor reads and writes. `None` for the Default profile, and always
    /// under test, so a developer's own profile never steers the suite.
    settings_profile: Option<String>,
    /// Where "open scrollback in an editor tab" writes the pane's coloured
    /// scrollback so the rendered log view can index it from disk (#257).
    /// `~/.cache/croft/scrollback` normally; a test points it at a temp dir.
    scrollback_dir: PathBuf,
    /// Hit-test rectangles of the one-column strips collapsed panes leave
    /// behind, in lock-step with `terminals` (#313). Clicking anywhere down a
    /// strip gives that pane its width back; expanded panes hold an empty
    /// rect. The WHOLE strip is the target, not just the chevron: one column
    /// is already a small thing to hit without also demanding the right row.
    terminal_strip_rects: Vec<Rect>,
    /// Index of the terminal being drag-reordered (grabbed by its pane label
    /// or its maximize-rail row). The reorder is live: crossing another pane
    /// or rail row moves the terminal there immediately, and this follows it.
    terminal_drag_from: Option<usize>,
    /// Index of the editor tab being drag-reordered (#1565), set by a press
    /// on a tab and cleared on release. Live like `terminal_drag_from`:
    /// crossing another tab moves the dragged one there, and this follows.
    editor_tab_drag_from: Option<usize>,
    /// A left press that was FORWARDED to a pane because its child tracks
    /// the mouse (#474), live until the release. The release goes to the
    /// same pane, clamped to its grid if the pointer left it. What the drag
    /// in between does depends on what the child asked for: motion reports
    /// under 1002/1003, croft's own text selection anchored at the press
    /// cell under click-only 1000 (Claude Code's mode), since a child that
    /// never asked for motion cannot use the drag and the user still needs
    /// to copy text out of it. The pane is named by `uid`, so a pane closed
    /// or reordered mid-press is found again (or found gone) rather than
    /// confused with whichever pane now holds its old index. Every later
    /// event of the gesture -- motion, selection extension, release,
    /// copy-on-select -- is aimed at THAT pane, never at the active one:
    /// the keyboard can move the active pane while the button is held.
    ///
    /// Same hole as `terminal_drag_from`: a modal that takes the screen
    /// while the button is held swallows the mouse-up in `handle_mouse`'s
    /// overlay gates, so the latch stays set until the next release. Two
    /// consequences the older latch never had: the child keeps a button it
    /// believes is down until its next click, and the auto-scroll tick keeps
    /// standing down for a pane that is not active. Reaching it needs a
    /// keystroke with the button held.
    terminal_pointer_forwarded: Option<ForwardedPointer>,
    /// True while one terminal pane fills the panel's width and the others
    /// are listed in the right-side rail (the per-pane `⛶` button / Cmd+K M).
    /// Distinct from `terminal_maximized`, which grows the panel vertically
    /// over the editor. Only meaningful with 2+ terminals; closing down to
    /// one resets it.
    terminal_pane_maximized: bool,
    /// Which tab the bottom panel group is showing (TERMINAL or PROBLEMS).
    bottom_panel_tab: BottomPanelTab,
    /// The PROBLEMS view: aggregated workspace diagnostics, projected from
    /// `lsp_diagnostics`. Rendered into the panel content when its tab is active.
    problems: crate::widgets::problems::ProblemsPanel,
    /// The OUTPUT view: a read-only per-channel log viewer over the in-process
    /// `crate::output` bus. Rendered into the panel content when its tab active.
    output: crate::widgets::output::OutputPanel,
    /// The PORTS view: the registry of detected loopback ports, fed by the
    /// terminal output scrape and the periodic socket poll. Rendered into the
    /// panel content when its tab is active.
    ports: crate::widgets::ports::PortsPanel,
    /// The CAPTURES view: output lines collected by `capture` triggers
    /// (iTerm2's Capture Output); Enter / click jumps back to the line.
    pub captures: crate::widgets::captures::CapturesPanel,
    /// Transient click-only notification for a freshly detected browsable port,
    /// offering forward / open. `None` when nothing is pending.
    port_toast: Option<PortToast>,
    /// Last time the socket poll was kicked off, so it fires on a low cadence
    /// rather than every tick.
    last_port_poll: std::time::Instant,
    /// Socket-poll snapshots arrive here from a one-shot background thread (one
    /// complete listener set per cycle), so the blocking `lsof`/`ps` never runs
    /// on the event loop (keeps input at 0ms). A full set, not individual hits,
    /// so the app can reconcile ports that stopped listening.
    port_poll_rx: std::sync::mpsc::Receiver<PortPoll>,
    /// Results of a running fleet command (#363), delivered off the event
    /// loop. A fleet run waits on the network for up to `FLEET_TIMEOUT`, and
    /// blocking the loop for that would freeze the editor — the user could
    /// not scroll, type, or cancel while their fleet answered.
    fleet_rx: std::sync::mpsc::Receiver<Vec<crate::fleet::HostResult>>,
    fleet_tx: std::sync::mpsc::Sender<Vec<crate::fleet::HostResult>>,
    /// True while a run is in flight, so a second invocation says so rather
    /// than starting a competing fleet against the same hosts.
    fleet_running: bool,
    /// The command the fleet run in flight is running, for its tab (#363).
    fleet_command: String,
    port_poll_tx: std::sync::mpsc::Sender<PortPoll>,
    /// True while a poll thread is in flight, so cadence ticks don't pile up
    /// overlapping `lsof` invocations.
    port_poll_inflight: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Auto-label refresh for terminal panes: `(shell_pid, process_name,
    /// script)` resolved off the event loop, applied to the pane with that
    /// shell pid. `script` is what a script runtime (`node`) runs, for agent
    /// recognition (#1380); `None` for anything else.
    last_label_refresh: std::time::Instant,
    label_rx: std::sync::mpsc::Receiver<Vec<(i32, String, Option<String>)>>,
    label_tx: std::sync::mpsc::Sender<Vec<(i32, String, Option<String>)>>,
    label_inflight: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Hit-test rect of the "PROBLEMS" tab in the panel tab strip. Empty when
    /// the panel is hidden.
    problems_tab_rect: Rect,
    /// Hit-test rect of the "OUTPUT" tab in the panel tab strip. Empty when the
    /// panel is hidden.
    output_tab_rect: Rect,
    /// Hit-test rect of the "PORTS" tab in the panel tab strip. Empty when the
    /// panel is hidden.
    ports_tab_rect: Rect,
    /// Hit-test rect of the "CAPTURES" tab in the panel tab strip.
    captures_tab_rect: Rect,
    /// Hit-test rect of the "TERMINAL" tab in the panel tab strip. Empty when
    /// the panel is hidden.
    terminal_tab_rect: Rect,
    /// Last pointer cell reported by a `Moved` event. Render-time hover
    /// hit-testing (the Black theme's teal pill behind the terminal `-`/`+`
    /// buttons) reads this instead of tracking enter/leave transitions.
    pointer_cell: Option<(u16, u16)>,
    /// Which activity-bar icon the pointer is currently over, if any. Drives
    /// the hover variant of the icon (brightened glyph, no selection pill).
    /// Changing it arms `overlays.activity.mark_dirty()` so the swapped
    /// pre-baked image is re-emitted; it never triggers a re-bake.
    hovered_activity_icon: Option<ActivityIcon>,
    /// Which layout toolbar icon the pointer is over (0 = side bar, 1 = panel,
    /// 2 = customize), so a hover change can re-emit the brighter image variant
    /// like [`Self::hovered_activity_icon`] does for the activity bar.
    hovered_layout_icon: Option<usize>,
    /// On-screen keyboard for touch-only environments (Termux). `Some`
    /// while the bottom keyboard band is visible; taps on its keys
    /// synthesize `KeyEvent`s through `handle_key`, so they reach the
    /// editor, terminal PTY, and modals exactly like hardware keystrokes.
    osk: Option<crate::widgets::osk::Osk>,
    /// True when taps in editable areas (editor text, terminal panes, the
    /// Search input) auto-raise the on-screen keyboard. Armed on Termux,
    /// where active mouse tracking permanently suppresses the native soft
    /// keyboard; `CROFT_FORCE_OSK=1` arms it elsewhere for testing.
    osk_auto: bool,
    /// Total width / height of the right-hand content area, captured on
    /// every render so a splitter drag can clamp to the live viewport.
    last_content_width: u16,
    last_content_height: u16,
    /// Optional LSP orchestrator. None when LspManager::new failed at
    /// startup (e.g. the background tokio runtime couldn't spawn); the
    /// rest of the editor stays fully functional without it.
    pub lsp: Option<crate::lsp::LspManager>,
    /// Per-path snapshot of the last edit_seq we forwarded to the LSP
    /// manager. sync_lsp diffs every tick against the current tabs to
    /// emit did_open / did_change / did_close.
    lsp_last_seen: std::collections::HashMap<PathBuf, u64>,
    /// Whether this session already looked for shellcheck when a shell
    /// script was opened (#1511), so its notice is shown at most once.
    shell_lint_notice_shown: bool,
    /// The PATH the helper tools servers shell out to are looked up in
    /// (#1511): croft's own, which its servers inherit.
    tool_search_path: Option<std::ffi::OsString>,
    /// Newest semantic-token request generation already applied per path.
    /// `seq` alone is not enough: two full requests can be in flight at the
    /// same seq (`sync_lsp` plus a server-driven refresh), and the request
    /// retry loop emits an EMPTY batch once its backoff is exhausted, so a
    /// late empty loser would otherwise blank a good overlay and be written
    /// to the content-keyed disk cache.
    semantic_generation_seen: std::collections::HashMap<PathBuf, u64>,
    /// Diagnostics store: per file, the latest set from each server. Servers
    /// push complete sets via `textDocument/publishDiagnostics`, so the inner
    /// map is keyed by server name (ty + ruff can both publish for one file)
    /// and a server's entry is replaced wholesale on each batch (an empty
    /// batch removes it). The merged value for the visible file is handed to
    /// the editor; the store outlives tab switches so re-focusing a file shows
    /// its squiggles again without waiting for the next server push.
    /// Open-buffer paths as of the last PROBLEMS rebuild. Open Files scope
    /// filters on this set, so the panel must refresh when the set changes —
    /// not only when diagnostics do, or opening a file with stored
    /// diagnostics would show nothing until unrelated churn happened by.
    problems_open_set: std::collections::BTreeSet<PathBuf>,
    lsp_diagnostics: std::collections::HashMap<
        PathBuf,
        std::collections::HashMap<String, Vec<crate::lsp::manager::Diagnostic>>,
    >,
    /// Markdown-lint's own edit-seq cursor, mirroring `lsp_last_seen` but
    /// independent of the LSP: `.md` files get linted whether or not any
    /// language server is running (there is none for Markdown), so this
    /// cannot share the LSP's cursor map. Keyed by path; a path's entry is
    /// removed when its tab closes so a later re-open re-lints instead of
    /// trusting a stale seq (a different file could reuse a `PathBuf` after
    /// a rename-in-place on disk). Each entry holds the lines that were
    /// linted as well as their seq, because two split panes count edits
    /// separately and can share a seq while holding different text.
    markdown_lint_last_seen: std::collections::HashMap<PathBuf, (u64, Vec<String>)>,
    /// Live Run (`crate::live_run`): the files armed to run as they are
    /// edited. Per file, never global, so no script runs unasked.
    live_run_files: std::collections::HashSet<PathBuf>,
    /// The buffer text last sent per armed file, so an unchanged buffer is
    /// not re-run on every tick.
    live_run_sent: std::collections::HashMap<PathBuf, Vec<String>>,
    /// The background runner, spawned on the first toggle.
    live_run_runner: Option<crate::live_run::Runner>,
    /// The last position [`App::sync_markdown_scroll`] pushed across split
    /// panes: (path, whether the preview led, source line), so a still
    /// viewport costs nothing per tick.
    md_scroll_synced: Option<(PathBuf, bool, usize)>,
    /// Code lenses on or off (#608, `Editor: Toggle CodeLens`), and the
    /// edit seq last requested per path, so a still buffer asks once.
    code_lens_enabled: bool,
    code_lens_requested: std::collections::HashMap<PathBuf, u64>,
    /// Function breakpoints (#611), handed to each session as it launches.
    function_breakpoints: Vec<String>,
    /// Screen reader mode (#621), its speech command, the announcer, and
    /// where the status bar drew the announcement this frame (the cursor's
    /// resting place when no text has focus).
    screen_reader: bool,
    screen_reader_command: Option<String>,
    announcer: crate::a11y::Announcer,
    announce_pos: Option<(u16, u16)>,
    /// The latest event screen reader mode reads out whatever has focus
    /// (#1547), numbered so a repeat is still read.
    a11y_event: Option<(u64, String)>,
    /// Why the focused debug session last stopped, until the stop's
    /// location is known and the pause can be announced (#1547).
    debug_pause_reason: Option<String>,
    /// Terminal command suggestions (#614): on or off, and the one painted
    /// this frame as (pane index, the text Right arrow would type).
    term_suggest_enabled: bool,
    term_suggestion: Option<(usize, String, String)>,
    /// The command whose new shortcut the next keystroke records (#612).
    recording_shortcut: Option<(crate::widgets::command_palette::Command, std::time::Instant)>,
    /// A recorded chord another command already has (#1482), waiting for
    /// Enter to reassign it: the command, the chord, and every command it
    /// runs now (a built-in chord can be shared by several).
    confirming_shortcut: Option<(
        crate::widgets::command_palette::Command,
        KeyEvent,
        Vec<crate::widgets::command_palette::Command>,
    )>,
    /// Inline AI suggestions (#607): off until the user turns them on, since
    /// they send code to a model. The worker, the backend it was started
    /// for, the request in flight (id plus what it answers for), and the
    /// last request's key so a still caret asks once.
    inline_enabled: bool,
    inline_worker: Option<crate::inline_complete::Worker>,
    inline_config: Option<crate::inline_complete::Config>,
    inline_job: Option<(u64, PathBuf, u64, usize, usize)>,
    /// The language server's inline completion in flight, keyed like
    /// `inline_job`: a server that offers them answers for its files.
    inline_lsp: Option<(u64, PathBuf, u64, usize, usize)>,
    inline_asked: Option<(PathBuf, u64, usize, usize)>,
    inline_next_id: u64,
    /// Where recorded shortcuts are written: the user's keybindings.json,
    /// or under test a scratch file, so a test run never edits the real one.
    keybindings_file: PathBuf,
    /// A Search Editor rerun in flight (#615): the tab it lands in, the
    /// header it ran, and where its hits arrive.
    search_editor_job: Option<(
        PathBuf,
        crate::search_editor::Header,
        std::sync::mpsc::Receiver<Vec<crate::widgets::search::SearchHit>>,
    )>,
    /// The open SARIF logs (path, disk stamp) the published diagnostics were
    /// built from (#577); a different set means publish again.
    sarif_diag_signature: Vec<(PathBuf, Option<(std::time::SystemTime, u64)>)>,
    /// When the SARIF viewers' added logs were last checked for changes.
    sarif_added_polled: Option<std::time::Instant>,
    /// The selection and open files the SARIF step marks were drawn for.
    sarif_step_signature: Option<SarifStepSignature>,
    /// Every (file, source key) SARIF diagnostics were published under, so a
    /// closed log's results can be withdrawn exactly.
    sarif_published: Vec<(PathBuf, String)>,
    /// Live LSP work-done progress, keyed by server name (e.g. "rust-analyzer"
    /// -> "Indexing 112/340 33%"). An entry exists only while that server has
    /// an active task; the status bar surfaces it so a busy-priming server is
    /// never mistaken for a dead one. Cleared on the server's `End`.
    lsp_progress: std::collections::HashMap<String, String>,
    /// Language-server questions (`window/showMessageRequest`, #1599)
    /// waiting for the list picker, oldest first, and the one it shows.
    /// Dropping one answers the server "no action".
    lsp_questions: std::collections::VecDeque<LspQuestion>,
    lsp_question: Option<LspQuestion>,
    /// Active completion popup, when a completion request has been issued
    /// and the user hasn't dismissed the result. Populated by drain_lsp_completion
    /// once the worker sends a CompletionResult back.
    pub completion_popup: Option<crate::widgets::completion_popup::CompletionPopup>,
    /// Most recent completion request id; later responses with a stale
    /// id are dropped so a slow earlier response cannot clobber a fresh
    /// one (e.g. user already moved past the original trigger).
    completion_request_id: Option<u64>,
    /// Where the caret was (row, char column) when the pending completion was
    /// requested: the server's edit ranges describe the line then, and the
    /// accept shifts them by what was typed or deleted since.
    completion_origin: Option<(usize, usize)>,
    /// The caret position the latest completion request was sent at. Unlike
    /// `completion_origin` (the session's anchor, kept across re-asks so the
    /// reply still opens in the word) it moves with every re-ask, because a
    /// server's ranges describe the line at that very point.
    completion_ask_pos: Option<(usize, usize)>,
    /// The request position of the reply the open popup's items came from:
    /// what an accepted item's ranges are shifted from.
    completion_items_pos: Option<(usize, usize)>,
    /// The pending completion's last reply was marked `isIncomplete` (#1529):
    /// each word edit re-asks the server, whether or not any of its old items
    /// are still on screen. Lives only while `completion_request_id` does.
    completion_incomplete: bool,
    /// A word edit in an incomplete session is waiting to re-ask the server;
    /// sent after `sync_lsp` so the request follows the edit's `didChange`.
    completion_reask_due: bool,
    /// A typed key that opens completion (#929) in the file it was typed
    /// in, waiting like the re-ask: a trigger character or a word begun.
    completion_ask_due: Option<(CompletionTrigger, PathBuf)>,
    /// The pending completion was asked for, not opened by typing (#929):
    /// only then does a reply with nothing matching the word say so.
    completion_explicit: bool,
    /// `disable_quick_suggestions` unset: a word begun opens completion.
    quick_suggestions: bool,
    /// The pending completion was asked for (Ctrl+Space), so the words of
    /// the open documents fill in when no server offers anything (#1566).
    /// A `.` typed asks the server alone: a word list there would open on
    /// every full stop of a plain-text note.
    completion_words: bool,
    /// Ids for completions answered here rather than by a server (#1566),
    /// counting down from the top so they never meet the manager's.
    local_completion_seq: u64,
    /// The last accepted completion, while its server resolves the
    /// auto-import it left out of the list (#928).
    completion_resolve: Option<PendingCompletionResolve>,
    /// `word_based_suggestions`: which open documents word completion
    /// draws on (#1566).
    pub word_suggestions: crate::word_suggest::WordSuggestions,
    /// Signature Help (parameter hints) popup, anchored to the caret while the
    /// user is inside a call. Populated by `drain_lsp_signature_help`.
    pub signature_help_popup: Option<crate::widgets::signature_help_popup::SignatureHelpPopup>,
    signature_help_request_id: Option<u64>,
    /// (cursor_row, cursor_col, edit_seq) at the last signatureHelp request, so
    /// the tick can re-query when the caret moves or the buffer changes while
    /// the popup is open and close it once the caret leaves the call.
    signature_help_anchor: Option<(usize, usize, u64)>,
    hover: HoverDwell,
    hover_popup: Option<crate::widgets::hover_popup::HoverPopup>,
    hover_request_id: Option<u64>,
    /// The rust-analyzer request a `Rust:` command is waiting on (#1626);
    /// a reply to any other id is stale and dropped.
    rust_analyzer_request: Option<(u64, crate::lsp::rust_analyzer::RaRequest)>,
    hover_anchor: Option<(u16, u16)>,
    hover_word: Option<(usize, usize, usize)>,
    /// Diagnostic block for the point a hover is anchored to, captured
    /// synchronously when the dwell fires (server diagnostics already live in
    /// the editor). Stacked above the async LSP hover reply by `compose_hover`
    /// so a squiggle's message shows even when the server returns no type info.
    hover_diagnostic: Option<String>,
    /// Set when the keyboard asked for the hover at the caret (#1518), not
    /// the pointer's rest: its reply is checked against the caret, and says
    /// there is no hover only over the status line this holds, the one shown
    /// at the request, so a newer message is not replaced.
    hover_at_caret: Option<String>,
    /// The tab and edit count the hover was asked about: a reply after
    /// either changed (a paste over the word keeps its span) is dropped.
    hover_doc: (Option<PathBuf>, u64),
    /// Separate dwell timer for the editor tab strip. Hovering a tab for
    /// `HOVER_DELAY` surfaces `tab_tooltip` with the tab's full path so
    /// same-named files can be told apart. Kept distinct from `hover`
    /// (which drives LSP word hover in the editor body) so the two never
    /// fight over the same timer.
    tab_hover: HoverDwell,
    tab_tooltip: Option<crate::widgets::hover_popup::HoverPopup>,
    tab_hover_idx: Option<usize>,
    /// Shared dwell + popup for chrome button hints (search-panel actions and
    /// toggles, activity-bar icons, …). One timer for all chrome controls,
    /// keyed on the label so the dwell only restarts when the pointer crosses
    /// to a different control; extending coverage to a new surface is one arm
    /// in `ui_tooltip_at`.
    ui_hover: HoverDwell,
    ui_tooltip: Option<crate::widgets::hover_popup::HoverPopup>,
    ui_tooltip_label: Option<std::borrow::Cow<'static, str>>,
    /// The Search list's `(scroll, hits, shown)` when the hint was last read
    /// against it: [`Self::reread_ui_tooltip`] reads it again on a change.
    search_hint_view: (usize, usize, bool),
    /// The chrome tooltip's final on-screen rect from the most recent render
    /// (after the activity-bar shift / viewport clamp), or `None` when no
    /// tooltip is showing. Read by the post-draw flush so the box can be
    /// re-painted on top of any sidebar illustration it overlaps, since
    /// OSC-1337 images composite above ratatui's text layer.
    ui_tooltip_area: Option<Rect>,
    /// Where the update card sat on the last frame, `None` while it is
    /// folded, gone or too big for the screen. The activity bar's gear and
    /// heart and the side bar's illustrations are inline images, which land
    /// above text, so they hold back wherever the card is (#1958).
    update_card_rect: Option<Rect>,
    definition_request_id: Option<u64>,
    /// In-flight Peek Definition request (#115): same LSP request as the
    /// jump, different consumer — the response opens the peek popup at the
    /// caret instead of moving it.
    peek_definition_request_id: Option<u64>,
    /// The open peek popup, rendered beside `hover_popup` against the whole
    /// frame; `peek_target` is where Enter jumps.
    peek_popup: Option<crate::widgets::hover_popup::HoverPopup>,
    peek_target: Option<(PathBuf, u32, u32)>,
    /// Peek References (#616): every reference and the one the popup shows.
    /// Up / Down step through them; any close of the popup clears this.
    peek_refs: Option<(Vec<PeekLocation>, usize)>,
    /// Set when the in-flight references request came from Peek References,
    /// so its reply opens the popup instead of the picker.
    references_want_peek: bool,
    declaration_request_id: Option<u64>,
    type_definition_request_id: Option<u64>,
    /// The `(active file, last-seen LSP edit seq)` the OUTLINE was last synced
    /// to, so `sync_outline` re-requests symbols exactly once per tab switch or
    /// edit batch. `None` seq means the file has no language server.
    outline_synced: Option<(PathBuf, Option<u64>)>,
    /// A large buffer's syntax outline being parsed off the UI thread
    /// (#1150): the `outline_synced` key its snapshot was taken at, and
    /// where the symbols land. One at a time; typing meanwhile coalesces
    /// into one fresh parse when it lands.
    outline_parse: Option<(OutlineKey, OutlineParse)>,
    /// The `outline_synced` key the last off-thread parse was started for.
    outline_parsed_for: Option<(PathBuf, Option<u64>)>,
    /// The file and edit seq whose language-server outline last landed: a
    /// syntax parse of that same buffer finishing later must not replace
    /// the richer symbols.
    outline_lsp_landed: Option<(PathBuf, u64)>,
    implementation_request_id: Option<u64>,
    references_request_id: Option<u64>,
    call_hierarchy_request_id: Option<u64>,
    /// In-flight documentHighlight request, with the (path, row, col,
    /// edit_seq) it was fired for so a reply for a caret or buffer state the
    /// editor has since left is dropped.
    occurrences_request: Option<(u64, PathBuf, usize, usize, u64)>,
    /// In-flight linkedEditingRange request (#254): (id, path, edit_seq).
    linked_request: Option<(u64, PathBuf, u64)>,
    /// Last observed `(path, row, col, seq)` for the linked-editing
    /// debounce, and when it settled — occurrences' idle pattern.
    linked_observed: Option<(PathBuf, usize, usize, u64)>,
    linked_observed_at: std::time::Instant,
    /// A request already went out for `linked_observed` (#1299). Kept
    /// apart from `linked_request`, which the reply clears: a `null`
    /// answer left nothing to say the position was asked, so every UI
    /// wake asked again while the caret stayed put.
    linked_asked: bool,
    /// In-flight Expand Selection request (#254). Extra Shift+Alt+Right
    /// presses while the chains are in flight stack up and apply
    /// together on drain; an `unsupported` verdict resolves them via
    /// tree-sitter instead. The reply is gated on id + path + edit_seq
    /// AND the exact cursor positions the request was fired for — a
    /// caret move without an edit leaves `edit_seq` unchanged, and a
    /// chain computed for the old caret must not apply at the new one.
    selection_range_request: Option<PendingSelectionRanges>,
    /// The caret state `tick_occurrences` last observed, and when it changed.
    /// A caret resting on one spot for `OCCURRENCES_IDLE` fires one request.
    occ_observed: Option<(PathBuf, usize, usize, u64)>,
    occ_observed_at: std::time::Instant,
    rename_request_id: Option<u64>,
    /// The open buffers' `edit_seq`s when the in-flight rename went out
    /// (#1151): a reply for a buffer edited since is refused.
    rename_baseline: Vec<(PathBuf, u64)>,
    /// An Explorer rename or move waiting on the servers' `willRenameFiles`
    /// edit (#610); it runs when the answer lands or its deadline passes.
    pending_file_move: Option<PendingFileMove>,
    /// In-flight prepareRename (#254): (id, path, row, col, edit_seq) —
    /// the deferred rename prompt's context.
    prepare_rename_request: Option<(u64, PathBuf, usize, usize, u64)>,
    format_request_id: Option<u64>,
    /// True when the in-flight format request is Format Selection (#254)
    /// — the drain words its statuses accordingly.
    format_request_selection: bool,
    /// The `edit_seq` of the tab the in-flight format request was computed
    /// against (#1127): a reply for a buffer typed into since is dropped.
    format_request_seq: Option<u64>,
    /// VS Code's `editor.formatOnType` (#254): typing a server trigger
    /// character runs onTypeFormatting at the spot. Off by default.
    format_on_type: bool,
    /// In-flight on-type request: (id, the edit_seq it was computed
    /// against, the tab's path). The reply applies only while that same
    /// tab is active and its buffer has not moved since — `edit_seq` is
    /// per-tab, so the seq alone cannot tell tabs apart.
    on_type_request: Option<(u64, u64, PathBuf)>,
    code_action_request_id: Option<u64>,
    /// True when the in-flight code-action request is a `codeAction/resolve`
    /// (the reply is one resolved action to apply directly) rather than the
    /// initial `textDocument/codeAction` (whose reply opens the picker).
    code_action_pending_resolve: bool,
    /// The open buffers' `edit_seq`s when the code-action (or resolve)
    /// request whose edits are pending went out (#1151).
    code_action_baseline: Vec<(PathBuf, u64)>,
    /// The actions from the in-flight `textDocument/codeAction` reply, indexed
    /// by the row `id` the Quick Fix [`ListPicker`] presents.
    pending_code_actions: Vec<crate::lsp::manager::CodeActionItem>,
    /// Review threads loaded onto a file, as `(the file, its threads)` (#366).
    ///
    /// A SEPARATE field from `editor.comment_boxes`, which the render loop
    /// rebuilds from `navigator_notes` on every frame — assigning review
    /// threads there directly meant they were wiped before the user saw
    /// one, while the status line still reported having loaded them.
    ///
    /// Keyed by PATH because the merge runs on every frame: keyed only by
    /// "whatever was loaded last", one file's objections would hang off the
    /// next file's line numbers, against unrelated code. That is the harm
    /// `threads_are_filtered_to_the_file_being_viewed` prevents on the
    /// filter axis, and it re-enters through the lifetime axis if the boxes
    /// outlive the buffer they describe.
    ///
    /// Holds `Thread`s rather than finished `CommentBox`es so the geometry
    /// is derived against the LIVE buffer each frame. `box_line` clamps a
    /// box past the end back into view; computed once at load, that clamp
    /// goes stale the moment the user edits, and the box it was protecting
    /// becomes invisible again.
    review_boxes: Option<(PathBuf, Vec<crate::review_threads::Thread>)>,
    /// Writing reviews (#366): comments not yet submitted and the PR they
    /// were written on, the PR last looked up as (repo root, number), each
    /// thread's GraphQL node id, the chosen verdict while its summary is
    /// typed, the `gh` to run, and where finished jobs report.
    review_pending: Vec<crate::review_threads::PendingComment>,
    review_pending_pr: Option<(PathBuf, String)>,
    /// A Submit Review or export is in flight; another is refused until it
    /// reports, so the same comments are never posted twice.
    review_submitting: bool,
    /// Sticky notes (#367): every note in the workspace, where the owner
    /// keeps them (`None` under test), and which box id is which note.
    notes: crate::sticky_notes::Notes,
    notes_path: Option<PathBuf>,
    note_box_ids: std::collections::HashMap<u64, String>,
    notes_shown_gen: u64,
    /// Export to a PR (#368): the previewed comments and the navigator
    /// notes they came from, the sticky notes it would settle, and the
    /// prefix that marks the navigator's comments as AI-authored.
    review_export: Option<(
        Vec<crate::review_threads::PendingComment>,
        Vec<u64>,
        Vec<crate::widgets::list_picker::ListRow>,
    )>,
    review_export_sticky: Vec<String>,
    /// The PR the export preview named: posting goes there, even if a
    /// thread load answered for another PR in between.
    review_export_pr: Option<(PathBuf, String)>,
    review_ai_prefix: String,
    review_pr: Option<(PathBuf, String)>,
    /// The branch checked out when `review_pr` was recorded: the number is
    /// reused only while it is still the branch, or a `gh pr checkout` of
    /// another PR sent comments to the old one.
    review_pr_branch: Option<String>,
    review_nodes: std::collections::HashMap<u64, String>,
    review_verdict: Option<crate::review_threads::ReviewEvent>,
    review_gh: String,
    /// The review tab's `gh` call in flight (#365): what it is for, and
    /// where its answer arrives.
    pr_gh: Option<(PrGhJob, std::sync::mpsc::Receiver<Result<String, String>>)>,
    /// When the open review tab's checks were last re-read (#365).
    pr_checks_polled: Option<std::time::Instant>,
    /// GitHub's viewed state being read for the review tab keyed by the
    /// first half (#365), and [`Self::pr_viewed_seq`] when the read began.
    pr_viewed_fetch: Option<(String, u64, PrViewedRx)>,
    /// Viewed marks waiting to reach GitHub, sent one at a time so two
    /// quick toggles of one file arrive in order (#365).
    pr_viewed_queue: std::collections::VecDeque<PrViewedWrite>,
    /// The viewed mutation in flight: its tab's key and file, and gh's
    /// answer.
    pr_viewed_write: Option<(String, String, PrViewedWriteRx)>,
    /// A counter stamped on every viewed write when it is queued and when
    /// it finishes (#365).
    pr_viewed_seq: u64,
    /// Each `(tab key, file)`'s latest stamp from [`Self::pr_viewed_seq`].
    /// A viewed-state read that began before a file's write settled may
    /// have seen GitHub before the write landed, so its answer does not
    /// overwrite that file. Kept on the app, not the review view, so a
    /// refresh that replaces the view keeps the protection.
    pr_viewed_touched: std::collections::HashMap<(String, String), u64>,
    /// The pull request head review mode checked out (#365), removed again
    /// when review is left with nothing in it worth keeping.
    pr_checkout: Option<PrCheckout>,
    /// A checkout still fetching: the PR number, and where the worktree
    /// path and commit (or git's refusal) arrive.
    pr_checkout_rx: Option<(u64, PrCheckoutRx)>,
    review_tx: std::sync::mpsc::Sender<crate::review_ops::Outcome>,
    review_rx: std::sync::mpsc::Receiver<crate::review_ops::Outcome>,
    /// An open asciicast recording (#356): the writer, the file it appends
    /// to, and when it started. `None` when nothing is being recorded.
    recording: Option<(
        crate::asciicast::Recorder,
        std::fs::File,
        std::time::Instant,
    )>,
    /// The terminal size the open recording last wrote (#356). A resize is
    /// an `r` event, and a player that never receives one keeps rendering
    /// the header's size — so a session resized mid-recording plays back
    /// with every later frame wrapped at the wrong width.
    recorded_size: (u16, u16),
    /// The open history scrubber (#371), or `None` when the editor is
    /// showing the live buffer. `Some` at `Position::Working` is a real
    /// state: the scrubber is open and parked at the user's own edits.
    scrubber: Option<crate::scrubber::Scrubber>,
    /// The active file as it was at the scrubber's commit (#371): a separate
    /// read-only editor painted in place of the live one, which is never
    /// touched. `None` at the working tree.
    pub scrub_view: Option<crate::widgets::editor::Editor>,
    /// The file `scrub_view` was built for. Switching tabs while scrubbing
    /// rebuilds the view for the new file rather than showing the old one's
    /// history over it.
    scrub_for: Option<PathBuf>,
    /// The file and commit the OUTLINE currently shows while scrubbing
    /// (#371), so it is recomputed once per step, and so leaving the
    /// scrubber knows to bring the live file's outline back.
    outline_scrub_key: Option<(PathBuf, String)>,
    /// `a` in the approval popup (#347): the agent whose edits are
    /// approved without asking, until when.
    auto_approve: Option<(String, std::time::Instant)>,
    /// The scrubber slider's track, from the last frame, and whether a
    /// drag that started on it is under way (#371).
    scrub_slider: Rect,
    scrub_dragging: bool,
    /// File text per (commit, workspace-relative path) for the scrubber;
    /// `None` records that the file did not exist there.
    scrub_cache: std::collections::HashMap<(String, String), Option<std::sync::Arc<str>>>,
    /// Each scrubbed commit's tree under the workspace root, for the
    /// Explorer's dimming (#371); `None` records that the listing failed.
    scrub_trees: std::collections::HashMap<String, Option<std::sync::Arc<crate::git::CommitTree>>>,
    /// Per workspace-relative path, the name the file had at each commit
    /// the scrubber walks, so history before a rename reads the old name
    /// (#1304). One `--follow` walk per file per scrub.
    scrub_names: std::collections::HashMap<
        String,
        std::sync::Arc<std::collections::HashMap<String, String>>,
    >,
    /// Builds highlighted historical views off the UI thread (#371),
    /// started on the first step.
    scrub_builder: Option<crate::scrubber::ViewBuilder>,
    /// Finished views near the scrubber's cursor, most recently used last,
    /// at most `SCRUB_VIEWS_KEPT`.
    scrub_views: crate::scrubber::KeptViews,
    /// Plain stand-ins near the cursor, ready for the next step, likewise.
    scrub_plain: crate::scrubber::KeptViews,
    /// Each built view's outline, parsed with it off the UI thread, for the
    /// OUTLINE to show while scrubbing. Dropped with the kept views.
    scrub_outlines: std::collections::HashMap<
        crate::scrubber::ViewKey,
        Vec<crate::lsp::manager::OutlineSymbol>,
    >,
    /// What `scrub_view` shows, and whether it is the finished view rather
    /// than the plain stand-in awaiting one.
    scrub_view_key: Option<(crate::scrubber::ViewKey, bool)>,
    /// The running `croft demo` tour (#377), with its scratch project and
    /// the workspace to return to.
    pub tour: Option<TourRun>,
    /// The tour was finished or skipped once (#377), so the welcome panel
    /// no longer offers it.
    pub tour_done: bool,
    /// The welcome panel's Take the Tour button, from the last frame.
    pub welcome_tour_button: Rect,
    /// The `gh` executable GitHub features run; tests point it at a stand-in.
    gh_program: PathBuf,
    /// The SARIF result a code scanning dismissal is for (#577): its log's
    /// path and its run and result indices, marked suppressed on success.
    dismiss_target: Option<(PathBuf, usize, usize)>,
    /// The SARIF location a Locate prompt is open for (#577): its URI and
    /// the 1-based line and column to land on.
    sarif_locate_pending: Option<(String, i64, i64)>,
    /// A SARIF source download in flight (#577): the location's URI, its
    /// 1-based line and column, and the body or why it failed.
    sarif_download: Option<std::sync::mpsc::Receiver<SarifDownload>>,
    /// Fetches a URL's body; a field so tests can stand in for the network.
    sarif_fetch: fn(&str) -> Result<Vec<u8>, String>,
    /// A breakpoint croft set itself at the assertion the last run failed
    /// on (#373), as `(file, 1-based line)`. It is NOT one of the user's
    /// breakpoints: it is added to the launch set, rendered hollow-red like
    /// any unverified one, and removed when the session ends — a debugger
    /// that silently left breakpoints behind would be worse than one that
    /// set none.
    debug_temp_breakpoint: Option<(PathBuf, usize)>,
    /// Human-readable note about that breakpoint, folded into whichever
    /// launch message the user actually ends up seeing.
    debug_temp_note: Option<String>,
    /// Per-agent ledger of files changed while an agent was working (#345),
    /// and the review baselines that decide which of them still need a
    /// look.
    pub agent_ledger: crate::agent_lane::AgentLedger,
    /// Where the ledger is saved per workspace (#345); `None` keeps it in
    /// memory only (tests, unless one sets it).
    agent_ledger_store: Option<PathBuf>,
    /// The ledger generation last saved, and when.
    agent_ledger_saved: (u64, std::time::Instant),
    /// The (agent, file) behind each row of the lane-file picker.
    pending_lane_picks: Vec<(String, PathBuf)>,
    /// The tab and line behind each row of Bookmarks: View Bookmarks.
    pending_bookmarks: Vec<BookmarkTarget>,
    /// When the running Fix with Navigator turn started (#374): the clock
    /// its PROBLEMS-row spinner runs on, and the flag that keeps the main
    /// loop repainting while it runs. `None` when no fix is streaming.
    problems_fix_started: Option<std::time::Instant>,
    /// The in-flight `.http` request's channel (#370). One at a time: the
    /// worker owns the socket, the drain owns the tab, and a second send
    /// while one runs is refused rather than queued.
    http_run: Option<std::sync::mpsc::Receiver<HttpRunOutcome>>,
    /// The `.http` env files' named environment whose values requests use
    /// (#924), picked with HTTP: Switch Environment; `None` uses only
    /// `$shared` and flat values, as REST Client's "No Environment" does.
    http_environment: Option<String>,
    /// The presentation edits behind the open Change Color Presentation
    /// picker (#254), indexed by row id; cleared after a pick.
    pending_color_presentations: Vec<(String, Vec<crate::widgets::editor::TextSpanEdit>)>,
    /// The (path, edit_seq) the pending presentations were computed
    /// against; a pick is refused when either moved while the picker was
    /// open (an FS-sync reload bumps the seq without user edits).
    pending_color_context: Option<(PathBuf, u64)>,
    /// In-flight colorPresentation request: (id, path, edit_seq).
    color_presentations_request: Option<(u64, PathBuf, u64)>,
    nav: NavHistory,
    editor_vim_chord: EditorVimChord,
    /// `Cmd+K` leader: armed when the user presses the prefix, holding the
    /// instant it started so the next key within `CMD_K_LEADER_TIMEOUT`
    /// completes a `Cmd+K`-prefixed chord (VS Code's two-key model). `None`
    /// when no chord is in flight.
    cmd_k_leader: Option<std::time::Instant>,
    /// Whether croft asked the terminal for the kitty keyboard protocol at
    /// startup (what `croft keys` reports as `kitty=`). A terminal that
    /// honours it reports `Shift` held with `Ctrl` explicitly, so
    /// `Ctrl+Shift+S` can be told from `Ctrl+S` (#857); one that ignores it
    /// sends the same control byte for both, never a `Shift`. False until
    /// `run` asks.
    kitty_keys: bool,
    /// The terminal answered the startup probe but not its kitty keyboard
    /// query, so Ctrl+\, Ctrl+] and Ctrl+/ arrive as the bytes crossterm
    /// reads as Ctrl+4, Ctrl+5 and Ctrl+7 (#1007): see `legacy_ctrl_punct`.
    legacy_ctrl_punct: bool,
    /// Native modal (vim-style) editing for the editor pane. When enabled it
    /// supersedes the always-on `editor_vim_chord` convenience layer.
    vim: crate::vim::VimState,
    /// Last `f`/`t`/`F`/`T` target, replayed by `;` and `,`.
    vim_last_find: Option<(crate::vim::FindKind, char)>,
    /// Whether the active visual selection is linewise (`V`), so the pending
    /// visual operator deletes/yanks whole lines.
    vim_visual_line: bool,
    shortcuts_modal: Option<crate::widgets::shortcuts::ShortcutsModal>,
    /// The Settings editor (#612).
    settings_editor: Option<crate::widgets::settings_editor::SettingsEditor>,
    shortcuts_hit_rect: Option<Rect>,
    /// Status-bar clickable-segment hit rects, recorded each render. Empty when
    /// the bar is hidden or the segment doesn't fit. Diagnostics → PROBLEMS;
    /// encoding / EOL / language → their respective "change" pickers.
    status_diag_rect: Rect,
    /// The status message's own cells; a click opens the Messages output
    /// channel, where every message is kept in full (#1609).
    status_message_rect: Rect,
    /// The last status message logged to the Messages channel, so each one
    /// is logged once, when it first shows (#1609).
    status_logged: String,
    /// A new status message is showing and nothing has been pressed since,
    /// so a message too long for the room left by the right-hand cluster
    /// (Ln/Col, encoding, language) takes the whole bar (#1609). The next
    /// key or click hands the room back.
    status_wide: bool,
    /// The running CodeQL query's status-bar chip; a click cancels it (#578).
    status_codeql_rect: Rect,
    /// The croft update pills ("Update ready", the drift rebuild hint); a
    /// click does what the updater chord does (#865).
    status_update_rect: Rect,
    /// The "Update Available" pill under the update card; a click opens or
    /// folds the card.
    status_offer_rect: Rect,
    status_indent_rect: Rect,
    status_encoding_rect: Rect,
    status_eol_rect: Rect,
    status_language_rect: Rect,
    connect_dialog: Option<crate::widgets::connect_dialog::ConnectDialog>,
    connect_auth: Option<crate::remote_connect::SshAuth>,
    install_session: Option<crate::install_session::InstallSession>,
    /// Background self-update watcher, present only on a remote-launched
    /// croft (gated by `CROFT_REMOTE_AUTOUPDATE`). Polls the install-stamp
    /// the local cross-build writes when it finishes shipping a newer
    /// binary, so the running croft can re-exec into it in place.
    update_watch: Option<crate::update_watch::UpdateWatch>,
    /// True while this croft has stopped its language servers because a
    /// source build of croft is compiling on the same host (#694), so the
    /// servers are restarted once it ends. Only a remote-launched croft
    /// ever sets it.
    lsp_paused_for_build: bool,
    /// When the build marker was last checked: it is a file read, so at
    /// most once a second rather than every loop iteration.
    build_marker_checked: Option<std::time::Instant>,
    /// One-shot startup probe (#242): does the repo this binary was built
    /// from now sit at a different commit/dirty state than the binary has
    /// baked in? Local-only — a remote-launched croft is re-stamped by its
    /// launcher instead. Dropped once its verdict is drained.
    drift_probe: Option<crate::update_watch::DriftProbe>,
    /// The repo's current hash label when the probe found drift ("def456",
    /// "def456-dirty"). Arms the "Ctrl+Shift+F9 to rebuild" status-bar
    /// hint. Kept through a running rebuild — the Idle guards hide the hint
    /// while it runs, and a failure then re-arms the updater for a retry
    /// instead of silently dropping the offer.
    local_drift: Option<String>,
    /// Background `cargo install --path <repo>` reinstalling the local
    /// croft after the user took the drift hint's offer. Reported through
    /// the same UpdateEvent lifecycle as the remote watcher.
    self_install: Option<crate::update_watch::SelfInstall>,
    /// One-shot startup check (#333): is a newer release published than
    /// the version this binary carries? Local-only, throttled to a daily
    /// network request by its cache file. Dropped once its verdict lands.
    update_check: Option<crate::update_check::UpdateCheck>,
    /// A check the user asked for with Help: Update croft when nothing was
    /// on offer (#1844), and how it asks the network (stubbed in tests).
    manual_update_check: Option<crate::update_check::ManualCheck>,
    update_fetch: fn() -> Result<Option<crate::update_check::Release>, String>,
    /// The "vX available" popup, present from the offer landing until the
    /// user picks Update, Skip, or (once staged) Relaunch; Later only folds
    /// it into its status-bar pill.
    update_toast: Option<UpdateToast>,
    /// Whether this binary may update itself, or belongs to a package
    /// manager that must do it (#375). Read once at startup.
    install_source: crate::update_check::InstallSource,
    /// A background `cargo install --root <cache>/staged` of the offered
    /// release. Reported through the same UpdateEvent lifecycle as the
    /// remote watcher; the binary on PATH is untouched until Relaunch.
    staged_install: Option<crate::update_check::StagedInstall>,
    /// The staged release build's own lifecycle (#333). `update_status`
    /// belongs to the remote watcher and the drift rebuild; the two cells
    /// are never written by the other's producer, so a rebuild starting or
    /// failing after the release landed cannot disarm Relaunch, and the
    /// reverse. Readers that mean "any update" consult both.
    staged_status: UpdateStatus,
    update_status: UpdateStatus,
    /// When a background self-update is in progress, the instant it started.
    /// Drives the spinning update glyph in the status bar (and the redraw
    /// cadence that animates it). `None` whenever no update is running.
    update_spinner_start: Option<std::time::Instant>,
    /// Set when an update landed and croft should re-exec after the main
    /// loop unwinds and the alt-screen is surrendered. Read by `run`.
    pending_reexec: bool,
    overlays: OverlayManager,
    /// VS Code-style Cmd+F in-editor find overlay. None when closed.
    pub editor_find: Option<crate::widgets::editor_find::EditorFind>,
    /// Cmd+F find-in-terminal overlay: searches the active pane's grid +
    /// scrollback (like iTerm2 / Ghostty scrollback search). Reuses the
    /// editor find widget; the replace row is never shown. None when closed.
    pub terminal_find: Option<crate::widgets::editor_find::EditorFind>,
    /// The pane index `terminal_find` opened on. Find is bound to that pane
    /// like quick-select and copy mode: a gesture that activates a sibling
    /// closes it (per-frame invariant in `sync_focus_flags`), so its
    /// highlight and jump anchor never stray onto another pane's grid.
    terminal_find_pane: usize,
    /// The active terminal-find match as `(clock, abs_grid_line, col, len)`,
    /// the anchor next/prev navigation walks from — the line is stamped with
    /// the scroll-clock reading of the snapshot it was found in, and every
    /// use re-bases it first, so output streaming under an open find bar
    /// cannot leave navigation walking from a phantom row. None until the
    /// first match.
    terminal_find_match: Option<(i64, i32, usize, usize)>,
    /// Ctrl+Shift+Space quick-select hint mode on the active terminal pane
    /// (WezTerm's quick-select): every labelled match on the viewport plus
    /// the label prefix typed so far. None when the mode is off.
    pub terminal_quick_select: Option<QuickSelectState>,
    /// Durable cross-session command history (atuin's model): every
    /// finished command from a shell-integrated pane, with cwd / exit /
    /// duration / timestamp, persisted as JSONL under the config dir.
    pub command_history: crate::command_history::CommandHistory,
    /// Ctrl+Shift+H search popup over [`Self::command_history`]. None when
    /// closed.
    pub command_history_popup: Option<crate::widgets::history_popup::HistoryPopup>,
    /// Where the terminal-session snapshot lives
    /// (`terminal_sessions.json`); a field so tests point it at a tempdir.
    pub terminal_session_path: PathBuf,
    /// Whether a restore brings back plain panes whose shells have ended as
    /// fresh shells (#1458): only over SSH, where a dropped connection should
    /// come back to its panel. Locally they would return empty, so they stay
    /// closed.
    pub terminal_restore_respawns: bool,
    /// The most recent terminal-session persistence failure, latched by
    /// `save_terminal_session` and cleared by a later success; composed
    /// into structural-change statuses by `terminal_status` so a corrupt
    /// store is never silently ignored (#157/#158).
    terminal_store_error: Option<String>,
    /// The persisted folder-set store (#153); a field so tests redirect it.
    pub workspace_folders_path: PathBuf,
    /// A pane reorder is pending a session write. `move_terminal` runs once
    /// per motion report during a drag, and the write is a full
    /// read-modify-write of the session file plus a cwd syscall per pane, so
    /// it is marked here and flushed once per tick instead of inline.
    pub terminal_session_dirty: bool,
    /// Ctrl+Shift+Y copy mode on the active terminal pane (WezTerm / tmux):
    /// vi keys walk a cursor through the scrollback, v / V / Ctrl+V select,
    /// y copies. None when the mode is off.
    pub terminal_copy_mode: Option<CopyModeState>,
    /// The user's terminal trigger set (`triggers.json`), pushed into every
    /// pane on the drain tick and swapped wholesale on config reload.
    pub triggers: std::sync::Arc<crate::triggers::TriggerSet>,
    /// The agents croft recognises in a pane's foreground (#344): built in
    /// plus `agents.json`, reloaded on save.
    agents: crate::agents::AgentTable,
    /// Agent transitions from the last sample, drained on the tick.
    agent_events: std::collections::VecDeque<crate::agents::AgentEvent>,
    /// Hit rect of the status bar's agents chip; empty when no agent is seated.
    status_agents_rect: Rect,
    /// Hit rect of the status bar's `Ln X, Col Y` readout. A click opens the
    /// Go to Line prompt: a readout becomes a control, as in VS Code.
    status_position_rect: Rect,
    /// User-defined problem matchers (#252): `matchers.json` (user config
    /// dir + workspace `.croft/`), batch-scanned at the FinishedCommand
    /// boundary alongside the built-in table. Swapped wholesale on reload.
    pub matchers: std::sync::Arc<crate::problem_matchers::MatcherSet>,
    /// The background-capable subset of `matchers`, pushed into every pane
    /// on the drain tick exactly like the trigger set.
    pub watch_set: std::sync::Arc<crate::problem_matchers::WatchSet>,
    /// tasks.json `problemMatcher` assignments: pane uid → the matcher the
    /// task running there declared. Exclusive for that pane's batch scans
    /// (VS Code's model: an explicit matcher replaces the defaults).
    task_matcher_by_pane:
        std::collections::HashMap<u64, std::sync::Arc<crate::problem_matchers::CompiledMatcher>>,
    /// Panes whose PROBLEMS batch a watch matcher currently owns. The
    /// watcher eventually exiting fires a FinishedCommand whose whole-
    /// history rescan would resurrect every error of every past cycle, so
    /// that one scan is skipped (the flag clears with it — the pane's next
    /// ordinary command scans normally).
    watch_published_panes: std::collections::HashSet<u64>,
    /// Broadcast input (Cmd+K I, iTerm2's Cmd+Opt+I): while on, shell-bound
    /// keystrokes and pastes in the terminal go to every pane instead of
    /// just the focused one. Session-scoped; never persisted.
    pub broadcast_input: bool,
    /// True while the "enable broadcast?" confirm popup is up (enabling is
    /// gated: the classic footgun is typing into panes you forgot were
    /// listening).
    pub pending_broadcast_enable: bool,
    /// A multi-line paste waiting on its confirm popup (#1274): written to
    /// the pane only once the user picks Paste or Paste as one line.
    pub pending_terminal_paste: Option<Vec<u8>>,
    /// The `terminal_multiline_paste_warning` setting (#1274).
    pub multiline_paste_warning: crate::prefs::MultilinePasteWarning,
    /// A runnable fence waiting on the confirm popup (#353): destructive-
    /// looking blocks and `{confirm}` fences ask before they type.
    pending_run_block: Option<PendingRunBlock>,
    /// Blocks whose output croft is still waiting for (#354).
    pending_captures: Vec<PendingCapture>,
    /// Captured output per fence, keyed by the pane name (which already
    /// encodes the document's path under the workspace) and the fence
    /// opener's source line. In memory for the session unless the fence
    /// said `{persist}`.
    block_outputs: std::collections::HashMap<(String, usize), crate::markdown::BlockOutput>,
    /// Paint the terminal panes' right-edge arrival-time gutter ("Terminal:
    /// Toggle Timestamps"). Session-scoped, off by default.
    pub show_terminal_timestamps: bool,
    /// Notification sinks from config.json `notifications` (#358).
    notifier: crate::notifications::Notifier,
    /// Compiled per-host pane accent rules from config.json `host_accents`.
    /// First match wins.
    host_accents: Vec<HostAccent>,
    /// Recently closed terminal panes parked for the undo-close grace window
    /// (Ghostty's undo, iTerm2's Undo Close Session): the PTY and child stay
    /// alive but hidden until Cmd+K ⇧T restores the newest or the tick reaps
    /// entries older than [`UNDO_CLOSE_GRACE`]. Newest last.
    pub closed_terminals: Vec<ClosedTerminal>,
    /// Recently closed editor tabs (VS Code "Reopen Closed Editor"): the
    /// path plus enough view state (pin, cursor, scroll) to restore the tab
    /// as it was. Newest last; Cmd+K ⇧W pops. Bounded at
    /// [`CLOSED_TAB_STACK_CAP`]; entries whose file has since vanished are
    /// skipped at reopen time.
    pub closed_tabs: Vec<ClosedTab>,
    /// VS Code-style Cmd+P / Ctrl+P quick-open file finder. None when
    /// the modal is closed.
    pub file_finder: Option<crate::widgets::file_finder::FileFinder>,
    /// The range the last hinted quick-open pick installed, and the file it
    /// was installed in: the next pick clears it if it is still the
    /// selection there, and nothing else, so a selection the user made by
    /// hand (or extended from it) survives a re-pick of the same file.
    quick_open_range: Option<(PathBuf, crate::widgets::editor::EditorSelection)>,
    /// VS Code-style Cmd+Shift+P / Ctrl+Shift+P command palette. None when
    /// the modal is closed.
    pub command_palette: Option<crate::widgets::command_palette::CommandPalette>,
    /// VS Code-style Cmd+Shift+O / Ctrl+Shift+O "Go to Symbol in Editor"
    /// picker (also Go to Line via a `:` prefix). None when the modal is closed.
    pub go_to_symbol: Option<crate::widgets::symbol_picker::SymbolPicker>,
    /// "Go to Symbol in Workspace" (`#` in Quick Open): query + merged
    /// `workspace/symbol` results across every running language server.
    pub workspace_symbols: Option<crate::widgets::workspace_symbols::WorkspaceSymbolPicker>,
    /// The newest `workspace/symbol` request id; stale replies are dropped.
    ws_symbols_request_id: Option<u64>,
    /// Armed by a picker keystroke; `workspace_symbols_dispatch_due` fires
    /// the request once the query has been still for the debounce window.
    ws_symbols_pending: Option<std::time::Instant>,
    /// Receiver for an in-flight MCP sidecar command's result (the off-thread
    /// worker delivers one [`crate::mcp::McpOutcome`] then disconnects). `None`
    /// when no extension command is running; polled each frame by `poll_mcp`.
    mcp_rx: Option<std::sync::mpsc::Receiver<crate::mcp::McpOutcome>>,
    /// While an extension command runs: when it started (drives the status-bar
    /// spinner frame) and its human label (the animated badge text). Both `None`
    /// when idle; set in `spawn_mcp_command`, cleared in `poll_mcp`.
    mcp_started: Option<std::time::Instant>,
    mcp_busy_label: Option<String>,
    /// "Debug: Attach to Python Process" picker (PEP 768). None when closed.
    /// Lists running CPython 3.14+ processes; selecting one spawns
    /// `pdb -p <pid>` in a PTY.
    pub process_picker: Option<crate::widgets::process_picker::ProcessPicker>,
    /// Active debug session: a debugpy DAP launch. None when not debugging.
    /// Polled each frame by [`App::poll_dap`], which mirrors the paused location
    /// into `editor.stop_line`.
    /// The debug sessions in flight, one FOCUSED (#310). Was a bare
    /// `Option`, which made "the session" the only thing a call site could
    /// mean; a compound needs several, so every site now says whether it
    /// means the focused one or all of them.
    pub debug_sessions: crate::dap::session::DebugSessions,
    /// launch.json configurations discovered for the picker (#250); refreshed
    /// on every picker open so edits are picked up without a restart.
    debug_configs: Vec<crate::dap::configs::DebugConfig>,
    /// The configuration **Debug: Add Configuration…** is building (#250),
    /// between its steps.
    debug_config_draft: Option<crate::dap::configs::ConfigDraft>,
    /// Compounds declared beside those configurations. Listed in the picker so
    /// a workspace's compounds are visible; launching one needs the
    /// multi-session model tracked in #310.
    debug_compounds: Vec<crate::dap::configs::Compound>,
    /// Name of the launch.json configuration F5 starts. `None` = the
    /// zero-config "Debug active file" behavior.
    selected_debug_config: Option<String>,
    /// Name of the compound F5 starts, when the last picker launch was a
    /// compound. Exclusive with `selected_debug_config`: a compound and a
    /// configuration may share a name, so one string cannot say which (#567).
    selected_debug_compound: Option<String>,
    /// The launched compound asked for `stopAll: true` (#567): the first
    /// member to end stops the rest. Set on every compound launch, so a
    /// single-config launch (one session, no siblings) never reads a stale
    /// value in a way that matters.
    debug_stop_all: bool,
    /// The `postDebugTask`s of the configurations launched into the current
    /// run (#250), run once each when the run ends: the user stops it, or
    /// the last session terminates. A new launch starts a fresh list.
    debug_post_tasks: Vec<String>,
    /// The running compound's name, kept past launch so the report rebuilt
    /// when one member ends can still say which compound is live (#567).
    debug_compound: Option<String>,
    /// A config launch parked behind its `preLaunchTask`: the task runs in
    /// its pane, and the FinishedCommand sweep launches (exit 0) or aborts
    /// (non-zero) when the matching command completes. Replaced by a newer
    /// F5, so only one launch can be pending.
    pending_debug_launch: Option<PendingDebugLaunch>,
    /// The `${input:…}` values a launch or task is asking for (#1424).
    input_run: Option<InputRun>,
    /// F5's C/C++ build running off the UI thread (#1359): the source it
    /// debugs and where the build's result arrives. `poll_debug_build`
    /// launches lldb-dap on the program once it is built.
    debug_build: Option<(
        PathBuf,
        std::sync::mpsc::Receiver<Result<crate::cpp_build::Built, crate::cpp_build::Failed>>,
    )>,
    /// An attach launch waiting for the user to pick its process (#250):
    /// the resolved config and where it launches once `processId` is known.
    pending_attach: Option<(crate::dap::configs::ResolvedConfig, LaunchSlot)>,
    /// In-flight background `cargo test --no-run` for debug-a-test: the
    /// receiver yields the picked binary (or the build error) and the test
    /// name rides along for the launch. Drained per tick; a second request
    /// while one is pending is refused with a status line.
    pending_test_debug: Option<PendingTestDebug>,
    /// CodeLLDB `cargo` launches building their programs on background
    /// threads (#1392): each receiver yields the executable (or the build
    /// error), and its config launches into its slot when it lands. One per
    /// compound member that builds; Stop Debugging drops them all.
    pending_cargo_launches: Vec<PendingCargoLaunch>,
    /// Variable `variablesReference`s the user has expanded in the Variables
    /// panel. Persisted across polls so re-fetched variables stay open.
    pub debug_expanded: std::collections::HashSet<i64>,
    /// Debugger watch expressions (#112), in display order. Session-scoped.
    pub watch_exprs: Vec<String>,
    /// Latest evaluate result per expression: `(value, success)`.
    pub watch_vals: std::collections::HashMap<String, (String, bool)>,
    /// The previous stop's values, for the changed-value highlight.
    watch_prev: std::collections::HashMap<String, String>,
    /// Armed by every `Stopped` event, consumed by the first
    /// `InspectionUpdated` that follows: the baseline rotates exactly once
    /// per stop. `InspectionUpdated` also fires for frame switches and
    /// variable expansions within the same stop, and rotating on those
    /// clobbered the baseline with the stop's own values — the changed
    /// highlight never fired (#112).
    watch_baseline_pending: bool,
    /// The latest Copy Value waiting on the adapter's `clipboard` evaluate
    /// (#1546): the expression asked for and the row's shown value, copied
    /// instead if the evaluate fails. A response for any other expression
    /// answers an earlier Copy Value the user has since replaced, and is
    /// dropped.
    debug_copy_pending: Option<(String, String)>,
    /// Debug console log: program stdout/stderr (DAP output events) plus REPL
    /// echoes and results. Capped; the tail is shown in the Run & Debug panel.
    /// Each line is escape-free text with the colour spans its SGR sequences
    /// set (#1501), so search and copy see `FAILED`, not `[31mFAILED[0m`.
    pub debug_console: Vec<crate::ansi_text::AnsiLine>,
    /// How many lines have left the front of `debug_console`, to the cap or
    /// to a new run's clear, so a stream's open line keeps its place by
    /// counting from the first line the console ever held (#932).
    debug_console_dropped: usize,
    /// True once the current/just-ended debug session stopped at least once
    /// (breakpoint / step / exception). Drives the "exited without hitting a
    /// breakpoint" end message. Reset when a new session starts.
    debug_ever_stopped: bool,
    /// zoxide-backed Explorer jump popup (Cmd+Z). None when closed. Opens
    /// only from the Explorer pane, so it never collides with the editor's
    /// Cmd+Z undo.
    pub zoxide_jump: Option<crate::widgets::zoxide_jump::ZoxideJump>,
    /// True while the zoxide popup is picking a folder to ADD to the
    /// workspace (#147) rather than re-rooting; reset on close.
    pub zoxide_add_folder: bool,
    pub branch_picker: Option<crate::widgets::branch_picker::BranchPicker>,
    /// Why the branch picker is open, so its Enter dispatches the right
    /// verb (checkout vs merge vs rebase vs delete vs create-from base).
    pub branch_purpose: BranchPurpose,
    /// The switch local changes blocked (#925), with the repository it is
    /// in and the branch it was refused on (None when detached), while the
    /// Stash & Checkout picker offers a way through.
    blocked_switch: Option<(
        PathBuf,
        Option<String>,
        crate::widgets::branch_picker::BranchAction,
    )>,
    /// The Source Control "⋯" title menu (open/expanded state + hit rects).
    pub scm_menu: crate::widgets::scm_menu::ScmMenuState,
    /// Single-line input modal for SCM ops that need a typed value
    /// (clone URL, branch rename, remote name/URL, tag name).
    pub input_prompt: Option<crate::widgets::input_prompt::InputPrompt>,
    /// Single-choice picker for SCM ops that act on one of a git-owned list
    /// (apply/pop/drop a stash, delete a tag, remove/push-to a remote).
    pub list_picker: Option<crate::widgets::list_picker::ListPicker>,
    /// Privileged control channel to this session's host when running under
    /// a persistent multiplayer session (docs/MULTIPLAYER.md); drives the
    /// participants picker's grant/revoke/disconnect actions. None outside
    /// a session host.
    pub session_channel: Option<crate::session_host::InnerChannel>,
    /// Roster last read from the session host's presence sidecar.
    pub session_participants: Vec<crate::session_host::Participant>,
    /// Whether the host's stale-image marker (#238) has already been
    /// announced, so the status hint fires once per session, not per poll.
    session_host_stale_seen: bool,
    /// The stale-client notice the current roster warrants (#626, #652), so
    /// it is raised once when it changes, not on every roster churn.
    session_stale_notice: Option<String>,
    /// Multiplayer: the participant whose keystrokes are currently flowing
    /// into this croft (from the host's typing attribution frames).
    session_typist: Option<u64>,
    /// Multiplayer: every participant's last-known caret, stashed when the
    /// typist changes and restored when they type again, so interleaved
    /// input drives per-participant carets in the shared buffer.
    session_carets: std::collections::HashMap<u64, SessionCaret>,
    /// Sidecar mtime at the last roster read, so the poll is a cheap stat.
    session_presence_mtime: Option<std::time::SystemTime>,
    /// Cadence gate for the presence poll (stat every 500ms, not every tick).
    last_session_presence_poll: std::time::Instant,
    /// Phase D independent-viewport collab session (docs/MULTIPLAYER.md):
    /// per-file replicated documents over the workspace's collab relay.
    /// None until the lazy connect in `poll_collab` succeeds.
    collab: Option<crate::collab::CollabSession>,
    /// The collab socket and role the launch tail exported, read once at
    /// startup. None when this croft is not a collab participant.
    collab_config: Option<(PathBuf, crate::collab::CollabRole)>,
    /// Throttle for the lazy relay connect (the owner starts before any
    /// guest has spawned the relay, so the first attempts fail by design).
    last_collab_connect: Option<std::time::Instant>,
    /// Per-file merge base harvested when the relay link died: the last
    /// replicated text every participant agreed on. A reconnect bootstrap
    /// replays only this side's offline delta against it (three-way with the
    /// owner's snapshot), so owner edits made during the outage survive.
    collab_offline_base: std::collections::HashMap<String, String>,
    /// Peers' last-known carets by site id. Painted as ghost carets in the
    /// peer's color when their file is active, with a name tag while the
    /// caret is inside its fade window.
    collab_carets: std::collections::HashMap<u64, CollabCaret>,
    /// The last caret this croft broadcast, so an unmoved cursor sends
    /// nothing on the tick.
    collab_caret_sent: Option<(String, usize, usize)>,
    /// Whether any caret name tag was inside its fade window last tick;
    /// flipping to false is the one redraw an expiring tag needs.
    collab_labels_visible: bool,
    /// Some while an AI pilot (`croft pair`) streams an edit into a shared
    /// file: drives the status badge and the gutter stop button; cleared by
    /// the pilot's StreamState(inactive) broadcast.
    collab_stream: Option<CollabStreamInfo>,
    /// The resident navigator (docs/MULTIPLAYER.md): the in-process pilot
    /// host, seated while the workspace's pair record is enabled. Dropping
    /// it tears the seat down (revert, hang up, grace-kill).
    pair_host: Option<crate::pair_host::PairHost>,
    /// Where the workspace's pair activation record lives. A field so tests
    /// point it at a tempdir instead of the real sessions dir.
    pair_record_path: PathBuf,
    /// The collab socket the navigator (and a self-appointed owner seat)
    /// uses. A field for the same test reason.
    pair_socket: PathBuf,
    /// Path of the single-host lock (see [`pair_host_lock`]). A field for the
    /// same test reason.
    pair_host_lock_path: PathBuf,
    /// Cadence gate for the pair-record stat (1s, not every tick).
    last_pair_check: Option<std::time::Instant>,
    /// Per-file (0-based row, body) snapshots of the navigator's anchored
    /// notes, refreshed each tick from the host; drives the gutter ◆ marks
    /// and the note popup.
    navigator_notes: std::collections::HashMap<String, Vec<(u64, usize, String)>>,
    /// The navigator's prose accumulated over the streaming turn; lands as
    /// one comment box at the turn's origin when the turn ends (boxes are
    /// the navigator's single voice — there is no OUTPUT channel for it).
    pair_commentary_buf: String,
    /// Where the current turn was invoked: (collab file key, 0-based row).
    /// Anchors the turn's commentary box.
    pair_turn_origin: Option<(String, usize)>,
    /// Notes the navigator left since its turn started, so the TurnDone
    /// status can surface them instead of overwriting the NoteAdded status.
    pair_notes_this_turn: usize,
    /// The file the turn's notes LAST landed in (anchors the turn's prose
    /// when the invocation origin is unknown).
    pair_last_noted_file: Option<String>,
    /// Every file the turn's notes landed in: the TurnDone hint names the
    /// file only when there is exactly one ("3 comments in b.rs" was a lie
    /// when two of them sat in a.rs).
    pair_noted_files: std::collections::BTreeSet<String>,
    /// Held while this croft is the self-appointed navigator owner: exactly
    /// one plain croft per workspace may host the pilot (and claim collab
    /// owner site 1). Released by the OS on exit, so a crashed host hands off.
    pair_host_lock: Option<crate::session::PairHostLock>,
    /// Set when the pilot died or failed to seat: suppresses the 1s re-seat
    /// so a broken claude (unauthenticated, missing, crashing) is not
    /// respawned forever. Cleared when the record is deactivated OR
    /// rewritten (any `croft pair` run re-arms seating, as the CLI
    /// promises), so an explicit off/on (or toggle) re-activates. Matches
    /// the LSP precedent: no auto-respawn within a session; a fresh croft
    /// retries.
    navigator_down: bool,
    /// The pair record's raw bytes at the last 1s check: changed bytes mean
    /// someone ran `croft pair` (even enabled→enabled — every write stamps
    /// the record, so no rewrite is byte-identical) and clear
    /// [`navigator_down`]. Content, not mtime: a same-grain rewrite on a
    /// coarse-timestamp filesystem keeps its mtime.
    last_pair_record_raw: Option<Vec<u8>>,
    /// Which host-lock refusal has already been announced: the losing
    /// window keeps polling for takeover every second, and re-announcing
    /// each poll clobbered its status line forever (and forced a 1 Hz
    /// repaint). The KIND is latched, not just the fact, so a refusal that
    /// changes reason (an fd limit clears and another window now holds the
    /// lock, or the reverse) is announced again rather than leaving a
    /// message that is no longer true (#337). Per kind, not per message: an
    /// open failure whose errno changes keeps the first announcement.
    pair_lock_denied: Option<PairLockDenial>,
    /// Proactive navigator looks (docs/MULTIPLAYER.md): a completed new
    /// construct plus a typing pause hands the seated navigator a
    /// comment-only turn on its own. Opt-out via
    /// `Prefs::disable_proactive_navigator` / the palette toggle.
    proactive_navigator_enabled: bool,
    /// Per file, the edit_seq the proactive trigger last scanned: one
    /// buffer state is parsed at most once, however long the pause lasts
    /// and however often the driver switches tabs across visited files.
    proactive_scanned: std::collections::HashMap<PathBuf, u64>,
    /// Test seam: builds the PairHost from the config (the real path spawns
    /// the claude CLI, which a test must never do).
    #[cfg(test)]
    #[allow(clippy::type_complexity)]
    pair_spawn_override:
        Option<Box<dyn Fn(&crate::pair::PairConfig) -> anyhow::Result<crate::pair_host::PairHost>>>,
    /// Test seam: the kernel bridge Select Kernel starts, by kernel name
    /// (the real one runs Python).
    #[cfg(test)]
    #[allow(clippy::type_complexity)]
    kernel_start_override: Option<Box<dyn Fn(&str) -> crate::notebook_kernel::KernelSession>>,
    /// Rolling log of git commands croft has run and their summaries, shown
    /// read-only by the "Show Git Output" action (VS Code's Git channel).
    pub git_output_log: Vec<String>,
    /// Build diagnostics from the problem matchers (#119): finished-command
    /// output scanned into PROBLEMS entries, keyed by resolved file path.
    /// Paired with the producer that installed it (the pane uid, or
    /// `PROJECT_CHECK_ID` for the whole-project sweep). Without the pairing
    /// a re-run removed the whole entry and took the OTHER producer's rows
    /// with it, and the second producer to reach a file could not clear its
    /// own rows at all (#532).
    build_diagnostics:
        std::collections::HashMap<PathBuf, Vec<(u64, crate::widgets::problems::ProblemItem)>>,
    /// Which files each terminal pane contributed, keyed by the pane's
    /// STABLE uid (Vec positions shift on close/insert/reorder), so a
    /// pane's next scanned command replaces its previous run's diagnostics
    /// (VS Code clears a task's problems when the task re-runs).
    build_diag_files_by_pane: std::collections::HashMap<u64, Vec<PathBuf>>,
    /// The HEAD a warned Undo Last Commit (#1350) is armed for: a commit
    /// already on its upstream is undone on the second ask at that HEAD.
    undo_pushed_commit_armed: Option<String>,
    /// What a warned Reset Current Branch to Here > Hard (#1627) is armed
    /// for: the repo root, the commit, and the working tree's
    /// [`crate::git::worktree_fingerprint`] when it warned. It runs on the
    /// second ask only while all three still match, so work made since the
    /// warning is warned about afresh.
    reset_hard_armed: Option<(PathBuf, String, String)>,
    /// True while the "Discard All Changes" confirmation modal is up.
    pub pending_discard_all: bool,
    /// Drop Stash, picked but not yet confirmed (#1128).
    pending_stash_drop: Option<PendingStashDrop>,
    /// The Replace All confirmation (#123): `(occurrences, files, dirty
    /// files to be skipped)` counted without writing; the modal shows them,
    /// Enter/Y commits, Esc cancels.
    pending_replace_all: Option<(usize, usize, usize)>,
    /// Cached workspace file index. Built lazily on first Cmd+P and
    /// shared across reopens so repeated invocations are instant.
    file_finder_index: Option<std::sync::Arc<Vec<crate::widgets::file_finder::FileEntry>>>,
    /// Background-build receiver: App::new spawns a thread that walks
    /// the workspace and sends the index here; Cmd+P opens against the
    /// hot index instead of walking on the UI thread. None once the
    /// index has been received (or if a build was never kicked off).
    file_finder_index_rx:
        Option<std::sync::mpsc::Receiver<Vec<crate::widgets::file_finder::FileEntry>>>,
    /// Set when a rebuild was requested while one was already in flight.
    /// poll_file_finder_index drains it on completion and re-kicks the
    /// background walker so a long-running rebuild (multi-GB monorepo)
    /// can never miss a later FS event that landed mid-walk.
    file_finder_index_dirty: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct EditorVimChord {
    op: VimOp,
    count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorImageLayout {
    pub cell_x: u16,
    pub cell_y: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    pub path: PathBuf,
    /// Which page of a PDF is baked into this overlay (1 for anything else).
    /// Part of the re-emit key so paging re-bakes on its own: without it the
    /// path and rect are unchanged on a page turn and the stale picture stays
    /// on screen unless every call site remembers to invalidate the layout.
    pub page: u32,
    /// Content stamp of the baked bytes ([`crate::widgets::editor::ImageView::generation`]).
    /// Part of the re-emit key so a file rebuilt on disk re-bakes even when
    /// the path, rect and page are all unchanged (a pdflatex rebuild lands
    /// on the same page the reader was on).
    pub generation: u64,
}

/// Re-emit key for the terminal pane's inline-image overlay: the cell rect
/// the picture occupies plus which image (per-pane `seq`) and pane it is.
/// Scrolling moves the anchor row, changing `cell_y` — a layout mismatch —
/// so the overlay clears and re-emits at the new position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalImageLayout {
    pub cell_x: u16,
    pub cell_y: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    pub seq: u64,
    pub pane: usize,
}

/// Markdown-preview inline image placement (#176): geometry plus the
/// bake identity (path + mtime), so an externally rewritten picture
/// re-bakes and a pure scroll reuses the payload.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkdownImageLayout {
    pub cell_x: u16,
    pub cell_y: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    pub path: PathBuf,
    pub mtime_ms: u128,
}

/// Cached full-document raster for the minimap. The expensive part (walking
/// every line's syntax spans) runs only when `sig` changes; scrolling reuses
/// `rgba` and just re-composites the viewport box on top.
struct MinimapBase {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    /// (path, edit_seq, canvas_w_px, canvas_h_px, bg, lines drawn,
    /// highlight generation) — a change rebakes. The lines drawn tell a
    /// symbol tab (#369) from its file's tab; the highlight generation moves
    /// when a syntax pass lands after its edit (#850).
    sig: MinimapSig,
}

/// Re-emit key for the minimap overlay. Differs from the base sig: the
/// viewport fields move on scroll (recomposite, no rebake), the cell fields
/// move on resize/layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinimapLayout {
    cell_x: u16,
    cell_y: u16,
    cell_w: u16,
    cell_h: u16,
    top: usize,
    rows: usize,
    total: usize,
    edit_seq: u64,
    /// The editor's `highlight_generation`: a background syntax pass that
    /// lands after the edit (#850) recolors the strip.
    highlight_generation: u64,
    side: MinimapSide,
    /// Theme editor bg; a change here (theme switch) forces a rebake.
    bg: (u8, u8, u8),
    /// Active selection `(start_row, end_row)`; a change recomposites the band.
    selection: Option<(usize, usize)>,
    /// Which document and which of its lines the strip draws: a hash of
    /// the path (no per-frame allocation) and the drawn span, so switching
    /// between two tabs whose other fields agree (two same-length symbol
    /// tabs, #369, or two same-length files) still re-emits. A tab without
    /// a path is only ever the empty fallback left by closing the last
    /// tab, whose strip is the same for every one.
    doc: (u64, (usize, usize)),
}

/// One socket-poll result. `hits` is the pane-subtree scan that decides what to
/// surface; `listening` is every loopback port on the box, the only set that can
/// say a port has stopped. `None` when neither probe ran.
struct PortPoll {
    hits: Vec<crate::port_detect::PortHit>,
    listening: Option<std::collections::HashSet<u16>>,
}

/// The per-frame minimap overlays (everything not baked into the raster).
struct MinimapOverlay {
    /// First visible line, and number of visible lines (the viewport box).
    top: usize,
    rows: usize,
    total: usize,
    light: bool,
    /// Active selection `(start_row, end_row)`, marked by a thin left accent.
    selection: Option<(usize, usize)>,
    sel_rgb: (u8, u8, u8),
    /// Pixel height the lines are mapped into (top-aligned); `<= h` for short
    /// files so the viewport box and selection accent track the raster.
    content_h: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WelcomeLayout {
    cell_x: u16,
    cell_y: u16,
    cell_w: u16,
    cell_h: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScpUpload {
    pub alias: String,
    pub src: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRemotePull {
    pub request_id: String,
    pub src_display: String,
    pub basename: String,
    pub dest_dir: PathBuf,
    pub started_at: std::time::Instant,
    pub kind: RemotePullKind,
}

struct RemoteLaunch {
    host: String,
    path: Option<String>,
    adopted: Option<crate::remote::AdoptedMaster>,
}

/// A finished SARIF source download (#577).
struct SarifDownload {
    uri: String,
    /// 1-based position to open at.
    line: i64,
    col: i64,
    body: Result<Vec<u8>, String>,
}

fn split_at_char_count(s: &str, count: usize) -> (String, String) {
    let left: String = s.chars().take(count).collect();
    let right: String = s.chars().skip(count).collect();
    (left, right)
}

fn wrap_cells_variable_width(text: &str, first_width: u16, rest_width: u16) -> Vec<String> {
    let first_width = first_width.max(1) as usize;
    let rest_width = rest_width.max(1) as usize;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    for word in trimmed.split_whitespace() {
        let mut remaining = word.to_string();
        while !remaining.is_empty() {
            let width = if lines.is_empty() {
                first_width
            } else {
                rest_width
            };
            let sep = usize::from(!current.is_empty());
            let remaining_len = remaining.chars().count();
            let current_len = current.chars().count();
            if current_len + sep + remaining_len <= width {
                if sep == 1 {
                    current.push(' ');
                }
                current.push_str(&remaining);
                break;
            }
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                continue;
            }
            let (head, tail) = split_at_char_count(&remaining, width);
            lines.push(head);
            remaining = tail;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// Where Croft Loch's welcome stack puts each block, in rows from the top of
/// the welcome area. Computed by [`loch_welcome_plan`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LochWelcomePlan {
    /// The logo's first row and its height: one row for the text wordmark,
    /// 4..=14 for the picture.
    logo_y: u16,
    logo_h: u16,
    tagline_y: u16,
    /// The Start list's heading row, when the list shows.
    start_y: Option<u16>,
    /// The Take the Tour chip's row, when the Start list cannot show it.
    chip_y: Option<u16>,
    /// The release card's top row and height, and how many notes it shows
    /// whole (`notes == 0`: no card).
    card_y: u16,
    card_h: u16,
    notes: usize,
}

use crate::gradient::{GRAD_TL, GRAD_TR, POPUP_SEL_BG, paint_gradient_box, rgb_color};

/// A language server's `window/showMessageRequest` waiting for an answer
/// (#1599). Dropping it answers "no action".
struct LspQuestion {
    server: String,
    text: String,
    actions: Vec<lsp_types::MessageActionItem>,
    reply: tokio::sync::oneshot::Sender<Option<lsp_types::MessageActionItem>>,
}

/// An accepted completion waiting on the auto-import its server sends only
/// from `completionItem/resolve` (#928).
struct PendingCompletionResolve {
    request_id: u64,
    path: PathBuf,
    /// Where the accepted text begins; the import must end before it.
    start: (usize, usize),
    /// The caret after the accept, and a hash of the text before it.
    upto: (usize, usize),
    text_hash: u64,
    undo: ImportUndo,
}

fn open_url(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    // Detach the opener's stdio: croft is on the alternate screen in raw
    // mode, and `open`/`xdg-open` chatter ("No application knows how to
    // open URL ...") would land straight on the grid.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().with_context(|| format!("opening {url}"))?;
    Ok(())
}

/// Short label for this machine, shown as the origin of a remote port in the
/// PORTS panel (`⇄ build-box`). Resolved once via `hostname`, falling back to a
/// generic word so the column is never blank.
fn remote_host_label() -> String {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().split('.').next().unwrap_or("remote").to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| String::from("remote"))
    })
    .clone()
}

impl App {
    pub fn new(root: PathBuf) -> Result<Self> {
        // Reap any js-debug adapter tree a previous croft left orphaned (a
        // crash or force-quit skips `DapTransport`'s `Drop`, so the server +
        // its detached watchdog get reparented to init and linger). Runs on a
        // detached thread to stay off the time-to-first-frame critical path,
        // and is safe even while another croft session is live: only processes
        // reparented to init are touched. See `crate::dap::reaper`.
        crate::dap::reaper::reap_orphaned_js_debug_async(std::time::Duration::ZERO);
        let tree = FileTree::new(root.clone());
        let search = SearchPanel::new(root.clone());
        let remote = RemotePanel::new();
        let source_control = SourceControlPanel::new();
        let run_debug = RunDebugPanel::new();
        let extensions = crate::widgets::extensions::ExtensionsPanel::new();
        let outline = OutlinePanel::new();
        let open_editors = OpenEditorsPanel::new();
        let timeline = TimelinePanel::new();
        let mut dependencies = DependenciesPanel::new();
        // Detect the workspace's package ecosystems up front so the DEPENDENCIES
        // section's visibility, ⋯-menu entry, and header are correct before the
        // first sync tick. Re-detected on every re-root (see sync_explorer_panels).
        let dep_ecosystems = crate::widgets::dependencies::detect_ecosystems(&root);
        dependencies.set_header(crate::widgets::dependencies::header_label(&dep_ecosystems));
        // The active profile (#618) decides which settings, keybindings and
        // snippets files are read below, so it is chosen first. Never under
        // test: a developer's own profile choice must not leak into the suite.
        if !cfg!(test) {
            crate::profiles::activate_for_workspace(&root);
        }
        // Layered settings (#251): defaults ← user ← user-local ← workspace
        // layers of the primary root, one merged view. Warnings (refused
        // workspace keys, parse errors) land in the Settings OUTPUT channel.
        let merged_settings = crate::config_layers::load_merged(Some(&root));
        for w in &merged_settings.warnings {
            crate::output::push("Settings", crate::output::OutputLevel::Warn, w);
        }
        let loaded_prefs = merged_settings.prefs.clone();
        let (review_tx, review_rx) = std::sync::mpsc::channel();
        let data_dir =
            (!cfg!(test)).then(|| user_data_dir(&croft_cache_dir(), &crate::paths::state_dir()));
        let notes_path = data_dir
            .as_deref()
            .map(|dir| crate::sticky_notes::Notes::store_path(dir, &root));
        if !cfg!(test) {
            crate::i18n::init(loaded_prefs.locale.as_deref());
        }
        // Seed the log view's opening default from the saved preference
        // BEFORE any log can be opened (#466): the toggle keeps the two in
        // step afterwards, but a startup that set only `App::log_highlight`
        // opened the first log highlighted despite a saved opt-out. Test-
        // inert like the keymap and matcher loads below: the suite builds
        // hundreds of apps on parallel threads, and an unguarded write to
        // the process-wide default from each would race the tests that
        // read it; those call `seed_log_highlight_default` explicitly.
        seed_log_highlight_default_at_startup(!loaded_prefs.disable_log_highlight);
        // Same treatment for keybindings: a row croft refused (an unknown
        // command id, a gesture that can never fire, a reserved bare click)
        // used to vanish silently, which reads as croft being broken rather
        // than the binding being wrong (#259).
        // Under test an empty keymap, exactly like the matcher set below: the
        // developer's real keybindings.json must never steer app tests. That
        // mattered less when a user keymap only rebound key chords, which no
        // test dispatches; a mouse row now gates an early return at the top
        // of `handle_mouse` that every mouse test flows through, so one line
        // in a file the suite does not control could redirect thousands of
        // tests. Tests that want a keymap assign `app.keymap` themselves.
        let loaded_keymap = if cfg!(test) {
            crate::keymap::Keymap::default()
        } else {
            crate::keymap::Keymap::load(&crate::keymap::keybindings_path())
        };
        // `Keymap::load` already reports its own warnings to OUTPUT ·
        // Keybindings, so nothing to do here.
        // Problem matchers (#252). Under test an empty set: the developer's
        // real matchers.json must never steer app tests; tests inject their
        // own. Compile warnings surface once, in OUTPUT · Matchers.
        let matchers = std::sync::Arc::new(if cfg!(test) {
            crate::problem_matchers::MatcherSet::default()
        } else {
            crate::problem_matchers::MatcherSet::load(
                &crate::problem_matchers::matchers_path(),
                Some(&crate::problem_matchers::workspace_matchers_path(&root)),
            )
        });
        for w in &matchers.warnings {
            crate::output::push("Matchers", crate::output::OutputLevel::Warn, w);
        }
        let watch_set = std::sync::Arc::new(matchers.watch_set());
        crate::startup_trace::mark("settings, keymap and matchers loaded");
        let explorer_views = ExplorerViewVisibility::from_prefs(loaded_prefs.explorer_views);
        let disabled_extensions = loaded_prefs.disabled_extensions.clone();
        // Under test only the bundled set: an extension the developer added
        // must never steer app tests; a test adds what it needs.
        let installed_extensions = if cfg!(test) {
            crate::mcp::catalog::bundled_ids()
        } else {
            crate::mcp::catalog::installed_ids(&crate::lsp::manifest::user_extensions_dir())
        };
        let (timeline_tx, timeline_rx) = std::sync::mpsc::channel();
        let (codeql_size_tx, codeql_size_rx) = std::sync::mpsc::channel();
        let (history_done_tx, history_done_rx) = std::sync::mpsc::channel();
        let (project_check_tx, project_check_rx) = std::sync::mpsc::channel();
        let (project_hint_tx, project_hint_rx) = std::sync::mpsc::channel();
        let (test_jump_tx, test_jump_rx) = std::sync::mpsc::channel();
        let (graph_tx, graph_rx) = std::sync::mpsc::channel();
        let (blame_tx, blame_rx) = std::sync::mpsc::channel();
        let (deps_tx, deps_rx) = std::sync::mpsc::channel();
        let (voice_tx, voice_rx) = std::sync::mpsc::channel();
        let editor = EditorTabs::new();
        // Bound before the first pane, so that pane's shell carries the
        // socket like every later one. `spawn_with_preamble` reads
        // `view_ipc::SOCK_PATH` at spawn time, so this ordering is what
        // decides whether the startup pane can use `croft view` at all (#362).
        let view_listener = Self::bind_view_socket();
        // A second croft on the same workspace finds the socket live and
        // leaves it to the first: one editor answers a workspace's agents.
        let (hook_listener, hook_bind_error) = if cfg!(test) {
            (None, None)
        } else {
            match crate::agent_approval::bind(&root) {
                Ok(listener) => (Some(listener), None),
                Err(e) => (None, hook_bind_status(&e)),
            }
        };
        let view_bind_error = VIEW_BIND_ERROR.lock().unwrap().take();
        crate::startup_trace::mark("sockets bound");
        let term = PtyTerminal::new(&root).context("spawning terminal")?;
        crate::startup_trace::mark("terminal shell spawned");

        // Background git worker: every `git status` / `git status
        // --porcelain` shell-out goes through this channel instead of
        // running on the UI thread. The very first request seeds the
        // status-bar badge; subsequent ones come from the FS watcher
        // and from sidebar-icon clicks (see `refresh_source_control`,
        // `refresh_git_status_debounced`). Removes the 15-50 ms
        // input-to-paint stall that used to fire on every Source
        // Control click.
        let git = GitWorker::spawn(root.clone());

        let welcome = WelcomeState::new();
        let ext_index_refresh = Some(crate::mcp::registry_index::spawn_refresh());

        let (search_query_tx, search_query_rx) = std::sync::mpsc::channel();
        let (search_results_tx, search_results_rx) = std::sync::mpsc::channel();
        let (port_poll_tx, port_poll_rx) = std::sync::mpsc::channel();
        let (fleet_tx, fleet_rx) = std::sync::mpsc::channel();
        let (label_tx, label_rx) = std::sync::mpsc::channel();
        let (ssh_tx, ssh_rx) = std::sync::mpsc::channel();
        let search_root = root.clone();
        std::thread::spawn(move || {
            crate::widgets::search::search_worker_loop(
                search_root,
                search_query_rx,
                search_results_tx,
            );
        });
        // Pre-build the Cmd+P file index on a background thread so the
        // first Cmd+P keystroke does not stall the UI for the length
        // of a workspace walk. The result is delivered via channel;
        // open_file_finder calls poll_file_finder_index to consume it.
        let (file_finder_index_tx, file_finder_index_rx) =
            std::sync::mpsc::channel::<Vec<crate::widgets::file_finder::FileEntry>>();
        let index_root = root.clone();
        std::thread::spawn(move || {
            // A walk of the whole workspace, a home folder's included.
            crate::startup_gate::wait();
            let entries = crate::widgets::file_finder::build_file_index(&index_root);
            let _ = file_finder_index_tx.send(entries);
        });
        let fs_watch = FsWatch::spawn(&root, &tree);
        crate::startup_trace::mark("file watcher started");
        // Persisted layout preferences (Customize Layout popup). Skipped under
        // test so the suite never reads/writes the developer's ~/.config,
        // mirroring the theme load above.
        let layout_prefs = if cfg!(test) {
            crate::prefs::LayoutPrefs::default()
        } else {
            loaded_prefs.layout
        };
        let mut app = Self {
            tree,
            search,
            remote,
            source_control,
            commit_graph: crate::widgets::commit_graph::CommitGraphPanel::new(),
            graph_rx,
            graph_revspec: None,
            graph_tx,
            graph_fetch_inflight: false,
            graph_refetch_queued: false,
            run_debug,
            testing: crate::widgets::testing::TestingPanel::new(),
            codeql: crate::widgets::codeql::CodeqlPanel::new(),
            test_worker: crate::testing::worker::TestWorker::spawn(root.clone()),
            test_run_status: None,
            last_test_run: None,
            pane_run_statuses: Vec::new(),
            extensions,
            vscode_listed: Vec::new(),
            disabled_extensions,
            installed_extensions,
            // Built from `loaded_keymap` above, which applies the same
            // cfg!(test) guard main landed here independently: the
            // developer's real keybindings.json must never steer app tests.
            // Hoisted rather than inlined so the parse warnings can be
            // reported once, in OUTPUT · Keybindings.
            keymap: loaded_keymap,
            last_mouse_pos: None,
            tab_click_target: None,
            macro_recording: None,
            macro_last: None,
            macro_registers: crate::macros::load(&crate::macros::macros_path()),
            macro_replaying: false,
            macros_path: crate::macros::macros_path(),
            config_watch: crate::config_sync::ConfigWatch::new(config_watch_paths(
                &merged_settings.chain,
            )),
            snippets: load_all_snippets([root.as_path()]),
            format_on_save: loaded_prefs.format_on_save,
            default_formatter: loaded_prefs.default_formatter.clone(),
            language_formatters: loaded_prefs.language_formatters.clone(),
            foreign_formatters_noted: std::collections::BTreeSet::new(),
            language_prefs: loaded_prefs.languages.clone(),
            copy_on_select: loaded_prefs.copy_on_select,
            files_exclude: Vec::new(),
            files_associations: std::collections::BTreeMap::new(),
            files_associations_roots: Vec::new(),
            files_encoding: None,
            log_highlight: !loaded_prefs.disable_log_highlight,
            secret_redaction: !loaded_prefs.disable_secret_redaction,
            redaction_reveal_until: None,
            auto_save: loaded_prefs.auto_save,
            auto_save_on_focus_change: loaded_prefs.auto_save_on_focus_change,
            last_focus_signature: None,
            save_after_format: None,
            prettier_job: None,

            format_save_deadline: None,
            outline,
            open_editors,
            timeline,
            agent_lane_panel: crate::widgets::agent_lane::AgentLanePanel::new(),
            lane_change_counts: std::collections::HashMap::new(),
            dependencies,
            dep_ecosystems,
            explorer_views,
            settings_chain: merged_settings.chain,
            settings_provenance: merged_settings.provenance,
            timeline_rx,
            timeline_tx,
            codeql_size_rx,
            codeql_size_tx,
            codeql_size_pending: std::collections::HashSet::new(),
            history_done_rx,
            history_done_tx,
            project_check_rx,
            project_check_tx,
            project_check_running: false,
            project_check_rows: 0,
            project_hint_rx,
            project_hint_tx,
            project_check_hinted: false,
            problems_project_scope: if loaded_prefs.problems_project_scope.is_empty() {
                String::from("auto")
            } else {
                loaded_prefs.problems_project_scope.clone()
            },
            build_column_unit: crate::build_matchers::ColumnUnit::from_pref(
                &loaded_prefs.build_column_unit,
            ),
            last_problem_jump: None,
            test_jump_rx,
            test_jump_tx,
            timeline_fetched: None,
            nested_repo_scan: None,
            blame_rx,
            blame_tx,
            blame_fetched: None,
            blame_hover: None,
            git_net_job: None,
            git_view_job: None,
            vim_mode_pref: false,
            inline_blame_enabled: !loaded_prefs.disable_inline_blame,
            provenance_overlay: false,
            indent_guides_enabled: !loaded_prefs.disable_indent_guides,
            home_walks_rows: loaded_prefs.home_walks_wrapped_rows,
            bracket_colors_enabled: !loaded_prefs.disable_bracket_colors,
            whitespace_mode: crate::widgets::editor::WhitespaceMode::from_pref(
                &loaded_prefs.render_whitespace,
            ),
            rulers: crate::widgets::editor::normalize_rulers(&loaded_prefs.rulers),
            line_numbers: crate::widgets::editor::LineNumbers::from_pref(
                &loaded_prefs.line_numbers,
            ),
            diff_ws_default: crate::widgets::diff::DiffWhitespace::from_config(
                &loaded_prefs.diff_ignore_whitespace,
            ),
            inline_values_enabled: !loaded_prefs.disable_inline_values,
            save_before_debug: !loaded_prefs.disable_save_before_debug,
            auto_close_pairs: !loaded_prefs.disable_auto_close_pairs,
            paste_reindent: !loaded_prefs.disable_paste_reindent,
            save_cleanup: crate::widgets::editor::SaveCleanup::from_prefs(&loaded_prefs),
            inlay_hints_enabled: !loaded_prefs.disable_inlay_hints,
            // Keep the suite off the user's real ~/.config/croft/history: a
            // per-process temp root in test builds, the real dir otherwise.
            history_root: if cfg!(test) {
                std::env::temp_dir().join(format!("croft-test-history-{}", std::process::id()))
            } else {
                crate::history::history_dir()
            },
            history_restore: None,
            deps_rx,
            deps_tx,
            deps_fetched: None,
            voice_rx,
            voice_tx,
            voice_handle: None,
            voice_canceling: false,
            editor,
            editor_layout: editor_layout::EditorLayout::single(),
            force_save_armed: false,
            sudo_save_armed: None,
            pending_sudo_saves: Vec::new(),
            terminals: vec![term],
            active_terminal: 0,
            perf: PerfHud::default(),
            screencast: Screencast::default(),
            roots: crate::workspace::WorkspaceRoots::single(root.clone()),
            sidebar_view: SidebarView::Explorer,
            sidebar_areas: SidebarAreas::default(),
            // Read the persisted theme from the real config only outside tests;
            // under test fall back to the default so the suite stays hermetic
            // and never depends on (nor mutates) the developer's ~/.config. This
            // mirrors the cfg!(test) write-skip in `apply_theme`.
            theme: if cfg!(test) {
                crate::theme::Theme::default()
            } else {
                loaded_prefs.theme()
            },
            focus: Pane::Tree,
            show_tree: true,
            show_terminal: true,
            terminal_maximized: false,
            activity_bar_visible: layout_prefs.activity_bar,
            status_bar_visible: layout_prefs.status_bar,
            side_bar_position: layout_prefs.side_bar_position,
            secondary_side_bar_visible: layout_prefs.secondary_side_bar,
            panel_alignment: layout_prefs.panel_alignment,
            quick_input_position: layout_prefs.quick_input_position,
            zen_mode: false,
            centered_layout: layout_prefs.centered_layout,
            zen_center_layout: layout_prefs.zen_center_layout,
            zen_centered: false,
            centered_layout_width: layout_prefs.centered_layout_width,
            zen_esc_at: None,
            zen_hint_until: None,
            sidebar_auto_hide: loaded_prefs.sidebar_auto_hide,
            sidebar_auto_hide_suspended: false,
            sidebar_pinned_open: false,
            sidebar_dwell: SidebarDwell::default(),
            pre_zen: None,
            layout_icon_areas: LayoutIconAreas::default(),
            status: match (
                merged_settings.user_config_error.as_deref(),
                hook_bind_error,
            ) {
                (Some(error), _) => settings_not_loaded_status(error),
                (None, Some(hook)) if view_bind_error.is_none() => hook,
                _ => view_status(view_bind_error.as_deref()),
            },
            persistence_warning: remote_persistence_status(
                is_remote_session(),
                std::env::var_os("CROFT_SESSION_PERSISTENT").is_some(),
            ),
            quit: false,
            drop_to_local: false,
            is_remote: is_remote_session(),
            context_menu: None,
            crumb_filter: (String::new(), Vec::new()),
            prompt: None,
            fs_watch,
            fs_watch_extra: Vec::new(),
            git_extra: Vec::new(),
            active_scm_root: root.clone(),
            scm_pin: None,
            scm_follow_root: root.clone(),
            active_test_root: root.clone(),
            test_root_follow: None,
            pending_workspace_folders: None,
            git_head_oids: std::collections::BTreeMap::new(),
            git,
            cursor_blink: CursorBlink::new(),
            overwrite_mode: false,
            caret_block_sent: false,
            editor_click: ClickTracker::default(),
            tree_click: ClickTracker::default(),
            terminal_click: ClickTracker::default(),
            modified_click: ModifiedClickTracker::default(),
            ports_click: ClickTracker::default(),
            terminal_select_autoscroll: None,
            scrollbar_drag: None,
            editor_hscrollbar_drag: false,
            breakpoint_lane_press: false,
            editor_line_drag: None,
            outline_scrollbar_drag: false,
            secondary_outline: Default::default(),
            secondary_outline_scrollbar_drag: false,
            open_editors_scrollbar_drag: false,
            timeline_scrollbar_drag: false,
            deps_scrollbar_drag: false,
            testing_scrollbar_drag: false,
            graph_scrollbar_drag: false,
            welcome,
            ext_index_refresh,
            codeql_program: PathBuf::from("codeql"),
            codeql_run: None,
            codeql_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            codeql_run_name: String::new(),
            codeql_run_queue: std::collections::VecDeque::new(),
            codeql_batch: (0, 0),
            codeql_multi_db: None,
            codeql_upgrade: None,
            codeql_version: None,
            codeql_log_summary: None,
            codeql_log_viewer: None,
            codeql_model_job: None,
            codeql_perf_compare: None,
            codeql_query_help: None,
            codeql_pack_job: None,
            codeql_db_job: None,
            codeql_ast_job: None,
            codeql_doc_job: None,
            codeql_results_walk: None,
            codeql_cli_setting: loaded_prefs.codeql_cli_path.clone(),
            codeql_cli_source: crate::codeql_cli::Source::Fallback,
            codeql_cli_latest: None,
            codeql_cli_download: None,
            codeql_cli_check: None,
            codeql_cli_net: crate::codeql_cli::Net::default(),
            codeql_code_search: None,
            codeql_variant_submit: None,
            codeql_variant_poll: None,
            codeql_variant_polled: None,
            codeql_variant_results: None,
            codeql_variant_export: None,
            codeql_queries_job: None,
            ext_index_manual_refresh: false,
            search_query_tx,
            search_results_rx,
            cell_pixel: None,
            chrome_bake: None,
            chrome_shown: None,
            chrome_bake_next: None,
            welcome_bake: None,
            last_pdf_wheel: None,
            formula_overwrite_armed: false,
            scm_change_badge: None,
            scm_change_badge_count: 0,
            explorer_unsaved_badge: None,
            explorer_unsaved_count: 0,
            remote_ports_badge: None,
            remote_ports_count: 0,
            testing_badge: None,
            testing_badge_count: 0,
            problems_badge_count: 0,
            problems_badge_cell: None,
            inline_protocol: crate::iterm2_inline::detect_inline_image_protocol(),
            sixel_supported: false,
            resize_repaint_needed: false,
            minimap_visible: layout_prefs.minimap,
            minimap_side: MinimapSide::Right,
            minimap_base: None,
            minimap_img_rect: Rect::default(),
            minimap_content_h: 0,
            minimap_baked_at: None,
            minimap_edit_pending: false,
            minimap_text: false,
            minimap_text_cache: None,
            minimap_room: true,
            drawn_buffer: None,
            chrome_sent: std::collections::HashMap::new(),
            chrome_seen: std::collections::HashSet::new(),
            minimap_drag: false,
            clipboard_reader: read_system_clipboard,
            is_relay_session: !cfg!(test) && std::env::var_os("CROFT_RELAY_KEY").is_some(),
            remote_launch: None,
            pending_remote_launch_host: None,
            pending_remote_launch_path: None,
            ssh_offer: None,
            ssh_offer_seen: std::collections::BTreeMap::new(),
            ssh_rx,
            ssh_tx,
            remote_offer_disabled: loaded_prefs.disable_remote_offer,
            remote_offer_excluded: loaded_prefs.remote_offer_excluded_hosts.clone(),
            fleet_groups: loaded_prefs.fleet_groups.clone(),
            code_scanning: loaded_prefs.code_scanning,
            code_scan_seen: None,
            code_scan_list: None,
            code_scan_fetch: None,
            remote_offer_refused: crate::remote::load_refused_hosts(
                &crate::remote::refused_hosts_path(&croft_cache_dir()),
                std::time::SystemTime::now(),
            ),
            lane_agent: loaded_prefs.lane_agent.clone(),
            color_depth: loaded_prefs
                .color_depth
                .as_deref()
                .and_then(crate::color_depth::ColorDepth::from_setting)
                .unwrap_or_else(|| {
                    crate::color_depth::ColorDepth::detect(|name| std::env::var(name).ok())
                }),
            lane_panes: std::collections::BTreeMap::new(),
            tree_clipboard: None,
            tree_typeahead: None,
            compare_anchor: None,
            last_frame_area: Rect::default(),
            last_sidebar_area: None,
            tree_drag: None,
            pending_scp_uploads: Vec::new(),
            pending_remote_pulls: Vec::new(),
            view_listener,
            hook_listener,
            hook_root: root.clone(),
            approvals: std::collections::VecDeque::new(),
            approval_check: None,
            approval_ui: None,
            approval_edit: None,
            approval_merge_into: None,
            notebook_kernels: std::collections::HashMap::new(),
            kernel_listing: None,
            kernel_choice: None,
            coverage_at: None,
            coverage_lens_path: None,
            coverage_install: None,
            view_drain_budget: std::time::Duration::from_millis(20),
            pending_local_open: None,
            pending_discard: None,
            pending_unsaved_scm: None,
            remote_tag_root: None,
            scm_unsaved_cleared: false,
            scm_after_format_save: None,
            pending_revert_hunk: None,
            pending_unsaved: None,
            pending_lossy_save: None,
            remote_handoff_ok: None,
            hot_exit_dir: data_dir.as_deref().map(|dir| dir.join("hot-exit")),
            hot_exit_written: None,
            hot_exit_retry_at: None,
            hot_exit_consumed: Vec::new(),
            hot_exit_unremoved: Vec::new(),
            exit_notes: Vec::new(),
            session_dir: data_dir.as_deref().map(|dir| dir.join("sessions")),
            saved_session: None,
            closed_bookmarks: std::collections::HashMap::new(),
            run_tasks: Vec::new(),
            last_task: None,
            pending_terminal_warning: false,
            commit_menu_open: false,
            default_branch_label: None,
            default_branch_ask: None,
            trust_local_browser: false,
            sidebar_width: SIDEBAR_WIDTH_DEFAULT,
            terminal_height: None,
            splitter_drag: None,
            editor_seams: Vec::new(),
            editor_layout_area: Rect::default(),
            sidebar_splitter_x: None,
            terminal_splitter_y: None,
            terminal_add_buttons: Vec::new(),
            terminal_profile_buttons: Vec::new(),
            terminal_close_buttons: Vec::new(),
            terminal_collapse_buttons: Vec::new(),
            consented_extensions: crate::prefs::Prefs::load_or_default().mcp_consented,
            config_dir: crate::paths::config_dir(),
            settings_profile: if cfg!(test) {
                None
            } else {
                crate::profiles::active()
            },
            scrollback_dir: crate::session_state::dirs_cache_croft().join("scrollback"),
            terminal_strip_rects: Vec::new(),
            terminal_focus_button: Rect::default(),
            panel_maximize_button: Rect::default(),
            terminal_rail_rects: Vec::new(),
            terminal_rail_area: Rect::default(),
            terminal_rail_scroll: 0,
            terminal_rail_revealed: None,
            terminal_label_rects: Vec::new(),
            terminal_drag_from: None,
            editor_tab_drag_from: None,
            terminal_pointer_forwarded: None,
            terminal_pane_maximized: false,
            bottom_panel_tab: BottomPanelTab::Terminal,
            problems: crate::widgets::problems::ProblemsPanel::new(),
            output: crate::widgets::output::OutputPanel::new(),
            ports: crate::widgets::ports::PortsPanel::new(),
            captures: crate::widgets::captures::CapturesPanel::new(),
            port_toast: None,
            last_port_poll: std::time::Instant::now(),
            port_poll_rx,
            fleet_rx,
            fleet_tx,
            fleet_running: false,
            fleet_command: String::new(),
            port_poll_tx,
            port_poll_inflight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_label_refresh: std::time::Instant::now(),
            label_rx,
            label_tx,
            label_inflight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            problems_tab_rect: Rect::default(),
            output_tab_rect: Rect::default(),
            ports_tab_rect: Rect::default(),
            captures_tab_rect: Rect::default(),
            terminal_tab_rect: Rect::default(),
            pointer_cell: None,
            hovered_activity_icon: None,
            hovered_layout_icon: None,
            osk: None,
            osk_auto: crate::iterm2_inline::detect_osk_auto(),
            last_content_width: 0,
            last_content_height: 0,
            lsp: match crate::lsp::LspManager::new(root.clone()) {
                Ok(m) => Some(m),
                Err(e) => {
                    eprintln!("lsp manager init failed: {e}");
                    None
                }
            },
            lsp_last_seen: std::collections::HashMap::new(),
            shell_lint_notice_shown: false,
            tool_search_path: std::env::var_os("PATH"),
            semantic_generation_seen: std::collections::HashMap::new(),
            problems_open_set: std::collections::BTreeSet::new(),
            lsp_diagnostics: std::collections::HashMap::new(),
            markdown_lint_last_seen: std::collections::HashMap::new(),
            live_run_files: std::collections::HashSet::new(),
            live_run_sent: std::collections::HashMap::new(),
            live_run_runner: None,
            md_scroll_synced: None,
            code_lens_enabled: true,
            function_breakpoints: Vec::new(),
            screen_reader: loaded_prefs.screen_reader,
            screen_reader_command: loaded_prefs.screen_reader_command.clone(),
            announcer: crate::a11y::Announcer::default(),
            announce_pos: None,
            a11y_event: None,
            debug_pause_reason: None,
            term_suggest_enabled: true,
            term_suggestion: None,
            recording_shortcut: None,
            confirming_shortcut: None,
            inline_enabled: false,
            inline_worker: None,
            inline_config: None,
            inline_job: None,
            inline_lsp: None,
            inline_asked: None,
            inline_next_id: 0,
            keybindings_file: if cfg!(test) {
                std::env::temp_dir().join(format!(
                    "croft-test-keybindings-{}-{}.json",
                    std::process::id(),
                    TEST_KEYBINDINGS_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                ))
            } else {
                crate::keymap::keybindings_path()
            },
            search_editor_job: None,
            code_lens_requested: std::collections::HashMap::new(),
            sarif_diag_signature: Vec::new(),
            sarif_added_polled: None,
            sarif_step_signature: None,
            sarif_published: Vec::new(),
            lsp_progress: std::collections::HashMap::new(),
            lsp_questions: std::collections::VecDeque::new(),
            lsp_question: None,
            completion_popup: None,
            editor_vim_chord: EditorVimChord::default(),
            cmd_k_leader: None,
            kitty_keys: false,
            legacy_ctrl_punct: false,
            vim: crate::vim::VimState::new(),
            vim_last_find: None,
            vim_visual_line: false,
            shortcuts_modal: None,
            settings_editor: None,
            shortcuts_hit_rect: None,
            status_diag_rect: Rect::default(),
            status_message_rect: Rect::default(),
            status_logged: String::new(),
            status_wide: false,
            status_codeql_rect: Rect::default(),
            status_update_rect: Rect::default(),
            status_offer_rect: Rect::default(),
            status_indent_rect: Rect::default(),
            status_encoding_rect: Rect::default(),
            status_eol_rect: Rect::default(),
            status_language_rect: Rect::default(),
            connect_dialog: None,
            connect_auth: None,
            install_session: None,
            update_watch: None,
            lsp_paused_for_build: false,
            build_marker_checked: None,
            drift_probe: None,
            local_drift: None,
            self_install: None,
            update_check: None,
            manual_update_check: None,
            update_fetch: if cfg!(test) {
                || Err(String::from("tests never ask the network"))
            } else {
                crate::update_check::fetch_latest_checked
            },
            update_toast: None,
            install_source: crate::update_check::current_install_source(),
            staged_install: None,
            staged_status: UpdateStatus::Idle,
            update_status: UpdateStatus::Idle,
            update_spinner_start: None,
            pending_reexec: false,
            overlays: {
                let mut overlays = OverlayManager::default();
                overlays.welcome.mark_dirty();
                overlays.hero.mark_dirty();
                overlays.activity.mark_dirty();
                overlays
            },
            editor_find: None,
            terminal_find: None,
            terminal_find_pane: 0,
            terminal_find_match: None,
            terminal_quick_select: None,
            // Under test, both durable stores point at per-process scratch
            // files so app tests that run commands or split panes can never
            // touch (or race on) the user's real history / session files;
            // tests that exercise the stores inject their own paths.
            command_history: crate::command_history::CommandHistory::load(&if cfg!(test) {
                std::env::temp_dir().join(format!(
                    "croft-test-command-history-{}.jsonl",
                    std::process::id()
                ))
            } else {
                crate::command_history::history_path()
            }),
            command_history_popup: None,
            terminal_session_dirty: false,
            terminal_store_error: None,
            terminal_restore_respawns: crate::remote::running_over_ssh(),
            terminal_session_path: if cfg!(test) {
                std::env::temp_dir().join(format!(
                    "croft-test-terminal-sessions-{}.json",
                    std::process::id()
                ))
            } else {
                crate::terminal_session::path()
            },
            workspace_folders_path: if cfg!(test) {
                std::env::temp_dir().join(format!(
                    "croft-test-workspace-folders-{}.json",
                    std::process::id()
                ))
            } else {
                crate::workspace::folders_store_path()
            },
            terminal_copy_mode: None,
            triggers: load_trigger_set(!loaded_prefs.disable_secret_redaction),
            agents: crate::agents::AgentTable::load(&crate::agents::agents_path()),
            agent_events: std::collections::VecDeque::new(),
            status_agents_rect: Rect::default(),
            status_position_rect: Rect::default(),
            matchers,
            watch_set,
            task_matcher_by_pane: std::collections::HashMap::new(),
            watch_published_panes: std::collections::HashSet::new(),
            broadcast_input: false,
            pending_broadcast_enable: false,
            pending_terminal_paste: None,
            multiline_paste_warning: loaded_prefs.terminal_multiline_paste_warning,
            pending_run_block: None,
            pending_captures: Vec::new(),
            block_outputs: std::collections::HashMap::new(),
            show_terminal_timestamps: false,
            notifier: crate::notifications::Notifier::new(&loaded_prefs.notifications),
            host_accents: compile_host_accents(&loaded_prefs.host_accents),
            closed_terminals: Vec::new(),
            closed_tabs: Vec::new(),
            file_finder: None,
            quick_open_range: None,
            command_palette: None,
            go_to_symbol: None,
            workspace_symbols: None,
            ws_symbols_request_id: None,
            ws_symbols_pending: None,
            mcp_rx: None,
            mcp_started: None,
            mcp_busy_label: None,
            process_picker: None,
            debug_sessions: Default::default(),
            debug_configs: Vec::new(),
            debug_config_draft: None,
            debug_compounds: Vec::new(),
            selected_debug_config: None,
            selected_debug_compound: None,
            debug_stop_all: false,
            debug_post_tasks: Vec::new(),
            debug_compound: None,
            pending_debug_launch: None,
            input_run: None,
            debug_build: None,
            pending_attach: None,
            pending_test_debug: None,
            pending_cargo_launches: Vec::new(),
            debug_expanded: std::collections::HashSet::new(),
            watch_exprs: Vec::new(),
            watch_vals: std::collections::HashMap::new(),
            watch_prev: std::collections::HashMap::new(),
            watch_baseline_pending: false,
            debug_copy_pending: None,
            debug_console: Vec::new(),
            debug_console_dropped: 0,
            debug_ever_stopped: false,
            zoxide_jump: None,
            zoxide_add_folder: false,
            branch_picker: None,
            branch_purpose: BranchPurpose::Checkout,
            blocked_switch: None,
            scm_menu: crate::widgets::scm_menu::ScmMenuState::default(),
            input_prompt: None,
            list_picker: None,
            session_channel: crate::session_host::InnerChannel::from_env(),
            session_participants: Vec::new(),
            session_host_stale_seen: false,
            session_stale_notice: None,
            session_typist: None,
            session_carets: std::collections::HashMap::new(),
            session_presence_mtime: None,
            last_session_presence_poll: std::time::Instant::now(),
            collab: None,
            collab_config: crate::collab::CollabChannel::env_config(),
            last_collab_connect: None,
            collab_offline_base: std::collections::HashMap::new(),
            collab_carets: std::collections::HashMap::new(),
            collab_caret_sent: None,
            collab_stream: None,
            pair_host: None,
            pair_record_path: crate::session::pair_record_path(&root),
            pair_socket: crate::session::collab_socket_path(&root),
            pair_host_lock_path: crate::session::pair_host_lock_path(&root),
            last_pair_check: None,
            navigator_notes: std::collections::HashMap::new(),
            pair_commentary_buf: String::new(),
            pair_turn_origin: None,
            pair_notes_this_turn: 0,
            pair_last_noted_file: None,
            pair_host_lock: None,
            navigator_down: false,
            last_pair_record_raw: None,
            pair_lock_denied: None,
            pair_noted_files: std::collections::BTreeSet::new(),
            proactive_navigator_enabled: !loaded_prefs.disable_proactive_navigator,
            proactive_scanned: std::collections::HashMap::new(),
            #[cfg(test)]
            pair_spawn_override: None,
            #[cfg(test)]
            kernel_start_override: None,
            collab_labels_visible: false,
            git_output_log: Vec::new(),
            build_diagnostics: std::collections::HashMap::new(),
            build_diag_files_by_pane: std::collections::HashMap::new(),
            undo_pushed_commit_armed: None,
            reset_hard_armed: None,
            pending_discard_all: false,
            pending_stash_drop: None,
            pending_replace_all: None,
            file_finder_index: None,
            file_finder_index_rx: Some(file_finder_index_rx),
            file_finder_index_dirty: false,
            completion_request_id: None,
            completion_origin: None,
            completion_ask_pos: None,
            completion_items_pos: None,
            completion_incomplete: false,
            completion_reask_due: false,
            completion_ask_due: None,
            completion_explicit: false,
            quick_suggestions: !loaded_prefs.disable_quick_suggestions,
            completion_words: false,
            local_completion_seq: u64::MAX,
            completion_resolve: None,
            word_suggestions: crate::word_suggest::WordSuggestions::from_pref(
                &loaded_prefs.word_based_suggestions,
            ),
            signature_help_popup: None,
            signature_help_request_id: None,
            signature_help_anchor: None,
            rename_request_id: None,
            rename_baseline: Vec::new(),
            pending_file_move: None,
            prepare_rename_request: None,
            format_request_id: None,
            format_request_seq: None,
            format_request_selection: false,
            format_on_type: loaded_prefs.format_on_type,
            on_type_request: None,
            code_action_request_id: None,
            code_action_pending_resolve: false,
            code_action_baseline: Vec::new(),
            pending_code_actions: Vec::new(),
            review_boxes: None,
            review_pending: Vec::new(),
            review_pending_pr: None,
            review_submitting: false,
            notes: notes_path
                .as_deref()
                .map(crate::sticky_notes::Notes::load)
                .unwrap_or_default(),
            notes_path,
            note_box_ids: std::collections::HashMap::new(),
            notes_shown_gen: 0,
            review_export: None,
            review_export_sticky: Vec::new(),
            review_export_pr: None,
            review_ai_prefix: loaded_prefs
                .review_ai_prefix
                .clone()
                .unwrap_or_else(|| String::from(crate::review_threads::DEFAULT_AI_PREFIX)),
            review_pr: None,
            review_pr_branch: None,
            review_nodes: std::collections::HashMap::new(),
            review_verdict: None,
            review_gh: String::from("gh"),
            pr_gh: None,
            pr_checks_polled: None,
            pr_viewed_fetch: None,
            pr_viewed_queue: std::collections::VecDeque::new(),
            pr_viewed_write: None,
            pr_viewed_seq: 0,
            pr_viewed_touched: std::collections::HashMap::new(),
            pr_checkout: None,
            pr_checkout_rx: None,
            review_tx,
            review_rx,
            recording: None,
            recorded_size: (0, 0),
            scrubber: None,
            scrub_view: None,
            scrub_for: None,
            outline_scrub_key: None,
            auto_approve: None,
            scrub_slider: Rect::default(),
            scrub_dragging: false,
            scrub_cache: std::collections::HashMap::new(),
            scrub_trees: std::collections::HashMap::new(),
            scrub_names: std::collections::HashMap::new(),
            scrub_builder: None,
            scrub_views: std::collections::VecDeque::new(),
            scrub_plain: std::collections::VecDeque::new(),
            scrub_outlines: std::collections::HashMap::new(),
            scrub_view_key: None,
            tour: None,
            tour_done: loaded_prefs.tour_done,
            welcome_tour_button: Rect::default(),
            gh_program: PathBuf::from("gh"),
            dismiss_target: None,
            sarif_locate_pending: None,
            sarif_download: None,
            sarif_fetch: fetch_sarif_source,
            debug_temp_breakpoint: None,
            debug_temp_note: None,
            agent_ledger: crate::agent_lane::AgentLedger::new(),
            agent_ledger_store: (!cfg!(test)).then(agent_ledger_store_path),
            agent_ledger_saved: (0, std::time::Instant::now()),
            pending_lane_picks: Vec::new(),
            pending_bookmarks: Vec::new(),
            problems_fix_started: None,
            http_run: None,
            http_environment: None,
            pending_color_presentations: Vec::new(),
            pending_color_context: None,
            color_presentations_request: None,
            hover: HoverDwell::default(),
            hover_popup: None,
            hover_request_id: None,
            rust_analyzer_request: None,
            hover_anchor: None,
            hover_word: None,
            hover_diagnostic: None,
            hover_at_caret: None,
            hover_doc: (None, 0),
            tab_hover: HoverDwell::default(),
            tab_tooltip: None,
            tab_hover_idx: None,
            ui_hover: HoverDwell::default(),
            ui_tooltip: None,
            ui_tooltip_label: None,
            search_hint_view: (0, 0, false),
            ui_tooltip_area: None,
            update_card_rect: None,
            definition_request_id: None,
            peek_definition_request_id: None,
            peek_popup: None,
            peek_target: None,
            peek_refs: None,
            references_want_peek: false,
            outline_synced: None,
            outline_parse: None,
            outline_parsed_for: None,
            outline_lsp_landed: None,
            declaration_request_id: None,
            type_definition_request_id: None,
            implementation_request_id: None,
            references_request_id: None,
            call_hierarchy_request_id: None,
            occurrences_request: None,
            linked_request: None,
            linked_observed: None,
            linked_observed_at: std::time::Instant::now(),
            linked_asked: false,
            selection_range_request: None,
            occ_observed: None,
            occ_observed_at: std::time::Instant::now(),
            nav: NavHistory::default(),
        };
        crate::startup_trace::mark("editor state built");
        // Initialise the per-pane focus/gradient flags to match the starting
        // pane (Tree) and persisted theme. Without this, the explorer boots
        // focused-but-not-gradient: the Black-theme gradient border only
        // arms on the first focus change because nothing ran the sync at
        // construction time.
        // #1286: start in Vim mode when it was saved as the default.
        app.follow_vim_mode_pref(loaded_prefs.vim_mode);
        // #256: the PROBLEMS scope comes from prefs; set it after
        // construction so the panel keeps one plain `new()`.
        app.problems.scope =
            crate::widgets::problems::ProblemScope::from_config(&loaded_prefs.problems_scope);
        // #578: the Testing view runs CodeQL tests with the same `codeql`
        // as the side bar, from the first tree click on. Tests keep plain
        // `codeql` rather than whatever the machine has installed.
        if cfg!(test) {
            app.test_worker
                .set_codeql_program(app.codeql_program.clone());
        } else {
            app.resolve_codeql_cli();
        }
        // And finds runners among the extensions the app has added: CodeQL's
        // comes with the CodeQL extension (#1842).
        app.test_worker.set_extensions_dir(app.extensions_dir());
        app.sync_focus_flags();
        // Seed the highlighter with the persisted theme's code palette so a
        // file opened before the first theme switch already highlights in the
        // active theme's colors (not the Base16 default).
        crate::highlight::set_syntax_palette(app.theme.syntax());
        // The agent review queue as this workspace last left it (#345).
        let root = app.workspace_root().to_path_buf();
        app.apply_project_excludes(&loaded_prefs);
        app.tree
            .set_compact_folders(!loaded_prefs.disable_compact_folders);
        app.apply_file_associations(&loaded_prefs);
        app.apply_files_encoding(&loaded_prefs);
        app.watch_project_snippets();
        app.agent_ledger = app.load_agent_ledger(&root);
        app.sync_agent_lane_decorations();
        Ok(app)
    }

    /// Post-draw flush of the activity-bar icons. Re-emits the pre-encoded
    /// OSC-1337 image bytes each frame (ratatui doesn't track image cells, so
    /// neighbouring SGR traffic can evict them from iTerm2's cache) — but
    /// first applies the same emit-time move-detection as the run-debug
    /// headline icon: if the icon cells shifted since the last emit (a
    /// terminal resize, or any future layout change that recenters the bar),
    /// a plain re-emit stacks a fresh image on top of the one iTerm2 still
    /// caches at the old cell, the doubled-icon ghost. On a move it arms one
    /// `terminal.clear()` (the proven `\x1b[2J` eviction the main loop folds
    /// into its OR chain), forgets the emit positions, and skips painting
    /// this frame; the next frame clears, full-redraws, and re-emits a single
    /// clean set at the new cells.
    pub fn flush_activity_image_overlays(&mut self, around: &[u64]) {
        use std::io::Write;
        // The keepalive re-emit fights iTerm2's image-cache eviction. Neither
        // the sixel cell buffer nor Kitty's image layer evicts, so there the
        // icons re-emit only when dirty: on Kitty the keepalive cost about
        // 13 KB every two seconds, idle, over SSH (#682).
        let dirty = self.overlays.activity.dirty();
        let allow_keepalive = self.activity_keepalive_allowed()
            && (0..around.len()).any(|i| self.overlays.activity.neighbourhood_changed(i, around));
        if !self.overlays.activity.should_refresh(allow_keepalive) {
            return;
        }
        let mut overlays = self.pending_activity_image_overlays();
        let positions: Vec<(u16, u16)> = overlays.iter().map(|(cell, _)| *cell).collect();
        // Checked before the empty return below: an update card over every
        // icon left nothing to send, and Kitty kept the old ones over it.
        if self.overlays.activity.positions_moved(&positions) {
            self.overlays.activity.request_clear();
            self.overlays.activity.forget_positions();
            self.overlays.activity.forget_sent();
            return;
        }
        if overlays.is_empty() {
            return;
        }
        let hashes: Vec<u64> = overlays
            .iter()
            .map(|(_, seq)| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                seq.hash(&mut h);
                h.finish()
            })
            .collect();
        let sent: Vec<((u16, u16), u64)> = positions.iter().copied().zip(hashes).collect();
        // Everything goes out after a wipe or on the first frame. Otherwise
        // a dirty flush sends only the slots whose cell or image changed,
        // and a keepalive only the icons whose surroundings changed (`around`
        // is in the order of the unchanged positions).
        if !self.overlays.activity.nothing_sent() {
            let same = positions.as_slice() == self.overlays.activity.last_positions();
            let mut i = 0;
            overlays.retain(|_| {
                let keep = if dirty {
                    !self.overlays.activity.was_sent(sent[i].0, sent[i].1)
                } else {
                    same && self.overlays.activity.neighbourhood_changed(i, around)
                };
                i += 1;
                keep
            });
        }
        if overlays.is_empty() {
            self.overlays.activity.mark_emitted();
            self.overlays.activity.store_positions(positions);
            self.overlays.activity.set_neighbourhood(around.to_vec());
            self.overlays.activity.set_sent(sent);
            return;
        }
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        for ((x, y), seq) in &overlays {
            // Same screen bound as the image writers (#513): a raw
            // absolute-CUP write that ratatui cannot repair, so the origin
            // is checked before it is used.
            //
            // 1x1 is NARROWER than some payloads here, and knowingly so.
            // `pending_activity_image_overlays` discards each block's
            // `Rect` and returns only its origin, and a count badge is 2-3
            // cells wide (`count_badge_cells_w`), so a badge whose left
            // edge is on screen and right edge is not would pass this.
            // Not reachable today -- the badges anchor to the left-hand
            // activity bar -- and making it exact means returning the
            // `Rect` from that function, which is a wider change than this
            // guard. Recorded rather than left to be rediscovered.
            if !self.overlay_fits_on_screen(*x, *y, 1, 1) {
                continue;
            }
            let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1); // 1-based
            let _ = out.write_all(seq.as_bytes());
        }
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
        self.overlays.activity.mark_emitted();
        self.overlays.activity.store_positions(positions);
        self.overlays.activity.set_neighbourhood(around.to_vec());
        self.overlays.activity.set_sent(sent);
    }

    /// Reset the blink phase so the caret is solidly visible for the next
    /// 530ms. Call after any edit, cursor movement, or focus change so the
    /// user always sees where the cursor just landed before it starts to
    /// blink off.
    fn poke_cursor(&mut self) {
        self.cursor_blink.poke();
    }

    fn sync_open_file_poll_mtime(&mut self) {
        let paths = self.polled_open_file_paths();
        self.fs_watch.sync_open_file_mtime(&paths);
    }

    /// The repository toplevel owning the workspace, from the git worker's
    /// last status reply — the only correct base for porcelain-derived
    /// paths and `HEAD:<rel>` pathspecs, which git resolves against the
    /// toplevel however `-C` is set (#139). Identical to the workspace
    /// root whenever that root IS the toplevel; falls back to it outside
    /// a repo (where the ops fail with their own message) and before the
    /// first status reply lands.
    /// The primary workspace root — what `workspace_root` the field meant
    /// before the set model (#143). Single-root semantics throughout.
    pub fn workspace_root(&self) -> &Path {
        self.roots.primary()
    }

    fn scm_root(&self) -> PathBuf {
        let ws = self.active_scm_root.clone();
        self.git_worker_for_root(&ws)
            .status()
            .repo_root
            .clone()
            .unwrap_or(ws)
    }

    /// The workspace root the focus-following surfaces resolve against:
    /// the root owning the focused editor's file, else the primary
    /// (#149 git, #151 testing/tasks/run).
    fn active_workspace_root(&self) -> PathBuf {
        self.editor
            .path
            .as_deref()
            .and_then(|p| self.roots.owning_root(p))
            .unwrap_or_else(|| self.roots.primary())
            .to_path_buf()
    }

    /// Every workspace folder as `(name, root)`, for a compound member's
    /// `"folder"` and `${workspaceFolder:name}` (#1425).
    fn workspace_folders(&self) -> Vec<(String, PathBuf)> {
        crate::dap::configs::workspace_folders(self.roots.iter())
    }

    /// Every open buffer's path, across the active leaf and every inactive
    /// one. `App::editor` is the hoisted active leaf and `inactive_leaf_tabs`
    /// walks the rest, so together they cover each leaf exactly once.
    fn open_buffer_paths(&self) -> std::collections::BTreeSet<PathBuf> {
        self.editor
            .editors
            .iter()
            .chain(
                self.editor_layout
                    .inactive_leaf_tabs()
                    .into_iter()
                    .flat_map(|g| g.editors.iter()),
            )
            .filter_map(|e| e.path.clone())
            .collect()
    }

    fn cycle_focus(&mut self) {
        // Skip hidden panes when cycling.
        let mut next = self.focus;
        for _ in 0..3 {
            next = match next {
                Pane::Tree => Pane::Editor,
                Pane::Editor => Pane::Terminal,
                Pane::Terminal => Pane::Tree,
            };
            if self.pane_visible(next) {
                break;
            }
        }
        // Through `focus_pane`, never a bare assignment: every route into a
        // pane shares its guarantees (terminal find/copy-mode teardown, and
        // restoring the editor from under a maximized terminal).
        self.focus_pane(next);
    }

    fn pane_visible(&self, p: Pane) -> bool {
        match p {
            Pane::Tree => self.show_tree,
            Pane::Terminal => self.show_terminal,
            Pane::Editor => true,
        }
    }

    pub fn terminal(&self) -> &PtyTerminal {
        &self.terminals[self.active_terminal]
    }

    pub fn terminal_mut(&mut self) -> &mut PtyTerminal {
        &mut self.terminals[self.active_terminal]
    }

    /// Whether a modal overlay owns the screen right now. The same eleven-way
    /// check was repeated verbatim at three overlay-flush sites; auto-hide
    /// (#260) needs it too, so it lives here once.
    fn modal_overlay_open(&self) -> bool {
        self.approval_ui.is_some()
            || self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.command_palette.is_some()
            || self.go_to_symbol.is_some()
            || self.workspace_symbols.is_some()
            || self.process_picker.is_some()
            || self.zoxide_jump.is_some()
            || self.command_history_popup.is_some()
            || self.branch_picker.is_some()
            || self.input_prompt.is_some()
            || self.list_picker.is_some()
    }

    fn focus_pane(&mut self, p: Pane) {
        // Leaving the terminal closes its find bar so a stale match highlight
        // never lingers on the grid once input goes elsewhere. Quick-select
        // labels are torn down for the same reason.
        if p != Pane::Terminal && self.terminal_find.is_some() {
            self.close_terminal_find();
        }
        if p != Pane::Terminal && self.terminal_quick_select.is_some() {
            self.close_terminal_quick_select();
        }
        if p != Pane::Terminal && self.terminal_copy_mode.is_some() {
            self.close_terminal_copy_mode();
        }
        // A maximized terminal gives the editor zero rows; focusing the
        // editor is always an intent to use it, so restore it (VS Code's
        // convention when a file is revealed over a maximized panel).
        // `terminal_pane_maximized` (width, among terminal panes) is
        // unrelated and stays.
        if p == Pane::Editor && self.terminal_maximized {
            self.terminal_maximized = false;
        }
        // Auto-hide (#260): moving into the editor or a terminal collapses
        // the sidebar; moving INTO the sidebar releases the pin a reveal
        // command took, so the next move out can collapse it again.
        match p {
            Pane::Tree => {
                self.sidebar_pinned_open = false;
                // Focus came back inside the grace window: the click was
                // passing through, or landed here. Cancel the pending
                // collapse (#302) rather than letting it fire under the user.
                self.sidebar_dwell.disarm();
            }
            Pane::Editor | Pane::Terminal => self.maybe_auto_hide_sidebar(),
        }
        self.focus = p;
        self.sync_focus_flags();
        if self.editor.focused {
            self.poke_cursor();
        }
    }

    fn sync_focus_flags(&mut self) {
        // Focus must never rest on a COLLAPSED pane (#313). A folded pane is
        // one column wide and is never rendered, so it paints no cursor and no
        // focus border: the user would be typing into a pane they cannot see
        // receiving it.
        //
        // Enforced HERE rather than at each gesture. The first attempt guarded
        // `cycle_terminal` alone, which left closing the pane beside a folded
        // one, the maximize rail, and a right-click on a strip - the last of
        // which that same round OPENED, since making a strip right-clickable
        // also made it focusable. A fix at the call site closes the path you
        // were looking at; a fix at the invariant closes the ones you were not.
        //
        // Two things this function does NOT do, contrary to an earlier
        // version of this comment. It is not called per frame - there are
        // eleven call sites, none in a render path - and it is not on every
        // path that moves `active_terminal`: `activate_terminal_pane` and
        // `move_terminal` both assign without calling it. Those two are safe
        // for reasons of their own (the first takes its index from
        // `terminal_at_pos`, which cannot name a folded pane because its
        // `last_area` is cleared; the second keeps the active pane pointed at
        // its own terminal), so the invariant's totality rests on those
        // reasons rather than on this being a universal funnel. A change to
        // what feeds `terminal_at_pos` would break it silently.
        //
        // And this is the rule for focus that DRIFTS. A caller deliberately
        // choosing a pane by index wants it made visible instead, which is
        // `reveal_terminal_pane`.
        //
        // Skipped while a pane is maximized: the flags are deliberately
        // ignored there and the active pane is painted full width whatever it
        // holds, so moving focus would fight the user's own selection. Leaving
        // maximize calls back through here, which is where it is repaired.
        if !self.terminal_pane_maximized
            && self
                .terminals
                .get(self.active_terminal)
                .is_some_and(|t| t.collapsed)
            && let Some(next) = self
                .terminals
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.collapsed)
                .min_by_key(|(candidate, _)| candidate.abs_diff(self.active_terminal))
                .map(|(candidate, _)| candidate)
        {
            self.active_terminal = next;
        }
        // Quick-select is bound to the pane it opened on; this per-frame
        // invariant covers every gesture that can move `active_terminal`
        // (clicks, chords, pane closes shifting indices) in one place, so
        // stale labels never survive a pane switch to hijack the new pane.
        if self
            .terminal_quick_select
            .as_ref()
            .is_some_and(|st| st.pane != self.active_terminal)
        {
            self.close_terminal_quick_select();
        }
        // Terminal find is pane-bound the same way.
        if self.terminal_find.is_some() && self.terminal_find_pane != self.active_terminal {
            self.close_terminal_find();
        }
        self.tree.focused = self.focus == Pane::Tree && self.sidebar_view == SidebarView::Explorer;
        self.search.focused = self.focus == Pane::Tree && self.sidebar_view == SidebarView::Search;
        self.source_control.focused =
            self.focus == Pane::Tree && self.sidebar_view == SidebarView::SourceControl;
        self.remote.focused = self.focus == Pane::Tree && self.sidebar_view == SidebarView::Remote;
        self.run_debug.focused =
            self.focus == Pane::Tree && self.sidebar_view == SidebarView::RunDebug;
        self.extensions.focused =
            self.focus == Pane::Tree && self.sidebar_view == SidebarView::Extensions;
        self.editor.focused = self.focus == Pane::Editor;
        // Inactive groups are by definition NOT the focused editor group, so
        // their borders never light up even while the editor pane holds focus.
        // `editor` (the active group) carries the highlight.
        for group in self.editor_layout.inactive_groups_mut() {
            group.focused = false;
        }
        let focused_pane = self.focus == Pane::Terminal;
        let active = self.active_terminal;
        for (i, t) in self.terminals.iter_mut().enumerate() {
            t.focused = focused_pane && i == active;
        }
        // Only the Black theme dresses the focused pane in the gradient
        // border; Croft Dark keeps the historical solid-blue highlight.
        let gradient = self.theme.gradient();
        // Every editor in both groups, not just the active one (deref would
        // only hit the active editor): a tab switch or the split group must
        // not resurrect the legacy navy active-tab chip between syncs.
        // The bundled PDF / CSV viewers are gated by the Extensions panel:
        // when disabled, the editor opens those file types as plain text.
        let pdf_on = self.is_extension_enabled("pdf");
        let csv_on = self.is_extension_enabled("csv");
        let (langs, ws_global) = (&self.language_prefs, self.whitespace_mode);
        for ed in self.editor.editors.iter_mut() {
            ed.focus_gradient = gradient;
            ed.theme = self.theme;
            ed.pdf_viewer_enabled = pdf_on;
            ed.csv_viewer_enabled = csv_on;
            ed.show_indent_guides = self.indent_guides_enabled;
            ed.home_walks_rows = self.home_walks_rows;
            ed.show_bracket_colors = self.bracket_colors_enabled;
            ed.provenance_overlay = self.provenance_overlay;
            ed.whitespace_mode = whitespace_mode_in(langs, ws_global, editor_scope_id(ed));
            ed.line_numbers = self.line_numbers;
            ed.diff_ws_default = self.diff_ws_default;
            ed.rulers.clone_from(&self.rulers);
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in group.editors.iter_mut() {
                ed.focus_gradient = gradient;
                ed.theme = self.theme;
                ed.pdf_viewer_enabled = pdf_on;
                ed.csv_viewer_enabled = csv_on;
                ed.show_indent_guides = self.indent_guides_enabled;
                ed.home_walks_rows = self.home_walks_rows;
                ed.show_bracket_colors = self.bracket_colors_enabled;
                ed.provenance_overlay = self.provenance_overlay;
                ed.whitespace_mode = whitespace_mode_in(langs, ws_global, editor_scope_id(ed));
                ed.line_numbers = self.line_numbers;
                ed.diff_ws_default = self.diff_ws_default;
                ed.rulers.clone_from(&self.rulers);
            }
        }
        self.tree.focus_gradient = gradient;
        self.tree.theme = self.theme;
        self.search.focus_gradient = gradient;
        self.search.theme = self.theme;
        self.source_control.focus_gradient = gradient;
        self.source_control.theme = self.theme;
        self.refresh_scm_repositories();
        self.run_debug.focus_gradient = gradient;
        self.run_debug.theme = self.theme;
        self.codeql.focus_gradient = gradient;
        self.codeql.theme = self.theme;
        let explorer_focused =
            self.focus == Pane::Tree && self.sidebar_view == SidebarView::Explorer;
        self.outline.focus_gradient = gradient;
        self.outline.theme = self.theme;
        self.outline.focused = explorer_focused;
        self.open_editors.focus_gradient = gradient;
        self.open_editors.theme = self.theme;
        self.open_editors.focused = explorer_focused;
        self.timeline.focus_gradient = gradient;
        self.timeline.theme = self.theme;
        self.timeline.focused = explorer_focused;
        self.agent_lane_panel.theme = self.theme;
        self.agent_lane_panel.focused = explorer_focused;
        self.commit_graph.focus_gradient = gradient;
        self.commit_graph.theme = self.theme;
        self.commit_graph.focused =
            self.focus == Pane::Tree && self.sidebar_view == SidebarView::SourceControl;
        self.dependencies.focus_gradient = gradient;
        self.dependencies.theme = self.theme;
        self.dependencies.focused = explorer_focused;
        self.remote.focus_gradient = gradient;
        self.remote.theme = self.theme;
        self.extensions.theme = self.theme;
        self.problems.focus_gradient = gradient;
        self.problems.theme = self.theme;
        self.problems.focused = self.focus == Pane::Terminal;
        self.problems.hover_pointer = self.pointer_cell;
        self.problems.fix_glyph = match self.problems_fix_started {
            Some(t) => update_spinner_frame(t.elapsed().as_millis()),
            None => "",
        };
        self.output.focus_gradient = gradient;
        self.output.theme = self.theme;
        self.output.focused = self.focus == Pane::Terminal;
        self.output.hover_pointer = self.pointer_cell;
        self.output.sync();
        self.ports.focus_gradient = gradient;
        self.ports.theme = self.theme;
        self.ports.focused = self.focus == Pane::Terminal;
        self.ports.hover_pointer = self.pointer_cell;
        for t in self.terminals.iter_mut() {
            t.show_timestamps = self.show_terminal_timestamps;
        }
        self.captures.focus_gradient = gradient;
        self.captures.theme = self.theme;
        self.captures.focused = self.focus == Pane::Terminal;
        self.captures.hover_pointer = self.pointer_cell;
        for t in self.terminals.iter_mut() {
            t.focus_gradient = gradient;
            t.theme = self.theme;
            t.set_palette(self.theme.ansi());
        }
        self.dress_host_accents();
    }

    /// Whether popups/menus/tooltips should wear the orange→green gradient
    /// border and the muted dark-teal selection fill instead of the legacy
    /// bright-blue accent. Gated on the Black theme to mirror the focused-pane
    /// border, so Croft Dark keeps its coherent all-blue look.
    fn popup_gradient(&self) -> bool {
        self.theme.gradient()
    }

    /// Open `url` in the user's browser: through the relay or a detected
    /// URL on a remote session, and never from a test.
    fn open_url_for_user(&mut self, url: &str) {
        self.try_open_url_for_user(url);
    }

    /// `open_url_for_user`, returning false when the browser could not be
    /// started.
    fn try_open_url_for_user(&mut self, url: &str) -> bool {
        if cfg!(test) {
            // Tests never start a browser.
            self.status = format!("Open {url}");
            return true;
        }
        if self.drop_relay_active() || is_remote_session() {
            self.open_detected_url(url);
            return true;
        }
        let opened = open_url(url);
        let ok = opened.is_ok();
        self.status = match opened {
            Ok(()) => format!("Opened {url}"),
            Err(e) => format!("Could not open a browser ({e}): {url}"),
        };
        ok
    }

    /// A path as typed: `~` expanded, relative to the workspace root.
    fn typed_path(&self, value: &str) -> PathBuf {
        self.typed_path_checked(value).unwrap_or_else(|| {
            let v = value.trim();
            self.workspace_root()
                .join(v.strip_prefix("~/").unwrap_or(v))
        })
    }

    /// As `typed_path`, but `None` for `~/…` when HOME is unset, so the
    /// caller can refuse it rather than write under the workspace.
    fn typed_path_checked(&self, value: &str) -> Option<PathBuf> {
        resolve_typed_path(
            value,
            std::env::var_os("HOME").map(PathBuf::from).as_deref(),
            self.workspace_root(),
        )
    }

    /// True when the active editor tab holds an editable text buffer (not a
    /// read-only diff / spreadsheet / image preview), so text-editing chords
    /// know whether to act.
    fn editor_is_text(&self) -> bool {
        !self.editor.has_non_text_view() && self.editor.markdown_preview.is_none()
    }

    /// Workspace-relative rendering for status transients and prompt
    /// labels: tabs and breadcrumbs already speak root-relative, and an
    /// absolute path under a deep root pushed the one informative part —
    /// the filename — off the visible end of the bar (#100). Paths outside
    /// the root (`~/.ssh/config`, files opened by absolute path) keep
    /// their absolute form.
    fn status_path(&self, path: &Path) -> String {
        // Folder-relative via the OWNING root (#143): identical to a plain
        // primary-root strip while the set holds one entry, and already the
        // multi-root label rule once it holds more.
        let rel = self
            .roots
            .owning_root(path)
            .and_then(|r| path.strip_prefix(r).ok())
            .unwrap_or(path);
        if rel.as_os_str().is_empty() {
            String::from(".")
        } else {
            rel.display().to_string()
        }
    }
}

/// Shrink `s` to at most `max` terminal cells by cutting the middle,
/// keeping roughly a third of the head and two thirds of the tail — for
/// path-shaped messages the tail carries the filename, the part worth
/// keeping. Never appends a trailing ellipsis (the cut is interior by
/// construction). Measured in cells, not chars, so a CJK name is not
/// overdrawn by the status bar's right cluster.
fn elide_middle(s: &str, max: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if s.width() <= max {
        return s.to_string();
    }
    if max < 3 {
        let mut out = String::new();
        let mut used = 0;
        for c in s.chars() {
            let w = c.width().unwrap_or(0);
            if used + w > max {
                break;
            }
            used += w;
            out.push(c);
        }
        return out;
    }
    crate::widgets::process_picker::elide_middle_cols(s, max)
}

/// `text` on the status bar's one row (#925): each line break or tab, with
/// the spaces around it, becomes one space. Drawn as is, the control
/// characters were dropped and the words either side ran together
/// ("…switch branches.Aborting"). The Messages channel keeps the lines.
fn one_line(text: String) -> String {
    if !text.contains(char::is_control) {
        return text;
    }
    let pieces = text.split(char::is_control).map(str::trim);
    pieces
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` with each absolute path under `root` written relative to it, for
/// the status bar: `Saved /home/me/proj/src/main.rs` reads `Saved
/// src/main.rs`. A path only counts where it starts a word (after the start,
/// a space, a quote or a bracket), so `root` found inside some other
/// absolute path is left alone; the root on its own is kept as is.
fn workspace_relative(text: &str, root: &Path) -> String {
    let root = root.to_string_lossy();
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return text.to_string();
    }
    let needle = format!("{root}/");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(&needle) {
        let starts_word = rest[..i]
            .chars()
            .next_back()
            .or_else(|| out.chars().next_back())
            .is_none_or(|c| c.is_whitespace() || "'\"`([{<".contains(c));
        out.push_str(&rest[..i]);
        if !starts_word {
            out.push_str(&needle);
        }
        rest = &rest[i + needle.len()..];
    }
    out.push_str(rest);
    out
}

/// Milliseconds since the Unix epoch, for stamping local-history snapshots
/// and dating TIMELINE rows.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `n` with the word for one or for many, as screen reader mode reads a
/// count: "1 error", "3 matches".
fn a11y_count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Whitespace rendering for a file in language `scope` (#1440): its
/// language block's `editor.renderWhitespace`, else `global`. A free fn so
/// the render loops can call it while they hold the editors mutably.
fn whitespace_mode_in(
    langs: &std::collections::BTreeMap<String, crate::prefs::LanguagePrefs>,
    global: crate::widgets::editor::WhitespaceMode,
    scope: &str,
) -> crate::widgets::editor::WhitespaceMode {
    langs
        .get(scope)
        .and_then(|l| l.render_whitespace.as_deref())
        .map_or(global, crate::widgets::editor::WhitespaceMode::from_pref)
}

/// The VS Code language id of the file at `path`, as its tab would have
/// it (#1440: which `languages` block applies).
fn path_scope_id(path: &Path) -> &'static str {
    settings_scope_id(
        crate::widgets::editor::language_scope_id(crate::highlight::lang_for_path(path)),
        Some(path),
    )
}

/// The VS Code language id of `ed`'s buffer for settings lookup (#1440).
fn editor_scope_id(ed: &crate::widgets::editor::Editor) -> &'static str {
    settings_scope_id(ed.scope_id(), ed.path.as_deref())
}

/// Refines the highlighter's language id `base` to the VS Code id the
/// file's extension carries where several ids share one grammar (#1440):
/// `.jsx` is `javascriptreact`, `.jsonc` is `jsonc`, `.scss` is `scss`, so
/// their `"[…]"` blocks apply. A buffer switched to another language keeps
/// that language's id.
fn settings_scope_id(base: &'static str, path: Option<&Path>) -> &'static str {
    let ext = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match (base, ext.as_deref()) {
        ("javascript", Some("jsx")) => "javascriptreact",
        ("json", Some("jsonc")) => "jsonc",
        ("css", Some("scss")) => "scss",
        ("css", Some("sass")) => "sass",
        _ => base,
    }
}

/// Inlay hints for a file in language `scope` (#1440): its language
/// block's `disable_inlay_hints`, else `global`. A free fn so callers can
/// use it while they hold the editors mutably.
fn inlay_hints_on_in(
    langs: &std::collections::BTreeMap<String, crate::prefs::LanguagePrefs>,
    global: bool,
    scope: &str,
) -> bool {
    langs
        .get(scope)
        .and_then(|l| l.disable_inlay_hints)
        .map_or(global, |off| !off)
}

use crate::widgets::terminal::cwd_is_observable;

/// The cells of `r` that lie inside `buf`, row by row.
fn cells_in(buf: &ratatui::buffer::Buffer, r: Rect) -> Vec<ratatui::buffer::Cell> {
    let r = r.intersection(buf.area);
    r.positions().map(|p| buf[p].clone()).collect()
}

/// Copy `text` to the host system clipboard.
///
/// Order of attempts:
///   1. **macOS native** — `clipboard::write_string` posts directly to
///      `NSPasteboard.generalPasteboard` (with `pbcopy` as a process-level
///      fall-back inside the same helper). This is the canonical Cocoa
///      clipboard API and is the *only* path that survives every iTerm2
///      configuration we have seen in the wild. OSC 52 is, empirically,
///      silently dropped by iTerm2 in some setups even with
///      `AllowClipboardAccess` enabled in `com.googlecode.iterm2` — and
///      writing escape bytes directly to the alt-screen stdout always
///      risks racing with the next ratatui frame.
///   2. **OSC 52 fall-back** — only if the native write returned false
///      (non-macOS croft binary on a Linux remote host, where the only
///      hand-shake with the user's local terminal is via escape bytes
///      through the SSH PTY). The fall-back is best-effort.
///
/// Returns true when the native path acknowledged the write so the call
/// site can downgrade the status message if it ever needs to. Today all
/// call sites discard the boolean; the return value is here so future
/// code can branch on it.
fn copy_to_clipboard(text: &str) -> bool {
    crate::clipboard::remember_copy(text);
    if crate::clipboard::write_string(text) {
        return true;
    }
    use std::io::Write;
    let bytes = crate::widgets::terminal::osc52_copy_seq(text);
    let mut out = std::io::stdout();
    let _ = out.write_all(&bytes);
    let _ = out.flush();
    false
}

/// Cap a string at `max` characters by inserting an ellipsis in the
/// end so the start (scheme/host) stays visible. No ellipsis marker anywhere in
/// croft, so overflow is cut plainly from the right rather than elided in the
/// middle. `max <= 8` returns the original.
fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max || max <= 8 {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Whether a process with `pid` exists. Signal 0 delivers nothing and only
/// checks; a process this user may not signal still exists, so `EPERM`
/// reads as alive. Off unix nothing is reaped, on the conservative side.
/// Existence, not liveness, is enough here (unlike `session::is_alive`,
/// which probes by connecting): a pid that was reused reads as alive and
/// only leaks a file, and every wrong answer this can give errs that way.
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    // SAFETY: `kill` with signal 0 only checks for the process; no signal is
    // delivered and no memory is touched.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// True when croft was invoked over an SSH login (or otherwise inside a
/// remote shell). Used to throttle PTY-driven redraws further so the SSH
/// pipe never saturates and starves input handling on the same thread.
fn is_remote_session() -> bool {
    crate::remote::running_over_ssh()
}

/// What the `.http` worker hands back (#370): the file it ran from, the
/// RAW request line (variables unresolved, so a secret can never land in a
/// status line, history entry, or response tab through it), and the result.
struct HttpRunOutcome {
    file: PathBuf,
    raw_line: String,
    result: Result<crate::http_file::HttpResponse, String>,
}

/// What fetching a variant analysis's results made (#578): the combined
/// SARIF log and table, how many repositories they hold, and the
/// repositories whose results could not be fetched, with why.
struct VariantResultFiles {
    sarif: Option<PathBuf>,
    csv: Option<PathBuf>,
    repos: usize,
    failed: Vec<(String, String)>,
}

/// Where an export of variant analysis results went (#578).
struct VariantExport {
    /// The summary file of a folder export.
    summary: Option<PathBuf>,
    /// The gist a gist export made.
    gist: Option<String>,
    repos: usize,
    failed: Vec<(String, String)>,
}

/// Hit rects for one terminal pane's header buttons, as laid out by the most
/// recent render. Any may be `None` when the pane is too narrow.
#[derive(Default)]
struct TerminalPaneButtons {
    add: Option<Rect>,
    profile: Option<Rect>,
    close: Option<Rect>,
    collapse: Option<Rect>,
}

#[cfg(test)]
mod tests;

/// One participant's parked caret in a multiplayer session: where they were
/// when someone else took over typing (docs/MULTIPLAYER.md).
#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionCaret {
    path: PathBuf,
    row: usize,
    col: usize,
}

/// A collab peer's last-known caret: where it is, whose it is, and when it
/// last moved (the name tag shows while `last_moved` is inside
/// [`CARET_LABEL_FADE`]). Keyed by site id, which is per-file, so one peer
/// legitimately holds one entry per shared file.
struct CollabCaret {
    file: String,
    row: usize,
    col: usize,
    name: String,
    last_moved: std::time::Instant,
}

/// A live AI stream announced over the relay (`CollabMsg::StreamState`):
/// who is streaming, into which file, and the pilot's site id in that file
/// (keys the pilot's ghost caret for the gutter stop button's row).
struct CollabStreamInfo {
    file: String,
    name: String,
    site: u64,
    /// The undo group the stream's edits share (#1207), so the whole
    /// stream undoes in one step.
    undo_group: u64,
}

/// The stdout wrapper that is ratatui's writer. It counts the bytes each
/// frame ships, to surface the per-frame payload size in the F8 perf HUD
/// (which reads and resets the count once per frame), and holds them until
/// the frame flushes, so the whole frame reaches the terminal in one write.
/// Straight through to stdout, a full repaint went out a kilobyte at a time,
/// tens of writes the terminal could draw between, so a launch's first frame
/// visibly painted in; and every few-byte cursor move or colour change took
/// stdout's lock on its way.
struct CountingWriter {
    inner: Stdout,
    pending: Vec<u8>,
    bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl CountingWriter {
    fn new(inner: Stdout, bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self {
            inner,
            pending: Vec::with_capacity(64 * 1024),
            bytes,
        }
    }
}

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buf);
        self.bytes
            .fetch_add(buf.len(), std::sync::atomic::Ordering::Relaxed);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.pending.is_empty() {
            let sent = self.inner.write_all(&self.pending);
            self.pending.clear();
            sent?;
        }
        self.inner.flush()
    }
}

/// DEC private mode 2026, synchronized output: the terminal shows nothing a
/// frame draws until the frame ends, then all of it at once, so a repaint
/// never appears half done. Terminals without it ignore the mode.
const BEGIN_SYNCHRONIZED_UPDATE: &[u8] = b"\x1b[?2026h";
const END_SYNCHRONIZED_UPDATE: &[u8] = b"\x1b[?2026l";

type CroftTerminal = Terminal<CrosstermBackend<CountingWriter>>;

pub fn run(
    root: PathBuf,
    restore_session: Option<PathBuf>,
    open_file: Option<PathBuf>,
    open_at: Option<(usize, usize)>,
    zen: bool,
    workspace_folders: Vec<PathBuf>,
) -> Result<()> {
    // What building the editor reads that takes real work to make: the
    // bundled extension manifests (one TOML parse each) and the built-in
    // secret and agent-prompt regexes. Together they were most of what a
    // launch did before its first frame, all on this thread. Each starts on
    // a thread of its own now, while this one binds sockets and spawns the
    // shell, and whatever reaches one before it is done waits only for the
    // rest of it.
    std::thread::spawn(crate::lsp::manifest::prewarm_bundled);
    std::thread::spawn(crate::triggers::prewarm_builtin_redactions);
    // Every job from here on that the first frame never shows waits for it
    // (see `startup_gate`), so the UI thread has the CPU to itself.
    crate::startup_gate::arm();
    crate::startup_gate::spawn_after_first_frame(crate::agents::prewarm_builtin);
    // Termux ships no font with codicon glyphs, so the activity-bar glyph
    // fallback renders blank cells out of the box. Install the Meslo Nerd
    // Font Mono into ~/.termux/font.ttf in the background (no-op on every
    // other host, and when the user already has a font.ttf).
    crate::termux::ensure_codicon_font_in_background();
    // Build the tree-sitter highlight configurations on a background thread so
    // the first file open paints its syntax (keywords, strings, comments)
    // instantly without the per-tab query-compilation snap. That compile is
    // hundreds of milliseconds of CPU, so it starts once the first frame is
    // up rather than competing with it. The configs land in a process-wide
    // cache, so the UI thread reads the finished build straight out of it on
    // first open (or builds on demand if the user opens a file before the
    // prewarm finishes).
    crate::startup_gate::spawn_after_first_frame(crate::highlight::prewarm_configs);
    // Hand the heap a large file's parse passes freed back to the OS
    // (#1707), so croft doesn't stay resident at the parse's peak.
    crate::mem_relief::start();
    let title = build_title(&root);
    crate::startup_trace::mark("background work started");
    // Raw mode, and the host terminal's capability questions sent, before
    // the editor is built: the answers travel while App::new runs instead of
    // after it, which over SSH is a network round trip off the first frame.
    enable_raw_mode().context("enable raw mode")?;
    #[cfg(unix)]
    let host_probe = StartupProbe(Some(crate::iterm2_inline::ask_host()));
    let mut app = App::new(root.clone())?;
    crate::startup_trace::mark("editor built");
    // Resurrect this workspace's terminal panel from the last session
    // (pane layout, cwds, names, focus) before the first frame paints.
    app.restore_terminal_session();
    // A hosted default pane (#694) is recorded at once, restored or not.
    app.save_terminal_session_if_hosted();
    // A launch from a .code-workspace file (#163) defines the folder set
    // exactly; otherwise the automatic per-primary store restores.
    if workspace_folders.is_empty() {
        let restored_folders = app.restore_workspace_folders();
        if restored_folders > 0 {
            app.status = format!("Restored {restored_folders} workspace folder(s)");
        }
    } else {
        for folder in workspace_folders {
            app.add_workspace_folder(folder);
        }
    }
    // Restore the tabs / layout carried across a self-update re-exec, then
    // delete the handoff file so a later normal launch starts clean.
    if let Some(session_path) = restore_session.as_ref() {
        match crate::session_state::SessionState::load(session_path) {
            Ok(state) => {
                let left = app.apply_session_state(&state);
                if app.hold_unrestored_tabs(left) {
                    let _ = std::fs::remove_file(session_path);
                } else {
                    // Kept: it holds the only copy of cells not put back.
                    app.status = format!(
                        "Some unsaved xlsx cells could not be restored; the session is kept at {}",
                        session_path.display()
                    );
                }
                // The hot-exit backup follows what the relaunch brought
                // back (#862), at once rather than after the first edit.
                let _ = app.write_hot_exit();
            }
            // Kept: it may hold the only copy of unsaved text.
            Err(e) => {
                app.status = format!(
                    "Could not restore the session ({e:#}); it is kept at {}",
                    session_path.display()
                );
            }
        }
    } else {
        // A normal launch brings back what hot exit kept of this workspace
        // when croft last ended without a clean quit (#862). The session
        // the last clean exit saved brings back its tabs when hot exit had
        // none, and its bookmarks either way; it is read once the first
        // frame is up, off this thread.
        let restored = app.restore_hot_exit();
        app.read_saved_session(!restored);
    }
    crate::startup_trace::mark("session restored");
    // `--zen`: hide the Explorer sidebar and terminal so the editor fills the
    // window. "Move/Copy into New Window" launches the new window this way so it
    // opens focused on just the handed-off file. Toggle back with Cmd+B / Cmd+J.
    if zen {
        app.show_tree = false;
        app.show_terminal = false;
    }
    // `croft <root> --open-file <path>`: open the handed-off file (used by the
    // new window that Move / Copy into New Window spawns).
    if let Some(file) = open_file.as_ref() {
        // `croft file:line[:col]` (#1606).
        if app.open_file_at_launch(file)
            && let Some((row, col)) = open_at
        {
            app.without_auto_hide(|app| app.place_caret_at(row, col));
        }
    }
    // A `croft://` link (#359) names what to land on.
    match std::env::var("CROFT_FOCUS").as_deref() {
        Ok("terminal") => app.focus_pane(Pane::Terminal),
        Ok("editor") => app.focus_pane(Pane::Editor),
        // The approval popup is modal and opens by itself for a waiting
        // edit, so landing on it needs no focus change; say so when there
        // is nothing to land on (answered or expired on the way here).
        Ok("approval") => {
            app.drain_hook_requests();
            if app.approvals.is_empty() {
                app.status = String::from("No agent edit is waiting for approval");
            }
        }
        _ => {}
    }
    app.start_update_watch_if_remote();
    app.start_drift_probe_if_local();
    app.start_update_check_if_local();
    // `croft pr <n>` (#365): open the review once the app is up.
    if let Some(selector) = crate::pr_review::take_startup_pr() {
        app.start_pr_review(selector);
    }
    // `croft demo` (#377): start the tour once the app is up.
    if crate::tour::take_startup_demo() {
        app.start_demo();
    }

    // Sixel has no env var, and whether the terminal speaks the kitty
    // keyboard protocol has none either, so probe the host: the kitty query,
    // then DA1. This runs in raw mode but before the alt-screen switch and
    // long before the event loop's first poll, so crossterm's (lazily-started)
    // event reader can't swallow the reply. A non-sixel terminal (e.g. Apple
    // Terminal) answers DA1 without attribute 4; init_graphics then promotes
    // the protocol to Sixel only when neither iTerm2 nor Kitty was detected
    // from the environment and the reply lists it. tmux answers DA1 itself,
    // listing sixel when it was built with it whatever the terminal around
    // it can draw, so there the reply says nothing about the screen.
    #[cfg(unix)]
    {
        let host = host_probe.answer();
        if app.inline_protocol == crate::iterm2_inline::InlineImageProtocol::None {
            app.sixel_supported = host.sixel && !crate::iterm2_inline::detect_tmux();
            crate::widgets::terminal::SIXEL_HOST
                .store(app.sixel_supported, std::sync::atomic::Ordering::Relaxed);
        }
        app.legacy_ctrl_punct = host.answered && !host.kitty_keyboard;
    }
    crate::startup_trace::mark("host terminal answered");
    let mut out = stdout();
    // Alt screen + mouse + bracketed paste + SteadyBar cursor (thin vertical
    // line, no hardware blink: the blink is done in software so it works even
    // when the host terminal's own "Blinking cursor" preference is off). The
    // shared takeover block keeps this list identical to the dtach-reattach
    // re-assert; add new modes in takeover_mode_seq, not here.
    {
        use std::io::Write;
        out.write_all(&takeover_mode_seq())
            .and_then(|()| out.flush())
            .context("enter alt screen")?;
    }
    // From here the tty is in raw mode + alt-screen + mouse + bracketed
    // paste. The normal teardown below restores all of it, but a panic
    // unwinds straight past it and would drop the user back to a shell
    // with mouse-tracking still on (stray `^[[<35;…M` on every mouse
    // move). Install a hook that restores the tty first, then chains to
    // the original so the panic message + backtrace still print.
    install_terminal_restore_panic_hook();
    // Best-effort: terminals that don't speak the kitty keyboard protocol just
    // ignore this; ones that do (iTerm2 >=3.5, Ghostty, kitty, WezTerm) start
    // delivering Cmd/Super as a real modifier so cmd+s reaches the app.
    let kbd_enhanced = execute!(
        out,
        PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
    )
    .is_ok();
    app.kitty_keys = kbd_enhanced;
    {
        use std::io::Write;
        out.write_all(&set_title_seq(&title)).ok();
        // Force iTerm2's session bg to sRGB(EDITOR_BG_RGB). Combined with
        // `Color::Reset` on welcome-pane cells and the same sRGB hex baked
        // into the OSC-1337 PNG canvas, both surfaces flow through iTerm2's
        // sRGB → display path and the welcome image bg matches the
        // surrounding pane bg pixel-for-pixel
        // (https://gitlab.com/gnachman/iterm2/-/issues/12529).
        let theme = crate::prefs::Prefs::load_or_default().theme();
        out.write_all(theme_host_colors_seq(theme).as_bytes()).ok();
        out.flush().ok();
    }
    // Env-var-only iTerm2 detection: no stdin queries, so this can't
    // contend with the crossterm event reader.
    app.init_graphics();
    // With the inline-image protocol fully resolved (env detection + the sixel
    // DA1 probe above), decide whether to nudge the user toward a terminal that
    // can render croft's icons and images. Runs identically on the local Mac
    // and the remote Linux box (GOLDEN RULE): both reach `run`, and `croft
    // remote` forwards the local terminal's identity so an SSH session from
    // iTerm2/Ghostty is never falsely warned.
    app.arm_terminal_warning();

    // Wrap stdout so we can count exactly how many bytes ratatui's diff
    // ships per frame — over SSH that byte count, alongside the per-frame
    // render time, is the signal that tells a fat repaint from a slow
    // remote CPU. Toggled on screen with F8.
    let perf_bytes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    app.perf.attach_counter(perf_bytes.clone());
    let backend = CrosstermBackend::new(CountingWriter::new(out, perf_bytes));
    let mut terminal: CroftTerminal = Terminal::new(backend).context("create terminal")?;
    crate::startup_trace::mark("screen set up");

    let result = main_loop(&mut app, &mut terminal);
    // An owner's goodbye goes out now (#1147): the drop-to-local and
    // self-update exits below never drop the App.
    drop(app.collab.take());

    // Snapshot the terminal panel for the next launch (cwds are read live
    // here, so plain `cd`s during the session are captured at quit).
    app.save_terminal_session();
    // A test run still in flight ends with croft (#1534): its runner was
    // left running under PID 1.
    if app.testing.is_busy() {
        app.test_worker.cancel();
    }

    // Quitting mid-debug ends the program as Stop Debugging does (#1536):
    // nothing sent `disconnect` here, and the debuggee outlived croft.
    // The teardowns run while the terminal is restored and are joined
    // below, before any exit path.
    if !app.debug_sessions.is_empty() {
        app.debug_stop();
    }

    disable_raw_mode().ok();
    {
        use std::io::Write;
        let mut out = stdout();
        out.write_all(&set_title_seq("")).ok();
        // Revert the host terminal's default colors (OSC 110/111, plus the
        // iTerm2 sRGB override where present) so the user's shell after
        // croft exits doesn't keep our forced theme colours.
        out.write_all(reset_host_colors_seq().as_bytes()).ok();
        if crate::iterm2_inline::detect_iterm2_inline_support() {
            out.write_all(reset_session_bg_seq().as_bytes()).ok();
        }
        out.flush().ok();
    }
    // A frame cut short by an error never sent its end; the shell must not
    // inherit a terminal still holding its updates back.
    {
        use std::io::Write;
        let backend = terminal.backend_mut();
        let _ = backend.write_all(END_SYNCHRONIZED_UPDATE);
        let _ = backend.flush();
    }
    if kbd_enhanced {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags).ok();
    }
    execute!(
        terminal.backend_mut(),
        crossterm::cursor::SetCursorStyle::DefaultUserShape,
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
    )
    .ok();
    terminal.show_cursor().ok();

    // Reap the navigator's claude child before any exit: drop-to-local
    // (process::exit) and self-update (exec) below both bypass Drop, and even
    // a normal return races the detached Drop thread against process exit.
    // shutdown_blocking reverts, hangs up, and grace-kills synchronously.
    if let Some(host) = app.pair_host.take() {
        host.shutdown_blocking();
    }
    // And every teardown a recent unseat DETACHED (toggle off, record
    // disabled, pilot death): those threads race process exit, and a thread
    // that dies mid-grace-kill leaves the claude child running.
    crate::pair_host::join_teardowns();
    crate::dap::transport::join_teardowns();

    // What croft could not do on the way out, said once the shell is the
    // user's again (#862).
    for note in app.exit_notes.drain(..) {
        eprintln!("croft: {note}");
    }
    result?;
    if app.drop_to_local {
        std::process::exit(crate::remote::DROP_TO_LOCAL_EXIT_CODE);
    }
    // A background self-update landed: serialize the session and replace
    // this process image with the freshly-installed binary. The unix
    // process-image swap keeps the same controlling tty (the ssh PTY), so
    // there is no reconnect - the user sees a redraw, not a dropped
    // session. No shell is involved; args are passed as an argv vector.
    if app.pending_reexec {
        let state = app.capture_session_state();
        let session_path = crate::session_state::handoff_path();
        // Saved once already when the relaunch was armed; this refresh is
        // best-effort, and a failure leaves that copy in place.
        let _ = state.save(&session_path);
        use std::os::unix::process::CommandExt;
        // A staged release (#333) is swapped over the installed binary
        // only now, on the user's Relaunch: until this line PATH still
        // held the old version.
        let target = reexec_binary_path(app.self_install.as_ref());
        if let Some(staged) = app.staged_update_binary()
            && let Err(err) = crate::update_check::apply_staged(&staged, &target)
        {
            // The installed binary is untouched (copy-beside + rename), so
            // relaunching it is still right: the editor must never simply
            // exit because an update could not be applied.
            eprintln!(
                "croft update failed: could not replace {} with {}: {err} - relaunching the current version",
                target.display(),
                staged.display()
            );
        }
        let mut cmd = std::process::Command::new(target);
        cmd.arg(app.workspace_root())
            .arg("--restore-session")
            .arg(&session_path);
        let err = CommandExt::exec(&mut cmd);
        eprintln!("croft self-relaunch failed: {err}");
        return Ok(());
    }
    if let Some(remote) = app.remote_launch.take() {
        app.fs_watch.disable();
        let launch_result = if let Some(adopted) = remote.adopted {
            crate::remote::launch_only(adopted, remote.path.as_deref())
        } else {
            crate::remote::launch_croft_with(&remote.host, remote.path.as_deref(), None, false)
        };
        restore_host_terminal_state();
        match launch_result? {
            crate::remote::RemoteOutcome::ReturnToLocal => {
                return run(root, None, None, None, false, Vec::new());
            }
            crate::remote::RemoteOutcome::Exited => return Ok(()),
        }
    }
    // A clean exit: what the next launch of this workspace opens with.
    if let Err(e) = app.save_session_for_next_launch() {
        eprintln!("croft: could not save the session: {e:#}");
    }
    Ok(())
}

/// The host probe `run` sent before building the editor. Should building it
/// fail, the replies are still read off the tty, so they never land at the
/// shell's prompt, and raw mode is undone before the error prints.
#[cfg(unix)]
struct StartupProbe(Option<crate::iterm2_inline::PendingHostProbe>);

#[cfg(unix)]
impl StartupProbe {
    fn answer(mut self) -> crate::iterm2_inline::HostProbe {
        self.0
            .take()
            .map(crate::iterm2_inline::PendingHostProbe::answer)
            .unwrap_or_default()
    }
}

#[cfg(unix)]
impl Drop for StartupProbe {
    fn drop(&mut self) {
        if let Some(pending) = self.0.take() {
            let _ = pending.answer();
            disable_raw_mode().ok();
        }
    }
}

/// Fingerprints of the text cells under each large post-frame image this
/// frame (see [`overlay::ImageOverlay::needs_emit`]).
struct ImageUnderlays {
    /// Around each activity-bar and toolbar icon, for iTerm2's keepalive.
    activity: Vec<u64>,
    editor: [u64; 2],
    terminal: u64,
    markdown: u64,
    minimap: u64,
}

/// Consume every overlay's one-shot image-clear latch; true when any fired.
/// Shared by the pre-draw and post-draw eviction checks in [`main_loop`].
fn consume_any_image_clear(app: &mut App) -> bool {
    let fired = [
        app.consume_welcome_image_clear(),
        app.consume_editor_image_clear(),
        app.consume_terminal_image_clear(),
        app.consume_markdown_image_clear(),
        app.consume_run_debug_image_clear(),
        app.consume_problems_badge_image_clear(),
        app.consume_no_repo_hero_image_clear(),
        app.consume_shortcuts_image_clear(),
        app.consume_file_finder_image_clear(),
        app.consume_command_palette_image_clear(),
        app.consume_process_picker_image_clear(),
        app.consume_zoxide_jump_image_clear(),
        app.consume_branch_picker_image_clear(),
        app.consume_input_prompt_image_clear(),
        app.consume_list_picker_image_clear(),
        app.consume_ssh_empty_state_image_clear(),
        app.consume_minimap_image_clear(),
        app.consume_activity_image_clear(),
    ];
    fired.into_iter().any(|f| f)
}

fn main_loop(app: &mut App, terminal: &mut CroftTerminal) -> Result<()> {
    // Force the very first frame to render so the user sees the UI even
    // before the first event arrives or any timer fires.
    let mut needs_redraw = true;
    let mut first_frame = true;
    let mut last_blink_visible = app.cursor_visible_phase();
    // Advances every UPDATE_SPINNER_FRAME_MS while a background update runs;
    // forces a status-bar redraw so the spinner animates. Constant otherwise.
    let mut last_spinner_phase = app.update_spinner_phase();
    // Frame-rate accounting for the F8 HUD: count draws over a rolling 1s
    // window so a high idle FPS (always-dirty redraw loop saturating the
    // SSH link) is visible at a glance.
    let mut fps_window_start = std::time::Instant::now();
    let mut frames_in_window: u32 = 0;
    let mut keys_in_window: u32 = 0;
    let mut iters_in_window: u32 = 0;
    let mut work_max_us: u128 = 0;
    let mut poll_max_us: u128 = 0;
    // PTY-only redraws are capped so a chatty embedded shell (Claude Code,
    // npm install, log streams) cannot saturate stdout and starve mouse /
    // key events on the same thread. Input + FS + search + git + remote
    // signals always bypass the cap, so clicks and keystrokes stay at 0ms.
    // Over SSH the user's link is the bottleneck, so we drop further to
    // ~20 Hz; locally ~30 Hz is plenty for shell text.
    let pty_min_interval = if is_remote_session() {
        Duration::from_millis(50)
    } else {
        Duration::from_millis(33)
    };
    // A keystroke echo, a shell line-rewrite, or an input-box update
    // advances only a handful of bytes into the grid; a bulk stream (cat,
    // build logs, a full-pane scroll, a TUI's streaming output) advances
    // many. We let small updates redraw immediately so echo feels native
    // even when croft itself is the remote end of an SSH link, and keep
    // the cap on large updates so a flood can't saturate the pipe and
    // starve input. A full-pane repaint runs tens of KB; interactive
    // echo stays well under this, so the two never alias.
    const PTY_SMALL_UPDATE_BYTES: usize = 4096;
    let mut last_pty_redraw = std::time::Instant::now()
        .checked_sub(pty_min_interval)
        .unwrap_or_else(std::time::Instant::now);

    while !app.quit {
        iters_in_window += 1;
        if fps_window_start.elapsed() >= Duration::from_secs(1) {
            app.perf.record_window(
                frames_in_window,
                keys_in_window,
                iters_in_window,
                work_max_us,
                poll_max_us,
            );
            frames_in_window = 0;
            keys_in_window = 0;
            iters_in_window = 0;
            work_max_us = 0;
            poll_max_us = 0;
            fps_window_start = std::time::Instant::now();
        }
        let iter_start = std::time::Instant::now();
        // If the user dropped files onto the Remote Explorer, suspend the
        // alt-screen and run scp in the host shell so the user sees its
        // progress, can answer password / FIDO / known_hosts prompts,
        // and gets explicit success / failure output before we resume.
        if !app.pending_scp_uploads.is_empty() {
            run_pending_scp_uploads(app, terminal)?;
            needs_redraw = true;
        }
        // Same-file tabs agree before the watcher is read: a save from one
        // symbol tab leaves its siblings holding the saved text, so the
        // change event reads as their own write, not a conflict (#369).
        let symbol_views_settled = app.sync_symbol_views();
        // Pull in any filesystem-watcher events first so the tree reflects
        // disk reality on the very next frame.
        let fs_changed = app.drain_fs_events();
        // Peek without clearing so we can coalesce PTY-only redraws without
        // losing bytes that the reader thread has already advanced into the
        // terminal grid.
        let pty_pending = app.peek_terminals_dirty();
        let blink_visible = app.cursor_visible_phase();
        let blink_changed = blink_visible != last_blink_visible;
        // Either spinner advancing must wake a redraw; both stay 0 when idle.
        let spinner_phase =
            app.update_spinner_phase() + app.mcp_spinner_phase() + app.problems_fix_spinner_phase();
        let spinner_changed = spinner_phase != last_spinner_phase;
        let ext_index_changed = app.drain_ext_index_refresh();
        let codeql_changed = app.drain_codeql_run()
            | app.drain_codeql_ast()
            | app.drain_codeql_model()
            | app.drain_codeql_doc()
            | app.drain_codeql_upgrade()
            | app.drain_codeql_version()
            | app.drain_codeql_log_summary()
            | app.drain_codeql_log_viewer()
            | app.drain_codeql_perf_compare()
            | app.drain_codeql_query_help()
            | app.drain_codeql_pack_job()
            | app.drain_codeql_db_job()
            | app.drain_codeql_cli_download()
            | app.drain_codeql_cli_check()
            | app.drain_codeql_code_search()
            | app.drain_codeql_variant_submit()
            | app.poll_codeql_variant_runs()
            | app.drain_codeql_variant_results()
            | app.drain_codeql_variant_export()
            | app.drain_codeql_queries();
        let search_changed = app.drain_search_results();
        let log_index_changed = app.poll_log_index();
        let remote_changed = app.refresh_remote_if_config_changed();
        let pulls_changed = app.drain_remote_pulls();
        let view_changed = app.drain_view_requests();
        let hook_changed = app.drain_hook_requests()
            | app.poll_pr_gh()
            | app.poll_pr_checkout()
            | app.poll_code_scanning();
        app.sync_approval_check();
        app.persist_agent_ledger(false);
        let kernel_changed = app.poll_notebook_kernels();
        let ports_changed = app.drain_ports_and_poll();
        let session_presence_changed = app.poll_session_presence();
        let session_typing_changed = app.poll_session_typing();
        let collab_changed = app.poll_collab();
        let symbol_views_changed = app.sync_symbol_views() | symbol_views_settled;
        let bells_changed = app.drain_terminal_bells();
        // A block whose command has finished gets its output box (#354).
        let captures_changed = app.settle_block_captures();
        // Every tick, not only when a capture settles: this is what notices
        // the editor has moved to another document (#354 review).
        app.publish_block_outputs();
        let labels_changed =
            app.refresh_terminal_labels() | app.drain_agent_events() | app.drain_fleet_results();
        app.flush_terminal_session();
        let auto_save_changed = app.tick_auto_save();
        app.tick_hot_exit(std::time::Instant::now());
        let highlights_changed = app.poll_highlights();
        // Pictures baked off the UI thread (#1973) go up as they land.
        let images_changed =
            app.poll_chrome_bake() | app.poll_welcome_bake() | app.poll_saved_session();
        // A closed tab's bookmarks are kept, and come back with its file.
        let bookmarks_changed = app.settle_bookmarks();
        let live_run_changed = app.tick_live_run()
            | app.sync_markdown_side_previews()
            | app.sync_markdown_scroll()
            | app.tick_minimap();
        app.tick_code_lens();
        let code_lens_changed = app.tick_config_watch()
            | app.drain_lsp_code_lens()
            | app.drain_search_editor()
            | app.tick_inline_complete()
            | app.drain_review_ops();
        let connect_changed = app.poll_connect_dialog() | app.drain_scrub_views();
        let install_changed = app.poll_install_session();
        let update_changed = app.poll_update_watch();
        let http_changed = app.drain_http_responses();
        let reveal_changed = app.tick_redaction_reveal();
        app.sync_lsp();
        app.send_due_completion_ask();
        app.send_due_completion_reask();
        let markdown_lint_changed = app.sync_markdown_lint();
        let sarif_diagnostics_changed = app.sync_sarif_diagnostics();
        app.sync_sarif_selection_to_cursor();
        app.sync_sarif_step_marks();
        app.refresh_sarif_preview();
        app.poll_sarif_download();
        app.sync_git_gutters();
        app.sync_blame();
        app.sync_provenance();
        let blame_changed = app.drain_blame() | app.drain_git_net() | app.drain_git_view();
        // Request/refresh the OUTLINE for the active file (after sync_lsp so the
        // edit-seq it reads is current) and advance follow-cursor.
        let outline_sync_changed = app.sync_outline();
        let outline_changed = app.drain_lsp_document_symbols();
        app.workspace_symbols_dispatch_due();
        let ws_symbols_changed = app.drain_lsp_workspace_symbols();
        // Refresh the Explorer's other stacked sub-views (Open Editors list,
        // Timeline git history, Dependencies) and drain their workers.
        let explorer_panels_changed = app.sync_explorer_panels();
        app.refresh_semantic_tokens_if_requested();
        app.refresh_inlay_hints_if_requested();
        app.refresh_workspace_diagnostics_if_requested();
        let lsp_changed = app.drain_lsp_completion();
        app.refresh_signature_help_if_moved();
        let sig_help_changed = app.drain_lsp_signature_help();
        app.poll_hover();
        let hover_changed =
            app.drain_lsp_hover() | app.drain_lsp_rust_analyzer() | app.drain_blame_hover();
        let tab_hover_changed = app.poll_tab_hover();
        let ui_tooltip_changed = app.poll_ui_tooltip();
        let definition_changed = app.drain_lsp_definition();
        let declaration_changed = app.drain_lsp_declaration();
        let type_definition_changed = app.drain_lsp_type_definition();
        let implementation_changed = app.drain_lsp_implementation();
        let references_changed = app.drain_lsp_references();
        let call_hierarchy_changed = app.drain_lsp_call_hierarchy();
        let sidebar_hide_changed = app.tick_sidebar_auto_hide();
        let occ_tick_changed = app.tick_occurrences();
        let linked_tick_changed = app.tick_linked_editing();
        let linked_changed = app.drain_lsp_linked_editing();
        app.tick_on_type_formatting();
        let on_type_changed = app.drain_lsp_on_type_format();
        let prepare_rename_changed = app.drain_lsp_prepare_rename();
        let occurrences_changed = app.drain_lsp_document_highlights();
        let rename_changed = app.drain_lsp_rename() | app.tick_file_moves();
        let format_changed = app.drain_lsp_format() | app.drain_prettier_format();
        let code_action_changed = app.drain_lsp_code_actions();
        let semantic_changed = app.drain_lsp_semantic_tokens();
        let inlay_changed = app.drain_lsp_inlay_hints();
        app.drain_lsp_document_links();
        let folds_changed = app.drain_lsp_folding_ranges();
        let selection_ranges_changed = app.drain_lsp_selection_ranges();
        let colors_changed = app.drain_lsp_document_colors();
        let color_pres_changed = app.drain_lsp_color_presentations();
        let diagnostics_changed = app.drain_lsp_diagnostics();
        let progress_changed = app.drain_lsp_progress();
        let dap_changed = app.poll_dap() | app.poll_debug_build();
        let tests_changed = app.test_worker.drain(&mut app.testing)
            | app.drain_pending_test_debug()
            | app.drain_pending_cargo_launch();
        // A queued run the worker refused (runner disabled after the entry
        // check) rolled its cases back in the drain; say why nothing ran.
        if app.testing.take_refusal() {
            app.status = String::from(crate::testing::NO_RUNNER_STATUS);
        }
        if app.testing.take_cancelled() {
            app.status = String::from("Test run cancelled");
        }
        // One notification per red run (#358), from the latch the panel
        // sets when a run ends, never per test case.
        let coverage_changed = app.sync_coverage();
        let watch_changed = app.tick_test_watch();
        if app.testing.take_failed_run() {
            let (passed, failed, _) = app.testing.counts();
            app.notifier.emit(
                crate::notifications::Event::TestsFailed {
                    failed,
                    passed,
                    reported: app.testing.run_reported_failed(),
                },
                app.roots.primary(),
                &remote_host_label(),
            );
        }
        let mcp_changed = app.poll_mcp();
        let pair_changed =
            app.maybe_seat_navigator() | app.poll_pair() | app.tick_proactive_navigator();
        let voice_changed = app.drain_voice();
        let zen_hint_changed = app.expire_zen_hint(std::time::Instant::now());
        let screencast_changed = app.screencast.expire(std::time::Instant::now());
        let zoxide_changed = app.poll_zoxide_install();
        // Surface managed language-server install progress in the status bar so
        // the background work (which can take a few seconds) is visible.
        let install_status_changed = match crate::lsp::install::take_status() {
            Some(msg) => {
                app.status = msg;
                true
            }
            None => false,
        };
        // When a managed install finishes, forget the LSP sync state for the
        // docs that language serves so the next `sync_lsp` re-opens them. That
        // re-open makes the manager re-probe and spawn the just-installed
        // server, so "installed" becomes "working" without a further action.
        let just_installed = crate::lsp::install::take_just_installed();
        if !just_installed.is_empty() {
            app.lsp_last_seen
                .retain(|path, _| !just_installed_covers_path(&just_installed, path));
        }

        let non_pty_dirty = needs_redraw
            || zen_hint_changed
            || screencast_changed
            || zoxide_changed
            || fs_changed
            || mcp_changed
            || pair_changed
            || tests_changed
            || coverage_changed
            || watch_changed
            || blink_changed
            || spinner_changed
            || ext_index_changed
            || codeql_changed
            || search_changed
            || log_index_changed
            || remote_changed
            || pulls_changed
            || view_changed
            || hook_changed
            || kernel_changed
            || ports_changed
            || session_presence_changed
            || session_typing_changed
            || collab_changed
            || symbol_views_changed
            || bells_changed
            || captures_changed
            || labels_changed
            || auto_save_changed
            || highlights_changed
            || images_changed
            || bookmarks_changed
            || live_run_changed
            || code_lens_changed
            || ws_symbols_changed
            || connect_changed
            || install_changed
            || update_changed
            || http_changed
            || reveal_changed
            || lsp_changed
            || sig_help_changed
            || hover_changed
            || tab_hover_changed
            || ui_tooltip_changed
            || definition_changed
            || declaration_changed
            || type_definition_changed
            || implementation_changed
            || references_changed
            || call_hierarchy_changed
            || sidebar_hide_changed
            || occ_tick_changed
            || linked_tick_changed
            || linked_changed
            || on_type_changed
            || prepare_rename_changed
            || occurrences_changed
            || rename_changed
            || format_changed
            || code_action_changed
            || semantic_changed
            || inlay_changed
            || folds_changed
            || selection_ranges_changed
            || colors_changed
            || color_pres_changed
            || diagnostics_changed
            || progress_changed
            || voice_changed
            || outline_sync_changed
            || outline_changed
            || explorer_panels_changed
            || install_status_changed
            || blame_changed
            || dap_changed
            || markdown_lint_changed
            || sarif_diagnostics_changed;
        let pty_eligible = pty_pending
            && (app.peek_terminals_pending_bytes() <= PTY_SMALL_UPDATE_BYTES
                || last_pty_redraw.elapsed() >= pty_min_interval);
        let do_redraw = non_pty_dirty || pty_eligible;
        // Consume the PTY dirty flags only when we are actually redrawing.
        // Skipping a frame must leave them set so the next eligible tick
        // emits the deferred output.
        let pty_changed = if do_redraw {
            app.drain_terminals_dirty()
        } else {
            false
        };

        if do_redraw {
            if pty_changed {
                last_pty_redraw = std::time::Instant::now();
            }
            // Everything this frame sends, the diff and the pictures after
            // it, goes up as one update.
            {
                use std::io::Write;
                let _ = terminal.backend_mut().write_all(BEGIN_SYNCHRONIZED_UPDATE);
            }
            // If the welcome OSC-1337 image was painted earlier and the
            // user has just opened a file, wipe the screen so iTerm drops
            // its cached image cells AND ratatui repaints every cell on
            // the next draw (its diff alone misses cells whose content
            // didn't change between welcome and editor buffers).
            // A resize/WINCH is not image eviction: it invalidated the text on
            // screen (dtach reattach repaints into a blank terminal), so it
            // wipes unconditionally, Kitty included. It never fires while the
            // user merely types, so the blink fix below is untouched.
            let resize_repaint = app.consume_resize_repaint();
            if consume_any_image_clear(app) || resize_repaint {
                // iTerm2/sixel image cells live in the normal cell buffer, so
                // evicting them takes a full `2J` + repaint. Kitty images are
                // deleted on their own layer below - wiping the text there
                // blanks the whole app for an SSH round trip per latch (the
                // every-keystroke blink), so Kitty skips the screen clear.
                if resize_repaint || app.image_eviction_needs_screen_wipe() {
                    terminal.clear()?;
                }
                app.evict_inline_images_on_clear();
                // Activity-bar icons live outside ratatui too; re-emit
                // them on the next post-draw flush. The welcome wordmark
                // and the source-control hero also live in iTerm's image
                // cache that just got wiped - re-arm them so the next
                // render re-emits whichever one is currently visible.
                app.overlays.activity.mark_dirty();
                app.overlays.welcome.mark_dirty();
                app.overlays.hero.mark_dirty();
                app.forget_sent_images();
            }
            let draw_start = std::time::Instant::now();
            let drawn = terminal.draw(|f| app.render_for_terminal(f))?;
            app.sync_caret_style();
            let mut underlays = app.image_underlays(drawn.buffer);
            app.drawn_buffer = Some(drawn.buffer.clone());
            // Record render+flush time and the bytes ratatui shipped this
            // frame so the F8 HUD can show where remote latency goes.
            app.perf.record_draw(draw_start.elapsed().as_micros());
            frames_in_window += 1;
            if consume_any_image_clear(app) {
                if app.image_eviction_needs_screen_wipe() {
                    terminal.clear()?;
                }
                app.evict_inline_images_on_clear();
                app.overlays.activity.mark_dirty();
                app.overlays.welcome.mark_dirty();
                app.overlays.hero.mark_dirty();
                app.forget_sent_images();
                let drawn = terminal.draw(|f| app.render_for_terminal(f))?;
                underlays = app.image_underlays(drawn.buffer);
                app.drawn_buffer = Some(drawn.buffer.clone());
            }
            // After ratatui flushes its diff, paint the activity-bar icons
            // directly via OSC-1337 on every redraw. We previously gated
            // this on a `dirty` flag and only re-emitted on resize / view
            // change, but the icons would intermittently vanish: iTerm2
            // can drop cached OSC-1337 image cells under heavy SGR traffic
            // on adjacent cells (cursor-blink redraws, search updates,
            // mouse motion bursts) and the dirty gate had no way to detect
            // that. Since `render_activity_bar` writes nothing to the
            // buffer in image-mode, ratatui's diff produces zero per-cell
            // writes here — re-emitting the pre-encoded OSC bytes every
            // frame is cheap and locks the images in.
            app.flush_activity_image_overlays(&underlays.activity);
            // Active editor image preview: baked once, and sent after
            // ratatui's diff only when it could be missing from the screen
            // (`ImageOverlay::needs_emit`), never on every frame: a picture
            // is megabytes, and over SSH resending it per keystroke stalls
            // the session (#682). One slot per editor split column (0 =
            // left, 1 = right); when not split only slot 0 has a payload.
            for side in 0..2 {
                if let Some((osc, layout)) = app.editor_image_payload(side)
                    && app.overlay_fits_on_screen(
                        layout.cell_x,
                        layout.cell_y,
                        layout.cell_w,
                        layout.cell_h,
                    )
                {
                    if !app.overlays.editor[side].needs_emit(underlays.editor[side]) {
                        continue;
                    }
                    use std::io::Write;
                    let mut out = stdout();
                    let cursor_on = app.cursor_should_be_visible();
                    let _ = write!(out, "\x1b[?25l\x1b[s");
                    let _ = write!(out, "\x1b[{};{}H", layout.cell_y + 1, layout.cell_x + 1);
                    let _ = out.write_all(osc.as_bytes());
                    let _ = write!(out, "\x1b[u");
                    if cursor_on {
                        let _ = write!(out, "\x1b[?25h");
                    }
                    let _ = out.flush();
                    app.mark_editor_image_displayed(side);
                    app.overlays.editor[side].mark_sent_over(underlays.editor[side]);
                } else {
                    app.overlays.editor[side].forget_sent();
                }
            }
            // Terminal-pane inline image (captured imgcat output): same
            // bake-once / send-when-missing overlay, anchored to its grid row.
            if let Some((osc, layout)) = app.terminal_image_payload()
                && app.overlay_fits_on_screen(
                    layout.cell_x,
                    layout.cell_y,
                    layout.cell_w,
                    layout.cell_h,
                )
            {
                if app.overlays.terminal_image.needs_emit(underlays.terminal) {
                    use std::io::Write;
                    let mut out = stdout();
                    let cursor_on = app.cursor_should_be_visible();
                    let _ = write!(out, "\x1b[?25l\x1b[s");
                    let _ = write!(out, "\x1b[{};{}H", layout.cell_y + 1, layout.cell_x + 1);
                    let _ = out.write_all(osc.as_bytes());
                    let _ = write!(out, "\x1b[u");
                    if cursor_on {
                        let _ = write!(out, "\x1b[?25h");
                    }
                    let _ = out.flush();
                    app.mark_terminal_image_displayed();
                    app.overlays
                        .terminal_image
                        .mark_sent_over(underlays.terminal);
                }
            } else {
                app.overlays.terminal_image.forget_sent();
            }
            // Markdown-preview inline image (#176): same bake-once /
            // send-when-missing overlay, anchored at its reserved rows.
            if let Some((osc, layout)) = app.markdown_image_payload()
                && app.overlay_fits_on_screen(
                    layout.cell_x,
                    layout.cell_y,
                    layout.cell_w,
                    layout.cell_h,
                )
            {
                if app.overlays.markdown_image.needs_emit(underlays.markdown) {
                    use std::io::Write;
                    let mut out = stdout();
                    let cursor_on = app.cursor_should_be_visible();
                    let _ = write!(out, "\x1b[?25l\x1b[s");
                    let _ = write!(out, "\x1b[{};{}H", layout.cell_y + 1, layout.cell_x + 1);
                    let _ = out.write_all(osc.as_bytes());
                    let _ = write!(out, "\x1b[u");
                    if cursor_on {
                        let _ = write!(out, "\x1b[?25h");
                    }
                    let _ = out.flush();
                    app.mark_markdown_image_displayed();
                    app.overlays
                        .markdown_image
                        .mark_sent_over(underlays.markdown);
                }
            } else {
                app.overlays.markdown_image.forget_sent();
            }
            // Editor minimap: same bake-once / send-when-missing overlay,
            // painted after ratatui's diff so the raster lands on the strip.
            if let Some((osc, layout)) = app.minimap_image_payload()
                && app.overlay_fits_on_screen(
                    layout.cell_x,
                    layout.cell_y,
                    layout.cell_w,
                    layout.cell_h,
                )
            {
                if app.overlays.minimap.needs_emit(underlays.minimap) {
                    use std::io::Write;
                    let mut out = stdout();
                    let cursor_on = app.cursor_should_be_visible();
                    let _ = write!(out, "\x1b[?25l\x1b[s");
                    let _ = write!(out, "\x1b[{};{}H", layout.cell_y + 1, layout.cell_x + 1);
                    let _ = out.write_all(osc.as_bytes());
                    let _ = write!(out, "\x1b[u");
                    if cursor_on {
                        let _ = write!(out, "\x1b[?25h");
                    }
                    let _ = out.flush();
                    app.mark_minimap_image_displayed();
                    app.overlays.minimap.mark_sent_over(underlays.minimap);
                }
            } else {
                app.overlays.minimap.forget_sent();
            }
            // Run-and-Debug headline icon: same re-emit-every-frame trick.
            // Only fires while the sidebar is on the Run-Debug view and the
            // panel reserved a cell for the icon on its last render. On
            // view change the `consume_run_debug_image_clear` gate above
            // already armed `terminal.clear()`, evicting the cached
            // image cells before the next sidebar paints.
            app.flush_run_debug_icon_overlay();
            app.flush_problems_badge_overlay();
            app.flush_no_repo_hero_overlay();
            app.flush_ssh_empty_state_overlay();
            app.end_chrome_flush();
            // Welcome-screen logo: same OSC-1337 trick, gated by its own
            // dirty flag and only emitted while the editor pane is in its
            // blank initial state.
            if let Some((img, layout)) = app.welcome_image_emit_payload()
                && app.overlay_fits_on_screen(
                    layout.cell_x,
                    layout.cell_y,
                    layout.cell_w,
                    layout.cell_h,
                )
            {
                use std::io::Write;
                let mut out = stdout();
                let cursor_on = app.cursor_should_be_visible();
                let _ = write!(out, "\x1b[?25l\x1b[s");
                let _ = write!(out, "\x1b[{};{}H", layout.cell_y + 1, layout.cell_x + 1);
                let _ = out.write_all(img.as_bytes());
                let _ = write!(out, "\x1b[u");
                if cursor_on {
                    let _ = write!(out, "\x1b[?25h");
                }
                let _ = out.flush();
                app.mark_welcome_image_emitted();
            }
            // Last of all: re-paint the chrome tooltip on top of any sidebar
            // illustration it overlaps. Must run after every inline-image flush
            // above so the box wins those cells (last writer per cell wins).
            app.flush_tooltip_over_image();
            {
                use std::io::Write;
                let backend = terminal.backend_mut();
                let _ = backend.write_all(END_SYNCHRONIZED_UPDATE);
                let _ = backend.flush();
            }
            if std::mem::take(&mut first_frame) {
                crate::startup_trace::mark("first frame on screen");
                // The work held back for the frame starts now.
                crate::startup_gate::open();
                // Pre-warm the workspace's evident language servers
                // (Cargo.toml -> rust-analyzer, and so on), VS Code's
                // `workspaceContains` activation: the cold indexing overlaps
                // with the user browsing the tree instead of starting only at
                // the first file open (#27). Asked now rather than while the
                // editor was built, so a server's start never competes with
                // the first frame.
                if let Some(lsp) = app.lsp.as_ref() {
                    lsp.prewarm_workspace();
                }
                crate::startup_trace::finish();
                // A GUI launch asks the login shell for its PATH again, for
                // the next launch (#1973), only now that the frame is up:
                // the shell runs the user's whole rc, which would otherwise
                // compete for the CPU with the frame and the pane's shell.
                crate::gui_path::refresh_in_background();
            }
            needs_redraw = false;
            last_blink_visible = blink_visible;
            last_spinner_phase = spinner_phase;
        }

        // 8 ms (~125 Hz) keeps echo lag tight when the embedded pty is the
        // hot path (remote-launched croft over SSH). Idle cost is still
        // negligible because the redraw branch above gates on dirty flags;
        // this poll just decides how often we wake up to *check*.
        work_max_us = work_max_us.max(iter_start.elapsed().as_micros());
        let poll_start = std::time::Instant::now();
        if event::poll(Duration::from_millis(8))? {
            // Drain every event already queued so a click burst (Down + zero-
            // movement Drag + Up, all delivered in <50ms by the terminal)
            // coalesces into a single redraw. Otherwise each event triggers
            // its own terminal.draw cycle, which Hides+Shows the hardware
            // caret each time and the user sees the cursor blink twice
            // rapidly before settling into the normal 530ms blink.
            loop {
                match event::read()? {
                    Event::Key(key) => {
                        if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                            keys_in_window += 1;
                        }
                        app.handle_key(key)?;
                    }
                    Event::Mouse(m) => app.handle_mouse(m),
                    Event::Paste(s) => app.handle_paste(&s),
                    Event::FocusLost => {
                        app.on_window_focus_lost();
                    }
                    Event::Resize(_, _) => {
                        // Alt-screen reflow blanks the activity bar's SGR cells
                        // but iTerm2's OSC-1337 image layer survives, so a plain
                        // re-emit ghosts a fresh icon over the stale one. Re-mark
                        // dirty AND arm a one-shot terminal.clear() to evict it.
                        app.on_resize();
                        // Under dtach a reattach arrives as a bare WINCH into a
                        // terminal that never saw the startup DECSETs: without
                        // this re-assert the mouse (and bracketed paste, kitty
                        // keyboard flags, alt screen) stays dead until the next
                        // updater relaunch happens to re-exec croft.
                        if std::env::var_os("CROFT_SESSION_PERSISTENT").is_some() {
                            use std::io::Write;
                            let out = terminal.backend_mut();
                            let _ = out.write_all(&reattach_reassert_seq(app.theme));
                            let _ = out.flush();
                            // The re-assert set the steady bar; resend the
                            // overwrite block on the next draw if wanted.
                            app.caret_block_sent = false;
                        }
                    }
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            needs_redraw = true;
        }
        poll_max_us = poll_max_us.max(poll_start.elapsed().as_micros());
        // Drive edge auto-scroll for an in-progress terminal drag-selection
        // even when the pointer is held still (no fresh events): the poll
        // above wakes us every 8 ms, and this advances the selection while
        // the pointer stays pinned past the pane edge.
        if app.tick_terminal_autoscroll() {
            needs_redraw = true;
        }
    }
    Ok(())
}
