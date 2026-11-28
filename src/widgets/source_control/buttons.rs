//! The buttons every view draws: the primary button (Commit, Initialize, Open, Run and Debug)
//! in its boxed and rounded looks, the Commit split button, their colours and the lift a
//! pointer gives them.

use super::*;

impl SourceControlPanel {
    /// Croft Loch's Commit split button: one pill on the middle row of its
    /// click targets (the rows above and below stay chrome), Commit and its
    /// dropdown caret capped only on their outer ends. The one-cell gap
    /// between the two click targets carries a thin darker seam at its left
    /// edge and belongs to the caret half's face, so it reads as one button
    /// with a second action rather than two blocks, and the chevron centres
    /// in the face right of the seam. Each half lifts on its own under the
    /// pointer; the gap lifts with the caret it is painted as.
    pub(super) fn render_soft_split_button(&self, buf: &mut Buffer, main: Rect, caret: Rect) {
        use crate::widgets::surface::{CHEVRON_UNFOLDED, paint_soft_button};
        let (fill, fg) = primary_button_colors(self.theme, self.focus_gradient);
        let lift = |r: Rect| {
            if crate::widgets::hover::contains(r, self.hover_pointer) {
                lift_button(fill)
            } else {
                fill
            }
        };
        let (main_fill, caret_fill) = (lift(main), lift(caret));
        let main_face = paint_soft_button(buf, main, main_fill, true, false);
        let caret_face = paint_soft_button(buf, caret, caret_fill, false, true);
        let gap = Rect {
            x: main.right(),
            width: caret.x.saturating_sub(main.right()),
            ..main
        };
        let mut caret_label = caret_face;
        if gap.width > 0 {
            let gap_face = paint_soft_button(buf, gap, caret_fill, false, false);
            if let Color::Rgb(r, g, b) = fill
                && gap_face.width > 0
            {
                let seam = crate::gradient::rgb_color(crate::gradient::lerp_rgb(
                    (r, g, b),
                    (0, 0, 0),
                    0.45,
                ));
                buf[(gap_face.x, gap_face.y)]
                    .set_symbol("\u{258f}")
                    .set_fg(seam);
            }
            if caret_face.width > 0 && gap_face.width > 0 {
                caret_label = Rect {
                    x: gap_face.x,
                    width: gap_face.width + caret_face.width,
                    ..caret_face
                };
            }
        }
        paint_button_label(buf, main_face, "\u{eab2} Commit", main_fill, fg);
        paint_button_label(
            buf,
            caret_label,
            &CHEVRON_UNFOLDED.to_string(),
            caret_fill,
            fg,
        );
    }
}

/// Paint a chunky 3-row solid-bg button at `area`, with rounded corner
/// glyphs that let the panel bg show through the four corner notches so
/// the button reads as a rounded rectangle (matching the VS Code mockup).
/// Shared between the Source Control commit button and the Run-and-Debug
/// button — same style, same corner treatment.
pub fn render_rounded_button(buf: &mut Buffer, area: Rect, label: &str, bg: Color, fg: Color) {
    if area.width < 2 || area.height < 1 {
        return;
    }
    // Fill every cell with the solid bg. The label and the corner glyphs
    // overwrite specific cells below.
    let solid = Style::default().bg(bg);
    for ry in 0..area.height {
        for rx in 0..area.width {
            buf[(area.x + rx, area.y + ry)]
                .set_symbol(" ")
                .set_style(solid);
        }
    }
    // Replace the four corner cells with rounded-border glyphs whose bg is
    // *unset* so the panel bg shows through the outer angle of each curve;
    // the glyph's stroke is drawn in the button bg colour so the curve
    // visually continues the button outline.
    let corner_style = Style::default().fg(bg);
    if area.height >= 2 {
        buf[(area.x, area.y)]
            .set_symbol("╭")
            .set_style(corner_style);
        buf[(area.x + area.width - 1, area.y)]
            .set_symbol("╮")
            .set_style(corner_style);
        buf[(area.x, area.y + area.height - 1)]
            .set_symbol("╰")
            .set_style(corner_style);
        buf[(area.x + area.width - 1, area.y + area.height - 1)]
            .set_symbol("╯")
            .set_style(corner_style);
    }
    // Truncate the label rather than dropping it entirely when the button
    // is narrower than `label.chars().count()`. The earlier silent-drop
    // path produced a blank blue rectangle in narrow side panels — the
    // user-visible "WTF is this" bug from the no-repo empty state.
    let max_w = area.width as usize;
    let visible_label: String = label.chars().take(max_w).collect();
    if visible_label.is_empty() {
        return;
    }
    let label_w = visible_label.chars().count() as u16;
    let label_x = area.x + (area.width - label_w) / 2;
    let label_y = area.y + area.height / 2;
    buf.set_string(
        label_x,
        label_y,
        &visible_label,
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    );
}

