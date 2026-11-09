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
    style::{Color, Modifier, Style},
    symbols::border,
    text::Span,
    widgets::{Block, Borders},
};

use crate::gradient::{GRAD_TL, GRAD_TR, lerp_rgb, rgb_color};
use crate::theme::Theme;

/// The selected row's accent bar: a quarter-cell stripe on the row's left
/// edge, the same mark the activity bar puts beside the active view.
pub const ROW_BAR: &str = "\u{258e}";

/// The working inks and fills a `surfaces` theme draws its chrome from,
/// derived once from the theme's four surface tones, its canvas and its
/// accent, so a light surfaces theme gets the same hierarchy turned the
/// other way.
///
/// The rules they encode: three text inks (`text`, `muted`, `faint`) and a
/// `strong` one for the single active title in a region; the accent only
/// for what has focus, is selected or is active; a selected row is a
/// `band` with the [`ROW_BAR`] in the accent; a card's edge is the `rim`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tones {
    /// The code canvas (editor and panel background).
    pub canvas: (u8, u8, u8),
    /// Side bar, activity bar, tab strip, panel title row, status bar.
    pub chrome: (u8, u8, u8),
    /// Every popover: palette, pickers, menus, hovers, toasts, dialogs.
    pub raised: (u8, u8, u8),
    /// The seam between two surfaces.
    pub hairline: (u8, u8, u8),
    /// A card's edge: a step past the hairline, so a popover's outline reads
    /// against the raised body it surrounds.
    pub rim: (u8, u8, u8),
    /// Body text: names, labels, values.
    pub text: (u8, u8, u8),
    /// The one emphasised label in a region: the focused view's title, the
    /// active tab.
    pub strong: (u8, u8, u8),
    /// Secondary text: inactive titles, paths, hints, idle icons.
    pub muted: (u8, u8, u8),
    /// Tertiary marks: line numbers, separators, dim chevrons.
    pub faint: (u8, u8, u8),
    /// Focus, selection and the active state, and nothing else.
    pub accent: (u8, u8, u8),
    /// White on a dark theme, black on a light one: what "lifting" a
    /// surface blends toward.
    pub lift: (u8, u8, u8),
}

impl Tones {
    /// The canvas as a cell colour.
    pub fn canvas(self) -> Color {
        rgb_color(self.canvas)
    }
    /// The chrome surface as a cell colour.
    pub fn chrome(self) -> Color {
        rgb_color(self.chrome)
    }
    /// The raised surface as a cell colour.
    pub fn raised(self) -> Color {
        rgb_color(self.raised)
    }
    /// The hairline as a cell colour.
    pub fn hairline(self) -> Color {
        rgb_color(self.hairline)
    }
    /// The card rim as a cell colour.
    pub fn rim(self) -> Color {
        rgb_color(self.rim)
    }
    /// Body text as a cell colour.
    pub fn text(self) -> Color {
        rgb_color(self.text)
    }
    /// The strong ink as a cell colour.
    pub fn strong(self) -> Color {
        rgb_color(self.strong)
    }
    /// Muted text as a cell colour.
    pub fn muted(self) -> Color {
        rgb_color(self.muted)
    }
    /// Faint marks as a cell colour.
    pub fn faint(self) -> Color {
        rgb_color(self.faint)
    }
    /// The accent as a cell colour.
    pub fn accent(self) -> Color {
        rgb_color(self.accent)
    }

    /// The selected-row fill on a surface `base`: the accent laid over it
    /// at a quarter strength, quiet enough that body text keeps its colour.
    pub fn band_on(self, base: (u8, u8, u8)) -> Color {
        rgb_color(lerp_rgb(base, self.accent, 0.24))
    }

    /// The selected-row fill on the raised surface (palette, pickers, menus).
    pub fn band(self) -> Color {
        self.band_on(self.raised)
    }

    /// A row the pointer is over, on surface `base`: a small neutral lift.
    pub fn hover_on(self, base: (u8, u8, u8)) -> Color {
        rgb_color(lerp_rgb(base, self.lift, 0.055))
    }

