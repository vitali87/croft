use alacritty_terminal::Term;
use alacritty_terminal::event::{Event as AlacEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection as TracerSelection, SelectionType};
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
    widgets::{Block, Borders, Widget},
};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const SCROLLBACK_LINES: usize = 5000;

/// A mouse button (or wheel direction) to forward to the child program
/// when it has mouse tracking enabled. Encoded as an SGR (1006) report when
/// the child selected that encoding, otherwise as a legacy X10 report.
#[derive(Clone, Copy, PartialEq)]
pub enum MouseButtonKind {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

/// What happened to the button: a press, a release, or motion while held.
#[derive(Clone, Copy, PartialEq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// Keyboard modifiers held during a mouse event, folded into the report's
/// button byte (Shift +4, Alt +8, Ctrl +16).
#[derive(Clone, Copy, Default)]
pub struct MouseMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

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
    /// Rectangular (column-block) selection: the highlight and the copied
    /// text cover the same column slice on every row between the endpoints
    /// (copy mode's Ctrl+V), instead of running row-major to the ends.
    pub block: bool,
}

/// What an alternate-screen selection remembers about its content so the
/// highlight can follow text that the app moves by repainting (alt-screen
/// programs own scrolling: the grid never scrolls, cells just change).
/// After every repaint the selection re-finds these rows in the grid and
/// shifts itself to them; while they are nowhere in view it goes `dormant`
/// — hidden, coordinates frozen — and reappears when the app scrolls the
/// content back.
struct AltSelAnchor {
    /// Full row text of every selected grid line at last sight, top to
    /// bottom. Rows the shift math placed outside the grid stay remembered
    /// so the block can re-anchor when they scroll back in.
    rows: Vec<String>,
    /// Grid line `rows[0]` was last seen at. Drift is measured against
    /// THIS, never against the selection's own top: mid-drag the selection
    /// top is the pointer (the head), not the anchored content, and
    /// deriving drift from it walked the anchor one row per mouse event on
    /// upward drags.
    top: i32,
    /// The extracted selection text at last sight: what copy yields while
    /// the content is scrolled out of view.
    text: String,
    /// The rows are nowhere in the grid right now: paint nothing, keep the
    /// coordinates frozen until the content reappears.
    dormant: bool,
    /// The rows (and column spans) that actually survive on screen when
    /// the block is only partially visible — the rest is off the grid or
    /// overdrawn by app chrome (Claude Code's input box, its floating
    /// "Jump to bottom" pill). Each entry is `(grid line, first column,
    /// last column)`: a row half-covered by a pill keeps its surviving
    /// prefix/suffix highlighted. Painting clips to these; `None` while
    /// the whole block is visible.
    visible: Option<Vec<(i32, u16, u16)>>,
    /// Row text just above / below the block at capture time (`None` at a
    /// grid edge). Pure tie-breakers for re-anchoring: full-screen apps
    /// repeat rows verbatim (divider rules, continuation markers), so a
    /// selection whose own fingerprint matches several shifts equally —
    /// inevitably a one-row selection on such a row — follows the copy
    /// whose neighbours also match instead of the nearest lookalike.
    ctx_above: Option<String>,
    ctx_below: Option<String>,
}

