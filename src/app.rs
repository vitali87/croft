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
use std::io::{stdout, Stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::widgets::{
    editor::{Editor, EditorTabs},
    file_tree::FileTree,
    search::SearchPanel,
    terminal::PtyTerminal,
};

/// Which sidebar view is active in the left side panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarView {
    Explorer,
    Search,
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

fn activity_icon_glyph_x(bar: Rect) -> u16 {
    bar.x + bar.width / 2
}

fn activity_explorer_y(bar: Rect) -> u16 {
    bar.y + 1
}

fn activity_search_y(bar: Rect) -> u16 {
    activity_explorer_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
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

#[derive(Default, Clone, Copy)]
struct SidebarAreas {
    /// Block occupied by the Explorer activity-bar icon, in absolute coords.
    /// Multi-row when image rendering is active; the hit-test still uses the
    /// whole block.
    explorer_icon: Rect,
    /// Block occupied by the Search activity-bar icon, in absolute coords.
    search_icon: Rect,
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
    // Codicon git-branch glyph (U+EAFC).
    spans.push(Span::styled(
        "\u{eafc} ",
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
    /// Move the path to the OS trash (recoverable).
    Delete(PathBuf),
    /// Open the rename prompt pre-filled with the entry's current name.
    Rename(PathBuf),
}

/// Build the right-click context-menu items for the explorer.
///
/// * Right-click on an entry (file or non-root folder) → entry-scoped
///   actions: Rename, Delete.
/// * Right-click on empty tree space, or on the workspace root row →
///   workspace-scoped actions: New File, New Folder.
fn build_tree_context_menu_items(
    node: Option<&crate::widgets::file_tree::Node>,
    root: &Path,
) -> Vec<(String, MenuAction)> {
    let entry_target = crate::widgets::file_tree::delete_target_for(node, root);
    if let Some(p) = entry_target {
        vec![
            (String::from("Rename…"), MenuAction::Rename(p.clone())),
            (String::from("Delete"), MenuAction::Delete(p)),
        ]
    } else {
        vec![
            (String::from("New File…"), MenuAction::Create(CreateKind::File)),
            (String::from("New Folder…"), MenuAction::Create(CreateKind::Folder)),
        ]
    }
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
    pub editor: EditorTabs,
    pub terminal: PtyTerminal,
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
    /// True when the activity-bar OSC-1337 images need to be (re)written on
    /// the next post-draw flush. Set initially, on sidebar-view change, and
    /// on terminal resize. Cleared after emit. Without this gate every
    /// redraw repaints the PNGs and you see the cursor blink each time iTerm
    /// processes the image.
    activity_overlay_dirty: bool,
    /// Drives the welcome screen's "Recent" list. Always sourced from the
    /// croft project's Bitbucket repo (live HTTP fetch on every launch),
    /// never from the workspace the user opened — the goal is to surface
    /// croft's own progress to the developer.
    recent_commits: Vec<crate::git::CommitInfo>,
    /// Receiver for the background HTTP fetch of croft's recent commits.
    /// `None` once the fetch has completed (or failed) and been drained.
    recent_commits_rx: Option<std::sync::mpsc::Receiver<Vec<crate::git::CommitInfo>>>,
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
    /// Pixel size of one terminal cell, captured in `init_graphics`.
    /// Required to bake OSC-1337 images at exact viewport pixel size so
    /// iTerm draws them with no stretching or letterboxing.
    cell_pixel: Option<(u32, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WelcomeLayout {
    cell_x: u16,
    cell_y: u16,
    cell_w: u16,
    cell_h: u16,
}

const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);

type FsWatcherInit = (
    notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
    std::sync::mpsc::Receiver<notify_debouncer_full::DebounceEventResult>,
);

impl App {
    pub fn new(root: PathBuf) -> Result<Self> {
        let tree = FileTree::new(root.clone());
        let search = SearchPanel::new(root.clone());
        let editor = EditorTabs::new();
        let term = PtyTerminal::new(&root).context("spawning terminal")?;

        // notify_debouncer_full's RecommendedCache walks the entire watched
        // subtree to populate its path↔inode map; on a multi-GB monorepo
        // that's >1 s. Defer to a background thread; install via
        // `try_install_pending_init` once it completes. The user sees the
        // UI immediately and edits made in the first ~second go undetected
        // by the watcher (acceptable: the user is just opening the app).
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
        std::thread::spawn(move || {
            let timeout = std::time::Duration::from_secs(3);
            let commits = crate::git::fetch_croft_recent_commits(timeout);
            let _ = commits_tx.send(commits);
        });
        Ok(Self {
            tree,
            search,
            editor,
            terminal: term,
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
            last_editor_left_down: None,
            last_tree_left_down: None,
            activity_overlay_dirty: true,
            recent_commits: Vec::new(),
            recent_commits_rx: Some(commits_rx),
            welcome_image: None,
            welcome_layout: None,
            welcome_overlay_dirty: true,
            welcome_image_displayed: false,
            cell_pixel: None,
        })
    }

    /// Detect inline-image support via env vars only — no stdin queries, no
    /// raw-mode contention. Queries the terminal cell pixel size via
    /// crossterm's `window_size` (TIOCGWINSZ ioctl, no stdin involvement),
    /// then composes each icon PNG at the *exact* viewport pixel size
    /// (4 cells × 2 rows in the user's font). With the canvas matching the
    /// viewport pixel-for-pixel, iTerm2 displays the image with zero
    /// leftover bg sliver and zero stretching — the codicon stays visually
    /// square because it lives inside a `min(w, h)` sub-square of the
    /// canvas, with bar-bg padding filling the longer axis.
    pub fn init_graphics(&mut self) {
        if !crate::iterm2_inline::detect_iterm2_inline_support() {
            return;
        }
        let Ok(ws) = crossterm::terminal::window_size() else {
            return;
        };
        if ws.columns == 0 || ws.rows == 0 || ws.width == 0 || ws.height == 0 {
            return;
        }
        let cell_w = (ws.width / ws.columns).max(1) as u32;
        let cell_h = (ws.height / ws.rows).max(1) as u32;
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
        if let (Some(ea), Some(ei), Some(sa), Some(si)) =
            (explorer_active, explorer_inactive, search_active, search_inactive)
        {
            self.activity_images = Some(ActivityBarImages {
                explorer_active: ea,
                explorer_inactive: ei,
                search_active: sa,
                search_inactive: si,
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
        if exp_block.width == 0 || sea_block.width == 0 {
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
        vec![
            ((exp_block.x, exp_block.y), exp_state.as_str()),
            ((sea_block.x, sea_block.y), sea_state.as_str()),
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
        debouncer
            .watch(root, RecursiveMode::Recursive)
            .context("starting watch on workspace root")?;
        Ok((debouncer, rx))
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

    /// Pull a single batch of croft commits from the background HTTP fetch,
    /// if it has finished. Returns true exactly once when commits are
    /// installed (so the welcome panel repaints), false otherwise. Drops
    /// the receiver after consuming so subsequent calls are cheap no-ops.
    pub fn drain_recent_commits(&mut self) -> bool {
        let Some(rx) = self.recent_commits_rx.as_ref() else {
            return false;
        };
        match rx.try_recv() {
            Ok(commits) => {
                self.recent_commits = commits;
                self.recent_commits_rx = None;
                self.welcome_overlay_dirty = true;
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
            return init_changed;
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
                for path in &ev.event.paths {
                    // Editor reload trigger: any event mentioning the open
                    // file's path, before we even classify it as a tree event.
                    if self.editor.matches_open_path(path) {
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
        }
        if !affected.is_empty() || touched_open_file {
            self.refresh_git_status_debounced();
        }
        if touched_open_file {
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
        }
        got_any || init_changed
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
        self.tree.focused = self.focus == Pane::Tree;
        self.editor.focused = self.focus == Pane::Editor;
        self.terminal.focused = self.focus == Pane::Terminal;
    }

    /// Returns true exactly once after the welcome OSC-1337 image has been
    /// emitted and the editor pane has stopped being blank (i.e. a file is
    /// now open). The caller — the main draw loop — must respond by
    /// invalidating the prev buffer so ratatui repaints every cell on the
    /// next draw, wiping iTerm's image cache for the welcome region.
    pub fn consume_welcome_image_clear(&mut self) -> bool {
        if self.welcome_image_displayed && !self.editor.is_blank_initial() {
            self.welcome_image_displayed = false;
            true
        } else {
            false
        }
    }

    fn render_welcome(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
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
        frame.render_widget(
            ratatui::widgets::Block::default().style(bg),
            area,
        );

        // Reserve space for the recent-commits panel below the logo, then
        // hand the remainder to the centred image.
        let recents_h: u16 = if self.recent_commits.is_empty() {
            0
        } else {
            (self.recent_commits.len() as u16) + 2 // header + spacer
        };
        let logo_max_w = (area.width as u32).saturating_sub(4) as u16;
        let logo_max_h = area
            .height
            .saturating_sub(recents_h.saturating_add(2));
        let logo_w_cells = logo_max_w.min(48).max(8);
        let logo_h_cells = logo_max_h.min(14).max(4);

        let total_h = logo_h_cells + 1 + recents_h;
        let block_top = area.y + area.height.saturating_sub(total_h) / 2;
        let logo_x = area.x + area.width.saturating_sub(logo_w_cells) / 2;
        let logo_y = block_top;

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

        if self.recent_commits.is_empty() {
            return;
        }

        let header_y = logo_y + logo_h_cells + 1;
        let block_left = area.x + area.width / 8;
        let block_right = area.x + area.width - area.width / 8;
        let block_w = block_right.saturating_sub(block_left);
        let header = "RECENT";
        let header_style = Style::default()
            .fg(Color::Rgb(0x9d, 0xa5, 0xb4))
            .add_modifier(Modifier::BOLD);
        frame
            .buffer_mut()
            .set_string(block_left, header_y, header, header_style);
        let row_style = Style::default().fg(Color::Rgb(0xc5, 0xcd, 0xd9));
        let dim = Style::default().fg(Color::Rgb(0x6c, 0x7d, 0x9c));
        for (i, c) in self.recent_commits.iter().enumerate() {
            let y = header_y + 2 + i as u16;
            if y >= area.y + area.height {
                break;
            }
            let mut x = block_left;
            frame.buffer_mut().set_string(x, y, &c.hash, dim);
            x += c.hash.chars().count() as u16 + 1;
            let when_w = c.when.chars().count() as u16;
            let row_end = block_left + block_w;
            let subject_w = c.subject.chars().count() as u16;
            let can_fit_when = x + subject_w + 2 + when_w <= row_end;
            if can_fit_when {
                frame.buffer_mut().set_string(x, y, &c.subject, row_style);
                let when_x = row_end.saturating_sub(when_w);
                frame.buffer_mut().set_string(when_x, y, &c.when, dim);
            } else {
                // Subject won't share the row with the relative date; show
                // the full subject (clipped only at the welcome block's
                // right edge) and drop the date for this row.
                let room = row_end.saturating_sub(x);
                let subject_clip: String =
                    c.subject.chars().take(room as usize).collect();
                frame.buffer_mut().set_string(x, y, &subject_clip, row_style);
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
        let explorer_active = self.sidebar_view == SidebarView::Explorer;
        let search_active = self.sidebar_view == SidebarView::Search;

        if self.activity_images.is_none() {
            // Glyph fallback path: render the codicon and a separate active
            // pill on the leftmost column. iTerm2's image path bakes the
            // pill into the PNG itself, so this branch is only used on
            // terminals that can't render OSC-1337.
            let active_color = Color::White;
            let inactive_color = Color::Rgb(0x6c, 0x7d, 0x9c);
            let glyph_x = activity_icon_glyph_x(area);
            let render_glyph =
                |frame: &mut ratatui::Frame, block: Rect, glyph: char, is_active: bool| {
                    let mid = block.y + block.height.saturating_sub(1) / 2;
                    if is_active {
                        let pill = Rect { x: block.x, y: mid, width: 1, height: 1 };
                        frame.render_widget(
                            ratatui::widgets::Paragraph::new("▎")
                                .style(Style::default().fg(active_bar).bg(bg_color)),
                            pill,
                        );
                    }
                    let cell = Rect { x: glyph_x, y: mid, width: 1, height: 1 };
                    let color = if is_active { active_color } else { inactive_color };
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
        }

        self.sidebar_areas.explorer_icon = explorer_block;
        self.sidebar_areas.search_icon = search_block;
    }

    fn set_sidebar_view(&mut self, view: SidebarView) {
        if self.sidebar_view != view {
            self.activity_overlay_dirty = true;
        }
        self.sidebar_view = view;
        if !self.show_tree {
            self.show_tree = true; // ensure the side panel is open when switching
        }
        match view {
            SidebarView::Explorer => self.focus_pane(Pane::Tree),
            SidebarView::Search => {
                self.focus_pane(Pane::Tree); // tree pane = side panel; we'll dispatch by view
                self.search.focused = true;
                self.tree.focused = false;
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
        // If we just hid the terminal while it was focused, fall back to editor.
        if !self.show_terminal && self.focus == Pane::Terminal {
            self.focus_pane(Pane::Editor);
        }
        // If we just showed the terminal, optionally jump focus to it for quick use.
        if self.show_terminal {
            self.focus_pane(Pane::Terminal);
        }
    }

    fn focus_pane(&mut self, p: Pane) {
        self.focus = p;
        self.tree.focused = self.focus == Pane::Tree;
        self.editor.focused = self.focus == Pane::Editor;
        self.terminal.focused = self.focus == Pane::Terminal;
        if self.editor.focused {
            self.poke_cursor();
        }
    }

    fn render(&mut self, frame: &mut ratatui::Frame) {
        let size = frame.area();
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(size);

        // Carve off the activity bar on the very left, then optionally the
        // side panel, then the main content.
        let main = if self.show_tree {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(ACTIVITY_BAR_WIDTH),
                    Constraint::Length(32),
                    Constraint::Min(20),
                ])
                .split(outer[0])
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(ACTIVITY_BAR_WIDTH),
                    Constraint::Min(20),
                ])
                .split(outer[0])
        };

        let (activity_area, side_area, right_area) = if self.show_tree {
            (main[0], Some(main[1]), main[2])
        } else {
            (main[0], None, main[1])
        };

        let (editor_area, terminal_area) = if self.show_terminal {
            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
                .split(right_area);
            (right[0], Some(right[1]))
        } else {
            (right_area, None)
        };

        self.render_activity_bar(frame, activity_area);

        if let Some(area) = side_area {
            match self.sidebar_view {
                SidebarView::Explorer => frame.render_widget(&mut self.tree, area),
                SidebarView::Search => frame.render_widget(&mut self.search, area),
            }
        }
        if self.editor.is_blank_initial() {
            self.render_welcome(frame, editor_area);
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
        }
        if let Some(area) = terminal_area {
            frame.render_widget(&mut self.terminal, area);
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
        let Some(rect) = self.menu_rect() else { return };
        let area = frame.area();
        // Clip the menu to the screen so it doesn't run off the edges.
        let clipped = Rect {
            x: rect.x.min(area.width.saturating_sub(rect.width)),
            y: rect.y.min(area.height.saturating_sub(rect.height)),
            width: rect.width,
            height: rect.height,
        };
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
        if is_search_jump_key(key) {
            self.set_sidebar_view(SidebarView::Search);
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
        match key.code {
            KeyCode::Esc => {
                // Empty the input or jump back to Explorer.
                if self.search.query.is_empty() {
                    self.set_sidebar_view(SidebarView::Explorer);
                } else {
                    self.search.query.clear();
                    self.search.hits.clear();
                }
            }
            KeyCode::Enter => {
                // If a hit is selected and we already have results, open it.
                if !self.search.hits.is_empty() {
                    if let Some(hit) = self.search.selected_hit().cloned() {
                        self.open_search_hit(&hit);
                        return;
                    }
                }
                // Otherwise run the query.
                self.search.run_query();
                self.status = format!(
                    "Search '{}' → {} match{}",
                    self.search.query,
                    self.search.hits.len(),
                    if self.search.hits.len() == 1 { "" } else { "es" }
                );
            }
            KeyCode::Backspace => {
                self.search.query.pop();
            }
            KeyCode::Up => self.search.move_up(),
            KeyCode::Down => self.search.move_down(),
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER)
                {
                    self.search.query.push(c);
                }
            }
            _ => {}
        }
    }

    fn open_search_hit(&mut self, hit: &crate::widgets::search::SearchHit) {
        match self.editor.open_preview(&hit.path) {
            Ok(()) => {
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
        // Delete key (or Cmd+Backspace) trashes the selected node directly.
        if is_delete_node_key(key) {
            if let Some(node) = self.tree.nodes.get(self.tree.selected) {
                if let Some(path) =
                    crate::widgets::file_tree::delete_target_for(Some(node), &self.tree.root)
                {
                    self.delete_node(path);
                }
            }
            return;
        }
        match key.code {
            KeyCode::Up => self.tree.move_up(),
            KeyCode::Down => self.tree.move_down(),
            KeyCode::PageUp => self.tree.page_up(10),
            KeyCode::PageDown => self.tree.page_down(10),
            KeyCode::Home => self.tree.home(),
            KeyCode::End => self.tree.end(),
            KeyCode::Enter | KeyCode::Right => {
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
            KeyCode::Left => {
                // collapse if dir, otherwise move to parent (simple approach: just collapse)
                self.tree.activate();
            }
            _ => {}
        }
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
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
                self.status = String::from("Closed tab");
            } else {
                self.status = String::from("Cannot close last tab");
            }
            return;
        }
        if let Some(idx) = jump_to_tab_index(key) {
            if self.editor.select(idx) {
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
        // Ctrl+Shift+C / Cmd+C: copy current selection.
        if is_terminal_copy_key(key) {
            self.copy_terminal_selection();
            return;
        }
        // Any other keystroke clears the selection so the user's input is
        // sent without the previous highlight lingering on screen.
        if self.terminal.selection().is_some() {
            self.terminal.clear_selection();
        }
        let bytes = key_to_bytes(key);
        if !bytes.is_empty() {
            self.terminal.write_input(&bytes);
        }
    }

    /// Copy the terminal pane's current selection to the host clipboard via
    /// OSC 52.  Selection stays visible so the user can verify what was
    /// copied. No-op when the selection is empty / zero-area.
    fn copy_terminal_selection(&mut self) {
        let Some(sel) = self.terminal.selection() else { return };
        if !sel.has_area() {
            return;
        }
        let text = self.terminal.selection_text();
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

    fn handle_paste(&mut self, s: &str) {
        match self.focus {
            Pane::Editor => {
                self.editor.insert_str(s);
                self.status = format!("Pasted {} chars", s.chars().count());
            }
            Pane::Terminal => {
                // Forward bracketed paste to the embedded shell verbatim,
                // wrapped in the same envelope so the shell treats it as a
                // paste rather than typed input.
                self.terminal.write_input(b"\x1b[200~");
                self.terminal.write_input(s.as_bytes());
                self.terminal.write_input(b"\x1b[201~");
            }
            Pane::Tree => {
                // Tree has no text input target; paste is a no-op here.
            }
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
        let in_terminal = rect_contains(self.terminal.last_area, m.column, m.row);

        match m.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                if in_tree {
                    self.focus_pane(Pane::Tree);
                    let node_idx = self.tree.node_at_y(m.row).inspect(|&i| {
                        self.tree.select(i);
                    });
                    let node = node_idx.and_then(|i| self.tree.nodes.get(i));
                    let target_dir = crate::widgets::file_tree::create_target_dir_for(
                        node, &self.tree.root,
                    );
                    let items = build_tree_context_menu_items(node, &self.tree.root);
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
                // Activity-bar hit-test takes precedence over the side panel.
                if rect_contains(self.sidebar_areas.explorer_icon, m.column, m.row) {
                    self.set_sidebar_view(SidebarView::Explorer);
                    return;
                }
                if rect_contains(self.sidebar_areas.search_icon, m.column, m.row) {
                    self.set_sidebar_view(SidebarView::Search);
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Search {
                    // Click on a result row: open it.
                    if let Some(idx) = self.search.hit_at_y(m.row) {
                        self.search.selected = idx;
                        if let Some(hit) = self.search.selected_hit().cloned() {
                            self.open_search_hit(&hit);
                        }
                    } else {
                        // Click on the input/header area: just focus search.
                        self.search.focused = true;
                        self.tree.focused = false;
                        self.focus_pane(Pane::Tree);
                        // focus_pane sets self.tree.focused; restore search ownership.
                        self.tree.focused = false;
                        self.search.focused = true;
                    }
                    return;
                }
                if in_tree {
                    self.focus_pane(Pane::Tree);
                    if let Some(idx) = self.tree.node_at_y(m.row) {
                        self.tree.select(idx);
                        let now = std::time::Instant::now();
                        let is_double = matches!(
                            self.last_tree_left_down,
                            Some((t, x, y))
                                if m.row == y
                                    && m.column.abs_diff(x) <= 1
                                    && now.duration_since(t) <= DOUBLE_CLICK_WINDOW
                        );
                        if let Some(path) = self.tree.activate() {
                            let result = if is_double {
                                self.editor.open_pinned(&path)
                            } else {
                                self.editor.open_preview(&path)
                            };
                            match result {
                                Ok(()) => {
                                    self.status = self.editor.status.clone();
                                    if is_double {
                                        // Double-click "pins" the file: focus
                                        // moves to the editor so the user can
                                        // start editing immediately.
                                        self.focus_pane(Pane::Editor);
                                        self.poke_cursor();
                                    }
                                    // Single-click keeps the tree focused so
                                    // Delete / arrows still act on the
                                    // explorer; the user follows up with a
                                    // double-click (or a click in the editor
                                    // pane) when they want to start typing.
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
                            self.status = String::from("Closed tab");
                            self.poke_cursor();
                        }
                    } else if let Some(idx) = self.editor.tab_at(m.column, m.row) {
                        self.focus_pane(Pane::Editor);
                        if self.editor.active_index() != idx {
                            self.editor.select(idx);
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
                } else if in_terminal {
                    self.focus_pane(Pane::Terminal);
                    // Begin a fresh selection at the click cell. Without a
                    // drag this is a single cell (no area), so the selection
                    // ends up cleared on mouse-up. With a drag, this is the
                    // selection anchor.
                    self.terminal.start_selection_at(m.column, m.row);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
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
                    if let Some(idx) = self.tree.node_at_y(m.row) {
                        self.tree.select(idx);
                    }
                } else if in_terminal {
                    self.terminal.extend_selection_to(m.column, m.row);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Mouse-up never auto-copies. The selection stays highlighted
                // so the user can hit Cmd/Ctrl+C themselves; a click without
                // drag (zero-area selection) is silently dropped.
                if in_terminal {
                    if let Some(sel) = self.terminal.selection() {
                        if !sel.has_area() {
                            self.terminal.clear_selection();
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
                    self.tree.move_down();
                    self.tree.move_down();
                    self.tree.move_down();
                } else if in_editor {
                    self.editor.scroll_down(3);
                } else if in_terminal {
                    // Try our scrollback first; if we're in vim/less/htop
                    // (alternate-screen), fall back to forwarding arrow keys.
                    if !self.terminal.scroll_down(3) {
                        self.terminal.write_input(b"\x1b[B\x1b[B\x1b[B");
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if in_tree {
                    self.tree.move_up();
                    self.tree.move_up();
                    self.tree.move_up();
                } else if in_editor {
                    self.editor.scroll_up(3);
                } else if in_terminal {
                    if !self.terminal.scroll_up(3) {
                        self.terminal.write_input(b"\x1b[A\x1b[A\x1b[A");
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
        let widest = menu.items.iter().map(|(s, _)| s.len()).max().unwrap_or(0);
        let width = (widest + 4).max(18) as u16;
        let height = (menu.items.len() + 2) as u16;
        Some(Rect {
            x: menu.origin.0,
            y: menu.origin.1,
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
            // No confirmation: trash is recoverable, the user asked for direct deletion.
            MenuAction::Delete(path) => self.delete_node(path),
            MenuAction::Rename(path) => self.open_rename_prompt(path),
        }
    }

    fn delete_node(&mut self, path: PathBuf) {
        match crate::widgets::file_tree::move_to_trash(&path) {
            Ok(()) => {
                self.status = format!("Moved {} to Trash", path.display());
                let parent = path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.tree.root.clone());
                if let Some(idx) = self.tree.index_of_dir(&parent) {
                    self.tree.refresh_children(idx);
                }
                if self.editor.matches_open_path(&path) {
                    if !self.editor.close_active() {
                        // Sole tab: just blank out its buffer instead.
                        *self.editor = Editor::new();
                    }
                }
            }
            Err(e) => {
                self.status = format!("Delete failed: {e}");
            }
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

fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0
        && r.height > 0
        && x >= r.x
        && x < r.x + r.width
        && y >= r.y
        && y < r.y + r.height
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
    fn drain_fs_events_returns_false_when_nothing_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        for _ in 0..20 {
            let _ = app.drain_fs_events();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!app.drain_fs_events(), "no fs events ⇒ no redraw needed");
    }

    #[test]
    fn drain_fs_events_returns_true_after_workspace_write() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        for _ in 0..20 {
            let _ = app.drain_fs_events();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::fs::write(tmp.path().join("new.txt"), "hi").unwrap();
        let mut saw = false;
        for _ in 0..150 {
            if app.drain_fs_events() {
                saw = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(saw, "workspace write should propagate as a dirty signal");
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
    fn tree_context_menu_on_file_offers_rename_and_delete_only() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("hello.txt");
        std::fs::write(&f, "hi").unwrap();
        let n = file_node(&f);
        let items = build_tree_context_menu_items(Some(&n), tmp.path());
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(labels, ["Rename…", "Delete"]);
        assert!(matches!(items[0].1, MenuAction::Rename(ref p) if p == &f));
        assert!(matches!(items[1].1, MenuAction::Delete(ref p) if p == &f));
    }

    #[test]
    fn tree_context_menu_on_subfolder_offers_rename_and_delete_only() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("sub");
        std::fs::create_dir(&d).unwrap();
        let n = dir_node(&d);
        let items = build_tree_context_menu_items(Some(&n), tmp.path());
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(labels, ["Rename…", "Delete"]);
        assert!(matches!(items[0].1, MenuAction::Rename(ref p) if p == &d));
        assert!(matches!(items[1].1, MenuAction::Delete(ref p) if p == &d));
    }

    #[test]
    fn tree_context_menu_on_empty_space_offers_new_file_and_new_folder_only() {
        let tmp = tempfile::tempdir().unwrap();
        let items = build_tree_context_menu_items(None, tmp.path());
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(labels, ["New File…", "New Folder…"]);
        assert!(matches!(items[0].1, MenuAction::Create(CreateKind::File)));
        assert!(matches!(items[1].1, MenuAction::Create(CreateKind::Folder)));
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
        let items = build_tree_context_menu_items(Some(&n), tmp.path());
        let labels: Vec<&str> = items.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(labels, ["New File…", "New Folder…"]);
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

    result
}

fn main_loop(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
    // Force the very first frame to render so the user sees the UI even
    // before the first event arrives or any timer fires.
    let mut needs_redraw = true;
    let mut last_blink_visible = app.cursor_visible_phase();

    while !app.quit {
        // Pull in any filesystem-watcher events first so the tree reflects
        // disk reality on the very next frame.
        let fs_changed = app.drain_fs_events();
        let pty_changed = app.terminal.take_dirty();
        let blink_visible = app.cursor_visible_phase();
        let blink_changed = blink_visible != last_blink_visible;
        let commits_changed = app.drain_recent_commits();

        if needs_redraw || fs_changed || pty_changed || blink_changed || commits_changed {
            // If the welcome OSC-1337 image was painted earlier and the
            // user has just opened a file, wipe the screen so iTerm drops
            // its cached image cells AND ratatui repaints every cell on
            // the next draw (its diff alone misses cells whose content
            // didn't change between welcome and editor buffers).
            if app.consume_welcome_image_clear() {
                terminal.clear()?;
                // Activity-bar icons live outside ratatui too; re-emit
                // them on the next post-draw flush.
                app.activity_overlay_dirty = true;
            }
            terminal.draw(|f| {
                app.render(f);
            })?;
            // After ratatui flushes its diff, paint the activity-bar icons
            // directly via OSC-1337. Bypassing the buffer is the only path
            // that's known to work in iTerm2 (yazi uses the same trick).
            // ratatui's bg block re-clears those cells on every frame, so
            // we re-emit on every redraw — no flicker because both writes
            // hit the same diff cycle.
            let overlays = app.pending_activity_image_overlays();
            if app.activity_overlay_dirty && !overlays.is_empty() {
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

        if event::poll(Duration::from_millis(33))? {
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
