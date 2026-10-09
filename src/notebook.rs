//! Jupyter notebook rendered view (#180): parse .ipynb JSON and build
//! the same (lines, images) state the Markdown preview machinery
//! renders, so scrolling, wrapping, the inline-image overlay, and
//! "Reopen as Text" (the raw JSON) all come for free.
//!
//! Markdown cells render through the markdown builder (local images
//! resolve against the notebook's directory); code cells render as
//! fenced blocks in the kernel's language with an `In [n]` frame;
//! text/stream outputs paint dim (ANSI stripped), errors red, and
//! image/png outputs are written to the session scratch directory and
//! reserve overlay rows exactly like a Markdown picture. Each code cell
//! wears the run glyph; running it goes through the notebook's kernel
//! (`notebook_kernel`, #355), whose outputs land in the file itself.

use crate::markdown::{MdImage, MdRunnable};
use crate::theme::Theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::Path;

const DIM: Color = Color::Rgb(0x80, 0x84, 0x90);
const ERR: Color = Color::Rgb(0xe0, 0x6c, 0x75);
const FRAME: Color = Color::Rgb(0x4e, 0x9a, 0xff);

/// True when the text parses as a notebook document (a cell list is
/// present): the router's content check.
pub fn looks_like_notebook(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .is_some_and(|v| v.get("cells").is_some_and(|c| c.is_array()))
}

/// Parse SGR-colored output text into styled spans (#199 review): the
/// 16 basic/bright foregrounds map through the theme's ANSI palette,
/// bold is honored, reset returns to `base`; every other escape
/// (unsupported CSI, OSC) is stripped. One Line per text line.
fn ansi_lines(text: &str, base: Style, theme: Theme, indent: &str) -> Vec<Line<'static>> {
    let ansi = theme.ansi();
    let mut out = Vec::new();
    for raw in text.lines() {
        let mut spans: Vec<Span<'static>> = vec![Span::raw(indent.to_string())];
        let mut cur = String::new();
        let mut style = base;
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\u{1b}' {
                cur.push(c);
                continue;
            }
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut fin = ' ';
                    for t in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&t) {
                            fin = t;
                            break;
                        }
                        params.push(t);
                    }
                    if fin == 'm' {
                        if !cur.is_empty() {
                            spans.push(Span::styled(std::mem::take(&mut cur), style));
                        }
                        for p in params.split(';') {
                            match p.parse::<u8>().unwrap_or(0) {
                                0 => style = base,
                                1 => style = style.add_modifier(Modifier::BOLD),
                                n @ 30..=37 => {
                                    let (r, g, b) = ansi[(n - 30) as usize];
                                    style = style.fg(Color::Rgb(r, g, b));
                                }
                                n @ 90..=97 => {
                                    let (r, g, b) = ansi[(n - 90 + 8) as usize];
                                    style = style.fg(Color::Rgb(r, g, b));
                                }
                                39 => style = style.fg(base.fg.unwrap_or(Color::Reset)),
                                _ => {}
                            }
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(t) = chars.next() {
                        if t == '\u{7}' {
                            break;
                        }
                        if t == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        if !cur.is_empty() {
            spans.push(Span::styled(cur, style));
        }
        out.push(Line::from(spans));
    }
    out
}

/// Strip ANSI escape sequences (CSI and OSC) from output text.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for t in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&t) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(t) = chars.next() {
                    if t == '\u{7}' {
                        break;
                    }
                    if t == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Write a scratch image with no-follow semantics (#199 review): the
/// hash-named file is reused only when it already exists as a REGULAR
/// file; otherwise it is created with create_new, so a planted symlink
/// at a predicted name is refused, never followed. On unix the scratch
/// dir is created owner-only.
fn write_scratch_no_follow(scratch: &Path, path: &Path, bytes: &[u8]) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => return true,
        Ok(_) => return false,
        Err(_) => {}
    }
    if std::fs::create_dir_all(scratch).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(scratch, std::fs::Permissions::from_mode(0o700));
    }
    use std::io::Write as _;
    let Ok(mut f) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    else {
        return false;
    };
    f.write_all(bytes).is_ok()
}

/// Join a notebook "multiline string" (either a JSON string or a list
/// of line strings, per nbformat).
fn joined(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.as_str())
            .collect::<Vec<_>>()
            .concat(),
        _ => String::new(),
    }
}

