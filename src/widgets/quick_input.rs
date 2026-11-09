//! The quick-input card a `surfaces` theme (Croft Loch) draws for the
//! Command Palette, Go to File and the list pickers, one anatomy for all
//! three, after VS Code's quick input and Zed's command bar:
//!
//! ```text
//! ▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔   accent lip, no title
//! ▏ › query|                     12 matches ▕   prompt glyph, count
//! ▏▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▕   the field's underline
//! ▎   View: Toggle Terminal     [Ctrl][J]   ▕   selected: band + bar
//! ▏   Terminal: Create New ...              ▕
//! ▏ ↑↓ navigate   ↵ run   esc close          ▕   footer on chrome
//! ▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁
//! ```
//!
//! The list keeps the rows the boxed themes give it (three below the card's
//! top edge), so every hit test stays where it was; the card is as tall as
//! its results, its top fixed where the tallest card's would be, so it
//! grows and shrinks downward while the user types. The query uses the
//! terminal's own blinking caret: [`paint_card`] returns where it goes.
//!
//! Boxed themes never reach this module; their pickers keep their own
//! drawing, byte for byte.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;
use crate::widgets::surface::{self, Tones};

/// The fewest list rows a card shows, so a single result (or the empty
/// state's line) is not wedged between the rule and the footer.
pub const MIN_LIST_ROWS: u16 = 3;

/// Card rows that are not the list: the top edge, the input row, the rule
/// under it, the footer and the bottom edge.
const CHROME_ROWS: u16 = 5;

/// The prompt glyph's (and a file icon's) column, from the card's left edge:
/// the edge itself holds a selected row's accent bar, then one pad cell.
const GLYPH_COL: u16 = 2;

/// Where text starts, from the card's left edge: past the glyph slot and a
/// space, so a row's label lines up under the typed query.
const TEXT_COL: u16 = 4;

/// The geometry of one quick-input card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Card {
    /// The whole card, edges included: what the widget stores as its
    /// `last_rect` for the click-outside test.
    pub rect: Rect,
    /// The result rows: three below the top edge, between the side edges.
    pub list: Rect,
    /// The footer row, when the card has room for one.
    footer: Option<u16>,
}

impl Card {
    /// Lay a card out in `area`: `width` wide, centred, never taller than
    /// `max_height`, and its top where a `max_height` card's top would sit
    /// (a quarter of the way down, or the middle with `center`, as Quick
    /// Input Position says). The list is sized to `rows` results, at least
    /// [`MIN_LIST_ROWS`], at most what the height leaves. A terminal too
    /// short for the footer drops it before it drops a result row.
    pub fn layout(area: Rect, width: u16, max_height: u16, rows: usize, center: bool) -> Card {
        let width = width.min(area.width);
        let max_h = max_height.min(area.height);
        let slack = area.height.saturating_sub(max_h);
        let y = area.y + if center { slack / 2 } else { slack / 4 };
        let footer = max_h >= CHROME_ROWS + MIN_LIST_ROWS;
        let chrome = if footer { CHROME_ROWS } else { CHROME_ROWS - 1 };
        let room = max_h.saturating_sub(chrome);
        let wanted = u16::try_from(rows).unwrap_or(u16::MAX).max(MIN_LIST_ROWS);
        let list_h = wanted.min(room);
        let height = (chrome + list_h).min(max_h);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y,
            width,
            height,
        };
        let list = Rect {
            x: rect.x + 1,
            y: rect.y + 3,
            width: rect.width.saturating_sub(2),
            height: list_h,
        };
        Card {
            rect,
            list,
            footer: footer.then_some(list.y + list_h),
        }
    }

    /// The first column past the content: one gutter cell short of the
    /// right edge, so keycaps and the count never touch the rim.
    fn content_end(&self) -> u16 {
        self.rect.right().saturating_sub(2)
    }
}

/// What a card's input row and footer say.
pub struct Input<'a> {
    /// The accent glyph before the query (`>` for commands, `›` for files).
    pub glyph: &'a str,
    pub query: &'a str,
    /// The caret, in chars into `query`.
    pub cursor: usize,
    /// Shown, muted and italic, while the query is empty.
    pub placeholder: &'a str,
    /// Right-aligned in muted ink: how many results there are.
    pub count: &'a str,
    /// What Enter does, for the footer (`run`, `open`, `select`).
    pub verb: &'a str,
    /// The count names what Enter will do (a list picker's title while the
    /// user filters), so it keeps its place on the row: the query scrolls
    /// in what is left instead of crowding the count out.
    pub keep_count: bool,
}

