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
    HiSpan, LangKind, LangRegistry, compute_line_starts, decode_semantic_tokens, highlight_text,
    lang_for_extension,
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

fn render_image_placeholder(
    image: &ImageView,
    path: Option<&Path>,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
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
            .bg(Color::Rgb(0x09, 0x4d, 0x77))
            .add_modifier(Modifier::BOLD),
    );
}

fn render_sheet(
    view: &crate::sheet::SheetView,
    path: Option<&Path>,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
) {
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
        " {} · {} · sheet {}/{} ({}) · {} rows × {} cols ",
        name,
        view.kind.label(),
        view.current_sheet + 1,
        view.sheets.len(),
        sheet.name,
        row_count,
        col_count,
    );
    buf.set_string(
        inner.x,
        inner.y,
        &header,
        Style::default()
            .fg(Color::White)
            .bg(Color::Rgb(0x09, 0x4d, 0x77))
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
        .bg(Color::Rgb(0x07, 0x33, 0x55))
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
        .bg(Color::Rgb(0x24, 0x29, 0x37));
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
            write_cell(buf, body_x + *x_off, y, w, cell_text, style);
        }
    }

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
            .fg(Color::Gray)
            .bg(Color::Rgb(0x14, 0x18, 0x22)),
    );
}

/// Returns the hit-test rects of the prev / next change arrows painted
/// in the diff header (in that order). Both are `Rect::default()` when
/// the header was too narrow to allocate them.
fn render_diff(
    diff: &mut crate::widgets::diff::DiffData,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
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
        return render_unified_deletion(diff, inner, buf, bg);
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
    let head_bg = if diff.bytes_differ_but_lines_equal {
        Color::Rgb(0x8a, 0x4a, 0x10)
    } else {
        Color::Rgb(0x09, 0x4d, 0x77)
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
        buf[(seam_x, y)].set_style(Style::default().fg(Color::Rgb(0x3a, 0x42, 0x52)));
    }

    let total = diff.rows.len();
    let viewport = body_height as usize;
    let max_scroll = total.saturating_sub(viewport);
    if diff.scroll > max_scroll {
        diff.scroll = max_scroll;
    }

    let removed_bg = Color::Rgb(0x4a, 0x1f, 0x1f);
    let removed_fg = Color::Rgb(0xff, 0xb3, 0xb3);
    let added_bg = Color::Rgb(0x1f, 0x42, 0x2a);
    let added_fg = Color::Rgb(0xb6, 0xee, 0xc4);
    let equal_fg = Color::Rgb(0xc5, 0xcd, 0xd9);
    let gutter_fg = Color::Rgb(0x6c, 0x7d, 0x9c);

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
        let row = diff.rows[row_idx];
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
            .fg(Color::Gray)
            .bg(Color::Rgb(0x14, 0x18, 0x22)),
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
        paint_selection_band(buf, text_x, y, text_w, cs_screen, ce_screen);
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
        .bg(Color::Rgb(0x6b, 0x1f, 0x1f))
        .add_modifier(Modifier::BOLD);
    for x in inner.x..inner.x + inner.width {
        buf[(x, inner.y)].set_style(head_style);
        buf[(x, inner.y)].set_symbol(" ");
    }
    buf.set_string(inner.x, inner.y, &header, head_style);
    let (prev_arrow, next_arrow) = paint_diff_nav_arrows(inner, Color::Rgb(0x6b, 0x1f, 0x1f), buf);

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

    let removed_bg = Color::Rgb(0x4a, 0x1f, 0x1f);
    let removed_fg = Color::Rgb(0xff, 0xb3, 0xb3);
    let gutter_fg = Color::Rgb(0x6c, 0x7d, 0x9c);

    let end = (diff.scroll + viewport).min(total);
    for (vis_row, row_idx) in (diff.scroll..end).enumerate() {
        let y = body_top + vis_row as u16;
        let row = diff.rows[row_idx];
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
                    Color::Rgb(0xc5, 0xcd, 0xd9)
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
            .fg(Color::Gray)
            .bg(Color::Rgb(0x14, 0x18, 0x22)),
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
}

/// Per-tab results of an FS-sync sweep across all open tabs
/// (`EditorTabs::reload_externally_changed_tabs`).
#[derive(Debug, Default)]
pub struct ExternalReloadReport {
    /// Paths of clean tabs that were silently reloaded from disk.
    pub reloaded: Vec<PathBuf>,
    /// Paths of dirty tabs flagged as conflicts (not reloaded, not saved).
    pub conflicts: Vec<PathBuf>,
}

impl ExternalReloadReport {
    pub fn is_empty(&self) -> bool {
        self.reloaded.is_empty() && self.conflicts.is_empty()
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

pub struct Editor {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
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
    /// Merge-conflict blocks in the buffer, lazily recomputed whenever
    /// `edit_seq` moves (same pattern as the git-gutter marks).
    conflicts: Vec<crate::merge::ConflictBlock>,
    /// The `edit_seq` `conflicts` was computed at; `u64::MAX` forces the
    /// first scan.
    conflicts_seq: u64,
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
    /// `self.lines.len()` at the moment `folded` was last set. Fold headers are
    /// line indexes, so an insert/delete anywhere shifts them; when the count
    /// no longer matches, the render drops every fold rather than hide the
    /// wrong lines. ponytail: whole-buffer invalidation, not per-fold anchor
    /// tracking; upgrade to sticky anchors if folds-survive-edits is wanted.
    fold_epoch_lines: usize,
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
    /// Line-ending style, detected on open and applied on save. Surfaced in the
    /// status bar; the user can switch it there.
    pub eol: LineEnding,
    /// Text encoding the buffer was decoded from and is re-encoded to on save.
    /// Defaults to UTF-8; the status bar's "Reopen with Encoding" switches it.
    pub encoding: &'static encoding_rs::Encoding,
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
    /// Decoded per-logical-line hint runs `(char_col, label)`, sorted by
    /// column. The render loop splices each label into the row as dim italic
    /// virtual cells at its anchor; every overlay painter, the caret, and
    /// mouse mapping translate buffer columns past them.
    inlay_spans: Vec<Vec<(usize, String)>>,
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
    pub image: Option<ImageView>,
    /// Read-only spreadsheet preview for `.csv` / `.tsv` / `.xlsx` / etc.
    /// Mutually exclusive with `image` and the text path; none of the
    /// editor's text fields are populated when this is `Some`.
    pub sheet: Option<crate::sheet::SheetView>,
    /// Read-only side-by-side diff view. Mutually exclusive with the text
    /// path, `image`, and `sheet` — when set the renderer paints two
    /// columns based on `diff.rows` and ignores `lines`.
    pub diff: Option<crate::widgets::diff::DiffData>,
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
    /// When the buffer last changed, driving the auto-save delay. `None`
    /// until the first edit.
    pub last_edit_at: Option<std::time::Instant>,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            path: None,
            lines: Vec::new(),
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
            conflicts: Vec::new(),
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
            fold_epoch_lines: 0,
            preview: false,
            pinned: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit_kind: None,
            lang: None,
            indent_override: None,
            eol: LineEnding::Lf,
            encoding: encoding_rs::UTF_8,
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
            registry: LangRegistry::new(),
            search_highlight: None,
            search_highlight_opts: crate::widgets::search::SearchOpts::default(),
            active_search_match: None,
            markdown_preview: None,
            image: None,
            sheet: None,
            diff: None,
            diff_prev_arrow: Rect::default(),
            diff_next_arrow: Rect::default(),
            disk_stamp: None,
            disk_conflict: false,
            last_edit_at: None,
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
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.hscroll_content_cols = None;
        self.wrap_total_cache.clear();
        self.occurrences.clear();
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
                    .map(|hs| hs.iter().map(|(_, label)| label.chars().count()).sum())
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
        if extension_is_image(ext) {
            return self.open_image(path);
        }
        if extension_is_pdf(ext) && self.pdf_viewer_enabled {
            return self.open_pdf(path);
        }
        if crate::sheet::extension_is_sheet(ext) && self.csv_viewer_enabled {
            return self.open_sheet(path);
        }
        let meta = std::fs::metadata(path)?;
        if meta.len() > MAX_FILE_BYTES {
            anyhow::bail!("File too large ({} bytes)", meta.len());
        }
        let bytes = std::fs::read(path)?;
        if is_binary(&bytes) {
            anyhow::bail!("Binary file");
        }
        // Decode with the file's BOM-declared encoding if it has one, else
        // UTF-8. A later "Reopen with Encoding" overrides this.
        let changed_file = self.path.as_deref() != Some(path);
        let enc = encoding_rs::Encoding::for_bom(&bytes)
            .map(|(e, _)| e)
            .unwrap_or(encoding_rs::UTF_8);
        self.encoding = enc;
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
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.hscroll_content_cols = None;
        self.wrap_total_cache.clear();
        self.scroll_sub = 0;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        self.lang = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(lang_for_extension);
        self.wrap_override = None;
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
        // A real file supersedes any diff view this editor was showing —
        // without this a restore-then-reload keeps rendering the stale diff.
        self.diff = None;
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
        self.inlay_spans = Vec::new();
        self.recompute_highlights();
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
        self.lines = vec![String::new()];
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
        self.sheet = None;
        self.status = format!("Opened image {}", path.display());
        Ok(())
    }

    fn open_sheet(&mut self, path: &Path) -> Result<()> {
        let view = crate::sheet::open_sheet(path)
            .map_err(|e| anyhow::anyhow!("Spreadsheet open failed: {e}"))?;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        self.lines = vec![String::new()];
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
        self.image = None;
        self.status = format!("Opened {} ({})", path.display(), view.kind.label());
        self.sheet = Some(view);
        Ok(())
    }

    fn open_pdf(&mut self, path: &Path) -> Result<()> {
        let backend = crate::pdf::detect_backend()
            .ok_or_else(|| anyhow::anyhow!("Install poppler (pdftoppm) to preview PDFs"))?;
        let meta = std::fs::metadata(path)?;
        let page_count = crate::pdf::detect_page_count(path);
        let bytes = crate::pdf::rasterize_page(path, 1, backend)
            .map_err(|e| anyhow::anyhow!("PDF render failed: {e}"))?;
        let (pixel_w, pixel_h) = image::load_from_memory(&bytes)
            .map(|img| (img.width(), img.height()))
            .map_err(|e| anyhow::anyhow!("Could not decode rasterised PDF: {e}"))?;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        self.lines = vec![String::new()];
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
                current_page: 1,
                page_count,
                backend,
                source_byte_size: meta.len(),
                links: None,
            }),
        });
        self.sheet = None;
        self.status = format!("Opened PDF {}", path.display());
        Ok(())
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
        if self.markdown_preview.is_some() {
            let text = self.lines.join("\n");
            let lines = crate::markdown::render_markdown(&text, self.theme, &mut self.registry);
            if let Some(md) = self.markdown_preview.as_mut() {
                md.lines = lines;
                md.built_seq = self.edit_seq;
            }
        }
    }

    fn recompute_highlights(&mut self) {
        match self.lang {
            Some(kind) => {
                let text = self.lines.join("\n");
                let bytes = text.as_bytes();
                let line_starts = compute_line_starts(bytes);
                self.highlights = highlight_text(&mut self.registry, kind, bytes, &line_starts);
            }
            None => {
                self.highlights = vec![Vec::new(); self.lines.len()];
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
        if !same_file || self.inlay_hints.is_empty() {
            self.inlay_spans = Vec::new();
            return;
        }
        let mut spans: Vec<Vec<(usize, String)>> = vec![Vec::new(); self.lines.len()];
        for h in &self.inlay_hints {
            let Some(text) = self.lines.get(h.line as usize) else {
                continue;
            };
            let col = utf16_to_char_col(text, h.character).min(text.chars().count());
            spans[h.line as usize].push((col, h.label.clone()));
        }
        for line in &mut spans {
            line.sort_by_key(|(c, _)| *c);
        }
        self.inlay_spans = spans;
    }

    /// Inlay hints of logical line `line`, or nothing in wrap mode: the
    /// wrapped row segmentation knows nothing about hint cells.
    /// ponytail: hints skip wrap mode; code files don't wrap by default, and
    /// Markdown (the wrapping default) has no hint-serving server.
    fn row_inlay_spans(&self, line: usize) -> &[(usize, String)] {
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
        let is_md_text = matches!(self.lang, Some(LangKind::Markdown))
            && self.image.is_none()
            && self.sheet.is_none()
            && self.diff.is_none();
        if !is_md_text {
            return false;
        }
        let text = self.lines.join("\n");
        let lines = crate::markdown::render_markdown(&text, self.theme, &mut self.registry);
        self.markdown_preview = Some(crate::markdown::MarkdownPreview {
            lines,
            scroll: 0,
            built_seq: self.edit_seq,
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
    }

    pub fn insert_char(&mut self, c: char) {
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

    /// The buffer's active indentation style: the status-bar override if set,
    /// else the language default (2 spaces for YAML, 4 spaces otherwise).
    pub fn indent_style(&self) -> IndentStyle {
        self.indent_override.unwrap_or(IndentStyle {
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
        let text = enc.decode(&bytes).0.into_owned();
        self.encoding = enc;
        self.eol = if text.contains("\r\n") {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        };
        self.lines = split_into_lines(&text);
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
        self.pin_on_edit();
        self.push_undo(EditKind::Paste);
        if self.selection.is_some() {
            self.delete_selection_inner();
        }
        for c in s.chars() {
            if c == '\n' {
                self.insert_newline_raw();
            } else {
                self.insert_char_raw(c);
            }
        }
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
        self.insert_str(&inserted);

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

    pub fn clear_selection(&mut self) {
        self.selection = None;
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

    /// Whether `line` is hidden inside a collapsed region. Testing every folded
    /// header is enough even with nesting: an outer fold's range covers any
    /// inner fold's range, so a line hidden by the inner is also covered by the
    /// outer. ponytail: O(folds) per query, fine for realistic fold counts.
    pub fn is_line_hidden(&self, line: usize) -> bool {
        self.folded
            .iter()
            .any(|&h| matches!(self.fold_range(h), Some((_, end)) if h < line && line <= end))
    }

    /// Toggle the fold owning `line` (VS Code `editor.toggleFold`). Collapsing a
    /// block that contains the cursor snaps the cursor up to the header so it
    /// never strands on a hidden line.
    pub fn toggle_fold(&mut self, line: usize) {
        let Some(header) = self.enclosing_fold_header(line) else {
            return;
        };
        if !self.folded.remove(&header) {
            self.folded.insert(header);
            if self.is_line_hidden(self.cursor_row) {
                let len = self
                    .lines
                    .get(header)
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
                self.cursor_row = header;
                self.cursor_col = self.cursor_col.min(len);
            }
        }
        self.fold_epoch_lines = self.lines.len();
    }

    /// Collapse every foldable region (VS Code "Fold All", Cmd+K Cmd+0),
    /// snapping the cursor up to the nearest visible line.
    pub fn fold_all(&mut self) {
        self.folded = (0..self.lines.len())
            .filter(|&l| self.is_foldable(l))
            .collect();
        self.fold_epoch_lines = self.lines.len();
        while self.cursor_row > 0 && self.is_line_hidden(self.cursor_row) {
            self.cursor_row -= 1;
        }
    }

    /// Expand every fold (VS Code "Unfold All", Cmd+K Cmd+J).
    pub fn unfold_all(&mut self) {
        self.folded.clear();
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            selection: self.selection,
            carets: self.carets.clone(),
            dirty: self.dirty,
        }
    }

    /// Push an undo entry tagged with the kind of edit about to happen.
    /// Coalesces consecutive `InsertChar` ops into one step so a typing
    /// burst is undone as one unit; everything else opens a new step.
    fn push_undo(&mut self, kind: EditKind) {
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
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = snap.cursor_row.min(self.lines.len().saturating_sub(1));
        self.cursor_col = snap.cursor_col.min(self.line_char_len(self.cursor_row));
        self.selection = snap.selection;
        self.carets = snap.carets;
        self.dirty = snap.dirty;
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
        let prev_pdf_page = self.pdf_page();
        let result = self.open(&path);
        // Clamp the restored cursor to the new contents so it stays valid
        // even if the file shrank.
        self.cursor_row = prev_row.min(self.lines.len().saturating_sub(1));
        self.cursor_col = prev_col.min(self.line_char_len(self.cursor_row));
        self.scroll = prev_scroll.min(self.lines.len().saturating_sub(1));
        // A PDF's page is its cursor: a rebuild on disk (pdflatex finishing)
        // must not snap the reader back to page 1. `set_pdf_page` clamps to
        // the new page count if the document shrank.
        if let Some(page) = prev_pdf_page.filter(|&p| p > 1) {
            self.set_pdf_page(page);
        }
        result
    }

    /// Discard local edits and reload from disk unconditionally — the
    /// "Revert" half of a conflict resolution.
    pub fn revert_to_disk(&mut self) -> Result<()> {
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
        self.write_buffer_to_disk()?;
        Ok(SaveOutcome::Saved)
    }

    /// Save unconditionally, overwriting any external change. The "Overwrite"
    /// half of a conflict resolution.
    pub fn save_to_disk_force(&mut self) -> Result<()> {
        self.write_buffer_to_disk()
    }

    fn write_buffer_to_disk(&mut self) -> Result<()> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No file open"))?
            .clone();
        let content = self.lines.join(self.eol.sequence());
        // Re-encode to the buffer's encoding (UTF-8 is identity). Unmappable
        // characters are replaced per encoding_rs, matching VS Code.
        let (encoded, _, _) = self.encoding.encode(&content);
        std::fs::write(&path, encoded)?;
        self.dirty = false;
        self.status = format!("Saved {}", path.display());
        // The buffer now matches disk, so this is the new sync point and any
        // prior conflict is resolved.
        self.mark_synced_with_disk();
        Ok(())
    }

    pub fn move_up(&mut self) {
        if self.wrap_enabled() {
            self.wrap_move_vertical(-1);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        }
        self.last_edit_kind = None;
    }

    pub fn move_down(&mut self) {
        if self.wrap_enabled() {
            self.wrap_move_vertical(1);
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
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
        self.lines[row].replace_range(from..to, text);
        self.cursor_row = row;
        self.cursor_col = col_chars + text.chars().count();
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
        if self.cursor_row < self.scroll {
            self.cursor_row = self.scroll;
        } else if self.cursor_row > last_visible {
            self.cursor_row = last_visible;
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    fn scroll_view_to(&mut self, top: usize) {
        let viewport = self.last_inner.height as usize;
        if viewport == 0 || self.lines.is_empty() {
            self.scroll = 0;
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.last_edit_kind = None;
            return;
        }
        self.scroll = top.min(self.lines.len().saturating_sub(viewport));
        let last_visible = (self.scroll + viewport - 1).min(self.lines.len().saturating_sub(1));
        if self.cursor_row < self.scroll {
            self.cursor_row = self.scroll;
        } else if self.cursor_row > last_visible {
            self.cursor_row = last_visible;
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
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
                .filter(|(hc, _)| *hc >= self.scroll_col && *hc <= c)
                .map(|(_, l)| l.chars().count())
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
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    normalized.lines().map(|s| s.to_string()).collect()
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
            Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style);
        let inner = block.inner(area);
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

        let height = inner.height as usize;
        if height == 0 {
            return;
        }
        let cbg = canvas_bg(
            crate::iterm2_inline::detect_iterm2_inline_support(),
            self.theme,
        );
        if let Some(image) = self.image.as_ref() {
            render_image_placeholder(image, self.path.as_deref(), inner, buf, cbg);
            return;
        }
        if let Some(view) = self.sheet.as_ref() {
            render_sheet(view, self.path.as_deref(), inner, buf, cbg);
            return;
        }
        if self.markdown_preview.is_some() {
            self.render_markdown_preview(inner, buf);
            return;
        }
        if let Some(diff) = self.diff.as_mut() {
            let (prev_arrow, next_arrow) = render_diff(diff, inner, buf, cbg);
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
        // Rescan merge-conflict blocks on the same cadence so the region
        // tints below always match the buffer.
        self.conflicts();
        // Fold headers are line indexes; an insert/delete anywhere shifts them.
        // If the line count changed since folds were set, drop them wholesale
        // rather than hide the wrong lines (see `fold_epoch_lines`).
        if !self.folded.is_empty() && self.lines.len() != self.fold_epoch_lines {
            self.folded.clear();
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
                        Style::default().fg(Color::Rgb(0xff, 0xcc, 0x00)),
                    );
                    sign_taken = true;
                } else if is_bp {
                    // Hollow dimmed ring when the adapter could not bind it; a
                    // red diamond for a conditional breakpoint; an amber one
                    // for a logpoint; a solid red dot for a plain, live one.
                    let (glyph, color) = if is_unverified {
                        ("○", Color::Rgb(0x99, 0x99, 0x99))
                    } else if is_conditional {
                        ("◆", Color::Rgb(0xe5, 0x1c, 0x23))
                    } else if is_logpoint {
                        ("◆", Color::Rgb(0xe5, 0xc0, 0x7b))
                    } else {
                        ("●", Color::Rgb(0xe5, 0x1c, 0x23))
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
                    Style::default().fg(Color::Rgb(0xff, 0x9d, 0x2f)),
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
                    Style::default().fg(Color::Rgb(0x73, 0xc9, 0x91)),
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
                    ("▸", Color::Gray)
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
                .filter(|(hc, _)| *hc >= row_start && *hc <= hint_cap)
                .map(|(hc, l)| (*hc, l.chars().count()))
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
                for (hcol, label) in self
                    .row_inlay_spans(line_idx)
                    .iter()
                    .filter(|(hc, _)| *hc >= row_start && *hc <= hint_cap)
                {
                    if *hcol > from {
                        out.extend(inlay_text_segment(raw, &merged, from, *hcol));
                    }
                    out.push(Span::styled(label.clone(), hint_style));
                    from = from.max(*hcol);
                }
                if row_end > from {
                    out.extend(inlay_text_segment(raw, &merged, from, row_end));
                }
                buf.set_line(text_x, y, &Line::from(out), row_width);
            }

            // Merge-conflict region tints (VS Code's current/incoming
            // backgrounds), painted right after the text so diagnostics,
            // search highlights, and the selection all win over them.
            if let Some(tint) = conflict_row_tint(&self.conflicts, line_idx) {
                paint_full_row_bg(buf, text_x, y, row_width, tint);
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
                    paint_diagnostic_underline(buf, text_x, y, row_width, vs, ve, severity);
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
                    paint_selection_band(buf, text_x, y, row_width, visible_start, visible_end);
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
                        paint_selection_band(buf, text_x, y, row_width, vs, ve);
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
                    paint_block_cursor(buf, text_x, y, row_width, c + ex_caret(c), row_start);
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
                .filter(|(hc, _)| *hc >= caret_start && *hc <= hint_cap)
                .map(|(hc, l)| (*hc, l.chars().count()))
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
            let max_rows = (inner.height - 1) as usize;
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
        if stale {
            let text = self.lines.join("\n");
            let lines = crate::markdown::render_markdown(&text, self.theme, &mut self.registry);
            if let Some(md) = self.markdown_preview.as_mut() {
                md.lines = lines;
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
        para.scroll((md.scroll, 0)).render(text_area, buf);
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
            let (_, start, end) = self.text_row(idx)?;
            let visible_col = self.cursor_col - start;
            if end == start || visible_col >= end - start {
                return None;
            }
            return Some((text_x + visible_col as u16, self.last_inner.y + idx as u16));
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
            .filter(|(hc, _)| *hc >= scroll_col && *hc < self.cursor_col)
            .map(|(_, l)| l.chars().count())
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
) {
    if needle.is_empty() {
        return;
    }
    let inactive_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Rgb(0xff, 0xd7, 0x4a))
        .add_modifier(Modifier::BOLD);
    let active_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Rgb(0xff, 0x8c, 0x2a))
        .add_modifier(Modifier::BOLD);
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
) {
    if needle.is_empty() {
        return;
    }
    let bg = Color::Rgb(0x37, 0x61, 0x8e);
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
            .bg(Color::Rgb(0xae, 0xc6, 0xff))
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
fn conflict_row_tint(blocks: &[crate::merge::ConflictBlock], row: usize) -> Option<Color> {
    let block = blocks.iter().find(|b| b.contains(row))?;
    let ours_end = block.base_start.unwrap_or(block.sep);
    Some(if row == block.ours_start {
        Color::Rgb(0x2a, 0x4f, 0x33)
    } else if row < ours_end {
        Color::Rgb(0x1b, 0x33, 0x22)
    } else if row < block.sep {
        Color::Rgb(0x2a, 0x2a, 0x2a)
    } else if row == block.sep {
        Color::Rgb(0x28, 0x28, 0x28)
    } else if row == block.theirs_end {
        Color::Rgb(0x1f, 0x41, 0x66)
    } else {
        Color::Rgb(0x16, 0x2b, 0x44)
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
) {
    let bg = Color::Rgb(0x26, 0x4f, 0x78);
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
fn paint_diagnostic_underline(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    start_char: usize,
    end_char: usize,
    severity: crate::lsp::manager::DiagnosticSeverity,
) {
    use crate::lsp::manager::DiagnosticSeverity;
    let color = match severity {
        DiagnosticSeverity::Error => Color::Rgb(0xf1, 0x4c, 0x4c),
        DiagnosticSeverity::Warning => Color::Rgb(0xcc, 0xa7, 0x00),
        DiagnosticSeverity::Information | DiagnosticSeverity::Hint => Color::Rgb(0x3b, 0x9e, 0xff),
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
                ExternalChange::Unchanged | ExternalChange::ReloadFailed => {}
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
        // Park the viewport on the first change hunk so the user lands on
        // the first edit instead of reading through unchanged leading
        // lines. Identical files stay at scroll 0.
        if let Some(row) = data.first_change_row() {
            data.scroll_to_row(row);
        }
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
        let data =
            crate::widgets::diff::DiffData::build_unified_deletion(path.to_path_buf(), head_text);
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
        let data = crate::widgets::diff::DiffData::build_side_by_side_from_git_text(
            label.to_path_buf(),
            raw_diff,
        );
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
        for (i, ed) in self.editors.iter().enumerate() {
            let label_text = tab_label(ed);
            let label_chars = label_text.chars().count() as u16;
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
                let shown: String = crumb.label.chars().take(avail).collect();
                let sw = shown.chars().count() as u16;
                // Symbol crumbs (clickable) read brighter than the informational
                // path crumbs, mirroring VS Code's active/inactive breadcrumbs.
                let fg = if crumb.target.is_some() {
                    Color::Gray
                } else {
                    Color::DarkGray
                };
                buf.set_string(x, crumb_y, &shown, Style::default().fg(fg).bg(bg));
                self.breadcrumb_ranges.push((x, sw, crumb.target));
                x += sw;
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
        tabs.open_head_diff_with_text(PathBuf::from("a.txt (local snapshot)"), "old\n", &path)
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
    fn is_binary_detects_nul() {
        assert!(is_binary(b"hello\0world"));
        assert!(!is_binary(b"hello world"));
        assert!(!is_binary(b""));
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
    fn open_rejects_binary_files() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"\x00\x01\x02binary garbage").unwrap();
        let mut e = Editor::new();
        let err = e.open(tmp.path()).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("binary"));
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
            "an external rebuild must not move the reader back to page 1"
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
        );
        assert_eq!(
            buf[(1, area.height - 1)].bg,
            themed.editor_bg(),
            "the canvas body must wear the theme bg, not the host default"
        );
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
}
