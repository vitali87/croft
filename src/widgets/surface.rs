//! Pane and overlay chrome for a theme that draws layered surfaces instead of
//! line boxes ([`Theme::surfaces`], Croft Loch).
//!
//! A pane keeps its `Borders::ALL` inset under every theme, so its inner
//! rect, its hit targets and every test of where content lands are the same
//! with or without surfaces. Only what the frame cells show changes: padding
//! in the pane's surface tone, a hairline seam where two surfaces meet (drawn
//! by the workbench, which knows the layout), and a rule along the focused
//! pane's top edge. Overlays get a card edge and a one-cell shadow in place
//! of the gradient box.

use ratatui::{
    buffer::{Buffer, Cell},
    layout::Rect,
    style::{Color, Style},
    symbols::border,
    widgets::{Block, Borders},
};

use crate::gradient::{GRAD_TL, GRAD_TR, lerp_rgb, rgb_color};
use crate::theme::Theme;

/// Which side of its cells a seam hugs. The glyphs are one-eighth blocks, so
/// a seam sits on the cell boundary instead of the cell's middle, and two
/// panes' seams on either side of one boundary read as a single line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    fn glyph(self) -> &'static str {
        match self {
            Edge::Top => "\u{2594}",
            Edge::Bottom => "\u{2581}",
            Edge::Left => "\u{258f}",
            Edge::Right => "\u{2595}",
        }
    }
}

/// One-eighth blocks on the outer edge of a frame: a seam on the cell
/// boundary instead of a line through the cell's middle. The corners reuse
/// the edge glyphs; at an eighth of a cell the missing corner pixel is not
/// visible.
pub const HAIRLINE: border::Set = border::Set {
    top_left: "\u{2594}",
    top_right: "\u{2594}",
    bottom_left: "\u{2581}",
    bottom_right: "\u{2581}",
    vertical_left: "\u{258f}",
    vertical_right: "\u{2595}",
    horizontal_top: "\u{2594}",
    horizontal_bottom: "\u{2581}",
};

/// The rule a stacked side-bar section draws between itself and the next
/// (Outline, Timeline, Open Editors, Dependencies, the agent lane, the
/// commit graph): the dim line in `style` on a boxed theme, a hairline under
/// `surfaces`.
pub fn section_block<'a>(theme: Theme, borders: Borders, style: Style) -> Block<'a> {
    match theme.surfaces() {
        Some(s) => Block::default()
            .borders(borders)
            .border_set(HAIRLINE)
            .border_style(Style::default().fg(s.hairline())),
        None => Block::default().borders(borders).border_style(style),
    }
}

/// The block a pane renders under a `surfaces` theme: the same one-cell
/// inset as a bordered box, painted as padding in `fill`, which also becomes
/// the background of everything the pane leaves unpainted.
pub fn pane_block<'a>(fill: Color) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_set(border::EMPTY)
        .style(Style::default().bg(fill))
}

/// The tone a pane fills its frame with: the chrome surface for the side
/// bars, the code canvas for the editor and the panel views.
pub fn pane_fill(theme: Theme, chrome: bool) -> Color {
    match theme.surfaces() {
        Some(s) if chrome => s.chrome(),
        _ => theme.editor_bg(),
    }
}

/// The frame a side-bar or panel view renders: the bordered box in
/// `border_style` on a boxed theme, padding in the view's surface tone under
/// `surfaces` (`chrome` for the side bars, the canvas otherwise). The inner
/// rect is the same either way.
pub fn view_block<'a>(theme: Theme, chrome: bool, border_style: Style) -> Block<'a> {
    match theme.surfaces() {
        Some(_) => pane_block(pane_fill(theme, chrome)),
        None => Block::default()
            .borders(Borders::ALL)
            .border_style(border_style),
    }
}

/// Mark a focused view: the focus rule under `surfaces`, the gradient box on
/// a boxed brand theme (`focus_gradient`), nothing otherwise, where the
/// border colour already says it. Returns true when the gradient box went
/// over the top row, so a caller with a title there re-stamps it.
pub fn paint_view_focus(
    buf: &mut Buffer,
    area: Rect,
    theme: Theme,
    focused: bool,
    focus_gradient: bool,
) -> bool {
    if !focused {
        return false;
    }
    if theme.surfaces().is_some() {
        paint_focus_rule(buf, area, theme);
        false
    } else if focus_gradient {
        crate::gradient::paint_gradient_box(buf, area);
        true
    } else {
        false
    }
}

/// A frame cell a seam or rule may draw over: blank padding or a line glyph
/// from a frame. Titles, buttons, badges and gauges are left alone.
fn is_frame_cell(cell: &Cell) -> bool {
    matches!(
        cell.symbol(),
        " " | "\u{2500}"
            | "\u{2502}"
            | "\u{250c}"
            | "\u{2510}"
            | "\u{2514}"
            | "\u{2518}"
            | "\u{256d}"
            | "\u{256e}"
            | "\u{2570}"
            | "\u{256f}"
            | "\u{2581}"
            | "\u{2594}"
            | "\u{258f}"
            | "\u{2595}"
    )
}

