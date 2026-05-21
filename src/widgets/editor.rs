use anyhow::{Context, Result};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

use crate::highlight::{
    compute_line_starts, highlight_text, lang_for_extension, HiSpan, LangKind, LangRegistry,
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
const IMAGE_EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "gif", "bmp", "webp"];

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
    /// Set when this preview was rasterised from a PDF page; tracks the
    /// page-navigation state so re-renders on Page Down/Up know which
    /// page to ask the rasteriser for next.
    pub pdf: Option<PdfState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfState {
    pub source_path: PathBuf,
    pub current_page: u32,
    pub page_count: Option<u32>,
    pub backend: crate::pdf::PdfBackend,
    pub source_byte_size: u64,
}

fn render_image_placeholder(
    image: &ImageView,
    path: Option<&Path>,
    inner: Rect,
    buf: &mut Buffer,
) {
    // Solid bg fill so the OSC-1337 inline image (emitted post-frame on
    // capable terminals) sits on a clean canvas; on non-capable terminals
    // the metadata header below is the only content the user sees.
    let bg_style = Style::default().bg(Color::Rgb(0x1e, 0x22, 0x2e));
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
) {
    // Bg fill so the spreadsheet sits on a clean canvas regardless of
    // what the previous tab left behind.
    let bg_style = Style::default().bg(Color::Rgb(0x1e, 0x22, 0x2e));
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
    for (c, w) in sheet
        .col_widths
        .iter()
        .enumerate()
        .skip(sheet.scroll_col)
    {
        if x_off + w + 1 > body_w {
            break;
        }
        visible.push((c, x_off));
        x_off += w + 1; // +1 for inter-column gap
    }

    // Header text.
    for (c, x_off) in &visible {
        let label = sheet
            .headers
            .get(*c)
            .map(|s| s.as_str())
            .unwrap_or("");
        let cell_x = body_x + *x_off;
        let w = sheet.col_widths[*c];
        write_cell(buf, cell_x, header_y, w, label, head_style);
    }

    // Data rows.
    let row_end = (sheet.scroll_row + data_rows).min(row_count);
    let row_style = Style::default()
        .fg(Color::White)
        .bg(Color::Rgb(0x1e, 0x22, 0x2e));
    let alt_row_style = Style::default()
        .fg(Color::White)
        .bg(Color::Rgb(0x24, 0x29, 0x37));
    let gutter_style = Style::default().fg(Color::DarkGray);
    for (display_row, row_idx) in (sheet.scroll_row..row_end).enumerate() {
        let y = data_top + display_row as u16;
        let style = if display_row % 2 == 0 { row_style } else { alt_row_style };
        for x in inner.x..inner.x + inner.width {
            buf[(x, y)].set_style(style);
            buf[(x, y)].set_symbol(" ");
        }
        let row_label = format!(" {:>width$} ", row_idx + 1, width = (gutter_w - 2) as usize);
        buf.set_string(inner.x, y, &row_label, gutter_style.bg(style.bg.unwrap_or(Color::Reset)));
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
) -> (Rect, Rect) {
    use crate::widgets::diff::DiffRow;
    // Background fill so the diff sits on a clean canvas.
    let bg_style = Style::default().bg(Color::Rgb(0x1e, 0x22, 0x2e));
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
        return render_unified_deletion(diff, inner, buf);
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

    let end = (diff.scroll + viewport).min(total);
    for (vis_row, row_idx) in (diff.scroll..end).enumerate() {
        let y = body_top + vis_row as u16;
        let row = diff.rows[row_idx];
        let (l_cell_bg, l_sign, l_text) = match row {
            DiffRow::Equal { left, .. } => {
                (Color::Reset, ' ', diff.left_lines.get(left).cloned().unwrap_or_default())
            }
            DiffRow::Removed { left } => {
                (removed_bg, '-', diff.left_lines.get(left).cloned().unwrap_or_default())
            }
            DiffRow::Replaced { left, .. } => {
                (removed_bg, '-', diff.left_lines.get(left).cloned().unwrap_or_default())
            }
            DiffRow::Added { .. } => (added_bg, ' ', String::new()),
        };
        let (r_cell_bg, r_sign, r_text) = match row {
            DiffRow::Equal { right, .. } => (
                Color::Reset,
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
            &format!("{l_sign} "),
            Style::default()
                .fg(if l_cell_bg == removed_bg { removed_fg } else { equal_fg })
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
                .fg(if l_cell_bg == removed_bg { removed_fg } else { equal_fg })
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
            &format!("{r_sign} "),
            Style::default()
                .fg(if r_cell_bg == added_bg { added_fg } else { equal_fg })
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
                .fg(if r_cell_bg == added_bg { added_fg } else { equal_fg })
                .bg(r_cell_bg),
        );
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
        Rect { x: prev_x, y, width: 1, height: 1 },
        Rect { x: next_x, y, width: 1, height: 1 },
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
    let (prev_arrow, next_arrow) =
        paint_diff_nav_arrows(inner, Color::Rgb(0x6b, 0x1f, 0x1f), buf);

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
            DiffRow::Equal { left, .. } => (Some(left), ' ', Color::Reset),
            DiffRow::Added { .. } => (None, ' ', Color::Reset),
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
            &format!("{sign} "),
            Style::default()
                .fg(if cell_bg == removed_bg { removed_fg } else { gutter_fg })
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
                .fg(if cell_bg == removed_bg { removed_fg } else { Color::Rgb(0xc5, 0xcd, 0xd9) })
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

fn write_cell(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    w: u16,
    text: &str,
    style: Style,
) {
    let max_chars = w as usize;
    let mut content: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars && max_chars >= 1 {
        // Replace the last visible char with an ellipsis to make
        // truncation visually obvious.
        content.pop();
        content.push('…');
    }
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
        Self { anchor: (row, col), head: (row, col) }
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
}

#[derive(Clone, Debug)]
struct Snapshot {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    selection: Option<EditorSelection>,
    dirty: bool,
}

const UNDO_STACK_LIMIT: usize = 500;

pub struct Editor {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
    /// Monotonic counter that bumps on every buffer mutation. The App's
    /// per-tick sync_lsp diff reads this to know when to forward a
    /// did_change to the LSP server, so building lines.join("\n") only
    /// happens on actual changes, not every frame.
    pub edit_seq: u64,
    pub scroll: usize,
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
    pub dirty: bool,
    pub status: String,
    pub last_area: Rect,
    pub last_inner: Rect,
    pub last_scrollbar: Rect,
    pub last_gutter_width: u16,
    pub selection: Option<EditorSelection>,
    /// True when this tab is the single replaceable "preview" slot. Single-
    /// click / plain-Enter opens replace the preview tab's contents in place;
    /// double-click / Ctrl+Enter / typing into the buffer pin the tab
    /// (preview = false) so subsequent previews don't overwrite it.
    pub preview: bool,
    undo_stack: Vec<Snapshot>,
    last_edit_kind: Option<EditKind>,
    lang: Option<LangKind>,
    highlights: Vec<Vec<HiSpan>>,
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
}

impl Editor {
    pub fn new() -> Self {
        Self {
            path: None,
            lines: Vec::new(),
            edit_seq: 0,
            scroll: 0,
            scroll_col: 0,
            cursor_row: 0,
            cursor_col: 0,
            focused: false,
            dirty: false,
            status: String::from("No file open"),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            last_scrollbar: Rect::default(),
            last_gutter_width: 0,
            selection: None,
            preview: false,
            undo_stack: Vec::new(),
            last_edit_kind: None,
            lang: None,
            highlights: Vec::new(),
            registry: LangRegistry::new(),
            search_highlight: None,
            search_highlight_opts: crate::widgets::search::SearchOpts::default(),
            active_search_match: None,
            image: None,
            sheet: None,
            diff: None,
            diff_prev_arrow: Rect::default(),
            diff_next_arrow: Rect::default(),
        }
    }

    pub fn set_search_highlight(&mut self, term: Option<String>) {
        self.search_highlight = term.filter(|s| !s.is_empty());
    }

    fn mark_buffer_changed(&mut self) {
        self.dirty = true;
        self.edit_seq = self.edit_seq.wrapping_add(1);
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
        if extension_is_pdf(ext) {
            return self.open_pdf(path);
        }
        if crate::sheet::extension_is_sheet(ext) {
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
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.lines = text.lines().map(|s| s.to_string()).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.path = Some(path.to_path_buf());
        self.lang = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(lang_for_extension);
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.last_edit_kind = None;
        self.image = None;
        self.sheet = None;
        self.status = format!("Opened {}", path.display());
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
        self.lines = vec![String::new()];
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.image = Some(ImageView {
            bytes,
            format_label,
            pixel_w,
            pixel_h,
            byte_size: meta.len(),
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
        self.lines = vec![String::new()];
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
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
        self.lines = vec![String::new()];
        self.lang = None;
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.last_edit_kind = None;
        self.highlights = vec![Vec::new()];
        self.image = Some(ImageView {
            bytes,
            format_label: String::from("PDF"),
            pixel_w,
            pixel_h,
            byte_size: meta.len(),
            pdf: Some(PdfState {
                source_path: path.to_path_buf(),
                current_page: 1,
                page_count,
                backend,
                source_byte_size: meta.len(),
            }),
        });
        self.sheet = None;
        self.status = format!("Opened PDF {}", path.display());
        Ok(())
    }

    /// Re-rasterise the active PDF preview at a new page. Returns true if
    /// the page actually changed, so the caller can flag the OSC overlay
    /// for re-bake. Wraps around at the document boundaries when the page
    /// count is known; clamps at page 1 below otherwise.
    pub fn change_pdf_page(&mut self, delta: i32) -> bool {
        let Some(image) = self.image.as_mut() else {
            return false;
        };
        let Some(pdf) = image.pdf.clone() else {
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
        if new_page == pdf.current_page {
            return false;
        }
        let bytes =
            match crate::pdf::rasterize_page(&pdf.source_path, new_page, pdf.backend) {
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
        image.pixel_w = pixel_w;
        image.pixel_h = pixel_h;
        if let Some(state) = image.pdf.as_mut() {
            state.current_page = new_page;
        }
        true
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
        let had_selection = self
            .selection
            .map(|s| s.has_area())
            .unwrap_or(false);
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
        let last_non_ws = prefix_chars.iter().rev().find(|c| !c.is_whitespace()).copied();
        let next_char = line.chars().nth(col);

        let unit = indent_unit_for(self.lang);
        let extra = if extra_indent_triggered(self.lang, last_non_ws) {
            unit
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
        if self.delete_selection_inner() {
            self.recompute_highlights();
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
        self.recompute_highlights();
    }

    pub fn delete_forward(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::DeleteForward);
        if self.delete_selection_inner() {
            self.recompute_highlights();
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
        self.recompute_highlights();
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
        let Some(sel) = self.selection else { return String::new() };
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

    /// Delete the current selection if it has area.  Returns true iff content
    /// was removed.  Cursor lands at the start of the deleted range and the
    /// selection is cleared.  Pushes an undo step.
    pub fn delete_selection(&mut self) -> bool {
        if !self
            .selection
            .map(|s| s.has_area())
            .unwrap_or(false)
        {
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
        let Some(sel) = self.selection else { return false };
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

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            selection: self.selection,
            dirty: self.dirty,
        }
    }

    /// Push an undo entry tagged with the kind of edit about to happen.
    /// Coalesces consecutive `InsertChar` ops into one step so a typing
    /// burst is undone as one unit; everything else opens a new step.
    fn push_undo(&mut self, kind: EditKind) {
        let coalesce = kind == EditKind::InsertChar
            && self.last_edit_kind == Some(EditKind::InsertChar);
        if !coalesce {
            self.undo_stack.push(self.snapshot());
            if self.undo_stack.len() > UNDO_STACK_LIMIT {
                self.undo_stack.remove(0);
            }
        }
        self.last_edit_kind = Some(kind);
    }

    /// Undo the most recent edit step. Returns true iff state was changed.
    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo_stack.pop() else { return false };
        self.lines = snap.lines;
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = snap
            .cursor_row
            .min(self.lines.len().saturating_sub(1));
        self.cursor_col = snap
            .cursor_col
            .min(self.line_char_len(self.cursor_row));
        self.selection = snap.selection;
        self.dirty = snap.dirty;
        self.last_edit_kind = None;
        self.recompute_highlights();
        true
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

    /// Reload from disk *only if* there are no unsaved local edits. Returns
    /// `Some(Ok(()))` if a reload happened, `Some(Err(_))` if reload failed,
    /// `None` if reload was skipped because the buffer is dirty (caller
    /// should surface a "file changed on disk" warning instead).
    pub fn reload_if_clean(&mut self) -> Option<Result<()>> {
        if self.dirty {
            return None;
        }
        let path = self.path.as_ref().cloned()?;
        let prev_row = self.cursor_row;
        let prev_col = self.cursor_col;
        let prev_scroll = self.scroll;
        let result = self.open(&path);
        // Clamp the restored cursor to the new contents so it stays valid
        // even if the file shrank.
        self.cursor_row = prev_row.min(self.lines.len().saturating_sub(1));
        self.cursor_col = prev_col.min(self.line_char_len(self.cursor_row));
        self.scroll = prev_scroll.min(self.lines.len().saturating_sub(1));
        Some(result)
    }

    pub fn save_to_disk(&mut self) -> Result<()> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No file open"))?
            .clone();
        let content = self.lines.join("\n");
        std::fs::write(&path, content)?;
        self.dirty = false;
        self.status = format!("Saved {}", path.display());
        Ok(())
    }

    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        }
        self.last_edit_kind = None;
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
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
        let width = self.visible_text_width();
        if width == 0 {
            return;
        }
        if self.cursor_col < self.scroll_col {
            self.scroll_col = self.cursor_col;
        } else if self.cursor_col >= self.scroll_col + width {
            self.scroll_col = self.cursor_col + 1 - width;
        }
    }

    /// One screen worth of rows, derived from the editor's last rendered
    /// inner height.  Falls back to a sensible default before the first
    /// render (when `last_inner.height` is still 0).
    pub fn page_size(&self) -> usize {
        let from_inner = self.last_inner.height as usize;
        if from_inner > 0 {
            from_inner
        } else {
            20
        }
    }

    /// Move the viewport down by exactly one screen so the first
    /// previously-unseen row becomes the new top of the viewport, and place
    /// the cursor on that new top row.  Clamps at end of file.
    pub fn page_down_one_screen(&mut self) {
        let page = self.page_size();
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

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_view_to(self.scroll.saturating_sub(n));
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_view_to(self.scroll.saturating_add(n));
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.lines.len(),
            self.last_inner.height as usize,
            self.scroll,
        ) else {
            return false;
        };
        self.scroll_view_to(scrollbar::scroll_for_y(metrics, y));
        true
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
        let target_line = (self.scroll + row_idx).min(self.lines.len().saturating_sub(1));
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        let target_col = if col < text_x {
            0
        } else {
            (col - text_x) as usize
        };
        self.cursor_row = target_line;
        self.cursor_col = target_col.min(self.line_char_len(target_line));
        self.last_edit_kind = None;
    }
}

/// Convert a char index within `s` to a byte index, saturating at `s.len()`.
fn char_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(i, _)| i).unwrap_or(s.len())
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn indent_unit_for(lang: Option<LangKind>) -> &'static str {
    match lang {
        Some(LangKind::Yaml) => "  ",
        _ => "    ",
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

fn is_bracket_pair_split(
    lang: Option<LangKind>,
    prev: Option<char>,
    next: Option<char>,
) -> bool {
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

/// Shift highlight spans left by `byte_start`, dropping spans that fall
/// entirely before the cut and clamping spans straddling the cut.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

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

    #[test]
    fn is_binary_detects_nul() {
        assert!(is_binary(b"hello\0world"));
        assert!(!is_binary(b"hello world"));
        assert!(!is_binary(b""));
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
        let line = "1778548312.915 lsp[ruff] stderr: 2026-05-12 02:11:52 INFO some workspace setting\n";
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
        assert_eq!(e.lines, vec!["def hello():".to_string(), "    ".to_string()]);
        assert_eq!(e.cursor_row, 1);
        assert_eq!(e.cursor_col, 4);
    }

    #[test]
    fn insert_newline_python_no_extra_indent_without_colon() {
        let mut e = editor_with("    print(x)");
        e.lang = Some(LangKind::Python);
        e.cursor_col = e.line_char_len(0);
        e.insert_newline();
        assert_eq!(e.lines, vec!["    print(x)".to_string(), "    ".to_string()]);
        assert_eq!(e.cursor_col, 4);
    }

    #[test]
    fn insert_newline_python_stacks_indent_on_nested_colon() {
        let mut e = editor_with("    if x:");
        e.lang = Some(LangKind::Python);
        e.cursor_col = e.line_char_len(0);
        e.insert_newline();
        assert_eq!(e.lines, vec!["    if x:".to_string(), "        ".to_string()]);
        assert_eq!(e.cursor_col, 8);
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
            vec!["fn main() {".to_string(), "    ".to_string(), "}".to_string()]
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
    fn page_down_advances_one_full_viewport_and_puts_first_unseen_line_at_top() {
        // Simulate a 100-line file with the editor's viewport rendering 25
        // lines. After PageDown the cursor should land on row 25 (line 26 in
        // 1-indexed terms) and that row should be the new top of the view.
        let mut e = editor_with_lines(100);
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        assert_eq!(e.scroll, 0);
        assert_eq!(e.cursor_row, 0);
        e.page_down_one_screen();
        assert_eq!(e.cursor_row, 25, "cursor should jump to first previously-unseen row");
        assert_eq!(e.scroll, 25, "scroll should align with new cursor at top of viewport");
    }

    #[test]
    fn page_down_repeats_advance_one_viewport_at_a_time() {
        let mut e = editor_with_lines(100);
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 20 };
        e.page_down_one_screen();
        e.page_down_one_screen();
        assert_eq!(e.cursor_row, 40);
        assert_eq!(e.scroll, 40);
    }

    #[test]
    fn page_down_clamps_at_end_of_file() {
        let mut e = editor_with_lines(30);
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
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
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        e.scroll = 100;
        e.cursor_row = 100;
        e.page_up_one_screen();
        assert_eq!(e.cursor_row, 75);
        assert_eq!(e.scroll, 75);
    }

    #[test]
    fn page_up_clamps_at_top_of_file() {
        let mut e = editor_with_lines(50);
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
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
    fn duplicate_lines_down_with_no_selection_copies_the_current_line_below_and_moves_cursor_to_it() {
        let mut e = editor_with("alpha\nbeta\ngamma");
        e.cursor_row = 1;
        e.cursor_col = 2;
        e.duplicate_lines_down();
        assert_eq!(e.lines, vec!["alpha", "beta", "beta", "gamma"]);
        assert_eq!((e.cursor_row, e.cursor_col), (2, 2));
        assert!(e.dirty);
    }

    #[test]
    fn duplicate_lines_up_with_no_selection_copies_the_current_line_above_and_keeps_cursor_on_the_copy() {
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
        assert_eq!(e.lines, vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]);
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 0);
        assert!(!e.dirty);
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
    fn reload_if_clean_refuses_when_dirty() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "original\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.insert_str("local edit");
        assert!(e.dirty);

        std::fs::write(tmp.path(), "external change\n").unwrap();
        let outcome = e.reload_if_clean();
        assert!(outcome.is_none(), "should refuse to reload over dirty buffer");
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
    fn insert_str_inserts_newlines() {
        let mut e = editor_with("");
        e.insert_str("a\nb\nc");
        assert_eq!(e.lines, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(e.cursor_row, 2);
        assert_eq!(e.cursor_col, 1);
    }

    #[test]
    fn build_line_spans_no_highlights() {
        let spans = build_line_spans("hello", &[]);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn build_line_spans_full_line_highlighted() {
        let hi = vec![HiSpan { start: 0, end: 5, style: Style::default() }];
        let spans = build_line_spans("hello", &hi);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn build_line_spans_partial_highlights() {
        let hi = vec![HiSpan { start: 1, end: 3, style: Style::default() }];
        let spans = build_line_spans("abcde", &hi);
        // Expect: "a", "bc", "de"
        assert_eq!(spans.len(), 3);
    }

    #[test]
    fn editor_selection_normalised_handles_anchor_after_head() {
        let s = EditorSelection { anchor: (5, 4), head: (2, 1) };
        assert_eq!(s.normalised(), ((2, 1), (5, 4)));
    }

    #[test]
    fn editor_selection_normalised_handles_same_row() {
        let s = EditorSelection { anchor: (3, 9), head: (3, 2) };
        assert_eq!(s.normalised(), ((3, 2), (3, 9)));
    }

    #[test]
    fn editor_selection_has_area_only_when_endpoints_differ() {
        let s = EditorSelection::new(2, 5);
        assert!(!s.has_area());
        let s2 = EditorSelection { anchor: (2, 5), head: (2, 6) };
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
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        e.last_gutter_width = 2;
        e.mouse_down(3 + 0, 0); // text_x = 0 + 2 + 1 = 3, click col 3 → editor col 0
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

        let area = Rect { x: 0, y: 0, width: 30, height: 5 };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);

        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let cell = &buf[(text_x + 2, e.last_inner.y)];
        assert_eq!(cell.symbol(), "l", "editor render must leave the underlying glyph alone");
    }

    #[test]
    fn cursor_screen_pos_inside_viewport() {
        let mut e = editor_with("hello\nworld");
        e.last_inner = Rect { x: 5, y: 7, width: 80, height: 25 };
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
        e.last_inner = Rect { x: 0, y: 0, width: 40, height: 10 };
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
        let area = Rect { x: 0, y: 0, width: 60, height: 3 };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        let yellow = Color::Rgb(0xff, 0xd7, 0x4a);
        // First "needle" starts at char index 6 ("alpha "), 6 chars long.
        for col in 6..12u16 {
            assert_eq!(
                buf[(text_x + col, y)].bg, yellow,
                "first match cell {col} must have yellow bg"
            );
        }
        // Cells just outside the match must NOT be yellow.
        assert_ne!(buf[(text_x + 5, y)].bg, yellow, "cell before match");
        assert_ne!(buf[(text_x + 12, y)].bg, yellow, "cell after match");
        // Second "needle" starts at char index 19 ("alpha needle bravo "), 6 chars.
        for col in 19..25u16 {
            assert_eq!(
                buf[(text_x + col, y)].bg, yellow,
                "second match cell {col} must have yellow bg"
            );
        }
    }

    #[test]
    fn editor_render_does_not_paint_search_highlight_when_unset() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("alpha needle bravo");
        e.set_search_highlight(None);
        e.focused = true;
        let area = Rect { x: 0, y: 0, width: 40, height: 3 };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        let yellow = Color::Rgb(0xff, 0xd7, 0x4a);
        for col in 0..18u16 {
            assert_ne!(
                buf[(text_x + col, y)].bg, yellow,
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
        let area = Rect { x: 0, y: 0, width: 40, height: 3 };
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
        let area = Rect { x: 0, y: 0, width: 80, height: 20 };
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
        ed.diff_prev_arrow = Rect { x: 30, y: 0, width: 1, height: 1 };
        ed.diff_next_arrow = Rect { x: 32, y: 0, width: 1, height: 1 };
        let area = Rect { x: 0, y: 0, width: 40, height: 10 };
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
        let area = Rect { x: 0, y: 0, width: 80, height: 10 };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
        let ed = &t.editors[active_idx];
        let diff = ed.diff.as_ref().unwrap();
        // Header is at ed.last_inner.y; body starts at last_inner.y + 1.
        let body_top = ed.last_inner.y + 1;
        // Left text column begins at l_text_x = inner.x + l_gutter + 2.
        let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
        let l_text_x = ed.last_inner.x + l_gutter + 2;
        let hit = crate::widgets::editor::diff_hit_test(
            diff,
            ed.last_inner,
            l_text_x + 2,
            body_top + 1,
        );
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
        let area = Rect { x: 0, y: 0, width: 80, height: 10 };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
        let ed = &t.editors[active_idx];
        let diff = ed.diff.as_ref().unwrap();
        let body_top = ed.last_inner.y + 1;
        let half = ed.last_inner.width / 2;
        let r_gutter = (diff.right_lines.len() + 1).to_string().len() as u16 + 1;
        let r_text_x = ed.last_inner.x + half + 1 + r_gutter + 2;
        let hit = crate::widgets::editor::diff_hit_test(
            diff,
            ed.last_inner,
            r_text_x + 3,
            body_top + 1,
        );
        assert!(
            matches!(hit, Some((DiffSide::Right, 1, 3))),
            "a click three cells into the right text column of the second body row must map to Right, row 1, char col 3; got {hit:?}"
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
        let area = Rect { x: 0, y: 0, width: 80, height: 10 };
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
        let l = f1.path().file_name().unwrap().to_string_lossy().into_owned();
        let r = f2.path().file_name().unwrap().to_string_lossy().into_owned();
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
        let active = t.editors.iter().find(|e| e.path.as_deref() == Some(f2.path())).unwrap();
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
        let third = t.editors.iter().find(|e| e.path.as_deref() == Some(f3.path())).unwrap();
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
        let area = Rect { x: 0, y: 0, width: 30, height: 5 };
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
        // With scroll_col = 3, the line "ABCDEFGHIJ" should display starting
        // from 'D' at the text origin, not from 'A'.
        let mut e = editor_with("ABCDEFGHIJ");
        e.scroll_col = 3;
        e.focused = true;
        let area = Rect { x: 0, y: 0, width: 30, height: 5 };
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
    fn render_paints_selection_band_on_selected_cells() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("hello world");
        e.cursor_col = 0;
        e.start_selection_at_cursor();
        e.cursor_col = 5;
        e.extend_selection_to_cursor();
        e.focused = true;

        let area = Rect { x: 0, y: 0, width: 30, height: 5 };
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

        let area = Rect { x: 0, y: 0, width: 30, height: 6 };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);

        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let selected_bg = Color::Rgb(0x26, 0x4f, 0x78);

        // Row 0: cols 2..end (all the way past the end of "first")
        let row0_y = e.last_inner.y;
        assert_eq!(buf[(text_x + 2, row0_y)].bg, selected_bg, "row 0 col 2");
        assert_eq!(buf[(text_x + 4, row0_y)].bg, selected_bg, "row 0 col 4");
        assert_ne!(buf[(text_x, row0_y)].bg, selected_bg, "row 0 col 0 not selected");

        // Row 1 (full line "second"): all cells in selection
        let row1_y = e.last_inner.y + 1;
        assert_eq!(buf[(text_x, row1_y)].bg, selected_bg, "row 1 col 0");
        assert_eq!(buf[(text_x + 5, row1_y)].bg, selected_bg, "row 1 col 5");

        // Row 2 (last line "third"): cols 0..2 in selection
        let row2_y = e.last_inner.y + 2;
        assert_eq!(buf[(text_x, row2_y)].bg, selected_bg, "row 2 col 0");
        assert_eq!(buf[(text_x + 1, row2_y)].bg, selected_bg, "row 2 col 1");
        assert_ne!(buf[(text_x + 2, row2_y)].bg, selected_bg, "row 2 col 2 not selected");
    }

    #[test]
    fn double_click_selects_word_and_moves_cursor_to_end() {
        let mut e = editor_with("hello world");
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
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
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
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
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
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
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
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
        assert!(t.close_active(), "closing the only tab resets it instead of refusing");
        assert_eq!(t.tab_count(), 1);
        assert!(t.path.is_none());
    }

    #[test]
    fn close_at_maps_a_click_on_the_x_glyph_to_its_tab_index() {
        use ratatui::buffer::Buffer;
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
        t.add_tab_with_path(std::path::PathBuf::from("/bar.rs"));
        let area = Rect { x: 0, y: 0, width: 80, height: 10 };
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let cx0 = t.close_screen_x(0).expect("tab 0 has a close button when count > 1");
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
        let area = Rect { x: 0, y: 0, width: 60, height: 5 };
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
        let area = Rect { x: 0, y: 0, width: 60, height: 10 };
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
        assert!(t.preview_index().is_none(), "pinned open must not leave preview state");
    }

    #[test]
    fn open_preview_drops_stale_preview_when_switching_to_pinned_tab() {
        let mut t = EditorTabs::new();
        let mut f1 = NamedTempFile::new().unwrap();
        write!(f1, "a").unwrap();
        let mut f2 = NamedTempFile::new().unwrap();
        write!(f2, "b").unwrap();
        t.open_preview(f1.path()).unwrap();   // preview slot = f1, only tab
        t.pin_active();                       // f1 pinned
        t.open_preview(f2.path()).unwrap();   // preview tab for f2 alongside pinned f1
        assert_eq!(t.tab_count(), 2);
        t.open_preview(f1.path()).unwrap();   // back to f1 → stale f2 preview must vanish
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
        assert!(t.preview_index().is_none(), "no tab should be in preview state");
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
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        e.last_gutter_width = 2;
        let text_x: u16 = 3;
        e.mouse_down(text_x + 0, 0);
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
        self.last_area = area;
        self.last_inner = inner;
        self.last_scrollbar = Rect::default();

        let height = inner.height as usize;
        if height == 0 {
            return;
        }
        if let Some(image) = self.image.as_ref() {
            render_image_placeholder(image, self.path.as_deref(), inner, buf);
            return;
        }
        if let Some(view) = self.sheet.as_ref() {
            render_sheet(view, self.path.as_deref(), inner, buf);
            return;
        }
        if let Some(diff) = self.diff.as_mut() {
            let (prev_arrow, next_arrow) = render_diff(diff, inner, buf);
            self.diff_prev_arrow = prev_arrow;
            self.diff_next_arrow = next_arrow;
            return;
        }
        // Non-diff tabs: clear the hit rects so a stale arrow click on a
        // tab the user just switched away from can't fire.
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();
        if self.cursor_row < self.scroll {
            self.scroll = self.cursor_row;
        } else if self.cursor_row >= self.scroll + height {
            self.scroll = self.cursor_row + 1 - height;
        }

        let gutter_width = (self.lines.len() + 1).to_string().len() as u16 + 1;
        self.last_gutter_width = gutter_width;
        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: inner.y,
            width: u16::from(inner.width > 0),
            height: inner.height,
        };
        let scrollbar_metrics =
            scrollbar::vertical_metrics(scrollbar_area, self.lines.len(), height, self.scroll);
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }
        let scrollbar_width = u16::from(scrollbar_metrics.is_some());
        let text_x = inner.x + gutter_width + 1;
        let text_width = inner.width.saturating_sub(gutter_width + 2 + scrollbar_width);

        let sel_norm = self
            .selection
            .filter(|s| s.has_area())
            .map(|s| s.normalised());

        let end = (self.scroll + height).min(self.lines.len());
        for (row_idx, line_idx) in (self.scroll..end).enumerate() {
            let y = inner.y + row_idx as u16;
            let line_no = format!("{:>width$} ", line_idx + 1, width = gutter_width as usize - 1);
            let gutter = Line::from(Span::styled(line_no, Style::default().fg(Color::DarkGray)));
            buf.set_line(inner.x, y, &gutter, gutter_width);

            let raw = &self.lines[line_idx];
            let empty: Vec<HiSpan> = Vec::new();
            let line_spans = self.highlights.get(line_idx).unwrap_or(&empty);
            // Apply horizontal scroll: take the substring starting at
            // `scroll_col` characters in and shift the highlight ranges
            // by the same byte offset so syntax colouring follows.
            let byte_start = byte_index_of_char(raw, self.scroll_col);
            let visible_raw = &raw[byte_start..];
            let shifted = shift_spans_for_view(line_spans, byte_start);
            let spans = build_line_spans(visible_raw, &shifted);
            let line = Line::from(spans);
            buf.set_line(text_x, y, &line, text_width);

            if let Some(term) = self.search_highlight.as_deref() {
                let active_on_line = self
                    .active_search_match
                    .filter(|(r, _, _)| *r == line_idx)
                    .map(|(_, c, l)| (c, l));
                paint_search_highlight(
                    buf,
                    text_x,
                    y,
                    text_width,
                    raw,
                    term,
                    self.search_highlight_opts,
                    self.scroll_col,
                    active_on_line,
                );
            }

            if let Some(((sr, sc), (er, ec))) = sel_norm {
                if line_idx >= sr && line_idx <= er {
                    let line_chars = self.line_char_len(line_idx);
                    let row_start = if line_idx == sr { sc } else { 0 };
                    // For non-final selected rows, paint past the line content
                    // by one cell to make the trailing newline visible.
                    let row_end = if line_idx == er {
                        ec
                    } else {
                        line_chars + 1
                    };
                    let visible_start = row_start.saturating_sub(self.scroll_col);
                    let visible_end = row_end.saturating_sub(self.scroll_col);
                    if visible_end > visible_start {
                        paint_selection_band(
                            buf,
                            text_x,
                            y,
                            text_width,
                            visible_start,
                            visible_end,
                        );
                    }
                }
            }

            // The cursor itself is drawn by the host terminal as a hardware
            // caret (DECSCUSR `BlinkingBar`); App calls
            // `frame.set_cursor_position(...)` so the blink/overlay never
            // hides the underlying character.
        }
        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical(buf, metrics, self.focused);
        }
    }
}

impl Editor {
    /// Absolute (column, row) of the editor's cursor in screen coordinates,
    /// or `None` if the cursor is outside the visible viewport. Used by
    /// `App::render` to position the host terminal's hardware caret.
    pub fn cursor_screen_pos(&self) -> Option<(u16, u16)> {
        if self.last_inner.height == 0 {
            return None;
        }
        if self.cursor_row < self.scroll {
            return None;
        }
        let row_in_view = self.cursor_row - self.scroll;
        if (row_in_view as u16) >= self.last_inner.height {
            return None;
        }
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        let text_width = self
            .last_inner
            .width
            .saturating_sub(self.last_gutter_width + 2 + u16::from(self.last_scrollbar.width > 0));
        if text_width == 0 {
            return None;
        }
        if self.cursor_col < self.scroll_col {
            return None;
        }
        let visible_col = self.cursor_col - self.scroll_col;
        if (visible_col as u16) >= text_width {
            return None;
        }
        let cx = text_x + visible_col as u16;
        let cy = self.last_inner.y + row_in_view as u16;
        Some((cx, cy))
    }
}

/// Overpaint every match of `needle` in `raw_line` with the search-match
/// style, honouring `opts` (case-sensitive / whole-word / regex). Delegates
/// to `split_for_highlight` so the highlight rule stays 1:1 with the
/// search-engine matcher; column conversion uses `chars().count()` over
/// the byte prefix to stay correct for Unicode.
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
            let is_active = active_match_on_line
                .is_some_and(|(c, l)| c == abs_col && l == chunk_cols);
            let style = if is_active { active_style } else { inactive_style };
            for c in 0..chunk_cols {
                let absolute = abs_col + c;
                if absolute < scroll_col {
                    continue;
                }
                let col = (absolute - scroll_col) as u16;
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

/// Apply the selection background colour to columns `[start_char..end_char)`
/// of row `y`, where columns are character indices within the editor's text
/// area.  Clamps to the visible width.
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

/// Multi-buffer editor: a stack of `Editor` instances with a single active
/// one, plus a 1-row clickable tab strip rendered above the active editor.
/// `Deref`/`DerefMut` aim at the active editor so existing call sites that
/// were written for a single `Editor` continue to work without rewrites.
pub struct EditorTabs {
    pub editors: Vec<Editor>,
    active: usize,
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
}

impl EditorTabs {
    pub fn new() -> Self {
        Self {
            editors: vec![Editor::new()],
            active: 0,
            tab_screen_ranges: Vec::new(),
            tab_close_x: Vec::new(),
            tab_strip_y: 0,
            last_full_area: Rect::default(),
            search_highlight_term: None,
            search_highlight_opts: crate::widgets::search::SearchOpts::default(),
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

    /// If any tab currently points at `old`, repoint it to `new`. The on-
    /// disk file has already been moved; this only updates the in-memory
    /// path so subsequent saves and the tab label track the new name.
    pub fn rename_open_path(&mut self, old: &Path, new: &Path) {
        for e in &mut self.editors {
            if e.path.as_deref() == Some(old) {
                e.path = Some(new.to_path_buf());
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

    /// Close every tab whose index ≠ `keep_idx`. The kept tab stays
    /// active. Returns how many tabs were actually removed (0 when
    /// `keep_idx` is out of range or only one tab is open). Mirrors VS
    /// Code's "Close Others" context-menu action.
    pub fn close_others(&mut self, keep_idx: usize) -> usize {
        if keep_idx >= self.editors.len() || self.editors.len() <= 1 {
            return 0;
        }
        let kept = self.editors.remove(keep_idx);
        let removed = self.editors.len();
        self.editors.clear();
        self.editors.push(kept);
        self.active = 0;
        self.editors[0].focused = true;
        removed
    }

    /// Close every tab whose index > `from_idx`. The tab at `from_idx`
    /// stays active; tabs to the left are untouched. Returns the number
    /// of tabs removed. Matches VS Code's "Close to the Right".
    pub fn close_to_right(&mut self, from_idx: usize) -> usize {
        if from_idx >= self.editors.len() {
            return 0;
        }
        let target_len = from_idx + 1;
        if self.editors.len() <= target_len {
            return 0;
        }
        let removed = self.editors.len() - target_len;
        self.editors.truncate(target_len);
        if self.active >= self.editors.len() {
            self.active = self.editors.len() - 1;
        }
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
        self.tab_close_x
            .iter()
            .position(|&x| x != 0 && x == col)
    }

    /// Index of the first tab whose `path` matches `target` either by
    /// literal equality or by canonicalised equality (so symlink + relative
    /// path aliases dedupe to the same tab). Returns `None` if no tab is
    /// currently holding that file.
    pub fn find_tab_with_path(&self, target: &Path) -> Option<usize> {
        let canon_target = target.canonicalize().ok();
        self.editors.iter().position(|e| {
            let Some(p) = e.path.as_ref() else { return false };
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

    /// VS Code "preview tab" semantics: open `path` in the single
    /// replaceable preview slot. If the file is already in some tab, just
    /// switch to it. Otherwise reuse the existing preview slot, or create a
    /// fresh preview tab next to the active one when none exists.
    pub fn open_preview(&mut self, path: &Path) -> Result<()> {
        if let Some(idx) = self.find_tab_with_path(path) {
            // Switching to an already-open file: if a stale preview tab
            // exists for some OTHER file, drop it so the user doesn't
            // accumulate ghost tabs from quick "peek" navigations.
            if let Some(prev) = self.preview_index() {
                if prev != idx {
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
        let left_text = std::fs::read_to_string(left)
            .with_context(|| format!("reading {}", left.display()))?;
        let right_text = std::fs::read_to_string(right)
            .with_context(|| format!("reading {}", right.display()))?;
        let left_lines: Vec<String> =
            left_text.lines().map(str::to_string).collect();
        let right_lines: Vec<String> =
            right_text.lines().map(str::to_string).collect();
        let data = crate::widgets::diff::DiffData::build(
            left.to_path_buf(),
            right.to_path_buf(),
            left_lines,
            right_lines,
        );

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
        // user clicks through the change list.
        if let Some(idx) = self.find_tab_with_path(right) {
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
    pub fn open_deleted_diff_with_text(
        &mut self,
        path: &Path,
        head_text: &str,
    ) -> Result<()> {
        let data = crate::widgets::diff::DiffData::build_unified_deletion(
            path.to_path_buf(),
            head_text,
        );
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(path.to_path_buf());
        e.diff = Some(data);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        if let Some(idx) = self.find_tab_with_path(path) {
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

    /// Pinned-tab open: if the file is already in some tab, pin and switch
    /// to it. Otherwise create a fresh pinned tab next to the active one.
    /// Used by double-click in the explorer and Ctrl+Enter on a tree row.
    pub fn open_pinned(&mut self, path: &Path) -> Result<()> {
        if let Some(idx) = self.find_tab_with_path(path) {
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

const TAB_STRIP_BG: Color = Color::Rgb(0x1f, 0x24, 0x36);
const TAB_INACTIVE_BG: Color = Color::Rgb(0x2a, 0x2f, 0x3e);
const TAB_ACTIVE_BG: Color = Color::Rgb(0x1e, 0x3a, 0x6e);
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
        let strip = Rect { x: area.x, y: area.y, width: area.width, height: strip_h };
        let body = Rect {
            x: area.x,
            y: area.y + strip_h,
            width: area.width,
            height: area.height - strip_h,
        };

        // Paint strip background first so the gap to the right of the last
        // tab still reads as the tab-strip colour rather than terminal default.
        let strip_bg_style = Style::default().bg(TAB_STRIP_BG);
        for x in strip.x..strip.x + strip.width {
            buf[(x, strip.y)].set_style(strip_bg_style);
            buf[(x, strip.y)].set_symbol(" ");
        }

        self.tab_strip_y = strip.y;
        self.tab_screen_ranges.clear();
        self.tab_close_x.clear();
        let mut cursor_x = strip.x;
        let active = self.active;
        for (i, ed) in self.editors.iter().enumerate() {
            let label_text = tab_label(ed);
            let label_chars = label_text.chars().count() as u16;
            let pad: u16 = 1;
            let close_pad: u16 = 2;
            let width = label_chars.saturating_add(pad * 2).saturating_add(close_pad);
            if cursor_x.saturating_add(width) > strip.x + strip.width {
                self.tab_screen_ranges.push((cursor_x, 0));
                self.tab_close_x.push(0);
                continue;
            }
            let is_active = i == active;
            let bg = if is_active { TAB_ACTIVE_BG } else { TAB_INACTIVE_BG };
            let fg = if is_active { TAB_ACTIVE_FG } else { TAB_INACTIVE_FG };
            let mut modifiers = Modifier::empty();
            if is_active {
                modifiers |= Modifier::BOLD;
            }
            if ed.preview {
                modifiers |= Modifier::ITALIC;
            }
            let style = Style::default().fg(fg).bg(bg).add_modifier(modifiers);
            // Layout: " " + label + " " + ✕ + " "
            let padded = format!(" {label_text} \u{2715} ");
            buf.set_string(cursor_x, strip.y, &padded, style);
            self.tab_screen_ranges.push((cursor_x, width));
            self.tab_close_x.push(cursor_x + 1 + label_chars + 1);
            cursor_x = cursor_x.saturating_add(width);
        }

        let active_editor = &mut self.editors[active];
        Widget::render(active_editor, body, buf);
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
