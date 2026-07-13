//! A generic single-choice list picker for Source Control operations that
//! act on one item out of a list git already owns: apply/pop/drop a stash,
//! delete a tag, remove a remote, push to a remote. The user filters by
//! typing, navigates with the arrows, and Enter selects.
//!
//! Distinct from [`crate::widgets::branch_picker`], which adds a
//! "create new branch" affordance. This picker only ever selects an
//! existing row, so it stays a thin list. The App tags it with a
//! [`ListPurpose`] and reads back the chosen row's `id`.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

/// Why the picker is open, so the App dispatches the right git op on Enter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListPurpose {
    StashApply,
    StashPop,
    StashDrop,
    RemoveRemote,
    DeleteTag,
    PushToRemote,
    /// LSP Quick Fix: the rows are the code actions at the cursor; the App
    /// reads back the chosen row's `id` as an index into its pending actions.
    CodeAction,
    /// Merge-conflict resolution: the rows are Accept Current / Incoming /
    /// Both for the conflict block at the cursor; `id` names the resolution.
    MergeConflict,
    /// Auto-detected project tasks (Tasks: Run Task): the rows are the
    /// commands discovered from the workspace's manifests; `id` is an
    /// index into the App's discovered-task list.
    RunTask,
    /// The searchable Settings hub (Preferences: Open Settings). Rows toggle a
    /// boolean setting (`id` = `toggle:<field>`) or run a follow-up command
    /// (`id` = `cmd:<command_id>`, e.g. open a JSON file or the theme picker).
    Settings,
    /// Multiplayer session roster (Session: Participants): the rows are the
    /// clients attached to this session's host; `id` is the participant id.
    /// Enter opens the per-participant action picker.
    SessionParticipant,
    /// Action on one participant: `id` is `grant:<id>` / `revoke:<id>` /
    /// `kick:<id>`, applied through the session host's control channel.
    SessionParticipantAction,
}

/// One selectable row: a stable `id` the App acts on (a stash index, a
/// remote/tag name) and the `label` shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListRow {
    pub id: String,
    pub label: String,
}

pub struct ListPicker {
    pub purpose: ListPurpose,
    pub title: String,
    pub rows: Vec<ListRow>,
    pub query: String,
    pub cursor: usize,
    pub selected: usize,
    pub scroll: usize,
    pub last_rect: Rect,
}

impl ListPicker {
    pub fn new(purpose: ListPurpose, title: impl Into<String>, rows: Vec<ListRow>) -> Self {
        Self {
            purpose,
            title: title.into(),
            rows,
            query: String::new(),
            cursor: 0,
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
        }
    }

    fn char_count(&self) -> usize {
        self.query.chars().count()
    }

