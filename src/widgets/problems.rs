//! The bottom panel's PROBLEMS tab: aggregated workspace diagnostics, grouped
//! by file exactly like VS Code's Problems view. Each file is a collapsible
//! header (file icon + name + relative dir + a count badge); under it sits one
//! row per diagnostic (severity glyph + message + source server). Clicking a
//! diagnostic jumps the editor to that line; clicking a header collapses the
//! group. The rows are a pure projection of the app's per-file diagnostics
//! store, refreshed whenever a language server republishes.

use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::lsp::manager::DiagnosticSeverity;
use crate::theme::Theme;
use crate::widgets::scrollbar;

const COLOR_HEADER: Color = Color::Rgb(0xE8, 0xEE, 0xF8);
const COLOR_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const COLOR_MSG: Color = Color::Rgb(0xCC, 0xCC, 0xCC);

/// Severity colours, matching the editor's diagnostic underline palette.
const COLOR_ERROR: Color = Color::Rgb(0xf1, 0x4c, 0x4c);
const COLOR_WARNING: Color = Color::Rgb(0xcc, 0xa7, 0x00);
const COLOR_INFO: Color = Color::Rgb(0x3b, 0x9e, 0xff);

/// Codicon severity glyphs, verified against the upstream codicon
/// `mapping.json` on 2026-06-28: `error` = 60039 (U+EA87), `warning` = 60012
/// (U+EA6C), `info` = 60020 (U+EA74). Nerd Fonts preserve codicon codepoints
/// (cross-checked against the activity-bar glyphs in `crate::icons`).
const GLYPH_ERROR: char = '\u{ea87}';
const GLYPH_WARNING: char = '\u{ea6c}';
const GLYPH_INFO: char = '\u{ea74}';

/// One diagnostic in the Problems list, projected from a language server's
/// published set. Positions are zero-based (LSP), shown one-based in the row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemItem {
    pub line: u32,
    pub col: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    /// The server that produced it (e.g. `rust-analyzer`, `ruff`), shown dim
    /// at the end of the row like VS Code's diagnostic source.
    pub source: String,
}

/// All diagnostics for one file, the unit the Problems view groups by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemGroup {
    pub path: PathBuf,
    /// File basename shown in the header.
    pub name: String,
    /// Directory of the file relative to the workspace root, shown dim after
    /// the name (empty for a file at the root).
    pub rel_dir: String,
    pub items: Vec<ProblemItem>,
}

/// A rendered line in the panel, used to map a click row back to an action.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RenderRow {
    Header(usize),
    Diag(usize, usize),
}

/// The action a click on a Problems row resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProblemHit {
    /// A file header: collapse/expand its group.
    Header(PathBuf),
    /// A diagnostic: jump the editor to `(path, line, col)` (zero-based).
    Diagnostic { path: PathBuf, line: u32, col: u32 },
}

pub struct ProblemsPanel {
    groups: Vec<ProblemGroup>,
    collapsed: HashSet<PathBuf>,
    scroll: usize,

    pub focus_gradient: bool,
    pub theme: Theme,
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,

    pub last_area: Rect,
    pub last_scrollbar: Rect,

    rows: Vec<RenderRow>,
    first_row_y: u16,
    visible_rows: u16,
    viewport_rows: u16,
}

impl ProblemsPanel {
    pub fn new() -> Self {
        Self {
            groups: Vec::new(),
            collapsed: HashSet::new(),
            scroll: 0,
            focus_gradient: false,
            theme: Theme::default(),
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            rows: Vec::new(),
            first_row_y: 0,
            visible_rows: 0,
            viewport_rows: 0,
        }
    }

