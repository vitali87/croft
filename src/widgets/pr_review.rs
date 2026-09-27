//! The PULL REQUEST tab (#365): a pull request's changed files with their
//! `+n −m` and a viewed checkbox, its checks, and a summary line. Read-only
//! review; comments are #366.

use crate::pr_review::{CheckState, PrInfo};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use std::collections::BTreeSet;

/// A selectable row: a changed file or a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    File(usize),
    Check(usize),
}

#[derive(Debug, Clone)]
pub struct PrReviewView {
    pub pr: PrInfo,
    /// `owner/repo#number`, the viewed-store key.
    pub key: String,
    pub viewed: BTreeSet<String>,
    pub selected: usize,
    pub scroll: usize,
    /// The whole PR's patch from `gh pr diff`, split per file on first use.
    pub diff: Option<std::collections::BTreeMap<String, String>>,
    /// Frame truth for mouse hits: first row's screen line and row count.
    pub rows_top: u16,
    pub rows_visible: u16,
}

impl PrReviewView {
    pub fn new(pr: PrInfo, key: String, viewed: BTreeSet<String>) -> PrReviewView {
        PrReviewView {
            pr,
            key,
            viewed,
            selected: 0,
            scroll: 0,
            diff: None,
            rows_top: 0,
            rows_visible: 0,
        }
    }

    /// Files first, then checks, in `gh`'s order.
    pub fn rows(&self) -> Vec<Row> {
        (0..self.pr.files.len())
            .map(Row::File)
            .chain((0..self.pr.checks.len()).map(Row::Check))
            .collect()
    }

    pub fn move_selection(&mut self, delta: isize) {
        let last = self.rows().len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.rows().get(self.selected).copied()
    }

    pub fn selected_file(&self) -> Option<&crate::pr_review::PrFile> {
        match self.selected_row()? {
            Row::File(i) => self.pr.files.get(i),
            Row::Check(_) => None,
        }
    }

    pub fn selected_check(&self) -> Option<&crate::pr_review::Check> {
        match self.selected_row()? {
            Row::Check(i) => self.pr.checks.get(i),
            Row::File(_) => None,
        }
    }
}

