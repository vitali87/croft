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

/// The severity filter applied to the list, cycled from the toolbar. `Warnings`
/// keeps errors and warnings (VS Code's "Show Errors & Warnings"); `Errors`
/// keeps only errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProblemFilter {
    All,
    Errors,
    Warnings,
}

impl ProblemFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Errors,
            Self::Errors => Self::Warnings,
            Self::Warnings => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Errors => "Errors",
            Self::Warnings => "Errors & Warnings",
        }
    }

    fn keeps(self, sev: DiagnosticSeverity) -> bool {
        match self {
            Self::All => true,
            Self::Errors => sev == DiagnosticSeverity::Error,
            Self::Warnings => {
                matches!(sev, DiagnosticSeverity::Error | DiagnosticSeverity::Warning)
            }
        }
    }
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
    /// Severity filter, cycled from the toolbar (VS Code's funnel menu).
    filter: ProblemFilter,
    /// Free-text filter (VS Code's Problems filter box): a case-insensitive
    /// substring matched against each diagnostic's message and its file's
    /// path, composing with the severity filter. Typed directly while the
    /// panel is focused; Backspace pops, Esc clears.
    pub text_filter: String,
    /// Group diagnostics under a file header (VS Code default) or show a flat,
    /// file-annotated list. Toggled from the toolbar.
    group_by_file: bool,

    pub focus_gradient: bool,
    pub theme: Theme,
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,

    pub last_area: Rect,
    pub last_scrollbar: Rect,

    // Toolbar hit rects, recomputed each render.
    filter_rect: Rect,
    group_rect: Rect,

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
            filter: ProblemFilter::All,
            text_filter: String::new(),
            group_by_file: true,
            focus_gradient: false,
            theme: Theme::default(),
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            filter_rect: Rect::default(),
            group_rect: Rect::default(),
            rows: Vec::new(),
            first_row_y: 0,
            visible_rows: 0,
            viewport_rows: 0,
        }
    }

    /// The current grouped projection (read-only; tests and counters).
    pub fn groups(&self) -> &[ProblemGroup] {
        &self.groups
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

    /// The visible groups after the severity filter: `(group index, indices of
    /// its items that pass the filter)`. Groups with no passing item are
    /// dropped so a filtered-away file shows no header. Indices are into
    /// `self.groups`, so `hit_at` / `*_spans` need no remapping.
    /// True when `it` in `group` passes BOTH the severity cycle and the
    /// free-text filter (case-insensitive substring of the message or the
    /// file path).
    fn keeps_item(&self, group: &ProblemGroup, it: &ProblemItem) -> bool {
        if !self.filter.keeps(it.severity) {
            return false;
        }
        if self.text_filter.is_empty() {
            return true;
        }
        let needle = self.text_filter.to_lowercase();
        it.message.to_lowercase().contains(&needle)
            || group
                .path
                .to_string_lossy()
                .to_lowercase()
                .contains(&needle)
    }

    pub fn push_filter_char(&mut self, c: char) {
        self.text_filter.push(c);
        self.scroll = 0;
    }

    pub fn pop_filter_char(&mut self) {
        self.text_filter.pop();
        self.scroll = 0;
    }

    /// Clear the text filter, reporting whether there was one (so Esc can
    /// clear first and only leave the panel when already clear).
    pub fn clear_text_filter(&mut self) -> bool {
        if self.text_filter.is_empty() {
            return false;
        }
        self.text_filter.clear();
        self.scroll = 0;
        true
    }

    fn visible_groups(&self) -> Vec<(usize, Vec<usize>)> {
        self.groups
            .iter()
            .enumerate()
            .filter_map(|(g, group)| {
                let items: Vec<usize> = group
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, it)| self.keeps_item(group, it))
                    .map(|(i, _)| i)
                    .collect();
                (!items.is_empty()).then_some((g, items))
            })
            .collect()
    }

    /// The number of laid-out rows for the current filter, collapse state, and
    /// grouping. Grouped: one header per visible file plus one row per shown
    /// diagnostic of an expanded group. Flat: one row per shown diagnostic.
    fn total_rows(&self) -> usize {
        self.visible_groups()
            .iter()
            .map(|(g, items)| {
                if !self.group_by_file {
                    items.len()
                } else if self.collapsed.contains(&self.groups[*g].path) {
                    1
                } else {
                    1 + items.len()
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

    /// Rebuild the flat row list for the current filter, collapse state, and
    /// grouping. Called at the top of `render` so `rows` always matches what
    /// was painted.
    fn build_rows(&mut self) {
        self.rows.clear();
        for (g, items) in self.visible_groups() {
            if !self.group_by_file {
                // Flat list: diagnostics only, no headers, no collapse.
                self.rows
                    .extend(items.into_iter().map(|i| RenderRow::Diag(g, i)));
                continue;
            }
            self.rows.push(RenderRow::Header(g));
            if !self.collapsed.contains(&self.groups[g].path) {
                self.rows
                    .extend(items.into_iter().map(|i| RenderRow::Diag(g, i)));
            }
        }
    }

    /// Cycle the severity filter (toolbar funnel). Resets scroll so the
    /// viewport can't strand past a now-shorter list.
    fn cycle_filter(&mut self) {
        self.filter = self.filter.next();
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// Toggle grouping diagnostics by file (toolbar). Resets scroll.
    fn toggle_group_by_file(&mut self) {
        self.group_by_file = !self.group_by_file;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// Handle a click at `(x, y)`. Returns true if it landed on a toolbar
    /// button (and was consumed); false if the caller should resolve it as a
    /// row click via [`ProblemsPanel::hit_at`].
    pub fn click(&mut self, x: u16, y: u16) -> bool {
        if rect_has(self.filter_rect, x, y) {
            self.cycle_filter();
            return true;
        }
        if rect_has(self.group_rect, x, y) {
            self.toggle_group_by_file();
            return true;
        }
        false
    }
}

fn rect_has(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

impl Default for ProblemsPanel {
    fn default() -> Self {
        Self::new()
    }
}

fn severity_glyph(severity: DiagnosticSeverity, theme: crate::theme::Theme) -> (char, Color) {
    match severity {
        DiagnosticSeverity::Error => (GLYPH_ERROR, theme.ui(COLOR_ERROR)),
        DiagnosticSeverity::Warning => (GLYPH_WARNING, theme.ui(COLOR_WARNING)),
        DiagnosticSeverity::Information | DiagnosticSeverity::Hint => {
            (GLYPH_INFO, theme.ui(COLOR_INFO))
        }
    }
}

impl Widget for &mut ProblemsPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_area = area;
        self.last_scrollbar = Rect::default();
        self.filter_rect = Rect::default();
        self.group_rect = Rect::default();
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
                Style::default().fg(self.theme.ui(COLOR_DIM)),
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

        // Reserve the top row for the toolbar (filter funnel + group toggle);
        // the diagnostic list scrolls in the remaining rows below it.
        let toolbar = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        self.render_toolbar(toolbar, buf);
        let body = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height.saturating_sub(1),
        };
        if body.height == 0 {
            return;
        }
        if self.rows.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                "No problems match the current filter.",
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            )))
            .render(
                Rect {
                    x: body.x + 1,
                    y: body.y,
                    width: body.width.saturating_sub(1),
                    height: 1,
                },
                buf,
            );
            return;
        }

        self.first_row_y = body.y;
        self.viewport_rows = body.height;
        let total = self.total_rows();
        self.scroll = self.scroll.min(total.saturating_sub(body.height as usize));

        let bar = scrollbar::vertical_metrics(
            Rect {
                x: body.x + body.width.saturating_sub(1),
                y: body.y,
                width: 1,
                height: body.height,
            },
            total,
            body.height as usize,
            self.scroll,
        );
        let content_w = body.width.saturating_sub(u16::from(bar.is_some()));

        let visible = (body.height as usize).min(self.rows.len().saturating_sub(self.scroll));
        self.visible_rows = visible as u16;

        for row in 0..visible {
            let render_row = self.rows[self.scroll + row].clone();
            let y = body.y + row as u16;
            let row_rect = Rect {
                x: body.x,
                y,
                width: content_w,
                height: 1,
            };
            if let Some(bg) =
                crate::widgets::hover::row_hover_bg(row_rect, self.hover_pointer, self.theme)
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
    /// Paint the toolbar row, right-aligned like VS Code's panel filter: a
    /// severity-filter control (`<label> ▾`) and a grouping toggle
    /// (`Grouped`/`Flat`). Both are plain muted text at their default state;
    /// when off-default they get a soft translucent accent chip so a lit filter
    /// or flat view reads at a glance. Clickable (hit rects recorded for
    /// [`ProblemsPanel::click`]).
    fn render_toolbar(&mut self, area: Rect, buf: &mut Buffer) {
        let muted = self.theme.ignored_fg();
        let accent = self.theme.accent();
        let chip = self.theme.accent_chip_bg();

        let filter_label = if self.text_filter.is_empty() {
            format!(" {} \u{25be} ", self.filter.label())
        } else {
            format!(
                " {} \u{25be} \u{eb85} {} ",
                self.filter.label(),
                self.text_filter
            )
        };
        let group_label = format!(
            " {} ",
            if self.group_by_file {
                "Grouped"
            } else {
                "Flat"
            }
        );
        let fw = filter_label.chars().count() as u16;
        let gw = group_label.chars().count() as u16;
        // A one-cell gap between the two controls and one trailing cell of
        // breathing room at the right edge.
        let total = fw + 1 + gw + 1;

        let mut x = area.x + area.width.saturating_sub(total.min(area.width));
        let mut control = |label: String, active: bool, buf: &mut Buffer| -> Rect {
            let w = (label.chars().count() as u16).min(area.right().saturating_sub(x));
            let r = Rect {
                x,
                y: area.y,
                width: w,
                height: 1,
            };
            let style = if active {
                Style::default().fg(accent).bg(chip)
            } else {
                Style::default().fg(muted)
            };
            Paragraph::new(Line::from(Span::styled(label, style))).render(r, buf);
            x += w + 1;
            r
        };
        self.filter_rect = control(
            filter_label,
            self.filter != ProblemFilter::All || !self.text_filter.is_empty(),
            buf,
        );
        self.group_rect = control(group_label, !self.group_by_file, buf);
    }

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
            Span::styled(
                format!("{chevron} "),
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            ),
            Span::styled(
                format!("{} ", icon.glyph),
                Style::default().fg(self.theme.ui(icon.color)),
            ),
            Span::styled(
                group.name.clone(),
                Style::default()
                    .fg(self.theme.ui(COLOR_HEADER))
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if !group.rel_dir.is_empty() {
            spans.push(Span::styled(
                format!(" {}", group.rel_dir),
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            ));
        }
        // Count what the filter actually shows beneath this header, not the
        // group's raw size — "Errors" over 1 error + 5 warnings reads 1.
        let shown = group
            .items
            .iter()
            .filter(|it| self.keeps_item(group, it))
            .count();
        spans.push(Span::styled(
            format!("  {shown}"),
            Style::default().fg(self.theme.ui(COLOR_DIM)),
        ));
        spans
    }

    fn diag_spans(&self, g: usize, i: usize) -> Vec<Span<'static>> {
        let item = &self.groups[g].items[i];
        let (glyph, color) = severity_glyph(item.severity, self.theme);
        // The message can span several lines on the wire; collapse to one row.
        let message = item.message.replace('\n', " ");
        let mut spans = vec![
            Span::styled(format!("   {glyph} "), Style::default().fg(color)),
            Span::styled(message, Style::default().fg(self.theme.ui(COLOR_MSG))),
        ];
        if !item.source.is_empty() {
            spans.push(Span::styled(
                format!(" {} ", item.source),
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            ));
        }
        spans.push(Span::styled(
            format!("[Ln {}, Col {}]", item.line + 1, item.col + 1),
            Style::default().fg(self.theme.ui(COLOR_DIM)),
        ));
        // Flat list has no file header, so name the file on the row itself.
        if !self.group_by_file {
            spans.push(Span::styled(
                format!(" {}", self.groups[g].name),
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            ));
        }
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

    /// Issue #212: the free-text filter narrows by message AND path
    /// substring, case-insensitively, composing with the severity cycle.
    #[test]
    fn text_filter_narrows_by_message_and_path() {
        let mut p = ProblemsPanel::new();
        p.set_groups(vec![
            group(
                "alpha.rs",
                vec![
                    diag(0, DiagnosticSeverity::Error, "Mismatched types"),
                    diag(1, DiagnosticSeverity::Warning, "unused variable"),
                ],
            ),
            group("beta.rs", vec![diag(0, DiagnosticSeverity::Error, "boom")]),
        ]);
        assert_eq!(p.total_rows(), 5, "2 headers + 3 diagnostics");
        // Message substring, case-insensitive.
        for c in "MISMATCH".chars() {
            p.push_filter_char(c.to_ascii_lowercase());
        }
        assert_eq!(p.total_rows(), 2, "one header + the one matching row");
        // Backspace back to empty restores everything.
        while !p.text_filter.is_empty() {
            p.pop_filter_char();
        }
        assert_eq!(p.total_rows(), 5);
        // Path substring: "beta" keeps the beta.rs group only.
        for c in "beta".chars() {
            p.push_filter_char(c);
        }
        assert_eq!(p.total_rows(), 2);
        // Composes with the severity cycle. Narrow the text to alpha's
        // WARNING, then cycle to Errors-only: nothing survives both.
        // (`Warnings` here means "Errors & Warnings", so Errors is the
        // filter that actually excludes a warning.)
        while !p.text_filter.is_empty() {
            p.pop_filter_char();
        }
        for c in "unused".chars() {
            p.push_filter_char(c);
        }
        assert_eq!(p.total_rows(), 2, "the warning matches on its own");
        while p.filter != ProblemFilter::Errors {
            p.cycle_filter();
        }
        assert_eq!(p.total_rows(), 0, "text and severity filters compose");
        assert!(p.clear_text_filter(), "clearing reports it had text");
        assert!(!p.clear_text_filter(), "second clear is a no-op");
    }

    /// The file header's count badge must reflect the ACTIVE severity
    /// filter: with "Errors" selected, a file with 1 error and 2 warnings
    /// used to still show 3 over a single listed row.
    #[test]
    fn the_header_count_follows_the_severity_filter() {
        let mut p = ProblemsPanel::new();
        p.set_groups(vec![group(
            "a.rs",
            vec![
                diag(0, DiagnosticSeverity::Error, "boom"),
                diag(1, DiagnosticSeverity::Warning, "meh1"),
                diag(2, DiagnosticSeverity::Warning, "meh2"),
            ],
        )]);
        while p.filter != ProblemFilter::Errors {
            p.cycle_filter();
        }
        let text: String = p
            .header_spans(0)
            .iter()
            .map(|s| s.content.clone())
            .collect();
        assert!(
            text.ends_with(" 1"),
            "the badge must count only diagnostics passing the filter, got {text:?}"
        );
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
        // Row 0 is the toolbar; the file header and diagnostics start at row 1.
        assert_eq!(p.hit_at(0), None, "row 0 is the toolbar, not a list row");
        assert_eq!(
            p.hit_at(1),
            Some(ProblemHit::Header(PathBuf::from("/repo/src/a.rs"))),
            "row 1 is the file header",
        );
        assert_eq!(
            p.hit_at(2),
            Some(ProblemHit::Diagnostic {
                path: PathBuf::from("/repo/src/a.rs"),
                line: 4,
                col: 0,
            }),
        );
        assert_eq!(
            p.hit_at(3),
            Some(ProblemHit::Diagnostic {
                path: PathBuf::from("/repo/src/a.rs"),
                line: 9,
                col: 0,
            }),
        );
        assert_eq!(p.hit_at(4), None, "below the last row");
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
        // Row 0 is the toolbar; the header sits at row 1 and has no diag below.
        assert_eq!(p.hit_at(2), None, "no diagnostic row when collapsed");
        assert_eq!(p.hit_at(1), Some(ProblemHit::Header(path)));
    }

    #[test]
    fn empty_shows_no_problems_message() {
        let mut p = ProblemsPanel::new();
        let text = render(&mut p, 60, 3);
        assert!(text.contains("No problems"), "empty state: {text:?}");
    }

    #[test]
    fn severity_glyphs_are_the_verified_codicons() {
        assert_eq!(
            severity_glyph(DiagnosticSeverity::Error, crate::theme::Theme::default()).0,
            '\u{ea87}'
        );
        assert_eq!(
            severity_glyph(DiagnosticSeverity::Warning, crate::theme::Theme::default()).0,
            '\u{ea6c}'
        );
        assert_eq!(
            severity_glyph(
                DiagnosticSeverity::Information,
                crate::theme::Theme::default()
            )
            .0,
            '\u{ea74}'
        );
        assert_eq!(
            severity_glyph(DiagnosticSeverity::Hint, crate::theme::Theme::default()).0,
            '\u{ea74}'
        );
    }

    #[test]
    fn severity_filter_hides_lower_severities_and_empty_groups() {
        let mut p = ProblemsPanel::new();
        p.set_groups(vec![
            group(
                "a.rs",
                vec![
                    diag(0, DiagnosticSeverity::Error, "boom"),
                    diag(1, DiagnosticSeverity::Warning, "meh"),
                ],
            ),
            group(
                "b.rs",
                vec![diag(2, DiagnosticSeverity::Warning, "only warn")],
            ),
        ]);
        // All: 2 headers + 3 diags = 5 rows.
        assert_eq!(p.total_rows(), 5);
        // Errors only: b.rs has no error, so it drops entirely; a.rs keeps its
        // one error. 1 header + 1 diag = 2 rows.
        p.cycle_filter();
        assert_eq!(p.filter, ProblemFilter::Errors);
        assert_eq!(p.total_rows(), 2);
        // Tab badge counts stay unfiltered.
        assert_eq!(p.total_count(), 3);
        assert_eq!(p.error_count(), 1);
    }

    #[test]
    fn group_by_file_toggle_flattens_to_diagnostics_only() {
        let mut p = ProblemsPanel::new();
        p.set_groups(vec![
            group("a.rs", vec![diag(0, DiagnosticSeverity::Error, "boom")]),
            group("b.rs", vec![diag(2, DiagnosticSeverity::Error, "kaboom")]),
        ]);
        // Grouped: 2 headers + 2 diags.
        assert_eq!(p.total_rows(), 4);
        p.toggle_group_by_file();
        assert!(!p.group_by_file);
        // Flat: 2 diags, no headers.
        assert_eq!(p.total_rows(), 2);
        render(&mut p, 80, 6);
        // Row 1 (below the toolbar) is a diagnostic, not a header.
        assert!(matches!(p.hit_at(1), Some(ProblemHit::Diagnostic { .. })));
    }

    #[test]
    fn active_toolbar_control_gets_the_accent_chip_default_is_plain() {
        let mut p = ProblemsPanel::new();
        p.set_groups(vec![group(
            "a.rs",
            vec![diag(0, DiagnosticSeverity::Error, "boom")],
        )]);
        // Default: filter All, grouped — neither control is chipped.
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        let chip = p.theme.accent_chip_bg();
        let cell_bg = |b: &Buffer, r: Rect| b[(r.x, r.y)].style().bg;
        assert_ne!(
            cell_bg(&buf, p.filter_rect),
            Some(chip),
            "All filter is plain, no chip",
        );

        // Turn the filter off-default: it lights up with the accent chip.
        p.cycle_filter();
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        assert_eq!(
            cell_bg(&buf, p.filter_rect),
            Some(chip),
            "a non-default filter is chipped with the theme accent",
        );
        assert_ne!(
            cell_bg(&buf, p.group_rect),
            Some(chip),
            "grouping is still default, so its control stays plain",
        );
    }

    #[test]
    fn toolbar_click_cycles_filter_then_toggles_grouping() {
        let mut p = ProblemsPanel::new();
        p.set_groups(vec![group(
            "a.rs",
            vec![diag(0, DiagnosticSeverity::Error, "boom")],
        )]);
        render(&mut p, 80, 6);
        let fr = p.filter_rect;
        assert!(fr.width > 0, "filter button rect recorded");
        assert!(p.click(fr.x, fr.y), "filter button consumes the click");
        assert_eq!(p.filter, ProblemFilter::Errors);
        let gr = p.group_rect;
        assert!(p.click(gr.x, gr.y), "group button consumes the click");
        assert!(!p.group_by_file);
        // A click in the empty body is not consumed by the toolbar.
        assert!(!p.click(0, 5));
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