    /// Replace the grouped diagnostics, returning whether anything changed so
    /// the caller only forces a redraw on a real update (never once per tick).
    /// Drops collapse state for files that no longer have diagnostics.
    pub fn set_groups(&mut self, groups: Vec<ProblemGroup>) -> bool {
        if self.groups == groups {
            return false;
        }
        self.collapsed
            .retain(|p| groups.iter().any(|g| &g.path == p));
        self.groups = groups;
        self.scroll = self.scroll.min(self.max_scroll());
        true
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Total diagnostics across all files, for the tab badge.
    pub fn total_count(&self) -> usize {
        self.groups.iter().map(|g| g.items.len()).sum()
    }

    pub fn error_count(&self) -> usize {
        self.severity_count(DiagnosticSeverity::Error)
    }

    pub fn warning_count(&self) -> usize {
        self.severity_count(DiagnosticSeverity::Warning)
    }

    fn severity_count(&self, want: DiagnosticSeverity) -> usize {
        self.groups
            .iter()
            .flat_map(|g| g.items.iter())
            .filter(|i| i.severity == want)
            .count()
    }

    pub fn toggle_collapse(&mut self, path: &PathBuf) {
        if !self.collapsed.remove(path) {
            self.collapsed.insert(path.clone());
        }
        // A collapse changes total row count, so a stale scroll could strand
        // the viewport past the new end.
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.max_scroll());
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn max_scroll(&self) -> usize {
        self.total_rows()
            .saturating_sub(self.viewport_rows as usize)
    }

    /// The number of laid-out rows given the current collapse state (one per
    /// header plus one per diagnostic of an expanded group).
    fn total_rows(&self) -> usize {
        self.groups
            .iter()
            .map(|g| {
                1 + if self.collapsed.contains(&g.path) {
                    0
                } else {
                    g.items.len()
                }
            })
            .sum()
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.total_rows(),
            self.viewport_rows as usize,
            self.scroll,
        ) else {
            return false;
        };
        let target = scrollbar::scroll_for_y(metrics, y);
        let moved = target != self.scroll;
        self.scroll = target;
        moved
    }

    /// Resolve a click at row `y` to a header (collapse) or diagnostic (jump).
    pub fn hit_at(&self, y: u16) -> Option<ProblemHit> {
        if self.visible_rows == 0 || y < self.first_row_y {
            return None;
        }
        let offset = (y - self.first_row_y) as usize;
        if offset >= self.visible_rows as usize {
            return None;
        }
        match self.rows.get(self.scroll + offset)? {
            RenderRow::Header(g) => {
                let group = self.groups.get(*g)?;
                Some(ProblemHit::Header(group.path.clone()))
            }
            RenderRow::Diag(g, i) => {
                let group = self.groups.get(*g)?;
                let item = group.items.get(*i)?;
                Some(ProblemHit::Diagnostic {
                    path: group.path.clone(),
                    line: item.line,
                    col: item.col,
                })
            }
        }
    }

    /// Rebuild the flat row list for the current collapse state. Called at the
    /// top of `render` so `rows` always matches what was painted.
    fn build_rows(&mut self) {
        self.rows.clear();
        for (g, group) in self.groups.iter().enumerate() {
            self.rows.push(RenderRow::Header(g));
            if !self.collapsed.contains(&group.path) {
                for i in 0..group.items.len() {
                    self.rows.push(RenderRow::Diag(g, i));
                }
            }
        }
    }
}

impl Default for ProblemsPanel {
    fn default() -> Self {
        Self::new()
    }
}

fn severity_glyph(severity: DiagnosticSeverity) -> (char, Color) {
    match severity {
        DiagnosticSeverity::Error => (GLYPH_ERROR, COLOR_ERROR),
        DiagnosticSeverity::Warning => (GLYPH_WARNING, COLOR_WARNING),
        DiagnosticSeverity::Information | DiagnosticSeverity::Hint => (GLYPH_INFO, COLOR_INFO),
    }
}

