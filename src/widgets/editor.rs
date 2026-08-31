use anyhow::{Context, Result};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::highlight::{
    HiSpan, LangKind, LangRegistry, compute_line_starts, decode_semantic_tokens, lang_for_extension,
};
use crate::widgets::scrollbar;

/// Hard cap on the size of a single text file the editor will load.
/// 50MB comfortably accommodates real-world LSP / build / debug logs
/// (the user hit "File too large" on a 7.4MB lsp.log under the old 5MB
/// cap) while still capping pathological inputs that would freeze the
/// UI thread during initial `Vec<String>` allocation + tree-sitter
/// highlight recompute. The cap applies AFTER the dedicated image /
/// PDF / spreadsheet branches in `open`, so larger media keep their
/// own per-format limits.
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp"];

/// Read-only image preview attached to a tab. Holds the raw file bytes so
/// the OSC-1337 inline-image bake can re-fit on resize without rereading
/// from disk, plus parsed metadata for the header line that's painted in
/// the buffer (so non-image-capable terminals still see meaningful info).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageView {
    pub bytes: Vec<u8>,
    pub format_label: String,
    pub pixel_w: u32,
    pub pixel_h: u32,
    pub byte_size: u64,
    /// Monotonic content stamp, fresh on every (re)load of the bytes. Part
    /// of the overlay's re-emit key: a PDF rebuilt on disk reloads to the
    /// same path, rect and page, and without this stamp the baked overlay
    /// kept showing the pre-rebuild pixels until a page turn.
    pub generation: u64,
    /// Set when this preview was rasterised from a PDF page; tracks the
    /// page-navigation state so re-renders on Page Down/Up know which
    /// page to ask the rasteriser for next.
    pub pdf: Option<PdfState>,
}

/// Next value for [`ImageView::generation`]: process-wide monotonic counter,
/// bumped on every byte (re)load anywhere.
fn next_image_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfState {
    pub source_path: PathBuf,
    pub current_page: u32,
    pub page_count: Option<u32>,
    pub backend: crate::pdf::PdfBackend,
    pub source_byte_size: u64,
    /// Link regions of `current_page`, extracted lazily on the first click
    /// (never on render — a page flip must not pay a second subprocess).
    /// `None` = not extracted yet; cleared on every page change.
    pub links: Option<crate::pdf::PageLinks>,
}

/// The background the image/sheet/diff canvases fill with. On iTerm2,
/// `Reset` inherits the session bg that `SetColors` re-tints per theme, so
/// the canvas matches the surrounding panes for free. Every other host
/// (Ghostty, Kitty, sixel) ignores `SetColors`, so `Reset` falls through to
/// the terminal's own background — and the frame prefill cannot save it,
/// because these fills override the prefill (ratatui applies any `Some`
/// bg, and `Some(Reset)` is `Some`). Those hosts paint the theme bg
/// explicitly instead, or a CSV/PDF/diff tab sits as a host-black island
/// inside an otherwise themed frame.
fn canvas_bg(host_is_iterm2: bool, theme: crate::theme::Theme) -> Color {
    if host_is_iterm2 {
        Color::Reset
    } else {
        theme.editor_bg()
    }
}

/// The background the image/PDF canvas fills with — a separate choice from
/// [`canvas_bg`] because the picture is an inline image the canvas must not
/// occlude. Kitty places the preview at `KITTY_Z_BELOW_TEXT_AND_BG` and
/// draws any cell with a non-default background OVER an image that deep, so
/// the canvas has to keep the DEFAULT background there; iTerm2 keeps
/// `Reset` for the [`canvas_bg`] reason (the `SetColors` session bg).
/// Neither host shows an untinted island while a picture is up: the bake
/// letterboxes with the theme bg pixel, covering every canvas cell. Sixel
/// blits into the cell buffer over the canvas, and a no-graphics host shows
/// only the placeholder, so both keep the themed fill.
fn image_canvas_bg(
    protocol: crate::iterm2_inline::InlineImageProtocol,
    theme: crate::theme::Theme,
) -> Color {
    use crate::iterm2_inline::InlineImageProtocol;
    match protocol {
        InlineImageProtocol::ITerm2 | InlineImageProtocol::Kitty => Color::Reset,
        InlineImageProtocol::Sixel | InlineImageProtocol::None => theme.editor_bg(),
    }
}

fn render_image_placeholder(
    image: &ImageView,
    path: Option<&Path>,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
) {
    // Solid bg fill so the OSC-1337 inline image (emitted post-frame on
    // capable terminals) sits on a clean canvas; on non-capable terminals
    // the metadata header below is the only content the user sees.
    let bg_style = Style::default().bg(bg);
    for y in inner.y..inner.y + inner.height {
        for x in inner.x..inner.x + inner.width {
            buf[(x, y)].set_style(bg_style);
            buf[(x, y)].set_symbol(" ");
        }
    }
    let name = path
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("(unnamed image)"));
    let header = if let Some(pdf) = image.pdf.as_ref() {
        let page_label = match pdf.page_count {
            Some(total) => format!("page {} / {}", pdf.current_page, total),
            None => format!("page {}", pdf.current_page),
        };
        format!(
            " {} · {} · {}×{} · {} · PDF (← / → to flip) ",
            name,
            page_label,
            image.pixel_w,
            image.pixel_h,
            format_bytes_human(pdf.source_byte_size),
        )
    } else {
        format!(
            " {} · {}×{} · {} · {} ",
            name,
            image.pixel_w,
            image.pixel_h,
            format_bytes_human(image.byte_size),
            image.format_label,
        )
    };
    buf.set_string(
        inner.x,
        inner.y,
        &header,
        Style::default()
            .fg(Color::White)
            .bg(theme.ui(Color::Rgb(0x09, 0x4d, 0x77)))
            .add_modifier(Modifier::BOLD),
    );
}

/// Resolve one parsed ANSI colour against the active theme, so the rendered
/// log matches what a terminal pane would show under the same theme. The low
/// 16 stay symbolic until here precisely so a theme switch recolours without
/// reparsing the file.
pub(crate) fn ansi_color_to_tui(
    c: crate::ansi_text::AnsiColor,
    theme: crate::theme::Theme,
) -> Color {
    use crate::ansi_text::AnsiColor;
    match c {
        AnsiColor::Indexed(i) => {
            let p = theme.ansi();
            let (r, g, b) = p[(i as usize).min(15)];
            Color::Rgb(r, g, b)
        }
        // The 6x6x6 cube and the 24-step greyscale ramp are fixed by the
        // xterm spec, not by the theme, so they are computed rather than
        // looked up.
        AnsiColor::Palette256(i) => {
            let i = i as u16;
            if i < 232 {
                let n = i - 16;
                let lvl = |v: u16| -> u8 { if v == 0 { 0 } else { (55 + v * 40) as u8 } };
                Color::Rgb(lvl(n / 36), lvl((n / 6) % 6), lvl(n % 6))
            } else {
                let v = (8 + (i - 232) * 10) as u8;
                Color::Rgb(v, v, v)
            }
        }
        AnsiColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Paint a rendered ANSI log tab (#257): one buffer line per file line, with
/// SGR spans resolved through the theme palette. Mutates the view because the
/// window is refilled around the viewport — the file is never held whole.
#[allow(clippy::too_many_arguments)]
fn render_log(
    view: &mut crate::log_view::LogView,
    path: Option<&Path>,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
    scroll: usize,
    search: Option<(&str, crate::widgets::search::SearchOpts)>,
    active: Option<(usize, usize, usize)>,
) {
    let bg_style = Style::default().bg(bg);
    for y in inner.y..inner.y + inner.height {
        for x in inner.x..inner.x + inner.width {
            buf[(x, y)].set_style(bg_style);
            buf[(x, y)].set_symbol(" ");
        }
    }
    if inner.height == 0 || inner.width == 0 {
        // Frame truth cuts both ways: a frame that paints nothing must
        // publish nothing. Returning with the previous rect still stored let
        // the mouse path accept a click in an area this frame did not paint,
        // after a resize or a layout change.
        view.last_body = Rect::default();
        return;
    }
    let name = path
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("log"));
    let truncated = view.truncated;
    let total = view.len();
    // While the background pass runs the count is a lower bound (#394); the
    // header says so rather than presenting a moving number as a total.
    let header = if truncated {
        format!(" {name} — rendered log, {total} lines shown (file truncated at the index cap) ")
    } else if view.indexing() {
        format!(" {name} — rendered log, {total} lines so far, indexing… ")
    } else {
        format!(" {name} — rendered log, {total} lines ")
    };
    let head_style = Style::default()
        .fg(Color::White)
        .bg(theme.ui(Color::Rgb(0x09, 0x4d, 0x77)));
    buf.set_stringn(
        inner.x,
        inner.y,
        format!("{header:width$}", width = inner.width as usize),
        inner.width as usize,
        head_style,
    );
    let body_top = inner.y + 1;
    let rows = inner.height.saturating_sub(1) as usize;
    if rows == 0 {
        view.last_body = Rect::default();
        return;
    }
    // Frame truth: the mouse path reads the body rect this frame painted,
    // rather than recomputing the header offset in a second place.
    view.last_body = Rect {
        x: inner.x,
        y: body_top,
        width: inner.width,
        height: rows as u16,
    };
    let selection = view.ordered_selection_public();
    // Refill the window around the viewport: one bounded read per scroll.
    let _ = view.ensure(scroll, rows);
    // Where each character lands, by display width: a CJK pair is four
    // cells, and the span after it starts on the fifth (#404). Advancing
    // by `chars().count()` put it on the third, over the pair's second
    // half, and the drift grew with every wide character on the line. One
    // map, refilled per row, so the redraw path allocates nothing.
    let mut cells = crate::cell_map::CellMap::default();
    let right = inner.x + inner.width;
    for r in 0..rows {
        let idx = scroll + r;
        let Some(line) = view.line(idx) else { break };
        let y = body_top + r as u16;
        cells.build_into(&line.text);
        for span in &line.spans {
            let x = inner.x.saturating_add(cells.cell_of_byte(span.start));
            if x >= right {
                break;
            }
            let text = &line.text[span.start..span.end];
            let mut style = Style::default().bg(bg);
            let (fg, sbg) = if span.style.inverse {
                // Swap against the theme's default pair, which the parser
                // deliberately does not know.
                (span.style.bg, span.style.fg)
            } else {
                (span.style.fg, span.style.bg)
            };
            if let Some(c) = fg {
                style = style.fg(ansi_color_to_tui(c, theme));
            } else if span.style.inverse {
                style = style.fg(bg);
            }
            if let Some(c) = sbg {
                style = style.bg(ansi_color_to_tui(c, theme));
            }
            if span.style.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if span.style.dim {
                style = style.add_modifier(Modifier::DIM);
            }
            if span.style.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if span.style.underline {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            let room = right.saturating_sub(x) as usize;
            buf.set_stringn(x, y, text, room, style);
        }
        // Selection goes under the find highlight and over the log's own
        // colours: it is the coarser mark, and a match inside a selection
        // should still read as the match.
        if let Some(((sr, sc), (er, ec))) =
            selection.filter(|((sr, _), (er, _))| idx >= *sr && idx <= *er)
        {
            {
                let from = if idx == sr { sc } else { 0 };
                let to = if idx == er {
                    ec
                } else {
                    line.text.chars().count()
                };
                for c in from..to {
                    if !paint_log_cells(buf, inner.x, right, y, &cells, c, |s| {
                        s.bg(theme.selection())
                    }) {
                        break;
                    }
                }
            }
        }
        // Find highlight goes over the painted colours, keyed off the same
        // stripped text the search ran on, so the columns line up with what
        // the spans put on screen (#257).
        if let Some((needle, opts)) = search {
            let active_on_line = active.filter(|(r, _, _)| *r == idx).map(|(_, c, l)| (c, l));
            paint_search_highlight(
                buf,
                inner.x,
                y,
                inner.width,
                &line.text,
                needle,
                opts,
                0,
                active_on_line,
                &[],
                Some(&cells),
                theme,
            );
        }
    }
}

/// Restyle every cell character `col` occupies on a rendered log row, using
/// the same cell map the painter used so a band lands on the character it
/// names rather than on its character index. `restyle` is given the cell's
/// current style. Returns false once the row's right edge is reached, so a
/// caller walking columns can stop.
fn paint_log_cells(
    buf: &mut Buffer,
    left: u16,
    right: u16,
    y: u16,
    cells: &crate::cell_map::CellMap,
    col: usize,
    restyle: impl Fn(Style) -> Style,
) -> bool {
    let first = left.saturating_add(cells.cell_of_char(col));
    let width = cells.width_of_char(col);
    // A character that does not fit WHOLE is one `set_stringn` dropped
    // entirely, so a band on its first half would colour a cell the painter
    // deliberately left blank: the same painter/annotator disagreement this
    // module exists to close, at the boundary.
    if first >= right || first.saturating_add(width) > right {
        return false;
    }
    for x in first..first + width {
        let cell = &mut buf[(x, y)];
        cell.set_style(restyle(cell.style()));
    }
    true
}

/// Paint a hex tab (#172): header row, `offset  hex  |ascii|` body rows,
/// and a status row with the cursor offset. Mutates the view: the window
/// is refilled around the viewport, and the chosen bytes-per-row plus the
/// frame's layout are written back for navigation and mouse hit-testing
/// (frame truth, the `last_wrap_rows` pattern).
fn render_hex(
    view: &mut crate::hex::HexView,
    path: Option<&Path>,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
) {
    let bg_style = Style::default().bg(bg);
    for y in inner.y..inner.y + inner.height {
        for x in inner.x..inner.x + inner.width {
            buf[(x, y)].set_style(bg_style);
            buf[(x, y)].set_symbol(" ");
        }
    }
    // Zero-size guard AFTER the layout reset below would leave stale hit
    // rects; clear them first (the #103 frame-truth invariant).
    view.layout = crate::hex::HexLayout::default();
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let offw = view.offset_digits() as u16;
    // Narrowest workable layout: lead + offset + gap + 8 hex cells
    // (3/byte minus the trailing space) + gap + 8 ascii cells. Below it
    // the per-cell clip would paint offsets with no bytes — a half-drawn
    // grid — so a too-narrow split shows a deliberate empty state (the
    // clamped header only) instead.
    let need8 = 1 + offw + 2 + (8 * 3 - 1) + 2 + 8;
    let name = path
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("(unnamed)"));
    let header = format!(" {} · HEX · {} bytes ", name, view.file_len);
    buf.set_stringn(
        inner.x,
        inner.y,
        &header,
        inner.width as usize,
        Style::default()
            .fg(Color::White)
            .bg(theme.ui(Color::Rgb(0x09, 0x4d, 0x77)))
            .add_modifier(Modifier::BOLD),
    );
    if inner.height < 3 || inner.width < need8 {
        return;
    }

    // lead + offset + gap + hex cells (3/byte minus the trailing space)
    // + mid-group gap + gap + ascii gutter
    let need16 = 1 + offw + 2 + (16 * 3 - 1) + 1 + 2 + 16;
    let bpr: u64 = if inner.width >= need16 { 16 } else { 8 };
    view.bytes_per_row = bpr;

    let data_top = inner.y + 1;
    let data_rows = inner.height - 2; // header + status row
    // Clamp the scroll to the new geometry and make the span resident
    // (a delta of zero only clamps + refills).
    view.scroll_by(0, data_rows as usize);

    let hex_x = inner.x + 1 + offw + 2;
    let mid_gap: u16 = if bpr == 16 { 1 } else { 0 };
    let ascii_x = hex_x + (bpr as u16) * 3 - 1 + mid_gap + 2;
    view.layout = crate::hex::HexLayout {
        data_top,
        data_rows,
        hex_x,
        ascii_x,
    };

    let sel = view.selection();
    let dim = Style::default().fg(Color::DarkGray).bg(bg);
    for r in 0..data_rows {
        let row = view.top_row + r as u64;
        if row >= view.total_rows() {
            break;
        }
        let y = data_top + r;
        let base = row * bpr;
        buf.set_string(
            inner.x + 1,
            y,
            format!("{base:0w$X}", w = offw as usize),
            dim,
        );
        for i in 0..bpr {
            let off = base + i;
            if off >= view.file_len {
                break;
            }
            let x = hex_x + (i as u16) * 3 + if i >= 8 { mid_gap } else { 0 };
            let ax = ascii_x + i as u16;
            if ax >= inner.x + inner.width || x + 1 >= inner.x + inner.width {
                break;
            }
            let edited = view.edits.contains_key(&off);
            let (mut hex_s, ascii_ch, byte_dim) = match view.effective_byte(off) {
                Some(v) => (
                    format!("{v:02X}"),
                    if (0x20..0x7f).contains(&v) {
                        (v as char).to_string()
                    } else {
                        String::from("·")
                    },
                    v == 0 && !edited,
                ),
                // Not resident (an IO error mid-scroll): an honest blank,
                // never a stale byte.
                None => (String::from("··"), String::from("·"), true),
            };
            let selected = sel.map(|(a, b)| off >= a && off < b).unwrap_or(false);
            let is_cursor = off == view.cursor;
            // A half-typed byte shows its high nibble in place (#173).
            if is_cursor
                && !view.ascii_focus
                && let Some(n) = view.pending_nibble
            {
                hex_s = format!("{n:X}·");
            }
            // The focused pane's cursor is the accent block; its mirror
            // in the other pane is a dim block so the pairing stays
            // visible without stealing focus.
            let cursor_style = Style::default()
                .fg(theme.accent_contrast_fg())
                .bg(theme.accent());
            let mirror_style = Style::default().fg(Color::White).bg(Color::DarkGray);
            let edited_style = Style::default()
                .fg(theme.ui(Color::Rgb(0xe0, 0x9a, 0x4e)))
                .bg(bg)
                .add_modifier(Modifier::BOLD);
            let base_style = if selected {
                Style::default().fg(Color::White).bg(theme.selection())
            } else if edited {
                edited_style
            } else if byte_dim {
                dim
            } else {
                Style::default().fg(theme.ui(Color::Gray)).bg(bg)
            };
            let (hex_style, ascii_style) = if is_cursor {
                if view.ascii_focus {
                    (mirror_style, cursor_style)
                } else {
                    (cursor_style, mirror_style)
                }
            } else {
                (base_style, base_style)
            };
            buf.set_string(x, y, &hex_s, hex_style);
            buf.set_string(ax, y, &ascii_ch, ascii_style);
        }
    }
    let status = format!(" {} ", view.status_line());
    buf.set_stringn(
        inner.x,
        inner.y + inner.height - 1,
        &status,
        inner.width as usize,
        Style::default()
            .fg(theme.ui(Color::Gray))
            .bg(theme.ui(Color::Rgb(0x07, 0x33, 0x55))),
    );
}

/// Paint an archive browser tab (#179): header, member rows (selected
/// highlighted, sizes right-aligned), and a hint row. Frame truth for
/// the row band is written back for mouse hit-testing.
fn render_archive(
    view: &mut crate::archive::ArchiveView,
    path: Option<&Path>,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
) {
    let bg_style = Style::default().bg(bg);
    for y in inner.y..inner.y + inner.height {
        for x in inner.x..inner.x + inner.width {
            buf[(x, y)].set_style(bg_style);
            buf[(x, y)].set_symbol(" ");
        }
    }
    view.rows_top = 0;
    view.rows_visible = 0;
    if inner.height < 4 || inner.width < 20 {
        return;
    }
    let name = path
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("(archive)"));
    let header = format!(
        " {} · {} · {} members · {} bytes ",
        name,
        view.kind.label(),
        view.entries.len(),
        view.source_byte_size
    );
    buf.set_stringn(
        inner.x,
        inner.y,
        &header,
        inner.width as usize,
        Style::default()
            .fg(Color::White)
            .bg(theme.ui(Color::Rgb(0x09, 0x4d, 0x77)))
            .add_modifier(Modifier::BOLD),
    );
    let rows_top = inner.y + 1;
    let rows = inner.height - 2;
    view.rows_top = rows_top;
    view.rows_visible = rows;
    // Keep the selection visible.
    if view.selected < view.scroll {
        view.scroll = view.selected;
    } else if view.selected >= view.scroll + rows as usize {
        view.scroll = view.selected + 1 - rows as usize;
    }
    for r in 0..rows as usize {
        let idx = view.scroll + r;
        let Some(entry) = view.entries.get(idx) else {
            break;
        };
        let y = rows_top + r as u16;
        let marker = if entry.dir { "▸ " } else { "  " };
        let size = if entry.dir {
            String::new()
        } else {
            format!("{} ", entry.size)
        };
        let style = if idx == view.selected {
            Style::default()
                .fg(theme.accent_contrast_fg())
                .bg(theme.accent())
        } else if entry.dir {
            Style::default().fg(Color::DarkGray).bg(bg)
        } else {
            Style::default().fg(theme.ui(Color::Gray)).bg(bg)
        };
        let name_w = (inner.width as usize).saturating_sub(size.len() + 3);
        let line = format!(" {marker}{:<name_w$}{size}", entry.path, name_w = name_w);
        buf.set_stringn(inner.x, y, &line, inner.width as usize, style);
    }
    let hint = " Enter: open member · E: extract member to a folder ";
    buf.set_stringn(
        inner.x,
        inner.y + inner.height - 1,
        hint,
        inner.width as usize,
        Style::default()
            .fg(theme.ui(Color::Gray))
            .bg(theme.ui(Color::Rgb(0x07, 0x33, 0x55))),
    );
}

fn render_sheet(
    view: &mut crate::sheet::SheetView,
    path: Option<&Path>,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
) {
    // Frame truth: reset before any early return so stale geometry can
    // never serve a mouse hit-test.
    view.grid = crate::sheet::SheetGridLayout::default();
    let editing = view.editing.clone();
    let dirty = view.dirty;
    // Bg fill so the spreadsheet sits on a clean canvas regardless of
    // what the previous tab left behind (see [`canvas_bg`]).
    let bg_style = Style::default().bg(bg);
    for y in inner.y..inner.y + inner.height {
        for x in inner.x..inner.x + inner.width {
            buf[(x, y)].set_style(bg_style);
            buf[(x, y)].set_symbol(" ");
        }
    }
    if inner.height < 3 || inner.width < 8 {
        return;
    }
    let name = path
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("(unnamed sheet)"));
    let sheet = match view.sheets.get(view.current_sheet) {
        Some(s) => s,
        None => return,
    };
    let row_count = sheet.row_count();
    let col_count = sheet.col_count();
    let header = format!(
        " {} · {} · sheet {}/{} ({}) · {} rows × {} cols{} ",
        name,
        view.kind.label(),
        view.current_sheet + 1,
        view.sheets.len(),
        sheet.name,
        row_count,
        col_count,
        if dirty { " · edited (Cmd+S)" } else { "" },
    );
    buf.set_string(
        inner.x,
        inner.y,
        &header,
        Style::default()
            .fg(Color::White)
            .bg(theme.ui(Color::Rgb(0x09, 0x4d, 0x77)))
            .add_modifier(Modifier::BOLD),
    );

    let grid_top = inner.y + 1;
    let grid_height = inner.height.saturating_sub(2); // header + status row
    let grid_w = inner.width;
    if grid_height < 2 || col_count == 0 {
        return;
    }

    // Reserve a row-number gutter so the user can see the absolute row
    // index even after horizontal scrolling.
    let gutter_w = (row_count.max(1).to_string().len() as u16 + 2).max(4);
    if grid_w <= gutter_w + 2 {
        return;
    }
    let body_x = inner.x + gutter_w;
    let body_w = grid_w - gutter_w;

    let header_y = grid_top;
    let data_top = grid_top + 1;
    let data_rows = grid_height.saturating_sub(1) as usize;

    // Header row backdrop.
    let head_style = Style::default()
        .fg(Color::White)
        .bg(theme.ui(Color::Rgb(0x07, 0x33, 0x55)))
        .add_modifier(Modifier::BOLD);
    for x in inner.x..inner.x + inner.width {
        buf[(x, header_y)].set_style(head_style);
        buf[(x, header_y)].set_symbol(" ");
    }

    // Lay out visible columns from `scroll_col` rightwards until we run
    // out of horizontal space.
    let mut visible: Vec<(usize, u16)> = Vec::new(); // (col_idx, x_offset)
    let mut x_off = 0u16;
    for (c, w) in sheet.col_widths.iter().enumerate().skip(sheet.scroll_col) {
        if x_off + w + 1 > body_w {
            break;
        }
        visible.push((c, x_off));
        x_off += w + 1; // +1 for inter-column gap
    }

    // Header text.
    for (c, x_off) in &visible {
        let label = sheet.headers.get(*c).map(|s| s.as_str()).unwrap_or("");
        let cell_x = body_x + *x_off;
        let w = sheet.col_widths[*c];
        write_cell(buf, cell_x, header_y, w, label, head_style);
    }

    // Data rows.
    let row_end = (sheet.scroll_row + data_rows).min(row_count);
    // Base rows wear the canvas bg (see [`canvas_bg`]); alternating rows
    // keep an explicit lift for the zebra stripe.
    let row_style = Style::default().fg(Color::White).bg(bg);
    let alt_row_style = Style::default()
        .fg(Color::White)
        .bg(theme.ui(Color::Rgb(0x24, 0x29, 0x37)));
    let gutter_style = Style::default().fg(Color::DarkGray);
    for (display_row, row_idx) in (sheet.scroll_row..row_end).enumerate() {
        let y = data_top + display_row as u16;
        let style = if display_row % 2 == 0 {
            row_style
        } else {
            alt_row_style
        };
        for x in inner.x..inner.x + inner.width {
            buf[(x, y)].set_style(style);
            buf[(x, y)].set_symbol(" ");
        }
        let row_label = format!(" {:>width$} ", row_idx + 1, width = (gutter_w - 2) as usize);
        buf.set_string(
            inner.x,
            y,
            &row_label,
            gutter_style.bg(style.bg.unwrap_or(Color::Reset)),
        );
        let row = &sheet.rows[row_idx];
        for (c, x_off) in &visible {
            let cell_text = row.get(*c).map(|s| s.as_str()).unwrap_or("");
            let w = sheet.col_widths[*c];
            let is_cursor = row_idx == sheet.cur_row && *c == sheet.cur_col;
            if is_cursor {
                if let Some(edit) = editing.as_ref() {
                    // In-grid editor (#177): show the tail that keeps the
                    // caret visible, caret cell reversed.
                    // Window in CHARACTERS (#193 review: the cursor is
                    // a byte offset, and byte math shortened the tail
                    // for multi-byte text), then convert to a byte start.
                    let avail = w as usize;
                    let chars_before = edit.value[..edit.cursor].chars().count();
                    let skip = chars_before.saturating_sub(avail.saturating_sub(1));
                    let start = edit
                        .value
                        .char_indices()
                        .nth(skip)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    let shown = &edit.value[start..];
                    let edit_style = Style::default()
                        .fg(theme.accent_contrast_fg())
                        .bg(theme.accent());
                    write_cell(buf, body_x + *x_off, y, w, shown, edit_style);
                    let caret_cells = shown[..edit.cursor - start].chars().count() as u16;
                    let cx = body_x + *x_off + caret_cells.min(w.saturating_sub(1));
                    buf[(cx, y)].set_style(
                        Style::default()
                            .fg(theme.accent())
                            .bg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    );
                } else {
                    let cursor_style = Style::default()
                        .fg(theme.accent_contrast_fg())
                        .bg(theme.accent())
                        .add_modifier(Modifier::BOLD);
                    write_cell(buf, body_x + *x_off, y, w, cell_text, cursor_style);
                }
            } else {
                write_cell(buf, body_x + *x_off, y, w, cell_text, style);
            }
        }
    }
    view.grid = crate::sheet::SheetGridLayout {
        data_top,
        data_rows: data_rows as u16,
        body_x,
        body_w,
    };

    // Status row at the bottom showing scroll position + nav hint.
    let status_y = inner.y + inner.height - 1;
    let visible_first = sheet.scroll_row + 1;
    let visible_last = (sheet.scroll_row + data_rows).min(row_count);
    let visible_col_first = sheet.scroll_col + 1;
    let visible_col_last = visible
        .last()
        .map(|(c, _)| *c + 1)
        .unwrap_or(visible_col_first);
    let status = format!(
        " rows {visible_first}–{visible_last} of {row_count} · cols {visible_col_first}–{visible_col_last} of {col_count} · ←/→ ↑/↓ PgUp/PgDn Tab=next sheet "
    );
    buf.set_string(
        inner.x,
        status_y,
        &status,
        Style::default()
            .fg(theme.ui(Color::Gray))
            .bg(theme.ui(Color::Rgb(0x14, 0x18, 0x22))),
    );
}

/// Render the merge editor's source panes (#253) into the TOP of `inner`
/// and return the remaining rect for the ordinary text path — the
/// editable Result pane. One header row carries the resolved counter,
/// then Current | (Base) | Incoming as columns (stacked vertically when
/// the terminal is narrow), then a separator row labelling the Result.
/// Per-conflict checkboxes are painted in a small gutter on the region's
/// first row and their hit rects recorded on the view for mouse routing.
fn render_merge_panes(
    mv: &mut crate::merge_editor::MergeView,
    inner: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
) -> Rect {
    use crate::merge_editor::{CheckSide, ConflictState};
    mv.check_spans.clear();
    mv.last_panes_area = Rect::default();
    // Too small for source panes: the Result gets everything, and the
    // commands (F7, accepts) still work off the tracked regions.
    if inner.height < 9 || inner.width < 24 {
        return inner;
    }
    let top_h = (inner.height / 2).min(inner.height - 4);
    let sep_y = inner.y + top_h;
    mv.last_panes_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: top_h + 1,
    };

    // Header: the acceptance counter plus the working chords.
    let resolved = mv.resolved_count();
    let total = mv.conflicts.len();
    let head_style = Style::default()
        .fg(Color::White)
        .bg(theme.ui(Color::Rgb(0x09, 0x4d, 0x77)))
        .add_modifier(Modifier::BOLD);
    for x in inner.x..inner.x + inner.width {
        buf[(x, inner.y)].set_style(head_style);
        buf[(x, inner.y)].set_symbol(" ");
    }
    let src = if mv.from_markers {
        "markers"
    } else {
        "git stages"
    };
    let header = format!(
        " MERGE ({src})  {resolved}/{total} conflict{} resolved \u{2022} F7 next \u{2022} Alt+\u{2191}\u{2193} scroll sources ",
        if total == 1 { "" } else { "s" }
    );
    buf.set_stringn(inner.x, inner.y, &header, inner.width as usize, head_style);

    // Separator row above the Result.
    let sep_style = Style::default()
        .fg(theme.ui(Color::Rgb(0x9a, 0xa4, 0xb2)))
        .bg(theme.ui(Color::Rgb(0x20, 0x24, 0x2c)));
    for x in inner.x..inner.x + inner.width {
        buf[(x, sep_y)].set_style(sep_style);
        buf[(x, sep_y)].set_symbol(" ");
    }
    buf.set_stringn(
        inner.x,
        sep_y,
        " RESULT (editable) \u{2014} \"Merge: Complete Merge\" stages the file ",
        inner.width as usize,
        sep_style,
    );

    // The panes: Current | (Base) | Incoming, or stacked when narrow.
    struct Pane<'a> {
        title: &'a str,
        lines: &'a [String],
        scroll: usize,
        side: Option<CheckSide>,
        // (start row in this pane's text, len, conflict idx, checked)
        regions: Vec<(usize, usize, usize, bool)>,
        tint: Color,
        active_tint: Color,
    }
    let region = |start: usize, len: usize, idx: usize, checked: bool| (start, len, idx, checked);
    let checked_cur = |s: ConflictState| {
        matches!(
            s,
            ConflictState::Current | ConflictState::Both | ConflictState::BothReverse
        )
    };
    let checked_inc = |s: ConflictState| {
        matches!(
            s,
            ConflictState::Incoming | ConflictState::Both | ConflictState::BothReverse
        )
    };
    let mut panes: Vec<Pane> = Vec::new();
    panes.push(Pane {
        title: "CURRENT (yours)",
        lines: &mv.ours,
        scroll: mv.ours_scroll,
        side: Some(CheckSide::Current),
        regions: mv
            .conflicts
            .iter()
            .enumerate()
            .map(|(i, c)| region(c.ours_start, c.ours.len(), i, checked_cur(c.state)))
            .collect(),
        tint: theme.ui(Color::Rgb(0x1b, 0x33, 0x22)),
        active_tint: theme.ui(Color::Rgb(0x2a, 0x4f, 0x33)),
    });
    if mv.show_base {
        panes.push(Pane {
            title: "BASE",
            lines: &mv.base,
            scroll: mv.base_scroll,
            side: None,
            regions: mv
                .conflicts
                .iter()
                .enumerate()
                .map(|(i, c)| region(c.base_start, c.base.len(), i, false))
                .collect(),
            tint: theme.ui(Color::Rgb(0x2a, 0x2a, 0x2a)),
            active_tint: theme.ui(Color::Rgb(0x3a, 0x3a, 0x3a)),
        });
    }
    panes.push(Pane {
        title: "INCOMING (theirs)",
        lines: &mv.theirs,
        scroll: mv.theirs_scroll,
        side: Some(CheckSide::Incoming),
        regions: mv
            .conflicts
            .iter()
            .enumerate()
            .map(|(i, c)| region(c.theirs_start, c.theirs.len(), i, checked_inc(c.state)))
            .collect(),
        tint: theme.ui(Color::Rgb(0x16, 0x2b, 0x44)),
        active_tint: theme.ui(Color::Rgb(0x1f, 0x41, 0x66)),
    });

    let panes_top = inner.y + 1;
    let panes_h = top_h.saturating_sub(1);
    let n = panes.len() as u16;
    let stacked = inner.width < 72;
    let mut rects: Vec<Rect> = Vec::new();
    if stacked {
        // Vertical stack: each pane gets an equal share of the rows.
        let share = panes_h / n;
        for i in 0..n {
            rects.push(Rect {
                x: inner.x,
                y: panes_top + i * share,
                width: inner.width,
                height: if i == n - 1 {
                    panes_h - share * (n - 1)
                } else {
                    share
                },
            });
        }
    } else {
        // Columns with a 1-cell seam between neighbours.
        let w = (inner.width - (n - 1)) / n;
        let mut x = inner.x;
        for i in 0..n {
            let width = if i == n - 1 {
                inner.x + inner.width - x
            } else {
                w
            };
            rects.push(Rect {
                x,
                y: panes_top,
                width,
                height: panes_h,
            });
            x += w;
            if i != n - 1 {
                for y in panes_top..panes_top + panes_h {
                    buf[(x, y)].set_symbol("\u{2502}");
                    buf[(x, y)]
                        .set_style(Style::default().fg(theme.ui(Color::Rgb(0x3a, 0x42, 0x52))));
                }
                x += 1;
            }
        }
    }

    let title_style = Style::default()
        .fg(theme.ui(Color::Rgb(0xc8, 0xd0, 0xdc)))
        .bg(theme.ui(Color::Rgb(0x20, 0x24, 0x2c)))
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(theme.ui(Color::Rgb(0xb6, 0xbd, 0xc8)));
    let gutter = 4u16; // "[x] " on a region's first row
    for (pane, rect) in panes.iter().zip(&rects) {
        if rect.height < 2 || rect.width < gutter + 4 {
            continue;
        }
        for x in rect.x..rect.x + rect.width {
            buf[(x, rect.y)].set_style(title_style);
            buf[(x, rect.y)].set_symbol(" ");
        }
        buf.set_stringn(
            rect.x + 1,
            rect.y,
            pane.title,
            rect.width.saturating_sub(1) as usize,
            title_style,
        );
        let body_h = (rect.height - 1) as usize;
        let text_w = (rect.width - gutter) as usize;
        for vis in 0..body_h {
            let idx = pane.scroll + vis;
            let y = rect.y + 1 + vis as u16;
            if idx >= pane.lines.len() {
                break;
            }
            let in_region = pane
                .regions
                .iter()
                .find(|(start, len, _, _)| *len > 0 && idx >= *start && idx < *start + *len);
            let style = match in_region {
                Some(&(_, _, ci, _)) if ci == mv.active => text_style.bg(pane.active_tint),
                Some(_) => text_style.bg(pane.tint),
                None => text_style,
            };
            if in_region.is_some() {
                for x in rect.x..rect.x + rect.width {
                    buf[(x, y)].set_style(style);
                    buf[(x, y)].set_symbol(" ");
                }
            }
            if let Some(&(start, _, ci, checked)) = in_region
                && idx == start
                && let Some(side) = pane.side
            {
                let mark = if checked { "[x]" } else { "[ ]" };
                buf.set_stringn(rect.x, y, mark, 3, style.add_modifier(Modifier::BOLD));
                mv.check_spans.push((y, rect.x..rect.x + 3, ci, side));
            }
            buf.set_stringn(rect.x + gutter, y, &pane.lines[idx], text_w, style);
        }
    }

    Rect {
        x: inner.x,
        y: sep_y + 1,
        width: inner.width,
        height: inner.height - top_h - 1,
    }
}

/// Returns the hit-test rects of the prev / next change arrows painted
/// in the diff header (in that order). Both are `Rect::default()` when
/// the header was too narrow to allocate them.
fn render_diff(
    diff: &mut crate::widgets::diff::DiffData,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
) -> (Rect, Rect) {
    use crate::widgets::diff::DiffRow;
    // Background fill so the diff sits on a clean canvas (see [`canvas_bg`]).
    let bg_style = Style::default().bg(bg);
    for y in inner.y..inner.y + inner.height {
        for x in inner.x..inner.x + inner.width {
            buf[(x, y)].set_style(bg_style);
            buf[(x, y)].set_symbol(" ");
        }
    }
    if inner.height < 3 || inner.width < 16 {
        return (Rect::default(), Rect::default());
    }
    if diff.unified {
        return render_unified_deletion(diff, inner, buf, bg, theme);
    }
    let left_name = diff
        .left_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| diff.left_path.display().to_string());
    let right_is_real = diff.right_path != std::path::Path::new("/dev/null")
        && !diff.right_path.as_os_str().is_empty();
    let right_name = if right_is_real {
        Some(
            diff.right_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| diff.right_path.display().to_string()),
        )
    } else {
        None
    };
    let header = match (&right_name, diff.bytes_differ_but_lines_equal) {
        (Some(r), true) => format!(
            " diff: {left_name}  \u{2194}  {r}   \u{2022} whitespace-only change (trailing newline / CRLF / BOM) — no line-level diff "
        ),
        (Some(r), false) => format!(" diff: {left_name}  \u{2194}  {r} "),
        // Synthetic git-diff text view: no real right-side path, so the
        // header reads as the diff command instead of trailing "↔ null".
        (None, _) => format!(" {left_name} "),
    };
    // Ignore-whitespace is a lens over the real diff, so say so in the header:
    // a reader must never mistake a hidden change for an absent one.
    let header = match diff.ws_mode {
        crate::widgets::diff::DiffWhitespace::Off => header,
        mode => format!("{header}\u{2022} ignoring whitespace: {} ", mode.label()),
    };
    let head_bg = if diff.bytes_differ_but_lines_equal {
        theme.ui(Color::Rgb(0x8a, 0x4a, 0x10))
    } else {
        theme.ui(Color::Rgb(0x09, 0x4d, 0x77))
    };
    let head_style = Style::default()
        .fg(Color::White)
        .bg(head_bg)
        .add_modifier(Modifier::BOLD);
    for x in inner.x..inner.x + inner.width {
        buf[(x, inner.y)].set_style(head_style);
        buf[(x, inner.y)].set_symbol(" ");
    }
    buf.set_string(inner.x, inner.y, &header, head_style);
    let (prev_arrow, next_arrow) = paint_diff_nav_arrows(inner, head_bg, buf);

    // Two columns split exactly down the middle. Each column has a small
    // line-number gutter on its left and a 1-cell sign column showing
    // -, +, or space.
    let body_top = inner.y + 1;
    let body_height = inner.height.saturating_sub(2);
    let status_y = inner.y + inner.height - 1;
    if body_height == 0 {
        return (prev_arrow, next_arrow);
    }
    let half = inner.width / 2;
    if half < 8 {
        return (prev_arrow, next_arrow);
    }
    let left_max = diff.left_lines.len();
    let right_max = diff.right_lines.len();
    let l_gutter = (left_max + 1).to_string().len() as u16 + 1;
    let r_gutter = (right_max + 1).to_string().len() as u16 + 1;
    // Per-column layout: gutter + sign + content.
    let l_x = inner.x;
    let l_sign_x = l_x + l_gutter;
    let l_text_x = l_sign_x + 2;
    let l_text_w = half.saturating_sub(l_gutter + 2 + 1); // -1 spacer
    let r_x = inner.x + half + 1;
    let r_sign_x = r_x + r_gutter;
    let r_text_x = r_sign_x + 2;
    let r_text_w = (inner.width - (half + 1)).saturating_sub(r_gutter + 2);

    // Vertical seam between the two columns.
    let seam_x = inner.x + half;
    for y in body_top..body_top + body_height {
        buf[(seam_x, y)].set_symbol("\u{2502}");
        buf[(seam_x, y)].set_style(Style::default().fg(theme.ui(Color::Rgb(0x3a, 0x42, 0x52))));
    }

    let total = diff.rows.len();
    let viewport = body_height as usize;
    let max_scroll = total.saturating_sub(viewport);
    if diff.scroll > max_scroll {
        diff.scroll = max_scroll;
    }

    let removed_bg = theme.ui(Color::Rgb(0x4a, 0x1f, 0x1f));
    let removed_fg = theme.ui(Color::Rgb(0xff, 0xb3, 0xb3));
    let added_bg = theme.ui(Color::Rgb(0x1f, 0x42, 0x2a));
    let added_fg = theme.ui(Color::Rgb(0xb6, 0xee, 0xc4));
    let equal_fg = theme.ui(Color::Rgb(0xc5, 0xcd, 0xd9));
    let gutter_fg = theme.ui(Color::Rgb(0x6c, 0x7d, 0x9c));

    let find_needle = diff.find.needle.clone();
    let find_opts = diff.find.opts;
    let find_active = diff.find.active;
    // VS Code-style selection highlight: when a single word/run is selected,
    // every OTHER occurrence on screen gets a muted-blue box, mirroring the
    // plain editor. Painted per row below, under the brighter active band.
    let occ_needle = diff.selection_occurrence_needle();

    let end = (diff.scroll + viewport).min(total);
    for (vis_row, row_idx) in (diff.scroll..end).enumerate() {
        let y = body_top + vis_row as u16;
        // Paint what the ignore-whitespace toggle says; staging reads the real
        // rows independently (see DiffData::display_rows).
        let row = diff.display_rows()[row_idx];
        let (l_cell_bg, l_sign, l_text) = match row {
            DiffRow::Equal { left, .. } => (
                bg,
                ' ',
                diff.left_lines.get(left).cloned().unwrap_or_default(),
            ),
            DiffRow::Removed { left } => (
                removed_bg,
                '-',
                diff.left_lines.get(left).cloned().unwrap_or_default(),
            ),
            DiffRow::Replaced { left, .. } => (
                removed_bg,
                '-',
                diff.left_lines.get(left).cloned().unwrap_or_default(),
            ),
            DiffRow::Added { .. } => (added_bg, ' ', String::new()),
        };
        let (r_cell_bg, r_sign, r_text) = match row {
            DiffRow::Equal { right, .. } => (
                bg,
                ' ',
                diff.right_lines.get(right).cloned().unwrap_or_default(),
            ),
            DiffRow::Added { right } => (
                added_bg,
                '+',
                diff.right_lines.get(right).cloned().unwrap_or_default(),
            ),
            DiffRow::Replaced { right, .. } => (
                added_bg,
                '+',
                diff.right_lines.get(right).cloned().unwrap_or_default(),
            ),
            DiffRow::Removed { .. } => (removed_bg, ' ', String::new()),
        };

        // Left column.
        let l_left_idx = match row {
            DiffRow::Equal { left, .. }
            | DiffRow::Removed { left }
            | DiffRow::Replaced { left, .. } => Some(left),
            DiffRow::Added { .. } => None,
        };
        let l_label = l_left_idx
            .map(|i| format!("{:>width$} ", i + 1, width = l_gutter as usize - 1))
            .unwrap_or_else(|| " ".repeat(l_gutter as usize));
        buf.set_string(
            l_x,
            y,
            &l_label,
            Style::default().fg(gutter_fg).bg(l_cell_bg),
        );
        buf.set_string(
            l_sign_x,
            y,
            format!("{l_sign} "),
            Style::default()
                .fg(if l_cell_bg == removed_bg {
                    removed_fg
                } else {
                    equal_fg
                })
                .bg(l_cell_bg)
                .add_modifier(Modifier::BOLD),
        );
        let l_clipped: String = l_text
            .chars()
            .skip(diff.scroll_x)
            .take(l_text_w as usize)
            .collect();
        let mut l_padded = l_clipped.clone();
        let l_pad = (l_text_w as usize).saturating_sub(l_padded.chars().count());
        for _ in 0..l_pad {
            l_padded.push(' ');
        }
        buf.set_string(
            l_text_x,
            y,
            &l_padded,
            Style::default()
                .fg(if l_cell_bg == removed_bg {
                    removed_fg
                } else {
                    equal_fg
                })
                .bg(l_cell_bg),
        );

        // Right column.
        let r_right_idx = match row {
            DiffRow::Equal { right, .. }
            | DiffRow::Added { right }
            | DiffRow::Replaced { right, .. } => Some(right),
            DiffRow::Removed { .. } => None,
        };
        let r_label = r_right_idx
            .map(|i| format!("{:>width$} ", i + 1, width = r_gutter as usize - 1))
            .unwrap_or_else(|| " ".repeat(r_gutter as usize));
        buf.set_string(
            r_x,
            y,
            &r_label,
            Style::default().fg(gutter_fg).bg(r_cell_bg),
        );
        buf.set_string(
            r_sign_x,
            y,
            format!("{r_sign} "),
            Style::default()
                .fg(if r_cell_bg == added_bg {
                    added_fg
                } else {
                    equal_fg
                })
                .bg(r_cell_bg)
                .add_modifier(Modifier::BOLD),
        );
        let r_clipped: String = r_text
            .chars()
            .skip(diff.scroll_x)
            .take(r_text_w as usize)
            .collect();
        let mut r_padded = r_clipped.clone();
        let r_pad = (r_text_w as usize).saturating_sub(r_padded.chars().count());
        for _ in 0..r_pad {
            r_padded.push(' ');
        }
        buf.set_string(
            r_text_x,
            y,
            &r_padded,
            Style::default()
                .fg(if r_cell_bg == added_bg {
                    added_fg
                } else {
                    equal_fg
                })
                .bg(r_cell_bg),
        );

        // Inline-find highlight: overpaint every occurrence of the needle
        // on top of the text just laid down, lighting the active match in
        // a stronger colour. Reuses the plain editor's painter so the diff
        // and text views look identical under Cmd+F.
        if let Some(needle) = find_needle.as_deref() {
            use crate::widgets::diff::DiffSide;
            if l_left_idx.is_some() && l_text_w > 0 {
                let active = find_active
                    .filter(|a| a.row == row_idx && a.side == DiffSide::Left)
                    .map(|a| (a.col_chars, a.len_chars));
                paint_search_highlight(
                    buf,
                    l_text_x,
                    y,
                    l_text_w,
                    &l_text,
                    needle,
                    find_opts,
                    diff.scroll_x,
                    active,
                    &[],
                    None,
                    theme,
                );
            }
            if r_right_idx.is_some() && r_text_w > 0 {
                let active = find_active
                    .filter(|a| a.row == row_idx && a.side == DiffSide::Right)
                    .map(|a| (a.col_chars, a.len_chars));
                paint_search_highlight(
                    buf,
                    r_text_x,
                    y,
                    r_text_w,
                    &r_text,
                    needle,
                    find_opts,
                    diff.scroll_x,
                    active,
                    &[],
                    None,
                    theme,
                );
            }
        }

        // Selection-occurrence highlight: muted-blue box over every match of
        // the selected word on both columns. Painted after the find pass so
        // the two layers compose like the plain editor; the active selection
        // band overpaints these in a brighter blue below the row loop.
        if let Some(needle) = occ_needle.as_deref() {
            if l_left_idx.is_some() && l_text_w > 0 {
                paint_selection_occurrences(
                    buf,
                    l_text_x,
                    y,
                    l_text_w,
                    &l_text,
                    needle,
                    diff.scroll_x,
                    &[],
                    theme,
                );
            }
            if r_right_idx.is_some() && r_text_w > 0 {
                paint_selection_occurrences(
                    buf,
                    r_text_x,
                    y,
                    r_text_w,
                    &r_text,
                    needle,
                    diff.scroll_x,
                    &[],
                    theme,
                );
            }
        }
    }

    paint_diff_selection_band(
        diff,
        body_top,
        body_height,
        l_text_x,
        l_text_w,
        r_text_x,
        r_text_w,
        end,
        buf,
        theme,
    );

    // Status footer.
    let visible_first = diff.scroll + 1;
    let visible_last = end;
    let status = format!(
        " {visible_first}–{visible_last} of {total}  ·  ↑/↓ PgUp/PgDn  ·  ‹/› click arrows or F7 for next/prev change "
    );
    buf.set_string(
        inner.x,
        status_y,
        &status,
        Style::default()
            .fg(theme.ui(Color::Gray))
            .bg(theme.ui(Color::Rgb(0x14, 0x18, 0x22))),
    );
    (prev_arrow, next_arrow)
}

/// Overlay the diff's drag-select highlight on top of whatever the row
/// loop just painted. Walks the visible window once and paints the same
/// `paint_selection_band` overlay the regular text editor uses, so the
/// user sees an identical blue band over selected cells on whichever
/// column they're dragging in.
// Render helper: each argument is an independent painting input (data,
// geometry, target buffer); bundling them into a struct would add indirection
// without improving clarity.
#[allow(clippy::too_many_arguments)]
fn paint_diff_selection_band(
    diff: &crate::widgets::diff::DiffData,
    body_top: u16,
    body_height: u16,
    l_text_x: u16,
    l_text_w: u16,
    r_text_x: u16,
    r_text_w: u16,
    end: usize,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
) {
    use crate::widgets::diff::DiffSide;
    let Some(sel) = diff.selection else {
        return;
    };
    if !sel.has_area() {
        return;
    }
    let (start, stop) = sel.normalized();
    let (text_x, text_w) = match sel.side {
        DiffSide::Left => (l_text_x, l_text_w),
        DiffSide::Right => (r_text_x, r_text_w),
    };
    if text_w == 0 {
        return;
    }
    let first_visible = diff.scroll;
    let last_visible = end;
    let row_start = start.0.max(first_visible);
    let row_end = stop.0.min(last_visible.saturating_sub(1));
    if row_end < row_start {
        return;
    }
    for row_idx in row_start..=row_end {
        let y = body_top + (row_idx - first_visible) as u16;
        if y >= body_top + body_height {
            break;
        }
        let (cs, ce) = if start.0 == stop.0 {
            (start.1, stop.1)
        } else if row_idx == start.0 {
            (start.1, usize::MAX)
        } else if row_idx == stop.0 {
            (0, stop.1)
        } else {
            (0, usize::MAX)
        };
        let cs_screen = cs.saturating_sub(diff.scroll_x);
        let ce_screen = ce.saturating_sub(diff.scroll_x);
        paint_selection_band(buf, text_x, y, text_w, cs_screen, ce_screen, theme);
    }
}

/// Hit-test a click against the side-by-side diff body. Returns
/// `Some((side, diff_row_idx, char_col))` when the click landed inside
/// either column's text area; `None` when the click was in the header,
/// status footer, seam, gutter outside the body, or when the diff isn't
/// renderable at this size. The math mirrors `render_diff` exactly so a
/// click on a character returns the char column the user can see at that
/// cell.
pub fn diff_hit_test(
    diff: &crate::widgets::diff::DiffData,
    last_inner: Rect,
    col: u16,
    row: u16,
) -> Option<(crate::widgets::diff::DiffSide, usize, usize)> {
    use crate::widgets::diff::DiffSide;
    if diff.unified {
        return None;
    }
    if last_inner.width < 16 || last_inner.height < 3 {
        return None;
    }
    let body_top = last_inner.y + 1;
    let body_height = last_inner.height.saturating_sub(2);
    if body_height == 0 {
        return None;
    }
    if row < body_top || row >= body_top + body_height {
        return None;
    }
    let half = last_inner.width / 2;
    if half < 8 {
        return None;
    }
    let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
    let r_gutter = (diff.right_lines.len() + 1).to_string().len() as u16 + 1;
    let l_x = last_inner.x;
    let l_sign_x = l_x + l_gutter;
    let l_text_x = l_sign_x + 2;
    let l_text_w = half.saturating_sub(l_gutter + 2 + 1);
    let r_x = last_inner.x + half + 1;
    let r_sign_x = r_x + r_gutter;
    let r_text_x = r_sign_x + 2;
    let r_text_w = (last_inner.width - (half + 1)).saturating_sub(r_gutter + 2);
    let seam_x = last_inner.x + half;

    let vis_row = (row - body_top) as usize;
    let row_idx = diff.scroll + vis_row;
    if row_idx >= diff.rows.len() {
        return None;
    }

    let (side, text_x, text_w) = if col < seam_x {
        (DiffSide::Left, l_text_x, l_text_w)
    } else if col > seam_x {
        (DiffSide::Right, r_text_x, r_text_w)
    } else {
        return None;
    };
    let screen_col = if col >= text_x {
        (col - text_x).min(text_w.saturating_sub(1)) as usize
    } else {
        0
    };
    let char_col = screen_col + diff.scroll_x;
    Some((side, row_idx, char_col))
}

/// Paint `‹` and `›` glyphs at the right edge of a diff header so the
/// user can click between change hunks without scrolling. Returns the
/// hit-test rects (prev, next) so the click handler can route mouse
/// events back to `prev_change_row` / `next_change_row`. Returns two
/// empty rects when the header doesn't have room for the glyphs (very
/// narrow editor pane).
fn paint_diff_nav_arrows(inner: Rect, head_bg: Color, buf: &mut Buffer) -> (Rect, Rect) {
    // Reserve a 7-cell strip on the right: " ‹  ›  ".
    let strip_w: u16 = 7;
    if inner.width < strip_w + 4 {
        return (Rect::default(), Rect::default());
    }
    let strip_right = inner.x + inner.width;
    let prev_x = strip_right - strip_w + 1;
    let next_x = strip_right - 3;
    let y = inner.y;
    let arrow_style = Style::default()
        .fg(Color::White)
        .bg(head_bg)
        .add_modifier(Modifier::BOLD);
    buf.set_string(prev_x, y, "\u{2039}", arrow_style);
    buf.set_string(next_x, y, "\u{203a}", arrow_style);
    (
        Rect {
            x: prev_x,
            y,
            width: 1,
            height: 1,
        },
        Rect {
            x: next_x,
            y,
            width: 1,
            height: 1,
        },
    )
}

/// One-column unified diff for a deleted file. Every visible row is a
/// `Removed` row painted with a red band, `-` sign, and the HEAD line
/// number in the gutter — visually identical to `git diff` for a
/// removed file, but rendered inside the editor pane. Returns the
/// header-arrow hit rects the same way `render_diff` does so the click
/// handler can navigate deletion-view changes too.
fn render_unified_deletion(
    diff: &mut crate::widgets::diff::DiffData,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
) -> (Rect, Rect) {
    use crate::widgets::diff::DiffRow;
    let name = diff
        .left_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| diff.left_path.display().to_string());
    let header = format!(" diff: {name}  \u{2022} deleted (HEAD \u{2192} /dev/null) ");
    let head_style = Style::default()
        .fg(Color::White)
        .bg(theme.ui(Color::Rgb(0x6b, 0x1f, 0x1f)))
        .add_modifier(Modifier::BOLD);
    for x in inner.x..inner.x + inner.width {
        buf[(x, inner.y)].set_style(head_style);
        buf[(x, inner.y)].set_symbol(" ");
    }
    buf.set_string(inner.x, inner.y, &header, head_style);
    let (prev_arrow, next_arrow) =
        paint_diff_nav_arrows(inner, theme.ui(Color::Rgb(0x6b, 0x1f, 0x1f)), buf);

    let body_top = inner.y + 1;
    let body_height = inner.height.saturating_sub(2);
    let status_y = inner.y + inner.height - 1;
    if body_height == 0 {
        return (prev_arrow, next_arrow);
    }
    let left_max = diff.left_lines.len();
    let gutter = (left_max + 1).to_string().len() as u16 + 1;
    let sign_x = inner.x + gutter;
    let text_x = sign_x + 2;
    let text_w = inner.width.saturating_sub(gutter + 2);

    let total = diff.rows.len();
    let viewport = body_height as usize;
    let max_scroll = total.saturating_sub(viewport);
    if diff.scroll > max_scroll {
        diff.scroll = max_scroll;
    }

    let removed_bg = theme.ui(Color::Rgb(0x4a, 0x1f, 0x1f));
    let removed_fg = theme.ui(Color::Rgb(0xff, 0xb3, 0xb3));
    let gutter_fg = theme.ui(Color::Rgb(0x6c, 0x7d, 0x9c));

    let end = (diff.scroll + viewport).min(total);
    for (vis_row, row_idx) in (diff.scroll..end).enumerate() {
        let y = body_top + vis_row as u16;
        // Paint what the ignore-whitespace toggle says; staging reads the real
        // rows independently (see DiffData::display_rows).
        let row = diff.display_rows()[row_idx];
        let (left_idx, sign, cell_bg) = match row {
            DiffRow::Removed { left } | DiffRow::Replaced { left, .. } => {
                (Some(left), '-', removed_bg)
            }
            DiffRow::Equal { left, .. } => (Some(left), ' ', bg),
            DiffRow::Added { .. } => (None, ' ', bg),
        };
        let label = left_idx
            .map(|i| format!("{:>width$} ", i + 1, width = gutter as usize - 1))
            .unwrap_or_else(|| " ".repeat(gutter as usize));
        buf.set_string(
            inner.x,
            y,
            &label,
            Style::default().fg(gutter_fg).bg(cell_bg),
        );
        buf.set_string(
            sign_x,
            y,
            format!("{sign} "),
            Style::default()
                .fg(if cell_bg == removed_bg {
                    removed_fg
                } else {
                    gutter_fg
                })
                .bg(cell_bg)
                .add_modifier(Modifier::BOLD),
        );
        let text = left_idx
            .and_then(|i| diff.left_lines.get(i).cloned())
            .unwrap_or_default();
        let clipped: String = text
            .chars()
            .skip(diff.scroll_x)
            .take(text_w as usize)
            .collect();
        let mut padded = clipped;
        let pad = (text_w as usize).saturating_sub(padded.chars().count());
        for _ in 0..pad {
            padded.push(' ');
        }
        buf.set_string(
            text_x,
            y,
            &padded,
            Style::default()
                .fg(if cell_bg == removed_bg {
                    removed_fg
                } else {
                    theme.ui(Color::Rgb(0xc5, 0xcd, 0xd9))
                })
                .bg(cell_bg),
        );
    }

    let visible_first = diff.scroll + 1;
    let visible_last = end;
    let status = format!(
        " {visible_first}\u{2013}{visible_last} of {total}  \u{00b7}  every line removed at HEAD  \u{00b7}  \u{2191}/\u{2193} PgUp/PgDn to scroll "
    );
    buf.set_string(
        inner.x,
        status_y,
        &status,
        Style::default()
            .fg(theme.ui(Color::Gray))
            .bg(theme.ui(Color::Rgb(0x14, 0x18, 0x22))),
    );
    (prev_arrow, next_arrow)
}

fn write_cell(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str, style: Style) {
    let max_chars = w as usize;
    // Overflow is cut plainly with no trailing ellipsis marker (the user wants
    // no ellipsis anywhere in croft).
    let content: String = text.chars().take(max_chars).collect();
    let mut padded = content;
    let pad_count = max_chars.saturating_sub(padded.chars().count());
    for _ in 0..pad_count {
        padded.push(' ');
    }
    buf.set_string(x, y, &padded, style);
}

fn format_bytes_human(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

pub fn extension_is_image(ext: &str) -> bool {
    let lc = ext.to_ascii_lowercase();
    IMAGE_EXTENSIONS.iter().any(|e| *e == lc)
}

pub fn extension_is_pdf(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("pdf")
}

pub fn image_format_label_from_ext(ext: &str) -> String {
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "JPEG".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

/// Inclusive char-indexed range `(row, col)` anchor and head, where head
/// follows the cursor as the user drags / shift-arrows.  `normalised()` returns
/// the pair in row-major order so callers don't have to care which end came
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorSelection {
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

impl EditorSelection {
    pub fn new(row: usize, col: usize) -> Self {
        Self {
            anchor: (row, col),
            head: (row, col),
        }
    }
    pub fn normalised(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
    pub fn has_area(&self) -> bool {
        self.anchor != self.head
    }
}

/// One linked-editing span (#254): a single-line char-column range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LinkedRange {
    row: usize,
    start: usize,
    len: usize,
}

/// The Expand/Shrink Selection state (#254): per-cursor step stacks.
/// See the `select_expand` field doc for the validity contract.
struct SelectExpandStacks {
    /// The `edit_seq` the stacks were built against.
    edit_seq: u64,
    /// One stack per cursor: primary first, then `carets` in order.
    stacks: Vec<ExpandStack>,
}

/// One cursor's selection-growth history, smallest step first.
struct ExpandStack {
    steps: Vec<EditorSelection>,
    /// Index of the step the buffer currently shows.
    pos: usize,
}

/// A single text replacement in char-indexed `(row, col)` coordinates, as
/// produced by an LSP rename `WorkspaceEdit`. Applied by
/// [`apply_span_edits_to_lines`] and [`Editor::apply_span_edits`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSpanEdit {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub new_text: String,
}

/// Apply `edits` to `lines` (char-indexed coords), bottom-to-top so an
/// earlier replacement never shifts the coordinates of a later one. Returns
/// the number of edits applied. Out-of-range edits are skipped.
pub fn apply_span_edits_to_lines(lines: &mut Vec<String>, edits: &[TextSpanEdit]) -> usize {
    let mut order: Vec<&TextSpanEdit> = edits.iter().collect();
    order.sort_by_key(|e| std::cmp::Reverse(e.start));
    let mut applied = 0;
    for e in order {
        if replace_span(lines, e) {
            applied += 1;
        }
    }
    applied
}

fn replace_span(lines: &mut Vec<String>, e: &TextSpanEdit) -> bool {
    let (sr, sc) = e.start;
    let (er, ec) = e.end;
    if sr >= lines.len() || er >= lines.len() || (er, ec) < (sr, sc) {
        return false;
    }
    let from = char_byte(&lines[sr], sc);
    let to = char_byte(&lines[er], ec);
    let head = lines[sr][..from].to_string();
    let tail = lines[er][to..].to_string();
    // Build the replacement text in full, then split it back on '\n' so a
    // multi-line `new_text` (e.g. ruff's "Organize Imports", a whole-document
    // reformat) becomes separate rows instead of one row with embedded newline
    // bytes. A single-line edit (rename) yields exactly one row, unchanged.
    let combined = format!("{head}{}{tail}", e.new_text);
    let new_lines: Vec<String> = combined.split('\n').map(str::to_string).collect();
    let _ = lines.splice(sr..=er, new_lines);
    true
}

/// Coarse classification of the most recent edit, used so consecutive
/// `InsertChar` ops coalesce into a single undo step (typing burst) but
/// any other edit kind always opens a new step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    InsertChar,
    Newline,
    Backspace,
    DeleteForward,
    Paste,
    DeleteSelection,
    DuplicateLines,
    DeleteLines,
    KillToEol,
    KillToBol,
    OpenLine,
    /// Block indent / outdent (Tab / Shift+Tab over a line range). Never
    /// coalesces, so each Tab press is its own undo step like VS Code.
    Indent,
    /// A single keystroke applied simultaneously across every multi-cursor
    /// caret. Never coalesces, so each multi-edit is its own undo step.
    MultiEdit,
    /// Move a line / block up or down (Alt+Up / Alt+Down). Its own step.
    MoveLines,
    /// Toggle line or block comment (Cmd+/ / Shift+Alt+A). Its own step.
    ToggleComment,
    /// Join selected lines into one. Its own step.
    JoinLines,
    /// Upper / lower / title-case a selection. Its own step.
    TransformCase,
    /// Sort the selected lines. Its own step.
    SortLines,
    /// Trim trailing whitespace across the buffer. Its own step.
    TrimWhitespace,
    /// Transpose the two characters around the cursor (Ctrl+T). Its own step.
    Transpose,
    /// Convert leading indentation between tabs and spaces. Its own step.
    IndentConvert,
    /// Remove trailing blank lines at end of file. Its own step.
    TrimFinalNewlines,
    /// Find-bar Replace / Replace All. Never coalesces, so each replace is
    /// its own undo step like VS Code.
    Replace,
}

/// The three case transforms VS Code exposes as
/// `editor.action.transformTo{Upper,Lower,Title}case`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseTransform {
    Upper,
    Lower,
    Title,
}

/// A single keystroke to fan out across every multi-cursor caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaretEdit {
    Insert(char),
    Backspace,
    DeleteForward,
}

#[derive(Clone, Debug)]
struct Snapshot {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    selection: Option<EditorSelection>,
    carets: Vec<EditorSelection>,
    dirty: bool,
    /// The editor's `save_seq` when this snapshot was taken. A restore that
    /// crosses a save point re-dirties the buffer: its content no longer
    /// matches disk even though the snapshot predates the edit.
    save_seq: u64,
}

const UNDO_STACK_LIMIT: usize = 500;
/// Max selected-text length that still drives occurrence highlighting, matching
/// VS Code's default `editor.selectionHighlightMaxLength`.
const SELECTION_HIGHLIGHT_MAX_LEN: usize = 200;

/// Outcome of an FS-sync sweep over a single tab (`reload_or_flag_conflict`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalChange {
    /// Disk matches the buffer's last-synced stamp; nothing to do.
    Unchanged,
    /// The buffer was clean and has been reloaded from disk.
    Reloaded,
    /// Disk changed but the reload failed (e.g. the file became unreadable).
    ReloadFailed,
    /// The buffer had unsaved edits, so the external change was flagged as a
    /// conflict instead of being applied.
    Conflict,
}

/// Outcome of a guarded `save_to_disk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The buffer was written to disk.
    Saved,
    /// The file changed on disk since we last synced; the save was refused
    /// to avoid clobbering the external edit. The caller must force it.
    DiskConflict,
    /// The buffer holds characters the chosen encoding cannot represent, so
    /// writing would replace them with HTML numeric character references and
    /// lose the originals irreversibly (the buffer itself is unchanged, so
    /// undo cannot bring them back). The save was refused; the caller
    /// surfaces it and sets `lossy_save_armed` to write anyway.
    EncodingLoss,
}

/// Per-tab results of an FS-sync sweep across all open tabs
/// (`EditorTabs::reload_externally_changed_tabs`).
#[derive(Debug, Default)]
pub struct ExternalReloadReport {
    /// Paths of clean tabs that were silently reloaded from disk.
    pub reloaded: Vec<PathBuf>,
    /// Paths of dirty tabs flagged as conflicts (not reloaded, not saved).
    pub conflicts: Vec<PathBuf>,
    /// Paths whose disk copy changed but whose reload FAILED (the file
    /// became unreadable, was replaced by a directory, a PDF's re-render
    /// died). The tab keeps its last good view; dropping these on the
    /// floor left the user staring at stale content with no indication
    /// the sweep even tried (#37).
    pub failed: Vec<PathBuf>,
}

impl ExternalReloadReport {
    pub fn is_empty(&self) -> bool {
        self.reloaded.is_empty() && self.conflicts.is_empty() && self.failed.is_empty()
    }
}

/// The navigator's identity color: its comment boxes and its caret (the
/// same orange its note ◆ wore). Fixed like the git decoration colors —
/// legible on every dark background.
pub(crate) const NAVIGATOR_ACCENT: Color = Color::Rgb(0xff, 0x9d, 0x2f);
/// Columns of the comment-box footer tail ` ✕ Ignore ╯`.
const IGNORE_TAIL_COLS: usize = 11;

/// One comment box: the AI pair programmer's voice, anchored below a buffer
/// line. Rendered as an unnumbered block (title, body, reply field + Ignore
/// button); it never touches the buffer text and is never saved. The App
/// feeds these per tick from the pair host's note snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentBox {
    /// The note's stable id in the pilot's state (Ignore removes it, a
    /// reply appends to it).
    pub id: u64,
    /// 0-based buffer line the box hangs under.
    pub line: usize,
    /// The navigator's caret name, shown in the title row.
    pub author: String,
    /// Body text; replies append as further lines.
    pub body: String,
}

/// What a click inside a comment box landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentHit {
    /// The title or body area: focus the box.
    Body,
    /// The footer's reply field: focus and place the caret.
    Reply,
    /// The footer's ✕ Ignore button: dismiss the box.
    Ignore,
}

/// The reply draft in the focused box's footer field. While set, typing goes
/// here instead of the buffer (the App routes keys).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentFocus {
    /// Which box is focused (its [`CommentBox::id`]).
    pub id: u64,
    /// The draft text.
    pub reply: String,
    /// Caret position in chars within `reply`.
    pub cursor: usize,
}

/// One visual row of the painted layout: a buffer line's wrap segment
/// (`Text`), or one row of a comment box, which belongs to no buffer line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VisRow {
    /// `line`'s chars `[start, end)` (the whole visible span when not
    /// wrapping).
    Text {
        line: usize,
        start: usize,
        end: usize,
    },
    /// Row `box_row` (0-based within the block) of `comment_boxes[box_idx]`.
    Box { box_idx: usize, box_row: usize },
}

/// The line-ending style of the open buffer, shown in the status bar and
/// applied when saving. Detected on open (CRLF if any `\r\n` is present);
/// the user can switch it from the status bar, converting on the next save.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::Crlf => "CRLF",
        }
    }

    pub fn sequence(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

/// The buffer's indentation preference for newly typed indentation: the tab
/// width and whether Tab inserts spaces or a literal tab. Surfaced in the
/// status bar's "Spaces: N" / "Tab Size: N" pill, which the user can click to
/// change (VS Code's "Indent Using Spaces / Tabs" + "Change Tab Display Size").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IndentStyle {
    pub width: u32,
    pub use_spaces: bool,
}

/// Detect the dominant indentation of a buffer's lines (VS Code's
/// `editor.detectIndentation`). Tabs vs spaces is a straight majority of
/// indented lines; the space width is the most common positive step
/// between consecutive indentation depths (2..=8), falling back to the
/// smallest leading run when the file never nests. `None` when nothing is
/// indented, so the caller keeps the language default. Scans at most the
/// first 10k lines: enough signal for any real file, bounded for huge ones.
pub fn detect_indentation(lines: &[String]) -> Option<IndentStyle> {
    let mut tab_lines = 0usize;
    let mut space_lines = 0usize;
    let mut steps: [usize; 9] = [0; 9];
    let mut prev_depth = 0usize;
    let mut min_run = usize::MAX;
    for line in lines.iter().take(10_000) {
        if line.starts_with('\t') {
            // Whitespace-only lines carry no intent, same as the space
            // branch below: a blank line with leftover tab indentation
            // must not vote.
            if line.chars().any(|c| !c.is_whitespace()) {
                tab_lines += 1;
            }
            continue;
        }
        let depth = line.chars().take_while(|&c| c == ' ').count();
        if depth == 0 {
            if !line.is_empty() {
                prev_depth = 0;
            }
            continue;
        }
        if line.chars().nth(depth).is_none() {
            // Whitespace-only lines carry no intent.
            continue;
        }
        space_lines += 1;
        min_run = min_run.min(depth);
        let step = depth.abs_diff(prev_depth);
        if (2..=8).contains(&step) {
            steps[step] += 1;
        }
        prev_depth = depth;
    }
    if tab_lines == 0 && space_lines == 0 {
        return None;
    }
    if tab_lines >= space_lines {
        return Some(IndentStyle {
            width: 4,
            use_spaces: false,
        });
    }
    let width = steps
        .iter()
        .enumerate()
        .skip(2)
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(&a.0)))
        .filter(|&(_, &n)| n > 0)
        .map(|(w, _)| w)
        .or(((2..=8).contains(&min_run)).then_some(min_run))?;
    Some(IndentStyle {
        width: width as u32,
        use_spaces: true,
    })
}

impl IndentStyle {
    /// The indentation unit as text: `width` spaces, or a single tab.
    pub fn unit(self) -> String {
        if self.use_spaces {
            " ".repeat(self.width as usize)
        } else {
            "\t".to_string()
        }
    }

    /// Status-bar label, matching VS Code ("Spaces: 4" / "Tab Size: 4").
    pub fn label(self) -> String {
        let kind = if self.use_spaces {
            "Spaces"
        } else {
            "Tab Size"
        };
        format!("{kind}: {}", self.width)
    }
}

/// How a buffer line differs from its committed (HEAD) version, for the git
/// gutter. The colour carries the meaning, exactly like VS Code's gutter:
/// green added, blue modified, red where lines were deleted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitMark {
    Added,
    Modified,
    /// One or more lines were removed just above this surviving line (or at
    /// end of file, on the last line). VS Code paints a triangle here; the
    /// gutter shows a red bar on the boundary line.
    Deleted,
}

/// An in-progress snippet expansion: the caret is cycling the tab stops of a
/// just-expanded snippet. See [`Editor::expand_snippet`].
#[derive(Debug, Clone)]
struct SnippetSession {
    /// Start `(row, col)` of the current stop's placeholder.
    anchor: (usize, usize),
    /// Placeholder length (chars) of the current stop, selected on landing.
    cur_len: usize,
    /// Remaining stops to visit, in order: `(row, col, placeholder_len)`.
    stops: std::collections::VecDeque<(usize, usize, usize)>,
}

// Counts `rebuild_hidden_ranges` calls so a test can pin that the fold lookup
// is derived once per fold change and not per rendered row. Plain comment, not
// a doc comment: those cannot attach to a `thread_local!`.
#[cfg(test)]
thread_local! {
    static FOLD_RANGE_REBUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub struct Editor {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
    /// Which seat wrote each line (#349), for the gutter overlay and the
    /// inline blame annotation.
    ///
    /// Only edits croft can attribute are recorded — a line it did not watch
    /// being written stays unknown rather than inheriting from its
    /// neighbour, because a provenance overlay that is right most of the
    /// time gets read as fact and the line it gets wrong is the one someone
    /// is arguing about.
    pub provenance: crate::provenance::Provenance,
    /// Debugger breakpoints, keyed by file path, as 1-based line numbers.
    /// Rendered as red dots in the gutter and pushed to the DAP adapter on
    /// launch. Keyed by path (not just the active file) so switching the buffer
    /// to another file and back keeps its breakpoints.
    pub breakpoints: std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    /// Where the debugger is currently paused (path, 1-based line), or None when
    /// not stopped. Drawn as a highlighted row with a gutter arrow.
    pub stop_line: Option<(PathBuf, usize)>,
    /// Breakpoint lines the adapter reported as NOT verified (could not bind,
    /// e.g. a blank/comment line). Rendered as a hollow ○ instead of a solid ●
    /// so the user sees the breakpoint is inert. Keyed by path.
    pub unverified_breakpoints:
        std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    /// Optional condition expression per breakpoint line (path -> line ->
    /// condition). A conditional breakpoint only pauses when the expression is
    /// true. Rendered with a distinct gutter glyph.
    pub breakpoint_conditions:
        std::collections::HashMap<PathBuf, std::collections::HashMap<usize, String>>,
    /// Optional log message per breakpoint line (path -> line -> message): a
    /// logpoint. The adapter interpolates `{expr}` holes and prints instead
    /// of pausing. Rendered as an amber diamond in the gutter.
    pub breakpoint_logs:
        std::collections::HashMap<PathBuf, std::collections::HashMap<usize, String>>,
    /// Monotonic counter that bumps on every buffer mutation. The App's
    /// per-tick sync_lsp diff reads this to know when to forward a
    /// did_change to the LSP server, so building lines.join("\n") only
    /// happens on actual changes, not every frame.
    pub edit_seq: u64,
    /// The `edit_seq` the collab session last synced this buffer at (Phase D
    /// independent viewports, docs/MULTIPLAYER.md). The tick diff extracts
    /// ops only when `edit_seq` has moved past this, and it is re-pinned
    /// after a remote edit is applied so the diff never rebroadcasts one
    /// (same lazy-recompute pattern as `git_marks_seq`).
    pub collab_synced_seq: u64,
    /// The [`CollabDoc::text_gen`](crate::collab::CollabDoc::text_gen) this
    /// buffer last synced at, 0 = never attached to the live doc. A buffer
    /// created after its file went live (a split duplicate, a reopen) holds
    /// stale disk text: it must never extract (its diff would revert every
    /// peer) and is instead seeded from the replica when this lags.
    pub collab_doc_gen: u64,
    /// HEAD baseline for the git gutter: the committed version's lines. Set by
    /// the app (read off the workspace git root once per file / HEAD change).
    /// `None` when the file is untracked, outside a repo, or not yet fetched.
    pub git_head_lines: Option<Vec<String>>,
    /// The path `git_head_lines` was fetched for. The app refetches when this
    /// stops matching `path` (the tab switched files) so the gutter can never
    /// show another file's diff.
    pub git_baseline_for: Option<PathBuf>,
    /// Per 0-based buffer line, how it differs from HEAD. Recomputed from
    /// `git_head_lines` vs `lines` whenever `edit_seq` moves (the diff is a
    /// whole-file Myers pass, so it runs once per edit-batch, not per frame).
    git_marks: std::collections::HashMap<usize, GitMark>,
    /// The `edit_seq` `git_marks` was computed at; `u64::MAX` forces a first
    /// recompute once a baseline arrives.
    git_marks_seq: u64,
    /// Auto-closing pairs (#121): typing an opener/quote inserts the pair,
    /// closers type over, selections surround, and backspace eats an empty
    /// pair. Synced from the app's persisted preference like `blame_enabled`.
    pub auto_close_pairs: bool,
    /// The caret position and `edit_seq` recorded by the LAST auto-close
    /// insertion. Pair-backspace fires only while the caret still sits
    /// there with no edits since — a pre-existing `()` in the file must
    /// never lose both sides to one backspace (#122 review).
    auto_pair_at: Option<(usize, usize, u64)>,
    /// Merge-conflict blocks in the buffer, lazily recomputed whenever
    /// `edit_seq` moves (same pattern as the git-gutter marks).
    conflicts: Vec<crate::merge::ConflictBlock>,
    /// The `edit_seq` `conflicts` was computed at; `u64::MAX` forces the
    /// first scan.
    conflicts_seq: u64,
    /// Clickable accept-action spans painted on conflict header rows this
    /// frame: `(screen y, x range, header row, resolution)`. Cleared at
    /// render start so the hit test always describes the painted frame.
    pub merge_action_spans: Vec<(u16, std::ops::Range<u16>, usize, crate::merge::Resolution)>,
    /// Per-source-line git blame for the current file, index 0 = line 1. Set
    /// by the app off-thread once per (file, HEAD); `None` until fetched or
    /// when blame is disabled. Drives the GitLens-style current-line inline
    /// annotation.
    pub blame_lines: Option<Vec<crate::git::BlameLine>>,
    /// The path `blame_lines` was fetched for; the app refetches when this
    /// stops matching `path` so blame can never show another file's authors.
    pub blame_for: Option<PathBuf>,
    /// Whether to paint the current-line blame annotation (user pref, default
    /// on). The blame data is still fetched so toggling is instant.
    pub blame_enabled: bool,
    pub scroll: usize,
    /// In soft-wrap mode, the index of the first visible visual segment within
    /// the top logical line (`self.scroll`). Lets the viewport start partway
    /// through a paragraph that wraps to more rows than the pane is tall.
    /// Always 0 outside wrap mode.
    scroll_sub: usize,
    /// Cached total visual-row counts for soft-wrap, one `(width, total)` entry
    /// per width probed this frame (the renderer tests both the wide and the
    /// vbar-narrowed width, so a single slot would thrash). Cleared when the
    /// buffer changes (alongside `hscroll_content_cols`). Drives the vertical
    /// scrollbar's extent in wrap mode without an O(total chars) sweep/frame.
    wrap_total_cache: Vec<(usize, usize)>,
    /// Horizontal scroll offset in CHARACTERS. Long lines (e.g. minified
    /// JS, base64 blobs in HTML) need to scroll right so search hits and
    /// cursor positions deep in a line are reachable. The render slices
    /// each line at this column and `move_right`/`move_left` keep the
    /// cursor visible.
    pub scroll_col: usize,
    /// Cursor column as a CHARACTER index (not bytes), for the current line.
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub focused: bool,
    /// When focused, draw the orange→green gradient border (Black theme)
    /// instead of the solid blue one. Set by the app's focus/theme sync.
    pub focus_gradient: bool,
    /// Active color theme; drives the scrollbar track/thumb colors. Set by the
    /// app's theme sync.
    pub theme: crate::theme::Theme,
    /// Whether the bundled PDF and CSV viewer extensions are enabled. Set by the
    /// app from its disabled-extensions set; when off, `open` skips the inline
    /// viewer and falls through to opening the file as plain text. Default true
    /// so a freshly constructed editor (and every test) keeps the viewers on.
    pub pdf_viewer_enabled: bool,
    pub csv_viewer_enabled: bool,
    pub dirty: bool,
    /// Bumped on every SAVE (`mark_synced_with_disk`, reached only from
    /// `write_buffer_to_disk`). Undo snapshots record it so a restore across
    /// a save point re-dirties. Load/reload paths do not bump it; they clear
    /// the undo stacks instead, so no stale snapshot can span them — a
    /// change that preserves undo history across a reload must bump it too.
    save_seq: u64,
    pub status: String,
    pub last_area: Rect,
    pub last_inner: Rect,
    pub last_scrollbar: Rect,
    /// Hit-test rect for the horizontal scrollbar painted on the bottom inner
    /// row when the longest line overflows the text column. `Rect::default()`
    /// when no line overflows (the row is given back to text). `App` consults
    /// this on click/drag to map an X coordinate onto `scroll_col`.
    pub last_hscrollbar: Rect,
    /// Number of rows actually used for text in the last render, i.e.
    /// `inner.height` minus the horizontal scrollbar row when present. The
    /// vertical viewport math (page size, vertical thumb) reads this so the
    /// cursor never hides behind the horizontal bar.
    last_text_rows: u16,
    /// Cached longest-line width in CHARACTERS. Recomputing it every frame is
    /// O(total chars), so it is invalidated (set `None`) only when the buffer
    /// content changes (`open`/`mark_buffer_changed`) and recomputed lazily.
    hscroll_content_cols: Option<usize>,
    /// Visual-row layout captured by the most recent render: one entry per
    /// painted screen row, each `(logical_line, char_start, char_end)`. In
    /// soft-wrap mode a logical line spans several entries; otherwise there is
    /// one entry per visible line with the range `[scroll_col, scroll_col +
    /// text_width)`. `cursor_screen_pos`/`buffer_pos_at` invert it so the
    /// cursor and clicks land on the right cell regardless of wrapping.
    /// Comment-box rows appear inline as `VisRow::Box` entries.
    last_wrap_rows: Vec<VisRow>,
    pub last_gutter_width: u16,
    pub selection: Option<EditorSelection>,
    /// Secondary carets for multi-cursor editing (VS Code "Change All
    /// Occurrences"). The primary caret stays in `cursor_row`/`cursor_col`
    /// plus `selection`; these are the *extra* carets. Empty in the common
    /// single-cursor case, which keeps every existing edit path unchanged.
    /// Each entry is a full selection (anchor and head); head is that caret's
    /// cursor. Edits apply to the primary and all of these as one undo step.
    pub carets: Vec<EditorSelection>,
    /// Linked editing ranges (#254): equal-length single-line spans the
    /// server says rename together (paired HTML/JSX tags). While the
    /// caret edits inside one, `mirror_linked_edit` replays the change
    /// onto the others; anything structurally surprising (multi-row
    /// edit, invalid tag character, missed edits) drops the set — the
    /// next caret-idle round trip re-establishes it.
    linked_ranges: Vec<LinkedRange>,
    /// Char-length snapshot of every row a linked range sits on, taken
    /// when the set was installed/re-synced; the single changed row is
    /// how the mirror finds the edit and its length delta.
    linked_rows: Vec<(usize, usize)>,
    /// The `edit_seq` the stored positions describe. The mirror only
    /// trusts a single-step advance; anything else clears the set.
    linked_seq: u64,
    /// Cursor position at the last undo-push, i.e. where the last edit
    /// began (pre-edit coordinates) — the mirror's edit locator.
    pub last_edit_origin: (usize, usize),
    /// Expand/Shrink Selection stacks (#254): one per cursor (primary
    /// first, then `carets` in order), each recording the selections the
    /// gesture stepped through so shrink retraces exactly. Valid only
    /// while `edit_seq` matches and every cursor's CURRENT selection
    /// still equals its stack's current step — any edit, click, or
    /// caret reshuffle fails the check and the next gesture rebuilds
    /// from scratch (the `tick_occurrences` invalidation posture).
    select_expand: Option<SelectExpandStacks>,
    /// The last typed character and the `edit_seq` it produced — the
    /// on-type formatting trigger detector (#254). Stale once any other
    /// edit bumps the seq.
    pub last_typed: Option<(char, u64)>,
    /// Collaborators' caret positions in this file (multiplayer sessions,
    /// docs/MULTIPLAYER.md): (row, char col, participant color). Painted as
    /// colored block cells like secondary carets; the App rebuilds this
    /// before each render from the session roster.
    pub ghost_carets: Vec<(usize, usize, Color)>,
    /// Name tags for ghost carets still inside their fade window (VS Code
    /// Live Share style): (caret row, caret char col, name, participant
    /// color). Painted on the visual row above the caret, falling back
    /// below when the caret sits on the viewport's top row. The App
    /// rebuilds this before each render alongside `ghost_carets`.
    pub ghost_caret_labels: Vec<(usize, usize, String, Color)>,
    /// The 0-based buffer line wearing the AI-stream stop button in the
    /// sign margin (`croft pair`): clicking it — or Cmd+K X — cancels the
    /// stream and reverts the streamed text. The App rebuilds this before
    /// each render from the stream state and the pilot's ghost caret.
    pub stream_stop_line: Option<usize>,
    /// The AI pair programmer's comment boxes for this buffer: each renders
    /// as an unnumbered block between its anchor line and the next line
    /// (title, body, reply field + Ignore button). The App rebuilds this per
    /// tick from the pilot host's anchored-note snapshot for the active
    /// file; boxes belong to no buffer line and never touch the text.
    pub comment_boxes: Vec<CommentBox>,
    /// The reply draft riding the focused box's footer field. While set,
    /// the App routes typing here instead of the buffer.
    pub comment_focus: Option<CommentFocus>,
    /// Enclosing scope header lines to pin at the top of the viewport (VS Code
    /// "Sticky Scroll"), outermost first, set by `App` from the outline scope
    /// chain of the top visible line. Empty disables the feature (e.g. wrap
    /// mode, no symbols, or nothing scrolled off).
    pub sticky_lines: Vec<u32>,
    /// Screen `(row, logical line)` pairs the sticky bar painted last render, so
    /// `App` can map a click on a pinned header to a jump.
    sticky_click_rows: Vec<(u16, u32)>,
    /// Anchor of an in-progress column (box) selection (VS Code's Shift+Alt+drag),
    /// as a `(row, col)` buffer position. `Some` while the box drag is live; each
    /// drag rebuilds `carets` from the rectangle between it and the pointer.
    box_anchor: Option<(usize, usize)>,
    /// Active snippet tab-stop session (VS Code snippet expansion). `Some` while
    /// the caret is cycling through `$1`, `$2`, … stops of a just-expanded
    /// snippet; Tab advances, and any structural key cancels it. See
    /// [`Editor::expand_snippet`].
    snippet: Option<SnippetSession>,
    /// Currently-collapsed fold headers, as 0-based logical line indexes. A
    /// header's body (the more-indented lines below it, up to `fold_range`'s
    /// end) is hidden from the render while its index is in this set. Purely a
    /// view: the buffer is untouched, so every edit/undo path ignores it.
    folded: std::collections::BTreeSet<usize>,
    /// Merged, ascending, disjoint spans of lines hidden by `folded`, rebuilt
    /// only when the fold set or the buffer changes. `is_line_hidden` runs on
    /// every rendered row of every frame, and deriving it from `folded` there
    /// meant re-scanning each folded region per row — Fold All on a large file
    /// turned one frame into millions of iterations.
    hidden_ranges: Vec<(usize, usize)>,
    /// `self.lines.len()` at the moment `folded` was last set. Fold headers are
    /// line indexes, so an insert/delete anywhere shifts them; when the count
    /// no longer matches, the render drops every fold rather than hide the
    /// wrong lines. ponytail: whole-buffer invalidation, not per-fold anchor
    /// tracking; upgrade to sticky anchors if folds-survive-edits is wanted.
    fold_epoch_lines: usize,
    /// Server fold spans (#254), authoritative for `fold_range` while
    /// present AND measured against the current line count. `None` (no
    /// capable server, or none answered yet) keeps the indentation /
    /// marker fallback in charge — the two-phase posture of the outline.
    lsp_folds: Option<Vec<crate::lsp::manager::FoldingRangeItem>>,
    /// `self.lines.len()` when `lsp_folds` was applied; a mismatch means
    /// the spans predate an edit and are ignored until the next reply.
    lsp_folds_lines: usize,
    /// Fallback fold table beyond plain indentation: `#region` /
    /// `// region` marker pairs (kind Region) and runs of full-line
    /// comments (kind Comment). Rebuilt lazily per `edit_seq` by
    /// `refresh_fold_tables`, read by `fold_range` / `fold_kind_at`.
    fallback_kind_folds: Vec<(usize, usize, crate::lsp::manager::FoldRangeKind)>,
    /// The `edit_seq` `fallback_kind_folds` was built for (`None` = never).
    fallback_folds_seq: Option<u64>,
    /// True when this tab is the single replaceable "preview" slot. Single-
    /// click / plain-Enter opens replace the preview tab's contents in place;
    /// double-click / Ctrl+Enter / typing into the buffer pin the tab
    /// (preview = false) so subsequent previews don't overwrite it.
    pub preview: bool,
    /// True when the user has pinned this tab (VS Code "Pin"). Pinned tabs are
    /// kept leftmost in the strip, survive Close Others / Close to the Right,
    /// and show a thumb-tack glyph in place of the close `\u{2715}`. A pinned
    /// tab is never the replaceable preview slot (pinning clears `preview`).
    pub pinned: bool,
    undo_stack: Vec<Snapshot>,
    /// States popped off `undo_stack` by `undo`, awaiting `redo`. Cleared by any
    /// fresh edit (`push_undo`) so a new edit branches history like VS Code.
    redo_stack: Vec<Snapshot>,
    last_edit_kind: Option<EditKind>,
    lang: Option<LangKind>,
    /// Explicit indentation preference set from the status-bar pill. `None`
    /// falls back to the language default (2 spaces for YAML, 4 otherwise);
    /// `Some` pins spaces-vs-tabs and width for newly typed indentation.
    indent_override: Option<IndentStyle>,
    /// Indentation detected from the buffer's own content on open (VS
    /// Code's `editor.detectIndentation`): `Some` when the file showed a
    /// clear preference, `None` when it gave no signal. The manual
    /// status-bar override always wins over this.
    detected_indent: Option<IndentStyle>,
    /// Line-ending style, detected on open and applied on save. Surfaced in the
    /// status bar; the user can switch it there.
    pub eol: LineEnding,
    /// Text encoding the buffer was decoded from and is re-encoded to on save.
    /// Defaults to UTF-8; the status bar's "Reopen with Encoding" switches it.
    pub encoding: &'static encoding_rs::Encoding,
    /// Whether the file on disk began with a byte-order mark. `decode` strips
    /// it, so without remembering it every save silently dropped it.
    bom: bool,
    /// Indentation guides (VS Code `editor.guides.indentation`): dim vertical
    /// lines at each indent level in a line's leading whitespace, with the
    /// cursor's block highlighted. App-synced from prefs; on by default.
    pub show_indent_guides: bool,
    /// Bracket-pair colorization (#131, VS Code on-by-default since 1.67):
    /// per-line `(char column, colour index)` pairs for every `()[]{}` outside
    /// strings and comments, colour cycling by nesting depth
    /// (`UNEXPECTED_BRACKET` marks an unmatched closer). Rebuilt with the
    /// syntax spans in `recompute_highlights` — one linear scan per edit,
    /// never per frame. App-synced from prefs; on by default.
    pub show_bracket_colors: bool,
    bracket_colors: Vec<Vec<(usize, u8)>>,
    /// Whitespace glyph rendering (#133); app-synced from prefs.
    pub whitespace_mode: WhitespaceMode,
    /// Starting ignore-whitespace mode for diffs opened in this editor
    /// (#258, `diff.ignore_whitespace`); app-synced from prefs. Note this is
    /// unrelated to `whitespace_mode` above, which paints glyphs.
    pub diff_ws_default: crate::widgets::diff::DiffWhitespace,
    /// Debugger inline values (#135, VS Code `debug.inlineValues`): 0-based
    /// line → composed "name = value" trailer, rebuilt by the app on every
    /// stop from the VARIABLES data and cleared on resume/step/terminate.
    /// Painted like the blame trailer; the cursor-line blame yields to it.
    pub inline_values: std::collections::BTreeMap<usize, String>,
    /// Per-tab override for soft-wrap (VS Code "View: Toggle Word Wrap",
    /// Alt+Z). `None` means follow the language default (`wrap_enabled`
    /// wraps Markdown only); `Some(true)`/`Some(false)` force it on/off for
    /// this buffer. Reset to `None` on every `open` so each file starts at
    /// its language default.
    wrap_override: Option<bool>,
    highlights: Vec<Vec<HiSpan>>,
    /// LSP semantic-token overlay decoded per line (byte offsets within
    /// the line), painted over `highlights` at render so a parameter (and
    /// other resolved symbols) keep their color everywhere they appear,
    /// not just at the declaration. Empty until the server replies. This
    /// is the editor half of the VS Code / Zed "combined" model.
    semantic_overlay: Vec<Vec<HiSpan>>,
    /// The raw last semantic-token batch (relative-encoded data + the
    /// server legend), retained so the overlay can be re-decoded against
    /// the buffer after each edit. `semantic_path` records which file the
    /// batch is for, so a stale batch is ignored after a tab switch.
    semantic_data: Vec<u32>,
    semantic_legend: Option<std::sync::Arc<Vec<String>>>,
    semantic_path: Option<PathBuf>,
    /// Whether the retained batch covers the WHOLE document (a
    /// `semanticTokens/full` reply) rather than just the opening viewport (a
    /// `semanticTokens/range` reply). Once a full batch lands for a file, a
    /// late range reply for that same file is dropped so it cannot blank the
    /// off-screen colour the full batch painted.
    semantic_is_full: bool,
    /// Diagnostics for the loaded file, retained with their raw LSP UTF-16
    /// positions so `diagnostic_spans` can be re-decoded against the buffer.
    /// `diagnostics_path` guards a stale batch from underlining the wrong file
    /// after a tab switch, exactly as `semantic_path` does for tokens.
    diagnostics: Vec<crate::lsp::manager::Diagnostic>,
    diagnostics_path: Option<PathBuf>,
    /// Decoded per-logical-line underline runs `(start_char, end_char,
    /// severity)`, recomputed from `diagnostics` whenever they or the buffer
    /// change. The render loop paints a coloured underline over each run.
    diagnostic_spans: Vec<Vec<(usize, usize, crate::lsp::manager::DiagnosticSeverity)>>,
    /// Inlay hints for the loaded file, retained with their raw LSP UTF-16
    /// positions so `inlay_spans` can be re-decoded against the buffer after
    /// each edit. `inlay_path` guards a stale batch from annotating the wrong
    /// file after a tab switch, exactly as `diagnostics_path` does.
    inlay_hints: Vec<crate::lsp::manager::InlayHintItem>,
    inlay_path: Option<PathBuf>,
    /// Occurrences of the symbol under the caret (LSP documentHighlight),
    /// already converted to `(row, start_char, end_char, write)` against the
    /// current buffer. Cleared on every edit — the server's columns are
    /// stale the moment the text changes — and replaced when the app's next
    /// idle-caret request answers.
    occurrences: Vec<(usize, usize, usize, bool)>,
    /// Decoded per-logical-line virtual-cell runs `(char_col, label,
    /// swatch)`, sorted by column: LSP inlay hints (swatch `None`, dim
    /// italic) and document-color swatches (#254: a `■` whose fg IS the
    /// color). The render loop splices each label into the row at its
    /// anchor; every overlay painter, the caret, and mouse mapping
    /// translate buffer columns past them.
    inlay_spans: Vec<Vec<(usize, String, Option<Color>)>>,
    /// The server's resolved document links (#254): raw UTF-16 ranges +
    /// target URIs, path-stamped like `inlay_path`. Consulted by the
    /// Ctrl+click dispatch BEFORE firing Go to Definition.
    doc_links: Vec<crate::lsp::manager::DocumentLinkItem>,
    doc_links_path: Option<PathBuf>,
    /// The document's color values (#254): raw UTF-16 positions + RGB,
    /// re-decoded like `inlay_hints`; feeds the `■` swatch spans and the
    /// Change Color Presentation picker's range lookup.
    color_infos: Vec<crate::lsp::manager::ColorItem>,
    /// The file `color_infos` describes (same contract as `inlay_path`).
    color_path: Option<PathBuf>,
    registry: LangRegistry,
    /// When set, every occurrence of this string in the visible portion of
    /// the buffer is overpainted with the search-match style after the
    /// syntax-highlighted line is laid down. Mirrors the highlight in the
    /// Search panel so the user sees their query lit up in the file too.
    pub search_highlight: Option<String>,
    /// Toggle state that drives `search_highlight` matching: case
    /// sensitivity, whole-word boundaries, regex. Mirrors `SearchPanel.opts`
    /// so the editor's yellow highlight stays consistent with what the
    /// search panel claims is a match.
    pub search_highlight_opts: crate::widgets::search::SearchOpts,
    /// When the inline editor Find overlay (Cmd+F) is open and the cursor
    /// has landed on a match, this records (row, col_chars, len_chars) so
    /// the renderer can paint the active match in a stronger orange while
    /// every other match keeps the regular yellow. Mirrors VS Code's
    /// "current match" highlight. Cleared when the find bar closes or when
    /// the highlight term is cleared.
    pub active_search_match: Option<(usize, usize, usize)>,
    /// Some when this tab is a read-only image preview rather than a text
    /// buffer. The text fields (`lines`, undo, highlights, …) are left in
    /// their default empty state and the renderer paints metadata only;
    /// the actual pixels are emitted as an OSC-1337 inline image overlay
    /// by `App` after each frame.
    /// Rendered Markdown preview (Cmd/Ctrl+Shift+V) replacing the source view
    /// while set; rebuilt lazily when `built_seq` falls behind `edit_seq`.
    pub markdown_preview: Option<crate::markdown::MarkdownPreview>,
    /// Captured runs to show under their fences in the preview (#354).
    /// Owned by the app and copied in before a rebuild, because the editor
    /// has no view of which panes ran what.
    pub md_outputs: crate::markdown::BlockOutputs,
    pub image: Option<ImageView>,
    /// A reload's request that `open_pdf` come up on this page instead of
    /// page 1, so the reader's place survives an external rebuild in ONE
    /// rasterisation. Rendering page 1 first and re-rendering afterwards
    /// opened a window where a transient failure silently snapped the
    /// reader back to page 1 (#72). Set by `reload_from_disk`, consumed
    /// (or cleared) by the `open` it wraps.
    pdf_restore_page: Option<u32>,
    /// Read-only spreadsheet preview for `.csv` / `.tsv` / `.xlsx` / etc.
    /// Mutually exclusive with `image` and the text path; none of the
    /// editor's text fields are populated when this is `Some`.
    pub sheet: Option<crate::sheet::SheetView>,
    /// Read-only side-by-side diff view. Mutually exclusive with the text
    /// path, `image`, and `sheet` — when set the renderer paints two
    /// columns based on `diff.rows` and ignores `lines`.
    pub diff: Option<crate::widgets::diff::DiffData>,
    /// Read-only hex viewer (#172): the routing fallback for every file
    /// the text heuristic rejects, and the explicit "Reopen as Hex"
    /// target. Mutually exclusive with the text path and the other
    /// preview kinds; windowed IO, so the file is never loaded whole.
    pub hex: Option<crate::hex::HexView>,
    /// Rendered ANSI log view (#257): colour-bearing logs paint with the
    /// theme's ANSI palette instead of showing raw escapes. Windowed like
    /// [`crate::hex`], so a multi-gigabyte log opens instantly. Read-only;
    /// "Reopen as Text" (`force_text`) shows the raw bytes.
    pub log: Option<crate::log_view::LogView>,
    /// Archive browser (#179): the member list of a zip/jar/whl/tar
    /// archive; Enter extracts one member to scratch and opens it
    /// through the normal dispatch. Read-only.
    pub archive: Option<crate::archive::ArchiveView>,
    /// Three-way merge editor (#253). UNLIKE the other view kinds this is
    /// not read-only and not in `has_non_text_view`: `lines` holds the
    /// editable Result and keeps the whole text path (LSP, undo, save);
    /// the renderer only carves the source panes off the top of the area.
    pub merge: Option<crate::merge_editor::MergeView>,
    /// Cursor row at the last undo-push (i.e. where the last edit began);
    /// the merge editor's region tracker keys its shift heuristic off it.
    pub merge_edit_row: usize,
    /// Per-tab "Reopen as Text" override (#175): when set, `open` skips
    /// every preview route (extension and sniffed alike) and lands in
    /// the text editor — an SVG's XML source, a workbook's bytes (which
    /// the binary heuristic then sends to hex). It STICKS across
    /// same-path reloads, so the FS-sync sweep cannot flip the tab back
    /// to a preview, and clears when the tab opens a different file.
    pub force_text: bool,
    /// Hit-test rect for the "previous change" arrow painted in the diff
    /// header. Empty when the tab isn't a diff or the header was clipped.
    /// `App` consults this on left-click to jump to the previous hunk.
    pub diff_prev_arrow: Rect,
    /// Hit-test rect for the "next change" arrow painted in the diff
    /// header. Mirror of `diff_prev_arrow`.
    pub diff_next_arrow: Rect,
    /// Disk identity (mtime, len) captured the last time this tab was in
    /// sync with disk — i.e. at open and after a successful save. The FS
    /// sync sweep compares the file's current stamp against this to decide
    /// whether an external process changed the file underneath us. `None`
    /// for a tab with no file (the blank initial buffer).
    disk_stamp: Option<(SystemTime, u64)>,
    /// Set when an external on-disk change is detected while this buffer has
    /// unsaved edits. Reloading would discard the user's edits and saving
    /// would clobber the external change, so neither happens automatically:
    /// the flag drives the conflict warning and makes `save_to_disk` refuse
    /// to overwrite until the user forces it.
    pub disk_conflict: bool,
    /// Set when a save was refused because `encoding` cannot represent the
    /// buffer. Latches like `disk_conflict` so auto save stops retrying (and
    /// stops re-reporting) a file only an explicit Cmd+S can resolve; cleared
    /// by a successful write and by any reopen, which may change `encoding`.
    pub encoding_loss: bool,
    /// One-shot consent to write characters `encoding` cannot represent,
    /// answering an `EncodingLoss` refusal. Set by the explicit-save path
    /// only (never by auto save) and consumed by the write it authorises.
    pub lossy_save_armed: bool,
    /// When the buffer last changed, driving the auto-save delay. `None`
    /// until the first edit.
    pub last_edit_at: Option<std::time::Instant>,
    /// The caret position of the last buffer edit, for "Go to Last Edit
    /// Location" (Cmd+K Cmd+Q). Set beside `last_edit_at` in
    /// `mark_buffer_changed`.
    pub last_edit_pos: Option<(usize, usize)>,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            path: None,
            lines: Vec::new(),
            provenance: crate::provenance::Provenance::new(),
            breakpoints: std::collections::HashMap::new(),
            stop_line: None,
            unverified_breakpoints: std::collections::HashMap::new(),
            breakpoint_conditions: std::collections::HashMap::new(),
            breakpoint_logs: std::collections::HashMap::new(),
            edit_seq: 0,
            collab_synced_seq: 0,
            collab_doc_gen: 0,
            git_head_lines: None,
            git_baseline_for: None,
            git_marks: std::collections::HashMap::new(),
            git_marks_seq: u64::MAX,
            auto_close_pairs: true,
            auto_pair_at: None,
            conflicts: Vec::new(),
            merge_action_spans: Vec::new(),
            conflicts_seq: u64::MAX,
            blame_lines: None,
            blame_for: None,
            blame_enabled: true,
            scroll: 0,
            scroll_sub: 0,
            wrap_total_cache: Vec::new(),
            scroll_col: 0,
            cursor_row: 0,
            cursor_col: 0,
            focused: false,
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            pdf_viewer_enabled: true,
            csv_viewer_enabled: true,
            dirty: false,
            save_seq: 0,
            status: String::from("No file open"),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            last_scrollbar: Rect::default(),
            last_hscrollbar: Rect::default(),
            last_text_rows: 0,
            hscroll_content_cols: None,
            last_wrap_rows: Vec::new(),
            last_gutter_width: 0,
            selection: None,
            carets: Vec::new(),
            linked_ranges: Vec::new(),
            linked_rows: Vec::new(),
            linked_seq: 0,
            last_edit_origin: (0, 0),
            select_expand: None,
            last_typed: None,
            ghost_carets: Vec::new(),
            ghost_caret_labels: Vec::new(),
            stream_stop_line: None,
            comment_boxes: Vec::new(),
            comment_focus: None,
            sticky_lines: Vec::new(),
            sticky_click_rows: Vec::new(),
            box_anchor: None,
            snippet: None,
            folded: std::collections::BTreeSet::new(),
            hidden_ranges: Vec::new(),
            fold_epoch_lines: 0,
            lsp_folds: None,
            lsp_folds_lines: 0,
            fallback_kind_folds: Vec::new(),
            fallback_folds_seq: None,
            preview: false,
            pinned: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit_kind: None,
            lang: None,
            indent_override: None,
            detected_indent: None,
            eol: LineEnding::Lf,
            encoding: encoding_rs::UTF_8,
            bom: false,
            show_indent_guides: true,
            show_bracket_colors: true,
            bracket_colors: Vec::new(),
            whitespace_mode: WhitespaceMode::default(),
            diff_ws_default: crate::widgets::diff::DiffWhitespace::default(),
            inline_values: std::collections::BTreeMap::new(),
            wrap_override: None,
            highlights: Vec::new(),
            semantic_overlay: Vec::new(),
            semantic_data: Vec::new(),
            semantic_legend: None,
            semantic_path: None,
            semantic_is_full: false,
            diagnostics: Vec::new(),
            diagnostics_path: None,
            diagnostic_spans: Vec::new(),
            inlay_hints: Vec::new(),
            inlay_path: None,
            occurrences: Vec::new(),
            inlay_spans: Vec::new(),
            doc_links: Vec::new(),
            doc_links_path: None,
            color_infos: Vec::new(),
            color_path: None,
            registry: LangRegistry::new(),
            search_highlight: None,
            search_highlight_opts: crate::widgets::search::SearchOpts::default(),
            active_search_match: None,
            markdown_preview: None,
            md_outputs: crate::markdown::BlockOutputs::new(),
            image: None,
            pdf_restore_page: None,
            sheet: None,
            diff: None,
            hex: None,
            log: None,
            archive: None,
            merge: None,
            merge_edit_row: 0,
            force_text: false,
            diff_prev_arrow: Rect::default(),
            diff_next_arrow: Rect::default(),
            disk_stamp: None,
            disk_conflict: false,
            encoding_loss: false,
            lossy_save_armed: false,
            last_edit_at: None,
            last_edit_pos: None,
        }
    }

    pub fn set_search_highlight(&mut self, term: Option<String>) {
        self.search_highlight = term.filter(|s| !s.is_empty());
    }

    /// Toggle a debugger breakpoint on the cursor's current line for the active
    /// file. Returns `(path, 1-based line, now_set)` where `now_set` is `true`
    /// if a breakpoint was just added and `false` if one was removed, or `None`
    /// when no file is open. Empty sets are dropped so the map stays clean for
    /// the gutter renderer.
    pub fn toggle_breakpoint(&mut self) -> Option<(PathBuf, usize, bool)> {
        self.toggle_breakpoint_line(self.cursor_row + 1) // gutter + DAP are 1-based
    }

    /// Toggle a breakpoint on an explicit 1-based `line` (rather than the
    /// cursor). Drives the gutter right-click "Add / Remove Breakpoint" menu,
    /// where the target line is the row the user clicked, not where the caret
    /// happens to sit. Returns `(path, line, now_set)`, or `None` with no open
    /// file.
    pub fn toggle_breakpoint_line(&mut self, line: usize) -> Option<(PathBuf, usize, bool)> {
        let path = self.path.clone()?;
        let set = self.breakpoints.entry(path.clone()).or_default();
        let now_set = if set.remove(&line) {
            // Removing a breakpoint removes the whole breakpoint: an
            // orphaned condition or log message would silently re-attach to
            // the next plain breakpoint set on this line (a resurrected
            // logpoint never pauses at all).
            if let Some(conds) = self.breakpoint_conditions.get_mut(&path) {
                conds.remove(&line);
                if conds.is_empty() {
                    self.breakpoint_conditions.remove(&path);
                }
            }
            if let Some(logs) = self.breakpoint_logs.get_mut(&path) {
                logs.remove(&line);
                if logs.is_empty() {
                    self.breakpoint_logs.remove(&path);
                }
            }
            false
        } else {
            set.insert(line);
            true
        };
        if set.is_empty() {
            self.breakpoints.remove(&path);
        }
        Some((path, line, now_set))
    }

    /// The 0-based buffer line under screen `(col, row)`, but ONLY when `col`
    /// falls in the left gutter (glyph margin + line-number columns) rather
    /// than the text body. Returns `None` over the body, past the last line,
    /// or outside the viewport. Complements [`Editor::buffer_pos_at`] (which
    /// resolves body clicks) and drives the gutter right-click breakpoint menu,
    /// mirroring VS Code's glyph-margin context menu.
    pub fn gutter_line_at(&self, col: u16, row: u16) -> Option<usize> {
        if self.last_inner.height == 0 || self.lines.is_empty() {
            return None;
        }
        if row < self.last_inner.y || row >= self.last_inner.y + self.last_inner.height {
            return None;
        }
        // text_x mirrors `buffer_pos_at`: the gutter occupies every column to
        // its left, so `col < text_x` is exactly "in the gutter".
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        if col < self.last_inner.x || col >= text_x {
            return None;
        }
        // `last_wrap_rows` is the row->line map the render built, which already
        // skips folded-away lines, so it is correct for both wrap and fold.
        // Before the first paint it is empty; fall back to the linear map, which
        // is exact then since nothing can be wrapped or folded yet.
        let vis_row = (row - self.last_inner.y) as usize;
        let line = if self.last_wrap_rows.is_empty() {
            self.scroll + vis_row
        } else {
            self.text_row(vis_row)?.0
        };
        (line < self.lines.len()).then_some(line)
    }

    /// If `(col, row)` lands on a fold chevron in the gutter, toggle that fold
    /// and return `true`. The chevron sits at `inner.x + 1` on a foldable
    /// header's first visual row (see the render's fold-chevron block).
    pub fn fold_chevron_at(&mut self, col: u16, row: u16) -> bool {
        if col != self.last_inner.x + 1 || row < self.last_inner.y {
            return false;
        }
        let Some((line, start, _)) = self.text_row((row - self.last_inner.y) as usize) else {
            return false;
        };
        // The chevron draws only on a header's first visual row.
        if self.wrap_enabled() && start != 0 {
            return false;
        }
        if self.is_foldable(line) {
            self.toggle_fold(line);
            return true;
        }
        false
    }

    /// If `(col, row)` lands on a test fn's gutter play glyph, the test's
    /// name — the app runs it. Same geometry as the fold chevron, one cell
    /// left (`inner.x`, the sign margin the breakpoint dot shares).
    pub fn test_glyph_at(&self, col: u16, row: u16) -> Option<String> {
        if col != self.last_inner.x || row < self.last_inner.y {
            return None;
        }
        let (line, start, _) = self.text_row((row - self.last_inner.y) as usize)?;
        // The glyph draws only on the definition's first visual row.
        if self.wrap_enabled() && start != 0 {
            return None;
        }
        // Render precedence: the stop arrow, breakpoint glyphs, and the
        // AI-stream square all outrank the play bead in the shared sign
        // cell — a click on one of those glyphs must not start a test run.
        if self.sign_cell_taken(line) {
            return None;
        }
        crate::testing::locate::test_fn_on_line(self.path.as_deref(), &self.lines, line)
    }

    /// Whether the sign cell of 0-based `line` is claimed by a glyph that
    /// outranks the test play bead: the debugger's stop arrow, any
    /// breakpoint glyph (dot, diamond, hollow ring), or the AI-stream stop
    /// square — the same precedence the render pass applies.
    fn sign_cell_taken(&self, line: usize) -> bool {
        if self.stream_stop_line == Some(line) {
            return true;
        }
        let Some(path) = self.path.as_deref() else {
            return false;
        };
        let here = line + 1; // gutter is 1-based
        self.stop_line
            .as_ref()
            .is_some_and(|(p, l)| p == path && *l == here)
            || self
                .breakpoints
                .get(path)
                .is_some_and(|s| s.contains(&here))
    }

    /// Whether `(col, row)` lands on the AI-stream stop button in the sign
    /// margin: same geometry as [`test_glyph_at`](Self::test_glyph_at), only
    /// on the streamed line's first visual row.
    pub fn stream_stop_at(&self, col: u16, row: u16) -> bool {
        let Some(stop_line) = self.stream_stop_line else {
            return false;
        };
        if col != self.last_inner.x || row < self.last_inner.y {
            return false;
        }
        let Some((line, start, _)) = self.text_row((row - self.last_inner.y) as usize) else {
            return false;
        };
        if self.wrap_enabled() && start != 0 {
            return false;
        }
        line == stop_line
    }

    /// This frame's screen row of `line`'s first visual row, when visible
    /// (None = scrolled off or folded away). Anchors line-tied popups.
    pub fn screen_row_of_line(&self, line: usize) -> Option<u16> {
        self.last_wrap_rows
            .iter()
            .position(|r| matches!(r, VisRow::Text { line: l, .. } if *l == line))
            .map(|idx| self.last_inner.y.saturating_add(idx as u16))
    }

    /// 1-based breakpoint lines for `path`, ascending, for a DAP
    /// `setBreakpoints` request.
    pub fn breakpoint_lines(&self, path: &Path) -> Vec<u32> {
        self.breakpoints
            .get(path)
            .map(|s| s.iter().map(|&l| l as u32).collect())
            .unwrap_or_default()
    }

    /// Breakpoints for `path` as DAP `SourceBreakpoint`s, attaching any stored
    /// per-line condition and log message. `lines` is the path's breakpoint
    /// set (passed in to avoid a second lookup at the call site).
    pub fn source_breakpoints(
        &self,
        path: &Path,
        lines: &std::collections::BTreeSet<usize>,
    ) -> Vec<crate::dap::session::SourceBreakpoint> {
        let conds = self.breakpoint_conditions.get(path);
        let logs = self.breakpoint_logs.get(path);
        lines
            .iter()
            .map(|&l| crate::dap::session::SourceBreakpoint {
                line: l as u32,
                condition: conds.and_then(|c| c.get(&l)).cloned(),
                log_message: logs.and_then(|m| m.get(&l)).cloned(),
            })
            .collect()
    }

    fn mark_buffer_changed(&mut self) {
        self.dirty = true;
        self.last_edit_at = Some(std::time::Instant::now());
        self.last_edit_pos = Some((self.cursor_row, self.cursor_col));
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.hscroll_content_cols = None;
        self.wrap_total_cache.clear();
        self.occurrences.clear();
        // Link ranges were measured against the pre-edit text; an edit shifts
        // the spans they point at, so a Ctrl+click would follow the wrong one
        // until the next server batch lands.
        self.doc_links.clear();
        self.doc_links_path = None;
        // Lossy-save consent named the unmappable characters the buffer held
        // when the prompt fired; an edit changes what a write would destroy,
        // so stale consent must not carry over (unlike `force_save_armed`,
        // whose overwrite consent is about the DISK state, which typing does
        // not touch). Clearing the latch also lets auto save retry a buffer
        // the user fixed by deleting the offending characters.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
    }

    /// Install (or clear) the git-gutter HEAD baseline for `path`. The app
    /// calls this once per file (and again after HEAD moves) with the
    /// committed version's lines; `None` means untracked / not a repo. Forces
    /// the per-line marks to recompute on the next render.
    pub fn set_git_head_lines(&mut self, path: PathBuf, head: Option<Vec<String>>) {
        self.git_baseline_for = Some(path);
        self.git_head_lines = head;
        self.git_marks_seq = u64::MAX;
    }

    /// Install (or clear) per-line git blame for `path`. The app fetches this
    /// off-thread once per file and after HEAD moves.
    pub fn set_blame(&mut self, path: PathBuf, blame: Option<Vec<crate::git::BlameLine>>) {
        self.blame_for = Some(path);
        self.blame_lines = blame;
    }

    /// The GitLens-style inline annotation for the cursor's line, or `None`
    /// when blame is off, unavailable, or the line was edited past the blamed
    /// range. Committed lines read `author, age • summary`; a line git blames
    /// against the zero hash (a working-tree edit) reads `Uncommitted changes`.
    pub fn current_line_blame_annotation(&self) -> Option<String> {
        if !self.blame_enabled {
            return None;
        }
        // Inside a conflict block the annotation is noise (markers have no
        // meaningful author) and on the header row it painted over the
        // [Accept …] actions — the accept affordance wins.
        if crate::merge::conflict_containing(&self.conflicts, self.cursor_row).is_some() {
            return None;
        }
        // A reused tab (the preview) switches path before the new fetch
        // lands; the old file's blame must not paint on the new one.
        if self.blame_for != self.path {
            return None;
        }
        let blame = self.blame_lines.as_ref()?;
        // A line the git gutter flags as added/modified diverges from HEAD, so
        // its committed author is meaningless — show the uncommitted marker
        // (this also covers unsaved edits, since the gutter recomputes per
        // edit while `blame` is only refetched per file / HEAD change).
        if matches!(
            self.git_mark_at(self.cursor_row),
            Some(GitMark::Added | GitMark::Modified)
        ) {
            return Some("Uncommitted changes".to_string());
        }
        // Insertions/deletions shift line indices out from under the blamed
        // snapshot; only trust the 1:1 mapping while the line counts match.
        if blame.len() != self.lines.len() {
            return None;
        }
        let line = blame.get(self.cursor_row)?;
        if line.uncommitted {
            return Some("Uncommitted changes".to_string());
        }
        Some(format!(
            "{}, {} • {}",
            line.author,
            crate::git::humanize_age(line.age_secs),
            line.summary
        ))
    }

    /// The buffer's merge-conflict blocks, rescanned only when the buffer
    /// changed since the last call — cheap enough for the render loop and
    /// every cursor-position query.
    pub fn conflicts(&mut self) -> &[crate::merge::ConflictBlock] {
        if self.conflicts_seq != self.edit_seq {
            self.conflicts_seq = self.edit_seq;
            self.conflicts = crate::merge::find_conflicts(&self.lines);
        }
        &self.conflicts
    }

    /// Resolve the conflict containing `row` (VS Code's Accept Current /
    /// Incoming / Both): one undo step replacing the whole marker block.
    /// Returns false when `row` is not inside a conflict.
    pub fn resolve_conflict_at(&mut self, row: usize, res: crate::merge::Resolution) -> bool {
        let Some(block) = crate::merge::conflict_containing(self.conflicts(), row) else {
            return false;
        };
        let replacement = crate::merge::resolution_lines(&self.lines, &block, res);
        self.pin_on_edit();
        self.push_undo(EditKind::Replace);
        self.clear_selection();
        self.lines
            .splice(block.ours_start..=block.theirs_end, replacement);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = block.ours_start.min(self.lines.len() - 1);
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        true
    }

    /// Replace Result rows `[start, end)` with `replacement` as ONE undo
    /// step — the merge editor's accept-action primitive (#253), and (with
    /// the full range) the transform that turns a marker buffer into the
    /// initial Result, leaving the markers one Undo away. Mirrors
    /// `resolve_conflict_at`'s splice discipline.
    pub fn splice_result_rows(&mut self, start: usize, end: usize, replacement: Vec<String>) {
        let start = start.min(self.lines.len());
        let end = end.clamp(start, self.lines.len());
        self.pin_on_edit();
        self.push_undo(EditKind::Replace);
        self.clear_selection();
        self.lines.splice(start..end, replacement);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = start.min(self.lines.len() - 1);
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    /// Recompute the per-line git marks from the cached HEAD baseline if the
    /// buffer has changed since they were last built. Cheap no-op when the
    /// `edit_seq` is unchanged, so the render loop can call it every frame.
    fn refresh_git_marks(&mut self) {
        use crate::widgets::diff::DiffRow;
        if self.git_marks_seq == self.edit_seq {
            return;
        }
        self.git_marks_seq = self.edit_seq;
        self.git_marks.clear();
        let Some(head) = self.git_head_lines.as_ref() else {
            return;
        };
        // `build_diff_rows` pairs consecutive delete+insert into `Replaced`, so
        // a bare `Removed` run is a real deletion: attach a `Deleted` marker to
        // the next surviving line (the gutter's deletion boundary, VS Code-style).
        let rows = crate::widgets::diff::build_diff_rows(head, &self.lines);
        let mut pending_delete = false;
        for row in &rows {
            match *row {
                DiffRow::Added { right } => {
                    self.git_marks.insert(right, GitMark::Added);
                    pending_delete = false;
                }
                DiffRow::Replaced { right, .. } => {
                    self.git_marks.insert(right, GitMark::Modified);
                    pending_delete = false;
                }
                DiffRow::Removed { .. } => pending_delete = true,
                DiffRow::Equal { right, .. } => {
                    if pending_delete {
                        self.git_marks.insert(right, GitMark::Deleted);
                        pending_delete = false;
                    }
                }
            }
        }
        // A deletion at end of file has no surviving line after it: mark the
        // last line so a trailing removal still shows.
        if pending_delete && !self.lines.is_empty() {
            self.git_marks
                .entry(self.lines.len() - 1)
                .or_insert(GitMark::Deleted);
        }
    }

    /// The git-gutter mark for 0-based buffer line `line`, if any. Reads the
    /// last computed marks (call after a render, or after `refresh_git_marks`).
    pub fn git_mark_at(&self, line: usize) -> Option<GitMark> {
        self.git_marks.get(&line).copied()
    }

    /// Soft-wrap mode: long lines fold onto multiple visual rows instead of
    /// scrolling horizontally. Matches VS Code, which ships word-wrap on by
    /// default for Markdown only (`"[markdown]": { "editor.wordWrap": "on" }`).
    /// `toggle_wrap` (Alt+Z) sets a per-buffer override that wins over the
    /// language default.
    pub fn wrap_enabled(&self) -> bool {
        self.wrap_override
            .unwrap_or(matches!(self.lang, Some(LangKind::Markdown)))
    }

    /// Longest line width in characters, cached until the buffer changes.
    /// Drives the horizontal scrollbar's content extent and the `scroll_col`
    /// clamp. `&mut self` so the lazy recompute can populate the cache.
    fn content_cols(&mut self) -> usize {
        if let Some(cols) = self.hscroll_content_cols {
            return cols;
        }
        let cols = self
            .lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                // Spliced hint cells widen the line's display, so the
                // horizontal extent must include them or the tail of a long
                // hinted line could never scroll into view.
                let extra: usize = self
                    .inlay_spans
                    .get(i)
                    .map(|hs| hs.iter().map(|(_, label, _)| label.chars().count()).sum())
                    .unwrap_or(0);
                l.chars().count() + extra
            })
            .max()
            .unwrap_or(0);
        self.hscroll_content_cols = Some(cols);
        cols
    }

    // -- Comment-box geometry ----------------------------------------------
    // Comment boxes are unnumbered blocks rendered between a buffer line and
    // the next. They add visual rows without owning buffer positions; every
    // helper here is O(#boxes) and short-circuits to zero when no box is
    // present, so the hot path pays nothing.

    /// The layout entry at visual index `i` when it is a buffer-text row
    /// (None = out of range or a comment-box row). The one funnel every
    /// visual-row inversion goes through.
    fn text_row(&self, i: usize) -> Option<(usize, usize, usize)> {
        match self.last_wrap_rows.get(i)? {
            VisRow::Text { line, start, end } => Some((*line, *start, *end)),
            VisRow::Box { .. } => None,
        }
    }

    /// The wrapped display lines of a box body at `bw` columns (its inner
    /// text width). Bodies are short; this allocates only while boxes exist.
    fn comment_body_lines(body: &str, bw: usize) -> Vec<String> {
        let bw = bw.max(8);
        let mut out = Vec::new();
        for line in body.split('\n') {
            let chars: Vec<char> = line.chars().collect();
            for &(s, e) in &wrap_segments(&chars, bw) {
                out.push(chars[s..e].iter().collect());
            }
        }
        if out.is_empty() {
            out.push(String::new());
        }
        out
    }

    /// Total rows `comment_boxes[idx]` occupies at text width `tw`:
    /// title + wrapped body + footer.
    fn comment_box_height(&self, idx: usize, tw: usize) -> usize {
        let body = &self.comment_boxes[idx].body;
        2 + Self::comment_body_lines(body, tw.saturating_sub(4)).len()
    }

    /// Rows contributed by the boxes anchored under `line` at width `tw`.
    fn box_rows_at_line(&self, line: usize, tw: usize) -> usize {
        if self.comment_boxes.is_empty() {
            return 0;
        }
        (0..self.comment_boxes.len())
            .filter(|&i| self.comment_boxes[i].line == line)
            .map(|i| self.comment_box_height(i, tw))
            .sum()
    }

    /// Rows contributed by boxes anchored under lines `[from, to)`.
    fn box_rows_between(&self, from: usize, to: usize, tw: usize) -> usize {
        if self.comment_boxes.is_empty() {
            return 0;
        }
        (0..self.comment_boxes.len())
            .filter(|&i| {
                let l = self.comment_boxes[i].line;
                // A box on a line hidden inside a collapsed fold is never
                // painted (the layout skips hidden lines wholesale), so it
                // must not count toward any scroll geometry either - it
                // kept a scrollbar alive for invisible content and let the
                // wheel walk the viewport into blank space.
                l >= from && l < to && !self.is_line_hidden(l)
            })
            .map(|i| self.comment_box_height(i, tw))
            .sum()
    }

    /// Paint one row of a comment box: blank unnumbered gutter, then the
    /// block chrome. Rows: 0 = title (author), middle = body, last = footer
    /// (reply field left, ✕ Ignore right).
    #[allow(clippy::too_many_arguments)]
    fn paint_comment_box_row(
        &self,
        buf: &mut ratatui::buffer::Buffer,
        inner: Rect,
        y: u16,
        gutter_width: u16,
        text_x: u16,
        text_width: u16,
        box_idx: usize,
        box_row: usize,
    ) {
        // The whole gutter goes blank: no number, no glyphs, no git bar.
        buf.set_line(
            inner.x,
            y,
            &Line::from(" ".repeat(gutter_width as usize + 1)),
            gutter_width + 1,
        );
        let w = text_width as usize;
        if w < 8 {
            return;
        }
        let b = &self.comment_boxes[box_idx];
        let accent = Style::default()
            .fg(NAVIGATOR_ACCENT)
            .bg(self.theme.sticky_scroll_bg());
        let body_st = Style::default().bg(self.theme.sticky_scroll_bg());
        let dim = Style::default()
            .fg(Color::DarkGray)
            .bg(self.theme.sticky_scroll_bg());
        let height = self.comment_box_height(box_idx, w);
        let pad = |s: &str, n: usize| -> String {
            let mut out: String = s.chars().take(n).collect();
            while out.chars().count() < n {
                out.push(' ');
            }
            out
        };
        let line = if box_row == 0 {
            let head = format!("\u{256d}\u{2500} \u{25c6} {} ", b.author);
            let fill = w.saturating_sub(head.chars().count() + 1);
            Line::from(Span::styled(
                format!("{head}{}\u{256e}", "\u{2500}".repeat(fill)),
                accent,
            ))
        } else if box_row + 1 < height {
            let body_lines = Self::comment_body_lines(&b.body, w.saturating_sub(4));
            let text = body_lines.get(box_row - 1).cloned().unwrap_or_default();
            Line::from(vec![
                Span::styled("\u{2502} ", accent),
                Span::styled(pad(&text, w - 4), body_st),
                Span::styled(" \u{2502}", accent),
            ])
        } else {
            // Footer: `╰ ❯ <reply>            ✕ Ignore ╯`
            let focus = self.comment_focus.as_ref().filter(|f| f.id == b.id);
            let field_w = w.saturating_sub(4 + IGNORE_TAIL_COLS);
            // Window the draft around the caret: a reply longer than the
            // field otherwise froze at its first field_w chars and the
            // user typed blind, with no caret drawn anywhere.
            let win = focus.map_or(0, |f| f.cursor.saturating_sub(field_w.saturating_sub(1)));
            let (draft, style) = match focus {
                Some(f) => {
                    let visible: String = f.reply.chars().skip(win).take(field_w).collect();
                    (pad(&visible, field_w), body_st)
                }
                None => (pad("Reply", field_w), dim),
            };
            let mut spans = vec![
                Span::styled("\u{2570} ", accent),
                Span::styled("\u{276f} ", accent),
            ];
            match focus {
                Some(f) if field_w > 0 && f.cursor - win < field_w => {
                    // Split the draft around the caret cell so it shows.
                    let cursor = f.cursor - win;
                    let chars: Vec<char> = draft.chars().collect();
                    let before: String = chars[..cursor].iter().collect();
                    let at: String = chars[cursor..=cursor].iter().collect();
                    let after: String = chars[cursor + 1..].iter().collect();
                    spans.push(Span::styled(before, style));
                    spans.push(Span::styled(
                        at,
                        style.add_modifier(ratatui::style::Modifier::REVERSED),
                    ));
                    spans.push(Span::styled(after, style));
                }
                _ => spans.push(Span::styled(draft, style)),
            }
            spans.push(Span::styled(" \u{2715} Ignore ", accent));
            spans.push(Span::styled("\u{256f}", accent));
            Line::from(spans)
        };
        buf.set_line(text_x, y, &line, text_width);
        // Ensure the block's background reaches the right edge even when a
        // span fell short (padding already covers the normal case).
        for x in text_x..text_x + text_width {
            if buf[(x, y)].symbol() == " " && buf[(x, y)].style().bg.is_none() {
                buf[(x, y)].set_style(body_st);
            }
        }
    }

    /// What a click at screen `(col, row)` hits inside a comment box:
    /// Ignore on the footer's ✕ Ignore cells, Reply on the rest of the
    /// footer, Body anywhere else in the block. None = not a box row.
    pub fn comment_box_hit(&self, col: u16, row: u16) -> Option<(u64, CommentHit)> {
        if self.comment_boxes.is_empty() {
            return None;
        }
        let inner = self.last_inner;
        if row < inner.y || col < inner.x {
            return None;
        }
        let VisRow::Box { box_idx, box_row } =
            *self.last_wrap_rows.get((row - inner.y) as usize)?
        else {
            return None;
        };
        let b = &self.comment_boxes[box_idx];
        let text_x = inner.x + self.last_gutter_width + 1;
        let w = self.visible_text_width();
        if box_row + 1 == self.comment_box_height(box_idx, w) {
            // Footer: the ` ✕ Ignore ╯` tail owns its cells.
            let rel = (col.saturating_sub(text_x)) as usize;
            if rel + IGNORE_TAIL_COLS >= w && rel < w {
                return Some((b.id, CommentHit::Ignore));
            }
            return Some((b.id, CommentHit::Reply));
        }
        Some((b.id, CommentHit::Body))
    }

    // -- Soft-wrap visual-row geometry ------------------------------------
    // In wrap mode a logical line maps to one or more visual rows. These
    // helpers convert between logical positions and the flattened visual-row
    // space the viewport scrolls over. They are only called on the wrap path;
    // the non-wrap path keeps its cheap one-row-per-line arithmetic.

    /// Wrap segments of a single logical line at the given text width.
    fn line_segments(&self, line: usize, width: usize) -> Vec<(usize, usize)> {
        let chars: Vec<char> = self
            .lines
            .get(line)
            .map(|l| l.chars().collect())
            .unwrap_or_default();
        wrap_segments(&chars, width)
    }

    /// Number of visual rows a logical line occupies. Avoids allocating the
    /// segment vector for the common case of a line that already fits.
    fn line_visual_rows(&self, line: usize, width: usize) -> usize {
        if width == 0 {
            return 1;
        }
        let len = self.line_char_len(line);
        if len <= width {
            1
        } else {
            self.line_segments(line, width).len()
        }
    }

    /// Visual rows of `line`'s whole group: its wrap segments plus the
    /// comment-box rows hanging under it. All flattened visual-row math
    /// (totals, viewport top, cursor row, decomposition) speaks group rows
    /// so boxes shift everything below them consistently.
    fn group_visual_rows(&self, line: usize, width: usize) -> usize {
        self.line_visual_rows(line, width) + self.box_rows_at_line(line, width)
    }

    /// Total visual rows across the whole buffer. The text-only total is
    /// cached per width; box rows are added fresh (boxes are few and can
    /// change between frames without touching the buffer).
    fn total_visual_rows(&mut self, width: usize) -> usize {
        let text =
            if let Some(&(_, total)) = self.wrap_total_cache.iter().find(|&&(w, _)| w == width) {
                total
            } else {
                let total: usize = (0..self.lines.len())
                    .map(|l| self.line_visual_rows(l, width))
                    .sum();
                self.wrap_total_cache.push((width, total));
                total
            };
        text + self.box_rows_between(0, self.lines.len(), width)
    }

    /// Visual-row index of the current viewport top `(scroll, scroll_sub)`.
    fn top_visual_row(&self, width: usize) -> usize {
        (0..self.scroll)
            .map(|l| self.group_visual_rows(l, width))
            .sum::<usize>()
            + self.scroll_sub
    }

    /// Visual-row index of the segment containing the cursor. A box under
    /// the cursor's own line renders below its segments, so only boxes on
    /// earlier lines shift the cursor.
    fn cursor_visual_row(&self, width: usize) -> usize {
        let before: usize = (0..self.cursor_row)
            .map(|l| self.group_visual_rows(l, width))
            .sum();
        before + self.segment_index_of_col(self.cursor_row, self.cursor_col, width)
    }

    /// Which segment of `line` holds character column `col`. A column sitting
    /// exactly on a wrap boundary belongs to the *next* segment (its start),
    /// matching where `cursor_screen_pos` paints the caret; only the true line
    /// end maps to the final segment. Keeping this predicate identical to the
    /// one in `cursor_screen_pos` is what keeps vertical motion consistent with
    /// where the caret actually renders.
    fn segment_index_of_col(&self, line: usize, col: usize, width: usize) -> usize {
        let segs = self.line_segments(line, width);
        let line_len = self.line_char_len(line);
        segs.iter()
            .position(|&(s, e)| col >= s && (col < e || (col == e && e == line_len)))
            .unwrap_or(segs.len().saturating_sub(1))
    }

    /// Point the viewport top at a global visual-row index, decomposing it back
    /// into `(scroll, scroll_sub)`. Parks at the last segment when `target`
    /// runs past the end of the buffer.
    fn set_top_to_visual_row(&mut self, target: usize, width: usize) {
        let mut acc = 0;
        for line in 0..self.lines.len() {
            let n = self.group_visual_rows(line, width);
            if acc + n > target {
                self.scroll = line;
                // scroll_sub may point into the line's box region (past its
                // segments); the layout builder starts mid-box then.
                self.scroll_sub = target - acc;
                return;
            }
            acc += n;
        }
        self.scroll = self.lines.len().saturating_sub(1);
        self.scroll_sub = self.group_visual_rows(self.scroll, width).saturating_sub(1);
    }

    /// Logical `(line, char_col)` at the start of a global visual row -
    /// the inverse of `cursor_visual_row` for the row's first column. A
    /// target inside a line's comment-box region snaps to that line's last
    /// segment (a box row has no buffer position of its own).
    fn logical_pos_at_visual_row(&self, target: usize, width: usize) -> (usize, usize) {
        let mut acc = 0;
        for line in 0..self.lines.len() {
            let segs = self.line_segments(line, width);
            let group = segs.len() + self.box_rows_at_line(line, width);
            if acc + group > target {
                let idx = (target - acc).min(segs.len().saturating_sub(1));
                return (line, segs[idx].0);
            }
            acc += group;
        }
        (self.lines.len().saturating_sub(1), 0)
    }

    /// True when global visual row `target` falls inside a comment-box
    /// region (no buffer position). Cursor motion steps over these.
    fn visual_row_is_box(&self, target: usize, width: usize) -> bool {
        if self.comment_boxes.is_empty() {
            return false;
        }
        let mut acc = 0;
        for line in 0..self.lines.len() {
            let segs = self.line_visual_rows(line, width);
            let group = segs + self.box_rows_at_line(line, width);
            if acc + group > target {
                return target - acc >= segs;
            }
            acc += group;
        }
        false
    }

    /// Move the wrap viewport so the given visual row is at the top, clamped to
    /// the document, then pull the cursor into the new viewport so the render's
    /// cursor-visibility clamp doesn't immediately undo the scroll. Used by the
    /// wheel and the vertical scrollbar in wrap mode.
    fn wrap_set_top(&mut self, top_target: usize) {
        let width = self.visible_text_width();
        let viewport = self.text_rows();
        if width == 0 || viewport == 0 {
            return;
        }
        let total = self.total_visual_rows(width);
        let new_top = top_target.min(total.saturating_sub(viewport));
        self.set_top_to_visual_row(new_top, width);
        let cursor_vrow = self.cursor_visual_row(width);
        let target_row = if cursor_vrow < new_top {
            Some(new_top)
        } else if cursor_vrow >= new_top + viewport {
            Some(new_top + viewport - 1)
        } else {
            None
        };
        if let Some(vrow) = target_row {
            let (line, col) = self.logical_pos_at_visual_row(vrow, width);
            self.cursor_row = line;
            self.cursor_col = col.min(self.line_char_len(line));
        }
        self.last_edit_kind = None;
    }

    /// Move the cursor one visual row up (`dir = -1`) or down (`dir = 1`) in
    /// wrap mode, preserving the visual column within the row. Unlike the
    /// logical `move_up`/`move_down`, this steps through wrapped segments of a
    /// paragraph rather than skipping the whole logical line.
    fn wrap_move_vertical(&mut self, dir: isize) {
        let width = self.visible_text_width().max(1);
        let segs = self.line_segments(self.cursor_row, width);
        let seg_idx = self.segment_index_of_col(self.cursor_row, self.cursor_col, width);
        let goal_col = self.cursor_col - segs[seg_idx].0;
        let current = self.cursor_visual_row(width) as isize;
        let total = self.total_visual_rows(width) as isize;
        let mut target = current + dir;
        // Step over comment-box rows: the caret never enters a box.
        while target >= 0 && target < total && self.visual_row_is_box(target as usize, width) {
            target += dir;
        }
        if target < 0 || target >= total {
            return;
        }
        let (line, seg_start) = self.logical_pos_at_visual_row(target as usize, width);
        let (s, e) = self
            .line_segments(line, width)
            .into_iter()
            .find(|&(s, _)| s == seg_start)
            .unwrap_or((seg_start, seg_start));
        self.cursor_row = line;
        self.cursor_col = (s + goal_col).min(e).min(self.line_char_len(line));
    }

    /// Identifier chars immediately to the left of the cursor on the
    /// current line. Used as the LSP completion popup's filter prefix
    /// so the menu narrows to items the user is actually typing toward.
    pub fn word_before_cursor(&self) -> String {
        let Some(line) = self.lines.get(self.cursor_row) else {
            return String::new();
        };
        let chars: Vec<char> = line.chars().collect();
        let end = self.cursor_col.min(chars.len());
        let mut start = end;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        chars[start..end].iter().collect()
    }

    pub fn open(&mut self, path: &Path) -> Result<()> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        // The Reopen-as-Text override is PER TAB and per file: a path
        // change is a different document, which routes normally again.
        if self.path.as_deref() != Some(path) {
            self.force_text = false;
        }
        if !self.force_text {
            // Rendered ANSI log (#257): a colour-bearing log paints through
            // the theme palette instead of showing raw escapes. Sniffed, so a
            // plain .txt with pytest output routes here too; a failure falls
            // through to the normal text path.
            if Self::should_open_as_log(path) && self.open_log(path).is_ok() {
                return Ok(());
            }
            // Media (#183): header-probed info card; junk with a media
            // extension falls through to the binary path.
            if crate::media::extension_is_media(ext) && self.open_media_preview(path).is_ok() {
                return Ok(());
            }
            // SQLite (#182): by extension here, by magic below.
            let is_sqlite_ext = path
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(crate::sqlite_view::extension_is_sqlite);
            if is_sqlite_ext {
                // Propagate errors when the MAGIC confirms sqlite (#201
                // review: a corrupt database must error, not fall to
                // hex); a .db that is not sqlite at all falls through.
                let magic = read_file_head(path)
                    .map(|h| crate::magic::sniff(&h) == Some(crate::magic::Magic::Sqlite))
                    .unwrap_or(false);
                if magic {
                    return self.open_sqlite(path).map_err(|e| anyhow::anyhow!("{e}"));
                }
            }
            // docx/odt (#181): a document that fails the walk falls
            // through (its zip container then reaches the archive route).
            if crate::docx::extension_is_doc(ext) && self.open_doc_preview(path).is_ok() {
                return Ok(());
            }
            // A corrupt archive falls through to content routing and,
            // ultimately, the hex fallback.
            if let Some(kind) = crate::archive::kind_from_ext(path)
                && self.open_archive(path, kind).is_ok()
            {
                return Ok(());
            }
            if extension_is_image(ext) {
                // A DECODE failure means the extension lied (#174): fall
                // through to content routing instead of failing the open.
                // Filesystem errors stay errors — the file may not exist.
                match self.open_image(path) {
                    Err(e) if e.to_string().starts_with("Could not decode image") => {}
                    other => return other,
                }
            }
            // SVG renders through resvg into the image pipeline (#175); a
            // parse failure falls through to the XML source in the text
            // editor.
            if ext.eq_ignore_ascii_case("svg") && self.open_svg(path).is_ok() {
                return Ok(());
            }
            if extension_is_pdf(ext) && self.pdf_viewer_enabled {
                return self.open_pdf(path);
            }
            if crate::sheet::extension_is_sheet(ext) && self.csv_viewer_enabled {
                // Unsaved grid edits survive a same-path re-open (#177):
                // a tree re-click must not rebuild the view over them.
                // The FS sweep never reloads a dirty tab, and Revert
                // clears the flag first, re-arming the reload.
                if self.path.as_deref() == Some(path)
                    && self
                        .sheet
                        .as_ref()
                        .is_some_and(|v| v.dirty || v.editing.is_some())
                {
                    return Ok(());
                }
                // Same fall-through for a workbook/CSV that does not parse
                // as its extension claims (#174) — but ONLY for parse
                // failures. A SIZE refusal ("too large") is a deliberate
                // cap, and rerouting it into the text path would silently
                // bypass the guard the cap exists to enforce (#188 review).
                match self.open_sheet(path) {
                    Err(e)
                        if e.to_string().starts_with("Spreadsheet open failed")
                            && !e.to_string().contains("too large") => {}
                    other => return other,
                }
            }
        }
        let meta = std::fs::metadata(path)?;
        if meta.len() > MAX_FILE_BYTES {
            // Sniff only the head: the hex viewer reads windows on
            // demand and never loads the file whole, so an over-limit
            // BINARY file opens fine (#172). Only text — which the
            // editor genuinely must hold in memory — keeps the guard.
            let head = read_file_head(path)?;
            let head_bom_text = encoding_rs::Encoding::for_bom(&head).is_some();
            if !head_bom_text && is_binary(&head) {
                return self.open_hex(path);
            }
            anyhow::bail!("File too large ({} bytes)", meta.len());
        }
        let bytes = std::fs::read(path)?;
        // Content routing (#174): the extension gave no answer, or lied
        // and fell through above. Try the sniffed kind's viewer; any
        // failure falls back to the text/binary path below, whose hex
        // fallback guarantees the open never dead-ends. The
        // `!extension_*` guards keep a route that already failed by
        // extension from being retried against the same bytes.
        match (!self.force_text)
            .then(|| crate::magic::sniff(&bytes))
            .flatten()
        {
            Some(m) if m.is_image() && !extension_is_image(ext) => {
                if self.open_image(path).is_ok() {
                    return Ok(());
                }
            }
            Some(crate::magic::Magic::Pdf) if !extension_is_pdf(ext) && self.pdf_viewer_enabled => {
                if self.open_pdf(path).is_ok() {
                    return Ok(());
                }
            }
            // A zip container is an xlsx candidate (the only zip-backed
            // format croft renders today); a plain archive fails the
            // parse and falls through. Only the `.xlsx` extension is
            // excluded — that exact route already ran and failed above;
            // a workbook wearing `.csv` (or any other sheet extension)
            // failed a DIFFERENT parser, so the xlsx retry is genuinely
            // new information (#188 review).
            Some(crate::magic::Magic::Zip) => {
                if !ext.eq_ignore_ascii_case("xlsx")
                    && self.csv_viewer_enabled
                    && let Ok(view) =
                        crate::sheet::open_sheet_with_kind(path, crate::sheet::SheetKind::Xlsx)
                {
                    self.install_sheet_view(path, view);
                    return Ok(());
                }
                // Not a workbook: browse it as the archive it is (#179).
                if self
                    .open_archive(path, crate::archive::ArchiveKind::Zip)
                    .is_ok()
                {
                    return Ok(());
                }
            }
            // The remaining container kinds browse too when they parse.
            Some(crate::magic::Magic::Gzip)
                if self
                    .open_archive(path, crate::archive::ArchiveKind::TarGz)
                    .is_ok() =>
            {
                return Ok(());
            }
            Some(crate::magic::Magic::Sqlite) => {
                return self.open_sqlite(path);
            }
            Some(crate::magic::Magic::Tar)
                if self
                    .open_archive(path, crate::archive::ArchiveKind::Tar)
                    .is_ok() =>
            {
                return Ok(());
            }
            _ => {}
        }
        // Decode with the file's BOM-declared encoding if it has one, else
        // UTF-8. A later "Reopen with Encoding" overrides this.
        let sniffed = encoding_rs::Encoding::for_bom(&bytes);
        let enc = sniffed.map(|(e, _)| e).unwrap_or(encoding_rs::UTF_8);
        // The BOM sniff comes FIRST: UTF-16 text is half NUL bytes, so the
        // binary heuristic would reject every UTF-16 file — including the ones
        // croft itself writes when the user picks UTF-16 from the encoding
        // menu. A byte-order mark is a positive declaration of text, so it
        // settles the question the heuristic is only guessing at.
        let bom_declares_text = enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE;
        if !bom_declares_text && is_binary(&bytes) {
            // Never a dead-end: binary files open in the hex viewer
            // instead of failing with "Binary file" (#172).
            return self.open_hex(path);
        }
        let changed_file = self.path.as_deref() != Some(path);
        // `open` is also the same-path reload behind the FS-sync sweep and
        // every revert, so auto-detection here would throw away an encoding
        // the user picked through "Reopen with Encoding" — decoding the same
        // bytes as UTF-8 and handing the next save the mojibake to write back.
        // The choice sticks until the file itself declares otherwise.
        let enc = if changed_file || sniffed.is_some() {
            enc
        } else {
            self.encoding
        };
        self.encoding = enc;
        // `decode` strips the BOM, so remember it or the next save drops it.
        self.bom = sniffed.is_some();
        let text = enc.decode(&bytes).0.into_owned();
        // Detect the file's line-ending style before normalisation so a save
        // preserves it (and the status bar reports it). A single `\r\n` marks
        // the file CRLF, matching VS Code's "files.eol auto" heuristic.
        self.eol = if text.contains("\r\n") {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        };
        self.lines = split_into_lines(&text);
        // The map described the text that was just replaced (#349). Clearing
        // is the safe direction: an unknown line is correct, a line credited
        // to whoever wrote the file's PREVIOUS contents is not.
        self.provenance = crate::provenance::Provenance::new();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        // VS Code's editor.detectIndentation: the freshly loaded content
        // decides the buffer's indent style unless the user overrides it.
        self.detected_indent = detect_indentation(&self.lines);
        self.hscroll_content_cols = None;
        self.wrap_total_cache.clear();
        self.scroll_sub = 0;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lang = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(lang_for_extension);
        self.wrap_override = None;
        // Folds are line numbers into the OLD file. A preview tab is reused for
        // the next single-click, so without this the incoming file arrives with
        // the previous one's blocks collapsed. Only for a DIFFERENT file
        // though: `open` is also the same-path reload behind every FS-sync
        // sweep, and dropping the set there popped the user's blocks open
        // whenever anything rewrote the file. That case is already covered by
        // the render-time `fold_epoch_lines` guard, which drops folds if the
        // reload changed the line count.
        if changed_file {
            self.folded.clear();
            self.hidden_ranges.clear();
            self.fold_epoch_lines = 0;
            self.lsp_folds = None;
            self.lsp_folds_lines = 0;
            self.fallback_kind_folds.clear();
            self.fallback_folds_seq = None;
            // Inline-value trailers are line numbers into the OLD file too
            // (#136 review): a reused preview tab must not dress the incoming
            // file in the previous one's debug state. The same-path reload
            // keeps them — the next stop rebuilds them anyway.
            self.inline_values.clear();
        } else if !self.folded.is_empty() {
            // The retained headers were measured against text that has just
            // been replaced. Re-measure their spans, or a reload that keeps the
            // line count while moving the indentation leaves the cache hiding
            // lines the folds no longer cover — the epoch guard below only
            // catches a change in line COUNT.
            self.rebuild_hidden_ranges();
        }
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.image = None;
        self.sheet = None;
        self.markdown_preview = None;
        self.hex = None;
        self.log = None;
        self.archive = None;
        // A real file supersedes any diff view this editor was showing —
        // without this a restore-then-reload keeps rendering the stale diff.
        self.diff = None;
        self.merge = None;
        self.status = format!("Opened {}", path.display());
        // The buffer now matches disk; bump the edit seq so the LSP doc sync
        // sees the new content (an external reload lands here too, and without
        // the bump the server keeps analysing the old text and never sends
        // fresh semantic tokens — codeberg issue #39).
        self.edit_seq = self.edit_seq.wrapping_add(1);
        // A path CHANGE detaches the buffer from any collab doc it was
        // synced to: the old generation belongs to another file, and a
        // reused tab (the preview tab) keeping it would let the next collab
        // tick broadcast this file's disk text as edits to that doc. A
        // same-path reload keeps its attachment deliberately — that is how
        // an owner's reload-diff (external change, Replace All) converges
        // into the session as ops.
        if changed_file {
            self.collab_doc_gen = 0;
            self.collab_synced_seq = self.edit_seq;
        }
        // Drop any semantic-token batch measured against the previous content:
        // decoding it over the new lines paints the old file's colors at the
        // wrong offsets. Tree-sitter colors cover the gap until the fresh
        // batch triggered by the seq bump arrives.
        self.semantic_data = Vec::new();
        self.semantic_overlay = Vec::new();
        self.semantic_is_full = false;
        // Same for inlay hints: anchors measured against another file's text
        // must never splice into this one.
        self.inlay_hints = Vec::new();
        self.inlay_path = None;
        self.doc_links = Vec::new();
        self.doc_links_path = None;
        self.color_infos = Vec::new();
        self.color_path = None;
        self.inlay_spans = Vec::new();
        self.recompute_highlights();
        // Notebooks auto-open rendered (#180): the JSON stays one
        // Reopen-as-Text away, and force_text keeps that choice sticky.
        if !self.force_text && path.extension().and_then(|e| e.to_str()) == Some("ipynb") {
            self.build_notebook_preview();
        }
        Ok(())
    }

    fn open_image(&mut self, path: &Path) -> Result<()> {
        let meta = std::fs::metadata(path)?;
        if meta.len() > MAX_IMAGE_BYTES {
            anyhow::bail!("Image too large ({} bytes)", meta.len());
        }
        let bytes = std::fs::read(path)?;
        let (pixel_w, pixel_h) = image::load_from_memory(&bytes)
            .map(|img| (img.width(), img.height()))
            .map_err(|e| anyhow::anyhow!("Could not decode image: {e}"))?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let format_label = image_format_label_from_ext(ext);
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // A whole-buffer swap like every other opener: caches memoised on
        // edit_seq (conflicts, git marks) must not survive into this tab.
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.image = Some(ImageView {
            bytes,
            format_label,
            pixel_w,
            pixel_h,
            byte_size: meta.len(),
            generation: next_image_generation(),
            pdf: None,
        });
        // A diff view is superseded like every other kind: `open`'s text
        // tail clears it, but the preview openers return before reaching
        // that tail, and the render's arrow-rect clearing sits after the
        // preview arms' early returns — so a diff tab reopened as a
        // preview kept its label, caret, and CLICKABLE hunk arrows
        // (#187 review). Clear the state and the frame-truth rects here.
        self.diff = None;
        self.merge = None;
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();
        self.sheet = None;
        self.hex = None;
        self.log = None;
        self.archive = None;
        self.status = format!("Opened image {}", path.display());
        Ok(())
    }

    fn open_sheet(&mut self, path: &Path) -> Result<()> {
        let view = crate::sheet::open_sheet(path)
            .map_err(|e| anyhow::anyhow!("Spreadsheet open failed: {e}"))?;
        self.install_sheet_view(path, view);
        Ok(())
    }

    /// Media info view (#183): header-probed properties rendered as a
    /// markdown card, with an ffmpeg poster frame when available.
    fn open_media_preview(&mut self, path: &Path) -> Result<()> {
        let info = crate::media::probe(path)
            .ok_or_else(|| anyhow::anyhow!("no recognisable media header"))?;
        let scratch = std::env::temp_dir().join(format!("croft-media-{}", std::process::id()));
        let md_text = crate::media::to_markdown(path, &info, &scratch);
        let (lines, images) = crate::markdown::render_markdown_with_images(
            &md_text,
            self.theme,
            &mut self.registry,
            Some(&scratch),
        );
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.diff = None;
        self.merge = None;
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();
        self.image = None;
        self.sheet = None;
        self.hex = None;
        self.log = None;
        self.archive = None;
        self.markdown_preview = Some(crate::markdown::MarkdownPreview {
            rows: Vec::new(),
            selection: None,
            dragging: false,
            lines,
            scroll: 0,
            built_seq: self.edit_seq,
            images,
            anchor_rows: Vec::new(),
            runnables: Vec::new(),
            run_rows: Vec::new(),
            wrap_key: (0, 0),
            last_area: Rect::default(),
            notebook: false,
            doc_path: Some(path.to_path_buf()),
            media: true,
        });
        self.status = format!("Opened media info {}", path.display());
        Ok(())
    }

    /// Read-only SQLite browser (#182): tables as sheet-grid
    /// worksheets, through the standard sheet install.
    fn open_sqlite(&mut self, path: &Path) -> Result<()> {
        let view = crate::sqlite_view::open_database(path)
            .map_err(|e| anyhow::anyhow!("SQLite open failed: {e}"))?;
        self.install_sheet_view(path, view);
        Ok(())
    }

    /// docx/odt read-only preview (#181): the document walks into
    /// markdown and renders through the preview machinery; embedded
    /// images extract to scratch and ride the inline-image overlay.
    fn open_doc_preview(&mut self, path: &Path) -> Result<()> {
        let scratch = std::env::temp_dir().join("croft-doc-images");
        let md = crate::docx::to_markdown(path, &scratch)
            .ok_or_else(|| anyhow::anyhow!("not a recognisable document"))?;
        let (lines, images) = crate::markdown::render_markdown_with_images(
            &md,
            self.theme,
            &mut self.registry,
            Some(&scratch),
        );
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.diff = None;
        self.merge = None;
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();
        self.image = None;
        self.sheet = None;
        self.hex = None;
        self.log = None;
        self.archive = None;
        self.markdown_preview = Some(crate::markdown::MarkdownPreview {
            rows: Vec::new(),
            selection: None,
            dragging: false,
            lines,
            scroll: 0,
            built_seq: self.edit_seq,
            images,
            anchor_rows: Vec::new(),
            runnables: Vec::new(),
            run_rows: Vec::new(),
            wrap_key: (0, 0),
            last_area: Rect::default(),
            notebook: false,
            doc_path: Some(path.to_path_buf()),
            media: false,
        });
        self.status = format!("Opened document {}", path.display());
        Ok(())
    }

    /// Archive browser (#179): list the members read-only; Enter
    /// extracts one to scratch and reopens it through the dispatch.
    fn open_archive(&mut self, path: &Path, kind: crate::archive::ArchiveKind) -> Result<()> {
        let view = crate::archive::list(path, kind)
            .map_err(|e| anyhow::anyhow!("Archive open failed: {e}"))?;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.diff = None;
        self.merge = None;
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();
        self.image = None;
        self.sheet = None;
        self.hex = None;
        self.log = None;
        self.markdown_preview = None;
        self.archive = Some(view);
        self.status = format!("Opened archive {}", path.display());
        Ok(())
    }

    /// Rendered SVG preview (#175): rasterise through resvg at a fixed
    /// quality (the PDF philosophy — one raster per open, no re-render
    /// on pane resize) and hand the PNG to the standard image pipeline.
    fn open_svg(&mut self, path: &Path) -> Result<()> {
        let meta = std::fs::metadata(path)?;
        if meta.len() > crate::svg::MAX_SVG_BYTES {
            anyhow::bail!("SVG too large ({} bytes)", meta.len());
        }
        let src = std::fs::read(path)?;
        let (png, pixel_w, pixel_h) =
            crate::svg::rasterize(&src).map_err(|e| anyhow::anyhow!("SVG render failed: {e}"))?;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // A whole-buffer swap like every other opener: caches memoised on
        // edit_seq (conflicts, git marks) must not survive into this tab.
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        // A diff view is superseded like every other kind: `open`'s text
        // tail clears it, but the preview openers return before reaching
        // that tail, and the render's arrow-rect clearing sits after the
        // preview arms' early returns — so a diff tab reopened as a
        // preview kept its label, caret, and CLICKABLE hunk arrows
        // (#187 review). Clear the state and the frame-truth rects here.
        self.diff = None;
        self.merge = None;
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();
        self.sheet = None;
        self.hex = None;
        self.log = None;
        self.archive = None;
        self.markdown_preview = None;
        self.image = Some(ImageView {
            bytes: png,
            format_label: String::from("SVG"),
            pixel_w,
            pixel_h,
            byte_size: meta.len(),
            generation: next_image_generation(),
            pdf: None,
        });
        self.status = format!("Opened SVG {}", path.display());
        Ok(())
    }

    /// The state swap half of a sheet open, shared with the content
    /// router (#174), which fetches the view by sniffed kind instead of
    /// by extension.
    fn install_sheet_view(&mut self, path: &Path, view: crate::sheet::SheetView) {
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // A whole-buffer swap like every other opener: caches memoised on
        // edit_seq (conflicts, git marks) must not survive into this tab.
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        // A diff view is superseded like every other kind: `open`'s text
        // tail clears it, but the preview openers return before reaching
        // that tail, and the render's arrow-rect clearing sits after the
        // preview arms' early returns — so a diff tab reopened as a
        // preview kept its label, caret, and CLICKABLE hunk arrows
        // (#187 review). Clear the state and the frame-truth rects here.
        self.diff = None;
        self.merge = None;
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();
        self.image = None;
        self.hex = None;
        self.log = None;
        self.archive = None;
        self.status = format!("Opened {} ({})", path.display(), view.kind.label());
        self.sheet = Some(view);
    }

    fn open_pdf(&mut self, path: &Path) -> Result<()> {
        let backend = crate::pdf::detect_backend()
            .ok_or_else(|| anyhow::anyhow!("Install poppler (pdftoppm) to preview PDFs"))?;
        let meta = std::fs::metadata(path)?;
        let page_count = crate::pdf::detect_page_count(path);
        // A reload comes up on the reader's page in this single
        // rasterisation — clamped when the count is known, so a genuinely
        // shrunken document lands on its new last page. On failure the
        // whole open fails and the caller's failed-reload path keeps the
        // last good render AND the place (#72).
        let page = match self.pdf_restore_page.take() {
            Some(p) => p.clamp(1, page_count.unwrap_or(p).max(1)),
            None => 1,
        };
        let bytes = crate::pdf::rasterize_page(path, page, backend)
            .map_err(|e| anyhow::anyhow!("PDF render failed: {e}"))?;
        let (pixel_w, pixel_h) = image::load_from_memory(&bytes)
            .map(|img| (img.width(), img.height()))
            .map_err(|e| anyhow::anyhow!("Could not decode rasterised PDF: {e}"))?;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // A whole-buffer swap like every other opener: caches memoised on
        // edit_seq (conflicts, git marks) must not survive into this tab.
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.image = Some(ImageView {
            bytes,
            format_label: String::from("PDF"),
            pixel_w,
            pixel_h,
            byte_size: meta.len(),
            generation: next_image_generation(),
            pdf: Some(PdfState {
                source_path: path.to_path_buf(),
                current_page: page,
                page_count,
                backend,
                source_byte_size: meta.len(),
                links: None,
            }),
        });
        // A diff view is superseded like every other kind: `open`'s text
        // tail clears it, but the preview openers return before reaching
        // that tail, and the render's arrow-rect clearing sits after the
        // preview arms' early returns — so a diff tab reopened as a
        // preview kept its label, caret, and CLICKABLE hunk arrows
        // (#187 review). Clear the state and the frame-truth rects here.
        self.diff = None;
        self.merge = None;
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();
        self.sheet = None;
        self.hex = None;
        self.log = None;
        self.archive = None;
        self.status = format!("Opened PDF {}", path.display());
        Ok(())
    }

    /// True when this tab shows any non-text view: a diff, sheet,
    /// image/PDF, or hex tab. Text-only affordances (save, LSP sync,
    /// vim, merge commands, encoding, markdown preview) refuse on it.
    /// Every preview kind the format epic (#184) adds joins HERE, so the
    /// twenty-odd guard sites never enumerate kinds again.
    pub fn has_non_text_view(&self) -> bool {
        self.diff.is_some()
            || self.sheet.is_some()
            || self.image.is_some()
            || self.hex.is_some()
            || self.archive.is_some()
            // A rendered log's text side is an empty stub, so a save would
            // write one blank line over the file — the #185 truncation class.
            || self.log.is_some()
            // A docx/odt preview's text side is a STUB (#200 review):
            // letting a save through would overwrite the document with
            // the placeholder, the exact #185 class.
            || self
                .markdown_preview
                .as_ref()
                .is_some_and(|md| md.doc_path.is_some())
    }

    /// Open `path` as a rendered ANSI log (#257): colours paint through the
    /// theme's palette and escapes never reach the text. Windowed, so the
    /// file is not read whole. Fails (and the caller falls through to the
    /// normal text path) when the file cannot be indexed.
    pub fn open_log(&mut self, path: &Path) -> Result<()> {
        let view = crate::log_view::LogView::open(path)
            .map_err(|e| anyhow::anyhow!("Log open failed: {e}"))?;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        // Same supersede-every-other-kind reset the other preview openers do.
        self.diff = None;
        self.merge = None;
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();
        self.image = None;
        self.sheet = None;
        self.markdown_preview = None;
        self.archive = None;
        self.hex = None;
        self.log = Some(view);
        self.status = format!("Opened {} as a rendered log", path.display());
        Ok(())
    }

    /// Whether `path` should open as a rendered log: a log-ish extension, or
    /// any extension at all if the first bytes carry an SGR sequence. The
    /// content sniff is what catches `foo.txt` holding pytest output; the
    /// extension list is what avoids sniffing every file on open.
    pub fn should_open_as_log(path: &Path) -> bool {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(ext.as_str(), "log" | "ansi" | "out" | "txt" | "") {
            return false;
        }
        // Sniff a bounded prefix: a colour log announces itself immediately,
        // and reading more would tax every plain .txt open.
        let Ok(mut f) = std::fs::File::open(path) else {
            return false;
        };
        use std::io::Read;
        let mut buf = [0u8; 8192];
        let Ok(n) = f.read(&mut buf) else {
            return false;
        };
        crate::ansi_text::looks_like_ansi(&String::from_utf8_lossy(&buf[..n]))
    }

    /// Open `path` in the read-only hex viewer (#172): the routing
    /// fallback for files the text heuristic rejects, and the target of
    /// the explicit "Reopen as Hex" command. `pub` for that command's
    /// dispatch; extension routing never needs to call it directly.
    pub fn open_hex(&mut self, path: &Path) -> Result<()> {
        // `open` is also the same-path reload behind the FS-sync sweep:
        // refresh the existing view in place so the reader keeps their
        // offset through an external rewrite (the PDF restore-page
        // precedent, without the second-render window).
        if self.path.as_deref() == Some(path)
            && let Some(view) = self.hex.as_mut()
        {
            // Pending overwrites survive a same-path re-open (a tree
            // re-click must not silently drop them); the FS sweep never
            // reloads a dirty tab, and an explicit Revert discards via
            // `discard_edits` first, which re-arms this refresh.
            if view.has_edits() {
                return Ok(());
            }
            view.refresh_from_disk(path)
                .map_err(|e| anyhow::anyhow!("Hex reload failed: {e}"))?;
            self.disk_stamp = Self::disk_stamp_of(path);
            self.disk_conflict = false;
            return Ok(());
        }
        let view =
            crate::hex::HexView::open(path).map_err(|e| anyhow::anyhow!("Hex open failed: {e}"))?;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // A whole-buffer swap like every other opener: caches memoised on
        // edit_seq (conflicts, git marks) must not survive into this tab.
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        // A diff view is superseded like every other kind: `open`'s text
        // tail clears it, but the preview openers return before reaching
        // that tail, and the render's arrow-rect clearing sits after the
        // preview arms' early returns — so a diff tab reopened as a
        // preview kept its label, caret, and CLICKABLE hunk arrows
        // (#187 review). Clear the state and the frame-truth rects here.
        self.diff = None;
        self.merge = None;
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();
        self.image = None;
        self.sheet = None;
        self.markdown_preview = None;
        self.archive = None;
        // The render dispatch checks `log` BEFORE `hex`, so a stale log would
        // keep painting after "Reopen as Hex" reported success.
        self.log = None;
        self.hex = Some(view);
        self.status = format!("Opened {} in the hex viewer", path.display());
        Ok(())
    }

    /// Write a hex tab's pending byte overwrites to disk (#173). The
    /// same disk-conflict contract as `save_to_disk`: an external change
    /// since the last sync refuses (once), and `force` overrides. Never
    /// routes through `write_buffer_to_disk` — that choke point refuses
    /// every preview kind (#185), and hex writes bytes, not lines.
    pub fn hex_save(&mut self, force: bool) -> Result<SaveOutcome> {
        let Some(path) = self.path.clone() else {
            anyhow::bail!("No file open");
        };
        if self.hex.is_none() {
            anyhow::bail!("Not a hex tab");
        }
        // Nothing pending: touch NOTHING (#191 review). Re-anchoring the
        // disk stamp here would claim sync with an external change the
        // window never read, and the FS sweep would then skip the reload.
        if self.hex.as_ref().is_some_and(|v| !v.has_edits()) {
            self.status = String::from("No pending byte edits");
            return Ok(SaveOutcome::Saved);
        }
        if !force && self.disk_changed_externally() {
            self.disk_conflict = true;
            return Ok(SaveOutcome::DiskConflict);
        }
        let Some(view) = self.hex.as_mut() else {
            unreachable!("checked above");
        };
        let written = view
            .save_edits(&path)
            .map_err(|e| anyhow::anyhow!("Hex save failed: {e}"))?;
        self.disk_stamp = Self::disk_stamp_of(&path);
        self.disk_conflict = false;
        self.dirty = false;
        self.status = format!(
            "Wrote {written} byte{} to {}",
            if written == 1 { "" } else { "s" },
            path.display()
        );
        Ok(SaveOutcome::Saved)
    }

    /// Write a CSV/TSV sheet tab's grid edits back to disk (#177),
    /// serialised with the delimiter the file was read with. Same
    /// disk-conflict contract as text saves; never the text choke point
    /// (#185).
    pub fn sheet_save(&mut self, force: bool, overwrite_formulas: bool) -> Result<SaveOutcome> {
        let Some(path) = self.path.clone() else {
            anyhow::bail!("No file open");
        };
        let Some(view) = self.sheet.as_ref() else {
            anyhow::bail!("Not a sheet tab");
        };
        let delim = match view.kind {
            crate::sheet::SheetKind::Csv => Some(b','),
            crate::sheet::SheetKind::Tsv => Some(b'\t'),
            crate::sheet::SheetKind::Xlsx => None,
            crate::sheet::SheetKind::Sqlite => {
                anyhow::bail!("SQLite databases are read-only in the browser")
            }
            _ => anyhow::bail!("This workbook format is read-only (xls/ods/xlsb)"),
        };
        if !force && self.disk_changed_externally() {
            self.disk_conflict = true;
            return Ok(SaveOutcome::DiskConflict);
        }
        let view = self.sheet.as_mut().expect("checked above");
        match delim {
            Some(delim) => {
                let data = match view.sheets.get(view.current_sheet) {
                    Some(d) => d,
                    None => anyhow::bail!("No worksheet"),
                };
                let bytes = crate::sheet::serialize_delimited(data, delim);
                std::fs::write(&path, &bytes)
                    .map_err(|e| anyhow::anyhow!("Sheet save failed: {e}"))?;
                view.dirty = false;
                view.cell_edits.clear();
                self.status = format!("Saved {}", path.display());
            }
            None => {
                // xlsx (#178): apply exactly the edited cells through
                // umya. `force` doubles as formula-overwrite consent, the
                // same double-press contract as disk conflicts; skipped
                // formula cells stay pending so the tab stays dirty.
                let edits = view.cell_edits.clone();
                let report =
                    crate::sheet::save_xlsx_edits(&path, &view.sheets, &edits, overwrite_formulas)
                        .map_err(|e| anyhow::anyhow!("Workbook save failed: {e}"))?;
                if report.formula_skipped.is_empty() {
                    view.cell_edits.clear();
                    view.dirty = false;
                    self.status = format!(
                        "Saved {} ({} cell{})",
                        path.display(),
                        report.written,
                        if report.written == 1 { "" } else { "s" }
                    );
                } else {
                    // Keep ONLY the skipped formula cells pending.
                    let names = &view.sheets;
                    view.cell_edits.retain(|&(si, r, c)| {
                        names.get(si).is_some_and(|d| {
                            report.formula_skipped.contains(&format!(
                                "{}!{}{}",
                                d.name,
                                crate::sheet::column_letters_pub(d.origin.1 + c as u32 + 1),
                                d.origin.0 + r as u32 + 2
                            ))
                        })
                    });
                    view.dirty = true;
                    self.status = format!(
                        "Saved {} cell{}; {} formula cell{} kept ({}) - Cmd+S again overwrites the formulas",
                        report.written,
                        if report.written == 1 { "" } else { "s" },
                        report.formula_skipped.len(),
                        if report.formula_skipped.len() == 1 {
                            ""
                        } else {
                            "s"
                        },
                        report.formula_skipped.join(", ")
                    );
                }
            }
        }
        let still_dirty = self.sheet.as_ref().is_some_and(|v| v.dirty);
        self.disk_stamp = Self::disk_stamp_of(&path);
        self.disk_conflict = false;
        self.dirty = still_dirty;
        Ok(SaveOutcome::Saved)
    }

    /// The page an open PDF preview is showing, if this tab is one.
    pub fn pdf_page(&self) -> Option<u32> {
        self.image.as_ref()?.pdf.as_ref().map(|p| p.current_page)
    }

    /// Step the active PDF preview by `delta` pages. Returns true if the page
    /// actually changed, so the caller can flag the OSC overlay for re-bake.
    /// Wraps around at the document boundaries when the page count is known;
    /// clamps at page 1 below otherwise.
    pub fn change_pdf_page(&mut self, delta: i32) -> bool {
        let Some(pdf) = self.image.as_ref().and_then(|i| i.pdf.clone()) else {
            return false;
        };
        let new_page = if let Some(total) = pdf.page_count {
            if total == 0 {
                return false;
            }
            let cur = pdf.current_page as i64;
            let next = ((cur - 1 + delta as i64).rem_euclid(total as i64)) + 1;
            next as u32
        } else if delta > 0 {
            pdf.current_page.saturating_add(delta as u32)
        } else {
            pdf.current_page.saturating_sub((-delta) as u32).max(1)
        };
        self.render_pdf_page(new_page)
    }

    /// Jump the active PDF preview to an absolute page, clamped to the
    /// document. `u32::MAX` therefore means "last page" without the caller
    /// knowing the count - which is what Home / End need, and what stepping
    /// by a huge delta cannot express (a step wraps).
    pub fn set_pdf_page(&mut self, page: u32) -> bool {
        let Some(pdf) = self.image.as_ref().and_then(|i| i.pdf.clone()) else {
            return false;
        };
        // "Last page" is unanswerable without a page count (sips-only Macs
        // where mdls reports nothing): rendering the sentinel itself would
        // just spray "PDF page 4294967295 failed" into the status bar.
        if page == u32::MAX && pdf.page_count.is_none() {
            self.status = String::from("PDF page count unknown; cannot jump to the last page");
            return false;
        }
        let last = pdf.page_count.unwrap_or(u32::MAX).max(1);
        self.render_pdf_page(page.clamp(1, last))
    }

    fn render_pdf_page(&mut self, new_page: u32) -> bool {
        let Some(image) = self.image.as_mut() else {
            return false;
        };
        let Some(pdf) = image.pdf.clone() else {
            return false;
        };
        if new_page == pdf.current_page {
            return false;
        }
        let bytes = match crate::pdf::rasterize_page(&pdf.source_path, new_page, pdf.backend) {
            Ok(b) => b,
            Err(e) => {
                self.status = format!("PDF page {new_page} failed: {e}");
                return false;
            }
        };
        let (pixel_w, pixel_h) = match image::load_from_memory(&bytes) {
            Ok(img) => (img.width(), img.height()),
            Err(_) => return false,
        };
        image.bytes = bytes;
        image.generation = next_image_generation();
        image.pixel_w = pixel_w;
        image.pixel_h = pixel_h;
        if let Some(state) = image.pdf.as_mut() {
            state.current_page = new_page;
            state.links = None;
        }
        true
    }

    /// Recompute this editor's syntax spans against the current global palette.
    /// Called on a theme switch: cached spans carry baked colors, so without
    /// this the open file keeps the old theme's code colors until the next edit.
    pub fn rehighlight_for_theme(&mut self) {
        self.recompute_highlights();
        // The markdown preview bakes the theme into its lines at build time
        // and rebuilds only when the buffer edits (`built_seq`), so a theme
        // switch must rebuild it here or it keeps the old colors until the
        // user touches the file. Requires `self.theme` to already carry the
        // new theme (the caller assigns it first).
        if let Some(doc) = self
            .markdown_preview
            .as_ref()
            .and_then(|md| md.doc_path.clone())
        {
            let is_media = self.markdown_preview.as_ref().is_some_and(|md| md.media);
            let scroll = self
                .markdown_preview
                .as_ref()
                .map(|m| m.scroll)
                .unwrap_or(0);
            // Media cards re-probe headers; documents re-walk XML (#183).
            let rebuilt = if is_media {
                self.open_media_preview(&doc).is_ok()
            } else {
                self.open_doc_preview(&doc).is_ok()
            };
            if rebuilt && let Some(md) = self.markdown_preview.as_mut() {
                md.scroll = scroll;
            }
        } else if self.markdown_preview.as_ref().is_some_and(|md| md.notebook) {
            let scroll = self
                .markdown_preview
                .as_ref()
                .map(|m| m.scroll)
                .unwrap_or(0);
            if self.build_notebook_preview() {
                if let Some(md) = self.markdown_preview.as_mut() {
                    md.scroll = scroll;
                }
            } else {
                // Same fallback as the stale path (#199 review): an
                // invalid document drops to the raw text.
                self.markdown_preview = None;
            }
        } else if self.markdown_preview.is_some() {
            self.rebuild_markdown_preview();
        }
    }

    /// Rebuild an open Markdown preview from the current buffer, theme and
    /// captured outputs. No-op when no preview is open.
    ///
    /// Extracted so a settled capture (#354) and a theme change share one
    /// implementation: the theme path had a hard-won detail the other would
    /// otherwise have had to rediscover.
    pub fn rebuild_markdown_preview(&mut self) {
        if self.markdown_preview.is_none() {
            return;
        }
        // The image-aware builder (#196 review): the plain one left
        // md.images pointing at anchors into the REPLACED lines, and
        // the wrap-key recompute then sliced with stale first_line
        // values - an out-of-range panic on a theme switch.
        let text = self.lines.join("\n");
        let base = self
            .path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let (lines, images, runnables) = crate::markdown::render_markdown_full(
            &text,
            self.theme,
            &mut self.registry,
            base.as_deref(),
            self.md_outputs.clone(),
        );
        if let Some(md) = self.markdown_preview.as_mut() {
            md.lines = lines;
            md.images = images;
            md.runnables = runnables;
            md.built_seq = self.edit_seq;
        }
    }

    fn recompute_highlights(&mut self) {
        match self.lang {
            Some(kind) => {
                let text = self.lines.join("\n");
                let bytes = text.as_bytes();
                let line_starts = compute_line_starts(bytes);
                let (spans, protected) = crate::highlight::highlight_text_with_protected(
                    &mut self.registry,
                    kind,
                    bytes,
                    &line_starts,
                );
                self.highlights = spans;
                self.bracket_colors = scan_bracket_colors(&self.lines, &protected);
            }
            None => {
                self.highlights = vec![Vec::new(); self.lines.len()];
                // No grammar means no string/comment knowledge; brackets in
                // plain text still colorize (VS Code does the same) — unless
                // the buffer is big enough that the per-edit scan would make
                // typing in a large log pay for every character.
                let bytes: usize = self.lines.iter().map(String::len).sum();
                self.bracket_colors = if bytes > BRACKET_SCAN_MAX_BYTES {
                    Vec::new()
                } else {
                    scan_bracket_colors(&self.lines, &[])
                };
            }
        }
        self.recompute_semantic_overlay();
        self.recompute_diagnostic_spans();
        self.recompute_inlay_spans();
    }

    /// Store a fresh semantic-token batch from the LSP and decode it into
    /// the per-line overlay. Called by the app when `drain_semantic_tokens`
    /// yields a batch for this editor's file.
    pub fn apply_semantic_tokens(
        &mut self,
        path: PathBuf,
        data: Vec<u32>,
        legend: std::sync::Arc<Vec<String>>,
        is_full: bool,
    ) {
        // A viewport-only (range) batch must never clobber a whole-document
        // batch already in place for the same file: the full set colours the
        // off-screen lines the range set omits, so letting a late range reply
        // win would blank them. Full batches always win; a range batch only
        // applies as the first paint, before the full reply arrives.
        let same_file = self.semantic_path.as_deref() == Some(path.as_path());
        if !is_full && same_file && self.semantic_is_full {
            return;
        }
        self.semantic_path = Some(path);
        self.semantic_data = data;
        self.semantic_legend = Some(legend);
        self.semantic_is_full = is_full;
        self.recompute_semantic_overlay();
    }

    /// Store a fresh inlay-hint set from the LSP and decode it into per-line
    /// `(char_col, label)` runs. Called by the app when `drain_inlay_hints`
    /// yields a batch for this editor's file (an empty set clears them).
    pub fn apply_inlay_hints(
        &mut self,
        path: PathBuf,
        hints: Vec<crate::lsp::manager::InlayHintItem>,
    ) {
        self.inlay_path = Some(path);
        self.inlay_hints = hints;
        self.recompute_inlay_spans();
    }

    /// Install the server's link set (#254): wholesale replace.
    pub fn apply_document_links(
        &mut self,
        path: PathBuf,
        links: Vec<crate::lsp::manager::DocumentLinkItem>,
    ) {
        self.doc_links_path = Some(path);
        self.doc_links = links;
    }

    /// The link target under `(row, col)` — the Ctrl+click dispatch's
    /// lookup, valid only for the stamped file. Range ends are exclusive
    /// like every LSP range.
    pub fn document_link_at(&self, row: usize, col: usize) -> Option<&str> {
        if self.doc_links_path.as_deref() != self.path.as_deref() {
            return None;
        }
        self.doc_links
            .iter()
            .find(|l| {
                let (sr, er) = (l.line as usize, l.end_line as usize);
                if row < sr || row > er {
                    return false;
                }
                let sc = self
                    .lines
                    .get(sr)
                    .map(|t| utf16_to_char_col(t, l.character))
                    .unwrap_or(0);
                let ec = self
                    .lines
                    .get(er)
                    .map(|t| utf16_to_char_col(t, l.end_character))
                    .unwrap_or(0);
                (row > sr || col >= sc) && (row < er || col < ec)
            })
            .map(|l| l.target.as_str())
    }

    /// Install the document's color values (#254): wholesale replace,
    /// like inlay hints — an empty batch clears the swatches.
    pub fn apply_document_colors(
        &mut self,
        path: PathBuf,
        colors: Vec<crate::lsp::manager::ColorItem>,
    ) {
        self.color_path = Some(path);
        self.color_infos = colors;
        self.recompute_inlay_spans();
    }

    /// The color value whose range contains `(row, col)` — the Change
    /// Color Presentation picker's target lookup.
    pub fn color_at(&self, row: usize, col: usize) -> Option<&crate::lsp::manager::ColorItem> {
        if self.color_path.as_deref() != self.path.as_deref() {
            return None;
        }
        self.color_infos.iter().find(|c| {
            let (sr, er) = (c.line as usize, c.end_line as usize);
            if row < sr || row > er {
                return false;
            }
            let sc = self.utf16_col_to_char_pub(sr, c.character);
            let ec = self.utf16_col_to_char_pub(er, c.end_character);
            (row > sr || col >= sc) && (row < er || col <= ec)
        })
    }

    /// Public UTF-16 → char-column bridge for one row.
    pub fn utf16_col_to_char_pub(&self, row: usize, character: u32) -> usize {
        self.lines
            .get(row)
            .map(|l| utf16_to_char_col(l, character))
            .unwrap_or(0)
    }

    /// Install the occurrences of the symbol under the caret, converting the
    /// server's UTF-16 columns to character columns row by row. A multi-line
    /// occurrence (rare, but legal) tints its first row from the start column,
    /// its last row up to the end column, and any rows between end to end.
    pub fn apply_occurrences(&mut self, items: Vec<crate::lsp::manager::OccurrenceItem>) {
        self.occurrences.clear();
        for item in items {
            for row in item.start_line..=item.end_line {
                let row = row as usize;
                let Some(text) = self.lines.get(row) else {
                    continue;
                };
                let cols = text.chars().count();
                let start = if row as u32 == item.start_line {
                    utf16_to_char_col(text, item.start_char).min(cols)
                } else {
                    0
                };
                let end = if row as u32 == item.end_line {
                    utf16_to_char_col(text, item.end_char).min(cols)
                } else {
                    cols
                };
                if end > start {
                    self.occurrences.push((row, start, end, item.write));
                }
            }
        }
    }

    /// Drop the occurrence tints (caret moved; the app re-requests on idle).
    pub fn clear_occurrences(&mut self) -> bool {
        let had = !self.occurrences.is_empty();
        self.occurrences.clear();
        had
    }

    /// Drop every inlay hint (the "Editor: Toggle Inlay Hints" off switch).
    pub fn clear_inlay_hints(&mut self) {
        self.inlay_hints = Vec::new();
        self.inlay_path = None;
        self.recompute_inlay_spans();
    }

    /// Re-decode the retained hints into per-logical-line `(char_col, label)`
    /// runs against the current buffer. A no-op (clears the runs) unless the
    /// batch belongs to the loaded file. Between an edit and the next reply
    /// the anchors drift with the old positions (clamped to the line), exactly
    /// like VS Code until its refresh lands; the app replaces the set once the
    /// server answers for the new edit seq.
    fn recompute_inlay_spans(&mut self) {
        // Hint cells change the display width of their lines.
        self.hscroll_content_cols = None;
        let same_file = self.inlay_path.as_deref() == self.path.as_deref();
        let colors_same_file = self.color_path.as_deref() == self.path.as_deref();
        let hints_live = same_file && !self.inlay_hints.is_empty();
        let colors_live = colors_same_file && !self.color_infos.is_empty();
        if !hints_live && !colors_live {
            self.inlay_spans = Vec::new();
            return;
        }
        let mut spans: Vec<Vec<(usize, String, Option<Color>)>> =
            vec![Vec::new(); self.lines.len()];
        if hints_live {
            for h in &self.inlay_hints {
                let Some(text) = self.lines.get(h.line as usize) else {
                    continue;
                };
                let col = utf16_to_char_col(text, h.character).min(text.chars().count());
                spans[h.line as usize].push((col, h.label.clone(), None));
            }
        }
        // Document-color swatches (#254): a one-cell `■` virtual span
        // whose foreground IS the color, anchored at the value's start.
        if colors_live {
            for c in &self.color_infos {
                let Some(text) = self.lines.get(c.line as usize) else {
                    continue;
                };
                let col = utf16_to_char_col(text, c.character).min(text.chars().count());
                spans[c.line as usize].push((
                    col,
                    String::from("\u{25a0}"),
                    Some(Color::Rgb(c.red, c.green, c.blue)),
                ));
            }
        }
        for line in &mut spans {
            line.sort_by_key(|(c, _, _)| *c);
        }
        self.inlay_spans = spans;
    }

    /// Inlay hints of logical line `line`, or nothing in wrap mode: the
    /// wrapped row segmentation knows nothing about hint cells.
    /// ponytail: hints skip wrap mode; code files don't wrap by default, and
    /// Markdown (the wrapping default) has no hint-serving server.
    fn row_inlay_spans(&self, line: usize) -> &[(usize, String, Option<Color>)] {
        if self.wrap_enabled() {
            return &[];
        }
        self.inlay_spans.get(line).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The buffer text to key a semantic-token cache entry on, but only when
    /// the buffer is clean: a dirty buffer no longer matches the on-disk
    /// content the next open will read, so caching it would key the entry under
    /// the wrong content. Returns the same normalised (`\n`-joined) text the
    /// app sends to the server, so the key matches on both store and load.
    pub fn clean_cache_text(&self) -> Option<String> {
        (!self.dirty).then(|| self.lines.join("\n"))
    }

    #[cfg(test)]
    pub(crate) fn semantic_overlay_for_test(&self) -> &[Vec<HiSpan>] {
        &self.semantic_overlay
    }

    #[cfg(test)]
    pub(crate) fn occurrence_count_for_test(&self) -> usize {
        self.occurrences.len()
    }

    #[cfg(test)]
    pub(crate) fn lines_for_test(&self) -> &[String] {
        &self.lines
    }

    /// Store a fresh diagnostics set for `path` and decode it into per-line
    /// underline runs. Called by the app when `drain_diagnostics` yields a
    /// batch for this editor's file (an empty `diagnostics` clears them, which
    /// is how an "all clear" republish erases the squiggles).
    pub fn apply_diagnostics(
        &mut self,
        path: PathBuf,
        diagnostics: Vec<crate::lsp::manager::Diagnostic>,
    ) {
        self.diagnostics_path = Some(path);
        self.diagnostics = diagnostics;
        self.recompute_diagnostic_spans();
    }

    /// The file the editor's current diagnostics belong to, if any. The app
    /// reads this to re-apply the stored set after a tab switch (diagnostics
    /// are server-pushed, never re-requested on focus like semantic tokens).
    pub fn diagnostics_path(&self) -> Option<&Path> {
        self.diagnostics_path.as_deref()
    }

    /// Clones of the diagnostics whose range intersects the inclusive logical
    /// line span `[start_line, end_line]`. Passed as `codeAction` context so the
    /// language server can attach quick fixes to the errors under the cursor.
    pub fn diagnostics_in_line_range(
        &self,
        start_line: u32,
        end_line: u32,
    ) -> Vec<crate::lsp::manager::Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.start_line <= end_line && d.end_line >= start_line)
            .cloned()
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_spans_for_test(
        &self,
    ) -> &[Vec<(usize, usize, crate::lsp::manager::DiagnosticSeverity)>] {
        &self.diagnostic_spans
    }

    /// Re-decode the retained diagnostics into per-logical-line character runs
    /// against the current buffer. A no-op (clears the runs) unless the batch
    /// belongs to the loaded file, so stale squiggles never bleed across a tab
    /// switch. A zero-width diagnostic (e.g. a "missing token" pointing between
    /// two characters) is widened to one cell so it is still visible, matching
    /// VS Code drawing a squiggle under at least one character.
    fn recompute_diagnostic_spans(&mut self) {
        let same_file = self.diagnostics_path.as_deref() == self.path.as_deref();
        if !same_file || self.diagnostics.is_empty() {
            self.diagnostic_spans = Vec::new();
            return;
        }
        let mut spans: Vec<Vec<(usize, usize, crate::lsp::manager::DiagnosticSeverity)>> =
            vec![Vec::new(); self.lines.len()];
        for d in &self.diagnostics {
            let start_line = d.start_line as usize;
            let end_line = d.end_line as usize;
            // Zip bounds the walk to the buffer, replacing the per-line
            // `lines.get` check a stale diagnostic range would otherwise need.
            for (line, (text, line_spans)) in self
                .lines
                .iter()
                .zip(spans.iter_mut())
                .enumerate()
                .take(end_line + 1)
                .skip(start_line)
            {
                let line_chars = text.chars().count();
                let from = if line == start_line {
                    utf16_to_char_col(text, d.start_char)
                } else {
                    0
                };
                let to = if line == end_line {
                    utf16_to_char_col(text, d.end_char)
                } else {
                    line_chars
                };
                // Widen an empty run to one cell so a point diagnostic is seen.
                let to = to.max(from + 1);
                line_spans.push((from, to, d.severity));
            }
        }
        self.diagnostic_spans = spans;
    }

    /// Re-decode the retained semantic-token batch against the current
    /// buffer. A no-op (clears the overlay) unless the batch belongs to
    /// the file currently loaded, so a stale batch never colors the wrong
    /// file after a tab switch.
    fn recompute_semantic_overlay(&mut self) {
        let same_file = self.semantic_path.as_deref() == self.path.as_deref();
        let Some(legend) = self.semantic_legend.as_ref().filter(|_| same_file) else {
            self.semantic_overlay = Vec::new();
            return;
        };
        if self.semantic_data.is_empty() {
            self.semantic_overlay = Vec::new();
            return;
        }
        let text = self.lines.join("\n");
        let bytes = text.as_bytes();
        let line_starts = compute_line_starts(bytes);
        self.semantic_overlay =
            decode_semantic_tokens(&self.semantic_data, legend, bytes, &line_starts);
    }

    /// Number of logical lines in the buffer (the inlay-hint request range).
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Markdown: Toggle Preview (Cmd/Ctrl+Shift+V). Returns false when the
    /// active tab is not a Markdown text buffer (the caller reports why).
    pub fn toggle_markdown_preview(&mut self) -> bool {
        if self.markdown_preview.take().is_some() {
            return true;
        }
        let is_notebook = self
            .path
            .as_deref()
            .is_some_and(|p| p.extension().and_then(|e| e.to_str()) == Some("ipynb"))
            && !self.has_non_text_view();
        if is_notebook {
            return self.build_notebook_preview();
        }
        let is_md_text = matches!(self.lang, Some(LangKind::Markdown))
            && self.image.is_none()
            && self.sheet.is_none()
            && self.diff.is_none();
        if !is_md_text {
            return false;
        }
        let text = self.lines.join("\n");
        let base = self
            .path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let (lines, images, runnables) = crate::markdown::render_markdown_full(
            &text,
            self.theme,
            &mut self.registry,
            base.as_deref(),
            self.md_outputs.clone(),
        );
        self.markdown_preview = Some(crate::markdown::MarkdownPreview {
            rows: Vec::new(),
            selection: None,
            dragging: false,
            lines,
            scroll: 0,
            built_seq: self.edit_seq,
            images,
            anchor_rows: Vec::new(),
            runnables,
            run_rows: Vec::new(),
            wrap_key: (0, 0),
            last_area: Rect::default(),
            notebook: false,
            doc_path: None,
            media: false,
        });
        true
    }

    /// Build (or rebuild) the rendered notebook view (#180) over the
    /// raw-JSON text tab. Returns false when the JSON does not parse as
    /// a notebook - the tab stays plain text.
    pub fn build_notebook_preview(&mut self) -> bool {
        let text = self.lines.join("\n");
        if !crate::notebook::looks_like_notebook(&text) {
            return false;
        }
        let base = self
            .path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let scratch = std::env::temp_dir().join("croft-notebook-outputs");
        let Some((lines, images)) = crate::notebook::render(
            &text,
            self.theme,
            &mut self.registry,
            base.as_deref(),
            &scratch,
        ) else {
            return false;
        };
        self.markdown_preview = Some(crate::markdown::MarkdownPreview {
            rows: Vec::new(),
            selection: None,
            dragging: false,
            lines,
            scroll: 0,
            built_seq: self.edit_seq,
            images,
            anchor_rows: Vec::new(),
            runnables: Vec::new(),
            run_rows: Vec::new(),
            wrap_key: (0, 0),
            last_area: Rect::default(),
            notebook: true,
            doc_path: None,
            media: false,
        });
        true
    }

    /// Scroll the active Markdown preview; returns false when none is open
    /// so the caller falls through to normal buffer scrolling.
    pub fn scroll_markdown_preview(&mut self, delta: i32) -> bool {
        let Some(md) = self.markdown_preview.as_mut() else {
            return false;
        };
        md.scroll = md.scroll.saturating_add_signed(delta as i16);
        true
    }

    fn line_char_len(&self, row: usize) -> usize {
        self.lines.get(row).map(|s| s.chars().count()).unwrap_or(0)
    }

    fn byte_index(&self, row: usize, col: usize) -> usize {
        let line = &self.lines[row];
        line.char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(line.len())
    }

    /// Any user-driven mutation pins the buffer (matches VS Code: typing in
    /// a preview tab promotes it to a regular tab so a subsequent
    /// single-click on another file can't silently replace your edits).
    fn pin_on_edit(&mut self) {
        self.preview = false;
        // Every mutating entry point calls this, so it is the one place that
        // can guarantee no edit ever lands on a line hidden inside a fold.
        self.reveal_cursor_fold();
    }

    pub fn insert_char(&mut self, c: char) {
        if self.auto_close_pairs && self.insert_char_with_pairs(c) {
            // Still a keystroke: on-type formatting (#254) keys off the
            // typed character, whether or not the pair machinery handled it.
            self.last_typed = Some((c, self.edit_seq));
            return;
        }
        self.pin_on_edit();
        // Selection-replace counts as one logical edit (Replace), not two.
        // Coalesce subsequent typed chars onto the same step only when the
        // previous edit was also a single-char insert with no selection.
        let had_selection = self.selection.map(|s| s.has_area()).unwrap_or(false);
        let kind = if had_selection {
            EditKind::DeleteSelection
        } else {
            EditKind::InsertChar
        };
        self.push_undo(kind);
        self.delete_selection_inner();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let byte = self.byte_index(row, col);
        self.lines[row].insert(byte, c);
        self.cursor_col += 1;
        self.mark_buffer_changed();
        self.recompute_highlights();
        // On-type formatting (#254) reads this to know the last edit was
        // this keystroke (seq must still match at the tick). Set here, not
        // in insert_char_raw: paste and snippet expansion go through the
        // raw path and must never count as typing.
        self.last_typed = Some((c, self.edit_seq));
    }

    /// The auto-closing-pairs behaviors, returning true when the keystroke
    /// was fully handled here (#121). VS Code's default semantics:
    /// type-over for closers/quotes, selection surround for openers/quotes,
    /// pair insertion guarded so a closer is never jammed into a following
    /// word (and a quote never pairs against a preceding word character —
    /// the apostrophe-in-a-word case).
    fn insert_char_with_pairs(&mut self, c: char) -> bool {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let close = auto_close_partner(c);
        let has_sel = self.selection.map(|s| s.has_area()).unwrap_or(false);
        // Selection surround: openers and quotes wrap, closers fall through
        // to the ordinary replace-selection insert.
        if has_sel {
            let Some(close) = close else {
                return false;
            };
            if is_pair_closer(c) {
                return false;
            }
            self.pin_on_edit();
            self.push_undo(EditKind::Paste);
            let ((sr, sc), (er, ec)) = self.selection.unwrap().normalised();
            let close_byte = self.byte_index(er, ec);
            self.lines[er].insert(close_byte, close);
            let open_byte = self.byte_index(sr, sc);
            self.lines[sr].insert(open_byte, c);
            let inner_end = if sr == er { ec + 1 } else { ec };
            self.selection = Some(EditorSelection {
                anchor: (sr, sc + 1),
                head: (er, inner_end),
            });
            self.cursor_row = er;
            self.cursor_col = inner_end;
            self.mark_buffer_changed();
            self.recompute_highlights();
            return true;
        }
        let next = self
            .lines
            .get(self.cursor_row)
            .and_then(|l| l.chars().nth(self.cursor_col));
        // Type-over: the exact closer/quote already sits at the caret.
        if (is_pair_closer(c) || is_pair_quote(c)) && next == Some(c) {
            self.cursor_col += 1;
            self.ensure_cursor_col_visible();
            return true;
        }
        let Some(close) = close else {
            return false;
        };
        if is_pair_closer(c) {
            return false;
        }
        // Opener guard: never before a word character. Quote guard: also
        // never after a word character or the same quote.
        let next_ok = next.is_none_or(|n| !n.is_alphanumeric() && n != '_' && n != c);
        let prev = if self.cursor_col == 0 {
            None
        } else {
            self.lines
                .get(self.cursor_row)
                .and_then(|l| l.chars().nth(self.cursor_col - 1))
        };
        let prev_ok =
            !is_pair_quote(c) || prev.is_none_or(|p| !p.is_alphanumeric() && p != '_' && p != c);
        if !next_ok || !prev_ok {
            return false;
        }
        self.pin_on_edit();
        self.push_undo(EditKind::InsertChar);
        let row = self.cursor_row;
        let byte = self.byte_index(row, self.cursor_col);
        self.lines[row].insert(byte, close);
        self.lines[row].insert(byte, c);
        self.cursor_col += 1;
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.auto_pair_at = Some((self.cursor_row, self.cursor_col, self.edit_seq));
        true
    }

    /// Apply LSP rename edits to this buffer in-memory as a single undo step,
    /// marking the tab dirty (the user saves to persist). Returns the number
    /// of edits applied.
    pub fn apply_span_edits(&mut self, edits: &[TextSpanEdit]) -> usize {
        if edits.is_empty() {
            return 0;
        }
        self.pin_on_edit();
        self.push_undo(EditKind::Paste);
        let n = apply_span_edits_to_lines(&mut self.lines, edits);
        if n > 0 {
            self.mark_buffer_changed();
            self.recompute_highlights();
        }
        n
    }

    /// The buffer's active indentation style: the status-bar override if
    /// set, else the style detected from the file's content on open, else
    /// the language default (2 spaces for YAML, 4 spaces otherwise).
    pub fn indent_style(&self) -> IndentStyle {
        self.indent_override
            .or(self.detected_indent)
            .unwrap_or(IndentStyle {
                width: indent_unit_for(self.lang).chars().count() as u32,
                use_spaces: true,
            })
    }

    /// Pin the buffer's indentation style (status-bar "Indent Using …" /
    /// "Change Tab Display Size"). Affects newly typed indentation only; use
    /// [`Editor::convert_indentation`] to rewrite existing lines.
    pub fn set_indent_style(&mut self, style: IndentStyle) {
        self.indent_override = Some(style);
    }

    /// The indentation unit as text for the active style: N spaces or one tab.
    fn indent_unit(&self) -> String {
        self.indent_style().unit()
    }

    /// The editor's indentation preference as the LSP `FormattingOptions`
    /// fields (`tab_size`, `insert_spaces`).
    pub fn indent_preference(&self) -> (u32, bool) {
        let s = self.indent_style();
        (s.width, s.use_spaces)
    }

    /// Human label for the buffer's language mode, for the status bar.
    pub fn language_label(&self) -> &'static str {
        language_label(self.lang)
    }

    /// The active language, for the status-bar "Change Language Mode" picker
    /// to pre-select the current entry.
    pub fn language(&self) -> Option<LangKind> {
        self.lang
    }

    /// Override the buffer's language mode (status-bar "Change Language Mode").
    /// Re-runs tree-sitter highlighting under the new grammar; the app re-syncs
    /// the LSP separately off the changed language.
    pub fn set_language(&mut self, lang: Option<LangKind>) {
        self.lang = lang;
        self.recompute_highlights();
    }

    /// Re-read the open file and decode it with `enc` (status-bar "Reopen with
    /// Encoding"). Replaces the buffer, resets the cursor, and re-highlights;
    /// subsequent saves re-encode with `enc`. No-op without a backing file.
    pub fn reopen_with_encoding(&mut self, enc: &'static encoding_rs::Encoding) -> Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No file open"))?;
        let bytes = std::fs::read(&path)?;
        // `decode` does its own BOM sniffing and OVERRIDES the encoding passed
        // in when the file carries one, returning what it actually used. Take
        // that, or the buffer would hold text decoded one way while claiming to
        // be another — and the save would then re-encode it wrongly.
        let (decoded, used, _) = enc.decode(&bytes);
        let text = decoded.into_owned();
        self.encoding = used;
        // Re-sniff against the bytes just read: reinterpreting the file under a
        // new encoding must not carry the previous one's BOM answer over.
        self.bom = encoding_rs::Encoding::for_bom(&bytes).is_some();
        self.eol = if text.contains("\r\n") {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        };
        self.lines = split_into_lines(&text);
        // The map described the text that was just replaced (#349). Clearing
        // is the safe direction: an unknown line is correct, a line credited
        // to whoever wrote the file's PREVIOUS contents is not.
        self.provenance = crate::provenance::Provenance::new();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        // A whole-buffer swap invalidates both history stacks: a redo/undo
        // popped afterwards would reinstate the buffer as decoded under the
        // OLD encoding, silently discarding the re-decode.
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.hscroll_content_cols = None;
        self.wrap_total_cache.clear();
        self.scroll = 0;
        self.scroll_sub = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.selection = None;
        // The buffer now matches disk decoded as `enc`, so it is clean; bump the
        // edit seq so the LSP/outline/highlight resync sees the new content.
        self.dirty = false;
        // The encoding just changed, so a refusal recorded against the old
        // one is stale — without this, auto save would keep skipping a tab
        // whose new encoding covers the buffer fine.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.recompute_highlights();
        Ok(())
    }

    /// Clamp the cursor back inside the buffer after an edit that may have
    /// removed rows or shortened the cursor's line (e.g. a whole-document
    /// reformat). Without this a stale `cursor_row`/`cursor_col` can index past
    /// the end of `lines` on the next keystroke.
    pub fn clamp_cursor(&mut self) {
        if self.lines.is_empty() {
            self.cursor_row = 0;
            self.cursor_col = 0;
            return;
        }
        self.cursor_row = self.cursor_row.min(self.lines.len() - 1);
        let max_col = self.lines[self.cursor_row].chars().count();
        self.cursor_col = self.cursor_col.min(max_col);
    }

    pub fn insert_str(&mut self, s: &str) {
        self.insert_str_as(s, crate::provenance::Seat::Me);
    }

    /// [`Self::insert_str`], attributing the inserted lines to `seat` (#349).
    ///
    /// The seat is a parameter rather than editor state because the same
    /// buffer takes text from several of them — the person typing, the
    /// navigator's accepted stream, an agent's write — and which one is
    /// making THIS edit is known only at the call site.
    pub fn insert_str_as(&mut self, s: &str, seat: crate::provenance::Seat) {
        self.pin_on_edit();
        self.push_undo(EditKind::Paste);
        // A selection is deleted FIRST, and the map has to learn about that
        // deletion separately: without this, the lines below a multi-line
        // selection keep their old indices while the buffer's lines shift up,
        // so a surviving line paints as whoever wrote a different one. That
        // is the module's one forbidden outcome, reachable by an ordinary
        // select-and-type.
        if let Some(sel) = self.selection {
            let ((sr, _), (er, _)) = sel.normalised();
            if self.delete_selection_inner() {
                // The selected span collapses to a single line.
                self.provenance.splice(sr, er.saturating_sub(sr) + 1, 1);
            }
        }
        let start_line = self.cursor_row;
        let before = self.lines.len();
        for c in s.chars() {
            if c == '\n' {
                self.insert_newline_raw();
            } else {
                self.insert_char_raw(c);
            }
        }
        // The line the insertion started on is rewritten too, so it counts as
        // written by this seat — hence the `+ 1`. But clamp to the lines that
        // actually EXIST: inserting into an empty buffer pushes the first
        // line, so the count moves 0 -> 1 and a naive `added + 1` claims two
        // lines when there is one. Attributing a line that is not there is
        // precisely what the module's invariant forbids, so the clamp is the
        // invariant rather than defensiveness.
        let added = self.lines.len().saturating_sub(before);
        let end = (start_line + added + 1).min(self.lines.len());
        self.provenance
            .splice(start_line, 1, end.saturating_sub(start_line));
        self.provenance.record(start_line..end, seat);
        // "No key is ever past the end" as a structural guarantee rather than
        // an argument about arithmetic: cheap, and it holds across every path
        // into this function at once instead of one proof per path.
        self.provenance.truncate(self.lines.len());
        self.recompute_highlights();
    }

    fn insert_char_raw(&mut self, c: char) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let byte = self.byte_index(row, col);
        self.lines[row].insert(byte, c);
        self.cursor_col += 1;
        self.mark_buffer_changed();
    }

    /// The language id (VS Code identifier) of this buffer, for snippet scoping.
    pub fn scope_id(&self) -> &'static str {
        language_scope_id(self.lang)
    }

    /// True while a snippet's tab stops are being cycled with Tab.
    pub fn snippet_active(&self) -> bool {
        self.snippet.is_some()
    }

    /// Abandon any active snippet session (a structural key moved the caret
    /// away, so Tab should return to its normal meaning).
    pub fn cancel_snippet(&mut self) {
        self.snippet = None;
    }

    /// Expand snippet `body` at the caret. `prefix_len` chars of the already
    /// typed prefix are removed first, then the resolved text is inserted with
    /// continuation lines indented to the current line. The caret lands on the
    /// first tab stop (selecting its placeholder); when more stops remain a
    /// session starts so Tab cycles through them.
    pub fn expand_snippet(&mut self, body: &str, prefix_len: usize) {
        self.snippet = None;
        for _ in 0..prefix_len {
            self.backspace();
        }
        let start = (self.cursor_row, self.cursor_col);
        let indent: String = self
            .lines
            .get(start.0)
            .map(|l| {
                l.chars()
                    .take(start.1)
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect()
            })
            .unwrap_or_default();
        let indent_len = indent.chars().count();
        let parsed = crate::snippets::parse_body(body);
        let inserted = if indent.is_empty() {
            parsed.text.clone()
        } else {
            parsed.text.replace('\n', &format!("\n{indent}"))
        };
        // A snippet body is text the user chose, not text they wrote (#349).
        self.insert_str_as(&inserted, crate::provenance::Seat::Generated);

        let mut abs: std::collections::VecDeque<(usize, usize, usize)> = parsed
            .stops
            .iter()
            .map(|s| {
                let (ld, col) = crate::snippets::offset_to_line_col(&parsed.text, s.offset);
                let row = start.0 + ld;
                let c = if ld == 0 {
                    start.1 + col
                } else {
                    indent_len + col
                };
                (row, c, s.len)
            })
            .collect();
        let Some(first) = abs.pop_front() else {
            return; // no stops: leave the caret at the end of the insertion
        };
        self.place_at_snippet_stop(first);
        if !abs.is_empty() {
            self.snippet = Some(SnippetSession {
                anchor: (first.0, first.1),
                cur_len: first.2,
                stops: abs,
            });
        }
    }

    /// Advance to the next snippet tab stop. Later stops on the current line are
    /// shifted by the net chars typed at the current stop first. Returns false
    /// when no session is active (so the caller falls back to indenting).
    pub fn snippet_next(&mut self) -> bool {
        let Some(mut sess) = self.snippet.take() else {
            return false;
        };
        // ponytail: only edits made at the current stop, on its own line, are
        // tracked. Typing newlines or editing elsewhere mid-session drifts the
        // later stops; upgrade to real markers if that workflow matters.
        if self.cursor_row == sess.anchor.0 {
            let typed = self.cursor_col as isize - sess.anchor.1 as isize;
            let shift = typed - sess.cur_len as isize;
            if shift != 0 {
                let after = sess.anchor.1 + sess.cur_len;
                for s in sess.stops.iter_mut() {
                    if s.0 == sess.anchor.0 && s.1 >= after {
                        s.1 = (s.1 as isize + shift).max(0) as usize;
                    }
                }
            }
        }
        let Some(next) = sess.stops.pop_front() else {
            return false;
        };
        self.place_at_snippet_stop(next);
        if !sess.stops.is_empty() {
            sess.anchor = (next.0, next.1);
            sess.cur_len = next.2;
            self.snippet = Some(sess);
        }
        true
    }

    /// Move the caret to a resolved tab stop, selecting its placeholder text so
    /// the next keystroke replaces it.
    fn place_at_snippet_stop(&mut self, stop: (usize, usize, usize)) {
        // A stop can outlive its row (a backspace join mid-session shrinks
        // the buffer): clamp to the live grid before planting anything —
        // clamping only the cursor left a selection whose row indexed past
        // the buffer end on the next edit.
        let (row, col, len) = stop;
        let row = row.min(self.lines.len().saturating_sub(1));
        let line_len = self.line_char_len(row);
        let col = col.min(line_len);
        let end = (col + len).min(line_len);
        self.cursor_row = row;
        self.cursor_col = end;
        self.selection = (end > col).then_some(EditorSelection {
            anchor: (row, col),
            head: (row, end),
        });
    }

    fn insert_newline_raw(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let byte = self.byte_index(row, col);
        let right = self.lines[row].split_off(byte);
        self.lines.insert(row + 1, right);
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.mark_buffer_changed();
    }

    pub fn insert_newline(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::Newline);
        self.delete_selection_inner();
        self.smart_insert_newline_inner();
        self.recompute_highlights();
        // Newline is an on-type trigger for several servers (#254).
        self.last_typed = Some(('\n', self.edit_seq));
    }

    fn smart_insert_newline_inner(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let line = self.lines[row].clone();

        let leading: String = line
            .chars()
            .take(col)
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();

        let prefix_chars: Vec<char> = line.chars().take(col).collect();
        let last_non_ws = prefix_chars
            .iter()
            .rev()
            .find(|c| !c.is_whitespace())
            .copied();
        let next_char = line.chars().nth(col);

        let unit = self.indent_unit();
        let extra = if extra_indent_triggered(self.lang, last_non_ws) {
            unit.as_str()
        } else {
            ""
        };
        let pair_split = is_bracket_pair_split(self.lang, last_non_ws, next_char);

        self.insert_newline_raw();

        let new_indent = format!("{leading}{extra}");
        for c in new_indent.chars() {
            self.insert_char_raw(c);
        }

        if pair_split {
            self.insert_newline_raw();
            for c in leading.chars() {
                self.insert_char_raw(c);
            }
            self.cursor_row -= 1;
            self.cursor_col = new_indent.chars().count();
        }
    }

    pub fn backspace(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::Backspace);
        // Between an empty auto-close pair, backspace eats both sides —
        // typing `(` then backspace must round-trip to nothing (#121).
        // ONLY for the pair the last auto-close inserted, still untouched
        // (position and edit_seq both match): a pre-existing `()` in the
        // file keeps its closer (#122 review).
        if self.auto_close_pairs
            && !self.selection.map(|s| s.has_area()).unwrap_or(false)
            && self.cursor_col > 0
            && self.auto_pair_at == Some((self.cursor_row, self.cursor_col, self.edit_seq))
        {
            let line = self.lines.get(self.cursor_row).cloned().unwrap_or_default();
            let prev = line.chars().nth(self.cursor_col - 1);
            let next = line.chars().nth(self.cursor_col);
            if let (Some(p), Some(n)) = (prev, next)
                && auto_close_partner(p) == Some(n)
                && !is_pair_closer(p)
            {
                let byte = self.byte_index(self.cursor_row, self.cursor_col);
                self.lines[self.cursor_row].remove(byte);
            }
        }
        self.backspace_raw();
        self.recompute_highlights();
    }

    /// Backspace body without snapshotting or re-highlighting. Callers that
    /// batch multiple edits (multi-cursor) snapshot once and recompute once.
    fn backspace_raw(&mut self) {
        if self.delete_selection_inner() {
            return;
        }
        if self.cursor_col > 0 {
            let row = self.cursor_row;
            let col = self.cursor_col - 1;
            let from = self.byte_index(row, col);
            let to = self.byte_index(row, col + 1);
            self.lines[row].replace_range(from..to, "");
            self.cursor_col -= 1;
            self.mark_buffer_changed();
        } else if self.cursor_row > 0 {
            let cur = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.line_char_len(self.cursor_row);
            self.lines[self.cursor_row].push_str(&cur);
            self.mark_buffer_changed();
        }
    }

    pub fn delete_forward(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::DeleteForward);
        self.delete_forward_raw();
        self.recompute_highlights();
    }

    /// Forward-delete body without snapshotting or re-highlighting.
    fn delete_forward_raw(&mut self) {
        if self.delete_selection_inner() {
            return;
        }
        let row = self.cursor_row;
        let len = self.line_char_len(row);
        if self.cursor_col < len {
            let from = self.byte_index(row, self.cursor_col);
            let to = self.byte_index(row, self.cursor_col + 1);
            self.lines[row].replace_range(from..to, "");
            self.mark_buffer_changed();
        } else if row + 1 < self.lines.len() {
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
            self.mark_buffer_changed();
        }
    }

    /// True while multi-cursor mode is active (one or more secondary carets
    /// in addition to the primary cursor).
    pub fn has_multi_cursor(&self) -> bool {
        !self.carets.is_empty()
    }

    /// Drop every secondary caret, leaving just the primary cursor. Called on
    /// Esc, mouse clicks, and any movement that isn't a multi-cursor edit.
    pub fn collapse_carets(&mut self) {
        self.carets.clear();
    }

    /// VS Code "Change All Occurrences" (Cmd+F2): select every whole-word,
    /// case-sensitive textual match of the identifier under the cursor in the
    /// current file and turn each into a caret. The match at the cursor (or
    /// the first, if the cursor isn't on one) becomes the primary selection;
    /// the rest become secondary carets. Returns the number of occurrences
    /// selected (0 when the cursor isn't on a word, leaving state untouched).
    pub fn select_all_occurrences_of_word_at_cursor(&mut self) -> usize {
        let Some((start, end)) = self.word_at(self.cursor_row, self.cursor_col) else {
            return 0;
        };
        let word: Vec<char> = self.lines[self.cursor_row]
            .chars()
            .skip(start)
            .take(end - start)
            .collect();
        if word.is_empty() {
            return 0;
        }
        let mut occ: Vec<EditorSelection> = Vec::new();
        for (row, line) in self.lines.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            for col in find_word_occurrences(&chars, &word) {
                occ.push(EditorSelection {
                    anchor: (row, col),
                    head: (row, col + word.len()),
                });
            }
        }
        if occ.is_empty() {
            return 0;
        }
        // The occurrence containing the cursor becomes primary so typing
        // continues where the user is looking; otherwise fall back to the
        // first match in the file.
        let primary_idx = occ
            .iter()
            .position(|s| {
                s.anchor.0 == self.cursor_row
                    && self.cursor_col >= s.anchor.1
                    && self.cursor_col <= s.head.1
            })
            .unwrap_or(0);
        let primary = occ[primary_idx];
        self.selection = Some(primary);
        self.cursor_row = primary.head.0;
        self.cursor_col = primary.head.1;
        self.carets = occ
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i != primary_idx)
            .map(|(_, s)| s)
            .collect();
        self.carets.len() + 1
    }

    /// VS Code "Add Selection to Next Find Match" (`Cmd+D`): the first press
    /// selects the word under the cursor; each further press finds the next
    /// occurrence of the selected text (document order, wrapping) that is not
    /// already a caret and adds it as a secondary caret, moving the primary to
    /// it so the view follows. Returns the number of cursors (0 when the first
    /// press lands off a word, leaving state untouched).
    pub fn select_next_occurrence(&mut self) -> usize {
        // First press with no selection: select the word under the cursor.
        let Some(sel) = self.selection else {
            let Some((s, e)) = self.word_at(self.cursor_row, self.cursor_col) else {
                return 0;
            };
            self.selection = Some(EditorSelection {
                anchor: (self.cursor_row, s),
                head: (self.cursor_row, e),
            });
            self.cursor_col = e;
            self.ensure_cursor_col_visible();
            return 1;
        };
        // Only single-line word selections drive the incremental match search.
        let (start, end) = sel.normalised();
        if start.0 != end.0 {
            return self.carets.len() + 1;
        }
        let word: Vec<char> = self.lines[start.0]
            .chars()
            .skip(start.1)
            .take(end.1 - start.1)
            .collect();
        if word.is_empty() {
            return self.carets.len() + 1;
        }
        // Every occurrence in document order, and the set already selected
        // (the primary plus each existing caret), keyed by its start.
        let mut occ: Vec<(usize, usize)> = Vec::new();
        for (row, line) in self.lines.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            for col in find_word_occurrences(&chars, &word) {
                occ.push((row, col));
            }
        }
        let mut taken: Vec<(usize, usize)> = self.carets.iter().map(|c| c.normalised().0).collect();
        taken.push(start);
        // Scan forward from the primary, wrapping, for the first match not yet
        // taken.
        let from = occ.iter().position(|&o| o == start).unwrap_or(0);
        let n = occ.len();
        let next = (1..=n)
            .map(|k| occ[(from + k) % n])
            .find(|o| !taken.contains(o));
        let Some((row, col)) = next else {
            return self.carets.len() + 1;
        };
        // Demote the current primary to a caret, promote the new match.
        self.carets.push(sel);
        let head = (row, col + word.len());
        self.selection = Some(EditorSelection {
            anchor: (row, col),
            head,
        });
        self.cursor_row = head.0;
        self.cursor_col = head.1;
        self.ensure_cursor_col_visible();
        self.carets.len() + 1
    }

    /// Insert a character at every caret. One undo step.
    ///
    /// Deliberately does not set `last_typed`: an on-type formatting reply
    /// (#254) is computed at ONE position and would be wrong at the other
    /// carets, so multi-cursor typing never arms the trigger.
    pub fn multi_insert_char(&mut self, c: char) {
        self.multi_apply(CaretEdit::Insert(c));
    }

    /// Backspace at every caret. One undo step.
    pub fn multi_backspace(&mut self) {
        self.multi_apply(CaretEdit::Backspace);
    }

    /// Forward-delete at every caret. One undo step.
    pub fn multi_delete_forward(&mut self) {
        self.multi_apply(CaretEdit::DeleteForward);
    }

    /// Apply `edit` at the primary caret and every secondary caret as a
    /// single undo step. Carets are processed bottom-to-top so an edit never
    /// shifts the coordinates of a caret still to be processed.
    fn multi_apply(&mut self, edit: CaretEdit) {
        self.pin_on_edit();
        self.push_undo(EditKind::MultiEdit);

        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        let mut items: Vec<(bool, EditorSelection)> = Vec::with_capacity(self.carets.len() + 1);
        items.push((true, primary));
        for s in &self.carets {
            items.push((false, *s));
        }
        // Descending by start so the lowest caret is edited first.
        items.sort_by_key(|item| std::cmp::Reverse(item.1.normalised().0));
        // Clear so the per-caret raw ops can't see stale carets.
        self.carets.clear();

        let mut new_primary = (self.cursor_row, self.cursor_col);
        let mut new_secondary: Vec<(usize, usize)> = Vec::new();
        for (is_primary, sel) in items {
            if sel.has_area() {
                self.selection = Some(sel);
            } else {
                self.selection = None;
            }
            let last = self.lines.len().saturating_sub(1);
            self.cursor_row = sel.head.0.min(last);
            self.cursor_col = sel.head.1.min(self.line_char_len(self.cursor_row));
            self.apply_caret_edit_raw(edit);
            let pos = (self.cursor_row, self.cursor_col);
            if is_primary {
                new_primary = pos;
            } else {
                new_secondary.push(pos);
            }
        }

        self.selection = None;
        let last = self.lines.len().saturating_sub(1);
        self.cursor_row = new_primary.0.min(last);
        self.cursor_col = new_primary.1.min(self.line_char_len(self.cursor_row));
        new_secondary.sort_unstable();
        self.carets = new_secondary
            .into_iter()
            .map(|(r, c)| EditorSelection::new(r, c))
            .collect();
        self.recompute_highlights();
    }

    /// One caret's worth of a multi-edit: no snapshot, no re-highlight (the
    /// batching caller in `multi_apply` does both once).
    fn apply_caret_edit_raw(&mut self, edit: CaretEdit) {
        match edit {
            CaretEdit::Insert(c) => {
                self.delete_selection_inner();
                self.insert_char_raw(c);
            }
            CaretEdit::Backspace => self.backspace_raw(),
            CaretEdit::DeleteForward => self.delete_forward_raw(),
        }
    }

    pub fn start_selection_at_cursor(&mut self) {
        self.selection = Some(EditorSelection::new(self.cursor_row, self.cursor_col));
    }

    pub fn extend_selection_to_cursor(&mut self) {
        if let Some(sel) = self.selection.as_mut() {
            sel.head = (self.cursor_row, self.cursor_col);
        } else {
            self.start_selection_at_cursor();
        }
    }

    /// An editor `(row, char col)` as an LSP UTF-16 `(line, character)`.
    pub fn pos_to_utf16(&self, row: usize, col: usize) -> (u32, u32) {
        if self.lines.is_empty() {
            return (0, 0);
        }
        let row = row.min(self.lines.len() - 1);
        let line = &self.lines[row];
        let byte = char_byte(line, col.min(line.chars().count()));
        (row as u32, line[..byte].encode_utf16().count() as u32)
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    // ---- Linked editing (#254) ------------------------------------------

    /// Install the server's linked-editing set: UTF-16
    /// `(start_line, start_char, end_line, end_char)` spans. Multi-line
    /// spans (which the protocol forbids anyway) and sets smaller than a
    /// pair are dropped. Positions are snapshotted against the current
    /// buffer (`edit_seq` + per-row lengths).
    pub fn set_linked_ranges(&mut self, spans: &[(u32, u32, u32, u32)]) {
        let mut ranges: Vec<LinkedRange> = spans
            .iter()
            .filter(|&&(sl, _, el, _)| sl == el && (sl as usize) < self.lines.len())
            .map(|&(sl, sc, _, ec)| {
                let row = sl as usize;
                let start = utf16_to_char_col(&self.lines[row], sc);
                let end = utf16_to_char_col(&self.lines[row], ec);
                LinkedRange {
                    row,
                    start,
                    len: end.saturating_sub(start),
                }
            })
            .collect();
        ranges.sort_by_key(|r| (r.row, r.start));
        if ranges.len() < 2 {
            self.clear_linked_ranges();
            return;
        }
        self.linked_ranges = ranges;
        self.resnapshot_linked_rows();
        self.linked_seq = self.edit_seq;
    }

    /// The primary caret as an LSP UTF-16 `(line, character)` pair.
    pub fn cursor_position_utf16(&self) -> (u32, u32) {
        let row = self.cursor_row.min(self.lines.len().saturating_sub(1));
        let line = &self.lines[row];
        let byte = char_byte(line, self.cursor_col.min(line.chars().count()));
        (row as u32, line[..byte].encode_utf16().count() as u32)
    }

    pub fn clear_linked_ranges(&mut self) {
        self.linked_ranges.clear();
        self.linked_rows.clear();
    }

    pub fn has_linked_ranges(&self) -> bool {
        !self.linked_ranges.is_empty()
    }

    /// True when `(row, col)` sits inside (or at the end boundary of) a
    /// linked range — the caret positions where the set stays alive.
    pub fn linked_ranges_contain(&self, row: usize, col: usize) -> bool {
        self.linked_ranges
            .iter()
            .any(|r| r.row == row && col >= r.start && col <= r.start + r.len)
    }

    fn resnapshot_linked_rows(&mut self) {
        let mut rows: Vec<usize> = self.linked_ranges.iter().map(|r| r.row).collect();
        rows.dedup();
        self.linked_rows = rows
            .into_iter()
            .map(|row| (row, self.lines[row].chars().count()))
            .collect();
    }

    /// Replay the last edit across the sibling linked ranges (#254): the
    /// paired-tag auto-rename. Called once per tick after key handling;
    /// returns true when siblings were rewritten. Trusts only a clean
    /// single-step, single-row edit that began inside one range and
    /// keeps the content a plausible tag word — anything else clears
    /// the set rather than corrupt text far from the caret.
    pub fn mirror_linked_edit(&mut self) -> bool {
        if self.linked_ranges.len() < 2 || self.edit_seq == self.linked_seq {
            return false;
        }
        // A single edit advances the seq exactly once; a missed frame
        // (paste burst, undo, reload) is not safely replayable.
        if self.edit_seq != self.linked_seq.wrapping_add(1) {
            self.clear_linked_ranges();
            return false;
        }
        let (erow, ecol) = self.last_edit_origin;
        // Exactly one linked row may have changed, by the edit's delta.
        let changed: Vec<(usize, isize)> = self
            .linked_rows
            .iter()
            .filter_map(|&(row, old_len)| {
                let now = self.lines.get(row)?.chars().count() as isize;
                let delta = now - old_len as isize;
                (delta != 0).then_some((row, delta))
            })
            .collect();
        let delta = match changed.as_slice() {
            [] => 0isize,
            [(row, delta)] if *row == erow => *delta,
            _ => {
                self.clear_linked_ranges();
                return false;
            }
        };
        // The edit must have begun inside one range on that row.
        let Some(i) = self
            .linked_ranges
            .iter()
            .position(|r| r.row == erow && ecol >= r.start && ecol <= r.start + r.len)
        else {
            self.clear_linked_ranges();
            return false;
        };
        let new_len = self.linked_ranges[i].len as isize + delta;
        if new_len < 0 {
            self.clear_linked_ranges();
            return false;
        }
        let new_len = new_len as usize;
        // A boundary-adjacent backspace/delete-forward removes the
        // delimiter OUTSIDE the range, not the range's own content, and
        // leaves the caret outside the updated span. Reject that instead
        // of writing a truncated word into the siblings.
        if self.cursor_row != erow
            || self.cursor_col < self.linked_ranges[i].start
            || self.cursor_col > self.linked_ranges[i].start + new_len
        {
            self.clear_linked_ranges();
            return false;
        }
        let src = self.linked_ranges[i];
        let line = &self.lines[src.row];
        let from = char_byte(line, src.start);
        let to = char_byte(line, src.start + new_len);
        let text: String = line[from..to].to_string();
        // Keep it a plausible identifier/tag word; a delimiter means the
        // user is doing something the mirror shouldn't propagate.
        if text
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '<' | '>' | '/' | '"' | '\''))
        {
            self.clear_linked_ranges();
            return false;
        }
        // Rewrite every sibling, rightmost-first per row so earlier
        // starts stay valid while lengths change.
        let mut order: Vec<usize> = (0..self.linked_ranges.len()).filter(|&j| j != i).collect();
        order.sort_by_key(|&j| {
            let r = &self.linked_ranges[j];
            (r.row, std::cmp::Reverse(r.start))
        });
        for j in order {
            let r = self.linked_ranges[j];
            // Siblings after the edit on the edited row already shifted.
            let start = if r.row == erow && r.start > ecol {
                (r.start as isize + delta).max(0) as usize
            } else {
                r.start
            };
            let line = &mut self.lines[r.row];
            let from = char_byte(line, start);
            let to = char_byte(line, start + r.len);
            line.replace_range(from..to, &text);
        }
        // Recompute stored positions: every range now has `new_len`; on
        // each row, starts shift by the accumulated growth of preceding
        // ranges (the edited range's own delta included).
        let per = new_len as isize - src.len as isize;
        let mut shift: std::collections::HashMap<usize, isize> = std::collections::HashMap::new();
        for r in self.linked_ranges.iter_mut() {
            let acc = shift.entry(r.row).or_insert(0);
            r.start = (r.start as isize + *acc).max(0) as usize;
            r.len = new_len;
            *acc += per;
        }
        // The mirrors changed the buffer: bump the seq so highlights /
        // LSP resync, and adopt it as the new baseline. Deliberately no
        // undo push — the user's keystroke snapshot holds the whole
        // pre-edit buffer, so one Undo reverts the keystroke AND the
        // mirrors together (VS Code's model).
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.resnapshot_linked_rows();
        self.linked_seq = self.edit_seq;
        true
    }

    // ---- Expand / Shrink Selection (#254) -------------------------------

    /// Every cursor's current selection, primary first — a zero-area
    /// selection at the caret when nothing is selected.
    fn cursor_selections(&self) -> Vec<EditorSelection> {
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        std::iter::once(primary)
            .chain(self.carets.iter().copied())
            .collect()
    }

    /// Ensure the expand stacks describe the CURRENT cursors; rebuild
    /// from scratch on any mismatch (edit, click, caret reshuffle). See
    /// the `select_expand` field doc for why matching, not indexing, is
    /// the validity test.
    pub fn validate_expand_stacks(&mut self) {
        let cur = self.cursor_selections();
        let valid = self.select_expand.as_ref().is_some_and(|se| {
            se.edit_seq == self.edit_seq
                && se.stacks.len() == cur.len()
                && se.stacks.iter().zip(&cur).all(|(st, c)| {
                    st.steps
                        .get(st.pos)
                        .is_some_and(|s| s.normalised() == c.normalised())
                })
        });
        if !valid {
            self.select_expand = Some(SelectExpandStacks {
                edit_seq: self.edit_seq,
                stacks: cur
                    .into_iter()
                    .map(|c| ExpandStack {
                        steps: vec![c],
                        pos: 0,
                    })
                    .collect(),
            });
        }
    }

    /// Write every stack's current step back onto the cursors, keeping
    /// the cursor-follows-head convention of every other selection setter.
    fn apply_expand_steps(&mut self) {
        let Some(se) = self.select_expand.as_ref() else {
            return;
        };
        let sels: Vec<EditorSelection> = se.stacks.iter().map(|st| st.steps[st.pos]).collect();
        let primary = sels[0];
        self.selection = primary.has_area().then_some(primary);
        self.cursor_row = primary.head.0.min(self.lines.len().saturating_sub(1));
        self.cursor_col = primary.head.1.min(self.line_char_len(self.cursor_row));
        self.carets = sels[1..].to_vec();
        self.ensure_cursor_col_visible();
    }

    /// Step every cursor one level up its cached chain. False when no
    /// stack has a deeper step cached (the caller then computes one —
    /// LSP chain or syntax ancestry).
    pub fn expand_selection_from_stack(&mut self) -> bool {
        let Some(se) = self.select_expand.as_mut() else {
            return false;
        };
        let mut any = false;
        for st in &mut se.stacks {
            if st.pos + 1 < st.steps.len() {
                st.pos += 1;
                any = true;
            }
        }
        if any {
            self.apply_expand_steps();
        }
        any
    }

    /// Shrink Selection: retrace one step down the stack. False at the
    /// bottom (the gesture's starting point).
    pub fn shrink_selection_step(&mut self) -> bool {
        self.validate_expand_stacks();
        let Some(se) = self.select_expand.as_mut() else {
            return false;
        };
        let mut any = false;
        for st in &mut se.stacks {
            if st.pos > 0 {
                st.pos -= 1;
                any = true;
            }
        }
        if any {
            self.apply_expand_steps();
        }
        any
    }

    /// Install per-cursor LSP `selectionRange` chains (#254): UTF-16
    /// `(start_line, start_char, end_line, end_char)` spans, smallest
    /// first, index-aligned with the cursors. Each stack's history above
    /// its current step is replaced by the strictly-growing tail of its
    /// chain; the caller then calls [`Self::expand_selection_from_stack`].
    pub fn install_selection_chains(&mut self, chains: Vec<Vec<(u32, u32, u32, u32)>>) {
        self.validate_expand_stacks();
        // Convert before mutably borrowing the stacks.
        let converted: Vec<Vec<EditorSelection>> = chains
            .iter()
            .map(|chain| {
                chain
                    .iter()
                    .map(|&(sl, sc, el, ec)| {
                        let sr = (sl as usize).min(self.lines.len().saturating_sub(1));
                        let er = (el as usize).min(self.lines.len().saturating_sub(1));
                        EditorSelection {
                            anchor: (sr, utf16_to_char_col(&self.lines[sr], sc)),
                            head: (er, utf16_to_char_col(&self.lines[er], ec)),
                        }
                    })
                    .collect()
            })
            .collect();
        let Some(se) = self.select_expand.as_mut() else {
            return;
        };
        for (st, chain) in se.stacks.iter_mut().zip(converted) {
            let cur = st.steps[st.pos].normalised();
            st.steps.truncate(st.pos + 1);
            let mut last = cur;
            for sel in chain {
                let n = sel.normalised();
                // Keep only strictly-growing spans that still contain the
                // current selection, so a chain answered for a slightly
                // different anchor can't shrink or jump the gesture.
                if n.0 <= last.0 && n.1 >= last.1 && n != last {
                    st.steps.push(sel);
                    last = n;
                }
            }
        }
    }

    /// Grow every cursor's selection to the nearest strictly-larger
    /// tree-sitter node (#254's serverless fallback). Without a grammar,
    /// grows to the line span and then the whole buffer, so the command
    /// always answers. Returns false when nothing could grow.
    pub fn expand_selection_syntax(&mut self) -> bool {
        self.validate_expand_stacks();
        let tree = self.lang.map(crate::highlight::language_for).and_then(|l| {
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&l).ok()?;
            parser.parse(self.lines.join("\n"), None)
        });
        let Some(se) = self.select_expand.as_ref() else {
            return false;
        };
        let mut grown: Vec<Option<EditorSelection>> = Vec::with_capacity(se.stacks.len());
        for st in &se.stacks {
            let cur = st.steps[st.pos].normalised();
            let next = match &tree {
                Some(t) => self.syntax_enclosing_range(t, cur),
                None => self.textual_enclosing_range(cur),
            };
            grown.push(next);
        }
        let Some(se) = self.select_expand.as_mut() else {
            return false;
        };
        let mut any = false;
        for (st, next) in se.stacks.iter_mut().zip(grown) {
            if let Some(sel) = next {
                st.steps.truncate(st.pos + 1);
                st.steps.push(sel);
                st.pos += 1;
                any = true;
            }
        }
        if any {
            self.apply_expand_steps();
        }
        any
    }

    /// The smallest tree-sitter node range strictly containing `cur`
    /// (char coordinates), climbing `.parent()` past same-range wrappers.
    fn syntax_enclosing_range(
        &self,
        tree: &tree_sitter::Tree,
        cur: ((usize, usize), (usize, usize)),
    ) -> Option<EditorSelection> {
        let start_b = self.char_pos_to_byte(cur.0);
        let end_b = self.char_pos_to_byte(cur.1);
        let mut node = tree.root_node().descendant_for_byte_range(start_b, end_b)?;
        loop {
            let (ns, ne) = (node.start_byte(), node.end_byte());
            let contains = ns <= start_b && ne >= end_b;
            let strictly = contains && (ns < start_b || ne > end_b);
            if strictly {
                let anchor = self.ts_point_to_char(node.start_position());
                let head = self.ts_point_to_char(node.end_position());
                return Some(EditorSelection { anchor, head });
            }
            node = node.parent()?;
        }
    }

    /// Grammar-less growth: the current line span, then the whole buffer.
    fn textual_enclosing_range(
        &self,
        cur: ((usize, usize), (usize, usize)),
    ) -> Option<EditorSelection> {
        let (start, end) = cur;
        let line_sel = EditorSelection {
            anchor: (start.0, 0),
            head: (end.0, self.line_char_len(end.0)),
        };
        let n = line_sel.normalised();
        if n.0 < start || n.1 > end {
            return Some(line_sel);
        }
        let last = self.lines.len().saturating_sub(1);
        let all = EditorSelection {
            anchor: (0, 0),
            head: (last, self.line_char_len(last)),
        };
        let n = all.normalised();
        (n.0 < start || n.1 > end).then_some(all)
    }

    /// Byte offset of char position `(row, col)` in the `\n`-joined text.
    fn char_pos_to_byte(&self, pos: (usize, usize)) -> usize {
        let mut off = 0usize;
        for (i, l) in self.lines.iter().enumerate() {
            if i == pos.0 {
                return off + char_byte(l, pos.1.min(l.chars().count()));
            }
            off += l.len() + 1;
        }
        off.saturating_sub(1)
    }

    /// Tree-sitter `Point` (row + BYTE column) → editor `(row, char col)`.
    fn ts_point_to_char(&self, p: tree_sitter::Point) -> (usize, usize) {
        let row = p.row.min(self.lines.len().saturating_sub(1));
        let line = &self.lines[row];
        let col = line[..p.column.min(line.len())].chars().count();
        (row, col)
    }

    /// The cursors' positions as LSP UTF-16 `(line, character)` pairs,
    /// primary first — `textDocument/selectionRange`'s `positions` input.
    pub fn cursor_positions_utf16(&self) -> Vec<(u32, u32)> {
        self.cursor_selections()
            .iter()
            .map(|s| {
                let (row, col) = s.head;
                let row = row.min(self.lines.len().saturating_sub(1));
                let line = &self.lines[row];
                let byte = char_byte(line, col.min(line.chars().count()));
                let u16col = line[..byte].encode_utf16().count() as u32;
                (row as u32, u16col)
            })
            .collect()
    }

    pub fn select_all(&mut self) {
        if self.lines.is_empty() {
            self.selection = None;
            return;
        }
        let last_row = self.lines.len() - 1;
        let last_col = self.line_char_len(last_row);
        self.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (last_row, last_col),
        });
        self.cursor_row = last_row;
        self.cursor_col = last_col;
    }

    /// Extract the selection text (`\n`-joined across rows) using the
    /// editor's char-indexed coordinates.  Returns "" when there's no
    /// selection or the selection is zero-area.
    pub fn selection_text(&self) -> String {
        let Some(sel) = self.selection else {
            return String::new();
        };
        if !sel.has_area() {
            return String::new();
        }
        let ((sr, sc), (er, ec)) = sel.normalised();
        if sr == er {
            let line = &self.lines[sr];
            let from = char_byte(line, sc);
            let to = char_byte(line, ec);
            return line[from..to].to_string();
        }
        let mut out = String::new();
        // first row: from sc to end of line
        let first = &self.lines[sr];
        let from = char_byte(first, sc);
        out.push_str(&first[from..]);
        out.push('\n');
        // full middle rows
        for r in (sr + 1)..er {
            out.push_str(&self.lines[r]);
            out.push('\n');
        }
        // last row: from start to ec
        let last = &self.lines[er];
        let to = char_byte(last, ec);
        out.push_str(&last[..to]);
        out
    }

    /// The literal text to light up as "other occurrences" of the current
    /// selection (VS Code's selection highlight). Returns `None` when the
    /// selection shouldn't drive occurrence highlighting at all. Computed once
    /// per render and fed to `paint_selection_occurrences` for every visible
    /// line.
    fn selection_occurrence_needle(&self) -> Option<String> {
        let sel = self.selection?;
        if !sel.has_area() {
            return None;
        }
        let ((sr, sc), (er, ec)) = sel.normalised();
        if sr != er {
            // VS Code only highlights occurrences for single-line selections.
            return None;
        }
        let line = &self.lines[sr];
        let text = &line[char_byte(line, sc)..char_byte(line, ec)];
        // Mirror VS Code's non-empty-selection branch: skip whitespace-only
        // selections and anything past the 200-char ceiling
        // (`editor.selectionHighlightMaxLength`); otherwise highlight the
        // literal substring. No minimum length, no whole-word filter.
        if text.trim().is_empty() || text.chars().count() > SELECTION_HIGHLIGHT_MAX_LEN {
            return None;
        }
        Some(text.to_string())
    }

    /// Delete the current selection if it has area.  Returns true iff content
    /// was removed.  Cursor lands at the start of the deleted range and the
    /// selection is cleared.  Pushes an undo step.
    pub fn delete_selection(&mut self) -> bool {
        if !self.selection.map(|s| s.has_area()).unwrap_or(false) {
            self.selection = None;
            return false;
        }
        self.push_undo(EditKind::DeleteSelection);
        let removed = self.delete_selection_inner();
        if removed {
            self.recompute_highlights();
        }
        removed
    }

    /// Same as `delete_selection` but does NOT push an undo step or
    /// recompute highlights — used by other public mutators that have
    /// already snapshotted state and will recompute themselves.
    fn delete_selection_inner(&mut self) -> bool {
        let Some(sel) = self.selection else {
            return false;
        };
        if !sel.has_area() {
            self.selection = None;
            return false;
        }
        let ((sr, sc), (er, ec)) = sel.normalised();
        if sr == er {
            let line = &mut self.lines[sr];
            let from = char_byte(line, sc);
            let to = char_byte(line, ec);
            line.replace_range(from..to, "");
        } else {
            let last = self.lines.remove(er);
            for _ in (sr + 1)..er {
                self.lines.remove(sr + 1);
            }
            let first = &mut self.lines[sr];
            let from = char_byte(first, sc);
            first.truncate(from);
            let to = char_byte(&last, ec);
            first.push_str(&last[to..]);
        }
        self.cursor_row = sr;
        self.cursor_col = sc;
        self.selection = None;
        self.mark_buffer_changed();
        true
    }

    /// Leading-whitespace width of `line`, or `None` for a blank line (a blank
    /// line belongs to whatever block surrounds it, so it carries no indent of
    /// its own). Tabs and spaces each count as one column, which is all the
    /// fold heuristic needs: it only ever compares indents with `>`.
    fn indent_width(&self, line: usize) -> Option<usize> {
        let s = self.lines.get(line)?;
        (!s.trim().is_empty()).then(|| s.chars().take_while(|c| *c == ' ' || *c == '\t').count())
    }

    /// The foldable region headed by `line`, as `(line, last_line_inclusive)`,
    /// or `None` when nothing below `line` is more indented. Interior blank
    /// lines are absorbed; trailing blanks and the first line back at (or
    /// below) the header's indent are not. This is VS Code's default
    /// indentation-based folding: a pure function of the text, no LSP or
    /// tree-sitter round-trip.
    pub fn fold_range(&self, line: usize) -> Option<(usize, usize)> {
        // Server spans first (#254): while a capable server has answered
        // for this exact line count, its ranges REPLACE the heuristics
        // wholesale — VS Code's provider model. rust-analyzer and friends
        // emit region markers and comment spans themselves.
        if let Some(ranges) = self.lsp_folds_current() {
            return ranges
                .iter()
                .filter(|r| r.start_line == line)
                .map(|r| (line, r.end_line.min(self.lines.len().saturating_sub(1))))
                .max_by_key(|&(_, end)| end);
        }
        // Fallback #1: the marker/comment table (`#region` pairs, comment
        // runs) — folds indentation alone can't see.
        if let Some(&(s, e, _)) = self
            .fallback_kind_folds
            .iter()
            .find(|&&(s, _, _)| s == line)
        {
            return Some((s, e.min(self.lines.len().saturating_sub(1))));
        }
        // Fallback #2: the indentation scan.
        self.indent_fold_range(line)
    }

    /// The plain indentation-based fold heuristic, a pure function of the
    /// text — the last-resort scanner behind the server spans and the
    /// marker/comment table.
    fn indent_fold_range(&self, line: usize) -> Option<(usize, usize)> {
        let base = self.indent_width(line)?;
        let mut end = line;
        let mut i = line + 1;
        while i < self.lines.len() {
            match self.indent_width(i) {
                None => {} // blank: possibly interior, keep scanning
                Some(w) if w > base => end = i,
                Some(_) => break, // back to base indent or shallower
            }
            i += 1;
        }
        (end > line).then_some((line, end))
    }

    /// Whether `line` heads a collapsible region.
    pub fn is_foldable(&self, line: usize) -> bool {
        self.fold_range(line).is_some()
    }

    /// Server fold spans, but only while they still describe this buffer:
    /// an edit changes the line count and orphans them until the next
    /// reply (same whole-buffer posture as `fold_epoch_lines`).
    fn lsp_folds_current(&self) -> Option<&[crate::lsp::manager::FoldingRangeItem]> {
        match &self.lsp_folds {
            Some(ranges) if self.lsp_folds_lines == self.lines.len() => Some(ranges),
            _ => None,
        }
    }

    /// Install a server's fold-span set (#254). The caller (the app's LSP
    /// drain) already seq-gated the reply; the line count is recorded so
    /// later edits orphan the spans instead of folding the wrong lines.
    pub fn set_lsp_folds(&mut self, ranges: Vec<crate::lsp::manager::FoldingRangeItem>) {
        self.lsp_folds = Some(ranges);
        self.lsp_folds_lines = self.lines.len();
        // Spans under existing collapsed headers may have moved.
        if !self.folded.is_empty() {
            self.rebuild_hidden_ranges();
        }
    }

    /// Rebuild the fallback marker/comment fold table when the buffer
    /// changed. Cheap no-op per frame otherwise; called from the render
    /// and from every fold command so headless callers agree with paint.
    pub fn refresh_fold_tables(&mut self) {
        if self.fallback_folds_seq == Some(self.edit_seq) {
            return;
        }
        self.fallback_folds_seq = Some(self.edit_seq);
        self.fallback_kind_folds.clear();
        use crate::lsp::manager::FoldRangeKind;
        // Region markers: a stack pairs nested #region/#endregion.
        let mut stack: Vec<usize> = Vec::new();
        for (i, l) in self.lines.iter().enumerate() {
            match region_marker(l) {
                Some(true) => stack.push(i),
                Some(false) => {
                    if let Some(start) = stack.pop()
                        && i > start
                    {
                        self.fallback_kind_folds
                            .push((start, i, FoldRangeKind::Region));
                    }
                }
                None => {}
            }
        }
        // Comment runs: ≥2 consecutive full-line comments fold as one
        // block from the first line.
        let mut run_start: Option<usize> = None;
        for i in 0..=self.lines.len() {
            let is_comment = self
                .lines
                .get(i)
                .is_some_and(|l| comment_line(l) && region_marker(l).is_none());
            match (run_start, is_comment) {
                (None, true) => run_start = Some(i),
                (Some(s), false) => {
                    if i - s >= 2 {
                        self.fallback_kind_folds
                            .push((s, i - 1, FoldRangeKind::Comment));
                    }
                    run_start = None;
                }
                _ => {}
            }
        }
        self.fallback_kind_folds
            .sort_unstable_by_key(|&(s, e, _)| (s, e));
    }

    /// The fold kind at a header line, when one is known: the server's
    /// kind, else the fallback table's. Plain indentation folds have no
    /// kind — "Fold All Comments/Regions" skips them.
    pub fn fold_kind_at(&self, line: usize) -> Option<crate::lsp::manager::FoldRangeKind> {
        if let Some(ranges) = self.lsp_folds_current() {
            return ranges
                .iter()
                .filter(|r| r.start_line == line)
                .max_by_key(|r| r.end_line)
                .map(|r| r.kind);
        }
        self.fallback_kind_folds
            .iter()
            .find(|&&(s, _, _)| s == line)
            .map(|&(_, _, k)| k)
    }

    /// Collapse every fold of `kind` — "Fold All Comments" (Cmd+K Cmd+/) /
    /// "Fold All Regions" (Cmd+K Cmd+8).
    pub fn fold_all_of_kind(&mut self, kind: crate::lsp::manager::FoldRangeKind) {
        self.refresh_fold_tables();
        let headers: Vec<usize> = (0..self.lines.len())
            .filter(|&l| self.fold_kind_at(l) == Some(kind) && self.is_foldable(l))
            .collect();
        if headers.is_empty() {
            return;
        }
        self.folded.extend(headers);
        self.fold_epoch_lines = self.lines.len();
        self.rebuild_hidden_ranges();
        while self.is_line_hidden(self.cursor_row) {
            self.cursor_row = self.cursor_row.saturating_sub(1);
        }
    }

    /// Expand every collapsed fold of `kind` — "Unfold All Regions"
    /// (Cmd+K Cmd+9). The mirror of [`Self::fold_all_of_kind`].
    pub fn unfold_all_of_kind(&mut self, kind: crate::lsp::manager::FoldRangeKind) {
        self.refresh_fold_tables();
        let drop: Vec<usize> = self
            .folded
            .iter()
            .copied()
            .filter(|&l| self.fold_kind_at(l) == Some(kind))
            .collect();
        if drop.is_empty() {
            return;
        }
        for l in drop {
            self.folded.remove(&l);
        }
        self.rebuild_hidden_ranges();
    }

    /// The OUTERMOST fold header whose region covers `line` — the function's
    /// `def`/`fn` line rather than the nearest `if`/`for` block that
    /// [`Self::enclosing_fold_header`] stops at. Climbs header-by-header;
    /// each step strictly decreases the line, so it terminates.
    fn outermost_enclosing_header(&self, line: usize) -> Option<usize> {
        let mut head = self.enclosing_fold_header(line)?;
        loop {
            let above = (0..head)
                .rev()
                .find(|&h| matches!(self.fold_range(h), Some((_, end)) if head <= end));
            match above {
                Some(a) => head = a,
                None => return Some(head),
            }
        }
    }

    /// Rebuild the debugger inline-value trailers (#135) for a stop at
    /// 1-based `stop_line`. Each line from the enclosing function's header
    /// down to the stop that mentions a local as a whole identifier gets a
    /// "name = value" list, first-mention order, capped per line; long
    /// values elide in the middle. Lines past the execution point stay bare
    /// — their state doesn't exist yet, which is VS Code's rule too.
    pub fn set_inline_values_from_locals(&mut self, stop_line: usize, locals: &[(String, String)]) {
        const MAX_ENTRIES_PER_LINE: usize = 4;
        const MAX_VALUE_CHARS: usize = 40;
        const MAX_SPAN_LINES: usize = 200;
        self.inline_values.clear();
        if locals.is_empty() || stop_line == 0 || self.lines.is_empty() {
            return;
        }
        let stop = (stop_line - 1).min(self.lines.len() - 1);
        let from = self
            .outermost_enclosing_header(stop)
            .unwrap_or(stop)
            .max(stop.saturating_sub(MAX_SPAN_LINES));
        for li in from..=stop {
            let line = self.lines[li].clone();
            let mut parts: Vec<String> = Vec::new();
            for token in identifier_tokens(&line) {
                if parts.len() >= MAX_ENTRIES_PER_LINE {
                    break;
                }
                let Some((name, value)) = locals.iter().find(|(n, _)| n == token) else {
                    continue;
                };
                if parts
                    .iter()
                    .any(|p| p.starts_with(name.as_str()) && p[name.len()..].starts_with(" = "))
                {
                    continue;
                }
                let shown: String = if value.chars().count() > MAX_VALUE_CHARS {
                    let head: String = value.chars().take(MAX_VALUE_CHARS / 2).collect();
                    let tail: String = value
                        .chars()
                        .rev()
                        .take(MAX_VALUE_CHARS / 2 - 1)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    format!("{head}\u{2026}{tail}")
                } else {
                    value.clone()
                };
                parts.push(format!("{name} = {shown}"));
            }
            if !parts.is_empty() {
                self.inline_values.insert(li, parts.join(", "));
            }
        }
    }

    /// Column distance between indentation guides: the indent width for
    /// space-indented buffers, one column for tab indentation (a leading tab
    /// occupies a single cell, so each tab is one nesting level).
    fn guide_step(&self) -> usize {
        let s = self.indent_style();
        if s.use_spaces {
            s.width.max(1) as usize
        } else {
            1
        }
    }

    /// Indent width `line` contributes to guide painting. A non-blank line is
    /// its own leading-whitespace width; a blank line borrows the minimum of
    /// its nearest non-blank neighbours, so guides run through gaps inside a
    /// block but stop between top-level blocks (VS Code's rule). The
    /// neighbour scans are capped: past a 200-blank-line gap guides simply
    /// stop, which keeps the per-row render cost bounded.
    fn guide_indent_width(&self, line: usize) -> usize {
        const BLANK_SCAN_CAP: usize = 200;
        if let Some(w) = self.indent_width(line) {
            return w;
        }
        let above = (0..line)
            .rev()
            .take(BLANK_SCAN_CAP)
            .find_map(|i| self.indent_width(i))
            .unwrap_or(0);
        let below = (line + 1..self.lines.len())
            .take(BLANK_SCAN_CAP)
            .find_map(|i| self.indent_width(i))
            .unwrap_or(0);
        above.min(below)
    }

    /// The active indentation guide (VS Code
    /// `editor.guides.highlightActiveIndentation`): the innermost guide of the
    /// block containing the cursor, as `(guide column, first line, last line)`
    /// inclusive. A line whose next non-blank neighbour is deeper is a block
    /// header and activates the guide of the body it opens. `None` at top
    /// level. The block walk is capped so a pathological single block can't
    /// make a frame scan the whole file.
    fn active_indent_guide(&self) -> Option<(usize, usize, usize)> {
        const BLOCK_WALK_CAP: usize = 5_000;
        let step = self.guide_step();
        let cr = self.cursor_row.min(self.lines.len().saturating_sub(1));
        let own = self
            .indent_width(cr)
            .unwrap_or_else(|| self.guide_indent_width(cr));
        let below = (cr + 1..self.lines.len())
            .take(BLOCK_WALK_CAP)
            .find_map(|i| self.indent_width(i))
            .unwrap_or(0);
        let target = own.max(below);
        if target == 0 {
            return None;
        }
        // The innermost guide of a block indented `target`: the last guide
        // column strictly below it.
        let col = (target - 1) / step * step;
        // Anchor on a row that actually draws the guide: the cursor row for a
        // body line, the first body row for a header.
        let anchor = if own > col { cr } else { cr + 1 };
        if anchor >= self.lines.len() || self.guide_indent_width(anchor) <= col {
            return None;
        }
        let mut lo = anchor;
        while lo > 0 && anchor - (lo - 1) <= BLOCK_WALK_CAP && self.guide_indent_width(lo - 1) > col
        {
            lo -= 1;
        }
        let mut hi = anchor;
        while hi + 1 < self.lines.len()
            && (hi + 1) - anchor <= BLOCK_WALK_CAP
            && self.guide_indent_width(hi + 1) > col
        {
            hi += 1;
        }
        Some((col, lo, hi))
    }

    /// The nearest foldable header at or above `line` whose region contains
    /// `line`, so "fold at cursor" collapses the enclosing block when the
    /// cursor sits in a body line rather than on the header itself.
    fn enclosing_fold_header(&self, line: usize) -> Option<usize> {
        if self.is_foldable(line) {
            return Some(line);
        }
        (0..line)
            .rev()
            .find(|&h| matches!(self.fold_range(h), Some((_, end)) if line <= end))
    }

    /// Whether `line` is hidden inside a collapsed region. A binary search over
    /// the merged spans in [`Self::hidden_ranges`], which the fold writes keep
    /// current — this is called for every rendered row of every frame.
    pub fn is_line_hidden(&self, line: usize) -> bool {
        self.hidden_ranges
            .binary_search_by(|&(header, end)| {
                if line <= header {
                    std::cmp::Ordering::Greater
                } else if line > end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }

    /// Recompute [`Self::hidden_ranges`] from `folded`. Every write to `folded`
    /// must call this. Headers are visited in ascending order and any header
    /// already inside the span being built is skipped: an inner fold's range is
    /// contained by its outer's, so it can add nothing.
    fn rebuild_hidden_ranges(&mut self) {
        #[cfg(test)]
        FOLD_RANGE_REBUILDS.with(|c| c.set(c.get() + 1));
        let mut out: Vec<(usize, usize)> = Vec::new();
        for &h in &self.folded {
            if let Some(&(_, end)) = out.last()
                && h <= end
            {
                continue;
            }
            if let Some(span) = self.fold_range(h) {
                out.push(span);
            }
        }
        self.hidden_ranges = out;
    }

    /// Unfold whatever collapsed region the cursor is sitting inside. Movement
    /// is not the only way in — search, go-to-definition and goto-line all set
    /// `cursor_row` directly — and a caret on a hidden line cannot be painted
    /// at all (`cursor_screen_pos` has no row for it), so an edit there would
    /// change content the user cannot see. Cheap when nothing is folded.
    pub fn reveal_cursor_fold(&mut self) {
        if self.folded.is_empty() {
            return;
        }
        let row = self.cursor_row;
        let covering: Vec<usize> = self
            .folded
            .iter()
            .copied()
            .filter(|&h| matches!(self.fold_range(h), Some((_, end)) if h < row && row <= end))
            .collect();
        if covering.is_empty() {
            // Nothing to reveal, so the spans are still current. `pin_on_edit`
            // calls this on every mutation: rebuilding here would rescan the
            // whole buffer once per keystroke for every folded header, which
            // after Fold All is the expensive case this cache exists to avoid.
            return;
        }
        for h in covering {
            self.folded.remove(&h);
        }
        self.rebuild_hidden_ranges();
    }

    /// Next visible line at or after `line`, walking out of any collapsed
    /// region. `None` past the end of the buffer.
    fn next_visible_line(&self, line: usize) -> Option<usize> {
        let mut l = line;
        while l < self.lines.len() {
            if !self.is_line_hidden(l) {
                return Some(l);
            }
            l += 1;
        }
        None
    }

    /// Previous visible line at or before `line`.
    fn prev_visible_line(&self, line: usize) -> usize {
        let mut l = line;
        while l > 0 && self.is_line_hidden(l) {
            l -= 1;
        }
        l
    }

    /// Toggle the fold owning `line` (VS Code `editor.toggleFold`). Collapsing a
    /// block that contains the cursor snaps the cursor up to the header so it
    /// never strands on a hidden line.
    pub fn toggle_fold(&mut self, line: usize) {
        self.refresh_fold_tables();
        let Some(header) = self.enclosing_fold_header(line) else {
            return;
        };
        let collapsing = !self.folded.remove(&header);
        if collapsing {
            self.folded.insert(header);
        }
        // Both branches changed the fold set, so both must refresh the spans.
        self.rebuild_hidden_ranges();
        if collapsing && self.is_line_hidden(self.cursor_row) {
            let len = self
                .lines
                .get(header)
                .map(|s| s.chars().count())
                .unwrap_or(0);
            self.cursor_row = header;
            self.cursor_col = self.cursor_col.min(len);
        }
        self.fold_epoch_lines = self.lines.len();
    }

    /// Collapse every foldable region (VS Code "Fold All", Cmd+K Cmd+0),
    /// snapping the cursor up to the nearest visible line.
    pub fn fold_all(&mut self) {
        self.refresh_fold_tables();
        self.folded = (0..self.lines.len())
            .filter(|&l| self.is_foldable(l))
            .collect();
        self.fold_epoch_lines = self.lines.len();
        self.rebuild_hidden_ranges();
        while self.cursor_row > 0 && self.is_line_hidden(self.cursor_row) {
            self.cursor_row -= 1;
        }
    }

    /// Expand every fold (VS Code "Unfold All", Cmd+K Cmd+J).
    pub fn unfold_all(&mut self) {
        self.folded.clear();
        self.hidden_ranges.clear();
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            selection: self.selection,
            carets: self.carets.clone(),
            dirty: self.dirty,
            save_seq: self.save_seq,
        }
    }

    /// Push an undo entry tagged with the kind of edit about to happen.
    /// Coalesces consecutive `InsertChar` ops into one step so a typing
    /// burst is undone as one unit; everything else opens a new step.
    fn push_undo(&mut self, kind: EditKind) {
        // Where the edit about to happen begins — the linked-editing
        // mirror reads this to locate the keystroke (#254).
        self.last_edit_origin = (self.cursor_row, self.cursor_col);
        // Where the edit about to happen begins — the merge editor's
        // region tracker reads this when it reconciles (#253).
        self.merge_edit_row = self.cursor_row;
        let coalesce =
            kind == EditKind::InsertChar && self.last_edit_kind == Some(EditKind::InsertChar);
        if !coalesce {
            self.undo_stack.push(self.snapshot());
            if self.undo_stack.len() > UNDO_STACK_LIMIT {
                self.undo_stack.remove(0);
            }
        }
        // A fresh edit branches history: whatever was undone can no longer be
        // redone (VS Code's model). Cleared even on a coalesced keystroke so a
        // typing burst after an undo discards the redo branch on its first char.
        self.redo_stack.clear();
        self.last_edit_kind = Some(kind);
    }

    /// Undo the most recent edit step. Returns true iff state was changed.
    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo_stack.pop() else {
            return false;
        };
        // Stash the pre-undo state so `redo` can reinstate it.
        self.redo_stack.push(self.snapshot());
        self.restore_snapshot(snap);
        true
    }

    /// Reapply the most recently undone edit step. Returns true iff state was
    /// changed. Mirrors `undo`, moving the current state back onto the undo
    /// stack so the redone edit can itself be undone.
    pub fn redo(&mut self) -> bool {
        let Some(snap) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(self.snapshot());
        self.restore_snapshot(snap);
        true
    }

    /// Replace the buffer state with a snapshot (shared by undo/redo), clamping
    /// the caret into the restored buffer and refreshing highlights.
    fn restore_snapshot(&mut self, snap: Snapshot) {
        self.lines = snap.lines;
        // Undo/redo swaps the whole buffer, and `Snapshot` does not carry the
        // map, so the attributions describe text that is no longer here.
        // Cleared rather than kept: #349 lists surviving an undo cycle as a
        // criterion, and carrying a stale map would falsify it silently
        // instead of leaving the lines honestly unknown. Restoring it
        // properly means putting `provenance` in `Snapshot`, which is the
        // later layer of that issue.
        self.provenance = crate::provenance::Provenance::new();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = snap.cursor_row.min(self.lines.len().saturating_sub(1));
        self.cursor_col = snap.cursor_col.min(self.line_char_len(self.cursor_row));
        self.selection = snap.selection;
        self.carets = snap.carets;
        // Crossing a save point (auto save or Cmd+S since the snapshot)
        // means the restored text differs from disk: stay dirty so the next
        // sweep reconverges them instead of leaving a silent divergence.
        self.dirty = snap.dirty || self.save_seq != snap.save_seq;
        // Undo/redo restored the WHOLE buffer, siblings included; the
        // linked-editing mirror must not replay on top of that (#254).
        self.linked_ranges.clear();
        self.linked_rows.clear();
        self.merge_edit_row = self.cursor_row;
        self.last_edit_kind = None;
        // A restore changes the buffer content, so bump the change counter (the
        // app resyncs the LSP / git gutter off `edit_seq`) and drop the wrap /
        // hscroll geometry caches keyed to the old text. `dirty` is taken from
        // the snapshot above, so this is not `mark_buffer_changed` (which would
        // force it true and lose an undo back to a saved state).
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.hscroll_content_cols = None;
        self.wrap_total_cache.clear();
        self.recompute_highlights();
    }

    /// Open a new undo step for the next edit (so a typing run doesn't
    /// merge with whatever comes after a movement / mouse / focus change).
    pub fn break_undo_coalescing(&mut self) {
        self.last_edit_kind = None;
    }

    /// Mouse-down: position the cursor at the click point and start a
    /// fresh zero-area selection there. A subsequent drag widens it.
    pub fn mouse_down(&mut self, col: u16, row: u16) {
        self.click(col, row);
        self.start_selection_at_cursor();
    }

    /// Mouse-drag: move the cursor to the drag point and extend the selection
    /// head to the new cursor.  Anchors at the current cursor if no prior
    /// selection exists.
    pub fn mouse_drag(&mut self, col: u16, row: u16) {
        if self.selection.is_none() {
            self.start_selection_at_cursor();
        }
        self.click(col, row);
        self.extend_selection_to_cursor();
    }

    /// Returns true if the on-disk file at `event_path` is the file currently
    /// open in this editor (used by the filesystem watcher to decide whether
    /// to reload).
    pub fn matches_open_path(&self, event_path: &Path) -> bool {
        let Some(open) = self.path.as_ref() else {
            return false;
        };
        if open == event_path {
            return true;
        }
        if let (Ok(a), Ok(b)) = (open.canonicalize(), event_path.canonicalize()) {
            return a == b;
        }
        false
    }

    /// Read the file's identity (mtime, len) for change detection. `None`
    /// when there is no path or the file can't be stat'd (e.g. it was
    /// deleted out from under us).
    fn disk_stamp_of(path: &Path) -> Option<(SystemTime, u64)> {
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    }

    /// Record that this buffer is now in sync with disk. Called at open and
    /// after a successful save so the next external write is detected.
    fn mark_synced_with_disk(&mut self) {
        self.disk_stamp = self.path.as_deref().and_then(Self::disk_stamp_of);
        self.disk_conflict = false;
        // A new buffer<->disk sync point; undo snapshots from before it
        // restore as dirty (see `restore_snapshot`).
        self.save_seq = self.save_seq.wrapping_add(1);
    }

    /// True when the file on disk no longer matches the (mtime, len) we last
    /// loaded or saved — i.e. some other process wrote it. A file with no
    /// recorded stamp (blank buffer, or never synced) is treated as
    /// unchanged so we never reload or block on a phantom diff.
    pub fn disk_changed_externally(&self) -> bool {
        let Some(path) = self.path.as_deref() else {
            return false;
        };
        match self.disk_stamp {
            Some(known) => Self::disk_stamp_of(path) != Some(known),
            None => false,
        }
    }

    /// Reload from disk *only if* there are no unsaved local edits. Returns
    /// `Some(Ok(()))` if a reload happened, `Some(Err(_))` if reload failed,
    /// `None` if reload was skipped because the buffer is dirty (caller
    /// should surface a "file changed on disk" warning instead). Retained
    /// for the explicit-revert path; the FS sync sweep uses
    /// `reload_or_flag_conflict`.
    pub fn reload_if_clean(&mut self) -> Option<Result<()>> {
        if self.dirty {
            return None;
        }
        Some(self.reload_from_disk())
    }

    /// Reload the buffer from disk, preserving the cursor/scroll position as
    /// far as the new contents allow. Updates the disk stamp so the reloaded
    /// state is the new sync point.
    fn reload_from_disk(&mut self) -> Result<()> {
        let Some(path) = self.path.as_ref().cloned() else {
            return Ok(());
        };
        let prev_row = self.cursor_row;
        let prev_col = self.cursor_col;
        let prev_scroll = self.scroll;
        // A PDF's page is its cursor: a rebuild on disk (pdflatex finishing)
        // must not snap the reader back to page 1. `open_pdf` consumes the
        // request and comes up on that page directly — restoring it with a
        // second render after the open left a window where that render's
        // transient failure lost the reader's place (#72).
        self.pdf_restore_page = self.pdf_page().filter(|&p| p > 1);
        let result = self.open(&path);
        // Cleared even when the open bailed before reaching `open_pdf`, so
        // the request can never leak into an unrelated later open.
        self.pdf_restore_page = None;
        // Clamp the restored cursor to the new contents so it stays valid
        // even if the file shrank.
        self.cursor_row = prev_row.min(self.lines.len().saturating_sub(1));
        self.cursor_col = prev_col.min(self.line_char_len(self.cursor_row));
        self.scroll = prev_scroll.min(self.lines.len().saturating_sub(1));
        result
    }

    /// Discard local edits and reload from disk unconditionally — the
    /// "Revert" half of a conflict resolution.
    pub fn revert_to_disk(&mut self) -> Result<()> {
        // A hex tab's pending overwrites are the thing being reverted:
        // discard them first so the same-path refresh guard in
        // `open_hex` lets the disk reload through (#173).
        if let Some(view) = self.hex.as_mut() {
            view.discard_edits();
        }
        // Same for a sheet tab's grid edits (#177): drop the dirty flag
        // so the same-path guard in `open` lets the rebuild through.
        if let Some(view) = self.sheet.as_mut() {
            view.dirty = false;
            view.editing = None;
        }
        // Reload FIRST: clearing `dirty` before a reload that then fails
        // (file deleted between the conflict popup and the Enter) would
        // launder unsaved edits as clean — the tab loses its marker and
        // the next FS sweep silently auto-reverts them away.
        self.reload_from_disk()?;
        self.dirty = false;
        Ok(())
    }

    /// FS-sync sweep entry point, applied to every open tab. Compares the
    /// file's current disk stamp against the buffer's last-synced stamp and:
    ///   * clean buffer + external change -> silently reload (VS Code's
    ///     non-dirty auto-revert behaviour),
    ///   * dirty buffer + external change -> flag a conflict so neither the
    ///     buffer nor the disk is clobbered,
    ///   * no change -> nothing.
    pub fn reload_or_flag_conflict(&mut self) -> ExternalChange {
        if !self.disk_changed_externally() {
            return ExternalChange::Unchanged;
        }
        if self.dirty {
            // Announce the conflict only on the transition into it. A buffer
            // that stays dirty while disk keeps differing must not re-fire on
            // every FS poll, or the confirm popup would reopen endlessly.
            let newly_conflicted = !self.disk_conflict;
            self.disk_conflict = true;
            return if newly_conflicted {
                ExternalChange::Conflict
            } else {
                ExternalChange::Unchanged
            };
        }
        match self.reload_from_disk() {
            Ok(()) => ExternalChange::Reloaded,
            Err(_) => ExternalChange::ReloadFailed,
        }
    }

    /// Save, refusing to overwrite if the file changed on disk since we last
    /// synced (an external edit would be silently lost otherwise). Returns
    /// `DiskConflict` instead of writing in that case; the caller surfaces
    /// the conflict and may call `save_to_disk_force` to overwrite anyway.
    pub fn save_to_disk(&mut self) -> Result<SaveOutcome> {
        if self.disk_changed_externally() {
            self.disk_conflict = true;
            return Ok(SaveOutcome::DiskConflict);
        }
        self.write_buffer_to_disk()
    }

    /// Save past a disk conflict, overwriting the external change. The
    /// "Overwrite" half of a conflict resolution.
    ///
    /// It does NOT bypass the encoding-loss guard: consenting to overwrite
    /// someone else's edit is not consent to mangle your own characters, so a
    /// buffer with both problems still reports `EncodingLoss` here and needs
    /// `lossy_save_armed` on top.
    pub fn save_to_disk_force(&mut self) -> Result<SaveOutcome> {
        self.write_buffer_to_disk()
    }

    /// Encode the buffer for disk in the encoding it claims, re-emitting the
    /// byte-order mark if the file had one.
    ///
    /// UTF-16 is hand-rolled because `encoding_rs` deliberately cannot produce
    /// it: the WHATWG Encoding Standard makes UTF-16 decode-only, so
    /// `Encoding::encode` routes UTF-16LE/BE (and REPLACEMENT) through
    /// `output_encoding()` to UTF-8 and hands back the string's UTF-8 bytes.
    /// Taking that output at face value wrote a file that disagreed with the
    /// encoding the status bar reported for it.
    ///
    /// Returns the bytes and whether the encoder had to substitute: for a
    /// legacy encoding `encoding_rs` does NOT write `?` the way iconv-lite
    /// (and so VS Code) does — per the WHATWG Encoding Standard it emits an
    /// HTML numeric character reference, which is valid content that no
    /// longer round-trips. Callers must not write that without consent.
    fn encode_for_disk(&self, content: &str) -> (Vec<u8>, bool) {
        let enc = self.encoding;
        if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE {
            let le = enc == encoding_rs::UTF_16LE;
            let mut out = Vec::with_capacity(content.len() * 2 + 2);
            // ALWAYS, regardless of whether the source had one: UTF-16 is
            // otherwise indistinguishable from binary, so a file written
            // without it cannot be reopened by croft or auto-detected by
            // anything else. VS Code's UTF-16 entries write it unconditionally
            // for the same reason.
            out.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
            for unit in content.encode_utf16() {
                out.extend_from_slice(&if le {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            // Every `char` encodes to UTF-16 losslessly (a `str` cannot hold
            // an unpaired surrogate), so this branch never substitutes.
            return (out, false);
        }
        let (encoded, _, had_errors) = enc.encode(content);
        if self.bom && enc == encoding_rs::UTF_8 {
            let mut out = Vec::with_capacity(encoded.len() + 3);
            out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            out.extend_from_slice(&encoded);
            return (out, had_errors);
        }
        (encoded.into_owned(), had_errors)
    }

    /// The distinct characters `encoding` cannot represent, in first-seen
    /// order and capped, for the message an `EncodingLoss` refusal shows.
    ///
    /// Walks the buffer and encodes each non-ASCII character on its own, so
    /// call it once to build that message rather than per frame. Empty for
    /// UTF-8 and UTF-16, which cover every `char`.
    pub fn unmappable_chars(&self) -> Vec<char> {
        /// Enough to name the problem without turning the status line into a
        /// character dump.
        const MAX: usize = 8;
        let enc = self.encoding;
        if enc == encoding_rs::UTF_8 || enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE
        {
            return Vec::new();
        }
        let mut out: Vec<char> = Vec::new();
        let mut buf = [0u8; 4];
        for line in &self.lines {
            // ASCII is mappable in every encoding croft offers, and is the
            // overwhelming majority of a source file: skip it without paying
            // for an encode call.
            for ch in line.chars().filter(|c| !c.is_ascii()) {
                if out.contains(&ch) {
                    continue;
                }
                let (_, _, had_errors) = enc.encode(ch.encode_utf8(&mut buf));
                if had_errors {
                    out.push(ch);
                    if out.len() == MAX {
                        return out;
                    }
                }
            }
        }
        out
    }

    fn write_buffer_to_disk(&mut self) -> Result<SaveOutcome> {
        // Preview tabs (image/PDF, sheet, diff, hex) hold the whole-
        // buffer-swap PLACEHOLDER in `lines`, not the file's content:
        // serialising it truncated the previewed file to nothing (#185).
        // Guarded here — the declared single choke point — so no save
        // path (explicit, force, auto, format-on-save) can route around.
        if self.has_non_text_view() {
            anyhow::bail!("This tab is a read-only preview; nothing to save");
        }
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No file open"))?
            .clone();
        let content = self.lines.join(self.eol.sequence());
        let (encoded, had_errors) = self.encode_for_disk(&content);
        // The single choke point every save funnels through, so no caller can
        // route around the guard. The consent flag survives a failed write
        // (the user still consented) and is cleared only once the bytes land.
        if had_errors && !self.lossy_save_armed {
            self.encoding_loss = true;
            return Ok(SaveOutcome::EncodingLoss);
        }
        std::fs::write(&path, encoded)?;
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.dirty = false;
        // The next keystroke opens a fresh undo step: coalescing across the
        // save point would make one Cmd+Z discard work from both sides.
        self.last_edit_kind = None;
        self.status = format!("Saved {}", path.display());
        // The buffer now matches disk, so this is the new sync point and any
        // prior conflict is resolved.
        self.mark_synced_with_disk();
        Ok(SaveOutcome::Saved)
    }

    pub fn move_up(&mut self) {
        if self.wrap_enabled() {
            self.wrap_move_vertical(-1);
        } else if self.cursor_row > 0 {
            // Step over a collapsed region, as `move_down` does.
            self.cursor_row = self.prev_visible_line(self.cursor_row - 1);
            self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        }
        self.last_edit_kind = None;
    }

    pub fn move_down(&mut self) {
        if self.wrap_enabled() {
            self.wrap_move_vertical(1);
        } else if self.cursor_row + 1 < self.lines.len() {
            // Step OVER a collapsed region rather than into it, the way VS Code
            // does: the fold stays shut and the caret lands on the next line
            // the user can actually see.
            if let Some(next) = self.next_visible_line(self.cursor_row + 1) {
                self.cursor_row = next;
                self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
            }
        }
        self.last_edit_kind = None;
    }

    pub fn goto_top(&mut self) {
        self.clear_selection();
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    pub fn goto_bottom(&mut self) {
        self.clear_selection();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    /// Copy the caret + scroll position from another editor onto this one,
    /// clamped to this buffer's bounds. Used when the editor is split so
    /// the duplicate opens exactly where the source was - including the
    /// wrap sub-line offset (`scroll_sub`), which is private to this
    /// module and can't be set from `App`.
    pub fn copy_view_position_from(&mut self, src: &Editor) {
        let max_row = self.lines.len().saturating_sub(1);
        self.cursor_row = src.cursor_row.min(max_row);
        let max_col = self
            .lines
            .get(self.cursor_row)
            .map_or(0, |l| l.chars().count());
        self.cursor_col = src.cursor_col.min(max_col);
        self.scroll = src.scroll.min(max_row);
        self.scroll_sub = src.scroll_sub;
        self.scroll_col = src.scroll_col;
    }

    pub fn goto_line(&mut self, one_based: usize) {
        self.clear_selection();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let max = self.lines.len() - 1;
        let target = one_based.saturating_sub(1).min(max);
        self.cursor_row = target;
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    pub fn yank_lines(&self, count: usize) -> String {
        let n = count.max(1);
        let start = self.cursor_row;
        self.lines_slice_text(start, n)
    }

    pub fn delete_lines(&mut self, count: usize) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.push_undo(EditKind::DeleteLines);
        self.clear_selection();
        let n = count.max(1);
        let start = self.cursor_row;
        let end = (start + n).min(self.lines.len());
        let yanked = self.lines_slice_text(start, end - start);
        self.lines.drain(start..end);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = start.min(self.lines.len() - 1);
        self.cursor_col = 0;
        self.mark_buffer_changed();
        self.recompute_highlights();
        yanked
    }

    /// Delete every line touched by the primary cursor or selection and by
    /// each secondary caret, yanking the removed text. VS Code's
    /// `editor.action.deleteLines` works across all cursors, and `Cmd+D`
    /// exists to make them: deleting only the primary's line would strand the
    /// others, still painted on rows that moved out from under them.
    pub fn delete_caret_lines(&mut self) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.push_undo(EditKind::DeleteLines);
        let last = self.lines.len() - 1;
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        // Mark while collecting, then scan once: the rows come out sorted and
        // deduped without a sort, and Change All Occurrences (Cmd+F2) can put
        // a caret on every match in the file.
        let mut doomed = vec![false; self.lines.len()];
        for sel in std::iter::once(primary).chain(self.carets.iter().copied()) {
            let (a, h) = sel.normalised();
            for slot in &mut doomed[a.0.min(last)..=h.0.min(last)] {
                *slot = true;
            }
        }
        let rows: Vec<usize> = doomed
            .iter()
            .enumerate()
            .filter_map(|(r, &drop)| drop.then_some(r))
            .collect();
        let yanked = rows
            .iter()
            .map(|&r| self.lines[r].as_str())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        // One retain pass, not a `remove` per row: removing tens of thousands
        // of rows one at a time is quadratic — a multi-second freeze on a large
        // file, on the render thread.
        let mut i = 0;
        self.lines.retain(|_| {
            let keep = !doomed[i];
            i += 1;
            keep
        });
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.selection = None;
        self.carets.clear();
        self.cursor_row = rows[0].min(self.lines.len() - 1);
        self.cursor_col = 0;
        self.mark_buffer_changed();
        self.recompute_highlights();
        yanked
    }

    pub fn kill_to_bol(&mut self) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.clear_selection();
        let row = self.cursor_row;
        if self.cursor_col == 0 {
            return String::new();
        }
        self.push_undo(EditKind::KillToBol);
        let from = self.byte_index(row, 0);
        let to = self.byte_index(row, self.cursor_col);
        let killed = self.lines[row][from..to].to_string();
        self.lines[row].replace_range(from..to, "");
        self.cursor_col = 0;
        self.mark_buffer_changed();
        self.recompute_highlights();
        killed
    }

    pub fn open_line_below(&mut self) {
        self.pin_on_edit();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.push_undo(EditKind::OpenLine);
        self.clear_selection();
        let row = self.cursor_row;
        let indent: String = self.lines[row]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        self.lines.insert(row + 1, indent.clone());
        self.cursor_row = row + 1;
        self.cursor_col = indent.chars().count();
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    pub fn open_line_above(&mut self) {
        self.pin_on_edit();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.push_undo(EditKind::OpenLine);
        self.clear_selection();
        let row = self.cursor_row;
        let indent: String = self.lines[row]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        self.lines.insert(row, indent.clone());
        self.cursor_col = indent.chars().count();
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    pub fn kill_to_eol(&mut self) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.clear_selection();
        let row = self.cursor_row;
        let line_len = self.line_char_len(row);
        if self.cursor_col >= line_len {
            return String::new();
        }
        self.push_undo(EditKind::KillToEol);
        let from = self.byte_index(row, self.cursor_col);
        let to = self.byte_index(row, line_len);
        let killed = self.lines[row][from..to].to_string();
        self.lines[row].replace_range(from..to, "");
        self.mark_buffer_changed();
        self.recompute_highlights();
        killed
    }

    /// Splice `text` over the `len_chars` characters starting at
    /// `(row, col_chars)` — the find bar's Replace. One undo step; the
    /// cursor lands at the end of the inserted text so replace-and-advance
    /// continues from there.
    pub fn replace_find_match(
        &mut self,
        row: usize,
        col_chars: usize,
        len_chars: usize,
        text: &str,
    ) {
        if row >= self.lines.len() {
            return;
        }
        let line_len = self.line_char_len(row);
        if col_chars.saturating_add(len_chars) > line_len {
            return;
        }
        self.pin_on_edit();
        self.push_undo(EditKind::Replace);
        self.clear_selection();
        let from = self.byte_index(row, col_chars);
        let to = self.byte_index(row, col_chars + len_chars);
        let text = &normalize_newlines(text);
        let breaks = text.matches('\n').count();
        if breaks == 0 {
            self.lines[row].replace_range(from..to, text);
            self.cursor_row = row;
            self.cursor_col = col_chars + text.chars().count();
        } else {
            // A replacement newline (VS Code's regex `\n`) becomes a real
            // line break, never an embedded control char in one line.
            let line = std::mem::take(&mut self.lines[row]);
            let merged = format!("{}{}{}", &line[..from], text, &line[to..]);
            self.lines
                .splice(row..=row, merged.split('\n').map(str::to_string));
            self.cursor_row = row + breaks;
            self.cursor_col = text
                .rsplit('\n')
                .next()
                .map_or(0, |tail| tail.chars().count());
        }
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    /// Swap in a whole new buffer as one undo step — the find bar's
    /// Replace All. The cursor clamps to the new buffer's bounds.
    pub fn replace_all_lines(&mut self, new_lines: Vec<String>) {
        self.pin_on_edit();
        self.push_undo(EditKind::Replace);
        self.clear_selection();
        self.lines = new_lines;
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = self.cursor_row.min(self.lines.len() - 1);
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    fn lines_slice_text(&self, start: usize, count: usize) -> String {
        if count == 0 || start >= self.lines.len() {
            return String::new();
        }
        let end = (start + count).min(self.lines.len());
        let mut out = self.lines[start..end].join("\n");
        out.push('\n');
        out
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.line_char_len(self.cursor_row);
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    pub fn move_right(&mut self) {
        if self.cursor_col < self.line_char_len(self.cursor_row) {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    /// Word-step right (Option+Right on macOS, Ctrl+Right elsewhere). Skips
    /// any non-word run starting at the cursor — across line boundaries —
    /// then skips the following word run on the line where it landed and
    /// stops. Word chars are ASCII alphanumerics + underscore; everything
    /// else (whitespace, punctuation, EOL) is non-word. Mirrors readline
    /// `forward-word` and VS Code's "Cursor Word Right" command.
    pub fn move_word_right(&mut self) {
        loop {
            let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
            while self.cursor_col < chars.len() && !is_word_char(chars[self.cursor_col]) {
                self.cursor_col += 1;
            }
            if self.cursor_col < chars.len() {
                break;
            }
            if self.cursor_row + 1 < self.lines.len() {
                self.cursor_row += 1;
                self.cursor_col = 0;
            } else {
                self.ensure_cursor_col_visible();
                self.last_edit_kind = None;
                return;
            }
        }
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        while self.cursor_col < chars.len() && is_word_char(chars[self.cursor_col]) {
            self.cursor_col += 1;
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    /// VS Code's "Copy Line Down" (Shift+Option+Down on macOS). Duplicates
    /// the current line — or, when a selection is active, every line the
    /// selection touches as one block — directly below the original, then
    /// moves the cursor (and the selection, if any) onto the duplicate so
    /// the next keystroke acts on the copy. Recorded as a single undo
    /// step via the dedicated `EditKind::DuplicateLines`.
    pub fn duplicate_lines_down(&mut self) {
        self.push_undo(EditKind::DuplicateLines);
        let (start_row, end_row) = self.selected_or_cursor_row_range();
        let block: Vec<String> = self.lines[start_row..=end_row].to_vec();
        let block_len = block.len();
        let insert_at = end_row + 1;
        for (i, line) in block.into_iter().enumerate() {
            self.lines.insert(insert_at + i, line);
        }
        self.cursor_row += block_len;
        if let Some(sel) = self.selection.as_mut() {
            sel.anchor.0 += block_len;
            sel.head.0 += block_len;
        }
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code's "Copy Line Up" (Shift+Option+Up on macOS). Mirror of
    /// `duplicate_lines_down`: inserts the copy *above* the original
    /// block, leaving the cursor on the upper copy at the same row index
    /// it started at (the original is pushed down by `block_len`).
    pub fn duplicate_lines_up(&mut self) {
        self.push_undo(EditKind::DuplicateLines);
        let (start_row, end_row) = self.selected_or_cursor_row_range();
        let block: Vec<String> = self.lines[start_row..=end_row].to_vec();
        for (i, line) in block.into_iter().enumerate() {
            self.lines.insert(start_row + i, line);
        }
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    fn selected_or_cursor_row_range(&self) -> (usize, usize) {
        match &self.selection {
            Some(sel) => {
                let (start, end) = sel.normalised();
                (start.0, end.0)
            }
            None => (self.cursor_row, self.cursor_row),
        }
    }

    /// Char-based slice of the buffer between two `(row, char_col)` points
    /// (`start <= end`), joining spanned rows with `\n`.
    fn char_range_text(&self, start: (usize, usize), end: (usize, usize)) -> String {
        if start.0 == end.0 {
            return self.lines[start.0]
                .chars()
                .skip(start.1)
                .take(end.1.saturating_sub(start.1))
                .collect();
        }
        let mut out: String = self.lines[start.0].chars().skip(start.1).collect();
        for line in &self.lines[start.0 + 1..end.0] {
            out.push('\n');
            out.push_str(line);
        }
        out.push('\n');
        let last: String = self.lines[end.0].chars().take(end.1).collect();
        out.push_str(&last);
        out
    }

    /// Replace the char-range `start..end` with `new`, re-splitting on `\n`
    /// so a replacement that adds or removes newlines reshapes `lines`.
    fn replace_char_range(&mut self, start: (usize, usize), end: (usize, usize), new: &str) {
        let prefix: String = self.lines[start.0].chars().take(start.1).collect();
        let suffix: String = self.lines[end.0].chars().skip(end.1).collect();
        let combined = format!("{prefix}{new}{suffix}");
        let replacement: Vec<String> = combined.split('\n').map(str::to_string).collect();
        self.lines.splice(start.0..=end.0, replacement);
    }

    /// VS Code "Move Line Down" (Alt+Down): swap the current line / selected
    /// block with the line below it, carrying the cursor and selection along.
    /// No-op when the block already touches the last line.
    pub fn move_lines_down(&mut self) {
        let (start, end) = self.selected_or_cursor_row_range();
        if end + 1 >= self.lines.len() {
            return;
        }
        self.push_undo(EditKind::MoveLines);
        self.lines[start..=end + 1].rotate_right(1);
        self.cursor_row += 1;
        if let Some(sel) = self.selection.as_mut() {
            sel.anchor.0 += 1;
            sel.head.0 += 1;
        }
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Move Line Up" (Alt+Up): mirror of `move_lines_down`. No-op
    /// when the block already touches the first line.
    pub fn move_lines_up(&mut self) {
        let (start, end) = self.selected_or_cursor_row_range();
        if start == 0 {
            return;
        }
        self.push_undo(EditKind::MoveLines);
        self.lines[start - 1..=end].rotate_left(1);
        self.cursor_row -= 1;
        if let Some(sel) = self.selection.as_mut() {
            sel.anchor.0 -= 1;
            sel.head.0 -= 1;
        }
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Toggle Line Comment" (Cmd+/): comment or uncomment every line
    /// the cursor or selection touches using the language's line-comment
    /// marker. Comments are inserted at the block's common (minimum)
    /// indentation so the markers align; the whole block uncomments only when
    /// every non-blank line is already commented. Returns false (a no-op) for
    /// languages with no line comment.
    pub fn toggle_line_comment(&mut self) -> bool {
        let Some(token) = line_comment_token(self.lang) else {
            return false;
        };
        let (start, end) = self.selected_or_cursor_row_range();
        let non_blank: Vec<usize> = (start..=end)
            .filter(|&r| !self.lines[r].trim().is_empty())
            .collect();
        if non_blank.is_empty() {
            return false;
        }
        self.push_undo(EditKind::ToggleComment);
        let all_commented = non_blank
            .iter()
            .all(|&r| self.lines[r].trim_start().starts_with(token));
        if all_commented {
            for &r in &non_blank {
                let line = self.lines[r].clone();
                let off = line.len() - line.trim_start().len();
                let mut rest = line[off..].to_string();
                rest.drain(..token.len());
                if rest.starts_with(' ') {
                    rest.drain(..1);
                }
                self.lines[r] = format!("{}{}", &line[..off], rest);
            }
        } else {
            let min_indent = non_blank
                .iter()
                .map(|&r| {
                    let l = &self.lines[r];
                    l.len() - l.trim_start().len()
                })
                .min()
                .unwrap_or(0);
            for &r in &non_blank {
                self.lines[r].insert_str(min_indent, &format!("{token} "));
            }
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// VS Code "Toggle Block Comment" (Shift+Alt+A): wrap the selection (or,
    /// with no selection, the current line) in the language's block-comment
    /// delimiters, or strip them when the selection already is a block
    /// comment. Returns false for languages with no block comment (e.g. YAML).
    pub fn toggle_block_comment(&mut self) -> bool {
        let Some((open, close)) = block_comment_tokens(self.lang) else {
            return false;
        };
        let (start, end) = match &self.selection {
            Some(sel) => sel.normalised(),
            None => {
                let len = self.line_char_len(self.cursor_row);
                ((self.cursor_row, 0), (self.cursor_row, len))
            }
        };
        let text = self.char_range_text(start, end);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return false;
        }
        self.push_undo(EditKind::ToggleComment);
        let wrapped = trimmed.starts_with(open)
            && trimmed.ends_with(close)
            && trimmed.len() >= open.len() + close.len();
        let new = if wrapped {
            trimmed[open.len()..trimmed.len() - close.len()]
                .trim()
                .to_string()
        } else {
            format!("{open} {trimmed} {close}")
        };
        self.replace_char_range(start, end, &new);
        self.selection = None;
        self.cursor_row = start.0;
        self.cursor_col = self.line_char_len(start.0);
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// VS Code "Join Lines": collapse the selected lines (or the current line
    /// with the one below it) into one, trimming each joined line's leading
    /// whitespace and separating with a single space. No-op when there is no
    /// following line to join.
    pub fn join_lines(&mut self) {
        let (start, end) = self.selected_or_cursor_row_range();
        let last = if start == end { start + 1 } else { end };
        if last >= self.lines.len() {
            return;
        }
        self.push_undo(EditKind::JoinLines);
        let mut result = self.lines[start].trim_end().to_string();
        let cursor_col = result.chars().count();
        for line in &self.lines[start + 1..=last] {
            let piece = line.trim_start();
            if !result.is_empty() && !result.ends_with(char::is_whitespace) && !piece.is_empty() {
                result.push(' ');
            }
            result.push_str(piece);
        }
        self.lines.splice(start..=last, std::iter::once(result));
        self.cursor_row = start;
        self.cursor_col = cursor_col;
        self.selection = None;
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Transform to Upper/Lower/Title Case": rewrite the selection
    /// (or, with no selection, the word under the cursor) in the requested
    /// case. No-op when there is nothing to transform.
    pub fn transform_selection_case(&mut self, kind: CaseTransform) {
        let range = match &self.selection {
            Some(sel) => Some(sel.normalised()),
            None => self
                .word_at(self.cursor_row, self.cursor_col)
                .map(|(s, e)| ((self.cursor_row, s), (self.cursor_row, e))),
        };
        let Some((start, end)) = range else {
            return;
        };
        let text = self.char_range_text(start, end);
        if text.is_empty() {
            return;
        }
        self.push_undo(EditKind::TransformCase);
        let new = match kind {
            CaseTransform::Upper => text.to_uppercase(),
            CaseTransform::Lower => text.to_lowercase(),
            CaseTransform::Title => title_case(&text),
        };
        self.replace_char_range(start, end, &new);
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Sort Lines Ascending/Descending": sort the selected lines
    /// lexicographically, or the whole buffer when nothing is selected. No-op
    /// on a single line.
    pub fn sort_lines(&mut self, ascending: bool) {
        let (start, end) = match &self.selection {
            Some(sel) => {
                let (s, e) = sel.normalised();
                (s.0, e.0)
            }
            None => (0, self.lines.len().saturating_sub(1)),
        };
        if start >= end {
            return;
        }
        self.push_undo(EditKind::SortLines);
        let mut block: Vec<String> = self.lines[start..=end].to_vec();
        block.sort();
        if !ascending {
            block.reverse();
        }
        self.lines.splice(start..=end, block);
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    /// VS Code "Trim Trailing Whitespace": strip trailing spaces and tabs from
    /// every line, clamping the cursor onto the new line end. Returns false
    /// (a no-op) when the buffer is already clean.
    pub fn trim_trailing_whitespace(&mut self) -> bool {
        let any = self
            .lines
            .iter()
            .any(|l| l.trim_end_matches([' ', '\t']).len() != l.len());
        if !any {
            return false;
        }
        self.push_undo(EditKind::TrimWhitespace);
        for line in &mut self.lines {
            let trimmed_len = line.trim_end_matches([' ', '\t']).len();
            line.truncate(trimmed_len);
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        true
    }

    /// The character at `(row, char_col)`, or None when out of range. Used by
    /// the bracket-matching helpers, which work in char indices.
    fn char_at(&self, row: usize, col: usize) -> Option<char> {
        self.lines.get(row)?.chars().nth(col)
    }

    /// Forward-scan from the opening bracket at `open` to its matching close,
    /// counting nesting of the SAME pair type. Returns the `(row, col)` of the
    /// closing bracket, or None when unbalanced.
    fn match_bracket_forward(&self, open: BPos) -> Option<BPos> {
        let oc = self.char_at(open.0, open.1)?;
        let cc = matching_close(oc)?;
        let mut depth = 0i32;
        let (mut row, mut col) = open;
        loop {
            match self.char_at(row, col) {
                Some(ch) => {
                    if ch == oc {
                        depth += 1;
                    } else if ch == cc {
                        depth -= 1;
                        if depth == 0 {
                            return Some((row, col));
                        }
                    }
                    col += 1;
                }
                None => {
                    row += 1;
                    col = 0;
                    if row >= self.lines.len() {
                        return None;
                    }
                }
            }
        }
    }

    /// Backward-scan from the closing bracket at `close` to its matching open.
    fn match_bracket_backward(&self, close: BPos) -> Option<BPos> {
        let cc = self.char_at(close.0, close.1)?;
        let oc = matching_open(cc)?;
        let mut depth = 0i32;
        let mut row = close.0;
        let mut col = close.1 as isize;
        loop {
            if col < 0 {
                if row == 0 {
                    return None;
                }
                row -= 1;
                col = self.line_char_len(row) as isize - 1;
                continue;
            }
            let ch = self.char_at(row, col as usize)?;
            if ch == cc {
                depth += 1;
            } else if ch == oc {
                depth -= 1;
                if depth == 0 {
                    return Some((row, col as usize));
                }
            }
            col -= 1;
        }
    }

    /// The innermost bracket pair enclosing the cursor, found by walking
    /// backwards for the first unmatched opening bracket and matching it
    /// forwards. Mirrors VS Code's `findEnclosingBrackets`.
    fn enclosing_brackets(&self) -> Option<(BPos, BPos)> {
        let mut row = self.cursor_row;
        let mut col = self.cursor_col as isize - 1;
        let mut expected: Vec<char> = Vec::new();
        loop {
            if col < 0 {
                if row == 0 {
                    return None;
                }
                row -= 1;
                col = self.line_char_len(row) as isize - 1;
                continue;
            }
            let ch = self.char_at(row, col as usize)?;
            if is_close_bracket(ch) {
                expected.push(matching_open(ch)?);
            } else if is_open_bracket(ch) {
                match expected.last() {
                    Some(&want) if want == ch => {
                        expected.pop();
                    }
                    _ => {
                        let open = (row, col as usize);
                        let close = self.match_bracket_forward(open)?;
                        return Some((open, close));
                    }
                }
            }
            col -= 1;
        }
    }

    /// The first bracket pair at or after the cursor (`findNextBracket`).
    fn next_bracket_pair(&self) -> Option<(BPos, BPos)> {
        let mut row = self.cursor_row;
        let mut col = self.cursor_col;
        loop {
            match self.char_at(row, col) {
                Some(ch) if is_open_bracket(ch) => {
                    let open = (row, col);
                    return self.match_bracket_forward(open).map(|c| (open, c));
                }
                Some(ch) if is_close_bracket(ch) => {
                    let close = (row, col);
                    return self.match_bracket_backward(close).map(|o| (o, close));
                }
                Some(_) => col += 1,
                None => {
                    row += 1;
                    col = 0;
                    if row >= self.lines.len() {
                        return None;
                    }
                }
            }
        }
    }

    /// Resolve the bracket pair relevant to the cursor, returning
    /// `(open, close, anchored_on_close)`. `anchored_on_close` is true when the
    /// cursor sits on the closing bracket, so a jump should target the open
    /// bracket instead. Adjacency priority mirrors VS Code's `matchBracket`:
    /// a close to the left wins, then an open to the right, then an open to the
    /// left, then a close to the right; failing all four it falls back to the
    /// enclosing pair, then the next bracket forward.
    fn find_bracket_pair(&self) -> Option<(BPos, BPos, bool)> {
        if let Some(pair) = self.adjacent_bracket_pair() {
            return Some(pair);
        }
        if let Some((open, close)) = self.enclosing_brackets() {
            return Some((open, close, false));
        }
        self.next_bracket_pair().map(|(o, c)| (o, c, false))
    }

    /// The bracket pair whose bracket sits immediately to the left or right of
    /// the caret, as `(open, close, anchored_on_close)`. `None` when the caret
    /// touches no bracket. Adjacency priority mirrors VS Code's `matchBracket`:
    /// a close to the left wins, then an open to the right, then an open to the
    /// left, then a close to the right.
    fn adjacent_bracket_pair(&self) -> Option<(BPos, BPos, bool)> {
        let row = self.cursor_row;
        let col = self.cursor_col;
        let left = col.checked_sub(1).and_then(|c| self.char_at(row, c));
        let right = self.char_at(row, col);
        if left.is_some_and(is_close_bracket) {
            let close = (row, col - 1);
            return self.match_bracket_backward(close).map(|o| (o, close, true));
        }
        if right.is_some_and(is_open_bracket) {
            let open = (row, col);
            return self.match_bracket_forward(open).map(|c| (open, c, false));
        }
        if left.is_some_and(is_open_bracket) {
            let open = (row, col - 1);
            return self.match_bracket_forward(open).map(|c| (open, c, false));
        }
        if right.is_some_and(is_close_bracket) {
            let close = (row, col);
            return self.match_bracket_backward(close).map(|o| (o, close, true));
        }
        None
    }

    /// The bracket pair to highlight (VS Code's `editorBracketMatch`): the
    /// bracket adjacent to the caret and its match, as `(open, close)`. Unlike
    /// [`Self::find_bracket_pair`] it never falls back to the enclosing or next
    /// pair, so the highlight appears only when the caret is beside a bracket.
    pub fn bracket_match_pair(&self) -> Option<(BPos, BPos)> {
        self.adjacent_bracket_pair().map(|(o, c, _)| (o, c))
    }

    /// VS Code "Go to Bracket" (`editor.action.jumpToBracket`, Cmd+Shift+\):
    /// move the cursor to the matching bracket. From an opening bracket it
    /// lands on the closing one and vice versa; from inside a pair it lands on
    /// the enclosing closing bracket. Returns false (a no-op) when no bracket
    /// is in play.
    pub fn jump_to_matching_bracket(&mut self) -> bool {
        let Some((open, close, anchored_on_close)) = self.find_bracket_pair() else {
            return false;
        };
        let target = if anchored_on_close { open } else { close };
        self.clear_selection();
        self.cursor_row = target.0;
        self.cursor_col = target.1;
        self.last_edit_kind = None;
        self.ensure_cursor_col_visible();
        true
    }

    /// VS Code "Select to Bracket" (`editor.action.selectToBracket`): select
    /// the region between the matching brackets, including the brackets
    /// themselves (from the opening bracket's start to the closing bracket's
    /// end). Returns false (a no-op) when no bracket pair is in play.
    pub fn select_to_matching_bracket(&mut self) -> bool {
        let Some((open, close, _)) = self.find_bracket_pair() else {
            return false;
        };
        let head = (close.0, close.1 + 1);
        self.selection = Some(EditorSelection { anchor: open, head });
        self.cursor_row = head.0;
        self.cursor_col = head.1;
        self.last_edit_kind = None;
        self.ensure_cursor_col_visible();
        true
    }

    /// VS Code "Transpose Characters around the Cursor"
    /// (`editor.action.transpose`): swap the character before the cursor with
    /// the one after it and advance the cursor (emacs `transpose-chars`). At
    /// the end of a non-final line the last character moves across the line
    /// break to the start of the next line. At the end of the final line it is
    /// a no-op.
    pub fn transpose_chars(&mut self) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        let len = self.line_char_len(row);
        if col >= len {
            if row + 1 >= self.lines.len() {
                return;
            }
            self.push_undo(EditKind::Transpose);
            if len > 0 {
                let mut chars: Vec<char> = self.lines[row].chars().collect();
                let moved = chars.pop().unwrap();
                self.lines[row] = chars.into_iter().collect();
                self.lines[row + 1].insert(0, moved);
                self.cursor_col = 1;
            } else {
                self.cursor_col = 0;
            }
            self.cursor_row = row + 1;
            self.mark_buffer_changed();
            self.recompute_highlights();
            self.ensure_cursor_col_visible();
            return;
        }
        if col == 0 {
            // VS Code's single-character range at column 1: no swap, just step
            // the cursor right.
            self.cursor_col = 1;
            self.ensure_cursor_col_visible();
            return;
        }
        self.push_undo(EditKind::Transpose);
        let mut chars: Vec<char> = self.lines[row].chars().collect();
        chars.swap(col - 1, col);
        self.lines[row] = chars.into_iter().collect();
        self.cursor_col = col + 1;
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Convert Indentation to Spaces" (`editor.action.indentationToSpaces`):
    /// replace every tab in each line's leading indentation with `tabSize`
    /// spaces. Only leading whitespace is touched.
    pub fn indentation_to_spaces(&mut self) {
        self.convert_indentation(true);
    }

    /// VS Code "Convert Indentation to Tabs" (`editor.action.indentationToTabs`):
    /// replace each run of `tabSize` spaces in the leading indentation with a
    /// tab, leaving any remainder. Only leading whitespace is touched.
    pub fn indentation_to_tabs(&mut self) {
        self.convert_indentation(false);
    }

    fn convert_indentation(&mut self, to_spaces: bool) {
        let tab_w = (self.indent_style().width as usize).max(1);
        let spaces = " ".repeat(tab_w);
        let mut new_lines = self.lines.clone();
        let mut changed = false;
        for line in new_lines.iter_mut() {
            let indent_len = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            let indent: String = line.chars().take(indent_len).collect();
            let converted = if to_spaces {
                indent.replace('\t', &spaces)
            } else {
                indent.replace(&spaces, "\t")
            };
            if converted != indent {
                let rest: String = line.chars().skip(indent_len).collect();
                *line = format!("{converted}{rest}");
                changed = true;
            }
        }
        if !changed {
            return;
        }
        self.push_undo(EditKind::IndentConvert);
        self.lines = new_lines;
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code's `files.trimFinalNewlines`, surfaced as a command: drop trailing
    /// empty lines at the end of the buffer, always keeping at least one line.
    /// Returns false (a no-op) when there are no trailing blank lines.
    pub fn trim_final_newlines(&mut self) -> bool {
        let mut end = self.lines.len();
        while end > 1 && self.lines[end - 1].is_empty() {
            end -= 1;
        }
        if end == self.lines.len() {
            return false;
        }
        self.push_undo(EditKind::TrimFinalNewlines);
        self.lines.truncate(end);
        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len() - 1;
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        true
    }

    /// VS Code "Add Cursor Below" (Cmd+Alt+Down): drop a secondary caret on
    /// the row below the lowest current caret, at the same column (clamped to
    /// that line's length). No-op at the last row.
    pub fn add_cursor_below(&mut self) {
        let max_row = self
            .carets
            .iter()
            .map(|c| c.head.0)
            .chain(std::iter::once(self.cursor_row))
            .max()
            .unwrap_or(self.cursor_row);
        let target = max_row + 1;
        if target >= self.lines.len() {
            return;
        }
        let col = self.cursor_col.min(self.line_char_len(target));
        self.carets.push(EditorSelection {
            anchor: (target, col),
            head: (target, col),
        });
    }

    /// VS Code Alt+click: add a caret at the screen cell `(col, row)`, keeping
    /// the existing cursor(s). The old primary (with any selection) becomes a
    /// caret and the clicked point becomes the new primary. Clicking an
    /// existing zero-width caret removes it (toggle). Returns false when the
    /// click misses the text area.
    pub fn add_caret_at_screen(&mut self, col: u16, row: u16) -> bool {
        let Some((r, c)) = self.buffer_pos_at(col, row) else {
            return false;
        };
        // Clicking the current primary is a no-op (nothing to add).
        if (r, c) == (self.cursor_row, self.cursor_col) {
            return true;
        }
        // Clicking an existing zero-width caret removes it (toggle off).
        if let Some(idx) = self
            .carets
            .iter()
            .position(|s| !s.has_area() && s.head == (r, c))
        {
            self.carets.remove(idx);
            return true;
        }
        // Demote the current primary (preserving any selection) to a caret and
        // promote the clicked point.
        let primary = self.selection.unwrap_or(EditorSelection {
            anchor: (self.cursor_row, self.cursor_col),
            head: (self.cursor_row, self.cursor_col),
        });
        self.carets.push(primary);
        self.selection = None;
        self.cursor_row = r;
        self.cursor_col = c;
        self.ensure_cursor_col_visible();
        true
    }

    /// Begin a column (box) selection anchored at the screen cell `(col, row)`
    /// (VS Code's Shift+Alt+drag). Clears any existing selection/carets and
    /// parks the primary at the anchor. Returns false if the cell misses text.
    pub fn begin_box_select(&mut self, col: u16, row: u16) -> bool {
        let Some((r, c)) = self.buffer_pos_at(col, row) else {
            return false;
        };
        self.box_anchor = Some((r, c));
        self.carets.clear();
        self.selection = None;
        self.cursor_row = r;
        self.cursor_col = c;
        true
    }

    /// Whether a column (box) selection drag is in progress.
    pub fn box_selecting(&self) -> bool {
        self.box_anchor.is_some()
    }

    /// Extend the live column selection to the screen cell `(col, row)`,
    /// rebuilding one caret per spanned row. No-op if no box drag is active.
    pub fn box_drag_to_screen(&mut self, col: u16, row: u16) {
        if self.box_anchor.is_none() {
            return;
        }
        if let Some((r, c)) = self.buffer_pos_at(col, row) {
            self.box_select_to(r, c);
        }
    }

    /// Rebuild the column selection so it spans the rectangle from the box
    /// anchor to `(head_row, head_col)`: every row in the range gets a caret
    /// over the shared column span (clamped to that line's length); the head
    /// row is the primary. Zero-width columns become bare carets.
    fn box_select_to(&mut self, head_row: usize, head_col: usize) {
        let Some((ar, ac)) = self.box_anchor else {
            return;
        };
        let (r0, r1) = (ar.min(head_row), ar.max(head_row));
        let (c0, c1) = (ac.min(head_col), ac.max(head_col));
        self.carets.clear();
        self.selection = None;
        for r in r0..=r1 {
            let len = self.line_char_len(r);
            let s = c0.min(len);
            let e = c1.min(len);
            let sel = EditorSelection {
                anchor: (r, s),
                head: (r, e),
            };
            if r == head_row {
                self.selection = (e > s).then_some(sel);
                self.cursor_row = r;
                self.cursor_col = e;
            } else {
                self.carets.push(sel);
            }
        }
        self.ensure_cursor_col_visible();
    }

    /// End the column-selection drag, keeping the carets it produced.
    pub fn end_box_select(&mut self) {
        self.box_anchor = None;
    }

    /// VS Code "Add Cursor Above" (Cmd+Alt+Up): mirror of `add_cursor_below`.
    /// No-op at the first row.
    pub fn add_cursor_above(&mut self) {
        let min_row = self
            .carets
            .iter()
            .map(|c| c.head.0)
            .chain(std::iter::once(self.cursor_row))
            .min()
            .unwrap_or(self.cursor_row);
        if min_row == 0 {
            return;
        }
        let target = min_row - 1;
        let col = self.cursor_col.min(self.line_char_len(target));
        self.carets.push(EditorSelection {
            anchor: (target, col),
            head: (target, col),
        });
    }

    /// VS Code "View: Toggle Word Wrap" (Alt+Z): flip this buffer's soft-wrap
    /// state, overriding the language default until the file is reopened.
    pub fn toggle_wrap(&mut self) {
        let now = self.wrap_enabled();
        self.wrap_override = Some(!now);
    }

    /// True when the active selection spans more than one row, so Tab should
    /// indent the whole block (VS Code) rather than replace the selection
    /// with a single indentation unit.
    pub fn selection_is_multiline(&self) -> bool {
        match &self.selection {
            Some(sel) => {
                let (start, end) = sel.normalised();
                start.0 != end.0
            }
            None => false,
        }
    }

    /// VS Code's plain-Tab indent (no multi-line selection): insert spaces up
    /// to the next tab stop from the cursor column, replacing any single-line
    /// selection first. With `editor.insertSpaces` + `editor.useTabStops`
    /// (both on by default) a Tab at column 2 with a width-4 unit adds 2
    /// spaces to reach column 4, not a flat 4.
    pub fn indent_at_cursor(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::Indent);
        if self.selection.is_some() {
            self.delete_selection_inner();
        }
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let style = self.indent_style();
        let row = self.cursor_row;
        let byte = self.byte_index(row, self.cursor_col);
        let (ins, advance): (String, usize) = if style.use_spaces {
            let unit_w = (style.width as usize).max(1);
            let pad = unit_w - (self.cursor_col % unit_w);
            (" ".repeat(pad), pad)
        } else {
            // A tab-indented buffer inserts one literal tab (useTabStops).
            ("\t".to_string(), 1)
        };
        self.lines[row].insert_str(byte, &ins);
        self.cursor_col += advance;
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Indent Lines" (Tab with a multi-line selection): prepend one
    /// indentation unit to every line the selection touches. Empty lines are
    /// left untouched, matching VS Code's "do not indent empty lines" rule
    /// for block indent. Cursor and selection columns shift right by the unit
    /// width on the lines that actually gained indentation so the same text
    /// stays highlighted.
    pub fn indent_lines(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::Indent);
        let unit = self.indent_unit();
        let unit_w = unit.chars().count();
        let (start_row, end_row) = self.selected_or_cursor_row_range();
        for row in start_row..=end_row {
            if !self.lines[row].is_empty() {
                self.lines[row].insert_str(0, &unit);
            }
        }
        // A row gained indentation iff it was non-empty (empty lines were
        // skipped and stay empty), so a row that is non-empty now is exactly
        // one that shifted right by `unit_w`.
        let shift = |row: usize, col: usize, lines: &[String]| -> usize {
            if row >= start_row && row <= end_row && !lines[row].is_empty() {
                col + unit_w
            } else {
                col
            }
        };
        self.cursor_col = shift(self.cursor_row, self.cursor_col, &self.lines);
        if let Some(mut sel) = self.selection {
            sel.anchor.1 = shift(sel.anchor.0, sel.anchor.1, &self.lines);
            sel.head.1 = shift(sel.head.0, sel.head.1, &self.lines);
            self.selection = Some(sel);
        }
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Outdent" (Shift+Tab): strip one indentation level from every
    /// line the selection touches, or just the current line when there is no
    /// selection. Removal is tab-stop aligned (the `useTabStops` default): a
    /// line indented 6 spaces drops to 4, a line indented 3 drops to 0. A
    /// single leading tab counts as one level. Cursor and selection columns
    /// follow the removal on their own row.
    pub fn dedent_lines(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::Indent);
        let unit_w = (self.indent_style().width as usize).max(1);
        let (start_row, end_row) = self.selected_or_cursor_row_range();
        let mut removed = vec![0usize; self.lines.len()];
        let mut any = false;
        for (row, slot) in removed
            .iter_mut()
            .enumerate()
            .take(end_row + 1)
            .skip(start_row)
        {
            let n = leading_outdent_chars(&self.lines[row], unit_w);
            if n > 0 {
                self.lines[row].replace_range(0..n, "");
                *slot = n;
                any = true;
            }
        }
        let pull = |row: usize, col: usize, removed: &[usize]| -> usize {
            if row < removed.len() {
                col.saturating_sub(removed[row])
            } else {
                col
            }
        };
        self.cursor_col = pull(self.cursor_row, self.cursor_col, &removed);
        if let Some(mut sel) = self.selection {
            sel.anchor.1 = pull(sel.anchor.0, sel.anchor.1, &removed);
            sel.head.1 = pull(sel.head.0, sel.head.1, &removed);
            self.selection = Some(sel);
        }
        if any {
            self.mark_buffer_changed();
            self.recompute_highlights();
        }
        self.ensure_cursor_col_visible();
    }

    /// Word-step left (Option+Left on macOS). Symmetric to `move_word_right`:
    /// skips non-word chars backwards across line boundaries, then walks
    /// back over the preceding word run, landing on its first char.
    pub fn move_word_left(&mut self) {
        loop {
            let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
            while self.cursor_col > 0 && !is_word_char(chars[self.cursor_col - 1]) {
                self.cursor_col -= 1;
            }
            if self.cursor_col > 0 {
                break;
            }
            if self.cursor_row > 0 {
                self.cursor_row -= 1;
                self.cursor_col = self.line_char_len(self.cursor_row);
            } else {
                self.ensure_cursor_col_visible();
                self.last_edit_kind = None;
                return;
            }
        }
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        while self.cursor_col > 0 && is_word_char(chars[self.cursor_col - 1]) {
            self.cursor_col -= 1;
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    pub fn scroll_left_by(&mut self, n: usize) {
        self.scroll_col = self.scroll_col.saturating_sub(n);
    }

    pub fn scroll_right_by(&mut self, n: usize) {
        self.scroll_col = self.scroll_col.saturating_add(n);
    }

    /// Recompute the visible text width from `last_inner` / `last_gutter_width`
    /// (set during the most recent render) and use it to decide whether the
    /// current cursor column is on-screen. This is the same arithmetic the
    /// renderer uses, kept in one place so move-and-scroll stays in lock-step.
    fn visible_text_width(&self) -> usize {
        let scrollbar_w = u16::from(self.last_scrollbar.width > 0);
        self.last_inner
            .width
            .saturating_sub(self.last_gutter_width + 2 + scrollbar_w) as usize
    }

    /// Pull `scroll_col` so the cursor sits inside the current viewport.
    /// Mirrors `ensure_cursor_visible` for vertical scroll - keyboard nav
    /// follows; wheel scrolling does not call this so the user's wheel
    /// position is sacrosanct. `pub(crate)` so callers like
    /// `App::open_search_hit` can sync horizontal scroll after a jump.
    pub(crate) fn ensure_cursor_col_visible(&mut self) {
        if self.wrap_enabled() {
            return;
        }
        let width = self.visible_text_width();
        if width == 0 {
            return;
        }
        if self.cursor_col < self.scroll_col {
            self.scroll_col = self.cursor_col;
        } else {
            // Hint cells between the scroll origin and the caret consume
            // viewport width too. Scrolling right drops leading hints out of
            // the row, so re-measure until the caret fits (the loop is
            // bounded: `scroll_col` only grows, capped at `cursor_col`).
            loop {
                let extra = self.inlay_cells_before_cursor(self.cursor_row, self.scroll_col);
                if self.cursor_col + extra < self.scroll_col + width {
                    break;
                }
                let target = (self.cursor_col + extra + 1).saturating_sub(width);
                if target <= self.scroll_col {
                    break;
                }
                self.scroll_col = target.min(self.cursor_col);
            }
        }
    }

    /// One screen worth of rows, derived from the editor's last rendered
    /// inner height.  Falls back to a sensible default before the first
    /// render (when `last_inner.height` is still 0).
    pub fn page_size(&self) -> usize {
        let from_rows = self.text_rows();
        if from_rows > 0 { from_rows } else { 20 }
    }

    /// Move the viewport down by exactly one screen so the first
    /// previously-unseen row becomes the new top of the viewport, and place
    /// the cursor on that new top row.  Clamps at end of file.
    pub fn page_down_one_screen(&mut self) {
        let page = self.page_size();
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top + page);
            return;
        }
        let max_row = self.lines.len().saturating_sub(1);
        let new_top = (self.scroll + page).min(max_row);
        self.scroll = new_top;
        self.cursor_row = new_top;
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    /// Move the viewport up by exactly one screen so the new top is `page`
    /// rows above the previous top.  Cursor lands on the new top row.
    /// Clamps at the start of file.
    pub fn page_up_one_screen(&mut self) {
        let page = self.page_size();
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top.saturating_sub(page));
            return;
        }
        let new_top = self.scroll.saturating_sub(page);
        self.scroll = new_top;
        self.cursor_row = new_top;
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    pub fn home_line(&mut self) {
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    pub fn end_line(&mut self) {
        self.cursor_col = self.line_char_len(self.cursor_row);
        self.last_edit_kind = None;
    }

    /// Scroll an open spreadsheet preview by `rows` (negative scrolls up).
    /// Returns true when this tab is a spreadsheet, so the generic scroll
    /// paths can hand it off instead of moving the (empty) text buffer.
    pub fn scroll_sheet_rows(&mut self, rows: isize) -> bool {
        let Some(sheet) = self.sheet.as_mut() else {
            return false;
        };
        let Some(data) = sheet.sheets.get_mut(sheet.current_sheet) else {
            return false;
        };
        let last = data.rows.len().saturating_sub(1);
        data.scroll_row = data.scroll_row.saturating_add_signed(rows).min(last);
        true
    }

    pub fn scroll_up(&mut self, n: usize) {
        // A rendered log scrolls by file line, ahead of the wrap/comment-box
        // paths that assume an editable buffer (#257).
        if self.log.is_some() {
            self.scroll_view_to(self.scroll.saturating_sub(n));
            return;
        }
        if self.scroll_markdown_preview(-(n as i32)) {
            return;
        }
        if self.scroll_sheet_rows(-(n as isize)) {
            return;
        }
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top.saturating_sub(n));
        } else if self.comment_boxes.is_empty() {
            self.scroll_view_to(self.scroll.saturating_sub(n));
        } else {
            self.nonwrap_set_top(self.nonwrap_top_content_row().saturating_sub(n));
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        if self.log.is_some() {
            self.scroll_view_to(self.scroll.saturating_add(n));
            return;
        }
        if self.scroll_markdown_preview(n as i32) {
            return;
        }
        if self.scroll_sheet_rows(n as isize) {
            return;
        }
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top.saturating_add(n));
        } else if self.comment_boxes.is_empty() {
            self.scroll_view_to(self.scroll.saturating_add(n));
        } else {
            self.nonwrap_set_top(self.nonwrap_top_content_row().saturating_add(n));
        }
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let viewport = self.text_rows();
        if self.wrap_enabled() {
            let width = self.visible_text_width();
            let total = self.total_visual_rows(width);
            let Some(metrics) = scrollbar::vertical_metrics(
                self.last_scrollbar,
                total,
                viewport,
                self.top_visual_row(width),
            ) else {
                return false;
            };
            self.wrap_set_top(scrollbar::scroll_for_y(metrics, y));
            return true;
        }
        // Same content length the render sized the bar with: lines plus
        // comment-box rows. Mapping through bare `lines.len()` made the bar
        // of a short file with a tall navigator comment dead (metrics said
        // "no overflow") and a long file's thumb run away from the pointer.
        let bw = self.visible_text_width();
        let content = self.lines.len() + self.box_rows_between(0, self.lines.len(), bw);
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            content,
            viewport,
            self.nonwrap_top_content_row(),
        ) else {
            return false;
        };
        self.nonwrap_set_top(scrollbar::scroll_for_y(metrics, y));
        true
    }

    /// Map a click/drag X coordinate on the horizontal scrollbar onto
    /// `scroll_col`. Returns false when there is no horizontal bar (no
    /// overflow), so the caller can fall through to other hit-tests.
    pub fn scroll_to_bar_x(&mut self, x: u16) -> bool {
        let width = self.visible_text_width();
        let Some(metrics) = scrollbar::horizontal_metrics(
            self.last_hscrollbar,
            self.content_cols(),
            width,
            self.scroll_col,
        ) else {
            return false;
        };
        self.scroll_col = scrollbar::scroll_for_x(metrics, x);
        true
    }

    /// Rows used for text in the last render (inner height minus the
    /// horizontal scrollbar row when present). Falls back to the full inner
    /// height before the first render populates `last_text_rows`.
    pub(crate) fn text_rows(&self) -> usize {
        let rows = self.last_text_rows as usize;
        if rows > 0 {
            rows
        } else {
            self.last_inner.height as usize
        }
    }

    /// Number of text rows currently visible (the minimap viewport box height).
    pub fn visible_rows(&self) -> usize {
        self.text_rows()
    }

    /// Inclusive `(start_row, end_row)` of the active selection, or `None` when
    /// there's no selection with area. Drives the minimap selection band.
    pub fn selection_rows(&self) -> Option<(usize, usize)> {
        let sel = self.selection?;
        if !sel.has_area() {
            return None;
        }
        let (start, end) = sel.normalised();
        Some((start.0, end.0))
    }

    /// Move the cursor to `line` and center the viewport on it (minimap click /
    /// drag navigation). The cursor moves too because the render's scroll-follow
    /// snaps the viewport back to keep the caret visible; centering the caret
    /// here means that follow logic leaves the chosen scroll position alone.
    pub fn goto_line_centered(&mut self, line: usize) {
        if self.lines.is_empty() {
            return;
        }
        let line = line.min(self.lines.len() - 1);
        self.cursor_row = line;
        self.cursor_col = self.cursor_col.min(self.line_char_len(line));
        let viewport = self.text_rows();
        self.scroll = line
            .saturating_sub(viewport / 2)
            .min(self.lines.len().saturating_sub(viewport));
        self.last_edit_kind = None;
    }

    /// Rasterize the whole document into a `w`x`h` RGBA buffer for the minimap:
    /// one column per character (clipped to `w`), each non-whitespace char
    /// painted in its syntax color. Whitespace stays `bg`, so indentation reads
    /// as the file's shape, like VS Code's minimap. The lines are mapped into
    /// `content_h` rows (top-aligned) rather than the full `h`: a short file
    /// uses a fixed small per-line height and leaves the rest of the strip
    /// blank, instead of stretching six lines down the whole column. For a file
    /// long enough to fill the strip, `content_h == h` and the mapping is the
    /// plain whole-file fit.
    /// ponytail: 1px per char, clipped at strip width; sub-pixel char scaling
    /// only if a wider strip ever needs to show more columns.
    pub fn minimap_rgba(
        &self,
        w: u32,
        h: u32,
        content_h: u32,
        bg: (u8, u8, u8),
        fg: (u8, u8, u8),
    ) -> Vec<u8> {
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for px in buf.chunks_exact_mut(4) {
            px[0] = bg.0;
            px[1] = bg.1;
            px[2] = bg.2;
            px[3] = 0xff;
        }
        let total = self.lines.len().max(1) as u64;
        for (i, line) in self.lines.iter().enumerate() {
            let y0 = (i as u64 * content_h as u64 / total) as u32;
            if y0 >= h {
                break;
            }
            let y1 = (((i as u64 + 1) * content_h as u64 / total) as u32).clamp(y0 + 1, h);
            let merged = merge_overlay(
                self.highlights.get(i).map(Vec::as_slice).unwrap_or(&[]),
                self.semantic_overlay
                    .get(i)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            );
            let mut si = 0usize;
            for (col, (byte, ch)) in (0_u32..).zip(line.char_indices()) {
                if col >= w {
                    break;
                }
                while si < merged.len() && merged[si].end <= byte {
                    si += 1;
                }
                if !ch.is_whitespace() {
                    let (r, g, b) = if si < merged.len() && merged[si].start <= byte {
                        span_rgb(merged[si].style, fg)
                    } else {
                        fg
                    };
                    for y in y0..y1 {
                        let idx = ((y * w + col) * 4) as usize;
                        buf[idx] = r;
                        buf[idx + 1] = g;
                        buf[idx + 2] = b;
                    }
                }
            }
        }
        buf
    }

    /// The non-wrap viewport top in CONTENT rows: buffer lines plus the
    /// comment-box rows above (and inside, via `scroll_sub`) the top line.
    fn nonwrap_top_content_row(&self) -> usize {
        let bw = self.visible_text_width();
        self.scroll + self.box_rows_between(0, self.scroll, bw) + self.scroll_sub
    }

    /// Set the non-wrap viewport top to content row `row`, landing mid-box
    /// when it falls inside a comment box (`scroll_sub` counts rows past
    /// the top line's own text row). Mirrors `wrap_set_top` for the
    /// box-extended non-wrap layout; a tall box is unreadable and its
    /// Reply/Ignore footer unreachable with a line-granular top. Pulls the
    /// cursor along like `scroll_view_to`.
    fn nonwrap_set_top(&mut self, row: usize) {
        let viewport = self.text_rows();
        if viewport == 0 || self.lines.is_empty() {
            self.scroll = 0;
            self.scroll_sub = 0;
            return;
        }
        if self.comment_boxes.is_empty() {
            self.scroll_sub = 0;
            self.scroll_view_to(row);
            return;
        }
        let bw = self.visible_text_width();
        let total = self.lines.len() + self.box_rows_between(0, self.lines.len(), bw);
        let row = row.min(total.saturating_sub(viewport));
        let mut acc = 0usize;
        let mut line = 0usize;
        while line < self.lines.len() {
            let group = 1 + self.box_rows_between(line, line + 1, bw);
            if acc + group > row {
                break;
            }
            acc += group;
            line += 1;
        }
        self.scroll = line.min(self.lines.len().saturating_sub(1));
        self.scroll_sub = row.saturating_sub(acc);
        let last_visible = (self.scroll + viewport - 1).min(self.lines.len().saturating_sub(1));
        let floor = self.caret_floor_row().min(last_visible);
        if self.cursor_row < floor {
            self.cursor_row = floor;
        } else if self.cursor_row > last_visible {
            self.cursor_row = last_visible;
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    fn scroll_view_to(&mut self, top: usize) {
        let viewport = self.last_inner.height as usize;
        // A rendered log's text side is a one-line stub, so clamping against
        // `lines` would pin scroll at 0 and the view could never move past its
        // first screen (#257). Clamp against the log's own line count instead;
        // its body area is one row shorter than the viewport (the header).
        if let Some(log) = self.log.as_ref() {
            let rows = viewport.saturating_sub(1).max(1);
            self.scroll = top.min(log.len().saturating_sub(rows));
            return;
        }
        if viewport == 0 || self.lines.is_empty() {
            self.scroll = 0;
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.last_edit_kind = None;
            return;
        }
        self.scroll = top.min(self.lines.len().saturating_sub(viewport));
        let last_visible = (self.scroll + viewport - 1).min(self.lines.len().saturating_sub(1));
        let floor = self.caret_floor_row().min(last_visible);
        if self.cursor_row < floor {
            self.cursor_row = floor;
        } else if self.cursor_row > last_visible {
            self.cursor_row = last_visible;
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    /// Topmost line the caret may be dragged to when the viewport scrolls out
    /// from under it: the first row the sticky band does not cover. Parking it
    /// on `scroll` itself puts it under the pinned headers, where it cannot be
    /// seen and where the band would have to yield to stay honest — which
    /// deleted the band for the whole of a wheel scroll.
    ///
    /// The band is measured from the previous frame's `sticky_lines` (the app
    /// layer rebuilds them from `scroll` on the next render). A burst of wheel
    /// events can therefore be one header out mid-gesture, which the render's
    /// own caret guard absorbs by trimming the band for that frame.
    fn caret_floor_row(&self) -> usize {
        if self.wrap_enabled() || self.sticky_lines.is_empty() {
            return self.scroll;
        }
        let rows_avail = (self.last_inner.height as usize).saturating_sub(1);
        self.scroll + self.sticky_lines.len().min(rows_avail)
    }

    /// Double-click word selection: place the cursor at the click point, then
    /// select the maximal run of word characters covering it and leave the
    /// caret at the right edge of that run (VS Code parity). A click on
    /// whitespace or past the end of an empty line clears any selection.
    pub fn select_word_at(&mut self, col: u16, row: u16) {
        self.click(col, row);
        if self.lines.is_empty() {
            self.selection = None;
            return;
        }
        let r = self.cursor_row;
        let chars: Vec<char> = self.lines[r].chars().collect();
        let c = self.cursor_col;
        let pivot = if c < chars.len() && is_word_char(chars[c]) {
            Some(c)
        } else if c == chars.len() && c > 0 && is_word_char(chars[c - 1]) {
            Some(c - 1)
        } else {
            None
        };
        let Some(p) = pivot else {
            self.selection = None;
            return;
        };
        let mut start = p;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = p + 1;
        while end < chars.len() && is_word_char(chars[end]) {
            end += 1;
        }
        self.cursor_col = end;
        self.selection = Some(EditorSelection {
            anchor: (r, start),
            head: (r, end),
        });
        self.last_edit_kind = None;
    }

    /// Move the cursor to the screen coordinates (col, row). Used for mouse clicks.
    pub fn click(&mut self, col: u16, row: u16) {
        if self.lines.is_empty() || self.last_inner.height == 0 {
            return;
        }
        if row < self.last_inner.y || row >= self.last_inner.y + self.last_inner.height {
            return;
        }
        let row_idx = (row - self.last_inner.y) as usize;
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        // Gutter clicks land on the row's first column (saturates to 0).
        let visible = col.saturating_sub(text_x) as usize;
        // Map the screen row through the last render's visual-row layout (the
        // same map `buffer_pos_at` reads) so wrapped and folded lines resolve
        // to the line actually shown on that row; the linear map drifts by one
        // for every wrapped continuation row above the pointer. A press below
        // the last visual row clamps to it. The map is empty only before the
        // first paint, when nothing can be wrapped or folded and the linear
        // map is exact.
        // A press on a comment-box row leaves the caret alone (the App
        // intercepts box clicks; this is the belt-and-braces path). A press
        // below the last visual row clamps to the nearest text row above.
        let clamped = row_idx.min(self.last_wrap_rows.len().saturating_sub(1));
        let nearest_text = (0..=clamped)
            .rev()
            .find_map(|i| self.text_row(i).map(|t| (i, t)));
        if !self.last_wrap_rows.is_empty()
            && row_idx < self.last_wrap_rows.len()
            && self.text_row(row_idx).is_none()
        {
            return;
        }
        // `start + visible` is a DISPLAY column, so inlay hints spliced into
        // the row have to be mapped back out of it — the same inversion
        // `buffer_pos_at` (and therefore hover) applies. Without this the
        // caret lands `hint` cells right of the pointer.
        let (target_line, target_col) = match nearest_text {
            Some((_, (line, start, end))) => {
                // In wrap mode a drag past a segment's right edge stops at the
                // segment end; non-wrap keeps the click-to-line-end behaviour.
                let cap = if self.wrap_enabled() { end } else { usize::MAX };
                (
                    line,
                    self.buffer_col_at_display(line, start + visible).min(cap),
                )
            }
            None => {
                let line = (self.scroll + row_idx).min(self.lines.len().saturating_sub(1));
                (
                    line,
                    self.buffer_col_at_display(line, visible + self.scroll_col),
                )
            }
        };
        self.cursor_row = target_line;
        self.cursor_col = target_col.min(self.line_char_len(target_line));
        self.last_edit_kind = None;
        // A click is a deliberate reposition; any multi-cursor session ends.
        self.carets.clear();
    }

    pub fn buffer_pos_at(&self, col: u16, row: u16) -> Option<(usize, usize)> {
        if self.last_inner.height == 0 || self.lines.is_empty() || row < self.last_inner.y {
            return None;
        }
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        if self.wrap_enabled() {
            // Map the screen row through the last render's visual-row layout so
            // a click lands on the right logical line/column even when wrapped.
            // The blank tail of a folded row maps to the segment end, letting
            // you click to end-of-visual-line.
            let (line, start, end) = self.text_row((row - self.last_inner.y) as usize)?;
            if col < text_x {
                return None;
            }
            let visible_col = (col - text_x) as usize;
            let char_col = (start + visible_col).min(end).min(self.line_char_len(line));
            return Some((line, char_col));
        }
        // Non-wrap: map through the painted layout (comment boxes shift the
        // rows below them); before the first paint the linear map is exact.
        if row >= self.last_inner.y + self.last_inner.height {
            return None;
        }
        let line = if self.last_wrap_rows.is_empty() {
            self.scroll + (row - self.last_inner.y) as usize
        } else {
            self.text_row((row - self.last_inner.y) as usize)?.0
        };
        if line >= self.lines.len() || col < text_x {
            return None;
        }
        let text_width = self
            .last_inner
            .width
            .saturating_sub(self.last_gutter_width + 2 + u16::from(self.last_scrollbar.width > 0));
        let visible_col = (col - text_x) as usize;
        if visible_col >= text_width as usize {
            return None;
        }
        Some((
            line,
            self.buffer_col_at_display(line, self.scroll_col + visible_col),
        ))
    }

    /// Invert the hint-splice display map: the first buffer column of `line`
    /// whose display cell reaches absolute display column `display_col`. A
    /// point landing inside a hint's own cells resolves to its anchor column,
    /// so the caret snaps beside the code the hint annotates. Without hints
    /// this is the identity (clamped to the line length).
    fn buffer_col_at_display(&self, line: usize, display_col: usize) -> usize {
        let line_len = self.line_char_len(line);
        let hints = self.row_inlay_spans(line);
        if hints.is_empty() {
            return display_col.min(line_len);
        }
        let mut c = self.scroll_col;
        while c < line_len {
            let extra: usize = hints
                .iter()
                .filter(|(hc, _, _)| *hc >= self.scroll_col && *hc <= c)
                .map(|(_, l, _)| l.chars().count())
                .sum();
            if c + extra >= display_col {
                break;
            }
            c += 1;
        }
        c.min(line_len)
    }

    pub fn word_at(&self, line: usize, col: usize) -> Option<(usize, usize)> {
        let chars: Vec<char> = self.lines.get(line)?.chars().collect();
        if col >= chars.len() || !is_word_char(chars[col]) {
            return None;
        }
        let mut start = col;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = col + 1;
        while end < chars.len() && is_word_char(chars[end]) {
            end += 1;
        }
        Some((start, end))
    }

    /// The identifier text under `(line, col)`, or None over non-word
    /// characters. Used by debug hover-to-evaluate to name the variable.
    pub fn word_string_at(&self, line: usize, col: usize) -> Option<String> {
        let (start, end) = self.word_at(line, col)?;
        let chars: Vec<char> = self.lines.get(line)?.chars().collect();
        Some(chars[start..end].iter().collect())
    }

    /// Severities and messages of every diagnostic whose range covers the
    /// character position `(line, ch)`. Decoded on demand from the retained
    /// `diagnostics` (UTF-16 positions) rather than the per-line span cache so
    /// the messages don't have to be duplicated into the render-time cache.
    /// Only runs on a hover dwell, never per frame. A zero-width diagnostic is
    /// widened to one cell to match the underline, so a point diagnostic is
    /// still hoverable. Returns an empty vec when no squiggle is under the
    /// point (and when the retained batch belongs to another file).
    pub fn diagnostics_at(
        &self,
        line: usize,
        ch: usize,
    ) -> Vec<(crate::lsp::manager::DiagnosticSeverity, String)> {
        if self.diagnostics_path.as_deref() != self.path.as_deref() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for d in &self.diagnostics {
            let start_line = d.start_line as usize;
            let end_line = d.end_line as usize;
            if line < start_line || line > end_line {
                continue;
            }
            let Some(text) = self.lines.get(line) else {
                continue;
            };
            let from = if line == start_line {
                utf16_to_char_col(text, d.start_char)
            } else {
                0
            };
            let to = if line == end_line {
                utf16_to_char_col(text, d.end_char)
            } else {
                text.chars().count()
            };
            let to = to.max(from + 1);
            if ch >= from && ch < to {
                out.push((d.severity, d.message.clone()));
            }
        }
        out
    }

    /// The region a hover popup should be keyed to at `(line, ch)`: the word
    /// range when one is under the point, otherwise the span of the first
    /// diagnostic covering it (so a squiggle on punctuation, e.g. a `..`
    /// syntax error, still anchors and dismisses a hover). `None` when neither
    /// is present. Shared by `poll_hover`, `drain_lsp_hover`, and the
    /// mouse-move dismissal so all three agree on what the popup belongs to.
    pub fn hover_region_at(&self, line: usize, ch: usize) -> Option<(usize, usize, usize)> {
        if let Some((start, end)) = self.word_at(line, ch) {
            return Some((line, start, end));
        }
        if self.diagnostics_path.as_deref() != self.path.as_deref() {
            return None;
        }
        for d in &self.diagnostics {
            let start_line = d.start_line as usize;
            let end_line = d.end_line as usize;
            if line < start_line || line > end_line {
                continue;
            }
            let text = self.lines.get(line)?;
            let from = if line == start_line {
                utf16_to_char_col(text, d.start_char)
            } else {
                0
            };
            let to = if line == end_line {
                utf16_to_char_col(text, d.end_char)
            } else {
                text.chars().count()
            };
            let to = to.max(from + 1);
            if ch >= from && ch < to {
                return Some((line, from, to));
            }
        }
        None
    }
}

/// Convert a char index within `s` to a byte index, saturating at `s.len()`.
fn char_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A `(row, char_col)` position of a bracket character, used by the bracket
/// matching helpers.
type BPos = (usize, usize);

/// The bracket pairs croft matches, mirroring VS Code's default language
/// brackets `()`, `[]`, `{}`.
const BRACKET_PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

fn is_open_bracket(c: char) -> bool {
    BRACKET_PAIRS.iter().any(|&(o, _)| o == c)
}

fn is_close_bracket(c: char) -> bool {
    BRACKET_PAIRS.iter().any(|&(_, cl)| cl == c)
}

fn matching_close(c: char) -> Option<char> {
    BRACKET_PAIRS
        .iter()
        .find(|&&(o, _)| o == c)
        .map(|&(_, cl)| cl)
}

fn matching_open(c: char) -> Option<char> {
    BRACKET_PAIRS
        .iter()
        .find(|&&(_, cl)| cl == c)
        .map(|&(o, _)| o)
}

/// Start char-indices of every whole-word, case-sensitive occurrence of
/// `word` within `chars`. "Whole-word" means the char immediately before and
/// after the match is not a word char, so `foo` does not match inside
/// `foobar`. Mirrors VS Code's "Change All Occurrences" matching.
fn find_word_occurrences(chars: &[char], word: &[char]) -> Vec<usize> {
    let mut out = Vec::new();
    if word.is_empty() || word.len() > chars.len() {
        return out;
    }
    let last_start = chars.len() - word.len();
    let mut i = 0;
    while i <= last_start {
        if chars[i..i + word.len()] == *word {
            let before_ok = i == 0 || !is_word_char(chars[i - 1]);
            let after_idx = i + word.len();
            let after_ok = after_idx >= chars.len() || !is_word_char(chars[after_idx]);
            if before_ok && after_ok {
                out.push(i);
                i += word.len();
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The line-comment marker for a language (VS Code `lineComment`), or `None`
/// for languages with no line comment, in which case Toggle Line Comment is a
/// no-op.
fn line_comment_token(lang: Option<LangKind>) -> Option<&'static str> {
    match lang {
        Some(LangKind::Rust)
        | Some(LangKind::JavaScript)
        | Some(LangKind::TypeScript)
        | Some(LangKind::Tsx)
        | Some(LangKind::Go)
        | Some(LangKind::C)
        | Some(LangKind::Cpp) => Some("//"),
        Some(LangKind::Python)
        | Some(LangKind::Yaml)
        | Some(LangKind::Toml)
        | Some(LangKind::Bash) => Some("#"),
        _ => None,
    }
}

/// The block-comment delimiters for a language (VS Code `blockComment`), or
/// `None` for languages without one (e.g. YAML), in which case Toggle Block
/// Comment is a no-op. Python has no real block comment, but VS Code maps the
/// command onto a triple-quoted string, so it returns `""" """`.
fn block_comment_tokens(lang: Option<LangKind>) -> Option<(&'static str, &'static str)> {
    match lang {
        Some(LangKind::Rust)
        | Some(LangKind::JavaScript)
        | Some(LangKind::TypeScript)
        | Some(LangKind::Tsx)
        | Some(LangKind::Go)
        | Some(LangKind::C)
        | Some(LangKind::Cpp)
        | Some(LangKind::Css) => Some(("/*", "*/")),
        Some(LangKind::Html) | Some(LangKind::Markdown) => Some(("<!--", "-->")),
        // Python has no true block comment; VS Code's language config maps
        // Toggle Block Comment onto a triple-quoted string (`""" """`).
        Some(LangKind::Python) => Some(("\"\"\"", "\"\"\"")),
        _ => None,
    }
}

/// Title-case a string: the first alphanumeric of each run is upper-cased,
/// the rest lower-cased, separators preserved. Used by Transform to Title
/// Case.
fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_alnum = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if prev_alnum {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_alnum = true;
        } else {
            out.push(c);
            prev_alnum = false;
        }
    }
    out
}

/// Whitespace rendering (#133, VS Code `editor.renderWhitespace`): which
/// spaces/tabs paint a visible glyph (`·` / `→`). `Selection` — VS Code's
/// default — shows them only inside selected text; `All` across the whole
/// buffer; `None` never. The palette command cycles Selection → All → None.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhitespaceMode {
    None,
    #[default]
    Selection,
    All,
}

impl WhitespaceMode {
    /// The persisted config id (`render_whitespace` in config.json).
    pub fn pref_id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Selection => "selection",
            Self::All => "all",
        }
    }

    /// Parse the persisted id; anything unrecognised (including the empty
    /// string an older config deserializes to) is the VS Code default.
    pub fn from_pref(id: &str) -> Self {
        match id {
            "none" => Self::None,
            "all" => Self::All,
            _ => Self::Selection,
        }
    }

    /// Settings/status label.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Selection => "selection",
            Self::All => "all",
        }
    }

    /// The palette toggle's cycle order (VS Code default first).
    pub fn next(self) -> Self {
        match self {
            Self::Selection => Self::All,
            Self::All => Self::None,
            Self::None => Self::Selection,
        }
    }
}

/// The whole-identifier tokens of `line`, in order: maximal
/// `[A-Za-z_][A-Za-z0-9_]*` runs, so an inline-value lookup for `x` can
/// never match the `x` inside `max` (#135).
fn identifier_tokens(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, ch) in line.char_indices() {
        let is_ident = ch == '_' || ch.is_ascii_alphanumeric();
        match (start, is_ident) {
            (None, true) if ch == '_' || ch.is_ascii_alphabetic() => start = Some(i),
            (Some(s), false) => {
                out.push(&line[s..i]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(&line[s..]);
    }
    out
}

/// Colour-index marker for an unmatched closing bracket (painted red).
const UNEXPECTED_BRACKET: u8 = u8::MAX;
/// Plain-text buffers above this size skip bracket colorization: a
/// no-grammar file has no tree-sitter pass absorbing a per-edit full scan,
/// so typing in a large log must not pay O(total chars) per keystroke.
/// Grammar-backed buffers are not gated here — re-highlighting already
/// rescans the document per edit, which bounds what ever reaches this scan.
const BRACKET_SCAN_MAX_BYTES: usize = 1_000_000;
/// Nesting-depth colours cycle through this many entries (VS Code's default
/// themes define three `editorBracketHighlight` foregrounds).
const BRACKET_COLOR_CYCLE: usize = 3;

/// Bracket-pair colorization scan: per line, the `(char column, colour
/// index)` of every `()[]{}` outside `protected` (absolute byte ranges of
/// strings and comments, ascending — the grammar's view from
/// `highlight_text_with_protected`). One shared depth counter across the
/// three bracket kinds (VS Code's default pool); an opener colours at its own
/// depth, a matching closer at the opener's depth, and a closer that matches
/// nothing — or not the innermost open bracket — marks `UNEXPECTED_BRACKET`
/// while leaving the stack alone, so `(]` reddens the `]` and a later `)`
/// still pairs with the `(`.
fn scan_bracket_colors(lines: &[String], protected: &[(usize, usize)]) -> Vec<Vec<(usize, u8)>> {
    let mut out = vec![Vec::new(); lines.len()];
    let mut stack: Vec<char> = Vec::new();
    let mut abs = 0usize;
    let mut pi = 0usize;
    for (li, line) in lines.iter().enumerate() {
        for (ci, ch) in line.chars().enumerate() {
            let b = abs;
            abs += ch.len_utf8();
            while pi < protected.len() && protected[pi].1 <= b {
                pi += 1;
            }
            if pi < protected.len() && protected[pi].0 <= b {
                continue;
            }
            match ch {
                '(' | '[' | '{' => {
                    out[li].push((ci, (stack.len() % BRACKET_COLOR_CYCLE) as u8));
                    stack.push(ch);
                }
                ')' | ']' | '}' => {
                    let want = match ch {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    if stack.last() == Some(&want) {
                        stack.pop();
                        out[li].push((ci, (stack.len() % BRACKET_COLOR_CYCLE) as u8));
                    } else {
                        out[li].push((ci, UNEXPECTED_BRACKET));
                    }
                }
                _ => {}
            }
        }
        abs += 1; // the '\n' separator between joined lines
    }
    out
}

fn indent_unit_for(lang: Option<LangKind>) -> &'static str {
    match lang {
        Some(LangKind::Yaml) => "  ",
        _ => "    ",
    }
}

/// Display name for a language mode, matching VS Code's status-bar labels.
/// `None` is a buffer with no recognised grammar.
pub fn language_label(lang: Option<LangKind>) -> &'static str {
    match lang {
        None => "Plain Text",
        Some(LangKind::Rust) => "Rust",
        Some(LangKind::Python) => "Python",
        Some(LangKind::JavaScript) => "JavaScript",
        Some(LangKind::TypeScript) => "TypeScript",
        Some(LangKind::Tsx) => "TypeScript JSX",
        Some(LangKind::Json) => "JSON",
        Some(LangKind::Xml) => "XML",
        Some(LangKind::Toml) => "TOML",
        Some(LangKind::Yaml) => "YAML",
        Some(LangKind::Markdown) => "Markdown",
        Some(LangKind::Go) => "Go",
        Some(LangKind::Html) => "HTML",
        Some(LangKind::Css) => "CSS",
        Some(LangKind::Bash) => "Shell Script",
        Some(LangKind::C) => "C",
        Some(LangKind::Cpp) => "C++",
    }
}

/// The VS Code language id for `lang`, used to scope user snippets (their
/// `scope` field names these). Lowercase, matching VS Code's identifiers so a
/// config copied from there just works.
pub fn language_scope_id(lang: Option<LangKind>) -> &'static str {
    match lang {
        None => "plaintext",
        Some(LangKind::Rust) => "rust",
        Some(LangKind::Python) => "python",
        Some(LangKind::JavaScript) => "javascript",
        Some(LangKind::TypeScript) => "typescript",
        Some(LangKind::Tsx) => "typescriptreact",
        Some(LangKind::Json) => "json",
        Some(LangKind::Xml) => "xml",
        Some(LangKind::Toml) => "toml",
        Some(LangKind::Yaml) => "yaml",
        Some(LangKind::Markdown) => "markdown",
        Some(LangKind::Go) => "go",
        Some(LangKind::Html) => "html",
        Some(LangKind::Css) => "css",
        Some(LangKind::Bash) => "shellscript",
        Some(LangKind::C) => "c",
        Some(LangKind::Cpp) => "cpp",
    }
}

/// Every selectable language mode, in the order the status-bar picker lists
/// them. `None` (Plain Text) is offered separately by the picker.
pub const SELECTABLE_LANGUAGES: &[LangKind] = &[
    LangKind::Rust,
    LangKind::Python,
    LangKind::JavaScript,
    LangKind::TypeScript,
    LangKind::Tsx,
    LangKind::Json,
    LangKind::Toml,
    LangKind::Yaml,
    LangKind::Markdown,
    LangKind::Go,
    LangKind::Html,
    LangKind::Css,
    LangKind::Bash,
    LangKind::C,
    LangKind::Cpp,
];

/// Number of leading whitespace bytes to strip for one outdent step, matching
/// VS Code's tab-stop-aligned `unshiftIndent`. A leading tab counts as one
/// level (strip it). For spaces, strip back to the previous tab stop: with a
/// width-4 unit, 4 -> remove 4, 6 -> remove 2, 3 -> remove 3, 1 -> remove 1.
/// All leading whitespace here is ASCII (`' '` / `'\t'`), so byte count equals
/// char count.
fn leading_outdent_chars(line: &str, unit_w: usize) -> usize {
    if unit_w == 0 {
        return 0;
    }
    if line.starts_with('\t') {
        return 1;
    }
    let spaces = line.chars().take_while(|c| *c == ' ').count();
    if spaces == 0 {
        0
    } else {
        ((spaces - 1) % unit_w) + 1
    }
}

fn extra_indent_triggered(lang: Option<LangKind>, last_non_ws: Option<char>) -> bool {
    let last = match last_non_ws {
        Some(c) => c,
        None => return false,
    };
    match lang {
        Some(LangKind::Python) => matches!(last, ':' | '(' | '[' | '{'),
        Some(LangKind::Rust)
        | Some(LangKind::JavaScript)
        | Some(LangKind::TypeScript)
        | Some(LangKind::Tsx)
        | Some(LangKind::Json)
        | Some(LangKind::Go)
        | Some(LangKind::Css) => matches!(last, '(' | '[' | '{'),
        _ => false,
    }
}

fn is_bracket_pair_split(lang: Option<LangKind>, prev: Option<char>, next: Option<char>) -> bool {
    let bracket_aware = matches!(
        lang,
        Some(LangKind::Rust)
            | Some(LangKind::Python)
            | Some(LangKind::JavaScript)
            | Some(LangKind::TypeScript)
            | Some(LangKind::Tsx)
            | Some(LangKind::Json)
            | Some(LangKind::Go)
            | Some(LangKind::Css)
    );
    if !bracket_aware {
        return false;
    }
    matches!(
        (prev, next),
        (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')'))
    )
}

/// First 4 KiB of a file (or all of it when shorter): the sample both the
/// BOM sniff and [`is_binary`] need, read without loading the whole file
/// so the over-limit routing in `open` stays O(1) in file size.
fn read_file_head(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut head = vec![0u8; 4096];
    let mut f = std::fs::File::open(path)?;
    let mut filled = 0;
    while filled < head.len() {
        let n = f.read(&mut head[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    head.truncate(filled);
    Ok(head)
}

fn is_binary(data: &[u8]) -> bool {
    let sample = &data[..data.len().min(4096)];
    if sample.contains(&0) {
        return true;
    }
    if sample.is_empty() {
        return false;
    }
    let nontext = sample
        .iter()
        .filter(|&&b| !(b >= 0x20 || matches!(b, b'\n' | b'\r' | b'\t' | 0x0c | 0x08)))
        .count();
    (nontext as f32 / sample.len() as f32) > 0.30
}

/// Build a Vec<Span> from a line and its byte-range highlight spans.
/// Byte index of the character at character index `chars_in`, or the line's
/// byte length when `chars_in` falls past the end. Used to slice a line at
/// a horizontal scroll offset measured in characters.
fn byte_index_of_char(line: &str, chars_in: usize) -> usize {
    line.char_indices()
        .nth(chars_in)
        .map(|(b, _)| b)
        .unwrap_or(line.len())
}

/// Character column for the LSP UTF-16 offset `u16_off` within `line`. LSP
/// positions count UTF-16 code units; the editor lays out by character, so a
/// line containing astral-plane characters (which take two UTF-16 units) needs
/// this conversion. An offset past the line clamps to the line's char length.
fn utf16_to_char_col(line: &str, u16_off: u32) -> usize {
    let mut units: u32 = 0;
    for (char_idx, ch) in line.chars().enumerate() {
        if units >= u16_off {
            return char_idx;
        }
        units += ch.len_utf16() as u32;
    }
    line.chars().count()
}

/// A character VS Code breaks a wrapped line *after* (a subset of its default
/// `editor.wordWrapBreakAfterCharacters` that matters for Latin/Markdown
/// prose): whitespace plus trailing punctuation, so the next word starts a
/// fresh visual row.
fn is_wrap_break_after(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            ')' | ']' | '}' | '-' | '/' | ',' | '.' | ';' | ':' | '?' | '!'
        )
}

/// Soft-wrap a logical line into visual segments for word-wrap mode.
///
/// Each segment is a half-open `(start_char, end_char)` range over `chars`;
/// the segments tile the line with no gaps or overlaps, so the inverse maps
/// (cursor -> screen, click -> cursor) stay exact. Breaks after the last
/// break-after character at or before `width` when one exists (word wrap), and
/// hard-breaks a single token longer than `width` at the column. A line that
/// already fits, an empty line, or `width == 0` yields one segment so the line
/// still occupies a row. Mirrors VS Code's monospace wrap: pick the rightmost
/// break opportunity within the column, else split the over-long token.
fn wrap_segments(chars: &[char], width: usize) -> Vec<(usize, usize)> {
    if width == 0 || chars.len() <= width {
        return vec![(0, chars.len())];
    }
    let mut segs = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        if chars.len() - start <= width {
            segs.push((start, chars.len()));
            break;
        }
        let limit = start + width;
        // Scan back from the hard limit for the rightmost break-after char;
        // `i` is the char count, so the char sits at `chars[i - 1]` and the
        // segment becomes `[start, i)`. Stop at `start + 1` so a break right
        // at the segment start can't produce a one-char row (or stall).
        let mut brk = limit;
        let mut i = limit;
        while i > start + 1 {
            if is_wrap_break_after(chars[i - 1]) {
                brk = i;
                break;
            }
            i -= 1;
        }
        segs.push((start, brk));
        start = brk;
    }
    segs
}

/// Shift highlight spans left by `byte_start`, dropping spans that fall
/// entirely before the cut and clamping spans straddling the cut.
/// Split file text into buffer lines the way the LSP spec / VS Code / Zed do:
/// `\r\n`, a lone `\r`, and `\n` are all line terminators. Rust's `str::lines`
/// only breaks on `\n` (stripping a trailing `\r`), so a stray lone `\r` would
/// leave croft's line count one short of the language server's. Every LSP
/// position past that `\r` (semantic tokens, diagnostics, hover, definition)
/// would then resolve one row off. Normalizing first keeps the two in lockstep
/// and is a no-op for clean `\n`-only files.
fn split_into_lines(text: &str) -> Vec<String> {
    normalize_newlines(text)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

/// Fold every line-ending convention to `\n` so callers can split on it
/// alone. Shared by the file loader and the find bar's replacement paths: a
/// replacement carrying `\r` (VS Code's escape, or pasted CRLF text) must
/// become a real line break, never a bare CR sitting inside a line — a CRLF
/// buffer would then save `\r\r\n` and every line count would disagree.
pub(crate) fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn shift_spans_for_view(spans: &[HiSpan], byte_start: usize) -> Vec<HiSpan> {
    let mut out = Vec::with_capacity(spans.len());
    for sp in spans {
        if sp.end <= byte_start {
            continue;
        }
        let new_start = sp.start.saturating_sub(byte_start);
        let new_end = sp.end - byte_start;
        out.push(HiSpan {
            start: new_start,
            end: new_end,
            style: sp.style,
        });
    }
    out
}

/// Overlay `over` onto `base` (both sorted, non-overlapping, byte offsets
/// within one line) and return a sorted, non-overlapping span list. Where
/// the two cover the same bytes, `over` wins; `base` shows through in the
/// gaps. This is the per-line realization of the VS Code / Zed "combined"
/// rule: semantic tokens repaint the bytes they resolve, tree-sitter
/// syntax fills the rest.
/// Resolve a span's foreground to RGB for the minimap; non-RGB colors
/// (named/indexed/reset) fall back to the editor's default text color.
fn span_rgb(style: ratatui::style::Style, default: (u8, u8, u8)) -> (u8, u8, u8) {
    match style.fg {
        Some(ratatui::style::Color::Rgb(r, g, b)) => (r, g, b),
        _ => default,
    }
}

fn merge_overlay(base: &[HiSpan], over: &[HiSpan]) -> Vec<HiSpan> {
    if over.is_empty() {
        return base.to_vec();
    }
    let mut out: Vec<HiSpan> = Vec::with_capacity(base.len() + over.len());
    for b in base {
        // Emit the parts of this base span not covered by any overlay span.
        let mut cur = b.start;
        for o in over.iter().filter(|o| o.end > b.start && o.start < b.end) {
            if o.start > cur {
                out.push(HiSpan {
                    start: cur,
                    end: o.start,
                    style: b.style,
                });
            }
            cur = cur.max(o.end);
            if cur >= b.end {
                break;
            }
        }
        if cur < b.end {
            out.push(HiSpan {
                start: cur,
                end: b.end,
                style: b.style,
            });
        }
    }
    out.extend_from_slice(over);
    out.sort_by_key(|s| s.start);
    out
}

fn build_line_spans<'a>(line: &'a str, spans: &[HiSpan]) -> Vec<Span<'a>> {
    if spans.is_empty() {
        return vec![Span::raw(line)];
    }
    let mut out: Vec<Span> = Vec::with_capacity(spans.len() * 2);
    let mut cursor = 0usize;
    for sp in spans {
        if sp.start > cursor && sp.start <= line.len() {
            let slice = &line[cursor..sp.start];
            if !slice.is_empty() {
                out.push(Span::raw(slice));
            }
        }
        let s = sp.start.min(line.len());
        let e = sp.end.min(line.len());
        if e > s {
            out.push(Span::styled(&line[s..e], sp.style));
            cursor = e;
        }
    }
    if cursor < line.len() {
        out.push(Span::raw(&line[cursor..]));
    }
    out
}

impl Widget for &mut Editor {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style);
        let mut inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the orange→green
        // gradient that matches the welcome activity box. The editor has no
        // title, so nothing needs re-stamping over the gradient top edge.
        if self.focused && self.focus_gradient {
            crate::gradient::paint_gradient_box(buf, area);
        }
        self.last_area = area;
        self.last_inner = inner;
        self.last_scrollbar = Rect::default();
        self.last_hscrollbar = Rect::default();
        self.merge_action_spans.clear();
        // Every rect this frame publishes is cleared up front, so a frame
        // that paints nothing leaves nothing behind for the mouse path to
        // hit-test against. `render_log` sets its own when it paints; the
        // early return below means it may never run at all, which is the
        // case a reset inside `render_log` cannot cover.
        if let Some(log) = self.log.as_mut() {
            log.last_body = Rect::default();
        }

        if inner.height == 0 {
            return;
        }
        let cbg = canvas_bg(
            crate::iterm2_inline::detect_iterm2_inline_support(),
            self.theme,
        );
        // Merge editor (#253): reconcile the tracked regions with any
        // manual edit since last frame, carve the source panes off the
        // top, then FALL THROUGH to the text path with the remaining
        // rect — the Result below is the ordinary text buffer.
        let (len, seq, row) = (self.lines.len(), self.edit_seq, self.merge_edit_row);
        let theme = self.theme;
        if let Some(mv) = self.merge.as_mut() {
            mv.sync_with_buffer(len, seq, row);
            inner = render_merge_panes(mv, inner, buf, theme);
            self.last_inner = inner;
        }
        // The Result height comes from the POST-carve rect: the merge
        // panes above just shrank `inner`, and every row / scrollbar /
        // cursor-visibility computation below must see what is left.
        let height = inner.height as usize;
        if height == 0 {
            return;
        }
        if let Some(image) = self.image.as_ref() {
            let ibg = image_canvas_bg(
                crate::iterm2_inline::detect_inline_image_protocol(),
                self.theme,
            );
            render_image_placeholder(image, self.path.as_deref(), inner, buf, ibg, self.theme);
            return;
        }
        if let Some(view) = self.sheet.as_mut() {
            render_sheet(view, self.path.as_deref(), inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.archive.as_mut() {
            render_archive(view, self.path.as_deref(), inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.log.as_mut() {
            let search = self
                .search_highlight
                .as_deref()
                .map(|t| (t, self.search_highlight_opts));
            let active = self.active_search_match;
            render_log(
                view,
                self.path.as_deref(),
                inner,
                buf,
                cbg,
                self.theme,
                self.scroll,
                search,
                active,
            );
            return;
        }
        if let Some(view) = self.hex.as_mut() {
            render_hex(view, self.path.as_deref(), inner, buf, cbg, self.theme);
            return;
        }
        if self.markdown_preview.is_some() {
            self.render_markdown_preview(inner, buf);
            return;
        }
        if let Some(diff) = self.diff.as_mut() {
            let (prev_arrow, next_arrow) = render_diff(diff, inner, buf, cbg, self.theme);
            self.diff_prev_arrow = prev_arrow;
            self.diff_next_arrow = next_arrow;
            return;
        }
        // Non-diff tabs: clear the hit rects so a stale arrow click on a
        // tab the user just switched away from can't fire.
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();

        // Gutter = a left glyph margin (the breakpoint / stop column, VS Code
        // style) + the right-aligned line number + its trailing space. Folding
        // the margin into `gutter_width` shifts the numbers and code right
        // together, so the number-to-code gap is unchanged and click/cursor
        // mapping (which derives from `gutter_width`) stays correct for free.
        const SIGN_MARGIN: u16 = 2;
        let gutter_width = (self.lines.len() + 1).to_string().len() as u16 + 1 + SIGN_MARGIN;
        self.last_gutter_width = gutter_width;
        // Rebuild the git-gutter marks if the buffer moved since last frame
        // (cheap no-op otherwise), so the bars below diff against HEAD.
        self.refresh_git_marks();
        // Same cadence for the fallback fold table (#254): region markers
        // and comment runs the gutter chevrons below consult.
        self.refresh_fold_tables();
        // Rescan merge-conflict blocks on the same cadence so the region
        // tints below always match the buffer.
        self.conflicts();
        // Fold headers are line indexes; an insert/delete anywhere shifts them.
        // If the line count changed since folds were set, drop them wholesale
        // rather than hide the wrong lines (see `fold_epoch_lines`).
        if !self.folded.is_empty() && self.lines.len() != self.fold_epoch_lines {
            self.folded.clear();
            self.hidden_ranges.clear();
        }
        // A caret on a hidden line has no painted row, so nothing is drawn and
        // the user cannot see where they are. Every direct `cursor_row` setter
        // can land there — go-to-definition, search, Ctrl+G, a restored session
        // — and enumerating them is how one gets missed, so normalise here,
        // where all of them converge before anything is painted. One binary
        // search against `hidden_ranges` when a fold exists, nothing when none
        // does.
        if !self.folded.is_empty() && self.is_line_hidden(self.cursor_row) {
            self.reveal_cursor_fold();
        }
        let wrap = self.wrap_enabled();
        if wrap {
            // Wrapped text folds onto extra rows instead of scrolling sideways.
            self.scroll_col = 0;
        }
        let text_x = inner.x + gutter_width + 1;
        let content_cols = self.content_cols();

        // Layout has a small cycle: the horizontal bar takes the bottom row
        // only when a line overflows the text column, but the text column's
        // width depends on whether the vertical bar takes a column. Break it
        // in one pass: provisionally reserve the vertical bar's column from
        // the FULL height to decide the horizontal bar, then recompute the
        // vertical thumb against the real (possibly shortened) viewport.
        // Reserving the horizontal row can only ever narrow the text column,
        // never widen it, so a line that overflows under the provisional
        // width still overflows under the final one - the decision is stable.
        // The horizontal bar never appears in wrap mode (lines fold instead).
        let provisional_text_width = inner
            .width
            .saturating_sub(gutter_width + 2 + u16::from(self.lines.len() > height))
            as usize;
        let hbar_present =
            !wrap && provisional_text_width > 0 && content_cols > provisional_text_width;
        let text_height = height - usize::from(hbar_present);
        self.last_text_rows = text_height as u16;

        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: inner.y,
            width: u16::from(inner.width > 0),
            height: text_height as u16,
        };

        // Decide the vertical scrollbar and final text width, then clamp the
        // scroll so the cursor stays visible. In wrap mode the content extent
        // and scroll position are measured in VISUAL rows (a logical line may
        // span several); otherwise in logical lines, one per row.
        let (text_width, scrollbar_metrics) = if wrap {
            let wide = inner.width.saturating_sub(gutter_width + 2) as usize;
            // Overflow at the wide width forces the vbar, which narrows the
            // column and only ever adds rows, so the decision is stable.
            let vbar = self.total_visual_rows(wide) > text_height;
            let tw = if vbar { wide.saturating_sub(1) } else { wide };
            let content_rows = self.total_visual_rows(tw);

            // Keep the cursor's visual row inside the viewport.
            if text_height > 0 {
                let cursor_vrow = self.cursor_visual_row(tw);
                let mut top = self.top_visual_row(tw);
                if cursor_vrow < top {
                    top = cursor_vrow;
                } else if cursor_vrow >= top + text_height {
                    top = cursor_vrow + 1 - text_height;
                }
                top = top.min(content_rows.saturating_sub(text_height));
                self.set_top_to_visual_row(top, tw);
            }
            let metrics = scrollbar::vertical_metrics(
                scrollbar_area,
                content_rows,
                text_height,
                self.top_visual_row(tw),
            );
            (tw as u16, metrics)
        } else {
            // `scroll_sub` counts rows INTO the top line's comment box (set
            // by the content-row scroller); clamp it to the group and drop
            // it entirely when a cursor clamp below moves the top line.
            if self.comment_boxes.is_empty() {
                self.scroll_sub = 0;
            } else {
                let bw = inner.width.saturating_sub(gutter_width + 3) as usize;
                let group = 1 + self.box_rows_between(self.scroll, self.scroll + 1, bw);
                self.scroll_sub = self.scroll_sub.min(group - 1);
            }
            let scroll_before = self.scroll;
            if text_height > 0 {
                if self.cursor_row < self.scroll {
                    self.scroll = self.cursor_row;
                } else if self.cursor_row >= self.scroll + text_height {
                    self.scroll = self.cursor_row + 1 - text_height;
                }
                // Comment boxes between the top and the cursor consume rows;
                // scroll further until the cursor's painted row fits. Boxes
                // are measured at the pre-scrollbar width, one column
                // narrower than the final text column, which can only
                // overestimate their height (the cursor stays visible).
                if !self.comment_boxes.is_empty() {
                    let bw = inner.width.saturating_sub(gutter_width + 3) as usize;
                    while self.cursor_row + self.box_rows_between(self.scroll, self.cursor_row, bw)
                        >= self.scroll + text_height
                    {
                        self.scroll += 1;
                    }
                }
            }
            if self.scroll != scroll_before {
                self.scroll_sub = 0;
            }
            let bw = inner.width.saturating_sub(gutter_width + 3) as usize;
            let box_rows = self.box_rows_between(0, self.lines.len(), bw);
            let metrics = scrollbar::vertical_metrics(
                scrollbar_area,
                self.lines.len() + box_rows,
                text_height,
                // Content-row top: lines above plus box rows above plus the
                // rows scrolled into the top line's own box.
                self.scroll + self.box_rows_between(0, self.scroll, bw) + self.scroll_sub,
            );
            let sw = u16::from(metrics.is_some());
            let tw = inner.width.saturating_sub(gutter_width + 2 + sw);
            (tw, metrics)
        };
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }

        // Clamp horizontal scroll (non-wrap only) so a wheel swipe can't
        // strand the buffer off-screen, then lay out the horizontal bar.
        let max_scroll_col = content_cols.saturating_sub(text_width as usize);
        self.scroll_col = self.scroll_col.min(max_scroll_col);
        let hbar_metrics = scrollbar::horizontal_metrics(
            Rect {
                x: text_x,
                y: inner.y + text_height as u16,
                width: text_width,
                height: u16::from(hbar_present),
            },
            content_cols,
            text_width as usize,
            self.scroll_col,
        );
        self.last_hscrollbar = hbar_metrics.map(|m| m.area).unwrap_or_default();

        // Build the visual-row layout the loop paints and the inverse maps
        // (cursor/click) read back. Each entry is (logical_line, char_start,
        // char_end). Non-wrap: one row per visible line, range [scroll_col,
        // scroll_col + text_width). Wrap: the wrapped segments from
        // (scroll, scroll_sub) down, capped at the viewport height.
        let mut visual_rows: Vec<VisRow> = Vec::with_capacity(text_height);
        // The boxes hanging under a line, emitted right after its last
        // segment. `group_pos` is the row's index within the line's whole
        // group (segments first, then box rows) so a wrap viewport top that
        // decomposed into a box region resumes mid-box.
        let push_box_rows = |rows: &mut Vec<VisRow>,
                             ed: &Editor,
                             line: usize,
                             skip: usize,
                             seg_count: usize,
                             tw: usize| {
            if ed.comment_boxes.is_empty() {
                return;
            }
            let mut group_pos = seg_count;
            for idx in 0..ed.comment_boxes.len() {
                if ed.comment_boxes[idx].line != line {
                    continue;
                }
                for box_row in 0..ed.comment_box_height(idx, tw) {
                    if group_pos >= skip && rows.len() < text_height {
                        rows.push(VisRow::Box {
                            box_idx: idx,
                            box_row,
                        });
                    }
                    group_pos += 1;
                }
            }
        };
        if wrap {
            let tw = text_width as usize;
            let mut line = self.scroll;
            let mut skip = self.scroll_sub;
            while line < self.lines.len() && visual_rows.len() < text_height {
                if self.is_line_hidden(line) {
                    line += 1;
                    continue;
                }
                let segs = self.line_segments(line, tw);
                for (i, &(s, e)) in segs.iter().enumerate() {
                    if i < skip {
                        continue;
                    }
                    if visual_rows.len() >= text_height {
                        break;
                    }
                    visual_rows.push(VisRow::Text {
                        line,
                        start: s,
                        end: e,
                    });
                }
                push_box_rows(&mut visual_rows, self, line, skip, segs.len(), tw);
                skip = 0;
                line += 1;
            }
        } else {
            // Collect up to `text_height` VISIBLE logical lines from `scroll`,
            // skipping any hidden inside a collapsed fold.
            let tw = text_width as usize;
            let mut line = self.scroll;
            // Group-position skip on the top line: 0 = its text row, 1..
            // = rows into its comment box, so the viewport can start
            // mid-box exactly like the wrap path.
            let mut skip = self.scroll_sub;
            while line < self.lines.len() && visual_rows.len() < text_height {
                if !self.is_line_hidden(line) {
                    if skip == 0 && visual_rows.len() < text_height {
                        visual_rows.push(VisRow::Text {
                            line,
                            start: self.scroll_col,
                            end: self.scroll_col + text_width as usize,
                        });
                    }
                    push_box_rows(&mut visual_rows, self, line, skip, 1, tw);
                    skip = 0;
                }
                line += 1;
            }
        }
        self.last_wrap_rows = visual_rows.clone();

        let sel_norm = self
            .selection
            .filter(|s| s.has_area())
            .map(|s| s.normalised());
        let occ_needle = self.selection_occurrence_needle();
        // Bracket-match highlight (VS Code `editorBracketMatch`): only in the
        // focused editor, and only when the caret is beside a bracket.
        let bracket_pair = if self.focused {
            self.bracket_match_pair()
        } else {
            None
        };
        // Indentation guides: the cursor block's active guide (focused editor
        // only) and the paint colours, resolved once per frame — the active
        // walk is O(block), not per-row work.
        let guide_step = self.guide_step();
        let active_guide = (self.show_indent_guides && self.focused)
            .then(|| self.active_indent_guide())
            .flatten();
        let guide_fg = self.theme.indent_guide();
        let guide_active_fg = self.theme.indent_guide_active();

        for (row_idx, vis_row) in visual_rows.iter().enumerate() {
            let y = inner.y + row_idx as u16;
            let (line_idx, row_start, row_end) = match *vis_row {
                VisRow::Box { box_idx, box_row } => {
                    self.paint_comment_box_row(
                        buf,
                        inner,
                        y,
                        gutter_width,
                        text_x,
                        text_width,
                        box_idx,
                        box_row,
                    );
                    continue;
                }
                VisRow::Text { line, start, end } => (line, start, end),
            };
            // Current-line highlight (VS Code `editor.lineHighlightBackground`,
            // #248): a whisper of the accent behind every visual row of the
            // cursor's line, gutter through code. Focused editor only, and
            // hidden while a selection is active so the two washes never stack
            // — VS Code's `renderLineHighlight` behaves the same. Painted
            // first: spans that carry their own background (selection, search,
            // occurrences) patch over it, spans that don't inherit it.
            if self.focused && line_idx == self.cursor_row && sel_norm.is_none() {
                buf.set_style(
                    Rect::new(inner.x, y, gutter_width + 1 + text_width, 1),
                    Style::default().bg(self.theme.current_line_bg()),
                );
            }
            // The line number shows once per logical line - on its first visual
            // row; wrapped continuation rows get a blank gutter, like VS Code.
            if !wrap || row_start == 0 {
                let line_no = format!(
                    "{:>width$} ",
                    line_idx + 1,
                    width = gutter_width as usize - 1
                );
                let gutter =
                    Line::from(Span::styled(line_no, Style::default().fg(Color::DarkGray)));
                buf.set_line(inner.x, y, &gutter, gutter_width);
            } else {
                buf.set_line(
                    inner.x,
                    y,
                    &Line::from(" ".repeat(gutter_width as usize)),
                    gutter_width,
                );
            }

            // Debugger gutter glyphs, on a logical line's first visual row only:
            // a yellow stop arrow (▶) takes priority over a red breakpoint dot
            // (●). Painted in the dedicated glyph margin at the far left
            // (`inner.x`), VS Code-style, so it never crowds the line number or
            // the code and the number-to-code gap is untouched.
            let sign_x = inner.x;
            let mut sign_taken = false;
            if (!wrap || row_start == 0)
                && let Some(path) = self.path.as_deref()
            {
                let here = line_idx + 1; // gutter is 1-based
                let is_stop = self
                    .stop_line
                    .as_ref()
                    .is_some_and(|(p, l)| p == path && *l == here);
                let is_bp = self
                    .breakpoints
                    .get(path)
                    .is_some_and(|s| s.contains(&here));
                let is_unverified = self
                    .unverified_breakpoints
                    .get(path)
                    .is_some_and(|s| s.contains(&here));
                let is_conditional = self
                    .breakpoint_conditions
                    .get(path)
                    .is_some_and(|c| c.contains_key(&here));
                let is_logpoint = self
                    .breakpoint_logs
                    .get(path)
                    .is_some_and(|m| m.contains_key(&here));
                if is_stop {
                    buf.set_string(
                        sign_x,
                        y,
                        "▶",
                        Style::default().fg(self.theme.ui(Color::Rgb(0xff, 0xcc, 0x00))),
                    );
                    sign_taken = true;
                } else if is_bp {
                    // Hollow dimmed ring when the adapter could not bind it; a
                    // red diamond for a conditional breakpoint; an amber one
                    // for a logpoint; a solid red dot for a plain, live one.
                    let (glyph, color) = if is_unverified {
                        ("○", self.theme.ui(Color::Rgb(0x99, 0x99, 0x99)))
                    } else if is_conditional {
                        ("◆", self.theme.ui(Color::Rgb(0xe5, 0x1c, 0x23)))
                    } else if is_logpoint {
                        ("◆", self.theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b)))
                    } else {
                        ("●", self.theme.ui(Color::Rgb(0xe5, 0x1c, 0x23)))
                    };
                    buf.set_string(sign_x, y, glyph, Style::default().fg(color));
                    sign_taken = true;
                }
            }

            // AI-stream stop button (croft pair): while a pilot streams into
            // this file, the row under its caret wears a stop square in the
            // badge's orange; clicking it (or Cmd+K X) cancels and reverts
            // the stream. Outranks the test play glyph; the debugger's
            // glyphs above still win the shared cell.
            if (!wrap || row_start == 0) && !sign_taken && self.stream_stop_line == Some(line_idx) {
                buf.set_string(
                    sign_x,
                    y,
                    "■",
                    Style::default().fg(self.theme.ui(Color::Rgb(0xff, 0x9d, 0x2f))),
                );
                sign_taken = true;
            }

            // Testing gutter glyph: the play button beside a test fn's
            // definition (VS Code's run bead, in its testing green), sharing
            // the sign cell with the debugger — whose stop arrow and
            // breakpoint dot both outrank it.
            if (!wrap || row_start == 0)
                && !sign_taken
                && crate::testing::locate::test_fn_on_line(
                    self.path.as_deref(),
                    &self.lines,
                    line_idx,
                )
                .is_some()
            {
                buf.set_string(
                    sign_x,
                    y,
                    "\u{eb2c}", // cod-play, the Testing panel's run glyph
                    Style::default().fg(self.theme.ui(Color::Rgb(0x73, 0xc9, 0x91))),
                );
            }

            // Fold chevron: on a foldable header's first visual row, in the
            // second sign-margin column (`sign_x + 1`) so it never collides with
            // a breakpoint dot at `sign_x`. ▾ = expanded, ▸ = collapsed; the
            // collapsed one is brighter so hidden content is noticeable. Matches
            // the gutter's own DarkGray line numbers (which are theme-agnostic).
            if (!wrap || row_start == 0) && self.is_foldable(line_idx) {
                let collapsed = self.folded.contains(&line_idx);
                let (glyph, color) = if collapsed {
                    ("▸", self.theme.ui(Color::Gray))
                } else {
                    ("▾", Color::DarkGray)
                };
                buf.set_string(sign_x + 1, y, glyph, Style::default().fg(color));
            }

            // Git gutter: a thin coloured bar in the spacer cell between the
            // line number and the code (VS Code's dirty-diff lane), on a
            // logical line's first visual row only. Colour carries add/mod/del.
            // ponytail: one heavy bar in three colours; the bar's column also
            // shows deletions, which VS Code renders as a small triangle — fine
            // as a first cut, the colour already disambiguates.
            if (!wrap || row_start == 0)
                && let Some(mark) = self.git_marks.get(&line_idx)
            {
                let color = match mark {
                    GitMark::Added => self.theme.git_added(),
                    GitMark::Modified => self.theme.git_modified(),
                    GitMark::Deleted => self.theme.git_deleted(),
                };
                buf.set_string(
                    inner.x + gutter_width,
                    y,
                    "\u{2503}", // ┃ heavy vertical
                    Style::default().fg(color),
                );
            }

            // Per-row window: the segment [row_start, row_end). For non-wrap
            // this is exactly the horizontally-scrolled view; for wrap it is
            // one folded segment. The overlay painters treat `row_start` as
            // the scroll offset and `row_width` as the text width, so the
            // same code paints both modes.
            let raw = &self.lines[line_idx];
            let line_len = self.line_char_len(line_idx);
            let row_width = (row_end - row_start) as u16;
            let byte_start = byte_index_of_char(raw, row_start);
            let byte_end = byte_index_of_char(raw, row_end);
            let visible_raw = &raw[byte_start..byte_end];
            let seg_bytes = byte_end - byte_start;
            let empty: Vec<HiSpan> = Vec::new();
            let line_spans = self.highlights.get(line_idx).unwrap_or(&empty);
            // Paint the LSP semantic overlay over the tree-sitter base so
            // resolved symbols (parameters, etc.) win wherever the server
            // has an opinion, syntax fills the gaps (the "combined" model).
            let sem_spans = self.semantic_overlay.get(line_idx).unwrap_or(&empty);
            let merged = merge_overlay(line_spans, sem_spans);
            // Inlay hints anchored inside this row's window, as (anchor col,
            // display cells) pairs. A hint's label is spliced into the row
            // BEFORE the character at its anchor; every buffer column at or
            // past an anchor therefore paints `inlay_cells_before` cells
            // further right, and each overlay painter below translates its
            // columns through that same map so highlights, underlines, and
            // carets stay glued to their glyphs.
            let hint_cap = row_end.min(line_len);
            let hint_cells: Vec<(usize, usize)> = self
                .row_inlay_spans(line_idx)
                .iter()
                .filter(|(hc, _, _)| *hc >= row_start && *hc <= hint_cap)
                .map(|(hc, l, _)| (*hc, l.chars().count()))
                .collect();
            let ex = |c: usize| inlay_cells_before(&hint_cells, c);
            // Caret placement uses the strictly-before rule instead: a
            // caret AT a hint's anchor sits left of the hint (the
            // `cursor_screen_pos` / VS Code convention), while a CHARACTER
            // at the anchor paints right of it (`ex` above).
            let ex_caret = |c: usize| inlay_cells_strictly_before(&hint_cells, c);
            if hint_cells.is_empty() {
                // Shift highlight spans to the row origin and clip them to the
                // segment so build_line_spans never slices past `visible_raw`.
                let shifted: Vec<HiSpan> = shift_spans_for_view(&merged, byte_start)
                    .into_iter()
                    .filter_map(|mut sp| {
                        if sp.start >= seg_bytes {
                            return None;
                        }
                        sp.end = sp.end.min(seg_bytes);
                        Some(sp)
                    })
                    .collect();
                let spans = build_line_spans(visible_raw, &shifted);
                buf.set_line(text_x, y, &Line::from(spans), row_width);
            } else {
                // Splice each hint label between the text segments it splits.
                // Style A (Zed's look): dim italic text, one shade quieter
                // than comments, no chip background.
                let hint_style = Style::default()
                    .fg(self.theme.ignored_fg())
                    .add_modifier(Modifier::ITALIC);
                let mut out: Vec<Span> = Vec::new();
                let mut from = row_start;
                for (hcol, label, swatch) in self
                    .row_inlay_spans(line_idx)
                    .iter()
                    .filter(|(hc, _, _)| *hc >= row_start && *hc <= hint_cap)
                {
                    if *hcol > from {
                        out.extend(inlay_text_segment(raw, &merged, from, *hcol));
                    }
                    // A color swatch paints in ITS color (#254); plain
                    // hints keep the dim italic Zed look.
                    let style = match swatch {
                        Some(c) => Style::default().fg(*c),
                        None => hint_style,
                    };
                    out.push(Span::styled(label.clone(), style));
                    from = from.max(*hcol);
                }
                if row_end > from {
                    out.extend(inlay_text_segment(raw, &merged, from, row_end));
                }
                buf.set_line(text_x, y, &Line::from(out), row_width);
            }

            // Bracket-pair colorization (#131): recolour each bracket cell by
            // its nesting depth (red for an unmatched closer). A foreground
            // override on the already-painted glyph, so selection / search /
            // occurrence layers below still lay their backgrounds over it —
            // and the find layer, painted later, wins outright like it does
            // over syntax colours. Columns translate through inlay cells and
            // the row window like every painter; wrap continuation segments
            // participate (brackets live anywhere in the line, unlike the
            // leading-whitespace guides).
            if self.show_bracket_colors
                && let Some(cols) = self.bracket_colors.get(line_idx)
            {
                for &(c, ci) in cols {
                    if c < row_start {
                        continue;
                    }
                    let col = (c + ex(c) - row_start) as u16;
                    if col >= row_width {
                        break;
                    }
                    let fg = if ci == UNEXPECTED_BRACKET {
                        self.theme.bracket_unexpected_fg()
                    } else {
                        self.theme.bracket_pair_color(usize::from(ci))
                    };
                    let cell = &mut buf[(text_x + col, y)];
                    let style = cell.style().fg(fg);
                    cell.set_style(style);
                }
            }

            // Indentation guides (VS Code `editor.guides.indentation`): a dim
            // │ at each indent-unit column of the line's leading whitespace,
            // with the cursor block's guide highlighted. Painted into blank
            // cells only, right after the text, so text always wins and the
            // overlays below (selection band, conflict tints) lay their
            // backgrounds over the glyph. A wrap continuation row carries no
            // leading whitespace, so only a line's first segment draws guides;
            // in the flat path `row_start` is the horizontal scroll and
            // translates the columns like every other painter here.
            if self.show_indent_guides && (!wrap || row_start == 0) {
                for c in (0..self.guide_indent_width(line_idx)).step_by(guide_step) {
                    if c < row_start {
                        continue;
                    }
                    let col = (c + ex(c) - row_start) as u16;
                    if col >= row_width {
                        break;
                    }
                    let cell = &mut buf[(text_x + col, y)];
                    if cell.symbol() != " " {
                        continue;
                    }
                    let fg = match active_guide {
                        Some((ac, lo, hi)) if ac == c && (lo..=hi).contains(&line_idx) => {
                            guide_active_fg
                        }
                        _ => guide_fg,
                    };
                    cell.set_symbol("\u{2502}");
                    let style = cell.style().fg(fg);
                    cell.set_style(style);
                }
            }

            // Render whitespace (#133): swap space/tab cells to `·`/`→` in a
            // dim theme colour — across the whole row in All mode, inside the
            // primary and secondary-caret selections in Selection mode (the
            // VS Code default). Painted after the indent guides so the glyph
            // wins the cell: it reports a real character where the guide only
            // decorates, and the selection band below touches backgrounds
            // only, so the glyph rides the band like text does.
            if self.whitespace_mode != WhitespaceMode::None {
                let ws_fg = self.theme.whitespace_fg();
                let mut ws_spans: Vec<(usize, usize)> = Vec::new();
                match self.whitespace_mode {
                    WhitespaceMode::All => ws_spans.push((0, line_len)),
                    WhitespaceMode::Selection => {
                        if let Some(((sr, sc), (er, ec))) = sel_norm
                            && line_idx >= sr
                            && line_idx <= er
                        {
                            let s = if line_idx == sr { sc } else { 0 };
                            let e = if line_idx == er { ec } else { line_len };
                            if e > s {
                                ws_spans.push((s, e));
                            }
                        }
                        for caret in &self.carets {
                            let ((cr0, cc0), (cr1, cc1)) = caret.normalised();
                            if line_idx >= cr0 && line_idx <= cr1 {
                                let s = if line_idx == cr0 { cc0 } else { 0 };
                                let e = if line_idx == cr1 { cc1 } else { line_len };
                                if e > s {
                                    ws_spans.push((s, e));
                                }
                            }
                        }
                    }
                    WhitespaceMode::None => {}
                }
                for (s, e) in ws_spans {
                    let from = s.max(row_start);
                    let to = e.min(row_end).min(line_len);
                    if to <= from {
                        continue;
                    }
                    for (ci, ch) in raw.chars().enumerate().skip(from).take(to - from) {
                        let glyph = match ch {
                            ' ' => "\u{b7}",
                            '\t' => "\u{2192}",
                            _ => continue,
                        };
                        let col = (ci + ex(ci) - row_start) as u16;
                        if col >= row_width {
                            break;
                        }
                        let cell = &mut buf[(text_x + col, y)];
                        cell.set_symbol(glyph);
                        let style = cell.style().fg(ws_fg);
                        cell.set_style(style);
                    }
                }
            }

            // Merge-conflict region tints (VS Code's current/incoming
            // backgrounds), painted right after the text so diagnostics,
            // search highlights, and the selection all win over them.
            if let Some(tint) = conflict_row_tint(&self.conflicts, line_idx, self.theme) {
                paint_full_row_bg(buf, text_x, y, row_width, tint);
            }
            // Clickable accept actions on the conflict header row — VS Code's
            // merge-conflict CodeLens, drawn after the marker text where the
            // `<<<<<<<` line is otherwise dead space. Hit spans are recorded
            // per frame (cleared at render start) so the rects always
            // describe the painted frame (#103's invariant).
            if let Some(block_idx) = self.conflicts.iter().position(|b| b.ours_start == line_idx) {
                let block = self.conflicts[block_idx];
                let marker_len = self
                    .lines
                    .get(line_idx)
                    .map(|l| l.chars().count() as u16)
                    .unwrap_or(0)
                    .saturating_sub(row_start as u16);
                let mut x = text_x + marker_len.min(row_width) + 2;
                let actions: [(&str, crate::merge::Resolution); 3] = [
                    ("[Accept Current]", crate::merge::Resolution::Current),
                    ("[Accept Incoming]", crate::merge::Resolution::Incoming),
                    ("[Accept Both]", crate::merge::Resolution::Both),
                ];
                for (label, res) in actions {
                    let w = label.len() as u16;
                    if x + w > text_x + row_width {
                        break;
                    }
                    buf.set_string(
                        x,
                        y,
                        label,
                        Style::default()
                            .fg(self.theme.ui(Color::Rgb(0x8a, 0xb4, 0xf8)))
                            .add_modifier(Modifier::UNDERLINED),
                    );
                    self.merge_action_spans
                        .push((y, x..x + w, block.ours_start, res));
                    x += w + 2;
                }
            }

            // LSP diagnostics: underline each problem span in its severity
            // colour, clipped to the visible row window. VS Code draws a wavy
            // underline; the crossterm backend exposes only a straight one, so
            // a coloured straight underline carries the severity instead. The
            // underline colour is independent of the glyph's foreground, so the
            // syntax/semantic colour underneath is preserved.
            let empty_diag: Vec<(usize, usize, crate::lsp::manager::DiagnosticSeverity)> =
                Vec::new();
            for &(sc, ec, severity) in self.diagnostic_spans.get(line_idx).unwrap_or(&empty_diag) {
                let vs = (sc + ex(sc)).saturating_sub(row_start);
                // The exclusive end shifts by the hints before its LAST char,
                // so an underline never swallows a hint anchored right at it.
                let ve = (ec + ex(ec.saturating_sub(1))).saturating_sub(row_start);
                if ve > vs {
                    paint_diagnostic_underline(
                        buf, text_x, y, row_width, vs, ve, severity, self.theme,
                    );
                }
            }

            // LSP occurrences of the symbol under the caret (word highlight):
            // painted before the find layer, the selection band, and the
            // bracket match so all three stay visible on top. The find layer
            // in particular paints black-on-gold; an occurrence bg over it
            // left black text on a dark grey (the caret parked on a find
            // match makes the two layers cover the same cells).
            for &(occ_row, occ_start, occ_end, occ_write) in &self.occurrences {
                if occ_row != line_idx {
                    continue;
                }
                let bg = if occ_write {
                    self.theme.occurrence_write_bg()
                } else {
                    self.theme.occurrence_bg()
                };
                for c in occ_start..occ_end {
                    if c < row_start {
                        continue;
                    }
                    let col = (c + inlay_cells_before(&hint_cells, c) - row_start) as u16;
                    if col >= row_width {
                        break;
                    }
                    let cell = &mut buf[(text_x + col, y)];
                    cell.set_style(cell.style().bg(bg));
                }
            }

            if let Some(term) = self.search_highlight.as_deref() {
                let active_on_line = self
                    .active_search_match
                    .filter(|(r, _, _)| *r == line_idx)
                    .map(|(_, c, l)| (c, l));
                paint_search_highlight(
                    buf,
                    text_x,
                    y,
                    row_width,
                    raw,
                    term,
                    self.search_highlight_opts,
                    row_start,
                    active_on_line,
                    &hint_cells,
                    None,
                    self.theme,
                );
            }

            if let Some(needle) = occ_needle.as_deref() {
                paint_selection_occurrences(
                    buf,
                    text_x,
                    y,
                    row_width,
                    raw,
                    needle,
                    row_start,
                    &hint_cells,
                    self.theme,
                );
            }

            if let Some(((sr, sc), (er, ec))) = sel_norm
                && line_idx >= sr
                && line_idx <= er
            {
                let sel_start = if line_idx == sr { sc } else { 0 };
                // For non-final selected rows, paint past the content by one
                // cell to make the trailing newline visible.
                let sel_end = if line_idx == er { ec } else { line_len + 1 };
                let visible_start = (sel_start + ex(sel_start)).saturating_sub(row_start);
                let visible_end =
                    (sel_end + ex(sel_end.saturating_sub(1))).saturating_sub(row_start);
                if visible_end > visible_start {
                    paint_selection_band(
                        buf,
                        text_x,
                        y,
                        row_width,
                        visible_start,
                        visible_end,
                        self.theme,
                    );
                }
            }

            // Bracket-match highlight: tint the two matched bracket cells that
            // fall on this visual row, honouring horizontal scroll.
            if let Some((open, close)) = bracket_pair {
                for pos in [open, close] {
                    if pos.0 == line_idx {
                        paint_bracket_match(
                            buf,
                            text_x,
                            y,
                            row_width,
                            pos.1 + ex(pos.1),
                            row_start,
                            self.theme,
                        );
                    }
                }
            }

            // Secondary multi-cursor carets ("Change All Occurrences"): paint
            // each one's selection band and a software block cursor at its
            // head. The primary cursor is the hardware caret; these blocks
            // mark the extra cursors so the user sees every edit point.
            for caret in &self.carets {
                let ((cr0, cc0), (cr1, cc1)) = caret.normalised();
                if line_idx >= cr0 && line_idx <= cr1 {
                    let cs = if line_idx == cr0 { cc0 } else { 0 };
                    let ce = if line_idx == cr1 { cc1 } else { line_len + 1 };
                    let vs = (cs + ex(cs)).saturating_sub(row_start);
                    let ve = (ce + ex(ce.saturating_sub(1))).saturating_sub(row_start);
                    if ve > vs {
                        paint_selection_band(buf, text_x, y, row_width, vs, ve, self.theme);
                    }
                }
                // Only paint the block on the visual row that holds the caret.
                let c = caret.head.1;
                let on_row = caret.head.0 == line_idx
                    && c >= row_start
                    && (c < row_end || (c == row_end && row_end == line_len));
                if on_row {
                    // Strictly-before, not `ex`: a caret AT a hint's anchor
                    // sits left of the hint like the primary caret
                    // (`cursor_screen_pos`), marking the cell its typed
                    // character will land on, inside its own band.
                    paint_block_cursor(
                        buf,
                        text_x,
                        y,
                        row_width,
                        c + ex_caret(c),
                        row_start,
                        self.theme,
                    );
                }
            }

            // Collaborators' ghost carets (multiplayer): the same block shape
            // as secondary carets, in each participant's color. The local
            // hardware caret is painted by the terminal after drawing, so it
            // always sits above these.
            for &(gr, gc, color) in &self.ghost_carets {
                let on_row = gr == line_idx
                    && gc >= row_start
                    && (gc < row_end || (gc == row_end && row_end == line_len));
                if on_row {
                    paint_ghost_caret(
                        buf,
                        text_x,
                        y,
                        row_width,
                        gc + ex_caret(gc),
                        row_start,
                        color,
                    );
                }
            }

            // GitLens-style inline blame: a dim italic annotation trailing the
            // cursor's line, on its last visual segment, in the focused editor
            // only. Painted last so no overlay covers it, and clipped to the
            // pane's right edge.
            if self.focused
                && line_idx == self.cursor_row
                && row_end >= line_len
                && !self.inline_values.contains_key(&line_idx)
                && let Some(note) = self.current_line_blame_annotation()
            {
                let text_cols = (line_len + ex(line_len)).saturating_sub(row_start);
                let start_x = text_x + text_cols as u16 + 2;
                let right = inner.x + inner.width;
                if start_x < right {
                    let avail = (right - start_x) as usize;
                    let shown: String = note.chars().take(avail).collect();
                    buf.set_string(
                        start_x,
                        y,
                        &shown,
                        Style::default()
                            .fg(self.theme.ignored_fg())
                            .add_modifier(Modifier::ITALIC),
                    );
                }
            }

            // Debugger inline values (#135): the "name = value" trailer for a
            // stopped session, on the line's last visual segment like the
            // blame above (which yields to it on the cursor line — while
            // stepping, state beats history). Clipped to the pane edge.
            if row_end >= line_len
                && let Some(note) = self.inline_values.get(&line_idx)
            {
                let text_cols = (line_len + ex(line_len)).saturating_sub(row_start);
                let start_x = text_x + text_cols as u16 + 2;
                let right = inner.x + inner.width;
                if start_x < right {
                    let avail = (right - start_x) as usize;
                    let shown: String = note.chars().take(avail).collect();
                    buf.set_string(
                        start_x,
                        y,
                        &shown,
                        Style::default()
                            .fg(self.theme.ignored_fg())
                            .add_modifier(Modifier::ITALIC),
                    );
                }
            }

            // The cursor itself is drawn by the host terminal as a hardware
            // caret (DECSCUSR `BlinkingBar`); App calls
            // `frame.set_cursor_position(...)` so the blink/overlay never
            // hides the underlying character.
        }

        // Ghost caret name tags (VS Code Live Share): while a peer's caret is
        // inside its fade window the App feeds a label here, painted on the
        // visual row above the caret — below when the caret sits on the
        // viewport's top row — at the caret's on-screen column. Painted after
        // the row loop (a tag overlays a neighbouring row's content) but
        // before sticky scroll, which floats above everything.
        for (cl, cc, name, color) in &self.ghost_caret_labels {
            let Some(caret_vidx) = visual_rows.iter().position(|r| {
                matches!(r, VisRow::Text { line: li, start: s, end: e }
                    if *li == *cl && *cc >= *s
                        && (*cc < *e || (*cc == *e && *e == self.line_char_len(*li))))
            }) else {
                continue;
            };
            let label_vidx = if caret_vidx > 0 {
                caret_vidx - 1
            } else {
                caret_vidx + 1
            };
            if label_vidx >= visual_rows.len() {
                continue;
            }
            // The caret's on-screen column: translate through the caret row's
            // own inlay map, exactly as its ghost cell was painted.
            let VisRow::Text {
                start: caret_start,
                end: caret_end,
                ..
            } = visual_rows[caret_vidx]
            else {
                continue;
            };
            let hint_cap = caret_end.min(self.line_char_len(*cl));
            let hint_cells: Vec<(usize, usize)> = self
                .row_inlay_spans(*cl)
                .iter()
                .filter(|(hc, _, _)| *hc >= caret_start && *hc <= hint_cap)
                .map(|(hc, l, _)| (*hc, l.chars().count()))
                .collect();
            let col_cells = cc + inlay_cells_before(&hint_cells, *cc);
            if col_cells < caret_start {
                continue;
            }
            let x0 = (col_cells - caret_start) as u16;
            let y = inner.y + label_vidx as u16;
            let style = Style::default()
                .fg(Color::Black)
                .bg(*color)
                .add_modifier(Modifier::BOLD);
            for (i, ch) in name.chars().enumerate() {
                let x = x0 + i as u16;
                if x >= text_width {
                    break;
                }
                buf[(text_x + x, y)].set_char(ch).set_style(style);
            }
        }

        // Sticky scroll: pin the enclosing scope headers to the top of the
        // viewport, overpainting the topmost content rows (VS Code's sticky
        // widget floats over the content the same way). Non-wrap only; the app
        // supplies the header lines outermost-first, already filtered to those
        // scrolled above the viewport.
        self.sticky_click_rows.clear();
        if !wrap && !self.sticky_lines.is_empty() && inner.height > 1 {
            let bg = self.theme.sticky_scroll_bg();
            let text_w = (inner.x + inner.width).saturating_sub(text_x);
            // Never paint over the caret's own row: a caret under the band
            // cannot be seen at all. `caret_floor_row` keeps a scroll from
            // parking it there, so this normally trims nothing; it still has to
            // hold for the frames where the caret arrived some other way.
            //
            // The caret's PAINTED row, from the layout built above — counting
            // buffer lines instead would overstate it whenever a collapsed fold
            // or a comment box sits between the top of the viewport and the
            // caret, and the band would paint straight over it. `None` means
            // the caret's line is not painted at all (scrolled off, or hidden
            // inside a fold), so nothing needs to be kept clear.
            let rows_avail = (inner.height - 1) as usize;
            let caret_row = self
                .last_wrap_rows
                .iter()
                .position(|r| matches!(r, VisRow::Text { line, .. } if *line == self.cursor_row));
            let max_rows = match caret_row {
                Some(row) => rows_avail.min(row),
                None => rows_avail,
            };
            let sticky = self.sticky_lines.clone();
            for (i, &line) in sticky.iter().take(max_rows).enumerate() {
                let y = inner.y + i as u16;
                for x in inner.x..inner.x + inner.width {
                    buf[(x, y)].set_symbol(" ");
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
                let line_no = format!(
                    "{:>width$} ",
                    line + 1,
                    width = gutter_width.saturating_sub(1) as usize
                );
                buf.set_string(
                    inner.x,
                    y,
                    &line_no,
                    Style::default().fg(Color::DarkGray).bg(bg),
                );
                self.paint_highlighted_line(buf, text_x, y, text_w, line as usize);
                self.sticky_click_rows.push((y, line));
            }
            // The band just overpainted those rows, so any [Accept …] span
            // recorded there no longer matches what is on screen — a click
            // on a sticky header must never resolve an unseen conflict.
            let band_end = inner.y + sticky.len().min(max_rows) as u16;
            self.merge_action_spans
                .retain(|(y, _, _, _)| *y >= band_end);
        }

        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }
        if let Some(metrics) = hbar_metrics {
            scrollbar::render_horizontal(buf, metrics, self.focused, self.theme);
        }
    }
}

impl Editor {
    /// Paint the rendered Markdown preview: the pre-built lines flow through
    /// a wrapping `Paragraph` so paragraphs reflow with the pane, scrolled by
    /// the preview's own offset, with a scrollbar over the wrapped height.
    /// Rebuilds first when the buffer moved under the preview (a live edit in
    /// a split, an external reload) so it always shows the current text.
    fn render_markdown_preview(&mut self, inner: Rect, buf: &mut Buffer) {
        let stale = self
            .markdown_preview
            .as_ref()
            .is_some_and(|md| md.built_seq != self.edit_seq);
        let is_nb = self.markdown_preview.as_ref().is_some_and(|md| md.notebook);
        if stale && is_nb {
            // Notebook rebuild (#180): keep the scroll, refresh the rest.
            // A JSON made invalid by the edit clears the preview and
            // falls back to the raw text (#199 review) - retaining the
            // stale render would repaint and re-fail every frame.
            let scroll = self
                .markdown_preview
                .as_ref()
                .map(|m| m.scroll)
                .unwrap_or(0);
            if self.build_notebook_preview() {
                if let Some(md) = self.markdown_preview.as_mut() {
                    md.scroll = scroll;
                }
            } else {
                self.markdown_preview = None;
                return;
            }
        } else if stale {
            // The image-aware builder (#176): the plain one here left the
            // images list pointing at STALE anchors after a live edit.
            let text = self.lines.join("\n");
            let base = self
                .path
                .as_ref()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()));
            let (lines, images, runnables) = crate::markdown::render_markdown_full(
                &text,
                self.theme,
                &mut self.registry,
                base.as_deref(),
                self.md_outputs.clone(),
            );
            if let Some(md) = self.markdown_preview.as_mut() {
                md.lines = lines;
                md.images = images;
                md.runnables = runnables;
                md.built_seq = self.edit_seq;
            }
        }
        let Some(md) = self.markdown_preview.as_mut() else {
            return;
        };
        // One-cell left margin; the right column stays free for the bar.
        let text_area = Rect {
            x: inner.x + 1,
            width: inner.width.saturating_sub(3),
            ..inner
        };
        if text_area.width == 0 {
            return;
        }
        let para = Paragraph::new(Text::from(md.lines.clone())).wrap(Wrap { trim: false });
        let total = para.line_count(text_area.width);
        let max_scroll = total.saturating_sub(inner.height as usize) as u16;
        md.scroll = md.scroll.min(max_scroll);
        // Anchor mapping (#176): each image's first reserved line as a
        // VISUAL row, through the same wrap the paragraph uses. Cached on
        // (built_seq, width) - blank lines never wrap, so the prefix
        // line_count is exact.
        if md.wrap_key != (md.built_seq, text_area.width) {
            let visual_row = |first_line: usize| {
                let prefix: Vec<Line> = md.lines[..first_line].to_vec();
                Paragraph::new(Text::from(prefix))
                    .wrap(Wrap { trim: false })
                    .line_count(text_area.width)
            };
            md.anchor_rows = md
                .images
                .iter()
                .map(|img| visual_row(img.first_line))
                .collect();
            // Same mapping for the play glyphs (#353); a glyph line can
            // wrap, but its first visual row is where the glyph paints.
            md.run_rows = md
                .runnables
                .iter()
                .map(|r| visual_row(r.first_line))
                .collect();
            md.wrap_key = (md.built_seq, text_area.width);
        }
        md.last_area = text_area;
        para.scroll((md.scroll, 0)).render(text_area, buf);
        // Frame truth for selection (#215): the rendered view is a wrapped
        // Paragraph, so the only faithful record of what the user sees is
        // the cells we just painted. Read them back per row, then tint the
        // selected span.
        md.rows = (0..text_area.height)
            .map(|dy| {
                (0..text_area.width)
                    .map(|dx| {
                        buf[(text_area.x + dx, text_area.y + dy)]
                            .symbol()
                            .to_string()
                    })
                    .collect::<Vec<String>>()
            })
            .collect();
        if md.has_selection() {
            let sel_bg = self.theme.selection();
            for dy in 0..text_area.height {
                for dx in 0..text_area.width {
                    if md.cell_selected(dy, dx) {
                        buf[(text_area.x + dx, text_area.y + dy)].set_bg(sel_bg);
                    }
                }
            }
        }
        if let Some(metrics) = scrollbar::vertical_metrics(
            Rect {
                x: inner.x + inner.width.saturating_sub(1),
                y: inner.y,
                width: 1,
                height: inner.height,
            },
            total,
            inner.height as usize,
            md.scroll as usize,
        ) {
            self.last_scrollbar = metrics.area;
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }
    }

    /// Paint logical line `line_idx`'s syntax/semantic-highlighted text at row
    /// `y` from column 0 (no horizontal scroll), clipped to `width`. Used by
    /// the sticky-scroll bar to render pinned scope headers. Assumes the row
    /// background is already filled by the caller.
    fn paint_highlighted_line(
        &self,
        buf: &mut Buffer,
        text_x: u16,
        y: u16,
        width: u16,
        line_idx: usize,
    ) {
        let Some(raw) = self.lines.get(line_idx) else {
            return;
        };
        let empty: Vec<HiSpan> = Vec::new();
        let line_spans = self.highlights.get(line_idx).unwrap_or(&empty);
        let sem_spans = self.semantic_overlay.get(line_idx).unwrap_or(&empty);
        let merged = merge_overlay(line_spans, sem_spans);
        let raw_bytes = raw.len();
        let clamped: Vec<HiSpan> = merged
            .into_iter()
            .filter_map(|mut sp| {
                if sp.start >= raw_bytes {
                    return None;
                }
                sp.end = sp.end.min(raw_bytes);
                Some(sp)
            })
            .collect();
        let spans = build_line_spans(raw, &clamped);
        buf.set_line(text_x, y, &Line::from(spans), width);
    }

    /// If `(col, row)` lands on a pinned sticky-scroll header, return the
    /// logical line to jump to. `None` when the row is not a sticky header.
    pub fn sticky_line_at(&self, col: u16, row: u16) -> Option<u32> {
        if col < self.last_inner.x || col >= self.last_inner.x + self.last_inner.width {
            return None;
        }
        self.sticky_click_rows
            .iter()
            .find(|&&(y, _)| y == row)
            .map(|&(_, line)| line)
    }

    /// Absolute (column, row) of the editor's cursor in screen coordinates,
    /// or `None` if the cursor is outside the visible viewport. Used by
    /// `App::render` to position the host terminal's hardware caret.
    pub fn cursor_screen_pos(&self) -> Option<(u16, u16)> {
        if self.last_inner.height == 0 {
            return None;
        }
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        if self.wrap_enabled() {
            // Find the visual row holding the cursor in the layout the last
            // render captured (a logical line spans several wrapped rows). A
            // column on a wrap boundary belongs to the next row's start.
            let idx = self.last_wrap_rows.iter().position(|r| {
                matches!(r, VisRow::Text { line, start, end }
                    if *line == self.cursor_row
                        && self.cursor_col >= *start
                        && (self.cursor_col < *end
                            || (self.cursor_col == *end
                                && *end == self.line_char_len(*line))))
            })?;
            let (_, start, _) = self.text_row(idx)?;
            // The caret is valid on every cell of the segment AND one past
            // its last cell (end of line / blank line — where the user is
            // about to type). Bail only when the cell would fall outside the
            // text column: past the pane edge or onto the vertical scrollbar.
            let visible_col = self.cursor_col - start;
            let x = text_x as usize + visible_col;
            let right = (self.last_inner.x + self.last_inner.width)
                .saturating_sub(u16::from(self.last_scrollbar.width > 0))
                as usize;
            if x >= right {
                return None;
            }
            return Some((x as u16, self.last_inner.y + idx as u16));
        }
        // Non-wrap: one row per line, horizontally scrolled by `scroll_col`;
        // comment boxes shift the rows below them, so map through the
        // painted layout when one exists.
        if self.cursor_row < self.scroll {
            return None;
        }
        let row_in_view = if self.last_wrap_rows.is_empty() {
            self.cursor_row - self.scroll
        } else {
            self.last_wrap_rows
                .iter()
                .position(|r| matches!(r, VisRow::Text { line, .. } if *line == self.cursor_row))?
        };
        if (row_in_view as u16) >= self.last_inner.height {
            return None;
        }
        let text_width = self
            .last_inner
            .width
            .saturating_sub(self.last_gutter_width + 2 + u16::from(self.last_scrollbar.width > 0));
        if text_width == 0 || self.cursor_col < self.scroll_col {
            return None;
        }
        // Inlay-hint cells before the caret shift it right. Strictly-before
        // (`hc < cursor_col`, not `<=`): a caret AT a hint's anchor sits left
        // of the hint, like VS Code, so typing there pushes the hint along.
        let extra = self.inlay_cells_before_cursor(self.cursor_row, self.scroll_col);
        let visible_col = self.cursor_col - self.scroll_col + extra;
        if (visible_col as u16) >= text_width {
            return None;
        }
        Some((
            text_x + visible_col as u16,
            self.last_inner.y + row_in_view as u16,
        ))
    }

    /// Hint cells spliced between `scroll_col` and the caret on `line`
    /// (anchors strictly before `cursor_col`, at or past `scroll_col`).
    fn inlay_cells_before_cursor(&self, line: usize, scroll_col: usize) -> usize {
        self.row_inlay_spans(line)
            .iter()
            .filter(|(hc, _, _)| *hc >= scroll_col && *hc < self.cursor_col)
            .map(|(_, l, _)| l.chars().count())
            .sum()
    }

    /// Screen cell of the diff view's read-only caret (its selection head),
    /// clamped to the viewport. The exact inverse of `diff_hit_test`: feed
    /// it the same layout (`last_inner`, gutters, seam, scroll) so the
    /// blinking caret lands on the cell a click there would map back to.
    /// `None` when there's no diff, no caret, or the caret is scrolled out.
    pub fn diff_caret_screen_pos(&self) -> Option<(u16, u16)> {
        use crate::widgets::diff::DiffSide;
        let diff = self.diff.as_ref()?;
        if diff.unified {
            return None;
        }
        let (side, row_idx, char_col) = diff.caret()?;
        let inner = self.last_inner;
        if inner.width < 16 || inner.height < 3 {
            return None;
        }
        let body_top = inner.y + 1;
        let body_height = inner.height.saturating_sub(2);
        if body_height == 0 || row_idx < diff.scroll {
            return None;
        }
        let vis_row = row_idx - diff.scroll;
        if vis_row as u16 >= body_height {
            return None;
        }
        let half = inner.width / 2;
        if half < 8 {
            return None;
        }
        let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
        let r_gutter = (diff.right_lines.len() + 1).to_string().len() as u16 + 1;
        let (text_x, text_w) = match side {
            DiffSide::Left => (
                inner.x + l_gutter + 2,
                half.saturating_sub(l_gutter + 2 + 1),
            ),
            DiffSide::Right => (
                inner.x + half + 1 + r_gutter + 2,
                (inner.width - (half + 1)).saturating_sub(r_gutter + 2),
            ),
        };
        if text_w == 0 || char_col < diff.scroll_x {
            return None;
        }
        let vis_col = char_col - diff.scroll_x;
        if vis_col as u16 >= text_w {
            return None;
        }
        Some((text_x + vis_col as u16, body_top + vis_row as u16))
    }
}

/// Overpaint every match of `needle` in `raw_line` with the search-match
/// style, honouring `opts` (case-sensitive / whole-word / regex). Delegates
/// to `split_for_highlight` so the highlight rule stays 1:1 with the
/// search-engine matcher; column conversion uses `chars().count()` over
/// the byte prefix to stay correct for Unicode.
/// Hint cells inserted at or before buffer column `c` on this row: the
/// display-column shift every overlay painter applies past inlay hints.
/// `hints` is the row's visible (anchor col, label cells) set.
fn inlay_cells_before(hints: &[(usize, usize)], c: usize) -> usize {
    hints
        .iter()
        .filter(|(hc, _)| *hc <= c)
        .map(|(_, n)| n)
        .sum()
}

/// Hint cells strictly before buffer column `c`: the shift for a CARET at
/// `c`, which sits left of a hint anchored exactly there (a character at
/// `c` sits right of it — that is [`inlay_cells_before`]).
fn inlay_cells_strictly_before(hints: &[(usize, usize)], c: usize) -> usize {
    hints.iter().filter(|(hc, _)| *hc < c).map(|(_, n)| n).sum()
}

/// The syntax-highlighted spans of `raw`'s character range `[from, to)`,
/// clipped out of the line's merged highlight spans. Paints the text
/// segments between spliced inlay-hint labels.
fn inlay_text_segment<'a>(
    raw: &'a str,
    merged: &[HiSpan],
    from: usize,
    to: usize,
) -> Vec<Span<'a>> {
    let byte_from = byte_index_of_char(raw, from);
    let byte_to = byte_index_of_char(raw, to);
    let seg = &raw[byte_from..byte_to];
    let seg_bytes = byte_to - byte_from;
    let shifted: Vec<HiSpan> = shift_spans_for_view(merged, byte_from)
        .into_iter()
        .filter_map(|mut sp| {
            if sp.start >= seg_bytes {
                return None;
            }
            sp.end = sp.end.min(seg_bytes);
            Some(sp)
        })
        .collect();
    build_line_spans(seg, &shifted)
}

/// The (inactive, active) find-highlight styles, shared by the editor's
/// highlighter and the rendered log's so the two tabs read the same.
fn search_highlight_styles(theme: crate::theme::Theme) -> (Style, Style) {
    let inactive = Style::default()
        .fg(Color::Black)
        .bg(theme.ui(Color::Rgb(0xff, 0xd7, 0x4a)))
        .add_modifier(Modifier::BOLD);
    let active = Style::default()
        .fg(Color::Black)
        .bg(theme.ui(Color::Rgb(0xff, 0x8c, 0x2a)))
        .add_modifier(Modifier::BOLD);
    (inactive, active)
}

// Render helper: each argument is an independent painting input (buffer,
// geometry, text, styling); bundling them into a struct would add indirection
// without improving clarity.
#[allow(clippy::too_many_arguments)]
fn paint_search_highlight(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    raw_line: &str,
    needle: &str,
    opts: crate::widgets::search::SearchOpts,
    scroll_col: usize,
    active_match_on_line: Option<(usize, usize)>,
    hints: &[(usize, usize)],
    cells: Option<&crate::cell_map::CellMap>,
    theme: crate::theme::Theme,
) {
    if needle.is_empty() {
        return;
    }
    let (inactive_style, active_style) = search_highlight_styles(theme);
    let segments = crate::widgets::search::split_for_highlight(raw_line, needle, opts);
    // `abs_col` tracks the absolute character index in the original line.
    // Visible columns are `abs_col - scroll_col`, painted only when
    // non-negative and inside `text_width`.
    let mut abs_col: usize = 0;
    for (chunk, is_match) in segments {
        let chunk_cols = chunk.chars().count();
        if is_match {
            let is_active =
                active_match_on_line.is_some_and(|(c, l)| c == abs_col && l == chunk_cols);
            let style = if is_active {
                active_style
            } else {
                inactive_style
            };
            for c in 0..chunk_cols {
                let absolute = abs_col + c;
                if absolute < scroll_col {
                    continue;
                }
                // A rendered log paints by display width (#404), so its
                // band goes through the same cell map the text did; the
                // editor's text path is one cell per character.
                if let Some(cells) = cells {
                    if !paint_log_cells(
                        buf,
                        text_x,
                        text_x + text_width,
                        y,
                        cells,
                        absolute,
                        |_| style,
                    ) {
                        break;
                    }
                    continue;
                }
                let col = (absolute + inlay_cells_before(hints, absolute) - scroll_col) as u16;
                if col >= text_width {
                    break;
                }
                buf[(text_x + col, y)].set_style(style);
            }
        }
        abs_col = abs_col.saturating_add(chunk_cols);
        if abs_col >= scroll_col + text_width as usize {
            break;
        }
    }
}

/// Paint VS Code-style "selection highlight": a muted blue box over every
/// occurrence of `needle` on this line. Matches the literal selected text
/// case-sensitively (like VS Code's `editor.selectionHighlight`). Only the
/// background is repainted, so syntax foreground colours show through. The
/// active selection itself is overpainted afterwards by
/// `paint_selection_band` with a brighter blue, giving the two-tone look.
// Render helper: same independent-inputs shape as paint_search_highlight.
#[allow(clippy::too_many_arguments)]
fn paint_selection_occurrences(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    raw_line: &str,
    needle: &str,
    scroll_col: usize,
    hints: &[(usize, usize)],
    theme: crate::theme::Theme,
) {
    if needle.is_empty() {
        return;
    }
    let bg = theme.ui(Color::Rgb(0x37, 0x61, 0x8e));
    let needle_cols = needle.chars().count();
    let mut search_from = 0usize;
    while let Some(rel) = raw_line[search_from..].find(needle) {
        let byte_start = search_from + rel;
        let char_start = raw_line[..byte_start].chars().count();
        for c in 0..needle_cols {
            let absolute = char_start + c;
            if absolute < scroll_col {
                continue;
            }
            let col = (absolute + inlay_cells_before(hints, absolute) - scroll_col) as u16;
            if col >= text_width {
                break;
            }
            let cell = &mut buf[(text_x + col, y)];
            cell.set_style(cell.style().bg(bg));
        }
        search_from = byte_start + needle.len();
    }
}

/// Paint a software block cursor for a secondary multi-cursor caret at char
/// column `col_char` of row `y`. Uses a reversed bar so it reads as a caret
/// distinct from the host terminal's hardware caret on the primary cursor.
fn paint_block_cursor(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    col_char: usize,
    scroll_col: usize,
    theme: crate::theme::Theme,
) {
    if col_char < scroll_col {
        return;
    }
    let col = col_char - scroll_col;
    if col as u16 >= text_width {
        return;
    }
    let cell = &mut buf[(text_x + col as u16, y)];
    cell.set_style(
        Style::default()
            .fg(Color::Black)
            .bg(theme.ui(Color::Rgb(0xae, 0xc6, 0xff)))
            .add_modifier(Modifier::BOLD),
    );
}

/// Paint one collaborator's caret cell in their color (multiplayer ghost
/// caret): same shape and clipping as `paint_block_cursor`, but tinted per
/// participant so everyone can see where the others are.
fn paint_ghost_caret(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    col_char: usize,
    scroll_col: usize,
    color: Color,
) {
    if col_char < scroll_col {
        return;
    }
    let col = col_char - scroll_col;
    if col as u16 >= text_width {
        return;
    }
    let cell = &mut buf[(text_x + col as u16, y)];
    cell.set_style(
        Style::default()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    );
}

/// Tint the single bracket cell at char column `col_char` of row `y` with the
/// theme's bracket-match background, leaving the glyph's foreground intact so
/// the bracket stays readable. No-op when the cell is scrolled out of view.
fn paint_bracket_match(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    col_char: usize,
    scroll_col: usize,
    theme: crate::theme::Theme,
) {
    if col_char < scroll_col {
        return;
    }
    let col = (col_char - scroll_col) as u16;
    if col >= text_width {
        return;
    }
    let cell = &mut buf[(text_x + col, y)];
    cell.set_style(cell.style().bg(theme.bracket_match_bg()));
}

/// Apply the selection background colour to columns `[start_char..end_char)`
/// of row `y`, where columns are character indices within the editor's text
/// area.  Clamps to the visible width.
/// The background tint for a row inside a merge-conflict block, or `None`
/// outside one. Header / footer marker rows tint stronger than their
/// content, mirroring VS Code's current(green) / incoming(blue) scheme; the
/// diff3 base section and the `=======` separator sit on a neutral grey.
/// Fixed dark tints, like the selection band: both bundled themes are dark,
/// and the hues are semantic (green = yours, blue = theirs), not accents.
/// VS Code's default auto-closing set: the partner a typed opener/quote
/// pairs with, `None` for everything else (closers included — they only
/// type over).
fn auto_close_partner(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        '`' => Some('`'),
        _ => None,
    }
}

fn is_pair_closer(c: char) -> bool {
    matches!(c, ')' | ']' | '}')
}

fn is_pair_quote(c: char) -> bool {
    matches!(c, '"' | '\'' | '`')
}

fn conflict_row_tint(
    blocks: &[crate::merge::ConflictBlock],
    row: usize,
    theme: crate::theme::Theme,
) -> Option<Color> {
    let block = blocks.iter().find(|b| b.contains(row))?;
    let ours_end = block.base_start.unwrap_or(block.sep);
    Some(if row == block.ours_start {
        theme.ui(Color::Rgb(0x2a, 0x4f, 0x33))
    } else if row < ours_end {
        theme.ui(Color::Rgb(0x1b, 0x33, 0x22))
    } else if row < block.sep {
        theme.ui(Color::Rgb(0x2a, 0x2a, 0x2a))
    } else if row == block.sep {
        theme.ui(Color::Rgb(0x28, 0x28, 0x28))
    } else if row == block.theirs_end {
        theme.ui(Color::Rgb(0x1f, 0x41, 0x66))
    } else {
        theme.ui(Color::Rgb(0x16, 0x2b, 0x44))
    })
}

/// Tint every cell of one text row, keeping the glyphs and their colours.
fn paint_full_row_bg(buf: &mut Buffer, text_x: u16, y: u16, text_width: u16, bg: Color) {
    for col in 0..text_width {
        let x = text_x + col;
        let cell = &mut buf[(x, y)];
        cell.set_style(cell.style().bg(bg));
    }
}

fn paint_selection_band(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    start_char: usize,
    end_char: usize,
    theme: crate::theme::Theme,
) {
    let bg = theme.ui(Color::Rgb(0x26, 0x4f, 0x78));
    let s = start_char.min(text_width as usize);
    let e = end_char.min(text_width as usize);
    if e <= s {
        return;
    }
    for col in s..e {
        let x = text_x + col as u16;
        let cell = &mut buf[(x, y)];
        cell.set_style(cell.style().bg(bg));
    }
}

/// Paint a coloured underline across `[start_char, end_char)` of a rendered
/// row to mark an LSP diagnostic. The colour encodes severity, matching VS
/// Code's palette (red error, yellow warning, teal info/hint). Uses the
/// terminal's separate underline colour so the glyph's foreground (its syntax
/// or semantic colour) is left untouched, the way VS Code's squiggle sits
/// under unchanged text.
#[allow(clippy::too_many_arguments)]
fn paint_diagnostic_underline(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    start_char: usize,
    end_char: usize,
    severity: crate::lsp::manager::DiagnosticSeverity,
    theme: crate::theme::Theme,
) {
    use crate::lsp::manager::DiagnosticSeverity;
    let color = match severity {
        DiagnosticSeverity::Error => theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c)),
        DiagnosticSeverity::Warning => theme.ui(Color::Rgb(0xcc, 0xa7, 0x00)),
        DiagnosticSeverity::Information | DiagnosticSeverity::Hint => {
            theme.ui(Color::Rgb(0x3b, 0x9e, 0xff))
        }
    };
    let s = start_char.min(text_width as usize);
    let e = end_char.min(text_width as usize);
    if e <= s {
        return;
    }
    for col in s..e {
        let x = text_x + col as u16;
        let cell = &mut buf[(x, y)];
        cell.set_style(
            cell.style()
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(color),
        );
    }
}

/// Multi-buffer editor: a stack of `Editor` instances with a single active
/// one, plus a 1-row clickable tab strip rendered above the active editor.
/// `Deref`/`DerefMut` aim at the active editor so existing call sites that
/// were written for a single `Editor` continue to work without rewrites.
/// One segment of the breadcrumb bar (VS Code's editor breadcrumbs): a path
/// folder, the file name, or an enclosing symbol. `target` is the caret
/// position a click jumps to; `None` for the informational path/file crumbs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub target: Option<(u32, u32)>,
}

/// A rendered breadcrumb crumb's hit-test span: `(x_start, width, jump target)`.
type BreadcrumbRange = (u16, u16, Option<(u32, u32)>);

pub struct EditorTabs {
    pub editors: Vec<Editor>,
    active: usize,
    /// Breadcrumb segments for the active file, set by `App` each frame from
    /// the workspace-relative path and the outline scope chain at the caret.
    /// Empty hides the bar (e.g. the welcome screen or an unsaved buffer).
    pub breadcrumbs: Vec<Crumb>,
    /// Per-crumb `(x_start, width, target)` recorded by the most recent render,
    /// so `App::handle_mouse` can map a click on the breadcrumb row to a jump.
    breadcrumb_ranges: Vec<BreadcrumbRange>,
    /// Screen row the breadcrumb bar rendered on, or `None` when it was hidden.
    breadcrumb_y: Option<u16>,
    /// Per-tab on-screen `(x_start, width)` recorded by the most recent
    /// render. `tab_at(col, row)` reads this to map mouse clicks to tab
    /// indices.
    tab_screen_ranges: Vec<(u16, u16)>,
    /// Per-tab absolute column where the close `\u{2715}` glyph lives.
    /// `0` means "no close button rendered for this tab" (e.g. when the
    /// tab is the only one — closing it isn't allowed so we hide the X).
    tab_close_x: Vec<u16>,
    tab_strip_y: u16,
    /// The full pane area (tab strip + body) from the most recent render.
    /// Used by `App::handle_mouse` for hit-testing — the active editor's
    /// own `last_area` only covers the body below the strip.
    pub last_full_area: Rect,
    /// Source of truth for the search-match term that's currently being
    /// painted in every tab's body. Each `Editor.search_highlight` is a
    /// copy kept in sync with this; storing it here lets a freshly-created
    /// editor (e.g. after closing the previous tab and opening a new file
    /// from a search hit) inherit the term without the App needing to
    /// re-call `set_search_highlight` after every open.
    search_highlight_term: Option<String>,
    /// Toggle state matching `search_highlight_term`. Same propagation
    /// strategy: every newly-created editor inherits these.
    search_highlight_opts: crate::widgets::search::SearchOpts,
    /// Last pointer cell, fed in by `App` before each render so the tab
    /// strip can paint a hover lift on the tab body under the pointer and a
    /// pill behind the close `\u{2715}` cell when the pointer rests on it.
    /// `None` when the pointer is off-screen or unknown.
    pub hover_pointer: Option<(u16, u16)>,
}

impl EditorTabs {
    pub fn new() -> Self {
        Self {
            editors: vec![Editor::new()],
            active: 0,
            breadcrumbs: Vec::new(),
            breadcrumb_ranges: Vec::new(),
            breadcrumb_y: None,
            tab_screen_ranges: Vec::new(),
            tab_close_x: Vec::new(),
            tab_strip_y: 0,
            last_full_area: Rect::default(),
            search_highlight_term: None,
            search_highlight_opts: crate::widgets::search::SearchOpts::default(),
            hover_pointer: None,
        }
    }

    pub fn tab_count(&self) -> usize {
        self.editors.len()
    }

    /// Set (or clear) the search-match highlight term + opts for every
    /// open tab, so opening another file from search keeps the same query
    /// lit, clearing the search box wipes the highlights, and toggling a
    /// search mode (case / whole-word / regex) re-paints the file with
    /// the matching rule. Also persists term + opts so editors created
    /// after this call (e.g. after a close + reopen) inherit them.
    pub fn set_search_highlight(
        &mut self,
        term: Option<String>,
        opts: crate::widgets::search::SearchOpts,
    ) {
        let normalised = term.filter(|s| !s.is_empty());
        let cleared = normalised.is_none();
        self.search_highlight_term = normalised.clone();
        self.search_highlight_opts = opts;
        for ed in &mut self.editors {
            ed.search_highlight = normalised.clone();
            ed.search_highlight_opts = opts;
            if cleared {
                ed.active_search_match = None;
            }
        }
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The tab strip label for the tab at `idx` (file name, diff pair, or
    /// "untitled"), for status messages. Empty string for an out-of-range idx.
    pub fn tab_display_label(&self, idx: usize) -> String {
        self.editors.get(idx).map(tab_label).unwrap_or_default()
    }

    /// The on-disk path backing the tab at `idx`, if any. `None` for an
    /// out-of-range index or a blank/untitled buffer. Used to seed the tab
    /// context-menu's path-bearing actions (Reveal in Finder, Copy Path).
    pub fn tab_path(&self, idx: usize) -> Option<PathBuf> {
        self.editors.get(idx).and_then(|e| e.path.clone())
    }

    /// The strip's display titles, disambiguated (#167); index-aligned
    /// with `iter_tabs` so projections (OPEN EDITORS) match the strip.
    pub fn tab_display_labels(&self) -> Vec<String> {
        disambiguated_tab_labels(&self.editors)
    }

    #[cfg(test)]
    pub fn tab_strip_y_for_test(&self) -> u16 {
        self.tab_strip_y
    }

    pub fn iter_tabs(&self) -> impl Iterator<Item = &Editor> {
        self.editors.iter()
    }

    pub fn select(&mut self, idx: usize) -> bool {
        if idx >= self.editors.len() {
            return false;
        }
        self.editors[self.active].focused = false;
        self.active = idx;
        self.editors[self.active].focused = true;
        true
    }

    /// Sweep EVERY open tab (not just the active one) for external on-disk
    /// changes. Clean tabs are silently reloaded; dirty tabs are flagged as
    /// conflicts. This is the core of the FS-sync invariant: a file open in
    /// a background tab must reflect disk reality just like the focused one.
    /// `skip` exempts paths from the sweep: a collab guest never reloads
    /// shared files from disk — its replica is authoritative, and the
    /// owner's own reload reaches it as ops (docs/MULTIPLAYER.md, Phase D).
    pub fn reload_externally_changed_tabs(
        &mut self,
        skip: &dyn Fn(&Path) -> bool,
    ) -> ExternalReloadReport {
        let mut report = ExternalReloadReport::default();
        for ed in &mut self.editors {
            // Diff/image/sheet views don't carry an editable text buffer to
            // reload, and a path-less blank tab has nothing to sync.
            if ed.path.is_none() || ed.diff.is_some() {
                continue;
            }
            if ed.path.as_deref().is_some_and(skip) {
                continue;
            }
            let path = ed.path.clone();
            match ed.reload_or_flag_conflict() {
                ExternalChange::Reloaded => {
                    if let Some(p) = path {
                        report.reloaded.push(p);
                    }
                }
                ExternalChange::Conflict => {
                    if let Some(p) = path {
                        report.conflicts.push(p);
                    }
                }
                ExternalChange::ReloadFailed => {
                    if let Some(p) = path {
                        report.failed.push(p);
                    }
                }
                ExternalChange::Unchanged => {}
            }
        }
        report
    }

    /// Discard unsaved edits and reload the given paths from disk — the
    /// "Reload" resolution of an external-change conflict. Returns the paths
    /// actually reverted (a tab must still be open on each).
    pub fn revert_paths_to_disk(&mut self, paths: &[PathBuf]) -> Vec<PathBuf> {
        let mut reverted = Vec::new();
        for ed in &mut self.editors {
            let Some(p) = ed.path.clone() else { continue };
            if paths.iter().any(|q| q == &p) && ed.revert_to_disk().is_ok() {
                reverted.push(p);
            }
        }
        reverted
    }

    /// If any tab currently points at `old`, repoint it to `new`. The on-
    /// disk file has already been moved; this only updates the in-memory
    /// path so subsequent saves and the tab label track the new name.
    pub fn rename_open_path(&mut self, old: &Path, new: &Path) {
        for e in &mut self.editors {
            if e.path.as_deref() == Some(old) {
                e.path = Some(new.to_path_buf());
                // Re-anchor the disk stamp to the new path so the rename
                // isn't mistaken for an external content change on the next
                // FS-sync sweep.
                e.mark_synced_with_disk();
            }
        }
    }

    /// Open `path` in a brand-new tab inserted directly after the active
    /// one, then make that new tab active. Returns the result of the
    /// underlying `Editor::open` so the caller can surface errors.
    pub fn open_in_new_tab(&mut self, path: &Path) -> Result<()> {
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.open(path)?;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Test-only / disk-less helper: insert a tab whose path is set but
    /// whose contents are empty. Production code should call
    /// `open_in_new_tab` so the file is actually loaded from disk.
    pub fn add_tab_with_path(&mut self, path: PathBuf) {
        let mut e = Editor::new();
        e.path = Some(path);
        e.focused = self.editors[self.active].focused;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Close the currently active tab. Refuses (returns false) when only one
    /// tab remains — closing the last would leave the editor pane empty.
    pub fn close_active(&mut self) -> bool {
        self.close_tab(self.active)
    }

    /// Close the tab at `idx`. When more than one tab is open the tab is
    /// removed and `self.active` is shifted so it still points at a valid
    /// tab. When this is the last remaining tab it is reset to the blank
    /// just-launched state instead of being removed (so the editor pane
    /// always has at least one buffer to render). Returns false only on an
    /// out-of-range index.
    pub fn close_tab(&mut self, idx: usize) -> bool {
        if idx >= self.editors.len() {
            return false;
        }
        if self.editors.len() == 1 {
            let was_focused = self.editors[0].focused;
            let mut fresh = Editor::new();
            fresh.focused = was_focused;
            self.editors[0] = fresh;
            self.active = 0;
            return true;
        }
        self.editors.remove(idx);
        if self.active > idx {
            self.active -= 1;
        } else if self.active >= self.editors.len() {
            self.active = self.editors.len() - 1;
        }
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        true
    }

    /// Close every tab whose index ≠ `keep_idx`, except pinned tabs, which
    /// always survive (VS Code "Close Others" never closes a pinned tab). The
    /// kept tab stays active. Returns how many tabs were actually removed (0
    /// when `keep_idx` is out of range or nothing else is closeable). Mirrors
    /// VS Code's "Close Others" context-menu action.
    pub fn close_others(&mut self, keep_idx: usize) -> usize {
        if keep_idx >= self.editors.len() || self.editors.len() <= 1 {
            return 0;
        }
        let before = self.editors.len();
        let mut new_active = 0;
        let mut kept: Vec<Editor> = Vec::with_capacity(before);
        for (i, ed) in std::mem::take(&mut self.editors).into_iter().enumerate() {
            if i == keep_idx {
                new_active = kept.len();
                kept.push(ed);
            } else if ed.pinned {
                kept.push(ed);
            }
        }
        let removed = before - kept.len();
        self.editors = kept;
        self.active = new_active;
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        removed
    }

    /// Close every tab whose index > `from_idx`, except pinned tabs, which
    /// always survive (VS Code "Close to the Right" never closes a pinned
    /// tab). The tab at `from_idx` stays active; tabs to the left are
    /// untouched. Returns the number of tabs removed. Matches VS Code's
    /// "Close to the Right".
    pub fn close_to_right(&mut self, from_idx: usize) -> usize {
        if from_idx >= self.editors.len() {
            return 0;
        }
        let before = self.editors.len();
        let old_active = self.active;
        let mut new_active: Option<usize> = None;
        let mut pivot_pos = 0;
        let mut kept: Vec<Editor> = Vec::with_capacity(before);
        for (i, ed) in std::mem::take(&mut self.editors).into_iter().enumerate() {
            if i <= from_idx || ed.pinned {
                if i == from_idx {
                    pivot_pos = kept.len();
                }
                if i == old_active {
                    new_active = Some(kept.len());
                }
                kept.push(ed);
            }
        }
        let removed = before - kept.len();
        self.editors = kept;
        // Keep the previously-active tab active when it survived; otherwise
        // fall back to the pivot tab's new position.
        self.active = new_active.unwrap_or(pivot_pos);
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        removed
    }

    /// Close every tab, resetting the editor pane to the single blank
    /// just-launched state — mirrors `close_tab` on the last remaining
    /// tab. Returns how many tabs were collapsed away (always ≥ 1 when
    /// the editor had any content). Matches VS Code's "Close All".
    pub fn close_all(&mut self) -> usize {
        let n = self.editors.len();
        let was_focused = self.editors[self.active].focused;
        let mut fresh = Editor::new();
        fresh.focused = was_focused;
        self.editors = vec![fresh];
        self.active = 0;
        n
    }

    /// Close every saved (non-dirty) tab, keeping any with unsaved changes.
    /// When no dirty tab remains the pane resets to a single blank buffer,
    /// mirroring `close_all`; otherwise the surviving dirty tabs are kept in
    /// order and the first becomes active. Returns how many tabs were removed
    /// (0 when every open tab is dirty). Matches VS Code's "Close Saved"
    /// (`workbench.action.closeUnmodifiedEditors`).
    pub fn close_saved(&mut self) -> usize {
        let before = self.editors.len();
        let was_focused = self.editors[self.active].focused;
        let mut kept: Vec<Editor> = std::mem::take(&mut self.editors)
            .into_iter()
            .filter(|e| e.dirty)
            .collect();
        let removed = before - kept.len();
        if kept.is_empty() {
            let mut fresh = Editor::new();
            fresh.focused = was_focused;
            kept.push(fresh);
        }
        self.editors = kept;
        self.active = 0;
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == 0;
        }
        removed
    }

    /// Map a mouse cell `(col, row)` to a tab index, or `None` if the click
    /// missed every tab. Uses the on-screen ranges captured during the most
    /// recent render.
    pub fn tab_at(&self, col: u16, row: u16) -> Option<usize> {
        if row != self.tab_strip_y {
            return None;
        }
        for (i, &(x, w)) in self.tab_screen_ranges.iter().enumerate() {
            if col >= x && col < x.saturating_add(w) {
                return Some(i);
            }
        }
        None
    }

    /// Full filesystem path for a tab, used as the hover tooltip that
    /// disambiguates same-named files (the tab label only shows the bare
    /// `file_name()`). Returns `None` for an unsaved "untitled" buffer
    /// since there is nothing to disambiguate. Diff tabs report both
    /// sides so the viewer can tell which revisions are being compared.
    pub fn tab_full_path(&self, idx: usize) -> Option<String> {
        let e = self.editors.get(idx)?;
        if let Some(diff) = e.diff.as_ref() {
            let l = diff.left_path.to_string_lossy().into_owned();
            let r_is_real = diff.right_path != Path::new("/dev/null")
                && !diff.right_path.as_os_str().is_empty();
            if r_is_real {
                let r = diff.right_path.to_string_lossy().into_owned();
                return Some(format!("{l} \u{2194} {r}"));
            }
            return Some(l);
        }
        e.path.as_ref().map(|p| p.to_string_lossy().into_owned())
    }

    pub fn tab_screen_x(&self, idx: usize) -> Option<(u16, u16)> {
        self.tab_screen_ranges.get(idx).copied()
    }

    pub fn close_screen_x(&self, idx: usize) -> Option<u16> {
        self.tab_close_x.get(idx).copied().filter(|&x| x != 0)
    }

    /// Map a mouse cell to the tab whose close `\u{2715}` glyph occupies it,
    /// or `None` if the click missed every close button. Used by the App's
    /// mouse handler to short-circuit ahead of `tab_at` so a click on the X
    /// closes the tab instead of selecting it.
    pub fn close_at(&self, col: u16, row: u16) -> Option<usize> {
        if row != self.tab_strip_y {
            return None;
        }
        self.tab_close_x.iter().position(|&x| x != 0 && x == col)
    }

    /// Index of the first tab whose `path` matches `target` either by
    /// literal equality or by canonicalised equality (so symlink + relative
    /// path aliases dedupe to the same tab). Returns `None` if no tab is
    /// currently holding that file.
    pub fn find_tab_with_path(&self, target: &Path) -> Option<usize> {
        self.find_tab_matching(target, |_| true)
    }

    /// If `path` is open in a tab, apply LSP rename `edits` to that buffer
    /// in-memory (one undo step, marked dirty) and return the count applied.
    /// `None` when no tab holds the file, so the caller can edit it on disk.
    pub fn apply_rename_to_open_tab(
        &mut self,
        path: &Path,
        edits: &[TextSpanEdit],
    ) -> Option<usize> {
        let idx = self.find_tab_with_path(path)?;
        Some(self.editors[idx].apply_span_edits(edits))
    }

    fn find_tab_matching(&self, target: &Path, extra: impl Fn(&Editor) -> bool) -> Option<usize> {
        let canon_target = target.canonicalize().ok();
        self.editors.iter().position(|e| {
            if !extra(e) {
                return false;
            }
            let Some(p) = e.path.as_ref() else {
                return false;
            };
            if p == target {
                return true;
            }
            match (canon_target.as_ref(), p.canonicalize().ok()) {
                (Some(a), Some(b)) => *a == b,
                _ => false,
            }
        })
    }

    pub fn preview_index(&self) -> Option<usize> {
        self.editors.iter().position(|e| e.preview)
    }

    /// Mark the active tab as pinned (no longer the preview slot). Called
    /// when the user double-clicks a file, hits Ctrl+Enter, or starts typing
    /// inside a preview tab.
    pub fn pin_active(&mut self) {
        self.editors[self.active].preview = false;
    }

    /// True when the tab at `idx` is the replaceable preview slot. Drives the
    /// tab menu's "Keep Open" entry, which only makes sense for a preview tab.
    pub fn is_preview(&self, idx: usize) -> bool {
        self.editors.get(idx).is_some_and(|e| e.preview)
    }

    /// VS Code "Keep Open": promote the tab at `idx` out of the preview slot
    /// so a subsequent single-click open no longer replaces it. Returns true
    /// when a preview tab was actually promoted (false if it was already
    /// permanent or the index is out of range).
    pub fn keep_open(&mut self, idx: usize) -> bool {
        match self.editors.get_mut(idx) {
            Some(e) if e.preview => {
                e.preview = false;
                true
            }
            _ => false,
        }
    }

    /// True when the tab at `idx` is pinned. Drives the tab menu's
    /// "Pin"/"Unpin" label and the close-cell glyph (pin vs `\u{2715}`).
    pub fn is_pinned(&self, idx: usize) -> bool {
        self.editors.get(idx).is_some_and(|e| e.pinned)
    }

    /// VS Code "Pin"/"Unpin" the tab at `idx`. Returns the tab's new pinned
    /// state. Pinning clears the preview flag (a pinned tab is never the
    /// replaceable preview slot) and reorders the strip so pinned tabs stay
    /// leftmost: a stable partition lands a newly-pinned tab at the end of the
    /// pinned block and a newly-unpinned tab at the front of the unpinned
    /// block, matching VS Code. The active tab follows its editor across the
    /// reorder.
    pub fn toggle_pin(&mut self, idx: usize) -> bool {
        if idx >= self.editors.len() {
            return false;
        }
        let now_pinned = !self.editors[idx].pinned;
        self.editors[idx].pinned = now_pinned;
        if now_pinned {
            self.editors[idx].preview = false;
        }
        // Stable partition: pinned first (keeping their relative order), then
        // unpinned (keeping theirs). `sort_by_key` is stable, so the two blocks
        // preserve order and only the toggled tab crosses the boundary.
        let n = self.editors.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| !self.editors[i].pinned);
        let new_active = order.iter().position(|&i| i == self.active).unwrap_or(0);
        let mut slots: Vec<Option<Editor>> = self.editors.drain(..).map(Some).collect();
        self.editors = order.iter().map(|&i| slots[i].take().unwrap()).collect();
        self.active = new_active;
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        now_pinned
    }

    /// Remove and return the active editor (for a Move to another group). When
    /// it was the only tab the group falls back to a single blank tab (like
    /// closing the last tab), so the group stays renderable; the caller can
    /// detect that empty state via [`Self::is_blank_initial`] and prune it.
    pub fn take_active_editor(&mut self) -> Editor {
        let ed = self.editors.remove(self.active);
        if self.editors.is_empty() {
            self.editors.push(Editor::new());
            self.active = 0;
        } else if self.active >= self.editors.len() {
            self.active = self.editors.len() - 1;
        }
        for (i, e) in self.editors.iter_mut().enumerate() {
            e.focused = i == self.active;
        }
        ed
    }

    /// Add `ed` (moved from another group) and make it active. A blank-initial
    /// group is replaced in place so a Move never leaves a stray blank tab
    /// beside the moved editor.
    pub fn push_editor(&mut self, mut ed: Editor) {
        if self.is_blank_initial() {
            ed.focused = true;
            self.editors[0] = ed;
            self.active = 0;
            return;
        }
        self.editors.push(ed);
        self.active = self.editors.len() - 1;
        for (i, e) in self.editors.iter_mut().enumerate() {
            e.focused = i == self.active;
        }
    }

    /// VS Code "preview tab" semantics: open `path` in the single
    /// replaceable preview slot. If the file is already in some tab, just
    /// switch to it. Otherwise reuse the existing preview slot, or create a
    /// fresh preview tab next to the active one when none exists.
    pub fn open_preview(&mut self, path: &Path) -> Result<()> {
        if let Some(idx) = self.find_tab_with_path(path) {
            // Switching to an already-open file: if a stale preview tab
            // exists for some OTHER file, drop it so the user doesn't
            // accumulate ghost tabs from quick "peek" navigations.
            if let Some(prev) = self.preview_index()
                && prev != idx
            {
                self.editors.remove(prev);
                let new_idx = if idx > prev { idx - 1 } else { idx };
                // Bypass `select` here because the removal may have left
                // `self.active` pointing past the end (when the active
                // tab was the preview we just dropped).
                self.active = new_idx;
                for (i, ed) in self.editors.iter_mut().enumerate() {
                    ed.focused = i == new_idx;
                }
                return Ok(());
            }
            self.select(idx);
            return Ok(());
        }
        if let Some(idx) = self.preview_index() {
            self.select(idx);
            self.editors[idx].open(path)?;
            self.editors[idx].preview = true;
            return Ok(());
        }
        if self.is_blank_initial() {
            let active = self.active;
            self.editors[active].open(path)?;
            self.editors[active].preview = true;
            self.editors[active].search_highlight = self.search_highlight_term.clone();
            self.editors[active].search_highlight_opts = self.search_highlight_opts;
            return Ok(());
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.open(path)?;
        e.preview = true;
        e.search_highlight = self.search_highlight_term.clone();
        e.search_highlight_opts = self.search_highlight_opts;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Open a side-by-side diff of two files in a fresh pinned tab next
    /// to the active one. The new tab is read-only: edits, save, and the
    /// text-rendering path are all bypassed via the `diff: Some(...)` flag
    /// on the underlying Editor.
    pub fn open_diff(&mut self, left: &Path, right: &Path) -> Result<()> {
        let left_text =
            std::fs::read_to_string(left).with_context(|| format!("reading {}", left.display()))?;
        let right_text = std::fs::read_to_string(right)
            .with_context(|| format!("reading {}", right.display()))?;
        let left_lines: Vec<String> = left_text.lines().map(str::to_string).collect();
        let right_lines: Vec<String> = right_text.lines().map(str::to_string).collect();
        let mut data = crate::widgets::diff::DiffData::build(
            left.to_path_buf(),
            right.to_path_buf(),
            left_lines,
            right_lines,
        );
        data.left_is_real_file = true;
        data.set_whitespace_mode(self.diff_ws_default);

        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        // The path is set so close-by-path lookups work; `diff` being Some
        // is what diverts the renderer onto the side-by-side path.
        e.path = Some(left.to_path_buf());
        e.diff = Some(data);
        // Reuse the blank-initial slot if the editor pane was empty;
        // otherwise insert a new tab next to the active one.
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Open a side-by-side diff between an in-memory `left_text` (e.g.
    /// the HEAD version of a file) and the working-tree file at `right`.
    /// The left "path" is purely a label; nothing reads from it on disk.
    /// Used by the Source Control panel to show working-tree-vs-HEAD
    /// diffs when the user clicks a Modified entry.
    pub fn open_head_diff_with_text(
        &mut self,
        left_label: PathBuf,
        left_text: &str,
        right: &Path,
        left_is_git_head: bool,
    ) -> Result<()> {
        let right_text = std::fs::read_to_string(right)
            .with_context(|| format!("reading {}", right.display()))?;
        let left_lines: Vec<String> = left_text.lines().map(str::to_string).collect();
        let right_lines: Vec<String> = right_text.lines().map(str::to_string).collect();
        let mut data = crate::widgets::diff::DiffData::build_with_byte_check(
            left_label,
            right.to_path_buf(),
            left_lines,
            right_lines,
            Some(left_text),
            Some(&right_text),
        );
        data.set_whitespace_mode(self.diff_ws_default);
        // Park the viewport on the first change hunk so the user lands on
        // the first edit instead of reading through unchanged leading
        // lines. Identical files stay at scroll 0.
        if let Some(row) = data.first_change_row() {
            data.scroll_to_row(row);
        }
        data.left_is_git_head = left_is_git_head;
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(right.to_path_buf());
        e.diff = Some(data);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        // If a tab is already showing this path (as a plain file or a
        // diff), reuse it so we don't pile up duplicate tabs as the
        // user clicks through the change list — unless it holds unsaved
        // edits, which replacing wholesale would silently destroy.
        if let Some(idx) = self.find_tab_with_path(right)
            && !self.editors[idx].dirty
        {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Open a single-pane unified diff that paints every line of
    /// `head_text` as a removed line. `path` is the workspace-relative
    /// path that no longer exists on disk; it doubles as the display
    /// label in the diff header and as the tab dedup key so repeated
    /// clicks on the same Source-Control row reuse the tab rather than
    /// stacking new ones.
    pub fn open_deleted_diff_with_text(&mut self, path: &Path, head_text: &str) -> Result<()> {
        let mut data =
            crate::widgets::diff::DiffData::build_unified_deletion(path.to_path_buf(), head_text);
        data.set_whitespace_mode(self.diff_ws_default);
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(path.to_path_buf());
        e.diff = Some(data);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        // Same dirty guard as `open_head_diff_with_text`: a deleted-on-disk
        // file can still hold unsaved edits in its tab.
        if let Some(idx) = self.find_tab_with_path(path)
            && !self.editors[idx].dirty
        {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Open a side-by-side view of raw `git diff` text (e.g. the stdout
    /// of `git diff --staged` or `git diff <branch>`). `label` is a
    /// synthetic path used as the tab title and as the dedup key so a
    /// repeat invocation reuses the existing tab instead of stacking new
    /// ones. The text is parsed into separate left/right streams so the
    /// standard two-column renderer takes over — every `+`/`-` pair in a
    /// hunk lines up horizontally instead of zigzagging vertically.
    pub fn open_git_diff_side_by_side(&mut self, label: &Path, raw_diff: &str) -> Result<()> {
        let mut data = crate::widgets::diff::DiffData::build_side_by_side_from_git_text(
            label.to_path_buf(),
            raw_diff,
        );
        data.set_whitespace_mode(self.diff_ws_default);
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(label.to_path_buf());
        e.diff = Some(data);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        if let Some(idx) = self.find_tab_with_path(label) {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Open arbitrary text in a scratch tab labelled `label` (no file on
    /// disk). Used by "Show Git Output" to surface the git command log. The
    /// tab has no `disk_stamp`, so the FS-sync layer never tries to reload
    /// or overwrite it.
    pub fn open_text_buffer(&mut self, label: &Path, text: &str) -> Result<()> {
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(label.to_path_buf());
        e.lines = split_into_lines(text);
        if e.lines.is_empty() {
            e.lines.push(String::new());
        }
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        if let Some(idx) = self.find_tab_with_path(label) {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Pinned-tab open: if the file is already in some tab, pin and switch
    /// to it. Otherwise create a fresh pinned tab next to the active one.
    /// Used by double-click in the explorer and Ctrl+Enter on a tree row.
    pub fn open_pinned(&mut self, path: &Path) -> Result<()> {
        // A diff tab can carry the working file's `path` yet is not an
        // editable view of it, so skip diffs here: Enter on a diff (or a
        // double-click) opens the real file beside the diff rather than just
        // re-selecting the diff tab.
        if let Some(idx) = self.find_tab_matching(path, |e| e.diff.is_none()) {
            self.editors[idx].preview = false;
            self.select(idx);
            return Ok(());
        }
        if self.is_blank_initial() {
            let active = self.active;
            self.editors[active].open(path)?;
            self.editors[active].preview = false;
            self.editors[active].search_highlight = self.search_highlight_term.clone();
            self.editors[active].search_highlight_opts = self.search_highlight_opts;
            return Ok(());
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.open(path)?;
        e.preview = false;
        e.search_highlight = self.search_highlight_term.clone();
        e.search_highlight_opts = self.search_highlight_opts;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// True iff the editor is in its just-launched state: a single tab with
    /// no file loaded and no edits. `App::render` uses this to swap the
    /// editor pane for the welcome screen, and `open_preview` /
    /// `open_pinned` use it to reuse the placeholder tab rather than stack
    /// a new one on top.
    pub fn is_blank_initial(&self) -> bool {
        self.editors.len() == 1
            && self.editors[0].path.is_none()
            && !self.editors[0].dirty
            && self.editors[0].lines.iter().all(|l| l.is_empty())
    }

    /// Test-only helper that mirrors `open_preview` without going through
    /// the disk: insert a preview tab whose path is set but whose buffer is
    /// empty. Used by unit tests to exercise preview-slot bookkeeping in
    /// isolation from filesystem I/O.
    pub fn add_preview_tab_with_path(&mut self, path: PathBuf) {
        let mut e = Editor::new();
        e.path = Some(path);
        e.preview = true;
        e.focused = self.editors[self.active].focused;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Test-only helper for the "single-click on a different file when a
    /// preview tab already exists" path: rewrite the existing preview tab's
    /// path without changing tab count.
    pub fn repoint_preview_to(&mut self, path: PathBuf) {
        if let Some(idx) = self.preview_index() {
            self.editors[idx].path = Some(path);
        }
    }
}

impl Default for EditorTabs {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for EditorTabs {
    type Target = Editor;
    fn deref(&self) -> &Editor {
        &self.editors[self.active]
    }
}

impl DerefMut for EditorTabs {
    fn deref_mut(&mut self) -> &mut Editor {
        &mut self.editors[self.active]
    }
}

// Tab-strip chrome (strip, tab bodies, hover lift, close pill) is per-theme
// data: `Theme::tab_*` carries the two built-ins' historical constants and
// derives colors for every manifest theme, so the strip follows the selected
// theme instead of staying VS-Code navy. Only the label foregrounds stay
// fixed: this muted grey and white are legible on all dark themes.
const TAB_INACTIVE_FG: Color = Color::Rgb(0x9d, 0xa5, 0xb4);
const TAB_ACTIVE_FG: Color = Color::White;

impl Widget for &mut EditorTabs {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_full_area = area;
        if area.height == 0 || area.width == 0 {
            // Ctrl+Shift+J maximises the terminal pane, which collapses
            // the editor area to height 0. The inner editor's `render`
            // is never reached on this branch, so its `last_area` would
            // otherwise keep the rectangle from the pre-maximise frame
            // and `App::handle_mouse`'s `in_editor` hit-test
            // (rect_contains(self.editor.last_area, ...)) would still
            // win against the terminal in the dispatch chain at
            // app.rs:7229 / 7336 — clicks meant to begin a terminal
            // selection get routed to the editor's mouse_down and the
            // file caret jumps instead. Zero the inner editor's
            // hit-test rectangle here so the terminal pane wins
            // unambiguously while the editor is collapsed.
            for ed in self.editors.iter_mut() {
                ed.last_area = Rect {
                    x: area.x,
                    y: area.y,
                    width: area.width,
                    height: 0,
                };
                ed.last_inner = ed.last_area;
                ed.last_scrollbar = Rect::default();
            }
            return;
        }
        let strip_h: u16 = 1;
        let strip = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: strip_h,
        };
        let mut body = Rect {
            x: area.x,
            y: area.y + strip_h,
            width: area.width,
            height: area.height - strip_h,
        };

        // The tab chrome follows the theme. Like the brand flag below, the
        // theme propagates from a synced sibling: a tab opened since the last
        // sync still carries the default theme and must not flash the wrong
        // chrome for a frame.
        let theme = self
            .editors
            .iter()
            .map(|e| e.theme)
            .find(|t| *t != crate::theme::Theme::default())
            .unwrap_or_default();
        // Paint strip background first so the gap to the right of the last
        // tab still reads as the tab-strip colour rather than terminal default.
        let strip_bg_style = Style::default().bg(theme.tab_strip_bg());
        for x in strip.x..strip.x + strip.width {
            buf[(x, strip.y)].set_style(strip_bg_style);
            buf[(x, strip.y)].set_symbol(" ");
        }

        self.tab_strip_y = strip.y;
        self.tab_screen_ranges.clear();
        self.tab_close_x.clear();
        let mut cursor_x = strip.x;
        let active = self.active;
        // Black theme (`focus_gradient` doubles as the theme flag): the
        // active tab wears the muted teal selection fill instead of the
        // legacy navy chip; Croft Dark keeps the navy. Sync pushes the flag
        // to every editor, but one freshly opened since the last sync still
        // defaults to false — propagate from its siblings so it can't flash
        // the navy chip (or a blue body border) for a frame.
        let brand = self.editors.iter().any(|e| e.focus_gradient);
        if brand {
            for ed in self.editors.iter_mut() {
                ed.focus_gradient = true;
            }
        }
        let active_tab_bg = theme.tab_active_bg();
        let pointer = self.hover_pointer;
        let display_labels = disambiguated_tab_labels(&self.editors);
        for (i, ed) in self.editors.iter().enumerate() {
            let label_text = display_labels[i].clone();
            // Display CELLS, not chars (#168 review): a CJK file or
            // directory name is double-width, and a char count shifted
            // the close button and hit ranges left of the painted text.
            let label_chars = Span::raw(label_text.as_str()).width() as u16;
            let pad: u16 = 1;
            let close_pad: u16 = 2;
            let width = label_chars
                .saturating_add(pad * 2)
                .saturating_add(close_pad);
            if cursor_x.saturating_add(width) > strip.x + strip.width {
                self.tab_screen_ranges.push((cursor_x, 0));
                self.tab_close_x.push(0);
                continue;
            }
            let is_active = i == active;
            let close_x = cursor_x + 1 + label_chars + 1;
            // Treatment C: the pointer resting anywhere on a tab lifts an
            // inactive tab's body (the active tab is already prominent), and
            // resting on the close cross gives that single cell a pill.
            let tab_hovered = pointer.is_some_and(|(px, py)| {
                py == strip.y && px >= cursor_x && px < cursor_x.saturating_add(width)
            });
            let on_close = pointer == Some((close_x, strip.y));
            let hover_lift = tab_hovered && !is_active;
            let bg = if hover_lift {
                theme.tab_hover_bg()
            } else if is_active {
                active_tab_bg
            } else {
                theme.tab_inactive_bg()
            };
            let fg = if is_active || hover_lift {
                TAB_ACTIVE_FG
            } else {
                TAB_INACTIVE_FG
            };
            let mut modifiers = Modifier::empty();
            if is_active {
                modifiers |= Modifier::BOLD;
            }
            if ed.preview {
                modifiers |= Modifier::ITALIC;
            }
            let style = Style::default().fg(fg).bg(bg).add_modifier(modifiers);
            // The close cell shows a thumb-tack on a pinned tab (clicking it
            // unpins) and the close cross otherwise (clicking it closes).
            let close_glyph = if ed.pinned { "\u{f08d}" } else { "\u{2715}" };
            // Layout: " " + label + " " + ✕/pin + " "
            let padded = format!(" {label_text} {close_glyph} ");
            buf.set_string(cursor_x, strip.y, &padded, style);
            // Overpaint the close/pin cell with its hover pill so the user sees
            // exactly which glyph their click will land on.
            if on_close {
                let pill_bg = theme.tab_close_pill_bg();
                buf.set_string(
                    close_x,
                    strip.y,
                    close_glyph,
                    Style::default()
                        .fg(Color::White)
                        .bg(pill_bg)
                        .add_modifier(Modifier::BOLD),
                );
            }
            self.tab_screen_ranges.push((cursor_x, width));
            self.tab_close_x.push(close_x);
            cursor_x = cursor_x.saturating_add(width);
        }

        // Breadcrumb bar (VS Code editor breadcrumbs): one row below the tab
        // strip showing the file path + enclosing symbol chain. Carve it off
        // the body so the editor's own coordinate math shrinks with it.
        self.breadcrumb_ranges.clear();
        self.breadcrumb_y = None;
        if !self.breadcrumbs.is_empty() && body.height > 1 {
            let crumb_y = body.y;
            self.breadcrumb_y = Some(crumb_y);
            let theme = self.editors[active].theme;
            let bg = theme.editor_bg();
            for x in area.x..area.x + area.width {
                buf[(x, crumb_y)].set_symbol(" ");
                buf[(x, crumb_y)].set_style(Style::default().bg(bg));
            }
            let max_x = area.x + area.width;
            let mut x = area.x + 1;
            for (i, crumb) in self.breadcrumbs.iter().enumerate() {
                if i > 0 {
                    let sep = " \u{203A} "; // " › "
                    let sw = sep.chars().count() as u16;
                    if x + sw >= max_x {
                        break;
                    }
                    buf.set_string(x, crumb_y, sep, Style::default().fg(Color::DarkGray).bg(bg));
                    x += sw;
                }
                if x >= max_x {
                    break;
                }
                let avail = (max_x - x) as usize;
                // Symbol crumbs (clickable) read brighter than the informational
                // path crumbs, mirroring VS Code's active/inactive breadcrumbs.
                let fg = if crumb.target.is_some() {
                    self.theme.ui(Color::Gray)
                } else {
                    Color::DarkGray
                };
                // Budget and advance in display CELLS: `set_stringn` clips by
                // grapheme width and reports where it stopped, so a CJK or
                // emoji crumb neither overruns the bar nor gets painted over by
                // the next crumb, and its hit rect covers what was drawn.
                let (end_x, _) = buf.set_stringn(
                    x,
                    crumb_y,
                    &crumb.label,
                    avail,
                    Style::default().fg(fg).bg(bg),
                );
                let sw = end_x.saturating_sub(x);
                self.breadcrumb_ranges.push((x, sw, crumb.target));
                x = end_x;
            }
            body = Rect {
                y: body.y + 1,
                height: body.height - 1,
                ..body
            };
        }

        let active_editor = &mut self.editors[active];
        Widget::render(active_editor, body, buf);
    }
}

impl EditorTabs {
    /// If `(col, row)` lands on a breadcrumb crumb with a jump target, return
    /// the caret position to navigate to. Path/file crumbs return `None`.
    pub fn breadcrumb_target_at(&self, col: u16, row: u16) -> Option<(u32, u32)> {
        if self.breadcrumb_y != Some(row) {
            return None;
        }
        self.breadcrumb_ranges
            .iter()
            .find(|&&(x, w, _)| col >= x && col < x + w)
            .and_then(|&(_, _, target)| target)
    }
}

/// Tab titles for a strip, with VS Code's labelFormat disambiguation
/// (#167): colliding file names gain the SHORTEST distinguishing
/// trailing directory context (`main.rs — alpha` beside
/// `main.rs — beta`; deeper only when the parents collide too), and
/// unique titles stay bare. Diff/preview tabs keep their own labels.
pub(crate) fn disambiguated_tab_labels(editors: &[Editor]) -> Vec<String> {
    let base: Vec<String> = editors.iter().map(tab_label).collect();
    let mut out = base.clone();
    // Group by FILE NAME, not the rendered label: decorations (the dirty
    // dot) are part of the base string, and keying on it let a dirty tab
    // escape its collision group.
    let mut groups: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, e) in editors.iter().enumerate() {
        if e.diff.is_none()
            && let Some(name) = e.path.as_deref().and_then(|p| p.file_name())
        {
            groups
                .entry(name.to_string_lossy().into_owned())
                .or_default()
                .push(i);
        }
    }
    for idxs in groups.into_values() {
        if idxs.len() < 2 {
            continue;
        }
        // Walk as deep as the longest parent in the group: two DISTINCT
        // paths sharing a filename must differ somewhere in their
        // parents, so the full-parent suffixes are always distinct — a
        // fixed cap could leave deep-shared tails identical (#168
        // review).
        let max_depth = idxs
            .iter()
            .filter_map(|&i| editors[i].path.as_deref())
            .filter_map(|p| p.parent())
            .map(|p| p.components().count())
            .max()
            .unwrap_or(1)
            .max(1);
        for depth in 1..=max_depth {
            let sufs: Vec<String> = idxs
                .iter()
                .map(|&i| {
                    dir_suffix(
                        editors[i]
                            .path
                            .as_deref()
                            .unwrap_or(std::path::Path::new("")),
                        depth,
                    )
                })
                .collect();
            let mut seen = std::collections::HashSet::new();
            let all_unique = sufs.iter().all(|s| seen.insert(s.clone()));
            if all_unique || depth == max_depth {
                for (k, &i) in idxs.iter().enumerate() {
                    if !sufs[k].is_empty() {
                        out[i] = format!("{} — {}", base[i], sufs[k]);
                    }
                }
                break;
            }
        }
    }
    out
}

/// The last `depth` directory components of `path`'s parent, joined
/// with `/` — the trailing context the disambiguated label shows.
fn dir_suffix(path: &std::path::Path, depth: usize) -> String {
    let Some(parent) = path.parent() else {
        return String::new();
    };
    let comps: Vec<String> = parent
        .components()
        .rev()
        .take(depth)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    comps.into_iter().rev().collect::<Vec<_>>().join("/")
}

/// Classify a line as a fold-region marker (#254): after leading
/// whitespace and a comment introducer (`//`, `#`, `--`, `;`, `/*`, with
/// an optional extra `#` as in `// #region`), the word `region`
/// (Some(true)) or `endregion` (Some(false)) at a word boundary — the
/// language-agnostic `#region` family VS Code folds.
fn region_marker(line: &str) -> Option<bool> {
    let trimmed = line.trim_start();
    let mut rest = None;
    for intro in ["//", "#", "--", ";", "/*"] {
        if let Some(r) = trimmed.strip_prefix(intro) {
            rest = Some(r);
            break;
        }
    }
    let s = rest?.trim_start();
    let s = s.strip_prefix('#').unwrap_or(s);
    let boundary = |r: &str| !r.starts_with(|c: char| c.is_alphanumeric() || c == '_');
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("endregion") {
        return boundary(&s[9..]).then_some(false);
    }
    if lower.starts_with("region") {
        return boundary(&s[6..]).then_some(true);
    }
    None
}

/// Whether a line is a full-line comment for the fallback comment-run
/// scanner. Rust/C attributes (`#[...]`, `#![...]`) are code, not
/// comments, despite the leading `#`.
fn comment_line(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("#[") || t.starts_with("#![") {
        return false;
    }
    ["//", "#", "--", ";", "/*", "*/", "* "]
        .iter()
        .any(|intro| t.starts_with(intro))
        || t == "*"
}

fn tab_label(e: &Editor) -> String {
    if let Some(diff) = e.diff.as_ref() {
        let l = diff
            .left_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| diff.left_path.to_string_lossy().into_owned());
        // Synthetic single-sided diffs (deletion view, raw `git diff`
        // text view) leave `right_path` empty / `/dev/null` so the tab
        // label collapses to just the left label instead of trailing a
        // misleading "↔ null".
        let r_is_real = diff.right_path != std::path::Path::new("/dev/null")
            && !diff.right_path.as_os_str().is_empty();
        if r_is_real {
            let r = diff
                .right_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| diff.right_path.to_string_lossy().into_owned());
            return format!("{l} \u{2194} {r}");
        }
        return l;
    }
    let name = match &e.path {
        Some(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("untitled")),
        None => String::from("untitled"),
    };
    // The merge editor is text underneath (dirty dot still applies), but
    // the tab says which flavour of the file it is showing.
    let name = if e.merge.is_some() {
        format!("{name} (merge)")
    } else {
        name
    };
    if e.dirty {
        format!("\u{25cf} {name}")
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn blame_line(
        author: &str,
        summary: &str,
        age: i64,
        uncommitted: bool,
    ) -> crate::git::BlameLine {
        crate::git::BlameLine {
            short_hash: "abc12345".into(),
            summary: summary.into(),
            author: author.into(),
            age_secs: age,
            uncommitted,
        }
    }

    #[test]
    fn snippet_expansion_places_caret_and_tab_advances_with_drift() {
        let mut e = editor_with("");
        e.insert_str("for");
        e.expand_snippet("for ${1:i} in ${2:xs}:", 3);
        // Prefix removed, body inserted, caret on the first placeholder with it
        // selected so the next keystroke replaces it.
        assert_eq!(e.lines[0], "for i in xs:");
        assert_eq!((e.cursor_row, e.cursor_col), (0, 5));
        assert!(e.snippet_active());
        assert_eq!(e.selection_text(), "i");
        // Replacing the 1-char placeholder with a 3-char name shifts $2 right.
        e.insert_str("idx");
        assert_eq!(e.lines[0], "for idx in xs:");
        assert!(e.snippet_next());
        assert_eq!(
            e.selection_text(),
            "xs",
            "$2 must track the drift from the longer $1"
        );
        // The last stop ends the session.
        assert!(!e.snippet_active());
    }

    /// On-type formatting (#254) keys off real keystrokes only: the typed
    /// paths (`insert_char`, auto-pairs included, and `insert_newline`)
    /// record the trigger, while paste/snippet insertion through
    /// `insert_str` must not — VS Code never fires formatOnType on paste.
    #[test]
    fn typing_records_last_typed_but_paste_does_not() {
        let mut e = editor_with("");
        e.insert_char(';');
        assert_eq!(e.last_typed, Some((';', e.edit_seq)));
        e.insert_newline();
        assert_eq!(e.last_typed, Some(('\n', e.edit_seq)));
        e.auto_close_pairs = true;
        e.insert_char('(');
        assert_eq!(
            e.last_typed,
            Some(('(', e.edit_seq)),
            "an auto-paired opener is still a keystroke"
        );
        // Typing the closer over the auto-inserted one moves the caret but
        // changes no content, so the seq deliberately stays put: `edit_seq`
        // means "buffer content unchanged", and an in-flight formatting
        // reply computed against byte-identical text is still safe to apply.
        let seq_before_typeover = e.edit_seq;
        e.insert_char(')');
        assert_eq!(
            e.last_typed,
            Some((')', seq_before_typeover)),
            "a type-over records the keystroke against the unchanged seq"
        );
        assert_eq!(
            e.edit_seq, seq_before_typeover,
            "caret-only motion must not masquerade as a buffer edit"
        );
        let before = e.last_typed;
        e.insert_str("let x = 1;");
        assert_eq!(e.last_typed, before, "paste must not count as typing");
        e.multi_insert_char(';');
        assert_eq!(
            e.last_typed, before,
            "multi-cursor typing must not arm the trigger: one reply cannot \
             be right at every caret"
        );
        assert_ne!(
            e.last_typed.unwrap().1,
            e.edit_seq,
            "and the stale record can no longer match the buffer seq"
        );
    }

    /// UTF-16 mapping for LSP positions: characters count by UTF-16 code
    /// units, and an empty buffer (a fresh scratch tab before any edit)
    /// answers the origin instead of panicking.
    #[test]
    fn pos_to_utf16_counts_utf16_units_and_survives_an_empty_buffer() {
        let e = editor_with("日本🙂x");
        assert_eq!(e.pos_to_utf16(0, 0), (0, 0));
        assert_eq!(e.pos_to_utf16(0, 2), (0, 2));
        assert_eq!(e.pos_to_utf16(0, 3), (0, 4), "🙂 is a surrogate pair");
        assert_eq!(e.pos_to_utf16(0, 4), (0, 5));
        assert_eq!(e.pos_to_utf16(9, 99), (0, 5), "past-the-end clamps");
        let mut empty = Editor::new();
        empty.lines.clear();
        assert_eq!(empty.pos_to_utf16(0, 0), (0, 0));
    }

    /// #288: every position-carrying LSP request sends UTF-16 columns and every
    /// server answer comes back in them, so the two conversions must be exact
    /// inverses. A char column used raw as UTF-16 (or the reverse) lands the
    /// caret to the LEFT of the symbol on any line with a surrogate pair, which
    /// is what made hover/definition/completion miss on emoji lines.
    #[test]
    fn utf16_and_char_columns_round_trip_across_surrogate_pairs() {
        let e = editor_with("let 🙂 = 🙂🙂;");
        let chars = e.lines[0].chars().count();
        for col in 0..=chars {
            let (_, u16col) = e.pos_to_utf16(0, col);
            assert_eq!(
                e.utf16_col_to_char_pub(0, u16col),
                col,
                "char col {col} must survive the round trip"
            );
        }
        // The divergence the bug rode on: past three surrogate pairs the UTF-16
        // column runs three units ahead of the character column.
        let (_, after_three) = e.pos_to_utf16(0, 10);
        assert_eq!(after_three, 13, "each pair costs one extra unit");
        assert_eq!(
            e.utf16_col_to_char_pub(0, 10),
            9,
            "reading a UTF-16 column as a char column would slip two to the left"
        );
    }

    /// A failed revert must not launder the buffer clean: clearing `dirty`
    /// before the fallible reload meant a file deleted between the conflict
    /// popup and the Enter left unsaved edits flagged clean — the tab lost
    /// its unsaved marker, auto-save ignored it, and the next FS sweep
    /// silently auto-reverted the edits when the file reappeared.
    #[test]
    fn a_failed_revert_keeps_the_buffer_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("gone.txt");
        std::fs::write(&path, "disk\n").unwrap();
        let mut e = Editor::new();
        e.open(&path).unwrap();
        e.insert_char('x');
        assert!(e.dirty, "staging: unsaved edits");
        std::fs::remove_file(&path).unwrap();
        assert!(e.revert_to_disk().is_err(), "staging: the reload fails");
        assert!(
            e.dirty,
            "a failed reload must keep the unsaved edits marked dirty"
        );
    }

    /// A snippet stop can outlive its row: rows deleted mid-session (the
    /// backspace join) leave later stops pointing past the end of the
    /// buffer, and Tab then planted a selection on the vanished row — the
    /// next edit indexed `lines[row]` out of bounds and panicked the TUI.
    #[test]
    fn a_snippet_stop_on_a_deleted_row_cannot_plant_a_selection_out_of_bounds() {
        let mut e = editor_with("");
        e.insert_str("try");
        e.expand_snippet(
            "try:\n    ${1:pass}\nexcept ${2:Exception}:\n    ${3:raise}",
            3,
        );
        assert!(e.snippet_active());
        assert_eq!(e.selection_text(), "pass");
        // Delete the placeholder, its indent, and the row join — the buffer
        // shrinks below stop $3's recorded row, and backspace keeps the
        // session alive by design.
        for _ in 0..6 {
            e.backspace();
        }
        assert!(e.lines.len() < 4, "staging: a row was joined away");
        e.snippet_next();
        e.snippet_next();
        // The next edit must not panic on a selection past the buffer end.
        e.insert_char('x');
    }

    /// A reused tab (the preview tab navigating file to file) must drop its
    /// collab attachment when it loads a DIFFERENT path: the old generation
    /// belongs to another file's doc, and keeping it lets the next collab
    /// tick broadcast this file's disk text as edits to that doc. A
    /// same-path reload keeps the attachment on purpose — that is how an
    /// owner's reload-diff (external change, Replace All) converges as ops.
    #[test]
    fn open_resets_collab_attachment_only_on_a_path_change() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        std::fs::write(&a, "aaa").unwrap();
        std::fs::write(&b, "bbb").unwrap();
        let mut e = Editor::new();
        e.open(&a).unwrap();
        e.collab_doc_gen = 7;
        e.collab_synced_seq = 3;
        e.open(&b).unwrap();
        assert_eq!(
            e.collab_doc_gen, 0,
            "opening another path must detach the buffer from the old doc"
        );
        assert_eq!(
            e.collab_synced_seq, e.edit_seq,
            "a fresh open has nothing pending to extract"
        );
        e.collab_doc_gen = 7;
        e.open(&b).unwrap();
        assert_eq!(
            e.collab_doc_gen, 7,
            "a same-path reload must keep its attachment so the reload broadcasts as edits"
        );
    }

    #[test]
    fn snippet_indents_continuation_lines_to_the_caret() {
        let mut e = editor_with("    ");
        e.cursor_row = 0;
        e.cursor_col = 4;
        e.expand_snippet("if x:\n    $0", 0);
        assert_eq!(e.lines[0], "    if x:");
        assert_eq!(
            e.lines[1], "        ",
            "continuation line keeps the 4-space block indent plus its own"
        );
    }

    #[test]
    fn current_line_blame_annotation_reads_author_age_and_summary() {
        let mut e = editor_with("one\ntwo\n");
        e.path = Some(PathBuf::from("f.rs"));
        e.set_blame(
            PathBuf::from("f.rs"),
            Some(vec![
                blame_line("Vitali", "fix: the thing", 90, false),
                blame_line("Alice", "feat: two", 7200, false),
            ]),
        );
        e.cursor_row = 0;
        assert_eq!(
            e.current_line_blame_annotation().as_deref(),
            Some("Vitali, 1 minute ago • fix: the thing")
        );
        e.cursor_row = 1;
        assert_eq!(
            e.current_line_blame_annotation().as_deref(),
            Some("Alice, 2 hours ago • feat: two")
        );
    }

    #[test]
    fn current_line_blame_annotation_marks_uncommitted_lines() {
        let mut e = editor_with("edited\n");
        e.path = Some(PathBuf::from("f.rs"));
        e.set_blame(
            PathBuf::from("f.rs"),
            Some(vec![blame_line("Not Committed Yet", "x", 0, true)]),
        );
        e.cursor_row = 0;
        assert_eq!(
            e.current_line_blame_annotation().as_deref(),
            Some("Uncommitted changes")
        );
    }

    #[test]
    fn current_line_blame_annotation_is_none_when_disabled_or_unavailable() {
        let mut e = editor_with("one\n");
        e.path = Some(PathBuf::from("f.rs"));
        // No blame fetched yet.
        assert!(e.current_line_blame_annotation().is_none());
        e.set_blame(
            PathBuf::from("f.rs"),
            Some(vec![blame_line("V", "s", 5, false)]),
        );
        e.blame_enabled = false;
        assert!(
            e.current_line_blame_annotation().is_none(),
            "the toggle suppresses the annotation without dropping the data"
        );
        // A cursor past the blamed range (line added after the fetch) is safe.
        e.blame_enabled = true;
        e.cursor_row = 9;
        assert!(e.current_line_blame_annotation().is_none());
    }

    /// A blame reply fetched for one file must never paint on another: the
    /// preview tab is reused across files, and the async refetch takes frames
    /// to land, so a same-line-count neighbour would wear the old blame.
    #[test]
    fn blame_for_another_file_never_paints_on_this_one() {
        let mut e = editor_with("one\ntwo\n");
        e.path = Some(PathBuf::from("a.rs"));
        e.set_blame(
            PathBuf::from("a.rs"),
            Some(vec![
                blame_line("Alice", "s", 5, false),
                blame_line("Alice", "s", 5, false),
            ]),
        );
        e.path = Some(PathBuf::from("b.rs"));
        assert!(
            e.current_line_blame_annotation().is_none(),
            "a.rs blame painted on b.rs while its own fetch was still in flight"
        );
    }

    /// #258: every diff opener must apply the configured default, not just
    /// the HEAD-vs-working one. `open_diff` backs the Compare actions, and
    /// missing the setter there meant Compare always started in `Off`
    /// regardless of config (caught in review on #292).
    #[test]
    fn every_diff_opener_applies_the_configured_whitespace_default() {
        use crate::widgets::diff::DiffWhitespace;
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        std::fs::write(&a, "x = 1;\n").unwrap();
        std::fs::write(&b, "    x = 1;\n").unwrap();

        // The openers live on EditorTabs, which derefs to the active tab, so
        // the default is read from the tab the app syncs it onto.
        let mut tabs = EditorTabs::new();
        tabs.editors[tabs.active].diff_ws_default = DiffWhitespace::Leading;

        // Compare two explorer files.
        tabs.open_diff(&a, &b).unwrap();
        assert_eq!(
            tabs.diff.as_ref().unwrap().ws_mode,
            DiffWhitespace::Leading,
            "open_diff (the Compare actions) must honour the default"
        );

        // HEAD-vs-working, the other user-facing entry point.
        let mut t2 = EditorTabs::new();
        t2.editors[t2.active].diff_ws_default = DiffWhitespace::All;
        t2.open_head_diff_with_text(a.clone(), "x = 1;\n", &b, true)
            .unwrap();
        assert_eq!(
            t2.diff.as_ref().unwrap().ws_mode,
            DiffWhitespace::All,
            "the HEAD diff must honour it too"
        );
    }

    /// Opening a real file into an editor showing a diff must drop the diff
    /// view, or a restore-then-reload keeps rendering the stale side-by-side.
    #[test]
    fn opening_a_real_file_clears_a_stale_diff_view() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "disk\n").unwrap();
        let mut e = Editor::new();
        e.diff = Some(crate::widgets::diff::DiffData::build_unified_deletion(
            path.clone(),
            "old\n",
        ));
        e.open(&path).unwrap();
        assert!(
            e.diff.is_none(),
            "the stale diff kept rendering over the freshly opened file"
        );
    }

    /// Deduping a snapshot/HEAD diff onto an existing tab must not destroy a
    /// dirty buffer: the TIMELINE always targets the file being edited, so
    /// replacing the tab wholesale silently discards unsaved edits.
    #[test]
    fn a_head_diff_never_replaces_a_dirty_tab_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "disk\n").unwrap();
        let mut tabs = EditorTabs::new();
        tabs.open(&path).unwrap();
        tabs.lines[0].push('x');
        tabs.dirty = true;
        tabs.open_head_diff_with_text(
            PathBuf::from("a.txt (local snapshot)"),
            "old\n",
            &path,
            false,
        )
        .unwrap();
        assert!(
            tabs.editors
                .iter()
                .any(|e| e.dirty && e.diff.is_none() && e.lines[0] == "diskx"),
            "the dirty buffer was destroyed by the snapshot diff"
        );
        assert!(
            tabs.editors.iter().any(|e| e.diff.is_some()),
            "staging: the diff itself still opened in some tab"
        );
    }

    #[test]
    fn disabling_the_csv_viewer_opens_the_file_as_plain_text() {
        // A .csv opens in the inline sheet viewer by default, but when the CSV
        // extension is disabled in the Extensions panel the same file must fall
        // through to a normal text buffer (no sheet, raw rows as lines).
        let f = tempfile::Builder::new()
            .suffix(".csv")
            .tempfile()
            .expect("temp csv");
        std::fs::write(f.path(), "a,b\n1,2\n").expect("write csv");

        let mut on = Editor::new();
        on.open(f.path()).expect("open with viewer enabled");
        assert!(on.sheet.is_some(), "enabled CSV viewer renders the sheet");

        let mut off = Editor::new();
        off.csv_viewer_enabled = false;
        off.open(f.path()).expect("open with viewer disabled");
        assert!(
            off.sheet.is_none(),
            "disabled CSV viewer must not build a sheet"
        );
        assert_eq!(
            off.lines,
            vec!["a,b".to_string(), "1,2".to_string()],
            "disabled CSV viewer opens the raw rows as text lines"
        );
    }

    /// A whole-buffer swap leaves no attribution behind (#349).
    ///
    /// Undo/redo and a reload both replace `lines` outright, and the map
    /// described the text that was replaced. Keeping it would credit lines to
    /// whoever wrote the buffer's PREVIOUS contents — the invariant's worst
    /// form, because nothing about the resulting overlay looks wrong.
    #[test]
    fn a_whole_buffer_swap_forgets_the_old_attributions() {
        let mut ed = Editor::new();
        ed.lines = vec![String::from("a"), String::from("b")];
        ed.cursor_row = 0;
        ed.cursor_col = 0;
        ed.insert_str_as("x\ny", crate::provenance::Seat::Navigator);
        assert!(
            ed.provenance.attributed() > 0,
            "staging: something is attributed before the swap"
        );

        ed.undo();
        assert_eq!(
            ed.provenance.attributed(),
            0,
            "undo left a map describing text that is gone: {:?}",
            ed.provenance
        );

        // A reload is the same shape: `load_text` replaces the buffer, so
        // whatever the map said described the file's previous contents.
        let mut re = Editor::new();
        re.lines = vec![String::from("old")];
        re.cursor_row = 0;
        re.cursor_col = 0;
        re.insert_str_as("mine", crate::provenance::Seat::Me);
        assert!(re.provenance.attributed() > 0, "staging: attributed");
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("other.txt");
        std::fs::write(&f, "entirely different\ntext\n").unwrap();
        re.open(&f).unwrap();
        assert_eq!(
            re.provenance.attributed(),
            0,
            "a reload kept attributions for text it replaced: {:?}",
            re.provenance
        );
    }

    /// Splitting a line attributes BOTH halves to the seat that split it,
    /// and that is deliberate (#349).
    ///
    /// It looks wrong on first read — the tail is someone else's text — but
    /// the line they wrote no longer exists. Neither half is a line they
    /// wrote, so crediting them would be the wrong answer in the other
    /// direction. Pinned with this note so the next reader finds the
    /// reasoning rather than filing it as a bug.
    #[test]
    fn splitting_a_line_gives_both_halves_to_the_splitting_seat() {
        let mut ed = Editor::new();
        ed.lines = vec![String::from("hello world")];
        ed.provenance
            .record(0..1, crate::provenance::Seat::Peer(String::from("ada")));
        ed.cursor_row = 0;
        ed.cursor_col = 5;
        ed.insert_str_as("\n", crate::provenance::Seat::Me);

        assert_eq!(
            ed.lines,
            vec![String::from("hello"), String::from(" world")]
        );
        assert_eq!(ed.provenance.seat(0), Some(&crate::provenance::Seat::Me));
        assert_eq!(
            ed.provenance.seat(1),
            Some(&crate::provenance::Seat::Me),
            "the tail is a NEW line, not ada's old one"
        );
    }

    /// Inserted text is attributed to the seat that inserted it, and
    /// nothing else in the buffer is (#349).
    ///
    /// Asserted as a contrast: an untouched line beside the written ones,
    /// because "the new lines are attributed" passes just as well against an
    /// implementation that attributes the whole buffer to whoever edited
    /// last — which is precisely the wrong answer this feature exists to
    /// avoid giving.
    #[test]
    fn inserted_text_carries_the_seat_that_wrote_it() {
        let mut ed = Editor::new();
        ed.lines = vec![String::from("existing"), String::from("tail")];
        ed.cursor_row = 1;
        ed.cursor_col = 0;

        ed.insert_str_as("one\ntwo", crate::provenance::Seat::Navigator);

        assert_eq!(
            ed.provenance.seat(1),
            Some(&crate::provenance::Seat::Navigator),
            "the first inserted line is the navigator's"
        );
        assert_eq!(
            ed.provenance.seat(2),
            Some(&crate::provenance::Seat::Navigator),
            "and so is the second"
        );
        // The line that was already there is NOT attributed: croft did not
        // watch it being written.
        assert_eq!(
            ed.provenance.seat(0),
            None,
            "a pre-existing line must not inherit the editing seat"
        );

        // An insert into an EMPTY buffer: `insert_char_raw` pushes the first
        // line, so the line count moves 0 -> 1 and the recorded range covers
        // exactly the line that now exists. Worth pinning because reasoning
        // about it from the arithmetic alone gets it wrong — `added` looks
        // like 0 until you notice the push.
        let mut empty = Editor::new();
        empty.insert_str_as("hello", crate::provenance::Seat::Me);
        assert_eq!(empty.lines.len(), 1, "staging: the buffer has one line");
        assert_eq!(
            empty.provenance.seat(0),
            Some(&crate::provenance::Seat::Me),
            "the only line must be attributed"
        );
        assert_eq!(
            empty.provenance.attributed(),
            1,
            "and nothing beyond it: {:?}",
            empty.provenance
        );

        // Replacing a MULTI-LINE selection. `delete_selection_inner` runs
        // before the line-count snapshot, so `added` measures only the
        // insertion — but the lines the selection removed must lose their
        // attribution rather than carry it onto the replacement, which is
        // the invariant in its most tempting-to-break form.
        let mut sel = Editor::new();
        sel.lines = vec![
            String::from("keep"),
            String::from("gone one"),
            String::from("gone two"),
            String::from("tail"),
        ];
        // FOUR DISTINCT SEATS, deliberately. With one seat on every line a
        // misattribution is invisible — the wrong seat and the right one are
        // the same value — so the assertions below would pass against the
        // very bug they exist to catch. Only the count would fail.
        sel.provenance.record(0..1, crate::provenance::Seat::Me);
        sel.provenance
            .record(1..2, crate::provenance::Seat::Navigator);
        sel.provenance
            .record(2..3, crate::provenance::Seat::Agent(String::from("pane 2")));
        sel.provenance
            .record(3..4, crate::provenance::Seat::Peer(String::from("ada")));
        sel.cursor_row = 1;
        sel.cursor_col = 0;
        sel.selection = Some(EditorSelection {
            anchor: (1, 0),
            head: (2, 8),
        });
        sel.insert_str_as("replaced", crate::provenance::Seat::Generated);

        assert_eq!(
            sel.provenance.seat(0),
            Some(&crate::provenance::Seat::Me),
            "the line above the selection keeps its own seat"
        );
        assert_eq!(
            sel.provenance.seat(1),
            Some(&crate::provenance::Seat::Generated),
            "the replacement belongs to the seat that made it"
        );
        // The surviving `tail` was ada's. Before the fix it painted as the
        // AGENT — the seat of a line the selection destroyed — which is the
        // bug in the form that matters, and the form a same-seat fixture
        // cannot see.
        assert_eq!(
            sel.provenance.seat(2),
            Some(&crate::provenance::Seat::Peer(String::from("ada"))),
            "the surviving line kept its own seat, not a destroyed line's"
        );
        assert_eq!(
            sel.provenance.attributed(),
            sel.lines.len(),
            "one seat per line, none past the end: {:?}",
            sel.provenance
        );

        // A second seat writing elsewhere does not repaint the first's work.
        ed.cursor_row = 0;
        ed.cursor_col = 0;
        ed.insert_str_as("mine", crate::provenance::Seat::Me);
        assert_eq!(ed.provenance.seat(0), Some(&crate::provenance::Seat::Me));
        assert_eq!(
            ed.provenance.seat(2),
            Some(&crate::provenance::Seat::Navigator),
            "the navigator's lines kept their seat through another edit"
        );
    }

    #[test]
    fn toggle_breakpoint_adds_then_removes_on_cursor_line() {
        let mut e = Editor::new();
        e.path = Some(PathBuf::from("/x/a.py"));
        e.lines = vec!["a".into(), "b".into(), "c".into()];
        e.cursor_row = 1; // 0-based row 1 => 1-based line 2
        // First toggle adds it (now_set = true).
        assert_eq!(
            e.toggle_breakpoint(),
            Some((PathBuf::from("/x/a.py"), 2, true))
        );
        assert_eq!(e.breakpoint_lines(Path::new("/x/a.py")), vec![2u32]);
        // Second toggle removes it (now_set = false).
        assert_eq!(
            e.toggle_breakpoint(),
            Some((PathBuf::from("/x/a.py"), 2, false))
        );
        assert!(e.breakpoint_lines(Path::new("/x/a.py")).is_empty());
    }

    #[test]
    fn toggle_breakpoint_without_open_file_is_noop() {
        let mut e = Editor::new();
        assert_eq!(e.toggle_breakpoint(), None);
    }

    #[test]
    fn removing_a_breakpoint_discards_its_logpoint_and_condition() {
        // F9 twice on a logpoint line reads as "remove, then set a plain
        // breakpoint" — but the orphaned message survived in
        // `breakpoint_logs` and re-attached on the next set, resurrecting a
        // logpoint that never pauses. Same leak for conditions.
        let path = PathBuf::from("/x/a.py");
        let mut e = Editor::new();
        e.path = Some(path.clone());
        e.lines = vec!["a".into(), "b".into(), "c".into()];
        e.toggle_breakpoint_line(2);
        e.breakpoint_logs
            .entry(path.clone())
            .or_default()
            .insert(2, String::from("x is {x}"));
        e.breakpoint_conditions
            .entry(path.clone())
            .or_default()
            .insert(2, String::from("x > 1"));
        e.toggle_breakpoint_line(2); // remove
        e.toggle_breakpoint_line(2); // re-add: a PLAIN breakpoint
        let lines = e.breakpoints.get(&path).cloned().unwrap();
        let specs = e.source_breakpoints(&path, &lines);
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].log_message, None,
            "a re-added breakpoint must pause, not print"
        );
        assert_eq!(
            specs[0].condition, None,
            "a re-added breakpoint must be unconditional"
        );
    }

    #[test]
    fn toggle_breakpoint_line_targets_an_explicit_line_not_the_cursor() {
        let mut e = Editor::new();
        e.path = Some(PathBuf::from("/x/a.py"));
        e.lines = vec!["a".into(), "b".into(), "c".into()];
        e.cursor_row = 0; // cursor on line 1, but we target line 3
        assert_eq!(
            e.toggle_breakpoint_line(3),
            Some((PathBuf::from("/x/a.py"), 3, true))
        );
        assert_eq!(e.breakpoint_lines(Path::new("/x/a.py")), vec![3u32]);
        // Cursor never moved.
        assert_eq!(e.cursor_row, 0);
        // Toggling the same line removes it again.
        assert_eq!(
            e.toggle_breakpoint_line(3),
            Some((PathBuf::from("/x/a.py"), 3, false))
        );
        assert!(e.breakpoint_lines(Path::new("/x/a.py")).is_empty());
    }

    #[test]
    fn gutter_line_at_maps_gutter_clicks_and_ignores_the_body() {
        let mut e = editor_with("a\nbb\nccc\ndddd\neeeee");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2; // text_x = 0 + 2 + 1 = 3
        // A click in the glyph margin / line-number columns maps to the line.
        assert_eq!(e.gutter_line_at(0, 0), Some(0), "col 0 is the glyph margin");
        assert_eq!(e.gutter_line_at(2, 2), Some(2), "col 2 is the line number");
        // The body (col >= text_x) is NOT the gutter.
        assert_eq!(e.gutter_line_at(3, 0), None, "col 3 is the text body");
        assert_eq!(e.gutter_line_at(10, 1), None, "deep in the body");
        // Past the last line there is no gutter target.
        assert_eq!(e.gutter_line_at(0, 20), None, "row past content");
    }

    #[test]
    fn gutter_line_at_follows_vertical_scroll() {
        let mut e = editor_with("a\nbb\nccc\ndddd\neeeee");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        e.scroll = 2;
        assert_eq!(
            e.gutter_line_at(1, 0),
            Some(2),
            "the top visible gutter row is buffer line scroll = 2"
        );
    }

    #[test]
    fn breakpoint_glyph_sits_in_left_margin_not_over_the_line_number() {
        let f = NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "aaa\nbbb\nccc\n").unwrap();
        let mut ed = Editor::new();
        ed.open(f.path()).unwrap();
        ed.breakpoints
            .entry(f.path().to_path_buf())
            .or_default()
            .insert(2); // breakpoint on line 2
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
        // The editor insets by a 1-col border, so inner.x == 1 and content rows
        // start at y == 1; line 2 is therefore at y == 2.
        let inner_x = ed.last_inner.x;
        let gutter = ed.last_gutter_width;
        let row = ed.last_inner.y + 1; // line 2 is the 2nd content row
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buf[(x, row)].symbol());
        }
        // The dot lives in the far-left glyph margin...
        assert_eq!(
            buf[(inner_x, row)].symbol(),
            "●",
            "breakpoint dot must be in the left glyph margin; row was {line:?}"
        );
        // ...the line number is right-aligned after the margin (its last digit
        // sits one cell before the gutter's trailing space) and is intact...
        assert_eq!(
            buf[(inner_x + gutter - 2, row)].symbol(),
            "2",
            "line number must survive after the margin; row was {line:?}"
        );
        // ...and the margin never overwrites a digit (the cell after the dot is
        // a blank, not a number).
        assert_eq!(
            buf[(inner_x + 1, row)].symbol(),
            " ",
            "a space separates the dot from the line number; row was {line:?}"
        );
    }

    #[test]
    fn source_breakpoints_attach_conditions() {
        let mut e = Editor::new();
        let p = PathBuf::from("/x/a.py");
        let mut lines = std::collections::BTreeSet::new();
        lines.insert(3usize);
        lines.insert(5usize);
        e.breakpoint_conditions
            .entry(p.clone())
            .or_default()
            .insert(5, String::from("i == 10"));
        let specs = e.source_breakpoints(&p, &lines);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].line, 3);
        assert_eq!(specs[0].condition, None);
        assert_eq!(specs[1].line, 5);
        assert_eq!(specs[1].condition.as_deref(), Some("i == 10"));
    }

    #[test]
    fn source_breakpoints_attach_log_messages() {
        let mut e = Editor::new();
        let p = PathBuf::from("/x/a.py");
        let mut lines = std::collections::BTreeSet::new();
        lines.insert(3usize);
        lines.insert(5usize);
        e.breakpoint_logs
            .entry(p.clone())
            .or_default()
            .insert(5, String::from("value is {v}"));
        let specs = e.source_breakpoints(&p, &lines);
        assert_eq!(specs[0].log_message, None);
        assert_eq!(specs[1].log_message.as_deref(), Some("value is {v}"));
    }

    #[test]
    fn logpoint_wears_an_amber_diamond_in_the_gutter() {
        let mut e = editor_with("a\nb\nc");
        let p = PathBuf::from("/x/lp.py");
        e.path = Some(p.clone());
        e.breakpoints.entry(p.clone()).or_default().insert(2);
        e.breakpoint_logs
            .entry(p)
            .or_default()
            .insert(2, String::from("hit {x}"));
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 6,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let cell = &buf[(e.last_inner.x, e.last_inner.y + 1)];
        assert_eq!(
            cell.symbol(),
            "◆",
            "a logpoint paints the diamond in the glyph margin"
        );
        assert_eq!(
            cell.fg,
            Color::Rgb(0xe5, 0xc0, 0x7b),
            "the logpoint diamond is amber, distinct from the conditional's red"
        );
    }

    #[test]
    fn word_string_at_reads_identifier_under_cursor() {
        let mut e = Editor::new();
        e.lines = vec!["    total = count + 1".into()];
        // cursor inside "total" (chars 4..9)
        assert_eq!(e.word_string_at(0, 6).as_deref(), Some("total"));
        // cursor inside "count"
        assert_eq!(e.word_string_at(0, 13).as_deref(), Some("count"));
        // over the '=' (non-word) => None
        assert_eq!(e.word_string_at(0, 10), None);
    }

    fn editor_with(text: &str) -> Editor {
        let mut e = Editor::new();
        e.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.lines().map(|s| s.to_string()).collect()
        };
        if e.lines.is_empty() {
            e.lines.push(String::new());
        }
        e
    }

    fn head(lines: &[&str]) -> Option<Vec<String>> {
        Some(lines.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn git_gutter_marks_added_modified_and_deleted_lines() {
        let mut e = editor_with("a\nB2\nINSERTED\nc");
        // HEAD: a / b / c / d  → b became B2 (modified), INSERTED is new (added),
        // and d was removed before EOF.
        e.set_git_head_lines(PathBuf::from("f.rs"), head(&["a", "b", "c", "d"]));
        e.refresh_git_marks();
        assert_eq!(e.git_mark_at(0), None, "unchanged line carries no mark");
        assert_eq!(e.git_mark_at(1), Some(GitMark::Modified));
        assert_eq!(e.git_mark_at(2), Some(GitMark::Added));
        // `c` survives at line 3 with the deletion of `d` just below it.
        assert_eq!(e.git_mark_at(3), Some(GitMark::Deleted));
    }

    #[test]
    fn git_gutter_no_baseline_means_no_marks() {
        let mut e = editor_with("a\nb\nc");
        e.refresh_git_marks();
        assert_eq!(e.git_mark_at(0), None);
        assert_eq!(e.git_mark_at(1), None);
    }

    #[test]
    fn git_gutter_recomputes_after_an_edit() {
        let mut e = editor_with("a\nb");
        e.set_git_head_lines(PathBuf::from("f.rs"), head(&["a", "b"]));
        e.refresh_git_marks();
        assert_eq!(e.git_mark_at(1), None, "buffer matches HEAD: clean");
        // Edit line 1, then a fresh render-time refresh must light it up.
        e.cursor_row = 1;
        e.cursor_col = 1;
        e.insert_char('X');
        e.refresh_git_marks();
        assert_eq!(e.git_mark_at(1), Some(GitMark::Modified));
    }

    #[test]
    fn git_gutter_render_paints_bar_in_the_spacer_cell() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("a\nb");
        e.set_git_head_lines(PathBuf::from("f.rs"), head(&["a", "DIFFERENT"]));
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 5,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        // Line 1 (`b` vs `DIFFERENT`) is modified; the bar sits in the spacer
        // cell at `inner.x + gutter_width`, one cell left of the text.
        let bar_x = e.last_inner.x + e.last_gutter_width;
        let cell = &buf[(bar_x, e.last_inner.y + 1)];
        assert_eq!(
            cell.symbol(),
            "\u{2503}",
            "modified line shows a gutter bar"
        );
    }

    #[test]
    fn save_on_a_preview_tab_never_touches_the_file() {
        // #185: the save choke point used to serialise the placeholder
        // text buffer over the previewed file, truncating a PNG to zero
        // bytes. Every preview kind must refuse at the choke point so no
        // caller (explicit save, force save, auto save, format-on-save)
        // can route around the guard.
        let tmp = tempfile::tempdir().unwrap();

        let png = tmp.path().join("pic.png");
        image::RgbaImage::new(2, 2).save(&png).unwrap();
        let png_bytes = std::fs::read(&png).unwrap();
        let mut e = Editor::new();
        e.open(&png).unwrap();
        assert!(e.image.is_some());
        assert!(e.save_to_disk().is_err(), "image tab save must refuse");
        assert!(e.save_to_disk_force().is_err(), "force save too");
        assert_eq!(std::fs::read(&png).unwrap(), png_bytes, "file intact");

        let bin = tmp.path().join("blob.bin");
        std::fs::write(&bin, b"\x00\x01\x02\x03").unwrap();
        let mut e = Editor::new();
        e.open(&bin).unwrap();
        assert!(e.hex.is_some());
        assert!(e.save_to_disk().is_err(), "hex tab save must refuse");
        assert_eq!(std::fs::read(&bin).unwrap(), b"\x00\x01\x02\x03");

        let csv = tmp.path().join("t.csv");
        std::fs::write(&csv, "a,b\n1,2\n").unwrap();
        let mut e = Editor::new();
        e.open(&csv).unwrap();
        assert!(e.sheet.is_some());
        assert!(e.save_to_disk().is_err(), "sheet tab save must refuse");
        assert_eq!(std::fs::read(&csv).unwrap(), b"a,b\n1,2\n");
    }
    #[test]
    fn is_binary_detects_nul() {
        assert!(is_binary(b"hello\0world"));
        assert!(!is_binary(b"hello world"));
        assert!(!is_binary(b""));
    }

    #[test]
    fn binary_file_opens_in_the_hex_viewer_instead_of_erroring() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("blob.bin");
        std::fs::write(&p, b"\x00\x01\x02payload\xff\xfe").unwrap();
        let mut e = Editor::new();
        e.open(&p).unwrap();
        assert!(e.hex.is_some(), "binary files land in the hex viewer");
        assert_eq!(
            e.lines,
            vec![String::new()],
            "the whole-buffer-swap convention: one empty line, like image/sheet tabs"
        );
        assert_eq!(e.path.as_deref(), Some(p.as_path()));
        assert!(!e.dirty, "hex tabs are read-only");
    }

    #[test]
    fn oversized_binary_opens_in_hex_but_oversized_text_still_errors() {
        // Only the head is sniffed, so the hex route must not read the
        // whole file — an over-limit BINARY opens fine (windowed IO),
        // while an over-limit TEXT file keeps the too-large error (the
        // text editor genuinely would have to load it whole).
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("huge.bin");
        let f = std::fs::File::create(&bin).unwrap();
        f.set_len(MAX_FILE_BYTES + 5).unwrap(); // sparse: leading NULs = binary
        drop(f);
        let mut e = Editor::new();
        e.open(&bin).unwrap();
        assert!(e.hex.is_some(), "an over-limit binary opens in hex");

        let txt = tmp.path().join("huge.log");
        let f = std::fs::File::create(&txt).unwrap();
        use std::io::Write as _;
        let mut w = std::io::BufWriter::new(f);
        w.write_all(&vec![b'a'; 8192]).unwrap();
        drop(w);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&txt)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 5)
            .unwrap();
        let mut e2 = Editor::new();
        let err = e2.open(&txt).unwrap_err().to_string();
        assert!(
            err.contains("too large"),
            "text keeps the size guard: {err}"
        );
    }

    #[test]
    fn hex_tab_renders_offsets_hex_bytes_ascii_and_status() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("blob.bin");
        let mut data = b"MZ\x90\x00".to_vec();
        data.extend_from_slice(b"Hello!\x00\xde\xad\xbe\xef");
        std::fs::write(&p, &data).unwrap();
        let mut ed = Editor::new();
        ed.open(&p).unwrap();
        assert!(ed.hex.is_some());
        let area = Rect {
            x: 0,
            y: 0,
            width: 92,
            height: 12,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
        let mut rows = Vec::new();
        for y in 0..area.height {
            let mut line = String::new();
            for x in 0..area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            rows.push(line);
        }
        let all = rows.join("\n");
        assert!(all.contains("blob.bin"), "header names the file: {all}");
        assert!(all.contains("00000000"), "offset column paints: {all}");
        assert!(
            all.contains("4D 5A 90 00"),
            "hex bytes paint uppercase: {all}"
        );
        assert!(
            all.contains("MZ·"),
            "ascii gutter: printables verbatim, non-printables as ·: {all}"
        );
        assert!(
            all.contains("0x00000000"),
            "status row shows the cursor offset: {all}"
        );
        let view = ed.hex.as_ref().unwrap();
        assert_eq!(view.bytes_per_row, 16, "a 92-col pane fits 16 bytes/row");
        assert!(
            view.layout.data_rows > 0,
            "layout written back for mouse hit-testing"
        );
    }

    #[test]
    fn hex_mouse_hit_test_round_trips_through_the_painted_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("blob.bin");
        // 60 bytes: the last 16-byte row is PARTIAL, so its tail cells
        // genuinely sit past EOF.
        std::fs::write(&p, (0u8..60).collect::<Vec<_>>()).unwrap();
        let mut ed = Editor::new();
        ed.open(&p).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: 92,
            height: 12,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
        let view = ed.hex.as_ref().unwrap();
        let l = view.layout;
        // Row 1 (second data row), byte 3: base 16 + 3 = 19.
        let y = l.data_top + 1;
        assert_eq!(view.hit_test(l.hex_x + 3 * 3, y), Some(19), "hex cell");
        assert_eq!(view.hit_test(l.ascii_x + 3, y), Some(19), "ascii cell");
        // Byte 10 sits past the 8-byte group gap in the hex grid.
        assert_eq!(
            view.hit_test(l.hex_x + 10 * 3 + 1, y),
            Some(26),
            "post-gap hex cell accounts for the group divider"
        );
        // Cells past EOF, the offset gutter, and the header all miss.
        let last_row_y = l.data_top + 3;
        assert_eq!(
            view.hit_test(l.ascii_x + 15, last_row_y),
            None,
            "past EOF misses"
        );
        assert_eq!(view.hit_test(area.x + 1, y), None, "offset gutter misses");
        assert_eq!(
            view.hit_test(l.hex_x, l.data_top - 1),
            None,
            "header misses"
        );
    }

    /// Smallest xlsx calamine accepts: the four structural parts plus one
    /// worksheet, stored uncompressed. Used by the content-routing tests.
    fn write_minimal_xlsx(p: &Path) {
        use std::io::Write as _;
        let f = std::fs::File::create(p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let parts: &[(&str, &str)] = &[
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#,
            ),
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>hello</t></is></c><c r="B1"><v>42</v></c></row></sheetData>
</worksheet>"#,
            ),
        ];
        for (name, body) in parts {
            z.start_file(*name, opts).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn svg_opens_as_a_rendered_preview_and_reopen_as_text_edits_the_source() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("logo.svg");
        std::fs::write(
            &p,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
<rect width="100" height="50" fill="#3366ff"/></svg>"##,
        )
        .unwrap();
        let mut e = Editor::new();
        e.open(&p).unwrap();
        let img = e.image.as_ref().expect("svg opens rendered");
        assert_eq!(img.format_label, "SVG");
        assert!(img.pixel_w > 0 && img.pixel_h > 0);
        assert!(
            image::load_from_memory(&img.bytes).is_ok(),
            "the overlay pipeline gets a decodable raster"
        );

        // Reopen as Text: the per-tab override lands in the XML source
        // and STICKS across the same-path reload behind FS sync.
        e.force_text = true;
        e.open(&p).unwrap();
        assert!(e.image.is_none());
        assert!(e.lines[0].contains("<svg"), "editing the source");
        e.open(&p).unwrap();
        assert!(
            e.image.is_none(),
            "the override survives a same-path reload (FS sync)"
        );
        // Reopen as Preview: clearing the override routes normally
        // again (the command's dispatch does exactly this).
        e.force_text = false;
        e.open(&p).unwrap();
        assert!(
            e.image.is_some(),
            "clearing the override restores the render"
        );
        // A DIFFERENT file drops the override: routing is per tab, not
        // per session.
        e.force_text = true;
        e.open(&p).unwrap();
        let q = tmp.path().join("other.svg");
        std::fs::copy(&p, &q).unwrap();
        e.open(&q).unwrap();
        assert!(e.image.is_some(), "a path change resets force_text");
    }

    /// #257: a colour-bearing log opens rendered, escapes never reach the
    /// text, and "Reopen as Text" round-trips back to the raw bytes.
    #[test]
    fn a_color_log_opens_rendered_and_reopen_as_text_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("build.log");
        std::fs::write(
            &p,
            "plain\n\u{1b}[31mERROR\u{1b}[0m boom\n\u{1b}[32mok\u{1b}[0m\n",
        )
        .unwrap();
        let mut e = Editor::new();
        e.open(&p).unwrap();

        let log = e.log.as_mut().expect("a colour log opens rendered");
        assert_eq!(log.len(), 3);
        log.ensure(0, 3).unwrap();
        assert_eq!(
            log.visible_text(1),
            Some("ERROR boom"),
            "escapes are stripped from what the user sees"
        );
        assert_eq!(
            log.line(1).unwrap().spans[0].style.fg,
            Some(crate::ansi_text::AnsiColor::Indexed(1)),
            "the theme palette resolves the colour, so it stays symbolic"
        );

        // Reopen as Text: raw escapes, and the override sticks across the
        // same-path reload the FS sweep performs.
        e.force_text = true;
        e.open(&p).unwrap();
        assert!(e.log.is_none());
        assert!(
            e.lines[1].contains('\u{1b}'),
            "the text view shows the real bytes"
        );
        e.open(&p).unwrap();
        assert!(e.log.is_none(), "the override survives a same-path reload");
        e.force_text = false;
        e.open(&p).unwrap();
        assert!(e.log.is_some(), "clearing the override renders again");
    }

    /// #257: the find highlight must reach the rendered log. The search runs
    /// on the stripped text, and the renderer paints spans from that same
    /// text, so the highlight columns land on the match the user searched
    /// for rather than being shifted by the escapes in the raw bytes.
    #[test]
    fn the_find_highlight_paints_over_a_rendered_log_line() {
        use ratatui::buffer::Buffer;
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("build.log");
        // The escape before `boom` would shift the columns by five if the
        // highlight were computed against the raw bytes.
        std::fs::write(&p, "plain\n\u{1b}[31mboom\u{1b}[0m tail\n").unwrap();
        let mut e = Editor::new();
        e.open(&p).unwrap();
        e.set_search_highlight(Some(String::from("boom")));
        e.active_search_match = Some((1, 0, 4));

        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);

        // Body row 1 is file line 1; the log body starts below the header.
        let y = e.last_inner.y + 2;
        let cell = &buf[(e.last_inner.x, y)];
        assert_eq!(cell.symbol(), "b", "the match starts at column 0");
        assert_eq!(
            cell.style().bg,
            Some(Color::Rgb(0xff, 0x8c, 0x2a)),
            "the ACTIVE match paints orange over the log's own colour"
        );
        let after = &buf[(e.last_inner.x + 4, y)];
        assert_ne!(
            after.style().bg,
            Some(Color::Rgb(0xff, 0x8c, 0x2a)),
            "the highlight stops at the end of the match"
        );
    }

    /// The sniff must not hijack ordinary files: a plain log routes to text,
    /// while ANSI inside a `.txt` still renders. Extension alone is not the
    /// signal, and neither is the mere presence of an escape byte.
    #[test]
    fn only_files_carrying_sgr_sequences_route_to_the_log_view() {
        let tmp = tempfile::TempDir::new().unwrap();
        let plain = tmp.path().join("plain.log");
        std::fs::write(&plain, "no colours here\njust text\n").unwrap();
        let mut e = Editor::new();
        e.open(&plain).unwrap();
        assert!(
            e.log.is_none(),
            "a log without colours is an ordinary editable file"
        );

        let txt = tmp.path().join("pytest.txt");
        std::fs::write(&txt, "\u{1b}[32mPASSED\u{1b}[0m\n").unwrap();
        e.open(&txt).unwrap();
        assert!(e.log.is_some(), "the sniff catches ANSI under a .txt name");

        // A screen recording (cursor movement, no SGR) is not a colour log.
        let rec = tmp.path().join("session.log");
        std::fs::write(&rec, "\u{1b}[2J\u{1b}[10;1Hmoved\n").unwrap();
        e.open(&rec).unwrap();
        assert!(e.log.is_none(), "cursor movement alone does not render");
    }

    /// #257 acceptance: a log far past the text size guard still opens,
    /// because the view is windowed rather than loaded whole. Under the old
    /// route this exact file was a "File too large" dead end.
    #[test]
    fn a_huge_color_log_opens_rendered_instead_of_hitting_the_size_guard() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("huge-color.log");
        // Real colour bytes up front so the sniff fires, then a sparse tail
        // that pushes the file well past MAX_FILE_BYTES without writing it.
        std::fs::write(&p, "\u{1b}[31mERROR\u{1b}[0m first line\n").unwrap();
        let f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.set_len(MAX_FILE_BYTES + 4096).unwrap();
        drop(f);

        let mut e = Editor::new();
        e.open(&p).expect("a windowed log open has no size ceiling");
        let log = e.log.as_mut().expect("opens rendered, not refused");
        assert!(log.file_len > MAX_FILE_BYTES);
        log.ensure(0, 1).unwrap();
        assert_eq!(
            log.visible_text(0),
            Some("ERROR first line"),
            "the first window parses without reading the whole file"
        );
    }

    /// #257 acceptance: the view must scroll past its first screen. It could
    /// not before — `scroll_view_to` clamped against `lines`, which is a
    /// one-line stub for a log, pinning scroll at 0 — so the windowing this
    /// feature exists for was unreachable in the shipped UI.
    #[test]
    fn a_rendered_log_scrolls_past_the_first_screen() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("big.log");
        let mut body = String::from("\u{1b}[31mstart\u{1b}[0m\n");
        for i in 1..2_000 {
            body.push_str(&format!("line{i}\n"));
        }
        std::fs::write(&p, body).unwrap();

        let mut e = Editor::new();
        e.open(&p).unwrap();
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        e.render(area, &mut buf);
        assert_eq!(e.scroll, 0);

        e.scroll_down(500);
        assert_eq!(e.scroll, 500, "the log scrolls by file line");
        e.render(area, &mut buf);
        let total = {
            let log = e.log.as_ref().unwrap();
            assert_eq!(
                log.visible_text(500),
                Some("line500"),
                "and the window follows the viewport"
            );
            log.len()
        };

        // Clamped at the tail rather than scrolling into blank space.
        e.scroll_down(usize::MAX / 2);
        assert!(e.scroll < total, "scroll stays inside the file");
        e.scroll_up(usize::MAX / 2);
        assert_eq!(e.scroll, 0, "and back to the top");
    }

    /// Colours resolve through the ACTIVE theme's ANSI palette, so a rendered
    /// log matches the terminal panes rather than a hardcoded table.
    #[test]
    fn rendered_log_paints_colors_from_the_theme_palette() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("c.log");
        std::fs::write(&p, "\u{1b}[31mRED\u{1b}[0m\n").unwrap();
        let mut e = Editor::new();
        e.open(&p).unwrap();

        let area = Rect::new(0, 0, 40, 4);
        let mut buf = Buffer::empty(area);
        e.render(area, &mut buf);

        let (r, g, b) = e.theme.ansi()[1];
        // Find the painted "RED" rather than assuming where the frame and
        // header put it.
        let mut found = None;
        for y in 0..area.height {
            for x in 0..area.width {
                if buf[(x, y)].symbol() == "R"
                    && buf[(x, y)].style().fg == Some(Color::Rgb(r, g, b))
                {
                    found = Some((x, y));
                }
            }
        }
        let (x, y) = found.expect("the red R paints somewhere in the body");
        assert_eq!(buf[(x + 1, y)].symbol(), "E", "the stripped text paints");
        assert_eq!(
            buf[(x + 1, y)].style().fg,
            Some(Color::Rgb(r, g, b)),
            "SGR 31 resolves through the theme's ANSI slot 1 across the span"
        );
    }

    #[test]
    fn broken_svg_falls_through_to_the_text_editor() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("broken.svg");
        std::fs::write(&p, "this is not xml").unwrap();
        let mut e = Editor::new();
        e.open(&p).unwrap();
        assert!(e.image.is_none());
        assert_eq!(e.lines, vec!["this is not xml".to_string()]);
    }

    #[test]
    fn extensionless_png_routes_to_the_image_preview_by_content() {
        let tmp = tempfile::tempdir().unwrap();
        let png = tmp.path().join("pic.png");
        image::RgbaImage::new(2, 2).save(&png).unwrap();
        let bare = tmp.path().join("logo");
        std::fs::copy(&png, &bare).unwrap();
        let mut e = Editor::new();
        e.open(&bare).unwrap();
        assert!(
            e.image.is_some(),
            "content routing must recognise the PNG without an extension"
        );
        assert!(e.hex.is_none());
    }

    #[test]
    fn misnamed_image_extension_falls_through_to_the_real_content() {
        let tmp = tempfile::tempdir().unwrap();
        // Plain text wearing .png: the decode failure must fall through
        // to the text editor, not fail the whole open.
        let fake = tmp.path().join("notes.png");
        std::fs::write(&fake, "just words\n").unwrap();
        let mut e = Editor::new();
        e.open(&fake).unwrap();
        assert!(e.image.is_none());
        assert_eq!(e.lines, vec!["just words".to_string()]);

        // Binary garbage wearing .png: decode fails, content is binary,
        // the hex fallback catches it — never a dead-end.
        let junk = tmp.path().join("junk.png");
        std::fs::write(&junk, b"\x00\x01\x02\x03garbage").unwrap();
        let mut e = Editor::new();
        e.open(&junk).unwrap();
        assert!(e.hex.is_some(), "misnamed binary lands in hex");
    }

    #[test]
    fn extensionless_workbook_routes_to_the_sheet_viewer_by_content() {
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("report");
        write_minimal_xlsx(&bare);
        let mut e = Editor::new();
        e.open(&bare).unwrap();
        let sheet = e
            .sheet
            .as_ref()
            .expect("zip content routed to the sheet viewer");
        assert_eq!(sheet.kind, crate::sheet::SheetKind::Xlsx);
        let data = &sheet.sheets[0];
        let all: String = data
            .headers
            .iter()
            .chain(data.rows.iter().flatten())
            .cloned()
            .collect::<Vec<_>>()
            .join(",");
        assert!(all.contains("hello") && all.contains("42"), "cells: {all}");
    }

    #[test]
    fn oversized_csv_keeps_its_size_refusal_instead_of_silently_opening() {
        // #188 review: the parse-failure fallthrough must not swallow
        // the sheet viewer's SIZE refusal — pre-#174 an over-cap CSV
        // errored loudly, and rerouting it into the text/binary path
        // silently bypassed the cap.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("big.csv");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(26 * 1024 * 1024).unwrap(); // sparse: over the 25MB cap
        drop(f);
        let mut e = Editor::new();
        let err = e.open(&p).unwrap_err().to_string();
        assert!(
            err.contains("too large"),
            "size refusal must surface: {err}"
        );
        assert!(e.sheet.is_none() && e.hex.is_none());
    }

    #[test]
    fn workbook_misnamed_as_csv_still_reaches_the_sheet_viewer() {
        // #188 review: a real xlsx wearing .csv fails the CSV parse and
        // falls through; the zip retry guard must only skip the ONE
        // route that already ran (.xlsx), not every sheet extension.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("data.csv");
        write_minimal_xlsx(&p);
        let mut e = Editor::new();
        e.open(&p).unwrap();
        let sheet = e.sheet.as_ref().expect("zip content retried as xlsx");
        assert_eq!(sheet.kind, crate::sheet::SheetKind::Xlsx);
    }

    #[test]
    fn extensionless_pdf_and_plain_zip_never_dead_end() {
        let tmp = tempfile::tempdir().unwrap();
        // A real-enough PDF head: whether poppler is installed decides
        // pdf-vs-hex, but the open must SUCCEED either way.
        let pdf = tmp.path().join("doc");
        std::fs::write(
            &pdf,
            b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n\x00",
        )
        .unwrap();
        let mut e = Editor::new();
        e.open(&pdf).unwrap();
        assert!(
            e.image.as_ref().is_some_and(|i| i.pdf.is_some()) || e.hex.is_some(),
            "a sniffed PDF opens in the reader or falls to hex, never errors"
        );

        // A zip that is NOT a workbook: the xlsx attempt fails, the hex
        // fallback catches it.
        let plain = tmp.path().join("bundle");
        let f = std::fs::File::create(&plain).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        use std::io::Write as _;
        z.start_file("readme.txt", opts).unwrap();
        z.write_all(b"inside").unwrap();
        z.finish().unwrap();
        let mut e = Editor::new();
        e.open(&plain).unwrap();
        assert!(
            e.archive.is_some(),
            "a non-workbook zip lands in the archive browser (#179)"
        );
        assert!(e.sheet.is_none() && e.hex.is_none());
    }

    #[test]
    fn preview_openers_supersede_a_diff_view_and_its_arrow_rects() {
        // #187 review: only `open`'s TEXT tail cleared `diff`, and the
        // preview arms return before the render's arrow-rect clearing —
        // a diff tab reopened as a preview kept its "left ↔ right"
        // label, its caret, and clickable hunk arrows.
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("blob.bin");
        std::fs::write(&bin, b"\x00\x01\x02").unwrap();
        let mut e = Editor::new();
        e.diff = Some(crate::widgets::diff::DiffData::build(
            std::path::PathBuf::from("left.txt"),
            std::path::PathBuf::from("right.txt"),
            vec![String::from("a")],
            vec![String::from("b")],
        ));
        e.diff_prev_arrow = Rect {
            x: 1,
            y: 1,
            width: 3,
            height: 1,
        };
        e.diff_next_arrow = e.diff_prev_arrow;
        e.open(&bin).unwrap();
        assert!(e.hex.is_some());
        assert!(e.diff.is_none(), "the diff view is superseded");
        assert_eq!(e.diff_prev_arrow, Rect::default(), "stale hit rect cleared");
        assert_eq!(e.diff_next_arrow, Rect::default());

        let png = tmp.path().join("pic.png");
        image::RgbaImage::new(2, 2).save(&png).unwrap();
        let mut e = Editor::new();
        e.diff = Some(crate::widgets::diff::DiffData::build(
            std::path::PathBuf::from("left.txt"),
            std::path::PathBuf::from("right.txt"),
            vec![String::from("a")],
            vec![String::from("b")],
        ));
        e.open(&png).unwrap();
        assert!(e.image.is_some());
        assert!(e.diff.is_none(), "image opener supersedes the diff too");
    }

    #[test]
    fn hex_render_narrower_than_the_8_byte_layout_paints_no_half_grid() {
        // #187 review: between the old 24-col floor and the 8-byte
        // layout's real need, the per-cell clip painted offsets with no
        // bytes. The empty state is the clamped header alone.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("blob.bin");
        std::fs::write(&p, vec![0u8; 64]).unwrap();
        let mut ed = Editor::new();
        ed.open(&p).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: 34, // inner 32: >= 24, < the 44 the 8-byte layout needs
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
        let mut all = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                all.push_str(buf[(x, y)].symbol());
            }
            all.push('\n');
        }
        assert!(all.contains("HEX"), "header still names the tab: {all}");
        assert!(
            !all.contains("00000000"),
            "no offset column without bytes to go with it: {all}"
        );
        assert!(!all.contains("00 "), "no lone byte cells: {all}");
        let view = ed.hex.as_ref().unwrap();
        assert_eq!(
            view.layout.data_rows, 0,
            "empty state publishes no hit-test rows"
        );
    }

    #[test]
    fn pending_hex_edits_survive_a_reclick_and_die_on_revert() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("blob.bin");
        std::fs::write(&p, vec![0u8; 8]).unwrap();
        let mut e = Editor::new();
        e.open(&p).unwrap();
        e.hex.as_mut().unwrap().apply_edit(0, 0x77);
        e.dirty = true;
        // A same-path re-open (tree re-click) must not drop the edits.
        e.open(&p).unwrap();
        assert_eq!(e.hex.as_ref().unwrap().effective_byte(0), Some(0x77));
        // Revert is the explicit discard: disk truth returns.
        e.revert_to_disk().unwrap();
        assert!(!e.hex.as_ref().unwrap().has_edits());
        assert_eq!(e.hex.as_ref().unwrap().effective_byte(0), Some(0));
        assert!(!e.dirty);
    }

    #[test]
    fn hex_render_drops_to_8_bytes_per_row_in_a_narrow_pane() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("blob.bin");
        std::fs::write(&p, vec![0x00u8; 64]).unwrap();
        let mut ed = Editor::new();
        ed.open(&p).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
        assert_eq!(ed.hex.as_ref().unwrap().bytes_per_row, 8);
    }

    #[test]
    fn hex_view_swaps_away_cleanly_when_a_text_file_reopens() {
        // The same tab object is reused across opens (the sheet/image
        // precedent): a hex tab navigated away to a text file must drop
        // the hex state, and vice versa.
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("blob.bin");
        std::fs::write(&bin, b"\x00\x01\x02").unwrap();
        let txt = tmp.path().join("notes.txt");
        std::fs::write(&txt, "hello\n").unwrap();
        let mut e = Editor::new();
        e.open(&bin).unwrap();
        assert!(e.hex.is_some());
        e.open(&txt).unwrap();
        assert!(e.hex.is_none(), "text reopen drops the hex state");
        assert_eq!(e.lines, vec!["hello".to_string()]);
        e.open(&bin).unwrap();
        assert!(e.hex.is_some(), "and back again");
        assert_eq!(e.lines, vec![String::new()]);
    }

    fn diag(
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
        severity: crate::lsp::manager::DiagnosticSeverity,
    ) -> crate::lsp::manager::Diagnostic {
        crate::lsp::manager::Diagnostic {
            start_line,
            start_char,
            end_line,
            end_char,
            severity,
            message: String::from("test diagnostic"),
        }
    }

    #[test]
    fn apply_diagnostics_decodes_into_per_line_char_spans() {
        use crate::lsp::manager::DiagnosticSeverity;
        let mut e = editor_with("const x = 1;");
        let p = std::path::PathBuf::from("/tmp/diag.ts");
        e.path = Some(p.clone());
        // Underline the `x` (chars 6..7).
        e.apply_diagnostics(p, vec![diag(0, 6, 0, 7, DiagnosticSeverity::Error)]);
        let spans = e.diagnostic_spans_for_test();
        assert_eq!(
            spans[0],
            vec![(6, 7, DiagnosticSeverity::Error)],
            "the diagnostic must decode to a single-char run over `x`"
        );
    }

    #[test]
    fn zero_width_diagnostic_is_widened_to_one_cell() {
        use crate::lsp::manager::DiagnosticSeverity;
        let mut e = editor_with("const x = 1;");
        let p = std::path::PathBuf::from("/tmp/diag.ts");
        e.path = Some(p.clone());
        // A point diagnostic (start == end) must still show one cell.
        e.apply_diagnostics(p, vec![diag(0, 6, 0, 6, DiagnosticSeverity::Warning)]);
        assert_eq!(
            e.diagnostic_spans_for_test()[0],
            vec![(6, 7, DiagnosticSeverity::Warning)],
            "a zero-width diagnostic must widen to one cell so it stays visible"
        );
    }

    #[test]
    fn diagnostics_for_another_file_do_not_decode() {
        use crate::lsp::manager::DiagnosticSeverity;
        let mut e = editor_with("const x = 1;");
        e.path = Some(std::path::PathBuf::from("/tmp/open.ts"));
        // Batch is for a different file than the one loaded.
        e.apply_diagnostics(
            std::path::PathBuf::from("/tmp/other.ts"),
            vec![diag(0, 0, 0, 5, DiagnosticSeverity::Error)],
        );
        assert!(
            e.diagnostic_spans_for_test().iter().all(|l| l.is_empty()),
            "a batch for a different file must not underline the loaded one"
        );
    }

    #[test]
    fn empty_diagnostics_batch_clears_the_underlines() {
        use crate::lsp::manager::DiagnosticSeverity;
        let mut e = editor_with("const x = 1;");
        let p = std::path::PathBuf::from("/tmp/diag.ts");
        e.path = Some(p.clone());
        e.apply_diagnostics(p.clone(), vec![diag(0, 6, 0, 7, DiagnosticSeverity::Error)]);
        assert!(!e.diagnostic_spans_for_test()[0].is_empty());
        // An "all clear" republish carries an empty list.
        e.apply_diagnostics(p, Vec::new());
        assert!(
            e.diagnostic_spans_for_test().iter().all(|l| l.is_empty()),
            "an empty batch must erase the squiggles"
        );
    }

    #[test]
    fn diagnostics_at_returns_messages_covering_the_point() {
        use crate::lsp::manager::{Diagnostic, DiagnosticSeverity};
        let mut e = editor_with("const x = 1;");
        let p = std::path::PathBuf::from("/tmp/diag.ts");
        e.path = Some(p.clone());
        // Underline the `x` (chars 6..7) with a real message.
        e.apply_diagnostics(
            p,
            vec![Diagnostic {
                start_line: 0,
                start_char: 6,
                end_line: 0,
                end_char: 7,
                severity: DiagnosticSeverity::Error,
                message: String::from("Type 'number' is not assignable to type 'string'."),
            }],
        );
        assert_eq!(
            e.diagnostics_at(0, 6),
            vec![(
                DiagnosticSeverity::Error,
                String::from("Type 'number' is not assignable to type 'string'.")
            )],
            "a point inside the diagnostic range must surface its message"
        );
        assert!(
            e.diagnostics_at(0, 0).is_empty(),
            "a point outside every diagnostic range must surface nothing"
        );
    }

    #[test]
    fn diagnostics_at_widens_a_zero_width_diagnostic_to_one_cell() {
        use crate::lsp::manager::{Diagnostic, DiagnosticSeverity};
        let mut e = editor_with("const x = 1;");
        let p = std::path::PathBuf::from("/tmp/diag.ts");
        e.path = Some(p.clone());
        e.apply_diagnostics(
            p,
            vec![Diagnostic {
                start_line: 0,
                start_char: 6,
                end_line: 0,
                end_char: 6,
                severity: DiagnosticSeverity::Warning,
                message: String::from("missing semicolon"),
            }],
        );
        assert_eq!(
            e.diagnostics_at(0, 6),
            vec![(
                DiagnosticSeverity::Warning,
                String::from("missing semicolon")
            )],
            "a zero-width diagnostic must still be hoverable over its one widened cell"
        );
    }

    #[test]
    fn hover_region_at_prefers_the_word_then_falls_back_to_the_diagnostic_span() {
        use crate::lsp::manager::{Diagnostic, DiagnosticSeverity};
        // `a.b` — the `.` at char 1 is not a word char, but a diagnostic covers it.
        let mut e = editor_with("a.b");
        let p = std::path::PathBuf::from("/tmp/dot.ts");
        e.path = Some(p.clone());
        e.apply_diagnostics(
            p,
            vec![Diagnostic {
                start_line: 0,
                start_char: 1,
                end_line: 0,
                end_char: 2,
                severity: DiagnosticSeverity::Error,
                message: String::from("unexpected token"),
            }],
        );
        assert_eq!(
            e.hover_region_at(0, 0),
            Some((0, 0, 1)),
            "over the `a` identifier the region is the word range"
        );
        assert_eq!(
            e.hover_region_at(0, 1),
            Some((0, 1, 2)),
            "over the punctuation `.` the region falls back to the covering diagnostic span"
        );
    }

    #[test]
    fn render_paints_severity_coloured_underline_over_the_diagnostic_span() {
        use crate::lsp::manager::DiagnosticSeverity;
        let mut e = editor_with("const x = 1;");
        let p = std::path::PathBuf::from("/tmp/diag.ts");
        e.path = Some(p.clone());
        e.apply_diagnostics(p, vec![diag(0, 6, 0, 7, DiagnosticSeverity::Error)]);
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 5,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let red = Color::Rgb(0xf1, 0x4c, 0x4c);
        let mut underlined: Vec<(u16, u16, String)> = Vec::new();
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                if cell.modifier.contains(Modifier::UNDERLINED) && cell.underline_color == red {
                    underlined.push((x, y, cell.symbol().to_string()));
                }
            }
        }
        assert_eq!(
            underlined.len(),
            1,
            "exactly one cell (the `x`) must carry a red error underline; got {underlined:?}"
        );
        assert_eq!(
            underlined[0].2, "x",
            "the underline must sit under the offending `x` glyph"
        );
    }

    #[test]
    fn focused_editor_gradient_border_is_gated_on_focus_gradient() {
        use crate::gradient::{GRAD_TL, rgb_color};
        let mut e = editor_with("hi");
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 5,
        };

        // Black theme: rounded gradient corner in the gradient's TL colour.
        e.focus_gradient = true;
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "\u{256d}");
        assert_eq!(buf[(0, 0)].fg, rgb_color(GRAD_TL));

        // Croft Dark: square corner, solid blue (the historical highlight).
        e.focus_gradient = false;
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "\u{250c}");
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(0x4e, 0x9a, 0xff));
    }

    /// #248: the cursor line wears the theme's accent wash (VS Code
    /// `editor.lineHighlightBackground`), gutter through code, in the
    /// focused editor only, and it follows the cursor.
    #[test]
    fn current_line_wash_tracks_the_cursor_and_focus() {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 6,
        };
        let wash = e.theme.current_line_bg();
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        // Line 0 (rendered at y=1 inside the border) holds the cursor: its
        // row wears the wash from the gutter through the code cells, and its
        // neighbour does not.
        assert_eq!(buf[(1, 1)].bg, wash, "gutter cell wears the wash");
        assert_eq!(buf[(8, 1)].bg, wash, "code cell wears the wash");
        assert_ne!(buf[(8, 2)].bg, wash, "the next row stays unwashed");
        // The wash follows the cursor.
        e.cursor_row = 2;
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        assert_ne!(buf[(8, 1)].bg, wash, "the old row is released");
        assert_eq!(buf[(8, 3)].bg, wash, "the new cursor row is washed");
        // An unfocused editor paints no wash at all.
        e.focused = false;
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        assert_ne!(buf[(8, 3)].bg, wash, "no wash without focus");
    }

    fn inlay(line: u32, character: u32, label: &str) -> crate::lsp::manager::InlayHintItem {
        crate::lsp::manager::InlayHintItem {
            line,
            character,
            label: label.to_string(),
        }
    }

    /// Render `e` into a fresh 60x5 buffer and return the full text of the
    /// first content row (y = 1, inside the border).
    fn first_row_text(e: &mut Editor) -> String {
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 5,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (e as &mut Editor).render(area, &mut buf);
        (0..area.width)
            .map(|x| buf[(x, 1)].symbol().to_string())
            .collect()
    }

    #[test]
    fn inlay_hint_splices_dim_italic_label_into_the_row() {
        let mut e = editor_with("let x = f(y);");
        let p = std::path::PathBuf::from("/tmp/hints.rs");
        e.path = Some(p.clone());
        e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32"), inlay(0, 10, "n: ")]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 5,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let row: String = (0..area.width)
            .map(|x| buf[(x, 1)].symbol().to_string())
            .collect();
        assert!(
            row.contains("let x: i32 = f(n: y);"),
            "hints must splice into the rendered row; got {row:?}"
        );
        // The hint cells are dim italic; the real code cells are not.
        let hint_fg = e.theme.ignored_fg();
        let hint_x = (0..area.width)
            .find(|&x| {
                buf[(x, 1)].symbol() == "i"
                    && buf[(x + 1, 1)].symbol() == "3"
                    && buf[(x + 2, 1)].symbol() == "2"
            })
            .expect("the i32 hint must be on the row");
        for dx in 0..3 {
            let cell = &buf[(hint_x + dx, 1)];
            assert!(
                cell.modifier.contains(Modifier::ITALIC),
                "hint cells must be italic"
            );
            assert_eq!(cell.fg, hint_fg, "hint cells must use the muted grey");
        }
        let code_x = (0..area.width)
            .find(|&x| buf[(x, 1)].symbol() == "x")
            .expect("the x binding must be on the row");
        assert!(
            !buf[(code_x, 1)].modifier.contains(Modifier::ITALIC),
            "real code cells must stay non-italic"
        );
    }

    #[test]
    fn gutter_play_glyph_marks_test_fns_and_maps_clicks_to_the_name() {
        let mut e = editor_with("#[test]\nfn my_case() {}\nfn helper() {}");
        // Beads need a saved .rs file: an unsaved buffer has nothing a
        // runner could target.
        e.path = Some(PathBuf::from("/x/a.rs"));
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 6,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let sign_x = e.last_inner.x;
        // Buffer line 1 (`fn my_case`) paints on content row y = 2.
        assert_eq!(
            buf[(sign_x, 2)].symbol(),
            "\u{eb2c}",
            "a test fn's first row must wear the play glyph in the sign margin"
        );
        assert_ne!(
            buf[(sign_x, 3)].symbol(),
            "\u{eb2c}",
            "a plain fn must not wear the glyph"
        );
        assert_eq!(
            e.test_glyph_at(sign_x, 2).as_deref(),
            Some("my_case"),
            "clicking the glyph must resolve the test's name"
        );
        assert_eq!(
            e.test_glyph_at(sign_x, 3),
            None,
            "clicking the sign margin of a non-test line resolves nothing"
        );
        assert_eq!(
            e.test_glyph_at(sign_x + 1, 2),
            None,
            "the fold-chevron column is not the play glyph"
        );
    }

    /// The AI-stream stop button: while `stream_stop_line` is set, that
    /// line's sign cell wears the stop square (outranking the test play
    /// glyph), and `stream_stop_at` maps clicks on exactly that cell.
    #[test]
    fn gutter_stream_stop_glyph_renders_maps_clicks_and_outranks_the_play_glyph() {
        let mut e = editor_with("#[test]\nfn my_case() {}\nfn helper() {}");
        e.path = Some(PathBuf::from("/x/a.rs")); // beads need a saved .rs file
        e.stream_stop_line = Some(1); // 0-based: the test fn's line
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 6,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let sign_x = e.last_inner.x;
        // Buffer line 1 paints on content row y = 2 (breadcrumb row above).
        assert_eq!(
            buf[(sign_x, 2)].symbol(),
            "■",
            "the streamed row's sign cell wears the stop square, not the play glyph"
        );
        assert!(e.stream_stop_at(sign_x, 2), "the stop cell is clickable");
        assert!(
            !e.stream_stop_at(sign_x + 1, 2),
            "the fold-chevron column is not the button"
        );
        assert!(
            !e.stream_stop_at(sign_x, 3),
            "other rows are not the button"
        );

        // Stream over: the glyph clears and the play glyph returns.
        e.stream_stop_line = None;
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        assert_eq!(
            buf[(sign_x, 2)].symbol(),
            "\u{eb2c}",
            "without a stream the test play glyph owns the cell again"
        );
        assert!(!e.stream_stop_at(sign_x, 2));
    }

    /// A comment box anchored to line 0 renders as an unnumbered block
    /// between line 0 and line 1: title row naming the author, body, and a
    /// footer with the reply field and Ignore button. The gutter's number
    /// column stays blank on box rows, and every following line paints
    /// (and reports its screen row) shifted down by the box height.
    #[test]
    fn comment_box_renders_unnumbered_rows_between_lines() {
        let mut e = editor_with("first\nsecond\nthird");
        e.comment_boxes = vec![CommentBox {
            id: 7,
            line: 0,
            author: "navigator".into(),
            body: "tighten this".into(),
        }];
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);

        let row_text = |y: u16| -> String {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect()
        };
        // Breadcrumb row at y=0; line 0 at y=1 with its number.
        assert!(row_text(1).contains("first"));
        assert!(row_text(1).contains('1'), "line 0 keeps its number");
        // Box: title (author), body, footer (reply + Ignore) at y=2..=4.
        assert!(row_text(2).contains("navigator"), "title names the author");
        assert!(row_text(3).contains("tighten this"), "body renders");
        assert!(row_text(4).contains("Ignore"), "footer has the button");
        let gutter = e.last_gutter_width;
        for y in 2..=4u16 {
            let g: String = (0..gutter)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            assert!(
                g.chars().all(|c| !c.is_ascii_digit()),
                "box rows are unnumbered, got gutter {g:?} at y={y}"
            );
        }
        // Line 1 shifted below the box, with its own number.
        assert!(row_text(5).contains("second"));
        assert!(row_text(5).contains('2'));
        assert_eq!(
            e.screen_row_of_line(1),
            Some(5),
            "screen row mapping shifts by the box height"
        );
    }

    /// Box rows belong to no buffer line: a click inside the box maps to no
    /// buffer position, and a click on the line below the box maps to that
    /// line at its shifted screen row.
    #[test]
    fn clicks_map_through_comment_box_rows() {
        let mut e = editor_with("first\nsecond\nthird");
        e.comment_boxes = vec![CommentBox {
            id: 7,
            line: 0,
            author: "navigator".into(),
            body: "tighten this".into(),
        }];
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        assert_eq!(
            e.buffer_pos_at(text_x, 3),
            None,
            "a box row is not a buffer cell"
        );
        assert_eq!(
            e.buffer_pos_at(text_x, 5),
            Some((1, 0)),
            "the line below the box maps at its shifted row"
        );
    }

    /// The box's mouse surface: the footer's ✕ Ignore cells hit Ignore, the
    /// rest of the footer hits Reply, body rows hit Body, and buffer-text
    /// rows hit nothing.
    #[test]
    fn comment_box_hit_finds_reply_ignore_and_body() {
        let mut e = editor_with("first\nsecond\nthird");
        e.comment_boxes = vec![CommentBox {
            id: 7,
            line: 0,
            author: "navigator".into(),
            body: "tighten this".into(),
        }];
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        // Find the ✕ on the footer row rather than hard-coding geometry.
        let ignore_x = (0..area.width)
            .find(|&x| buf[(x, 4)].symbol() == "\u{2715}")
            .expect("the footer renders the ✕ Ignore button");
        assert_eq!(
            e.comment_box_hit(ignore_x, 4),
            Some((7, CommentHit::Ignore))
        );
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        assert_eq!(
            e.comment_box_hit(text_x + 2, 4),
            Some((7, CommentHit::Reply)),
            "the rest of the footer is the reply field"
        );
        assert_eq!(
            e.comment_box_hit(text_x + 2, 3),
            Some((7, CommentHit::Body))
        );
        assert_eq!(e.comment_box_hit(text_x + 2, 1), None, "text rows miss");
        assert_eq!(e.comment_box_hit(text_x + 2, 5), None);
    }

    /// In wrap mode (visual-row cursor motion) the caret steps over a box:
    /// down from line 0 lands on line 1, never inside the block.
    #[test]
    fn cursor_skips_comment_box_rows_in_wrap_mode() {
        let mut e = editor_with("first\nsecond\nthird");
        e.wrap_override = Some(true);
        e.comment_boxes = vec![CommentBox {
            id: 7,
            line: 0,
            author: "navigator".into(),
            body: "tighten this".into(),
        }];
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.move_down();
        assert_eq!(e.cursor_row, 1, "the caret skips the box");
        e.move_up();
        assert_eq!(e.cursor_row, 0);
    }

    /// A comment box on a line hidden inside a collapsed fold is never
    /// painted, so it must not count toward the scroll extent either: it
    /// used to inflate the content length, keeping a scrollbar alive for
    /// invisible content and letting the wheel scroll into blank space.
    #[test]
    fn a_box_hidden_by_a_fold_does_not_extend_the_scroll_geometry() {
        let mut e = editor_with("fn a() {\n    body();\n}\nlast");
        e.comment_boxes = vec![CommentBox {
            id: 1,
            line: 1,
            author: "navigator".into(),
            body: (0..20)
                .map(|i| format!("p{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        }];
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        e.toggle_fold(0);
        assert!(e.is_line_hidden(1), "the box's line folds away");
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        assert_eq!(
            e.last_scrollbar.width, 0,
            "4 lines minus a fold fit the viewport; nothing left to scroll"
        );
        e.scroll_down(3);
        assert_eq!(
            (e.scroll, e.scroll_sub),
            (0, 0),
            "scrolling must not walk into folded-away box rows"
        );
    }

    /// A comment box taller than the viewport must be reachable in non-wrap
    /// mode: scrolling used to be line-granular (a 3-line file cannot
    /// scroll at all), so a tall box was truncated at the viewport edge and
    /// its footer - the Reply field and ✕ Ignore - could never be seen.
    #[test]
    fn nonwrap_scrolling_reaches_a_tall_comment_boxes_footer() {
        let mut e = editor_with("first\nsecond\nthird");
        e.comment_boxes = vec![CommentBox {
            id: 1,
            line: 0,
            author: "navigator".into(),
            body: (0..30)
                .map(|i| format!("point {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        }];
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let screen = |buf: &ratatui::buffer::Buffer| -> String {
            (0..area.height)
                .map(|y| {
                    (0..area.width)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(
            !screen(&buf).contains("Ignore"),
            "the tall box overflows: its footer starts off-screen"
        );
        // Scroll far enough that the footer row must be inside the viewport.
        for _ in 0..12 {
            e.scroll_down(3);
        }
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        assert!(
            screen(&buf).contains("Ignore"),
            "scrolling must reach the box footer; got:\n{}",
            screen(&buf)
        );
    }

    /// A reply longer than the footer field must window around the caret:
    /// the frozen first-N-chars rendering meant typing past the field width
    /// went blind - no caret anywhere and the new text never appearing.
    #[test]
    fn a_long_reply_windows_so_the_caret_and_tail_stay_visible() {
        let mut e = editor_with("first\nsecond\nthird");
        e.comment_boxes = vec![CommentBox {
            id: 7,
            line: 0,
            author: "navigator".into(),
            body: "tighten this".into(),
        }];
        let reply = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ_TAIL";
        e.comment_focus = Some(CommentFocus {
            id: 7,
            reply: reply.into(),
            cursor: reply.chars().count(),
        });
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let footer: String = (0..area.width)
            .map(|x| buf[(x, 4)].symbol().to_string())
            .collect();
        assert!(
            footer.contains("_TAIL"),
            "the field must window to keep the tail under the caret visible: {footer:?}"
        );
        let caret_cells = (0..area.width)
            .filter(|&x| {
                buf[(x, 4)]
                    .style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            })
            .count();
        assert_eq!(caret_cells, 1, "exactly one caret cell renders: {footer:?}");
    }

    /// The focused box renders the typed reply in its footer field.
    #[test]
    fn focused_comment_box_renders_the_reply_draft() {
        let mut e = editor_with("first\nsecond\nthird");
        e.comment_boxes = vec![CommentBox {
            id: 7,
            line: 0,
            author: "navigator".into(),
            body: "tighten this".into(),
        }];
        e.comment_focus = Some(CommentFocus {
            id: 7,
            reply: String::from("why here?"),
            cursor: 9,
        });
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let footer: String = (0..area.width)
            .map(|x| buf[(x, 4)].symbol().to_string())
            .collect();
        assert!(
            footer.contains("why here?"),
            "the reply draft renders in the footer: {footer:?}"
        );
    }

    #[test]
    fn find_highlights_stay_readable_over_occurrence_tints() {
        // Cmd+F then Enter parks the caret ON a match; the idle-caret
        // documentHighlight then returns every occurrence of that word —
        // exactly the cells the find layer just painted black-on-gold. The
        // find layer must stay on top: an occurrence bg painted over it
        // keeps the BLACK foreground on a dark grey, which is unreadable,
        // and kills the orange active-match cue.
        let mut e = editor_with("config here");
        e.path = Some(PathBuf::from("/x/a.rs"));
        e.set_search_highlight(Some(String::from("config")));
        e.occurrences = vec![(0, 0, 6, false)];
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 5,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let gold = Color::Rgb(0xff, 0xd7, 0x4a);
        let occ = crate::theme::Theme::BLACK.occurrence_bg();
        let mut gold_cells = 0;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                if cell.bg == gold {
                    gold_cells += 1;
                }
                assert!(
                    !(cell.fg == Color::Black && cell.bg == occ),
                    "cell ({x},{y}) '{}' wears the find layer's black fg on the occurrence bg — unreadable",
                    cell.symbol()
                );
            }
        }
        assert!(
            gold_cells >= 6,
            "the find highlight must survive the occurrence pass (got {gold_cells} gold cells)"
        );
    }

    #[test]
    fn a_claimed_sign_cell_does_not_hit_test_as_the_play_bead() {
        // Render gives the sign cell to the stop arrow / breakpoint glyph /
        // AI-stream square before the play bead; the CLICK hit-test must
        // agree, or the natural gesture of clicking a red ● (VS Code toggles
        // a breakpoint there) silently starts a test run instead.
        let mut e = editor_with("#[test]\nfn my_case() {}");
        e.path = Some(PathBuf::from("/x/a.rs"));
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 6,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let sign_x = e.last_inner.x;
        assert_eq!(e.test_glyph_at(sign_x, 2).as_deref(), Some("my_case"));
        e.toggle_breakpoint_line(2);
        assert_eq!(
            e.test_glyph_at(sign_x, 2),
            None,
            "the breakpoint dot's cell must not run tests on click"
        );
        e.toggle_breakpoint_line(2);
        assert_eq!(
            e.test_glyph_at(sign_x, 2).as_deref(),
            Some("my_case"),
            "removing the dot hands the cell back to the bead"
        );
        e.stop_line = Some((e.path.clone().unwrap(), 2));
        assert_eq!(
            e.test_glyph_at(sign_x, 2),
            None,
            "the paused stop arrow owns its cell"
        );
        e.stop_line = None;
        e.stream_stop_line = Some(1);
        assert_eq!(
            e.test_glyph_at(sign_x, 2),
            None,
            "the AI-stream stop square owns its cell"
        );
    }

    #[test]
    fn gutter_play_glyph_yields_to_a_breakpoint_dot() {
        let mut e = editor_with("#[test]\nfn my_case() {}");
        let p = std::path::PathBuf::from("/tmp/bp.rs");
        e.path = Some(p.clone());
        e.breakpoints.entry(p).or_default().insert(2); // 1-based: the fn line
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 5,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        assert_eq!(
            buf[(e.last_inner.x, 2)].symbol(),
            "●",
            "a breakpoint on the fn line owns the shared sign cell"
        );
    }

    fn occ(
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
        write: bool,
    ) -> crate::lsp::manager::OccurrenceItem {
        crate::lsp::manager::OccurrenceItem {
            start_line,
            start_char,
            end_line,
            end_char,
            write,
        }
    }

    #[test]
    fn lsp_occurrences_paint_theme_tints_with_utf16_columns_and_clear_on_edit() {
        // "a𐐀x = x;" — 𐐀 is one char but two UTF-16 code units, so the
        // server's UTF-16 columns for the second `x` (units 7..8) land on
        // char column 6, one LESS than a naive unit-as-char mapping.
        let mut e = editor_with("a𐐀x = x;");
        let p = std::path::PathBuf::from("/tmp/occ.rs");
        e.path = Some(p.clone());
        e.apply_occurrences(vec![occ(0, 0, 0, 4, true), occ(0, 7, 0, 8, false)]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 5,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let write_bg = e.theme.occurrence_write_bg();
        let read_bg = e.theme.occurrence_bg();
        let a_x = (0..area.width)
            .find(|&x| buf[(x, 1)].symbol() == "a")
            .expect("the identifier must be on the row");
        for dx in 0..3 {
            assert_eq!(
                buf[(a_x + dx, 1)].bg,
                write_bg,
                "the write occurrence must tint all three identifier chars (dx={dx})"
            );
        }
        let second_x = (a_x + 3..area.width)
            .find(|&x| buf[(x, 1)].symbol() == "x")
            .expect("the second x must be on the row");
        assert_eq!(
            buf[(second_x, 1)].bg,
            read_bg,
            "the read occurrence must tint exactly the second x"
        );
        assert_ne!(
            buf[(second_x - 1, 1)].bg,
            read_bg,
            "the cell before the second x must stay untinted"
        );
        // Any edit invalidates the server's columns: the tints must vanish
        // until the app's next idle-cursor request answers.
        e.mark_buffer_changed();
        let mut buf2 = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf2);
        assert_ne!(
            buf2[(second_x, 1)].bg,
            read_bg,
            "an edit must clear the occurrence tints"
        );
    }

    #[test]
    fn inlay_hints_for_another_file_do_not_paint() {
        let mut e = editor_with("let x = f(y);");
        e.path = Some(std::path::PathBuf::from("/tmp/current.rs"));
        e.apply_inlay_hints(
            std::path::PathBuf::from("/tmp/other.rs"),
            vec![inlay(0, 5, ": i32")],
        );
        let row = first_row_text(&mut e);
        assert!(
            row.contains("let x = f(y);") && !row.contains("i32"),
            "hints for a different file must not paint; got {row:?}"
        );
    }

    #[test]
    fn wrap_mode_suppresses_inlay_hints() {
        let mut e = editor_with("let x = f(y);");
        let p = std::path::PathBuf::from("/tmp/hints.rs");
        e.path = Some(p.clone());
        e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
        e.wrap_override = Some(true);
        let row = first_row_text(&mut e);
        assert!(
            row.contains("let x = f(y);") && !row.contains("i32"),
            "wrap mode must render the raw text unshifted; got {row:?}"
        );
    }

    #[test]
    fn cursor_screen_pos_shifts_past_inlay_hints() {
        let mut e = editor_with("let x = f(y);");
        let p = std::path::PathBuf::from("/tmp/hints.rs");
        e.path = Some(p.clone());
        e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
        e.focused = true;
        let _ = first_row_text(&mut e);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        // Caret on the `=` (buffer col 6): five hint cells sit before it.
        e.cursor_row = 0;
        e.cursor_col = 6;
        assert_eq!(
            e.cursor_screen_pos(),
            Some((text_x + 11, e.last_inner.y)),
            "the caret must account for hint cells before it"
        );
        // Caret exactly at the hint's anchor (buffer col 5) stays LEFT of the
        // hint, like VS Code: typing there pushes the hint right.
        e.cursor_col = 5;
        assert_eq!(
            e.cursor_screen_pos(),
            Some((text_x + 5, e.last_inner.y)),
            "the caret at the anchor must sit before the hint"
        );
    }

    #[test]
    fn secondary_and_ghost_carets_sit_left_of_an_inlay_hint_like_the_primary() {
        // Change All Occurrences puts every extra caret at a word end —
        // exactly where rust-analyzer anchors a binding's type hint. The
        // primary caret deliberately sits LEFT of such a hint (see
        // cursor_screen_pos_shifts_past_inlay_hints); the block painted for
        // a secondary caret, and a collaborator's ghost caret, must agree
        // instead of landing a hint-width to the right, outside their own
        // selection band.
        let mut e = editor_with("let x = f(y);");
        let p = std::path::PathBuf::from("/tmp/hints.rs");
        e.path = Some(p.clone());
        e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 5,
        };

        // Secondary caret at the hint's anchor (buffer col 5).
        e.carets = vec![EditorSelection::new(0, 5)];
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let block_x = (0..area.width).find(|&x| buf[(x, 1)].bg == Color::Rgb(0xae, 0xc6, 0xff));
        assert_eq!(
            block_x,
            Some(text_x + 5),
            "the secondary caret's block must sit where the primary would"
        );

        // Ghost caret at the same anchor, painted in the participant color.
        e.carets.clear();
        e.ghost_carets = vec![(0, 5, Color::Red)];
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        let ghost_x = (0..area.width).find(|&x| buf[(x, 1)].bg == Color::Red);
        assert_eq!(
            ghost_x,
            Some(text_x + 5),
            "a collaborator's ghost caret must sit where the primary would"
        );
    }

    #[test]
    fn click_and_word_select_map_through_inlay_hints() {
        // Repro: with a type hint spliced into the row, the caret / selection
        // landed `hint` cells right of the pointer while hover (which goes
        // through `buffer_pos_at`) resolved the symbol under it correctly.
        let mut e = editor_with("let x = f(y);");
        let p = std::path::PathBuf::from("/tmp/hints.rs");
        e.path = Some(p.clone());
        e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
        let _ = first_row_text(&mut e);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;

        // `f` displays at col 13 (buffer col 8 plus the five hint cells).
        e.click(text_x + 13, y);
        assert_eq!(
            (e.cursor_row, e.cursor_col),
            (0, 8),
            "the caret must land where the pointer is, not past the hint"
        );

        // Double-click on the shifted `y` selects `y`, not a neighbour.
        e.select_word_at(text_x + 15, y);
        assert_eq!(
            e.selection.map(|s| (s.anchor, s.head)),
            Some(((0, 10), (0, 11))),
            "word select must resolve the word under the pointer"
        );
    }

    #[test]
    fn buffer_pos_at_maps_clicks_through_inlay_hints() {
        let mut e = editor_with("let x = f(y);");
        let p = std::path::PathBuf::from("/tmp/hints.rs");
        e.path = Some(p.clone());
        e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
        let _ = first_row_text(&mut e);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        // A click on the shifted `=` (display col 11) lands on buffer col 6.
        assert_eq!(e.buffer_pos_at(text_x + 11, y), Some((0, 6)));
        // A click inside the hint's cells snaps to its anchor column.
        assert_eq!(e.buffer_pos_at(text_x + 7, y), Some((0, 5)));
        // Cells before the hint are unaffected.
        assert_eq!(e.buffer_pos_at(text_x + 2, y), Some((0, 2)));
    }

    #[test]
    fn opening_a_file_drops_the_previous_files_inlay_hints() {
        let mut e = editor_with("let x = f(y);");
        let p = std::path::PathBuf::from("/tmp/hints.rs");
        e.path = Some(p.clone());
        e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
        let dir = std::env::temp_dir().join("croft_inlay_open_test");
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("other.rs");
        std::fs::write(&other, "fn g() {}\n").unwrap();
        e.open(&other).unwrap();
        let row = first_row_text(&mut e);
        assert!(
            !row.contains("i32"),
            "a newly opened file must not inherit stale hints; got {row:?}"
        );
    }

    #[test]
    fn markdown_preview_toggles_renders_and_returns_to_source() {
        let mut e = editor_with("# Title\n\nSome body text.");
        e.lang = Some(LangKind::Markdown);
        assert!(e.toggle_markdown_preview(), "a Markdown tab must toggle");
        let text = first_row_screen(&mut e, 60, 8);
        assert!(
            text.contains("Title") && !text.contains("# Title"),
            "the preview must render the heading without its # marker; got:\n{text}"
        );
        assert!(text.contains("Some body text."), "got:\n{text}");
        assert!(e.toggle_markdown_preview(), "toggling again returns");
        assert!(e.markdown_preview.is_none());
        let text = first_row_screen(&mut e, 60, 8);
        assert!(
            text.contains("# Title"),
            "the source view must show the raw markdown again; got:\n{text}"
        );
    }

    #[test]
    fn markdown_preview_refuses_non_markdown_tabs() {
        let mut e = editor_with("fn main() {}");
        e.lang = Some(LangKind::Rust);
        assert!(!e.toggle_markdown_preview());
        assert!(e.markdown_preview.is_none());
    }

    #[test]
    fn markdown_preview_rebuilds_when_the_buffer_moves() {
        let mut e = editor_with("# Old heading");
        e.lang = Some(LangKind::Markdown);
        assert!(e.toggle_markdown_preview());
        e.lines[0] = String::from("# New heading");
        e.edit_seq = e.edit_seq.wrapping_add(1);
        let text = first_row_screen(&mut e, 60, 8);
        assert!(
            text.contains("New heading") && !text.contains("Old heading"),
            "a stale preview must rebuild against the edited buffer; got:\n{text}"
        );
    }

    /// Render into a fresh buffer and return the whole screen as one string.
    fn first_row_screen(e: &mut Editor, width: u16, height: u16) -> String {
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (e as &mut Editor).render(area, &mut buf);
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn range_batch_does_not_overwrite_a_full_batch_for_the_same_file() {
        // The viewport range request races the whole-document request on open.
        // If a range reply lands AFTER the full reply, it must be dropped so it
        // cannot blank the off-screen colour the full reply painted.
        let mut e = editor_with("def f(x):\n    return x\n");
        let p = std::path::PathBuf::from("/tmp/sem_guard.py");
        e.path = Some(p.clone());
        let legend = std::sync::Arc::new(vec!["parameter".to_string()]);
        // Full batch: signature param (line 0) AND body reference (line 1).
        let full = vec![0, 6, 1, 0, 0, 1, 11, 1, 0, 0];
        e.apply_semantic_tokens(p.clone(), full, legend.clone(), true);
        let body_spans = e.semantic_overlay_for_test()[1].len();
        assert!(body_spans > 0, "full batch must colour the body reference");

        // A late range batch covering only line 0 must be rejected.
        let range_only_line0 = vec![0, 6, 1, 0, 0];
        e.apply_semantic_tokens(p.clone(), range_only_line0, legend, false);
        assert_eq!(
            e.semantic_overlay_for_test()[1].len(),
            body_spans,
            "a range batch must not erase off-screen colour from the full batch"
        );
    }

    #[test]
    fn range_batch_paints_first_then_full_replaces() {
        // The intended first-paint order: range colours the viewport, then the
        // full reply arrives and extends colour to off-screen lines.
        let mut e = editor_with("def f(x):\n    return x\n");
        let p = std::path::PathBuf::from("/tmp/sem_first.py");
        e.path = Some(p.clone());
        let legend = std::sync::Arc::new(vec!["parameter".to_string()]);
        // Range first: only line 0 (the viewport) is coloured.
        e.apply_semantic_tokens(p.clone(), vec![0, 6, 1, 0, 0], legend.clone(), false);
        let line1_before = e
            .semantic_overlay_for_test()
            .get(1)
            .map(|s| s.len())
            .unwrap_or(0);
        assert_eq!(
            line1_before, 0,
            "range batch leaves the off-screen line bare"
        );
        // Full reply lands and colours the body reference on line 1.
        e.apply_semantic_tokens(p.clone(), vec![0, 6, 1, 0, 0, 1, 11, 1, 0, 0], legend, true);
        assert!(
            !e.semantic_overlay_for_test()[1].is_empty(),
            "full reply must colour the previously-bare off-screen line"
        );
    }

    #[test]
    fn find_word_occurrences_matches_whole_words_only() {
        let chars: Vec<char> = "foo foobar foo_ foo".chars().collect();
        let word: Vec<char> = "foo".chars().collect();
        // Only the standalone "foo" at col 0 and the final "foo" qualify;
        // "foobar" and "foo_" are larger identifiers.
        assert_eq!(find_word_occurrences(&chars, &word), vec![0, 16]);
    }

    #[test]
    fn select_all_occurrences_picks_cursor_match_as_primary() {
        let mut e = editor_with("foo bar foo\nfoo");
        e.cursor_row = 0;
        e.cursor_col = 8; // second "foo" on line 0
        let n = e.select_all_occurrences_of_word_at_cursor();
        assert_eq!(n, 3);
        // Primary selection covers the occurrence under the cursor.
        assert_eq!(e.selection.unwrap().normalised(), ((0, 8), (0, 11)));
        // The other two become secondary carets.
        assert_eq!(e.carets.len(), 2);
        assert!(e.has_multi_cursor());
    }

    #[test]
    fn multi_insert_replaces_every_occurrence_in_one_undo_step() {
        let mut e = editor_with("foo bar foo\nfoo");
        e.cursor_row = 0;
        e.cursor_col = 0;
        assert_eq!(e.select_all_occurrences_of_word_at_cursor(), 3);
        e.multi_insert_char('X');
        assert_eq!(e.lines, vec!["X bar X".to_string(), "X".to_string()]);
        // One undo restores the whole batch at once.
        assert!(e.undo());
        assert_eq!(e.lines, vec!["foo bar foo".to_string(), "foo".to_string()]);
    }

    #[test]
    fn multi_backspace_deletes_at_every_caret() {
        let mut e = editor_with("ab ab ab");
        e.cursor_row = 0;
        e.cursor_col = 0;
        assert_eq!(e.select_all_occurrences_of_word_at_cursor(), 3);
        // Selections are active; backspace deletes each selected "ab".
        e.multi_backspace();
        assert_eq!(e.lines, vec!["  ".to_string()]);
    }

    #[test]
    fn collapse_carets_drops_secondary_cursors() {
        let mut e = editor_with("x x x");
        e.cursor_col = 0;
        e.select_all_occurrences_of_word_at_cursor();
        assert!(e.has_multi_cursor());
        e.collapse_carets();
        assert!(!e.has_multi_cursor());
        assert!(e.carets.is_empty());
    }

    #[test]
    fn apply_span_edits_applies_bottom_to_top() {
        let mut lines = vec!["foo bar foo".to_string()];
        let edits = vec![
            TextSpanEdit {
                start: (0, 0),
                end: (0, 3),
                new_text: "baz".to_string(),
            },
            TextSpanEdit {
                start: (0, 8),
                end: (0, 11),
                new_text: "baz".to_string(),
            },
        ];
        assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 2);
        assert_eq!(lines, vec!["baz bar baz".to_string()]);
    }

    #[test]
    fn apply_span_edits_splits_multiline_new_text_into_separate_lines() {
        // Ruff's "Organize Imports" returns ONE edit whose new_text spans several
        // lines. The result must become several Vec entries, not one line with
        // embedded '\n' (the bug that mashed the imports onto a single line).
        let mut lines = vec![
            "import typing".to_string(),
            "import pandas".to_string(),
            "import logging".to_string(),
            "import re".to_string(),
        ];
        let edits = vec![TextSpanEdit {
            start: (0, 0),
            end: (3, 9),
            new_text: "import logging\nimport re\nimport typing\n\nimport pandas".to_string(),
        }];
        assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 1);
        assert_eq!(
            lines,
            vec![
                "import logging".to_string(),
                "import re".to_string(),
                "import typing".to_string(),
                String::new(),
                "import pandas".to_string(),
            ]
        );
    }

    #[test]
    fn apply_span_edits_single_line_range_with_multiline_new_text() {
        // A same-line range whose new_text introduces a newline must split the
        // line in two, keeping the untouched prefix and suffix.
        let mut lines = vec!["abXYef".to_string()];
        let edits = vec![TextSpanEdit {
            start: (0, 2),
            end: (0, 4),
            new_text: "C\nD".to_string(),
        }];
        assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 1);
        assert_eq!(lines, vec!["abC".to_string(), "Def".to_string()]);
    }

    #[test]
    fn editor_apply_span_edits_marks_dirty_and_one_undo() {
        let mut e = editor_with("name = 1\nprint(name)");
        let edits = vec![
            TextSpanEdit {
                start: (0, 0),
                end: (0, 4),
                new_text: "label".to_string(),
            },
            TextSpanEdit {
                start: (1, 6),
                end: (1, 10),
                new_text: "label".to_string(),
            },
        ];
        assert_eq!(e.apply_span_edits(&edits), 2);
        assert!(e.dirty);
        assert_eq!(
            e.lines,
            vec!["label = 1".to_string(), "print(label)".to_string()]
        );
        assert!(e.undo());
        assert_eq!(
            e.lines,
            vec!["name = 1".to_string(), "print(name)".to_string()]
        );
    }

    #[test]
    fn is_binary_detects_high_nontext_ratio() {
        let mut data = vec![0x01u8; 100];
        data.extend_from_slice(b"abc");
        assert!(is_binary(&data));
    }

    #[test]
    fn is_binary_accepts_normal_text() {
        let txt = "fn main() { println!(\"hello\"); }\n// this is fine\n";
        assert!(!is_binary(txt.as_bytes()));
    }

    #[test]
    fn open_loads_a_real_world_sized_log_file_above_the_old_5mb_cap() {
        // Regression for the user's "I can't open .croft/lsp.log" report:
        // the LSP log grew past 5MB during normal use (their copy was
        // 7.4MB) and the old cap silently bailed with "File too large".
        // The current cap MUST accommodate at least 8MB of ASCII text so
        // a typical LSP log session round-trips.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.log");
        let line =
            "1778548312.915 lsp[ruff] stderr: 2026-05-12 02:11:52 INFO some workspace setting\n";
        let line_bytes = line.len();
        let target_bytes = 8 * 1024 * 1024;
        let line_count = target_bytes / line_bytes + 1;
        let mut buf = String::with_capacity(line_count * line_bytes);
        for _ in 0..line_count {
            buf.push_str(line);
        }
        std::fs::write(&path, &buf).unwrap();
        assert!(
            std::fs::metadata(&path).unwrap().len() > 5 * 1024 * 1024,
            "test setup must produce a file larger than the historical 5MB cap"
        );
        let mut e = Editor::new();
        e.open(&path)
            .expect("editor.open must accept an 8MB plain-text log; the user reported lsp.log (7.4MB) being unopenable");
        assert_eq!(e.lines.len(), line_count);
    }

    #[test]
    fn line_char_len_counts_chars_not_bytes() {
        let mut e = editor_with("héllo");
        assert_eq!(e.line_char_len(0), 5);
        e.lines[0] = String::from("日本語");
        assert_eq!(e.line_char_len(0), 3);
    }

    #[test]
    fn byte_index_ascii() {
        let e = editor_with("abcdef");
        assert_eq!(e.byte_index(0, 0), 0);
        assert_eq!(e.byte_index(0, 3), 3);
        assert_eq!(e.byte_index(0, 6), 6);
        assert_eq!(e.byte_index(0, 99), 6); // saturates at end
    }

    #[test]
    fn byte_index_multibyte() {
        let e = editor_with("héllo");
        // 'h'=1 byte, 'é'=2 bytes, 'l'=1 byte
        assert_eq!(e.byte_index(0, 0), 0);
        assert_eq!(e.byte_index(0, 1), 1); // before 'é'
        assert_eq!(e.byte_index(0, 2), 3); // after 'é'
        assert_eq!(e.byte_index(0, 3), 4);
    }

    #[test]
    fn insert_char_at_end() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_char('d');
        assert_eq!(e.lines[0], "abcd");
        assert_eq!(e.cursor_col, 4);
        assert!(e.dirty);
    }

    #[test]
    fn insert_char_at_start() {
        let mut e = editor_with("bc");
        e.cursor_col = 0;
        e.insert_char('a');
        assert_eq!(e.lines[0], "abc");
        assert_eq!(e.cursor_col, 1);
    }

    #[test]
    fn insert_char_in_middle() {
        let mut e = editor_with("ac");
        e.cursor_col = 1;
        e.insert_char('b');
        assert_eq!(e.lines[0], "abc");
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn insert_char_multibyte_position() {
        let mut e = editor_with("aé");
        e.cursor_col = 2; // after 'é'
        e.insert_char('z');
        assert_eq!(e.lines[0], "aéz");
    }

    #[test]
    fn insert_newline_splits_line() {
        let mut e = editor_with("hello world");
        e.cursor_col = 5;
        e.insert_newline();
        assert_eq!(e.lines, vec!["hello".to_string(), " world".to_string()]);
        assert_eq!(e.cursor_row, 1);
        assert_eq!(e.cursor_col, 0);
    }

    #[test]
    fn insert_newline_at_end_creates_blank_line() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_newline();
        assert_eq!(e.lines, vec!["abc".to_string(), String::new()]);
        assert_eq!(e.cursor_row, 1);
    }

    #[test]
    fn insert_newline_copies_previous_indent_for_unknown_language() {
        let mut e = editor_with("    abc");
        e.cursor_col = 7;
        e.insert_newline();
        assert_eq!(e.lines, vec!["    abc".to_string(), "    ".to_string()]);
        assert_eq!(e.cursor_row, 1);
        assert_eq!(e.cursor_col, 4);
    }

    #[test]
    fn insert_newline_python_indents_one_step_after_colon() {
        let mut e = editor_with("def hello():");
        e.lang = Some(LangKind::Python);
        e.cursor_col = e.line_char_len(0);
        e.insert_newline();
        assert_eq!(
            e.lines,
            vec!["def hello():".to_string(), "    ".to_string()]
        );
        assert_eq!(e.cursor_row, 1);
        assert_eq!(e.cursor_col, 4);
    }

    #[test]
    fn insert_newline_python_no_extra_indent_without_colon() {
        let mut e = editor_with("    print(x)");
        e.lang = Some(LangKind::Python);
        e.cursor_col = e.line_char_len(0);
        e.insert_newline();
        assert_eq!(
            e.lines,
            vec!["    print(x)".to_string(), "    ".to_string()]
        );
        assert_eq!(e.cursor_col, 4);
    }

    #[test]
    fn insert_newline_python_stacks_indent_on_nested_colon() {
        let mut e = editor_with("    if x:");
        e.lang = Some(LangKind::Python);
        e.cursor_col = e.line_char_len(0);
        e.insert_newline();
        assert_eq!(
            e.lines,
            vec!["    if x:".to_string(), "        ".to_string()]
        );
        assert_eq!(e.cursor_col, 8);
    }

    #[test]
    fn shift_tab_dedents_current_line_when_no_selection() {
        let mut e = editor_with("        pass");
        e.lang = Some(LangKind::Python);
        e.cursor_row = 0;
        e.cursor_col = 8;
        e.dedent_lines();
        assert_eq!(e.lines, vec!["    pass".to_string()]);
        assert_eq!(e.cursor_col, 4, "cursor follows the four stripped spaces");
    }

    #[test]
    fn shift_tab_dedent_aligns_to_previous_tab_stop() {
        // Six leading spaces drop to four (the previous width-4 tab stop),
        // removing two, not a flat four.
        let mut e = editor_with("      x = 1");
        e.lang = Some(LangKind::Python);
        e.cursor_col = 6;
        e.dedent_lines();
        assert_eq!(e.lines, vec!["    x = 1".to_string()]);
        assert_eq!(e.cursor_col, 4);
    }

    #[test]
    fn shift_tab_dedent_partial_indent_removes_all_leading_spaces() {
        let mut e = editor_with("   y");
        e.lang = Some(LangKind::Python);
        e.cursor_col = 3;
        e.dedent_lines();
        assert_eq!(e.lines, vec!["y".to_string()]);
        assert_eq!(e.cursor_col, 0);
    }

    #[test]
    fn shift_tab_dedent_on_flush_left_line_is_a_noop() {
        let mut e = editor_with("x");
        e.lang = Some(LangKind::Python);
        e.cursor_col = 1;
        e.dedent_lines();
        assert_eq!(e.lines, vec!["x".to_string()]);
        assert_eq!(e.cursor_col, 1);
    }

    #[test]
    fn shift_tab_dedent_strips_a_single_leading_tab() {
        let mut e = editor_with("\tpass");
        e.lang = Some(LangKind::Python);
        e.cursor_col = 1;
        e.dedent_lines();
        assert_eq!(e.lines, vec!["pass".to_string()]);
        assert_eq!(e.cursor_col, 0);
    }

    #[test]
    fn shift_tab_dedents_every_line_a_selection_touches() {
        let mut e = editor_with("    a\n    b\n    c");
        e.lang = Some(LangKind::Python);
        e.cursor_row = 2;
        e.cursor_col = 5;
        e.selection = Some(EditorSelection {
            anchor: (0, 4),
            head: (2, 5),
        });
        e.dedent_lines();
        assert_eq!(
            e.lines,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(
            e.selection.unwrap(),
            EditorSelection {
                anchor: (0, 0),
                head: (2, 1),
            }
        );
    }

    #[test]
    fn tab_indents_every_line_a_multiline_selection_touches() {
        let mut e = editor_with("a\nb");
        e.lang = Some(LangKind::Python);
        e.cursor_row = 1;
        e.cursor_col = 1;
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (1, 1),
        });
        e.indent_lines();
        assert_eq!(e.lines, vec!["    a".to_string(), "    b".to_string()]);
        assert_eq!(
            e.selection.unwrap(),
            EditorSelection {
                anchor: (0, 4),
                head: (1, 5),
            }
        );
    }

    #[test]
    fn tab_indent_block_leaves_empty_lines_untouched() {
        let mut e = editor_with("a\n\nb");
        e.lang = Some(LangKind::Python);
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (2, 1),
        });
        e.indent_lines();
        assert_eq!(
            e.lines,
            vec!["    a".to_string(), String::new(), "    b".to_string()]
        );
    }

    #[test]
    fn tab_at_cursor_pads_to_the_next_tab_stop() {
        // Cursor at column 2 inserts two spaces to reach column 4, not four.
        let mut e = editor_with("ab");
        e.lang = Some(LangKind::Python);
        e.cursor_col = 2;
        e.indent_at_cursor();
        assert_eq!(e.lines, vec!["ab  ".to_string()]);
        assert_eq!(e.cursor_col, 4);
    }

    #[test]
    fn dedent_then_undo_restores_the_buffer_in_one_step() {
        let mut e = editor_with("        pass");
        e.lang = Some(LangKind::Python);
        e.cursor_col = 8;
        e.dedent_lines();
        assert_eq!(e.lines, vec!["    pass".to_string()]);
        assert!(e.undo());
        assert_eq!(e.lines, vec!["        pass".to_string()]);
    }

    #[test]
    fn redo_reapplies_an_undone_edit() {
        let mut e = editor_with("");
        e.insert_char('a');
        e.insert_char('b');
        assert_eq!(e.lines, vec!["ab".to_string()]);
        assert!(e.undo());
        assert_eq!(e.lines, vec![String::new()]);
        assert!(e.redo());
        assert_eq!(e.lines, vec!["ab".to_string()]);
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn redo_is_a_noop_on_an_empty_redo_stack() {
        let mut e = editor_with("x");
        assert!(!e.redo());
    }

    #[test]
    fn undo_and_redo_bump_edit_seq_so_the_app_resyncs() {
        // The app drives LSP did_change / git-gutter refresh off edit_seq; a
        // buffer restore must bump it or diagnostics go stale until the next
        // keystroke.
        let mut e = editor_with("");
        e.insert_char('a');
        let after_edit = e.edit_seq;
        assert!(e.undo());
        assert!(e.edit_seq > after_edit, "undo must bump edit_seq");
        let after_undo = e.edit_seq;
        assert!(e.redo());
        assert!(e.edit_seq > after_undo, "redo must bump edit_seq");
    }

    /// "Reopen with Encoding" swaps the whole buffer for a re-decode of the
    /// file on disk, like every other whole-buffer replacement — and like
    /// them it must clear BOTH history stacks: a redo popped after the swap
    /// used to reinstate the entire buffer as decoded under the OLD
    /// encoding (plus its dirty flag), silently discarding the re-decode.
    #[test]
    fn reopening_with_an_encoding_clears_both_history_stacks() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("enc.txt");
        // 0xC2 0xA2 decodes to one char as UTF-8 ("\u{a2}") and two as
        // Windows-1252 ("\u{c2}\u{a2}"), so the assertions below prove the
        // buffer really was re-decoded, not merely re-read.
        std::fs::write(&path, [0xC2_u8, 0xA2, b'\n']).unwrap();
        let mut e = Editor::new();
        e.open(&path).unwrap();
        assert_eq!(e.lines, vec!["\u{a2}".to_string()], "staging: UTF-8 decode");
        e.insert_char('x');
        assert!(e.undo());
        assert_eq!(e.lines, vec!["\u{a2}".to_string()], "staging: edit undone");
        e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
        assert_eq!(
            e.lines,
            vec!["\u{c2}\u{a2}".to_string()],
            "the buffer holds the Windows-1252 decode"
        );
        assert!(
            !e.redo(),
            "redo must not resurrect the buffer decoded under the old encoding"
        );
        assert!(
            !e.undo(),
            "undo history from before the re-decode is equally stale"
        );
        assert_eq!(e.lines, vec!["\u{c2}\u{a2}".to_string()]);
    }

    #[test]
    fn a_new_edit_after_undo_discards_the_redo_branch() {
        let mut e = editor_with("");
        e.insert_char('a');
        assert!(e.undo());
        assert_eq!(e.lines, vec![String::new()]);
        // A fresh edit branches history: the redo of "a" must be discarded.
        e.insert_char('b');
        assert!(!e.redo(), "a fresh edit must clear the redo stack");
        assert_eq!(e.lines, vec!["b".to_string()]);
    }

    #[test]
    fn insert_newline_rust_indents_after_open_brace() {
        let mut e = editor_with("fn main() {");
        e.lang = Some(LangKind::Rust);
        e.cursor_col = e.line_char_len(0);
        e.insert_newline();
        assert_eq!(e.lines, vec!["fn main() {".to_string(), "    ".to_string()]);
        assert_eq!(e.cursor_col, 4);
    }

    #[test]
    fn insert_newline_typescript_indents_after_open_paren() {
        let mut e = editor_with("foo(");
        e.lang = Some(LangKind::TypeScript);
        e.cursor_col = e.line_char_len(0);
        e.insert_newline();
        assert_eq!(e.lines, vec!["foo(".to_string(), "    ".to_string()]);
        assert_eq!(e.cursor_col, 4);
    }

    #[test]
    fn insert_newline_rust_bracket_pair_split_places_close_on_own_line() {
        let mut e = editor_with("fn main() {}");
        e.lang = Some(LangKind::Rust);
        e.cursor_col = 11;
        e.insert_newline();
        assert_eq!(
            e.lines,
            vec![
                "fn main() {".to_string(),
                "    ".to_string(),
                "}".to_string()
            ]
        );
        assert_eq!(e.cursor_row, 1);
        assert_eq!(e.cursor_col, 4);
    }

    #[test]
    fn insert_newline_rust_bracket_pair_split_preserves_outer_indent() {
        let mut e = editor_with("    let v = vec![];");
        e.lang = Some(LangKind::Rust);
        e.cursor_col = 17;
        e.insert_newline();
        assert_eq!(
            e.lines,
            vec![
                "    let v = vec![".to_string(),
                "        ".to_string(),
                "    ];".to_string(),
            ]
        );
        assert_eq!(e.cursor_row, 1);
        assert_eq!(e.cursor_col, 8);
    }

    #[test]
    fn insert_newline_python_bracket_pair_split_places_close_on_own_line() {
        let mut e = editor_with("foo()");
        e.lang = Some(LangKind::Python);
        e.cursor_col = 4;
        e.insert_newline();
        assert_eq!(
            e.lines,
            vec!["foo(".to_string(), "    ".to_string(), ")".to_string()]
        );
        assert_eq!(e.cursor_row, 1);
        assert_eq!(e.cursor_col, 4);
    }

    #[test]
    fn insert_newline_yaml_uses_two_space_indent() {
        let mut e = editor_with("  key:");
        e.lang = Some(LangKind::Yaml);
        e.cursor_col = e.line_char_len(0);
        e.insert_newline();
        assert_eq!(e.lines, vec!["  key:".to_string(), "  ".to_string()]);
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn backspace_mid_line() {
        let mut e = editor_with("abcd");
        e.cursor_col = 3;
        e.backspace();
        assert_eq!(e.lines[0], "abd");
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn backspace_at_col_zero_joins_with_previous_line() {
        let mut e = editor_with("hello\nworld");
        e.cursor_row = 1;
        e.cursor_col = 0;
        e.backspace();
        assert_eq!(e.lines, vec!["helloworld".to_string()]);
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 5);
    }

    #[test]
    fn backspace_at_origin_does_nothing_destructive() {
        let mut e = editor_with("abc");
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.backspace();
        assert_eq!(e.lines, vec!["abc".to_string()]);
    }

    #[test]
    fn delete_forward_mid_line() {
        let mut e = editor_with("abcd");
        e.cursor_col = 1;
        e.delete_forward();
        assert_eq!(e.lines[0], "acd");
        assert_eq!(e.cursor_col, 1);
    }

    #[test]
    fn delete_forward_at_end_joins_with_next_line() {
        let mut e = editor_with("hello\nworld");
        e.cursor_row = 0;
        e.cursor_col = 5;
        e.delete_forward();
        assert_eq!(e.lines, vec!["helloworld".to_string()]);
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 5);
    }

    #[test]
    fn buffer_pos_at_maps_cells_to_line_and_char() {
        let mut e = editor_with("fn main() {}\nlet x = 1;");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        assert_eq!(
            e.buffer_pos_at(3, 0),
            Some((0, 0)),
            "first text cell (text_x = x + gutter + 1 = 3) is char 0 of line 0"
        );
        assert_eq!(
            e.buffer_pos_at(6, 0),
            Some((0, 3)),
            "three cells into the text is char 3"
        );
        assert_eq!(
            e.buffer_pos_at(3, 1),
            Some((1, 0)),
            "the second screen row is the second buffer line"
        );
    }

    #[test]
    fn buffer_pos_at_returns_none_over_the_gutter() {
        let mut e = editor_with("hello");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        assert_eq!(e.buffer_pos_at(0, 0), None, "column 0 is in the gutter");
        assert_eq!(e.buffer_pos_at(2, 0), None, "still left of text_x");
    }

    #[test]
    fn buffer_pos_at_accounts_for_vertical_scroll() {
        let mut e = editor_with("a\nbb\nccc\ndddd\neeeee");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        e.scroll = 2;
        assert_eq!(
            e.buffer_pos_at(3, 0),
            Some((2, 0)),
            "the top visible row is buffer line scroll = 2"
        );
    }

    #[test]
    fn buffer_pos_at_accounts_for_horizontal_scroll() {
        let mut e = editor_with("abcdefghijklmnop");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        e.scroll_col = 5;
        assert_eq!(
            e.buffer_pos_at(3, 0),
            Some((0, 5)),
            "the leftmost text cell maps to char scroll_col, not char 0"
        );
        assert_eq!(
            e.buffer_pos_at(5, 0),
            Some((0, 7)),
            "two cells right of the left edge is char scroll_col + 2"
        );
    }

    #[test]
    fn buffer_pos_at_honours_pane_offset() {
        let mut e = editor_with("hello");
        e.last_inner = Rect {
            x: 10,
            y: 4,
            width: 40,
            height: 10,
        };
        e.last_gutter_width = 3;
        assert_eq!(e.buffer_pos_at(14, 4), Some((0, 0)), "text_x = 10 + 3 + 1");
        assert_eq!(e.buffer_pos_at(16, 4), Some((0, 2)));
        assert_eq!(e.buffer_pos_at(13, 4), None, "one cell left of text_x");
    }

    #[test]
    fn buffer_pos_at_returns_none_outside_viewport_and_past_content() {
        let mut e = editor_with("one\ntwo");
        e.last_inner = Rect {
            x: 0,
            y: 2,
            width: 80,
            height: 4,
        };
        e.last_gutter_width = 2;
        assert_eq!(e.buffer_pos_at(3, 1), None, "row above the pane");
        assert_eq!(e.buffer_pos_at(3, 6), None, "row below the pane");
        assert_eq!(
            e.buffer_pos_at(3, 4),
            None,
            "row maps past the last line of content"
        );
    }

    #[test]
    fn buffer_pos_at_clamps_past_end_of_line() {
        let mut e = editor_with("hi");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        assert_eq!(
            e.buffer_pos_at(40, 0),
            Some((0, 2)),
            "a cell past the line's text clamps to the line length"
        );
    }

    #[test]
    fn word_at_returns_identifier_bounds() {
        let e = editor_with("fn main() {}");
        assert_eq!(e.word_at(0, 0), Some((0, 2)), "fn");
        assert_eq!(e.word_at(0, 1), Some((0, 2)), "still within fn");
        assert_eq!(e.word_at(0, 3), Some((3, 7)), "main starts at char 3");
        assert_eq!(e.word_at(0, 6), Some((3, 7)), "last char of main");
    }

    #[test]
    fn word_at_returns_none_off_a_word() {
        let e = editor_with("fn main() {}");
        assert_eq!(e.word_at(0, 2), None, "the space between fn and main");
        assert_eq!(e.word_at(0, 7), None, "the open paren");
        assert_eq!(e.word_at(0, 99), None, "past the end of the line");
    }

    #[test]
    fn word_at_includes_underscores_and_digits() {
        let e = editor_with("let foo_bar2 = 1");
        assert_eq!(
            e.word_at(0, 4),
            Some((4, 12)),
            "foo_bar2 is a single identifier"
        );
        assert_eq!(
            e.word_at(0, 11),
            Some((4, 12)),
            "a trailing digit is part of the identifier"
        );
    }

    #[test]
    fn word_at_handles_missing_line() {
        let e = editor_with("only one line");
        assert_eq!(e.word_at(5, 0), None, "no such line");
    }

    #[test]
    fn page_down_advances_one_full_viewport_and_puts_first_unseen_line_at_top() {
        // Simulate a 100-line file with the editor's viewport rendering 25
        // lines. After PageDown the cursor should land on row 25 (line 26 in
        // 1-indexed terms) and that row should be the new top of the view.
        let mut e = editor_with_lines(100);
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        assert_eq!(e.scroll, 0);
        assert_eq!(e.cursor_row, 0);
        e.page_down_one_screen();
        assert_eq!(
            e.cursor_row, 25,
            "cursor should jump to first previously-unseen row"
        );
        assert_eq!(
            e.scroll, 25,
            "scroll should align with new cursor at top of viewport"
        );
    }

    #[test]
    fn page_down_repeats_advance_one_viewport_at_a_time() {
        let mut e = editor_with_lines(100);
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 20,
        };
        e.page_down_one_screen();
        e.page_down_one_screen();
        assert_eq!(e.cursor_row, 40);
        assert_eq!(e.scroll, 40);
    }

    #[test]
    fn page_down_clamps_at_end_of_file() {
        let mut e = editor_with_lines(30);
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        // Realistic state: scroll = 4 means rows 4..=28 are on screen, with
        // line 29 (cursor_row 28) visible at the bottom.
        e.scroll = 4;
        e.cursor_row = 28;
        e.page_down_one_screen();
        // 4 + 25 = 29 → last row.  Cursor and scroll land there.
        assert_eq!(e.cursor_row, 29);
        assert_eq!(e.scroll, 29);
    }

    #[test]
    fn page_up_rewinds_one_full_viewport() {
        let mut e = editor_with_lines(200);
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.scroll = 100;
        e.cursor_row = 100;
        e.page_up_one_screen();
        assert_eq!(e.cursor_row, 75);
        assert_eq!(e.scroll, 75);
    }

    #[test]
    fn page_up_clamps_at_top_of_file() {
        let mut e = editor_with_lines(50);
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.scroll = 5;
        e.cursor_row = 5;
        e.page_up_one_screen();
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.scroll, 0);
    }

    #[test]
    fn page_size_falls_back_when_viewport_is_unknown() {
        // Before the first render last_inner is zero-sized; PageDown should
        // still advance by some sensible default rather than no-op.
        let mut e = editor_with_lines(100);
        e.last_inner = Rect::default();
        e.page_down_one_screen();
        assert!(e.cursor_row > 0, "should advance even with zero last_inner");
    }

    fn editor_with_lines(n: usize) -> Editor {
        let mut e = Editor::new();
        e.lines = (0..n).map(|i| format!("line {i}")).collect();
        e
    }

    #[test]
    fn move_word_right_jumps_to_end_of_current_word() {
        let mut e = editor_with("hello world");
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.move_word_right();
        assert_eq!((e.cursor_row, e.cursor_col), (0, 5));
    }

    #[test]
    fn move_word_right_skips_intervening_whitespace_and_lands_at_end_of_next_word() {
        let mut e = editor_with("hello   world!!!end");
        e.cursor_row = 0;
        e.cursor_col = 5;
        e.move_word_right();
        assert_eq!(
            (e.cursor_row, e.cursor_col),
            (0, 13),
            "from end of 'hello' the next forward-word skips the spaces and stops at end of 'world'"
        );
        e.move_word_right();
        assert_eq!(
            (e.cursor_row, e.cursor_col),
            (0, 19),
            "second forward-word skips '!!!' and lands at end of 'end'"
        );
    }

    #[test]
    fn move_word_right_at_eol_advances_to_first_word_boundary_on_next_line() {
        let mut e = editor_with("foo\n  bar baz");
        e.cursor_row = 0;
        e.cursor_col = 3;
        e.move_word_right();
        assert_eq!((e.cursor_row, e.cursor_col), (1, 5));
    }

    #[test]
    fn move_word_right_at_eof_is_a_noop() {
        let mut e = editor_with("hi");
        e.cursor_row = 0;
        e.cursor_col = 2;
        e.move_word_right();
        assert_eq!((e.cursor_row, e.cursor_col), (0, 2));
    }

    #[test]
    fn move_word_right_treats_underscore_as_part_of_the_word() {
        let mut e = editor_with("snake_case rest");
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.move_word_right();
        assert_eq!(
            (e.cursor_row, e.cursor_col),
            (0, 10),
            "underscore is a word char so the whole identifier is one word"
        );
    }

    #[test]
    fn move_word_left_jumps_to_start_of_current_word() {
        let mut e = editor_with("hello world");
        e.cursor_row = 0;
        e.cursor_col = 11;
        e.move_word_left();
        assert_eq!((e.cursor_row, e.cursor_col), (0, 6));
    }

    #[test]
    fn move_word_left_skips_back_over_whitespace_and_punctuation() {
        let mut e = editor_with("hello   world!!!end");
        e.cursor_row = 0;
        e.cursor_col = 19;
        e.move_word_left();
        assert_eq!(
            (e.cursor_row, e.cursor_col),
            (0, 16),
            "from end of 'end' the backward-word lands at start of 'end'"
        );
        e.move_word_left();
        assert_eq!(
            (e.cursor_row, e.cursor_col),
            (0, 8),
            "next backward-word skips '!!!' and lands at start of 'world'"
        );
    }

    #[test]
    fn move_word_left_at_bol_jumps_to_end_of_previous_line() {
        let mut e = editor_with("foo bar\nbaz");
        e.cursor_row = 1;
        e.cursor_col = 0;
        e.move_word_left();
        assert_eq!(
            (e.cursor_row, e.cursor_col),
            (0, 4),
            "backward-word from start of line 1 should cross the boundary and land at start of 'bar' on line 0"
        );
    }

    #[test]
    fn move_word_left_at_bof_is_a_noop() {
        let mut e = editor_with("hi");
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.move_word_left();
        assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
    }

    #[test]
    fn duplicate_lines_down_with_no_selection_copies_the_current_line_below_and_moves_cursor_to_it()
    {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.cursor_row = 1;
        e.cursor_col = 2;
        e.duplicate_lines_down();
        assert_eq!(e.lines, vec!["alpha", "beta", "beta", "gamma"]);
        assert_eq!((e.cursor_row, e.cursor_col), (2, 2));
        assert!(e.dirty);
    }

    #[test]
    fn duplicate_lines_up_with_no_selection_copies_the_current_line_above_and_keeps_cursor_on_the_copy()
     {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.cursor_row = 1;
        e.cursor_col = 3;
        e.duplicate_lines_up();
        assert_eq!(e.lines, vec!["alpha", "beta", "beta", "gamma"]);
        assert_eq!(
            (e.cursor_row, e.cursor_col),
            (1, 3),
            "cursor stays at the same row index — that row is now the upper copy, the original was pushed to row 2"
        );
    }

    #[test]
    fn duplicate_lines_down_with_multiline_selection_duplicates_the_whole_block() {
        let mut e = editor_with("a\nb\nc\nd");
        e.cursor_row = 2;
        e.cursor_col = 1;
        e.selection = Some(EditorSelection {
            anchor: (1, 0),
            head: (2, 1),
        });
        e.duplicate_lines_down();
        assert_eq!(e.lines, vec!["a", "b", "c", "b", "c", "d"]);
        assert_eq!(
            (e.cursor_row, e.cursor_col),
            (4, 1),
            "cursor follows the duplicated block — it was at (2,1) over the original 'c', now it's at (4,1) over the duplicate"
        );
        assert_eq!(
            e.selection.unwrap(),
            EditorSelection {
                anchor: (3, 0),
                head: (4, 1),
            },
            "the selection migrates onto the duplicated block too so the next gesture acts on the copy"
        );
    }

    #[test]
    fn duplicate_lines_up_with_multiline_selection_keeps_the_cursor_on_the_upper_copy() {
        let mut e = editor_with("a\nb\nc\nd");
        e.cursor_row = 2;
        e.cursor_col = 0;
        e.selection = Some(EditorSelection {
            anchor: (1, 0),
            head: (2, 1),
        });
        e.duplicate_lines_up();
        assert_eq!(e.lines, vec!["a", "b", "c", "b", "c", "d"]);
        assert_eq!(
            (e.cursor_row, e.cursor_col),
            (2, 0),
            "the duplicate sits at rows 1-2 and the original was pushed down to rows 3-4; cursor at (2,0) is on the upper copy"
        );
    }

    #[test]
    fn duplicate_lines_down_then_undo_restores_the_buffer_in_one_step() {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.cursor_row = 1;
        e.cursor_col = 0;
        e.duplicate_lines_down();
        assert_eq!(e.lines.len(), 4);
        assert!(e.undo());
        assert_eq!(e.lines, vec!["alpha", "beta", "gamma"]);
        assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
    }

    #[test]
    fn duplicate_lines_up_at_first_row_inserts_the_copy_at_row_zero() {
        let mut e = editor_with("alpha\nbeta");
        e.cursor_row = 0;
        e.cursor_col = 4;
        e.duplicate_lines_up();
        assert_eq!(e.lines, vec!["alpha", "alpha", "beta"]);
        assert_eq!((e.cursor_row, e.cursor_col), (0, 4));
    }

    #[test]
    fn goto_top_lands_on_row_zero_col_zero() {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.cursor_row = 2;
        e.cursor_col = 4;
        e.goto_top();
        assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
    }

    #[test]
    fn goto_bottom_lands_on_last_row_col_zero() {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.cursor_row = 0;
        e.cursor_col = 3;
        e.goto_bottom();
        assert_eq!((e.cursor_row, e.cursor_col), (2, 0));
    }

    #[test]
    fn goto_line_uses_one_based_indexing_and_clamps_to_last_row() {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.goto_line(2);
        assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
        e.goto_line(999);
        assert_eq!((e.cursor_row, e.cursor_col), (2, 0));
        e.goto_line(0);
        assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
    }

    #[test]
    fn yank_lines_one_returns_current_line_with_trailing_newline() {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.cursor_row = 1;
        let yanked = e.yank_lines(1);
        assert_eq!(yanked, "beta\n");
        assert_eq!(e.lines, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn yank_lines_n_grabs_current_plus_below() {
        let mut e = editor_with("alpha\nbeta\ngamma\ndelta");
        e.cursor_row = 1;
        let yanked = e.yank_lines(3);
        assert_eq!(yanked, "beta\ngamma\ndelta\n");
    }

    #[test]
    fn delete_lines_one_drops_current_row() {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.cursor_row = 1;
        let yanked = e.delete_lines(1);
        assert_eq!(yanked, "beta\n");
        assert_eq!(e.lines, vec!["alpha", "gamma"]);
        assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
    }

    #[test]
    fn delete_lines_n_clamps_at_end_of_buffer() {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.cursor_row = 1;
        let yanked = e.delete_lines(99);
        assert_eq!(yanked, "beta\ngamma\n");
        assert_eq!(e.lines, vec!["alpha"]);
        assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
    }

    #[test]
    fn delete_lines_leaves_an_empty_line_when_buffer_emptied() {
        let mut e = editor_with("alpha\nbeta");
        e.cursor_row = 0;
        e.delete_lines(2);
        assert_eq!(e.lines, vec![""]);
        assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
    }

    #[test]
    fn delete_lines_is_undoable_in_one_step() {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.cursor_row = 1;
        e.delete_lines(1);
        assert!(e.undo());
        assert_eq!(e.lines, vec!["alpha", "beta", "gamma"]);
        assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
    }

    #[test]
    fn kill_to_bol_removes_head_of_current_line() {
        let mut e = editor_with("hello world\nnext");
        e.cursor_row = 0;
        e.cursor_col = 6;
        let killed = e.kill_to_bol();
        assert_eq!(killed, "hello ");
        assert_eq!(e.lines, vec!["world", "next"]);
        assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
    }

    #[test]
    fn kill_to_bol_at_col_zero_is_noop() {
        let mut e = editor_with("hello\nnext");
        e.cursor_row = 0;
        e.cursor_col = 0;
        let killed = e.kill_to_bol();
        assert_eq!(killed, "");
        assert_eq!(e.lines, vec!["hello", "next"]);
    }

    #[test]
    fn open_line_below_inserts_empty_line_after_current() {
        let mut e = editor_with("alpha\nbeta");
        e.cursor_row = 0;
        e.cursor_col = 3;
        e.open_line_below();
        assert_eq!(e.lines, vec!["alpha", "", "beta"]);
        assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
    }

    #[test]
    fn open_line_below_inherits_indent_from_current_row() {
        let mut e = editor_with("    alpha\nbeta");
        e.cursor_row = 0;
        e.cursor_col = 6;
        e.open_line_below();
        assert_eq!(e.lines, vec!["    alpha", "    ", "beta"]);
        assert_eq!((e.cursor_row, e.cursor_col), (1, 4));
    }

    #[test]
    fn open_line_above_inserts_empty_line_before_current() {
        let mut e = editor_with("alpha\nbeta");
        e.cursor_row = 1;
        e.cursor_col = 2;
        e.open_line_above();
        assert_eq!(e.lines, vec!["alpha", "", "beta"]);
        assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
    }

    #[test]
    fn open_line_above_inherits_indent_from_current_row() {
        let mut e = editor_with("alpha\n        beta");
        e.cursor_row = 1;
        e.cursor_col = 10;
        e.open_line_above();
        assert_eq!(e.lines, vec!["alpha", "        ", "        beta"]);
        assert_eq!((e.cursor_row, e.cursor_col), (1, 8));
    }

    #[test]
    fn kill_to_eol_removes_tail_of_current_line() {
        let mut e = editor_with("hello world\nnext");
        e.cursor_row = 0;
        e.cursor_col = 5;
        let killed = e.kill_to_eol();
        assert_eq!(killed, " world");
        assert_eq!(e.lines, vec!["hello", "next"]);
        assert_eq!((e.cursor_row, e.cursor_col), (0, 5));
    }

    #[test]
    fn kill_to_eol_at_eol_is_noop() {
        let mut e = editor_with("hello\nnext");
        e.cursor_row = 0;
        e.cursor_col = 5;
        let killed = e.kill_to_eol();
        assert_eq!(killed, "");
        assert_eq!(e.lines, vec!["hello", "next"]);
    }

    #[test]
    fn move_left_crosses_line_boundary() {
        let mut e = editor_with("abc\ndef");
        e.cursor_row = 1;
        e.cursor_col = 0;
        e.move_left();
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 3);
    }

    #[test]
    fn move_right_crosses_line_boundary() {
        let mut e = editor_with("abc\ndef");
        e.cursor_row = 0;
        e.cursor_col = 3;
        e.move_right();
        assert_eq!(e.cursor_row, 1);
        assert_eq!(e.cursor_col, 0);
    }

    #[test]
    fn move_up_clamps_column() {
        let mut e = editor_with("ab\nlongline");
        e.cursor_row = 1;
        e.cursor_col = 7;
        e.move_up();
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn home_and_end() {
        let mut e = editor_with("hello world");
        e.cursor_col = 5;
        e.home_line();
        assert_eq!(e.cursor_col, 0);
        e.end_line();
        assert_eq!(e.cursor_col, 11);
    }

    #[test]
    fn open_reads_file_and_splits_lines() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "alpha").unwrap();
        writeln!(tmp, "beta").unwrap();
        write!(tmp, "gamma").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert_eq!(
            e.lines,
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
        );
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 0);
        assert!(!e.dirty);
    }

    #[test]
    fn open_detects_crlf_and_save_preserves_it() {
        use std::io::Read;
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"alpha\r\nbeta\r\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert_eq!(e.eol, LineEnding::Crlf, "CRLF file detected");
        assert_eq!(e.lines, vec!["alpha".to_string(), "beta".to_string()]);
        e.save_to_disk().unwrap();
        let mut raw = Vec::new();
        std::fs::File::open(tmp.path())
            .unwrap()
            .read_to_end(&mut raw)
            .unwrap();
        assert_eq!(
            raw, b"alpha\r\nbeta",
            "save re-applies CRLF, no trailing EOL"
        );
    }

    #[test]
    fn open_defaults_to_lf_for_unix_files() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"a\nb\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert_eq!(e.eol, LineEnding::Lf);
    }

    #[test]
    fn reopen_with_encoding_decodes_windows_1252() {
        use std::io::Write as _;
        // 0xE9 is 'é' in Windows-1252 but invalid UTF-8 (renders as the
        // replacement char when first opened as UTF-8).
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&[b'c', b'a', b'f', 0xE9]).unwrap();
        tmp.flush().unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert_ne!(e.lines[0], "café", "UTF-8 decode mangles the 0xE9 byte");
        e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
        assert_eq!(e.lines[0], "café", "Windows-1252 decodes 0xE9 as é");
        assert_eq!(e.encoding, encoding_rs::WINDOWS_1252);
    }

    /// A same-path reload (the FS-sync sweep on a clean buffer, or an explicit
    /// revert) re-enters `open`, which auto-detects. Without a guard that
    /// throws away the encoding the user picked through "Reopen with
    /// Encoding", and the bytes get decoded as UTF-8 — so the buffer fills
    /// with replacement characters and the next save writes them over the
    /// original file.
    #[test]
    fn a_reload_keeps_the_encoding_the_user_reopened_with() {
        use std::io::Write as _;
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&[b'c', b'a', b'f', 0xE9]).unwrap();
        tmp.flush().unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
        // The file changes on disk, still Windows-1252 and still BOM-less.
        std::fs::write(tmp.path(), [b't', b'h', 0xE9]).unwrap();
        e.reload_if_clean().unwrap().unwrap();
        assert_eq!(
            e.encoding,
            encoding_rs::WINDOWS_1252,
            "a same-path reload must keep the encoding the user chose"
        );
        assert_eq!(e.lines[0], "thé", "and decode the new bytes with it");
    }

    /// A UTF-16 file must survive the whole round trip: open, edit, save,
    /// open again. Two things blocked it. `Encoding::encode` is decode-only
    /// for UTF-16 per the WHATWG spec, so it silently emitted UTF-8; and
    /// `is_binary` runs BEFORE the BOM sniff, so UTF-16's NUL bytes were
    /// rejected as binary — meaning croft could offer UTF-16 in the encoding
    /// picker and then never read back what it wrote.
    #[test]
    fn a_utf16_file_survives_open_edit_save_open() {
        let tmp = NamedTempFile::new().unwrap();
        let mut bytes = vec![0xFF, 0xFE]; // UTF-16LE BOM
        for u in "hi\nthere\n".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(tmp.path(), &bytes).unwrap();

        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert_eq!(e.encoding, encoding_rs::UTF_16LE, "opened as UTF-16LE");
        assert_eq!(e.lines[0], "hi");
        assert_eq!(e.lines[1], "there");
        e.cursor_col = 2;
        e.insert_char('!');
        e.save_to_disk().unwrap();

        let out = std::fs::read(tmp.path()).unwrap();
        assert_eq!(&out[..2], &[0xFF, 0xFE], "the BOM must survive");
        let mut e2 = Editor::new();
        e2.open(tmp.path())
            .expect("croft must be able to reopen what it just wrote");
        assert_eq!(e2.encoding, encoding_rs::UTF_16LE);
        assert_eq!(e2.lines[0], "hi!", "the edit round-tripped");
        assert_eq!(e2.lines[1], "there");
    }

    /// UTF-16 is self-describing only via its BOM: without one, the bytes are
    /// NUL-laden and no tool (croft included) can tell them from binary. VS
    /// Code always writes it for its UTF-16 entries, so a file that never had
    /// one still gets one when saved as UTF-16.
    #[test]
    fn saving_as_utf16_always_writes_a_bom() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "plain\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert!(!e.bom, "the source file had no BOM");
        e.encoding = encoding_rs::UTF_16BE;
        e.dirty = true;
        e.save_to_disk().unwrap();
        let out = std::fs::read(tmp.path()).unwrap();
        assert_eq!(&out[..2], &[0xFE, 0xFF], "UTF-16BE BOM written anyway");
        let mut e2 = Editor::new();
        e2.open(tmp.path()).expect("and it reopens");
        assert_eq!(e2.lines[0], "plain");
    }

    /// A UTF-8 BOM is stripped by `decode` and nothing re-emitted it, so every
    /// save quietly dropped it.
    #[test]
    fn saving_keeps_a_utf8_bom() {
        let tmp = NamedTempFile::new().unwrap();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hi\n");
        std::fs::write(tmp.path(), &bytes).unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert_eq!(e.lines[0], "hi");
        e.cursor_col = 2;
        e.insert_char('!');
        e.save_to_disk().unwrap();
        let out = std::fs::read(tmp.path()).unwrap();
        assert_eq!(&out[..3], &[0xEF, 0xBB, 0xBF], "the UTF-8 BOM must survive");
        assert_eq!(&out[3..6], b"hi!");
    }

    #[test]
    fn set_language_updates_the_label() {
        let mut e = editor_with("x = 1");
        e.set_language(Some(crate::highlight::LangKind::Python));
        assert_eq!(e.language_label(), "Python");
        e.set_language(None);
        assert_eq!(e.language_label(), "Plain Text");
    }

    #[test]
    fn open_splits_on_lone_cr_to_match_lsp_line_numbering() {
        // A file with a stray lone `\r` (mixed line endings). The LSP/VS Code
        // treat `\r`, `\r\n`, and `\n` all as line breaks, so its token
        // positions count this as a line boundary. Rust's `str::lines` only
        // breaks on `\n`, so croft must split the same way or every LSP
        // position past the stray CR lands one row off.
        let mut tmp = NamedTempFile::new().unwrap();
        // "a" <LF> "" (the lone CR) "b" <CRLF> "c"
        write!(tmp, "a\n\rb\r\nc").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert_eq!(
            e.lines,
            vec![
                "a".to_string(),
                String::new(),
                "b".to_string(),
                "c".to_string()
            ],
            "lone CR and CRLF must each split a line, matching the LSP"
        );
    }

    #[test]
    fn open_png_populates_image_view_and_skips_text_buffer() {
        // 1×1 transparent PNG, hand-crafted via the image crate.
        let img: image::RgbaImage = image::ImageBuffer::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        let mut buf: Vec<u8> = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pic.png");
        std::fs::write(&path, &buf).unwrap();
        let mut e = Editor::new();
        e.open(&path).unwrap();
        let img = e.image.as_ref().expect("image-mode tab");
        assert_eq!(img.pixel_w, 1);
        assert_eq!(img.pixel_h, 1);
        assert_eq!(img.format_label, "PNG");
        // Text scaffolding should be inert.
        assert_eq!(e.lines, vec![String::new()]);
        assert!(e.path.is_some());
        assert!(e.lang.is_none());
        assert!(!e.dirty);
    }

    /// A file rebuilt on disk reloads to the same path (and, for a PDF, the
    /// same page and rect) - the overlay's re-emit key tells the loads apart
    /// only through the content generation, so every reload must stamp a
    /// fresh one even when the bytes happen to be identical.
    #[test]
    fn a_reloaded_image_carries_a_fresh_generation() {
        let img: image::RgbaImage = image::ImageBuffer::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        let mut buf: Vec<u8> = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pic.png");
        std::fs::write(&path, &buf).unwrap();
        let mut e = Editor::new();
        e.open(&path).unwrap();
        let first = e.image.as_ref().unwrap().generation;
        e.open(&path).unwrap();
        let second = e.image.as_ref().unwrap().generation;
        assert_ne!(
            first, second,
            "a reload must stamp fresh content, or the baked overlay stays stale"
        );
    }

    /// The scrollbar is drawn against `lines + comment-box rows`, so the
    /// drag must map through the same content length. Mapping through bare
    /// `lines.len()` made the bar of a short file with a tall navigator
    /// comment completely dead (metrics said "no overflow"), and made a
    /// long file's thumb run away from the pointer.
    #[test]
    fn dragging_the_scrollbar_of_a_file_with_a_comment_box_scrolls() {
        let mut e = editor_with("a\nb\nc\nd\ne");
        e.comment_boxes.push(CommentBox {
            id: 1,
            line: 1,
            author: String::from("navigator"),
            body: (0..20)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        });
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        assert!(
            e.last_scrollbar.width > 0,
            "the box overflows the viewport, so a bar must be drawn"
        );
        assert!(
            e.scroll_to_bar_y(e.last_scrollbar.y + e.last_scrollbar.height / 2),
            "a drag on the drawn bar must scroll, not be refused as no-overflow"
        );
    }

    /// End means "last page", which is unanswerable when the page count is
    /// unknown (sips-only Macs where mdls reports nothing): the sentinel
    /// must not be rendered literally, which sprayed
    /// "PDF page 4294967295 failed" into the status bar.
    #[test]
    fn end_with_an_unknown_page_count_reports_instead_of_rendering_the_sentinel() {
        let mut e = Editor::new();
        e.image = Some(ImageView {
            bytes: Vec::new(),
            format_label: String::from("PDF"),
            pixel_w: 1,
            pixel_h: 1,
            byte_size: 0,
            generation: 0,
            pdf: Some(PdfState {
                source_path: PathBuf::from("/nonexistent/doc.pdf"),
                current_page: 1,
                page_count: None,
                backend: crate::pdf::PdfBackend::SipsCli,
                source_byte_size: 0,
                links: None,
            }),
        });
        assert!(!e.set_pdf_page(u32::MAX));
        assert!(
            !e.status.contains("4294967295"),
            "the sentinel page must not leak into the status: {}",
            e.status
        );
        assert_eq!(e.pdf_page(), Some(1), "the page must not move");
    }

    #[test]
    fn extension_is_image_recognises_common_formats() {
        for ext in ["png", "PNG", "jpg", "jpeg", "JPEG", "gif", "bmp", "webp"] {
            assert!(extension_is_image(ext), "should recognise: {ext}");
        }
        for ext in ["txt", "rs", "md", "py", ""] {
            assert!(!extension_is_image(ext), "should not recognise: {ext}");
        }
    }

    #[test]
    fn open_unrecognised_file_after_image_clears_image_view() {
        // Open an image, then a text file in the same Editor — `image`
        // must reset so the text-rendering path comes back.
        let img: image::RgbaImage = image::ImageBuffer::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        let mut buf: Vec<u8> = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let img_path = dir.path().join("pic.png");
        std::fs::write(&img_path, &buf).unwrap();
        let txt_path = dir.path().join("hi.txt");
        std::fs::write(&txt_path, "hi\n").unwrap();
        let mut e = Editor::new();
        e.open(&img_path).unwrap();
        assert!(e.image.is_some());
        e.open(&txt_path).unwrap();
        assert!(e.image.is_none());
        assert_eq!(e.lines, vec!["hi".to_string()]);
    }

    #[test]
    fn open_routes_binary_files_to_hex_not_text() {
        // The pre-#172 contract was a "Binary file" ERROR; the new one is
        // that binary content never lands in the text path — it routes to
        // the hex viewer, and no text state is populated.
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"\x00\x01\x02binary garbage").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert!(e.hex.is_some(), "binary routes to hex");
        assert!(!e.dirty);
        assert_eq!(e.lines, vec![String::new()], "text path untouched");
    }

    #[test]
    fn matches_open_path_handles_canonical_difference() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "x").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert!(e.matches_open_path(tmp.path()));
        let bogus = std::path::Path::new("/definitely/not/the/same/path.txt");
        assert!(!e.matches_open_path(bogus));
    }

    #[test]
    fn reload_if_clean_picks_up_external_changes() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "def hello():").unwrap();
        writeln!(tmp, "    print(\"hi\")").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert!(e.lines[0].contains("hello"));

        // Simulate an external edit (vim, git pull, etc.).
        std::fs::write(tmp.path(), "def hi():\n    print(\"hi\")\n").unwrap();
        let outcome = e.reload_if_clean();
        assert!(matches!(outcome, Some(Ok(()))));
        assert_eq!(e.lines[0], "def hi():");
        assert!(!e.dirty);
    }

    #[test]
    fn external_reload_bumps_edit_seq_and_drops_stale_semantic_overlay() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "def hello():\n    pass\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        let seq_before = e.edit_seq;
        let legend = std::sync::Arc::new(vec![String::from("function")]);
        e.apply_semantic_tokens(tmp.path().to_path_buf(), vec![0, 4, 5, 0, 0], legend, true);
        assert!(e.semantic_overlay_for_test().iter().any(|l| !l.is_empty()));

        std::fs::write(tmp.path(), "x = 1\n").unwrap();
        assert!(matches!(
            e.reload_or_flag_conflict(),
            ExternalChange::Reloaded
        ));
        // The LSP doc resync is keyed on edit_seq; if the reload doesn't move
        // it, the server never hears about the new content and never sends
        // fresh semantic tokens.
        assert_ne!(e.edit_seq, seq_before);
        // The old token batch was measured against the old text; decoding it
        // over the new lines paints wrong colors, so it must be dropped.
        assert!(e.semantic_overlay_for_test().iter().all(|l| l.is_empty()));
    }

    #[test]
    fn reload_if_clean_refuses_when_dirty() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "original\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.insert_str("local edit");
        assert!(e.dirty);

        std::fs::write(tmp.path(), "external change\n").unwrap();
        let outcome = e.reload_if_clean();
        assert!(
            outcome.is_none(),
            "should refuse to reload over dirty buffer"
        );
        assert!(e.lines[0].contains("local edit"));
    }

    #[test]
    fn save_round_trips_content() {
        let tmp = NamedTempFile::new().unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.insert_str("hello\nworld");
        assert!(e.dirty);
        e.save_to_disk().unwrap();
        assert!(!e.dirty);
        let written = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(written, "hello\nworld");
    }

    #[test]
    fn dirty_flag_lifecycle() {
        let mut e = editor_with("abc");
        assert!(!e.dirty);
        e.insert_char('z');
        assert!(e.dirty);
    }

    #[test]
    fn switching_a_preview_tab_to_an_image_drops_the_stale_conflict_cache() {
        // open_image swaps the whole buffer; a conflict list memoised on
        // edit_seq from the previous text file must not survive, or a
        // palette merge-resolve slices a one-line buffer with stale
        // indices and panics.
        let tmp = tempfile::tempdir().unwrap();
        let mut e = Editor::new();
        e.lines = vec![
            "<<<<<<< HEAD".to_string(),
            "ours".to_string(),
            "=======".to_string(),
            "theirs".to_string(),
            ">>>>>>> branch".to_string(),
        ];
        e.mark_buffer_changed();
        assert_eq!(e.conflicts().len(), 1, "the conflict parses");
        let png = tmp.path().join("p.png");
        image::RgbaImage::new(1, 1).save(&png).unwrap();
        e.open_image(&png).unwrap();
        assert!(
            e.conflicts().is_empty(),
            "an image tab has no merge conflicts; a stale cache here is a panic waiting in resolution_lines"
        );
    }

    #[test]
    fn undo_past_a_save_re_dirties_the_buffer() {
        // Auto save writes one second after a keystroke; Cmd+Z then restores
        // the pre-edit snapshot with dirty:false while disk keeps the edit.
        // dirty means "buffer differs from disk", so crossing a save point
        // backwards must re-dirty or the divergence is permanent and silent.
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "hello\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.insert_char('x');
        assert!(matches!(e.save_to_disk(), Ok(SaveOutcome::Saved)));
        assert!(!e.dirty);
        assert!(e.undo());
        assert!(
            e.dirty,
            "the undone buffer no longer matches disk and must say so"
        );
    }

    #[test]
    fn a_save_breaks_insert_coalescing() {
        // Typing after a save must open a new undo step: coalescing across
        // the save point makes one Cmd+Z discard work from both sides of it.
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.insert_char('x');
        assert!(matches!(e.save_to_disk(), Ok(SaveOutcome::Saved)));
        e.insert_char('y');
        assert!(e.undo());
        assert_eq!(
            e.lines[0], "x",
            "undo steps back to the save point, not past it"
        );
    }

    #[test]
    fn a_replacement_carriage_return_does_not_survive_in_a_line() {
        let mut e = Editor::new();
        e.lines = vec!["abc".to_string()];
        e.replace_find_match(0, 1, 1, "x\r\ny");
        assert_eq!(e.lines, vec!["ax".to_string(), "yc".to_string()]);
        assert!(e.lines.iter().all(|l| !l.contains('\r')));
    }

    #[test]
    fn a_multi_line_replacement_splices_into_separate_lines() {
        // A regex replacement containing a real newline (VS Code's \n) must
        // become two buffer lines, never an embedded control char in one.
        let mut e = Editor::new();
        e.lines = vec!["abc".to_string()];
        e.replace_find_match(0, 1, 1, "x\ny");
        assert_eq!(e.lines, vec!["ax".to_string(), "yc".to_string()]);
        assert_eq!(
            (e.cursor_row, e.cursor_col),
            (1, 1),
            "caret after the inserted text"
        );
    }

    #[test]
    fn an_external_reload_moves_the_edit_seq_past_an_in_flight_token_request() {
        // Semantic-token replies now echo the `edit_seq` their request was
        // fired for, and the app drops a reply whose seq no longer matches.
        // That guard is only meaningful while a reload actually MOVES the
        // seq: drop the bump and a batch computed against the old text
        // matches again, paints the new buffer at the old offsets, and gets
        // persisted to the on-disk cache under the new content's key.
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "fn a() {}\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.apply_semantic_tokens(
            tmp.path().to_path_buf(),
            vec![0, 0, 2, 0, 0],
            std::sync::Arc::new(vec!["keyword".to_string()]),
            true,
        );
        let in_flight = e.edit_seq;
        std::fs::write(tmp.path(), "fn bbbb() {}\nfn c() {}\n").unwrap();
        e.reload_from_disk().unwrap();
        assert_ne!(
            e.edit_seq, in_flight,
            "a reload must invalidate every in-flight token request"
        );
        assert!(
            e.semantic_data.is_empty(),
            "and drop the batch measured against the old text"
        );
    }

    /// `open` also serves the same-path reload, so clearing the fold set there
    /// unconditionally made every FS-sync sweep pop the user's blocks open:
    /// a `git checkout`, an external formatter, or another split saving the
    /// file. Only a DIFFERENT file arriving in this tab invalidates them.
    #[test]
    fn an_external_reload_of_the_same_file_keeps_its_folds() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "def f():\n    a\n    b\ntail\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.toggle_fold(0);
        assert!(e.is_line_hidden(1), "the block starts collapsed");
        // An external rewrite of the same shape: same line count, same length,
        // so only the mtime moves.
        std::fs::write(tmp.path(), "def f():\n    A\n    B\ntail\n").unwrap();
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
        std::fs::OpenOptions::new()
            .write(true)
            .open(tmp.path())
            .unwrap()
            .set_modified(newer)
            .unwrap();
        assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
        assert_eq!(e.lines[1], "    A", "the reload landed");
        assert!(
            e.is_line_hidden(1),
            "the fold must survive a reload of the file it was set on"
        );
    }

    /// Keeping `folded` across a same-path reload is only half the job: the
    /// cached spans were measured against the OLD text. A reload that keeps the
    /// line count but moves the indentation (an external formatter, a
    /// `git checkout`) slips past the render-time `fold_epoch_lines` guard, so
    /// the stale cache goes on hiding lines the fold no longer covers.
    #[test]
    fn a_same_path_reload_remeasures_the_fold_spans() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "def f():\n    a\n    b\ntail\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.toggle_fold(0);
        assert!(e.is_line_hidden(1), "the block starts collapsed");
        // Same line count, same length: only the indentation moved, so the
        // header no longer has a body to fold.
        std::fs::write(tmp.path(), "def f():\na    \nb    \ntail\n").unwrap();
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
        std::fs::OpenOptions::new()
            .write(true)
            .open(tmp.path())
            .unwrap()
            .set_modified(newer)
            .unwrap();
        assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
        assert_eq!(e.lines[1], "a    ", "the reload landed");
        assert!(
            !e.is_line_hidden(1),
            "a line the fold no longer covers must not stay hidden"
        );
    }

    #[test]
    fn reload_or_flag_conflict_reloads_clean_buffer() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "old\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        // External edit grows the file so mtime and len both move.
        std::fs::write(tmp.path(), "new content here\n").unwrap();
        assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
        assert_eq!(e.lines[0], "new content here");
        assert!(!e.disk_conflict);
        // A second sweep with no further change is a no-op.
        assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Unchanged);
    }

    #[test]
    fn reload_or_flag_conflict_flags_dirty_buffer_without_clobbering() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "original\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.insert_str("my unsaved edit");
        std::fs::write(tmp.path(), "external change\n").unwrap();
        assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Conflict);
        assert!(
            e.lines[0].contains("my unsaved edit"),
            "dirty buffer must NOT be reloaded over"
        );
        assert!(e.disk_conflict);
    }

    #[test]
    fn reload_or_flag_conflict_fires_once_then_stays_quiet() {
        // A dirty buffer whose disk keeps differing must announce the conflict
        // exactly once so the confirm popup doesn't reopen on every FS poll.
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "original\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.insert_str("my unsaved edit");
        std::fs::write(tmp.path(), "external change\n").unwrap();
        assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Conflict);
        // Disk still differs and the buffer is still dirty, but the conflict
        // has already been announced.
        assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Unchanged);
        assert!(e.disk_conflict);
    }

    #[test]
    fn revert_paths_to_disk_discards_edits_and_reloads() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "original\n").unwrap();
        let mut tabs = EditorTabs::new();
        tabs.open_pinned(tmp.path()).unwrap();
        tabs.insert_str("my unsaved edit"); // deref to the active editor
        std::fs::write(tmp.path(), "external change\n").unwrap();
        let reverted = tabs.revert_paths_to_disk(&[tmp.path().to_path_buf()]);
        assert_eq!(reverted, vec![tmp.path().to_path_buf()]);
        assert_eq!(tabs.lines[0], "external change");
        assert!(!tabs.dirty);
        assert!(!tabs.disk_conflict);
    }

    #[test]
    fn reload_or_flag_conflict_unchanged_when_disk_matches() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "stable\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Unchanged);
    }

    #[test]
    fn save_refuses_to_clobber_external_change_then_force_overwrites() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "original\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.insert_str("local");
        // Someone else writes the file after we opened it.
        std::fs::write(tmp.path(), "theirs\n").unwrap();
        assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::DiskConflict);
        assert_eq!(
            std::fs::read_to_string(tmp.path()).unwrap(),
            "theirs\n",
            "a guarded save must NOT clobber the external change"
        );
        assert!(e.disk_conflict);
        // Explicit force overwrites with our buffer and clears the conflict.
        e.save_to_disk_force().unwrap();
        assert!(
            std::fs::read_to_string(tmp.path())
                .unwrap()
                .contains("local")
        );
        assert!(!e.disk_conflict);
    }

    /// A legacy encoding that cannot represent the buffer must never write
    /// silently: `encoding_rs` substitutes HTML numeric character references
    /// (NOT `?`), so the file ends up holding ASCII text that no longer
    /// round-trips, while the buffer still shows the originals.
    #[test]
    fn saving_text_the_encoding_cannot_represent_is_refused_not_silently_mangled() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "cafe\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
        // € lives at 0x80 in Windows-1252, so only the CJK is unmappable.
        e.lines = vec![String::from("日本語 costs 5€")];
        e.dirty = true;
        assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::EncodingLoss);
        assert_eq!(
            std::fs::read_to_string(tmp.path()).unwrap(),
            "cafe\n",
            "a refused save must leave the file untouched"
        );
        assert!(e.encoding_loss, "the refusal latches so auto save skips it");
        assert_eq!(e.unmappable_chars(), vec!['日', '本', '語']);
        // Explicit consent (the second Cmd+S) writes the replacements.
        e.lossy_save_armed = true;
        assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
        assert_eq!(
            std::fs::read(tmp.path()).unwrap(),
            b"&#26085;&#26412;&#35486; costs 5\x80",
            "consent writes encoding_rs' references, and € as the 0x80 byte"
        );
        assert!(!e.encoding_loss);
        assert!(!e.lossy_save_armed, "consent is one-shot");
    }

    /// The refusal must not fire for a buffer the encoding covers, and must
    /// not fire at all for UTF-8/UTF-16, which cover everything.
    #[test]
    fn a_representable_buffer_saves_without_a_prompt() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "cafe\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
        e.lines = vec![String::from("café")];
        assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
        assert_eq!(std::fs::read(tmp.path()).unwrap(), b"caf\xE9");
        assert!(e.unmappable_chars().is_empty());
        // The same text in UTF-8 is always representable.
        let mut u = Editor::new();
        u.open(tmp.path()).unwrap();
        u.lines = vec![String::from("日本語")];
        assert_eq!(u.encoding, encoding_rs::UTF_8);
        assert_eq!(u.save_to_disk().unwrap(), SaveOutcome::Saved);
        assert!(u.unmappable_chars().is_empty());
    }

    /// Switching the buffer to an encoding that covers it must clear the
    /// latch, or auto save would stay wedged on a file that is now fine.
    #[test]
    fn reopening_with_another_encoding_clears_the_encoding_loss_latch() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "日本語").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.encoding = encoding_rs::WINDOWS_1252;
        assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::EncodingLoss);
        assert!(e.encoding_loss);
        e.reopen_with_encoding(encoding_rs::UTF_8).unwrap();
        assert!(!e.encoding_loss);
        assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    }

    /// Armed consent covers the buffer the prompt described. An edit changes
    /// what the write would destroy, so it must revoke the stale consent (and
    /// the auto-save latch) and make the next save prompt afresh — otherwise
    /// characters typed after the prompt get mangled under a consent that
    /// never named them.
    #[test]
    fn an_edit_after_a_lossy_refusal_revokes_the_armed_consent() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "cafe\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
        e.lines = vec![String::from("日")];
        e.dirty = true;
        assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::EncodingLoss);
        e.lossy_save_armed = true; // the app arms the second Cmd+S...
        e.insert_char('本'); // ...but the user edits instead of pressing it
        assert!(!e.lossy_save_armed, "an edit revokes the stale consent");
        assert!(
            !e.encoding_loss,
            "the latch clears too, so a fixed-up buffer resumes auto save"
        );
        assert_eq!(
            e.save_to_disk().unwrap(),
            SaveOutcome::EncodingLoss,
            "the next save must prompt again for the changed buffer"
        );
        assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), "cafe\n");
    }

    #[test]
    fn a_failed_external_reload_is_reported_not_swallowed() {
        // #37: `ReloadFailed` was dropped on the floor — the user's file is
        // replaced on disk by something unreadable (here: a directory), the
        // sweep tries and fails to reload, and nothing said so.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "one\n").unwrap();
        let mut tabs = EditorTabs::new();
        tabs.open_pinned(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let report = tabs.reload_externally_changed_tabs(&|_| false);
        assert_eq!(
            report.failed,
            vec![path.clone()],
            "the failure must surface in the report"
        );
        assert!(!report.is_empty(), "a failed reload is not an empty sweep");
        assert_eq!(
            tabs.iter_tabs().next().unwrap().lines[0],
            "one",
            "the last good buffer survives the failed reload"
        );
    }

    #[test]
    fn reload_externally_changed_tabs_reloads_background_tab() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "a old\n").unwrap();
        std::fs::write(&b, "b old\n").unwrap();
        let mut tabs = EditorTabs::new();
        tabs.open_pinned(&a).unwrap(); // tab for a
        tabs.open_pinned(&b).unwrap(); // tab for b is now active; a is in the background
        // External edit to the BACKGROUND file. The new content is the same
        // length as the old ("a old\n" / "a NEW\n" are both 6 bytes), so the
        // change is detectable only by mtime — and two back-to-back writes can
        // land in the same mtime tick on a coarse-granularity filesystem,
        // making detection flaky. Force a distinctly-newer mtime so the test is
        // deterministic across filesystems (the production path keys off this
        // same (mtime, len) stamp).
        std::fs::write(&a, "a NEW\n").unwrap();
        let bumped = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&a)
            .unwrap()
            .set_modified(bumped)
            .unwrap();
        let report = tabs.reload_externally_changed_tabs(&|_| false);
        assert_eq!(report.reloaded, vec![a.clone()]);
        assert!(report.conflicts.is_empty());
        // A skipped path (a collab guest's shared file) is left untouched.
        std::fs::write(&b, "b NEW\n").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&b)
            .unwrap()
            .set_modified(bumped)
            .unwrap();
        let report = tabs.reload_externally_changed_tabs(&|p| p == b.as_path());
        assert!(report.reloaded.is_empty());
        let b_tab = tabs
            .iter_tabs()
            .find(|e| e.path.as_deref() == Some(b.as_path()))
            .unwrap();
        assert_eq!(b_tab.lines[0], "b old");
        let a_tab = tabs
            .iter_tabs()
            .find(|e| e.path.as_deref() == Some(a.as_path()))
            .unwrap();
        assert_eq!(a_tab.lines[0], "a NEW");
    }

    /// The field gesture behind the pdftoppm stderr fix: a PDF is open while
    /// pdflatex rewrites it. Mid-write the file is truncated garbage - the
    /// FS-sync sweep must fail quietly and keep the last good page, then
    /// reload once the write completes.
    #[test]
    fn open_pdf_tab_keeps_last_good_page_through_a_mid_write_sweep() {
        if crate::pdf::detect_backend().is_none() {
            eprintln!("skipping: no PDF rasteriser installed");
            return;
        }
        let one_page_pdf = concat!(
            "%PDF-1.4\n",
            "1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n",
            "2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n",
            "3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R>>endobj\n",
            "4 0 obj<</Length 27>>stream\n0 0 1 rg 10 10 100 100 re f\nendstream endobj\n",
            "trailer<</Root 1 0 R/Size 5>>\n%%EOF\n",
        );
        let bump_mtime = |path: &Path, secs: u64| {
            let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(secs);
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(newer)
                .unwrap();
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presentation.pdf");
        std::fs::write(&path, one_page_pdf).unwrap();
        let mut tabs = EditorTabs::new();
        tabs.open_pinned(&path).unwrap();
        let good_bytes = {
            let tab = tabs.iter_tabs().next().unwrap();
            tab.image
                .as_ref()
                .expect("PDF tab renders a page")
                .bytes
                .clone()
        };
        // pdflatex truncates and starts rewriting the file.
        std::fs::write(&path, b"%PDF-1.7\ntruncated mid-write, no xref").unwrap();
        bump_mtime(&path, 2);
        let report = tabs.reload_externally_changed_tabs(&|_| false);
        assert!(
            report.reloaded.is_empty(),
            "a failed render is not a reload"
        );
        assert!(
            report.conflicts.is_empty(),
            "a clean tab is never a conflict"
        );
        let tab = tabs.iter_tabs().next().unwrap();
        assert_eq!(
            tab.image
                .as_ref()
                .expect("last good page must survive")
                .bytes,
            good_bytes,
            "mid-write failure must keep showing the last good render"
        );
        // The write completes; the next sweep picks it up.
        std::fs::write(&path, one_page_pdf).unwrap();
        bump_mtime(&path, 4);
        let report = tabs.reload_externally_changed_tabs(&|_| false);
        assert_eq!(report.reloaded, vec![path.clone()]);
        let tab = tabs.iter_tabs().next().unwrap();
        assert!(tab.image.is_some(), "completed write renders again");
    }

    /// Reading page 2 of a deck while pdflatex rebuilds the file: the
    /// FS-sync reload must come back on page 2, not snap the reader to
    /// page 1 and lose their place.
    #[test]
    fn pdf_reload_keeps_the_current_page() {
        if crate::pdf::detect_backend().is_none() {
            eprintln!("skipping: no PDF rasteriser installed");
            return;
        }
        let two_page_pdf = concat!(
            "%PDF-1.4\n",
            "1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n",
            "2 0 obj<</Type/Pages/Kids[3 0 R 5 0 R]/Count 2>>endobj\n",
            "3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R>>endobj\n",
            "4 0 obj<</Length 27>>stream\n0 0 1 rg 10 10 100 100 re f\nendstream endobj\n",
            "5 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 6 0 R>>endobj\n",
            "6 0 obj<</Length 27>>stream\n1 0 0 rg 10 10 100 100 re f\nendstream endobj\n",
            "trailer<</Root 1 0 R/Size 7>>\n%%EOF\n",
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.pdf");
        std::fs::write(&path, two_page_pdf).unwrap();
        let mut tabs = EditorTabs::new();
        tabs.open_pinned(&path).unwrap();
        assert!(tabs.change_pdf_page(1), "two pages, so page 2 renders");
        assert_eq!(tabs.pdf_page(), Some(2));
        // pdflatex rewrites the file (same content is enough; only the
        // disk stamp needs to move).
        std::fs::write(&path, two_page_pdf).unwrap();
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(newer)
            .unwrap();
        let report = tabs.reload_externally_changed_tabs(&|_| false);
        assert_eq!(report.reloaded, vec![path.clone()]);
        assert_eq!(
            tabs.pdf_page(),
            Some(2),
            "an external rebuild must not move the reader back to page 1; status: {:?}",
            tabs.iter_tabs().next().unwrap().status
        );
    }

    /// The transient arm of the rebuild race (#72): the reader is on page 2,
    /// the file is rebuilt, and the rasterisation of THEIR page fails once
    /// (a spawn refused under load). That failure must flow into the
    /// failed-reload path — last good render, place kept — never leave a
    /// half-restored tab sitting on page 1. The next sweep recovers.
    #[test]
    fn a_transient_page_render_failure_during_reload_keeps_the_readers_place() {
        if crate::pdf::detect_backend().is_none() {
            eprintln!("skipping: no PDF rasteriser installed");
            return;
        }
        let two_page_pdf = concat!(
            "%PDF-1.4\n",
            "1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n",
            "2 0 obj<</Type/Pages/Kids[3 0 R 5 0 R]/Count 2>>endobj\n",
            "3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R>>endobj\n",
            "4 0 obj<</Length 27>>stream\n0 0 1 rg 10 10 100 100 re f\nendstream endobj\n",
            "5 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 6 0 R>>endobj\n",
            "6 0 obj<</Length 27>>stream\n1 0 0 rg 10 10 100 100 re f\nendstream endobj\n",
            "trailer<</Root 1 0 R/Size 7>>\n%%EOF\n",
        );
        let bump_mtime = |path: &Path, secs: u64| {
            let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(secs);
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(newer)
                .unwrap();
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.pdf");
        std::fs::write(&path, two_page_pdf).unwrap();
        let mut tabs = EditorTabs::new();
        tabs.open_pinned(&path).unwrap();
        assert!(tabs.change_pdf_page(1), "two pages, so page 2 renders");
        let good_bytes = tabs
            .iter_tabs()
            .next()
            .unwrap()
            .image
            .as_ref()
            .unwrap()
            .bytes
            .clone();
        std::fs::write(&path, two_page_pdf).unwrap();
        bump_mtime(&path, 2);
        *crate::pdf::FAIL_RASTERIZE_ONCE_FOR_TEST.lock().unwrap() = Some((path.clone(), 2));
        let report = tabs.reload_externally_changed_tabs(&|_| false);
        assert!(
            report.reloaded.is_empty(),
            "a reload that cannot restore the reader's page is a failed \
             reload, not a successful one that lost their place"
        );
        assert_eq!(
            tabs.pdf_page(),
            Some(2),
            "a transient render failure must not snap the reader to page 1; status: {:?}",
            tabs.iter_tabs().next().unwrap().status
        );
        assert_eq!(
            tabs.iter_tabs()
                .next()
                .unwrap()
                .image
                .as_ref()
                .unwrap()
                .bytes,
            good_bytes,
            "the last good render survives the transient failure"
        );
        // The failure was transient: the next sweep reloads and the reader
        // is still on their page.
        bump_mtime(&path, 4);
        let report = tabs.reload_externally_changed_tabs(&|_| false);
        assert_eq!(report.reloaded, vec![path.clone()]);
        assert_eq!(
            tabs.pdf_page(),
            Some(2),
            "recovery re-renders the reader's page"
        );
    }

    #[test]
    fn insert_str_inserts_newlines() {
        let mut e = editor_with("");
        e.insert_str("a\nb\nc");
        assert_eq!(
            e.lines,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(e.cursor_row, 2);
        assert_eq!(e.cursor_col, 1);
    }

    #[test]
    fn build_line_spans_no_highlights() {
        let spans = build_line_spans("hello", &[]);
        assert_eq!(spans.len(), 1);
    }

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn wrap_segments_short_line_is_one_segment() {
        assert_eq!(wrap_segments(&chars("hello"), 20), vec![(0, 5)]);
        assert_eq!(wrap_segments(&chars(""), 20), vec![(0, 0)]);
        // width 0 must not loop forever - one segment covering the line.
        assert_eq!(wrap_segments(&chars("abc"), 0), vec![(0, 3)]);
    }

    #[test]
    fn wrap_segments_breaks_after_a_space() {
        // "the quick brown" at width 10 breaks after "the quick " (10 chars).
        let segs = wrap_segments(&chars("the quick brown"), 10);
        assert_eq!(segs, vec![(0, 10), (10, 15)]);
    }

    #[test]
    fn wrap_segments_hard_breaks_a_too_long_word() {
        // No space in the first 6 chars, so the word is split at the column.
        let segs = wrap_segments(&chars("abcdefghij"), 6);
        assert_eq!(segs, vec![(0, 6), (6, 10)]);
    }

    #[test]
    fn wrap_segments_breaks_after_punctuation_like_vscode() {
        // VS Code breaks after `/` (a break-after char), so a path folds at
        // the slash rather than mid-segment.
        let segs = wrap_segments(&chars("abc/defgh"), 6);
        assert_eq!(segs, vec![(0, 4), (4, 9)]);
    }

    #[test]
    fn wrap_segments_tile_the_line_without_gaps() {
        let line = "alpha beta gamma delta epsilon zeta eta theta";
        let segs = wrap_segments(&chars(line), 12);
        assert_eq!(segs.first().unwrap().0, 0);
        assert_eq!(segs.last().unwrap().1, line.chars().count());
        for pair in segs.windows(2) {
            assert_eq!(pair[0].1, pair[1].0, "segments must be contiguous");
        }
    }

    #[test]
    fn build_line_spans_full_line_highlighted() {
        let hi = vec![HiSpan {
            start: 0,
            end: 5,
            style: Style::default(),
        }];
        let spans = build_line_spans("hello", &hi);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn build_line_spans_partial_highlights() {
        let hi = vec![HiSpan {
            start: 1,
            end: 3,
            style: Style::default(),
        }];
        let spans = build_line_spans("abcde", &hi);
        // Expect: "a", "bc", "de"
        assert_eq!(spans.len(), 3);
    }

    #[test]
    fn merge_overlay_semantic_wins_and_base_fills_gaps() {
        use ratatui::style::Color;
        let base_style = Style::default().fg(Color::Rgb(1, 1, 1));
        let over_style = Style::default().fg(Color::Rgb(2, 2, 2));
        // Base covers the whole 0..10 line; overlay repaints 3..6.
        let base = vec![HiSpan {
            start: 0,
            end: 10,
            style: base_style,
        }];
        let over = vec![HiSpan {
            start: 3,
            end: 6,
            style: over_style,
        }];
        let merged = merge_overlay(&base, &over);
        // Expect base[0..3], over[3..6], base[6..10], sorted and gapless.
        assert_eq!(merged.len(), 3);
        assert_eq!((merged[0].start, merged[0].end), (0, 3));
        assert_eq!(merged[0].style.fg, Some(Color::Rgb(1, 1, 1)));
        assert_eq!((merged[1].start, merged[1].end), (3, 6));
        assert_eq!(
            merged[1].style.fg,
            Some(Color::Rgb(2, 2, 2)),
            "overlay must win over base where they overlap"
        );
        assert_eq!((merged[2].start, merged[2].end), (6, 10));
        assert_eq!(merged[2].style.fg, Some(Color::Rgb(1, 1, 1)));
    }

    #[test]
    fn merge_overlay_empty_overlay_returns_base() {
        let base = vec![HiSpan {
            start: 0,
            end: 4,
            style: Style::default(),
        }];
        assert_eq!(merge_overlay(&base, &[]).len(), 1);
    }

    #[test]
    fn editor_selection_normalised_handles_anchor_after_head() {
        let s = EditorSelection {
            anchor: (5, 4),
            head: (2, 1),
        };
        assert_eq!(s.normalised(), ((2, 1), (5, 4)));
    }

    #[test]
    fn editor_selection_normalised_handles_same_row() {
        let s = EditorSelection {
            anchor: (3, 9),
            head: (3, 2),
        };
        assert_eq!(s.normalised(), ((3, 2), (3, 9)));
    }

    #[test]
    fn editor_selection_has_area_only_when_endpoints_differ() {
        let s = EditorSelection::new(2, 5);
        assert!(!s.has_area());
        let s2 = EditorSelection {
            anchor: (2, 5),
            head: (2, 6),
        };
        assert!(s2.has_area());
    }

    #[test]
    fn start_selection_at_cursor_creates_zero_area_selection() {
        let mut e = editor_with("hello");
        e.cursor_row = 0;
        e.cursor_col = 2;
        e.start_selection_at_cursor();
        let sel = e.selection.expect("selection should exist");
        assert_eq!(sel.anchor, (0, 2));
        assert_eq!(sel.head, (0, 2));
        assert!(!sel.has_area());
    }

    #[test]
    fn extend_selection_to_cursor_updates_head_only() {
        let mut e = editor_with("abcdef");
        e.cursor_col = 1;
        e.start_selection_at_cursor();
        e.cursor_col = 4;
        e.extend_selection_to_cursor();
        let sel = e.selection.unwrap();
        assert_eq!(sel.anchor, (0, 1));
        assert_eq!(sel.head, (0, 4));
        assert!(sel.has_area());
    }

    #[test]
    fn selection_text_single_line() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        assert_eq!(e.selection_text(), "world");
    }

    #[test]
    fn selection_text_handles_reversed_endpoints() {
        let mut e = editor_with("hello world");
        e.cursor_col = 11;
        e.start_selection_at_cursor();
        e.cursor_col = 6;
        e.extend_selection_to_cursor();
        assert_eq!(e.selection_text(), "world");
    }

    #[test]
    fn selection_text_multi_line_includes_newlines() {
        let mut e = editor_with("first line\nsecond line\nthird");
        e.cursor_row = 0;
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_row = 1;
        e.cursor_col = 6;
        e.extend_selection_to_cursor();
        assert_eq!(e.selection_text(), "line\nsecond");
    }

    #[test]
    fn selection_text_multibyte_chars() {
        let mut e = editor_with("héllo");
        e.cursor_col = 1;
        e.start_selection_at_cursor();
        e.cursor_col = 3;
        e.extend_selection_to_cursor();
        assert_eq!(e.selection_text(), "él");
    }

    #[test]
    fn clear_selection_removes_it() {
        let mut e = editor_with("abc");
        e.start_selection_at_cursor();
        assert!(e.selection.is_some());
        e.clear_selection();
        assert!(e.selection.is_none());
    }

    #[test]
    fn delete_selection_removes_range_within_one_line() {
        let mut e = editor_with("hello world");
        e.cursor_col = 5;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        assert!(e.delete_selection());
        assert_eq!(e.lines, vec!["hello".to_string()]);
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 5);
        assert!(e.selection.is_none());
        assert!(e.dirty);
    }

    #[test]
    fn delete_selection_collapses_multiple_lines() {
        let mut e = editor_with("first\nsecond\nthird");
        e.cursor_row = 0;
        e.cursor_col = 2;
        e.start_selection_at_cursor();
        e.cursor_row = 2;
        e.cursor_col = 2;
        e.extend_selection_to_cursor();
        assert!(e.delete_selection());
        assert_eq!(e.lines, vec!["fiird".to_string()]);
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn delete_selection_returns_false_when_no_selection() {
        let mut e = editor_with("abc");
        assert!(!e.delete_selection());
        assert_eq!(e.lines, vec!["abc".to_string()]);
    }

    #[test]
    fn delete_selection_returns_false_when_zero_area() {
        let mut e = editor_with("abc");
        e.cursor_col = 1;
        e.start_selection_at_cursor();
        assert!(!e.delete_selection());
        assert_eq!(e.lines, vec!["abc".to_string()]);
    }

    #[test]
    fn insert_char_replaces_selection_when_active() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        e.insert_char('X');
        assert_eq!(e.lines, vec!["hello X".to_string()]);
        assert_eq!(e.cursor_col, 7);
        assert!(e.selection.is_none());
    }

    #[test]
    fn backspace_deletes_selection_when_active() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        e.backspace();
        assert_eq!(e.lines, vec!["hello ".to_string()]);
        assert!(e.selection.is_none());
    }

    #[test]
    fn delete_forward_deletes_selection_when_active() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        e.delete_forward();
        assert_eq!(e.lines, vec!["hello ".to_string()]);
        assert!(e.selection.is_none());
    }

    #[test]
    fn insert_str_replaces_selection_when_active() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        e.insert_str("everyone");
        assert_eq!(e.lines, vec!["hello everyone".to_string()]);
        assert!(e.selection.is_none());
    }

    #[test]
    fn select_all_spans_entire_buffer() {
        let mut e = editor_with("a\nbc\nd");
        e.select_all();
        let sel = e.selection.unwrap();
        let (start, end) = sel.normalised();
        assert_eq!(start, (0, 0));
        assert_eq!(end, (2, 1));
        assert_eq!(e.selection_text(), "a\nbc\nd");
    }

    #[test]
    fn mouse_down_starts_zero_area_selection_at_click() {
        let mut e = editor_with("hello");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        e.mouse_down(3, 0); // text_x = 0 + 2 + 1 = 3, click col 3 → editor col 0
        assert_eq!(e.cursor_col, 0);
        let sel = e.selection.expect("anchor created on mouse down");
        assert_eq!(sel.anchor, (0, 0));
        assert!(!sel.has_area());
    }

    #[test]
    fn render_never_replaces_character_at_cursor() {
        // The hardware caret (DECSCUSR BlinkingBar) overlays the cell at the
        // cursor position; the editor's own render must NEVER change the
        // symbol there or the blink would visibly swallow the letter.
        use ratatui::buffer::Buffer;
        let mut e = editor_with("hello");
        e.cursor_col = 2;
        e.focused = true;

        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 5,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);

        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let cell = &buf[(text_x + 2, e.last_inner.y)];
        assert_eq!(
            cell.symbol(),
            "l",
            "editor render must leave the underlying glyph alone"
        );
    }

    #[test]
    fn cursor_screen_pos_inside_viewport() {
        let mut e = editor_with("hello\nworld");
        e.last_inner = Rect {
            x: 5,
            y: 7,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        e.cursor_row = 1;
        e.cursor_col = 3;
        // text_x = inner.x + gutter + 1 = 5 + 2 + 1 = 8
        // cy = inner.y + (cursor_row - scroll) = 7 + 1 = 8
        assert_eq!(e.cursor_screen_pos(), Some((8 + 3, 8)));
    }

    #[test]
    fn cursor_screen_pos_wrap_shows_caret_on_blank_line() {
        // Regression: in soft-wrap mode (the Markdown default) the caret
        // vanished on an empty line — the row's segment is (start 0, end 0)
        // and the old guard bailed on `end == start`, so the user typed
        // blind on every blank line.
        let mut e = editor_with("hello\n\nworld");
        e.wrap_override = Some(true);
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        e.cursor_row = 1;
        e.cursor_col = 0;
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        assert_eq!(
            e.cursor_screen_pos(),
            Some((text_x, e.last_inner.y + 1)),
            "the caret must be visible on a blank line in wrap mode"
        );
    }

    #[test]
    fn cursor_screen_pos_wrap_shows_caret_at_end_of_line() {
        // Regression: typing at the end of a line in wrap mode hid the
        // caret — `cursor_col == end == line length` is accepted by the
        // row matcher but was then rejected by the old
        // `visible_col >= end - start` guard.
        let mut e = editor_with("hello\nworld");
        e.wrap_override = Some(true);
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        e.cursor_row = 0;
        e.cursor_col = 5;
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        assert_eq!(
            e.cursor_screen_pos(),
            Some((text_x + 5, e.last_inner.y)),
            "the caret must be visible one past the last character in wrap mode"
        );
    }

    #[test]
    fn cursor_screen_pos_wrap_boundary_still_maps_to_next_row_start() {
        // A caret on a wrap boundary belongs to the NEXT visual row's first
        // cell, never one past the previous row's last cell.
        let mut e = editor_with("aaaa bbbb cccc dddd eeee ffff gggg hhhh");
        e.wrap_override = Some(true);
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 6,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        // Find the second visual row's start and put the caret exactly there
        // (the first row's `end`).
        let Some((_, start, _)) = e.text_row(1) else {
            panic!("expected a wrapped second row");
        };
        e.cursor_row = 0;
        e.cursor_col = start;
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        assert_eq!(
            e.cursor_screen_pos(),
            Some((text_x, e.last_inner.y + 1)),
            "a wrap-boundary caret must land on the next row's first cell"
        );
    }

    #[test]
    fn cursor_screen_pos_wrap_zero_width_column_rejects_out_of_pane_cells() {
        // A pane narrow enough leaves a zero-width wrap column, where
        // wrap_segments degenerates to one whole-line segment: a caret deep
        // in the line would compute a cell far past the pane's right edge
        // (across the scrollbar column and beyond). The right-edge clamp
        // must reject it, not paint it. With any nonzero column width the
        // segments are capped at that width, so this degenerate layout is
        // the only way an accepted caret can reach the clamp.
        let mut e = editor_with("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        e.wrap_override = Some(true);
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 5,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let (_, start, end) = e.text_row(0).expect("a text row");
        assert_eq!(
            (start, end),
            (0, 40),
            "expected the zero-width degenerate one-segment layout"
        );
        e.cursor_row = 0;
        e.cursor_col = 20;
        assert_eq!(
            e.cursor_screen_pos(),
            None,
            "a caret cell past the pane edge must not be painted"
        );
    }

    #[test]
    fn cursor_screen_pos_returns_none_when_scrolled_off() {
        let mut e = editor_with_lines(50);
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        e.last_gutter_width = 3;
        e.scroll = 30;
        e.cursor_row = 5; // above viewport
        assert_eq!(e.cursor_screen_pos(), None);
    }

    #[test]
    fn undo_restores_previous_buffer_and_cursor() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_char('d');
        assert_eq!(e.lines[0], "abcd");
        assert!(e.undo());
        assert_eq!(e.lines[0], "abc");
        assert_eq!(e.cursor_col, 3);
    }

    #[test]
    fn undo_on_empty_stack_returns_false() {
        let mut e = editor_with("abc");
        assert!(!e.undo());
        assert_eq!(e.lines[0], "abc");
    }

    #[test]
    fn undo_coalesces_consecutive_typed_chars_into_one_step() {
        let mut e = editor_with("");
        e.insert_char('h');
        e.insert_char('i');
        e.insert_char('!');
        // One undo undoes the whole typed run "hi!".
        assert!(e.undo());
        assert_eq!(e.lines[0], "");
    }

    #[test]
    fn undo_does_not_coalesce_across_movement() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_char('d');
        e.move_left();
        e.insert_char('z');
        // First undo removes 'z', second undo removes 'd'.
        assert!(e.undo());
        assert_eq!(e.lines[0], "abcd");
        assert!(e.undo());
        assert_eq!(e.lines[0], "abc");
    }

    #[test]
    fn undo_does_not_coalesce_across_backspace() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_char('d');
        e.backspace();
        e.insert_char('e');
        assert!(e.undo());
        assert_eq!(e.lines[0], "abc");
        assert!(e.undo());
        assert_eq!(e.lines[0], "abcd");
        assert!(e.undo());
        assert_eq!(e.lines[0], "abc");
    }

    #[test]
    fn undo_paste_is_one_step() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_str("XYZ");
        assert_eq!(e.lines[0], "abcXYZ");
        assert!(e.undo());
        assert_eq!(e.lines[0], "abc");
    }

    #[test]
    fn undo_after_replace_selection_restores_original() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        e.insert_char('X');
        assert_eq!(e.lines[0], "hello X");
        assert!(e.undo());
        assert_eq!(e.lines[0], "hello world");
    }

    #[test]
    fn undo_restores_dirty_flag() {
        let mut e = editor_with("abc");
        assert!(!e.dirty);
        e.cursor_col = 3;
        e.insert_char('d');
        assert!(e.dirty);
        e.undo();
        assert!(!e.dirty, "undoing the only edit restores the clean state");
    }

    #[test]
    fn editor_render_paints_search_match_cells_with_yellow_bg() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("alpha needle bravo needle zulu");
        e.set_search_highlight(Some(String::from("needle")));
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 3,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        let yellow = Color::Rgb(0xff, 0xd7, 0x4a);
        // First "needle" starts at char index 6 ("alpha "), 6 chars long.
        for col in 6..12u16 {
            assert_eq!(
                buf[(text_x + col, y)].bg,
                yellow,
                "first match cell {col} must have yellow bg"
            );
        }
        // Cells just outside the match must NOT be yellow.
        assert_ne!(buf[(text_x + 5, y)].bg, yellow, "cell before match");
        assert_ne!(buf[(text_x + 12, y)].bg, yellow, "cell after match");
        // Second "needle" starts at char index 19 ("alpha needle bravo "), 6 chars.
        for col in 19..25u16 {
            assert_eq!(
                buf[(text_x + col, y)].bg,
                yellow,
                "second match cell {col} must have yellow bg"
            );
        }
    }

    #[test]
    fn editor_render_lights_other_occurrences_of_selection_in_blue() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("alpha needle bravo needle zulu");
        // Select the first "needle" (chars 6..12).
        e.selection = Some(EditorSelection {
            anchor: (0, 6),
            head: (0, 12),
        });
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 3,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        let occurrence = Color::Rgb(0x37, 0x61, 0x8e);
        let selection = Color::Rgb(0x26, 0x4f, 0x78);
        // The selected occurrence wears the brighter selection band, not the
        // dimmer occurrence blue.
        for col in 6..12u16 {
            assert_eq!(
                buf[(text_x + col, y)].bg,
                selection,
                "selected occurrence cell {col} must show the selection band"
            );
        }
        // The OTHER "needle" (chars 19..25) wears the occurrence blue.
        for col in 19..25u16 {
            assert_eq!(
                buf[(text_x + col, y)].bg,
                occurrence,
                "other occurrence cell {col} must show the occurrence blue"
            );
        }
        // A non-matching cell stays unpainted.
        assert_ne!(buf[(text_x + 18, y)].bg, occurrence, "space before match");
    }

    #[test]
    fn editor_render_skips_occurrence_highlight_for_whitespace_selection() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("a  b  c  d");
        // Select two spaces (chars 1..3) — whitespace only.
        e.selection = Some(EditorSelection {
            anchor: (0, 1),
            head: (0, 3),
        });
        e.focused = true;
        assert_eq!(e.selection_occurrence_needle(), None);
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 3,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        let occurrence = Color::Rgb(0x37, 0x61, 0x8e);
        for col in 0..10u16 {
            assert_ne!(
                buf[(text_x + col, y)].bg,
                occurrence,
                "whitespace-only selection must not light occurrences"
            );
        }
    }

    #[test]
    fn editor_render_skips_occurrence_highlight_for_multiline_selection() {
        let mut e = editor_with("needle\nneedle");
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (1, 6),
        });
        assert_eq!(
            e.selection_occurrence_needle(),
            None,
            "multi-line selections do not drive occurrence highlighting"
        );
    }

    #[test]
    fn editor_render_does_not_paint_search_highlight_when_unset() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("alpha needle bravo");
        e.set_search_highlight(None);
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 3,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        let yellow = Color::Rgb(0xff, 0xd7, 0x4a);
        for col in 0..18u16 {
            assert_ne!(
                buf[(text_x + col, y)].bg,
                yellow,
                "no cell should be highlighted when search_highlight is None"
            );
        }
    }

    #[test]
    fn editor_render_search_highlight_is_case_insensitive() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("Foo bar FOO baz");
        e.set_search_highlight(Some(String::from("foo")));
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 3,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        let yellow = Color::Rgb(0xff, 0xd7, 0x4a);
        for col in 0..3u16 {
            assert_eq!(buf[(text_x + col, y)].bg, yellow, "cell {col} of 'Foo'");
        }
        for col in 8..11u16 {
            assert_eq!(buf[(text_x + col, y)].bg, yellow, "cell {col} of 'FOO'");
        }
    }

    #[test]
    fn open_diff_replaces_blank_initial_tab_with_diff_view() {
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "alpha\nbravo\ncharlie\n").unwrap();
        std::fs::write(f2.path(), "alpha\nBRAVO\ncharlie\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_diff(f1.path(), f2.path()).unwrap();
        assert_eq!(t.tab_count(), 1, "blank initial slot should be reused");
        let active = &t.editors[t.active_index()];
        assert!(active.diff.is_some(), "active tab must hold diff data");
        let diff = active.diff.as_ref().unwrap();
        assert_eq!(diff.left_lines, vec!["alpha", "bravo", "charlie"]);
        assert_eq!(diff.right_lines, vec!["alpha", "BRAVO", "charlie"]);
        assert_eq!(diff.rows.len(), 3);
    }

    #[test]
    fn tab_full_path_reports_the_whole_path_for_disambiguation() {
        // The tab label only shows `file_name()`, so two `app.ts` tabs look
        // identical. The hover tooltip must expose the full path to tell
        // them apart.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("app.ts");
        std::fs::write(&p, "export const x = 1;\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_pinned(&p).unwrap();
        let idx = t.active_index();
        assert_eq!(
            t.tab_full_path(idx).as_deref(),
            Some(p.to_string_lossy().as_ref()),
            "tooltip must expose the full path, not just the file name"
        );
    }

    #[test]
    fn tab_full_path_is_none_for_an_unsaved_buffer() {
        let t = EditorTabs::new();
        assert_eq!(
            t.tab_full_path(t.active_index()),
            None,
            "an untitled buffer has no path to disambiguate"
        );
    }

    #[test]
    fn tab_full_path_shows_both_sides_of_a_diff() {
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "a\n").unwrap();
        std::fs::write(f2.path(), "b\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_diff(f1.path(), f2.path()).unwrap();
        let label = t.tab_full_path(t.active_index()).unwrap();
        assert!(label.contains(&f1.path().to_string_lossy().into_owned()));
        assert!(label.contains(&f2.path().to_string_lossy().into_owned()));
        assert!(label.contains('\u{2194}'), "diff tooltip shows both sides");
    }

    #[test]
    fn diff_header_paints_prev_next_arrows_at_the_right_edge() {
        // Regression: the user asked for clickable arrows in the diff
        // header so they can hop between change hunks without
        // hand-scrolling. The arrows must paint at the rightmost cells
        // of the header row AND must populate the editor's hit-test
        // fields so the click handler can route mouse events to
        // prev/next-change navigation.
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "alpha\nbravo\ncharlie\n").unwrap();
        std::fs::write(f2.path(), "alpha\nBRAVO\ncharlie\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_diff(f1.path(), f2.path()).unwrap();
        let active_idx = t.active_index();
        t.editors[active_idx].focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 20,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
        let ed = &t.editors[active_idx];
        assert!(
            ed.diff_prev_arrow.width > 0,
            "prev arrow rect must be tracked so a click can navigate to the previous hunk"
        );
        assert!(
            ed.diff_next_arrow.width > 0,
            "next arrow rect must be tracked so a click can navigate to the next hunk"
        );
        // Glyphs land where the rects say they do.
        assert_eq!(
            buf[(ed.diff_prev_arrow.x, ed.diff_prev_arrow.y)].symbol(),
            "\u{2039}",
            "prev arrow cell must paint ‹"
        );
        assert_eq!(
            buf[(ed.diff_next_arrow.x, ed.diff_next_arrow.y)].symbol(),
            "\u{203a}",
            "next arrow cell must paint ›"
        );
        // Next sits to the right of prev on the same header row.
        assert_eq!(ed.diff_prev_arrow.y, ed.diff_next_arrow.y);
        assert!(ed.diff_prev_arrow.x < ed.diff_next_arrow.x);
        // Both arrows sit inside the editor's inner rect's right band.
        let right_edge = ed.last_inner.x + ed.last_inner.width;
        assert!(ed.diff_next_arrow.x < right_edge);
    }

    #[test]
    fn non_diff_tab_clears_diff_arrow_hit_rects_on_render() {
        // Regression guard: switching from a diff tab to a regular file
        // tab must NOT leave the stale arrow rects in place — otherwise
        // a click at those cells on the new tab would mis-route as a
        // change-navigation event.
        let f = NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "alpha\nbravo\ncharlie\n").unwrap();
        let mut ed = Editor::new();
        ed.open(f.path()).unwrap();
        ed.diff_prev_arrow = Rect {
            x: 30,
            y: 0,
            width: 1,
            height: 1,
        };
        ed.diff_next_arrow = Rect {
            x: 32,
            y: 0,
            width: 1,
            height: 1,
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
        assert_eq!(ed.diff_prev_arrow, Rect::default());
        assert_eq!(ed.diff_next_arrow, Rect::default());
    }

    #[test]
    fn diff_hit_test_maps_click_in_left_text_column_to_left_side_and_char_col() {
        use crate::widgets::diff::DiffSide;
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "alpha\nbravo\ncharlie\n").unwrap();
        std::fs::write(f2.path(), "alpha\nBRAVO\ncharlie\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_diff(f1.path(), f2.path()).unwrap();
        let active_idx = t.active_index();
        t.editors[active_idx].focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
        let ed = &t.editors[active_idx];
        let diff = ed.diff.as_ref().unwrap();
        // Header is at ed.last_inner.y; body starts at last_inner.y + 1.
        let body_top = ed.last_inner.y + 1;
        // Left text column begins at l_text_x = inner.x + l_gutter + 2.
        let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
        let l_text_x = ed.last_inner.x + l_gutter + 2;
        let hit =
            crate::widgets::editor::diff_hit_test(diff, ed.last_inner, l_text_x + 2, body_top + 1);
        assert_eq!(
            hit,
            Some((DiffSide::Left, 1, 2)),
            "a click two cells into the left text column of the second body row must map to Left, row 1, char col 2"
        );
    }

    #[test]
    fn diff_hit_test_maps_click_in_right_text_column_to_right_side() {
        use crate::widgets::diff::DiffSide;
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "alpha\nbravo\n").unwrap();
        std::fs::write(f2.path(), "alpha\nBRAVO\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_diff(f1.path(), f2.path()).unwrap();
        let active_idx = t.active_index();
        t.editors[active_idx].focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
        let ed = &t.editors[active_idx];
        let diff = ed.diff.as_ref().unwrap();
        let body_top = ed.last_inner.y + 1;
        let half = ed.last_inner.width / 2;
        let r_gutter = (diff.right_lines.len() + 1).to_string().len() as u16 + 1;
        let r_text_x = ed.last_inner.x + half + 1 + r_gutter + 2;
        let hit =
            crate::widgets::editor::diff_hit_test(diff, ed.last_inner, r_text_x + 3, body_top + 1);
        assert!(
            matches!(hit, Some((DiffSide::Right, 1, 3))),
            "a click three cells into the right text column of the second body row must map to Right, row 1, char col 3; got {hit:?}"
        );
    }

    #[test]
    fn diff_caret_screen_pos_inverts_the_right_column_hit_test() {
        use crate::widgets::diff::DiffSide;
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "alpha\nbravo\n").unwrap();
        std::fs::write(f2.path(), "alpha\nBRAVO\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_diff(f1.path(), f2.path()).unwrap();
        let active_idx = t.active_index();
        t.editors[active_idx].focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
        let body_top = t.editors[active_idx].last_inner.y + 1;
        let half = t.editors[active_idx].last_inner.width / 2;
        let diff = t.editors[active_idx].diff.as_ref().unwrap();
        let r_gutter = (diff.right_lines.len() + 1).to_string().len() as u16 + 1;
        let r_text_x = t.editors[active_idx].last_inner.x + half + 1 + r_gutter + 2;
        t.editors[active_idx]
            .diff
            .as_mut()
            .unwrap()
            .start_selection(DiffSide::Right, 1, 3);
        let pos = t.editors[active_idx].diff_caret_screen_pos();
        assert_eq!(
            pos,
            Some((r_text_x + 3, body_top + 1)),
            "caret at right row 1 col 3 must land on the cell diff_hit_test maps that screen point back to"
        );
    }

    #[test]
    fn render_diff_paints_selection_band_over_selected_cells() {
        use crate::widgets::diff::DiffSide;
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "alpha\nbravo\n").unwrap();
        std::fs::write(f2.path(), "alpha\nBRAVO\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_diff(f1.path(), f2.path()).unwrap();
        let active_idx = t.active_index();
        t.editors[active_idx].focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
        let diff_mut = t.editors[active_idx].diff.as_mut().unwrap();
        diff_mut.start_selection(DiffSide::Left, 1, 0);
        diff_mut.extend_selection_to(1, 5);
        ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
        let ed = &t.editors[active_idx];
        let diff = ed.diff.as_ref().unwrap();
        let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
        let l_text_x = ed.last_inner.x + l_gutter + 2;
        let y = ed.last_inner.y + 2;
        let band_bg = Color::Rgb(0x26, 0x4f, 0x78);
        for col in 0..5u16 {
            assert_eq!(
                buf[(l_text_x + col, y)].bg,
                band_bg,
                "cell {col} of the second body row must paint the selection band on the left column"
            );
        }
    }

    #[test]
    fn open_diff_inserts_a_new_tab_when_an_open_file_already_exists() {
        let existing = NamedTempFile::new().unwrap();
        std::fs::write(existing.path(), "x\n").unwrap();
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "1\n").unwrap();
        std::fs::write(f2.path(), "2\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_pinned(existing.path()).unwrap();
        let before = t.tab_count();
        t.open_diff(f1.path(), f2.path()).unwrap();
        assert_eq!(t.tab_count(), before + 1, "must insert a new diff tab");
        assert!(t.editors[t.active_index()].diff.is_some());
    }

    #[test]
    fn diff_tab_label_uses_arrow_between_filenames() {
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "a\n").unwrap();
        std::fs::write(f2.path(), "b\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_diff(f1.path(), f2.path()).unwrap();
        let label = tab_label(&t.editors[t.active_index()]);
        let l = f1
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let r = f2
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(label, format!("{l} \u{2194} {r}"));
    }

    #[test]
    fn git_diff_text_tab_label_omits_arrow_when_right_side_is_synthetic() {
        // Regression for "git diff master ↔ null" in the tab strip:
        // synthetic single-sided diffs (raw `git diff` text view, full-
        // file deletion view) leave `right_path` empty; the tab label
        // must collapse to just the left label rather than trailing the
        // misleading "↔ null".
        let mut t = EditorTabs::new();
        t.open_git_diff_side_by_side(
            std::path::Path::new("git diff master"),
            "diff --git a/x b/x\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        let label = tab_label(&t.editors[t.active_index()]);
        assert_eq!(
            label, "git diff master",
            "synthetic git-diff tab must not show '↔ null' — the right side is virtual"
        );
    }

    #[test]
    fn editor_tabs_search_highlight_survives_close_then_open_via_preview() {
        // Reported bug: after closing the first tab with X, the next file
        // opened from search lost the yellow highlights. The new editor
        // was created without the EditorTabs' current term applied.
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "alpha needle bravo\n").unwrap();
        std::fs::write(f2.path(), "second needle line\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_pinned(f1.path()).unwrap();
        t.set_search_highlight(
            Some(String::from("needle")),
            crate::widgets::search::SearchOpts::default(),
        );
        // Close the first (and only non-blank) tab — leaves the blank
        // initial tab.
        let close_idx = t
            .editors
            .iter()
            .position(|e| e.path.as_deref() == Some(f1.path()))
            .unwrap();
        assert!(t.close_tab(close_idx));
        // Now open a different file via preview, mirroring "click on a
        // search hit" in the running app.
        t.open_preview(f2.path()).unwrap();
        let active = t
            .editors
            .iter()
            .find(|e| e.path.as_deref() == Some(f2.path()))
            .unwrap();
        assert_eq!(
            active.search_highlight.as_deref(),
            Some("needle"),
            "newly opened tab must inherit the EditorTabs' current search term"
        );
    }

    #[test]
    fn editor_tabs_search_highlight_survives_open_pinned_after_close() {
        // Same scenario as above but via open_pinned (double-click /
        // Ctrl+Enter). The new editor must also inherit the term.
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "first\n").unwrap();
        std::fs::write(f2.path(), "second needle line\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_pinned(f1.path()).unwrap();
        t.open_pinned(f2.path()).unwrap();
        t.set_search_highlight(
            Some(String::from("needle")),
            crate::widgets::search::SearchOpts::default(),
        );
        // Close f1; both should still be highlighted, and now a fresh open
        // of a third file should inherit too.
        let f3 = NamedTempFile::new().unwrap();
        std::fs::write(f3.path(), "third needle row\n").unwrap();
        let close_idx = t
            .editors
            .iter()
            .position(|e| e.path.as_deref() == Some(f1.path()))
            .unwrap();
        assert!(t.close_tab(close_idx));
        t.open_pinned(f3.path()).unwrap();
        let third = t
            .editors
            .iter()
            .find(|e| e.path.as_deref() == Some(f3.path()))
            .unwrap();
        assert_eq!(third.search_highlight.as_deref(), Some("needle"));
    }

    #[test]
    fn editor_tabs_set_search_highlight_propagates_to_every_tab() {
        let mut t = EditorTabs::new();
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "hello").unwrap();
        std::fs::write(f2.path(), "world").unwrap();
        t.open_pinned(f1.path()).unwrap();
        t.open_pinned(f2.path()).unwrap();
        t.set_search_highlight(
            Some(String::from("term")),
            crate::widgets::search::SearchOpts::default(),
        );
        for ed in &t.editors {
            assert_eq!(ed.search_highlight.as_deref(), Some("term"));
        }
        t.set_search_highlight(None, crate::widgets::search::SearchOpts::default());
        for ed in &t.editors {
            assert!(ed.search_highlight.is_none());
        }
    }

    #[test]
    fn editor_starts_with_zero_horizontal_scroll() {
        let e = editor_with("abcdef");
        assert_eq!(e.scroll_col, 0);
    }

    #[test]
    fn scroll_right_by_advances_horizontal_offset() {
        let mut e = editor_with("a".repeat(2000).as_str());
        e.scroll_right_by(40);
        assert_eq!(e.scroll_col, 40);
        e.scroll_right_by(60);
        assert_eq!(e.scroll_col, 100);
        e.scroll_left_by(30);
        assert_eq!(e.scroll_col, 70);
        e.scroll_left_by(1000); // saturating
        assert_eq!(e.scroll_col, 0);
    }

    #[test]
    fn move_right_past_visible_edge_advances_horizontal_scroll() {
        // User-reported bug: long-line files (HTML blob with base64 inline)
        // had no horizontal scroll, so moving the cursor right past the
        // viewport edge just clamped the visible cursor at the right
        // border without actually advancing scroll_col.
        let mut e = editor_with("a".repeat(500).as_str());
        e.focused = true;
        // Render once so last_inner / last_gutter_width are populated.
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 5,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let text_width = e
            .last_inner
            .width
            .saturating_sub(e.last_gutter_width + 2 + u16::from(e.last_scrollbar.width > 0))
            as usize;
        assert!(text_width > 0 && text_width < 500);
        // Move cursor to the very first off-screen column.
        e.cursor_col = text_width;
        e.move_right();
        assert!(
            e.scroll_col > 0,
            "moving the cursor past the visible edge must advance horizontal scroll"
        );
    }

    #[test]
    fn render_starts_line_from_scroll_col() {
        // With scroll_col = 3, a long line should display starting from its
        // 4th character ('D') at the text origin, not from 'A'. The line must
        // overflow the viewport, otherwise render clamps scroll_col to 0 (a
        // line that fits has nothing to scroll past).
        let mut e = editor_with("ABCDEFGHIJ".repeat(10).as_str());
        e.scroll_col = 3;
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 5,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        assert_eq!(
            buf[(text_x, e.last_inner.y)].symbol(),
            "D",
            "first visible column must be the (scroll_col)th character"
        );
    }

    #[test]
    fn render_clamps_scroll_col_to_content_width() {
        // A trackpad swipe can push scroll_col arbitrarily far right via
        // scroll_right_by (no clamp of its own). Render must pull it back so
        // the buffer can never be stranded entirely off-screen.
        let mut e = editor_with("a".repeat(40).as_str());
        e.focused = true;
        e.scroll_right_by(10_000);
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 5,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let text_width = e
            .last_inner
            .width
            .saturating_sub(e.last_gutter_width + 2 + u16::from(e.last_scrollbar.width > 0))
            as usize;
        assert!(text_width > 0);
        assert_eq!(
            e.scroll_col,
            40usize.saturating_sub(text_width),
            "scroll_col must clamp to (content cols - text width)"
        );
    }

    #[test]
    fn render_shows_horizontal_scrollbar_only_on_overflow() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };

        // Short line: no horizontal overflow, no bar.
        let mut short = editor_with("abc");
        short.focused = true;
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut short).render(area, &mut buf);
        assert_eq!(
            short.last_hscrollbar,
            Rect::default(),
            "a line that fits must not reserve a horizontal scrollbar row"
        );

        // Long line: overflow, bar painted on the bottom inner row.
        let mut long = editor_with("a".repeat(500).as_str());
        long.focused = true;
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut long).render(area, &mut buf);
        assert!(
            long.last_hscrollbar.width > 0 && long.last_hscrollbar.height == 1,
            "an overflowing line must paint a one-row horizontal scrollbar"
        );
        assert_eq!(
            long.last_hscrollbar.y,
            long.last_inner.y + long.last_inner.height - 1,
            "the horizontal scrollbar sits on the bottom inner row"
        );
    }

    #[test]
    fn scroll_to_bar_x_jumps_horizontal_offset() {
        let mut e = editor_with("a".repeat(500).as_str());
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        assert!(e.last_hscrollbar.width > 0);
        // Click the far-right end of the track → scroll to (near) max.
        let right_x = e.last_hscrollbar.x + e.last_hscrollbar.width - 1;
        assert!(e.scroll_to_bar_x(right_x));
        assert!(
            e.scroll_col > 0,
            "dragging the thumb to the right edge must advance scroll_col"
        );
        // Click the far-left end → scroll back to 0.
        assert!(e.scroll_to_bar_x(e.last_hscrollbar.x));
        assert_eq!(e.scroll_col, 0);
    }

    fn md_editor(text: &str) -> Editor {
        let mut e = editor_with(text);
        e.lang = Some(LangKind::Markdown);
        e
    }

    fn render_at(e: &mut Editor, w: u16, h: u16) {
        e.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (e as &mut Editor).render(area, &mut buf);
    }

    /// On hosts that ignore iTerm2's `SetColors` (Ghostty, Kitty, sixel) the
    /// image/CSV/diff canvases must paint the theme background explicitly:
    /// their `Reset` fill overrides the non-iTerm2 frame prefill (ratatui
    /// applies any `Some` bg), so with the chrome themed they were the only
    /// host-black islands left in the frame.
    #[test]
    fn non_iterm2_canvases_paint_the_theme_background_not_reset() {
        let themed = crate::theme::Theme::from_id("solarized-dark");
        assert_eq!(themed.id(), "solarized-dark", "bundled theme resolves");
        assert_eq!(
            canvas_bg(false, themed),
            themed.editor_bg(),
            "a non-iTerm2 host fills the canvas with the theme bg"
        );
        assert_eq!(
            canvas_bg(true, themed),
            Color::Reset,
            "iTerm2 keeps Reset so the SetColors session bg shows through"
        );
        // And the fill actually lands on every cell of the canvas.
        let mut e = editor_with("");
        e.theme = themed;
        e.image = Some(ImageView {
            bytes: Vec::new(),
            format_label: String::from("png"),
            pixel_w: 4,
            pixel_h: 4,
            byte_size: 1,
            generation: 0,
            pdf: None,
        });
        let area = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 6,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        render_image_placeholder(
            e.image.as_ref().unwrap(),
            None,
            area,
            &mut buf,
            canvas_bg(false, themed),
            crate::theme::Theme::default(),
        );
        assert_eq!(
            buf[(1, area.height - 1)].bg,
            themed.editor_bg(),
            "the canvas body must wear the theme bg, not the host default"
        );
    }

    /// The editor picture sits at `KITTY_Z_BELOW_TEXT_AND_BG`, and Kitty
    /// draws any cell with a non-default background OVER an image that deep.
    /// Painting the canvas with the explicit theme bg (the CSV/diff island
    /// fix) therefore hid the preview behind its own canvas on Ghostty/Kitty
    /// — transmitted every frame, never visible. The image canvas must keep
    /// the DEFAULT background on the protocols that layer the picture
    /// against the cells (Kitty) or blit it over them post-frame (iTerm2);
    /// the themed look survives because the bake letterboxes with the theme
    /// bg pixel. Sixel and no-graphics hosts keep the themed fill.
    #[test]
    fn the_kitty_image_canvas_keeps_the_default_bg_so_the_deep_z_preview_shows() {
        use crate::iterm2_inline::InlineImageProtocol;
        let themed = crate::theme::Theme::from_id("solarized-dark");
        assert_eq!(themed.id(), "solarized-dark", "bundled theme resolves");
        assert_eq!(
            image_canvas_bg(InlineImageProtocol::Kitty, themed),
            Color::Reset,
            "Kitty layers the deep-z image below non-default-bg cells, so the picture canvas must stay default-bg"
        );
        assert_eq!(
            image_canvas_bg(InlineImageProtocol::ITerm2, themed),
            Color::Reset,
            "iTerm2 keeps Reset so the SetColors session bg shows through"
        );
        assert_eq!(
            image_canvas_bg(InlineImageProtocol::Sixel, themed),
            themed.editor_bg(),
            "sixel blits into the cell buffer over the canvas, so the themed fill stays"
        );
        assert_eq!(
            image_canvas_bg(InlineImageProtocol::None, themed),
            themed.editor_bg(),
            "with no graphics the placeholder is all the user sees; it stays themed"
        );
        // And the Kitty choice actually lands on every canvas cell.
        let image = ImageView {
            bytes: Vec::new(),
            format_label: String::from("png"),
            pixel_w: 4,
            pixel_h: 4,
            byte_size: 1,
            generation: 0,
            pdf: None,
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 6,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        render_image_placeholder(
            &image,
            None,
            area,
            &mut buf,
            image_canvas_bg(InlineImageProtocol::Kitty, themed),
            crate::theme::Theme::default(),
        );
        // The metadata pill on the first row keeps its explicit bg by design:
        // the overlay anchors the picture one row BELOW it
        // (`update_editor_image_overlay` carves off the header row), so the
        // pill never overlaps the image. Every cell in the picture band under
        // it must stay default-bg.
        assert_ne!(
            buf[(1, area.y)].bg,
            Color::Reset,
            "the metadata pill row keeps its own background"
        );
        for y in area.y + 1..area.bottom() {
            for x in area.x..area.right() {
                assert_eq!(
                    buf[(x, y)].bg,
                    Color::Reset,
                    "canvas cell ({x}, {y}) must keep the default background or Kitty paints it over the image"
                );
            }
        }
    }

    /// Cmd+K Cmd+T while a preview is open: the preview bakes its colors at
    /// build time and was rebuilt only when the buffer edited, so it kept the
    /// previous theme's colors (chrome and code alike) until the user touched
    /// the file or toggled the preview off and on.
    #[test]
    fn a_theme_switch_rebuilds_an_open_markdown_preview() {
        let mut e = md_editor("```notalanguage\nplain code line\n```");
        assert!(e.toggle_markdown_preview());
        let themed = *crate::theme::Theme::all()
            .iter()
            .find(|t| t.syntax().fg != crate::theme::SyntaxPalette::BASE16.fg)
            .expect("a bundled theme with a non-Base16 code palette");
        e.theme = themed;
        e.rehighlight_for_theme();
        let (r, g, b) = themed.syntax().fg;
        let md = e.markdown_preview.as_ref().expect("preview stays open");
        let code = md
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.as_ref() == "plain code line")
            .expect("the code line must render");
        assert_eq!(
            code.style.fg,
            Some(Color::Rgb(r, g, b)),
            "the preview must recolor on a theme switch, not wait for an edit"
        );
    }

    #[test]
    fn markdown_wraps_long_line_without_horizontal_scrollbar() {
        let mut e = md_editor(&"word ".repeat(40)); // 200 chars, one logical line
        render_at(&mut e, 30, 10);
        assert_eq!(
            e.last_hscrollbar,
            Rect::default(),
            "wrapped markdown must not show a horizontal scrollbar"
        );
        assert_eq!(e.scroll_col, 0, "wrap mode never scrolls horizontally");
        let rows_for_line0 = e
            .last_wrap_rows
            .iter()
            .filter(|r| matches!(r, VisRow::Text { line: 0, .. }))
            .count();
        assert!(
            rows_for_line0 > 1,
            "a 200-char paragraph must fold onto multiple visual rows"
        );
    }

    #[test]
    fn non_markdown_long_line_still_scrolls_horizontally() {
        let mut e = editor_with(&"a".repeat(200));
        e.lang = Some(LangKind::Rust);
        render_at(&mut e, 30, 10);
        assert!(
            e.last_hscrollbar.width > 0,
            "code keeps the horizontal scrollbar instead of wrapping"
        );
    }

    #[test]
    fn markdown_cursor_screen_pos_lands_on_the_wrapped_row() {
        let mut e = md_editor(&"a".repeat(100));
        render_at(&mut e, 30, 10);
        let (_, seg0_start, seg0_end) = e.text_row(0).unwrap();
        assert_eq!(seg0_start, 0);
        // A column just past the first visual row's content sits on row 2.
        e.cursor_row = 0;
        e.cursor_col = seg0_end + 1;
        let (_, cy) = e.cursor_screen_pos().expect("cursor must be visible");
        assert_eq!(
            cy,
            e.last_inner.y + 1,
            "a column past the first segment renders on the second visual row"
        );
    }

    #[test]
    fn markdown_click_on_second_visual_row_maps_into_the_line() {
        let mut e = md_editor(&"a".repeat(100));
        render_at(&mut e, 30, 10);
        let seg0_end = e.text_row(0).unwrap().2;
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let (line, col) = e
            .buffer_pos_at(text_x, e.last_inner.y + 1)
            .expect("a click on the second visual row is a text hit");
        assert_eq!(line, 0, "still the same logical line");
        assert_eq!(
            col, seg0_end,
            "the first column of the second visual row is where segment one ended"
        );
    }

    #[test]
    fn markdown_mouse_down_below_wrapped_lines_lands_on_the_row_under_the_pointer() {
        // Line 0 wraps onto several visual rows; pressing on the row that
        // SHOWS line 1 must anchor the selection on line 1, not on the
        // linear `scroll + visual_row` (which counts every wrapped
        // continuation row as its own line and drifts downward).
        let mut e = md_editor(&format!("{}\nsecond\nthird\nfourth", "a".repeat(100)));
        render_at(&mut e, 30, 10);
        let vis_row = e
            .last_wrap_rows
            .iter()
            .position(|r| matches!(r, VisRow::Text { line: 1, .. }))
            .expect("line 1 is on screen");
        assert!(vis_row > 1, "line 0 must wrap for the rows to be displaced");
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        e.mouse_down(text_x, e.last_inner.y + vis_row as u16);
        assert_eq!(
            e.cursor_row, 1,
            "the press lands on the line under the pointer"
        );
        assert_eq!(e.cursor_col, 0);
    }

    #[test]
    fn markdown_move_down_steps_within_a_wrapped_paragraph() {
        let mut e = md_editor(&"a".repeat(100));
        render_at(&mut e, 30, 10);
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.move_down();
        assert_eq!(
            e.cursor_row, 0,
            "move_down stays on the same logical line - it steps to the next wrapped segment"
        );
        assert!(e.cursor_col > 0, "and advances into that next segment");
    }

    #[test]
    fn markdown_scrolls_to_reveal_tail_of_a_tall_paragraph() {
        // One logical line taller than the viewport must still be scrollable
        // via sub-line (segment) scroll.
        let mut e = md_editor(&"a".repeat(400));
        render_at(&mut e, 30, 6);
        let before = e.text_row(0).unwrap();
        e.scroll_down(3);
        render_at(&mut e, 30, 6);
        let after = e.text_row(0).unwrap();
        assert_eq!(after.0, 0, "still the single logical line");
        assert!(
            after.1 > before.1,
            "scrolling down advances into the paragraph (sub-line scroll)"
        );
    }

    #[test]
    fn render_paints_selection_band_on_selected_cells() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("hello world");
        e.cursor_col = 0;
        e.start_selection_at_cursor();
        e.cursor_col = 5;
        e.extend_selection_to_cursor();
        e.focused = true;

        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 5,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);

        // gutter for 1 line: "1 ".len() = 2 → text_x = 0+1+2+1 = 4
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let selected_bg = Color::Rgb(0x26, 0x4f, 0x78);
        // chars 0..5 should be highlighted
        for col in 0..5u16 {
            let bg = buf[(text_x + col, e.last_inner.y)].bg;
            assert_eq!(
                bg, selected_bg,
                "cell at col {col} should have selection bg, got {bg:?}"
            );
        }
        // char 5 (the space) is OUTSIDE the selection (head exclusive end)
        let bg5 = buf[(text_x + 5, e.last_inner.y)].bg;
        assert_ne!(bg5, selected_bg, "cell at col 5 should NOT be highlighted");
    }

    #[test]
    fn render_paints_selection_band_across_multiple_lines() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("first\nsecond\nthird");
        e.cursor_row = 0;
        e.cursor_col = 2;
        e.start_selection_at_cursor();
        e.cursor_row = 2;
        e.cursor_col = 2;
        e.extend_selection_to_cursor();
        e.focused = true;

        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);

        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let selected_bg = Color::Rgb(0x26, 0x4f, 0x78);

        // Row 0: cols 2..end (all the way past the end of "first")
        let row0_y = e.last_inner.y;
        assert_eq!(buf[(text_x + 2, row0_y)].bg, selected_bg, "row 0 col 2");
        assert_eq!(buf[(text_x + 4, row0_y)].bg, selected_bg, "row 0 col 4");
        assert_ne!(
            buf[(text_x, row0_y)].bg,
            selected_bg,
            "row 0 col 0 not selected"
        );

        // Row 1 (full line "second"): all cells in selection
        let row1_y = e.last_inner.y + 1;
        assert_eq!(buf[(text_x, row1_y)].bg, selected_bg, "row 1 col 0");
        assert_eq!(buf[(text_x + 5, row1_y)].bg, selected_bg, "row 1 col 5");

        // Row 2 (last line "third"): cols 0..2 in selection
        let row2_y = e.last_inner.y + 2;
        assert_eq!(buf[(text_x, row2_y)].bg, selected_bg, "row 2 col 0");
        assert_eq!(buf[(text_x + 1, row2_y)].bg, selected_bg, "row 2 col 1");
        assert_ne!(
            buf[(text_x + 2, row2_y)].bg,
            selected_bg,
            "row 2 col 2 not selected"
        );
    }

    #[test]
    fn double_click_selects_word_and_moves_cursor_to_end() {
        let mut e = editor_with("hello world");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        let text_x: u16 = 3;
        e.select_word_at(text_x + 7, 0);
        let sel = e.selection.expect("word selection created");
        assert_eq!(sel.normalised(), ((0, 6), (0, 11)));
        assert_eq!(e.cursor_col, 11);
    }

    #[test]
    fn double_click_on_first_word_selects_it_from_column_zero() {
        let mut e = editor_with("foo bar");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        let text_x: u16 = 3;
        e.select_word_at(text_x + 1, 0);
        let sel = e.selection.unwrap();
        assert_eq!(sel.normalised(), ((0, 0), (0, 3)));
        assert_eq!(e.cursor_col, 3);
    }

    #[test]
    fn double_click_on_whitespace_does_not_create_a_selection() {
        let mut e = editor_with("hello world");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        let text_x: u16 = 3;
        e.select_word_at(text_x + 5, 0);
        assert!(
            e.selection.map(|s| !s.has_area()).unwrap_or(true),
            "whitespace double-click must not start a non-empty selection"
        );
    }

    #[test]
    fn double_click_past_end_of_line_extends_last_word() {
        let mut e = editor_with("foo");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        let text_x: u16 = 3;
        e.select_word_at(text_x + 12, 0);
        let sel = e.selection.unwrap();
        assert_eq!(sel.normalised(), ((0, 0), (0, 3)));
        assert_eq!(e.cursor_col, 3);
    }

    #[test]
    fn editor_tabs_starts_with_one_empty_editor() {
        let t = EditorTabs::new();
        assert_eq!(t.tab_count(), 1);
        assert_eq!(t.active_index(), 0);
        assert!(t.path.is_none());
    }

    #[test]
    fn editor_tabs_open_in_new_tab_appends_and_activates() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/tmp/a.rs"));
        t.add_tab_with_path(std::path::PathBuf::from("/tmp/b.rs"));
        assert_eq!(t.tab_count(), 2);
        assert_eq!(t.active_index(), 1);
        assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/tmp/b.rs")));
    }

    #[test]
    fn editor_tabs_new_tab_lands_immediately_after_active() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        // active is now 2 (c). Switch back to a (idx 0), open d → should be at idx 1.
        t.select(0);
        t.add_tab_with_path(std::path::PathBuf::from("/d"));
        let labels: Vec<_> = t
            .iter_tabs()
            .map(|e| e.path.as_ref().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(labels, vec!["/a", "/d", "/b", "/c"]);
        assert_eq!(t.active_index(), 1);
    }

    #[test]
    fn editor_tabs_close_active_drops_tab_and_reselects() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        t.select(1); // active = b
        assert!(t.close_active());
        assert_eq!(t.tab_count(), 2);
        // After closing the middle tab, the next tab takes its slot, which is c.
        assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/c")));
    }

    #[test]
    fn editor_tabs_close_last_tab_resets_buffer_to_blank() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        assert!(
            t.close_active(),
            "closing the only tab resets it instead of refusing"
        );
        assert_eq!(t.tab_count(), 1);
        assert!(t.path.is_none());
    }

    #[test]
    fn close_at_maps_a_click_on_the_x_glyph_to_its_tab_index() {
        use ratatui::buffer::Buffer;
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
        t.add_tab_with_path(std::path::PathBuf::from("/bar.rs"));
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let cx0 = t
            .close_screen_x(0)
            .expect("tab 0 has a close button when count > 1");
        let cx1 = t.close_screen_x(1).expect("tab 1 has a close button");
        assert_eq!(t.close_at(cx0, area.y), Some(0));
        assert_eq!(t.close_at(cx1, area.y), Some(1));
        // A click on a non-close cell of the tab still routes to `tab_at`,
        // not `close_at`.
        let (tab0_x, _) = t.tab_screen_x(0).unwrap();
        assert_ne!(tab0_x, cx0);
        assert_eq!(t.close_at(tab0_x, area.y), None);
        // Clicks outside the strip row are not close clicks.
        assert_eq!(t.close_at(cx0, area.y + 2), None);
    }

    #[test]
    fn hovering_the_close_cross_paints_a_pill_on_that_cell() {
        use ratatui::buffer::Buffer;
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
        t.add_tab_with_path(std::path::PathBuf::from("/bar.rs"));
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        // First render to learn where the close crosses landed.
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let cx = t.close_screen_x(0).expect("two tabs -> tab 0 has a cross");
        let other = t.close_screen_x(1).expect("tab 1 has a cross");
        // Hover the first tab's cross and re-render.
        t.hover_pointer = Some((cx, area.y));
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let pill = crate::theme::Theme::default().tab_close_pill_bg();
        assert_eq!(
            buf[(cx, area.y)].bg,
            pill,
            "the hovered cross cell wears the pill"
        );
        assert_eq!(buf[(cx, area.y)].fg, Color::White);
        assert_eq!(
            buf[(cx, area.y)].symbol(),
            "\u{2715}",
            "the pill sits behind the cross glyph, not a blank cell"
        );
        assert_ne!(
            buf[(other, area.y)].bg,
            pill,
            "an un-hovered cross gets no pill"
        );
    }

    #[test]
    fn black_theme_close_pill_is_distinct_from_the_active_tab_bg() {
        use ratatui::buffer::Buffer;
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
        t.editors[0].focus_gradient = true; // turn on the Black theme
        t.add_tab_with_path(std::path::PathBuf::from("/bar.rs"));
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let active = t.active;
        let cx = t
            .close_screen_x(active)
            .expect("the active tab has a close cross");
        t.hover_pointer = Some((cx, area.y));
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let black = crate::theme::Theme::from_id("black");
        assert_eq!(
            black.tab_active_bg(),
            crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG),
            "the Black theme's active chip is still the brand selection teal"
        );
        assert_eq!(
            buf[(cx, area.y)].bg,
            black.tab_close_pill_bg(),
            "the Black-theme close pill uses the brighter teal"
        );
        assert_ne!(
            buf[(cx, area.y)].bg,
            black.tab_active_bg(),
            "the pill must not equal the active tab bg, or the cross shows no hover at all"
        );
    }

    #[test]
    fn hovering_an_inactive_tab_lifts_its_body_not_the_active_one() {
        use ratatui::buffer::Buffer;
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
        // Adding a tab makes the new one (index 1) active; index 0 is inactive.
        t.add_tab_with_path(std::path::PathBuf::from("/bar.rs"));
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let (x0, _) = t.tab_screen_x(0).unwrap();
        let (x1, _) = t.tab_screen_x(1).unwrap();
        // Hover the inactive tab's label area (one cell past its left pad).
        t.hover_pointer = Some((x0 + 1, area.y));
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let hover = crate::theme::Theme::default().tab_hover_bg();
        assert_eq!(
            buf[(x0 + 1, area.y)].bg,
            hover,
            "an inactive tab body lifts under the pointer"
        );
        assert_ne!(
            buf[(x1 + 1, area.y)].bg,
            hover,
            "the active tab keeps its own bg when a sibling is hovered"
        );
    }

    /// The ten manifest themes recolor the editor body; the tab strip must
    /// follow, not stay the hardcoded VS-Code navy of the two built-ins. A
    /// theme without explicit tab colors derives them: the strip wears the
    /// theme's secondary panel fill and the active tab its selection fill.
    #[test]
    fn tab_strip_follows_the_selected_theme() {
        use ratatui::buffer::Buffer;
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
        let solarized = crate::theme::Theme::from_id("solarized-dark");
        assert_eq!(solarized.id(), "solarized-dark", "bundled theme resolves");
        t.editors[0].theme = solarized;
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        assert_eq!(
            buf[(area.width - 1, 0)].bg,
            solarized.search_bg(),
            "the gap right of the last tab wears the theme's strip color"
        );
        let (x0, _) = t.tab_screen_x(0).unwrap();
        assert_eq!(
            buf[(x0 + 1, 0)].bg,
            solarized.selection(),
            "the active tab wears the theme's selection fill, not the navy chip"
        );
    }

    #[test]
    fn close_tab_removes_specific_index_and_keeps_active_pointing_correctly() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        // active = 2 (c). Closing tab 1 (b) shifts c to index 1; active follows.
        assert!(t.close_tab(1));
        assert_eq!(t.tab_count(), 2);
        assert_eq!(t.active_index(), 1);
        assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/c")));
    }

    #[test]
    fn close_others_keeps_only_the_chosen_tab_and_makes_it_active() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        // Keep "b" (originally index 1). The other two must go.
        let removed = t.close_others(1);
        assert_eq!(removed, 2, "close_others must report how many were dropped");
        assert_eq!(t.tab_count(), 1);
        assert_eq!(t.active_index(), 0);
        assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/b")));
        assert!(t.focused, "the surviving tab takes focus");
    }

    #[test]
    fn toggle_pin_keeps_pinned_tabs_leftmost_and_follows_the_active_tab() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c")); // active = /c (idx 2)
        // Pin /c: it joins the (empty) pinned block at the front and stays
        // active even though its index changed from 2 to 0.
        assert!(t.toggle_pin(2));
        assert!(t.is_pinned(0));
        assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/c")));
        assert_eq!(t.active_index(), 0, "the active tab follows its editor");
        assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/a")));
        assert_eq!(t.tab_path(2).as_deref(), Some(std::path::Path::new("/b")));
        // Pinning /b too lands it at the END of the pinned block (after /c).
        let b_idx = 2;
        assert!(t.toggle_pin(b_idx));
        assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/c")));
        assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/b")));
        assert!(t.is_pinned(0) && t.is_pinned(1) && !t.is_pinned(2));
        // Unpinning /c returns it to the front of the unpinned block.
        assert!(!t.toggle_pin(0));
        assert!(t.is_pinned(0), "/b is still pinned and stays leftmost");
        assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/b")));
        assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/c")));
    }

    #[test]
    fn close_others_keeps_pinned_tabs_alongside_the_kept_tab() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        t.add_tab_with_path(std::path::PathBuf::from("/d"));
        t.toggle_pin(0); // pin /a (stays leftmost); order [/a*, /b, /c, /d]
        let removed = t.close_others(2); // keep /c
        assert_eq!(removed, 2, "only the unpinned non-kept tabs (/b, /d) close");
        assert_eq!(t.tab_count(), 2);
        assert!(t.is_pinned(0));
        assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/a")));
        assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/c")));
        assert_eq!(t.active_index(), 1, "the kept tab is active");
    }

    #[test]
    fn close_to_right_keeps_pinned_tabs_to_the_right_of_the_pivot() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        t.add_tab_with_path(std::path::PathBuf::from("/d"));
        t.toggle_pin(0); // pin /a
        t.toggle_pin(1); // pin /b -> end of pinned block; order [/a*, /b*, /c, /d]
        let removed = t.close_to_right(0); // close to the right of /a
        assert_eq!(
            removed, 2,
            "/c and /d close; the pinned /b to the right survives"
        );
        assert_eq!(t.tab_count(), 2);
        assert!(t.is_pinned(0) && t.is_pinned(1));
        assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/a")));
        assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/b")));
        assert_eq!(t.active_index(), 0, "the pivot tab is active");
    }

    #[test]
    fn keep_open_promotes_a_preview_tab_and_returns_true() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.editors[0].preview = true;
        assert!(t.is_preview(0));
        assert!(t.keep_open(0), "promoting a preview tab returns true");
        assert!(!t.is_preview(0), "the tab is no longer the preview slot");
    }

    #[test]
    fn keep_open_is_a_noop_on_an_already_permanent_tab() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.editors[0].preview = false;
        assert!(
            !t.keep_open(0),
            "a permanent tab returns false (nothing to promote)"
        );
    }

    #[test]
    fn keep_open_out_of_range_is_false() {
        let mut t = EditorTabs::new();
        assert!(!t.keep_open(99));
    }

    #[test]
    fn keep_open_only_affects_the_target_tab() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.editors[0].preview = true;
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.editors[1].preview = true;
        t.keep_open(0);
        assert!(!t.is_preview(0));
        assert!(t.is_preview(1), "the sibling's preview flag is untouched");
    }

    #[test]
    fn is_preview_and_is_pinned_are_false_out_of_range() {
        let t = EditorTabs::new();
        assert!(!t.is_preview(99));
        assert!(!t.is_pinned(99));
    }

    #[test]
    fn toggle_pin_clears_the_preview_flag_when_pinning() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.editors[0].preview = true;
        assert!(t.toggle_pin(0), "pinning returns the new state = true");
        assert!(t.is_pinned(0));
        assert!(!t.is_preview(0), "a pinned tab is never the preview slot");
    }

    #[test]
    fn toggle_pin_returns_new_state_and_round_trips() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        assert!(t.toggle_pin(0), "first toggle pins");
        assert!(!t.toggle_pin(0), "second toggle unpins");
        assert!(!t.is_pinned(0));
    }

    #[test]
    fn toggle_pin_out_of_range_is_a_noop_and_false() {
        let mut t = EditorTabs::new();
        let before = t.tab_count();
        assert!(!t.toggle_pin(99));
        assert_eq!(t.tab_count(), before, "no panic, no mutation");
    }

    #[test]
    fn close_all_closes_pinned_tabs_too() {
        // VS Code parity: Close All does not spare pinned tabs.
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.toggle_pin(0);
        let removed = t.close_all();
        assert_eq!(removed, 2);
        assert_eq!(t.tab_count(), 1, "collapses to a single blank tab");
        assert!(!t.is_pinned(0));
    }

    #[test]
    fn close_saved_closes_clean_pinned_tabs() {
        // VS Code parity: Close Saved closes a clean tab even when pinned;
        // only the dirty tab survives.
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.editors[1].dirty = true;
        t.toggle_pin(0); // pin the clean /a
        let removed = t.close_saved();
        assert_eq!(removed, 1, "the clean pinned tab is still closed");
        assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/b")));
    }

    #[test]
    fn pinned_tab_paints_the_thumb_tack_instead_of_the_close_cross() {
        let f1 = NamedTempFile::new().unwrap();
        let f2 = NamedTempFile::new().unwrap();
        std::fs::write(f1.path(), "1\n").unwrap();
        std::fs::write(f2.path(), "2\n").unwrap();
        let mut t = EditorTabs::new();
        t.open_pinned(f1.path()).unwrap();
        t.open_pinned(f2.path()).unwrap();
        t.toggle_pin(0); // pin the first tab (stays leftmost)
        let active_idx = t.active_index();
        t.editors[active_idx].focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let strip: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(
            strip.contains('\u{f08d}'),
            "the pinned tab shows the thumb-tack glyph in its close cell; strip was {strip:?}"
        );
        assert!(
            strip.contains('\u{2715}'),
            "the unpinned tab still shows the close cross; strip was {strip:?}"
        );
    }

    #[test]
    fn take_active_editor_removes_and_returns_the_active_tab() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b")); // active = /b
        let ed = t.take_active_editor();
        assert_eq!(ed.path.as_deref(), Some(std::path::Path::new("/b")));
        assert_eq!(t.tab_count(), 1);
        assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/a")));
    }

    #[test]
    fn take_active_editor_on_the_last_tab_leaves_a_blank_group() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        let ed = t.take_active_editor();
        assert_eq!(ed.path.as_deref(), Some(std::path::Path::new("/a")));
        assert_eq!(t.tab_count(), 1);
        assert!(t.is_blank_initial(), "the group falls back to a blank tab");
    }

    #[test]
    fn push_editor_replaces_a_blank_group_then_appends() {
        let mut src = EditorTabs::new();
        src.editors[0].path = Some(std::path::PathBuf::from("/a"));
        let ed = src.take_active_editor();
        let mut dst = EditorTabs::new(); // blank-initial
        dst.push_editor(ed);
        assert_eq!(dst.tab_count(), 1, "pushing into a blank group replaces it");
        assert_eq!(dst.tab_path(0).as_deref(), Some(std::path::Path::new("/a")));
        assert_eq!(dst.active_index(), 0);
        let mut src2 = EditorTabs::new();
        src2.editors[0].path = Some(std::path::PathBuf::from("/b"));
        let ed2 = src2.take_active_editor();
        dst.push_editor(ed2);
        assert_eq!(dst.tab_count(), 2, "a second push appends and activates it");
        assert_eq!(dst.active_index(), 1);
        assert_eq!(dst.tab_path(1).as_deref(), Some(std::path::Path::new("/b")));
    }

    #[test]
    fn close_others_is_a_noop_when_only_one_tab_is_open() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/only"));
        let removed = t.close_others(0);
        assert_eq!(removed, 0, "nothing to drop");
        assert_eq!(t.tab_count(), 1);
        assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/only")));
    }

    #[test]
    fn close_to_right_drops_only_tabs_past_the_pivot_and_keeps_left_side() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        t.add_tab_with_path(std::path::PathBuf::from("/d"));
        // Pivot on "b" (index 1). c and d disappear; a and b stay.
        let removed = t.close_to_right(1);
        assert_eq!(removed, 2);
        assert_eq!(t.tab_count(), 2);
        assert_eq!(
            t.editors[0].path.as_deref(),
            Some(std::path::Path::new("/a")),
            "left-side tabs are untouched"
        );
        assert_eq!(
            t.editors[1].path.as_deref(),
            Some(std::path::Path::new("/b"))
        );
        assert!(
            t.active_index() < t.tab_count(),
            "active must still point at a valid tab"
        );
    }

    #[test]
    fn close_to_right_at_last_tab_is_a_noop() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        let before = t.tab_count();
        let removed = t.close_to_right(1);
        assert_eq!(removed, 0);
        assert_eq!(t.tab_count(), before);
    }

    #[test]
    fn close_all_collapses_to_a_single_blank_tab() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        let removed = t.close_all();
        assert_eq!(removed, 3, "report how many tabs were collapsed");
        assert_eq!(t.tab_count(), 1, "always at least one tab survives");
        assert!(t.path.is_none(), "the surviving tab is a fresh blank slot");
    }

    #[test]
    fn close_saved_drops_clean_tabs_and_keeps_dirty_ones() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        // Mark "b" (index 1) as having unsaved changes; a and c are saved.
        t.editors[1].dirty = true;
        let removed = t.close_saved();
        assert_eq!(removed, 2, "the two saved tabs (a, c) are closed");
        assert_eq!(t.tab_count(), 1, "only the dirty tab survives");
        assert_eq!(
            t.editors[0].path.as_deref(),
            Some(std::path::Path::new("/b")),
            "the kept tab is the dirty one"
        );
        assert_eq!(t.active_index(), 0, "the kept tab becomes active");
        assert!(t.editors[0].focused, "the kept tab is focused");
    }

    #[test]
    fn close_saved_collapses_to_blank_when_no_tab_is_dirty() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        let removed = t.close_saved();
        assert_eq!(removed, 2, "both saved tabs are closed");
        assert_eq!(t.tab_count(), 1, "always at least one tab survives");
        assert!(t.path.is_none(), "the survivor is a fresh blank slot");
    }

    #[test]
    fn close_saved_is_a_noop_when_every_tab_is_dirty() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.editors[0].dirty = true;
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.editors[1].dirty = true;
        let removed = t.close_saved();
        assert_eq!(removed, 0, "nothing saved means nothing to close");
        assert_eq!(t.tab_count(), 2, "both dirty tabs stay open");
    }

    #[test]
    fn close_tab_on_last_remaining_tab_resets_to_blank_buffer() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/only.rs"));
        t.editors[0].lines = vec!["something".to_string()];
        t.editors[0].dirty = true;
        assert!(t.close_tab(0));
        assert_eq!(t.tab_count(), 1);
        assert!(t.path.is_none());
        assert!(t.lines.is_empty() || t.lines == vec![String::new()]);
        assert!(!t.dirty);
    }

    #[test]
    fn close_screen_x_is_present_even_for_a_single_open_tab() {
        use ratatui::buffer::Buffer;
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/only.rs"));
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 5,
        };
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let close_x = t.close_screen_x(0).expect("single tab still shows X");
        assert_eq!(t.close_at(close_x, area.y), Some(0));
    }

    #[test]
    fn editor_tabs_tab_at_x_returns_index_of_clicked_tab() {
        use ratatui::buffer::Buffer;
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/long_name.rs"));
        t.add_tab_with_path(std::path::PathBuf::from("/b.rs"));
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let first = t.tab_screen_x(0).expect("tab 0 laid out");
        let second = t.tab_screen_x(1).expect("tab 1 laid out");
        assert_eq!(t.tab_at(first.0, area.y), Some(0));
        assert_eq!(t.tab_at(second.0, area.y), Some(1));
        assert_eq!(t.tab_at(area.x + area.width - 1, area.y + 5), None);
    }

    #[test]
    fn open_preview_creates_a_preview_tab_when_none_exists() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.editors[0].preview = false;
        // Stand in for a real disk open: simulate the side-effects directly.
        t.add_preview_tab_with_path(std::path::PathBuf::from("/b"));
        assert_eq!(t.tab_count(), 2);
        assert_eq!(t.preview_index(), Some(1));
        assert_eq!(t.active_index(), 1);
    }

    #[test]
    fn open_preview_reuses_existing_preview_slot_for_a_new_path() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.editors[0].preview = false;
        t.add_preview_tab_with_path(std::path::PathBuf::from("/b"));
        // Now repoint the preview tab at /c — count must NOT grow.
        t.repoint_preview_to(std::path::PathBuf::from("/c"));
        assert_eq!(t.tab_count(), 2);
        assert_eq!(t.preview_index(), Some(1));
        assert_eq!(
            t.editors[1].path.as_deref(),
            Some(std::path::Path::new("/c"))
        );
    }

    #[test]
    fn open_preview_reuses_blank_initial_tab_instead_of_stacking() {
        let mut t = EditorTabs::new();
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "hello").unwrap();
        t.open_preview(f.path()).unwrap();
        assert_eq!(t.tab_count(), 1, "blank initial tab must be reused");
        assert_eq!(t.preview_index(), Some(0));
        assert_eq!(t.path.as_deref(), Some(f.path()));
    }

    #[test]
    fn tab_labels_disambiguate_colliding_names_with_the_shortest_dir_context() {
        // #167, VS Code labelFormat: colliding basenames gain the shortest
        // distinguishing trailing directory; unique names stay bare.
        let tmp = tempfile::tempdir().unwrap();
        for d in ["alpha", "beta", "a/x", "b/x"] {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        for f in [
            "alpha/main.rs",
            "beta/main.rs",
            "solo.rs",
            "a/x/mod.rs",
            "b/x/mod.rs",
        ] {
            std::fs::write(tmp.path().join(f), "x\n").unwrap();
        }
        let mut tabs = EditorTabs::new();
        for f in [
            "alpha/main.rs",
            "beta/main.rs",
            "solo.rs",
            "a/x/mod.rs",
            "b/x/mod.rs",
        ] {
            tabs.open_pinned(&tmp.path().join(f)).unwrap();
        }
        let labels = tabs.tab_display_labels();
        assert_eq!(labels[0], "main.rs — alpha");
        assert_eq!(labels[1], "main.rs — beta");
        assert_eq!(labels[2], "solo.rs", "a unique name stays bare");
        // Parents collide (`x` vs `x`), so the suffix walks one level up.
        assert_eq!(labels[3], "mod.rs — a/x");
        assert_eq!(labels[4], "mod.rs — b/x");
    }

    #[test]
    fn tab_labels_disambiguate_deep_shared_tails_and_measure_wide_names_in_cells() {
        // #168 review: a fixed depth cap left paths sharing a deep tail
        // identical, and char-count width shifted hit ranges for wide
        // (CJK) names.
        let tmp = tempfile::tempdir().unwrap();
        let deep = "d01/d02/d03/d04/d05/d06/d07/d08/d09/d10/d11/d12/d13/d14/d15/d16/d17";
        let a = tmp.path().join("alpha").join(deep);
        let b = tmp.path().join("beta").join(deep);
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("main.rs"), "a\n").unwrap();
        std::fs::write(b.join("main.rs"), "b\n").unwrap();
        let wide = tmp.path().join("日本語");
        std::fs::create_dir_all(&wide).unwrap();
        std::fs::write(wide.join("main.rs"), "w\n").unwrap();

        let mut tabs = EditorTabs::new();
        tabs.open_pinned(&a.join("main.rs")).unwrap();
        tabs.open_pinned(&b.join("main.rs")).unwrap();
        tabs.open_pinned(&wide.join("main.rs")).unwrap();
        let labels = tabs.tab_display_labels();
        let mut seen = std::collections::HashSet::new();
        assert!(
            labels.iter().all(|l| seen.insert(l.clone())),
            "every colliding tab gets a distinct title, however deep the shared tail: {labels:?}"
        );
        assert!(
            labels[2].contains("日本語"),
            "the wide dir name suffixes too: {labels:?}"
        );
        // The strip measures in display cells: a render must place the
        // close glyph inside each tab's recorded range (frame truth).
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 600,
            height: 20,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        use ratatui::widgets::Widget;
        (&mut tabs).render(area, &mut buf);
        let ranges: Vec<(u16, u16)> = tabs.tab_screen_ranges.clone();
        for (i, (x, w)) in ranges.iter().enumerate() {
            if *w == 0 {
                continue;
            }
            let row: String = (*x..*x + *w)
                .map(|cx| buf[(cx, tabs.tab_strip_y_for_test())].symbol().to_string())
                .collect();
            assert!(
                row.contains('✕'),
                "tab {i}'s close glyph must sit inside its hit range; range painted: {row:?}"
            );
        }
    }

    #[test]
    fn open_pinned_reuses_blank_initial_tab_instead_of_stacking() {
        let mut t = EditorTabs::new();
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "hello").unwrap();
        t.open_pinned(f.path()).unwrap();
        assert_eq!(t.tab_count(), 1, "blank initial tab must be reused");
        assert!(
            t.preview_index().is_none(),
            "pinned open must not leave preview state"
        );
    }

    #[test]
    fn open_preview_drops_stale_preview_when_switching_to_pinned_tab() {
        let mut t = EditorTabs::new();
        let mut f1 = NamedTempFile::new().unwrap();
        write!(f1, "a").unwrap();
        let mut f2 = NamedTempFile::new().unwrap();
        write!(f2, "b").unwrap();
        t.open_preview(f1.path()).unwrap(); // preview slot = f1, only tab
        t.pin_active(); // f1 pinned
        t.open_preview(f2.path()).unwrap(); // preview tab for f2 alongside pinned f1
        assert_eq!(t.tab_count(), 2);
        t.open_preview(f1.path()).unwrap(); // back to f1 → stale f2 preview must vanish
        assert_eq!(t.tab_count(), 1);
        assert!(t.preview_index().is_none());
        assert_eq!(t.path.as_deref(), Some(f1.path()));
    }

    #[test]
    fn pin_active_clears_preview_flag() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_preview_tab_with_path(std::path::PathBuf::from("/b"));
        assert_eq!(t.preview_index(), Some(1));
        t.pin_active();
        assert!(
            t.preview_index().is_none(),
            "no tab should be in preview state"
        );
        assert!(!t.editors[1].preview);
    }

    #[test]
    fn editor_tabs_find_tab_with_path_returns_index_when_open() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        assert_eq!(t.find_tab_with_path(std::path::Path::new("/a")), Some(0));
        assert_eq!(t.find_tab_with_path(std::path::Path::new("/b")), Some(1));
        assert_eq!(t.find_tab_with_path(std::path::Path::new("/c")), Some(2));
        assert_eq!(t.find_tab_with_path(std::path::Path::new("/missing")), None);
    }

    #[test]
    fn editor_tabs_deref_exposes_active_editor_state() {
        let mut t = EditorTabs::new();
        t.lines = vec!["abc".to_string()];
        t.cursor_col = 3;
        // Field access reaches active editor via DerefMut.
        assert_eq!(t.lines, vec!["abc".to_string()]);
        assert_eq!(t.cursor_col, 3);
    }

    #[test]
    fn mouse_drag_extends_selection() {
        let mut e = editor_with("hello world");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        let text_x: u16 = 3;
        e.mouse_down(text_x, 0);
        e.mouse_drag(text_x + 5, 0);
        let sel = e.selection.unwrap();
        assert_eq!(sel.anchor, (0, 0));
        assert_eq!(sel.head, (0, 5));
        assert_eq!(e.cursor_col, 5);
    }

    #[test]
    fn editor_tabs_rename_open_path_updates_matching_tab_only() {
        // Renaming a file on disk must update the matching tab so the
        // editor keeps tracking the entry. Other tabs must be untouched.
        let mut tabs = EditorTabs::new();
        tabs.add_tab_with_path(std::path::PathBuf::from("/work/old.txt"));
        tabs.add_tab_with_path(std::path::PathBuf::from("/work/other.txt"));
        let old = std::path::PathBuf::from("/work/old.txt");
        let new = std::path::PathBuf::from("/work/renamed.txt");
        tabs.rename_open_path(&old, &new);
        let paths: Vec<Option<std::path::PathBuf>> =
            tabs.iter_tabs().map(|e| e.path.clone()).collect();
        assert!(paths.contains(&Some(new.clone())));
        assert!(!paths.contains(&Some(old)));
        assert!(paths.contains(&Some(std::path::PathBuf::from("/work/other.txt"))));
    }

    #[test]
    fn editor_tabs_rename_open_path_is_noop_when_path_not_open() {
        let mut tabs = EditorTabs::new();
        tabs.add_tab_with_path(std::path::PathBuf::from("/work/a.txt"));
        let before: Vec<Option<std::path::PathBuf>> =
            tabs.iter_tabs().map(|e| e.path.clone()).collect();
        tabs.rename_open_path(
            std::path::Path::new("/work/never-opened.txt"),
            std::path::Path::new("/work/whatever.txt"),
        );
        let after: Vec<Option<std::path::PathBuf>> =
            tabs.iter_tabs().map(|e| e.path.clone()).collect();
        assert_eq!(before, after);
    }

    // ---- Move Line Up / Down (Alt+Up / Alt+Down) ----

    #[test]
    fn move_line_down_swaps_with_next_and_follows_cursor() {
        let mut e = editor_with("a\nb\nc");
        e.cursor_row = 0;
        e.move_lines_down();
        assert_eq!(e.lines, vec!["b", "a", "c"]);
        assert_eq!(e.cursor_row, 1, "cursor follows the moved line");
    }

    #[test]
    fn move_line_up_swaps_with_previous_and_follows_cursor() {
        let mut e = editor_with("a\nb\nc");
        e.cursor_row = 2;
        e.move_lines_up();
        assert_eq!(e.lines, vec!["a", "c", "b"]);
        assert_eq!(e.cursor_row, 1);
    }

    #[test]
    fn move_line_up_at_top_is_noop() {
        let mut e = editor_with("a\nb");
        e.cursor_row = 0;
        e.move_lines_up();
        assert_eq!(e.lines, vec!["a", "b"]);
        assert_eq!(e.cursor_row, 0);
    }

    #[test]
    fn move_line_down_at_bottom_is_noop() {
        let mut e = editor_with("a\nb");
        e.cursor_row = 1;
        e.move_lines_down();
        assert_eq!(e.lines, vec!["a", "b"]);
        assert_eq!(e.cursor_row, 1);
    }

    #[test]
    fn move_line_down_moves_whole_selected_block() {
        let mut e = editor_with("a\nb\nc\nd");
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (1, 1),
        });
        e.cursor_row = 1;
        e.move_lines_down();
        assert_eq!(e.lines, vec!["c", "a", "b", "d"]);
        assert_eq!(e.cursor_row, 2);
        let sel = e.selection.unwrap();
        assert_eq!((sel.anchor.0, sel.head.0), (1, 2), "selection rows shift");
    }

    // ---- Toggle Line Comment (Cmd+/) ----

    #[test]
    fn toggle_line_comment_adds_rust_token() {
        let mut e = editor_with("    let x = 1;");
        e.lang = Some(LangKind::Rust);
        e.cursor_row = 0;
        assert!(e.toggle_line_comment());
        assert_eq!(e.lines, vec!["    // let x = 1;"]);
    }

    #[test]
    fn toggle_line_comment_removes_existing_rust_token() {
        let mut e = editor_with("    // let x = 1;");
        e.lang = Some(LangKind::Rust);
        e.cursor_row = 0;
        assert!(e.toggle_line_comment());
        assert_eq!(e.lines, vec!["    let x = 1;"]);
    }

    #[test]
    fn toggle_line_comment_python_hash() {
        let mut e = editor_with("x = 1");
        e.lang = Some(LangKind::Python);
        e.cursor_row = 0;
        assert!(e.toggle_line_comment());
        assert_eq!(e.lines, vec!["# x = 1"]);
    }

    #[test]
    fn toggle_line_comment_refreshes_highlights() {
        // Commenting a line must re-run tree-sitter so the line repaints as a
        // comment, not keep the stale pre-comment code spans. Without the
        // refresh the rendered comment shows code colours shifted by `# `.
        let mut e = editor_with("x = 1");
        e.lang = Some(LangKind::Python);
        e.recompute_highlights();
        e.cursor_row = 0;
        assert!(e.toggle_line_comment());
        let after: Vec<(usize, usize, Option<Color>)> = e.highlights[0]
            .iter()
            .map(|s| (s.start, s.end, s.style.fg))
            .collect();
        e.recompute_highlights();
        let fresh: Vec<(usize, usize, Option<Color>)> = e.highlights[0]
            .iter()
            .map(|s| (s.start, s.end, s.style.fg))
            .collect();
        assert_eq!(
            after, fresh,
            "toggle_line_comment must refresh syntax highlights to match the new buffer"
        );
    }

    #[test]
    fn toggle_line_comment_block_comments_at_min_indent() {
        let mut e = editor_with("  a\n    b");
        e.lang = Some(LangKind::Python);
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (1, 5),
        });
        assert!(e.toggle_line_comment());
        assert_eq!(e.lines, vec!["  # a", "  #   b"]);
    }

    #[test]
    fn toggle_line_comment_block_uncomments_when_all_commented() {
        let mut e = editor_with("  # a\n  #   b");
        e.lang = Some(LangKind::Python);
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (1, 7),
        });
        assert!(e.toggle_line_comment());
        assert_eq!(e.lines, vec!["  a", "    b"]);
    }

    #[test]
    fn toggle_line_comment_unknown_lang_is_noop() {
        let mut e = editor_with("x = 1");
        e.lang = None;
        assert!(!e.toggle_line_comment());
        assert_eq!(e.lines, vec!["x = 1"]);
    }

    // ---- Toggle Block Comment (Shift+Alt+A) ----

    #[test]
    fn toggle_block_comment_wraps_selection() {
        let mut e = editor_with("let x = 1;");
        e.lang = Some(LangKind::Rust);
        e.selection = Some(EditorSelection {
            anchor: (0, 4),
            head: (0, 5),
        });
        assert!(e.toggle_block_comment());
        assert_eq!(e.lines, vec!["let /* x */ = 1;"]);
    }

    #[test]
    fn toggle_block_comment_unwraps_when_already_wrapped() {
        let mut e = editor_with("let /* x */ = 1;");
        e.lang = Some(LangKind::Rust);
        e.selection = Some(EditorSelection {
            anchor: (0, 4),
            head: (0, 11),
        });
        assert!(e.toggle_block_comment());
        assert_eq!(e.lines, vec!["let x = 1;"]);
    }

    #[test]
    fn toggle_block_comment_python_wraps_in_triple_quotes() {
        // VS Code's Python language config defines blockComment as `""" """`,
        // so Shift+Alt+A wraps the selection in a triple-quoted string.
        let mut e = editor_with("x = 1");
        e.lang = Some(LangKind::Python);
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (0, 5),
        });
        assert!(e.toggle_block_comment());
        assert_eq!(e.lines, vec!["\"\"\" x = 1 \"\"\""]);
    }

    #[test]
    fn toggle_block_comment_python_unwraps_triple_quotes() {
        let mut e = editor_with("\"\"\" x = 1 \"\"\"");
        e.lang = Some(LangKind::Python);
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (0, 13),
        });
        assert!(e.toggle_block_comment());
        assert_eq!(e.lines, vec!["x = 1"]);
    }

    // ---- Join Lines ----

    #[test]
    fn join_lines_merges_current_with_next() {
        let mut e = editor_with("hello\n    world");
        e.cursor_row = 0;
        e.join_lines();
        assert_eq!(e.lines, vec!["hello world"]);
    }

    #[test]
    fn join_lines_merges_whole_selection() {
        let mut e = editor_with("a\n  b\n  c\nd");
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (2, 3),
        });
        e.cursor_row = 2;
        e.join_lines();
        assert_eq!(e.lines, vec!["a b c", "d"]);
    }

    #[test]
    fn join_lines_on_last_line_is_noop() {
        let mut e = editor_with("a\nb");
        e.cursor_row = 1;
        e.join_lines();
        assert_eq!(e.lines, vec!["a", "b"]);
    }

    // ---- Transform Case ----

    #[test]
    fn transform_upper_on_selection() {
        let mut e = editor_with("hello world");
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (0, 5),
        });
        e.transform_selection_case(CaseTransform::Upper);
        assert_eq!(e.lines, vec!["HELLO world"]);
    }

    #[test]
    fn transform_lower_on_selection() {
        let mut e = editor_with("HELLO world");
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (0, 5),
        });
        e.transform_selection_case(CaseTransform::Lower);
        assert_eq!(e.lines, vec!["hello world"]);
    }

    #[test]
    fn transform_title_on_selection() {
        let mut e = editor_with("hello there world");
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (0, 17),
        });
        e.transform_selection_case(CaseTransform::Title);
        assert_eq!(e.lines, vec!["Hello There World"]);
    }

    #[test]
    fn transform_upper_falls_back_to_word_at_cursor() {
        let mut e = editor_with("alpha beta");
        e.cursor_row = 0;
        e.cursor_col = 7; // inside "beta"
        e.transform_selection_case(CaseTransform::Upper);
        assert_eq!(e.lines, vec!["alpha BETA"]);
    }

    // ---- Sort Lines ----

    #[test]
    fn sort_lines_ascending_over_selection() {
        let mut e = editor_with("banana\napple\ncherry");
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (2, 6),
        });
        e.sort_lines(true);
        assert_eq!(e.lines, vec!["apple", "banana", "cherry"]);
    }

    #[test]
    fn sort_lines_descending_over_selection() {
        let mut e = editor_with("apple\nbanana\ncherry");
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (2, 6),
        });
        e.sort_lines(false);
        assert_eq!(e.lines, vec!["cherry", "banana", "apple"]);
    }

    #[test]
    fn sort_lines_without_selection_sorts_whole_buffer() {
        let mut e = editor_with("c\na\nb");
        e.selection = None;
        e.sort_lines(true);
        assert_eq!(e.lines, vec!["a", "b", "c"]);
    }

    // ---- Trim Trailing Whitespace ----

    #[test]
    fn trim_trailing_whitespace_strips_every_line() {
        let mut e = editor_with("a   \nb\t\n  c  ");
        assert!(e.trim_trailing_whitespace());
        assert_eq!(e.lines, vec!["a", "b", "  c"]);
    }

    #[test]
    fn trim_trailing_whitespace_clamps_cursor() {
        let mut e = editor_with("abc    ");
        e.cursor_row = 0;
        e.cursor_col = 7;
        assert!(e.trim_trailing_whitespace());
        assert_eq!(e.cursor_col, 3, "cursor clamps to trimmed end");
    }

    #[test]
    fn trim_trailing_whitespace_noop_when_clean() {
        let mut e = editor_with("a\nb");
        assert!(!e.trim_trailing_whitespace());
        assert_eq!(e.lines, vec!["a", "b"]);
    }

    // ---- Insert Cursor Above / Below ----

    #[test]
    fn add_cursor_below_pushes_caret_on_next_row() {
        let mut e = editor_with("aaaa\nbbbb\ncccc");
        e.cursor_row = 0;
        e.cursor_col = 2;
        e.add_cursor_below();
        assert_eq!(e.carets.len(), 1);
        let c = e.carets[0];
        assert_eq!(c.head, (1, 2));
        assert_eq!(c.anchor, (1, 2), "zero-width caret");
    }

    #[test]
    fn add_cursor_above_pushes_caret_on_previous_row() {
        let mut e = editor_with("aaaa\nbbbb\ncccc");
        e.cursor_row = 2;
        e.cursor_col = 3;
        e.add_cursor_above();
        assert_eq!(e.carets.len(), 1);
        assert_eq!(e.carets[0].head, (1, 3));
    }

    #[test]
    fn add_cursor_below_clamps_to_short_line() {
        let mut e = editor_with("aaaa\nbb");
        e.cursor_row = 0;
        e.cursor_col = 4;
        e.add_cursor_below();
        assert_eq!(e.carets[0].head, (1, 2), "column clamps to line length");
    }

    #[test]
    fn add_cursor_below_at_last_row_is_noop() {
        let mut e = editor_with("aaaa\nbbbb");
        e.cursor_row = 1;
        e.cursor_col = 1;
        e.add_cursor_below();
        assert!(e.carets.is_empty());
    }

    #[test]
    fn document_link_at_answers_inside_the_range_for_the_stamped_file_only() {
        use crate::lsp::manager::DocumentLinkItem;
        let mut e = editor_with("// see https://example.com/docs for more");
        e.path = Some(std::path::PathBuf::from("/tmp/a.rs"));
        e.apply_document_links(
            std::path::PathBuf::from("/tmp/a.rs"),
            vec![DocumentLinkItem {
                line: 0,
                character: 7,
                end_line: 0,
                end_character: 31,
                target: String::from("https://example.com/docs"),
            }],
        );
        assert_eq!(e.document_link_at(0, 7), Some("https://example.com/docs"));
        assert_eq!(e.document_link_at(0, 30), Some("https://example.com/docs"));
        assert_eq!(e.document_link_at(0, 31), None, "range end is exclusive");
        assert_eq!(e.document_link_at(0, 6), None);
        // A set stamped for another file never answers.
        e.path = Some(std::path::PathBuf::from("/tmp/b.rs"));
        assert_eq!(e.document_link_at(0, 10), None);
    }

    #[test]
    fn linked_editing_mirrors_typing_and_deletion_across_paired_tags() {
        let mut e = editor_with("<title>\n  text\n</title>");
        // Spans over both tag names, UTF-16 == char cols here.
        e.set_linked_ranges(&[(0, 1, 0, 6), (2, 2, 2, 7)]);
        assert!(e.has_linked_ranges());
        // Type 's' at the end of the opening tag name.
        e.cursor_row = 0;
        e.cursor_col = 6;
        e.insert_char('s');
        assert!(e.mirror_linked_edit(), "the keystroke mirrors");
        assert_eq!(e.lines[0], "<titles>");
        assert_eq!(e.lines[2], "</titles>");
        // Keep typing: positions were resynced, the set is still live.
        e.insert_char('x');
        assert!(e.mirror_linked_edit());
        assert_eq!(e.lines[2], "</titlesx>");
        // Backspace mirrors too.
        e.backspace();
        assert!(e.mirror_linked_edit());
        assert_eq!(e.lines[0], "<titles>");
        assert_eq!(e.lines[2], "</titles>");
        // One Undo reverts the keystroke AND its mirror together, and
        // the restore drops the set so nothing replays on top.
        assert!(e.undo());
        assert!(!e.mirror_linked_edit());
        assert!(!e.has_linked_ranges());
        assert_eq!(e.lines[0], e.lines[0].clone(), "no panic path");
    }

    #[test]
    fn linked_editing_handles_a_same_row_pair_with_shifting_offsets() {
        let mut e = editor_with("<b>hi</b>");
        e.set_linked_ranges(&[(0, 1, 0, 2), (0, 7, 0, 8)]);
        e.cursor_row = 0;
        e.cursor_col = 2; // end of the opening tag name
        e.insert_char('r');
        assert!(e.mirror_linked_edit());
        assert_eq!(e.lines[0], "<br>hi</br>");
        // And again — the closing tag's start shifted and must resync.
        e.insert_char('x');
        assert!(e.mirror_linked_edit());
        assert_eq!(e.lines[0], "<brx>hi</brx>");
    }

    #[test]
    fn linked_editing_drops_the_set_on_delimiters_and_structural_edits() {
        // A delimiter character must not propagate.
        let mut e = editor_with("<div>\n</div>");
        e.set_linked_ranges(&[(0, 1, 0, 4), (1, 2, 1, 5)]);
        e.cursor_row = 0;
        e.cursor_col = 4;
        e.insert_char(' ');
        assert!(!e.mirror_linked_edit());
        assert!(!e.has_linked_ranges(), "a space cleared the set");
        assert_eq!(e.lines[1], "</div>", "the sibling was untouched");

        // A newline inside the range is a multi-row edit: clear.
        let mut e2 = editor_with("<div>\n</div>");
        e2.set_linked_ranges(&[(0, 1, 0, 4), (1, 2, 1, 5)]);
        e2.cursor_row = 0;
        e2.cursor_col = 3;
        e2.insert_newline();
        assert!(!e2.mirror_linked_edit());
        assert!(!e2.has_linked_ranges());
    }

    #[test]
    fn linked_editing_rejects_boundary_deletes_of_the_outside_delimiters() {
        // Backspace with the caret AT the range start deletes the `<`
        // BEFORE the range; the post-edit caret sits outside the span,
        // so nothing may mirror (the old bug wrote "iv" into siblings).
        let mut e = editor_with("<div>\n</div>");
        e.set_linked_ranges(&[(0, 1, 0, 4), (1, 2, 1, 5)]);
        e.cursor_row = 0;
        e.cursor_col = 1;
        e.backspace();
        assert!(!e.mirror_linked_edit());
        assert!(!e.has_linked_ranges());
        assert_eq!(e.lines[0], "div>", "only the user's own edit landed");
        assert_eq!(e.lines[1], "</div>", "the sibling was untouched");

        // Delete-forward with the caret AT the range end deletes the `>`
        // AFTER the range — same rejection, symmetric case.
        let mut e2 = editor_with("<div>\n</div>");
        e2.set_linked_ranges(&[(0, 1, 0, 4), (1, 2, 1, 5)]);
        e2.cursor_row = 0;
        e2.cursor_col = 4;
        e2.delete_forward();
        assert!(!e2.mirror_linked_edit());
        assert!(!e2.has_linked_ranges());
        assert_eq!(e2.lines[0], "<div");
        assert_eq!(e2.lines[1], "</div>", "the sibling was untouched");
    }

    #[test]
    fn linked_editing_ignores_edits_outside_every_range() {
        let mut e = editor_with("<div>body</div>");
        e.set_linked_ranges(&[(0, 1, 0, 4), (0, 11, 0, 14)]);
        e.cursor_row = 0;
        e.cursor_col = 7; // inside "body"
        e.insert_char('!');
        assert!(!e.mirror_linked_edit());
        assert!(
            !e.has_linked_ranges(),
            "an edit outside the set clears it (positions may have shifted)"
        );
        assert_eq!(e.lines[0], "<div>bo!dy</div>");
    }

    #[test]
    fn set_linked_ranges_rejects_multiline_and_singleton_sets() {
        let mut e = editor_with("<a>\n</a>");
        e.set_linked_ranges(&[(0, 1, 1, 2)]);
        assert!(
            !e.has_linked_ranges(),
            "multi-line span filtered, pair too small"
        );
        e.set_linked_ranges(&[(0, 1, 0, 2)]);
        assert!(!e.has_linked_ranges(), "a single range cannot mirror");
    }

    #[test]
    fn expand_selection_syntax_walks_rust_node_ancestry_and_shrink_retraces() {
        let mut e = editor_with("fn main() {\n    let value = compute(1, 2);\n}");
        e.set_language(Some(crate::highlight::LangKind::Rust));
        // Caret inside `value`, nothing selected.
        e.cursor_row = 1;
        e.cursor_col = 9;
        e.validate_expand_stacks();
        assert!(e.expand_selection_syntax(), "first grow");
        let first = e.selection.expect("a selection appeared").normalised();
        assert!(
            first.0 <= (1, 8) && first.1 >= (1, 13),
            "the identifier (or more) is selected: {first:?}"
        );
        assert!(e.expand_selection_syntax(), "second grow");
        let second = e.selection.unwrap().normalised();
        assert!(
            second.0 <= first.0 && second.1 >= first.1 && second != first,
            "each step strictly grows: {first:?} -> {second:?}"
        );
        // Shrink retraces the EXACT ranges, then bottoms out.
        assert!(e.shrink_selection_step());
        assert_eq!(e.selection.unwrap().normalised(), first);
        assert!(e.shrink_selection_step());
        assert!(
            e.selection.is_none(),
            "the gesture's start was a zero-area caret"
        );
        assert!(!e.shrink_selection_step(), "bottom of the stack");
    }

    #[test]
    fn expand_stack_rebuilds_after_an_edit_or_a_cursor_move() {
        let mut e = editor_with("fn main() {\n    let value = 1;\n}");
        e.set_language(Some(crate::highlight::LangKind::Rust));
        e.cursor_row = 1;
        e.cursor_col = 9;
        e.validate_expand_stacks();
        assert!(e.expand_selection_syntax());
        // A manual cursor move away from the stack's step invalidates it:
        // shrink from the new spot has nothing to retrace.
        e.clear_selection();
        e.cursor_row = 0;
        e.cursor_col = 0;
        assert!(!e.shrink_selection_step(), "stack rebuilt at the new spot");
    }

    #[test]
    fn install_selection_chains_keeps_only_strictly_growing_containing_spans() {
        let mut e = editor_with("alpha beta\ngamma");
        e.cursor_row = 0;
        e.cursor_col = 7; // inside "beta"
        e.validate_expand_stacks();
        // UTF-16 chain: the word, a NON-containing span (filtered), the
        // line, a repeat of the line (filtered), both lines.
        e.install_selection_chains(vec![vec![
            (0, 6, 0, 10),
            (0, 0, 0, 5),
            (0, 0, 0, 10),
            (0, 0, 0, 10),
            (0, 0, 1, 5),
        ]]);
        assert!(e.expand_selection_from_stack());
        assert_eq!(e.selection.unwrap().normalised(), ((0, 6), (0, 10)));
        assert!(e.expand_selection_from_stack());
        assert_eq!(e.selection.unwrap().normalised(), ((0, 0), (0, 10)));
        assert!(e.expand_selection_from_stack());
        assert_eq!(e.selection.unwrap().normalised(), ((0, 0), (1, 5)));
        assert!(!e.expand_selection_from_stack(), "chain exhausted");
    }

    #[test]
    fn textual_fallback_grows_to_line_then_whole_buffer_without_a_grammar() {
        let mut e = editor_with("first line here\nsecond");
        e.cursor_row = 0;
        e.cursor_col = 6;
        e.validate_expand_stacks();
        assert!(e.expand_selection_syntax());
        assert_eq!(
            e.selection.unwrap().normalised(),
            ((0, 0), (0, 15)),
            "no grammar: the line first"
        );
        assert!(e.expand_selection_syntax());
        assert_eq!(
            e.selection.unwrap().normalised(),
            ((0, 0), (1, 6)),
            "then the whole buffer"
        );
        assert!(!e.expand_selection_syntax(), "nothing larger exists");
    }

    #[test]
    fn expand_selection_grows_every_cursor_in_a_multi_cursor_set() {
        let mut e = editor_with("let alpha = 1;\nlet beta = 2;");
        e.set_language(Some(crate::highlight::LangKind::Rust));
        e.cursor_row = 0;
        e.cursor_col = 5; // inside alpha
        e.carets = vec![EditorSelection::new(1, 5)]; // inside beta
        e.validate_expand_stacks();
        assert!(e.expand_selection_syntax());
        let primary = e.selection.expect("primary grew").normalised();
        assert_eq!(primary.0.0, 0, "primary stays on its own row");
        assert_eq!(e.carets.len(), 1);
        let caret = e.carets[0].normalised();
        assert!(
            caret.0.0 == 1 && caret.1.1 > caret.0.1,
            "the secondary caret grew on ITS row: {caret:?}"
        );
    }

    #[test]
    fn document_colors_splice_a_swatch_cell_and_answer_color_at() {
        use crate::lsp::manager::ColorItem;
        let mut e = editor_with("a { color: #ff0000; }");
        e.path = Some(std::path::PathBuf::from("/tmp/x.css"));
        let item = ColorItem {
            line: 0,
            character: 11,
            end_line: 0,
            end_character: 18,
            red: 255,
            green: 0,
            blue: 0,
            raw: (1.0, 0.0, 0.0, 1.0),
        };
        e.apply_document_colors(std::path::PathBuf::from("/tmp/x.css"), vec![item]);
        let spans = e.row_inlay_spans(0);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, 11, "anchored at the value's start");
        assert_eq!(spans[0].1, "\u{25a0}");
        assert_eq!(spans[0].2, Some(Color::Rgb(255, 0, 0)));
        // Range lookup, inclusive of the end boundary.
        assert!(e.color_at(0, 11).is_some());
        assert!(e.color_at(0, 18).is_some());
        assert!(e.color_at(0, 10).is_none());
        assert!(e.color_at(0, 19).is_none());
        // An empty batch clears the swatches.
        e.apply_document_colors(std::path::PathBuf::from("/tmp/x.css"), Vec::new());
        assert!(e.row_inlay_spans(0).is_empty());
        // Colors for ANOTHER file never splice into this one.
        let stale = ColorItem {
            line: 0,
            character: 0,
            end_line: 0,
            end_character: 1,
            red: 1,
            green: 2,
            blue: 3,
            raw: (0.0, 0.0, 0.0, 1.0),
        };
        e.apply_document_colors(std::path::PathBuf::from("/tmp/other.css"), vec![stale]);
        assert!(e.row_inlay_spans(0).is_empty());
        assert!(e.color_at(0, 0).is_none());
    }

    #[test]
    fn select_next_occurrence_grows_selection_one_match_at_a_time() {
        let mut e = editor_with("foo bar foo baz foo");
        e.cursor_row = 0;
        e.cursor_col = 1; // inside the first "foo"
        // First press selects the word under the cursor; no extra carets yet.
        assert_eq!(e.select_next_occurrence(), 1);
        assert!(!e.has_multi_cursor());
        assert_eq!(e.selection.unwrap().normalised(), ((0, 0), (0, 3)));
        // Each further press adds the next occurrence as a secondary caret.
        assert_eq!(e.select_next_occurrence(), 2);
        assert!(e.has_multi_cursor());
        assert_eq!(e.select_next_occurrence(), 3);
        // Every "foo" is now selected: another press wraps and adds nothing.
        assert_eq!(e.select_next_occurrence(), 3);
    }

    #[test]
    fn add_caret_at_screen_adds_and_toggles_a_secondary_caret() {
        let mut e = editor_with("fn main() {}\nlet x = 1;");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2;
        e.cursor_row = 0;
        e.cursor_col = 0;
        // Alt+click on line 1, char 3: the old primary becomes a caret and the
        // click point becomes the new primary.
        assert!(e.add_caret_at_screen(6, 1));
        assert_eq!((e.cursor_row, e.cursor_col), (1, 3));
        assert_eq!(e.carets.len(), 1, "the previous cursor is now a caret");
        assert_eq!(e.carets[0].head, (0, 0));
        // Alt+click again on that same caret removes it (toggle).
        assert!(e.add_caret_at_screen(3, 0));
        assert_eq!(e.carets.len(), 0, "clicking an existing caret removes it");
    }

    #[test]
    fn box_select_spans_rows_into_a_column_of_carets() {
        let mut e = editor_with("aaaa\nbbbb\ncccc");
        e.last_inner = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 25,
        };
        e.last_gutter_width = 2; // text_x = 3
        // Anchor at row 0 char 1 (screen col 4), drag to row 2 char 3 (col 6).
        assert!(e.begin_box_select(4, 0));
        e.box_drag_to_screen(6, 2);
        assert!(e.box_selecting());
        // Head row is the primary; its selection covers cols 1..3.
        assert_eq!(e.selection.unwrap().normalised(), ((2, 1), (2, 3)));
        // The other two rows become carets over the same column span.
        assert_eq!(e.carets.len(), 2);
        assert_eq!(e.carets[0].normalised(), ((0, 1), (0, 3)));
        assert_eq!(e.carets[1].normalised(), ((1, 1), (1, 3)));
        e.end_box_select();
        assert!(!e.box_selecting());
    }

    #[test]
    fn select_next_occurrence_noop_off_a_word() {
        let mut e = editor_with("   foo");
        e.cursor_row = 0;
        e.cursor_col = 0; // on whitespace, not a word
        assert_eq!(e.select_next_occurrence(), 0);
        assert!(e.selection.is_none());
    }

    // ---- Toggle Word Wrap (Alt+Z) ----

    #[test]
    fn toggle_wrap_turns_wrap_on_for_code_file() {
        let mut e = editor_with("fn main() {}");
        e.lang = Some(LangKind::Rust);
        assert!(!e.wrap_enabled(), "code files default to no wrap");
        e.toggle_wrap();
        assert!(e.wrap_enabled());
    }

    #[test]
    fn toggle_wrap_turns_wrap_off_for_markdown() {
        let mut e = editor_with("# title");
        e.lang = Some(LangKind::Markdown);
        assert!(e.wrap_enabled(), "markdown defaults to wrap on");
        e.toggle_wrap();
        assert!(!e.wrap_enabled());
    }

    // ---- Go to Bracket (editor.action.jumpToBracket) ----

    #[test]
    fn jump_to_bracket_from_open_moves_to_matching_close() {
        let mut e = editor_with("foo(bar)baz");
        e.cursor_row = 0;
        e.cursor_col = 3; // immediately before the '('
        assert!(e.jump_to_matching_bracket());
        assert_eq!((e.cursor_row, e.cursor_col), (0, 7)); // start of ')'
    }

    #[test]
    fn jump_to_bracket_from_close_moves_to_matching_open() {
        let mut e = editor_with("foo(bar)baz");
        e.cursor_row = 0;
        e.cursor_col = 8; // immediately after the ')'
        assert!(e.jump_to_matching_bracket());
        assert_eq!((e.cursor_row, e.cursor_col), (0, 3)); // start of '('
    }

    #[test]
    fn jump_to_bracket_from_inside_goes_to_enclosing_close() {
        let mut e = editor_with("foo(bar)baz");
        e.cursor_row = 0;
        e.cursor_col = 5; // inside, between 'a' and 'r'
        assert!(e.jump_to_matching_bracket());
        assert_eq!((e.cursor_row, e.cursor_col), (0, 7)); // start of ')'
    }

    #[test]
    fn jump_to_bracket_respects_nesting() {
        let mut e = editor_with("(a(b)c)");
        e.cursor_row = 0;
        e.cursor_col = 0; // before the outer '('
        assert!(e.jump_to_matching_bracket());
        assert_eq!((e.cursor_row, e.cursor_col), (0, 6)); // outer ')'
    }

    #[test]
    fn jump_to_bracket_works_across_lines() {
        let mut e = editor_with("fn f() {\n    x\n}");
        e.cursor_row = 0;
        e.cursor_col = 7; // before the '{'
        assert!(e.jump_to_matching_bracket());
        assert_eq!((e.cursor_row, e.cursor_col), (2, 0)); // the '}'
    }

    #[test]
    fn jump_to_bracket_noop_without_a_bracket() {
        let mut e = editor_with("hello world");
        e.cursor_row = 0;
        e.cursor_col = 2;
        assert!(!e.jump_to_matching_bracket());
        assert_eq!((e.cursor_row, e.cursor_col), (0, 2));
    }

    // ---- Breadcrumbs bar ----

    #[test]
    fn sticky_scroll_never_covers_the_caret_row() {
        // The sticky band floats over the topmost content rows, and
        // `scroll_view_to` pulls the caret down to `scroll` whenever it drifts
        // above the viewport — so the caret lands on exactly the row the band
        // repaints, and vanishes. VS Code keeps the cursor line clear of its
        // sticky widget.
        let mut e = editor_with(&format!("fn a() {{\n{}}}\n", "    x\n".repeat(60)));
        e.scroll = 3;
        e.cursor_row = 3;
        e.sticky_lines = vec![0];
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let caret = e
            .cursor_screen_pos()
            .expect("the caret must have a screen row");
        assert!(
            !e.sticky_click_rows.iter().any(|&(y, _)| y == caret.1),
            "the sticky band painted over the caret's own row {}",
            caret.1
        );
    }

    #[test]
    fn sticky_band_erases_accept_spans_it_paints_over() {
        // The band overpaints the topmost content rows AFTER the row loop
        // records the [Accept …] spans; a span left under it would make a
        // click on a visible scope header resolve an unseen conflict.
        let mut e = editor_with(
            "fn outer() {\n    a();\n<<<<<<< HEAD\n    ours();\n=======\n    theirs();\n>>>>>>> feature\n    b();\n}",
        );
        e.scroll = 2; // the conflict header is the viewport's top row
        e.cursor_row = 7;
        e.sticky_lines = vec![0];
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 12,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        assert!(
            !e.sticky_click_rows.is_empty(),
            "precondition: the sticky band must have painted"
        );
        let band_rows: Vec<u16> = e.sticky_click_rows.iter().map(|&(y, _)| y).collect();
        assert!(
            !e.merge_action_spans
                .iter()
                .any(|(y, _, _, _)| band_rows.contains(y)),
            "spans under the sticky band must be dropped; spans={:?} band={band_rows:?}",
            e.merge_action_spans
        );
        assert!(
            e.merge_action_spans.iter().all(|(_, _, row, _)| *row == 2),
            "the header's spans below the band (if visible elsewhere) still target the real block"
        );
    }

    #[test]
    fn breadcrumb_row_renders_below_tab_strip_and_maps_symbol_clicks() {
        let mut tabs = EditorTabs::new();
        tabs.breadcrumbs = vec![
            Crumb {
                label: "src".into(),
                target: None,
            },
            Crumb {
                label: "m.rs".into(),
                target: None,
            },
            Crumb {
                label: "build".into(),
                target: Some((5, 2)),
            },
        ];
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut tabs).render(area, &mut buf);
        // The bar sits on the row directly below the one-row tab strip.
        let by = area.y + 1;
        // Somewhere on the bar a click lands on the "build" symbol crumb and
        // resolves to its jump target; the path crumbs resolve to nothing.
        let hit = (0..area.width).find_map(|x| tabs.breadcrumb_target_at(x, by));
        assert_eq!(hit, Some((5, 2)), "clicking the symbol crumb jumps there");
        // A click off the bar (the tab strip row) never maps to a crumb.
        assert_eq!(tabs.breadcrumb_target_at(10, area.y), None);
    }

    #[test]
    fn breadcrumbs_measure_wide_glyphs_in_display_cells() {
        // The bar budgeted and advanced in `char`s while the buffer advances by
        // display CELLS, so every crumb after a CJK one was painted right of
        // the rect recorded for it and its click missed. Same class as the
        // terminal pane pill.
        let mut tabs = EditorTabs::new();
        tabs.breadcrumbs = vec![
            Crumb {
                label: "文档文档".into(),
                target: None,
            },
            Crumb {
                label: "build".into(),
                target: Some((5, 2)),
            },
        ];
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut tabs).render(area, &mut buf);
        let by = area.y + 1;
        // The column where "build" was actually PAINTED: scan cells, since a
        // wide glyph takes one cell and leaves the next as a spacer.
        let row: String = (0..area.width)
            .map(|x| buf[(x, by)].symbol().to_string())
            .collect::<Vec<_>>()
            .concat();
        // The advance was a char count while the glyphs take two cells each,
        // so the separator and the next crumb were painted four cells INTO
        // this label and destroyed its second half.
        // A wide glyph owns one cell and leaves the next as a spacer, so the
        // concatenated row interleaves blanks: count the glyphs instead.
        assert_eq!(
            (row.matches('文').count(), row.matches('档').count()),
            (2, 2),
            "the whole wide label must survive, got {row:?}"
        );
        assert!(
            row.contains("build"),
            "and the next crumb must still be painted, got {row:?}"
        );
        // The recorded hit rect must cover every cell the label was painted on,
        // or clicking its right half misses.
        let (x, w, _) = tabs.breadcrumb_ranges[0];
        assert_eq!(w, 8, "a 4-char CJK label occupies 8 cells, not 4");
        let (next_x, _, _) = tabs.breadcrumb_ranges[1];
        assert!(
            next_x >= x + w,
            "the next crumb must start past this one, got {next_x} vs {x}+{w}"
        );
    }

    #[test]
    fn breadcrumb_bar_hidden_when_there_are_no_crumbs() {
        let mut tabs = EditorTabs::new();
        tabs.breadcrumbs.clear();
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut tabs).render(area, &mut buf);
        assert_eq!(tabs.breadcrumb_target_at(10, area.y + 1), None);
    }

    // ---- Sticky scroll ----

    #[test]
    fn sticky_scroll_pins_header_rows_and_maps_clicks_to_their_lines() {
        let text = (0..40)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut e = editor_with(&text);
        e.focused = true;
        e.scroll = 20;
        // The caret must sit inside the viewport and below the band: with it on
        // the top visible row there is nowhere for the band to go that would
        // not hide it, and render pulls `scroll` back to the caret.
        e.cursor_row = 25;
        e.sticky_lines = vec![0, 10];
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 15,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let top = e.last_inner.y;
        assert_eq!(e.sticky_line_at(5, top), Some(0), "row 0 pins line 0");
        assert_eq!(e.sticky_line_at(5, top + 1), Some(10), "row 1 pins line 10");
        assert_eq!(
            e.sticky_line_at(5, top + 5),
            None,
            "a content row below the band is not sticky"
        );
    }

    /// The wheel drags the caret along with the viewport (`scroll_view_to`
    /// clamps it to `scroll`), which used to park it on the very row the band
    /// wants. Suppressing the band there would delete sticky scroll for the
    /// one gesture it exists to serve, so the caret lands below the band
    /// instead.
    #[test]
    fn a_wheel_scroll_keeps_the_band_and_parks_the_caret_below_it() {
        let text = (0..80)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut e = editor_with(&text);
        e.focused = true;
        e.sticky_lines = vec![0, 10];
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 15,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf); // seats `last_inner` for scroll_down
        assert_eq!(e.cursor_row, 0, "caret starts on the first line");
        e.scroll_down(20);
        (&mut e).render(area, &mut buf);
        let top = e.last_inner.y;
        assert_eq!(
            e.sticky_line_at(5, top),
            Some(0),
            "the band must survive a wheel scroll"
        );
        assert_eq!(e.sticky_line_at(5, top + 1), Some(10));
        assert!(
            e.cursor_row >= e.scroll + 2,
            "caret dragged below the two-row band, not under it (row {}, scroll {})",
            e.cursor_row,
            e.scroll
        );
    }

    /// The band stops above the caret's PAINTED row. A collapsed fold between
    /// the top of the viewport and the caret removes rows, so counting buffer
    /// lines overstates how far down the caret is and the band paints over it.
    #[test]
    fn the_band_stops_above_the_caret_with_a_fold_between_them() {
        let mut text: Vec<String> = (0..10).map(|i| format!("head{i}")).collect();
        text.push(String::from("def f():"));
        text.extend((0..3).map(|i| format!("    body{i}")));
        text.extend((0..10).map(|i| format!("tail{i}")));
        let mut e = editor_with(&text.join("\n"));
        e.focused = true;
        e.scroll = 10;
        e.toggle_fold(10); // hides lines 11..=13
        assert!(e.is_line_hidden(12), "the fold body is hidden");
        // Line 14 is the second PAINTED row (row 0 is the fold header on 10),
        // even though it is four buffer lines below the top.
        e.cursor_row = 14;
        e.sticky_lines = vec![0, 5];
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 15,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let top = e.last_inner.y;
        assert_eq!(e.sticky_line_at(5, top), Some(0), "outermost header pins");
        assert_eq!(
            e.sticky_line_at(5, top + 1),
            None,
            "the caret's own painted row must stay clear of the band"
        );
    }

    #[test]
    fn sticky_scroll_hidden_without_pinned_lines() {
        let mut e = editor_with("a\nb\nc\n");
        e.focused = true;
        e.sticky_lines.clear();
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        assert_eq!(e.sticky_line_at(5, e.last_inner.y), None);
    }

    // ---- Bracket match highlight (editorBracketMatch) ----

    #[test]
    fn bracket_match_pair_from_open_returns_both_brackets() {
        let mut e = editor_with("foo(bar)baz");
        e.cursor_col = 3; // immediately before '('
        assert_eq!(e.bracket_match_pair(), Some(((0, 3), (0, 7))));
    }

    #[test]
    fn bracket_match_pair_from_after_close_returns_both_brackets() {
        let mut e = editor_with("foo(bar)baz");
        e.cursor_col = 8; // immediately after ')'
        assert_eq!(e.bracket_match_pair(), Some(((0, 3), (0, 7))));
    }

    #[test]
    fn bracket_match_pair_none_when_caret_touches_no_bracket() {
        let mut e = editor_with("foo(bar)baz");
        e.cursor_col = 5; // inside the pair but not adjacent to a bracket
        assert_eq!(e.bracket_match_pair(), None);
    }

    #[test]
    fn render_paints_bracket_match_background_on_both_brackets() {
        let mut e = editor_with("foo(bar)baz");
        e.focused = true;
        e.cursor_col = 3; // beside '('
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        let bm = e.theme.bracket_match_bg();
        assert_eq!(buf[(text_x + 3, y)].bg, bm, "the '(' is highlighted");
        assert_eq!(buf[(text_x + 7, y)].bg, bm, "the ')' is highlighted");
        assert_ne!(
            buf[(text_x + 5, y)].bg,
            bm,
            "a non-bracket cell is not highlighted"
        );
    }

    // ---- Indentation guides (editor.guides.indentation) ----

    fn guide_buf(e: &mut Editor, w: u16, h: u16) -> Buffer {
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut buf = Buffer::empty(area);
        (e as &mut Editor).render(area, &mut buf);
        buf
    }

    #[test]
    fn render_paints_indent_guides_at_each_indent_level() {
        let mut e = editor_with("fn main() {\n    if x {\n        y();\n    }\n}");
        let buf = guide_buf(&mut e, 40, 8);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        let g = e.theme.indent_guide();
        assert_eq!(buf[(text_x, y0 + 1)].symbol(), "\u{2502}", "level-1 guide");
        assert_eq!(buf[(text_x, y0 + 1)].fg, g);
        assert_eq!(
            buf[(text_x, y0 + 2)].symbol(),
            "\u{2502}",
            "level-1 guide, row 2"
        );
        assert_eq!(
            buf[(text_x + 4, y0 + 2)].symbol(),
            "\u{2502}",
            "level-2 guide"
        );
        assert_eq!(buf[(text_x + 4, y0 + 2)].fg, g);
        assert_eq!(
            buf[(text_x, y0)].symbol(),
            "f",
            "a top-level line gets no guide and keeps its text"
        );
        assert_eq!(
            buf[(text_x + 4, y0 + 1)].symbol(),
            "i",
            "guides never overwrite text"
        );
    }

    #[test]
    fn indent_guides_continue_through_blank_lines_inside_a_block_only() {
        let mut e = editor_with("def f():\n    a = 1\n\n    b = 2\n\ndef g():\n    pass");
        let buf = guide_buf(&mut e, 40, 9);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        assert_eq!(
            buf[(text_x, y0 + 2)].symbol(),
            "\u{2502}",
            "a blank line inside a block continues the guide"
        );
        assert_eq!(
            buf[(text_x, y0 + 4)].symbol(),
            " ",
            "a blank line between top-level blocks shows no guide"
        );
    }

    #[test]
    fn active_indent_guide_highlights_the_cursor_block_only() {
        let mut e = editor_with("fn a() {\n    x();\n}\nfn b() {\n    y();\n}");
        e.focused = true;
        e.cursor_row = 4;
        e.cursor_col = 4;
        let buf = guide_buf(&mut e, 40, 8);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        assert_eq!(
            buf[(text_x, y0 + 4)].fg,
            e.theme.indent_guide_active(),
            "the cursor's block guide is highlighted"
        );
        assert_eq!(
            buf[(text_x, y0 + 1)].fg,
            e.theme.indent_guide(),
            "the other block's guide stays dim"
        );
    }

    #[test]
    fn header_line_activates_the_guide_of_the_block_it_opens() {
        let mut e = editor_with("fn a() {\n    x();\n    y();\n}");
        e.focused = true;
        e.cursor_row = 0;
        e.cursor_col = 0;
        let buf = guide_buf(&mut e, 40, 8);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        assert_eq!(buf[(text_x, y0 + 1)].fg, e.theme.indent_guide_active());
        assert_eq!(buf[(text_x, y0 + 2)].fg, e.theme.indent_guide_active());
    }

    #[test]
    fn indent_guides_toggle_off_paints_plain_whitespace() {
        let mut e = editor_with("fn main() {\n    if x {\n        y();\n    }\n}");
        e.show_indent_guides = false;
        let buf = guide_buf(&mut e, 40, 8);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        assert_eq!(buf[(text_x, y0 + 1)].symbol(), " ");
        assert_eq!(buf[(text_x + 4, y0 + 2)].symbol(), " ");
    }

    #[test]
    fn indent_guides_honour_horizontal_scroll() {
        let mut e = editor_with(
            "fn main() {\n    if x {\n        y(); // padding so the pane really scrolls\n    }\n}",
        );
        e.scroll_col = 4;
        let buf = guide_buf(&mut e, 40, 8);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        assert_eq!(
            buf[(text_x, y0 + 2)].symbol(),
            "\u{2502}",
            "the level-2 guide shifts left with the scroll"
        );
        assert_eq!(
            buf[(text_x, y0 + 1)].symbol(),
            "i",
            "the scrolled-off level-1 guide leaves the text alone"
        );
    }

    #[test]
    fn guide_indent_width_resolves_blank_lines_from_neighbours() {
        let e = editor_with("def f():\n    a = 1\n\n    b = 2\n\ndef g():\n    pass");
        assert_eq!(e.guide_indent_width(2), 4, "blank inside a block");
        assert_eq!(e.guide_indent_width(4), 0, "blank between blocks");
    }

    #[test]
    fn active_indent_guide_spans_the_enclosing_block() {
        let mut e = editor_with("fn a() {\n    x();\n}\nfn b() {\n    y();\n    z();\n}");
        e.cursor_row = 4;
        assert_eq!(e.active_indent_guide(), Some((0, 4, 5)));
        e.cursor_row = 0; // header activates the body it opens
        assert_eq!(e.active_indent_guide(), Some((0, 1, 1)));
        e.cursor_row = 2; // "}" is top-level: no active guide
        assert_eq!(e.active_indent_guide(), None);
    }

    // ---- Bracket-pair colorization (#131) ----

    #[test]
    fn scan_colors_brackets_by_shared_nesting_depth() {
        let lines = vec![String::from("a(b[c{d}e]f)g")];
        let out = scan_bracket_colors(&lines, &[]);
        assert_eq!(
            out[0],
            vec![(1, 0), (3, 1), (5, 2), (7, 2), (9, 1), (11, 0)],
            "openers colour at their depth, closers at their opener's depth"
        );
    }

    #[test]
    fn scan_marks_unmatched_and_mismatched_closers_unexpected() {
        let lines = vec![String::from("x)"), String::from("(]y)")];
        let out = scan_bracket_colors(&lines, &[]);
        assert_eq!(out[0], vec![(1, UNEXPECTED_BRACKET)], "no opener at all");
        assert_eq!(
            out[1],
            vec![(0, 0), (1, UNEXPECTED_BRACKET), (3, 0)],
            "a mismatched closer reddens without consuming the open bracket"
        );
    }

    #[test]
    fn scan_skips_brackets_inside_protected_ranges() {
        // "s(" with the '(' at byte 1 protected: only line 2's bracket shows.
        let lines = vec![String::from("s("), String::from("f()")];
        let out = scan_bracket_colors(&lines, &[(1, 2)]);
        assert!(out[0].is_empty(), "the protected '(' does not participate");
        assert_eq!(out[1], vec![(1, 0), (2, 0)]);
    }

    #[test]
    fn scan_depth_cycles_past_the_palette() {
        let lines = vec![String::from("([{(x)}])")];
        let out = scan_bracket_colors(&lines, &[]);
        assert_eq!(
            out[0][3],
            (3, 0),
            "depth 3 wraps back to the first cycle colour"
        );
    }

    #[test]
    fn recompute_highlights_skips_brackets_in_rust_strings_and_comments() {
        let mut e = editor_with("let s = \"(prose)\";\nf(1); // (note)\n");
        e.set_language(Some(LangKind::Rust));
        assert!(
            e.bracket_colors[0].is_empty(),
            "string-literal brackets are prose: {:?}",
            e.bracket_colors[0]
        );
        assert_eq!(
            e.bracket_colors[1],
            vec![(1, 0), (3, 0)],
            "code brackets colour; comment brackets do not: {:?}",
            e.bracket_colors[1]
        );
    }

    #[test]
    fn plain_text_bracket_scan_is_gated_by_buffer_size() {
        // A no-grammar buffer has no tree-sitter pass absorbing a per-edit
        // full scan, so an oversized one must skip bracket colorization
        // entirely (typing in an 8MB log must stay O(lines), not O(chars)).
        let big_line = "x".repeat(64 * 1024);
        let mut e = editor_with("");
        e.lines = vec![big_line; (BRACKET_SCAN_MAX_BYTES / (64 * 1024)) + 2];
        e.lines.push(String::from("f()"));
        e.recompute_highlights();
        assert!(
            e.bracket_colors.iter().all(Vec::is_empty),
            "an oversized plain-text buffer opts out of the scan"
        );
        let mut small = editor_with("f()");
        small.recompute_highlights();
        assert_eq!(small.bracket_colors[0], vec![(1, 0), (2, 0)]);
    }

    #[test]
    fn render_colors_brackets_by_nesting_depth() {
        let mut e = editor_with("a(b[c{d}e]f)g");
        e.recompute_highlights();
        let buf = guide_buf(&mut e, 40, 6);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        for (col, depth) in [(1u16, 0), (3, 1), (5, 2), (7, 2), (9, 1), (11, 0)] {
            assert_eq!(
                buf[(text_x + col, y0)].fg,
                e.theme.bracket_pair_color(depth),
                "bracket at col {col} wears depth-{depth} colour"
            );
        }
        assert_ne!(
            buf[(text_x + 2, y0)].fg,
            e.theme.bracket_pair_color(0),
            "non-bracket text keeps its syntax colour"
        );
    }

    #[test]
    fn render_paints_unmatched_closer_red() {
        let mut e = editor_with("x)");
        e.recompute_highlights();
        let buf = guide_buf(&mut e, 40, 6);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        assert_eq!(
            buf[(text_x + 1, e.last_inner.y)].fg,
            e.theme.bracket_unexpected_fg()
        );
    }

    #[test]
    fn render_bracket_colors_toggle_off_keeps_syntax_color() {
        let mut e = editor_with("a(b)c");
        e.recompute_highlights();
        e.show_bracket_colors = false;
        let buf = guide_buf(&mut e, 40, 6);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        assert_ne!(
            buf[(text_x + 1, e.last_inner.y)].fg,
            e.theme.bracket_pair_color(0)
        );
    }

    // ---- Render whitespace (#133) ----

    #[test]
    fn selection_mode_paints_whitespace_glyphs_only_inside_the_selection() {
        let mut e = editor_with("a b c d");
        e.selection = Some(EditorSelection {
            anchor: (0, 3),
            head: (0, 6),
        });
        let buf = guide_buf(&mut e, 40, 6);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        assert_eq!(buf[(text_x + 3, y0)].symbol(), "\u{b7}", "selected space");
        assert_eq!(buf[(text_x + 3, y0)].fg, e.theme.whitespace_fg());
        assert_eq!(buf[(text_x + 5, y0)].symbol(), "\u{b7}");
        assert_eq!(
            buf[(text_x + 1, y0)].symbol(),
            " ",
            "an unselected space stays invisible"
        );
        assert_eq!(buf[(text_x + 4, y0)].symbol(), "c", "text is untouched");
    }

    #[test]
    fn all_mode_paints_every_space_and_tab() {
        let mut e = editor_with("a b\n\tc");
        e.whitespace_mode = WhitespaceMode::All;
        let buf = guide_buf(&mut e, 40, 6);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        assert_eq!(buf[(text_x + 1, y0)].symbol(), "\u{b7}");
        assert_eq!(
            buf[(text_x, y0 + 1)].symbol(),
            "\u{2192}",
            "a tab renders as an arrow"
        );
    }

    #[test]
    fn none_mode_paints_no_whitespace_glyphs_even_selected() {
        let mut e = editor_with("a b");
        e.whitespace_mode = WhitespaceMode::None;
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (0, 3),
        });
        let buf = guide_buf(&mut e, 40, 6);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        assert_eq!(buf[(text_x + 1, e.last_inner.y)].symbol(), " ");
    }

    #[test]
    fn selection_mode_covers_every_line_of_a_multiline_selection() {
        let mut e = editor_with("a b\nc d");
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (1, 3),
        });
        let buf = guide_buf(&mut e, 40, 6);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        assert_eq!(buf[(text_x + 1, y0)].symbol(), "\u{b7}");
        assert_eq!(buf[(text_x + 1, y0 + 1)].symbol(), "\u{b7}");
    }

    // ---- Debugger inline values (#135) ----

    fn locals(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn inline_values_annotate_function_lines_up_to_the_stop_only() {
        let mut e = editor_with("def f():\n    x = 1\n    y = x + 1\n    print(x, y)\n\nz = 9");
        // Stopped ON line 3 (1-based): `y = x + 1` is about to run.
        e.set_inline_values_from_locals(3, &locals(&[("x", "1"), ("y", "2")]));
        assert_eq!(e.inline_values.get(&1).map(String::as_str), Some("x = 1"));
        assert_eq!(
            e.inline_values.get(&2).map(String::as_str),
            Some("y = 2, x = 1"),
            "first-mention order on the stop line"
        );
        assert!(
            !e.inline_values.contains_key(&3),
            "lines past the execution point stay bare"
        );
        assert!(!e.inline_values.contains_key(&5), "outside the function");
    }

    #[test]
    fn inline_values_span_the_whole_function_from_a_nested_block_stop() {
        let mut e =
            editor_with("def f():\n    x = 1\n    if True:\n        y = 2\n        print(x, y)");
        // Stopped on the print, inside the `if` block: the scan must climb to
        // `def f():`, not stop at `if True:` (#136 review).
        e.set_inline_values_from_locals(5, &locals(&[("x", "1"), ("y", "2")]));
        assert_eq!(
            e.inline_values.get(&1).map(String::as_str),
            Some("x = 1"),
            "a line above the nested block still annotates"
        );
        assert_eq!(
            e.inline_values.get(&4).map(String::as_str),
            Some("x = 1, y = 2")
        );
    }

    #[test]
    fn inline_values_clear_when_a_reused_tab_opens_a_different_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.py");
        let b = dir.path().join("b.py");
        std::fs::write(&a, "x = 1\n").unwrap();
        std::fs::write(&b, "y = 2\n").unwrap();
        let mut e = Editor::new();
        e.open(&a).unwrap();
        e.inline_values.insert(0, String::from("x = 1"));
        e.open(&b).unwrap();
        assert!(
            e.inline_values.is_empty(),
            "a different file must not inherit the old file's trailers"
        );
    }

    #[test]
    fn inline_values_match_whole_identifiers_only() {
        let mut e = editor_with("def f():\n    max = 9\n    q = max");
        e.set_inline_values_from_locals(3, &locals(&[("x", "1"), ("q", "7")]));
        assert!(
            !e.inline_values.contains_key(&1),
            "`x` must not match inside `max`"
        );
        assert_eq!(e.inline_values.get(&2).map(String::as_str), Some("q = 7"));
    }

    #[test]
    fn inline_values_elide_long_values_in_the_middle() {
        let mut e = editor_with("def f():\n    x = 1");
        let long = "a".repeat(60);
        e.set_inline_values_from_locals(2, &locals(&[("x", &long)]));
        let note = e.inline_values.get(&1).unwrap();
        assert!(note.contains('\u{2026}'), "middle ellipsis: {note}");
        assert!(note.chars().count() < 60, "elided: {note}");
    }

    #[test]
    fn inline_values_clear_when_locals_are_empty() {
        let mut e = editor_with("def f():\n    x = 1");
        e.set_inline_values_from_locals(2, &locals(&[("x", "1")]));
        assert!(!e.inline_values.is_empty());
        e.set_inline_values_from_locals(2, &[]);
        assert!(e.inline_values.is_empty());
    }

    #[test]
    fn render_paints_inline_value_trailer_after_the_line_end() {
        let mut e = editor_with("x = 1\ny = 2");
        e.inline_values.insert(0, String::from("x = 1"));
        let buf = guide_buf(&mut e, 40, 6);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        // line 0 is 5 chars wide; the trailer starts 2 cells past it.
        let start = text_x + 5 + 2;
        assert_eq!(buf[(start, y0)].symbol(), "x");
        assert_eq!(buf[(start, y0)].fg, e.theme.ignored_fg());
        assert_eq!(buf[(start + 2, y0)].symbol(), "=");
        assert_eq!(
            buf[(text_x + 5 + 2, y0 + 1)].symbol(),
            " ",
            "an unannotated line gets no trailer"
        );
    }

    #[test]
    fn identifier_tokens_split_on_word_boundaries() {
        assert_eq!(
            identifier_tokens("print(x, _y2) + max"),
            vec!["print", "x", "_y2", "max"]
        );
    }

    // ---- Select to Bracket (editor.action.selectToBracket) ----

    #[test]
    fn select_to_bracket_includes_both_brackets() {
        let mut e = editor_with("foo(bar)baz");
        e.cursor_row = 0;
        e.cursor_col = 3; // on the '('
        assert!(e.select_to_matching_bracket());
        assert_eq!(e.selection_text(), "(bar)");
    }

    #[test]
    fn select_to_bracket_from_inside_selects_enclosing_pair() {
        let mut e = editor_with("foo(bar)baz");
        e.cursor_row = 0;
        e.cursor_col = 5; // inside the parentheses
        assert!(e.select_to_matching_bracket());
        assert_eq!(e.selection_text(), "(bar)");
    }

    #[test]
    fn select_to_bracket_spans_multiple_lines() {
        let mut e = editor_with("fn f() {\n    x\n}");
        e.cursor_row = 0;
        e.cursor_col = 7; // on the '{'
        assert!(e.select_to_matching_bracket());
        assert_eq!(e.selection_text(), "{\n    x\n}");
    }

    // ---- Transpose Characters (editor.action.transpose) ----

    #[test]
    fn transpose_swaps_chars_around_cursor_and_advances() {
        let mut e = editor_with("abcd");
        e.cursor_row = 0;
        e.cursor_col = 2; // between 'b' and 'c'
        e.transpose_chars();
        assert_eq!(e.lines, vec!["acbd"]);
        assert_eq!((e.cursor_row, e.cursor_col), (0, 3));
    }

    #[test]
    fn transpose_at_line_start_only_advances() {
        let mut e = editor_with("abcd");
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.transpose_chars();
        assert_eq!(e.lines, vec!["abcd"]);
        assert_eq!((e.cursor_row, e.cursor_col), (0, 1));
    }

    #[test]
    fn transpose_at_end_of_line_moves_char_across_break() {
        let mut e = editor_with("ab\ncd");
        e.cursor_row = 0;
        e.cursor_col = 2; // end of "ab", not the last line
        e.transpose_chars();
        assert_eq!(e.lines, vec!["a", "bcd"]);
        assert_eq!((e.cursor_row, e.cursor_col), (1, 1));
    }

    #[test]
    fn transpose_at_end_of_final_line_is_noop() {
        let mut e = editor_with("abc");
        e.cursor_row = 0;
        e.cursor_col = 3; // end of the only line
        e.transpose_chars();
        assert_eq!(e.lines, vec!["abc"]);
        assert_eq!((e.cursor_row, e.cursor_col), (0, 3));
    }

    // ---- Convert Indentation to Spaces / Tabs ----

    #[test]
    fn indentation_to_spaces_converts_only_leading_tabs() {
        let mut e = editor_with("\tfoo\n\t\tbar\nbaz\ta");
        e.indentation_to_spaces();
        assert_eq!(e.lines, vec!["    foo", "        bar", "baz\ta"]);
    }

    #[test]
    fn indentation_to_tabs_converts_leading_space_groups() {
        let mut e = editor_with("    foo\n      bar\nb  c");
        e.indentation_to_tabs();
        // 4 spaces -> 1 tab; 6 spaces -> 1 tab + 2 leftover; interior spaces stay.
        assert_eq!(e.lines, vec!["\tfoo", "\t  bar", "b  c"]);
    }

    /// Issue #211: VS Code's editor.detectIndentation. Opening a file
    /// whose content indents with 2 spaces must set the buffer's indent
    /// style to 2 spaces even though the language default is 4; tabs win
    /// when the content uses tabs; a flat file keeps the language default;
    /// the manual status-bar override still beats detection.
    #[test]
    fn open_detects_indentation_from_file_content() {
        let tmp = tempfile::tempdir().unwrap();
        let two = tmp.path().join("two.rs");
        std::fs::write(
            &two,
            "fn main() {\n  let a = 1;\n  if a > 0 {\n    println!();\n  }\n}\n",
        )
        .unwrap();
        let mut e = Editor::new();
        e.open(&two).unwrap();
        assert_eq!(
            e.indent_style(),
            IndentStyle {
                width: 2,
                use_spaces: true
            },
            "2-space content must be detected over the 4-space rust default"
        );

        let tabs = tmp.path().join("tabs.rs");
        std::fs::write(
            &tabs,
            "fn main() {\n\tlet a = 1;\n\tif a > 0 {\n\t\tprintln!();\n\t}\n}\n",
        )
        .unwrap();
        let mut t = Editor::new();
        t.open(&tabs).unwrap();
        assert!(
            !t.indent_style().use_spaces,
            "tab-indented content must detect tabs"
        );

        let flat = tmp.path().join("flat.rs");
        std::fs::write(&flat, "fn main() {}\nfn other() {}\n").unwrap();
        let mut f = Editor::new();
        f.open(&flat).unwrap();
        assert_eq!(
            f.indent_style(),
            IndentStyle {
                width: 4,
                use_spaces: true
            },
            "a file with no indented lines keeps the language default"
        );

        // The manual override still wins over detection.
        let mut o = Editor::new();
        o.open(&two).unwrap();
        o.set_indent_style(IndentStyle {
            width: 8,
            use_spaces: true,
        });
        assert_eq!(o.indent_style().width, 8, "override beats detection");
    }

    /// The pure detector: majority rules between tabs and spaces, and the
    /// space width comes from the most common indentation step.
    #[test]
    fn detect_indentation_picks_majority_and_step() {
        let lines = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            detect_indentation(&lines(&["a {", "    b;", "        c;", "    d;", "}"])),
            Some(IndentStyle {
                width: 4,
                use_spaces: true
            })
        );
        assert_eq!(
            detect_indentation(&lines(&["a {", "\tb;", "\t\tc;", "}"])),
            Some(IndentStyle {
                width: 4,
                use_spaces: false
            })
        );
        assert_eq!(detect_indentation(&lines(&["a", "b", "c"])), None);
        // Mixed content: the majority style wins (three tab lines, one
        // spaced line).
        assert_eq!(
            detect_indentation(&lines(&["a {", "\tb;", "\tc;", "\td;", "  e;", "}"])),
            Some(IndentStyle {
                width: 4,
                use_spaces: false
            })
        );
        // Whitespace-only lines carry no intent on EITHER side: tab-only
        // blank lines must not outvote real space indentation (review
        // round 1).
        assert_eq!(
            detect_indentation(&lines(&["a {", "  b;", "  c;", "\t", "\t\t", "\t \t", "}"])),
            Some(IndentStyle {
                width: 2,
                use_spaces: true
            })
        );
    }

    #[test]
    fn indent_override_switches_tab_to_a_literal_tab() {
        let mut e = editor_with("foo");
        // Default: spaces, width 4 -> Tab pads to the next stop with spaces.
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.indent_at_cursor();
        assert_eq!(e.lines[0], "    foo");
        assert_eq!(e.indent_preference(), (4, true));

        // Pin tabs: Tab now inserts one literal tab, LSP prefs flip insert_spaces.
        let mut t = editor_with("foo");
        t.set_indent_style(IndentStyle {
            width: 4,
            use_spaces: false,
        });
        t.cursor_row = 0;
        t.cursor_col = 0;
        t.indent_at_cursor();
        assert_eq!(t.lines[0], "\tfoo");
        assert_eq!(t.indent_preference(), (4, false));
    }

    #[test]
    fn indent_override_width_changes_status_label_and_unit() {
        let mut e = editor_with("");
        e.set_indent_style(IndentStyle {
            width: 2,
            use_spaces: true,
        });
        assert_eq!(e.indent_style().label(), "Spaces: 2");
        assert_eq!(e.indent_style().unit(), "  ");
        e.set_indent_style(IndentStyle {
            width: 4,
            use_spaces: false,
        });
        assert_eq!(e.indent_style().label(), "Tab Size: 4");
        assert_eq!(e.indent_style().unit(), "\t");
    }

    // ---- Trim Final Newlines (files.trimFinalNewlines) ----

    #[test]
    fn trim_final_newlines_removes_trailing_blank_lines() {
        let mut e = editor_with("a\nb\n\n\n");
        // editor_with drops the empty trailing element from `lines()`, so build
        // the buffer explicitly with trailing blanks.
        e.lines = vec![
            String::from("a"),
            String::from("b"),
            String::new(),
            String::new(),
        ];
        assert!(e.trim_final_newlines());
        assert_eq!(e.lines, vec!["a", "b"]);
    }

    #[test]
    fn trim_final_newlines_keeps_at_least_one_line() {
        let mut e = editor_with("");
        e.lines = vec![String::new(), String::new(), String::new()];
        assert!(e.trim_final_newlines());
        assert_eq!(e.lines, vec![""]);
    }

    #[test]
    fn trim_final_newlines_noop_when_no_trailing_blanks() {
        let mut e = editor_with("a\nb");
        assert!(!e.trim_final_newlines());
        assert_eq!(e.lines, vec!["a", "b"]);
    }

    #[test]
    fn fold_range_covers_an_indented_block() {
        let e = editor_with("fn a() {\n    let x = 1;\n    let y = 2;\n}\nfn b() {}");
        // Header line 0; body lines 1-2 are more indented; the closing brace on
        // line 3 returns to base indent and is not part of the region.
        assert_eq!(e.fold_range(0), Some((0, 2)));
        // A leaf line with no deeper line below it is not foldable.
        assert_eq!(e.fold_range(1), None);
        assert_eq!(e.fold_range(4), None);
    }

    #[test]
    fn fold_range_absorbs_interior_blank_lines_but_not_trailing_ones() {
        let e = editor_with("fn a() {\n    x\n\n    y\n}\n");
        // The interior blank (line 2) sits between two deeper lines, so it is
        // inside the region; the region ends at the last deeper line (3).
        assert_eq!(e.fold_range(0), Some((0, 3)));
    }

    #[test]
    fn region_markers_fold_in_the_fallback_scanner_including_nesting() {
        use crate::lsp::manager::FoldRangeKind;
        let mut e = editor_with(
            "top\n#region outer\na\n// region inner\nb\n// endregion\nc\n#endregion\ntail",
        );
        e.refresh_fold_tables();
        assert_eq!(e.fold_range(1), Some((1, 7)), "outer #region pair");
        assert_eq!(e.fold_range(3), Some((3, 5)), "nested // region pair");
        assert_eq!(e.fold_kind_at(1), Some(FoldRangeKind::Region));
        assert_eq!(e.fold_kind_at(0), None, "plain text has no kind");
        // A line that merely CONTAINS the word is not a marker.
        let mut e2 = editor_with("let region = 1;\nregional stuff\n#endregion");
        e2.refresh_fold_tables();
        assert_eq!(e2.fold_range(0), None);
        assert_eq!(e2.fold_range(1), None);
    }

    #[test]
    fn comment_runs_fold_but_rust_attributes_do_not_count_as_comments() {
        use crate::lsp::manager::FoldRangeKind;
        let mut e = editor_with(
            "// docs line one\n// docs line two\n// docs line three\nfn a() {}\n#[derive(Debug)]\n#[cfg(test)]\nstruct S;",
        );
        e.refresh_fold_tables();
        assert_eq!(e.fold_range(0), Some((0, 2)), "a 3-line comment run folds");
        assert_eq!(e.fold_kind_at(0), Some(FoldRangeKind::Comment));
        assert_eq!(
            e.fold_kind_at(4),
            None,
            "attribute lines are code, not a comment run"
        );
        // A single comment line is not a run.
        let mut e2 = editor_with("// alone\nfn a() {}");
        e2.refresh_fold_tables();
        assert_eq!(e2.fold_kind_at(0), None);
    }

    #[test]
    fn server_fold_spans_replace_the_heuristics_until_the_buffer_moves() {
        use crate::lsp::manager::{FoldRangeKind, FoldingRangeItem};
        let mut e = editor_with("fn a() {\n    x\n    y\n}\nfn b() {}");
        // Server says the whole item folds through its closing brace —
        // something indentation cannot express.
        e.set_lsp_folds(vec![FoldingRangeItem {
            start_line: 0,
            end_line: 3,
            kind: FoldRangeKind::Other,
        }]);
        assert_eq!(e.fold_range(0), Some((0, 3)), "server span wins");
        assert_eq!(
            e.fold_range(4),
            None,
            "server authoritative: no indentation fallthrough"
        );
        // An edit that changes the line count orphans the spans; the
        // indentation scan is back in charge until the next reply.
        e.lines.push(String::from("tail"));
        assert_eq!(e.fold_range(0), Some((0, 2)), "stale spans ignored");
    }

    #[test]
    fn fold_all_regions_and_unfold_all_regions_round_trip() {
        use crate::lsp::manager::FoldRangeKind;
        let mut e = editor_with("#region a\nx\n#endregion\n// one\n// two\nfn f() {\n    body\n}");
        e.fold_all_of_kind(FoldRangeKind::Region);
        assert!(e.is_line_hidden(1), "the region body folded");
        assert!(!e.is_line_hidden(4), "the comment run did not");
        assert!(!e.is_line_hidden(6), "the indented block did not");
        e.fold_all_of_kind(FoldRangeKind::Comment);
        assert!(e.is_line_hidden(4), "now the comment run folded too");
        e.unfold_all_of_kind(FoldRangeKind::Region);
        assert!(!e.is_line_hidden(1), "regions expanded");
        assert!(e.is_line_hidden(4), "comment folds untouched");
    }

    #[test]
    fn folding_a_header_hides_its_body_only() {
        let mut e = editor_with("fn a() {\n    let x = 1;\n    let y = 2;\n}\nfn b() {}");
        e.toggle_fold(0);
        assert!(!e.is_line_hidden(0), "the header line stays visible");
        assert!(e.is_line_hidden(1));
        assert!(e.is_line_hidden(2));
        assert!(
            !e.is_line_hidden(3),
            "the closing brace at base indent stays"
        );
        assert!(!e.is_line_hidden(4));
    }

    #[test]
    fn moving_down_steps_over_a_folded_block_instead_of_into_it() {
        // `is_line_hidden` was consulted only by the render loops and by the
        // fold toggles' own cursor snap. No cursor-movement path knew about
        // folds, so Down walked straight into a collapsed region: the caret
        // vanished (`cursor_screen_pos` cannot map a hidden line) and anything
        // typed went into content the user could not see.
        let mut e = editor_with("fn a() {\n    x\n    y\n}\nafter");
        e.toggle_fold(0);
        assert_eq!(e.cursor_row, 0, "the fold snapped the caret to the header");
        e.move_down();
        assert_eq!(
            e.cursor_row, 3,
            "Down clears the whole collapsed body, landing on the next visible line"
        );
        assert!(!e.is_line_hidden(e.cursor_row));
        e.move_up();
        assert_eq!(e.cursor_row, 0, "Up steps back over it the same way");
    }

    #[test]
    fn editing_reveals_a_fold_the_caret_ended_up_inside() {
        // Not every way into a fold is an arrow key — search, go-to-definition
        // and goto-line all set `cursor_row` directly. Wherever the caret came
        // from, an edit must never land on a line the user cannot see.
        let mut e = editor_with("fn a() {\n    x\n    y\n}\nafter");
        e.toggle_fold(0);
        e.cursor_row = 1;
        // Past the indent: typing at column 0 would strip line 1's leading
        // whitespace and collapse the fold RANGE, which unhides the line for a
        // reason that has nothing to do with the caret.
        e.cursor_col = 5;
        assert!(e.is_line_hidden(1), "the caret is parked inside the fold");
        e.insert_char('Z');
        assert!(
            !e.is_line_hidden(1),
            "the edit revealed the region it landed in"
        );
        assert!(e.lines[1].contains('Z'), "and the text went to line 1");
    }

    /// Waiting for an EDIT to reveal the fold is too late for a jump: a caret
    /// on a hidden line has no painted row at all, so `cursor_screen_pos`
    /// returns `None` and no caret is drawn anywhere on screen. Ctrl+G stands
    /// in for every direct `cursor_row` setter (go-to-definition, search).
    #[test]
    fn jumping_into_a_collapsed_block_opens_it_so_the_caret_is_painted() {
        let mut e = editor_with("fn a() {\n    x\n    y\n}\nafter");
        e.focused = true;
        e.toggle_fold(0);
        assert!(e.is_line_hidden(1), "the block starts collapsed");
        e.goto_line(2); // one-based: line index 1, inside the fold
        assert_eq!(e.cursor_row, 1);
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        assert!(
            !e.is_line_hidden(1),
            "the jump target's own block must open"
        );
        assert!(
            e.cursor_screen_pos().is_some(),
            "so the caret has somewhere to be painted"
        );
    }

    #[test]
    fn opening_another_file_clears_fold_state() {
        // `open` resets scroll, cursor, selection, undo, tokens and hints, but
        // left `folded` alone. A preview tab is REUSED for the next
        // single-click, so the new file arrived with the old file's fold
        // headers collapsed — and the line-count guard only fires when the two
        // files differ in length.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        std::fs::write(&a, "fn a() {\n    x\n}\n").unwrap();
        std::fs::write(&b, "fn b() {\n    y\n}\n").unwrap();
        let mut e = Editor::new();
        e.open(&a).unwrap();
        e.toggle_fold(0);
        assert!(e.is_line_hidden(1), "a.rs is folded");
        e.open(&b).unwrap();
        assert!(
            !e.is_line_hidden(1),
            "the next file must not inherit the previous file's folds"
        );
    }

    #[test]
    fn the_fold_lookup_is_derived_once_per_fold_change_not_per_query() {
        // `is_line_hidden` ran on every rendered row of every frame and
        // re-derived itself from `folded`, scanning each folded region. Fold
        // All on a large file turned a single frame into millions of
        // iterations. The spans are now built once per fold write.
        let text = (0..400)
            .map(|i| format!("fn f{i}() {{\n    body\n}}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut e = editor_with(&text);
        e.fold_all();
        assert!(
            !e.folded.is_empty(),
            "Fold All must actually fold something"
        );

        FOLD_RANGE_REBUILDS.with(|c| c.set(0));
        for line in 0..e.lines.len() {
            std::hint::black_box(e.is_line_hidden(line));
        }
        assert_eq!(
            FOLD_RANGE_REBUILDS.with(|c| c.get()),
            0,
            "querying must never rebuild the spans"
        );

        e.toggle_fold(0);
        assert_eq!(
            FOLD_RANGE_REBUILDS.with(|c| c.get()),
            1,
            "changing the fold set rebuilds exactly once"
        );
    }

    #[test]
    fn toggling_a_fold_twice_restores_visibility() {
        let mut e = editor_with("fn a() {\n    body\n}");
        e.toggle_fold(0);
        assert!(e.is_line_hidden(1));
        e.toggle_fold(0);
        assert!(!e.is_line_hidden(1), "a second toggle unfolds the block");
    }

    #[test]
    fn folding_from_inside_the_body_folds_the_enclosing_block() {
        let mut e = editor_with("fn a() {\n    body\n}");
        e.cursor_row = 1;
        e.toggle_fold(e.cursor_row);
        assert!(
            e.is_line_hidden(1),
            "folding at a body line folds its owner"
        );
        assert_eq!(
            e.cursor_row, 0,
            "cursor on a hidden line snaps up to the header"
        );
    }

    #[test]
    fn render_omits_folded_body_lines() {
        let mut e = editor_with("fn a() {\n    x\n    y\n}\nfn b() {}");
        e.toggle_fold(0);
        render_at(&mut e, 40, 10);
        let shown: Vec<usize> = (0..e.last_wrap_rows.len())
            .filter_map(|i| e.text_row(i).map(|t| t.0))
            .collect();
        assert!(shown.contains(&0), "the header row is drawn");
        assert!(!shown.contains(&1), "a folded body line is not drawn");
        assert!(!shown.contains(&2), "a folded body line is not drawn");
        assert!(
            shown.contains(&3) && shown.contains(&4),
            "lines after the fold are still drawn"
        );
    }

    #[test]
    fn fold_all_collapses_every_block_and_unfold_all_restores() {
        let mut e = editor_with("fn a() {\n    x\n}\nfn b() {\n    y\n}");
        e.fold_all();
        assert!(
            e.is_line_hidden(1) && e.is_line_hidden(4),
            "every body is hidden"
        );
        e.unfold_all();
        assert!(
            !e.is_line_hidden(1) && !e.is_line_hidden(4),
            "unfold restores all"
        );
    }

    #[test]
    fn clicking_the_gutter_chevron_toggles_the_fold() {
        let mut e = editor_with("fn a() {\n    x\n}\nfn b() {}");
        render_at(&mut e, 40, 10);
        // The chevron is the second sign-margin cell; the header sits on the
        // first visual row at the top of the inner rect.
        let cx = e.last_inner.x + 1;
        let cy = e.last_inner.y;
        assert!(
            e.fold_chevron_at(cx, cy),
            "a click on the header chevron hits"
        );
        assert!(e.is_line_hidden(1), "the body is now folded");
        assert!(e.fold_chevron_at(cx, cy), "a second click unfolds");
        assert!(!e.is_line_hidden(1));
    }

    #[test]
    fn clicking_off_the_chevron_column_is_not_a_fold_toggle() {
        let mut e = editor_with("fn a() {\n    x\n}");
        render_at(&mut e, 40, 10);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        assert!(
            !e.fold_chevron_at(text_x, e.last_inner.y),
            "the text area is not the chevron"
        );
        assert!(!e.is_line_hidden(1));
    }

    #[test]
    fn ghost_carets_paint_participant_colored_cells() {
        let f = NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "hello world\nsecond line\n").unwrap();
        let mut ed = Editor::new();
        ed.open(f.path()).unwrap();
        let color = Color::Rgb(1, 2, 3);
        // One caret at (0, 6); one on a row far past the buffer, which must
        // simply never paint.
        ed.ghost_carets = vec![(0, 6, color), (99, 0, color)];
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 10,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
        let hits: Vec<(u16, u16)> = (area.y..area.bottom())
            .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].style().bg == Some(color))
            .collect();
        assert_eq!(hits.len(), 1, "exactly one ghost caret cell: {hits:?}");
        let (x, y) = hits[0];
        assert_eq!(y, ed.last_inner.y, "ghost sits on the first content row");
        let text_x = ed.last_inner.x + ed.last_gutter_width + 1;
        assert_eq!(x, text_x + 6, "ghost sits at char column 6");
    }

    /// A ghost caret's name tag paints on the visual row above the caret at
    /// the caret's column; on the viewport's top row it falls back below.
    #[test]
    fn ghost_caret_labels_paint_name_above_and_fall_back_below() {
        let f = NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "hello world\nsecond line\nthird line\n").unwrap();
        let mut ed = Editor::new();
        ed.open(f.path()).unwrap();
        let color = Color::Rgb(9, 8, 7);
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 10,
        };
        let text_x = |ed: &Editor| ed.last_inner.x + ed.last_gutter_width + 1;

        // Caret on line 1: the tag paints on the row above (line 0's row).
        ed.ghost_caret_labels = vec![(1, 2, "alice".into(), color)];
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
        let x0 = text_x(&ed) + 2;
        let y0 = ed.last_inner.y;
        let shown: String = (0..5)
            .map(|i| buf[(x0 + i, y0)].symbol().to_string())
            .collect();
        assert_eq!(shown, "alice", "tag paints above the caret at its column");
        assert_eq!(
            buf[(x0, y0)].style().bg,
            Some(color),
            "tag wears the participant color"
        );

        // Caret on the top row: the tag falls back to the row below.
        ed.ghost_caret_labels = vec![(0, 0, "bob".into(), color)];
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
        let x1 = text_x(&ed);
        let y1 = ed.last_inner.y + 1;
        let shown: String = (0..3)
            .map(|i| buf[(x1 + i, y1)].symbol().to_string())
            .collect();
        assert_eq!(shown, "bob", "tag falls back below on the top row");
    }
    // ---- Auto-closing pairs (#121) ----

    #[test]
    fn typing_an_opener_auto_closes_with_the_caret_between() {
        let mut e = editor_with("fn main() ");
        e.cursor_row = 0;
        e.cursor_col = 10; // end of line
        e.insert_char('{');
        assert_eq!(e.lines[0], "fn main() {}");
        assert_eq!(e.cursor_col, 11, "the caret sits between the pair");
        // One undo removes BOTH characters.
        e.undo();
        assert_eq!(e.lines[0], "fn main() ");
    }

    #[test]
    fn openers_do_not_auto_close_before_a_word_character() {
        let mut e = editor_with("foo");
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.insert_char('(');
        assert_eq!(
            e.lines[0], "(foo",
            "no closer may be jammed into the following word"
        );
    }

    #[test]
    fn typing_the_closer_steps_over_instead_of_inserting() {
        let mut e = editor_with("");
        e.insert_char('(');
        assert_eq!(e.lines[0], "()");
        e.insert_char(')');
        assert_eq!(e.lines[0], "()", "the closer types over, never doubles");
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn an_apostrophe_inside_a_word_stays_single() {
        let mut e = editor_with("don");
        e.cursor_row = 0;
        e.cursor_col = 3;
        e.insert_char('\'');
        assert_eq!(e.lines[0], "don'", "no auto-pair after a word character");
    }

    #[test]
    fn typing_a_bracket_with_a_selection_surrounds_it() {
        let mut e = editor_with("abc def");
        e.cursor_row = 0;
        e.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (0, 3),
        }); // "abc"
        e.insert_char('(');
        assert_eq!(e.lines[0], "(abc) def");
        let sel = e
            .selection
            .expect("the selection survives, on the inner text");
        let ((sr, sc), (er, ec)) = sel.normalised();
        assert_eq!((sr, sc, er, ec), (0, 1, 0, 4));
        e.undo();
        assert_eq!(e.lines[0], "abc def", "surround is one undo step");
    }

    #[test]
    fn backspace_between_an_empty_pair_deletes_both() {
        let mut e = editor_with("");
        e.insert_char('(');
        assert_eq!(e.lines[0], "()");
        e.backspace();
        assert_eq!(e.lines[0], "", "the empty pair dies together");
    }

    #[test]
    fn typing_an_opener_into_a_fresh_empty_buffer_never_panics() {
        // Editor::new starts with NO lines at all; the pair path indexed
        // lines[0] and panicked on the first keystroke of an untitled
        // buffer (#122 review, Critical).
        let mut e = Editor::new();
        e.auto_close_pairs = true;
        e.insert_char('(');
        assert_eq!(e.lines[0], "()");
    }

    #[test]
    fn backspace_keeps_the_closer_of_a_pre_existing_pair() {
        // Only the pair the last auto-close inserted may die together; a
        // `()` already in the file keeps its closer (#122 review).
        let mut e = editor_with("()");
        e.cursor_row = 0;
        e.cursor_col = 1;
        e.backspace();
        assert_eq!(e.lines[0], ")", "the manual pair loses only one side");
    }

    #[test]
    fn the_toggle_disables_every_pair_behavior() {
        let mut e = editor_with("");
        e.auto_close_pairs = false;
        e.insert_char('(');
        assert_eq!(e.lines[0], "(", "off means plain inserts");
        e.insert_char(')');
        assert_eq!(e.lines[0], "()");
        e.cursor_col = 1;
        e.backspace();
        assert_eq!(e.lines[0], ")", "off means plain backspace");
    }
}
