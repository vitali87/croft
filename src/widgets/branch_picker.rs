//! The Source Control branch picker: a VS Code-style "Checkout to…" quick
//! pick. The user types to filter the repo's branches, arrow-keys to move,
//! and Enter checks out the highlighted branch — or, when the typed name
//! matches no existing branch, creates it. Opened by clicking the branch
//! name in the SOURCE CONTROL header or the commit dropdown's
//! "Checkout / Create Branch…" item.
//!
//! Like [`crate::widgets::zoxide_jump`], this widget is deliberately pure:
//! it owns the query text and selection but never shells out to git. The
//! app layer runs `crate::git::list_branches` once at open and feeds the
//! list in, so every behaviour here is unit-testable without a git repo
//! and the one subprocess call stays off the render path.

use crate::git::BranchInfo;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

/// What pressing Enter on the highlighted row resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchAction {
    /// Switch to an existing branch — the string is the `checkout_name`
    /// (short name for remotes, so git creates the tracking branch).
    Checkout(String),
    /// Create a new branch off HEAD with this name and switch to it.
    Create(String),
}

/// One visible row: an existing branch (index into `branches`) or the
/// synthetic "create this branch" affordance carrying the typed name.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Existing(usize),
    Create(String),
}

pub struct BranchPicker {
    pub query: String,
    pub cursor: usize,
    pub branches: Vec<BranchInfo>,
    pub selected: usize,
    pub scroll: usize,
    pub last_rect: Rect,
    pub last_inner_height: u16,
}