impl Widget for &mut ProblemsPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_area = area;
        self.last_scrollbar = Rect::default();
        self.visible_rows = 0;
        self.build_rows();
        if area.width == 0 || area.height == 0 {
            return;
        }
        // Paint the panel background so the previous frame's terminal cells
        // can't ghost through the gaps between rows.
        buf.set_style(area, Style::default().bg(self.theme.editor_bg()));

        if self.groups.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                "No problems have been detected in the workspace.",
                Style::default().fg(COLOR_DIM),
            )))
            .render(
                Rect {
                    x: area.x + 1,
                    y: area.y,
                    width: area.width.saturating_sub(1),
                    height: 1,
                },
                buf,
            );
            return;
        }

        self.first_row_y = area.y;
        self.viewport_rows = area.height;
        let total = self.total_rows();
        self.scroll = self.scroll.min(total.saturating_sub(area.height as usize));

        let bar = scrollbar::vertical_metrics(
            Rect {
                x: area.x + area.width.saturating_sub(1),
                y: area.y,
                width: 1,
                height: area.height,
            },
            total,
            area.height as usize,
            self.scroll,
        );
        let content_w = area.width.saturating_sub(u16::from(bar.is_some()));

        let visible = (area.height as usize).min(self.rows.len().saturating_sub(self.scroll));
        self.visible_rows = visible as u16;
        let brand = self.focus_gradient;

        for row in 0..visible {
            let render_row = self.rows[self.scroll + row].clone();
            let y = area.y + row as u16;
            let row_rect = Rect {
                x: area.x,
                y,
                width: content_w,
                height: 1,
            };
            if let Some(bg) =
                crate::widgets::hover::row_hover_bg(row_rect, self.hover_pointer, brand)
            {
                buf.set_style(row_rect, Style::default().bg(bg));
            }
            let spans = match render_row {
                RenderRow::Header(g) => self.header_spans(g),
                RenderRow::Diag(g, i) => self.diag_spans(g, i),
            };
            Paragraph::new(Line::from(spans)).render(row_rect, buf);
        }

        if let Some(metrics) = bar {
            self.last_scrollbar = metrics.area;
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }
    }
}