/// Render the notebook to preview lines + overlay images + one runnable
/// per code cell. `scratch` receives decoded image outputs (one file per
/// output, named by a content hash so rebuilds reuse them). Cells listed
/// in `running` show `In [*]`, as Jupyter marks a cell that is running or
/// queued.
pub fn render(
    text: &str,
    theme: Theme,
    registry: &mut crate::highlight::LangRegistry,
    base_dir: Option<&Path>,
    scratch: &Path,
    running: &[usize],
) -> Option<(Vec<Line<'static>>, Vec<MdImage>, Vec<MdRunnable>)> {
    let doc: serde_json::Value = serde_json::from_str(text).ok()?;
    let cells = doc.get("cells")?.as_array()?;
    let lang = doc
        .pointer("/metadata/kernelspec/language")
        .and_then(|v| v.as_str())
        .unwrap_or("python")
        .to_string();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut images: Vec<MdImage> = Vec::new();
    let mut runnables: Vec<MdRunnable> = Vec::new();
    let frame = Style::default()
        .fg(theme.ui(FRAME))
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(theme.ui(DIM));
    for (index, cell) in cells.iter().enumerate() {
        let kind = cell.get("cell_type").and_then(|v| v.as_str()).unwrap_or("");
        let source = cell.get("source").map(joined).unwrap_or_default();
        match kind {
            "markdown" => {
                let (mut md_lines, md_images) = crate::markdown::render_markdown_with_images(
                    &source, theme, registry, base_dir,
                );
                let base = lines.len();
                for mut img in md_images {
                    img.first_line += base;
                    images.push(img);
                }
                lines.append(&mut md_lines);
                lines.push(Line::default());
            }
            "code" => {
                let n = if running.contains(&index) {
                    String::from("*")
                } else {
                    cell.get("execution_count")
                        .and_then(|v| v.as_u64())
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| String::from(" "))
                };
                runnables.push(MdRunnable {
                    first_line: lines.len(),
                    lines: (0, 0),
                    code: source.clone(),
                    interpreter: "kernel",
                    destructive: false,
                    cwd_root: false,
                    persist: false,
                    capture_timeout: None,
                    kernel_cell: Some(index),
                });
                lines.push(Line::from(vec![
                    Span::styled(crate::markdown::RUN_GLYPH, frame),
                    Span::styled(format!("In [{n}]:"), frame),
                ]));
                let fenced = format!("```{lang}\n{source}\n```");
                let (mut code_lines, _) =
                    crate::markdown::render_markdown_with_images(&fenced, theme, registry, None);
                lines.append(&mut code_lines);
                for output in cell
                    .get("outputs")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    let out = OutputSink {
                        scratch,
                        registry: &mut *registry,
                        base_dir,
                        lines: &mut lines,
                        images: &mut images,
                    };
                    render_output(output, out, dim, theme);
                }
                lines.push(Line::default());
            }
            _ => {}
        }
    }
    Some((lines, images, runnables))
}

/// Where an output's lines and pictures go, and what a Markdown output
/// renders with.
struct OutputSink<'a> {
    scratch: &'a Path,
    registry: &'a mut crate::highlight::LangRegistry,
    base_dir: Option<&'a Path>,
    lines: &'a mut Vec<Line<'static>>,
    images: &'a mut Vec<MdImage>,
}

impl OutputSink<'_> {
    /// Write a decoded picture to scratch and reserve rows for it. False
    /// when it is not an image croft can size, or is too large.
    fn picture(&mut self, bytes: &[u8], ext: &str) -> bool {
        let Ok(Some((px_w, px_h))) = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .map(|r| r.into_dimensions().ok())
        else {
            return false;
        };
        if (px_w as u64) * (px_h as u64) > 64_000_000 {
            return false;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash as _, Hasher as _};
        bytes.hash(&mut hasher);
        let name = format!("nb-{:016x}.{ext}", hasher.finish());
        let path = self.scratch.join(name);
        if !write_scratch_no_follow(self.scratch, &path, bytes) {
            return false;
        }
        let rows = ((px_h as f32 / px_w.max(1) as f32) * 72.0 / 2.0)
            .round()
            .clamp(3.0, 18.0) as u16;
        let first_line = self.lines.len();
        for _ in 0..rows {
            self.lines.push(Line::default());
        }
        self.images.push(MdImage {
            first_line,
            rows,
            path,
        });
        true
    }

    /// Render `md` as a Markdown cell renders.
    fn markdown(&mut self, md: &str, theme: Theme) {
        let (mut md_lines, md_images) =
            crate::markdown::render_markdown_with_images(md, theme, self.registry, self.base_dir);
        let base = self.lines.len();
        for mut img in md_images {
            img.first_line += base;
            self.images.push(img);
        }
        self.lines.append(&mut md_lines);
    }
}

