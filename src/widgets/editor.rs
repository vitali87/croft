use anyhow::Result;
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

const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
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
    pub scroll: usize,
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
}

impl Editor {
    pub fn new() -> Self {
        Self {
            path: None,
            lines: Vec::new(),
            scroll: 0,
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
            image: None,
            sheet: None,
        }
    }

    pub fn set_search_highlight(&mut self, term: Option<String>) {
        self.search_highlight = term.filter(|s| !s.is_empty());
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
        self.dirty = true;
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
        self.dirty = true;
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
        self.dirty = true;
    }

    pub fn insert_newline(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::Newline);
        self.delete_selection_inner();
        self.insert_newline_raw();
        self.recompute_highlights();
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
            self.dirty = true;
        } else if self.cursor_row > 0 {
            let cur = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.line_char_len(self.cursor_row);
            self.lines[self.cursor_row].push_str(&cur);
            self.dirty = true;
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
            self.dirty = true;
        } else if row + 1 < self.lines.len() {
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
            self.dirty = true;
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
        self.dirty = true;
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

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.line_char_len(self.cursor_row);
        }
        self.last_edit_kind = None;
    }

    pub fn move_right(&mut self) {
        if self.cursor_col < self.line_char_len(self.cursor_row) {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        self.last_edit_kind = None;
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
            let spans = build_line_spans(raw, line_spans);
            let line = Line::from(spans);
            buf.set_line(text_x, y, &line, text_width);

            if let Some(term) = self.search_highlight.as_deref() {
                paint_search_highlight(
                    buf,
                    text_x,
                    y,
                    text_width,
                    raw,
                    term,
                    self.search_highlight_opts,
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
                    paint_selection_band(
                        buf,
                        text_x,
                        y,
                        text_width,
                        row_start,
                        row_end,
                    );
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
        let col = (self.cursor_col as u16).min(text_width.saturating_sub(1));
        let cx = text_x + col;
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
) {
    if needle.is_empty() {
        return;
    }
    let style = Style::default()
        .fg(Color::Black)
        .bg(Color::Rgb(0xff, 0xd7, 0x4a))
        .add_modifier(Modifier::BOLD);
    let segments = crate::widgets::search::split_for_highlight(raw_line, needle, opts);
    let mut col_cursor: u16 = 0;
    for (chunk, is_match) in segments {
        let chunk_cols = chunk.chars().count() as u16;
        if is_match {
            for c in 0..chunk_cols {
                let col = col_cursor + c;
                if col >= text_width {
                    break;
                }
                buf[(text_x + col, y)].set_style(style);
            }
        }
        col_cursor = col_cursor.saturating_add(chunk_cols);
        if col_cursor >= text_width {
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
        self.search_highlight_term = normalised.clone();
        self.search_highlight_opts = opts;
        for ed in &mut self.editors {
            ed.search_highlight = normalised.clone();
            ed.search_highlight_opts = opts;
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
