use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        MouseButton, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
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

mod click;
mod cursor_blink;
mod fs_watch;
mod git_worker;
mod hover;
mod nav;
mod overlay;
mod perf_hud;
mod sys_monitor;
mod welcome;
use click::ClickTracker;
use cursor_blink::CursorBlink;
use fs_watch::FsWatch;
use git_worker::GitWorker;
use hover::{HOVER_DELAY, HoverDwell};
use nav::{NavHistory, NavLoc};
use overlay::OverlayManager;
use perf_hud::PerfHud;
use sys_monitor::SysMonitor;
use welcome::WelcomeState;

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
    system_panel::SystemPanel,
    terminal::PtyTerminal,
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
}

/// The toggleable sub-views stacked inside the Explorer side panel, mirroring
/// VS Code's "Views and More Actions" (⋯) menu on the EXPLORER header. Ordered
/// top-to-bottom exactly as they stack in the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExplorerView {
    OpenEditors,
    Folders,
    Outline,
    Timeline,
    Dependencies,
}

impl ExplorerView {
    /// Every view, in stacking order — the order the ⋯ menu lists them.
    pub const ALL: [ExplorerView; 5] = [
        ExplorerView::OpenEditors,
        ExplorerView::Folders,
        ExplorerView::Outline,
        ExplorerView::Timeline,
        ExplorerView::Dependencies,
    ];

    /// Menu / section-header label, matching VS Code's wording. Dependencies is
    /// a static fallback only — its real label is language-aware and computed
    /// from the detected ecosystems (see [`crate::widgets::dependencies`]).
    pub fn label(self) -> &'static str {
        match self {
            ExplorerView::OpenEditors => "Open Editors",
            ExplorerView::Folders => "Folders",
            ExplorerView::Outline => "Outline",
            ExplorerView::Timeline => "Timeline",
            ExplorerView::Dependencies => "Dependencies",
        }
    }
}

/// Which Explorer sub-views are shown, toggled from the ⋯ menu. Wraps the
/// persisted [`crate::prefs::ExplorerViewsPrefs`] so the app can query/flip a
/// view by its [`ExplorerView`] without touching the serde struct's fields
/// directly.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ExplorerViewVisibility {
    flags: crate::prefs::ExplorerViewsPrefs,
}

impl ExplorerViewVisibility {
    fn from_prefs(flags: crate::prefs::ExplorerViewsPrefs) -> Self {
        Self { flags }
    }

    fn as_prefs(self) -> crate::prefs::ExplorerViewsPrefs {
        self.flags
    }

    pub fn is_visible(&self, view: ExplorerView) -> bool {
        match view {
            ExplorerView::OpenEditors => self.flags.open_editors,
            ExplorerView::Folders => self.flags.folders,
            ExplorerView::Outline => self.flags.outline,
            ExplorerView::Timeline => self.flags.timeline,
            ExplorerView::Dependencies => self.flags.dependencies,
        }
    }

    /// Flip one view's visibility, returning the new state.
    pub fn toggle(&mut self, view: ExplorerView) -> bool {
        let slot = match view {
            ExplorerView::OpenEditors => &mut self.flags.open_editors,
            ExplorerView::Folders => &mut self.flags.folders,
            ExplorerView::Outline => &mut self.flags.outline,
            ExplorerView::Timeline => &mut self.flags.timeline,
            ExplorerView::Dependencies => &mut self.flags.dependencies,
        };
        *slot = !*slot;
        *slot
    }
}

/// The clickable icons on the activity bar, used to track which one the
/// pointer is hovering. Mirrors `SidebarView` plus the bottom-anchored
/// settings gear, which is not a sidebar view but is still a hoverable
/// button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActivityIcon {
    Explorer,
    Search,
    SourceControl,
    Remote,
    RunDebug,
    Settings,
}

const ACTIVITY_BAR_WIDTH: u16 = 4;

const ACTIVITY_ICON_HEIGHT: u16 = 2;
const ACTIVITY_ICON_GAP: u16 = 0;
const FALLBACK_CELL_PIXEL: (u32, u32) = (10, 20);

fn activity_icon_glyph_x(bar: Rect) -> u16 {
    bar.x + bar.width / 2
}

/// Rank an executable basename as an lldb-dap candidate, or `None` if it is not
/// one. LLVM ships the adapter under several names: the bare `lldb-dap`, the
/// versioned `lldb-dap-18` that Debian/Ubuntu install (often with no
/// unversioned symlink), and the legacy `lldb-vscode[-NN]` name from older
/// releases. Higher rank wins: an unversioned binary (tracks the system
/// default) beats any versioned one, a newer LLVM release beats an older one,
/// and at equal version the modern `lldb-dap` name beats legacy `lldb-vscode`.
fn lldb_dap_rank(name: &str) -> Option<i64> {
    if name == "lldb-dap" {
        return Some(i64::MAX);
    }
    if name == "lldb-vscode" {
        return Some(i64::MAX - 1);
    }
    // `family` breaks ties at equal version: lldb-dap (1) over lldb-vscode (0).
    let (version, family) = name
        .strip_prefix("lldb-dap-")
        .map(|v| (v, 1))
        .or_else(|| name.strip_prefix("lldb-vscode-").map(|v| (v, 0)))?;
    let version: i64 = version.parse().ok()?;
    Some(version * 2 + family)
}

/// Resolve the `lldb-dap` adapter binary: the Xcode Command Line Tools copy
/// first (always present on a dev Mac), then the best-ranked match found by
/// scanning `PATH` for `lldb-dap`/`lldb-dap-NN`/`lldb-vscode[-NN]`. `None` if
/// none is installed.
fn lldb_dap_path() -> Option<String> {
    let xcode = "/Library/Developer/CommandLineTools/usr/bin/lldb-dap";
    if std::path::Path::new(xcode).is_file() {
        return Some(xcode.to_string());
    }
    let path_var = std::env::var_os("PATH")?;
    let mut best: Option<(i64, String)> = None;
    for dir in std::env::split_paths(&path_var) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(rank) = lldb_dap_rank(&name) else {
                continue;
            };
            let full = entry.path();
            if is_executable_file(&full) && best.as_ref().is_none_or(|(r, _)| rank > *r) {
                best = Some((rank, full.to_string_lossy().into_owned()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The error shown when no lldb-dap adapter is installed, with a host-correct
/// install hint: Xcode Command Line Tools on macOS, the LLVM package elsewhere.
fn lldb_dap_missing_message() -> String {
    let hint = if cfg!(target_os = "macos") {
        "install Xcode Command Line Tools"
    } else {
        "install LLVM, e.g. apt install lldb"
    };
    format!("lldb-dap not found ({hint})")
}

/// Whether `path` is a regular file with the executable bit set — i.e. a
/// compiled binary or script croft can run directly (`./file`) and debug via
/// lldb-dap without a build step. The honest signal for "directly executable",
/// since such files usually have no extension.
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Append variable rows to the debug tree, recursing into any expanded
/// (`variablesReference` in `expanded`) container whose children are loaded.
/// Pulled out as a free function so it can borrow the session immutably while
/// the caller mutates the row vector.
fn push_variable_rows(
    rows: &mut Vec<crate::widgets::run_debug::DebugRow>,
    session: &crate::dap::session::DapSession,
    expanded: &std::collections::HashSet<i64>,
    vars: &[crate::dap::session::Variable],
    indent: u8,
) {
    use crate::widgets::run_debug::{DebugRow, DebugRowKind};
    for v in vars {
        let expandable = v.variables_ref > 0;
        let is_open = expandable && expanded.contains(&v.variables_ref);
        rows.push(DebugRow {
            indent,
            kind: DebugRowKind::Variable {
                reference: v.variables_ref,
                expandable,
                expanded: is_open,
                name: v.name.clone(),
                value: v.value.clone(),
                type_name: v.type_name.clone(),
            },
        });
        if is_open && let Some(children) = session.variables.get(&v.variables_ref) {
            push_variable_rows(rows, session, expanded, children, indent.saturating_add(1));
        }
    }
}

fn activity_explorer_y(bar: Rect) -> u16 {
    bar.y + 1
}

fn activity_search_y(bar: Rect) -> u16 {
    activity_explorer_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
}

fn activity_source_control_y(bar: Rect) -> u16 {
    activity_search_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
}

fn activity_run_debug_y(bar: Rect) -> u16 {
    activity_source_control_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
}

fn activity_remote_y(bar: Rect) -> u16 {
    activity_run_debug_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
}

fn activity_explorer_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_explorer_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

fn activity_search_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_search_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

fn activity_source_control_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_source_control_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

fn activity_remote_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_remote_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

fn activity_run_debug_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_run_debug_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

/// The settings gear, anchored to the BOTTOM of the activity bar (VS Code's
/// "Manage" gear), asymmetric to the view icons stacked from the top. Returns
/// an empty rect when the bar is too short to fit the gear below the last view
/// icon, so the caller skips rendering and hit-testing it rather than letting
/// it collide with Run-and-Debug on a tiny terminal.
///
/// Reserves a 1-cell bottom inset (`ACTIVITY_ICON_HEIGHT + 1`) so the gap below
/// the gear matches the 1-cell top inset the view icons get from
/// `activity_explorer_y`'s `bar.y + 1`. Without it the gear hugged the row
/// directly above the status bar, reading as lopsidedly low.
const ACTIVITY_BOTTOM_INSET: u16 = 1;

fn activity_settings_block(bar: Rect) -> Rect {
    let y = bar
        .y
        .saturating_add(bar.height)
        .saturating_sub(ACTIVITY_ICON_HEIGHT + ACTIVITY_BOTTOM_INSET);
    let last_view_bottom = activity_remote_y(bar) + ACTIVITY_ICON_HEIGHT;
    if y < last_view_bottom {
        return Rect::default();
    }
    Rect {
        x: bar.x,
        y,
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
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
    /// Block occupied by the settings gear, bottom-anchored on the activity
    /// bar (VS Code's "Manage" button). Asymmetric to the view icons up top;
    /// clicking it opens the settings menu (Color Theme picker).
    settings_icon: Rect,
}

/// Pre-encoded iTerm2 OSC-1337 inline-image escape sequences for each icon
/// state. Encoded once in `App::init_graphics` (no PNG re-encoding per
/// frame) and rewritten under the activity-bar block after every ratatui
/// frame draw, since ratatui's bg-clear overdraws the image.
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
    /// The settings gear. `active` is shown while its menu is open so the
    /// button reads as pressed, mirroring the view icons' selected state;
    /// `hovered` brightens it while the pointer rests on it.
    settings_active: String,
    settings_inactive: String,
    settings_hovered: String,
}

/// Single source of truth for the application's user-facing name.
pub const APP_NAME: &str = "croft";

/// Agnoster-style status colours: clean working tree is green, any dirtiness
/// (modified, staged, or untracked) flips the pill to yellow/orange.
const GIT_CLEAN_COLOR: Color = Color::Rgb(0xa3, 0xbe, 0x8c);
const GIT_DIRTY_COLOR: Color = Color::Rgb(0xeb, 0xcb, 0x8b);

/// Build the status-bar spans for the git pill: branch glyph, branch name,
/// optional ahead/behind counts.  Colour alone carries clean/dirty state, in
/// the spirit of the Agnoster zsh theme.  Returns an empty vec outside a git
/// repo so the bar shows nothing.
fn git_status_spans<'a>(status: &'a crate::git::GitStatus) -> Vec<Span<'a>> {
    if !status.in_repo {
        return Vec::new();
    }
    let pill_color = if status.dirty {
        GIT_DIRTY_COLOR
    } else {
        GIT_CLEAN_COLOR
    };
    let mut spans: Vec<Span> = Vec::with_capacity(6);
    spans.push(Span::raw("  "));
    // Codicon `cod-source-control` (U+EA68): the Y-fork that matches the
    // activity-bar Source Control icon, so the status-bar branch indicator
    // and the SCM panel share the same visual mark.
    spans.push(Span::styled("\u{ea68} ", Style::default().fg(pill_color)));
    let label: &str = match (&status.branch, &status.detached_hash) {
        (Some(b), _) => b.as_str(),
        (None, Some(h)) => h.as_str(),
        (None, None) => "(no head)",
    };
    spans.push(Span::styled(
        label,
        Style::default().fg(pill_color).add_modifier(Modifier::BOLD),
    ));
    if status.ahead > 0 {
        spans.push(Span::styled(
            format!(" \u{2191}{}", status.ahead),
            Style::default().fg(GIT_CLEAN_COLOR),
        ));
    }
    if status.behind > 0 {
        spans.push(Span::styled(
            format!(" \u{2193}{}", status.behind),
            Style::default().fg(GIT_DIRTY_COLOR),
        ));
    }
    spans
}

/// Logo orange (#e97c41), sampled from `assets/logo.png`: the colour of the
/// `<>` angle brackets in the "cr<>ft" wordmark.
const BRAND_ORANGE: Color = Color::Rgb(0xe9, 0x7c, 0x41);

/// Spans for the "cr<>ft" wordmark at the left of the status bar, styled to
/// mirror the app logo: white letters with the `<>` (the stylised "o") in the
/// brand orange, sitting on the bar's own background rather than a pill.
fn brand_spans() -> Vec<Span<'static>> {
    let letter = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    vec![
        Span::styled(" cr", letter),
        Span::styled(
            "<>",
            Style::default()
                .fg(BRAND_ORANGE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("ft ", letter),
    ]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Tree,
    Editor,
    Terminal,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CreateKind {
    File,
    Folder,
}

/// State of a background self-update observed by a remote-launched croft.
/// `Idle` is the steady state; `InProgress` paints an "Updating…" hint in
/// the status bar; `Ready` arms the re-exec into the freshly-shipped binary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UpdateStatus {
    Idle,
    InProgress,
    Ready,
}

/// Rotating-arc frames for the in-progress update glyph. Cycled to read as a
/// spinning circular arrow, the universal "working" indicator.
const UPDATE_SPINNER_FRAMES: [&str; 6] = [
    "\u{25DC}", "\u{25E0}", "\u{25DD}", "\u{25DE}", "\u{25E1}", "\u{25DF}",
];
/// Milliseconds each spinner frame is held; ~10 frames/second reads as smooth
/// motion while keeping the status-bar-only redraws light over SSH.
const UPDATE_SPINNER_FRAME_MS: u128 = 100;

/// Pick the spinner frame for a given elapsed time. Pure so the wrap-around
/// is unit-testable without a clock.
fn update_spinner_frame(elapsed_ms: u128) -> &'static str {
    let n = UPDATE_SPINNER_FRAMES.len() as u128;
    UPDATE_SPINNER_FRAMES[((elapsed_ms / UPDATE_SPINNER_FRAME_MS) % n) as usize]
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
    /// Reveal the path in macOS Finder via `open -R`. Local-macOS only:
    /// on a remote SSH session croft runs on the headless host where no
    /// Finder exists, so the menu omits this entry entirely rather than
    /// offering a no-op.
    RevealInFinder(PathBuf),
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
    /// Split the editor into two side-by-side columns, duplicating the
    /// active file into the new (right) group.
    SplitEditor,
    /// Move keyboard focus to the left editor group (while split).
    FocusGroupLeft,
    /// Move keyboard focus to the right editor group (while split).
    FocusGroupRight,
    /// Editor body: LSP "Rename Symbol" of the identifier at buffer `(row,
    /// col)`. Captured at right-click so the later dispatch targets the same
    /// symbol the user clicked.
    RenameSymbolAt {
        row: usize,
        col: usize,
    },
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
    /// Jump straight to an already-resolved location (LSP line/character). Used
    /// for the entries of the multi-result "Go to Implementations" / "Go to
    /// References" pickers, where each row is one location; selecting it records
    /// nav history and opens the file at that position.
    GoToLocation {
        path: PathBuf,
        line: u32,
        col: u32,
    },
    /// Editor gutter: toggle a breakpoint on 1-based `line`. Built from a
    /// glyph-margin right-click; the label reads "Add Breakpoint" or "Remove
    /// Breakpoint" depending on whether one already sits on that line, but both
    /// resolve to the same toggle. Mirrors VS Code's glyph-margin menu.
    ToggleBreakpointAt {
        line: usize,
    },
    /// Editor gutter: open the breakpoint-condition editor for 1-based `line`,
    /// creating the breakpoint if absent. VS Code's "Add Conditional
    /// Breakpoint…" / "Edit Condition…".
    EditBreakpointConditionAt {
        line: usize,
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
}

/// Return the macOS-style keyboard shortcut hint to display on the right
/// side of a context-menu row, when one exists. Mirrors the bindings in
/// `handle_tree_key` / `handle_explorer_shortcut` so the menu is a
/// discovery surface for the same accelerators the keyboard accepts.
fn shortcut_for(action: &MenuAction) -> Option<&'static str> {
    match action {
        MenuAction::Create(CreateKind::File) => Some("⌘F"),
        MenuAction::Create(CreateKind::Folder) => Some("⌘⇧N"),
        MenuAction::Cut(_) => Some("⌘X"),
        MenuAction::Copy(_) => Some("⌘C"),
        MenuAction::Paste(_) => Some("⌘V"),
        MenuAction::Rename(_) => Some("⌘R"),
        MenuAction::MakeRoot(_) => Some("⌘/"),
        MenuAction::RevealInFinder(_) => Some("⌥⌘R"),
        MenuAction::Delete { .. } => Some("⌫"),
        MenuAction::CloseTab(_) => Some("⌘W"),
        MenuAction::CloseOtherTabs(_) => Some("⌥⌘T"),
        MenuAction::CloseAllTabs => Some("⌘K W"),
        MenuAction::SplitEditor => Some("⌘\\"),
        MenuAction::FocusGroupLeft => Some("⌥⌘←"),
        MenuAction::FocusGroupRight => Some("⌥⌘→"),
        MenuAction::RenameSymbolAt { .. } => Some("F2"),
        MenuAction::ChangeAllOccurrencesAt { .. } => Some("⌘F2"),
        MenuAction::GoToDefinitionAt { .. } => Some("F12"),
        MenuAction::GoToReferencesAt { .. } => Some("⇧F12"),
        MenuAction::GoToDeclarationAt { .. } => Some("⌃⇧F12"),
        MenuAction::GoToTypeDefinitionAt { .. } => Some("⌃F12"),
        MenuAction::GoToImplementationAt { .. } => Some("⌘F12"),
        MenuAction::ToggleBreakpointAt { .. } => Some("F9"),
        _ => None,
    }
}

/// The end-of-session feedback line. When breakpoints were set but the program
/// exited without ever stopping, say so explicitly — that is the common "I set a
/// breakpoint and nothing happened" confusion (e.g. running a library module
/// whose breakpointed code is never called). Otherwise a plain end message.
fn debug_end_message(had_breakpoints: bool, ever_stopped: bool) -> &'static str {
    if had_breakpoints && !ever_stopped {
        "Program exited without hitting a breakpoint"
    } else {
        "Debug session ended"
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExplorerClipMode {
    Copy,
    Cut,
}

#[derive(Clone, Debug)]
struct ExplorerClipboard {
    mode: ExplorerClipMode,
    paths: Vec<PathBuf>,
}

/// Accumulated type-to-jump state for the Explorer. `prefix` lowercase
/// only; `last` is the instant of the most recent keystroke.
#[derive(Clone, Debug)]
struct TreeTypeahead {
    prefix: String,
    last: std::time::Instant,
}

/// Inter-keystroke gap that keeps the typeahead buffer alive. Anything
/// longer than this resets the prefix to a single character and cycles
/// past the current selection so repeating the same letter walks
/// through matches.
const TREE_TYPEAHEAD_WINDOW: std::time::Duration = std::time::Duration::from_millis(750);

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

/// Build the right-click context-menu items for an editor tab strip
/// click on tab `idx`. `tab_count` is the total number of open tabs;
/// used to gate "Close Others" / "Close to the Right" so they don't
/// surface when they would be no-ops.
fn build_tab_context_menu_items(
    idx: usize,
    tab_count: usize,
    is_split: bool,
) -> Vec<(String, MenuAction)> {
    let mut items: Vec<(String, MenuAction)> = Vec::new();
    items.push((String::from("Close"), MenuAction::CloseTab(idx)));
    if tab_count > 1 {
        items.push((
            String::from("Close Others"),
            MenuAction::CloseOtherTabs(idx),
        ));
    }
    if idx + 1 < tab_count {
        items.push((
            String::from("Close to the Right"),
            MenuAction::CloseTabsToRight(idx),
        ));
    }
    items.push((String::from("Close All"), MenuAction::CloseAllTabs));
    // Editor-group actions: split is offered when not already split;
    // focus-group navigation only once a second column exists.
    if is_split {
        items.push((String::from("Focus Left Group"), MenuAction::FocusGroupLeft));
        items.push((
            String::from("Focus Right Group"),
            MenuAction::FocusGroupRight,
        ));
    } else {
        items.push((String::from("Split Editor Right"), MenuAction::SplitEditor));
    }
    items
}

/// Build the right-click context-menu items for the explorer.
///
/// * Right-click on an entry (file or non-root folder) → entry-scoped
///   actions: Cut, Copy, Paste, Rename, Delete. Multi-select promotes
///   Delete to a count and keeps Rename on a single entry only.
/// * Right-click on empty tree space, or on the workspace root row →
///   workspace-scoped actions: New File, New Folder, Paste.
fn build_tree_context_menu_items(
    node: Option<&crate::widgets::file_tree::Node>,
    root: &Path,
    selection: &[PathBuf],
    target_dir: &Path,
    clipboard: Option<&ExplorerClipboard>,
    compare_anchor: Option<&Path>,
) -> Vec<(String, MenuAction)> {
    let entry_target = crate::widgets::file_tree::delete_target_for(node, root);
    let mut items: Vec<(String, MenuAction)> = Vec::new();
    if let Some(p) = entry_target {
        // Right-click on any node (file or folder, nested or not):
        // surface New File… / New Folder… at the top so the user can
        // create a new entry next to / inside whatever they clicked,
        // without having to scroll up to the workspace root first.
        // `target_dir` is already routed by `create_target_dir_for`:
        // a folder click resolves to the folder itself, a file click
        // to the file's parent — same convention VS Code uses.
        items.push((
            String::from("New File"),
            MenuAction::Create(CreateKind::File),
        ));
        items.push((
            String::from("New Folder"),
            MenuAction::Create(CreateKind::Folder),
        ));
        let paths_for_action: Vec<PathBuf> = if selection.iter().any(|sp| sp == &p) {
            selection.to_vec()
        } else {
            vec![p.clone()]
        };
        items.push((
            String::from("Cut"),
            MenuAction::Cut(paths_for_action.clone()),
        ));
        items.push((
            String::from("Copy"),
            MenuAction::Copy(paths_for_action.clone()),
        ));
        if clipboard.is_some() {
            items.push((
                String::from("Paste"),
                MenuAction::Paste(target_dir.to_path_buf()),
            ));
        }
        if paths_for_action.len() == 1 {
            items.push((String::from("Rename"), MenuAction::Rename(p.clone())));
        }
        // Compare actions only make sense for a single regular file.
        let single_file_target = paths_for_action
            .first()
            .filter(|_| paths_for_action.len() == 1)
            .filter(|pp| pp.is_file());
        if let Some(file) = single_file_target {
            match compare_anchor {
                Some(anchor) if anchor != file.as_path() => {
                    items.push((
                        format!(
                            "Compare with Selected ({})",
                            anchor
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| anchor.display().to_string()),
                        ),
                        MenuAction::CompareWithSelected {
                            anchor: anchor.to_path_buf(),
                            other: file.clone(),
                        },
                    ));
                }
                _ => {}
            }
            items.push((
                String::from("Select for Compare"),
                MenuAction::SelectForCompare(file.clone()),
            ));
        }
        // Two regular files selected: offer a direct diff between them,
        // skipping the two-step Select-for-Compare anchor dance. Folders
        // can't be diffed, so any folder in the selection suppresses it.
        if paths_for_action.len() == 2 && paths_for_action.iter().all(|p| p.is_file()) {
            items.push((
                String::from("Compare Selected"),
                MenuAction::CompareWithSelected {
                    anchor: paths_for_action[0].clone(),
                    other: paths_for_action[1].clone(),
                },
            ));
        }
        // "Make root" appears only for non-root folders. Re-roots the
        // workspace at the clicked folder so the user can dive into a
        // nested git repo without relaunching croft.
        let single_folder_target: Option<&PathBuf> = paths_for_action
            .first()
            .filter(|_| paths_for_action.len() == 1)
            .filter(|_| node.map(|n| n.is_dir).unwrap_or(false))
            .filter(|pp| pp.as_path() != root);
        if let Some(folder) = single_folder_target {
            items.push((
                String::from("Make root"),
                MenuAction::MakeRoot(folder.clone()),
            ));
        }
        let label = if paths_for_action.len() > 1 {
            format!("Delete {} items", paths_for_action.len())
        } else {
            String::from("Delete")
        };
        items.push((
            label,
            MenuAction::Delete {
                paths: paths_for_action,
            },
        ));
    } else {
        items.push((
            String::from("New File"),
            MenuAction::Create(CreateKind::File),
        ));
        items.push((
            String::from("New Folder"),
            MenuAction::Create(CreateKind::Folder),
        ));
        if clipboard.is_some() {
            items.push((
                String::from("Paste"),
                MenuAction::Paste(target_dir.to_path_buf()),
            ));
        }
    }
    items
}

/// Insert "Reveal in Finder" into a freshly-built tree context menu, but only
/// when the action can actually do something: `enabled` is true (a local macOS
/// session) and a concrete entry was clicked (`clicked`). On a remote SSH
/// session croft runs on the headless host where there is no Finder, so the
/// item is omitted entirely rather than shown as a dead no-op, keeping the
/// local and remote menus honest. The entry lands directly beneath the New
/// File / New Folder block to mirror VS Code's top "reveal" placement.
fn maybe_add_reveal_in_finder(
    items: &mut Vec<(String, MenuAction)>,
    clicked: Option<&Path>,
    enabled: bool,
) {
    let Some(path) = clicked.filter(|_| enabled) else {
        return;
    };
    // Insert directly after the leading New File / New Folder block: find the
    // first non-Create entry and slot in ahead of it. Deriving the position
    // this way (rather than a literal index) keeps the placement correct even
    // if the Create block grows or the empty-area menu shape changes.
    let pos = items
        .iter()
        .position(|(_, a)| !matches!(a, MenuAction::Create(_)))
        .unwrap_or(items.len());
    items.insert(
        pos,
        (
            String::from("Reveal in Finder"),
            MenuAction::RevealInFinder(path.to_path_buf()),
        ),
    );
}

struct ContextMenu {
    /// Top-left of the menu in absolute terminal coordinates.
    origin: (u16, u16),
    /// Items, in display order. Each is the label + the action.
    items: Vec<(String, MenuAction)>,
    /// Highlighted row.
    selected: usize,
    /// Where any New File / New Folder should be created.
    target_dir: PathBuf,
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum PromptKind {
    Create(CreateKind),
    /// Rename the entry at `path`. The prompt's `target_dir` holds the
    /// entry's parent; `buffer` is pre-filled with the current basename.
    Rename(PathBuf),
    /// LSP "Rename Symbol" (F2 in the editor): rename the identifier at
    /// `(row, col)` in `path`. `buffer` is pre-filled with the symbol; on
    /// commit the new name drives a `textDocument/rename` request.
    RenameSymbol {
        path: PathBuf,
        row: usize,
        col: usize,
    },
    /// Debugger "Add Conditional Breakpoint…" / "Edit Condition…": edit the
    /// boolean condition for the breakpoint on 1-based `line` of `path`. The
    /// buffer is pre-filled with any existing condition; on commit the
    /// breakpoint is created (if absent) and the condition attached/cleared,
    /// then pushed to a live session. A popup, never the status line.
    BreakpointCondition {
        path: PathBuf,
        line: usize,
    },
}

struct Prompt {
    label: String,
    buffer: String,
    kind: PromptKind,
    target_dir: PathBuf,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingDiscard {
    rel_path: String,
    untracked: bool,
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

pub struct App {
    pub tree: FileTree,
    pub search: SearchPanel,
    pub remote: RemotePanel,
    pub source_control: SourceControlPanel,
    pub run_debug: RunDebugPanel,
    /// The Explorer's OPEN EDITORS section: the open editor tabs, projected
    /// from `editor`/`editor_split` each frame. See [`OpenEditorsPanel`].
    pub open_editors: OpenEditorsPanel,
    /// The Explorer's OUTLINE section, stacked below the file tree: the active
    /// editor's `documentSymbol` tree. See [`OutlinePanel`].
    pub outline: OutlinePanel,
    /// The Explorer's TIMELINE section: the active file's git history, fetched
    /// off-thread via [`crate::git::file_history`]. See [`TimelinePanel`].
    pub timeline: TimelinePanel,
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
    /// Background `git log` results for the TIMELINE, keyed by the file they
    /// describe so a stale reply for a since-closed file is ignored on drain.
    timeline_rx: std::sync::mpsc::Receiver<(PathBuf, Vec<crate::git::FileHistoryEntry>)>,
    timeline_tx: std::sync::mpsc::Sender<(PathBuf, Vec<crate::git::FileHistoryEntry>)>,
    /// The file the TIMELINE has been fetched (or is being fetched) for, so the
    /// fetch fires exactly once per active-file change.
    timeline_fetched: Option<PathBuf>,
    /// Background per-ecosystem dependency-resolution result for DEPENDENCIES.
    deps_rx: std::sync::mpsc::Receiver<Vec<Dependency>>,
    deps_tx: std::sync::mpsc::Sender<Vec<Dependency>>,
    /// The workspace root DEPENDENCIES was detected/fetched for, so detection
    /// and the fetch fire once per root (re-fired by Make Root / re-root).
    deps_fetched: Option<PathBuf>,
    pub system_panel: SystemPanel,
    pub editor: EditorTabs,
    /// The secondary editor group, shown side by side with `editor` when
    /// the user splits the pane (`Cmd+\`). `None` = not split. Invariant:
    /// `editor` is ALWAYS the focused group, so the 430-odd `self.editor`
    /// call sites keep operating on whichever pane has focus. Moving focus
    /// to the other group `mem::swap`s the two so the invariant holds.
    pub editor_split: Option<EditorTabs>,
    /// While split, `true` when the focused group (`editor`) occupies the
    /// LEFT column and `editor_split` the right. A focus swap flips this so
    /// the physical columns stay put. Meaningless when not split.
    split_focus_left: bool,
    /// Pinned width in cells of the LEFT editor column while split. `None`
    /// = even 50/50 split. Set by dragging the seam between the two panes;
    /// mirrors `sidebar_width` / `terminal_height`.
    editor_split_left_width: Option<u16>,
    /// One-shot latch armed when a save is refused because the file changed
    /// on disk. The next `save()` overwrites anyway, giving the user a
    /// "press Cmd+S again to overwrite" escape hatch without a modal.
    force_save_armed: bool,
    pub terminals: Vec<PtyTerminal>,
    pub active_terminal: usize,
    perf: PerfHud,
    workspace_root: PathBuf,
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
    prompt: Option<Prompt>,
    fs_watch: FsWatch,
    git: GitWorker,
    sysmon: SysMonitor,
    cursor_blink: CursorBlink,
    editor_click: ClickTracker,
    tree_click: ClickTracker,
    terminal_click: ClickTracker,
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
    /// A left-drag is moving the OUTLINE panel's scrollbar thumb. The outline
    /// is not a focusable `Pane`, so it gets its own drag latch rather than
    /// riding `scrollbar_drag`.
    outline_scrollbar_drag: bool,
    /// Scrollbar-thumb drag latches for the other stacked Explorer sub-views,
    /// matching `outline_scrollbar_drag` (none is a focusable `Pane`).
    open_editors_scrollbar_drag: bool,
    timeline_scrollbar_drag: bool,
    deps_scrollbar_drag: bool,
    welcome: WelcomeState,
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
    /// Clipboard read entrypoint. Production uses the host clipboard; tests
    /// can swap in a deterministic reader for Cmd+V routing assertions.
    clipboard_reader: fn() -> Option<String>,
    remote_launch: Option<RemoteLaunch>,
    pending_remote_launch_host: Option<String>,
    pending_remote_launch_path: Option<String>,
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
    /// Last-rendered x of the seam between the two editor columns while
    /// split. `None` when not split. Used to hit-test the editor-split
    /// drag, mirroring `sidebar_splitter_x`.
    editor_splitter_x: Option<u16>,
    /// Hit-test rectangles of the "[+]" buttons - one per terminal pane,
    /// indexed in lock-step with `terminals`. Empty when the pane is hidden
    /// or every pane is too narrow for the label.
    terminal_add_buttons: Vec<Rect>,
    /// Hit-test rectangles of the "[-]" buttons - one per terminal pane,
    /// indexed in lock-step with `terminals`. Empty when only one terminal
    /// is open (closing the last one would leave the pane empty, which we
    /// explicitly forbid) or the pane is hidden.
    terminal_close_buttons: Vec<Rect>,
    /// Last pointer cell reported by a `Moved` event. Render-time hover
    /// hit-testing (the Black theme's teal pill behind the terminal `-`/`+`
    /// buttons) reads this instead of tracking enter/leave transitions.
    pointer_cell: Option<(u16, u16)>,
    /// Which activity-bar icon the pointer is currently over, if any. Drives
    /// the hover variant of the icon (brightened glyph, no selection pill).
    /// Changing it arms `overlays.activity.mark_dirty()` so the swapped
    /// pre-baked image is re-emitted; it never triggers a re-bake.
    hovered_activity_icon: Option<ActivityIcon>,
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
    /// Diagnostics store: per file, the latest set from each server. Servers
    /// push complete sets via `textDocument/publishDiagnostics`, so the inner
    /// map is keyed by server name (ty + ruff can both publish for one file)
    /// and a server's entry is replaced wholesale on each batch (an empty
    /// batch removes it). The merged value for the visible file is handed to
    /// the editor; the store outlives tab switches so re-focusing a file shows
    /// its squiggles again without waiting for the next server push.
    lsp_diagnostics: std::collections::HashMap<
        PathBuf,
        std::collections::HashMap<String, Vec<crate::lsp::manager::Diagnostic>>,
    >,
    /// Live LSP work-done progress, keyed by server name (e.g. "rust-analyzer"
    /// -> "Indexing 112/340 33%"). An entry exists only while that server has
    /// an active task; the status bar surfaces it so a busy-priming server is
    /// never mistaken for a dead one. Cleared on the server's `End`.
    lsp_progress: std::collections::HashMap<String, String>,
    /// Active completion popup, when a completion request has been issued
    /// and the user hasn't dismissed the result. Populated by drain_lsp_completion
    /// once the worker sends a CompletionResult back.
    pub completion_popup: Option<crate::widgets::completion_popup::CompletionPopup>,
    /// Most recent completion request id; later responses with a stale
    /// id are dropped so a slow earlier response cannot clobber a fresh
    /// one (e.g. user already moved past the original trigger).
    completion_request_id: Option<u64>,
    hover: HoverDwell,
    hover_popup: Option<crate::widgets::hover_popup::HoverPopup>,
    hover_request_id: Option<u64>,
    hover_anchor: Option<(u16, u16)>,
    hover_word: Option<(usize, usize, usize)>,
    /// Diagnostic block for the point a hover is anchored to, captured
    /// synchronously when the dwell fires (server diagnostics already live in
    /// the editor). Stacked above the async LSP hover reply by `compose_hover`
    /// so a squiggle's message shows even when the server returns no type info.
    hover_diagnostic: Option<String>,
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
    ui_tooltip_label: Option<&'static str>,
    definition_request_id: Option<u64>,
    declaration_request_id: Option<u64>,
    type_definition_request_id: Option<u64>,
    /// The `(active file, last-seen LSP edit seq)` the OUTLINE was last synced
    /// to, so `sync_outline` re-requests symbols exactly once per tab switch or
    /// edit batch. `None` seq means the file has no language server.
    outline_synced: Option<(PathBuf, Option<u64>)>,
    implementation_request_id: Option<u64>,
    references_request_id: Option<u64>,
    rename_request_id: Option<u64>,
    nav: NavHistory,
    editor_vim_chord: EditorVimChord,
    /// Native modal (vim-style) editing for the editor pane. When enabled it
    /// supersedes the always-on `editor_vim_chord` convenience layer.
    vim: crate::vim::VimState,
    /// Last `f`/`t`/`F`/`T` target, replayed by `;` and `,`.
    vim_last_find: Option<(crate::vim::FindKind, char)>,
    /// Whether the active visual selection is linewise (`V`), so the pending
    /// visual operator deletes/yanks whole lines.
    vim_visual_line: bool,
    shortcuts_modal: Option<crate::widgets::shortcuts::ShortcutsModal>,
    shortcuts_hit_rect: Option<Rect>,
    connect_dialog: Option<crate::widgets::connect_dialog::ConnectDialog>,
    connect_auth: Option<crate::remote_connect::SshAuth>,
    install_session: Option<crate::install_session::InstallSession>,
    /// Background self-update watcher, present only on a remote-launched
    /// croft (gated by `CROFT_REMOTE_AUTOUPDATE`). Polls the install-stamp
    /// the local cross-build writes when it finishes shipping a newer
    /// binary, so the running croft can re-exec into it in place.
    update_watch: Option<crate::update_watch::UpdateWatch>,
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
    /// VS Code-style Cmd+P / Ctrl+P quick-open file finder. None when
    /// the modal is closed.
    pub file_finder: Option<crate::widgets::file_finder::FileFinder>,
    /// VS Code-style Cmd+Shift+P / Ctrl+Shift+P command palette. None when
    /// the modal is closed.
    pub command_palette: Option<crate::widgets::command_palette::CommandPalette>,
    /// "Debug: Attach to Python Process" picker (PEP 768). None when closed.
    /// Lists running CPython 3.14+ processes; selecting one spawns
    /// `pdb -p <pid>` in a PTY.
    pub process_picker: Option<crate::widgets::process_picker::ProcessPicker>,
    /// Active debug session: a debugpy DAP launch. None when not debugging.
    /// Polled each frame by [`App::poll_dap`], which mirrors the paused location
    /// into `editor.stop_line`.
    pub dap_session: Option<crate::dap::session::DapSession>,
    /// Variable `variablesReference`s the user has expanded in the Variables
    /// panel. Persisted across polls so re-fetched variables stay open.
    pub debug_expanded: std::collections::HashSet<i64>,
    /// Debug console log: program stdout/stderr (DAP output events) plus REPL
    /// echoes and results. Capped; the tail is shown in the Run & Debug panel.
    pub debug_console: Vec<String>,
    /// True once the current/just-ended debug session stopped at least once
    /// (breakpoint / step / exception). Drives the "exited without hitting a
    /// breakpoint" end message. Reset when a new session starts.
    debug_ever_stopped: bool,
    /// zoxide-backed Explorer jump popup (Cmd+Z). None when closed. Opens
    /// only from the Explorer pane, so it never collides with the editor's
    /// Cmd+Z undo.
    pub zoxide_jump: Option<crate::widgets::zoxide_jump::ZoxideJump>,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum VimOp {
    #[default]
    None,
    G,
    D,
    Y,
}

#[derive(Clone, Copy, Debug, Default)]
struct EditorVimChord {
    op: VimOp,
    count: usize,
}

impl EditorVimChord {
    fn reset(&mut self) {
        self.op = VimOp::None;
        self.count = 0;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorImageLayout {
    pub cell_x: u16,
    pub cell_y: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SplitterDrag {
    Sidebar,
    Terminal,
    /// Dragging the vertical seam between the two editor columns while
    /// the editor is split side by side.
    EditorSplit,
}

/// Minimum width in cells of either editor column while split, so a drag
/// can't collapse a pane to nothing.
const EDITOR_SPLIT_MIN: u16 = 10;

const SIDEBAR_WIDTH_DEFAULT: u16 = 32;
const SIDEBAR_WIDTH_MIN: u16 = 12;
const TERMINAL_HEIGHT_MIN: u16 = 3;
const EDITOR_HEIGHT_MIN: u16 = 3;
/// Shortest frame that still shows the on-screen keyboard band; below this
/// the workspace would be unusably squeezed, so the band collapses instead.
const OSK_MIN_FRAME_HEIGHT: u16 = 16;
const RIGHT_PANE_MIN: u16 = 20;
/// Minimum width of the zoxide jump popup. The Explorer column it anchors
/// over is typically ~32 cells, too narrow to read frecency paths like
/// `~/Documents/platform/app/...`, so the box widens rightward to at least
/// this many cells (capped to the frame) while staying left-anchored on
/// the Explorer.
const ZOXIDE_JUMP_MIN_WIDTH: u16 = 56;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemotePullKind {
    /// File or directory copied from the user's local Mac into the
    /// inbox; on completion the relay file is moved into `dest_dir`.
    File,
    /// Local-Mac clipboard contents staged at `<inbox>/<id>/clipboard.txt`;
    /// on completion the bytes are pasted into the focused terminal.
    Clipboard,
    /// URL handed to the user's local Mac `open(1)`; on completion croft
    /// just surfaces a status confirmation. No payload is shipped back.
    Open,
}

const REMOTE_PULL_TIMEOUT: Duration = Duration::from_secs(120);

struct RemoteLaunch {
    host: String,
    path: Option<String>,
    adopted: Option<crate::remote::AdoptedMaster>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WelcomeLink {
    rect: Rect,
    url: String,
    label: String,
}

fn welcome_provider_label(remote: &str) -> &'static str {
    crate::git::commit_api_provider_for_remote(remote)
        .map(crate::git::CommitApiProvider::label)
        .unwrap_or("Repo")
}

fn welcome_provider_badge(remote: &str) -> String {
    match crate::git::commit_api_provider_for_remote(remote) {
        Some(crate::git::CommitApiProvider::Bitbucket) => "\u{f171} Bitbucket".to_string(),
        Some(crate::git::CommitApiProvider::GitHub) => "\u{f09b} GitHub".to_string(),
        // Codeberg has no reliable Nerd Font codepoint (many fonts ship
        // without one), and the previous `\u{ea60}` placeholder rendered
        // as the wrong symbol. Three leading spaces reserve the layout:
        // two cells for the OSC-1337 image overlay that paints the actual
        // Codeberg logo on iTerm2, plus one cell of visual gap so the
        // logo doesn't butt against the "C". On non-image terminals the
        // badge reads as plain "   Codeberg".
        Some(crate::git::CommitApiProvider::Codeberg) => "   Codeberg".to_string(),
        None => "Repo".to_string(),
    }
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

fn welcome_commit_widths(commit: &crate::git::CommitInfo, block_w: u16) -> (u16, u16) {
    let hash_w = commit.hash.chars().count() as u16;
    let when_w = commit.when.chars().count() as u16;
    let prefix_w = hash_w.saturating_add(1);
    let first = block_w
        .saturating_sub(prefix_w)
        .saturating_sub(when_w.saturating_add(2))
        .max(1);
    let rest = block_w.saturating_sub(prefix_w).max(1);
    (first, rest)
}

fn wrapped_welcome_commit_subject(commit: &crate::git::CommitInfo, block_w: u16) -> Vec<String> {
    let (first, rest) = welcome_commit_widths(commit, block_w);
    wrap_cells_variable_width(&commit.subject, first, rest)
}

fn welcome_commit_row_height(commit: &crate::git::CommitInfo, block_w: u16) -> u16 {
    wrapped_welcome_commit_subject(commit, block_w).len().max(1) as u16
}

fn welcome_recents_height(
    remote: Option<&str>,
    commits: &[crate::git::CommitInfo],
    block_w: u16,
) -> u16 {
    if remote.is_none() && commits.is_empty() {
        return 0;
    }
    let remote_h = u16::from(remote.is_some());
    let commit_h = if commits.is_empty() {
        0
    } else {
        1 + commits
            .iter()
            .map(|c| welcome_commit_row_height(c, block_w))
            .sum::<u16>()
    };
    1 + remote_h + commit_h
}

const WELCOME_TAGLINE: &str = "LIGHTWEIGHT.  BLAZINGLY FAST.  BUILT FOR DEVELOPERS.";

use crate::gradient::{GRAD_TL, GRAD_TR, POPUP_SEL_BG, paint_gradient_box, rgb_color};

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
    cmd.spawn().with_context(|| format!("opening {url}"))?;
    Ok(())
}

impl App {
    pub fn new(root: PathBuf) -> Result<Self> {
        let tree = FileTree::new(root.clone());
        let search = SearchPanel::new(root.clone());
        let remote = RemotePanel::new();
        let source_control = SourceControlPanel::new();
        let run_debug = RunDebugPanel::new();
        let outline = OutlinePanel::new();
        let open_editors = OpenEditorsPanel::new();
        let timeline = TimelinePanel::new();
        let mut dependencies = DependenciesPanel::new();
        // Detect the workspace's package ecosystems up front so the DEPENDENCIES
        // section's visibility, ⋯-menu entry, and header are correct before the
        // first sync tick. Re-detected on every re-root (see sync_explorer_panels).
        let dep_ecosystems = crate::widgets::dependencies::detect_ecosystems(&root);
        dependencies.set_header(crate::widgets::dependencies::header_label(&dep_ecosystems));
        let explorer_views = ExplorerViewVisibility::from_prefs(
            crate::prefs::Prefs::load_or_default().explorer_views,
        );
        let (timeline_tx, timeline_rx) = std::sync::mpsc::channel();
        let (deps_tx, deps_rx) = std::sync::mpsc::channel();
        let editor = EditorTabs::new();
        let term = PtyTerminal::new(&root).context("spawning terminal")?;

        // Background git worker: every `git status` / `git status
        // --porcelain` shell-out goes through this channel instead of
        // running on the UI thread. The very first request seeds the
        // status-bar badge; subsequent ones come from the FS watcher
        // and from sidebar-icon clicks (see `refresh_source_control`,
        // `refresh_git_status_debounced`). Removes the 15-50 ms
        // input-to-paint stall that used to fire on every Source
        // Control click.
        let git = GitWorker::spawn(root.clone());

        let sysmon = SysMonitor::spawn();

        let welcome = WelcomeState::spawn();

        let (search_query_tx, search_query_rx) = std::sync::mpsc::channel();
        let (search_results_tx, search_results_rx) = std::sync::mpsc::channel();
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
            let entries = crate::widgets::file_finder::build_file_index(&index_root);
            let _ = file_finder_index_tx.send(entries);
        });
        let fs_watch = FsWatch::spawn(&root, &tree);
        let mut app = Self {
            tree,
            search,
            remote,
            source_control,
            run_debug,
            outline,
            open_editors,
            timeline,
            dependencies,
            dep_ecosystems,
            explorer_views,
            timeline_rx,
            timeline_tx,
            timeline_fetched: None,
            deps_rx,
            deps_tx,
            deps_fetched: None,
            system_panel: {
                let mut p = SystemPanel::new();
                // Pre-existing app/SC tests assume the sidebar column
                // has the full main-pane height. The SYSTEM panel
                // would otherwise eat ~3 rows from the activity
                // widget and shrink the SCM list under its
                // entry-rendering threshold. Hide it by default in
                // test builds; tests that exercise the panel itself
                // call `set_hidden(false)`.
                if cfg!(test) {
                    p.set_hidden(true);
                }
                p
            },
            editor,
            editor_split: None,
            split_focus_left: true,
            editor_split_left_width: None,
            force_save_armed: false,
            terminals: vec![term],
            active_terminal: 0,
            perf: PerfHud::default(),
            workspace_root: root.clone(),
            sidebar_view: SidebarView::Explorer,
            sidebar_areas: SidebarAreas::default(),
            // Read the persisted theme from the real config only outside tests;
            // under test fall back to the default so the suite stays hermetic
            // and never depends on (nor mutates) the developer's ~/.config. This
            // mirrors the cfg!(test) write-skip in `apply_theme`.
            theme: if cfg!(test) {
                crate::theme::Theme::default()
            } else {
                crate::prefs::Prefs::load_or_default().theme()
            },
            focus: Pane::Tree,
            show_tree: true,
            show_terminal: true,
            terminal_maximized: false,
            status: String::from("Ready"),
            persistence_warning: remote_persistence_status(
                is_remote_session(),
                std::env::var_os("CROFT_SESSION_PERSISTENT").is_some(),
            ),
            quit: false,
            drop_to_local: false,
            is_remote: is_remote_session(),
            context_menu: None,
            prompt: None,
            fs_watch,
            git,
            sysmon,
            cursor_blink: CursorBlink::new(),
            editor_click: ClickTracker::default(),
            tree_click: ClickTracker::default(),
            terminal_click: ClickTracker::default(),
            terminal_select_autoscroll: None,
            scrollbar_drag: None,
            editor_hscrollbar_drag: false,
            outline_scrollbar_drag: false,
            open_editors_scrollbar_drag: false,
            timeline_scrollbar_drag: false,
            deps_scrollbar_drag: false,
            welcome,
            search_query_tx,
            search_results_rx,
            cell_pixel: None,
            inline_protocol: crate::iterm2_inline::detect_inline_image_protocol(),
            sixel_supported: false,
            clipboard_reader: read_system_clipboard,
            remote_launch: None,
            pending_remote_launch_host: None,
            pending_remote_launch_path: None,
            tree_clipboard: None,
            tree_typeahead: None,
            compare_anchor: None,
            last_frame_area: Rect::default(),
            last_sidebar_area: None,
            tree_drag: None,
            pending_scp_uploads: Vec::new(),
            pending_remote_pulls: Vec::new(),
            pending_local_open: None,
            pending_discard: None,
            pending_terminal_warning: false,
            commit_menu_open: false,
            default_branch_label: None,
            trust_local_browser: false,
            sidebar_width: SIDEBAR_WIDTH_DEFAULT,
            terminal_height: None,
            splitter_drag: None,
            editor_splitter_x: None,
            sidebar_splitter_x: None,
            terminal_splitter_y: None,
            terminal_add_buttons: Vec::new(),
            terminal_close_buttons: Vec::new(),
            pointer_cell: None,
            hovered_activity_icon: None,
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
            lsp_diagnostics: std::collections::HashMap::new(),
            lsp_progress: std::collections::HashMap::new(),
            completion_popup: None,
            editor_vim_chord: EditorVimChord::default(),
            vim: crate::vim::VimState::new(),
            vim_last_find: None,
            vim_visual_line: false,
            shortcuts_modal: None,
            shortcuts_hit_rect: None,
            connect_dialog: None,
            connect_auth: None,
            install_session: None,
            update_watch: None,
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
            file_finder: None,
            command_palette: None,
            process_picker: None,
            dap_session: None,
            debug_expanded: std::collections::HashSet::new(),
            debug_console: Vec::new(),
            debug_ever_stopped: false,
            zoxide_jump: None,
            file_finder_index: None,
            file_finder_index_rx: Some(file_finder_index_rx),
            file_finder_index_dirty: false,
            completion_request_id: None,
            rename_request_id: None,
            hover: HoverDwell::default(),
            hover_popup: None,
            hover_request_id: None,
            hover_anchor: None,
            hover_word: None,
            hover_diagnostic: None,
            tab_hover: HoverDwell::default(),
            tab_tooltip: None,
            tab_hover_idx: None,
            ui_hover: HoverDwell::default(),
            ui_tooltip: None,
            ui_tooltip_label: None,
            definition_request_id: None,
            outline_synced: None,
            declaration_request_id: None,
            type_definition_request_id: None,
            implementation_request_id: None,
            references_request_id: None,
            nav: NavHistory::default(),
        };
        // Initialise the per-pane focus/gradient flags to match the starting
        // pane (Tree) and persisted theme. Without this, the explorer boots
        // focused-but-not-gradient: the Black-theme gradient border only
        // arms on the first focus change because nothing ran the sync at
        // construction time.
        app.sync_focus_flags();
        Ok(app)
    }

    /// Detect inline-image support via env vars only — no stdin queries, no
    /// raw-mode contention. Queries the terminal cell pixel size via
    /// crossterm's `window_size` (TIOCGWINSZ ioctl, no stdin involvement).
    /// SSH PTYs often report only rows/columns and leave pixel dimensions
    /// as zero, so fall back to a sane 10×20 cell estimate; OSC-1337 still
    /// scales the image to the requested cell rectangle.
    /// The active theme's background as a fully-opaque image pixel. Used to
    /// fill the OSC-1337 icon/hero canvases so the baked PNGs sit on the same
    /// color as the surrounding (session-bg) panes, with no visible seam.
    fn theme_bg_pixel(&self) -> image::Rgba<u8> {
        let (r, g, b) = self.theme.editor_bg_rgb();
        image::Rgba([r, g, b, 0xff])
    }

    /// True when the host terminal can render inline images via either
    /// protocol. Gates the bake/emit path; the iTerm2-only `SetColors` /
    /// `Color::Reset` tricks stay behind `detect_iterm2_inline_support`.
    fn inline_images_enabled(&self) -> bool {
        self.inline_protocol != crate::iterm2_inline::InlineImageProtocol::None
    }

    /// Kitty graphics live on a layer that survives the `\x1b[2J` the main
    /// loop fires to evict iTerm2's image cells, so on the Kitty path that
    /// same clear frame must also delete every placement; the visible
    /// overlays re-emit themselves on the following post-draw flush. No-op on
    /// the iTerm2 path, where the clear already evicted the cached cells.
    pub fn evict_inline_images_on_clear(&self) {
        use std::io::Write;
        if self.inline_protocol == crate::iterm2_inline::InlineImageProtocol::Kitty {
            let mut out = stdout();
            let _ = out.write_all(crate::iterm2_inline::delete_all_kitty_images().as_bytes());
            let _ = out.flush();
        }
    }

    pub fn init_graphics(&mut self) {
        // Env-advertised protocols (iTerm2/Kitty) win; otherwise fall back to a
        // sixel host detected earlier by the DA1 probe. Re-derived here so the
        // theme-switch re-bake keeps the right protocol, and `sixel_supported`
        // carries the probe result across the env re-detect (sixel has none).
        self.inline_protocol = match crate::iterm2_inline::detect_inline_image_protocol() {
            crate::iterm2_inline::InlineImageProtocol::None if self.sixel_supported => {
                crate::iterm2_inline::InlineImageProtocol::Sixel
            }
            other => other,
        };
        if !self.inline_images_enabled() {
            return;
        }
        let Ok(ws) = crossterm::terminal::window_size() else {
            return;
        };
        if ws.columns == 0 || ws.rows == 0 {
            return;
        }
        let (cell_w, cell_h) = if ws.width > 0 && ws.height > 0 {
            (
                (ws.width / ws.columns).max(1) as u32,
                (ws.height / ws.rows).max(1) as u32,
            )
        } else {
            FALLBACK_CELL_PIXEL
        };
        self.cell_pixel = Some((cell_w, cell_h));
        // OSC-1337 hero will own the no-repo illustration cells; the
        // widget skips its ASCII fallback so resize doesn't flash old
        // text through the image.
        self.source_control.inline_hero_image_active = true;
        let canvas_w = cell_w * ACTIVITY_BAR_WIDTH as u32;
        let canvas_h = cell_h * ACTIVITY_ICON_HEIGHT as u32;
        let is_tmux = crate::iterm2_inline::detect_tmux();
        let w_cells = ACTIVITY_BAR_WIDTH;
        let h_cells = ACTIVITY_ICON_HEIGHT;
        let icon_bg = self.theme_bg_pixel();
        let protocol = self.inline_protocol;
        // The activity-bar icons are codicon SVGs, rasterised at the exact icon
        // pixel size (no multi-MB raster decode, crisp at any cell size). `id`
        // is the icon's stable per-slot Kitty image/placement id (shared by its
        // active and inactive variants so swapping one for the other replaces
        // the placement in place); ignored on the iTerm2 path.
        let encode = |src_svg: &[u8],
                      state: crate::iterm2_inline::IconState,
                      off_y_bias: i64,
                      id: u32|
         -> Option<String> {
            let baked = crate::iterm2_inline::compose_icon_svg(
                src_svg, canvas_w, canvas_h, state, icon_bg, off_y_bias,
            )?;
            // preserveAspectRatio=0: stretch to exactly fill 4×2 cells.
            // Since the canvas was composed at exactly that pixel size,
            // there's no actual scaling and the codicon's square area
            // remains a true square on screen.
            let raw = crate::iterm2_inline::build_inline_image(
                protocol, &baked, w_cells, h_cells, false, id,
            )?;
            Some(crate::iterm2_inline::maybe_tmux_wrap(
                protocol, is_tmux, raw,
            ))
        };
        // View icons render dead-centered in their cell block; the gear gets a
        // downward bias so it bottom-aligns within its (bottom-inset) block,
        // shaving the canvas's bottom padding off the gap above the status bar.
        // The value need only exceed the no-clip ceiling; `compose_icon_svg`
        // clamps it, so this stays correct at any cell size.
        let gear_off_y_bias = canvas_h as i64;
        use crate::iterm2_inline as ki;
        // Bake all three states per icon up front (selected / hovered / resting)
        // so the render path only ever swaps which pre-encoded sequence it emits
        // — a hover never triggers a re-bake, just a different cached string.
        let bake = |src: &[u8], off_y_bias: i64, id: u32| -> Option<(String, String, String)> {
            Some((
                encode(src, ki::IconState::Active, off_y_bias, id)?,
                encode(src, ki::IconState::Inactive, off_y_bias, id)?,
                encode(src, ki::IconState::Hovered, off_y_bias, id)?,
            ))
        };
        if let (
            Some((ea, ei, eh)),
            Some((sa, si, sh)),
            Some((sca, sci, sch)),
            Some((ra, ri, rh)),
            Some((rda, rdi, rdh)),
            Some((gea, gei, geh)),
        ) = (
            bake(ki::EXPLORER_SRC_SVG, 0, ki::KITTY_ID_EXPLORER),
            bake(ki::SEARCH_SRC_SVG, 0, ki::KITTY_ID_SEARCH),
            bake(ki::SOURCE_CONTROL_SRC_SVG, 0, ki::KITTY_ID_SOURCE_CONTROL),
            bake(ki::REMOTE_SRC_SVG, 0, ki::KITTY_ID_REMOTE),
            bake(ki::RUN_DEBUG_SRC_SVG, 0, ki::KITTY_ID_RUN_DEBUG_BAR),
            bake(
                ki::SETTINGS_GEAR_SRC_SVG,
                gear_off_y_bias,
                ki::KITTY_ID_SETTINGS,
            ),
        ) {
            self.overlays.activity.set_images(ActivityBarImages {
                explorer_active: ea,
                explorer_inactive: ei,
                explorer_hovered: eh,
                search_active: sa,
                search_inactive: si,
                search_hovered: sh,
                source_control_active: sca,
                source_control_inactive: sci,
                source_control_hovered: sch,
                remote_active: ra,
                remote_inactive: ri,
                remote_hovered: rh,
                run_debug_active: rda,
                run_debug_inactive: rdi,
                run_debug_hovered: rdh,
                settings_active: gea,
                settings_inactive: gei,
                settings_hovered: geh,
            });
        }
        // Codeberg badge for the welcome panel: 2 cells wide, 1 cell tall,
        // rendered as an OSC-1337 image overlay at the badge's anchor cell
        // because Nerd Fonts have no reliable Codeberg codepoint.
        let badge_w_px = cell_w * 2;
        let badge_h_px = cell_h;
        if let Ok(baked) = crate::iterm2_inline::fit_image_auto(
            crate::iterm2_inline::CODEBERG_SRC_PNG,
            badge_w_px,
            badge_h_px,
            icon_bg,
        ) && let Some(raw) = crate::iterm2_inline::build_inline_image(
            protocol,
            &baked,
            2,
            1,
            false,
            crate::iterm2_inline::KITTY_ID_BADGE,
        ) {
            self.overlays
                .badge
                .set_image(crate::iterm2_inline::maybe_tmux_wrap(
                    protocol, is_tmux, raw,
                ));
        }
        // Run-and-Debug headline icon: the same debug-alt PNG used in the
        // activity bar, fitted to the panel's icon block (RUN_DEBUG_ICON_CELLS_W
        // cells wide × RUN_DEBUG_ICON_CELLS_H cells tall) and tinted at icon-
        // inactive grey so it reads as the same monochrome family as the rest
        // of the activity-bar icons. Encoded once at init; emitted post-frame
        // by `flush_run_debug_icon_overlay` while the panel is visible.
        let rd_icon_w_cells = crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_W as u32;
        let rd_icon_h_cells = crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_H as u32;
        let rd_icon_w_px = cell_w * rd_icon_w_cells;
        let rd_icon_h_px = cell_h * rd_icon_h_cells;
        if let Ok(baked) = crate::iterm2_inline::compose_icon(
            crate::iterm2_inline::RUN_DEBUG_SRC_PNG,
            rd_icon_w_px,
            rd_icon_h_px,
            false,
            icon_bg,
            0,
        ) && let Some(raw) = crate::iterm2_inline::build_inline_image(
            protocol,
            &baked,
            crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_W,
            crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_H,
            false,
            crate::iterm2_inline::KITTY_ID_RUN_DEBUG_PANEL,
        ) {
            self.overlays
                .run_debug
                .set_image(crate::iterm2_inline::maybe_tmux_wrap(
                    protocol, is_tmux, raw,
                ));
        }
        // SSH empty-state illustration baked to a fixed cell box (18 × 8
        // cells). Letterboxed onto the card's dark bg so iTerm2 has no
        // transparent edge to ghost the underlying buffer. Emitted post-
        // draw by `flush_ssh_empty_state_overlay` while the SSH section
        // shows no hosts.
        let ssh_cells_w: u16 = crate::widgets::remote::SSH_EMPTY_STATE_CELLS_W;
        let ssh_cells_h: u16 = crate::widgets::remote::SSH_EMPTY_STATE_CELLS_H;
        let ssh_w_px = cell_w * ssh_cells_w as u32;
        let ssh_h_px = cell_h * ssh_cells_h as u32;
        // Transparent letterbox so the host terminal's session bg shows
        // through the empty padding around the illustration. Any opaque
        // fill here would render as a visible darker rectangle behind the
        // PNG, which is exactly what the user told us to remove.
        let card_bg = ::image::Rgba([0x00, 0x00, 0x00, 0x00]);
        if let Ok(baked) = crate::iterm2_inline::fit_image_auto(
            crate::iterm2_inline::SSH_EMPTY_STATE_PNG,
            ssh_w_px,
            ssh_h_px,
            card_bg,
        ) && let Some(raw) = crate::iterm2_inline::build_inline_image(
            protocol,
            &baked,
            ssh_cells_w,
            ssh_cells_h,
            false,
            crate::iterm2_inline::KITTY_ID_SSH,
        ) {
            self.overlays
                .ssh
                .set_image(crate::iterm2_inline::maybe_tmux_wrap(
                    protocol, is_tmux, raw,
                ));
        }
    }

    /// Emit the OSC-1337 image carrying the run-debug headline icon at the
    /// cell the panel reserved on its most recent render. No-op when the
    /// panel isn't the active sidebar view, when the icon hasn't been
    /// baked (non-iTerm2 host), or when the panel didn't reserve a cell
    /// (too short to fit the icon). Critically, the panel-hidden case
    /// does NOT stamp blanks or any other "wipe" payload here: doing so
    /// after `terminal.clear()` + redraw stomps freshly-painted text in
    /// the next sidebar (this was the bug in the prior space-stamp wipe
    /// attempt). Eviction of iTerm2's cached image cells is handled
    /// exclusively by the `consume_run_debug_image_clear` →
    /// `terminal.clear()` gate in the main loop, the same pipeline the
    /// welcome wordmark and editor preview image use.
    /// Stop showing the Run/Debug empty-state icon: if an OSC-1337 image was
    /// emitted, arm the one-shot `terminal.clear()` latch so the cached image
    /// cells are evicted (text painted over an inline image does not remove
    /// it), then reset the emitted-position tracking so the clear fires exactly
    /// once rather than every frame.
    fn clear_run_debug_icon_if_emitted(&mut self) {
        if self.overlays.run_debug.was_emitted() {
            self.overlays.run_debug.request_clear();
        }
        self.overlays.run_debug.clear_emitted();
    }

    pub fn flush_run_debug_icon_overlay(&mut self) {
        use std::io::Write;
        if self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.command_palette.is_some()
            || self.process_picker.is_some()
            || self.zoxide_jump.is_some()
        {
            return;
        }
        let panel_visible = self.show_tree
            && self.sidebar_view == SidebarView::RunDebug
            && self.overlays.run_debug.has_image();
        if !panel_visible {
            self.clear_run_debug_icon_if_emitted();
            return;
        }
        // No icon cell this frame means the panel is showing the paused
        // call-stack tree (debug_active) or is too short for the icon. The
        // empty-state icon is an OSC-1337 image; drawing the tree over it does
        // NOT evict it, so an emitted icon must be cleared with the one-shot
        // `\x1b[2J` latch or it ghosts on top of the call stack.
        let Some((cx, cy)) = self.run_debug.last_icon_cell else {
            self.clear_run_debug_icon_if_emitted();
            return;
        };
        // The icon moved since the last emit (terminal resize OR an internal
        // pane-drag, which never fires Event::Resize and so escapes
        // `on_resize`). A plain re-emit at the new cell stacks a fresh
        // OSC-1337 image on top of the one iTerm2 still caches at the old
        // cell — the doubled/garbled headline-icon ghost. Plain SGR
        // overwrites don't evict those cells, so reuse the proven
        // `terminal.clear()` latch: arm one clear, drop the stale emit
        // position, and skip painting this frame. Next frame the main loop's
        // OR chain fires `\x1b[2J`, full-redraws, and this flush re-emits a
        // single clean icon at the new cell.
        if let Some(prev) = self.overlays.run_debug.last_emitted()
            && prev != (cx, cy)
        {
            self.overlays.run_debug.request_clear();
            self.overlays.run_debug.clear_emitted();
            return;
        }
        let Some(osc) = self.overlays.run_debug.image() else {
            return;
        };
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        let _ = write!(out, "\x1b[{};{}H", cy + 1, cx + 1);
        let _ = out.write_all(osc.as_bytes());
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
        self.overlays.run_debug.mark_emitted_at((cx, cy));
    }

    /// Re-bake (when needed) and re-emit the OSC-1337 inline image for
    /// the no-repo source-control hero. Pure no-op when the sidebar isn't
    /// on Source Control, the workspace IS a git repo, or the host
    /// terminal can't render inline images. Called from the post-draw
    /// flush each frame so iTerm2 keeps the image painted even after
    /// surrounding cells redraw.
    pub fn flush_no_repo_hero_overlay(&mut self) {
        use std::io::Write;
        if self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.command_palette.is_some()
            || self.process_picker.is_some()
            || self.zoxide_jump.is_some()
        {
            return;
        }
        let panel_visible = self.show_tree
            && self.sidebar_view == SidebarView::SourceControl
            && !self.source_control.status.in_repo;
        if !panel_visible {
            self.overlays.hero.mark_hidden();
            return;
        }
        let hero = self.source_control.last_hero_area;
        if hero.width == 0 || hero.height == 0 {
            return;
        }
        let Some((cw, ch)) = self.cell_pixel else {
            return;
        };
        let desired = WelcomeLayout {
            cell_x: hero.x,
            cell_y: hero.y,
            cell_w: hero.width,
            cell_h: hero.height,
        };
        if !self.overlays.hero.layout_matches(&desired) || !self.overlays.hero.has_image() {
            let canvas_w = (hero.width as u32) * cw;
            let canvas_h = (hero.height as u32) * ch;
            let bg = self.theme_bg_pixel();
            let protocol = self.inline_protocol;
            if let Ok(baked) = crate::iterm2_inline::fit_image(
                crate::iterm2_inline::NO_REPO_HERO_PNG,
                canvas_w,
                canvas_h,
                bg,
            ) && let Some(raw) = crate::iterm2_inline::build_inline_image(
                protocol,
                &baked,
                hero.width,
                hero.height,
                false,
                crate::iterm2_inline::KITTY_ID_HERO,
            ) {
                let osc = crate::iterm2_inline::maybe_tmux_wrap(
                    protocol,
                    crate::iterm2_inline::detect_tmux(),
                    raw,
                );
                self.overlays.hero.set(osc, desired);
            }
        }
        let Some(osc) = self.overlays.hero.image() else {
            return;
        };
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        let _ = write!(out, "\x1b[{};{}H", hero.y + 1, hero.x + 1);
        let _ = out.write_all(osc.as_bytes());
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
        self.overlays.hero.mark_emitted();
    }

    /// Emit the OSC-1337 SSH-empty-state illustration at the cell the
    /// remote panel reserved on its most recent render. When the panel
    /// is no longer the active view (or has hosts, or the shortcuts
    /// modal is open), this arms a one-shot terminal.clear() via
    /// `consume_ssh_empty_state_image_clear()` so iTerm2 evicts the
    /// cached image cells — otherwise the illustration ghosts onto
    /// whatever view (Explorer / Search / …) replaces it.
    pub fn flush_ssh_empty_state_overlay(&mut self) {
        use std::io::Write;
        let should_show = self.shortcuts_modal.is_none()
            && self.file_finder.is_none()
            && self.zoxide_jump.is_none()
            && self.show_tree
            && self.sidebar_view == SidebarView::Remote
            && self.remote.targets.is_empty();
        if !should_show {
            self.overlays.ssh.hide_and_request_clear();
            return;
        }
        let Some((cx, cy)) = self.remote.last_image_cell else {
            return;
        };
        let Some(osc) = self.overlays.ssh.image() else {
            return;
        };
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        let _ = write!(out, "\x1b[{};{}H", cy + 1, cx + 1);
        let _ = out.write_all(osc.as_bytes());
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
        self.overlays.ssh.mark_displayed();
    }

    /// One-shot consumer paired with `flush_ssh_empty_state_overlay`. The
    /// main loop ORs the result into its `terminal.clear()` chain so
    /// iTerm2's cached image cells are evicted on view change.
    pub fn consume_ssh_empty_state_image_clear(&mut self) -> bool {
        self.overlays.ssh.consume_clear_latch()
    }

    /// One-shot consumer for the "leaving Run-Debug after the icon was
    /// emitted" flag. The main loop folds the result into the same OR
    /// chain that fires `terminal.clear()` for the welcome wordmark and
    /// editor preview image.
    pub fn consume_run_debug_image_clear(&mut self) -> bool {
        self.overlays.run_debug.consume_clear()
    }

    /// One-shot consumer for the "a resize may have left stale activity-bar
    /// icon bitmaps in iTerm2's image layer" flag, armed by `on_resize`. The
    /// main loop folds the result into the same OR chain that fires
    /// `terminal.clear()` for the welcome wordmark and editor preview image,
    /// evicting the ghost before the icons re-emit on the next draw.
    pub fn consume_activity_image_clear(&mut self) -> bool {
        self.overlays.activity.consume_clear()
    }

    /// Handle a terminal/pane resize. The alt-screen reflow blanks the SGR
    /// cells but iTerm2's OSC-1337 image layer survives, so a plain re-emit
    /// would stack a fresh icon on top of the stale one (the doubled-icon
    /// ghost). Re-mark the icons dirty so they re-emit, and — in images mode,
    /// the only state the stale layer can exist in — arm a one-shot
    /// `terminal.clear()` so iTerm2 drops the cached bitmaps first.
    pub fn on_resize(&mut self) {
        self.overlays.activity.mark_dirty();
        if self.overlays.activity.has_images() {
            self.overlays.activity.request_clear();
        }
    }

    /// One-shot consumer for the "leaving Source Control after the no-repo
    /// hero PNG was emitted" flag. Same shape as
    /// `consume_run_debug_image_clear` - the main loop folds the result
    /// into its `terminal.clear()` chain so iTerm2's cached image cells
    /// are evicted on view change.
    pub fn consume_no_repo_hero_image_clear(&mut self) -> bool {
        self.overlays.hero.consume_clear_latch()
    }

    /// Returns the post-frame OSC-1337 escapes to write under the activity
    /// bar, paired with the absolute terminal cell where each one starts
    /// (just past the active-pill column). Empty when image rendering is
    /// disabled or the activity bar hasn't been laid out yet.
    pub fn pending_activity_image_overlays(&self) -> Vec<((u16, u16), &str)> {
        let Some(images) = self.overlays.activity.images() else {
            return Vec::new();
        };
        let exp_block = self.sidebar_areas.explorer_icon;
        let sea_block = self.sidebar_areas.search_icon;
        let scm_block = self.sidebar_areas.source_control_icon;
        let rem_block = self.sidebar_areas.remote_icon;
        let rdb_block = self.sidebar_areas.run_debug_icon;
        if exp_block.width == 0
            || sea_block.width == 0
            || scm_block.width == 0
            || rem_block.width == 0
            || rdb_block.width == 0
        {
            return Vec::new();
        }
        // Three-way per icon: the selected view shows the active (pilled)
        // variant; otherwise the pointer-hovered icon brightens (no pill); the
        // rest stay muted. Selected always wins over hovered.
        let hov = self.hovered_activity_icon;
        let exp_state = if self.sidebar_view == SidebarView::Explorer {
            &images.explorer_active
        } else if hov == Some(ActivityIcon::Explorer) {
            &images.explorer_hovered
        } else {
            &images.explorer_inactive
        };
        let sea_state = if self.sidebar_view == SidebarView::Search {
            &images.search_active
        } else if hov == Some(ActivityIcon::Search) {
            &images.search_hovered
        } else {
            &images.search_inactive
        };
        let scm_state = if self.sidebar_view == SidebarView::SourceControl {
            &images.source_control_active
        } else if hov == Some(ActivityIcon::SourceControl) {
            &images.source_control_hovered
        } else {
            &images.source_control_inactive
        };
        let rem_state = if self.sidebar_view == SidebarView::Remote {
            &images.remote_active
        } else if hov == Some(ActivityIcon::Remote) {
            &images.remote_hovered
        } else {
            &images.remote_inactive
        };
        let rdb_state = if self.sidebar_view == SidebarView::RunDebug {
            &images.run_debug_active
        } else if hov == Some(ActivityIcon::RunDebug) {
            &images.run_debug_hovered
        } else {
            &images.run_debug_inactive
        };
        // The gear is bottom-anchored and may be absent on a short bar
        // (empty block). It reads "active" while its menu/picker is open.
        let set_block = self.sidebar_areas.settings_icon;
        let set_state = if self.settings_menu_active() {
            &images.settings_active
        } else if hov == Some(ActivityIcon::Settings) {
            &images.settings_hovered
        } else {
            &images.settings_inactive
        };
        // The shortcuts modal and file finder are centered over the editor
        // column, not the far-left activity bar, so the icons stay visible
        // beside them. Suppress an icon only when its block actually
        // intersects an open panel (possible only when the terminal is
        // narrow enough that the centered panel reaches column 0); otherwise
        // a terminal.clear() armed by the modal wipes iTerm2's cached image
        // cells and they never repaint while the panel is up.
        let occluders = [
            self.shortcuts_modal.as_ref().map(|m| m.last_rect),
            self.file_finder.as_ref().map(|f| f.last_rect),
            self.command_palette.as_ref().map(|p| p.last_rect),
            self.process_picker.as_ref().map(|p| p.last_rect),
        ];
        let visible = |block: Rect| {
            occluders
                .iter()
                .flatten()
                .all(|panel| !block.intersects(*panel))
        };
        let mut blocks = vec![
            (exp_block, exp_state.as_str()),
            (sea_block, sea_state.as_str()),
            (scm_block, scm_state.as_str()),
            (rem_block, rem_state.as_str()),
            (rdb_block, rdb_state.as_str()),
        ];
        if set_block.width > 0 {
            blocks.push((set_block, set_state.as_str()));
        }
        blocks
            .into_iter()
            .filter(|(block, _)| visible(*block))
            .map(|(block, state)| ((block.x, block.y), state))
            .collect()
    }

    /// True while the activity-bar gear's menu or theme picker is open, so the
    /// gear renders in its pressed/active state. Derived from the live context
    /// menu's actions rather than a separate flag that could drift out of sync
    /// with the many places the menu is dismissed.
    fn settings_menu_active(&self) -> bool {
        self.context_menu.as_ref().is_some_and(|menu| {
            menu.items.iter().any(|(_, action)| {
                matches!(
                    action,
                    MenuAction::OpenThemePicker | MenuAction::SetTheme(_)
                )
            })
        })
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
    pub fn flush_activity_image_overlays(&mut self) {
        use std::io::Write;
        // The keepalive re-emit fights iTerm2's image-cache eviction; the sixel
        // cell buffer doesn't evict, so its icons re-emit only when dirty.
        let allow_keepalive =
            self.inline_protocol != crate::iterm2_inline::InlineImageProtocol::Sixel;
        if !self.overlays.activity.should_refresh(allow_keepalive) {
            return;
        }
        let overlays = self.pending_activity_image_overlays();
        if overlays.is_empty() {
            return;
        }
        let positions: Vec<(u16, u16)> = overlays.iter().map(|(cell, _)| *cell).collect();
        if self.overlays.activity.positions_moved(&positions) {
            self.overlays.activity.request_clear();
            self.overlays.activity.forget_positions();
            return;
        }
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        for ((x, y), seq) in &overlays {
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
    }

    /// Paint the Codeberg badge OSC-1337 image at its current welcome-panel
    /// cell, after first wiping any previous emit position. iTerm2 keeps
    /// image cells in a layer that survives plain SGR redraws, so when
    /// `welcome_codeberg_badge_cell` changes (window resize, sidebar
    /// toggle, terminal-pane open/close) the *old* image stays on screen
    /// unless we explicitly stamp blanks over it. Idempotent: when the
    /// badge cell hasn't moved we just re-emit at the same position to
    /// outlast iTerm2's image-cache eviction under heavy SGR traffic.
    fn stamp_blank_pair(&self, px: u16, py: u16) {
        use std::io::Write;
        let mut out = stdout();
        // On Kitty the badge lives on the graphics layer, so stamping blank
        // cells over it does nothing; delete the placement by id instead.
        if self.inline_protocol == crate::iterm2_inline::InlineImageProtocol::Kitty {
            let _ = out.write_all(
                crate::iterm2_inline::delete_kitty_image(crate::iterm2_inline::KITTY_ID_BADGE)
                    .as_bytes(),
            );
            let _ = out.flush();
            return;
        }
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        let _ = write!(out, "\x1b[{};{}H  ", py + 1, px + 1);
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
    }

    pub fn flush_welcome_codeberg_badge_overlay(&mut self) {
        use std::io::Write;
        if self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.command_palette.is_some()
            || self.process_picker.is_some()
            || self.zoxide_jump.is_some()
            || self.connect_dialog.is_some()
        {
            if let Some((px, py)) = self.overlays.badge.take_last_emitted() {
                self.stamp_blank_pair(px, py);
            }
            return;
        }
        if !self.editor.is_blank_initial() {
            // Welcome panel hidden: clear any previous emit and stop
            // re-emitting until we're back on the welcome screen.
            if let Some((px, py)) = self.overlays.badge.take_last_emitted() {
                self.stamp_blank_pair(px, py);
            }
            return;
        }
        let Some(osc) = self.overlays.badge.image() else {
            return;
        };
        let Some((cx, cy)) = self.overlays.badge.target() else {
            // Welcome visible but provider isn't Codeberg: still need to
            // wipe a stale prior emit (e.g. user just changed providers
            // without restart, or layout collapsed the badge row).
            if let Some((px, py)) = self.overlays.badge.take_last_emitted() {
                self.stamp_blank_pair(px, py);
            }
            return;
        };
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        // If the cell moved since the last emit, stamp blanks over the
        // previous position first so the old logo doesn't ghost-overlay
        // the freshly-drawn welcome layout.
        if let Some((px, py)) = self.overlays.badge.last_emitted()
            && (px, py) != (cx, cy)
        {
            let _ = write!(out, "\x1b[{};{}H  ", py + 1, px + 1);
        }
        let _ = write!(out, "\x1b[{};{}H", cy + 1, cx + 1);
        let _ = out.write_all(osc.as_bytes());
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
        self.overlays.badge.mark_emitted_at((cx, cy));
    }

    /// Reset the blink phase so the caret is solidly visible for the next
    /// 530ms. Call after any edit, cursor movement, or focus change so the
    /// user always sees where the cursor just landed before it starts to
    /// blink off.
    fn poke_cursor(&mut self) {
        self.cursor_blink.poke();
    }

    /// Mirrors the predicate used in `App::render` to decide whether to call
    /// `frame.set_cursor_position`. Used by the post-draw overlay writer so
    /// it can re-Show the caret after the OSC-1337 image emit only when the
    /// editor would have shown it this frame.
    fn cursor_should_be_visible(&self) -> bool {
        if self.context_menu.is_some() || self.prompt.is_some() || !self.cursor_visible_phase() {
            return false;
        }
        let editor_caret = if self.editor.diff.is_some() {
            self.editor.diff_caret_screen_pos()
        } else {
            self.editor.cursor_screen_pos()
        };
        if self.focus == Pane::Editor && editor_caret.is_some() {
            return true;
        }
        if self.focus == Pane::Tree
            && self.sidebar_view == SidebarView::SourceControl
            && self.source_control.cursor_screen_pos().is_some()
        {
            return true;
        }
        false
    }

    /// True iff the caret is currently in its visible half of the blink
    /// cycle. VS Code uses a 530ms half-period; we match. We blink in
    /// software (by toggling whether `frame.set_cursor_position` is called
    /// each frame) instead of relying on the host terminal's own blink
    /// support, which iTerm2 / Terminal.app may have disabled in user
    /// preferences.
    fn cursor_visible_phase(&self) -> bool {
        self.cursor_blink.visible_phase()
    }

    /// Re-query git status, but no more than once every ~400ms to avoid
    /// spawning a `git` process on every keystroke.  Called after the file
    /// watcher reports any changes.
    fn refresh_git_status_debounced(&mut self) {
        self.git
            .request_status_debounced(self.sidebar_view == SidebarView::SourceControl);
    }

    fn sync_open_file_poll_mtime(&mut self) {
        self.fs_watch
            .sync_open_file_mtime(self.editor.path.as_deref());
    }

    /// Sweep EVERY open tab for external on-disk changes, not just the
    /// focused one. Clean tabs reload silently; dirty tabs are flagged as
    /// conflicts (the buffer and the disk are both preserved). Returns true
    /// if any tab reloaded or entered conflict, so the caller redraws.
    fn reload_open_file_after_external_change(&mut self) -> bool {
        let report = self.editor.reload_externally_changed_tabs();
        if report.is_empty() {
            return false;
        }
        self.refresh_git_status_debounced();
        match (report.reloaded.len(), report.conflicts.len()) {
            (1, 0) => {
                let p = report.reloaded[0].display();
                self.status = format!("Reloaded {p} (external change)");
            }
            (n, 0) => {
                self.status = format!("Reloaded {n} files changed on disk");
            }
            (_, _) => {
                let p = report.conflicts[0].display();
                self.status = format!(
                    "{p} changed on disk and has unsaved edits - save to overwrite, or close the tab to keep the disk version"
                );
            }
        }
        // Keep the watcher's active-file baseline aligned with the reload so
        // a just-applied change doesn't re-trigger on the next poll.
        self.sync_open_file_poll_mtime();
        true
    }

    fn poll_filesystem_changes(&mut self) -> bool {
        let poll = self
            .fs_watch
            .poll(&mut self.tree, self.editor.path.as_deref());
        // Any directory change can also be a write to a file open in some
        // tab (background or active), so sweep all tabs whenever the poll
        // saw activity, not only when the active file's own stat moved.
        if poll.open_file_changed || poll.dirs_changed {
            self.reload_open_file_after_external_change();
        }
        if poll.dirs_changed {
            self.refresh_git_status_debounced();
            if poll.finder_relevant {
                self.kick_file_finder_index_rebuild();
            }
        }
        poll.open_file_changed || poll.dirs_changed
    }

    /// Drain any pending filesystem events from the watcher and refresh the
    /// tree directories whose contents may have changed. Also reloads the
    /// editor's open file when an external write (vim, git pull, etc.)
    /// changes it on disk.
    /// Drain pending filesystem events. Returns `true` iff anything was
    /// processed (so the main loop knows it owes a redraw).
    /// Install background-initialised resources (fs watcher, git status) if
    /// their threads have finished. Returns true if any were installed this
    /// tick (so the caller redraws). Cheap no-op once both are installed.
    pub fn try_install_pending_init(&mut self) -> bool {
        let mut changed = self.fs_watch.try_install_watcher();
        if self.drain_git_responses() {
            changed = true;
        }
        // Land the background-built Cmd+P file index the moment the
        // walker thread finishes, so the very next Cmd+P opens
        // against a hot index instead of triggering the synchronous
        // fallback walk.
        self.poll_file_finder_index();
        changed
    }

    /// Drain everything the git worker has shipped back since the last
    /// tick and apply each response to the matching cache (`git_status`
    /// for the status-bar badge, `source_control.entries` for the SC
    /// panel rows). Returns true iff anything was applied so the main
    /// loop knows to redraw. Cheap when the rx is empty: a single
    /// non-blocking try_recv. Called from `try_install_pending_init`,
    /// which already runs every tick.
    pub fn drain_git_responses(&mut self) -> bool {
        // Ship any refresh the debounce window coalesced once the gap clears,
        // so the trailing edge of an edit burst lands without another FS event.
        self.git.flush_pending();
        let changed = self.git.drain(&mut self.source_control);
        if changed && self.git.status().in_repo && self.default_branch_label.is_none() {
            self.default_branch_label = crate::git::default_branch(&self.tree.root).ok();
        }
        changed
    }

    /// Drain every SystemSample the background sampler has produced
    /// since the last tick and feed the freshest one into the SYSTEM
    /// panel's history. Returns true iff at least one sample was
    /// applied so the main loop knows to redraw. Cheap when the rx is
    /// empty: a single non-blocking try_recv.
    pub fn drain_sysmon(&mut self) -> bool {
        // Only collect system metrics while the SYSTEM panel is actually showing
        // them: collapsed (the default) or hidden means the sampler thread idles.
        let collecting = !self.system_panel.hidden && !self.system_panel.last_effective_collapsed;
        self.sysmon.set_active(collecting);
        self.sysmon.drain(&mut self.system_panel)
    }

    /// Push the current search query string and toggle state onto the
    /// worker channel and keep the editor's match highlight synced to the
    /// same term so the active file lights up matches as the user types.
    /// Called whenever the search input or one of the toggles changes.
    /// Clears the live result list so streamed `Hits` events accumulate
    /// from zero rather than appending to leftover hits from the prior query.
    fn submit_search_query(&mut self) {
        self.search.hits.clear();
        self.search.selected = 0;
        self.search.scroll = 0;
        // Snapshot unsaved editor buffers so the worker searches their live
        // content (and skips the stale disk copy). This is what makes an
        // unsaved edit — e.g. a Rename Symbol — findable before save, matching
        // VS Code / Zed. Sent before the query so the next scan sees it.
        let dirty: Vec<(PathBuf, String)> = self
            .editor
            .iter_tabs()
            .filter(|t| t.dirty)
            .filter_map(|t| t.path.clone().map(|p| (p, t.lines.join("\n"))))
            .collect();
        let _ = self
            .search_query_tx
            .send(SearchRequest::SetDirtyBuffers(dirty));
        // Push the current files-to-include / files-to-exclude globs so the
        // next scan honours them (VS Code's Search filter inputs).
        let _ = self.search_query_tx.send(SearchRequest::SetFilter(
            self.search.include.clone(),
            self.search.exclude.clone(),
        ));
        let _ = self.search_query_tx.send(SearchRequest::Query(
            self.search.query.clone(),
            self.search.opts,
        ));
        let term = if self.search.query.trim().is_empty() {
            None
        } else {
            Some(self.search.query.clone())
        };
        self.editor.set_search_highlight(term, self.search.opts);
    }

    /// Apply any pending search events from the background worker. Drops
    /// stale events whose query no longer matches the input field (the
    /// user has typed past it). `Hits` events append to the live result
    /// list as files yield matches; `Done` is informational. Returns true
    /// iff hits were updated, so the main loop knows to redraw.
    pub fn drain_search_results(&mut self) -> bool {
        use crate::widgets::search::SearchEvent;
        let mut applied = false;
        while let Ok(event) = self.search_results_rx.try_recv() {
            match event {
                SearchEvent::Hits(q, opts, batch) => {
                    if q == self.search.query && opts == self.search.opts {
                        self.search.hits.extend(batch);
                        applied = true;
                    }
                }
                SearchEvent::Done(_, _) => {}
            }
        }
        applied
    }

    /// Pull a single batch of croft commits from the background HTTP fetch,
    /// if it has finished. Returns true exactly once when commits are
    /// installed (so the welcome panel repaints), false otherwise. Drops
    /// the receiver after consuming so subsequent calls are cheap no-ops.
    pub fn drain_recent_commits(&mut self) -> bool {
        let drain = self.welcome.drain();
        if !drain.installed {
            return false;
        }
        if self.editor.is_blank_initial() {
            self.overlays.welcome.request_clear_if_displayed();
        }
        self.overlays.welcome.mark_dirty();
        if let Some(msg) = drain.status {
            self.status = msg;
        }
        true
    }

    /// Per-tick LSP doc sync. Diffs every text-buffer tab's edit_seq
    /// against `lsp_last_seen` and forwards did_open / did_change /
    /// did_close to the LspManager. Building `lines.join("\n")` only
    /// happens when a tab's edit_seq has actually moved, so the cost
    /// scales with edits, not with frames.
    pub fn sync_lsp(&mut self) {
        if self.lsp.is_none() {
            return;
        }
        let mut current: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        // `(is_open, path, text, seq, viewport)`. The viewport `(start, end)`
        // lines are captured only for opens, so the first paint can fire a fast
        // `semanticTokens/range` over what is on screen ahead of the
        // whole-document request.
        type DocSync = (bool, PathBuf, String, u64, Option<(u32, u32)>);
        let mut to_send: Vec<DocSync> = Vec::new();
        // Semantic-token batches recovered from the on-disk cache for files
        // being opened this tick. Applied after the tab loop (which borrows the
        // editor immutably) so the correct colours paint on the first frame,
        // before the server finishes its ~1.8s cold analysis. See
        // `crate::lsp::semantic_cache`.
        let mut cache_hits: Vec<(PathBuf, std::sync::Arc<Vec<String>>, Vec<u32>)> = Vec::new();
        for tab in self.editor.iter_tabs() {
            if tab.image.is_some() || tab.sheet.is_some() || tab.diff.is_some() {
                continue;
            }
            let Some(path) = tab.path.as_ref() else {
                continue;
            };
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if crate::lsp::Language::from_extension(ext).is_none() {
                continue;
            }
            current.insert(path.clone());
            let seq = tab.edit_seq;
            let prev = self.lsp_last_seen.get(path).copied();
            match prev {
                None => {
                    // A small look-ahead past the visible rows keeps colour
                    // stable through the first nudge of the scroll wheel
                    // before the full-document reply lands.
                    let start = tab.scroll as u32;
                    let end = (tab.scroll + tab.page_size() + 16).min(tab.lines.len()) as u32;
                    let text = tab.lines.join("\n");
                    // Paint the last-known semantic colours instantly from the
                    // disk cache, so an import-heavy file does not flash grey
                    // then snap to the server's class/namespace colours seconds
                    // later. A hit is identical to what the server will send, so
                    // the live reply is a no-op repaint.
                    if let Some((legend, data)) = crate::lsp::semantic_cache::load(path, &text) {
                        cache_hits.push((path.clone(), legend, data));
                    }
                    to_send.push((true, path.clone(), text, seq, Some((start, end))));
                }
                Some(p) if p != seq => {
                    to_send.push((false, path.clone(), tab.lines.join("\n"), seq, None))
                }
                _ => {}
            }
        }
        let lsp = match self.lsp.as_ref() {
            Some(l) => l,
            None => return,
        };
        for (is_open, path, text, seq, viewport) in to_send {
            if is_open {
                lsp.open_doc(path.clone(), text);
                // Paint the visible lines first: ty answers a viewport range
                // query in tens of ms even on a cold workspace, so on-screen
                // code colours immediately while the full request fills in the
                // rest. The editor refuses to let this range batch overwrite
                // the full one when it arrives.
                if let Some((start, end)) = viewport {
                    lsp.request_semantic_tokens_range(path.clone(), start, end);
                }
            } else {
                lsp.change_doc(path.clone(), text);
            }
            // Refresh the whole document's semantic tokens whenever it is
            // opened or changed. Gated by the same `seq` diff as did_change
            // above, so this fires once per edit-batch, not per keystroke.
            lsp.request_semantic_tokens(path.clone());
            self.lsp_last_seen.insert(path, seq);
        }
        let closed: Vec<PathBuf> = self
            .lsp_last_seen
            .keys()
            .filter(|p| !current.contains(*p))
            .cloned()
            .collect();
        for p in closed {
            lsp.close_doc(p.clone());
            self.lsp_last_seen.remove(&p);
            // Drop the closed file's diagnostics so the store doesn't grow
            // unbounded across a long session of opening and closing files.
            self.lsp_diagnostics.remove(&p);
        }
        // Paint cached semantic colours onto the visible editor(s) now that the
        // immutable tab borrow above has ended. `is_full = true` so a later
        // server `range` reply can't blank the off-screen colours, and the
        // identical `full` reply repaints to the same pixels (no snap).
        for (path, legend, data) in cache_hits {
            if self.editor.path.as_deref() == Some(path.as_path()) {
                self.editor
                    .apply_semantic_tokens(path.clone(), data.clone(), legend.clone(), true);
            }
            if let Some(split) = self.editor_split.as_mut()
                && split.path.as_deref() == Some(path.as_path())
            {
                split.apply_semantic_tokens(path, data, legend, true);
            }
        }
    }

    /// Drain LSP completion responses. Drops responses with stale request
    /// ids so a slow earlier reply cannot clobber a fresher popup. Returns
    /// true when the popup state changed and the next frame should redraw.
    pub fn drain_lsp_completion(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut changed = false;
        while let Some(result) = lsp.drain_completion() {
            if Some(result.request_id) != self.completion_request_id {
                continue;
            }
            if result.items.is_empty() {
                if self.completion_popup.is_some() {
                    self.completion_popup = None;
                    changed = true;
                }
                continue;
            }
            let Some((cx, cy)) = self.editor.cursor_screen_pos() else {
                continue;
            };
            let prefix = self.editor.word_before_cursor();
            let server_count = result.items.len();
            let popup = crate::widgets::completion_popup::CompletionPopup::new(
                result.items,
                prefix.clone(),
                (cx, cy),
                result.path,
                result.request_id,
            );
            let filtered = popup.visible_indices().len();
            crate::lsp::log_file::log(&format!(
                "popup filter: prefix={prefix:?} server={server_count} visible={filtered}"
            ));
            if popup.visible_is_empty() {
                self.completion_popup = None;
                self.status =
                    format!("No completions match '{prefix}' ({server_count} from server)");
            } else {
                self.completion_popup = Some(popup);
            }
            changed = true;
        }
        changed
    }

    pub fn trigger_completion(&mut self) {
        if self.editor.diff.is_some() || self.editor.sheet.is_some() || self.editor.image.is_some()
        {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let line = self.editor.cursor_row as u32;
        let character = self.editor.cursor_col as u32;
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_completion(path, line, character);
        self.completion_request_id = Some(id);
    }

    /// Map a pointer cell to the activity-bar icon under it, if any. Uses the
    /// same per-icon rectangles the click handler hit-tests, so hover and
    /// click target exactly the same cells.
    fn activity_icon_at(&self, col: u16, row: u16) -> Option<ActivityIcon> {
        let a = &self.sidebar_areas;
        if rect_contains(a.explorer_icon, col, row) {
            Some(ActivityIcon::Explorer)
        } else if rect_contains(a.search_icon, col, row) {
            Some(ActivityIcon::Search)
        } else if rect_contains(a.source_control_icon, col, row) {
            Some(ActivityIcon::SourceControl)
        } else if rect_contains(a.remote_icon, col, row) {
            Some(ActivityIcon::Remote)
        } else if rect_contains(a.run_debug_icon, col, row) {
            Some(ActivityIcon::RunDebug)
        } else if rect_contains(a.settings_icon, col, row) {
            Some(ActivityIcon::Settings)
        } else {
            None
        }
    }

    /// The hover-tooltip label for whatever chrome control sits under the
    /// pointer, or `None` over inert space. The single dispatch point for
    /// button hints: each surface contributes one branch that reuses that
    /// surface's own hit-test, so adding a new tooltipped control is a
    /// one-line change here plus a label in the widget.
    fn ui_tooltip_at(&self, col: u16, row: u16) -> Option<&'static str> {
        // Activity bar: always visible, so it is checked regardless of which
        // sidebar view is open.
        if let Some(icon) = self.activity_icon_at(col, row) {
            return Some(match icon {
                ActivityIcon::Explorer => "Explorer",
                ActivityIcon::Search => "Search",
                ActivityIcon::SourceControl => "Source Control",
                ActivityIcon::Remote => "Remote",
                ActivityIcon::RunDebug => "Run and Debug",
                ActivityIcon::Settings => "Manage",
            });
        }
        // Search panel actions/toggles, only while the SEARCH view is showing
        // (its stored geometry is stale once another view replaces it).
        if self.sidebar_view == SidebarView::Search
            && let Some(label) = self.search.tooltip_at(col, row)
        {
            return Some(label);
        }
        None
    }

    /// Fire the chrome button-hint tooltip once its dwell crosses
    /// `HOVER_DELAY`. Returns `true` when a tooltip was created so the frame
    /// loop redraws. Suppressed while a context menu is open so the two never
    /// overlap.
    fn poll_ui_tooltip(&mut self) -> bool {
        if !self.ui_hover.due(std::time::Instant::now(), HOVER_DELAY) {
            return false;
        }
        self.ui_hover.mark_fired();
        if self.context_menu.is_some() {
            return false;
        }
        let Some(label) = self.ui_tooltip_label else {
            return false;
        };
        let Some((col, row)) = self.ui_hover.cell() else {
            return false;
        };
        self.ui_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new_compact(
            label.to_string(),
            (col, row),
        ));
        true
    }

    fn on_mouse_moved(&mut self, col: u16, row: u16, in_editor: bool) {
        // Activity-bar hover: brighten the icon under the pointer. Re-emit the
        // swapped pre-baked image only when the hovered icon actually changes,
        // so a pointer drifting within one icon (or anywhere else) costs
        // nothing. Computed before the tab/editor early-returns below so the
        // hover clears the moment the pointer leaves the bar.
        let new_hover = self.activity_icon_at(col, row);
        if self.hovered_activity_icon != new_hover {
            self.hovered_activity_icon = new_hover;
            self.overlays.activity.mark_dirty();
        }
        // Chrome button hints: dwell over a labelled control (activity-bar
        // icons, search-panel actions/toggles) to surface its name. Keyed on
        // the label so the dwell only restarts when the pointer crosses to a
        // different control, and is left running while it drifts within one.
        // Computed before the tab/editor early-returns so it tracks the
        // pointer everywhere, including over the sidebar.
        match self.ui_tooltip_at(col, row) {
            Some(label) if self.ui_tooltip_label == Some(label) => {}
            Some(label) => {
                self.ui_hover.on_move(std::time::Instant::now(), col, row);
                self.ui_tooltip_label = Some(label);
                self.ui_tooltip = None;
            }
            None => {
                self.ui_hover.clear();
                self.ui_tooltip = None;
                self.ui_tooltip_label = None;
            }
        }
        // Editor tab strip: dwell over a tab to surface its full path. The
        // dwell only restarts when the pointer crosses into a *different*
        // tab, so the tooltip keeps showing while the pointer drifts a few
        // cells within the same tab (matches the LSP "same word" rule).
        if let Some(idx) = self.editor.tab_at(col, row) {
            self.hover.clear();
            self.hover_popup = None;
            self.hover_word = None;
            self.hover_diagnostic = None;
            self.hover_request_id = None;
            if self.tab_hover_idx != Some(idx) {
                self.tab_hover.on_move(std::time::Instant::now(), col, row);
                self.tab_hover_idx = Some(idx);
                self.tab_tooltip = None;
            }
            return;
        }
        self.tab_hover.clear();
        self.tab_tooltip = None;
        self.tab_hover_idx = None;
        if !in_editor {
            self.hover.clear();
            self.hover_popup = None;
            self.hover_word = None;
            self.hover_diagnostic = None;
            self.hover_request_id = None;
            return;
        }
        self.hover.on_move(std::time::Instant::now(), col, row);
        if self.hover_popup.is_some() {
            let current = self
                .editor
                .buffer_pos_at(col, row)
                .and_then(|(line, c)| self.editor.hover_region_at(line, c));
            if current != self.hover_word {
                self.hover_popup = None;
                self.hover_word = None;
                self.hover_diagnostic = None;
                self.hover_request_id = None;
            }
        }
    }

    fn poll_hover(&mut self) {
        if !self.hover.due(std::time::Instant::now(), HOVER_DELAY) {
            return;
        }
        self.hover.mark_fired();
        if self.completion_popup.is_some() {
            return;
        }
        if self.editor.diff.is_some() || self.editor.sheet.is_some() || self.editor.image.is_some()
        {
            return;
        }
        let Some((col, row)) = self.hover.cell() else {
            return;
        };
        self.hover_at_cell(col, row);
    }

    /// Resolve a hover at screen cell `(col, row)`: capture any diagnostic
    /// under the point synchronously and, for a real identifier, fire the
    /// async `textDocument/hover`. Split out of `poll_hover` so the dwell
    /// timer doesn't have to be driven to test the resolution logic.
    fn hover_at_cell(&mut self, col: u16, row: u16) {
        let Some((line, c)) = self.editor.buffer_pos_at(col, row) else {
            return;
        };
        // The region (word, or else a diagnostic span over punctuation) the
        // popup belongs to. Nothing of interest under the point → no hover.
        let Some(region) = self.editor.hover_region_at(line, c) else {
            return;
        };
        self.hover_anchor = Some((col, row));
        self.hover_word = Some(region);
        // Debug hover-to-evaluate: while paused, hovering a variable shows its
        // current value (from the loaded locals/globals) instead of the LSP
        // type hover. Takes priority because the live value is what the user
        // wants mid-debug.
        if let Some(text) = self.debug_hover_value(line, c) {
            self.hover_request_id = None;
            self.hover_diagnostic = None;
            self.hover_popup = Some(crate::widgets::hover_popup::HoverPopup::new(
                text,
                (col, row),
            ));
            return;
        }
        // Server diagnostics are already in the editor, so capture the
        // message synchronously; the squiggle's message must show even if the
        // server has no type info (or no LSP is attached at all).
        self.hover_diagnostic = format_diagnostics(&self.editor.diagnostics_at(line, c));
        // Only a real word is worth a `textDocument/hover` round-trip; over
        // punctuation there is no symbol to describe, so show the diagnostic
        // alone right away.
        let word = self.editor.word_at(line, c).is_some();
        match (word, self.editor.path.clone(), self.lsp.as_mut()) {
            (true, Some(path), Some(lsp)) => {
                let id = lsp.request_hover(path, line as u32, c as u32);
                self.hover_request_id = Some(id);
            }
            _ => {
                self.hover_request_id = None;
                self.hover_popup = compose_hover(self.hover_diagnostic.as_deref(), None)
                    .map(|text| crate::widgets::hover_popup::HoverPopup::new(text, (col, row)));
            }
        }
    }

    /// Fire the tab-strip tooltip once its dwell crosses `HOVER_DELAY`.
    /// Returns `true` when a tooltip was created so the frame loop knows
    /// to redraw. Suppressed while a tab context menu is open so the
    /// right-click menu never collides with the tooltip.
    fn poll_tab_hover(&mut self) -> bool {
        if !self.tab_hover.due(std::time::Instant::now(), HOVER_DELAY) {
            return false;
        }
        self.tab_hover.mark_fired();
        if self.context_menu.is_some() {
            return false;
        }
        let Some(idx) = self.tab_hover_idx else {
            return false;
        };
        let Some((col, row)) = self.tab_hover.cell() else {
            return false;
        };
        let Some(path) = self.editor.tab_full_path(idx) else {
            return false;
        };
        self.tab_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new(
            path,
            (col, row),
        ));
        true
    }

    pub fn drain_lsp_hover(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut changed = false;
        while let Some(result) = lsp.drain_hover() {
            if Some(result.request_id) != self.hover_request_id {
                continue;
            }
            let current = self
                .hover
                .cell()
                .and_then(|(c, r)| self.editor.buffer_pos_at(c, r))
                .and_then(|(line, ch)| self.editor.hover_region_at(line, ch));
            if current != self.hover_word {
                continue;
            }
            // Diagnostic on top, type info below (VS Code PR #166560). Either
            // side may be empty; an all-empty compose hides the popup.
            let anchor = self.hover_anchor.unwrap_or((0, 0));
            self.hover_popup =
                compose_hover(self.hover_diagnostic.as_deref(), result.text.as_deref())
                    .map(|text| crate::widgets::hover_popup::HoverPopup::new(text, anchor));
            changed = true;
        }
        changed
    }

    /// Drain semantic-token batches and hand each to whichever editor group
    /// currently shows that file. Returns true when an overlay changed and
    /// the next frame should redraw.
    pub fn drain_lsp_semantic_tokens(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        // Collect first so the immutable borrow of `lsp` ends before the
        // editors are mutated below.
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_semantic_tokens() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            if self.editor.path.as_deref() == Some(u.path.as_path()) {
                self.editor.apply_semantic_tokens(
                    u.path.clone(),
                    u.data.clone(),
                    u.legend.clone(),
                    u.is_full,
                );
                changed = true;
            }
            if let Some(split) = self.editor_split.as_mut()
                && split.path.as_deref() == Some(u.path.as_path())
            {
                split.apply_semantic_tokens(
                    u.path.clone(),
                    u.data.clone(),
                    u.legend.clone(),
                    u.is_full,
                );
                changed = true;
            }
            // Persist whole-document batches so the next open of this exact
            // content paints these colours instantly instead of waiting out the
            // server's cold analysis. Only when a visible editor holds the file
            // unchanged, so the cache key matches the bytes the next open reads.
            if u.is_full {
                let clean_text = (self.editor.path.as_deref() == Some(u.path.as_path()))
                    .then(|| self.editor.clean_cache_text())
                    .flatten()
                    .or_else(|| {
                        self.editor_split.as_ref().and_then(|s| {
                            (s.path.as_deref() == Some(u.path.as_path()))
                                .then(|| s.clean_cache_text())
                                .flatten()
                        })
                    });
                if let Some(text) = clean_text {
                    crate::lsp::semantic_cache::store(&u.path, &text, &u.legend, &u.data);
                }
            }
        }
        changed
    }

    /// Flatten the per-server diagnostics stored for `path` into one list for
    /// the editor. Servers are layered, not merged across (each owns its own
    /// findings), so this is a simple concatenation of every server's set.
    fn merged_diagnostics(&self, path: &Path) -> Vec<crate::lsp::manager::Diagnostic> {
        self.lsp_diagnostics
            .get(path)
            .map(|by_server| by_server.values().flatten().cloned().collect())
            .unwrap_or_default()
    }

    /// Drain server-pushed diagnostics into the per-file store, then refresh
    /// the underline overlay on whichever editor group shows an affected (or
    /// just-switched-to) file. Returns true when an overlay changed and the
    /// next frame should redraw.
    pub fn drain_lsp_diagnostics(&mut self) -> bool {
        // Collect first so the immutable borrow of `lsp` ends before the store
        // and the editors are mutated below.
        let mut updates = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(u) = lsp.drain_diagnostics() {
                updates.push(u);
            }
        }
        let mut touched: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        for u in updates {
            touched.insert(u.path.clone());
            let by_server = self.lsp_diagnostics.entry(u.path).or_default();
            // An empty batch is the server saying "all clear" for its findings.
            if u.diagnostics.is_empty() {
                by_server.remove(&u.server);
            } else {
                by_server.insert(u.server, u.diagnostics);
            }
        }
        let mut changed = false;
        // The active editor re-decodes when its file was touched this tick OR
        // when it now shows a file whose stored diagnostics it hasn't applied
        // yet (a tab switch: diagnostics are pushed, never re-requested).
        if let Some(path) = self.editor.path.clone() {
            let stale = self.editor.diagnostics_path() != Some(path.as_path());
            if stale || touched.contains(&path) {
                let merged = self.merged_diagnostics(&path);
                self.editor.apply_diagnostics(path, merged);
                changed = true;
            }
        }
        // Same for the split group, computed without overlapping borrows.
        let split_target = self.editor_split.as_ref().and_then(|s| {
            let p = s.path.clone()?;
            let stale = s.diagnostics_path() != Some(p.as_path());
            (stale || touched.contains(&p)).then_some(p)
        });
        if let Some(path) = split_target {
            let merged = self.merged_diagnostics(&path);
            if let Some(split) = self.editor_split.as_mut() {
                split.apply_diagnostics(path, merged);
                changed = true;
            }
        }
        changed
    }

    /// Drain server work-done progress updates into the per-server map the
    /// status bar reads. Returns true when anything changed so the main loop
    /// repaints the bar. A `Some(message)` upserts the server's status; a
    /// `None` (the server's `End`) clears it.
    pub fn drain_lsp_progress(&mut self) -> bool {
        let mut updates = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(u) = lsp.drain_progress() {
                updates.push(u);
            }
        }
        let mut changed = false;
        for u in updates {
            match u.message {
                Some(message) => {
                    self.lsp_progress.insert(u.server, message);
                    changed = true;
                }
                None => {
                    if self.lsp_progress.remove(&u.server).is_some() {
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// The active LSP progress line for the status bar, if any server is busy
    /// (e.g. `rust-analyzer: Indexing 112/340 33%`). Joins multiple busy
    /// servers with `  ·  ` so a Rust + Python workspace shows both.
    fn lsp_progress_status(&self) -> Option<String> {
        if self.lsp_progress.is_empty() {
            return None;
        }
        let mut entries: Vec<String> = self
            .lsp_progress
            .iter()
            .map(|(server, msg)| format!("{server}: {msg}"))
            .collect();
        entries.sort();
        Some(entries.join("  ·  "))
    }

    /// Re-pull semantic tokens when a server asked us to via
    /// `workspace/semanticTokens/refresh`. rust-analyzer answers the first
    /// request from partial syntax analysis, then sends refresh once its
    /// cargo/crate-graph analysis resolves the richer type-aware tokens; this
    /// is how that upgraded set reaches the editor without an edit. Only the
    /// visible editor(s) are re-requested (matching VS Code), and only for
    /// files the manager has open. The fresh full batch replaces the partial
    /// one via the editor's `is_full` guard.
    pub fn refresh_semantic_tokens_if_requested(&self) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        if !lsp.take_semantic_refresh() {
            return;
        }
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Some(p) = self.editor.path.clone() {
            paths.push(p);
        }
        if let Some(split) = self.editor_split.as_ref()
            && let Some(p) = split.path.clone()
        {
            paths.push(p);
        }
        for p in paths {
            if self.lsp_last_seen.contains_key(&p) {
                lsp.request_semantic_tokens(p);
            }
        }
    }

    fn go_to_definition(&mut self, path: PathBuf, line: u32, col: u32) {
        if let Some(from) = self.editor.path.clone() {
            self.nav.record(NavLoc {
                path: from,
                row: self.editor.cursor_row,
                col: self.editor.cursor_col,
            });
        }
        match self.open_at(&path, line as usize, col as usize) {
            Ok(()) => self.status = format!("Jumped to {} line {}", path.display(), line + 1),
            Err(e) => self.status = format!("Go to definition failed: {e}"),
        }
    }

    /// Keep the OUTLINE in sync with the active editor: request fresh symbols
    /// when the file or its edit-seq changed (once per change), settle a
    /// loaded-empty outline for files no language server tracks, and advance
    /// follow-cursor. Returns whether the highlight moved (for redraw).
    pub fn sync_outline(&mut self) -> bool {
        let Some(path) = self.editor.path.clone() else {
            // Back on the welcome screen: drop any stale outline.
            if self.outline.path.is_some() {
                self.outline.clear();
            }
            self.outline_synced = None;
            return false;
        };
        // `lsp_last_seen` carries the edit seq the manager last saw for an open
        // doc; absent means no language server tracks this file.
        let seq = self.lsp_last_seen.get(&path).copied();
        let key = (path.clone(), seq);
        if self.outline_synced.as_ref() != Some(&key) {
            self.outline_synced = Some(key);
            // Tree-sitter paints the outline instantly from the buffer's own
            // syntax tree, before any language server replies. The LSP request
            // below refines it with richer kinds/detail when (and if) it lands.
            let syntax = self.compute_syntax_outline(&path);
            let lsp_pending = seq.is_some();
            self.outline
                .set_syntax_symbols(path.clone(), syntax, lsp_pending);
            if lsp_pending && let Some(lsp) = self.lsp.as_ref() {
                lsp.request_document_symbols(path);
            }
        }
        // Follow-cursor every tick; cheap (O(symbols)) and only repaints when
        // the enclosing symbol actually changes.
        self.outline.follow_caret(self.editor.cursor_row as u32)
    }

    /// Keep the Explorer's non-OUTLINE stacked sub-views current: re-project the
    /// OPEN EDITORS list from the tabs, kick off the TIMELINE / DEPENDENCIES
    /// background fetches when their input (active file / workspace root) changed,
    /// and drain any worker replies. Returns whether anything changed, so the
    /// main loop only repaints when it must. All the heavy work (`git log`,
    /// `cargo metadata`) runs on spawned threads — never on this tick.
    fn sync_explorer_panels(&mut self) -> bool {
        let mut changed = false;

        // --- OPEN EDITORS: a pure projection of the focused group's tabs. ---
        let active = self.editor.active_index();
        let items: Vec<OpenEditorItem> = self
            .editor
            .iter_tabs()
            .enumerate()
            .filter_map(|(i, ed)| {
                ed.path.as_ref().map(|p| OpenEditorItem {
                    path: p.clone(),
                    name: p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.display().to_string()),
                    dirty: ed.dirty,
                    active: i == active,
                })
            })
            .collect();
        changed |= self.open_editors.set_items(items);

        // --- TIMELINE: git history of the active file, fetched off-thread. ---
        if self.explorer_views.is_visible(ExplorerView::Timeline) {
            match self.editor.path.clone() {
                Some(path) => {
                    if self.timeline_fetched.as_ref() != Some(&path) {
                        self.timeline_fetched = Some(path.clone());
                        self.timeline.begin_loading(path.clone());
                        changed = true;
                        if let Ok(rel) = path.strip_prefix(&self.tree.root) {
                            let root = self.tree.root.clone();
                            let rel = rel.to_string_lossy().into_owned();
                            let tx = self.timeline_tx.clone();
                            std::thread::spawn(move || {
                                let entries = crate::git::file_history(&root, &rel, 50);
                                let _ = tx.send((path, entries));
                            });
                        }
                    }
                }
                None => {
                    if self.timeline_fetched.is_some() {
                        self.timeline_fetched = None;
                        self.timeline.clear();
                        changed = true;
                    }
                }
            }
        }
        // Drain any TIMELINE replies; ignore one for a since-closed file.
        while let Ok((path, entries)) = self.timeline_rx.try_recv() {
            if self.editor.path.as_deref() == Some(path.as_path()) {
                self.timeline.set_history(path, entries);
                changed = true;
            }
        }

        // --- DEPENDENCIES: detect the workspace's package ecosystems from its
        // manifest files (cheap, once per root), set the language-aware header,
        // then resolve the packages off-thread. The section is gated on
        // detection: a root with no recognised manifest detects nothing, so it
        // is hidden and dropped from the ⋯ menu (the rule every reference editor
        // follows). The fetch only fires when the section is actually shown. ---
        let root = self.tree.root.clone();
        if self.deps_fetched.as_ref() != Some(&root) {
            self.deps_fetched = Some(root.clone());
            self.dep_ecosystems = crate::widgets::dependencies::detect_ecosystems(&root);
            self.dependencies
                .set_header(crate::widgets::dependencies::header_label(
                    &self.dep_ecosystems,
                ));
            changed = true;
            if !self.dep_ecosystems.is_empty()
                && self.explorer_views.is_visible(ExplorerView::Dependencies)
            {
                let tx = self.deps_tx.clone();
                std::thread::spawn(move || {
                    let deps = crate::widgets::dependencies::fetch_dependencies(&root);
                    let _ = tx.send(deps);
                });
            }
        }
        while let Ok(deps) = self.deps_rx.try_recv() {
            self.dependencies.set_deps(deps);
            changed = true;
        }

        changed
    }

    /// Activate the editor tab for `path` (clicked in OPEN EDITORS). The path is
    /// already open by definition, so this selects its tab; should it have been
    /// closed between the projection and the click, it re-opens it.
    fn activate_open_editor(&mut self, path: PathBuf) {
        if let Some(idx) = self.editor.find_tab_with_path(&path) {
            self.editor.select(idx);
        } else if self.editor.open(&path).is_err() {
            self.status = format!("Could not open {}", path.display());
            return;
        }
        self.focus_pane(Pane::Editor);
        self.sync_open_file_poll_mtime();
    }

    /// Open the diff a TIMELINE commit introduced for `path` (`git show <hash>
    /// -- <path>`) in a read-only side-by-side editor tab.
    fn open_timeline_diff(&mut self, path: PathBuf, hash: String) {
        let rel = match path.strip_prefix(&self.tree.root) {
            Ok(r) => r.to_string_lossy().into_owned(),
            Err(_) => path.display().to_string(),
        };
        match crate::git::show_commit_file_diff(&self.tree.root, &hash, &rel) {
            Ok(raw) => {
                let label = std::path::PathBuf::from(format!("{rel} @ {hash}"));
                if let Err(e) = self.editor.open_git_diff_side_by_side(&label, &raw) {
                    self.status = format!("Could not open diff: {e}");
                    return;
                }
                self.focus_pane(Pane::Editor);
                self.status = format!("Showing {rel} at {hash}");
            }
            Err(e) => {
                self.status = format!("git show failed: {e}");
            }
        }
    }

    /// Apply a `documentSymbol` reply to the OUTLINE panel, but only when its
    /// `path` is still the active editor file — a reply for a file the user
    /// navigated away from is dropped. Refreshes follow-cursor on success.
    /// Returns whether the outline changed (for redraw).
    fn apply_outline_symbols(
        &mut self,
        path: std::path::PathBuf,
        symbols: Vec<crate::lsp::manager::OutlineSymbol>,
    ) -> bool {
        if self.editor.path.as_ref() != Some(&path) {
            return false;
        }
        // An empty reply (server lacks `documentSymbol`, or answered before it
        // finished indexing) must not blank an outline tree-sitter already
        // filled. Keep the syntactic symbols rather than flashing "No symbols".
        if symbols.is_empty() && !self.outline.is_empty() {
            return false;
        }
        self.outline.set_symbols(path, symbols);
        self.outline.follow_caret(self.editor.cursor_row as u32);
        true
    }

    /// Compute the active editor's outline directly from its syntax tree, so
    /// the OUTLINE panel can paint before a (possibly cold) language server
    /// answers `documentSymbol`. Empty for files whose language has no outline
    /// query; those fall back to the LSP reply.
    fn compute_syntax_outline(
        &self,
        path: &std::path::Path,
    ) -> Vec<crate::lsp::manager::OutlineSymbol> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Some(kind) = crate::highlight::lang_for_extension(ext) else {
            return Vec::new();
        };
        let text = self.editor.lines.join("\n");
        crate::outline_syntax::symbols_for(kind, text.as_bytes())
    }

    /// Drain pending `documentSymbol` replies and apply the freshest to the
    /// OUTLINE (guarded to the active file by `apply_outline_symbols`). Mirrors
    /// the other `drain_lsp_*` pollers; the latest batch per file wins.
    pub fn drain_lsp_document_symbols(&mut self) -> bool {
        let mut latest: Option<(PathBuf, Vec<crate::lsp::manager::OutlineSymbol>)> = None;
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(r) = lsp.drain_document_symbols() {
                latest = Some((r.path, r.symbols));
            }
        }
        match latest {
            Some((path, symbols)) => self.apply_outline_symbols(path, symbols),
            None => false,
        }
    }

    pub fn drain_lsp_definition(&mut self) -> bool {
        let mut target = None;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_definition() {
                if Some(result.request_id) != self.definition_request_id {
                    continue;
                }
                target = result.target;
            }
        }
        match target {
            Some((path, line, col)) => {
                self.go_to_definition(path, line, col);
                true
            }
            None => false,
        }
    }

    pub fn drain_lsp_declaration(&mut self) -> bool {
        let mut target = None;
        let mut unsupported = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_declaration() {
                if Some(result.request_id) != self.declaration_request_id {
                    continue;
                }
                target = result.target;
                unsupported = result.unsupported;
            }
        }
        match target {
            // The jump itself is identical to definition: open the target file,
            // move the caret, record nav history.
            Some((path, line, col)) => {
                self.go_to_definition(path, line, col);
                true
            }
            // Be explicit rather than silently no-op: say when the language
            // server has no declaration provider (e.g. TypeScript's vtsls) and
            // when it simply found nothing at this position.
            None if unsupported => {
                self.status =
                    String::from("Go to Declaration: not supported by this file's language server");
                true
            }
            None => false,
        }
    }

    pub fn drain_lsp_type_definition(&mut self) -> bool {
        let mut target = None;
        let mut unsupported = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_type_definition() {
                if Some(result.request_id) != self.type_definition_request_id {
                    continue;
                }
                target = result.target;
                unsupported = result.unsupported;
            }
        }
        match target {
            // The jump itself is identical to definition: open the target file,
            // move the caret, record nav history.
            Some((path, line, col)) => {
                self.go_to_definition(path, line, col);
                true
            }
            None if unsupported => {
                self.status = String::from(
                    "Go to Type Definition: not supported by this file's language server",
                );
                true
            }
            None => false,
        }
    }

    pub fn drain_lsp_implementation(&mut self) -> bool {
        let mut targets: Vec<(PathBuf, u32, u32)> = Vec::new();
        let mut unsupported = false;
        let mut responded = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_implementation() {
                if Some(result.request_id) != self.implementation_request_id {
                    continue;
                }
                targets = result.targets;
                unsupported = result.unsupported;
                responded = true;
            }
        }
        if !responded {
            return false;
        }
        if unsupported {
            self.status =
                String::from("Go to Implementations: not supported by this file's language server");
            return true;
        }
        match targets.len() {
            0 => self.status = String::from("No implementations found"),
            // One implementor: behave exactly like Go to Definition.
            1 => {
                let (path, line, col) = targets.into_iter().next().expect("len == 1");
                self.go_to_definition(path, line, col);
            }
            // Many implementors: offer a picker rather than silently choosing
            // the first. This 1:many case is the whole point of the feature.
            _ => self.open_location_picker(targets, "implementations"),
        }
        true
    }

    pub fn drain_lsp_references(&mut self) -> bool {
        let mut targets: Vec<(PathBuf, u32, u32)> = Vec::new();
        let mut unsupported = false;
        let mut responded = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_references() {
                if Some(result.request_id) != self.references_request_id {
                    continue;
                }
                targets = result.targets;
                unsupported = result.unsupported;
                responded = true;
            }
        }
        if !responded {
            return false;
        }
        if unsupported {
            self.status =
                String::from("Go to References: not supported by this file's language server");
            return true;
        }
        match targets.len() {
            0 => self.status = String::from("No references found"),
            // A symbol used exactly once: jump straight there like Go to
            // Definition rather than pop a one-row picker.
            1 => {
                let (path, line, col) = targets.into_iter().next().expect("len == 1");
                self.go_to_definition(path, line, col);
            }
            // The common case: a symbol used in many places. Offer every use as
            // a picker, exactly like Go to Implementations.
            _ => self.open_location_picker(targets, "references"),
        }
        true
    }

    /// Build a multi-location picker as a `ContextMenu` anchored at the editor
    /// cursor: one row per target, labelled `relative/path:line`, each carrying a
    /// `GoToLocation` that jumps on select. Reuses the context menu's existing
    /// keyboard (arrows + Enter) and mouse handling. Shared by Go to
    /// Implementations and Go to References; `noun` is the status-line plural
    /// ("implementations" / "references").
    fn open_location_picker(&mut self, targets: Vec<(PathBuf, u32, u32)>, noun: &str) {
        let root = self.tree.root.clone();
        let items: Vec<(String, MenuAction)> = targets
            .into_iter()
            .map(|(path, line, col)| {
                let label = format!(
                    "{}:{}",
                    path.strip_prefix(&root).unwrap_or(&path).display(),
                    line + 1
                );
                (label, MenuAction::GoToLocation { path, line, col })
            })
            .collect();
        self.status = format!("{} {noun}", items.len());
        let origin = self.editor.cursor_screen_pos().unwrap_or((
            self.editor.last_full_area.x + 1,
            self.editor.last_full_area.y + 1,
        ));
        self.focus_pane(Pane::Editor);
        self.context_menu = Some(ContextMenu {
            origin,
            items,
            selected: 0,
            target_dir: root,
        });
    }

    /// Anchor a `ContextMenu` at the activity-bar gear. `menu_rect` clamps it
    /// onto the screen, so a bottom-anchored gear pops the menu upward — the
    /// same direction VS Code's "Manage" menu opens.
    fn settings_menu_origin(&self) -> (u16, u16) {
        let gear = self.sidebar_areas.settings_icon;
        (gear.x + gear.width, gear.y)
    }

    /// Open the activity-bar gear's settings menu (VS Code's "Manage" menu).
    /// Currently one entry — Color Theme — which opens the theme picker; more
    /// settings entries slot in here later.
    fn open_settings_menu(&mut self) {
        let origin = self.settings_menu_origin();
        self.context_menu = Some(ContextMenu {
            origin,
            items: vec![(String::from("Color Theme"), MenuAction::OpenThemePicker)],
            selected: 0,
            target_dir: self.workspace_root.clone(),
        });
        // The gear renders "active" while its menu is open; re-emit it so the
        // pressed-state PNG replaces the idle one.
        self.overlays.activity.mark_dirty();
    }

    /// Replace the gear menu with the Color Theme picker: one row per theme,
    /// a check on the active one. Selecting a row switches + persists it.
    fn open_theme_picker(&mut self) {
        let origin = self.settings_menu_origin();
        let active = self.theme;
        let items: Vec<(String, MenuAction)> = crate::theme::Theme::ALL
            .iter()
            .map(|&t| {
                let mark = if t == active { "✔ " } else { "  " };
                (format!("{mark}{}", t.label()), MenuAction::SetTheme(t))
            })
            .collect();
        let selected = crate::theme::Theme::ALL
            .iter()
            .position(|&t| t == active)
            .unwrap_or(0);
        self.context_menu = Some(ContextMenu {
            origin,
            items,
            selected,
            target_dir: self.workspace_root.clone(),
        });
        self.overlays.activity.mark_dirty();
    }

    /// Open the Explorer's "Views and More Actions" (⋯) menu: one row per
    /// stacked sub-view, each prefixed with a check when currently shown.
    /// Selecting a row toggles that view (see [`Self::toggle_explorer_view`]).
    /// The ⋯-menu / status label for a sub-view. DEPENDENCIES is language-aware
    /// ("Rust Dependencies", "Python Dependencies", or plain "Dependencies" for
    /// a polyglot root), computed from the detected ecosystems; every other view
    /// uses its static name.
    fn explorer_view_label(&self, view: ExplorerView) -> String {
        match view {
            ExplorerView::Dependencies => {
                crate::widgets::dependencies::menu_label(&self.dep_ecosystems)
            }
            other => other.label().to_string(),
        }
    }

    /// Whether a sub-view may appear in the ⋯ menu and stack in the panel.
    /// DEPENDENCIES is additionally gated on a manifest being detected at the
    /// root, so a plain folder shows no dependency view at all; every other view
    /// is always available.
    fn explorer_view_available(&self, view: ExplorerView) -> bool {
        match view {
            ExplorerView::Dependencies => !self.dep_ecosystems.is_empty(),
            _ => true,
        }
    }

    /// Anchored just below the ⋯ button on the EXPLORER title line; the
    /// menu-rect clamp pulls it left so it never spills off the right edge.
    fn open_explorer_views_menu(&mut self) {
        let btn = self.tree.header_views_btn;
        let origin = if btn.width > 0 {
            (btn.x, btn.y + 1)
        } else {
            (self.tree.last_area.x + 1, self.tree.last_area.y + 1)
        };
        let items: Vec<(String, MenuAction)> = ExplorerView::ALL
            .iter()
            .filter(|&&view| self.explorer_view_available(view))
            .map(|&view| {
                let mark = if self.explorer_views.is_visible(view) {
                    "\u{2713} "
                } else {
                    "  "
                };
                (
                    format!("{mark}{}", self.explorer_view_label(view)),
                    MenuAction::ToggleExplorerView(view),
                )
            })
            .collect();
        self.context_menu = Some(ContextMenu {
            origin,
            items,
            selected: 0,
            target_dir: self.workspace_root.clone(),
        });
    }

    /// Flip one Explorer sub-view's visibility, persist the new set, and (for a
    /// data-backed view turned on) clear its fetch latch so the next frame
    /// re-populates it. The layout reads `explorer_views` directly, so no other
    /// wiring is needed for the section to appear or vanish.
    fn toggle_explorer_view(&mut self, view: ExplorerView) {
        let now_visible = self.explorer_views.toggle(view);
        let _ = crate::prefs::save_explorer_views(self.explorer_views.as_prefs());
        if now_visible {
            match view {
                ExplorerView::Timeline => self.timeline_fetched = None,
                ExplorerView::Dependencies => self.deps_fetched = None,
                _ => {}
            }
        }
        self.status = format!(
            "{} {}",
            if now_visible { "Showing" } else { "Hiding" },
            self.explorer_view_label(view)
        );
    }

    /// Lay out and paint the Explorer's visible stacked sub-views top-to-bottom
    /// inside `area` (the region above the SYSTEM footer). Folders (the file
    /// tree) is the flexible region that absorbs leftover rows; every other
    /// visible section takes its own collapsible height, clipped so the tree
    /// always keeps a few rows. Sections hidden via the ⋯ menu are skipped, and
    /// their hit-test rects are zeroed so a stale click can't reach them.
    fn render_explorer_sections(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        self.open_editors.last_area = Rect::default();
        self.outline.last_area = Rect::default();
        self.timeline.last_area = Rect::default();
        self.dependencies.last_area = Rect::default();

        let folders_visible = self.explorer_views.is_visible(ExplorerView::Folders);
        if !folders_visible {
            // The tree isn't painted, so zero its geometry too — otherwise a
            // click on a since-hidden row would still resolve against it.
            self.tree.last_inner = Rect::default();
            self.tree.last_area = Rect::default();
        }
        if area.height == 0 || area.width == 0 {
            if folders_visible {
                frame.render_widget(&mut self.tree, area);
            }
            return;
        }

        // Requested rows per visible section, in stacking order. Folders is a
        // placeholder (0) here; it claims the leftover after the others.
        let mut heights: Vec<(ExplorerView, u16)> = Vec::new();
        for &v in &ExplorerView::ALL {
            if !self.explorer_views.is_visible(v) || !self.explorer_view_available(v) {
                continue;
            }
            let h = match v {
                ExplorerView::Folders => 0,
                ExplorerView::OpenEditors => self.open_editors.desired_height(area.height),
                ExplorerView::Outline => self.outline.desired_height(area.height),
                ExplorerView::Timeline => self.timeline.desired_height(area.height),
                ExplorerView::Dependencies => self.dependencies.desired_height(area.height),
            };
            heights.push((v, h));
        }
        if heights.is_empty() {
            return;
        }

        // Reserve a floor for the tree, then hand the fixed (non-Folders)
        // sections rows in order, clipping each to what's left.
        let total = area.height;
        let tree_floor = if folders_visible { 3u16.min(total) } else { 0 };
        let mut budget = total.saturating_sub(tree_floor);
        let mut fixed_sum = 0u16;
        for (v, h) in heights.iter_mut() {
            if *v == ExplorerView::Folders {
                continue;
            }
            let assigned = (*h).min(budget);
            *h = assigned;
            budget -= assigned;
            fixed_sum += assigned;
        }
        if folders_visible {
            if let Some(slot) = heights
                .iter_mut()
                .find(|(v, _)| *v == ExplorerView::Folders)
            {
                slot.1 = total.saturating_sub(fixed_sum);
            }
        } else {
            // No tree to absorb slack: grow the first section to fill the gap.
            let used: u16 = heights.iter().map(|(_, h)| *h).sum();
            if used < total
                && let Some(first) = heights.first_mut()
            {
                first.1 += total - used;
            }
        }

        let mut y = area.y;
        for (v, h) in heights {
            if h == 0 {
                continue;
            }
            let rect = Rect {
                x: area.x,
                y,
                width: area.width,
                height: h,
            };
            y = y.saturating_add(h);
            match v {
                ExplorerView::Folders => frame.render_widget(&mut self.tree, rect),
                ExplorerView::OpenEditors => frame.render_widget(&mut self.open_editors, rect),
                ExplorerView::Outline => frame.render_widget(&mut self.outline, rect),
                ExplorerView::Timeline => frame.render_widget(&mut self.timeline, rect),
                ExplorerView::Dependencies => frame.render_widget(&mut self.dependencies, rect),
            }
        }
    }

    /// Switch the IDE color theme: persist it, repaint the iTerm2 session
    /// background (so every `Color::Reset` surface follows), re-bake the
    /// OSC-1337 icon PNGs against the new background, and drop the lazily-baked
    /// image overlays + arm a full clear so nothing ghosts the old background.
    fn apply_theme(&mut self, theme: crate::theme::Theme) {
        self.theme = theme;
        // Refresh the per-pane gradient-border flag for the new theme so the
        // focused box switches its highlight style on the very next frame.
        self.sync_focus_flags();
        // Persistence is best-effort: a write failure never blocks the
        // in-session switch, which has already taken effect above. Skipped
        // under test so the suite never clobbers the developer's real config.
        if !cfg!(test) {
            let _ = crate::prefs::save_theme(theme);
        }
        if !cfg!(test) && crate::iterm2_inline::detect_iterm2_inline_support() {
            use std::io::Write;
            let mut out = stdout();
            let _ = out.write_all(set_session_bg_srgb_seq(theme.editor_bg_rgb()).as_bytes());
            let _ = out.flush();
        }
        // Re-bake the fixed icon/badge/run-debug PNGs against the new bg.
        self.init_graphics();
        // The welcome wordmark, no-repo hero, SSH empty state, and editor
        // image previews are baked lazily and keyed on layout, not theme —
        // drop them so the next render re-bakes them on the new background.
        self.overlays.welcome.disable();
        self.overlays.hero.disable();
        self.overlays.ssh.disable();
        self.overlays.editor[0].disable();
        self.overlays.editor[1].disable();
        // Evict the stale activity-icon image layer and force a full repaint
        // (the clear latch the main loop folds into `terminal.clear()`), so
        // every cell — SGR and image alike — repaints on the new background.
        self.overlays.activity.mark_dirty();
        self.overlays.activity.request_clear();
        self.status = format!("Color Theme: {}", theme.label());
    }

    fn open_at(&mut self, path: &Path, row: usize, col: usize) -> Result<()> {
        self.hover_popup = None;
        self.hover_diagnostic = None;
        self.editor.open_preview(path)?;
        self.sync_open_file_poll_mtime();
        let row = row.min(self.editor.lines.len().saturating_sub(1));
        let max_col = self
            .editor
            .lines
            .get(row)
            .map(|l| l.chars().count())
            .unwrap_or(0);
        self.editor.cursor_row = row;
        self.editor.cursor_col = col.min(max_col);
        let page = self.editor.page_size().max(1);
        self.editor.scroll = row.saturating_sub(page / 2);
        self.editor.scroll_col = 0;
        self.editor.ensure_cursor_col_visible();
        self.focus_pane(Pane::Editor);
        self.poke_cursor();
        Ok(())
    }

    fn nav_back(&mut self) {
        let Some(loc) = self.nav.back() else {
            self.status = "No previous location".to_string();
            return;
        };
        let line = loc.row;
        match self.open_at(&loc.path, loc.row, loc.col) {
            Ok(()) => self.status = format!("Back to {} line {}", loc.path.display(), line + 1),
            Err(e) => self.status = format!("Go back failed: {e}"),
        }
    }

    fn trigger_definition_at(&mut self, col: u16, row: u16) {
        if self.editor.diff.is_some() || self.editor.sheet.is_some() || self.editor.image.is_some()
        {
            return;
        }
        let Some((line, c)) = self.editor.buffer_pos_at(col, row) else {
            return;
        };
        self.editor.cursor_row = line;
        self.editor.cursor_col = c;
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_definition(path, line as u32, c as u32);
        self.definition_request_id = Some(id);
    }

    /// Request the LSP definition of the symbol at the current cursor (buffer)
    /// position. Used by the editor right-click "Go to Definition" item, where
    /// the cursor has already been moved to the click point.
    fn request_definition_at_cursor(&mut self) {
        let row = self.editor.cursor_row;
        let col = self.editor.cursor_col;
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_definition(path, row as u32, col as u32);
        self.definition_request_id = Some(id);
    }

    /// Request the LSP declaration of the symbol at the current cursor (buffer)
    /// position. Used by the editor right-click "Go to Declaration" item and the
    /// `⇧F12` keybinding. Declaration and definition coincide for languages with
    /// no separate declaration site (e.g. Python); they diverge for C/C++ (header
    /// prototype) and TypeScript (`.d.ts`).
    fn request_declaration_at_cursor(&mut self) {
        let row = self.editor.cursor_row;
        let col = self.editor.cursor_col;
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_declaration(path, row as u32, col as u32);
        self.declaration_request_id = Some(id);
    }

    /// Request the LSP type definition of the symbol at the current cursor
    /// (buffer) position. Used by the editor right-click "Go to Type Definition"
    /// item and the `⌃F12` keybinding. Jumps to where the *type* of the
    /// expression is defined, as opposed to the symbol itself (definition).
    fn request_type_definition_at_cursor(&mut self) {
        let row = self.editor.cursor_row;
        let col = self.editor.cursor_col;
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_type_definition(path, row as u32, col as u32);
        self.type_definition_request_id = Some(id);
    }

    /// Request the LSP implementations of the abstraction at the current cursor
    /// (buffer) position. Used by the editor right-click "Go to Implementations"
    /// item and the `⌘F12` keybinding. Meaningful on traits / interfaces /
    /// abstract methods; the server may return many, and croft jumps to the first.
    fn request_implementation_at_cursor(&mut self) {
        let row = self.editor.cursor_row;
        let col = self.editor.cursor_col;
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_implementation(path, row as u32, col as u32);
        self.implementation_request_id = Some(id);
    }

    /// Request the LSP references of the symbol at the current cursor (buffer)
    /// position. Used by the editor right-click "Go to References" item and the
    /// `⇧F12` keybinding. The server almost always returns many uses, which
    /// `drain_lsp_references` then offers as a picker.
    fn request_references_at_cursor(&mut self) {
        let row = self.editor.cursor_row;
        let col = self.editor.cursor_col;
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_references(path, row as u32, col as u32);
        self.references_request_id = Some(id);
    }

    /// Whether the current editor file's language server implements
    /// `textDocument/declaration`. Drives whether the right-click menu shows the
    /// "Go to Declaration" row. Unknown (server not yet spawned) or no server is
    /// treated as unsupported, so the row only appears once support is confirmed.
    fn editor_language_supports_declaration(&self) -> bool {
        let Some(lang) = self
            .editor
            .path
            .as_deref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(crate::lsp::Language::from_extension)
        else {
            return false;
        };
        self.lsp
            .as_ref()
            .and_then(|lsp| lsp.language_supports_declaration(lang))
            .unwrap_or(false)
    }

    /// Whether the current editor file's language server implements
    /// `textDocument/typeDefinition`. Drives whether the right-click menu shows
    /// the "Go to Type Definition" row. Unknown (server not yet spawned) or no
    /// server is treated as unsupported, so the row only appears once support is
    /// confirmed.
    fn editor_language_supports_type_definition(&self) -> bool {
        let Some(lang) = self
            .editor
            .path
            .as_deref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(crate::lsp::Language::from_extension)
        else {
            return false;
        };
        self.lsp
            .as_ref()
            .and_then(|lsp| lsp.language_supports_type_definition(lang))
            .unwrap_or(false)
    }

    /// Whether the current editor file's language server implements
    /// `textDocument/implementation`. Drives whether the right-click menu shows
    /// the "Go to Implementations" row. Unknown (server not yet spawned) or no
    /// server is treated as unsupported, so the row only appears once support is
    /// confirmed.
    fn editor_language_supports_implementation(&self) -> bool {
        let Some(lang) = self
            .editor
            .path
            .as_deref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(crate::lsp::Language::from_extension)
        else {
            return false;
        };
        self.lsp
            .as_ref()
            .and_then(|lsp| lsp.language_supports_implementation(lang))
            .unwrap_or(false)
    }

    /// Whether the current editor file's language server implements
    /// `textDocument/references`. Drives whether the right-click menu shows the
    /// "Go to References" row. Unknown (server not yet spawned) or no server is
    /// treated as unsupported, so the row only appears once support is confirmed.
    fn editor_language_supports_references(&self) -> bool {
        let Some(lang) = self
            .editor
            .path
            .as_deref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(crate::lsp::Language::from_extension)
        else {
            return false;
        };
        self.lsp
            .as_ref()
            .and_then(|lsp| lsp.language_supports_references(lang))
            .unwrap_or(false)
    }

    /// VS Code "Change All Occurrences" (Cmd/Ctrl+F2): select every textual
    /// match of the word under the cursor in the current file as multi-cursor
    /// carets so the next keystrokes edit them all at once.
    fn start_change_all_occurrences(&mut self) {
        if self.editor.diff.is_some() || self.editor.sheet.is_some() || self.editor.image.is_some()
        {
            return;
        }
        let n = self.editor.select_all_occurrences_of_word_at_cursor();
        if n == 0 {
            self.status = String::from("No symbol under cursor");
        } else {
            self.status = format!("Selected {n} occurrences (type to replace, Esc to cancel)");
        }
    }

    /// VS Code "Rename Symbol" (F2): prompt for a new name pre-filled with the
    /// identifier under the cursor; committing fires an LSP `rename` request.
    fn start_rename_symbol(&mut self) {
        if self.editor.diff.is_some() || self.editor.sheet.is_some() || self.editor.image.is_some()
        {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        let row = self.editor.cursor_row;
        let col = self.editor.cursor_col;
        let Some((start, end)) = self.editor.word_at(row, col) else {
            self.status = String::from("No symbol under cursor");
            return;
        };
        if self.lsp.is_none() {
            self.status = String::from("No language server for this file");
            return;
        }
        let word: String = self.editor.lines[row]
            .chars()
            .skip(start)
            .take(end - start)
            .collect();
        self.prompt = Some(Prompt {
            label: format!("Rename Symbol '{word}'"),
            buffer: word,
            kind: PromptKind::RenameSymbol { path, row, col },
            target_dir: PathBuf::new(),
            error: None,
        });
    }

    pub fn drain_lsp_rename(&mut self) -> bool {
        let mut arrived = false;
        let mut edits: Option<Vec<(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)>> = None;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_rename() {
                if Some(result.request_id) != self.rename_request_id {
                    continue;
                }
                arrived = true;
                edits = result.edits;
            }
        }
        if !arrived {
            return false;
        }
        self.rename_request_id = None;
        match edits {
            Some(files) => match self.apply_rename_edits(&files) {
                Ok((file_count, occ)) => {
                    self.status =
                        format!("Renamed {occ} occurrence(s) across {file_count} file(s)");
                }
                Err(e) => self.status = format!("Rename failed: {e}"),
            },
            None => self.status = String::from("Rename produced no changes"),
        }
        true
    }

    /// Apply an LSP rename `WorkspaceEdit` (already normalised to per-file
    /// char-indexed spans). Open tabs are edited in-memory as one undo step
    /// and left dirty; closed files are rewritten on disk (the fs watcher
    /// refreshes the tree). Returns (files touched, occurrences replaced).
    fn apply_rename_edits(
        &mut self,
        files: &[(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)],
    ) -> Result<(usize, usize)> {
        let mut file_count = 0;
        let mut occ_count = 0;
        for (path, edits) in files {
            if edits.is_empty() {
                continue;
            }
            if let Some(n) = self.editor.apply_rename_to_open_tab(path, edits) {
                occ_count += n;
            } else {
                let content = std::fs::read_to_string(path)?;
                let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
                occ_count += crate::widgets::editor::apply_span_edits_to_lines(&mut lines, edits);
                std::fs::write(path, lines.join("\n"))?;
            }
            file_count += 1;
        }
        Ok((file_count, occ_count))
    }

    /// Route the key through the completion popup when one is open.
    /// Up/Down navigate, Enter/Tab accept and replace the typed prefix
    /// with the chosen completion's full text, Esc dismisses. Printable
    /// chars and Backspace pass through to the editor and refresh the
    /// popup's filter prefix in place; if no items match the new prefix
    /// the popup dismisses on the next tick.
    fn handle_completion_popup_key(&mut self, key: KeyEvent) -> bool {
        if self.completion_popup.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Up => {
                if let Some(p) = self.completion_popup.as_mut() {
                    p.move_up();
                }
                true
            }
            KeyCode::Down => {
                if let Some(p) = self.completion_popup.as_mut() {
                    p.move_down();
                }
                true
            }
            KeyCode::Enter | KeyCode::Tab => {
                let prefix_len = self
                    .completion_popup
                    .as_ref()
                    .map(|p| p.prefix.chars().count())
                    .unwrap_or(0);
                let text = self
                    .completion_popup
                    .as_ref()
                    .and_then(|p| p.selected_label());
                self.completion_popup = None;
                self.completion_request_id = None;
                if let Some(t) = text {
                    for _ in 0..prefix_len {
                        self.editor.backspace();
                    }
                    self.editor.insert_str(&t);
                }
                true
            }
            KeyCode::Esc => {
                self.completion_popup = None;
                self.completion_request_id = None;
                true
            }
            KeyCode::Backspace => {
                self.editor.backspace();
                self.refresh_completion_prefix();
                true
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::SUPER)
                {
                    self.completion_popup = None;
                    self.completion_request_id = None;
                    return false;
                }
                self.editor.insert_char(c);
                self.refresh_completion_prefix();
                if !is_word_continuation(c) {
                    self.completion_popup = None;
                    self.completion_request_id = None;
                }
                true
            }
            _ => {
                self.completion_popup = None;
                self.completion_request_id = None;
                false
            }
        }
    }

    fn refresh_completion_prefix(&mut self) {
        let prefix = self.editor.word_before_cursor();
        let Some(popup) = self.completion_popup.as_mut() else {
            return;
        };
        popup.set_prefix(prefix);
        if popup.visible_is_empty() {
            self.completion_popup = None;
            self.completion_request_id = None;
        }
    }

    fn drain_fs_events(&mut self) -> bool {
        let init_changed = self.try_install_pending_init();
        let drain = self.fs_watch.drain(&mut self.tree, &self.editor);
        if drain.dirs_changed {
            self.refresh_git_status_debounced();
            if drain.finder_relevant {
                self.kick_file_finder_index_rebuild();
            }
        }
        // `touched_open_file` only tracks the ACTIVE tab; a background tab's
        // file change still arrives as a watcher event (got_any), so sweep
        // every tab whenever any event lands. The sweep is stamp-based, so
        // a benign Access/Metadata event that didn't move mtime+len is a
        // no-op and won't clobber selection or reload spuriously.
        if drain.got_any {
            self.reload_open_file_after_external_change();
        }
        let polled = self.poll_filesystem_changes();
        drain.got_any || init_changed || polled
    }

    fn cycle_focus(&mut self) {
        // Skip hidden panes when cycling.
        for _ in 0..3 {
            self.focus = match self.focus {
                Pane::Tree => Pane::Editor,
                Pane::Editor => Pane::Terminal,
                Pane::Terminal => Pane::Tree,
            };
            if self.pane_visible(self.focus) {
                break;
            }
        }
        self.sync_focus_flags();
    }

    /// Returns true exactly once after the welcome OSC-1337 image has been
    /// emitted and the editor pane has stopped being blank (i.e. a file is
    /// now open). The caller — the main draw loop — must respond by
    /// invalidating the prev buffer so ratatui repaints every cell on the
    /// next draw, wiping iTerm's image cache for the welcome region.
    pub fn consume_welcome_image_clear(&mut self) -> bool {
        let opened_file = !self.editor.is_blank_initial();
        self.overlays.welcome.consume_clear_or(opened_file)
    }

    fn render_welcome(&mut self, frame: &mut ratatui::Frame, outer_area: Rect) {
        self.welcome.clear_links();
        self.overlays.badge.set_target(None);
        if outer_area.width == 0 || outer_area.height == 0 {
            return;
        }
        // In iTerm2 image mode we let the iTerm session bg (forced to
        // sRGB(EDITOR_BG_RGB) by `SetColors=bg=srgb:…` at startup) show
        // through the welcome cells via SGR 49 (default bg). The OSC-1337
        // PNG canvas is filled with the same sRGB hex, so both surfaces
        // display the same physical pixel — no Generic-RGB vs sRGB seam.
        // Outside iTerm we fall back to explicit truecolor since there's
        // no PNG to match against.
        let bg = if crate::iterm2_inline::detect_iterm2_inline_support() {
            Style::default().bg(Color::Reset)
        } else {
            Style::default().bg(self.theme.editor_bg())
        };
        // Paint a full bordered box around the welcome area so the editor
        // pane has the same visible envelope as when a file is open
        // (Borders::ALL on the editor widget). Without this, the welcome
        // bg bleeds into the tree, the terminal, and the window edge with
        // no perceptible seam — and the sidebar splitter looks
        // unreachable. The bg style is applied to the same block so the
        // inside is filled in one pass. The welcome content then renders
        // into `block.inner(outer_area)` so a tall recents list can never
        // paint over the border rows.
        let outer_block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .style(bg);
        let area = outer_block.inner(outer_area);
        frame.render_widget(outer_block, outer_area);
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Layout regions, top-to-bottom:
        //   [logo + version badge]
        //   [tagline]
        //   [gradient box: header, provider, commits]
        //   [footer chevron line]
        let block_left = area.x + area.width / 8;
        let block_right = area.x + area.width - area.width / 8;
        let block_w = block_right.saturating_sub(block_left);
        // The gradient box's content area is 2 cells narrower (the box
        // border itself).
        let inner_w = block_w.saturating_sub(2);
        let has_recent_panel = self.welcome.has_recent_panel();
        let recents_inner_h =
            welcome_recents_height(self.welcome.remote(), self.welcome.commits(), inner_w);
        let tagline_h = 1u16;
        let footer_h = 1u16;
        // Gaps: blank row after logo, after tagline, after box.
        let gaps_h = 3u16;

        let logo_max_w = (area.width as u32).saturating_sub(4) as u16;
        let logo_w_cells = logo_max_w.clamp(8, 48);
        // Pick the logo height first, then size the recents box to fit
        // whatever's left. Without this a tall commit list would extend the
        // stack past the bottom of the welcome area and we'd panic painting
        // into rows that don't exist (ratatui buffers are fixed-size).
        let logo_h_cells = area
            .height
            .saturating_sub(tagline_h + footer_h + gaps_h)
            .clamp(4, 14);
        let used_above_box = logo_h_cells + 1 + tagline_h + 1; // logo, gap, tagline, gap
        let used_below_box = 1 + footer_h; // gap, footer
        let max_box_h = area
            .height
            .saturating_sub(used_above_box)
            .saturating_sub(used_below_box);
        // Box content needs at least the 4-cell border+inset envelope to be
        // worth drawing.
        let desired_box_h = if has_recent_panel {
            recents_inner_h.saturating_add(4)
        } else {
            0
        };
        let box_h = desired_box_h.min(max_box_h);

        let total_h = used_above_box + box_h + used_below_box;
        let block_top = area.y + area.height.saturating_sub(total_h) / 2;
        let logo_x = area.x + area.width.saturating_sub(logo_w_cells) / 2;
        let logo_y = block_top;
        let area_max_y = area.y + area.height;

        let desired = WelcomeLayout {
            cell_x: logo_x,
            cell_y: logo_y,
            cell_w: logo_w_cells,
            cell_h: logo_h_cells,
        };

        // Re-bake the OSC-1337 image whenever the layout shifts (resize,
        // sidebar toggle, font size change). The canvas is filled with the
        // raw `EDITOR_BG_RGB` bytes interpreted as sRGB (iTerm2 decodes
        // PNG bytes as sRGB by default). The pane's SGR-painted cells use
        // `Color::Reset` so they fall back to the iTerm session bg, which
        // we force to the same sRGB hex via `SetColors=bg=srgb:…` at
        // startup. Both surfaces therefore display the same physical
        // pixel pair-for-pair.
        if !self.overlays.welcome.layout_matches(&desired) {
            self.overlays.welcome.request_clear_if_displayed();
            if let Some((cw, ch)) = self.cell_pixel {
                let canvas_w = (logo_w_cells as u32) * cw;
                let canvas_h = (logo_h_cells as u32) * ch;
                let bg = self.theme_bg_pixel();
                let protocol = self.inline_protocol;
                if let Ok(baked) = crate::iterm2_inline::fit_image(
                    crate::iterm2_inline::WELCOME_LOGO_PNG,
                    canvas_w,
                    canvas_h,
                    bg,
                ) && let Some(raw) = crate::iterm2_inline::build_inline_image(
                    protocol,
                    &baked,
                    logo_w_cells,
                    logo_h_cells,
                    false,
                    crate::iterm2_inline::KITTY_ID_WELCOME,
                ) {
                    let osc = crate::iterm2_inline::maybe_tmux_wrap(
                        protocol,
                        crate::iterm2_inline::detect_tmux(),
                        raw,
                    );
                    self.overlays.welcome.set_image(osc);
                }
            }
            self.overlays.welcome.set_layout(desired);
            self.overlays.welcome.mark_dirty();
        }

        if !self.overlays.welcome.has_image() {
            // Text fallback for non-iTerm2 terminals.
            let label = " croft ";
            let lx = logo_x
                .saturating_add(logo_w_cells.saturating_sub(label.chars().count() as u16) / 2);
            let ly = logo_y + logo_h_cells / 2;
            frame.buffer_mut().set_string(
                lx,
                ly,
                label,
                Style::default()
                    .fg(Color::Rgb(0x9d, 0xa5, 0xb4))
                    .add_modifier(Modifier::BOLD),
            );
        }

        // Version badge to the right of the wordmark (the wordmark sits
        // in the lower portion of the logo PNG; the badge tracks that row).
        let version_label = format!(" v{} ", env!("CARGO_PKG_VERSION"));
        let version_w = version_label.chars().count() as u16;
        let badge_x = logo_x + logo_w_cells + 1;
        let badge_y = logo_y + (logo_h_cells * 5) / 6;
        if badge_x + version_w + 2 <= area.x + area.width && badge_y + 2 < area_max_y {
            let badge_style = Style::default()
                .fg(rgb_color(GRAD_TL))
                .add_modifier(Modifier::BOLD);
            frame
                .buffer_mut()
                .set_string(badge_x, badge_y, "\u{256d}", badge_style);
            for i in 0..version_w {
                frame
                    .buffer_mut()
                    .set_string(badge_x + 1 + i, badge_y, "\u{2500}", badge_style);
            }
            frame.buffer_mut().set_string(
                badge_x + 1 + version_w,
                badge_y,
                "\u{256e}",
                badge_style,
            );
            frame
                .buffer_mut()
                .set_string(badge_x, badge_y + 1, "\u{2502}", badge_style);
            frame.buffer_mut().set_string(
                badge_x + 1,
                badge_y + 1,
                &version_label,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );
            frame.buffer_mut().set_string(
                badge_x + 1 + version_w,
                badge_y + 1,
                "\u{2502}",
                badge_style,
            );
            frame
                .buffer_mut()
                .set_string(badge_x, badge_y + 2, "\u{2570}", badge_style);
            for i in 0..version_w {
                frame.buffer_mut().set_string(
                    badge_x + 1 + i,
                    badge_y + 2,
                    "\u{2500}",
                    badge_style,
                );
            }
            frame.buffer_mut().set_string(
                badge_x + 1 + version_w,
                badge_y + 2,
                "\u{256f}",
                badge_style,
            );
        }

        // Tagline.
        let tagline_y = logo_y + logo_h_cells + 1;
        let tagline_w = WELCOME_TAGLINE.chars().count() as u16;
        let tagline_x = area.x + area.width.saturating_sub(tagline_w) / 2;
        if tagline_y < area_max_y && tagline_w <= area.width {
            frame.buffer_mut().set_string(
                tagline_x,
                tagline_y,
                WELCOME_TAGLINE,
                Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0)),
            );
        }

        let box_y = tagline_y + tagline_h + 1;
        if has_recent_panel && box_h >= 4 && block_w >= 4 && box_y + box_h <= area_max_y {
            let box_rect = Rect {
                x: block_left,
                y: box_y,
                width: block_w,
                height: box_h,
            };
            paint_gradient_box(frame.buffer_mut(), box_rect);

            // Inner content area: 1-cell inset from each border.
            let inner_x = box_rect.x + 2;
            let inner_y = box_rect.y + 1;
            let inner_w_actual = box_rect.width.saturating_sub(4);

            let row_style = Style::default().fg(Color::Rgb(0xc5, 0xcd, 0xd9));
            let dim = Style::default().fg(Color::Rgb(0x6c, 0x7d, 0x9c));
            let link_style = Style::default()
                .fg(Color::Rgb(0x4e, 0x9a, 0xff))
                .add_modifier(Modifier::BOLD);

            // Header row: "▎ RECENT ACTIVITY".
            let header_y = inner_y;
            frame.buffer_mut().set_string(
                inner_x,
                header_y,
                "\u{258e} ",
                Style::default()
                    .fg(rgb_color(GRAD_TR))
                    .add_modifier(Modifier::BOLD),
            );
            frame.buffer_mut().set_string(
                inner_x + 2,
                header_y,
                "RECENT ACTIVITY",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );

            let recent_remote = self.welcome.remote().map(String::from);
            let recent_commits = self.welcome.commits().to_vec();
            let mut row_y = header_y + 2;
            if let Some(remote) = recent_remote.as_ref() {
                let provider = welcome_provider_label(remote);
                let badge = welcome_provider_badge(remote);
                frame
                    .buffer_mut()
                    .set_string(inner_x, row_y, &badge, link_style);
                let is_codeberg = matches!(
                    crate::git::commit_api_provider_for_remote(remote),
                    Some(crate::git::CommitApiProvider::Codeberg)
                );
                let badge_target = if is_codeberg && self.overlays.badge.has_image() {
                    Some((inner_x, row_y))
                } else {
                    None
                };
                self.overlays.badge.set_target(badge_target);
                let badge_w = badge.chars().count() as u16;
                let remote_x = inner_x + badge_w + 2;
                let room = (inner_x + inner_w_actual).saturating_sub(remote_x) as usize;
                let clipped: String = remote.chars().take(room).collect();
                frame.buffer_mut().set_string(remote_x, row_y, clipped, dim);
                let link_w = badge_w
                    .saturating_add(2)
                    .saturating_add(room.min(remote.chars().count()) as u16)
                    .min(inner_w_actual);
                self.welcome.push_link(WelcomeLink {
                    rect: Rect {
                        x: inner_x,
                        y: row_y,
                        width: link_w,
                        height: 1,
                    },
                    url: remote.clone(),
                    label: format!("Open {provider} repository"),
                });
                row_y += 2;
            }
            for c in &recent_commits {
                let y = row_y;
                if y >= box_rect.y + box_rect.height - 1 {
                    break;
                }
                frame
                    .buffer_mut()
                    .set_string(inner_x, y, &c.hash, link_style);
                let subject_x = inner_x + c.hash.chars().count() as u16 + 2;
                let when_w = c.when.chars().count() as u16;
                let row_end = inner_x + inner_w_actual;
                let when_x = row_end.saturating_sub(when_w);
                let subject_lines = wrapped_welcome_commit_subject(c, inner_w_actual);
                for (line_idx, line) in subject_lines.iter().enumerate() {
                    let line_y = y + line_idx as u16;
                    if line_y >= box_rect.y + box_rect.height - 1 {
                        break;
                    }
                    let room = if line_idx == 0 {
                        when_x.saturating_sub(subject_x).saturating_sub(2)
                    } else {
                        row_end.saturating_sub(subject_x)
                    };
                    let clipped: String = line.chars().take(room as usize).collect();
                    frame
                        .buffer_mut()
                        .set_string(subject_x, line_y, clipped, row_style);
                }
                frame.buffer_mut().set_string(when_x, y, &c.when, dim);
                if let Some(remote) = recent_remote.as_ref()
                    && let Some(url) = crate::git::commit_url_for_remote(remote, &c.full_hash)
                {
                    let height = subject_lines.len().max(1) as u16;
                    self.welcome.push_link(WelcomeLink {
                        rect: Rect {
                            x: inner_x,
                            y,
                            width: inner_w_actual,
                            height,
                        },
                        url,
                        label: format!("Open commit {}", c.hash),
                    });
                }
                row_y = row_y.saturating_add(subject_lines.len().max(1) as u16);
            }
        }
    }

    fn render_activity_bar(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // In images mode the activity bar inherits the iTerm session bg
        // (forced to sRGB(EDITOR_BG_RGB) via SetColors), matching the rest
        // of the panes. The glyph-fallback path keeps a solid bg for
        // terminals that can't render OSC-1337.
        let bg = if self.overlays.activity.has_images() {
            Style::default().bg(Color::Reset)
        } else {
            Style::default().bg(self.theme.editor_bg())
        };
        // In images mode the icon PNG owns the entire activity-bar block —
        // background, codicon, and active pill are baked in. Rendering a bg
        // block here would force a per-cell diff every frame, which in turn
        // would force us to re-emit the OSC-1337 images on every draw. Both
        // are visible to the user. So in images mode we leave the cells
        // untouched and let the post-draw OSC writer paint them once.
        if !self.overlays.activity.has_images() {
            frame.render_widget(ratatui::widgets::Block::default().style(bg), area);
        }
        let active_bar = Color::Rgb(0x4e, 0x9a, 0xff);
        let bg_color = bg.bg.unwrap_or(Color::Reset);
        let explorer_block = activity_explorer_block(area);
        let search_block = activity_search_block(area);
        let source_control_block = activity_source_control_block(area);
        let remote_block = activity_remote_block(area);
        let run_debug_block = activity_run_debug_block(area);
        let settings_block = activity_settings_block(area);
        let explorer_active = self.sidebar_view == SidebarView::Explorer;
        let search_active = self.sidebar_view == SidebarView::Search;
        let source_control_active = self.sidebar_view == SidebarView::SourceControl;
        let remote_active = self.sidebar_view == SidebarView::Remote;
        let run_debug_active = self.sidebar_view == SidebarView::RunDebug;
        let settings_active = self.settings_menu_active();
        // Hover brightens the glyph (like active) but draws no selection pill,
        // mirroring the image path's hovered variant. Selected wins over hover.
        let hov = self.hovered_activity_icon;
        let explorer_hovered = hov == Some(ActivityIcon::Explorer);
        let search_hovered = hov == Some(ActivityIcon::Search);
        let source_control_hovered = hov == Some(ActivityIcon::SourceControl);
        let remote_hovered = hov == Some(ActivityIcon::Remote);
        let run_debug_hovered = hov == Some(ActivityIcon::RunDebug);
        let settings_hovered = hov == Some(ActivityIcon::Settings);

        let active_color = Color::White;
        let inactive_color = Color::Rgb(0x6c, 0x7d, 0x9c);
        let glyph_x = activity_icon_glyph_x(area);
        let render_glyph = |frame: &mut ratatui::Frame,
                            block: Rect,
                            glyph: char,
                            is_active: bool,
                            is_hovered: bool| {
            let mid = block.y + block.height.saturating_sub(1) / 2;
            if is_active {
                let pill = Rect {
                    x: block.x,
                    y: mid,
                    width: 1,
                    height: 1,
                };
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("▎")
                        .style(Style::default().fg(active_bar).bg(bg_color)),
                    pill,
                );
            }
            let cell = Rect {
                x: glyph_x,
                y: mid,
                width: 1,
                height: 1,
            };
            let color = if is_active || is_hovered {
                active_color
            } else {
                inactive_color
            };
            frame.render_widget(
                ratatui::widgets::Paragraph::new(glyph.to_string()).style(
                    Style::default()
                        .fg(color)
                        .bg(bg_color)
                        .add_modifier(Modifier::BOLD),
                ),
                cell,
            );
        };

        if !self.overlays.activity.has_images() {
            // Glyph fallback path: render the codicon and a separate active
            // pill on the leftmost column. iTerm2's image path bakes the
            // pill into the PNG itself, so this branch is only used on
            // terminals that can't render OSC-1337.
            render_glyph(
                frame,
                explorer_block,
                crate::icons::ACTIVITY_EXPLORER,
                explorer_active,
                explorer_hovered,
            );
            render_glyph(
                frame,
                search_block,
                crate::icons::ACTIVITY_SEARCH,
                search_active,
                search_hovered,
            );
            render_glyph(
                frame,
                source_control_block,
                crate::icons::ACTIVITY_SOURCE_CONTROL,
                source_control_active,
                source_control_hovered,
            );
            render_glyph(
                frame,
                remote_block,
                crate::icons::ACTIVITY_REMOTE,
                remote_active,
                remote_hovered,
            );
            render_glyph(
                frame,
                run_debug_block,
                crate::icons::ACTIVITY_RUN_DEBUG,
                run_debug_active,
                run_debug_hovered,
            );
            if settings_block.width > 0 {
                render_glyph(
                    frame,
                    settings_block,
                    crate::icons::ACTIVITY_SETTINGS,
                    settings_active,
                    settings_hovered,
                );
            }
        }

        self.sidebar_areas.explorer_icon = explorer_block;
        self.sidebar_areas.search_icon = search_block;
        self.sidebar_areas.source_control_icon = source_control_block;
        self.sidebar_areas.remote_icon = remote_block;
        self.sidebar_areas.run_debug_icon = run_debug_block;
        self.sidebar_areas.settings_icon = settings_block;
    }

    fn set_sidebar_view(&mut self, view: SidebarView) {
        if self.sidebar_view != view {
            self.overlays.activity.mark_dirty();
            // Leaving Search while a scan is in flight: send an
            // empty-query request so the worker cancels the live walker
            // threads instead of letting them keep saturating every core
            // until the scan finishes. The user's hits stay populated for
            // when they come back; only the live work is dropped.
            if self.sidebar_view == SidebarView::Search && view != SidebarView::Search {
                let _ = self
                    .search_query_tx
                    .send(SearchRequest::Query(String::new(), self.search.opts));
            }
            // Leaving Run-Debug after the headline icon was on screen:
            // arm a one-shot `terminal.clear()` so iTerm2 evicts the
            // cached OSC-1337 image cells. Plain SGR overwrites do not
            // reliably evict those cells, but `\x1b[2J` (which
            // `terminal.clear()` emits) does — same gate the welcome
            // wordmark and editor preview pipelines use. The post-draw
            // flush function is silent on the non-Run-Debug path, so
            // nothing stomps the freshly-drawn next sidebar.
            if self.sidebar_view == SidebarView::RunDebug
                && view != SidebarView::RunDebug
                && self.overlays.run_debug.was_emitted()
            {
                self.overlays.run_debug.request_clear();
            }
            // Same eviction trick for the Source Control no-repo hero
            // PNG: leaving Source Control after the image was painted
            // would otherwise leave the cached PNG under whatever
            // sidebar replaces it.
            if self.sidebar_view == SidebarView::SourceControl && view != SidebarView::SourceControl
            {
                self.overlays.hero.request_clear_if_displayed();
            }
            // The Source Control commit dropdown is anchored to that view;
            // close it on any view change so it can't linger over the next
            // sidebar.
            self.commit_menu_open = false;
        }
        self.sidebar_view = view;
        if self.sidebar_view == SidebarView::Remote && self.remote.refresh_if_config_changed() {
            self.status = String::from("Reloaded SSH remotes");
        }
        if !self.show_tree {
            self.show_tree = true; // ensure the side panel is open when switching
        }
        match view {
            SidebarView::Explorer => self.focus_pane(Pane::Tree),
            SidebarView::Search => {
                self.focus_pane(Pane::Tree); // tree pane = side panel; dispatch by view
                self.search.select_all_query();
            }
            SidebarView::SourceControl => {
                self.refresh_source_control();
                self.focus_pane(Pane::Tree);
            }
            SidebarView::Remote => self.focus_pane(Pane::Tree),
            SidebarView::RunDebug => {
                self.refresh_run_debug();
                self.focus_pane(Pane::Tree);
            }
        }
    }

    fn pane_visible(&self, p: Pane) -> bool {
        match p {
            Pane::Tree => self.show_tree,
            Pane::Terminal => self.show_terminal,
            Pane::Editor => true,
        }
    }

    fn toggle_terminal(&mut self) {
        self.show_terminal = !self.show_terminal;
        // Hiding the terminal must also clear the maximize flag, otherwise
        // the next time the user reopens the terminal it would still hide
        // the editor — which is surprising because Ctrl+J is the user's
        // mental "show my normal layout again" key.
        if !self.show_terminal {
            self.terminal_maximized = false;
        }
        // If we just hid the terminal while it was focused, fall back to editor.
        if !self.show_terminal && self.focus == Pane::Terminal {
            self.focus_pane(Pane::Editor);
        }
        // If we just showed the terminal, optionally jump focus to it for quick use.
        if self.show_terminal {
            self.focus_pane(Pane::Terminal);
        }
    }

    /// `Ctrl+Shift+J`: flip the maximize-terminal flag. When turning ON,
    /// also ensure the terminal pane is visible and focused so the user
    /// can start typing immediately. When turning OFF, re-arm the welcome
    /// wordmark so it re-paints in the restored editor area.
    fn toggle_terminal_maximize(&mut self) {
        self.terminal_maximized = !self.terminal_maximized;
        if self.terminal_maximized {
            if !self.show_terminal {
                self.show_terminal = true;
            }
            self.focus_pane(Pane::Terminal);
            self.overlays.welcome.request_clear_if_displayed();
        } else {
            self.overlays.welcome.mark_dirty();
        }
    }

    pub fn terminal(&self) -> &PtyTerminal {
        &self.terminals[self.active_terminal]
    }

    pub fn terminal_mut(&mut self) -> &mut PtyTerminal {
        &mut self.terminals[self.active_terminal]
    }

    /// Spawn a new PTY-backed terminal next to the existing ones and make
    /// it the active one. The pane becomes visible if it was hidden. The
    /// new terminal's cwd is the *live* cwd of the active terminal's shell
    /// (so a `cd somewhere` inside the user's prompt is reflected), with
    /// the workspace root as a fallback if we can't resolve it.
    ///
    /// The new terminal lands immediately to the right of the currently
    /// active one, not at the far end of the strip, so the user's mental
    /// model of "the new tab sits next to where I was" matches what they
    /// see. Inserting at the end would scatter related splits across the
    /// row whenever the active terminal wasn't already on the right edge.
    pub fn split_terminal(&mut self) -> Result<()> {
        let cwd = self
            .terminal()
            .pid()
            .and_then(cwd_of_pid)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| self.workspace_root.clone());
        let term = PtyTerminal::new(&cwd).context("spawning terminal")?;
        let target = self.active_terminal + 1;
        self.terminals.insert(target, term);
        self.active_terminal = target;
        if !self.show_terminal {
            self.show_terminal = true;
        }
        self.focus_pane(Pane::Terminal);
        Ok(())
    }

    /// Drop the terminal at `idx`. Returns false (and does nothing) when
    /// only one terminal is left or `idx` is out of range. Hiding the pane
    /// is the user's job (Ctrl+J), not ours, so the last terminal stays.
    /// The active index shifts so the same logical terminal remains active
    /// when an inactive pane is closed; closing the active pane reseats the
    /// focus on the neighbour to the left (or the new last if we removed
    /// the right edge).
    pub fn close_terminal_at(&mut self, idx: usize) -> bool {
        if self.terminals.len() <= 1 || idx >= self.terminals.len() {
            return false;
        }
        self.terminals.remove(idx);
        if self.active_terminal == idx {
            if self.active_terminal >= self.terminals.len() {
                self.active_terminal = self.terminals.len() - 1;
            }
        } else if self.active_terminal > idx {
            self.active_terminal -= 1;
        }
        self.sync_focus_flags();
        true
    }

    /// Drop the currently-active terminal. Thin wrapper kept for the
    /// keyboard shortcut and existing callers.
    pub fn close_active_terminal(&mut self) -> bool {
        self.close_terminal_at(self.active_terminal)
    }

    /// Cycle the active terminal forward by one slot, wrapping at the end.
    pub fn cycle_terminal(&mut self) {
        if self.terminals.len() <= 1 {
            return;
        }
        self.active_terminal = (self.active_terminal + 1) % self.terminals.len();
        self.sync_focus_flags();
    }

    /// Cycle the active terminal backward by one slot, wrapping at the
    /// front. Mirror of `cycle_terminal` for the Cmd+[ binding.
    pub fn cycle_terminal_back(&mut self) {
        let n = self.terminals.len();
        if n <= 1 {
            return;
        }
        self.active_terminal = (self.active_terminal + n - 1) % n;
        self.sync_focus_flags();
    }

    /// OR-fold dirty flags across all terminals while clearing each. Use `|`
    /// (not `||`) so every terminal's flag is consumed even after the first
    /// dirty one is found.
    pub fn drain_terminals_dirty(&mut self) -> bool {
        self.terminals
            .iter()
            .fold(false, |acc, t| acc | t.take_dirty())
    }

    /// Like `drain_terminals_dirty` but does not clear the underlying flags.
    /// The main loop uses this to decide whether to coalesce a PTY-only
    /// redraw without losing the signal if it chooses to skip.
    pub fn peek_terminals_dirty(&self) -> bool {
        self.terminals.iter().any(|t| t.peek_dirty())
    }

    /// Total bytes the PTY reader threads have advanced into their grids
    /// since the last redraw. The main loop uses this to bypass the PTY
    /// redraw cap for small interactive echo while keeping bulk streams
    /// capped. Peeks without clearing; `drain_terminals_dirty` resets it.
    pub fn peek_terminals_pending_bytes(&self) -> usize {
        self.terminals.iter().map(|t| t.peek_pending_bytes()).sum()
    }

    fn terminal_at_pos(&self, col: u16, row: u16) -> Option<usize> {
        self.terminals
            .iter()
            .position(|t| rect_contains(t.last_area, col, row))
    }

    fn focus_pane(&mut self, p: Pane) {
        self.focus = p;
        self.sync_focus_flags();
        if self.editor.focused {
            self.poke_cursor();
        }
    }

    fn sync_focus_flags(&mut self) {
        self.tree.focused = self.focus == Pane::Tree && self.sidebar_view == SidebarView::Explorer;
        self.search.focused = self.focus == Pane::Tree && self.sidebar_view == SidebarView::Search;
        self.source_control.focused =
            self.focus == Pane::Tree && self.sidebar_view == SidebarView::SourceControl;
        self.remote.focused = self.focus == Pane::Tree && self.sidebar_view == SidebarView::Remote;
        self.run_debug.focused =
            self.focus == Pane::Tree && self.sidebar_view == SidebarView::RunDebug;
        self.editor.focused = self.focus == Pane::Editor;
        // The split group is by definition the NON-focused editor group,
        // so its border never lights up even while the editor pane holds
        // focus. `editor` (the focused group) carries the highlight.
        if let Some(split) = self.editor_split.as_mut() {
            split.focused = false;
        }
        let focused_pane = self.focus == Pane::Terminal;
        let active = self.active_terminal;
        for (i, t) in self.terminals.iter_mut().enumerate() {
            t.focused = focused_pane && i == active;
        }
        // Only the Black theme dresses the focused pane in the gradient
        // border; Croft Dark keeps the historical solid-blue highlight.
        let gradient = self.theme == crate::theme::Theme::Black;
        // Every editor in both groups, not just the active one (deref would
        // only hit the active editor): a tab switch or the split group must
        // not resurrect the legacy navy active-tab chip between syncs.
        for ed in self.editor.editors.iter_mut() {
            ed.focus_gradient = gradient;
            ed.theme = self.theme;
        }
        if let Some(split) = self.editor_split.as_mut() {
            for ed in split.editors.iter_mut() {
                ed.focus_gradient = gradient;
                ed.theme = self.theme;
            }
        }
        self.tree.focus_gradient = gradient;
        self.tree.theme = self.theme;
        self.search.focus_gradient = gradient;
        self.search.theme = self.theme;
        self.source_control.focus_gradient = gradient;
        self.source_control.theme = self.theme;
        self.run_debug.focus_gradient = gradient;
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
        self.dependencies.focus_gradient = gradient;
        self.dependencies.theme = self.theme;
        self.dependencies.focused = explorer_focused;
        self.remote.focus_gradient = gradient;
        self.remote.theme = self.theme;
        for t in self.terminals.iter_mut() {
            t.focus_gradient = gradient;
        }
    }

    /// Whether popups/menus/tooltips should wear the orange→green gradient
    /// border and the muted dark-teal selection fill instead of the legacy
    /// bright-blue accent. Gated on the Black theme to mirror the focused-pane
    /// border, so Croft Dark keeps its coherent all-blue look.
    fn popup_gradient(&self) -> bool {
        self.theme == crate::theme::Theme::Black
    }

    /// Split the editor into two side-by-side columns (`Cmd+\`). The new
    /// group duplicates the active file (re-opens its path, copies the
    /// cursor/scroll position) and takes focus, landing in the right
    /// column. No-op if already split or the active tab has no path on
    /// disk (a blank/unsaved buffer has nothing to mirror).
    fn split_editor(&mut self) {
        if self.editor_split.is_some() {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Cannot split: save the file first");
            return;
        };
        // A fresh EditorTabs starts blank-initial; `open_preview` reuses
        // that placeholder tab so the new group holds exactly the file
        // (not a stray blank tab beside it). Opening it as the PREVIEW
        // slot - not pinned - means the next file the user opens in this
        // column swaps straight into it (clean "left = A, right = B"
        // comparison) rather than stacking a second tab. Editing or
        // double-clicking pins it, as everywhere else.
        let mut group = EditorTabs::default();
        if group.open_preview(&path).is_err() {
            self.status = String::from("Cannot split: failed to open file");
            return;
        }
        // Mirror the source view state (caret + scroll, incl. the wrap
        // sub-line offset) so the duplicate opens exactly where the
        // source was rather than at the top of the file.
        group.copy_view_position_from(&self.editor);
        // The existing group stays in the LEFT column; the new focused
        // group renders on the RIGHT, so `editor` (always the focused
        // group) is the right column → `split_focus_left = false`.
        self.editor_split = Some(std::mem::replace(&mut self.editor, group));
        self.split_focus_left = false;
        self.editor_split_left_width = None;
        self.focus_pane(Pane::Editor);
    }

    /// Move keyboard focus to the editor group in the given physical
    /// column (`true` = left, `false` = right). Swaps `editor` and
    /// `editor_split` so the focused group is always `editor`, keeping the
    /// physical columns fixed. No-op when not split or already focused
    /// there.
    fn focus_editor_group(&mut self, want_left: bool) {
        if self.editor_split.is_none() || self.split_focus_left == want_left {
            // Still ensure the editor pane has focus even if the side is
            // already correct, so the chord doubles as "focus editor".
            if self.editor_split.is_some() && self.focus != Pane::Editor {
                self.focus_pane(Pane::Editor);
            }
            return;
        }
        if let Some(split) = self.editor_split.as_mut() {
            std::mem::swap(&mut self.editor, split);
        }
        self.split_focus_left = want_left;
        self.focus_pane(Pane::Editor);
    }

    /// After a tab close, collapse the split if the focused group ran out
    /// of real tabs. The surviving (other) group becomes the sole editor.
    fn collapse_split_if_empty(&mut self) {
        if self.editor_split.is_none() || !self.editor.is_blank_initial() {
            return;
        }
        // The focused group is empty; promote the other group to be the
        // single editor and tear the split down. Evict the right slot's
        // image so it can't ghost after the column disappears.
        if let Some(other) = self.editor_split.take() {
            self.editor = other;
        }
        self.split_focus_left = true;
        self.editor_split_left_width = None;
        self.editor_splitter_x = None;
        self.disable_editor_image(1);
        // Focus stays on the editor pane; just re-light the promoted
        // group's border. (No `focus_pane` so this is safe to call from
        // any close path without re-poking the cursor mid-frame.)
        self.sync_focus_flags();
    }

    fn render(&mut self, frame: &mut ratatui::Frame) {
        let size = frame.area();
        self.last_frame_area = size;
        // The on-screen keyboard docks as a band between the panes and the
        // status bar, scaling its keys to thumb size on tall frames; frames
        // too short to host it keep it collapsed so the workspace can't be
        // squeezed into nothing.
        let osk_h = if self.osk.is_some() && size.height >= OSK_MIN_FRAME_HEIGHT {
            crate::widgets::osk::band_height(size.height)
        } else {
            0
        };
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(osk_h),
                Constraint::Length(1),
            ])
            .split(size);

        // Clamp sidebar width to leave at least RIGHT_PANE_MIN cells for
        // the editor + terminal. Window resizes shrink the sidebar to fit
        // rather than refusing to render the right pane.
        let total_w = outer[0].width;
        let max_sidebar = total_w
            .saturating_sub(ACTIVITY_BAR_WIDTH)
            .saturating_sub(RIGHT_PANE_MIN);
        let sidebar_w = self
            .sidebar_width
            .clamp(SIDEBAR_WIDTH_MIN, max_sidebar.max(SIDEBAR_WIDTH_MIN));
        // Persist the clamped value so subsequent drags start from where
        // the user can actually see the splitter.
        self.sidebar_width = sidebar_w;

        let main = if self.show_tree {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(ACTIVITY_BAR_WIDTH),
                    Constraint::Length(sidebar_w),
                    Constraint::Min(RIGHT_PANE_MIN),
                ])
                .split(outer[0])
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(ACTIVITY_BAR_WIDTH),
                    Constraint::Min(RIGHT_PANE_MIN),
                ])
                .split(outer[0])
        };

        let (activity_area, side_area, right_area) = if self.show_tree {
            (main[0], Some(main[1]), main[2])
        } else {
            (main[0], None, main[1])
        };
        // Remember the Explorer column so the zoxide jump popup can anchor
        // over it instead of centering on the whole frame.
        self.last_sidebar_area = side_area;

        // Splitter column is the leftmost cell of the right (editor) pane —
        // i.e. the seam where the sidebar's border meets the editor's
        // border. Mouse-down on that column starts a horizontal drag.
        self.sidebar_splitter_x = if self.show_tree {
            Some(right_area.x)
        } else {
            None
        };
        self.last_content_width = right_area.width;
        self.last_content_height = right_area.height;

        // While the on-screen keyboard is up, the screen is too small to
        // host editor + terminal + keys, and the user is only typing into
        // one pane anyway: the focused pane keeps the whole right column
        // (riding directly above the keys) and the other folds away.
        let (editor_area, terminal_area) =
            if osk_h > 0 && self.focus == Pane::Terminal && self.show_terminal {
                let zero_editor = Rect {
                    x: right_area.x,
                    y: right_area.y,
                    width: right_area.width,
                    height: 0,
                };
                self.terminal_splitter_y = None;
                (zero_editor, Some(right_area))
            } else if osk_h > 0 {
                self.terminal_splitter_y = None;
                (right_area, None)
            } else if self.show_terminal {
                if self.terminal_maximized {
                    // Editor / welcome collapses; terminal fills the entire
                    // right column next to the Explorer. Splitter is hidden
                    // because there is nothing to drag against.
                    let zero_editor = Rect {
                        x: right_area.x,
                        y: right_area.y,
                        width: right_area.width,
                        height: 0,
                    };
                    self.terminal_splitter_y = None;
                    (zero_editor, Some(right_area))
                } else {
                    let total_h = right_area.height;
                    let pinned = self.terminal_height.map(|h| {
                        h.clamp(
                            TERMINAL_HEIGHT_MIN,
                            total_h
                                .saturating_sub(EDITOR_HEIGHT_MIN)
                                .max(TERMINAL_HEIGHT_MIN),
                        )
                    });
                    let right = if let Some(term_h) = pinned {
                        self.terminal_height = Some(term_h);
                        Layout::default()
                            .direction(Direction::Vertical)
                            .constraints([
                                Constraint::Min(EDITOR_HEIGHT_MIN),
                                Constraint::Length(term_h),
                            ])
                            .split(right_area)
                    } else {
                        Layout::default()
                            .direction(Direction::Vertical)
                            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
                            .split(right_area)
                    };
                    self.terminal_splitter_y = Some(right[1].y);
                    (right[0], Some(right[1]))
                }
            } else {
                self.terminal_splitter_y = None;
                (right_area, None)
            };

        self.render_activity_bar(frame, activity_area);

        if let Some(area) = side_area {
            let usable_area = area;
            // Pin the SYSTEM panel to the bottom of the usable strip,
            // beneath whichever activity is showing. The active widget
            // gets the remaining space and scrolls within it; the panel
            // shrinks to a 1-row chevron strip when collapsed, and
            // hides entirely when the column is too short to leave the
            // activity widget enough room to be usable.
            let panel_h = self.system_panel.desired_height(usable_area.height);
            let (after_system, panel_area) = if panel_h > 0 {
                let split = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(1), Constraint::Length(panel_h)])
                    .split(usable_area);
                (split[0], Some(split[1]))
            } else {
                (usable_area, None)
            };
            // Feed the render-time pointer cell to every side panel so the row
            // under the cursor (and the remote header pills) can light up,
            // mirroring the editor tab strip and terminal pane buttons.
            let panel_pointer = self.pointer_cell;
            self.tree.hover_pointer = panel_pointer;
            self.search.hover_pointer = panel_pointer;
            self.source_control.hover_pointer = panel_pointer;
            self.remote.hover_pointer = panel_pointer;
            self.outline.hover_pointer = panel_pointer;
            self.open_editors.hover_pointer = panel_pointer;
            self.timeline.hover_pointer = panel_pointer;
            self.dependencies.hover_pointer = panel_pointer;
            // Explorer stacks its toggled sub-views (Open Editors / Folders /
            // Outline / Timeline / Dependencies) inside `after_system`.
            // Every other sidebar view fills it with its single activity widget.
            match self.sidebar_view {
                SidebarView::Explorer => self.render_explorer_sections(frame, after_system),
                SidebarView::Search => frame.render_widget(&mut self.search, after_system),
                SidebarView::SourceControl => {
                    frame.render_widget(&mut self.source_control, after_system)
                }
                SidebarView::Remote => frame.render_widget(&mut self.remote, after_system),
                SidebarView::RunDebug => frame.render_widget(&mut self.run_debug, after_system),
            }
            if let Some(pa) = panel_area {
                frame.render_widget(&mut self.system_panel, pa);
            } else {
                self.system_panel.last_area = Rect::default();
            }
        }
        if self.editor_split.is_none() && self.editor.is_blank_initial() {
            self.render_welcome(frame, editor_area);
            // The previous frame may have rendered an image-preview tab
            // whose OSC-1337 pixels are still cached in iTerm's image
            // store. Closing that tab brings us here, but ratatui's diff
            // alone doesn't tell iTerm to drop the image — flag it so the
            // main loop wipes the screen on the next pass. Both slots, in
            // case a split was just collapsed back to the welcome screen.
            self.disable_editor_image(0);
            self.disable_editor_image(1);
            self.editor_splitter_x = None;
            // Keep the editor's hit-test rectangles fresh so the activity-bar
            // / tree click logic still works even though we skipped the
            // EditorTabs widget this frame.
            self.editor.last_full_area = editor_area;
            self.editor.last_area = Rect {
                x: editor_area.x,
                y: editor_area.y,
                width: editor_area.width,
                height: 0,
            };
        } else {
            // The editor just overdrew whatever cells the welcome image
            // occupied; if the user reopens the welcome screen we'll need
            // to re-emit it.
            self.overlays.welcome.mark_dirty();
            self.overlays.badge.set_target(None);
            // Feed the render-time pointer cell to every editor group so the
            // tab strip can light the hovered tab body / close-cross pill,
            // mirroring the terminal pane buttons' hover affordance.
            let pointer = self.pointer_cell;
            self.editor.hover_pointer = pointer;
            if let Some(split) = self.editor_split.as_mut() {
                split.hover_pointer = pointer;
            }
            // Render either a single editor group or two side-by-side
            // columns, and report which column has focus so the
            // completion / hover popups anchor there.
            let focused_area = if self.editor_split.is_some() {
                let total = editor_area.width;
                let max_left = total.saturating_sub(EDITOR_SPLIT_MIN).max(EDITOR_SPLIT_MIN);
                let left_w = self
                    .editor_split_left_width
                    .unwrap_or(total / 2)
                    .clamp(EDITOR_SPLIT_MIN, max_left);
                let cols = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Length(left_w),
                        Constraint::Min(EDITOR_SPLIT_MIN),
                    ])
                    .split(editor_area);
                let (left_area, right_area) = (cols[0], cols[1]);
                // The seam sits on the right column's left border; that
                // single column is the drag hit target.
                self.editor_splitter_x = Some(right_area.x);
                // `editor` is always the focused group; `split_focus_left`
                // says which physical column it occupies.
                if self.split_focus_left {
                    frame.render_widget(&mut self.editor, left_area);
                    if let Some(split) = self.editor_split.as_mut() {
                        frame.render_widget(split, right_area);
                    }
                } else {
                    if let Some(split) = self.editor_split.as_mut() {
                        frame.render_widget(split, left_area);
                    }
                    frame.render_widget(&mut self.editor, right_area);
                }
                // Image overlays are keyed by physical column.
                self.update_editor_image_overlay(0, left_area);
                self.update_editor_image_overlay(1, right_area);
                if self.split_focus_left {
                    left_area
                } else {
                    right_area
                }
            } else {
                frame.render_widget(&mut self.editor, editor_area);
                self.editor_splitter_x = None;
                self.update_editor_image_overlay(0, editor_area);
                // No right pane: make sure its slot can't ghost an image.
                self.disable_editor_image(1);
                editor_area
            };
            let grad = self.popup_gradient();
            if self.focus == Pane::Editor
                && self.completion_popup.is_some()
                && let Some((cx, cy)) = self.editor.cursor_screen_pos()
            {
                if let Some(popup) = self.completion_popup.as_mut() {
                    popup.anchor = (cx, cy);
                    popup.gradient = grad;
                }
                let popup_ref = self.completion_popup.as_ref().unwrap();
                let area = popup_ref.area_for(focused_area);
                if area.width > 0 && area.height > 0 {
                    frame.render_widget(popup_ref, area);
                }
            }
            if let Some(popup) = self.hover_popup.as_mut() {
                popup.gradient = grad;
                let area = popup.area_for(focused_area);
                if area.width > 0 && area.height > 0 {
                    frame.render_widget(&*popup, area);
                }
            }
            if let Some(popup) = self.tab_tooltip.as_mut() {
                popup.gradient = grad;
                let area = popup.area_for(focused_area);
                if area.width > 0 && area.height > 0 {
                    frame.render_widget(&*popup, area);
                }
            }
            if let Some(popup) = self.ui_tooltip.as_mut() {
                popup.gradient = grad;
                let area = popup.area_for(focused_area);
                if area.width > 0 && area.height > 0 {
                    frame.render_widget(&*popup, area);
                }
            }
        }
        if let Some(area) = terminal_area {
            let n = self.terminals.len().max(1);
            let constraints: Vec<Constraint> =
                (0..n).map(|_| Constraint::Ratio(1, n as u32)).collect();
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(constraints)
                .split(area);
            for (i, t) in self.terminals.iter_mut().enumerate() {
                frame.render_widget(t, cols[i]);
            }
            let show_close = self.terminals.len() > 1;
            let brand = self.theme == crate::theme::Theme::Black;
            self.terminal_add_buttons.clear();
            self.terminal_close_buttons.clear();
            for col in cols.iter().take(self.terminals.len()) {
                let (add_rect, close_rect) =
                    paint_terminal_pane_buttons(frame, *col, show_close, brand, self.pointer_cell);
                if let Some(r) = add_rect {
                    self.terminal_add_buttons.push(r);
                }
                if let Some(r) = close_rect {
                    self.terminal_close_buttons.push(r);
                }
            }
        } else {
            self.terminal_add_buttons.clear();
            self.terminal_close_buttons.clear();
            // A hidden terminal must not keep stale hit rects alive, or
            // `terminal_at_pos` would phantom-match clicks meant for
            // whatever now occupies those cells (mirrors the SYSTEM
            // panel's hidden-state reset).
            for t in self.terminals.iter_mut() {
                t.last_area = Rect::default();
            }
        }

        let mut spans: Vec<Span> = Vec::with_capacity(20);
        spans.extend(brand_spans());
        if let Some(span) = self.perf.status_span() {
            spans.push(span);
        }
        if self.update_status == UpdateStatus::Ready {
            spans.push(Span::styled(
                " ⟳ Update ready - F9 to relaunch ",
                Style::default()
                    .bg(Color::Rgb(0x1f, 0x7a, 0x33))
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if self.update_status == UpdateStatus::InProgress {
            // Red + a spinning circular-arrow glyph to draw the eye to the
            // in-progress background update.
            spans.push(Span::styled(
                format!(
                    " {} Updating croft in the background ",
                    self.update_spinner_glyph()
                ),
                Style::default()
                    .fg(Color::Rgb(0xff, 0x45, 0x4d))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(progress) = self.lsp_progress_status() {
            // A busy language server (rust-analyzer priming its crate graph,
            // etc.) is otherwise invisible: hover/completion just return empty
            // until it finishes. Surfacing "rust-analyzer: Indexing …" tells
            // the user to wait rather than assume the server is dead.
            spans.push(Span::styled(
                format!(" ⟳ {progress} "),
                Style::default()
                    .fg(Color::Rgb(0x4e, 0x9a, 0xff))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if self.vim.enabled {
            let (text, bg) = match self.vim.command_line() {
                Some(cmd) => (format!(" {cmd} "), Color::Rgb(0x33, 0x33, 0x33)),
                None => {
                    let label = self.vim.mode_label().unwrap_or("NORMAL");
                    let bg = match self.vim.mode() {
                        crate::vim::VimMode::Insert => Color::Rgb(0x1f, 0x7a, 0x33),
                        crate::vim::VimMode::Visual | crate::vim::VimMode::VisualLine => {
                            Color::Rgb(0x8a, 0x4f, 0xbf)
                        }
                        _ => Color::Rgb(0x4e, 0x9a, 0xff),
                    };
                    (format!(" {label} "), bg)
                }
            };
            spans.push(Span::styled(
                text,
                Style::default()
                    .bg(bg)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(" "));
        }
        spans.extend(git_status_spans(self.git.status()));
        spans.push(Span::raw("  "));
        // Persistent orange badge for a remote session with no dtach: it can't
        // survive a transport drop, so flag it for the whole session (rendered
        // before the transient status so a status update never hides it).
        if let Some(warning) = self.persistence_warning {
            spans.push(Span::styled(
                format!(" \u{26a0} {warning} "),
                Style::default()
                    .bg(Color::Rgb(0xff, 0x9d, 0x2f))
                    .fg(Color::Rgb(0x10, 0x13, 0x1a))
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw("  "));
        }
        spans.push(Span::raw(&self.status));
        spans.push(Span::raw("  "));
        spans.push(Span::styled("^q", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" Quit  "));
        spans.push(Span::styled("^s", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" Save  "));
        spans.push(Span::styled("F6", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" Cycle pane  "));
        spans.push(Span::styled("^b", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" Tree  "));
        spans.push(Span::styled("^j", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" Term  "));
        let shortcuts_label_start_col: u16 =
            spans.iter().map(|s| s.content.chars().count() as u16).sum();
        spans.push(Span::styled("F1", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" Shortcuts"));
        let shortcuts_label_len: u16 = "F1 Shortcuts".chars().count() as u16;
        if let Some(osk) = self.osk.as_mut() {
            if osk_h > 0 {
                crate::widgets::osk::render_osk(osk, outer[1], frame.buffer_mut(), self.theme);
            } else {
                // Collapsed (frame too short): zero the hit rect so the
                // invisible band can't swallow clicks.
                osk.last_area = Rect::default();
            }
        }

        let status_rect = outer[2];
        let hit_x = status_rect.x.saturating_add(shortcuts_label_start_col);
        let hit_end = hit_x
            .saturating_add(shortcuts_label_len)
            .min(status_rect.right());
        self.shortcuts_hit_rect = (hit_end > hit_x).then(|| Rect {
            x: hit_x,
            y: status_rect.y,
            width: hit_end - hit_x,
            height: 1,
        });
        // Black theme: drop the navy status strip to a near-black seam (a hair
        // lighter than the #000000 editor so the bar still reads as distinct),
        // matching VS Code's OLED themes. Croft Dark keeps the navy band.
        let status_bg = if self.popup_gradient() {
            Color::Rgb(0x18, 0x18, 0x18)
        } else {
            Color::Rgb(0x1e, 0x3a, 0x6e)
        };
        let status = Paragraph::new(Line::from(spans)).style(Style::default().bg(status_bg));
        frame.render_widget(status, outer[2]);

        // Overlays render last so they sit on top of everything else.
        self.render_context_menu(frame);
        self.render_commit_dropdown(frame);
        self.render_prompt(frame);
        self.render_local_open_confirm(frame);
        self.render_discard_confirm(frame);
        self.render_editor_find(frame);
        self.render_file_finder(frame);
        self.render_command_palette(frame);
        self.render_process_picker(frame);
        self.render_zoxide_jump(frame);
        self.render_shortcuts_modal(frame);
        self.render_connect_dialog(frame);
        // The startup unsupported-terminal nudge renders last so it sits above
        // every other overlay until the user dismisses it.
        self.render_terminal_warning(frame);

        // Show the host terminal's hardware caret only when the editor is
        // focused and has no modal overlay. The DECSCUSR style is set to
        // BlinkingBar at startup, so the terminal blinks a thin vertical
        // line over the cursor cell without replacing its character.
        if self.focus == Pane::Editor
            && self.context_menu.is_none()
            && self.prompt.is_none()
            && self.cursor_visible_phase()
        {
            let pos = if self.editor.diff.is_some() {
                self.editor.diff_caret_screen_pos()
            } else {
                self.editor.cursor_screen_pos()
            };
            if let Some((cx, cy)) = pos {
                frame.set_cursor_position((cx, cy));
            }
        }

        // Source-control commit-message caret. Same software-blink rule:
        // the user can't see where they're typing in the message box
        // without a visible caret, since `set_string` just lays the text
        // flat with no native caret cell.
        if self.focus == Pane::Tree
            && self.sidebar_view == SidebarView::SourceControl
            && self.context_menu.is_none()
            && self.prompt.is_none()
            && self.cursor_visible_phase()
            && let Some((cx, cy)) = self.source_control.cursor_screen_pos()
        {
            frame.set_cursor_position((cx, cy));
        }
    }

    fn render_context_menu(&self, frame: &mut ratatui::Frame) {
        let Some(menu) = &self.context_menu else {
            return;
        };
        // `menu_rect` already clamps against `last_frame_area`, so the
        // rect we draw here is the same rect `menu_item_at` hit-tests
        // against. Keeping the two in lock-step is what prevents the
        // off-by-N row dispatch when the menu has to shift up to fit.
        let Some(clipped) = self.menu_rect() else {
            return;
        };
        // Black theme: orange→green gradient border + muted dark-teal
        // selection. Croft Dark keeps the legacy bright-blue accent so the
        // menu stays coherent with that theme's blue focus border.
        let grad = self.popup_gradient();
        let border_blue = Color::Rgb(0x4e, 0x9a, 0xff);
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(border_blue))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x1e, 0x1e)));
        frame.render_widget(ratatui::widgets::Clear, clipped);
        frame.render_widget(block, clipped);
        if grad {
            paint_gradient_box(frame.buffer_mut(), clipped);
        }
        let sel_bg = if grad {
            rgb_color(POPUP_SEL_BG)
        } else {
            border_blue
        };
        let sel_fg = if grad { Color::White } else { Color::Black };
        let inner = Rect {
            x: clipped.x + 1,
            y: clipped.y + 1,
            width: clipped.width.saturating_sub(2),
            height: clipped.height.saturating_sub(2),
        };
        for (i, (label, action)) in menu.items.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }
            let row = Rect {
                x: inner.x,
                y: inner.y + i as u16,
                width: inner.width,
                height: 1,
            };
            let selected = i == menu.selected;
            let row_style = if selected {
                Style::default().fg(sel_fg).bg(sel_bg)
            } else {
                Style::default().fg(Color::White)
            };
            // Paint the label, then right-align the shortcut hint on the
            // same row so the menu reads like VS Code's: action on the
            // left, accelerator on the right.
            let line = ratatui::text::Line::from(format!(" {label}"));
            frame.render_widget(ratatui::widgets::Paragraph::new(line).style(row_style), row);
            if let Some(sc) = shortcut_for(action) {
                let sc_w = sc.chars().count() as u16;
                if sc_w + 2 <= inner.width {
                    let sc_x = inner.x + inner.width - sc_w - 1;
                    let sc_style = if selected {
                        Style::default().fg(sel_fg).bg(sel_bg)
                    } else {
                        Style::default().fg(Color::Rgb(0x9d, 0xa5, 0xb4))
                    };
                    frame.buffer_mut().set_string(sc_x, row.y, sc, sc_style);
                }
            }
        }
    }

    fn render_commit_dropdown(&mut self, frame: &mut ratatui::Frame) {
        use crate::widgets::source_control::CommitMenuItem;
        // The dropdown belongs to the Source Control view only. Gate on it
        // so a stale `commit_menu_open` flag can never paint the menu over
        // another sidebar view (e.g. Run-Debug) after a view switch.
        if !self.commit_menu_open || self.sidebar_view != SidebarView::SourceControl {
            self.source_control.commit_menu_item_areas.clear();
            return;
        }
        let caret = self.source_control.last_commit_caret_area;
        let main = self.source_control.last_button_area;
        if caret.width == 0 || main.width == 0 {
            return;
        }
        let default_branch = self
            .default_branch_label
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let items: Vec<(CommitMenuItem, String)> = CommitMenuItem::ALL
            .iter()
            .map(|item| (*item, item.label(&default_branch)))
            .collect();
        // Row layout: [pad][icon][gap][label][pad]. The icon occupies one
        // cell at item_x+1; the label starts two cells later at item_x+3.
        const ICON_PAD: u16 = 3;
        let label_w = items
            .iter()
            .map(|(_, l)| l.chars().count() as u16 + ICON_PAD + 1)
            .max()
            .unwrap_or(0);
        let item_w: u16 = label_w.max(caret.width);
        let item_x = (caret.x + caret.width).saturating_sub(item_w).max(main.x);
        let item_y_top = caret.y + caret.height;
        let menu_rect = Rect {
            x: item_x,
            y: item_y_top,
            width: item_w,
            height: items.len() as u16,
        };
        let bg = Color::Rgb(0x25, 0x2b, 0x37);
        let fg = Color::White;
        let style = Style::default().fg(fg).bg(bg);
        frame.render_widget(ratatui::widgets::Clear, menu_rect);
        self.source_control.commit_menu_item_areas.clear();
        for (i, (item, label)) in items.into_iter().enumerate() {
            let row_y = item_y_top + i as u16;
            let row_rect = Rect {
                x: item_x,
                y: row_y,
                width: item_w,
                height: 1,
            };
            for rx in 0..row_rect.width {
                frame.buffer_mut()[(row_rect.x + rx, row_rect.y)]
                    .set_symbol(" ")
                    .set_style(style);
            }
            // Leading codicon, colored per action: push glyphs in blue,
            // diff/compare glyphs in purple, mirroring VS Code's SCM accents.
            let icon_color = match item {
                CommitMenuItem::CommitAndPush => Color::Rgb(0x9a, 0xa4, 0xb2),
                CommitMenuItem::Push => Color::Rgb(0x6c, 0xb6, 0xff),
                CommitMenuItem::ViewStagedDiff
                | CommitMenuItem::ViewPreviousCommitDiff
                | CommitMenuItem::ViewDefaultBranchDiff => Color::Rgb(0xc0, 0xa4, 0xf5),
            };
            frame.buffer_mut().set_string(
                row_rect.x + 1,
                row_rect.y,
                item.icon().to_string(),
                Style::default().fg(icon_color).bg(bg),
            );
            frame
                .buffer_mut()
                .set_string(row_rect.x + 3, row_rect.y, &label, style);
            self.source_control
                .commit_menu_item_areas
                .push((row_rect, item));
        }
    }

    fn dispatch_commit_menu_item(&mut self, item: crate::widgets::source_control::CommitMenuItem) {
        use crate::widgets::source_control::CommitMenuItem;
        match item {
            CommitMenuItem::CommitAndPush => self.commit_and_push_source_control(),
            CommitMenuItem::Push => self.push_source_control(),
            CommitMenuItem::ViewStagedDiff => self.view_staged_diff_source_control(),
            CommitMenuItem::ViewPreviousCommitDiff => {
                self.view_previous_commit_diff_source_control()
            }
            CommitMenuItem::ViewDefaultBranchDiff => self.view_default_branch_diff_source_control(),
        }
    }

    fn render_discard_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(pd) = self.pending_discard.as_ref() else {
            return;
        };
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(50, 96);
        let height: u16 = 8;
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        let warn = Color::Rgb(0xe7, 0x70, 0x70);
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(warn))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x1e, 0x1e)))
            .title(ratatui::text::Span::styled(
                " DISCARD CHANGES? ",
                Style::default()
                    .fg(Color::White)
                    .bg(warn)
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        let warn_text = if pd.untracked {
            "This will permanently delete the file from disk."
        } else {
            "This will overwrite your local changes from HEAD."
        };
        let body = ratatui::text::Text::from(vec![
            ratatui::text::Line::from(ratatui::text::Span::styled(
                warn_text,
                Style::default().fg(Color::White),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(ratatui::text::Span::styled(
                truncate_for_display(&pd.rel_path, inner.width as usize),
                Style::default().fg(Color::Rgb(0xeb, 0xcb, 0x8b)),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(vec![
                ratatui::text::Span::styled(
                    "[Y]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("es, discard   "),
                ratatui::text::Span::styled(
                    "[N]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("o / Esc"),
            ]),
        ]);
        frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
    }

    fn render_terminal_warning(&self, frame: &mut ratatui::Frame) {
        if !self.pending_terminal_warning {
            return;
        }
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(50, 96);
        let height: u16 = 11;
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        let warn = Color::Rgb(0xff, 0xa5, 0x00);
        let accent = Color::Rgb(0x4e, 0x9a, 0xff);
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(warn))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x1e, 0x1e)))
            .title(ratatui::text::Span::styled(
                " UNSUPPORTED TERMINAL ",
                Style::default()
                    .fg(Color::Black)
                    .bg(warn)
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        let current = std::env::var("TERM_PROGRAM")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("TERM").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| String::from("this terminal"));
        let body = ratatui::text::Text::from(vec![
            Line::from(Span::styled(
                "Croft draws its icons and images with inline-image protocols",
                Style::default().fg(Color::White),
            )),
            Line::from(Span::styled(
                format!("that {current} does not support, so the activity bar and"),
                Style::default().fg(Color::White),
            )),
            Line::from(Span::styled(
                "file icons stay blank. Croft still runs; for the full UI use:",
                Style::default().fg(Color::White),
            )),
            Line::from(""),
            Line::from(vec![
                Span::raw("  macOS:  "),
                Span::styled(
                    "iTerm2",
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  or  "),
                Span::styled(
                    "Ghostty",
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw("  Linux:  "),
                Span::styled(
                    "Ghostty",
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "[any key]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" dismiss    "),
                Span::styled(
                    "[D]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("on't show again"),
            ]),
        ]);
        frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
    }

    fn render_local_open_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(url) = &self.pending_local_open else {
            return;
        };
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(50, 96);
        let height: u16 = 8;
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(0xff, 0xa5, 0x00)))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x1e, 0x1e)))
            .title(ratatui::text::Span::styled(
                " OPEN ON LOCAL MAC? ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(0xff, 0xa5, 0x00))
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        let body = ratatui::text::Text::from(vec![
            ratatui::text::Line::from(ratatui::text::Span::styled(
                "This URL will open in YOUR LOCAL MAC's browser via the croft relay.",
                Style::default().fg(Color::White),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(ratatui::text::Span::styled(
                truncate_for_display(url, inner.width as usize),
                Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(vec![
                ratatui::text::Span::styled(
                    "[Y]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("es once   "),
                ratatui::text::Span::styled(
                    "[A]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("lways for this session   "),
                ratatui::text::Span::styled(
                    "[N]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("o / Esc"),
            ]),
        ]);
        frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
    }

    fn render_prompt(&self, frame: &mut ratatui::Frame) {
        let Some(p) = &self.prompt else { return };
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(40, 80);
        let height = if p.error.is_some() { 6 } else { 5 };
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        let grad = self.popup_gradient();
        let cursor_fg = if grad {
            rgb_color(GRAD_TL)
        } else {
            Color::Rgb(0x4e, 0x9a, 0xff)
        };
        let title_bg = if grad {
            rgb_color(POPUP_SEL_BG)
        } else {
            Color::Rgb(0x1e, 0x3a, 0x6e)
        };
        let title = ratatui::text::Span::styled(
            format!(" {} ", p.label),
            Style::default()
                .fg(Color::White)
                .bg(title_bg)
                .add_modifier(Modifier::BOLD),
        );
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x1e, 0x1e)))
            .title(title.clone());
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        // Black theme: overpaint the solid border with the gradient, then
        // re-stamp the title the gradient top edge just covered.
        if grad {
            paint_gradient_box(frame.buffer_mut(), rect);
            frame
                .buffer_mut()
                .set_span(rect.x + 1, rect.y, &title, title.width() as u16);
        }
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        let (top_line, hint_text) = match &p.kind {
            PromptKind::Create(_) => (
                ratatui::text::Line::from(vec![
                    ratatui::text::Span::raw("> "),
                    ratatui::text::Span::styled(
                        p.buffer.as_str(),
                        Style::default().fg(Color::White),
                    ),
                    ratatui::text::Span::styled("█", Style::default().fg(cursor_fg)),
                ]),
                "Enter to create, Esc to cancel",
            ),
            PromptKind::Rename(_) => (
                ratatui::text::Line::from(vec![
                    ratatui::text::Span::raw("> "),
                    ratatui::text::Span::styled(
                        p.buffer.as_str(),
                        Style::default().fg(Color::White),
                    ),
                    ratatui::text::Span::styled("█", Style::default().fg(cursor_fg)),
                ]),
                "Enter to rename, Esc to cancel",
            ),
            PromptKind::RenameSymbol { .. } => (
                ratatui::text::Line::from(vec![
                    ratatui::text::Span::raw("> "),
                    ratatui::text::Span::styled(
                        p.buffer.as_str(),
                        Style::default().fg(Color::White),
                    ),
                    ratatui::text::Span::styled("█", Style::default().fg(cursor_fg)),
                ]),
                "Enter to rename symbol, Esc to cancel",
            ),
            PromptKind::BreakpointCondition { .. } => (
                ratatui::text::Line::from(vec![
                    ratatui::text::Span::raw("> "),
                    ratatui::text::Span::styled(
                        p.buffer.as_str(),
                        Style::default().fg(Color::White),
                    ),
                    ratatui::text::Span::styled("█", Style::default().fg(cursor_fg)),
                ]),
                "Enter to set condition (blank for a plain breakpoint), Esc to cancel",
            ),
        };
        frame.render_widget(
            ratatui::widgets::Paragraph::new(top_line),
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
        );
        let hint = ratatui::widgets::Paragraph::new(ratatui::text::Line::from(hint_text))
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(
            hint,
            Rect {
                x: inner.x,
                y: inner.y + 2,
                width: inner.width,
                height: 1,
            },
        );
        if let Some(err) = &p.error {
            let line = ratatui::widgets::Paragraph::new(ratatui::text::Line::from(format!(
                "Error: {err}"
            )))
            .style(Style::default().fg(Color::Rgb(0xe8, 0x27, 0x4b)));
            frame.render_widget(
                line,
                Rect {
                    x: inner.x,
                    y: inner.y + 3,
                    width: inner.width,
                    height: 1,
                },
            );
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return Ok(());
        }
        self.hover_popup = None;
        self.hover_diagnostic = None;
        self.hover_request_id = None;
        // Modal layer: the unsupported-terminal nudge sits on top of everything
        // at startup. Any key dismisses it for this session; D also persists the
        // dismissal so it never shows again. All keys are swallowed so the user
        // can't accidentally type through into the UI below.
        if self.pending_terminal_warning {
            match key.code {
                KeyCode::Char('d') | KeyCode::Char('D') => self.dismiss_terminal_warning(true),
                _ => self.dismiss_terminal_warning(false),
            }
            return Ok(());
        }
        if self.connect_dialog.is_some() {
            self.handle_connect_dialog_key(key);
            return Ok(());
        }
        if self.shortcuts_modal.is_some() {
            self.handle_shortcuts_modal_key(key);
            return Ok(());
        }
        if self.file_finder.is_some() {
            self.handle_file_finder_key(key);
            return Ok(());
        }
        if self.command_palette.is_some() {
            self.handle_command_palette_key(key);
            return Ok(());
        }
        if self.process_picker.is_some() {
            self.handle_process_picker_key(key);
            return Ok(());
        }
        if self.zoxide_jump.is_some() {
            self.handle_zoxide_jump_key(key);
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(1)) {
            self.open_shortcuts_modal();
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(8)) {
            self.perf.toggle();
            return Ok(());
        }
        // A background update has landed and is waiting: F9 triggers the
        // re-exec into the new binary. Gated on Ready so F9 passes through
        // untouched the rest of the time.
        if matches!(key.code, KeyCode::F(9)) && self.update_status == UpdateStatus::Ready {
            self.pending_reexec = true;
            self.quit = true;
            return Ok(());
        }
        // Debugger chords (VS Code defaults). F5 start/continue, Shift+F5 stop,
        // F9 toggle breakpoint, F10 step over, F11/Shift+F11 step in/out.
        if matches!(key.code, KeyCode::F(5)) {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                self.debug_stop();
            } else {
                self.debug_start_or_continue();
            }
            return Ok(());
        }
        // F6 pauses a running program (VS Code's Pause key) ONLY while a debug
        // session is live; with no session it falls through to its long-standing
        // role as croft's cycle-focus key.
        if matches!(key.code, KeyCode::F(6)) && self.dap_session.is_some() {
            self.debug_pause();
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(9)) {
            self.debug_toggle_breakpoint();
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(10)) {
            self.debug_step("next");
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(11)) {
            let cmd = if key.modifiers.contains(KeyModifiers::SHIFT) {
                "stepOut"
            } else {
                "stepIn"
            };
            self.debug_step(cmd);
            return Ok(());
        }
        if is_file_finder_key(key) {
            self.open_file_finder();
            return Ok(());
        }
        if is_command_palette_key(key) {
            self.open_command_palette();
            return Ok(());
        }
        // Modal layer: prompt eats every key while it's open.
        if self.prompt.is_some() {
            self.handle_prompt_key(key);
            return Ok(());
        }
        // Modal layer: local-browser confirmation eats every key.
        if self.pending_local_open.is_some() {
            self.handle_local_open_confirm_key(key);
            return Ok(());
        }
        // Modal layer: discard-changes confirmation eats every key. Y/Enter
        // discards; N/Esc cancels. Anything else is swallowed so the user
        // can't accidentally type-through into the editor below.
        if self.pending_discard.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.confirm_pending_discard();
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.cancel_pending_discard();
                }
                _ => {}
            }
            return Ok(());
        }
        // Modal layer: open context menu eats keyboard navigation.
        if self.context_menu.is_some() {
            self.handle_menu_key(key);
            return Ok(());
        }
        // Cmd+E toggles native modal (vim) editing. Global (above the focus
        // dispatch) so it works from any pane and even with no file open: the
        // mode is a single app-wide flag that only affects the editor, but it
        // can be pre-armed from the tree or terminal and stays on as you open
        // and switch between files.
        if is_vim_toggle_key(key) {
            self.toggle_vim_mode();
            return Ok(());
        }
        // App-wide shortcuts (priority). Skip Cmd+S when the Source
        // Control pane is focused so the SC handler can use it as the
        // stage shortcut — saving an editor file with no editor focus
        // doesn't make sense anyway, and globally swallowing Cmd+S here
        // is what made the user's first attempt feel inert.
        let sc_focused =
            self.focus == Pane::Tree && self.sidebar_view == SidebarView::SourceControl;
        if is_save_key(key) && !sc_focused {
            self.save();
            return Ok(());
        }
        if is_terminal_toggle_key(key) {
            self.toggle_terminal();
            return Ok(());
        }
        if is_terminal_maximize_key(key) {
            self.toggle_terminal_maximize();
            return Ok(());
        }
        if is_editor_split_key(key) {
            self.split_editor();
            return Ok(());
        }
        if is_focus_group_left_key(key) {
            self.focus_editor_group(true);
            return Ok(());
        }
        if is_focus_group_right_key(key) {
            self.focus_editor_group(false);
            return Ok(());
        }
        if is_terminal_split_key(key) {
            match self.split_terminal() {
                Ok(()) => {
                    self.status = format!("Split terminal: {} active", self.terminals.len());
                }
                Err(e) => {
                    self.status = format!("Split terminal failed: {e}");
                }
            }
            return Ok(());
        }
        if is_terminal_focus_key(key) {
            if !self.show_terminal {
                self.show_terminal = true;
            }
            self.focus_pane(Pane::Terminal);
            return Ok(());
        }
        // Explorer-pane shortcuts get first refusal so Cmd+Shift+F creates
        // a folder when the user is browsing the tree, rather than always
        // jumping to Search. Outside Explorer the global Search-jump wins.
        if self.is_explorer_focused() && self.handle_explorer_shortcut(key) {
            return Ok(());
        }
        if is_search_jump_key(key) {
            // Jumping to global Search supersedes the in-file find: tear the
            // local find bar down first, otherwise its overlay keeps painting
            // over the editor (render_editor_find only gates on editor_find
            // being Some, not on focus) and shadows the global search input.
            self.close_editor_find();
            self.set_sidebar_view(SidebarView::Search);
            return Ok(());
        }
        // Cmd+Shift+S jumps to Source Control from any pane — croft's Source
        // Control gesture. There is no editor Save-As to collide with (Cmd+S
        // alone saves), so it fires even while the editor is focused.
        if is_source_control_jump_key(key) {
            self.set_sidebar_view(SidebarView::SourceControl);
            return Ok(());
        }
        if is_explorer_jump_key(key) {
            self.show_tree = true;
            self.set_sidebar_view(SidebarView::Explorer);
            return Ok(());
        }
        if is_run_debug_jump_key(key) {
            self.show_tree = true;
            self.set_sidebar_view(SidebarView::RunDebug);
            return Ok(());
        }
        if is_remote_jump_key(key) {
            self.show_tree = true;
            self.set_sidebar_view(SidebarView::Remote);
            return Ok(());
        }
        if self.is_remote && is_drop_to_local_key(key) {
            self.drop_to_local = true;
            self.quit = true;
            return Ok(());
        }
        if self.sidebar_view == SidebarView::Search
            && self.focus == Pane::Tree
            && is_search_editing_shortcut(key)
        {
            self.handle_search_key(key);
            return Ok(());
        }
        if is_sidebar_toggle_key(key) {
            let was_visible = self.show_tree;
            self.show_tree = !self.show_tree;
            // Hiding the sidebar while Run-Debug was on screen: arm
            // the same OSC-1337 image-cell evict gate that fires on
            // a sidebar-view change. Without this the bug+play icon
            // ghosts on top of the editor / terminal that fills the
            // space the panel just vacated.
            if was_visible
                && !self.show_tree
                && self.sidebar_view == SidebarView::RunDebug
                && self.overlays.run_debug.was_emitted()
            {
                self.overlays.run_debug.request_clear();
            }
            return Ok(());
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
                self.quit = true;
                return Ok(());
            }
            (KeyCode::F(6), _) => {
                self.cycle_focus();
                return Ok(());
            }
            _ => {}
        }

        match self.focus {
            Pane::Tree => match self.sidebar_view {
                SidebarView::Explorer => self.handle_tree_key(key),
                SidebarView::Search => self.handle_search_key(key),
                SidebarView::SourceControl => self.handle_source_control_key(key),
                SidebarView::Remote => self.handle_remote_key(key),
                SidebarView::RunDebug => self.handle_run_debug_key(key),
            },
            Pane::Editor => {
                self.handle_editor_key(key);
                self.poke_cursor();
            }
            Pane::Terminal => self.handle_terminal_key(key),
        }
        Ok(())
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        use crate::widgets::search::SearchField;
        if is_search_paste_key(key) {
            let text = (self.clipboard_reader)();
            self.paste_clipboard_into_search(text.as_deref());
            return;
        }
        if is_editor_select_all_key(key) {
            self.search.select_all_active();
            return;
        }
        if is_editor_copy_key(key) {
            let text = self.search.active_selection_text();
            if !text.is_empty() {
                copy_to_clipboard(&text);
                self.status = format!("Copied {} chars to clipboard", text.chars().count());
            }
            return;
        }
        if is_editor_cut_key(key) {
            let text = self.search.active_selection_text();
            if !text.is_empty() {
                copy_to_clipboard(&text);
                let n = text.chars().count();
                self.search.backspace();
                self.refresh_search_if_filtering();
                self.status = format!("Cut {n} chars to clipboard");
            }
            return;
        }
        // Tab cycles through the visible inputs (Query → Replace → include →
        // exclude → Query), mirroring VS Code. Shift+Tab is left to fall
        // through to default since BackTab arrives as its own key code.
        if key.code == KeyCode::Tab && key.modifiers.is_empty() {
            self.search.focus_next_field();
            return;
        }
        match key.code {
            KeyCode::Esc => {
                if self.search.active_text().is_empty() {
                    if self.search.field == SearchField::Query {
                        self.set_sidebar_view(SidebarView::Explorer);
                    } else {
                        self.search.focus_field(SearchField::Query);
                    }
                } else {
                    let was_query = self.search.field == SearchField::Query;
                    self.search.active_text_mut_clear();
                    self.search.clear_active_selection();
                    if was_query {
                        self.search.hits.clear();
                    }
                    self.refresh_search_if_filtering();
                }
            }
            KeyCode::Enter => {
                // On the Replace field, Enter runs Replace All across the
                // current results; elsewhere it opens the selected hit.
                if self.search.field == SearchField::Replace {
                    self.run_search_replace_all();
                } else if let Some(hit) = self.search.selected_hit().cloned() {
                    self.open_search_hit(&hit);
                }
            }
            KeyCode::Backspace if self.search.backspace() => {
                self.refresh_search_if_filtering();
            }
            KeyCode::Up => self.search.move_up(),
            KeyCode::Down => self.search.move_down(),
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER)
                {
                    self.search.type_char(c);
                    self.refresh_search_if_filtering();
                } else if key.modifiers.contains(KeyModifiers::SUPER) {
                    self.status = format!("Search: unhandled Cmd+{c}");
                } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.status = format!("Search: unhandled Ctrl+{c}");
                }
            }
            _ => {}
        }
    }

    /// Re-run the search after a field edit, but only when the edited field
    /// actually affects the result set. Editing the Replace text changes
    /// nothing about which files match, so it must not clear / re-scan.
    fn refresh_search_if_filtering(&mut self) {
        if self.search.field != crate::widgets::search::SearchField::Replace {
            self.submit_search_query();
        }
    }

    /// Replace every match in the current results with the Replace text, then
    /// re-run the query so the (now stale) hit list refreshes.
    fn run_search_replace_all(&mut self) {
        if self.search.query.trim().is_empty() {
            self.status = String::from("Replace All: enter a search term first");
            return;
        }
        let n = self.search.replace_all_on_disk();
        if n == 0 {
            self.status = String::from("Replace All: nothing to replace");
        } else {
            self.status = format!("Replace All: replaced {n} occurrence(s)");
        }
        self.submit_search_query();
    }

    fn handle_remote_key(&mut self, key: KeyEvent) {
        let has_mod = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match key.code {
            KeyCode::Up => self.remote.move_up(),
            KeyCode::Down => self.remote.move_down(),
            KeyCode::PageUp => self.remote.scroll_up(10),
            KeyCode::PageDown => self.remote.scroll_down(10),
            KeyCode::Backspace if !self.remote.filter.is_empty() => {
                self.remote.pop_filter_char();
            }
            KeyCode::Char(' ') if has_mod => {}
            KeyCode::Char(c) if !has_mod => {
                self.remote.push_filter_char(c);
            }
            KeyCode::Enter => {
                if let Some(target) = self.remote.selected_target().cloned() {
                    self.request_remote_launch(target.alias, None);
                }
            }
            KeyCode::Esc if !self.remote.clear_filter() => {
                self.set_sidebar_view(SidebarView::Explorer);
            }
            _ => {}
        }
    }

    fn handle_source_control_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Esc) {
            self.set_sidebar_view(SidebarView::Explorer);
            return;
        }
        let cmd_or_ctrl = key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL);
        // Ctrl+Enter: commit & push. We deliberately match ONLY Control
        // here (not Super). iTerm2's default keymap binds Cmd+Enter to
        // "Toggle Fullscreen" and never delivers the chord to the TUI,
        // which made the original Cmd+Enter wiring feel inert; Ctrl is
        // unintercepted across every terminal we target.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Enter) {
            self.commit_and_push_source_control();
            return;
        }
        // Cmd+A: select every entry row in the panel so a follow-up Cmd+S
        // stages them all in one go. Distinct from the editor's Cmd+A
        // (select-all text) which the user kept hitting in the SC pane
        // and getting the wrong scope.
        if cmd_or_ctrl && matches!(key.code, KeyCode::Char('a') | KeyCode::Char('A')) {
            let n = self.source_control.select_all_changes();
            self.status = format!("Selected {n} change(s)");
            return;
        }
        // Cmd+S: stage the selected entry, or every entry in the
        // multi-selection. Mirrors clicking the inline "+" icon but
        // sweeps across the whole selection.
        if cmd_or_ctrl && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S')) {
            self.stage_selected_source_control_entries();
            return;
        }
        if is_clipboard_paste_key(key) {
            if let Some(text) = (self.clipboard_reader)() {
                self.source_control.insert_str(&text);
                self.poke_cursor();
            }
            return;
        }
        match key.code {
            KeyCode::Backspace => {
                self.source_control.backspace();
                self.poke_cursor();
            }
            KeyCode::Delete => {
                self.source_control.delete_char();
                self.poke_cursor();
            }
            KeyCode::Left => {
                self.source_control.move_cursor_left();
                self.poke_cursor();
            }
            KeyCode::Right => {
                self.source_control.move_cursor_right();
                self.poke_cursor();
            }
            KeyCode::Home => {
                self.source_control.home();
                self.poke_cursor();
            }
            KeyCode::End => {
                self.source_control.end();
                self.poke_cursor();
            }
            KeyCode::Up => self.source_control.scroll_up(1),
            KeyCode::Down => self.source_control.scroll_down(1),
            KeyCode::Enter => self.commit_source_control(),
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER) =>
            {
                self.source_control.insert_char(c);
                self.poke_cursor();
            }
            _ => {}
        }
    }

    fn refresh_source_control(&mut self) {
        // Non-blocking: post a Changes request and let the worker reply
        // via `git_response_rx`. The panel paints immediately from
        // whatever entries the last drain installed (typically <400 ms
        // old via the FS-watcher tick); the fresh entries land within
        // one or two frames after the click. Removes the synchronous
        // `git status --porcelain` shell-out that used to stall the UI
        // thread on every Source Control click.
        self.git.request_changes();
    }

    fn handle_run_debug_key(&mut self, key: KeyEvent) {
        // While a session is live the panel is a REPL: text edits the prompt and
        // Enter evaluates it. Esc always leaves the view.
        if self.run_debug.debug_active {
            match key.code {
                KeyCode::Esc => self.set_sidebar_view(SidebarView::Explorer),
                KeyCode::Enter => {
                    self.debug_repl_submit();
                    self.refresh_debug_panel();
                }
                KeyCode::Backspace => {
                    self.run_debug.repl_input.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.run_debug.repl_input.push(c);
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.set_sidebar_view(SidebarView::Explorer),
            KeyCode::Enter => self.run_debug_button_action(),
            _ => {}
        }
    }

    fn refresh_run_debug(&mut self) {
        let path = self.editor.path.clone();
        // Decide the button: scripts and executables are "Run" (terminal);
        // compiled source (no script runner, but a debug adapter) is "Debug"
        // (build + lldb). Files that are neither (e.g. `.git/config`) get no
        // button. run_spec_for already covers executables (the +x bit), so a
        // compiled binary is "Run" — run it directly.
        let (runnable, is_debug) = match path.as_deref() {
            Some(p) => {
                let ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if Self::run_spec_for(p, &self.workspace_root).is_some() {
                    (true, false)
                } else if crate::dap::session::adapter_for_extension(&ext).is_some() {
                    (true, true)
                } else {
                    (false, false)
                }
            }
            None => (false, false),
        };
        self.run_debug.runnable = runnable;
        self.run_debug.run_is_debug = is_debug;
        self.run_debug.set_active_file(path);
        self.run_debug.feedback = None;
        self.run_debug.feedback_is_error = false;
    }

    /// Resolve how to run `file`: which command line to feed the PTY and
    /// which directory the PTY should `chdir` to. Returns `None` for
    /// extensions we don't know how to run.
    ///
    /// For Python files the resolver walks from the file's directory up to
    /// (and including) `workspace_root`, picking the first `.venv/bin/python`,
    /// `venv/bin/python`, or `.env/bin/python` it finds. The cwd is set to
    /// the directory that owns the venv so relative imports, `.env` reads,
    /// and any path-relative work the script does line up with how the user
    /// would run it from their own shell. With no venv anywhere on the
    /// path the resolver falls back to the system `python3` and the file's
    /// own directory.
    fn run_spec_for(file: &Path, workspace_root: &Path) -> Option<RunSpec> {
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let file_str = file.to_string_lossy().into_owned();
        let file_dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        if matches!(ext.as_str(), "py" | "pyi" | "pyw") {
            if let Some((py, venv_dir)) = find_python_venv(&file_dir, workspace_root) {
                return Some(RunSpec {
                    program: py.to_string_lossy().into_owned(),
                    args: vec![file_str],
                    cwd: venv_dir,
                });
            }
            return Some(RunSpec {
                program: String::from("python3"),
                args: vec![file_str],
                cwd: file_dir,
            });
        }
        let runner: &str = match ext.as_str() {
            "js" | "mjs" | "cjs" => "node",
            "ts" | "tsx" => "tsx",
            "rb" => "ruby",
            "lua" => "lua",
            "php" => "php",
            "pl" => "perl",
            "sh" | "bash" => "bash",
            "zsh" => "zsh",
            "fish" => "fish",
            _ => {
                // No known script runner. If the file is itself an executable
                // (a compiled binary, usually extensionless), run it directly.
                if is_executable_file(file) {
                    return Some(RunSpec {
                        program: file_str,
                        args: vec![],
                        cwd: file_dir,
                    });
                }
                return None;
            }
        };
        Some(RunSpec {
            program: String::from(runner),
            args: vec![file_str],
            cwd: file_dir,
        })
    }

    /// Spawn a fresh PTY at the resolved working directory, seed it with the
    /// runner command for the active editor tab's file, and focus the
    /// terminal pane so the user sees output immediately.
    /// Drain the debug session each frame. Returns true if anything changed
    /// (so the frame loop redraws). Mirrors the paused location into the
    /// editor's stop-line, surfaces program output + stop reasons, and tears
    /// the session down when the debuggee exits.
    pub fn poll_dap(&mut self) -> bool {
        use crate::dap::session::{DapEvent, SessionPhase};
        if self.dap_session.is_none() {
            return false;
        }
        let events = self
            .dap_session
            .as_mut()
            .map(|s| s.poll())
            .unwrap_or_default();
        let mut changed = !events.is_empty();
        for ev in events {
            match ev {
                // Route program output to the debug console (each embedded
                // newline becomes its own line), but drop debugpy's own
                // `telemetry` banner (the `ptvsd` / `debugpy` noise).
                DapEvent::Output { category, text }
                    if crate::dap::session::output_is_user_visible(&category) =>
                {
                    for line in text.split('\n') {
                        if !line.is_empty() {
                            self.debug_console_push(line.to_string());
                        }
                    }
                }
                // REPL submissions echo "❯ expr" then the result; watch/hover
                // results are consumed elsewhere.
                DapEvent::Evaluated {
                    context,
                    expression,
                    result,
                } if context == "repl" => {
                    self.debug_console_push(format!("❯ {expression}"));
                    self.debug_console_push(result);
                }
                DapEvent::Stopped { reason, .. } => {
                    self.run_debug.feedback = Some(format!("Paused ({reason})"));
                    self.run_debug.feedback_is_error = false;
                }
                DapEvent::BreakpointsUpdated => {
                    // Mirror the adapter's unverified set into the editor so the
                    // gutter can hollow out inert breakpoints, and warn once if
                    // any failed to bind (the "I set a breakpoint and it just
                    // ran" case).
                    let unverified: std::collections::HashMap<
                        PathBuf,
                        std::collections::BTreeSet<usize>,
                    > = self
                        .dap_session
                        .as_ref()
                        .map(|s| s.unverified_breakpoints.clone().into_iter().collect())
                        .unwrap_or_default();
                    let total: usize = unverified.values().map(|s| s.len()).sum();
                    self.editor.unverified_breakpoints = unverified;
                    if total > 0 {
                        self.status = format!(
                            "{total} breakpoint(s) could not bind (no executable code on that line)"
                        );
                    }
                }
                _ => {}
            }
        }
        let phase = self.dap_session.as_ref().map(|s| s.phase);
        match phase {
            Some(SessionPhase::Stopped) => {
                self.debug_ever_stopped = true;
                let loc = self
                    .dap_session
                    .as_ref()
                    .and_then(|s| s.current_location.clone());
                if let Some(loc) = loc
                    && self.editor.stop_line.as_ref() != Some(&loc)
                {
                    self.editor.stop_line = Some(loc);
                    changed = true;
                }
            }
            Some(SessionPhase::Terminated) => {
                self.editor.stop_line = None;
                self.editor.unverified_breakpoints.clear();
                self.dap_session = None;
                let had_breakpoints = !self.editor.breakpoints.is_empty();
                self.run_debug.feedback =
                    Some(debug_end_message(had_breakpoints, self.debug_ever_stopped).to_string());
                self.run_debug.feedback_is_error = false;
                // Keep `debug_console` so its output stays visible after the run;
                // the panel renders it until the next session starts.
                self.run_debug.session_ended = true;
                changed = true;
            }
            _ => {
                if self.editor.stop_line.take().is_some() {
                    changed = true;
                }
            }
        }
        // Rebuild the Call Stack / Variables panel from the (now-updated) session.
        self.refresh_debug_panel();
        changed
    }

    /// Rebuild the Run & Debug panel's paused-state tree from the session, or
    /// deactivate it when not paused. Cheap; called after each `poll_dap`.
    fn refresh_debug_panel(&mut self) {
        let (active, status, rows) = self.build_debug_view();
        self.run_debug.debug_active = active;
        if active {
            self.run_debug.debug_status = status;
            self.run_debug.debug_rows = rows;
            let max_scroll = self.run_debug.debug_rows.len().saturating_sub(1);
            if self.run_debug.debug_scroll > max_scroll {
                self.run_debug.debug_scroll = max_scroll;
            }
            // Show the console tail (most recent output / REPL lines).
            let n = self.debug_console.len();
            self.run_debug.console_tail = self.debug_console[n.saturating_sub(50)..].to_vec();
        } else {
            self.run_debug.debug_rows.clear();
            self.run_debug.debug_scroll = 0;
            self.run_debug.repl_input.clear();
            // Retain the just-ended session's console output until the next run;
            // otherwise the idle Run panel shows nothing.
            if self.run_debug.session_ended && !self.debug_console.is_empty() {
                let n = self.debug_console.len();
                self.run_debug.console_tail = self.debug_console[n.saturating_sub(50)..].to_vec();
            } else {
                self.run_debug.console_tail.clear();
            }
        }
    }

    /// Build the (active, status, rows) tuple for the debug panel from the
    /// session. `&self` so it can borrow the session and expansion set without
    /// conflicting with the `&mut self` assignment in `refresh_debug_panel`.
    fn build_debug_view(&self) -> (bool, String, Vec<crate::widgets::run_debug::DebugRow>) {
        use crate::dap::session::SessionPhase;
        use crate::widgets::run_debug::{DebugRow, DebugRowKind};
        let Some(session) = self.dap_session.as_ref() else {
            return (false, String::new(), Vec::new());
        };
        // Active for any live session: while Running the tree is empty but the
        // console still shows output; while Stopped the full tree is built.
        if session.phase == SessionPhase::Terminated {
            return (false, String::new(), Vec::new());
        }
        if session.phase != SessionPhase::Stopped {
            let status = self
                .run_debug
                .feedback
                .clone()
                .unwrap_or_else(|| String::from("Running"));
            return (true, status, Vec::new());
        }
        let mut rows = Vec::new();
        rows.push(DebugRow {
            indent: 0,
            kind: DebugRowKind::Header {
                title: String::from("CALL STACK"),
            },
        });
        for f in &session.stack_frames {
            let loc = f
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            rows.push(DebugRow {
                indent: 1,
                kind: DebugRowKind::Frame {
                    id: f.id,
                    selected: Some(f.id) == session.selected_frame,
                    name: f.name.clone(),
                    location: format!("{loc}:{}", f.line),
                },
            });
        }
        rows.push(DebugRow {
            indent: 0,
            kind: DebugRowKind::Header {
                title: String::from("VARIABLES"),
            },
        });
        for scope in &session.scopes {
            rows.push(DebugRow {
                indent: 1,
                kind: DebugRowKind::Scope {
                    name: scope.name.clone(),
                },
            });
            if let Some(vars) = session.variables.get(&scope.variables_ref) {
                push_variable_rows(&mut rows, session, &self.debug_expanded, vars, 2);
            }
        }
        let status = self
            .run_debug
            .feedback
            .clone()
            .unwrap_or_else(|| String::from("Paused"));
        (true, status, rows)
    }

    /// Append a line to the debug console, capping the backlog so a chatty
    /// program can't grow it without bound.
    fn debug_console_push(&mut self, line: String) {
        const CAP: usize = 1000;
        self.debug_console.push(line);
        if self.debug_console.len() > CAP {
            let overflow = self.debug_console.len() - CAP;
            self.debug_console.drain(..overflow);
        }
    }

    /// Submit the Run & Debug panel's REPL input: echo + evaluate it at the
    /// selected frame. No-op on empty input or with no session.
    fn debug_repl_submit(&mut self) {
        let expr = self.run_debug.repl_input.trim().to_string();
        self.run_debug.repl_input.clear();
        if expr.is_empty() {
            return;
        }
        match self.dap_session.as_mut() {
            Some(session) => session.evaluate(&expr, "repl"),
            None => self.debug_console_push(String::from("(no debug session)")),
        }
    }

    /// Hover-to-evaluate: while a debug session is paused, return a popup string
    /// for the identifier at `(line, c)` if it's a loaded local/global, else
    /// None (so the normal LSP hover proceeds).
    fn debug_hover_value(&self, line: usize, c: usize) -> Option<String> {
        use crate::dap::session::SessionPhase;
        let session = self.dap_session.as_ref()?;
        if session.phase != SessionPhase::Stopped {
            return None;
        }
        let word = self.editor.word_string_at(line, c)?;
        let var = session.lookup_local(&word)?;
        let ty = if var.type_name.is_empty() {
            String::new()
        } else {
            format!(": {}", var.type_name)
        };
        Some(format!("{} = {}{ty}", var.name, var.value))
    }

    /// Act on a click of debug-panel row `idx`: select a call-stack frame (load
    /// its scopes/variables) or toggle an expandable variable open/closed.
    fn debug_panel_click(&mut self, idx: usize) {
        use crate::widgets::run_debug::DebugRowKind;
        let Some(row) = self.run_debug.debug_rows.get(idx) else {
            return;
        };
        match row.kind.clone() {
            DebugRowKind::Frame { id, .. } => {
                if let Some(session) = self.dap_session.as_mut() {
                    session.load_frame(id);
                }
                // Collapse expansions: variable refs are frame-scoped.
                self.debug_expanded.clear();
            }
            DebugRowKind::Variable {
                reference,
                expandable: true,
                ..
            } => {
                if self.debug_expanded.contains(&reference) {
                    self.debug_expanded.remove(&reference);
                } else {
                    self.debug_expanded.insert(reference);
                    if let Some(session) = self.dap_session.as_mut() {
                        session.expand_variable(reference);
                    }
                }
            }
            _ => {}
        }
        self.refresh_debug_panel();
    }

    /// The Run/Debug panel button action: debug compiled source (build + lldb),
    /// or run scripts/executables in a terminal — matching the button's verb.
    fn run_debug_button_action(&mut self) {
        if self.run_debug.run_is_debug {
            self.debug_start_or_continue();
        } else {
            self.run_active_file();
        }
    }

    /// Open and focus the Run and Debug view so the call stack, variables,
    /// console, and REPL are visible the instant a session starts (VS Code
    /// reveals its debug view on launch the same way).
    fn reveal_debug_view(&mut self) {
        // A fresh session: drop the previous run's retained console + stop
        // tracking so the "exited without hitting a breakpoint" hint and the
        // console reflect only this run.
        self.debug_console.clear();
        self.debug_ever_stopped = false;
        self.run_debug.session_ended = false;
        self.show_tree = true;
        self.set_sidebar_view(SidebarView::RunDebug);
        self.focus_pane(Pane::Tree);
    }

    /// Surface a debug-start failure on BOTH the styled Run/Debug panel
    /// feedback line AND the always-visible status bar. The panel line only
    /// renders while the Run/Debug sidebar is the active view (`reveal_debug_view`
    /// is reached only on success), so without the status-bar mirror F5 fails
    /// silently whenever the user is looking at any other sidebar — which read
    /// as "F5 does nothing" on a remote with no debug adapter installed.
    fn debug_error(&mut self, msg: String) {
        self.run_debug.feedback = Some(msg.clone());
        self.run_debug.feedback_is_error = true;
        self.status = msg;
    }

    /// F5: start debugging the active file, or resume if already paused.
    pub fn debug_start_or_continue(&mut self) {
        use crate::dap::session::SessionPhase;
        if let Some(session) = self.dap_session.as_mut() {
            if session.phase == SessionPhase::Stopped {
                session.continue_execution();
                self.editor.stop_line = None;
            }
            return;
        }
        self.start_debug_session();
    }

    /// Launch a debug session for the active file, choosing the adapter by file
    /// type. Python (debugpy) is the verified path; other recognised compiled
    /// languages route to lldb-dap.
    fn start_debug_session(&mut self) {
        use crate::dap::session::AdapterKind;
        use std::collections::BTreeMap;
        let Some(path) = self.editor.path.clone() else {
            self.debug_error(String::from("Open a file to debug"));
            return;
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match crate::dap::session::adapter_for_extension(&ext) {
            Some(AdapterKind::Debugpy) => {}
            Some(AdapterKind::LldbDap) => {
                self.start_lldb_debug_session(&path);
                return;
            }
            Some(AdapterKind::JsDebug) => {
                self.start_js_debug_session(&path);
                return;
            }
            None => {
                // An executable binary (e.g. a compiled `target/debug/app`) can
                // be debugged directly by lldb-dap with no build step.
                if is_executable_file(&path) {
                    self.launch_lldb(&path, &path);
                } else {
                    self.debug_error(format!(
                        "No debugger for .{ext} files (Python, JS/TS, Rust, C/C++ are supported)"
                    ));
                }
                return;
            }
        }
        let py = match crate::dap::install::ensure_debug_venv() {
            Ok(py) => py,
            Err(e) => {
                self.debug_error(format!("Debugger setup failed: {e}"));
                return;
            }
        };
        let breakpoints: BTreeMap<PathBuf, Vec<crate::dap::session::SourceBreakpoint>> = self
            .editor
            .breakpoints
            .iter()
            .map(|(p, lines)| (p.clone(), self.editor.source_breakpoints(p, lines)))
            .collect();
        let adapter_args = vec![String::from("-m"), String::from("debugpy.adapter")];
        let py_str = py.to_string_lossy().into_owned();
        match crate::dap::session::DapSession::launch(
            &py_str,
            &adapter_args,
            &self.workspace_root,
            &path,
            &py,
            breakpoints,
            false,
        ) {
            Ok(session) => {
                self.dap_session = Some(session);
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.run_debug.feedback = Some(format!("Debugging {name}"));
                self.run_debug.feedback_is_error = false;
                self.status =
                    format!("Debugging {name} — F5 continue · F10 step over · Shift+F5 stop");
                self.reveal_debug_view();
            }
            Err(e) => {
                self.debug_error(format!("Failed to start debugger: {e}"));
            }
        }
    }

    /// Launch a vscode-js-debug session for a `.js`/`.ts` file run under Node.
    /// Provisions the js-debug server on first use, then drives the shared DAP
    /// session machinery (which transparently spawns the parent + child
    /// connections js-debug requires). TypeScript binds via source maps.
    fn start_js_debug_session(&mut self, path: &Path) {
        use std::collections::BTreeMap;
        let node = match crate::dap::install::node_program() {
            Ok(n) => n,
            Err(e) => {
                self.debug_error(format!("{e}"));
                return;
            }
        };
        let server = match crate::dap::install::ensure_js_debug() {
            Ok(s) => s,
            Err(e) => {
                self.debug_error(format!("Debugger setup failed: {e}"));
                return;
            }
        };
        let breakpoints: BTreeMap<PathBuf, Vec<crate::dap::session::SourceBreakpoint>> = self
            .editor
            .breakpoints
            .iter()
            .map(|(p, lines)| (p.clone(), self.editor.source_breakpoints(p, lines)))
            .collect();
        match crate::dap::session::DapSession::launch_js(
            &node,
            &server,
            &self.workspace_root,
            path,
            breakpoints,
            false,
        ) {
            Ok(session) => {
                self.dap_session = Some(session);
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.run_debug.feedback = Some(format!("Debugging {name}"));
                self.run_debug.feedback_is_error = false;
                self.status =
                    format!("Debugging {name} — F5 continue · F10 step over · Shift+F5 stop");
                self.reveal_debug_view();
            }
            Err(e) => {
                self.debug_error(format!("Failed to start debugger: {e}"));
            }
        }
    }

    /// Launch lldb-dap on a Rust / C / C++ source file: build a debuggable
    /// binary, then drive the same DAP session machinery as Python. The binary
    /// build is the only language-specific step; breakpoints, stepping, call
    /// stack, variables, and the REPL all flow through the shared session.
    ///
    /// Note: lldb-dap requires a permitted `debugserver` (macOS gates this
    /// behind code-signing / `DevToolsSecurity`); where that isn't granted the
    /// process runs without binding breakpoints. croft surfaces the adapter's
    /// own status rather than pretending otherwise.
    fn start_lldb_debug_session(&mut self, path: &Path) {
        if lldb_dap_path().is_none() {
            self.debug_error(lldb_dap_missing_message());
            return;
        }
        // Compiled source: build a debuggable binary first, then debug it.
        let binary = match self.build_debuggable_binary(path) {
            Ok(b) => b,
            Err(e) => {
                self.debug_error(format!("Build for debug failed: {e}"));
                return;
            }
        };
        self.launch_lldb(&binary, path);
    }

    /// Launch lldb-dap on an already-built `binary`, attributing status to
    /// `label_path` (the source for a compiled language, or the binary itself
    /// for a directly-debugged executable). Shared by the compiled-source path
    /// and the executable-binary path so neither duplicates the launch plumbing.
    fn launch_lldb(&mut self, binary: &Path, label_path: &Path) {
        use std::collections::BTreeMap;
        let Some(adapter) = lldb_dap_path() else {
            self.debug_error(lldb_dap_missing_message());
            return;
        };
        let breakpoints: BTreeMap<PathBuf, Vec<crate::dap::session::SourceBreakpoint>> = self
            .editor
            .breakpoints
            .iter()
            .map(|(p, lines)| (p.clone(), self.editor.source_breakpoints(p, lines)))
            .collect();
        let cwd = label_path
            .parent()
            .unwrap_or(&self.workspace_root)
            .to_path_buf();
        match crate::dap::session::DapSession::launch_with(
            &adapter,
            &[],
            &cwd,
            crate::dap::session::lldb_launch_request(binary, false),
            breakpoints,
        ) {
            Ok(session) => {
                self.dap_session = Some(session);
                let name = label_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.run_debug.feedback = Some(format!("Debugging {name} (lldb)"));
                self.run_debug.feedback_is_error = false;
                self.status =
                    format!("Debugging {name} via lldb-dap — F5 continue · Shift+F5 stop");
                self.reveal_debug_view();
            }
            Err(e) => {
                self.debug_error(format!("Failed to start lldb-dap: {e}"));
            }
        }
    }

    /// Compile `path` to a debuggable binary (with debug info, no optimisation)
    /// and return the binary path. C via `cc`, C++ via `c++`, Rust via `rustc`
    /// for a single file. The binary lands next to the source as `<stem>.dbg`.
    fn build_debuggable_binary(&self, path: &Path) -> std::io::Result<PathBuf> {
        use std::process::Command;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let out = path.with_extension("dbg");
        let (program, args): (&str, Vec<String>) = match ext.as_str() {
            "rs" => (
                "rustc",
                vec![
                    String::from("-g"),
                    String::from("-o"),
                    out.to_string_lossy().into_owned(),
                    path.to_string_lossy().into_owned(),
                ],
            ),
            "c" | "h" | "m" => (
                "cc",
                vec![
                    String::from("-g"),
                    String::from("-O0"),
                    path.to_string_lossy().into_owned(),
                    String::from("-o"),
                    out.to_string_lossy().into_owned(),
                ],
            ),
            _ => (
                "c++",
                vec![
                    String::from("-g"),
                    String::from("-O0"),
                    path.to_string_lossy().into_owned(),
                    String::from("-o"),
                    out.to_string_lossy().into_owned(),
                ],
            ),
        };
        let status = Command::new(program).args(&args).status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "{program} exited with {status}"
            )));
        }
        Ok(out)
    }

    /// F9: toggle a breakpoint on the cursor line, pushing it live if a session
    /// is running.
    pub fn debug_toggle_breakpoint(&mut self) {
        self.debug_toggle_breakpoint_line(self.editor.cursor_row + 1);
    }

    /// Toggle a breakpoint on an explicit 1-based `line`, then push the file's
    /// breakpoints to a live session. Shared by F9 (cursor line) and the gutter
    /// right-click menu (clicked line).
    fn debug_toggle_breakpoint_line(&mut self, line: usize) {
        match self.editor.toggle_breakpoint_line(line) {
            Some((_, line, now_set)) => {
                self.status = if now_set {
                    format!("Breakpoint set at line {line}")
                } else {
                    format!("Breakpoint removed from line {line}")
                };
            }
            None => {
                self.status = String::from("Open a file to set a breakpoint");
                return;
            }
        }
        if let Some(path) = self.editor.path.clone() {
            let specs = self
                .editor
                .breakpoints
                .get(&path)
                .map(|lines| self.editor.source_breakpoints(&path, lines))
                .unwrap_or_default();
            if let Some(session) = self.dap_session.as_mut() {
                session.update_breakpoints(&path, &specs);
            }
        }
    }

    /// Open the breakpoint-condition editor for the cursor line, pre-filled with
    /// any existing condition.
    pub fn debug_edit_condition(&mut self) {
        self.debug_edit_condition_line(self.editor.cursor_row + 1);
    }

    /// Open the breakpoint-condition editor for an explicit 1-based `line` as a
    /// centered popup (`PromptKind::BreakpointCondition`), pre-filled with any
    /// existing condition. Shared by the command-palette entry (cursor line)
    /// and the gutter right-click menu (clicked line). The popup, not the
    /// status line, captures the input; commit lives in [`App::commit_prompt`].
    fn debug_edit_condition_line(&mut self, line: usize) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Open a file to set a conditional breakpoint");
            return;
        };
        let existing = self
            .editor
            .breakpoint_conditions
            .get(&path)
            .and_then(|c| c.get(&line))
            .cloned()
            .unwrap_or_default();
        let label = if existing.is_empty() {
            format!("Add Conditional Breakpoint · line {line}")
        } else {
            format!("Edit Condition · line {line}")
        };
        let target_dir = self.tree.root.clone();
        self.prompt = Some(Prompt {
            label,
            buffer: existing,
            kind: PromptKind::BreakpointCondition { path, line },
            target_dir,
            error: None,
        });
    }

    /// Commit the breakpoint-condition popup: ensure a breakpoint exists on the
    /// line, attach the typed expression (or clear it when blank), and push the
    /// file's breakpoints to a live session.
    fn commit_breakpoint_condition(&mut self, path: PathBuf, line: usize, expr: &str) {
        self.editor
            .breakpoints
            .entry(path.clone())
            .or_default()
            .insert(line);
        let cond = expr.trim();
        if cond.is_empty() {
            if let Some(c) = self.editor.breakpoint_conditions.get_mut(&path) {
                c.remove(&line);
            }
            self.status = format!("Plain breakpoint at line {line}");
        } else {
            self.editor
                .breakpoint_conditions
                .entry(path.clone())
                .or_default()
                .insert(line, cond.to_string());
            self.status = format!("Conditional breakpoint at line {line}");
        }
        let specs = self
            .editor
            .breakpoints
            .get(&path)
            .map(|l| self.editor.source_breakpoints(&path, l))
            .unwrap_or_default();
        if let Some(session) = self.dap_session.as_mut() {
            session.update_breakpoints(&path, &specs);
        }
    }

    /// F10/F11/Shift+F11: step the paused thread (`next` / `stepIn` / `stepOut`).
    pub fn debug_step(&mut self, command: &str) {
        if let Some(session) = self.dap_session.as_mut() {
            session.step(command);
            self.editor.stop_line = None;
        }
    }

    /// Toggle "break on raised exceptions" for the active session. `uncaught` is
    /// always kept on so unhandled exceptions still pause. No-op without a
    /// session.
    pub fn debug_toggle_raised_exceptions(&mut self) {
        let Some(session) = self.dap_session.as_mut() else {
            self.status = String::from("Start debugging first to set exception breakpoints");
            return;
        };
        let raised = !session.has_exception_filter("raised");
        let mut filters = vec![String::from("uncaught")];
        if raised {
            filters.push(String::from("raised"));
        }
        session.set_exception_filters(filters);
        self.status = if raised {
            String::from("Breaking on raised exceptions (and uncaught)")
        } else {
            String::from("Breaking on uncaught exceptions only")
        };
    }

    /// F6: interrupt a running program so it stops at the current line.
    pub fn debug_pause(&mut self) {
        match self.dap_session.as_mut() {
            Some(session) => {
                session.pause();
                self.status = String::from("Pausing…");
            }
            None => self.status = String::from("No debug session to pause"),
        }
    }

    /// Restart: tear down the current session and relaunch the active file with
    /// the same breakpoints.
    pub fn debug_restart(&mut self) {
        if self.dap_session.is_some() {
            self.debug_stop();
        }
        self.start_debug_session();
    }

    /// Shift+F5: stop debugging and tear the session down.
    pub fn debug_stop(&mut self) {
        if let Some(session) = self.dap_session.as_mut() {
            session.disconnect();
        }
        self.dap_session = None;
        self.editor.stop_line = None;
        self.editor.unverified_breakpoints.clear();
        self.status = String::from("Debug session stopped");
        // Clear the paused-state feedback ("Paused (breakpoint)") so the
        // empty-state panel doesn't keep showing it under the Run button.
        self.run_debug.feedback = None;
        self.run_debug.feedback_is_error = false;
        // Refresh now: `poll_dap` early-returns once the session is gone, so it
        // would never deactivate the panel on its own — without this the Run &
        // Debug panel keeps showing the stale "Paused" tree until the next event.
        self.refresh_debug_panel();
    }

    pub fn run_active_file(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            self.run_debug.feedback = Some(String::from("Open a file first"));
            self.run_debug.feedback_is_error = true;
            return;
        };
        let Some(spec) = Self::run_spec_for(&path, &self.workspace_root) else {
            self.run_debug.feedback = Some(format!(
                "Don't know how to run {}",
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            ));
            self.run_debug.feedback_is_error = true;
            return;
        };
        match PtyTerminal::new_running(&spec.program, &spec.args, &spec.cwd) {
            Ok(term) => {
                let target = self.active_terminal + 1;
                self.terminals.insert(target, term);
                self.active_terminal = target;
                if !self.show_terminal {
                    self.show_terminal = true;
                }
                self.focus_pane(Pane::Terminal);
                self.run_debug.feedback = Some(format!(
                    "Running {}",
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string())
                ));
                self.run_debug.feedback_is_error = false;
            }
            Err(e) => {
                self.run_debug.feedback = Some(format!("Failed to spawn terminal: {e}"));
                self.run_debug.feedback_is_error = true;
            }
        }
    }

    /// Open the change at `entry_idx` in the editor pane: a side-by-side
    /// diff against HEAD for Modified / StagedModified entries (the
    /// VS Code Source Control click behaviour), or a plain file open
    /// for everything else where the working tree IS the only version
    /// (Untracked, StagedAdded) or HEAD doesn't carry the file
    /// (Deleted). Selection state on the panel updates so the user can
    /// see which entry the editor is bound to.
    pub fn open_source_control_entry(&mut self, entry_idx: usize) {
        use crate::git::ChangeKind;
        let Some(entry) = self.source_control.entries.get(entry_idx).cloned() else {
            return;
        };
        self.source_control.selected_change = Some(entry_idx);
        let abs = self.tree.root.join(&entry.path);
        let opened = match entry.kind {
            ChangeKind::Modified | ChangeKind::StagedModified => {
                match crate::git::read_file_at_head(&self.tree.root, &entry.path) {
                    Ok(head_text) => {
                        let label = std::path::PathBuf::from(format!("{} (HEAD)", entry.path));
                        match self
                            .editor
                            .open_head_diff_with_text(label, &head_text, &abs)
                        {
                            Ok(()) => true,
                            Err(e) => {
                                self.status = format!("Diff failed: {e}");
                                false
                            }
                        }
                    }
                    Err(e) => {
                        self.status = format!("git show HEAD failed: {e}");
                        // Fall back to plain open so the user still sees
                        // something rather than nothing.
                        if abs.is_file() {
                            self.editor.open_pinned(&abs).is_ok()
                        } else {
                            false
                        }
                    }
                }
            }
            ChangeKind::Deleted | ChangeKind::StagedDeleted => {
                // No working-tree file to diff against — show the HEAD
                // blob as a one-sided unified diff so every removed
                // line is visible in red, matching `git diff` output
                // for a deletion.
                match crate::git::read_file_at_head(&self.tree.root, &entry.path) {
                    Ok(head_text) => {
                        let label = std::path::PathBuf::from(&entry.path);
                        match self.editor.open_deleted_diff_with_text(&label, &head_text) {
                            Ok(()) => true,
                            Err(e) => {
                                self.status = format!("Open deletion view failed: {e}");
                                false
                            }
                        }
                    }
                    Err(e) => {
                        self.status = format!("git show HEAD failed: {e}");
                        false
                    }
                }
            }
            _ => {
                if abs.is_file() {
                    match self.editor.open_pinned(&abs) {
                        Ok(()) => true,
                        Err(e) => {
                            self.status = format!("Open failed: {e}");
                            false
                        }
                    }
                } else {
                    false
                }
            }
        };
        if opened {
            self.focus_pane(Pane::Editor);
            self.sync_open_file_poll_mtime();
        }
    }

    fn initialize_repository(&mut self) {
        match std::process::Command::new("git")
            .arg("-C")
            .arg(&self.tree.root)
            .arg("init")
            .output()
        {
            Ok(out) if out.status.success() => {
                self.status = format!(
                    "Initialized empty Git repository in {}",
                    self.tree.root.display()
                );
                self.git.bypass_debounce();
                self.refresh_git_status_debounced();
                self.refresh_source_control();
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                self.status = format!("git init failed: {err}");
            }
            Err(e) => {
                self.status = format!("git init failed: {e}");
            }
        }
    }

    pub fn stage_selected_source_control_entries(&mut self) {
        let indices: Vec<usize> = if self.source_control.multi_selection.is_empty() {
            self.source_control
                .selected_change
                .iter()
                .copied()
                .collect()
        } else {
            self.source_control
                .multi_selection
                .iter()
                .copied()
                .collect()
        };
        if indices.is_empty() {
            self.status = String::from("No change selected to stage");
            return;
        }
        let paths: Vec<String> = indices
            .iter()
            .filter_map(|i| self.source_control.entries.get(*i))
            .filter(|e| e.kind.section() != crate::git::ChangeSection::Staged)
            .map(|e| e.path.clone())
            .collect();
        if paths.is_empty() {
            self.status = String::from("Selection is already staged");
            return;
        }
        match crate::git::stage_paths(&self.tree.root, &paths) {
            Ok(()) => {
                self.status = if paths.len() == 1 {
                    format!("Staged {}", paths[0])
                } else {
                    format!("Staged {} files", paths.len())
                };
            }
            Err(err) => {
                self.status = format!("Stage failed: {err}");
                self.source_control.commit_feedback = Some(err);
                self.source_control.commit_feedback_is_error = true;
            }
        }
        self.git.bypass_debounce();
        self.refresh_git_status_debounced();
        self.refresh_source_control();
    }

    /// Push the current branch without committing. Sibling of
    /// `commit_and_push_source_control`, called from the commit dropdown's
    /// "Push" item so users can ship already-committed work without
    /// having to type a no-op commit message first.
    pub fn push_source_control(&mut self) {
        match crate::git::push_current_branch(&self.tree.root) {
            Ok(summary) => {
                self.source_control.commit_feedback = Some(if summary.is_empty() {
                    "pushed".to_string()
                } else {
                    format!("pushed: {summary}")
                });
                self.source_control.commit_feedback_is_error = false;
                self.status = format!("Pushed: {summary}");
            }
            Err(err) => {
                self.source_control.commit_feedback = Some(format!("push failed: {err}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Push failed: {err}");
            }
        }
        self.git.bypass_debounce();
        self.refresh_git_status_debounced();
    }

    /// Open the staged-changes diff (`git diff --staged`) in a read-only
    /// editor tab. Mirrors VS Code's "View Staged Changes" — users hit
    /// the dropdown, click View Staged Changes, and the editor tab pops up
    /// with every `+`/`-` line of the staged hunks highlighted.
    pub fn view_staged_diff_source_control(&mut self) {
        let raw = match crate::git::diff_staged(&self.tree.root) {
            Ok(r) => r,
            Err(err) => {
                self.source_control.commit_feedback = Some(format!("diff failed: {err}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("View Staged Changes failed: {err}");
                return;
            }
        };
        let label = std::path::PathBuf::from("git diff --staged");
        if let Err(err) = self.editor.open_git_diff_side_by_side(&label, &raw) {
            self.source_control.commit_feedback = Some(format!("open failed: {err}"));
            self.source_control.commit_feedback_is_error = true;
            self.status = format!("Could not open staged diff: {err}");
            return;
        }
        self.source_control.commit_feedback = None;
        self.status = String::from("Showing git diff --staged");
        self.focus_pane(Pane::Editor);
    }

    /// Open `git diff <default-branch>` in a read-only editor tab. The
    /// default branch is resolved via `git::default_branch` (origin/HEAD
    /// first, then common local names). If resolution fails the error
    /// surfaces in the commit-feedback line so the user can fix the
    /// upstream-HEAD config instead of getting a silent no-op.
    pub fn view_default_branch_diff_source_control(&mut self) {
        let branch = match crate::git::default_branch(&self.tree.root) {
            Ok(b) => b,
            Err(err) => {
                self.source_control.commit_feedback = Some(err.clone());
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Default branch: {err}");
                return;
            }
        };
        let raw = match crate::git::diff_against_branch(&self.tree.root, &branch) {
            Ok(r) => r,
            Err(err) => {
                self.source_control.commit_feedback = Some(format!("diff failed: {err}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("View Changes vs {branch} failed: {err}");
                return;
            }
        };
        let label = std::path::PathBuf::from(format!("git diff {branch}"));
        if let Err(err) = self.editor.open_git_diff_side_by_side(&label, &raw) {
            self.source_control.commit_feedback = Some(format!("open failed: {err}"));
            self.source_control.commit_feedback_is_error = true;
            self.status = format!("Could not open diff vs {branch}: {err}");
            return;
        }
        self.default_branch_label = Some(branch.clone());
        self.source_control.commit_feedback = None;
        self.status = format!("Showing git diff {branch}");
        self.focus_pane(Pane::Editor);
    }

    /// Open `git diff HEAD~1` (working tree versus the commit before HEAD)
    /// in a read-only editor tab. Mirrors "View Changes vs <default>" but
    /// anchored on the previous commit, so the user can review everything
    /// that changed since the last commit. A single-commit repo has no
    /// `HEAD~1`; that error surfaces in the commit-feedback line rather
    /// than as a silent no-op.
    pub fn view_previous_commit_diff_source_control(&mut self) {
        let raw = match crate::git::diff_previous_commit(&self.tree.root) {
            Ok(r) => r,
            Err(err) => {
                self.source_control.commit_feedback = Some(format!("diff failed: {err}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("View Changes vs previous failed: {err}");
                return;
            }
        };
        let label = std::path::PathBuf::from("git diff HEAD~1");
        if let Err(err) = self.editor.open_git_diff_side_by_side(&label, &raw) {
            self.source_control.commit_feedback = Some(format!("open failed: {err}"));
            self.source_control.commit_feedback_is_error = true;
            self.status = format!("Could not open diff vs previous: {err}");
            return;
        }
        self.source_control.commit_feedback = None;
        self.status = String::from("Showing git diff HEAD~1");
        self.focus_pane(Pane::Editor);
    }

    pub fn commit_and_push_source_control(&mut self) {
        let message = self.source_control.message.trim().to_string();
        if message.is_empty() {
            self.source_control.commit_feedback = Some(String::from("Empty commit message"));
            self.source_control.commit_feedback_is_error = true;
            return;
        }
        let commit_summary = match crate::git::commit_all_tracked(&self.tree.root, &message) {
            Ok(s) => s,
            Err(err) => {
                self.source_control.commit_feedback = Some(err.clone());
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Commit failed: {err}");
                return;
            }
        };
        self.source_control.clear_message();
        self.status = format!("Committed: {commit_summary}");
        match crate::git::push_current_branch(&self.tree.root) {
            Ok(push_summary) => {
                let combined = if push_summary.is_empty() {
                    commit_summary
                } else {
                    format!("{commit_summary} | pushed: {push_summary}")
                };
                self.source_control.commit_feedback = Some(combined.clone());
                self.source_control.commit_feedback_is_error = false;
                self.status = format!("Committed & pushed: {combined}");
            }
            Err(err) => {
                self.source_control.commit_feedback =
                    Some(format!("commit ok; push failed: {err}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Commit ok; push failed: {err}");
            }
        }
        self.git.bypass_debounce();
        self.refresh_git_status_debounced();
        self.refresh_source_control();
    }

    pub fn stage_source_control_entry(&mut self, entry_idx: usize) {
        let Some(entry) = self.source_control.entries.get(entry_idx).cloned() else {
            return;
        };
        match crate::git::stage_path(&self.tree.root, &entry.path) {
            Ok(()) => {
                self.status = format!("Staged {}", entry.path);
                self.git.bypass_debounce();
                self.refresh_git_status_debounced();
                self.refresh_source_control();
            }
            Err(err) => {
                self.status = format!("Stage failed: {err}");
                self.source_control.commit_feedback = Some(err);
                self.source_control.commit_feedback_is_error = true;
            }
        }
    }

    pub fn request_discard_source_control_entry(&mut self, entry_idx: usize) {
        use crate::git::ChangeKind;
        let Some(entry) = self.source_control.entries.get(entry_idx).cloned() else {
            return;
        };
        let untracked = matches!(entry.kind, ChangeKind::Untracked);
        self.pending_discard = Some(PendingDiscard {
            rel_path: entry.path,
            untracked,
        });
    }

    pub fn confirm_pending_discard(&mut self) {
        let Some(pd) = self.pending_discard.take() else {
            return;
        };
        match crate::git::discard_path(&self.tree.root, &pd.rel_path, pd.untracked) {
            Ok(()) => {
                self.status = format!("Discarded {}", pd.rel_path);
                self.git.bypass_debounce();
                self.refresh_git_status_debounced();
                self.refresh_source_control();
            }
            Err(err) => {
                self.status = format!("Discard failed: {err}");
                self.source_control.commit_feedback = Some(err);
                self.source_control.commit_feedback_is_error = true;
            }
        }
    }

    pub fn cancel_pending_discard(&mut self) {
        self.pending_discard = None;
    }

    /// Decide whether to show the unsupported-terminal nudge. Call AFTER
    /// `init_graphics` has resolved `inline_protocol` (including the sixel
    /// promotion), so a sixel host is correctly treated as supported. No-op
    /// when the user has already silenced the nudge in their prefs.
    fn arm_terminal_warning(&mut self) {
        if crate::prefs::Prefs::load_or_default().suppress_terminal_warning {
            return;
        }
        self.pending_terminal_warning = crate::iterm2_inline::terminal_warrants_switch_warning(
            self.inline_protocol,
            crate::iterm2_inline::detect_termux(),
        );
    }

    /// Dismiss the unsupported-terminal nudge. `dont_show_again` persists the
    /// dismissal so it never reappears; otherwise it returns next launch.
    fn dismiss_terminal_warning(&mut self, dont_show_again: bool) {
        self.pending_terminal_warning = false;
        if dont_show_again {
            let _ = crate::prefs::save_suppress_terminal_warning(true);
        }
    }

    fn commit_source_control(&mut self) {
        let message = self.source_control.message.trim().to_string();
        if message.is_empty() {
            self.source_control.commit_feedback = Some(String::from("Empty commit message"));
            self.source_control.commit_feedback_is_error = true;
            return;
        }
        match crate::git::commit_all_tracked(&self.tree.root, &message) {
            Ok(summary) => {
                self.source_control.clear_message();
                self.source_control.commit_feedback = Some(summary.clone());
                self.source_control.commit_feedback_is_error = false;
                self.status = format!("Committed: {summary}");
                self.git.bypass_debounce();
                self.refresh_git_status_debounced();
                self.refresh_source_control();
            }
            Err(err) => {
                self.source_control.commit_feedback = Some(err.clone());
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Commit failed: {err}");
            }
        }
    }

    fn request_remote_launch(&mut self, host: String, path: Option<String>) {
        self.status = format!("Connecting to {host}");
        match crate::remote_connect::SshAuth::start(&host) {
            Ok(auth) => {
                self.connect_auth = Some(auth);
                self.connect_dialog = Some(crate::widgets::connect_dialog::ConnectDialog::new(
                    host.clone(),
                ));
                self.pending_remote_launch_path = path;
                self.pending_remote_launch_host = Some(host);
                self.overlays.welcome.request_clear();
            }
            Err(e) => {
                self.status = format!("Could not start ssh: {e}");
            }
        }
    }

    pub fn poll_connect_dialog(&mut self) -> bool {
        let Some(auth) = self.connect_auth.as_mut() else {
            return false;
        };
        let events = auth.drain_events();
        if events.is_empty() {
            return false;
        }
        let Some(dialog) = self.connect_dialog.as_mut() else {
            return false;
        };
        let mut authenticated = false;
        let mut failed: Option<String> = None;
        for ev in events {
            match ev {
                crate::remote_connect::SshAuthEvent::Log(chunk) => {
                    dialog.push_log(&chunk);
                }
                crate::remote_connect::SshAuthEvent::Prompt { kind, line } => {
                    dialog.set_prompt(kind, line);
                }
                crate::remote_connect::SshAuthEvent::Authenticated => {
                    dialog.set_installing();
                    authenticated = true;
                }
                crate::remote_connect::SshAuthEvent::Failed(detail) => {
                    failed = Some(detail);
                }
            }
        }
        if let Some(detail) = failed {
            dialog.set_failed(detail);
            self.tear_down_connect_auth();
        }
        if authenticated
            && let (Some(auth), Some(host)) = (
                self.connect_auth.take(),
                self.pending_remote_launch_host.take(),
            )
        {
            let adopted = auth.hand_off();
            let path = self.pending_remote_launch_path.take();
            self.install_session = Some(crate::install_session::InstallSession::start(
                adopted,
                host.clone(),
                path,
            ));
            self.status = format!("Preparing remote croft on {host}…");
        }
        true
    }

    pub fn poll_install_session(&mut self) -> bool {
        let Some(session) = self.install_session.as_mut() else {
            return false;
        };
        let events = session.drain_events();
        let mut changed = !events.is_empty();
        let mut can_launch = false;
        let mut done = false;
        let mut failure: Option<String> = None;
        for ev in events {
            match ev {
                crate::install_session::InstallEvent::Log(line) => {
                    if let Some(dialog) = self.connect_dialog.as_mut() {
                        dialog.push_log(&line);
                    }
                }
                crate::install_session::InstallEvent::CanLaunch => can_launch = true,
                crate::install_session::InstallEvent::Done(Ok(())) => done = true,
                crate::install_session::InstallEvent::Done(Err(detail)) => {
                    failure = Some(detail);
                }
            }
        }
        // Once we've handed the terminal to the remote croft, the install
        // keeps running in a detached thread and signals completion to that
        // remote process via the install-stamp it watches; nothing here.
        if self.remote_launch.is_some() {
            return changed;
        }
        // A croft is already on the remote: drop the user into it now and
        // let the (re)install finish in the background. The running remote
        // croft re-execs into the new binary when the stamp advances.
        if can_launch {
            if let Some(session) = self.install_session.as_mut()
                && let Some(adopted) = session.take_adopted()
            {
                let host = session.host.clone();
                let path = session.path.clone();
                self.remote_launch = Some(RemoteLaunch {
                    host: host.clone(),
                    path,
                    adopted: Some(adopted),
                });
                self.status = format!("Launching croft on {host}…");
                self.connect_dialog = None;
                self.quit = true;
            }
            return true;
        }
        if let Some(detail) = failure {
            if let Some(dialog) = self.connect_dialog.as_mut() {
                dialog.set_failed(detail.clone());
            }
            self.install_session = None;
            self.status = format!("Remote install failed: {detail}");
            changed = true;
        } else if done {
            if let Some(mut session) = self.install_session.take()
                && let Some(adopted) = session.take_adopted()
            {
                let host = session.host.clone();
                let path = session.path.clone();
                self.remote_launch = Some(RemoteLaunch {
                    host: host.clone(),
                    path,
                    adopted: Some(adopted),
                });
                self.status = format!("Launching croft on {host}…");
                self.connect_dialog = None;
                self.quit = true;
            }
            changed = true;
        }
        changed
    }

    /// Start the background self-update watcher on a remote-launched croft.
    /// No-op unless `CROFT_REMOTE_AUTOUPDATE` is set (it is, by the remote
    /// launch command) and an install-stamp exists to compare against.
    fn start_update_watch_if_remote(&mut self) {
        if std::env::var_os("CROFT_REMOTE_AUTOUPDATE").is_none() {
            return;
        }
        let cache_dir = croft_cache_dir();
        let launch_stamp = std::fs::read_to_string(cache_dir.join("install-stamp"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        self.update_watch = Some(crate::update_watch::UpdateWatch::start(
            cache_dir,
            launch_stamp,
        ));
    }

    pub fn poll_update_watch(&mut self) -> bool {
        let Some(watch) = self.update_watch.as_ref() else {
            return false;
        };
        let mut changed = false;
        for ev in watch.drain() {
            changed = true;
            match ev {
                crate::update_watch::UpdateEvent::InProgress => {
                    self.update_status = UpdateStatus::InProgress;
                    // The dedicated red, spinning status-bar span renders the
                    // message now, so the generic status line stays clear.
                    self.update_spinner_start = Some(std::time::Instant::now());
                    self.status.clear();
                }
                crate::update_watch::UpdateEvent::Failed => {
                    self.update_status = UpdateStatus::Idle;
                    self.update_spinner_start = None;
                    self.status =
                        String::from("Background croft update failed; staying on current version");
                }
                crate::update_watch::UpdateEvent::Ready => {
                    self.update_spinner_start = None;
                    // Do NOT yank the user mid-work: surface a persistent
                    // "Update ready - F9 to relaunch" prompt in the status
                    // bar and let them pick the moment. The re-exec only
                    // fires when they press F9 (handle_key).
                    self.update_status = UpdateStatus::Ready;
                    self.status = String::from(
                        "Update ready - press F9 to relaunch croft (terminals will reset)",
                    );
                }
            }
        }
        changed
    }

    /// Current spinner glyph for an in-progress update (frame 0 when idle).
    fn update_spinner_glyph(&self) -> &'static str {
        match self.update_spinner_start {
            Some(t) => update_spinner_frame(t.elapsed().as_millis()),
            None => UPDATE_SPINNER_FRAMES[0],
        }
    }

    /// Monotonic spinner phase the main loop watches to force a redraw each
    /// time the glyph should advance. Stays `0` whenever no update is running,
    /// so it never wakes the renderer outside an active update.
    fn update_spinner_phase(&self) -> u128 {
        match self.update_spinner_start {
            Some(t) => t.elapsed().as_millis() / UPDATE_SPINNER_FRAME_MS,
            None => 0,
        }
    }

    pub fn capture_session_state(&self) -> crate::session_state::SessionState {
        let mut tabs = Vec::new();
        let mut active_tab = 0;
        for (idx, ed) in self.editor.editors.iter().enumerate() {
            // Only plain text file tabs survive a re-exec; diff / sheet /
            // image tabs are derived views with no path to reopen from.
            if ed.diff.is_some() || ed.sheet.is_some() || ed.image.is_some() {
                continue;
            }
            let Some(path) = ed.path.clone() else {
                continue;
            };
            if idx == self.editor.active_index() {
                active_tab = tabs.len();
            }
            let unsaved_text = if ed.dirty {
                Some(ed.lines.join("\n"))
            } else {
                None
            };
            tabs.push(crate::session_state::OpenTabState {
                path,
                cursor_row: ed.cursor_row,
                cursor_col: ed.cursor_col,
                scroll: ed.scroll,
                scroll_col: ed.scroll_col,
                dirty: ed.dirty,
                unsaved_text,
            });
        }
        crate::session_state::SessionState {
            workspace_root: self.workspace_root.clone(),
            tabs,
            active_tab,
            sidebar_view: sidebar_view_label(self.sidebar_view).to_string(),
            sidebar_width: self.sidebar_width,
            terminal_height: self.terminal_height,
            focus_editor: self.focus == Pane::Editor,
        }
    }

    fn apply_session_state(&mut self, state: &crate::session_state::SessionState) {
        for tab in &state.tabs {
            if self.editor.open_pinned(&tab.path).is_err() {
                continue;
            }
            let Some(ed) = self
                .editor
                .editors
                .iter_mut()
                .find(|e| e.path.as_deref() == Some(tab.path.as_path()))
            else {
                continue;
            };
            if tab.dirty
                && let Some(text) = &tab.unsaved_text
            {
                ed.lines = text.split('\n').map(str::to_string).collect();
                if ed.lines.is_empty() {
                    ed.lines.push(String::new());
                }
                ed.dirty = true;
            }
            let max_row = ed.lines.len().saturating_sub(1);
            ed.cursor_row = tab.cursor_row.min(max_row);
            let max_col = ed.lines.get(ed.cursor_row).map_or(0, |l| l.chars().count());
            ed.cursor_col = tab.cursor_col.min(max_col);
            ed.scroll = tab.scroll;
            ed.scroll_col = tab.scroll_col;
        }
        if state.active_tab < self.editor.tab_count() {
            self.editor.select(state.active_tab);
        }
        if let Some(view) = sidebar_view_from_label(&state.sidebar_view) {
            self.set_sidebar_view(view);
        }
        self.sidebar_width = state.sidebar_width;
        self.terminal_height = state.terminal_height;
        self.focus = if state.focus_editor {
            Pane::Editor
        } else {
            Pane::Tree
        };
    }

    fn tear_down_connect_auth(&mut self) {
        if let Some(mut auth) = self.connect_auth.take() {
            auth.cancel();
        }
        self.install_session = None;
        self.pending_remote_launch_host = None;
        self.pending_remote_launch_path = None;
    }

    pub fn handle_connect_dialog_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.connect_dialog.as_mut() else {
            return;
        };
        if dialog.phase.is_terminal() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ')) {
                self.connect_dialog = None;
                self.tear_down_connect_auth();
            }
            return;
        }
        let has_mod = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match key.code {
            KeyCode::Esc => {
                self.connect_dialog = None;
                self.tear_down_connect_auth();
                self.status = String::from("Connect cancelled");
            }
            KeyCode::Enter => {
                if !dialog.phase.awaits_input() {
                    return;
                }
                let payload = dialog.input_for_submit();
                dialog.submitted = true;
                dialog.status_line = format!("Verifying credentials with {}…", dialog.host);
                dialog.clear_input();
                if let Some(auth) = self.connect_auth.as_mut() {
                    auth.respond_password(&payload);
                }
            }
            KeyCode::Backspace => dialog.pop_input_char(),
            KeyCode::Char('l') | KeyCode::Char('L') if has_mod || !dialog.phase.awaits_input() => {
                dialog.toggle_logs();
            }
            KeyCode::Char(c) if !has_mod => dialog.push_input_char(c),
            _ => {}
        }
    }

    pub fn handle_connect_dialog_click(&mut self, col: u16, row: u16) -> bool {
        let Some(dialog) = self.connect_dialog.as_mut() else {
            return false;
        };
        if rect_contains(dialog.toggle_logs_btn, col, row) {
            dialog.toggle_logs();
            return true;
        }
        if rect_contains(dialog.cancel_btn, col, row) {
            self.connect_dialog = None;
            self.tear_down_connect_auth();
            self.status = String::from("Connect cancelled");
            return true;
        }
        if rect_contains(dialog.submit_btn, col, row) && dialog.phase.awaits_input() {
            let payload = dialog.input_for_submit();
            dialog.submitted = true;
            dialog.status_line = format!(
                "Sending {} chars to {}…",
                payload.chars().count(),
                dialog.host
            );
            dialog.clear_input();
            if let Some(auth) = self.connect_auth.as_mut() {
                auth.respond_password(&payload);
            }
            return true;
        }
        rect_contains(dialog.last_area, col, row)
    }

    fn open_ssh_config_in_editor(&mut self) {
        let Some(path) = crate::remote::primary_ssh_config_path() else {
            self.status = String::from("Cannot find $HOME/.ssh/config");
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if !path.exists()
            && let Err(e) = std::fs::write(&path, "")
        {
            self.status = format!("Could not create {}: {e}", path.display());
            return;
        }
        match self.editor.open_pinned(&path) {
            Ok(()) => {
                self.focus_pane(Pane::Editor);
                self.status = format!("Opened {}", path.display());
            }
            Err(e) => {
                self.status = format!("Open {} failed: {e}", path.display());
            }
        }
    }

    fn open_ssh_learn_more(&mut self) {
        let url = "https://man.openbsd.org/ssh_config";
        if self.drop_relay_active() {
            if self.trust_local_browser {
                self.request_remote_url_open(url.to_string());
                self.status = String::from("Opening ssh_config(5) on your local Mac…");
            } else {
                self.pending_local_open = Some(url.to_string());
            }
            return;
        }
        match open_url(url) {
            Ok(()) => self.status = String::from("Opened ssh_config(5) docs"),
            Err(e) => self.status = format!("Open link failed: {e}"),
        }
    }

    fn refresh_remote_if_config_changed(&mut self) -> bool {
        if self.sidebar_view != SidebarView::Remote {
            return false;
        }
        if !self.remote.refresh_if_config_changed() {
            return false;
        }
        self.status = String::from("Reloaded SSH remotes");
        true
    }

    fn open_search_hit(&mut self, hit: &crate::widgets::search::SearchHit) {
        match self.editor.open_preview(&hit.path) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                let row = hit
                    .line_no
                    .saturating_sub(1)
                    .min(self.editor.lines.len().saturating_sub(1));
                self.editor.cursor_row = row;
                // Drop the cursor at the first match column on this line so
                // long lines (HTML with base64 blobs, minified JS) auto-scroll
                // horizontally and the user lands on the match instead of the
                // line's leftmost cell - which on a 200k-char line means
                // never seeing the match without manual scrolling.
                let needle = self.search.query.trim();
                let opts = self.search.opts;
                let line = self.editor.lines.get(row).cloned().unwrap_or_default();
                // Locate the first match on the line: its start column and
                // length (in chars). The length lets us mark this occurrence
                // as the *active* match so the renderer paints it the stronger
                // orange while sibling matches stay yellow - mirroring the
                // Cmd+F path's `active_search_match` behaviour and VS Code's
                // "current match" highlight.
                let first_match = if needle.is_empty() {
                    None
                } else {
                    let mut col = 0usize;
                    let mut found = None;
                    for (chunk, is_match) in
                        crate::widgets::search::split_for_highlight(&line, needle, opts)
                    {
                        if is_match {
                            found = Some((col, chunk.chars().count()));
                            break;
                        }
                        col = col.saturating_add(chunk.chars().count());
                    }
                    found
                };
                let match_col = first_match.map(|(c, _)| c).unwrap_or(0);
                self.editor.active_search_match = first_match.map(|(c, len)| (row, c, len));
                self.editor.cursor_col = match_col;
                self.editor.scroll_col = 0;
                self.editor.ensure_cursor_col_visible();
                self.status = format!("Opened {} at line {}", hit.path.display(), hit.line_no);
                self.focus_pane(Pane::Editor);
            }
            Err(e) => {
                self.status = format!("Open failed: {e}");
            }
        }
    }

    fn handle_tree_key(&mut self, key: KeyEvent) {
        // Delete key (or Cmd+Backspace) trashes every selected node.
        if is_delete_node_key(key) {
            self.delete_selection();
            return;
        }
        if is_editor_select_all_key(key) {
            self.tree.select_all_visible();
            return;
        }
        if is_editor_copy_key(key) {
            self.copy_selection_to_explorer_clipboard(ExplorerClipMode::Copy);
            return;
        }
        if is_editor_cut_key(key) {
            self.copy_selection_to_explorer_clipboard(ExplorerClipMode::Cut);
            return;
        }
        if is_clipboard_paste_key(key) {
            self.paste_explorer_clipboard();
            return;
        }
        if is_compare_key(key) {
            self.toggle_compare_on_selected_file();
            return;
        }
        if key.code == KeyCode::Esc {
            if !self.tree.marked.is_empty() {
                self.tree.clear_marks();
            }
            return;
        }
        let extending = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Up => {
                if extending {
                    self.tree.move_up_extend();
                } else {
                    self.tree.move_up();
                }
            }
            KeyCode::Down => {
                if extending {
                    self.tree.move_down_extend();
                } else {
                    self.tree.move_down();
                }
            }
            KeyCode::PageUp => {
                if extending {
                    self.tree.page_up_extend(10);
                } else {
                    self.tree.page_up(10);
                }
            }
            KeyCode::PageDown => {
                if extending {
                    self.tree.page_down_extend(10);
                } else {
                    self.tree.page_down(10);
                }
            }
            KeyCode::Home => {
                if extending {
                    self.tree.home_extend();
                } else {
                    self.tree.home();
                }
            }
            KeyCode::End => {
                if extending {
                    self.tree.end_extend();
                } else {
                    self.tree.end();
                }
            }
            KeyCode::Enter => {
                if let Some(path) = self.tree.activate() {
                    // Ctrl+Enter is the primary "open in new tab" gesture — it
                    // reaches the app on every terminal. Cmd+Enter is also
                    // accepted but iTerm2 binds it to Toggle Fullscreen by
                    // default and never forwards it to the app unless the user
                    // has remapped that shortcut in iTerm Preferences →
                    // Profiles → Keys.
                    let in_new_tab = key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META,
                    );
                    let result = if in_new_tab {
                        self.editor.open_pinned(&path)
                    } else {
                        self.editor.open_preview(&path)
                    };
                    match result {
                        Ok(()) => {
                            self.sync_open_file_poll_mtime();
                            self.status = self.editor.status.clone();
                            // Stay focused on the tree so Delete / arrows still
                            // act on the explorer; click into the editor pane
                            // to start typing.
                        }
                        Err(e) => {
                            self.status = format!("Error: {e}");
                        }
                    }
                }
            }
            KeyCode::Right => {
                let selected_is_dir = self
                    .tree
                    .nodes
                    .get(self.tree.selected)
                    .is_some_and(|node| node.is_dir);
                if selected_is_dir {
                    self.tree.expand_selected();
                } else if let Some(path) = self.tree.activate() {
                    let result = self.editor.open_preview(&path);
                    match result {
                        Ok(()) => {
                            self.sync_open_file_poll_mtime();
                            self.status = self.editor.status.clone();
                        }
                        Err(e) => {
                            self.status = format!("Error: {e}");
                        }
                    }
                }
            }
            KeyCode::Left => {
                self.tree.collapse_selected();
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.apply_tree_typeahead(c, std::time::Instant::now());
            }
            _ => {}
        }
    }

    /// Flip native modal (vim) editing on or off. Driven by the global Cmd+E
    /// shortcut, so it can fire from any pane. Turning it off also dismisses
    /// any lingering `/` `?` search-match highlights, since the modal layer is
    /// the only place that clears them.
    fn toggle_vim_mode(&mut self) {
        self.vim.toggle();
        if self.vim.enabled {
            self.status = String::from("vim mode on");
        } else {
            self.editor
                .set_search_highlight(None, crate::widgets::search::SearchOpts::default());
            self.status = String::from("vim mode off");
        }
    }

    /// True when the active editor tab holds an editable text buffer (not a
    /// read-only diff / spreadsheet / image preview), so text-editing chords
    /// know whether to act.
    fn editor_is_text(&self) -> bool {
        self.editor.diff.is_none() && self.editor.sheet.is_none() && self.editor.image.is_none()
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
        // Inline find bar (Cmd+F / Ctrl+F) eats every key while open.
        if self.editor_find.is_some() {
            self.handle_editor_find_key(key);
            return;
        }
        if is_editor_find_key(key) {
            self.open_editor_find();
            return;
        }
        // Completion popup intercepts navigation / accept / dismiss keys
        // before normal editor handling. Any other key dismisses the popup
        // and falls through so the character lands in the buffer.
        if self.handle_completion_popup_key(key) {
            return;
        }
        if is_completion_trigger_key(key) {
            self.trigger_completion();
            return;
        }
        // Rename Symbol (F2) and Change All Occurrences (Cmd/Ctrl+F2) are only
        // meaningful on a text buffer, so they sit above the read-only diff /
        // sheet / image guards below.
        if is_rename_symbol_key(key) {
            self.start_rename_symbol();
            return;
        }
        if is_change_all_occurrences_key(key) {
            self.start_change_all_occurrences();
            return;
        }
        // VS Code "Toggle Line Comment" (Cmd+/ / Ctrl+/), "Toggle Block
        // Comment" (Shift+Alt+A), and "View: Toggle Word Wrap" (Alt+Z). All
        // act on a text buffer, so they no-op on read-only diff / sheet /
        // image tabs (the keystroke is still swallowed). Each is also
        // reachable from the Command Palette, which guarantees parity on
        // terminals that don't deliver the Alt+letter chords.
        if is_toggle_line_comment_key(key) {
            if self.editor_is_text() {
                self.editor.toggle_line_comment();
            }
            return;
        }
        if is_toggle_block_comment_key(key) {
            if self.editor_is_text() {
                self.editor.toggle_block_comment();
            }
            return;
        }
        if is_toggle_wrap_key(key) {
            if self.editor_is_text() {
                self.editor.toggle_wrap();
            }
            return;
        }
        // Go to Definition (F12) / References (Shift+F12) / Type Definition
        // (Ctrl+F12) / Implementations (Cmd+F12) / Declaration (Ctrl+Shift+F12),
        // the VS Code F12-family bindings, also need a real text buffer. All are
        // no-ops on read-only diff / sheet / image tabs; the keystroke is still
        // swallowed so it never leaks into the buffer.
        if is_go_to_definition_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.request_definition_at_cursor();
            }
            return;
        }
        if is_go_to_declaration_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.request_declaration_at_cursor();
            }
            return;
        }
        if is_go_to_type_definition_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.request_type_definition_at_cursor();
            }
            return;
        }
        if is_go_to_implementation_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.request_implementation_at_cursor();
            }
            return;
        }
        if is_go_to_references_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.request_references_at_cursor();
            }
            return;
        }
        // While multi-cursor mode is live, printable typing / backspace /
        // delete fan out to every caret and Esc collapses back to one cursor.
        // Any other key collapses the carets and falls through to normal
        // single-cursor handling (e.g. an arrow moves just the primary).
        if self.editor.has_multi_cursor() {
            match key.code {
                KeyCode::Esc => {
                    self.editor.collapse_carets();
                    self.status = String::from("Cursors collapsed");
                    return;
                }
                KeyCode::Backspace => {
                    self.editor.multi_backspace();
                    return;
                }
                KeyCode::Delete => {
                    self.editor.multi_delete_forward();
                    return;
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT)
                        && !key.modifiers.contains(KeyModifiers::SUPER) =>
                {
                    self.editor.multi_insert_char(c);
                    return;
                }
                _ => self.editor.collapse_carets(),
            }
        }
        // Close-tab (Cmd/Ctrl+W) applies to every tab type, so it sits above
        // the read-only diff / sheet / image guards below. Those guards each
        // short-circuit to a view-specific handler that has no close branch,
        // so without this hoist Cmd+W is silently swallowed on a diff, sheet,
        // or image tab and the tab can never be closed from the keyboard.
        if is_close_tab_key(key) {
            if self.editor.close_active() {
                self.sync_open_file_poll_mtime();
                self.status = String::from("Closed tab");
                // Closing the focused group's last tab while split closes
                // the group: collapse back to the surviving column.
                self.collapse_split_if_empty();
            } else {
                self.status = String::from("Cannot close last tab");
            }
            return;
        }
        if self.editor.diff.is_some() {
            self.handle_diff_key(key);
            return;
        }
        if self.editor.sheet.is_some() {
            self.handle_sheet_key(key);
            return;
        }
        // Image preview tabs are read-only. PDF tabs allow page navigation
        // via Left/Right + PageUp/PageDown; everything else is swallowed.
        if self.editor.image.is_some() {
            if self.editor.image.as_ref().is_some_and(|i| i.pdf.is_some()) {
                let delta: i32 = match key.code {
                    KeyCode::Right | KeyCode::PageDown | KeyCode::Char(' ') => 1,
                    KeyCode::Left | KeyCode::PageUp => -1,
                    KeyCode::Home => i32::MIN,
                    KeyCode::End => i32::MAX,
                    _ => 0,
                };
                if delta != 0 {
                    let absolute = matches!(key.code, KeyCode::Home | KeyCode::End);
                    let stepped = if absolute {
                        let target_page: i32 = if delta < 0 { 1 } else { i32::MAX };
                        let cur = self
                            .editor
                            .image
                            .as_ref()
                            .and_then(|i| i.pdf.as_ref())
                            .map(|p| p.current_page as i32)
                            .unwrap_or(1);
                        self.editor.change_pdf_page(target_page - cur)
                    } else {
                        self.editor.change_pdf_page(delta)
                    };
                    if stepped {
                        // Force the OSC overlay to re-bake on next render.
                        let side = self.focused_image_side();
                        self.overlays.editor[side].invalidate_layout();
                    }
                }
            }
            return;
        }
        // When modal editing is on it fully owns the editor pane and the
        // always-on `editor_vim_chord` convenience layer is bypassed. A
        // PassThrough means the key is not claimed by vim (Insert-mode typing,
        // or any Cmd/Ctrl shortcut in Normal mode) and falls through to the
        // editor's normal handling below.
        if self.vim.enabled {
            match self.vim.on_key(key) {
                crate::vim::VimKeyResult::Consumed(actions) => {
                    for action in actions {
                        self.apply_vim_action(action);
                    }
                    return;
                }
                crate::vim::VimKeyResult::PassThrough => {}
            }
        } else if self.handle_editor_vim_chord(key) {
            return;
        }
        if is_editor_line_home_key(key) {
            self.editor.clear_selection();
            self.editor.home_line();
            return;
        }
        if is_editor_line_end_key(key) {
            self.editor.clear_selection();
            self.editor.end_line();
            return;
        }
        if is_editor_kill_to_eol_key(key) {
            let killed = self.editor.kill_to_eol();
            if !killed.is_empty() {
                copy_to_clipboard(&killed);
                self.status = format!("Killed {} chars", killed.chars().count());
            }
            return;
        }
        if is_editor_kill_to_bol_key(key) {
            let killed = self.editor.kill_to_bol();
            if !killed.is_empty() {
                copy_to_clipboard(&killed);
                self.status = format!("Killed {} chars", killed.chars().count());
            }
            return;
        }
        if is_editor_open_line_below_key(key) {
            self.editor.open_line_below();
            self.status = String::from("Opened line below");
            return;
        }
        if is_editor_open_line_above_key(key) {
            self.editor.open_line_above();
            self.status = String::from("Opened line above");
            return;
        }
        // Clipboard gestures take precedence over text input. They never
        // conflict with normal typing (Ctrl/Cmd + C/X/A) so the order is
        // safe even when the user is in the middle of editing.
        if is_editor_copy_key(key) {
            self.copy_editor_selection();
            return;
        }
        if is_editor_cut_key(key) {
            self.cut_editor_selection();
            return;
        }
        if is_editor_paste_key(key) {
            let text = (self.clipboard_reader)();
            self.paste_clipboard_into_editor(text.as_deref());
            return;
        }
        if is_editor_select_all_key(key) {
            self.editor.select_all();
            self.status = format!(
                "Selected {} chars",
                self.editor.selection_text().chars().count()
            );
            return;
        }
        if is_editor_undo_key(key) {
            if self.editor.undo() {
                self.status = String::from("Undo");
            } else {
                self.status = String::from("Nothing to undo");
            }
            return;
        }
        if let Some(idx) = jump_to_tab_index(key) {
            if self.editor.select(idx) {
                self.sync_open_file_poll_mtime();
                self.status = format!("Tab {}", idx + 1);
            }
            return;
        }
        if matches!(key.code, KeyCode::Esc) && self.editor.selection.is_some() {
            self.editor.clear_selection();
            return;
        }

        // Shift+<motion> extends selection in the motion's direction.
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let is_motion = matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        );
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // The command modifier: Cmd (SUPER) on macOS, Ctrl on Linux / Termux.
        let cmd = key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL);
        // VS Code "Add Cursor Above/Below" (Cmd+Opt+Up / Cmd+Opt+Down). Must
        // come before the plain-Alt move-line check so the cmd+alt combo is
        // routed here, not there.
        if cmd && alt && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            match key.code {
                KeyCode::Up => self.editor.add_cursor_above(),
                KeyCode::Down => self.editor.add_cursor_below(),
                _ => unreachable!(),
            }
            return;
        }
        if shift && alt && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            match key.code {
                KeyCode::Up => self.editor.duplicate_lines_up(),
                KeyCode::Down => self.editor.duplicate_lines_down(),
                _ => unreachable!(),
            }
            return;
        }
        // VS Code "Move Line Up/Down" (Alt+Up / Alt+Down, no Shift, no Cmd).
        if alt && !shift && !cmd && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            match key.code {
                KeyCode::Up => self.editor.move_lines_up(),
                KeyCode::Down => self.editor.move_lines_down(),
                _ => unreachable!(),
            }
            return;
        }
        if is_motion && shift {
            if self.editor.selection.is_none() {
                self.editor.start_selection_at_cursor();
            }
            match key.code {
                KeyCode::Up => self.editor.move_up(),
                KeyCode::Down => self.editor.move_down(),
                KeyCode::Left if alt => self.editor.move_word_left(),
                KeyCode::Right if alt => self.editor.move_word_right(),
                KeyCode::Left => self.editor.move_left(),
                KeyCode::Right => self.editor.move_right(),
                KeyCode::Home => self.editor.home_line(),
                KeyCode::End => self.editor.end_line(),
                KeyCode::PageUp => self.editor.page_up_one_screen(),
                KeyCode::PageDown => self.editor.page_down_one_screen(),
                _ => {}
            }
            self.editor.extend_selection_to_cursor();
            return;
        }
        if is_motion {
            // Plain motion clears any prior selection (VS Code / Sublime
            // convention: arrows without Shift collapse the selection).
            self.editor.clear_selection();
        }

        match key.code {
            KeyCode::Up => self.editor.move_up(),
            KeyCode::Down => self.editor.move_down(),
            KeyCode::Left if alt => self.editor.move_word_left(),
            KeyCode::Right if alt => self.editor.move_word_right(),
            KeyCode::Left => self.editor.move_left(),
            KeyCode::Right => self.editor.move_right(),
            KeyCode::PageUp => self.editor.page_up_one_screen(),
            KeyCode::PageDown => self.editor.page_down_one_screen(),
            KeyCode::Home => self.editor.home_line(),
            KeyCode::End => self.editor.end_line(),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete_forward(),
            KeyCode::Enter => self.editor.insert_newline(),
            // Tab over a multi-line selection indents the whole block (VS
            // Code); otherwise it inserts indentation to the next tab stop.
            KeyCode::Tab if self.editor.selection_is_multiline() => {
                self.editor.indent_lines();
            }
            KeyCode::Tab => self.editor.indent_at_cursor(),
            // Shift+Tab arrives as BackTab. Outdent the current line, or every
            // line a selection touches, mirroring VS Code's Shift+Tab.
            KeyCode::BackTab => self.editor.dedent_lines(),
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER) =>
            {
                self.editor.insert_char(c);
                // `.` is the canonical LSP trigger character for member
                // access in Python / TS / Rust. Fire a completion request
                // immediately so the popup pops without a Ctrl+Space.
                // The popup's fallthrough already dismissed any prior
                // popup when this char arrived, so the new request just
                // populates a fresh popup when the response lands.
                if c == '.' {
                    self.trigger_completion();
                }
            }
            _ => {}
        }
    }

    /// Vim-style chord layer for the editor: `cmd+gg` jumps to top, `cmd+G`
    /// jumps to bottom, `cmd+dd` deletes a line, `cmd+yy` yanks a line.
    /// Counts go between the chord start and the completion key
    /// (`cmd+g 12 g` jumps to line 12; `cmd+d 3 d` deletes three lines).
    /// Returns true when the key was consumed; false means the caller
    /// should continue with normal editor handling.
    fn handle_editor_vim_chord(&mut self, key: KeyEvent) -> bool {
        if is_vim_goto_bottom(key) {
            let count = self.editor_vim_chord.count;
            self.editor_vim_chord.reset();
            if count > 0 {
                self.editor.goto_line(count);
                self.status = format!("Line {count}");
            } else {
                self.editor.goto_bottom();
                self.status = String::from("Bottom of file");
            }
            return true;
        }

        let count_pending = self.editor_vim_chord.count > 0;
        let op_pending = self.editor_vim_chord.op != VimOp::None;

        if (op_pending || count_pending)
            && let Some(d) = chord_digit_value(key)
        {
            self.editor_vim_chord.count = self
                .editor_vim_chord
                .count
                .saturating_mul(10)
                .saturating_add(d);
            let op = match self.editor_vim_chord.op {
                VimOp::G => 'g',
                VimOp::D => 'd',
                VimOp::Y => 'y',
                VimOp::None => ' ',
            };
            self.status = chord_status_text(op, self.editor_vim_chord.count);
            return true;
        }

        if !op_pending {
            if is_vim_chord_g(key) {
                self.editor_vim_chord.op = VimOp::G;
                self.status = chord_status_text('g', self.editor_vim_chord.count);
                return true;
            }
            if is_vim_chord_d(key) {
                self.editor_vim_chord.op = VimOp::D;
                self.status = chord_status_text('d', self.editor_vim_chord.count);
                return true;
            }
            if is_vim_chord_y(key) {
                self.editor_vim_chord.op = VimOp::Y;
                self.status = chord_status_text('y', self.editor_vim_chord.count);
                return true;
            }
            if !count_pending {
                if let Some(d) = cmd_only_digit_value(key) {
                    self.editor_vim_chord.count = d;
                    self.status = chord_status_text(' ', self.editor_vim_chord.count);
                    return true;
                }
                return false;
            }
            self.editor_vim_chord.reset();
            self.status = String::from("Chord cancelled");
            return false;
        }

        let completes = match self.editor_vim_chord.op {
            VimOp::G => is_plain_letter(key, 'g') || is_vim_chord_g(key),
            VimOp::D => is_plain_letter(key, 'd') || is_vim_chord_d(key),
            VimOp::Y => is_plain_letter(key, 'y') || is_vim_chord_y(key),
            VimOp::None => false,
        };

        if completes {
            let count = self.editor_vim_chord.count;
            match self.editor_vim_chord.op {
                VimOp::G => {
                    if count > 0 {
                        self.editor.goto_line(count);
                        self.status = format!("Line {count}");
                    } else {
                        self.editor.goto_top();
                        self.status = String::from("Top of file");
                    }
                }
                VimOp::D => {
                    let n = count.max(1);
                    let yanked = self.editor.delete_lines(n);
                    if !yanked.is_empty() {
                        copy_to_clipboard(&yanked);
                    }
                    self.status = format!("Deleted {n} {}", if n == 1 { "line" } else { "lines" });
                }
                VimOp::Y => {
                    let n = count.max(1);
                    let yanked = self.editor.yank_lines(n);
                    if !yanked.is_empty() {
                        copy_to_clipboard(&yanked);
                    }
                    self.status = format!("Yanked {n} {}", if n == 1 { "line" } else { "lines" });
                }
                VimOp::None => {}
            }
            self.editor_vim_chord.reset();
            return true;
        }

        self.editor_vim_chord.reset();
        self.status = String::from("Chord cancelled");
        false
    }

    /// Apply one resolved vim intent to the editor. The vim state machine has
    /// already advanced its own mode; this only mutates the buffer/cursor.
    fn apply_vim_action(&mut self, action: crate::vim::VimAction) {
        use crate::vim::{Target, VimAction};
        match action {
            VimAction::Move(motion, count) => {
                self.vim_apply_motion(motion, count);
                self.vim_sync_visual_selection();
            }
            VimAction::Operate { op, target, count } => match target {
                Target::Lines => self.vim_operate_lines(op, self.editor.cursor_row, count),
                Target::Object(obj) => self.vim_operate_textobject(op, obj),
                Target::Motion(m) => self.vim_operate_motion(op, m, count),
            },
            VimAction::DeleteChar(count) => self.vim_delete_chars(count),
            VimAction::Insert(at) => self.vim_enter_insert(at),
            VimAction::Paste(at, count) => self.vim_paste(at, count),
            VimAction::Undo => {
                self.status = if self.editor.undo() {
                    String::from("Undo")
                } else {
                    String::from("Nothing to undo")
                };
            }
            VimAction::EnterVisual { line } => {
                self.editor.break_undo_coalescing();
                self.vim_visual_line = line;
                self.vim
                    .set_visual_anchor(self.editor.cursor_row, self.editor.cursor_col);
                self.editor.start_selection_at_cursor();
                self.status = String::from(if line {
                    "-- VISUAL LINE --"
                } else {
                    "-- VISUAL --"
                });
            }
            VimAction::ExitVisual => {
                self.editor.clear_selection();
                self.vim_visual_line = false;
                self.status.clear();
            }
            VimAction::VisualOperate(op) => self.vim_visual_operate(op),
            VimAction::ExitInsert => {
                self.editor.break_undo_coalescing();
                if self.editor.cursor_col > 0 {
                    self.editor.cursor_col -= 1;
                }
                self.status.clear();
            }
            VimAction::RunEx(cmd) => self.vim_run_ex(&cmd),
            VimAction::Search(dir, term) => self.vim_search(dir, &term),
            VimAction::ClearPending => {
                self.editor.clear_selection();
                self.vim_visual_line = false;
            }
        }
    }

    fn vim_line_len(&self, row: usize) -> usize {
        self.editor
            .lines
            .get(row)
            .map(|l| l.chars().count())
            .unwrap_or(0)
    }

    fn vim_first_non_blank(&self, row: usize) -> usize {
        self.editor
            .lines
            .get(row)
            .map(|l| l.chars().take_while(|c| c.is_whitespace()).count())
            .unwrap_or(0)
    }

    fn vim_apply_motion(&mut self, motion: crate::vim::Motion, count: usize) {
        use crate::vim::Motion;
        let n = count.max(1);
        match motion {
            Motion::Left => {
                self.editor.cursor_col = self.editor.cursor_col.saturating_sub(n);
            }
            Motion::Right => {
                let len = self.vim_line_len(self.editor.cursor_row);
                self.editor.cursor_col = (self.editor.cursor_col + n).min(len);
            }
            Motion::Up => {
                for _ in 0..n {
                    self.editor.move_up();
                }
            }
            Motion::Down => {
                for _ in 0..n {
                    self.editor.move_down();
                }
            }
            Motion::WordForward => {
                for _ in 0..n {
                    self.vim_word_forward();
                }
            }
            Motion::WordBackward => {
                for _ in 0..n {
                    self.vim_word_backward();
                }
            }
            Motion::WordEnd => {
                for _ in 0..n {
                    self.vim_word_end();
                }
            }
            Motion::LineStart => self.editor.cursor_col = 0,
            Motion::FirstNonBlank => {
                self.editor.cursor_col = self.vim_first_non_blank(self.editor.cursor_row);
            }
            Motion::LineEnd => {
                self.editor.cursor_col = self.vim_line_len(self.editor.cursor_row);
            }
            Motion::FileStart => self.editor.goto_top(),
            Motion::FileEnd => self.editor.goto_bottom(),
            Motion::GotoLine(line) => self.editor.goto_line(line),
            Motion::Find(kind, ch) => {
                self.vim_last_find = Some((kind, ch));
                self.vim_find_char(kind, ch, n);
            }
            Motion::RepeatFind => {
                if let Some((kind, ch)) = self.vim_last_find {
                    self.vim_find_char(kind, ch, n);
                }
            }
            Motion::RepeatFindRev => {
                if let Some((kind, ch)) = self.vim_last_find {
                    self.vim_find_char(reverse_find(kind), ch, n);
                }
            }
        }
        self.editor.ensure_cursor_col_visible();
    }

    fn vim_find_char(&mut self, kind: crate::vim::FindKind, ch: char, count: usize) {
        use crate::vim::FindKind;
        let row = self.editor.cursor_row;
        let Some(line) = self.editor.lines.get(row) else {
            return;
        };
        let chars: Vec<char> = line.chars().collect();
        let mut col = self.editor.cursor_col;
        for _ in 0..count.max(1) {
            match kind {
                FindKind::CharForward => match (col + 1..chars.len()).find(|&i| chars[i] == ch) {
                    Some(idx) => col = idx,
                    None => return,
                },
                FindKind::TillForward => match (col + 1..chars.len()).find(|&i| chars[i] == ch) {
                    Some(idx) if idx > 0 => col = idx - 1,
                    _ => return,
                },
                FindKind::CharBackward => match (0..col).rev().find(|&i| chars[i] == ch) {
                    Some(idx) => col = idx,
                    None => return,
                },
                FindKind::TillBackward => match (0..col).rev().find(|&i| chars[i] == ch) {
                    Some(idx) => col = idx + 1,
                    None => return,
                },
            }
        }
        self.editor.cursor_col = col;
    }

    /// Character class for vim word motions: 0 = blank (or past line end),
    /// 1 = keyword (alphanumeric/underscore), 2 = punctuation. A `w`/`b`/`e`
    /// word is a maximal run of one non-blank class.
    fn vim_class_at(&self, row: usize, col: usize) -> u8 {
        match self.editor.lines.get(row).and_then(|l| l.chars().nth(col)) {
            None => 0,
            Some(c) if c.is_whitespace() => 0,
            Some(c) if c.is_alphanumeric() || c == '_' => 1,
            Some(_) => 2,
        }
    }

    fn vim_next_pos(&self, row: usize, col: usize) -> Option<(usize, usize)> {
        if col < self.vim_line_len(row) {
            Some((row, col + 1))
        } else if row + 1 < self.editor.lines.len() {
            Some((row + 1, 0))
        } else {
            None
        }
    }

    fn vim_prev_pos(&self, row: usize, col: usize) -> Option<(usize, usize)> {
        if col > 0 {
            Some((row, col - 1))
        } else if row > 0 {
            Some((row - 1, self.vim_line_len(row - 1)))
        } else {
            None
        }
    }

    fn vim_word_forward(&mut self) {
        let mut row = self.editor.cursor_row;
        let mut col = self.editor.cursor_col;
        let start = self.vim_class_at(row, col);
        if start != 0 {
            while self.vim_class_at(row, col) == start {
                match self.vim_next_pos(row, col) {
                    Some((r, c)) => {
                        row = r;
                        col = c;
                    }
                    None => {
                        self.editor.cursor_row = row;
                        self.editor.cursor_col = col;
                        return;
                    }
                }
            }
        }
        while self.vim_class_at(row, col) == 0 {
            match self.vim_next_pos(row, col) {
                Some((r, c)) => {
                    row = r;
                    col = c;
                }
                None => break,
            }
        }
        self.editor.cursor_row = row;
        self.editor.cursor_col = col;
    }

    fn vim_word_backward(&mut self) {
        let mut row = self.editor.cursor_row;
        let mut col = self.editor.cursor_col;
        let Some((r, c)) = self.vim_prev_pos(row, col) else {
            return;
        };
        row = r;
        col = c;
        while self.vim_class_at(row, col) == 0 {
            match self.vim_prev_pos(row, col) {
                Some((r, c)) => {
                    row = r;
                    col = c;
                }
                None => {
                    self.editor.cursor_row = row;
                    self.editor.cursor_col = col;
                    return;
                }
            }
        }
        let class = self.vim_class_at(row, col);
        while let Some((pr, pc)) = self.vim_prev_pos(row, col) {
            if self.vim_class_at(pr, pc) != class {
                break;
            }
            row = pr;
            col = pc;
        }
        self.editor.cursor_row = row;
        self.editor.cursor_col = col;
    }

    fn vim_word_end(&mut self) {
        let mut row = self.editor.cursor_row;
        let mut col = self.editor.cursor_col;
        let Some((r, c)) = self.vim_next_pos(row, col) else {
            return;
        };
        row = r;
        col = c;
        while self.vim_class_at(row, col) == 0 {
            match self.vim_next_pos(row, col) {
                Some((r, c)) => {
                    row = r;
                    col = c;
                }
                None => {
                    self.editor.cursor_row = row;
                    self.editor.cursor_col = col;
                    return;
                }
            }
        }
        let class = self.vim_class_at(row, col);
        while let Some((nr, nc)) = self.vim_next_pos(row, col) {
            if self.vim_class_at(nr, nc) != class {
                break;
            }
            row = nr;
            col = nc;
        }
        self.editor.cursor_row = row;
        self.editor.cursor_col = col;
    }

    fn vim_operate_lines(&mut self, op: crate::vim::Op, start_row: usize, count: usize) {
        use crate::vim::Op;
        self.editor.cursor_row = start_row.min(self.editor.lines.len().saturating_sub(1));
        let n = count.max(1);
        match op {
            Op::Delete => {
                let text = self.editor.delete_lines(n);
                copy_to_clipboard(&text);
                self.status = format!("Deleted {n} line(s)");
            }
            Op::Yank => {
                let text = self.editor.yank_lines(n);
                copy_to_clipboard(&text);
                self.status = format!("Yanked {n} line(s)");
            }
            Op::Change => {
                let text = self.editor.delete_lines(n);
                copy_to_clipboard(&text);
                self.editor.open_line_above();
                self.vim.enter_insert_mode();
                self.status = String::from("-- INSERT --");
            }
        }
    }

    fn vim_operate_motion(&mut self, op: crate::vim::Op, motion: crate::vim::Motion, count: usize) {
        if vim_motion_is_linewise(motion) {
            let (first, last) = self.vim_line_range_for_motion(motion, count);
            self.vim_operate_lines(op, first, last + 1 - first);
            return;
        }
        let start = (self.editor.cursor_row, self.editor.cursor_col);
        self.vim_apply_motion(motion, count);
        let end = (self.editor.cursor_row, self.editor.cursor_col);
        let (lo, hi) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        // Forward-inclusive motions (f/t/e) cover the char they land on.
        let head = if start <= end && vim_motion_is_forward_inclusive(motion) {
            let len = self.vim_line_len(hi.0);
            (hi.0, (hi.1 + 1).min(len))
        } else {
            hi
        };
        self.editor.selection = Some(crate::widgets::editor::EditorSelection { anchor: lo, head });
        self.vim_apply_operator_to_selection(op, lo);
    }

    fn vim_operate_textobject(&mut self, op: crate::vim::Op, obj: crate::vim::TextObject) {
        use crate::vim::TextObject;
        let row = self.editor.cursor_row;
        let Some((start, mut end)) = self.editor.word_at(row, self.editor.cursor_col) else {
            return;
        };
        if obj == TextObject::AWord
            && let Some(line) = self.editor.lines.get(row)
        {
            let chars: Vec<char> = line.chars().collect();
            while end < chars.len() && chars[end].is_whitespace() {
                end += 1;
            }
        }
        self.editor.selection = Some(crate::widgets::editor::EditorSelection {
            anchor: (row, start),
            head: (row, end),
        });
        self.vim_apply_operator_to_selection(op, (row, start));
    }

    /// Delete / yank / change whatever the editor selection currently spans,
    /// leaving the cursor at `start` and entering Insert mode for a change.
    fn vim_apply_operator_to_selection(&mut self, op: crate::vim::Op, start: (usize, usize)) {
        use crate::vim::Op;
        let text = self.editor.selection_text();
        copy_to_clipboard(&text);
        match op {
            Op::Yank => {
                self.editor.clear_selection();
                self.editor.cursor_row = start.0;
                self.editor.cursor_col = start.1;
            }
            Op::Delete => {
                self.editor.delete_selection();
            }
            Op::Change => {
                self.editor.delete_selection();
                self.vim.enter_insert_mode();
                self.status = String::from("-- INSERT --");
            }
        }
    }

    fn vim_line_range_for_motion(
        &self,
        motion: crate::vim::Motion,
        count: usize,
    ) -> (usize, usize) {
        use crate::vim::Motion;
        let row = self.editor.cursor_row;
        let last = self.editor.lines.len().saturating_sub(1);
        let n = count.max(1);
        match motion {
            Motion::Down => (row, (row + n).min(last)),
            Motion::Up => (row.saturating_sub(n), row),
            Motion::FileStart => (0, row),
            Motion::FileEnd => (row, last),
            Motion::GotoLine(line) => {
                let target = line.saturating_sub(1).min(last);
                if target >= row {
                    (row, target)
                } else {
                    (target, row)
                }
            }
            _ => (row, row),
        }
    }

    fn vim_delete_chars(&mut self, count: usize) {
        let row = self.editor.cursor_row;
        let Some(line) = self.editor.lines.get(row) else {
            return;
        };
        let chars: Vec<char> = line.chars().collect();
        let col = self.editor.cursor_col;
        let end = (col + count.max(1)).min(chars.len());
        if col < end {
            let deleted: String = chars[col..end].iter().collect();
            copy_to_clipboard(&deleted);
            for _ in col..end {
                self.editor.delete_forward();
            }
        }
    }

    fn vim_enter_insert(&mut self, at: crate::vim::InsertAt) {
        use crate::vim::InsertAt;
        let row = self.editor.cursor_row;
        match at {
            InsertAt::Cursor => {}
            InsertAt::After => {
                let len = self.vim_line_len(row);
                if self.editor.cursor_col < len {
                    self.editor.cursor_col += 1;
                }
            }
            InsertAt::LineStart => self.editor.cursor_col = self.vim_first_non_blank(row),
            InsertAt::LineEnd => self.editor.cursor_col = self.vim_line_len(row),
            InsertAt::Below => self.editor.open_line_below(),
            InsertAt::Above => self.editor.open_line_above(),
        }
        self.editor.break_undo_coalescing();
        self.status = String::from("-- INSERT --");
    }

    fn vim_paste(&mut self, at: crate::vim::PasteAt, count: usize) {
        use crate::vim::PasteAt;
        let Some(text) = (self.clipboard_reader)() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let n = count.max(1);
        if let Some(body) = text.strip_suffix('\n') {
            // Linewise paste: the yank ended on a newline, so it lands on
            // fresh lines rather than inside the current one.
            let block = std::iter::repeat_n(body, n).collect::<Vec<_>>().join("\n");
            match at {
                PasteAt::After => {
                    self.editor.cursor_col = self.vim_line_len(self.editor.cursor_row);
                    self.editor.insert_str(&format!("\n{block}"));
                }
                PasteAt::Before => {
                    self.editor.cursor_col = 0;
                    self.editor.insert_str(&format!("{block}\n"));
                }
            }
        } else {
            let block = text.repeat(n);
            if at == PasteAt::After {
                let len = self.vim_line_len(self.editor.cursor_row);
                if self.editor.cursor_col < len {
                    self.editor.cursor_col += 1;
                }
            }
            self.editor.insert_str(&block);
        }
        self.status = String::from("Pasted");
    }

    fn vim_visual_operate(&mut self, op: crate::vim::Op) {
        use crate::vim::Op;
        let Some(sel) = self.editor.selection else {
            return;
        };
        if self.vim_visual_line {
            let ((sr, _), (er, _)) = sel.normalised();
            self.vim_operate_lines(op, sr, er + 1 - sr);
            self.vim_visual_line = false;
            return;
        }
        // Charwise visual is inclusive of the cell under the cursor, so extend
        // the high end one char before operating.
        let ((sr, sc), (er, ec)) = sel.normalised();
        let len = self.vim_line_len(er);
        self.editor.selection = Some(crate::widgets::editor::EditorSelection {
            anchor: (sr, sc),
            head: (er, (ec + 1).min(len)),
        });
        self.vim_apply_operator_to_selection(op, (sr, sc));
        self.vim_visual_line = false;
        if !matches!(op, Op::Change) {
            self.status.clear();
        }
    }

    fn vim_run_ex(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        if let Ok(line) = cmd.parse::<usize>() {
            self.editor.goto_line(line);
            return;
        }
        match cmd {
            "w" | "write" => self.save(),
            "q" | "q!" | "quit" => {
                if self.editor.close_active() {
                    self.sync_open_file_poll_mtime();
                    self.status = String::from("Closed tab");
                } else {
                    self.quit = true;
                }
            }
            "wq" | "x" | "wq!" => {
                self.save();
                if !self.editor.close_active() {
                    self.quit = true;
                }
            }
            "qa" | "qa!" | "quitall" => self.quit = true,
            other => self.status = format!("Unknown command: {other}"),
        }
    }

    fn vim_search(&mut self, dir: crate::vim::SearchDir, term: &str) {
        self.editor.set_search_highlight(
            Some(term.to_string()),
            crate::widgets::search::SearchOpts::default(),
        );
        match self.vim_find_match(dir, term) {
            Some((row, col)) => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.editor.ensure_cursor_col_visible();
                // Record the hit the cursor jumped to so the renderer paints it
                // in the distinct active colour (orange) while the remaining
                // matches keep the regular yellow. Literal search, so the match
                // is exactly `term` long in characters.
                self.editor.active_search_match = Some((row, col, term.chars().count()));
                self.status = match dir {
                    crate::vim::SearchDir::Forward => format!("/{term}"),
                    crate::vim::SearchDir::Backward => format!("?{term}"),
                };
            }
            None => {
                self.editor.active_search_match = None;
                self.status = format!("Pattern not found: {term}");
            }
        }
    }

    fn vim_find_match(&self, dir: crate::vim::SearchDir, term: &str) -> Option<(usize, usize)> {
        use crate::vim::SearchDir;
        let lines = &self.editor.lines;
        if term.is_empty() || lines.is_empty() {
            return None;
        }
        let (cr, cc) = (self.editor.cursor_row, self.editor.cursor_col);
        let total = lines.len();
        match dir {
            SearchDir::Forward => {
                for off in 0..=total {
                    let row = (cr + off) % total;
                    let from = if off == 0 { cc + 1 } else { 0 };
                    if let Some(col) = find_char_index(&lines[row], term, from) {
                        return Some((row, col));
                    }
                }
                None
            }
            SearchDir::Backward => {
                for off in 0..=total {
                    let row = (cr + total - off) % total;
                    let limit = if off == 0 { cc } else { usize::MAX };
                    if let Some(col) = rfind_char_index(&lines[row], term, limit) {
                        return Some((row, col));
                    }
                }
                None
            }
        }
    }

    fn vim_sync_visual_selection(&mut self) {
        if !matches!(
            self.vim.mode(),
            crate::vim::VimMode::Visual | crate::vim::VimMode::VisualLine
        ) {
            return;
        }
        if let Some(anchor) = self.vim.visual_anchor() {
            self.editor.selection = Some(crate::widgets::editor::EditorSelection {
                anchor,
                head: (self.editor.cursor_row, self.editor.cursor_col),
            });
        }
    }

    fn handle_terminal_key(&mut self, key: KeyEvent) {
        // Ctrl+Shift+T: open another terminal next to the active one.
        if is_terminal_split_key(key) {
            match self.split_terminal() {
                Ok(()) => {
                    self.status = format!("Split terminal: {} active", self.terminals.len());
                }
                Err(e) => {
                    self.status = format!("Split terminal failed: {e}");
                }
            }
            return;
        }
        // Cmd+W (or legacy Ctrl+Shift+W): close the active terminal (no-op when
        // only one is left).
        if is_terminal_close_key(key) {
            if self.close_active_terminal() {
                self.status = format!("Closed terminal: {} remaining", self.terminals.len());
            } else {
                self.status = String::from("Cannot close the last terminal; press Ctrl+J to hide");
            }
            return;
        }
        // Cmd+]: next terminal. Cmd+[: previous terminal.
        if is_terminal_cycle_key(key) {
            self.cycle_terminal();
            return;
        }
        if is_terminal_cycle_back_key(key) {
            self.cycle_terminal_back();
            return;
        }
        // Ctrl+Shift+C / Cmd+C: copy current selection.
        if is_terminal_copy_key(key) {
            self.copy_terminal_selection();
            return;
        }
        // Cmd+V / Ctrl+V / Ctrl+Shift+V: paste local clipboard into the
        // embedded shell. Without this, raw Ctrl+V bytes (\x16) just go
        // through to the shell unchanged, and Cmd+V — when not eaten by
        // the host terminal's menu shortcut — would do nothing useful.
        if is_clipboard_paste_key(key) {
            match crate::clipboard::read_string() {
                Some(text) if !text.is_empty() => {
                    self.terminal_mut().paste_input(text.as_bytes());
                }
                Some(_) => {
                    self.status = String::from("Cmd+V: clipboard is empty");
                }
                None if self.drop_relay_active() => {
                    self.request_remote_clipboard();
                }
                None => {
                    self.status =
                        String::from("Cmd+V: clipboard read failed (no pbpaste / NSPasteboard)");
                }
            }
            return;
        }
        // Any other keystroke clears the selection so the user's input is
        // sent without the previous highlight lingering on screen.
        if self.terminal().selection().is_some() {
            self.terminal_mut().clear_selection();
        }
        let bytes = key_to_bytes(key);
        if !bytes.is_empty() {
            self.terminal_mut().write_input(&bytes);
        }
    }

    /// Copy the terminal pane's current selection to the host clipboard via
    /// OSC 52.  Selection stays visible so the user can verify what was
    /// copied. No-op when the selection is empty / zero-area.
    fn copy_terminal_selection(&mut self) {
        let Some(sel) = self.terminal().selection() else {
            return;
        };
        if !sel.has_area() {
            return;
        }
        let text = self.terminal().selection_text();
        if text.is_empty() {
            return;
        }
        copy_to_clipboard(&text);
        self.status = format!("Copied {} chars to clipboard", text.chars().count());
    }

    fn copy_editor_selection(&mut self) {
        if let Some(diff) = self.editor.diff.as_ref() {
            if let Some(text) = diff.selection_text()
                && !text.is_empty()
            {
                copy_to_clipboard(&text);
                self.status = format!("Copied {} chars to clipboard", text.chars().count());
            }
            return;
        }
        let Some(sel) = self.editor.selection else {
            return;
        };
        if !sel.has_area() {
            return;
        }
        let text = self.editor.selection_text();
        if text.is_empty() {
            return;
        }
        copy_to_clipboard(&text);
        self.status = format!("Copied {} chars to clipboard", text.chars().count());
    }

    fn cut_editor_selection(&mut self) {
        let Some(sel) = self.editor.selection else {
            return;
        };
        if !sel.has_area() {
            return;
        }
        let text = self.editor.selection_text();
        if text.is_empty() {
            self.editor.clear_selection();
            return;
        }
        copy_to_clipboard(&text);
        let n = text.chars().count();
        self.editor.delete_selection();
        self.status = format!("Cut {n} chars to clipboard");
    }

    fn paste_clipboard_into_search(&mut self, text: Option<&str>) {
        let Some(s) = text else {
            self.status = String::from("Cmd+V: clipboard read failed");
            return;
        };
        if s.is_empty() {
            self.status = String::from("Cmd+V: clipboard is empty");
            return;
        }
        self.search.insert_str(s);
        // Replace text doesn't change which files match, so only re-run the
        // scan when the paste landed in a search-affecting field.
        if self.search.field != crate::widgets::search::SearchField::Replace {
            self.submit_search_query();
        }
        self.status = format!(
            "Cmd+V: pasted into {} field",
            search_field_name(self.search.field)
        );
    }

    fn paste_clipboard_into_editor(&mut self, text: Option<&str>) {
        let Some(s) = text else {
            self.status = String::from("Cmd+V: clipboard read failed");
            return;
        };
        if s.is_empty() {
            self.status = String::from("Cmd+V: clipboard is empty");
            return;
        }
        self.editor.insert_str(s);
        self.status = format!("Pasted {} chars", s.chars().count());
    }

    fn handle_paste(&mut self, s: &str) {
        // Modal overlays own paste entirely while open — paste lands in
        // the modal's input field, never in the editor / tree / terminal
        // behind it. Without this, hitting Cmd+V right after Cmd+P pastes
        // into whatever pane was last focused instead of into the finder
        // query, which makes the "open + paste a filename" path that the
        // VS Code Quick Open is meant to support unusable.
        if let Some(finder) = self.file_finder.as_mut() {
            for c in s.chars() {
                if !c.is_control() {
                    finder.push_char(c);
                }
            }
            return;
        }
        // The in-file find bar is a pane-scoped widget, not a modal: it only
        // owns the paste while the editor itself is focused. Without the focus
        // guard, an open find bar would swallow a paste meant for the terminal
        // (or tree) the user has since clicked into — the keystroke path is
        // already focus-gated via `match self.focus`, so paste must match it.
        if self.focus == Pane::Editor && self.editor_find.is_some() {
            let mut new_q = self
                .editor_find
                .as_ref()
                .map(|st| st.query.clone())
                .unwrap_or_default();
            for c in s.chars() {
                if !c.is_control() {
                    new_q.push(c);
                }
            }
            self.editor_find_set_query(new_q);
            return;
        }
        // Finder drag-and-drop into croft arrives via the host terminal as
        // a bracketed paste containing absolute filesystem path(s). The
        // drop in iTerm2 does NOT shift mouse focus, so we cannot require
        // `self.focus == Pane::Tree` for the gesture: the user can have
        // last clicked into the editor or the embedded terminal and still
        // expect the drop on the sidebar to import.
        //
        //   * Remote view: any path-shaped paste is an scp upload. There
        //     is no other reasonable thing the user could want, since the
        //     visible UI is a host list.
        //   * Explorer view: a path-shaped paste is an import only when
        //     focus is on the tree, otherwise we keep the current
        //     behaviour (typing a path string into the focused editor or
        //     terminal command line) because that is a legitimate use.
        // iTerm2's inline-image drag artefact ALWAYS short-circuits the
        // paste pipeline: a stray click-and-microdrag on an OSC-1337 cell
        // (activity-bar icon, welcome wordmark, no-repo / run-debug / SSH
        // hero) makes iTerm2 write the decoded PNG to $TMPDIR/iTerm2.<rand>.png
        // and re-inject the path as a bracketed paste. parse_dropped_paths
        // already filters those paths out of the import list, but without
        // an early-return here the fallthrough would still paste the raw
        // path string into the editor body (the user reported this as
        // "Pasted 67 chars" appearing in a freshly-opened CLAUDE.local.md
        // tab while they were trying to resize the sidebar near the
        // Explorer icon). Detect the artefact at the payload level and
        // silently drop the entire paste — no text, no import, no status.
        if parsed_drop_tokens(s)
            .iter()
            .any(|p| p.is_absolute() && is_iterm2_inline_image_drop(p))
        {
            return;
        }
        let dropped = parse_dropped_paths(s);
        if !dropped.is_empty() {
            match self.sidebar_view {
                SidebarView::Remote if self.focus == Pane::Tree => {
                    self.import_paths_into_remote(&dropped);
                    return;
                }
                SidebarView::Explorer if self.focus == Pane::Tree => {
                    self.import_paths_into_explorer(&dropped);
                    return;
                }
                _ => {}
            }
        }
        // Remote-launched croft case: the path the user dragged from
        // their local Finder doesn't exist on this remote box, so the
        // strict parser above returned nothing. If the drop-relay env
        // is plumbed (set by the local-croft parent over the SSH
        // session) and the paste shape is path-like, request a reverse
        // pull through the relay.
        if self.drop_relay_active() && self.sidebar_view == SidebarView::Explorer {
            let foreign = parse_foreign_dropped_paths(s);
            if !foreign.is_empty() {
                self.request_remote_pulls(&foreign);
                return;
            }
        }
        if self.sidebar_view == SidebarView::Search && self.focus == Pane::Tree {
            self.search.insert_str_into_query(s);
            self.submit_search_query();
            self.status = format!("Pasted {} chars", s.chars().count());
            return;
        }
        // Source Control commit message box: bracketed paste must
        // populate it the same way Cmd+V via the key path does
        // (`handle_source_control_key` -> `source_control.insert_str`).
        // Without this branch, in the production setup where iTerm2
        // emits Cmd+V as a bracketed paste rather than a key event,
        // the payload silently vanishes when the user has the SC
        // sidebar focused.
        if self.sidebar_view == SidebarView::SourceControl
            && self.focus != Pane::Editor
            && self.focus != Pane::Terminal
        {
            self.source_control.insert_str(s);
            self.status = format!("Pasted {} chars", s.chars().count());
            return;
        }
        match self.focus {
            Pane::Editor => {
                self.editor.insert_str(s);
                self.status = format!("Pasted {} chars", s.chars().count());
            }
            Pane::Terminal => {
                self.terminal_mut().paste_input(s.as_bytes());
            }
            Pane::Tree => {}
        }
    }

    /// Move every dropped path into the directory the explorer is
    /// currently pointing at. Files outside the workspace are *moved* in
    /// (matching the user's Finder-drop expectation), not copied — they
    /// disappear from the source location and re-appear in the explorer.
    fn import_paths_into_explorer(&mut self, paths: &[PathBuf]) {
        let dest_dir = self.paste_target_dir();
        let total = paths.len();
        let mut placed: Vec<PathBuf> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut affected: BTreeSet<PathBuf> = BTreeSet::new();
        affected.insert(dest_dir.clone());
        for src in paths {
            if let Some(parent) = src.parent() {
                affected.insert(parent.to_path_buf());
            }
            match crate::widgets::file_tree::move_into(&dest_dir, src) {
                Ok(p) => placed.push(p),
                Err(e) => errors.push(format!("{}: {e}", src.display())),
            }
        }
        for dir in &affected {
            if let Some(idx) = self.tree.index_of_dir(dir) {
                self.tree.refresh_children(idx);
            }
        }
        self.tree.marked.clear();
        for p in &placed {
            self.tree.marked.insert(p.clone());
        }
        if let Some(first) = placed.first()
            && let Some(idx) = self.tree.nodes.iter().position(|n| &n.path == first)
        {
            self.tree.selected = idx;
            self.tree.anchor = idx;
        }
        self.status = if !errors.is_empty() {
            format!(
                "Imported {}/{}; failed: {}",
                placed.len(),
                total,
                errors.join("; ")
            )
        } else if total == 1 {
            format!(
                "Imported {} into {}",
                paths[0].display(),
                dest_dir.display()
            )
        } else {
            format!("Imported {total} items into {}", dest_dir.display())
        };
    }

    /// Queue every dropped path for an interactive SCP upload to the
    /// currently selected SSH target. The actual scp invocation happens
    /// in `main_loop` after suspending the alt-screen, so scp inherits
    /// the host shell's stdin / stdout / stderr — that means password
    /// prompts, FIDO touch requests, and host-key confirmations all work
    /// the way the user expects, and the user sees scp's progress and
    /// errors directly. After all uploads finish (success or failure),
    /// croft prompts for Enter and resumes the TUI.
    fn import_paths_into_remote(&mut self, paths: &[PathBuf]) {
        let Some(target) = self.remote.selected_target().cloned() else {
            self.status = String::from("Drop ignored: no Remote Explorer host selected");
            return;
        };
        if paths.is_empty() {
            return;
        }
        for src in paths {
            self.pending_scp_uploads.push(ScpUpload {
                alias: target.alias.clone(),
                src: src.clone(),
            });
        }
        self.status = format!(
            "Queued {} item(s) for scp copy to {} (you'll see scp's prompts next)…",
            paths.len(),
            target.alias,
        );
    }

    pub fn take_pending_scp_uploads(&mut self) -> Vec<ScpUpload> {
        std::mem::take(&mut self.pending_scp_uploads)
    }

    /// Service edge auto-scroll while a terminal drag-selection is held
    /// past the top/bottom edge. Called every main-loop wakeup; advances
    /// the viewport by one row at ~30 ms intervals (a readable ~33 rows/s)
    /// and re-pins the selection head to the new edge. Returns true when a
    /// scroll happened so the caller knows it owes a redraw.
    pub fn tick_terminal_autoscroll(&mut self) -> bool {
        let Some(state) = self.terminal_select_autoscroll else {
            return false;
        };
        if state.last.elapsed() < std::time::Duration::from_millis(30) {
            return false;
        }
        self.terminal_mut().autoscroll_select(state.dir, state.col);
        self.terminal_select_autoscroll = Some(TerminalSelectAutoScroll {
            last: std::time::Instant::now(),
            ..state
        });
        true
    }

    fn drop_relay_active(&self) -> bool {
        std::env::var_os("CROFT_DROP_RELAY_LOG").is_some()
            && std::env::var_os("CROFT_DROP_RELAY_INBOX").is_some()
    }

    fn request_remote_pulls(&mut self, paths: &[PathBuf]) {
        let dest_dir = self.paste_target_dir();
        let Some(log_path) = std::env::var_os("CROFT_DROP_RELAY_LOG").map(PathBuf::from) else {
            self.status = String::from("Drop relay not available on this remote session");
            return;
        };
        let mut lines = String::new();
        let mut staged: Vec<PendingRemotePull> = Vec::new();
        let now = std::time::Instant::now();
        for src in paths {
            let request_id = format!(
                "{}-{}",
                std::process::id(),
                now.elapsed().as_nanos().wrapping_add(staged.len() as u128),
            );
            let basename = src
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| String::from("dropped"));
            lines.push_str("pull\t");
            lines.push_str(&request_id);
            lines.push('\t');
            lines.push_str(&src.to_string_lossy());
            lines.push('\n');
            staged.push(PendingRemotePull {
                request_id,
                src_display: src.to_string_lossy().into_owned(),
                basename,
                dest_dir: dest_dir.clone(),
                started_at: now,
                kind: RemotePullKind::File,
            });
        }
        match append_to_relay_log(&log_path, &lines) {
            Ok(()) => {
                self.status = format!(
                    "Fetching {} item(s) from your local Mac via croft relay…",
                    staged.len(),
                );
                self.pending_remote_pulls.extend(staged);
            }
            Err(e) => {
                self.status = format!("Drop relay write failed: {e}");
            }
        }
    }

    /// Ask the local-croft drop pump to fetch the user's macOS clipboard
    /// and stage it on this remote box. The drained pull resolves to a
    /// `paste_input` call on the embedded terminal — see `drain_remote_pulls`.
    fn request_remote_clipboard(&mut self) {
        let Some(log_path) = std::env::var_os("CROFT_DROP_RELAY_LOG").map(PathBuf::from) else {
            self.status = String::from("Cmd+V: drop relay vanished");
            return;
        };
        let request_id = format!(
            "clip-{}-{}",
            std::process::id(),
            std::time::Instant::now().elapsed().as_nanos(),
        );
        let mut line = String::from("clipboard\t");
        line.push_str(&request_id);
        line.push('\n');
        match append_to_relay_log(&log_path, &line) {
            Ok(()) => {
                self.pending_remote_pulls.push(PendingRemotePull {
                    request_id,
                    src_display: String::from("clipboard"),
                    basename: String::from("clipboard.txt"),
                    dest_dir: PathBuf::new(),
                    started_at: std::time::Instant::now(),
                    kind: RemotePullKind::Clipboard,
                });
                self.status = String::from("Cmd+V: fetching local Mac clipboard via croft relay…");
            }
            Err(e) => {
                self.status = format!("Cmd+V: relay write failed: {e}");
            }
        }
    }

    /// Check the relay inbox for completed pulls and surface them in the
    /// explorer. Returns true if any pending pull resolved (success,
    /// failure, or timeout) so the main loop knows to redraw.
    pub fn drain_remote_pulls(&mut self) -> bool {
        if self.pending_remote_pulls.is_empty() {
            return false;
        }
        let Some(inbox) = std::env::var_os("CROFT_DROP_RELAY_INBOX").map(PathBuf::from) else {
            self.pending_remote_pulls.clear();
            self.status = String::from("Drop relay vanished mid-pull");
            return true;
        };
        let mut still_pending: Vec<PendingRemotePull> = Vec::new();
        let mut placed: Vec<PathBuf> = Vec::new();
        let mut opened_urls: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut affected: BTreeSet<PathBuf> = BTreeSet::new();
        let pulls = std::mem::take(&mut self.pending_remote_pulls);
        let mut clipboard_payload: Option<Vec<u8>> = None;
        for pull in pulls {
            let request_dir = inbox.join(&pull.request_id);
            let ok = request_dir.join(".ok");
            let err = request_dir.join(".err");
            if ok.exists() {
                match pull.kind {
                    RemotePullKind::File => {
                        let staged_src = request_dir.join(&pull.basename);
                        affected.insert(pull.dest_dir.clone());
                        match crate::widgets::file_tree::move_into(&pull.dest_dir, &staged_src) {
                            Ok(p) => placed.push(p),
                            Err(e) => errors.push(format!("{}: {e}", pull.src_display)),
                        }
                    }
                    RemotePullKind::Clipboard => {
                        let staged = request_dir.join("clipboard.txt");
                        match std::fs::read(&staged) {
                            Ok(bytes) => clipboard_payload = Some(bytes),
                            Err(e) => errors.push(format!("clipboard relay: {e}")),
                        }
                    }
                    RemotePullKind::Open => {
                        opened_urls.push(pull.src_display.clone());
                    }
                }
                let _ = std::fs::remove_dir_all(&request_dir);
            } else if err.exists() {
                let msg =
                    std::fs::read_to_string(&err).unwrap_or_else(|_| String::from("relay error"));
                errors.push(format!("{}: {}", pull.src_display, msg.trim()));
                let _ = std::fs::remove_dir_all(&request_dir);
            } else if pull.started_at.elapsed() > REMOTE_PULL_TIMEOUT {
                errors.push(format!("{}: timed out after 120s", pull.src_display));
                let _ = std::fs::remove_dir_all(&request_dir);
            } else {
                still_pending.push(pull);
            }
        }
        if let Some(bytes) = clipboard_payload
            && !bytes.is_empty()
        {
            self.terminal_mut().paste_input(&bytes);
        }
        let resolved_any = !placed.is_empty() || !opened_urls.is_empty() || !errors.is_empty();
        self.pending_remote_pulls = still_pending;
        for dir in &affected {
            if let Some(idx) = self.tree.index_of_dir(dir) {
                self.tree.refresh_children(idx);
            }
        }
        if !placed.is_empty() {
            self.tree.marked.clear();
            for p in &placed {
                self.tree.marked.insert(p.clone());
            }
            if let Some(first) = placed.first()
                && let Some(idx) = self.tree.nodes.iter().position(|n| &n.path == first)
            {
                self.tree.selected = idx;
                self.tree.anchor = idx;
            }
        }
        if resolved_any {
            self.status = if errors.is_empty() {
                if !opened_urls.is_empty() && placed.is_empty() {
                    if opened_urls.len() == 1 {
                        format!("Opened {} in your local browser", opened_urls[0])
                    } else {
                        format!("Opened {} URL(s) in your local browser", opened_urls.len())
                    }
                } else if placed.len() == 1 {
                    format!("Pulled {} from your Mac", placed[0].display())
                } else {
                    format!("Pulled {} item(s) from your Mac", placed.len())
                }
            } else if placed.is_empty() && opened_urls.is_empty() {
                format!("Drop relay failed: {}", errors.join("; "))
            } else {
                format!(
                    "Pulled {} / opened {}; failures: {}",
                    placed.len(),
                    opened_urls.len(),
                    errors.join("; "),
                )
            };
        }
        resolved_any
    }

    pub fn report_scp_results(
        &mut self,
        moved: usize,
        total: usize,
        errors: usize,
        affected_dirs: &[PathBuf],
    ) {
        for dir in affected_dirs {
            if let Some(idx) = self.tree.index_of_dir(dir) {
                self.tree.refresh_children(idx);
            }
        }
        self.status = if errors == 0 {
            format!("Uploaded {moved}/{total} via scp")
        } else {
            format!("Uploaded {moved}/{total} via scp; {errors} error(s)")
        };
    }

    fn welcome_link_at(&self, col: u16, row: u16) -> Option<&WelcomeLink> {
        self.welcome
            .links()
            .iter()
            .find(|link| rect_contains(link.rect, col, row))
    }

    fn activate_welcome_link(&mut self, col: u16, row: u16) -> bool {
        let Some(link) = self.welcome_link_at(col, row).cloned() else {
            return false;
        };
        // Remote-launched croft has no working `xdg-open`, and even if it
        // did, the user wants the URL on their *local* Mac browser. Route
        // it through the drop-relay back to local croft. First click in
        // a session pops a confirmation; subsequent clicks go silently
        // once the user has chosen "Always".
        if self.drop_relay_active() {
            if self.trust_local_browser {
                self.request_remote_url_open(link.url.clone());
                self.status = format!("Opening {} on your local Mac…", link.label);
            } else {
                self.pending_local_open = Some(link.url.clone());
            }
            return true;
        }
        match open_url(&link.url) {
            Ok(()) => {
                self.status = link.label;
            }
            Err(e) => {
                self.status = format!("Open link failed: {e}");
            }
        }
        true
    }

    fn request_remote_url_open(&mut self, url: String) {
        let Some(log_path) = std::env::var_os("CROFT_DROP_RELAY_LOG").map(PathBuf::from) else {
            self.status = String::from("Open link: drop relay vanished");
            return;
        };
        let request_id = format!(
            "open-{}-{}",
            std::process::id(),
            std::time::Instant::now().elapsed().as_nanos(),
        );
        let mut line = String::from("open\t");
        line.push_str(&request_id);
        line.push('\t');
        line.push_str(&url);
        line.push('\n');
        match append_to_relay_log(&log_path, &line) {
            Ok(()) => {
                self.pending_remote_pulls.push(PendingRemotePull {
                    request_id,
                    src_display: url,
                    basename: String::new(),
                    dest_dir: PathBuf::new(),
                    started_at: std::time::Instant::now(),
                    kind: RemotePullKind::Open,
                });
            }
            Err(e) => {
                self.status = format!("Open link: relay write failed: {e}");
            }
        }
    }

    /// Handle a key while the local-browser confirmation modal is open.
    /// `Y`/`Enter` opens this URL once. `A` opens it AND remembers
    /// "always" for the rest of the session. `N`/`Esc` cancels.
    fn handle_local_open_confirm_key(&mut self, key: KeyEvent) {
        let Some(url) = self.pending_local_open.clone() else {
            return;
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.pending_local_open = None;
                self.request_remote_url_open(url.clone());
                self.status = format!("Opening {url} on your local Mac…");
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.pending_local_open = None;
                self.trust_local_browser = true;
                self.request_remote_url_open(url.clone());
                self.status = format!("Trusted local browser for this session. Opening {url}…");
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.pending_local_open = None;
                self.status = String::from("Open link cancelled");
            }
            _ => {}
        }
    }

    fn open_shortcuts_modal(&mut self) {
        if self.shortcuts_modal.is_none() {
            self.shortcuts_modal = Some(crate::widgets::shortcuts::ShortcutsModal::default());
            self.overlays.shortcuts_clear.request();
            self.status = String::from("Shortcuts: Esc to close");
        }
    }

    fn close_shortcuts_modal(&mut self) {
        if self.shortcuts_modal.take().is_some() {
            self.overlays.shortcuts_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    pub fn consume_shortcuts_image_clear(&mut self) -> bool {
        self.overlays.shortcuts_clear.consume()
    }

    pub fn consume_file_finder_image_clear(&mut self) -> bool {
        self.overlays.file_finder_clear.consume()
    }

    /// Drain the background-build receiver into `file_finder_index`
    /// without blocking. Called from `open_file_finder` and from the
    /// main loop tick. Accepts REPLACEMENT results too: when the FS
    /// watcher or a workspace re-root has triggered a rebuild, the
    /// fresh index swaps in here and is pushed into the open finder
    /// (if any) so an in-progress Cmd+P session immediately re-ranks
    /// against the new file set. If a follow-up rebuild was queued
    /// while this one was in flight (the `file_finder_index_dirty`
    /// flag), it is re-kicked here so a long-running build on a
    /// multi-GB monorepo can never miss a later FS event.
    fn poll_file_finder_index(&mut self) {
        let Some(rx) = self.file_finder_index_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(entries) => {
                let arc = std::sync::Arc::new(entries);
                self.file_finder_index = Some(arc.clone());
                self.file_finder_index_rx = None;
                if let Some(finder) = self.file_finder.as_mut() {
                    finder.replace_entries(arc);
                }
                if self.file_finder_index_dirty {
                    self.kick_file_finder_index_rebuild();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.file_finder_index_rx = None;
            }
        }
    }

    /// Spawn a background walker that rebuilds the Cmd+P file index
    /// against the current `workspace_root`. If a rebuild is already
    /// in flight, sets the dirty flag instead; `poll_file_finder_index`
    /// will re-kick on completion. Called from `change_workspace_root`
    /// so Cmd+P reflects the new project, and from `drain_fs_events`
    /// so creates, deletes, and git-checkout-driven swaps land in the
    /// index without a croft restart.
    fn kick_file_finder_index_rebuild(&mut self) {
        if self.file_finder_index_rx.is_some() {
            self.file_finder_index_dirty = true;
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<Vec<crate::widgets::file_finder::FileEntry>>();
        let root = self.workspace_root.clone();
        std::thread::spawn(move || {
            let entries = crate::widgets::file_finder::build_file_index(&root);
            let _ = tx.send(entries);
        });
        self.file_finder_index_rx = Some(rx);
        self.file_finder_index_dirty = false;
    }

    fn open_file_finder(&mut self) {
        if self.file_finder.is_some() {
            return;
        }
        // Drain the background-built index if it has landed.
        self.poll_file_finder_index();
        // Fallback: the worker thread has not finished yet AND no
        // index has ever been cached. Walk synchronously this once
        // rather than open an empty finder; this only fires when
        // Cmd+P is pressed within the first few hundred ms of launch.
        if self.file_finder_index.is_none() {
            let entries = crate::widgets::file_finder::build_file_index(&self.workspace_root);
            self.file_finder_index = Some(std::sync::Arc::new(entries));
            self.file_finder_index_rx = None;
        }
        let entries = self
            .file_finder_index
            .clone()
            .unwrap_or_else(|| std::sync::Arc::new(Vec::new()));
        let count = entries.len();
        self.file_finder = Some(crate::widgets::file_finder::FileFinder::new(entries));
        self.overlays.file_finder_clear.request();
        self.status = format!("Go to File: {count} files indexed — Esc to close");
    }

    fn close_file_finder(&mut self) {
        if self.file_finder.take().is_some() {
            self.overlays.file_finder_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    fn handle_file_finder_key(&mut self, key: KeyEvent) {
        // Cmd+V / Ctrl+V: paste the system clipboard into the query as if
        // each character had been typed. Hosts that route Cmd+V through
        // bracketed paste (iTerm2 default) deliver an `Event::Paste(s)`
        // instead, which `handle_paste` forwards to the finder — but if
        // the user re-bound Cmd+V to a CSI-u Cmd+V sequence or is running
        // in a terminal that disables bracketed paste, the chord arrives
        // here as a key event, and reading the clipboard ourselves keeps
        // the "open finder, paste a filename" gesture working everywhere.
        if is_clipboard_paste_key(key) {
            if let Some(text) = (self.clipboard_reader)()
                && let Some(finder) = self.file_finder.as_mut()
            {
                for c in text.chars() {
                    if !c.is_control() {
                        finder.push_char(c);
                    }
                }
            }
            return;
        }
        let Some(finder) = self.file_finder.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.close_file_finder();
            }
            (KeyCode::Enter, _) => {
                let path = finder.selected_entry().map(|e| e.path.clone());
                if let Some(path) = path {
                    self.close_file_finder();
                    match self.editor.open_preview(&path) {
                        Ok(()) => {
                            self.sync_open_file_poll_mtime();
                            // VS Code's `explorer.autoReveal` analogue:
                            // expand every parent dir of the picked file
                            // and park the cursor on its row so the user
                            // sees where in the workspace the file lives.
                            self.tree.reveal_path(&path);
                            self.focus_pane(Pane::Editor);
                            self.status = format!("Opened {}", path.display());
                        }
                        Err(e) => {
                            self.status = format!("Open failed: {e}");
                        }
                    }
                }
            }
            (KeyCode::Up, _) => finder.select_prev(),
            (KeyCode::Down, _) => finder.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    finder.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    finder.select_next();
                }
            }
            (KeyCode::Left, _) => finder.move_cursor_left(),
            (KeyCode::Right, _) => finder.move_cursor_right(),
            (KeyCode::Home, _) => finder.move_cursor_home(),
            (KeyCode::End, _) => finder.move_cursor_end(),
            (KeyCode::Backspace, _) => finder.pop_char(),
            (KeyCode::Delete, _) => finder.delete_char(),
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                finder.push_char(c);
            }
            _ => {}
        }
    }

    fn handle_file_finder_mouse(&mut self, m: MouseEvent) {
        let Some(finder) = self.file_finder.as_mut() else {
            return;
        };
        let inside = rect_contains(finder.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    finder.select_next();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    finder.select_prev();
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_file_finder();
            }
            _ => {}
        }
    }

    fn render_file_finder(&mut self, frame: &mut ratatui::Frame) {
        let gradient = self.popup_gradient();
        let Some(finder) = self.file_finder.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::file_finder::render_file_finder(finder, area, frame.buffer_mut(), gradient);
    }

    pub fn consume_command_palette_image_clear(&mut self) -> bool {
        self.overlays.command_palette_clear.consume()
    }

    /// Open the Command Palette (Cmd/Ctrl+Shift+P). The palette is built from
    /// the static command registry, so there is nothing to index — it opens
    /// instantly regardless of workspace size.
    fn open_command_palette(&mut self) {
        if self.command_palette.is_some() {
            return;
        }
        let palette = crate::widgets::command_palette::CommandPalette::new();
        let count = palette.results.len();
        self.command_palette = Some(palette);
        self.overlays.command_palette_clear.request();
        self.status = format!("Command Palette: {count} commands — Esc to close");
    }

    fn close_command_palette(&mut self) {
        if self.command_palette.take().is_some() {
            self.overlays.command_palette_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    fn handle_command_palette_key(&mut self, key: KeyEvent) {
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_command_palette(),
            (KeyCode::Enter, _) => {
                let cmd = palette.selected_command();
                self.close_command_palette();
                if let Some(cmd) = cmd {
                    self.run_command(cmd);
                }
            }
            (KeyCode::Up, _) => palette.select_prev(),
            (KeyCode::Down, _) => palette.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    palette.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    palette.select_next();
                }
            }
            (KeyCode::Left, _) => palette.move_cursor_left(),
            (KeyCode::Right, _) => palette.move_cursor_right(),
            (KeyCode::Home, _) => palette.move_cursor_home(),
            (KeyCode::End, _) => palette.move_cursor_end(),
            (KeyCode::Backspace, _) => palette.pop_char(),
            (KeyCode::Delete, _) => palette.delete_char(),
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                palette.push_char(c);
            }
            _ => {}
        }
    }

    fn handle_command_palette_mouse(&mut self, m: MouseEvent) {
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        let inside = rect_contains(palette.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    palette.select_next();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    palette.select_prev();
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_command_palette();
            }
            _ => {}
        }
    }

    fn render_command_palette(&mut self, frame: &mut ratatui::Frame) {
        let gradient = self.popup_gradient();
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::command_palette::render_command_palette(
            palette,
            area,
            frame.buffer_mut(),
            gradient,
        );
    }

    pub fn consume_process_picker_image_clear(&mut self) -> bool {
        self.overlays.process_picker_clear.consume()
    }

    /// Open the "Debug: Attach to Python Process" picker. Enumerates running
    /// CPython 3.14+ processes (those with PEP 768 `sys.remote_exec`); shows a
    /// status message and opens nothing when there are none.
    fn open_attach_python_picker(&mut self) {
        if self.process_picker.is_some() {
            return;
        }
        let targets = crate::dap::discovery::attachable_python_targets();
        if targets.is_empty() {
            self.run_debug.feedback = Some(String::from(
                "No attachable Python 3.14+ processes found (PEP 768 needs CPython 3.14+)",
            ));
            self.run_debug.feedback_is_error = true;
            self.status = String::from("Attach: no Python 3.14+ processes running");
            return;
        }
        let count = targets.len();
        self.process_picker = Some(crate::widgets::process_picker::ProcessPicker::new(targets));
        self.overlays.process_picker_clear.request();
        self.status = format!("Attach: {count} Python process(es) — Esc to close, Enter to attach");
    }

    fn close_process_picker(&mut self) {
        if self.process_picker.take().is_some() {
            self.overlays.process_picker_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    fn handle_process_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.process_picker.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_process_picker(),
            (KeyCode::Enter, _) => {
                let target = picker.selected_target().cloned();
                self.close_process_picker();
                if let Some(target) = target {
                    self.attach_to_python_target(target);
                }
            }
            (KeyCode::Up, _) => picker.select_prev(),
            (KeyCode::Down, _) => picker.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    picker.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    picker.select_next();
                }
            }
            _ => {}
        }
    }

    fn handle_process_picker_mouse(&mut self, m: MouseEvent) {
        let Some(picker) = self.process_picker.as_mut() else {
            return;
        };
        let inside = rect_contains(picker.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    picker.select_next();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    picker.select_prev();
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_process_picker();
            }
            _ => {}
        }
    }

    fn render_process_picker(&mut self, frame: &mut ratatui::Frame) {
        let gradient = self.popup_gradient();
        let Some(picker) = self.process_picker.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::process_picker::render_process_picker(
            picker,
            area,
            frame.buffer_mut(),
            gradient,
        );
    }

    /// Spawn `pdb -p <pid>` for the chosen target in a new PTY terminal,
    /// wrapping in `sudo` when the platform requires elevation (always on
    /// macOS; on Linux unless Yama `ptrace_scope` is relaxed). The PTY hosts
    /// both the interactive pdb prompt and any `sudo` password prompt — the
    /// edge a GUI debugger lacks.
    fn attach_to_python_target(&mut self, target: crate::dap::discovery::PyTarget) {
        use crate::dap::remote_attach::{Platform, plan_attach, read_yama_ptrace_scope};
        let pid = target.pid;
        let plan = match plan_attach(
            target.version,
            target.exe,
            pid,
            Platform::current(),
            read_yama_ptrace_scope(),
        ) {
            Ok(plan) => plan,
            Err(e) => {
                self.run_debug.feedback = Some(format!("Cannot attach to PID {pid}: {e:?}"));
                self.run_debug.feedback_is_error = true;
                return;
            }
        };
        let (program, args) = plan.command_line();
        match PtyTerminal::new_running(&program, &args, &self.workspace_root) {
            Ok(term) => {
                let slot = self.active_terminal + 1;
                self.terminals.insert(slot, term);
                self.active_terminal = slot;
                if !self.show_terminal {
                    self.show_terminal = true;
                }
                self.focus_pane(Pane::Terminal);
                self.run_debug.feedback = Some(format!("Attaching debugger to PID {pid}"));
                self.run_debug.feedback_is_error = false;
            }
            Err(e) => {
                self.run_debug.feedback = Some(format!("Failed to spawn terminal: {e}"));
                self.run_debug.feedback_is_error = true;
            }
        }
    }

    /// Execute a Command Palette entry. Editor text commands act on the active
    /// editor buffer; view / navigation commands defer to the same methods
    /// their dedicated chords use, so the palette can never drift from the
    /// keyboard surface.
    fn run_command(&mut self, cmd: crate::widgets::command_palette::Command) {
        use crate::widgets::command_palette::Command as Cmd;
        use crate::widgets::editor::CaseTransform;
        match cmd {
            Cmd::MoveLineUp => self.editor.move_lines_up(),
            Cmd::MoveLineDown => self.editor.move_lines_down(),
            Cmd::ToggleLineComment => {
                if !self.editor.toggle_line_comment() {
                    self.status = String::from("Toggle Line Comment: no comment for this language");
                }
            }
            Cmd::ToggleBlockComment => {
                if !self.editor.toggle_block_comment() {
                    self.status =
                        String::from("Toggle Block Comment: no block comment for this language");
                }
            }
            Cmd::JoinLines => self.editor.join_lines(),
            Cmd::TransformUpper => self.editor.transform_selection_case(CaseTransform::Upper),
            Cmd::TransformLower => self.editor.transform_selection_case(CaseTransform::Lower),
            Cmd::TransformTitle => self.editor.transform_selection_case(CaseTransform::Title),
            Cmd::SortLinesAscending => self.editor.sort_lines(true),
            Cmd::SortLinesDescending => self.editor.sort_lines(false),
            Cmd::TrimTrailingWhitespace => {
                if self.editor.trim_trailing_whitespace() {
                    self.status = String::from("Trimmed trailing whitespace");
                }
            }
            Cmd::ToggleWordWrap => {
                self.editor.toggle_wrap();
                self.status = if self.editor.wrap_enabled() {
                    String::from("Word wrap: on")
                } else {
                    String::from("Word wrap: off")
                };
            }
            Cmd::AddCursorAbove => self.editor.add_cursor_above(),
            Cmd::AddCursorBelow => self.editor.add_cursor_below(),
            Cmd::SaveFile => self.save(),
            Cmd::CloseEditor => {
                if self.editor.close_active() {
                    self.sync_open_file_poll_mtime();
                    self.collapse_split_if_empty();
                    self.status = String::from("Closed tab");
                }
            }
            Cmd::SplitEditor => self.split_editor(),
            Cmd::QuickOpen => self.open_file_finder(),
            Cmd::ToggleVimMode => self.toggle_vim_mode(),
            Cmd::ShowExplorer => self.set_sidebar_view(SidebarView::Explorer),
            Cmd::ShowSearch => self.set_sidebar_view(SidebarView::Search),
            Cmd::ShowSourceControl => self.set_sidebar_view(SidebarView::SourceControl),
            Cmd::ShowRunDebug => self.set_sidebar_view(SidebarView::RunDebug),
            Cmd::ShowRemote => self.set_sidebar_view(SidebarView::Remote),
            Cmd::ToggleSideBar => self.show_tree = !self.show_tree,
            Cmd::ToggleTerminal => self.toggle_terminal(),
            Cmd::NewTerminal => match self.split_terminal() {
                Ok(()) => {
                    self.status = format!("Split terminal: {} active", self.terminals.len());
                }
                Err(e) => {
                    self.status = format!("Split terminal failed: {e}");
                }
            },
            Cmd::StartDebugging => self.debug_start_or_continue(),
            Cmd::StopDebugging => self.debug_stop(),
            Cmd::PauseDebugging => self.debug_pause(),
            Cmd::RestartDebugging => self.debug_restart(),
            Cmd::ToggleBreakpoint => self.debug_toggle_breakpoint(),
            Cmd::EditBreakpointCondition => self.debug_edit_condition(),
            Cmd::StepOver => self.debug_step("next"),
            Cmd::ToggleRaisedExceptions => self.debug_toggle_raised_exceptions(),
            Cmd::AttachPythonProcess => self.open_attach_python_picker(),
            Cmd::ColorTheme => self.open_theme_picker(),
            Cmd::KeyboardShortcuts => self.open_shortcuts_modal(),
        }
    }

    pub fn consume_zoxide_jump_image_clear(&mut self) -> bool {
        self.overlays.zoxide_jump_clear.consume()
    }

    /// Open the zoxide jump popup and seed it with the full frecency list
    /// (`zoxide query -l`), so it shows the user's top directories before
    /// they type anything — like pressing `j<Tab>` in the shell.
    fn open_zoxide_jump(&mut self) {
        if self.zoxide_jump.is_some() {
            return;
        }
        let mut jump = crate::widgets::zoxide_jump::ZoxideJump::new();
        jump.install_state = crate::zoxide::install_state();
        jump.termux = crate::iterm2_inline::detect_termux();
        jump.set_results(crate::zoxide::query(""));
        // Snapshot the full frecency list so the typo-tolerant fuzzy
        // fallback can rank against it without re-spawning zoxide on every
        // missed keystroke.
        jump.set_all_dirs(jump.results.clone());
        let status = if jump.available {
            format!(
                "Jump: {} directories — type to filter, Esc to close",
                jump.results.len()
            )
        } else {
            // Same honest copy as the popup body (single source of truth).
            format!(
                "Jump: {}",
                crate::widgets::zoxide_jump::unavailable_message(jump.install_state, jump.termux)
                    .trim_start()
            )
        };
        self.zoxide_jump = Some(jump);
        self.overlays.zoxide_jump_clear.request();
        self.status = status;
    }

    fn close_zoxide_jump(&mut self) {
        if self.zoxide_jump.take().is_some() {
            self.overlays.zoxide_jump_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    /// Re-run `zoxide query` for the popup's current text and feed the
    /// ranked matches back to the widget. The one subprocess per keystroke
    /// is cheap (zoxide reads a small frecency DB; single-digit ms) and
    /// fires only on input, never on the render hot path.
    ///
    /// Two layers: the strict `zoxide query` is authoritative and handles
    /// the common case. Only when it returns zero matches for a non-empty
    /// needle does the typo-tolerant `fuzzy_rank` fallback run, ranking the
    /// cached frecency DB by edit distance so a transposition like `spilt`
    /// still finds `pr-split` (zoxide's own subsequence matcher cannot).
    /// The fallback is purely in-memory, so it never spawns zoxide twice.
    fn refresh_zoxide_results(&mut self) {
        let Some(jump) = self.zoxide_jump.as_mut() else {
            return;
        };
        let needle = jump.query.clone();
        // The background install may have advanced (Running -> Done/Failed)
        // since the popup opened; keep the unavailable message current.
        jump.install_state = crate::zoxide::install_state();
        match crate::zoxide::query(&needle) {
            Some(paths) if paths.is_empty() && !needle.trim().is_empty() => {
                let ranked = crate::zoxide::fuzzy_rank(&needle, &jump.all_dirs);
                if ranked.is_empty() {
                    jump.set_results(Some(Vec::new()));
                } else {
                    jump.set_approximate_results(ranked);
                }
            }
            other => jump.set_results(other),
        }
    }

    fn handle_zoxide_jump_key(&mut self, key: KeyEvent) {
        let Some(jump) = self.zoxide_jump.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.close_zoxide_jump();
            }
            (KeyCode::Enter, _) => {
                let path = jump.selected_path().map(|p| p.to_path_buf());
                if let Some(path) = path {
                    self.close_zoxide_jump();
                    self.change_workspace_root(path);
                }
            }
            (KeyCode::Up, _) => jump.select_prev(),
            (KeyCode::Down, _) => jump.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    jump.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    jump.select_next();
                }
            }
            (KeyCode::Left, _) => jump.move_cursor_left(),
            (KeyCode::Right, _) => jump.move_cursor_right(),
            (KeyCode::Home, _) => jump.move_cursor_home(),
            (KeyCode::End, _) => jump.move_cursor_end(),
            (KeyCode::Backspace, _) => {
                jump.pop_char();
                self.refresh_zoxide_results();
            }
            (KeyCode::Delete, _) => {
                jump.delete_char();
                self.refresh_zoxide_results();
            }
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                jump.push_char(c);
                self.refresh_zoxide_results();
            }
            _ => {}
        }
    }

    fn handle_zoxide_jump_mouse(&mut self, m: MouseEvent) {
        let Some(jump) = self.zoxide_jump.as_mut() else {
            return;
        };
        let inside = rect_contains(jump.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    jump.select_next();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    jump.select_prev();
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_zoxide_jump();
            }
            _ => {}
        }
    }

    fn render_zoxide_jump(&mut self, frame: &mut ratatui::Frame) {
        let sidebar = self.last_sidebar_area;
        let full = frame.area();
        let gradient = self.popup_gradient();
        let Some(jump) = self.zoxide_jump.as_mut() else {
            return;
        };
        // Anchor the popup over the Explorer column (it is an Explorer
        // action), dropping from the sidebar's top-left. The sidebar is
        // narrow, so widen the box rightward over the editor for readable
        // paths, capped to the frame. When the sidebar is collapsed there
        // is nothing to anchor to, so fall back to a centered box.
        let rect = match sidebar {
            Some(side) if side.width >= 2 && side.height >= 4 => {
                let width = side
                    .width
                    .max(ZOXIDE_JUMP_MIN_WIDTH)
                    .min(full.width.saturating_sub(side.x));
                Rect {
                    x: side.x,
                    y: side.y,
                    width,
                    height: side.height,
                }
            }
            _ => {
                let width = (full.width.saturating_mul(7) / 10).clamp(40, 100.min(full.width));
                let height = (full.height.saturating_mul(6) / 10).clamp(10, full.height);
                Rect {
                    x: full.x + (full.width.saturating_sub(width)) / 2,
                    y: full.y + (full.height.saturating_sub(height)) / 4,
                    width,
                    height,
                }
            }
        };
        crate::widgets::zoxide_jump::render_zoxide_jump(jump, rect, frame.buffer_mut(), gradient);
    }

    fn open_editor_find(&mut self) {
        if self.editor_find.is_some() {
            return;
        }
        let opts = self.search.opts;
        if self.editor.diff.is_some() {
            let initial = self
                .editor
                .diff
                .as_ref()
                .and_then(|d| d.selection_text())
                .filter(|s| !s.contains('\n'))
                .unwrap_or_default();
            self.editor_find = Some(crate::widgets::editor_find::EditorFind {
                query: String::new(),
                opts,
                ..Default::default()
            });
            if !initial.is_empty() {
                self.diff_find_set_query(initial);
            }
            self.status =
                String::from("Find: type to search, Enter next, Shift+Enter prev, Esc close");
            return;
        }
        let initial = if !self.editor.selection_text().is_empty()
            && !self.editor.selection_text().contains('\n')
        {
            self.editor.selection_text()
        } else {
            self.editor.word_before_cursor()
        };
        let mut state = crate::widgets::editor_find::EditorFind {
            query: initial.clone(),
            opts,
            ..Default::default()
        };
        if !initial.is_empty() {
            state.match_count =
                crate::widgets::editor_find::count_matches(&self.editor.lines, &initial, opts);
            self.editor
                .set_search_highlight(Some(initial.clone()), opts);
        }
        self.editor_find = Some(state);
        self.status = String::from("Find: type to search, Enter next, Shift+Enter prev, Esc close");
    }

    fn close_editor_find(&mut self) {
        if self.editor_find.take().is_some() {
            self.editor.set_search_highlight(None, self.search.opts);
            if let Some(diff) = self.editor.diff.as_mut() {
                diff.find.needle = None;
                diff.find.active = None;
            }
            self.status.clear();
        }
    }

    /// Viewport geometry (visible rows, visible text columns) of the diff
    /// body, derived from the last rendered inner rect the same way
    /// `render_diff` lays the columns out. Used to keep a jumped-to match
    /// on screen.
    fn diff_find_viewport(&self) -> (usize, usize) {
        let inner = self.editor.last_inner;
        let viewport_rows = (inner.height as usize).saturating_sub(2);
        let half = inner.width / 2;
        let text_cols = match self.editor.diff.as_ref() {
            Some(diff) => {
                let r_gutter = (diff.right_lines.len() + 1).to_string().len() as u16 + 1;
                inner
                    .width
                    .saturating_sub(half + 1)
                    .saturating_sub(r_gutter + 2) as usize
            }
            None => 0,
        };
        (viewport_rows, text_cols)
    }

    /// Recompute the diff's matches for the current query, select the
    /// match at `target` (wrapping), scroll it into view, and sync the
    /// find bar's count / index. Drives every diff find action.
    fn diff_find_apply(&mut self, target: usize) {
        let (needle, opts) = match self.editor_find.as_ref() {
            Some(s) => (s.query.clone(), s.opts),
            None => return,
        };
        let (viewport_rows, text_cols) = self.diff_find_viewport();
        let Some(diff) = self.editor.diff.as_mut() else {
            return;
        };
        diff.find.opts = opts;
        if needle.is_empty() {
            diff.find.needle = None;
            diff.find.active = None;
            if let Some(s) = self.editor_find.as_mut() {
                s.match_count = 0;
                s.match_index = None;
            }
            return;
        }
        diff.find.needle = Some(needle.clone());
        let matches = diff.find_matches(&needle, opts);
        let count = matches.len();
        if count == 0 {
            diff.find.active = None;
            if let Some(s) = self.editor_find.as_mut() {
                s.match_count = 0;
                s.match_index = None;
            }
            return;
        }
        let pos = target % count;
        let m = matches[pos];
        diff.find.active = Some(m);
        diff.scroll_to_match(m, viewport_rows, text_cols);
        if let Some(s) = self.editor_find.as_mut() {
            s.match_count = count;
            s.match_index = Some(pos + 1);
        }
    }

    fn diff_find_set_query(&mut self, new_query: String) {
        if let Some(s) = self.editor_find.as_mut() {
            s.query = new_query;
        }
        self.diff_find_apply(0);
    }

    fn diff_find_current_pos(&self) -> usize {
        self.editor_find
            .as_ref()
            .and_then(|s| s.match_index)
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0)
    }

    fn editor_find_set_query(&mut self, new_query: String) {
        let unchanged = self
            .editor_find
            .as_ref()
            .map(|s| s.query == new_query)
            .unwrap_or(true);
        if unchanged {
            return;
        }
        if self.editor.diff.is_some() {
            self.diff_find_set_query(new_query);
            return;
        }
        let Some(state) = self.editor_find.as_mut() else {
            return;
        };
        state.query = new_query;
        let opts = state.opts;
        state.match_count =
            crate::widgets::editor_find::count_matches(&self.editor.lines, &state.query, opts);
        if state.query.is_empty() {
            state.match_index = None;
            self.editor.active_search_match = None;
            self.editor.set_search_highlight(None, opts);
            return;
        }
        self.editor
            .set_search_highlight(Some(state.query.clone()), opts);
        let from_row = self.editor.cursor_row;
        let from_col = self.editor.cursor_col;
        if let Some(m) = crate::widgets::editor_find::find_next_match(
            &self.editor.lines,
            &state.query,
            opts,
            from_row,
            from_col,
            false,
        ) {
            self.jump_editor_to_match(m);
        }
        self.refresh_editor_find_index();
    }

    fn editor_find_jump_next(&mut self) {
        let Some(state) = self.editor_find.as_ref() else {
            return;
        };
        if state.query.is_empty() {
            return;
        }
        if self.editor.diff.is_some() {
            self.diff_find_apply(self.diff_find_current_pos() + 1);
            return;
        }
        let opts = state.opts;
        let needle = state.query.clone();
        if let Some(m) = crate::widgets::editor_find::find_next_match(
            &self.editor.lines,
            &needle,
            opts,
            self.editor.cursor_row,
            self.editor.cursor_col,
            true,
        ) {
            self.jump_editor_to_match(m);
            self.refresh_editor_find_index();
        }
    }

    fn editor_find_jump_prev(&mut self) {
        let Some(state) = self.editor_find.as_ref() else {
            return;
        };
        if state.query.is_empty() {
            return;
        }
        if self.editor.diff.is_some() {
            let count = state.match_count;
            if count == 0 {
                return;
            }
            self.diff_find_apply(self.diff_find_current_pos() + count - 1);
            return;
        }
        let opts = state.opts;
        let needle = state.query.clone();
        if let Some(m) = crate::widgets::editor_find::find_prev_match(
            &self.editor.lines,
            &needle,
            opts,
            self.editor.cursor_row,
            self.editor.cursor_col,
            true,
        ) {
            self.jump_editor_to_match(m);
            self.refresh_editor_find_index();
        }
    }

    fn jump_editor_to_match(&mut self, m: crate::widgets::editor_find::MatchPos) {
        self.editor.cursor_row = m.row;
        self.editor.cursor_col = m.col_chars;
        self.editor.active_search_match = Some((m.row, m.col_chars, m.len_chars));
        let viewport = self.editor.last_inner.height as usize;
        if viewport > 0 {
            if self.editor.cursor_row < self.editor.scroll {
                self.editor.scroll = self.editor.cursor_row;
            } else if self.editor.cursor_row >= self.editor.scroll + viewport {
                self.editor.scroll = self.editor.cursor_row + 1 - viewport;
            }
        }
        self.editor.ensure_cursor_col_visible();
    }

    fn refresh_editor_find_index(&mut self) {
        let Some(state) = self.editor_find.as_mut() else {
            return;
        };
        let opts = state.opts;
        let needle = state.query.clone();
        state.match_index = crate::widgets::editor_find::match_index_at(
            &self.editor.lines,
            &needle,
            opts,
            self.editor.cursor_row,
            self.editor.cursor_col,
        );
    }

    fn handle_editor_find_key(&mut self, key: KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_editor_find(),
            (KeyCode::Enter, _) => {
                if shift {
                    self.editor_find_jump_prev();
                } else {
                    self.editor_find_jump_next();
                }
            }
            (KeyCode::F(3), _) => {
                if shift {
                    self.editor_find_jump_prev();
                } else {
                    self.editor_find_jump_next();
                }
            }
            (KeyCode::Backspace, _) => {
                let mut new_q = self
                    .editor_find
                    .as_ref()
                    .map(|s| s.query.clone())
                    .unwrap_or_default();
                new_q.pop();
                self.editor_find_set_query(new_q);
            }
            (KeyCode::Char('v'), m)
                if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::SUPER) =>
            {
                if let Some(text) = (self.clipboard_reader)() {
                    let mut new_q = self
                        .editor_find
                        .as_ref()
                        .map(|s| s.query.clone())
                        .unwrap_or_default();
                    for c in text.chars() {
                        if !c.is_control() {
                            new_q.push(c);
                        }
                    }
                    self.editor_find_set_query(new_q);
                }
            }
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                let mut new_q = self
                    .editor_find
                    .as_ref()
                    .map(|s| s.query.clone())
                    .unwrap_or_default();
                new_q.push(c);
                self.editor_find_set_query(new_q);
            }
            _ => {}
        }
    }

    fn render_editor_find(&mut self, frame: &mut ratatui::Frame) {
        let gradient = self.popup_gradient();
        let Some(state) = self.editor_find.as_mut() else {
            return;
        };
        let area = self.editor.last_full_area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        crate::widgets::editor_find::render_editor_find(state, area, frame.buffer_mut(), gradient);
    }

    fn handle_shortcuts_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.shortcuts_modal.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::F(1), _) => self.close_shortcuts_modal(),
            (KeyCode::Char('q'), KeyModifiers::NONE) => self.close_shortcuts_modal(),
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => modal.scroll_down(1),
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => modal.scroll_up(1),
            (KeyCode::PageDown, _) | (KeyCode::Char(' '), _) => modal.scroll_down(10),
            (KeyCode::PageUp, _) => modal.scroll_up(10),
            (KeyCode::Home, _) | (KeyCode::Char('g'), _) => modal.scroll_to_top(),
            (KeyCode::End, _) | (KeyCode::Char('G'), _) => modal.scroll_to_bottom(),
            _ => {}
        }
    }

    fn handle_shortcuts_modal_mouse(&mut self, m: MouseEvent) {
        let Some(modal) = self.shortcuts_modal.as_mut() else {
            return;
        };
        let inside = rect_contains(modal.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => modal.scroll_down(3),
            MouseEventKind::ScrollUp => modal.scroll_up(3),
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_shortcuts_modal();
            }
            _ => {}
        }
    }

    fn render_shortcuts_modal(&mut self, frame: &mut ratatui::Frame) {
        let gradient = self.popup_gradient();
        let Some(modal) = self.shortcuts_modal.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::shortcuts::render_shortcuts_modal(
            modal,
            area,
            frame.buffer_mut(),
            gradient,
        );
    }

    fn render_connect_dialog(&mut self, frame: &mut ratatui::Frame) {
        let visible = self.cursor_visible_phase();
        let gradient = self.popup_gradient();
        let Some(dialog) = self.connect_dialog.as_mut() else {
            return;
        };
        dialog.gradient = gradient;
        let area = frame.area();
        use ratatui::widgets::Widget as _;
        dialog.render(area, frame.buffer_mut());
        if visible && let Some((cx, cy)) = dialog.cursor_screen_pos() {
            frame.set_cursor_position((cx, cy));
        }
    }

    /// A tap inside the on-screen keyboard band. Layer/modifier keys mutate
    /// the keyboard; character/named keys synthesize a `KeyEvent` and feed
    /// it through `handle_key`, so it reaches whatever currently owns the
    /// keyboard focus (editor, terminal, search, or an open modal).
    fn handle_osk_mouse(&mut self, m: MouseEvent) {
        if !matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }
        let Some(osk) = self.osk.as_mut() else {
            return;
        };
        let Some(key) = osk.key_at(m.column, m.row) else {
            return;
        };
        if key == crate::widgets::osk::OskKey::Hide {
            self.osk = None;
            return;
        }
        let ev = osk.tap(key);
        // Layer/modifier taps reshape the band; refresh the hit rects
        // immediately so a follow-up tap that lands before the next frame
        // hits the new layer, not the stale one.
        let band = osk.last_area;
        osk.layout(band);
        // The split choice outlives the session (best-effort, like themes).
        if key == crate::widgets::osk::OskKey::SplitToggle {
            let _ = crate::prefs::save_osk_split(osk.split);
        }
        if let Some(ev) = ev
            && let Err(e) = self.handle_key(ev)
        {
            self.status = format!("On-screen key failed: {e}");
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        // On-screen keyboard taps outrank every other gate - including the
        // modal overlays - because the OSK is how Termux users type into
        // those modals. Its band is laid out disjoint from all panes, so
        // swallowing events inside it never steals a pane's click.
        if self
            .osk
            .as_ref()
            .is_some_and(|k| rect_contains(k.last_area, m.column, m.row))
        {
            self.handle_osk_mouse(m);
            return;
        }
        if self.connect_dialog.is_some() {
            if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.handle_connect_dialog_click(m.column, m.row);
            }
            return;
        }
        if self.shortcuts_modal.is_some() {
            self.handle_shortcuts_modal_mouse(m);
            return;
        }
        if self.file_finder.is_some() {
            self.handle_file_finder_mouse(m);
            return;
        }
        if self.process_picker.is_some() {
            self.handle_process_picker_mouse(m);
            return;
        }
        if self.command_palette.is_some() {
            self.handle_command_palette_mouse(m);
            return;
        }
        if self.zoxide_jump.is_some() {
            self.handle_zoxide_jump_mouse(m);
            return;
        }
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && self
                .shortcuts_hit_rect
                .is_some_and(|r| rect_contains(r, m.column, m.row))
        {
            self.open_shortcuts_modal();
            return;
        }
        // While a prompt is open, mouse events are ignored.
        if self.prompt.is_some() {
            return;
        }

        // While a context menu is open, route clicks to it.
        if let Some(menu) = &self.context_menu {
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(idx) = self.menu_item_at(m.column, m.row) {
                        let action = menu.items[idx].1.clone();
                        let dir = menu.target_dir.clone();
                        self.context_menu = None;
                        self.dispatch_menu_action(action, dir);
                    } else {
                        // click outside the menu: dismiss
                        self.context_menu = None;
                    }
                    return;
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    self.context_menu = None;
                    // fall through so the right-click below can re-open if applicable
                }
                _ => return,
            }
        }

        // When the editor is split, a press anywhere in the NON-focused
        // column (except its draggable seam) moves focus there first, so
        // the rest of this handler - which all reads `self.editor` - then
        // operates on the clicked group. The swap keeps the physical
        // columns fixed.
        if self.editor_split.is_some()
            && matches!(m.kind, MouseEventKind::Down(_))
            && self.editor_splitter_x != Some(m.column)
            && self
                .editor_split
                .as_ref()
                .is_some_and(|s| rect_contains(s.last_full_area, m.column, m.row))
        {
            self.focus_editor_group(!self.split_focus_left);
        }

        // Hit-test the side panel against the ACTIVE widget's last_area
        // rather than always against `self.tree.last_area`. The tree is
        // only re-rendered when the Explorer view is active; switching
        // to Search / Source Control / Remote / Run-Debug freezes
        // `tree.last_area` at whatever rectangle the last Explorer
        // render captured, which can be smaller than the current side
        // panel and silently swallow clicks on rows below that rect.
        let active_sidebar_area = match self.sidebar_view {
            SidebarView::Explorer => self.tree.last_area,
            SidebarView::Search => self.search.last_area,
            SidebarView::SourceControl => self.source_control.last_area,
            SidebarView::Remote => self.remote.last_area,
            SidebarView::RunDebug => self.run_debug.last_area,
        };
        let in_tree = self.show_tree && rect_contains(active_sidebar_area, m.column, m.row);
        // The OUTLINE section sits below the tree in the Explorer sidebar and
        // scrolls independently, so wheel events over it must reach the panel
        // rather than the tree above.
        let in_outline = self.sidebar_view == SidebarView::Explorer
            && rect_contains(self.outline.last_area, m.column, m.row);
        let in_open_editors = self.sidebar_view == SidebarView::Explorer
            && rect_contains(self.open_editors.last_area, m.column, m.row);
        let in_timeline = self.sidebar_view == SidebarView::Explorer
            && rect_contains(self.timeline.last_area, m.column, m.row);
        let in_deps = self.sidebar_view == SidebarView::Explorer
            && rect_contains(self.dependencies.last_area, m.column, m.row);
        let in_editor_pane = rect_contains(self.editor.last_full_area, m.column, m.row);
        let in_editor = rect_contains(self.editor.last_area, m.column, m.row);
        let terminal_hit = self.terminal_at_pos(m.column, m.row);
        let in_terminal = terminal_hit.is_some();
        let in_tree_scrollbar = self.sidebar_view == SidebarView::Explorer
            && rect_contains(self.tree.last_scrollbar, m.column, m.row);
        let in_remote_scrollbar = self.sidebar_view == SidebarView::Remote
            && rect_contains(self.remote.last_scrollbar, m.column, m.row);
        let in_search_scrollbar = self.sidebar_view == SidebarView::Search
            && rect_contains(self.search.last_scrollbar, m.column, m.row);
        let in_editor_scrollbar = rect_contains(self.editor.last_scrollbar, m.column, m.row);
        let in_editor_hscrollbar = rect_contains(self.editor.last_hscrollbar, m.column, m.row);

        if matches!(m.kind, MouseEventKind::Moved) {
            self.pointer_cell = Some((m.column, m.row));
            self.on_mouse_moved(m.column, m.row, in_editor);
            return;
        }
        // The release of a press is not a new gesture, so it keeps the popup:
        // that is what lets a press-and-hold (touch) read the hover after
        // lifting the finger. Every other event dismisses it.
        if !matches!(m.kind, MouseEventKind::Up(_)) {
            self.hover_popup = None;
            self.hover_diagnostic = None;
        }
        // Any click dismisses the tab tooltip (selecting/closing a tab,
        // or clicking elsewhere) so it can't linger over the new state.
        self.tab_tooltip = None;
        self.tab_hover.clear();
        self.tab_hover_idx = None;
        // Same for the chrome button hint: a click acts on the control, so the
        // hint has served its purpose and must not survive the state change.
        self.ui_tooltip = None;
        self.ui_hover.clear();
        self.ui_tooltip_label = None;

        // A left press in the editor body arms the same dwell a resting
        // pointer does: a touchscreen emits no Moved events (a finger cannot
        // hover), so the tap itself is the "pointer arrived here" signal and
        // the LSP hover fires HOVER_DELAY later. This matches VS Code on
        // desktop too, where the hover re-shows when the pointer rests on a
        // symbol after a click. Drags (selection) and scrolls cancel the
        // pending dwell; the release does not, so a quick tap still fires.
        match m.kind {
            MouseEventKind::Down(MouseButton::Left)
                if in_editor
                    && !in_editor_scrollbar
                    && !in_editor_hscrollbar
                    && self.editor.tab_at(m.column, m.row).is_none() =>
            {
                // clear() before on_move(): a repeat tap on the same cell
                // must restart the timer (on_move alone keys off cell change).
                self.hover.clear();
                self.hover
                    .on_move(std::time::Instant::now(), m.column, m.row);
            }
            MouseEventKind::Up(_) => {}
            _ => self.hover.clear(),
        }

        // Termux: a tap in any editable area raises the on-screen keyboard,
        // standing in for the native soft keyboard that active mouse
        // tracking permanently suppresses. The tap still falls through to
        // its normal focus / cursor handling below.
        if self.osk_auto
            && self.osk.is_none()
            && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && (in_editor || in_terminal || (in_tree && self.sidebar_view == SidebarView::Search))
        {
            let mut osk = crate::widgets::osk::Osk::new();
            // Restore the persisted split-layout choice (foldables).
            osk.split = crate::prefs::Prefs::load_or_default().osk_split;
            self.osk = Some(osk);
            // The raise reflows the whole frame, so a dwell armed at the
            // pre-raise coordinates would resolve the wrong buffer cell.
            self.hover.clear();
        }

        match m.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                // Editor tab strip: right-click on a tab opens the
                // Close / Close Others / Close to the Right / Close All
                // menu. Routed here BEFORE the explorer branch so the
                // hit-test wins even when the editor pane shares its
                // column with the sidebar.
                if let Some(tab_idx) = self.editor.tab_at(m.column, m.row) {
                    self.focus_pane(Pane::Editor);
                    let items = build_tab_context_menu_items(
                        tab_idx,
                        self.editor.tab_count(),
                        self.editor_split.is_some(),
                    );
                    self.context_menu = Some(ContextMenu {
                        origin: (m.column, m.row),
                        items,
                        selected: 0,
                        target_dir: self.tree.root.clone(),
                    });
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Explorer {
                    self.focus_pane(Pane::Tree);
                    let node_idx = self.tree.node_at_y(m.row);
                    if let Some(idx) = node_idx {
                        let path_clicked = self.tree.nodes[idx].path.clone();
                        let already_marked = self.tree.marked.contains(&path_clicked);
                        if already_marked {
                            self.tree.selected = idx;
                        } else {
                            self.tree.select_replace(idx);
                        }
                    }
                    let node = node_idx.and_then(|i| self.tree.nodes.get(i));
                    let target_dir =
                        crate::widgets::file_tree::create_target_dir_for(node, &self.tree.root);
                    let selection = self.tree.action_paths();
                    let mut items = build_tree_context_menu_items(
                        node,
                        &self.tree.root,
                        &selection,
                        &target_dir,
                        self.tree_clipboard.as_ref(),
                        self.compare_anchor.as_deref(),
                    );
                    let clicked = node.map(|n| n.path.clone());
                    maybe_add_reveal_in_finder(
                        &mut items,
                        clicked.as_deref(),
                        !self.is_remote && cfg!(target_os = "macos"),
                    );
                    self.context_menu = Some(ContextMenu {
                        origin: (m.column, m.row),
                        items,
                        selected: 0,
                        target_dir,
                    });
                    return;
                }
                // Editor gutter: right-click in the glyph margin / line-number
                // column opens the breakpoint menu (Add / Remove Breakpoint,
                // Add Conditional / Edit Condition), mirroring VS Code's
                // glyph-margin context menu. Routed before the body branch so a
                // gutter click never falls through to the symbol menu. Diff /
                // sheet / image tabs have no debuggable source lines.
                if in_editor
                    && self.editor.diff.is_none()
                    && self.editor.sheet.is_none()
                    && self.editor.image.is_none()
                    && let Some(line0) = self.editor.gutter_line_at(m.column, m.row)
                {
                    self.focus_pane(Pane::Editor);
                    let line = line0 + 1; // 1-based, as breakpoints are stored
                    let (has_bp, has_cond) = self
                        .editor
                        .path
                        .as_ref()
                        .map(|p| {
                            let has_bp = self
                                .editor
                                .breakpoints
                                .get(p)
                                .is_some_and(|s| s.contains(&line));
                            let has_cond = self
                                .editor
                                .breakpoint_conditions
                                .get(p)
                                .is_some_and(|c| c.contains_key(&line));
                            (has_bp, has_cond)
                        })
                        .unwrap_or((false, false));
                    let toggle_label = if has_bp {
                        String::from("Remove Breakpoint")
                    } else {
                        String::from("Add Breakpoint")
                    };
                    let cond_label = if has_cond {
                        String::from("Edit Condition\u{2026}")
                    } else {
                        String::from("Add Conditional Breakpoint\u{2026}")
                    };
                    let items = vec![
                        (toggle_label, MenuAction::ToggleBreakpointAt { line }),
                        (cond_label, MenuAction::EditBreakpointConditionAt { line }),
                    ];
                    self.context_menu = Some(ContextMenu {
                        origin: (m.column, m.row),
                        items,
                        selected: 0,
                        target_dir: self.tree.root.clone(),
                    });
                    return;
                }
                // Editor body: right-click over a text buffer opens the symbol
                // menu (Go to Definition / Rename Symbol / Change All
                // Occurrences). Read-only diff / sheet / image tabs have no
                // symbols, so they're excluded.
                if in_editor
                    && self.editor.diff.is_none()
                    && self.editor.sheet.is_none()
                    && self.editor.image.is_none()
                    && let Some((row, col)) = self.editor.buffer_pos_at(m.column, m.row)
                {
                    self.focus_pane(Pane::Editor);
                    let mut items = vec![(
                        String::from("Go to Definition"),
                        MenuAction::GoToDefinitionAt { row, col },
                    )];
                    // Only offer "Go to Declaration" when this file's language
                    // server actually implements `textDocument/declaration`.
                    // TypeScript's vtsls advertises `declarationProvider: false`,
                    // so the row is hidden there, exactly as VS Code does.
                    if self.editor_language_supports_declaration() {
                        items.push((
                            String::from("Go to Declaration"),
                            MenuAction::GoToDeclarationAt { row, col },
                        ));
                    }
                    // "Go to Type Definition" is offered only when the server
                    // implements `textDocument/typeDefinition` (vtsls,
                    // basedpyright, rust-analyzer and gopls all do), mirroring
                    // VS Code's ordering right after Go to Declaration.
                    if self.editor_language_supports_type_definition() {
                        items.push((
                            String::from("Go to Type Definition"),
                            MenuAction::GoToTypeDefinitionAt { row, col },
                        ));
                    }
                    // "Go to Implementations" is offered only when the server
                    // implements `textDocument/implementation` (rust-analyzer,
                    // gopls, vtsls do), placed right after Type Definition to
                    // mirror VS Code's Go To submenu order.
                    if self.editor_language_supports_implementation() {
                        items.push((
                            String::from("Go to Implementations"),
                            MenuAction::GoToImplementationAt { row, col },
                        ));
                    }
                    // "Go to References" is offered only when the server
                    // implements `textDocument/references` (every server croft
                    // ships does), placed last in the Go To group to mirror VS
                    // Code's submenu order.
                    if self.editor_language_supports_references() {
                        items.push((
                            String::from("Go to References"),
                            MenuAction::GoToReferencesAt { row, col },
                        ));
                    }
                    items.push((
                        String::from("Rename Symbol"),
                        MenuAction::RenameSymbolAt { row, col },
                    ));
                    items.push((
                        String::from("Change All Occurrences"),
                        MenuAction::ChangeAllOccurrencesAt { row, col },
                    ));
                    self.context_menu = Some(ContextMenu {
                        origin: (m.column, m.row),
                        items,
                        selected: 0,
                        target_dir: self.tree.root.clone(),
                    });
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // (H) iTerm2 folds Cmd onto the Meta bit, so Cmd+click arrives as
                // ALT (Go to Definition) and Cmd+Shift+click as ALT|SHIFT (navigate back).
                if in_editor && m.modifiers.contains(KeyModifiers::ALT) {
                    if m.modifiers.contains(KeyModifiers::SHIFT) {
                        self.nav_back();
                    } else {
                        self.trigger_definition_at(m.column, m.row);
                    }
                    return;
                }
                if !in_editor {
                    self.editor_click.clear();
                }
                if !in_tree {
                    self.tree_click.clear();
                }
                // Splitter hit-test runs before everything else: clicking the
                // single-column seam between sidebar and editor (or the
                // single-row seam between editor and terminal) starts a
                // resize drag instead of falling through to the underlying
                // pane's click handler.
                if let Some(x) = self.sidebar_splitter_x {
                    // Two-column hit-zone: the seam itself (`x`, the editor's
                    // left edge) and one column to the left (the tree's right
                    // border). Either grab starts a sidebar drag — a 1-cell
                    // target is too easy to miss.
                    //
                    // The OUTLINE panel draws no right border, so its scrollbar
                    // sits on the sidebar's rightmost column (`x - 1`), inside
                    // this zone. Exempt that column when it's the outline bar so
                    // the press reaches the scrollbar handler below instead of
                    // being swallowed as a resize.
                    if (m.column == x || m.column == x.saturating_sub(1))
                        && !rect_contains(self.outline.last_scrollbar, m.column, m.row)
                    {
                        self.splitter_drag = Some(SplitterDrag::Sidebar);
                        return;
                    }
                }
                // Seam between the two editor columns while split. Same
                // two-column hit-zone as the sidebar seam.
                if let Some(x) = self.editor_splitter_x
                    && (m.column == x || m.column == x.saturating_sub(1))
                {
                    self.splitter_drag = Some(SplitterDrag::EditorSplit);
                    return;
                }
                // SYSTEM panel sits at the bottom of the sidebar column.
                // A click on its header row toggles collapse; any other
                // click inside its rect is swallowed so it doesn't fall
                // through to the activity widget above (which would
                // otherwise see the click as occurring on a row that's
                // visually covered by the panel).
                // OPEN EDITORS: header toggles collapse; a row activates that
                // editor's tab; the scrollbar lane starts a thumb drag. Any
                // other click is swallowed so it can't fall through to a panel
                // visually beneath this one.
                if rect_contains(self.open_editors.last_area, m.column, m.row) {
                    if rect_contains(self.open_editors.last_scrollbar, m.column, m.row) {
                        self.open_editors.scroll_to_bar_y(m.row);
                        self.open_editors_scrollbar_drag = true;
                    } else if self.open_editors.hit_header(m.column, m.row) {
                        self.open_editors.toggle_collapse();
                    } else if let Some(idx) = self.open_editors.row_at(m.row)
                        && let Some(path) = self.open_editors.path_at(idx)
                    {
                        self.activate_open_editor(path);
                    }
                    return;
                }
                // TIMELINE: header toggles collapse; a row opens that commit's
                // diff for the file in a read-only editor tab.
                if rect_contains(self.timeline.last_area, m.column, m.row) {
                    if rect_contains(self.timeline.last_scrollbar, m.column, m.row) {
                        self.timeline.scroll_to_bar_y(m.row);
                        self.timeline_scrollbar_drag = true;
                    } else if self.timeline.hit_header(m.column, m.row) {
                        self.timeline.toggle_collapse();
                    } else if let Some(idx) = self.timeline.row_at(m.row)
                        && let Some((path, hash)) = self.timeline.diff_target(idx)
                    {
                        self.open_timeline_diff(path, hash);
                    }
                    return;
                }
                // DEPENDENCIES: display-only, so only the header (collapse)
                // and the scrollbar lane are interactive.
                if rect_contains(self.dependencies.last_area, m.column, m.row) {
                    if rect_contains(self.dependencies.last_scrollbar, m.column, m.row) {
                        self.dependencies.scroll_to_bar_y(m.row);
                        self.deps_scrollbar_drag = true;
                    } else if self.dependencies.hit_header(m.column, m.row) {
                        self.dependencies.toggle_collapse();
                    }
                    return;
                }
                // The OUTLINE section stacks just above the SYSTEM panel. A
                // click on its header toggles collapse; a click on a symbol row
                // jumps the editor to that symbol (recording nav history, like
                // go-to-definition); any other click inside its rect is
                // swallowed so it doesn't fall through to the tree above.
                if rect_contains(self.outline.last_area, m.column, m.row) {
                    if rect_contains(self.outline.last_scrollbar, m.column, m.row) {
                        self.outline.scroll_to_bar_y(m.row);
                        self.outline_scrollbar_drag = true;
                    } else if self.outline.hit_header(m.column, m.row) {
                        self.outline.toggle_collapse();
                    } else if let Some(idx) = self.outline.row_at(m.row)
                        && let Some((path, line, col)) = self.outline.jump_target(idx)
                    {
                        self.go_to_definition(path, line, col);
                    }
                    return;
                }
                if rect_contains(self.system_panel.last_area, m.column, m.row) {
                    if self.system_panel.hit_header(m.column, m.row) {
                        self.system_panel.toggle_collapse();
                    }
                    return;
                }
                // The "[+]" / "[-]" buttons sit on the same row as the
                // editor/terminal splitter, so they have to win the
                // hit-test before the splitter-drag handler claims this
                // click. Each terminal pane has its own pair so the user
                // can kill any pane, not only the active one.
                if let Some(idx) = self
                    .terminal_close_buttons
                    .iter()
                    .position(|r| rect_contains(*r, m.column, m.row))
                {
                    if self.close_terminal_at(idx) {
                        self.status =
                            format!("Closed terminal: {} remaining", self.terminals.len());
                    }
                    return;
                }
                if self
                    .terminal_add_buttons
                    .iter()
                    .any(|r| rect_contains(*r, m.column, m.row))
                {
                    match self.split_terminal() {
                        Ok(()) => {
                            self.status =
                                format!("Split terminal: {} active", self.terminals.len());
                        }
                        Err(e) => {
                            self.status = format!("Split terminal failed: {e}");
                        }
                    }
                    return;
                }
                if let Some(y) = self.terminal_splitter_y {
                    // Two-row hit-zone: the terminal's top border (`y`) and
                    // the editor / welcome's bottom border (`y - 1`).
                    // Symmetric with the sidebar drag — either edge of the
                    // visible seam grabs the splitter.
                    //
                    // Gate by column too: the editor / terminal splitter
                    // only spans the RIGHT column (everything at or past
                    // `sidebar_splitter_x`). Without this gate, a click
                    // on a tree row in the LEFT column whose y lines up
                    // with the splitter is silently hijacked into a
                    // splitter-drag and never reaches `node_at_y` -
                    // exactly the dead-zone the user hit on the last
                    // two files in the Explorer.
                    let in_splitter_x_range = self.sidebar_splitter_x.is_none_or(|x| m.column >= x);
                    if in_splitter_x_range && (m.row == y || m.row == y.saturating_sub(1)) {
                        self.splitter_drag = Some(SplitterDrag::Terminal);
                        return;
                    }
                }
                // Activity-bar hit-test takes precedence over the side panel.
                if rect_contains(self.sidebar_areas.explorer_icon, m.column, m.row) {
                    self.set_sidebar_view(SidebarView::Explorer);
                    return;
                }
                if rect_contains(self.sidebar_areas.search_icon, m.column, m.row) {
                    self.set_sidebar_view(SidebarView::Search);
                    return;
                }
                if rect_contains(self.sidebar_areas.source_control_icon, m.column, m.row) {
                    self.set_sidebar_view(SidebarView::SourceControl);
                    return;
                }
                if rect_contains(self.sidebar_areas.remote_icon, m.column, m.row) {
                    self.set_sidebar_view(SidebarView::Remote);
                    return;
                }
                if rect_contains(self.sidebar_areas.run_debug_icon, m.column, m.row) {
                    self.set_sidebar_view(SidebarView::RunDebug);
                    return;
                }
                if rect_contains(self.sidebar_areas.settings_icon, m.column, m.row) {
                    self.open_settings_menu();
                    return;
                }
                if in_editor_pane
                    && self.editor.is_blank_initial()
                    && self.activate_welcome_link(m.column, m.row)
                {
                    return;
                }
                if in_tree_scrollbar {
                    self.focus_pane(Pane::Tree);
                    self.tree.scroll_to_bar_y(m.row);
                    self.scrollbar_drag = Some(Pane::Tree);
                    self.tree_click.clear();
                    return;
                }
                if in_remote_scrollbar {
                    self.focus_pane(Pane::Tree);
                    self.remote.scroll_to_bar_y(m.row);
                    self.scrollbar_drag = Some(Pane::Tree);
                    self.tree_click.clear();
                    return;
                }
                if in_search_scrollbar {
                    self.focus_pane(Pane::Tree);
                    self.search.scroll_to_bar_y(m.row);
                    self.scrollbar_drag = Some(Pane::Tree);
                    self.tree_click.clear();
                    return;
                }
                if in_editor_scrollbar {
                    self.focus_pane(Pane::Editor);
                    self.editor.scroll_to_bar_y(m.row);
                    self.scrollbar_drag = Some(Pane::Editor);
                    self.editor_click.clear();
                    self.poke_cursor();
                    return;
                }
                if in_editor_hscrollbar {
                    self.focus_pane(Pane::Editor);
                    self.editor.scroll_to_bar_x(m.column);
                    self.editor_hscrollbar_drag = true;
                    self.editor_click.clear();
                    self.poke_cursor();
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Search {
                    self.focus_pane(Pane::Tree);
                    if self.search.paste_button_at(m.column, m.row) {
                        let text = (self.clipboard_reader)();
                        self.paste_clipboard_into_search(text.as_deref());
                        return;
                    }
                    // Header "Refresh" re-runs the current query.
                    if self.search.refresh_at(m.column, m.row) {
                        self.submit_search_query();
                        return;
                    }
                    // Header "Clear Search Results" wipes the query + results.
                    if self.search.clear_at(m.column, m.row) {
                        self.search.clear_all();
                        return;
                    }
                    // Left chevron expands/collapses the Replace row.
                    if self.search.chevron_at(m.column, m.row) {
                        self.search.toggle_replace();
                        return;
                    }
                    // "..." ellipsis expands/collapses files-to-include/exclude.
                    if self.search.ellipsis_at(m.column, m.row) {
                        self.search.toggle_details();
                        return;
                    }
                    // Replace-all action icon runs Replace All across results.
                    if self.search.replace_all_at(m.column, m.row) {
                        self.run_search_replace_all();
                        return;
                    }
                    // Click inside one of the input boxes focuses that field.
                    if let Some(field) = self.search.field_at(m.column, m.row) {
                        self.search.focus_field(field);
                        return;
                    }
                    if let Some(t) = self.search.toggle_at(m.column, m.row) {
                        match t {
                            crate::widgets::search::SearchToggle::CaseSensitive => {
                                self.search.opts.case_sensitive = !self.search.opts.case_sensitive;
                            }
                            crate::widgets::search::SearchToggle::WholeWord => {
                                self.search.opts.whole_word = !self.search.opts.whole_word;
                            }
                            crate::widgets::search::SearchToggle::UseRegex => {
                                self.search.opts.use_regex = !self.search.opts.use_regex;
                            }
                        }
                        self.submit_search_query();
                        return;
                    }
                    // Click on a result row: open it. Mirrors the Explorer's
                    // preview/pin gesture — a single click opens the hit in
                    // the replaceable preview slot, a double-click pins the
                    // tab so moving on to the next result opens beside it
                    // instead of replacing the file under review.
                    if let Some(idx) = self.search.hit_at_y(m.row) {
                        self.search.selected = idx;
                        let now = std::time::Instant::now();
                        let is_double = self.tree_click.is_double(now, m.column, m.row);
                        if let Some(hit) = self.search.selected_hit().cloned() {
                            self.open_search_hit(&hit);
                            if is_double {
                                self.editor.pin_active();
                            }
                        }
                        if is_double {
                            self.tree_click.clear();
                        } else {
                            self.tree_click.record(now, m.column, m.row);
                        }
                    } else {
                        // Click on the input/header area: just focus search.
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::SourceControl {
                    self.focus_pane(Pane::Tree);
                    if self.source_control.click_header_refresh(m.column, m.row) {
                        self.refresh_source_control();
                        self.status = String::from("Refreshed Source Control");
                        return;
                    }
                    if self.commit_menu_open
                        && let Some(item) =
                            self.source_control.click_commit_menu_item(m.column, m.row)
                    {
                        self.commit_menu_open = false;
                        self.dispatch_commit_menu_item(item);
                        return;
                    }
                    if self.source_control.click_commit_caret(m.column, m.row) {
                        self.commit_menu_open = !self.commit_menu_open;
                        return;
                    }
                    if self.commit_menu_open {
                        // Any click outside the caret/menu closes the
                        // dropdown without firing its action; fall through
                        // so the underlying gesture still applies (the
                        // user can dismiss-by-clicking-elsewhere instead
                        // of needing Esc + click).
                        self.commit_menu_open = false;
                    }
                    if self.source_control.click_init_repo_button(m.column, m.row) {
                        self.initialize_repository();
                        return;
                    }
                    if self.source_control.click_button(m.column, m.row) {
                        self.commit_source_control();
                        return;
                    }
                    if self.source_control.click_input(m.column, m.row) {
                        return;
                    }
                    if let Some(idx) = self.source_control.click_discard_action(m.column, m.row) {
                        self.request_discard_source_control_entry(idx);
                        return;
                    }
                    if let Some(idx) = self.source_control.click_stage_action(m.column, m.row) {
                        self.stage_source_control_entry(idx);
                        return;
                    }
                    if let Some(idx) = self.source_control.entry_at_y(m.row) {
                        self.open_source_control_entry(idx);
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::RunDebug {
                    self.focus_pane(Pane::Tree);
                    if self.run_debug.debug_active {
                        if let Some(idx) = self.run_debug.debug_row_at(m.row) {
                            self.debug_panel_click(idx);
                        }
                    } else if self.run_debug.click_button(m.column, m.row) {
                        self.run_debug_button_action();
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Remote {
                    self.focus_pane(Pane::Tree);
                    if rect_contains(self.remote.header_chevron_btn, m.column, m.row) {
                        self.remote.toggle_collapsed();
                        self.status = if self.remote.collapsed {
                            String::from("Collapsed SSH section")
                        } else {
                            String::from("Expanded SSH section")
                        };
                        return;
                    }
                    if rect_contains(self.remote.header_add_btn, m.column, m.row)
                        || rect_contains(self.remote.empty_primary_btn, m.column, m.row)
                        || rect_contains(self.remote.empty_secondary_btn, m.column, m.row)
                    {
                        self.open_ssh_config_in_editor();
                        return;
                    }
                    if rect_contains(self.remote.header_refresh_btn, m.column, m.row) {
                        self.remote.refresh();
                        self.status = String::from("Reloaded SSH hosts from ~/.ssh/config");
                        return;
                    }
                    if rect_contains(self.remote.empty_learn_link, m.column, m.row) {
                        self.open_ssh_learn_more();
                        return;
                    }
                    if let Some(idx) = self.remote.target_at_y(m.row) {
                        self.remote.select(idx);
                        let now = std::time::Instant::now();
                        let is_double = self.tree_click.is_double(now, m.column, m.row);
                        if is_double {
                            if let Some(target) = self.remote.selected_target().cloned() {
                                self.request_remote_launch(target.alias, None);
                            }
                            self.tree_click.clear();
                        } else {
                            self.tree_click.record(now, m.column, m.row);
                        }
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Explorer {
                    self.focus_pane(Pane::Tree);
                    // Header toolbar (root folder row): New File / New Folder /
                    // Refresh / Collapse Folders. Checked before row hit-testing
                    // so a click on an icon never falls through to the node.
                    if rect_contains(self.tree.header_new_file_btn, m.column, m.row) {
                        let target = self.explorer_create_target_dir();
                        self.open_create_prompt(CreateKind::File, target);
                        return;
                    }
                    if rect_contains(self.tree.header_new_folder_btn, m.column, m.row) {
                        let target = self.explorer_create_target_dir();
                        self.open_create_prompt(CreateKind::Folder, target);
                        return;
                    }
                    if rect_contains(self.tree.header_refresh_btn, m.column, m.row) {
                        self.tree.refresh_children(0);
                        self.status = String::from("Refreshed Explorer");
                        return;
                    }
                    if rect_contains(self.tree.header_collapse_btn, m.column, m.row) {
                        self.tree.collapse_all();
                        self.status = String::from("Collapsed all folders");
                        return;
                    }
                    // The ⋯ "Views and More Actions" button on the EXPLORER
                    // title line opens the menu that toggles which sub-views
                    // (Open Editors / Folders / Outline / Timeline /
                    // Dependencies) are stacked in the panel.
                    if rect_contains(self.tree.header_views_btn, m.column, m.row) {
                        self.open_explorer_views_menu();
                        return;
                    }
                    if let Some(idx) = self.tree.node_at_y(m.row) {
                        let now = std::time::Instant::now();
                        let is_double = self.tree_click.is_double(now, m.column, m.row);
                        let has_shift = m.modifiers.contains(KeyModifiers::SHIFT);
                        // macOS terminals (iTerm2, Terminal.app) never put the
                        // Cmd bit on mouse events — the SGR mouse encoding only
                        // carries Shift/Alt/Ctrl. Treat Alt (Option on macOS)
                        // and Ctrl as the cherry-pick modifier so the gesture
                        // actually reaches the app.
                        let has_toggle_mod = m.modifiers.contains(KeyModifiers::ALT)
                            || m.modifiers.contains(KeyModifiers::CONTROL);
                        // Shift-click extends the range from the anchor. No
                        // activation, no drag.
                        if has_shift {
                            self.tree.extend_to(idx);
                            self.tree_click.clear();
                            self.tree_drag = None;
                            return;
                        }
                        // Alt/Ctrl-click: defer the toggle until mouse-up so a
                        // movement in between can promote the gesture into an
                        // Alt-drag (copy) instead. Don't activate (no file
                        // open, no folder toggle) and don't include the row
                        // in the multi-selection yet.
                        if has_toggle_mod {
                            self.tree_click.clear();
                            let mut drag_paths = self.tree.action_paths();
                            let path_clicked = self.tree.nodes[idx].path.clone();
                            if !drag_paths.iter().any(|p| p == &path_clicked) {
                                drag_paths.push(path_clicked);
                            }
                            self.tree_drag = Some(ExplorerDrag {
                                paths: drag_paths,
                                target_idx: None,
                                armed: false,
                                started_at: (now, m.column, m.row),
                                start_idx: idx,
                                toggle_on_release: true,
                            });
                            return;
                        }
                        // Plain click: if the row is already in the marked set,
                        // keep the marks intact so a subsequent drag carries
                        // every selected entry; otherwise collapse selection
                        // to this single row.
                        let already_marked = self.tree.marked.contains(&self.tree.nodes[idx].path);
                        if !already_marked {
                            self.tree.select_replace(idx);
                        } else {
                            self.tree.selected = idx;
                            self.tree.anchor = idx;
                        }
                        let clicked_is_dir =
                            self.tree.nodes.get(idx).is_some_and(|node| node.is_dir);
                        if clicked_is_dir && is_double {
                            self.tree_click.clear();
                            self.tree_drag = None;
                            return;
                        }
                        // Arm a potential drag-source. Movement off this cell
                        // promotes it into a real drag in the Drag handler;
                        // a stationary release keeps the click semantics.
                        let drag_paths = self.tree.action_paths();
                        if !drag_paths.is_empty() {
                            self.tree_drag = Some(ExplorerDrag {
                                paths: drag_paths,
                                target_idx: None,
                                armed: false,
                                started_at: (now, m.column, m.row),
                                start_idx: idx,
                                toggle_on_release: false,
                            });
                        }
                        if let Some(path) = self.tree.activate() {
                            let result = if is_double {
                                self.editor.open_pinned(&path)
                            } else {
                                self.editor.open_preview(&path)
                            };
                            match result {
                                Ok(()) => {
                                    self.sync_open_file_poll_mtime();
                                    self.status = self.editor.status.clone();
                                    if is_double {
                                        self.focus_pane(Pane::Editor);
                                        self.poke_cursor();
                                    }
                                }
                                Err(e) => self.status = format!("Error: {e}"),
                            }
                        }
                        if is_double {
                            self.tree_click.clear();
                        } else {
                            self.tree_click.record(now, m.column, m.row);
                        }
                    }
                } else if in_editor_pane && !in_editor {
                    if let Some(idx) = self.editor.close_at(m.column, m.row) {
                        if self.editor.close_tab(idx) {
                            self.sync_open_file_poll_mtime();
                            self.status = String::from("Closed tab");
                            self.poke_cursor();
                            self.collapse_split_if_empty();
                        }
                    } else if let Some(idx) = self.editor.tab_at(m.column, m.row) {
                        self.focus_pane(Pane::Editor);
                        if self.editor.active_index() != idx {
                            self.editor.select(idx);
                            self.sync_open_file_poll_mtime();
                        }
                        self.poke_cursor();
                    }
                } else if in_editor {
                    self.focus_pane(Pane::Editor);
                    // Mirror of the terminal branch: a selection lives in one
                    // pane only, so anchoring one in the editor drops any
                    // leftover terminal selection (issue #23).
                    self.terminal_mut().clear_selection();
                    // Diff header arrows: take precedence over selection so a
                    // click on the ‹ / › glyph jumps to prev/next change
                    // instead of anchoring a selection at the header row.
                    if self.editor.diff.is_some() {
                        if rect_contains(self.editor.diff_prev_arrow, m.column, m.row) {
                            self.jump_diff_change(false);
                            return;
                        }
                        if rect_contains(self.editor.diff_next_arrow, m.column, m.row) {
                            self.jump_diff_change(true);
                            return;
                        }
                        let now = std::time::Instant::now();
                        let is_double = self.editor_click.is_double(now, m.column, m.row);
                        let last_inner = self.editor.last_inner;
                        if let Some(diff) = self.editor.diff.as_mut() {
                            if let Some((side, row_idx, char_col)) =
                                crate::widgets::editor::diff_hit_test(
                                    diff, last_inner, m.column, m.row,
                                )
                            {
                                if is_double {
                                    diff.select_word_at(side, row_idx, char_col);
                                } else {
                                    diff.start_selection(side, row_idx, char_col);
                                }
                            } else {
                                diff.clear_selection();
                            }
                        }
                        if is_double {
                            self.editor_click.clear();
                        } else {
                            self.editor_click.record(now, m.column, m.row);
                        }
                        self.poke_cursor();
                        return;
                    }
                    let now = std::time::Instant::now();
                    let is_double = self.editor_click.is_double(now, m.column, m.row);
                    if is_double {
                        self.editor.select_word_at(m.column, m.row);
                        self.editor_click.clear();
                    } else {
                        // Anchor a fresh selection at the click; a drag widens it,
                        // a clean click ends up cleared on mouse-up.
                        self.editor.mouse_down(m.column, m.row);
                        self.editor_click.record(now, m.column, m.row);
                    }
                    self.poke_cursor();
                } else if let Some(idx) = terminal_hit {
                    if self.active_terminal != idx {
                        self.active_terminal = idx;
                    }
                    self.focus_pane(Pane::Terminal);
                    // A selection belongs to exactly one pane: starting one in
                    // the terminal drops any leftover editor selection so the
                    // two panes never paint a highlight at once (issue #23).
                    self.editor.clear_selection();
                    let now = std::time::Instant::now();
                    let is_double = self.terminal_click.is_double(now, m.column, m.row);
                    if is_double {
                        self.terminal_mut().select_word_at(m.column, m.row);
                        self.terminal_click.clear();
                    } else {
                        // Begin a fresh selection at the click cell. Without a
                        // drag this is a single cell (no area), so the selection
                        // ends up cleared on mouse-up. With a drag, this is the
                        // selection anchor.
                        self.terminal_mut().start_selection_at(m.column, m.row);
                        self.terminal_click.record(now, m.column, m.row);
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(kind) = self.splitter_drag {
                    self.handle_splitter_drag(kind, m.column, m.row);
                    return;
                }
                if self.editor_hscrollbar_drag {
                    self.editor.scroll_to_bar_x(m.column);
                    self.poke_cursor();
                    return;
                }
                if self.outline_scrollbar_drag {
                    self.outline.scroll_to_bar_y(m.row);
                    return;
                }
                if self.open_editors_scrollbar_drag {
                    self.open_editors.scroll_to_bar_y(m.row);
                    return;
                }
                if self.timeline_scrollbar_drag {
                    self.timeline.scroll_to_bar_y(m.row);
                    return;
                }
                if self.deps_scrollbar_drag {
                    self.dependencies.scroll_to_bar_y(m.row);
                    return;
                }
                if let Some(pane) = self.scrollbar_drag {
                    match pane {
                        Pane::Tree => match self.sidebar_view {
                            SidebarView::Explorer => {
                                self.tree.scroll_to_bar_y(m.row);
                            }
                            SidebarView::Remote => {
                                self.remote.scroll_to_bar_y(m.row);
                            }
                            SidebarView::SourceControl => {
                                self.source_control.scroll_to_bar_y(m.row);
                            }
                            SidebarView::Search => {
                                self.search.scroll_to_bar_y(m.row);
                            }
                            SidebarView::RunDebug => {}
                        },
                        Pane::Editor => {
                            self.editor.scroll_to_bar_y(m.row);
                            self.poke_cursor();
                        }
                        Pane::Terminal => {}
                    }
                    return;
                }
                // Some terminals emit a Drag at the same cell as the Down even
                // when the user hasn't actually dragged. Only forget the prior
                // click when the pointer has truly moved off that cell.
                self.editor_click.clear_if_moved(m.column, m.row);
                self.tree_click.clear_if_moved(m.column, m.row);
                self.terminal_click.clear_if_moved(m.column, m.row);
                // Terminal drag-selection is handled before the per-pane
                // branches so it keeps extending even when the pointer
                // leaves the pane: dragging past the bottom edge is how the
                // user grows a selection down through scrollback. The
                // returned direction arms edge auto-scroll (serviced each
                // main-loop tick); a pointer back inside the pane disarms it.
                if self.focus == Pane::Terminal && self.terminal().selection().is_some() {
                    let dir = self.terminal_mut().drag_select_to(m.column, m.row);
                    self.terminal_select_autoscroll =
                        (dir != 0).then_some(TerminalSelectAutoScroll {
                            dir,
                            col: m.column,
                            last: std::time::Instant::now(),
                        });
                    return;
                }
                if in_editor {
                    if self.editor.diff.is_some() {
                        let last_inner = self.editor.last_inner;
                        if let Some(diff) = self.editor.diff.as_mut()
                            && let Some((_, row_idx, char_col)) =
                                crate::widgets::editor::diff_hit_test(
                                    diff, last_inner, m.column, m.row,
                                )
                        {
                            diff.extend_selection_to(row_idx, char_col);
                        }
                        self.poke_cursor();
                        return;
                    }
                    self.editor.mouse_drag(m.column, m.row);
                    self.poke_cursor();
                } else if in_tree {
                    match self.sidebar_view {
                        SidebarView::Explorer => {
                            // Promote to a real drag-and-drop the moment the
                            // pointer leaves the initiating cell.
                            if let Some(drag) = self.tree_drag.as_mut() {
                                let (_, sx, sy) = drag.started_at;
                                if m.column != sx || m.row != sy {
                                    drag.armed = true;
                                }
                                if drag.armed {
                                    drag.target_idx =
                                        drag_target_index(&self.tree, m.row, &drag.paths);
                                    self.tree.drag_target = drag.target_idx;
                                }
                            }
                            // While not dragging, treat continued movement as a
                            // range-extend from the initial anchor — just like
                            // VS Code's drag-to-select.
                            if self.tree_drag.as_ref().is_none_or(|d| !d.armed)
                                && let Some(idx) = self.tree.node_at_y(m.row)
                            {
                                self.tree.select(idx);
                            }
                        }
                        SidebarView::Remote => {
                            if let Some(idx) = self.remote.target_at_y(m.row) {
                                self.remote.select(idx);
                            }
                        }
                        SidebarView::Search
                        | SidebarView::SourceControl
                        | SidebarView::RunDebug => {}
                    }
                } else if in_terminal {
                    self.terminal_mut().extend_selection_to(m.column, m.row);
                } else if self.tree_drag.is_some() {
                    // Pointer dragged outside the tree pane: still keep the drag
                    // alive so dropping back inside lands; just clear the drop
                    // highlight so the user knows nothing will happen here.
                    if let Some(drag) = self.tree_drag.as_mut() {
                        drag.target_idx = None;
                    }
                    self.tree.drag_target = None;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Releasing the button ends any terminal edge auto-scroll.
                self.terminal_select_autoscroll = None;
                if self.splitter_drag.take().is_some() {
                    return;
                }
                if self.scrollbar_drag.take().is_some() {
                    return;
                }
                if std::mem::take(&mut self.editor_hscrollbar_drag) {
                    return;
                }
                if std::mem::take(&mut self.outline_scrollbar_drag) {
                    return;
                }
                if std::mem::take(&mut self.open_editors_scrollbar_drag) {
                    return;
                }
                if std::mem::take(&mut self.timeline_scrollbar_drag) {
                    return;
                }
                if std::mem::take(&mut self.deps_scrollbar_drag) {
                    return;
                }
                if let Some(drag) = self.tree_drag.take() {
                    self.tree.drag_target = None;
                    if drag.armed {
                        // Drag while Alt/Ctrl is held copies; default is move.
                        let copy = drag.toggle_on_release
                            || m.modifiers.contains(KeyModifiers::ALT)
                            || m.modifiers.contains(KeyModifiers::CONTROL);
                        let mode = if copy {
                            ExplorerClipMode::Copy
                        } else {
                            ExplorerClipMode::Cut
                        };
                        if let Some(target_idx) = drag.target_idx {
                            let target_dir = drop_target_dir(&self.tree, target_idx);
                            self.apply_paste_or_drop(&target_dir, &drag.paths, mode);
                        } else {
                            self.status = String::from("Drop cancelled");
                        }
                        return;
                    }
                    if drag.toggle_on_release {
                        // Stationary Alt/Ctrl-click: now perform the toggle.
                        self.tree.toggle_mark(drag.start_idx);
                        return;
                    }
                }
                // Mouse-up never auto-copies. The selection stays highlighted
                // so the user can hit Cmd/Ctrl+C themselves; a click without
                // drag (zero-area selection) is silently dropped.
                if in_terminal {
                    if let Some(sel) = self.terminal().selection()
                        && !sel.has_area()
                    {
                        self.terminal_mut().clear_selection();
                    }
                } else if in_editor
                    && let Some(sel) = self.editor.selection
                    && !sel.has_area()
                {
                    self.editor.clear_selection();
                }
            }
            MouseEventKind::ScrollDown => {
                if in_outline {
                    self.outline.scroll_down(3);
                } else if in_open_editors {
                    self.open_editors.scroll_down(3);
                } else if in_timeline {
                    self.timeline.scroll_down(3);
                } else if in_deps {
                    self.dependencies.scroll_down(3);
                } else if in_tree {
                    match self.sidebar_view {
                        SidebarView::Explorer => self.tree.scroll_down(3),
                        SidebarView::Remote => self.remote.scroll_down(3),
                        SidebarView::SourceControl => self.source_control.scroll_down(3),
                        SidebarView::Search => self.search.scroll_down(3),
                        SidebarView::RunDebug => {}
                    }
                } else if in_editor {
                    if let Some(diff) = self.editor.diff.as_mut() {
                        diff.scroll_down_by(3);
                    } else {
                        self.editor.scroll_down(3);
                    }
                } else if let Some(idx) = terminal_hit {
                    let t = &mut self.terminals[idx];
                    // Try our scrollback first; if we're in vim/less/htop
                    // (alternate-screen), fall back to forwarding arrow keys.
                    if !t.scroll_down(3) {
                        t.write_input(b"\x1b[B\x1b[B\x1b[B");
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if in_outline {
                    self.outline.scroll_up(3);
                } else if in_open_editors {
                    self.open_editors.scroll_up(3);
                } else if in_timeline {
                    self.timeline.scroll_up(3);
                } else if in_deps {
                    self.dependencies.scroll_up(3);
                } else if in_tree {
                    match self.sidebar_view {
                        SidebarView::Explorer => self.tree.scroll_up(3),
                        SidebarView::Remote => self.remote.scroll_up(3),
                        SidebarView::SourceControl => self.source_control.scroll_up(3),
                        SidebarView::Search => self.search.scroll_up(3),
                        SidebarView::RunDebug => {}
                    }
                } else if in_editor {
                    if let Some(diff) = self.editor.diff.as_mut() {
                        diff.scroll_up_by(3);
                    } else {
                        self.editor.scroll_up(3);
                    }
                } else if let Some(idx) = terminal_hit {
                    let t = &mut self.terminals[idx];
                    if !t.scroll_up(3) {
                        t.write_input(b"\x1b[A\x1b[A\x1b[A");
                    }
                }
            }
            MouseEventKind::ScrollLeft if in_editor => {
                if let Some(diff) = self.editor.diff.as_mut() {
                    diff.scroll_left_by(4);
                } else {
                    self.editor.scroll_left_by(4);
                }
            }
            MouseEventKind::ScrollRight if in_editor => {
                if let Some(diff) = self.editor.diff.as_mut() {
                    diff.scroll_right_by(4);
                } else {
                    self.editor.scroll_right_by(4);
                }
            }
            _ => {}
        }
    }

    fn save(&mut self) {
        use crate::widgets::editor::SaveOutcome;
        // A previously-refused save arms a one-shot override: the very next
        // Cmd+S overwrites the external change. Consume it here so it never
        // lingers past the press it was meant for.
        if std::mem::take(&mut self.force_save_armed) {
            match self.editor.save_to_disk_force() {
                Ok(()) => self.status = self.editor.status.clone(),
                Err(e) => self.status = format!("Save failed: {e}"),
            }
            return;
        }
        match self.editor.save_to_disk() {
            Ok(SaveOutcome::Saved) => self.status = self.editor.status.clone(),
            Ok(SaveOutcome::DiskConflict) => {
                self.force_save_armed = true;
                let name = self
                    .editor
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.status = format!(
                    "{name} changed on disk since you opened it - press Cmd+S again to overwrite, or close the tab to keep the disk version"
                );
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    /// Compute the menu's bounding rect from current state.
    fn menu_rect(&self) -> Option<Rect> {
        let menu = self.context_menu.as_ref()?;
        // Use char count (not byte len) so multi-byte glyphs in menu
        // labels (e.g. the "…" in "Rename", the arrow in "Compare with
        // Selected") don't inflate the menu width past what's needed.
        // Width must also fit the shortcut hint on the right side, with
        // at least 2 cells of gap between label and shortcut.
        let widest = menu
            .items
            .iter()
            .map(|(label, action)| {
                let label_w = label.chars().count();
                let shortcut_w = shortcut_for(action)
                    .map(|s| s.chars().count() + 2)
                    .unwrap_or(0);
                label_w + shortcut_w
            })
            .max()
            .unwrap_or(0);
        let width = (widest + 4).max(18) as u16;
        let height = (menu.items.len() + 2) as u16;
        let area = self.last_frame_area;
        // Clamp identically to `render_context_menu` so hit-testing maps
        // clicks to the same row the user actually sees. Without this, a
        // menu that has to shift up to fit (right-click low on screen)
        // dispatches the row above the one the user clicked.
        let x = if area.width > 0 {
            menu.origin.0.min(area.width.saturating_sub(width))
        } else {
            menu.origin.0
        };
        let y = if area.height > 0 {
            menu.origin.1.min(area.height.saturating_sub(height))
        } else {
            menu.origin.1
        };
        Some(Rect {
            x,
            y,
            width,
            height,
        })
    }

    /// If (x, y) hits a menu item row, return its index.
    fn menu_item_at(&self, x: u16, y: u16) -> Option<usize> {
        let r = self.menu_rect()?;
        if !rect_contains(r, x, y) {
            return None;
        }
        // Items live inside the 1-cell border.
        let inner_y = y.checked_sub(r.y + 1)?;
        let menu = self.context_menu.as_ref()?;
        let idx = inner_y as usize;
        if idx < menu.items.len() {
            Some(idx)
        } else {
            None
        }
    }

    fn handle_menu_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.context_menu = None;
            }
            KeyCode::Up => {
                if let Some(menu) = self.context_menu.as_mut()
                    && menu.selected > 0
                {
                    menu.selected -= 1;
                }
            }
            KeyCode::Down => {
                if let Some(menu) = self.context_menu.as_mut()
                    && menu.selected + 1 < menu.items.len()
                {
                    menu.selected += 1;
                }
            }
            KeyCode::Enter => {
                if let Some(menu) = self.context_menu.as_ref() {
                    let action = menu.items[menu.selected].1.clone();
                    let dir = menu.target_dir.clone();
                    self.context_menu = None;
                    self.dispatch_menu_action(action, dir);
                }
            }
            _ => {}
        }
    }

    /// Re-root the workspace at `new_root`. Resets the file tree, updates
    /// `workspace_root`, refreshes git status and the source-control
    /// panel, and respawns the FS watcher off-thread (a recursive stat
    /// walk on a multi-GB monorepo is the dominant cost - blocking the
    /// UI thread for it is what froze the app on "Make root"). The
    /// polling fallback in `drain_fs_events` keeps the tree fresh until
    /// the watcher comes online via `try_install_pending_init`. Open
    /// editor tabs are NOT closed - a path that escapes the new root
    /// still resolves on disk and the user may want to keep it open.
    /// Compare anchor / clipboard / marks are reset because they point
    /// into the old workspace.
    pub fn change_workspace_root(&mut self, new_root: PathBuf) {
        let display = new_root.display().to_string();
        let mut shell_synced = true;
        if let Some(active) = self.terminals.get_mut(self.active_terminal) {
            if active.foreground_is_shell() {
                active.change_cwd(&new_root);
            } else {
                shell_synced = false;
            }
        }
        self.workspace_root = new_root.clone();
        // Rebind the LSP servers to the new root. The manager captured its
        // workspace folder once, at App::new, from croft's launch directory -
        // and the Croft.app launcher hardcodes that to ~/Documents. Re-rooting
        // into a child repo left rust-analyzer running `cargo metadata` against
        // the parent, which has no Cargo.toml, so every opened .rs came back as
        // an `unlinked-file` with no hover / completion / semantic tokens.
        // Spawn a fresh manager rooted at new_root; assigning over the Option
        // drops the old manager, whose Drop shuts its servers down. Clearing
        // lsp_last_seen makes the next sync_lsp() re-open every live editor tab
        // against the new servers; the stale diagnostics / progress from the
        // old root are dropped with it.
        self.lsp = match crate::lsp::LspManager::new(new_root.clone()) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("lsp manager rebind failed: {e}");
                None
            }
        };
        self.lsp_last_seen.clear();
        self.lsp_diagnostics.clear();
        self.lsp_progress.clear();
        // Refresh the window/icon title: it was emitted once at startup from
        // the launch dir and never updated, so after a re-root it kept showing
        // the parent (e.g. "Documents") instead of the repo. OSC 0 is
        // grid-independent, so writing it straight to stdout between draws is
        // safe - the same pattern startup uses.
        {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(&set_title_seq(&build_title(&new_root)));
            let _ = out.flush();
        }
        self.tree.set_root(new_root.clone());
        self.tree_clipboard = None;
        self.compare_anchor = None;
        self.fs_watch.rebind(&new_root, &self.tree);
        self.git.bypass_debounce();
        // Rebind the worker's root before any query, otherwise the next
        // Status / Changes request runs against the stale root captured
        // at App::new and the Source Control panel keeps reporting the
        // OLD repo's state (or "no repo") even after a Make Root that
        // landed inside a real git tree.
        self.git.set_root(new_root.clone());
        self.refresh_git_status_debounced();
        self.refresh_source_control();
        // Rebind the search worker and the panel's display root to the new
        // Explorer root so the next search targets this directory and its
        // subdirectories, not the root captured when the worker was spawned.
        // Drop the old root's hits so nothing stale lingers in the panel.
        let _ = self
            .search_query_tx
            .send(SearchRequest::SetRoot(new_root.clone()));
        self.search.root = new_root.clone();
        self.search.hits.clear();
        self.search.selected = 0;
        self.search.scroll = 0;
        // Drop any in-flight walker for the OLD root so its result is
        // discarded when it tries to send. Then kick a fresh walk
        // against the new root. file_finder_index keeps the old entries
        // for now so a Cmd+P press during the rebuild window shows
        // stale-but-immediate results instead of a UI freeze.
        self.file_finder_index_rx = None;
        self.file_finder_index_dirty = false;
        self.kick_file_finder_index_rebuild();
        self.status = if shell_synced {
            format!("Workspace root: {display}")
        } else {
            format!("Workspace root: {display} (terminal busy; shell not moved)")
        };
    }

    fn is_explorer_focused(&self) -> bool {
        self.focus == Pane::Tree && self.sidebar_view == SidebarView::Explorer
    }

    /// Dispatch the Explorer-pane keyboard shortcuts (Cmd+F / Cmd+Shift+F /
    /// Cmd+R / F2 / Cmd+/). Returns true if the key was handled. The
    /// target node is whichever row the user has highlighted; folder-only
    /// actions (Make root) are no-ops on file selections.
    fn handle_explorer_shortcut(&mut self, key: KeyEvent) -> bool {
        if is_tree_zoxide_jump_key(key) {
            self.open_zoxide_jump();
            return true;
        }
        if is_tree_new_file_key(key) {
            let target = self.explorer_create_target_dir();
            self.open_create_prompt(CreateKind::File, target);
            return true;
        }
        if is_tree_new_folder_key(key) {
            let target = self.explorer_create_target_dir();
            self.open_create_prompt(CreateKind::Folder, target);
            return true;
        }
        if is_tree_rename_key(key) {
            if let Some(path) = self.explorer_selected_path() {
                self.open_rename_prompt(path);
            }
            return true;
        }
        // Reveal in Finder is a local-macOS-only host bridge: on a remote SSH
        // session croft runs on a headless host with no Finder, so the chord
        // falls through there exactly as the menu entry is withheld.
        if !self.is_remote && cfg!(target_os = "macos") && is_tree_reveal_in_finder_key(key) {
            if let Some(path) = self.explorer_selected_path() {
                self.reveal_in_finder(path);
            }
            return true;
        }
        if is_tree_make_root_key(key) {
            if let Some(path) = self.explorer_selected_folder() {
                self.change_workspace_root(path);
            }
            return true;
        }
        if is_tree_make_parent_root_key(key) {
            if let Some(path) = self.explorer_parent_for_make_root() {
                self.change_workspace_root(path);
            }
            return true;
        }
        false
    }

    /// Resolve the parent folder of the highlighted node for the
    /// `Cmd+Shift+/` "Make root at parent" gesture. Works on files
    /// (parent = containing directory), folders (parent = its parent),
    /// and the tree root itself (parent = one filesystem level up).
    /// Returns `None` when the selected path has no parent (filesystem
    /// root) or no node is currently selected.
    fn explorer_parent_for_make_root(&self) -> Option<PathBuf> {
        let n = self.tree.nodes.get(self.tree.selected)?;
        n.path.parent().map(|p| p.to_path_buf())
    }

    fn explorer_selected_path(&self) -> Option<PathBuf> {
        self.tree
            .nodes
            .get(self.tree.selected)
            .map(|n| n.path.clone())
    }

    fn explorer_selected_folder(&self) -> Option<PathBuf> {
        let n = self.tree.nodes.get(self.tree.selected)?;
        if n.is_dir && n.path != self.tree.root {
            Some(n.path.clone())
        } else {
            None
        }
    }

    fn explorer_create_target_dir(&self) -> PathBuf {
        let node = self.tree.nodes.get(self.tree.selected);
        crate::widgets::file_tree::create_target_dir_for(node, &self.tree.root)
    }

    /// VS Code-style type-to-jump for the Explorer. Each printable
    /// keystroke arriving while the file tree is focused appends to a
    /// short-lived prefix buffer and snaps selection to the first
    /// visible node whose name starts with that prefix. Keystrokes
    /// further apart than `TREE_TYPEAHEAD_WINDOW` reset the buffer to
    /// a single character and cycle to the next match, so re-pressing
    /// the same letter walks through siblings beginning with it.
    /// `now` is injected for deterministic tests.
    fn apply_tree_typeahead(&mut self, c: char, now: std::time::Instant) {
        let within_window = self
            .tree_typeahead
            .as_ref()
            .is_some_and(|t| now.duration_since(t.last) < TREE_TYPEAHEAD_WINDOW);
        let prefix = if within_window {
            let mut p = self.tree_typeahead.as_ref().unwrap().prefix.clone();
            p.push(c.to_ascii_lowercase());
            p
        } else {
            c.to_ascii_lowercase().to_string()
        };
        let n = self.tree.nodes.len();
        if n > 0 {
            let start = if within_window {
                self.tree.selected
            } else {
                (self.tree.selected + 1) % n
            };
            if let Some(idx) = self.tree.find_prefix(&prefix, start) {
                self.tree.selected = idx;
                self.tree.anchor = idx;
                self.tree.marked.clear();
            }
        }
        self.tree_typeahead = Some(TreeTypeahead { prefix, last: now });
    }

    fn dispatch_menu_action(&mut self, action: MenuAction, target_dir: PathBuf) {
        match action {
            MenuAction::Create(kind) => self.open_create_prompt(kind, target_dir),
            MenuAction::Delete { paths } => self.delete_paths(paths),
            MenuAction::Rename(path) => self.open_rename_prompt(path),
            MenuAction::Cut(paths) => {
                let n = paths.len();
                self.tree_clipboard = Some(ExplorerClipboard {
                    mode: ExplorerClipMode::Cut,
                    paths,
                });
                self.status = if n == 1 {
                    String::from("Cut 1 item")
                } else {
                    format!("Cut {n} items")
                };
            }
            MenuAction::Copy(paths) => {
                let n = paths.len();
                self.tree_clipboard = Some(ExplorerClipboard {
                    mode: ExplorerClipMode::Copy,
                    paths,
                });
                self.status = if n == 1 {
                    String::from("Copied 1 item")
                } else {
                    format!("Copied {n} items")
                };
            }
            MenuAction::Paste(dest) => self.paste_into(dest),
            MenuAction::SelectForCompare(path) => {
                let label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.compare_anchor = Some(path);
                self.status = format!("Selected {label} for compare");
            }
            MenuAction::MakeRoot(folder) => {
                self.change_workspace_root(folder);
            }
            MenuAction::RevealInFinder(path) => self.reveal_in_finder(path),
            MenuAction::CloseTab(idx) => {
                if self.editor.close_tab(idx) {
                    self.sync_open_file_poll_mtime();
                    self.status = String::from("Closed tab");
                    self.poke_cursor();
                    self.collapse_split_if_empty();
                }
            }
            MenuAction::CloseOtherTabs(keep_idx) => {
                let removed = self.editor.close_others(keep_idx);
                if removed > 0 {
                    self.sync_open_file_poll_mtime();
                    self.status = if removed == 1 {
                        String::from("Closed 1 other tab")
                    } else {
                        format!("Closed {removed} other tabs")
                    };
                    self.poke_cursor();
                }
            }
            MenuAction::CloseTabsToRight(from_idx) => {
                let removed = self.editor.close_to_right(from_idx);
                if removed > 0 {
                    self.sync_open_file_poll_mtime();
                    self.status = if removed == 1 {
                        String::from("Closed 1 tab to the right")
                    } else {
                        format!("Closed {removed} tabs to the right")
                    };
                    self.poke_cursor();
                }
            }
            MenuAction::CloseAllTabs => {
                let removed = self.editor.close_all();
                self.sync_open_file_poll_mtime();
                self.status = if removed == 1 {
                    String::from("Closed 1 tab")
                } else {
                    format!("Closed {removed} tabs")
                };
                self.poke_cursor();
                self.collapse_split_if_empty();
            }
            MenuAction::SplitEditor => self.split_editor(),
            MenuAction::FocusGroupLeft => self.focus_editor_group(true),
            MenuAction::FocusGroupRight => self.focus_editor_group(false),
            MenuAction::CompareWithSelected { anchor, other } => {
                match self.editor.open_diff(&anchor, &other) {
                    Ok(()) => {
                        self.focus_pane(Pane::Editor);
                        self.compare_anchor = None;
                        self.sync_open_file_poll_mtime();
                        let l = anchor
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| anchor.display().to_string());
                        let r = other
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| other.display().to_string());
                        self.status = format!("Diff: {l} \u{2194} {r}");
                    }
                    Err(e) => {
                        self.status = format!("Diff failed: {e}");
                    }
                }
            }
            MenuAction::RenameSymbolAt { row, col } => {
                self.editor.collapse_carets();
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.start_rename_symbol();
            }
            MenuAction::ChangeAllOccurrencesAt { row, col } => {
                self.editor.collapse_carets();
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.start_change_all_occurrences();
            }
            MenuAction::GoToDefinitionAt { row, col } => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.request_definition_at_cursor();
            }
            MenuAction::GoToDeclarationAt { row, col } => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.request_declaration_at_cursor();
            }
            MenuAction::GoToTypeDefinitionAt { row, col } => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.request_type_definition_at_cursor();
            }
            MenuAction::GoToImplementationAt { row, col } => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.request_implementation_at_cursor();
            }
            MenuAction::GoToReferencesAt { row, col } => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.request_references_at_cursor();
            }
            MenuAction::GoToLocation { path, line, col } => {
                self.go_to_definition(path, line, col);
            }
            MenuAction::ToggleBreakpointAt { line } => {
                self.focus_pane(Pane::Editor);
                self.debug_toggle_breakpoint_line(line);
            }
            MenuAction::EditBreakpointConditionAt { line } => {
                self.focus_pane(Pane::Editor);
                self.debug_edit_condition_line(line);
            }
            MenuAction::OpenThemePicker => self.open_theme_picker(),
            MenuAction::SetTheme(theme) => self.apply_theme(theme),
            MenuAction::ToggleExplorerView(view) => self.toggle_explorer_view(view),
        }
    }

    /// Reveal `path` in macOS Finder by spawning `open -R`, which selects the
    /// entry inside its containing folder. Fire-and-forget: `open` hands off to
    /// LaunchServices and returns immediately, so the TUI never blocks. Only
    /// reachable on a local macOS session because `maybe_add_reveal_in_finder`
    /// withholds the menu entry everywhere else; the `cfg` guard keeps the call
    /// out of non-macOS builds entirely.
    fn reveal_in_finder(&mut self, path: PathBuf) {
        #[cfg(target_os = "macos")]
        {
            self.status = if std::process::Command::new("open")
                .arg("-R")
                .arg(&path)
                .spawn()
                .is_ok()
            {
                String::from("Revealed in Finder")
            } else {
                String::from("Could not reveal in Finder")
            };
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
        }
    }

    /// The editor group occupying physical column `side` (0 = left,
    /// 1 = right). When not split only side 0 exists. `editor` is always
    /// the focused group, so which physical column it sits in depends on
    /// `split_focus_left`.
    fn group_on_side(&self, side: usize) -> Option<&EditorTabs> {
        match &self.editor_split {
            None => (side == 0).then_some(&self.editor),
            Some(split) => Some(if (side == 0) == self.split_focus_left {
                &self.editor
            } else {
                split
            }),
        }
    }

    /// Physical overlay-slot index of the focused group: 0 when unsplit or
    /// focused-left, 1 when focused-right.
    fn focused_image_side(&self) -> usize {
        if self.editor_split.is_some() && !self.split_focus_left {
            1
        } else {
            0
        }
    }

    /// Re-bake both editor image slots on the next render. Used when a
    /// full-screen modal that covered both panes closes.
    fn invalidate_editor_image_layouts(&mut self) {
        self.overlays.editor[0].invalidate_layout();
        self.overlays.editor[1].invalidate_layout();
    }

    /// Bake an OSC-1337 inline-image escape sized to one editor column so
    /// the active image tab can be painted on top of ratatui's text
    /// buffer. `side` selects the physical column (and the overlay slot).
    /// Skipped when that column's active tab is text or when the host
    /// terminal can't render inline images (Terminal.app, raw SSH); in
    /// those cases the metadata header line painted by the editor widget
    /// is the entire preview the user sees.
    fn update_editor_image_overlay(&mut self, side: usize, editor_area: Rect) {
        // Cheap pre-checks that don't touch the (large) image bytes, so
        // the common per-frame path skips the clone+bake entirely.
        if self.group_on_side(side).is_none_or(|g| g.image.is_none()) {
            self.disable_editor_image(side);
            return;
        }
        let Some(path) = self.group_on_side(side).and_then(|g| g.path.clone()) else {
            self.disable_editor_image(side);
            return;
        };
        if !self.inline_images_enabled() {
            self.disable_editor_image(side);
            return;
        }
        let Some((cw_px, ch_px)) = self.cell_pixel else {
            return;
        };
        // The editor widget paints its own 1-cell border + a 1-row header
        // strip; the EditorTabs widget paints a 1-row tab strip above
        // that. Carve those off before baking the image so the OSC
        // doesn't bleed over the labels.
        let tab_strip = 1u16;
        let border = 1u16;
        let header = 1u16;
        if editor_area.height < tab_strip + 2 * border + header + 2
            || editor_area.width < 2 * border + 4
        {
            self.disable_editor_image(side);
            return;
        }
        let cell_x = editor_area.x + border;
        let cell_y = editor_area.y + tab_strip + border + header;
        let cell_w = editor_area.width.saturating_sub(2 * border);
        let cell_h = editor_area
            .height
            .saturating_sub(tab_strip + 2 * border + header);
        let desired = EditorImageLayout {
            cell_x,
            cell_y,
            cell_w,
            cell_h,
            path,
        };
        // Skip the bake when nothing about the layout changed - this is
        // the hot path on every frame an image is on screen.
        if self.overlays.editor[side].layout_matches(&desired) {
            return;
        }
        self.overlays.editor[side].request_clear_if_displayed();
        let canvas_w = cell_w as u32 * cw_px;
        let canvas_h = cell_h as u32 * ch_px;
        let bg = self.theme_bg_pixel();
        // Borrow the image bytes from the group on this side directly (a
        // disjoint-field borrow from `self.overlays`); the borrow ends at
        // the last use inside the bake, before we store into the slot.
        let baked = {
            let Some(image) = (match &self.editor_split {
                None => self.editor.image.as_ref(),
                Some(split) => {
                    if (side == 0) == self.split_focus_left {
                        self.editor.image.as_ref()
                    } else {
                        split.image.as_ref()
                    }
                }
            }) else {
                return;
            };
            crate::iterm2_inline::fit_image_auto(&image.bytes, canvas_w, canvas_h, bg)
        };
        if let Ok(baked) = baked
            && let Some(raw) = crate::iterm2_inline::build_inline_image(
                self.inline_protocol,
                &baked,
                cell_w,
                cell_h,
                false,
                crate::iterm2_inline::KITTY_ID_EDITOR_BASE + side as u32,
            )
        {
            let osc = crate::iterm2_inline::maybe_tmux_wrap(
                self.inline_protocol,
                crate::iterm2_inline::detect_tmux(),
                raw,
            );
            self.overlays.editor[side].set(osc, desired);
        }
    }

    fn disable_editor_image(&mut self, side: usize) {
        self.overlays.editor[side].disable();
    }

    /// Returns true if the cached editor-image cells need to be repainted
    /// by ratatui this frame (because the user closed/switched away from
    /// the image, or the layout changed). Called from the main loop to
    /// force a full redraw before re-emitting. ORs both split slots and
    /// consumes BOTH latches (never short-circuits).
    pub fn consume_editor_image_clear(&mut self) -> bool {
        let left = self.overlays.editor[0].consume_clear();
        let right = self.overlays.editor[1].consume_clear();
        left || right
    }

    pub fn editor_image_payload(&self, side: usize) -> Option<(&str, &EditorImageLayout)> {
        if self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.command_palette.is_some()
            || self.process_picker.is_some()
            || self.zoxide_jump.is_some()
        {
            return None;
        }
        self.overlays.editor[side].payload()
    }

    pub fn mark_editor_image_displayed(&mut self, side: usize) {
        self.overlays.editor[side].mark_displayed();
    }

    pub fn welcome_image_emit_payload(&self) -> Option<(&str, &WelcomeLayout)> {
        if self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.command_palette.is_some()
            || self.process_picker.is_some()
            || self.zoxide_jump.is_some()
            || self.connect_dialog.is_some()
            || !self.editor.is_blank_initial()
            || self.terminal_maximized
        {
            return None;
        }
        self.overlays.welcome.emit_payload()
    }

    pub fn mark_welcome_image_emitted(&mut self) {
        self.overlays.welcome.mark_emitted();
    }

    /// Keyboard navigation for spreadsheet preview tabs. All gestures are
    /// scroll-only: arrows pan one row/column, PageUp/PageDown jump by a
    /// full viewport, Home/End jump to the first/last row, Tab/Shift+Tab
    /// switch worksheets. Anything else is swallowed so a stray keystroke
    /// can't insert characters into a buffer the user can't see.
    /// Scroll the active diff to the next change hunk (forward=true) or
    /// the previous one (forward=false), wrapping around the ends so a
    /// user can keep clicking the same arrow to cycle through every
    /// hunk. Mirrors the F7 / Shift+F7 key path so click and keybinding
    /// stay in sync.
    fn jump_diff_change(&mut self, forward: bool) {
        let Some(diff) = self.editor.diff.as_mut() else {
            return;
        };
        if forward {
            diff.jump_next_change();
        } else {
            diff.jump_prev_change();
        }
    }

    /// Open the working-tree file the diff caret sits on at its line, the
    /// Enter action in the diff view. Resolves the row to a (file, line)
    /// via `DiffData::jump_target`, then reuses the explorer's pinned-tab
    /// open so the diff tab is left intact beside the opened file.
    fn jump_to_diff_file_under_caret(&mut self) {
        let root = self.tree.root.clone();
        let target = {
            let Some(diff) = self.editor.diff.as_ref() else {
                return;
            };
            let row = diff.caret_row().unwrap_or(diff.scroll);
            diff.jump_target(row, &root)
        };
        let Some(j) = target else {
            return;
        };
        match self.editor.open_pinned(&j.path) {
            Ok(()) => {
                self.editor.goto_line(j.line);
                self.focus_pane(Pane::Editor);
                self.sync_open_file_poll_mtime();
                self.poke_cursor();
                self.status = format!("{}:{}", j.path.display(), j.line);
            }
            Err(err) => {
                self.status = format!("Could not open {}: {err}", j.path.display());
            }
        }
    }

    fn handle_diff_key(&mut self, key: KeyEvent) {
        // Cmd+C / Ctrl+Shift+C on a diff selection: copy the dragged
        // substring of whichever column the selection is anchored on.
        // Must run BEFORE the scroll match below so the keystroke isn't
        // dropped as "no scroll mapping". The general editor key handler
        // short-circuits to this function when a diff is open, so without
        // this branch Cmd+C in a diff view literally never copies.
        if is_editor_copy_key(key) {
            self.copy_editor_selection();
            return;
        }
        // Enter opens the file under the caret at the matching line. The
        // caret is the selection head a click leaves behind; with no
        // selection yet we fall back to the top visible row.
        if matches!(key.code, KeyCode::Enter) {
            self.jump_to_diff_file_under_caret();
            return;
        }
        // Page = inner viewport rows minus the header + footer the diff
        // renderer reserves. Falls back to a sane default when the editor
        // hasn't laid out yet.
        let page = (self.editor.last_inner.height as usize)
            .saturating_sub(2)
            .max(1);
        let Some(diff) = self.editor.diff.as_mut() else {
            return;
        };
        // F7 / Shift+F7: jump to next / previous change hunk. Matches the
        // VS Code keybinding for "Next/Previous Change in Diff Editor" so
        // users can scan a long diff without scrolling line-by-line.
        if matches!(key.code, KeyCode::F(7)) {
            // Shared backend with the click arrows: the jump anchors on the
            // hunk the previous jump landed on (tracked in `nav_anchor`),
            // not on `scroll` — `render_diff` clamps `scroll` at the bottom,
            // which used to pin the forward arrow on the last hunk. Both
            // directions wrap so F7 at the last hunk loops to the first.
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                diff.jump_prev_change();
            } else {
                diff.jump_next_change();
            }
            return;
        }
        match key.code {
            KeyCode::Up => diff.scroll_up_by(1),
            KeyCode::Down => diff.scroll_down_by(1),
            KeyCode::PageUp => diff.scroll_up_by(page),
            KeyCode::PageDown => diff.scroll_down_by(page),
            KeyCode::Home => diff.scroll_home(),
            KeyCode::End => diff.scroll_end(),
            KeyCode::Left => diff.scroll_left_by(4),
            KeyCode::Right => diff.scroll_right_by(4),
            _ => {}
        }
    }

    fn handle_sheet_key(&mut self, key: KeyEvent) {
        let visible = sheet_visible_rows(self.editor.last_inner);
        let Some(sheet) = self.editor.sheet.as_mut() else {
            return;
        };
        let total_sheets = sheet.sheets.len();
        let current = sheet.current_sheet;
        let data = match sheet.sheets.get_mut(current) {
            Some(d) => d,
            None => return,
        };
        let row_count = data.rows.len();
        let col_count = data.col_widths.len();
        match key.code {
            KeyCode::Down if data.scroll_row + 1 < row_count => {
                data.scroll_row += 1;
            }
            KeyCode::Up => {
                data.scroll_row = data.scroll_row.saturating_sub(1);
            }
            KeyCode::Right if data.scroll_col + 1 < col_count => {
                data.scroll_col += 1;
            }
            KeyCode::Left => {
                data.scroll_col = data.scroll_col.saturating_sub(1);
            }
            KeyCode::PageDown => {
                data.scroll_row = (data.scroll_row + visible).min(row_count.saturating_sub(1));
            }
            KeyCode::PageUp => {
                data.scroll_row = data.scroll_row.saturating_sub(visible);
            }
            KeyCode::Home => {
                data.scroll_row = 0;
                data.scroll_col = 0;
            }
            KeyCode::End => {
                data.scroll_row = row_count.saturating_sub(visible.max(1));
            }
            KeyCode::Tab if total_sheets > 1 => {
                sheet.current_sheet = (current + 1) % total_sheets;
            }
            KeyCode::BackTab if total_sheets > 1 => {
                sheet.current_sheet = (current + total_sheets - 1) % total_sheets;
            }
            _ => {}
        }
    }

    /// Resize the sidebar / terminal pane while a splitter drag is in
    /// progress. The pointer's screen coordinate maps directly to the new
    /// edge: dragging horizontally sets the sidebar width to (column −
    /// activity-bar width); dragging vertically sets the terminal height
    /// to (right-pane bottom − row).
    fn handle_splitter_drag(&mut self, kind: SplitterDrag, column: u16, row: u16) {
        match kind {
            SplitterDrag::Sidebar => {
                let activity_w = ACTIVITY_BAR_WIDTH;
                let new_w = column.saturating_sub(activity_w);
                let total = activity_w + self.sidebar_width + self.last_content_width;
                let max_sidebar = total
                    .saturating_sub(activity_w)
                    .saturating_sub(RIGHT_PANE_MIN);
                self.sidebar_width =
                    new_w.clamp(SIDEBAR_WIDTH_MIN, max_sidebar.max(SIDEBAR_WIDTH_MIN));
            }
            SplitterDrag::Terminal => {
                let Some(splitter_y) = self.terminal_splitter_y else {
                    return;
                };
                // Right pane spans [splitter_y - editor_h, splitter_y +
                // current_terminal_h). Compute the right-pane bottom from
                // the captured height so a drag past it just clamps.
                let bottom = splitter_y + self.terminal_height.unwrap_or(0);
                let actual_bottom = if bottom == splitter_y {
                    // Pre-drag we may have used a percent split; fall back
                    // to the captured content height.
                    splitter_y.saturating_add(self.last_content_height)
                } else {
                    bottom
                };
                let new_h = actual_bottom.saturating_sub(row);
                let max_h = self
                    .last_content_height
                    .saturating_sub(EDITOR_HEIGHT_MIN)
                    .max(TERMINAL_HEIGHT_MIN);
                self.terminal_height = Some(new_h.clamp(TERMINAL_HEIGHT_MIN, max_h));
            }
            SplitterDrag::EditorSplit => {
                if self.editor_split.is_none() {
                    return;
                }
                // The editor area starts after the activity bar and (when
                // shown) the sidebar. The pointer column minus that left
                // edge is the new left-column width; the seam carries no
                // dedicated cell so left + right == total editor width.
                let editor_left = ACTIVITY_BAR_WIDTH
                    + if self.show_tree {
                        self.sidebar_width
                    } else {
                        0
                    };
                let total = self.last_content_width;
                let new_left = column.saturating_sub(editor_left);
                let max_left = total.saturating_sub(EDITOR_SPLIT_MIN).max(EDITOR_SPLIT_MIN);
                self.editor_split_left_width = Some(new_left.clamp(EDITOR_SPLIT_MIN, max_left));
            }
        }
    }

    /// Trash every entry in the tree's current action set (the multi-
    /// selection if non-empty, otherwise just the focused row). Refuses the
    /// workspace root, even if some other path in the set succeeds.
    fn delete_selection(&mut self) {
        let paths: Vec<PathBuf> = self
            .tree
            .action_paths()
            .into_iter()
            .filter(|p| {
                let canon_root = self.tree.root.canonicalize().ok();
                let canon_p = p.canonicalize().ok();
                p != &self.tree.root && canon_root != canon_p
            })
            .collect();
        if paths.is_empty() {
            return;
        }
        self.delete_paths(paths);
    }

    fn delete_paths(&mut self, paths: Vec<PathBuf>) {
        let total = paths.len();
        if total == 0 {
            return;
        }
        let mut affected_dirs: BTreeSet<PathBuf> = BTreeSet::new();
        for path in &paths {
            let parent = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.tree.root.clone());
            affected_dirs.insert(parent);
        }
        // One trash call for the whole batch — on macOS this routes through
        // Finder so the trash sound effect fires once for the operation
        // instead of once per file. The single-path path keeps the cheaper
        // NSFileManager backend.
        let result = if total == 1 {
            crate::widgets::file_tree::move_to_trash(&paths[0])
        } else {
            crate::widgets::file_tree::move_to_trash_bulk(&paths)
        };
        match result {
            Ok(()) => {
                for path in &paths {
                    if self.editor.matches_open_path(path) {
                        if !self.editor.close_active() {
                            *self.editor = Editor::new();
                        }
                        self.sync_open_file_poll_mtime();
                    }
                }
                for dir in &affected_dirs {
                    if let Some(idx) = self.tree.index_of_dir(dir) {
                        self.tree.refresh_children(idx);
                    }
                }
                self.tree.clear_marks();
                self.status = if total == 1 {
                    format!("Moved {} to Trash", paths[0].display())
                } else {
                    format!("Moved {total} items to Trash")
                };
            }
            Err(e) => {
                for dir in &affected_dirs {
                    if let Some(idx) = self.tree.index_of_dir(dir) {
                        self.tree.refresh_children(idx);
                    }
                }
                self.status = if total == 1 {
                    format!("Delete failed: {e}")
                } else {
                    format!("Bulk delete failed: {e}")
                };
            }
        }
    }

    /// Stash the current action paths into the explorer clipboard. The
    /// system text clipboard is left untouched so editor/terminal text
    /// copy gestures still work elsewhere in the app.
    fn copy_selection_to_explorer_clipboard(&mut self, mode: ExplorerClipMode) {
        let paths = self.tree.action_paths();
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        self.tree_clipboard = Some(ExplorerClipboard { mode, paths });
        self.status = match (mode, n) {
            (ExplorerClipMode::Copy, 1) => String::from("Copied 1 item"),
            (ExplorerClipMode::Copy, _) => format!("Copied {n} items"),
            (ExplorerClipMode::Cut, 1) => String::from("Cut 1 item"),
            (ExplorerClipMode::Cut, _) => format!("Cut {n} items"),
        };
    }

    /// Resolve the directory that an explorer paste/drop should land in,
    /// based on the currently focused node. Mirrors `create_target_dir_for`
    /// but for keyboard paste (no right-click coordinates).
    fn paste_target_dir(&self) -> PathBuf {
        let node = self.tree.nodes.get(self.tree.selected);
        crate::widgets::file_tree::create_target_dir_for(node, &self.tree.root)
    }

    /// Move (Cut) or copy (Copy) every clipboard path into `dest_dir`,
    /// then refresh affected directories and select the freshly-pasted
    /// items. Cut clears the explorer clipboard on success; Copy preserves
    /// it so the user can paste again.
    fn paste_explorer_clipboard(&mut self) {
        let dest_dir = self.paste_target_dir();
        self.paste_into(dest_dir);
    }

    /// `Ctrl/Cmd+D` from the explorer: smart toggle that mirrors the right-
    /// click "Select for Compare" / "Compare with Selected" / clear-anchor
    /// chain in a single key press.
    fn toggle_compare_on_selected_file(&mut self) {
        let Some(node) = self.tree.nodes.get(self.tree.selected) else {
            self.status = String::from("No file selected to compare");
            return;
        };
        if node.is_dir {
            self.status = String::from("Compare needs a file, not a folder");
            return;
        }
        let path = node.path.clone();
        match self.compare_anchor.as_ref() {
            Some(anchor) if anchor == &path => {
                self.compare_anchor = None;
                self.status = String::from("Cleared compare anchor");
            }
            Some(anchor) => {
                let anchor_clone = anchor.clone();
                match self.editor.open_diff(&anchor_clone, &path) {
                    Ok(()) => {
                        self.focus_pane(Pane::Editor);
                        self.compare_anchor = None;
                        self.sync_open_file_poll_mtime();
                        let l = anchor_clone
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| anchor_clone.display().to_string());
                        let r = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        self.status = format!("Diff: {l} \u{2194} {r}");
                    }
                    Err(e) => {
                        self.status = format!("Diff failed: {e}");
                    }
                }
            }
            None => {
                let label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.compare_anchor = Some(path);
                self.status =
                    format!("Selected {label} for compare — Ctrl/Cmd+D again on another file");
            }
        }
    }

    fn paste_into(&mut self, dest_dir: PathBuf) {
        let Some(clip) = self.tree_clipboard.clone() else {
            self.status = String::from("Explorer clipboard is empty");
            return;
        };
        self.apply_paste_or_drop(&dest_dir, &clip.paths, clip.mode);
        if matches!(clip.mode, ExplorerClipMode::Cut) {
            self.tree_clipboard = None;
        }
    }

    /// Shared implementation for explorer paste and drag-drop. `mode`
    /// distinguishes a move (Cut/drag) from a copy (Copy/Alt-drag).
    fn apply_paste_or_drop(&mut self, dest_dir: &Path, paths: &[PathBuf], mode: ExplorerClipMode) {
        let mut affected: BTreeSet<PathBuf> = BTreeSet::new();
        affected.insert(dest_dir.to_path_buf());
        let mut placed: Vec<PathBuf> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for src in paths {
            let result = match mode {
                ExplorerClipMode::Cut => {
                    let parent = src
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| self.tree.root.clone());
                    affected.insert(parent);
                    crate::widgets::file_tree::move_into(dest_dir, src)
                }
                ExplorerClipMode::Copy => crate::widgets::file_tree::copy_into(dest_dir, src),
            };
            match result {
                Ok(p) => {
                    if matches!(mode, ExplorerClipMode::Cut) && self.editor.matches_open_path(src) {
                        self.editor.rename_open_path(src, &p);
                    }
                    placed.push(p);
                }
                Err(e) => errors.push(format!("{}: {e}", src.display())),
            }
        }
        for dir in &affected {
            if let Some(idx) = self.tree.index_of_dir(dir) {
                self.tree.refresh_children(idx);
            }
        }
        self.tree.marked.clear();
        for p in &placed {
            self.tree.marked.insert(p.clone());
        }
        if let Some(first) = placed.first()
            && let Some(idx) = self.tree.nodes.iter().position(|n| &n.path == first)
        {
            self.tree.selected = idx;
            self.tree.anchor = idx;
        }
        let verb = match mode {
            ExplorerClipMode::Cut => "Moved",
            ExplorerClipMode::Copy => "Copied",
        };
        if !errors.is_empty() {
            self.status = format!(
                "{verb} {}/{}; failed: {}",
                placed.len(),
                paths.len(),
                errors.join("; ")
            );
        } else if placed.len() == 1 {
            self.status = format!("{verb} 1 item to {}", dest_dir.display());
        } else {
            self.status = format!("{verb} {} items to {}", placed.len(), dest_dir.display());
        }
    }

    fn open_create_prompt(&mut self, kind: CreateKind, target_dir: PathBuf) {
        let label = match kind {
            CreateKind::File => format!("New File in {}", target_dir.display()),
            CreateKind::Folder => format!("New Folder in {}", target_dir.display()),
        };
        self.prompt = Some(Prompt {
            label,
            buffer: String::new(),
            kind: PromptKind::Create(kind),
            target_dir,
            error: None,
        });
    }

    fn open_rename_prompt(&mut self, path: PathBuf) {
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.tree.root.clone());
        let current = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let label = format!("Rename {}", path.display());
        self.prompt = Some(Prompt {
            label,
            buffer: current,
            kind: PromptKind::Rename(path),
            target_dir: parent,
            error: None,
        });
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                self.status = String::from("Cancelled");
            }
            KeyCode::Enter => self.commit_prompt(),
            KeyCode::Backspace => {
                if let Some(p) = self.prompt.as_mut() {
                    p.buffer.pop();
                    p.error = None;
                }
            }
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER)
                    && let Some(p) = self.prompt.as_mut()
                {
                    p.buffer.push(c);
                    p.error = None;
                }
            }
            _ => {}
        }
    }

    fn commit_prompt(&mut self) {
        let Some(prompt) = self.prompt.as_ref() else {
            return;
        };
        let kind = prompt.kind.clone();
        match kind {
            PromptKind::Create(create_kind) => {
                let name = prompt.buffer.trim().to_string();
                if let Err(msg) = crate::widgets::file_tree::validate_new_name(&name) {
                    if let Some(p) = self.prompt.as_mut() {
                        p.error = Some(msg.to_string());
                    }
                    return;
                }
                let target_dir = prompt.target_dir.clone();
                let result = match create_kind {
                    CreateKind::File => {
                        crate::widgets::file_tree::create_file_in(&target_dir, &name)
                    }
                    CreateKind::Folder => {
                        crate::widgets::file_tree::create_folder_in(&target_dir, &name)
                    }
                };
                match result {
                    Ok(path) => {
                        self.prompt = None;
                        self.status = match create_kind {
                            CreateKind::File => format!("Created file {}", path.display()),
                            CreateKind::Folder => format!("Created folder {}", path.display()),
                        };
                        if let Some(idx) = self.tree.index_of_dir(&target_dir) {
                            self.tree.refresh_children(idx);
                            if let Some(new_idx) =
                                self.tree.nodes.iter().position(|n| n.path == path)
                            {
                                self.tree.select(new_idx);
                            }
                        }
                        if create_kind == CreateKind::File {
                            if let Err(e) = self.editor.open(&path) {
                                self.status = format!("Created but could not open: {e}");
                            } else {
                                self.sync_open_file_poll_mtime();
                                self.focus_pane(Pane::Editor);
                            }
                        }
                    }
                    Err(e) => {
                        if let Some(p) = self.prompt.as_mut() {
                            p.error = Some(e.to_string());
                        }
                    }
                }
            }
            PromptKind::Rename(old_path) => {
                let new_name = prompt.buffer.trim().to_string();
                let parent = prompt.target_dir.clone();
                match crate::widgets::file_tree::rename_in(&parent, &old_path, &new_name) {
                    Ok(new_path) => {
                        self.prompt = None;
                        self.status = format!("Renamed to {}", new_path.display());
                        if let Some(idx) = self.tree.index_of_dir(&parent) {
                            self.tree.refresh_children(idx);
                            if let Some(new_idx) =
                                self.tree.nodes.iter().position(|n| n.path == new_path)
                            {
                                self.tree.select(new_idx);
                            }
                        }
                        self.editor.rename_open_path(&old_path, &new_path);
                        self.sync_open_file_poll_mtime();
                    }
                    Err(e) => {
                        if let Some(p) = self.prompt.as_mut() {
                            p.error = Some(e.to_string());
                        }
                    }
                }
            }
            PromptKind::RenameSymbol { path, row, col } => {
                let new_name = prompt.buffer.trim().to_string();
                if new_name.is_empty() {
                    if let Some(p) = self.prompt.as_mut() {
                        p.error = Some("Name cannot be empty".to_string());
                    }
                    return;
                }
                let Some(lsp) = self.lsp.as_mut() else {
                    if let Some(p) = self.prompt.as_mut() {
                        p.error = Some("No language server for this file".to_string());
                    }
                    return;
                };
                let id = lsp.request_rename(path, row as u32, col as u32, new_name);
                self.rename_request_id = Some(id);
                self.prompt = None;
                self.status = String::from("Renaming symbol...");
            }
            PromptKind::BreakpointCondition { path, line } => {
                let expr = prompt.buffer.clone();
                self.prompt = None;
                self.commit_breakpoint_condition(path, line, &expr);
            }
        }
    }
}

/// Kitty keyboard-protocol flags croft requests on startup.
///
/// `DISAMBIGUATE_ESCAPE_CODES` is the bit that makes terminals deliver
/// modifier keys (notably Cmd/Super on macOS in iTerm2 ≥3.5, Ghostty, kitty,
/// WezTerm) as real key events rather than swallowing them or routing them to
/// menus. We deliberately do NOT request `REPORT_ALL_KEYS_AS_ESCAPE_CODES`
/// because that re-encodes ordinary printable input and would break typing.
fn keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
}

/// True when the "command" modifier is effectively held. That is `Super`
/// (Cmd) everywhere; on Termux (Android, no Cmd key) `Ctrl` stands in for it
/// too, mirroring VS Code's Linux keymap so the Super-only chords stay
/// reachable. `termux` is threaded in explicitly to keep the decision pure
/// and testable.
fn cmd_active(mods: KeyModifiers, termux: bool) -> bool {
    mods.contains(KeyModifiers::SUPER) || (termux && mods.contains(KeyModifiers::CONTROL))
}

/// `cmd_active` over the live environment (cached Termux detection).
fn has_cmd(mods: KeyModifiers) -> bool {
    cmd_active(mods, crate::iterm2_inline::detect_termux())
}

/// Build the OSC 0 escape sequence that sets the terminal's window/icon title.
///
/// Format: `ESC ] 0 ; <title> BEL`.  Control bytes that would break the escape
/// (BEL, ESC, newlines) are stripped from `title` so untrusted input cannot
/// inject further sequences.
fn set_title_seq(title: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(title.len() + 5);
    out.extend_from_slice(b"\x1b]0;");
    for byte in title.as_bytes() {
        match *byte {
            0x00..=0x1f | 0x7f => continue,
            b => out.push(b),
        }
    }
    out.push(0x07);
    out
}

/// iTerm2 proprietary OSC that overrides the *session* (not profile-on-disk)
/// bg colour. The `srgb:` prefix forces iTerm2 to interpret the hex bytes
/// as sRGB regardless of the user's "Use sRGB colour space" profile setting,
/// so combining this with an OSC-1337 PNG canvas filled with the same hex
/// guarantees both surfaces display the same physical pixel.
/// Format: `ESC ] 1 3 3 7 ; SetColors=bg=srgb:RRGGBB BEL`.
fn set_session_bg_srgb_seq(rgb: (u8, u8, u8)) -> String {
    format!(
        "\x1b]1337;SetColors=bg=srgb:{:02x}{:02x}{:02x}\x07",
        rgb.0, rgb.1, rgb.2,
    )
}

/// Revert the iTerm2 session bg to the user's profile default. Emitted on
/// exit so the user's shell doesn't inherit croft's forced bg colour.
fn reset_session_bg_seq() -> String {
    String::from("\x1b]1337;SetColors=bg=default\x07")
}

fn build_title(workspace: &std::path::Path) -> String {
    let name = workspace
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| workspace.display().to_string());
    format!("{APP_NAME}  {name}")
}

/// Returns true if the given key event should copy the terminal pane's
/// current selection to the clipboard.  Recognises:
///   * `Ctrl+Shift+C` — universal Linux terminal copy convention; doesn't
///     collide with `Ctrl+C` (SIGINT).
///   * `Cmd+C`        — for terminals that deliver Super via the kitty
///     keyboard protocol.
fn is_terminal_copy_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'c') {
        return false;
    }
    let ctrl_shift = key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::SHIFT);
    let super_only = key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::CONTROL);
    ctrl_shift || super_only
}

/// Returns true if the given key event should jump to the Search sidebar
/// view (VS Code's Ctrl/Cmd+Shift+F "Find in Files" gesture).
/// `Ctrl+D` / `Cmd+D` (no Shift). Used in the Explorer to drive the
/// "compare two files" flow:
///   * no anchor yet → stash the highlighted file as the anchor;
///   * anchor + different file → open a side-by-side diff;
///   * anchor + same file → clear the anchor (toggle off).
fn is_compare_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'d') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

fn is_search_jump_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'f') {
        return false;
    }
    let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let has_ctrl_or_super = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER);
    has_shift && has_ctrl_or_super
}

/// VS Code-style `Cmd+F` / `Ctrl+F` inside the editor: open the inline
/// Find bar. Same chord as the Explorer's "New File" — the dispatch in
/// `handle_key` gives the Explorer handler first refusal so this only
/// fires when the editor is the focused pane.
fn is_editor_find_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'f') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// VS Code-style `Cmd+P` / `Ctrl+P`: open the Quick Open file finder.
/// Plain `p` with no modifier (or with Shift / Alt only) falls through so
/// the editor still receives literal `p` keystrokes.
fn is_file_finder_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'p') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// `Ctrl/Cmd+Shift+P`: open the Command Palette from any pane, mirroring VS
/// Code. Shares the `is_cmd_shift_letter` helper with the sidebar-view jumps
/// so the SHIFT / case handling is identical, and sits after
/// `is_file_finder_key` in the dispatch (which rejects SHIFT, so `Cmd+P` and
/// `Cmd+Shift+P` never collide).
fn is_command_palette_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'p')
}

/// `Cmd+/` (macOS) / `Ctrl+/` (Linux / Termux): Toggle Line Comment. Rejects
/// Shift and Alt so it never collides with a future chord on `/`.
fn is_toggle_line_comment_key(key: KeyEvent) -> bool {
    if key.code != KeyCode::Char('/') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// `Shift+Alt+A`: Toggle Block Comment (the VS Code default). Requires both
/// Shift and Alt and rejects the command modifier. Also reachable from the
/// Command Palette, which is the parity path on terminals that swallow the
/// Alt+letter chord.
fn is_toggle_block_comment_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'a') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SHIFT)
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
}

/// `Alt+Z`: View: Toggle Word Wrap (the VS Code default). Rejects Shift and
/// the command modifier. Also reachable from the Command Palette.
fn is_toggle_wrap_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'z') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Explorer-pane shortcut: `Cmd+Z` — open the zoxide jump popup. `z` for
/// **z**oxide; the user's shell still uses `j` for the same jump. Requires
/// SUPER (iTerm2 already forwards Cmd+Z as `Char('z') + SUPER` for the
/// editor's undo via the `CMD_Z` GlobalKeyMap entry). Off Termux it rejects
/// CONTROL so a terminal `Ctrl+Z` suspend never reaches it and it stays
/// distinct from the Ctrl-based terminal-toggle chords on `j`; on Termux,
/// where Ctrl is the Cmd surrogate, `Ctrl+Z` opens the popup (this predicate
/// only runs while the Explorer is focused, so there is no suspend to clash).
/// SHIFT is rejected because `Cmd+Shift+Z` is the reserved redo chord.
/// Editor-pane `Cmd+Z` is untouched: this predicate is only consulted from
/// `handle_explorer_shortcut`, which runs solely when the Explorer is
/// focused (and gets first refusal in the dispatch chain).
fn is_tree_zoxide_jump_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'z') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    // `has_cmd` is SUPER-only off Termux (so a terminal `Ctrl+Z` suspend is
    // never swallowed); on Termux Ctrl is the command key, and this predicate
    // only runs while the Explorer is focused, so there is no suspend to clash.
    has_cmd(key.modifiers)
}

/// Explorer-pane shortcut: `Cmd+F` / `Ctrl+F` (no Shift, no Alt) - "New File".
fn is_tree_new_file_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'f') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Explorer-pane shortcut: `Cmd+Shift+N` / `Ctrl+Shift+N` - "New Folder".
/// The global dispatcher gives the Explorer-context handler first refusal
/// so this only fires when the tree is the focused pane; outside Explorer
/// the chord is unbound and falls through to the focused pane.
fn is_tree_new_folder_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'n') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Explorer-pane shortcut: `Cmd+R` / `Ctrl+R` (no Shift) OR plain `F2` -
/// "Rename". F2 is the cross-IDE convention; Cmd+R matches the user-
/// requested chord (overrides iTerm's "Clear buffer" via setup-iterm2).
fn is_tree_rename_key(key: KeyEvent) -> bool {
    if matches!(key.code, KeyCode::F(2)) {
        return true;
    }
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'r') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Explorer-pane shortcut: `Cmd+Opt+R` / `Ctrl+Opt+R` - "Reveal in Finder".
/// The Alt requirement keeps this disjoint from `Cmd+R` (Rename), which
/// rejects Alt, so the two R chords never collide. The caller additionally
/// gates this to a local macOS session, matching the context-menu entry.
fn is_tree_reveal_in_finder_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'r') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Explorer-pane shortcut: `Cmd+/` / `Ctrl+/` - "Make root". Re-roots the
/// workspace at the selected folder.
fn is_tree_make_root_key(key: KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Char('/')) {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Explorer-pane shortcut: `Cmd+Shift+/` / `Ctrl+Shift+/` - "Make root at
/// parent". Re-roots the workspace at the parent of the selected node
/// (file or folder). The selected node itself remains on disk and the
/// new root is one filesystem level up. Terminals that strip Shift on
/// shifted punctuation report `?`; we accept both `/+Shift` and `?` to
/// cover the spread.
fn is_tree_make_parent_root_key(key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    let has_ctrl_or_super = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER);
    if !has_ctrl_or_super {
        return false;
    }
    match key.code {
        KeyCode::Char('/') => key.modifiers.contains(KeyModifiers::SHIFT),
        KeyCode::Char('?') => true,
        _ => false,
    }
}

/// `Ctrl/Cmd+Shift+E`: jump to the Explorer sidebar view from any pane.
/// Matches VS Code's "View: Show Explorer" command.
fn is_explorer_jump_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'e')
}

/// `Ctrl/Cmd+Shift+S`: jump to the Source Control sidebar view from any pane.
/// This is croft's Source Control gesture. Croft has no editor Save-As (Cmd+S
/// alone saves), so claiming Cmd+Shift+S is collision-free even while editing.
fn is_source_control_jump_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 's')
}

/// `Ctrl/Cmd+Shift+D`: jump to the Run and Debug sidebar view from any pane.
/// Matches VS Code's "View: Show Run and Debug" command. iTerm2 binds the
/// same chord to "Split Horizontally with Same Profile" — `setup-iterm2`
/// relocates that menu item to Cmd+Opt+Shift+D so this chord reaches croft.
fn is_run_debug_jump_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'd')
}

/// `Ctrl/Cmd+Shift+R`: jump to the Remote (SSH) sidebar view from any pane.
/// VS Code uses this for "Remote Explorer" in the Remote Development pack —
/// same intent.
fn is_remote_jump_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'r')
}

fn is_drop_to_local_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'l')
}

fn is_cmd_shift_letter(key: KeyEvent, letter: char) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&letter) {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let has_ctrl_or_super = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER);
    has_shift && has_ctrl_or_super
}

/// `Cmd+B` (macOS) / `Ctrl+B` (Linux): toggle the primary side bar (left
/// pane) visibility, mirroring VS Code's "View: Toggle Primary Side Bar".
/// Accepts SUPER *or* CONTROL so the chord behaves identically whether
/// iTerm2 forwards `Cmd+B` as the CSI-u Super sequence (see `src/iterm2.rs`)
/// or a raw `Ctrl+B` control byte arrives on a remote Linux session. SHIFT
/// and ALT must not be held, so it never collides with a future bracketed
/// or word-motion chord on `b`.
fn is_sidebar_toggle_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'b') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Returns true if the given key event should toggle the terminal pane.
/// VS Code uses `Ctrl+`` ` `` (backtick); we use `Ctrl+J` to match its
/// "Toggle Terminal Panel" shortcut, which is more reliable to type on
/// non-US keyboards. Case-insensitive on the letter. SHIFT must NOT be
/// held — Ctrl+Shift+J is reserved for the maximize-terminal chord and
/// must not double-fire as a toggle.
fn is_terminal_toggle_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'j') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Ctrl+Shift+J`: toggle "maximize terminal" — the editor / welcome pane
/// collapses and the terminal fills the right column next to the Explorer.
/// Pressing it again restores the previous editor↔terminal split.
fn is_terminal_maximize_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'j') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SUPER)
}

/// `Cmd+T` (or legacy `Ctrl+Shift+T`): spawn an additional terminal next
/// to the active one. iTerm2's GlobalKeyMap forwards `Cmd+T` as
/// `ESC [ 116 ; 9 u`, which crossterm decodes to `Char('t') + SUPER`.
/// In the SUPER branch we reject SHIFT so the `Cmd+Shift+T` focus chord
/// (see `is_terminal_focus_key`) does not double-fire as a split, and
/// reject ALT so `Cmd+Opt+T` (iTerm2's relocated New Tab) is left alone.
fn is_terminal_split_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'t') {
        return false;
    }
    if has_cmd(key.modifiers)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
    {
        return true;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Cmd+\`: split the editor into two side-by-side columns (VS Code's
/// `workbench.action.splitEditor`). iTerm2's GlobalKeyMap forwards it as
/// `ESC [ 92 ; 9 u`, which crossterm decodes to `Char('\\') + SUPER`.
/// SUPER-only; reject SHIFT/ALT so it can't double-fire with a future
/// modified backslash chord.
fn is_editor_split_key(key: KeyEvent) -> bool {
    let KeyCode::Char('\\') = key.code else {
        return false;
    };
    // SUPER off Termux; on Termux `Ctrl+\` is the command surrogate (note this
    // shadows the shell's SIGQUIT there, the accepted cost of Ctrl parity).
    has_cmd(key.modifiers)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// `Cmd+Opt+Left`: move keyboard focus to the LEFT editor group while
/// split. Distinct from the plain `Opt+Left` word-left motion (no SUPER)
/// by requiring both SUPER and ALT. iTerm2 forwards it as `ESC [ 1 ; 12 D`.
fn is_focus_group_left_key(key: KeyEvent) -> bool {
    let KeyCode::Left = key.code else {
        return false;
    };
    has_cmd(key.modifiers) && key.modifiers.contains(KeyModifiers::ALT)
}

/// `Cmd+Opt+Right`: move keyboard focus to the RIGHT editor group while
/// split. Mirror of `is_focus_group_left_key`. iTerm2 forwards it as
/// `ESC [ 1 ; 12 C`.
fn is_focus_group_right_key(key: KeyEvent) -> bool {
    let KeyCode::Right = key.code else {
        return false;
    };
    has_cmd(key.modifiers) && key.modifiers.contains(KeyModifiers::ALT)
}

/// `Cmd+Shift+T` (Mac SUPER+SHIFT+T): focus the Terminal pane from any
/// pane, un-hiding it if it was collapsed via Ctrl+J. SUPER-only so the
/// cross-platform `Ctrl+Shift+T` (split-terminal) chord keeps its
/// historical meaning - the two chords share the same letter but are
/// disambiguated by the modifier.
fn is_terminal_focus_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'t') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Cmd+W` (or legacy `Ctrl+Shift+W`): close the active terminal (no-op if
/// it's the only one). Plain `Ctrl+W` is intentionally excluded so the shell's
/// delete-word-backward keeps working; the close action requires either the
/// Super (Cmd) modifier or the Ctrl+Shift pair.
fn is_terminal_close_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'w') {
        return false;
    }
    let cmd_w =
        key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::META);
    let ctrl_shift_w = key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::SHIFT);
    cmd_w || ctrl_shift_w
}

/// `Cmd+]`: cycle to the next (right) terminal in the pane. iTerm2's
/// GlobalKeyMap forwards it as `ESC [ 93 ; 9 u` (modifier 9 = base +
/// Super), which crossterm decodes to `Char(']') + SUPER`. Brackets are
/// used because every arrow option is blocked: Ctrl+arrows by macOS
/// Spaces, Option+arrows by shell word-motion, Cmd+arrows by user choice.
/// Accepts the shifted glyph `}` defensively in case a terminal pre-applies
/// the shift.
fn is_terminal_cycle_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    (c == ']' || c == '}') && has_cmd(key.modifiers)
}

/// `Cmd+[`: cycle to the previous (left) terminal in the pane. Mirror of
/// `is_terminal_cycle_key` for the reverse direction.
fn is_terminal_cycle_back_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    (c == '[' || c == '{') && has_cmd(key.modifiers)
}

/// Returns true if the given key event should trash the currently-selected
/// node in the file tree. Recognises:
///
/// * `KeyCode::Delete` — the Forward-Delete key on full-size keyboards
///   (and `fn+Delete` on Mac laptops).
/// * `KeyCode::Backspace` — the key labeled "delete" on every Mac keyboard.
///   The tree pane has no text input that needs Backspace, so plain
///   Backspace is safe here.
/// * `Cmd+Backspace` — the macOS Finder gesture for trashing.
///
/// This helper is only called from `handle_tree_key`, so plain Backspace is
/// only swallowed when the tree pane has focus; the editor and terminal panes
/// continue to consume Backspace for their own purposes.
fn is_delete_node_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Delete | KeyCode::Backspace)
}

/// Returns true if the given key event should trigger "Save".
/// Recognises Ctrl+S (cross-platform) and Cmd/Super+S (macOS-style).
/// Case-insensitive on the letter so Shift+Ctrl+S also works.
fn is_word_continuation(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_completion_trigger_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if c != ' ' {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

fn is_save_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'s') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    let has_super = key.modifiers.contains(KeyModifiers::SUPER);
    let has_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // Cmd+Shift+S is reserved for the Source Control jump
    // (`is_source_control_jump_key`), so reject Shift when Super is
    // held. Ctrl+Shift+S still triggers Save because some terminals
    // capitalise the letter when Ctrl is held and the user genuinely
    // means Ctrl+S — that's the existing `shift_ctrl_s_is_save_key`
    // contract we want to preserve.
    if has_super && has_shift {
        return false;
    }
    has_super || has_ctrl
}

/// Read the system clipboard. Used by the search input when Cmd+V arrives as
/// a key event rather than bracketed paste content.
fn read_system_clipboard() -> Option<String> {
    crate::clipboard::read_string()
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

/// Returns true if the given key event should copy the editor's current
/// selection to the system clipboard. Recognises plain `Ctrl+C` and `Cmd+C`
/// — there's no SIGINT collision since the editor pane is not a shell.
fn is_editor_copy_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'c') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Cut: `Ctrl+X` / `Cmd+X`.
fn is_editor_cut_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'x') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Paste in the search input. Normal terminal paste is handled via
/// `Event::Paste`; `setup-iterm2` maps Cmd+V to a CSI-u Cmd+V sequence, and
/// the raw Ctrl+V byte is accepted as a fallback for older setups.
fn is_search_paste_key(key: KeyEvent) -> bool {
    is_clipboard_paste_key(key)
}

/// Human-readable name of a Search input field, for status-line messages.
fn search_field_name(field: crate::widgets::search::SearchField) -> &'static str {
    use crate::widgets::search::SearchField;
    match field {
        SearchField::Query => "search",
        SearchField::Replace => "replace",
        SearchField::Include => "files-to-include",
        SearchField::Exclude => "files-to-exclude",
    }
}

fn is_editor_paste_key(key: KeyEvent) -> bool {
    is_clipboard_paste_key(key)
}

fn is_clipboard_paste_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if c == '\u{16}' {
        return true;
    }
    if !c.eq_ignore_ascii_case(&'v') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

fn is_search_editing_shortcut(key: KeyEvent) -> bool {
    is_search_paste_key(key)
        || is_editor_select_all_key(key)
        || is_editor_copy_key(key)
        || is_editor_cut_key(key)
}

/// Select-all: `Ctrl+A` / `Cmd+A`.
fn is_editor_select_all_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'a') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Undo: `Ctrl+Z` / `Cmd+Z`. Plain Shift is ignored (Shift+Cmd+Z is reserved
/// for redo, which croft does not implement yet).
fn is_editor_undo_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'z') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Editor-pane Rename Symbol: plain `F2` (VS Code's binding). In the Explorer
/// tree the same key renames the selected file (`is_tree_rename_key`); the
/// pane in focus decides which fires.
fn is_rename_symbol_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(2))
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// True for `.ts`/`.tsx`/`.js`/`.jsx` paths — the languages served by the
/// croft-managed vtsls, used to re-open just those docs after vtsls installs.
/// Collapse a language to the server "family" it shares a managed server with.
/// vtsls serves the whole TypeScript/JS family but reports its config language
/// as `TypeScript`, so all four map together; every other language is its own
/// family. Used to decide which open docs to re-open after a managed install.
fn lsp_server_family(lang: crate::lsp::Language) -> crate::lsp::Language {
    use crate::lsp::Language;
    match lang {
        Language::Tsx | Language::JavaScript | Language::Jsx => Language::TypeScript,
        other => other,
    }
}

/// True when `path`'s language is served by one of the just-installed servers,
/// so its LSP sync state should be dropped to force a re-open + re-probe.
fn just_installed_covers_path(installed: &[crate::lsp::Language], path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let Some(lang) = crate::lsp::Language::from_extension(ext) else {
        return false;
    };
    installed
        .iter()
        .any(|&i| lsp_server_family(i) == lsp_server_family(lang))
}

/// Editor-pane Change All Occurrences: `Cmd+F2` (macOS) or `Ctrl+F2` (Linux),
/// matching VS Code on each platform so local and remote behave identically.
/// iTerm2 may deliver Cmd folded onto the Meta/ALT bit (the same reason
/// Cmd+click arrives as ALT for go-to-definition), so any of SUPER / CONTROL /
/// ALT alongside F2 counts. Plain F2 (no modifiers) stays Rename Symbol.
fn is_change_all_occurrences_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(2))
        && (key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT))
}

/// Editor-pane Go to Definition: plain `F12` (VS Code's binding). The F12
/// navigation family splits on the modifier, matching VS Code: bare F12 is
/// Definition, `Shift+F12` is Declaration, `Ctrl+F12` is Type Definition, and
/// `Cmd+F12` (SUPER) is Implementations. Definition therefore requires F12 with
/// none of Shift / Ctrl / Super held. (Alt is still accepted: iTerm2 can fold a
/// held Cmd onto the Meta/Alt bit for some events, and Option+F12 is otherwise
/// unused, so leaving Alt on Definition is harmless.)
fn is_go_to_definition_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
}

/// Editor-pane Go to References: `Shift+F12`, VS Code's real binding for this
/// action. This is the chord croft previously gave to Go to Declaration; with
/// References taking its rightful VS Code default, Declaration moved one
/// modifier over to `Ctrl+Shift+F12`. References therefore requires F12 with
/// Shift held but neither Ctrl (that is Declaration) nor Super (Implementations).
fn is_go_to_references_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
}

/// Editor-pane Go to Declaration: `Ctrl+Shift+F12`. VS Code leaves Declaration
/// unbound, so croft assigns it within the F12 navigation family. It moved off
/// the bare `Shift+F12` it once held when Go to References (added later) claimed
/// that VS Code default, so Declaration now needs both Shift and Ctrl (Super
/// would be Implementations). iTerm2 emits `CSI 24 ; 6 ~` for Ctrl+Shift+F12,
/// which crossterm decodes to `F(12) + SHIFT + CONTROL`; `iterm2.rs` also
/// installs a defensive GlobalKeyMap forwarder emitting exactly those bytes so a
/// future iTerm2 default cannot swallow the chord.
fn is_go_to_declaration_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
}

/// Editor-pane Go to Type Definition: `Ctrl+F12`. VS Code leaves Type
/// Definition unbound, so croft assigns it within the F12 navigation family.
/// iTerm2 emits `CSI 24 ; 5 ~` for Ctrl+F12, which crossterm decodes to
/// `F(12) + CONTROL` (the same legacy-tilde path that delivers plain F12 and
/// Shift+F12), so no iTerm2 GlobalKeyMap forwarder is needed.
fn is_go_to_type_definition_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// Editor-pane Go to Implementations: `Cmd+F12` (SUPER), VS Code's real macOS
/// binding for this action. Unlike its F12-family siblings, Cmd+F12 is captured
/// by macOS, so `iterm2.rs` installs a GlobalKeyMap forwarder emitting
/// `CSI 24 ; 9 ~`; crossterm decodes that to `F(12) + SUPER`. This must not
/// collide with the three siblings: bare F12 is Definition, `Shift+F12` is
/// Declaration, `Ctrl+F12` is Type Definition.
fn is_go_to_implementation_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
}

fn chord_status_text(op: char, count: usize) -> String {
    if count > 0 {
        format!("Pending: {op} (count {count})")
    } else {
        format!("Pending: {op}")
    }
}

fn is_vim_chord_letter(key: KeyEvent, letter: char) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if c != letter {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
}

fn is_vim_chord_g(key: KeyEvent) -> bool {
    is_vim_chord_letter(key, 'g')
}

fn is_vim_chord_d(key: KeyEvent) -> bool {
    is_vim_chord_letter(key, 'd')
}

fn is_vim_chord_y(key: KeyEvent) -> bool {
    is_vim_chord_letter(key, 'y')
}

fn is_vim_goto_bottom(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'g') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
}

fn chord_digit_value(key: KeyEvent) -> Option<usize> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let d = c.to_digit(10)? as usize;
    if key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return None;
    }
    Some(d)
}

fn cmd_only_digit_value(key: KeyEvent) -> Option<usize> {
    let d = chord_digit_value(key)?;
    if !key.modifiers.contains(KeyModifiers::SUPER) {
        return None;
    }
    Some(d)
}

/// Cmd+E toggles native modal (vim) editing in the editor pane. A plain
/// Cmd+letter reaches croft reliably as SUPER (the same path as Cmd+S / Cmd+C),
/// unlike Option+letter which emits a glyph on macOS; and it stays out of the
/// Cmd+Shift+letter space reserved for activity-bar view jumps.
fn is_vim_toggle_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('e') | KeyCode::Char('E'))
        && key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

fn reverse_find(kind: crate::vim::FindKind) -> crate::vim::FindKind {
    use crate::vim::FindKind;
    match kind {
        FindKind::CharForward => FindKind::CharBackward,
        FindKind::CharBackward => FindKind::CharForward,
        FindKind::TillForward => FindKind::TillBackward,
        FindKind::TillBackward => FindKind::TillForward,
    }
}

/// Vertical / whole-file motions operate linewise under an operator (`dj`,
/// `dG`), unlike the charwise horizontal and word motions.
fn vim_motion_is_linewise(motion: crate::vim::Motion) -> bool {
    use crate::vim::Motion;
    matches!(
        motion,
        Motion::Up | Motion::Down | Motion::FileStart | Motion::FileEnd | Motion::GotoLine(_)
    )
}

/// `f`/`t`/`e` land on a char that the operator should include, so the
/// deletion range needs to extend one past the landing column.
fn vim_motion_is_forward_inclusive(motion: crate::vim::Motion) -> bool {
    use crate::vim::{FindKind, Motion};
    matches!(
        motion,
        Motion::WordEnd
            | Motion::Find(FindKind::CharForward, _)
            | Motion::Find(FindKind::TillForward, _)
    )
}

/// First char index `>= from` where `term` begins in `line`, comparing by
/// char (not byte) so the result indexes the editor's char-based cursor.
fn find_char_index(line: &str, term: &str, from: usize) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let needle: Vec<char> = term.chars().collect();
    let tlen = needle.len();
    if tlen == 0 || chars.len() < tlen {
        return None;
    }
    (from..=chars.len() - tlen).find(|&i| chars[i..i + tlen] == needle[..])
}

/// Last char index `< limit` where `term` begins in `line`.
fn rfind_char_index(line: &str, term: &str, limit: usize) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let needle: Vec<char> = term.chars().collect();
    let tlen = needle.len();
    if tlen == 0 || chars.len() < tlen {
        return None;
    }
    let upper = limit.min(chars.len() - tlen + 1);
    (0..upper).rev().find(|&i| chars[i..i + tlen] == needle[..])
}

fn is_plain_letter(key: KeyEvent, letter: char) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if c != letter {
        return false;
    }
    !(key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER)
        || key.modifiers.contains(KeyModifiers::ALT))
}

fn is_editor_line_home_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'a') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn is_editor_line_end_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'e') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn is_editor_kill_to_eol_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'k') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn is_editor_kill_to_bol_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'u') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn is_editor_open_line_below_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if c != 'o' {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
}

fn is_editor_open_line_above_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'o') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Close active tab: `Ctrl+W` / `Cmd+W`.
fn is_close_tab_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'w') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER)
        || key.modifiers.contains(KeyModifiers::META)
}

/// Cmd+1..9 / Ctrl+1..9 — jump straight to that tab (1-based; returns the
/// 0-based index). Anything outside that range returns `None`.
fn jump_to_tab_index(key: KeyEvent) -> Option<usize> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    if !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::META)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
    {
        return None;
    }
    let d = c.to_digit(10)?;
    if !(1..=9).contains(&d) {
        return None;
    }
    Some((d - 1) as usize)
}

/// Map a screen row to the tree row that should be highlighted as a drop
/// target. Hovering a directory targets that directory; hovering a file
/// targets its parent directory (so dragging onto a file row drops the
/// payload as siblings of that file). Returns `None` when the pointer is
/// over empty tree space, when the row would be a no-op (hovering an
/// item that's part of the drag set), or when the resolved directory is
/// inside one of the dragged folders.
fn drag_target_index(
    tree: &crate::widgets::file_tree::FileTree,
    y: u16,
    drag_paths: &[PathBuf],
) -> Option<usize> {
    let idx = tree.node_at_y(y)?;
    let node = tree.nodes.get(idx)?;
    let dir_idx = if node.is_dir {
        idx
    } else {
        let parent = node.path.parent()?;
        tree.index_of_dir(parent)?
    };
    let dir_path = tree.nodes.get(dir_idx)?.path.clone();
    for src in drag_paths {
        if crate::widgets::file_tree::is_descendant_or_same(&dir_path, src) {
            return None;
        }
        if let Some(parent) = src.parent()
            && parent == dir_path
            && drag_paths.len() == 1
        {
            // Dropping a single source onto its own parent is a no-op;
            // don't show a highlight so the user knows nothing will move.
            return None;
        }
    }
    Some(dir_idx)
}

fn drop_target_dir(tree: &crate::widgets::file_tree::FileTree, target_idx: usize) -> PathBuf {
    tree.nodes
        .get(target_idx)
        .map(|n| n.path.clone())
        .unwrap_or_else(|| tree.root.clone())
}

/// Parse a bracketed-paste payload that originated from a Finder-style
/// drag-and-drop into the host terminal. Returns the absolute existing
/// paths the user dropped, or an empty Vec when the payload is plain text
/// (in which case the caller falls back to the normal paste path). Each
/// candidate token survives only if it resolves to an existing absolute
/// path on disk, so a stray paste of "/usr/bin" worth of typed text still
/// behaves like text.
pub fn parse_dropped_paths(s: &str) -> Vec<PathBuf> {
    parsed_drop_tokens(s)
        .into_iter()
        .filter(|p| p.is_absolute() && p.exists())
        .filter(|p| !is_iterm2_inline_image_drop(p))
        .collect()
}

/// Parse a bracketed-paste payload as candidate filesystem paths WITHOUT
/// requiring the paths to exist on this machine. Used by the remote-
/// launched croft to recognise drops whose paths refer to files on the
/// user's local Mac, which the relay will fetch over scp.
pub fn parse_foreign_dropped_paths(s: &str) -> Vec<PathBuf> {
    parsed_drop_tokens(s)
        .into_iter()
        .filter(|p| p.is_absolute())
        .filter(|p| !is_iterm2_inline_image_drop(p))
        .collect()
}

/// True when the dropped path is iTerm2's temp file for an inline-image
/// drag. iTerm2 lets the user drag any OSC-1337 inline image out of the
/// terminal; on mouse-up it writes the image bytes to `$TMPDIR/iTerm2.<rand>.png`
/// and re-injects the path into the same terminal as a bracketed paste.
/// Croft renders the activity-bar icons, welcome wordmark, run-debug
/// hero, no-repo hero, and SSH empty-state hero as OSC-1337 image cells,
/// so a click-with-microdrag on any of those triggers iTerm2's drag-
/// handler and croft would otherwise open the resulting PNG as an image
/// preview tab or import it into the Explorer. Filtering these paths is
/// always safe — the user has no legitimate reason to drop iTerm2's own
/// temp image into croft, and matching by filename pattern is iTerm2-
/// specific enough that it cannot collide with real user assets.
fn is_iterm2_inline_image_drop(p: &Path) -> bool {
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !name.starts_with("iTerm2.") || !name.ends_with(".png") {
        return false;
    }
    let middle = &name["iTerm2.".len()..name.len() - ".png".len()];
    if middle.is_empty() || !middle.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    // Defence in depth: only treat it as an iTerm2 drag artefact when
    // the file lives under a temp directory. macOS puts iTerm2's drag
    // temp under `/var/folders/.../TemporaryItems/` or `$TMPDIR`; both
    // resolve through `std::env::temp_dir()` ancestry. The starts_with
    // chain is forgiving because the path may be a symlinked variant
    // (e.g. `/private/var/...` vs `/var/...`).
    let parent = p.parent().unwrap_or(p);
    let parent_str = parent.to_string_lossy();
    parent_str.contains("/T/")
        || parent_str.contains("/TemporaryItems")
        || parent_str.starts_with("/tmp")
        || parent_str.starts_with("/var/folders")
        || parent_str.starts_with("/private/var/folders")
}

fn parsed_drop_tokens(s: &str) -> Vec<PathBuf> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    shell_split_drop_payload(trimmed)
        .into_iter()
        .filter_map(|raw| normalise_dropped_token(&raw))
        .collect()
}

/// Split a Finder / iTerm2 drag-drop payload into individual path tokens
/// the way a POSIX-ish shell would. Backslash escapes the next char (so
/// `\ ` keeps a literal space inside a filename); single- and double-
/// quoted runs stay intact; unescaped whitespace (space, tab, CR, LF)
/// separates tokens. Quote/backslash characters are *retained* in the
/// emitted token so downstream `normalise_dropped_token` can reuse its
/// existing un-escape + un-quote logic.
fn shell_split_drop_payload(s: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_double = false;
    let mut in_single = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && !in_single {
            // Carry the backslash and its escapee through verbatim;
            // `normalise_dropped_token` strips them later.
            cur.push('\\');
            if let Some(next) = chars.next() {
                cur.push(next);
            }
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            cur.push(c);
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
            cur.push(c);
            continue;
        }
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r') && !in_single && !in_double {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
            continue;
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Strip surrounding quotes, decode `file://` URLs (with %xx escapes), and
/// unescape `\<space>` style backslash escapes that some shells / drag
/// sources produce. Anything that isn't a plausible filesystem path
/// returns None and is dropped by the caller.
fn normalise_dropped_token(raw: &str) -> Option<PathBuf> {
    let mut s = raw.trim().to_string();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        s = s[1..s.len() - 1].to_string();
    }
    if let Some(rest) = s.strip_prefix("file://") {
        let mut path = String::with_capacity(rest.len());
        let bytes = rest.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && let (Some(h), Some(l)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2]))
            {
                path.push((h * 16 + l) as char);
                i += 3;
                continue;
            }
            path.push(bytes[i] as char);
            i += 1;
        }
        s = path;
    }
    let mut unescaped = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(next) = chars.next()
        {
            unescaped.push(next);
            continue;
        }
        unescaped.push(c);
    }
    if unescaped.is_empty() {
        return None;
    }
    Some(PathBuf::from(unescaped))
}

/// Cap a string at `max` characters by inserting an ellipsis in the
/// middle so the start (scheme/host) and end (path tail) both stay
/// visible. `max <= 8` returns the original; below that the ellipsis
/// alone wouldn't help.
fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max || max <= 8 {
        return s.to_string();
    }
    let head = max / 2 - 1;
    let tail = max - head - 1;
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    out.extend(chars.iter().take(head));
    out.push('…');
    out.extend(chars.iter().skip(chars.len() - tail));
    out
}

fn append_to_relay_log(log_path: &Path, payload: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    f.write_all(payload.as_bytes())?;
    f.sync_data().ok();
    Ok(())
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn sheet_visible_rows(inner: Rect) -> usize {
    // Renderer reserves 1 row for the header line, 1 row for the column
    // labels, and 1 row for the bottom status. Anything left is data.
    inner.height.saturating_sub(3) as usize
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

/// Walk from `start` up to and including `workspace_root` looking for a
/// venv-style Python interpreter. Returns `(interpreter_path, owning_dir)`
/// where `owning_dir` is the directory that contains the venv folder — the
/// natural cwd for the run.
///
/// The walk stops at `workspace_root`: a venv that lives ABOVE the workspace
/// would belong to a different project the user has not opened, and using it
/// silently would surprise them.
fn find_python_venv(start: &Path, workspace_root: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut probe = start.to_path_buf();
    loop {
        for venv_name in [".venv", "venv", ".env"] {
            let py = probe.join(venv_name).join("bin").join("python");
            if py.is_file() {
                return Some((py, probe));
            }
        }
        if probe == workspace_root {
            return None;
        }
        match probe.parent() {
            Some(parent) if parent != probe => {
                if !parent.starts_with(workspace_root) && parent != workspace_root {
                    return None;
                }
                probe = parent.to_path_buf();
            }
            _ => return None,
        }
    }
}

const TERMINAL_ADD_LABEL: &str = " + ";
const TERMINAL_CLOSE_LABEL: &str = " - ";

/// True when croft was invoked over an SSH login (or otherwise inside a
/// remote shell). Used to throttle PTY-driven redraws further so the SSH
/// pipe never saturates and starves input handling on the same thread.
fn is_remote_session() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_TTY").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
}

/// Status-line advisory for a remote croft session that will not survive an
/// SSH transport drop. `croft remote` launches the remote croft under dtach
/// and exports `CROFT_SESSION_PERSISTENT=1`; when that marker is absent on a
/// remote (SSH) session the host has no dtach, so croft tells the user how to
/// turn persistence on. Local sessions never see it. Returned as a status
/// string only (the bottom line is info-only; no input).
fn remote_persistence_status(is_remote: bool, persistent: bool) -> Option<&'static str> {
    if is_remote && !persistent {
        Some("Persistence off: install dtach")
    } else {
        None
    }
}

fn croft_cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("croft")
}

/// Resolve the binary to re-exec into after an update. The remote install
/// always lands at `~/.cargo/bin/croft`; preferring that path (over
/// `current_exe`, which may resolve to the now-replaced inode) guarantees
/// the swap picks up the freshly-shipped file.
fn reexec_binary_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".cargo").join("bin").join("croft");
        if p.exists() {
            return p;
        }
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("croft"))
}

fn sidebar_view_label(view: SidebarView) -> &'static str {
    match view {
        SidebarView::Explorer => "Explorer",
        SidebarView::Search => "Search",
        SidebarView::SourceControl => "SourceControl",
        SidebarView::Remote => "Remote",
        SidebarView::RunDebug => "RunDebug",
    }
}

fn sidebar_view_from_label(label: &str) -> Option<SidebarView> {
    match label {
        "Explorer" => Some(SidebarView::Explorer),
        "Search" => Some(SidebarView::Search),
        "SourceControl" => Some(SidebarView::SourceControl),
        "Remote" => Some(SidebarView::Remote),
        "RunDebug" => Some(SidebarView::RunDebug),
        _ => None,
    }
}

/// Live cwd of a running process by PID, or None when the platform doesn't
/// expose one. Used by `split_terminal` so a new pane lands wherever the
/// user has `cd`'d the active shell.
#[cfg(target_os = "linux")]
fn cwd_of_pid(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// macOS: call `proc_pidinfo` with `PROC_PIDVNODEPATHINFO` directly via the
/// libSystem FFI instead of shelling out to `lsof -d cwd`. Two reasons:
///   1. `lsof` on Sonoma+ tickles the "App Management" / "App Data" TCC
///      privacy class (the OS sees the responsible parent process — iTerm
///      — inspecting another process's open files and prompts the user).
///      `proc_pidinfo` against our own child PID needs no TCC entitlement.
///   2. No fork/exec on the hot path of every terminal split.
///
/// Struct layout matches `<sys/proc_info.h>`. We read the path field of
/// `pvi_cdir` (the cwd vnode) at the documented offset.
#[cfg(target_os = "macos")]
fn cwd_of_pid(pid: u32) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::raw::{c_int, c_void};
    use std::os::unix::ffi::OsStringExt;

    const PROC_PIDVNODEPATHINFO: c_int = 9;
    const MAXPATHLEN: usize = 1024;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct VinfoStat {
        vst_dev: u32,
        vst_mode: u16,
        vst_nlink: u16,
        vst_ino: u64,
        vst_uid: u32,
        vst_gid: u32,
        vst_atime: i64,
        vst_atimensec: i64,
        vst_mtime: i64,
        vst_mtimensec: i64,
        vst_ctime: i64,
        vst_ctimensec: i64,
        vst_birthtime: i64,
        vst_birthtimensec: i64,
        vst_size: i64,
        vst_blocks: i64,
        vst_blksize: i32,
        vst_flags: u32,
        vst_gen: u32,
        vst_rdev: u32,
        vst_qspare: [i64; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VnodeInfo {
        vi_stat: VinfoStat,
        vi_type: i32,
        vi_pad: i32,
        vi_fsid: [i32; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VnodeInfoPath {
        vip_vi: VnodeInfo,
        vip_path: [u8; MAXPATHLEN],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcVnodePathInfo {
        pvi_cdir: VnodeInfoPath,
        pvi_rdir: VnodeInfoPath,
    }

    unsafe extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
    }

    let mut info: ProcVnodePathInfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<ProcVnodePathInfo>() as c_int;
    let ret = unsafe {
        proc_pidinfo(
            pid as c_int,
            PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut c_void,
            size,
        )
    };
    if ret <= 0 {
        return None;
    }
    let path = &info.pvi_cdir.vip_path;
    let len = path.iter().position(|&b| b == 0).unwrap_or(0);
    if len == 0 {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(path[..len].to_vec())))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn cwd_of_pid(_pid: u32) -> Option<PathBuf> {
    None
}

/// Paint the `[+]` and (when more than one terminal is open) `[-]` buttons
/// on the top border of the terminal pane and return their hit-test
/// rectangles `(add, close)`. Either side is None when the pane is too
/// narrow / short for the label, or — for close — when only one terminal
/// is open and there's nothing to drop.
fn paint_terminal_pane_buttons(
    frame: &mut ratatui::Frame,
    area: Rect,
    show_close_button: bool,
    brand: bool,
    pointer: Option<(u16, u16)>,
) -> (Option<Rect>, Option<Rect>) {
    let add_w = TERMINAL_ADD_LABEL.chars().count() as u16;
    let close_w = TERMINAL_CLOSE_LABEL.chars().count() as u16;
    if area.height == 0 {
        return (None, None);
    }
    let y = area.y;
    // Styling is the shared header-action look (see `header_pill::action_style`):
    // quiet teal icons under Black with a teal hover pill, navy chips under
    // Croft Dark. The Explorer `⋯` button draws from the same source of truth.
    let style_at = |x: u16, w: u16| -> Style {
        let hovered = pointer.is_some_and(|(px, py)| py == y && px >= x && px < x + w);
        crate::widgets::header_pill::action_style(brand, hovered)
    };
    let mut add_rect: Option<Rect> = None;
    let mut close_rect: Option<Rect> = None;

    if area.width >= add_w + 2 {
        let x = area.x + area.width - add_w - 1;
        frame
            .buffer_mut()
            .set_string(x, y, TERMINAL_ADD_LABEL, style_at(x, add_w));
        add_rect = Some(Rect {
            x,
            y,
            width: add_w,
            height: 1,
        });
    }
    if show_close_button && area.width >= add_w + close_w + 2 {
        // Sit just to the left of the add button.
        let x = area.x + area.width - add_w - close_w - 1;
        frame
            .buffer_mut()
            .set_string(x, y, TERMINAL_CLOSE_LABEL, style_at(x, close_w));
        close_rect = Some(Rect {
            x,
            y,
            width: close_w,
            height: 1,
        });
    }
    (add_rect, close_rect)
}

fn key_to_bytes(key: KeyEvent) -> Vec<u8> {
    use KeyCode::*;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        Enter => vec![b'\r'],
        Tab => vec![b'\t'],
        BackTab => b"\x1b[Z".to_vec(),
        Backspace => vec![0x7f],
        Esc => vec![0x1b],
        Up => b"\x1b[A".to_vec(),
        Down => b"\x1b[B".to_vec(),
        Right if alt => b"\x1bf".to_vec(),
        Left if alt => b"\x1bb".to_vec(),
        Right => b"\x1b[C".to_vec(),
        Left => b"\x1b[D".to_vec(),
        Home => b"\x1b[H".to_vec(),
        End => b"\x1b[F".to_vec(),
        PageUp => b"\x1b[5~".to_vec(),
        PageDown => b"\x1b[6~".to_vec(),
        Insert => b"\x1b[2~".to_vec(),
        Delete => b"\x1b[3~".to_vec(),
        F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => b"\x1b[15~".to_vec(),
            6 => b"\x1b[17~".to_vec(),
            7 => b"\x1b[18~".to_vec(),
            8 => b"\x1b[19~".to_vec(),
            9 => b"\x1b[20~".to_vec(),
            10 => b"\x1b[21~".to_vec(),
            11 => b"\x1b[23~".to_vec(),
            12 => b"\x1b[24~".to_vec(),
            _ => Vec::new(),
        },
        Char(c) => {
            if ctrl {
                let lc = c.to_ascii_lowercase();
                if lc.is_ascii_lowercase() {
                    return vec![(lc as u8) - b'a' + 1];
                }
                match c {
                    '@' => vec![0x00],
                    '\\' => vec![0x1c],
                    ']' => vec![0x1d],
                    _ => Vec::new(),
                }
            } else if alt {
                let mut v = vec![0x1b];
                let mut buf = [0u8; 4];
                v.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                v
            } else {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf).as_bytes().to_vec()
            }
        }
        _ => Vec::new(),
    }
}

/// Divider drawn between the diagnostic block and the language hover content,
/// mirroring the horizontal rule VS Code renders between hover sections.
const HOVER_DIVIDER: &str = "────────";

/// Render the diagnostics under the hovered point into a single labelled text
/// block, errors first (so the most severe message leads), each line prefixed
/// with its severity. `None` when there are no diagnostics. The severity word
/// answers the user's "what type of error is it" directly, since a TUI can't
/// show VS Code's coloured marker icon.
fn format_diagnostics(
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
fn compose_hover(diagnostic: Option<&str>, hover: Option<&str>) -> Option<String> {
    match (diagnostic, hover) {
        (Some(d), Some(h)) => Some(format!("{d}\n{HOVER_DIVIDER}\n{h}")),
        (Some(d), None) => Some(d.to_string()),
        (None, Some(h)) => Some(h.to_string()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests;

/// Stdout wrapper that tallies every byte ratatui flushes through the
/// backend. The main loop reads + resets the counter once per frame to
/// surface the per-frame payload size in the F8 perf HUD.
struct CountingWriter {
    inner: Stdout,
    bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes
            .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

type CroftTerminal = Terminal<CrosstermBackend<CountingWriter>>;

pub fn run(root: PathBuf, restore_session: Option<PathBuf>) -> Result<()> {
    // zoxide is a hard dependency of the Cmd+Z jump popup. Probe for it
    // and install it in the background if missing, off the launch path so
    // the first frame is never blocked. Runs on every host croft starts
    // on — the local Mac and the remote Linux box both reach `run` — so
    // the dependency is satisfied identically on both (GOLDEN RULE).
    crate::zoxide::ensure_installed_in_background();
    // Termux ships no font with codicon glyphs, so the activity-bar glyph
    // fallback renders blank cells out of the box. Install the Meslo Nerd
    // Font Mono into ~/.termux/font.ttf in the background (no-op on every
    // other host, and when the user already has a font.ttf).
    crate::termux::ensure_codicon_font_in_background();
    // Build the tree-sitter highlight configurations on a background thread so
    // the first file open paints its syntax (keywords, strings, comments)
    // instantly without the per-tab query-compilation snap, yet the ~80ms of
    // query compilation never blocks the cold-start path before the first
    // frame. The configs land in a process-wide cache, so the UI thread reads
    // the finished build straight out of it on first open (or builds on demand
    // if the user opens a file before the prewarm finishes).
    std::thread::spawn(crate::highlight::prewarm_configs);
    let title = build_title(&root);
    let mut app = App::new(root.clone())?;
    // Restore the tabs / layout carried across a self-update re-exec, then
    // delete the handoff file so a later normal launch starts clean.
    if let Some(session_path) = restore_session.as_ref() {
        if let Ok(state) = crate::session_state::SessionState::load(session_path) {
            app.apply_session_state(&state);
        }
        let _ = std::fs::remove_file(session_path);
    }
    app.start_update_watch_if_remote();

    enable_raw_mode().context("enable raw mode")?;
    // Sixel has no env var, so when neither iTerm2 nor Kitty was detected from
    // the environment, probe the host with a DA1 query. This runs in raw mode
    // but before the alt-screen switch and long before the event loop's first
    // poll, so crossterm's (lazily-started) event reader can't swallow the
    // reply. A non-sixel terminal (e.g. Apple Terminal) answers DA1 without
    // attribute 4 and the probe returns instantly; init_graphics then promotes
    // the protocol to Sixel only when this is true.
    #[cfg(unix)]
    if app.inline_protocol == crate::iterm2_inline::InlineImageProtocol::None {
        app.sixel_supported = crate::iterm2_inline::probe_sixel_support();
    }
    let mut out = stdout();
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        // SteadyBar = thin vertical line, no hardware blink. The blink is
        // done in software (App toggles whether the OS cursor is positioned
        // each frame) so it works even when the host terminal's own
        // "Blinking cursor" preference is off.
        crossterm::cursor::SetCursorStyle::SteadyBar,
    )
    .context("enter alt screen")?;
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
    {
        use std::io::Write;
        out.write_all(&set_title_seq(&title)).ok();
        // Force iTerm2's session bg to sRGB(EDITOR_BG_RGB). Combined with
        // `Color::Reset` on welcome-pane cells and the same sRGB hex baked
        // into the OSC-1337 PNG canvas, both surfaces flow through iTerm2's
        // sRGB → display path and the welcome image bg matches the
        // surrounding pane bg pixel-for-pixel
        // (https://gitlab.com/gnachman/iterm2/-/issues/12529).
        if crate::iterm2_inline::detect_iterm2_inline_support() {
            let theme = crate::prefs::Prefs::load_or_default().theme();
            out.write_all(set_session_bg_srgb_seq(theme.editor_bg_rgb()).as_bytes())
                .ok();
        }
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
    let backend = CrosstermBackend::new(CountingWriter {
        inner: out,
        bytes: perf_bytes,
    });
    let mut terminal: CroftTerminal = Terminal::new(backend).context("create terminal")?;

    let result = main_loop(&mut app, &mut terminal);

    disable_raw_mode().ok();
    {
        use std::io::Write;
        let mut out = stdout();
        out.write_all(&set_title_seq("")).ok();
        // Revert iTerm2's session bg to the profile default so the user's
        // shell after croft exits doesn't keep our forced editor-bg colour.
        if crate::iterm2_inline::detect_iterm2_inline_support() {
            out.write_all(reset_session_bg_seq().as_bytes()).ok();
        }
        out.flush().ok();
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
    )
    .ok();
    terminal.show_cursor().ok();

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
        let _ = state.save(&session_path);
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new(reexec_binary_path());
        cmd.arg(&app.workspace_root)
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
            crate::remote::launch_croft_with(&remote.host, remote.path.as_deref(), None)
        };
        restore_host_terminal_state();
        match launch_result? {
            crate::remote::RemoteOutcome::ReturnToLocal => return run(root, None),
            crate::remote::RemoteOutcome::Exited => return Ok(()),
        }
    }
    Ok(())
}

/// Re-send the terminal-restore escape sequences after a remote ssh
/// session ends. The remote croft enables alt-screen, mouse tracking,
/// and bracketed paste on the local tty; if it crashed or exited
/// abnormally those modes can leak back into the user's shell. Re-
/// emitting the disable sequences here ensures the host shell is
/// always returned to a clean state.
fn restore_host_terminal_state() {
    use std::io::Write;
    let mut out = stdout();
    let _ = out.write_all(TERMINAL_RESTORE_SEQ);
    let _ = out.flush();
}

// Disable every mouse-tracking mode crossterm knows about (1000/1002/1003
// normal+button+any-motion, 1015/1006 urxvt+SGR encodings), bracketed
// paste (2004), exit any leftover alt-screen (1049l), and reset the
// cursor style. Written as raw bytes because the callers that need it
// (the panic hook, the post-remote-ssh restore) no longer hold a
// crossterm Backend handle for `execute!`.
const TERMINAL_RESTORE_SEQ: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[?2004l\x1b[?1049l\x1b[ q";

fn install_terminal_restore_panic_hook() {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            use std::io::Write;
            // Persist the panic BEFORE touching the terminal: the TUI teardown
            // can garble or scroll away the message printed to stderr, so a file
            // is the only reliable record (read with `cat ~/.cache/croft/panic.log`).
            log_panic_to_file(info);
            let _ = disable_raw_mode();
            let mut out = stdout();
            let _ = out.write_all(TERMINAL_RESTORE_SEQ);
            let _ = out.write_all(b"\x1b[?25h");
            let _ = out.flush();
            original(info);
        }));
    });
}

/// Append a panic record (version, thread, location, message, backtrace) to
/// `~/.cache/croft/panic.log`. Best-effort and silent on any I/O failure.
fn log_panic_to_file(info: &std::panic::PanicHookInfo<'_>) {
    use std::io::Write;
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let dir = std::path::PathBuf::from(home).join(".cache").join("croft");
    let _ = std::fs::create_dir_all(&dir);
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("panic.log"))
    else {
        return;
    };
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| String::from("<non-string panic payload>"));
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| String::from("<unknown>"));
    let thread = std::thread::current()
        .name()
        .unwrap_or("<unnamed>")
        .to_string();
    let backtrace = std::backtrace::Backtrace::force_capture();
    let _ = writeln!(
        f,
        "=== croft {} panicked ===\nthread: {thread}\nlocation: {location}\nmessage: {message}\nbacktrace:\n{backtrace}\n",
        env!("CARGO_PKG_VERSION"),
    );
}

/// Suspend the alt-screen, run every queued scp upload with the host
/// shell's stdin / stdout / stderr inherited (so the user sees scp's
/// progress and can answer any prompt it raises), then prompt for Enter
/// and restore the alt-screen. The local source is removed only on a
/// successful upload — if scp fails, the local copy stays put so the
/// user can retry without losing data.
fn run_pending_scp_uploads(app: &mut App, terminal: &mut CroftTerminal) -> Result<()> {
    use std::io::Write;
    let uploads = app.take_pending_scp_uploads();
    if uploads.is_empty() {
        return Ok(());
    }
    // Tear down the TUI surface so scp can use the real terminal.
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        crossterm::cursor::SetCursorStyle::DefaultUserShape,
    )
    .ok();
    terminal.show_cursor().ok();

    let total = uploads.len();
    let mut moved = 0usize;
    let mut error_count = 0usize;
    let mut affected_dirs: Vec<PathBuf> = Vec::new();
    {
        let mut out = stdout();
        let _ = writeln!(out);
        let _ = writeln!(out, "── croft: uploading {total} item(s) via scp ──");
        let _ = writeln!(
            out,
            "(scp prompts for password / passphrase / host-key will appear below)"
        );
        let _ = writeln!(out);
        let _ = out.flush();
    }
    for (i, upload) in uploads.iter().enumerate() {
        if let Some(parent) = upload.src.parent() {
            affected_dirs.push(parent.to_path_buf());
        }
        {
            let mut out = stdout();
            let _ = writeln!(
                out,
                "[{}/{}] scp -r {} {}:",
                i + 1,
                total,
                upload.src.display(),
                upload.alias,
            );
            let _ = out.flush();
        }
        if !upload.src.exists() {
            eprintln!("  ! source no longer exists, skipping");
            error_count += 1;
            continue;
        }
        let dest = format!("{}:", upload.alias);
        let status = std::process::Command::new("scp")
            .arg("-r")
            .arg(&upload.src)
            .arg(&dest)
            .status();
        match status {
            Ok(s) if s.success() => {
                moved += 1;
            }
            Ok(s) => {
                eprintln!("  ! scp exited with {s}; local source preserved");
                error_count += 1;
            }
            Err(e) => {
                eprintln!("  ! could not spawn scp: {e}");
                error_count += 1;
            }
        }
    }
    {
        let mut out = stdout();
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "── done: {moved}/{total} uploaded, {error_count} error(s) ──"
        );
        let _ = write!(out, "Press Enter to return to croft… ");
        let _ = out.flush();
    }
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);

    // Restore the TUI.
    enable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        crossterm::cursor::SetCursorStyle::SteadyBar,
    )
    .ok();
    terminal.clear().ok();
    app.overlays.activity.mark_dirty();
    app.overlays.welcome.mark_dirty();
    app.report_scp_results(moved, total, error_count, &affected_dirs);
    Ok(())
}

fn main_loop(app: &mut App, terminal: &mut CroftTerminal) -> Result<()> {
    // Force the very first frame to render so the user sees the UI even
    // before the first event arrives or any timer fires.
    let mut needs_redraw = true;
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
        // Pull in any filesystem-watcher events first so the tree reflects
        // disk reality on the very next frame.
        let fs_changed = app.drain_fs_events();
        // Peek without clearing so we can coalesce PTY-only redraws without
        // losing bytes that the reader thread has already advanced into the
        // terminal grid.
        let pty_pending = app.peek_terminals_dirty();
        let blink_visible = app.cursor_visible_phase();
        let blink_changed = blink_visible != last_blink_visible;
        let spinner_phase = app.update_spinner_phase();
        let spinner_changed = spinner_phase != last_spinner_phase;
        let commits_changed = app.drain_recent_commits();
        let search_changed = app.drain_search_results();
        let remote_changed = app.refresh_remote_if_config_changed();
        let pulls_changed = app.drain_remote_pulls();
        let connect_changed = app.poll_connect_dialog();
        let install_changed = app.poll_install_session();
        let update_changed = app.poll_update_watch();
        app.sync_lsp();
        // Request/refresh the OUTLINE for the active file (after sync_lsp so the
        // edit-seq it reads is current) and advance follow-cursor.
        let outline_sync_changed = app.sync_outline();
        let outline_changed = app.drain_lsp_document_symbols();
        // Refresh the Explorer's other stacked sub-views (Open Editors list,
        // Timeline git history, Dependencies) and drain their workers.
        let explorer_panels_changed = app.sync_explorer_panels();
        app.refresh_semantic_tokens_if_requested();
        let lsp_changed = app.drain_lsp_completion();
        app.poll_hover();
        let hover_changed = app.drain_lsp_hover();
        let tab_hover_changed = app.poll_tab_hover();
        let ui_tooltip_changed = app.poll_ui_tooltip();
        let definition_changed = app.drain_lsp_definition();
        let declaration_changed = app.drain_lsp_declaration();
        let type_definition_changed = app.drain_lsp_type_definition();
        let implementation_changed = app.drain_lsp_implementation();
        let references_changed = app.drain_lsp_references();
        let rename_changed = app.drain_lsp_rename();
        let semantic_changed = app.drain_lsp_semantic_tokens();
        let diagnostics_changed = app.drain_lsp_diagnostics();
        let progress_changed = app.drain_lsp_progress();
        let sysmon_changed = app.drain_sysmon();
        let dap_changed = app.poll_dap();
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
            || fs_changed
            || blink_changed
            || spinner_changed
            || commits_changed
            || search_changed
            || remote_changed
            || pulls_changed
            || connect_changed
            || install_changed
            || update_changed
            || lsp_changed
            || hover_changed
            || tab_hover_changed
            || ui_tooltip_changed
            || definition_changed
            || declaration_changed
            || type_definition_changed
            || implementation_changed
            || references_changed
            || rename_changed
            || semantic_changed
            || diagnostics_changed
            || progress_changed
            || outline_sync_changed
            || outline_changed
            || explorer_panels_changed
            || install_status_changed
            || sysmon_changed
            || dap_changed;
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
            // If the welcome OSC-1337 image was painted earlier and the
            // user has just opened a file, wipe the screen so iTerm drops
            // its cached image cells AND ratatui repaints every cell on
            // the next draw (its diff alone misses cells whose content
            // didn't change between welcome and editor buffers).
            if app.consume_welcome_image_clear()
                || app.consume_editor_image_clear()
                || app.consume_run_debug_image_clear()
                || app.consume_no_repo_hero_image_clear()
                || app.consume_shortcuts_image_clear()
                || app.consume_file_finder_image_clear()
                || app.consume_command_palette_image_clear()
                || app.consume_process_picker_image_clear()
                || app.consume_zoxide_jump_image_clear()
                || app.consume_ssh_empty_state_image_clear()
                || app.consume_activity_image_clear()
            {
                terminal.clear()?;
                // `\x1b[2J` evicts iTerm2's cached image cells but leaves
                // Kitty graphics on their layer; on the Kitty path delete
                // every placement here too so nothing ghosts. No-op on iTerm2.
                app.evict_inline_images_on_clear();
                // Activity-bar icons live outside ratatui too; re-emit
                // them on the next post-draw flush. The welcome wordmark
                // and the source-control hero also live in iTerm's image
                // cache that just got wiped - re-arm them so the next
                // render re-emits whichever one is currently visible.
                app.overlays.activity.mark_dirty();
                app.overlays.welcome.mark_dirty();
                app.overlays.hero.mark_dirty();
            }
            let draw_start = std::time::Instant::now();
            terminal.draw(|f| {
                app.render(f);
            })?;
            // Record render+flush time and the bytes ratatui shipped this
            // frame so the F8 HUD can show where remote latency goes.
            app.perf.record_draw(draw_start.elapsed().as_micros());
            frames_in_window += 1;
            if app.consume_welcome_image_clear()
                || app.consume_editor_image_clear()
                || app.consume_run_debug_image_clear()
                || app.consume_no_repo_hero_image_clear()
                || app.consume_shortcuts_image_clear()
                || app.consume_file_finder_image_clear()
                || app.consume_command_palette_image_clear()
                || app.consume_process_picker_image_clear()
                || app.consume_zoxide_jump_image_clear()
                || app.consume_ssh_empty_state_image_clear()
                || app.consume_activity_image_clear()
            {
                terminal.clear()?;
                app.evict_inline_images_on_clear();
                app.overlays.activity.mark_dirty();
                app.overlays.welcome.mark_dirty();
                app.overlays.hero.mark_dirty();
                terminal.draw(|f| {
                    app.render(f);
                })?;
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
            app.flush_activity_image_overlays();
            // Active editor image preview: bake-once-emit-each-frame
            // overlay, just like the welcome wordmark. Sent after ratatui
            // has finished its diff so the image bytes land on cells
            // ratatui won't repaint until layout changes again. One slot
            // per editor split column (0 = left, 1 = right); when not
            // split only slot 0 ever has a payload.
            for side in 0..2 {
                if let Some((osc, layout)) = app.editor_image_payload(side) {
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
                }
            }
            // Codeberg badge image overlay on the welcome panel. Same
            // re-emit-every-frame strategy as the activity bar: ratatui
            // doesn't track OSC-1337 image cells, so any neighbouring SGR
            // burst can evict them in iTerm2's cache. Only fires when the
            // welcome panel is visible, the open repo is on Codeberg, and
            // we successfully baked the icon at init time.
            app.flush_welcome_codeberg_badge_overlay();
            // Run-and-Debug headline icon: same re-emit-every-frame trick.
            // Only fires while the sidebar is on the Run-Debug view and the
            // panel reserved a cell for the icon on its last render. On
            // view change the `consume_run_debug_image_clear` gate above
            // already armed `terminal.clear()`, evicting the cached
            // image cells before the next sidebar paints.
            app.flush_run_debug_icon_overlay();
            app.flush_no_repo_hero_overlay();
            app.flush_ssh_empty_state_overlay();
            // Welcome-screen logo: same OSC-1337 trick, gated by its own
            // dirty flag and only emitted while the editor pane is in its
            // blank initial state.
            if let Some((img, layout)) = app.welcome_image_emit_payload() {
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
                    Event::Resize(_, _) => {
                        // Alt-screen reflow blanks the activity bar's SGR cells
                        // but iTerm2's OSC-1337 image layer survives, so a plain
                        // re-emit ghosts a fresh icon over the stale one. Re-mark
                        // dirty AND arm a one-shot terminal.clear() to evict it.
                        app.on_resize();
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