/// Paint the card: the raised body, the edge (accent lip, rim, shadow), the
/// input row, the rule under it and the footer on the chrome tone. Returns
/// the cell for the terminal's caret, or `None` when the row has no room.
pub fn paint_card(
    buf: &mut Buffer,
    card: &Card,
    theme: Theme,
    t: Tones,
    input: &Input,
) -> Option<(u16, u16)> {
    let rect = card.rect;
    if rect.width < 2 || rect.height < 2 || rect.intersection(buf.area) != rect {
        return None;
    }
    Clear.render(rect, buf);
    buf.set_style(rect, Style::default().bg(t.raised()).fg(t.text()));
    if let Some(fy) = card.footer {
        // The footer and the bottom edge under it share the chrome tone,
        // so the footer reads as a tray the rim closes, not a stripe.
        let tray = Rect {
            y: fy,
            height: rect.bottom().saturating_sub(fy),
            ..rect
        };
        buf.set_style(tray, Style::default().bg(t.chrome()));
    }
    surface::paint_overlay_frame(buf, rect, theme);
    let end = card.content_end();
    let caret = if rect.height >= 3 {
        paint_input_row(buf, rect.x, rect.y + 1, end, t, input)
    } else {
        None
    };
    if rect.height >= 4 {
        let rule = "\u{2594}".repeat(usize::from(rect.width - 2));
        buf.set_string(
            rect.x + 1,
            rect.y + 2,
            rule,
            Style::default().fg(t.hairline()),
        );
    }
    if let Some(fy) = card.footer {
        let key = Style::default().fg(t.text());
        let word = Style::default().fg(t.muted());
        let hints = Line::from(vec![
            Span::styled("\u{2191}\u{2193}", key),
            Span::styled(" navigate   ", word),
            Span::styled("\u{21b5}", key),
            Span::styled(format!(" {}   ", input.verb), word),
            Span::styled("esc", key),
            Span::styled(" close", word),
        ]);
        let x = rect.x + GLYPH_COL;
        buf.set_line(x, fy, &hints, end.saturating_sub(x));
    }
    caret
}

/// The input row: the accent glyph, the query (or the placeholder) and the
/// count at the right. A query wider than the field scrolls to keep the
/// caret in view, a `…` standing for the hidden start. Returns the caret.
/// The fewest query cells a kept count leaves beside itself; below that the
/// count yields to the query as an ordinary count does.
const MIN_KEPT_QUERY: usize = 6;

fn paint_input_row(
    buf: &mut Buffer,
    left: u16,
    y: u16,
    end: u16,
    t: Tones,
    input: &Input,
) -> Option<(u16, u16)> {
    let glyph_x = left + GLYPH_COL;
    if glyph_x < end {
        buf.set_stringn(
            glyph_x,
            y,
            input.glyph,
            usize::from(end - glyph_x),
            Style::default().fg(t.accent()),
        );
    }
    let x0 = left + TEXT_COL;
    if x0 >= end {
        return None;
    }
    let field = usize::from(end - x0);
    let count_w = input.count.width();
    // A kept count claims its cells (and a two-cell gap) first, as long as
    // a few cells of query still fit beside it; the query then scrolls in
    // the rest, so typing never pushes the count off the row.
    let kept = input.keep_count && count_w > 0 && count_w + 2 + MIN_KEPT_QUERY <= field;
    let end_q = if kept {
        end - u16::try_from(count_w + 2).unwrap_or(0)
    } else {
        end
    };
    let field_q = usize::from(end_q - x0);
    let (text_w, caret_x) = if input.query.is_empty() {
        let style = Style::default()
            .fg(t.muted())
            .add_modifier(Modifier::ITALIC);
        let (after_x, _) = buf.set_stringn(x0, y, input.placeholder, field_q, style);
        (usize::from(after_x - x0), x0)
    } else {
        let cursor = input.cursor.min(input.query.chars().count());
        let (skip, lead) = crate::widgets::input_prompt::field_scroll(input.query, cursor, field_q);
        let mut x = x0;
        if lead {
            buf.set_string(x, y, "\u{2026}", Style::default().fg(t.faint()));
            x += 1;
        }
        let visible: String = input.query.chars().skip(skip).collect();
        let before: String = input.query.chars().skip(skip).take(cursor - skip).collect();
        let caret_x = x + u16::try_from(before.width()).unwrap_or(u16::MAX);
        let (after_x, _) = buf.set_stringn(
            x,
            y,
            &visible,
            usize::from(end_q.saturating_sub(x)),
            Style::default().fg(t.text()),
        );
        (
            usize::from(after_x - x0),
            caret_x.min(end_q.max(x0 + 1) - 1),
        )
    };
    // The count only where it clears the text and the caret by two cells,
    // unless it is kept, when its cells were set aside above.
    if count_w > 0 && (kept || text_w + 1 + 2 + count_w <= field) {
        let cx = end - u16::try_from(count_w).unwrap_or(0);
        buf.set_string(cx, y, input.count, Style::default().fg(t.muted()));
    }
    Some((caret_x, y))
}

