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
    /// The extracted selection text at last sight: what copy yields while
    /// the content is scrolled out of view.
    text: String,
    /// The rows are nowhere in the grid right now: paint nothing, keep the
    /// coordinates frozen until the content reappears.
    dormant: bool,
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

/// One user note pinned to a span of terminal output (iTerm2's
/// annotations), anchored like a mark so it rides the scrollback.
#[derive(Clone, Debug)]
struct PaneAnnotation {
    line_rec: i32,
    hist_rec: usize,
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
    /// While broadcast input is on, an excluded pane stops receiving the
    /// mirrored keystrokes (it still gets its own input when focused).
    /// Toggled per pane with Cmd+K Shift+I; session-scoped.
    pub broadcast_excluded: bool,
    /// When focused, draw the orange→green gradient border (Black theme)
    /// instead of the solid blue one. Set by the app's focus/theme sync.
    pub focus_gradient: bool,
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
    /// Monotonic scroll clock, folded up to the last `tick_clock`.
    clock_base: i64,
    /// Grid line the tracer selection was planted at during the last tick;
    /// `None` before the first tick.
    clock_planted: Option<i32>,
    /// `history_size` at the last tick: fallback delta source for the rare
    /// windows where the tracer died (screen clear, alt-screen round trip).
    clock_hist: i64,
    /// Content anchor for a selection made on the alternate screen, where
    /// apps (Claude Code, vim) never scroll the grid — they repaint it in
    /// place, so no scroll clock can see the text move. `None` while the
    /// selection lives on the primary screen.
    alt_sel: Option<AltSelAnchor>,
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
    /// The active match `(abs_line, col, len)` painted in the brighter accent,
    /// versus the muted highlight on every other occurrence (VS Code's
    /// current-vs-other match colours).
    current_match: Option<(i32, usize, usize)>,
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
    /// User notes pinned to output spans (Cmd+K N on a selection).
    /// Session-scoped, like the scrollback they describe.
    annotations: Vec<PaneAnnotation>,
    /// Finished commands (exit, duration) from the reader thread, awaiting
    /// the app's drain for long-command notifications.
    finished_rx: std::sync::mpsc::Receiver<FinishedCommand>,
    /// Quick-select hint spans pushed down by the app while hint mode is
    /// active; the render loop paints the match spans and overlays each
    /// label. `None` when quick-select is off.
    hints: Option<Vec<HintSpan>>,
    /// The user's trigger set, shared with the reader thread (which scans
    /// completed lines for notify/bell firings) and read by the render loop
    /// (which paints highlight-trigger matches). The inner Arc is swapped by
    /// [`Self::set_triggers`] on startup and config reload.
    triggers: Arc<std::sync::Mutex<std::sync::Arc<crate::triggers::TriggerSet>>>,
    /// Notify/bell trigger firings from the reader thread, awaiting the
    /// app's drain into the status bar.
    trigger_rx: std::sync::mpsc::Receiver<crate::triggers::TriggerHit>,
    /// The theme's 16 ANSI colors; Named and Indexed 0-15 cell colors render
    /// through it so panes look the same on every host terminal (VS Code
    /// owns its terminal palette the same way). Synced by the app's theme
    /// pass via [`Self::set_palette`].
    palette: [(u8, u8, u8); 16],
    /// Inline images captured from the pane's output (iTerm2 OSC 1337
    /// `inline=1`, the imgcat protocol), anchored like marks. Capped at
    /// [`IMAGES_MAX`]; the app overlays the newest visible one.
    images: Arc<std::sync::Mutex<Vec<StoredImage>>>,
}

