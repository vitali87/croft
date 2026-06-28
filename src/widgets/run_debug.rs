use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use std::path::PathBuf;

const BUTTON_BG_RGB: (u8, u8, u8) = (0x09, 0x67, 0xb8);
const BUTTON_FG_RGB: (u8, u8, u8) = (0xff, 0xff, 0xff);
const BODY_FG_RGB: (u8, u8, u8) = (0xb0, 0xb8, 0xc8);
const TITLE_FG_RGB: (u8, u8, u8) = (0xff, 0xff, 0xff);
const FOCUS_BORDER_RGB: (u8, u8, u8) = (0x4e, 0x9a, 0xff);

// --- Debug-panel (Variant A "Darcula") palette. Fixed darks chosen to read
// well on both the Black (#000) and Dark Blue (#1e222e) theme backgrounds. ---
const DBG_BAR_BG: Color = Color::Rgb(0x1c, 0x27, 0x33); // section-header bar fill
const DBG_BAR_FG: Color = Color::Rgb(0x8f, 0xd9, 0xcf); // section-header text
const DBG_FRAME_BG: Color = Color::Rgb(0x20, 0x2a, 0x37); // selected-frame fill
const DBG_ACCENT: Color = Color::Rgb(0x5c, 0xd6, 0xc8); // brand teal stripe / caret
const DBG_SCOPE: Color = Color::Rgb(0xc8, 0xa0, 0x6a); // Locals / Globals (gold)
const DBG_NAME: Color = Color::Rgb(0xbc, 0xbe, 0xc4); // identifiers / var names
const DBG_EQ: Color = Color::Rgb(0x6b, 0x72, 0x80); // "="
const DBG_TYPE: Color = Color::Rgb(0x78, 0x7c, 0x80); // ": type"
const DBG_STR: Color = Color::Rgb(0x6a, 0xab, 0x73); // strings / collections
const DBG_NUM: Color = Color::Rgb(0x2a, 0xac, 0xb8); // numbers
const DBG_KW: Color = Color::Rgb(0xcf, 0x8e, 0x6d); // None / bool
const DBG_CHEV: Color = Color::Rgb(0x5c, 0x66, 0x75); // expand chevrons
const DBG_LOC: Color = Color::Rgb(0x6b, 0x76, 0x89); // file:line / dim console
const DBG_PILL_BG: Color = Color::Rgb(0x0e, 0x7e, 0x76); // status pill
const DBG_PILL_FG: Color = Color::Rgb(0xdf, 0xfd, 0xf7);
const DBG_FRAME_SEL_FG: Color = Color::Rgb(0xff, 0xff, 0xff);
const DBG_FIELD_BG: Color = Color::Rgb(0x0d, 0x11, 0x17); // REPL input field fill
const DBG_PLACEHOLDER: Color = Color::Rgb(0x4b, 0x55, 0x63); // ghost hint text

/// Write `s` at `(x, y)` styled, clipped to the half-open column range
/// `[x, right)`, and return the column just past what was drawn. Lets the
/// renderer lay coloured segments left-to-right without overflowing the panel.
fn put(buf: &mut Buffer, x: u16, y: u16, right: u16, s: &str, style: Style) -> u16 {
    if x >= right {
        return x;
    }
    let budget = (right - x) as usize;
    let clipped: String = s.chars().take(budget).collect();
    let drawn = clipped.chars().count() as u16;
    buf.set_string(x, y, &clipped, style);
    x + drawn
}

/// Add a background colour to `style` when `bg` is `Some` (so a selected frame's
/// segments share its highlight fill).
fn with_bg(style: Style, bg: Option<Color>) -> Style {
    match bg {
        Some(c) => style.bg(c),
        None => style,
    }
}

/// Colour a Python value by its `type_name` (PyCharm/Darcula-ish): strings and
/// collections green, numbers cyan, None/bool orange, everything else (objects)
/// the neutral name grey.
fn value_color(type_name: &str) -> Color {
    match type_name {
        "str" => DBG_STR,
        "int" | "float" | "complex" => DBG_NUM,
        "bool" | "NoneType" => DBG_KW,
        "list" | "tuple" | "dict" | "set" | "frozenset" | "bytes" | "bytearray" => DBG_STR,
        _ => DBG_NAME,
    }
}

/// Cells reserved above the headline for the OSC-1337 debug-alt icon
/// overlay. Six cells wide × three cells tall lands a roughly 60×60-pixel
/// codicon at typical iTerm2 cell sizes (10×20 px), matching the VS Code
/// empty-state proportions the user supplied. The image is painted post-
/// frame by `App::flush_run_debug_icon_overlay`; on view change away from
/// Run-Debug the App's `terminal.clear()` evicts the cached image cells
/// (the same pipeline the welcome wordmark and editor preview use).
pub const RUN_DEBUG_ICON_CELLS_W: u16 = 6;
pub const RUN_DEBUG_ICON_CELLS_H: u16 = 3;

