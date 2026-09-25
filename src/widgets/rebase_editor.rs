//! The interactive rebase modal (#620): one row per commit of `base..HEAD`,
//! oldest first, with its action.

use crate::rebase_todo::{Action, Entry};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Widget};

#[derive(Debug)]
pub struct RebaseEditor {
    pub base: String,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub scroll: usize,
    pub last_rect: Rect,
}

impl RebaseEditor {
    pub fn new(base: String, entries: Vec<Entry>) -> Self {
        Self {
            base,
            entries,
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
        }
    }
}

/// Paint the editor centred in `area`.
pub fn render(ed: &mut RebaseEditor, area: Rect, buf: &mut Buffer, theme: crate::theme::Theme) {
    let width = (area.width.saturating_mul(8) / 10).clamp(40.min(area.width), 110.min(area.width));
    let height = (area.height.saturating_mul(7) / 10).clamp(8.min(area.height), area.height);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 4,
        width,
        height,
    };
    ed.last_rect = rect;
    Widget::render(Clear, rect, buf);
    let bg = theme.ui(Color::Rgb(0x16, 0x18, 0x1f));
    let text = Style::default()
        .fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        .bg(bg);
    let dim = Style::default()
        .fg(theme.ui(Color::Rgb(0x8a, 0x93, 0xa6)))
        .bg(bg);
    let sel = text.bg(theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e)));
    let title = format!(" Interactive Rebase onto {} ", ed.base);
    Widget::render(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
            .title(title)
            .style(Style::default().bg(bg)),
        rect,
        buf,
    );
    let inner = Rect {
        x: rect.x + 2,
        y: rect.y + 1,
        width: rect.width.saturating_sub(4),
        height: rect.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height < 3 {
        return;
    }
    let w = inner.width as usize;
    buf.set_stringn(
        inner.x,
        inner.y,
        "p pick \u{b7} r reword \u{b7} e edit \u{b7} s squash \u{b7} f fixup \u{b7} d drop \u{b7} Alt+\u{2191}\u{2193} move \u{b7} Enter rebase \u{b7} Esc cancel",
        w,
        dim,
    );
    let top = inner.y + 2;
    let rows_h = (inner.y + inner.height).saturating_sub(top) as usize;
    if ed.selected < ed.scroll {
        ed.scroll = ed.selected;
    } else if rows_h > 0 && ed.selected >= ed.scroll + rows_h {
        ed.scroll = ed.selected + 1 - rows_h;
    }
    for (n, e) in ed.entries.iter().enumerate().skip(ed.scroll).take(rows_h) {
        let base = if n == ed.selected { sel } else { text };
        let style = match e.action {
            Action::Drop => base
                .fg(theme.ui(Color::Rgb(0x8a, 0x93, 0xa6)))
                .add_modifier(Modifier::CROSSED_OUT),
            Action::Squash | Action::Fixup => base.fg(theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b))),
            _ => base,
        };
        let subject = e.message.as_deref().unwrap_or(&e.subject);
        let line = format!(
            "{:<6} {} {subject}",
            e.action.word(),
            &e.sha[..e.sha.len().min(7)]
        );
        buf.set_stringn(
            inner.x,
            top + (n - ed.scroll) as u16,
            format!("{line:<w$}"),
            w,
            style,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_show_action_short_sha_and_subject_with_the_keys() {
        let entries = vec![
            Entry {
                action: Action::Pick,
                sha: "aaaaaaaaaa".into(),
                subject: "first".into(),
                message: None,
            },
            Entry {
                action: Action::Squash,
                sha: "bbbbbbbbbb".into(),
                subject: "second".into(),
                message: None,
            },
        ];
        let mut ed = RebaseEditor::new(String::from("main"), entries);
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 30));
        render(
            &mut ed,
            Rect::new(0, 0, 120, 30),
            &mut buf,
            crate::theme::Theme::default(),
        );
        let text: String = (0..30)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(text.contains("Interactive Rebase onto main"), "{text}");
        assert!(text.contains("pick   aaaaaaa first"), "{text}");
        assert!(text.contains("squash bbbbbbb second"), "{text}");
        assert!(text.contains("p pick"), "{text}");
    }
}