/// One captured inline image at its recording-time anchor (same drift model
/// as [`StoredMark`]: current line = `line_rec - (history_now - hist_rec)`).
struct StoredImage {
    seq: u64,
    data: std::sync::Arc<Vec<u8>>,
    line_rec: i32,
    hist_rec: usize,
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

/// One OSC 133 mark at its recording-time position. `line_rec` is the grid
/// line the cursor sat on (0-based live screen), `hist_rec` the scrollback
/// size at that moment; the current position is
/// `line_rec - (history_now - hist_rec)`.
///
/// Ceiling: once scrollback saturates (5000 lines), `history_size` stops
/// growing while content keeps scrolling, so surviving marks drift by the
/// evicted-line count. Marks that old point near-evicted content anyway
/// and are GC'd as they pass the scrollback floor.
struct StoredMark {
    kind: crate::shell_integration::OscEvent,
    line_rec: i32,
    hist_rec: usize,
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
}

/// A finished command derived from the OSC 133 marks: the grid line of the
/// prompt it was typed at (current coords, negative = scrollback), its exit
/// code (`None` when the shell omitted it), how long it ran, where its typed
/// text starts (`PromptEnd` line + column), and its output span
/// (`CommandStart` line up to but excluding the `CommandEnd` line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandDecoration {
    pub line: i32,
    pub exit: Option<i32>,
    pub duration: Option<std::time::Duration>,
    pub input: Option<(i32, usize)>,
    pub output_start: i32,
    pub output_end: i32,
}

