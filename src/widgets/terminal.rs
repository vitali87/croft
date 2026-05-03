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

/// Inclusive cell range in the terminal's *inner* coordinate system (0,0 at
/// the top-left of the content area, not the border).  Stored as the
/// (row, col) of the cell where the user pressed the mouse and the cell
/// where it currently is.  `selection_range()` normalises them so end >= start
/// in row-major order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

impl Selection {
    pub fn new(row: u16, col: u16) -> Self {
        Self { anchor: (row, col), head: (row, col) }
    }
    /// `(start_row, start_col, end_row, end_col)` with start <= end in
    /// row-major order.
    pub fn normalised(&self) -> (u16, u16, u16, u16) {
        let (a_r, a_c) = self.anchor;
        let (b_r, b_c) = self.head;
        let after = (a_r, a_c) <= (b_r, b_c);
        if after {
            (a_r, a_c, b_r, b_c)
        } else {
            (b_r, b_c, a_r, a_c)
        }
    }
    /// True if the selected region covers more than a single cell.
    pub fn has_area(&self) -> bool {
        self.anchor != self.head
    }
}

pub struct PtyTerminal {
    parser: Arc<Mutex<vt100::Parser>>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    cols: u16,
    rows: u16,
    pub focused: bool,
    pub last_area: Rect,
    pub last_inner: Rect,
    selection: Option<Selection>,
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
        // 5000 rows of scrollback ≈ 1 MB at typical line widths; plenty for
        // mouse-wheel history navigation, cheap in memory.
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 5000)));
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
            last_area: Rect::default(),
            last_inner: Rect::default(),
            selection: None,
        })
    }

    /// Translate an absolute terminal mouse coordinate to an inner cell, or
    /// `None` if outside the terminal pane's content area.
    pub fn cell_at(&self, col: u16, row: u16) -> Option<(u16, u16)> {
        let inner = self.last_inner;
        if inner.width == 0 || inner.height == 0 {
            return None;
        }
        if col < inner.x || col >= inner.x + inner.width {
            return None;
        }
        if row < inner.y || row >= inner.y + inner.height {
            return None;
        }
        Some((row - inner.y, col - inner.x))
    }

    pub fn start_selection_at(&mut self, col: u16, row: u16) {
        if let Some((r, c)) = self.cell_at(col, row) {
            self.selection = Some(Selection::new(r, c));
        }
    }

    pub fn extend_selection_to(&mut self, col: u16, row: u16) {
        let cell = self.cell_at(col, row);
        if let (Some(sel), Some((r, c))) = (self.selection.as_mut(), cell) {
            sel.head = (r, c);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    /// Extract the text covered by the current selection by walking the
    /// vt100 screen cells in row-major order.  Returns an empty string if
    /// nothing is selected.  Trailing spaces on each row are trimmed (the
    /// usual "row of cells" → "displayed text" convention).
    pub fn selection_text(&self) -> String {
        let Some(sel) = self.selection else { return String::new() };
        let (sr, sc, er, ec) = sel.normalised();
        let parser = match self.parser.lock() {
            Ok(p) => p,
            Err(_) => return String::new(),
        };
        let screen = parser.screen();
        extract_selection_text(screen, sr, sc, er, ec)
    }

    pub fn write_input(&mut self, data: &[u8]) {
        // Snap back to the live bottom on user input so the response is visible.
        self.reset_scrollback();
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    /// Scroll the visible window up by `n` rows into history. Returns false
    /// if we're in alternate-screen mode (vim/less/htop manage their own
    /// scrollback), so the caller can fall back to sending arrow keys.
    pub fn scroll_up(&mut self, n: usize) -> bool {
        let mut parser = match self.parser.lock() {
            Ok(p) => p,
            Err(_) => return false,
        };
        scroll_screen_up(parser.screen_mut(), n)
    }

    pub fn scroll_down(&mut self, n: usize) -> bool {
        let mut parser = match self.parser.lock() {
            Ok(p) => p,
            Err(_) => return false,
        };
        scroll_screen_down(parser.screen_mut(), n)
    }

    pub fn reset_scrollback(&mut self) {
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_scrollback(0);
        }
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
            p.screen_mut().set_size(rows, cols);
        }
    }
}

use std::io::Read;

/// Walk the vt100 screen from (sr, sc) to (er, ec) inclusive in row-major
/// order, joining cell contents row by row, trimming trailing spaces, and
/// inserting `\n` between rows.  Public so it can be unit-tested.
pub fn extract_selection_text(
    screen: &vt100::Screen,
    sr: u16,
    sc: u16,
    er: u16,
    ec: u16,
) -> String {
    let cols = screen.size().1;
    let mut out = String::new();
    for row in sr..=er {
        let row_start = if row == sr { sc } else { 0 };
        let row_end = if row == er { ec } else { cols.saturating_sub(1) };
        let mut line = String::new();
        for col in row_start..=row_end {
            if let Some(cell) = screen.cell(row, col) {
                let s = cell.contents();
                if s.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(s);
                }
            } else {
                line.push(' ');
            }
        }
        let trimmed = line.trim_end();
        out.push_str(trimmed);
        if row != er {
            out.push('\n');
        }
    }
    out
}

