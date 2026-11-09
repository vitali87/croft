use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

use std::cell::OnceCell;

use crate::highlight::{HiSpan, LangKind};
use crate::widgets::surface;

const MIN_WIDTH: u16 = 20;
/// Compact tooltips (single-label button hints) shrink to fit their text
/// instead of reserving the 20-cell minimum an LSP type signature wants, so a
/// one-word hint like "Refresh" reads as a snug chip rather than a wide box.
const COMPACT_MIN_WIDTH: u16 = 3;
const MAX_WIDTH: u16 = 80;
/// The body's row floor. A taller screen lends a long hover up to half its
/// height (#1254); past that the popup scrolls and its border counts the
/// rows it hides.
const MAX_HEIGHT: u16 = 16;

pub struct HoverPopup {
    pub lines: Vec<String>,
    pub anchor: (u16, u16),
    /// Black theme: wear the orange→green gradient border instead of the
    /// legacy bright-blue. Set by the app before render from `popup_gradient`.
    pub theme: crate::theme::Theme,
    /// Shrink-to-fit width for short button hints. Cleared (false) for the
    /// LSP/diagnostic hover, which wants the wider minimum for readability.
    pub compact: bool,
    /// Wrapped rows scrolled off the top (#1254).
    pub scroll: u16,
    /// Where the popup last painted, so the mouse can tell it is over it.
    pub last_area: Rect,
    /// Whether the hover's last section is a language server's account of
    /// a symbol (see [`HoverPopup::about_symbol`]).
    symbol: bool,
    /// The code language the symbol's signature is coloured in; `None` for
    /// a file croft has no grammar for, whose signature takes the body ink.
    lang: Option<LangKind>,
    /// What each line is on a `surfaces` card, read the first time one is
    /// painted, so a frame neither re-reads the hover nor re-highlights its
    /// signature (and a boxed theme never pays for either).
    card: OnceCell<Vec<CardLine>>,
}

impl HoverPopup {
    pub fn new(text: String, anchor: (u16, u16)) -> Self {
        Self {
            lines: text.lines().map(|s| s.to_string()).collect(),
            anchor,
            theme: crate::theme::Theme::default(),
            compact: false,
            scroll: 0,
            last_area: Rect::default(),
            symbol: false,
            lang: None,
            card: OnceCell::new(),
        }
    }

    /// Mark the hover's last section (all of it, or what follows a
    /// problem's message and the divider) as a language server's account
    /// of a symbol in a `lang` file. A `surfaces` card then reads it the way
    /// VS Code lays a hover out: the module path quiet, the signature in
    /// the code's own colours, a hairline, then the docs. Other popups (a
    /// problem alone, a debug value, a terminal note) keep the plain ramp.
    pub fn about_symbol(mut self, lang: Option<LangKind>) -> Self {
        self.symbol = true;
        self.lang = lang;
        self.card = OnceCell::new();
        self
    }

    /// Scroll by `delta` wrapped rows, held between the first row and the
    /// one that puts the last line on the bottom row of `last_area`.
    pub fn scroll_by(&mut self, delta: i32) {
        let max = self.max_scroll(self.last_area);
        self.scroll = (i32::from(self.scroll) + delta).clamp(0, i32::from(max)) as u16;
    }

    /// The body, wrapped as it is drawn; `area_for` measures this same
    /// paragraph, so the box always has the rows the text takes.
    fn paragraph(&self) -> Paragraph<'_> {
        let text = Text::from(
            self.lines
                .iter()
                .map(|l| Line::from(l.as_str()))
                .collect::<Vec<_>>(),
        );
        Paragraph::new(text).wrap(Wrap { trim: false })
    }

    /// Rows the text takes once wrapped to `inner_w` cells.
    fn wrapped_rows(&self, inner_w: u16) -> u16 {
        let rows = self.paragraph().line_count(inner_w.max(1));
        u16::try_from(rows).unwrap_or(u16::MAX)
    }

    /// How far a popup painted in `area` can scroll.
    fn max_scroll(&self, area: Rect) -> u16 {
        if area.width < 3 || area.height < 3 {
            return 0;
        }
        let shown = area.height - 2;
        self.wrapped_rows(area.width - 2).saturating_sub(shown)
    }

    /// A button-hint tooltip: same bordered box, but width hugs the label.
    pub fn new_compact(text: String, anchor: (u16, u16)) -> Self {
        Self {
            compact: true,
            ..Self::new(text, anchor)
        }
    }

    fn content_width(&self) -> u16 {
        self.lines
            .iter()
            .map(|l| l.chars().count() as u16)
            .max()
            .unwrap_or(0)
    }

    pub fn area_for(&self, viewport: Rect) -> Rect {
        let min_width = if self.compact {
            COMPACT_MIN_WIDTH
        } else {
            MIN_WIDTH
        };
        let width = self
            .content_width()
            .saturating_add(2)
            .clamp(min_width, MAX_WIDTH);
        let inner_w = width.saturating_sub(2).max(1);
        let max_body = (viewport.height / 2).max(MAX_HEIGHT);
        let body = self.wrapped_rows(inner_w).clamp(1, max_body);
        let height = body.saturating_add(2);
        let (cx, cy) = self.anchor;
        let mut x = cx;
        let mut y = if cy >= height {
            cy - height
        } else {
            cy.saturating_add(1)
        };
        if x.saturating_add(width) > viewport.right() {
            x = viewport.right().saturating_sub(width);
        }
        if x < viewport.x {
            x = viewport.x;
        }
        if y.saturating_add(height) > viewport.bottom() {
            y = viewport.bottom().saturating_sub(height);
        }
        if y < viewport.y {
            y = viewport.y;
        }
        Rect {
            x,
            y,
            width: width.min(viewport.width.saturating_sub(x.saturating_sub(viewport.x))),
            height: height.min(viewport.height.saturating_sub(y.saturating_sub(viewport.y))),
        }
    }
}

