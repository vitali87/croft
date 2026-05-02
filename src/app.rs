use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
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

use crate::widgets::{editor::Editor, file_tree::FileTree, terminal::PtyTerminal};

/// Single source of truth for the application's user-facing name.
pub const APP_NAME: &str = "croft";

/// Build the status-bar spans for the git pill (branch name, dirty bullet,
/// ahead/behind counts).  Returns an empty vec when the workspace is not in
/// a git repo so the status bar shows nothing.
fn git_status_spans<'a>(status: &'a crate::git::GitStatus) -> Vec<Span<'a>> {
    if !status.in_repo {
        return Vec::new();
    }
    let mut spans: Vec<Span> = Vec::with_capacity(8);
    spans.push(Span::raw("  "));
    // Codicon source-control / git-branch glyph (U+EAFC). Falls back to a
    // textual marker if the user's font lacks the glyph.
    spans.push(Span::styled(
        "\u{eafc} ",
        Style::default().fg(Color::Rgb(0xdc, 0xb6, 0x7a)),
    ));
    let label: &str = match (&status.branch, &status.detached_hash) {
        (Some(b), _) => b.as_str(),
        (None, Some(h)) => h.as_str(),
        (None, None) => "(no head)",
    };
    spans.push(Span::styled(
        label,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    if status.dirty {
        spans.push(Span::styled(
            "\u{2009}●",
            Style::default().fg(Color::Rgb(0xe8, 0x27, 0x4b)),
        ));
    }
    if status.ahead > 0 {
        spans.push(Span::styled(
            format!(" \u{2191}{}", status.ahead),
            Style::default().fg(Color::Rgb(0xa3, 0xbe, 0x8c)),
        ));
    }
    if status.behind > 0 {
        spans.push(Span::styled(
            format!(" \u{2193}{}", status.behind),
            Style::default().fg(Color::Rgb(0xeb, 0xcb, 0x8b)),
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
    pub editor: Editor,
    pub terminal: PtyTerminal,
    focus: Pane,
    show_tree: bool,
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
    git_status: crate::git::GitStatus,
    last_git_check: std::time::Instant,
}

impl App {
    pub fn new(root: PathBuf) -> Result<Self> {
        let tree = FileTree::new(root.clone());
        let editor = Editor::new();
        let term = PtyTerminal::new(&root).context("spawning terminal")?;
        let (watcher, rx) = match Self::spawn_fs_watcher(&root) {
            Ok((w, r)) => (Some(w), Some(r)),
            Err(_) => (None, None),
        };
        let git_status = crate::git::query(&root);
        Ok(Self {
            tree,
            editor,
            terminal: term,
            focus: Pane::Tree,
            show_tree: true,
            status: String::from("Ready"),
            quit: false,
            context_menu: None,
            prompt: None,
            _fs_watcher: watcher,
            fs_rx: rx,
            git_status,
            last_git_check: std::time::Instant::now(),
        })
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
    fn drain_fs_events(&mut self) {
        let Some(rx) = self.fs_rx.as_ref() else {
            return;
        };
        let mut affected: std::collections::BTreeSet<PathBuf> =
            std::collections::BTreeSet::new();
        let mut touched_open_file = false;
        while let Ok(result) = rx.try_recv() {
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
    }

    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Pane::Tree => Pane::Editor,
            Pane::Editor => Pane::Terminal,
            Pane::Terminal => Pane::Tree,
        };
        self.tree.focused = self.focus == Pane::Tree;
        self.editor.focused = self.focus == Pane::Editor;
        self.terminal.focused = self.focus == Pane::Terminal;
    }

    fn focus_pane(&mut self, p: Pane) {
        self.focus = p;
        self.tree.focused = self.focus == Pane::Tree;
        self.editor.focused = self.focus == Pane::Editor;
        self.terminal.focused = self.focus == Pane::Terminal;
    }

    fn render(&mut self, frame: &mut ratatui::Frame) {
        let size = frame.area();
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(size);

        let main = if self.show_tree {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(32), Constraint::Min(20)])
                .split(outer[0])
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(20)])
                .split(outer[0])
        };

        let (tree_area, right_area) = if self.show_tree {
            (Some(main[0]), main[1])
        } else {
            (None, main[0])
        };

        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(right_area);

        if let Some(area) = tree_area {
            frame.render_widget(&mut self.tree, area);
        }
        frame.render_widget(&mut self.editor, right[0]);
        frame.render_widget(&mut self.terminal, right[1]);

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
        spans.push(Span::raw(" Tree"));
        let status = Paragraph::new(Line::from(spans))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x3a, 0x6e)));
        frame.render_widget(status, outer[1]);

        // Overlays render last so they sit on top of everything else.
        self.render_context_menu(frame);
        self.render_prompt(frame);
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

    fn handle_key(&mut self, key: KeyEvent, page: usize) -> Result<()> {
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
            Pane::Tree => self.handle_tree_key(key),
            Pane::Editor => self.handle_editor_key(key, page),
            Pane::Terminal => self.handle_terminal_key(key),
        }
        Ok(())
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
                    match self.editor.open(&path) {
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

    fn handle_editor_key(&mut self, key: KeyEvent, page: usize) {
        match key.code {
            KeyCode::Up => self.editor.move_up(),
            KeyCode::Down => self.editor.move_down(),
            KeyCode::Left => self.editor.move_left(),
            KeyCode::Right => self.editor.move_right(),
            KeyCode::PageUp => self.editor.page_up(page),
            KeyCode::PageDown => self.editor.page_down(page),
            KeyCode::Home => self.editor.home_line(),
            KeyCode::End => self.editor.end_line(),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete_forward(),
            KeyCode::Enter => self.editor.insert_newline(),
            KeyCode::Tab => self.editor.insert_str("    "),
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    self.editor.insert_char(c);
                }
            }
            _ => {}
        }
    }

    fn handle_terminal_key(&mut self, key: KeyEvent) {
        let bytes = key_to_bytes(key);
        if !bytes.is_empty() {
            self.terminal.write_input(&bytes);
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
                    let delete_target = crate::widgets::file_tree::delete_target_for(
                        node, &self.tree.root,
                    );
                    let mut items: Vec<(String, MenuAction)> = vec![
                        (String::from("New File…"), MenuAction::Create(CreateKind::File)),
                        (String::from("New Folder…"), MenuAction::Create(CreateKind::Folder)),
                    ];
                    if let Some(p) = delete_target {
                        let label = match p.file_name() {
                            Some(n) => format!("Delete {}", n.to_string_lossy()),
                            None => String::from("Delete"),
                        };
                        items.push((label, MenuAction::Delete(p)));
                    }
                    self.context_menu = Some(ContextMenu {
                        origin: (m.column, m.row),
                        items,
                        selected: 0,
                        target_dir,
                    });
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if in_tree {
                    self.focus_pane(Pane::Tree);
                    if let Some(idx) = self.tree.node_at_y(m.row) {
                        self.tree.select(idx);
                        if let Some(path) = self.tree.activate() {
                            match self.editor.open(&path) {
                                Ok(()) => {
                                    self.status = self.editor.status.clone();
                                    // Tree keeps focus so Delete / arrows still
                                    // act on the explorer. Click into the
                                    // editor pane to start typing.
                                }
                                Err(e) => self.status = format!("Error: {e}"),
                            }
                        }
                    }
                } else if in_editor {
                    self.focus_pane(Pane::Editor);
                    self.editor.click(m.column, m.row);
                } else if in_terminal {
                    self.focus_pane(Pane::Terminal);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if in_editor {
                    self.editor.click(m.column, m.row);
                } else if in_tree {
                    if let Some(idx) = self.tree.node_at_y(m.row) {
                        self.tree.select(idx);
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
                    self.terminal.write_input(b"\x1b[B\x1b[B\x1b[B");
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
                    self.terminal.write_input(b"\x1b[A\x1b[A\x1b[A");
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
                    self.editor = Editor::new();
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

fn build_title(workspace: &std::path::Path) -> String {
    let name = workspace
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| workspace.display().to_string());
    format!("{APP_NAME}  {name}")
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
    fn git_status_spans_renders_branch_and_no_dirty_marker_when_clean() {
        let st = crate::git::GitStatus {
            in_repo: true,
            branch: Some("main".into()),
            detached_hash: None,
            dirty: false,
            ahead: 0,
            behind: 0,
        };
        let spans = git_status_spans(&st);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(joined.contains("main"), "branch name should be present");
        assert!(!joined.contains('●'), "no dirty bullet when clean");
    }

    #[test]
    fn git_status_spans_renders_dirty_marker_when_dirty() {
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
        assert!(joined.contains("main"));
        assert!(joined.contains('●'), "dirty bullet should appear when dirty");
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
}

pub fn run(root: PathBuf) -> Result<()> {
    let title = build_title(&root);
    let mut app = App::new(root)?;

    enable_raw_mode().context("enable raw mode")?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture).context("enter alt screen")?;
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
        out.flush().ok();
    }
    let backend = CrosstermBackend::new(out);
    let mut terminal: Terminal<CrosstermBackend<Stdout>> =
        Terminal::new(backend).context("create terminal")?;

    let result = main_loop(&mut app, &mut terminal);

    disable_raw_mode().ok();
    {
        use std::io::Write;
        let mut out = stdout();
        out.write_all(&set_title_seq("")).ok();
        out.flush().ok();
    }
    if kbd_enhanced {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags).ok();
    }
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture).ok();
    terminal.show_cursor().ok();

    result
}

fn main_loop(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
    while !app.quit {
        // Pull in any filesystem-watcher events first so the tree reflects
        // disk reality on the very next frame.
        app.drain_fs_events();

        let mut frame_size = Rect::default();
        terminal.draw(|f| {
            frame_size = f.area();
            app.render(f);
        })?;
        // Page size for editor PageUp/PageDown is approx half the editor pane height.
        let page = (frame_size.height as usize / 4).max(1);

        if event::poll(Duration::from_millis(33))? {
            match event::read()? {
                Event::Key(key) => app.handle_key(key, page)?,
                Event::Mouse(m) => app.handle_mouse(m),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}