/// What a [`DebugRow`] represents, so a click can act on it (navigate to a
/// frame, expand a variable) and so the renderer can colour each part. Each
/// variant carries the structured pieces the renderer styles independently
/// (name vs value vs type), PyCharm-style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugRowKind {
    /// A section header ("CALL STACK", "VARIABLES") drawn as a filled bar.
    Header { title: String },
    /// A call-stack frame; `name` is the function, `location` is `file:line`.
    Frame {
        id: i64,
        selected: bool,
        name: String,
        location: String,
    },
    /// A scope container row ("Locals", "Globals").
    Scope { name: String },
    /// A variable. `reference > 0` means expandable; `value`/`type_name` are
    /// coloured by type.
    Variable {
        reference: i64,
        expandable: bool,
        expanded: bool,
        name: String,
        value: String,
        type_name: String,
    },
}

/// One rendered line of the paused-state debug tree. The app builds these from
/// the session; the panel renders them and maps clicks back to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugRow {
    pub indent: u8,
    pub kind: DebugRowKind,
}

pub struct RunDebugPanel {
    pub focused: bool,
    /// True under the Black theme: overpaint the focused outer border with the
    /// orange→green brand gradient instead of the legacy solid blue. Set by the
    /// app's focus/theme sync.
    pub focus_gradient: bool,
    pub active_file: Option<PathBuf>,
    /// Whether the active file is something croft can run or debug. When false
    /// (e.g. an extensionless `.git/config`) the empty state shows a "can't run
    /// this file" note instead of an actionable button. Set by the app.
    pub runnable: bool,
    /// Whether the button's action is debug (compiled source: build + lldb)
    /// rather than run-in-terminal (scripts / executables). Drives both the
    /// button verb ("Debug" vs "Run") and the click dispatch. Set by the app.
    pub run_is_debug: bool,
    pub last_area: Rect,
    pub last_button_area: Rect,
    /// When a debug session is paused, the app fills these rows (call stack +
    /// variables) and sets `debug_active`; the panel then renders the tree
    /// instead of the empty-state Run button.
    pub debug_active: bool,
    pub debug_status: String,
    pub debug_rows: Vec<DebugRow>,
    pub debug_scroll: usize,
    /// Screen y of the first rendered debug row and how many rows were drawn,
    /// recorded each frame so a click can be mapped to a row index.
    pub last_debug_row_y0: u16,
    pub last_debug_rows_shown: usize,
    /// Tail of the debug console (program output + REPL echoes/results), set by
    /// the app each refresh; rendered below the variables tree.
    pub console_tail: Vec<String>,
    /// Current REPL input line, rendered as a `❯` prompt at the panel bottom.
    pub repl_input: String,
    /// Top-left cell of the OSC-1337 icon overlay block. The post-draw
    /// flush in `App` reads this to emit the rasterised debug-alt PNG
    /// above the headline. `None` when the panel hasn't been laid out,
    /// or when the panel is too short for the icon to fit alongside
    /// the rest of the cluster.
    pub last_icon_cell: Option<(u16, u16)>,
    pub feedback: Option<String>,
    pub feedback_is_error: bool,
    /// True from when a debug session ends until the next one starts. While set
    /// (and `console_tail` is non-empty) the idle panel keeps the just-ended
    /// session's console output on screen instead of wiping straight back to the
    /// Run button, so a fast-exiting program's output/errors stay readable.
    pub session_ended: bool,
}

impl RunDebugPanel {
    pub fn new() -> Self {
        Self {
            focused: false,
            focus_gradient: false,
            active_file: None,
            runnable: false,
            run_is_debug: false,
            last_area: Rect::default(),
            last_button_area: Rect::default(),
            debug_active: false,
            debug_status: String::new(),
            debug_rows: Vec::new(),
            debug_scroll: 0,
            last_debug_row_y0: 0,
            last_debug_rows_shown: 0,
            console_tail: Vec::new(),
            repl_input: String::new(),
            last_icon_cell: None,
            feedback: None,
            feedback_is_error: false,
            session_ended: false,
        }
    }

    /// Map a click at screen row `y` to the index of the debug row drawn there,
    /// or `None` if the click missed the rendered rows.
    pub fn debug_row_at(&self, y: u16) -> Option<usize> {
        if !self.debug_active || self.last_debug_rows_shown == 0 || y < self.last_debug_row_y0 {
            return None;
        }
        let offset = (y - self.last_debug_row_y0) as usize;
        if offset < self.last_debug_rows_shown {
            Some(self.debug_scroll + offset)
        } else {
            None
        }
    }

