//! The update popup (#333) drawn as a card, in the manner of Ghostty's: a
//! title, the release in a label / value grid, a row of pill buttons with the
//! one primary action set apart on the right, and under a divider a link to
//! the release's notes. Pure: the App says what the card holds and where it
//! sits, and gets back where each control landed.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

use crate::gradient::{lerp_rgb, rgb_color};
use crate::theme::Theme;

/// Columns the card takes when the screen has them: room for the widest
/// button row (Skip, Later and Copy Command) with air between.
pub const WIDTH: u16 = 46;
/// The narrowest card that still fits that row; a narrower window waits
/// for room.
pub const MIN_WIDTH: u16 = 42;
/// Border plus padding on each side of the content.
const INSET: u16 = 3;
/// How far the grid's labels sit in from the content's left edge.
const GRID_INDENT: u16 = 2;

/// Codicon `package` (U+EB29): the update's glyph, on the card and its
/// status-bar pill.
pub const PACKAGE_GLYPH: char = '\u{eb29}';
/// Codicon `error` (U+EA87): a staging that failed.
pub const ERROR_GLYPH: char = '\u{ea87}';
/// Codicon `pass-filled` (U+EBB3): a staged build waiting for Relaunch.
pub const READY_GLYPH: char = '\u{ebb3}';
/// Codicon `book` (U+EAA4), leading the release-notes link.
pub const NOTES_GLYPH: char = '\u{eaa4}';
/// Codicon `output` (U+EB9D), leading the link to a failed staging's log.
pub const LOG_GLYPH: char = '\u{eb9d}';
/// Powerline half circles, the rounded ends of a pill (as on the PROBLEMS
/// badge and the Extensions switches).
const CAP_L: &str = "\u{e0b6}";
const CAP_R: &str = "\u{e0b4}";

