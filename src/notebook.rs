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
//! reserve overlay rows exactly like a Markdown picture. Croft runs no
//! kernels: the view is read-only truth about the file.

use crate::markdown::MdImage;
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

/// Render the notebook to preview lines + overlay images. `scratch`
/// receives decoded image outputs (one file per output, named by a
/// content hash so rebuilds reuse them).
pub fn render(
    text: &str,
    theme: Theme,
    registry: &mut crate::highlight::LangRegistry,
    base_dir: Option<&Path>,
    scratch: &Path,
) -> Option<(Vec<Line<'static>>, Vec<MdImage>)> {
    let doc: serde_json::Value = serde_json::from_str(text).ok()?;
    let cells = doc.get("cells")?.as_array()?;
    let lang = doc
        .pointer("/metadata/kernelspec/language")
        .and_then(|v| v.as_str())
        .unwrap_or("python")
        .to_string();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut images: Vec<MdImage> = Vec::new();
    let frame = Style::default()
        .fg(theme.ui(FRAME))
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(theme.ui(DIM));
    for cell in cells {
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
                let n = cell
                    .get("execution_count")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| String::from(" "));
                lines.push(Line::from(Span::styled(format!("In [{n}]:"), frame)));
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
                    render_output(output, scratch, &mut lines, &mut images, dim, theme);
                }
                lines.push(Line::default());
            }
            _ => {}
        }
    }
    Some((lines, images))
}

fn render_output(
    output: &serde_json::Value,
    scratch: &Path,
    lines: &mut Vec<Line<'static>>,
    images: &mut Vec<MdImage>,
    dim: Style,
    theme: Theme,
) {
    let otype = output
        .get("output_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match otype {
        "stream" => {
            let text = output.get("text").map(joined).unwrap_or_default();
            lines.extend(ansi_lines(&text, dim, theme, "  "));
        }
        "error" => {
            let err = Style::default().fg(theme.ui(ERR));
            for tl in output
                .get("traceback")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                lines.extend(ansi_lines(tl.as_str().unwrap_or(""), err, theme, "  "));
            }
        }
        "execute_result" | "display_data" => {
            let picture = [("image/png", "png"), ("image/jpeg", "jpg")]
                .iter()
                .find_map(|(mime, ext)| {
                    output
                        .get("data")
                        .and_then(|d| d.get(*mime))
                        .map(joined)
                        .filter(|s| !s.is_empty())
                        .map(|b64| (b64, *ext))
                });
            if let Some((b64, ext)) = picture {
                use base64::Engine as _;
                let compact: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(compact)
                    && let Ok(Some((px_w, px_h))) =
                        image::ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
                            .with_guessed_format()
                            .map(|r| r.into_dimensions().ok())
                    && (px_w as u64) * (px_h as u64) <= 64_000_000
                {
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    use std::hash::{Hash as _, Hasher as _};
                    bytes.hash(&mut hasher);
                    let name = format!("nb-{:016x}.{ext}", hasher.finish());
                    let path = scratch.join(name);
                    let ok = write_scratch_no_follow(scratch, &path, &bytes);
                    if ok {
                        let rows = ((px_h as f32 / px_w.max(1) as f32) * 72.0 / 2.0)
                            .round()
                            .clamp(3.0, 18.0) as u16;
                        let first_line = lines.len();
                        for _ in 0..rows {
                            lines.push(Line::default());
                        }
                        images.push(MdImage {
                            first_line,
                            rows,
                            path,
                        });
                        return;
                    }
                }
            }
            let text = strip_ansi(
                &output
                    .get("data")
                    .and_then(|d| d.get("text/plain"))
                    .map(joined)
                    .unwrap_or_default(),
            );
            for l in text.lines() {
                lines.push(Line::from(Span::styled(format!("  {l}"), dim)));
            }
        }
        _ => {}
    }
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
        let (lines, images) =
            render(&doc, Theme::BLACK, &mut reg, None, tmp.path()).expect("parses");
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
        let (lines, images) =
            render(&doc, Theme::BLACK, &mut reg, None, tmp.path()).expect("parses");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].rows, 18);
        assert!(images[0].path.is_file(), "decoded png written to scratch");
        for i in 0..images[0].rows as usize {
            assert!(lines[images[0].first_line + i].spans.is_empty());
        }
        // Rebuild reuses the SAME hash-named file.
        let (_, again) = render(&doc, Theme::BLACK, &mut reg, None, tmp.path()).unwrap();
        assert_eq!(again[0].path, images[0].path);
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