    pub fn set_active_file(&mut self, path: Option<PathBuf>) {
        self.active_file = path;
    }

    pub fn click_button(&self, x: u16, y: u16) -> bool {
        let r = self.last_button_area;
        r.width > 0
            && r.height > 0
            && x >= r.x
            && x < r.x + r.width
            && y >= r.y
            && y < r.y + r.height
    }

    /// Render the paused-state tree (Variant A / Darcula): a status pill, the
    /// call-stack + variables tree with type-coloured values, then the console
    /// and a REPL bar pinned to the bottom. Records `last_debug_row_y0` /
    /// `last_debug_rows_shown` for click mapping.
    fn render_debug_tree(&mut self, inner: Rect, buf: &mut Buffer) {
        let right = inner.x + inner.width;
        let mut y = inner.y;

        // Status pill, e.g. " ⏸ Paused (breakpoint) ".
        if !self.debug_status.is_empty() {
            let pill = format!(" ⏸ {} ", self.debug_status);
            let pill: String = pill.chars().take(inner.width as usize).collect();
            buf.set_string(
                inner.x + 1,
                y,
                &pill,
                Style::default()
                    .fg(DBG_PILL_FG)
                    .bg(DBG_PILL_BG)
                    .add_modifier(Modifier::BOLD),
            );
            y += 2;
        }
        if y >= inner.y + inner.height {
            return;
        }

        // Reserve the bottom for the console + the REPL input area. The input
        // area is 2 rows: a "DEBUG CONSOLE" label and the filled input field.
        // The console is sized to its actual line count (capped at 6) so the
        // newest line sits directly above the label and output grows UPWARD,
        // like a terminal — never floating at the top of a fixed-height box.
        let bottom = inner.y + inner.height;
        let input_h: u16 = 2;
        let console_avail = bottom.saturating_sub(y).saturating_sub(input_h + 2);
        let console_h: u16 = (self.console_tail.len() as u16).min(6).min(console_avail);
        let rows_bottom = bottom.saturating_sub(input_h + console_h);
        let rows_area_h = rows_bottom.saturating_sub(y) as usize;
        self.last_debug_row_y0 = y;

        let mut shown = 0usize;
        for row in self
            .debug_rows
            .iter()
            .skip(self.debug_scroll)
            .take(rows_area_h)
        {
            let row_y = y + shown as u16;
            match &row.kind {
                DebugRowKind::Header { title } => {
                    // Filled bar across the full width, title in teal.
                    buf.set_style(
                        Rect {
                            x: inner.x,
                            y: row_y,
                            width: inner.width,
                            height: 1,
                        },
                        Style::default().bg(DBG_BAR_BG),
                    );
                    let t: String = title
                        .chars()
                        .take(inner.width.saturating_sub(2) as usize)
                        .collect();
                    buf.set_string(
                        inner.x + 1,
                        row_y,
                        &t,
                        Style::default()
                            .fg(DBG_BAR_FG)
                            .bg(DBG_BAR_BG)
                            .add_modifier(Modifier::BOLD),
                    );
                }
                DebugRowKind::Frame {
                    selected,
                    name,
                    location,
                    ..
                } => {
                    if *selected {
                        buf.set_style(
                            Rect {
                                x: inner.x,
                                y: row_y,
                                width: inner.width,
                                height: 1,
                            },
                            Style::default().bg(DBG_FRAME_BG),
                        );
                        buf.set_string(
                            inner.x,
                            row_y,
                            "▌",
                            Style::default().fg(DBG_ACCENT).bg(DBG_FRAME_BG),
                        );
                    }
                    let bg = if *selected { Some(DBG_FRAME_BG) } else { None };
                    let mut x = inner.x + 2;
                    let name_style = if *selected {
                        Style::default()
                            .fg(DBG_FRAME_SEL_FG)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(DBG_NAME)
                    };
                    x = put(buf, x, row_y, right, name, with_bg(name_style, bg));
                    x = put(buf, x, row_y, right, " ", with_bg(Style::default(), bg));
                    put(
                        buf,
                        x,
                        row_y,
                        right,
                        location,
                        with_bg(Style::default().fg(DBG_LOC), bg),
                    );
                }
                DebugRowKind::Scope { name } => {
                    put(
                        buf,
                        inner.x + 1,
                        row_y,
                        right,
                        name,
                        Style::default().fg(DBG_SCOPE).add_modifier(Modifier::BOLD),
                    );
                }
                DebugRowKind::Variable {
                    expandable,
                    expanded,
                    name,
                    value,
                    type_name,
                    ..
                } => {
                    let indent = (row.indent as u16) * 2;
                    let mut x = inner.x + indent;
                    let chev = if *expandable {
                        if *expanded { "▾ " } else { "▸ " }
                    } else {
                        "  "
                    };
                    x = put(buf, x, row_y, right, chev, Style::default().fg(DBG_CHEV));
                    x = put(buf, x, row_y, right, name, Style::default().fg(DBG_NAME));
                    x = put(buf, x, row_y, right, " = ", Style::default().fg(DBG_EQ));
                    x = put(
                        buf,
                        x,
                        row_y,
                        right,
                        value,
                        Style::default().fg(value_color(type_name)),
                    );
                    if !type_name.is_empty() {
                        put(
                            buf,
                            x,
                            row_y,
                            right,
                            &format!(" : {type_name}"),
                            Style::default().fg(DBG_TYPE).add_modifier(Modifier::ITALIC),
                        );
                    }
                }
            }
            shown += 1;
        }
        self.last_debug_rows_shown = shown;

        // Bottom-up: the input field is the last row, the "DEBUG CONSOLE" label
        // the row above it, and any console output sits above that.
        let field_y = bottom - 1;
        let label_y = bottom - 2;

        // Console: program output + REPL echoes, above the label.
        if console_h > 0 {
            let console_y0 = label_y.saturating_sub(console_h);
            for (i, line) in self
                .console_tail
                .iter()
                .rev()
                .take(console_h as usize)
                .rev()
                .enumerate()
            {
                let style = if line.starts_with('❯') {
                    Style::default().fg(DBG_ACCENT)
                } else {
                    Style::default().fg(DBG_LOC)
                };
                put(buf, inner.x + 1, console_y0 + i as u16, right, line, style);
            }
        }

        // "DEBUG CONSOLE" label above the field.
        put(
            buf,
            inner.x + 1,
            label_y,
            right,
            "DEBUG CONSOLE",
            Style::default().fg(DBG_LOC).add_modifier(Modifier::BOLD),
        );

        // The input field: a subtle full-width fill makes it read as a text box.
        // A teal caret, then the input (or a dim-italic placeholder when empty),
        // with a teal block cursor marking the insertion point.
        buf.set_style(
            Rect {
                x: inner.x,
                y: field_y,
                width: inner.width,
                height: 1,
            },
            Style::default().bg(DBG_FIELD_BG),
        );
        let bg = Some(DBG_FIELD_BG);
        let caret_end = put(
            buf,
            inner.x + 1,
            field_y,
            right,
            "❯ ",
            with_bg(
                Style::default().fg(DBG_ACCENT).add_modifier(Modifier::BOLD),
                bg,
            ),
        );
        if self.repl_input.is_empty() {
            // Block cursor at the insertion point, then the ghost placeholder.
            let after_cursor = put(
                buf,
                caret_end,
                field_y,
                right,
                " ",
                Style::default().bg(DBG_ACCENT),
            );
            put(
                buf,
                after_cursor,
                field_y,
                right,
                "Evaluate expression",
                with_bg(
                    Style::default()
                        .fg(DBG_PLACEHOLDER)
                        .add_modifier(Modifier::ITALIC),
                    bg,
                ),
            );
        } else {
            let input_end = put(
                buf,
                caret_end,
                field_y,
                right,
                &self.repl_input,
                with_bg(Style::default().fg(DBG_NAME), bg),
            );
            put(
                buf,
                input_end,
                field_y,
                right,
                " ",
                Style::default().bg(DBG_ACCENT),
            );
        }
    }

