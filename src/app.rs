use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
        EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Terminal,
};
use std::collections::BTreeSet;
use std::io::{stdout, Stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::widgets::{
    editor::{Editor, EditorTabs},
    file_finder::is_noise_dir,
    file_tree::FileTree,
    remote::RemotePanel,
    run_debug::RunDebugPanel,
    search::SearchPanel,
    source_control::SourceControlPanel,
    system_panel::SystemPanel,
    terminal::PtyTerminal,
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

const ACTIVITY_BAR_WIDTH: u16 = 4;

/// Single source of truth for the editor pane background. Used both as
/// ratatui's bg style and as the alpha-blend target behind the welcome
/// half-block raster, so the wordmark seamlessly merges with the pane.
/// When the IDE later supports themes, swap this for a lookup against the
/// active theme and the welcome grid will re-bake on the next render.
const EDITOR_BG_RGB: (u8, u8, u8) = (0x1e, 0x22, 0x2e);
const ACTIVITY_ICON_HEIGHT: u16 = 2;
const ACTIVITY_ICON_GAP: u16 = 0;
const FALLBACK_CELL_PIXEL: (u32, u32) = (10, 20);

fn activity_icon_glyph_x(bar: Rect) -> u16 {
    bar.x + bar.width / 2
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
}

/// Pre-encoded iTerm2 OSC-1337 inline-image escape sequences for each icon
/// state. Encoded once in `App::init_graphics` (no PNG re-encoding per
/// frame) and rewritten under the activity-bar block after every ratatui
/// frame draw, since ratatui's bg-clear overdraws the image.
struct ActivityBarImages {
    explorer_active: String,
    explorer_inactive: String,
    search_active: String,
    search_inactive: String,
    source_control_active: String,
    source_control_inactive: String,
    remote_active: String,
    remote_inactive: String,
    run_debug_active: String,
    run_debug_inactive: String,
}

/// Single source of truth for the application's user-facing name.
pub const APP_NAME: &str = "croft";

/// Agnoster-style status colours: clean working tree is green, any dirtiness
/// (modified, staged, or untracked) flips the pill to yellow/orange.
const GIT_CLEAN_COLOR: Color = Color::Rgb(0xa3, 0xbe, 0x8c);
const GIT_DIRTY_COLOR: Color = Color::Rgb(0xeb, 0xcb, 0x8b);
const FS_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Top-level directory names that the FS watcher must NOT descend into.
/// Each lives behind a macOS TCC class (`kTCCServiceSystemPolicyAppData`,
/// `kTCCServiceSystemPolicyContainersGroups`, etc.); statting their
/// contents from a non-owning process trips the App Management privacy
/// prompt for the responsible parent terminal.
const FS_WATCH_PROTECTED_NAMES: &[&str] = &["Library", ".Trash"];

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
    spans.push(Span::styled(
        "\u{ea68} ",
        Style::default().fg(pill_color),
    ));
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

/// Text shown inside the colored "brand" pill at the left of the status bar.
fn brand_pill_text() -> String {
    format!(" {APP_NAME} ")
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

#[derive(Clone, PartialEq, Eq, Debug)]
enum MenuAction {
    Create(CreateKind),
    /// Move every path in `paths` to the OS trash (recoverable). One entry
    /// for a single right-click, more when a multi-selection is active.
    Delete { paths: Vec<PathBuf> },
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
    CompareWithSelected { anchor: PathBuf, other: PathBuf },
    /// Re-root the workspace at the right-clicked folder. Folder-only -
    /// files can't be roots. Used to dive into a nested git repo without
    /// relaunching croft.
    MakeRoot(PathBuf),
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
        MenuAction::Delete { .. } => Some("⌫"),
        MenuAction::CloseTab(_) => Some("⌘W"),
        MenuAction::CloseOtherTabs(_) => Some("⌥⌘T"),
        MenuAction::CloseAllTabs => Some("⌘K W"),
        _ => None,
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
fn build_tab_context_menu_items(idx: usize, tab_count: usize) -> Vec<(String, MenuAction)> {
    let mut items: Vec<(String, MenuAction)> = Vec::new();
    items.push((String::from("Close"), MenuAction::CloseTab(idx)));
    if tab_count > 1 {
        items.push((String::from("Close Others"), MenuAction::CloseOtherTabs(idx)));
    }
    if idx + 1 < tab_count {
        items.push((
            String::from("Close to the Right"),
            MenuAction::CloseTabsToRight(idx),
        ));
    }
    items.push((String::from("Close All"), MenuAction::CloseAllTabs));
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
        items.push((String::from("Cut"), MenuAction::Cut(paths_for_action.clone())));
        items.push((String::from("Copy"), MenuAction::Copy(paths_for_action.clone())));
        if clipboard.is_some() {
            items.push((String::from("Paste"), MenuAction::Paste(target_dir.to_path_buf())));
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
            items.push((String::from("Paste"), MenuAction::Paste(target_dir.to_path_buf())));
        }
    }
    items
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

pub struct App {
    pub tree: FileTree,
    pub search: SearchPanel,
    pub remote: RemotePanel,
    pub source_control: SourceControlPanel,
    pub run_debug: RunDebugPanel,
    pub system_panel: SystemPanel,
    pub editor: EditorTabs,
    pub terminals: Vec<PtyTerminal>,
    pub active_terminal: usize,
    perf_hud: bool,
    perf_bytes: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    last_draw_us: u128,
    last_frame_bytes: usize,
    last_fps: u32,
    last_kps: u32,
    last_ips: u32,
    last_work_us: u128,
    last_poll_us: u128,
    workspace_root: PathBuf,
    sidebar_view: SidebarView,
    sidebar_areas: SidebarAreas,
    focus: Pane,
    show_tree: bool,
    show_terminal: bool,
    /// `Ctrl+Shift+J` maximizes the terminal pane: the editor / welcome
    /// collapses to zero height and the terminal fills the entire right
    /// column next to the Explorer. Pressing the chord again, or hiding
    /// the terminal entirely with `Ctrl+J`, clears the flag.
    terminal_maximized: bool,
    status: String,
    quit: bool,
    context_menu: Option<ContextMenu>,
    prompt: Option<Prompt>,
    /// Filesystem watcher; held to keep it alive. Events flow into `fs_rx`.
    _fs_watcher: Option<
        notify_debouncer_full::Debouncer<
            notify::RecommendedWatcher,
            notify_debouncer_full::RecommendedCache,
        >,
    >,
    fs_rx: Option<std::sync::mpsc::Receiver<notify_debouncer_full::DebounceEventResult>>,
    /// Receives the debouncer + event channel from a background thread once
    /// notify_debouncer_full finishes its initial recursive cache walk. On
    /// large monorepos this walk is the dominant startup cost (≥1 s), so
    /// it must not block `App::new`. None once installed.
    fs_watcher_init_rx: Option<std::sync::mpsc::Receiver<FsWatcherInit>>,
    git_status: crate::git::GitStatus,
    /// Outbox for `git status` / `git status --porcelain` requests sent
    /// to the background worker (`git::git_worker_loop`). Sidebar-icon
    /// clicks and FS-watcher ticks `send` a `GitRequest` here instead
    /// of shelling out on the UI thread; the worker shells out on its
    /// own thread and replies via `git_response_rx`. Sending is a
    /// non-blocking unbounded mpsc push (~hundreds of ns), so the
    /// hot-path cost on the click is "queue one request" rather than
    /// "fork+exec git and wait 15-50 ms" — the entire reason croft is
    /// in Rust.
    git_request_tx: std::sync::mpsc::Sender<crate::git::GitRequest>,
    /// Inbox for `GitResponse`s coming back from the worker. Drained
    /// each tick by `drain_git_responses`; a non-empty drain forces
    /// the next redraw so the new status / entries appear within one
    /// frame of arrival.
    git_response_rx: std::sync::mpsc::Receiver<crate::git::GitResponse>,
    /// Inbox for `SystemSample`s from the background sampler thread
    /// (`sysmon::system_monitor_loop`). Drained each tick by
    /// `drain_sysmon`; a non-empty drain forces a redraw so the SYSTEM
    /// panel's sparklines advance within one frame of arrival. Sampling
    /// stays off the render thread because sysinfo's CPU refresh holds
    /// for the platform's minimum-interval to compute accurate %.
    sysmon_rx: std::sync::mpsc::Receiver<crate::sysmon::SystemSample>,
    last_git_check: std::time::Instant,
    /// Anchor instant for the cursor blink. `tick_cursor_visible()` reads
    /// this to compute whether the caret is currently in its on-half or
    /// off-half. `poke_cursor()` resets it so the caret stays solidly
    /// visible right after any user activity.
    cursor_blink_anchor: std::time::Instant,
    /// Pre-encoded inline-image escapes for the activity-bar icons. `None`
    /// when the host terminal can't render OSC-1337 (we then fall back to
    /// the codicon glyph rendered inside the same multi-row block).
    activity_images: Option<ActivityBarImages>,
    /// Pre-encoded OSC-1337 image for the Codeberg "Recent Activity" badge,
    /// sized to a 2x1 cell rectangle. Painted by the main loop right after
    /// ratatui flushes the welcome panel, at `welcome_codeberg_badge_cell`.
    /// `None` when the host terminal lacks OSC-1337 image support.
    welcome_codeberg_badge_osc: Option<String>,
    /// Pre-encoded OSC-1337 image of the codicon `debug-alt` glyph sized
    /// to the run-debug panel's headline icon block. `None` when the host
    /// terminal lacks OSC-1337 support; the panel falls back to the codicon
    /// glyph painted directly into ratatui's buffer.
    run_debug_icon_osc: Option<String>,
    /// Cell where the run-debug headline image was actually painted on
    /// the previous post-draw flush. Read by `set_sidebar_view` to decide
    /// whether the user is leaving Run-Debug after the icon was on
    /// screen, in which case a one-shot `terminal.clear()` is armed
    /// to evict iTerm2's cached image cells (plain SGR overwrites do
    /// not reliably evict OSC-1337 image cells).
    run_debug_icon_last_emitted: Option<(u16, u16)>,
    /// One-shot flag: arm a `terminal.clear()` on the next render pass
    /// when the run-debug headline icon was last emitted but the panel
    /// is no longer the active sidebar view. Identical pattern to
    /// `welcome_image_clear_requested` and `editor_image_clear_requested` —
    /// the main loop folds all three into the same clear+redraw gate
    /// that nukes iTerm2's image cache. Without this, switching sidebar
    /// views away from Run-Debug leaves the bug+play picture ghosting
    /// in the middle of whichever panel comes next.
    run_debug_image_clear_requested: bool,
    /// Absolute terminal cell where the Codeberg badge image goes. Recorded
    /// during welcome render; consumed post-draw. `None` when the welcome
    /// panel isn't visible or the open repo isn't on Codeberg.
    welcome_codeberg_badge_cell: Option<(u16, u16)>,
    /// Cell where the Codeberg badge image was actually painted on the
    /// previous redraw. Used to wipe the OLD position before re-emitting
    /// at a new one after a resize: iTerm2's OSC-1337 image cells survive
    /// `terminal.clear()` and a plain SGR overwrite, so without explicitly
    /// stamping blanks at the old cell pair the logo "ghosts" mid-screen
    /// when the welcome layout shifts.
    welcome_codeberg_badge_last_emitted: Option<(u16, u16)>,
    /// Last mouse-down on the editor pane: `(when, column, row)`. Used to
    /// detect a double-click as two left-down events at the same cell within
    /// `DOUBLE_CLICK_WINDOW`. Cleared when the next click lands elsewhere or
    /// after the double-click fires.
    last_editor_left_down: Option<(std::time::Instant, u16, u16)>,
    /// Same idea as `last_editor_left_down` but for the file-tree pane.
    /// Double-click on a tree row (within `DOUBLE_CLICK_WINDOW`) opens the
    /// file in a new editor tab and moves focus to it; a single click keeps
    /// the existing preview-style behaviour (replace active tab, keep tree
    /// focused).
    last_tree_left_down: Option<(std::time::Instant, u16, u16)>,
    /// Same idea as `last_editor_left_down` but for the terminal pane:
    /// two left-downs at the same cell within `DOUBLE_CLICK_WINDOW`
    /// trigger word-select via `PtyTerminal::select_word_at`.
    last_terminal_left_down: Option<(std::time::Instant, u16, u16)>,
    /// Pane whose scrollbar is currently being dragged with the left mouse button.
    scrollbar_drag: Option<Pane>,
    /// True when the activity-bar OSC-1337 images need to be (re)written on
    /// the next post-draw flush. Set initially, on sidebar-view change, and
    /// on terminal resize. Cleared after emit. Without this gate every
    /// redraw repaints the PNGs and you see the cursor blink each time iTerm
    /// processes the image.
    activity_overlay_dirty: bool,
    /// Drives the welcome screen's "Recent" list. Always sourced from the
    /// croft repository remote baked into this binary at build time, never
    /// from the workspace the user opened.
    recent_repo_remote: Option<String>,
    recent_commits: Vec<crate::git::CommitInfo>,
    welcome_links: Vec<WelcomeLink>,
    /// Receiver for the background HTTP fetch of croft's recent commits.
    /// `None` once the fetch has completed (or failed) and been drained.
    recent_commits_rx: Option<
        std::sync::mpsc::Receiver<(crate::git::RecentCommits, crate::git::RecentCommitsError)>,
    >,
    /// Channel to the background search worker. Each keystroke or toggle
    /// flip pushes a `(query, opts)` request here; the worker debounces
    /// and runs `search_workspace` off the UI thread.
    search_query_tx: std::sync::mpsc::Sender<crate::widgets::search::SearchRequest>,
    /// Results coming back from the search worker: `(query, hits)`. The
    /// query is echoed so we can drop stale results when the user has
    /// typed past the query that produced them.
    search_results_rx: std::sync::mpsc::Receiver<crate::widgets::search::SearchEvent>,
    /// Pre-encoded OSC-1337 escape carrying the croft wordmark sized to the
    /// welcome banner block, painted on a canvas filled with the sRGB-
    /// equivalent of `EDITOR_BG_RGB` so its bg matches the SGR-painted
    /// editor pane pixel-for-pixel. None when the host terminal can't
    /// render inline images.
    welcome_image: Option<String>,
    /// Cell `(x, y, width, height)` the welcome image was last baked at.
    /// A change here triggers a re-bake on the next render.
    welcome_layout: Option<WelcomeLayout>,
    welcome_overlay_dirty: bool,
    /// True between the moment the welcome OSC-1337 image is written to the
    /// terminal and the moment we explicitly clear it. iTerm caches the
    /// image bytes outside ratatui's buffer, so when the user opens a file
    /// ratatui's diff misses cells whose buffer content didn't change and
    /// the image bleeds through under the editor. `consume_welcome_image_clear`
    /// returns true once when this needs to be wiped.
    welcome_image_displayed: bool,
    /// One-shot request to clear the cached welcome image while the welcome
    /// pane is still visible. This is needed when async recents or a resize
    /// move the logo; otherwise iTerm keeps the old image cached too.
    welcome_image_clear_requested: bool,
    /// OSC-1337 escape carrying the no-repo hero PNG (the file-with-git-tree
    /// illustration). Re-baked when the hero rect's cell size changes.
    no_repo_hero_image: Option<String>,
    no_repo_hero_layout: Option<WelcomeLayout>,
    no_repo_hero_overlay_dirty: bool,
    no_repo_hero_displayed: bool,
    /// One-shot flag: arm a `terminal.clear()` on the next render pass
    /// after the source-control hero image was emitted and the user has
    /// just navigated to a different sidebar view (or the workspace
    /// transitioned into a git repo). Same eviction trick the welcome
    /// wordmark and Run-Debug icon pipelines use - iTerm2 caches the
    /// image cells outside ratatui's diff, so without this the PNG
    /// stays painted under whatever sidebar replaces it.
    no_repo_hero_image_clear_requested: bool,
    /// Pixel size of one terminal cell, captured in `init_graphics`.
    /// Required to bake OSC-1337 images at exact viewport pixel size so
    /// iTerm draws them with no stretching or letterboxing.
    cell_pixel: Option<(u32, u32)>,
    /// Clipboard read entrypoint. Production uses the host clipboard; tests
    /// can swap in a deterministic reader for Cmd+V routing assertions.
    clipboard_reader: fn() -> Option<String>,
    /// Last low-frequency reconciliation of expanded directories. This is a
    /// backstop for events missed while the async watcher is still starting,
    /// and for host watcher failures.
    fs_poll_last_check: std::time::Instant,
    fs_poll_interval: Duration,
    fs_poll_dir_mtimes: std::collections::BTreeMap<PathBuf, Option<std::time::SystemTime>>,
    fs_poll_open_file_mtime: Option<(PathBuf, Option<(std::time::SystemTime, u64)>)>,
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
    /// Hit-test rectangles of the "[+]" buttons - one per terminal pane,
    /// indexed in lock-step with `terminals`. Empty when the pane is hidden
    /// or every pane is too narrow for the label.
    terminal_add_buttons: Vec<Rect>,
    /// Hit-test rectangles of the "[-]" buttons - one per terminal pane,
    /// indexed in lock-step with `terminals`. Empty when only one terminal
    /// is open (closing the last one would leave the pane empty, which we
    /// explicitly forbid) or the pane is hidden.
    terminal_close_buttons: Vec<Rect>,
    /// When the activity-bar OSC-1337 overlay was last written to stdout.
    /// Re-emitting on every redraw (the previous behaviour) flickered the
    /// editor caret at the PTY redraw rate; we now refresh on dirty plus a
    /// periodic keep-alive to defeat iTerm2's image-cell eviction.
    last_activity_overlay_emit: Option<std::time::Instant>,
    /// Total width / height of the right-hand content area, captured on
    /// every render so a splitter drag can clamp to the live viewport.
    last_content_width: u16,
    last_content_height: u16,
    /// Pre-encoded OSC-1337 escape carrying a fitted PNG of the active
    /// image-preview tab. Re-baked when the tab path or its target cell
    /// rect changes; emitted post-frame the same way the welcome wordmark
    /// is. None when the active tab is text.
    editor_image_osc: Option<String>,
    /// Cell rectangle the OSC was last baked at: (x, y, w, h, path-key).
    /// Drives the "needs re-bake" check and tells the post-frame writer
    /// where to position the cursor before sending the escape.
    editor_image_layout: Option<EditorImageLayout>,
    /// True from the moment we send the OSC bytes to iTerm until we
    /// explicitly clear them; gates the redraw-clearing so we don't keep
    /// re-emitting the same image every tick.
    editor_image_displayed: bool,
    /// One-shot request to wipe the cached image cells (set when the user
    /// switches to a non-image tab, closes the image, or the editor area
    /// shrinks).
    editor_image_clear_requested: bool,
    /// Optional LSP orchestrator. None when LspManager::new failed at
    /// startup (e.g. the background tokio runtime couldn't spawn); the
    /// rest of the editor stays fully functional without it.
    pub lsp: Option<crate::lsp::LspManager>,
    /// Per-path snapshot of the last edit_seq we forwarded to the LSP
    /// manager. sync_lsp diffs every tick against the current tabs to
    /// emit did_open / did_change / did_close.
    lsp_last_seen: std::collections::HashMap<PathBuf, u64>,
    /// Active completion popup, when a completion request has been issued
    /// and the user hasn't dismissed the result. Populated by drain_lsp_completion
    /// once the worker sends a CompletionResult back.
    pub completion_popup: Option<crate::widgets::completion_popup::CompletionPopup>,
    /// Most recent completion request id; later responses with a stale
    /// id are dropped so a slow earlier response cannot clobber a fresh
    /// one (e.g. user already moved past the original trigger).
    completion_request_id: Option<u64>,
    editor_vim_chord: EditorVimChord,
    shortcuts_modal: Option<crate::widgets::shortcuts::ShortcutsModal>,
    shortcuts_hit_rect: Option<Rect>,
    connect_dialog: Option<crate::widgets::connect_dialog::ConnectDialog>,
    connect_auth: Option<crate::remote_connect::SshAuth>,
    install_session: Option<crate::install_session::InstallSession>,
    /// One-shot flag the driver consumes to fire `terminal.clear()` so
    /// iTerm2 evicts the OSC-1337 image cells (activity bar icons,
    /// welcome wordmark, editor image preview) that would otherwise
    /// bleed through the modal's text. Armed on open AND on close so
    /// the modal's own cells get wiped when it dismisses too.
    shortcuts_image_clear_requested: bool,
    /// Pre-baked OSC-1337 sequence for the SSH-empty-state illustration,
    /// sized to a fixed cell box. None when the host terminal can't
    /// render inline images.
    ssh_empty_state_osc: Option<String>,
    /// Latch: true between the moment the SSH-empty-state PNG is written
    /// to the terminal and the moment we explicitly clear it. iTerm2
    /// caches the image bytes outside ratatui's buffer, so without an
    /// explicit terminal.clear() the illustration ghosts onto whatever
    /// view the user navigates to next (Explorer, Search, …).
    ssh_empty_state_displayed: bool,
    /// One-shot: armed when the illustration was displayed and the panel
    /// is no longer the active sidebar view (or has gained hosts).
    /// Consumed in the driver's terminal.clear() OR chain.
    ssh_empty_state_image_clear_requested: bool,
    /// VS Code-style Cmd+F in-editor find overlay. None when closed.
    pub editor_find: Option<crate::widgets::editor_find::EditorFind>,
    /// VS Code-style Cmd+P / Ctrl+P quick-open file finder. None when
    /// the modal is closed.
    pub file_finder: Option<crate::widgets::file_finder::FileFinder>,
    /// Cached workspace file index. Built lazily on first Cmd+P and
    /// shared across reopens so repeated invocations are instant.
    file_finder_index: Option<std::sync::Arc<Vec<crate::widgets::file_finder::FileEntry>>>,
    /// Background-build receiver: App::new spawns a thread that walks
    /// the workspace and sends the index here; Cmd+P opens against the
    /// hot index instead of walking on the UI thread. None once the
    /// index has been received (or if a build was never kicked off).
    file_finder_index_rx:
        Option<std::sync::mpsc::Receiver<Vec<crate::widgets::file_finder::FileEntry>>>,
    /// One-shot: armed on open and close to evict iTerm2's image cache
    /// behind the finder, same pipeline as the shortcuts modal.
    file_finder_image_clear_requested: bool,
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
}

const SIDEBAR_WIDTH_DEFAULT: u16 = 32;
const SIDEBAR_WIDTH_MIN: u16 = 12;
const TERMINAL_HEIGHT_MIN: u16 = 3;
const EDITOR_HEIGHT_MIN: u16 = 3;
const RIGHT_PANE_MIN: u16 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WelcomeLayout {
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

const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);

type FsWatcherInit = (
    notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
    std::sync::mpsc::Receiver<notify_debouncer_full::DebounceEventResult>,
);

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
            let width = if lines.is_empty() { first_width } else { rest_width };
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

fn welcome_commit_widths(
    commit: &crate::git::CommitInfo,
    block_w: u16,
) -> (u16, u16) {
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

fn wrapped_welcome_commit_subject(
    commit: &crate::git::CommitInfo,
    block_w: u16,
) -> Vec<String> {
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

const GRAD_TL: (u8, u8, u8) = (0x5c, 0xd6, 0xc8);
const GRAD_TR: (u8, u8, u8) = (0xec, 0x8c, 0x5a);
const GRAD_BR: (u8, u8, u8) = (0x4f, 0xb1, 0xa6);
const GRAD_BL: (u8, u8, u8) = (0x35, 0x80, 0x78);

fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| ((1.0 - t) * x as f32 + t * y as f32).round() as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

fn rgb_color((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

/// Paint a rounded-rectangle border whose stroke colour interpolates
/// linearly between the four corner colours along each edge. The interior
/// is left untouched so the caller can fill it with content.
///
/// The rect is clipped against the buffer's area, so callers don't have to
/// do the bounds math themselves — passing a rect that runs off the edge
/// (e.g., a 80x25 default startup buffer with a tall recents list) draws
/// nothing instead of panicking inside `set_string`.
fn paint_gradient_box(buf: &mut ratatui::buffer::Buffer, rect: Rect) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let buf_area = buf.area;
    if rect.x < buf_area.x
        || rect.y < buf_area.y
        || rect.x + rect.width > buf_area.x + buf_area.width
        || rect.y + rect.height > buf_area.y + buf_area.height
    {
        return;
    }
    let max_x = rect.width - 1;
    let max_y = rect.height - 1;
    for x in 0..rect.width {
        let u = if max_x > 0 { x as f32 / max_x as f32 } else { 0.0 };
        let top = lerp_rgb(GRAD_TL, GRAD_TR, u);
        let bot = lerp_rgb(GRAD_BL, GRAD_BR, u);
        let top_ch = if x == 0 {
            "\u{256d}"
        } else if x == max_x {
            "\u{256e}"
        } else {
            "\u{2500}"
        };
        let bot_ch = if x == 0 {
            "\u{2570}"
        } else if x == max_x {
            "\u{256f}"
        } else {
            "\u{2500}"
        };
        buf.set_string(
            rect.x + x,
            rect.y,
            top_ch,
            Style::default().fg(rgb_color(top)),
        );
        buf.set_string(
            rect.x + x,
            rect.y + max_y,
            bot_ch,
            Style::default().fg(rgb_color(bot)),
        );
    }
    for y in 1..max_y {
        let v = if max_y > 0 { y as f32 / max_y as f32 } else { 0.0 };
        let left = lerp_rgb(GRAD_TL, GRAD_BL, v);
        let right = lerp_rgb(GRAD_TR, GRAD_BR, v);
        buf.set_string(
            rect.x,
            rect.y + y,
            "\u{2502}",
            Style::default().fg(rgb_color(left)),
        );
        buf.set_string(
            rect.x + max_x,
            rect.y + y,
            "\u{2502}",
            Style::default().fg(rgb_color(right)),
        );
    }
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
    cmd.spawn()
        .with_context(|| format!("opening {url}"))?;
    Ok(())
}

impl App {
    pub fn new(root: PathBuf) -> Result<Self> {
        let tree = FileTree::new(root.clone());
        let search = SearchPanel::new(root.clone());
        let remote = RemotePanel::new();
        let source_control = SourceControlPanel::new();
        let run_debug = RunDebugPanel::new();
        let editor = EditorTabs::new();
        let term = PtyTerminal::new(&root).context("spawning terminal")?;

        // notify_debouncer_full's RecommendedCache walks the entire watched
        // subtree to populate its path↔inode map; on a multi-GB monorepo
        // that's >1 s. Defer to a background thread; install via
        // `try_install_pending_init` once it completes. The user sees the
        // UI immediately; the 50ms polling fallback below reconciles the
        // visible tree and active editor while the watcher starts.
        let (fs_init_tx, fs_init_rx) = std::sync::mpsc::channel();
        let root_for_fs = root.clone();
        std::thread::spawn(move || {
            if let Ok(pair) = Self::spawn_fs_watcher(&root_for_fs) {
                let _ = fs_init_tx.send(pair);
            }
        });

        // Background git worker: every `git status` / `git status
        // --porcelain` shell-out goes through this channel instead of
        // running on the UI thread. The very first request seeds the
        // status-bar badge; subsequent ones come from the FS watcher
        // and from sidebar-icon clicks (see `refresh_source_control`,
        // `refresh_git_status_debounced`). Removes the 15-50 ms
        // input-to-paint stall that used to fire on every Source
        // Control click.
        let (git_request_tx, git_request_rx) =
            std::sync::mpsc::channel::<crate::git::GitRequest>();
        let (git_response_tx, git_response_rx) =
            std::sync::mpsc::channel::<crate::git::GitResponse>();
        let git_root = root.clone();
        std::thread::spawn(move || {
            crate::git::git_worker_loop(git_root, git_request_rx, git_response_tx);
        });
        let _ = git_request_tx.send(crate::git::GitRequest::Status);

        // Background system-metrics sampler: CPU / MEM / NET / DISK / TEMP
        // refresh once per second on a dedicated thread and arrive via
        // `sysmon_rx`. The render path never touches sysinfo because
        // its CPU refresh needs MINIMUM_CPU_UPDATE_INTERVAL between
        // reads to produce accurate percentages.
        let (sysmon_tx, sysmon_rx) =
            std::sync::mpsc::channel::<crate::sysmon::SystemSample>();
        std::thread::spawn(move || {
            crate::sysmon::system_monitor_loop(sysmon_tx);
        });

        let (commits_tx, commits_rx) = std::sync::mpsc::channel();
        let recent_repo_remote = crate::git::croft_repository_remote();
        std::thread::spawn(move || {
            let timeout = std::time::Duration::from_secs(3);
            let result = crate::git::fetch_croft_recent_commits_full(timeout);
            let _ = commits_tx.send(result);
        });

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
        let fs_poll_dir_mtimes = Self::snapshot_expanded_dir_mtimes(&tree);
        Ok(Self {
            tree,
            search,
            remote,
            source_control,
            run_debug,
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
            terminals: vec![term],
            active_terminal: 0,
            perf_hud: false,
            perf_bytes: None,
            last_draw_us: 0,
            last_frame_bytes: 0,
            last_fps: 0,
            last_kps: 0,
            last_ips: 0,
            last_work_us: 0,
            last_poll_us: 0,
            workspace_root: root.clone(),
            sidebar_view: SidebarView::Explorer,
            sidebar_areas: SidebarAreas::default(),
            focus: Pane::Tree,
            show_tree: true,
            show_terminal: true,
            terminal_maximized: false,
            status: String::from("Ready"),
            quit: false,
            context_menu: None,
            prompt: None,
            _fs_watcher: None,
            fs_rx: None,
            fs_watcher_init_rx: Some(fs_init_rx),
            git_status: crate::git::GitStatus::default(),
            git_request_tx,
            git_response_rx,
            sysmon_rx,
            last_git_check: std::time::Instant::now(),
            cursor_blink_anchor: std::time::Instant::now(),
            activity_images: None,
            welcome_codeberg_badge_osc: None,
            run_debug_icon_osc: None,
            run_debug_icon_last_emitted: None,
            run_debug_image_clear_requested: false,
            welcome_codeberg_badge_cell: None,
            welcome_codeberg_badge_last_emitted: None,
            last_editor_left_down: None,
            last_tree_left_down: None,
            last_terminal_left_down: None,
            scrollbar_drag: None,
            activity_overlay_dirty: true,
            recent_repo_remote,
            recent_commits: Vec::new(),
            welcome_links: Vec::new(),
            recent_commits_rx: Some(commits_rx),
            search_query_tx,
            search_results_rx,
            welcome_image: None,
            welcome_layout: None,
            welcome_overlay_dirty: true,
            welcome_image_displayed: false,
            welcome_image_clear_requested: false,
            no_repo_hero_image: None,
            no_repo_hero_layout: None,
            no_repo_hero_overlay_dirty: true,
            no_repo_hero_displayed: false,
            no_repo_hero_image_clear_requested: false,
            cell_pixel: None,
            clipboard_reader: read_system_clipboard,
            fs_poll_last_check: std::time::Instant::now(),
            fs_poll_interval: FS_POLL_INTERVAL,
            fs_poll_dir_mtimes,
            fs_poll_open_file_mtime: None,
            remote_launch: None,
            pending_remote_launch_host: None,
            pending_remote_launch_path: None,
            tree_clipboard: None,
            tree_typeahead: None,
            compare_anchor: None,
            last_frame_area: Rect::default(),
            tree_drag: None,
            pending_scp_uploads: Vec::new(),
            pending_remote_pulls: Vec::new(),
            pending_local_open: None,
            pending_discard: None,
            commit_menu_open: false,
            default_branch_label: None,
            trust_local_browser: false,
            sidebar_width: SIDEBAR_WIDTH_DEFAULT,
            terminal_height: None,
            splitter_drag: None,
            sidebar_splitter_x: None,
            terminal_splitter_y: None,
            terminal_add_buttons: Vec::new(),
            terminal_close_buttons: Vec::new(),
            last_activity_overlay_emit: None,
            last_content_width: 0,
            last_content_height: 0,
            editor_image_osc: None,
            editor_image_layout: None,
            editor_image_displayed: false,
            editor_image_clear_requested: false,
            lsp: match crate::lsp::LspManager::new(root.clone()) {
                Ok(m) => Some(m),
                Err(e) => {
                    eprintln!("lsp manager init failed: {e}");
                    None
                }
            },
            lsp_last_seen: std::collections::HashMap::new(),
            completion_popup: None,
            editor_vim_chord: EditorVimChord::default(),
            shortcuts_modal: None,
            shortcuts_hit_rect: None,
            connect_dialog: None,
            connect_auth: None,
            install_session: None,
            shortcuts_image_clear_requested: false,
            ssh_empty_state_osc: None,
            ssh_empty_state_displayed: false,
            ssh_empty_state_image_clear_requested: false,
            editor_find: None,
            file_finder: None,
            file_finder_index: None,
            file_finder_index_rx: Some(file_finder_index_rx),
            file_finder_image_clear_requested: false,
            file_finder_index_dirty: false,
            completion_request_id: None,
        })
    }

    /// Detect inline-image support via env vars only — no stdin queries, no
    /// raw-mode contention. Queries the terminal cell pixel size via
    /// crossterm's `window_size` (TIOCGWINSZ ioctl, no stdin involvement).
    /// SSH PTYs often report only rows/columns and leave pixel dimensions
    /// as zero, so fall back to a sane 10×20 cell estimate; OSC-1337 still
    /// scales the image to the requested cell rectangle.
    pub fn init_graphics(&mut self) {
        if !crate::iterm2_inline::detect_iterm2_inline_support() {
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
        let icon_bg = image::Rgba([
            EDITOR_BG_RGB.0,
            EDITOR_BG_RGB.1,
            EDITOR_BG_RGB.2,
            0xff,
        ]);
        let encode = |src: &[u8], is_active: bool| -> Option<String> {
            let baked =
                crate::iterm2_inline::compose_icon(src, canvas_w, canvas_h, is_active, icon_bg)
                    .ok()?;
            // preserveAspectRatio=0: stretch to exactly fill 4×2 cells.
            // Since the PNG was composed at exactly that pixel size,
            // there's no actual scaling and the codicon's square area
            // remains a true square on screen.
            let raw =
                crate::iterm2_inline::build_inline_image_osc(&baked, w_cells, h_cells, false);
            Some(if is_tmux {
                crate::iterm2_inline::tmux_passthrough_wrap(&raw)
            } else {
                raw
            })
        };
        let explorer_active = encode(crate::iterm2_inline::EXPLORER_SRC_PNG, true);
        let explorer_inactive = encode(crate::iterm2_inline::EXPLORER_SRC_PNG, false);
        let search_active = encode(crate::iterm2_inline::SEARCH_SRC_PNG, true);
        let search_inactive = encode(crate::iterm2_inline::SEARCH_SRC_PNG, false);
        let source_control_active =
            encode(crate::iterm2_inline::NO_REPO_HERO_PNG, true);
        let source_control_inactive =
            encode(crate::iterm2_inline::NO_REPO_HERO_PNG, false);
        let remote_active = encode(crate::iterm2_inline::REMOTE_SRC_PNG, true);
        let remote_inactive = encode(crate::iterm2_inline::REMOTE_SRC_PNG, false);
        let run_debug_active = encode(crate::iterm2_inline::RUN_DEBUG_SRC_PNG, true);
        let run_debug_inactive = encode(crate::iterm2_inline::RUN_DEBUG_SRC_PNG, false);
        if let (
            Some(ea),
            Some(ei),
            Some(sa),
            Some(si),
            Some(sca),
            Some(sci),
            Some(ra),
            Some(ri),
            Some(rda),
            Some(rdi),
        ) = (
            explorer_active,
            explorer_inactive,
            search_active,
            search_inactive,
            source_control_active,
            source_control_inactive,
            remote_active,
            remote_inactive,
            run_debug_active,
            run_debug_inactive,
        ) {
            self.activity_images = Some(ActivityBarImages {
                explorer_active: ea,
                explorer_inactive: ei,
                search_active: sa,
                search_inactive: si,
                source_control_active: sca,
                source_control_inactive: sci,
                remote_active: ra,
                remote_inactive: ri,
                run_debug_active: rda,
                run_debug_inactive: rdi,
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
        ) {
            let raw = crate::iterm2_inline::build_inline_image_osc(&baked, 2, 1, false);
            self.welcome_codeberg_badge_osc = Some(if is_tmux {
                crate::iterm2_inline::tmux_passthrough_wrap(&raw)
            } else {
                raw
            });
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
        ) {
            let raw = crate::iterm2_inline::build_inline_image_osc(
                &baked,
                crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_W,
                crate::widgets::run_debug::RUN_DEBUG_ICON_CELLS_H,
                false,
            );
            self.run_debug_icon_osc = Some(if is_tmux {
                crate::iterm2_inline::tmux_passthrough_wrap(&raw)
            } else {
                raw
            });
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
        ) {
            let raw = crate::iterm2_inline::build_inline_image_osc(
                &baked,
                ssh_cells_w,
                ssh_cells_h,
                false,
            );
            self.ssh_empty_state_osc = Some(if is_tmux {
                crate::iterm2_inline::tmux_passthrough_wrap(&raw)
            } else {
                raw
            });
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
    pub fn flush_run_debug_icon_overlay(&mut self) {
        use std::io::Write;
        if self.shortcuts_modal.is_some() || self.file_finder.is_some() {
            return;
        }
        let panel_visible = self.show_tree
            && self.sidebar_view == SidebarView::RunDebug
            && self.run_debug_icon_osc.is_some();
        if !panel_visible {
            self.run_debug_icon_last_emitted = None;
            return;
        }
        let Some((cx, cy)) = self.run_debug.last_icon_cell else {
            self.run_debug_icon_last_emitted = None;
            return;
        };
        let Some(osc) = self.run_debug_icon_osc.as_deref() else {
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
        self.run_debug_icon_last_emitted = Some((cx, cy));
    }

    /// Re-bake (when needed) and re-emit the OSC-1337 inline image for
    /// the no-repo source-control hero. Pure no-op when the sidebar isn't
    /// on Source Control, the workspace IS a git repo, or the host
    /// terminal can't render inline images. Called from the post-draw
    /// flush each frame so iTerm2 keeps the image painted even after
    /// surrounding cells redraw.
    pub fn flush_no_repo_hero_overlay(&mut self) {
        use std::io::Write;
        if self.shortcuts_modal.is_some() || self.file_finder.is_some() {
            return;
        }
        let panel_visible = self.show_tree
            && self.sidebar_view == SidebarView::SourceControl
            && !self.source_control.status.in_repo;
        if !panel_visible {
            if self.no_repo_hero_displayed {
                self.no_repo_hero_displayed = false;
                self.no_repo_hero_overlay_dirty = true;
            }
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
        if self.no_repo_hero_layout != Some(desired) || self.no_repo_hero_image.is_none() {
            let canvas_w = (hero.width as u32) * cw;
            let canvas_h = (hero.height as u32) * ch;
            let bg = image::Rgba([
                EDITOR_BG_RGB.0,
                EDITOR_BG_RGB.1,
                EDITOR_BG_RGB.2,
                0xff,
            ]);
            if let Ok(baked) = crate::iterm2_inline::fit_image(
                crate::iterm2_inline::NO_REPO_HERO_PNG,
                canvas_w,
                canvas_h,
                bg,
            ) {
                let raw = crate::iterm2_inline::build_inline_image_osc(
                    &baked,
                    hero.width,
                    hero.height,
                    false,
                );
                let osc = if crate::iterm2_inline::detect_tmux() {
                    crate::iterm2_inline::tmux_passthrough_wrap(&raw)
                } else {
                    raw
                };
                self.no_repo_hero_image = Some(osc);
                self.no_repo_hero_layout = Some(desired);
            }
        }
        let Some(osc) = self.no_repo_hero_image.as_deref() else {
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
        self.no_repo_hero_displayed = true;
        self.no_repo_hero_overlay_dirty = false;
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
            && self.show_tree
            && self.sidebar_view == SidebarView::Remote
            && self.remote.targets.is_empty();
        if !should_show {
            if self.ssh_empty_state_displayed {
                self.ssh_empty_state_displayed = false;
                self.ssh_empty_state_image_clear_requested = true;
            }
            return;
        }
        let Some((cx, cy)) = self.remote.last_image_cell else {
            return;
        };
        let Some(osc) = self.ssh_empty_state_osc.as_deref() else {
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
        self.ssh_empty_state_displayed = true;
    }

    /// One-shot consumer paired with `flush_ssh_empty_state_overlay`. The
    /// main loop ORs the result into its `terminal.clear()` chain so
    /// iTerm2's cached image cells are evicted on view change.
    pub fn consume_ssh_empty_state_image_clear(&mut self) -> bool {
        if self.ssh_empty_state_image_clear_requested {
            self.ssh_empty_state_image_clear_requested = false;
            true
        } else {
            false
        }
    }

    /// One-shot consumer for the "leaving Run-Debug after the icon was
    /// emitted" flag. The main loop folds the result into the same OR
    /// chain that fires `terminal.clear()` for the welcome wordmark and
    /// editor preview image.
    pub fn consume_run_debug_image_clear(&mut self) -> bool {
        if self.run_debug_image_clear_requested {
            self.run_debug_image_clear_requested = false;
            true
        } else {
            false
        }
    }

    /// One-shot consumer for the "leaving Source Control after the no-repo
    /// hero PNG was emitted" flag. Same shape as
    /// `consume_run_debug_image_clear` - the main loop folds the result
    /// into its `terminal.clear()` chain so iTerm2's cached image cells
    /// are evicted on view change.
    pub fn consume_no_repo_hero_image_clear(&mut self) -> bool {
        if self.no_repo_hero_image_clear_requested {
            self.no_repo_hero_image_clear_requested = false;
            true
        } else {
            false
        }
    }

    /// Returns the post-frame OSC-1337 escapes to write under the activity
    /// bar, paired with the absolute terminal cell where each one starts
    /// (just past the active-pill column). Empty when image rendering is
    /// disabled or the activity bar hasn't been laid out yet.
    pub fn pending_activity_image_overlays(&self) -> Vec<((u16, u16), &str)> {
        if self.shortcuts_modal.is_some() || self.file_finder.is_some() {
            return Vec::new();
        }
        let Some(images) = self.activity_images.as_ref() else {
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
        let exp_state = if self.sidebar_view == SidebarView::Explorer {
            &images.explorer_active
        } else {
            &images.explorer_inactive
        };
        let sea_state = if self.sidebar_view == SidebarView::Search {
            &images.search_active
        } else {
            &images.search_inactive
        };
        let scm_state = if self.sidebar_view == SidebarView::SourceControl {
            &images.source_control_active
        } else {
            &images.source_control_inactive
        };
        let rem_state = if self.sidebar_view == SidebarView::Remote {
            &images.remote_active
        } else {
            &images.remote_inactive
        };
        let rdb_state = if self.sidebar_view == SidebarView::RunDebug {
            &images.run_debug_active
        } else {
            &images.run_debug_inactive
        };
        vec![
            ((exp_block.x, exp_block.y), exp_state.as_str()),
            ((sea_block.x, sea_block.y), sea_state.as_str()),
            ((scm_block.x, scm_block.y), scm_state.as_str()),
            ((rem_block.x, rem_block.y), rem_state.as_str()),
            ((rdb_block.x, rdb_block.y), rdb_state.as_str()),
        ]
    }

    /// Paint the Codeberg badge OSC-1337 image at its current welcome-panel
    /// cell, after first wiping any previous emit position. iTerm2 keeps
    /// image cells in a layer that survives plain SGR redraws, so when
    /// `welcome_codeberg_badge_cell` changes (window resize, sidebar
    /// toggle, terminal-pane open/close) the *old* image stays on screen
    /// unless we explicitly stamp blanks over it. Idempotent: when the
    /// badge cell hasn't moved we just re-emit at the same position to
    /// outlast iTerm2's image-cache eviction under heavy SGR traffic.
    pub fn flush_welcome_codeberg_badge_overlay(&mut self) {
        use std::io::Write;
        if self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.connect_dialog.is_some()
        {
            if let Some((px, py)) = self.welcome_codeberg_badge_last_emitted.take() {
                let mut out = stdout();
                let cursor_on = self.cursor_should_be_visible();
                let _ = write!(out, "\x1b[?25l\x1b[s");
                let _ = write!(out, "\x1b[{};{}H  ", py + 1, px + 1);
                let _ = write!(out, "\x1b[u");
                if cursor_on {
                    let _ = write!(out, "\x1b[?25h");
                }
                let _ = out.flush();
            }
            return;
        }
        if !self.editor.is_blank_initial() {
            // Welcome panel hidden: clear any previous emit and stop
            // re-emitting until we're back on the welcome screen.
            if let Some((px, py)) = self.welcome_codeberg_badge_last_emitted.take() {
                let mut out = stdout();
                let cursor_on = self.cursor_should_be_visible();
                let _ = write!(out, "\x1b[?25l\x1b[s");
                let _ = write!(out, "\x1b[{};{}H  ", py + 1, px + 1);
                let _ = write!(out, "\x1b[u");
                if cursor_on {
                    let _ = write!(out, "\x1b[?25h");
                }
                let _ = out.flush();
            }
            return;
        }
        let Some(osc) = self.welcome_codeberg_badge_osc.as_deref() else {
            return;
        };
        let Some((cx, cy)) = self.welcome_codeberg_badge_cell else {
            // Welcome visible but provider isn't Codeberg: still need to
            // wipe a stale prior emit (e.g. user just changed providers
            // without restart, or layout collapsed the badge row).
            if let Some((px, py)) = self.welcome_codeberg_badge_last_emitted.take() {
                let mut out = stdout();
                let cursor_on = self.cursor_should_be_visible();
                let _ = write!(out, "\x1b[?25l\x1b[s");
                let _ = write!(out, "\x1b[{};{}H  ", py + 1, px + 1);
                let _ = write!(out, "\x1b[u");
                if cursor_on {
                    let _ = write!(out, "\x1b[?25h");
                }
                let _ = out.flush();
            }
            return;
        };
        let mut out = stdout();
        let cursor_on = self.cursor_should_be_visible();
        let _ = write!(out, "\x1b[?25l\x1b[s");
        // If the cell moved since the last emit, stamp blanks over the
        // previous position first so the old logo doesn't ghost-overlay
        // the freshly-drawn welcome layout.
        if let Some((px, py)) = self.welcome_codeberg_badge_last_emitted {
            if (px, py) != (cx, cy) {
                let _ = write!(out, "\x1b[{};{}H  ", py + 1, px + 1);
            }
        }
        let _ = write!(out, "\x1b[{};{}H", cy + 1, cx + 1);
        let _ = out.write_all(osc.as_bytes());
        let _ = write!(out, "\x1b[u");
        if cursor_on {
            let _ = write!(out, "\x1b[?25h");
        }
        let _ = out.flush();
        self.welcome_codeberg_badge_last_emitted = Some((cx, cy));
    }

    /// Reset the blink phase so the caret is solidly visible for the next
    /// 530ms. Call after any edit, cursor movement, or focus change so the
    /// user always sees where the cursor just landed before it starts to
    /// blink off.
    fn poke_cursor(&mut self) {
        self.cursor_blink_anchor = std::time::Instant::now();
    }

    /// Mirrors the predicate used in `App::render` to decide whether to call
    /// `frame.set_cursor_position`. Used by the post-draw overlay writer so
    /// it can re-Show the caret after the OSC-1337 image emit only when the
    /// editor would have shown it this frame.
    fn cursor_should_be_visible(&self) -> bool {
        if self.context_menu.is_some() || self.prompt.is_some() || !self.cursor_visible_phase() {
            return false;
        }
        if self.focus == Pane::Editor && self.editor.cursor_screen_pos().is_some() {
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
        const HALF: std::time::Duration = std::time::Duration::from_millis(530);
        let elapsed = self.cursor_blink_anchor.elapsed();
        let phases = (elapsed.as_millis() / HALF.as_millis()) as u64;
        phases % 2 == 0
    }

    /// Re-query git status, but no more than once every ~400ms to avoid
    /// spawning a `git` process on every keystroke.  Called after the file
    /// watcher reports any changes.
    fn refresh_git_status_debounced(&mut self) {
        let min_gap = std::time::Duration::from_millis(400);
        if self.last_git_check.elapsed() < min_gap {
            return;
        }
        self.last_git_check = std::time::Instant::now();
        // Off-thread: post a request and let the worker reply via
        // `git_response_rx`. Used to shell out to `git status [+
        // --porcelain]` synchronously here, blocking the UI thread for
        // 15-50 ms on every FS-watcher tick — the same stall the
        // Source Control click had until this was rewritten.
        let req = if self.sidebar_view == SidebarView::SourceControl {
            crate::git::GitRequest::StatusAndChanges
        } else {
            crate::git::GitRequest::Status
        };
        let _ = self.git_request_tx.send(req);
    }

    fn spawn_fs_watcher(
        root: &Path,
    ) -> Result<(
        notify_debouncer_full::Debouncer<
            notify::RecommendedWatcher,
            notify_debouncer_full::RecommendedCache,
        >,
        std::sync::mpsc::Receiver<notify_debouncer_full::DebounceEventResult>,
    )> {
        use notify::RecursiveMode;
        use notify_debouncer_full::new_debouncer;
        use std::time::Duration;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debouncer = new_debouncer(Duration::from_millis(100), None, tx)
            .context("creating filesystem watcher")?;
        // The watcher backend is fundamentally different on the two
        // first-class targets, so the install strategy MUST differ too -
        // a single uniform strategy is pathological on one of them:
        //
        //  * macOS / FSEvents is per-TREE. One recursive watch covers an
        //    entire subtree in a single stream with no kernel watch limit,
        //    and every `watch()` call tears down and rebuilds that stream.
        //    Calling `watch()` once per directory therefore fires thousands
        //    of FSEventStreamCreate calls and pins a core at ~100% forever
        //    (confirmed by `sample` 2026-05: the FSEvents thread sits in
        //    FSEventStreamCreate). So on macOS we make as FEW calls as
        //    possible.
        //
        //  * Linux / inotify is per-DIRECTORY. A recursive watch installs
        //    one watch per dir in the subtree, and a real repo's
        //    node_modules/target/.git push past
        //    /proc/sys/fs/inotify/max_user_watches (often 8192 on a VPS),
        //    returning ENOSPC so NO watcher installs and the main loop
        //    falls back to a blocking stat-walk that freezes typing over
        //    SSH. Each inotify_add_watch is cheap and independent (no
        //    stream rebuild), so on Linux we add MANY non-recursive watches.
        //
        // Both branches prune the same protected + noise dirs (target/,
        // node_modules/, .git/, ~/Library Containers) which generate
        // cargo/npm/git write storms the debouncer's FileIdMap memcmp loop
        // can't keep up with, and which on macOS also trip Sonoma's App
        // Management TCC class.
        let is_skippable = |name: &std::ffi::OsStr| -> bool {
            FS_WATCH_PROTECTED_NAMES.iter().any(|n| name == *n) || is_noise_dir(name)
        };
        #[cfg(target_os = "macos")]
        {
            let entries: Vec<std::fs::DirEntry> = std::fs::read_dir(root)
                .map(|rd| rd.filter_map(Result::ok).collect())
                .unwrap_or_default();
            if entries.iter().any(|e| is_skippable(&e.file_name())) {
                debouncer
                    .watch(root, RecursiveMode::NonRecursive)
                    .context("starting non-recursive watch on workspace root")?;
                for entry in entries {
                    if is_skippable(&entry.file_name()) {
                        continue;
                    }
                    let path = entry.path();
                    if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                        let _ = debouncer.watch(&path, RecursiveMode::Recursive);
                    }
                }
            } else {
                debouncer
                    .watch(root, RecursiveMode::Recursive)
                    .context("starting watch on workspace root")?;
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Hard ceiling so a pathological tree can't spend forever
            // issuing inotify_add_watch syscalls on the init thread;
            // anything beyond is covered by the adaptive-backoff poll
            // fallback. Per-dir failures are tolerated (we keep going), so
            // a tree that still exceeds the limit installs with partial
            // coverage instead of nothing.
            const MAX_WATCHES: usize = 50_000;
            let mut stack = vec![root.to_path_buf()];
            let mut watched = 0usize;
            while let Some(dir) = stack.pop() {
                if debouncer.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
                    watched += 1;
                }
                if watched >= MAX_WATCHES {
                    break;
                }
                let Ok(rd) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in rd.filter_map(Result::ok) {
                    if is_skippable(&entry.file_name()) {
                        continue;
                    }
                    // `file_type()` does not follow symlinks, so a symlinked
                    // directory reports `is_dir() == false` and we never
                    // descend into it - that also rules out symlink cycles.
                    if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                        stack.push(entry.path());
                    }
                }
            }
        }
        Ok((debouncer, rx))
    }

    fn dir_modified(path: &Path) -> Option<std::time::SystemTime> {
        std::fs::metadata(path).and_then(|m| m.modified()).ok()
    }

    fn file_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
        let meta = std::fs::metadata(path).ok()?;
        let modified = meta.modified().ok()?;
        Some((modified, meta.len()))
    }

    fn snapshot_open_file_mtime(&self) -> Option<(PathBuf, Option<(std::time::SystemTime, u64)>)> {
        self.editor
            .path
            .as_ref()
            .map(|path| (path.clone(), Self::file_stamp(path)))
    }

    fn sync_open_file_poll_mtime(&mut self) {
        self.fs_poll_open_file_mtime = self.snapshot_open_file_mtime();
    }

    fn snapshot_expanded_dir_mtimes(
        tree: &FileTree,
    ) -> std::collections::BTreeMap<PathBuf, Option<std::time::SystemTime>> {
        tree.nodes
            .iter()
            .filter(|n| n.is_dir && n.expanded)
            .map(|n| (n.path.clone(), Self::dir_modified(&n.path)))
            .collect()
    }

    fn reload_open_file_after_external_change(&mut self) -> bool {
        self.refresh_git_status_debounced();
        match self.editor.reload_if_clean() {
            Some(Ok(())) => {
                let path_disp = self
                    .editor
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.status = format!("Reloaded {path_disp} (external change)");
            }
            Some(Err(e)) => {
                self.status = format!("External change but reload failed: {e}");
            }
            None => {
                self.status = String::from(
                    "Open file changed on disk; you have unsaved edits, save or revert to reload",
                );
            }
        }
        self.sync_open_file_poll_mtime();
        true
    }

    fn poll_open_file_change(&mut self) -> bool {
        let current = self.snapshot_open_file_mtime();
        let changed = match (&self.fs_poll_open_file_mtime, &current) {
            (Some((old_path, old_stamp)), Some((path, stamp))) if old_path == path => {
                old_stamp != stamp
            }
            _ => {
                self.fs_poll_open_file_mtime = current;
                return false;
            }
        };
        self.fs_poll_open_file_mtime = current;
        if changed {
            self.reload_open_file_after_external_change()
        } else {
            false
        }
    }

    fn poll_filesystem_changes(&mut self) -> bool {
        if self.fs_poll_last_check.elapsed() < self.fs_poll_interval {
            return false;
        }
        // This walk runs on the render/input thread. When the inotify
        // watcher failed to install (common on remotes: ENOSPC on
        // max_user_watches, or a slow recursive init), it is the only
        // way the tree learns about disk changes — but on a remote
        // filesystem the stat walk can cost hundreds of ms, and at a
        // fixed 50ms cadence it dominates the loop and starves typing
        // (measured: a single iteration > 600ms over SSH). Back the next
        // poll off to a multiple of how long this one actually took, so a
        // cheap local FS still polls at ~20Hz while a slow remote FS polls
        // rarely enough that the loop stays responsive.
        let poll_start = std::time::Instant::now();
        self.fs_poll_last_check = poll_start;
        let mut changed = self.poll_open_file_change();
        let current = Self::snapshot_expanded_dir_mtimes(&self.tree);
        let changed_dirs: Vec<PathBuf> = current
            .iter()
            .filter_map(|(path, stamp)| {
                if self.fs_poll_dir_mtimes.get(path) == Some(stamp) {
                    None
                } else {
                    Some(path.clone())
                }
            })
            .collect();
        if changed_dirs.is_empty() {
            self.fs_poll_dir_mtimes = current;
            self.fs_poll_interval = poll_start
                .elapsed()
                .saturating_mul(10)
                .clamp(FS_POLL_INTERVAL, Duration::from_secs(10));
            return changed;
        }
        for dir in changed_dirs.iter().rev() {
            if let Some(idx) = self.tree.index_of_dir(dir) {
                self.tree.refresh_children(idx);
            }
        }
        self.fs_poll_dir_mtimes = Self::snapshot_expanded_dir_mtimes(&self.tree);
        self.refresh_git_status_debounced();
        // Polling fallback fires when the watcher missed events
        // (no-watcher mode, or notify on a network FS). Kick the index
        // rebuild the same way the watcher path does so Cmd+P stays in
        // sync without a croft restart. Same noise-dir gate as the
        // watcher path — see drain_fs_events for the diagnosis.
        let finder_relevant = changed_dirs
            .iter()
            .any(|p| !crate::widgets::file_finder::is_path_under_noise_dir(p));
        if finder_relevant {
            self.kick_file_finder_index_rebuild();
        }
        self.fs_poll_interval = poll_start
            .elapsed()
            .saturating_mul(10)
            .clamp(FS_POLL_INTERVAL, Duration::from_secs(10));
        changed = true;
        changed
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
        let mut changed = false;
        if let Some(rx) = self.fs_watcher_init_rx.as_ref() {
            match rx.try_recv() {
                Ok((w, evrx)) => {
                    self._fs_watcher = Some(w);
                    self.fs_rx = Some(evrx);
                    self.fs_watcher_init_rx = None;
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.fs_watcher_init_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
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
        let mut changed = false;
        loop {
            match self.git_response_rx.try_recv() {
                Ok(crate::git::GitResponse::Status(s)) => {
                    if self.git_status != s {
                        self.git_status = s.clone();
                        // Mirror the FS-watcher path: the SC panel
                        // header reads the status snapshot too, even
                        // when the row list isn't being refreshed.
                        self.source_control.status = s;
                        changed = true;
                    }
                }
                Ok(crate::git::GitResponse::Changes(entries)) => {
                    self.source_control
                        .set_status(self.git_status.clone(), entries);
                    changed = true;
                }
                Ok(crate::git::GitResponse::StatusAndChanges(s, entries)) => {
                    self.git_status = s.clone();
                    self.source_control.set_status(s, entries);
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if changed && self.git_status.in_repo && self.default_branch_label.is_none() {
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
        let mut changed = false;
        loop {
            match self.sysmon_rx.try_recv() {
                Ok(sample) => {
                    self.system_panel.apply_sample(sample);
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        changed
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
        let _ = self
            .search_query_tx
            .send((self.search.query.clone(), self.search.opts));
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
        let Some(rx) = self.recent_commits_rx.as_ref() else {
            return false;
        };
        match rx.try_recv() {
            Ok((commits, err)) => {
                self.recent_repo_remote = commits.remote;
                self.recent_commits = commits.commits;
                self.recent_commits_rx = None;
                if self.editor.is_blank_initial() && self.welcome_image_displayed {
                    self.welcome_image_clear_requested = true;
                }
                self.welcome_overlay_dirty = true;
                match err {
                    crate::git::RecentCommitsError::None => {}
                    crate::git::RecentCommitsError::Network => {
                        self.status =
                            String::from("Recent commits unavailable: git fetch failed");
                    }
                    crate::git::RecentCommitsError::NoEndpoint => {
                        self.status =
                            String::from("Recent commits unavailable: no remote configured");
                    }
                }
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.recent_commits_rx = None;
                false
            }
        }
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
        let mut to_send: Vec<(bool, PathBuf, String, u64)> = Vec::new();
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
                None => to_send.push((true, path.clone(), tab.lines.join("\n"), seq)),
                Some(p) if p != seq => {
                    to_send.push((false, path.clone(), tab.lines.join("\n"), seq))
                }
                _ => {}
            }
        }
        let lsp = match self.lsp.as_ref() {
            Some(l) => l,
            None => return,
        };
        for (is_open, path, text, seq) in to_send {
            if is_open {
                lsp.open_doc(path.clone(), text);
            } else {
                lsp.change_doc(path.clone(), text);
            }
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
                self.status = format!(
                    "No completions match '{prefix}' ({server_count} from server)"
                );
            } else {
                self.completion_popup = Some(popup);
            }
            changed = true;
        }
        changed
    }

    pub fn trigger_completion(&mut self) {
        if self.editor.diff.is_some()
            || self.editor.sheet.is_some()
            || self.editor.image.is_some()
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
        // Pick up the watcher if its background init has just finished.
        let init_changed = self.try_install_pending_init();
        let Some(rx) = self.fs_rx.as_ref() else {
            let polled = self.poll_filesystem_changes();
            return init_changed || polled;
        };
        let mut affected: std::collections::BTreeSet<PathBuf> =
            std::collections::BTreeSet::new();
        let mut touched_open_file = false;
        let mut got_any = false;
        while let Ok(result) = rx.try_recv() {
            got_any = true;
            let events = match result {
                Ok(evs) => evs,
                Err(_) => continue,
            };
            for ev in events {
                let mutates_content = event_mutates_content(&ev.event.kind);
                for path in &ev.event.paths {
                    // Editor reload trigger: only events that mutate the
                    // file's content. Access reads and metadata-only changes
                    // (atime updates from `cat` / indexers / containerised
                    // overlay filesystems) used to flip this flag too, which
                    // reloaded the editor and wiped any in-flight selection
                    // — confirmed empirically on a Linux remote where the
                    // status bar repeatedly read "Reloaded README.md
                    // (external change)" while the user was trying to
                    // Cmd+A / Shift+Right / mouse-drag.
                    if mutates_content && self.editor.matches_open_path(path) {
                        touched_open_file = true;
                    }
                    if let Some(dir) = crate::widgets::file_tree::affected_dir_for_event(
                        path,
                        &self.tree.root,
                    ) {
                        affected.insert(dir);
                    } else if path == &self.tree.root
                        || path.canonicalize().ok().as_deref()
                            == self.tree.root.canonicalize().ok().as_deref()
                    {
                        affected.insert(self.tree.root.clone());
                    }
                }
            }
        }
        if !affected.is_empty() {
            for dir in affected.iter().rev() {
                if let Some(idx) = self.tree.index_of_dir(dir) {
                    self.tree.refresh_children(idx);
                } else if let Some(c) = dir.canonicalize().ok() {
                    if let Some(idx) = self.tree.index_of_dir(&c) {
                        self.tree.refresh_children(idx);
                    }
                }
            }
            self.fs_poll_dir_mtimes = Self::snapshot_expanded_dir_mtimes(&self.tree);
        }
        if !affected.is_empty() {
            self.refresh_git_status_debounced();
            // A directory's children changed: files were created, deleted,
            // or renamed (or git swapped them on a checkout). Kick a
            // background rebuild of the Cmd+P index so the next Cmd+P
            // reflects what is actually on disk. Coalesces via the dirty
            // flag so a 10k-file checkout produces one trailing rebuild,
            // not 10k of them. Skip rebuild when EVERY affected dir is
            // inside a noise dir (target/, node_modules/, .git/, …) —
            // those paths are excluded from the finder's walk anyway, so
            // a rebuild would re-walk the workspace just to re-emit the
            // same set. Without this gate, a sustained `cargo build`
            // pegged every core via the parallel walker firing
            // back-to-back. Root-caused empirically with
            // `sample $(pgrep -x croft) 5` showing ~50 worker threads in
            // `ignore::WalkBuilder::build_parallel`.
            let finder_relevant = affected
                .iter()
                .any(|p| !crate::widgets::file_finder::is_path_under_noise_dir(p));
            if finder_relevant {
                self.kick_file_finder_index_rebuild();
            }
        }
        if touched_open_file {
            self.reload_open_file_after_external_change();
        }
        let polled = self.poll_filesystem_changes();
        got_any || init_changed || polled
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
        if self.welcome_image_clear_requested
            || (self.welcome_image_displayed && !self.editor.is_blank_initial())
        {
            self.welcome_image_displayed = false;
            self.welcome_image_clear_requested = false;
            true
        } else {
            false
        }
    }

    fn render_welcome(&mut self, frame: &mut ratatui::Frame, outer_area: Rect) {
        self.welcome_links.clear();
        self.welcome_codeberg_badge_cell = None;
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
            Style::default().bg(Color::Rgb(
                EDITOR_BG_RGB.0,
                EDITOR_BG_RGB.1,
                EDITOR_BG_RGB.2,
            ))
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
        let has_recent_panel =
            self.recent_repo_remote.is_some() || !self.recent_commits.is_empty();
        let recents_inner_h = welcome_recents_height(
            self.recent_repo_remote.as_deref(),
            &self.recent_commits,
            inner_w,
        );
        let tagline_h = 1u16;
        let footer_h = 1u16;
        // Gaps: blank row after logo, after tagline, after box.
        let gaps_h = 3u16;

        let logo_max_w = (area.width as u32).saturating_sub(4) as u16;
        let logo_w_cells = logo_max_w.min(48).max(8);
        // Pick the logo height first, then size the recents box to fit
        // whatever's left. Without this a tall commit list would extend the
        // stack past the bottom of the welcome area and we'd panic painting
        // into rows that don't exist (ratatui buffers are fixed-size).
        let logo_h_cells = area
            .height
            .saturating_sub(tagline_h + footer_h + gaps_h)
            .min(14)
            .max(4);
        let used_above_box = logo_h_cells + 1 + tagline_h + 1; // logo, gap, tagline, gap
        let used_below_box = 1 + footer_h; // gap, footer
        let max_box_h = area
            .height
            .saturating_sub(used_above_box)
            .saturating_sub(used_below_box);
        // Box content needs at least the 4-cell border+inset envelope to be
        // worth drawing.
        let desired_box_h = if has_recent_panel { recents_inner_h.saturating_add(4) } else { 0 };
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
        if self.welcome_layout != Some(desired) {
            if self.welcome_image_displayed {
                self.welcome_image_clear_requested = true;
            }
            if let Some((cw, ch)) = self.cell_pixel {
                let canvas_w = (logo_w_cells as u32) * cw;
                let canvas_h = (logo_h_cells as u32) * ch;
                let bg = image::Rgba([
                    EDITOR_BG_RGB.0,
                    EDITOR_BG_RGB.1,
                    EDITOR_BG_RGB.2,
                    0xff,
                ]);
                if let Ok(baked) = crate::iterm2_inline::fit_image(
                    crate::iterm2_inline::WELCOME_LOGO_PNG,
                    canvas_w,
                    canvas_h,
                    bg,
                ) {
                    let raw = crate::iterm2_inline::build_inline_image_osc(
                        &baked,
                        logo_w_cells,
                        logo_h_cells,
                        false,
                    );
                    let osc = if crate::iterm2_inline::detect_tmux() {
                        crate::iterm2_inline::tmux_passthrough_wrap(&raw)
                    } else {
                        raw
                    };
                    self.welcome_image = Some(osc);
                }
            }
            self.welcome_layout = Some(desired);
            self.welcome_overlay_dirty = true;
        }

        if self.welcome_image.is_none() {
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
        if badge_x + version_w + 2 <= area.x + area.width
            && badge_y + 2 < area_max_y
        {
            let badge_style = Style::default()
                .fg(rgb_color(GRAD_TL))
                .add_modifier(Modifier::BOLD);
            frame
                .buffer_mut()
                .set_string(badge_x, badge_y, "\u{256d}", badge_style);
            for i in 0..version_w {
                frame.buffer_mut().set_string(
                    badge_x + 1 + i,
                    badge_y,
                    "\u{2500}",
                    badge_style,
                );
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
        if has_recent_panel
            && box_h >= 4
            && block_w >= 4
            && box_y + box_h <= area_max_y
        {
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

            let mut row_y = header_y + 2;
            if let Some(remote) = self.recent_repo_remote.as_ref() {
                let provider = welcome_provider_label(remote);
                let badge = welcome_provider_badge(remote);
                frame
                    .buffer_mut()
                    .set_string(inner_x, row_y, &badge, link_style);
                let is_codeberg = matches!(
                    crate::git::commit_api_provider_for_remote(remote),
                    Some(crate::git::CommitApiProvider::Codeberg)
                );
                self.welcome_codeberg_badge_cell =
                    if is_codeberg && self.welcome_codeberg_badge_osc.is_some() {
                        Some((inner_x, row_y))
                    } else {
                        None
                    };
                let badge_w = badge.chars().count() as u16;
                let remote_x = inner_x + badge_w + 2;
                let room = (inner_x + inner_w_actual)
                    .saturating_sub(remote_x) as usize;
                let clipped: String = remote.chars().take(room).collect();
                frame.buffer_mut().set_string(remote_x, row_y, clipped, dim);
                let link_w = badge_w
                    .saturating_add(2)
                    .saturating_add(room.min(remote.chars().count()) as u16)
                    .min(inner_w_actual);
                self.welcome_links.push(WelcomeLink {
                    rect: Rect { x: inner_x, y: row_y, width: link_w, height: 1 },
                    url: remote.clone(),
                    label: format!("Open {provider} repository"),
                });
                row_y += 2;
            }
            for c in &self.recent_commits {
                let y = row_y;
                if y >= box_rect.y + box_rect.height - 1 {
                    break;
                }
                frame.buffer_mut().set_string(inner_x, y, &c.hash, link_style);
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
                if let Some(remote) = self.recent_repo_remote.as_ref() {
                    if let Some(url) = crate::git::commit_url_for_remote(remote, &c.full_hash) {
                        let height = subject_lines.len().max(1) as u16;
                        self.welcome_links.push(WelcomeLink {
                            rect: Rect { x: inner_x, y, width: inner_w_actual, height },
                            url,
                            label: format!("Open commit {}", c.hash),
                        });
                    }
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
        let bg = if self.activity_images.is_some() {
            Style::default().bg(Color::Reset)
        } else {
            Style::default().bg(Color::Rgb(EDITOR_BG_RGB.0, EDITOR_BG_RGB.1, EDITOR_BG_RGB.2))
        };
        // In images mode the icon PNG owns the entire activity-bar block —
        // background, codicon, and active pill are baked in. Rendering a bg
        // block here would force a per-cell diff every frame, which in turn
        // would force us to re-emit the OSC-1337 images on every draw. Both
        // are visible to the user. So in images mode we leave the cells
        // untouched and let the post-draw OSC writer paint them once.
        if self.activity_images.is_none() {
            frame.render_widget(
                ratatui::widgets::Block::default().style(bg),
                area,
            );
        }
        let active_bar = Color::Rgb(0x4e, 0x9a, 0xff);
        let bg_color = bg.bg.unwrap_or(Color::Reset);
        let explorer_block = activity_explorer_block(area);
        let search_block = activity_search_block(area);
        let source_control_block = activity_source_control_block(area);
        let remote_block = activity_remote_block(area);
        let run_debug_block = activity_run_debug_block(area);
        let explorer_active = self.sidebar_view == SidebarView::Explorer;
        let search_active = self.sidebar_view == SidebarView::Search;
        let source_control_active = self.sidebar_view == SidebarView::SourceControl;
        let remote_active = self.sidebar_view == SidebarView::Remote;
        let run_debug_active = self.sidebar_view == SidebarView::RunDebug;

        let active_color = Color::White;
        let inactive_color = Color::Rgb(0x6c, 0x7d, 0x9c);
        let glyph_x = activity_icon_glyph_x(area);
        let render_glyph =
            |frame: &mut ratatui::Frame, block: Rect, glyph: char, is_active: bool| {
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
                let color = if is_active {
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

        if self.activity_images.is_none() {
            // Glyph fallback path: render the codicon and a separate active
            // pill on the leftmost column. iTerm2's image path bakes the
            // pill into the PNG itself, so this branch is only used on
            // terminals that can't render OSC-1337.
            render_glyph(
                frame,
                explorer_block,
                crate::icons::ACTIVITY_EXPLORER,
                explorer_active,
            );
            render_glyph(
                frame,
                search_block,
                crate::icons::ACTIVITY_SEARCH,
                search_active,
            );
            render_glyph(
                frame,
                source_control_block,
                crate::icons::ACTIVITY_SOURCE_CONTROL,
                source_control_active,
            );
            render_glyph(
                frame,
                remote_block,
                crate::icons::ACTIVITY_REMOTE,
                remote_active,
            );
            render_glyph(
                frame,
                run_debug_block,
                crate::icons::ACTIVITY_RUN_DEBUG,
                run_debug_active,
            );
        }

        self.sidebar_areas.explorer_icon = explorer_block;
        self.sidebar_areas.search_icon = search_block;
        self.sidebar_areas.source_control_icon = source_control_block;
        self.sidebar_areas.remote_icon = remote_block;
        self.sidebar_areas.run_debug_icon = run_debug_block;
    }

    fn set_sidebar_view(&mut self, view: SidebarView) {
        if self.sidebar_view != view {
            self.activity_overlay_dirty = true;
            // Leaving Search while a scan is in flight: send an
            // empty-query request so the worker cancels the live walker
            // threads instead of letting them keep saturating every core
            // until the scan finishes. The user's hits stay populated for
            // when they come back; only the live work is dropped.
            if self.sidebar_view == SidebarView::Search && view != SidebarView::Search {
                let _ = self
                    .search_query_tx
                    .send((String::new(), self.search.opts));
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
                && self.run_debug_icon_last_emitted.is_some()
            {
                self.run_debug_image_clear_requested = true;
            }
            // Same eviction trick for the Source Control no-repo hero
            // PNG: leaving Source Control after the image was painted
            // would otherwise leave the cached PNG under whatever
            // sidebar replaces it.
            if self.sidebar_view == SidebarView::SourceControl
                && view != SidebarView::SourceControl
                && self.no_repo_hero_displayed
            {
                self.no_repo_hero_image_clear_requested = true;
            }
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
            if self.welcome_image_displayed {
                self.welcome_image_clear_requested = true;
            }
        } else {
            self.welcome_overlay_dirty = true;
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
    /// front. Mirror of `cycle_terminal` for the Ctrl+Shift+[ binding.
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
        self.terminals.iter().fold(false, |acc, t| acc | t.take_dirty())
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
        let focused_pane = self.focus == Pane::Terminal;
        let active = self.active_terminal;
        for (i, t) in self.terminals.iter_mut().enumerate() {
            t.focused = focused_pane && i == active;
        }
    }

    fn render(&mut self, frame: &mut ratatui::Frame) {
        let size = frame.area();
        self.last_frame_area = size;
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
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

        let (editor_area, terminal_area) = if self.show_terminal {
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
                        total_h.saturating_sub(EDITOR_HEIGHT_MIN).max(TERMINAL_HEIGHT_MIN),
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
            let (activity_area, panel_area) = if panel_h > 0 {
                let split = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(1), Constraint::Length(panel_h)])
                    .split(usable_area);
                (split[0], Some(split[1]))
            } else {
                (usable_area, None)
            };
            match self.sidebar_view {
                SidebarView::Explorer => frame.render_widget(&mut self.tree, activity_area),
                SidebarView::Search => frame.render_widget(&mut self.search, activity_area),
                SidebarView::SourceControl => {
                    frame.render_widget(&mut self.source_control, activity_area)
                }
                SidebarView::Remote => frame.render_widget(&mut self.remote, activity_area),
                SidebarView::RunDebug => frame.render_widget(&mut self.run_debug, activity_area),
            }
            if let Some(pa) = panel_area {
                frame.render_widget(&mut self.system_panel, pa);
            } else {
                self.system_panel.last_area = Rect::default();
            }
        }
        if self.editor.is_blank_initial() {
            self.render_welcome(frame, editor_area);
            // The previous frame may have rendered an image-preview tab
            // whose OSC-1337 pixels are still cached in iTerm's image
            // store. Closing that tab brings us here, but ratatui's diff
            // alone doesn't tell iTerm to drop the image — flag it so the
            // main loop wipes the screen on the next pass.
            self.disable_editor_image();
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
            frame.render_widget(&mut self.editor, editor_area);
            // The editor just overdrew whatever cells the welcome image
            // occupied; if the user reopens the welcome screen we'll need
            // to re-emit it.
            self.welcome_overlay_dirty = true;
            self.welcome_codeberg_badge_cell = None;
            self.update_editor_image_overlay(editor_area);
            if self.focus == Pane::Editor && self.completion_popup.is_some() {
                if let Some((cx, cy)) = self.editor.cursor_screen_pos() {
                    if let Some(popup) = self.completion_popup.as_mut() {
                        popup.anchor = (cx, cy);
                    }
                    let popup_ref = self.completion_popup.as_ref().unwrap();
                    let area = popup_ref.area_for(editor_area);
                    if area.width > 0 && area.height > 0 {
                        frame.render_widget(popup_ref, area);
                    }
                }
            }
        }
        if let Some(area) = terminal_area {
            let n = self.terminals.len().max(1);
            let constraints: Vec<Constraint> = (0..n)
                .map(|_| Constraint::Ratio(1, n as u32))
                .collect();
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(constraints)
                .split(area);
            for (i, t) in self.terminals.iter_mut().enumerate() {
                frame.render_widget(t, cols[i]);
            }
            let show_close = self.terminals.len() > 1;
            self.terminal_add_buttons.clear();
            self.terminal_close_buttons.clear();
            for col in cols.iter().take(self.terminals.len()) {
                let (add_rect, close_rect) =
                    paint_terminal_pane_buttons(frame, *col, show_close);
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
        }

        let mut spans: Vec<Span> = Vec::with_capacity(20);
        spans.push(Span::styled(
            brand_pill_text(),
            Style::default()
                .bg(Color::Rgb(0x4e, 0x9a, 0xff))
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ));
        if self.perf_hud {
            spans.push(Span::styled(
                format!(
                    " ⚡d{}µs w{}µs p{}µs {}ips {}kps ",
                    self.last_draw_us,
                    self.last_work_us,
                    self.last_poll_us,
                    self.last_ips,
                    self.last_kps
                ),
                Style::default()
                    .bg(Color::Rgb(0x7a, 0x1f, 0x1f))
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.extend(git_status_spans(&self.git_status));
        spans.push(Span::raw("  "));
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
        let shortcuts_label_start_col: u16 = spans
            .iter()
            .map(|s| s.content.chars().count() as u16)
            .sum();
        spans.push(Span::styled("F1", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" Shortcuts"));
        let shortcuts_label_len: u16 = "F1 Shortcuts".chars().count() as u16;
        let status_rect = outer[1];
        let hit_x = status_rect.x.saturating_add(shortcuts_label_start_col);
        let hit_end = hit_x.saturating_add(shortcuts_label_len).min(status_rect.right());
        self.shortcuts_hit_rect = (hit_end > hit_x).then(|| Rect {
            x: hit_x,
            y: status_rect.y,
            width: hit_end - hit_x,
            height: 1,
        });
        let status = Paragraph::new(Line::from(spans))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x3a, 0x6e)));
        frame.render_widget(status, outer[1]);

        // Overlays render last so they sit on top of everything else.
        self.render_context_menu(frame);
        self.render_commit_dropdown(frame);
        self.render_prompt(frame);
        self.render_local_open_confirm(frame);
        self.render_discard_confirm(frame);
        self.render_editor_find(frame);
        self.render_file_finder(frame);
        self.render_shortcuts_modal(frame);
        self.render_connect_dialog(frame);

        // Show the host terminal's hardware caret only when the editor is
        // focused and has no modal overlay. The DECSCUSR style is set to
        // BlinkingBar at startup, so the terminal blinks a thin vertical
        // line over the cursor cell without replacing its character.
        if self.focus == Pane::Editor
            && self.context_menu.is_none()
            && self.prompt.is_none()
            && self.cursor_visible_phase()
        {
            if let Some((cx, cy)) = self.editor.cursor_screen_pos() {
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
        {
            if let Some((cx, cy)) = self.source_control.cursor_screen_pos() {
                frame.set_cursor_position((cx, cy));
            }
        }
    }

    fn render_context_menu(&self, frame: &mut ratatui::Frame) {
        let Some(menu) = &self.context_menu else { return };
        // `menu_rect` already clamps against `last_frame_area`, so the
        // rect we draw here is the same rect `menu_item_at` hit-tests
        // against. Keeping the two in lock-step is what prevents the
        // off-by-N row dispatch when the menu has to shift up to fit.
        let Some(clipped) = self.menu_rect() else { return };
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x1e, 0x1e)));
        frame.render_widget(ratatui::widgets::Clear, clipped);
        frame.render_widget(block, clipped);
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
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(0x4e, 0x9a, 0xff))
            } else {
                Style::default().fg(Color::White)
            };
            // Paint the label, then right-align the shortcut hint on the
            // same row so the menu reads like VS Code's: action on the
            // left, accelerator on the right.
            let line = ratatui::text::Line::from(format!(" {label}"));
            frame.render_widget(
                ratatui::widgets::Paragraph::new(line).style(row_style),
                row,
            );
            if let Some(sc) = shortcut_for(action) {
                let sc_w = sc.chars().count() as u16;
                if sc_w + 2 <= inner.width {
                    let sc_x = inner.x + inner.width - sc_w - 1;
                    let sc_style = if selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Rgb(0x4e, 0x9a, 0xff))
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
        if !self.commit_menu_open {
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
            .map(|item| (*item, format!(" {} ", item.label(&default_branch))))
            .collect();
        let label_w = items
            .iter()
            .map(|(_, l)| l.chars().count() as u16)
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
            let row_rect = Rect { x: item_x, y: row_y, width: item_w, height: 1 };
            for rx in 0..row_rect.width {
                frame.buffer_mut()[(row_rect.x + rx, row_rect.y)]
                    .set_symbol(" ")
                    .set_style(style);
            }
            frame.buffer_mut().set_string(
                row_rect.x,
                row_rect.y,
                &label,
                style,
            );
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
            CommitMenuItem::ViewDefaultBranchDiff => {
                self.view_default_branch_diff_source_control()
            }
        }
    }

    fn render_discard_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(pd) = self.pending_discard.as_ref() else { return };
        let area = frame.area();
        let width = area.width.saturating_sub(8).min(96).max(50);
        let height: u16 = 8;
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect { x, y, width, height };
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
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("o / Esc"),
            ]),
        ]);
        frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
    }

    fn render_local_open_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(url) = &self.pending_local_open else { return };
        let area = frame.area();
        let width = area.width.saturating_sub(8).min(96).max(50);
        let height: u16 = 8;
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect { x, y, width, height };
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
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("es once   "),
                ratatui::text::Span::styled(
                    "[A]",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
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
        let width = area.width.saturating_sub(8).min(80).max(40);
        let height = if p.error.is_some() { 6 } else { 5 };
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect { x, y, width, height };
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x1e, 0x1e)))
            .title(ratatui::text::Span::styled(
                format!(" {} ", p.label),
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
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
        let (top_line, hint_text) = match &p.kind {
            PromptKind::Create(_) => (
                ratatui::text::Line::from(vec![
                    ratatui::text::Span::raw("> "),
                    ratatui::text::Span::styled(
                        p.buffer.as_str(),
                        Style::default().fg(Color::White),
                    ),
                    ratatui::text::Span::styled(
                        "█",
                        Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)),
                    ),
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
                    ratatui::text::Span::styled(
                        "█",
                        Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)),
                    ),
                ]),
                "Enter to rename, Esc to cancel",
            ),
        };
        frame.render_widget(
            ratatui::widgets::Paragraph::new(top_line),
            Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 },
        );
        let hint = ratatui::widgets::Paragraph::new(ratatui::text::Line::from(hint_text))
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(
            hint,
            Rect { x: inner.x, y: inner.y + 2, width: inner.width, height: 1 },
        );
        if let Some(err) = &p.error {
            let line = ratatui::widgets::Paragraph::new(ratatui::text::Line::from(format!(
                "Error: {err}"
            )))
            .style(Style::default().fg(Color::Rgb(0xe8, 0x27, 0x4b)));
            frame.render_widget(
                line,
                Rect { x: inner.x, y: inner.y + 3, width: inner.width, height: 1 },
            );
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
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
        if matches!(key.code, KeyCode::F(1)) {
            self.open_shortcuts_modal();
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(8)) {
            self.perf_hud = !self.perf_hud;
            return Ok(());
        }
        if is_file_finder_key(key) {
            self.open_file_finder();
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
        // App-wide shortcuts (priority). Skip Cmd+S when the Source
        // Control pane is focused so the SC handler can use it as the
        // stage shortcut — saving an editor file with no editor focus
        // doesn't make sense anyway, and globally swallowing Cmd+S here
        // is what made the user's first attempt feel inert.
        let sc_focused = self.focus == Pane::Tree
            && self.sidebar_view == SidebarView::SourceControl;
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
        if is_terminal_split_key(key) {
            match self.split_terminal() {
                Ok(()) => {
                    self.status =
                        format!("Split terminal: {} active", self.terminals.len());
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
            self.set_sidebar_view(SidebarView::Search);
            return Ok(());
        }
        if is_source_control_jump_key(key) && self.focus != Pane::Editor {
            self.set_sidebar_view(SidebarView::SourceControl);
            return Ok(());
        }
        if is_explorer_jump_key(key) {
            self.show_tree = true;
            self.set_sidebar_view(SidebarView::Explorer);
            return Ok(());
        }
        if is_source_control_jump_via_s_key(key) {
            self.show_tree = true;
            self.set_sidebar_view(SidebarView::SourceControl);
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
        if self.sidebar_view == SidebarView::Search
            && self.focus == Pane::Tree
            && is_search_editing_shortcut(key)
        {
            self.handle_search_key(key);
            return Ok(());
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
                self.quit = true;
                return Ok(());
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
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
                    && self.run_debug_icon_last_emitted.is_some()
                {
                    self.run_debug_image_clear_requested = true;
                }
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
        if is_search_paste_key(key) {
            let text = (self.clipboard_reader)();
            self.paste_clipboard_into_search(text.as_deref());
            return;
        }
        if is_editor_select_all_key(key) {
            self.search.select_all_query();
            return;
        }
        if is_editor_copy_key(key) {
            let text = self.search.selection_text();
            if !text.is_empty() {
                copy_to_clipboard(&text);
                self.status = format!("Copied {} chars to clipboard", text.chars().count());
            }
            return;
        }
        if is_editor_cut_key(key) {
            let text = self.search.selection_text();
            if !text.is_empty() {
                copy_to_clipboard(&text);
                let n = text.chars().count();
                self.search.delete_selection();
                self.submit_search_query();
                self.status = format!("Cut {n} chars to clipboard");
            }
            return;
        }
        match key.code {
            KeyCode::Esc => {
                if self.search.query.is_empty() {
                    self.set_sidebar_view(SidebarView::Explorer);
                } else {
                    self.search.query.clear();
                    self.search.clear_selection();
                    self.search.hits.clear();
                    self.submit_search_query();
                }
            }
            KeyCode::Enter => {
                if let Some(hit) = self.search.selected_hit().cloned() {
                    self.open_search_hit(&hit);
                }
            }
            KeyCode::Backspace => {
                if self.search.delete_selection() {
                    self.submit_search_query();
                } else if self.search.query.pop().is_some() {
                    self.submit_search_query();
                }
            }
            KeyCode::Up => self.search.move_up(),
            KeyCode::Down => self.search.move_down(),
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER)
                {
                    self.search.delete_selection();
                    self.search.query.push(c);
                    self.submit_search_query();
                } else if key.modifiers.contains(KeyModifiers::SUPER) {
                    self.status = format!("Search: unhandled Cmd+{c}");
                } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.status = format!("Search: unhandled Ctrl+{c}");
                }
            }
            _ => {}
        }
    }

    fn handle_remote_key(&mut self, key: KeyEvent) {
        let has_mod = key.modifiers.intersects(
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
        );
        match key.code {
            KeyCode::Up => self.remote.move_up(),
            KeyCode::Down => self.remote.move_down(),
            KeyCode::PageUp => self.remote.scroll_up(10),
            KeyCode::PageDown => self.remote.scroll_down(10),
            KeyCode::Backspace => {
                if !self.remote.filter.is_empty() {
                    self.remote.pop_filter_char();
                }
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
            KeyCode::Esc => {
                if !self.remote.clear_filter() {
                    self.set_sidebar_view(SidebarView::Explorer);
                }
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
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Enter)
        {
            self.commit_and_push_source_control();
            return;
        }
        // Cmd+A: select every entry row in the panel so a follow-up Cmd+S
        // stages them all in one go. Distinct from the editor's Cmd+A
        // (select-all text) which the user kept hitting in the SC pane
        // and getting the wrong scope.
        if cmd_or_ctrl
            && matches!(key.code, KeyCode::Char('a') | KeyCode::Char('A'))
        {
            let n = self.source_control.select_all_changes();
            self.status = format!("Selected {n} change(s)");
            return;
        }
        // Cmd+S: stage the selected entry, or every entry in the
        // multi-selection. Mirrors clicking the inline "+" icon but
        // sweeps across the whole selection.
        if cmd_or_ctrl
            && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S'))
        {
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
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER)
                {
                    self.source_control.insert_char(c);
                    self.poke_cursor();
                }
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
        let _ = self
            .git_request_tx
            .send(crate::git::GitRequest::Changes);
    }

    fn handle_run_debug_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.set_sidebar_view(SidebarView::Explorer),
            KeyCode::Enter => self.run_active_file(),
            _ => {}
        }
    }

    fn refresh_run_debug(&mut self) {
        let path = self.editor.path.clone();
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
            _ => return None,
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
                        match self.editor.open_head_diff_with_text(label, &head_text, &abs) {
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
                self.last_git_check = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(1))
                    .unwrap_or_else(std::time::Instant::now);
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
            self.source_control.selected_change.iter().copied().collect()
        } else {
            self.source_control.multi_selection.iter().copied().collect()
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
        self.last_git_check = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
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
        self.last_git_check = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
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

    pub fn commit_and_push_source_control(&mut self) {
        let message = self.source_control.message.trim().to_string();
        if message.is_empty() {
            self.source_control.commit_feedback =
                Some(String::from("Empty commit message"));
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
        self.last_git_check = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
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
                self.last_git_check = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(1))
                    .unwrap_or_else(std::time::Instant::now);
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
        let Some(pd) = self.pending_discard.take() else { return };
        match crate::git::discard_path(&self.tree.root, &pd.rel_path, pd.untracked) {
            Ok(()) => {
                self.status = format!("Discarded {}", pd.rel_path);
                self.last_git_check = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(1))
                    .unwrap_or_else(std::time::Instant::now);
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

    fn commit_source_control(&mut self) {
        let message = self.source_control.message.trim().to_string();
        if message.is_empty() {
            self.source_control.commit_feedback =
                Some(String::from("Empty commit message"));
            self.source_control.commit_feedback_is_error = true;
            return;
        }
        match crate::git::commit_all_tracked(&self.tree.root, &message) {
            Ok(summary) => {
                self.source_control.clear_message();
                self.source_control.commit_feedback = Some(summary.clone());
                self.source_control.commit_feedback_is_error = false;
                self.status = format!("Committed: {summary}");
                self.last_git_check = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(1))
                    .unwrap_or_else(std::time::Instant::now);
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
                self.connect_dialog =
                    Some(crate::widgets::connect_dialog::ConnectDialog::new(host.clone()));
                self.pending_remote_launch_path = path;
                self.pending_remote_launch_host = Some(host);
                self.welcome_image_clear_requested = true;
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
        if authenticated {
            if let (Some(auth), Some(host)) = (
                self.connect_auth.take(),
                self.pending_remote_launch_host.take(),
            ) {
                let adopted = auth.hand_off();
                let path = self.pending_remote_launch_path.take();
                self.install_session = Some(
                    crate::install_session::InstallSession::start(adopted, host.clone(), path),
                );
                self.status = format!("Preparing remote croft on {host}…");
            }
        }
        true
    }

    pub fn poll_install_session(&mut self) -> bool {
        let Some(session) = self.install_session.as_mut() else {
            return false;
        };
        let events = session.drain_events();
        let mut changed = !events.is_empty();
        let mut done = false;
        let mut failure: Option<String> = None;
        if let Some(dialog) = self.connect_dialog.as_mut() {
            for ev in events {
                match ev {
                    crate::install_session::InstallEvent::Log(line) => {
                        dialog.push_log(&line);
                    }
                    crate::install_session::InstallEvent::Done(Ok(())) => {
                        done = true;
                    }
                    crate::install_session::InstallEvent::Done(Err(detail)) => {
                        failure = Some(detail);
                    }
                }
            }
        } else {
            for ev in events {
                match ev {
                    crate::install_session::InstallEvent::Done(Ok(())) => done = true,
                    crate::install_session::InstallEvent::Done(Err(detail)) => {
                        failure = Some(detail)
                    }
                    crate::install_session::InstallEvent::Log(_) => {}
                }
            }
        }
        if let Some(detail) = failure {
            if let Some(dialog) = self.connect_dialog.as_mut() {
                dialog.set_failed(detail.clone());
            }
            self.install_session = None;
            self.status = format!("Remote install failed: {detail}");
            changed = true;
        } else if done {
            if let Some(mut session) = self.install_session.take() {
                if let Some(adopted) = session.take_adopted() {
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
            }
            changed = true;
        }
        changed
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
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ')
            ) {
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
            KeyCode::Char('l') | KeyCode::Char('L')
                if has_mod || !dialog.phase.awaits_input() =>
            {
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
            dialog.status_line =
                format!("Sending {} chars to {}…", payload.chars().count(), dialog.host);
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
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, "") {
                self.status = format!("Could not create {}: {e}", path.display());
                return;
            }
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
                let row = hit.line_no.saturating_sub(1).min(
                    self.editor.lines.len().saturating_sub(1),
                );
                self.editor.cursor_row = row;
                // Drop the cursor at the first match column on this line so
                // long lines (HTML with base64 blobs, minified JS) auto-scroll
                // horizontally and the user lands on the match instead of the
                // line's leftmost cell - which on a 200k-char line means
                // never seeing the match without manual scrolling.
                let needle = self.search.query.trim();
                let opts = self.search.opts;
                let line = self.editor.lines.get(row).cloned().unwrap_or_default();
                let match_col = if needle.is_empty() {
                    0
                } else {
                    let mut col = 0usize;
                    let mut found = None;
                    for (chunk, is_match) in
                        crate::widgets::search::split_for_highlight(&line, needle, opts)
                    {
                        if is_match {
                            found = Some(col);
                            break;
                        }
                        col = col.saturating_add(chunk.chars().count());
                    }
                    found.unwrap_or(0)
                };
                self.editor.cursor_col = match_col;
                self.editor.scroll_col = 0;
                self.editor.ensure_cursor_col_visible();
                self.status = format!(
                    "Opened {} at line {}",
                    hit.path.display(),
                    hit.line_no
                );
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
                        self.editor_image_layout = None;
                    }
                }
            }
            return;
        }
        if self.handle_editor_vim_chord(key) {
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
        if is_close_tab_key(key) {
            if self.editor.close_active() {
                self.sync_open_file_poll_mtime();
                self.status = String::from("Closed tab");
            } else {
                self.status = String::from("Cannot close last tab");
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
        if shift && alt && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            match key.code {
                KeyCode::Up => self.editor.duplicate_lines_up(),
                KeyCode::Down => self.editor.duplicate_lines_down(),
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
            KeyCode::Tab => self.editor.insert_str("    "),
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER)
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

        if op_pending || count_pending {
            if let Some(d) = chord_digit_value(key) {
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
                    self.status = format!(
                        "Deleted {n} {}",
                        if n == 1 { "line" } else { "lines" }
                    );
                }
                VimOp::Y => {
                    let n = count.max(1);
                    let yanked = self.editor.yank_lines(n);
                    if !yanked.is_empty() {
                        copy_to_clipboard(&yanked);
                    }
                    self.status = format!(
                        "Yanked {n} {}",
                        if n == 1 { "line" } else { "lines" }
                    );
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

    fn handle_terminal_key(&mut self, key: KeyEvent) {
        // Ctrl+Shift+T: open another terminal next to the active one.
        if is_terminal_split_key(key) {
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
        // Ctrl+Shift+W: close the active terminal (no-op when only one is left).
        if is_terminal_close_key(key) {
            if self.close_active_terminal() {
                self.status =
                    format!("Closed terminal: {} remaining", self.terminals.len());
            } else {
                self.status = String::from("Cannot close the last terminal; press Ctrl+J to hide");
            }
            return;
        }
        // Ctrl+Shift+]: next terminal. Ctrl+Shift+[: previous terminal.
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
        let Some(sel) = self.terminal().selection() else { return };
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
            if let Some(text) = diff.selection_text() {
                if !text.is_empty() {
                    copy_to_clipboard(&text);
                    self.status =
                        format!("Copied {} chars to clipboard", text.chars().count());
                }
            }
            return;
        }
        let Some(sel) = self.editor.selection else { return };
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
        let Some(sel) = self.editor.selection else { return };
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
        let before = self.search.query.chars().count();
        self.search.insert_str_into_query(s);
        self.submit_search_query();
        let after = self.search.query.chars().count();
        let inserted = after.saturating_sub(before);
        if inserted == 0 {
            self.status = format!(
                "Cmd+V: saw clipboard chars={}, inserted 0 after filtering",
                s.chars().count()
            );
        } else {
            self.status = format!("Cmd+V: inserted {inserted} chars; query len {after}");
        }
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
        if self.editor_find.is_some() {
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
                SidebarView::Remote => {
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
        if self.sidebar_view == SidebarView::Search && self.focus != Pane::Editor {
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
        if let Some(first) = placed.first() {
            if let Some(idx) = self.tree.nodes.iter().position(|n| &n.path == first) {
                self.tree.selected = idx;
                self.tree.anchor = idx;
            }
        }
        self.status = if !errors.is_empty() {
            format!(
                "Imported {}/{}; failed: {}",
                placed.len(),
                total,
                errors.join("; ")
            )
        } else if total == 1 {
            format!("Imported {} into {}", paths[0].display(), dest_dir.display())
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
            self.status =
                String::from("Drop ignored: no Remote Explorer host selected");
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
                self.status =
                    String::from("Cmd+V: fetching local Mac clipboard via croft relay…");
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
                let msg = std::fs::read_to_string(&err)
                    .unwrap_or_else(|_| String::from("relay error"));
                errors.push(format!("{}: {}", pull.src_display, msg.trim()));
                let _ = std::fs::remove_dir_all(&request_dir);
            } else if pull.started_at.elapsed() > REMOTE_PULL_TIMEOUT {
                errors.push(format!("{}: timed out after 120s", pull.src_display));
                let _ = std::fs::remove_dir_all(&request_dir);
            } else {
                still_pending.push(pull);
            }
        }
        if let Some(bytes) = clipboard_payload {
            if !bytes.is_empty() {
                self.terminal_mut().paste_input(&bytes);
            }
        }
        let resolved_any =
            !placed.is_empty() || !opened_urls.is_empty() || !errors.is_empty();
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
            if let Some(first) = placed.first() {
                if let Some(idx) = self.tree.nodes.iter().position(|n| &n.path == first) {
                    self.tree.selected = idx;
                    self.tree.anchor = idx;
                }
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
        self.welcome_links
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
                self.status = format!(
                    "Trusted local browser for this session. Opening {url}…"
                );
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
            self.shortcuts_modal =
                Some(crate::widgets::shortcuts::ShortcutsModal::default());
            self.shortcuts_image_clear_requested = true;
            self.status = String::from("Shortcuts: Esc to close");
        }
    }

    fn close_shortcuts_modal(&mut self) {
        if self.shortcuts_modal.take().is_some() {
            self.shortcuts_image_clear_requested = true;
            self.activity_overlay_dirty = true;
            self.welcome_overlay_dirty = true;
            self.no_repo_hero_overlay_dirty = true;
            self.editor_image_layout = None;
            self.status.clear();
        }
    }

    pub fn consume_shortcuts_image_clear(&mut self) -> bool {
        if self.shortcuts_image_clear_requested {
            self.shortcuts_image_clear_requested = false;
            true
        } else {
            false
        }
    }

    pub fn consume_file_finder_image_clear(&mut self) -> bool {
        if self.file_finder_image_clear_requested {
            self.file_finder_image_clear_requested = false;
            true
        } else {
            false
        }
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
        let (tx, rx) = std::sync::mpsc::channel::<
            Vec<crate::widgets::file_finder::FileEntry>,
        >();
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
            let entries =
                crate::widgets::file_finder::build_file_index(&self.workspace_root);
            self.file_finder_index = Some(std::sync::Arc::new(entries));
            self.file_finder_index_rx = None;
        }
        let entries = self
            .file_finder_index
            .clone()
            .unwrap_or_else(|| std::sync::Arc::new(Vec::new()));
        let count = entries.len();
        self.file_finder = Some(crate::widgets::file_finder::FileFinder::new(entries));
        self.file_finder_image_clear_requested = true;
        self.status = format!("Go to File: {count} files indexed — Esc to close");
    }

    fn close_file_finder(&mut self) {
        if self.file_finder.take().is_some() {
            self.file_finder_image_clear_requested = true;
            self.activity_overlay_dirty = true;
            self.welcome_overlay_dirty = true;
            self.no_repo_hero_overlay_dirty = true;
            self.editor_image_layout = None;
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
            if let Some(text) = (self.clipboard_reader)() {
                if let Some(finder) = self.file_finder.as_mut() {
                    for c in text.chars() {
                        if !c.is_control() {
                            finder.push_char(c);
                        }
                    }
                }
            }
            return;
        }
        let Some(finder) = self.file_finder.as_mut() else { return };
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
            (KeyCode::Char(c), m)
                if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                finder.push_char(c);
            }
            _ => {}
        }
    }

    fn handle_file_finder_mouse(&mut self, m: MouseEvent) {
        let Some(finder) = self.file_finder.as_mut() else { return };
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
            MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::Down(MouseButton::Right) => {
                if !inside {
                    self.close_file_finder();
                }
            }
            _ => {}
        }
    }

    fn render_file_finder(&mut self, frame: &mut ratatui::Frame) {
        let Some(finder) = self.file_finder.as_mut() else { return };
        let area = frame.area();
        crate::widgets::file_finder::render_file_finder(finder, area, frame.buffer_mut());
    }

    fn open_editor_find(&mut self) {
        if self.editor_find.is_some() {
            return;
        }
        let opts = self.search.opts;
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
            state.match_count = crate::widgets::editor_find::count_matches(
                &self.editor.lines,
                &initial,
                opts,
            );
            self.editor.set_search_highlight(Some(initial.clone()), opts);
        }
        self.editor_find = Some(state);
        self.status = String::from("Find: type to search, Enter next, Shift+Enter prev, Esc close");
    }

    fn close_editor_find(&mut self) {
        if self.editor_find.take().is_some() {
            self.editor
                .set_search_highlight(None, self.search.opts);
            self.status.clear();
        }
    }

    fn editor_find_set_query(&mut self, new_query: String) {
        let Some(state) = self.editor_find.as_mut() else { return };
        if state.query == new_query {
            return;
        }
        state.query = new_query;
        let opts = state.opts;
        state.match_count = crate::widgets::editor_find::count_matches(
            &self.editor.lines,
            &state.query,
            opts,
        );
        if state.query.is_empty() {
            state.match_index = None;
            self.editor.active_search_match = None;
            self.editor.set_search_highlight(None, opts);
            return;
        }
        self.editor.set_search_highlight(Some(state.query.clone()), opts);
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
        let Some(state) = self.editor_find.as_ref() else { return };
        if state.query.is_empty() {
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
        let Some(state) = self.editor_find.as_ref() else { return };
        if state.query.is_empty() {
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
        let Some(state) = self.editor_find.as_mut() else { return };
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
            (KeyCode::Char(c), m)
                if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
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
        let Some(state) = self.editor_find.as_mut() else { return };
        let area = self.editor.last_full_area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        crate::widgets::editor_find::render_editor_find(state, area, frame.buffer_mut());
    }

    fn handle_shortcuts_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.shortcuts_modal.as_mut() else { return };
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
        let Some(modal) = self.shortcuts_modal.as_mut() else { return };
        let inside = rect_contains(modal.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => modal.scroll_down(3),
            MouseEventKind::ScrollUp => modal.scroll_up(3),
            MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::Down(MouseButton::Right) => {
                if !inside {
                    self.close_shortcuts_modal();
                }
            }
            _ => {}
        }
    }

    fn render_shortcuts_modal(&mut self, frame: &mut ratatui::Frame) {
        let Some(modal) = self.shortcuts_modal.as_mut() else { return };
        let area = frame.area();
        crate::widgets::shortcuts::render_shortcuts_modal(modal, area, frame.buffer_mut());
    }

    fn render_connect_dialog(&mut self, frame: &mut ratatui::Frame) {
        let visible = self.cursor_visible_phase();
        let Some(dialog) = self.connect_dialog.as_mut() else { return };
        let area = frame.area();
        use ratatui::widgets::Widget as _;
        dialog.render(area, frame.buffer_mut());
        if visible {
            if let Some((cx, cy)) = dialog.cursor_screen_pos() {
                frame.set_cursor_position((cx, cy));
            }
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
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

        match m.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                // Editor tab strip: right-click on a tab opens the
                // Close / Close Others / Close to the Right / Close All
                // menu. Routed here BEFORE the explorer branch so the
                // hit-test wins even when the editor pane shares its
                // column with the sidebar.
                if let Some(tab_idx) = self.editor.tab_at(m.column, m.row) {
                    self.focus_pane(Pane::Editor);
                    let items =
                        build_tab_context_menu_items(tab_idx, self.editor.tab_count());
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
                    let target_dir = crate::widgets::file_tree::create_target_dir_for(
                        node, &self.tree.root,
                    );
                    let selection = self.tree.action_paths();
                    let items = build_tree_context_menu_items(
                        node,
                        &self.tree.root,
                        &selection,
                        &target_dir,
                        self.tree_clipboard.as_ref(),
                        self.compare_anchor.as_deref(),
                    );
                    self.context_menu = Some(ContextMenu {
                        origin: (m.column, m.row),
                        items,
                        selected: 0,
                        target_dir,
                    });
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if !in_editor {
                    self.last_editor_left_down = None;
                }
                if !in_tree {
                    self.last_tree_left_down = None;
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
                    if m.column == x || m.column == x.saturating_sub(1) {
                        self.splitter_drag = Some(SplitterDrag::Sidebar);
                        return;
                    }
                }
                // SYSTEM panel sits at the bottom of the sidebar column.
                // A click on its header row toggles collapse; any other
                // click inside its rect is swallowed so it doesn't fall
                // through to the activity widget above (which would
                // otherwise see the click as occurring on a row that's
                // visually covered by the panel).
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
                    let in_splitter_x_range = self
                        .sidebar_splitter_x
                        .is_none_or(|x| m.column >= x);
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
                    self.last_tree_left_down = None;
                    return;
                }
                if in_remote_scrollbar {
                    self.focus_pane(Pane::Tree);
                    self.remote.scroll_to_bar_y(m.row);
                    self.scrollbar_drag = Some(Pane::Tree);
                    self.last_tree_left_down = None;
                    return;
                }
                if in_search_scrollbar {
                    self.focus_pane(Pane::Tree);
                    self.search.scroll_to_bar_y(m.row);
                    self.scrollbar_drag = Some(Pane::Tree);
                    self.last_tree_left_down = None;
                    return;
                }
                if in_editor_scrollbar {
                    self.focus_pane(Pane::Editor);
                    self.editor.scroll_to_bar_y(m.row);
                    self.scrollbar_drag = Some(Pane::Editor);
                    self.last_editor_left_down = None;
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
                    // Click on a result row: open it.
                    if let Some(idx) = self.search.hit_at_y(m.row) {
                        self.search.selected = idx;
                        if let Some(hit) = self.search.selected_hit().cloned() {
                            self.open_search_hit(&hit);
                        }
                    } else {
                        // Click on the input/header area: just focus search.
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::SourceControl {
                    self.focus_pane(Pane::Tree);
                    if self.commit_menu_open {
                        if let Some(item) =
                            self.source_control.click_commit_menu_item(m.column, m.row)
                        {
                            self.commit_menu_open = false;
                            self.dispatch_commit_menu_item(item);
                            return;
                        }
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
                    if let Some(idx) =
                        self.source_control.click_discard_action(m.column, m.row)
                    {
                        self.request_discard_source_control_entry(idx);
                        return;
                    }
                    if let Some(idx) =
                        self.source_control.click_stage_action(m.column, m.row)
                    {
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
                    if self.run_debug.click_button(m.column, m.row) {
                        self.run_active_file();
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
                        let is_double = matches!(
                            self.last_tree_left_down,
                            Some((t, x, y))
                                if m.row == y
                                    && m.column.abs_diff(x) <= 1
                                    && now.duration_since(t) <= DOUBLE_CLICK_WINDOW
                        );
                        if is_double {
                            if let Some(target) = self.remote.selected_target().cloned() {
                                self.request_remote_launch(target.alias, None);
                            }
                            self.last_tree_left_down = None;
                        } else {
                            self.last_tree_left_down = Some((now, m.column, m.row));
                        }
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Explorer {
                    self.focus_pane(Pane::Tree);
                    if let Some(idx) = self.tree.node_at_y(m.row) {
                        let now = std::time::Instant::now();
                        let is_double = matches!(
                            self.last_tree_left_down,
                            Some((t, x, y))
                                if m.row == y
                                    && m.column.abs_diff(x) <= 1
                                    && now.duration_since(t) <= DOUBLE_CLICK_WINDOW
                        );
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
                            self.last_tree_left_down = None;
                            self.tree_drag = None;
                            return;
                        }
                        // Alt/Ctrl-click: defer the toggle until mouse-up so a
                        // movement in between can promote the gesture into an
                        // Alt-drag (copy) instead. Don't activate (no file
                        // open, no folder toggle) and don't include the row
                        // in the multi-selection yet.
                        if has_toggle_mod {
                            self.last_tree_left_down = None;
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
                        let already_marked =
                            self.tree.marked.contains(&self.tree.nodes[idx].path);
                        if !already_marked {
                            self.tree.select_replace(idx);
                        } else {
                            self.tree.selected = idx;
                            self.tree.anchor = idx;
                        }
                        let clicked_is_dir = self
                            .tree
                            .nodes
                            .get(idx)
                            .is_some_and(|node| node.is_dir);
                        if clicked_is_dir && is_double {
                            self.last_tree_left_down = None;
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
                            self.last_tree_left_down = None;
                        } else {
                            self.last_tree_left_down = Some((now, m.column, m.row));
                        }
                    }
                } else if in_editor_pane && !in_editor {
                    if let Some(idx) = self.editor.close_at(m.column, m.row) {
                        if self.editor.close_tab(idx) {
                            self.sync_open_file_poll_mtime();
                            self.status = String::from("Closed tab");
                            self.poke_cursor();
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
                        let is_double = matches!(
                            self.last_editor_left_down,
                            Some((t, x, y))
                                if m.row == y
                                    && m.column.abs_diff(x) <= 1
                                    && now.duration_since(t) <= DOUBLE_CLICK_WINDOW
                        );
                        let last_inner = self.editor.last_inner;
                        if let Some(diff) = self.editor.diff.as_mut() {
                            if let Some((side, row_idx, char_col)) =
                                crate::widgets::editor::diff_hit_test(
                                    diff,
                                    last_inner,
                                    m.column,
                                    m.row,
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
                        self.last_editor_left_down = if is_double {
                            None
                        } else {
                            Some((now, m.column, m.row))
                        };
                        self.poke_cursor();
                        return;
                    }
                    let now = std::time::Instant::now();
                    let is_double = matches!(
                        self.last_editor_left_down,
                        Some((t, x, y))
                            if m.row == y
                                && m.column.abs_diff(x) <= 1
                                && now.duration_since(t) <= DOUBLE_CLICK_WINDOW
                    );
                    if is_double {
                        self.editor.select_word_at(m.column, m.row);
                        self.last_editor_left_down = None;
                    } else {
                        // Anchor a fresh selection at the click; a drag widens it,
                        // a clean click ends up cleared on mouse-up.
                        self.editor.mouse_down(m.column, m.row);
                        self.last_editor_left_down = Some((now, m.column, m.row));
                    }
                    self.poke_cursor();
                } else if let Some(idx) = terminal_hit {
                    if self.active_terminal != idx {
                        self.active_terminal = idx;
                    }
                    self.focus_pane(Pane::Terminal);
                    let now = std::time::Instant::now();
                    let is_double = matches!(
                        self.last_terminal_left_down,
                        Some((t, x, y))
                            if m.row == y
                                && m.column.abs_diff(x) <= 1
                                && now.duration_since(t) <= DOUBLE_CLICK_WINDOW
                    );
                    if is_double {
                        self.terminal_mut().select_word_at(m.column, m.row);
                        self.last_terminal_left_down = None;
                    } else {
                        // Begin a fresh selection at the click cell. Without a
                        // drag this is a single cell (no area), so the selection
                        // ends up cleared on mouse-up. With a drag, this is the
                        // selection anchor.
                        self.terminal_mut().start_selection_at(m.column, m.row);
                        self.last_terminal_left_down = Some((now, m.column, m.row));
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(kind) = self.splitter_drag {
                    self.handle_splitter_drag(kind, m.column, m.row);
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
                if let Some((_, x, y)) = self.last_editor_left_down {
                    if m.column != x || m.row != y {
                        self.last_editor_left_down = None;
                    }
                }
                if let Some((_, x, y)) = self.last_tree_left_down {
                    if m.column != x || m.row != y {
                        self.last_tree_left_down = None;
                    }
                }
                if let Some((_, x, y)) = self.last_terminal_left_down {
                    if m.column != x || m.row != y {
                        self.last_terminal_left_down = None;
                    }
                }
                if in_editor {
                    if self.editor.diff.is_some() {
                        let last_inner = self.editor.last_inner;
                        if let Some(diff) = self.editor.diff.as_mut() {
                            if let Some((_, row_idx, char_col)) =
                                crate::widgets::editor::diff_hit_test(
                                    diff,
                                    last_inner,
                                    m.column,
                                    m.row,
                                )
                            {
                                diff.extend_selection_to(row_idx, char_col);
                            }
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
                                    drag.target_idx = drag_target_index(
                                        &self.tree,
                                        m.row,
                                        &drag.paths,
                                    );
                                    self.tree.drag_target = drag.target_idx;
                                }
                            }
                            // While not dragging, treat continued movement as a
                            // range-extend from the initial anchor — just like
                            // VS Code's drag-to-select.
                            if self.tree_drag.as_ref().is_none_or(|d| !d.armed) {
                                if let Some(idx) = self.tree.node_at_y(m.row) {
                                    self.tree.select(idx);
                                }
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
                if self.splitter_drag.take().is_some() {
                    return;
                }
                if self.scrollbar_drag.take().is_some() {
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
                    if let Some(sel) = self.terminal().selection() {
                        if !sel.has_area() {
                            self.terminal_mut().clear_selection();
                        }
                    }
                } else if in_editor {
                    if let Some(sel) = self.editor.selection {
                        if !sel.has_area() {
                            self.editor.clear_selection();
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                if in_tree {
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
                if in_tree {
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
            MouseEventKind::ScrollLeft => {
                if in_editor {
                    if let Some(diff) = self.editor.diff.as_mut() {
                        diff.scroll_left_by(4);
                    } else {
                        self.editor.scroll_left_by(4);
                    }
                }
            }
            MouseEventKind::ScrollRight => {
                if in_editor {
                    if let Some(diff) = self.editor.diff.as_mut() {
                        diff.scroll_right_by(4);
                    } else {
                        self.editor.scroll_right_by(4);
                    }
                }
            }
            _ => {}
        }
    }

    fn save(&mut self) {
        match self.editor.save_to_disk() {
            Ok(()) => self.status = self.editor.status.clone(),
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
        Some(Rect { x, y, width, height })
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
                if let Some(menu) = self.context_menu.as_mut() {
                    if menu.selected > 0 {
                        menu.selected -= 1;
                    }
                }
            }
            KeyCode::Down => {
                if let Some(menu) = self.context_menu.as_mut() {
                    if menu.selected + 1 < menu.items.len() {
                        menu.selected += 1;
                    }
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
        if let Some(active) = self.terminals.get_mut(self.active_terminal) {
            active.change_cwd(&new_root);
        }
        self.workspace_root = new_root.clone();
        self.tree.set_root(new_root.clone());
        self.tree_clipboard = None;
        self.compare_anchor = None;
        // Drop the old watcher synchronously (free), spawn the new one
        // on a worker thread. Same pattern App::new uses at startup.
        self._fs_watcher = None;
        self.fs_rx = None;
        let (fs_init_tx, fs_init_rx) = std::sync::mpsc::channel();
        let root_for_fs = new_root.clone();
        std::thread::spawn(move || {
            if let Ok(pair) = Self::spawn_fs_watcher(&root_for_fs) {
                let _ = fs_init_tx.send(pair);
            }
        });
        self.fs_watcher_init_rx = Some(fs_init_rx);
        self.fs_poll_dir_mtimes = Self::snapshot_expanded_dir_mtimes(&self.tree);
        self.last_git_check = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
        // Rebind the worker's root before any query, otherwise the next
        // Status / Changes request runs against the stale root captured
        // at App::new and the Source Control panel keeps reporting the
        // OLD repo's state (or "no repo") even after a Make Root that
        // landed inside a real git tree.
        let _ = self
            .git_request_tx
            .send(crate::git::GitRequest::SetRoot(new_root.clone()));
        self.refresh_git_status_debounced();
        self.refresh_source_control();
        // Drop any in-flight walker for the OLD root so its result is
        // discarded when it tries to send. Then kick a fresh walk
        // against the new root. file_finder_index keeps the old entries
        // for now so a Cmd+P press during the rebuild window shows
        // stale-but-immediate results instead of a UI freeze.
        self.file_finder_index_rx = None;
        self.file_finder_index_dirty = false;
        self.kick_file_finder_index_rebuild();
        self.status = format!("Workspace root: {display}");
    }

    fn is_explorer_focused(&self) -> bool {
        self.focus == Pane::Tree && self.sidebar_view == SidebarView::Explorer
    }

    /// Dispatch the Explorer-pane keyboard shortcuts (Cmd+F / Cmd+Shift+F /
    /// Cmd+R / F2 / Cmd+/). Returns true if the key was handled. The
    /// target node is whichever row the user has highlighted; folder-only
    /// actions (Make root) are no-ops on file selections.
    fn handle_explorer_shortcut(&mut self, key: KeyEvent) -> bool {
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
            MenuAction::CloseTab(idx) => {
                if self.editor.close_tab(idx) {
                    self.sync_open_file_poll_mtime();
                    self.status = String::from("Closed tab");
                    self.poke_cursor();
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
            }
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
        }
    }

    /// Bake an OSC-1337 inline-image escape sized to the editor pane so
    /// the active image tab can be painted on top of ratatui's text
    /// buffer. Skipped when the active tab is text or when the host
    /// terminal can't render inline images (Terminal.app, raw SSH); in
    /// those cases the metadata header line painted by the editor widget
    /// is the entire preview the user sees.
    fn update_editor_image_overlay(&mut self, editor_area: Rect) {
        let Some(image) = self.editor.image.clone() else {
            self.disable_editor_image();
            return;
        };
        let path = match self.editor.path.clone() {
            Some(p) => p,
            None => {
                self.disable_editor_image();
                return;
            }
        };
        if !crate::iterm2_inline::detect_iterm2_inline_support() {
            self.disable_editor_image();
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
            self.disable_editor_image();
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
        if self.editor_image_layout.as_ref() == Some(&desired) {
            return;
        }
        if self.editor_image_displayed {
            self.editor_image_clear_requested = true;
        }
        let canvas_w = cell_w as u32 * cw_px;
        let canvas_h = cell_h as u32 * ch_px;
        let bg = image::Rgba([
            EDITOR_BG_RGB.0,
            EDITOR_BG_RGB.1,
            EDITOR_BG_RGB.2,
            0xff,
        ]);
        if let Ok(baked) =
            crate::iterm2_inline::fit_image_auto(&image.bytes, canvas_w, canvas_h, bg)
        {
            let raw = crate::iterm2_inline::build_inline_image_osc(
                &baked, cell_w, cell_h, false,
            );
            let osc = if crate::iterm2_inline::detect_tmux() {
                crate::iterm2_inline::tmux_passthrough_wrap(&raw)
            } else {
                raw
            };
            self.editor_image_osc = Some(osc);
            self.editor_image_layout = Some(desired);
        }
    }

    fn disable_editor_image(&mut self) {
        if self.editor_image_displayed {
            self.editor_image_clear_requested = true;
        }
        self.editor_image_osc = None;
        self.editor_image_layout = None;
    }

    /// Returns true if the cached editor-image cells need to be repainted
    /// by ratatui this frame (because the user closed/switched away from
    /// the image, or the layout changed). Called from the main loop to
    /// force a full redraw before re-emitting.
    pub fn consume_editor_image_clear(&mut self) -> bool {
        if self.editor_image_clear_requested {
            self.editor_image_clear_requested = false;
            self.editor_image_displayed = false;
            return true;
        }
        false
    }

    pub fn editor_image_payload(&self) -> Option<(&str, &EditorImageLayout)> {
        if self.shortcuts_modal.is_some() || self.file_finder.is_some() {
            return None;
        }
        let osc = self.editor_image_osc.as_deref()?;
        let layout = self.editor_image_layout.as_ref()?;
        Some((osc, layout))
    }

    pub fn mark_editor_image_displayed(&mut self) {
        self.editor_image_displayed = true;
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
        let anchor = diff.scroll.saturating_add(2);
        let target = if forward {
            diff.next_change_row_wrap(anchor)
        } else {
            diff.prev_change_row_wrap(anchor)
        };
        if let Some(row) = target {
            diff.scroll_to_row(row);
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
        // Page = inner viewport rows minus the header + footer the diff
        // renderer reserves. Falls back to a sane default when the editor
        // hasn't laid out yet.
        let page = (self.editor.last_inner.height as usize).saturating_sub(2).max(1);
        let Some(diff) = self.editor.diff.as_mut() else {
            return;
        };
        // F7 / Shift+F7: jump to next / previous change hunk. Matches the
        // VS Code keybinding for "Next/Previous Change in Diff Editor" so
        // users can scan a long diff without scrolling line-by-line.
        if matches!(key.code, KeyCode::F(7)) {
            // `scroll_to_row` parks the viewport 2 rows above the target,
            // so the hunk currently in view sits at row `scroll + 2`.
            // Anchoring next/prev queries at that row, not at `scroll`,
            // means F7 reliably jumps PAST the hunk the user is reading.
            // Use the wrap variants so F7 at the last hunk loops back to
            // the first, matching the click-arrow behaviour.
            let anchor = diff.scroll.saturating_add(2);
            let target = if key.modifiers.contains(KeyModifiers::SHIFT) {
                diff.prev_change_row_wrap(anchor)
            } else {
                diff.next_change_row_wrap(anchor)
            };
            if let Some(row) = target {
                diff.scroll_to_row(row);
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
            KeyCode::Down => {
                if data.scroll_row + 1 < row_count {
                    data.scroll_row += 1;
                }
            }
            KeyCode::Up => {
                data.scroll_row = data.scroll_row.saturating_sub(1);
            }
            KeyCode::Right => {
                if data.scroll_col + 1 < col_count {
                    data.scroll_col += 1;
                }
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
            KeyCode::Tab => {
                if total_sheets > 1 {
                    sheet.current_sheet = (current + 1) % total_sheets;
                }
            }
            KeyCode::BackTab => {
                if total_sheets > 1 {
                    sheet.current_sheet = (current + total_sheets - 1) % total_sheets;
                }
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
                self.terminal_height =
                    Some(new_h.clamp(TERMINAL_HEIGHT_MIN, max_h));
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
    fn apply_paste_or_drop(
        &mut self,
        dest_dir: &Path,
        paths: &[PathBuf],
        mode: ExplorerClipMode,
    ) {
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
                ExplorerClipMode::Copy => {
                    crate::widgets::file_tree::copy_into(dest_dir, src)
                }
            };
            match result {
                Ok(p) => {
                    if matches!(mode, ExplorerClipMode::Cut)
                        && self.editor.matches_open_path(src)
                    {
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
        if let Some(first) = placed.first() {
            if let Some(idx) = self.tree.nodes.iter().position(|n| &n.path == first) {
                self.tree.selected = idx;
                self.tree.anchor = idx;
            }
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
                {
                    if let Some(p) = self.prompt.as_mut() {
                        p.buffer.push(c);
                        p.error = None;
                    }
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
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'d') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT)
        || key.modifiers.contains(KeyModifiers::ALT)
    {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

fn is_search_jump_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
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
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'f') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT)
        || key.modifiers.contains(KeyModifiers::ALT)
    {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// VS Code-style `Cmd+P` / `Ctrl+P`: open the Quick Open file finder.
/// Plain `p` with no modifier (or with Shift / Alt only) falls through so
/// the editor still receives literal `p` keystrokes.
fn is_file_finder_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'p') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT)
        || key.modifiers.contains(KeyModifiers::ALT)
    {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Explorer-pane shortcut: `Cmd+F` / `Ctrl+F` (no Shift, no Alt) - "New File".
fn is_tree_new_file_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'f') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT)
        || key.modifiers.contains(KeyModifiers::ALT)
    {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Explorer-pane shortcut: `Cmd+Shift+N` / `Ctrl+Shift+N` - "New Folder".
/// The global dispatcher gives the Explorer-context handler first refusal
/// so this only fires when the tree is the focused pane; outside Explorer
/// the chord is unbound and falls through to the focused pane.
fn is_tree_new_folder_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'n') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::SHIFT)
        || key.modifiers.contains(KeyModifiers::ALT)
    {
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
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'r') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT)
        || key.modifiers.contains(KeyModifiers::ALT)
    {
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
    if key.modifiers.contains(KeyModifiers::SHIFT)
        || key.modifiers.contains(KeyModifiers::ALT)
    {
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

/// `Ctrl/Cmd+Shift+S`: jump to the Source Control sidebar from any pane.
/// Companion to the older `Cmd+Shift+G` chord (which has the `focus != Editor`
/// guard because Cmd+Shift+G in the editor is goto-bottom). Croft has no
/// Save-As, so claiming Cmd+Shift+S across every pane is collision-free.
fn is_source_control_jump_via_s_key(key: KeyEvent) -> bool {
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

fn is_cmd_shift_letter(key: KeyEvent, letter: char) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
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

/// `Ctrl/Cmd+Shift+G`: jump to the Source Control sidebar view, matching
/// VS Code's "Show Source Control" gesture.
fn is_source_control_jump_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'g') {
        return false;
    }
    let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let has_ctrl_or_super = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER);
    has_shift && has_ctrl_or_super
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
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
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

/// `Ctrl+Shift+T`: spawn an additional terminal next to the active one.
/// Rejects SUPER so the Mac-side `Cmd+Shift+T` focus chord (see
/// `is_terminal_focus_key`) does not double-fire as a split when the
/// user happens to be holding both modifiers.
fn is_terminal_split_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'t') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SUPER) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Cmd+Shift+T` (Mac SUPER+SHIFT+T): focus the Terminal pane from any
/// pane, un-hiding it if it was collapsed via Ctrl+J. SUPER-only so the
/// cross-platform `Ctrl+Shift+T` (split-terminal) chord keeps its
/// historical meaning - the two chords share the same letter but are
/// disambiguated by the modifier.
fn is_terminal_focus_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'t') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT)
        || key.modifiers.contains(KeyModifiers::CONTROL)
    {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Ctrl+Shift+W`: close the active terminal (no-op if it's the only one).
fn is_terminal_close_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'w') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Ctrl+Shift+]`: cycle to the next terminal in the pane.
fn is_terminal_cycle_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if c != ']' && c != '}' {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Ctrl+Shift+[`: cycle to the previous terminal in the pane. Mirror of
/// `is_terminal_cycle_key` for the reverse direction. Accepts both the
/// raw `[` keycode and the shift-applied `{` since crossterm reports one
/// or the other depending on terminal flavour.
fn is_terminal_cycle_back_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if c != '[' && c != '{' {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// Returns true if the given key event should trash the currently-selected
/// node in the file tree. Recognises:
///   * `KeyCode::Delete`       — the Forward-Delete key on full-size keyboards
///                                (and `fn+Delete` on Mac laptops).
///   * `KeyCode::Backspace`    — the key labeled "delete" on every Mac keyboard.
///                                The tree pane has no text input that needs
///                                Backspace, so plain Backspace is safe here.
///   * `Cmd+Backspace`         — the macOS Finder gesture for trashing.
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
    // Cmd+Shift+S is reserved for the Source Control sidebar jump
    // (`is_source_control_jump_via_s_key`), so reject Shift when Super is
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
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'c') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Cut: `Ctrl+X` / `Cmd+X`.
fn is_editor_cut_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
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

fn is_editor_paste_key(key: KeyEvent) -> bool {
    is_clipboard_paste_key(key)
}

fn is_clipboard_paste_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
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
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'a') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Undo: `Ctrl+Z` / `Cmd+Z`. Plain Shift is ignored (Shift+Cmd+Z is reserved
/// for redo, which croft does not implement yet).
fn is_editor_undo_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'z') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

fn chord_status_text(op: char, count: usize) -> String {
    if count > 0 {
        format!("Pending: {op} (count {count})")
    } else {
        format!("Pending: {op}")
    }
}

fn is_vim_chord_letter(key: KeyEvent, letter: char) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if c != letter {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT)
        || key.modifiers.contains(KeyModifiers::ALT)
    {
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
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'g') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
}

fn chord_digit_value(key: KeyEvent) -> Option<usize> {
    let KeyCode::Char(c) = key.code else { return None };
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

fn is_plain_letter(key: KeyEvent, letter: char) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if c != letter {
        return false;
    }
    !(key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER)
        || key.modifiers.contains(KeyModifiers::ALT))
}

fn is_editor_line_home_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'a') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn is_editor_line_end_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'e') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn is_editor_kill_to_eol_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'k') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn is_editor_kill_to_bol_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'u') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn is_editor_open_line_below_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if c != 'o' {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
}

fn is_editor_open_line_above_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
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
    let KeyCode::Char(c) = key.code else { return false };
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
    let KeyCode::Char(c) = key.code else { return None };
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
        if let Some(parent) = src.parent() {
            if parent == dir_path && drag_paths.len() == 1 {
                // Dropping a single source onto its own parent is a no-op;
                // don't show a highlight so the user knows nothing will move.
                return None;
            }
        }
    }
    Some(dir_idx)
}

fn drop_target_dir(
    tree: &crate::widgets::file_tree::FileTree,
    target_idx: usize,
) -> PathBuf {
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
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r')
            && !in_single
            && !in_double
        {
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
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let (Some(h), Some(l)) =
                    (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2]))
                {
                    path.push((h * 16 + l) as char);
                    i += 3;
                    continue;
                }
            }
            path.push(bytes[i] as char);
            i += 1;
        }
        s = path;
    }
    let mut unescaped = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                unescaped.push(next);
                continue;
            }
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
    r.width > 0
        && r.height > 0
        && x >= r.x
        && x < r.x + r.width
        && y >= r.y
        && y < r.y + r.height
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

/// Classify a notify `EventKind` as touching file content. Pure reads
/// (`Access(_)`) and metadata-only mutations (`Modify(Metadata(_))` —
/// chmod, chown, atime, xattr) leave bytes on disk unchanged and must
/// not trigger an editor reload, otherwise the open buffer's selection
/// is wiped on every benign indexer/atime update on Linux remotes.
fn event_mutates_content(kind: &notify::EventKind) -> bool {
    use notify::event::ModifyKind;
    use notify::EventKind;
    match kind {
        EventKind::Access(_) => false,
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        _ => true,
    }
}

/// True when croft was invoked over an SSH login (or otherwise inside a
/// remote shell). Used to throttle PTY-driven redraws further so the SSH
/// pipe never saturates and starves input handling on the same thread.
fn is_remote_session() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_TTY").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
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
) -> (Option<Rect>, Option<Rect>) {
    let add_w = TERMINAL_ADD_LABEL.chars().count() as u16;
    let close_w = TERMINAL_CLOSE_LABEL.chars().count() as u16;
    if area.height == 0 {
        return (None, None);
    }
    let style = Style::default()
        .fg(Color::White)
        .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
        .add_modifier(Modifier::BOLD);
    let y = area.y;
    let mut add_rect: Option<Rect> = None;
    let mut close_rect: Option<Rect> = None;

    if area.width >= add_w + 2 {
        let x = area.x + area.width - add_w - 1;
        frame.buffer_mut().set_string(x, y, TERMINAL_ADD_LABEL, style);
        add_rect = Some(Rect { x, y, width: add_w, height: 1 });
    }
    if show_close_button && area.width >= add_w + close_w + 2 {
        // Sit just to the left of the add button.
        let x = area.x + area.width - add_w - close_w - 1;
        frame.buffer_mut().set_string(x, y, TERMINAL_CLOSE_LABEL, style);
        close_rect = Some(Rect { x, y, width: close_w, height: 1 });
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
                if ('a'..='z').contains(&lc) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn fs_watcher_does_not_descend_into_protected_top_level_dirs() {
        // Regression for the macOS App Management TCC prompt: when the
        // workspace contains a `Library` subdir at the top level (as $HOME
        // does), the watcher must not call WalkDir+stat into it. Asserted
        // here by making `Library` unreadable (mode 000): a recursive
        // watch would fail spawning because notify_debouncer_full would
        // hit a permission error inside the walk; our split-watch path
        // skips Library entirely and starts cleanly.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src").join("a.txt"), "hi").unwrap();
        let library = tmp.path().join("Library");
        std::fs::create_dir(&library).unwrap();
        std::fs::create_dir(library.join("Containers")).unwrap();
        std::fs::write(library.join("Containers").join("payload"), "x").unwrap();
        // Mode 000 makes any descent fail; if the watcher were to descend,
        // the WalkDir/stat would error during cache init.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&library, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let result = App::spawn_fs_watcher(tmp.path());
        // Restore perms so tempdir cleanup works.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&library, std::fs::Permissions::from_mode(0o755));
        }
        assert!(
            result.is_ok(),
            "watcher must skip protected dirs and start cleanly"
        );
    }

    #[test]
    fn fs_watcher_does_not_deliver_events_from_target_or_node_modules_subtrees() {
        // Regression for the M4 CPU storm: when the workspace contains
        // `target/` or `node_modules/` at the top level, the FSEvents
        // thread used to spend ~60% of its samples inside
        // notify_debouncer_full's FileIdMap memcmp loop processing cargo
        // and npm writes that we then discarded post-event. The fix is to
        // never subscribe to those subtrees in the first place (same
        // split-watch pattern already used for Library/.Trash, extended
        // to NOISE_DIR_NAMES). This test asserts the contract by writing
        // a burst of files into `target/` and verifying the debouncer
        // channel receives zero events for those paths, while writes
        // under a normal sibling like `src/` still come through.
        let tmp = tempfile::tempdir().unwrap();
        let src_raw = tmp.path().join("src");
        let target_raw = tmp.path().join("target");
        let node_modules_raw = tmp.path().join("node_modules");
        std::fs::create_dir(&src_raw).unwrap();
        std::fs::create_dir(&target_raw).unwrap();
        std::fs::create_dir(&node_modules_raw).unwrap();
        let src = src_raw.canonicalize().unwrap();
        let target = target_raw.canonicalize().unwrap();
        let node_modules = node_modules_raw.canonicalize().unwrap();

        let (_debouncer, rx) = App::spawn_fs_watcher(tmp.path())
            .expect("watcher must start cleanly with noise dirs present");

        for _ in 0..30 {
            while rx.try_recv().is_ok() {}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        for i in 0..50 {
            std::fs::write(target.join(format!("cargo_{i}.o")), b"x").unwrap();
            std::fs::write(node_modules.join(format!("pkg_{i}.js")), b"x").unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(400));

        let mut noise_events: Vec<std::path::PathBuf> = Vec::new();
        while let Ok(batch) = rx.try_recv() {
            for ev in batch.unwrap_or_default() {
                for path in &ev.event.paths {
                    if path.starts_with(&target) || path.starts_with(&node_modules) {
                        noise_events.push(path.clone());
                    }
                }
            }
        }
        assert!(
            noise_events.is_empty(),
            "writes under target/ and node_modules/ must not produce debouncer events; got {noise_events:?}"
        );

        std::fs::write(src.join("a.rs"), b"fn main() {}").unwrap();
        let started = std::time::Instant::now();
        let mut signal_events: Vec<std::path::PathBuf> = Vec::new();
        while started.elapsed() < std::time::Duration::from_millis(1500) {
            while let Ok(batch) = rx.try_recv() {
                for ev in batch.unwrap_or_default() {
                    for path in &ev.event.paths {
                        if path.starts_with(&src) {
                            signal_events.push(path.clone());
                        }
                    }
                }
            }
            if !signal_events.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !signal_events.is_empty(),
            "writes under src/ must still produce debouncer events after the noise-dir split"
        );
    }

    #[test]
    fn drain_fs_events_returns_false_when_nothing_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.fs_watcher_init_rx = None;
        for _ in 0..20 {
            let _ = app.drain_fs_events();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!app.drain_fs_events(), "no fs events ⇒ no redraw needed");
    }

    #[test]
    fn drain_fs_events_returns_true_after_workspace_write() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "*.txt\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        for _ in 0..20 {
            let _ = app.drain_fs_events();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let new_file = tmp.path().join("new.txt");
        std::fs::write(&new_file, "hi").unwrap();
        let started = std::time::Instant::now();
        let mut saw = false;
        let mut saw_tree = false;
        for _ in 0..150 {
            if app.drain_fs_events() {
                saw = true;
            }
            if app.tree.nodes.iter().any(|n| n.path == new_file) {
                saw_tree = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(saw, "workspace write should propagate as a dirty signal");
        assert!(saw_tree, "workspace write should refresh the tree with the created file");
        assert!(
            started.elapsed() <= std::time::Duration::from_millis(200),
            "created file should appear in Explorer within 200ms"
        );
    }

    #[test]
    fn drain_fs_events_removes_deleted_root_file_from_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let doomed = tmp.path().join("doomed.txt");
        std::fs::write(&doomed, "bye").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        assert!(
            app.tree.nodes.iter().any(|n| n.path == doomed),
            "precondition: initial tree contains the file"
        );

        std::fs::remove_file(&doomed).unwrap();
        let started = std::time::Instant::now();
        let mut saw_tree = false;
        for _ in 0..150 {
            let _ = app.drain_fs_events();
            if !app.tree.nodes.iter().any(|n| n.path == doomed) {
                saw_tree = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(saw_tree, "deleted file should disappear from the tree");
        assert!(
            started.elapsed() <= std::time::Duration::from_millis(200),
            "deleted file should disappear from Explorer within 200ms"
        );
    }

    #[test]
    fn drain_fs_events_polling_fallback_refreshes_tree_without_watcher_event() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app._fs_watcher = None;
        app.fs_rx = None;
        app.fs_watcher_init_rx = None;
        app.fs_poll_dir_mtimes.clear();
        app.fs_poll_last_check = std::time::Instant::now() - FS_POLL_INTERVAL;

        let new_file = tmp.path().join("new.txt");
        std::fs::write(&new_file, "hi").unwrap();

        assert!(app.drain_fs_events());
        assert!(
            app.tree.nodes.iter().any(|n| n.path == new_file),
            "polling fallback should refresh the tree when no watcher event arrives"
        );
    }

    #[test]
    fn drain_fs_events_polling_fallback_reloads_clean_open_file_without_watcher_event() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("open.txt");
        std::fs::write(&file, "old\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_pinned(&file).unwrap();
        app.sync_open_file_poll_mtime();
        app._fs_watcher = None;
        app.fs_rx = None;
        app.fs_watcher_init_rx = None;
        app.fs_poll_last_check = std::time::Instant::now() - FS_POLL_INTERVAL;

        std::fs::write(&file, "new content\n").unwrap();

        assert!(app.drain_fs_events());
        assert_eq!(app.editor.lines, vec!["new content"]);
        assert!(!app.editor.dirty);
    }

    /// Watcher backends on Linux fire `Access(...)` events when croft (or any
    /// other process) reads the open file — the inotify subsystem does not
    /// distinguish a benign read from a real content change. Treating those
    /// as "external change" reloads the editor, which clears `selection` and
    /// kills any in-flight Cmd+A / mouse-drag / Shift+Right gesture. The
    /// reload must only fire for events that mutate content.
    #[test]
    fn drain_fs_events_preserves_editor_selection_on_access_only_event() {
        use notify::event::{AccessKind, AccessMode, EventKind};
        use notify::Event as NotifyEvent;
        use notify_debouncer_full::DebouncedEvent;
        use std::sync::mpsc;

        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("readme.md");
        std::fs::write(&file, "hello world\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_pinned(&file).unwrap();
        app.editor.select_all();
        assert!(
            app.editor.selection.is_some(),
            "precondition: selection set after select_all"
        );

        let (tx, rx) = mpsc::channel();
        app._fs_watcher = None;
        app.fs_watcher_init_rx = None;
        app.fs_rx = Some(rx);
        app.sync_open_file_poll_mtime();

        let access_event = NotifyEvent::new(EventKind::Access(AccessKind::Open(AccessMode::Read)))
            .add_path(file.clone());
        tx.send(Ok(vec![DebouncedEvent::new(
            access_event,
            std::time::Instant::now(),
        )]))
        .unwrap();

        app.drain_fs_events();
        assert!(
            app.editor.selection.is_some(),
            "Access(Read) on the open file must not clobber editor selection"
        );
    }

    /// Same class of spurious wake-up as the Access event, via the other
    /// notify branch: `chmod`, `touch -a`, ownership changes, and xattr
    /// updates surface as `Modify(Metadata(_))`. None mutate file content,
    /// so none should trigger an editor reload that wipes selection.
    #[test]
    fn drain_fs_events_preserves_editor_selection_on_metadata_event() {
        use notify::event::{EventKind, MetadataKind, ModifyKind};
        use notify::Event as NotifyEvent;
        use notify_debouncer_full::DebouncedEvent;
        use std::sync::mpsc;

        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("readme.md");
        std::fs::write(&file, "hello world\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_pinned(&file).unwrap();
        app.editor.select_all();

        let (tx, rx) = mpsc::channel();
        app._fs_watcher = None;
        app.fs_watcher_init_rx = None;
        app.fs_rx = Some(rx);
        app.sync_open_file_poll_mtime();

        let metadata_event = NotifyEvent::new(EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::AccessTime,
        )))
        .add_path(file.clone());
        tx.send(Ok(vec![DebouncedEvent::new(
            metadata_event,
            std::time::Instant::now(),
        )]))
        .unwrap();

        app.drain_fs_events();
        assert!(
            app.editor.selection.is_some(),
            "Modify(Metadata(AccessTime)) on the open file must not clobber editor selection"
        );
    }

    /// Companion to the access-event test: a real content change still has to
    /// trigger the reload, otherwise external edits would never refresh the
    /// buffer. Guards against an over-broad fix that drops legitimate events.
    #[test]
    fn drain_fs_events_reloads_on_modify_data_event() {
        use notify::event::{DataChange, EventKind, ModifyKind};
        use notify::Event as NotifyEvent;
        use notify_debouncer_full::DebouncedEvent;
        use std::sync::mpsc;

        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("readme.md");
        std::fs::write(&file, "old\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_pinned(&file).unwrap();

        let (tx, rx) = mpsc::channel();
        app._fs_watcher = None;
        app.fs_watcher_init_rx = None;
        app.fs_rx = Some(rx);
        app.sync_open_file_poll_mtime();

        std::fs::write(&file, "new content\n").unwrap();
        let modify_event =
            NotifyEvent::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                .add_path(file.clone());
        tx.send(Ok(vec![DebouncedEvent::new(
            modify_event,
            std::time::Instant::now(),
        )]))
        .unwrap();

        app.drain_fs_events();
        assert_eq!(app.editor.lines, vec!["new content"]);
    }

    #[test]
    fn rect_contains_basic() {
        let r = Rect { x: 5, y: 5, width: 10, height: 10 };
        assert!(rect_contains(r, 5, 5));
        assert!(rect_contains(r, 14, 14));
        assert!(!rect_contains(r, 4, 5));
        assert!(!rect_contains(r, 15, 5));
        assert!(!rect_contains(r, 5, 15));
    }

    #[test]
    fn rect_contains_zero_sized_is_empty() {
        let r = Rect { x: 0, y: 0, width: 0, height: 0 };
        assert!(!rect_contains(r, 0, 0));
    }

    fn commit_fixture(subject: &str, when: &str) -> crate::git::CommitInfo {
        crate::git::CommitInfo {
            hash: "abc1234".to_string(),
            full_hash: "abc1234deadbeef".to_string(),
            when: when.to_string(),
            subject: subject.to_string(),
        }
    }

    #[test]
    fn welcome_commit_wrapping_reserves_timestamp_column() {
        let c = commit_fixture(
            "feat(search): clickable paste button in input row reads pbpaste and keeps going",
            "3 hours ago",
        );
        let (first, rest) = welcome_commit_widths(&c, 42);
        assert!(first < rest, "first line should reserve room for the timestamp");
        let lines = wrapped_welcome_commit_subject(&c, 42);
        assert!(lines.len() > 1, "long commit subjects should wrap");
        assert!(lines[0].chars().count() <= first as usize);
        assert!(lines[1].chars().count() <= rest as usize);
    }

    #[test]
    fn welcome_recents_height_accounts_for_wrapped_commit_rows() {
        let c = commit_fixture(
            "feat(search): clickable paste button in input row reads pbpaste and keeps going",
            "3 hours ago",
        );
        let compact = welcome_recents_height(Some("https://bitbucket.org/a/b"), &[c], 42);
        assert!(compact > 1 + 1 + 1, "height must include wrapped commit continuation rows");
    }

    #[test]
    fn welcome_provider_badge_uses_repo_provider() {
        assert!(welcome_provider_badge("https://bitbucket.org/a/b").contains("Bitbucket"));
        assert!(welcome_provider_badge("https://github.com/a/b").contains("GitHub"));
    }

    /// Codeberg's badge must show the literal name without a Nerd Font glyph
    /// in front of it: most installed Nerd Fonts (including the one croft
    /// ships with via `setup-iterm2`) do not have a Codeberg codepoint, so
    /// the previous `\u{ea60}` placeholder rendered as a stray symbol or
    /// tofu box. The actual Codeberg logo is composited as an OSC-1337
    /// image overlay at the badge cell when iTerm2 is detected (handled
    /// elsewhere); the text path stays glyph-free so non-iTerm2 terminals
    /// also display correctly.
    #[test]
    fn welcome_provider_badge_for_codeberg_has_no_unicode_glyph() {
        let badge = welcome_provider_badge("ssh://git@codeberg.org/vitali87/croft.git");
        assert!(badge.contains("Codeberg"), "badge: {badge:?}");
        assert!(
            badge.chars().all(|c| c.is_ascii() || c == ' '),
            "badge must be glyph-free for Codeberg until/unless we render the icon as an image overlay; current badge: {badge:?}"
        );
    }

    /// The Codeberg icon is overlaid as a 2-cell-wide OSC-1337 image at the
    /// badge's anchor; the text after it must start at least one cell later
    /// so the logo doesn't visually butt against the "C". That means three
    /// leading spaces total: two reserved for the icon, one for the gap.
    #[test]
    fn welcome_provider_badge_for_codeberg_leaves_a_gap_between_icon_and_text() {
        let badge = welcome_provider_badge("https://codeberg.org/vitali87/croft");
        assert!(
            badge.starts_with("   Codeberg"),
            "badge must start with three spaces (2 for the icon overlay + 1 visual gap) followed by 'Codeberg'; got: {badge:?}"
        );
    }

    #[test]
    fn lerp_rgb_hits_endpoints_exactly() {
        let a = (10u8, 20, 30);
        let b = (100u8, 200, 250);
        assert_eq!(lerp_rgb(a, b, 0.0), a);
        assert_eq!(lerp_rgb(a, b, 1.0), b);
        let mid = lerp_rgb(a, b, 0.5);
        assert!(mid.0 > a.0 && mid.0 < b.0);
    }

    #[test]
    fn paint_gradient_box_skips_when_rect_runs_off_buffer() {
        // The panic that motivated the bounds clip: an 80x25 buffer asked
        // to render a tall recents box that extended past row 25.
        let buf_area = Rect { x: 0, y: 0, width: 80, height: 25 };
        let mut buf = ratatui::buffer::Buffer::empty(buf_area);
        let oversized = Rect { x: 0, y: 0, width: 80, height: 60 };
        // Must not panic; the no-op clip is the contract.
        paint_gradient_box(&mut buf, oversized);
        let off_top_left = Rect { x: 0, y: 0, width: 80, height: 25 };
        // Sanity: a fitting rect still draws.
        paint_gradient_box(&mut buf, off_top_left);
        assert_eq!(buf[(0, 0)].symbol(), "\u{256d}");
    }

    #[test]
    /// On terminal resize the welcome layout shifts, so
    /// `welcome_codeberg_badge_cell` moves. iTerm2's OSC-1337 image cells
    /// survive plain SGR redraws, so without explicit cleanup the old
    /// logo "ghosts" mid-screen. The flush method must record the last
    /// emit cell, wipe it on move, and clear it when the welcome panel
    /// goes away (file opened, or provider switches off Codeberg).
    #[test]
    fn clicking_an_scm_entry_below_a_stale_tree_last_area_still_selects_and_opens() {
        // User report: after the Source Control list got long enough to
        // reach below where the file tree had last rendered, the rows in
        // that lower band became unclickable. Root cause: the mouse
        // handler gated all sidebar-view click branches on `rect_contains
        // (self.tree.last_area, ...)`, but `tree.last_area` is only
        // refreshed when the Explorer view renders — switching to
        // Source Control freezes that rectangle, and any click below
        // its bottom edge fell through even though it was inside the
        // SCM panel.
        use crate::git::{ChangeEntry, ChangeKind};
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Pretend the Explorer view rendered into a tall pane; then the
        // user opened the terminal which shrank the side area; then they
        // switched to Source Control. Set tree.last_area to the OLD,
        // taller rectangle so we can prove the SCM click-handler is no
        // longer dependent on it.
        app.tree.last_area = Rect { x: 4, y: 1, width: 30, height: 6 };
        app.set_sidebar_view(SidebarView::SourceControl);
        // Set entries AFTER set_sidebar_view because that path calls
        // refresh_source_control which overwrites entries with the live
        // git query (empty in a non-repo temp dir).
        app.source_control.status.in_repo = true;
        app.source_control.status.branch = Some("main".into());
        app.source_control.entries = (0..15)
            .map(|i| ChangeEntry {
                path: format!("file{i}.py"),
                kind: ChangeKind::Untracked,
                ..Default::default()
            })
            .collect();

        let backend = ratatui::backend::TestBackend::new(80, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();

        // Pick the y of a row that lives BELOW tree.last_area's bottom
        // but inside the SCM list. Walk the rendered list area to find
        // one.
        let stale_bottom = app.tree.last_area.y + app.tree.last_area.height;
        let list_area = app.source_control.last_list_area;
        assert!(
            list_area.y + list_area.height > stale_bottom,
            "test setup must put SCM rows below the stale tree.last_area"
        );
        // Walk from stale_bottom down looking for a row that maps to an
        // entry (skip section-header rows).
        let target_row = (stale_bottom..list_area.y + list_area.height)
            .find(|y| app.source_control.entry_at_y(*y).is_some())
            .expect("at least one entry must render below the stale tree band");

        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: list_area.x + 4,
            row: target_row,
            modifiers: KeyModifiers::NONE,
        });

        assert!(
            app.source_control.selected_change.is_some(),
            "click on an SCM entry below the stale tree.last_area must still select it"
        );
    }

    #[test]
    fn click_on_a_deleted_source_control_entry_opens_unified_diff_with_head_lines_all_removed() {
        // A file that's tracked in HEAD but missing from the working
        // tree (or staged for deletion) used to leave the editor empty
        // when clicked, because `open_source_control_entry` fell into
        // the generic open-from-disk branch and the path doesn't exist
        // any more. New behaviour: clicking the row opens a one-pane
        // unified diff that paints every line of the HEAD blob as a
        // removed line — visually identical to `git diff` for a deletion.
        use crate::git::{ChangeEntry, ChangeKind};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(init.success());
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "t@t"])
            .current_dir(root)
            .status();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(root)
            .status();
        let f = root.join("doomed.txt");
        std::fs::write(&f, b"alpha\nbeta\ngamma\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "doomed.txt"])
            .current_dir(root)
            .status();
        let _ = std::process::Command::new("git")
            .args(["commit", "-q", "-m", "seed"])
            .current_dir(root)
            .status();
        // Delete in the working tree but leave it tracked in HEAD/index
        // — that's exactly the `Deleted` (unstaged) state.
        std::fs::remove_file(&f).unwrap();
        let mut app = App::new(root.to_path_buf()).unwrap();
        app.source_control.entries =
            vec![ChangeEntry { path: "doomed.txt".into(), kind: ChangeKind::Deleted, ..Default::default() }];
        app.set_sidebar_view(SidebarView::SourceControl);
        app.open_source_control_entry(0);
        let diff = app
            .editor
            .diff
            .as_ref()
            .expect("clicking a Deleted entry must open a diff view, not a blank tab");
        assert!(
            diff.unified,
            "the deletion view must be the single-column unified flavour, not side-by-side"
        );
        assert_eq!(diff.left_lines, vec!["alpha", "beta", "gamma"]);
        assert!(
            diff.rows
                .iter()
                .all(|r| matches!(r, crate::widgets::diff::DiffRow::Removed { .. })),
            "every visible row must be Removed so the whole pane paints red"
        );
    }

    #[test]
    fn click_on_a_modified_source_control_entry_opens_a_diff_view_against_head() {
        // VS Code surfaces the diff between the working-tree file and
        // the HEAD version when the user clicks a Modified entry in the
        // Source Control panel. Croft used to just open the working
        // file as plain text — assert the new behaviour: the active
        // editor tab carries a `diff: Some(...)` payload.
        use crate::git::{ChangeEntry, ChangeKind};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Init a real repo so git show HEAD:<path> returns content.
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(init.success());
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "t@t"])
            .current_dir(root)
            .status();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(root)
            .status();
        let f = root.join("a.py");
        std::fs::write(&f, b"print(1)\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "a.py"])
            .current_dir(root)
            .status();
        let _ = std::process::Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(root)
            .status();
        std::fs::write(&f, b"print(2)\n").unwrap();
        let mut app = App::new(root.to_path_buf()).unwrap();
        app.source_control.entries =
            vec![ChangeEntry { path: "a.py".into(), kind: ChangeKind::Modified, ..Default::default() }];
        app.set_sidebar_view(SidebarView::SourceControl);
        app.open_source_control_entry(0);
        assert!(
            app.editor.diff.is_some(),
            "clicking a Modified entry must open the editor in diff-vs-HEAD mode, not as plain text"
        );
    }

    #[test]
    fn flush_welcome_codeberg_badge_overlay_tracks_last_emitted_cell_across_resizes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Stub a baked OSC so the flush has something to "emit". The
        // bytes never reach a real terminal in tests — we're checking
        // the bookkeeping, not the wire format.
        app.welcome_codeberg_badge_osc = Some("\x1b]1337;File=...\x07".to_string());

        // Initial render of welcome panel: badge anchored at (10, 5).
        app.welcome_codeberg_badge_cell = Some((10, 5));
        app.flush_welcome_codeberg_badge_overlay();
        assert_eq!(
            app.welcome_codeberg_badge_last_emitted,
            Some((10, 5)),
            "after the first flush the last-emitted cell must match the target cell"
        );

        // Simulate a window resize that shifts the welcome panel right.
        app.welcome_codeberg_badge_cell = Some((20, 8));
        app.flush_welcome_codeberg_badge_overlay();
        assert_eq!(
            app.welcome_codeberg_badge_last_emitted,
            Some((20, 8)),
            "after a resize the last-emitted cell must update to the new target so the next resize knows where to wipe"
        );

        // Repeated flush at the same cell is a no-op for tracking.
        app.flush_welcome_codeberg_badge_overlay();
        assert_eq!(
            app.welcome_codeberg_badge_last_emitted,
            Some((20, 8)),
            "re-emitting at the same cell must not change tracking"
        );

        // User opens a file: welcome panel hides, badge cell cleared,
        // and the next flush must drop the last-emitted record so the
        // image cells are wiped and not re-emitted.
        let file = tmp.path().join("README.md");
        std::fs::write(&file, "hi").unwrap();
        app.editor.open_pinned(&file).unwrap();
        app.welcome_codeberg_badge_cell = None;
        app.flush_welcome_codeberg_badge_overlay();
        assert_eq!(
            app.welcome_codeberg_badge_last_emitted, None,
            "leaving the welcome panel must clear the last-emitted record so a future welcome render starts fresh"
        );
    }

    #[test]
    fn render_welcome_clears_codeberg_badge_cell_when_recents_box_collapses() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.recent_repo_remote = Some("https://codeberg.org/vitali87/croft".to_string());
        app.welcome_codeberg_badge_osc = Some("\x1b]1337;File=...\x07".to_string());

        let tall = Rect { x: 0, y: 0, width: 120, height: 30 };
        let backend = ratatui::backend::TestBackend::new(tall.width, tall.height);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render_welcome(f, tall)).unwrap();
        assert!(
            app.welcome_codeberg_badge_cell.is_some(),
            "with a tall welcome area the recents box must fit and the badge cell must be set so the flush emits the OSC-1337 image"
        );

        let tight = Rect { x: 0, y: 0, width: 120, height: 10 };
        let backend = ratatui::backend::TestBackend::new(tight.width, tight.height);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render_welcome(f, tight)).unwrap();
        assert_eq!(
            app.welcome_codeberg_badge_cell, None,
            "when the recents box can't fit, the badge cell must reset to None so the post-draw flush wipes the stale image instead of re-emitting at the prior anchor"
        );
    }

    /// The user dislikes underlined links in the welcome panel; the blue
    /// foreground colour already signals "clickable", and the underline
    /// adds visual noise (especially under the Codeberg logo overlay).
    /// Assert the rendered buffer contains zero cells with the UNDERLINED
    /// modifier so this preference is enforced by the test suite.
    #[test]
    fn welcome_panel_renders_without_any_underlined_cells() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.recent_repo_remote = Some("https://codeberg.org/vitali87/croft".to_string());
        app.recent_commits = vec![crate::git::CommitInfo {
            hash: "abc1234".to_string(),
            full_hash: "abc1234fffffffffffffffffffffffffffffffffff".to_string(),
            when: "1 hour ago".to_string(),
            subject: "feat: do thing".to_string(),
        }];
        let area = Rect { x: 0, y: 0, width: 120, height: 30 };
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render_welcome(f, area)).unwrap();
        let buffer = term.backend().buffer();
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buffer[(x, y)];
                assert!(
                    !cell.modifier.contains(Modifier::UNDERLINED),
                    "cell at ({x}, {y}) symbol={:?} has UNDERLINED set; the welcome panel must render no underlines",
                    cell.symbol()
                );
            }
        }
    }

    #[test]
    fn welcome_panel_does_not_paint_the_blazingly_fast_slogan_footer() {
        // The user asked to drop the chevron-bracketed
        // "Blazingly fast by design. Secure by default. Loved by developers."
        // line. Render the welcome panel into a buffer and confirm none
        // of the slogan fragments appear anywhere on the canvas.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let area = Rect { x: 0, y: 0, width: 120, height: 40 };
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render_welcome(f, area)).unwrap();
        let buffer = term.backend().buffer();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            for needle in [
                "Blazingly fast by design",
                "Secure by default",
                "Loved by developers",
            ] {
                assert!(
                    !row.contains(needle),
                    "welcome row {y} still carries removed slogan {needle:?}: {row:?}"
                );
            }
        }
    }

    #[test]
    fn render_welcome_does_not_panic_in_default_80x25_with_many_commits() {
        // Repro for the index-(41,70) panic at startup: ratatui's initial
        // backend size is 80x25 before the alt-screen reflow. With a long
        // recents list the previous code computed a box taller than 25,
        // ran past the buffer, and panicked inside set_string.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.recent_repo_remote = Some("https://bitbucket.org/u/repo".to_string());
        app.recent_commits = (0..40)
            .map(|i| crate::git::CommitInfo {
                hash: format!("hash{i:04}"),
                full_hash: format!("fullhash{i:040}"),
                when: "1 hour ago".to_string(),
                subject:
                    "this is a long subject line that will wrap to multiple lines on a narrow recents column"
                        .to_string(),
            })
            .collect();
        let area = Rect { x: 0, y: 0, width: 80, height: 25 };
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render_welcome(f, area)).unwrap();
    }

    #[test]
    fn paint_gradient_box_draws_rounded_corners_with_corner_colours() {
        let rect = Rect { x: 0, y: 0, width: 8, height: 4 };
        let mut buf = ratatui::buffer::Buffer::empty(rect);
        paint_gradient_box(&mut buf, rect);
        assert_eq!(buf[(0, 0)].symbol(), "\u{256d}", "top-left rounded");
        assert_eq!(buf[(7, 0)].symbol(), "\u{256e}", "top-right rounded");
        assert_eq!(buf[(0, 3)].symbol(), "\u{2570}", "bottom-left rounded");
        assert_eq!(buf[(7, 3)].symbol(), "\u{256f}", "bottom-right rounded");
        assert_eq!(buf[(3, 0)].symbol(), "\u{2500}", "top edge horizontal");
        assert_eq!(buf[(0, 1)].symbol(), "\u{2502}", "left edge vertical");
        // Top-left cell carries the GRAD_TL fg colour exactly.
        let tl_fg = buf[(0, 0)].fg;
        assert_eq!(tl_fg, rgb_color(GRAD_TL));
        let tr_fg = buf[(7, 0)].fg;
        assert_eq!(tr_fg, rgb_color(GRAD_TR));
    }

    #[test]
    fn welcome_tagline_constant_is_present() {
        assert!(WELCOME_TAGLINE.contains("LIGHTWEIGHT"));
        assert!(WELCOME_TAGLINE.contains("BLAZINGLY FAST"));
        assert!(WELCOME_TAGLINE.contains("DEVELOPERS"));
    }

    #[test]
    fn key_to_bytes_arrows() {
        assert_eq!(key_to_bytes(key(KeyCode::Up, KeyModifiers::NONE)), b"\x1b[A");
        assert_eq!(key_to_bytes(key(KeyCode::Down, KeyModifiers::NONE)), b"\x1b[B");
        assert_eq!(key_to_bytes(key(KeyCode::Right, KeyModifiers::NONE)), b"\x1b[C");
        assert_eq!(key_to_bytes(key(KeyCode::Left, KeyModifiers::NONE)), b"\x1b[D");
    }

    #[test]
    fn key_to_bytes_enter_tab_backspace() {
        assert_eq!(key_to_bytes(key(KeyCode::Enter, KeyModifiers::NONE)), b"\r");
        assert_eq!(key_to_bytes(key(KeyCode::Tab, KeyModifiers::NONE)), b"\t");
        assert_eq!(key_to_bytes(key(KeyCode::Backspace, KeyModifiers::NONE)), &[0x7f]);
    }

    #[test]
    fn key_to_bytes_ctrl_letter_maps_to_control_byte() {
        let bytes = key_to_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(bytes, vec![0x03]);
        let bytes = key_to_bytes(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(bytes, vec![0x01]);
        let bytes = key_to_bytes(key(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(bytes, vec![0x1a]);
    }

    #[test]
    fn key_to_bytes_alt_letter_prefixes_esc() {
        let bytes = key_to_bytes(key(KeyCode::Char('x'), KeyModifiers::ALT));
        assert_eq!(bytes, vec![0x1b, b'x']);
    }

    #[test]
    fn key_to_bytes_alt_left_emits_esc_b_for_readline_backward_word() {
        let bytes = key_to_bytes(key(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(
            bytes, b"\x1bb",
            "Option+Left in the terminal must send ESC b so zsh/bash readline runs backward-word"
        );
    }

    #[test]
    fn key_to_bytes_alt_right_emits_esc_f_for_readline_forward_word() {
        let bytes = key_to_bytes(key(KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(
            bytes, b"\x1bf",
            "Option+Right in the terminal must send ESC f so zsh/bash readline runs forward-word"
        );
    }

    #[test]
    fn key_to_bytes_plain_arrows_unchanged_when_alt_not_held() {
        assert_eq!(key_to_bytes(key(KeyCode::Left, KeyModifiers::NONE)), b"\x1b[D");
        assert_eq!(key_to_bytes(key(KeyCode::Right, KeyModifiers::NONE)), b"\x1b[C");
    }

    #[test]
    fn key_to_bytes_plain_char_utf8() {
        assert_eq!(key_to_bytes(key(KeyCode::Char('a'), KeyModifiers::NONE)), b"a");
        assert_eq!(
            key_to_bytes(key(KeyCode::Char('é'), KeyModifiers::NONE)),
            "é".as_bytes()
        );
    }

    #[test]
    fn key_to_bytes_function_keys() {
        assert_eq!(key_to_bytes(key(KeyCode::F(1), KeyModifiers::NONE)), b"\x1bOP");
        assert_eq!(key_to_bytes(key(KeyCode::F(5), KeyModifiers::NONE)), b"\x1b[15~");
        assert_eq!(key_to_bytes(key(KeyCode::F(12), KeyModifiers::NONE)), b"\x1b[24~");
    }

    #[test]
    fn key_to_bytes_unknown_returns_empty() {
        assert!(key_to_bytes(key(KeyCode::CapsLock, KeyModifiers::NONE)).is_empty());
    }

    #[test]
    fn ctrl_s_is_save_key() {
        assert!(is_save_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn cmd_s_is_save_key() {
        assert!(is_save_key(key(KeyCode::Char('s'), KeyModifiers::SUPER)));
    }

    #[test]
    fn shift_ctrl_s_is_save_key() {
        // Some terminals report capital S with Ctrl pressed.
        let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(is_save_key(key(KeyCode::Char('S'), mods)));
    }

    #[test]
    fn plain_s_is_not_save_key() {
        assert!(!is_save_key(key(KeyCode::Char('s'), KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_q_is_not_save_key() {
        assert!(!is_save_key(key(KeyCode::Char('q'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn alt_s_is_not_save_key() {
        assert!(!is_save_key(key(KeyCode::Char('s'), KeyModifiers::ALT)));
    }

    #[test]
    fn git_status_spans_empty_when_not_in_repo() {
        let st = crate::git::GitStatus::default();
        let spans = git_status_spans(&st);
        assert!(spans.is_empty(), "no git pill outside a git repo");
    }

    #[test]
    fn git_status_spans_clean_branch_is_green() {
        // Agnoster convention: clean working tree → green pill.
        let st = crate::git::GitStatus {
            in_repo: true,
            branch: Some("main".into()),
            detached_hash: None,
            dirty: false,
            ahead: 0,
            behind: 0,
        };
        let spans = git_status_spans(&st);
        let main_span = spans
            .iter()
            .find(|s| s.content.contains("main"))
            .expect("branch span");
        assert_eq!(main_span.style.fg, Some(GIT_CLEAN_COLOR));
    }

    #[test]
    fn git_status_spans_dirty_branch_is_yellow_not_red() {
        // Agnoster convention: dirty working tree → yellow/orange pill.
        // No red bullet either — colour alone carries the state.
        let st = crate::git::GitStatus {
            in_repo: true,
            branch: Some("main".into()),
            detached_hash: None,
            dirty: true,
            ahead: 0,
            behind: 0,
        };
        let spans = git_status_spans(&st);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!joined.contains('●'), "no red bullet — colour signals dirtiness");
        let main_span = spans
            .iter()
            .find(|s| s.content.contains("main"))
            .expect("branch span");
        assert_eq!(main_span.style.fg, Some(GIT_DIRTY_COLOR));
    }

    #[test]
    fn git_status_spans_renders_detached_hash_when_no_branch() {
        let st = crate::git::GitStatus {
            in_repo: true,
            branch: None,
            detached_hash: Some("abc1234".into()),
            dirty: false,
            ahead: 0,
            behind: 0,
        };
        let spans = git_status_spans(&st);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(joined.contains("abc1234"));
    }

    #[test]
    fn git_status_spans_renders_ahead_behind_counts() {
        let st = crate::git::GitStatus {
            in_repo: true,
            branch: Some("main".into()),
            detached_hash: None,
            dirty: false,
            ahead: 2,
            behind: 1,
        };
        let spans = git_status_spans(&st);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(joined.contains("↑2"));
        assert!(joined.contains("↓1"));
    }

    #[test]
    fn ctrl_shift_f_jumps_to_search() {
        let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(is_search_jump_key(key(KeyCode::Char('F'), mods)));
    }

    #[test]
    fn cmd_shift_f_jumps_to_search() {
        let mods = KeyModifiers::SUPER | KeyModifiers::SHIFT;
        assert!(is_search_jump_key(key(KeyCode::Char('F'), mods)));
    }

    #[test]
    fn plain_f_does_not_jump_to_search() {
        assert!(!is_search_jump_key(key(KeyCode::Char('f'), KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_c_is_editor_copy_key() {
        assert!(is_editor_copy_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn cmd_c_is_editor_copy_key() {
        assert!(is_editor_copy_key(key(KeyCode::Char('c'), KeyModifiers::SUPER)));
    }

    #[test]
    fn plain_c_is_not_editor_copy_key() {
        assert!(!is_editor_copy_key(key(KeyCode::Char('c'), KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_x_is_editor_cut_key() {
        assert!(is_editor_cut_key(key(KeyCode::Char('x'), KeyModifiers::CONTROL)));
        assert!(is_editor_cut_key(key(KeyCode::Char('x'), KeyModifiers::SUPER)));
        assert!(!is_editor_cut_key(key(KeyCode::Char('x'), KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_a_is_editor_select_all_key() {
        assert!(is_editor_select_all_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL)));
        assert!(is_editor_select_all_key(key(KeyCode::Char('a'), KeyModifiers::SUPER)));
        assert!(!is_editor_select_all_key(key(KeyCode::Char('a'), KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_z_is_editor_undo_key() {
        assert!(is_editor_undo_key(key(KeyCode::Char('z'), KeyModifiers::CONTROL)));
        assert!(is_editor_undo_key(key(KeyCode::Char('z'), KeyModifiers::SUPER)));
    }

    #[test]
    fn plain_z_is_not_editor_undo_key() {
        assert!(!is_editor_undo_key(key(KeyCode::Char('z'), KeyModifiers::NONE)));
    }

    #[test]
    fn shift_cmd_z_reserved_for_redo_not_undo() {
        let mods = KeyModifiers::SUPER | KeyModifiers::SHIFT;
        assert!(!is_editor_undo_key(key(KeyCode::Char('z'), mods)));
    }

    #[test]
    fn ctrl_shift_c_is_recognized_as_terminal_copy() {
        let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(is_terminal_copy_key(key(KeyCode::Char('C'), mods)));
    }

    #[test]
    fn cmd_c_is_recognized_as_terminal_copy() {
        // For terminals that pass Cmd via the kitty protocol; iTerm2 users
        // can also remap ⌘C → Send Hex Code 0x03 for terminal-pane copies,
        // but for keyboard-protocol-aware terminals we accept Super here.
        assert!(is_terminal_copy_key(key(KeyCode::Char('c'), KeyModifiers::SUPER)));
    }

    #[test]
    fn plain_ctrl_c_is_not_terminal_copy() {
        // Ctrl+C must remain SIGINT in the embedded shell; it must not be
        // intercepted as the croft copy gesture.
        assert!(!is_terminal_copy_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    }

    /// Cmd+C from the croft terminal pane must land the selection on the
    /// real macOS clipboard. This is the user-visible regression test
    /// for the long-standing "I copied from croft and pbpaste shows the
    /// previous value" bug, where the old OSC 52 path was silently
    /// dropped by the host terminal. The contract: writing through
    /// `copy_to_clipboard` puts the text on the system pasteboard such
    /// that `clipboard::read_string` (which is what Cmd+V uses) returns
    /// the same bytes. If this ever fails again, copy/paste between
    /// croft and other apps is broken and the test must catch it before
    /// the binary ships.
    #[cfg(target_os = "macos")]
    #[test]
    fn copy_to_clipboard_writes_text_to_the_macos_system_pasteboard() {
        let _g = crate::clipboard::lock_clipboard_for_test();
        let sentinel = format!(
            "croft-copy-helper-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let used_native = copy_to_clipboard(&sentinel);
        assert!(
            used_native,
            "on macOS, copy_to_clipboard must succeed via NSPasteboard and return true so the call site can skip the OSC 52 fallback (and the host terminal's quirks can't silently drop the write)"
        );
        let got = crate::clipboard::read_string()
            .expect("read_string must see what we just wrote");
        assert_eq!(
            got, sentinel,
            "round-trip through the system pasteboard must be byte-exact - if this ever drifts, every Cmd+C from croft is silently lossy"
        );
    }

    /// Tighter regression: a real `App` with a real `PtyTerminal`, a
    /// terminal-pane selection, and `App::copy_terminal_selection` must
    /// land the text on the macOS system clipboard. Exercises the full
    /// wire from PTY bytes → alacritty grid → `selection_text` →
    /// `copy_to_clipboard` → `NSPasteboard.setString:forType:` → what
    /// `pbpaste` / `NSPasteboard.stringForType:` returns. A refactor
    /// that reintroduces an OSC-52-only path in the terminal copy
    /// handler would fail this test even if `copy_to_clipboard` itself
    /// still resolved to the native path elsewhere.
    #[cfg(target_os = "macos")]
    #[test]
    fn cmd_c_on_terminal_selection_lands_text_on_macos_clipboard() {
        let _g = crate::clipboard::lock_clipboard_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();

        // Render once so `terminal.last_inner` is populated and the
        // `start_selection_at` / `extend_selection_to` chord can map
        // viewport coordinates to cell coordinates the way the live
        // input handler does on a real keystroke.
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        app.terminal_mut().focused = true;
        term.draw(|f| app.render(f)).unwrap();

        let probe = format!("croft-terminal-cmdc-{}", std::process::id());
        app.terminal_mut()
            .write_input(format!("printf '{probe}\\n'\r").as_bytes());

        // Wait for the shell to push bytes through the PTY and the
        // background reader thread to drain them into the grid.
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_millis(3000) {
            if app.terminal().visible_text().contains(&probe) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // Re-render to refresh `last_inner` after the grid has grown.
        term.draw(|f| app.render(f)).unwrap();

        let snapshot = app.terminal().visible_text();
        assert!(
            snapshot.contains(&probe),
            "shell must echo the probe within 3s; got:\n{snapshot}"
        );

        let row_idx = snapshot
            .lines()
            .position(|l| l.contains(&probe))
            .expect("probe row must exist");
        let line = snapshot.lines().nth(row_idx).unwrap();
        let col_start = line.find(&probe).unwrap();
        let col_end = col_start + probe.len() - 1;

        let inner = app.terminal().last_inner;
        let screen_y = inner.y + row_idx as u16;
        let screen_x_a = inner.x + col_start as u16;
        let screen_x_b = inner.x + col_end as u16;
        app.terminal_mut().start_selection_at(screen_x_a, screen_y);
        app.terminal_mut().extend_selection_to(screen_x_b, screen_y);
        assert_eq!(
            app.terminal().selection_text(),
            probe,
            "selection_text must reproduce the probe exactly so a Cmd+C copy is byte-faithful"
        );

        app.copy_terminal_selection();
        let got = crate::clipboard::read_string()
            .expect("read_string must surface what copy_terminal_selection just wrote");
        assert_eq!(
            got, probe,
            "after a Cmd+C on a terminal selection, the system clipboard must contain the exact selection text - this is the non-negotiable round-trip the user relies on for every copy out of croft"
        );
    }

    #[test]
    fn cmd_c_on_side_by_side_diff_selection_lands_text_on_macos_clipboard() {
        let _g = crate::clipboard::lock_clipboard_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let f1 = tmp.path().join("a.txt");
        let f2 = tmp.path().join("b.txt");
        std::fs::write(&f1, "alpha\nbravo\ncharlie\n").unwrap();
        std::fs::write(&f2, "alpha\nBRAVO\ncharlie\n").unwrap();
        app.editor.open_diff(&f1, &f2).unwrap();
        app.focus = Pane::Editor;
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();

        let diff = app.editor.diff.as_ref().unwrap();
        let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
        let l_text_x = app.editor.last_inner.x + l_gutter + 2;
        let body_top = app.editor.last_inner.y + 1;
        // Row 1 in the diff (after the @@/header rows) holds "bravo" on the
        // left. Click at col 0, drag to col 5 to select all five chars.
        let click_y = body_top + 1;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
            column: l_text_x,
            row: click_y,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(MouseButton::Left),
            column: l_text_x + 5,
            row: click_y,
            modifiers: KeyModifiers::NONE,
        });
        let sel = app
            .editor
            .diff
            .as_ref()
            .unwrap()
            .selection
            .expect("drag must have produced a diff selection");
        assert!(sel.has_area(), "drag must produce a non-zero-area selection");

        // Drive the actual Cmd+C keystroke through handle_key. This is the
        // path the live app takes when the user presses Cmd+C; the
        // editor's main key handler short-circuits to handle_diff_key when
        // a diff is open, so a copy keybinding must be wired explicitly
        // INSIDE handle_diff_key. Without that branch, this assertion
        // fails — which is exactly the regression the user just reported:
        // selecting text in a diff and pressing Cmd+C did nothing,
        // leaving stale test sentinels on the system clipboard for the
        // next paste to surface.
        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::SUPER));
        let got = crate::clipboard::read_string()
            .expect("Cmd+C on a diff selection must put text on the clipboard");
        assert_eq!(
            got, "bravo",
            "selecting cols 0..5 of the left column of row 1 and pressing Cmd+C must copy the exact left-side text via handle_diff_key's copy branch"
        );
    }

    #[test]
    fn double_click_in_diff_selects_word_and_cmd_c_copies_it() {
        // End-to-end: two left-clicks in quick succession on a word in
        // the left column must snap a word-bounded selection (same UX
        // as VS Code / standard text editors), and the very next Cmd+C
        // keystroke must put exactly that word on the clipboard.
        let _g = crate::clipboard::lock_clipboard_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let f1 = tmp.path().join("a.txt");
        let f2 = tmp.path().join("b.txt");
        std::fs::write(&f1, "alpha bravo charlie\nsecond\n").unwrap();
        std::fs::write(&f2, "alpha BRAVO charlie\nsecond\n").unwrap();
        app.editor.open_diff(&f1, &f2).unwrap();
        app.focus = Pane::Editor;
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();

        let diff = app.editor.diff.as_ref().unwrap();
        let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
        let l_text_x = app.editor.last_inner.x + l_gutter + 2;
        let body_top = app.editor.last_inner.y + 1;
        // "alpha bravo charlie" — "bravo" starts at char col 6.
        let bravo_x = l_text_x + 8;
        let click_y = body_top;
        let first = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
            column: bravo_x,
            row: click_y,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(first);
        let second = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
            column: bravo_x,
            row: click_y,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(second);
        let sel = app
            .editor
            .diff
            .as_ref()
            .unwrap()
            .selection
            .expect("double-click must produce a word selection");
        assert!(sel.has_area(), "word selection must span >0 chars");

        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::SUPER));
        let got = crate::clipboard::read_string()
            .expect("Cmd+C after double-click must put the word on the clipboard");
        assert_eq!(
            got, "bravo",
            "double-click word selection + Cmd+C must copy exactly the clicked word"
        );
    }

    #[test]
    fn lock_clipboard_for_test_restores_prior_clipboard_text_on_drop() {
        // Tests that hit the real NSPasteboard must never leave their
        // sentinels behind. This test simulates a user already having
        // text on the clipboard, runs a guarded section that writes a
        // sentinel, drops the guard, and asserts the clipboard now
        // reads back the simulated "user" text rather than the test
        // sentinel.
        //
        // To avoid leaking the simulated text into the developer's real
        // clipboard, capture the genuine pre-test contents first and
        // restore them at the very end via a Drop guard so even a panic
        // in the middle leaves the host clipboard untouched.
        struct RestoreClip(Option<String>);
        impl Drop for RestoreClip {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(s) => {
                        crate::clipboard::macos::write_string_native(&s);
                    }
                    None => crate::clipboard::macos::clear_native(),
                }
            }
        }
        let _restore = RestoreClip(crate::clipboard::macos::read_string_native());

        let pre = format!("user-preexisting-clip-{}", std::process::id());
        crate::clipboard::macos::write_string_native(&pre);
        {
            let _g = crate::clipboard::lock_clipboard_for_test();
            let sentinel = "test-sentinel-should-not-leak";
            assert!(
                crate::clipboard::write_string(sentinel),
                "guard-held write must hit the real NSPasteboard"
            );
            assert_eq!(
                crate::clipboard::read_string().as_deref(),
                Some(sentinel),
                "while the guard is held the test reads what it just wrote"
            );
        }
        let after = crate::clipboard::macos::read_string_native();
        assert_eq!(
            after.as_deref(),
            Some(pre.as_str()),
            "dropping the guard MUST restore the pre-test clipboard so a cargo test run never leaves a test sentinel on the developer's system clipboard for the next paste to surface"
        );
    }

    #[test]
    fn ctrl_j_is_recognized_as_terminal_toggle() {
        assert!(is_terminal_toggle_key(key(KeyCode::Char('j'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn ctrl_shift_j_is_not_terminal_toggle_anymore() {
        // Ctrl+Shift+J is now reserved for the terminal-maximize chord, so
        // the terminal-toggle matcher must reject it. Ctrl+J alone (with no
        // shift) still toggles.
        let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(!is_terminal_toggle_key(key(KeyCode::Char('J'), mods)));
        assert!(!is_terminal_toggle_key(key(KeyCode::Char('j'), mods)));
    }

    #[test]
    fn plain_j_is_not_terminal_toggle() {
        assert!(!is_terminal_toggle_key(key(KeyCode::Char('j'), KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_shift_j_is_terminal_maximize_key() {
        let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(is_terminal_maximize_key(key(KeyCode::Char('J'), mods)));
        assert!(is_terminal_maximize_key(key(KeyCode::Char('j'), mods)));
    }

    #[test]
    fn plain_ctrl_j_is_not_terminal_maximize_key() {
        assert!(!is_terminal_maximize_key(key(KeyCode::Char('j'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn plain_shift_j_is_not_terminal_maximize_key() {
        assert!(!is_terminal_maximize_key(key(KeyCode::Char('J'), KeyModifiers::SHIFT)));
    }

    #[test]
    fn alt_or_super_blocks_terminal_maximize_key() {
        let with_alt = KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::ALT;
        let with_super = KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::SUPER;
        assert!(!is_terminal_maximize_key(key(KeyCode::Char('J'), with_alt)));
        assert!(!is_terminal_maximize_key(key(KeyCode::Char('J'), with_super)));
    }

    #[test]
    fn ctrl_shift_j_toggles_terminal_maximize_state_via_handle_key() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        assert!(!app.terminal_maximized, "fresh app starts un-maximized");
        let chord = key(
            KeyCode::Char('J'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        app.handle_key(chord).unwrap();
        assert!(app.terminal_maximized, "first chord maximizes");
        assert!(app.show_terminal, "maximize forces terminal visible");
        assert!(
            matches!(app.focus, Pane::Terminal),
            "maximize focuses the terminal"
        );
        app.handle_key(chord).unwrap();
        assert!(!app.terminal_maximized, "second chord restores split");
    }

    /// User-visible regression: with the terminal maximised via
    /// Ctrl+Shift+J, a mouse drag inside the (now fullscreen) terminal
    /// pane must reach `PtyTerminal::extend_selection_to` so the user
    /// can highlight terminal text. Before this fix `EditorTabs::render`
    /// returned early on `area.height == 0` without zeroing the active
    /// editor's `last_area`, so `handle_mouse`'s `in_editor` hit-test
    /// (`rect_contains(self.editor.last_area, ...)`) still matched the
    /// pre-maximise rectangle and the dispatch at app.rs:7229 / 7336
    /// routed the click into `editor.mouse_down` / `editor.mouse_drag`.
    /// The user saw the file caret jump instead of a selection
    /// appearing in the terminal — symptom: "the cursor is jumping up
    /// and down as I go back and forth."
    #[test]
    fn maximised_terminal_owns_mouse_drag_for_selection_not_the_collapsed_editor() {
        let tmp = tempfile::tempdir().unwrap();
        // Seed a real file and open it so the editor actually claims a
        // non-empty rectangle on the first render (the blank-initial
        // branch zeroes `last_area.height` to skip welcome-pane
        // overdraw, which would mask the bug we're probing).
        let f = tmp.path().join("probe.txt");
        std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_pinned(&f).unwrap();

        // Render once in split layout so the editor's `last_area` ends
        // up pointing at the right-hand pane — the exact stale
        // rectangle the bug would later read from.
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let editor_area_before_maximise = app.editor.last_area;
        assert!(
            editor_area_before_maximise.width > 0
                && editor_area_before_maximise.height > 0,
            "precondition: in split layout with an open file the editor must claim a real rectangle, otherwise the bug we're testing for can't manifest"
        );

        // Trigger the maximize chord.
        let chord = key(
            KeyCode::Char('J'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        app.handle_key(chord).unwrap();
        assert!(app.terminal_maximized);

        // Re-render in the maximised layout — this is the frame that
        // used to leave `editor.last_area` stale.
        term.draw(|f| app.render(f)).unwrap();
        assert_eq!(
            app.editor.last_area.height, 0,
            "after maximise, the editor's hit-test rectangle must have height 0 so `in_editor` cannot match a click that lands on the maximised terminal"
        );

        // Drive a real Left-Drag through the production key/mouse
        // dispatch: anywhere inside the maximised terminal pane. With
        // the fix in place the drag must end up extending the terminal
        // selection, not poking the editor caret.
        let term_area = app.terminal().last_area;
        assert!(
            term_area.width > 4 && term_area.height > 4,
            "precondition: the maximised terminal must own a real rect for the drag to land in"
        );
        let click_col = term_area.x + 2;
        let click_row = term_area.y + 2;
        let drag_col = click_col + 5;
        let drag_row = click_row;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: click_col,
            row: click_row,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: drag_col,
            row: drag_row,
            modifiers: KeyModifiers::NONE,
        });
        let sel = app.terminal().selection();
        assert!(
            sel.is_some_and(|s| s.has_area()),
            "drag inside the maximised terminal must seed a non-empty selection on the PtyTerminal - if this is None or zero-area the click was eaten by the stale editor hit-test"
        );
    }

    /// Cross-pane paste matrix. In production, Cmd+V on macOS does NOT
    /// arrive as a key event — `setup-iterm2` deliberately leaves Cmd+V
    /// unbound (src/iterm2.rs:225-235) so iTerm2's native Paste action
    /// fires and emits a bracketed-paste sequence carrying the OS
    /// clipboard into the PTY. crossterm surfaces that as
    /// `Event::Paste(s)`, dispatched by `main_loop` to
    /// `App::handle_paste(&s)` (src/app.rs:17131).
    ///
    /// This matrix therefore drives `handle_paste` directly with the
    /// payload that iTerm2 would have delivered. Every destination
    /// (editor, editor-find, search query, file-finder query,
    /// terminal, source-control commit box) must consume the payload
    /// in BOTH terminal layouts (split + maximised). If a case
    /// silently fails here, that's the exact production symptom the
    /// user described: "I copied from one place, pasted in another,
    /// nothing happened."
    #[test]
    fn cross_pane_paste_via_bracketed_paste_works_in_every_destination_and_terminal_layout() {
        use crossterm::event::{KeyCode, KeyModifiers};

        fn seed_app() -> (tempfile::TempDir, App) {
            let tmp = tempfile::tempdir().unwrap();
            let f = tmp.path().join("probe.txt");
            std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
            let mut app = App::new(tmp.path().to_path_buf()).unwrap();
            app.editor.open_pinned(&f).unwrap();
            (tmp, app)
        }

        fn render(app: &mut App) -> ratatui::Terminal<ratatui::backend::TestBackend> {
            let backend = ratatui::backend::TestBackend::new(140, 50);
            let mut term = ratatui::Terminal::new(backend).unwrap();
            term.draw(|f| app.render(f)).unwrap();
            term
        }

        // Drive a bracketed paste from a non-path token, which is what
        // iTerm2 emits when the user presses Cmd+V with terminal text
        // on the clipboard. Non-path so the drop-handler branches in
        // handle_paste don't divert us into "import into Explorer / scp
        // to Remote / fetch via drop-relay".
        let payload_editor_split = "clip-into-editor-split";
        let payload_editor_max = "clip-into-editor-maximised";
        let payload_find_split = "clip-into-find-split";
        let payload_find_max = "clip-into-find-maximised";
        let payload_search_split = "clip-into-search-split";
        let payload_search_max = "clip-into-search-maximised";
        let payload_finder = "clip-into-file-finder";
        let payload_sc = "clip-into-source-control";

        // -- Case 1: paste into the editor while terminal is split.
        {
            let (_tmp, mut app) = seed_app();
            let _draw = render(&mut app);
            app.focus_pane(Pane::Editor);
            app.handle_paste(payload_editor_split);
            assert!(
                app.editor.lines.iter().any(|l| l.contains(payload_editor_split)),
                "bracketed paste with editor focus + terminal split must insert into the editor buffer; lines={:?}",
                app.editor.lines
            );
        }

        // -- Case 2: paste into the editor while terminal is maximised.
        {
            let (_tmp, mut app) = seed_app();
            let _draw = render(&mut app);
            app.toggle_terminal_maximize();
            let _draw = render(&mut app);
            app.focus_pane(Pane::Editor);
            app.handle_paste(payload_editor_max);
            assert!(
                app.editor.lines.iter().any(|l| l.contains(payload_editor_max)),
                "bracketed paste with editor focus + terminal maximised must still insert into the editor; lines={:?}",
                app.editor.lines
            );
        }

        // -- Case 3: paste into the editor-find bar while terminal is
        //    split. The user explicitly named this as broken.
        {
            let (_tmp, mut app) = seed_app();
            let _draw = render(&mut app);
            app.focus_pane(Pane::Editor);
            app.open_editor_find();
            app.handle_paste(payload_find_split);
            let q = app
                .editor_find
                .as_ref()
                .map(|s| s.query.clone())
                .unwrap_or_default();
            assert!(
                q.contains(payload_find_split),
                "bracketed paste with editor-find open + terminal split must append to the query; query={q:?}"
            );
        }

        // -- Case 4: paste into the editor-find bar while terminal is
        //    maximised.
        {
            let (_tmp, mut app) = seed_app();
            let _draw = render(&mut app);
            app.toggle_terminal_maximize();
            let _draw = render(&mut app);
            app.focus_pane(Pane::Editor);
            app.open_editor_find();
            app.handle_paste(payload_find_max);
            let q = app
                .editor_find
                .as_ref()
                .map(|s| s.query.clone())
                .unwrap_or_default();
            assert!(
                q.contains(payload_find_max),
                "bracketed paste with editor-find open + terminal maximised must append to the query - this is the path the user named explicitly; query={q:?}"
            );
        }

        // -- Case 5: paste into the Search sidebar query while
        //    terminal is split.
        {
            let (_tmp, mut app) = seed_app();
            let _draw = render(&mut app);
            app.set_sidebar_view(SidebarView::Search);
            let _draw = render(&mut app);
            app.handle_paste(payload_search_split);
            assert!(
                app.search.query.contains(payload_search_split),
                "bracketed paste in Search sidebar + terminal split must append to the search query; query={:?}",
                app.search.query
            );
        }

        // -- Case 6: paste into Search sidebar while terminal is
        //    maximised. The user explicitly named the split layout as
        //    failing while maximised worked; both must succeed.
        {
            let (_tmp, mut app) = seed_app();
            let _draw = render(&mut app);
            app.toggle_terminal_maximize();
            let _draw = render(&mut app);
            app.set_sidebar_view(SidebarView::Search);
            let _draw = render(&mut app);
            app.handle_paste(payload_search_max);
            assert!(
                app.search.query.contains(payload_search_max),
                "bracketed paste in Search sidebar + terminal maximised must also append; query={:?}",
                app.search.query
            );
        }

        // -- Case 7: paste into the embedded terminal while it is
        //    split. The terminal pane consumes the bracketed paste by
        //    writing the bytes back into the PTY.
        {
            let (_tmp, mut app) = seed_app();
            let _draw = render(&mut app);
            app.focus_pane(Pane::Terminal);
            // No panic + handler completes is enough. Verifying the
            // PTY received the bytes requires polling the shell which
            // is too timing-fragile for CI.
            app.handle_paste("clip-into-terminal-split");
        }

        // -- Case 8: paste into the embedded terminal while
        //    maximised.
        {
            let (_tmp, mut app) = seed_app();
            let _draw = render(&mut app);
            app.toggle_terminal_maximize();
            let _draw = render(&mut app);
            app.focus_pane(Pane::Terminal);
            app.handle_paste("clip-into-terminal-maximised");
        }

        // -- Case 9: paste into the Source Control commit message
        //    box while terminal split. NOTE: at the time of writing,
        //    handle_paste has no SourceControl branch — this case
        //    will FAIL until handle_paste learns to route into the
        //    commit message when sidebar_view == SourceControl.
        {
            let (_tmp, mut app) = seed_app();
            let _draw = render(&mut app);
            app.set_sidebar_view(SidebarView::SourceControl);
            let _draw = render(&mut app);
            app.handle_paste(payload_sc);
            assert!(
                app.source_control.message.contains(payload_sc),
                "bracketed paste in the Source Control commit box must append the clipboard; got: {:?}",
                app.source_control.message
            );
        }

        // -- Case 10: paste into the file-finder (Cmd+P quick-open).
        {
            let (_tmp, mut app) = seed_app();
            let _draw = render(&mut app);
            app.open_file_finder();
            app.handle_paste(payload_finder);
            let q = app
                .file_finder
                .as_ref()
                .map(|f| f.query.clone())
                .unwrap_or_default();
            assert!(
                q.contains(payload_finder),
                "bracketed paste in the file finder (Cmd+P) must append to the query; query={q:?}"
            );
        }
    }

    /// Double-clicking a word in the terminal pane must word-select
    /// in BOTH terminal layouts. The user reported that double-click
    /// "doesn't auto-select" in some state. The detection logic in
    /// `handle_mouse` keys off `last_terminal_left_down` + window;
    /// the test sends two consecutive Down events on the same cell
    /// within DOUBLE_CLICK_WINDOW and asserts a non-empty word
    /// selection appears.
    #[test]
    fn double_click_in_terminal_word_selects_in_split_and_maximised_layout() {
        for maximise in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let mut app = App::new(tmp.path().to_path_buf()).unwrap();
            let backend = ratatui::backend::TestBackend::new(140, 50);
            let mut term = ratatui::Terminal::new(backend).unwrap();
            term.draw(|f| app.render(f)).unwrap();
            if maximise {
                app.toggle_terminal_maximize();
                term.draw(|f| app.render(f)).unwrap();
            }
            app.focus_pane(Pane::Terminal);
            let probe = format!("croft-dclick-{}", std::process::id());
            app.terminal_mut()
                .write_input(format!("printf '{probe}\\n'\r").as_bytes());
            let started = std::time::Instant::now();
            while started.elapsed() < std::time::Duration::from_millis(3000) {
                if app.terminal().visible_text().contains(&probe) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            term.draw(|f| app.render(f)).unwrap();
            let snap = app.terminal().visible_text();
            let row_idx = snap
                .lines()
                .position(|l| l.contains(&probe))
                .expect("probe must render");
            let line = snap.lines().nth(row_idx).unwrap();
            let mid_col = line.find(&probe).unwrap() + probe.len() / 2;
            let inner = app.terminal().last_inner;
            let screen_y = inner.y + row_idx as u16;
            let screen_x = inner.x + mid_col as u16;

            // Two clicks within the DOUBLE_CLICK_WINDOW on the same
            // cell — same path a real macOS double-click takes.
            for _ in 0..2 {
                app.handle_mouse(crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: screen_x,
                    row: screen_y,
                    modifiers: KeyModifiers::NONE,
                });
            }

            let sel_text = app.terminal().selection_text();
            assert!(
                sel_text.contains(&probe) || probe.contains(&sel_text),
                "double-click on the middle of the probe word must word-select the run that contains it in {} layout; got selection={sel_text:?}",
                if maximise { "maximised" } else { "split" }
            );
        }
    }

    /// End-to-end variant of the matrix that exercises the user's
    /// exact gesture chain: text on screen in the embedded terminal →
    /// real Cmd+C through the terminal pane handler → switch to the
    /// destination pane → paste. The clipboard is the per-thread mock
    /// (see `crate::clipboard::test_clip`) so both ends ride the same
    /// transport every croft Cmd+C / Cmd+V hits in production. Every
    /// pair below must succeed in BOTH terminal layouts because the
    /// user explicitly demands identical behaviour ("the terminal
    /// size should not matter").
    #[test]
    fn terminal_copy_paste_into_other_panes_works_in_split_and_maximised_layout() {
        // Seed a PtyTerminal grid with a known string, set a selection
        // over it, and trigger the production Cmd+C path so the mock
        // clipboard ends up with the payload. Returns the payload that
        // landed on the clipboard.
        fn copy_terminal_word(app: &mut App) -> String {
            let probe = format!("croft-e2e-{}", std::process::id());
            app.terminal_mut()
                .write_input(format!("printf '{probe}\\n'\r").as_bytes());
            let started = std::time::Instant::now();
            while started.elapsed() < std::time::Duration::from_millis(3000) {
                if app.terminal().visible_text().contains(&probe) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let snap = app.terminal().visible_text();
            let row_idx = snap.lines().position(|l| l.contains(&probe)).unwrap();
            let line = snap.lines().nth(row_idx).unwrap();
            let col_start = line.find(&probe).unwrap();
            let col_end = col_start + probe.len() - 1;
            let inner = app.terminal().last_inner;
            let screen_y = inner.y + row_idx as u16;
            let screen_x_a = inner.x + col_start as u16;
            let screen_x_b = inner.x + col_end as u16;
            app.terminal_mut().start_selection_at(screen_x_a, screen_y);
            app.terminal_mut().extend_selection_to(screen_x_b, screen_y);
            assert_eq!(
                app.terminal().selection_text(),
                probe,
                "terminal selection_text must reproduce the probe before we copy"
            );
            app.copy_terminal_selection();
            probe
        }

        // Drive every destination/layout pair. The clipboard lives on
        // the test thread, so cross-pane wiring is the only thing
        // that can fail here.
        let layouts: &[bool] = &[false, true]; // false = split, true = maximised
        for &maximise in layouts {
            // ---- Destination: Search sidebar ----
            {
                let _g = crate::clipboard::lock_clipboard_for_test();
                let tmp = tempfile::tempdir().unwrap();
                let mut app = App::new(tmp.path().to_path_buf()).unwrap();
                let backend = ratatui::backend::TestBackend::new(140, 50);
                let mut term = ratatui::Terminal::new(backend).unwrap();
                term.draw(|f| app.render(f)).unwrap();
                if maximise {
                    app.toggle_terminal_maximize();
                    term.draw(|f| app.render(f)).unwrap();
                }
                app.focus_pane(Pane::Terminal);
                let probe = copy_terminal_word(&mut app);
                // Switch to Search like a real user would (Cmd+Shift+F
                // through the production key path).
                let cmd_shift_f = key(
                    KeyCode::Char('F'),
                    KeyModifiers::SUPER | KeyModifiers::SHIFT,
                );
                app.handle_key(cmd_shift_f).unwrap();
                term.draw(|f| app.render(f)).unwrap();
                // Simulate the bracketed paste iTerm2 would send on
                // Cmd+V (production path with setup-iterm2 applied).
                let clip = crate::clipboard::read_string()
                    .expect("clipboard must hold what copy_terminal_selection just wrote");
                assert_eq!(clip, probe);
                app.handle_paste(&clip);
                assert!(
                    app.search.query.contains(&probe),
                    "terminal copy -> Search paste must work in {} layout; got query={:?}",
                    if maximise { "maximised" } else { "split" },
                    app.search.query
                );
            }

            // ---- Destination: editor-find ----
            {
                let _g = crate::clipboard::lock_clipboard_for_test();
                let tmp = tempfile::tempdir().unwrap();
                let f = tmp.path().join("probe.txt");
                std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
                let mut app = App::new(tmp.path().to_path_buf()).unwrap();
                app.editor.open_pinned(&f).unwrap();
                let backend = ratatui::backend::TestBackend::new(140, 50);
                let mut term = ratatui::Terminal::new(backend).unwrap();
                term.draw(|f| app.render(f)).unwrap();
                if maximise {
                    app.toggle_terminal_maximize();
                    term.draw(|f| app.render(f)).unwrap();
                }
                app.focus_pane(Pane::Terminal);
                let probe = copy_terminal_word(&mut app);
                // Switch back to editor + open find.
                app.focus_pane(Pane::Editor);
                app.open_editor_find();
                term.draw(|f| app.render(f)).unwrap();
                let clip = crate::clipboard::read_string().unwrap();
                assert_eq!(clip, probe);
                app.handle_paste(&clip);
                let q = app
                    .editor_find
                    .as_ref()
                    .map(|s| s.query.clone())
                    .unwrap_or_default();
                assert!(
                    q.contains(&probe),
                    "terminal copy -> editor-find paste must work in {} layout; got query={q:?}",
                    if maximise { "maximised" } else { "split" }
                );
            }

            // ---- Destination: editor body ----
            {
                let _g = crate::clipboard::lock_clipboard_for_test();
                let tmp = tempfile::tempdir().unwrap();
                let f = tmp.path().join("probe.txt");
                std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
                let mut app = App::new(tmp.path().to_path_buf()).unwrap();
                app.editor.open_pinned(&f).unwrap();
                let backend = ratatui::backend::TestBackend::new(140, 50);
                let mut term = ratatui::Terminal::new(backend).unwrap();
                term.draw(|f| app.render(f)).unwrap();
                if maximise {
                    app.toggle_terminal_maximize();
                    term.draw(|f| app.render(f)).unwrap();
                }
                app.focus_pane(Pane::Terminal);
                let probe = copy_terminal_word(&mut app);
                app.focus_pane(Pane::Editor);
                term.draw(|f| app.render(f)).unwrap();
                let clip = crate::clipboard::read_string().unwrap();
                assert_eq!(clip, probe);
                app.handle_paste(&clip);
                assert!(
                    app.editor.lines.iter().any(|l| l.contains(&probe)),
                    "terminal copy -> editor body paste must work in {} layout; lines={:?}",
                    if maximise { "maximised" } else { "split" },
                    app.editor.lines
                );
            }

            // ---- Destination: source-control commit box ----
            {
                let _g = crate::clipboard::lock_clipboard_for_test();
                let tmp = tempfile::tempdir().unwrap();
                let mut app = App::new(tmp.path().to_path_buf()).unwrap();
                let backend = ratatui::backend::TestBackend::new(140, 50);
                let mut term = ratatui::Terminal::new(backend).unwrap();
                term.draw(|f| app.render(f)).unwrap();
                if maximise {
                    app.toggle_terminal_maximize();
                    term.draw(|f| app.render(f)).unwrap();
                }
                app.focus_pane(Pane::Terminal);
                let probe = copy_terminal_word(&mut app);
                app.set_sidebar_view(SidebarView::SourceControl);
                term.draw(|f| app.render(f)).unwrap();
                let clip = crate::clipboard::read_string().unwrap();
                assert_eq!(clip, probe);
                app.handle_paste(&clip);
                assert!(
                    app.source_control.message.contains(&probe),
                    "terminal copy -> Source Control paste must work in {} layout; msg={:?}",
                    if maximise { "maximised" } else { "split" },
                    app.source_control.message
                );
            }
        }
    }

    /// User-reported root cause: with the Search sidebar open, Cmd+C /
    /// Cmd+V in the terminal silently no-op because the search-editing
    /// shortcut guard fired on `focus != Editor`, swallowing the keys
    /// into the (empty) search query instead of the focused terminal.
    /// Copying from the terminal must work regardless of which sidebar
    /// view is showing.
    #[test]
    fn terminal_cmd_c_copies_even_when_search_sidebar_is_open() {
        let _g = crate::clipboard::lock_clipboard_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let backend = ratatui::backend::TestBackend::new(140, 50);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        term.draw(|f| app.render(f)).unwrap();

        let probe = format!("croft-search-open-{}", std::process::id());
        app.terminal_mut()
            .write_input(format!("printf '{probe}\\n'\r").as_bytes());
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_millis(3000) {
            if app.terminal().visible_text().contains(&probe) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        term.draw(|f| app.render(f)).unwrap();
        let snap = app.terminal().visible_text();
        let row_idx = snap.lines().position(|l| l.contains(&probe)).unwrap();
        let line = snap.lines().nth(row_idx).unwrap();
        let col_start = line.find(&probe).unwrap();
        let col_end = col_start + probe.len() - 1;
        let inner = app.terminal().last_inner;
        let screen_y = inner.y + row_idx as u16;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
            column: inner.x + col_start as u16,
            row: screen_y,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(MouseButton::Left),
            column: inner.x + col_end as u16,
            row: screen_y,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(MouseButton::Left),
            column: inner.x + col_end as u16,
            row: screen_y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.focus == Pane::Terminal, "terminal click must focus terminal");
        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::SUPER))
            .unwrap();
        assert_eq!(
            crate::clipboard::read_string().as_deref(),
            Some(probe.as_str()),
            "Cmd+C in the terminal must copy the terminal selection even when the Search sidebar is the active view"
        );
        assert!(
            app.search.query.is_empty(),
            "Cmd+C in the terminal must NOT leak into the search query; query={:?}",
            app.search.query
        );
    }

    #[test]
    fn ctrl_j_clears_maximize_when_hiding_terminal() {
        // Mental contract: Ctrl+J means "give me my normal layout back."
        // Hiding the terminal must clear the maximize flag so the next
        // Ctrl+J reopen restores the editor+terminal split, not a stuck
        // maximized terminal.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let max_chord = key(
            KeyCode::Char('J'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        let toggle_chord = key(KeyCode::Char('j'), KeyModifiers::CONTROL);
        app.handle_key(max_chord).unwrap();
        assert!(app.terminal_maximized);
        app.handle_key(toggle_chord).unwrap();
        assert!(!app.show_terminal, "Ctrl+J hides the terminal");
        assert!(
            !app.terminal_maximized,
            "hiding the terminal also clears maximize"
        );
        app.handle_key(toggle_chord).unwrap();
        assert!(app.show_terminal, "Ctrl+J reshows the terminal");
        assert!(
            !app.terminal_maximized,
            "reopened terminal stays in normal split, not maximized"
        );
    }

    #[test]
    fn maximized_terminal_collapses_editor_in_render_layout() {
        // Drive an actual frame and assert the editor pane received zero
        // height while the terminal occupies the full right column. This
        // pins the user-visible behavior, not just the flag.
        use ratatui::{Terminal, backend::TestBackend};
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let max_chord = key(
            KeyCode::Char('J'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        app.handle_key(max_chord).unwrap();
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        assert_eq!(
            app.editor.last_area.height, 0,
            "maximized terminal must collapse editor to zero rows"
        );
        assert!(
            app.editor.last_full_area.width > 0,
            "editor still tracks its column for hit-testing"
        );
    }

    #[test]
    fn maximizing_terminal_requests_welcome_image_clear_when_image_was_displayed() {
        // Repro for "wordmark + icon bleed through the maximized terminal":
        // the welcome OSC-1337 image was painted at the editor pane's cells
        // before maximize. Maximizing collapses the editor to zero height
        // and the terminal pane now covers those cells, but iTerm2 still
        // has the image cached at the old cell positions. ratatui's text
        // writes do NOT evict that cache - only a one-shot terminal.clear()
        // does, gated by consume_welcome_image_clear().
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.welcome_image_displayed = true;
        let chord = key(
            KeyCode::Char('J'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        app.handle_key(chord).unwrap();
        assert!(app.terminal_maximized, "first chord maximizes");
        assert!(
            app.consume_welcome_image_clear(),
            "maximize must arm a one-shot welcome image clear"
        );
    }

    #[test]
    fn maximizing_terminal_is_noop_for_welcome_clear_when_image_was_not_displayed() {
        // No prior emit means iTerm has no cached cells to evict; the clear
        // chain must stay silent so the main loop doesn't pointlessly
        // CSI-2J the screen every time the user toggles maximize before
        // they ever land on the welcome panel.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.welcome_image_displayed = false;
        let chord = key(
            KeyCode::Char('J'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        app.handle_key(chord).unwrap();
        assert!(app.terminal_maximized);
        assert!(
            !app.consume_welcome_image_clear(),
            "maximize must NOT arm a clear when no image was ever emitted"
        );
    }

    #[test]
    fn restoring_terminal_from_maximize_rearms_welcome_overlay_dirty() {
        // After a clear, the post-draw emitter re-paints the welcome only
        // when welcome_overlay_dirty is true. render_welcome only sets it
        // when the layout *changes*, so if the restored editor area is
        // bit-identical to the pre-maximize area the dirty flag would
        // stay false and the wordmark would never come back. Exit must
        // arm it explicitly.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let chord = key(
            KeyCode::Char('J'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        app.handle_key(chord).unwrap();
        assert!(app.terminal_maximized);
        app.welcome_overlay_dirty = false;
        app.handle_key(chord).unwrap();
        assert!(!app.terminal_maximized);
        assert!(
            app.welcome_overlay_dirty,
            "exiting maximize must re-arm welcome_overlay_dirty so the wordmark re-paints"
        );
    }

    #[test]
    fn delete_key_is_recognized_as_delete_node() {
        assert!(is_delete_node_key(key(KeyCode::Delete, KeyModifiers::NONE)));
    }

    #[test]
    fn cmd_backspace_is_recognized_as_delete_node() {
        // macOS Finder convention: ⌘⌫ moves the selection to the Trash.
        assert!(is_delete_node_key(key(KeyCode::Backspace, KeyModifiers::SUPER)));
    }

    #[test]
    fn plain_backspace_is_delete_node_on_mac_layouts() {
        // On Mac keyboards the key labeled "delete" reports as Backspace; in
        // the tree pane (the only context this helper is called from) it must
        // trigger deletion to match the user's expectation.
        assert!(is_delete_node_key(key(KeyCode::Backspace, KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_d_is_not_delete_node() {
        assert!(!is_delete_node_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn open_create_prompt_populates_state() {
        // We can't easily build a full App in a test (PtyTerminal spawns a real
        // shell), so we exercise the Prompt struct directly via the public
        // helpers it relies on.  This guards the data the prompt carries.
        use crate::widgets::file_tree::{
            create_file_in, create_folder_in, validate_new_name,
        };
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        // commit_prompt path: validate → create → returns path on success.
        assert!(validate_new_name("hello.rs").is_ok());
        let p = create_file_in(tmp.path(), "hello.rs").unwrap();
        assert!(p.is_file());
        let d = create_folder_in(tmp.path(), "newdir").unwrap();
        assert!(d.is_dir());
    }

    #[test]
    fn set_title_seq_wraps_with_osc0_and_bel() {
        let bytes = set_title_seq("croft");
        assert_eq!(bytes, b"\x1b]0;croft\x07");
    }

    #[test]
    fn set_title_seq_handles_empty_title() {
        let bytes = set_title_seq("");
        assert_eq!(bytes, b"\x1b]0;\x07");
    }

    #[test]
    fn set_title_seq_passes_through_unicode() {
        let bytes = set_title_seq("croft  README.md");
        assert_eq!(bytes, "\x1b]0;croft  README.md\x07".as_bytes());
    }

    #[test]
    fn set_title_seq_strips_control_bytes_that_would_break_the_escape() {
        // BEL terminates the OSC, ESC starts a new sequence — both must be filtered.
        let bytes = set_title_seq("evil\x07file");
        assert_eq!(bytes, b"\x1b]0;evilfile\x07");
        let bytes = set_title_seq("evil\x1bfile");
        assert_eq!(bytes, b"\x1b]0;evilfile\x07");
        let bytes = set_title_seq("evil\nfile");
        assert_eq!(bytes, b"\x1b]0;evilfile\x07");
    }

    #[test]
    fn app_name_constant_is_croft() {
        assert_eq!(APP_NAME, "croft");
    }

    #[test]
    fn set_session_bg_srgb_seq_uses_iterm_osc1337_and_srgb_prefix() {
        // iTerm2 must read the bg as sRGB so the OSC-1337 PNG canvas (also
        // sRGB) matches pixel-for-pixel. Locking in the exact bytes catches
        // any future refactor that drops the `srgb:` prefix or the iTerm2
        // OSC introducer.
        let seq = set_session_bg_srgb_seq(EDITOR_BG_RGB);
        assert_eq!(seq, "\x1b]1337;SetColors=bg=srgb:1e222e\x07");
    }

    #[test]
    fn reset_session_bg_seq_restores_profile_default() {
        // On exit we must revert iTerm2's session bg so the user's shell
        // doesn't keep croft's forced bg colour.
        assert_eq!(reset_session_bg_seq(), "\x1b]1337;SetColors=bg=default\x07");
    }

    #[test]
    fn welcome_logo_png_asset_is_present_and_decodable() {
        // Regression guard for the welcome screen: the bundled logo asset
        // must remain a valid PNG so the OSC-1337 path can bake it. If
        // someone replaces it with a half-block fallback or empties the
        // asset, this catches it on every CI run.
        let bytes = crate::iterm2_inline::WELCOME_LOGO_PNG;
        assert!(bytes.len() > 1024, "welcome logo asset suspiciously small");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG file");
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .expect("welcome logo PNG must decode");
        assert!(img.width() > 0 && img.height() > 0);
    }

    #[test]
    fn welcome_image_bake_produces_osc1337_carrying_logo_pixels() {
        // End-to-end regression for the welcome render path: feeding the
        // bundled logo PNG through the same `fit_image` + `build_inline_image_osc`
        // pipeline `render_welcome` uses must yield an OSC-1337 sequence
        // that (a) starts with the iTerm2 introducer, (b) advertises the
        // requested cell dimensions, and (c) carries an opaque canvas
        // filled with `EDITOR_BG_RGB`. If anyone swaps OSC-1337 for a
        // half-block or text fallback, this test will refuse to compile
        // or fail the byte assertions.
        let canvas_w = 48u32 * 8; // approx welcome cell w * cell pixel w
        let canvas_h = 14u32 * 16;
        let bg = image::Rgba([
            EDITOR_BG_RGB.0,
            EDITOR_BG_RGB.1,
            EDITOR_BG_RGB.2,
            0xff,
        ]);
        let baked = crate::iterm2_inline::fit_image(
            crate::iterm2_inline::WELCOME_LOGO_PNG,
            canvas_w,
            canvas_h,
            bg,
        )
        .expect("baked welcome PNG");
        assert_eq!(&baked[..8], b"\x89PNG\r\n\x1a\n", "fit_image must emit a PNG");
        let osc = crate::iterm2_inline::build_inline_image_osc(&baked, 48, 14, false);
        assert!(osc.starts_with("\x1b]1337;File=inline=1"));
        assert!(osc.ends_with('\x07'));
        assert!(osc.contains("width=48"));
        assert!(osc.contains("height=14"));

        // Verify the canvas corner pixels are exactly EDITOR_BG_RGB so the
        // welcome image bg matches the sRGB-decoded session bg.
        let img = image::load_from_memory_with_format(&baked, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        for &(x, y) in &[
            (0u32, 0u32),
            (canvas_w - 1, 0),
            (0, canvas_h - 1),
            (canvas_w - 1, canvas_h - 1),
        ] {
            let p = img.get_pixel(x, y);
            assert_eq!(
                (p.0[0], p.0[1], p.0[2], p.0[3]),
                (EDITOR_BG_RGB.0, EDITOR_BG_RGB.1, EDITOR_BG_RGB.2, 0xff),
                "canvas corner ({x}, {y}) must be opaque editor bg"
            );
        }
    }

    #[test]
    fn brand_pill_uses_app_name_constant() {
        assert_eq!(brand_pill_text(), " croft ");
    }

    #[test]
    fn keyboard_enhancement_flags_request_disambiguate_escape_codes() {
        let f = keyboard_enhancement_flags();
        assert!(
            f.contains(crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            "Cmd / modifier delivery requires DISAMBIGUATE_ESCAPE_CODES"
        );
    }

    #[test]
    fn keyboard_enhancement_flags_do_not_force_all_keys_as_escapes() {
        // REPORT_ALL_KEYS_AS_ESCAPE_CODES rewrites plain printable input.
        // We only need the disambiguation bit, not the all-keys bit.
        let f = keyboard_enhancement_flags();
        assert!(
            !f.contains(crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES),
            "we should not opt into REPORT_ALL_KEYS_AS_ESCAPE_CODES"
        );
    }

    #[test]
    fn status_bar_advertises_terminal_toggle_shortcut() {
        let src = include_str!("app.rs");
        assert!(
            src.contains("\"^j\""),
            "status bar should advertise ^j as the terminal-toggle shortcut"
        );
        assert!(
            src.contains("Span::raw(\" Term"),
            "status bar should label the ^j shortcut 'Term' (matched a `Span::raw(\" Term` prefix so cosmetic trailing-whitespace tweaks do not break the assertion)"
        );
    }

    #[test]
    fn no_stale_tcode_pill_literal_in_source() {
        let src = include_str!("app.rs");
        assert!(
            !src.contains("\" tcode \""),
            "stale ` tcode ` pill literal still present in src/app.rs"
        );
    }

    #[test]
    fn build_title_uses_basename_and_app_name() {
        let p = std::path::Path::new("/Users/somebody/projects/croft");
        assert_eq!(build_title(p), "croft  croft");
        let p = std::path::Path::new("/");
        assert_eq!(build_title(p), "croft  /");
    }

    fn file_node(path: &Path) -> crate::widgets::file_tree::Node {
        crate::widgets::file_tree::Node {
            path: path.to_path_buf(),
            depth: 1,
            is_dir: false,
            expanded: false,
            loaded: false,
        }
    }

    fn dir_node(path: &Path) -> crate::widgets::file_tree::Node {
        crate::widgets::file_tree::Node {
            path: path.to_path_buf(),
            depth: 1,
            is_dir: true,
            expanded: false,
            loaded: false,
        }
    }

    #[test]
    fn tree_context_menu_on_file_offers_new_file_new_folder_then_cut_copy_rename_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("hello.txt");
        std::fs::write(&f, "hi").unwrap();
        let n = file_node(&f);
        let target = f.parent().unwrap().to_path_buf();
        let items = build_tree_context_menu_items(
            Some(&n),
            tmp.path(),
            &[f.clone()],
            &target,
            None,
            None,
        );
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            labels,
            [
                "New File",
                "New Folder",
                "Cut",
                "Copy",
                "Rename",
                "Select for Compare",
                "Delete",
            ],
        );
        assert!(matches!(&items[0].1, MenuAction::Create(CreateKind::File)));
        assert!(matches!(&items[1].1, MenuAction::Create(CreateKind::Folder)));
        assert!(matches!(&items[2].1, MenuAction::Cut(ps) if ps == &vec![f.clone()]));
        assert!(matches!(&items[3].1, MenuAction::Copy(ps) if ps == &vec![f.clone()]));
        assert!(matches!(&items[4].1, MenuAction::Rename(p) if p == &f));
        assert!(matches!(&items[5].1, MenuAction::SelectForCompare(p) if p == &f));
        assert!(matches!(&items[6].1, MenuAction::Delete { paths } if paths == &vec![f.clone()]));
    }

    #[test]
    fn explorer_shortcut_predicates_recognise_expected_keys() {
        assert!(is_tree_new_file_key(key(KeyCode::Char('f'), KeyModifiers::SUPER)));
        assert!(is_tree_new_file_key(key(KeyCode::Char('F'), KeyModifiers::CONTROL)));
        assert!(!is_tree_new_file_key(key(KeyCode::Char('f'), KeyModifiers::NONE)));
        // Shift+Super disqualifies New File so Cmd+Shift+F can fall through to
        // the global Search jump (New Folder now lives on Cmd+Shift+N).
        assert!(!is_tree_new_file_key(key(
            KeyCode::Char('F'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT
        )));

        assert!(is_tree_new_folder_key(key(
            KeyCode::Char('N'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT
        )));
        assert!(is_tree_new_folder_key(key(
            KeyCode::Char('N'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(!is_tree_new_folder_key(key(
            KeyCode::Char('n'),
            KeyModifiers::SUPER
        )));
        assert!(
            !is_tree_new_folder_key(key(
                KeyCode::Char('F'),
                KeyModifiers::SUPER | KeyModifiers::SHIFT
            )),
            "Cmd+Shift+F is no longer the New Folder chord — it must fall through to the global Search jump"
        );

        assert!(is_tree_rename_key(key(KeyCode::Char('r'), KeyModifiers::SUPER)));
        assert!(is_tree_rename_key(key(KeyCode::F(2), KeyModifiers::NONE)));
        assert!(!is_tree_rename_key(key(KeyCode::Char('r'), KeyModifiers::NONE)));

        assert!(is_tree_make_root_key(key(KeyCode::Char('/'), KeyModifiers::SUPER)));
        assert!(is_tree_make_root_key(key(KeyCode::Char('/'), KeyModifiers::CONTROL)));
        assert!(!is_tree_make_root_key(key(KeyCode::Char('/'), KeyModifiers::NONE)));

        let cmd_shift = KeyModifiers::SUPER | KeyModifiers::SHIFT;
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(is_tree_make_parent_root_key(key(KeyCode::Char('/'), cmd_shift)));
        assert!(is_tree_make_parent_root_key(key(KeyCode::Char('/'), ctrl_shift)));
        assert!(is_tree_make_parent_root_key(key(KeyCode::Char('?'), KeyModifiers::SUPER)));
        assert!(is_tree_make_parent_root_key(key(KeyCode::Char('?'), KeyModifiers::CONTROL)));
        assert!(is_tree_make_parent_root_key(key(KeyCode::Char('?'), cmd_shift)));
        assert!(!is_tree_make_parent_root_key(key(KeyCode::Char('/'), KeyModifiers::SUPER)));
        assert!(!is_tree_make_parent_root_key(key(KeyCode::Char('/'), KeyModifiers::NONE)));
        assert!(!is_tree_make_parent_root_key(key(KeyCode::Char('?'), KeyModifiers::NONE)));
        assert!(!is_tree_make_root_key(key(KeyCode::Char('/'), cmd_shift)));
    }

    #[test]
    fn tree_typeahead_first_keystroke_jumps_to_first_matching_visible_node() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("apple")).unwrap();
        std::fs::write(tmp.path().join("banana.txt"), "").unwrap();
        std::fs::write(tmp.path().join("cherry.txt"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.apply_tree_typeahead('b', std::time::Instant::now());
        let picked = app.tree.nodes[app.tree.selected]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(picked, "banana.txt", "typing 'b' must select banana.txt");
    }

    #[test]
    fn tree_typeahead_accumulates_prefix_within_window() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("test")).unwrap();
        std::fs::create_dir(tmp.path().join("toolbox")).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let t0 = std::time::Instant::now();
        app.apply_tree_typeahead('t', t0);
        app.apply_tree_typeahead('e', t0 + std::time::Duration::from_millis(100));
        let name = app.tree.nodes[app.tree.selected]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            name, "test",
            "'te' typed within the window must land on 'test', not 'toolbox'"
        );
    }

    #[test]
    fn tree_typeahead_resets_after_window_expires_and_cycles() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("test")).unwrap();
        std::fs::create_dir(tmp.path().join("toolbox")).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let t0 = std::time::Instant::now();
        app.apply_tree_typeahead('t', t0);
        let first = app.tree.selected;
        let first_name = app.tree.nodes[first]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.apply_tree_typeahead('t', t0 + std::time::Duration::from_millis(1000));
        let second = app.tree.selected;
        let second_name = app.tree.nodes[second]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_ne!(
            first, second,
            "the second 't' after the window expires must cycle to the next 't' match"
        );
        assert!(
            first_name.starts_with('t') && second_name.starts_with('t'),
            "both cycle stops must start with 't'"
        );
    }

    #[test]
    fn tree_typeahead_is_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("README.md"), "").unwrap();
        std::fs::write(tmp.path().join("zzz.txt"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.apply_tree_typeahead('r', std::time::Instant::now());
        let picked = app.tree.nodes[app.tree.selected]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(picked, "README.md");
    }

    #[test]
    fn tree_typeahead_via_handle_tree_key_jumps_selection() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("apple")).unwrap();
        std::fs::write(tmp.path().join("banana.txt"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Tree);
        app.sidebar_view = SidebarView::Explorer;
        app.handle_tree_key(key(KeyCode::Char('b'), KeyModifiers::NONE));
        let picked = app.tree.nodes[app.tree.selected]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            picked, "banana.txt",
            "a plain letter routed through handle_tree_key must drive typeahead"
        );
    }

    #[test]
    fn tree_typeahead_does_not_fire_when_modifier_held() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("banana.txt"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let before = app.tree.selected;
        app.focus_pane(Pane::Tree);
        app.sidebar_view = SidebarView::Explorer;
        app.handle_tree_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(
            app.tree.selected, before,
            "Ctrl+letter must not be consumed as typeahead"
        );
        assert!(
            app.tree_typeahead.is_none(),
            "Ctrl-modified keys must not seed the typeahead buffer"
        );
    }

    #[test]
    fn cmd_shift_n_in_explorer_pane_creates_a_new_folder() {
        // With Explorer focused, Cmd+Shift+N opens the New Folder prompt
        // (the user-requested binding, moved off Cmd+Shift+F so that chord
        // can always reach the global Search jump).
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Tree);
        app.sidebar_view = SidebarView::Explorer;
        app.handle_key(key(
            KeyCode::Char('N'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert!(
            matches!(
                app.prompt.as_ref(),
                Some(Prompt { kind: PromptKind::Create(CreateKind::Folder), .. })
            ),
            "Explorer-focused Cmd+Shift+N must open a New Folder prompt"
        );
        assert_eq!(app.sidebar_view, SidebarView::Explorer, "must NOT jump to Search");
    }

    #[test]
    fn cmd_shift_f_in_explorer_pane_jumps_to_search_not_new_folder() {
        // Cmd+Shift+F is no longer the Explorer's New Folder chord — it
        // now always jumps to the Search sidebar, even when Explorer is
        // focused. New Folder lives on Cmd+Shift+N.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Tree);
        app.sidebar_view = SidebarView::Explorer;
        app.handle_key(key(
            KeyCode::Char('F'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert!(
            app.prompt.is_none(),
            "Cmd+Shift+F must NOT open the New Folder prompt anymore"
        );
        assert_eq!(app.sidebar_view, SidebarView::Search);
    }

    #[test]
    fn cmd_shift_f_outside_explorer_still_jumps_to_search() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Editor);
        app.sidebar_view = SidebarView::Explorer;
        app.handle_key(key(
            KeyCode::Char('F'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(app.sidebar_view, SidebarView::Search);
    }

    #[test]
    fn shortcut_for_returns_expected_shortcuts_for_each_explorer_action() {
        let p = std::path::PathBuf::from("/x");
        assert_eq!(
            shortcut_for(&MenuAction::Create(CreateKind::File)),
            Some("⌘F")
        );
        assert_eq!(
            shortcut_for(&MenuAction::Create(CreateKind::Folder)),
            Some("⌘⇧N")
        );
        assert_eq!(shortcut_for(&MenuAction::Cut(vec![p.clone()])), Some("⌘X"));
        assert_eq!(shortcut_for(&MenuAction::Copy(vec![p.clone()])), Some("⌘C"));
        assert_eq!(shortcut_for(&MenuAction::Paste(p.clone())), Some("⌘V"));
        assert_eq!(shortcut_for(&MenuAction::Rename(p.clone())), Some("⌘R"));
        assert_eq!(shortcut_for(&MenuAction::MakeRoot(p.clone())), Some("⌘/"));
        assert_eq!(
            shortcut_for(&MenuAction::Delete { paths: vec![p.clone()] }),
            Some("⌫")
        );
        // Compare actions have no keybinding (no shortcut shown).
        assert_eq!(shortcut_for(&MenuAction::SelectForCompare(p)), None);
    }

    #[test]
    fn tree_context_menu_on_subfolder_offers_make_root_between_rename_and_delete() {
        // "Make root" should appear on folder right-click so the user can
        // re-root the workspace into that folder (e.g., a nested git repo).
        // It must NOT appear on file right-clicks - files can't be a
        // workspace root.
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("project_a");
        std::fs::create_dir(&d).unwrap();
        let n = dir_node(&d);
        let items = build_tree_context_menu_items(
            Some(&n),
            tmp.path(),
            &[d.clone()],
            &d,
            None,
            None,
        );
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert!(
            labels.contains(&"Make root"),
            "folder right-click must include Make root, got {labels:?}"
        );
        let mr = items
            .iter()
            .find_map(|(_, a)| match a {
                MenuAction::MakeRoot(p) => Some(p),
                _ => None,
            })
            .expect("Make root action must carry the folder path");
        assert_eq!(mr, &d);
    }

    #[test]
    fn tree_context_menu_on_file_does_not_offer_make_root() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("hello.txt");
        std::fs::write(&f, "hi").unwrap();
        let n = file_node(&f);
        let target = f.parent().unwrap().to_path_buf();
        let items = build_tree_context_menu_items(
            Some(&n),
            tmp.path(),
            &[f.clone()],
            &target,
            None,
            None,
        );
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert!(
            !labels.contains(&"Make root"),
            "files cannot be roots; Make root must be hidden for files: {labels:?}"
        );
    }

    #[test]
    fn cmd_shift_slash_on_a_file_changes_workspace_root_to_the_file_s_parent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let inner = tmp.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        let file = inner.join("note.txt");
        std::fs::write(&file, "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let target_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path == file)
            .or_else(|| {
                let i = app.tree.nodes.iter().position(|n| n.path == inner).unwrap();
                app.tree.selected = i;
                app.tree.expand_selected();
                app.tree.nodes.iter().position(|n| n.path == file)
            })
            .expect("file node must be visible after expanding inner/");
        app.tree.selected = target_idx;
        app.focus = Pane::Tree;
        app.sidebar_view = SidebarView::Explorer;

        let handled = app.handle_explorer_shortcut(key(
            KeyCode::Char('/'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ));

        assert!(handled, "Cmd+Shift+/ must be handled by the Explorer dispatcher");
        assert_eq!(app.workspace_root, inner);
        assert_eq!(app.tree.root, inner);
    }

    #[test]
    fn cmd_shift_slash_on_a_folder_changes_workspace_root_to_that_folder_s_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let mid = tmp.path().join("mid");
        let leaf = mid.join("leaf");
        std::fs::create_dir_all(&leaf).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let mid_idx = app.tree.nodes.iter().position(|n| n.path == mid).unwrap();
        app.tree.selected = mid_idx;
        app.tree.expand_selected();
        let leaf_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path == leaf)
            .expect("leaf must be visible after expanding mid/");
        app.tree.selected = leaf_idx;
        app.focus = Pane::Tree;
        app.sidebar_view = SidebarView::Explorer;

        let handled = app.handle_explorer_shortcut(key(
            KeyCode::Char('?'),
            KeyModifiers::SUPER,
        ));

        assert!(handled);
        assert_eq!(app.workspace_root, mid);
        assert_eq!(app.tree.root, mid);
    }

    #[test]
    fn cmd_shift_slash_on_the_tree_root_walks_up_one_filesystem_level() {
        let tmp = tempfile::tempdir().unwrap();
        let inner = tmp.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        let mut app = App::new(inner.clone()).unwrap();
        app.tree.selected = 0;
        app.focus = Pane::Tree;
        app.sidebar_view = SidebarView::Explorer;

        let handled = app.handle_explorer_shortcut(key(
            KeyCode::Char('/'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ));

        assert!(handled);
        assert_eq!(app.workspace_root, tmp.path());
        assert_eq!(app.tree.root, tmp.path());
    }

    #[test]
    fn dispatch_make_root_changes_tree_root_and_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project_b");
        std::fs::create_dir(&project).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let original_root = app.tree.root.clone();
        app.dispatch_menu_action(
            MenuAction::MakeRoot(project.clone()),
            project.clone(),
        );
        assert_ne!(app.tree.root, original_root);
        assert_eq!(app.tree.root, project);
        assert_eq!(app.workspace_root, project);
        // The tree's root node must reflect the new path.
        assert_eq!(app.tree.nodes[0].path, project);
    }

    #[test]
    fn change_workspace_root_writes_cd_into_active_terminal_pty() {
        let tmp = tempfile::tempdir().unwrap();
        let target_dir = tmp.path().join("seeded");
        std::fs::create_dir(&target_dir).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();

        // Clear the freshly-spawned shell's "first paint" dirty bit so
        // the next take_dirty() exclusively reflects bytes written by
        // change_workspace_root itself. change_workspace_root performs
        // no other write_input on the terminal, so a dirty flag after
        // the call can only have come from the change_cwd seed.
        let _ = app.terminal_mut().take_dirty();
        app.change_workspace_root(target_dir.clone());

        assert!(
            app.terminal_mut().take_dirty(),
            "change_workspace_root must seed the active terminal's PTY with a cd command so the shell follows the Explorer; nothing was written"
        );
        assert_eq!(
            app.workspace_root, target_dir,
            "workspace_root must still flip to the new path"
        );
    }

    #[test]
    fn tree_context_menu_on_subfolder_offers_new_file_new_folder_then_cut_copy_rename_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("sub");
        std::fs::create_dir(&d).unwrap();
        let n = dir_node(&d);
        let target = d.clone();
        let items = build_tree_context_menu_items(
            Some(&n),
            tmp.path(),
            &[d.clone()],
            &target,
            None,
            None,
        );
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            labels,
            ["New File", "New Folder", "Cut", "Copy", "Rename", "Make root", "Delete"],
        );
    }

    #[test]
    fn tree_context_menu_with_clipboard_offers_paste_on_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("sub");
        std::fs::create_dir(&d).unwrap();
        let n = dir_node(&d);
        let clip = ExplorerClipboard {
            mode: ExplorerClipMode::Cut,
            paths: vec![tmp.path().join("a.txt")],
        };
        let items = build_tree_context_menu_items(
            Some(&n),
            tmp.path(),
            &[d.clone()],
            &d,
            Some(&clip),
            None,
        );
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            labels,
            ["New File", "New Folder", "Cut", "Copy", "Paste", "Rename", "Make root", "Delete"],
        );
    }

    #[test]
    fn tree_context_menu_on_a_deeply_nested_folder_creates_inside_that_folder() {
        // Regression for the user-reported "right-click on nested
        // folders only shows Cut/Copy/Rename/Delete; I cannot create a
        // new file or folder inside the folder I clicked". The menu
        // must surface New File… / New Folder…, and the resolved
        // target_dir must be the right-clicked folder itself so the
        // create lands inside it (not at the workspace root).
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        let n = dir_node(&nested);
        let resolved_target =
            crate::widgets::file_tree::create_target_dir_for(Some(&n), tmp.path());
        assert_eq!(
            resolved_target, nested,
            "right-click on a nested folder must target the folder itself, not the workspace root",
        );
        let items = build_tree_context_menu_items(
            Some(&n),
            tmp.path(),
            &[nested.clone()],
            &resolved_target,
            None,
            None,
        );
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert!(
            labels.contains(&"New File"),
            "nested-folder right-click must offer New File…; labels = {labels:?}",
        );
        assert!(
            labels.contains(&"New Folder"),
            "nested-folder right-click must offer New Folder…; labels = {labels:?}",
        );
    }

    #[test]
    fn arrow_and_pageup_pagedown_scroll_the_diff_view() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let f1 = tmp.path().join("a.txt");
        let f2 = tmp.path().join("b.txt");
        // 30 distinct lines so the diff has plenty of rows to scroll.
        let left: String = (0..30).map(|i| format!("left-{i}\n")).collect();
        let right: String = (0..30).map(|i| format!("right-{i}\n")).collect();
        std::fs::write(&f1, left).unwrap();
        std::fs::write(&f2, right).unwrap();
        app.editor.open_diff(&f1, &f2).unwrap();
        // Pretend the editor inner is 12 rows tall (page = 10 after the
        // header + footer reservation).
        app.editor.last_inner = Rect { x: 0, y: 0, width: 80, height: 12 };
        app.focus_pane(Pane::Editor);

        // Down once → +1.
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE)).unwrap();
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 1);
        // PageDown → +page (10).
        app.handle_key(key(KeyCode::PageDown, KeyModifiers::NONE)).unwrap();
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 11);
        // Up once → -1.
        app.handle_key(key(KeyCode::Up, KeyModifiers::NONE)).unwrap();
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 10);
        // PageUp → -page.
        app.handle_key(key(KeyCode::PageUp, KeyModifiers::NONE)).unwrap();
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 0);
        // Home / End.
        app.handle_key(key(KeyCode::End, KeyModifiers::NONE)).unwrap();
        let total = app.editor.diff.as_ref().unwrap().rows.len();
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll, total);
        app.handle_key(key(KeyCode::Home, KeyModifiers::NONE)).unwrap();
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 0);
    }

    #[test]
    fn ctrl_d_with_no_anchor_stashes_selected_file() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "v1").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path == f)
            .expect("file must appear in tree");
        app.tree.selected = idx;
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.compare_anchor.as_deref(), Some(f.as_path()));
    }

    #[test]
    fn ctrl_d_again_on_same_file_clears_the_anchor() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "v1").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let idx = app.tree.nodes.iter().position(|n| n.path == f).unwrap();
        app.tree.selected = idx;
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(app.compare_anchor.is_none(), "second press toggles off");
    }

    #[test]
    fn ctrl_d_with_anchor_on_other_file_opens_a_diff_tab() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        std::fs::write(&a, "alpha\nbravo\n").unwrap();
        std::fs::write(&b, "alpha\nBRAVO\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let a_idx = app.tree.nodes.iter().position(|n| n.path == a).unwrap();
        let b_idx = app.tree.nodes.iter().position(|n| n.path == b).unwrap();
        // Anchor a.txt.
        app.tree.selected = a_idx;
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        // Move to b.txt and press again.
        app.tree.selected = b_idx;
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(app.compare_anchor.is_none(), "anchor consumed by compare");
        assert!(
            app.editor.diff.is_some(),
            "editor must now hold a diff tab"
        );
    }

    #[test]
    fn left_right_arrows_pan_diff_horizontally() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let f1 = tmp.path().join("a.txt");
        let f2 = tmp.path().join("b.txt");
        let long_line: String = std::iter::repeat('x').take(200).collect();
        std::fs::write(&f1, format!("{long_line}\n")).unwrap();
        std::fs::write(&f2, format!("{long_line}y\n")).unwrap();
        app.editor.open_diff(&f1, &f2).unwrap();
        app.editor.last_inner = Rect { x: 0, y: 0, width: 80, height: 12 };
        app.focus_pane(Pane::Editor);

        app.handle_key(key(KeyCode::Right, KeyModifiers::NONE)).unwrap();
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll_x, 4);
        app.handle_key(key(KeyCode::Right, KeyModifiers::NONE)).unwrap();
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll_x, 8);
        app.handle_key(key(KeyCode::Left, KeyModifiers::NONE)).unwrap();
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll_x, 4);
        // Vertical Up/Down still go to diff.scroll, not scroll_x.
        let before_y = app.editor.diff.as_ref().unwrap().scroll;
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE)).unwrap();
        assert!(app.editor.diff.as_ref().unwrap().scroll >= before_y);
    }

    #[test]
    fn mouse_horizontal_wheel_over_diff_pans_horizontally() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let f1 = tmp.path().join("a.txt");
        let f2 = tmp.path().join("b.txt");
        let long: String = std::iter::repeat('z').take(120).collect();
        std::fs::write(&f1, format!("{long}\n")).unwrap();
        std::fs::write(&f2, format!("{long}\n")).unwrap();
        app.editor.open_diff(&f1, &f2).unwrap();
        app.editor.last_area = Rect { x: 0, y: 0, width: 80, height: 20 };
        app.editor.last_full_area = app.editor.last_area;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollRight,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll_x, 4);
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollLeft,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll_x, 0);
    }

    #[test]
    fn mouse_wheel_over_search_panel_scrolls_the_results_list() {
        // User report: search shows thousands of matches piling up but
        // the panel can't be scrolled with the wheel - the user is stuck
        // at the top. The result list must respond to ScrollUp/ScrollDown
        // when the cursor is over the side panel and Search is the
        // active view.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.sidebar_view = SidebarView::Search;
        app.show_tree = true;
        // Pretend a render captured the search panel at this rect.
        app.search.last_area = Rect { x: 0, y: 0, width: 60, height: 30 };
        app.search.last_inner = Rect { x: 1, y: 1, width: 58, height: 28 };
        // 1000 hits, well past anything that can fit in 30 rows.
        app.search.hits = (0..1000)
            .map(|i| crate::widgets::search::SearchHit {
                path: tmp.path().join("a.txt"),
                line_no: i + 1,
                line_text: format!("line {i}"),
            })
            .collect();
        app.search.scroll = 0;
        app.search.selected = 0;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollDown,
            column: 30,
            row: 15,
            modifiers: KeyModifiers::NONE,
        });
        assert!(
            app.search.scroll > 0,
            "wheel-down on the search panel must advance the result list scroll"
        );
        let after_down = app.search.scroll;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollUp,
            column: 30,
            row: 15,
            modifiers: KeyModifiers::NONE,
        });
        assert!(
            app.search.scroll < after_down,
            "wheel-up must reduce the scroll back toward the top"
        );
    }

    #[test]
    fn search_panel_paints_a_scrollbar_when_hits_exceed_visible_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.sidebar_view = SidebarView::Search;
        app.show_tree = true;
        app.search.hits = (0..500)
            .map(|i| crate::widgets::search::SearchHit {
                path: tmp.path().join("a.txt"),
                line_no: i + 1,
                line_text: format!("line {i}"),
            })
            .collect();
        let area = Rect { x: 0, y: 0, width: 60, height: 30 };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut app.search, area, &mut buf);
        assert!(
            app.search.last_scrollbar.width > 0 && app.search.last_scrollbar.height > 0,
            "scrollbar rect must be tracked when results overflow the viewport"
        );
    }

    #[test]
    fn mouse_wheel_over_diff_scrolls_the_diff_not_the_text_buffer() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let f1 = tmp.path().join("a.txt");
        let f2 = tmp.path().join("b.txt");
        std::fs::write(&f1, "1\n2\n3\n4\n5\n6\n").unwrap();
        std::fs::write(&f2, "1\n2\n3\n4\n5\n6\n").unwrap();
        app.editor.open_diff(&f1, &f2).unwrap();
        // Place the editor pane somewhere the mouse can hit it.
        app.editor.last_area = Rect { x: 0, y: 0, width: 80, height: 20 };
        app.editor.last_full_area = app.editor.last_area;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 3);
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollUp,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.editor.diff.as_ref().unwrap().scroll, 0);
    }

    #[test]
    fn menu_item_at_handles_clipped_menu_so_clicks_dispatch_the_visible_row() {
        // Repro: with a 5-item menu (Cut, Copy, Rename…, Select for
        // Compare, Delete), if the user right-clicks low enough that
        // the menu must shift up by 1 to fit on screen, a click on the
        // visible "Select for Compare" row used to map to "Rename"
        // because hit-testing used the unclipped rect while rendering
        // used the clipped one.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Frame is 60 wide, 12 tall. Menu has 7 items + 2 borders = 9
        // rows. Origin at y=6 would make the menu run from row 6 to row
        // 14 (off-screen). The renderer (and hit-test) clamp y to
        // 12 - 9 = 3.
        app.last_frame_area = Rect { x: 0, y: 0, width: 60, height: 12 };
        let f = tmp.path().join("file.txt");
        std::fs::write(&f, "x").unwrap();
        let n = crate::widgets::file_tree::Node {
            path: f.clone(),
            depth: 1,
            is_dir: false,
            expanded: false,
            loaded: true,
        };
        let target = f.parent().unwrap().to_path_buf();
        let items = build_tree_context_menu_items(
            Some(&n),
            tmp.path(),
            &[f.clone()],
            &target,
            None,
            None,
        );
        // Sanity: items[5] is "Select for Compare" after the New
        // File… / New Folder… prefix lifted everything down by two.
        assert!(matches!(&items[5].1, MenuAction::SelectForCompare(_)));
        app.context_menu = Some(ContextMenu {
            origin: (10, 6),
            items,
            selected: 0,
            target_dir: target,
        });
        // The visible "Select for Compare" row sits at clipped.y + 1
        // (top border) + 5 (item index) = 3 + 1 + 5 = 9.
        let idx = app.menu_item_at(11, 9).expect("hit must land inside the menu");
        assert_eq!(idx, 5, "click on visible row 9 must map to item 5 (Select for Compare)");
    }

    #[test]
    fn tree_context_menu_offers_compare_with_selected_when_anchor_is_present() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        std::fs::write(&a, "v1").unwrap();
        let b = tmp.path().join("b.txt");
        std::fs::write(&b, "v2").unwrap();
        let n = file_node(&b);
        let items = build_tree_context_menu_items(
            Some(&n),
            tmp.path(),
            &[b.clone()],
            tmp.path(),
            None,
            Some(a.as_path()),
        );
        let kinds: Vec<&MenuAction> = items.iter().map(|(_, a)| a).collect();
        assert!(
            kinds.iter().any(|a| matches!(a, MenuAction::CompareWithSelected { .. })),
            "Compare with Selected must be offered when an anchor is set"
        );
        assert!(
            kinds.iter().any(|a| matches!(a, MenuAction::SelectForCompare(_))),
            "Select for Compare must always be offered for single files"
        );
    }

    #[test]
    fn tree_context_menu_hides_compare_when_anchor_is_the_same_file() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        std::fs::write(&a, "v1").unwrap();
        let n = file_node(&a);
        let items = build_tree_context_menu_items(
            Some(&n),
            tmp.path(),
            &[a.clone()],
            tmp.path(),
            None,
            Some(a.as_path()),
        );
        let kinds: Vec<&MenuAction> = items.iter().map(|(_, a)| a).collect();
        assert!(
            !kinds.iter().any(|a| matches!(a, MenuAction::CompareWithSelected { .. })),
            "Compare with Selected must not appear against itself"
        );
    }

    #[test]
    fn tree_context_menu_with_multi_selection_promotes_delete_count() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("hello.txt");
        std::fs::write(&f, "hi").unwrap();
        let f2 = tmp.path().join("bye.txt");
        std::fs::write(&f2, "bye").unwrap();
        let n = file_node(&f);
        let target = f.parent().unwrap().to_path_buf();
        let items = build_tree_context_menu_items(
            Some(&n),
            tmp.path(),
            &[f.clone(), f2.clone()],
            &target,
            None,
            None,
        );
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            labels,
            ["New File", "New Folder", "Cut", "Copy", "Delete 2 items"],
        );
    }

    #[test]
    fn tree_context_menu_on_empty_space_offers_new_file_and_new_folder_only() {
        let tmp = tempfile::tempdir().unwrap();
        let items = build_tree_context_menu_items(
            None,
            tmp.path(),
            &[],
            tmp.path(),
            None,
            None,
        );
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(labels, ["New File", "New Folder"]);
        assert!(matches!(items[0].1, MenuAction::Create(CreateKind::File)));
        assert!(matches!(items[1].1, MenuAction::Create(CreateKind::Folder)));
    }

    #[test]
    fn tree_context_menu_on_empty_space_includes_paste_when_clipboard_set() {
        let tmp = tempfile::tempdir().unwrap();
        let clip = ExplorerClipboard {
            mode: ExplorerClipMode::Copy,
            paths: vec![tmp.path().join("x.txt")],
        };
        let items = build_tree_context_menu_items(
            None,
            tmp.path(),
            &[],
            tmp.path(),
            Some(&clip),
            None,
        );
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(labels, ["New File", "New Folder", "Paste"]);
    }

    #[test]
    fn consume_welcome_image_clear_fires_once_when_editor_opens_a_file() {
        // Repro for the "logo bleeds through under an open file" bug:
        // after the welcome OSC-1337 has been written to iTerm, opening a
        // file must signal a one-shot screen clear so ratatui repaints
        // every cell on the next draw and iTerm's image cache for those
        // cells is wiped.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.welcome_image_displayed = true;
        let f = tmp.path().join("hi.txt");
        std::fs::write(&f, "hi").unwrap();
        app.editor.open_pinned(&f).unwrap();
        assert!(app.consume_welcome_image_clear(), "first call must fire");
        assert!(!app.welcome_image_displayed, "flag must reset after consume");
        assert!(
            !app.consume_welcome_image_clear(),
            "second call must be a no-op until welcome is re-shown"
        );
    }

    #[test]
    fn consume_welcome_image_clear_is_noop_while_editor_pane_is_blank() {
        // The image is still meant to be visible, so don't clear.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.welcome_image_displayed = true;
        assert!(!app.consume_welcome_image_clear());
        assert!(
            app.welcome_image_displayed,
            "flag must NOT reset while image is still meant to show"
        );
    }

    #[test]
    fn consume_welcome_image_clear_handles_reposition_request_while_blank() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.welcome_image_displayed = true;
        app.welcome_image_clear_requested = true;

        assert!(app.consume_welcome_image_clear());
        assert!(!app.welcome_image_displayed);
        assert!(!app.welcome_image_clear_requested);
    }

    #[test]
    fn consume_welcome_image_clear_is_noop_when_image_was_never_shown() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.welcome_image_displayed = false;
        let f = tmp.path().join("hi.txt");
        std::fs::write(&f, "hi").unwrap();
        app.editor.open_pinned(&f).unwrap();
        assert!(!app.consume_welcome_image_clear());
    }

    #[test]
    fn clicking_source_control_does_not_block_on_git_and_posts_a_changes_request() {
        // Regression for the user-reported "Source Control click feels
        // sluggish vs the other sidebar icons" bug. The cause was a
        // synchronous `git status --porcelain` shell-out from
        // `refresh_source_control` running on the UI thread, costing
        // 15-50 ms per click on a small repo and 100-300 ms on a large
        // one. The fix routes every git status query through a
        // background worker; the click path now does a single
        // non-blocking `mpsc::Sender::send` and returns within the
        // microsecond range.
        let tmp = tempfile::tempdir().unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(tmp.path()).args(["init", "-q", "-b", "main"]).output();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(tmp.path()).args(["config", "user.email", "a@b"]).status();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(tmp.path()).args(["config", "user.name", "a"]).status();
        std::fs::write(tmp.path().join("dirty.txt"), "x").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Pretend the worker is idle by draining its initial Status
        // response (App::new posts one to seed the badge). This isn't
        // strictly required for the assertion but keeps the test from
        // racing with that startup reply.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let _ = app.drain_git_responses();
        let started = std::time::Instant::now();
        app.set_sidebar_view(SidebarView::SourceControl);
        let elapsed = started.elapsed();
        // The synchronous shell-out used to take 15-50 ms minimum on
        // any real machine; an order of magnitude headroom catches the
        // regression even on a fast CI box.
        assert!(
            elapsed < std::time::Duration::from_millis(5),
            "Source Control click must not block on git; took {elapsed:?}",
        );
        // The Changes request must have landed on the worker — give it
        // up to 2 s to respond, then drain. The response carries the
        // single untracked file we wrote above.
        let mut got_changes = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            if app.drain_git_responses() {
                if !app.source_control.entries.is_empty() {
                    got_changes = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            got_changes,
            "the worker must reply with the dirty file's Changes entry; entries = {:?}",
            app.source_control.entries,
        );
    }

    #[test]
    fn leaving_run_debug_after_emitting_the_icon_arms_a_one_shot_terminal_clear() {
        // Regression for the bug+play headline icon ghosting on top of
        // whichever sidebar replaced Run-Debug. iTerm2's OSC-1337 image
        // cells survive plain SGR overwrites, so the welcome / editor
        // pipelines arm a `terminal.clear()` on the next render to evict
        // them. Run-Debug needs the same gate.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Pretend init_graphics succeeded and the panel emitted the icon
        // while the user was on Run-Debug.
        app.run_debug_icon_osc = Some("\x1b]1337;File=...\x07".to_string());
        app.set_sidebar_view(SidebarView::RunDebug);
        app.run_debug.last_icon_cell = Some((10, 5));
        app.flush_run_debug_icon_overlay();
        assert!(
            app.run_debug_icon_last_emitted.is_some(),
            "test setup: the flush must record the emit position"
        );
        // Switch away. The clear flag must be armed so the main loop's
        // OR chain fires `terminal.clear()` once before the next draw.
        app.set_sidebar_view(SidebarView::Explorer);
        assert!(
            app.consume_run_debug_image_clear(),
            "leaving Run-Debug after emitting the icon must arm exactly one terminal.clear() so iTerm2's image cells are evicted",
        );
        assert!(
            !app.consume_run_debug_image_clear(),
            "the clear request must be one-shot — consuming twice in a row returns false",
        );
    }

    #[test]
    fn flush_run_debug_icon_overlay_is_silent_when_panel_is_not_visible() {
        // Critical invariant: when the user has switched away from
        // Run-Debug, the post-draw flush MUST NOT write any wipe payload
        // (no spaces, no replacement OSC). The prior space-stamp wipe
        // fired AFTER the redraw and erased the freshly-painted next
        // sidebar in those cells. Eviction is now handled exclusively
        // by `terminal.clear()` via `consume_run_debug_image_clear`.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.run_debug_icon_osc = Some("\x1b]1337;File=...\x07".to_string());
        app.set_sidebar_view(SidebarView::RunDebug);
        app.run_debug.last_icon_cell = Some((10, 5));
        app.flush_run_debug_icon_overlay();
        assert_eq!(app.run_debug_icon_last_emitted, Some((10, 5)));
        // Switch away.
        app.set_sidebar_view(SidebarView::Explorer);
        // The flush in this state must clear last_emitted but emit
        // nothing (we can only assert the bookkeeping side effect here;
        // the absence of stdout writes is asserted indirectly by the
        // function returning early before the write_all call).
        app.flush_run_debug_icon_overlay();
        assert_eq!(
            app.run_debug_icon_last_emitted, None,
            "flush on a hidden panel must reset last_emitted so a re-entry primes a fresh emit",
        );
    }

    #[test]
    fn tree_context_menu_on_workspace_root_offers_new_file_and_new_folder_only() {
        // Right-clicking the workspace root row must NOT offer Rename/Delete
        // (the root cannot be renamed or moved to trash from inside croft).
        let tmp = tempfile::tempdir().unwrap();
        let n = dir_node(tmp.path());
        let items = build_tree_context_menu_items(
            Some(&n),
            tmp.path(),
            &[],
            tmp.path(),
            None,
            None,
        );
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(labels, ["New File", "New Folder"]);
    }

    #[test]
    fn bracketed_paste_into_search_input_appends_to_query() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.search.query = String::from("foo");
        app.handle_paste("bar");
        assert_eq!(app.search.query, "foobar");
    }

    #[test]
    fn bracketed_paste_into_search_strips_newlines() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.handle_paste("hello\nworld\r\n");
        assert_eq!(app.search.query, "helloworld");
    }

    #[test]
    fn search_view_focuses_search_input_not_terminal_or_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.show_terminal = true;
        app.focus_pane(Pane::Terminal);
        app.handle_key(key(
            KeyCode::Char('F'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert!(app.focus == Pane::Tree);
        assert!(app.search.focused);
        assert!(!app.tree.focused);
        assert!(!app.terminal().focused);
    }

    #[test]
    fn cycling_focus_away_from_search_clears_search_focus_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.cycle_focus();
        assert!(!app.search.focused);
    }

    #[test]
    fn search_editing_shortcuts_route_to_search_only_when_the_search_input_is_focused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        assert!(app.focus == Pane::Tree, "Search view focuses the side panel");
        app.search.query = String::from("alpha");
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
            .unwrap();
        assert_eq!(app.search.selection_range(), Some((0, 5)));
    }

    #[test]
    fn search_visible_does_not_steal_editing_shortcuts_from_a_focused_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.show_terminal = true;
        app.focus_pane(Pane::Terminal);
        app.search.query = String::from("alpha");
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
            .unwrap();
        assert_eq!(
            app.search.selection_range(),
            None,
            "Cmd+A must act on the focused terminal, not the search query"
        );
    }

    #[test]
    fn search_visible_routes_cmd_v_to_search_only_when_search_input_focused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.clipboard_reader = || Some(String::from("needle"));
        app.set_sidebar_view(SidebarView::Search);

        app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
            .unwrap();

        assert_eq!(app.search.query, "needle");
    }

    #[test]
    fn search_visible_routes_cmd_v_to_terminal_not_search_when_terminal_focused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.clipboard_reader = || Some(String::from("needle"));
        app.set_sidebar_view(SidebarView::Search);
        app.show_terminal = true;
        app.focus_pane(Pane::Terminal);

        app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
            .unwrap();

        assert_eq!(
            app.search.query, "",
            "Cmd+V must paste into the focused terminal, not leak into the search query"
        );
    }

    #[test]
    fn change_workspace_root_into_a_git_subdir_flips_source_control_into_repo_state() {
        // Regression for the user's "Make Root into a .git-bearing folder
        // doesn't update Source Control" bug: the git worker thread was
        // spawned ONCE with the initial root captured by value. After
        // change_workspace_root, the worker kept shelling git status
        // against the OLD root, so the Source Control panel stayed in
        // its no-repo empty state even though the new root was a real
        // git tree.
        let outer = tempfile::tempdir().unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(["init", "-q", "-b", "main"])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(["config", "user.email", "a@b"])
            .status();
        let _ = std::process::Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(["config", "user.name", "a"])
            .status();
        let mut app = App::new(outer.path().to_path_buf()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
        let _ = app.drain_git_responses();
        assert!(
            !app.source_control.status.in_repo,
            "preconditions: outer dir must NOT be a git repo"
        );

        app.change_workspace_root(repo.clone());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut flipped = false;
        while std::time::Instant::now() < deadline {
            if app.drain_git_responses() {
                if app.source_control.status.in_repo {
                    flipped = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            flipped,
            "after Make Root into a real git tree the worker must re-query against the new root and the Source Control panel must report in_repo=true; saw status={:?}",
            app.source_control.status,
        );
        assert_eq!(app.source_control.status.branch.as_deref(), Some("main"));
    }

    fn make_committed_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(p)
            .args(["config", "user.email", "a@b"])
            .status();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(p)
            .args(["config", "user.name", "a"])
            .status();
        std::fs::write(p.join("seed.txt"), b"seed\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(p)
            .args(["add", "seed.txt"])
            .status();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(p)
            .args(["commit", "-q", "-m", "init"])
            .status();
        tmp
    }

    fn wait_for_changes(app: &mut App, predicate: impl Fn(&App) -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut next_kick = std::time::Instant::now();
        while std::time::Instant::now() < deadline {
            if std::time::Instant::now() >= next_kick {
                app.refresh_source_control();
                next_kick = std::time::Instant::now() + std::time::Duration::from_millis(200);
            }
            app.drain_git_responses();
            if predicate(app) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        predicate(app)
    }

    #[test]
    fn stage_source_control_entry_runs_git_add_and_flips_kind_to_staged() {
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        assert!(
            wait_for_changes(&mut app, |a| a
                .source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt"
                    && e.kind == crate::git::ChangeKind::Modified)),
            "preconditions: seed.txt must appear as Modified"
        );
        let idx = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "seed.txt")
            .unwrap();
        app.stage_source_control_entry(idx);
        assert!(
            wait_for_changes(&mut app, |a| a
                .source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt"
                    && e.kind == crate::git::ChangeKind::StagedModified)),
            "after stage_source_control_entry the file must read as StagedModified; saw {:?}",
            app.source_control.entries,
        );
    }

    #[test]
    fn request_discard_source_control_entry_only_opens_a_confirm_modal_without_touching_disk() {
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt")
        });
        let idx = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "seed.txt")
            .unwrap();
        app.request_discard_source_control_entry(idx);
        assert!(
            app.pending_discard.is_some(),
            "Discard click must request user confirmation rather than discarding immediately"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
            "changed\n",
            "request alone must not touch the working tree"
        );
    }

    #[test]
    fn confirming_discard_for_modified_file_restores_head_content() {
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt")
        });
        let idx = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "seed.txt")
            .unwrap();
        app.request_discard_source_control_entry(idx);
        app.confirm_pending_discard();
        assert!(app.pending_discard.is_none(), "modal must close after confirm");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
            "seed\n",
            "discard must restore HEAD content of a Modified file"
        );
    }

    #[test]
    fn confirming_discard_for_untracked_file_deletes_it_from_disk() {
        let tmp = make_committed_repo();
        let new_file = tmp.path().join("scratch.txt");
        std::fs::write(&new_file, b"untracked\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "scratch.txt"
                    && e.kind == crate::git::ChangeKind::Untracked)
        });
        let idx = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "scratch.txt")
            .unwrap();
        app.request_discard_source_control_entry(idx);
        assert!(matches!(
            app.pending_discard.as_ref(),
            Some(pd) if pd.untracked
        ));
        app.confirm_pending_discard();
        assert!(
            !new_file.exists(),
            "discard on an Untracked entry must delete the file from disk"
        );
    }

    #[test]
    fn cancel_pending_discard_clears_modal_without_acting() {
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt")
        });
        let idx = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "seed.txt")
            .unwrap();
        app.request_discard_source_control_entry(idx);
        app.cancel_pending_discard();
        assert!(app.pending_discard.is_none());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
            "changed\n",
            "cancel must NOT touch the working tree"
        );
    }

    #[test]
    fn clicking_the_stage_icon_on_a_selected_unstaged_row_stages_that_entry() {
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt")
        });
        app.set_sidebar_view(SidebarView::SourceControl);
        let backend = ratatui::backend::TestBackend::new(80, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let idx = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "seed.txt")
            .unwrap();
        app.source_control.selected_change = Some(idx);
        term.draw(|f| app.render(f)).unwrap();
        let actions = app
            .source_control
            .last_row_actions
            .expect("selected unstaged row must expose action rects");
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: actions.stage.x,
            row: actions.stage.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(
            wait_for_changes(&mut app, |a| a
                .source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt"
                    && e.kind == crate::git::ChangeKind::StagedModified)),
            "clicking the stage cell must run `git add` against that entry"
        );
    }

    #[test]
    fn clicking_the_discard_icon_on_a_selected_unstaged_row_opens_the_confirm_modal() {
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt")
        });
        app.set_sidebar_view(SidebarView::SourceControl);
        let backend = ratatui::backend::TestBackend::new(80, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let idx = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "seed.txt")
            .unwrap();
        app.source_control.selected_change = Some(idx);
        term.draw(|f| app.render(f)).unwrap();
        let actions = app
            .source_control
            .last_row_actions
            .expect("selected unstaged row must expose action rects");
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: actions.discard.x,
            row: actions.discard.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(
            app.pending_discard.is_some(),
            "discard click must arm the confirm modal (NEVER discard silently)"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
            "changed\n",
            "discard click alone must NOT touch the working tree until the user confirms"
        );
    }

    #[test]
    fn cmd_a_in_source_control_pane_selects_every_change_row() {
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        std::fs::write(tmp.path().join("scratch.txt"), b"new\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2);
        app.set_sidebar_view(SidebarView::SourceControl);
        let n = app.source_control.entries.len();
        app.handle_source_control_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
        assert_eq!(
            app.source_control.multi_selection.len(),
            n,
            "Cmd+A in SC must populate multi_selection with every entry"
        );
    }

    #[test]
    fn cmd_s_through_handle_key_dispatches_to_source_control_not_to_editor_save() {
        // Regression for the user's "Cmd+S didn't react" report: the
        // global `is_save_key` intercept used to fire BEFORE the SC
        // handler, so Cmd+S called `App::save` (a no-op when no editor
        // file was open) and the user's selected change never moved to
        // the index. Route through `handle_key` (not `handle_source_control_key`)
        // to exercise the full dispatcher.
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt")
        });
        app.set_sidebar_view(SidebarView::SourceControl);
        app.source_control.select_all_changes();
        app.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
            .unwrap();
        assert!(
            wait_for_changes(&mut app, |a| a
                .source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt"
                    && e.kind.section() == crate::git::ChangeSection::Staged)),
            "Cmd+S routed through handle_key must reach the SC stage handler, not the global save key intercept"
        );
    }

    #[test]
    fn staging_a_filename_with_a_space_works_after_porcelain_dequoting() {
        // Regression for "fatal: pathspec '\"linked_list copy.py\"' did
        // not match any files": porcelain output wraps filenames with
        // spaces in double quotes, and we used to feed those quotes
        // straight into `git add`. The parser now dequotes; the staging
        // path must end at the real filename.
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("linked_list copy.py"), b"x\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "linked_list copy.py")
        });
        assert!(
            !app.source_control
                .entries
                .iter()
                .any(|e| e.path.starts_with('"') || e.path.ends_with('"')),
            "no entry's path may still carry the porcelain quoting wrapper"
        );
        let idx = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "linked_list copy.py")
            .unwrap();
        app.stage_source_control_entry(idx);
        assert!(
            wait_for_changes(&mut app, |a| a
                .source_control
                .entries
                .iter()
                .any(|e| e.path == "linked_list copy.py"
                    && e.kind.section() == crate::git::ChangeSection::Staged)),
            "Staging a file whose name contains a space must succeed; saw status={:?}",
            app.status,
        );
    }

    #[test]
    fn cmd_s_in_source_control_pane_stages_every_multi_selected_entry() {
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        std::fs::write(tmp.path().join("scratch.txt"), b"new\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2);
        app.set_sidebar_view(SidebarView::SourceControl);
        app.source_control.select_all_changes();
        app.handle_source_control_key(key(KeyCode::Char('s'), KeyModifiers::SUPER));
        assert!(
            wait_for_changes(&mut app, |a| {
                let kinds: Vec<_> = a
                    .source_control
                    .entries
                    .iter()
                    .map(|e| e.kind.section())
                    .collect();
                !kinds.is_empty()
                    && kinds
                        .iter()
                        .all(|s| *s == crate::git::ChangeSection::Staged)
            }),
            "Cmd+S with multi-selection must stage every selected entry; saw {:?}",
            app.source_control.entries,
        );
    }

    #[test]
    fn cmd_s_with_single_selection_stages_only_that_entry() {
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        std::fs::write(tmp.path().join("scratch.txt"), b"new\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2);
        app.set_sidebar_view(SidebarView::SourceControl);
        let scratch_idx = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "scratch.txt")
            .unwrap();
        // Single click resets multi_selection to just this row.
        app.source_control.multi_selection.clear();
        app.source_control.multi_selection.insert(scratch_idx);
        app.source_control.selected_change = Some(scratch_idx);
        app.handle_source_control_key(key(KeyCode::Char('s'), KeyModifiers::SUPER));
        assert!(
            wait_for_changes(&mut app, |a| a
                .source_control
                .entries
                .iter()
                .any(|e| e.path == "scratch.txt"
                    && e.kind.section() == crate::git::ChangeSection::Staged)),
            "scratch.txt must have been staged"
        );
        assert!(
            app.source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt"
                    && e.kind == crate::git::ChangeKind::Modified),
            "seed.txt must remain unstaged (single-row stage scope)"
        );
    }

    #[test]
    fn cmd_enter_in_source_control_falls_back_to_plain_commit_without_pushing() {
        // iTerm2 swallows Cmd+Enter as Toggle Fullscreen so the chord never
        // reaches croft. Terminals that DO deliver it should treat Cmd as
        // a no-op modifier — Enter alone commits, Ctrl+Enter pushes. The
        // SC handler's plain `KeyCode::Enter` match arm catches the
        // residual Cmd+Enter and calls `commit_source_control` (no push).
        let tmp = make_committed_repo();
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt")
        });
        app.set_sidebar_view(SidebarView::SourceControl);
        app.source_control.message = "fix: plain cmd-enter".to_string();
        app.source_control.message_cursor = app.source_control.message.chars().count();
        app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::SUPER));
        let log = std::process::Command::new("git")
            .args(["-C"]).arg(tmp.path())
            .args(["log", "--oneline"])
            .output()
            .unwrap();
        let log_out = String::from_utf8_lossy(&log.stdout);
        assert!(
            log_out.contains("fix: plain cmd-enter"),
            "Cmd+Enter must still commit locally (push is bound to Ctrl+Enter only): log={log_out:?}"
        );
    }

    #[test]
    fn ctrl_enter_in_source_control_commits_and_pushes() {
        let tmp = make_committed_repo();
        // Wire a bare remote and set it as origin so `git push` has
        // somewhere to send.
        let remote = tempfile::tempdir().unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(remote.path())
            .args(["init", "--bare", "-q", "-b", "main"])
            .status();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(tmp.path())
            .args(["remote", "add", "origin"])
            .arg(remote.path())
            .status();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(tmp.path())
            .args(["push", "-u", "origin", "main"])
            .status();
        // Make a tracked change and stage it so commit-all-tracked has
        // something to commit.
        std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt")
        });
        app.set_sidebar_view(SidebarView::SourceControl);
        app.source_control.message = "fix: bump seed".to_string();
        app.source_control.message_cursor = app.source_control.message.chars().count();
        app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::CONTROL));
        // Verify the remote received the commit (its log now reports two
        // commits — the initial push + our new one).
        let log = std::process::Command::new("git")
            .args(["-C"]).arg(remote.path())
            .args(["log", "--oneline"])
            .output()
            .unwrap();
        let log_out = String::from_utf8_lossy(&log.stdout);
        assert!(
            log_out.lines().count() >= 2,
            "Cmd+Enter must have committed AND pushed (remote log: {log_out:?})"
        );
        assert!(
            log_out.contains("fix: bump seed"),
            "remote's log must include the new commit's subject"
        );
    }

    #[test]
    fn opening_a_modified_file_diff_parks_scroll_on_the_first_change_hunk() {
        let tmp = make_committed_repo();
        let big = (0..50)
            .map(|i| format!("line {i}\n"))
            .collect::<String>();
        std::fs::write(tmp.path().join("seed.txt"), &big).unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(tmp.path())
            .args(["add", "seed.txt"])
            .status();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(tmp.path())
            .args(["commit", "-q", "-m", "big seed"])
            .status();
        // Now change a line deep in the file so the first hunk lives well
        // past row 0.
        let mut lines: Vec<String> = big.lines().map(str::to_string).collect();
        lines[30] = "edited-30".to_string();
        std::fs::write(tmp.path().join("seed.txt"), lines.join("\n") + "\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt")
        });
        let idx = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "seed.txt")
            .unwrap();
        app.open_source_control_entry(idx);
        let diff = app
            .editor
            .diff
            .as_ref()
            .expect("opening a Modified entry must build a diff");
        assert!(
            diff.scroll >= 25 && diff.scroll <= 30,
            "diff must auto-scroll near the change (row ~30, with 2-row context above); saw scroll={}",
            diff.scroll,
        );
    }

    #[test]
    fn f7_in_the_diff_view_jumps_to_the_next_change_hunk() {
        let tmp = make_committed_repo();
        // Build a multi-hunk file: edits at lines 10 and 40.
        let original: String = (0..50)
            .map(|i| format!("line {i}\n"))
            .collect();
        std::fs::write(tmp.path().join("seed.txt"), &original).unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(tmp.path())
            .args(["add", "seed.txt"])
            .status();
        let _ = std::process::Command::new("git")
            .args(["-C"]).arg(tmp.path())
            .args(["commit", "-q", "-m", "seed"])
            .status();
        let mut edited: Vec<String> = original.lines().map(str::to_string).collect();
        edited[10] = "edited-10".to_string();
        edited[40] = "edited-40".to_string();
        std::fs::write(tmp.path().join("seed.txt"), edited.join("\n") + "\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        wait_for_changes(&mut app, |a| {
            a.source_control
                .entries
                .iter()
                .any(|e| e.path == "seed.txt")
        });
        let idx = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "seed.txt")
            .unwrap();
        app.open_source_control_entry(idx);
        let first_scroll = app.editor.diff.as_ref().unwrap().scroll;
        // F7 jumps to the next hunk.
        app.handle_editor_key(key(KeyCode::F(7), KeyModifiers::NONE));
        let next_scroll = app.editor.diff.as_ref().unwrap().scroll;
        assert!(
            next_scroll > first_scroll,
            "F7 must advance scroll past the current hunk (saw first={first_scroll}, next={next_scroll})"
        );
        // Shift+F7 walks back.
        app.handle_editor_key(key(KeyCode::F(7), KeyModifiers::SHIFT));
        let prev_scroll = app.editor.diff.as_ref().unwrap().scroll;
        assert!(
            prev_scroll < next_scroll,
            "Shift+F7 must walk back to the prior hunk (saw next={next_scroll}, prev={prev_scroll})"
        );
    }

    #[test]
    fn focused_source_control_input_drives_a_visible_caret_at_the_message_cursor() {
        // Regression for the user's "I don't see the caret blinking in the
        // Source Control message box, so I have no clue where it is at any
        // point in time" report. After focusing the SC panel in a real
        // repo, `cursor_should_be_visible()` must report true and the
        // caret cell must point inside the rendered input box.
        let tmp = tempfile::tempdir().unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C"])
            .arg(tmp.path())
            .args(["init", "-q", "-b", "main"])
            .output();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            app.drain_git_responses();
            if app.source_control.status.in_repo {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            app.source_control.status.in_repo,
            "preconditions: tempdir must register as a git repo"
        );

        app.set_sidebar_view(SidebarView::SourceControl);
        let backend = ratatui::backend::TestBackend::new(80, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        app.handle_source_control_key(key(KeyCode::Char('f'), KeyModifiers::NONE));
        app.handle_source_control_key(key(KeyCode::Char('i'), KeyModifiers::NONE));
        app.handle_source_control_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        term.draw(|f| app.render(f)).unwrap();

        assert!(
            app.cursor_should_be_visible(),
            "caret must be visible when the Source Control panel owns focus and the input is laid out"
        );
        let (cx, cy) = app
            .source_control
            .cursor_screen_pos()
            .expect("focused SC panel must expose a caret cell");
        let r = app.source_control.last_input_area;
        assert!(
            cx >= r.x + 2 && cx < r.x + r.width - 1 && cy == r.y + 1,
            "caret cell ({cx},{cy}) must sit inside the input's text row (input rect {r:?})"
        );
        assert_eq!(
            cx,
            r.x + 2 + 3,
            "caret must advance one cell per typed char (3 chars typed → +3 cells past the first text cell)"
        );
    }

    #[test]
    fn editor_shift_alt_down_duplicates_the_current_line_via_handle_key() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Editor);
        app.editor.lines = vec![String::from("alpha"), String::from("beta")];
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 3;
        app.handle_key(key(KeyCode::Down, KeyModifiers::ALT | KeyModifiers::SHIFT))
            .unwrap();
        assert_eq!(app.editor.lines, vec!["alpha", "alpha", "beta"]);
        assert_eq!(
            (app.editor.cursor_row, app.editor.cursor_col),
            (1, 3),
            "cursor must follow onto the duplicated line"
        );
    }

    #[test]
    fn editor_shift_alt_up_duplicates_the_current_line_via_handle_key() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Editor);
        app.editor.lines = vec![String::from("alpha"), String::from("beta")];
        app.editor.cursor_row = 1;
        app.editor.cursor_col = 2;
        app.handle_key(key(KeyCode::Up, KeyModifiers::ALT | KeyModifiers::SHIFT))
            .unwrap();
        assert_eq!(app.editor.lines, vec!["alpha", "beta", "beta"]);
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 2));
    }

    #[test]
    fn editor_alt_right_jumps_one_word_forward() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Editor);
        app.editor.lines = vec![String::from("hello world rest")];
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 0;
        app.handle_key(key(KeyCode::Right, KeyModifiers::ALT)).unwrap();
        assert_eq!(app.editor.cursor_col, 5);
        app.handle_key(key(KeyCode::Right, KeyModifiers::ALT)).unwrap();
        assert_eq!(app.editor.cursor_col, 11);
    }

    fn editor_app_with_lines(lines: &[&str]) -> App {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Editor);
        app.editor.lines = lines.iter().map(|s| (*s).to_string()).collect();
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 0;
        app
    }

    #[test]
    fn editor_cmd_gg_jumps_to_top_of_file() {
        let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
        app.editor.cursor_row = 3;
        app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER)).unwrap();
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 0));
    }

    #[test]
    fn editor_cmd_g_then_plain_g_also_jumps_to_top() {
        let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
        app.editor.cursor_row = 3;
        app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('g'), KeyModifiers::NONE)).unwrap();
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 0));
    }

    #[test]
    fn editor_cmd_g_count_g_jumps_to_specific_line() {
        let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
        app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('3'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('g'), KeyModifiers::NONE)).unwrap();
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (2, 0));
    }

    #[test]
    fn editor_cmd_g_multidigit_count_g_jumps_to_specific_line() {
        let mut app = editor_app_with_lines(&(0..30).map(|_| "x").collect::<Vec<_>>());
        app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('1'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('2'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('g'), KeyModifiers::NONE)).unwrap();
        assert_eq!(app.editor.cursor_row, 11);
    }

    #[test]
    fn editor_cmd_shift_g_jumps_to_bottom_of_file() {
        let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
        app.handle_key(key(
            KeyCode::Char('G'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(app.editor.cursor_row, 3);
        assert_ne!(
            app.sidebar_view,
            SidebarView::SourceControl,
            "cmd+shift+g in editor pane must NOT hijack to source control"
        );
    }

    #[test]
    fn editor_cmd_dd_deletes_current_line() {
        let mut app = editor_app_with_lines(&["alpha", "beta", "gamma"]);
        app.editor.cursor_row = 1;
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER)).unwrap();
        assert_eq!(app.editor.lines, vec!["alpha", "gamma"]);
    }

    #[test]
    fn editor_cmd_d_count_d_deletes_n_lines() {
        let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
        app.editor.cursor_row = 1;
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('3'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE)).unwrap();
        assert_eq!(app.editor.lines, vec!["a", "e"]);
    }

    #[test]
    fn editor_cmd_yy_does_not_modify_buffer() {
        let mut app = editor_app_with_lines(&["alpha", "beta", "gamma"]);
        app.editor.cursor_row = 1;
        app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER)).unwrap();
        assert_eq!(app.editor.lines, vec!["alpha", "beta", "gamma"]);
        assert!(app.status.starts_with("Yanked"));
    }

    #[test]
    fn editor_chord_cancels_on_unrelated_key() {
        let mut app = editor_app_with_lines(&["alpha"]);
        app.editor.cursor_col = 5;
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE)).unwrap();
        assert_eq!(app.editor.lines, vec!["alphax"]);
    }

    #[test]
    fn editor_ctrl_a_jumps_to_start_of_line() {
        let mut app = editor_app_with_lines(&["hello world"]);
        app.editor.cursor_col = 7;
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(app.editor.cursor_col, 0);
    }

    #[test]
    fn editor_ctrl_e_jumps_to_end_of_line() {
        let mut app = editor_app_with_lines(&["hello world"]);
        app.editor.cursor_col = 3;
        app.handle_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(app.editor.cursor_col, 11);
    }

    #[test]
    fn editor_ctrl_k_kills_to_end_of_line() {
        let mut app = editor_app_with_lines(&["hello world", "next"]);
        app.editor.cursor_col = 5;
        app.handle_key(key(KeyCode::Char('k'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(app.editor.lines, vec!["hello", "next"]);
    }

    #[test]
    fn editor_cmd_count_gg_jumps_to_line_with_leading_count() {
        let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
        app.handle_key(key(KeyCode::Char('3'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER)).unwrap();
        assert_eq!(app.editor.cursor_row, 2);
    }

    #[test]
    fn editor_cmd_count_dd_deletes_n_lines_with_leading_count() {
        let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
        app.editor.cursor_row = 1;
        app.handle_key(key(KeyCode::Char('2'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER)).unwrap();
        assert_eq!(app.editor.lines, vec!["a", "d", "e"]);
    }

    #[test]
    fn editor_cmd_count_yy_yanks_n_lines_with_leading_count() {
        let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
        app.editor.cursor_row = 0;
        app.handle_key(key(KeyCode::Char('2'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER)).unwrap();
        assert_eq!(app.editor.lines, vec!["a", "b", "c", "d"]);
        assert!(app.status.contains("Yanked 2"));
    }

    #[test]
    fn editor_cmd_count_then_shift_g_jumps_to_specific_line() {
        let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
        app.handle_key(key(KeyCode::Char('4'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(
            KeyCode::Char('G'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(app.editor.cursor_row, 3);
    }

    #[test]
    fn editor_ctrl_u_kills_to_start_of_line() {
        let mut app = editor_app_with_lines(&["hello world"]);
        app.editor.cursor_col = 6;
        app.handle_key(key(KeyCode::Char('u'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(app.editor.lines, vec!["world"]);
        assert_eq!(app.editor.cursor_col, 0);
    }

    #[test]
    fn editor_cmd_o_opens_line_below_with_indent() {
        let mut app = editor_app_with_lines(&["    foo", "bar"]);
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 4;
        app.handle_key(key(KeyCode::Char('o'), KeyModifiers::SUPER)).unwrap();
        assert_eq!(app.editor.lines, vec!["    foo", "    ", "bar"]);
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 4));
    }

    #[test]
    fn editor_cmd_shift_o_opens_line_above_with_indent() {
        let mut app = editor_app_with_lines(&["foo", "    bar"]);
        app.editor.cursor_row = 1;
        app.editor.cursor_col = 6;
        app.handle_key(key(
            KeyCode::Char('O'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(app.editor.lines, vec!["foo", "    ", "    bar"]);
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 4));
    }

    #[test]
    fn f1_opens_the_shortcuts_modal_from_any_focus() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Editor);
        app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE)).unwrap();
        assert!(app.shortcuts_modal.is_some(), "F1 must open the shortcuts panel");
    }

    #[test]
    fn esc_closes_the_shortcuts_modal_and_restores_normal_input() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE)).unwrap();
        assert!(app.shortcuts_modal.is_none(), "Esc must dismiss the shortcuts panel");
    }

    #[test]
    fn shortcuts_modal_swallows_keys_so_editor_does_not_receive_them() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Editor);
        app.editor.lines = vec![String::from("hello")];
        app.editor.cursor_col = 5;
        app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE)).unwrap();
        assert_eq!(
            app.editor.lines,
            vec!["hello"],
            "while the shortcuts modal is open, typed letters must not leak into the editor buffer"
        );
    }

    #[test]
    fn opening_the_shortcuts_modal_arms_one_image_clear_so_iterm_evicts_cached_osc_1337_cells() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE)).unwrap();
        assert!(
            app.consume_shortcuts_image_clear(),
            "F1 must arm terminal.clear() so iTerm2's image cache (activity bar icons, welcome wordmark, editor preview) is wiped before the modal paints, otherwise those cached image cells bleed through the modal text"
        );
        assert!(
            !app.consume_shortcuts_image_clear(),
            "the flag is one-shot — a second consume call must return false so the driver does not re-clear every frame"
        );
    }

    #[test]
    fn closing_the_shortcuts_modal_arms_image_clear_and_re_dirties_overlays() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE)).unwrap();
        let _ = app.consume_shortcuts_image_clear();
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE)).unwrap();
        assert!(
            app.consume_shortcuts_image_clear(),
            "Esc must also arm terminal.clear() so the modal's text cells get wiped and the activity bar / welcome wordmark / hero icons can be re-emitted cleanly"
        );
        assert!(app.activity_overlay_dirty);
        assert!(app.welcome_overlay_dirty);
        assert!(app.no_repo_hero_overlay_dirty);
    }

    #[test]
    fn ssh_empty_state_overlay_arms_image_clear_when_user_switches_off_remote_view() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Simulate the post-emit state without actually writing OSC bytes.
        app.ssh_empty_state_displayed = true;
        app.set_sidebar_view(SidebarView::Explorer);
        app.flush_ssh_empty_state_overlay();
        assert!(
            app.consume_ssh_empty_state_image_clear(),
            "leaving the Remote view after the illustration was painted must arm one terminal.clear() so iTerm2 evicts the cached image cells — otherwise the server-rack PNG ghosts onto the Explorer tree the user just clicked to"
        );
        assert!(!app.ssh_empty_state_displayed);
    }

    #[test]
    fn ssh_empty_state_overlay_arms_image_clear_when_modal_opens_on_top() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Remote);
        app.ssh_empty_state_displayed = true;
        app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE)).unwrap();
        app.flush_ssh_empty_state_overlay();
        assert!(
            app.consume_ssh_empty_state_image_clear(),
            "F1 opening the shortcuts modal over the SSH empty state must arm an image-clear so the illustration doesn't bleed through the modal text"
        );
    }

    #[test]
    fn activity_overlay_emission_is_suppressed_while_the_shortcuts_modal_is_open() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let before = app.pending_activity_image_overlays().len();
        app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE)).unwrap();
        let during = app.pending_activity_image_overlays().len();
        assert_eq!(
            during, 0,
            "while the modal is open the activity-bar OSC-1337 emitter must return an empty overlay list, otherwise the icons re-paint over the modal every 2 seconds via the keep-alive timer; before opening saw {before} overlays"
        );
    }

    #[test]
    fn shortcuts_modal_scrolls_down_then_back_to_top_via_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE)).unwrap();
        let modal = app.shortcuts_modal.as_mut().unwrap();
        modal.last_inner_height = 5;
        modal.last_content_height = 50;
        app.handle_key(key(KeyCode::PageDown, KeyModifiers::NONE)).unwrap();
        assert_eq!(app.shortcuts_modal.as_ref().unwrap().scroll, 10);
        app.handle_key(key(KeyCode::Home, KeyModifiers::NONE)).unwrap();
        assert_eq!(app.shortcuts_modal.as_ref().unwrap().scroll, 0);
    }

    #[test]
    fn cmd_p_opens_the_file_finder_modal_for_quick_open() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        assert!(
            app.file_finder.is_some(),
            "Cmd+P must open the Quick Open file finder modal so the user can type a name and jump to a file the way VS Code does"
        );
    }

    #[test]
    fn ctrl_p_opens_the_file_finder_modal_on_systems_without_super() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL)).unwrap();
        assert!(
            app.file_finder.is_some(),
            "Ctrl+P must also open the Quick Open finder so the chord works on Linux / iTerm2 sessions that report CONTROL rather than SUPER"
        );
    }

    #[test]
    fn plain_p_keystroke_does_not_open_the_file_finder() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE)).unwrap();
        assert!(
            app.file_finder.is_none(),
            "an unmodified 'p' must reach the editor / tree typeahead and never trigger the modal"
        );
    }

    #[test]
    fn esc_closes_the_file_finder_modal() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE)).unwrap();
        assert!(app.file_finder.is_none(), "Esc must dismiss the file finder");
    }

    #[test]
    fn typing_in_file_finder_fuzzy_filters_the_result_list() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub").join("beta.rs"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('e'), KeyModifiers::NONE)).unwrap();
        let finder = app.file_finder.as_ref().expect("finder must still be open while typing");
        let names: Vec<String> = finder
            .visible_results()
            .iter()
            .map(|r| r.entry.rel.clone())
            .collect();
        assert!(
            names.iter().any(|n| n == "sub/beta.rs"),
            "typing 'be' must keep sub/beta.rs in the result list — got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "alpha.rs"),
            "typing 'be' must drop alpha.rs because 'be' is not a subsequence of 'alpha.rs' — got {names:?}"
        );
    }

    #[test]
    fn enter_in_file_finder_opens_the_selected_file_and_closes_the_modal() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "fn main() {}").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        assert!(app.file_finder.is_none(), "Enter must close the modal after opening the file");
        let opened = app.editor.path.as_ref().expect("the active tab must now have a path");
        assert_eq!(
            opened.file_name().and_then(|n| n.to_str()),
            Some("alpha.rs"),
            "Enter must open the highlighted entry (alpha.rs) into the active editor tab"
        );
    }

    #[test]
    fn down_arrow_moves_selection_in_the_file_finder() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a1.rs"), "").unwrap();
        std::fs::write(tmp.path().join("a2.rs"), "").unwrap();
        std::fs::write(tmp.path().join("a3.rs"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        let before = app.file_finder.as_ref().unwrap().selected_index();
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE)).unwrap();
        let after = app.file_finder.as_ref().unwrap().selected_index();
        assert_eq!(after, before + 1, "Down arrow must move the selection cursor one row");
    }

    #[test]
    fn backspace_removes_a_character_from_the_finder_query() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE)).unwrap();
        assert_eq!(app.file_finder.as_ref().unwrap().query, "a");
    }

    #[test]
    fn opening_the_file_finder_arms_one_image_clear_so_iterm_evicts_cached_osc_1337_cells() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        assert!(
            app.consume_file_finder_image_clear(),
            "Cmd+P must arm terminal.clear() so iTerm2's image cache (activity bar icons, welcome wordmark, editor preview) is wiped before the modal paints, otherwise those cached image cells bleed through the file list"
        );
        assert!(
            !app.consume_file_finder_image_clear(),
            "the flag is one-shot — a second consume call must return false so the driver does not re-clear every frame"
        );
    }

    #[test]
    fn cmd_shift_e_jumps_to_explorer_from_any_pane() {
        let mut app = editor_app_with_lines(&["x"]);
        app.set_sidebar_view(SidebarView::Search);
        app.show_tree = false;
        app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER | KeyModifiers::SHIFT)).unwrap();
        assert_eq!(app.sidebar_view, SidebarView::Explorer);
        assert!(app.show_tree, "Cmd+Shift+E must un-collapse the sidebar if it was hidden");
    }

    #[test]
    fn cmd_shift_s_jumps_to_source_control_even_when_editor_is_focused() {
        let mut app = editor_app_with_lines(&["x"]);
        app.focus_pane(Pane::Editor);
        app.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER | KeyModifiers::SHIFT)).unwrap();
        assert_eq!(
            app.sidebar_view,
            SidebarView::SourceControl,
            "Cmd+Shift+S must jump to Source Control even from the editor (unlike Cmd+Shift+G, which is overloaded by goto-bottom in the editor)"
        );
    }

    #[test]
    fn cmd_shift_d_jumps_to_run_and_debug_from_any_pane() {
        let mut app = editor_app_with_lines(&["x"]);
        app.focus_pane(Pane::Editor);
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER | KeyModifiers::SHIFT)).unwrap();
        assert_eq!(app.sidebar_view, SidebarView::RunDebug);
    }

    #[test]
    fn cmd_shift_r_jumps_to_remote_from_any_pane() {
        let mut app = editor_app_with_lines(&["x"]);
        app.focus_pane(Pane::Editor);
        app.handle_key(key(KeyCode::Char('r'), KeyModifiers::SUPER | KeyModifiers::SHIFT)).unwrap();
        assert_eq!(app.sidebar_view, SidebarView::Remote);
    }

    #[test]
    fn cmd_shift_t_focuses_the_terminal_pane_from_the_editor() {
        let mut app = editor_app_with_lines(&["x"]);
        app.focus_pane(Pane::Editor);
        app.handle_key(key(KeyCode::Char('t'), KeyModifiers::SUPER | KeyModifiers::SHIFT)).unwrap();
        assert!(
            matches!(app.focus, Pane::Terminal),
            "Cmd+Shift+T must move focus to the Terminal pane from the editor so the user can start typing in the shell without reaching for the mouse"
        );
    }

    #[test]
    fn cmd_shift_t_focuses_the_terminal_pane_from_the_tree() {
        let mut app = editor_app_with_lines(&["x"]);
        app.focus_pane(Pane::Tree);
        app.handle_key(key(KeyCode::Char('t'), KeyModifiers::SUPER | KeyModifiers::SHIFT)).unwrap();
        assert!(
            matches!(app.focus, Pane::Terminal),
            "Cmd+Shift+T must move focus to the Terminal pane even when the Explorer is focused"
        );
    }

    #[test]
    fn cmd_shift_t_unhides_the_terminal_if_it_was_collapsed() {
        let mut app = editor_app_with_lines(&["x"]);
        app.focus_pane(Pane::Editor);
        app.show_terminal = false;
        app.handle_key(key(KeyCode::Char('T'), KeyModifiers::SUPER | KeyModifiers::SHIFT)).unwrap();
        assert!(
            app.show_terminal,
            "Cmd+Shift+T must un-hide the terminal if it was collapsed (Ctrl+J) — otherwise the focus jump lands in a zero-row pane"
        );
        assert!(matches!(app.focus, Pane::Terminal));
    }

    #[test]
    fn ctrl_shift_t_still_splits_the_terminal_and_does_not_double_fire_as_focus() {
        let mut app = editor_app_with_lines(&["x"]);
        app.focus_pane(Pane::Editor);
        let before = app.terminals.len();
        app.handle_key(key(KeyCode::Char('t'), KeyModifiers::CONTROL | KeyModifiers::SHIFT)).unwrap();
        assert_eq!(
            app.terminals.len(),
            before + 1,
            "Ctrl+Shift+T must remain bound to split_terminal — the new Cmd+Shift+T focus chord must not steal it"
        );
    }

    #[test]
    fn terminal_focus_key_predicate_accepts_cmd_shift_t_and_rejects_neighbours() {
        let cmd_shift = KeyModifiers::SUPER | KeyModifiers::SHIFT;
        assert!(is_terminal_focus_key(key(KeyCode::Char('t'), cmd_shift)));
        assert!(is_terminal_focus_key(key(KeyCode::Char('T'), cmd_shift)));
        assert!(
            !is_terminal_focus_key(key(KeyCode::Char('t'), KeyModifiers::SUPER)),
            "Cmd+T (no Shift) is reserved by macOS for New Tab; the focus chord requires Shift"
        );
        assert!(
            !is_terminal_focus_key(key(KeyCode::Char('t'), KeyModifiers::CONTROL | KeyModifiers::SHIFT)),
            "Ctrl+Shift+T must stay routed to is_terminal_split_key, not focus"
        );
        assert!(
            !is_terminal_focus_key(key(
                KeyCode::Char('t'),
                KeyModifiers::SUPER | KeyModifiers::SHIFT | KeyModifiers::ALT
            )),
            "Cmd+Opt+Shift+T must not focus — that chord is reserved (we relocate iTerm2's Restore Closed Session onto it)"
        );
    }

    #[test]
    fn iterm2_image_drag_artefact_does_not_paste_its_path_into_the_editor() {
        // Root cause of the user's "Pasted 67 chars" surprise: when iTerm2
        // hijacks a click-drag on an OSC-1337 activity-bar icon, it writes
        // the decoded PNG to $TMPDIR/iTerm2.<rand>.png and re-injects the
        // path as a bracketed paste. parse_dropped_paths filters those
        // paths out of the IMPORT list, but without an early-return in
        // handle_paste the fallthrough would still insert the raw path
        // text into the focused editor.
        let mut app = editor_app_with_lines(&[""]);
        app.focus_pane(Pane::Editor);
        let tmp = tempfile::tempdir().unwrap();
        let temp_dir = tmp.path().join("T").join("com.googlecode.iterm2");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let fake = temp_dir.join("iTerm2.5AO7K5.png");
        std::fs::write(&fake, b"\x89PNG\r\n\x1a\n").unwrap();
        let payload = format!("{}\n", fake.display());
        app.handle_paste(&payload);
        assert_eq!(
            app.editor.lines,
            vec![""],
            "iTerm2 inline-image drag artefact must be silently dropped at every level — the path string must NOT be pasted as text into the editor body (the user reported a CLAUDE.local.md tab opening with 'Pasted 67 chars' after trying to resize the sidebar near the Explorer icon)"
        );
    }

    #[test]
    fn cmd_f_in_editor_opens_the_inline_find_overlay_not_the_new_file_prompt() {
        let mut app = editor_app_with_lines(&["alpha beta", "alpha gamma"]);
        app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER)).unwrap();
        assert!(
            app.editor_find.is_some(),
            "Cmd+F with the editor focused must open the VS Code-style inline Find bar over the editor pane, not the Explorer 'new file' prompt"
        );
        assert!(
            app.prompt.is_none(),
            "Cmd+F in editor must NOT open the new-file Prompt — that is reserved for the Explorer pane"
        );
    }

    #[test]
    fn cmd_f_in_explorer_still_opens_the_new_file_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Tree);
        app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER)).unwrap();
        assert!(
            app.prompt.is_some(),
            "Cmd+F in the Explorer pane must keep creating a new file — the existing behaviour the user asked us to preserve"
        );
        assert!(
            app.editor_find.is_none(),
            "Cmd+F in the Explorer must NOT open the editor Find overlay; the chord is overloaded by pane"
        );
    }

    #[test]
    fn typing_in_editor_find_jumps_the_cursor_to_the_next_match() {
        let mut app = editor_app_with_lines(&["alpha beta", "alpha gamma"]);
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 0;
        app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('e'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('t'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        assert_eq!(
            (app.editor.cursor_row, app.editor.cursor_col),
            (0, 6),
            "typing 'beta' must jump the cursor to the first 'beta' occurrence — got ({}, {})",
            app.editor.cursor_row,
            app.editor.cursor_col
        );
    }

    #[test]
    fn enter_in_editor_find_jumps_to_the_next_match_and_shift_enter_walks_back() {
        let mut app = editor_app_with_lines(&["alpha", "beta alpha", "alpha"]);
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 0;
        app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('l'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('h'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        assert_eq!(
            app.editor.cursor_row, 1,
            "Enter must jump to the next 'alpha' match on row 1"
        );
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        assert_eq!(
            app.editor.cursor_row, 2,
            "Enter again must walk forward to the row 2 match"
        );
        app.handle_key(key(KeyCode::Enter, KeyModifiers::SHIFT)).unwrap();
        assert_eq!(
            app.editor.cursor_row, 1,
            "Shift+Enter must walk back one match to row 1"
        );
    }

    #[test]
    fn jumping_to_a_match_records_an_active_search_match_so_the_renderer_can_paint_it_orange() {
        let mut app = editor_app_with_lines(&["alpha beta alpha"]);
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 0;
        app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('l'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('h'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        let first = app.editor.active_search_match;
        assert_eq!(
            first,
            Some((0, 0, 5)),
            "typing 'alpha' lands the cursor on the first match (col 0, len 5); active_search_match must record exactly that so the renderer can paint just those 5 cells orange while the other 'alpha' at col 11 keeps the regular yellow — got {first:?}"
        );
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        let second = app.editor.active_search_match;
        assert_eq!(
            second,
            Some((0, 11, 5)),
            "Enter walks to the next match at col 11; the active marker must follow so the previously-orange match resets to yellow and the new cursor row gets the orange band"
        );
    }

    #[test]
    fn esc_closes_the_editor_find_overlay_and_clears_the_highlight() {
        let mut app = editor_app_with_lines(&["alpha"]);
        app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        assert!(app.editor.search_highlight.is_some());
        assert!(
            app.editor.active_search_match.is_some(),
            "typing should land the cursor on a match and arm the active-match marker"
        );
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE)).unwrap();
        assert!(app.editor_find.is_none(), "Esc must close the find bar");
        assert!(
            app.editor.search_highlight.is_none(),
            "Esc must clear the editor's match highlight so leftover dim cells don't survive into normal typing"
        );
        assert!(
            app.editor.active_search_match.is_none(),
            "Esc must also clear the active-match marker so the orange band does not survive the overlay closing"
        );
    }

    #[test]
    fn editor_find_pre_fills_the_query_from_word_under_cursor_on_open() {
        let mut app = editor_app_with_lines(&["alphabet"]);
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 5;
        app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER)).unwrap();
        assert_eq!(
            app.editor_find.as_ref().unwrap().query,
            "alpha",
            "opening Cmd+F with the cursor mid-word must pre-fill the query with the identifier chars to the left of the cursor, matching VS Code"
        );
    }

    #[test]
    fn editor_find_paste_appends_clipboard_to_the_query() {
        let mut app = editor_app_with_lines(&["alpha needle beta"]);
        app.clipboard_reader = || Some(String::from("needle"));
        app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER)).unwrap();
        assert_eq!(
            app.editor_find.as_ref().unwrap().query,
            "needle",
            "Cmd+V inside the find bar must paste from the system clipboard"
        );
    }

    #[test]
    fn picking_a_nested_file_via_cmd_p_expands_every_parent_dir_in_the_explorer_and_selects_the_row() {
        let tmp = tempfile::tempdir().unwrap();
        let nested_dir = tmp.path().join("packages").join("inner").join("citations");
        std::fs::create_dir_all(&nested_dir).unwrap();
        let target = nested_dir.join("storage.py");
        std::fs::write(&target, "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.tree.last_inner.height = 20;
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('t'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Char('o'), KeyModifiers::NONE)).unwrap();
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        let selected = app
            .tree
            .selected_path()
            .map(|p| p.to_path_buf())
            .expect("a row must be selected after Enter on the finder");
        assert_eq!(
            selected, target,
            "the explorer cursor must land on the picked file, not stay on whichever row was selected before — got {selected:?}"
        );
        let visible_paths: Vec<String> = app
            .tree
            .nodes
            .iter()
            .map(|n| n.path.display().to_string())
            .collect();
        for expected_parent in [
            tmp.path().join("packages"),
            tmp.path().join("packages").join("inner"),
            tmp.path().join("packages").join("inner").join("citations"),
        ] {
            let s = expected_parent.display().to_string();
            assert!(
                visible_paths.contains(&s),
                "parent dir {s} must appear in the flattened node list — i.e. every ancestor of the picked file must be expanded so the row is reachable: got {visible_paths:?}"
            );
        }
    }

    #[test]
    fn bracketed_paste_into_open_file_finder_appends_to_the_query_not_the_editor() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Editor);
        app.editor.lines = vec![String::from("hello")];
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        app.handle_paste("alpha");
        assert_eq!(
            app.file_finder.as_ref().unwrap().query,
            "alpha",
            "an iTerm2 bracketed paste while the finder is open must land in the finder query so the user can Cmd+P then Cmd+V a filename"
        );
        assert_eq!(
            app.editor.lines,
            vec!["hello"],
            "the editor pane behind the modal must not receive the pasted text"
        );
    }

    #[test]
    fn cmd_v_keystroke_in_file_finder_reads_clipboard_into_the_query() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.clipboard_reader = || Some(String::from("alpha"));
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER)).unwrap();
        assert_eq!(
            app.file_finder.as_ref().unwrap().query,
            "alpha",
            "Cmd+V inside the finder must read the system clipboard and append every char to the query — this covers terminals that disable bracketed paste or route Cmd+V via CSI-u"
        );
    }

    #[test]
    fn file_finder_paste_strips_control_chars_so_a_pasted_newline_does_not_submit_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        app.handle_paste("al\nph\ta");
        assert_eq!(
            app.file_finder.as_ref().unwrap().query,
            "alpha",
            "control chars in pasted text must be filtered out — a stray newline must not end the line and a tab must not navigate"
        );
    }

    #[test]
    fn file_finder_modal_swallows_typed_characters_so_editor_does_not_receive_them() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Editor);
        app.editor.lines = vec![String::from("hello")];
        app.editor.cursor_col = 5;
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER)).unwrap();
        app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE)).unwrap();
        assert_eq!(
            app.editor.lines,
            vec!["hello"],
            "while the file finder is open, typed letters must not leak into the editor buffer"
        );
        assert_eq!(
            app.file_finder.as_ref().unwrap().query,
            "x",
            "typed letters must go into the finder query instead"
        );
    }

    #[test]
    fn editor_cmd_a_still_selects_all() {
        let mut app = editor_app_with_lines(&["hello", "world"]);
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER)).unwrap();
        assert_eq!(app.editor.selection_text(), "hello\nworld");
    }

    #[test]
    fn editor_alt_left_jumps_one_word_backward() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Editor);
        app.editor.lines = vec![String::from("hello world rest")];
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 16;
        app.handle_key(key(KeyCode::Left, KeyModifiers::ALT)).unwrap();
        assert_eq!(app.editor.cursor_col, 12);
        app.handle_key(key(KeyCode::Left, KeyModifiers::ALT)).unwrap();
        assert_eq!(app.editor.cursor_col, 6);
    }

    #[test]
    fn editor_shift_alt_right_extends_selection_by_word() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Editor);
        app.editor.lines = vec![String::from("hello world")];
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 0;
        app.handle_key(key(KeyCode::Right, KeyModifiers::ALT | KeyModifiers::SHIFT))
            .unwrap();
        assert_eq!(
            app.editor.selection_text(),
            "hello",
            "Shift+Option+Right should select from cursor to the next word boundary"
        );
    }

    #[test]
    fn search_visible_does_not_steal_cmd_x_from_focused_editor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.focus_pane(Pane::Editor);
        app.search.query = String::from("needle");
        app.editor.lines = vec![String::from("alpha")];
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 5;
        app.editor.select_all();

        app.handle_key(key(KeyCode::Char('x'), KeyModifiers::SUPER))
            .unwrap();

        assert_eq!(app.editor.lines, [String::new()]);
        assert_eq!(app.search.query, "needle");
    }

    #[test]
    fn search_visible_does_not_steal_bracketed_paste_from_focused_editor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.focus_pane(Pane::Editor);
        app.search.query = String::from("needle");
        app.editor.lines = vec![String::from("alpha")];
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 5;

        app.handle_paste(" beta");

        assert_eq!(app.editor.lines, [String::from("alpha beta")]);
        assert_eq!(app.search.query, "needle");
    }

    #[test]
    fn cmd_v_pastes_clipboard_into_focused_editor_even_when_search_sidebar_visible() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.clipboard_reader = || Some(String::from(" beta"));
        app.set_sidebar_view(SidebarView::Search);
        app.focus_pane(Pane::Editor);
        app.search.query = String::from("needle");
        app.editor.lines = vec![String::from("alpha")];
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 5;

        app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
            .unwrap();

        assert_eq!(app.editor.lines, [String::from("alpha beta")]);
        assert_eq!(app.search.query, "needle");
    }

    #[test]
    fn cmd_a_in_search_selects_entire_query() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.search.query = String::from("alpha");
        app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
        assert_eq!(
            app.search.selection_range(),
            Some((0, "alpha".len())),
            "Cmd+A in search input must select the whole query"
        );
    }

    #[test]
    fn cmd_c_in_search_with_full_selection_clears_no_text() {
        // After Cmd+A then Cmd+C, the query stays put (copy is non-destructive).
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.search.query = String::from("alpha");
        app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
        app.handle_search_key(key(KeyCode::Char('c'), KeyModifiers::SUPER));
        assert_eq!(app.search.query, "alpha");
    }

    #[test]
    fn cmd_x_in_search_with_full_selection_clears_query() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.search.query = String::from("alpha");
        app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
        app.handle_search_key(key(KeyCode::Char('x'), KeyModifiers::SUPER));
        assert_eq!(app.search.query, "");
        assert_eq!(app.search.selection_range(), None);
    }

    #[test]
    fn paste_clipboard_into_search_with_injected_text_appends_to_query() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.search.query = String::from("foo");
        app.paste_clipboard_into_search(Some("BAR"));
        assert_eq!(app.search.query, "fooBAR");
    }

    #[test]
    fn paste_clipboard_into_search_with_no_text_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.search.query = String::from("foo");
        app.paste_clipboard_into_search(None);
        assert_eq!(app.search.query, "foo");
    }

    #[test]
    fn paste_clipboard_into_search_with_no_text_sets_diagnostic_status() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.paste_clipboard_into_search(None);
        assert!(
            app.status.to_lowercase().contains("clipboard"),
            "expected diagnostic status mentioning the clipboard, got: {:?}",
            app.status
        );
    }

    #[test]
    fn paste_clipboard_into_search_with_empty_text_sets_diagnostic_status() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.paste_clipboard_into_search(Some(""));
        assert!(
            app.status.to_lowercase().contains("clipboard"),
            "expected diagnostic status mentioning the clipboard, got: {:?}",
            app.status
        );
    }

    #[test]
    fn unmatched_cmd_key_in_search_logs_diagnostic_status() {
        // If iTerm or the kitty CSI u path delivers a Cmd+<letter> the search
        // handler doesn't recognise, leave a breadcrumb so the user can tell
        // us exactly what crossterm saw — this is how we'll diagnose Cmd+V
        // delivery problems without an interactive debugger.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.handle_search_key(key(KeyCode::Char('q'), KeyModifiers::SUPER));
        assert!(
            app.status.contains("Cmd+"),
            "expected unhandled-cmd diagnostic, got: {:?}",
            app.status
        );
    }

    #[test]
    fn cmd_v_is_recognised_as_search_paste_key() {
        assert!(is_search_paste_key(key(KeyCode::Char('v'), KeyModifiers::SUPER)));
        assert!(is_search_paste_key(key(KeyCode::Char('v'), KeyModifiers::CONTROL)));
        assert!(is_search_paste_key(key(KeyCode::Char('\u{16}'), KeyModifiers::NONE)));
        assert!(!is_search_paste_key(key(KeyCode::Char('v'), KeyModifiers::NONE)));
        assert!(!is_search_paste_key(key(KeyCode::Char('a'), KeyModifiers::SUPER)));
    }

    #[test]
    fn cmd_v_is_recognised_as_editor_paste_key() {
        assert!(is_editor_paste_key(key(KeyCode::Char('v'), KeyModifiers::SUPER)));
        assert!(is_editor_paste_key(key(KeyCode::Char('v'), KeyModifiers::CONTROL)));
        assert!(is_editor_paste_key(key(KeyCode::Char('\u{16}'), KeyModifiers::NONE)));
        assert!(!is_editor_paste_key(key(KeyCode::Char('v'), KeyModifiers::NONE)));
        assert!(!is_editor_paste_key(key(KeyCode::Char('a'), KeyModifiers::SUPER)));
    }

    #[test]
    fn left_click_on_paste_button_triggers_clipboard_paste_into_search() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.tree.last_area = Rect { x: 0, y: 0, width: 60, height: 12 };
        app.search.last_area = Rect { x: 0, y: 0, width: 60, height: 12 };
        app.search.last_inner = Rect { x: 1, y: 1, width: 58, height: 10 };
        app.search.paste_button_x = 40;
        app.search.paste_button_y = 1;
        app.search.paste_button_w = 5;
        let original_status = app.status.clone();
        let m = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 42,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(m);
        assert_ne!(
            app.status, original_status,
            "click on Paste button must invoke the clipboard paste path (status updates whether clipboard read succeeds, fails, or returns empty)"
        );
    }

    #[test]
    fn cmd_p_index_is_built_in_the_background_so_open_does_not_walk_synchronously() {
        // Regression for the user's "Cmd+P is so slow" report. The old
        // open_file_finder ran build_file_index synchronously on the
        // UI thread on first invocation; for a sizeable workspace
        // that stalled the TUI for the entire walk. The fix kicks
        // off the walk in App::new and delivers the index via
        // channel - by the time the user reaches for Cmd+P, the
        // index is already cached.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), b"").unwrap();
        std::fs::create_dir(tmp.path().join(".croft")).unwrap();
        std::fs::write(tmp.path().join(".croft").join("lsp.log"), b"").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Poll for the background-built index to land. With this tiny
        // workspace it should arrive within a couple of hundred ms.
        let mut waited_ms = 0u32;
        loop {
            app.poll_file_finder_index();
            if app.file_finder_index.is_some() {
                break;
            }
            if waited_ms >= 5000 {
                panic!(
                    "background walker thread must deliver the file index within 5s of App::new on a 2-file workspace"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited_ms += 20;
        }
        // Once the index has landed, open_file_finder must consume it
        // without falling back to the synchronous walk - and the index
        // MUST contain the .croft/lsp.log entry the user expected to
        // find via Cmd+P typing 'lsp.log'.
        let rels: Vec<String> = app
            .file_finder_index
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| e.rel.clone())
            .collect();
        assert!(
            rels.iter().any(|r| r == ".croft/lsp.log"),
            "background index must contain .croft/lsp.log; got {rels:?}"
        );
    }

    #[test]
    fn cmd_p_index_refreshes_after_a_new_file_lands_on_disk() {
        // Regression: the Cmd+P index was built once at App::new and
        // never refreshed, so files created later (by another process,
        // by a git checkout that swapped the working tree, or by an
        // in-app create) stayed invisible to Cmd+P until croft was
        // restarted. drain_fs_events now triggers kick_file_finder_index_rebuild
        // so creates and deletes propagate into the haystack the
        // finder ranks against.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), b"").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Wait for the initial background index to land.
        let mut waited = 0u32;
        loop {
            app.poll_file_finder_index();
            if app.file_finder_index.is_some() {
                break;
            }
            if waited >= 5000 {
                panic!("initial index must land within 5s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        // Drain any startup FS-watcher noise so the next loop reacts
        // only to the file we are about to create.
        for _ in 0..20 {
            let _ = app.drain_fs_events();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let new_dir = tmp.path().join("packages/ant-ts-lib-noggin/src/entities/files");
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::write(new_dir.join("resolver.ts"), b"export {};\n").unwrap();
        let started = std::time::Instant::now();
        let mut saw = false;
        for _ in 0..400 {
            let _ = app.drain_fs_events();
            app.poll_file_finder_index();
            if let Some(idx) = app.file_finder_index.as_ref() {
                if idx
                    .iter()
                    .any(|e| e.rel == "packages/ant-ts-lib-noggin/src/entities/files/resolver.ts")
                {
                    saw = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            saw,
            "Cmd+P index must include resolver.ts within a few seconds of it being written to disk; without this, branch-new files stay invisible to Cmd+P until croft restarts"
        );
        assert!(
            started.elapsed() <= std::time::Duration::from_secs(5),
            "index refresh must finish within 5s of the FS event on a 2-file workspace"
        );
    }

    #[test]
    fn cmd_p_index_drops_a_file_deleted_off_disk() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), b"").unwrap();
        let target = tmp.path().join("doomed.rs");
        std::fs::write(&target, b"").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let mut waited = 0u32;
        loop {
            app.poll_file_finder_index();
            if let Some(idx) = app.file_finder_index.as_ref() {
                if idx.iter().any(|e| e.rel == "doomed.rs") {
                    break;
                }
            }
            if waited >= 5000 {
                panic!("initial index must contain doomed.rs within 5s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        for _ in 0..20 {
            let _ = app.drain_fs_events();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::fs::remove_file(&target).unwrap();
        let mut gone = false;
        for _ in 0..400 {
            let _ = app.drain_fs_events();
            app.poll_file_finder_index();
            if let Some(idx) = app.file_finder_index.as_ref() {
                if !idx.iter().any(|e| e.rel == "doomed.rs") {
                    gone = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            gone,
            "deleted file must drop out of the Cmd+P index after the FS-event-driven rebuild lands"
        );
    }

    #[test]
    fn change_workspace_root_rebuilds_cmd_p_index_against_the_new_root() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("root_a");
        let b = tmp.path().join("root_b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("only_in_a.rs"), b"").unwrap();
        std::fs::write(b.join("only_in_b.rs"), b"").unwrap();
        let mut app = App::new(a.clone()).unwrap();
        let mut waited = 0u32;
        loop {
            app.poll_file_finder_index();
            if let Some(idx) = app.file_finder_index.as_ref() {
                if idx.iter().any(|e| e.rel == "only_in_a.rs") {
                    break;
                }
            }
            if waited >= 5000 {
                panic!("initial index against root_a must land within 5s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        app.change_workspace_root(b.clone());
        let mut saw_b = false;
        for _ in 0..400 {
            app.poll_file_finder_index();
            if let Some(idx) = app.file_finder_index.as_ref() {
                let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
                if rels.contains(&"only_in_b.rs") && !rels.contains(&"only_in_a.rs") {
                    saw_b = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            saw_b,
            "after change_workspace_root(b), the Cmd+P index must reflect root_b's files (only_in_b.rs present, only_in_a.rs gone); without this, Cmd+/ leaves Cmd+P searching the previous project"
        );
    }

    #[test]
    fn open_cmd_p_finder_re_ranks_in_place_when_the_index_swaps() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), b"").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let mut waited = 0u32;
        loop {
            app.poll_file_finder_index();
            if app.file_finder_index.is_some() {
                break;
            }
            if waited >= 5000 {
                panic!("initial index must land within 5s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        // Open Cmd+P, type the new filename — initially zero results.
        app.open_file_finder();
        for c in "resolver.ts".chars() {
            app.file_finder.as_mut().unwrap().push_char(c);
        }
        assert!(
            app.file_finder
                .as_ref()
                .unwrap()
                .visible_results()
                .is_empty(),
            "resolver.ts is not on disk yet; finder must show no matches before the file is created"
        );
        let new_dir = tmp.path().join("packages/ant-ts-lib-noggin/src/entities/files");
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::write(new_dir.join("resolver.ts"), b"").unwrap();
        let mut surfaced = false;
        for _ in 0..400 {
            let _ = app.drain_fs_events();
            app.poll_file_finder_index();
            let finder = app.file_finder.as_ref().unwrap();
            if finder.visible_results().iter().any(|r| {
                r.entry.rel == "packages/ant-ts-lib-noggin/src/entities/files/resolver.ts"
            }) {
                surfaced = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            surfaced,
            "an open Cmd+P finder must re-rank against the rebuilt index without the user closing and reopening it"
        );
    }

    #[test]
    fn click_on_tree_row_aligned_with_terminal_splitter_y_selects_the_row_not_the_splitter() {
        // Regression for the user's "I can't click on the last two files,
        // but arrow+Enter opens them" report. Root cause: the editor /
        // terminal splitter handler at handle_mouse checks `m.row == y
        // || m.row == y.saturating_sub(1)` WITHOUT gating by column, so
        // a click on a tree row in the LEFT column whose y happens to
        // line up with the editor/terminal splitter (which only lives
        // in the RIGHT column) was hijacked into a splitter-drag and
        // never reached the Explorer's `node_at_y` branch.
        let tmp = tempfile::tempdir().unwrap();
        for name in [
            "alpha.txt",
            "beta.txt",
            "gamma.txt",
            "delta.txt",
            "epsilon.txt",
            "zeta.txt",
        ] {
            std::fs::write(tmp.path().join(name), b"").unwrap();
        }
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Layout: activity-bar(4) + tree-pane on the left, editor +
        // terminal stacked on the right. Pick a splitter row that
        // intersects one of the rendered tree rows.
        app.tree.last_area = Rect { x: 4, y: 0, width: 32, height: 12 };
        app.tree.last_inner = Rect { x: 5, y: 1, width: 30, height: 10 };
        app.sidebar_splitter_x = Some(36);
        // Splitter sits inside the tree's rendered row band.
        app.terminal_splitter_y = Some(6);
        // The clicked tree row's y MUST equal terminal_splitter_y to
        // trigger the bug; column stays well inside tree.last_inner.x..
        let click_row = 6u16;
        let click_col = app.tree.last_inner.x + 2;
        let target_idx = (click_row - app.tree.last_inner.y) as usize;
        assert!(
            target_idx < app.tree.nodes.len(),
            "test setup must put a real tree row at the splitter y"
        );
        let click = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(
                crossterm::event::MouseButton::Left,
            ),
            column: click_col,
            row: click_row,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(click);
        assert!(
            app.splitter_drag.is_none(),
            "click in the LEFT (tree) column must never arm a splitter drag, even when the row y lines up with the editor/terminal splitter that lives in the RIGHT column"
        );
        assert_eq!(
            app.tree.selected, target_idx,
            "the tree row under the click must become selected; the user reported the bottom rows of the tree being unclickable even though they were plainly visible"
        );
    }

    #[test]
    fn double_clicking_directory_toggles_only_once() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub").join("child.txt"), "hi").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.tree.last_area = Rect { x: 0, y: 0, width: 40, height: 8 };
        app.tree.last_inner = Rect { x: 1, y: 1, width: 38, height: 6 };
        let sub_idx = app
            .tree
            .nodes
            .iter()
            .position(|node| node.path.ends_with("sub"))
            .unwrap();
        let row = app.tree.last_inner.y + sub_idx as u16;
        let click = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(
                crossterm::event::MouseButton::Left,
            ),
            column: 6,
            row,
            modifiers: KeyModifiers::NONE,
        };

        app.handle_mouse(click);
        let sub_idx = app
            .tree
            .nodes
            .iter()
            .position(|node| node.path.ends_with("sub"))
            .unwrap();
        assert!(app.tree.nodes[sub_idx].expanded);
        let expanded_len = app.tree.nodes.len();

        app.handle_mouse(click);
        let sub_idx = app
            .tree
            .nodes
            .iter()
            .position(|node| node.path.ends_with("sub"))
            .unwrap();
        assert!(app.tree.nodes[sub_idx].expanded);
        assert_eq!(app.tree.nodes.len(), expanded_len);
    }

    #[test]
    fn typing_after_select_all_replaces_query() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.search.query = String::from("alpha");
        app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
        app.handle_search_key(key(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(app.search.query, "z");
    }

    #[test]
    fn shift_arrow_extends_tree_selection() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "").unwrap();
        std::fs::write(tmp.path().join("c.txt"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.tree.select_replace(1);
        app.handle_tree_key(key(KeyCode::Down, KeyModifiers::SHIFT));
        app.handle_tree_key(key(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(app.tree.action_paths().len(), 3);
    }

    #[test]
    fn cmd_a_in_tree_marks_all_visible() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_tree_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
        assert_eq!(app.tree.marked.len(), app.tree.nodes.len());
    }

    #[test]
    fn esc_in_tree_clears_marks() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.tree.toggle_mark(1);
        assert!(!app.tree.marked.is_empty());
        app.handle_tree_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.tree.marked.is_empty());
    }

    #[test]
    fn cmd_c_then_cmd_v_copies_explorer_paths_into_target() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("src.txt"), "hello").unwrap();
        std::fs::create_dir(tmp.path().join("dest")).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let src_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("src.txt"))
            .unwrap();
        app.tree.select_replace(src_idx);
        app.handle_tree_key(key(KeyCode::Char('c'), KeyModifiers::SUPER));
        let dest_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("dest"))
            .unwrap();
        app.tree.select_replace(dest_idx);
        app.handle_tree_key(key(KeyCode::Char('v'), KeyModifiers::SUPER));
        let dest_file = tmp.path().join("dest/src.txt");
        assert!(dest_file.exists(), "copy must place file in target dir");
        assert!(
            tmp.path().join("src.txt").exists(),
            "copy must keep the source"
        );
    }

    #[test]
    fn cmd_x_then_cmd_v_moves_explorer_paths_into_target() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("src.txt"), "hello").unwrap();
        std::fs::create_dir(tmp.path().join("dest")).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let src_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("src.txt"))
            .unwrap();
        app.tree.select_replace(src_idx);
        app.handle_tree_key(key(KeyCode::Char('x'), KeyModifiers::SUPER));
        let dest_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("dest"))
            .unwrap();
        app.tree.select_replace(dest_idx);
        app.handle_tree_key(key(KeyCode::Char('v'), KeyModifiers::SUPER));
        assert!(tmp.path().join("dest/src.txt").exists());
        assert!(
            !tmp.path().join("src.txt").exists(),
            "cut must remove source"
        );
        assert!(
            app.tree_clipboard.is_none(),
            "cut clipboard must be consumed on paste"
        );
    }

    #[test]
    fn delete_key_trashes_every_marked_path() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let a_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("a.txt"))
            .unwrap();
        let b_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("b.txt"))
            .unwrap();
        app.tree.select_replace(a_idx);
        app.tree.toggle_mark(b_idx);
        app.handle_tree_key(key(KeyCode::Delete, KeyModifiers::NONE));
        assert!(!tmp.path().join("a.txt").exists());
        assert!(!tmp.path().join("b.txt").exists());
    }

    #[test]
    fn drag_drop_moves_marked_paths_to_target_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("src.txt"), "x").unwrap();
        std::fs::create_dir(tmp.path().join("dest")).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.tree.last_area = Rect { x: 0, y: 0, width: 40, height: 8 };
        app.tree.last_inner = Rect { x: 1, y: 1, width: 38, height: 6 };
        let src_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("src.txt"))
            .unwrap();
        let dest_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("dest"))
            .unwrap();
        let src_row = app.tree.last_inner.y + src_idx as u16;
        let dest_row = app.tree.last_inner.y + dest_idx as u16;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 6,
            row: src_row,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 6,
            row: dest_row,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 6,
            row: dest_row,
            modifiers: KeyModifiers::NONE,
        });
        assert!(tmp.path().join("dest/src.txt").exists());
        assert!(!tmp.path().join("src.txt").exists());
    }

    #[test]
    fn alt_drag_copies_instead_of_moves() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("src.txt"), "x").unwrap();
        std::fs::create_dir(tmp.path().join("dest")).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.tree.last_area = Rect { x: 0, y: 0, width: 40, height: 8 };
        app.tree.last_inner = Rect { x: 1, y: 1, width: 38, height: 6 };
        let src_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("src.txt"))
            .unwrap();
        let dest_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("dest"))
            .unwrap();
        let src_row = app.tree.last_inner.y + src_idx as u16;
        let dest_row = app.tree.last_inner.y + dest_idx as u16;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 6,
            row: src_row,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 6,
            row: dest_row,
            modifiers: KeyModifiers::ALT,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 6,
            row: dest_row,
            modifiers: KeyModifiers::ALT,
        });
        assert!(tmp.path().join("dest/src.txt").exists());
        assert!(
            tmp.path().join("src.txt").exists(),
            "Alt-drag must keep the source"
        );
    }

    #[test]
    fn shift_click_extends_marks_from_anchor() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "").unwrap();
        std::fs::write(tmp.path().join("c.txt"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.tree.last_area = Rect { x: 0, y: 0, width: 40, height: 8 };
        app.tree.last_inner = Rect { x: 1, y: 1, width: 38, height: 6 };
        let first_idx = 1usize;
        let last_idx = app.tree.nodes.len() - 1;
        let first_row = app.tree.last_inner.y + first_idx as u16;
        let last_row = app.tree.last_inner.y + last_idx as u16;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 6,
            row: first_row,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 6,
            row: first_row,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 6,
            row: last_row,
            modifiers: KeyModifiers::SHIFT,
        });
        let span = last_idx - first_idx + 1;
        assert_eq!(app.tree.action_paths().len(), span);
    }

    #[test]
    fn alt_click_toggles_individual_mark_on_release() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.tree.last_area = Rect { x: 0, y: 0, width: 40, height: 8 };
        app.tree.last_inner = Rect { x: 1, y: 1, width: 38, height: 6 };
        let a_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("a.txt"))
            .unwrap();
        let b_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("b.txt"))
            .unwrap();
        let a_row = app.tree.last_inner.y + a_idx as u16;
        let b_row = app.tree.last_inner.y + b_idx as u16;
        for row in [a_row, b_row] {
            app.handle_mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(
                    crossterm::event::MouseButton::Left,
                ),
                column: 6,
                row,
                modifiers: KeyModifiers::ALT,
            });
            app.handle_mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Up(
                    crossterm::event::MouseButton::Left,
                ),
                column: 6,
                row,
                modifiers: KeyModifiers::ALT,
            });
        }
        let marked: BTreeSet<PathBuf> = app.tree.marked.clone();
        assert!(marked.contains(&tmp.path().join("a.txt")));
        assert!(marked.contains(&tmp.path().join("b.txt")));
    }

    #[test]
    fn ctrl_click_also_toggles_individual_mark() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.tree.last_area = Rect { x: 0, y: 0, width: 40, height: 8 };
        app.tree.last_inner = Rect { x: 1, y: 1, width: 38, height: 6 };
        let a_idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("a.txt"))
            .unwrap();
        let row = app.tree.last_inner.y + a_idx as u16;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 6,
            row,
            modifiers: KeyModifiers::CONTROL,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 6,
            row,
            modifiers: KeyModifiers::CONTROL,
        });
        assert!(app.tree.marked.contains(&tmp.path().join("a.txt")));
    }

    #[test]
    fn alt_click_does_not_open_the_file() {
        // Toggle is meant to mark — never to open. The deferred-toggle flow
        // must not reach `editor.open_preview` when Alt/Ctrl is held.
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "hi").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.tree.last_area = Rect { x: 0, y: 0, width: 40, height: 8 };
        app.tree.last_inner = Rect { x: 1, y: 1, width: 38, height: 6 };
        let idx = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path == f)
            .unwrap();
        let row = app.tree.last_inner.y + idx as u16;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 6,
            row,
            modifiers: KeyModifiers::ALT,
        });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 6,
            row,
            modifiers: KeyModifiers::ALT,
        });
        assert!(app.editor.is_blank_initial(), "alt-click must not open the file");
    }

    #[test]
    fn parse_dropped_paths_accepts_a_single_existing_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("dropped.txt");
        std::fs::write(&f, "hi").unwrap();
        let parsed = parse_dropped_paths(&format!("{}\n", f.display()));
        assert_eq!(parsed, vec![f]);
    }

    #[test]
    fn parse_dropped_paths_drops_iterm2_inline_image_drag_artefact() {
        // iTerm2 lets the user drag any OSC-1337 inline image (which is
        // how croft paints activity-bar icons / welcome wordmark / SSH
        // hero) out of the terminal. On mouse-up iTerm2 writes the bytes
        // to $TMPDIR/iTerm2.<rand>.png and re-injects the path as a
        // bracketed paste, which croft would otherwise see as "the user
        // dragged a file into the editor" and open as an image preview.
        // That has happened to the user accidentally when they tried to
        // click a sidebar icon and the click drifted into a drag — the
        // file `iTerm2.5AO7K5.png` opened as a tab showing the Explorer
        // glyph. The drop must be silently filtered.
        let tmp = tempfile::tempdir().unwrap();
        let temp_dir = tmp.path().join("T").join("com.googlecode.iterm2");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let fake = temp_dir.join("iTerm2.5AO7K5.png");
        std::fs::write(&fake, b"\x89PNG\r\n\x1a\n").unwrap();
        let parsed = parse_dropped_paths(&format!("{}\n", fake.display()));
        assert!(
            parsed.is_empty(),
            "iTerm2's inline-image drag temp file must be filtered out so a stray click-drag on an activity-bar icon does not open a phantom PNG tab — got {parsed:?}"
        );
    }

    #[test]
    fn parse_dropped_paths_keeps_user_pngs_that_happen_to_share_a_temp_dir() {
        // The filter must key on the `iTerm2.<rand>.png` filename
        // pattern, not just "anything under /tmp". A real user-authored
        // PNG that happens to live under a temp directory must still be
        // accepted, otherwise the heuristic over-fires.
        let tmp = tempfile::tempdir().unwrap();
        let temp_dir = tmp.path().join("T");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let user_png = temp_dir.join("my_screenshot.png");
        std::fs::write(&user_png, b"\x89PNG\r\n\x1a\n").unwrap();
        let parsed = parse_dropped_paths(&format!("{}\n", user_png.display()));
        assert_eq!(
            parsed,
            vec![user_png.clone()],
            "a user-named PNG under a temp dir must still be droppable — only the literal iTerm2.<rand>.png pattern is the artefact"
        );
    }

    #[test]
    fn parse_dropped_paths_strips_file_url_prefix_and_decodes() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("with space.txt");
        std::fs::write(&f, "hi").unwrap();
        let url = format!(
            "file://{}/with%20space.txt",
            tmp.path().to_string_lossy()
        );
        let parsed = parse_dropped_paths(&url);
        assert_eq!(parsed, vec![f]);
    }

    #[test]
    fn parse_dropped_paths_handles_backslash_escaped_spaces() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("with space.txt");
        std::fs::write(&f, "hi").unwrap();
        let escaped = format!(
            "{}/with\\ space.txt",
            tmp.path().to_string_lossy()
        );
        let parsed = parse_dropped_paths(&escaped);
        assert_eq!(parsed, vec![f]);
    }

    #[test]
    fn parse_dropped_paths_supports_multi_file_drop_via_newlines() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        std::fs::write(&a, "1").unwrap();
        std::fs::write(&b, "2").unwrap();
        let payload = format!("{}\n{}\n", a.display(), b.display());
        let parsed = parse_dropped_paths(&payload);
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains(&a));
        assert!(parsed.contains(&b));
    }

    #[test]
    fn parse_dropped_paths_supports_multi_file_drop_via_unescaped_spaces() {
        // iTerm2's Finder-drop format for two files with spaces in their
        // names is `<path1> <path2>`, where spaces inside a path are
        // backslash-escaped and the separator between paths is a literal
        // space. Make sure both paths come through.
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("02 Аленький цветочек.mp3");
        let b = tmp.path().join("03 Буратино.mp3");
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "y").unwrap();
        let payload = format!(
            "{}/02\\ Аленький\\ цветочек.mp3 {}/03\\ Буратино.mp3",
            tmp.path().to_string_lossy(),
            tmp.path().to_string_lossy(),
        );
        let parsed = parse_dropped_paths(&payload);
        assert_eq!(parsed.len(), 2, "expected two paths, got {parsed:?}");
        assert!(parsed.contains(&a));
        assert!(parsed.contains(&b));
    }

    #[test]
    fn parse_dropped_paths_keeps_quoted_path_with_space_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("with space.txt");
        std::fs::write(&a, "x").unwrap();
        let payload = format!("\"{}\"", a.display());
        let parsed = parse_dropped_paths(&payload);
        assert_eq!(parsed, vec![a]);
    }

    #[test]
    fn parse_dropped_paths_handles_mix_of_quoted_and_escaped_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("plain.txt");
        let b = tmp.path().join("with space.txt");
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "y").unwrap();
        let payload = format!(
            "{} \"{}\"",
            a.display(),
            b.display(),
        );
        let parsed = parse_dropped_paths(&payload);
        assert_eq!(parsed.len(), 2, "got {parsed:?}");
        assert!(parsed.contains(&a));
        assert!(parsed.contains(&b));
    }

    #[test]
    fn parse_dropped_paths_returns_empty_for_plain_text() {
        // Plain typed text must not be hijacked as a drop import.
        let parsed = parse_dropped_paths("hello world this is some text");
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_dropped_paths_skips_nonexistent_paths() {
        let parsed = parse_dropped_paths("/this/path/definitely/does/not/exist");
        assert!(parsed.is_empty());
    }

    #[test]
    fn finder_drop_into_explorer_moves_file_into_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let src = outside.path().join("dragged.txt");
        std::fs::write(&src, "hello").unwrap();
        let mut app = App::new(workspace.path().to_path_buf()).unwrap();
        app.focus_pane(Pane::Tree);
        app.set_sidebar_view(SidebarView::Explorer);
        app.handle_paste(&format!("{}\n", src.display()));
        let dest = workspace.path().join("dragged.txt");
        assert!(dest.exists(), "file should land in the workspace");
        assert!(!src.exists(), "Finder drop must move, not copy");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello");
    }

    #[test]
    fn finder_drop_on_remote_view_queues_scp_even_when_terminal_focused() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let src = outside.path().join("dragged.txt");
        std::fs::write(&src, "hello").unwrap();
        let mut app = App::new(workspace.path().to_path_buf()).unwrap();
        app.remote.targets = vec![crate::remote::RemoteTarget {
            alias: String::from("alpha"),
            host_name: Some(String::from("alpha.example.com")),
            user: Some(String::from("vitali")),
        }];
        app.remote.selected = 0;
        app.set_sidebar_view(SidebarView::Remote);
        // The user clicked into the embedded terminal and never returned
        // focus to the sidebar before dragging from Finder. iTerm2's
        // drag-drop arrives as a bracketed paste; mouse focus does not
        // shift on drop.
        app.show_terminal = true;
        app.focus_pane(Pane::Terminal);
        app.handle_paste(&format!("{}\n", src.display()));
        let queued = app.take_pending_scp_uploads();
        assert_eq!(queued.len(), 1, "one scp upload should be queued");
        assert_eq!(queued[0].alias, "alpha");
        assert_eq!(queued[0].src, src);
        assert!(src.exists(), "scp must copy, not move");
    }

    #[test]
    fn finder_drop_on_remote_view_queues_scp_even_when_editor_focused() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let src = outside.path().join("e.txt");
        std::fs::write(&src, "x").unwrap();
        let mut app = App::new(workspace.path().to_path_buf()).unwrap();
        app.remote.targets = vec![crate::remote::RemoteTarget {
            alias: String::from("beta"),
            host_name: None,
            user: None,
        }];
        app.remote.selected = 0;
        app.set_sidebar_view(SidebarView::Remote);
        app.focus_pane(Pane::Editor);
        let editor_before = app.editor.lines.clone();
        app.handle_paste(&format!("{}\n", src.display()));
        assert_eq!(
            app.editor.lines, editor_before,
            "remote drop must not leak the path text into the editor",
        );
        let queued = app.take_pending_scp_uploads();
        assert_eq!(queued.len(), 1);
    }

    #[test]
    fn finder_drop_on_remote_view_with_no_target_reports_clear_status() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let src = outside.path().join("dragged.txt");
        std::fs::write(&src, "hi").unwrap();
        let mut app = App::new(workspace.path().to_path_buf()).unwrap();
        app.remote.targets.clear();
        app.remote.selected = 0;
        app.set_sidebar_view(SidebarView::Remote);
        app.focus_pane(Pane::Tree);
        app.handle_paste(&format!("{}\n", src.display()));
        assert!(app.take_pending_scp_uploads().is_empty());
        assert!(
            app.status.contains("no Remote Explorer host"),
            "status must explain the failure, was: {:?}",
            app.status,
        );
        assert!(
            src.exists(),
            "with no host the source file must be left intact",
        );
    }

    fn relay_test_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[test]
    fn remote_launched_drop_queues_pull_request_via_relay_log() {
        let _guard = relay_test_lock().lock().unwrap();
        // Simulates the case where the user dragged a Finder file onto a
        // remote-launched croft. The Mac path doesn't exist on the remote
        // box, but the parent local-croft has plumbed the relay env, so
        // the drop is recorded in the request log instead of being
        // pasted into the editor.
        let workspace = tempfile::tempdir().unwrap();
        let relay = tempfile::tempdir().unwrap();
        let log = relay.path().join("requests.log");
        let inbox = relay.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        // SAFETY: tests are single-threaded for env var manipulation.
        unsafe {
            std::env::set_var("CROFT_DROP_RELAY_LOG", &log);
            std::env::set_var("CROFT_DROP_RELAY_INBOX", &inbox);
        }
        let mut app = App::new(workspace.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Explorer);
        app.focus_pane(Pane::Tree);
        app.handle_paste("/Users/vitali/Documents/foo.txt\n");
        unsafe {
            std::env::remove_var("CROFT_DROP_RELAY_LOG");
            std::env::remove_var("CROFT_DROP_RELAY_INBOX");
        }
        let written = std::fs::read_to_string(&log).expect("relay log was written");
        assert!(
            written.starts_with("pull\t"),
            "relay log entry must be a pull request, got: {written:?}",
        );
        assert!(
            written.contains("/Users/vitali/Documents/foo.txt"),
            "relay log must carry the foreign path, got: {written:?}",
        );
        assert_eq!(app.pending_remote_pulls.len(), 1);
    }

    #[test]
    fn drain_remote_pulls_imports_file_when_relay_signals_ok() {
        let _guard = relay_test_lock().lock().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let relay = tempfile::tempdir().unwrap();
        let inbox = relay.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        unsafe {
            std::env::set_var("CROFT_DROP_RELAY_INBOX", &inbox);
        }
        let mut app = App::new(workspace.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Explorer);
        app.focus_pane(Pane::Tree);
        let request_id = String::from("test-req-1");
        let request_dir = inbox.join(&request_id);
        std::fs::create_dir_all(&request_dir).unwrap();
        let staged = request_dir.join("foo.txt");
        std::fs::write(&staged, "payload").unwrap();
        std::fs::write(request_dir.join(".ok"), b"").unwrap();
        app.pending_remote_pulls.push(PendingRemotePull {
            request_id: request_id.clone(),
            src_display: String::from("/Users/v/Docs/foo.txt"),
            basename: String::from("foo.txt"),
            dest_dir: workspace.path().to_path_buf(),
            started_at: std::time::Instant::now(),
            kind: RemotePullKind::File,
        });
        let changed = app.drain_remote_pulls();
        unsafe {
            std::env::remove_var("CROFT_DROP_RELAY_INBOX");
        }
        assert!(changed);
        assert!(app.pending_remote_pulls.is_empty());
        let landed = workspace.path().join("foo.txt");
        assert!(landed.exists(), "file should land in workspace");
        assert_eq!(std::fs::read_to_string(&landed).unwrap(), "payload");
        assert!(
            !request_dir.exists(),
            "relay request dir must be cleaned up after import",
        );
        assert!(app.status.contains("Pulled"));
    }

    #[test]
    fn drain_remote_pulls_surfaces_relay_error_message() {
        let _guard = relay_test_lock().lock().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let relay = tempfile::tempdir().unwrap();
        let inbox = relay.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        unsafe {
            std::env::set_var("CROFT_DROP_RELAY_INBOX", &inbox);
        }
        let mut app = App::new(workspace.path().to_path_buf()).unwrap();
        let request_dir = inbox.join("req-2");
        std::fs::create_dir_all(&request_dir).unwrap();
        std::fs::write(request_dir.join(".err"), b"scp exited with 1").unwrap();
        app.pending_remote_pulls.push(PendingRemotePull {
            request_id: String::from("req-2"),
            src_display: String::from("/Users/v/missing.txt"),
            basename: String::from("missing.txt"),
            dest_dir: workspace.path().to_path_buf(),
            started_at: std::time::Instant::now(),
            kind: RemotePullKind::File,
        });
        let changed = app.drain_remote_pulls();
        unsafe {
            std::env::remove_var("CROFT_DROP_RELAY_INBOX");
        }
        assert!(changed);
        assert!(app.status.contains("scp exited with 1"));
    }

    #[test]
    fn explorer_drop_on_terminal_focus_does_not_hijack_text_paste() {
        // Regression guard: when sidebar is Explorer and focus is on the
        // embedded terminal, pasting still goes to the terminal so the
        // user can type a path into a shell command.
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let src = outside.path().join("typed.txt");
        std::fs::write(&src, "x").unwrap();
        let mut app = App::new(workspace.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Explorer);
        app.show_terminal = true;
        app.focus_pane(Pane::Terminal);
        app.handle_paste(&format!("{}\n", src.display()));
        assert!(
            src.exists(),
            "explorer-with-terminal-focus drop must NOT move the source",
        );
        assert!(!workspace.path().join("typed.txt").exists());
    }

    #[test]
    fn closing_image_tab_requests_overlay_clear_on_next_render() {
        // Repro for: opening a PNG/PDF then closing the tab leaves the
        // OSC-1337 pixels bleeding through the welcome screen. The render
        // path that swaps to welcome must mark the editor image overlay
        // for clearing so the main loop wipes the screen.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor_image_displayed = true;
        app.editor_image_osc = Some(String::from("dummy-osc"));
        app.editor_image_layout = Some(EditorImageLayout {
            cell_x: 0,
            cell_y: 0,
            cell_w: 10,
            cell_h: 10,
            path: tmp.path().join("doomed.png"),
        });
        // Editor is blank-initial right after construction, so calling
        // disable_editor_image directly mimics what render() does on
        // the welcome branch.
        app.disable_editor_image();
        assert!(app.editor_image_osc.is_none());
        assert!(app.editor_image_layout.is_none());
        assert!(
            app.consume_editor_image_clear(),
            "first call must report a pending clear"
        );
        assert!(
            !app.consume_editor_image_clear(),
            "second call must be a no-op"
        );
    }

    #[test]
    fn dragging_sidebar_splitter_resizes_sidebar() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Simulate a render so splitter coords are populated.
        app.sidebar_splitter_x = Some(36); // activity(4) + sidebar(32) = 36
        app.last_content_width = 60;
        app.last_content_height = 20;
        app.sidebar_width = 32;
        // Mouse-down on the splitter column.
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 36,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.splitter_drag, Some(SplitterDrag::Sidebar));
        // Drag right by 10 cells: column 46 -> sidebar should grow to 42.
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 46,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.sidebar_width, 42);
        // Mouse-up clears the drag state.
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 46,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.splitter_drag.is_none());
    }

    #[test]
    fn sidebar_drag_grabs_one_column_left_of_seam() {
        // The seam is a single column wide. To make it easier to grab (and
        // to mirror the visible pair of borders the user sees), a click on
        // the cell immediately to its left also starts the drag.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.sidebar_splitter_x = Some(36);
        app.last_content_width = 60;
        app.last_content_height = 20;
        app.sidebar_width = 32;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 35,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.splitter_drag, Some(SplitterDrag::Sidebar));
    }

    #[test]
    fn welcome_paints_a_full_bordered_box_around_the_editor_area() {
        // Without a visible envelope, users on the welcome page can't
        // perceive that the explorer is resizable and can't see where the
        // editor pane ends. Regression guard: the welcome render must
        // paint a full Borders::ALL frame around `area` so all four edges
        // are perceptible.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let area = ratatui::layout::Rect::new(40, 0, 80, 40);
        term.draw(|f| app.render_welcome(f, area)).unwrap();
        let buf = term.backend().buffer();
        let left = area.x;
        let right = area.x + area.width - 1;
        let top = area.y;
        let bot = area.y + area.height - 1;
        // Vertical edges: left and right columns must be vertical bars on
        // most rows (corners use ─/└/┌/┘).
        let mut left_bars = 0;
        let mut right_bars = 0;
        for y in (top + 1)..bot {
            if buf[(left, y)].symbol() == "│" {
                left_bars += 1;
            }
            if buf[(right, y)].symbol() == "│" {
                right_bars += 1;
            }
        }
        let inner_h = (bot - top - 1) as usize;
        assert!(left_bars >= inner_h / 2, "left edge underpainted: {left_bars}");
        assert!(right_bars >= inner_h / 2, "right edge underpainted: {right_bars}");
        // Horizontal edges: top and bottom rows must be horizontal bars on
        // most columns.
        let mut top_bars = 0;
        let mut bot_bars = 0;
        for x in (left + 1)..right {
            if buf[(x, top)].symbol() == "─" {
                top_bars += 1;
            }
            if buf[(x, bot)].symbol() == "─" {
                bot_bars += 1;
            }
        }
        let inner_w = (right - left - 1) as usize;
        assert!(top_bars >= inner_w / 2, "top edge underpainted: {top_bars}");
        assert!(bot_bars >= inner_w / 2, "bottom edge underpainted: {bot_bars}");
        // Corners.
        assert_eq!(buf[(left, top)].symbol(), "┌");
        assert_eq!(buf[(right, top)].symbol(), "┐");
        assert_eq!(buf[(left, bot)].symbol(), "└");
        assert_eq!(buf[(right, bot)].symbol(), "┘");
    }

    #[test]
    fn sidebar_drag_clamps_to_minimum() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.sidebar_splitter_x = Some(36);
        app.last_content_width = 60;
        app.last_content_height = 20;
        app.sidebar_width = 32;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 36,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        // Drag far to the left — sidebar should clamp to its min, not collapse.
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.sidebar_width, SIDEBAR_WIDTH_MIN);
    }

    #[test]
    fn dragging_terminal_splitter_resizes_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.terminal_splitter_y = Some(15);
        app.last_content_height = 20;
        app.last_content_width = 60;
        app.terminal_height = Some(5); // bottom = 15 + 5 = 20
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 50,
            row: 15,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.splitter_drag, Some(SplitterDrag::Terminal));
        // Drag the splitter up to row 10: terminal height = 20 - 10 = 10.
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 50,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.terminal_height, Some(10));
    }

    #[test]
    fn terminal_drag_grabs_one_row_above_seam() {
        // The terminal splitter sits on the terminal's top border row, but
        // the editor / welcome bottom border row (one row above) is also a
        // grabbable handle — symmetric with the sidebar's two-column zone.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.terminal_splitter_y = Some(15);
        app.last_content_height = 20;
        app.last_content_width = 60;
        app.terminal_height = Some(5);
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 50,
            row: 14,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.splitter_drag, Some(SplitterDrag::Terminal));
    }

    #[test]
    fn fresh_app_has_one_active_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let app = App::new(tmp.path().to_path_buf()).unwrap();
        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.active_terminal, 0);
    }

    #[test]
    fn split_terminal_appends_and_activates_new_one() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.split_terminal().unwrap();
        assert_eq!(app.terminals.len(), 2);
        assert_eq!(app.active_terminal, 1);
        app.split_terminal().unwrap();
        assert_eq!(app.terminals.len(), 3);
        assert_eq!(app.active_terminal, 2);
    }

    #[test]
    fn split_terminal_inserts_to_the_right_of_the_active_terminal_not_at_the_end() {
        // Open three terminals so the strip is [T0, T1, T2] with T2
        // active. Focus T0 (the left-most), split. The new terminal must
        // land at index 1 — immediately to the right of where the user
        // was looking — and T1/T2 must shift right to indices 2 and 3,
        // matching the user's mental model that a new pane appears next
        // to the one they're working in, not at the far right edge.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.split_terminal().unwrap();
        app.split_terminal().unwrap();
        let pid_t0 = app.terminals[0].pid();
        let pid_t1 = app.terminals[1].pid();
        let pid_t2 = app.terminals[2].pid();
        app.active_terminal = 0;
        app.split_terminal().unwrap();
        assert_eq!(app.terminals.len(), 4);
        assert_eq!(
            app.active_terminal, 1,
            "a split from the left-most terminal must focus the new one at index 1, not at the far right"
        );
        assert_eq!(
            app.terminals[0].pid(),
            pid_t0,
            "the originally-active terminal must keep its slot"
        );
        assert_eq!(
            app.terminals[2].pid(),
            pid_t1,
            "the old neighbour to the right must shift one slot right, not stay put"
        );
        assert_eq!(
            app.terminals[3].pid(),
            pid_t2,
            "the right-most terminal must shift to the new right edge"
        );
    }

    #[test]
    fn close_active_terminal_drops_the_active_one() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.split_terminal().unwrap();
        app.split_terminal().unwrap();
        assert_eq!(app.active_terminal, 2);
        assert!(app.close_active_terminal());
        assert_eq!(app.terminals.len(), 2);
        assert_eq!(app.active_terminal, 1);
        assert!(app.close_active_terminal());
        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.active_terminal, 0);
    }

    #[test]
    fn close_active_terminal_refuses_to_drop_the_last_one() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        assert!(!app.close_active_terminal());
        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.active_terminal, 0);
    }

    #[test]
    fn cycle_terminal_wraps_around() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.split_terminal().unwrap();
        app.split_terminal().unwrap();
        app.active_terminal = 0;
        app.cycle_terminal();
        assert_eq!(app.active_terminal, 1);
        app.cycle_terminal();
        assert_eq!(app.active_terminal, 2);
        app.cycle_terminal();
        assert_eq!(app.active_terminal, 0);
    }

    #[test]
    fn cycle_terminal_back_wraps_around_to_the_left() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.split_terminal().unwrap();
        app.split_terminal().unwrap();
        app.active_terminal = 0;
        app.cycle_terminal_back();
        assert_eq!(
            app.active_terminal, 2,
            "cycle-back from the left edge must wrap to the right-most slot"
        );
        app.cycle_terminal_back();
        assert_eq!(app.active_terminal, 1);
        app.cycle_terminal_back();
        assert_eq!(app.active_terminal, 0);
    }

    #[test]
    fn ctrl_shift_open_bracket_cycles_to_the_previous_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.split_terminal().unwrap();
        app.split_terminal().unwrap();
        app.focus = Pane::Terminal;
        app.active_terminal = 2;
        app.handle_key(key(
            KeyCode::Char('['),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(
            app.active_terminal, 1,
            "Ctrl+Shift+[ must move focus one terminal to the left"
        );
        app.handle_key(key(
            KeyCode::Char('{'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(
            app.active_terminal, 0,
            "Ctrl+Shift+{{ (shift-applied codepoint) must also cycle back so the binding works under crossterm flavours that pre-apply the shift"
        );
    }

    #[test]
    fn ctrl_shift_t_globally_splits_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        assert_eq!(app.terminals.len(), 1);
        app.handle_key(key(
            KeyCode::Char('T'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(app.terminals.len(), 2);
        assert_eq!(app.active_terminal, 1);
    }

    #[test]
    fn ctrl_shift_w_in_terminal_pane_closes_active() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.split_terminal().unwrap();
        app.focus_pane(Pane::Terminal);
        app.handle_key(key(
            KeyCode::Char('W'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(app.terminals.len(), 1);
    }

    #[test]
    fn clicking_terminal_add_button_splits() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.terminal_add_buttons = vec![Rect { x: 50, y: 30, width: 3, height: 1 }];
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 51,
            row: 30,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.terminals.len(), 2);
        assert_eq!(app.active_terminal, 1);
    }

    #[test]
    fn clicking_terminal_close_button_drops_active_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.split_terminal().unwrap();
        assert_eq!(app.terminals.len(), 2);
        assert_eq!(app.active_terminal, 1);
        app.terminal_close_buttons = vec![
            Rect { x: 10, y: 30, width: 3, height: 1 },
            Rect { x: 50, y: 30, width: 3, height: 1 },
        ];
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 51,
            row: 30,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.terminals.len(), 1);
    }

    #[test]
    fn clicking_close_button_on_non_active_terminal_drops_that_specific_terminal() {
        // The user has three terminals open with the rightmost (idx 2) active;
        // clicking the [-] on the leftmost terminal must close idx 0, not the
        // active one. Regression for "I would like to kill any of them, not
        // only the right one."
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.split_terminal().unwrap();
        app.split_terminal().unwrap();
        assert_eq!(app.terminals.len(), 3);
        assert_eq!(app.active_terminal, 2);
        let pid_left = app.terminals[0].pid();
        let pid_right = app.terminals[2].pid();
        app.terminal_close_buttons = vec![
            Rect { x: 10, y: 30, width: 3, height: 1 },
            Rect { x: 30, y: 30, width: 3, height: 1 },
            Rect { x: 50, y: 30, width: 3, height: 1 },
        ];
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 11,
            row: 30,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.terminals.len(), 2);
        assert!(
            app.terminals.iter().all(|t| t.pid() != pid_left),
            "the leftmost terminal (the one whose [-] was clicked) must be gone"
        );
        assert_eq!(
            app.terminals[1].pid(),
            pid_right,
            "the formerly-rightmost terminal must remain"
        );
        assert_eq!(
            app.active_terminal, 1,
            "active terminal index must shift down so the same terminal stays active"
        );
    }

    #[test]
    fn each_terminal_paints_its_own_add_close_buttons_when_multiple_open() {
        // Per-pane chrome: with N terminals visible there should be N [+] hit
        // rects (one per pane) and, when N > 1, N [-] hit rects so the user
        // can kill any one of them.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.split_terminal().unwrap();
        app.split_terminal().unwrap();
        assert_eq!(app.terminals.len(), 3);
        let backend = ratatui::backend::TestBackend::new(180, 40);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        assert_eq!(
            app.terminal_add_buttons.len(),
            3,
            "each terminal pane must record its own [+] hit rect"
        );
        assert_eq!(
            app.terminal_close_buttons.len(),
            3,
            "each terminal pane must record its own [-] hit rect when more than one is open"
        );
        // Distinct columns: the buttons must sit on different panes.
        let xs: std::collections::HashSet<u16> =
            app.terminal_add_buttons.iter().map(|r| r.x).collect();
        assert_eq!(xs.len(), 3, "add-button rects must be on three distinct columns");
    }

    #[test]
    fn cwd_of_pid_on_self_returns_a_directory_under_tempdir() {
        // The harness runs each test from the crate root by default, but
        // we don't depend on that — we just need cwd_of_pid to round-trip
        // *some* directory for the current process. lsof / /proc gives us
        // the live cwd of the shell that started the test runner.
        let pid = std::process::id();
        match cwd_of_pid(pid) {
            Some(p) => assert!(p.is_dir(), "cwd lookup must return a real directory, got {p:?}"),
            None => {
                // Acceptable on platforms where neither /proc nor lsof
                // resolve the cwd in the sandbox. The fallback in
                // split_terminal handles that case; nothing to assert here.
            }
        }
    }

    #[test]
    fn add_button_wins_over_terminal_splitter_on_same_row() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Splitter and button share the row, as they do in real layout.
        app.terminal_splitter_y = Some(30);
        app.terminal_add_buttons = vec![Rect { x: 50, y: 30, width: 3, height: 1 }];
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 51,
            row: 30,
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.splitter_drag.is_none(), "click on [+] must not start a splitter drag");
        assert_eq!(app.terminals.len(), 2);
    }

    #[test]
    fn peek_terminals_dirty_does_not_clear_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Fresh App starts dirty so the first frame paints.
        assert!(app.peek_terminals_dirty(), "fresh app should be dirty");
        // Two consecutive peeks must both see the flag - peek must not clear.
        assert!(app.peek_terminals_dirty(), "peek must not consume the dirty flag");
        // Drain consumes it.
        let drained = app.drain_terminals_dirty();
        assert!(drained, "drain after peek should still report dirty once");
        assert!(!app.peek_terminals_dirty(), "after drain, peek must be clean");
    }

    #[test]
    fn ctrl_shift_g_jumps_to_source_control() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.handle_key(key(
            KeyCode::Char('G'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(app.sidebar_view, SidebarView::SourceControl);
        assert!(app.source_control.focused);
    }

    #[test]
    fn activity_bar_render_lays_out_run_debug_between_source_control_and_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let bar = Rect { x: 0, y: 0, width: ACTIVITY_BAR_WIDTH, height: 30 };
        let backend = ratatui::backend::TestBackend::new(40, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render_activity_bar(f, bar)).unwrap();
        let scm = app.sidebar_areas.source_control_icon;
        let run = app.sidebar_areas.run_debug_icon;
        let rem = app.sidebar_areas.remote_icon;
        assert!(run.height > 0, "run-debug icon must claim a non-zero block");
        assert!(
            run.y > scm.y,
            "run-debug must sit BELOW source-control (scm.y={}, run.y={})",
            scm.y,
            run.y
        );
        assert!(
            run.y < rem.y,
            "run-debug must sit ABOVE remote (run.y={}, rem.y={}) — VS Code's activity-bar order",
            run.y,
            rem.y
        );
    }

    #[test]
    fn click_on_run_debug_icon_switches_sidebar_to_run_debug() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let backend = ratatui::backend::TestBackend::new(80, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let run = app.sidebar_areas.run_debug_icon;
        assert!(run.height > 0, "run-debug icon must be laid out");
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: run.x,
            row: run.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.sidebar_view, SidebarView::RunDebug);
    }

    #[test]
    fn run_spec_for_python_with_no_venv_falls_back_to_system_python3() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("script.py");
        std::fs::write(&f, b"").unwrap();
        let spec = App::run_spec_for(&f, tmp.path()).unwrap();
        assert_eq!(spec.program, "python3");
        assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
        assert_eq!(spec.cwd, tmp.path());
    }

    #[test]
    fn run_spec_for_node_picks_node_for_dot_js() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("server.js");
        std::fs::write(&f, b"").unwrap();
        let spec = App::run_spec_for(&f, tmp.path()).unwrap();
        assert_eq!(spec.program, "node");
        assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
    }

    #[test]
    fn run_spec_for_bash_picks_bash_for_dot_sh() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("build.sh");
        std::fs::write(&f, b"").unwrap();
        let spec = App::run_spec_for(&f, tmp.path()).unwrap();
        assert_eq!(spec.program, "bash");
        assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
    }

    #[test]
    fn run_spec_for_unknown_extension_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("notes.txt");
        std::fs::write(&f, b"").unwrap();
        assert!(App::run_spec_for(&f, tmp.path()).is_none());
    }

    #[test]
    fn run_spec_passes_apostrophe_paths_through_argv_unescaped() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("O'Reilly.py");
        std::fs::write(&f, b"").unwrap();
        let spec = App::run_spec_for(&f, tmp.path()).unwrap();
        assert_eq!(
            spec.args,
            vec![f.to_string_lossy().into_owned()],
            "argv carries the path verbatim — no shell quoting because no shell is invoked"
        );
    }

    /// Drop a fake `bin/python` shim in `dir/.venv/bin/python` so the venv
    /// detector finds an executable file. Returns the full path to the shim
    /// for assertions.
    fn make_venv(dir: &Path, venv_name: &str) -> PathBuf {
        let bin = dir.join(venv_name).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let py = bin.join("python");
        std::fs::write(&py, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&py).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&py, perms).unwrap();
        }
        py
    }

    #[test]
    fn run_spec_picks_dot_venv_python_in_the_file_directory_over_system_python3() {
        // Reproduces the user-reported case: croft is opened on a parent
        // directory, the file lives in a subfolder that owns a .venv, and
        // running the file with system python3 misses the project's deps.
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let venv_python = make_venv(&project, ".venv");
        let f = project.join("main.py");
        std::fs::write(&f, b"").unwrap();
        let spec = App::run_spec_for(&f, tmp.path()).unwrap();
        assert_eq!(
            spec.program,
            venv_python.to_string_lossy().into_owned(),
            "venv interpreter must be the spawned program (looked for {})",
            venv_python.display()
        );
        assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
        assert_eq!(
            spec.cwd, project,
            "cwd must be the directory that owns the venv so relative imports / .env files resolve"
        );
    }

    #[test]
    fn run_spec_walks_up_from_file_dir_to_find_venv_in_an_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let nested = project.join("src").join("pkg");
        std::fs::create_dir_all(&nested).unwrap();
        make_venv(&project, ".venv");
        let f = nested.join("main.py");
        std::fs::write(&f, b"").unwrap();
        let spec = App::run_spec_for(&f, tmp.path()).unwrap();
        assert!(
            spec.program.ends_with(".venv/bin/python"),
            "expected venv python program, got: {}",
            spec.program
        );
        assert_eq!(spec.cwd, project);
    }

    #[test]
    fn run_spec_also_picks_a_plain_venv_directory() {
        let tmp = tempfile::tempdir().unwrap();
        make_venv(tmp.path(), "venv");
        let f = tmp.path().join("main.py");
        std::fs::write(&f, b"").unwrap();
        let spec = App::run_spec_for(&f, tmp.path()).unwrap();
        assert!(
            spec.program.ends_with("venv/bin/python"),
            "expected plain-venv python program, got: {}",
            spec.program
        );
    }

    #[test]
    fn run_spec_does_not_walk_above_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        // venv lives ABOVE the workspace root — must be ignored.
        make_venv(tmp.path(), ".venv");
        let workspace = tmp.path().join("inner");
        std::fs::create_dir_all(&workspace).unwrap();
        let f = workspace.join("main.py");
        std::fs::write(&f, b"").unwrap();
        let spec = App::run_spec_for(&f, &workspace).unwrap();
        assert_eq!(
            spec.program, "python3",
            "must NOT use a venv that sits above the workspace root, got: {}",
            spec.program
        );
    }

    #[test]
    fn run_active_file_with_no_open_file_records_feedback_and_does_not_spawn_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let term_count_before = app.terminals.len();
        app.run_active_file();
        assert_eq!(
            app.terminals.len(),
            term_count_before,
            "no file open: must NOT spawn an extra terminal"
        );
        assert!(app.run_debug.feedback_is_error);
        assert!(app.run_debug.feedback.is_some());
    }

    #[test]
    fn run_active_file_with_unknown_extension_records_feedback_and_does_not_spawn_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("notes.txt");
        std::fs::write(&f, b"hi").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open(&f).unwrap();
        let term_count_before = app.terminals.len();
        app.run_active_file();
        assert_eq!(app.terminals.len(), term_count_before);
        assert!(app.run_debug.feedback_is_error);
    }

    #[test]
    fn run_active_file_with_python_file_spawns_a_new_terminal_and_focuses_it() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("hello.py");
        std::fs::write(&f, b"print('hi')\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open(&f).unwrap();
        let term_count_before = app.terminals.len();
        app.run_active_file();
        assert_eq!(
            app.terminals.len(),
            term_count_before + 1,
            "running a known extension must spawn a fresh terminal so the previous one's history isn't clobbered"
        );
        assert!(app.show_terminal, "terminal pane must become visible");
        assert!(matches!(app.focus, Pane::Terminal));
        assert!(!app.run_debug.feedback_is_error);
    }

    #[test]
    fn focus_flag_only_set_on_active_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.split_terminal().unwrap();
        app.focus_pane(Pane::Terminal);
        let active = app.active_terminal;
        for (i, t) in app.terminals.iter().enumerate() {
            assert_eq!(t.focused, i == active);
        }
        app.focus_pane(Pane::Editor);
        for t in &app.terminals {
            assert!(!t.focused);
        }
    }

    #[test]
    fn build_tab_context_menu_items_includes_all_four_close_actions_in_the_middle_of_a_strip() {
        // Right-click on tab 1 of 3 (i.e. a middle tab) should surface
        // every close action — there's at least one tab on each side.
        let items = build_tab_context_menu_items(1, 3);
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            labels,
            ["Close", "Close Others", "Close to the Right", "Close All"],
            "all four close actions must appear and in this order — mirrors VS Code's tab strip"
        );
        assert!(matches!(&items[0].1, MenuAction::CloseTab(1)));
        assert!(matches!(&items[1].1, MenuAction::CloseOtherTabs(1)));
        assert!(matches!(&items[2].1, MenuAction::CloseTabsToRight(1)));
        assert!(matches!(&items[3].1, MenuAction::CloseAllTabs));
    }

    #[test]
    fn build_tab_context_menu_items_hides_close_to_the_right_on_the_last_tab() {
        // Last tab → "Close to the Right" would close zero tabs, so it
        // must be suppressed (matches VS Code).
        let items = build_tab_context_menu_items(2, 3);
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            labels,
            ["Close", "Close Others", "Close All"],
            "Close to the Right must be suppressed on the rightmost tab"
        );
    }

    #[test]
    fn build_tab_context_menu_items_hides_close_others_when_only_one_tab_is_open() {
        let items = build_tab_context_menu_items(0, 1);
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            labels,
            ["Close", "Close All"],
            "with a single tab, Close Others and Close to the Right are both no-ops and must be suppressed"
        );
    }
}

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

pub fn run(root: PathBuf) -> Result<()> {
    let title = build_title(&root);
    let mut app = App::new(root)?;

    enable_raw_mode().context("enable raw mode")?;
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
            out.write_all(set_session_bg_srgb_seq(EDITOR_BG_RGB).as_bytes()).ok();
        }
        out.flush().ok();
    }
    // Env-var-only iTerm2 detection: no stdin queries, so this can't
    // contend with the crossterm event reader.
    app.init_graphics();

    // Wrap stdout so we can count exactly how many bytes ratatui's diff
    // ships per frame — over SSH that byte count, alongside the per-frame
    // render time, is the signal that tells a fat repaint from a slow
    // remote CPU. Toggled on screen with F8.
    let perf_bytes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    app.perf_bytes = Some(perf_bytes.clone());
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
    if let Some(remote) = app.remote_launch.take() {
        app._fs_watcher = None;
        app.fs_rx = None;
        app.fs_watcher_init_rx = None;
        let launch_result = if let Some(adopted) = remote.adopted {
            crate::remote::launch_only(adopted, remote.path.as_deref())
        } else {
            crate::remote::launch_croft_with(
                &remote.host,
                remote.path.as_deref(),
                None,
            )
        };
        restore_host_terminal_state();
        launch_result?;
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
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        let _ = disable_raw_mode();
        let mut out = stdout();
        let _ = out.write_all(TERMINAL_RESTORE_SEQ);
        let _ = out.write_all(b"\x1b[?25h");
        let _ = out.flush();
        original(info);
    }));
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
        let _ = writeln!(
            out,
            "── croft: uploading {total} item(s) via scp ──"
        );
        let _ = writeln!(out, "(scp prompts for password / passphrase / host-key will appear below)");
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
    app.activity_overlay_dirty = true;
    app.welcome_overlay_dirty = true;
    app.report_scp_results(moved, total, error_count, &affected_dirs);
    Ok(())
}

fn main_loop(app: &mut App, terminal: &mut CroftTerminal) -> Result<()> {
    // Force the very first frame to render so the user sees the UI even
    // before the first event arrives or any timer fires.
    let mut needs_redraw = true;
    let mut last_blink_visible = app.cursor_visible_phase();
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
            app.last_fps = frames_in_window;
            app.last_kps = keys_in_window;
            app.last_ips = iters_in_window;
            app.last_work_us = work_max_us;
            app.last_poll_us = poll_max_us;
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
        let commits_changed = app.drain_recent_commits();
        let search_changed = app.drain_search_results();
        let remote_changed = app.refresh_remote_if_config_changed();
        let pulls_changed = app.drain_remote_pulls();
        let connect_changed = app.poll_connect_dialog();
        let install_changed = app.poll_install_session();
        app.sync_lsp();
        let lsp_changed = app.drain_lsp_completion();
        let sysmon_changed = app.drain_sysmon();

        let non_pty_dirty = needs_redraw
            || fs_changed
            || blink_changed
            || commits_changed
            || search_changed
            || remote_changed
            || pulls_changed
            || connect_changed
            || install_changed
            || lsp_changed
            || sysmon_changed;
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
                || app.consume_ssh_empty_state_image_clear()
            {
                terminal.clear()?;
                // Activity-bar icons live outside ratatui too; re-emit
                // them on the next post-draw flush. The welcome wordmark
                // and the source-control hero also live in iTerm's image
                // cache that just got wiped - re-arm them so the next
                // render re-emits whichever one is currently visible.
                app.activity_overlay_dirty = true;
                app.welcome_overlay_dirty = true;
                app.no_repo_hero_overlay_dirty = true;
            }
            let draw_start = std::time::Instant::now();
            terminal.draw(|f| {
                app.render(f);
            })?;
            // Record render+flush time and the bytes ratatui shipped this
            // frame so the F8 HUD can show where remote latency goes.
            app.last_draw_us = draw_start.elapsed().as_micros();
            if let Some(counter) = app.perf_bytes.as_ref() {
                app.last_frame_bytes = counter.swap(0, std::sync::atomic::Ordering::Relaxed);
            }
            frames_in_window += 1;
            if app.consume_welcome_image_clear()
                || app.consume_editor_image_clear()
                || app.consume_run_debug_image_clear()
                || app.consume_no_repo_hero_image_clear()
                || app.consume_shortcuts_image_clear()
                || app.consume_file_finder_image_clear()
                || app.consume_ssh_empty_state_image_clear()
            {
                terminal.clear()?;
                app.activity_overlay_dirty = true;
                app.welcome_overlay_dirty = true;
                app.no_repo_hero_overlay_dirty = true;
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
            // Keep-alive interval for the activity overlay refresh. Long
            // enough that the editor caret never flickers at the PTY redraw
            // cadence, short enough that iTerm2's image-cell eviction is
            // imperceptible if it ever fires.
            const ACTIVITY_OVERLAY_KEEPALIVE: Duration = Duration::from_secs(2);
            let must_refresh_activity = app.activity_overlay_dirty
                || app
                    .last_activity_overlay_emit
                    .map_or(true, |t| t.elapsed() >= ACTIVITY_OVERLAY_KEEPALIVE);
            let overlays = if must_refresh_activity {
                app.pending_activity_image_overlays()
            } else {
                Vec::new()
            };
            if !overlays.is_empty() {
                use std::io::Write;
                let mut out = stdout();
                let cursor_on = app.cursor_should_be_visible();
                let _ = write!(out, "\x1b[?25l\x1b[s");
                for ((x, y), seq) in overlays {
                    let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1); // 1-based
                    let _ = out.write_all(seq.as_bytes());
                }
                let _ = write!(out, "\x1b[u");
                if cursor_on {
                    let _ = write!(out, "\x1b[?25h");
                }
                let _ = out.flush();
                app.activity_overlay_dirty = false;
                app.last_activity_overlay_emit = Some(std::time::Instant::now());
            }
            // Active editor image preview: bake-once-emit-each-frame
            // overlay, just like the welcome wordmark. Sent after ratatui
            // has finished its diff so the image bytes land on cells
            // ratatui won't repaint until layout changes again.
            if let Some((osc, layout)) = app.editor_image_payload() {
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
                app.mark_editor_image_displayed();
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
            if app.shortcuts_modal.is_none()
                && app.file_finder.is_none()
                && app.connect_dialog.is_none()
                && app.editor.is_blank_initial()
                && app.welcome_overlay_dirty
                && !app.terminal_maximized
            {
                if let (Some(img), Some(layout)) =
                    (app.welcome_image.as_ref(), app.welcome_layout)
                {
                    use std::io::Write;
                    let mut out = stdout();
                    let cursor_on = app.cursor_should_be_visible();
                    let _ = write!(out, "\x1b[?25l\x1b[s");
                    let _ = write!(
                        out,
                        "\x1b[{};{}H",
                        layout.cell_y + 1,
                        layout.cell_x + 1
                    );
                    let _ = out.write_all(img.as_bytes());
                    let _ = write!(out, "\x1b[u");
                    if cursor_on {
                        let _ = write!(out, "\x1b[?25h");
                    }
                    let _ = out.flush();
                    app.welcome_overlay_dirty = false;
                    app.welcome_image_displayed = true;
                }
            }
            needs_redraw = false;
            last_blink_visible = blink_visible;
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
                        // Alt-screen reflow blanks the activity bar cells; the
                        // OSC images need to be re-emitted on the next draw.
                        app.activity_overlay_dirty = true;
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
    }
    Ok(())
}