impl BranchPicker {
    pub fn new(branches: Vec<BranchInfo>) -> Self {
        Self {
            query: String::new(),
            cursor: 0,
            branches,
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
            last_inner_height: 0,
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

    /// Indices of branches whose display name contains the query
    /// (case-insensitive). An empty query matches everything, preserving
    /// the most-recently-committed ordering `list_branches` supplies.
    fn matches(&self) -> Vec<usize> {
        let needle = self.query.trim().to_lowercase();
        self.branches
            .iter()
            .enumerate()
            .filter(|(_, b)| needle.is_empty() || b.display.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    /// The typed name to offer as a new branch, when it is non-empty and
    /// no existing branch already carries that exact display name. This is
    /// what makes the single picker serve both "checkout" and "create".
    fn create_name(&self) -> Option<String> {
        let name = self.query.trim();
        if name.is_empty() {
            return None;
        }
        let exists = self.branches.iter().any(|b| {
            b.display.eq_ignore_ascii_case(name) || b.checkout_name.eq_ignore_ascii_case(name)
        });
        (!exists).then(|| name.to_string())
    }

    /// The flat, ordered list of selectable rows: matching branches first,
    /// then the optional create row last (mirroring VS Code, where "Create
    /// new branch" sits at the bottom of the quick pick).
    fn rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self.matches().into_iter().map(Row::Existing).collect();
        if let Some(name) = self.create_name() {
            rows.push(Row::Create(name));
        }
        rows
    }

    pub fn row_count(&self) -> usize {
        self.rows().len()
    }

    /// Resolve the highlighted row to the git action Enter should perform,
    /// or `None` when there is nothing to act on (e.g. an empty repo with
    /// an empty query).
    pub fn selected_action(&self) -> Option<BranchAction> {
        match self.rows().get(self.selected)? {
            Row::Existing(i) => self
                .branches
                .get(*i)
                .map(|b| BranchAction::Checkout(b.checkout_name.clone())),
            Row::Create(name) => Some(BranchAction::Create(name.clone())),
        }
    }

    // --- query editing (mirrors ZoxideJump) -------------------------------

    pub fn push_char(&mut self, c: char) {
        let at = self.byte_offset(self.cursor);
        self.query.insert(at, c);
        self.cursor += 1;
        self.reset_selection();
    }

    pub fn pop_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_offset(self.cursor - 1);
        self.query.remove(at);
        self.cursor -= 1;
        self.reset_selection();
    }

    pub fn delete_char(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.query.remove(at);
        self.reset_selection();
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor = self.char_count();
    }

    /// A query edit reshapes the row list, so collapse the highlight back
    /// to the top match rather than risk pointing past the new end.
    fn reset_selection(&mut self) {
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn select_next(&mut self) {
        let count = self.row_count();
        if count == 0 {
            return;
        }
        if self.selected + 1 < count {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }
}

/// Draw the picker into `rect`. Placement is decided by the caller in
/// `App::render_branch_picker`; this widget just fills the box it is handed.
pub fn render_branch_picker(
    picker: &mut BranchPicker,
    rect: Rect,
    buf: &mut Buffer,
    gradient: bool,
) {
    picker.last_rect = rect;

    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        " Checkout / Create Branch — Esc to close, ↑/↓ navigate, Enter select ",
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

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Prompt row with a blinking caret, identical treatment to the jumper.
    let query_style = Style::default()
        .fg(Color::Rgb(0xec, 0xef, 0xf4))
        .add_modifier(Modifier::BOLD);
    let caret_style = Style::default()
        .fg(Color::Rgb(0x16, 0x18, 0x1f))
        .bg(Color::Rgb(0xec, 0xef, 0xf4))
        .add_modifier(Modifier::SLOW_BLINK);
    let cursor = picker.cursor.min(picker.query.chars().count());
    let before: String = picker.query.chars().take(cursor).collect();
    let at: String = picker.query.chars().skip(cursor).take(1).collect();
    let after: String = picker.query.chars().skip(cursor + 1).collect();
    let caret_glyph = if at.is_empty() { String::from(" ") } else { at };
    let prompt_line = Line::from(vec![
        Span::styled(
            "\u{ea68} ",
            Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0)),
        ),
        Span::styled(before, query_style),
        Span::styled(caret_glyph, caret_style),
        Span::styled(after, query_style),
    ]);
    Widget::render(
        Paragraph::new(prompt_line),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
        buf,
    );

    let sep_line = Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(Color::Rgb(0x3b, 0x42, 0x52)),
    ));
    Widget::render(
        Paragraph::new(sep_line),
        Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: 1,
        },
        buf,
    );

    let list_rect = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };
    picker.last_inner_height = list_rect.height;
    if list_rect.height == 0 {
        return;
    }

    let rows = picker.rows();
    if rows.is_empty() {
        Widget::render(
            Paragraph::new(Line::from(Span::styled(
                "  No branches",
                Style::default().fg(Color::Rgb(0x7a, 0x82, 0x90)),
            ))),
            list_rect,
            buf,
        );
        return;
    }

    let visible = list_rect.height as usize;
    if picker.selected >= picker.scroll + visible {
        picker.scroll = picker.selected + 1 - visible;
    }
    if picker.selected < picker.scroll {
        picker.scroll = picker.selected;
    }
    let end = (picker.scroll + visible).min(rows.len());

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - picker.scroll);
    for (offset, row) in rows[picker.scroll..end].iter().enumerate() {
        let row_idx = picker.scroll + offset;
        let is_selected = row_idx == picker.selected;
        let base = if is_selected {
            Style::default().bg(sel_bg).fg(Color::White)
        } else {
            Style::default().fg(Color::Rgb(0xec, 0xef, 0xf4))
        };
        let prefix = if is_selected { "> " } else { "  " };
        let mut spans: Vec<Span<'static>> = vec![Span::styled(prefix.to_string(), base)];
        match row {
            Row::Existing(i) => {
                let b = &picker.branches[*i];
                let marker = if b.is_current { "● " } else { "  " };
                spans.push(Span::styled(
                    marker.to_string(),
                    if b.is_current {
                        Style::default().fg(Color::Rgb(0xa3, 0xbe, 0x8c))
                    } else {
                        base
                    },
                ));
                spans.push(Span::styled(b.display.clone(), base));
                if b.is_remote {
                    spans.push(Span::styled(
                        "  (remote)".to_string(),
                        Style::default().fg(Color::Rgb(0x7a, 0x82, 0x90)),
                    ));
                }
            }
            Row::Create(name) => {
                let accent = Style::default()
                    .fg(Color::Rgb(0x88, 0xc0, 0xd0))
                    .add_modifier(Modifier::BOLD);
                spans.push(Span::styled("\u{eadc} ".to_string(), accent));
                spans.push(Span::styled(
                    format!("Create new branch: {name}"),
                    if is_selected { base } else { accent },
                ));
            }
        }
        lines.push(Line::from(spans));
    }
    Widget::render(Paragraph::new(lines), list_rect, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(display: &str, current: bool, remote: bool) -> BranchInfo {
        let checkout_name = display.split_once('/').map(|(_, s)| s).unwrap_or(display);
        BranchInfo {
            display: display.to_string(),
            checkout_name: checkout_name.to_string(),
            is_current: current,
            is_remote: remote,
        }
    }

    fn sample() -> BranchPicker {
        BranchPicker::new(vec![
            branch("main", true, false),
            branch("feature/login", false, false),
            branch("origin/release", false, true),
        ])
    }

    #[test]
    fn empty_query_lists_every_branch_in_order() {
        let p = sample();
        assert_eq!(p.row_count(), 3);
        assert_eq!(
            p.selected_action(),
            Some(BranchAction::Checkout("main".to_string())),
            "first row is the current branch; Enter switches to it (harmless no-op in git)"
        );
    }

    #[test]
    fn typing_filters_to_matching_branches_case_insensitively() {
        let mut p = sample();
        for c in "LOG".chars() {
            p.push_char(c);
        }
        // Only feature/login matches "log"; no exact match, so a create row
        // is also offered.
        assert_eq!(p.row_count(), 2);
        assert_eq!(
            p.selected_action(),
            Some(BranchAction::Checkout("login".to_string())),
            "the matching branch is highlighted first"
        );
    }

    #[test]
    fn remote_branch_checks_out_by_its_short_name() {
        let mut p = sample();
        for c in "release".chars() {
            p.push_char(c);
        }
        assert_eq!(
            p.selected_action(),
            Some(BranchAction::Checkout("release".to_string())),
            "origin/release is offered as `git switch release` so git makes the tracking branch"
        );
    }

    #[test]
    fn a_novel_name_offers_a_create_row_and_no_phantom_checkout() {
        let mut p = sample();
        for c in "hotfix".chars() {
            p.push_char(c);
        }
        // No branch contains "hotfix", so the only row is the create row.
        assert_eq!(p.row_count(), 1);
        assert_eq!(
            p.selected_action(),
            Some(BranchAction::Create("hotfix".to_string()))
        );
    }

    #[test]
    fn an_exact_existing_name_does_not_offer_a_create_row() {
        let mut p = sample();
        for c in "main".chars() {
            p.push_char(c);
        }
        assert_eq!(
            p.row_count(),
            1,
            "main matches exactly, so only the checkout row shows — no create"
        );
        assert_eq!(
            p.selected_action(),
            Some(BranchAction::Checkout("main".to_string()))
        );
    }

    #[test]
    fn editing_the_query_resets_the_highlight_to_the_top() {
        let mut p = sample();
        p.select_next();
        p.select_next();
        assert_eq!(p.selected, 2);
        p.push_char('e');
        assert_eq!(
            p.selected, 0,
            "a query edit collapses the highlight to row 0"
        );
    }

    #[test]
    fn navigation_is_bounded_by_the_row_count() {
        let mut p = sample();
        for _ in 0..10 {
            p.select_next();
        }
        assert_eq!(p.selected, 2, "select_next must not overshoot the last row");
        for _ in 0..10 {
            p.select_prev();
        }
        assert_eq!(p.selected, 0, "select_prev must not underflow");
    }
}