impl Widget for &HoverPopup {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if let Some(t) = surface::tones(self.theme) {
            if self.compact && self.lines.len() == 1 && area.height == 3 {
                paint_pill(buf, area, &self.lines[0], t);
            } else {
                self.render_card(area, buf, t);
            }
            return;
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x21, 0x2a))));
        let max = self.max_scroll(area);
        let scroll = self.scroll.min(max);
        let para = self
            .paragraph()
            .block(block)
            .style(Style::default().fg(self.theme.ui(Color::Rgb(0xd0, 0xd6, 0xe0))))
            .scroll((scroll, 0));
        Widget::render(Clear, area, buf);
        para.render(area, buf);
        if self.theme.framed_overlays() {
            crate::widgets::surface::paint_overlay_frame(buf, area, self.theme);
        }
        // Painted after the gradient, which redraws the border glyphs.
        let marker = Style::default().fg(self.theme.ui(Color::Rgb(0xd0, 0xd6, 0xe0)));
        if scroll > 0 {
            paint_border_note(buf, area, area.y, '↑', scroll, marker);
        }
        if max > scroll {
            let y = area.bottom().saturating_sub(1);
            paint_border_note(buf, area, y, '↓', max - scroll, marker);
        }
    }
}

impl HoverPopup {
    /// The hover as a `surfaces` theme draws it: a quiet card (a hover never
    /// takes the keyboard, so its edge carries no accent lip) on the raised
    /// tone, with a type ramp instead of one grey (see [`CardLine`]), and
    /// the section divider a rule in the rim across the whole card. One
    /// styled line per text line, so the wrapping `area_for` measures is
    /// the wrapping drawn. The card's shadow keeps off the anchor's row: a
    /// hover placed just above a symbol must not dim the symbol.
    fn render_card(&self, area: Rect, buf: &mut Buffer, t: surface::Tones) {
        let inner_w = area.width.saturating_sub(2);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.rim()))
            .style(Style::default().bg(t.raised()));
        let max = self.max_scroll(area);
        let scroll = self.scroll.min(max);
        let roles = self
            .card
            .get_or_init(|| read_card(&self.lines, self.symbol, self.lang));
        let para = Paragraph::new(Text::from(card_lines(&self.lines, roles, t, inner_w)))
            .wrap(Wrap { trim: false })
            .block(block)
            .style(Style::default().fg(t.text()))
            .scroll((scroll, 0));
        Widget::render(Clear, area, buf);
        para.render(area, buf);
        surface::paint_anchored_frame(buf, area, self.theme, self.anchor.1, true);
        // A divider row carries on through the card's side edges, so the
        // rule meets the rim instead of stopping a cell short of it.
        if area.width >= 3 {
            let (left, right) = (area.x, area.right() - 1);
            for y in area.y + 1..area.bottom().saturating_sub(1) {
                let rule = (left + 1..right).all(|x| {
                    let cell = &buf[(x, y)];
                    cell.symbol() == "─" && cell.fg == t.rim()
                });
                if rule {
                    for x in [left, right] {
                        buf[(x, y)].set_symbol("─").set_fg(t.rim());
                    }
                }
            }
        }
        let marker = Style::default().fg(t.muted());
        if scroll > 0 {
            paint_border_note(buf, area, area.y, '↑', scroll, marker);
        }
        if max > scroll {
            let y = area.bottom().saturating_sub(1);
            paint_border_note(buf, area, y, '↓', max - scroll, marker);
        }
    }
}

/// The rule the app puts between a diagnostic and the language hover
/// (`compose_hover`); a `surfaces` card draws it across its whole width.
const DIVIDER: &str = "────────";

/// Body prose on a `surfaces` card: between the body ink and the muted one,
/// so the signature above it leads and the docs still read at length.
pub(crate) fn prose_ink(t: surface::Tones) -> Color {
    crate::gradient::rgb_color(crate::gradient::lerp_rgb(t.muted, t.text, 0.5))
}

