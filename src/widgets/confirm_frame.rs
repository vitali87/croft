//! The frame croft's confirm dialogs are drawn in: centred on the screen,
//! from 50 to 96 columns wide, cleared, bordered and titled in the dialog's
//! accent over the pop-up body colour, with the warning card
//! ([`paint_warning_card`]) laid over it once the body is drawn. Each
//! dialog says only what sets its frame apart ([`ConfirmFrame`]) and draws
//! its own body.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear};

use crate::theme::Theme;
use crate::widgets::approval_popup::paint_warning_card;

/// Where a dialog's body starts inside its border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inset {
    /// Two columns in from each side, one row in from the top and bottom.
    Wide,
    /// Right inside the border on every side.
    Tight,
}

/// What sets one confirm dialog's frame apart from another's.
#[derive(Clone, Copy, Debug)]
pub struct ConfirmFrame<'a> {
    /// The title on the top border, with a space each side.
    pub title: &'a str,
    /// The border and the title's fill, already fitted to the theme.
    pub accent: Color,
    /// The title's text, drawn as given: white on red, black on amber.
    pub title_fg: Color,
    /// Rows, borders included. A screen too short cuts the frame.
    pub height: u16,
    /// Where the body starts inside the border.
    pub inset: Inset,
}

/// The width a confirm dialog takes on `area`: 8 columns narrower than
/// the screen, from 50 to 96, never wider than the screen. A dialog sized
/// to its text measures it at this width.
pub fn confirm_width(area: Rect) -> u16 {
    // The lower bound must never beat the screen itself: on a terminal
    // narrower than 50 columns the dialog came out wider than the buffer
    // and the paint panicked (#192).
    area.width.saturating_sub(8).clamp(50, 96).min(area.width)
}

/// Draw `spec`'s frame centred on the screen, then `body` in the area
/// inside the border (where [`Inset`] puts it), then the warning card over
/// the whole dialog.
pub fn draw_confirm(
    frame: &mut Frame,
    theme: Theme,
    spec: ConfirmFrame,
    body: impl FnOnce(&mut Frame, Rect),
) {
    let area = frame.area();
    let width = confirm_width(area);
    // Cut to the screen: a terminal narrower or shorter than the dialog
    // made a rect outside the buffer, which panics.
    let rect = Rect {
        x: (area.width.saturating_sub(width)) / 2 + area.x,
        y: (area.height.saturating_sub(spec.height)) / 2 + area.y,
        width,
        height: spec.height,
    }
    .intersection(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(spec.accent))
        .style(Style::default().bg(theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))))
        .title(Span::styled(
            spec.title,
            Style::default()
                .fg(spec.title_fg)
                .bg(spec.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = match spec.inset {
        Inset::Wide => Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        },
        Inset::Tight => block.inner(rect),
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    body(frame, inner);
    paint_warning_card(frame.buffer_mut(), rect, theme, spec.accent);
}

/// The key line of a yes-or-no dialog: a red `[Y]` followed by `yes`, the
/// rest of the word and what it does (`"es, discard"`), then a green
/// `[N]o / Esc`.
pub fn yes_no_keys(yes: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "[Y]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{yes}   ")),
        Span::styled(
            "[N]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("o / Esc"),
    ])
}