/// A primary button's fill and label ink: the brand teal under the Black
/// theme (`focus_gradient`) and always under a `surfaces` theme, whose label
/// takes the strong ink rather than pure white; VS Code's button blue with
/// white on Croft Dark.
pub fn primary_button_colors(theme: crate::theme::Theme, focus_gradient: bool) -> (Color, Color) {
    let teal = crate::gradient::rgb_color(crate::gradient::PRIMARY_BTN_BG);
    match crate::widgets::surface::tones(theme) {
        Some(t) => (teal, t.strong()),
        None => (
            if focus_gradient {
                teal
            } else {
                Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2)
            },
            Color::Rgb(BUTTON_FG_RGB.0, BUTTON_FG_RGB.1, BUTTON_FG_RGB.2),
        ),
    }
}

/// A button colour lifted for the pointer: a step toward white, the hover a
/// filled button takes in VS Code (`button.hoverBackground`).
pub(super) fn lift_button(fill: Color) -> Color {
    match fill {
        Color::Rgb(r, g, b) => crate::gradient::rgb_color(crate::gradient::lerp_rgb(
            (r, g, b),
            (0xff, 0xff, 0xff),
            0.12,
        )),
        other => other,
    }
}

/// Write `label` bold and centred on `area`'s middle row, truncated to the
/// button's width rather than dropped (a blank button reads as broken).
pub(super) fn paint_button_label(buf: &mut Buffer, area: Rect, label: &str, bg: Color, fg: Color) {
    let visible_label: String = label.chars().take(area.width as usize).collect();
    if visible_label.is_empty() {
        return;
    }
    let label_w = visible_label.chars().count() as u16;
    buf.set_string(
        area.x + (area.width - label_w) / 2,
        area.y + area.height / 2,
        &visible_label,
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    );
}

/// The primary button every view draws (Commit, Initialize, Open, Run and
/// Debug). A boxed theme gets [`render_rounded_button`]. Under a `surfaces`
/// theme it is a pill ([`crate::widgets::surface::paint_soft_button`]): one
/// face row with half-block caps on the middle row of the three-row target,
/// the rows above and below left as the surface it sits on, instead of
/// drawing `╭╮╰╯` arcs on the chrome; the label centres between the caps
/// and the pointer lifts the face a step. The click target is the full
/// `area` either way, so the press area stays as generous as the box's.
pub fn render_button(
    buf: &mut Buffer,
    area: Rect,
    label: &str,
    bg: Color,
    fg: Color,
    theme: crate::theme::Theme,
    hovered: bool,
) {
    if crate::widgets::surface::tones(theme).is_none() {
        render_rounded_button(buf, area, label, bg, fg);
        return;
    }
    if area.width < 2 || area.height < 1 {
        return;
    }
    let fill = if hovered { lift_button(bg) } else { bg };
    let face = crate::widgets::surface::paint_soft_button(buf, area, fill, true, true);
    paint_button_label(buf, face, label, fill, fg);
}
