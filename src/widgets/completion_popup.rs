use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, StatefulWidget, Widget},
};

use crate::lsp::CompletionItem;

const MAX_VISIBLE_ITEMS: usize = 10;
const MIN_WIDTH: u16 = 24;
const MAX_WIDTH: u16 = 60;
/// The details pane beside the list (#1527): narrower than this it is not
/// worth drawing, and wider reads as a page rather than a note.
const DETAILS_MIN_WIDTH: u16 = 24;
const DETAILS_MAX_WIDTH: u16 = 60;
/// Rows of text the details pane shows before it cuts the documentation.
const DETAILS_MAX_LINES: u16 = 12;

/// Display width of `s` in cells.
fn cells(s: &str) -> u16 {
    unicode_width::UnicodeWidthStr::width(s).min(u16::MAX as usize) as u16
}

/// `s` cut to `max` cells, ending in `…` when anything was cut.
fn fit(s: &str, max: u16) -> String {
    if cells(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0u16;
    for c in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) as u16;
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Documentation as the lines the details pane shows: Markdown's code
/// fences dropped (their contents kept), trailing space trimmed, and runs
/// of blank lines folded to one.
fn doc_lines(doc: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in doc.lines() {
        if line.trim_start().starts_with("```") {
            continue;
        }
        let line = line.trim_end();
        if line.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(line.to_string());
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

pub struct CompletionPopup {
    pub items: Vec<CompletionItem>,
    pub prefix: String,
    pub selected: usize,
    pub anchor: (u16, u16),
    pub path: PathBuf,
    pub request_id: u64,
    /// Black theme: gradient border + muted dark-teal selection instead of the
    /// legacy bright-blue. Set by the app before render from `popup_gradient`.
    pub theme: crate::theme::Theme,
}

impl CompletionPopup {
    pub fn new(
        items: Vec<CompletionItem>,
        prefix: String,
        anchor: (u16, u16),
        path: PathBuf,
        request_id: u64,
    ) -> Self {
        Self {
            items,
            prefix,
            selected: 0,
            anchor,
            path,
            request_id,
            theme: crate::theme::Theme::default(),
        }
    }

    pub fn set_prefix(&mut self, prefix: String) {
        self.prefix = prefix;
        self.selected = 0;
    }

    pub fn visible_indices(&self) -> Vec<usize> {
        if self.prefix.is_empty() {
            return (0..self.items.len()).collect();
        }
        let needle = self.prefix.to_ascii_lowercase();
        (0..self.items.len())
            .filter(|&i| matches_typed(&self.items[i], &needle))
            .collect()
    }

    pub fn visible_is_empty(&self) -> bool {
        if self.prefix.is_empty() {
            return self.items.is_empty();
        }
        let needle = self.prefix.to_ascii_lowercase();
        !self.items.iter().any(|item| matches_typed(item, &needle))
    }

    pub fn move_up(&mut self) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = visible.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % visible.len();
    }

    pub fn selected_item(&self) -> Option<&CompletionItem> {
        let visible = self.visible_indices();
        visible.get(self.selected).and_then(|&i| self.items.get(i))
    }

    /// Full insertion text for the selected item. The caller is expected
    /// to first backspace `prefix.chars().count()` chars from the editor
    /// so that an already-typed prefix is replaced (handles the typo case
    /// `calX` -> `calc_n_gram` correctly, where simply appending would
    /// leave the X behind).
    pub fn selected_label(&self) -> Option<String> {
        // The edit's text first: when an item has a `textEdit`, LSP says
        // `insertText` is ignored (rust-analyzer sends a snippet only there,
        // and the label alone dropped its `($0)`).
        self.selected_item().map(|item| {
            item.text_edit
                .as_ref()
                .map(|e| e.new_text.clone())
                .or_else(|| item.insert_text.clone())
                .unwrap_or_else(|| item.label.clone())
        })
    }

    /// Whether the selected item's insertion text is a snippet body (tab stops)
    /// the caller must expand rather than insert verbatim.
    pub fn selected_is_snippet(&self) -> bool {
        self.selected_item().is_some_and(|item| item.is_snippet)
    }

    pub fn area_for(&self, viewport: Rect) -> Rect {
        let visible = self.visible_indices();
        let count = visible.len().clamp(1, MAX_VISIBLE_ITEMS);
        // Each row is its label and its detail (#1527): sized from the
        // labels alone, every signature was cut at the border.
        let max_row = visible
            .iter()
            .map(|&i| {
                let item = &self.items[i];
                let detail = item
                    .detail
                    .as_deref()
                    .map_or(0, |d| cells(d).saturating_add(2));
                cells(&item.label).saturating_add(detail)
            })
            .max()
            .unwrap_or(MIN_WIDTH);
        let width = max_row.saturating_add(6).clamp(MIN_WIDTH, MAX_WIDTH);
        let height = (count as u16).saturating_add(2);
        let (cx, cy) = self.anchor;
        let mut x = cx;
        let mut y = cy.saturating_add(1);
        if x.saturating_add(width) > viewport.right() {
            x = viewport.right().saturating_sub(width);
        }
        // Floor at the pane's own edges (#42): a popup wider than the pane
        // was pushed left past `viewport.x` and painted over the Explorer —
        // the trailing width clamp cannot repair an x already left of the
        // pane. Same floors the hover and signature-help popups carry.
        if x < viewport.x {
            x = viewport.x;
        }
        if y.saturating_add(height) > viewport.bottom() {
            y = cy.saturating_sub(height);
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

impl CompletionPopup {
    /// What the details pane says about the selected item (#1527): its
    /// full detail (the signature a row may have cut), then its
    /// documentation. Empty when the item carries neither.
    pub fn details_lines(&self) -> Vec<String> {
        let Some(item) = self.selected_item() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(detail) = item.detail.as_deref().map(str::trim)
            && !detail.is_empty()
        {
            out.push(detail.to_string());
        }
        let doc = item
            .documentation
            .as_deref()
            .map(doc_lines)
            .unwrap_or_default();
        if !doc.is_empty() {
            if !out.is_empty() {
                out.push(String::new());
            }
            out.extend(doc);
        }
        out
    }

    /// Where the details pane goes beside the list drawn at `list`, as VS
    /// Code places it (#1527): to the right, else to the left, else below
    /// or above the list; `None` when the selected item has nothing to show
    /// or there is no room.
    pub fn details_area_for(&self, list: Rect, viewport: Rect) -> Option<Rect> {
        let lines = self.details_lines();
        if lines.is_empty() || list.width == 0 {
            return None;
        }
        let content = lines.iter().map(|l| cells(l)).max().unwrap_or(0);
        let want = content
            .saturating_add(2)
            .clamp(DETAILS_MIN_WIDTH, DETAILS_MAX_WIDTH);
        let rows_at = |width: u16| -> u16 {
            let inner = width.saturating_sub(2).max(1);
            let rows: u16 = lines.iter().map(|l| cells(l).div_ceil(inner).max(1)).sum();
            rows.clamp(1, DETAILS_MAX_LINES).saturating_add(2)
        };
        let right_room = viewport.right().saturating_sub(list.right());
        let left_room = list.x.saturating_sub(viewport.x);
        let side = if right_room >= DETAILS_MIN_WIDTH {
            Some((list.right(), want.min(right_room)))
        } else if left_room >= DETAILS_MIN_WIDTH {
            let w = want.min(left_room);
            Some((list.x - w, w))
        } else {
            None
        };
        if let Some((x, width)) = side {
            let height = rows_at(width);
            // Level with the list, growing away from the caret's line.
            return Some(if list.y > self.anchor.1 {
                Rect {
                    x,
                    y: list.y,
                    width,
                    height: height.min(viewport.bottom().saturating_sub(list.y)),
                }
            } else {
                let y = list.bottom().saturating_sub(height).max(viewport.y);
                Rect {
                    x,
                    y,
                    width,
                    height: list.bottom() - y,
                }
            });
        }
        // No room at either side: on the far side of the list from the
        // caret, so the line being typed stays in view.
        let width = want.min(viewport.width);
        let x = list
            .x
            .min(viewport.right().saturating_sub(width))
            .max(viewport.x);
        let height = rows_at(width);
        if list.y > self.anchor.1 {
            let below = viewport.bottom().saturating_sub(list.bottom());
            (below >= 3).then(|| Rect {
                x,
                y: list.bottom(),
                width,
                height: height.min(below),
            })
        } else {
            let above = list.y.saturating_sub(viewport.y);
            let h = height.min(above);
            (above >= 3).then(|| Rect {
                x,
                y: list.y - h,
                width,
                height: h,
            })
        }
    }

    /// The details pane, for rendering at [`Self::details_area_for`].
    pub fn details(&self) -> CompletionDetails<'_> {
        CompletionDetails { popup: self }
    }
}

/// The selected completion's details pane (#1527).
pub struct CompletionDetails<'a> {
    popup: &'a CompletionPopup,
}

impl Widget for CompletionDetails<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let theme = self.popup.theme;
        let mut lines: Vec<Line> = Vec::new();
        for (i, l) in self.popup.details_lines().into_iter().enumerate() {
            // The first line is the signature when the item has one.
            let style = if i == 0
                && self
                    .popup
                    .selected_item()
                    .is_some_and(|it| it.detail.is_some())
            {
                Style::default()
                    .fg(theme.ui(Color::Rgb(0xd0, 0xd6, 0xe0)))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.ui(Color::Rgb(0xb0, 0xb8, 0xc8)))
            };
            lines.push(Line::from(Span::styled(l, style)));
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
            .style(Style::default().bg(theme.ui(Color::Rgb(0x1e, 0x21, 0x2a))));
        Widget::render(Clear, area, buf);
        ratatui::widgets::Paragraph::new(lines)
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .render(area, buf);
        if theme.framed_overlays() {
            crate::widgets::surface::paint_overlay_frame(buf, area, theme);
        }
    }
}

impl Widget for &CompletionPopup {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let visible = self.visible_indices();
        // Inside the borders, after the kind glyph: what a row has for its
        // label and detail.
        let row_room = area.width.saturating_sub(2 + 3);
        let items: Vec<ListItem> = visible
            .iter()
            .map(|&i| {
                let item = &self.items[i];
                let kind_glyph = kind_glyph(item.kind);
                let mut spans = vec![
                    Span::styled(
                        format!(" {kind_glyph} "),
                        Style::default().fg(self.theme.ui(Color::Rgb(0x6c, 0x88, 0xb0))),
                    ),
                    Span::raw(item.label.clone()),
                ];
                // A detail too long for the row ends in `…` rather than at
                // the border (#1527); the details pane has it whole.
                let room = row_room.saturating_sub(cells(&item.label) + 2);
                if let Some(detail) = item.detail.as_ref().filter(|_| room >= 2) {
                    spans.push(Span::styled(
                        format!("  {}", fit(detail, room)),
                        Style::default()
                            .fg(self.theme.ui(Color::Rgb(0x70, 0x78, 0x88)))
                            .add_modifier(Modifier::DIM),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x21, 0x2a))));

        let sel_bg = if self.theme.gradient() {
            let (r, g, b) = crate::gradient::POPUP_SEL_BG;
            Color::Rgb(r, g, b)
        } else {
            self.theme.ui(Color::Rgb(0x09, 0x67, 0xb8))
        };
        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(sel_bg)
                .fg(self.theme.ui(Color::Rgb(0xff, 0xff, 0xff)))
                .add_modifier(Modifier::BOLD),
        );

        Widget::render(Clear, area, buf);
        let mut state = ListState::default();
        state.select(Some(self.selected));
        StatefulWidget::render(list, area, buf, &mut state);
        if self.theme.framed_overlays() {
            crate::widgets::surface::paint_overlay_frame(buf, area, self.theme);
        }
    }
}