fn render_output(output: &serde_json::Value, mut out: OutputSink<'_>, dim: Style, theme: Theme) {
    let otype = output
        .get("output_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match otype {
        "stream" => {
            let text = output.get("text").map(joined).unwrap_or_default();
            out.lines.extend(ansi_lines(&text, dim, theme, "  "));
        }
        "error" => {
            let err = Style::default().fg(theme.ui(ERR));
            for tl in output
                .get("traceback")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                out.lines
                    .extend(ansi_lines(tl.as_str().unwrap_or(""), err, theme, "  "));
            }
        }
        // The richest type croft can draw wins, in Jupyter's order (#1188):
        // a picture, then Markdown, then HTML, and only then the
        // `text/plain` fallback (`<IPython.core.display.Markdown object>`).
        "execute_result" | "display_data" => {
            let data = |mime: &str| {
                output
                    .get("data")
                    .and_then(|d| d.get(mime))
                    .map(joined)
                    .filter(|s| !s.trim().is_empty())
            };
            for (mime, ext) in [("image/png", "png"), ("image/jpeg", "jpg")] {
                use base64::Engine as _;
                let Some(b64) = data(mime) else { continue };
                let compact: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(compact)
                    && out.picture(&bytes, ext)
                {
                    return;
                }
            }
            if let Some(svg) = data("image/svg+xml")
                && let Ok((png, _, _)) = crate::svg::rasterize(svg.as_bytes())
                && out.picture(&png, "png")
            {
                return;
            }
            if let Some(md) = data("text/markdown") {
                out.markdown(&md, theme);
                return;
            }
            if let Some(md) = data("text/html")
                .map(|html| html_to_markdown(&html))
                .filter(|md| !md.trim().is_empty())
            {
                out.markdown(&md, theme);
                return;
            }
            let text = strip_ansi(&data("text/plain").unwrap_or_default());
            for l in text.lines() {
                out.lines
                    .push(Line::from(Span::styled(format!("  {l}"), dim)));
            }
        }
        _ => {}
    }
}

/// HTML elements whose content is never shown.
const HTML_HIDDEN: &[&str] = &["style", "script", "head", "title", "template"];

/// Turn an HTML output into Markdown for the notebook view (#1188): the
/// structure a reader needs (headings, paragraphs, emphasis, lists, code,
/// tables such as a pandas DataFrame's) survives, every other tag is
/// dropped, and `<style>` / `<script>` contents never show.
fn html_to_markdown(html: &str) -> String {
    let mut md = String::new();
    // The open table, if any.
    let mut table: Option<HtmlTable> = None;
    let mut hidden = 0usize;
    let mut rest = html;
    let push = |md: &mut String, table: &mut Option<HtmlTable>, text: &str| match table {
        None => md.push_str(text),
        // A cell is one line of a pipe table; text between a table's cells
        // is whitespace or junk.
        Some(t) => t.push_text(&text.replace('\n', " ")),
    };
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if hidden == 0 {
                push(&mut md, &mut table, &html_text(rest));
            }
            break;
        };
        if hidden == 0 {
            push(&mut md, &mut table, &html_text(&rest[..lt]));
        }
        rest = &rest[lt..];
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |end| &rest[end + 3..]);
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if HTML_HIDDEN.contains(&name.as_str()) {
            if closing {
                hidden = hidden.saturating_sub(1);
            } else if !tag.ends_with('/') {
                hidden += 1;
            }
            continue;
        }
        if hidden > 0 {
            continue;
        }
        // Preformatted text keeps its lines and spacing (#1615): a fenced
        // block of the element's text, verbatim, rather than a paragraph
        // that collapses it onto one line.
        if name == "pre" && !closing && table.is_none() {
            let (inner, after) = match find_ascii_ci(rest, "</pre") {
                Some(end) => (
                    &rest[..end],
                    rest[end..].find('>').map_or("", |gt| &rest[end + gt + 1..]),
                ),
                None => (rest, ""),
            };
            rest = after;
            md.push_str(&fenced_pre(inner));
            continue;
        }
        let mark = match (name.as_str(), closing) {
            ("table", false) => {
                table = Some(HtmlTable::default());
                ""
            }
            ("table", true) => {
                if let Some(t) = table.take() {
                    md.push_str("\n\n");
                    md.push_str(&t.into_pipe_table());
                    md.push('\n');
                }
                ""
            }
            ("thead", closing) => {
                if let Some(t) = table.as_mut() {
                    t.in_head = !closing;
                }
                ""
            }
            ("tr", false) => {
                if let Some(t) = table.as_mut() {
                    t.start_row();
                }
                ""
            }
            ("td" | "th", false) => {
                if let Some(t) = table.as_mut() {
                    t.start_cell(span_attr(tag, "colspan"), span_attr(tag, "rowspan"));
                }
                ""
            }
            ("b" | "strong", _) => "**",
            ("i" | "em", _) => "*",
            ("code", _) => "`",
            ("br", _) => "  \n",
            ("li", false) => "\n- ",
            ("p" | "div" | "ul" | "ol" | "pre" | "blockquote", _) => "\n\n",
            (h, false) if heading_level(h).is_some() => {
                let hashes = "#".repeat(heading_level(h).unwrap_or(1));
                push(&mut md, &mut table, &format!("\n\n{hashes} "));
                ""
            }
            (h, true) if heading_level(h).is_some() => "\n\n",
            _ => "",
        };
        push(&mut md, &mut table, mark);
    }
    if let Some(t) = table.take() {
        md.push_str("\n\n");
        md.push_str(&t.into_pipe_table());
    }
    md.trim().to_string()
}

