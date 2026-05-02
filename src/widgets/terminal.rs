use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Widget},
};
use std::io::Write;
use std::sync::{Arc, Mutex};

pub struct PtyTerminal {
    parser: Arc<Mutex<vt100::Parser>>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    cols: u16,
    rows: u16,
    pub focused: bool,
}

impl PtyTerminal {
    pub fn new(cwd: &std::path::Path) -> Result<Self> {
        let pty_system = native_pty_system();
        let cols = 80u16;
        let rows = 24u16;
        let pair = pty_system
            .openpty(PtySize { cols, rows, pixel_width: 0, pixel_height: 0 })
            .context("openpty")?;

        let shell =
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).context("spawn shell")?;
        drop(pair.slave);

        let writer = pair.master.take_writer().context("take writer")?;
        let mut reader = pair.master.try_clone_reader().context("clone reader")?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let parser_for_thread = parser.clone();

        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut p) = parser_for_thread.lock() {
                            p.process(&buf[..n]);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            parser,
            master: pair.master,
            writer,
            _child: child,
            cols,
            rows,
            focused: false,
        })
    }

    pub fn write_input(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        let _ = self.master.resize(PtySize {
            cols,
            rows,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
    }
}

use std::io::Read;

fn vt_color(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

impl Widget for &mut PtyTerminal {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style)
            .title(Span::styled(
                " TERMINAL ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);

        let cols = inner.width;
        let rows = inner.height;
        self.resize(cols, rows);

        let parser = match self.parser.lock() {
            Ok(p) => p,
            Err(_) => return,
        };
        let screen = parser.screen();
        let (cur_row, cur_col) = screen.cursor_position();
        let cursor_visible = !screen.hide_cursor() && self.focused;

        let default_cell = vt100::Cell::default();
        for y in 0..rows {
            for x in 0..cols {
                let cell = screen.cell(y, x).unwrap_or(&default_cell);
                let ch = cell.contents();
                let display: &str = if ch.is_empty() { " " } else { &ch };
                let mut style = Style::default();
                if let Some(fg) = vt_color(cell.fgcolor()) {
                    style = style.fg(fg);
                }
                if let Some(bg) = vt_color(cell.bgcolor()) {
                    style = style.bg(bg);
                }
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                if cursor_visible && y == cur_row && x == cur_col {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                let target_x = inner.x + x;
                let target_y = inner.y + y;
                let target = &mut buf[(target_x, target_y)];
                target.set_symbol(display);
                target.set_style(style);
            }
        }
    }
}