/// What one line of a `surfaces` hover card is, and so which ink it takes.
#[derive(Debug, Clone)]
enum CardLine {
    /// A section's first line outside a code fence (a problem's message,
    /// a debug value): the strong ink.
    Headline,
    /// Documentation and other running text: the prose ink.
    Prose,
    /// A Markdown code fence's marker line: faint.
    Fence,
    /// Code: a fenced block, or a symbol's signature, coloured by `spans`
    /// over the line's bytes; with none, the body ink.
    Code(Vec<HiSpan>),
    /// The module or crate a symbol lives in, above its signature: muted,
    /// so the signature leads.
    Path,
    /// The rule between a problem and the symbol's hover, across the card
    /// and into its edges.
    Divider,
    /// A blank line between a symbol's signature and its docs (or a
    /// Markdown `---`) drawn as a hairline of `glyph` in the rim, inset from
    /// the edges so it reads as a pause inside the card, not a seam.
    Rule(&'static str),
}

/// Whether `line` opens or closes a Markdown code fence.
fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// Whether `line` is blank (a paragraph break).
fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Whether `line` reads as the module path a language server puts above a
/// signature (`wordcount`, `core::str`, `std::collections::hash_map`): one
/// word of identifier characters joined by `::`, `.` or `/`. A signature
/// always has a space or a bracket, so it never passes.
fn is_path_line(line: &str) -> bool {
    let l = line.trim();
    !l.is_empty()
        && l.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.' | '/' | '-' | '@'))
}

/// How the lines of a hover read on a `surfaces` card. Every section but a
/// symbol's (a problem's message, a debug value, a note) is a headline and
/// prose, its fenced code in the body ink; with `symbol`, the last section
/// (after the last divider) is read by [`read_symbol`].
fn read_card(lines: &[String], symbol: bool, lang: Option<LangKind>) -> Vec<CardLine> {
    let symbol_from = if symbol {
        lines
            .iter()
            .rposition(|l| l == DIVIDER)
            .map_or(0, |i| i + 1)
    } else {
        lines.len()
    };
    let mut out = Vec::with_capacity(lines.len());
    let mut in_fence = false;
    let mut headline = true;
    for l in &lines[..symbol_from] {
        out.push(if l == DIVIDER {
            headline = true;
            CardLine::Divider
        } else if is_fence(l) {
            in_fence = !in_fence;
            headline = false;
            CardLine::Fence
        } else if in_fence {
            CardLine::Code(Vec::new())
        } else if headline && !is_blank(l) {
            headline = false;
            CardLine::Headline
        } else {
            CardLine::Prose
        });
    }
    out.extend(read_symbol(&lines[symbol_from..], lang));
    out
}

/// A language server's hover about a symbol, read as path, signature and
/// docs: Markdown when it has code fences, else the plain text croft asks
/// servers for (it declares no Markdown hover support).
fn read_symbol(lines: &[String], lang: Option<LangKind>) -> Vec<CardLine> {
    if lines.iter().any(|l| is_fence(l)) {
        read_fenced_symbol(lines, lang)
    } else {
        read_plain_symbol(lines, lang)
    }
}

/// A plain-text symbol hover. Servers lay it out as paragraphs: an
/// optional one-line module path, the signature, then the docs. Where a
/// server's Markdown had a `---` rule (rust-analyzer between signature and
/// docs), its plain text leaves two blank lines, so the paragraphs before
/// the first double gap are the header; without one, the header is the
/// first paragraph, or a path-like line and the paragraph after it. The
/// gap after the header becomes the hairline, centred in it.
fn read_plain_symbol(lines: &[String], lang: Option<LangKind>) -> Vec<CardLine> {
    let mut roles = vec![CardLine::Prose; lines.len()];
    // Paragraphs as (first line, end, blank lines before it).
    let mut paras: Vec<(usize, usize, usize)> = Vec::new();
    let mut blanks = 0;
    for (i, l) in lines.iter().enumerate() {
        if is_blank(l) {
            blanks += 1;
            continue;
        }
        match paras.last_mut() {
            Some(p) if p.1 == i => p.1 = i + 1,
            _ => paras.push((i, i + 1, blanks)),
        }
        blanks = 0;
    }
    let Some(&(first, first_end, _)) = paras.first() else {
        return roles;
    };
    let one_line = first_end - first == 1;
    let header = match paras.iter().skip(1).position(|p| p.2 >= 2) {
        Some(k) => k + 1,
        None if one_line && paras.len() > 1 && paras[1].2 == 1 && is_path_line(&lines[first]) => 2,
        None => 1,
    };
    let path = header > 1 && one_line;
    for (n, &(start, end, _)) in paras[..header].iter().enumerate() {
        if n == 0 && path {
            roles[start] = CardLine::Path;
            continue;
        }
        for (i, spans) in (start..end).zip(highlight_block(&lines[start..end], lang)) {
            roles[i] = CardLine::Code(spans);
        }
    }
    if let Some(&(docs, _, gap)) = paras.get(header).filter(|p| p.2 > 0) {
        // An odd gap has a middle row for a centred line; an even one puts
        // a bottom-hugging eighth block on the row above the middle, so the
        // line sits on the boundary halfway between signature and docs.
        let (row, glyph) = if gap % 2 == 1 {
            (docs - gap + gap / 2, "\u{2500}")
        } else {
            (docs - gap + gap / 2 - 1, "\u{2581}")
        };
        roles[row] = CardLine::Rule(glyph);
    }
    roles
}

/// A Markdown symbol hover: fenced blocks in their fence's language (the
/// file's when the fence names none), fence markers faint, `---` a
/// hairline, the rest prose. A first block of one path-like line followed
/// straight by another block is the module path.
fn read_fenced_symbol(lines: &[String], lang: Option<LangKind>) -> Vec<CardLine> {
    let mut roles = Vec::with_capacity(lines.len());
    let mut open: Option<(usize, Option<LangKind>)> = None;
    let mut blocks: Vec<(usize, usize, Option<LangKind>)> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let trimmed = l.trim_start();
        if let Some(info) = trimmed.strip_prefix("```") {
            match open.take() {
                Some((start, kind)) => blocks.push((start, i, kind)),
                None => open = Some((i + 1, crate::highlight::lang_for_fence(info).or(lang))),
            }
            roles.push(CardLine::Fence);
        } else if open.is_some() {
            roles.push(CardLine::Code(Vec::new()));
        } else if matches!(trimmed.trim_end(), "---" | "***" | "___") {
            roles.push(CardLine::Rule("\u{2500}"));
        } else {
            roles.push(CardLine::Prose);
        }
    }
    // A fence left open runs to the end of the hover.
    if let Some((start, kind)) = open {
        blocks.push((start, lines.len(), kind));
    }
    for &(start, end, kind) in &blocks {
        for (i, spans) in (start..end).zip(highlight_block(&lines[start..end], kind)) {
            roles[i] = CardLine::Code(spans);
        }
    }
    if let [(start, end, _), (next, _, _), ..] = blocks[..]
        && end - start == 1
        && is_path_line(&lines[start])
        && lines[end + 1..next - 1].iter().all(|l| is_blank(l))
    {
        roles[start] = CardLine::Path;
    }
    roles
}