/// Increase the screen's scrollback offset by `n` rows (showing older
/// content). Refuses in alternate-screen mode so apps like vim / htop / less
/// keep their own scrolling semantics.
pub fn scroll_screen_up(screen: &mut vt100::Screen, n: usize) -> bool {
    if screen.alternate_screen() {
        return false;
    }
    let cur = screen.scrollback();
    screen.set_scrollback(cur + n);
    true
}

/// Decrease the scrollback offset (toward live bottom). Floors at zero.
/// Refuses in alternate-screen mode.
pub fn scroll_screen_down(screen: &mut vt100::Screen, n: usize) -> bool {
    if screen.alternate_screen() {
        return false;
    }
    let cur = screen.scrollback();
    screen.set_scrollback(cur.saturating_sub(n));
    true
}

/// Build the OSC 52 escape sequence that asks the host terminal to put `text`
/// on the system clipboard.  Format: `ESC ] 52 ; c ; <base64> BEL`. Supported
/// by iTerm2, Ghostty, kitty, WezTerm, Alacritty, and tmux (when configured).
pub fn osc52_copy_seq(text: &str) -> Vec<u8> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut out = Vec::with_capacity(encoded.len() + 8);
    out.extend_from_slice(b"\x1b]52;c;");
    out.extend_from_slice(encoded.as_bytes());
    out.push(0x07);
    out
}

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
        self.last_area = area;
        self.last_inner = inner;
        let sel_norm = self.selection.map(|s| s.normalised());

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

        for y in 0..rows {
            for x in 0..cols {
                let cell_opt = screen.cell(y, x);
                let (display, fg, bg, bold, italic, underline, inverse) = match cell_opt {
                    Some(cell) => {
                        let s = cell.contents();
                        let owned = if s.is_empty() {
                            String::from(" ")
                        } else {
                            s.to_string()
                        };
                        (
                            owned,
                            cell.fgcolor(),
                            cell.bgcolor(),
                            cell.bold(),
                            cell.italic(),
                            cell.underline(),
                            cell.inverse(),
                        )
                    }
                    None => (
                        String::from(" "),
                        vt100::Color::Default,
                        vt100::Color::Default,
                        false,
                        false,
                        false,
                        false,
                    ),
                };
                let mut style = Style::default();
                if let Some(c) = vt_color(fg) {
                    style = style.fg(c);
                }
                if let Some(c) = vt_color(bg) {
                    style = style.bg(c);
                }
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if underline {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if inverse {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                if cursor_visible && y == cur_row && x == cur_col {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                if let Some((sr, sc, er, ec)) = sel_norm {
                    if cell_in_selection(y, x, sr, sc, er, ec) {
                        style = style.bg(Color::Rgb(0x26, 0x4f, 0x78));
                    }
                }
                let target_x = inner.x + x;
                let target_y = inner.y + y;
                let target = &mut buf[(target_x, target_y)];
                target.set_symbol(&display);
                target.set_style(style);
            }
        }
    }
}

/// True iff (row, col) is inside the inclusive row-major range [(sr,sc)..=(er,ec)].
pub fn cell_in_selection(row: u16, col: u16, sr: u16, sc: u16, er: u16, ec: u16) -> bool {
    if row < sr || row > er {
        return false;
    }
    if row == sr && col < sc {
        return false;
    }
    if row == er && col > ec {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_normalised_handles_anchor_after_head() {
        let s = Selection { anchor: (5, 4), head: (2, 1) };
        assert_eq!(s.normalised(), (2, 1, 5, 4));
    }

    #[test]
    fn selection_normalised_handles_same_row() {
        let s = Selection { anchor: (3, 9), head: (3, 2) };
        assert_eq!(s.normalised(), (3, 2, 3, 9));
    }

    #[test]
    fn selection_has_area_only_when_endpoints_differ() {
        let s = Selection::new(2, 5);
        assert!(!s.has_area());
        let s2 = Selection { anchor: (2, 5), head: (2, 6) };
        assert!(s2.has_area());
    }

    #[test]
    fn cell_in_selection_within_single_row() {
        // selection: row 2, cols 3..=7
        assert!(cell_in_selection(2, 5, 2, 3, 2, 7));
        assert!(!cell_in_selection(2, 2, 2, 3, 2, 7));
        assert!(!cell_in_selection(2, 8, 2, 3, 2, 7));
        assert!(!cell_in_selection(1, 5, 2, 3, 2, 7));
        assert!(!cell_in_selection(3, 5, 2, 3, 2, 7));
    }

    #[test]
    fn cell_in_selection_spans_multiple_rows() {
        // selection: (1, 5) to (3, 2)
        assert!(!cell_in_selection(1, 4, 1, 5, 3, 2));
        assert!(cell_in_selection(1, 5, 1, 5, 3, 2));
        assert!(cell_in_selection(1, 9, 1, 5, 3, 2));
        assert!(cell_in_selection(2, 0, 1, 5, 3, 2));
        assert!(cell_in_selection(2, 9, 1, 5, 3, 2));
        assert!(cell_in_selection(3, 2, 1, 5, 3, 2));
        assert!(!cell_in_selection(3, 3, 1, 5, 3, 2));
        assert!(!cell_in_selection(4, 0, 1, 5, 3, 2));
    }

    #[test]
    fn osc52_copy_seq_wraps_with_correct_envelope() {
        let bytes = osc52_copy_seq("hello");
        // ESC ] 5 2 ; c ; aGVsbG8= BEL
        assert_eq!(bytes, b"\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn osc52_copy_seq_handles_empty() {
        assert_eq!(osc52_copy_seq(""), b"\x1b]52;c;\x07");
    }

    #[test]
    fn osc52_copy_seq_handles_unicode() {
        let bytes = osc52_copy_seq("héllo");
        // Just check the envelope and that the body decodes back.
        assert!(bytes.starts_with(b"\x1b]52;c;"));
        assert_eq!(*bytes.last().unwrap(), 0x07);
        let body = &bytes[7..bytes.len() - 1];
        use base64::Engine;
        let decoded =
            base64::engine::general_purpose::STANDARD.decode(body).unwrap();
        assert_eq!(decoded, "héllo".as_bytes());
    }

    #[test]
    fn scroll_screen_up_increments_scrollback_in_normal_mode() {
        let mut p = vt100::Parser::new(5, 20, 1000);
        // Push enough lines that scrolling back makes sense.
        for i in 0..50 {
            p.process(format!("line {i}\r\n").as_bytes());
        }
        let before = p.screen().scrollback();
        let scrolled = scroll_screen_up(p.screen_mut(), 3);
        assert!(scrolled);
        assert_eq!(p.screen().scrollback(), before + 3);
    }

    #[test]
    fn scroll_screen_down_decrements_with_floor_at_zero() {
        let mut p = vt100::Parser::new(5, 20, 1000);
        for i in 0..50 {
            p.process(format!("line {i}\r\n").as_bytes());
        }
        scroll_screen_up(p.screen_mut(), 5);
        scroll_screen_down(p.screen_mut(), 2);
        assert_eq!(p.screen().scrollback(), 3);
        // Floor at zero, no underflow.
        scroll_screen_down(p.screen_mut(), 100);
        assert_eq!(p.screen().scrollback(), 0);
    }

    #[test]
    fn scroll_screen_up_refuses_in_alternate_screen() {
        let mut p = vt100::Parser::new(5, 20, 1000);
        for i in 0..50 {
            p.process(format!("line {i}\r\n").as_bytes());
        }
        // Enter the alternate screen (DECSET 1049). Apps like vim / htop /
        // less use this; their internal scrollback handling should win.
        p.process(b"\x1b[?1049h");
        let before = p.screen().scrollback();
        let scrolled = scroll_screen_up(p.screen_mut(), 3);
        assert!(!scrolled, "should refuse scrollback in alternate-screen mode");
        assert_eq!(p.screen().scrollback(), before);
    }

    #[test]
    fn scroll_screen_down_also_refuses_in_alternate_screen() {
        let mut p = vt100::Parser::new(5, 20, 1000);
        p.process(b"\x1b[?1049h");
        let scrolled = scroll_screen_down(p.screen_mut(), 3);
        assert!(!scrolled);
    }

    #[test]
    fn extract_selection_text_single_line() {
        let mut p = vt100::Parser::new(5, 20, 0);
        p.process(b"hello world");
        let txt = extract_selection_text(p.screen(), 0, 6, 0, 10);
        assert_eq!(txt, "world");
    }

    #[test]
    fn extract_selection_text_multi_line_trims_trailing_spaces() {
        let mut p = vt100::Parser::new(5, 20, 0);
        p.process(b"first line\r\nsecond line");
        // From row 0 col 6 to row 1 col 5 (inclusive). vt100 reports cells
        // unreachable by content as spaces; trailing whitespace per row is
        // trimmed by extract_selection_text.
        let txt = extract_selection_text(p.screen(), 0, 6, 1, 5);
        assert_eq!(txt, "line\nsecond");
    }
}
