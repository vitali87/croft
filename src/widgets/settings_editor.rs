//! The Settings editor modal (#612): every setting with its value and the
//! layer that set it, searchable, edited in place and written to the user
//! or the workspace layer.

use crate::config_layers::LayerKind;
use crate::settings_editor::Row;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Widget};

#[derive(Debug)]
pub struct SettingsEditor {
    pub rows: Vec<Row>,
    pub query: String,
    /// Index into [`Self::visible`].
    pub selected: usize,
    pub scroll: usize,
    /// The layer edits are written to: `User` or `Workspace`.
    pub target: LayerKind,
    pub last_rect: Rect,
}

impl SettingsEditor {
    pub fn new(rows: Vec<Row>) -> Self {
        Self {
            rows,
            query: String::new(),
            selected: 0,
            scroll: 0,
            target: LayerKind::User,
            last_rect: Rect::default(),
        }
    }

    /// Indices of the rows matching the query, in order.
    pub fn visible(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&i| crate::settings_editor::matches(&self.rows[i], &self.query))
            .collect()
    }

    /// The selected row, if any.
    pub fn selected_row(&self) -> Option<&Row> {
        let i = *self.visible().get(self.selected)?;
        self.rows.get(i)
    }

    /// Move the selection by `delta`, clamped.
    pub fn move_selection(&mut self, delta: isize) {
        let len = self.visible().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.saturating_add_signed(delta).min(len - 1);
    }

    /// Replace the query; the selection returns to the top.
    pub fn set_query(&mut self, q: String) {
        self.query = q;
        self.selected = 0;
        self.scroll = 0;
    }
}

/// Paint the editor centred in `area`.
pub fn render(ed: &mut SettingsEditor, area: Rect, buf: &mut Buffer, theme: crate::theme::Theme) {
    let width = (area.width.saturating_mul(8) / 10).clamp(40.min(area.width), 120.min(area.width));
    let height = (area.height.saturating_mul(8) / 10).clamp(10.min(area.height), area.height);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 4,
        width,
        height,
    };
    ed.last_rect = rect;
    Widget::render(Clear, rect, buf);
    let bg = theme.ui(Color::Rgb(0x16, 0x18, 0x1f));
    let text = Style::default()
        .fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        .bg(bg);
    let dim = Style::default()
        .fg(theme.ui(Color::Rgb(0x8a, 0x93, 0xa6)))
        .bg(bg);
    let sel = text.bg(theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e)));
    Widget::render(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
            .title(" Settings ")
            .style(Style::default().bg(bg)),
        rect,
        buf,
    );
    let inner = Rect {
        x: rect.x + 2,
        y: rect.y + 1,
        width: rect.width.saturating_sub(4),
        height: rect.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height < 4 {
        return;
    }
    let w = inner.width as usize;
    buf.set_stringn(inner.x, inner.y, format!("> {}", ed.query), w, text);
    let target = if ed.target == LayerKind::Workspace {
        "workspace (.croft/config.json)"
    } else {
        "user (config.json)"
    };
    buf.set_stringn(
        inner.x,
        inner.y + 1,
        format!("Writing to {target} \u{b7} Tab switch \u{b7} Enter edit \u{b7} Esc close"),
        w,
        dim.add_modifier(Modifier::ITALIC),
    );
    let top = inner.y + 3;
    let rows_h = (inner.y + inner.height).saturating_sub(top) as usize;
    let visible = ed.visible();
    if ed.selected < ed.scroll {
        ed.scroll = ed.selected;
    } else if rows_h > 0 && ed.selected >= ed.scroll + rows_h {
        ed.scroll = ed.selected + 1 - rows_h;
    }
    let key_w = (w / 3).max(16).min(w);
    let layer_w = 16usize.min(w);
    let value_w = w.saturating_sub(key_w + layer_w + 2);
    for (n, &i) in visible.iter().enumerate().skip(ed.scroll).take(rows_h) {
        let row = &ed.rows[i];
        let style = if n == ed.selected { sel } else { text };
        let mut value = crate::settings_editor::display(&row.value);
        if value.chars().count() > value_w {
            value = value
                .chars()
                .take(value_w.saturating_sub(1))
                .collect::<String>()
                + "\u{2026}";
        }
        let line = format!(
            "{:<key_w$} {:<value_w$} {}",
            row.key.replace('_', " "),
            value,
            row.layer.label()
        );
        buf.set_stringn(
            inner.x,
            top + (n - ed.scroll) as u16,
            format!("{line:<w$}"),
            w,
            style,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> SettingsEditor {
        let prefs = crate::prefs::Prefs {
            auto_save: true,
            ..Default::default()
        };
        let prov =
            std::collections::BTreeMap::from([(String::from("auto_save"), LayerKind::Workspace)]);
        SettingsEditor::new(crate::settings_editor::rows(&prefs, &prov))
    }

    #[test]
    fn the_query_filters_and_the_selection_stays_inside_it() {
        let mut ed = editor();
        assert_eq!(ed.visible().len(), ed.rows.len());
        ed.set_query(String::from("auto save"));
        assert_eq!(ed.selected_row().map(|r| r.key.as_str()), Some("auto_save"));
        ed.move_selection(100);
        assert_eq!(ed.selected, ed.visible().len() - 1);
        ed.move_selection(-100);
        assert_eq!(ed.selected, 0);
        ed.set_query(String::from("no such setting at all"));
        assert!(ed.selected_row().is_none());
    }

    #[test]
    fn a_row_shows_its_key_value_and_layer_and_the_header_names_the_target() {
        let mut ed = editor();
        ed.set_query(String::from("auto_save"));
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 30));
        render(
            &mut ed,
            Rect::new(0, 0, 120, 30),
            &mut buf,
            crate::theme::Theme::default(),
        );
        let text: String = (0..30)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(text.contains("Settings"), "{text}");
        assert!(text.contains("auto save"), "{text}");
        assert!(text.contains("true"), "{text}");
        assert!(text.contains("workspace"), "the layer that set it: {text}");
        assert!(text.contains("Writing to user"), "{text}");
    }
}