/// Syntax spans for a block of code lines in `lang`, one list per line;
/// all empty when the language is unknown, so the block keeps the body ink.
/// A hover's block is a few lines, so one grammar pass is cheap, and the
/// card caches the result.
fn highlight_block(lines: &[String], lang: Option<LangKind>) -> Vec<Vec<HiSpan>> {
    let Some(kind) = lang else {
        return vec![Vec::new(); lines.len()];
    };
    let text = lines.join("\n");
    let starts = crate::highlight::compute_line_starts(text.as_bytes());
    let mut registry = crate::highlight::LangRegistry::new();
    let mut spans = crate::highlight::highlight_text(&mut registry, kind, text.as_bytes(), &starts);
    spans.resize(lines.len(), Vec::new());
    spans
}

/// The hover's `lines` styled for a `surfaces` card `inner_w` cells wide,
/// each as its [`CardLine`] says. A divider grows to the card's width and a
/// hairline to one cell short of it, so each still takes exactly one row.
fn card_lines(
    lines: &[String],
    roles: &[CardLine],
    t: surface::Tones,
    inner_w: u16,
) -> Vec<Line<'static>> {
    let ink = |l: &str, fg: Color| Line::from(Span::styled(l.to_string(), Style::default().fg(fg)));
    lines
        .iter()
        .zip(roles)
        .map(|(l, role)| match role {
            CardLine::Divider => ink(&"─".repeat(usize::from(inner_w.max(1))), t.rim()),
            CardLine::Rule(glyph) => ink(
                &glyph.repeat(usize::from(inner_w.saturating_sub(1))),
                t.rim(),
            ),
            CardLine::Fence => ink(l, t.faint()),
            CardLine::Code(spans) if !spans.is_empty() => {
                Line::from(crate::markdown::code_line_spans(l, spans, t.text()))
            }
            CardLine::Code(_) => ink(l, t.text()),
            CardLine::Headline => ink(l, t.strong()),
            CardLine::Path => ink(l, t.muted()),
            CardLine::Prose => ink(l, prose_ink(t)),
        })
        .collect()
}

/// A one-line tooltip on a `surfaces` theme: a pill on the raised tone with
/// half-block caps, on the middle row of the 3-row `area` that placement and
/// the hit tests measure. The rows above and below are left as they are, so
/// a hint reads as a label beside what it names rather than a boxed card.
fn paint_pill(buf: &mut Buffer, area: Rect, label: &str, t: surface::Tones) {
    let area = area.intersection(buf.area);
    if area.width < 3 || area.height < 2 {
        return;
    }
    let y = area.y + 1;
    let fill = t.raised();
    let body = Rect::new(area.x + 1, y, area.width - 2, 1);
    Widget::render(Clear, body, buf);
    buf.set_style(body, Style::default().bg(fill));
    buf.set_stringn(
        body.x,
        y,
        label,
        usize::from(body.width),
        Style::default().fg(t.text()).bg(fill),
    );
    // The caps keep the background beneath them, so the pill's ends round
    // off against whatever surface the hint floats over.
    for (x, cap) in [(area.x, "\u{2590}"), (area.right() - 1, "\u{258c}")] {
        let cell = &mut buf[(x, y)];
        cell.set_symbol(cap);
        cell.set_fg(fill);
        cell.modifier = ratatui::style::Modifier::empty();
    }
}