impl ProblemsPanel {
    fn header_spans(&self, g: usize) -> Vec<Span<'static>> {
        let group = &self.groups[g];
        let chevron = if self.collapsed.contains(&group.path) {
            crate::icons::CHEVRON_CLOSED
        } else {
            crate::icons::CHEVRON_OPEN
        };
        let suffix = group
            .name
            .rfind('.')
            .map(|i| group.name[i..].to_string())
            .unwrap_or_default();
        let icon = crate::icons::for_path(&group.name, &suffix);
        let mut spans = vec![
            Span::styled(format!("{chevron} "), Style::default().fg(COLOR_DIM)),
            Span::styled(format!("{} ", icon.glyph), Style::default().fg(icon.color)),
            Span::styled(
                group.name.clone(),
                Style::default()
                    .fg(COLOR_HEADER)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if !group.rel_dir.is_empty() {
            spans.push(Span::styled(
                format!(" {}", group.rel_dir),
                Style::default().fg(COLOR_DIM),
            ));
        }
        spans.push(Span::styled(
            format!("  {}", group.items.len()),
            Style::default().fg(COLOR_DIM),
        ));
        spans
    }

    fn diag_spans(&self, g: usize, i: usize) -> Vec<Span<'static>> {
        let item = &self.groups[g].items[i];
        let (glyph, color) = severity_glyph(item.severity);
        // The message can span several lines on the wire; collapse to one row.
        let message = item.message.replace('\n', " ");
        let mut spans = vec![
            Span::styled(format!("   {glyph} "), Style::default().fg(color)),
            Span::styled(message, Style::default().fg(COLOR_MSG)),
        ];
        if !item.source.is_empty() {
            spans.push(Span::styled(
                format!(" {} ", item.source),
                Style::default().fg(COLOR_DIM),
            ));
        }
        spans.push(Span::styled(
            format!("[Ln {}, Col {}]", item.line + 1, item.col + 1),
            Style::default().fg(COLOR_DIM),
        ));
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(line: u32, severity: DiagnosticSeverity, msg: &str) -> ProblemItem {
        ProblemItem {
            line,
            col: 0,
            severity,
            message: msg.to_string(),
            source: "rustc".to_string(),
        }
    }

    fn group(name: &str, items: Vec<ProblemItem>) -> ProblemGroup {
        ProblemGroup {
            path: PathBuf::from(format!("/repo/src/{name}")),
            name: name.to_string(),
            rel_dir: "src".to_string(),
            items,
        }
    }

    fn render(p: &mut ProblemsPanel, w: u16, h: u16) -> String {
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn counts_aggregate_across_files() {
        let mut p = ProblemsPanel::new();
        p.set_groups(vec![
            group(
                "a.rs",
                vec![
                    diag(0, DiagnosticSeverity::Error, "boom"),
                    diag(1, DiagnosticSeverity::Warning, "meh"),
                ],
            ),
            group("b.rs", vec![diag(2, DiagnosticSeverity::Error, "kaboom")]),
        ]);
        assert_eq!(p.total_count(), 3);
        assert_eq!(p.error_count(), 2);
        assert_eq!(p.warning_count(), 1);
    }

    #[test]
    fn header_then_diag_rows_map_to_hits() {
        let mut p = ProblemsPanel::new();
        p.set_groups(vec![group(
            "a.rs",
            vec![
                diag(4, DiagnosticSeverity::Error, "boom"),
                diag(9, DiagnosticSeverity::Warning, "meh"),
            ],
        )]);
        render(&mut p, 60, 6);
        assert_eq!(
            p.hit_at(0),
            Some(ProblemHit::Header(PathBuf::from("/repo/src/a.rs"))),
            "row 0 is the file header",
        );
        assert_eq!(
            p.hit_at(1),
            Some(ProblemHit::Diagnostic {
                path: PathBuf::from("/repo/src/a.rs"),
                line: 4,
                col: 0,
            }),
        );
        assert_eq!(
            p.hit_at(2),
            Some(ProblemHit::Diagnostic {
                path: PathBuf::from("/repo/src/a.rs"),
                line: 9,
                col: 0,
            }),
        );
        assert_eq!(p.hit_at(3), None, "below the last row");
    }

    #[test]
    fn collapse_hides_diagnostic_rows() {
        let mut p = ProblemsPanel::new();
        p.set_groups(vec![group(
            "a.rs",
            vec![diag(0, DiagnosticSeverity::Error, "boom")],
        )]);
        let path = PathBuf::from("/repo/src/a.rs");
        render(&mut p, 60, 6);
        assert_eq!(p.total_rows(), 2, "header + one diagnostic");
        p.toggle_collapse(&path);
        render(&mut p, 60, 6);
        assert_eq!(p.total_rows(), 1, "collapsed: header only");
        assert_eq!(p.hit_at(1), None, "no diagnostic row when collapsed");
        assert_eq!(p.hit_at(0), Some(ProblemHit::Header(path)));
    }

    #[test]
    fn empty_shows_no_problems_message() {
        let mut p = ProblemsPanel::new();
        let text = render(&mut p, 60, 3);
        assert!(text.contains("No problems"), "empty state: {text:?}");
    }

    #[test]
    fn severity_glyphs_are_the_verified_codicons() {
        assert_eq!(severity_glyph(DiagnosticSeverity::Error).0, '\u{ea87}');
        assert_eq!(severity_glyph(DiagnosticSeverity::Warning).0, '\u{ea6c}');
        assert_eq!(
            severity_glyph(DiagnosticSeverity::Information).0,
            '\u{ea74}'
        );
        assert_eq!(severity_glyph(DiagnosticSeverity::Hint).0, '\u{ea74}');
    }

    #[test]
    fn diag_row_shows_message_and_one_based_position() {
        let mut p = ProblemsPanel::new();
        p.set_groups(vec![group(
            "a.rs",
            vec![diag(
                4,
                DiagnosticSeverity::Error,
                "function `run` is never used",
            )],
        )]);
        let text = render(&mut p, 80, 4);
        assert!(text.contains("function `run` is never used"), "{text:?}");
        assert!(text.contains("Ln 5"), "one-based line: {text:?}");
    }
}
