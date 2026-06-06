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

pub struct CompletionPopup {
    pub items: Vec<CompletionItem>,
    pub prefix: String,
    pub selected: usize,
    pub anchor: (u16, u16),
    pub path: PathBuf,
    pub request_id: u64,
    /// Black theme: gradient border + muted dark-teal selection instead of the
    /// legacy bright-blue. Set by the app before render from `popup_gradient`.
    pub gradient: bool,
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
            gradient: false,
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
        self.items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                if filter_haystack(item)
                    .to_ascii_lowercase()
                    .starts_with(&needle)
                {
                    Some(i)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn visible_is_empty(&self) -> bool {
        if self.prefix.is_empty() {
            return self.items.is_empty();
        }
        let needle = self.prefix.to_ascii_lowercase();
        !self.items.iter().any(|item| {
            filter_haystack(item)
                .to_ascii_lowercase()
                .starts_with(&needle)
        })
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
        self.selected_item().map(|item| {
            item.insert_text
                .clone()
                .unwrap_or_else(|| item.label.clone())
        })
    }

    pub fn area_for(&self, viewport: Rect) -> Rect {
        let visible = self.visible_indices();
        let count = visible.len().clamp(1, MAX_VISIBLE_ITEMS);
        let max_label = visible
            .iter()
            .map(|&i| self.items[i].label.chars().count() as u16)
            .max()
            .unwrap_or(MIN_WIDTH);
        let width = max_label.saturating_add(6).clamp(MIN_WIDTH, MAX_WIDTH);
        let height = (count as u16).saturating_add(2);
        let (cx, cy) = self.anchor;
        let mut x = cx;
        let mut y = cy.saturating_add(1);
        if x.saturating_add(width) > viewport.right() {
            x = viewport.right().saturating_sub(width);
        }
        if y.saturating_add(height) > viewport.bottom() {
            y = cy.saturating_sub(height);
        }
        Rect {
            x,
            y,
            width: width.min(viewport.width.saturating_sub(x.saturating_sub(viewport.x))),
            height: height.min(viewport.height.saturating_sub(y.saturating_sub(viewport.y))),
        }
    }
}

impl Widget for &CompletionPopup {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let visible = self.visible_indices();
        let items: Vec<ListItem> = visible
            .iter()
            .map(|&i| {
                let item = &self.items[i];
                let kind_glyph = kind_glyph(item.kind);
                let mut spans = vec![
                    Span::styled(
                        format!(" {kind_glyph} "),
                        Style::default().fg(Color::Rgb(0x6c, 0x88, 0xb0)),
                    ),
                    Span::raw(item.label.clone()),
                ];
                if let Some(detail) = item.detail.as_ref() {
                    spans.push(Span::styled(
                        format!("  {detail}"),
                        Style::default()
                            .fg(Color::Rgb(0x70, 0x78, 0x88))
                            .add_modifier(Modifier::DIM),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x21, 0x2a)));

        let sel_bg = if self.gradient {
            let (r, g, b) = crate::gradient::POPUP_SEL_BG;
            Color::Rgb(r, g, b)
        } else {
            Color::Rgb(0x09, 0x67, 0xb8)
        };
        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(sel_bg)
                .fg(Color::Rgb(0xff, 0xff, 0xff))
                .add_modifier(Modifier::BOLD),
        );

        Widget::render(Clear, area, buf);
        let mut state = ListState::default();
        state.select(Some(self.selected));
        StatefulWidget::render(list, area, buf, &mut state);
        if self.gradient {
            crate::gradient::paint_gradient_box(buf, area);
        }
    }
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
            }],
            "match",
        );
        assert_eq!(p.visible_indices(), vec![0]);
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
}