    /// A keycap chip's fill on a row whose fill is `row` (the band when the
    /// row is selected).
    pub fn keycap_on(self, row: Color) -> Color {
        let base = match row {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => self.raised,
        };
        rgb_color(lerp_rgb(base, self.lift, 0.085))
    }
}

/// The [`Tones`] of a `surfaces` theme, or `None` on a boxed one.
pub fn tones(theme: Theme) -> Option<Tones> {
    let s = theme.surfaces()?;
    let canvas = theme.editor_bg_rgb();
    let lift = if theme.is_light() {
        (0x00, 0x00, 0x00)
    } else {
        (0xff, 0xff, 0xff)
    };
    Some(Tones {
        canvas,
        chrome: s.chrome,
        raised: s.raised,
        hairline: s.hairline,
        rim: lerp_rgb(s.raised, lift, 0.11),
        text: lerp_rgb(s.muted, lift, 0.70),
        strong: lerp_rgb(s.muted, lift, 0.90),
        muted: s.muted,
        faint: lerp_rgb(canvas, s.muted, 0.52),
        accent: theme.accent_rgb(),
        lift,
    })
}

/// Paint `row` as the selected row of a list on a `surfaces` theme: the
/// band over every cell, and, when the list has the keyboard, the accent
/// [`ROW_BAR`] in its first cell (a pad cell or the card's left edge). The
/// row's text keeps its colours. Without focus the band is the hairline
/// tone, as VS Code greys an inactive selection.
pub fn paint_selected_row(
    buf: &mut Buffer,
    row: Rect,
    t: Tones,
    base: (u8, u8, u8),
    focused: bool,
) {
    let row = row.intersection(buf.area);
    if row.width == 0 || row.height == 0 {
        return;
    }
    let band = if focused {
        t.band_on(base)
    } else {
        rgb_color(lerp_rgb(base, t.lift, 0.07))
    };
    buf.set_style(row, Style::default().bg(band));
    if focused {
        for y in row.top()..row.bottom() {
            let cell = &mut buf[(row.x, y)];
            cell.set_symbol(ROW_BAR);
            cell.set_fg(t.accent());
            cell.set_bg(band);
        }
    }
}

/// A key-binding hint as keycaps on a `surfaces` theme: each key of a chord
/// in its own chip, the chord's keys joined by a faint `+`, chords a space
/// apart. `row` is the fill of the row the caps sit on, so a selected row's
/// chips lift off its band rather than the card.
pub fn keycap_spans(hint: &str, t: Tones, row: Color, selected: bool) -> Vec<Span<'static>> {
    let cap = Style::default()
        .bg(t.keycap_on(row))
        .fg(if selected { t.text() } else { t.muted() });
    let sep = Style::default().fg(t.faint()).bg(row);
    let mut out = Vec::new();
    for (i, chord) in hint.split(' ').filter(|c| !c.is_empty()).enumerate() {
        if i > 0 {
            out.push(Span::styled(" ", sep));
        }
        for (j, key) in chord_keys(chord).into_iter().enumerate() {
            if j > 0 {
                out.push(Span::styled("+", sep));
            }
            out.push(Span::styled(format!(" {key} "), cap));
        }
    }
    out
}

/// The keys of one chord: `Ctrl+Shift+P` is Ctrl, Shift and P, and a `+`
/// that is itself the key (`Ctrl++`) stays a key.
pub fn chord_keys(chord: &str) -> Vec<&str> {
    let mut keys = Vec::new();
    let mut rest = chord;
    while let Some(first) = rest.chars().next() {
        let skip = first.len_utf8();
        match rest[skip..].find('+') {
            Some(k) => {
                keys.push(&rest[..skip + k]);
                rest = &rest[skip + k + 1..];
            }
            None => {
                keys.push(rest);
                break;
            }
        }
    }
    keys
}

