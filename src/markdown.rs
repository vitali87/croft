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
    /// Rows of cell texts while inside a table (row 0 is the header).
    table: Option<Vec<Vec<String>>>,
}

impl Renderer<'_> {
    fn inline_style(&self) -> Style {
        let mut style = Style::default().fg(FG);
        if let Some(level) = self.heading {
            style = style.add_modifier(Modifier::BOLD);
            style = match level {
                HeadingLevel::H1 => style
                    .fg(self.theme.accent())
                    .add_modifier(Modifier::UNDERLINED),
                HeadingLevel::H2 => style.fg(self.theme.accent()),
                HeadingLevel::H3 => style.fg(FG),
                _ => style.fg(DIM),
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
            style = style.fg(DIM).add_modifier(Modifier::ITALIC);
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
            self.cur
                .push(Span::styled("\u{258e} ", Style::default().fg(DIM)));
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
        let bar = Span::styled("\u{258e} ", Style::default().fg(self.theme.accent()));
        let (fr, fg_, fb) = self.theme.syntax().fg;
        let code_fg = Color::Rgb(fr, fg_, fb);
        let highlighted = kind.map(|k| {
            let bytes = text.as_bytes();
            let line_starts = compute_line_starts(bytes);
            highlight_text(self.registry, k, bytes, &line_starts)
        });
        for (i, line) in text.lines().enumerate() {
            let mut spans = vec![bar.clone()];
            match highlighted.as_ref().and_then(|h| h.get(i)) {
                Some(hi) if !hi.is_empty() => spans.extend(code_line_spans(line, hi, code_fg)),
                _ => spans.push(Span::styled(line.to_string(), Style::default().fg(code_fg))),
            }
            self.out.push(Line::from(spans));
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
                Style::default().fg(FG).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(FG)
            };
            for (i, width) in widths.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" \u{2502} ", Style::default().fg(DIM)));
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
                            Style::default().fg(DIM),
                        ));
                    }
                    sep.push(Span::styled(
                        "\u{2500}".repeat(*width),
                        Style::default().fg(DIM),
                    ));
                }
                self.out.push(Line::from(sep));
            }
        }
        self.out.push(Line::default());
    }
}

/// Render markdown `text` into styled preview lines.
pub fn render_markdown(
    text: &str,
    theme: Theme,
    registry: &mut LangRegistry,
) -> Vec<Line<'static>> {
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
        table: None,
    };
    for event in Parser::new_ext(text, options) {
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
                    r.begin_content();
                    let style = Style::default().fg(DIM).add_modifier(Modifier::ITALIC);
                    r.cur.push(Span::styled("\u{f03e} ", style));
                    // ponytail: images render as a labelled placeholder; true
                    // inline pictures need per-image Kitty placements, add
                    // when the overlay pipeline grows multi-image slots.
                    r.cur.push(Span::styled(format!("({dest_url}) "), style));
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
                TagEnd::Image => {}
                TagEnd::Table => r.end_table(),
                _ => {}
            },
            Event::Text(text) => {
                if let Some((_, buf)) = r.code_block.as_mut() {
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
                if let Some(rows) = r.table.as_mut() {
                    if let Some(cell) = rows.last_mut().and_then(|row| row.last_mut()) {
                        cell.push_str(&code);
                    }
                } else {
                    r.begin_content();
                    r.cur
                        .push(Span::styled(code.to_string(), Style::default().fg(CODE)));
                }
            }
            Event::SoftBreak => {
                let style = r.inline_style();
                r.push_text(" ", style);
            }
            Event::HardBreak => r.flush_line(),
            Event::Rule => {
                r.ensure_blank();
                r.out.push(Line::from(Span::styled(
                    "\u{2500}".repeat(RULE_COLS),
                    Style::default().fg(DIM),
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
    // the first real line.
    while r.out.first().is_some_and(|l| l.spans.is_empty()) {
        r.out.remove(0);
    }
    while r.out.last().is_some_and(|l| l.spans.is_empty()) {
        r.out.pop();
    }
    r.out
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
