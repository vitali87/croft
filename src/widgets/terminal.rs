use alacritty_terminal::Term;
use alacritty_terminal::event::{Event as AlacEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Processor, StdSyncHandler};
use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Widget},
};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const SCROLLBACK_LINES: usize = 5000;

/// A terminal text selection, anchored to *absolute* alacritty grid lines
/// rather than viewport rows. `line` is the grid `Line` index: `0..rows`
/// is the live screen, negative values are scrollback history. Storing
/// absolute lines (instead of `row - inner.y`) is what lets a selection
/// be taller than the visible pane and survive scrolling — the highlight
/// and the extracted text both track content, not screen position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (i32, u16),
    pub head: (i32, u16),
}

impl Selection {
    pub fn new(line: i32, col: u16) -> Self {
        Self {
            anchor: (line, col),
            head: (line, col),
        }
    }
    pub fn normalised(&self) -> (i32, u16, i32, u16) {
        let (a_l, a_c) = self.anchor;
        let (b_l, b_c) = self.head;
        if (a_l, a_c) <= (b_l, b_c) {
            (a_l, a_c, b_l, b_c)
        } else {
            (b_l, b_c, a_l, a_c)
        }
    }
    pub fn has_area(&self) -> bool {
        self.anchor != self.head
    }
}

/// Listener for events the embedded `Term` emits. Most variants (title
/// changes, cursor-blink toggles, clipboard load/store, child exit, etc.)
/// are owned by the outer croft TUI and ignored here, but two of them
/// MUST be reflected back into the shell's stdin or interactive TUIs
/// running inside the embedded terminal hang waiting for replies:
///
///   * `PtyWrite(text)` — DSR (`ESC[6n`), DA1/DA2 (`ESC[c`, `ESC[>c`),
///     keyboard modifier reports, sixel/synchronised-output queries,
///     and OSC 52 read backs all surface here. atuin times out with
///     "The cursor position could not be read within a normal duration"
///     when the DSR reply never lands on the PTY; helix, fzf, btop and
///     other Rust/Go TUIs do the same.
///   * `TextAreaSizeRequest(cb)` — `CSI 14 t` / `CSI 18 t` pixel/cell
///     size queries from TUIs that scale glyph cells (e.g. helix's
///     terminal-size detection, sixel-aware viewers). The callback
///     builds the reply string from the supplied `WindowSize`.
///
/// In tests we use `VoidListener::default()` (both channels `None`) so
/// the listener silently drops every event — the test helpers exercise
/// the parser, not the round-trip.
#[derive(Clone, Default)]
pub struct VoidListener {
    pty_response_tx: Option<std::sync::mpsc::Sender<String>>,
    size: Option<Arc<std::sync::Mutex<(u16, u16)>>>,
}

impl EventListener for VoidListener {
    fn send_event(&self, event: AlacEvent) {
        let Some(tx) = self.pty_response_tx.as_ref() else {
            return;
        };
        match event {
            AlacEvent::PtyWrite(text) => {
                let _ = tx.send(text);
            }
            AlacEvent::TextAreaSizeRequest(cb) => {
                let Some(size) = self.size.as_ref() else {
                    return;
                };
                let (cols, rows) = *size.lock().unwrap();
                let ws = alacritty_terminal::event::WindowSize {
                    num_lines: rows,
                    num_cols: cols,
                    cell_width: 1,
                    cell_height: 1,
                };
                let _ = tx.send(cb(ws));
            }
            _ => {}
        }
    }
}