/// The most columns or rows one cell may span. pandas never comes close;
/// the cap keeps a corrupt `colspan="99999999"` from building a huge row.
const MAX_CELL_SPAN: usize = 1000;

/// Past this many cells in one table, spans are read as 1, so a handful of
/// spanning tags cannot grow the grid a cell per column they claim.
const MAX_SPANNED_CELLS: usize = 100_000;

/// A `colspan` / `rowspan` attribute's value, 1 when absent or unreadable.
fn span_attr(tag: &str, attr: &str) -> usize {
    let lower = tag.to_ascii_lowercase();
    let Some(at) = lower.find(attr) else {
        return 1;
    };
    let value = lower[at + attr.len()..]
        .trim_start()
        .strip_prefix('=')
        .map(|v| v.trim_start().trim_start_matches(['"', '\'']))
        .unwrap_or("");
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    digits
        .parse::<usize>()
        .ok()
        .filter(|&n| n >= 1)
        .map_or(1, |n| n.min(MAX_CELL_SPAN))
}

/// The byte index of `needle` (ASCII) in `hay`, ignoring ASCII case.
fn find_ascii_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// A `<pre>` element's content as a fenced block: tags dropped, entities
/// decoded, every space and line kept.
fn fenced_pre(inner: &str) -> String {
    let mut raw = String::new();
    let mut rest = inner;
    while let Some(lt) = rest.find('<') {
        raw.push_str(&rest[..lt]);
        rest = rest[lt..].find('>').map_or("", |gt| &rest[lt + gt + 1..]);
    }
    raw.push_str(rest);
    let text = decode_entities(&raw);
    let text = text.strip_prefix('\n').unwrap_or(&text).trim_end();
    if text.trim().is_empty() {
        return String::new();
    }
    let longest_run = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest_run.max(2) + 1);
    format!("\n\n{fence}\n{text}\n{fence}\n\n")
}

/// One cell of an HTML table as it is laid out on the grid.
#[derive(Default)]
struct HtmlCell {
    text: String,
    /// A column a `colspan` cell to the left covers: shown as that cell's
    /// text in a header, empty in the body.
    spanned: bool,
}

/// An HTML table laid out on a grid, honouring `colspan` and `rowspan`
/// (#1615): pandas writes both for every MultiIndex, and dropping them
/// slid each later value under the wrong column.
#[derive(Default)]
struct HtmlTable {
    rows: Vec<Vec<HtmlCell>>,
    /// How many of the first rows are header rows (`<thead>`).
    head_rows: usize,
    in_head: bool,
    /// The cell text goes into: its column in the last row.
    current: Option<usize>,
    /// Per column, how many more rows a `rowspan` cell above still covers.
    covered: Vec<usize>,
    /// Cells laid out so far, fillers included.
    cells: usize,
}

impl HtmlTable {
    fn start_row(&mut self) {
        self.finish_row();
        self.rows.push(Vec::new());
        if self.in_head {
            self.head_rows += 1;
        }
        self.current = None;
    }

    /// Fill the columns a `rowspan` from above still covers at the end of
    /// the last row, so the next row's spans line up.
    fn finish_row(&mut self) {
        let Some(row) = self.rows.last_mut() else {
            return;
        };
        for col in row.len()..self.covered.len() {
            if self.covered[col] > 0 {
                self.covered[col] -= 1;
                while row.len() <= col {
                    row.push(HtmlCell::default());
                }
            }
        }
    }

    fn start_cell(&mut self, colspan: usize, rowspan: usize) {
        let (colspan, rowspan) = if self.cells.saturating_add(colspan * rowspan) > MAX_SPANNED_CELLS
        {
            (1, 1)
        } else {
            (colspan, rowspan)
        };
        self.cells += colspan * rowspan;
        if self.rows.is_empty() {
            self.rows.push(Vec::new());
        }
        let row = self.rows.last_mut().expect("a row");
        // Skip the columns a `rowspan` cell above still covers.
        while self.covered.get(row.len()).is_some_and(|&n| n > 0) {
            self.covered[row.len()] -= 1;
            row.push(HtmlCell::default());
        }
        let col = row.len();
        row.push(HtmlCell::default());
        for _ in 1..colspan {
            row.push(HtmlCell {
                text: String::new(),
                spanned: true,
            });
        }
        if self.covered.len() < col + colspan {
            self.covered.resize(col + colspan, 0);
        }
        for c in col..col + colspan {
            self.covered[c] = rowspan - 1;
        }
        self.current = Some(col);
    }