/// One result row's content, for [`paint_row`].
pub struct Row {
    /// The row has the selection (and the list the keyboard).
    pub selected: bool,
    /// A file icon, in the prompt glyph's column.
    pub glyph: Option<Span<'static>>,
    /// The label, at the text column.
    pub label: Vec<Span<'static>>,
    /// Right-aligned against the gutter: the keycaps.
    pub trail: Vec<Span<'static>>,
}

/// Paint result row `i` of the list (0 is the top visible row). A selected
/// row gets the band with the accent bar in the card's left edge, so the
/// band spans the card. `glyph` (a file icon) sits in the prompt glyph's
/// column, `label` at the text column, cut with a `…` when it runs out of
/// room, and `trail` (keycaps) right-aligned against the gutter, dropped
/// when the label leaves no room for it.
pub fn paint_row(buf: &mut Buffer, card: &Card, i: u16, t: Tones, row: Row) {
    let Row {
        selected,
        glyph,
        label,
        trail,
    } = row;
    if i >= card.list.height {
        return;
    }
    let y = card.list.y + i;
    let rect = card.rect;
    if selected {
        let row = Rect {
            x: rect.x,
            y,
            width: rect.width.saturating_sub(1),
            height: 1,
        };
        surface::paint_selected_row(buf, row, t, t.raised, true);
    }
    let end = card.content_end();
    if let Some(g) = glyph {
        let gx = rect.x + GLYPH_COL;
        if gx < end {
            buf.set_span(gx, y, &g, end - gx);
        }
    }
    let x0 = rect.x + TEXT_COL;
    if x0 >= end {
        return;
    }
    let room = usize::from(end - x0);
    let label_w: usize = label.iter().map(Span::width).sum();
    let trail_w: usize = trail.iter().map(Span::width).sum();
    let label_end = if trail_w > 0 && label_w + 2 + trail_w <= room {
        let tx = end - u16::try_from(trail_w).unwrap_or(0);
        buf.set_line(tx, y, &Line::from(trail), end - tx);
        tx - 2
    } else {
        end
    };
    let line = Line::from(label);
    let fit = usize::from(label_end - x0);
    if label_w <= fit {
        buf.set_line(x0, y, &line, label_end - x0);
    } else if fit > 0 {
        let (cut, _) = buf.set_line(x0, y, &line, label_end - x0 - 1);
        buf.set_string(cut, y, "\u{2026}", Style::default().fg(t.muted()));
    }
}

/// The empty state: one muted italic line, centred in the list.
pub fn paint_empty(buf: &mut Buffer, card: &Card, t: Tones, message: &str) {
    let list = card.list;
    if list.height == 0 || list.width == 0 {
        return;
    }
    let w = u16::try_from(message.width())
        .unwrap_or(u16::MAX)
        .min(list.width);
    let x = list.x + (list.width - w) / 2;
    let y = list.y + (list.height - 1) / 2;
    buf.set_stringn(
        x,
        y,
        message,
        usize::from(w),
        Style::default()
            .fg(t.muted())
            .add_modifier(Modifier::ITALIC),
    );
}

/// Where the visible rows sit in a longer list: the card's right edge
/// brightens to muted ink along the stretch the rows cover, so an
/// overflowing list says it scrolls without a scrollbar's column.
pub fn paint_scroll_cue(buf: &mut Buffer, card: &Card, t: Tones, total: usize, scroll: usize) {
    let track = usize::from(card.list.height);
    if track == 0 || total <= track || card.rect.width < 2 {
        return;
    }
    let thumb = (track * track).div_ceil(total).clamp(1, track);
    let travel = track - thumb;
    let max_scroll = total - track;
    let start = (scroll.min(max_scroll) * travel + max_scroll / 2) / max_scroll;
    let x = card.rect.right() - 1;
    for row in start..start + thumb {
        let y = card.list.y + u16::try_from(row).unwrap_or(0);
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol("\u{2595}");
            cell.set_fg(t.muted());
        }
    }
}

