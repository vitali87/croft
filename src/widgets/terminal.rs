use alacritty_terminal::Term;
use alacritty_terminal::event::{Event as AlacEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, GridCell, Row, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection as TracerSelection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::ClipboardType;
use alacritty_terminal::term::cell::{Cell, Flags};
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

mod clock;
mod mouse;
mod selection;
use clock::*;
pub use mouse::MouseAction;
pub use mouse::MouseButtonKind;
pub use mouse::MouseMods;
#[cfg(test)]
use mouse::*;
pub use selection::Selection;
pub use selection::is_terminal_word_char;
use selection::*;

const SCROLLBACK_LINES: usize = 5000;

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

/// Listener for events the embedded `Term` emits. Most variants (title
/// changes, cursor-blink toggles, clipboard load, child exit, etc.)
/// are owned by the outer croft TUI and ignored here. A clipboard STORE
/// (OSC 52 copy) is latched for the app to deliver (#678). Two more
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
    /// The newest OSC 52 clipboard store from the child (#678); drained by
    /// [`PtyTerminal::take_clipboard_store`].
    clipboard: Option<Arc<std::sync::Mutex<Option<String>>>>,
}

impl EventListener for VoidListener {
    fn send_event(&self, event: AlacEvent) {
        match event {
            AlacEvent::Bell => {
                if let Some(bell) = self.bell.as_ref() {
                    bell.store(true, Ordering::Release);
                }
            }
            // A program copying through OSC 52: Claude Code, tmux, nvim.
            // Over SSH it is their ONLY way to copy (a remote box has no
            // clipboard tool), so dropping it broke copy on a remote while
            // the same program copied fine locally through pbcopy (#678).
            // Only the clipboard, never the primary selection: X's
            // select-to-copy buffer has no counterpart on the user's side,
            // and writing it into the clipboard would clobber real copies.
            AlacEvent::ClipboardStore(ClipboardType::Clipboard, text) => {
                if let Some(clip) = self.clipboard.as_ref() {
                    *clip.lock().unwrap() = Some(text);
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

/// One destructive clear [`WipeSniffer::scan`] found: the chunk offset just
/// past its last byte, and whether it erased the primary screen (ED 2),
/// the scrollback (ED 3) or both (RIS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Wipe {
    end: usize,
    screen: bool,
    hist: bool,
}

impl WipeSniffer {
    /// Scan one chunk. `alt` is the terminal's alt-screen state where the
    /// chunk STARTS. Returns the primary-screen wipes in it, in order.
    fn scan(&mut self, chunk: &[u8], mut alt: bool) -> Vec<Wipe> {
        let mut data = std::mem::take(&mut self.carry);
        // Offsets into `data` are reported against `chunk`: a sequence
        // carried from the last read ends in this one, never before it.
        let carried = data.len();
        data.extend_from_slice(chunk);
        let mut wipes = Vec::new();
        let mut wipe = |end: usize, screen: bool, hist: bool| {
            let end = end.saturating_sub(carried);
            wipes.push(Wipe { end, screen, hist });
        };
        let mut i = 0;
        while i < data.len() {
            if data[i] != 0x1b {
                i += 1;
                continue;
            }
            let Some(&next) = data.get(i + 1) else {
                self.carry = data.split_off(i);
                return wipes;
            };
            match next {
                // RIS: full reset — everything is gone, back on primary.
                b'c' => {
                    wipe(i + 2, true, true);
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
                        return wipes;
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
                        b'J' if params == b"2" && !alt => wipe(j + 1, true, false),
                        b'J' if params == b"3" && !alt => wipe(j + 1, false, true),
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
        wipes
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

/// The visible screen as `(plain, coloured, wraps)` rows plus the cursor's
/// `(row, col)` on it, `None` when hidden: [`PtyTerminal::screen_ansi_wrapped`].
pub type ScreenAnsi = (Vec<(String, String, bool)>, Option<(u16, u16)>);

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
    /// What programs in the pane said about themselves over OSC 7501,
    /// updated by the reader thread in stream order.
    program_status: Arc<std::sync::Mutex<crate::program_status::Records>>,
    /// The event count reports are read from: what came before it was not
    /// the seated agent's ([`Self::sample_reporter`]).
    status_mark: u64,
    /// The event count when the seated agent was found.
    status_seated: u64,
    /// Listening loopback ports the reader thread scraped out of the output
    /// stream (`http://localhost:PORT` banners, `listening on :PORT` lines).
    /// The app drains this each tick to feed the PORTS panel and the toast.
    port_rx: std::sync::mpsc::Receiver<crate::port_detect::PortHit>,
    /// Who holds the pane's PTY: croft itself, or a pane host (#694).
    backend: PaneBackend,
    /// Bytes for the child's stdin, written by this pane's writer thread.
    /// The user-input path (`write_input`, `paste_*`, `cd_into`, mouse
    /// reports) and the responder that ships alacritty's replies both send
    /// here: portable-pty hands out one writer, and a blocking `write_all`
    /// on the UI thread froze all of croft when a large paste met a program
    /// that was not reading its stdin.
    input_tx: std::sync::mpsc::Sender<Vec<u8>>,
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
    /// The shell's exit once [`Self::poll_exit`] has reported it (#912):
    /// its exit code, or None when a pane host went away without one.
    exit: Option<Option<i32>>,
    /// When the shell was first found reaped with its output still open. A
    /// background job it left running can hold the pty open for good, so
    /// the exit is reported after [`EXIT_OUTPUT_GRACE`] all the same.
    reaped_at: Option<std::time::Instant>,
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
    /// When input was last written, in the same clock as `last_output_ms`:
    /// until output newer than this arrives, the screen does not yet show
    /// what was typed (#614).
    last_input_ms: u64,
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
    /// Collapsed to a one-column strip (#313): the pane keeps running and
    /// keeps its place in the row, but yields its width to the panes still
    /// expanded.
    ///
    /// A flag on the PANE rather than a set of indices held by the app.
    /// Indices shift when a pane is closed or reordered, so a stored one
    /// comes to name a different terminal than it was set against - the
    /// same class of bug as #440, where a positional index named the wrong
    /// command after an eviction. A flag that travels with the pane cannot
    /// be pointed at the wrong one.
    ///
    /// Session-scoped, and deliberately not written to the terminal session
    /// store: collapse says "I am not looking at this right now", which is
    /// not a fact worth restoring a workspace into.
    pub collapsed: bool,
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
    /// Latched by the event listener when the child copies through OSC 52
    /// (#678); drained by [`Self::take_clipboard_store`].
    clipboard_store: Arc<std::sync::Mutex<Option<String>>>,
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
    /// Where the lines `capture` triggers caught start (#1450), recorded by
    /// the reader thread. Lock order: term → clock → capture_rows.
    capture_rows: Arc<std::sync::Mutex<CaptureRows>>,
    /// The last few minutes of this pane's output, for Session: Rewind
    /// (#357). Written by the reader thread as chunks arrive; bounded by
    /// bytes, so a `yes`-style flood evicts rather than grows.
    rewind: Arc<std::sync::Mutex<crate::rewind::RewindBuffer>>,
    /// Test-only capture of every byte written toward the child's stdin.
    #[cfg(test)]
    written_for_test: Arc<std::sync::Mutex<Vec<u8>>>,
    /// Test-only sender into `finished_rx`, as the reader thread holds.
    #[cfg(test)]
    finished_tx_for_test: std::sync::mpsc::Sender<FinishedCommand>,
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

/// How many rows either side of its recorded row a captured line is still
/// looked for (#1450). The row is read with the parser stopped right after
/// the line, so it is exact unless a program moved the cursor around the
/// line; looking any further could land on another run's copy.
const CAPTURE_SLACK: i32 = 2;

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
/// command text (read off the B→C mark span at the C mark, before the
/// command can clear it) and the pane's OSC 7 cwd at finish time —
/// everything the durable command history records.
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

/// The command a pane is running, from its `133;C` mark to its `133;D`, as
/// the reader thread follows it. What its [`FinishedCommand`] reports is
/// pinned here when it starts rather than read back off the grid when it
/// ends: a command that clears the screen (`clear; make`) has erased the
/// rows its typed line and its first output sat on by then (#876).
struct RunningCommand {
    started: std::time::Instant,
    /// The typed line, read off the `133;B`→`133;C` span at the C mark.
    typed: String,
    /// The first output row, as the grid line and scroll clock reading it
    /// was recorded at, re-based through the clock when the command ends.
    /// The C mark's own row to begin with (`133;C` comes right after the
    /// command's newline, before the program prints anything), and the top
    /// of the screen once a wipe erases that row.
    out_line: i32,
    out_clock: i64,
}

impl RunningCommand {
    /// Follow a wipe that just landed, `now` being the clock read right
    /// after it. Once the first output row is erased, the output that
    /// survives starts at the top of the screen the wipe left: an ED 2
    /// erases that row only when it sat blank below the screen's last text
    /// (alacritty pushes the text into the scrollback), and an ED 3 only
    /// when it sat in the scrollback it empties.
    fn follow_wipe(&mut self, wipe: Wipe, now: i64) {
        let line = self.out_line - (now - self.out_clock) as i32;
        if (line >= 0 && wipe.screen) || (line < 0 && wipe.hist) {
            self.out_line = 0;
            self.out_clock = now;
        }
    }
}

/// Where the reader thread stops the parser inside one read:
/// * at a shell mark, to sample the cursor where it landed;
/// * at a wipe, to move the running command's output start past it;
/// * right after a line a `capture` trigger caught (the index of its hit),
///   to record the row the line starts on (#1450).
enum ReadStop {
    Mark(crate::shell_integration::OscEvent),
    Wipe(Wipe),
    Capture(usize),
}

/// Where each line a `capture` trigger caught starts in its pane (#1450),
/// by the id its hit carries to the CAPTURES entry. A row is stored as a
/// scroll-clock content id (grid line plus clock reading), which output
/// scrolling past leaves unchanged. A wipe that erases a row drops it, and
/// a resize, which re-wraps lines by different numbers of rows that one
/// clock shift cannot follow, re-finds every row by its place among the
/// lines above the cursor ([`Self::reflow`]). A row that a resize under a
/// full-screen program re-wrapped out of sight loses its place but keeps
/// its text and the serial of the printed line it is on, which still tell
/// it from its repeats ([`RowAt::Unknown`]). Ids come from the process-wide
/// [`next_mark_id`], so one never names another pane's row, and a later
/// capture always has a larger id.
#[derive(Default)]
struct CaptureRows {
    rows: std::collections::BTreeMap<u64, CapturedRow>,
    /// Serial of the printed line the newest capture is on.
    line: u64,
}

/// One captured line: its row, None once a resize under a full-screen
/// program re-wrapped it out of sight; its text; and the serial of the
/// printed line, which two triggers that catch the same line share.
struct CapturedRow {
    row: Option<i64>,
    text: String,
    line: u64,
}

/// Where a capture's row is now ([`CaptureRows::line`]).
enum RowAt {
    /// On this grid line.
    Line(i64),
    /// Still captured, but where is unknown. Output keeps its order, so it
    /// is the match of its text this many matches up from the bottom: one
    /// for each later printed line still held that the same text matches.
    Unknown { later: usize },
}

impl CaptureRows {
    /// Record the line the parser just completed, whose text is `text`,
    /// and return its id. The parser stands just past the line's newline,
    /// so the line ended on the row above the cursor and starts where that
    /// row's soft wraps begin. Past the CAPTURES panel's cap the oldest are
    /// dropped.
    fn record(
        &mut self,
        term: &mut Term<VoidListener>,
        clock: &mut ScrollClock,
        text: &str,
    ) -> u64 {
        let top = term.grid().topmost_line().0;
        let mut line = term.grid().cursor.point.line.0 - 1;
        while line > top && row_wraps(term, line - 1) {
            line -= 1;
        }
        let id = next_mark_id();
        let row = Some(clock.tick(term) + i64::from(line));
        // Two triggers that catch one line stop the parser at the same
        // place, so they record the same row: one printed line, one serial.
        if self
            .rows
            .last_key_value()
            .is_none_or(|(_, last)| last.row != row)
        {
            self.line += 1;
        }
        let text = text.trim_end().to_owned();
        let line = self.line;
        self.rows.insert(id, CapturedRow { row, text, line });
        while self.rows.len() > crate::widgets::captures::CAPTURES_MAX {
            self.rows.pop_first();
        }
        id
    }

    /// Where `id`'s row is at clock reading `now`, or None once it was
    /// erased. `needle` is the text the jump looks for.
    fn line(&self, id: u64, now: i64, needle: &str) -> Option<RowAt> {
        let captured = self.rows.get(&id)?;
        Some(match captured.row {
            Some(row) => RowAt::Line(row - now),
            None => {
                // Serials rise with ids, so the later lines' come in order.
                let mut later: Vec<u64> = self
                    .rows
                    .range((std::ops::Bound::Excluded(id), std::ops::Bound::Unbounded))
                    .filter(|(_, c)| c.line != captured.line && c.text.starts_with(needle))
                    .map(|(_, c)| c.line)
                    .collect();
                later.dedup();
                RowAt::Unknown { later: later.len() }
            }
        })
    }

    /// Drop the rows a wipe that just landed erased, `now` being the clock
    /// read right after it (the same test as [`RunningCommand::follow_wipe`]).
    /// A row whose place is unknown cannot be told on screen from in the
    /// scrollback, so it goes with any wipe of the scrollback (a reset, or
    /// the ED 3 of a `clear`, whose ED 2 has already pushed the screen
    /// there): an entry then says its line is gone rather than taking text
    /// printed after the wipe.
    fn follow_wipe(&mut self, wipe: Wipe, now: i64) {
        self.rows.retain(|_, captured| match captured.row {
            Some(row) => {
                let line = row - now;
                !((line >= 0 && wipe.screen) || (line < 0 && wipe.hist))
            }
            None => !wipe.hist,
        });
    }

    /// Carry every row across `resize`. A resize re-wraps a soft-wrapped
    /// line onto more or fewer rows, but keeps every line and their order
    /// (short of the scrollback overflowing, which drops the oldest), and
    /// the cursor stays on its line. So a row's place in the list of line
    /// starts counted up from the cursor survives it, and so does a row
    /// below the cursor. Under a full-screen program the primary grid is
    /// re-wrapped too, out of reach behind the alternate screen, so every
    /// row's place becomes unknown and its entry finds it by its text and
    /// its order among the captures of that text.
    fn reflow(
        &mut self,
        term: &mut Term<VoidListener>,
        clock: &mut ScrollClock,
        resize: impl FnOnce(&mut Term<VoidListener>),
    ) {
        if self.rows.is_empty() {
            resize(term);
            return;
        }
        if term.mode().contains(TermMode::ALT_SCREEN) {
            resize(term);
            self.rows
                .values_mut()
                .for_each(|captured| captured.row = None);
            return;
        }
        let now = clock.tick(term);
        let before = line_starts_upward(term);
        let top = i64::from(term.grid().topmost_line().0);
        let cursor = i64::from(term.grid().cursor.point.line.0);
        // (id, the row's text and serial, Ok(place among the line starts) |
        // Err(line below the cursor)), None for a row whose place was
        // already unknown.
        type Held = (u64, String, u64, Option<Result<usize, i64>>);
        let held: Vec<Held> = std::mem::take(&mut self.rows)
            .into_iter()
            .filter_map(
                |(
                    id,
                    CapturedRow {
                        row,
                        text,
                        line: serial,
                    },
                )| {
                    let Some(row) = row else {
                        return Some((id, text, serial, None));
                    };
                    let line = row - now;
                    let at = if line > cursor {
                        Err(line)
                    } else {
                        Ok(before.partition_point(|&s| i64::from(s) > line))
                    };
                    (line >= top).then_some((id, text, serial, Some(at)))
                },
            )
            .collect();
        resize(term);
        let now = clock.tick(term);
        let after = line_starts_upward(term);
        for (id, text, serial, at) in held {
            let row = match at {
                None => None,
                Some(Ok(k)) => match after.get(k) {
                    Some(&l) => Some(now + i64::from(l)),
                    None => continue,
                },
                Some(Err(line)) => Some(now + line),
            };
            self.rows.insert(
                id,
                CapturedRow {
                    row,
                    text,
                    line: serial,
                },
            );
        }
    }
}

/// The rows that start a logical line, from the cursor's line up to the top
/// of the scrollback.
fn line_starts_upward(term: &Term<VoidListener>) -> Vec<i32> {
    let top = term.grid().topmost_line().0;
    let cursor = term.grid().cursor.point.line.0;
    (top..=cursor)
        .rev()
        .filter(|&l| l == top || !row_wraps(term, l - 1))
        .collect()
}

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
/// return the shell's argv: `args` (its login flag) with whatever the
/// integration adds. Long options go first, because bash only recognises
/// them (`--posix`) ahead of single-character ones (`-l`).
/// zsh: point `ZDOTDIR` at croft's shim. bash: the kitty/Ghostty trick —
/// `--posix` plus `ENV` pointing at croft's shim (posix-mode bash reads
/// only `$ENV`; the shim backs out and replays the real startup files).
/// Below 4.4 bash ignores that `$ENV`, so it reads the shim through
/// `--rcfile`, non-login, and the shim replays the login files itself.
/// fish: prepend a `vendor_conf.d` data dir to `XDG_DATA_DIRS`; the script
/// defers to fish 4's native marks. Every shim sources the user's real
/// dotfiles unchanged and emits OSC 133 prompt marks + the OSC 7 cwd
/// report. Opt out with `CROFT_SHELL_INTEGRATION=0`. Failures are
/// non-fatal — the shell still spawns, just without marks.
fn apply_shell_integration_env(
    cmd: &mut CommandBuilder,
    shell_path: &str,
    config_dir: &std::path::Path,
    args: Vec<String>,
) -> Vec<String> {
    if std::env::var("CROFT_SHELL_INTEGRATION").as_deref() == Ok("0") {
        return args;
    }
    let base = std::path::Path::new(shell_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    match base {
        "zsh" => {
            let Ok(shim) = crate::shell_integration::ensure_zsh_shim(config_dir) else {
                return args;
            };
            // An inherited ZDOTDIR pointing at croft's own shim (croft
            // launched from a croft pane) is poisoned — the user's dotfiles
            // live in HOME.
            let inherited = std::env::var_os("ZDOTDIR").map(std::path::PathBuf::from);
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            if home.as_os_str().is_empty() {
                return args;
            }
            let user_zdotdir = crate::shell_integration::resolve_user_zdotdir(
                inherited.as_deref(),
                &config_dir.join("shell-integration"),
                &home,
            );
            cmd.env("CROFT_USER_ZDOTDIR", user_zdotdir);
            cmd.env("ZDOTDIR", &shim);
            args
        }
        "bash" => {
            let Ok(shim) = crate::shell_integration::ensure_bash_shim(config_dir) else {
                return args;
            };
            // Preserve a user $ENV (rare for bash) for the shim to restore.
            if let Ok(env) = std::env::var("ENV") {
                cmd.env("CROFT_BASH_ENV", env);
            }
            cmd.env("CROFT_BASH_INJECT", "1");
            if !bash_env_injection_supported(shell_path) {
                // No `$ENV` in posix mode (macOS's 3.2), so the shim goes in
                // as the rcfile, as VS Code does it. A login bash ignores
                // `--rcfile`, so the shell starts non-login and the shim
                // replays the login files in its place.
                if args.iter().any(|a| a == "-l" || a == "--login") {
                    cmd.env("CROFT_BASH_LOGIN", "1");
                }
                return vec!["--rcfile".to_string(), shim.display().to_string()];
            }
            cmd.env("ENV", &shim);
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
            std::iter::once("--posix".to_string()).chain(args).collect()
        }
        "fish" => {
            let Ok(dir) = crate::shell_integration::ensure_fish_integration(config_dir) else {
                return args;
            };
            let inherited = std::env::var("XDG_DATA_DIRS").ok();
            cmd.env(
                "XDG_DATA_DIRS",
                crate::shell_integration::fish_xdg_data_dirs(&dir, inherited.as_deref()),
            );
            cmd.env("CROFT_FISH_XDG_DATA_DIR", &dir);
            args
        }
        _ => args,
    }
}

/// The command for an interactive pane shell: `shell_path` as a login
/// shell, with croft's shell integration.
fn interactive_shell_command(shell_path: &str, config_dir: &std::path::Path) -> CommandBuilder {
    let (program, args) = interactive_shell_invocation(shell_path);
    let mut cmd = CommandBuilder::new(&program);
    for a in apply_shell_integration_env(&mut cmd, shell_path, config_dir, args) {
        cmd.arg(a);
    }
    cmd
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
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    if pid <= 0 {
        return None;
    }
    let p = Pid::from(pid as usize);
    let mut sys = System::new();
    // The argv must be asked for by name (#1846): sysinfo's plain
    // `refresh_processes` reads memory, CPU, disk use and the executable,
    // never the command line, so `cmd()` was always empty. Every pane that
    // ran `ssh box` then read as "not an SSH session": the open-workspace
    // offer never appeared, and the user stayed in the remote shell.
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[p]),
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
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
    // The pane is croft's emulator, not the host's (#1453). A program that
    // read Ghostty's `TERM_PROGRAM` here wrote Kitty graphics the pane drops,
    // so `croft plot` printed blank rows. VS Code does the same with
    // `TERM_PROGRAM=vscode`.
    cmd.env("TERM_PROGRAM", crate::iterm2_inline::CROFT_TERM_PROGRAM);
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    for host_only in HOST_TERMINAL_VARS {
        cmd.env_remove(host_only);
    }
    cmd.env(
        crate::iterm2_inline::CROFT_INLINE_IMAGES_ENV,
        if pane_images_shown() { "1" } else { "0" },
    );
    // A pane's shell must count characters the way the terminal draws them.
    // macOS forwards `LC_CTYPE=UTF-8` over ssh, which Linux does not know, so
    // a remote shell fell back to ASCII and counted a 3-byte prompt glyph
    // (`❯`, a Nerd Font icon) as 3 columns: every redraw landed 2 columns
    // off and left 2-character coloured tails behind (#537).
    if cfg!(target_os = "linux") {
        let var = |k: &str| std::env::var(k).ok();
        if let Some((key, value)) = utf8_ctype_fix(var("LC_ALL"), var("LC_CTYPE"), var("LANG")) {
            cmd.env(key, value);
        }
    }
    // `git rebase -i` opens its plan in this croft (#620), unless the user
    // chose their own sequence editor, which always wins: an exported
    // GIT_SEQUENCE_EDITOR here, or `sequence.editor` in git config, which
    // `croft edit --sequence-editor` looks up in the rebasing repo itself.
    if view_sock.is_some()
        && std::env::var_os("GIT_SEQUENCE_EDITOR").is_none()
        && let Some(exe) = std::env::current_exe().ok().and_then(live_exe)
    {
        cmd.env("GIT_SEQUENCE_EDITOR", sequence_editor_command(&exe));
    }
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

/// Variables that name the HOST terminal (#1453). In a pane they would send
/// a program down a graphics or feature path croft's emulator does not relay.
const HOST_TERMINAL_VARS: &[&str] = &[
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "ITERM_SESSION_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "WEZTERM_PANE",
    "WEZTERM_EXECUTABLE",
    "VTE_VERSION",
];

/// Set once the DA1 probe finds a sixel host, which no variable advertises.
pub static SIXEL_HOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this croft draws a pane's captured images (#1453): the outer
/// terminal speaks some image protocol. A pane opened before the sixel probe
/// answers says no, which costs a picture, never a blank chart.
fn pane_images_shown() -> bool {
    crate::iterm2_inline::detect_inline_image_protocol()
        != crate::iterm2_inline::InlineImageProtocol::None
        || SIXEL_HOST.load(std::sync::atomic::Ordering::Relaxed)
}

/// The locale variable to set so a Linux shell reads its text as UTF-8, or
/// None when the inherited locale already does (or the user pinned another
/// with `LC_ALL`, which wins over anything set here). The bare name `UTF-8`
/// is what macOS sends and no Linux locale is called that.
fn utf8_ctype_fix(
    lc_all: Option<String>,
    lc_ctype: Option<String>,
    lang: Option<String>,
) -> Option<(&'static str, &'static str)> {
    let is_utf8 = |v: &str| {
        let codeset = v
            .split('@')
            .next()
            .unwrap_or(v)
            .rsplit('.')
            .next()
            .unwrap_or("");
        v.contains('.') && matches!(codeset.to_ascii_lowercase().as_str(), "utf-8" | "utf8")
    };
    let bare = |v: &str| v.eq_ignore_ascii_case("utf-8") || v.eq_ignore_ascii_case("utf8");
    match lc_all.filter(|v| !v.is_empty()) {
        Some(v) if bare(&v) => return Some(("LC_ALL", "C.UTF-8")),
        Some(_) => return None,
        None => {}
    }
    let effective = lc_ctype
        .filter(|v| !v.is_empty())
        .or(lang.filter(|v| !v.is_empty()));
    match effective {
        Some(v) if is_utf8(&v) => None,
        _ => Some(("LC_CTYPE", "C.UTF-8")),
    }
}

/// Milliseconds since the epoch, the clock `last_output_ms` keeps.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `exe` as a path that still runs. Once an update renames a new binary
/// over the running one, Linux reports it as `.../croft (deleted)`, which
/// no shell can start; the new binary at the same path can, and `croft
/// edit` is the same command in both.
fn live_exe(exe: std::path::PathBuf) -> Option<std::path::PathBuf> {
    if exe.exists() {
        return Some(exe);
    }
    let replaced = std::path::PathBuf::from(exe.to_str()?.strip_suffix(" (deleted)")?);
    replaced.exists().then_some(replaced)
}

/// The `GIT_SEQUENCE_EDITOR` value naming this croft (#620). git runs it
/// through the shell, so the binary's path is single-quoted.
pub(crate) fn sequence_editor_command(exe: &std::path::Path) -> String {
    let quoted = exe.to_string_lossy().replace('\'', r"'\''");
    format!("'{quoted}' edit --wait --sequence-editor")
}

/// Put `bin` first on the pane's PATH, so `croft` in the pane is this croft
/// (#1854), and name it to the shell hooks. They put it first again once
/// the user's startup files have run, because those run after this: a
/// `.zprofile` running `brew shellenv` puts /opt/homebrew/bin, and a
/// Homebrew croft, back in front.
///
/// With no bin of its own, a pane drops the one it inherited. A croft
/// launched from a croft pane carries the OUTER croft's, and passing that
/// on would answer `croft` with a different croft than the window.
///
/// An unset PATH stays unset, for the shell to fill with its default: PATH
/// set to the bin alone would leave the pane without /usr/bin.
pub(crate) fn apply_pane_bin(cmd: &mut CommandBuilder, bin: Option<&std::path::Path>) {
    use crate::shell_integration::PANE_BIN_ENV;
    let inherited = cmd.get_env(PANE_BIN_ENV).map(std::path::PathBuf::from);
    let path = cmd.get_env("PATH").and_then(|path| {
        let rest = std::env::split_paths(path)
            .filter(|d| Some(d.as_path()) != bin && Some(d) != inherited.as_ref());
        std::env::join_paths(
            bin.map(std::path::Path::to_path_buf)
                .into_iter()
                .chain(rest),
        )
        .ok()
    });
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    match bin {
        Some(bin) => cmd.env(PANE_BIN_ENV, bin),
        None => cmd.env_remove(PANE_BIN_ENV),
    }
}

/// This croft's pane bin, made on first use. None when the running binary
/// is gone from disk (an upgrade removed it) or the cache cannot be
/// written; the pane then finds `croft` on the user's PATH, as before.
fn pane_bin_dir() -> Option<std::path::PathBuf> {
    let exe = live_exe(std::env::current_exe().ok()?)?;
    crate::shell_integration::ensure_pane_bin(&crate::paths::croft_cache_dir(), &exe).ok()
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
        let mut cmd = interactive_shell_command(&shell, &crate::paths::config_dir());
        cmd.cwd(cwd);
        Self::spawn_with_preamble(cmd, None, &transcript_bytes(transcript))
    }

    /// Spawn an interactive login session for a specific shell (a terminal
    /// profile), rather than `$SHELL`. The same login-flag handling as [`new`].
    pub fn new_shell(shell_path: &str, cwd: &std::path::Path) -> Result<Self> {
        let mut cmd = interactive_shell_command(shell_path, &crate::paths::config_dir());
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
        let (lines, _, top) = self.grid_lines_wrapped();
        (lines, top)
    }

    /// [`Self::grid_lines`] plus, per row, whether it soft-wraps into the
    /// next, read under the same lock: redaction joins wrapped rows, since a
    /// secret split across two rows matches neither on its own.
    pub fn grid_lines_wrapped(&self) -> (Vec<String>, Vec<bool>, i32) {
        grid_rows_wrapped(&self.term.lock())
    }

    /// The grid as find's logical lines: soft-wrapped rows joined, so a
    /// match the pane's width cut in two is found (#1275). See
    /// [`join_wrapped_rows`].
    pub fn find_lines(&self) -> Vec<FindLine> {
        let (lines, wraps, top) = self.grid_lines_wrapped();
        join_wrapped_rows(lines, &wraps, top)
    }

    /// [`Self::find_lines`] plus the scroll-clock reading, captured under
    /// ONE term lock. Terminal find stamps its match anchor with the clock;
    /// separate reads would let output land between them and mis-pair a
    /// line with a reading it wasn't captured at.
    pub fn find_lines_and_clock(&self) -> (Vec<FindLine>, i64) {
        let term = self.term.lock();
        let (lines, wraps, top) = grid_rows_wrapped(&term);
        (join_wrapped_rows(lines, &wraps, top), self.clock_now(&term))
    }

    /// Every readable grid row as `(plain, coloured)`: the plain text
    /// exactly as [`Self::grid_lines`] gives it, and the same row with the
    /// cells' colours and attributes written back as SGR, so the text can be
    /// re-rendered elsewhere with the colours the pane showed (#257: "open
    /// scrollback in an editor tab" through the rendered log view). Both
    /// forms come from ONE pass under ONE term lock, so a caller that masks
    /// the plain row and writes the coloured one can never pair a row with
    /// its neighbour after output landed between two reads. Wide-char
    /// spacers are skipped as in `grid_lines`; trailing default-styled
    /// blanks are trimmed (a row padded with a coloured background keeps
    /// those cells, which the plain twin drops, so the two forms can differ
    /// in length there); every row that set a style ends with a reset.
    pub fn grid_lines_ansi(&self) -> Vec<(String, String)> {
        self.grid_lines_ansi_wrapped()
            .into_iter()
            .map(|(plain, ansi, _)| (plain, ansi))
            .collect()
    }

    /// [`Self::grid_lines_ansi`] plus whether each row soft-wraps into the
    /// next (see [`Self::grid_lines_wrapped`]).
    pub fn grid_lines_ansi_wrapped(&self) -> Vec<(String, String, bool)> {
        let term = self.term.lock();
        if term.columns() == 0 {
            return Vec::new();
        }
        let top = term.grid().topmost_line().0;
        ansi_rows(&term, top)
    }

    /// The visible screen as [`Self::grid_lines_ansi_wrapped`] rows, with
    /// the shell cursor's cell on that screen, read under ONE term lock so
    /// the cursor belongs to the rows it is drawn over (#356: a recorded
    /// frame is the screen in colour, cursor included). The cursor is
    /// `None` when the program hid it; scrollback is never walked, so the
    /// cost is one screen however long the history is.
    pub fn screen_ansi_wrapped(&self) -> ScreenAnsi {
        let term = self.term.lock();
        if term.columns() == 0 {
            return (Vec::new(), None);
        }
        let rows = ansi_rows(&term, 0);
        let p = term.grid().cursor.point;
        let cursor = (term.mode().contains(TermMode::SHOW_CURSOR)
            && p.line.0 >= 0
            && (p.line.0 as usize) < rows.len())
        .then_some((p.line.0 as u16, p.column.0 as u16));
        (rows, cursor)
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
    /// render loop paints it in the brighter accent. `abs_line` is the first
    /// row of the match's logical line and `col` a char index into it (see
    /// [`FindLine`]). `clock` is the
    /// scroll-clock reading the line was captured against (the
    /// [`Self::find_lines_and_clock`] snapshot); the render re-bases per
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
        apply_pane_env(
            &mut cmd,
            crate::view_ipc::SOCK_PATH.get().map(|p| p.as_path()),
        );
        // Not under test: the running binary there is the test harness, and
        // its link would land in the developer's real cache dir.
        if !cfg!(test) {
            apply_pane_bin(&mut cmd, pane_bin_dir().as_deref());
        }
        // An interactive shell may live in a pane host (#694); a task or
        // run pane is croft's own.
        let hosted = run_label.is_none() && persistent_panes_enabled();
        Self::spawn_from(PaneSource::Spawn { cmd, hosted }, run_label, preamble)
    }

    /// Reattach to the pane host at `socket` (#694): a pane whose shell
    /// outlived the croft that opened it. The host redraws the screen.
    pub fn attach_hosted(socket: &std::path::Path) -> Result<Self> {
        Self::spawn_from(PaneSource::Attach(socket.to_path_buf()), None, &[])
    }

    fn spawn_from(source: PaneSource, run_label: Option<String>, preamble: &[u8]) -> Result<Self> {
        let cols = 80u16;
        let rows = 24u16;
        let PaneLink {
            mut writer,
            mut reader,
            poll_fd: pty_fd,
            shell_pid,
            backend,
        } = PaneLink::open(source, cols, rows)?;
        let (input_tx, input_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        // Ends once every sender is gone (the pane and its responder), or
        // when the child stops taking input.
        std::thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        });

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
        // The reader answers the OSC 7501 probe on the same channel as the
        // parser's replies, so the answer lands ahead of the reply to a
        // device-attributes query sent after it, which is how a program
        // tells "supported" from "not" without a timeout.
        let status_reply_tx = response_tx.clone();
        let bell = Arc::new(AtomicBool::new(false));
        let clipboard_store = Arc::new(std::sync::Mutex::new(None));
        let listener = VoidListener {
            pty_response_tx: Some(response_tx),
            size: Some(size_shared.clone()),
            bell: Some(bell.clone()),
            clipboard: Some(clipboard_store.clone()),
        };
        let mut term = Term::new(cfg, &term_size, listener);
        // Before the reader thread exists, so nothing the child writes can
        // land ahead of the restored output (#249).
        if !preamble.is_empty() {
            Processor::<StdSyncHandler>::new().advance(&mut term, preamble);
        }
        let term = Arc::new(FairMutex::new(term));
        let term_for_thread = term.clone();
        let input_for_responder = input_tx.clone();

        std::thread::spawn(move || {
            while let Ok(text) = response_rx.recv() {
                if input_for_responder.send(text.into_bytes()).is_err() {
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
        let capture_rows = Arc::new(std::sync::Mutex::new(CaptureRows::default()));
        let capture_rows_for_thread = capture_rows.clone();
        // Shutdown pipe + master fd for the reader's poll gate: the reader
        // must be wakeable without depending on the pty ever reaching EOF.
        let (shutdown_r, shutdown_w) = std::io::pipe().context("shutdown pipe")?;
        // Taken BEFORE the rewind registration below, which has no `?` after
        // it: an early return past a registered buffer would leave every
        // other pane trimmed to a share nobody is using.
        // One budget shared by every pane (#694), re-read per spawn like the
        // scrollback so a settings edit applies without a relaunch. Setting
        // it re-splits the budget over the panes already open; registering
        // then trims them to the smaller share before this pane records.
        let rewind_budget = crate::rewind::budget();
        let rewind_mb = crate::prefs::Prefs::load_or_default().terminal_rewind_mb;
        // Off by default until it can be replayed (#1342), but the recorder's
        // own tests drive it through real panes, and every spawn sets the one
        // shared total: test panes all record under the same explicit figure
        // rather than one turning it off under another.
        #[cfg(test)]
        let rewind_mb = rewind_mb.or(Some(128));
        rewind_budget.set_total(crate::rewind::configured_budget_bytes(rewind_mb));
        let rewind = rewind_budget.register();
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
        #[cfg(test)]
        let finished_tx_for_test = finished_tx.clone();
        let osc7_cwd = Arc::new(std::sync::Mutex::new(Option::<std::path::PathBuf>::None));
        let osc7_for_thread = osc7_cwd.clone();
        let notifications = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let notifications_for_thread = notifications.clone();
        let progress = Arc::new(std::sync::Mutex::new(None::<(u8, u8)>));
        let progress_for_thread = progress.clone();
        let program_status = Arc::new(std::sync::Mutex::new(
            crate::program_status::Records::default(),
        ));
        let program_status_for_thread = program_status.clone();
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

        let reader_thread = std::thread::spawn(move || {
            let mut processor = Processor::<StdSyncHandler>::new();
            let mut port_sniffer = crate::port_detect::PortSniffer::new();
            let mut osc_sniffer = crate::shell_integration::OscSniffer::default();
            let mut wipe_sniffer = WipeSniffer::default();
            let mut trigger_scanner = crate::triggers::TriggerScanner::new();
            let mut trigger_hits = Vec::new();
            let mut watch_engine = crate::problem_matchers::WatchEngine::default();
            let mut watch_lines: Vec<(usize, String)> = Vec::new();
            // Last-seen matcher sources, for detecting a swap mid-window
            // (Arc identity — the app only ever replaces them wholesale).
            let mut last_wset = watch_set_for_thread.lock().unwrap().clone();
            // The engine starts with no task matcher, whatever the slot
            // holds by the time this thread runs: a task assigned to a new
            // pane before its first read must still count as an assignment,
            // or its matcher is never armed (#1541).
            let mut last_pwatch: Option<Arc<crate::problem_matchers::CompiledMatcher>> = None;
            // Per-pane monotonic id for captured inline images; the overlay
            // layout key uses it to tell a new picture from a moved one.
            let mut image_seq = 0u64;
            // Content id of the cursor row after the previous chunk: the
            // rows this chunk touched run from there to the cursor's new
            // id, and each takes the chunk's arrival time (last touch wins,
            // so a row is stamped when its content actually landed, not
            // when the cursor first parked on it).
            let mut prev_stamp_id: i64 = 0;
            // Started by 133;C, finished by the next 133;D.
            let mut running: Option<RunningCommand> = None;
            let mut buf = [0u8; 65536];
            loop {
                // A dev-server URL that ended the last read is held back in
                // case the next read extends it; when the output goes quiet
                // instead, it is sent as it stands (#1449).
                let ready = if port_sniffer.has_pending() {
                    match poll_pty(pty_fd, &shutdown_r, PORT_URL_SETTLE_MS) {
                        PtyWait::Ready => true,
                        PtyWait::Closed => false,
                        PtyWait::Idle => {
                            port_sniffer.flush(&port_tx);
                            continue;
                        }
                    }
                } else {
                    match wait_pty_readable(pty_fd, &shutdown_r, watch_engine.deadline()) {
                        PtyWait::Ready => true,
                        PtyWait::Closed => false,
                        PtyWait::Idle => {
                            // A task matcher's grace ran out with no shell
                            // mark (#1541): its held lines are output after
                            // all.
                            let wset = watch_set_for_thread.lock().unwrap().clone();
                            let pwatch = pane_watch_for_thread.lock().unwrap().clone();
                            let batches = watch_engine.expire(
                                std::time::Instant::now(),
                                &wset,
                                pwatch.as_ref(),
                            );
                            for batch in batches {
                                let cwd = osc7_for_thread.lock().unwrap().clone();
                                let _ = watch_tx.send((cwd, batch));
                            }
                            continue;
                        }
                    }
                };
                if !ready {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        port_sniffer.sniff(&buf[..n], &port_tx);
                        // Shell-integration marks: split the advance at each
                        // OSC 133 so the cursor can be sampled exactly where
                        // the mark landed (alacritty drops the sequences
                        // themselves as unknown OSC).
                        let osc_events = osc_sniffer.scan(&buf[..n]);
                        // Where the shell's prompt and command marks fall in
                        // the chunk, for the watch engine below (#1541): the
                        // lines before a task's `133;C` are the shell echoing
                        // its command line, not its output.
                        let watch_marks: Vec<(usize, bool)> = osc_events
                            .iter()
                            .filter_map(|(end, ev)| {
                                use crate::shell_integration::OscEvent as E;
                                match ev {
                                    E::CommandStart => Some((*end, true)),
                                    E::PromptStart | E::PromptEnd | E::CommandEnd(_) => {
                                        Some((*end, false))
                                    }
                                    _ => None,
                                }
                            })
                            .collect();
                        // Notify/bell triggers match completed output lines,
                        // never inside a full-screen app (the alt screen owns
                        // the bytes; iTerm2 skips those too). The scanner
                        // tracks the alt boundary POSITIONALLY from the
                        // stream — the chunk's final mode says nothing about
                        // where a line fell — so it must see EVERY chunk, or
                        // its alt tracking and string state machine desync
                        // and skipped-gap output splices into phantom lines.
                        // It runs BEFORE the parse, so the parser can stop
                        // right after each captured line and read the row it
                        // landed on (#1450).
                        let trig = triggers_for_thread.lock().unwrap().clone();
                        // Watch problem matchers (#252) ride the same
                        // scanner: when any are configured, the completed
                        // lines it already produces feed the per-pane watch
                        // engine (no second byte scan over the stream).
                        let wset = watch_set_for_thread.lock().unwrap().clone();
                        let pwatch = pane_watch_for_thread.lock().unwrap().clone();
                        let watching = !wset.matchers.is_empty() || pwatch.is_some();
                        if watching {
                            trigger_scanner.scan_collect(
                                &buf[..n],
                                &trig,
                                &mut trigger_hits,
                                Some(&mut watch_lines),
                            );
                        } else {
                            trigger_scanner.scan(&buf[..n], &trig, &mut trigger_hits);
                        }
                        let mut t = term_for_thread.lock();
                        let wipes =
                            wipe_sniffer.scan(&buf[..n], t.mode().contains(TermMode::ALT_SCREEN));
                        let screen_wiped = wipes.iter().any(|w| w.screen);
                        let hist_wiped = wipes.iter().any(|w| w.hist);
                        // The parser also stops at each wipe, so the running
                        // command's output start is moved right where a
                        // wipe erased its row, before the scroll clock
                        // loses count of what scrolls after it (#876).
                        let mut stops: Vec<(usize, ReadStop)> = osc_events
                            .into_iter()
                            .map(|(end, ev)| (end, ReadStop::Mark(ev)))
                            .chain(wipes.into_iter().map(|w| (w.end, ReadStop::Wipe(w))))
                            .chain(trigger_hits.iter().enumerate().filter_map(|(i, h)| {
                                (h.action == crate::triggers::TriggerAction::Capture)
                                    .then_some((h.end, ReadStop::Capture(i)))
                            }))
                            .collect();
                        stops.sort_by_key(|(end, _)| *end);
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
                        for (end, stop) in stops {
                            processor.advance(&mut *t, &buf[done..end]);
                            done = end;
                            let ev = match stop {
                                ReadStop::Mark(ev) => ev,
                                ReadStop::Wipe(w) => {
                                    // Only RIS wipes both, and a full reset
                                    // removes every OSC 7501 record.
                                    if w.screen && w.hist {
                                        program_status_for_thread.lock().unwrap().reset();
                                    }
                                    let now = clock_for_thread.lock().unwrap().tick(&mut t);
                                    if let Some(run) = running.as_mut() {
                                        run.follow_wipe(w, now);
                                    }
                                    capture_rows_for_thread.lock().unwrap().follow_wipe(w, now);
                                    continue;
                                }
                                ReadStop::Capture(i) => {
                                    let mut clock = clock_for_thread.lock().unwrap();
                                    let mut rows = capture_rows_for_thread.lock().unwrap();
                                    let anchor =
                                        rows.record(&mut t, &mut clock, &trigger_hits[i].line);
                                    trigger_hits[i].anchor = Some(anchor);
                                    continue;
                                }
                            };
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
                                E::ProgramStatus(report) => {
                                    if report == crate::program_status::Report::Query {
                                        let reply = crate::program_status::PROBE_REPLY;
                                        let _ = status_reply_tx.send(reply.to_string());
                                    }
                                    program_status_for_thread.lock().unwrap().apply(report);
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
                                    // The program that reported is gone once
                                    // the shell draws its prompt again.
                                    if kind == E::PromptStart {
                                        program_status_for_thread.lock().unwrap().shell_prompt();
                                    }
                                    let dur = match &kind {
                                        E::CommandEnd(exit) => {
                                            // A finished command's gauge is
                                            // stale even if the program never
                                            // sent the state-0 clear.
                                            *progress_for_thread.lock().unwrap() = None;
                                            running.take().map(|run| {
                                                let dur = run.started.elapsed();
                                                // The typed command text and the
                                                // shell's cwd travel with the
                                                // completion so the durable
                                                // history records context.
                                                let now =
                                                    clock_for_thread.lock().unwrap().tick(&mut t);
                                                let start =
                                                    run.out_line - (now - run.out_clock) as i32;
                                                let output = command_output_text(&t, start);
                                                let cwd = osc7_for_thread.lock().unwrap().clone();
                                                let host =
                                                    osc7_host_for_thread.lock().unwrap().clone();
                                                let _ = finished_tx.send(FinishedCommand {
                                                    exit: *exit,
                                                    dur,
                                                    cmd: run.typed,
                                                    cwd,
                                                    host,
                                                    output,
                                                });
                                                dur
                                            })
                                        }
                                        _ => None,
                                    };
                                    let line_rec = t.grid().cursor.point.line.0;
                                    let clock_rec = clock_for_thread.lock().unwrap().tick(&mut t);
                                    let col_rec = t.grid().cursor.point.column.0;
                                    let started = matches!(kind, E::CommandStart);
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
                                    // Read the typed line now, while it is
                                    // still on screen: the command may clear
                                    // it before its 133;D (#876).
                                    if started {
                                        running = Some(RunningCommand {
                                            started: std::time::Instant::now(),
                                            typed: last_command_input_text(&t, &ms, clock_rec),
                                            out_line: line_rec,
                                            out_clock: clock_rec,
                                        });
                                    }
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
                        // A newly assigned task matcher waits for its
                        // command's output to start (#1541). A swapped
                        // global set only matters to a pane with no task
                        // matcher: the task's matcher owns its pane's
                        // windows, and dropping one here lost the task's
                        // first cycle whenever the app's first sync of the
                        // global set landed mid-build.
                        if !pwatch_same {
                            watch_engine.assign(pwatch.as_ref(), std::time::Instant::now());
                        } else if !Arc::ptr_eq(&wset, &last_wset) && pwatch.is_none() {
                            watch_engine.reset();
                        }
                        last_wset = wset.clone();
                        last_pwatch = pwatch.clone();
                        if watching {
                            // Fed up to each mark, then the mark, so the
                            // engine sees them in stream order.
                            let mut lines = watch_lines.drain(..).peekable();
                            let ends = watch_marks.iter().map(|&(end, c)| (end, Some(c)));
                            for (end, mark) in ends.chain(std::iter::once((n, None))) {
                                while let Some((_, line)) = lines.next_if(|(at, _)| *at <= end) {
                                    if let Some(batch) =
                                        watch_engine.feed(&line, &wset, pwatch.as_ref())
                                    {
                                        let cwd = osc7_for_thread.lock().unwrap().clone();
                                        let _ = watch_tx.send((cwd, batch));
                                    }
                                }
                                if let Some(command_start) = mark {
                                    watch_engine.mark(command_start, pwatch.as_ref());
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
            program_status,
            status_mark: 0,
            status_seated: 0,
            port_rx,
            backend,
            input_tx,
            reader_thread: Some(reader_thread),
            reader_shutdown: Some(shutdown_w),
            shell_pid,
            exit: None,
            reaped_at: None,
            uid: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            input_seen: false,
            last_input_ms: 0,
            run_pane: script_mode,
            manual_name_seen: false,
            cols,
            rows,
            size_shared,
            focused: false,
            broadcast_excluded: false,
            collapsed: false,
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
            clipboard_store,
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
            capture_rows,
            rewind,
            #[cfg(test)]
            written_for_test: Arc::new(std::sync::Mutex::new(Vec::new())),
            #[cfg(test)]
            finished_tx_for_test,
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
        without_continuation_prompts(&extract_selection_text(
            &term,
            line,
            col,
            d.output_start - 1,
            term.columns().saturating_sub(1),
        ))
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

    /// The text the child last copied through OSC 52 since the previous
    /// call, if any (drains the latch). Only the newest copy survives a
    /// tick: the clipboard holds one value, so older ones were overwritten
    /// anyway.
    pub fn take_clipboard_store(&self) -> Option<String> {
        self.clipboard_store.lock().unwrap().take()
    }

    /// The pane's foreground process group leader pid (what owns the tty now):
    /// the shell at a prompt, or a running command. `None` if unavailable.
    pub fn foreground_pid(&self) -> Option<i32> {
        self.backend.process_group_leader()
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

    /// Whether input was written that the shell has not echoed yet, so the
    /// prompt line on screen lags what was typed.
    pub fn awaiting_echo(&self) -> bool {
        self.last_input_ms > self.last_output_ms.load(Ordering::Relaxed)
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

    /// The status the pane's agent reported itself over OSC 7501, `None`
    /// when it has reported nothing.
    pub fn reported_status(&self) -> Option<crate::program_status::State> {
        self.program_status
            .lock()
            .unwrap()
            .reported_since(self.status_mark)
    }

    /// Keep the pane's OSC 7501 reports to the agent a sample found, given
    /// the agent the last sample found. Reports made while no agent had the
    /// foreground are no agent's. When one agent replaces another between
    /// two samples, a probe or shell prompt in the stream since the first
    /// was found divides their reports; with neither, the second's so far
    /// cannot be told from the first's and go too.
    pub fn sample_reporter(&mut self, prev: Option<&str>, next: Option<&str>) {
        let records = self.program_status.lock().unwrap();
        match (prev, next) {
            (_, None) => self.status_mark = records.seq(),
            (None, Some(_)) => self.status_seated = records.seq(),
            (Some(p), Some(n)) if p != n => {
                if !records.fresh_since(self.status_seated) {
                    self.status_mark = records.seq();
                }
                self.status_seated = records.seq();
            }
            _ => {}
        }
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

    /// What has been typed at the shell prompt, when the cursor sits at its
    /// end (#614): `(typed, cursor screen x, cursor screen y, pane right
    /// edge)`. `None` unless the newest shell-integration mark is the prompt
    /// end on the cursor's own row, the view is live (not scrolled back, not
    /// a full-screen program), and nothing is drawn right of the cursor.
    /// That last rule is what keeps croft out of the way of a shell drawing
    /// its own suggestion (fish, zsh-autosuggestions), which lands there.
    pub fn prompt_tail(&self) -> Option<(String, u16, u16, u16)> {
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) || term.grid().display_offset() != 0 {
            return None;
        }
        let cursor = term.grid().cursor.point;
        let line = cursor.line.0;
        let now = self.clock_now(&term);
        let ms = self.marks.lock().unwrap();
        let last = ms.last()?;
        if !matches!(last.kind, crate::shell_integration::OscEvent::PromptEnd)
            || last.line_rec - (now - last.clock_rec) as i32 != line
        {
            return None;
        }
        let (text, colmap) = row_text_and_cols(&term, line);
        let cur = cursor.column.0;
        let b_col = last.col_rec;
        // Characters at or right of the cursor: must be none.
        if colmap
            .iter()
            .zip(text.chars())
            .any(|(&c, ch)| c >= cur && !ch.is_whitespace())
        {
            return None;
        }
        let typed: String = colmap
            .iter()
            .zip(text.chars())
            .filter(|(c, _)| **c >= b_col && **c < cur)
            .map(|(_, ch)| ch)
            .collect();
        let inner = self.last_inner;
        if inner.width == 0 || line < 0 || line as u16 >= inner.height {
            return None;
        }
        Some((
            typed,
            inner.x + cur as u16,
            inner.y + line as u16,
            inner.x + inner.width,
        ))
    }

    /// The shell cursor's screen cell, when it is on screen and the program
    /// hasn't hidden it (#621: screen reader mode parks the cursor here).
    pub fn screen_cursor(&self) -> Option<(u16, u16)> {
        let term = self.term.lock();
        if !term.mode().contains(TermMode::SHOW_CURSOR) || term.grid().display_offset() != 0 {
            return None;
        }
        let p = term.grid().cursor.point;
        let inner = self.last_inner;
        let (line, col) = (p.line.0, p.column.0 as u16);
        if inner.width == 0 || line < 0 || line as u16 >= inner.height || col >= inner.width {
            return None;
        }
        Some((inner.x + col, inner.y + line as u16))
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

    /// The grid line where a captured line starts, or `None`. `anchor` is
    /// this pane's id for the row its reader saw the line start on
    /// ([`crate::triggers::TriggerHit::anchor`]): that row, or the matching
    /// row nearest it within [`CAPTURE_SLACK`] (the upper one on a tie), so a
    /// line every re-run prints again finds its own copy rather than the
    /// newest (#1450). Once the row was erased or rotated off the scrollback
    /// the line is gone, never stood in for by another copy. Without an
    /// anchor the newest match wins. The straight case is a row carrying
    /// `needle` as a prefix; a pane narrower than the needle instead shows
    /// the line's FIRST WRAPPED ROW, whose text is a prefix OF the needle.
    /// That reverse arm is gated on real wrap metadata, not row length: the
    /// row must itself soft-wrap (WRAPLINE on its last cell) — otherwise any
    /// short unrelated row sharing the prefix would win — and continuation
    /// rows (the PREVIOUS row wraps) never match either arm, or a later
    /// continuation sharing the prefix would beat the true start under the
    /// newest-first scan.
    pub fn find_captured_line(&self, needle: &str, anchor: Option<u64>) -> Option<i32> {
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
        let starts = |line: i32| {
            if !(top..=bottom).contains(&line) || (line > top && wraps(line - 1)) {
                return false;
            }
            let (text, _) = row_text_and_cols(&term, line);
            let t = text.trim_end();
            !t.is_empty() && (t.starts_with(needle) || (wraps(line) && needle.starts_with(t)))
        };
        let Some(id) = anchor else {
            return (top..=bottom).rev().find(|&line| starts(line));
        };
        let now = self.clock_now(&term);
        let at = match self.capture_rows.lock().unwrap().line(id, now, needle)? {
            RowAt::Line(at) => at,
            RowAt::Unknown { later } => {
                return (top..=bottom).rev().filter(|&line| starts(line)).nth(later);
            }
        };
        if at < i64::from(top) {
            return None;
        }
        let at = at.min(i64::from(bottom)) as i32;
        (0..=CAPTURE_SLACK).find_map(|d| [at - d, at + d].into_iter().find(|&line| starts(line)))
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
        let (_, colmap) = row_text_and_cols(&term, line);
        // Matched over the logical line, as the painter masks it: a secret
        // a soft wrap cut in two is found from a click on either half.
        let (text, offset) = logical_row_text(&term, line);
        drop(term);
        // The clicked column as a char index: a wide char's spacer column
        // resolves to the wide char itself (the `line_text_at` rule).
        let vc = vc as usize;
        // `row_text_and_cols` maps every non-spacer column, blanks
        // included, so a click on a blank cell resolves to that blank's own
        // char index - never back into a token. The `rposition` only ever
        // steps back for a wide char's spacer column, which is the wide
        // char itself.
        let ci = offset + colmap.iter().rposition(|&gc| gc <= vc)?;
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
    /// inherits the directory the user has `cd`'d into. None once the shell
    /// has exited: a reaped pid may already name another process.
    pub fn pid(&self) -> Option<u32> {
        self.backend.pid().filter(|_| self.exit.is_none())
    }

    /// Notice that the pane's shell has exited (#912): `exit`, Ctrl+D, a
    /// task that ran `exit 3`. The shell is reaped (it stayed a zombie for
    /// as long as the pane lived), and once the reader has painted all it
    /// wrote, the exit goes under its output and the pane is marked
    /// exited. Returns the exit code the first time only.
    pub fn poll_exit(&mut self) -> Option<Option<i32>> {
        if self.exit.is_some() {
            return None;
        }
        let output_closed = self.reader_thread.as_ref().is_none_or(|h| h.is_finished());
        let code = self.backend.try_exit(output_closed)?;
        if !output_closed {
            let since = *self.reaped_at.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() < EXIT_OUTPUT_GRACE {
                return None;
            }
        }
        self.exit = Some(code);
        // A run pane's reader paints its own footer.
        if !self.run_pane {
            let footer = match code {
                Some(c) => format!("\r\n\x1b[2m[Process exited with code {c}]\x1b[22m\r\n"),
                None => String::from("\r\n\x1b[2m[Process exited]\x1b[22m\r\n"),
            };
            Processor::<StdSyncHandler>::new().advance(&mut *self.term.lock(), footer.as_bytes());
            self.pty_dirty.store(true, Ordering::Release);
        }
        Some(code)
    }

    /// Whether [`Self::poll_exit`] has found the shell exited: the pane
    /// then takes no input.
    pub fn has_exited(&self) -> bool {
        self.exit.is_some()
    }

    /// Whether this is a `new_running` program pane rather than a shell.
    pub fn is_run_pane(&self) -> bool {
        self.run_pane
    }

    /// The pane host holding this pane's shell, when one does (#694): what
    /// a saved session records so the next croft can reattach.
    pub fn host_socket(&self) -> Option<&std::path::Path> {
        match &self.backend {
            PaneBackend::Hosted { socket, .. } => Some(socket),
            PaneBackend::Local { .. } => None,
        }
    }

    /// Read the dirty flag without clearing it. Lets the main loop decide
    /// whether to redraw now or coalesce, without losing the signal if we
    /// choose to skip this iteration.
    pub fn peek_dirty(&self) -> bool {
        self.pty_dirty.load(Ordering::Acquire)
    }

    /// Current scrollback offset: how many rows the viewport is scrolled
    /// up from the live bottom. Viewport row `r` maps to absolute grid
    /// line `r - display_offset`.
    fn display_offset(&self) -> i32 {
        self.term.lock().grid().display_offset() as i32
    }

    /// Text of the logical line under screen cell `(col, row)`, plus the
    /// 0-based char index within that text the click landed on. Drives
    /// Cmd/Ctrl+click URL and `path:line` detection. A line the terminal
    /// soft-wrapped (WRAPLINE) is stitched back together from all its rows,
    /// so a long URL resolves whole from any of them (#1355). `None` when
    /// the cell is outside the content area.
    pub fn line_text_at(&self, col: u16, row: u16) -> Option<(String, usize)> {
        let (r, c) = self.cell_at(col, row)?;
        let term = self.term.lock();
        let line = r as i32 - term.grid().display_offset() as i32;
        let (row_text, cols_map) = row_text_and_cols(&term, line);
        // Grid column → char index in the spacer-skipped text: the last
        // produced char at-or-before the clicked column, so clicking a wide
        // char's spacer cell resolves to the wide char itself.
        let idx = cols_map.iter().rposition(|&gc| gc <= c as usize)?;
        let top = -(term.grid().history_size() as i32);
        let bottom = term.screen_lines() as i32 - 1;
        let mut first = line;
        while first > top && row_wraps(&term, first - 1) {
            first -= 1;
        }
        let mut last = line;
        while last < bottom && row_wraps(&term, last) {
            last += 1;
        }
        if first == line && last == line {
            return Some((row_text, idx));
        }
        let mut text = String::new();
        let mut offset = 0;
        for l in first..=last {
            let part = if l == line {
                offset = text.chars().count();
                row_text.clone()
            } else {
                row_text_and_cols(&term, l).0
            };
            text.push_str(&part);
        }
        Some((text, offset + idx))
    }

    /// The OSC 8 hyperlink under a screen position (host cell coords), if
    /// any — the screen-coord twin of [`Self::hyperlink_at`].
    pub fn hyperlink_at_screen(&self, col: u16, row: u16) -> Option<String> {
        let (r, c) = self.cell_at(col, row)?;
        self.hyperlink_at(r as usize, c as usize)
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

    /// Scrollback rows held, the live screen's rows, and the grid's width,
    /// for the memory report (#694).
    pub fn grid_extent(&self) -> (usize, usize, usize) {
        let term = self.term.lock();
        (
            term.grid().history_size(),
            term.screen_lines(),
            term.columns(),
        )
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

    /// Test-only: the pane's grid lock, for tests that must observe what
    /// `resize` does WHILE the grid is held (#473).
    #[cfg(test)]
    pub fn grid_lock_for_test(&self) -> Arc<FairMutex<Term<VoidListener>>> {
        self.term.clone()
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

    /// The pane's stable identity (see the `uid` field).
    pub fn uid(&self) -> u64 {
        self.uid
    }

    pub fn write_input(&mut self, data: &[u8]) {
        self.reset_scrollback();
        self.input_seen = true;
        self.last_input_ms = now_ms();
        #[cfg(test)]
        self.written_for_test
            .lock()
            .unwrap()
            .extend_from_slice(data);
        let _ = self.input_tx.send(data.to_vec());
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Test-only: every byte the app has written toward the child's stdin.
    #[cfg(test)]
    pub fn written_bytes_for_test(&self) -> Vec<u8> {
        self.written_for_test.lock().unwrap().clone()
    }

    /// Test-only: report a finished command as the reader thread does at
    /// its `133;D` mark, so app tests can end a pane's command without a
    /// shell that speaks OSC 133.
    #[cfg(test)]
    pub fn finish_command_for_test(&self, exit: Option<i32>, dur: std::time::Duration) {
        let _ = self.finished_tx_for_test.send(FinishedCommand {
            exit,
            dur,
            cmd: String::new(),
            cwd: None,
            host: None,
            output: String::new(),
        });
    }

    /// Seed a `cd <path>` into the embedded shell so the terminal
    /// follows an Explorer-side workspace-root change. Sent verbatim to
    /// the PTY input - the shell parses it like a typed command. See
    /// `format_cd_command` for the line-clearing prefix and the path
    /// quoting strategy.
    pub fn change_cwd(&mut self, path: &std::path::Path) {
        self.write_input(&format_cd_command(path));
    }

    /// Whether the program in the pane turned on bracketed paste, so a
    /// pasted line break waits for Enter instead of running the line (#1274).
    pub fn bracketed_paste(&self) -> bool {
        self.term.lock().mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Paste a payload into the embedded shell. Bracketed-paste markers
    /// are added only if the inner program asked for them; otherwise the
    /// payload is sent raw so simple shells don't see literal `\e[200~`.
    pub fn paste_input(&mut self, payload: &[u8]) {
        // The terminal's own mode, as its parser tracked it: the byte
        // sniffer missed a `\e[?2004h` split across two reads, a combined
        // `\e[?1049;2004h` and a reset, and pasted line by line into a shell
        // that had asked for bracketed paste.
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            self.write_input(b"\x1b[200~");
            // No ESC inside the brackets, as xterm and VS Code do: pasted
            // text holding `\e[201~` would close the paste early, and the
            // shell would run what follows it (`ls\e[201~rm -rf ~\r`). Any
            // program in a pane can plant that on the clipboard via OSC 52.
            let clean: Vec<u8> = payload.iter().copied().filter(|&b| b != 0x1b).collect();
            self.write_input(&clean);
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
    /// still safe. `tcgetpgrp` is identical on macOS and Linux. A shell
    /// that has exited (#912) owns nothing.
    pub fn foreground_is_shell(&self) -> bool {
        if self.exit.is_some() {
            return false;
        }
        match (self.backend.process_group_leader(), self.shell_pid) {
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
                match (self.backend.process_group_leader(), self.shell_pid) {
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
        match (self.backend.process_group_leader(), self.shell_pid) {
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
            // The captured lines' rows go too: a CAPTURES entry must say its
            // line is gone, not jump to a copy printed after the Clear
            // (#1450). Cleared under the same `term` lock as the grid (lock
            // order term -> capture_rows): the reader records a new capture
            // while it holds `term`, so dropping the lock first would let a
            // line printed in between keep its row on screen but lose it here.
            self.capture_rows.lock().unwrap().rows.clear();
        }
        // The content the images and OSC 133 marks anchored to is gone;
        // keeping either leaves overlays (a floating picture, phantom
        // decoration dots whose menus copy or re-run the wrong rows) over
        // the fresh prompt.
        self.images.lock().unwrap().clear();
        self.marks.lock().unwrap().clear();
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// The PTY grid's current column count.
    ///
    /// Test-only, and the point of it is #313: a collapsed pane must not be
    /// reflowed into its one-column strip. `resize` clamps to two columns, so
    /// a collapsed pane that got rendered would not crash - it would quietly
    /// rewrap the running shell to a two-column grid and lose everything on
    /// its screen. Reading the real grid width is what tells that apart from
    /// a pane that was simply left alone.
    #[cfg(test)]
    pub fn grid_cols(&self) -> u16 {
        self.cols
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
        // Grid first, pty second, under one hold of the grid lock (#473).
        // The pty ioctl delivers SIGWINCH, and a full-screen child answers
        // it with a complete repaint sized to the NEW dimensions. If that
        // repaint reaches the reader before the grid has been resized, it
        // is parsed into the old grid: rows land on the wrong lines, wide
        // lines wrap, and once the grid does grow those cells sit where a
        // diff-based renderer (Claude Code, and anything else that only
        // redraws what it believes changed) considers them already blank.
        // Nothing ever repaints them, and the stale text stays on screen.
        // Resizing the grid while holding the lock across the ioctl means
        // every byte the child emits in reply meets a grid of the right
        // size.
        let mut term = self.term.lock();
        let size = TermSize::new(cols as usize, rows as usize);
        let mut clock = self.clock.lock().unwrap();
        self.capture_rows
            .lock()
            .unwrap()
            .reflow(&mut term, &mut clock, |term| resize_term(term, size));
        drop(clock);
        self.backend.resize(cols, rows);
        drop(term);
        self.pty_dirty.store(true, Ordering::Release);
    }
}

/// Where a pane's shell comes from.
enum PaneSource {
    /// Start `cmd`: on a PTY croft holds, or in a new pane host.
    Spawn { cmd: CommandBuilder, hosted: bool },
    /// Reattach to the pane host at this socket (#694).
    Attach(std::path::PathBuf),
}

/// Who holds a pane's PTY.
enum PaneBackend {
    /// croft: the PTY dies with croft.
    Local {
        master: Box<dyn MasterPty + Send>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
    },
    /// A pane host (#694): the shell outlives croft. `conn` carries input,
    /// resizes and the kill; the pid comes from the host's greeting, and
    /// `exit` from its notice that the shell ended.
    Hosted {
        conn: Arc<std::sync::Mutex<std::os::unix::net::UnixStream>>,
        socket: std::path::PathBuf,
        pid: Option<u32>,
        exit: Arc<std::sync::Mutex<Option<i32>>>,
    },
}

impl PaneBackend {
    fn pid(&self) -> Option<u32> {
        match self {
            PaneBackend::Local { child, .. } => child.process_id(),
            PaneBackend::Hosted { pid, .. } => *pid,
        }
    }

    /// The terminal's foreground process group: `tcgetpgrp` on a PTY croft
    /// holds, the shell's `tpgid` from the process table for a hosted one
    /// (the same value, read without the master).
    fn process_group_leader(&self) -> Option<i32> {
        match self {
            PaneBackend::Local { master, .. } => master.process_group_leader(),
            PaneBackend::Hosted { pid, .. } => pid.and_then(|p| foreground_group_of(p as i32)),
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        match self {
            PaneBackend::Local { master, .. } => {
                let _ = master.resize(PtySize {
                    cols,
                    rows,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            PaneBackend::Hosted { conn, .. } => {
                let frame = crate::session_host::encode_control_frame(
                    &crate::session_host::Control::Resize { cols, rows },
                );
                let _ = conn.lock().unwrap().write_all(&frame);
            }
        }
    }

    /// The shell's exit code once it has ended, reaping it on a PTY croft
    /// holds (#912). A pane host reaps its own shell and sends the code
    /// before the pane's output closes, so it is read once
    /// `output_closed`; a host gone without sending one gives `Some(None)`.
    fn try_exit(&mut self, output_closed: bool) -> Option<Option<i32>> {
        match self {
            PaneBackend::Local { child, .. } => {
                let status = child.try_wait().ok()??;
                Some(Some(status.exit_code() as i32))
            }
            PaneBackend::Hosted { exit, .. } => output_closed.then(|| *exit.lock().unwrap()),
        }
    }

    /// End the shell: kill and reap it, or ask its host to (which ends the
    /// host too), then drop the connection so the demux thread ends.
    fn end(&mut self) {
        match self {
            PaneBackend::Local { child, .. } => {
                // A shell already reaped is never signalled: its pid may
                // name another process by now.
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                let _ = child.kill();
                let _ = child.wait();
            }
            PaneBackend::Hosted { conn, .. } => {
                let frame =
                    crate::session_host::encode_control_frame(&crate::session_host::Control::Kill);
                let conn = conn.lock().unwrap();
                let _ = (&*conn).write_all(&frame);
                let _ = conn.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// The foreground process group of the terminal `pid` (a session leader)
/// controls: field 8 (`tpgid`) of `/proc/<pid>/stat` on Linux, the BSD
/// info's `e_tpgid` from `proc_pidinfo` on macOS. Hosted panes run only
/// where one of these answers.
#[cfg(target_os = "linux")]
fn foreground_group_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_tpgid(&stat)
}

#[cfg(target_os = "macos")]
fn foreground_group_of(pid: i32) -> Option<i32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // Our own child's info needs no entitlement, as for its cwd above.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if got != size {
        return None;
    }
    let tpgid = info.e_tpgid as i32;
    (tpgid > 0).then_some(tpgid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn foreground_group_of(_pid: i32) -> Option<i32> {
    None
}

/// `tpgid` from a `/proc/<pid>/stat` line. The command name is in
/// parentheses and may hold spaces or parentheses itself, so the fields
/// are counted from the LAST `)`.
#[cfg(any(target_os = "linux", test))]
fn parse_tpgid(stat: &str) -> Option<i32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // state ppid pgrp session tty_nr tpgid
    let tpgid: i32 = rest.split_whitespace().nth(5)?.parse().ok()?;
    (tpgid > 0).then_some(tpgid)
}

/// Whether new shell panes go in pane hosts (#694): the
/// `terminal_persistent_panes` setting, on Linux and macOS, where a hosted
/// pane can still read its foreground process group.
fn persistent_panes_enabled() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
        && crate::prefs::Prefs::load_or_default().terminal_persistent_panes
}

/// What a pane needs from wherever its shell runs: bytes to it, bytes from
/// it with an fd to poll for them, and the shell's pid.
struct PaneLink {
    writer: Box<dyn Write + Send>,
    reader: Box<dyn Read + Send>,
    poll_fd: std::os::fd::RawFd,
    shell_pid: Option<i32>,
    backend: PaneBackend,
}

/// Input for a hosted pane: each write is one bytes frame.
struct HostInput(Arc<std::sync::Mutex<std::os::unix::net::UnixStream>>);

impl Write for HostInput {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let frame = crate::session_host::encode_bytes_frame(buf);
        (&*self.0.lock().unwrap()).write_all(&frame)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl PaneLink {
    fn open(source: PaneSource, cols: u16, rows: u16) -> Result<Self> {
        match source {
            PaneSource::Spawn { cmd, hosted: true } => {
                let spec = crate::pane_host::PaneSpec {
                    socket: crate::pane_host::socket_for(&crate::pane_host::new_pane_id()),
                    cwd: cmd.get_cwd().map(std::path::PathBuf::from),
                    env: cmd
                        .iter_full_env_as_str()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                    cols,
                    rows,
                    argv: cmd
                        .get_argv()
                        .iter()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect(),
                };
                // A host that cannot start (no socket dir, a sandbox) must
                // not cost the user a terminal: the pane is croft's instead.
                match crate::pane_host::spawn(&spec)
                    .and_then(|()| Self::attach(&spec.socket, cols, rows))
                {
                    Ok(link) => Ok(link),
                    // Nothing to print from here: a write to stderr would
                    // land on croft's own screen (#537).
                    Err(_) => Self::local(cmd, cols, rows),
                }
            }
            PaneSource::Spawn { cmd, hosted: false } => Self::local(cmd, cols, rows),
            PaneSource::Attach(socket) => Self::attach(&socket, cols, rows),
        }
    }

    fn local(cmd: CommandBuilder, cols: u16, rows: u16) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;
        let child = pair.slave.spawn_command(cmd).context("spawn child")?;
        let shell_pid = child.process_id().map(|p| p as i32);
        drop(pair.slave);
        let writer = pair.master.take_writer().context("take writer")?;
        let reader = pair.master.try_clone_reader().context("clone reader")?;
        let poll_fd = pair
            .master
            .as_raw_fd()
            .context("pty master has no raw fd")?;
        Ok(Self {
            writer,
            reader,
            poll_fd,
            shell_pid,
            backend: PaneBackend::Local {
                master: pair.master,
                child,
            },
        })
    }

    /// Attach to a pane host: read its greeting for the shell's pid, then
    /// hand the pane a pipe a demux thread fills with the shell's bytes
    /// (the redraw first), so the reader reads it exactly as it reads a
    /// PTY. The pipe closes when the host says the shell exited or the
    /// connection ends, which the reader sees as the PTY's EOF.
    fn attach(socket: &std::path::Path, cols: u16, rows: u16) -> Result<Self> {
        use crate::session_host::{Control, Frame, FrameReader};
        use std::os::fd::AsRawFd;
        let stream = crate::pane_host::PaneClient::connect(socket, cols, rows)?.into_stream();
        let mut rd = stream
            .try_clone()
            .context("cloning the pane host connection")?;
        rd.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        let mut frames = FrameReader::new();
        let mut pending: Vec<u8> = Vec::new();
        let mut pid = None;
        // A shell that exits while it attaches can have its exit arrive in
        // the same read as the greeting (#912): kept, not dropped with the
        // frames the greeting has no use for.
        let mut greeting_exit = None;
        let mut buf = [0u8; 65536];
        while pid.is_none() {
            let n = rd
                .read(&mut buf)
                .context("reading the pane host's greeting")?;
            anyhow::ensure!(n > 0, "the pane host at {} closed", socket.display());
            for frame in frames.push(&buf[..n]) {
                match frame {
                    Frame::Control(Control::PaneInfo { pid: p }) => pid = Some(p),
                    Frame::Bytes(b) => pending.extend_from_slice(&b),
                    Frame::Control(Control::Exit { code }) => greeting_exit = Some(code),
                    _ => {}
                }
            }
        }
        rd.set_read_timeout(None)?;
        let (pipe_r, mut pipe_w) = std::io::pipe().context("pane host pipe")?;
        let exited = greeting_exit.is_some();
        let exit = Arc::new(std::sync::Mutex::new(greeting_exit));
        let exit_for_demux = exit.clone();
        std::thread::spawn(move || {
            if pipe_w.write_all(&pending).is_err() || exited {
                return;
            }
            let mut buf = [0u8; 65536];
            loop {
                let n = match rd.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                for frame in frames.push(&buf[..n]) {
                    match frame {
                        Frame::Bytes(b) => {
                            if pipe_w.write_all(&b).is_err() {
                                return;
                            }
                        }
                        Frame::Control(Control::Exit { code }) => {
                            *exit_for_demux.lock().unwrap() = Some(code);
                            return;
                        }
                        _ => {}
                    }
                }
            }
        });
        let conn = Arc::new(std::sync::Mutex::new(stream));
        Ok(Self {
            writer: Box::new(HostInput(conn.clone())),
            poll_fd: pipe_r.as_raw_fd(),
            reader: Box::new(pipe_r),
            shell_pid: pid.map(|p| p as i32),
            backend: PaneBackend::Hosted {
                conn,
                socket: socket.to_path_buf(),
                pid,
                exit,
            },
        })
    }
}

/// Block until the pty has data (or EOF/error — the following `read` then
/// observes it), until the shutdown pipe signals (its write end closed in
/// `Drop`), or until `deadline`, when there is one, passes (#1541). A bare
/// blocking `read` is not enough to ever exit: on Linux a background job
/// keeps the pty slave open after the shell dies, so the read never returns
/// and joining the reader would freeze the UI for as long as the job runs.
fn wait_pty_readable(
    pty_fd: std::os::fd::RawFd,
    shutdown: &std::io::PipeReader,
    deadline: Option<std::time::Instant>,
) -> PtyWait {
    loop {
        let timeout = match deadline {
            None => -1,
            Some(d) => {
                let left = d.saturating_duration_since(std::time::Instant::now());
                // Rounded up, so the wake is never early.
                left.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
            }
        };
        match poll_pty(pty_fd, shutdown, timeout) {
            // Woken before `deadline` all the same: wait out the rest.
            PtyWait::Idle if deadline.is_some_and(|d| std::time::Instant::now() < d) => {}
            wait => return wait,
        }
    }
}

/// How long the reader waits for more output before sending a held-back
/// dev-server URL as it stands.
const PORT_URL_SETTLE_MS: i32 = 150;

/// How long a pane whose shell was reaped waits for its output to close
/// before reporting the exit anyway: on Linux a background job the shell
/// left running keeps the pty open (see [`wait_pty_readable`]).
const EXIT_OUTPUT_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// What [`poll_pty`] saw.
#[derive(Debug, PartialEq, Eq)]
enum PtyWait {
    /// The pty has output.
    Ready,
    /// Shutdown was signalled, or polling failed.
    Closed,
    /// `timeout_ms` passed with neither.
    Idle,
}

/// [`wait_pty_readable`] with a timeout in milliseconds (`-1` waits forever).
fn poll_pty(
    pty_fd: std::os::fd::RawFd,
    shutdown: &std::io::PipeReader,
    timeout_ms: i32,
) -> PtyWait {
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
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return PtyWait::Closed;
        }
        if n == 0 {
            return PtyWait::Idle;
        }
        if fds[1].revents != 0 {
            return PtyWait::Closed;
        }
        if fds[0].revents != 0 {
            return PtyWait::Ready;
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
        //
        // A hosted pane (#694) asks its host to end the shell the same way:
        // closing a pane or quitting ends it, and only a croft that dies
        // without dropping its panes leaves hosts to reattach to.
        self.backend.end();
        // Free the rewind buffer now and hand its share of the budget back
        // to the other panes (#694). Not left to the Arc: the reader thread
        // holds a clone until it is joined below.
        crate::rewind::budget().release(&self.rewind);
        // Wake the reader (POLLHUP on its shutdown fd) and join it. EOF alone
        // is not a reliable wake: see `wait_pty_readable`.
        drop(self.reader_shutdown.take());
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

/// The logical line grid row `line_idx` belongs to (its soft-wrapped
/// neighbours joined, at most 64 rows either way) and the char offset of
/// that row's text within it, both in `row_text_and_cols` terms.
pub fn logical_row_text(term: &Term<VoidListener>, line_idx: i32) -> (String, usize) {
    let (joined, offset, _start) = logical_row_span(term, line_idx);
    (joined, offset)
}

/// [`logical_row_text`] plus the absolute grid line the logical line starts
/// on, which names a find match (see [`FindLine::start`]).
pub fn logical_row_span(term: &Term<VoidListener>, line_idx: i32) -> (String, usize, i32) {
    const REACH: i32 = 64;
    let top = term.grid().topmost_line().0;
    let bottom = term.screen_lines() as i32 - 1;
    let mut start = line_idx;
    while start > top && line_idx - start < REACH && row_wraps(term, start - 1) {
        start -= 1;
    }
    let mut end = line_idx;
    while end < bottom && end - line_idx < REACH && row_wraps(term, end) {
        end += 1;
    }
    let mut joined = String::new();
    let mut offset = 0;
    for l in start..=end {
        let (s, _) = row_text_and_cols(term, l);
        if l == line_idx {
            offset = joined.chars().count();
        }
        joined.push_str(&s);
    }
    (joined, offset, start)
}

/// Every grid row from the top of the scrollback, with its soft-wrap flag
/// and the absolute line of row 0: see [`PtyTerminal::grid_lines_wrapped`].
fn grid_rows_wrapped(term: &Term<VoidListener>) -> (Vec<String>, Vec<bool>, i32) {
    if term.columns() == 0 {
        return (Vec::new(), Vec::new(), 0);
    }
    let top = term.grid().topmost_line().0;
    let bottom = term.screen_lines() as i32 - 1;
    let mut lines = Vec::new();
    let mut wraps = Vec::new();
    let mut l = top;
    while l <= bottom {
        let (s, _cols) = row_text_and_cols(term, l);
        // A wrapped row keeps its trailing blanks: a space in its last
        // column separates words of the logical line (`Bearer ` then the
        // token), and trimming it glued them so a redact rule missed.
        let wraps_on = l < bottom && row_wraps(term, l);
        lines.push(if wraps_on {
            s
        } else {
            s.trim_end().to_string()
        });
        wraps.push(wraps_on);
        l += 1;
    }
    (lines, wraps, top)
}

/// One logical line of terminal output as find sees it: a run of rows the
/// pane soft-wrapped, joined back into the text that was printed (#1275).
/// A match position is a char index into `text`; `start` is the absolute
/// grid line of its first row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindLine {
    pub text: String,
    pub start: i32,
    /// Chars per row, in order, so a position maps back to its row.
    pub row_lens: Vec<usize>,
}

impl FindLine {
    /// The absolute grid line holding char `col` of this logical line.
    pub fn line_of(&self, col: usize) -> i32 {
        let mut seen = 0;
        for (i, len) in self.row_lens.iter().enumerate() {
            seen += len;
            if col < seen {
                return self.start + i as i32;
            }
        }
        self.start + self.row_lens.len().saturating_sub(1) as i32
    }
}

/// Join each run of rows whose wrap flag is set into one [`FindLine`]. A
/// row without the flag ends its line, so rows that merely sit next to each
/// other are never searched as one.
pub fn join_wrapped_rows(lines: Vec<String>, wraps: &[bool], top: i32) -> Vec<FindLine> {
    let mut out: Vec<FindLine> = Vec::new();
    let mut continues = false;
    for (i, row) in lines.into_iter().enumerate() {
        let len = row.chars().count();
        match out.last_mut() {
            Some(line) if continues => {
                line.text.push_str(&row);
                line.row_lens.push(len);
            }
            _ => out.push(FindLine {
                text: row,
                start: top + i as i32,
                row_lens: vec![len],
            }),
        }
        continues = wraps.get(i).copied().unwrap_or(false);
    }
    out
}

/// Grid rows from `top` to the bottom of the screen as `(plain, coloured,
/// wraps)`: see [`PtyTerminal::grid_lines_ansi_wrapped`], which reads from
/// the top of the scrollback, and [`PtyTerminal::screen_ansi_wrapped`],
/// which reads the visible screen only.
fn ansi_rows(term: &Term<VoidListener>, top: i32) -> Vec<(String, String, bool)> {
    let bottom = term.screen_lines() as i32 - 1;
    let ncols = term.columns();
    let mut lines = Vec::new();
    let mut l = top;
    while l <= bottom {
        // Cells are collected as (char, style key) - three Copy values,
        // no allocation per cell - so trailing default-styled blanks can
        // be trimmed before anything is serialised, and the SGR string
        // is built once per run of equal style rather than once per
        // cell. This runs under the term lock the reader thread shares,
        // and a saturated scrollback is hundreds of thousands of rows.
        let mut cells: Vec<(char, (AnsiColor, AnsiColor, Flags))> = Vec::with_capacity(ncols);
        for c in 0..ncols {
            let cell = &term.grid()[Point::new(Line(l), Column(c))];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let ch = cell_glyph(cell.c);
            // Only the flags an SGR carries: the soft-wrap mark on a row's
            // last cell and a wide char's own flag are not styles, and keyed
            // on them a run of one colour was reset and set again around them
            // (a cast wrote `w\e[0m\e[33mr` at the wrap column, #356).
            cells.push((ch, (cell.fg, cell.bg, cell.flags & SGR_FLAGS)));
        }
        let wraps_on = l < bottom && row_wraps(term, l);
        let plain: String = cells.iter().map(|(ch, _)| *ch).collect::<String>();
        // Trailing blanks trimmed only where the row does not wrap (see
        // `grid_lines_wrapped`).
        let plain = if wraps_on {
            plain
        } else {
            plain.trim_end().to_string()
        };
        while cells
            .last()
            .is_some_and(|(ch, (fg, bg, flags))| *ch == ' ' && style_is_default(*fg, *bg, *flags))
        {
            cells.pop();
        }
        let mut out = String::new();
        let mut current: Option<(AnsiColor, AnsiColor, Flags)> = None;
        let mut styled = false;
        for (ch, key) in cells {
            if current != Some(key) {
                if styled {
                    out.push_str("\x1b[0m");
                }
                let sgr = cell_sgr(key.0, key.1, key.2);
                styled = !sgr.is_empty();
                out.push_str(&sgr);
                current = Some(key);
            }
            out.push(ch);
        }
        if styled {
            out.push_str("\x1b[0m");
        }
        lines.push((plain, out, wraps_on));
        l += 1;
    }
    lines
}

/// [`ansi_rows`] for another module's model of a pane (#694's pane host).
pub(crate) fn ansi_rows_from(term: &Term<VoidListener>, top: i32) -> Vec<(String, String, bool)> {
    ansi_rows(term, top)
}

/// A `Flags` bit alacritty leaves unused (its own stop at bit 14), set on
/// every cell of a row [`resize_term`] clipped. Writing a cell replaces its
/// flags, so a row whose shown cells all still carry it is one no program
/// has written to since: the cells kept past its edge still belong to it.
const CLIPPED: Flags = Flags::from_bits_retain(0x8000);

/// Resize a pane's model, clipping rows rather than re-wrapping them (#1857).
///
/// alacritty reflows every row wider than a narrowed pane onto the row
/// below. Anything a program laid out to the old width falls apart: each
/// row of a Rich panel or a table splits in two, its right border lands
/// halfway along the next row, and a program that redraws by counting rows
/// up from the cursor misses the extra ones. So a row the program ended
/// itself (no soft wrap into it or out of it) is cut at the new width
/// instead. The cells cut off are kept past the row's last shown column,
/// where nothing draws or writes, and widening shows them again. A row a
/// program wrote to while cut drops them, since they no longer belong to
/// what it shows. Soft-wrapped lines still reflow, and so does the cursor's
/// row, as the shell redrawing a prompt expects. The primary screen is out
/// of reach while a full-screen program holds the alternate one, so a
/// resize then reflows it as before.
pub(crate) fn resize_term(term: &mut Term<VoidListener>, size: TermSize) {
    let (old, new) = (term.columns(), size.columns());
    if old == new || term.mode().contains(TermMode::ALT_SCREEN) {
        term.resize(size);
        return;
    }
    let grid = term.grid_mut();
    let cursor = grid.cursor.point.line.0;
    // A shorter pane drops the screen rows from here down (alacritty keeps
    // the cursor's row on it); a tail queued for one would go on another.
    let kept = (size.screen_lines() as i32).max(cursor + 1);
    let mut tails = Vec::new();
    let mut wrapped_into = false;
    for l in grid.topmost_line().0..=grid.bottommost_line().0 {
        let row = &mut grid[Line(l)];
        let restored = unclip_row(row, old);
        let wraps = !restored && row[Column(old - 1)].flags.contains(Flags::WRAPLINE);
        let overflows = row[..].iter().skip(new).any(|c| !c.is_empty());
        if overflows && l < kept && !wrapped_into && !wraps && (new > old || l != cursor) {
            let mut tail = clip_row(row, new);
            if new > old {
                // alacritty only grows a row narrower than the pane.
                row.append(&mut tail);
            } else {
                // Narrowing would reflow the tail; it goes back on after.
                tails.push(tail);
            }
        }
        wrapped_into = wraps;
    }
    term.resize(size);
    // The rows just clipped are the only marked ones, still in order, and
    // the oldest are the ones a full scrollback drops: match from the bottom.
    let grid = term.grid_mut();
    let mut tails = tails.into_iter().rev();
    for l in (grid.topmost_line().0..=grid.bottommost_line().0).rev() {
        let row = &mut grid[Line(l)];
        if row[Column(0)].flags.contains(CLIPPED)
            && let Some(mut tail) = tails.next()
        {
            row.append(&mut tail);
        }
    }
}

/// Cut `row` to show `cols` cells, marking them and returning the rest
/// (trailing blanks dropped), marked too. A wide char the edge would split
/// goes whole into the rest, leaving alacritty's placeholder for a wide
/// char moved on to the next row.
fn clip_row(row: &mut Row<Cell>, cols: usize) -> Vec<Cell> {
    let mut cells = row[..].to_vec();
    let end = cells
        .iter()
        .rposition(|c| !c.is_empty())
        .map_or(0, |i| i + 1);
    cells.truncate(end);
    let cut = if cells[cols - 1].flags.contains(Flags::WIDE_CHAR) {
        cols - 1
    } else {
        cols
    };
    let mut tail = cells.split_off(cut);
    if cut < cols {
        cells.push(Cell {
            flags: Flags::LEADING_WIDE_CHAR_SPACER,
            ..Cell::default()
        });
    }
    for cell in cells.iter_mut().chain(&mut tail) {
        cell.flags.insert(CLIPPED);
    }
    *row = Row::from_vec(cells, cols);
    tail
}

/// Undo [`clip_row`] on a row showing `cols` cells: put back what it kept
/// past its edge, or drop that when a program has written to the row since.
/// Whether the row came back whole.
fn unclip_row(row: &mut Row<Cell>, cols: usize) -> bool {
    if row.len() <= cols {
        // A clipped row alacritty reflowed under a full-screen program.
        if row[Column(0)].flags.contains(CLIPPED) {
            for cell in &mut row[..] {
                cell.flags.remove(CLIPPED);
            }
        }
        return false;
    }
    let intact = row[..Column(cols)]
        .iter()
        .all(|c| c.flags.contains(CLIPPED));
    let mut cells = row[..].to_vec();
    if !intact {
        cells.truncate(cols);
    } else if cells[cols - 1]
        .flags
        .contains(Flags::LEADING_WIDE_CHAR_SPACER)
    {
        cells.remove(cols - 1);
    }
    for cell in &mut cells {
        cell.flags.remove(CLIPPED);
    }
    let len = cells.len();
    *row = Row::from_vec(cells, len);
    intact
}

/// Whether grid row `line_idx` soft-wraps into the next (WRAPLINE on its
/// last cell).
pub fn row_wraps(term: &Term<VoidListener>, line_idx: i32) -> bool {
    let cols = term.columns();
    cols > 0
        && term.grid()[Point::new(Line(line_idx), Column(cols - 1))]
            .flags
            .contains(Flags::WRAPLINE)
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
/// What a grid cell shows: a blank for an empty cell and for a control
/// char. alacritty leaves `'\t'` in the cell a TAB started from so copy
/// can give the tab back, but painted raw the host terminal jumps to its
/// own tab stop while ratatui counts one cell, and the rest of the row
/// lands too far right over the previous frame's text (#1287).
fn cell_glyph(c: char) -> char {
    if c == '\0' || c.is_control() { ' ' } else { c }
}

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
        s.push(cell_glyph(cell.c));
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
    without_continuation_prompts(&extract_selection_text(
        term,
        bl,
        b.col_rec,
        cl - 1,
        term.columns().saturating_sub(1),
    ))
}

/// Typed input read off the screen, minus the shell's continuation prompt
/// on each line after the first (#1250). Only a hard newline starts such a
/// line, so a soft-wrapped row is never touched. The prompt is bash's `> `
/// or zsh's `%_> ` (`for> `, `cmdsubst dquote> `); stored as input, a
/// recalled command would run it as a redirect.
fn without_continuation_prompts(text: &str) -> String {
    let strip = |line: &str| -> String {
        let is_ps2 = |head: &str| {
            !head.starts_with(' ') && head.chars().all(|c| c.is_ascii_lowercase() || c == ' ')
        };
        if let Some(i) = line.find("> ")
            && is_ps2(&line[..i])
        {
            return line[i + 2..].to_string();
        }
        match line.strip_suffix('>') {
            Some(head) if is_ps2(head) => String::new(),
            _ => line.to_string(),
        }
    };
    let mut lines = text.split('\n');
    let mut out = lines.next().unwrap_or_default().to_string();
    for line in lines {
        out.push('\n');
        out.push_str(&strip(line));
    }
    out
}

/// The finished command's OUTPUT: the rows from `start`, its first output
/// row in current grid coordinates, to the row the cursor sits on as its
/// `133;D` arrives. Escape-free (the grid never stores escapes), tail-capped
/// at [`FINISHED_OUTPUT_CAP_LINES`].
fn command_output_text(term: &Term<VoidListener>, start: i32) -> String {
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

/// What croft types ahead of a command it puts into a pane's shell, so a
/// half-typed command at the prompt cannot concatenate with it: Ctrl-E to
/// the end of the line, a letter, then Ctrl-U to kill the line backward.
/// The letter keeps the line from being empty when Ctrl-U runs: bash's line
/// editor rings the bell for Ctrl-U on an empty line, and croft reported
/// that bell over the "Running …" status of the task it had just typed. A
/// letter, not a space: a space expands a fish abbreviation, and one set up
/// with `--set-cursor` leaves the cursor before its own suffix, which
/// Ctrl-U then keeps and croft's command runs into. No shell expands
/// anything on a letter. Before the shell's line editor is up, the
/// terminal's own line discipline takes Ctrl-U as its kill character, to
/// the same effect.
pub const CLEAR_TYPED_LINE: &str = "\x05x\x15";

/// Build the byte sequence that retargets a POSIX shell's working
/// directory to `path`. Prefixed with [`CLEAR_TYPED_LINE`] so a
/// half-typed command at the prompt does not concatenate with our `cd`.
/// Path is single-quoted with embedded
/// quotes escaped via the standard `'\''` sleight-of-hand so paths
/// containing spaces or apostrophes round-trip safely.
pub fn format_cd_command(path: &std::path::Path) -> Vec<u8> {
    let s = path.to_string_lossy();
    let mut out = Vec::with_capacity(s.len() + 8);
    out.extend_from_slice(CLEAR_TYPED_LINE.as_bytes());
    out.extend_from_slice(b"cd '");
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

/// The cell flags [`cell_sgr`] writes as SGR: everything else a cell carries
/// (wrap marks, wide-char spacers) is layout, not style.
const SGR_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::DOUBLE_UNDERLINE)
    .union(Flags::UNDERCURL)
    .union(Flags::DOTTED_UNDERLINE)
    .union(Flags::DASHED_UNDERLINE)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// The SGR sequence that reproduces a cell's colours and attributes, or an
/// empty string for a default cell (#257). Named and low-indexed colours go
/// out as the 16 standard codes so the reader resolves them through ITS
/// theme palette, the way the pane does; the 256-cube and truecolour pass
/// through as `38;5;n` / `38;2;r;g;b`.
fn cell_sgr(fg: AnsiColor, bg: AnsiColor, flags: Flags) -> String {
    let mut codes: Vec<String> = Vec::new();
    if flags.contains(Flags::BOLD) {
        codes.push("1".into());
    }
    if flags.contains(Flags::DIM) {
        codes.push("2".into());
    }
    if flags.contains(Flags::ITALIC) {
        codes.push("3".into());
    }
    if flags.intersects(
        Flags::UNDERLINE
            | Flags::DOUBLE_UNDERLINE
            | Flags::UNDERCURL
            | Flags::DOTTED_UNDERLINE
            | Flags::DASHED_UNDERLINE,
    ) {
        codes.push("4".into());
    }
    if flags.contains(Flags::INVERSE) {
        codes.push("7".into());
    }
    if flags.contains(Flags::HIDDEN) {
        codes.push("8".into());
    }
    if flags.contains(Flags::STRIKEOUT) {
        codes.push("9".into());
    }
    if let Some(code) = color_sgr(fg, false) {
        codes.push(code);
    }
    if let Some(code) = color_sgr(bg, true) {
        codes.push(code);
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// Whether a cell carries no colour and no attribute the export would
/// write: the cheap test the trailing-blank trim uses, with no SGR string
/// built to find out.
fn style_is_default(fg: AnsiColor, bg: AnsiColor, flags: Flags) -> bool {
    let plain = |c: AnsiColor| matches!(c, AnsiColor::Named(n) if named_slot(n).is_none());
    plain(fg)
        && plain(bg)
        && !flags.intersects(
            Flags::BOLD
                | Flags::DIM
                | Flags::ITALIC
                | Flags::UNDERLINE
                | Flags::DOUBLE_UNDERLINE
                | Flags::UNDERCURL
                | Flags::DOTTED_UNDERLINE
                | Flags::DASHED_UNDERLINE
                | Flags::INVERSE
                | Flags::HIDDEN
                | Flags::STRIKEOUT,
        )
}

/// One colour as its SGR parameter(s): `30..37`/`90..97` (or the `40..`
/// / `100..` background forms) for the 16 slots, `38;5;n` beyond them,
/// `38;2;r;g;b` for truecolour, `None` for the default colour.
fn color_sgr(c: AnsiColor, background: bool) -> Option<String> {
    let base = if background { 40 } else { 30 };
    let bright = if background { 100 } else { 90 };
    let extended = if background { 48 } else { 38 };
    let slot = |i: u8| {
        if i < 8 {
            (base + i as u16).to_string()
        } else {
            (bright + (i - 8) as u16).to_string()
        }
    };
    match c {
        AnsiColor::Spec(rgb) => Some(format!("{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b)),
        AnsiColor::Indexed(i) if i < 16 => Some(slot(i)),
        AnsiColor::Indexed(i) => Some(format!("{extended};5;{i}")),
        AnsiColor::Named(named) => named_slot(named).map(slot),
    }
}

/// The 16-slot index a named colour maps to, mirroring `named_to_ratatui`;
/// the terminal's own defaults (`Foreground`, `Background`, cursor colours)
/// have no slot and stay default.
fn named_slot(n: NamedColor) -> Option<u8> {
    use NamedColor::*;
    Some(match n {
        Foreground | Background | Cursor | DimForeground | BrightForeground => return None,
        Black | DimBlack => 0,
        Red | DimRed => 1,
        Green | DimGreen => 2,
        Yellow | DimYellow => 3,
        Blue | DimBlue => 4,
        Magenta | DimMagenta => 5,
        Cyan | DimCyan => 6,
        White | DimWhite => 7,
        BrightBlack => 8,
        BrightRed => 9,
        BrightGreen => 10,
        BrightYellow => 11,
        BrightBlue => 12,
        BrightMagenta => 13,
        BrightCyan => 14,
        BrightWhite => 15,
    })
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

impl PtyTerminal {
    /// The ink of a command that went well (a finished command's dot, a
    /// normal OSC 9;4 gauge): VS Code's blue on a boxed theme; on Croft Loch
    /// the accent eased a quarter of the way into the canvas, so success
    /// reads as on-palette and quieter than a failure's red.
    fn quiet_ok_ink(&self) -> Color {
        match crate::widgets::surface::tones(self.theme) {
            Some(t) => {
                crate::gradient::rgb_color(crate::gradient::lerp_rgb(t.accent, t.canvas, 0.25))
            }
            None => self.theme.ui(Color::Rgb(0x1b, 0x81, 0xa8)),
        }
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
        //
        // The focused pane draws its box in heavy line-drawing glyphs
        // (┏━┓) against the unfocused pane's light ones (┌─┐), so which
        // pane has the keyboard reads at a glance and not only from the
        // border's colour (#470).
        //
        // Weight rather than a second ring: the issue asks for "twice as
        // thick", and a terminal cell is the smallest unit there is, so a
        // literally doubled border means a second box one cell inside the
        // first -- costing the shell a row and a column on every side of
        // every pane. Heavy glyphs give the emphasis for free, which is
        // what iTerm2 and WezTerm do for the same reason.
        //
        // Croft Loch draws no box: the frame is padding on the canvas, every
        // pane's top edge is a hairline under the tab strip, and the focused
        // pane's carries the accent lip. A host accent keeps a full box
        // there too, because a production pane must read as dangerous at a
        // glance.
        let surfaces = self.theme.surfaces().is_some() && self.accent.is_none();
        let block = if surfaces {
            crate::widgets::surface::pane_block(self.theme.editor_bg())
        } else {
            Block::default()
                .borders(Borders::ALL)
                .border_type(if self.focused {
                    ratatui::widgets::BorderType::Thick
                } else {
                    ratatui::widgets::BorderType::Plain
                })
                .border_style(block_style)
        };
        let inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the orange→green
        // gradient (matching the welcome activity box).
        if surfaces {
            crate::widgets::surface::paint_pane_top(buf, area, self.theme, self.focused);
        } else if self.focused && self.focus_gradient && self.accent.is_none() {
            crate::gradient::paint_gradient_box_heavy(buf, area);
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
        // Croft Loch's inks and fills, looked up once per frame for the find
        // highlight's amber tint rather than once per cell.
        let tones = crate::widgets::surface::tones(self.theme);
        for y in 0..rows {
            // Find matches on this row once, then paint them per cell below.
            // Match positions are char indices in the spacer-skipped row text
            // (the same text the find bar searched); the colmap translates
            // them back to grid columns. 0 = no match, 1 = match, 2 = active.
            let row_line_idx = (y as i32) - (display_offset as i32);
            //
            // Matches are found on the logical line, as the find bar counts
            // them, and each paints the part that falls on this row: a match
            // the soft wrap cut in two lights up on both rows (#1275).
            let row_paint: Option<Vec<u8>> = self.search_needle.as_deref().map(|needle| {
                let (text, colmap) = row_text_and_cols(&term, row_line_idx);
                let (logical, offset, start) = logical_row_span(&term, row_line_idx);
                let row_len = text.chars().count();
                let mut paint = vec![0u8; cols as usize];
                for (mc, ml) in
                    crate::widgets::editor_find::line_matches(&logical, self.search_opts, needle)
                {
                    let (lo, hi) = (mc.max(offset), (mc + ml).min(offset + row_len));
                    if lo >= hi {
                        continue;
                    }
                    let active = active_match == Some((start, mc, ml));
                    for k in lo - offset..hi - offset {
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
                    // Over the logical line: a secret the soft wrap cut in
                    // two matches on neither row alone.
                    let (logical, offset) = logical_row_text(&term, row_line_idx);
                    let row_len = text.chars().count();
                    for s in crate::triggers::redact_spans(&logical, &trigger_set) {
                        let (lo, hi) =
                            (s.start.max(offset), (s.start + s.len).min(offset + row_len));
                        if lo >= hi {
                            continue;
                        }
                        // Counted on the row it starts on, once.
                        if s.start >= offset {
                            redacted_spans += 1;
                        }
                        for k in lo - offset..hi - offset {
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
                let mut display_char = cell_glyph(cell.c);
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
                // SGR 9: coding agents strike through finished todo items.
                if flags.contains(Flags::STRIKEOUT) {
                    style = style.add_modifier(Modifier::CROSSED_OUT);
                }
                // SGR 8 (conceal) paints a blank, not just HIDDEN, so the
                // text stays hidden on a host terminal that ignores SGR 8.
                // The grid keeps the real text for copy, as in xterm.
                if flags.contains(Flags::HIDDEN) {
                    style = style.add_modifier(Modifier::HIDDEN);
                    display_char = ' ';
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
                // orange on the active match (VS Code's find colours). Croft
                // Loch tints the cell's own background amber instead, as the
                // editor does, keeping the program's ink where it reads.
                if let Some(paint) = row_paint.as_ref() {
                    match (paint.get(x as usize), tones) {
                        (Some(&mark @ (1 | 2)), Some(t)) => {
                            style = crate::widgets::surface::match_style(
                                style,
                                t,
                                self.theme,
                                mark == 2,
                            );
                        }
                        (Some(1), None) => {
                            style = style
                                .fg(Color::Black)
                                .bg(self.theme.ui(Color::Rgb(0xff, 0xd7, 0x4a)))
                                .add_modifier(Modifier::BOLD);
                        }
                        (Some(2), None) => {
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
                _ => self.quiet_ok_ink(),
            };
            let by = area.y + area.height - 1;
            // The fill glyph has to be HEAVIER than the border it sits on,
            // or the gauge reads as full (#470): a focused pane's border is
            // already `━`, so painting `━` over it leaves only colour to
            // distinguish filled from unfilled -- and the focused pane is
            // the one most likely to be running the build being measured.
            // `═` against a heavy border is the same trick `━` plays
            // against a light one.
            // Croft Loch has no border to outweigh: a quarter block sits on
            // the frame row's floor, heavier than the hairline seam there.
            let fill_glyph = if surfaces {
                "\u{2582}"
            } else if self.focused {
                "═"
            } else {
                "━"
            };
            for i in 0..fill_len.min(w) {
                let x = inner.x + (fill_from + i) as u16;
                let cell = &mut buf[(x, by)];
                cell.set_symbol(fill_glyph);
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
                // Croft Loch pins the command on the raised surface in body
                // text, the scroll depth muted: a quiet card, not a banner.
                let tones = crate::widgets::surface::tones(self.theme);
                let bg = tones.map_or(self.theme.ui(Color::Rgb(0x25, 0x2b, 0x36)), |t| t.raised());
                let command_style = match tones {
                    Some(t) => Style::default().fg(t.text()).bg(bg),
                    None => Style::default()
                        .fg(self.theme.ui(Color::Rgb(0xec, 0xf0, 0xf4)))
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                };
                let depth_fg =
                    tones.map_or(self.theme.ui(Color::Rgb(0x8b, 0x93, 0xa1)), |t| t.muted());
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
                    cell.set_style(command_style);
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
                        cell.set_style(Style::default().fg(depth_fg).bg(bg));
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
                        self.quiet_ok_ink()
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

#[cfg(test)]
mod tests;
