use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget, Widget},
};

use crate::lsp::CompletionItem;

const MAX_VISIBLE_ITEMS: usize = 10;
const MIN_WIDTH: u16 = 24;
const MAX_WIDTH: u16 = 60;

pub struct CompletionPopup {
    pub items: Vec<CompletionItem>,
    pub selected: usize,
    pub anchor: (u16, u16),
    pub path: PathBuf,
    pub request_id: u64,
}

impl CompletionPopup {
    pub fn new(
        items: Vec<CompletionItem>,
        anchor: (u16, u16),
        path: PathBuf,
        request_id: u64,
    ) -> Self {
        Self {
            items,
            selected: 0,
            anchor,
            path,
            request_id,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn move_up(&mut self) {
        if self.items.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.items.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
    }

    pub fn selected_item(&self) -> Option<&CompletionItem> {
        self.items.get(self.selected)
    }

    pub fn insertion_text(&self) -> Option<String> {
        self.selected_item().map(|item| {
            item.insert_text
                .clone()
                .unwrap_or_else(|| item.label.clone())
        })
    }

    pub fn area_for(&self, viewport: Rect) -> Rect {
        let width = self
            .items
            .iter()
            .map(|i| i.label.chars().count() as u16)
            .max()
            .unwrap_or(MIN_WIDTH)
            .saturating_add(2)
            .clamp(MIN_WIDTH, MAX_WIDTH);
        let height = (self.items.len().min(MAX_VISIBLE_ITEMS) as u16).saturating_add(2);
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
        let items: Vec<ListItem> = self
            .items
            .iter()
            .map(|item| {
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

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(Color::Rgb(0x09, 0x67, 0xb8))
                .fg(Color::Rgb(0xff, 0xff, 0xff))
                .add_modifier(Modifier::BOLD),
        );

        let mut state = ListState::default();
        state.select(Some(self.selected));
        StatefulWidget::render(list, area, buf, &mut state);
    }
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
            kind: None,
        }
    }

    #[test]
    fn move_up_wraps_to_last() {
        let mut p = CompletionPopup::new(
            vec![item("a"), item("b"), item("c")],
            (0, 0),
            PathBuf::new(),
            1,
        );
        assert_eq!(p.selected, 0);
        p.move_up();
        assert_eq!(p.selected, 2);
    }

    #[test]
    fn move_down_wraps_to_first() {
        let mut p = CompletionPopup::new(vec![item("a"), item("b")], (0, 0), PathBuf::new(), 1);
        p.move_down();
        assert_eq!(p.selected, 1);
        p.move_down();
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn insertion_prefers_insert_text_over_label() {
        let p = CompletionPopup::new(
            vec![CompletionItem {
                label: "os.getcwd".into(),
                detail: None,
                insert_text: Some("getcwd".into()),
                kind: None,
            }],
            (0, 0),
            PathBuf::new(),
            1,
        );
        assert_eq!(p.insertion_text(), Some(String::from("getcwd")));
    }

    #[test]
    fn area_fits_within_viewport_when_anchor_is_near_right_edge() {
        let p = CompletionPopup::new(
            vec![item("aaaaaaaaaaaaaaaaaaaaaaaaa")],
            (100, 5),
            PathBuf::new(),
            1,
        );
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
        let p = CompletionPopup::new(vec![item("foo"), item("bar")], (0, 0), PathBuf::new(), 1);
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        (&p).render(area, &mut buf);
    }
}