/// The byte offsets in `hay` of the characters `query` matched, for the
/// accent highlight: the first case-insensitive run of the whole query when
/// there is one (what a typed word means), else the in-order subsequence
/// the fuzzy filters accept, else nothing (a row kept for its keyword or
/// its English title shows no highlight rather than a wrong one).
pub fn match_offsets(hay: &str, query: &str) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    run_offsets(hay, query)
        .or_else(|| crate::widgets::file_tree::subsequence_match(hay, query))
        .unwrap_or_default()
}

/// The byte offsets of the first case-insensitive run of all of `query` in
/// `hay`, compared char by char (as the filters lower-case), or `None`.
pub fn run_offsets(hay: &str, query: &str) -> Option<Vec<usize>> {
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let chars: Vec<(usize, char)> = hay.char_indices().collect();
    if needle.is_empty() || needle.len() > chars.len() {
        return None;
    }
    let same = |c: char, want: char| c.to_lowercase().eq([want]);
    (0..=chars.len() - needle.len()).find_map(|start| {
        let run = &chars[start..start + needle.len()];
        run.iter()
            .zip(&needle)
            .all(|(&(_, c), &w)| same(c, w))
            .then(|| run.iter().map(|&(b, _)| b).collect())
    })
}

/// Spans for `text`, which starts at byte `from` of the string `offsets`
/// index: the characters at those offsets in `hit`, the rest in `base`, a
/// run of either kept in one span.
pub fn highlighted(
    text: &str,
    from: usize,
    offsets: &[usize],
    base: Style,
    hit: Style,
) -> Vec<Span<'static>> {
    if offsets.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_hit = false;
    for (i, c) in text.char_indices() {
        let is_hit = offsets.binary_search(&(from + i)).is_ok();
        if is_hit != run_hit && !run.is_empty() {
            let style = if run_hit { hit } else { base };
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        run_hit = is_hit;
        run.push(c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_hit { hit } else { base }));
    }
    spans
}

/// The style a matched character takes: the accent, bold.
pub fn hit_style(t: Tones) -> Style {
    Style::default().fg(t.accent()).add_modifier(Modifier::BOLD)
}