/// Draw `glyph` along row `y` from `x` for `width` cells, colouring the
/// `i`th with `color(i)`, over the frame cells only. A lone space between
/// two painted characters is a title's word gap (`REMOTE EXPLORER`), not
/// padding, so it stays a space.
fn paint_edge_row(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    glyph: &str,
    color: impl Fn(u16) -> Color,
) {
    let frame: Vec<bool> = (0..width)
        .map(|i| is_frame_cell(&buf[(x + i, y)]))
        .collect();
    for i in 0..width {
        let i_us = usize::from(i);
        let word_gap = i_us > 0
            && i_us + 1 < frame.len()
            && !frame[i_us - 1]
            && !frame[i_us + 1]
            && buf[(x + i, y)].symbol() == " ";
        if frame[i_us] && !word_gap {
            let cell = &mut buf[(x + i, y)];
            cell.set_symbol(glyph);
            cell.set_fg(color(i));
        }
    }
}

/// Draw a seam along `edge` of every frame cell in `rect`, in `color` over
/// each cell's own background.
pub fn paint_seam(buf: &mut Buffer, rect: Rect, edge: Edge, color: Color) {
    let rect = rect.intersection(buf.area);
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            let cell = &mut buf[(x, y)];
            if is_frame_cell(cell) {
                cell.set_symbol(edge.glyph());
                cell.set_fg(color);
            }
        }
    }
}

/// The brand gradient's colour at cell `i` of `n` (teal to orange), or the
/// flat accent on a theme without the gradient.
fn rule_color(theme: Theme, i: u16, n: u16) -> Color {
    if theme.gradient() {
        let t = if n > 1 {
            f32::from(i) / f32::from(n - 1)
        } else {
            0.0
        };
        rgb_color(lerp_rgb(GRAD_TL, GRAD_TR, t))
    } else {
        theme.accent()
    }
}

/// The focused pane's marker under a `surfaces` theme: a rule along the top
/// edge of its frame row, in the brand gradient, broken wherever the pane
/// painted a title or a button there. It stands where VS Code puts
/// `tab.activeBorderTop`, and replaces the whole-pane focus box.
pub fn paint_focus_rule(buf: &mut Buffer, area: Rect, theme: Theme) {
    let area = area.intersection(buf.area);
    if area.height < 2 {
        return;
    }
    paint_edge_row(buf, area.x, area.y, area.width, Edge::Top.glyph(), |i| {
        rule_color(theme, i, area.width)
    });
}

/// Darken a colour toward black, for the overlay shadow. A cell left on the
/// terminal's default background shows the theme background, so that is
/// what gets darkened.
fn shade(c: Color, theme: Theme) -> Color {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => theme.editor_bg_rgb(),
    };
    rgb_color(lerp_rgb((r, g, b), (0, 0, 0), 0.55))
}

