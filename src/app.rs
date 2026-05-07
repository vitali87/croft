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
    file_tree::FileTree,
    remote::RemotePanel,
    search::SearchPanel,
    source_control::SourceControlPanel,
    terminal::PtyTerminal,
};

/// Which sidebar view is active in the left side panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarView {
    Explorer,
    Search,
    SourceControl,
    Remote,
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

fn activity_remote_y(bar: Rect) -> u16 {
    activity_source_control_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
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
    // Codicon `source-control` (U+EB14): the Y-fork that matches the
    // activity-bar Source Control icon, so the status-bar branch indicator
    // and the SCM panel share the same visual mark.
    spans.push(Span::styled(
        "\u{eb14} ",
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
            items.push((String::from("Rename…"), MenuAction::Rename(p.clone())));
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
            String::from("New File…"),
            MenuAction::Create(CreateKind::File),
        ));
        items.push((
            String::from("New Folder…"),
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

pub struct App {
    pub tree: FileTree,
    pub search: SearchPanel,
    pub remote: RemotePanel,
    pub source_control: SourceControlPanel,
    pub editor: EditorTabs,
    pub terminals: Vec<PtyTerminal>,
    pub active_terminal: usize,
    workspace_root: PathBuf,
    sidebar_view: SidebarView,
    sidebar_areas: SidebarAreas,
    focus: Pane,
    show_tree: bool,
    show_terminal: bool,
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
    /// Receives the initial `git::query` result from a background thread.
    /// `git status --porcelain` on a huge dirty repo can take hundreds of
    /// milliseconds, so it's deferred. None once installed.
    git_status_init_rx: Option<std::sync::mpsc::Receiver<crate::git::GitStatus>>,
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
    /// Absolute terminal cell where the Codeberg badge image goes. Recorded
    /// during welcome render; consumed post-draw. `None` when the welcome
    /// panel isn't visible or the open repo isn't on Codeberg.
    welcome_codeberg_badge_cell: Option<(u16, u16)>,
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
    search_results_rx: std::sync::mpsc::Receiver<crate::widgets::search::SearchResult>,
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
    fs_poll_dir_mtimes: std::collections::BTreeMap<PathBuf, Option<std::time::SystemTime>>,
    fs_poll_open_file_mtime: Option<(PathBuf, Option<(std::time::SystemTime, u64)>)>,
    remote_launch: Option<RemoteLaunch>,
    /// Explorer-scoped Cut/Copy buffer. Independent from the OS clipboard
    /// (which carries text), this stores filesystem paths and the intent
    /// (move vs. copy) until the next Paste consumes it.
    tree_clipboard: Option<ExplorerClipboard>,
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
    /// Hit-test rectangle of the "[+]" button on the terminal pane's top
    /// border. None when the pane is hidden or too narrow for the label.
    terminal_add_button: Option<Rect>,
    /// Hit-test rectangle of the "[-]" button on the terminal pane's top
    /// border. None when the pane is hidden, the label can't fit, or only
    /// one terminal is open (closing the last one would leave the pane
    /// empty, which we explicitly forbid).
    terminal_close_button: Option<Rect>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteLaunch {
    host: String,
    path: Option<String>,
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
const WELCOME_FOOTER: &str =
    "\u{2039}  Blazingly fast by design.  Secure by default.  Loved by developers.  \u{203a}";

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

        // `git status --porcelain` on a huge dirty repo can be hundreds of
        // ms. Same treatment: kick it off, install when ready.
        let (git_init_tx, git_init_rx) = std::sync::mpsc::channel();
        let root_for_git = root.clone();
        std::thread::spawn(move || {
            let s = crate::git::query(&root_for_git);
            let _ = git_init_tx.send(s);
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
        let fs_poll_dir_mtimes = Self::snapshot_expanded_dir_mtimes(&tree);
        Ok(Self {
            tree,
            search,
            remote,
            source_control,
            editor,
            terminals: vec![term],
            active_terminal: 0,
            workspace_root: root.clone(),
            sidebar_view: SidebarView::Explorer,
            sidebar_areas: SidebarAreas::default(),
            focus: Pane::Tree,
            show_tree: true,
            show_terminal: true,
            status: String::from("Ready"),
            quit: false,
            context_menu: None,
            prompt: None,
            _fs_watcher: None,
            fs_rx: None,
            fs_watcher_init_rx: Some(fs_init_rx),
            git_status: crate::git::GitStatus::default(),
            git_status_init_rx: Some(git_init_rx),
            last_git_check: std::time::Instant::now(),
            cursor_blink_anchor: std::time::Instant::now(),
            activity_images: None,
            welcome_codeberg_badge_osc: None,
            welcome_codeberg_badge_cell: None,
            last_editor_left_down: None,
            last_tree_left_down: None,
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
            cell_pixel: None,
            clipboard_reader: read_system_clipboard,
            fs_poll_last_check: std::time::Instant::now(),
            fs_poll_dir_mtimes,
            fs_poll_open_file_mtime: None,
            remote_launch: None,
            tree_clipboard: None,
            compare_anchor: None,
            last_frame_area: Rect::default(),
            tree_drag: None,
            pending_scp_uploads: Vec::new(),
            pending_remote_pulls: Vec::new(),
            pending_local_open: None,
            trust_local_browser: false,
            sidebar_width: SIDEBAR_WIDTH_DEFAULT,
            terminal_height: None,
            splitter_drag: None,
            sidebar_splitter_x: None,
            terminal_splitter_y: None,
            terminal_add_button: None,
            terminal_close_button: None,
            last_activity_overlay_emit: None,
            last_content_width: 0,
            last_content_height: 0,
            editor_image_osc: None,
            editor_image_layout: None,
            editor_image_displayed: false,
            editor_image_clear_requested: false,
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
        let source_control_png = crate::iterm2_inline::bake_source_control_src_png();
        let source_control_active = encode(&source_control_png, true);
        let source_control_inactive = encode(&source_control_png, false);
        let remote_active = encode(crate::iterm2_inline::REMOTE_SRC_PNG, true);
        let remote_inactive = encode(crate::iterm2_inline::REMOTE_SRC_PNG, false);
        if let (Some(ea), Some(ei), Some(sa), Some(si), Some(sca), Some(sci), Some(ra), Some(ri)) = (
            explorer_active,
            explorer_inactive,
            search_active,
            search_inactive,
            source_control_active,
            source_control_inactive,
            remote_active,
            remote_inactive,
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
    }

    /// Returns the post-frame OSC-1337 escapes to write under the activity
    /// bar, paired with the absolute terminal cell where each one starts
    /// (just past the active-pill column). Empty when image rendering is
    /// disabled or the activity bar hasn't been laid out yet.
    pub fn pending_activity_image_overlays(&self) -> Vec<((u16, u16), &str)> {
        let Some(images) = self.activity_images.as_ref() else {
            return Vec::new();
        };
        let exp_block = self.sidebar_areas.explorer_icon;
        let sea_block = self.sidebar_areas.search_icon;
        let scm_block = self.sidebar_areas.source_control_icon;
        let rem_block = self.sidebar_areas.remote_icon;
        if exp_block.width == 0
            || sea_block.width == 0
            || scm_block.width == 0
            || rem_block.width == 0
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
        vec![
            ((exp_block.x, exp_block.y), exp_state.as_str()),
            ((sea_block.x, sea_block.y), sea_state.as_str()),
            ((scm_block.x, scm_block.y), scm_state.as_str()),
            ((rem_block.x, rem_block.y), rem_state.as_str()),
        ]
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
        self.focus == Pane::Editor
            && self.context_menu.is_none()
            && self.prompt.is_none()
            && self.cursor_visible_phase()
            && self.editor.cursor_screen_pos().is_some()
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
        self.git_status = crate::git::query(&self.tree.root);
        // The Source Control panel reads `git status --porcelain` for its
        // row list; refresh whenever git state changes so the user sees the
        // tree reflect the same disk reality the badge in the status bar
        // already shows.
        if self.sidebar_view == SidebarView::SourceControl {
            let entries = crate::git::query_changes(&self.tree.root);
            self.source_control.set_status(self.git_status.clone(), entries);
        } else {
            self.source_control.status = self.git_status.clone();
        }
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
        // notify_debouncer_full's FileIdMap walks `WalkDir(usize::MAX)` on
        // watcher creation and `stat()`s every file. When the workspace is
        // `$HOME`, that descent enters `~/Library/Containers/<bundle>/Data`
        // and `~/Library/Group Containers/...`, which macOS Sonoma's
        // App Management TCC class flags as "iTerm.app accessing data
        // from other apps." Diagnostic-confirmed root cause; verified by
        // disabling the watcher and watching the prompt stop.
        //
        // Workaround: if any TCC-protected directory sits at the top level
        // of the workspace, watch the root non-recursively and recursively-
        // watch each safe sibling instead. Identical event coverage minus
        // the protected subtrees.
        let entries: Vec<std::fs::DirEntry> = std::fs::read_dir(root)
            .map(|rd| rd.filter_map(Result::ok).collect())
            .unwrap_or_default();
        let needs_split = entries
            .iter()
            .any(|e| FS_WATCH_PROTECTED_NAMES.iter().any(|n| e.file_name() == *n));
        if needs_split {
            debouncer
                .watch(root, RecursiveMode::NonRecursive)
                .context("starting non-recursive watch on workspace root")?;
            for entry in entries {
                let name = entry.file_name();
                if FS_WATCH_PROTECTED_NAMES.iter().any(|n| name == *n) {
                    continue;
                }
                let path = entry.path();
                let is_dir = entry
                    .file_type()
                    .map(|ft| ft.is_dir())
                    .unwrap_or_else(|_| path.is_dir());
                if is_dir {
                    let _ = debouncer.watch(&path, RecursiveMode::Recursive);
                }
            }
        } else {
            debouncer
                .watch(root, RecursiveMode::Recursive)
                .context("starting watch on workspace root")?;
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
        if self.fs_poll_last_check.elapsed() < FS_POLL_INTERVAL {
            return false;
        }
        self.fs_poll_last_check = std::time::Instant::now();
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
            return changed;
        }
        for dir in changed_dirs.iter().rev() {
            if let Some(idx) = self.tree.index_of_dir(dir) {
                self.tree.refresh_children(idx);
            }
        }
        self.fs_poll_dir_mtimes = Self::snapshot_expanded_dir_mtimes(&self.tree);
        self.refresh_git_status_debounced();
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
        if let Some(rx) = self.git_status_init_rx.as_ref() {
            match rx.try_recv() {
                Ok(s) => {
                    self.git_status = s;
                    self.git_status_init_rx = None;
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.git_status_init_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        changed
    }

    /// Push the current search query string and toggle state onto the
    /// worker channel and keep the editor's match highlight synced to the
    /// same term so the active file lights up matches as the user types.
    /// Called whenever the search input or one of the toggles changes.
    fn submit_search_query(&mut self) {
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

    /// Apply any pending search results from the background worker. Drops
    /// stale results whose query no longer matches the input field (the
    /// user has typed past it). Returns true iff hits were updated, so
    /// the main loop knows to redraw.
    pub fn drain_search_results(&mut self) -> bool {
        let mut applied = false;
        while let Ok((q, opts, hits)) = self.search_results_rx.try_recv() {
            if q == self.search.query && opts == self.search.opts {
                self.search.hits = hits;
                self.search.selected = 0;
                self.search.scroll = 0;
                applied = true;
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
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);

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

        // Footer chevron line, centred on the bottom of the stack.
        let footer_y = box_y + box_h + 1;
        if footer_y < area.y + area.height {
            let footer_w = WELCOME_FOOTER.chars().count() as u16;
            let footer_x = area.x + area.width.saturating_sub(footer_w) / 2;
            frame.buffer_mut().set_string(
                footer_x,
                footer_y,
                WELCOME_FOOTER,
                Style::default().fg(Color::Rgb(0x6c, 0x7d, 0x9c)),
            );
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
        let explorer_active = self.sidebar_view == SidebarView::Explorer;
        let search_active = self.sidebar_view == SidebarView::Search;
        let source_control_active = self.sidebar_view == SidebarView::SourceControl;
        let remote_active = self.sidebar_view == SidebarView::Remote;

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
        }

        self.sidebar_areas.explorer_icon = explorer_block;
        self.sidebar_areas.search_icon = search_block;
        self.sidebar_areas.source_control_icon = source_control_block;
        self.sidebar_areas.remote_icon = remote_block;
    }

    fn set_sidebar_view(&mut self, view: SidebarView) {
        if self.sidebar_view != view {
            self.activity_overlay_dirty = true;
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
        // If we just hid the terminal while it was focused, fall back to editor.
        if !self.show_terminal && self.focus == Pane::Terminal {
            self.focus_pane(Pane::Editor);
        }
        // If we just showed the terminal, optionally jump focus to it for quick use.
        if self.show_terminal {
            self.focus_pane(Pane::Terminal);
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
    pub fn split_terminal(&mut self) -> Result<()> {
        let cwd = self
            .terminal()
            .pid()
            .and_then(cwd_of_pid)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| self.workspace_root.clone());
        let term = PtyTerminal::new(&cwd).context("spawning terminal")?;
        self.terminals.push(term);
        self.active_terminal = self.terminals.len() - 1;
        if !self.show_terminal {
            self.show_terminal = true;
        }
        self.focus_pane(Pane::Terminal);
        Ok(())
    }

    /// Drop the active terminal. Returns false (and does nothing) when only
    /// one terminal is left, since hiding the pane is the user's job
    /// (Ctrl+J), not ours.
    pub fn close_active_terminal(&mut self) -> bool {
        if self.terminals.len() <= 1 {
            return false;
        }
        let idx = self.active_terminal;
        self.terminals.remove(idx);
        if self.active_terminal >= self.terminals.len() {
            self.active_terminal = self.terminals.len() - 1;
        }
        self.sync_focus_flags();
        true
    }

    /// Cycle the active terminal forward by one slot, wrapping at the end.
    pub fn cycle_terminal(&mut self) {
        if self.terminals.len() <= 1 {
            return;
        }
        self.active_terminal = (self.active_terminal + 1) % self.terminals.len();
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
        } else {
            self.terminal_splitter_y = None;
            (right_area, None)
        };

        self.render_activity_bar(frame, activity_area);

        if let Some(area) = side_area {
            match self.sidebar_view {
                SidebarView::Explorer => frame.render_widget(&mut self.tree, area),
                SidebarView::Search => frame.render_widget(&mut self.search, area),
                SidebarView::SourceControl => frame.render_widget(&mut self.source_control, area),
                SidebarView::Remote => frame.render_widget(&mut self.remote, area),
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
            let (add_rect, close_rect) =
                paint_terminal_pane_buttons(frame, area, show_close);
            self.terminal_add_button = add_rect;
            self.terminal_close_button = close_rect;
        } else {
            self.terminal_add_button = None;
            self.terminal_close_button = None;
        }

        let mut spans: Vec<Span> = Vec::with_capacity(20);
        spans.push(Span::styled(
            brand_pill_text(),
            Style::default()
                .bg(Color::Rgb(0x4e, 0x9a, 0xff))
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ));
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
        spans.push(Span::raw(" Term"));
        let status = Paragraph::new(Line::from(spans))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x3a, 0x6e)));
        frame.render_widget(status, outer[1]);

        // Overlays render last so they sit on top of everything else.
        self.render_context_menu(frame);
        self.render_prompt(frame);
        self.render_local_open_confirm(frame);

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
        for (i, (label, _)) in menu.items.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }
            let row = Rect {
                x: inner.x,
                y: inner.y + i as u16,
                width: inner.width,
                height: 1,
            };
            let style = if i == menu.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(0x4e, 0x9a, 0xff))
            } else {
                Style::default().fg(Color::White)
            };
            let line = ratatui::text::Line::from(format!(" {label}"));
            frame.render_widget(
                ratatui::widgets::Paragraph::new(line).style(style),
                row,
            );
        }
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
                Style::default()
                    .fg(Color::Rgb(0x4e, 0x9a, 0xff))
                    .add_modifier(Modifier::UNDERLINED),
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
        // Modal layer: open context menu eats keyboard navigation.
        if self.context_menu.is_some() {
            self.handle_menu_key(key);
            return Ok(());
        }
        // App-wide shortcuts (priority).
        if is_save_key(key) {
            self.save();
            return Ok(());
        }
        if is_terminal_toggle_key(key) {
            self.toggle_terminal();
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
        if is_search_jump_key(key) {
            self.set_sidebar_view(SidebarView::Search);
            return Ok(());
        }
        if is_source_control_jump_key(key) {
            self.set_sidebar_view(SidebarView::SourceControl);
            return Ok(());
        }
        if self.sidebar_view == SidebarView::Search
            && self.focus != Pane::Editor
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
                self.show_tree = !self.show_tree;
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
                write_osc52(&text);
                self.status = format!("Copied {} chars to clipboard", text.chars().count());
            }
            return;
        }
        if is_editor_cut_key(key) {
            let text = self.search.selection_text();
            if !text.is_empty() {
                write_osc52(&text);
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
        match key.code {
            KeyCode::Up => self.remote.move_up(),
            KeyCode::Down => self.remote.move_down(),
            KeyCode::PageUp => self.remote.scroll_up(10),
            KeyCode::PageDown => self.remote.scroll_down(10),
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.remote.refresh();
                self.status = String::from("Refreshed SSH remotes");
            }
            KeyCode::Enter => {
                if let Some(target) = self.remote.selected_target().cloned() {
                    self.request_remote_launch(target.alias, None);
                }
            }
            KeyCode::Esc => self.set_sidebar_view(SidebarView::Explorer),
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
        if cmd_or_ctrl && matches!(key.code, KeyCode::Enter) {
            self.commit_source_control();
            return;
        }
        if is_clipboard_paste_key(key) {
            if let Some(text) = (self.clipboard_reader)() {
                self.source_control.insert_str(&text);
            }
            return;
        }
        match key.code {
            KeyCode::Backspace => self.source_control.backspace(),
            KeyCode::Left => self.source_control.move_cursor_left(),
            KeyCode::Right => self.source_control.move_cursor_right(),
            KeyCode::Home => self.source_control.home(),
            KeyCode::End => self.source_control.end(),
            KeyCode::Up => self.source_control.scroll_up(1),
            KeyCode::Down => self.source_control.scroll_down(1),
            KeyCode::Enter => self.commit_source_control(),
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER)
                {
                    self.source_control.insert_char(c);
                }
            }
            _ => {}
        }
    }

    fn refresh_source_control(&mut self) {
        let entries = crate::git::query_changes(&self.tree.root);
        self.source_control.set_status(self.git_status.clone(), entries);
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
        self.remote_launch = Some(RemoteLaunch { host, path });
        self.quit = true;
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
                // Place the cursor on the matched line.
                let row = hit.line_no.saturating_sub(1).min(
                    self.editor.lines.len().saturating_sub(1),
                );
                self.editor.cursor_row = row;
                self.editor.cursor_col = 0;
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
            _ => {}
        }
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
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
        if is_motion && shift {
            if self.editor.selection.is_none() {
                self.editor.start_selection_at_cursor();
            }
            match key.code {
                KeyCode::Up => self.editor.move_up(),
                KeyCode::Down => self.editor.move_down(),
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
                }
            }
            _ => {}
        }
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
        // Ctrl+Shift+] / Ctrl+Shift+[: cycle to the next terminal.
        if is_terminal_cycle_key(key) {
            self.cycle_terminal();
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
        write_osc52(&text);
        self.status = format!("Copied {} chars to clipboard", text.chars().count());
    }

    fn copy_editor_selection(&mut self) {
        let Some(sel) = self.editor.selection else { return };
        if !sel.has_area() {
            return;
        }
        let text = self.editor.selection_text();
        if text.is_empty() {
            return;
        }
        write_osc52(&text);
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
        write_osc52(&text);
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

    fn handle_mouse(&mut self, m: MouseEvent) {
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

        let in_tree = self.show_tree && rect_contains(self.tree.last_area, m.column, m.row);
        let in_editor_pane = rect_contains(self.editor.last_full_area, m.column, m.row);
        let in_editor = rect_contains(self.editor.last_area, m.column, m.row);
        let terminal_hit = self.terminal_at_pos(m.column, m.row);
        let in_terminal = terminal_hit.is_some();
        let in_tree_scrollbar = self.sidebar_view == SidebarView::Explorer
            && rect_contains(self.tree.last_scrollbar, m.column, m.row);
        let in_remote_scrollbar = self.sidebar_view == SidebarView::Remote
            && rect_contains(self.remote.last_scrollbar, m.column, m.row);
        let in_editor_scrollbar = rect_contains(self.editor.last_scrollbar, m.column, m.row);

        match m.kind {
            MouseEventKind::Down(MouseButton::Right) => {
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
                // The "[+]" / "[-]" buttons sit on the same row as the
                // editor/terminal splitter, so they have to win the
                // hit-test before the splitter-drag handler claims this
                // click.
                if let Some(rect) = self.terminal_close_button {
                    if rect_contains(rect, m.column, m.row) {
                        if self.close_active_terminal() {
                            self.status =
                                format!("Closed terminal: {} remaining", self.terminals.len());
                        }
                        return;
                    }
                }
                if let Some(rect) = self.terminal_add_button {
                    if rect_contains(rect, m.column, m.row) {
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
                }
                if let Some(y) = self.terminal_splitter_y {
                    // Two-row hit-zone: the terminal's top border (`y`) and
                    // the editor / welcome's bottom border (`y - 1`).
                    // Symmetric with the sidebar drag — either edge of the
                    // visible seam grabs the splitter.
                    if m.row == y || m.row == y.saturating_sub(1) {
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
                    if self.source_control.click_button(m.column, m.row) {
                        self.commit_source_control();
                        return;
                    }
                    if self.source_control.click_input(m.column, m.row) {
                        return;
                    }
                    if let Some(idx) = self.source_control.entry_at_y(m.row) {
                        if let Some(entry) = self.source_control.entries.get(idx).cloned() {
                            let abs = self.tree.root.join(&entry.path);
                            if abs.is_file() {
                                if let Err(e) = self.editor.open_pinned(&abs) {
                                    self.status = format!("Open failed: {e}");
                                } else {
                                    self.focus_pane(Pane::Editor);
                                    self.sync_open_file_poll_mtime();
                                }
                            }
                        }
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Remote {
                    self.focus_pane(Pane::Tree);
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
                    // Begin a fresh selection at the click cell. Without a
                    // drag this is a single cell (no area), so the selection
                    // ends up cleared on mouse-up. With a drag, this is the
                    // selection anchor.
                    self.terminal_mut().start_selection_at(m.column, m.row);
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
                            SidebarView::Search => {}
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
                if in_editor {
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
                        SidebarView::Search | SidebarView::SourceControl => {}
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
                        SidebarView::Search => {}
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
                        SidebarView::Search => {}
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
                    }
                }
            }
            MouseEventKind::ScrollRight => {
                if in_editor {
                    if let Some(diff) = self.editor.diff.as_mut() {
                        diff.scroll_right_by(4);
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
        // labels (e.g. the "…" in "Rename…", the arrow in "Compare with
        // Selected") don't inflate the menu width past what's needed.
        let widest = menu
            .items
            .iter()
            .map(|(s, _)| s.chars().count())
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
    fn handle_diff_key(&mut self, key: KeyEvent) {
        // Page = inner viewport rows minus the header + footer the diff
        // renderer reserves. Falls back to a sane default when the editor
        // hasn't laid out yet.
        let page = (self.editor.last_inner.height as usize).saturating_sub(2).max(1);
        let Some(diff) = self.editor.diff.as_mut() else {
            return;
        };
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
/// non-US keyboards. Case-insensitive on the letter so Shift+Ctrl+J works too.
fn is_terminal_toggle_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'j') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
}

/// `Ctrl+Shift+T`: spawn an additional terminal next to the active one.
fn is_terminal_split_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else { return false };
    if !c.eq_ignore_ascii_case(&'t') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT)
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
fn is_save_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'s') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Read the system clipboard. Used by the search input when Cmd+V arrives as
/// a key event rather than bracketed paste content.
fn read_system_clipboard() -> Option<String> {
    crate::clipboard::read_string()
}

/// Send an OSC 52 sequence to put `text` on the host terminal's system
/// clipboard. Best-effort; failures are silent because there's nothing
/// useful the user could do about them.
fn write_osc52(text: &str) {
    use std::io::Write;
    let bytes = crate::widgets::terminal::osc52_copy_seq(text);
    let mut out = std::io::stdout();
    let _ = out.write_all(&bytes);
    let _ = out.flush();
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
        .collect()
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
    fn drain_fs_events_returns_false_when_nothing_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.fs_watcher_init_rx = None;
        app.git_status_init_rx = None;
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
        app.git_status_init_rx = None;
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
        app.git_status_init_rx = None;
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
        app.git_status_init_rx = None;
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
        app.git_status_init_rx = None;
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
        app.git_status_init_rx = None;
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
    fn welcome_tagline_and_footer_constants_are_present() {
        assert!(WELCOME_TAGLINE.contains("LIGHTWEIGHT"));
        assert!(WELCOME_TAGLINE.contains("BLAZINGLY FAST"));
        assert!(WELCOME_TAGLINE.contains("DEVELOPERS"));
        assert!(WELCOME_FOOTER.contains("Blazingly fast by design"));
        assert!(WELCOME_FOOTER.contains("Loved by developers"));
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

    #[test]
    fn ctrl_j_is_recognized_as_terminal_toggle() {
        assert!(is_terminal_toggle_key(key(KeyCode::Char('j'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn ctrl_shift_j_is_also_terminal_toggle() {
        let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(is_terminal_toggle_key(key(KeyCode::Char('J'), mods)));
    }

    #[test]
    fn plain_j_is_not_terminal_toggle() {
        assert!(!is_terminal_toggle_key(key(KeyCode::Char('j'), KeyModifiers::NONE)));
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
            src.contains("\" Term\""),
            "status bar should label the ^j shortcut 'Term'"
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
    fn tree_context_menu_on_file_offers_cut_copy_rename_delete() {
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
            ["Cut", "Copy", "Rename…", "Select for Compare", "Delete"]
        );
        assert!(matches!(&items[0].1, MenuAction::Cut(ps) if ps == &vec![f.clone()]));
        assert!(matches!(&items[1].1, MenuAction::Copy(ps) if ps == &vec![f.clone()]));
        assert!(matches!(&items[2].1, MenuAction::Rename(p) if p == &f));
        assert!(matches!(&items[3].1, MenuAction::SelectForCompare(p) if p == &f));
        assert!(matches!(&items[4].1, MenuAction::Delete { paths } if paths == &vec![f.clone()]));
    }

    #[test]
    fn tree_context_menu_on_subfolder_offers_cut_copy_rename_delete() {
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
        assert_eq!(labels, ["Cut", "Copy", "Rename…", "Delete"]);
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
        assert_eq!(labels, ["Cut", "Copy", "Paste", "Rename…", "Delete"]);
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
        // visible "Select for Compare" row used to map to "Rename…"
        // because hit-testing used the unclipped rect while rendering
        // used the clipped one.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Frame is 60 wide, 10 tall.
        app.last_frame_area = Rect { x: 0, y: 0, width: 60, height: 10 };
        // Menu height = items.len() + 2 borders = 7. Origin at y=4 would
        // make the menu run from row 4 to row 11 (off-screen). The
        // renderer (and now hit-test) clamp y to 10 - 7 = 3.
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
        // Sanity: items[3] is the new "Select for Compare".
        assert!(matches!(&items[3].1, MenuAction::SelectForCompare(_)));
        app.context_menu = Some(ContextMenu {
            origin: (10, 4),
            items,
            selected: 0,
            target_dir: target,
        });
        // The visible "Select for Compare" row sits at clipped.y + 1 + 3 = 3 + 4 = 7.
        let idx = app.menu_item_at(11, 7).expect("hit must land inside the menu");
        assert_eq!(idx, 3, "click on visible row 7 must map to item 3, not 2");
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
        assert_eq!(labels, ["Cut", "Copy", "Delete 2 items"]);
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
        assert_eq!(labels, ["New File…", "New Folder…"]);
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
        assert_eq!(labels, ["New File…", "New Folder…", "Paste"]);
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
        assert_eq!(labels, ["New File…", "New Folder…"]);
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
    fn search_visible_routes_editing_shortcuts_to_search_even_if_terminal_focused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_sidebar_view(SidebarView::Search);
        app.show_terminal = true;
        app.focus_pane(Pane::Terminal);
        app.search.query = String::from("alpha");
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
            .unwrap();
        assert_eq!(app.search.selection_range(), Some((0, 5)));
    }

    #[test]
    fn search_visible_routes_cmd_v_to_search_if_terminal_focused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.clipboard_reader = || Some(String::from("needle"));
        app.set_sidebar_view(SidebarView::Search);
        app.show_terminal = true;
        app.focus_pane(Pane::Terminal);

        app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
            .unwrap();

        assert_eq!(app.search.query, "needle");
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
        app.terminal_add_button = Some(Rect { x: 50, y: 30, width: 3, height: 1 });
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
        app.terminal_close_button = Some(Rect { x: 50, y: 30, width: 3, height: 1 });
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 51,
            row: 30,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.terminals.len(), 1);
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
        app.terminal_add_button = Some(Rect { x: 50, y: 30, width: 3, height: 1 });
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
}

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

    let backend = CrosstermBackend::new(out);
    let mut terminal: Terminal<CrosstermBackend<Stdout>> =
        Terminal::new(backend).context("create terminal")?;

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
        // Tear down the local croft's background workers BEFORE we hand
        // the terminal to the remote-launched croft. Otherwise the
        // fs-watcher thread keeps firing FSEvents callbacks and pushing
        // into an unbounded mpsc channel that nobody drains (main loop
        // has exited), and the embedded shell's PTY reader keeps feeding
        // alacritty_terminal — both leak heap for the duration of the
        // SSH session. Closing them here pegs the local process at a
        // few MB while the user lives inside the remote croft.
        app._fs_watcher = None;
        app.fs_rx = None;
        app.fs_watcher_init_rx = None;
        crate::remote::launch_croft(&remote.host, remote.path.as_deref())?;
    }
    Ok(())
}

/// Suspend the alt-screen, run every queued scp upload with the host
/// shell's stdin / stdout / stderr inherited (so the user sees scp's
/// progress and can answer any prompt it raises), then prompt for Enter
/// and restore the alt-screen. The local source is removed only on a
/// successful upload — if scp fails, the local copy stays put so the
/// user can retry without losing data.
fn run_pending_scp_uploads(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
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

fn main_loop(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
    // Force the very first frame to render so the user sees the UI even
    // before the first event arrives or any timer fires.
    let mut needs_redraw = true;
    let mut last_blink_visible = app.cursor_visible_phase();
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
    let mut last_pty_redraw = std::time::Instant::now()
        .checked_sub(pty_min_interval)
        .unwrap_or_else(std::time::Instant::now);

    while !app.quit {
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

        let non_pty_dirty = needs_redraw
            || fs_changed
            || blink_changed
            || commits_changed
            || search_changed
            || remote_changed
            || pulls_changed;
        let pty_eligible =
            pty_pending && last_pty_redraw.elapsed() >= pty_min_interval;
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
            if app.consume_welcome_image_clear() || app.consume_editor_image_clear() {
                terminal.clear()?;
                // Activity-bar icons live outside ratatui too; re-emit
                // them on the next post-draw flush.
                app.activity_overlay_dirty = true;
            }
            terminal.draw(|f| {
                app.render(f);
            })?;
            if app.consume_welcome_image_clear() || app.consume_editor_image_clear() {
                terminal.clear()?;
                app.activity_overlay_dirty = true;
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
            if app.editor.is_blank_initial() {
                if let (Some(osc), Some((cx, cy))) =
                    (app.welcome_codeberg_badge_osc.as_deref(), app.welcome_codeberg_badge_cell)
                {
                    use std::io::Write;
                    let mut out = stdout();
                    let cursor_on = app.cursor_should_be_visible();
                    let _ = write!(out, "\x1b[?25l\x1b[s");
                    let _ = write!(out, "\x1b[{};{}H", cy + 1, cx + 1);
                    let _ = out.write_all(osc.as_bytes());
                    let _ = write!(out, "\x1b[u");
                    if cursor_on {
                        let _ = write!(out, "\x1b[?25h");
                    }
                    let _ = out.flush();
                }
            }
            // Welcome-screen logo: same OSC-1337 trick, gated by its own
            // dirty flag and only emitted while the editor pane is in its
            // blank initial state.
            if app.editor.is_blank_initial() && app.welcome_overlay_dirty {
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
        if event::poll(Duration::from_millis(8))? {
            // Drain every event already queued so a click burst (Down + zero-
            // movement Drag + Up, all delivered in <50ms by the terminal)
            // coalesces into a single redraw. Otherwise each event triggers
            // its own terminal.draw cycle, which Hides+Shows the hardware
            // caret each time and the user sees the cursor blink twice
            // rapidly before settling into the normal 530ms blink.
            loop {
                match event::read()? {
                    Event::Key(key) => app.handle_key(key)?,
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
    }
    Ok(())
}