    pub fn button_label(&self) -> String {
        match self.active_file.as_ref().and_then(|p| p.file_name()) {
            Some(name) => {
                // Compiled source builds + launches the debugger; scripts and
                // executables run in a terminal.
                let verb = if self.run_is_debug { "Debug" } else { "Run" };
                format!("{verb} {}", name.to_string_lossy())
            }
            None => String::from("Run and Debug"),
        }
    }
}

impl Default for RunDebugPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &mut RunDebugPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let border_style = if self.focused {
            Style::default().fg(Color::Rgb(
                FOCUS_BORDER_RGB.0,
                FOCUS_BORDER_RGB.1,
                FOCUS_BORDER_RGB.2,
            ))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the brand gradient.
        // This panel has no border title, so nothing needs re-stamping.
        if self.focused && self.focus_gradient {
            crate::gradient::paint_gradient_box(buf, area);
        }
        self.last_area = area;
        self.last_button_area = Rect::default();
        self.last_icon_cell = None;
        self.last_debug_rows_shown = 0;
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Paused at a breakpoint: render the call-stack + variables tree instead
        // of the empty-state Run button.
        if self.debug_active {
            self.render_debug_tree(inner, buf);
            return;
        }

        // Vertical cluster, top-to-bottom: icon, gap, headline, gap, body, gap, button.
        // Centre the cluster vertically so the panel feels balanced like the mockup.
        const GAP_AFTER_ICON: u16 = 2;
        const TITLE_H: u16 = 1;
        const GAP_AFTER_TITLE: u16 = 1;
        const BODY_MAX_H: u16 = 3;
        const GAP_AFTER_BODY: u16 = 2;
        const BUTTON_H: u16 = 3;