pub struct PtyTerminal {
    term: Arc<FairMutex<Term<VoidListener>>>,
    /// Set by the PTY reader thread on every chunk and by `write_input`;
    /// cleared by `take_dirty`. The main loop only redraws when set.
    pty_dirty: Arc<AtomicBool>,
    /// Bytes the reader thread has advanced into the grid since the last
    /// redraw (reset by `take_dirty`). The main loop reads this to tell an
    /// interactive echo (a few bytes: a keystroke, a shell line-rewrite,
    /// an input-box update) from a bulk stream (cat / build logs / a
    /// full-pane scroll). Small updates bypass the PTY redraw cap so echo
    /// stays native; large ones stay capped so they can't saturate the
    /// ssh pipe and starve input.
    pty_pending_bytes: Arc<AtomicUsize>,
    /// Tracks whether the inner program has enabled DECSET 2004 (bracketed
    /// paste). Sniffed off the byte stream; not all parsers expose it.
    bracketed_paste_enabled: Arc<AtomicBool>,
    master: Box<dyn MasterPty + Send>,
    /// Shared between this struct's user-input path (`write_input`,
    /// `paste_*`, `cd_into`) and the background responder thread that
    /// ships alacritty's reply bytes (`PtyWrite`, `TextAreaSizeRequest`)
    /// back to the shell's stdin. portable-pty rejects a second
    /// `take_writer` call, so there is exactly one underlying writer
    /// and both paths funnel through this mutex.
    writer: Arc<std::sync::Mutex<Box<dyn Write + Send>>>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    /// The interactive shell's pid, captured at spawn. A shell at its
    /// prompt is its own foreground process-group leader, so this is the
    /// value `foreground_is_shell` compares `tcgetpgrp(master)` against.
    shell_pid: Option<i32>,
    cols: u16,
    rows: u16,
    /// Shared with the `VoidListener` so `TextAreaSizeRequest` callbacks
    /// see the live geometry without us having to thread a fresh
    /// listener instance through every `resize` call.
    size_shared: Arc<std::sync::Mutex<(u16, u16)>>,
    pub focused: bool,
    /// When focused, draw the orange→green gradient border (Black theme)
    /// instead of the solid blue one. Set by the app's focus/theme sync.
    pub focus_gradient: bool,
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
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let (program, args) = interactive_shell_invocation(&shell);
        let mut cmd = CommandBuilder::new(&program);
        for a in &args {
            cmd.arg(a);
        }
        cmd.cwd(cwd);
        Self::spawn_with(cmd, None)
    }

    pub fn new_running(program: &str, args: &[String], cwd: &std::path::Path) -> Result<Self> {
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
        let off = term.grid().display_offset() as i32;
        extract_selection_text(&term, -off, 0, rows as i32 - 1 - off, cols - 1)
    }

    fn spawn_with(mut cmd: CommandBuilder, run_label: Option<String>) -> Result<Self> {
        let pty_system = native_pty_system();
        let cols = 80u16;
        let rows = 24u16;
        let pair = pty_system
            .openpty(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;

        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).context("spawn child")?;
        let shell_pid = child.process_id().map(|p| p as i32);
        drop(pair.slave);

        let writer: Arc<std::sync::Mutex<Box<dyn Write + Send>>> = Arc::new(std::sync::Mutex::new(
            pair.master.take_writer().context("take writer")?,
        ));
        let mut reader = pair.master.try_clone_reader().context("clone reader")?;

        let term_size = TermSize::new(cols as usize, rows as usize);
        let cfg = Config {
            scrolling_history: SCROLLBACK_LINES,
            ..Config::default()
        };
        let size_shared = Arc::new(std::sync::Mutex::new((cols, rows)));
        let (response_tx, response_rx) = std::sync::mpsc::channel::<String>();
        let listener = VoidListener {
            pty_response_tx: Some(response_tx),
            size: Some(size_shared.clone()),
        };
        let term = Term::new(cfg, &term_size, listener);
        let term = Arc::new(FairMutex::new(term));
        let term_for_thread = term.clone();
        let writer_for_responder = writer.clone();

        std::thread::spawn(move || {
            while let Ok(text) = response_rx.recv() {
                let Ok(mut w) = writer_for_responder.lock() else {
                    break;
                };
                if w.write_all(text.as_bytes()).is_err() {
                    break;
                }
                if w.flush().is_err() {
                    break;
                }
            }
        });

        let pty_dirty = Arc::new(AtomicBool::new(true));
        let pty_dirty_for_thread = pty_dirty.clone();
        let pty_pending_bytes = Arc::new(AtomicUsize::new(0));
        let pty_pending_bytes_for_thread = pty_pending_bytes.clone();
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
                        sniff_bracketed_paste_mode(&buf[..n], &bracketed_paste_for_thread);
                        let mut t = term_for_thread.lock();
                        processor.advance(&mut *t, &buf[..n]);
                        drop(t);
                        pty_pending_bytes_for_thread.fetch_add(n, Ordering::Relaxed);
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
            pty_pending_bytes,
            bracketed_paste_enabled,
            master: pair.master,
            writer,
            _child: child,
            shell_pid,
            cols,
            rows,
            size_shared,
            focused: false,
            focus_gradient: false,
            last_area: Rect::default(),
            last_inner: Rect::default(),
            selection: None,
        })
    }

    pub fn take_dirty(&self) -> bool {
        self.pty_pending_bytes.store(0, Ordering::Relaxed);
        self.pty_dirty.swap(false, Ordering::AcqRel)
    }

    /// Bytes advanced into the grid since the last redraw, without
    /// clearing. The main loop uses this to classify a PTY-only redraw as
    /// interactive echo (small, redraw now) or bulk stream (large, stay
    /// capped). Reset on the next `take_dirty`.
    pub fn peek_pending_bytes(&self) -> usize {
        self.pty_pending_bytes.load(Ordering::Relaxed)
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

    /// Current scrollback offset: how many rows the viewport is scrolled
    /// up from the live bottom. Viewport row `r` maps to absolute grid
    /// line `r - display_offset`.
    fn display_offset(&self) -> i32 {
        self.term.lock().grid().display_offset() as i32
    }

    pub fn start_selection_at(&mut self, col: u16, row: u16) {
        if let Some((r, c)) = self.cell_at(col, row) {
            let line = r as i32 - self.display_offset();
            self.selection = Some(Selection::new(line, c));
        }
    }

    pub fn extend_selection_to(&mut self, col: u16, row: u16) {
        let cell = self.cell_at(col, row);
        let off = self.display_offset();
        if let (Some(sel), Some((r, c))) = (self.selection.as_mut(), cell) {
            sel.head = (r as i32 - off, c);
        }
    }

    /// Extend a drag-selection toward a pointer that may have left the
    /// pane. Columns and rows are clamped to the inner content rect; the
    /// returned value is the auto-scroll direction the caller should keep
    /// applying while the pointer stays past an edge: `-1` (pointer above
    /// the top, scroll into history), `+1` (below the bottom, scroll
    /// toward live), or `0` (inside the pane, no auto-scroll). Mirrors
    /// the click-drag-past-the-edge selection growth in iTerm2/VS Code.
    pub fn drag_select_to(&mut self, col: u16, row: u16) -> i32 {
        let inner = self.last_inner;
        if inner.width == 0 || inner.height == 0 {
            return 0;
        }
        let top = inner.y;
        let bottom = inner.y + inner.height - 1;
        let (vp_row, dir) = if row < top {
            (0u16, -1)
        } else if row > bottom {
            (inner.height - 1, 1)
        } else {
            (row - inner.y, 0)
        };
        let max_x = inner.x + inner.width - 1;
        let c = col.clamp(inner.x, max_x) - inner.x;
        let off = self.display_offset();
        if let Some(sel) = self.selection.as_mut() {
            sel.head = (vp_row as i32 - off, c);
        }
        dir
    }

    /// One step of edge auto-scroll while a drag-selection is held past
    /// the top (`dir < 0`) or bottom (`dir > 0`) edge. Scrolls the
    /// viewport by one row and re-pins the selection head to the new edge
    /// line at the last-known drag column. No-op in alternate-screen mode
    /// (where the inner program owns scrolling).
    pub fn autoscroll_select(&mut self, dir: i32, col: u16) {
        if dir == 0 {
            return;
        }
        let scrolled = if dir < 0 {
            self.scroll_up(1)
        } else {
            self.scroll_down(1)
        };
        if !scrolled {
            return;
        }
        let inner = self.last_inner;
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let max_x = inner.x + inner.width - 1;
        let c = col.clamp(inner.x, max_x) - inner.x;
        let vp_row = if dir < 0 { 0u16 } else { inner.height - 1 };
        let off = self.display_offset();
        if let Some(sel) = self.selection.as_mut() {
            sel.head = (vp_row as i32 - off, c);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Double-click word-select: expand the selection to cover the word
    /// under the screen coordinate `(col, row)`. No-op when the click
    /// lands on whitespace / punctuation or outside the live area.
    pub fn select_word_at(&mut self, col: u16, row: u16) {
        let Some((r, c)) = self.cell_at(col, row) else {
            return;
        };
        let term = self.term.lock();
        let display_offset = term.grid().display_offset();
        let Some((anchor, head)) =
            select_word_at_in_term(&term, display_offset, r as usize, c as usize)
        else {
            return;
        };
        drop(term);
        // `select_word_at_in_term` reports viewport rows; anchor them to
        // absolute grid lines so the selection stays put when scrolled.
        let off = display_offset as i32;
        let to_abs = |(vr, vc): (u16, u16)| (vr as i32 - off, vc);
        self.selection = Some(Selection {
            anchor: to_abs(anchor),
            head: to_abs(head),
        });
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub fn selection_text(&self) -> String {
        let Some(sel) = self.selection else {
            return String::new();
        };
        let (sr, sc, er, ec) = sel.normalised();
        let term = self.term.lock();
        extract_selection_text(&term, sr, sc as usize, er, ec as usize)
    }

    pub fn write_input(&mut self, data: &[u8]) {
        self.reset_scrollback();
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(data);
            let _ = w.flush();
        }
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

    /// True when the embedded shell is sitting at its prompt rather than
    /// running a foreground application (an editor, a pager, or an agent
    /// like Claude Code). Compares the PTY's foreground process group
    /// against the shell's own pid: a shell at its prompt is its own
    /// process-group leader, so `tcgetpgrp(master) == shell_pid`. Once
    /// the shell forks a command, that command owns a new foreground
    /// group and the two diverge. The only case that must suppress a
    /// `cd` seed is a foreground group that exists AND is not the shell,
    /// because that is an app reading the tty in raw mode; a missing
    /// foreground group (the brief window after spawn, before the shell
    /// claims the tty) means nothing has grabbed input, so a `cd` is
    /// still safe. `tcgetpgrp` is identical on macOS and Linux.
    pub fn foreground_is_shell(&self) -> bool {
        match (self.master.process_group_leader(), self.shell_pid) {
            (Some(fg), Some(pid)) => fg == pid,
            _ => true,
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
        *self.size_shared.lock().unwrap() = (cols, rows);
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
    sr: i32,
    sc: usize,
    er: i32,
    ec: usize,
) -> String {
    let cols = term.columns();
    // Clamp the range to the grid: the newest live line is
    // `screen_lines - 1`; the oldest readable line is the grid's topmost
    // (scrollback floor). Reading outside that panics alacritty's index.
    let max_line = term.screen_lines() as i32 - 1;
    let min_line = term.grid().topmost_line().0;
    let sr = sr.max(min_line);
    let er = er.min(max_line);
    let mut out = String::new();
    let mut line_idx = sr;
    while line_idx <= er {
        let row_start = if line_idx == sr { sc } else { 0 };
        let row_end = if line_idx == er {
            ec.min(cols.saturating_sub(1))
        } else {
            cols.saturating_sub(1)
        };
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
        if line_idx != er {
            out.push('\n');
        }
        line_idx += 1;
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
        // Black theme (`focus_gradient` doubles as the theme flag pushed to
        // every pane): chipless VS Code-style panel header — the gradient
        // border carries the brand, so the title drops the legacy navy chip.
        // Croft Dark keeps the historical white-on-navy chip.
        let title = if self.focus_gradient {
            Span::styled(
                " TERMINAL ",
                Style::default()
                    .fg(crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                " TERMINAL ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                    .add_modifier(Modifier::BOLD),
            )
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style)
            .title(title.clone());
        let inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the orange→green
        // gradient (matching the welcome activity box), then re-stamp the
        // title the gradient top edge just overwrote.
        if self.focused && self.focus_gradient {
            crate::gradient::paint_gradient_box(buf, area);
            buf.set_span(area.x + 1, area.y, &title, title.width() as u16);
        }
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
                if let Some((sr, sc, er, ec)) = sel_norm
                    && cell_in_selection(line_idx, x, sr, sc, er, ec)
                {
                    style = style.bg(Color::Rgb(0x26, 0x4f, 0x78));
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
pub fn cell_in_selection(row: i32, col: u16, sr: i32, sc: u16, er: i32, ec: u16) -> bool {
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
        let cfg = Config {
            scrolling_history: 1000,
            ..Config::default()
        };
        let size = TermSize::new(cols, rows);
        Term::new(cfg, &size, VoidListener::default())
    }

    fn feed(term: &mut Term<VoidListener>, bytes: &[u8]) {
        let mut p = Processor::<StdSyncHandler>::new();
        p.advance(term, bytes);
    }

    #[test]
    fn dsr_cursor_position_query_round_trips_into_the_pty_response_channel() {
        // Regression for atuin reverse-search (Ctrl+R) panicking with
        // "Error: The cursor position could not be read within a normal
        // duration": atuin sends DSR (`ESC[6n`) and waits for the
        // terminal to write back `ESC[<row>;<col>R` on stdin. Before
        // this fix, croft's `VoidListener` discarded every event
        // alacritty emitted, so the reply never reached the shell and
        // atuin timed out. With the responder wiring in place, the
        // listener must publish the reply on the channel that the
        // background thread forwards to the PTY master.
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let listener = VoidListener {
            pty_response_tx: Some(tx),
            size: Some(Arc::new(std::sync::Mutex::new((80, 24)))),
        };
        let cfg = Config::default();
        let size = TermSize::new(80, 24);
        let mut term = Term::new(cfg, &size, listener);
        let mut processor = Processor::<StdSyncHandler>::new();
        processor.advance(&mut term, b"\x1b[6n");
        let response = rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .expect("DSR cursor-position query must produce a PtyWrite event");
        assert_eq!(
            response, "\x1b[1;1R",
            "a fresh terminal's cursor is at row 1, column 1 (1-based), so the reply must be ESC[1;1R"
        );
    }

    #[test]
    fn text_area_size_request_replies_with_the_current_grid_geometry() {
        // CSI 18 t ("report text area size in characters") is what
        // helix, neovim, and other modern TUIs use to learn the
        // viewport size beyond what TIOCGWINSZ surfaces. The reply
        // must echo the live cols/rows held in `size_shared` so a
        // resize inside croft propagates to the embedded program
        // without a fresh listener instance.
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let size = Arc::new(std::sync::Mutex::new((120u16, 40u16)));
        let listener = VoidListener {
            pty_response_tx: Some(tx),
            size: Some(size),
        };
        let cfg = Config::default();
        let term_size = TermSize::new(120, 40);
        let mut term = Term::new(cfg, &term_size, listener);
        let mut processor = Processor::<StdSyncHandler>::new();
        processor.advance(&mut term, b"\x1b[18t");
        let response = rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .expect("CSI 18 t must produce a TextAreaSizeRequest reply on the channel");
        assert_eq!(
            response, "\x1b[8;40;120t",
            "CSI 18 t reply format is ESC[8;<rows>;<cols>t per xterm convention"
        );
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
        for path in [
            "/bin/bash",
            "/usr/local/bin/fish",
            "/bin/ksh",
            "/usr/bin/tcsh",
        ] {
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
        assert!(
            term.take_dirty(),
            "first take_dirty must be true so we draw the initial state"
        );
    }

    #[test]
    fn take_dirty_clears_the_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let term = PtyTerminal::new(tmp.path()).unwrap();
        let _ = term.take_dirty();
        assert!(
            !term.take_dirty(),
            "second take_dirty without new bytes must be false"
        );
    }

    #[test]
    fn write_input_marks_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term = PtyTerminal::new(tmp.path()).unwrap();
        let _ = term.take_dirty();
        term.write_input(b"echo hi\r");
        assert!(
            term.take_dirty(),
            "write_input must mark the terminal dirty"
        );
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
    fn foreground_is_shell_flips_false_while_a_foreground_command_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term = PtyTerminal::new(tmp.path()).unwrap();

        let mut waited = 0u32;
        while waited < 4000 && !term.foreground_is_shell() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert!(
            term.foreground_is_shell(),
            "a shell at its prompt is the tty's foreground process group, so a cd is safe to inject"
        );

        term.write_input(b"sleep 10\n");
        let mut waited = 0u32;
        while waited < 4000 && term.foreground_is_shell() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert!(
            !term.foreground_is_shell(),
            "while a foreground command owns the tty, the foreground group is the command not the shell, so a cd must not be injected"
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
    fn pending_bytes_counts_advanced_output_and_resets_on_take_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        let term = PtyTerminal::new_running(
            "/bin/echo",
            &[String::from("croft-pending-bytes-probe")],
            tmp.path(),
        )
        .unwrap();
        let mut waited_ms = 0u32;
        while waited_ms < 4000 && term.peek_pending_bytes() == 0 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited_ms += 20;
        }
        assert!(
            term.peek_pending_bytes() > 0,
            "the reader thread must accumulate the bytes it advanced so the main loop can tell echo from a bulk stream"
        );
        assert!(
            term.peek_pending_bytes() <= 4096,
            "a one-line echo must stay under the small-update threshold so it bypasses the redraw cap"
        );
        let _ = term.take_dirty();
        assert_eq!(
            term.peek_pending_bytes(),
            0,
            "take_dirty must reset the byte accumulator so the next window starts from zero"
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
        let term =
            PtyTerminal::new_running("/bin/echo", &[String::from("croft-exit-probe")], tmp.path())
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
        let s = Selection {
            anchor: (5, 4),
            head: (2, 1),
        };
        assert_eq!(s.normalised(), (2, 1, 5, 4));
    }

    #[test]
    fn selection_normalised_handles_same_row() {
        let s = Selection {
            anchor: (3, 9),
            head: (3, 2),
        };
        assert_eq!(s.normalised(), (3, 2, 3, 9));
    }

    #[test]
    fn selection_has_area_only_when_endpoints_differ() {
        let s = Selection::new(2, 5);
        assert!(!s.has_area());
        let s2 = Selection {
            anchor: (2, 5),
            head: (2, 6),
        };
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
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(body)
            .unwrap();
        assert_eq!(decoded, "héllo".as_bytes());
    }

    #[test]
    fn select_word_at_in_term_brackets_the_alphanumeric_run_around_the_pivot() {
        let mut t = fresh_term(40, 5);
        feed(&mut t, b"hello world from croft");
        let got = select_word_at_in_term(&t, 0, 0, 8);
        assert_eq!(
            got,
            Some(((0, 6), (0, 10))),
            "pivot inside 'world' must select cols 6..=10"
        );
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
            anchor,
            (0, 0),
            "anchor must mark the start of the word run on viewport row 0"
        );
        assert_eq!(
            head,
            (0, 5),
            "head must mark the last char of 'scroll' before the hyphen breaks the word run"
        );
    }

    #[test]
    fn extract_selection_text_single_line() {
        let mut t = fresh_term(20, 5);
        feed(&mut t, b"hello world");
        let txt = extract_selection_text(&t, 0, 6, 0, 10);
        assert_eq!(txt, "world");
    }

    #[test]
    fn extract_selection_text_multi_line_trims_trailing_spaces() {
        let mut t = fresh_term(20, 5);
        feed(&mut t, b"first line\r\nsecond line");
        let txt = extract_selection_text(&t, 0, 6, 1, 5);
        assert_eq!(txt, "line\nsecond");
    }

    /// `extract_selection_text` now addresses absolute grid lines, so a
    /// negative line index reads scrollback directly and a given line
    /// returns the same content no matter how the viewport is scrolled.
    /// This is the property that lets a selection survive scrolling: the
    /// endpoints name content, not screen rows.
    #[test]
    fn extract_selection_text_addresses_absolute_grid_lines() {
        let mut t = fresh_term(20, 3);
        // Six lines into a 3-row viewport. Each `\r\n` past the bottom
        // pushes the topmost row into scrollback. End-state:
        //   scrollback: scroll-A = Line(-3), scroll-B = Line(-2), scroll-C = Line(-1)
        //   live:       live-D = Line(0), live-E = Line(1), live-F = Line(2)
        feed(
            &mut t,
            b"scroll-A row\r\nscroll-B row\r\nscroll-C row\r\nlive-D row\r\nlive-E row\r\nlive-F row",
        );

        assert_eq!(
            extract_selection_text(&t, 0, 0, 0, 19),
            "live-D row",
            "Line(0) is the first live row"
        );
        assert_eq!(
            extract_selection_text(&t, -2, 0, -2, 19),
            "scroll-B row",
            "Line(-2) reads the scrollback row directly, no display_offset needed"
        );

        // Scrolling the viewport must NOT change what an absolute line
        // returns - that invariant is why a selection no longer drifts
        // when the user scrolls mid-drag.
        t.scroll_display(alacritty_terminal::grid::Scroll::Delta(2));
        assert_eq!(t.grid().display_offset(), 2);
        assert_eq!(
            extract_selection_text(&t, -2, 0, -2, 19),
            "scroll-B row",
            "Line(-2) still reads 'scroll-B row' after scrolling - absolute lines are scroll-invariant"
        );
    }

    /// The whole point of the fix: a selection that spans from scrollback
    /// history into the live screen extracts every line, even though it
    /// is far taller than the 3-row viewport. Before the absolute-
    /// coordinate model the selection was capped at the visible pane
    /// height and the off-screen lines were simply unreachable.
    #[test]
    fn selection_spanning_scrollback_into_live_extracts_every_line() {
        let mut t = fresh_term(20, 3);
        feed(&mut t, b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD\r\nEEEE\r\nFFFF");
        // Select from the oldest scrollback line (AAAA = Line(-3)) all
        // the way to the last live line (FFFF = Line(2)).
        let all = extract_selection_text(&t, -3, 0, 2, 19);
        assert_eq!(all, "AAAA\nBBBB\nCCCC\nDDDD\nEEEE\nFFFF");
    }
}