/// What the card says. `A` is the caller's action type, handed back with
/// the rect each control was painted at.
pub struct Card<A> {
    pub glyph: char,
    pub glyph_color: Color,
    pub title: String,
    /// The label / value grid; labels are right-aligned to one column.
    pub rows: Vec<(&'static str, String)>,
    /// Prose under the grid, wrapped to the card.
    pub message: Option<String>,
    /// The secondary buttons, then the primary one, which sits apart at the
    /// right of the row.
    pub buttons: Vec<(String, A)>,
    /// The link under the divider: its glyph, label and action.
    pub link: Option<(char, String, A)>,
}

/// The card's colours, derived from the theme once per paint. Every grey
/// is croft's dark chrome vocabulary through [`Theme::ui`], so a light,
/// high contrast or `surfaces` theme gets its own counterpart.
struct Palette {
    bg: Color,
    title: Color,
    label: Color,
    value: Color,
    divider: Color,
    secondary: Color,
    secondary_hover: Color,
    secondary_fg: Color,
    primary: Color,
    primary_hover: Color,
    link: Color,
}

impl Palette {
    fn of(theme: Theme) -> Self {
        let primary = theme.button();
        Palette {
            bg: theme.ui(Color::Rgb(0x16, 0x18, 0x1f)),
            title: theme.ui(Color::White),
            label: match theme.surfaces() {
                Some(s) => s.muted(),
                None => theme.ui(Color::Rgb(0x8e, 0x95, 0xa4)),
            },
            value: theme.ui(Color::Rgb(0xcc, 0xcc, 0xcc)),
            divider: theme.ui(Color::Rgb(0x3b, 0x42, 0x52)),
            secondary: theme.ui(Color::Rgb(0x3a, 0x40, 0x52)),
            secondary_hover: theme.ui(Color::Rgb(0x4a, 0x51, 0x66)),
            secondary_fg: theme.ui(Color::Rgb(0xe5, 0xe9, 0xf0)),
            primary,
            primary_hover: lift(primary, theme),
            link: theme.accent(),
        }
    }
}

/// `c` one step toward the theme's foreground: the hover state of a fill.
fn lift(c: Color, theme: Theme) -> Color {
    let Color::Rgb(r, g, b) = c else {
        return c;
    };
    let toward = if theme.is_light() {
        (0x00, 0x00, 0x00)
    } else {
        (0xff, 0xff, 0xff)
    };
    rgb_color(lerp_rgb((r, g, b), toward, 0.18))
}

/// `label` as a pill: a half-circle cap each side of the padded label, the
/// caps in the fill's colour over whatever lies beneath. Width is the
/// label's plus four.
pub fn pill_spans(label: &str, fill: Color, fg: Color) -> [Span<'static>; 3] {
    [
        Span::styled(CAP_L, Style::default().fg(fill)),
        Span::styled(
            format!(" {label} "),
            Style::default()
                .fg(fg)
                .bg(fill)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(CAP_R, Style::default().fg(fill)),
    ]
}

/// The status-bar pill's fill: the primary button's, so the pill and the
/// card it opens read as one control.
pub fn pill_fill(theme: Theme, hovered: bool) -> Color {
    let fill = theme.button();
    if hovered { lift(fill, theme) } else { fill }
}

/// Greedy word wrap to `width` columns. A word wider than the line keeps a
/// line of its own and is clipped where it is painted.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Cells a pill takes for `label`: the caps and a space either side.
fn pill_width(label: &str) -> u16 {
    label.chars().count() as u16 + 4
}

impl<A: Copy> Card<A> {
    fn message_lines(&self, width: u16) -> Vec<String> {
        let room = width.saturating_sub(2 * INSET) as usize;
        self.message
            .as_deref()
            .map(|m| wrap(m, room))
            .unwrap_or_default()
    }

    /// Rows the card takes at `width` columns: the border, the title and a
    /// gap, the grid and the message (a gap between them), a gap, the
    /// buttons, the divider and link when there is one, and the border.
    pub fn height(&self, width: u16) -> u16 {
        let grid = self.rows.len() as u16;
        let message = self.message_lines(width).len() as u16;
        let between = u16::from(grid > 0 && message > 0);
        let footer = if self.link.is_some() { 2 } else { 0 };
        1 + 2 + grid + between + message + 2 + footer + 1
    }

    /// Paint the card over `rect` (sized by [`Card::height`]) and return
    /// where each button and the link landed. `pointer` lifts whichever
    /// control it rests on.
    pub fn render(
        &self,
        buf: &mut Buffer,
        rect: Rect,
        theme: Theme,
        pointer: Option<(u16, u16)>,
    ) -> Vec<(Rect, A)> {
        let rect = rect.intersection(buf.area);
        if rect.width < 2 * INSET + 2 || rect.height < 4 {
            return Vec::new();
        }
        let p = Palette::of(theme);
        Clear.render(rect, buf);
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.accent()))
            .style(Style::default().bg(p.bg))
            .render(rect, buf);
        if theme.framed_overlays() {
            crate::widgets::surface::paint_overlay_frame(buf, rect, theme);
        }
        let x = rect.x + INSET;
        let w = rect.width - 2 * INSET;
        let right = x + w;
        let mut y = rect.y + 1;
        let mut hits = Vec::new();

        buf.set_stringn(
            x,
            y,
            self.glyph.to_string(),
            1,
            Style::default().fg(self.glyph_color),
        );
        buf.set_stringn(
            x + 2,
            y,
            &self.title,
            w.saturating_sub(2) as usize,
            Style::default().fg(p.title).add_modifier(Modifier::BOLD),
        );
        y += 2;

        let label_w = self
            .rows
            .iter()
            .map(|(l, _)| l.chars().count() as u16 + 1)
            .max()
            .unwrap_or(0);
        for (label, value) in &self.rows {
            let text = format!("{label}:");
            let lx = x + GRID_INDENT + label_w - text.chars().count() as u16;
            buf.set_stringn(lx, y, &text, w as usize, Style::default().fg(p.label));
            let vx = x + GRID_INDENT + label_w + 2;
            if vx < right {
                buf.set_stringn(
                    vx,
                    y,
                    value,
                    (right - vx) as usize,
                    Style::default().fg(p.value),
                );
            }
            y += 1;
        }
        let message = self.message_lines(rect.width);
        if !self.rows.is_empty() && !message.is_empty() {
            y += 1;
        }
        for line in &message {
            buf.set_stringn(x, y, line, w as usize, Style::default().fg(p.value));
            y += 1;
        }
        y += 1;

        // Secondary buttons from the left, the primary flush right; a row
        // too narrow for the gap lets the primary follow the others.
        let mut bx = x;
        let last = self.buttons.len().saturating_sub(1);
        for (i, (label, action)) in self.buttons.iter().enumerate() {
            let pw = pill_width(label);
            let primary = i == last;
            if primary {
                bx = bx.max(right.saturating_sub(pw));
            }
            if bx + pw > right {
                break;
            }
            let hit = Rect::new(bx, y, pw, 1);
            let hovered = crate::widgets::hover::contains(hit, pointer);
            let (fill, fg) = match (primary, hovered) {
                (true, false) => (p.primary, Color::White),
                (true, true) => (p.primary_hover, Color::White),
                (false, false) => (p.secondary, p.secondary_fg),
                (false, true) => (p.secondary_hover, p.secondary_fg),
            };
            let mut cx = bx;
            for span in pill_spans(label, fill, fg) {
                let (end, _) = buf.set_span(cx, y, &span, pw);
                cx = end;
            }
            hits.push((hit, *action));
            bx += pw + 1;
        }
        y += 1;

        if let Some((glyph, label, action)) = &self.link
            && y + 2 <= rect.bottom()
        {
            // The divider runs the card's full width and joins a line
            // border with tees; a `surfaces` card edge has no line to join.
            for dx in rect.x + 1..rect.right() - 1 {
                buf[(dx, y)].set_symbol("\u{2500}").set_fg(p.divider);
            }
            for (sx, tee) in [(rect.x, "\u{251c}"), (rect.right() - 1, "\u{2524}")] {
                let cell = &mut buf[(sx, y)];
                if cell.symbol() == "\u{2502}" {
                    cell.set_symbol(tee);
                }
            }
            y += 1;
            let hit = Rect::new(rect.x + 1, y, rect.width - 2, 1);
            let hovered = crate::widgets::hover::contains(hit, pointer);
            buf.set_stringn(x, y, glyph.to_string(), 1, Style::default().fg(p.link));
            let style = if hovered {
                Style::default()
                    .fg(p.link)
                    .add_modifier(Modifier::UNDERLINED)
            } else {
                Style::default().fg(p.value)
            };
            buf.set_stringn(x + 2, y, label, w.saturating_sub(4) as usize, style);
            buf.set_stringn(
                right - 1,
                y,
                "\u{2197}",
                1,
                Style::default().fg(if hovered { p.link } else { p.label }),
            );
            hits.push((hit, *action));
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Act {
        Skip,
        Later,
        Update,
        Notes,
    }

    fn card() -> Card<Act> {
        Card {
            glyph: PACKAGE_GLYPH,
            glyph_color: Color::Cyan,
            title: String::from("Update Available"),
            rows: vec![
                ("Version", String::from("v0.2.90")),
                ("Installed", String::from("v0.2.84")),
                ("Released", String::from("Mar 3, 2026")),
            ],
            message: None,
            buttons: vec![
                (String::from("Skip"), Act::Skip),
                (String::from("Later"), Act::Later),
                (String::from("Update"), Act::Update),
            ],
            link: Some((
                NOTES_GLYPH,
                String::from("What's New in v0.2.90"),
                Act::Notes,
            )),
        }
    }

    fn row(buf: &Buffer, y: u16) -> String {
        (buf.area.x..buf.area.right())
            .map(|x| buf[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn the_card_lays_out_title_grid_buttons_and_link() {
        let c = card();
        let h = c.height(WIDTH);
        assert_eq!(h, 1 + 2 + 3 + 2 + 2 + 1);
        let area = Rect::new(0, 0, WIDTH, h);
        let mut buf = Buffer::empty(area);
        let hits = c.render(&mut buf, area, Theme::DARK_BLUE, None);
        assert!(row(&buf, 1).contains("Update Available"));
        // Labels right-aligned to one column, values aligned after it.
        let version = row(&buf, 3);
        let released = row(&buf, 5);
        assert_eq!(version.find("v0.2.90"), released.find("Mar 3, 2026"));
        assert_eq!(version.find(':'), released.find(':'));
        let acts: Vec<Act> = hits.iter().map(|(_, a)| *a).collect();
        assert_eq!(acts, [Act::Skip, Act::Later, Act::Update, Act::Notes]);
        // Skip and Later sit left; Update ends at the content's right edge.
        let update = hits[2].0;
        assert_eq!(update.right(), WIDTH - INSET, "the primary is flush right");
        assert!(hits[1].0.right() < update.x);
        assert_eq!(buf[(update.x, update.y)].symbol(), CAP_L);
        assert_eq!(buf[(update.x + 2, update.y)].symbol(), "U");
        assert_eq!(buf[(update.x + 2, update.y)].bg, Theme::DARK_BLUE.button());
        // The divider joins the border with tees, and the link fills a row.
        assert_eq!(buf[(0, h - 3)].symbol(), "\u{251c}");
        assert_eq!(buf[(WIDTH - 1, h - 3)].symbol(), "\u{2524}");
        assert!(row(&buf, h - 2).contains("What's New in v0.2.90"));
        assert_eq!(hits[3].0, Rect::new(1, h - 2, WIDTH - 2, 1));
    }

    #[test]
    fn the_pointer_lifts_the_control_it_rests_on() {
        let c = card();
        let area = Rect::new(0, 0, WIDTH, c.height(WIDTH));
        let mut buf = Buffer::empty(area);
        let hits = c.render(&mut buf, area, Theme::BLACK, None);
        let later = hits[1].0;
        let idle = buf[(later.x + 2, later.y)].bg;
        let mut buf = Buffer::empty(area);
        c.render(&mut buf, area, Theme::BLACK, Some((later.x + 3, later.y)));
        assert_ne!(buf[(later.x + 2, later.y)].bg, idle, "Later lifts");
        let notes = hits[3].0;
        assert!(
            !buf[(notes.x + 4, notes.y)]
                .modifier
                .contains(Modifier::UNDERLINED)
        );
        let mut buf = Buffer::empty(area);
        c.render(&mut buf, area, Theme::BLACK, Some((notes.x + 20, notes.y)));
        assert!(
            buf[(notes.x + 4, notes.y)]
                .modifier
                .contains(Modifier::UNDERLINED),
            "the link underlines"
        );
    }

    #[test]
    fn a_message_wraps_to_the_card_and_grows_it() {
        let mut c = card();
        c.rows.truncate(1);
        c.message = Some(String::from(
            "Not installed: cargo was not found, and this install has no release binary to fetch.",
        ));
        let lines = c.message_lines(WIDTH);
        assert!(lines.len() > 1, "{lines:?}");
        assert!(
            lines
                .iter()
                .all(|l| l.chars().count() <= (WIDTH - 2 * INSET) as usize)
        );
        assert_eq!(
            c.height(WIDTH),
            1 + 2 + 1 + 1 + lines.len() as u16 + 2 + 2 + 1
        );
        let area = Rect::new(0, 0, WIDTH, c.height(WIDTH));
        let mut buf = Buffer::empty(area);
        c.render(&mut buf, area, Theme::BLACK, None);
        let text: String = (0..buf.area.height).map(|y| row(&buf, y)).collect();
        for line in &lines {
            assert!(text.contains(line.as_str()), "{line:?} in {text}");
        }
    }

    #[test]
    fn a_narrow_card_keeps_every_control_inside_its_border() {
        let mut c = card();
        // The widest row there is: a Homebrew install's.
        c.buttons[2].0 = String::from("Copy Command");
        let w = MIN_WIDTH;
        let area = Rect::new(0, 0, w, c.height(w));
        let mut buf = Buffer::empty(area);
        let hits = c.render(&mut buf, area, Theme::DARK_BLUE, None);
        for (r, _) in &hits {
            assert!(r.right() < w, "{r:?} stays inside the border");
        }
        assert!(hits.iter().any(|(_, a)| *a == Act::Update));
    }

    #[test]
    fn wrap_breaks_between_words() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("  ", 7), Vec::<String>::new());
        assert_eq!(wrap("unbreakable", 4), ["unbreakable"]);
    }
}