        let icon_w = RUN_DEBUG_ICON_CELLS_W.min(inner.width);
        let icon_h_full = RUN_DEBUG_ICON_CELLS_H;
        let cluster_full = icon_h_full
            + GAP_AFTER_ICON
            + TITLE_H
            + GAP_AFTER_TITLE
            + BODY_MAX_H
            + GAP_AFTER_BODY
            + BUTTON_H;

        // If the panel is too short for the full cluster, drop the icon
        // first (it's decorative) before sacrificing the body or button.
        let (icon_h, gap_after_icon, cluster) = if inner.height >= cluster_full {
            (icon_h_full, GAP_AFTER_ICON, cluster_full)
        } else {
            (
                0,
                0,
                TITLE_H + GAP_AFTER_TITLE + BODY_MAX_H + GAP_AFTER_BODY + BUTTON_H,
            )
        };

        // After a session ends with output, anchor the cluster to the top so the
        // retained console (rendered below the button) gets the lower region;
        // otherwise centre it like the resting state.
        let show_ended_console = self.session_ended && !self.console_tail.is_empty();
        let top_pad = if show_ended_console {
            0
        } else if inner.height > cluster {
            (inner.height - cluster) / 2
        } else {
            0
        };
        let mut y = inner.y + top_pad;

        if icon_h > 0 && y + icon_h <= inner.y + inner.height {
            let icon_x = inner.x + (inner.width.saturating_sub(icon_w)) / 2;
            self.last_icon_cell = Some((icon_x, y));
            // Glyph fallback: paint the cod-debug-alt codicon centred
            // inside the reserved icon block so terminals without
            // OSC-1337 image support still see a recognisable shape.
            // On iTerm2 the post-draw `flush_run_debug_icon_overlay`
            // overwrites these cells with the proper rasterised PNG.
            let glyph_style = Style::default()
                .fg(Color::Rgb(BODY_FG_RGB.0, BODY_FG_RGB.1, BODY_FG_RGB.2))
                .add_modifier(Modifier::BOLD);
            let glyph_x = icon_x + icon_w / 2;
            let glyph_y = y + icon_h / 2;
            buf.set_span(
                glyph_x,
                glyph_y,
                &Span::styled(crate::icons::ACTIVITY_RUN_DEBUG.to_string(), glyph_style),
                1,
            );
            y += icon_h + gap_after_icon;
        }

