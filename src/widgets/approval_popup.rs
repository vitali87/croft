//! The agent edit approval popup (#347): the proposal at the head of the
//! queue as a unified diff, with approve / deny / deny-with-reason.

use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::agent_approval::{ApprovalUi, Pending, diff_rows_numbered};
use crate::theme::Theme;

/// The popup's box: most of the screen, so a real diff fits.
pub fn popup_rect(area: Rect) -> Rect {
    let width = (area.width.saturating_sub(4))
        .min(120)
        .max(area.width.min(40));
    let height = (area.height.saturating_sub(4)).max(area.height.min(8));
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// The title: who wants to do what to which file, and how many more wait.
pub fn title(head: &Pending, pending: usize, root: &Path) -> String {
    let path = &head.proposal.path;
    let shown = path.strip_prefix(root).unwrap_or(path).display();
    let verb = if head.proposal.before.is_none() {
        "create"
    } else {
        "edit"
    };
    let more = if pending > 1 {
        format!(" · {pending} pending")
    } else {
        String::new()
    };
    format!(" {} wants to {verb} {shown}{more} ", head.request.agent)
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    head: &Pending,
    ui: &ApprovalUi,
    pending: usize,
    root: &Path,
    check: Option<&crate::agent_approval::ProposalCheck>,
) {
    let rect = popup_rect(area);
    let accent = theme.ui(Color::Rgb(0xd7, 0x99, 0x21));
    Clear.render(rect, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))))
        .title(Span::styled(
            title(head, pending, root),
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.height < 2 {
        return;
    }
    let mut body = Rect {
        height: inner.height - 1,
        ..inner
    };
    // What the language server makes of the proposal (#347), above the
    // diff; the rows it names carry the problem at their end.
    let diagnostics = check.map(|c| c.diagnostics()).unwrap_or_default();
    if let Some(check) = check
        && body.height > 1
    {
        use crate::lsp::manager::DiagnosticSeverity as S;
        let color = if !check.heard() {
            Color::Rgb(0x9a, 0xa4, 0xb2)
        } else if diagnostics.iter().any(|d| d.severity == S::Error) {
            Color::Rgb(0xe5, 0x73, 0x73)
        } else if diagnostics.iter().any(|d| d.severity == S::Warning) {
            Color::Rgb(0xe5, 0xc0, 0x7b)
        } else {
            Color::Rgb(0x81, 0xc7, 0x84)
        };
        Paragraph::new(Line::from(Span::styled(
            check.summary(std::time::Instant::now()),
            Style::default()
                .fg(theme.ui(color))
                .add_modifier(Modifier::BOLD),
        )))
        .render(Rect { height: 1, ..body }, buf);
        body = Rect {
            y: body.y + 1,
            height: body.height - 1,
            ..body
        };
    }
    let footer = Rect {
        y: inner.y + inner.height - 1,
        height: 1,
        ..inner
    };
    let rows = diff_rows_numbered(&head.proposal);
    let lines: Vec<Line> = rows
        .iter()
        .skip(ui.scroll)
        .take(body.height as usize)
        .map(|(tag, text, new_line)| {
            let fg = match tag {
                '+' => theme.ui(Color::Rgb(0x81, 0xc7, 0x84)),
                '-' => theme.ui(Color::Rgb(0xe5, 0x73, 0x73)),
                '@' => theme.ui(Color::Rgb(0x6c, 0x7d, 0x9c)),
                _ => theme.ui(Color::Rgb(0xc5, 0xcd, 0xd9)),
            };
            let shown = if *tag == '@' {
                text.clone()
            } else {
                format!("{tag} {text}")
            };
            let mut spans = vec![Span::styled(shown, Style::default().fg(fg))];
            // The worst problem the server found on this proposed line.
            if let Some(d) = new_line.and_then(|n| {
                diagnostics.iter().find(|d| {
                    (d.start_line as usize..=d.end_line as usize).contains(&n)
                        && matches!(
                            d.severity,
                            crate::lsp::manager::DiagnosticSeverity::Error
                                | crate::lsp::manager::DiagnosticSeverity::Warning
                        )
                })
            }) {
                let color = match d.severity {
                    crate::lsp::manager::DiagnosticSeverity::Error => Color::Rgb(0xe5, 0x73, 0x73),
                    _ => Color::Rgb(0xe5, 0xc0, 0x7b),
                };
                spans.push(Span::styled(
                    format!(
                        "  \u{25c0} {}",
                        d.message.lines().next().unwrap_or_default()
                    ),
                    Style::default().fg(theme.ui(color)),
                ));
            }
            Line::from(spans)
        })
        .collect();
    Paragraph::new(lines).render(body, buf);
    // A `surfaces` theme says "press this" with the body ink against muted
    // words; the boxed themes keep the bold amber keys.
    let (key, dim) = match crate::widgets::surface::tones(theme) {
        Some(t) => (
            Style::default().fg(t.text()),
            Style::default().fg(t.muted()),
        ),
        None => (
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
            Style::default().fg(theme.ui(Color::Rgb(0x9a, 0xa4, 0xb2))),
        ),
    };
    let hint = match &ui.reason {
        Some(reason) => Line::from(vec![
            Span::styled("Reason: ", key),
            Span::raw(format!("{reason}\u{258f}")),
            Span::styled("   Enter send · Esc back", dim),
        ]),
        None if ui.target_dirty => Line::from(vec![
            Span::styled("Unsaved edits here: ", key),
            Span::styled("Enter", key),
            Span::styled(
                " merge them with the agent's (disk · yours · agent's)   ",
                dim,
            ),
            Span::styled("Esc", key),
            Span::styled(" deny   ", dim),
            Span::styled("r", key),
            Span::styled(" deny with a reason   ", dim),
            Span::styled("↑↓", key),
            Span::styled(" scroll", dim),
        ]),
        None => Line::from(vec![
            Span::styled("Enter", key),
            Span::styled(" approve   ", dim),
            Span::styled("Esc", key),
            Span::styled(" deny   ", dim),
            Span::styled("a", key),
            Span::styled(" approve all from this agent for 10 min   ", dim),
            Span::styled("r", key),
            Span::styled(" deny with a reason   ", dim),
            Span::styled("e", key),
            Span::styled(" edit, then approve   ", dim),
            Span::styled("↑↓", key),
            Span::styled(" scroll", dim),
        ]),
    };
    Paragraph::new(hint).render(footer, buf);
    paint_warning_card(buf, rect, theme, accent);
}

/// Codicon `warning` (U+EA6C), before a warning card's title.
const WARNING_GLYPH: char = '\u{ea6c}';

/// Restyle a warning or confirmation dialog drawn the boxed way in `rect`
/// (a line box in `tint` with the title as an inverted chip on its top
/// edge) as a `surfaces` card; a no-op on a boxed theme. The card keeps
/// the raised body, gets [`surface::paint_tinted_frame`]'s edge with the
/// lip in `tint`, and its title becomes bold `tint` text after a warning
/// glyph, the chip gone. The body's inks move onto the tones: plain and
/// white text to the body ink, greys to muted or body by their lightness,
/// the amber path to the strong ink, and the named key-prompt colours
/// (red, yellow, green) to the danger red, the amber and the accent, all
/// at normal weight. One pass over the dialog's cells, only while one is
/// open, so every confirm shares one look without restating its layout.
///
/// [`surface::paint_tinted_frame`]: crate::widgets::surface::paint_tinted_frame
pub fn paint_warning_card(buf: &mut Buffer, rect: Rect, theme: Theme, tint: Color) {
    use crate::gradient::rgb_color;
    use crate::widgets::surface;
    let Some(t) = surface::tones(theme) else {
        return;
    };
    let rect = rect.intersection(buf.area);
    if rect.width < 4 || rect.height < 3 {
        return;
    }
    let Color::Rgb(tr, tg, tb) = tint else {
        return;
    };
    // The chip's words, read back from the cells it filled.
    let mut title = String::new();
    for x in rect.x + 1..rect.right() - 1 {
        let cell = &mut buf[(x, rect.y)];
        if cell.bg == tint {
            title.push_str(cell.symbol());
            cell.set_symbol(" ");
            cell.set_style(Style::reset().bg(t.raised()));
        }
    }
    let raised = t.raised();
    let lum = |(r, g, b): (u8, u8, u8)| u16::from(r) + u16::from(g) + u16::from(b);
    let mid = (lum(t.muted) + lum(t.text)) / 2;
    for y in rect.y + 1..rect.bottom() - 1 {
        for x in rect.x + 1..rect.right() - 1 {
            let cell = &mut buf[(x, y)];
            let fg = match cell.fg {
                Color::Reset | Color::White | Color::Gray => t.text(),
                Color::Red | Color::LightRed => rgb_color((0xe7, 0x70, 0x70)),
                Color::Yellow | Color::LightYellow => rgb_color((0xe7, 0xa7, 0x3c)),
                Color::Green | Color::LightGreen => t.accent(),
                Color::Rgb(0xeb, 0xcb, 0x8b) => t.strong(),
                Color::Rgb(r, g, b) if r.max(g).max(b) - r.min(g).min(b) < 0x20 => {
                    if lum((r, g, b)) < mid {
                        t.muted()
                    } else {
                        t.text()
                    }
                }
                other => other,
            };
            cell.set_fg(fg);
            cell.set_bg(raised);
            cell.modifier.remove(Modifier::BOLD);
        }
    }
    surface::paint_tinted_frame(buf, rect, theme, (tr, tg, tb));
    let title = title.trim();
    if !title.is_empty() {
        let label = format!(" {WARNING_GLYPH} {title} ");
        let room = usize::from(rect.width.saturating_sub(3));
        buf.set_stringn(
            rect.x + 1,
            rect.y,
            label,
            room,
            Style::default()
                .fg(tint)
                .bg(raised)
                .add_modifier(Modifier::BOLD),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A confirm drawn the boxed way: a line box in `tint` with an inverted
    /// title chip, a white sentence and red / green key prompts.
    fn boxed_confirm(buf: &mut Buffer, rect: Rect, tint: Color) {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(tint))
            .style(Style::default().bg(Color::Rgb(0x15, 0x18, 0x19)))
            .title(Span::styled(
                " DROP STASH? ",
                Style::default()
                    .fg(Color::White)
                    .bg(tint)
                    .add_modifier(Modifier::BOLD),
            ))
            .render(rect, buf);
        let body = vec![
            Line::from(Span::styled(
                "Gone for good.",
                Style::default().fg(Color::White),
            )),
            Line::from(vec![
                Span::styled(
                    "[Y]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw("es   "),
                Span::styled("[N]", Style::default().fg(Color::Green)),
                Span::raw("o"),
            ]),
        ];
        let inner = Rect::new(rect.x + 2, rect.y + 1, rect.width - 4, rect.height - 2);
        Paragraph::new(body).render(inner, buf);
    }

    /// Croft Loch: the chip becomes bold tinted text after a warning glyph,
    /// the edge the tinted card frame, and the body's inks the tones.
    #[test]
    fn croft_loch_turns_a_boxed_confirm_into_a_tinted_card() {
        let theme = Theme::from_id("loch");
        let t = crate::widgets::surface::tones(theme).unwrap();
        let tint = Color::Rgb(0xe7, 0x70, 0x70);
        let rect = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(Rect::new(0, 0, 42, 8));
        boxed_confirm(&mut buf, rect, tint);
        paint_warning_card(&mut buf, rect, theme, tint);
        let top: String = (0..40).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(
            top.contains(&format!("{WARNING_GLYPH} DROP STASH?")),
            "{top}"
        );
        for x in 0..40 {
            assert_ne!(buf[(x, 0)].bg, tint, "({x}) no inverted chip");
        }
        assert_eq!(buf[(4, 0)].fg, tint);
        assert!(buf[(4, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(0, 0)].symbol(), "\u{2594}", "the card edge");
        assert_eq!(buf[(0, 0)].fg, tint, "the lip in the tint");
        assert_eq!(buf[(0, 3)].symbol(), "\u{258f}");
        assert_eq!(buf[(2, 1)].fg, t.text(), "the sentence");
        assert_eq!(buf[(3, 2)].symbol(), "Y");
        assert_eq!(buf[(3, 2)].fg, tint);
        assert!(!buf[(3, 2)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(11, 2)].symbol(), "N");
        assert_eq!(buf[(11, 2)].fg, t.accent());
        assert_eq!(buf[(5, 2)].fg, t.text(), "plain words");
    }

    /// A boxed theme's confirm is left exactly as it was drawn.
    #[test]
    fn a_boxed_theme_confirm_is_untouched() {
        let tint = Color::Rgb(0xe7, 0x70, 0x70);
        let rect = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(Rect::new(0, 0, 42, 8));
        boxed_confirm(&mut buf, rect, tint);
        let before = buf.clone();
        paint_warning_card(&mut buf, rect, Theme::BLACK, tint);
        assert_eq!(buf, before);
    }
}
