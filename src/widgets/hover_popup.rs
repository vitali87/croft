use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

const MIN_WIDTH: u16 = 20;
/// Compact tooltips (single-label button hints) shrink to fit their text
/// instead of reserving the 20-cell minimum an LSP type signature wants, so a
/// one-word hint like "Refresh" reads as a snug chip rather than a wide box.
const COMPACT_MIN_WIDTH: u16 = 3;
const MAX_WIDTH: u16 = 80;
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
}

impl HoverPopup {
    pub fn new(text: String, anchor: (u16, u16)) -> Self {
        Self {
            lines: text.lines().map(|s| s.to_string()).collect(),
            anchor,
            theme: crate::theme::Theme::default(),
            compact: false,
        }
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
        // Rows as `render` lays them out: word wrap moves a word that does
        // not fit to the next row, so a character count came up short and
        // the last words were clipped without a sign (#1526).
        let body = u16::try_from(self.paragraph().line_count(inner_w))
            .unwrap_or(MAX_HEIGHT)
            .clamp(1, MAX_HEIGHT);
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
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x21, 0x2a))));
        let para = self
            .paragraph()
            .block(block)
            .style(Style::default().fg(self.theme.ui(Color::Rgb(0xd0, 0xd6, 0xe0))));
        Widget::render(Clear, area, buf);
        para.render(area, buf);
        if self.theme.gradient() {
            crate::gradient::paint_gradient_box(buf, area);
        }
    }
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
    /// each, and the 16-row cap still holds.
    #[test]
    fn area_for_adds_no_rows_text_does_not_need() {
        let vp = Rect::new(0, 0, 120, 60);
        let word = HoverPopup::new("x".repeat(156), (10, 50)).area_for(vp);
        assert_eq!(word.height, 2 + 2, "156 chars in 78-wide rows");
        let short = HoverPopup::new("fn a()\nfn b()\nfn c()".into(), (10, 50)).area_for(vp);
        assert_eq!(short.height, 3 + 2);
        let long = HoverPopup::new(vec!["word"; 40].join("\n"), (10, 50)).area_for(vp);
        assert_eq!(long.height, MAX_HEIGHT + 2);
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
}