/// The count at the input row's right: the whole list's size (`412
/// commands`) while nothing is typed, the number of matches after; `capped`
/// says the list stops at its limit, so there may be more (`40+ matches`).
pub fn count_label(n: usize, filtering: bool, capped: bool, one: &str, many: &str) -> String {
    if filtering {
        let plus = if capped { "+" } else { "" };
        let noun = if n == 1 && !capped {
            "match"
        } else {
            "matches"
        };
        format!("{n}{plus} {noun}")
    } else {
        format!("{n} {}", if n == 1 { one } else { many })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loch() -> Theme {
        Theme::from_id("loch")
    }

    fn input<'a>(query: &'a str) -> Input<'a> {
        Input {
            glyph: ">",
            query,
            cursor: query.chars().count(),
            placeholder: "Type a command",
            count: "3 matches",
            verb: "run",
            keep_count: false,
        }
    }

    fn row(buf: &Buffer, y: u16, rect: Rect) -> String {
        (rect.x..rect.right())
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn the_card_hugs_its_results_and_keeps_its_top() {
        let area = Rect::new(0, 0, 100, 40);
        let few = Card::layout(area, 70, 24, 2, false);
        let many = Card::layout(area, 70, 24, 200, false);
        assert_eq!(few.rect.y, many.rect.y, "the top never moves");
        assert_eq!(few.list.height, MIN_LIST_ROWS, "one result is not cramped");
        assert_eq!(few.rect.height, MIN_LIST_ROWS + CHROME_ROWS);
        assert_eq!(many.rect.height, 24);
        assert_eq!(
            many.list.y,
            many.rect.y + 3,
            "the list stays three rows down"
        );
        assert_eq!(few.footer, Some(few.list.bottom()));
    }

    #[test]
    fn a_short_terminal_drops_the_footer_before_a_row() {
        let card = Card::layout(Rect::new(0, 0, 80, 6), 60, 6, 9, false);
        assert_eq!(card.footer, None);
        assert_eq!(card.list.height, 2);
        assert_eq!(card.rect.bottom(), 6);
        for h in 0..9 {
            let area = Rect::new(0, 0, 30, h);
            let card = Card::layout(area, 40, 10, 5, false);
            let mut buf = Buffer::empty(area);
            paint_card(
                &mut buf,
                &card,
                loch(),
                surface::tones(loch()).unwrap(),
                &input("x"),
            );
            assert!(card.rect.bottom() <= h);
        }
    }

    #[test]
    fn the_input_row_has_the_glyph_the_query_the_count_and_the_caret() {
        let t = surface::tones(loch()).unwrap();
        let area = Rect::new(0, 0, 60, 20);
        let card = Card::layout(area, 50, 12, 3, false);
        let mut buf = Buffer::empty(area);
        let caret = paint_card(&mut buf, &card, loch(), t, &input("term")).unwrap();
        let r = card.rect;
        let line = row(&buf, r.y + 1, r);
        assert!(line.contains("> term"), "{line:?}");
        assert!(
            line.trim_end_matches('\u{2595}').ends_with("3 matches "),
            "{line:?}"
        );
        assert_eq!(
            buf[(r.x + 2, r.y + 1)].fg,
            t.accent(),
            "the glyph is the accent"
        );
        assert_eq!(buf[(r.x + 4, r.y + 1)].fg, t.text());
        assert!(!buf[(r.x + 4, r.y + 1)].modifier.contains(Modifier::BOLD));
        assert_eq!(caret, (r.x + 8, r.y + 1), "the caret sits after the query");
        let rule = row(&buf, r.y + 2, r);
        assert_eq!(
            rule,
            format!("\u{258f}{}\u{2595}", "\u{2594}".repeat(48)),
            "an eighth-block rule under the field"
        );
        assert_eq!(buf[(r.x + 5, r.y + 2)].fg, t.hairline());
        assert!(!rule.contains('\u{2500}'), "no box-drawing line");
    }

    #[test]
    fn an_empty_query_shows_the_placeholder_muted_and_italic() {
        let t = surface::tones(loch()).unwrap();
        let area = Rect::new(0, 0, 60, 20);
        let card = Card::layout(area, 50, 12, 3, false);
        let mut buf = Buffer::empty(area);
        let caret = paint_card(&mut buf, &card, loch(), t, &input("")).unwrap();
        let r = card.rect;
        assert!(row(&buf, r.y + 1, r).contains("Type a command"));
        let cell = &buf[(r.x + 4, r.y + 1)];
        assert_eq!(cell.fg, t.muted());
        assert!(cell.modifier.contains(Modifier::ITALIC));
        assert_eq!(caret, (r.x + 4, r.y + 1), "the caret waits at the start");
    }

    #[test]
    fn the_footer_sits_on_the_chrome_tone_with_the_keys() {
        let t = surface::tones(loch()).unwrap();
        let area = Rect::new(0, 0, 60, 20);
        let card = Card::layout(area, 50, 12, 3, false);
        let mut buf = Buffer::empty(area);
        paint_card(&mut buf, &card, loch(), t, &input(""));
        let fy = card.footer.unwrap();
        assert_eq!(fy, card.rect.bottom() - 2);
        let line = row(&buf, fy, card.rect);
        assert!(line.contains("navigate"), "{line:?}");
        assert!(line.contains("\u{21b5} run"), "{line:?}");
        assert!(line.contains("esc close"), "{line:?}");
        assert_eq!(buf[(card.rect.x + 6, fy)].bg, t.chrome());
        assert_eq!(buf[(card.rect.x + 6, fy + 1)].bg, t.chrome());
        assert_eq!(
            buf[(card.rect.x + 2, fy)].fg,
            t.text(),
            "the keys in text ink"
        );
        assert_eq!(buf[(card.rect.x + 6, fy)].fg, t.muted(), "the words muted");
    }

    #[test]
    fn a_selected_row_puts_the_bar_on_the_cards_edge_and_keycaps_clear_the_rim() {
        let t = surface::tones(loch()).unwrap();
        let area = Rect::new(0, 0, 60, 20);
        let card = Card::layout(area, 50, 12, 3, false);
        let mut buf = Buffer::empty(area);
        paint_card(&mut buf, &card, loch(), t, &input(""));
        let caps = surface::keycap_spans("Ctrl+J", t, t.band(), true);
        let label = vec![Span::raw("View: Toggle Terminal")];
        paint_row(
            &mut buf,
            &card,
            0,
            t,
            Row {
                selected: true,
                glyph: None,
                label,
                trail: caps,
            },
        );
        let (r, y) = (card.rect, card.list.y);
        assert_eq!(buf[(r.x, y)].symbol(), surface::ROW_BAR);
        assert_eq!(buf[(r.x, y)].fg, t.accent());
        assert_eq!(buf[(r.x + 1, y)].bg, t.band(), "the band spans the card");
        assert_eq!(buf[(r.right() - 2, y)].bg, t.band());
        assert_eq!(
            buf[(r.right() - 2, y)].symbol(),
            " ",
            "a gutter before the rim"
        );
        let line = row(&buf, y, r);
        assert!(
            line.starts_with("\u{258e}   View: Toggle Terminal"),
            "{line:?}"
        );
        assert!(line.contains(" Ctrl + J  \u{2595}"), "{line:?}");
        assert!(!line.contains('>'), "no ASCII marker");
    }

    #[test]
    fn a_label_too_long_for_the_row_ends_in_an_ellipsis_and_drops_its_keys() {
        let t = surface::tones(loch()).unwrap();
        let area = Rect::new(0, 0, 30, 12);
        let card = Card::layout(area, 30, 10, 1, false);
        let mut buf = Buffer::empty(area);
        paint_card(&mut buf, &card, loch(), t, &input(""));
        let caps = surface::keycap_spans("Ctrl+J", t, t.raised(), false);
        let label = vec![Span::raw("a".repeat(40))];
        paint_row(
            &mut buf,
            &card,
            0,
            t,
            Row {
                selected: false,
                glyph: None,
                label,
                trail: caps,
            },
        );
        let line = row(&buf, card.list.y, card.rect);
        assert!(line.contains("a\u{2026} \u{2595}"), "{line:?}");
        assert!(!line.contains("Ctrl"), "{line:?}");
    }

    #[test]
    fn the_empty_state_is_a_centred_muted_italic_line() {
        let t = surface::tones(loch()).unwrap();
        let area = Rect::new(0, 0, 60, 20);
        let card = Card::layout(area, 50, 12, 0, false);
        let mut buf = Buffer::empty(area);
        paint_card(&mut buf, &card, loch(), t, &input("zzz"));
        paint_empty(&mut buf, &card, t, "No matching commands");
        let y = card.list.y + 1;
        let x = card.list.x + (card.list.width - 20) / 2;
        assert_eq!(buf[(x, y)].symbol(), "N");
        let (left, right) = (x - card.list.x, card.list.right() - (x + 20));
        assert!(left.abs_diff(right) <= 1, "centred: {left} / {right}");
        assert_eq!(buf[(x, y)].fg, t.muted());
        assert!(buf[(x, y)].modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn an_overflowing_list_lights_its_stretch_of_the_right_edge() {
        let t = surface::tones(loch()).unwrap();
        let area = Rect::new(0, 0, 60, 20);
        let card = Card::layout(area, 50, 10, 40, false);
        let mut buf = Buffer::empty(area);
        paint_card(&mut buf, &card, loch(), t, &input(""));
        paint_scroll_cue(&mut buf, &card, t, 40, 0);
        let x = card.rect.right() - 1;
        assert_eq!(buf[(x, card.list.y)].fg, t.muted(), "the thumb at the top");
        assert_eq!(buf[(x, card.list.bottom() - 1)].fg, t.rim(), "the track");
    }

    #[test]
    fn matches_prefer_a_contiguous_run_then_fall_back_to_a_subsequence() {
        assert_eq!(
            match_offsets("View: Toggle Terminal", "term"),
            [13, 14, 15, 16]
        );
        assert_eq!(match_offsets("src/foo/bar.rs", "fbr"), [4, 8, 10]);
        assert_eq!(match_offsets("abc", "xyz"), Vec::<usize>::new());
        assert_eq!(match_offsets("abc", "  "), Vec::<usize>::new());
        let t = surface::tones(loch()).unwrap();
        let spans = highlighted(
            "Toggle",
            6,
            &[6, 7],
            Style::default().fg(t.text()),
            hit_style(t),
        );
        let text: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, ["To", "ggle"]);
        assert_eq!(spans[0].style, hit_style(t));
    }

    #[test]
    fn the_count_names_the_list_then_the_matches() {
        assert_eq!(
            count_label(412, false, false, "command", "commands"),
            "412 commands"
        );
        assert_eq!(count_label(1, true, false, "file", "files"), "1 match");
        assert_eq!(count_label(40, true, true, "file", "files"), "40+ matches");
    }
}
