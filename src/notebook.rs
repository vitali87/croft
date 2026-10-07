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
    // Rows and cells of the open table, if any.
    let mut table: Option<Vec<Vec<String>>> = None;
    let mut hidden = 0usize;
    let mut rest = html;
    let push = |md: &mut String, table: &mut Option<Vec<Vec<String>>>, text: &str| match table {
        None => md.push_str(text),
        // A cell is one line of a pipe table; text between a table's cells
        // is whitespace or junk.
        Some(t) => {
            if let Some(cell) = t.last_mut().and_then(|r| r.last_mut()) {
                cell.push_str(&text.replace('\n', " "));
            }
        }
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
        let mark = match (name.as_str(), closing) {
            ("table", false) => {
                table = Some(Vec::new());
                ""
            }
            ("table", true) => {
                if let Some(rows) = table.take() {
                    md.push_str("\n\n");
                    md.push_str(&pipe_table(rows));
                    md.push('\n');
                }
                ""
            }
            ("tr", false) => {
                if let Some(t) = table.as_mut() {
                    t.push(Vec::new());
                }
                ""
            }
            ("td" | "th", false) => {
                if let Some(row) = table.as_mut().and_then(|t| t.last_mut()) {
                    row.push(String::new());
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
    if let Some(rows) = table.take() {
        md.push_str("\n\n");
        md.push_str(&pipe_table(rows));
    }
    md.trim().to_string()
}

/// The level of a heading tag name (`h1`..`h6`).
fn heading_level(name: &str) -> Option<usize> {
    match name.as_bytes() {
        [b'h', n @ b'1'..=b'6'] => Some((n - b'0') as usize),
        _ => None,
    }
}

/// An HTML text run as Markdown text: entities decoded, whitespace
/// collapsed as a browser does, Markdown's own characters escaped.
fn html_text(raw: &str) -> String {
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
