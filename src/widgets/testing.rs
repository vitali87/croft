//! The Testing sidebar view: a tree of test suites and cases with live pass/
//! fail status, mirroring VS Code's Testing view. Results stream in from the
//! `cargo test` worker ([`crate::testing::worker`]); the panel projects them
//! into a suite tree (grouped by the module path) and exposes the failing-test
//! count that drives the beaker activity-icon badge. A run is kicked off from
//! the Command Palette ("Testing: Run All Tests") or the panel's Enter key.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Widget},
};

use crate::testing::model::{TestCase, TestStatus};
use crate::theme::Theme;
use crate::widgets::scrollbar;

const FOCUS_BORDER_RGB: (u8, u8, u8) = (0x4e, 0x9a, 0xff);
const COLOR_HEADER: Color = Color::Rgb(0xE8, 0xEE, 0xF8);
const COLOR_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const COLOR_CASE: Color = Color::Rgb(0xCC, 0xCC, 0xCC);

/// Codicon status glyphs (Nerd Fonts preserve codicon codepoints): `check`
/// (U+EAB2) for a pass, `error` (U+EA87) for a fail, `circle-filled` (U+EA71)
/// for the running / not-run / skipped dot. Verified against the glyphs already
/// used in `crate::widgets::problems` and `crate::icons`.
const GLYPH_PASS: char = '\u{eab2}';
const GLYPH_FAIL: char = '\u{ea87}';
const GLYPH_DOT: char = '\u{ea71}';

/// A rendered line, used to map a click row back to a case (suite headers are
/// not actionable in M1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenderRow {
    Header(usize),
    Case(usize),
}

pub struct TestingPanel {
    /// All known cases, kept sorted by name; status is updated in place as
    /// result lines stream in.
    cases: Vec<TestCase>,
    running: bool,
    /// `Some(ok)` after a run completes (`ok` = the runner exited 0). Drives the
    /// summary line; `None` before the first run.
    last_run_ok: Option<bool>,
    scroll: usize,

    pub focus_gradient: bool,
    pub theme: Theme,
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,

    pub last_area: Rect,
    rows: Vec<RenderRow>,
    first_row_y: u16,
    viewport_rows: u16,
}

impl TestingPanel {
    pub fn new() -> Self {
        Self {
            cases: Vec::new(),
            running: false,
            last_run_ok: None,
            scroll: 0,
            focus_gradient: false,
            theme: Theme::default(),
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            rows: Vec::new(),
            first_row_y: 0,
            viewport_rows: 0,
        }
    }

    /// A run is starting: clear stale results (cargo recompiles and re-emits the
    /// full set) and show the running state until the first case lands.
    pub fn on_run_started(&mut self) {
        self.cases.clear();
        self.running = true;
        self.last_run_ok = None;
        self.scroll = 0;
    }

    /// Apply one streamed result: update the matching case in place, or insert
    /// it (kept sorted by name so the tree is stable).
    pub fn apply_case(&mut self, case: TestCase) {
        match self.cases.binary_search_by(|c| c.name.cmp(&case.name)) {
            Ok(i) => self.cases[i].status = case.status,
            Err(i) => self.cases.insert(i, case),
        }
    }

    pub fn on_run_finished(&mut self, ok: bool) {
        self.running = false;
        self.last_run_ok = Some(ok);
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Count of failing tests — the number the beaker badge shows.
    pub fn failed_count(&self) -> usize {
        self.cases
            .iter()
            .filter(|c| c.status == TestStatus::Failed)
            .count()
    }

    fn counts(&self) -> (usize, usize, usize) {
        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;
        for c in &self.cases {
            match c.status {
                TestStatus::Passed => passed += 1,
                TestStatus::Failed => failed += 1,
                TestStatus::Skipped => skipped += 1,
                _ => {}
            }
        }
        (passed, failed, skipped)
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.max_scroll());
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn max_scroll(&self) -> usize {
        self.rows
            .len()
            .saturating_sub(self.viewport_rows.max(1) as usize)
    }

    /// Build the flat render-row list: a header per suite followed by its cases,
    /// in the cases' sorted order.
    fn build_rows(&mut self) {
        self.rows.clear();
        let mut last_suite: Option<String> = None;
        for (i, case) in self.cases.iter().enumerate() {
            let (suite, _) = case.suite_and_leaf();
            let suite = suite.unwrap_or("(root)");
            if last_suite.as_deref() != Some(suite) {
                self.rows.push(RenderRow::Header(i));
                last_suite = Some(suite.to_string());
            }
            self.rows.push(RenderRow::Case(i));
        }
    }
}

fn status_glyph(status: TestStatus, theme: Theme) -> (char, Color) {
    match status {
        TestStatus::Passed => (GLYPH_PASS, theme.git_added()),
        TestStatus::Failed => (GLYPH_FAIL, theme.git_deleted()),
        TestStatus::Running => (GLYPH_DOT, theme.accent()),
        TestStatus::Skipped | TestStatus::NotRun => (GLYPH_DOT, COLOR_DIM),
    }
}

impl Default for TestingPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &mut TestingPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let border_style = if self.focused {
            Style::default().fg(Color::Rgb(
                FOCUS_BORDER_RGB.0,
                FOCUS_BORDER_RGB.1,
                FOCUS_BORDER_RGB.2,
            ))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        block.render(area, buf);
        if self.focused && self.focus_gradient {
            crate::gradient::paint_gradient_box(buf, area);
        }
        self.last_area = area;
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Title.
        buf.set_string(
            inner.x + 1,
            inner.y,
            "TESTING",
            Style::default()
                .fg(COLOR_HEADER)
                .add_modifier(Modifier::BOLD),
        );