/// Pair the mark stream (oldest first, each with its current grid line and,
/// for `CommandEnd`, the measured duration) into one record per *finished*
/// command: the last `PromptStart` line before a `CommandStart` names the
/// row, the following `CommandEnd` supplies exit + duration. A `CommandEnd`
/// with no pending `CommandStart` is dropped — that's how a second
/// integration layer's duplicate marks (Ghostty's hooks chained behind
/// croft's) stay out of the record.
pub fn pair_decorations(
    marks: &[(
        crate::shell_integration::OscEvent,
        i32,
        usize,
        Option<std::time::Duration>,
    )],
) -> Vec<CommandDecoration> {
    use crate::shell_integration::OscEvent as E;
    let mut out = Vec::new();
    let mut prompt: Option<i32> = None;
    let mut input: Option<(i32, usize)> = None;
    let mut started: Option<i32> = None;
    for (kind, line, col, dur) in marks {
        match kind {
            E::PromptStart => {
                prompt = Some(*line);
                input = None;
            }
            E::PromptEnd => input = Some((*line, *col)),
            E::CommandStart => started = Some(*line),
            E::CommandEnd(exit) => {
                if let Some(output_start) = started.take() {
                    out.push(CommandDecoration {
                        line: prompt.unwrap_or(output_start),
                        exit: *exit,
                        duration: *dur,
                        input,
                        output_start,
                        output_end: *line,
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

/// The label to show for a pane: a manual name wins, else the live foreground
/// process name.
pub fn pick_pane_label<'a>(manual: Option<&'a str>, auto: &'a str) -> &'a str {
    manual.unwrap_or(auto)
}

impl PtyTerminal {
    pub fn new(cwd: &std::path::Path) -> Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        Self::new_shell(&shell, cwd)
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
    /// render loop paints it in the brighter accent.
    pub fn set_current_match(&mut self, m: Option<(i32, usize, usize)>) {
        self.current_match = m;
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Set (or clear) the quick-select hint overlay. The app owns hint-mode
    /// state and re-pushes the filtered set as the user types label chars.
    pub fn set_hints(&mut self, hints: Option<Vec<HintSpan>>) {
        self.hints = hints;
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
        let term = self.term.lock();
        let hist_now = term.grid().history_size() as i32;
        let floor = term.grid().topmost_line().0;
        drop(term);
        let mut imgs = self.images.lock().unwrap();
        imgs.retain(|m| m.line_rec - (hist_now - m.hist_rec as i32) >= floor);
        imgs.iter()
            .map(|m| PaneImage {
                seq: m.seq,
                data: m.data.clone(),
                line: m.line_rec - (hist_now - m.hist_rec as i32),
            })
            .collect()
    }

    /// Whether the pane is in the alternate screen (a full-screen app owns
    /// the viewport; anchored overlays make no sense there).
    pub fn alt_screen(&self) -> bool {
        self.term.lock().mode().contains(TermMode::ALT_SCREEN)
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
        let osc7_host = Arc::new(std::sync::Mutex::new(None::<String>));
        let osc7_host_for_thread = osc7_host.clone();
        // Trigger set shared with the reader thread; the inner Arc is swapped
        // by `set_triggers` on startup / config reload, picked up per chunk.
        let triggers = Arc::new(std::sync::Mutex::new(std::sync::Arc::new(
            crate::triggers::TriggerSet::default(),
        )));
        let triggers_for_thread = triggers.clone();
        let (trigger_tx, trigger_rx) = std::sync::mpsc::channel::<crate::triggers::TriggerHit>();

        let reader_thread = std::thread::spawn(move || {
            let mut processor = Processor::<StdSyncHandler>::new();
            let mut port_sniffer = crate::port_detect::PortSniffer::new();
            let mut osc_sniffer = crate::shell_integration::OscSniffer::default();
            let mut trigger_scanner = crate::triggers::TriggerScanner::new();
            let mut trigger_hits = Vec::new();
            // Per-pane monotonic id for captured inline images; the overlay
            // layout key uses it to tell a new picture from a moved one.
            let mut image_seq = 0u64;
            // Command timing: armed by 133;C, consumed by the next 133;D.
            // Cursor's absolute row after the previous chunk: the rows this
            // chunk touched run from there to the cursor's new row, and each
            // takes the chunk's arrival time (last touch wins, so a row is
            // stamped when its content actually landed, not when the cursor
            // first parked on it).
            let mut prev_cursor_abs: i64 = 0;
            let mut cmd_start: Option<std::time::Instant> = None;
            let mut buf = [0u8; 65536];
            loop {
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
                                    // position, exactly like a mark; the
                                    // overlay derives the current line via
                                    // the same drift model.
                                    let line_rec = t.grid().cursor.point.line.0;
                                    let hist_rec = t.grid().history_size();
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
                                        hist_rec,
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
                                                let cmd = {
                                                    let ms = marks_for_thread.lock().unwrap();
                                                    last_command_input_text(&t, &ms)
                                                };
                                                let cwd = osc7_for_thread.lock().unwrap().clone();
                                                let _ = finished_tx.send(FinishedCommand {
                                                    exit: *exit,
                                                    dur: d,
                                                    cmd,
                                                    cwd,
                                                });
                                            }
                                            dur
                                        }
                                        _ => None,
                                    };
                                    let line_rec = t.grid().cursor.point.line.0;
                                    let hist_rec = t.grid().history_size();
                                    let col_rec = t.grid().cursor.point.column.0;
                                    let mut ms = marks_for_thread.lock().unwrap();
                                    if ms.len() >= MARKS_MAX {
                                        let drop_n = ms.len() + 1 - MARKS_MAX;
                                        ms.drain(..drop_n);
                                    }
                                    ms.push(StoredMark {
                                        kind,
                                        line_rec,
                                        hist_rec,
                                        col_rec,
                                        dur,
                                    });
                                }
                            }
                        }
                        processor.advance(&mut *t, &buf[done..n]);
                        // Stamp newly-arrived rows for the timestamps gutter:
                        // every row the cursor moved past in this chunk gets
                        // the chunk's arrival time (one read of output is
                        // sub-millisecond, so chunk granularity is honest).
                        {
                            let hist = t.grid().history_size() as i64;
                            let cur_abs = hist + t.grid().cursor.point.line.0 as i64;
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let mut lt = line_times_for_thread.lock().unwrap();
                            if cur_abs < prev_cursor_abs {
                                // ED 3 wiped the scrollback: history reset, so
                                // every stored id belongs to a dead row.
                                lt.clear();
                                prev_cursor_abs = cur_abs;
                            }
                            for a in prev_cursor_abs..=cur_abs {
                                lt.insert(a, now_ms);
                            }
                            prev_cursor_abs = cur_abs;
                            // Bounded by the largest configurable scrollback
                            // plus a screen; oldest stamps go first.
                            while lt.len() > 210_000 {
                                lt.pop_first();
                            }
                        }
                        // Notify/bell triggers match completed output lines,
                        // never inside a full-screen app (the alt screen owns
                        // the bytes; iTerm2 skips those too). Skipped
                        // entirely when no event trigger is configured.
                        let trig = triggers_for_thread.lock().unwrap().clone();
                        if trig.has_events() && !t.mode().contains(TermMode::ALT_SCREEN) {
                            trigger_scanner.scan(&buf[..n], &trig, &mut trigger_hits);
                            for h in trigger_hits.drain(..) {
                                let _ = trigger_tx.send(h);
                            }
                        }
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
            port_rx,
            master: pair.master,
            writer,
            _child: child,
            reader_thread: Some(reader_thread),
            shell_pid,
            cols,
            rows,
            size_shared,
            focused: false,
            broadcast_excluded: false,
            focus_gradient: false,
            last_area: Rect::default(),
            last_inner: Rect::default(),
            selection: None,
            copy_cursor: None,
            sel_scrolled: 0,
            clock_base: 0,
            clock_planted: None,
            clock_hist: 0,
            alt_sel: None,
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
            annotations: Vec::new(),
            finished_rx,
            hints: None,
            triggers,
            trigger_rx,
            palette: crate::theme::VSCODE_ANSI,
            images,
        })
    }

    /// OSC 133 marks with their *current* grid line (negative = scrollback),
    /// oldest first. Marks whose content scrolled past the scrollback floor
    /// are garbage-collected here.
    pub fn command_marks(&self) -> Vec<(crate::shell_integration::OscEvent, i32)> {
        self.marks_snapshot()
            .into_iter()
            .map(|(kind, line, _, _)| (kind, line))
            .collect()
    }

    /// The marks with current grid lines and, for `CommandEnd`, the measured
    /// command duration. GC + drift adjustment as in [`Self::command_marks`].
    fn marks_snapshot(
        &self,
    ) -> Vec<(
        crate::shell_integration::OscEvent,
        i32,
        usize,
        Option<std::time::Duration>,
    )> {
        let term = self.term.lock();
        let hist_now = term.grid().history_size() as i32;
        let floor = term.grid().topmost_line().0;
        drop(term);
        let mut marks = self.marks.lock().unwrap();
        marks.retain(|m| m.line_rec - (hist_now - m.hist_rec as i32) >= floor);
        marks
            .iter()
            .map(|m| {
                (
                    m.kind.clone(),
                    m.line_rec - (hist_now - m.hist_rec as i32),
                    m.col_rec,
                    m.dur,
                )
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

    /// The user's manual pane name, when one was set via rename (what the
    /// session snapshot persists; `label()` mixes in the auto label).
    pub fn manual_name(&self) -> Option<&str> {
        self.manual_name.as_deref()
    }

    /// Set or clear the user's manual pane name (a blank name clears it).
    pub fn set_manual_name(&mut self, name: Option<String>) {
        self.manual_name = name.filter(|n| !n.trim().is_empty());
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
        let hist_now = term.grid().history_size() as i32;
        let ms = self.marks.lock().unwrap();
        let last = ms.last()?;
        if !matches!(last.kind, crate::shell_integration::OscEvent::PromptEnd) {
            return None;
        }
        if last.line_rec - (hist_now - last.hist_rec as i32) != line {
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
        let target = vc.clamp(b_col, end_col.max(cur));
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

    /// Pin a note to the span starting at current grid `line`, columns
    /// `start..start+len` (Cmd+K N's commit).
    pub fn add_annotation(&mut self, line: i32, start: u16, len: u16, text: String) {
        let hist_rec = self.term.lock().grid().history_size();
        self.annotations.push(PaneAnnotation {
            line_rec: line,
            hist_rec,
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
        let term = self.term.lock();
        let hist_now = term.grid().history_size() as i32;
        let top = term.grid().topmost_line().0;
        drop(term);
        self.annotations
            .retain(|a| a.line_rec - (hist_now - a.hist_rec as i32) >= top);
        self.annotations
            .iter()
            .map(|a| {
                (
                    a.line_rec - (hist_now - a.hist_rec as i32),
                    a.start,
                    a.len,
                    a.text.clone(),
                )
            })
            .collect()
    }

    /// The annotation index + note under screen cell (col, row), if any.
    pub fn annotation_at(&self, col: u16, row: u16) -> Option<(usize, String)> {
        let (vr, vc) = self.cell_at(col, row)?;
        let term = self.term.lock();
        let off = term.grid().display_offset() as i32;
        let hist_now = term.grid().history_size() as i32;
        drop(term);
        let line = vr as i32 - off;
        self.annotations
            .iter()
            .enumerate()
            .find(|(_, a)| {
                a.line_rec - (hist_now - a.hist_rec as i32) == line
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
        let hist = self.term.lock().grid().history_size() as i64;
        self.line_times
            .lock()
            .unwrap()
            .get(&(line as i64 + hist))
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

    /// The monotonic scroll clock: lines the primary screen has scrolled
    /// since the pane spawned, read from the tracer's drift. Immune to the
    /// `history_size` saturation that froze selections in long-lived panes
    /// (the Claude Code drag-select bug: scrollback full for ages, so the
    /// old history-growth delta was pinned at zero while content kept
    /// rotating through the ring). Alt screen freezes the clock — output
    /// goes to the alternate grid while primary content holds still. A dead
    /// tracer falls back to history growth since the last tick: exact for
    /// the viewport-pushing clear that killed it, zero for an alt-screen
    /// round trip.
    fn clock_now(&self, term: &Term<VoidListener>) -> i64 {
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return self.clock_base;
        }
        match (self.clock_planted, Self::tracer_line(term)) {
            (Some(planted), Some(cur)) => self.clock_base + i64::from(planted - cur),
            _ => {
                let growth = term.grid().history_size() as i64 - self.clock_hist;
                self.clock_base + growth.max(0)
            }
        }
    }

    /// Fold the tracer's drift into the clock, re-plant it fresh and return
    /// the folded reading. The newest history line is the parking spot:
    /// application clears only touch live-screen rows, so nothing kills it
    /// there, and it sits a full scrollback's depth from rotating off the
    /// top before the next tick (every rendered frame ticks).
    fn tick_clock(&mut self) -> i64 {
        let mut term = self.term.lock();
        let now = self.clock_now(&term);
        if !term.mode().contains(TermMode::ALT_SCREEN) {
            self.clock_base = now;
            self.clock_hist = term.grid().history_size() as i64;
            let park = if self.clock_hist > 0 { -1 } else { 0 };
            Self::plant_tracer(&mut term, park);
            self.clock_planted = Some(park);
        }
        now
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
        let sel = self.selection.unwrap();
        let old_top = sel.anchor.0.min(sel.head.0);
        let k = anchor.rows.len() as i32;
        // One grid snapshot up front: the matcher probes every candidate
        // shift, so per-candidate row reads would re-extract (and
        // re-allocate) the same rows dozens of times a frame.
        let grid_rows: Vec<String> = (0..rows_vis)
            .map(|l| row_text_and_cols(&term, l).0)
            .collect();
        let matches_at = |top: i32| {
            let mut nonblank = false;
            for (i, want) in anchor.rows.iter().enumerate() {
                let line = top + i as i32;
                if line < 0 || line >= rows_vis {
                    continue;
                }
                if grid_rows[line as usize] != *want {
                    return false;
                }
                nonblank |= !want.trim().is_empty();
            }
            nonblank
        };
        match (1 - k..rows_vis)
            .filter(|&t| matches_at(t))
            .min_by_key(|&t| (t - old_top).abs())
        {
            Some(top) => {
                let d = top - old_top;
                if d != 0
                    && let Some(s) = self.selection.as_mut()
                {
                    s.anchor.0 += d;
                    s.head.0 += d;
                }
                anchor.dormant = false;
                // Refresh the anchor — this is also what folds in a user
                // extension of the selection — but only when the whole
                // block is in view, so the remembered rows never lose an
                // off-screen part mid-scroll.
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
            self.stamp_selection_clock();
        }
    }

    pub fn extend_selection_to(&mut self, col: u16, row: u16) {
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
        // A dormant alt-screen selection names content currently scrolled
        // out of the app's view: the grid no longer holds it, but the
        // anchor remembered the text, so copy still yields what was
        // highlighted.
        if let Some(anchor) = self.alt_sel.as_ref().filter(|a| a.dormant) {
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

    pub fn write_input(&mut self, data: &[u8]) {
        self.reset_scrollback();
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(data);
            let _ = w.flush();
        }
        self.pty_dirty.store(true, Ordering::Release);
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

    pub fn reset_scrollback(&mut self) {
        let mut term = self.term.lock();
        term.scroll_display(Scroll::Bottom);
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Clear the visible screen and scrollback history (VS Code's terminal
    /// "Clear"), homing the cursor. Feeds the standard erase sequences into the
    /// grid (`ED 3` wipes scrollback, `ED 2` the screen); the shell redraws its
    /// prompt on the next keystroke. Does not touch the running program.
    pub fn clear_screen_and_scrollback(&mut self) {
        let mut processor = Processor::<StdSyncHandler>::new();
        {
            let mut term = self.term.lock();
            processor.advance(&mut *term, b"\x1b[3J\x1b[2J\x1b[H");
            term.scroll_display(Scroll::Bottom);
        }
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

impl Drop for PtyTerminal {
    fn drop(&mut self) {
        // Kill the shell so its slave fd closes; the reader thread's blocked
        // `read` then returns EOF and the thread exits, which we join here.
        // Without this a dropped terminal (a closed pane, or every terminal
        // an App test creates) leaves a live shell process and a parked
        // reader thread behind. The responder thread ends on its own once
        // `self.term` (holding its channel sender) drops with this struct.
        let _ = self._child.kill();
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
fn last_command_input_text(term: &Term<VoidListener>, marks: &[StoredMark]) -> String {
    use crate::shell_integration::OscEvent as E;
    let hist_now = term.grid().history_size() as i32;
    let cur = |m: &StoredMark| m.line_rec - (hist_now - m.hist_rec as i32);
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

/// Epoch millis → local wall-clock `HH:MM:SS` (libc localtime, the same
/// no-date-crate route the trash metadata writer takes).
fn hhmmss(millis: u64) -> String {
    let secs = (millis / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return String::from("--:--:--");
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
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
    let text = if sel.block {
        let (rl, cl, rh, ch) = sel.block_bounds();
        block_selection_text(term, rl, cl as usize, rh, ch as usize)
    } else {
        let (sr, sc, er, ec) = sel.normalised();
        extract_selection_text(term, sr, sc as usize, er, ec as usize)
    };
    Some(AltSelAnchor {
        rows,
        text,
        dormant: false,
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
            Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))
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
        // app's view and the frozen coordinates sit over unrelated text.
        let dormant = self.alt_sel.as_ref().is_some_and(|a| a.dormant);
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
        // Annotation spans at their current grid lines (mark-style drift).
        let ann_hist = term.grid().history_size() as i32;
        let ann_spans: Vec<(i32, u16, u16)> = self
            .annotations
            .iter()
            .map(|a| (a.line_rec - (ann_hist - a.hist_rec as i32), a.start, a.len))
            .collect();
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
                    let active = self.current_match == Some((row_line_idx, mc, ml));
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
            let hint_paint: Option<(Vec<u8>, Vec<char>)> = self.hints.as_ref().and_then(|hints| {
                let row_hints: Vec<&HintSpan> =
                    hints.iter().filter(|h| h.line == row_line_idx).collect();
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
                            paint[col] = Some((fg, bg));
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
                    && let Some(Some((fg, bg))) = paint.get(x as usize)
                {
                    if let Some(c) = fg {
                        style = style.fg(*c);
                    }
                    if let Some(c) = bg {
                        style = style.bg(*c);
                    }
                }
                // Annotated spans: amber + underline, under the cursor /
                // selection / find layers so those still win.
                if ann_spans
                    .iter()
                    .any(|&(l, s0, ln)| l == line_idx && x >= s0 && x < s0 + ln)
                {
                    style = style
                        .fg(Color::Rgb(0xe5, 0xc0, 0x7b))
                        .add_modifier(Modifier::UNDERLINED);
                }
                if cursor_visible
                    && (y as i32) == cursor_row_in_viewport
                    && (x as i32) == cursor_col_in_viewport
                {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                if let Some((block, (sr, sc, er, ec))) = sel_paint
                    && if block {
                        cell_in_block_selection(line_idx, x, sr, sc, er, ec)
                    } else {
                        cell_in_selection(line_idx, x, sr, sc, er, ec)
                    }
                {
                    style = style.bg(Color::Rgb(0x26, 0x4f, 0x78));
                }
                // Find highlight: muted amber on every occurrence, bright
                // orange on the active match (VS Code's find colours).
                if let Some(paint) = row_paint.as_ref() {
                    match paint.get(x as usize) {
                        Some(1) => {
                            style = style
                                .fg(Color::Black)
                                .bg(Color::Rgb(0xff, 0xd7, 0x4a))
                                .add_modifier(Modifier::BOLD);
                        }
                        Some(2) => {
                            style = style
                                .fg(Color::Black)
                                .bg(Color::Rgb(0xff, 0x8c, 0x2a))
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
                                .fg(Color::Rgb(0x66, 0xcc, 0x66))
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
                                .bg(Color::Rgb(0xff, 0xd7, 0x4a))
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
                        .bg(Color::Rgb(0x66, 0xcc, 0x66))
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
                2 => Color::Rgb(0xf1, 0x4c, 0x4c),
                4 => Color::Rgb(0xe5, 0xc0, 0x7b),
                _ => Color::Rgb(0x1b, 0x81, 0xa8),
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
            let lt = self.line_times.lock().unwrap();
            let hist = term.grid().history_size() as i64;
            for y in 0..rows {
                let abs = (y as i32 - display_offset as i32) as i64 + hist;
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
                    Color::Rgb(0xe5, 0xc0, 0x7b)
                } else {
                    Color::Rgb(0x5b, 0x64, 0x72)
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
                    let hist_now = term.grid().history_size() as i32;
                    let last = ms.last().unwrap();
                    let c_line = last.line_rec - (hist_now - last.hist_rec as i32);
                    (c_line < top_row).then(|| last_command_input_text(&term, &ms))
                })
                .filter(|t| !t.trim().is_empty());
            if let Some(text) = header {
                let bg = Color::Rgb(0x25, 0x2b, 0x36);
                for x in 0..inner.width {
                    let cell = &mut buf[(inner.x + x, inner.y)];
                    cell.set_symbol(" ");
                    cell.set_style(Style::default().bg(bg));
                }
                let label = format!("\u{25b6} {}", text.trim());
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
                            .fg(Color::Rgb(0xec, 0xf0, 0xf4))
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
                        cell.set_style(Style::default().fg(Color::Rgb(0x8b, 0x93, 0xa1)).bg(bg));
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
                        Color::Rgb(0x1b, 0x81, 0xa8)
                    } else {
                        Color::Rgb(0xf1, 0x4c, 0x4c)
                    }));
                }
            }
        }
    }
}

/// Per-cell trigger-highlight paint for one row: `None` = cell untouched,
/// `Some((fg, bg))` = the matching trigger's colours (either side optional,
/// leaving that half of the cell style alone).
type TrigRowPaint = Vec<Option<(Option<Color>, Option<Color>)>>;

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
        term.add_annotation(line, 0, 16, String::from("this is where it broke"));
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
        let tmp = tempfile::tempdir().unwrap();
        let script = "a=first; echo ${a}-marker; sleep 0.4; b=second; echo ${b}-marker; sleep 30";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        let mut waited = 0u32;
        loop {
            let (lines, top) = term.grid_lines();
            let a = lines.iter().position(|l| l.starts_with("first-marker"));
            let b = lines.iter().position(|l| l.starts_with("second-marker"));
            if let (Some(a), Some(b)) = (a, b) {
                let ta = term
                    .row_time(top + a as i32)
                    .expect("the first line must be stamped");
                let tb = term
                    .row_time(top + b as i32)
                    .expect("the second line must be stamped");
                assert!(
                    tb.saturating_sub(ta) >= 300,
                    "stamps must reflect the 400ms gap between the lines, got {}ms",
                    tb.saturating_sub(ta)
                );
                break;
            }
            assert!(waited < 8000, "markers never arrived");
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
    fn clear_screen_and_scrollback_wipes_the_grid() {
        fn dump(term: &PtyTerminal) -> String {
            let t = term.term.lock();
            let rows = t.screen_lines();
            let cols = t.columns();
            let off = t.grid().display_offset() as i32;
            extract_selection_text(&t, -off, 0, rows as i32 - 1 - off, cols.saturating_sub(1))
        }
        let tmp = tempfile::tempdir().unwrap();
        let mut term =
            PtyTerminal::new_running("/bin/echo", &[String::from("CLEAR-PROBE-XYZ")], tmp.path())
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
        let lines = wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains("filler-39")));
        let (_, top) = term.grid_lines();
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

    #[test]
    fn poisoned_zdotdir_from_a_nested_croft_still_loads_the_user_rc() {
        // Regression: every pane exports ZDOTDIR=<shim>, so a croft launched
        // FROM a croft pane inherited it and treated the shim as the user's
        // dotfile dir — the shim then sourced itself in a recursion loop and
        // the user's real .zshrc (their theme, aliases) never ran. A
        // poisoned CROFT_USER_ZDOTDIR pointing at the shim must be ignored
        // in favour of $HOME.
        let zsh = "/bin/zsh";
        if !std::path::Path::new(zsh).exists() {
            return;
        }
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
            pair_decorations(&marks),
            vec![
                CommandDecoration {
                    line: -10,
                    exit: Some(0),
                    duration: Some(ms(2400)),
                    input: Some((-10, 5)),
                    output_start: -9,
                    output_end: -7,
                },
                CommandDecoration {
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
        assert_eq!(pair_decorations(&dup).len(), 1);
        // A command still running (no CommandEnd yet) has no record.
        let running = [(E::PromptStart, 0, 0, None), (E::CommandStart, 0, 0, None)];
        assert!(pair_decorations(&running).is_empty());
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
        let script = "printf '\\033]133;A\\007$ cmd\\n\\033]133;B\\007\\033]133;C\\007'; sleep 0.3; printf 'out\\n\\033]133;D;2\\007'";
        let term =
            PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
                .unwrap();
        let mut waited = 0u32;
        loop {
            let decos = term.command_decorations();
            if let Some(d) = decos.first() {
                assert_eq!(d.exit, Some(2), "exit code from 133;D;2");
                let dur = d.duration.expect("duration must be measured");
                assert!(
                    dur >= std::time::Duration::from_millis(250),
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
        assert!(finished[0].dur >= std::time::Duration::from_millis(250));
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
        let zsh = "/bin/zsh";
        if !std::path::Path::new(zsh).exists() {
            return;
        }
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
        let zsh = "/bin/zsh";
        if !std::path::Path::new(zsh).exists() {
            return; // no zsh on this machine; covered on macOS dev boxes
        }
        let user_dir = tempfile::tempdir().unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir.path()).unwrap();
        let mut cmd = CommandBuilder::new(zsh);
        cmd.arg("-i");
        cmd.cwd(user_dir.path());
        cmd.env("ZDOTDIR", &shim);
        cmd.env("CROFT_USER_ZDOTDIR", user_dir.path());
        let term = PtyTerminal::spawn_with(cmd, None).unwrap();
        let mut waited_ms = 0u32;
        while term.prompt_lines().is_empty() {
            assert!(
                waited_ms < 8000,
                "zsh never emitted a prompt mark; grid: {:?}",
                term.grid_lines().0
            );
            std::thread::sleep(std::time::Duration::from_millis(40));
            waited_ms += 40;
        }
        assert!(
            term.shell_cwd().is_some(),
            "the shim's precmd must also report the cwd via OSC 7"
        );
    }

    /// A bash new enough for `$ENV` + `--posix` injection (>= 4.4), for the
    /// e2e tests: Homebrew/Linux bash qualifies, macOS's system 3.2 not.
    fn modern_bash() -> Option<&'static str> {
        ["/opt/homebrew/bin/bash", "/usr/local/bin/bash", "/bin/bash"]
            .into_iter()
            .find(|b| bash_env_injection_supported(b))
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
        let zsh = "/bin/zsh";
        if !std::path::Path::new(zsh).exists() {
            return;
        }
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
}
