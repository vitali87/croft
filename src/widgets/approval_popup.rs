//! The agent edit approval popup (#347): the proposal at the head of the
//! queue as a unified diff, with approve / deny / deny-with-reason.

use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::agent_approval::{ApprovalUi, Pending, diff_rows};
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

pub fn render(
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    head: &Pending,
    ui: &ApprovalUi,
    pending: usize,
    root: &Path,
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
    let body = Rect {
        height: inner.height - 1,
        ..inner
    };
    let footer = Rect {
        y: inner.y + inner.height - 1,
        height: 1,
        ..inner
    };
    let rows = diff_rows(&head.proposal);
    let lines: Vec<Line> = rows
        .iter()
        .skip(ui.scroll)
        .take(body.height as usize)
        .map(|(tag, text)| {
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
            Line::from(Span::styled(shown, Style::default().fg(fg)))
        })
        .collect();
    Paragraph::new(lines).render(body, buf);
    let key = Style::default().fg(accent).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(theme.ui(Color::Rgb(0x9a, 0xa4, 0xb2)));
    let hint = match &ui.reason {
        Some(reason) => Line::from(vec![
            Span::styled("Reason: ", key),
            Span::raw(format!("{reason}\u{258f}")),
            Span::styled("   Enter send · Esc back", dim),
        ]),
        None => Line::from(vec![
            Span::styled("Enter", key),
            Span::styled(" approve   ", dim),
            Span::styled("Esc", key),
            Span::styled(" deny   ", dim),
            Span::styled("r", key),
            Span::styled(" deny with a reason   ", dim),
            Span::styled("↑↓", key),
            Span::styled(" scroll", dim),
        ]),
    };
    Paragraph::new(hint).render(footer, buf);
}