    fn push_text(&mut self, text: &str) {
        let Some(col) = self.current else { return };
        if let Some(cell) = self.rows.last_mut().and_then(|r| r.get_mut(col)) {
            cell.text.push_str(text);
        }
    }

    /// The grid as a pipe table. Several header rows (pandas' MultiIndex
    /// columns, or a named index under them) become one, each column's
    /// labels joined top to bottom, since a pipe table has one header row.
    fn into_pipe_table(mut self) -> String {
        self.finish_row();
        let head_rows = self.head_rows.min(self.rows.len()).max(1);
        let cols = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return String::new();
        }
        let mut body = self.rows.split_off(head_rows.min(self.rows.len()));
        let head = self.rows;
        let mut header = vec![String::new(); cols];
        for row in &head {
            let mut left = String::new();
            for (c, cell) in row.iter().enumerate() {
                let text = if cell.spanned {
                    left.clone()
                } else {
                    cell.text.trim().to_string()
                };
                left = text.clone();
                if !text.is_empty() {
                    if !header[c].is_empty() {
                        header[c].push(' ');
                    }
                    header[c].push_str(&text);
                }
            }
        }
        let mut rows = vec![header];
        rows.extend(
            body.drain(..)
                .map(|r| r.into_iter().map(|cell| cell.text).collect()),
        );
        pipe_table(rows)
    }
}

/// The level of a heading tag name (`h1`..`h6`).
fn heading_level(name: &str) -> Option<usize> {
    match name.as_bytes() {
        [b'h', n @ b'1'..=b'6'] => Some((n - b'0') as usize),
        _ => None,
    }
}

/// HTML entities decoded; everything else, whitespace included, as is.
fn decode_entities(raw: &str) -> String {
    let mut text = String::new();
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        text.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let end = rest.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| match &rest[1..e] {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            n if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16)
                .ok()
                .and_then(char::from_u32),
            n if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        });
        match (decoded, end) {
            (Some(c), Some(e)) => {
                text.push(c);
                rest = &rest[e + 1..];
            }
            _ => {
                text.push('&');
                rest = &rest[1..];
            }
        }
    }
    text.push_str(rest);
    text
}

/// An HTML text run as Markdown text: entities decoded, whitespace
/// collapsed as a browser does, Markdown's own characters escaped.
fn html_text(raw: &str) -> String {
    let text = decode_entities(raw);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        // Whitespace between two tags still separates their text.
        return if text.is_empty() {
            text
        } else {
            String::from(" ")
        };
    }
    let mut out = crate::docx::md_escape(&collapsed);
    // Keep the spaces at either end: they separate this run from the
    // emphasis or text beside it.
    if text.starts_with(char::is_whitespace) {
        out.insert(0, ' ');
    }
    if text.ends_with(char::is_whitespace) {
        out.push(' ');
    }
    out
}