    fn byte_offset(&self, char_idx: usize) -> usize {
        self.query
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.query.len())
    }

    /// Indices of rows whose label matches the query (case-insensitive
    /// substring); an empty query matches everything.
    fn matches(&self) -> Vec<usize> {
        let needle = self.query.trim().to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| needle.is_empty() || r.label.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn visible_count(&self) -> usize {
        self.matches().len()
    }

    /// The row Enter would act on, if any.
    pub fn selected_row(&self) -> Option<&ListRow> {
        self.matches().get(self.selected).map(|i| &self.rows[*i])
    }

    pub fn push_char(&mut self, c: char) {
        let at = self.byte_offset(self.cursor);
        self.query.insert(at, c);
        self.cursor += 1;
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn pop_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_offset(self.cursor - 1);
        self.query.remove(at);
        self.cursor -= 1;
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
    }

    pub fn select_next(&mut self) {
        let count = self.visible_count();
        if count > 0 && self.selected + 1 < count {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// The matched-row index at screen row `y`, if `y` lands on a visible row.
    /// The list body starts two rows below `last_rect.y` (the top border, then
    /// the prompt row), so the renderer and this stay in lock-step. Used to map
    /// a mouse click to a selectable row.
    pub fn row_index_at(&self, y: u16) -> Option<usize> {
        let list_top = self.last_rect.y.saturating_add(2);
        if y < list_top {
            return None;
        }
        let idx = self.scroll + (y - list_top) as usize;
        (idx < self.visible_count()).then_some(idx)
    }
}

pub fn render_list_picker(picker: &mut ListPicker, screen: Rect, buf: &mut Buffer, gradient: bool) {
    let width = (screen.width.saturating_mul(6) / 10).clamp(36, 96.min(screen.width));
    let height = (screen.height.saturating_mul(6) / 10).clamp(8, screen.height);
    let rect = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y + (screen.height.saturating_sub(height)) / 4,
        width,
        height,
    };
    picker.last_rect = rect;
    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        format!(
            " {} — Esc to close, ↑/↓ navigate, Enter select ",
            picker.title
        ),
        Style::default()
            .fg(Color::Rgb(0xff, 0xff, 0xff))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
        .title(title.clone())
        .style(Style::default().bg(Color::Rgb(0x16, 0x18, 0x1f)));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    if gradient {
        crate::gradient::paint_gradient_box(buf, rect);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    let sel_bg = if gradient {
        let (r, g, b) = crate::gradient::POPUP_SEL_BG;
        Color::Rgb(r, g, b)
    } else {
        Color::Rgb(0x1e, 0x3a, 0x6e)
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Prompt row.
    let cursor = picker.cursor.min(picker.query.chars().count());
    let before: String = picker.query.chars().take(cursor).collect();
    let at: String = picker.query.chars().skip(cursor).take(1).collect();
    let after: String = picker.query.chars().skip(cursor + 1).collect();
    let caret = if at.is_empty() { " ".to_string() } else { at };
    let qstyle = Style::default().fg(Color::Rgb(0xec, 0xef, 0xf4));
    let caret_style = Style::default()
        .fg(Color::Rgb(0x16, 0x18, 0x1f))
        .bg(Color::Rgb(0xec, 0xef, 0xf4))
        .add_modifier(Modifier::SLOW_BLINK);
    Widget::render(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0))),
            Span::styled(before, qstyle),
            Span::styled(caret, caret_style),
            Span::styled(after, qstyle),
        ])),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
        buf,
    );

    let list = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    if list.height == 0 {
        return;
    }
    let matches = picker.matches();
    if matches.is_empty() {
        Widget::render(
            Paragraph::new(Line::from(Span::styled(
                "  (nothing to choose)",
                Style::default().fg(Color::Rgb(0x7a, 0x82, 0x90)),
            ))),
            list,
            buf,
        );
        return;
    }
    let visible = list.height as usize;
    if picker.selected >= picker.scroll + visible {
        picker.scroll = picker.selected + 1 - visible;
    }
    if picker.selected < picker.scroll {
        picker.scroll = picker.selected;
    }
    let end = (picker.scroll + visible).min(matches.len());
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - picker.scroll);
    for (offset, m) in matches[picker.scroll..end].iter().enumerate() {
        let row_idx = picker.scroll + offset;
        let is_sel = row_idx == picker.selected;
        let style = if is_sel {
            Style::default().bg(sel_bg).fg(Color::White)
        } else {
            Style::default().fg(Color::Rgb(0xec, 0xef, 0xf4))
        };
        let prefix = if is_sel { "> " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), style),
            Span::styled(picker.rows[*m].label.clone(), style),
        ]));
    }
    Widget::render(Paragraph::new(lines), list, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<ListRow> {
        vec![
            ListRow {
                id: "0".into(),
                label: "stash@{0}: WIP on main".into(),
            },
            ListRow {
                id: "1".into(),
                label: "stash@{1}: WIP on feature".into(),
            },
        ]
    }

    #[test]
    fn selected_row_tracks_navigation() {
        let mut p = ListPicker::new(ListPurpose::StashApply, "Apply Stash", rows());
        assert_eq!(p.selected_row().map(|r| r.id.as_str()), Some("0"));
        p.select_next();
        assert_eq!(p.selected_row().map(|r| r.id.as_str()), Some("1"));
        p.select_prev();
        assert_eq!(p.selected_row().map(|r| r.id.as_str()), Some("0"));
    }

    #[test]
    fn typing_filters_rows_and_resets_selection() {
        let mut p = ListPicker::new(ListPurpose::StashDrop, "Drop Stash", rows());
        p.select_next();
        for c in "feature".chars() {
            p.push_char(c);
        }
        assert_eq!(p.visible_count(), 1);
        assert_eq!(p.selected, 0, "a filter edit resets the highlight");
        assert_eq!(p.selected_row().map(|r| r.id.as_str()), Some("1"));
    }

    #[test]
    fn row_index_at_maps_click_y_to_the_row_under_the_pointer() {
        let mut p = ListPicker::new(ListPurpose::CodeAction, "Quick Fix", rows());
        // Simulate the render geometry: a box whose top border is at y=10, so the
        // prompt row is y=11 and the first list row is y=12.
        p.last_rect = Rect {
            x: 0,
            y: 10,
            width: 40,
            height: 8,
        };
        assert_eq!(p.row_index_at(12), Some(0), "first list row");
        assert_eq!(p.row_index_at(13), Some(1), "second list row");
        assert_eq!(p.row_index_at(11), None, "the prompt row is not selectable");
        assert_eq!(p.row_index_at(14), None, "below the last row is nothing");
    }

    #[test]
    fn navigation_is_bounded() {
        let mut p = ListPicker::new(ListPurpose::DeleteTag, "Delete Tag", rows());
        for _ in 0..5 {
            p.select_next();
        }
        assert_eq!(p.selected, 1);
        for _ in 0..5 {
            p.select_prev();
        }
        assert_eq!(p.selected, 0);
    }
}
