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
        }
    }

    /// Scroll by `delta` wrapped rows, held between the first row and the
    /// one that puts the last line on the bottom row of `last_area`.
    pub fn scroll_by(&mut self, delta: i32) {
        let max = self.max_scroll(self.last_area);
        self.scroll = (i32::from(self.scroll) + delta).clamp(0, i32::from(max)) as u16;
    }

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
        if self.theme.gradient() {
            crate::gradient::paint_gradient_box(buf, area);
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

    #[test]
    fn a_short_screen_keeps_the_sixteen_row_floor() {
        let p = HoverPopup::new(numbered(40), (10, 23));
        assert_eq!(p.area_for(VP).height - 2, MAX_HEIGHT);
    }
}