/// `rows` as a Markdown pipe table, the first row its header.
fn pipe_table(rows: Vec<Vec<String>>) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return String::new();
    }
    let mut md = String::new();
    for (i, row) in rows.iter().enumerate() {
        md.push('|');
        for c in 0..cols {
            md.push(' ');
            md.push_str(row.get(c).map_or("", |s| s.trim()));
            md.push_str(" |");
        }
        md.push('\n');
        if i == 0 {
            md.push('|');
            md.push_str(&"---|".repeat(cols));
            md.push('\n');
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nb(cells: &str) -> String {
        format!(
            r#"{{"nbformat":4,"metadata":{{"kernelspec":{{"language":"python"}}}},"cells":[{cells}]}}"#
        )
    }

    #[test]
    fn renders_markdown_code_outputs_and_errors() {
        let doc = nb(
            "{\"cell_type\":\"markdown\",\"source\":[\"# Head\\n\",\"body text\"]},\
             {\"cell_type\":\"code\",\"execution_count\":2,\"source\":[\"print(1)\\n\"],\
              \"outputs\":[{\"output_type\":\"stream\",\"text\":[\"out line\\n\"]},\
                           {\"output_type\":\"error\",\"traceback\":[\"\\u001b[31mBoom\\u001b[0m\"]}]}",
        );
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = crate::highlight::LangRegistry::default();
        let (lines, _, runnables) =
            render(&doc, Theme::BLACK, &mut reg, None, tmp.path(), &[]).expect("parses");
        // The code cell (index 1; the markdown cell is 0) wears the run
        // glyph on its `In [n]` line; the markdown cell does not run.
        assert_eq!(runnables.len(), 1);
        assert_eq!(runnables[0].kernel_cell, Some(1));
        assert_eq!(runnables[0].code, "print(1)\n");
        let frame = &lines[runnables[0].first_line];
        assert_eq!(frame.spans[0].content.as_ref(), crate::markdown::RUN_GLYPH);
        assert_eq!(frame.spans[1].content.as_ref(), "In [2]:");
        // Running or queued, the count reads `*`, as in Jupyter.
        let (lines, _, _) =
            render(&doc, Theme::BLACK, &mut reg, None, tmp.path(), &[1]).expect("parses");
        assert_eq!(
            lines[runnables[0].first_line].spans[1].content.as_ref(),
            "In [*]:"
        );
        let (lines, images, _) =
            render(&doc, Theme::BLACK, &mut reg, None, tmp.path(), &[]).expect("parses");
        let all: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(all.contains("Head"));
        assert!(all.contains("body text"));
        assert!(all.contains("In [2]:"));
        assert!(all.contains("print"));
        assert!(all.contains("out line"));
        assert!(all.contains("Boom"), "traceback text survives: {all}");
        assert!(!all.contains('\u{1b}'), "ANSI stripped");
        assert!(images.is_empty());
    }

    #[test]
    fn png_outputs_reserve_rows_and_land_in_scratch() {
        use base64::Engine as _;
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(100, 50)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        let doc = nb(&format!(
            "{{\"cell_type\":\"code\",\"execution_count\":1,\"source\":[\"plot()\"],\
              \"outputs\":[{{\"output_type\":\"display_data\",\"data\":{{\"image/png\":\"{b64}\"}}}}]}}"
        ));
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = crate::highlight::LangRegistry::default();
        let (lines, images, _) =
            render(&doc, Theme::BLACK, &mut reg, None, tmp.path(), &[]).expect("parses");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].rows, 18);
        assert!(images[0].path.is_file(), "decoded png written to scratch");
        for i in 0..images[0].rows as usize {
            assert!(lines[images[0].first_line + i].spans.is_empty());
        }
        // Rebuild reuses the SAME hash-named file.
        let (_, again, _) = render(&doc, Theme::BLACK, &mut reg, None, tmp.path(), &[]).unwrap();
        assert_eq!(again[0].path, images[0].path);
    }

    /// One code cell whose single output carries `data`.
    fn rich_output(data: &str) -> (tempfile::TempDir, Vec<Line<'static>>, Vec<MdImage>, String) {
        let doc = nb(&format!(
            "{{\"cell_type\":\"code\",\"execution_count\":1,\"source\":[\"show()\"],\
              \"outputs\":[{{\"output_type\":\"display_data\",\"data\":{data}}}]}}"
        ));
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = crate::highlight::LangRegistry::default();
        let (lines, images, _) =
            render(&doc, Theme::BLACK, &mut reg, None, tmp.path(), &[]).expect("parses");
        let text = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (tmp, lines, images, text)
    }

    /// #1188: `display(Markdown(...))` shows its Markdown, not the
    /// `<IPython.core.display.Markdown object>` fallback.
    #[test]
    fn a_markdown_output_renders_as_markdown() {
        let (_tmp, _, _, text) = rich_output(
            r###"{"text/markdown":"## Result\n\n| a | b |\n|---|---|\n| 1 | 2 |",
                "text/plain":"<IPython.core.display.Markdown object>"}"###,
        );
        assert!(text.contains("Result"), "{text}");
        assert!(!text.contains("##"), "the heading is rendered: {text}");
        assert!(text.contains('│'), "the table is drawn: {text}");
        assert!(!text.contains("Markdown object"), "{text}");
    }

    /// #1188: an SVG output is rasterised and placed like a PNG.
    #[test]
    fn an_svg_output_is_drawn_as_a_picture() {
        let svg = r##"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"80\" height=\"40\"><rect width=\"80\" height=\"40\" fill=\"red\"/></svg>"##;
        let (_tmp, _, images, text) = rich_output(&format!(
            r##"{{"image/svg+xml":"{svg}","text/plain":"<IPython.core.display.SVG object>"}}"##
        ));
        assert_eq!(images.len(), 1, "{text}");
        assert!(images[0].path.is_file());
        assert_eq!(
            images[0].path.extension().and_then(|e| e.to_str()),
            Some("png")
        );
        assert!(!text.contains("SVG object"), "{text}");
    }

    /// #1188: an HTML output shows its text with its emphasis, not raw tags
    /// or the fallback.
    #[test]
    fn an_html_output_renders_its_text() {
        let (_tmp, lines, _, text) = rich_output(
            r##"{"text/html":"<b>bold</b> and <i>more</i> &amp; done","text/plain":"<IPython.core.display.HTML object>"}"##,
        );
        assert!(text.contains("bold and more & done"), "{text}");
        assert!(!text.contains("<b>"), "{text}");
        assert!(!text.contains("HTML object"), "{text}");
        let bold = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains("bold"))
            .unwrap();
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
    }

    /// #1188: a pandas DataFrame's HTML is a table, and its `<style>` block
    /// never shows.
    #[test]
    fn an_html_table_renders_as_a_table() {
        let html = "<div><style scoped>.dataframe tbody tr th { vertical-align: top; }</style>\
            <table border=\\\"1\\\" class=\\\"dataframe\\\"><thead><tr><th></th><th>name</th><th>n</th></tr></thead>\
            <tbody><tr><th>0</th><td>apple</td><td>3</td></tr><tr><th>1</th><td>pear &lt;ripe&gt;</td><td>5</td></tr></tbody></table></div>";
        let (_tmp, _, _, text) = rich_output(&format!(
            r##"{{"text/html":"{html}","text/plain":"   name  n\n0  apple  3"}}"##
        ));
        assert!(!text.contains("vertical-align"), "{text}");
        let row = text.lines().find(|l| l.contains("apple")).expect(&text);
        assert!(row.contains('│') && row.contains('3'), "{text}");
        assert!(text.contains("pear <ripe>"), "{text}");
    }

    /// pandas' `groupby("region").agg({"units": ["sum", "mean"], "price":
    /// ["min", "max"]})._repr_html_()`, trimmed of its style block.
    const GROUPBY_HTML: &str = r#"<table border="1" class="dataframe">
  <thead>
    <tr>
      <th></th>
      <th colspan="2" halign="left">units</th>
      <th colspan="2" halign="left">price</th>
    </tr>
    <tr>
      <th></th>
      <th>sum</th>
      <th>mean</th>
      <th>min</th>
      <th>max</th>
    </tr>
    <tr>
      <th>region</th>
      <th></th>
      <th></th>
      <th></th>
      <th></th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <th>north</th>
      <td>255</td>
      <td>127</td>
      <td>9</td>
      <td>10</td>
    </tr>
    <tr>
      <th>south</th>
      <td>175</td>
      <td>87</td>
      <td>10</td>
      <td>11</td>
    </tr>
  </tbody>