        // Summary line: running, the pass/fail/skip tally, or a kickoff hint.
        let (passed, failed, skipped) = self.counts();
        let right = inner.x + inner.width;
        let summary_y = inner.y + 1;
        if self.running {
            buf.set_string(
                inner.x + 1,
                summary_y,
                "Running tests…",
                Style::default().fg(self.theme.accent()),
            );
        } else if self.cases.is_empty() {
            buf.set_string(
                inner.x + 1,
                summary_y,
                "Run All Tests (Enter)",
                Style::default().fg(COLOR_DIM),
            );
        } else {
            let summary = format!("{passed} passed · {failed} failed · {skipped} skipped");
            let color = if failed > 0 {
                self.theme.git_deleted()
            } else {
                self.theme.git_added()
            };
            buf.set_string(inner.x + 1, summary_y, &summary, Style::default().fg(color));
        }

        // Tree.
        self.build_rows();
        let body_y0 = inner.y + 2;
        let body_h = inner.height.saturating_sub(2);
        self.first_row_y = body_y0;
        self.viewport_rows = body_h;
        if body_h == 0 {
            return;
        }
        self.scroll = self.scroll.min(self.max_scroll());

        for (shown, row) in self
            .rows
            .iter()
            .skip(self.scroll)
            .take(body_h as usize)
            .enumerate()
        {
            let y = body_y0 + shown as u16;
            match row {
                RenderRow::Header(case_idx) => {
                    let (suite, _) = self.cases[*case_idx].suite_and_leaf();
                    let suite = suite.unwrap_or("(root)");
                    buf.set_string(
                        inner.x + 1,
                        y,
                        suite,
                        Style::default()
                            .fg(COLOR_HEADER)
                            .add_modifier(Modifier::BOLD),
                    );
                }
                RenderRow::Case(case_idx) => {
                    let case = &self.cases[*case_idx];
                    let (_, leaf) = case.suite_and_leaf();
                    let (glyph, color) = status_glyph(case.status, self.theme);
                    buf.set_string(
                        inner.x + 2,
                        y,
                        glyph.to_string(),
                        Style::default().fg(color),
                    );
                    let avail = right.saturating_sub(inner.x + 4) as usize;
                    let leaf: String = leaf.chars().take(avail).collect();
                    buf.set_string(inner.x + 4, y, &leaf, Style::default().fg(COLOR_CASE));
                }
            }
        }

        if let Some(metrics) = scrollbar::vertical_metrics(
            Rect {
                x: right.saturating_sub(1),
                y: body_y0,
                width: 1,
                height: body_h,
            },
            self.rows.len(),
            body_h as usize,
            self.scroll,
        ) {
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_count_reflects_applied_cases() {
        let mut p = TestingPanel::new();
        p.on_run_started();
        p.apply_case(TestCase {
            name: "m::a".into(),
            status: TestStatus::Passed,
        });
        p.apply_case(TestCase {
            name: "m::b".into(),
            status: TestStatus::Failed,
        });
        p.apply_case(TestCase {
            name: "m::c".into(),
            status: TestStatus::Failed,
        });
        assert_eq!(p.failed_count(), 2);
        // Re-applying a name updates in place, never duplicates.
        p.apply_case(TestCase {
            name: "m::b".into(),
            status: TestStatus::Passed,
        });
        assert_eq!(p.failed_count(), 1);
        assert_eq!(p.cases.len(), 3);
    }
}