        if y + TITLE_H > inner.y + inner.height {
            return;
        }
        let title = Paragraph::new(Line::from("RUN AND DEBUG"))
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(Color::Rgb(TITLE_FG_RGB.0, TITLE_FG_RGB.1, TITLE_FG_RGB.2))
                    .add_modifier(Modifier::BOLD),
            );
        title.render(
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: TITLE_H,
            },
            buf,
        );
        y += TITLE_H + GAP_AFTER_TITLE;

        if y >= inner.y + inner.height {
            return;
        }
        let body_text = match self.active_file.as_ref() {
            Some(_) if self.runnable => {
                "Press the button below to run the active file in a new terminal."
            }
            Some(_) => {
                "This file can't be run or debugged. Open a Python, Rust, C/C++, or script file."
            }
            None => "Open a file that can be run or debugged, then press the button below.",
        };
        let body_h = BODY_MAX_H.min(inner.y + inner.height - y);
        let body_x_pad = (inner.width / 12).clamp(1, 4);
        let body_area = Rect {
            x: inner.x + body_x_pad,
            y,
            width: inner.width.saturating_sub(body_x_pad * 2).max(1),
            height: body_h,
        };
        let body = Paragraph::new(body_text)
            .style(Style::default().fg(Color::Rgb(BODY_FG_RGB.0, BODY_FG_RGB.1, BODY_FG_RGB.2)))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
        body.render(body_area, buf);
        y += BODY_MAX_H + GAP_AFTER_BODY;

        // A file is open but croft can't run/debug it: show only the note, no
        // actionable button. (`last_button_area` stays default, so a click in
        // the panel does nothing.) The no-file case still shows the generic
        // "Run and Debug" button.
        if self.active_file.is_some() && !self.runnable {
            return;
        }
        if y + BUTTON_H > inner.y + inner.height {
            return;
        }
        let label = self.button_label();
        let label_chars = label.chars().count() as u16;
        // Pin the button to the panel's interior so it can never be wider
        // than the available space; without this clamp `inner.width -
        // button_w` underflowed at narrow sidebar widths and ratatui
        // panicked indexing the buffer at a wrapped-u16 column.
        let button_w = (label_chars + 8).min(inner.width.saturating_sub(2));
        if button_w < 4 {
            return;
        }
        let button_x = inner.x + inner.width.saturating_sub(button_w) / 2;
        let button_area = Rect {
            x: button_x,
            y,
            width: button_w,
            height: BUTTON_H,
        };
        self.last_button_area = button_area;
        // Black theme: brand-teal button fill; Croft Dark keeps VS Code blue.
        let button_bg = if self.focus_gradient {
            crate::gradient::rgb_color(crate::gradient::PRIMARY_BTN_BG)
        } else {
            Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2)
        };
        crate::widgets::source_control::render_rounded_button(
            buf,
            button_area,
            label.as_str(),
            button_bg,
            Color::Rgb(BUTTON_FG_RGB.0, BUTTON_FG_RGB.1, BUTTON_FG_RGB.2),
        );

        let mut next_y = button_area.y + button_area.height + 1;
        if let Some(msg) = self.feedback.as_ref()
            && next_y < inner.y + inner.height
        {
            let style = if self.feedback_is_error {
                Style::default().fg(Color::Rgb(0xe7, 0x70, 0x70))
            } else {
                Style::default().fg(Color::Rgb(0xa3, 0xbe, 0x8c))
            };
            buf.set_string(inner.x + 1, next_y, msg.as_str(), style);
            next_y = next_y.saturating_add(1);
        }
        // Retained console from the just-ended session: program output / errors
        // stay readable after the run instead of being wiped to the Run button.
        if show_ended_console && next_y + 1 < inner.y + inner.height {
            next_y = next_y.saturating_add(1);
            buf.set_string(
                inner.x + 1,
                next_y,
                "DEBUG CONSOLE",
                Style::default()
                    .fg(Color::Rgb(TITLE_FG_RGB.0, TITLE_FG_RGB.1, TITLE_FG_RGB.2))
                    .add_modifier(Modifier::BOLD),
            );
            next_y = next_y.saturating_add(1);
            let bottom = inner.y + inner.height;
            let avail = bottom.saturating_sub(next_y) as usize;
            let start = self.console_tail.len().saturating_sub(avail);
            let max_w = inner.width.saturating_sub(2) as usize;
            let line_style = Style::default().fg(Color::Rgb(0x9d, 0xa5, 0xb4));
            for line in &self.console_tail[start..] {
                if next_y >= bottom {
                    break;
                }
                let shown: String = line.chars().take(max_w).collect();
                buf.set_string(inner.x + 1, next_y, &shown, line_style);
                next_y = next_y.saturating_add(1);
            }
        }
        let _ = next_y;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_state_renders_call_stack_and_variables_and_maps_clicks() {
        let mut panel = RunDebugPanel::new();
        panel.debug_active = true;
        panel.debug_status = String::from("Paused (breakpoint)");
        panel.debug_rows = vec![
            DebugRow {
                indent: 0,
                kind: DebugRowKind::Header {
                    title: "CALL STACK".into(),
                },
            },
            DebugRow {
                indent: 1,
                kind: DebugRowKind::Frame {
                    id: 1000,
                    selected: true,
                    name: "isValid".into(),
                    location: "app.py:12".into(),
                },
            },
            DebugRow {
                indent: 0,
                kind: DebugRowKind::Header {
                    title: "VARIABLES".into(),
                },
            },
            DebugRow {
                indent: 1,
                kind: DebugRowKind::Scope {
                    name: "Locals".into(),
                },
            },
            DebugRow {
                indent: 2,
                kind: DebugRowKind::Variable {
                    reference: 0,
                    expandable: false,
                    expanded: false,
                    name: "x".into(),
                    value: "1".into(),
                    type_name: "int".into(),
                },
            },
            DebugRow {
                indent: 2,
                kind: DebugRowKind::Variable {
                    reference: 9,
                    expandable: true,
                    expanded: false,
                    name: "obj".into(),
                    value: "<Foo>".into(),
                    type_name: "Foo".into(),
                },
            },
        ];
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let dump = buffer_to_string(&buf);
        assert!(dump.contains("CALL STACK"), "call stack header:\n{dump}");
        assert!(dump.contains("VARIABLES"), "variables header:\n{dump}");
        assert!(dump.contains("isValid"), "frame name:\n{dump}");
        assert!(dump.contains("app.py:12"), "frame location:\n{dump}");
        assert!(dump.contains("x = 1"), "variable row:\n{dump}");
        assert!(
            dump.contains("▸ obj = <Foo>"),
            "expandable chevron:\n{dump}"
        );
        // The empty-state Run button must NOT be laid out while debugging.
        assert_eq!(panel.last_button_area, Rect::default());
        // Click mapping: first row sits at last_debug_row_y0.
        let y0 = panel.last_debug_row_y0;
        assert_eq!(panel.debug_row_at(y0), Some(0));
        assert_eq!(panel.debug_row_at(y0 + 1), Some(1));
        assert_eq!(panel.debug_row_at(y0.saturating_sub(1)), None);
    }

    #[test]
    fn paused_state_renders_console_tail_and_repl_prompt() {
        let mut panel = RunDebugPanel::new();
        panel.debug_active = true;
        panel.debug_status = String::from("Paused (breakpoint)");
        panel.debug_rows = vec![DebugRow {
            indent: 0,
            kind: DebugRowKind::Header {
                title: "CALL STACK".into(),
            },
        }];
        panel.console_tail = vec!["hello from program".into(), "42".into()];
        panel.repl_input = String::from("x + 1");
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("hello from program"),
            "console output:\n{dump}"
        );
        assert!(dump.contains("❯ x + 1"), "repl prompt with input:\n{dump}");
        assert!(
            dump.contains("DEBUG CONSOLE"),
            "input field must carry a label:\n{dump}"
        );
        // Bottom-anchored: the newest console line ("42") must sit on the row
        // directly above the DEBUG CONSOLE label, with no gap floating it up.
        let row_text = |y: u16| -> String {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        };
        let label_y = (0..area.height)
            .find(|&y| row_text(y).contains("DEBUG CONSOLE"))
            .expect("label present");
        assert!(
            row_text(label_y - 1).contains("42"),
            "newest console line must hug the label (row above it):\nlabel-1: {:?}",
            row_text(label_y - 1)
        );
    }

    #[test]
    fn empty_repl_shows_a_ghost_placeholder() {
        let mut panel = RunDebugPanel::new();
        panel.debug_active = true;
        panel.debug_status = String::from("Paused");
        panel.debug_rows = vec![DebugRow {
            indent: 0,
            kind: DebugRowKind::Header {
                title: "VARIABLES".into(),
            },
        }];
        // repl_input left empty
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("Evaluate expression"),
            "empty REPL must show a placeholder hint:\n{dump}"
        );
    }

    #[test]
    fn button_label_says_run_and_debug_when_no_file_is_active() {
        let panel = RunDebugPanel::new();
        assert_eq!(panel.button_label(), "Run and Debug");
    }

    #[test]
    fn button_label_includes_filename_when_a_file_is_active() {
        let mut panel = RunDebugPanel::new();
        panel.set_active_file(Some(PathBuf::from("/work/script.py")));
        assert_eq!(panel.button_label(), "Run script.py");
    }

    #[test]
    fn button_label_says_debug_for_compiled_source() {
        let mut panel = RunDebugPanel::new();
        panel.set_active_file(Some(PathBuf::from("/work/main.rs")));
        panel.run_is_debug = true;
        assert_eq!(panel.button_label(), "Debug main.rs");
    }

    #[test]
    fn click_button_is_inside_recorded_button_area() {
        let mut panel = RunDebugPanel::new();
        panel.last_button_area = Rect {
            x: 10,
            y: 5,
            width: 12,
            height: 3,
        };
        assert!(panel.click_button(10, 5));
        assert!(panel.click_button(21, 7));
        assert!(!panel.click_button(22, 5));
        assert!(!panel.click_button(15, 8));
    }

    #[test]
    fn rendering_lays_out_button_area_inside_panel() {
        let mut panel = RunDebugPanel::new();
        panel.set_active_file(Some(PathBuf::from("/work/run_me.rs")));
        panel.runnable = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let b = panel.last_button_area;
        assert!(b.width > 0 && b.height > 0, "button area must be laid out");
        assert!(b.x >= area.x && b.x + b.width <= area.x + area.width);
        assert!(b.y >= area.y && b.y < area.y + area.height);
    }

    #[test]
    fn non_runnable_file_shows_a_note_and_no_button() {
        // An open file croft can't run/debug (e.g. .git/config) must not offer a
        // Run button that would just fail.
        let mut panel = RunDebugPanel::new();
        panel.set_active_file(Some(PathBuf::from("/repo/.git/config")));
        panel.runnable = false;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        assert_eq!(
            panel.last_button_area,
            Rect::default(),
            "no Run button for a non-runnable file"
        );
        let mut dump = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                dump.push_str(buf[(x, y)].symbol());
            }
        }
        // The note wraps across rows in the narrow panel, so assert an
        // intra-line fragment rather than the whole phrase.
        assert!(
            dump.contains("can't be run") && dump.contains("debugged"),
            "must explain why there's no button: {dump:?}"
        );
    }

    fn buffer_to_string(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(buf.area.x + x, buf.area.y + y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn empty_state_renders_centred_title_and_description_and_chunky_button() {
        let mut panel = RunDebugPanel::new();
        let area = Rect {
            x: 0,
            y: 0,
            width: 36,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("RUN AND DEBUG"),
            "headline must be present: \n{dump}"
        );
        assert!(
            dump.contains("Open a file"),
            "body copy must be present: \n{dump}"
        );
        assert!(
            !dump.lines().next().unwrap().contains("RUN AND DEBUG"),
            "title bar must NOT sit on the top border row (mockup has no title bar): \n{dump}"
        );
        let button = panel.last_button_area;
        assert!(
            button.height >= 3,
            "button must be at least 3 rows tall to feel chunky like the mockup; got height={}",
            button.height
        );
        // The OSC-1337 icon block must be reserved ABOVE the button.
        let icon = panel.last_icon_cell.expect("icon block must be reserved");
        assert!(
            button.y > icon.1,
            "button must sit below the icon (icon top y={}, button.y={})",
            icon.1,
            button.y
        );
    }

    #[test]
    fn icon_block_reserves_an_osc_1337_overlay_cell_with_a_glyph_fallback() {
        // The headline icon is a 6x3-cell OSC-1337 image overlay (the
        // codicon `debug-alt` PNG, same one the activity-bar slot uses),
        // emitted post-frame by App::flush_run_debug_icon_overlay. The
        // widget's job is to (1) reserve the icon block via
        // last_icon_cell and (2) paint the codicon glyph as a text
        // fallback for terminals without OSC-1337 support; iTerm2
        // overwrites those cells with the rasterised image.
        let mut panel = RunDebugPanel::new();
        let area = Rect {
            x: 0,
            y: 0,
            width: 36,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let (ix, iy) = panel
            .last_icon_cell
            .expect("icon overlay cell must be reserved on a tall enough panel");
        let icon_w = RUN_DEBUG_ICON_CELLS_W;
        let icon_h = RUN_DEBUG_ICON_CELLS_H;
        let glyph_x = ix + icon_w / 2;
        let glyph_y = iy + icon_h / 2;
        let cell = buf[(glyph_x, glyph_y)].symbol().to_string();
        assert_eq!(
            cell,
            crate::icons::ACTIVITY_RUN_DEBUG.to_string(),
            "glyph fallback must be the cod-debug-alt codicon at the centre of the icon block"
        );
    }

    #[test]
    fn run_and_debug_button_uses_rounded_border_corners() {
        let mut panel = RunDebugPanel::new();
        let area = Rect {
            x: 0,
            y: 0,
            width: 36,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let b = panel.last_button_area;
        assert!(b.height >= 3 && b.width >= 4, "button must be laid out");
        let tl = buf[(b.x, b.y)].symbol().to_string();
        let tr = buf[(b.x + b.width - 1, b.y)].symbol().to_string();
        let bl = buf[(b.x, b.y + b.height - 1)].symbol().to_string();
        let br = buf[(b.x + b.width - 1, b.y + b.height - 1)]
            .symbol()
            .to_string();
        assert_eq!(
            tl, "╭",
            "top-left button corner must be rounded; got {tl:?}"
        );
        assert_eq!(
            tr, "╮",
            "top-right button corner must be rounded; got {tr:?}"
        );
        assert_eq!(
            bl, "╰",
            "bottom-left button corner must be rounded; got {bl:?}"
        );
        assert_eq!(
            br, "╯",
            "bottom-right button corner must be rounded; got {br:?}"
        );
    }

    #[test]
    fn rendering_at_narrow_widths_does_not_panic() {
        // Repro for the user-reported crash: dragging the sidebar
        // splitter all the way to the left squeezed Run-and-Debug to a
        // few cells wide and ratatui panicked with `index outside of
        // buffer`. The cause was `inner.width - button_w` wrapping to a
        // huge u16 when the "Run and Debug" label (13 chars) was wider
        // than `inner.width`. Render the panel at every width from 0 up
        // to a reasonable maximum and assert nothing crashes.
        for width in 0u16..40 {
            for height in 0u16..30 {
                let mut panel = RunDebugPanel::new();
                panel.set_active_file(Some(PathBuf::from("/work/run_me.rs")));
                let area = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };
                if area.width == 0 || area.height == 0 {
                    continue;
                }
                let mut buf = Buffer::empty(area);
                Widget::render(&mut panel, area, &mut buf);
            }
        }
    }

    #[test]
    fn small_panel_skips_icon_block_to_keep_button_visible() {
        let mut panel = RunDebugPanel::new();
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 12,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        assert!(
            panel.last_button_area.height >= 3,
            "button must remain laid out when the panel collapses the icon block"
        );
    }
}
