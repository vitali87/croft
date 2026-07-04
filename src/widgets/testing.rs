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

use crate::testing::model::{Activity, TestCase, TestStatus};
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
    activity: Activity,
    /// `Some(ok)` after a run completes (`ok` = the runner exited 0). Drives the
    /// summary line; `None` before the first run.
    last_run_ok: Option<bool>,
    /// Latest cargo build-status line while busy (e.g. "Compiling ratatui"), so
    /// a long compile shows movement instead of a static "Discovering tests".
    progress: Option<String>,
    scroll: usize,

    pub focus_gradient: bool,
    pub theme: Theme,
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,

    pub last_area: Rect,
    pub last_scrollbar: Rect,
    rows: Vec<RenderRow>,
    first_row_y: u16,
    viewport_rows: u16,
}

impl TestingPanel {
    pub fn new() -> Self {
        Self {
            cases: Vec::new(),
            activity: Activity::Idle,
            last_run_ok: None,
            progress: None,
            scroll: 0,
            focus_gradient: false,
            theme: Theme::default(),
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            rows: Vec::new(),
            first_row_y: 0,
            viewport_rows: 0,
        }
    }

    /// A run or discovery is starting: clear stale results (the test binary
    /// recompiles and re-emits the full set) and show the busy state until the
    /// first case lands.
    pub fn on_busy_started(&mut self, activity: Activity) {
        self.cases.clear();
        self.activity = activity;
        self.last_run_ok = None;
        self.progress = None;
        self.scroll = 0;
    }

    /// Update the live compile-progress line shown while busy.
    pub fn set_progress(&mut self, line: String) {
        self.progress = Some(line);
    }

    /// Begin a single-test run: mark that case Running and enter the busy state
    /// WITHOUT clearing the tree (unlike a full run/discovery, which re-emits
    /// every case). The worker streams the result back to update it in place.
    pub fn start_single(&mut self, name: &str) {
        self.activity = Activity::Running;
        self.progress = None;
        self.apply_case(TestCase {
            name: name.to_string(),
            status: TestStatus::Running,
        });
    }

    /// Apply one streamed result: update the matching case in place, or insert
    /// it (kept sorted by name so the tree is stable).
    pub fn apply_case(&mut self, case: TestCase) {
        match self.cases.binary_search_by(|c| c.name.cmp(&case.name)) {
            Ok(i) => self.cases[i].status = case.status,
            Err(i) => self.cases.insert(i, case),
        }
    }

    /// A run or discovery finished. `ok` is the runner's exit success for a run,
    /// or `None` for discovery (which reports no pass/fail).
    pub fn on_finished(&mut self, ok: Option<bool>) {
        self.activity = Activity::Idle;
        self.progress = None;
        if ok.is_some() {
            self.last_run_ok = ok;
        }
    }

    pub fn is_running(&self) -> bool {
        self.activity == Activity::Running
    }

    /// Whether a run or discovery is in flight (used to suppress overlapping
    /// kicks — the test binary compile is expensive, so never double-spawn).
    pub fn is_busy(&self) -> bool {
        self.activity != Activity::Idle
    }

    /// No tests known yet (never discovered or run). Drives the one-shot
    /// discover-on-open so re-opening a populated view doesn't recompile.
    pub fn is_empty(&self) -> bool {
        self.cases.is_empty()
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

    /// Map a click/drag y within the scrollbar lane to a scroll offset. Returns
    /// true if the offset moved.
    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.rows.len(),
            self.viewport_rows.max(1) as usize,
            self.scroll,
        ) else {
            return false;
        };
        let target = scrollbar::scroll_for_y(metrics, y);
        let moved = target != self.scroll;
        self.scroll = target;
        moved
    }

    /// The name of the test case at screen row `y`, if that row is a case (not a
    /// suite header or empty space). Used to run a single test on click.
    pub fn case_name_at(&self, y: u16) -> Option<String> {
        if y < self.first_row_y {
            return None;
        }
        let shown = (y - self.first_row_y) as usize;
        match self.rows.get(self.scroll + shown)? {
            RenderRow::Case(idx) => Some(self.cases[*idx].name.clone()),
            RenderRow::Header(_) => None,
        }
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

        // All content stops one column short of the border so the vertical
        // scrollbar (drawn at `right - 1`) has its own column and no string
        // bleeds past the panel into the neighbouring pane.
        let right = inner.x + inner.width;
        let text_right = right.saturating_sub(1);
        let clip = |s: &str, start_x: u16| -> String {
            let avail = text_right.saturating_sub(start_x) as usize;
            s.chars().take(avail).collect()
        };

        // Summary line: busy state, the pass/fail/skip tally, or a kickoff hint.
        let (passed, failed, skipped) = self.counts();
        let summary_y = inner.y + 1;
        if self.activity != Activity::Idle {
            let label = match self.activity {
                Activity::Discovering => "Discovering tests",
                _ => "Running tests",
            };
            buf.set_string(
                inner.x + 1,
                summary_y,
                clip(label, inner.x + 1),
                Style::default().fg(self.theme.accent()),
            );
        } else if self.cases.is_empty() {
            buf.set_string(
                inner.x + 1,
                summary_y,
                clip("Run All Tests (Enter)", inner.x + 1),
                Style::default().fg(COLOR_DIM),
            );
        } else {
            let summary = format!("{passed} passed · {failed} failed · {skipped} skipped");
            let color = if failed > 0 {
                self.theme.git_deleted()
            } else {
                self.theme.git_added()
            };
            buf.set_string(
                inner.x + 1,
                summary_y,
                clip(&summary, inner.x + 1),
                Style::default().fg(color),
            );
        }

        // Tree.
        self.build_rows();
        let body_y0 = inner.y + 2;
        let body_h = inner.height.saturating_sub(2);

        // While compiling the test binary the tree is empty; show the latest
        // cargo status line there so a multi-minute discovery visibly moves.
        if self.activity != Activity::Idle
            && self.cases.is_empty()
            && let Some(p) = &self.progress
            && body_h > 0
        {
            buf.set_string(
                inner.x + 1,
                body_y0,
                clip(p, inner.x + 1),
                Style::default().fg(COLOR_DIM),
            );
        }
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
                        clip(suite, inner.x + 1),
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
                    buf.set_string(
                        inner.x + 4,
                        y,
                        clip(leaf, inner.x + 4),
                        Style::default().fg(COLOR_CASE),
                    );
                }
            }
        }

        let lane = Rect {
            x: right.saturating_sub(1),
            y: body_y0,
            width: 1,
            height: body_h,
        };
        if let Some(metrics) =
            scrollbar::vertical_metrics(lane, self.rows.len(), body_h as usize, self.scroll)
        {
            self.last_scrollbar = lane;
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        } else {
            self.last_scrollbar = Rect::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_count_reflects_applied_cases() {
        let mut p = TestingPanel::new();
        p.on_busy_started(Activity::Running);
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

    #[test]
    fn start_single_marks_one_case_running_without_clearing_the_tree() {
        let mut p = TestingPanel::new();
        p.on_busy_started(Activity::Running);
        for n in ["m::a", "m::b", "m::c"] {
            p.apply_case(TestCase {
                name: n.into(),
                status: TestStatus::Passed,
            });
        }
        p.on_finished(Some(true));

        p.start_single("m::b");

        assert!(p.is_busy(), "a single-test run enters the busy state");
        assert_eq!(p.cases.len(), 3, "the rest of the tree is preserved");
        let b = p.cases.iter().find(|c| c.name == "m::b").unwrap();
        assert_eq!(b.status, TestStatus::Running, "only the target is Running");
    }
}