/// Whether `item` survives the typed word `needle` (lower-cased): its
/// filter text starts with it, or does once the punctuation it opens with
/// is skipped. The typed word never holds that punctuation: vtsls's member
/// of a possibly-undefined object filters on `.?.name` (its edit turns the
/// `.` into `?.`), a quoted key on `.content-type`, and matching those on
/// the bare word hid every one of them (#1472). Only an item with its own
/// edit gets the skip: that edit is what accounts for the punctuation. A
/// user snippet has none, so `!html` still needs its `!` typed, as
/// `Snippets::matching` requires.
fn matches_typed(item: &CompletionItem, needle: &str) -> bool {
    let haystack = filter_haystack(item).to_ascii_lowercase();
    haystack.starts_with(needle)
        || (item.text_edit.is_some()
            && haystack
                .trim_start_matches(|c: char| !(c.is_alphanumeric() || c == '_'))
                .starts_with(needle))
}

fn filter_haystack(item: &CompletionItem) -> &str {
    item.filter_text
        .as_deref()
        .or(item.insert_text.as_deref())
        .unwrap_or(item.label.as_str())
}

fn kind_glyph(kind: Option<lsp_types::CompletionItemKind>) -> char {
    use lsp_types::CompletionItemKind as K;
    match kind {
        Some(K::FUNCTION) | Some(K::METHOD) => 'ƒ',
        Some(K::CONSTRUCTOR) => 'c',
        Some(K::VARIABLE) | Some(K::FIELD) | Some(K::PROPERTY) => 'v',
        Some(K::CLASS) | Some(K::STRUCT) => 'C',
        Some(K::INTERFACE) => 'I',
        Some(K::MODULE) => 'M',
        Some(K::ENUM) | Some(K::ENUM_MEMBER) => 'E',
        Some(K::KEYWORD) => 'K',
        Some(K::SNIPPET) => '✄',
        Some(K::FILE) => 'F',
        Some(K::FOLDER) => 'D',
        Some(K::CONSTANT) => 'k',
        Some(K::TYPE_PARAMETER) => 'T',
        _ => '·',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn item(label: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            detail: None,
            insert_text: None,
            filter_text: None,
            kind: None,
            is_snippet: false,
            ..Default::default()
        }
    }

    fn popup(items: Vec<CompletionItem>, prefix: &str) -> CompletionPopup {
        CompletionPopup::new(items, prefix.to_string(), (0, 0), PathBuf::new(), 1)
    }

    #[test]
    fn empty_prefix_shows_every_item() {
        let p = popup(vec![item("a"), item("b"), item("c")], "");
        assert_eq!(p.visible_indices(), vec![0, 1, 2]);
    }

    #[test]
    fn prefix_filters_to_matching_labels() {
        let p = popup(
            vec![item("False"), item("calc_n_gram"), item("candidate")],
            "calc",
        );
        assert_eq!(p.visible_indices(), vec![1]);
    }

    #[test]
    fn prefix_match_is_case_insensitive() {
        let p = popup(vec![item("Calc"), item("calc"), item("CALC")], "ca");
        assert_eq!(p.visible_indices(), vec![0, 1, 2]);
    }

    #[test]
    fn visible_is_empty_when_no_match() {
        let p = popup(vec![item("foo"), item("bar")], "z");
        assert!(p.visible_is_empty());
    }

    #[test]
    fn move_up_wraps_over_visible_only() {
        let mut p = popup(
            vec![item("aardvark"), item("calc_one"), item("calc_two")],
            "calc",
        );
        assert_eq!(p.selected, 0);
        p.move_up();
        assert_eq!(p.selected, 1);
    }

    #[test]
    fn move_down_wraps_over_visible_only() {
        let mut p = popup(
            vec![item("aardvark"), item("calc_one"), item("calc_two")],
            "calc",
        );
        p.move_down();
        assert_eq!(p.selected, 1);
        p.move_down();
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn selected_item_indexes_visible() {
        let p = popup(
            vec![item("aardvark"), item("calc_one"), item("calc_two")],
            "calc",
        );
        assert_eq!(
            p.selected_item().map(|i| i.label.as_str()),
            Some("calc_one")
        );
    }

    #[test]
    fn selected_label_returns_full_text() {
        let p = popup(vec![item("calc_n_gram")], "calc_");
        assert_eq!(p.selected_label(), Some(String::from("calc_n_gram")));
    }

    #[test]
    fn selected_label_prefers_insert_text_over_label() {
        let p = popup(
            vec![CompletionItem {
                label: "os.getcwd".into(),
                detail: None,
                insert_text: Some("getcwd".into()),
                filter_text: None,
                kind: None,
                is_snippet: false,
                ..Default::default()
            }],
            "",
        );
        assert_eq!(p.selected_label(), Some(String::from("getcwd")));
    }

    #[test]
    fn filter_uses_filter_text_when_present() {
        let p = popup(
            vec![CompletionItem {
                label: "displayed".into(),
                detail: None,
                insert_text: None,
                filter_text: Some("matchme".into()),
                kind: None,
                is_snippet: false,
                ..Default::default()
            }],
            "match",
        );
        assert_eq!(p.visible_indices(), vec![0]);
    }

    /// An item whose filter text starts with punctuation the typed word
    /// cannot hold: vtsls's member of a possibly-undefined object
    /// (`.?.name`, its edit turns the `.` into `?.`) or a quoted key
    /// (`.content-type`).
    fn punctuated(label: &str, filter: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            filter_text: Some(filter.to_string()),
            text_edit: Some(crate::widgets::editor::TextSpanEdit {
                start: (0, 0),
                end: (0, 1),
                new_text: label.to_string(),
                utf16: true,
            }),
            ..Default::default()
        }
    }

    /// Negative: an item without its own edit, like a user snippet `!html`,
    /// keeps exact filtering, so `h` does not show it.
    #[test]
    fn punctuation_is_not_skipped_for_an_item_without_an_edit() {
        let snippet = CompletionItem {
            label: "!html".to_string(),
            filter_text: Some("!html".to_string()),
            is_snippet: true,
            ..Default::default()
        };
        let p = popup(vec![snippet], "h");
        assert!(p.visible_is_empty());
        assert!(p.visible_indices().is_empty());
    }

    #[test]
    fn a_filter_text_that_starts_with_punctuation_matches_the_word_after_it() {
        let p = popup(
            vec![
                punctuated("email", ".?.email"),
                punctuated("name", ".?.name"),
            ],
            "n",
        );
        assert_eq!(p.visible_indices(), vec![1], "`n` matches `.?.name` only");
        assert!(!p.visible_is_empty());
    }

    #[test]
    fn a_quoted_key_survives_its_first_letter() {
        let p = popup(
            vec![
                punctuated("accept", ".accept"),
                punctuated("content-type", ".content-type"),
            ],
            "c",
        );
        assert_eq!(p.visible_indices(), vec![1]);
    }

    /// Negative: past the punctuation the word still has to match.
    #[test]
    fn a_punctuated_filter_text_still_hides_what_does_not_match() {
        let p = popup(
            vec![
                punctuated("email", ".?.email"),
                punctuated("name", ".?.name"),
            ],
            "x",
        );
        assert!(p.visible_is_empty());
        assert!(p.visible_indices().is_empty());
    }

    /// Negative: only the leading punctuation is skipped. `na` sits inside
    /// `.x_name` but not at the start of its word, so it does not match.
    #[test]
    fn only_the_leading_punctuation_is_skipped() {
        let p = popup(vec![punctuated("x_name", ".x_name")], "na");
        assert!(p.visible_is_empty());
    }

    #[test]
    fn set_prefix_resets_selected() {
        let mut p = popup(vec![item("alpha"), item("beta"), item("calc")], "");
        p.move_down();
        p.move_down();
        assert_eq!(p.selected, 2);
        p.set_prefix(String::from("c"));
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn area_fits_within_viewport_when_anchor_is_near_right_edge() {
        let p = popup(vec![item("aaaaaaaaaaaaaaaaaaaaaaaaa")], "");
        let viewport = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let area = p.area_for(viewport);
        assert!(area.right() <= viewport.right());
    }

    /// #42: a popup wider than the editor pane was pushed left past the
    /// pane's own left edge and painted over the Explorer — the right-edge
    /// clamp had no floor, and the trailing width clamp cannot repair an x
    /// already left of the pane. Same defect class the signature-help
    /// popup fixed in PR #40; the hover popup always carried the floor.
    #[test]
    fn a_popup_wider_than_the_pane_stays_inside_it() {
        // The issue's proven probe: a 32-column Explorer, a 40-column
        // editor pane, one 54-char label (width clamps to MAX_WIDTH 60,
        // wider than the pane).
        let mut p = popup(
            vec![item(
                "a_very_long_completion_label_that_overflows_the_pane_x",
            )],
            "",
        );
        p.anchor = (44, 6);
        let viewport = Rect {
            x: 32,
            y: 0,
            width: 40,
            height: 24,
        };
        let area = p.area_for(viewport);
        assert!(
            area.x >= viewport.x,
            "the popup must never escape the pane's left edge into the \
             Explorer; area {area:?} vs viewport {viewport:?}"
        );
        assert!(area.right() <= viewport.right());
        // Clamped to the pane, the width shrinks to what fits.
        assert_eq!(area.width, viewport.width);
    }

    /// The vertical twin: flipping above a top-of-pane anchor must not
    /// escape past the viewport's top into the tab strip.
    #[test]
    fn a_popup_flipped_above_a_top_anchor_stays_inside_the_pane() {
        // A pane SHORTER than the popup (a squeezed editor above a tall
        // bottom panel): the flip above the anchor lands past the top.
        let items: Vec<CompletionItem> = (0..10).map(|i| item(&format!("it{i}"))).collect();
        let mut p = popup(items, "");
        p.anchor = (40, 13);
        let viewport = Rect {
            x: 32,
            y: 4,
            width: 40,
            height: 10,
        };
        let area = p.area_for(viewport);
        assert!(
            area.y >= viewport.y,
            "the popup must never escape the pane's top edge; area {area:?}"
        );
        assert!(area.bottom() <= viewport.bottom());
    }

    #[test]
    fn render_does_not_panic() {
        let p = popup(vec![item("foo"), item("bar")], "");
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        (&p).render(area, &mut buf);
    }

    #[test]
    fn render_paints_opaque_popup_background_over_editor_text_so_buffer_does_not_bleed_through() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        // Sentinel: a character that cannot appear in any popup content
        // (border glyphs, kind glyphs, item labels 'alpha' / 'beta').
        // If it survives the popup render, the popup left an editor cell
        // un-cleared — that's the bug the user saw in their screenshot.
        let sentinel = '#';
        let editor_bg = Color::Rgb(0x1a, 0x1c, 0x22);
        let editor_fg = Color::Rgb(0xd0, 0xd0, 0xd0);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &mut buf[(x, y)];
                cell.set_char(sentinel);
                cell.set_style(Style::default().fg(editor_fg).bg(editor_bg));
            }
        }
        let p = popup(vec![item("alpha"), item("beta")], "");
        (&p).render(area, &mut buf);
        let inner = Rect {
            x: area.left() + 1,
            y: area.top() + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        for y in inner.top()..inner.bottom() {
            for x in inner.left()..inner.right() {
                let cell = &buf[(x, y)];
                let ch = cell.symbol().chars().next().unwrap_or(' ');
                assert_ne!(
                    ch, sentinel,
                    "popup inner cell ({x},{y}) still shows the editor's sentinel '{sentinel}'; Block.render only set_style's the area without clearing characters, so a Clear widget pass is needed before the List renders, otherwise editor glyphs bleed through past each suggestion label"
                );
            }
        }
    }

    /// A method as rust-analyzer sends it for `s.tri`: a signature and
    /// Markdown documentation with a code fence.
    fn documented(label: &str, detail: &str, doc: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            detail: Some(detail.to_string()),
            documentation: Some(doc.to_string()),
            ..Default::default()
        }
    }

    const TRIM_DOC: &str = "Returns a string slice with leading and trailing whitespace removed.\n\n\n# Examples\n\n```rust\nlet s = \"\\n Hello\\tworld\\t\\n\";\n```\n";

    fn screen(p: &CompletionPopup, vp: Rect) -> (Rect, Option<Rect>, String) {
        let list = p.area_for(vp);
        let mut buf = Buffer::empty(vp);
        Widget::render(p, list, &mut buf);
        let details = p.details_area_for(list, vp);
        if let Some(d) = details {
            p.details().render(d, &mut buf);
        }
        let mut text = String::new();
        for y in 0..vp.height {
            for x in 0..vp.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        (list, details, text)
    }

    const VIEW: Rect = Rect {
        x: 0,
        y: 0,
        width: 140,
        height: 30,
    };

    /// #1527: the list was sized from the labels alone, so every signature
    /// was cut at the border after about ten cells.
    #[test]
    fn rows_are_sized_for_their_signatures() {
        let p = popup(
            vec![
                documented("trim", "fn(&self) -> &str", TRIM_DOC),
                documented("trim_start", "fn(&self) -> &str", ""),
            ],
            "tri",
        );
        let list = p.area_for(VIEW);
        assert_eq!(
            list.width,
            "trim_start".len() as u16 + 2 + "fn(&self) -> &str".len() as u16 + 6
        );
        let (_, _, text) = screen(&p, VIEW);
        assert!(text.contains("trim_start  fn(&self) -> &str"), "{text}");
    }

    /// #1527: a signature too long even for the widest list ends in `…`
    /// instead of at the border.
    #[test]
    fn a_signature_wider_than_the_list_is_cut_with_an_ellipsis() {
        let long = format!("fn({}) -> bool", "a: usize, ".repeat(10));
        let p = popup(vec![documented("check", &long, "")], "");
        let list = p.area_for(VIEW);
        assert_eq!(list.width, MAX_WIDTH);
        let (_, _, text) = screen(&p, VIEW);
        let row = text.lines().nth(list.y as usize + 1).unwrap();
        assert!(row.contains("check  fn(a: usize"), "{row}");
        assert!(row.contains("…"), "{row}");
    }

    /// #1527: the selected item's full signature and its documentation show
    /// in a pane beside the list, with the Markdown fences dropped.
    #[test]
    fn the_selected_items_documentation_shows_beside_the_list() {
        let mut p = popup(
            vec![
                documented("trim", "fn(&self) -> &str", TRIM_DOC),
                documented(
                    "trim_ascii",
                    "const fn(&self) -> &str",
                    "Returns a string slice with ASCII whitespace removed.",
                ),
            ],
            "tri",
        );
        p.anchor = (10, 4);
        let (list, details, text) = screen(&p, VIEW);
        let details = details.expect("a details pane");
        assert_eq!(details.x, list.right(), "beside the list, to its right");
        assert_eq!(details.y, list.y);
        // The pane wraps at its width, so look for words on one line.
        assert!(text.contains("leading and trailing"), "{text}");
        assert!(text.contains("# Examples"), "{text}");
        assert!(!text.contains("```"), "{text}");
        p.move_down();
        let (_, _, text) = screen(&p, VIEW);
        assert!(text.contains("ASCII whitespace removed"), "{text}");
        assert!(!text.contains("leading and trailing"), "{text}");
    }

    /// #1527: with no room at the right, the pane goes to the left of the
    /// list; with none at either side, under it, clear of the caret.
    #[test]
    fn the_details_pane_finds_room_away_from_the_caret() {
        let mut p = popup(vec![documented("trim", "fn(&self) -> &str", TRIM_DOC)], "");
        p.anchor = (110, 4);
        let (list, details, _) = screen(&p, VIEW);
        let details = details.unwrap();
        assert_eq!(
            details.right(),
            list.x,
            "to the left of a list at the right edge"
        );

        let narrow = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 30,
        };
        p.anchor = (2, 4);
        let (list, details, _) = screen(&p, narrow);
        let details = details.unwrap();
        assert_eq!(details.y, list.bottom(), "under the list");
        assert!(details.y > 4, "the caret's line stays in view");

        // A list drawn above the caret puts the pane above it too.
        p.anchor = (2, 28);
        let (list, details, _) = screen(&p, narrow);
        let details = details.unwrap();
        assert_eq!(details.bottom(), list.y);
    }

    /// #1527 negative: an item with no detail and no documentation gets no
    /// pane, and a plain label list keeps its old width.
    #[test]
    fn an_undocumented_item_gets_no_details_pane() {
        let p = popup(vec![item("alpha"), item("beta")], "");
        let list = p.area_for(VIEW);
        assert_eq!(list.width, MIN_WIDTH);
        assert_eq!(p.details_area_for(list, VIEW), None);
        assert!(p.details_lines().is_empty());
    }

    /// #1527: documentation lines drop fences and fold blank runs.
    #[test]
    fn documentation_is_read_without_its_fences() {
        assert_eq!(
            doc_lines("A.\n\n\n\n```py\nx = 1\n```\n\n"),
            vec!["A.", "", "x = 1"]
        );
        assert!(doc_lines("").is_empty());
    }
}
