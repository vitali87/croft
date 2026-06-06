use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

const MIN_WIDTH: u16 = 20;
const MAX_WIDTH: u16 = 80;
const MAX_HEIGHT: u16 = 16;

pub struct HoverPopup {
    pub lines: Vec<String>,
    pub anchor: (u16, u16),
    /// Black theme: wear the orange→green gradient border instead of the
    /// legacy bright-blue. Set by the app before render from `popup_gradient`.
    pub gradient: bool,
}

impl HoverPopup {
    pub fn new(text: String, anchor: (u16, u16)) -> Self {
        Self {
            lines: text.lines().map(|s| s.to_string()).collect(),
            anchor,
            gradient: false,
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
        let width = self
            .content_width()
            .saturating_add(2)
            .clamp(MIN_WIDTH, MAX_WIDTH);
        let inner_w = width.saturating_sub(2).max(1);
        let body = self
            .lines
            .iter()
            .map(|l| (l.chars().count() as u16).div_ceil(inner_w).max(1))
            .sum::<u16>()
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
            .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x21, 0x2a)));
        let text = Text::from(
            self.lines
                .iter()
                .map(|l| Line::from(l.clone()))
                .collect::<Vec<_>>(),
        );
        let para = Paragraph::new(text)
            .block(block)
            .style(Style::default().fg(Color::Rgb(0xd0, 0xd6, 0xe0)))
            .wrap(Wrap { trim: false });
        Widget::render(Clear, area, buf);
        para.render(area, buf);
        if self.gradient {
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