/// A cell colour as bytes for blending: RGB as is, the named and indexed
/// ANSI colours through the theme's palette, the terminal default as the
/// canvas (`bg`) or body text (`fg`).
fn cell_rgb(c: Color, theme: Theme, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    let ansi = theme.ansi();
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => ansi[0],
        Color::Red => ansi[1],
        Color::Green => ansi[2],
        Color::Yellow => ansi[3],
        Color::Blue => ansi[4],
        Color::Magenta => ansi[5],
        Color::Cyan => ansi[6],
        Color::Gray => ansi[7],
        Color::DarkGray => ansi[8],
        Color::LightRed => ansi[9],
        Color::LightGreen => ansi[10],
        Color::LightYellow => ansi[11],
        Color::LightBlue => ansi[12],
        Color::LightMagenta => ansi[13],
        Color::LightCyan => ansi[14],
        Color::White => ansi[15],
        Color::Indexed(i) if i < 16 => ansi[usize::from(i)],
        _ => fallback,
    }
}

/// Recede everything outside `keep` behind a modal on a `surfaces` theme,
/// the scrim Zed, Linear and Raycast draw under a command bar: each cell's
/// text sinks most of the way into its own background and the background
/// darkens a step, so the card is the only thing at full contrast. A no-op
/// on a boxed theme.
pub fn dim_backdrop(buf: &mut Buffer, keep: Rect, theme: Theme) {
    let Some(t) = tones(theme) else {
        return;
    };
    let area = buf.area;
    let sink = if theme.is_light() { 0.0 } else { 0.30 };
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if keep.contains(ratatui::layout::Position { x, y }) {
                continue;
            }
            let cell = &mut buf[(x, y)];
            let bg = cell_rgb(cell.bg, theme, t.canvas);
            let fg = cell_rgb(cell.fg, theme, t.text);
            let bg2 = lerp_rgb(bg, (0, 0, 0), sink);
            cell.set_fg(rgb_color(lerp_rgb(fg, bg2, 0.58)));
            cell.set_bg(rgb_color(bg2));
            cell.modifier.remove(Modifier::BOLD);
        }
    }
}

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

/// The codicon chevrons a `surfaces` theme folds rows with (`chevron-right`,
/// `chevron-down`), the pair the panel's own toggles already draw, so the
/// side bar and the panel speak one icon family.
pub const CHEVRON_FOLDED: char = '\u{eab6}';
pub const CHEVRON_UNFOLDED: char = '\u{eab4}';

/// The chevron a folding row shows: the codicon pair under `surfaces`, the
/// geometric triangles ([`crate::icons::CHEVRON_OPEN`]) a boxed theme has
/// always drawn. Both are one cell wide, so nothing after them moves.
pub fn chevron(theme: Theme, open: bool) -> char {
    match (theme.surfaces().is_some(), open) {
        (true, true) => CHEVRON_UNFOLDED,
        (true, false) => CHEVRON_FOLDED,
        (false, true) => crate::icons::CHEVRON_OPEN,
        (false, false) => crate::icons::CHEVRON_CLOSED,
    }
}

/// The boxed themes' section-header inks: a dim chevron and a near-white
/// bold label, shared by every stacked section so they can never drift.
const SECTION_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const SECTION_LABEL: Color = Color::Rgb(0xE8, 0xEE, 0xF8);

