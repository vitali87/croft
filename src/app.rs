use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
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
use std::path::PathBuf;
use std::time::Duration;

use crate::widgets::{editor::Editor, file_tree::FileTree, terminal::PtyTerminal};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Tree,
    Editor,
    Terminal,
}

pub struct App {
    pub tree: FileTree,
    pub editor: Editor,
    pub terminal: PtyTerminal,
    focus: Pane,
    show_tree: bool,
    status: String,
    quit: bool,
}

impl App {
    pub fn new(root: PathBuf) -> Result<Self> {
        let tree = FileTree::new(root.clone());
        let editor = Editor::new();
        let term = PtyTerminal::new(&root).context("spawning terminal")?;
        Ok(Self {
            tree,
            editor,
            terminal: term,
            focus: Pane::Tree,
            show_tree: true,
            status: String::from("Ready"),
            quit: false,
        })
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

        let status = Paragraph::new(Line::from(vec![
            Span::styled(
                " tcode ",
                Style::default()
                    .bg(Color::Rgb(0x4e, 0x9a, 0xff))
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::raw(&self.status),
            Span::raw("  "),
            Span::styled("^q", Style::default().fg(Color::Yellow)),
            Span::raw(" Quit  "),
            Span::styled("^s", Style::default().fg(Color::Yellow)),
            Span::raw(" Save  "),
            Span::styled("F6", Style::default().fg(Color::Yellow)),
            Span::raw(" Cycle pane  "),
            Span::styled("^b", Style::default().fg(Color::Yellow)),
            Span::raw(" Tree"),
        ]))
        .style(Style::default().bg(Color::Rgb(0x1e, 0x3a, 0x6e)));
        frame.render_widget(status, outer[1]);
    }

    fn handle_key(&mut self, key: KeyEvent, page: usize) -> Result<()> {
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return Ok(());
        }
        // App-wide shortcuts (priority).
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
                self.quit = true;
                return Ok(());
            }
            (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
                self.save();
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
                            self.focus_pane(Pane::Editor);
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
        let in_tree = self.show_tree && rect_contains(self.tree.last_area, m.column, m.row);
        let in_editor = rect_contains(self.editor.last_area, m.column, m.row);
        let in_terminal = rect_contains(self.terminal.last_area, m.column, m.row);

        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if in_tree {
                    self.focus_pane(Pane::Tree);
                    if let Some(idx) = self.tree.node_at_y(m.row) {
                        self.tree.select(idx);
                        if let Some(path) = self.tree.activate() {
                            match self.editor.open(&path) {
                                Ok(()) => {
                                    self.status = self.editor.status.clone();
                                    self.focus_pane(Pane::Editor);
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
}

pub fn run(root: PathBuf) -> Result<()> {
    let mut app = App::new(root)?;

    enable_raw_mode().context("enable raw mode")?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture).context("enter alt screen")?;
    let backend = CrosstermBackend::new(out);
    let mut terminal: Terminal<CrosstermBackend<Stdout>> =
        Terminal::new(backend).context("create terminal")?;

    let result = main_loop(&mut app, &mut terminal);

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture).ok();
    terminal.show_cursor().ok();

    result
}

fn main_loop(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
    while !app.quit {
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