pub fn render(
    view: &mut PrReviewView,
    area: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
) {
    let base = Style::default().bg(bg);
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_style(base);
            buf[(x, y)].set_symbol(" ");
        }
    }
    view.rows_top = 0;
    view.rows_visible = 0;
    if area.height < 6 || area.width < 30 {
        return;
    }
    let w = area.width as usize;
    let text = Style::default().fg(theme.ui(Color::Gray)).bg(bg);
    let dim = Style::default().fg(Color::DarkGray).bg(bg);
    let pr = &view.pr;
    let header = format!(
        " PR #{} · {} · {} · {} → {} ",
        pr.number, pr.title, pr.author, pr.head_ref, pr.base_ref
    );
    buf.set_stringn(
        area.x,
        area.y,
        &header,
        w,
        Style::default()
            .fg(Color::White)
            .bg(theme.ui(Color::Rgb(0x09, 0x4d, 0x77)))
            .add_modifier(Modifier::BOLD),
    );
    buf.set_stringn(
        area.x + 1,
        area.y + 1,
        crate::pr_review::summary(pr, &view.viewed),
        w.saturating_sub(1),
        text,
    );
    // Body: section labels are painted between rows but are not rows.
    let mut lines: Vec<(Option<usize>, String, Style)> = Vec::new();
    lines.push((
        None,
        String::from("FILES CHANGED"),
        text.add_modifier(Modifier::BOLD),
    ));
    for (i, f) in pr.files.iter().enumerate() {
        let mark = if view.viewed.contains(&f.path) {
            "[x]"
        } else {
            "[ ]"
        };
        let line = format!(
            " {mark} {:>6} {:>6}  {}",
            format!("+{}", f.additions),
            format!("\u{2212}{}", f.deletions),
            f.path
        );
        lines.push((Some(i), line, text));
    }
    lines.push((None, String::new(), text));
    lines.push((
        None,
        String::from("CHECKS"),
        text.add_modifier(Modifier::BOLD),
    ));
    for (i, c) in pr.checks.iter().enumerate() {
        let (glyph, color) = match c.state {
            CheckState::Pass => ("\u{2713}", Color::Rgb(0x5d, 0xbb, 0x85)),
            CheckState::Fail => ("\u{2717}", Color::Rgb(0xf1, 0x4c, 0x4c)),
            CheckState::Pending => ("\u{2026}", Color::Rgb(0xcc, 0xa7, 0x00)),
            CheckState::Neutral => ("-", Color::DarkGray),
        };
        lines.push((
            Some(pr.files.len() + i),
            format!(" {glyph} {}", c.name),
            Style::default().fg(color).bg(bg),
        ));
    }
    let top = area.y + 3;
    let visible = (area.height - 4) as usize;
    // Keep the selected row on screen.
    let sel_line = lines
        .iter()
        .position(|(r, _, _)| *r == Some(view.selected))
        .unwrap_or(0);
    if sel_line < view.scroll {
        view.scroll = sel_line;
    } else if sel_line >= view.scroll + visible {
        view.scroll = sel_line + 1 - visible;
    }
    view.rows_top = top;
    view.rows_visible = visible as u16;
    for (n, (row, line, style)) in lines.iter().enumerate().skip(view.scroll).take(visible) {
        let y = top + (n - view.scroll) as u16;
        let style = if *row == Some(view.selected) {
            Style::default()
                .fg(theme.accent_contrast_fg())
                .bg(theme.accent())
        } else {
            *style
        };
        buf.set_stringn(area.x, y, format!("{line:<w$}"), w, style);
    }
    let hint = " \u{2191}\u{2193} move \u{b7} Space viewed \u{b7} Enter diff (or a check's log) \u{b7} l failing log \u{b7} r refresh \u{b7} Esc leave review ";
    buf.set_stringn(
        area.x,
        area.y + area.height - 1,
        format!("{hint:<w$}"),
        w,
        dim,
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::pr_review::{Check, PrFile};

    pub(crate) fn sample() -> PrInfo {
        PrInfo {
            number: 579,
            title: "feat(sarif): model".into(),
            author: "vitali87".into(),
            url: "https://github.com/o/r/pull/579".into(),
            head_ref: "feat/x".into(),
            head_oid: "9f100db".into(),
            base_ref: "main".into(),
            files: vec![
                PrFile {
                    path: "src/sarif/view.rs".into(),
                    additions: 1048,
                    deletions: 0,
                    change: "ADDED".into(),
                },
                PrFile {
                    path: "Cargo.toml".into(),
                    additions: 1,
                    deletions: 1,
                    change: "MODIFIED".into(),
                },
            ],
            checks: vec![
                Check {
                    name: "clippy + tests".into(),
                    state: CheckState::Pass,
                    url: None,
                },
                Check {
                    name: "docs".into(),
                    state: CheckState::Fail,
                    url: Some("https://github.com/o/r/actions/runs/7/job/8".into()),
                },
            ],
        }
    }

    #[test]
    fn rows_list_files_then_checks_and_selection_clamps() {
        let mut v = PrReviewView::new(sample(), "o/r#579".into(), BTreeSet::new());
        assert_eq!(
            v.rows(),
            vec![Row::File(0), Row::File(1), Row::Check(0), Row::Check(1)]
        );
        assert_eq!(v.selected_file().unwrap().path, "src/sarif/view.rs");
        v.move_selection(1);
        assert_eq!(v.selected_file().unwrap().path, "Cargo.toml");
        v.move_selection(10);
        assert_eq!(v.selected_row(), Some(Row::Check(1)));
        assert!(v.selected_file().is_none());
        v.move_selection(-10);
        assert_eq!(v.selected_row(), Some(Row::File(0)));
    }
}