/// Paint the header of a stacked side-bar section (Outline, Timeline,
/// Dependencies, Open Editors, the agent lane, the commit graph) on `row`:
/// the chevron, the label and any `tail` spans after it (a count).
///
/// On a boxed theme this is the dim triangle and the bold near-white label
/// those sections have always drawn. Under `surfaces` the header steps back
/// a level from the view title: a muted codicon chevron, the label muted
/// while folded and body text while open (calm, so the rows beneath carry
/// the weight), and the pointer lifts the row the way it lifts a tree row,
/// with the label taking the strong ink. `row` is the clickable header row;
/// its last cell is the pane's right padding and stays out of the lift.
pub fn paint_section_header(
    buf: &mut Buffer,
    row: Rect,
    theme: Theme,
    open: bool,
    label: &str,
    tail: Vec<Span<'static>>,
    hovered: bool,
) {
    use ratatui::text::Line;
    use ratatui::widgets::{Paragraph, Widget};
    let row = row.intersection(buf.area);
    if row.width == 0 || row.height == 0 {
        return;
    }
    let chev = chevron(theme, open);
    let Some(t) = tones(theme) else {
        let mut spans = vec![
            Span::styled(
                format!("{chev} "),
                Style::default().fg(theme.ui(SECTION_DIM)),
            ),
            Span::styled(
                label.to_string(),
                Style::default()
                    .fg(theme.ui(SECTION_LABEL))
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        spans.extend(tail);
        Paragraph::new(Line::from(spans)).render(row, buf);
        return;
    };
    if hovered {
        let lift = Rect {
            width: row.width.saturating_sub(1).max(1),
            ..row
        };
        buf.set_style(lift, Style::default().bg(t.hover_on(t.chrome)));
    }
    let label_fg = if hovered {
        t.strong()
    } else if open {
        t.text()
    } else {
        t.muted()
    };
    let mut spans = vec![
        Span::styled(
            format!("{chev} "),
            Style::default().fg(if hovered { t.text() } else { t.muted() }),
        ),
        Span::styled(
            label.to_string(),
            Style::default().fg(label_fg).add_modifier(Modifier::BOLD),
        ),
    ];
    spans.extend(tail);
    buf.set_line(row.x, row.y, &Line::from(spans), row.width);
}

/// A text field on a `surfaces` theme: `rect`'s inner cells on the raised
/// tone, ringed by a one-eighth-block outline in `edge` that hugs them from
/// the frame cells around (`▁` above, `▔` below, `▕` and `▏` at the sides),
/// so the field reads as a recessed well with a crisp edge rather than a
/// line box. The corners stay blank: at an eighth of a cell the missing
/// corner pixel is not visible. The inner rect is a bordered block's, so
/// text and caret land exactly where they would inside a line box.
pub fn paint_field(buf: &mut Buffer, rect: Rect, t: Tones, edge: Color) {
    if rect.width < 2 || rect.height < 2 || rect.intersection(buf.area) != rect {
        return;
    }
    let inner = Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2);
    buf.set_style(inner, Style::default().bg(t.raised()));
    let (right, bottom) = (rect.right() - 1, rect.bottom() - 1);
    for x in rect.x..=right {
        let corner = x == rect.x || x == right;
        for (y, glyph) in [(rect.y, "\u{2581}"), (bottom, "\u{2594}")] {
            let cell = &mut buf[(x, y)];
            cell.set_symbol(if corner { " " } else { glyph });
            cell.set_fg(edge);
        }
    }
    for y in inner.top()..inner.bottom() {
        for (x, glyph) in [(rect.x, "\u{2595}"), (right, "\u{258f}")] {
            let cell = &mut buf[(x, y)];
            cell.set_symbol(glyph);
            cell.set_fg(edge);
        }
    }
}

/// A button's slab on a `surfaces` theme: a `fill`-coloured body whose first
/// and last rows are half blocks (`▄` over, `▀` under), so a three-row
/// button reads two rows tall, centred on its label row, with quadrant
/// notches (`▗▖▝▘`) softening the corners on the ends that are `round`.
/// An unrounded end is where a split button's halves join. A button shorter
/// than three rows is a plain slab. The click target stays the full `area`.
pub fn paint_soft_button(
    buf: &mut Buffer,
    area: Rect,
    fill: Color,
    round_left: bool,
    round_right: bool,
) {
    let area = area.intersection(buf.area);
    if area.width == 0 || area.height == 0 {
        return;
    }
    buf.set_style(
        Rect {
            y: area.y + u16::from(area.height >= 3),
            height: area.height - 2 * u16::from(area.height >= 3),
            ..area
        },
        Style::default().bg(fill),
    );
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_symbol(" ");
        }
    }
    if area.height < 3 {
        return;
    }
    let (right, bottom) = (area.right() - 1, area.bottom() - 1);
    for x in area.left()..=right {
        let (top, under) = match (x == area.x && round_left, x == right && round_right) {
            (true, _) => ("\u{2597}", "\u{259d}"),
            (_, true) => ("\u{2596}", "\u{2598}"),
            _ => ("\u{2584}", "\u{2580}"),
        };
        buf[(x, area.y)].set_symbol(top).set_fg(fill);
        buf[(x, bottom)].set_symbol(under).set_fg(fill);
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
/// each cell's own background. A row of text on the seam's row (the
/// scrubber on the editor's bottom edge) keeps its word gaps, as under the
/// focus rule.
pub fn paint_seam(buf: &mut Buffer, rect: Rect, edge: Edge, color: Color) {
    let rect = rect.intersection(buf.area);
    for y in rect.top()..rect.bottom() {
        paint_edge_row(buf, rect.x, y, rect.width, edge.glyph(), |_| color);
    }
}

/// The brand gradient's colour at cell `i` of `n` (teal to orange), or the
/// flat accent on a theme without the gradient. Kept for the boxed brand
/// theme's callers; a `surfaces` theme draws its rules with [`lip_color`].
#[allow(dead_code)]
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

/// The accent lip's colour at cell `i` of `n`: the accent at the leading
/// edge easing into `rest` across the first two fifths of the run (never
/// fewer than six cells), so a focused pane or a card is marked at its
/// corner and the rest of the edge is a quiet line. One hue, one meaning:
/// the accent says "this has the keyboard".
fn lip_color(accent: (u8, u8, u8), rest: (u8, u8, u8), i: u16, n: u16) -> Color {
    let span = (f32::from(n) * 0.4).max(6.0);
    let t = (f32::from(i) / span).clamp(0.0, 1.0);
    let eased = t * t * (3.0 - 2.0 * t);
    rgb_color(lerp_rgb(accent, rest, eased))
}

/// The focused pane's marker under a `surfaces` theme: a rule along the top
/// edge of its frame row, the accent easing into the hairline, broken
/// wherever the pane painted a title or a button there. It stands where VS
/// Code puts `tab.activeBorderTop`, and replaces the whole-pane focus box.
pub fn paint_focus_rule(buf: &mut Buffer, area: Rect, theme: Theme) {
    let area = area.intersection(buf.area);
    if area.height < 2 {
        return;
    }
    let Some(t) = tones(theme) else {
        paint_edge_row(buf, area.x, area.y, area.width, Edge::Top.glyph(), |i| {
            rule_color(theme, i, area.width)
        });
        return;
    };
    paint_edge_row(buf, area.x, area.y, area.width, Edge::Top.glyph(), |i| {
        lip_color(t.accent, t.hairline, i, area.width)
    });
}

/// A pane's top edge under a `surfaces` theme, focused or not: the focus
/// rule while it has the keyboard, the plain hairline otherwise, so the seam
/// between a header (tab strip, breadcrumbs, panel tabs) and the content
/// below it is always drawn and only its colour says where focus is.
pub fn paint_pane_top(buf: &mut Buffer, area: Rect, theme: Theme, focused: bool) {
    let Some(t) = tones(theme) else {
        return;
    };
    let area = area.intersection(buf.area);
    if area.height < 2 {
        return;
    }
    if focused {
        paint_focus_rule(buf, area, theme);
    } else {
        paint_edge_row(buf, area.x, area.y, area.width, Edge::Top.glyph(), |_| {
            t.hairline()
        });
    }
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
/// card edge in one-eighth blocks hugging the overlay's outer boundary: an
/// accent lip easing into the rim along the top, the rim on the other three
/// sides, with a one-cell shadow below and to the right; a title painted on
/// the top row keeps its cells.
pub fn paint_overlay_frame(buf: &mut Buffer, rect: Rect, theme: Theme) {
    let Some(t) = tones(theme) else {
        crate::gradient::paint_gradient_box(buf, rect);
        return;
    };
    paint_card_edge(buf, rect, theme, Some(t.accent));
}

/// [`paint_overlay_frame`] without the accent lip: the whole edge in the
/// rim. For hints and tooltips that never take the keyboard, so the accent
/// keeps meaning focus.
pub fn paint_quiet_frame(buf: &mut Buffer, rect: Rect, theme: Theme) {
    if tones(theme).is_none() {
        crate::gradient::paint_gradient_box(buf, rect);
        return;
    }
    paint_card_edge(buf, rect, theme, None);
}

/// [`paint_overlay_frame`] with the lip in `tint` instead of the accent: a
/// warning or a destructive confirmation marks its card in its own colour.
pub fn paint_tinted_frame(buf: &mut Buffer, rect: Rect, theme: Theme, tint: (u8, u8, u8)) {
    if tones(theme).is_none() {
        crate::gradient::paint_gradient_box(buf, rect);
        return;
    }
    paint_card_edge(buf, rect, theme, Some(tint));
}

fn paint_card_edge(buf: &mut Buffer, rect: Rect, theme: Theme, lip: Option<(u8, u8, u8)>) {
    let Some(t) = tones(theme) else {
        return;
    };
    let area = buf.area;
    if rect.width < 2 || rect.height < 2 || rect.intersection(area) != rect {
        return;
    }
    let (right, bottom) = (rect.right() - 1, rect.bottom() - 1);
    paint_edge_row(
        buf,
        rect.x,
        rect.y,
        rect.width,
        Edge::Top.glyph(),
        |i| match lip {
            Some(a) => lip_color(a, t.rim, i, rect.width),
            None => t.rim(),
        },
    );
    paint_edge_row(
        buf,
        rect.x,
        bottom,
        rect.width,
        Edge::Bottom.glyph(),
        |_| t.rim(),
    );
    for y in rect.y + 1..bottom {
        for (x, edge) in [(rect.x, Edge::Left), (right, Edge::Right)] {
            let cell = &mut buf[(x, y)];
            if is_frame_cell(cell) {
                cell.set_symbol(edge.glyph());
                cell.set_fg(t.rim());
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
    fn the_focus_rule_eases_from_the_accent_into_the_hairline_and_spares_the_title() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 24, 3));
        buf.set_string(1, 0, "EX", Style::default());
        let area = buf.area;
        paint_focus_rule(&mut buf, area, loch());
        let t = tones(loch()).unwrap();
        assert_eq!(buf[(0, 0)].symbol(), "\u{2594}");
        assert_eq!(buf[(0, 0)].fg, t.accent(), "the lip starts at the accent");
        assert_eq!(buf[(23, 0)].fg, t.hairline(), "and settles on the hairline");
        assert_eq!(buf[(1, 0)].symbol(), "E", "the title keeps its cells");
        assert_eq!(buf[(5, 1)].symbol(), " ", "only the top row is touched");
    }

    #[test]
    fn an_unfocused_pane_top_is_the_hairline() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 3));
        let area = buf.area;
        paint_pane_top(&mut buf, area, loch(), false);
        let t = tones(loch()).unwrap();
        for x in 0..10 {
            assert_eq!(buf[(x, 0)].symbol(), "\u{2594}");
            assert_eq!(buf[(x, 0)].fg, t.hairline());
        }
        let mut boxed = Buffer::empty(Rect::new(0, 0, 10, 3));
        paint_pane_top(&mut boxed, area, Theme::BLACK, true);
        assert_eq!(boxed, Buffer::empty(area), "a boxed theme draws nothing");
    }

    #[test]
    fn loch_tones_rank_text_above_muted_above_faint() {
        let t = tones(loch()).unwrap();
        let l = |c: (u8, u8, u8)| u32::from(c.0) + u32::from(c.1) + u32::from(c.2);
        assert!(l(t.strong) > l(t.text));
        assert!(l(t.text) > l(t.muted));
        assert!(l(t.muted) > l(t.faint));
        assert!(l(t.faint) > l(t.canvas));
        assert!(
            l(t.rim) > l(t.raised),
            "the rim reads against the card body"
        );
        assert!(tones(Theme::BLACK).is_none());
    }

    #[test]
    fn a_selected_row_is_a_band_with_the_accent_bar_only_while_focused() {
        let t = tones(loch()).unwrap();
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 2));
        buf.set_string(1, 0, "ab", Style::default().fg(Color::Red));
        paint_selected_row(&mut buf, Rect::new(0, 0, 8, 1), t, t.raised, true);
        assert_eq!(buf[(0, 0)].symbol(), ROW_BAR);
        assert_eq!(buf[(0, 0)].fg, t.accent());
        assert_eq!(buf[(1, 0)].bg, t.band());
        assert_eq!(buf[(1, 0)].fg, Color::Red, "text keeps its colour");
        assert_eq!(buf[(0, 1)].symbol(), " ", "only the row is touched");
        paint_selected_row(&mut buf, Rect::new(0, 1, 8, 1), t, t.raised, false);
        assert_eq!(buf[(0, 1)].symbol(), " ", "no bar without focus");
        assert_ne!(buf[(3, 1)].bg, t.band());
    }

    #[test]
    fn keycaps_put_each_key_of_a_chord_in_a_chip() {
        let t = tones(loch()).unwrap();
        let spans = keycap_spans("Ctrl+K Z", t, t.raised(), false);
        let text: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, [" Ctrl ", "+", " K ", " ", " Z "]);
        assert_eq!(spans[0].style.bg, Some(t.keycap_on(t.raised())));
        assert_eq!(spans[1].style.fg, Some(t.faint()));
        assert_eq!(chord_keys("Ctrl++"), ["Ctrl", "+"]);
    }

    #[test]
    fn the_backdrop_sinks_everything_but_the_card() {
        let t = tones(loch()).unwrap();
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
        buf.set_style(buf.area, Style::default().fg(t.text()).bg(t.chrome()));
        dim_backdrop(&mut buf, Rect::new(2, 1, 2, 1), loch());
        assert_eq!(buf[(2, 1)].fg, t.text(), "the card keeps its contrast");
        assert_ne!(buf[(0, 0)].fg, t.text());
        assert_ne!(buf[(0, 0)].bg, t.chrome());
        let mut boxed = Buffer::empty(Rect::new(0, 0, 6, 3));
        let before = boxed.clone();
        dim_backdrop(&mut boxed, Rect::new(2, 1, 2, 1), Theme::BLACK);
        assert_eq!(boxed, before);
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
    fn a_seam_along_a_row_keeps_the_space_between_words() {
        // The scrubber's label sits on the editor's bottom row, where the
        // seam against the panel runs.
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        buf.set_string(1, 0, " 2/5 \u{b7} old.py ", Style::default());
        let hair = Color::Rgb(0x23, 0x28, 0x2a);
        let area = buf.area;
        paint_seam(&mut buf, area, Edge::Bottom, hair);
        let row: String = (0..20).map(|x| buf[(x, 0)].symbol()).collect();
        let seam = Edge::Bottom.glyph();
        assert_eq!(
            row,
            format!("{seam}{seam}2/5 \u{b7} old.py{}", seam.repeat(6))
        );
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
        assert_eq!(buf[(8, 3)].fg, tones(theme).unwrap().rim());
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

    /// A boxed theme's section header is exactly what the six stacked
    /// sections drew before they shared it: the dim triangle and the bold
    /// near-white label.
    #[test]
    fn a_boxed_section_header_is_the_dim_triangle_and_bold_label() {
        use ratatui::text::Line;
        use ratatui::widgets::{Paragraph, Widget};
        let theme = Theme::BLACK;
        let row = Rect::new(1, 0, 20, 1);
        let mut ours = Buffer::empty(Rect::new(0, 0, 22, 1));
        paint_section_header(&mut ours, row, theme, false, "OUTLINE", Vec::new(), true);
        let mut theirs = Buffer::empty(Rect::new(0, 0, 22, 1));
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", crate::icons::CHEVRON_CLOSED),
                Style::default().fg(theme.ui(SECTION_DIM)),
            ),
            Span::styled(
                "OUTLINE",
                Style::default()
                    .fg(theme.ui(SECTION_LABEL))
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .render(row, &mut theirs);
        assert_eq!(ours, theirs);
    }

    /// Croft Loch's section header: a muted codicon chevron, the label
    /// muted while folded and body text while open, and a neutral lift
    /// with the strong ink under the pointer that stops short of the pane's
    /// right padding.
    #[test]
    fn a_croft_loch_section_header_is_calm_and_lifts_under_the_pointer() {
        let theme = loch();
        let t = tones(theme).unwrap();
        let row = Rect::new(1, 0, 20, 1);
        let paint = |open: bool, hovered: bool| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 22, 1));
            buf.set_style(buf.area, Style::default().bg(t.chrome()));
            paint_section_header(&mut buf, row, theme, open, "OUTLINE", Vec::new(), hovered);
            buf
        };
        let folded = paint(false, false);
        assert_eq!(folded[(1, 0)].symbol(), CHEVRON_FOLDED.to_string());
        assert_eq!(folded[(1, 0)].fg, t.muted());
        assert_eq!(folded[(3, 0)].fg, t.muted(), "a folded label is muted");
        assert_eq!(folded[(3, 0)].bg, t.chrome());
        let open = paint(true, false);
        assert_eq!(open[(1, 0)].symbol(), CHEVRON_UNFOLDED.to_string());
        assert_eq!(open[(3, 0)].fg, t.text());
        let hovered = paint(true, true);
        assert_eq!(hovered[(3, 0)].fg, t.strong());
        assert_eq!(hovered[(3, 0)].bg, t.hover_on(t.chrome));
        assert_eq!(hovered[(20, 0)].bg, t.chrome(), "the padding stays chrome");
    }

    /// A field's edge hugs its raised inner cells from the frame cells
    /// around them; the corners stay blank and the inner rect is untouched.
    #[test]
    fn a_field_is_a_raised_well_inside_an_eighth_block_edge() {
        let t = tones(loch()).unwrap();
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
        let area = buf.area;
        paint_field(&mut buf, area, t, t.rim());
        let rows: Vec<String> = (0..3)
            .map(|y| (0..6).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        assert_eq!(rows[0], " \u{2581}\u{2581}\u{2581}\u{2581} ");
        assert_eq!(rows[1], "\u{2595}    \u{258f}");
        assert_eq!(rows[2], " \u{2594}\u{2594}\u{2594}\u{2594} ");
        assert_eq!(buf[(2, 1)].bg, t.raised());
        assert_eq!(buf[(0, 1)].fg, t.rim());
    }

    /// A soft button is three rows whose caps are half blocks, so it reads
    /// two rows tall; only its rounded ends take the quadrant notches.
    #[test]
    fn a_soft_button_has_half_block_caps_and_notched_round_ends() {
        let fill = Color::Rgb(0x0e, 0x7e, 0x76);
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 3));
        let area = buf.area;
        paint_soft_button(&mut buf, area, fill, true, false);
        let rows: Vec<String> = (0..3)
            .map(|y| (0..5).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        assert_eq!(rows[0], "\u{2597}\u{2584}\u{2584}\u{2584}\u{2584}");
        assert_eq!(rows[2], "\u{259d}\u{2580}\u{2580}\u{2580}\u{2580}");
        assert_eq!(buf[(1, 0)].fg, fill);
        assert_eq!(
            buf[(1, 0)].bg,
            Color::Reset,
            "the cap keeps what is under it"
        );
        assert_eq!(buf[(2, 1)].bg, fill);
    }

    #[test]
    fn pane_fill_is_chrome_only_for_surfaces_side_bars() {
        let s = loch().surfaces().unwrap();
        assert_eq!(pane_fill(loch(), true), s.chrome());
        assert_eq!(pane_fill(loch(), false), loch().editor_bg());
        assert_eq!(pane_fill(Theme::BLACK, true), Theme::BLACK.editor_bg());
    }
}
