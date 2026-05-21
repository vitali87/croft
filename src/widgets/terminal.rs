use alacritty_terminal::event::{Event as AlacEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Processor, StdSyncHandler};
use alacritty_terminal::Term;
use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Widget},
};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const SCROLLBACK_LINES: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

impl Selection {
    pub fn new(row: u16, col: u16) -> Self {
        Self { anchor: (row, col), head: (row, col) }
    }
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
    pub fn has_area(&self) -> bool {
        self.anchor != self.head
    }
}

/// Listener that swallows events from the embedded `Term`. We don't act
/// on title changes / cursor blink / clipboard requests etc. — the outer
/// croft TUI owns all of those.
#[derive(Clone, Default)]
pub struct VoidListener;
impl EventListener for VoidListener {
    fn send_event(&self, _event: AlacEvent) {}
}

pub struct PtyTerminal {
    term: Arc<FairMutex<Term<VoidListener>>>,
    /// Set by the PTY reader thread on every chunk and by `write_input`;
    /// cleared by `take_dirty`. The main loop only redraws when set.
    pty_dirty: Arc<AtomicBool>,
    /// Tracks whether the inner program has enabled DECSET 2004 (bracketed
    /// paste). Sniffed off the byte stream; not all parsers expose it.
    bracketed_paste_enabled: Arc<AtomicBool>,
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

/// Pick the program + args to spawn the user's interactive shell so it
/// behaves like the one iTerm2 / Terminal.app launches. Both run the
/// shell as a *login* shell (`Login shell` is iTerm2's default Command
/// setting), which sources `~/.zprofile` / `~/.profile` in addition to
/// the interactive rc file. Without that, anything the user puts in
/// `.zprofile` — PATH munging, framework loaders, plugin managers like
/// zinit/oh-my-zsh that install keybindings, vi-mode toggles — is
/// silently absent inside croft. For mainstream POSIX shells we pass
/// `-l`; for anything exotic we leave argv empty rather than risk
/// breaking the spawn with an unknown flag.
pub fn interactive_shell_invocation(shell_path: &str) -> (String, Vec<String>) {
    let basename = std::path::Path::new(shell_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(shell_path);
    let supports_login_flag = matches!(
        basename,
        "zsh" | "bash" | "fish" | "ksh" | "mksh" | "dash" | "tcsh" | "sh"
    );
    let args = if supports_login_flag {
        vec!["-l".to_string()]
    } else {
        Vec::new()
    };
    (shell_path.to_string(), args)
}

impl PtyTerminal {
    pub fn new(cwd: &std::path::Path) -> Result<Self> {
        let shell =
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let (program, args) = interactive_shell_invocation(&shell);
        let mut cmd = CommandBuilder::new(&program);
        for a in &args {
            cmd.arg(a);
        }
        cmd.cwd(cwd);
        Self::spawn_with(cmd, None)
    }

    pub fn new_running(
        program: &str,
        args: &[String],
        cwd: &std::path::Path,
    ) -> Result<Self> {
        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        cmd.cwd(cwd);
        let label = if args.is_empty() {
            program.to_string()
        } else {
            format!("{program} {}", args.join(" "))
        };
        Self::spawn_with(cmd, Some(label))
    }

    pub fn visible_text(&self) -> String {
        let term = self.term.lock();
        let rows = term.screen_lines();
        let cols = term.columns();
        if rows == 0 || cols == 0 {
            return String::new();
        }
        let display_offset = term.grid().display_offset();
        extract_selection_text(&term, display_offset, 0, 0, rows - 1, cols - 1)
    }

    fn spawn_with(mut cmd: CommandBuilder, run_label: Option<String>) -> Result<Self> {
        let pty_system = native_pty_system();
        let cols = 80u16;
        let rows = 24u16;
        let pair = pty_system
            .openpty(PtySize { cols, rows, pixel_width: 0, pixel_height: 0 })
            .context("openpty")?;

        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).context("spawn child")?;
        drop(pair.slave);

        let writer = pair.master.take_writer().context("take writer")?;
        let mut reader = pair.master.try_clone_reader().context("clone reader")?;

        let term_size = TermSize::new(cols as usize, rows as usize);
        let cfg = Config { scrolling_history: SCROLLBACK_LINES, ..Config::default() };
        let term = Term::new(cfg, &term_size, VoidListener);
        let term = Arc::new(FairMutex::new(term));
        let term_for_thread = term.clone();

        let pty_dirty = Arc::new(AtomicBool::new(true));
        let pty_dirty_for_thread = pty_dirty.clone();
        let bracketed_paste_enabled = Arc::new(AtomicBool::new(false));
        let bracketed_paste_for_thread = bracketed_paste_enabled.clone();

        if let Some(label) = run_label.as_deref() {
            let header = format!("\x1b[2m▶ {label}\x1b[22m\r\n");
            let mut p = Processor::<StdSyncHandler>::new();
            let mut t = term.lock();
            p.advance(&mut *t, header.as_bytes());
        }

        let script_mode = run_label.is_some();

        std::thread::spawn(move || {
            let mut processor = Processor::<StdSyncHandler>::new();
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        sniff_bracketed_paste_mode(
                            &buf[..n],
                            &bracketed_paste_for_thread,
                        );
                        let mut t = term_for_thread.lock();
                        processor.advance(&mut *t, &buf[..n]);
                        drop(t);
                        pty_dirty_for_thread.store(true, Ordering::Release);
                    }
                    Err(_) => break,
                }
            }
            if script_mode {
                let footer = b"\r\n\x1b[2m[Process exited]\x1b[22m\r\n";
                let mut t = term_for_thread.lock();
                processor.advance(&mut *t, footer);
                drop(t);
                pty_dirty_for_thread.store(true, Ordering::Release);
            }
        });

        Ok(Self {
            term,
            pty_dirty,
            bracketed_paste_enabled,
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

    pub fn take_dirty(&self) -> bool {
        self.pty_dirty.swap(false, Ordering::AcqRel)
    }

    /// Process ID of the shell running inside this terminal, when the
    /// platform exposes one. Used to look up the live cwd so a new split
    /// inherits the directory the user has `cd`'d into.
    pub fn pid(&self) -> Option<u32> {
        self._child.process_id()
    }

    /// Read the dirty flag without clearing it. Lets the main loop decide
    /// whether to redraw now or coalesce, without losing the signal if we
    /// choose to skip this iteration.
    pub fn peek_dirty(&self) -> bool {
        self.pty_dirty.load(Ordering::Acquire)
    }

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

    /// Double-click word-select: expand the selection to cover the word
    /// under the screen coordinate `(col, row)`. No-op when the click
    /// lands on whitespace / punctuation or outside the live area.
    pub fn select_word_at(&mut self, col: u16, row: u16) {
        let Some((r, c)) = self.cell_at(col, row) else { return };
        let term = self.term.lock();
        let display_offset = term.grid().display_offset();
        let Some((anchor, head)) =
            select_word_at_in_term(&term, display_offset, r as usize, c as usize)
        else {
            return;
        };
        drop(term);
        self.selection = Some(Selection { anchor, head });
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub fn selection_text(&self) -> String {
        let Some(sel) = self.selection else { return String::new() };
        let (sr, sc, er, ec) = sel.normalised();
        let term = self.term.lock();
        let display_offset = term.grid().display_offset();
        extract_selection_text(
            &term,
            display_offset,
            sr as usize,
            sc as usize,
            er as usize,
            ec as usize,
        )
    }

    pub fn write_input(&mut self, data: &[u8]) {
        self.reset_scrollback();
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Seed a `cd <path>` into the embedded shell so the terminal
    /// follows an Explorer-side workspace-root change. Sent verbatim to
    /// the PTY input - the shell parses it like a typed command. See
    /// `format_cd_command` for the line-clearing prefix and the path
    /// quoting strategy.
    pub fn change_cwd(&mut self, path: &std::path::Path) {
        self.write_input(&format_cd_command(path));
    }

    /// Paste a payload into the embedded shell. Bracketed-paste markers
    /// are added only if the inner program asked for them; otherwise the
    /// payload is sent raw so simple shells don't see literal `\e[200~`.
    pub fn paste_input(&mut self, payload: &[u8]) {
        if self.bracketed_paste_enabled.load(Ordering::Acquire) {
            self.write_input(b"\x1b[200~");
            self.write_input(payload);
            self.write_input(b"\x1b[201~");
        } else {
            self.write_input(payload);
        }
    }

    /// Scroll the visible viewport up by `n` rows into scrollback.
    /// Returns false if the inner program is in alternate-screen mode
    /// (apps like vim/htop/less manage their own scrolling).
    pub fn scroll_up(&mut self, n: usize) -> bool {
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        term.scroll_display(Scroll::Delta(n as i32));
        self.pty_dirty.store(true, Ordering::Release);
        true
    }

    pub fn scroll_down(&mut self, n: usize) -> bool {
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        term.scroll_display(Scroll::Delta(-(n as i32)));
        self.pty_dirty.store(true, Ordering::Release);
        true
    }

    pub fn reset_scrollback(&mut self) {
        let mut term = self.term.lock();
        term.scroll_display(Scroll::Bottom);
        self.pty_dirty.store(true, Ordering::Release);
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
        let mut term = self.term.lock();
        let size = TermSize::new(cols as usize, rows as usize);
        term.resize(size);
        self.pty_dirty.store(true, Ordering::Release);
    }
}

/// Walk the visible grid from (sr, sc) to (er, ec) inclusive, joining
/// cell contents row-by-row, trimming trailing whitespace per row, and
/// inserting `\n` between rows. Coordinates are viewport-relative;
/// `display_offset` is the alacritty grid's current scrollback offset
/// (number of rows the viewport has been scrolled back from the live
/// bottom). The grid line we read is `row - display_offset`, which is
/// negative when the user has scrolled into history — alacritty's
/// `Grid::index<Point>` accepts those negative `Line` values and
/// returns the scrollback cell, so the extracted text matches exactly
/// what the user sees highlighted on screen. Without the subtraction
/// the function silently re-reads the live grid at the viewport row,
/// which is a different cell entirely once `display_offset > 0` and
/// the user gets the wrong line on the clipboard.
pub fn extract_selection_text(
    term: &Term<VoidListener>,
    display_offset: usize,
    sr: usize,
    sc: usize,
    er: usize,
    ec: usize,
) -> String {
    let cols = term.columns();
    let rows = term.screen_lines();
    let mut out = String::new();
    for row in sr..=er.min(rows.saturating_sub(1)) {
        let row_start = if row == sr { sc } else { 0 };
        let row_end = if row == er { ec.min(cols.saturating_sub(1)) } else { cols.saturating_sub(1) };
        let line_idx = row as i32 - display_offset as i32;
        let mut line = String::new();
        for col in row_start..=row_end {
            let p = Point::new(Line(line_idx), Column(col));
            let cell = &term.grid()[p];
            let c = cell.c;
            if c == '\0' {
                line.push(' ');
            } else {
                line.push(c);
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

/// Inspect a row of `term` around `(row, col)` (both viewport-relative)
/// and return the anchor/head pair that brackets the contiguous run of
/// word characters covering the pivot. `display_offset` is the
/// alacritty grid's current scroll-back offset so the lookup
/// `Line(row - display_offset)` resolves to the scrollback cell when
/// the user has scrolled into history. Without this, a double-click
/// on text the user sees on screen but lives in scrollback would read
/// the live grid at the same viewport y, which is usually blank — so
/// `is_terminal_word_char` returns false and the function returns
/// `None`, giving the user-visible symptom "double-click doesn't
/// auto-select."
///
/// Returns `None` when the pivot sits on a non-word character
/// (whitespace, punctuation), so a double click between words is a
/// no-op rather than a spurious selection. Word semantics match
/// `widgets::editor::is_word_char`: alphanumeric + underscore. Pure
/// function so the test suite can exercise it without spawning a PTY.
pub fn select_word_at_in_term(
    term: &Term<VoidListener>,
    display_offset: usize,
    row: usize,
    col: usize,
) -> Option<((u16, u16), (u16, u16))> {
    let cols = term.columns();
    if col >= cols {
        return None;
    }
    let row_idx = row as i32 - display_offset as i32;
    let cell_char = |c: usize| -> char {
        let p = Point::new(Line(row_idx), Column(c));
        let ch = term.grid()[p].c;
        if ch == '\0' { ' ' } else { ch }
    };
    if !is_terminal_word_char(cell_char(col)) {
        return None;
    }
    let mut start = col;
    while start > 0 && is_terminal_word_char(cell_char(start - 1)) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < cols && is_terminal_word_char(cell_char(end + 1)) {
        end += 1;
    }
    Some(((row as u16, start as u16), (row as u16, end as u16)))
}

fn is_terminal_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Build the byte sequence that retargets a POSIX shell's working
/// directory to `path`. Prefixed with `\x05\x15` so cursor moves to end
/// of line and the existing line is killed backward before the `cd` is
/// typed - that way a half-typed command at the prompt does not
/// concatenate with our `cd`. Path is single-quoted with embedded
/// quotes escaped via the standard `'\''` sleight-of-hand so paths
/// containing spaces or apostrophes round-trip safely.
pub fn format_cd_command(path: &std::path::Path) -> Vec<u8> {
    let s = path.to_string_lossy();
    let mut out = Vec::with_capacity(s.len() + 8);
    out.extend_from_slice(b"\x05\x15cd '");
    for c in s.chars() {
        if c == '\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out.extend_from_slice(b"'\n");
    out
}

/// Walk a chunk of PTY output and toggle the bracketed-paste flag when
/// we see `\e[?2004h` (set) / `\e[?2004l` (reset).
pub fn sniff_bracketed_paste_mode(chunk: &[u8], flag: &AtomicBool) {
    let needle_set: &[u8] = b"\x1b[?2004h";
    let needle_reset: &[u8] = b"\x1b[?2004l";
    let mut i = 0;
    while i < chunk.len() {
        if chunk[i..].starts_with(needle_set) {
            flag.store(true, Ordering::Release);
            i += needle_set.len();
        } else if chunk[i..].starts_with(needle_reset) {
            flag.store(false, Ordering::Release);
            i += needle_reset.len();
        } else {
            i += 1;
        }
    }
}

/// Build the OSC 52 escape sequence that asks the host terminal to put
/// `text` on the system clipboard.
pub fn osc52_copy_seq(text: &str) -> Vec<u8> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut out = Vec::with_capacity(encoded.len() + 8);
    out.extend_from_slice(b"\x1b]52;c;");
    out.extend_from_slice(encoded.as_bytes());
    out.push(0x07);
    out
}

fn ansi_to_ratatui(c: AnsiColor) -> Option<Color> {
    match c {
        AnsiColor::Spec(rgb) => Some(Color::Rgb(rgb.r, rgb.g, rgb.b)),
        AnsiColor::Indexed(i) => Some(Color::Indexed(i)),
        AnsiColor::Named(named) => named_to_ratatui(named),
    }
}

fn named_to_ratatui(n: NamedColor) -> Option<Color> {
    use NamedColor::*;
    match n {
        Foreground | Background | Cursor | DimForeground => None,
        Black => Some(Color::Black),
        Red => Some(Color::Red),
        Green => Some(Color::Green),
        Yellow => Some(Color::Yellow),
        Blue => Some(Color::Blue),
        Magenta => Some(Color::Magenta),
        Cyan => Some(Color::Cyan),
        White => Some(Color::Gray),
        BrightBlack => Some(Color::DarkGray),
        BrightRed => Some(Color::LightRed),
        BrightGreen => Some(Color::LightGreen),
        BrightYellow => Some(Color::LightYellow),
        BrightBlue => Some(Color::LightBlue),
        BrightMagenta => Some(Color::LightMagenta),
        BrightCyan => Some(Color::LightCyan),
        BrightWhite => Some(Color::White),
        DimBlack => Some(Color::Black),
        DimRed => Some(Color::Red),
        DimGreen => Some(Color::Green),
        DimYellow => Some(Color::Yellow),
        DimBlue => Some(Color::Blue),
        DimMagenta => Some(Color::Magenta),
        DimCyan => Some(Color::Cyan),
        DimWhite => Some(Color::Gray),
        BrightForeground => None,
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

        let term = self.term.lock();
        let display_offset = term.grid().display_offset();
        let cursor_visible = term.mode().contains(TermMode::SHOW_CURSOR) && self.focused;
        let alt_screen = term.mode().contains(TermMode::ALT_SCREEN);
        let cursor_point = term.grid().cursor.point;
        // The cursor is in absolute grid coords (Line is 0..rows in alt
        // screen, can wander into negative scrollback in normal screen).
        // Convert to viewport row by adding display_offset (>0 when the
        // user has scrolled up into history).
        let cursor_row_in_viewport = if alt_screen {
            cursor_point.line.0
        } else {
            cursor_point.line.0 + display_offset as i32
        };
        let cursor_col_in_viewport = cursor_point.column.0 as i32;

        for y in 0..rows {
            for x in 0..cols {
                let line_idx = (y as i32) - (display_offset as i32);
                let p = Point::new(Line(line_idx), Column(x as usize));
                let cell = &term.grid()[p];
                let display_char = if cell.c == '\0' { ' ' } else { cell.c };
                let mut style = Style::default();
                if let Some(c) = ansi_to_ratatui(cell.fg) {
                    style = style.fg(c);
                }
                if let Some(c) = ansi_to_ratatui(cell.bg) {
                    style = style.bg(c);
                }
                let flags = cell.flags;
                if flags.contains(Flags::BOLD) {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if flags.contains(Flags::ITALIC) {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if flags.intersects(Flags::ALL_UNDERLINES) {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if flags.contains(Flags::INVERSE) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                if cursor_visible
                    && (y as i32) == cursor_row_in_viewport
                    && (x as i32) == cursor_col_in_viewport
                {
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
                let mut tmp = [0u8; 4];
                target.set_symbol(display_char.encode_utf8(&mut tmp));
                target.set_style(style);
            }
        }
    }
}

/// True iff (row, col) is inside the inclusive row-major range
/// [(sr,sc)..=(er,ec)]. Public for unit testing.
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

    fn fresh_term(cols: usize, rows: usize) -> Term<VoidListener> {
        let cfg = Config { scrolling_history: 1000, ..Config::default() };
        let size = TermSize::new(cols, rows);
        Term::new(cfg, &size, VoidListener)
    }

    fn feed(term: &mut Term<VoidListener>, bytes: &[u8]) {
        let mut p = Processor::<StdSyncHandler>::new();
        p.advance(term, bytes);
    }

    #[test]
    fn interactive_shell_invocation_passes_login_flag_for_zsh() {
        let (program, args) = interactive_shell_invocation("/bin/zsh");
        assert_eq!(program, "/bin/zsh");
        assert_eq!(
            args,
            vec!["-l".to_string()],
            "zsh must be spawned as a login shell so ~/.zprofile is sourced and the embedded terminal inherits the same env / keybindings as the user's native iTerm2 shell"
        );
    }

    #[test]
    fn interactive_shell_invocation_passes_login_flag_for_bash_fish_ksh() {
        for path in ["/bin/bash", "/usr/local/bin/fish", "/bin/ksh", "/usr/bin/tcsh"] {
            let (_, args) = interactive_shell_invocation(path);
            assert_eq!(
                args,
                vec!["-l".to_string()],
                "{} must be spawned with -l so its login-shell rc files are sourced",
                path
            );
        }
    }

    #[test]
    fn interactive_shell_invocation_skips_login_flag_for_unknown_shells() {
        let (_, args) = interactive_shell_invocation("/opt/exotic/myshell");
        assert!(
            args.is_empty(),
            "an unknown shell must be spawned without -l in case the flag means something else there"
        );
    }

    #[test]
    fn sniff_bracketed_paste_mode_toggles_on_set_and_reset() {
        let flag = AtomicBool::new(false);
        sniff_bracketed_paste_mode(b"prompt> \x1b[?2004h", &flag);
        assert!(flag.load(Ordering::Acquire));
        sniff_bracketed_paste_mode(b"\x1b[?2004l\nbye", &flag);
        assert!(!flag.load(Ordering::Acquire));
    }

    #[test]
    fn sniff_bracketed_paste_mode_ignores_unrelated_dec_modes() {
        let flag = AtomicBool::new(false);
        sniff_bracketed_paste_mode(b"\x1b[?25h\x1b[?1049h", &flag);
        assert!(!flag.load(Ordering::Acquire));
    }

    #[test]
    fn pty_starts_dirty_so_first_frame_renders() {
        let tmp = tempfile::tempdir().unwrap();
        let term = PtyTerminal::new(tmp.path()).unwrap();
        assert!(term.take_dirty(), "first take_dirty must be true so we draw the initial state");
    }

    #[test]
    fn take_dirty_clears_the_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let term = PtyTerminal::new(tmp.path()).unwrap();
        let _ = term.take_dirty();
        assert!(!term.take_dirty(), "second take_dirty without new bytes must be false");
    }

    #[test]
    fn write_input_marks_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term = PtyTerminal::new(tmp.path()).unwrap();
        let _ = term.take_dirty();
        term.write_input(b"echo hi\r");
        assert!(term.take_dirty(), "write_input must mark the terminal dirty");
    }

    #[test]
    fn format_cd_command_single_quotes_plain_path() {
        let bytes = format_cd_command(std::path::Path::new("/tmp/foo"));
        assert_eq!(
            bytes,
            b"\x05\x15cd '/tmp/foo'\n".to_vec(),
            "Ctrl-E + Ctrl-U must precede the cd so a half-typed prompt line is cleared first, then the path is single-quoted and newline-terminated to fire immediately"
        );
    }

    #[test]
    fn format_cd_command_escapes_embedded_apostrophe() {
        let bytes = format_cd_command(std::path::Path::new("/tmp/it's a dir"));
        assert_eq!(
            bytes,
            b"\x05\x15cd '/tmp/it'\\''s a dir'\n".to_vec(),
            "embedded ' must be escaped as '\\'' so the cd does not break out of single quotes when the path contains an apostrophe (POSIX-standard quoting)"
        );
    }

    #[test]
    fn format_cd_command_quotes_paths_with_spaces() {
        let bytes = format_cd_command(std::path::Path::new("/tmp/a b/c d"));
        assert_eq!(
            bytes,
            b"\x05\x15cd '/tmp/a b/c d'\n".to_vec(),
            "single-quoting must preserve spaces verbatim so cd lands at the right directory"
        );
    }

    #[test]
    fn change_cwd_writes_cd_command_to_pty() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term = PtyTerminal::new(tmp.path()).unwrap();
        let _ = term.take_dirty();
        let target = tempfile::tempdir().unwrap();
        term.change_cwd(target.path());
        assert!(
            term.take_dirty(),
            "change_cwd must mark the terminal dirty so the next frame paints the new prompt"
        );
    }


    #[test]
    fn new_running_spawns_program_directly_and_produces_output() {
        let tmp = tempfile::tempdir().unwrap();
        let term = PtyTerminal::new_running(
            "/bin/echo",
            &[String::from("croft-direct-spawn")],
            tmp.path(),
        )
        .unwrap();
        let mut waited_ms = 0u32;
        while waited_ms < 4000 && !term.peek_dirty() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited_ms += 20;
        }
        assert!(
            term.peek_dirty(),
            "direct-spawned /bin/echo must produce output without any write_input"
        );
    }

    #[test]
    fn new_running_renders_running_header_in_term_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let term = PtyTerminal::new_running(
            "/bin/echo",
            &[String::from("croft-header-probe")],
            tmp.path(),
        )
        .unwrap();
        let snapshot = term.visible_text();
        assert!(
            snapshot.contains("/bin/echo croft-header-probe"),
            "expected the run command in the header line; got:\n{snapshot}"
        );
        assert!(
            snapshot.starts_with('▶') || snapshot.contains("▶ "),
            "expected a ▶ marker on the header line; got:\n{snapshot}"
        );
    }

    #[test]
    fn new_running_appends_exit_footer_when_child_finishes() {
        let tmp = tempfile::tempdir().unwrap();
        let term = PtyTerminal::new_running(
            "/bin/echo",
            &[String::from("croft-exit-probe")],
            tmp.path(),
        )
        .unwrap();
        let mut waited_ms = 0u32;
        let footer_needle = "[Process exited]";
        loop {
            if term.visible_text().contains(footer_needle) {
                break;
            }
            if waited_ms >= 4000 {
                let snap = term.visible_text();
                panic!(
                    "expected '{footer_needle}' to appear within 4s of the child exiting; got:\n{snap}"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited_ms += 20;
        }
    }

    #[test]
    fn new_for_interactive_shell_does_not_render_running_header() {
        let tmp = tempfile::tempdir().unwrap();
        let term = PtyTerminal::new(tmp.path()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let snapshot = term.visible_text();
        assert!(
            !snapshot.contains("▶ "),
            "interactive shell must not render a one-shot script header; got:\n{snapshot}"
        );
        assert!(
            !snapshot.contains("[Process exited]"),
            "interactive shell must not render an exit footer; got:\n{snapshot}"
        );
    }

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
        assert!(cell_in_selection(2, 5, 2, 3, 2, 7));
        assert!(!cell_in_selection(2, 2, 2, 3, 2, 7));
        assert!(!cell_in_selection(2, 8, 2, 3, 2, 7));
        assert!(!cell_in_selection(1, 5, 2, 3, 2, 7));
        assert!(!cell_in_selection(3, 5, 2, 3, 2, 7));
    }

    #[test]
    fn cell_in_selection_spans_multiple_rows() {
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
        assert_eq!(bytes, b"\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn osc52_copy_seq_handles_empty() {
        assert_eq!(osc52_copy_seq(""), b"\x1b]52;c;\x07");
    }

    #[test]
    fn osc52_copy_seq_handles_unicode() {
        let bytes = osc52_copy_seq("héllo");
        assert!(bytes.starts_with(b"\x1b]52;c;"));
        assert_eq!(*bytes.last().unwrap(), 0x07);
        let body = &bytes[7..bytes.len() - 1];
        use base64::Engine;
        let decoded =
            base64::engine::general_purpose::STANDARD.decode(body).unwrap();
        assert_eq!(decoded, "héllo".as_bytes());
    }

    #[test]
    fn select_word_at_in_term_brackets_the_alphanumeric_run_around_the_pivot() {
        let mut t = fresh_term(40, 5);
        feed(&mut t, b"hello world from croft");
        let got = select_word_at_in_term(&t, 0, 0, 8);
        assert_eq!(got, Some(((0, 6), (0, 10))), "pivot inside 'world' must select cols 6..=10");
    }

    #[test]
    fn select_word_at_in_term_returns_none_when_pivot_is_on_whitespace() {
        let mut t = fresh_term(40, 5);
        feed(&mut t, b"hello world");
        assert_eq!(
            select_word_at_in_term(&t, 0, 0, 5),
            None,
            "pivot on the space between 'hello' and 'world' must yield no selection"
        );
    }

    #[test]
    fn select_word_at_in_term_treats_underscores_as_word_chars_and_dots_as_boundaries() {
        let mut t = fresh_term(40, 5);
        feed(&mut t, b"foo_bar.baz");
        assert_eq!(
            select_word_at_in_term(&t, 0, 0, 2),
            Some(((0, 0), (0, 6))),
            "'foo_bar' is one word (underscore is a word char), '.baz' is separate"
        );
        assert_eq!(
            select_word_at_in_term(&t, 0, 0, 9),
            Some(((0, 8), (0, 10))),
            "pivot inside 'baz' must select cols 8..=10"
        );
    }

    /// Double-click on a word that lives in scrollback must still
    /// word-select. Pre-fix, `select_word_at_in_term` ignored the
    /// scrollback offset and read the live grid at the same viewport
    /// row, which was usually blank — the lookup returned None and
    /// the user saw "double-click doesn't auto-select."
    #[test]
    fn select_word_at_in_term_word_selects_in_scrollback_when_display_offset_is_nonzero() {
        let mut t = fresh_term(20, 3);
        // Push three lines into scrollback by feeding six rows into a
        // three-row viewport.
        feed(
            &mut t,
            b"scroll-A row\r\nscroll-B row\r\nscroll-C row\r\nlive-D row\r\nlive-E row\r\nlive-F row",
        );
        t.scroll_display(alacritty_terminal::grid::Scroll::Delta(2));
        let display_offset = t.grid().display_offset();
        assert_eq!(
            display_offset, 2,
            "precondition: Scroll::Delta(2) must move the viewport into scrollback"
        );
        // Viewport row 0 with display_offset=2 = grid Line(-2) =
        // "scroll-B row". Pivot at column 2 sits inside "scroll-B".
        let got = select_word_at_in_term(&t, display_offset, 0, 2);
        assert!(
            got.is_some(),
            "double-click on a scrolled-back row must return a real anchor/head, not None - if this is None the user sees the double-click 'do nothing'"
        );
        let (anchor, head) = got.unwrap();
        // "scroll-B" spans cols 0..=7 (s=0, c=1, r=2, o=3, l=4, l=5,
        // -=… actually '-' is not a word char, so the word is
        // "scroll" at cols 0..=5). The pivot is at col 2, which
        // lives inside "scroll". The selection must cover that run.
        assert_eq!(
            anchor, (0, 0),
            "anchor must mark the start of the word run on viewport row 0"
        );
        assert_eq!(
            head, (0, 5),
            "head must mark the last char of 'scroll' before the hyphen breaks the word run"
        );
    }

    #[test]
    fn extract_selection_text_single_line() {
        let mut t = fresh_term(20, 5);
        feed(&mut t, b"hello world");
        let txt = extract_selection_text(&t, 0, 0, 6, 0, 10);
        assert_eq!(txt, "world");
    }

    #[test]
    fn extract_selection_text_multi_line_trims_trailing_spaces() {
        let mut t = fresh_term(20, 5);
        feed(&mut t, b"first line\r\nsecond line");
        let txt = extract_selection_text(&t, 0, 0, 6, 1, 5);
        assert_eq!(txt, "line\nsecond");
    }

    /// User-visible regression: when the terminal is scrolled back into
    /// history and the user selects a row in the scrollback, the
    /// clipboard must contain that scrollback row, not whatever happens
    /// to be at the same viewport y on the live grid. This is the bug
    /// the user hit in production where selecting "useUploadArtifact is"
    /// pasted "▎ 3000 and 2000 are u" and selecting a row from a
    /// scrolled-back file:line marker pasted a different file:line —
    /// `extract_selection_text` was reading `Line(viewport_row)`
    /// regardless of `display_offset`, so once the user scrolled the
    /// painted highlight and the extracted text diverged.
    /// User-visible regression test: feed enough rows to a 3-row
    /// viewport that the first three lines spill into scrollback, then
    /// scroll the viewport back and confirm that
    /// `extract_selection_text` returns the scrollback row the user
    /// sees, not the live-grid row at the same viewport y.
    #[test]
    fn extract_selection_text_reads_scrollback_when_display_offset_is_nonzero() {
        let mut t = fresh_term(20, 3);
        // Six lines into a 3-row viewport. Each `\r\n` past the bottom
        // pushes the topmost row into scrollback. End-state:
        //   scrollback (oldest → newest): scroll-A, scroll-B, scroll-C
        //   live (top → bottom):          live-D,   live-E,   live-F
        // Cursor parks at the end of live-F, display_offset = 0.
        feed(
            &mut t,
            b"scroll-A row\r\nscroll-B row\r\nscroll-C row\r\nlive-D row\r\nlive-E row\r\nlive-F row",
        );

        // Sanity-check the live screen before scrolling: viewport row 0
        // is `live-D` because the three older rows already moved to
        // scrollback.
        let live_top_before = extract_selection_text(&t, 0, 0, 0, 0, 19);
        assert_eq!(
            live_top_before, "live-D row",
            "precondition: with no scroll, viewport row 0 must read 'live-D row' from the live grid - if this fails the scrollback never filled, so the scrolled-back assertion below would prove nothing"
        );

        // Scroll the viewport up by 2 rows into history. The cells the
        // user now sees at viewport y=0,1,2 correspond to scrollback
        // lines −2, −1 and live line 0 respectively. (alacritty's
        // `Scroll::Delta(n>0)` moves the viewport into history.)
        t.scroll_display(alacritty_terminal::grid::Scroll::Delta(2));
        let display_offset = t.grid().display_offset();
        assert_eq!(
            display_offset, 2,
            "precondition: Scroll::Delta(2) must produce display_offset=2 once scrollback has at least two lines in it"
        );

        // With the fix in place: viewport row 0 + display_offset=2
        // reads grid `Line(-2)`, the OLDER of the two scrollback rows
        // currently shown — that's `scroll-B row`.
        let scrollback = extract_selection_text(&t, display_offset, 0, 0, 0, 19);
        assert_eq!(
            scrollback, "scroll-B row",
            "viewport row 0 with display_offset=2 must surface the scrollback row the user actually sees highlighted, NOT the live grid row at the same y - this is the non-negotiable invariant the user lost when selection_text forgot to subtract display_offset, and the symptom was selecting one terminal row and pasting the contents of a completely different row"
        );

        // Without the offset (the pre-fix behaviour), the same call
        // would still report 'live-D row' because Line(0) names the
        // live first line. We assert that by passing 0 explicitly:
        let still_live = extract_selection_text(&t, 0, 0, 0, 0, 19);
        assert_eq!(
            still_live, "live-D row",
            "passing display_offset=0 must still read the live grid - the function must be honest in BOTH directions so future callers (snapshot helpers, tests) can opt into either semantic without ambiguity"
        );
    }
}

