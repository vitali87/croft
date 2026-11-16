//! Files the editor shows rather than edits as text: pictures and SVG, PDF pages,
//! spreadsheet and SQLite grids (their edits and saves too), archives, hex, logs and gzip,
//! SARIF, and the Markdown and notebook previews.

use super::*;

/// The most a single gzipped file may decompress to and still open as text
/// (#1469); past it the file stays a hex dump of its gzip bytes.
pub(super) const MAX_GUNZIP_BYTES: u64 = 256 * 1024 * 1024;

pub(super) const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

/// An image's pixel size from its header alone. Opening a tab decoded the
/// whole picture on the render thread just to read these two numbers: a
/// 48-megapixel photo meant a ~190 MB decode per open, and one past the
/// decoder's memory limit refused to open although only a downscale shows.
pub(super) fn image_dimensions_of(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

pub(super) const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff",
];

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
    /// The drawing's own size, as the header shows it, when the raster is a
    /// scaled rendering of it: an SVG's CSS-pixel width and height (#1008).
    /// Shown rather than `pixel_w`×`pixel_h`, which is croft's render
    /// quality. `None` for bitmaps, whose pixels are their size.
    pub natural_size: Option<String>,
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
pub(super) fn next_image_generation() -> u64 {
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
pub(super) fn canvas_bg(host_is_iterm2: bool, theme: crate::theme::Theme) -> Color {
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
pub(super) fn image_canvas_bg(
    protocol: crate::iterm2_inline::InlineImageProtocol,
    theme: crate::theme::Theme,
) -> Color {
    use crate::iterm2_inline::InlineImageProtocol;
    match protocol {
        InlineImageProtocol::ITerm2 | InlineImageProtocol::Kitty => Color::Reset,
        InlineImageProtocol::Sixel | InlineImageProtocol::None => theme.editor_bg(),
    }
}

pub(super) fn render_image_placeholder(
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
        let size = match &image.natural_size {
            Some(size) => size.clone(),
            None => format!("{}×{}", image.pixel_w, image.pixel_h),
        };
        format!(
            " {} · {} · {} · {} ",
            name,
            size,
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

/// A CSS-pixel size for the image header: each length whole when it is
/// (nearly) whole, else to one decimal, so `10mm` reads `37.8` rather than
/// `37.795277`.
pub(super) fn css_px_size((w, h): (f32, f32)) -> String {
    let px = |v: f32| {
        let tenths = (v * 10.0).round();
        if tenths % 10.0 == 0.0 {
            format!("{}", tenths / 10.0)
        } else {
            format!("{:.1}", tenths / 10.0)
        }
    };
    format!("{}×{}", px(w), px(h))
}

/// Half-block cells as `(columns, rows, (upper, lower) colours)`.
pub(super) type HalfBlocks = (u16, u16, Vec<(Color, Color)>);

/// Decode `bytes` and shrink the picture to fit `cols` × `rows` cells, two
/// pixels a cell, keeping its shape (a cell is about twice as tall as it is
/// wide, so a half cell is square) and never enlarging it. Transparency is
/// blended over `bg`. `None` for bytes the decoder refuses or an empty
/// area. Returns the cells' `(columns, rows, (upper, lower) colours)`.
pub(super) fn halfblock_cells(bytes: &[u8], cols: u16, rows: u16, bg: Color) -> Option<HalfBlocks> {
    if cols == 0 || rows == 0 {
        return None;
    }
    // The header is cheap and the decode is not: a small compressed file can
    // declare a canvas that decodes to gigabytes. The same budget the
    // inline-image bake enforces (#1160), checked before the decode.
    let (dw, dh) = image_dimensions_of(bytes)?;
    if u64::from(dw) * u64::from(dh) > crate::iterm2_inline::MAX_DECODE_PIXELS {
        return None;
    }
    #[cfg(test)]
    HALFBLOCK_DECODES.with(|c| c.set(c.get() + 1));
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (iw, ih) = img.dimensions();
    if iw == 0 || ih == 0 {
        return None;
    }
    let (max_w, max_h) = (u32::from(cols), u32::from(rows) * 2);
    let scale = (f64::from(max_w) / f64::from(iw))
        .min(f64::from(max_h) / f64::from(ih))
        .min(1.0);
    let w = ((f64::from(iw) * scale).round() as u32).clamp(1, max_w);
    let h = ((f64::from(ih) * scale).round() as u32).clamp(1, max_h);
    let small = if (w, h) == (iw, ih) {
        img
    } else {
        image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle)
    };
    let (br, bg_g, bb) = match bg {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    };
    let blend = |p: &image::Rgba<u8>| {
        let a = u16::from(p[3]);
        let mix =
            |c: u8, under: u8| ((u16::from(c) * a + u16::from(under) * (255 - a)) / 255) as u8;
        Color::Rgb(mix(p[0], br), mix(p[1], bg_g), mix(p[2], bb))
    };
    let cell_rows = h.div_ceil(2);
    let mut cells = Vec::with_capacity((w * cell_rows) as usize);
    for r in 0..cell_rows {
        for x in 0..w {
            let upper = blend(small.get_pixel(x, 2 * r));
            let lower = if 2 * r + 1 < h {
                blend(small.get_pixel(x, 2 * r + 1))
            } else {
                Color::Rgb(br, bg_g, bb)
            };
            cells.push((upper, lower));
        }
    }
    Some((w as u16, cell_rows as u16, cells))
}

/// Paint `picture` centred in `area`.
pub(super) fn paint_cell_image(picture: &CellImage, area: Rect, buf: &mut Buffer) {
    let x0 = area.x + area.width.saturating_sub(picture.cols) / 2;
    let y0 = area.y + area.height.saturating_sub(picture.rows) / 2;
    for r in 0..picture.rows.min(area.height) {
        for c in 0..picture.cols.min(area.width) {
            let (upper, lower) =
                picture.cells[usize::from(r) * usize::from(picture.cols) + usize::from(c)];
            let cell = &mut buf[(x0 + c, y0 + r)];
            cell.set_symbol("\u{2580}");
            cell.set_style(Style::default().fg(upper).bg(lower));
        }
    }
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
pub(super) fn render_log(
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
            if span.style.strikeout {
                style = style.add_modifier(Modifier::CROSSED_OUT);
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
                &RowCells::default(),
                active_on_line,
                Some(&cells),
                None,
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
pub(super) fn paint_log_cells(
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
pub(super) fn render_hex(
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
            // A high contrast theme's selection may be pale (High Contrast
            // Light, #1548): its pure foreground keeps selected bytes legible.
            let selected_fg = if theme.high_contrast() {
                theme.ui(Color::White)
            } else {
                Color::White
            };
            let base_style = if selected {
                Style::default().fg(selected_fg).bg(theme.selection())
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
pub(super) fn render_archive(
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

pub(super) fn render_sheet(
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
    let gutter_w = ((sheet.row_base + row_count).max(1).to_string().len() as u16 + 2).max(4);
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
    // out of horizontal space: (col_idx, x_offset, drawn width).
    let visible = crate::sheet::visible_columns(&sheet.col_widths, sheet.scroll_col, body_w);

    // Header text.
    for &(c, x_off, w) in &visible {
        let label = sheet.headers.get(c).map(|s| s.as_str()).unwrap_or("");
        let cell_x = body_x + x_off;
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
        // Numbered by place in the source (#1222): a SQLite page's rows
        // continue from the pages before it.
        let row_label = format!(
            " {:>width$} ",
            sheet.row_base + row_idx + 1,
            width = (gutter_w - 2) as usize
        );
        buf.set_string(
            inner.x,
            y,
            &row_label,
            gutter_style.bg(style.bg.unwrap_or(Color::Reset)),
        );
        let row = &sheet.rows[row_idx];
        for (c, x_off, w) in &visible {
            let w = *w;
            let cell_text = row.get(*c).map(|s| s.as_str()).unwrap_or("");
            let is_cursor = row_idx == sheet.cur_row && *c == sheet.cur_col;
            if is_cursor {
                if let Some(edit) = editing.as_ref() {
                    // In-grid editor (#177): show the tail that keeps the
                    // caret visible, caret cell reversed. The window is
                    // measured in screen columns (#1138), whole graphemes
                    // dropped from the left until what is before the caret
                    // fits with a column left for the caret itself.
                    use unicode_segmentation::UnicodeSegmentation;
                    let avail = w as usize;
                    let before = &edit.value[..edit.cursor];
                    let mut caret_cells = crate::sheet::text_width(before);
                    let mut start = 0;
                    let mut dropped = before.grapheme_indices(true);
                    while caret_cells + 1 > avail {
                        let Some((i, g)) = dropped.next() else {
                            break;
                        };
                        caret_cells -= crate::sheet::grapheme_width(g);
                        start = i + g.len();
                    }
                    let shown = &edit.value[start..];
                    let edit_style = Style::default()
                        .fg(theme.accent_contrast_fg())
                        .bg(theme.accent());
                    write_cell(buf, body_x + *x_off, y, w, shown, edit_style);
                    let caret_cells = caret_cells.min(u16::MAX as usize) as u16;
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
                    if sheet.null_cells.contains(&(row_idx, *c)) {
                        let null_style = cursor_style.add_modifier(Modifier::ITALIC);
                        let null = crate::sqlite_view::NULL_LABEL;
                        write_cell(buf, body_x + *x_off, y, w, null, null_style);
                    } else {
                        write_cell(buf, body_x + *x_off, y, w, cell_text, cursor_style);
                    }
                }
            } else if sheet.null_cells.contains(&(row_idx, *c)) {
                // SQL NULL (#1002): dimmed, so it reads apart from `''`.
                let null_style = style.fg(Color::DarkGray).add_modifier(Modifier::ITALIC);
                let null = crate::sqlite_view::NULL_LABEL;
                write_cell(buf, body_x + *x_off, y, w, null, null_style);
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
        .map(|(c, _, _)| *c + 1)
        .unwrap_or(visible_col_first);
    // A SQLite page names its rows by place in the table, with no total:
    // nobody counted the table (#1222), and the sheet name says whether
    // more rows follow.
    let rows = if view.kind == crate::sheet::SheetKind::Sqlite {
        let base = sheet.row_base;
        format!("rows {}–{}", base + visible_first, base + visible_last)
    } else {
        format!("rows {visible_first}–{visible_last} of {row_count}")
    };
    let status = format!(
        " {rows} · cols {visible_col_first}–{visible_col_last} of {col_count} · ←/→ ↑/↓ PgUp/PgDn Tab=next sheet "
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

pub(crate) fn format_bytes_human(n: u64) -> String {
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
        "tif" | "tiff" => "TIFF".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

/// The delimiter a CSV or TSV grid was read and is written with.
pub(super) fn sheet_delim(kind: crate::sheet::SheetKind) -> u8 {
    match kind {
        crate::sheet::SheetKind::Tsv => b'\t',
        _ => b',',
    }
}

impl Editor {
    /// Drop every view this tab could be showing other than its text: the
    /// diff and merge views with their hunk-arrow rects, the image or PDF,
    /// the sheet, hex, log and archive views, the Markdown, document or
    /// media preview, and the PR review, fleet and SARIF views.
    ///
    /// Every file opener calls this before installing its own view, so a
    /// tab reused for another file (the preview tab) keeps nothing of what
    /// it showed before. The render and the key dispatch each pick the first
    /// view that is set, so a leftover one paints over the new file or takes
    /// its keys: a diff reopened as a preview kept its clickable hunk arrows
    /// (#187), a stale log kept painting over "Reopen as Hex", and a
    /// document preview kept the keys of the image, PDF or sheet opened after
    /// it (#2069).
    pub(super) fn clear_viewers(&mut self) {
        self.diff = None;
        self.merge = None;
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();
        self.image = None;
        self.sheet = None;
        self.hex = None;
        self.log = None;
        self.archive = None;
        self.markdown_preview = None;
        self.pr_review = None;
        self.fleet = None;
        self.sarif = None;
    }

    pub(super) fn open_image(&mut self, path: &Path) -> Result<()> {
        let meta = std::fs::metadata(path)?;
        if meta.len() > MAX_IMAGE_BYTES {
            anyhow::bail!("Image too large ({} bytes)", meta.len());
        }
        let bytes = std::fs::read(path)?;
        let (pixel_w, pixel_h) =
            image_dimensions_of(&bytes).ok_or_else(|| anyhow::anyhow!("Could not decode image"))?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let format_label = image_format_label_from_ext(ext);
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_expect_absent = false;
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // The map described the text this swap replaced (#349); a viewer's
        // placeholder line was written by no seat, and a save from the
        // viewer must not persist the old map against the new bytes.
        self.provenance = crate::provenance::Provenance::new();
        // A whole-buffer swap like every other opener: caches memoised on
        // edit_seq (conflicts, git marks) must not survive into this tab.
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.language_by_hand = false;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.clear_viewers();
        self.image = Some(ImageView {
            bytes,
            format_label,
            pixel_w,
            pixel_h,
            natural_size: None,
            byte_size: meta.len(),
            generation: next_image_generation(),
            pdf: None,
        });
        self.status = format!("Opened image {}", path.display());
        Ok(())
    }

    pub(super) fn open_sheet(&mut self, path: &Path) -> Result<()> {
        let view = crate::sheet::open_sheet(path)
            .map_err(|e| anyhow::anyhow!("Spreadsheet open failed: {e}"))?;
        self.install_sheet_view(path, view);
        Ok(())
    }

    /// Media info view (#183): header-probed properties rendered as a
    /// markdown card, with an ffmpeg poster frame when available.
    pub(super) fn open_media_preview(&mut self, path: &Path) -> Result<()> {
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
        // The map described the text this swap replaced (#349); a viewer's
        // placeholder line was written by no seat, and a save from the
        // viewer must not persist the old map against the new bytes.
        self.provenance = crate::provenance::Provenance::new();
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.language_by_hand = false;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.clear_viewers();
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
            source_map: Vec::new(),
            row_of_line: Vec::new(),
            scroll_to_source: None,
            links: crate::markdown::MdLinks::default(),
            follows: None,
        });
        self.status = format!("Opened media info {}", path.display());
        Ok(())
    }

    /// Read-only SQLite browser (#182): tables as sheet-grid
    /// worksheets, through the standard sheet install.
    pub(super) fn open_sqlite(&mut self, path: &Path) -> Result<()> {
        let view = crate::sqlite_view::open_database(path)
            .map_err(|e| anyhow::anyhow!("SQLite open failed: {e}"))?;
        self.install_sheet_view(path, view);
        Ok(())
    }

    /// docx/odt read-only preview (#181): the document walks into
    /// markdown and renders through the preview machinery; embedded
    /// images extract to scratch and ride the inline-image overlay.
    pub(super) fn open_doc_preview(&mut self, path: &Path) -> Result<()> {
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
        // The map described the text this swap replaced (#349); a viewer's
        // placeholder line was written by no seat, and a save from the
        // viewer must not persist the old map against the new bytes.
        self.provenance = crate::provenance::Provenance::new();
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.language_by_hand = false;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.clear_viewers();
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
            source_map: Vec::new(),
            row_of_line: Vec::new(),
            scroll_to_source: None,
            links: crate::markdown::MdLinks::default(),
            follows: None,
        });
        self.status = format!("Opened document {}", path.display());
        Ok(())
    }

    /// Archive browser (#179): list the members read-only; Enter
    /// extracts one to scratch and reopens it through the dispatch.
    pub(super) fn open_archive(
        &mut self,
        path: &Path,
        kind: crate::archive::ArchiveKind,
    ) -> Result<()> {
        let view = crate::archive::list(path, kind)
            .map_err(|e| anyhow::anyhow!("Archive open failed: {e}"))?;
        // A listing that succeeds positively disproves any refusal armed
        // for this path, so it cannot be left to explain a later view.
        self.route_note = None;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // The map described the text this swap replaced (#349); a viewer's
        // placeholder line was written by no seat, and a save from the
        // viewer must not persist the old map against the new bytes.
        self.provenance = crate::provenance::Provenance::new();
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.language_by_hand = false;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.clear_viewers();
        self.archive = Some(view);
        self.status = format!("Opened archive {}", path.display());
        Ok(())
    }

    /// Rendered SVG preview (#175): rasterise through resvg at a fixed
    /// quality (the PDF philosophy — one raster per open, no re-render
    /// on pane resize) and hand the PNG to the standard image pipeline.
    pub(super) fn open_svg(&mut self, path: &Path) -> Result<()> {
        let meta = std::fs::metadata(path)?;
        if meta.len() > crate::svg::MAX_SVG_BYTES {
            anyhow::bail!("SVG too large ({} bytes)", meta.len());
        }
        let src = std::fs::read(path)?;
        let crate::svg::Raster {
            png,
            pixels: (pixel_w, pixel_h),
            natural,
        } = crate::svg::render(&src).map_err(|e| anyhow::anyhow!("SVG render failed: {e}"))?;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_expect_absent = false;
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // The map described the text this swap replaced (#349); a viewer's
        // placeholder line was written by no seat, and a save from the
        // viewer must not persist the old map against the new bytes.
        self.provenance = crate::provenance::Provenance::new();
        // A whole-buffer swap like every other opener: caches memoised on
        // edit_seq (conflicts, git marks) must not survive into this tab.
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.language_by_hand = false;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.clear_viewers();
        self.image = Some(ImageView {
            bytes: png,
            format_label: String::from("SVG"),
            pixel_w,
            pixel_h,
            natural_size: Some(match natural {
                crate::svg::NaturalSize::Px(w, h) => css_px_size((w, h)),
                crate::svg::NaturalSize::Relative(w, h) => format!("{w}×{h}"),
            }),
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
    pub(super) fn install_sheet_view(&mut self, path: &Path, view: crate::sheet::SheetView) {
        self.path = Some(path.to_path_buf());
        self.disk_stamp =
            Self::disk_stamp_with_wal(path, view.kind == crate::sheet::SheetKind::Sqlite);
        self.disk_expect_absent = false;
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // The map described the text this swap replaced (#349); a viewer's
        // placeholder line was written by no seat, and a save from the
        // viewer must not persist the old map against the new bytes.
        self.provenance = crate::provenance::Provenance::new();
        // A whole-buffer swap like every other opener: caches memoised on
        // edit_seq (conflicts, git marks) must not survive into this tab.
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.language_by_hand = false;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.clear_viewers();
        self.status = format!("Opened {} ({})", path.display(), view.kind.label());
        self.sheet = Some(view);
    }

    pub(super) fn open_pdf(&mut self, path: &Path) -> Result<()> {
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
        let (pixel_w, pixel_h) = image_dimensions_of(&bytes)
            .ok_or_else(|| anyhow::anyhow!("Could not decode rasterised PDF"))?;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_expect_absent = false;
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // The map described the text this swap replaced (#349); a viewer's
        // placeholder line was written by no seat, and a save from the
        // viewer must not persist the old map against the new bytes.
        self.provenance = crate::provenance::Provenance::new();
        // A whole-buffer swap like every other opener: caches memoised on
        // edit_seq (conflicts, git marks) must not survive into this tab.
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.language_by_hand = false;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.clear_viewers();
        self.image = Some(ImageView {
            bytes,
            format_label: String::from("PDF"),
            pixel_w,
            pixel_h,
            natural_size: None,
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
            || self.pr_review.is_some()
            || self.fleet.is_some()
            || self.sarif.is_some()
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

    /// Route a gzip-magic file: a tar inside opens the archive browser, one
    /// compressed text file opens as its text (#1469). `false` leaves the
    /// caller to fall back to hex; a size refusal on either route rides
    /// along as the route note so the viewer can say why.
    pub(super) fn route_gzip(&mut self, path: &Path) -> bool {
        match self.open_archive(path, crate::archive::ArchiveKind::TarGz) {
            Ok(()) => return true,
            Err(e) if crate::archive::is_list_cap_refusal(&e.to_string()) => {
                self.route_note = Some((path.to_path_buf(), e.to_string()));
                return false;
            }
            Err(_) => {}
        }
        // Not a tar: one compressed file, such as a rotated `app.log.1.gz`.
        // Its text opens; binary, or too large once decompressed, stays hex.
        match self.open_gunzipped(path) {
            Ok(()) => true,
            Err(e) => {
                if e.to_string().contains("too large") {
                    self.route_note = Some((path.to_path_buf(), e.to_string()));
                }
                false
            }
        }
    }

    /// A gzip of one file (#1469), such as logrotate's `app.log.1.gz` or a
    /// `dump.sql.gz`: its text, decompressed to a scratch copy, in the
    /// read-only log view, so it reads like the file it was and a save can
    /// never write the text over the archive. It used to open as a hex dump
    /// of the gzip bytes. Fails, so the caller falls back to hex, when the
    /// file does not decompress, holds binary data, or decompresses to more
    /// than [`MAX_GUNZIP_BYTES`], a bound against a decompression bomb.
    pub(super) fn open_gunzipped(&mut self, path: &Path) -> Result<()> {
        use std::io::{Read, Write};
        let file = std::fs::File::open(path)?;
        let mut decoder = flate2::read::MultiGzDecoder::new(std::io::BufReader::new(file))
            .take(MAX_GUNZIP_BYTES + 1);
        let mut head = vec![0u8; 8192];
        let mut filled = 0;
        while filled < head.len() {
            match decoder.read(&mut head[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        head.truncate(filled);
        // The log view reads UTF-8 only, so a UTF-16 BOM would render as
        // NULs and replacement characters: such a payload stays hex.
        match encoding_rs::Encoding::for_bom(&head) {
            Some((enc, _)) if enc != encoding_rs::UTF_8 => {
                anyhow::bail!("not UTF-8 text once decompressed");
            }
            Some(_) => {}
            None if is_binary(&head) => anyhow::bail!("not text once decompressed"),
            None => {}
        }
        let inner = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.strip_suffix(".gz").unwrap_or(n).to_string())
            .unwrap_or_default();
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            path.hash(&mut h);
            h.finish()
        };
        let scratch = gunzip_scratch_dir()?.join(format!("{key:016x}-{inner}"));
        // Never follow a link planted at the name: unlink whatever is there
        // (a link, not its target) and create the file exclusively.
        match std::fs::remove_file(&scratch) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&scratch)?;
        out.write_all(&head)?;
        let written = head.len() as u64 + std::io::copy(&mut decoder, &mut out)?;
        drop(out);
        if written > MAX_GUNZIP_BYTES {
            let _ = std::fs::remove_file(&scratch);
            anyhow::bail!(
                "Decompressed file too large to show (over {} MiB)",
                MAX_GUNZIP_BYTES / (1024 * 1024)
            );
        }
        self.open_log(&scratch)?;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.status = format!("Opened {} decompressed, read-only", path.display());
        Ok(())
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
        // The map described the text this swap replaced (#349); a viewer's
        // placeholder line was written by no seat, and a save from the
        // viewer must not persist the old map against the new bytes.
        self.provenance = crate::provenance::Provenance::new();
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.language_by_hand = false;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.clear_viewers();
        self.log = Some(view);
        self.log_reload_top = None;
        self.status = format!("Opened {} as a rendered log", path.display());
        Ok(())
    }

    /// Open `path` in the SARIF results viewer (#577). Fails with the
    /// loader's explanation (position of a JSON error, an unsupported
    /// version) so the caller can fall back to text and say why.
    pub fn open_sarif(&mut self, path: &Path) -> Result<()> {
        let meta = std::fs::metadata(path)?;
        if meta.len() > crate::sarif::view::MAX_LOG_BYTES {
            anyhow::bail!("SARIF log too large to load ({} bytes)", meta.len());
        }
        let bytes = std::fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        let log = crate::sarif::load::parse_log(&text).map_err(|e| anyhow::anyhow!("{e}"))?;
        let view = crate::sarif::view::SarifView::open(path, log);
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // The map described the text this swap replaced (#349); a viewer's
        // placeholder line was written by no seat, and a save from the
        // viewer must not persist the old map against the new bytes.
        self.provenance = crate::provenance::Provenance::new();
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.language_by_hand = false;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.clear_viewers();
        let n = view.entries.len();
        self.sarif = Some(view);
        self.status = format!(
            "Opened {} · {n} SARIF result{}",
            path.display(),
            if n == 1 { "" } else { "s" }
        );
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
            // A refresh writes no status line, so a note armed by the open
            // that called us would outlive its open and go stale against a
            // file that may since have come under the cap.
            self.route_note = None;
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
        self.disk_expect_absent = false;
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.lines = vec![String::new()];
        // The map described the text this swap replaced (#349); a viewer's
        // placeholder line was written by no seat, and a save from the
        // viewer must not persist the old map against the new bytes.
        self.provenance = crate::provenance::Provenance::new();
        // A whole-buffer swap like every other opener: caches memoised on
        // edit_seq (conflicts, git marks) must not survive into this tab.
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.lang = None;
        self.language_by_hand = false;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.clear_viewers();
        self.hex = Some(view);
        // Only for the file it was set for: an open can set the note and
        // then fail before reaching a viewer, and this is also reached
        // directly by "Reopen as Hex", so a note that outlived its open
        // would explain the wrong file.
        self.status = match self.route_note.take() {
            Some((noted, why)) if noted == path => {
                format!("Opened {} in the hex viewer: {why}", path.display())
            }
            _ => format!("Opened {} in the hex viewer", path.display()),
        };
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
                // The file's own line ending, read from its first line.
                let original = std::fs::read(&path).unwrap_or_default();
                let crlf = original
                    .iter()
                    .position(|&c| c == b'\n')
                    .is_some_and(|i| i > 0 && original[i - 1] == b'\r');
                // Unchanged rows go back as the file had them (#1375).
                let bytes = crate::sheet::serialize_delimited(data, delim, crlf, &original);
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

    /// The cell being typed into in this tab's grid, when what is typed
    /// differs from the cell (#1593): `(sheet, row, col, value)`. It is
    /// in no cell yet, so hot exit reads it here.
    pub fn sheet_pending_cell(&self) -> Option<(usize, usize, usize, &str)> {
        let view = self.sheet.as_ref()?;
        let edit = view.editing.as_ref()?;
        let data = view.sheets.get(view.current_sheet)?;
        let (r, c) = (data.cur_row, data.cur_col);
        (data.cell(r, c) != edit.value).then_some((view.current_sheet, r, c, edit.value.as_str()))
    }

    /// What hot exit and the update relaunch keep of a grid's unsaved
    /// edits (#1593), `None` when it has none: a CSV/TSV grid as the text a
    /// save would write, an xlsx as its edited cells `(sheet, row, col,
    /// value)` by worksheet name, the cells its save applies. The cell being typed into
    /// counts as edited, as a save would commit it.
    pub fn sheet_backup(&self) -> Option<(Option<String>, Vec<SheetCellEdit>)> {
        let view = self.sheet.as_ref()?;
        let pending = self.sheet_pending_cell();
        if !view.dirty && pending.is_none() {
            return None;
        }
        match view.kind {
            crate::sheet::SheetKind::Csv | crate::sheet::SheetKind::Tsv => {
                let data = view.sheets.get(view.current_sheet)?;
                let typed;
                let data = match pending {
                    Some((_, r, c, value)) => {
                        let mut copy = data.clone();
                        copy.set_cell(r, c, value.to_string());
                        typed = copy;
                        &typed
                    }
                    None => data,
                };
                let bytes =
                    crate::sheet::serialize_delimited(data, sheet_delim(view.kind), false, &[]);
                Some((
                    Some(String::from_utf8_lossy(&bytes).into_owned()),
                    Vec::new(),
                ))
            }
            crate::sheet::SheetKind::Xlsx => {
                let mut cells: Vec<_> = view
                    .cell_edits
                    .iter()
                    .filter(|&&(s, r, c)| pending.is_none_or(|p| (p.0, p.1, p.2) != (s, r, c)))
                    .filter_map(|&(s, r, c)| {
                        let data = view.sheets.get(s)?;
                        Some((data.name.clone(), r, c, data.cell(r, c).to_string()))
                    })
                    .collect();
                if let Some((s, r, c, value)) = pending
                    && let Some(data) = view.sheets.get(s)
                {
                    cells.push((data.name.clone(), r, c, value.to_string()));
                }
                (!cells.is_empty()).then_some((None, cells))
            }
            _ => None,
        }
    }

    /// Put a grid's backed-up edits (#1593) over the file just opened in
    /// it, as unsaved edits: a CSV/TSV's text re-read into the grid, an
    /// xlsx's cells set again and queued for its save. Returns the xlsx
    /// cells it could not set (their worksheet or row is gone from the
    /// workbook), which the caller keeps rather than lose. The tab turns
    /// dirty only when something was put back.
    pub fn restore_sheet_edits(
        &mut self,
        text: Option<&str>,
        cells: &[SheetCellEdit],
    ) -> Vec<SheetCellEdit> {
        let Some(view) = self.sheet.as_mut() else {
            return cells.to_vec();
        };
        let mut unapplied = Vec::new();
        match (view.kind, text) {
            (crate::sheet::SheetKind::Csv | crate::sheet::SheetKind::Tsv, Some(text)) => {
                let name = view
                    .sheets
                    .first()
                    .map_or_else(|| String::from("Sheet1"), |d| d.name.clone());
                let Ok(data) =
                    crate::sheet::parse_delimited(text.as_bytes(), sheet_delim(view.kind), &name)
                else {
                    return Vec::new();
                };
                view.sheets = vec![data];
                view.current_sheet = 0;
            }
            (crate::sheet::SheetKind::Xlsx, _) if !cells.is_empty() => {
                for cell in cells {
                    let (name, r, c, value) = cell;
                    let Some(s) = view
                        .sheets
                        .iter()
                        .position(|d| d.name == *name && *r < d.rows.len())
                    else {
                        unapplied.push(cell.clone());
                        continue;
                    };
                    view.sheets[s].set_cell(*r, *c, value.clone());
                    if !view.cell_edits.contains(&(s, *r, *c)) {
                        view.cell_edits.push((s, *r, *c));
                    }
                }
                if unapplied.len() == cells.len() {
                    return unapplied;
                }
            }
            _ => return cells.to_vec(),
        }
        view.dirty = true;
        self.dirty = true;
        unapplied
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

    pub(super) fn render_pdf_page(&mut self, new_page: u32) -> bool {
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
        let Some((pixel_w, pixel_h)) = image_dimensions_of(&bytes) else {
            return false;
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
        let text = self.lines.join("\n");
        self.rebuild_markdown_preview_from(&text, None);
    }

    /// [`Self::rebuild_markdown_preview`] from `text` instead of this tab's
    /// buffer (#1605): the source pane's text, for a preview to the side
    /// that follows it, with `follows` naming that source (`None` when
    /// `text` is the buffer's own).
    pub fn rebuild_markdown_preview_from(&mut self, text: &str, follows: Option<(u64, u64)>) {
        if self.markdown_preview.is_none() {
            return;
        }
        // The image-aware builder (#196 review): the plain one left
        // md.images pointing at anchors into the REPLACED lines, and
        // the wrap-key recompute then sliced with stale first_line
        // values - an out-of-range panic on a theme switch.
        let base = self
            .path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let (lines, images, runnables, source_map, links) = crate::markdown::render_markdown_mapped(
            text,
            self.theme,
            &mut self.registry,
            base.as_deref(),
            self.md_outputs.clone(),
        );
        if let Some(md) = self.markdown_preview.as_mut() {
            md.lines = lines;
            md.source_map = source_map;
            md.images = images;
            md.runnables = runnables;
            md.links = links;
            md.built_seq = self.edit_seq;
            md.follows = follows;
            // The visual-row caches (images, play glyphs, scroll sync) are
            // keyed on `(built_seq, width)`, and a preview following another
            // pane rebuilds with its own `built_seq` unchanged (#1605
            // review): without this they kept the first build's rows, so
            // clicks and scrolling landed on stale lines. `(0, 0)` never
            // matches a drawn width.
            md.wrap_key = (0, 0);
        }
    }

    /// Whether this buffer is a plain `.log` file shown as text (#1537): no
    /// grammar, no rendered view. It gets tailspin's colours, the same ones
    /// the rendered log view paints, and reads "Log" in the status bar.
    pub fn is_plain_log(&self) -> bool {
        self.lang.is_none()
            && !self.has_non_text_view()
            && self
                .path
                .as_deref()
                .and_then(|p| p.extension())
                .is_some_and(|e| e.eq_ignore_ascii_case("log"))
    }

    /// Recolour a plain `.log` tab after "Log: Highlighting (tailspin)"
    /// flips (#1537); every other buffer is unaffected by that setting.
    pub fn refresh_log_highlight(&mut self) {
        if self.is_plain_log() {
            self.recompute_highlights();
        }
    }

    /// Spans for a grammar-less buffer: none, except on a plain `.log`
    /// (#1537), where tailspin colours levels, dates, numbers and ids while
    /// "Log: Highlighting (tailspin)" is on. A log too big to rescan on every
    /// edit stays uncoloured rather than making typing pay for it.
    pub(super) fn plain_log_highlights(&mut self) -> Vec<Vec<HiSpan>> {
        let bytes: usize = self.lines.iter().map(String::len).sum();
        if !self.is_plain_log()
            || !crate::log_view::default_highlight()
            || bytes > LOG_HIGHLIGHT_MAX_BYTES
        {
            self.log_spans.clear();
            return vec![Vec::new(); self.lines.len()];
        }
        let mut seen = std::mem::take(&mut self.log_spans);
        let mut kept = std::collections::HashMap::with_capacity(seen.len());
        let theme = self.theme;
        let out = self
            .lines
            .iter()
            .map(|line| {
                let spans: &Vec<crate::ansi_text::AnsiSpan> =
                    kept.entry(line.clone()).or_insert_with(|| {
                        seen.remove(line)
                            .unwrap_or_else(|| crate::log_view::plain_line_spans(line))
                    });
                spans
                    .iter()
                    .map(|s| HiSpan::new(s.start, s.end, log_span_style(&s.style, theme)))
                    .collect()
            })
            .collect();
        self.log_spans = kept;
        out
    }

    /// Markdown: Toggle Preview (Cmd/Ctrl+Shift+V). Returns false when the
    /// active tab is not a Markdown text buffer (the caller reports why).
    pub fn toggle_markdown_preview(&mut self) -> bool {
        if let Some(md) = self.markdown_preview.take() {
            // Back to the source where the preview was (#619).
            if let Some(line) = md.source_line_at_top() {
                self.scroll = line.min(self.lines.len().saturating_sub(1));
                self.scroll_sub = 0;
            }
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
        let (lines, images, runnables, source_map, links) = crate::markdown::render_markdown_mapped(
            &text,
            self.theme,
            &mut self.registry,
            base.as_deref(),
            self.md_outputs.clone(),
        );
        // Open where the source is scrolled (#619).
        let scroll_to_source = Some(self.scroll);
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
            source_map,
            row_of_line: Vec::new(),
            scroll_to_source,
            links,
            follows: None,
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
        let Some((lines, images, runnables)) = crate::notebook::render(
            &text,
            self.theme,
            &mut self.registry,
            base.as_deref(),
            &scratch,
            &self.notebook_running,
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
            runnables,
            run_rows: Vec::new(),
            wrap_key: (0, 0),
            last_area: Rect::default(),
            notebook: true,
            doc_path: None,
            media: false,
            source_map: Vec::new(),
            row_of_line: Vec::new(),
            scroll_to_source: None,
            links: crate::markdown::MdLinks::default(),
            follows: None,
        });
        true
    }

    /// Rebuild the notebook view (#180) from the buffer, keeping its
    /// scroll. A document that no longer parses drops to the raw text
    /// (#199 review): keeping the stale render would repaint and re-fail
    /// every frame.
    pub(super) fn rebuild_notebook_preview(&mut self) {
        let scroll = self.markdown_preview.as_ref().map_or(0, |m| m.scroll);
        if self.build_notebook_preview() {
            if let Some(md) = self.markdown_preview.as_mut() {
                md.scroll = scroll;
            }
        } else {
            self.markdown_preview = None;
        }
    }

    /// Mark the notebook cells running or queued in its kernel (#355). The
    /// view redraws their `In [*]` marks at once: it otherwise rebuilds only
    /// on an edit, and a run can end without one, as when the kernel never
    /// starts (#878), which left the cells marked running for good.
    pub fn set_notebook_running(&mut self, running: Vec<usize>) {
        if self.notebook_running == running {
            return;
        }
        self.notebook_running = running;
        if self.markdown_preview.as_ref().is_some_and(|md| md.notebook) {
            self.rebuild_notebook_preview();
        }
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

    /// True when this tab is a SQLite browser, whose stamp covers the log.
    pub(super) fn is_sqlite_view(&self) -> bool {
        self.sheet
            .as_ref()
            .is_some_and(|s| s.kind == crate::sheet::SheetKind::Sqlite)
    }

    /// Where the reader is in a SQLite tab, for [`Self::reload_from_disk`]
    /// to put back on the rebuilt view.
    pub(super) fn sqlite_place(&self) -> Option<SqlitePlace> {
        let view = self.sheet.as_ref().filter(|_| self.is_sqlite_view())?;
        let data = view.sheets.get(view.current_sheet)?;
        let (table, page) = view.sqlite_pages.get(view.current_sheet)?.clone();
        Some(SqlitePlace {
            sheet: view.current_sheet,
            table,
            page,
            cell: (data.cur_row, data.cur_col),
            scroll: (data.scroll_row, data.scroll_col),
        })
    }

    /// Put a reloaded SQLite view back where [`Self::sqlite_place`] found
    /// the reader: the same table (found by name, else the same position
    /// clamped), the same page while the table still reaches it (else its
    /// last), and the cell and scroll clamped to what that page now holds.
    pub(super) fn restore_sqlite_place(&mut self, path: &Path, place: SqlitePlace) {
        if !self.is_sqlite_view() {
            return;
        }
        let Some(view) = self.sheet.as_mut() else {
            return;
        };
        let Some(last) = view.sheets.len().checked_sub(1) else {
            return;
        };
        let idx = view
            .sqlite_pages
            .iter()
            .position(|(t, _)| *t == place.table)
            .unwrap_or(place.sheet.min(last));
        view.current_sheet = idx;
        let table = view.sqlite_pages[idx].0.clone();
        let same_table = table == place.table;
        if same_table && place.page > 0 {
            // The page as it was; when the table has shrunk short of it, its
            // last page instead (counting only then, as Ctrl+End does).
            let fetch = |page| crate::sqlite_view::table_page(path, &table, page).ok();
            let found = match fetch(place.page) {
                Some(got) if !got.rows.is_empty() => Some((place.page, got)),
                _ => crate::sqlite_view::last_page(path, &table)
                    .ok()
                    .filter(|&l| l > 0 && l < place.page)
                    .and_then(|l| fetch(l).map(|got| (l, got))),
            };
            if let Some((page, got)) = found {
                view.sheets[idx] = got.into_sheet(&table, page);
                view.sqlite_pages[idx].1 = page;
            }
        }
        if !same_table {
            return;
        }
        let data = &mut view.sheets[idx];
        let rows = data.row_count().saturating_sub(1);
        let cols = data.col_count().saturating_sub(1);
        data.cur_row = place.cell.0.min(rows);
        data.cur_col = place.cell.1.min(cols);
        data.scroll_row = place.scroll.0.min(data.cur_row);
        data.scroll_col = place.scroll.1.min(data.cur_col);
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

    /// Scroll a reloaded log to the place its reload kept (#1538), clamped
    /// to what is indexed so far, and keep the request while the index is
    /// still growing so the place is reached once it covers it.
    pub fn settle_log_reload(&mut self) {
        let Some(top) = self.log_reload_top.take() else {
            return;
        };
        if self.log.is_none() {
            return;
        }
        self.scroll_view_to(top);
        if self.log.as_ref().is_some_and(|log| log.indexing()) {
            self.log_reload_top = Some(top);
        }
    }

    /// Drop the place a log reload is still settling (#1538): the reader
    /// moved the view some other way, such as a find-bar jump, and the next
    /// index batch must not take it back.
    pub fn cancel_log_reload(&mut self) {
        self.log_reload_top = None;
    }
}

/// Above this many bytes a plain `.log` buffer is left uncoloured (#1537):
/// tailspin runs over every line the first time, and the rendered log view,
/// which windows the file, is the place for logs this big.
pub(super) const LOG_HIGHLIGHT_MAX_BYTES: usize = 4_000_000;

/// A tailspin span's style for the text editor (#1537). Foreground and
/// emphasis only: the highlighter never sets a background, and the editor's
/// own background, selection and find marks must show through.
pub(super) fn log_span_style(s: &crate::ansi_text::AnsiStyle, theme: crate::theme::Theme) -> Style {
    let mut style = Style::default();
    if let Some(c) = s.fg {
        style = style.fg(ansi_color_to_tui(c, theme));
    }
    if s.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if s.dim {
        style = style.add_modifier(Modifier::DIM);
    }
    if s.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if s.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

/// The private folder that holds decompressed gzip copies (#1469): one per
/// user, mode 0700 on unix, and refused unless it is a real directory this
/// user owns that no one else can enter, so another local user can neither
/// plant links in it nor read the text written there.
pub(super) fn gunzip_scratch_dir() -> std::io::Result<std::path::PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let dir = std::env::temp_dir().join(format!("croft-gunzip-{uid}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
        let meta = std::fs::symlink_metadata(&dir)?;
        if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
            return Err(std::io::Error::other(format!(
                "{} is not a private directory",
                dir.display()
            )));
        }
        Ok(dir)
    }
    #[cfg(not(unix))]
    {
        let dir = std::env::temp_dir().join("croft-gunzip");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

impl Editor {
    /// Paint the rendered Markdown preview: the pre-built lines flow through
    /// a wrapping `Paragraph` so paragraphs reflow with the pane, scrolled by
    /// the preview's own offset, with a scrollbar over the wrapped height.
    /// Rebuilds first when the buffer moved under the preview (an edit, an
    /// external reload) so it always shows the current text. A preview to
    /// the side shows its source pane's text, which the app hands it
    /// (`App::sync_markdown_side_previews`, #1605).
    pub(super) fn render_markdown_preview(&mut self, inner: Rect, buf: &mut Buffer) {
        let stale = self
            .markdown_preview
            .as_ref()
            .is_some_and(|md| md.built_seq != self.edit_seq);
        let is_nb = self.markdown_preview.as_ref().is_some_and(|md| md.notebook);
        if stale && is_nb {
            // A JSON made invalid by the edit drops to the raw text.
            self.rebuild_notebook_preview();
        } else if stale {
            // The image-aware builder (#176): the plain one here left the
            // images list pointing at STALE anchors after a live edit.
            self.rebuild_markdown_preview();
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
            // Scroll sync (#619): each built line's first visual row. A
            // paragraph wraps line by line, so per-line counts sum exactly.
            let mut row = 0usize;
            md.row_of_line = md
                .lines
                .iter()
                .map(|l| {
                    let here = row;
                    row += Paragraph::new(Text::from(vec![l.clone()]))
                        .wrap(Wrap { trim: false })
                        .line_count(text_area.width)
                        .max(1);
                    here
                })
                .collect();
            md.wrap_key = (md.built_seq, text_area.width);
        }
        if let Some(line) = md.scroll_to_source.take()
            && let Some(row) = md.row_for_source_line(line)
        {
            md.scroll = row.min(usize::from(max_scroll)) as u16;
        }
        md.last_area = text_area;
        // The link under the pointer lifts like a hovered row (#1939), so it
        // reads as clickable before the click. Asked after the frame's
        // scroll, area and wrap are settled, so the hit-test sees this frame.
        match self.pointer.and_then(|(col, row)| md.link_at(col, row)) {
            Some(i) => {
                let bg = crate::widgets::hover::hover_bg(self.theme);
                let (lines, skip) = md.view_with_hovered_link(i, bg, text_area.height);
                Paragraph::new(Text::from(lines))
                    .wrap(Wrap { trim: false })
                    .scroll((skip, 0))
                    .render(text_area, buf);
            }
            None => para.scroll((md.scroll, 0)).render(text_area, buf),
        }
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
}

/// A reader's place in a SQLite tab, carried across a reload (#1643).
pub(super) struct SqlitePlace {
    sheet: usize,
    table: String,
    page: usize,
    cell: (usize, usize),
    scroll: (usize, usize),
}

/// Where SQLite keeps `path`'s write-ahead log, if `path` were a database
/// in WAL mode: the same name with `-wal` after it (#1643).
///
/// SQLite keeps the log beside the real database file, so a database opened
/// through a symlink has its log beside the link's target, not the link.
pub fn sqlite_wal_path(path: &Path) -> Option<PathBuf> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut name = path.file_name()?.to_os_string();
    name.push("-wal");
    Some(path.with_file_name(name))
}