impl Selection {
    pub fn new(line: i32, col: u16) -> Self {
        Self {
            anchor: (line, col),
            head: (line, col),
            block: false,
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
    /// Rectangle bounds with rows and columns min/maxed independently
    /// (row-major normalisation would pair the wrong corners when the head
    /// sits below-left of the anchor): (row_lo, col_lo, row_hi, col_hi).
    pub fn block_bounds(&self) -> (i32, u16, i32, u16) {
        let (a_l, a_c) = self.anchor;
        let (b_l, b_c) = self.head;
        (a_l.min(b_l), a_c.min(b_c), a_l.max(b_l), a_c.max(b_c))
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
    /// Latched when the child rings BEL (`\a`); the app drains it via
    /// [`PtyTerminal::take_bell`] to surface the bell in the UI.
    bell: Option<Arc<AtomicBool>>,
}

impl EventListener for VoidListener {
    fn send_event(&self, event: AlacEvent) {
        match event {
            AlacEvent::Bell => {
                if let Some(bell) = self.bell.as_ref() {
                    bell.store(true, Ordering::Release);
                }
            }
            AlacEvent::PtyWrite(text) => {
                if let Some(tx) = self.pty_response_tx.as_ref() {
                    let _ = tx.send(text);
                }
            }
            AlacEvent::TextAreaSizeRequest(cb) => {
                let (Some(tx), Some(size)) = (self.pty_response_tx.as_ref(), self.size.as_ref())
                else {
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

/// A row arriving this long after its predecessor is highlighted amber in
/// the timestamps gutter (the "where did the deploy stall" signal).
const STALL_GAP_MS: u64 = 60_000;

/// The pane's monotonic scroll clock: lines the primary screen has
/// scrolled since the pane spawned, read from a one-cell tracer selection's
/// drift. Immune to the `history_size` saturation that froze selections in
/// long-lived panes (scrollback full for ages, so a history-growth delta
/// was pinned at zero while content kept rotating through the ring). Alt
/// screen freezes the clock — output goes to the alternate grid while
/// primary content holds still. A dead tracer falls back to history growth
/// since the last tick: exact for the viewport-pushing clear that killed
/// it, zero for an alt-screen round trip.
///
/// Shared (behind a mutex) between the render thread — selection,
/// copy-mode, and annotation re-anchoring — and the PTY reader thread,
/// which keys arrival timestamps on it. Lock order everywhere:
/// term → clock → line_times; the term lock is held across the whole
/// sequence on both threads, and the inner two always nest in that order.
#[derive(Clone, Debug)]
struct ScrollClock {
    /// Monotonic reading, folded up to the last `tick`.
    base: i64,
    /// Grid line the tracer selection was planted at during the last tick;
    /// `None` before the first tick.
    planted: Option<i32>,
    /// `history_size` at the last tick: fallback delta source for the rare
    /// windows where the tracer died (screen clear, alt-screen round trip).
    hist: i64,
}

impl ScrollClock {
    fn new() -> Self {
        Self {
            base: 0,
            planted: None,
            hist: 0,
        }
    }

    /// Plant the one-cell tracer selection at `line` in alacritty's own
    /// `Term::selection` slot (croft paints selections itself, so the slot
    /// is otherwise unused). alacritty rotates it on every scroll —
    /// including ring rotation once the scrollback is full, where
    /// `history_size` saturates and stops counting — so its drift between
    /// ticks is the true lines-scrolled count.
    fn plant_tracer(term: &mut Term<VoidListener>, line: i32) {
        let point = Point::new(Line(line), Column(0));
        let mut tracer = TracerSelection::new(SelectionType::Simple, point, Side::Left);
        tracer.update(point, Side::Right);
        term.selection = Some(tracer);
    }

    /// Where the tracer sits now, when it survived since the last plant
    /// (screen clears, alt-screen swaps and content rotating fully off the
    /// scrollback kill it).
    fn tracer_line(term: &Term<VoidListener>) -> Option<i32> {
        let range = term.selection.as_ref()?.to_range(term)?;
        Some(range.start.line.0)
    }

    fn now(&self, term: &Term<VoidListener>) -> i64 {
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return self.base;
        }
        match (self.planted, Self::tracer_line(term)) {
            (Some(planted), Some(cur)) => self.base + i64::from(planted - cur),
            _ => {
                let growth = term.grid().history_size() as i64 - self.hist;
                self.base + growth.max(0)
            }
        }
    }

    /// Fold the tracer's drift into the clock, re-plant it fresh and return
    /// the folded reading. The newest history line is the parking spot:
    /// application clears only touch live-screen rows, so nothing kills it
    /// there, and it sits a full scrollback's depth from rotating off the
    /// top before the next tick (every rendered frame ticks).
    fn tick(&mut self, term: &mut Term<VoidListener>) -> i64 {
        let now = self.now(term);
        if !term.mode().contains(TermMode::ALT_SCREEN) {
            self.base = now;
            self.hist = term.grid().history_size() as i64;
            let park = if self.hist > 0 { -1 } else { 0 };
            Self::plant_tracer(term, park);
            self.planted = Some(park);
        }
        now
    }
}

/// One PTY chunk's arrival stamps for the timestamps gutter: every row the
/// cursor advanced past gets the chunk's arrival time, keyed on the scroll
/// clock (`clock reading + cursor grid line` = a stable content id) — NOT
/// on `history_size`, which treated any upward cursor motion as a
/// scrollback wipe (a `docker pull` progress redraw erased every stamp, an
/// alt-screen round trip erased them and then re-stamped the whole
/// scrollback with the exit time) and which saturates once the ring fills,
/// freezing ids to screen positions. Alternate-screen output stamps
/// nothing: it isn't scrollback rows, and the gutter is normal-screen only.
/// Destructive-clear detector over the raw PTY byte stream: ED 2
/// (`\x1b[2J`, erases the live screen), ED 3 (`\x1b[3J`, erases the
/// scrollback) and RIS (`\x1bc`, both). Keyed on the bytes rather than on
/// `history_size` movement, which is 0 → 0 when nothing has scrolled into
/// history yet. The scan is POSITIONAL: it tracks alt-screen entry/exit
/// (DECSET/DECRST 47/1047/1049) through the stream, so a wipe counts only
/// where it landed — `clear && vim` wipes the primary screen even though
/// the same chunk ends inside the alt screen. An unterminated trailing
/// sequence (bounded) carries to the next read; consumed bytes are never
/// rescanned, which keeps the alt tracking single-shot.
#[derive(Default)]
struct WipeSniffer {
    carry: Vec<u8>,
}

/// Longest carried partial sequence; a real DECSET list or ED is far
/// shorter, and the bound stops a hostile unterminated CSI from growing.
const WIPE_CARRY_MAX: usize = 64;

impl WipeSniffer {
    /// Scan one chunk. `alt` is the terminal's alt-screen state where the
    /// chunk STARTS. Returns (primary screen wiped, scrollback wiped).
    fn scan(&mut self, chunk: &[u8], mut alt: bool) -> (bool, bool) {
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(chunk);
        let mut screen = false;
        let mut hist = false;
        let mut i = 0;
        while i < data.len() {
            if data[i] != 0x1b {
                i += 1;
                continue;
            }
            let Some(&next) = data.get(i + 1) else {
                self.carry = data.split_off(i);
                return (screen, hist);
            };
            match next {
                // RIS: full reset — everything is gone, back on primary.
                b'c' => {
                    screen = true;
                    hist = true;
                    alt = false;
                    i += 2;
                }
                b'[' => {
                    let mut j = i + 2;
                    while j < data.len()
                        && !(0x40..=0x7e).contains(&data[j])
                        && !matches!(data[j], 0x18 | 0x1a | 0x1b)
                    {
                        j += 1;
                    }
                    if j >= data.len() {
                        if data.len() - i <= WIPE_CARRY_MAX {
                            self.carry = data.split_off(i);
                        }
                        return (screen, hist);
                    }
                    // CAN/SUB cancel the sequence and an ESC inside an
                    // incomplete CSI begins a NEW escape (VT100) — none may
                    // be swallowed as a parameter, or whatever follows a
                    // cancelled CSI goes unseen.
                    if matches!(data[j], 0x18 | 0x1a | 0x1b) {
                        i = if data[j] == 0x1b { j } else { j + 1 };
                        continue;
                    }
                    let params = &data[i + 2..j];
                    match data[j] {
                        b'J' if params == b"2" && !alt => screen = true,
                        b'J' if params == b"3" && !alt => hist = true,
                        fin @ (b'h' | b'l') if params.first() == Some(&b'?') => {
                            for p in params[1..].split(|&c| c == b';') {
                                if p == b"47" || p == b"1047" || p == b"1049" {
                                    alt = fin == b'h';
                                }
                            }
                        }
                        _ => {}
                    }
                    i = j + 1;
                }
                _ => i += 2,
            }
        }
        (screen, hist)
    }
}

fn stamp_chunk(
    term: &mut Term<VoidListener>,
    clock: &mut ScrollClock,
    lt: &mut std::collections::BTreeMap<i64, u64>,
    prev_id: &mut i64,
    now_ms: u64,
) {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return;
    }
    let now = clock.tick(term);
    let cur_id = now + i64::from(term.grid().cursor.point.line.0);
    // Stamp from the PREVIOUS cursor row inclusive (last touch wins: the
    // row content just landed on is re-stamped as it fills). A redraw that
    // moved the cursor UP re-arrives at existing rows: stamp only the
    // cursor row, never wipe.
    let from = (*prev_id).min(cur_id);
    for a in from..=cur_id {
        lt.insert(a, now_ms);
    }
    *prev_id = cur_id;
    // Bounded by the largest configurable scrollback plus a screen;
    // oldest stamps go first.
    while lt.len() > 210_000 {
        lt.pop_first();
    }
}

/// One user note pinned to a span of terminal output (iTerm2's
/// annotations), anchored like a mark so it rides the scrollback.
///
/// This doc had drifted onto `ScrollClock`, 200 lines up, when a `#[derive]`
/// was inserted beneath it. The derive originated here and is left where it
/// has effectively lived: neither struct uses `Clone` or `Debug` today (the
/// `clock.clone()` in the reader is `Arc::clone`), so moving it back would
/// only relocate dead code.
struct PaneAnnotation {
    /// Grid line the span sat on when recorded, paired with the scroll
    /// clock reading at that instant. Translated with clock movement — NOT
    /// `history_size` growth, which saturates once the scrollback fills and
    /// froze annotations in exactly the long-lived panes they target.
    line_rec: i32,
    clock_rec: i64,
    start: u16,
    len: u16,
    text: String,
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
    /// Epoch millis of the last PTY output byte, stamped by the reader (#344):
    /// "no output for N seconds" is half of what tells an agent's waiting
    /// from its working.
    last_output_ms: Arc<std::sync::atomic::AtomicU64>,
    /// The coding agent seated in this pane, when the foreground process is
    /// one (#344), carried between samples so a transition can be told.
    agent: Option<crate::agents::AgentLane>,
    /// Tracks whether the inner program has enabled DECSET 2004 (bracketed
    /// paste). Sniffed off the byte stream; not all parsers expose it.
    bracketed_paste_enabled: Arc<AtomicBool>,
    /// Listening loopback ports the reader thread scraped out of the output
    /// stream (`http://localhost:PORT` banners, `listening on :PORT` lines).
    /// The app drains this each tick to feed the PORTS panel and the toast.
    port_rx: std::sync::mpsc::Receiver<crate::port_detect::PortHit>,
    master: Box<dyn MasterPty + Send>,
    /// Shared between this struct's user-input path (`write_input`,
    /// `paste_*`, `cd_into`) and the background responder thread that
    /// ships alacritty's reply bytes (`PtyWrite`, `TextAreaSizeRequest`)
    /// back to the shell's stdin. portable-pty rejects a second
    /// `take_writer` call, so there is exactly one underlying writer
    /// and both paths funnel through this mutex.
    writer: Arc<std::sync::Mutex<Box<dyn Write + Send>>>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    /// The PTY reader thread's handle, joined in `Drop` after the child is
    /// killed so a dropped terminal never leaves a live shell + blocked
    /// reader behind. Across the test suite that leak piled up into resource
    /// pressure that broke timing-sensitive tests; production drops (closing
    /// a pane) leaked the same way.
    reader_thread: Option<std::thread::JoinHandle<()>>,
    /// Write end of the reader thread's shutdown pipe; `Drop` closes it,
    /// raising POLLHUP on the read end the reader polls alongside the pty.
    /// Without it the join above can hang forever: on Linux a background job
    /// keeps the pty slave open after the shell dies, so the reader's `read`
    /// never returns EOF (macOS revokes the tty instead).
    reader_shutdown: Option<std::io::PipeWriter>,
    /// The interactive shell's pid, captured at spawn. A shell at its
    /// prompt is its own foreground process-group leader, so this is the
    /// value `foreground_is_shell` compares `tcgetpgrp(master)` against.
    shell_pid: Option<i32>,
    /// Process-wide stable identity for this pane, assigned at spawn. Vec
    /// positions shift on close/insert/reorder; anything keying long-lived
    /// state to a pane (the build-diagnostic ownership, #119) uses this.
    uid: u64,
    /// Whether anything has ever been written toward the child's stdin
    /// (keystrokes, pastes, seeds, mouse reports). While false, no
    /// foreground application can have been launched in this pane, so a
    /// foreground group that differs from the shell can only be the
    /// shell's own rc startup — the state `cwd_seed_is_safe` treats as
    /// still seedable (#94).
    input_seen: bool,
    /// True for a `new_running` pane: the child is a launched program
    /// (a task, run-active-file, a debug attach), not an interactive
    /// shell. Such a pane is doing work the user asked for even though
    /// no input byte was ever written to it, so `is_pristine` must
    /// never mistake it for a replaceable startup default.
    run_pane: bool,
    /// Latches true the first time a non-blank manual name is set and
    /// never clears: a pane the user named is user state even after the
    /// name is cleared back, so `is_pristine` reads this latch rather
    /// than the live `manual_name` (which a blank rename empties).
    manual_name_seen: bool,
    cols: u16,
    rows: u16,
    /// Shared with the `VoidListener` so `TextAreaSizeRequest` callbacks
    /// see the live geometry without us having to thread a fresh
    /// listener instance through every `resize` call.
    size_shared: Arc<std::sync::Mutex<(u16, u16)>>,
    pub focused: bool,
    /// While broadcast input is on, an excluded pane stops receiving the
    /// mirrored keystrokes (it still gets its own input when focused).
    /// Toggled per pane with Cmd+K Shift+I; session-scoped.
    pub broadcast_excluded: bool,
    /// When focused, draw the orange→green gradient border (Black theme)
    /// instead of the solid blue one. Set by the app's focus/theme sync.
    pub focus_gradient: bool,
    /// Active color theme; the pane's chrome (borders, pills, gauges) routes
    /// through `Theme::ui` so it follows light themes. Set by the same sync.
    pub theme: crate::theme::Theme,
    pub last_area: Rect,
    pub last_inner: Rect,
    selection: Option<Selection>,
    /// Copy-mode cursor cell (absolute grid line, grid column): painted as
    /// a green modal block over the glyph so keyboard selection has a
    /// visible caret. None whenever copy mode is off.
    copy_cursor: Option<(i32, u16)>,
    /// Scroll-clock reading (see `clock_now`) when the selection /
    /// copy-cursor coordinates were recorded. Grid lines are relative to
    /// the buffer bottom, so every row the pane scrolls afterwards shifts
    /// their content up by one; subtracting the clock movement since this
    /// stamp keeps them glued to their text while output streams.
    sel_scrolled: i64,
    /// The pane's monotonic scroll clock, shared with the PTY reader
    /// thread: the render side re-anchors selections/annotations with it,
    /// the reader keys arrival timestamps on it (both under the term lock,
    /// then this mutex — always in that order).
    clock: Arc<std::sync::Mutex<ScrollClock>>,
    /// Content anchor for a selection made on the alternate screen, where
    /// apps (Claude Code, vim) never scroll the grid — they repaint it in
    /// place, so no scroll clock can see the text move. `None` while the
    /// selection lives on the primary screen.
    alt_sel: Option<AltSelAnchor>,
    /// A mouse drag-selection is in progress (button still held). While
    /// dragging, the selection follows the pointer over whatever the grid
    /// shows — it must never hide itself because the content anchor lost
    /// its rows (the drag may have started on a blank row or on an app's
    /// animated status line). The definitive anchor is captured on
    /// release (`end_drag`).
    drag_selecting: bool,
    /// User-given pane name (via rename), overriding the auto label. `None`
    /// until the user renames the pane.
    manual_name: Option<String>,
    /// Live foreground-process label (`zsh`, `vim`, `node`…), refreshed off the
    /// event loop on a cadence. Empty until the first refresh resolves.
    auto_label: String,
    /// Terminal find state: the needle currently highlighted across the grid
    /// and the options it matches under. `None` when no find bar is open.
    /// Set by the app's terminal find bar; the render loop paints every
    /// occurrence on each visible row.
    search_needle: Option<String>,
    search_opts: crate::widgets::search::SearchOpts,
    /// The active match painted in the brighter accent, versus the muted
    /// highlight on every other occurrence (VS Code's current-vs-other match
    /// colours). Stored as `(clock_rec, line_rec, col, len)` — the line is
    /// anchored on the scroll clock like annotations and hints, so streaming
    /// output moves the bright cell with its text instead of leaving it
    /// parked on a viewport row.
    current_match: Option<(i64, i32, usize, usize)>,
    /// Latched by the event listener when the child rings BEL; drained by
    /// [`Self::take_bell`].
    bell: Arc<AtomicBool>,
    /// OSC 133 semantic prompt marks recorded by the reader thread, in
    /// arrival order. Positions are stored as `(grid line, history size)`
    /// at record time; [`Self::command_marks`] translates to current grid
    /// lines (content scrolls into history as output arrives, so a stored
    /// line drifts by exactly the history growth since recording).
    marks: Arc<std::sync::Mutex<Vec<StoredMark>>>,
    /// Latest OSC 7 cwd report from the shell, when integration is active.
    osc7_cwd: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    /// Latest OSC 7 reporting host (SSH inside the pane moves it to the
    /// remote's hostname); feeds the per-host accent rules.
    osc7_host: Arc<std::sync::Mutex<Option<String>>>,
    /// Host-accent dressing resolved by the app against the prefs rules:
    /// border / pill color and the watermark badge.
    pub accent: Option<(u8, u8, u8)>,
    pub accent_badge: Option<String>,
    /// OSC 9 notification payloads awaiting the app's drain.
    notifications: Arc<std::sync::Mutex<Vec<String>>>,
    /// Latest OSC 9;4 progress report `(state, percent)`: 1 normal,
    /// 2 error, 3 indeterminate, 4 warning. `None` when idle (state 0,
    /// command end, or never reported). Drives the bottom-border gauge.
    progress: Arc<std::sync::Mutex<Option<(u8, u8)>>>,
    /// Arrival time (epoch millis) per grid row, keyed by the row's stable
    /// absolute id (`current line + history size`, constant as content
    /// scrolls). Stamped by the reader thread at chunk granularity; cleared
    /// wholesale when the scrollback is wiped (the ids restart).
    line_times: Arc<std::sync::Mutex<std::collections::BTreeMap<i64, u64>>>,
    /// Paint the right-edge HH:MM:SS gutter (the "Terminal: Toggle
    /// Timestamps" palette command flips this on every pane).
    pub show_timestamps: bool,
    /// Paint redact-trigger matches as themselves for a moment ("Terminal:
    /// Reveal Redacted Secrets"); the app sets it on every pane per frame.
    pub reveal_redactions: bool,
    /// How many redact-trigger spans the last paint masked, for the
    /// status-bar chip. Zero while revealing or when nothing matched.
    pub redacted_on_screen: usize,
    /// User notes pinned to output spans (Cmd+K N on a selection).
    /// Session-scoped, like the scrollback they describe.
    annotations: Vec<PaneAnnotation>,
    /// Finished commands (exit, duration) from the reader thread, awaiting
    /// the app's drain for long-command notifications.
    finished_rx: std::sync::mpsc::Receiver<FinishedCommand>,
    /// Quick-select hint spans pushed down by the app while hint mode is
    /// active, paired with the scroll-clock reading their lines were
    /// captured at: the render loop re-bases each span every frame so the
    /// labels follow their matches through streaming output instead of
    /// squatting on fixed viewport rows. `None` when quick-select is off.
    hints: Option<(i64, Vec<HintSpan>)>,
    /// The user's trigger set, shared with the reader thread (which scans
    /// completed lines for notify/bell firings) and read by the render loop
    /// (which paints highlight-trigger matches). The inner Arc is swapped by
    /// [`Self::set_triggers`] on startup and config reload.
    triggers: Arc<std::sync::Mutex<std::sync::Arc<crate::triggers::TriggerSet>>>,
    /// Notify/bell trigger firings from the reader thread, awaiting the
    /// app's drain into the status bar.
    trigger_rx: std::sync::mpsc::Receiver<crate::triggers::TriggerHit>,
    /// Background problem matchers (#252): the global watch-capable set
    /// (from matchers.json, swapped whole like `triggers`) plus this pane's
    /// task-assigned matcher, both read by the reader thread's watch
    /// engine per chunk.
    watch_set: Arc<std::sync::Mutex<std::sync::Arc<crate::problem_matchers::WatchSet>>>,
    pane_watch:
        Arc<std::sync::Mutex<Option<std::sync::Arc<crate::problem_matchers::CompiledMatcher>>>>,
    /// Published watch batches (cwd at publish time + diagnostics),
    /// awaiting the app's drain into PROBLEMS.
    #[allow(clippy::type_complexity)]
    watch_rx: std::sync::mpsc::Receiver<(
        Option<std::path::PathBuf>,
        Vec<crate::build_matchers::BuildDiag>,
    )>,
    /// The theme's 16 ANSI colors; Named and Indexed 0-15 cell colors render
    /// through it so panes look the same on every host terminal (VS Code
    /// owns its terminal palette the same way). Synced by the app's theme
    /// pass via [`Self::set_palette`].
    palette: [(u8, u8, u8); 16],
    /// Inline images captured from the pane's output (iTerm2 OSC 1337
    /// `inline=1`, the imgcat protocol), anchored like marks. Capped at
    /// [`IMAGES_MAX`]; the app overlays the newest visible one.
    images: Arc<std::sync::Mutex<Vec<StoredImage>>>,
    /// The last few minutes of this pane's output, for Session: Rewind
    /// (#357). Written by the reader thread as chunks arrive; bounded by
    /// bytes, so a `yes`-style flood evicts rather than grows.
    rewind: Arc<std::sync::Mutex<crate::rewind::RewindBuffer>>,
    /// Test-only capture of every byte written toward the child's stdin.
    #[cfg(test)]
    written_for_test: Arc<std::sync::Mutex<Vec<u8>>>,
}

/// One captured inline image at its recording-time anchor, keyed on the
/// scroll clock like annotations (current line =
/// `line_rec - (clock_now - clock_rec)`), NOT on `history_size`, whose
/// saturation froze the picture onto a viewport row in long-lived panes.
struct StoredImage {
    seq: u64,
    data: std::sync::Arc<Vec<u8>>,
    line_rec: i32,
    clock_rec: i64,
}

/// A pane inline image surfaced to the app: its per-pane id, the raw image
/// bytes, and the anchor's CURRENT grid line (negative = scrolled into
/// history).
#[derive(Clone)]
pub struct PaneImage {
    pub seq: u64,
    pub data: std::sync::Arc<Vec<u8>>,
    pub line: i32,
}

/// Most inline images kept per pane; older ones scroll away like text.
const IMAGES_MAX: usize = 4;

/// One quick-select hint for the render loop: the match span on absolute
/// grid line `line` (char indices into the spacer-skipped row text; the
/// colmap translates back to grid columns), the label overlaid at its start,
/// and how many label chars the user has already typed (those are consumed,
/// only the remainder renders).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintSpan {
    pub line: i32,
    pub start: usize,
    pub len: usize,
    pub label: String,
    pub typed: usize,
}

/// Source of [`StoredMark::id`], shared by every pane.
///
/// Process-wide rather than per-pane so an id is unambiguous wherever it
/// travels; `u64` so it cannot realistically wrap (at one mark per
/// microsecond it lasts half a million years), which matters because the
/// whole value of the id is that a stale one never matches a live command.
static NEXT_MARK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Take the next mark id.
///
/// `Relaxed` is sufficient and is not a shortcut. Uniqueness needs no
/// ordering at all — `fetch_add` on a single atomic is totally ordered on
/// itself, so no two callers can take the same value. And the id's
/// PUBLICATION is ordered by something else entirely: it reaches a reader
/// only inside a `StoredMark` behind the `marks` mutex, whose unlock/lock
/// pair supplies the happens-before. There is no second variable whose
/// visibility this atomic would have to fence, which is the only thing a
/// stronger ordering would buy. Do not "upgrade" it to `SeqCst`.
fn next_mark_id() -> u64 {
    NEXT_MARK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// One OSC 133 mark at its recording-time anchor, keyed on the scroll
/// clock like annotations and images (current position =
/// `line_rec - (clock_now - clock_rec)`), NOT on `history_size`, whose
/// saturation froze surviving marks onto viewport rows and whose zeroing
/// on a scrollback wipe inverted the math outright. Marks past the
/// scrollback floor are GC'd in `marks_snapshot`.
struct StoredMark {
    /// Monotonic identity, assigned once when the mark is recorded and never
    /// renumbered (#440).
    ///
    /// Positions are not identity here. Three paths remove marks —
    /// `marks_snapshot` GCs those past the scrollback floor, the reader drops
    /// the oldest at `MARKS_MAX`, and a destructive screen wipe clears the
    /// list outright — and the first two shift every index below them down.
    /// A caller holding "index 3" is holding a slot, not a command, and after
    /// an eviction that slot silently names a different one. An id turns that into "command
    /// 41": if 41 is gone the caller learns it is gone instead of reading 42.
    ///
    /// The counter is never reset, including by the wipe, so an id is not
    /// reused after a pane is cleared either.
    id: u64,
    kind: crate::shell_integration::OscEvent,
    line_rec: i32,
    clock_rec: i64,
    /// Cursor column when the mark landed (`PromptEnd`'s column is where the
    /// typed command starts on its prompt line).
    col_rec: usize,
    /// For `CommandEnd` marks: how long the command ran (CommandStart →
    /// CommandEnd, measured in the reader thread). `None` elsewhere.
    dur: Option<std::time::Duration>,
}

/// A finished command as reported by the reader thread at its `133;D`
/// mark: exit + duration (for the long-command notice) plus the typed
/// command text (extracted from the B→C mark span while the term lock is
/// held) and the pane's OSC 7 cwd at finish time — everything the durable
/// command history records.
#[derive(Clone, Debug)]
pub struct FinishedCommand {
    pub exit: Option<i32>,
    pub dur: std::time::Duration,
    pub cmd: String,
    pub cwd: Option<std::path::PathBuf>,
    /// The OSC 7 reporting hostname AT COMPLETION time. Captured with the
    /// 133;D mark, not read later at drain: an in-pane `ssh`/`exit` between
    /// the two would attribute the command to the wrong machine.
    pub host: Option<String>,
    /// The command's output (the 133;C→133;D span), escape-free, captured at
    /// the D mark while the rows are guaranteed still present. Capped to the
    /// LAST [`FINISHED_OUTPUT_CAP_LINES`] lines — the tail keeps a compiler's
    /// error summary; a build long enough to overflow the cap loses its
    /// earliest lines. Feeds the build problem matchers (#119).
    pub output: String,
}

/// Tail cap on [`FinishedCommand::output`].
pub const FINISHED_OUTPUT_CAP_LINES: usize = 5000;

/// A finished command derived from the OSC 133 marks: the grid line of the
/// prompt it was typed at (current coords, negative = scrollback), its exit
/// code (`None` when the shell omitted it), how long it ran, where its typed
/// text starts (`PromptEnd` line + column), and its output span
/// (`CommandStart` line up to but excluding the `CommandEnd` line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandDecoration {
    /// Identity of the command, stable across eviction (#440).
    ///
    /// Taken from the `CommandStart` mark, because that is the mark every
    /// finished command has exactly one of — the prompt marks before it are
    /// optional in the pairing below, so keying on one of those would leave
    /// some decorations without an id.
    ///
    /// **Only the identity is stable; the presentation fields are not.** The
    /// id, `exit` and the output span come from the `CommandStart`/`CommandEnd`
    /// pair, so they hold for as long as the decoration exists at all. But
    /// `line` and `input` come from the PROMPT marks, which sit on earlier
    /// grid rows and so cross the scrollback floor FIRST: the GC in
    /// `marks_snapshot` retains per mark, not per command, so a command can
    /// outlive its own prompt. When that happens `line` falls back to
    /// `output_start` and `input` becomes `None`, while the id keeps naming
    /// the same command throughout.
    ///
    /// So a consumer keyed on the id gets a durable answer to "which
    /// command", and a best-effort answer to "where on screen was it typed".
    /// That is the right split for the callers there are — the gutter wants
    /// a row and can use the fallback, and a re-run wants the text and must
    /// handle its absence — but it is a real limit, not an oversight, and
    /// worth knowing before keying anything else on the pair.
    pub id: u64,
    pub line: i32,
    pub exit: Option<i32>,
    pub duration: Option<std::time::Duration>,
    pub input: Option<(i32, usize)>,
    pub output_start: i32,
    pub output_end: i32,
}

/// One mark as a caller sees it: its identity, its CURRENT grid line, and
/// the fields the pairing needs.
///
/// A struct rather than the tuple this used to be. The tuple had reached four
/// positional fields and adding the id would have made five, at which point
/// `m.3` at a call site says nothing about what it holds — and the whole
/// change here is about not confusing a position with an identity.
#[derive(Clone, Debug)]
pub struct MarkView {
    /// Stable identity from [`StoredMark::id`] (#440).
    pub id: u64,
    pub kind: crate::shell_integration::OscEvent,
    /// Current grid line; negative means scrollback.
    pub line: i32,
    /// Cursor column when the mark landed.
    pub col: usize,
    /// For `CommandEnd`: how long the command ran.
    pub dur: Option<std::time::Duration>,
}

/// Pair the mark stream (oldest first, each with its current grid line and,
/// for `CommandEnd`, the measured duration) into one record per *finished*
/// command: the last `PromptStart` line before a `CommandStart` names the
/// row, the following `CommandEnd` supplies exit + duration. A `CommandEnd`
/// with no pending `CommandStart` is dropped — that's how a second
/// integration layer's duplicate marks (Ghostty's hooks chained behind
/// croft's) stay out of the record.
pub fn pair_decorations(marks: &[MarkView]) -> Vec<CommandDecoration> {
    use crate::shell_integration::OscEvent as E;
    let mut out = Vec::new();
    let mut prompt: Option<i32> = None;
    let mut input: Option<(i32, usize)> = None;
    // The id of the `CommandStart` currently open, carried alongside its
    // line so the decoration is keyed on the mark that actually defines the
    // command rather than on whichever prompt mark happened to precede it.
    let mut started: Option<(u64, i32)> = None;
    for m in marks {
        match &m.kind {
            E::PromptStart => {
                prompt = Some(m.line);
                input = None;
            }
            E::PromptEnd => input = Some((m.line, m.col)),
            E::CommandStart => started = Some((m.id, m.line)),
            E::CommandEnd(exit) => {
                if let Some((id, output_start)) = started.take() {
                    out.push(CommandDecoration {
                        id,
                        line: prompt.unwrap_or(output_start),
                        exit: *exit,
                        duration: m.dur,
                        input,
                        output_start,
                        output_end: m.line,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Compact human form of a command duration: "480ms", "3.4s", "2m 05s".
pub fn human_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{}ms", d.as_millis())
    } else if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m {:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

/// Cap on retained marks per pane (a mark per prompt/command boundary;
/// thousands would mean a very long session — drop the oldest).
const MARKS_MAX: usize = 2000;

/// The nearest prompt line strictly above (`forward == false`) or below
/// (`forward == true`) the viewport-top grid line `current_top`. Feeds
/// Cmd+Up / Cmd+Down command navigation.
pub fn pick_prompt_jump(prompt_lines: &[i32], current_top: i32, forward: bool) -> Option<i32> {
    if forward {
        prompt_lines
            .iter()
            .copied()
            .filter(|&l| l > current_top)
            .min()
    } else {
        prompt_lines
            .iter()
            .copied()
            .filter(|&l| l < current_top)
            .max()
    }
}

/// Inject croft's shell-integration environment for supported shells, and
/// return any argv to prepend before the login flag.
/// zsh: point `ZDOTDIR` at croft's shim. bash: the kitty/Ghostty trick —
/// `--posix` plus `ENV` pointing at croft's shim (posix-mode bash reads
/// only `$ENV`; the shim backs out and replays the real startup files).
/// fish: prepend a `vendor_conf.d` data dir to `XDG_DATA_DIRS`; the script
/// defers to fish 4's native marks. Every shim sources the user's real
/// dotfiles unchanged and emits OSC 133 prompt marks + the OSC 7 cwd
/// report. Opt out with `CROFT_SHELL_INTEGRATION=0`. Failures are
/// non-fatal — the shell still spawns, just without marks.
fn apply_shell_integration_env(
    cmd: &mut CommandBuilder,
    shell_path: &str,
    config_dir: &std::path::Path,
) -> Vec<String> {
    if std::env::var("CROFT_SHELL_INTEGRATION").as_deref() == Ok("0") {
        return Vec::new();
    }
    let base = std::path::Path::new(shell_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    match base {
        "zsh" => {
            let Ok(shim) = crate::shell_integration::ensure_zsh_shim(config_dir) else {
                return Vec::new();
            };
            // An inherited ZDOTDIR pointing at croft's own shim (croft
            // launched from a croft pane) is poisoned — the user's dotfiles
            // live in HOME.
            let inherited = std::env::var_os("ZDOTDIR").map(std::path::PathBuf::from);
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            if home.as_os_str().is_empty() {
                return Vec::new();
            }
            let user_zdotdir = crate::shell_integration::resolve_user_zdotdir(
                inherited.as_deref(),
                &config_dir.join("shell-integration"),
                &home,
            );
            cmd.env("CROFT_USER_ZDOTDIR", user_zdotdir);
            cmd.env("ZDOTDIR", &shim);
            Vec::new()
        }
        "bash" => {
            if !bash_env_injection_supported(shell_path) {
                return Vec::new();
            }
            let Ok(shim) = crate::shell_integration::ensure_bash_shim(config_dir) else {
                return Vec::new();
            };
            // Preserve a user $ENV (rare for bash) for the shim to restore.
            if let Ok(env) = std::env::var("ENV") {
                cmd.env("CROFT_BASH_ENV", env);
            }
            cmd.env("ENV", &shim);
            cmd.env("CROFT_BASH_INJECT", "1");
            // Posix mode defaults HISTFILE to ~/.sh_history; pre-seed the
            // bash default and let the shim unexport it.
            if std::env::var_os("HISTFILE").is_none()
                && let Some(home) = std::env::var_os("HOME")
            {
                cmd.env(
                    "HISTFILE",
                    std::path::Path::new(&home).join(".bash_history"),
                );
                cmd.env("CROFT_BASH_UNEXPORT_HISTFILE", "1");
            }
            vec!["--posix".to_string()]
        }
        "fish" => {
            let Ok(dir) = crate::shell_integration::ensure_fish_integration(config_dir) else {
                return Vec::new();
            };
            let inherited = std::env::var("XDG_DATA_DIRS").ok();
            cmd.env(
                "XDG_DATA_DIRS",
                crate::shell_integration::fish_xdg_data_dirs(&dir, inherited.as_deref()),
            );
            cmd.env("CROFT_FISH_XDG_DATA_DIR", &dir);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Whether this bash reads `$ENV` in posix mode, i.e. the injection works
/// at all. Verified empirically: macOS's system bash 3.2 never reads $ENV
/// under `--posix`, so injecting would strip its startup files for
/// nothing; Homebrew/Linux 5.x does. 4.4 is the ecosystem floor (kitty,
/// Ghostty). One short probe subprocess per pane spawn — pane creation
/// already forks a shell, so this is noise.
fn bash_env_injection_supported(shell_path: &str) -> bool {
    let Ok(out) = std::process::Command::new(shell_path)
        .args(["-c", "echo \"${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}\""])
        .output()
    else {
        return false;
    };
    parse_bash_version_supported(&String::from_utf8_lossy(&out.stdout))
}

/// `major.minor` >= 4.4, tolerant of anything a non-bash prints.
fn parse_bash_version_supported(s: &str) -> bool {
    let mut it = s.trim().split('.');
    let (Some(maj), Some(min)) = (
        it.next().and_then(|v| v.parse::<u32>().ok()),
        it.next().and_then(|v| v.parse::<u32>().ok()),
    ) else {
        return false;
    };
    maj > 4 || (maj == 4 && min >= 4)
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

/// Parse `/etc/shells` into `(path, basename)` terminal profiles: skip comments
/// and blanks, keep absolute paths only, and dedupe by basename (first wins, so
/// `/bin/bash` shadows `/usr/local/bin/bash`). The basename is the label shown.
pub fn parse_shells(contents: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('/') {
            continue;
        }
        let base = std::path::Path::new(line)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(line)
            .to_string();
        if out.iter().any(|(_, b)| *b == base) {
            continue;
        }
        out.push((line.to_string(), base));
    }
    out
}

/// Resolve a pid to its command name (basename), cross-platform, via `sysinfo`
/// (no subprocess). Used to label a terminal pane with its foreground process.
/// Refreshes only the one pid, so it's cheap, but still runs off the event loop.
pub fn process_name(pid: i32) -> Option<String> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    if pid <= 0 {
        return None;
    }
    let p = Pid::from(pid as usize);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[p]), true);
    sys.process(p)
        .map(|pr| pr.name().to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
}

/// The argv of a running process, or `None` when the platform will not say.
///
/// Separate from [`process_name`] because the two answer different questions
/// and cost differently: a name is what the label pill needs on every tick,
/// while argv is only wanted when the name says the process is worth asking
/// about (#364 wants it for `ssh` and nothing else).
pub fn process_cmdline(pid: i32) -> Option<Vec<String>> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    if pid <= 0 {
        return None;
    }
    let p = Pid::from(pid as usize);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[p]), true);
    let cmd: Vec<String> = sys
        .process(p)?
        .cmd()
        .iter()
        .map(|s| s.to_string_lossy().to_string())
        .collect();
    (!cmd.is_empty()).then_some(cmd)
}

/// Live cwd of a running process by PID, or None when the platform doesn't
/// expose one. Used by `split_terminal` so a new pane lands wherever the
/// user has `cd`'d the active shell, and by [`PtyTerminal::local_shell_cwd`]
/// as the ground truth an OSC 7 claim must match to be trusted.
#[cfg(target_os = "linux")]
pub fn cwd_of_pid(pid: u32) -> Option<std::path::PathBuf> {
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
pub fn cwd_of_pid(pid: u32) -> Option<std::path::PathBuf> {
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
    Some(std::path::PathBuf::from(OsString::from_vec(
        path[..len].to_vec(),
    )))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cwd_of_pid(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

/// Whether this platform can report a process's cwd AT ALL.
///
/// The distinction matters wherever a guard reads `cwd_of_pid`: on Android
/// — a separate `target_os` from `linux`, so it binds the stub above — the
/// answer is a permanent `None` rather than a per-pane one. A guard that
/// treats "cannot observe" as "observed something wrong" does not become
/// stricter there, it becomes unsatisfiable, and whatever it guards is
/// lost entirely for those users (#430).
pub const fn cwd_is_observable() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
}

/// A saved transcript rendered as bytes safe to feed a fresh parser.
///
/// Control characters are stripped rather than escaped: a transcript comes off
/// disk, and a grid should never have contained an escape in the first place,
/// so anything control-shaped here is either corruption or someone's idea of a
/// joke. TAB is the exception — it is legitimate output, and deleting it
/// silently collapses tab-aligned columns — so it becomes a space, which is
/// what the grid would have shown anyway.
fn transcript_bytes(lines: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for l in lines {
        let cleaned: String = l
            .chars()
            .map(|c| if c == '\t' { ' ' } else { c })
            .filter(|c| !c.is_control())
            .collect();
        bytes.extend(cleaned.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    bytes
}

/// The label to show for a pane: a manual name wins, else the live foreground
/// process name.
pub fn pick_pane_label<'a>(manual: Option<&'a str>, auto: &'a str) -> &'a str {
    manual.unwrap_or(auto)
}

/// Croft's own environment for a pane's shell: the terminal identity every
/// pane needs, plus the `croft view` socket when one is bound (#362).
///
/// `view_sock` is passed IN rather than read from a global here, so this is a
/// pure function a test can assert against without touching process-wide
/// state; the spawn funnel supplies it from [`crate::view_ipc::SOCK_PATH`].
///
/// The socket travels this way rather than through the process environment
/// because it cannot travel that way at all. portable-pty snapshots
/// `std::env::vars_os()` inside `CommandBuilder::new` and then `env_clear()`s
/// before applying only that snapshot, so a `set_var` performed after a pane
/// was constructed never reaches its shell. The listener is bound while
/// `App::new` runs, which is after the FIRST pane exists, so publishing by
/// environment left `croft view` broken in the one pane most users type in
/// and working in every pane opened later: breakage that reads as
/// intermittent rather than as a bug.
///
/// A `OnceLock` read at spawn time has no ordering hazard to get wrong, and
/// no `unsafe` either: `std::env::set_var` is unsafe in edition 2024 precisely
/// because croft has threads running by then, which is the condition
/// `src/gui_path.rs` and `src/session.rs` both document for their own calls.
pub(crate) fn apply_pane_env(cmd: &mut CommandBuilder, view_sock: Option<&std::path::Path>) {
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    match view_sock {
        Some(path) => cmd.env(crate::view_ipc::SOCK_ENV, path),
        // CLEARED, not merely not-added. `CommandBuilder::new` seeds itself
        // from `std::env::vars_os()`, so a croft launched from a croft pane
        // inherits the OUTER croft's socket. If this croft's own bind failed,
        // leaving that inherited value in place points its panes at the
        // parent: `croft view report.pdf` opens the file in the wrong window
        // and exits 0, which is worse than the honest refusal the field doc
        // promises ("panes then see no CROFT_VIEW_SOCK and the client says so
        // plainly").
        None => cmd.env_remove(crate::view_ipc::SOCK_ENV),
    }
}

impl PtyTerminal {
    pub fn new(cwd: &std::path::Path) -> Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        Self::new_shell(&shell, cwd)
    }

    /// Spawn a shell with `transcript` already painted into the grid, for a
    /// pane restored from a saved session (#249).
    ///
    /// The preamble is painted before the reader thread starts, not after the
    /// constructor returns. Replaying afterwards races the shell: both write
    /// the same `Term` behind the same lock, and a shell that reaches its
    /// first prompt before the replay wins, so the restored output lands
    /// below or through the prompt instead of above it.
    pub fn new_with_transcript(cwd: &std::path::Path, transcript: &[String]) -> Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let (program, args) = interactive_shell_invocation(&shell);
        let mut cmd = CommandBuilder::new(&program);
        let pre = apply_shell_integration_env(&mut cmd, &shell, &crate::prefs::config_dir());
        for a in pre.iter().chain(args.iter()) {
            cmd.arg(a);
        }
        cmd.cwd(cwd);
        Self::spawn_with_preamble(cmd, None, &transcript_bytes(transcript))
    }

    /// Spawn an interactive login session for a specific shell (a terminal
    /// profile), rather than `$SHELL`. The same login-flag handling as [`new`].
    pub fn new_shell(shell_path: &str, cwd: &std::path::Path) -> Result<Self> {
        let (program, args) = interactive_shell_invocation(shell_path);
        let mut cmd = CommandBuilder::new(&program);
        // Integration args go first: bash only recognises long options
        // (--posix) when they precede single-character ones (-l).
        let pre = apply_shell_integration_env(&mut cmd, shell_path, &crate::prefs::config_dir());
        for a in pre.iter().chain(args.iter()) {
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

    /// Every readable grid line as plain text — oldest scrollback first, then
    /// the live screen — paired with the absolute alacritty `Line` index of
    /// row 0. Terminal find searches these lines; `top + row` maps a hit's
    /// row back to its grid line so it can be scrolled into view and
    /// highlighted. Wide-char spacer cells are skipped (see
    /// [`row_text_and_cols`]), so match positions are char indices, not grid
    /// columns.
    pub fn grid_lines(&self) -> (Vec<String>, i32) {
        let term = self.term.lock();
        if term.columns() == 0 {
            return (Vec::new(), 0);
        }
        let top = term.grid().topmost_line().0;
        let bottom = term.screen_lines() as i32 - 1;
        let mut lines = Vec::new();
        let mut l = top;
        while l <= bottom {
            let (s, _cols) = row_text_and_cols(&term, l);
            lines.push(s.trim_end().to_string());
            l += 1;
        }
        (lines, top)
    }

    /// [`Self::grid_lines`] plus the scroll-clock reading, captured under
    /// ONE term lock. Terminal find stamps its match anchor with the clock;
    /// separate reads would let output land between them and mis-pair a
    /// line with a reading it wasn't captured at.
    pub fn grid_lines_and_clock(&self) -> (Vec<String>, i32, i64) {
        let term = self.term.lock();
        if term.columns() == 0 {
            return (Vec::new(), 0, self.clock_now(&term));
        }
        let top = term.grid().topmost_line().0;
        let bottom = term.screen_lines() as i32 - 1;
        let mut lines = Vec::new();
        let mut l = top;
        while l <= bottom {
            let (s, _cols) = row_text_and_cols(&term, l);
            lines.push(s.trim_end().to_string());
            l += 1;
        }
        (lines, top, self.clock_now(&term))
    }

    /// The OSC 8 hyperlink URI stored under viewport cell `(row, col)`, if
    /// any. Hyperlinked cells carry the URI invisibly; the app's
    /// Cmd/Ctrl+click handler checks this before the plain-text URL regex.
    pub fn hyperlink_at(&self, row: usize, col: usize) -> Option<String> {
        let term = self.term.lock();
        if col >= term.columns() {
            return None;
        }
        let line_idx = row as i32 - term.grid().display_offset() as i32;
        if line_idx < term.grid().topmost_line().0 || line_idx >= term.screen_lines() as i32 {
            return None;
        }
        term.grid()[Point::new(Line(line_idx), Column(col))]
            .hyperlink()
            .map(|h| h.uri().to_string())
    }

    /// Set (or clear) the find highlight. Every occurrence of `needle` is
    /// painted across the visible grid on the next render.
    pub fn set_search(&mut self, needle: Option<String>, opts: crate::widgets::search::SearchOpts) {
        self.search_needle = needle.filter(|s| !s.is_empty());
        self.search_opts = opts;
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Mark which occurrence is the active match `(abs_line, col, len)` so the
    /// render loop paints it in the brighter accent. `clock` is the
    /// scroll-clock reading the line was captured against (the
    /// [`Self::grid_lines_and_clock`] snapshot); the render re-bases per
    /// frame so the highlight follows its text through streaming output.
    pub fn set_current_match(&mut self, m: Option<(i32, usize, usize)>, clock: i64) {
        self.current_match = m.map(|(line, col, len)| (clock, line, col, len));
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Set (or clear) the quick-select hint overlay. The app owns hint-mode
    /// state and re-pushes the filtered set as the user types label chars;
    /// `clock` is the scroll-clock reading the span lines were captured at
    /// (from [`Self::visible_lines_and_clock`]), reused verbatim on every
    /// re-push so the render-time translation stays anchored to the open.
    pub fn set_hints(&mut self, hints: Option<Vec<HintSpan>>, clock: i64) {
        self.hints = hints.map(|h| (clock, h));
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// The visible viewport rows as spacer-skipped plain text, paired with
    /// the absolute grid line of the first row (`-display_offset`).
    /// Quick-select scans exactly what is on screen — labels the user cannot
    /// see cannot be typed.
    pub fn visible_lines(&self) -> (Vec<String>, i32) {
        let term = self.term.lock();
        if term.columns() == 0 {
            return (Vec::new(), 0);
        }
        let off = term.grid().display_offset() as i32;
        let rows = term.screen_lines() as i32;
        let mut lines = Vec::new();
        for l in -off..rows - off {
            let (s, _cols) = row_text_and_cols(&term, l);
            lines.push(s.trim_end().to_string());
        }
        (lines, -off)
    }

    /// [`Self::visible_lines`] plus per-row soft-wrap flags and the scroll
    /// clock, all from ONE term lock: coordinates and clock captured
    /// together so output landing between two separate reads can never
    /// mis-pair them (quick-select anchors its labels on this snapshot).
    /// `wraps[i]` says row `i` continues into row `i + 1` (WRAPLINE on its
    /// last cell); a wrapping row keeps its trailing spaces — they are real
    /// cells mid-line, and trimming them would glue the next row's first
    /// word onto this one when the rows are stitched for scanning (#64).
    pub fn visible_lines_and_clock(&mut self) -> (Vec<String>, Vec<bool>, i32, i64) {
        let mut term = self.term.lock();
        let clock = self.clock.lock().unwrap().tick(&mut term);
        if term.columns() == 0 {
            return (Vec::new(), Vec::new(), 0, clock);
        }
        let off = term.grid().display_offset() as i32;
        let rows = term.screen_lines() as i32;
        let cols = term.columns();
        let mut lines = Vec::new();
        let mut wraps = Vec::new();
        for l in -off..rows - off {
            let (s, _cols) = row_text_and_cols(&term, l);
            let wrapped = term.grid()[Point::new(Line(l), Column(cols - 1))]
                .flags
                .contains(Flags::WRAPLINE);
            lines.push(if wrapped { s } else { s.trim_end().to_string() });
            wraps.push(wrapped);
        }
        (lines, wraps, -off, clock)
    }

    /// Swap in a new trigger set. The app calls this for every pane on every
    /// drain tick (so a pane is covered no matter where it was created); the
    /// ptr-eq check makes the steady state a cheap no-op that never dirties
    /// the pane. The reader thread picks a new set up on its next chunk; the
    /// render loop on its next frame.
    pub fn set_triggers(&self, set: std::sync::Arc<crate::triggers::TriggerSet>) {
        let mut cur = self.triggers.lock().unwrap();
        if std::sync::Arc::ptr_eq(&cur, &set) {
            return;
        }
        *cur = set;
        drop(cur);
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Notify/bell trigger firings recorded by the reader thread since the
    /// last drain.
    pub fn drain_trigger_hits(&self) -> Vec<crate::triggers::TriggerHit> {
        self.trigger_rx.try_iter().collect()
    }

    /// Swap in the global background-matcher set (#252), same steady-state
    /// no-op contract as [`Self::set_triggers`].
    pub fn set_watch_set(&self, set: std::sync::Arc<crate::problem_matchers::WatchSet>) {
        let mut cur = self.watch_set.lock().unwrap();
        if std::sync::Arc::ptr_eq(&cur, &set) {
            return;
        }
        *cur = set;
    }

    /// Assign (or clear) the task-specific background matcher for this
    /// pane — the `problemMatcher` of the task running here.
    pub fn set_pane_watch(
        &self,
        matcher: Option<std::sync::Arc<crate::problem_matchers::CompiledMatcher>>,
    ) {
        *self.pane_watch.lock().unwrap() = matcher;
    }

    /// Watch batches published by the reader thread since the last drain:
    /// the pane cwd at publish time plus the cycle's diagnostics (possibly
    /// empty — an empty batch is what clears fixed errors).
    #[allow(clippy::type_complexity)]
    pub fn drain_watch_batches(
        &self,
    ) -> Vec<(
        Option<std::path::PathBuf>,
        Vec<crate::build_matchers::BuildDiag>,
    )> {
        self.watch_rx.try_iter().collect()
    }

    /// Swap the ANSI palette the render loop maps Named/Indexed 0-15 cell
    /// colors through (the theme sync calls this every pass; unchanged
    /// palettes are a no-op so the pane never dirties spuriously).
    pub fn set_palette(&mut self, palette: [(u8, u8, u8); 16]) {
        if self.palette != palette {
            self.palette = palette;
            self.pty_dirty.store(true, Ordering::Release);
        }
    }

    /// Inline images captured from the pane's output, oldest first, each
    /// with its anchor's CURRENT grid line (drift model identical to command
    /// marks). Images whose anchor scrolled past the scrollback floor are
    /// garbage-collected here.
    pub fn pane_images(&self) -> Vec<PaneImage> {
        let mut term = self.term.lock();
        let now = self.clock.lock().unwrap().tick(&mut term);
        let floor = term.grid().topmost_line().0;
        drop(term);
        let mut imgs = self.images.lock().unwrap();
        imgs.retain(|m| m.line_rec - (now - m.clock_rec) as i32 >= floor);
        imgs.iter()
            .map(|m| PaneImage {
                seq: m.seq,
                data: m.data.clone(),
                line: m.line_rec - (now - m.clock_rec) as i32,
            })
            .collect()
    }

    /// Whether the pane is in the alternate screen (a full-screen app owns
    /// the viewport; anchored overlays make no sense there).
    pub fn alt_screen(&self) -> bool {
        self.term.lock().mode().contains(TermMode::ALT_SCREEN)
    }

    /// Whether the child enabled application cursor keys (DECCKM, `\e[?1h`).
    /// Arrows and Home/End must then arrive as SS3 (`\eOA`…) — the form
    /// terminfo advertises, and the only one apps that bind keys from
    /// terminfo (Python's REPL, less) recognize.
    pub fn app_cursor_keys(&self) -> bool {
        self.term.lock().mode().contains(TermMode::APP_CURSOR)
    }

    /// The viewport's scroll offset into history (0 = live bottom). Public
    /// so the app can map an anchor's grid line to a viewport row.
    pub fn scroll_display_offset(&self) -> i32 {
        self.display_offset()
    }

    /// Scroll the viewport so absolute grid line `abs_line` sits near the
    /// middle of the pane, clamped to the scrollback range. No-op in
    /// alternate screen, where there is no scrollback to move through and the
    /// whole grid is already on screen.
    pub fn scroll_to_line(&mut self, abs_line: i32) {
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let rows = term.screen_lines() as i32;
        let max_off = (-term.grid().topmost_line().0).max(0);
        let cur = term.grid().display_offset() as i32;
        let desired = scroll_offset_for_line(rows, max_off, abs_line);
        let delta = desired - cur;
        if delta != 0 {
            term.scroll_display(Scroll::Delta(delta));
        }
        drop(term);
        self.pty_dirty.store(true, Ordering::Release);
    }

    fn spawn_with(cmd: CommandBuilder, run_label: Option<String>) -> Result<Self> {
        Self::spawn_with_preamble(cmd, run_label, &[])
    }

    /// As [`Self::spawn_with`], but paints `preamble` into the grid before the
    /// reader thread starts, so restored output cannot interleave with the new
    /// shell's first prompt.
    fn spawn_with_preamble(
        mut cmd: CommandBuilder,
        run_label: Option<String>,
        preamble: &[u8],
    ) -> Result<Self> {
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

        apply_pane_env(
            &mut cmd,
            crate::view_ipc::SOCK_PATH.get().map(|p| p.as_path()),
        );
        let child = pair.slave.spawn_command(cmd).context("spawn child")?;
        let shell_pid = child.process_id().map(|p| p as i32);
        drop(pair.slave);

        let writer: Arc<std::sync::Mutex<Box<dyn Write + Send>>> = Arc::new(std::sync::Mutex::new(
            pair.master.take_writer().context("take writer")?,
        ));
        let mut reader = pair.master.try_clone_reader().context("clone reader")?;

        let term_size = TermSize::new(cols as usize, rows as usize);
        let cfg = Config {
            // The user's `terminal_scrollback` (config.json) when set,
            // else the built-in default. Read per spawn so a settings edit
            // applies to the next pane without a relaunch.
            scrolling_history: crate::prefs::terminal_scrollback_lines(
                crate::prefs::Prefs::load_or_default().terminal_scrollback,
                SCROLLBACK_LINES,
            ),
            ..Config::default()
        };
        let size_shared = Arc::new(std::sync::Mutex::new((cols, rows)));
        let (response_tx, response_rx) = std::sync::mpsc::channel::<String>();
        let bell = Arc::new(AtomicBool::new(false));
        let listener = VoidListener {
            pty_response_tx: Some(response_tx),
            size: Some(size_shared.clone()),
            bell: Some(bell.clone()),
        };
        let mut term = Term::new(cfg, &term_size, listener);
        // Before the reader thread exists, so nothing the child writes can
        // land ahead of the restored output (#249).
        if !preamble.is_empty() {
            Processor::<StdSyncHandler>::new().advance(&mut term, preamble);
        }
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
        // Seeded with the spawn time, not zero: a pane that has not output
        // yet is quiet since it STARTED, so a just-launched agent reads as
        // working for its first seconds rather than as idle since 1970.
        let last_output_ms = Arc::new(std::sync::atomic::AtomicU64::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        ));
        let last_output_ms_for_thread = last_output_ms.clone();
        let bracketed_paste_enabled = Arc::new(AtomicBool::new(false));
        let bracketed_paste_for_thread = bracketed_paste_enabled.clone();
        let (port_tx, port_rx) = std::sync::mpsc::channel::<crate::port_detect::PortHit>();

        if let Some(label) = run_label.as_deref() {
            let header = format!("\x1b[2m▶ {label}\x1b[22m\r\n");
            let mut p = Processor::<StdSyncHandler>::new();
            let mut t = term.lock();
            p.advance(&mut *t, header.as_bytes());
        }

        let script_mode = run_label.is_some();

        let marks = Arc::new(std::sync::Mutex::new(Vec::<StoredMark>::new()));
        let marks_for_thread = marks.clone();
        let images = Arc::new(std::sync::Mutex::new(Vec::<StoredImage>::new()));
        let images_for_thread = images.clone();
        let rewind = Arc::new(std::sync::Mutex::new(crate::rewind::RewindBuffer::new(
            crate::rewind::DEFAULT_CAPACITY_BYTES,
        )));
        let rewind_for_thread = rewind.clone();
        // MONOTONIC, not the wall clock. Every read the buffer offers —
        // `span_ms`, `replay_from`, the orphan-keyframe sweep — assumes the
        // frames are ordered by `at_ms`, and `SystemTime` can step backwards
        // across an NTP correction or a manual set. A frame recorded during
        // such a step lands before its predecessors and becomes unreachable
        // through the only read API there is, which is a silent loss rather
        // than a visible fault. `Instant` cannot go backwards.
        let rewind_epoch = std::time::Instant::now();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel::<FinishedCommand>();
        let osc7_cwd = Arc::new(std::sync::Mutex::new(Option::<std::path::PathBuf>::None));
        let osc7_for_thread = osc7_cwd.clone();
        let notifications = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let notifications_for_thread = notifications.clone();
        let progress = Arc::new(std::sync::Mutex::new(None::<(u8, u8)>));
        let progress_for_thread = progress.clone();
        let line_times = Arc::new(std::sync::Mutex::new(
            std::collections::BTreeMap::<i64, u64>::new(),
        ));
        let line_times_for_thread = line_times.clone();
        let clock = Arc::new(std::sync::Mutex::new(ScrollClock::new()));
        let clock_for_thread = clock.clone();
        let osc7_host = Arc::new(std::sync::Mutex::new(None::<String>));
        let osc7_host_for_thread = osc7_host.clone();
        // Trigger set shared with the reader thread; the inner Arc is swapped
        // by `set_triggers` on startup / config reload, picked up per chunk.
        let triggers = Arc::new(std::sync::Mutex::new(std::sync::Arc::new(
            crate::triggers::TriggerSet::default(),
        )));
        let triggers_for_thread = triggers.clone();
        let (trigger_tx, trigger_rx) = std::sync::mpsc::channel::<crate::triggers::TriggerHit>();
        let watch_set = Arc::new(std::sync::Mutex::new(std::sync::Arc::new(
            crate::problem_matchers::WatchSet::default(),
        )));
        let watch_set_for_thread = watch_set.clone();
        let pane_watch = Arc::new(std::sync::Mutex::new(
            None::<std::sync::Arc<crate::problem_matchers::CompiledMatcher>>,
        ));
        let pane_watch_for_thread = pane_watch.clone();
        let (watch_tx, watch_rx) = std::sync::mpsc::channel::<(
            Option<std::path::PathBuf>,
            Vec<crate::build_matchers::BuildDiag>,
        )>();

        // Shutdown pipe + master fd for the reader's poll gate: the reader
        // must be wakeable without depending on the pty ever reaching EOF.
        let (shutdown_r, shutdown_w) = std::io::pipe().context("shutdown pipe")?;
        let pty_fd = pair
            .master
            .as_raw_fd()
            .context("pty master has no raw fd")?;
        let reader_thread = std::thread::spawn(move || {
            let mut processor = Processor::<StdSyncHandler>::new();
            let mut port_sniffer = crate::port_detect::PortSniffer::new();
            let mut osc_sniffer = crate::shell_integration::OscSniffer::default();
            let mut wipe_sniffer = WipeSniffer::default();
            let mut trigger_scanner = crate::triggers::TriggerScanner::new();
            let mut trigger_hits = Vec::new();
            let mut watch_engine = crate::problem_matchers::WatchEngine::default();
            let mut watch_lines: Vec<String> = Vec::new();
            // Last-seen matcher sources, for detecting a swap mid-window
            // (Arc identity — the app only ever replaces them wholesale).
            let mut last_wset = watch_set_for_thread.lock().unwrap().clone();
            let mut last_pwatch = pane_watch_for_thread.lock().unwrap().clone();
            // Per-pane monotonic id for captured inline images; the overlay
            // layout key uses it to tell a new picture from a moved one.
            let mut image_seq = 0u64;
            // Command timing: armed by 133;C, consumed by the next 133;D.
            // Content id of the cursor row after the previous chunk: the
            // rows this chunk touched run from there to the cursor's new
            // id, and each takes the chunk's arrival time (last touch wins,
            // so a row is stamped when its content actually landed, not
            // when the cursor first parked on it).
            let mut prev_stamp_id: i64 = 0;
            let mut cmd_start: Option<std::time::Instant> = None;
            let mut buf = [0u8; 65536];
            loop {
                if !wait_pty_readable(pty_fd, &shutdown_r) {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        sniff_bracketed_paste_mode(&buf[..n], &bracketed_paste_for_thread);
                        port_sniffer.sniff(&buf[..n], &port_tx);
                        // Shell-integration marks: split the advance at each
                        // OSC 133 so the cursor can be sampled exactly where
                        // the mark landed (alacritty drops the sequences
                        // themselves as unknown OSC).
                        let osc_events = osc_sniffer.scan(&buf[..n]);
                        let mut t = term_for_thread.lock();
                        let (screen_wiped, hist_wiped) =
                            wipe_sniffer.scan(&buf[..n], t.mode().contains(TermMode::ALT_SCREEN));
                        // Record the WHOLE read, before the OSC split below.
                        // The loop advances the parser per segment and leaves
                        // `done` at the LAST mark, so recording `buf[done..n]`
                        // afterwards kept only the tail — with shell
                        // integration on, marks arrive around every prompt and
                        // command, so most of the session was parsed to the
                        // screen and never recorded. That is the one thing
                        // this buffer exists to prevent, and the pty test
                        // covering it could not see the bug because its script
                        // emitted no marks (fixed: the OSC variant alongside
                        // it fails without this).
                        //
                        // Still recorded BEFORE the advance, so the buffer
                        // holds the bytes that PRODUCED the state a keyframe
                        // would capture. Takes only its own lock: the pane's
                        // order is term -> clock -> line_times and `t` is held
                        // here, so anything reaching back for `term` would
                        // deadlock — `push` returns a keyframe request rather
                        // than rendering one for exactly that reason.
                        // Poisons independently of `term` (std Mutex vs
                        // alacritty's FairMutex): a panic taken under this
                        // lock disables recording for the pane's life, and the
                        // `if let Ok` degrades to silence rather than
                        // propagating. Acceptable for a cache, but not the
                        // same failure mode as `term`'s.
                        if let Ok(mut rb) = rewind_for_thread.lock() {
                            let at = rewind_epoch.elapsed().as_millis() as u64;
                            // The keyframe request is dropped for now: this is
                            // the recording half, and nothing replays yet. The
                            // overlay that does will honour it here, taking the
                            // screen while it holds `t` rather than reaching
                            // back for the lock. Until then the buffer replays
                            // from the start, which is correct but unbounded.
                            let _wants_keyframe = rb.push(at, &buf[..n]);
                        }
                        let mut done = 0usize;
                        for (end, ev) in osc_events {
                            processor.advance(&mut *t, &buf[done..end]);
                            done = end;
                            use crate::shell_integration::OscEvent as E;
                            match ev {
                                E::Cwd(p, host) => {
                                    *osc7_for_thread.lock().unwrap() = Some(p);
                                    *osc7_host_for_thread.lock().unwrap() = host;
                                }
                                E::Notify(msg) => {
                                    notifications_for_thread.lock().unwrap().push(msg);
                                }
                                E::Progress(state, pct) => {
                                    // State 0 clears; the rest hold the latest
                                    // (state, percent) for the border gauge.
                                    *progress_for_thread.lock().unwrap() =
                                        (state != 0).then_some((state, pct));
                                }
                                E::InlineImage(data) => {
                                    // Anchor the picture at the cursor's grid
                                    // position, keyed on the scroll clock like
                                    // an annotation (lock order term → clock).
                                    let line_rec = t.grid().cursor.point.line.0;
                                    let clock_rec = clock_for_thread.lock().unwrap().tick(&mut t);
                                    let mut imgs = images_for_thread.lock().unwrap();
                                    if imgs.len() >= IMAGES_MAX {
                                        let drop_n = imgs.len() + 1 - IMAGES_MAX;
                                        imgs.drain(..drop_n);
                                    }
                                    image_seq += 1;
                                    imgs.push(StoredImage {
                                        seq: image_seq,
                                        data: std::sync::Arc::new(data),
                                        line_rec,
                                        clock_rec,
                                    });
                                }
                                kind @ (E::PromptStart
                                | E::PromptEnd
                                | E::CommandStart
                                | E::CommandEnd(_)) => {
                                    let dur = match &kind {
                                        E::CommandStart => {
                                            cmd_start = Some(std::time::Instant::now());
                                            None
                                        }
                                        E::CommandEnd(exit) => {
                                            // A finished command's gauge is
                                            // stale even if the program never
                                            // sent the state-0 clear.
                                            *progress_for_thread.lock().unwrap() = None;
                                            let dur = cmd_start.take().map(|s| s.elapsed());
                                            if let Some(d) = dur {
                                                // The typed command text and the
                                                // shell's cwd travel with the
                                                // completion so the durable
                                                // history records context.
                                                let (cmd, output) = {
                                                    let now = clock_for_thread
                                                        .lock()
                                                        .unwrap()
                                                        .tick(&mut t);
                                                    let ms = marks_for_thread.lock().unwrap();
                                                    (
                                                        last_command_input_text(&t, &ms, now),
                                                        last_command_output_text(&t, &ms, now),
                                                    )
                                                };
                                                let cwd = osc7_for_thread.lock().unwrap().clone();
                                                let host =
                                                    osc7_host_for_thread.lock().unwrap().clone();
                                                let _ = finished_tx.send(FinishedCommand {
                                                    exit: *exit,
                                                    dur: d,
                                                    cmd,
                                                    cwd,
                                                    host,
                                                    output,
                                                });
                                            }
                                            dur
                                        }
                                        _ => None,
                                    };
                                    let line_rec = t.grid().cursor.point.line.0;
                                    let clock_rec = clock_for_thread.lock().unwrap().tick(&mut t);
                                    let col_rec = t.grid().cursor.point.column.0;
                                    let mut ms = marks_for_thread.lock().unwrap();
                                    if ms.len() >= MARKS_MAX {
                                        let drop_n = ms.len() + 1 - MARKS_MAX;
                                        ms.drain(..drop_n);
                                    }
                                    ms.push(StoredMark {
                                        id: next_mark_id(),
                                        kind,
                                        line_rec,
                                        clock_rec,
                                        col_rec,
                                        dur,
                                    });
                                }
                            }
                        }
                        processor.advance(&mut *t, &buf[done..n]);
                        // A destructive clear erases the content the pane's
                        // captured images anchored to: ED 2 the live screen
                        // rows, ED 3 the scrollback ones, RIS both. The
                        // sniffer already excluded wipes that landed inside
                        // the alt screen (positional tracking), so no mode
                        // check here — the chunk's FINAL mode says nothing
                        // about where a wipe fell (`clear && vim`).
                        if screen_wiped || hist_wiped {
                            let now = clock_for_thread.lock().unwrap().tick(&mut t);
                            let erased = |line_rec: i32, clock_rec: i64| {
                                let line = line_rec - (now - clock_rec) as i32;
                                if line >= 0 { screen_wiped } else { hist_wiped }
                            };
                            images_for_thread
                                .lock()
                                .unwrap()
                                .retain(|m| !erased(m.line_rec, m.clock_rec));
                            // Marks anchored to erased content go the same
                            // way: a stale decoration dot's menu copies or
                            // re-runs whatever later lands on its row.
                            marks_for_thread
                                .lock()
                                .unwrap()
                                .retain(|m| !erased(m.line_rec, m.clock_rec));
                        }
                        // Stamp newly-arrived rows for the timestamps gutter:
                        // every row the cursor moved past in this chunk gets
                        // the chunk's arrival time (one read of output is
                        // sub-millisecond, so chunk granularity is honest).
                        {
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let mut clock = clock_for_thread.lock().unwrap();
                            let mut lt = line_times_for_thread.lock().unwrap();
                            stamp_chunk(&mut t, &mut clock, &mut lt, &mut prev_stamp_id, now_ms);
                        }
                        // Notify/bell triggers match completed output lines,
                        // never inside a full-screen app (the alt screen owns
                        // the bytes; iTerm2 skips those too). The scanner
                        // tracks the alt boundary POSITIONALLY from the
                        // stream — the chunk's final mode says nothing about
                        // where a line fell — so it must see EVERY chunk, or
                        // its alt tracking and string state machine desync
                        // and skipped-gap output splices into phantom lines.
                        let trig = triggers_for_thread.lock().unwrap().clone();
                        // Watch problem matchers (#252) ride the same
                        // scanner: when any are configured, the completed
                        // lines it already produces feed the per-pane watch
                        // engine (no second byte scan over the stream).
                        let wset = watch_set_for_thread.lock().unwrap().clone();
                        let pwatch = pane_watch_for_thread.lock().unwrap().clone();
                        // Matcher ownership changed since the last chunk
                        // (matchers.json reload, task (re)assignment):
                        // drop any half-open window, or its next `ends`
                        // would scan stale lines with the OLD matcher and
                        // publish a stale batch.
                        let pwatch_same = match (&pwatch, &last_pwatch) {
                            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                            (None, None) => true,
                            _ => false,
                        };
                        if !Arc::ptr_eq(&wset, &last_wset) || !pwatch_same {
                            watch_engine.reset();
                            last_wset = wset.clone();
                            last_pwatch = pwatch.clone();
                        }
                        if wset.matchers.is_empty() && pwatch.is_none() {
                            trigger_scanner.scan(&buf[..n], &trig, &mut trigger_hits);
                        } else {
                            watch_lines.clear();
                            trigger_scanner.scan_collect(
                                &buf[..n],
                                &trig,
                                &mut trigger_hits,
                                Some(&mut watch_lines),
                            );
                            for line in watch_lines.drain(..) {
                                if let Some(batch) =
                                    watch_engine.feed(&line, &wset, pwatch.as_ref())
                                {
                                    let cwd = osc7_for_thread.lock().unwrap().clone();
                                    let _ = watch_tx.send((cwd, batch));
                                }
                            }
                        }
                        for h in trigger_hits.drain(..) {
                            let _ = trigger_tx.send(h);
                        }
                        drop(t);
                        pty_pending_bytes_for_thread.fetch_add(n, Ordering::Relaxed);
                        last_output_ms_for_thread.store(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0),
                            Ordering::Relaxed,
                        );
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
            last_output_ms,
            agent: None,
            bracketed_paste_enabled,
            port_rx,
            master: pair.master,
            writer,
            _child: child,
            reader_thread: Some(reader_thread),
            reader_shutdown: Some(shutdown_w),
            shell_pid,
            uid: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            input_seen: false,
            run_pane: script_mode,
            manual_name_seen: false,
            cols,
            rows,
            size_shared,
            focused: false,
            broadcast_excluded: false,
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            selection: None,
            copy_cursor: None,
            sel_scrolled: 0,
            clock,
            alt_sel: None,
            drag_selecting: false,
            manual_name: None,
            auto_label: String::new(),
            search_needle: None,
            search_opts: crate::widgets::search::SearchOpts::default(),
            current_match: None,
            bell,
            marks,
            osc7_cwd,
            osc7_host,
            accent: None,
            accent_badge: None,
            notifications,
            progress,
            line_times,
            show_timestamps: false,
            reveal_redactions: false,
            redacted_on_screen: 0,
            annotations: Vec::new(),
            finished_rx,
            hints: None,
            triggers,
            trigger_rx,
            watch_set,
            pane_watch,
            watch_rx,
            palette: crate::theme::VSCODE_ANSI,
            images,
            rewind,
            #[cfg(test)]
            written_for_test: Arc::new(std::sync::Mutex::new(Vec::new())),
        })
    }

    /// OSC 133 marks with their *current* grid line (negative = scrollback),
    /// oldest first. Marks whose content scrolled past the scrollback floor
    /// are garbage-collected here.
    pub fn command_marks(&self) -> Vec<(crate::shell_integration::OscEvent, i32)> {
        self.marks_snapshot()
            .into_iter()
            .map(|m| (m.kind, m.line))
            .collect()
    }

    /// The marks with current grid lines and, for `CommandEnd`, the measured
    /// command duration. GC + drift adjustment as in [`Self::command_marks`].
    fn marks_snapshot(&self) -> Vec<MarkView> {
        let mut term = self.term.lock();
        let now = self.clock.lock().unwrap().tick(&mut term);
        let floor = term.grid().topmost_line().0;
        drop(term);
        let mut marks = self.marks.lock().unwrap();
        marks.retain(|m| m.line_rec - (now - m.clock_rec) as i32 >= floor);
        marks
            .iter()
            .map(|m| MarkView {
                id: m.id,
                kind: m.kind.clone(),
                line: m.line_rec - (now - m.clock_rec) as i32,
                col: m.col_rec,
                dur: m.dur,
            })
            .collect()
    }

    /// One record per finished command, for the gutter decorations: prompt
    /// line (current grid coords), exit code, duration.
    pub fn command_decorations(&self) -> Vec<CommandDecoration> {
        pair_decorations(&self.marks_snapshot())
    }

    /// Finished commands since the last drain: (exit code, duration). Feeds
    /// the status-bar notification for long commands in unfocused panes.
    pub fn drain_finished_commands(&self) -> Vec<FinishedCommand> {
        self.finished_rx.try_iter().collect()
    }

    /// The text a finished command printed: its output span (`output_start`
    /// up to but excluding `output_end`), trailing blank lines dropped.
    pub fn command_output_text(&self, d: &CommandDecoration) -> String {
        if d.output_end <= d.output_start {
            return String::new();
        }
        let term = self.term.lock();
        extract_selection_text(
            &term,
            d.output_start,
            0,
            d.output_end - 1,
            term.columns().saturating_sub(1),
        )
    }

    /// The command as it was typed at its prompt: from the `PromptEnd`
    /// mark's cell to the end of the input rows (the row before the output
    /// starts), soft-wrapped rows joined.
    pub fn command_input_text(&self, d: &CommandDecoration) -> String {
        let Some((line, col)) = d.input else {
            return String::new();
        };
        if d.output_start <= line {
            return String::new();
        }
        let term = self.term.lock();
        extract_selection_text(
            &term,
            line,
            col,
            d.output_start - 1,
            term.columns().saturating_sub(1),
        )
    }

    /// Select the command's output span (and snap the view to it), so the
    /// user sees exactly what Copy Output grabbed. No-op for empty output.
    pub fn select_command_output(&mut self, d: &CommandDecoration) {
        if d.output_end <= d.output_start {
            return;
        }
        self.selection = Some(Selection {
            anchor: (d.output_start, 0),
            head: (d.output_end - 1, self.cols.saturating_sub(1)),
            block: false,
        });
        self.stamp_selection_clock();
        self.scroll_to_line(d.output_start);
    }

    /// The decoration whose gutter dot sits at screen cell (col, row), if
    /// any: col must be the pane's left border column and row a viewport
    /// row whose grid line carries a finished command.
    pub fn decoration_at_screen(&self, col: u16, row: u16) -> Option<CommandDecoration> {
        let inner = self.last_inner;
        if col != self.last_area.x || row < inner.y || row >= inner.y + inner.height {
            return None;
        }
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let display_offset = term.grid().display_offset() as i32;
        drop(term);
        let line = i32::from(row - inner.y) - display_offset;
        self.command_decorations()
            .into_iter()
            .find(|d| d.line == line)
    }

    /// Current grid lines of the PromptStart marks — the Cmd+Up/Cmd+Down
    /// navigation targets.
    pub fn prompt_lines(&self) -> Vec<i32> {
        self.command_marks()
            .into_iter()
            .filter(|(kind, _)| *kind == crate::shell_integration::OscEvent::PromptStart)
            .map(|(_, line)| line)
            .collect()
    }

    /// The shell's live cwd per its latest OSC 7 report, when shell
    /// integration is active. Fresher than sampling `cwd_of_pid`.
    pub fn shell_cwd(&self) -> Option<std::path::PathBuf> {
        self.osc7_cwd.lock().unwrap().clone()
    }

    /// The kernel-reported cwd of the pane's shell process. Unlike
    /// [`Self::shell_cwd`] it needs no shell integration, so it answers for
    /// any local pane; `None` when the pid or the kernel query is
    /// unavailable (android, a dead shell, a remote pane's ssh process).
    pub fn kernel_shell_cwd(&self) -> Option<std::path::PathBuf> {
        cwd_of_pid(u32::try_from(self.shell_pid?).ok()?)
    }

    /// OSC 9 notification payloads since the last drain.
    pub fn drain_notifications(&self) -> Vec<String> {
        std::mem::take(&mut *self.notifications.lock().unwrap())
    }

    /// The viewport-top grid line: what `pick_prompt_jump` navigates from.
    pub fn viewport_top_line(&self) -> i32 {
        -(self.term.lock().grid().display_offset() as i32)
    }

    /// Scroll so absolute grid line `abs_line` sits at the top of the pane
    /// (VS Code parks the jumped-to command at the top). No-op in alternate
    /// screen, like the other scrollback moves.
    pub fn scroll_line_to_top(&mut self, abs_line: i32) {
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let max_off = (-term.grid().topmost_line().0).max(0);
        let desired = (-abs_line).clamp(0, max_off);
        let delta = desired - term.grid().display_offset() as i32;
        if delta != 0 {
            term.scroll_display(Scroll::Delta(delta));
        }
        drop(term);
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// True once if the child rang BEL since the last call (drains the latch).
    pub fn take_bell(&self) -> bool {
        self.bell.swap(false, Ordering::AcqRel)
    }

    /// The pane's foreground process group leader pid (what owns the tty now):
    /// the shell at a prompt, or a running command. `None` if unavailable.
    pub fn foreground_pid(&self) -> Option<i32> {
        self.master.process_group_leader()
    }

    /// The shell's own pid, stable for this pane's lifetime (the key the app
    /// uses to match async label lookups back to the right pane).
    pub fn shell_pid(&self) -> Option<i32> {
        self.shell_pid
    }

    /// The label to show for this pane: manual name if set, else the live
    /// foreground-process label.
    pub fn label(&self) -> &str {
        pick_pane_label(self.manual_name.as_deref(), &self.auto_label)
    }

    /// Set the foreground-process label (from the off-loop refresh).
    pub fn set_auto_label(&mut self, label: String) {
        self.auto_label = label;
    }

    /// How long since the PTY last produced output; a pane that never has is
    /// quiet since it was spawned.
    pub fn quiet_for(&self) -> std::time::Duration {
        let last = self.last_output_ms.load(Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        std::time::Duration::from_millis(now.saturating_sub(last))
    }

    /// The agent seated in this pane, if the last sample found one (#344).
    pub fn agent(&self) -> Option<&crate::agents::AgentLane> {
        self.agent.as_ref()
    }

    pub fn set_agent(&mut self, lane: Option<crate::agents::AgentLane>) {
        self.agent = lane;
    }

    /// The last `n` non-empty VISIBLE rows, oldest first, for prompt
    /// matching. Reads the viewport only: the whole scrollback is what
    /// `grid_lines` walks, and a prompt is on screen or it is not there.
    pub fn tail_rows(&self, n: usize) -> Vec<String> {
        let text = self.visible_text();
        let mut rows: Vec<String> = text
            .lines()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .take(n)
            .map(|l| l.trim_end().to_string())
            .collect();
        rows.reverse();
        rows
    }

    /// The user's manual pane name, when one was set via rename (what the
    /// session snapshot persists; `label()` mixes in the auto label).
    pub fn manual_name(&self) -> Option<&str> {
        self.manual_name.as_deref()
    }

    /// Set or clear the user's manual pane name (a blank name clears it).
    pub fn set_manual_name(&mut self, name: Option<String>) {
        self.manual_name = name.filter(|n| !n.trim().is_empty());
        if self.manual_name.is_some() {
            self.manual_name_seen = true;
        }
    }

    pub fn take_dirty(&self) -> bool {
        self.pty_pending_bytes.store(0, Ordering::Relaxed);
        self.pty_dirty.swap(false, Ordering::AcqRel)
    }

    /// The pane's live OSC 9;4 progress `(state, percent)`, if a program is
    /// reporting one.
    pub fn progress(&self) -> Option<(u8, u8)> {
        *self.progress.lock().unwrap()
    }

    /// The arrow-key bytes a plain click at screen cell (col, row) should
    /// send to walk the shell cursor to the clicked column (Ghostty's
    /// click-to-move-cursor). `Some` only when the shell is sitting at a
    /// prompt (the newest OSC 133 mark is B, on this same row), the click
    /// lands on the cursor's own row, and the target cell differs from the
    /// cursor. The target clamps to the typed span: never left of where
    /// input starts, never past one cell after the row's last glyph. The
    /// line itself is untouched — only cursor motion is synthesized, so it
    /// works identically in zsh, bash, and fish line editors.
    pub fn prompt_click_arrows(&self, col: u16, row: u16) -> Option<Vec<u8>> {
        let (vr, vc) = self.cell_at(col, row)?;
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let off = term.grid().display_offset() as i32;
        let line = vr as i32 - off;
        let cursor = term.grid().cursor.point;
        if line != cursor.line.0 {
            return None;
        }
        let now = self.clock_now(&term);
        let ms = self.marks.lock().unwrap();
        let last = ms.last()?;
        if !matches!(last.kind, crate::shell_integration::OscEvent::PromptEnd) {
            return None;
        }
        if last.line_rec - (now - last.clock_rec) as i32 != line {
            return None;
        }
        let b_col = last.col_rec as u16;
        let (text, colmap) = row_text_and_cols(&term, line);
        let chars = text.trim_end().chars().count();
        let end_col = if chars == 0 {
            b_col
        } else {
            colmap
                .get(chars - 1)
                .map(|&c| c as u16 + 1)
                .unwrap_or(b_col)
        };
        let cur = cursor.column.0 as u16;
        // The row may have been rewritten SHORTER than the recorded 133;B
        // column (a backgrounded progress writer, a shrunk pane): the live
        // bounds then sit left of `b_col`, and `clamp` with min > max
        // panics. Bound the lower edge by the upper so the gesture
        // degrades instead of taking the app down.
        let hi = end_col.max(cur);
        let target = vc.clamp(b_col.min(hi), hi);
        if target == cur {
            return None;
        }
        let (seq, n) = if target > cur {
            (b"\x1b[C".as_slice(), target - cur)
        } else {
            (b"\x1b[D".as_slice(), cur - target)
        };
        Some(seq.repeat(n as usize))
    }

    /// The hostname the shell last reported over OSC 7 (an in-pane SSH
    /// session with integration moves it to the remote host).
    pub fn shell_host(&self) -> Option<String> {
        self.osc7_host.lock().unwrap().clone()
    }

    /// [`Self::shell_cwd`], trusted only when the report provably applies
    /// to the LOCAL shell: the reporting host must be this machine (empty,
    /// "localhost", or the local hostname — RFC 8089 semantics via the
    /// command history's canonicalizer) AND the claimed path must name the
    /// directory the kernel says the shell is in ([`cwd_of_pid`]). PTY
    /// bytes carry no author — any foreground job can print a
    /// local-looking claim and die before the bytes are even parsed — so
    /// no tty-ownership sample at any moment establishes provenance.
    /// Matching the kernel's ground truth does: a forged claim equal to
    /// the truth grants nothing, and one that differs is rejected. The
    /// cost is that platforms without [`cwd_of_pid`] never trust a claim.
    pub fn local_shell_cwd(&self) -> Option<std::path::PathBuf> {
        let host = self.shell_host().unwrap_or_default();
        if !crate::command_history::is_local_host(&host) {
            return None;
        }
        let claim = self.shell_cwd()?;
        let real = cwd_of_pid(self.pid()?)?;
        // Symlink-tolerant equality (macOS reports /private/tmp for a
        // shell sitting in /tmp): equal canonical paths name the same
        // directory, so a claim passing this check cannot mislead.
        (claim.canonicalize().ok()? == real.canonicalize().ok()?).then_some(claim)
    }

    /// The pane's monotonic scroll-clock reading (see `clock_now`): the
    /// content anchor the app pairs with grid coordinates it captures now
    /// (annotation prompts, copy mode), so the pair survives streaming
    /// output and scrollback saturation alike.
    pub fn scroll_clock(&mut self) -> i64 {
        self.tick_clock()
    }

    /// Pin a note to the span starting at grid `line`, columns
    /// `start..start+len` (Cmd+K N's commit). `clock` is the scroll-clock
    /// reading `line` was captured against ([`Self::scroll_clock`] at the
    /// prompt's OPEN) — sampling it here instead would mis-anchor the note
    /// by every row that streamed while the user typed it.
    pub fn add_annotation(&mut self, line: i32, clock: i64, start: u16, len: u16, text: String) {
        self.annotations.push(PaneAnnotation {
            line_rec: line,
            clock_rec: clock,
            start,
            len: len.max(1),
            text,
        });
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Every annotation with its span translated to current grid lines:
    /// `(line, start, len, text)`. Spans whose content fell off the
    /// scrollback are dropped for good.
    pub fn annotations_current(&mut self) -> Vec<(i32, u16, u16, String)> {
        let now = self.tick_clock();
        let top = self.term.lock().grid().topmost_line().0;
        self.annotations
            .retain(|a| a.line_rec - (now - a.clock_rec) as i32 >= top);
        self.annotations
            .iter()
            .map(|a| {
                (
                    a.line_rec - (now - a.clock_rec) as i32,
                    a.start,
                    a.len,
                    a.text.clone(),
                )
            })
            .collect()
    }

    /// The newest grid line where a captured line starts, or `None`. The
    /// straight case is a row carrying `needle` as a prefix; a pane
    /// narrower than the needle instead shows the line's FIRST WRAPPED ROW,
    /// whose text is a prefix OF the needle. That reverse arm is gated on
    /// real wrap metadata, not row length: the row must itself soft-wrap
    /// (WRAPLINE on its last cell) — otherwise any short unrelated row
    /// sharing the prefix would win — and continuation rows (the PREVIOUS
    /// row wraps) never match either arm, or a later continuation sharing
    /// the prefix would beat the true start under the newest-first scan.
    pub fn find_captured_line(&self, needle: &str) -> Option<i32> {
        if needle.is_empty() {
            return None;
        }
        let term = self.term.lock();
        let cols = term.columns();
        let wraps = |line: i32| {
            cols > 0
                && term.grid()[Point::new(Line(line), Column(cols - 1))]
                    .flags
                    .contains(Flags::WRAPLINE)
        };
        let top = term.grid().topmost_line().0;
        let bottom = term.screen_lines() as i32 - 1;
        for line in (top..=bottom).rev() {
            if line > top && wraps(line - 1) {
                continue;
            }
            let (text, _) = row_text_and_cols(&term, line);
            let t = text.trim_end();
            if t.is_empty() {
                continue;
            }
            if t.starts_with(needle) || (wraps(line) && needle.starts_with(t)) {
                return Some(line);
            }
        }
        None
    }

    /// Every annotation translated against an EXPLICIT clock reading — the
    /// caller's own snapshot — as `(line, start, len, text)`. The
    /// existing-note lookup in the annotate prompt compares these lines
    /// against a selection captured WITH that reading; translating with a
    /// fresh (later) clock instead let output that scrolled in between
    /// shift the annotations away from the still-fixed selection line, so
    /// annotating the same span opened an empty prompt and duplicated the
    /// note. Pure translation: the scrolled-off GC stays in
    /// [`Self::annotations_current`].
    pub fn annotations_at_clock(&self, clock: i64) -> Vec<(i32, u16, u16, String)> {
        self.annotations
            .iter()
            .map(|a| {
                (
                    a.line_rec - (clock - a.clock_rec) as i32,
                    a.start,
                    a.len,
                    a.text.clone(),
                )
            })
            .collect()
    }

    /// The real text under a masked (redacted) cell at screen `(col, row)`,
    /// for the click-to-reveal popup (#360). None off the pane, off a
    /// mask, or while an alt-screen app owns the viewport.
    pub fn redacted_at(&self, col: u16, row: u16) -> Option<String> {
        let (vr, vc) = self.cell_at(col, row)?;
        let set = self.triggers.lock().unwrap().clone();
        if !set.has_redactions() {
            return None;
        }
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let line = vr as i32 - term.grid().display_offset() as i32;
        let (text, colmap) = row_text_and_cols(&term, line);
        drop(term);
        // The clicked column as a char index: a wide char's spacer column
        // resolves to the wide char itself (the `line_text_at` rule).
        let vc = vc as usize;
        // `row_text_and_cols` maps every non-spacer column, blanks
        // included, so a click on a blank cell resolves to that blank's own
        // char index - never back into a token. The `rposition` only ever
        // steps back for a wide char's spacer column, which is the wide
        // char itself.
        let ci = colmap.iter().rposition(|&gc| gc <= vc)?;
        crate::triggers::redact_spans(&text, &set)
            .into_iter()
            .find(|s| ci >= s.start && ci < s.start + s.len)
            .map(|s| text.chars().skip(s.start).take(s.len).collect())
    }

    /// The annotation index + note under screen cell (col, row), if any.
    /// Annotations live on the primary screen; while an alt-screen app owns
    /// the viewport their lines describe rows the user cannot see, so no
    /// cell resolves.
    pub fn annotation_at(&self, col: u16, row: u16) -> Option<(usize, String)> {
        let (vr, vc) = self.cell_at(col, row)?;
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let off = term.grid().display_offset() as i32;
        let now = self.clock_now(&term);
        drop(term);
        let line = vr as i32 - off;
        self.annotations
            .iter()
            .enumerate()
            .find(|(_, a)| {
                a.line_rec - (now - a.clock_rec) as i32 == line
                    && vc >= a.start
                    && vc < a.start + a.len
            })
            .map(|(i, a)| (i, a.text.clone()))
    }

    pub fn remove_annotation(&mut self, idx: usize) {
        if idx < self.annotations.len() {
            self.annotations.remove(idx);
            self.pty_dirty.store(true, Ordering::Release);
        }
    }

    pub fn set_annotation_text(&mut self, idx: usize, text: String) {
        if let Some(a) = self.annotations.get_mut(idx) {
            a.text = text;
            self.pty_dirty.store(true, Ordering::Release);
        }
    }

    /// When the row at current grid `line` arrived (epoch millis), if the
    /// reader thread stamped it.
    pub fn row_time(&self, line: i32) -> Option<u64> {
        let term = self.term.lock();
        let now = self.clock_now(&term);
        drop(term);
        self.line_times
            .lock()
            .unwrap()
            .get(&(i64::from(line) + now))
            .copied()
    }

    /// Drain the loopback ports the reader thread scraped since the last call.
    /// Each port is reported at most once over the terminal's lifetime, so this
    /// is empty on most ticks.
    pub fn drain_ports(&self) -> Vec<crate::port_detect::PortHit> {
        self.port_rx.try_iter().collect()
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

    /// Text of the grid row under screen cell `(col, row)`, plus the 0-based
    /// column within that text the click landed on. Drives Cmd/Ctrl+click URL
    /// detection. `None` when the cell is outside the content area.
    pub fn line_text_at(&self, col: u16, row: u16) -> Option<(String, usize)> {
        let (r, c) = self.cell_at(col, row)?;
        let term = self.term.lock();
        let line = r as i32 - term.grid().display_offset() as i32;
        let (text, cols_map) = row_text_and_cols(&term, line);
        // Grid column → char index in the spacer-skipped text: the last
        // produced char at-or-before the clicked column, so clicking a wide
        // char's spacer cell resolves to the wide char itself.
        let idx = cols_map.iter().rposition(|&gc| gc <= c as usize)?;
        Some((text, idx))
    }

    /// The OSC 8 hyperlink under a screen position (host cell coords), if
    /// any — the screen-coord twin of [`Self::hyperlink_at`].
    pub fn hyperlink_at_screen(&self, col: u16, row: u16) -> Option<String> {
        let (r, c) = self.cell_at(col, row)?;
        self.hyperlink_at(r as usize, c as usize)
    }

    /// The current scroll-clock reading against an already-locked term.
    fn clock_now(&self, term: &Term<VoidListener>) -> i64 {
        self.clock.lock().unwrap().now(term)
    }

    /// Fold the tracer's drift into the clock, re-plant it fresh and return
    /// the folded reading (see [`ScrollClock::tick`]).
    fn tick_clock(&mut self) -> i64 {
        let mut term = self.term.lock();
        self.clock.lock().unwrap().tick(&mut term)
    }

    /// Stamp freshly-set selection / copy-cursor coordinates against a
    /// fresh clock tick; a selection set on the alternate screen also
    /// captures its content anchor (see `AltSelAnchor`).
    fn stamp_selection_clock(&mut self) {
        self.sel_scrolled = self.tick_clock();
        let term = self.term.lock();
        self.alt_sel = if term.mode().contains(TermMode::ALT_SCREEN) {
            self.selection.and_then(|s| capture_alt_anchor(&term, s))
        } else {
            None
        };
    }

    /// Re-anchor an alternate-screen selection to its content after the
    /// app repainted (see `AltSelAnchor`). Finds the vertical shift whose
    /// on-grid rows all match the remembered ones — requiring at least one
    /// non-blank matched row so a blank fingerprint can't latch anywhere —
    /// preferring the smallest movement; no qualifying shift parks the
    /// selection dormant until its content scrolls back into view. A
    /// selection created on the primary screen has no anchor and simply
    /// stays frozen for the duration of the alt-screen trip.
    fn rebase_alt_selection(&mut self) {
        if self.selection.is_none() {
            self.alt_sel = None;
            return;
        }
        let Some(mut anchor) = self.alt_sel.take() else {
            return;
        };
        let term = self.term.lock();
        let rows_vis = term.screen_lines() as i32;
        let old_top = anchor.top;
        let k = anchor.rows.len() as i32;
        // One grid snapshot up front (text + char→column maps): the
        // matcher probes every candidate shift, so per-candidate row reads
        // would re-extract (and re-allocate) the same rows dozens of times
        // a frame.
        let grid_rows: Vec<(String, Vec<usize>)> =
            (0..rows_vis).map(|l| row_text_and_cols(&term, l)).collect();
        // Per candidate shift, how many block rows the grid matches
        // exactly at that offset. Rows can go missing not just off-grid:
        // app chrome (Claude Code's input box, its floating pills, an
        // animated status row) overdraws rows anywhere in the block, so
        // partial survival anchors by however many exact rows remain — at
        // least two (when the block has two) including a non-blank one,
        // so a lone repeated divider row can't latch the highlight onto an
        // unrelated copy of itself. More exact rows win; ties go to the
        // shift whose captured neighbour rows also match (a ONE-row block
        // has a one-row fingerprint, and full-screen apps repeat rows
        // verbatim — the neighbours tell its copy from the lookalikes),
        // then to the smallest movement.
        let k_rows = anchor.rows.len();
        let min_exact = k_rows.min(2);
        // (top, exact-row count, matched-neighbour count)
        let mut best: Option<(i32, usize, usize)> = None;
        for top in 1 - k..rows_vis {
            let (mut exact, mut nonblank) = (0usize, false);
            for (i, want) in anchor.rows.iter().enumerate() {
                let line = top + i as i32;
                if (0..rows_vis).contains(&line) && grid_rows[line as usize].0 == *want {
                    exact += 1;
                    nonblank |= !want.trim().is_empty();
                }
            }
            let mut ctx = 0usize;
            for (want, line) in [
                (anchor.ctx_above.as_ref(), top - 1),
                (anchor.ctx_below.as_ref(), top + k),
            ] {
                if let Some(want) = want
                    && (0..rows_vis).contains(&line)
                    && grid_rows[line as usize].0 == *want
                {
                    ctx += 1;
                }
            }
            if exact >= min_exact
                && nonblank
                && best.is_none_or(|(btop, bexact, bctx)| {
                    exact > bexact
                        || (exact == bexact && ctx > bctx)
                        || (exact == bexact
                            && ctx == bctx
                            && (top - old_top).abs() < (btop - old_top).abs())
                })
            {
                best = Some((top, exact, ctx));
            }
        }
        match best {
            Some((top, _, _)) => {
                let d = top - old_top;
                anchor.top = top;
                if d != 0
                    && let Some(s) = self.selection.as_mut()
                {
                    s.anchor.0 += d;
                    s.head.0 += d;
                }
                anchor.dormant = false;
                // The clip: every block row still on screen, whole when its
                // text matches exactly, or just the surviving prefix/suffix
                // columns when app chrome (a floating pill) overdraws part
                // of it. The shift is already pinned by the exact run, so
                // partial credit here can't drag the highlight elsewhere.
                let mut clips: Vec<(i32, u16, u16)> = Vec::new();
                let mut all_exact = true;
                for (i, want) in anchor.rows.iter().enumerate() {
                    let line = top + i as i32;
                    if !(0..rows_vis).contains(&line) {
                        all_exact = false;
                        continue;
                    }
                    let (got, colmap) = &grid_rows[line as usize];
                    if got == want {
                        clips.push((line, 0, u16::MAX));
                    } else {
                        all_exact = false;
                        for (lo, hi) in partial_overlap_cols(want, got, colmap) {
                            clips.push((line, lo, hi));
                        }
                    }
                }
                anchor.visible = (!all_exact).then_some(clips);
                // Refresh the anchor — this is also what folds in a user
                // extension of the selection — but only when every row is
                // in view and intact, so the remembered block never loses
                // a covered or off-screen part mid-scroll.
                if all_exact && let Some(fresh) = capture_alt_anchor(&term, self.selection.unwrap())
                {
                    anchor = fresh;
                }
            }
            // Nothing matched anywhere. Mid-drag that must NOT hide the
            // selection — the drag may have started on a blank row or an
            // animated status line, and the user is pointing at what they
            // see right now; the real anchor is captured on release.
            None if self.drag_selecting => {
                anchor.dormant = false;
                anchor.visible = None;
                if let Some(fresh) = capture_alt_anchor(&term, self.selection.unwrap()) {
                    anchor = fresh;
                }
            }
            None => anchor.dormant = true,
        }
        drop(term);
        self.alt_sel = Some(anchor);
    }

    /// Shift the stored selection and copy-cursor down-anchored coordinates
    /// by the scroll-clock movement since they were recorded, so they keep
    /// naming the same content while output streams. Endpoints whose
    /// content fell off the scrollback pin to the oldest line; a selection
    /// entirely off the buffer is dropped for good. No-op (and no lock)
    /// when neither exists — setters stamp `sel_scrolled` themselves.
    fn rebase_selection(&mut self) {
        if self.selection.is_none() && self.copy_cursor.is_none() {
            return;
        }
        if self.term.lock().mode().contains(TermMode::ALT_SCREEN) {
            self.rebase_alt_selection();
            return;
        }
        if self.alt_sel.take().is_some() {
            // The selection was made on the alternate screen and the app
            // has left it: the alternate grid — and the content the
            // coordinates named — is gone with it.
            self.selection = None;
            self.copy_cursor = None;
            return;
        }
        let now = self.tick_clock();
        let top = self.term.lock().grid().topmost_line().0;
        let delta = (now - self.sel_scrolled) as i32;
        self.sel_scrolled = now;
        if delta != 0 {
            if let Some(sel) = self.selection.as_mut() {
                sel.anchor.0 -= delta;
                sel.head.0 -= delta;
            }
            if let Some(cur) = self.copy_cursor.as_mut() {
                cur.0 -= delta;
            }
        }
        if self
            .selection
            .is_some_and(|s| s.anchor.0 < top && s.head.0 < top)
        {
            self.selection = None;
        } else if let Some(sel) = self.selection.as_mut() {
            sel.anchor.0 = sel.anchor.0.max(top);
            sel.head.0 = sel.head.0.max(top);
        }
        if let Some(cur) = self.copy_cursor.as_mut() {
            cur.0 = cur.0.max(top);
        }
    }

    pub fn start_selection_at(&mut self, col: u16, row: u16) {
        if let Some((r, c)) = self.cell_at(col, row) {
            let line = r as i32 - self.display_offset();
            self.selection = Some(Selection::new(line, c));
            self.drag_selecting = true;
            self.stamp_selection_clock();
        }
    }

    /// Replay a saved transcript into this pane's grid so a restored pane
    /// shows the output it had before a restart (#249), above the fresh
    /// prompt the new shell prints.
    ///
    /// Each line is written literally with CRLF and NO escape sequences:
    /// the transcript is plain text precisely so a replay cannot re-run a
    /// cursor move, a screen clear, or a title change that the original
    /// output contained. The shell is untouched — this only paints the
    /// grid, so the prompt sits below the restored text.
    pub fn replay_transcript(&self, lines: &[String]) {
        let bytes = transcript_bytes(lines);
        if bytes.is_empty() {
            return;
        }
        let mut p = Processor::<StdSyncHandler>::new();
        let mut term = self.term.lock();
        p.advance(&mut *term, &bytes);
    }

    /// The pane's rewind buffer (#357), for the scrubber to replay from.
    ///
    /// Handed out as the shared handle rather than a copy: the buffer is
    /// megabytes by design, and the reader thread is writing to it while the
    /// overlay reads, so a snapshot would be both expensive and immediately
    /// stale.
    pub fn rewind(&self) -> &Arc<std::sync::Mutex<crate::rewind::RewindBuffer>> {
        &self.rewind
    }

    /// Test-only: parse `bytes` straight into this pane's grid, as if the
    /// child had printed them — app-level tests drive alt-screen repaint
    /// scenarios deterministically through this.
    #[cfg(test)]
    pub fn feed_bytes_for_test(&self, bytes: &[u8]) {
        // Deliberately does NOT record into the rewind buffer, though the
        // reader thread does (#357). Recording here too would be a SECOND
        // implementation of that wiring, and a test driving this helper
        // would then pass with the reader thread recording nothing at all —
        // measured: deleting the reader's `rb.push` left the app-level test
        // green. The recording is proven through a real pty instead, by
        // `the_reader_thread_records_shell_output_for_rewind`.
        let mut p = Processor::<StdSyncHandler>::new();
        let mut term = self.term.lock();
        p.advance(&mut *term, bytes);
    }

    /// Test-only: run one reader-thread timestamp pass over the current
    /// grid, exactly as a real chunk arrival would.
    #[cfg(test)]
    pub fn stamp_chunk_for_test(&self, prev_id: &mut i64, now_ms: u64) {
        let mut term = self.term.lock();
        let mut clock = self.clock.lock().unwrap();
        let mut lt = self.line_times.lock().unwrap();
        stamp_chunk(&mut term, &mut clock, &mut lt, prev_id, now_ms);
    }

    /// Test-only: the stamped (content id, arrival ms) pairs.
    #[cfg(test)]
    pub fn line_time_entries_for_test(&self) -> Vec<(i64, u64)> {
        self.line_times
            .lock()
            .unwrap()
            .iter()
            .map(|(&k, &v)| (k, v))
            .collect()
    }

    /// Test-only: record an OSC 133 mark as the reader thread would,
    /// stamped against the grid's CURRENT cursor/history state, so tests
    /// can stage prompt-dependent gestures without a live shell.
    #[cfg(test)]
    pub fn push_mark_for_test(&self, kind: crate::shell_integration::OscEvent, col_rec: usize) {
        let mut term = self.term.lock();
        let line_rec = term.grid().cursor.point.line.0;
        let clock_rec = self.clock.lock().unwrap().tick(&mut term);
        drop(term);
        self.marks.lock().unwrap().push(StoredMark {
            id: next_mark_id(),
            kind,
            line_rec,
            clock_rec,
            col_rec,
            dur: None,
        });
    }

    /// Test-only: record an inline image as the reader thread would,
    /// anchored at the grid's CURRENT cursor row.
    #[cfg(test)]
    pub fn push_image_for_test(&self, data: Vec<u8>) {
        let mut term = self.term.lock();
        let line_rec = term.grid().cursor.point.line.0;
        let clock_rec = self.clock.lock().unwrap().tick(&mut term);
        drop(term);
        let mut imgs = self.images.lock().unwrap();
        let seq = imgs.last().map_or(0, |m| m.seq) + 1;
        imgs.push(StoredImage {
            seq,
            data: std::sync::Arc::new(data),
            line_rec,
            clock_rec,
        });
    }

    /// Test-only: whether a quick-select hint overlay is currently set.
    #[cfg(test)]
    pub fn has_hints_for_test(&self) -> bool {
        self.hints.is_some()
    }

    /// Test-only: whether a find highlight needle is currently set.
    #[cfg(test)]
    pub fn has_search_for_test(&self) -> bool {
        self.search_needle.is_some()
    }

    /// Test-only: the raw state behind the corrected selection accessors —
    /// (stored selection, sel_scrolled, clock_base, alt anchor top, alt
    /// mode) — for diagnosing drift math from app-level tests.
    #[cfg(test)]
    pub fn debug_sel_state(&self) -> (Option<Selection>, i64, i64, Option<i32>, bool) {
        (
            self.selection,
            self.sel_scrolled,
            self.clock.lock().unwrap().base,
            self.alt_sel.as_ref().map(|a| a.top),
            self.term.lock().mode().contains(TermMode::ALT_SCREEN),
        )
    }

    /// The mouse button came up: the drag-selection is final. Capture the
    /// definitive content anchor now — during the drag the selection
    /// followed the pointer over whatever the grid showed.
    pub fn end_drag(&mut self) {
        if self.drag_selecting {
            self.drag_selecting = false;
            self.stamp_selection_clock();
        }
    }

    pub fn extend_selection_to(&mut self, col: u16, row: u16) {
        self.drag_selecting = true;
        self.rebase_selection();
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
        self.drag_selecting = true;
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
        self.rebase_selection();
        let off = self.display_offset();
        if let Some(sel) = self.selection.as_mut() {
            sel.head = (vp_row as i32 - off, c);
        }
        dir
    }

    /// One step of edge auto-scroll while a drag-selection is held past
    /// the top (`dir < 0`) or bottom (`dir > 0`) edge. Scrolls the
    /// viewport by one row and re-pins the selection head to the new edge
    /// line at the last-known drag column. On the alternate screen the
    /// inner program owns scrolling, so a mouse-tracking app (Claude Code,
    /// a pager) gets a wheel report at the edge cell instead — it scrolls
    /// its own content, the content anchor drags the selection along, and
    /// the head re-pins each tick.
    pub fn autoscroll_select(&mut self, dir: i32, col: u16) {
        if dir == 0 {
            return;
        }
        if self.term.lock().mode().contains(TermMode::ALT_SCREEN) {
            let inner = self.last_inner;
            if inner.width == 0 || inner.height == 0 || !self.mouse_reporting() {
                return;
            }
            let edge_row = if dir < 0 {
                inner.y
            } else {
                inner.y + inner.height - 1
            };
            let ccol = col.clamp(inner.x, inner.x + inner.width - 1);
            let button = if dir < 0 {
                MouseButtonKind::WheelUp
            } else {
                MouseButtonKind::WheelDown
            };
            self.report_mouse(
                button,
                MouseAction::Press,
                ccol,
                edge_row,
                MouseMods::default(),
            );
            self.rebase_selection();
            if let Some(sel) = self.selection.as_mut() {
                sel.head = (i32::from(edge_row - inner.y), ccol - inner.x);
            }
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
        self.rebase_selection();
        let off = self.display_offset();
        if let Some(sel) = self.selection.as_mut() {
            sel.head = (vp_row as i32 - off, c);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.alt_sel = None;
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
            block: false,
        });
        self.stamp_selection_clock();
    }

    /// The selection with its endpoints corrected for the scroll-clock
    /// movement since they were recorded, so callers always see coordinates
    /// that name the content the user selected (see `rebase_selection`).
    pub fn selection(&self) -> Option<Selection> {
        let mut sel = self.selection?;
        let delta = (self.clock_now(&self.term.lock()) - self.sel_scrolled) as i32;
        sel.anchor.0 -= delta;
        sel.head.0 -= delta;
        Some(sel)
    }

    /// Replace (or clear) the selection wholesale. Copy mode drives the
    /// selection from the keyboard through this, bypassing the mouse path.
    pub fn set_selection(&mut self, sel: Option<Selection>) {
        self.rebase_selection();
        self.selection = sel;
        self.stamp_selection_clock();
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Show / move / hide the copy-mode cursor block.
    pub fn set_copy_cursor(&mut self, cur: Option<(i32, u16)>) {
        self.rebase_selection();
        self.copy_cursor = cur;
        self.stamp_selection_clock();
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// The shell cursor's cell in absolute grid coords (line can be
    /// negative only transiently; the live screen is 0-based): where copy
    /// mode starts.
    pub fn cursor_line_col(&self) -> (i32, u16) {
        let term = self.term.lock();
        let p = term.grid().cursor.point;
        (p.line.0, p.column.0 as u16)
    }

    /// The drift-corrected selection together with the scroll-clock reading
    /// it is valid against, from ONE terminal snapshot. Callers that pair a
    /// grid coordinate with a clock (the annotation prompt) must use this:
    /// reading them through separate locks lets PTY output land in between,
    /// pairing a pre-output line with a post-output clock and anchoring to
    /// the wrong content row.
    pub fn selection_and_clock(&mut self) -> (Option<Selection>, i64) {
        let mut term = self.term.lock();
        let now = self.clock.lock().unwrap().tick(&mut term);
        let sel = self.selection.map(|mut s| {
            let delta = (now - self.sel_scrolled) as i32;
            s.anchor.0 -= delta;
            s.head.0 -= delta;
            s
        });
        (sel, now)
    }

    /// Cursor cell, grid bounds, and the scroll-clock reading from ONE
    /// terminal snapshot (copy mode's open — same atomicity argument as
    /// [`Self::selection_and_clock`]).
    #[allow(clippy::type_complexity)]
    pub fn cursor_bounds_and_clock(&mut self) -> ((i32, u16), (i32, i32, u16, u16), i64) {
        let mut term = self.term.lock();
        let now = self.clock.lock().unwrap().tick(&mut term);
        let p = term.grid().cursor.point;
        let top = term.grid().topmost_line().0;
        let bottom = term.screen_lines() as i32 - 1;
        (
            (p.line.0, p.column.0 as u16),
            (top, bottom, self.cols, self.rows),
            now,
        )
    }

    /// One grid row as spacer-skipped text plus its char-index → grid-column
    /// map (the free `row_text_and_cols` behind the term lock).
    pub fn row_text(&self, line: i32) -> (String, Vec<usize>) {
        let term = self.term.lock();
        row_text_and_cols(&term, line)
    }

    /// The readable grid range and viewport size for keyboard navigation:
    /// (oldest line, newest line, columns, viewport rows).
    pub fn grid_bounds(&self) -> (i32, i32, u16, u16) {
        let term = self.term.lock();
        let top = term.grid().topmost_line().0;
        let bottom = term.screen_lines() as i32 - 1;
        (top, bottom, self.cols, self.rows)
    }

    pub fn selection_text(&self) -> String {
        // An alt-screen selection scrolled (partly) out of the app's view
        // names content the grid no longer fully holds — dormant, or
        // clipped by app chrome. The anchor remembered the text, so copy
        // still yields the whole highlighted block.
        if let Some(anchor) = self
            .alt_sel
            .as_ref()
            .filter(|a| a.dormant || a.visible.is_some())
        {
            return anchor.text.clone();
        }
        let Some(mut sel) = self.selection else {
            return String::new();
        };
        let term = self.term.lock();
        // Same scroll-clock correction as `selection()`, computed under
        // the lock already held for extraction.
        let delta = (self.clock_now(&term) - self.sel_scrolled) as i32;
        sel.anchor.0 -= delta;
        sel.head.0 -= delta;
        if sel.block {
            let (rl, cl, rh, ch) = sel.block_bounds();
            return block_selection_text(&term, rl, cl as usize, rh, ch as usize);
        }
        let (sr, sc, er, ec) = sel.normalised();
        extract_selection_text(&term, sr, sc as usize, er, ec as usize)
    }

    /// The pane's stable identity (see the `uid` field).
    pub fn uid(&self) -> u64 {
        self.uid
    }

    pub fn write_input(&mut self, data: &[u8]) {
        self.reset_scrollback();
        self.input_seen = true;
        #[cfg(test)]
        self.written_for_test
            .lock()
            .unwrap()
            .extend_from_slice(data);
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(data);
            let _ = w.flush();
        }
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Test-only: every byte the app has written toward the child's stdin.
    #[cfg(test)]
    pub fn written_bytes_for_test(&self) -> Vec<u8> {
        self.written_for_test.lock().unwrap().clone()
    }

    /// True when the child program has enabled any mouse-tracking mode
    /// (DECSET 1000 click / 1002 button-drag / 1003 any-motion). When it has,
    /// wheel events should be forwarded to it via `report_mouse` so it scrolls
    /// its own buffer rather than croft synthesising arrow keys.
    pub fn mouse_reporting(&self) -> bool {
        self.term.lock().mode().intersects(TermMode::MOUSE_MODE)
    }

    /// Encode a mouse gesture at host-screen cell `(col, row)` as a mouse
    /// report and send it to the child. Returns false without writing when
    /// the cell is outside the pane, the child isn't tracking the mouse, or
    /// the event is motion the child didn't ask for (no 1002/1003). The report
    /// is SGR (1006) when the child selected it, otherwise legacy X10.
    pub fn report_mouse(
        &mut self,
        button: MouseButtonKind,
        action: MouseAction,
        col: u16,
        row: u16,
        mods: MouseMods,
    ) -> bool {
        let Some((r, c)) = self.cell_at(col, row) else {
            return false;
        };
        let mode = *self.term.lock().mode();
        if !mode.intersects(TermMode::MOUSE_MODE) {
            return false;
        }
        // Motion (a held-button drag) is only wanted under 1002/1003.
        if action == MouseAction::Motion
            && !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
        {
            return false;
        }
        let report = encode_mouse_report(
            mode.contains(TermMode::SGR_MOUSE),
            button,
            action,
            c,
            r,
            mods,
        );
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(&report);
            let _ = w.flush();
        }
        self.input_seen = true;
        self.pty_dirty.store(true, Ordering::Release);
        true
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
            _ if self.input_seen => {
                // A pane that has received input CAN be running a launched
                // app, so an UNRESOLVABLE sample (failed tcgetpgrp, or a
                // spawn that never reported a shell pid) must not read as
                // "shell owns it" — under scheduler load the sample
                // transiently fails while a foreground command runs, and
                // the old blanket `true` let a cd seed reach that app
                // (#155). Retry once (the failure window is brief), then
                // answer FALSE: every consumer fails safe on false (seed
                // suppressed, task pane not reused, no prompt arrows).
                std::thread::sleep(std::time::Duration::from_millis(5));
                match (self.master.process_group_leader(), self.shell_pid) {
                    (Some(fg), Some(pid)) => fg == pid,
                    _ => false,
                }
            }
            // Startup window: no input has ever been written, so nothing
            // user-facing can own the tty yet — a missing group is the
            // shell's own rc still claiming it, and a seed queues as
            // type-ahead (#94).
            _ => true,
        }
    }

    /// True when seeding a `cd` into the PTY cannot reach a user-facing
    /// foreground application. Either the shell owns the foreground group
    /// (`foreground_is_shell`), or the pane is still in its startup
    /// window: nothing has ever been written toward the child's stdin and
    /// shell integration has recorded no prompt mark, so a foreground
    /// group that differs from the shell can only be the shell's own rc
    /// startup. A seed written then is ordinary type-ahead — the bytes
    /// wait in the tty input queue for the first prompt — because a pane
    /// that has never received input cannot be running a launched app.
    /// Without the carve-out, a Make Root landing inside that window
    /// silently skipped the seed and left the shell behind the Explorer
    /// (#94; under full-suite load the window stretches past the call).
    pub fn cwd_seed_is_safe(&self) -> bool {
        self.foreground_is_shell() || (!self.input_seen && self.marks.lock().unwrap().is_empty())
    }

    /// Test-only: the RESOLVED foreground sample — `Some(fg == shell)`
    /// when both sides are known, `None` otherwise. `foreground_is_shell`
    /// folds policy fallbacks (the startup carve-out, the fail-closed
    /// retry) into its answer, so a test precondition polling it can be
    /// satisfied by a fallback and race the state it believes it proved
    /// (#186); gate preconditions on `Some(..)` instead.
    #[cfg(test)]
    pub fn foreground_resolved_for_test(&self) -> Option<bool> {
        match (self.master.process_group_leader(), self.shell_pid) {
            (Some(fg), Some(pid)) => Some(fg == pid),
            _ => None,
        }
    }

    /// True while the pane is exactly as croft created it: an interactive
    /// shell (never a `new_running` program pane, whose child is doing
    /// launched work with no input byte ever written) that has received
    /// no input and carries no manual name — nothing the user could miss
    /// if the pane were replaced. The re-root restore (#137) may swap
    /// only such panes for the incoming workspace's saved layout; any
    /// input (a keystroke, a paste, a seeded `cd`) or a rename clears
    /// this permanently.
    pub fn is_pristine(&self) -> bool {
        !self.run_pane && !self.input_seen && !self.manual_name_seen
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

    /// Jump the viewport to the oldest scrollback line (Shift+Home). No-op
    /// in the alternate screen, which has no scrollback.
    pub fn scroll_to_top(&mut self) -> bool {
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        term.scroll_display(Scroll::Top);
        drop(term);
        self.pty_dirty.store(true, Ordering::Release);
        true
    }

    /// Jump the viewport back to the live bottom (Shift+End). Returns false
    /// in the alternate screen — no scrollback there, so the chord belongs
    /// to the program, exactly like its three scrolling siblings.
    pub fn reset_scrollback(&mut self) -> bool {
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        term.scroll_display(Scroll::Bottom);
        self.pty_dirty.store(true, Ordering::Release);
        true
    }

    /// Clear the visible screen and scrollback history (VS Code's terminal
    /// "Clear"), homing the cursor. Feeds the standard erase sequences into the
    /// grid (`ED 3` wipes scrollback, `ED 2` the screen); the shell redraws its
    /// prompt on the next keystroke. Does not touch the running program.
    pub fn clear_screen_and_scrollback(&mut self) {
        let mut processor = Processor::<StdSyncHandler>::new();
        {
            let mut term = self.term.lock();
            // ED 2 BEFORE ED 3 (xterm.js / VS Code's clear() order):
            // alacritty's primary-screen ED 2 scrolls the viewport INTO
            // history rather than blanking in place, so erasing history
            // first would leave the "cleared" rows alive in scrollback.
            processor.advance(&mut *term, b"\x1b[2J\x1b[3J\x1b[H");
            term.scroll_display(Scroll::Bottom);
        }
        // The content the images and OSC 133 marks anchored to is gone;
        // keeping either leaves overlays (a floating picture, phantom
        // decoration dots whose menus copy or re-run the wrong rows) over
        // the fresh prompt.
        self.images.lock().unwrap().clear();
        self.marks.lock().unwrap().clear();
        self.pty_dirty.store(true, Ordering::Release);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        // A degenerate pane (a host window shrunk to a sliver leaves the
        // layout no room) must not reach alacritty: Term::resize panics on
        // a zero/one-cell grid mid-reflow. Keep the last sane size; the
        // grid simply clips until the window grows back.
        let (cols, rows) = (cols.max(2), rows.max(2));
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

/// Block until the pty has data (or EOF/error — the following `read` then
/// observes it), or until the shutdown pipe signals (its write end closed in
/// `Drop`). Returns false on shutdown. A bare blocking `read` is not enough
/// to ever exit: on Linux a background job keeps the pty slave open after
/// the shell dies, so the read never returns and joining the reader would
/// freeze the UI for as long as the job runs.
fn wait_pty_readable(pty_fd: std::os::fd::RawFd, shutdown: &std::io::PipeReader) -> bool {
    use std::os::fd::AsRawFd;
    let mut fds = [
        libc::pollfd {
            fd: pty_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: shutdown.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        if fds[1].revents != 0 {
            return false;
        }
        if fds[0].revents != 0 {
            return true;
        }
    }
}

impl Drop for PtyTerminal {
    fn drop(&mut self) {
        // Kill the shell AND reap it: `kill()` reaps only when the shell dies
        // inside its SIGHUP grace loop; the SIGKILL escalation (a shell that
        // traps HUP) never waits, and `Child`'s own drop doesn't either, so
        // without the `wait` every such closed pane left a zombie for the
        // life of the process. The responder thread ends on its own once
        // `self.term` (holding its channel sender) drops with this struct.
        let _ = self._child.kill();
        let _ = self._child.wait();
        // Wake the reader (POLLHUP on its shutdown fd) and join it. EOF alone
        // is not a reliable wake: see `wait_pty_readable`.
        drop(self.reader_shutdown.take());
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
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
/// One grid row as text plus a char-index → grid-column map. A wide char
/// (CJK, emoji) occupies two grid columns — the `WIDE_CHAR` cell and a
/// spacer cell — so the spacers are skipped, making the text read
/// contiguously (`"日本語"`, never `"日 本 語"`). `cols[i]` is the grid
/// column the i-th char starts at, so highlight painters can map a match's
/// char range back onto grid cells.
pub fn row_text_and_cols(term: &Term<VoidListener>, line_idx: i32) -> (String, Vec<usize>) {
    let ncols = term.columns();
    let mut s = String::with_capacity(ncols);
    let mut cols = Vec::with_capacity(ncols);
    for c in 0..ncols {
        let cell = &term.grid()[Point::new(Line(line_idx), Column(c))];
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        s.push(if cell.c == '\0' { ' ' } else { cell.c });
        cols.push(c);
    }
    (s, cols)
}

/// The typed text of the command now ending: from the newest PromptEnd
/// (`133;B`) cell to the line before the newest CommandStart (`133;C`) —
/// the same span `pair_decorations` derives after the fact. Runs in the
/// reader thread at the `133;D` mark with the term lock held; empty when
/// the B/C marks are missing (no shell integration on this pane).
fn last_command_input_text(
    term: &Term<VoidListener>,
    marks: &[StoredMark],
    clock_now: i64,
) -> String {
    use crate::shell_integration::OscEvent as E;
    let cur = |m: &StoredMark| m.line_rec - (clock_now - m.clock_rec) as i32;
    let Some(ci) = marks
        .iter()
        .rposition(|m| matches!(m.kind, E::CommandStart))
    else {
        return String::new();
    };
    let Some(b) = marks[..ci]
        .iter()
        .rev()
        .find(|m| matches!(m.kind, E::PromptEnd))
    else {
        return String::new();
    };
    let bl = cur(b);
    let cl = cur(&marks[ci]);
    if cl <= bl {
        return String::new();
    }
    extract_selection_text(
        term,
        bl,
        b.col_rec,
        cl - 1,
        term.columns().saturating_sub(1),
    )
}

/// The finished command's OUTPUT: the rows between the newest `133;C` mark
/// and the row the cursor sits on as its `133;D` arrives (exclusive — that
/// row is the D mark's own). Escape-free (the grid never stores escapes),
/// tail-capped at [`FINISHED_OUTPUT_CAP_LINES`].
fn last_command_output_text(
    term: &Term<VoidListener>,
    marks: &[StoredMark],
    clock_now: i64,
) -> String {
    use crate::shell_integration::OscEvent as E;
    let Some(c) = marks
        .iter()
        .rev()
        .find(|m| matches!(m.kind, E::CommandStart))
    else {
        return String::new();
    };
    // The C mark was recorded when the command STARTED: everything the
    // command printed has scrolled the grid since, so its recorded line is
    // re-based through the scroll clock exactly like the input extractor.
    // The mark's own row is the FIRST output row — `133;C` is emitted right
    // after the command's newline, before the program prints anything.
    let start = c.line_rec - (clock_now - c.clock_rec) as i32;
    // The cursor row is INCLUDED: `133;D` moves no cursor, so a diagnostic
    // printed without a trailing newline leaves the mark ON that row — the
    // old `- 1` dropped exactly the line the matchers needed. With a
    // trailing newline the cursor row is a fresh blank, trimmed below.
    let end = term.grid().cursor.point.line.0;
    if end < start {
        return String::new();
    }
    let text = extract_selection_text(term, start, 0, end, term.columns().saturating_sub(1));
    let text = text.trim_end_matches('\n');
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > FINISHED_OUTPUT_CAP_LINES {
        lines[lines.len() - FINISHED_OUTPUT_CAP_LINES..].join("\n")
    } else {
        text.to_string()
    }
}

/// Epoch millis → local wall-clock `HH:MM:SS` (libc localtime, the same
/// no-date-crate route the trash metadata writer takes).
fn hhmmss(millis: u64) -> String {
    // `localtime_r` takes a `*const time_t`, so the binding's own definition is
    // the only correct type here. musl's is mid-migration to 64-bit and libc
    // marks the alias deprecated ahead of the change, which `-D warnings`
    // turns into a build error on the Linux targets; naming the field type
    // through `tm` is not possible (it has no `time_t` member), so the alias
    // stays and the deprecation is acknowledged at this one call.
    #[allow(deprecated)]
    let secs = (millis / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return String::from("--:--:--");
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// The surviving column spans of a block row that app chrome partially
/// overdrew: the longest common prefix and suffix between the remembered
/// row and what the grid shows. Returns the grid-column ranges to keep
/// highlighted — a pill floating over the middle of a row leaves BOTH
/// intact ends lit.
fn partial_overlap_cols(want: &str, got: &str, colmap: &[usize]) -> Vec<(u16, u16)> {
    // Row text is full grid width: strip the trailing blank run or it
    // counts as a huge shared suffix between any two rows.
    let want = want.trim_end();
    let got = got.trim_end();
    let want_n = want.chars().count();
    let got_n = got.chars().count();
    if want_n == 0 || got_n == 0 {
        return Vec::new();
    }
    let prefix = want
        .chars()
        .zip(got.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = want
        .chars()
        .rev()
        .zip(got.chars().rev())
        .take_while(|(a, b)| a == b)
        .count();
    // Each surviving end must carry real content, not just indentation.
    // No minimum share of the row: a pill over the MIDDLE leaves both
    // ends under half, and this credit is paint-only — the shift was
    // already anchored by exact rows, so it can't move the highlight.
    let solid =
        |chars: &mut dyn Iterator<Item = char>| chars.filter(|c| !c.is_whitespace()).count() >= 4;
    let good_prefix = prefix > 0 && solid(&mut want.chars().take(prefix));
    let good_suffix = suffix > 0 && solid(&mut want.chars().rev().take(suffix));
    let mut spans = Vec::new();
    if good_prefix && good_suffix && prefix + suffix >= got_n {
        // The ends overlap: the row differs by a few mid-row cells (an
        // animated counter ticking inside the selection) — keep it whole.
        spans.push((colmap[0] as u16, colmap[got_n - 1] as u16));
    } else {
        if good_prefix {
            spans.push((colmap[0] as u16, colmap[prefix - 1] as u16));
        }
        if good_suffix {
            spans.push((colmap[got_n - suffix] as u16, colmap[got_n - 1] as u16));
        }
    }
    spans
}

/// Capture the content anchor for an alternate-screen selection: its row
/// text and extracted text as the grid shows them now. `None` when any
/// selected line is outside the grid (a partially scrolled-out block keeps
/// its previous, fuller anchor instead).
fn capture_alt_anchor(term: &Term<VoidListener>, sel: Selection) -> Option<AltSelAnchor> {
    let rows_vis = term.screen_lines() as i32;
    let (lo, hi) = (sel.anchor.0.min(sel.head.0), sel.anchor.0.max(sel.head.0));
    if lo < 0 || hi >= rows_vis {
        return None;
    }
    let rows = (lo..=hi).map(|l| row_text_and_cols(term, l).0).collect();
    let ctx_above = (lo > 0).then(|| row_text_and_cols(term, lo - 1).0);
    let ctx_below = (hi + 1 < rows_vis).then(|| row_text_and_cols(term, hi + 1).0);
    let text = if sel.block {
        let (rl, cl, rh, ch) = sel.block_bounds();
        block_selection_text(term, rl, cl as usize, rh, ch as usize)
    } else {
        let (sr, sc, er, ec) = sel.normalised();
        extract_selection_text(term, sr, sc as usize, er, ec as usize)
    };
    Some(AltSelAnchor {
        rows,
        top: lo,
        text,
        dormant: false,
        visible: None,
        ctx_above,
        ctx_below,
    })
}

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
            // A wide char's spacer cell holds no glyph of its own — skip it
            // so copied CJK/emoji text comes out contiguous.
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
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
            // A soft-wrapped row (WRAPLINE on its last cell) continues the
            // same logical line: no separator, so copied text, the durable
            // command history, and the sticky header all see the line the
            // user actually typed — a '\n' here corrupted stored commands
            // and re-ran only their first fragment on paste. A hard row
            // break keeps the newline.
            let wrapped = cols > 0
                && term.grid()[Point::new(Line(line_idx), Column(cols - 1))]
                    .flags
                    .contains(Flags::WRAPLINE);
            if !wrapped {
                out.push('\n');
            }
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

pub fn is_terminal_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Encode a mouse event as the byte sequence a child program expects.
/// `sgr` selects SGR (1006) encoding over legacy X10. `local_col`/`local_row`
/// are 0-based cells within the pane; the wire protocol is 1-based, so both
/// are offset by one. Button/modifier bit layout follows xterm: Left/Middle/
/// Right = 0/1/2, wheel up/down = 64/65, +32 for motion, +4/+8/+16 for
/// Shift/Alt/Ctrl. Under X10 a release reports button code 3 and coordinates
/// clamp to 223 (the largest a single 0x20-based byte can carry).
pub fn encode_mouse_report(
    sgr: bool,
    button: MouseButtonKind,
    action: MouseAction,
    local_col: u16,
    local_row: u16,
    mods: MouseMods,
) -> Vec<u8> {
    let mut cb: u8 = match button {
        MouseButtonKind::Left => 0,
        MouseButtonKind::Middle => 1,
        MouseButtonKind::Right => 2,
        MouseButtonKind::WheelUp => 64,
        MouseButtonKind::WheelDown => 65,
    };
    if action == MouseAction::Motion {
        cb += 32;
    }
    if mods.shift {
        cb += 4;
    }
    if mods.alt {
        cb += 8;
    }
    if mods.ctrl {
        cb += 16;
    }
    let cx = local_col + 1;
    let cy = local_row + 1;
    if sgr {
        let terminator = if action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        format!("\x1b[<{cb};{cx};{cy}{terminator}").into_bytes()
    } else {
        if action == MouseAction::Release {
            cb = (cb & !0b11) | 0b11;
        }
        let enc = |v: u16| (v.min(223) as u8).wrapping_add(32);
        vec![0x1b, b'[', b'M', cb.wrapping_add(32), enc(cx), enc(cy)]
    }
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

/// Map an alacritty cell color to ratatui through the theme's 16-color ANSI
/// palette: Named colors and Indexed 0-15 resolve to the palette's RGB (so a
/// pane renders the same on every host terminal, the way VS Code owns its
/// terminal palette); true-color cells pass through; higher indexes keep the
/// standard 256-color cube the host renders.
fn ansi_to_ratatui(c: AnsiColor, palette: &[(u8, u8, u8); 16]) -> Option<Color> {
    match c {
        AnsiColor::Spec(rgb) => Some(Color::Rgb(rgb.r, rgb.g, rgb.b)),
        AnsiColor::Indexed(i) if i < 16 => Some(pal(palette, i as usize)),
        AnsiColor::Indexed(i) => Some(Color::Indexed(i)),
        AnsiColor::Named(named) => named_to_ratatui(named, palette),
    }
}

fn pal(palette: &[(u8, u8, u8); 16], i: usize) -> Color {
    let (r, g, b) = palette[i];
    Color::Rgb(r, g, b)
}

fn named_to_ratatui(n: NamedColor, palette: &[(u8, u8, u8); 16]) -> Option<Color> {
    use NamedColor::*;
    match n {
        Foreground | Background | Cursor | DimForeground => None,
        Black | DimBlack => Some(pal(palette, 0)),
        Red | DimRed => Some(pal(palette, 1)),
        Green | DimGreen => Some(pal(palette, 2)),
        Yellow | DimYellow => Some(pal(palette, 3)),
        Blue | DimBlue => Some(pal(palette, 4)),
        Magenta | DimMagenta => Some(pal(palette, 5)),
        Cyan | DimCyan => Some(pal(palette, 6)),
        White | DimWhite => Some(pal(palette, 7)),
        BrightBlack => Some(pal(palette, 8)),
        BrightRed => Some(pal(palette, 9)),
        BrightGreen => Some(pal(palette, 10)),
        BrightYellow => Some(pal(palette, 11)),
        BrightBlue => Some(pal(palette, 12)),
        BrightMagenta => Some(pal(palette, 13)),
        BrightCyan => Some(pal(palette, 14)),
        BrightWhite => Some(pal(palette, 15)),
        BrightForeground => None,
    }
}

impl Widget for &mut PtyTerminal {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // A host-accent rule outranks the focus color: a production pane
        // must read as dangerous whether or not it holds focus.
        let block_style = if let Some((r, g, b)) = self.accent {
            Style::default().fg(Color::Rgb(r, g, b))
        } else if self.focused {
            Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        // The panel group's tab strip already labels this region "TERMINAL", so
        // each pane stays titleless — a bare bordered box, matching VS Code's
        // unlabelled split terminal panes. The border alone frames the pane.
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style);
        let inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the orange→green
        // gradient (matching the welcome activity box).
        if self.focused && self.focus_gradient && self.accent.is_none() {
            crate::gradient::paint_gradient_box(buf, area);
        }
        self.last_area = area;
        self.last_inner = inner;
        // Re-anchor the selection / copy-cursor to the content they were
        // made on before painting, so the highlight scrolls WITH streaming
        // output instead of sitting at fixed screen rows while text slides
        // underneath (the Claude Code drag-select bug).
        self.rebase_selection();
        // (is_block, bounds): block selections rectangle-test each cell,
        // linear ones use the row-major span. A dormant alt-screen
        // selection paints nothing — its content is scrolled out of the
        // app's view and the frozen coordinates sit over unrelated text; a
        // partially covered one paints only its surviving rows.
        let dormant = self.alt_sel.as_ref().is_some_and(|a| a.dormant);
        let sel_clip = self.alt_sel.as_ref().and_then(|a| a.visible.clone());
        let sel_paint = self.selection.filter(|_| !dormant).map(|s| {
            if s.block {
                (true, s.block_bounds())
            } else {
                (false, s.normalised())
            }
        });

        let cols = inner.width;
        let rows = inner.height;
        self.resize(cols, rows);

        // Snapshot before taking the term lock: command_decorations locks
        // the term itself and FairMutex is not reentrant.
        let decorations = self.command_decorations();
        // Trigger set snapshot for the highlight pass (cheap Arc clone).
        let trigger_set = self.triggers.lock().unwrap().clone();

        let term = self.term.lock();
        // Annotation spans at their current grid lines, translated by the
        // scroll clock (same anchor as selections — history growth
        // saturates and froze these in long-lived panes). None while an
        // alt-screen app owns the viewport: the spans name primary-screen
        // rows, and painting them over vim/htop content is chart junk.
        let ann_spans: Vec<(i32, u16, u16)> = if term.mode().contains(TermMode::ALT_SCREEN) {
            Vec::new()
        } else {
            let ann_now = self.clock_now(&term);
            self.annotations
                .iter()
                .map(|a| (a.line_rec - (ann_now - a.clock_rec) as i32, a.start, a.len))
                .collect()
        };
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

        // Quick-select hint drift: lines were captured at the set's clock
        // reading; re-base them to now so labels follow their matches
        // through streaming output (the annotation translation, one read
        // per frame).
        let hint_drift: i32 = self
            .hints
            .as_ref()
            .map_or(0, |(c0, _)| (self.clock_now(&term) - c0) as i32);

        // The active find match, re-based to the current clock the same way
        // (its stored line drifts by exactly the rows scrolled since it was
        // anchored, so the bright cell stays glued to its text).
        let active_match: Option<(i32, usize, usize)> = self
            .current_match
            .map(|(c0, l, c, n)| (l - (self.clock_now(&term) - c0) as i32, c, n));

        let reveal_redactions = self.reveal_redactions;
        let mut redacted_spans = 0usize;
        for y in 0..rows {
            // Find matches on this row once, then paint them per cell below.
            // Match positions are char indices in the spacer-skipped row text
            // (the same text the find bar searched); the colmap translates
            // them back to grid columns. 0 = no match, 1 = match, 2 = active.
            let row_line_idx = (y as i32) - (display_offset as i32);
            let row_paint: Option<Vec<u8>> = self.search_needle.as_deref().map(|needle| {
                let (text, colmap) = row_text_and_cols(&term, row_line_idx);
                let mut paint = vec![0u8; cols as usize];
                for (mc, ml) in
                    crate::widgets::editor_find::line_matches(&text, self.search_opts, needle)
                {
                    let active = active_match == Some((row_line_idx, mc, ml));
                    for k in mc..mc + ml {
                        if let Some(&col) = colmap.get(k) {
                            paint[col] = if active { 2 } else { 1 };
                        }
                    }
                }
                paint
            });
            // Quick-select hints on this row: 1 = match span, 2 = label cell
            // (the char to draw rides in the parallel vec). Char indices go
            // through the same colmap as the find highlight.
            let hint_paint: Option<(Vec<u8>, Vec<char>)> =
                self.hints.as_ref().and_then(|(_, hints)| {
                    let row_hints: Vec<&HintSpan> = hints
                        .iter()
                        .filter(|h| h.line - hint_drift == row_line_idx)
                        .collect();
                    if row_hints.is_empty() {
                        return None;
                    }
                    let (_text, colmap) = row_text_and_cols(&term, row_line_idx);
                    let mut span = vec![0u8; cols as usize];
                    let mut label = vec!['\0'; cols as usize];
                    for h in row_hints {
                        for k in h.start..h.start + h.len {
                            if let Some(&col) = colmap.get(k) {
                                span[col] = 1;
                            }
                        }
                        for (j, c) in h.label.chars().skip(h.typed).enumerate() {
                            if let Some(&col) = colmap.get(h.start + j) {
                                span[col] = 2;
                                label[col] = c;
                            }
                        }
                    }
                    Some((span, label))
                });
            // Trigger highlights on this row: per-cell fg/bg from the first
            // matching highlight trigger. Painted under the find highlight
            // and quick-select labels, which both win.
            let trig_paint: Option<TrigRowPaint> = trigger_set.has_highlights().then(|| {
                let (text, colmap) = row_text_and_cols(&term, row_line_idx);
                let mut paint: TrigRowPaint = vec![None; cols as usize];
                for s in crate::triggers::highlight_spans(&text, &trigger_set) {
                    let fg = s.fg.map(|(r, g, b)| Color::Rgb(r, g, b));
                    let bg = s.bg.map(|(r, g, b)| Color::Rgb(r, g, b));
                    for k in s.start..s.start + s.len {
                        if let Some(&col) = colmap.get(k) {
                            paint[col] = Some(TrigCell {
                                fg,
                                bg,
                                mask: false,
                            });
                        }
                    }
                }
                // Redaction (#360) is paint-only too: the grid keeps the
                // real text, the cell shows a mask glyph. Counted per span
                // for the status chip; nothing is masked while revealing.
                if !reveal_redactions {
                    for s in crate::triggers::redact_spans(&text, &trigger_set) {
                        redacted_spans += 1;
                        for k in s.start..s.start + s.len {
                            if let Some(&col) = colmap.get(k) {
                                let cell = paint[col].get_or_insert(TrigCell {
                                    fg: None,
                                    bg: None,
                                    mask: false,
                                });
                                cell.mask = true;
                            }
                        }
                    }
                }
                paint
            });
            for x in 0..cols {
                let line_idx = (y as i32) - (display_offset as i32);
                let p = Point::new(Line(line_idx), Column(x as usize));
                let cell = &term.grid()[p];
                let mut display_char = if cell.c == '\0' { ' ' } else { cell.c };
                let mut style = Style::default();
                if let Some(c) = ansi_to_ratatui(cell.fg, &self.palette) {
                    style = style.fg(c);
                }
                if let Some(c) = ansi_to_ratatui(cell.bg, &self.palette) {
                    style = style.bg(c);
                }
                let flags = cell.flags;
                if flags.contains(Flags::BOLD) {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if flags.contains(Flags::ITALIC) {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                // SGR 2 (faint): Claude Code's ghost suggestions and other
                // TUIs de-emphasise text with this; dropping it makes the
                // suggestion read as typed input.
                if flags.contains(Flags::DIM) {
                    style = style.add_modifier(Modifier::DIM);
                }
                if flags.intersects(Flags::ALL_UNDERLINES) {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if flags.contains(Flags::INVERSE) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                // Trigger highlight: the user's per-trigger colours over the
                // matched span. Cursor / selection / find / quick-select all
                // paint after this, so they stay visible on top.
                if let Some(paint) = trig_paint.as_ref()
                    && let Some(Some(cell)) = paint.get(x as usize)
                {
                    if let Some(c) = cell.fg {
                        style = style.fg(c);
                    }
                    if let Some(c) = cell.bg {
                        style = style.bg(c);
                    }
                    if cell.mask {
                        display_char = crate::triggers::MASK;
                    }
                }
                // Annotated spans: amber + underline, under the cursor /
                // selection / find layers so those still win.
                if ann_spans
                    .iter()
                    .any(|&(l, s0, ln)| l == line_idx && x >= s0 && x < s0 + ln)
                {
                    style = style
                        .fg(self.theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b)))
                        .add_modifier(Modifier::UNDERLINED);
                }
                if cursor_visible
                    && (y as i32) == cursor_row_in_viewport
                    && (x as i32) == cursor_col_in_viewport
                {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                if let Some((block, (sr, sc, er, ec))) = sel_paint
                    && sel_clip.as_ref().is_none_or(|clips| {
                        clips
                            .iter()
                            .any(|&(l, lo, hi)| l == line_idx && x >= lo && x <= hi)
                    })
                    && if block {
                        cell_in_block_selection(line_idx, x, sr, sc, er, ec)
                    } else {
                        cell_in_selection(line_idx, x, sr, sc, er, ec)
                    }
                {
                    style = style.bg(self.theme.ui(Color::Rgb(0x26, 0x4f, 0x78)));
                }
                // Find highlight: muted amber on every occurrence, bright
                // orange on the active match (VS Code's find colours).
                if let Some(paint) = row_paint.as_ref() {
                    match paint.get(x as usize) {
                        Some(1) => {
                            style = style
                                .fg(Color::Black)
                                .bg(self.theme.ui(Color::Rgb(0xff, 0xd7, 0x4a)))
                                .add_modifier(Modifier::BOLD);
                        }
                        Some(2) => {
                            style = style
                                .fg(Color::Black)
                                .bg(self.theme.ui(Color::Rgb(0xff, 0x8c, 0x2a)))
                                .add_modifier(Modifier::BOLD);
                        }
                        _ => {}
                    }
                }
                // Quick-select: matched spans turn green, label cells overlay
                // black-on-gold and replace the glyph underneath (WezTerm's
                // quick-select colours).
                if let Some((span, labels)) = hint_paint.as_ref() {
                    match span.get(x as usize) {
                        Some(1) => {
                            style = style
                                .fg(self.theme.ui(Color::Rgb(0x66, 0xcc, 0x66)))
                                .add_modifier(Modifier::BOLD);
                        }
                        Some(2) => {
                            if let Some(&lc) = labels.get(x as usize)
                                && lc != '\0'
                            {
                                display_char = lc;
                            }
                            style = Style::default()
                                .fg(Color::Black)
                                .bg(self.theme.ui(Color::Rgb(0xff, 0xd7, 0x4a)))
                                .add_modifier(Modifier::BOLD);
                        }
                        _ => {}
                    }
                }
                // Copy-mode cursor: a green modal block, painted last so the
                // caret stays visible over selection / find / trigger paint.
                if let Some((cl, cc)) = self.copy_cursor
                    && line_idx == cl
                    && x == cc
                {
                    style = Style::default()
                        .fg(Color::Black)
                        .bg(self.theme.ui(Color::Rgb(0x66, 0xcc, 0x66)))
                        .add_modifier(Modifier::BOLD);
                }
                let target_x = inner.x + x;
                let target_y = inner.y + y;
                let target = &mut buf[(target_x, target_y)];
                let mut tmp = [0u8; 4];
                target.set_symbol(display_char.encode_utf8(&mut tmp));
                target.set_style(style);
            }
        }
        self.redacted_on_screen = redacted_spans;
        // OSC 9;4 progress gauge along the bottom border (Ghostty/WezTerm
        // parity): a fill in the state's colour over the border glyphs —
        // blue normal, red error, yellow warning — and a sweeping segment
        // while indeterminate. Border cells are croft chrome, so no program
        // content is ever covered (same contract as the decoration dots).
        if let Some((state, pct)) = *self.progress.lock().unwrap()
            && area.height >= 2
            && inner.width > 0
        {
            let w = inner.width as u32;
            let (fill_from, fill_len) = if state == 3 {
                // Indeterminate: a ~fifth-width segment sweeping left to
                // right, phased off the wall clock so each frame advances.
                let seg = (w / 5).max(1);
                let span = (w - seg).max(1);
                let ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u32)
                    .unwrap_or(0);
                ((ms / 80) % span, seg)
            } else {
                (0, w * u32::from(pct.min(100)) / 100)
            };
            let color = match state {
                2 => self.theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c)),
                4 => self.theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b)),
                _ => self.theme.ui(Color::Rgb(0x1b, 0x81, 0xa8)),
            };
            let by = area.y + area.height - 1;
            for i in 0..fill_len.min(w) {
                let x = inner.x + (fill_from + i) as u16;
                let cell = &mut buf[(x, by)];
                cell.set_symbol("━");
                cell.set_style(Style::default().fg(color));
            }
        }
        // Timestamps gutter: the arrival clock of each stamped row hugs the
        // right edge (iTerm2's Show Timestamps), amber with a warning mark
        // when the row landed a long stall after its predecessor. Chrome
        // over content, painted only while the palette toggle is on.
        if self.show_timestamps && !alt_screen && inner.width > 14 {
            // Row ids are `clock + grid line` — content-stable through
            // scrollback saturation, matching the reader thread's stamps.
            // Clock before line_times: the shared lock order is
            // term → clock → line_times (see [`ScrollClock`]).
            let clock_now = self.clock_now(&term);
            let lt = self.line_times.lock().unwrap();
            for y in 0..rows {
                let abs = clock_now + (y as i32 - display_offset as i32) as i64;
                let Some(&ms) = lt.get(&abs) else { continue };
                let prev = lt.range(..abs).next_back().map(|(_, &v)| v);
                let stalled = prev.is_some_and(|p| ms.saturating_sub(p) >= STALL_GAP_MS);
                let mut text = hhmmss(ms);
                if stalled {
                    text = format!("\u{26a0} {text}");
                }
                let tw = text.chars().count() as u16;
                if tw >= inner.width {
                    continue;
                }
                let x0 = inner.x + inner.width - tw;
                let fg = if stalled {
                    self.theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b))
                } else {
                    self.theme.ui(Color::Rgb(0x5b, 0x64, 0x72))
                };
                for (j, c) in text.chars().enumerate() {
                    let cell = &mut buf[(x0 + j as u16, inner.y + y)];
                    let mut tmpc = [0u8; 4];
                    cell.set_symbol(c.encode_utf8(&mut tmpc));
                    cell.set_style(Style::default().fg(fg));
                }
            }
        }
        // Host-accent badge watermark: the rule's badge text, dim in the
        // accent color, parked at the pane's top-right (under the sticky
        // header, over content — it is a warning, that is the point).
        if let (Some((r, g, b)), Some(badge)) = (self.accent, self.accent_badge.as_deref())
            && !badge.is_empty()
        {
            let bw = badge.chars().count() as u16;
            if bw + 2 < inner.width {
                let x0 = inner.x + inner.width - bw - 1;
                for (j, c) in badge.chars().enumerate() {
                    let cell = &mut buf[(x0 + j as u16, inner.y)];
                    let mut tmpc = [0u8; 4];
                    cell.set_symbol(c.encode_utf8(&mut tmpc));
                    cell.set_style(
                        Style::default()
                            .fg(Color::Rgb(r, g, b))
                            .add_modifier(Modifier::DIM | Modifier::BOLD),
                    );
                }
            }
        }
        // Sticky command header (Warp): when the viewport is scrolled so its
        // top row falls inside one command's output while that command's
        // prompt sits above the view, the typed command pins to the pane's
        // top row with the scroll depth. Finished commands resolve through
        // the decoration spans; the still-running one through the newest
        // CommandStart mark. Unpins the instant the top row leaves the span.
        if display_offset > 0 && !alt_screen && rows > 0 && inner.width > 4 {
            let top_row = -(display_offset as i32);
            let cols_last = term.columns().saturating_sub(1);
            let header = decorations
                .iter()
                .find(|d| d.line < top_row && d.output_start <= top_row && top_row < d.output_end)
                .and_then(|d| {
                    let (l, c) = d.input?;
                    (d.output_start > l)
                        .then(|| extract_selection_text(&term, l, c, d.output_start - 1, cols_last))
                })
                .or_else(|| {
                    let ms = self.marks.lock().unwrap();
                    let running = ms.last().is_some_and(|m| {
                        matches!(m.kind, crate::shell_integration::OscEvent::CommandStart)
                    });
                    if !running {
                        return None;
                    }
                    let now = self.clock_now(&term);
                    let last = ms.last().unwrap();
                    let c_line = last.line_rec - (now - last.clock_rec) as i32;
                    (c_line < top_row).then(|| last_command_input_text(&term, &ms, now))
                })
                .filter(|t| !t.trim().is_empty());
            if let Some(text) = header {
                let bg = self.theme.ui(Color::Rgb(0x25, 0x2b, 0x36));
                for x in 0..inner.width {
                    let cell = &mut buf[(inner.x + x, inner.y)];
                    cell.set_symbol(" ");
                    cell.set_style(Style::default().bg(bg));
                }
                // A multi-row command (soft-wrapped or a quoted/heredoc
                // newline) arrives with its rows joined by '\n'; a cell must
                // never hold ANY control char, so the whole class maps to
                // spaces, not just the row join.
                let label: String = format!("\u{25b6} {}", text.trim())
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect();
                let mut xw = inner.x + 1;
                for ch in label.chars() {
                    if xw + 1 >= inner.x + inner.width {
                        break;
                    }
                    let cell = &mut buf[(xw, inner.y)];
                    let mut tmpc = [0u8; 4];
                    cell.set_symbol(ch.encode_utf8(&mut tmpc));
                    cell.set_style(
                        Style::default()
                            .fg(self.theme.ui(Color::Rgb(0xec, 0xf0, 0xf4)))
                            .bg(bg)
                            .add_modifier(Modifier::BOLD),
                    );
                    xw += 1;
                }
                let right = format!(" \u{2191} {display_offset} ");
                let rw = right.chars().count() as u16;
                if xw + rw < inner.x + inner.width {
                    let x0 = inner.x + inner.width - rw;
                    for (j, ch) in right.chars().enumerate() {
                        let cell = &mut buf[(x0 + j as u16, inner.y)];
                        let mut tmpc = [0u8; 4];
                        cell.set_symbol(ch.encode_utf8(&mut tmpc));
                        cell.set_style(
                            Style::default()
                                .fg(self.theme.ui(Color::Rgb(0x8b, 0x93, 0xa1)))
                                .bg(bg),
                        );
                    }
                }
            }
        }
        // Command decorations: a VS Code-style dot on the left border at
        // each finished command's prompt row — blue for success, red for a
        // non-zero exit. Normal screen only; an alt-screen app owns the
        // viewport and has no prompts.
        if !alt_screen {
            for d in &decorations {
                let vp = d.line + display_offset as i32;
                if (0..rows as i32).contains(&vp) {
                    let ok = d.exit.unwrap_or(0) == 0;
                    let cell = &mut buf[(area.x, inner.y + vp as u16)];
                    cell.set_symbol("●");
                    cell.set_style(Style::default().fg(if ok {
                        self.theme.ui(Color::Rgb(0x1b, 0x81, 0xa8))
                    } else {
                        self.theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c))
                    }));
                }
            }
        }
    }
}

/// Per-cell trigger paint for one row: `None` = cell untouched, `Some` =
/// the matching trigger's colours (either side optional, leaving that half
/// of the cell style alone) and whether a redact rule masks the glyph.
#[derive(Clone, Copy, Debug, Default)]
struct TrigCell {
    fg: Option<Color>,
    bg: Option<Color>,
    mask: bool,
}
type TrigRowPaint = Vec<Option<TrigCell>>;

/// The display offset that brings absolute grid line `abs_line` to the
/// vertical middle of a `rows`-tall viewport, clamped to `[0, max_off]` (0 =
/// live bottom, `max_off` = oldest scrollback). The render loop shows the
/// grid line `y - display_offset` at viewport row `y`, so centering the
/// target means `display_offset = rows/2 - abs_line`. Pure for testing.
pub fn scroll_offset_for_line(rows: i32, max_off: i32, abs_line: i32) -> i32 {
    (rows / 2 - abs_line).clamp(0, max_off.max(0))
}

/// The text a rectangular selection covers: the same inclusive column
/// slice `[cl..=ch]` from every row `[rl..=rh]`, one line per row (what
/// vim's Ctrl+V yank produces). Rows are clamped to the readable grid the
/// same way `extract_selection_text` clamps.
pub fn block_selection_text(
    term: &Term<VoidListener>,
    rl: i32,
    cl: usize,
    rh: i32,
    ch: usize,
) -> String {
    let max_line = term.screen_lines() as i32 - 1;
    let min_line = term.grid().topmost_line().0;
    let rl = rl.max(min_line);
    let rh = rh.min(max_line);
    let mut rows = Vec::new();
    for line in rl..=rh {
        rows.push(extract_selection_text(term, line, cl, line, ch));
    }
    rows.join("\n")
}

/// True iff (row, col) is inside the inclusive rectangle whose corners are
/// (rl, cl) and (rh, ch) — [`Selection::block_bounds`] output. Public for
/// unit testing.
pub fn cell_in_block_selection(row: i32, col: u16, rl: i32, cl: u16, rh: i32, ch: u16) -> bool {
    row >= rl && row <= rh && col >= cl && col <= ch
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

    /// The zsh the shim tests drive, or an ANNOUNCED skip on a machine
    /// without one (#52). CI provisions zsh, so the gate holds where it
    /// matters; on a zsh-less dev box these tests must skip with a reason
    /// — a hard spawn failure (or a silent `return`) makes "suite green"
    /// ambiguous. Same convention as the cross-compile and PDF gates.
    fn zsh_or_skip() -> Option<&'static str> {
        let zsh = "/bin/zsh";
        if std::path::Path::new(zsh).exists() {
            Some(zsh)
        } else {
            eprintln!("SKIPPED: {zsh} not installed on this machine");
            None
        }
    }

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
    fn block_selection_highlights_and_extracts_a_rectangle() {
        let mut term = fresh_term(20, 5);
        feed(&mut term, b"alpha1\r\nbravo2\r\ncharlie3\r\n");
        // A rectangle over rows 0..=2, cols 1..=3, anchored bottom-right so
        // the bounds must come from independent min/max, not the row-major
        // normalisation linear selections use.
        let sel = Selection {
            anchor: (2, 3),
            head: (0, 1),
            block: true,
        };
        let (rl, cl, rh, ch) = sel.block_bounds();
        assert_eq!((rl, cl, rh, ch), (0, 1, 2, 3));
        assert!(cell_in_block_selection(1, 2, rl, cl, rh, ch));
        assert!(!cell_in_block_selection(1, 0, rl, cl, rh, ch));
        assert!(!cell_in_block_selection(1, 4, rl, cl, rh, ch));
        assert!(!cell_in_block_selection(3, 2, rl, cl, rh, ch));
        assert_eq!(
            block_selection_text(&term, rl, cl as usize, rh, ch as usize),
            "lph\nrav\nhar",
            "a block selection copies the same column slice from every row"
        );
    }

    #[test]
    fn annotations_anchor_to_content_and_ride_the_scrollback() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "n=note; echo ${n}-worthy-line; read x; i=0; while [ $i -lt 30 ]; do echo fill-$i; i=$((i+1)); done; sleep 30";
        let mut term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        term.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let mut waited = 0u32;
        while !term
            .grid_lines()
            .0
            .iter()
            .any(|l| l.starts_with("note-worthy-line"))
        {
            assert!(waited < 8000, "line never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        let (lines, top) = term.grid_lines();
        let line = top
            + lines
                .iter()
                .position(|l| l.starts_with("note-worthy-line"))
                .unwrap() as i32;
        let clock = term.scroll_clock();
        term.add_annotation(line, clock, 0, 16, String::from("this is where it broke"));
        // The screen cell over the span resolves to the note.
        let (idx, text) = term
            .annotation_at(3, line as u16)
            .expect("the annotated cell must resolve");
        assert_eq!(idx, 0);
        assert_eq!(text, "this is where it broke");
        assert!(
            term.annotation_at(30, line as u16).is_none(),
            "cells past the span carry no note"
        );

        // 30 more lines push the annotated row toward (or into) history;
        // the anchor drifts with the content.
        term.write_input(b"\n");
        let mut waited = 0u32;
        while !term.grid_lines().0.iter().any(|l| l.starts_with("fill-29")) {
            assert!(waited < 8000, "filler never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        let (lines, top) = term.grid_lines();
        let expect_line = top
            + lines
                .iter()
                .position(|l| l.starts_with("note-worthy-line"))
                .unwrap() as i32;
        let cur = term.annotations_current();
        assert_eq!(cur.len(), 1);
        assert_eq!(
            cur[0].0, expect_line,
            "the annotation must still sit on its content line"
        );
        assert_eq!(cur[0].3, "this is where it broke");
    }

    #[test]
    fn prompt_click_arrows_map_a_click_to_cursor_motion() {
        let tmp = tempfile::tempdir().unwrap();
        // A prompt with 133;A/B marks and typed text, cursor parked after
        // the text (no newline). `read` keeps the shell at the prompt.
        let script = "h=hello; printf '\\033]133;A\\007$ \\033]133;B\\007'; printf \"${h}-world\"; read x; sleep 30";
        let mut term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        term.last_inner = Rect {
            x: 1,
            y: 1,
            width: 80,
            height: 24,
        };
        let mut waited = 0u32;
        while !term
            .grid_lines()
            .0
            .iter()
            .any(|l| l.contains("hello-world"))
        {
            assert!(waited < 8000, "prompt text never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        let (lines, top) = term.grid_lines();
        let vrow = (lines
            .iter()
            .position(|l| l.contains("hello-world"))
            .unwrap() as i32
            + top) as u16; // display_offset 0: grid line == viewport row
        // Click on the 'h' (grid col 2): the cursor sits after 'd' (col 13),
        // so 11 left-arrows bring it there.
        let bytes = term
            .prompt_click_arrows(1 + 2, 1 + vrow)
            .expect("a prompt-row click must produce motion");
        assert_eq!(bytes, b"\x1b[D".repeat(11));
        // Clamped left of the input start: col 0 is the "$ " prompt glyphs.
        let bytes = term.prompt_click_arrows(1, 1 + vrow).unwrap();
        assert_eq!(bytes, b"\x1b[D".repeat(11), "clamps to the 133;B column");
        // A click on the cursor cell itself is a no-op.
        assert!(term.prompt_click_arrows(1 + 13, 1 + vrow).is_none());
        // A click on another row does nothing.
        assert!(term.prompt_click_arrows(1 + 2, 1 + vrow + 1).is_none());
    }

    #[test]
    fn row_times_record_when_each_line_arrived() {
        // Deterministic (#170): a loaded scheduler can hold the reader
        // past the writer's sleep, coalescing both echoes into ONE read
        // that stamp_chunk marks with a single now_ms — so no real-PTY
        // timing assertion can pin per-chunk stamps. Drive the stamping
        // path directly instead, one chunk per arrival.
        let (_tmp, t) = quiet_pty();
        let mut prev = 0i64;
        feed_pty(&t, b"first-marker\r\n");
        t.stamp_chunk_for_test(&mut prev, 1000);
        feed_pty(&t, b"second-marker\r\n");
        t.stamp_chunk_for_test(&mut prev, 1400);
        let (lines, top) = t.grid_lines();
        let a = lines
            .iter()
            .position(|l| l.starts_with("first-marker"))
            .expect("first line on the grid");
        let b = lines
            .iter()
            .position(|l| l.starts_with("second-marker"))
            .expect("second line on the grid");
        let ta = t
            .row_time(top + a as i32)
            .expect("the first line must be stamped");
        let tb = t
            .row_time(top + b as i32)
            .expect("the second line must be stamped");
        assert_eq!(ta, 1000, "first row keeps its own chunk's arrival time");
        assert_eq!(tb, 1400, "second row gets the later chunk's arrival time");
    }

    #[test]
    fn reader_thread_stamps_arriving_rows() {
        // End-to-end: the real reader loop must feed stamp_chunk. Only
        // presence and ordering are asserted — any gap assertion races
        // the scheduler (#170).
        let tmp = tempfile::tempdir().unwrap();
        let script = "a=first; echo ${a}-marker; sleep 30";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        let mut waited = 0u32;
        loop {
            let (lines, top) = term.grid_lines();
            if let Some(a) = lines.iter().position(|l| l.starts_with("first-marker")) {
                assert!(
                    term.row_time(top + a as i32).is_some(),
                    "the reader thread must stamp rows it delivers"
                );
                break;
            }
            assert!(waited < 8000, "marker never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
    }

    #[test]
    fn resize_reflows_long_lines_instead_of_truncating() {
        // Scrollback soft-reflow is an engine property (alacritty_terminal's
        // Term::resize reflows the normal screen); this pins it so a future
        // engine upgrade can't silently lose it.
        let mut term = fresh_term(30, 5);
        feed(&mut term, b"abcdefghijklmnopqrstuvwxyz1234\r\n");
        term.resize(TermSize::new(10, 5));
        let top = term.grid().topmost_line().0;
        let bottom = term.screen_lines() as i32 - 1;
        let mut all = String::new();
        for l in top..=bottom {
            let (s, _) = row_text_and_cols(&term, l);
            all.push_str(s.trim_end());
        }
        assert!(
            all.contains("abcdefghijklmnopqrstuvwxyz1234"),
            "text must survive a narrowing resize by reflowing: {all:?}"
        );
        // Widening back re-joins the soft-wrapped rows onto one line.
        term.resize(TermSize::new(40, 5));
        let joined: Vec<String> = (term.grid().topmost_line().0..term.screen_lines() as i32)
            .map(|l| row_text_and_cols(&term, l).0.trim_end().to_string())
            .collect();
        assert!(
            joined
                .iter()
                .any(|r| r.contains("abcdefghijklmnopqrstuvwxyz1234")),
            "widening must re-join the reflowed rows: {joined:?}"
        );
    }

    #[test]
    fn scroll_offset_centers_a_scrollback_match_and_clamps_to_the_range() {
        // 24-row pane, 100 lines of scrollback (max_off 100). A match on an
        // old line (-80) centers it: 12 - (-80) = 92, within [0, 100].
        assert_eq!(scroll_offset_for_line(24, 100, -80), 92);
        // A match on the live screen (line 20) wants a negative offset but is
        // clamped to 0 — you cannot scroll below the live bottom.
        assert_eq!(scroll_offset_for_line(24, 100, 20), 0);
        // A match older than the deepest scrollback is clamped to max_off.
        assert_eq!(scroll_offset_for_line(24, 100, -200), 100);
        // No scrollback (alt screen / fresh grid): always pinned to bottom.
        assert_eq!(scroll_offset_for_line(24, 0, -5), 0);
    }

    #[test]
    fn mouse_report_encoding_matches_the_xterm_wire_format() {
        let none = MouseMods::default();
        let shift = MouseMods {
            shift: true,
            ..MouseMods::default()
        };
        // SGR: 1-based coords, 'M' press / 'm' release, wheel = 64/65.
        assert_eq!(
            encode_mouse_report(true, MouseButtonKind::Left, MouseAction::Press, 0, 0, none),
            b"\x1b[<0;1;1M"
        );
        assert_eq!(
            encode_mouse_report(
                true,
                MouseButtonKind::Left,
                MouseAction::Release,
                4,
                2,
                none
            ),
            b"\x1b[<0;5;3m"
        );
        assert_eq!(
            encode_mouse_report(
                true,
                MouseButtonKind::WheelDown,
                MouseAction::Press,
                9,
                9,
                none
            ),
            b"\x1b[<65;10;10M"
        );
        // Motion adds 32; Shift adds 4.
        assert_eq!(
            encode_mouse_report(
                true,
                MouseButtonKind::Left,
                MouseAction::Motion,
                0,
                0,
                shift
            ),
            b"\x1b[<36;1;1M"
        );
        // Legacy X10: 0x20-based bytes, release collapses the button to code 3.
        assert_eq!(
            encode_mouse_report(false, MouseButtonKind::Left, MouseAction::Press, 0, 0, none),
            &[0x1b, b'[', b'M', 32, 33, 33]
        );
        assert_eq!(
            encode_mouse_report(
                false,
                MouseButtonKind::Left,
                MouseAction::Release,
                0,
                0,
                none
            ),
            &[0x1b, b'[', b'M', 35, 33, 33]
        );
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
            bell: None,
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
            bell: None,
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

    /// #357: the REAL reader thread records output into the rewind buffer.
    ///
    /// The app-level test drives `feed_bytes_for_test`, a `#[cfg(test)]`
    /// sibling hand-edited to mirror the reader. That proves the buffer works
    /// but not that the production path uses it — deleting the reader's
    /// `rb.push` leaves it green, which was measured, not assumed. This
    /// spawns a real shell and reads what the reader thread actually stored.
    #[test]
    fn the_reader_thread_records_shell_output_for_rewind() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term = PtyTerminal::new(tmp.path()).unwrap();
        // The needle only ever appears as command OUTPUT, so matching it in
        // the buffer proves the recorded bytes came through the pty rather
        // than from the echoed input line.
        term.write_input(b"echo REWIND_$(echo recorded)\n");
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(2000),
            "the shell to print the needle",
            || term.visible_text().contains("REWIND_recorded"),
        );

        let rb = term.rewind().lock().unwrap();
        let (_, frames) = rb.replay_from(u64::MAX);
        let seen: Vec<u8> = frames.iter().flat_map(|f| f.data.clone()).collect();
        assert!(
            String::from_utf8_lossy(&seen).contains("REWIND_recorded"),
            "the reader thread parsed output without recording it: {} bytes held",
            rb.bytes()
        );
    }

    /// Output arriving BEFORE an OSC 133 mark must be recorded too.
    ///
    /// The reader splits each read at OSC events and advances the parser per
    /// segment, leaving `done` at the LAST mark in the chunk. Recording
    /// `buf[done..n]` therefore keeps only the tail, and with shell
    /// integration on — marks arriving around every prompt and command — that
    /// silently discards most of the session, which is the one thing this
    /// buffer exists to keep.
    ///
    /// `the_reader_thread_records_shell_output_for_rewind` cannot see this:
    /// its script emits no marks, so `done` stays 0 and the tail happens to
    /// BE the whole chunk. The fixture agrees with the implementation. This
    /// test puts a mark between two needles so the two differ.
    #[test]
    fn output_before_an_osc_mark_is_recorded_not_just_the_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term = PtyTerminal::new(tmp.path()).unwrap();
        // Both needles must be command OUTPUT ONLY. The shell ECHOES the
        // input line before running it, and that echo arrives in one chunk
        // with no mark before it — so a needle written literally in the
        // command reaches the buffer via the echo whether or not the
        // recording under test works, and the test passes vacuously. `$(..)`
        // is unexpanded in the echo, so these two strings exist only in the
        // command's output. (The sibling test above documents this; I used it
        // as a template and dropped the property it exists to explain.)
        term.write_input(
            b"printf \"BEFORE_$(echo MARK)\\n\\033]133;C\\007AFTER_$(echo MARK)\\n\"\n",
        );
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(2000),
            "the shell to print both needles",
            || {
                let v = term.visible_text();
                v.contains("BEFORE_MARK") && v.contains("AFTER_MARK")
            },
        );

        let rb = term.rewind().lock().unwrap();
        let (_, frames) = rb.replay_from(u64::MAX);
        let seen: Vec<u8> = frames.iter().flat_map(|f| f.data.clone()).collect();
        let text = String::from_utf8_lossy(&seen);
        // The tail alone is the bug: AFTER_MARK present, BEFORE_MARK dropped.
        // Asserting BOTH is what distinguishes the fix from the defect — the
        // AFTER_MARK half is the paired presence assertion that keeps the
        // BEFORE_MARK claim from passing over an empty buffer.
        assert!(
            text.contains("AFTER_MARK"),
            "nothing was recorded at all: {} bytes held",
            rb.bytes()
        );
        assert!(
            text.contains("BEFORE_MARK"),
            "output before the OSC mark was parsed but not recorded -- only \
             the tail after the last mark survived: {:?}",
            text
        );
    }

    /// Drop must REAP the shell on every path. portable-pty's `kill()` reaps
    /// only when the shell dies inside its SIGHUP grace loop; a shell that
    /// ignores HUP gets the SIGKILL escalation, which never waits — so every
    /// such closed pane left a zombie for the life of the process.
    #[test]
    fn dropping_a_terminal_reaps_a_hup_ignoring_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term = PtyTerminal::new(tmp.path()).unwrap();
        let pid = term.pid().expect("spawned shell has a pid") as i32;
        // The needle only ever appears as command OUTPUT ($(..) is unexpanded
        // in the input echo), so matching it proves the trap line executed.
        term.write_input(b"trap '' HUP; echo TRAP_$(echo armed)\n");
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(1200),
            "the shell to confirm the HUP trap",
            || term.visible_text().contains("TRAP_armed"),
        );
        drop(term);
        let mut status = 0i32;
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert!(
            r < 0,
            "the shell was left as an unreaped zombie (waitpid still found it: r={r})"
        );
    }

    /// Drop must return promptly even when a background job keeps the pty
    /// slave open: on Linux the reader's blocked `read` never returns then
    /// (no tty revoke like macOS), so joining it would freeze the UI until
    /// the job exits. The shutdown pipe is what wakes the reader.
    #[test]
    fn a_background_job_cannot_stall_a_dropped_pane() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term = PtyTerminal::new(tmp.path()).unwrap();
        term.write_input(b"sleep 30 &\n");
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(term);
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(10)).is_ok(),
            "drop hung: the reader never woke while a background job held the pty slave"
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

    /// Spawn a pane whose "shell" is a script holding the tty foreground
    /// group with a job-control child — the rc-startup state the #94 flake
    /// caught `change_workspace_root` in. Returns the pane after the child
    /// has taken the foreground (bounded wait, asserted).
    fn startup_owned_pane(tmp: &tempfile::TempDir) -> PtyTerminal {
        let script = tmp.path().join("slow-startup.sh");
        std::fs::write(&script, "#!/bin/bash\nset -m\nsleep 30\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let term = PtyTerminal::new_running(script.to_str().unwrap(), &[], tmp.path()).unwrap();
        let mut waited = 0u32;
        while waited < 4000 && term.foreground_is_shell() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert!(
            !term.foreground_is_shell(),
            "precondition: the startup child must own the foreground group"
        );
        term
    }

    #[test]
    fn cwd_seed_stays_safe_through_startup_until_a_prompt_mark_arrives() {
        let tmp = tempfile::tempdir().unwrap();
        let term = startup_owned_pane(&tmp);
        assert!(
            term.cwd_seed_is_safe(),
            "an untouched pane whose foreground group is shell startup must stay seedable: the seed queues as type-ahead for the first prompt"
        );
        // Once shell integration reports a prompt, startup is over — a
        // non-shell foreground group is a real app from here on.
        term.push_mark_for_test(crate::shell_integration::OscEvent::PromptStart, 0);
        assert!(
            !term.cwd_seed_is_safe(),
            "after the first prompt mark, a non-shell foreground group must suppress the seed"
        );
    }

    #[test]
    fn cwd_seed_turns_unsafe_once_the_pane_has_received_input() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term = startup_owned_pane(&tmp);
        // Any input could have launched the app that owns the tty, so the
        // startup carve-out must die with the first written byte.
        term.write_input(b"q");
        assert!(
            !term.cwd_seed_is_safe(),
            "after input has been written, a non-shell foreground group must suppress the seed"
        );
    }

    /// A foreground process that owns the tty (an ssh client, exactly)
    /// controls everything the pane prints — including an OSC 7 report
    /// impersonating this machine's own hostname. The self-reported host
    /// is no proof of locality then: `local_shell_cwd` must refuse while
    /// the shell is not the foreground group, whatever the claim says.
    #[test]
    fn a_foreground_process_cannot_impersonate_a_local_cwd_report() {
        let out = std::process::Command::new("hostname").output().unwrap();
        let local = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if local.is_empty() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let mut term = PtyTerminal::new(tmp.path()).unwrap();
        let mut waited = 0u32;
        while waited < 4000 && !term.foreground_is_shell() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        // Stage the decoy: the shim's own prompt-time OSC 7 reports the
        // shell's real cwd (the tempdir), so `shell_cwd()` is `Some` long
        // before the forged claim exists — an `is_some()` stage exits on
        // this report and reads the wrong value (#69).
        let mut waited = 0u32;
        while waited < 4000 && term.shell_cwd().is_none() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        // A foreground job (the ssh stand-in) prints an OSC 7 claiming
        // this machine's own hostname. It sleeps BEFORE printing, so the
        // tty is lost to the job well before the claim can be parsed —
        // deterministically the ordering that flaked under suite load.
        term.write_input(
            format!("sleep 0.5; printf '\\033]7;file://{local}/tmp\\007'; sleep 10\n").as_bytes(),
        );
        let mut waited = 0u32;
        while waited < 4000 && term.foreground_is_shell() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert!(!term.foreground_is_shell(), "staging: a job owns the tty");
        // Wait for the claim ITSELF: any earlier `Some` is the decoy.
        let claim = std::path::PathBuf::from("/tmp");
        let mut waited = 0u32;
        while waited < 6000 && term.shell_cwd().as_deref() != Some(claim.as_path()) {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert_eq!(
            term.shell_cwd(),
            Some(claim),
            "staging: the claim was captured"
        );
        assert_eq!(
            term.local_shell_cwd(),
            None,
            "a tty-owning foreground process must not place local splits by claim"
        );
    }

    /// The buffered claim outliving its forger: a foreground job prints a
    /// local-looking OSC 7 and dies, but the reader thread only PARSES
    /// those bytes later (here: the render side held the term lock, the
    /// production delay). Any tty-ownership sample taken at parse time
    /// sees the shell back at its prompt — PTY bytes carry no author, so
    /// timing can never establish provenance. The claim must be rejected
    /// on content: it names a directory the shell is not actually in.
    #[test]
    fn a_claim_parsed_only_after_its_job_died_is_still_untrusted() {
        let Some(zsh) = zsh_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        // zsh -f: job control (the job gets its own pgroup) but no
        // integration, so nothing overwrites the forged claim.
        let mut term =
            PtyTerminal::new_running(zsh, &[String::from("-f"), String::from("-i")], tmp.path())
                .unwrap();
        let mut waited = 0u32;
        while waited < 4000 && !term.foreground_is_shell() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        // The job sleeps before printing, leaving time to stall the
        // reader below; then it prints the claim and lingers briefly so
        // the bytes enter the kernel buffer while it owns the tty.
        let term_arc = term.term.clone();
        term.write_input(
            b"/bin/sh -c 'sleep 1; printf \"\\033]7;file://localhost/tmp\\007\"; sleep 1'\n",
        );
        // Wait for the job to take the tty and the reader to drain the
        // command echo, THEN stall parsing by taking the term lock. The
        // lock cannot be held from the start: with the reader parked,
        // zsh's startup burst fills the kernel pty queue and blocks the
        // shell before it ever runs the line. The claim plus one prompt
        // redraw are small enough that nothing blocks during the stall.
        let mut waited = 0u32;
        while waited < 4000 && term.foreground_is_shell() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert!(!term.foreground_is_shell(), "staging: the job took the tty");
        std::thread::sleep(std::time::Duration::from_millis(150));
        let guard = term_arc.lock();
        // With parsing stalled, wait out the job's remaining life: claim
        // printed, job dead, shell back at its prompt. tcgetpgrp is
        // kernel state, untouched by our lock.
        let mut waited = 0u32;
        while waited < 8000 && !term.foreground_is_shell() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert!(
            term.foreground_is_shell(),
            "staging: the job is gone and the shell owns the tty again"
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(guard);
        let mut waited = 0u32;
        while waited < 4000 && term.shell_cwd() != Some(std::path::PathBuf::from("/tmp")) {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert_eq!(
            term.shell_cwd(),
            Some(std::path::PathBuf::from("/tmp")),
            "staging: the claim was captured after the job died"
        );
        assert_eq!(
            term.local_shell_cwd(),
            None,
            "a claim whose bytes were parsed after its job died must not be trusted"
        );
    }

    /// The forged claim outliving its forger: a foreground job prints an
    /// OSC 7 with a local-looking host and EXITS; the shell retakes the
    /// tty without re-reporting (no integration), so a foreground check at
    /// consultation time passes on the stale cache. A report that arrived
    /// from a job never becomes trusted, however long ago the job died.
    #[test]
    fn a_stale_claim_from_a_dead_foreground_job_stays_untrusted() {
        let Some(zsh) = zsh_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        // zsh -f: job control (the claim's job gets its own pgroup, like a
        // real ssh client) but NO integration — the shell never overwrites
        // the stale claim with a fresh trusted report of its own.
        let mut term =
            PtyTerminal::new_running(zsh, &[String::from("-f"), String::from("-i")], tmp.path())
                .unwrap();
        let mut waited = 0u32;
        while waited < 4000 && !term.foreground_is_shell() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        // An EXTERNAL job (its own pgroup, like an ssh client — a bare
        // printf would be a zsh builtin printing from the shell's own
        // pgroup) settles as the tty owner, prints the local-looking
        // claim, lingers so the capture happens while it owns the tty,
        // and dies.
        term.write_input(
            b"/bin/sh -c 'sleep 1; printf \"\\033]7;file://localhost/tmp\\007\"; sleep 1'\n",
        );
        let mut waited = 0u32;
        while waited < 8000
            && !(term.foreground_is_shell()
                && term.shell_cwd() == Some(std::path::PathBuf::from("/tmp")))
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert_eq!(
            term.shell_cwd(),
            Some(std::path::PathBuf::from("/tmp")),
            "staging: the claim was captured"
        );
        assert!(
            term.foreground_is_shell(),
            "staging: the job is gone and the shell owns the tty again"
        );
        assert_eq!(
            term.local_shell_cwd(),
            None,
            "a claim captured while a job owned the tty must never become trusted"
        );
    }

    #[test]
    fn clear_screen_and_scrollback_wipes_the_grid() {
        fn dump(term: &PtyTerminal) -> String {
            let t = term.term.lock();
            let rows = t.screen_lines();
            let cols = t.columns();
            let off = t.grid().display_offset() as i32;
            extract_selection_text(&t, -off, 0, rows as i32 - 1 - off, cols.saturating_sub(1))
        }
        let tmp = tempfile::tempdir().unwrap();
        // The spawn banner prints the command's argv, so the probe must never
        // appear verbatim in argv: under load the banner lands before the
        // child's output, the wait below matches the BANNER, and the clear
        // fires before the probe exists - which the assert then finds alive.
        // The shell concatenation keeps argv and output distinct.
        let mut term = PtyTerminal::new_running(
            "/bin/sh",
            &[String::from("-c"), String::from("echo CLEAR-PROBE-\"\"XYZ")],
            tmp.path(),
        )
        .unwrap();
        let mut waited = 0u32;
        while waited < 4000 && !dump(&term).contains("CLEAR-PROBE-XYZ") {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert!(
            dump(&term).contains("CLEAR-PROBE-XYZ"),
            "probe present before clear"
        );
        term.clear_screen_and_scrollback();
        assert!(
            !dump(&term).contains("CLEAR-PROBE-XYZ"),
            "clear wipes the screen and scrollback"
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
        // Wait on the BYTE COUNTER, not the dirty flag: `pty_dirty` is
        // constructed `true`, so peeking it succeeds before /bin/echo has
        // written anything and this test would pass just as happily if direct
        // spawns stopped reaching the reader thread entirely. `pending_bytes`
        // only moves when the reader actually advanced output.
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(500),
            "the directly spawned /bin/echo to deliver bytes through the PTY",
            || term.peek_pending_bytes() > 0,
        );
        assert!(
            term.peek_pending_bytes() > 0,
            "direct-spawned /bin/echo must produce output without any write_input"
        );
    }

    #[test]
    fn pristine_covers_only_an_untouched_interactive_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let mut shell = PtyTerminal::new(tmp.path()).unwrap();
        assert!(
            shell.is_pristine(),
            "a freshly spawned shell with no input and no name is pristine"
        );
        shell.write_input(b"echo touched\n");
        assert!(
            !shell.is_pristine(),
            "any input byte permanently clears pristineness"
        );

        let mut named = PtyTerminal::new(tmp.path()).unwrap();
        named.set_manual_name(Some(String::from("srv")));
        assert!(
            !named.is_pristine(),
            "a manual name is user state; the pane must not be replaceable"
        );
        named.set_manual_name(None);
        assert!(
            !named.is_pristine(),
            "a rename latches: clearing the name back must not make the pane replaceable"
        );

        let run = PtyTerminal::new_running(
            "/bin/sh",
            &[String::from("-c"), String::from("sleep 1")],
            tmp.path(),
        )
        .unwrap();
        assert!(
            !run.is_pristine(),
            "a launched-program pane is doing user work despite having seen no input"
        );
    }

    /// Poll `grid_lines` until a predicate matches or 4s elapse.
    fn wait_for_grid<F: Fn(&[String]) -> bool>(term: &PtyTerminal, pred: F) -> Vec<String> {
        let mut waited_ms = 0u32;
        loop {
            let (lines, _top) = term.grid_lines();
            if pred(&lines) {
                return lines;
            }
            assert!(
                waited_ms < 4000,
                "expected output never reached the grid; grid was: {lines:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited_ms += 20;
        }
    }

    #[test]
    fn ansi_named_colors_render_through_the_theme_palette() {
        // SGR 31 red must come out as the theme palette's red (VS Code's
        // #cd3131 by default), not the host terminal's Color::Red — croft
        // owns its terminal palette the way VS Code does. A palette swap
        // repaints with the new colors.
        let tmp = tempfile::tempdir().unwrap();
        // The sentinel is assembled at runtime (`${s}RED`) so the pane's
        // `▶ …` run-label header — which contains the raw command text —
        // can never match the scan below.
        let mut term = PtyTerminal::new_running(
            "/bin/sh",
            &[
                String::from("-c"),
                String::from("s=QQ; printf \"\\033[31m${s}RED\\033[0m\\n\"; sleep 30"),
            ],
            tmp.path(),
        )
        .unwrap();
        wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains("QQRED")));
        let area = Rect::new(0, 0, 60, 10);
        let find_q_fg = |term: &mut PtyTerminal| -> Option<Color> {
            let mut buf = Buffer::empty(area);
            Widget::render(term, area, &mut buf);
            for y in 0..area.height {
                for x in 1..area.width - 2 {
                    if buf[(x, y)].symbol() == "Q"
                        && buf[(x + 1, y)].symbol() == "Q"
                        && buf[(x + 2, y)].symbol() == "R"
                    {
                        return Some(buf[(x, y)].fg);
                    }
                }
            }
            None
        };
        assert_eq!(
            find_q_fg(&mut term),
            Some(Color::Rgb(0xcd, 0x31, 0x31)),
            "SGR red must map through the default palette"
        );
        let mut palette = crate::theme::VSCODE_ANSI;
        palette[1] = (0x12, 0x34, 0x56);
        term.set_palette(palette);
        assert_eq!(
            find_q_fg(&mut term),
            Some(Color::Rgb(0x12, 0x34, 0x56)),
            "a palette swap must repaint SGR red with the new color"
        );
    }

    /// #360: a redact-trigger match paints as a run of mask glyphs of the
    /// same width while the grid keeps the real text, so a click on the
    /// mask can still recover it; the count feeds the status chip; a
    /// reveal window paints the text as typed.
    #[test]
    fn redacted_spans_paint_as_masks_and_the_real_text_stays_recoverable() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term = PtyTerminal::new_running(
            "/bin/sh",
            &[
                String::from("-c"),
                String::from(
                    "printf 'key AKIAIOSFODNN7EXAMPLE end\\ntail AKIAIOSFODNN7EXAMPLE\\n'; sleep 30",
                ),
            ],
            tmp.path(),
        )
        .unwrap();
        // The pane's spawn banner echoes the command line, key included, so
        // wait for the OUTPUT line, which starts at column 0.
        wait_for_grid(&term, |ls| ls.iter().any(|l| l.starts_with("tail AKIA")));
        term.set_triggers(std::sync::Arc::new(
            crate::triggers::TriggerSet::default().with_builtin_redactions(),
        ));
        let area = Rect::new(0, 0, 60, 10);
        term.last_inner = Rect::new(1, 1, 58, 8);
        let mut buf = Buffer::empty(area);
        Widget::render(&mut term, area, &mut buf);
        let row_text = |buf: &Buffer, y: u16| -> String {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect()
        };
        let rows: Vec<String> = (0..area.height).map(|y| row_text(&buf, y)).collect();
        let (y, line) = rows
            .iter()
            .enumerate()
            .map(|(y, l)| (y as u16, l.clone()))
            .find(|(_, l)| l.starts_with("\u{2502}key "))
            .unwrap_or_else(|| {
                panic!(
                    "the key line rendered: {rows:#?} grid={:?} label={:?}",
                    term.grid_lines().0,
                    term.label()
                )
            });
        assert!(
            line.contains(&"\u{2022}".repeat(20)) && !line.contains("AKIA"),
            "the key paints as twenty masks: {line:?}"
        );
        assert!(
            line.contains(" end"),
            "text around the key is untouched: {line:?}"
        );
        assert!(
            term.redacted_on_screen >= 1,
            "the status chip counts at least the output line's span"
        );

        // The grid still holds the real text: a click on the mask finds it.
        // A column, not a byte offset: the border glyph before it is multibyte.
        let x = line.chars().position(|c| c == '\u{2022}').unwrap() as u16;
        assert_eq!(
            term.redacted_at(x, y).as_deref(),
            Some("AKIAIOSFODNN7EXAMPLE"),
            "the mask click recovers the value"
        );
        assert_eq!(
            term.redacted_at(x.saturating_sub(2), y),
            None,
            "off the mask: nothing"
        );
        // A row whose masked token is its LAST text: a click a few cells
        // right of the mask, still inside the pane, resolves to that blank
        // cell's own char (the colmap is dense) and reveals nothing.
        let (ty, tline) = rows
            .iter()
            .enumerate()
            .map(|(y, l)| (y as u16, l.clone()))
            .find(|(_, l)| l.starts_with("\u{2502}tail "))
            .expect("the tail line rendered");
        let tx = tline.chars().position(|c| c == '\u{2022}').unwrap() as u16;
        assert_eq!(
            term.redacted_at(tx, ty).as_deref(),
            Some("AKIAIOSFODNN7EXAMPLE"),
            "the last token's mask still reveals on itself"
        );
        assert!(tx + 24 < area.width - 1, "the probe stays inside the pane");
        assert_eq!(
            term.redacted_at(tx + 24, ty),
            None,
            "blank cells right of the row's text reveal nothing"
        );

        // A reveal window paints the text as typed and counts nothing.
        term.reveal_redactions = true;
        let mut buf = Buffer::empty(area);
        Widget::render(&mut term, area, &mut buf);
        assert!(row_text(&buf, y).contains("AKIAIOSFODNN7EXAMPLE"));
        assert_eq!(term.redacted_on_screen, 0);
    }

    #[test]
    fn faint_cells_render_with_the_dim_modifier() {
        // Claude Code paints its inline ghost suggestion as default-foreground
        // text under SGR 2 (faint). The pane must forward the flag as
        // Modifier::DIM, or the suggestion renders at full brightness and is
        // indistinguishable from text the user typed.
        let tmp = tempfile::tempdir().unwrap();
        let mut term = PtyTerminal::new_running(
            "/bin/sh",
            &[
                String::from("-c"),
                String::from("s=QQ; printf \"\\033[2m${s}GHOST\\033[0m\\n\"; sleep 30"),
            ],
            tmp.path(),
        )
        .unwrap();
        wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains("QQGHOST")));
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        Widget::render(&mut term, area, &mut buf);
        for y in 0..area.height {
            for x in 1..area.width - 2 {
                if buf[(x, y)].symbol() == "Q"
                    && buf[(x + 1, y)].symbol() == "Q"
                    && buf[(x + 2, y)].symbol() == "G"
                {
                    assert!(
                        buf[(x, y)].modifier.contains(Modifier::DIM),
                        "SGR 2 (faint) cells must carry Modifier::DIM"
                    );
                    return;
                }
            }
        }
        panic!("the faint sentinel never appeared in the rendered buffer");
    }

    #[test]
    fn imgcat_style_inline_images_are_captured_with_a_grid_anchor() {
        use base64::Engine;
        // A 1x1 red PNG; what `imgcat tiny.png` would emit.
        let mut png_buf = Vec::new();
        image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]))
            .write_to(
                &mut std::io::Cursor::new(&mut png_buf),
                image::ImageFormat::Png,
            )
            .expect("encode test png");
        let png: &[u8] = &png_buf;
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let tmp = tempfile::tempdir().unwrap();
        let script = format!(
            "printf 'before\\n'; printf '\\033]1337;File=inline=1:{b64}\\007\\n'; printf 'after\\n'; sleep 30"
        );
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
        wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains("after")));
        let mut waited = 0u32;
        let imgs = loop {
            let imgs = term.pane_images();
            if !imgs.is_empty() {
                break imgs;
            }
            assert!(waited < 8000, "inline image never captured");
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        };
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].data.as_slice(), png, "payload must round-trip");
        // Anchor: the row right after the `before` line (the run-label
        // header above it wraps, so absolute row numbers are not stable).
        let (lines, top) = term.grid_lines();
        let before_row = lines
            .iter()
            .rposition(|l| l.trim_end() == "before")
            .expect("the before line must be on the grid") as i32
            + top;
        assert_eq!(
            imgs[0].line,
            before_row + 1,
            "anchor must sit on the image's grid row; grid: {lines:?}"
        );
    }

    #[test]
    fn wide_chars_read_back_without_phantom_spacer_spaces() {
        // A CJK char occupies two grid columns: the WIDE_CHAR cell plus a
        // WIDE_CHAR_SPACER cell whose `c` is ' '. Text extraction must skip
        // the spacer, or copied/searched text comes out as "日 本 語" and a
        // find for "日本語" can never match.
        let tmp = tempfile::tempdir().unwrap();
        let term = PtyTerminal::new_running("/bin/echo", &[String::from("日本語 ok")], tmp.path())
            .unwrap();
        let lines = wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains('日')));
        assert!(
            lines.iter().any(|l| l.contains("日本語 ok")),
            "wide chars must read back contiguously (no spacer-cell spaces); grid was: {lines:?}"
        );
    }

    #[test]
    fn wide_chars_render_the_glyph_once_with_a_blank_spacer_cell() {
        // The render loop writes the double-width glyph into its WIDE_CHAR
        // cell and must leave the following spacer cell blank — ratatui skips
        // the cell after a width-2 symbol when diffing, so anything else
        // there would corrupt column alignment.
        let tmp = tempfile::tempdir().unwrap();
        let mut term =
            PtyTerminal::new_running("/bin/echo", &[String::from("日X")], tmp.path()).unwrap();
        wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains('日')));
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        Widget::render(&mut term, area, &mut buf);
        let mut found = false;
        for y in 0..area.height {
            for x in 0..area.width.saturating_sub(2) {
                if buf[(x, y)].symbol() == "日" {
                    assert_eq!(
                        buf[(x + 1, y)].symbol(),
                        " ",
                        "the spacer cell after a wide glyph must stay blank"
                    );
                    assert_eq!(
                        buf[(x + 2, y)].symbol(),
                        "X",
                        "the next glyph must land two columns after the wide char"
                    );
                    found = true;
                }
            }
        }
        assert!(found, "the wide glyph must appear in the rendered buffer");
    }

    #[test]
    fn hyperlink_at_returns_the_osc8_uri_under_the_cell() {
        // OSC 8 hyperlinks live in the cell, invisible in the text. The app's
        // Cmd/Ctrl+click handler asks `hyperlink_at` before falling back to
        // the plain-text URL regex.
        let tmp = tempfile::tempdir().unwrap();
        let script =
            "printf '\\033]8;;https://example.com/doc\\033\\\\CLICK-ME\\033]8;;\\033\\\\\\n'";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        // The run header echoes the command text, so "CLICK-ME" appears there
        // too — as plain text without a link. Wait for the child's actual
        // output row (the one without "printf").
        let is_output = |l: &String| l.contains("CLICK-ME") && !l.contains("printf");
        let lines = wait_for_grid(&term, |ls| ls.iter().any(is_output));
        let row = lines.iter().position(is_output).unwrap();
        let col = lines[row].find("CLICK-ME").unwrap();
        assert_eq!(
            term.hyperlink_at(row, col).as_deref(),
            Some("https://example.com/doc"),
            "the cell under the linked text must expose the OSC 8 URI"
        );
        assert_eq!(
            term.hyperlink_at(0, 0),
            None,
            "cells outside the link (the header row) carry no URI"
        );
    }

    #[test]
    fn osc133_marks_record_kind_exit_code_and_grid_line() {
        // Emit a prompt mark before "PROMPT", a command-start mark, and a
        // finished mark carrying exit 3. Marks must land on the grid lines
        // where the cursor sat when they arrived.
        let tmp = tempfile::tempdir().unwrap();
        let script = "printf 'before\\n\\033]133;A\\007PROMPT-LINE\\n\\033]133;C\\007out\\n\\033]133;D;3\\007'";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        let is_output = |l: &String| l.contains("PROMPT-LINE") && !l.contains("printf");
        let lines = wait_for_grid(&term, |ls| {
            ls.iter().any(is_output) && !term.command_marks().is_empty()
        });
        let (_, top) = term.grid_lines();
        let prompt_row = lines.iter().position(is_output).unwrap();
        let marks = term.command_marks();
        let prompt_line = marks
            .iter()
            .find(|(kind, _)| *kind == crate::shell_integration::OscEvent::PromptStart)
            .map(|(_, line)| *line)
            .expect("a PromptStart mark must be recorded");
        assert_eq!(
            prompt_line,
            top + prompt_row as i32,
            "the prompt mark must sit on the line where PROMPT-LINE was printed"
        );
        assert!(
            marks
                .iter()
                .any(|(kind, _)| *kind == crate::shell_integration::OscEvent::CommandEnd(Some(3))),
            "the finished mark must carry exit code 3; marks: {marks:?}"
        );
    }

    #[test]
    fn marks_follow_their_content_into_scrollback() {
        // A mark recorded on the live screen must keep pointing at the same
        // content after later output scrolls that content into history.
        let tmp = tempfile::tempdir().unwrap();
        let script = "printf '\\033]133;A\\007MARKED-PROMPT\\n'; i=0; while [ $i -lt 40 ]; do echo filler-$i; i=$((i+1)); done";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains("filler-39")));
        // The script's tail (trailing newline, shell exit) can scroll another
        // row or two AFTER filler-39 becomes visible. `lines` and `top` must
        // come from ONE settled snapshot, or the row index and the grid top
        // describe different moments and the compare is off by exactly the
        // late scroll (#110). Settle = two consecutive identical reads.
        let (lines, top) = {
            let mut prev = term.grid_lines();
            let mut waited = 0u32;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(40));
                let cur = term.grid_lines();
                if cur == prev || waited >= 4000 {
                    break cur;
                }
                prev = cur;
                waited += 40;
            }
        };
        let content_row = lines
            .iter()
            .position(|l| l.contains("MARKED-PROMPT") && !l.contains("printf"))
            .expect("the marked line must still be in scrollback");
        let marks = term.command_marks();
        let prompt_line = marks
            .iter()
            .find(|(kind, _)| *kind == crate::shell_integration::OscEvent::PromptStart)
            .map(|(_, line)| *line)
            .expect("the mark must survive the scroll");
        assert_eq!(
            prompt_line,
            top + content_row as i32,
            "the mark must move with its content into history (negative grid lines)"
        );
        assert!(
            prompt_line < 0,
            "after 40 filler lines the mark is in scrollback"
        );
    }

    #[test]
    fn osc7_updates_the_shell_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "printf '\\033]7;file://anyhost/tmp/croft%%20dir\\007ok\\n'";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        wait_for_grid(&term, |ls| {
            ls.iter().any(|l| l.contains("ok") && !l.contains("printf"))
        });
        assert_eq!(
            term.shell_cwd(),
            Some(std::path::PathBuf::from("/tmp/croft dir")),
            "OSC 7 must update the pane's live cwd"
        );
    }

    /// A remote shell's OSC 7 (an in-pane ssh with integration) names a
    /// path on the REMOTE machine: consumers that act on the local
    /// filesystem (split-in-cwd, session restore) must not trust it just
    /// because the same path happens to exist locally.
    #[test]
    fn a_remote_reported_cwd_is_not_trusted_for_local_use() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "printf '\\033]7;file://prod-db-1/tmp\\007ok\\n'; sleep 30";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        wait_for_grid(&term, |ls| {
            ls.iter().any(|l| l.contains("ok") && !l.contains("printf"))
        });
        assert_eq!(
            term.shell_cwd(),
            Some(std::path::PathBuf::from("/tmp")),
            "staging: the raw report is visible"
        );
        assert_eq!(
            term.local_shell_cwd(),
            None,
            "a path reported by another machine must not be reused locally"
        );
        // The truthful counterpart: the reporter really sits in the
        // directory it claims (spawned in /tmp), so the claim matches the
        // kernel's cwd for the pane's shell and localhost IS this machine.
        let script = "printf '\\033]7;file://localhost/tmp\\007ok\\n'; sleep 30";
        let term2 = PtyTerminal::new_running(
            "/bin/sh",
            &[String::from("-c"), script.into()],
            std::path::Path::new("/tmp"),
        )
        .unwrap();
        wait_for_grid(&term2, |ls| {
            ls.iter().any(|l| l.contains("ok") && !l.contains("printf"))
        });
        assert_eq!(
            term2.local_shell_cwd(),
            Some(std::path::PathBuf::from("/tmp")),
            "a localhost report IS this machine (RFC 8089)"
        );
    }

    #[test]
    fn osc9_notifications_drain_once() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "printf '\\033]9;build done\\007ok\\n'";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        wait_for_grid(&term, |ls| {
            ls.iter().any(|l| l.contains("ok") && !l.contains("printf"))
        });
        assert_eq!(term.drain_notifications(), vec![String::from("build done")]);
        assert!(
            term.drain_notifications().is_empty(),
            "a second drain returns nothing"
        );
    }

    #[test]
    fn notify_triggers_fire_on_completed_output_lines() {
        let tmp = tempfile::tempdir().unwrap();
        // The shell waits for a line of input, so the trigger set is in
        // place before the matching output is ever produced.
        let script = "read x; echo deploy failed; sleep 30";
        let mut term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        let set = crate::triggers::TriggerSet::from_json(
            r#"[ { "regex": "deploy (\\w+)", "action": "notify", "message": "deploy went \\1" } ]"#,
        );
        assert!(set.has_events());
        term.set_triggers(std::sync::Arc::new(set));
        term.write_input(b"\n");
        let mut waited = 0u32;
        let hits = loop {
            let hits = term.drain_trigger_hits();
            if !hits.is_empty() {
                break hits;
            }
            assert!(waited < 8000, "trigger never fired");
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        };
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].action, crate::triggers::TriggerAction::Notify);
        assert_eq!(hits[0].message, "deploy went failed");
    }

    /// A banner printed just before handing off to a full-screen app lands
    /// in the same PTY chunk as the alt-screen entry: gating the trigger
    /// scan on the chunk's FINAL mode dropped that whole chunk (and let a
    /// chunk that merely ENDED on the primary screen fire on alt content).
    /// The scanner tracks the boundary positionally, so the reader must
    /// feed it every chunk unconditionally.
    #[test]
    fn a_trigger_fires_for_primary_text_that_shares_a_chunk_with_alt_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "read x; printf 'deploy failed\\n\\033[?1049h'; sleep 1; printf '\\033[?1049lok\\n'; sleep 30";
        let mut term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        let set = crate::triggers::TriggerSet::from_json(
            r#"[ { "regex": "deploy (\\w+)", "action": "notify", "message": "deploy went \\1" } ]"#,
        );
        assert!(set.has_events());
        term.set_triggers(std::sync::Arc::new(set));
        term.write_input(b"\n");
        let mut waited = 0u32;
        let hits = loop {
            let hits = term.drain_trigger_hits();
            if !hits.is_empty() {
                break hits;
            }
            assert!(
                waited < 8000,
                "the primary-screen line before the alt entry never fired"
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        };
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].message, "deploy went failed");
    }

    #[test]
    fn poisoned_zdotdir_from_a_nested_croft_still_loads_the_user_rc() {
        // Regression: every pane exports ZDOTDIR=<shim>, so a croft launched
        // FROM a croft pane inherited it and treated the shim as the user's
        // dotfile dir — the shim then sourced itself in a recursion loop and
        // the user's real .zshrc (their theme, aliases) never ran. A
        // poisoned CROFT_USER_ZDOTDIR pointing at the shim must be ignored
        // in favour of $HOME.
        let Some(zsh) = zsh_or_skip() else { return };
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".zshrc"),
            "USER_RC_SENTINEL=loaded\nexport USER_RC_SENTINEL\n",
        )
        .unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir.path()).unwrap();
        let mut cmd = CommandBuilder::new(zsh);
        cmd.arg("-i");
        cmd.cwd(home.path());
        cmd.env("HOME", home.path());
        // The poison: both vars point at the shim itself, exactly what a
        // pane's environment hands a nested croft.
        cmd.env("ZDOTDIR", &shim);
        cmd.env("CROFT_USER_ZDOTDIR", &shim);
        let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
        let mut waited = 0u32;
        while term.prompt_lines().is_empty() {
            assert!(
                waited < 8000,
                "no prompt mark; grid: {:?}",
                term.grid_lines().0
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        term.write_input(b"echo SENTINEL_IS=$USER_RC_SENTINEL\r");
        let mut waited = 0u32;
        loop {
            let (lines, _) = term.grid_lines();
            assert!(
                !lines.iter().any(|l| l.contains("recursion limit")),
                "the shim must never source itself; grid: {lines:?}"
            );
            if lines
                .iter()
                .any(|l| l.contains("SENTINEL_IS=loaded") && !l.contains("echo"))
            {
                break;
            }
            assert!(
                waited < 8000,
                "user rc never ran under a poisoned ZDOTDIR; grid: {lines:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
    }

    /// Build a mark stream from the old `(kind, line, col, dur)` shape,
    /// numbering ids by position. The fixtures below care about pairing, not
    /// identity, so an id per row keeps them readable while still exercising
    /// the id path.
    fn views(
        marks: &[(
            crate::shell_integration::OscEvent,
            i32,
            usize,
            Option<std::time::Duration>,
        )],
    ) -> Vec<MarkView> {
        marks
            .iter()
            .enumerate()
            .map(|(i, (kind, line, col, dur))| MarkView {
                id: i as u64 + 1,
                kind: kind.clone(),
                line: *line,
                col: *col,
                dur: *dur,
            })
            .collect()
    }

    #[test]
    fn pair_decorations_matches_prompts_with_exits() {
        use crate::shell_integration::OscEvent as E;
        let ms = std::time::Duration::from_millis;
        // Two full cycles (ok then failing), then a bare prompt with no
        // command: two records, the bare prompt contributes nothing. The
        // PromptEnd cell names where the typed command starts; the output
        // span runs from the CommandStart line up to the CommandEnd line.
        let marks = [
            (E::PromptStart, -10, 0, None),
            (E::PromptEnd, -10, 5, None),
            (E::CommandStart, -9, 0, None),
            (E::CommandEnd(Some(0)), -7, 0, Some(ms(2400))),
            (E::PromptStart, -7, 0, None),
            (E::PromptEnd, -7, 5, None),
            (E::CommandStart, -6, 0, None),
            (E::CommandEnd(Some(3)), -2, 0, Some(ms(150))),
            (E::PromptStart, -2, 0, None),
        ];
        assert_eq!(
            pair_decorations(&views(&marks)),
            vec![
                CommandDecoration {
                    // The `CommandStart` mark is the third row of the
                    // fixture, and the id comes from that mark, not from the
                    // prompt before it.
                    id: 3,
                    line: -10,
                    exit: Some(0),
                    duration: Some(ms(2400)),
                    input: Some((-10, 5)),
                    output_start: -9,
                    output_end: -7,
                },
                CommandDecoration {
                    id: 7,
                    line: -7,
                    exit: Some(3),
                    duration: Some(ms(150)),
                    input: Some((-7, 5)),
                    output_start: -6,
                    output_end: -2,
                },
            ]
        );
        // A duplicate CommandEnd (a second integration layer echoing the
        // marks) has no pending CommandStart and must be dropped.
        let dup = [
            (E::PromptStart, -5, 0, None),
            (E::CommandStart, -5, 0, None),
            (E::CommandEnd(Some(0)), -4, 0, Some(ms(90))),
            (E::CommandEnd(Some(0)), -4, 0, None),
        ];
        assert_eq!(pair_decorations(&views(&dup)).len(), 1);
        // A command still running (no CommandEnd yet) has no record.
        let running = [(E::PromptStart, 0, 0, None), (E::CommandStart, 0, 0, None)];
        assert!(pair_decorations(&views(&running)).is_empty());
    }

    #[test]
    fn command_output_and_input_extract_and_select() {
        // Copy Output / Re-run Command need the exact spans back out of the
        // grid: the typed command (from the PromptEnd cell) and the output
        // rows between CommandStart and CommandEnd. Selecting the output
        // highlights precisely those rows.
        let tmp = tempfile::tempdir().unwrap();
        let script = "printf '\\033]133;A\\007$ \\033]133;B\\007cmd --flag\\n\\033]133;C\\007out1\\nout2\\n\\033]133;D;0\\007'";
        let mut term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        let mut waited = 0u32;
        while term.command_decorations().is_empty() {
            assert!(
                waited < 8000,
                "no decoration; marks: {:?}",
                term.command_marks()
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        let deco = term.command_decorations()[0];
        assert_eq!(term.command_input_text(&deco), "cmd --flag");
        assert_eq!(term.command_output_text(&deco), "out1\nout2");
        term.select_command_output(&deco);
        let sel = term.selection().expect("output must be selected");
        let (sr, _, er, _) = sel.normalised();
        assert_eq!(
            (sr, er),
            (deco.output_start, deco.output_end - 1),
            "selection covers exactly the output rows"
        );
        assert_eq!(term.selection_text(), "out1\nout2");
    }

    #[test]
    fn human_duration_formats_ms_seconds_minutes() {
        let d = std::time::Duration::from_millis;
        assert_eq!(human_duration(d(480)), "480ms");
        assert_eq!(human_duration(d(3400)), "3.4s");
        assert_eq!(human_duration(d(59_940)), "59.9s");
        assert_eq!(human_duration(d(125_000)), "2m 05s");
    }

    #[test]
    fn osc133_decorations_carry_exit_and_duration_end_to_end() {
        // A real PTY run emitting the mark protocol: the finished command
        // must surface as one decoration with its exit code and a duration
        // covering the sleep between CommandStart and CommandEnd, and the
        // same completion must arrive once through the notification drain.
        let tmp = tempfile::tempdir().unwrap();
        let script = "printf '\\033]133;A\\007$ cmd\\n\\033]133;B\\007\\033]133;C\\007'; sleep 1; printf 'out\\n\\033]133;D;2\\007'";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        let mut waited = 0u32;
        loop {
            let decos = term.command_decorations();
            if let Some(d) = decos.first() {
                assert_eq!(d.exit, Some(2), "exit code from 133;D;2");
                let dur = d.duration.expect("duration must be measured");
                // Marks are arrival-stamped by the reader thread, so a late
                // pickup of the pre-sleep chunk shrinks the measured gap
                // under suite load (#65): the floor sits at half the sleep.
                assert!(
                    dur >= std::time::Duration::from_millis(500),
                    "duration must cover the sleep; got {dur:?}"
                );
                break;
            }
            assert!(
                waited < 8000,
                "no decoration appeared; marks: {:?}",
                term.command_marks()
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        let finished = term.drain_finished_commands();
        assert_eq!(finished.len(), 1, "one completion in the drain");
        assert_eq!(finished[0].exit, Some(2));
        assert!(finished[0].dur >= std::time::Duration::from_millis(500));
        assert!(
            term.drain_finished_commands().is_empty(),
            "a second drain returns nothing"
        );
    }

    #[test]
    fn decoration_dot_paints_on_the_left_border_and_hit_tests() {
        // The finished command's prompt row gets a VS Code-style dot on the
        // pane's left border: red here (exit 2). Clicking the dot resolves
        // back to the record via decoration_at_screen.
        let tmp = tempfile::tempdir().unwrap();
        let script = "printf '\\033]133;A\\007$ cmd\\n\\033]133;B\\007\\033]133;C\\007out\\n\\033]133;D;2\\007'";
        let mut term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        let mut waited = 0u32;
        while term.command_decorations().is_empty() {
            assert!(
                waited < 8000,
                "no decoration; marks: {:?}",
                term.command_marks()
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        (&mut term).render(area, &mut buf);
        let deco = term.command_decorations()[0];
        // Live view: display_offset 0, viewport row = grid line; +1 for the
        // top border.
        let y = 1 + u16::try_from(deco.line).unwrap();
        let cell = &buf[(0, y)];
        assert_eq!(
            cell.symbol(),
            "●",
            "dot on the left border at the prompt row"
        );
        assert_eq!(
            cell.style().fg,
            Some(Color::Rgb(0xf1, 0x4c, 0x4c)),
            "non-zero exit paints the error red"
        );
        // The border above the dot is untouched.
        assert_eq!(buf[(0, y - 1)].symbol(), "│");
        assert_eq!(
            term.decoration_at_screen(0, y),
            Some(deco),
            "clicking the dot resolves the record"
        );
        assert_eq!(
            term.decoration_at_screen(0, y - 1),
            None,
            "no record on a bare border row"
        );
    }

    #[test]
    fn foreign_terminal_shim_zdotdir_chains_to_the_user_rc() {
        // Regression: Ghostty and kitty inject their own shell integration by
        // pointing ZDOTDIR at their shim dir before launching their child.
        // When that child is croft itself (ghostty --initial-command=croft),
        // croft inherited the foreign shim as the "user" dotfile dir — a dir
        // with no .zshrc — so the user's real rc (theme, aliases) never ran.
        // The wrapper contract (kitty invented it, Ghostty copied it) is that
        // the foreign .zshenv restores the REAL ZDOTDIR when sourced; croft's
        // shim must re-read ZDOTDIR after chaining and source the user's rc
        // from wherever it landed.
        let Some(zsh) = zsh_or_skip() else { return };
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".zshrc"),
            "USER_RC_SENTINEL=loaded\nexport USER_RC_SENTINEL\n",
        )
        .unwrap();
        // A Ghostty-style wrapper shim: only a .zshenv, which restores the
        // real ZDOTDIR (here: unset, meaning HOME) and chains onward.
        let foreign = tempfile::tempdir().unwrap();
        std::fs::write(
            foreign.path().join(".zshenv"),
            "builtin unset ZDOTDIR\n[[ -r \"$HOME/.zshenv\" ]] && builtin source \"$HOME/.zshenv\"\n",
        )
        .unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir.path()).unwrap();
        let mut cmd = CommandBuilder::new(zsh);
        cmd.arg("-i");
        cmd.cwd(home.path());
        cmd.env("HOME", home.path());
        cmd.env("ZDOTDIR", &shim);
        // What resolve_user_zdotdir hands the pane: the inherited foreign dir.
        cmd.env("CROFT_USER_ZDOTDIR", foreign.path());
        let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
        let mut waited = 0u32;
        while term.prompt_lines().is_empty() {
            assert!(
                waited < 8000,
                "no prompt mark; grid: {:?}",
                term.grid_lines().0
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        term.write_input(b"echo SENTINEL_IS=$USER_RC_SENTINEL\r");
        let mut waited = 0u32;
        loop {
            let (lines, _) = term.grid_lines();
            if lines
                .iter()
                .any(|l| l.contains("SENTINEL_IS=loaded") && !l.contains("echo"))
            {
                break;
            }
            assert!(
                waited < 8000,
                "user rc never ran behind a foreign terminal shim; grid: {lines:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
    }

    #[test]
    fn zsh_shim_emits_prompt_marks_end_to_end() {
        // The real proof: an interactive zsh reading croft's ZDOTDIR shim
        // must emit OSC 133 prompt marks and an OSC 7 cwd report at its
        // first prompt, with the user's own (here: empty) dotfiles sourced.
        let Some(zsh) = zsh_or_skip() else { return };
        let user_dir = tempfile::tempdir().unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir.path()).unwrap();
        let mut cmd = CommandBuilder::new(zsh);
        cmd.arg("-i");
        cmd.cwd(user_dir.path());
        cmd.env("ZDOTDIR", &shim);
        cmd.env("CROFT_USER_ZDOTDIR", user_dir.path());
        let term = PtyTerminal::spawn_with(cmd, None).unwrap();
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(1000),
            "zsh to emit a prompt mark",
            || !term.prompt_lines().is_empty(),
        );
        assert!(
            term.shell_cwd().is_some(),
            "the shim's precmd must also report the cwd via OSC 7"
        );
    }

    /// The zsh shim must emit `$PWD` percent-escaped (fish already does):
    /// the sniffer unconditionally percent-DECODES OSC 7 paths, so a real
    /// directory containing `%41` used to be reported as `A`, corrupting
    /// the cwd every consumer sees.
    #[test]
    fn a_percent_directory_round_trips_through_the_zsh_shim() {
        let Some(zsh) = zsh_or_skip() else { return };
        let user_dir = tempfile::tempdir().unwrap();
        let odd = user_dir.path().join("a%41b");
        std::fs::create_dir(&odd).unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir.path()).unwrap();
        let mut cmd = CommandBuilder::new(zsh);
        cmd.arg("-i");
        cmd.cwd(&odd);
        cmd.env("ZDOTDIR", &shim);
        cmd.env("CROFT_USER_ZDOTDIR", user_dir.path());
        let term = PtyTerminal::spawn_with(cmd, None).unwrap();
        // Two state-gated stages instead of one wall-clock guess: under full-
        // suite load, interactive zsh startup alone can eat a fixed budget and
        // the timeout measures the machine, not the shim (#109). First wait
        // for the prompt mark (proof the precmd hook ran), THEN the OSC 7
        // report — emitted by that same precmd — must follow almost at once.
        let mut waited = 0u32;
        while term.prompt_lines().is_empty() {
            assert!(
                waited < 20_000,
                "zsh never emitted a prompt mark; grid: {:?}",
                term.grid_lines().0
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        let mut waited = 0u32;
        let cwd = loop {
            if let Some(c) = term.shell_cwd() {
                break c;
            }
            assert!(
                waited < 4000,
                "the shim's precmd ran (prompt mark present) but never reported a cwd; grid: {:?}",
                term.grid_lines().0
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        };
        assert!(
            cwd.ends_with("a%41b"),
            "the literal directory name must survive the OSC 7 round trip, got {cwd:?}"
        );
    }

    /// A bash new enough for `$ENV` + `--posix` injection (>= 4.4), for the
    /// e2e tests: Homebrew/Linux bash qualifies, macOS's system 3.2 not.
    fn modern_bash() -> Option<&'static str> {
        ["/opt/homebrew/bin/bash", "/usr/local/bin/bash", "/bin/bash"]
            .into_iter()
            .find(|b| bash_env_injection_supported(b))
    }

    /// An empty Enter in a bash whose user set PROMPT_COMMAND as an ARRAY
    /// (bash 5.1+, ble.sh and prompt frameworks do) must not fabricate a
    /// command: the DEBUG-trap guard joined the array with a SPACE
    /// (`${arr[*]}` under default IFS), so its own precmd entries never
    /// matched and every blank prompt emitted a spurious 133;C + 133;D —
    /// a phantom decoration that hijacked Cmd+K Shift+R/C/S.
    #[test]
    fn an_empty_enter_with_an_array_prompt_command_fabricates_no_command() {
        let Some(bash) = modern_bash() else {
            return;
        };
        // Array PROMPT_COMMAND execution needs bash >= 5.1.
        let out = std::process::Command::new(bash)
            .args(["-c", "echo ${BASH_VERSINFO[0]}${BASH_VERSINFO[1]}"])
            .output()
            .unwrap();
        let v: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0);
        if v < 51 {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".bashrc"),
            "mytheme() { :; }\nPROMPT_COMMAND=(mytheme)\n",
        )
        .unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let mut cmd = CommandBuilder::new(bash);
        cmd.cwd(home.path());
        cmd.env("HOME", home.path());
        let args = apply_shell_integration_env(&mut cmd, bash, cfg.path());
        for a in &args {
            cmd.arg(a);
        }
        cmd.arg("-i");
        let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
        let mut waited = 0u32;
        while term.prompt_lines().is_empty() {
            assert!(waited < 8000, "bash never emitted a prompt mark");
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        let prompts_before = term.prompt_lines().len();
        term.write_input(b"\n");
        let mut waited = 0u32;
        while term.prompt_lines().len() <= prompts_before {
            assert!(waited < 8000, "the empty Enter never produced a new prompt");
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        assert!(
            pair_decorations(&term.marks_snapshot()).is_empty(),
            "an empty Enter must not fabricate a finished command"
        );
    }

    #[test]
    fn shell_integration_env_prepends_posix_for_modern_bash_only() {
        // bash is injected by inserting `--posix` before `-l` and pointing
        // $ENV at croft's shim — but only from 4.4 (the kitty/Ghostty
        // floor): macOS's system bash 3.2 ignores $ENV in posix mode, so
        // injecting would strip its startup files for nothing. zsh
        // (ZDOTDIR) and fish (XDG_DATA_DIRS) are env-only and prepend
        // nothing.
        let cfg = tempfile::tempdir().unwrap();
        if let Some(bash) = modern_bash() {
            let mut cmd = CommandBuilder::new(bash);
            assert_eq!(
                apply_shell_integration_env(&mut cmd, bash, cfg.path()),
                vec!["--posix".to_string()]
            );
        }
        assert!(!parse_bash_version_supported("3.2"));
        assert!(!parse_bash_version_supported("4.3"));
        assert!(!parse_bash_version_supported("garbage"));
        assert!(parse_bash_version_supported("4.4"));
        assert!(parse_bash_version_supported("5.2"));
        assert!(parse_bash_version_supported("10.0"));
        if std::path::Path::new("/bin/bash").exists() && !bash_env_injection_supported("/bin/bash")
        {
            // On a macOS box with system bash 3.2: no injection, no args.
            let mut cmd = CommandBuilder::new("/bin/bash");
            assert!(
                apply_shell_integration_env(&mut cmd, "/bin/bash", cfg.path()).is_empty(),
                "bash 3.2 must spawn clean, without posix injection"
            );
        }
        for shell in ["/bin/zsh", "/usr/bin/fish", "/bin/dash"] {
            let mut cmd = CommandBuilder::new(shell);
            assert!(
                apply_shell_integration_env(&mut cmd, shell, cfg.path()).is_empty(),
                "{shell} must not gain args"
            );
        }
    }

    #[test]
    fn zsh_shim_marks_the_prompt_end_for_input_extraction() {
        // Copy Output / Re-run Command read the typed command from the
        // PromptEnd (133;B) mark. The shim must emit it itself — relying on
        // a chained foreign integration (Ghostty's) leaves remote Linux
        // panes with no input span at all.
        let Some(zsh) = zsh_or_skip() else { return };
        let user_dir = tempfile::tempdir().unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir.path()).unwrap();
        let mut cmd = CommandBuilder::new(zsh);
        cmd.arg("-i");
        cmd.cwd(user_dir.path());
        cmd.env("ZDOTDIR", &shim);
        cmd.env("CROFT_USER_ZDOTDIR", user_dir.path());
        let term = PtyTerminal::spawn_with(cmd, None).unwrap();
        let mut waited = 0u32;
        loop {
            let marks = term.command_marks();
            if marks
                .iter()
                .any(|(k, _)| *k == crate::shell_integration::OscEvent::PromptEnd)
            {
                break;
            }
            assert!(
                waited < 8000,
                "zsh never emitted a PromptEnd mark; marks: {marks:?}, grid: {:?}",
                term.grid_lines().0
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
    }

    #[test]
    fn bash_shim_via_posix_env_emits_marks_and_sources_the_user_profile() {
        // The kitty/Ghostty bash injection end-to-end, exactly as croft
        // spawns it: `bash --posix -l` with ENV pointing at croft's shim.
        // In posix mode bash reads ONLY $ENV, so the shim must back out of
        // posix mode, replay the login startup files (the user's
        // .bash_profile here), and emit the full mark cycle with exit codes.
        // Needs a bash >= 4.4 (macOS system 3.2 ignores $ENV in posix mode
        // and is never injected).
        let Some(bash) = modern_bash() else {
            return;
        };
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".bash_profile"),
            "USER_RC_SENTINEL=loaded\nexport USER_RC_SENTINEL\n",
        )
        .unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        let shim = crate::shell_integration::ensure_bash_shim(cfg_dir.path()).unwrap();
        let mut cmd = CommandBuilder::new(bash);
        cmd.arg("--posix");
        cmd.arg("-l");
        cmd.cwd(home.path());
        cmd.env("HOME", home.path());
        cmd.env("ENV", &shim);
        cmd.env("CROFT_BASH_INJECT", "1");
        let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
        let mut waited = 0u32;
        while term.prompt_lines().is_empty() {
            assert!(
                waited < 8000,
                "bash never emitted a prompt mark; grid: {:?}",
                term.grid_lines().0
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        assert!(
            term.shell_cwd().is_some(),
            "the shim's precmd must report cwd via OSC 7"
        );
        term.write_input(b"echo SENTINEL_IS=$USER_RC_SENTINEL\r");
        let mut waited = 0u32;
        loop {
            let (lines, _) = term.grid_lines();
            if lines
                .iter()
                .any(|l| l.contains("SENTINEL_IS=loaded") && !l.contains("echo"))
            {
                break;
            }
            assert!(
                waited < 8000,
                "user .bash_profile never ran through the shim; grid: {lines:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        // A failing command must round-trip its exit code through 133;D and
        // the shim must mark the prompt end for input extraction.
        term.write_input(b"false\r");
        let mut waited = 0u32;
        loop {
            use crate::shell_integration::OscEvent as E;
            let marks = term.command_marks();
            if marks.iter().any(|(k, _)| *k == E::CommandEnd(Some(1))) {
                assert!(
                    marks.iter().any(|(k, _)| *k == E::PromptEnd),
                    "bash PS1 must carry the 133;B input-start mark; marks: {marks:?}"
                );
                break;
            }
            assert!(
                waited < 8000,
                "no CommandEnd(1) after `false`; marks: {marks:?}, grid: {:?}",
                term.grid_lines().0
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
    }

    /// A fish binary for the e2e: common install paths, then `$PATH`.
    /// Absent on plain macOS (skips), present on Linux boxes and via nix.
    fn find_fish() -> Option<String> {
        let fixed = [
            "/opt/homebrew/bin/fish",
            "/usr/local/bin/fish",
            "/usr/bin/fish",
        ];
        if let Some(f) = fixed.iter().find(|f| std::path::Path::new(f).exists()) {
            return Some((*f).to_string());
        }
        let path = std::env::var("PATH").ok()?;
        path.split(':')
            .map(|d| std::path::Path::new(d).join("fish"))
            .find(|p| p.is_file())
            .map(|p| p.display().to_string())
    }

    #[test]
    fn fish_integration_restores_xdg_dirs_and_yields_single_native_marks() {
        // fish end-to-end through croft's injection env: fish 4 must emit
        // the marks natively (croft's vendor script installs NO second
        // emitter, unlike Ghostty's which double-marks), the exit code must
        // round-trip, and the script must scrub the injection from the
        // session env so children never see it.
        let Some(fish) = find_fish() else {
            return;
        };
        let home = tempfile::tempdir().unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        let dir = crate::shell_integration::ensure_fish_integration(cfg_dir.path()).unwrap();
        let mut cmd = CommandBuilder::new(&fish);
        cmd.arg("-i");
        cmd.arg("-l");
        cmd.cwd(home.path());
        cmd.env("HOME", home.path());
        cmd.env(
            "XDG_DATA_DIRS",
            crate::shell_integration::fish_xdg_data_dirs(&dir, None),
        );
        cmd.env("CROFT_FISH_XDG_DATA_DIR", &dir);
        let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
        let mut waited = 0u32;
        while term.prompt_lines().is_empty() {
            assert!(
                waited < 8000,
                "fish never emitted a prompt mark; grid: {:?}",
                term.grid_lines().0
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        term.write_input(b"false\r");
        let mut waited = 0u32;
        loop {
            use crate::shell_integration::OscEvent as E;
            let marks = term.command_marks();
            if marks.iter().any(|(k, _)| *k == E::CommandEnd(Some(1))) {
                break;
            }
            assert!(
                waited < 8000,
                "no CommandEnd(1) after `false`; marks: {marks:?}, grid: {:?}",
                term.grid_lines().0
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        // Exactly one finished record for `false`: a second emitter would
        // double-decorate every command.
        let failing: Vec<_> = term
            .command_decorations()
            .into_iter()
            .filter(|d| d.exit == Some(1))
            .collect();
        assert_eq!(failing.len(), 1, "duplicate decorations: {failing:?}");
        // The injection env must be gone inside the session.
        term.write_input(b"echo DIRS=$XDG_DATA_DIRS; set -q CROFT_FISH_XDG_DATA_DIR; and echo SI_LEAKED; or echo SI_CLEAN\r");
        let needle = format!("{}", dir.display());
        let mut waited = 0u32;
        loop {
            let (lines, _) = term.grid_lines();
            if lines
                .iter()
                .any(|l| l.contains("SI_CLEAN") && !l.contains("echo"))
            {
                assert!(
                    !lines
                        .iter()
                        .any(|l| l.contains("SI_LEAKED") && !l.contains("echo")),
                    "grid: {lines:?}"
                );
                let dirs_line = lines
                    .iter()
                    .find(|l| l.contains("DIRS=") && !l.contains("echo"))
                    .cloned()
                    .unwrap_or_default();
                assert!(
                    !dirs_line.contains(&needle),
                    "XDG_DATA_DIRS still carries the injection dir: {dirs_line}"
                );
                break;
            }
            assert!(
                waited < 8000,
                "env scrub probe never answered; grid: {lines:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
    }

    #[test]
    fn prompt_jump_targets_walk_previous_and_next() {
        // Pure chooser used by Cmd+Up / Cmd+Down: pick the nearest prompt
        // line above / below the current viewport top, if any.
        let prompts = [-30, -12, 0];
        assert_eq!(pick_prompt_jump(&prompts, 0, false), Some(-12));
        assert_eq!(pick_prompt_jump(&prompts, -12, false), Some(-30));
        assert_eq!(pick_prompt_jump(&prompts, -30, false), None);
        assert_eq!(pick_prompt_jump(&prompts, -30, true), Some(-12));
        assert_eq!(pick_prompt_jump(&prompts, -12, true), Some(0));
        assert_eq!(pick_prompt_jump(&prompts, 0, true), None);
    }

    #[test]
    fn bell_sets_a_flag_the_app_can_drain() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "printf 'DONE\\007\\n'";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        // Wait for the child's output row, not the run header echoing the
        // command text — the BEL byte arrives with the output.
        wait_for_grid(&term, |ls| {
            ls.iter()
                .any(|l| l.contains("DONE") && !l.contains("printf"))
        });
        assert!(
            term.take_bell(),
            "BEL from the child must set the bell flag for the app to drain"
        );
        assert!(
            !term.take_bell(),
            "take_bell drains the flag — a second read is false"
        );
    }

    #[test]
    fn grid_lines_exposes_output_for_terminal_find() {
        // The find bar searches the grid text this returns. Spawn a program
        // that prints a known needle, wait for it, and confirm the search
        // helpers locate it on the mapped grid line.
        let tmp = tempfile::tempdir().unwrap();
        let needle = "croft-find-needle-42";
        let term =
            PtyTerminal::new_running("/bin/echo", &[String::from(needle)], tmp.path()).unwrap();
        let mut waited_ms = 0u32;
        loop {
            let (lines, top) = term.grid_lines();
            if let Some(row) = lines.iter().position(|l| l.contains(needle)) {
                let hit = crate::widgets::editor_find::find_next_match(
                    &lines,
                    needle,
                    crate::widgets::search::SearchOpts::default(),
                    0,
                    0,
                    false,
                )
                .expect("search must find the printed needle");
                assert_eq!(hit.row, row, "match row must line up with the grid row");
                // `top + row` maps the hit back to an absolute grid line the
                // viewport can scroll to; for the live screen that is >= 0.
                assert!(top + hit.row as i32 >= top);
                break;
            }
            assert!(waited_ms < 4000, "echo output never reached the grid");
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited_ms += 20;
        }
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
        // The wait is load-scaled (#307): /bin/echo through a PTY is
        // milliseconds on a quiet box and seconds when the suite is spawning
        // dozens of shells at once, and no constant here can know which.
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(500),
            "/bin/echo to deliver a byte through the PTY",
            || term.peek_pending_bytes() > 0,
        );
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
            block: false,
        };
        assert_eq!(s.normalised(), (2, 1, 5, 4));
    }

    #[test]
    fn selection_normalised_handles_same_row() {
        let s = Selection {
            anchor: (3, 9),
            head: (3, 2),
            block: false,
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
            block: false,
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
    fn selection_stays_glued_to_its_content_while_output_streams() {
        // A selection made while a program keeps printing (Claude Code, any
        // streaming TUI) must track its content into scrollback, not stay at
        // fixed grid lines while the text slides underneath.
        let tmp = tempfile::tempdir().unwrap();
        let go = tmp.path().join("go");
        // Sentinel assembled at runtime so the `▶ sh -c …` run-label header
        // (which echoes the script text) can never match it.
        let script = format!(
            "t=TAR; echo \"${{t}}GET-line\"; until [ -e {} ]; do sleep 0.05; done; seq 1 60; sleep 30",
            go.display()
        );
        let mut term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
        let mut waited = 0u32;
        let line = loop {
            let (lines, top) = term.grid_lines();
            if let Some(idx) = lines.iter().position(|l| l.starts_with("TARGET-line")) {
                break top + idx as i32;
            }
            assert!(waited < 8000, "sentinel never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        };
        term.set_selection(Some(Selection {
            anchor: (line, 0),
            head: (line, 30),
            block: false,
        }));
        assert_eq!(term.selection_text(), "TARGET-line");

        // Release the second phase: 60 more lines scroll the buffer.
        std::fs::File::create(&go).unwrap();
        let mut waited = 0u32;
        while !term.grid_lines().0.iter().any(|l| l == "60") {
            assert!(waited < 8000, "streamed output never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        assert_eq!(
            term.selection_text(),
            "TARGET-line",
            "the selection must follow its content as output streams past it"
        );
        let sel = term.selection().expect("selection survives streaming");
        assert!(
            sel.anchor.0 < line,
            "reported endpoints re-anchor upward as lines scroll into history"
        );
    }

    #[test]
    fn selection_survives_streaming_past_a_saturated_scrollback() {
        // Once a pane has scrolled more than SCROLLBACK_LINES, history_size
        // saturates at the cap while content keeps rotating through the
        // ring. A selection made in that state must still track its content
        // as more output streams (the live Claude Code pane: long-running,
        // scrollback full since forever).
        let tmp = tempfile::tempdir().unwrap();
        let go = tmp.path().join("go");
        let script = format!(
            "seq 1 5200; t=TAR; echo \"${{t}}GET-line\"; until [ -e {} ]; do sleep 0.05; done; seq 1 60; sleep 30",
            go.display()
        );
        let mut term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
        let mut waited = 0u32;
        let line = loop {
            let (lines, top) = term.grid_lines();
            if let Some(idx) = lines.iter().position(|l| l.starts_with("TARGET-line")) {
                break top + idx as i32;
            }
            assert!(waited < 8000, "sentinel never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        };
        let hist_at_select = term.term.lock().grid().history_size();
        term.set_selection(Some(Selection {
            anchor: (line, 0),
            head: (line, 30),
            block: false,
        }));
        assert_eq!(term.selection_text(), "TARGET-line");

        std::fs::File::create(&go).unwrap();
        let mut waited = 0u32;
        while !term.grid_lines().0.iter().any(|l| l == "60") {
            assert!(waited < 8000, "streamed output never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
        // Premise of this test: the scrollback was already full, so
        // history_size never moved even though 60 lines scrolled past.
        assert_eq!(
            term.term.lock().grid().history_size(),
            hist_at_select,
            "test setup must saturate the scrollback before streaming"
        );
        assert_eq!(
            term.selection_text(),
            "TARGET-line",
            "the selection must follow its content even with history_size saturated"
        );
    }

    #[test]
    fn alt_screen_selection_stays_partially_visible_when_app_chrome_covers_its_edge() {
        // Scrolling Claude Code slides the selected block toward its input
        // box; the rows that reach it are overdrawn by the box while the
        // rest stay visible. The surviving rows must keep their highlight
        // (matched as a partial block), and copy must still yield the whole
        // remembered selection.
        let (_tmp, mut t) = quiet_pty();
        feed_pty(
            &t,
            b"\x1b[?1049h\x1b[H\x1b[2Jalpha one\r\nbravo two\r\ncharlie three\r\ndelta four\r\necho five",
        );
        t.set_selection(Some(Selection {
            anchor: (1, 0),
            head: (3, 9),
            block: false,
        }));
        assert_eq!(t.selection_text(), "bravo two\ncharlie three\ndelta four");

        // The app scrolls one line; the block's last row slides under the
        // input box, which repaints over it. Rows 0-1 of the block survive
        // at lines 0-1; line 2 onward is app chrome.
        feed_pty(
            &t,
            b"\x1b[H\x1b[2Jbravo two\r\ncharlie three\r\n> INPUT BOX\r\n> more chrome\r\n> even more",
        );
        t.rebase_selection();
        let anchor = t.alt_sel.as_ref().expect("anchor survives");
        assert!(
            !anchor.dormant,
            "two of three selected rows are still on screen: not dormant"
        );
        assert_eq!(
            anchor.visible,
            Some(vec![(0, 0, u16::MAX), (1, 0, u16::MAX)]),
            "only the surviving rows keep their highlight"
        );
        assert_eq!(
            t.selection_text(),
            "bravo two\ncharlie three\ndelta four",
            "copy yields the whole remembered selection while partially covered"
        );

        // Scrolled back: the whole block is visible again, fully live.
        feed_pty(
            &t,
            b"\x1b[H\x1b[2Jalpha one\r\nbravo two\r\ncharlie three\r\ndelta four\r\necho five",
        );
        t.rebase_selection();
        let anchor = t.alt_sel.as_ref().expect("anchor survives");
        assert!(!anchor.dormant);
        assert_eq!(anchor.visible, None, "fully visible again: no clip");
        assert_eq!(t.selection_text(), "bravo two\ncharlie three\ndelta four");
    }

    #[test]
    fn a_single_row_selection_on_a_repeated_row_reanchors_to_its_own_copy() {
        // A one-row selection has a one-row fingerprint, and full-screen
        // apps repeat rows verbatim (divider rules, continuation markers).
        // After a repaint the nearest identical copy may be a DIFFERENT
        // copy: the neighbours captured with the anchor must break the tie
        // so the highlight follows its own row, not the closest lookalike.
        let (_tmp, mut t) = quiet_pty();
        feed_pty(
            &t,
            b"\x1b[?1049h\x1b[H\x1b[2Jalpha\r\n-----\r\nbravo\r\n-----\r\ncharlie",
        );
        // Select the SECOND divider (row 3), between bravo and charlie.
        t.set_selection(Some(Selection {
            anchor: (3, 0),
            head: (3, 4),
            block: false,
        }));
        assert_eq!(t.selection_text(), "-----");

        // The app scrolls two rows: the selected divider moves to row 1
        // (still between bravo and charlie), while a new divider appears
        // at row 4 — NEARER to the old position than the real one.
        feed_pty(
            &t,
            b"\x1b[H\x1b[2Jbravo\r\n-----\r\ncharlie\r\ndelta\r\n-----",
        );
        t.rebase_selection();
        let anchor = t.alt_sel.as_ref().expect("anchor survives");
        assert_eq!(
            anchor.top, 1,
            "the highlight must follow its own copy (neighbours bravo/charlie), not the nearest lookalike"
        );
        assert_eq!(
            t.selection.map(|s| (s.anchor, s.head)),
            Some(((1, 0), (1, 4))),
            "the selection coordinates must move with the content"
        );
        assert_eq!(t.selection_text(), "-----");
    }

    /// Drive a PtyTerminal's grid directly: parse `bytes` into its term as
    /// if the child had printed them. The child (`sleep`) stays silent, so
    /// the grid is exactly what the test painted.
    fn feed_pty(t: &PtyTerminal, bytes: &[u8]) {
        let mut p = Processor::<StdSyncHandler>::new();
        let mut term = t.term.lock();
        p.advance(&mut *term, bytes);
    }

    fn quiet_pty() -> (tempfile::TempDir, PtyTerminal) {
        let tmp = tempfile::tempdir().unwrap();
        let t = PtyTerminal::new_running("/bin/sleep", &[String::from("30")], tmp.path()).unwrap();
        (tmp, t)
    }

    /// An alt-screen round trip (`git log`, vim, htop) used to wipe every
    /// arrival stamp (the alternate grid has no history, so the cursor's
    /// absolute id collapsed and looked like an ED 3 wipe) and then
    /// re-stamp the whole scrollback with the exit time. Stamps are keyed
    /// on the scroll clock now, which the alternate screen freezes.
    #[test]
    fn timestamps_survive_an_alt_screen_round_trip() {
        let (_tmp, t) = quiet_pty();
        let mut prev = 0i64;
        feed_pty(&t, b"one\r\ntwo\r\nthree\r\n");
        t.stamp_chunk_for_test(&mut prev, 1000);
        let before = t.line_time_entries_for_test();
        assert!(!before.is_empty(), "rows got stamped");
        let (first_id, first_ms) = before[0];
        assert_eq!(first_ms, 1000);
        feed_pty(&t, b"\x1b[?1049h\x1b[Hpager-screen");
        t.stamp_chunk_for_test(&mut prev, 2000);
        feed_pty(&t, b"\x1b[?1049l");
        t.stamp_chunk_for_test(&mut prev, 3000);
        let after = t.line_time_entries_for_test();
        assert_eq!(
            before.iter().map(|&(k, _)| k).collect::<Vec<_>>(),
            after.iter().map(|&(k, _)| k).collect::<Vec<_>>(),
            "an alt-screen trip must not wipe or invent stamped rows"
        );
        let kept = after.iter().find(|&&(k, _)| k == first_id).unwrap().1;
        assert_eq!(
            kept, 1000,
            "an old row must keep its true arrival time, not the pager's exit time"
        );
    }

    /// Once the scrollback ring saturates, `history_size` stops moving, so
    /// history-keyed row ids froze to screen positions: new rows overwrote
    /// the same ids and the gutter looked up unrelated rows' times. Clock
    /// ids keep advancing one per scrolled line forever.
    #[test]
    fn timestamps_keep_advancing_past_a_saturated_scrollback() {
        let (_tmp, t) = quiet_pty();
        let mut prev = 0i64;
        let mut chunk = String::new();
        for i in 0..5300 {
            chunk.push_str(&format!("s-{i}\r\n"));
        }
        feed_pty(&t, chunk.as_bytes());
        t.stamp_chunk_for_test(&mut prev, 1000);
        let max1 = t
            .line_time_entries_for_test()
            .last()
            .map(|&(k, _)| k)
            .unwrap();
        let mut more = String::new();
        for i in 0..100 {
            more.push_str(&format!("t-{i}\r\n"));
        }
        feed_pty(&t, more.as_bytes());
        t.stamp_chunk_for_test(&mut prev, 2000);
        let max2 = t
            .line_time_entries_for_test()
            .last()
            .map(|&(k, _)| k)
            .unwrap();
        assert!(
            max2 >= max1 + 100,
            "row ids must keep advancing after saturation (got {max1} then {max2})"
        );
    }

    /// An inline image captured after the scrollback saturated must ride
    /// the ring rotation like the content it anchors: `history_size` stops
    /// growing there, so a history-delta anchor freezes onto a viewport
    /// row and the picture sits on top of every later command's output.
    #[test]
    fn an_inline_image_rides_scrollback_past_saturation() {
        let (_tmp, t) = quiet_pty();
        let mut chunk = String::new();
        for i in 0..5200 {
            chunk.push_str(&format!("pre-{i}\r\n"));
        }
        feed_pty(&t, chunk.as_bytes());
        feed_pty(&t, b"image-anchor-row");
        t.push_image_for_test(vec![9, 9, 9]);
        feed_pty(&t, b"\r\n");
        let find_anchor = |t: &PtyTerminal| {
            let (lines, top) = t.grid_lines();
            top + lines
                .iter()
                .position(|l| l.starts_with("image-anchor-row"))
                .unwrap() as i32
        };
        assert_eq!(
            t.pane_images().pop().unwrap().line,
            find_anchor(&t),
            "the anchor starts on its row"
        );
        let mut more = String::new();
        for i in 0..60 {
            more.push_str(&format!("post-{i}\r\n"));
        }
        feed_pty(&t, more.as_bytes());
        assert_eq!(
            t.pane_images().pop().unwrap().line,
            find_anchor(&t),
            "the anchor must follow its content through ring rotation"
        );
    }

    /// Typing `clear` reaches the grid as ED 3 through the PTY reader
    /// thread (modern terminfo wipes scrollback), never through the pane's
    /// own Clear method: the reader must drop captured images too, or the
    /// picture floats over the fresh prompt.
    #[test]
    fn a_program_emitted_scrollback_wipe_drops_captured_images() {
        use base64::Engine;
        let mut png_buf = Vec::new();
        image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 255, 0, 255]))
            .write_to(
                &mut std::io::Cursor::new(&mut png_buf),
                image::ImageFormat::Png,
            )
            .expect("encode test png");
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);
        let tmp = tempfile::tempdir().unwrap();
        // Fill lines push real content into scrollback first (ED 3 can only
        // shrink a nonempty history); the wipe then waits on stdin so the
        // test can observe the capture before it lands.
        let script = format!(
            "i=0; while [ $i -lt 60 ]; do echo fill-$i; i=$((i+1)); done; printf '\\033]1337;File=inline=1:{b64}\\007\\n'; read x; printf '\\033[3J\\033[2J'; printf 'wiped\\n'; sleep 30"
        );
        let mut term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
        let mut waited = 0u32;
        while term.pane_images().is_empty() {
            assert!(waited < 8000, "inline image never captured");
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        term.write_input(b"\n");
        wait_for_grid(&term, |ls| ls.iter().any(|l| l.starts_with("wiped")));
        assert!(
            term.pane_images().is_empty(),
            "an ED 3 scrollback wipe from the program must drop the pane's images"
        );
    }

    /// A wipe with EMPTY scrollback (picture captured on the first screen,
    /// nothing scrolled into history yet) must drop the image too: the
    /// history-shrink heuristic saw 0 → 0 and kept the stale overlay.
    #[test]
    fn a_zero_history_screen_wipe_drops_captured_images() {
        use base64::Engine;
        let mut png_buf = Vec::new();
        image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]))
            .write_to(
                &mut std::io::Cursor::new(&mut png_buf),
                image::ImageFormat::Png,
            )
            .expect("encode test png");
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);
        let tmp = tempfile::tempdir().unwrap();
        let script = format!(
            "printf '\\033]1337;File=inline=1:{b64}\\007\\n'; read x; printf '\\033[2J\\033[3J\\033[H'; printf 'wiped\\n'; sleep 30"
        );
        let mut term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
        let mut waited = 0u32;
        while term.pane_images().is_empty() {
            assert!(waited < 8000, "inline image never captured");
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        term.write_input(b"\n");
        wait_for_grid(&term, |ls| ls.iter().any(|l| l.starts_with("wiped")));
        assert!(
            term.pane_images().is_empty(),
            "a screen wipe with empty history must still drop the pane's images"
        );
    }

    /// CAN/SUB cancel an incomplete CSI and an ESC inside one begins a new
    /// escape (VT100): the wipe sniffer's CSI walk must not swallow the
    /// sequence that follows a cancelled CSI, or a wipe (or alt entry)
    /// right after one goes unseen.
    #[test]
    fn a_cancelled_csi_does_not_hide_a_wipe_from_the_sniffer() {
        let mut w = WipeSniffer::default();
        // ESC inside an incomplete CSI: the ED 3 after it must be seen.
        let (_s, h) = w.scan(b"\x1b[12\x1b[3J", false);
        assert!(h, "ESC-in-CSI swallowed the scrollback wipe");
        // CAN aborts; the alt entry after it must be seen, so the ED 2
        // inside the alt screen is correctly ignored.
        let mut w = WipeSniffer::default();
        let (s, _h) = w.scan(b"\x1b[\x18\x1b[?1049h\x1b[2J", false);
        assert!(!s, "CAN-cancelled CSI hid the alt entry from the sniffer");
    }

    /// `clear && vim`: the wipe and the alt-screen entry land in ONE PTY
    /// chunk. Judging the wipe by the chunk's FINAL mode attributed it to
    /// the alt screen and kept the invalidated primary images, which then
    /// reappeared over unrelated content when the app exited.
    #[test]
    fn a_wipe_followed_by_alt_entry_in_one_chunk_still_drops_images() {
        use base64::Engine;
        let mut png_buf = Vec::new();
        image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 255, 0, 255]))
            .write_to(
                &mut std::io::Cursor::new(&mut png_buf),
                image::ImageFormat::Png,
            )
            .expect("encode test png");
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);
        let tmp = tempfile::tempdir().unwrap();
        // The wipe and the alt ENTRY share one write (one PTY chunk, the
        // pane still in the alt screen when it ends); the exit comes later.
        let script = format!(
            "printf '\\033]1337;File=inline=1:{b64}\\007\\n'; read x; printf '\\033[2J\\033[3J\\033[?1049halt-owns-this'; sleep 1; printf '\\033[?1049l'; printf 'back\\n'; sleep 30"
        );
        let mut term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
        let mut waited = 0u32;
        while term.pane_images().is_empty() {
            assert!(waited < 8000, "inline image never captured");
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited += 40;
        }
        term.write_input(b"\n");
        wait_for_grid(&term, |ls| ls.iter().any(|l| l.starts_with("back")));
        assert!(
            term.pane_images().is_empty(),
            "the wipe preceded the alt entry, so the primary images are gone"
        );
    }

    /// Clearing the pane erases the content the OSC 133 marks anchored to:
    /// keeping them used to invert their drift math (history_size zeroed)
    /// so stale decoration dots later resurfaced on unrelated output rows,
    /// where clicking one copied or re-ran the wrong text.
    #[test]
    fn clearing_the_terminal_drops_its_marks() {
        use crate::shell_integration::OscEvent;
        let (_tmp, mut t) = quiet_pty();
        feed_pty(&t, b"\r\n$ ");
        t.push_mark_for_test(OscEvent::PromptStart, 0);
        t.push_mark_for_test(OscEvent::PromptEnd, 2);
        feed_pty(&t, b"cmd\r\nout\r\n");
        t.push_mark_for_test(OscEvent::CommandStart, 0);
        t.push_mark_for_test(OscEvent::CommandEnd(Some(0)), 0);
        assert_eq!(
            pair_decorations(&t.marks_snapshot()).len(),
            1,
            "staging: one finished command"
        );
        t.clear_screen_and_scrollback();
        assert!(
            pair_decorations(&t.marks_snapshot()).is_empty(),
            "cleared content keeps no command decorations"
        );
    }

    /// A decoration's id names the same command for as long as that command
    /// exists, and a positional index does not (#440).
    ///
    /// Asserted as a CONTRAST, because that is the actual claim: the same
    /// eviction is shown moving the index while leaving the id alone. An
    /// assertion that only checked "id 7 is still 7" would pass against a
    /// collector that renumbered from zero, since nothing would have moved.
    ///
    /// Driven through `pair_decorations` rather than a live pane: both
    /// eviction paths (the scrollback GC in `marks_snapshot`, the `MARKS_MAX`
    /// drain in the reader thread) end in the same observable state — a
    /// prefix of the mark stream is gone — and constructing that state
    /// directly tests the property at the level it actually holds, instead of
    /// depending on how a PTY happens to schedule its scrollback.
    #[test]
    fn a_decoration_id_outlives_the_eviction_that_moves_its_index() {
        use crate::shell_integration::OscEvent as E;
        let cycle = |line: i32| {
            [
                (E::PromptStart, line, 0, None),
                (E::PromptEnd, line, 2, None),
                (E::CommandStart, line + 1, 0, None),
                (E::CommandEnd(Some(0)), line + 2, 0, None),
            ]
        };
        let mut marks = Vec::new();
        for (n, line) in [-12, -8, -4].iter().enumerate() {
            marks.extend(cycle(*line));
            assert_eq!(marks.len(), (n + 1) * 4, "staging: four marks per cycle");
        }
        let views = views(&marks);

        let before = pair_decorations(&views);
        assert_eq!(before.len(), 3, "staging: three finished commands");
        assert!(
            before[0].id < before[1].id && before[1].id < before[2].id,
            "ids ascend with command order: {:?}",
            before.iter().map(|d| d.id).collect::<Vec<_>>()
        );

        // Evict the oldest command: its four marks leave the front of the
        // stream, exactly as the scrollback GC and the MARKS_MAX drain both
        // leave it. Ids are NOT reassigned, because they belong to the marks.
        let after = pair_decorations(&views[4..]);
        assert_eq!(after.len(), 2, "staging: one command was evicted");

        // The index has moved. The command that was last is still last here,
        // so the telling case is the MIDDLE one: it was at index 1 and is now
        // at index 0, which is what silently hands a caller the wrong command.
        assert_eq!(
            before[1].id, after[0].id,
            "the survivor moved down one slot"
        );
        assert_ne!(
            before[1].id, before[0].id,
            "staging: the slots must hold different commands"
        );
        assert_ne!(
            after[0].id,
            before.first().map(|d| d.id).unwrap(),
            "index 0 now names a different command than it did"
        );

        // The id still names the SAME command: same span, same exit.
        let found = after
            .iter()
            .find(|d| d.id == before[2].id)
            .expect("a surviving command keeps its id");
        assert_eq!(
            (found.exit, found.output_start, found.output_end),
            (before[2].exit, before[2].output_start, before[2].output_end),
            "the id must name the same command, not merely some decoration"
        );

        // And an evicted id reads as gone rather than as its neighbour, which
        // is the whole difference from a positional index.
        assert!(
            after.iter().all(|d| d.id != before[0].id),
            "an evicted command's id must never be reused by a survivor"
        );
    }

    /// A command outlives its own prompt marks, and the id says so (#440).
    ///
    /// The GC retains per MARK, not per command, and the prompt marks sit on
    /// earlier rows than the command's own — so the floor takes them first.
    /// The interesting case is therefore not the clean boundary the test
    /// above cuts, but this one: the same command, still identified, with its
    /// presentation fields degraded. Pinning it here so the limit documented
    /// on `CommandDecoration::id` cannot quietly stop being true.
    #[test]
    fn a_command_can_outlive_its_prompt_marks() {
        use crate::shell_integration::OscEvent as E;
        let marks = [
            (E::PromptStart, -10, 0, None),
            (E::PromptEnd, -10, 5, None),
            (E::CommandStart, -9, 0, None),
            (E::CommandEnd(Some(0)), -7, 0, None),
        ];
        let views = views(&marks);
        let whole = pair_decorations(&views);
        assert_eq!(whole.len(), 1, "staging: one finished command");

        // The floor rises past the prompt row but not past the command's.
        let survivors: Vec<MarkView> = views.iter().filter(|m| m.line >= -9).cloned().collect();
        assert_eq!(survivors.len(), 2, "staging: only the prompt marks evict");
        let after = pair_decorations(&survivors);
        assert_eq!(after.len(), 1, "the command itself survives");

        // Identity and the output span are untouched.
        assert_eq!(after[0].id, whole[0].id, "the id names the same command");
        assert_eq!(
            (after[0].exit, after[0].output_start, after[0].output_end),
            (whole[0].exit, whole[0].output_start, whole[0].output_end),
            "the fields taken from CommandStart/CommandEnd do not drift"
        );

        // The presentation fields DO degrade, exactly as documented.
        assert_eq!(
            whole[0].input,
            Some((-10, 5)),
            "staging: the typed-text position existed before the eviction"
        );
        assert_eq!(
            after[0].input, None,
            "the typed-text position goes with the prompt mark"
        );
        assert_eq!(whole[0].line, -10, "staging: the row was the prompt's");
        assert_eq!(
            after[0].line, after[0].output_start,
            "the row falls back to the command's own line"
        );
    }

    /// Ids are unique across panes, and a wipe does not restart them (#440).
    ///
    /// The counter is process-wide precisely so an id means one thing
    /// wherever it travels; a per-pane counter would hand two panes the same
    /// id and a wipe would hand the same pane its old ids back. Both are the
    /// kind of thing that looks fine until a consumer keys on the id, so both
    /// are pinned rather than left to the comment.
    #[test]
    fn ids_are_unique_across_panes_and_survive_a_wipe() {
        use crate::shell_integration::OscEvent;
        let run = |t: &PtyTerminal| {
            feed_pty(t, b"\r\n$ ");
            t.push_mark_for_test(OscEvent::PromptStart, 0);
            t.push_mark_for_test(OscEvent::PromptEnd, 2);
            feed_pty(t, b"cmd\r\nout\r\n");
            t.push_mark_for_test(OscEvent::CommandStart, 0);
            t.push_mark_for_test(OscEvent::CommandEnd(Some(0)), 0);
        };

        let (_a, mut a) = quiet_pty();
        let (_b, b) = quiet_pty();
        run(&a);
        run(&b);
        run(&a);
        let mut ids: Vec<u64> = a
            .command_decorations()
            .iter()
            .chain(b.command_decorations().iter())
            .map(|d| d.id)
            .collect();
        assert_eq!(ids.len(), 3, "staging: three commands across two panes");
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "ids must not collide between panes");

        // A wipe clears the pane's marks; the counter is not reset, so the
        // next command must not be handed an id the wipe just freed.
        let highest = *ids.last().unwrap();
        a.clear_screen_and_scrollback();
        assert!(
            a.command_decorations().is_empty(),
            "staging: the wipe must actually clear the marks"
        );
        run(&a);
        let after = a.command_decorations();
        assert_eq!(after.len(), 1, "staging: one command since the wipe");
        assert!(
            after[0].id > highest,
            "an id must not be reused after a wipe: {} came after {highest}",
            after[0].id
        );
    }

    /// A prompt mark recorded after the scrollback saturated must ride the
    /// ring rotation like its content: under the history-size model the
    /// delta froze, so the mark (and its decoration dot) drifted off its
    /// row as output streamed.
    #[test]
    fn a_prompt_mark_rides_scrollback_past_saturation() {
        use crate::shell_integration::OscEvent;
        let (_tmp, mut t) = quiet_pty();
        let mut chunk = String::new();
        for i in 0..5200 {
            chunk.push_str(&format!("pre-{i}\r\n"));
        }
        feed_pty(&t, chunk.as_bytes());
        feed_pty(&t, b"$ marker-prompt");
        t.push_mark_for_test(OscEvent::PromptStart, 0);
        feed_pty(&t, b"\r\n");
        let find = |t: &mut PtyTerminal| {
            let (lines, top) = t.grid_lines();
            top + lines
                .iter()
                .position(|l| l.starts_with("$ marker-prompt"))
                .unwrap() as i32
        };
        assert_eq!(
            t.marks_snapshot()[0].line,
            find(&mut t),
            "the mark starts on its prompt row"
        );
        let mut more = String::new();
        for i in 0..60 {
            more.push_str(&format!("post-{i}\r\n"));
        }
        feed_pty(&t, more.as_bytes());
        assert_eq!(
            t.marks_snapshot()[0].line,
            find(&mut t),
            "the mark must follow its prompt through ring rotation"
        );
    }

    /// The pane's Clear wipes screen AND scrollback: the content an inline
    /// image anchored to is gone, so the picture must go with it instead of
    /// floating over the fresh prompt at a now-meaningless row.
    #[test]
    fn clearing_the_terminal_drops_its_captured_images() {
        let (_tmp, mut t) = quiet_pty();
        feed_pty(&t, b"\r\nsome output");
        t.push_image_for_test(vec![1, 2, 3]);
        feed_pty(&t, b"\r\n");
        assert_eq!(t.pane_images().len(), 1);
        t.clear_screen_and_scrollback();
        assert!(
            t.pane_images().is_empty(),
            "a cleared pane keeps no image anchored to erased content"
        );
    }

    /// A captured line wider than the pane appears in the grid as a WRAPPED
    /// first row (a prefix of the needle): the jump must find it, must not
    /// be fooled by an unrelated short row sharing the prefix (only rows
    /// that really soft-wrap qualify for the reverse arm), and must skip
    /// continuation rows so the newest-first scan can't land mid-line.
    #[test]
    fn capture_jump_matches_wrapped_lines_without_false_prefixes() {
        let (_tmp, mut t) = quiet_pty();
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let needle: String = format!("error[E0308]: {}", "x".repeat(60))
            .chars()
            .take(60)
            .collect();
        // An unrelated NON-wrapped row that happens to share the prefix.
        feed_pty(&t, b"\r\nerror[E0308]: unrelated short\r\n");
        assert_eq!(
            t.find_captured_line(&needle),
            None,
            "a short non-wrapped lookalike must not match"
        );
        // The real captured line, wrapping across two grid rows.
        let full = format!("error[E0308]: {}", "x".repeat(60));
        feed_pty(&t, full.as_bytes());
        let start = {
            let term = t.term.lock();
            term.grid().cursor.point.line.0 - 1
        };
        assert_eq!(
            t.find_captured_line(&needle),
            Some(start),
            "the wrapped line's FIRST row is the jump target, not its continuation"
        );
    }

    /// A soft-wrapped logical line copies as ONE line: the WRAPLINE flag on
    /// the wrapping row's last cell tells a continuation from a real row
    /// break, so selection copy, the durable command history, and the
    /// sticky header all see the text the user actually typed. A '\n' at
    /// the wrap corrupted stored commands and re-ran only the first
    /// fragment on paste. Hard breaks keep their newline.
    #[test]
    fn selection_over_a_soft_wrapped_line_copies_one_logical_line() {
        let (_tmp, mut t) = quiet_pty();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let cols = t.term.lock().columns();
        // A fresh row below any spawn banner, then a line long enough to wrap.
        feed_pty(&t, b"\r\n");
        let start = t.term.lock().grid().cursor.point.line.0;
        let long: String = "abcdefghij".repeat(6); // 60 chars > pane width
        feed_pty(&t, long.as_bytes());
        t.set_selection(Some(Selection {
            anchor: (start, 0),
            head: (start + 1, cols.saturating_sub(1) as u16),
            block: false,
        }));
        assert_eq!(
            t.selection_text(),
            long,
            "the wrapped rows must rejoin without a newline"
        );
        // A hard row break keeps its newline.
        feed_pty(&t, b"\r\nsecond");
        t.set_selection(Some(Selection {
            anchor: (start + 1, 0),
            head: (start + 2, 6),
            block: false,
        }));
        assert!(
            t.selection_text().contains('\n'),
            "a real row break still copies as two lines: {:?}",
            t.selection_text()
        );
    }

    /// The annotate prompt's existing-note lookup compares the selection
    /// line captured at snapshot time against annotation lines; both must
    /// translate at the SAME clock reading, or output scrolling between the
    /// capture and the lookup shifts the annotations away from the fixed
    /// selection line and the same span duplicates instead of editing.
    #[test]
    fn annotation_lookup_translates_at_the_callers_clock() {
        let (_tmp, mut t) = quiet_pty();
        feed_pty(&t, b"existing span\r\n");
        let clock = t.scroll_clock();
        t.add_annotation(0, clock, 0, 13, String::from("existing note"));
        // The prompt captures selection + clock in one snapshot...
        let (_, snap_clock) = t.selection_and_clock();
        // ...then 30 rows land before the existing-note lookup runs.
        let mut fill = String::new();
        for i in 0..30 {
            fill.push_str(&format!("late-{i}\r\n"));
        }
        feed_pty(&t, fill.as_bytes());
        let cur = t.annotations_at_clock(snap_clock);
        assert_eq!(
            cur[0].0, 0,
            "translated at the captured clock, the note still names the captured line"
        );
    }

    /// Annotations name PRIMARY-screen rows; while vim/htop own the
    /// viewport the same line numbers describe unrelated alternate-grid
    /// content, so neither the amber paint nor click hit-testing may
    /// resolve there — and both come back when the app exits.
    #[test]
    fn annotations_hide_while_an_alt_screen_app_owns_the_viewport() {
        let (_tmp, mut t) = quiet_pty();
        t.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        feed_pty(&t, b"annotated content row\r\n");
        let clock = t.scroll_clock();
        t.add_annotation(0, clock, 0, 15, String::from("note"));
        assert!(
            t.annotation_at(3, 0).is_some(),
            "the note resolves on the primary screen"
        );
        feed_pty(&t, b"\x1b[?1049h\x1b[Hvim-owns-this-screen");
        assert!(
            t.annotation_at(3, 0).is_none(),
            "no hit while the alternate screen is up"
        );
        feed_pty(&t, b"\x1b[?1049l");
        assert!(
            t.annotation_at(3, 0).is_some(),
            "back on the primary screen the note resolves again"
        );
    }

    /// The Annotate prompt captures its span when it OPENS; rows that
    /// stream while the user types the note must not shift the pin. The
    /// commit anchors with the open-time (line, clock) pair — sampling a
    /// fresh baseline at commit pinned the note N rows below its text
    /// after N rows of output.
    #[test]
    fn an_annotation_committed_after_output_streamed_pins_where_it_was_captured() {
        let (_tmp, mut t) = quiet_pty();
        feed_pty(&t, b"target-line alpha\r\n");
        let (lines, top) = t.grid_lines();
        let line = top
            + lines
                .iter()
                .position(|l| l.starts_with("target-line"))
                .unwrap() as i32;
        let clock = t.scroll_clock();
        // 40 rows stream while the note is being typed.
        let mut more = String::new();
        for i in 0..40 {
            more.push_str(&format!("while-typing-{i}\r\n"));
        }
        feed_pty(&t, more.as_bytes());
        t.add_annotation(line, clock, 0, 11, String::from("note"));
        let cur = t.annotations_current();
        assert_eq!(cur.len(), 1, "the annotation survives");
        let term = t.term.lock();
        let text = extract_selection_text(&term, cur[0].0, 0, cur[0].0, 20);
        assert!(
            text.starts_with("target-line alpha"),
            "the note must pin to the text selected at open, got row: {text:?}"
        );
    }

    /// Past `SCROLLBACK_LINES` the ring rotates while `history_size` stops
    /// moving, so a history-growth anchor freezes to a screen position.
    /// Annotations ride the scroll clock (like selections) and must keep
    /// following their content in exactly the long-lived panes (Claude
    /// Code, `tail -f`) the feature targets.
    #[test]
    fn an_annotation_survives_streaming_past_a_saturated_scrollback() {
        let (_tmp, mut t) = quiet_pty();
        let mut chunk = String::new();
        for i in 0..5200 {
            chunk.push_str(&format!("pre-{i}\r\n"));
        }
        feed_pty(&t, chunk.as_bytes());
        feed_pty(&t, b"note-worthy-marker here\r\n");
        let (lines, top) = t.grid_lines();
        let line = top
            + lines
                .iter()
                .position(|l| l.starts_with("note-worthy-marker"))
                .unwrap() as i32;
        let clock = t.scroll_clock();
        t.add_annotation(line, clock, 0, 18, String::from("pinned"));
        let mut more = String::new();
        for i in 0..60 {
            more.push_str(&format!("post-{i}\r\n"));
        }
        feed_pty(&t, more.as_bytes());
        let cur = t.annotations_current();
        assert_eq!(cur.len(), 1, "the annotation survives the rotation");
        let term = t.term.lock();
        let text = extract_selection_text(&term, cur[0].0, 0, cur[0].0, 25);
        assert!(
            text.starts_with("note-worthy-marker"),
            "the note must follow its content through ring rotation, got row: {text:?}"
        );
    }

    /// A command spanning several grid rows (a quoted/heredoc newline, or a
    /// soft wrap shown in a later-widened pane) joins its rows with '\n' in
    /// `extract_selection_text`; the sticky header used to print that byte
    /// into a cell verbatim, corrupting the frame on the host terminal (the
    /// screen-corruption class the pdftoppm/DAP capture fixes exist to stop).
    #[test]
    fn sticky_header_never_paints_a_control_char_for_a_multi_row_command() {
        use crate::shell_integration::OscEvent;
        let (_tmp, mut t) = quiet_pty();
        // First render sizes the grid to the pane (78 cols), so the long
        // command genuinely wraps.
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        feed_pty(&t, b"$ ");
        t.push_mark_for_test(OscEvent::PromptEnd, 2);
        // A multi-line typed command (heredoc / quoted newline): the input
        // span covers two HARD rows, so the joined text carries a newline
        // well inside the label's painted width.
        feed_pty(&t, b"echo \"build-it-99\r\nsecond-line\"");
        feed_pty(&t, b"\r\n");
        t.push_mark_for_test(OscEvent::CommandStart, 0);
        for i in 0..40 {
            feed_pty(&t, format!("out-{i}\r\n").as_bytes());
        }
        t.push_mark_for_test(OscEvent::CommandEnd(Some(0)), 0);
        assert!(
            !t.command_decorations().is_empty(),
            "the marks must pair into a decoration"
        );
        t.scroll_to_top();
        for _ in 0..5 {
            t.scroll_down(1);
        }
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let inner = t.last_inner;
        let top: String = (inner.x..inner.x + inner.width)
            .map(|x| buf[(x, inner.y)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            top.contains("build-it-99"),
            "the wrapped command must still pin: {top:?}"
        );
        for y in 0..area.height {
            for x in 0..area.width {
                let sym = buf[(x, y)].symbol();
                assert!(
                    !sym.chars().any(|c| c.is_control()),
                    "cell ({x},{y}) holds a control char: {sym:?}"
                );
            }
        }
    }

    /// A prompt row rewritten SHORTER than the recorded 133;B column (a
    /// backgrounded `\r\x1b[K` progress writer, or a pane shrunk under the
    /// prompt) used to panic click-to-move: the clamp bounds inverted
    /// (`min > max`) and `u16::clamp` asserts. A click is never allowed to
    /// take the app down; the degraded gesture just clamps sanely.
    #[test]
    fn a_prompt_click_after_the_row_was_rewritten_shorter_does_not_panic() {
        let (_tmp, mut t) = quiet_pty();
        t.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        feed_pty(&t, b"user@host ~/Documents/croft % ");
        t.push_mark_for_test(crate::shell_integration::OscEvent::PromptEnd, 30);
        // The rewrite leaves 7 columns of text and the cursor at column 7,
        // both left of the recorded prompt-end column 30.
        feed_pty(&t, b"\r\x1b[Kworking");
        let row = t.term.lock().grid().cursor.point.line.0 as u16;
        // Live bounds: the text ends at column 7 and the cursor sits at
        // column 7, both left of the recorded 133;B column 30. The click at
        // column 5 clamps into [30.min(7), 7] = 7, which equals the cursor,
        // so the gesture degrades to no motion instead of panicking.
        assert_eq!(
            t.prompt_click_arrows(5, row),
            None,
            "a click on a shortened prompt row must degrade, not panic"
        );
    }

    #[test]
    fn alt_screen_selection_follows_content_across_repaints() {
        // Alt-screen apps (Claude Code) never scroll the grid — they
        // repaint it in place. The selection must re-find its text after
        // each repaint, hide while it is scrolled out of the app's view,
        // and come back when the content does.
        let (_tmp, mut t) = quiet_pty();
        feed_pty(
            &t,
            b"\x1b[?1049h\x1b[H\x1b[2Jalpha one\r\nbravo two\r\ncharlie three\r\ndelta four",
        );
        t.set_selection(Some(Selection {
            anchor: (1, 0),
            head: (1, 8),
            block: false,
        }));
        assert_eq!(t.selection_text(), "bravo two");

        // The app scrolls its transcript up one line and repaints.
        feed_pty(
            &t,
            b"\x1b[H\x1b[2Jbravo two\r\ncharlie three\r\ndelta four\r\necho five",
        );
        t.rebase_selection();
        assert_eq!(t.selection_text(), "bravo two");
        assert_eq!(
            t.selection().unwrap().anchor.0,
            0,
            "the highlight must move to where the app repainted its text"
        );

        // The selected content scrolls out of the app's view entirely:
        // highlight goes dormant but copy still yields the remembered text.
        feed_pty(
            &t,
            b"\x1b[H\x1b[2Jfoxtrot six\r\ngolf seven\r\nhotel eight\r\nindia nine",
        );
        t.rebase_selection();
        assert!(
            t.alt_sel.as_ref().is_some_and(|a| a.dormant),
            "no matching content anywhere in the grid: dormant"
        );
        assert_eq!(t.selection_text(), "bravo two");

        // The app scrolls the content back into view at a new position.
        feed_pty(
            &t,
            b"\x1b[H\x1b[2Jalpha one\r\nbravo two\r\ncharlie three\r\ndelta four",
        );
        t.rebase_selection();
        assert!(t.alt_sel.as_ref().is_some_and(|a| !a.dormant));
        assert_eq!(t.selection().unwrap().anchor.0, 1);
        assert_eq!(t.selection_text(), "bravo two");
    }

    #[test]
    fn alt_screen_row_half_covered_by_a_floating_pill_keeps_its_surviving_prefix() {
        // Claude Code floats a "Jump to bottom" pill ON TOP of a content
        // row. That row's text no longer equals the remembered row, but
        // its left part is untouched — it must keep a highlight clipped to
        // the surviving columns instead of dropping out entirely.
        let (_tmp, mut t) = quiet_pty();
        feed_pty(
            &t,
            b"\x1b[?1049h\x1b[H\x1b[2Jalpha one\r\nbravo two words here\r\ncharlie three words here\r\ndelta four words here\r\necho five",
        );
        t.set_selection(Some(Selection {
            anchor: (1, 0),
            head: (3, 20),
            block: false,
        }));
        // The app repaints the same content but overlays a pill over the
        // right half of the block's middle row.
        feed_pty(
            &t,
            b"\x1b[H\x1b[2Jalpha one\r\nbravo two words here\r\ncharlie three[ PILL ]here\r\ndelta four words here\r\necho five",
        );
        t.rebase_selection();
        let anchor = t.alt_sel.as_ref().expect("anchor survives");
        assert!(!anchor.dormant);
        let clips = anchor.visible.as_ref().expect("partially covered");
        assert!(
            clips.contains(&(1, 0, u16::MAX)) && clips.contains(&(3, 0, u16::MAX)),
            "intact rows keep their full highlight: {clips:?}"
        );
        assert!(
            clips
                .iter()
                .any(|&(l, lo, hi)| l == 2 && lo == 0 && (12..14).contains(&hi)),
            "the covered row keeps its surviving prefix ('charlie three'): {clips:?}"
        );
        assert!(
            clips
                .iter()
                .any(|&(l, lo, hi)| l == 2 && lo == 21 && hi == 24),
            "the surviving suffix ('here') stays lit too, even under half the row: {clips:?}"
        );
    }

    #[test]
    fn alt_screen_bottom_up_drag_from_a_blank_row_selects_and_survives_release() {
        // Bottom-up selections usually start on a blank row or an animated
        // status row: the anchor captured at mouse-down mismatches on the
        // next frame, and before drag-awareness that turned the whole drag
        // dormant (invisible). While the button is held the selection must
        // follow the pointer over whatever is on screen; release captures
        // the definitive anchor.
        let (_tmp, mut t) = quiet_pty();
        feed_pty(
            &t,
            b"\x1b[?1049h\x1b[H\x1b[2Jalpha one\r\nbravo two\r\ncharlie three\r\n\r\ntokens 42",
        );
        t.last_inner = Rect::new(0, 0, 40, 10);
        // Mouse-down on the blank row (row 3), then an app repaint changes
        // the animated status row so the blank-row anchor can't match as a
        // block, then drag upward.
        t.start_selection_at(5, 3);
        feed_pty(&t, b"\x1b[5;1Htokens 43\x1b[K");
        t.extend_selection_to(0, 1);
        let anchor = t.alt_sel.as_ref().expect("anchor present");
        assert!(
            !anchor.dormant,
            "a held drag never hides itself, whatever the anchor matched"
        );
        assert_eq!(t.selection_text(), "bravo two\ncharlie three\n");
        t.end_drag();
        // After release the anchor is definitive: the app scrolls its
        // content down one row and the highlight follows.
        feed_pty(
            &t,
            b"\x1b[H\x1b[2Jnew line\r\nalpha one\r\nbravo two\r\ncharlie three\r\n\r\ntokens 44",
        );
        t.rebase_selection();
        assert_eq!(t.selection().expect("survives").normalised().0, 2);
        assert_eq!(t.selection_text(), "bravo two\ncharlie three\n");
    }

    #[test]
    fn alt_screen_edge_autoscroll_forwards_wheel_to_the_tracking_app() {
        // On the alternate screen a drag held past the pane edge cannot
        // scroll croft scrollback (there is none); a mouse-tracking app
        // gets a wheel report at the edge cell so IT scrolls, and the
        // selection head re-pins to the edge row.
        let tmp = tempfile::tempdir().unwrap();
        // `cat -v` echoes whatever the pty receives back as printable
        // text, so the wheel report's arrival is observable in the grid.
        let mut t =
            PtyTerminal::new_running("/bin/cat", &[String::from("-v")], tmp.path()).unwrap();
        feed_pty(&t, b"\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[H\x1b[2J");
        t.last_inner = Rect::new(0, 0, 40, 10);
        t.set_selection(Some(Selection {
            anchor: (2, 0),
            head: (2, 5),
            block: false,
        }));
        t.autoscroll_select(1, 5);
        assert_eq!(
            t.selection().unwrap().head.0,
            9,
            "the head re-pins to the bottom edge row"
        );
        let mut waited = 0u32;
        // SGR wheel-down at 1-based cell (6, 10): `ESC[<65;6;10M`.
        while !t.grid_lines().0.iter().any(|l| l.contains("[<65;6;10M")) {
            assert!(waited < 8000, "the app never received the wheel report");
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited += 20;
        }
    }

    #[test]
    fn primary_selection_survives_alt_round_trip_but_alt_selection_dies_on_exit() {
        let (_tmp, mut t) = quiet_pty();
        feed_pty(&t, b"hello world\r\n");
        // The `▶ /bin/sleep 30` run-label header owns the top row(s), so
        // find where the fed line actually landed.
        let (lines, top) = t.grid_lines();
        let line = top
            + lines
                .iter()
                .position(|l| l.starts_with("hello"))
                .expect("fed line present") as i32;
        t.set_selection(Some(Selection {
            anchor: (line, 0),
            head: (line, 4),
            block: false,
        }));
        assert_eq!(t.selection_text(), "hello");
        // vim-style round trip: enter and leave the alternate screen.
        feed_pty(&t, b"\x1b[?1049h\x1b[H\x1b[2Jvim vim vim");
        t.rebase_selection();
        feed_pty(&t, b"\x1b[?1049l");
        t.rebase_selection();
        assert_eq!(
            t.selection_text(),
            "hello",
            "a primary-screen selection is frozen across an alt-screen trip"
        );
        // A selection made ON the alt screen names alternate-grid content;
        // leaving the alt screen destroys that content and the selection.
        feed_pty(&t, b"\x1b[?1049h\x1b[H\x1b[2Jquick brown fox");
        t.set_selection(Some(Selection {
            anchor: (0, 0),
            head: (0, 4),
            block: false,
        }));
        assert_eq!(t.selection_text(), "quick");
        feed_pty(&t, b"\x1b[?1049l");
        t.rebase_selection();
        assert!(
            t.selection().is_none(),
            "an alt-screen selection dies with the alternate grid"
        );
    }

    #[test]
    fn selection_spanning_scrollback_into_live_extracts_every_line() {
        let mut t = fresh_term(20, 3);
        feed(&mut t, b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD\r\nEEEE\r\nFFFF");
        // Select from the oldest scrollback line (AAAA = Line(-3)) all
        // the way to the last live line (FFFF = Line(2)).
        let all = extract_selection_text(&t, -3, 0, 2, 19);
        assert_eq!(all, "AAAA\nBBBB\nCCCC\nDDDD\nEEEE\nFFFF");
    }

    #[test]
    fn process_name_resolves_a_live_pid() {
        let me = std::process::id() as i32;
        let name = process_name(me);
        assert!(
            name.as_deref().is_some_and(|n| !n.is_empty()),
            "the test process resolves to a non-empty name: {name:?}"
        );
    }

    #[test]
    fn pick_pane_label_prefers_a_manual_name() {
        assert_eq!(pick_pane_label(Some("server"), "zsh"), "server");
        assert_eq!(pick_pane_label(None, "vim"), "vim");
        assert_eq!(pick_pane_label(None, ""), "");
    }

    #[test]
    fn parse_shells_skips_comments_and_dedupes_by_basename() {
        let s = "# /etc/shells\n/bin/zsh\n/bin/bash\n/usr/local/bin/bash\n\nnot-a-path\n/usr/bin/fish\n";
        let p = parse_shells(s);
        let bases: Vec<&str> = p.iter().map(|(_, b)| b.as_str()).collect();
        assert_eq!(
            bases,
            vec!["zsh", "bash", "fish"],
            "comments/blanks/non-paths skipped, basenames deduped"
        );
        assert_eq!(p[0].0, "/bin/zsh", "full path retained");
    }
    #[test]
    fn finished_command_carries_its_output_block() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "printf '\\033]133;A\\007$ \\033]133;B\\007make\\n\\033]133;C\\007'; printf 'main.c:7:3: error: boom\\nsecond line\\n'; printf '\\033]133;D;1\\007'";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut finished = Vec::new();
        while std::time::Instant::now() < deadline {
            finished.extend(term.drain_finished_commands());
            if !finished.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        let f = finished
            .first()
            .expect("the 133;D must yield a FinishedCommand");
        assert!(
            f.output.contains("main.c:7:3: error: boom") && f.output.contains("second line"),
            "the output block must ride the completion; got {:?}",
            f.output
        );
    }

    #[test]
    fn finished_output_keeps_a_final_line_without_trailing_newline() {
        // `133;D` moves no cursor: a diagnostic printed WITHOUT a trailing
        // newline leaves the mark on that very row, and excluding the
        // cursor row dropped exactly the line the matchers needed (#120
        // review).
        let tmp = tempfile::tempdir().unwrap();
        let script = "printf '\\033]133;A\\007$ \\033]133;B\\007cc\\n\\033]133;C\\007'; printf 'lib.c:2:1: error: no newline here'; printf '\\033]133;D;1\\007'";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut finished = Vec::new();
        while std::time::Instant::now() < deadline {
            finished.extend(term.drain_finished_commands());
            if !finished.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        let f = finished.first().expect("completion expected");
        assert!(
            f.output.contains("lib.c:2:1: error: no newline here"),
            "the unterminated final row must be captured; got {:?}",
            f.output
        );
    }
}
