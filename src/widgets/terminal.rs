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

mod annotations;
mod backend;
mod captures;
mod clock;
mod commands;
mod images;
mod input;
mod mouse;
mod overlays;
mod processes;
mod render;
mod scroll;
mod selection;
mod shell_env;
mod spawn;
mod status;
mod text;
use backend::*;
use captures::*;
use clock::*;
pub use commands::CommandDecoration;
pub use commands::PromptJump;
pub use commands::human_duration;
use commands::*;
pub use images::SIXEL_HOST;
use images::*;
pub use input::osc52_copy_seq;
pub use mouse::MouseAction;
pub use mouse::MouseButtonKind;
pub use mouse::MouseMods;
#[cfg(test)]
use mouse::*;
pub use overlays::HintSpan;
pub use processes::CLEAR_TYPED_LINE;
pub use processes::cwd_is_observable;
pub use processes::cwd_of_pid;
pub use processes::process_cmdline;
pub use processes::process_name;
use processes::*;
pub(crate) use scroll::resize_term;
use scroll::scroll_line_to_top;
#[cfg(test)]
use scroll::*;
pub use selection::Selection;
pub use selection::is_terminal_word_char;
use selection::*;
pub(crate) use shell_env::apply_pane_env;
pub use shell_env::parse_shells;
#[cfg(test)]
pub(crate) use shell_env::sequence_editor_command;
use shell_env::*;
#[cfg(test)]
use status::*;
pub use text::FindLine;
pub(crate) use text::ansi_rows_from;
use text::*;

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

/// One destructive clear [`WipeSniffer::scan`] found: the chunk offset just
/// past its last byte, and whether it erased the primary screen (ED 2),
/// the scrollback (ED 3) or both (RIS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Wipe {
    end: usize,
    screen: bool,
    hist: bool,
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
    /// at record time; [`Self::marks_snapshot`] translates to current grid
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

/// Milliseconds since the epoch, the clock `last_output_ms` keeps.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl PtyTerminal {
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

/// Per-cell trigger paint for one row: `None` = cell untouched, `Some` =
/// the matching trigger's colours (either side optional, leaving that half
/// of the cell style alone) and whether a redact rule masks the glyph.
#[derive(Clone, Copy, Debug, Default)]
struct TrigCell {
    fg: Option<Color>,
    bg: Option<Color>,
    mask: bool,
}

#[cfg(test)]
mod tests;