/// Write ` arrow count more lines ` into the border row `y`, right-aligned
/// inside the corners, falling back to ` arrow count ` on a narrow popup.
fn paint_border_note(buf: &mut Buffer, area: Rect, y: u16, arrow: char, count: u16, style: Style) {
    let long = format!(" {arrow} {count} more lines ");
    let short = format!(" {arrow} {count} ");
    let Some(label) = [long, short]
        .into_iter()
        .find(|l| area.width >= l.chars().count() as u16 + 4)
    else {
        return;
    };
    let x = area.right() - 2 - label.chars().count() as u16;
    buf.set_string(x, y, label, style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_splits_text_into_lines() {
        let p = HoverPopup::new("line one\nline two".into(), (0, 0));
        assert_eq!(
            p.lines,
            vec!["line one".to_string(), "line two".to_string()]
        );
    }

    #[test]
    fn compact_popup_hugs_a_short_label_instead_of_the_wide_minimum() {
        let vp = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let wide = HoverPopup::new("Refresh".into(), (10, 12)).area_for(vp);
        let compact = HoverPopup::new_compact("Refresh".into(), (10, 12)).area_for(vp);
        assert_eq!(
            wide.width, MIN_WIDTH,
            "default hover keeps the 20-cell floor"
        );
        assert_eq!(
            compact.width,
            "Refresh".len() as u16 + 2,
            "compact hint hugs the label plus its two border cells"
        );
        assert!(
            compact.width < wide.width,
            "compact must be narrower than the LSP-sized minimum"
        );
    }

    #[test]
    fn area_for_is_nonempty_and_within_viewport() {
        let p = HoverPopup::new("fn foo(x: i32) -> i32".into(), (10, 12));
        let vp = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let a = p.area_for(vp);
        assert!(a.width > 0 && a.height > 0, "popup must have a real size");
        assert!(
            a.x >= vp.x && a.right() <= vp.right(),
            "within horizontal bounds"
        );
        assert!(
            a.y >= vp.y && a.bottom() <= vp.bottom(),
            "within vertical bounds"
        );
    }

    #[test]
    fn area_for_sits_above_the_anchor_when_there_is_room() {
        let p = HoverPopup::new("sig".into(), (10, 15));
        let vp = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 40,
        };
        let a = p.area_for(vp);
        assert!(
            a.bottom() <= 15,
            "with room above, the popup ends at or above the anchor row, got bottom {}",
            a.bottom()
        );
    }

    #[test]
    fn area_for_flips_below_when_anchor_is_near_the_top() {
        let p = HoverPopup::new("a\nb\nc\nd".into(), (10, 0));
        let vp = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 40,
        };
        let a = p.area_for(vp);
        assert!(
            a.y >= 1,
            "no room above row 0, so the popup drops below the anchor, got y {}",
            a.y
        );
    }

    #[test]
    fn area_for_clamps_to_right_edge() {
        let p = HoverPopup::new(
            "a very long signature line that would overflow the viewport".into(),
            (78, 10),
        );
        let vp = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let a = p.area_for(vp);
        assert!(
            a.right() <= vp.right(),
            "must not spill past the right edge"
        );
    }

    /// #1526: the popup renders with word wrap, so a word that does not fit
    /// moves whole to the next row. Sized by characters (152 / 78 = 2 rows)
    /// it got one row short of the 3 word wrap needs, and the last words
    /// were clipped without a sign.
    #[test]
    fn area_for_reserves_the_rows_word_wrap_needs() {
        let line = format!("{} {} {}", "a".repeat(60), "b".repeat(60), "c".repeat(30));
        let p = HoverPopup::new(line, (10, 30));
        let vp = Rect::new(0, 0, 120, 40);
        let a = p.area_for(vp);
        assert_eq!(a.width, MAX_WIDTH);
        assert_eq!(a.height, 3 + 2, "three wrapped rows plus the border");
        let mut buf = Buffer::empty(vp);
        (&p).render(a, &mut buf);
        let text: String = (a.y..a.bottom())
            .map(|y| {
                (a.x..a.right())
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains(&"c".repeat(30)),
            "the last word is shown:\n{text}"
        );
    }

    /// #1526: the debug hover's `name = '<long value>': type` puts the
    /// value on its own rows after `name =`; the type at the end shows.
    #[test]
    fn a_long_debug_value_keeps_its_type_in_view() {
        let value = format!("eyJhbGciOiJIUzI1NiJ9.{}", "a".repeat(117));
        let p = HoverPopup::new(format!("token = '{value}': str"), (10, 30));
        let vp = Rect::new(0, 0, 120, 40);
        let a = p.area_for(vp);
        let mut buf = Buffer::empty(vp);
        (&p).render(a, &mut buf);
        let last_row: String = (a.x..a.right())
            .map(|x| buf[(x, a.bottom() - 2)].symbol())
            .collect();
        assert!(last_row.contains("': str"), "{last_row:?}");
    }

    /// #1526 negative: text that breaks evenly gets no extra rows. One long
    /// word splits at the width exactly as before, short lines take one row
    /// each, and the cap (half the viewport, at least 16 rows, past which
    /// the popup scrolls) still holds.
    #[test]
    fn area_for_adds_no_rows_text_does_not_need() {
        let vp = Rect::new(0, 0, 120, 60);
        let word = HoverPopup::new("x".repeat(156), (10, 50)).area_for(vp);
        assert_eq!(word.height, 2 + 2, "156 chars in 78-wide rows");
        let short = HoverPopup::new("fn a()\nfn b()\nfn c()".into(), (10, 50)).area_for(vp);
        assert_eq!(short.height, 3 + 2);
        let long = HoverPopup::new(vec!["word"; 40].join("\n"), (10, 50)).area_for(vp);
        assert_eq!(long.height, (vp.height / 2).max(MAX_HEIGHT) + 2);
    }

    #[test]
    fn render_clears_editor_text_under_the_popup() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        let sentinel = '#';
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_char(sentinel);
            }
        }
        let p = HoverPopup::new("fn foo()".into(), (0, 0));
        (&p).render(area, &mut buf);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let ch = buf[(x, y)].symbol().chars().next().unwrap_or(' ');
                assert_ne!(ch, sentinel, "cell ({x},{y}) still shows editor text");
            }
        }
    }

    fn numbered(n: usize) -> String {
        (1..=n)
            .map(|i| format!("line {i} of the hover docs"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn row_text(buf: &Buffer, area: Rect, y: u16) -> String {
        (area.left()..area.right())
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    fn painted(p: &HoverPopup, vp: Rect) -> (Rect, Buffer) {
        let area = p.area_for(vp);
        let mut buf = Buffer::empty(vp);
        p.render(area, &mut buf);
        (area, buf)
    }

    const VP: Rect = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };

    #[test]
    fn a_hover_taller_than_its_box_says_how_many_lines_are_hidden() {
        let p = HoverPopup::new(numbered(40), (10, 23));
        let (area, buf) = painted(&p, VP);
        let shown = area.height - 2;
        let bottom = row_text(&buf, area, area.bottom() - 1);
        assert!(
            bottom.contains(&format!("↓ {} more lines", 40 - shown)),
            "the bottom border must count the cut lines, got {bottom:?}"
        );
    }

    #[test]
    fn scrolling_the_hover_shows_later_lines_and_what_is_above() {
        let mut p = HoverPopup::new(numbered(40), (10, 23));
        p.last_area = p.area_for(VP);
        p.scroll_by(5);
        assert_eq!(p.scroll, 5);
        let (area, buf) = painted(&p, VP);
        let first = row_text(&buf, area, area.y + 1);
        assert!(
            first.contains("line 6 "),
            "row one shows line 6, got {first:?}"
        );
        let top = row_text(&buf, area, area.y);
        assert!(top.contains("↑ 5 more lines"), "got {top:?}");
    }

    #[test]
    fn scrolling_stops_at_the_last_line() {
        let mut p = HoverPopup::new(numbered(40), (10, 23));
        p.last_area = p.area_for(VP);
        let shown = p.last_area.height - 2;
        p.scroll_by(1000);
        assert_eq!(p.scroll, 40 - shown, "the last line sits on the bottom row");
        let (area, buf) = painted(&p, VP);
        let last = row_text(&buf, area, area.bottom() - 2);
        assert!(last.contains("line 40 "), "got {last:?}");
        assert!(!row_text(&buf, area, area.bottom() - 1).contains("more lines"));
        p.scroll_by(-1000);
        assert_eq!(p.scroll, 0);
    }

    #[test]
    fn a_tall_screen_gives_long_docs_more_than_sixteen_rows() {
        let vp = Rect {
            x: 0,
            y: 0,
            width: 160,
            height: 45,
        };
        let p = HoverPopup::new(numbered(40), (10, 44));
        let area = p.area_for(vp);
        assert!(
            area.height - 2 > MAX_HEIGHT,
            "half of a 45-row screen is more than 16 rows, got {}",
            area.height - 2
        );
        assert!(area.height - 2 <= vp.height / 2);
    }

    #[test]
    fn a_hover_that_fits_has_no_marker_and_does_not_scroll() {
        let mut p = HoverPopup::new(numbered(5), (10, 23));
        p.last_area = p.area_for(VP);
        assert_eq!(p.last_area.height, 7, "five lines and two borders");
        p.scroll_by(3);
        assert_eq!(p.scroll, 0, "nothing to scroll to");
        let (area, buf) = painted(&p, VP);
        for y in area.top()..area.bottom() {
            assert!(!row_text(&buf, area, y).contains("more lines"));
        }
    }

    #[test]
    fn a_narrow_hover_keeps_the_count_without_the_words() {
        let narrow = (1..=40)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let p = HoverPopup::new(narrow, (10, 23));
        let (area, buf) = painted(&p, VP);
        assert_eq!(area.width, MIN_WIDTH);
        let bottom = row_text(&buf, area, area.bottom() - 1);
        assert!(bottom.contains(" ↓ 24 "), "got {bottom:?}");
    }

    fn loch() -> crate::theme::Theme {
        crate::theme::Theme::from_id("loch")
    }

    /// Croft Loch: a quiet card (rim, no accent lip, since a hover never
    /// takes the keyboard) on the raised tone; the headline in the strong
    /// ink, prose quieter, and the diagnostic divider a rule across the
    /// whole card.
    #[test]
    fn croft_loch_hover_is_a_quiet_card_with_a_type_ramp() {
        let theme = loch();
        let t = surface::tones(theme).unwrap();
        let text = format!("Error: bad\n{DIVIDER}\nfn foo() -> u8\ndocs here");
        let mut p = HoverPopup::new(text, (10, 20));
        p.theme = theme;
        let (area, buf) = painted(&p, VP);
        assert_eq!(buf[(area.x, area.y)].symbol(), "\u{2594}");
        assert_eq!(buf[(area.x, area.y)].fg, t.rim(), "no accent lip");
        assert_eq!(buf[(area.x + 1, area.y + 1)].fg, t.strong(), "headline");
        assert_eq!(buf[(area.x + 1, area.y + 1)].bg, t.raised());
        for x in area.x..area.right() {
            assert_eq!(buf[(x, area.y + 2)].symbol(), "─", "({x}) the rule spans");
            assert_eq!(buf[(x, area.y + 2)].fg, t.rim());
        }
        assert_eq!(
            buf[(area.x + 1, area.y + 3)].fg,
            t.strong(),
            "a new section"
        );
        assert_eq!(buf[(area.x + 1, area.y + 4)].fg, prose_ink(t));
    }

    /// Croft Loch: a one-line tooltip is a pill on the middle row of the
    /// same 3-row area (placement and hit tests unchanged), with half-block
    /// caps and nothing drawn above or below it.
    #[test]
    fn croft_loch_compact_tooltip_is_a_one_row_pill() {
        let theme = loch();
        let t = surface::tones(theme).unwrap();
        let mut p = HoverPopup::new_compact("Refresh".into(), (10, 12));
        p.theme = theme;
        let area = p.area_for(VP);
        assert_eq!((area.width, area.height), (9, 3));
        let mut buf = Buffer::empty(VP);
        buf.set_string(area.x, area.y, "#########", Style::default());
        p.render(area, &mut buf);
        let y = area.y + 1;
        assert_eq!(buf[(area.x, y)].symbol(), "\u{2590}");
        assert_eq!(buf[(area.x, y)].fg, t.raised());
        assert_eq!(buf[(area.right() - 1, y)].symbol(), "\u{258c}");
        assert_eq!(row_text(&buf, area, y), "\u{2590}Refresh\u{258c}");
        assert_eq!(buf[(area.x + 1, y)].bg, t.raised());
        assert_eq!(buf[(area.x + 1, y)].fg, t.text());
        assert_eq!(row_text(&buf, area, area.y), "#########", "row above kept");
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    /// rust-analyzer's plain-text hover: the crate, a blank line, the
    /// signature, two blank lines where its Markdown had a `---`, the docs.
    const RA_HOVER: &str =
        "wordcount\n\nfn word_counts(text: &str) -> usize\n\n\nCount how often each word appears.";

    /// A symbol's plain hover reads as path, signature, hairline, docs; the
    /// hairline sits on the boundary halfway between the signature and the
    /// docs (the bottom of the first of the two blank rows).
    #[test]
    fn a_plain_symbol_hover_reads_as_path_signature_and_docs() {
        let roles = read_card(&lines(RA_HOVER), true, Some(LangKind::Rust));
        assert!(matches!(roles[0], CardLine::Path));
        assert!(matches!(roles[1], CardLine::Prose));
        assert!(matches!(&roles[2], CardLine::Code(spans) if !spans.is_empty()));
        assert!(matches!(roles[3], CardLine::Rule("\u{2581}")));
        assert!(matches!(roles[4], CardLine::Prose));
        assert!(matches!(roles[5], CardLine::Prose));
        // One blank line between them: a centred line on it. No path line
        // (it has spaces), so the first paragraph is the signature.
        let roles = read_card(
            &lines("def count(s: str) -> int\n\nCount words."),
            true,
            Some(LangKind::Python),
        );
        assert!(matches!(&roles[0], CardLine::Code(_)));
        assert!(matches!(roles[1], CardLine::Rule("\u{2500}")));
        assert!(matches!(roles[2], CardLine::Prose));
        // A path-like line straight above a paragraph, with no docs.
        let roles = read_card(
            &lines("core::str\n\npub fn len(&self) -> usize"),
            true,
            None,
        );
        assert!(matches!(roles[0], CardLine::Path));
        assert!(
            matches!(&roles[2], CardLine::Code(spans) if spans.is_empty()),
            "no grammar: the body ink"
        );
        // A keyword's hover: the word, then the docs after the rule.
        let roles = read_card(&lines("fn\n\n\nA function."), true, Some(LangKind::Rust));
        assert!(matches!(&roles[0], CardLine::Code(_)), "not a path");
        assert!(matches!(roles[3], CardLine::Prose));
    }

    /// A Markdown symbol hover: its fences faint, the code in the fence's
    /// language, `---` a hairline, a one-line first block the path; a
    /// problem above the divider keeps the headline ramp.
    #[test]
    fn a_fenced_symbol_hover_and_a_problem_above_it() {
        let text = format!(
            "Error: bad\n{DIVIDER}\n```rust\nwordcount\n```\n\n```rust\nfn f() -> u8\n```\n---\nDocs."
        );
        let roles = read_card(&lines(&text), true, None);
        assert!(matches!(roles[0], CardLine::Headline));
        assert!(matches!(roles[1], CardLine::Divider));
        assert!(matches!(roles[2], CardLine::Fence));
        assert!(matches!(roles[3], CardLine::Path));
        assert!(matches!(&roles[7], CardLine::Code(spans) if !spans.is_empty()));
        assert!(matches!(roles[9], CardLine::Rule("\u{2500}")));
        assert!(matches!(roles[10], CardLine::Prose));
        // Without `about_symbol` the same text keeps the plain ramp.
        let roles = read_card(&lines(&text), false, None);
        assert!(matches!(roles[2], CardLine::Fence));
        assert!(matches!(&roles[7], CardLine::Code(spans) if spans.is_empty()));
        assert!(matches!(roles[10], CardLine::Prose));
    }

    /// Croft Loch: a symbol hover's card inks the path muted, colours the
    /// signature as code, rules a hairline in the rim one cell short of the
    /// card's width, and sets the docs in the prose ink; its size is what
    /// the plain text measures.
    #[test]
    fn croft_loch_symbol_hover_is_typeset() {
        let theme = loch();
        let t = surface::tones(theme).unwrap();
        let mut p = HoverPopup::new(RA_HOVER.into(), (10, 20)).about_symbol(Some(LangKind::Rust));
        p.theme = theme;
        let plain = HoverPopup::new(RA_HOVER.into(), (10, 20));
        let (area, buf) = painted(&p, VP);
        assert_eq!(area, plain.area_for(VP), "sized as before");
        let (x, y) = (area.x + 1, area.y + 1);
        assert_eq!(buf[(x, y)].fg, t.muted(), "path");
        assert!(row_text(&buf, area, y + 2).contains("fn word_counts(text: &str) -> usize"));
        assert_ne!(
            buf[(x, y + 2)].fg,
            buf[(x + 3, y + 2)].fg,
            "`fn` and `word_counts` in their own colours"
        );
        let inner_w = area.width - 2;
        for i in 0..inner_w - 1 {
            assert_eq!(buf[(x + i, y + 3)].symbol(), "\u{2581}");
            assert_eq!(buf[(x + i, y + 3)].fg, t.rim());
        }
        assert_eq!(buf[(x + inner_w - 1, y + 3)].symbol(), " ", "inset");
        assert_eq!(
            buf[(area.x, y + 3)].symbol(),
            "\u{258f}",
            "the edge is whole"
        );
        assert_eq!(buf[(x, y + 5)].fg, prose_ink(t), "docs");
    }

    /// Croft Loch: a hover placed just above the symbol it describes keeps
    /// its shadow off that line, so the symbol is not dimmed.
    #[test]
    fn croft_loch_hover_never_shadows_the_hovered_line() {
        let theme = loch();
        let t = surface::tones(theme).unwrap();
        let mut p = HoverPopup::new("fn foo() -> u8\nDocs.".into(), (10, 12));
        p.theme = theme;
        let area = p.area_for(VP);
        assert_eq!(area.bottom(), 12, "the card ends right above its line");
        let mut buf = Buffer::empty(VP);
        // A lit surface, so a shadow over it shows (black stays black).
        buf.set_style(VP, Style::default().fg(t.text()).bg(t.chrome()));
        p.render(area, &mut buf);
        for x in VP.left()..VP.right() {
            assert_eq!(buf[(x, 12)].bg, t.chrome(), "({x}, 12) not shaded");
            assert_eq!(buf[(x, 12)].fg, t.text());
        }
        assert_ne!(
            buf[(area.right(), area.y + 1)].bg,
            t.chrome(),
            "the side shadow still falls"
        );
    }

    #[test]
    fn a_short_screen_keeps_the_sixteen_row_floor() {
        let p = HoverPopup::new(numbered(40), (10, 23));
        assert_eq!(p.area_for(VP).height - 2, MAX_HEIGHT);
    }
}