/// The frame an overlay draws (Command Palette, pickers, popups, dialogs).
/// On a boxed brand theme it is the gradient box. Under `surfaces` it is a
/// card edge in one-eighth blocks hugging the overlay's outer boundary, the
/// brand gradient along the top and the hairline on the other three sides,
/// with a one-cell shadow below and to the right; a title painted on the top
/// row keeps its cells.
pub fn paint_overlay_frame(buf: &mut Buffer, rect: Rect, theme: Theme) {
    let Some(s) = theme.surfaces() else {
        crate::gradient::paint_gradient_box(buf, rect);
        return;
    };
    let area = buf.area;
    if rect.width < 2 || rect.height < 2 || rect.intersection(area) != rect {
        return;
    }
    let (right, bottom) = (rect.right() - 1, rect.bottom() - 1);
    paint_edge_row(buf, rect.x, rect.y, rect.width, Edge::Top.glyph(), |i| {
        rule_color(theme, i, rect.width)
    });
    paint_edge_row(
        buf,
        rect.x,
        bottom,
        rect.width,
        Edge::Bottom.glyph(),
        |_| s.hairline(),
    );
    for y in rect.y + 1..bottom {
        for (x, edge) in [(rect.x, Edge::Left), (right, Edge::Right)] {
            let cell = &mut buf[(x, y)];
            if is_frame_cell(cell) {
                cell.set_symbol(edge.glyph());
                cell.set_fg(s.hairline());
            }
        }
    }
    // The shadow falls one cell right and one cell down, offset by a cell
    // so the card reads as lifted rather than outlined.
    let shadow = |cell: &mut Cell| {
        let (fg, bg) = (shade(cell.fg, theme), shade(cell.bg, theme));
        cell.set_fg(fg);
        cell.set_bg(bg);
    };
    if rect.right() < area.right() {
        for y in rect.y + 1..=bottom {
            shadow(&mut buf[(rect.right(), y)]);
        }
    }
    if rect.bottom() < area.bottom() {
        for x in rect.x + 1..=rect.right().min(area.right() - 1) {
            shadow(&mut buf[(x, rect.bottom())]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::Widget;

    fn loch() -> Theme {
        Theme::from_id("loch")
    }

    #[test]
    fn a_pane_block_keeps_the_bordered_inset() {
        let area = Rect::new(2, 1, 20, 8);
        let boxed = Block::default().borders(Borders::ALL).inner(area);
        assert_eq!(pane_block(Color::Black).inner(area), boxed);
    }

    #[test]
    fn a_pane_block_paints_padding_in_its_fill() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 4));
        let fill = Color::Rgb(0x0e, 0x10, 0x11);
        pane_block(fill).render(buf.area, &mut buf);
        for y in 0..4 {
            for x in 0..10 {
                assert_eq!(buf[(x, y)].symbol(), " ", "({x},{y}) is padding");
                assert_eq!(buf[(x, y)].bg, fill);
            }
        }
    }

    #[test]
    fn the_focus_rule_runs_the_brand_gradient_and_spares_the_title() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 3));
        buf.set_string(1, 0, "EX", Style::default());
        let area = buf.area;
        paint_focus_rule(&mut buf, area, loch());
        assert_eq!(buf[(0, 0)].symbol(), "\u{2594}");
        assert_eq!(buf[(0, 0)].fg, rgb_color(GRAD_TL));
        assert_eq!(buf[(11, 0)].fg, rgb_color(GRAD_TR));
        assert_eq!(buf[(1, 0)].symbol(), "E", "the title keeps its cells");
        assert_eq!(buf[(5, 1)].symbol(), " ", "only the top row is touched");
    }

    #[test]
    fn the_focus_rule_keeps_the_space_between_a_titles_words() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 3));
        buf.set_string(1, 0, " REMOTE EXPLORER ", Style::default());
        let area = buf.area;
        paint_focus_rule(&mut buf, area, loch());
        assert_eq!(buf[(8, 0)].symbol(), " ", "the word gap stays a space");
        assert_eq!(
            buf[(1, 0)].symbol(),
            "\u{2594}",
            "the title's padding takes the rule"
        );
        assert_eq!(buf[(19, 0)].symbol(), "\u{2594}");
    }

    #[test]
    fn a_seam_draws_over_padding_and_frame_lines_only() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 3));
        buf.set_string(3, 1, "x", Style::default());
        buf.set_string(3, 2, "\u{2500}", Style::default());
        let hair = Color::Rgb(0x23, 0x28, 0x2a);
        paint_seam(&mut buf, Rect::new(3, 0, 1, 3), Edge::Right, hair);
        assert_eq!(buf[(3, 0)].symbol(), "\u{2595}");
        assert_eq!(buf[(3, 0)].fg, hair);
        assert_eq!(buf[(3, 1)].symbol(), "x");
        assert_eq!(buf[(3, 2)].symbol(), "\u{2595}");
    }

    #[test]
    fn an_overlay_on_a_surfaces_theme_is_a_card_with_a_shadow() {
        let theme = loch();
        let s = theme.surfaces().unwrap();
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 6));
        buf.set_style(buf.area, Style::default().bg(s.chrome()));
        let card = Rect::new(1, 1, 8, 3);
        Block::default()
            .borders(Borders::ALL)
            .title("Hi")
            .render(card, &mut buf);
        paint_overlay_frame(&mut buf, card, theme);
        assert_eq!(buf[(1, 1)].symbol(), "\u{2594}", "top edge");
        assert_eq!(buf[(2, 1)].symbol(), "H", "the title survives");
        assert_eq!(buf[(1, 3)].symbol(), "\u{2581}", "bottom edge");
        assert_eq!(buf[(1, 2)].symbol(), "\u{258f}", "left edge");
        assert_eq!(buf[(8, 2)].symbol(), "\u{2595}", "right edge");
        assert_eq!(buf[(8, 3)].fg, s.hairline());
        // Shadow: right column from the second row, bottom row from the
        // second column; the cell diagonally past the top-left is untouched.
        let shaded = shade(s.chrome(), theme);
        assert_eq!(buf[(9, 2)].bg, shaded);
        assert_eq!(buf[(9, 4)].bg, shaded);
        assert_eq!(buf[(2, 4)].bg, shaded);
        assert_eq!(buf[(9, 1)].bg, s.chrome());
        assert_eq!(buf[(1, 4)].bg, s.chrome());
    }

    #[test]
    fn an_overlay_on_a_boxed_brand_theme_keeps_the_gradient_box() {
        let mut ours = Buffer::empty(Rect::new(0, 0, 10, 5));
        let mut theirs = ours.clone();
        let card = Rect::new(1, 1, 6, 3);
        paint_overlay_frame(&mut ours, card, Theme::BLACK);
        crate::gradient::paint_gradient_box(&mut theirs, card);
        assert_eq!(ours, theirs);
    }

    #[test]
    fn pane_fill_is_chrome_only_for_surfaces_side_bars() {
        let s = loch().surfaces().unwrap();
        assert_eq!(pane_fill(loch(), true), s.chrome());
        assert_eq!(pane_fill(loch(), false), loch().editor_bg());
        assert_eq!(pane_fill(Theme::BLACK, true), Theme::BLACK.editor_bg());
    }
}