</table>"#;

    /// pandas' `set_index(["region", "year"])._repr_html_()`: the shared
    /// `region` cell spans two rows.
    const SET_INDEX_HTML: &str = r#"<table border="1" class="dataframe">
  <thead>
    <tr style="text-align: right;">
      <th></th>
      <th></th>
      <th>units</th>
      <th>price</th>
    </tr>
    <tr>
      <th>region</th>
      <th>year</th>
      <th></th>
      <th></th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <th rowspan="2" valign="top">north</th>
      <th>2024</th>
      <td>120</td>
      <td>9</td>
    </tr>
    <tr>
      <th>2025</th>
      <td>135</td>
      <td>10</td>
    </tr>
    <tr>
      <th rowspan="2" valign="top">south</th>
      <th>2024</th>
      <td>80</td>
      <td>11</td>
    </tr>
    <tr>
      <th>2025</th>
      <td>95</td>
      <td>12</td>
    </tr>
  </tbody>
</table>"#;

    /// #1615: a column header spanning two columns labels both of them, and
    /// pandas' three header rows become one, so every value sits under its
    /// own column.
    #[test]
    fn a_colspan_header_labels_every_column_it_spans() {
        assert_eq!(
            html_to_markdown(GROUPBY_HTML),
            "| region | units sum | units mean | price min | price max |\n\
             |---|---|---|---|---|\n\
             | north | 255 | 127 | 9 | 10 |\n\
             | south | 175 | 87 | 10 | 11 |"
        );
    }

    /// #1615: the rows under a `rowspan` cell keep that column empty instead
    /// of sliding their values left into it.
    #[test]
    fn a_rowspan_cell_keeps_the_rows_below_it_aligned() {
        assert_eq!(
            html_to_markdown(SET_INDEX_HTML),
            "| region | year | units | price |\n\
             |---|---|---|---|\n\
             | north | 2024 | 120 | 9 |\n\
             |  | 2025 | 135 | 10 |\n\
             | south | 2024 | 80 | 11 |\n\
             |  | 2025 | 95 | 12 |"
        );
    }

    /// The issue's notebook, rendered: 135 is in the units column of 2025's
    /// row, not under year.
    #[test]
    fn a_multiindex_table_renders_values_under_their_headers() {
        let html = SET_INDEX_HTML.replace('"', "\\\"").replace('\n', "\\n");
        let (_tmp, _, _, text) =
            rich_output(&format!(r##"{{"text/html":"{html}","text/plain":"x"}}"##));
        let cells = |needle: &str| -> Vec<String> {
            let row = text.lines().find(|l| l.contains(needle)).expect(&text);
            row.split('│').map(|c| c.trim().to_string()).collect()
        };
        let header = cells("units");
        let row = cells("135");
        let col = |cells: &[String], v: &str| cells.iter().position(|c| c == v);
        assert_eq!(col(&row, "135"), col(&header, "units"), "{text}");
        assert_eq!(col(&row, "2025"), col(&header, "year"), "{text}");
    }

    /// #1615: `<pre>` output keeps its lines and spacing.
    #[test]
    fn preformatted_html_keeps_its_lines() {
        let md = html_to_markdown("<pre>step  loss\n   1  0.912\n   2  0.640</pre>");
        assert_eq!(md, "```\nstep  loss\n   1  0.912\n   2  0.640\n```");
        let (_tmp, _, _, text) = rich_output(
            r##"{"text/html":"<pre>step  loss\n   1  0.912\n   2  &lt;0.640&gt;</pre>","text/plain":"<HTML>"}"##,
        );
        assert!(text.contains("step  loss"), "{text}");
        assert!(text.contains("   1  0.912"), "{text}");
        assert!(text.contains("<0.640>"), "entities decoded: {text}");
        assert!(!text.contains("loss 1"), "not joined onto one line: {text}");
    }

    /// Negative: a table without spans and inline HTML are unchanged, and a
    /// corrupt span cannot blow up the grid.
    #[test]
    fn plain_tables_and_inline_html_are_unchanged() {
        assert_eq!(
            html_to_markdown(
                "<table><thead><tr><th></th><th>name</th></tr></thead>\
                 <tbody><tr><th>0</th><td>apple</td></tr></tbody></table>"
            ),
            "|  | name |\n|---|---|\n| 0 | apple |"
        );
        assert_eq!(
            html_to_markdown(
                "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>"
            ),
            "| a | b |\n|---|---|\n| c | d |",
            "with no <thead> the first row is still the header"
        );
        assert_eq!(
            html_to_markdown("<b>bold</b> and <code>x</code>"),
            "**bold** and `x`"
        );
        let huge = html_to_markdown(
            "<table><tr><td colspan=\"99999999\" rowspan=\"99999999\">a</td></tr><tr><td>b</td></tr></table>",
        );
        assert!(
            huge.len() < 10_000,
            "a corrupt span is capped: {} bytes",
            huge.len()
        );
        assert_eq!(span_attr("td colspan=\"0\"", "colspan"), 1);
        assert_eq!(span_attr("td colspan='3'", "colspan"), 3);
        assert_eq!(span_attr("td ROWSPAN=2", "rowspan"), 2);
        assert_eq!(span_attr("td", "rowspan"), 1);
    }

    /// #1188 negative: an output with only `text/plain` still shows it.
    #[test]
    fn a_plain_text_output_is_unchanged() {
        let (_tmp, _, images, text) = rich_output(r##"{"text/plain":"42"}"##);
        assert!(images.is_empty());
        assert!(text.contains("  42"), "{text}");
    }

    /// #1188 negative: an SVG that will not parse, or HTML with nothing
    /// to show, falls back to `text/plain`.
    #[test]
    fn unusable_rich_data_falls_back_to_plain_text() {
        let (_tmp, _, images, text) =
            rich_output(r##"{"image/svg+xml":"<svg nope","text/plain":"fallback one"}"##);
        assert!(images.is_empty());
        assert!(text.contains("fallback one"), "{text}");
        let (_tmp, _, _, text) = rich_output(
            r##"{"text/html":"<style>p { color: red }</style>","text/plain":"fallback two"}"##,
        );
        assert!(text.contains("fallback two"), "{text}");
    }

    #[test]
    fn notebook_detection_requires_a_cell_list() {
        assert!(looks_like_notebook("{\"cells\":[],\"nbformat\":4}"));
        assert!(!looks_like_notebook("{\"nbformat\":4}"));
        assert!(!looks_like_notebook("plain text"));
        assert!(!looks_like_notebook("{\"cells\":\"nope\"}"));
    }

    #[test]
    fn ansi_stripping_handles_csi_and_osc() {
        assert_eq!(strip_ansi("a\u{1b}[31mred\u{1b}[0mb"), "aredb");
        assert_eq!(strip_ansi("x\u{1b}]0;title\u{7}y"), "xy");
        assert_eq!(strip_ansi("plain"), "plain");
    }
}
