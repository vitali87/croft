//! The Testing sidebar view: a tree of test suites and cases with live pass/
//! fail status, mirroring VS Code's Testing view. Results stream in from the
//! test worker ([`crate::testing::worker`]); the panel projects them
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
/// for the running / skipped dot, `play` (U+EB2C) for a not-yet-run case and
/// the suite headers — the glyph doubles as the click-to-run button, so the
/// idle state advertises it. Verified against the glyphs already used in
/// `crate::widgets::problems` and `crate::icons`.
const GLYPH_PASS: char = '\u{eab2}';
const GLYPH_FAIL: char = '\u{ea87}';
const GLYPH_DOT: char = '\u{ea71}';
const GLYPH_PLAY: char = '\u{eb2c}';

/// A rendered line, used to map a click row back to a case or suite header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenderRow {
    Header(usize),
    Case(usize),
}

/// What a click in the tree resolves to (see [`TestingPanel::hit_at`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowHit {
    /// The play glyph of a test case: run it.
    RunCase(String),
    /// A test case's name: reveal its source in the editor.
    ShowCase(String),
    /// The play glyph of a suite header: run the whole suite.
    RunSuite(String),
}

pub struct TestingPanel {
    /// All known cases, kept sorted by name; status is updated in place as
    /// result lines stream in.
    cases: Vec<TestCase>,
    activity: Activity,
    /// `Some(ok)` after a run completes (`ok` = the runner exited 0); `None`
    /// before the first run. Surfaces a failed run that reported no Failed
    /// case (compile error, zero matching tests) in the summary line.
    last_run_ok: Option<bool>,
    /// Latest cargo build-status line while busy (e.g. "Compiling ratatui"), so
    /// a long compile shows movement instead of a static "Discovering tests".
    progress: Option<String>,
    /// Latched by [`Self::on_refused`] and consumed by the app's drain, which
    /// surfaces the no-runner status message.
    refused: bool,
    /// Pre-run statuses of the cases the last `start_single`/`start_filter`
    /// marked Running (`None` = the start inserted the case). A worker
    /// refusal restores the tree from this snapshot exactly — old
    /// Passed/Failed/Skipped results come back instead of demoting to
    /// NotRun. Cleared when the run actually starts answering.
    prerun: Vec<(String, Option<TestStatus>)>,
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
            refused: false,
            prerun: Vec::new(),
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
        self.prerun.clear();
        self.activity = activity;
        self.last_run_ok = None;
        self.progress = None;
        self.scroll = 0;
    }

    /// Forget everything (workspace re-root): the new project's tests are
    /// unrelated, so the tree, tally, busy state, and scroll all start over.
    /// Leaving the old tree in place would also block `open_testing_view`'s
    /// discover-on-empty, so the new project would never be discovered.
    pub fn reset(&mut self) {
        self.cases.clear();
        self.activity = Activity::Idle;
        self.last_run_ok = None;
        self.progress = None;
        // The refusal latch and rollback snapshot belong to the old
        // workspace; carrying either across a re-root would surface a stale
        // no-runner status or restore cases into the new project's tree.
        self.refused = false;
        self.prerun.clear();
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
        // Snapshot the pre-run status (None = about to be inserted) so a
        // worker refusal can put the tree back exactly.
        let old = self.cases.iter().find(|c| c.name == name).map(|c| c.status);
        self.prerun = vec![(name.to_string(), old)];
        self.apply_case(TestCase {
            name: name.to_string(),
            status: TestStatus::Running,
        });
    }

    /// Begin a filtered run (a suite, or a run-at-cursor by leaf name): mark
    /// every known case whose name contains `pattern` as Running and enter the
    /// busy state, keeping the rest of the tree. cargo's name filter is a
    /// substring match, so the same `pattern` selects the same set.
    pub fn start_filter(&mut self, pattern: &str) {
        self.activity = Activity::Running;
        self.progress = None;
        // Snapshot each selected case's pre-run status for a refusal rollback.
        self.prerun = Vec::new();
        for c in &mut self.cases {
            if c.name.contains(pattern) {
                self.prerun.push((c.name.clone(), Some(c.status)));
                c.status = TestStatus::Running;
            }
        }
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
    /// or `None` for discovery (which reports no pass/fail). A marked case the
    /// run never reported (compile error, a test renamed since discovery) is
    /// rolled back like a refusal — its old result returns, a start-inserted
    /// case disappears — instead of spinning its Running dot forever.
    pub fn on_finished(&mut self, ok: Option<bool>) {
        self.activity = Activity::Idle;
        self.progress = None;
        for (name, old) in std::mem::take(&mut self.prerun) {
            let Some(i) = self.cases.iter().position(|c| c.name == name) else {
                continue;
            };
            if self.cases[i].status != TestStatus::Running {
                continue; // it reported: keep the fresh result
            }
            match old {
                Some(status) => self.cases[i].status = status,
                None => {
                    self.cases.remove(i);
                }
            }
        }
        if ok.is_some() {
            self.last_run_ok = ok;
        }
    }

    /// The worker refused a queued request because no enabled runner claims
    /// the root anymore (disabled between the app's check and the queue
    /// drain). Restore the tree exactly as it was before the `start_*` call
    /// painted Running marks — old results come back, a start-inserted case
    /// is removed — and latch the refusal for the app to surface as a status.
    /// (A bare finish would have stranded the marks Running forever.)
    pub fn on_refused(&mut self) {
        self.activity = Activity::Idle;
        self.progress = None;
        for (name, old) in std::mem::take(&mut self.prerun) {
            match old {
                Some(status) => {
                    if let Some(c) = self.cases.iter_mut().find(|c| c.name == name) {
                        c.status = status;
                    }
                }
                None => self.cases.retain(|c| c.name != name),
            }
        }
        self.refused = true;
    }

    /// Consume the refusal latch (one status message per refusal).
    pub fn take_refusal(&mut self) -> bool {
        std::mem::take(&mut self.refused)
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

    #[cfg(test)]
    pub fn cases_for_test(&self) -> Vec<(String, TestStatus)> {
        self.cases
            .iter()
            .map(|c| (c.name.clone(), c.status))
            .collect()
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

    /// What a click at `(x, y)` in the tree hits. The play/status glyph runs
    /// the case or suite; the case name shows (jumps to) its source — VS Code
    /// separates the two the same way: the label reveals, the play icon runs.
    /// A suite header's name is inert so a stray click never kicks a whole
    /// suite. The x thresholds mirror the render columns (glyphs at
    /// `inner.x + 1`/`+ 2`, names at `inner.x + 3`/`+ 4`).
    pub fn hit_at(&self, x: u16, y: u16) -> Option<RowHit> {
        if y < self.first_row_y || y >= self.first_row_y + self.viewport_rows {
            return None;
        }
        // `last_area` is the full bordered rect and the caller gates on it,
        // so both border columns reach here; they are pane chrome, not tree
        // — without this bound a left-border click satisfied the glyph
        // thresholds below and ran a whole suite.
        if x <= self.last_area.x || x + 1 >= self.last_area.x + self.last_area.width {
            return None;
        }
        let inner_x = self.last_area.x + 1;
        let shown = (y - self.first_row_y) as usize;
        match self.rows.get(self.scroll + shown)? {
            RenderRow::Case(idx) => {
                let name = self.cases[*idx].name.clone();
                if x < inner_x + 4 {
                    Some(RowHit::RunCase(name))
                } else {
                    Some(RowHit::ShowCase(name))
                }
            }
            RenderRow::Header(idx) => {
                let suite = self.cases[*idx].suite_and_leaf().0?.to_string();
                (x < inner_x + 3).then_some(RowHit::RunSuite(suite))
            }
        }
    }

    /// A named case's current status, for tests that assert run marking.
    #[cfg(test)]
    pub fn status_of(&self, name: &str) -> Option<TestStatus> {
        self.cases.iter().find(|c| c.name == name).map(|c| c.status)
    }

    /// The full name of the ONLY discovered case whose leaf matches, or
    /// `None` when the tree is empty, stale, or the leaf ambiguous — the
    /// caller then falls back to a substring run. Lets run-at-cursor target
    /// `parse::run` exactly instead of every test containing `run`.
    pub fn sole_case_with_leaf(&self, leaf: &str) -> Option<String> {
        let mut it = self.cases.iter().filter(|c| c.suite_and_leaf().1 == leaf);
        let first = it.next()?;
        it.next().is_none().then(|| first.name.clone())
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
        TestStatus::Skipped => (GLYPH_DOT, COLOR_DIM),
        TestStatus::NotRun => (GLYPH_PLAY, COLOR_DIM),
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
        // Column budget up to the scrollbar lane. `set_stringn` clips by
        // DISPLAY width, so a double-width (CJK) name stops at the budget
        // instead of painting twice it and bleeding through the border.
        let avail = |start_x: u16| text_right.saturating_sub(start_x) as usize;

        // Summary line: busy state, the pass/fail/skip tally, or a kickoff hint.
        let (passed, failed, skipped) = self.counts();
        let summary_y = inner.y + 1;
        if self.activity != Activity::Idle {
            let label = match self.activity {
                Activity::Discovering => "Discovering tests",
                _ => "Running tests",
            };
            buf.set_stringn(
                inner.x + 1,
                summary_y,
                label,
                avail(inner.x + 1),
                Style::default().fg(self.theme.accent()),
            );
        } else if self.cases.is_empty() {
            // A run that wiped the tree and then failed (compile error on a
            // full run) must not hide behind the kickoff hint.
            let (text, color) = if self.last_run_ok == Some(false) {
                ("Run failed", self.theme.git_deleted())
            } else {
                ("Run All Tests (Enter)", COLOR_DIM)
            };
            buf.set_stringn(
                inner.x + 1,
                summary_y,
                text,
                avail(inner.x + 1),
                Style::default().fg(color),
            );
        } else {
            // A nonzero runner exit with no Failed case in the tree (compile
            // error, a filter matching nothing) would otherwise keep the old
            // green tally: say so.
            let run_failed = self.last_run_ok == Some(false) && failed == 0;
            // The marker leads so a narrow panel cannot clip it away.
            let summary = if run_failed {
                format!("run failed · {passed} passed · {skipped} skipped")
            } else {
                format!("{passed} passed · {failed} failed · {skipped} skipped")
            };
            let color = if failed > 0 || run_failed {
                self.theme.git_deleted()
            } else {
                self.theme.git_added()
            };
            buf.set_stringn(
                inner.x + 1,
                summary_y,
                &summary,
                avail(inner.x + 1),
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
            buf.set_stringn(
                inner.x + 1,
                body_y0,
                p,
                avail(inner.x + 1),
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
                    // The suite's run button; its name (like a case's) is inert.
                    buf.set_string(
                        inner.x + 1,
                        y,
                        GLYPH_PLAY.to_string(),
                        Style::default().fg(COLOR_DIM),
                    );
                    buf.set_stringn(
                        inner.x + 3,
                        y,
                        suite,
                        avail(inner.x + 3),
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
                    buf.set_stringn(
                        inner.x + 4,
                        y,
                        leaf,
                        avail(inner.x + 4),
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

    /// A worker refusal (runner disabled between the app's check and the
    /// queue drain) must restore the tree EXACTLY as it was before the
    /// `start_*` call painted Running marks: earlier Passed/Failed/Skipped
    /// results come back (not a demotion to NotRun), a case the start
    /// inserted disappears again, and one refusal is latched for the app's
    /// status line.
    #[test]
    fn a_refused_run_restores_prerun_statuses_and_latches_once() {
        let mut p = TestingPanel::new();
        p.on_busy_started(Activity::Running);
        for (n, st) in [
            ("m::a", TestStatus::Passed),
            ("m::b", TestStatus::Failed),
            ("m::c", TestStatus::Skipped),
        ] {
            p.apply_case(TestCase {
                name: n.into(),
                status: st,
            });
        }
        p.on_finished(Some(false));
        p.start_filter("m::");

        p.on_refused();

        assert!(!p.is_busy(), "a refusal returns the panel to idle");
        for (n, st) in [
            ("m::a", TestStatus::Passed),
            ("m::b", TestStatus::Failed),
            ("m::c", TestStatus::Skipped),
        ] {
            let c = p.cases.iter().find(|c| c.name == n).unwrap();
            assert_eq!(
                c.status, st,
                "{n} must get its pre-run result back, not NotRun"
            );
        }
        assert!(p.take_refusal(), "the refusal is latched for the app");
        assert!(!p.take_refusal(), "and consumed exactly once");

        // A single-run of a test the tree didn't know inserts it Running;
        // the refusal must remove it again, not leave a phantom NotRun row.
        p.start_single("m::new");
        p.on_refused();
        assert!(
            !p.cases.iter().any(|c| c.name == "m::new"),
            "a start-inserted case disappears again on refusal"
        );
        assert!(p.take_refusal());

        // A re-root (Explorer Make Root) drops both the latch and the
        // snapshot: neither a stale no-runner status nor an old workspace's
        // rollback may leak into the new project.
        p.start_single("m::stale");
        p.on_refused();
        p.reset();
        assert!(!p.take_refusal(), "reset clears the refusal latch");
        p.apply_case(TestCase {
            name: "other::t".into(),
            status: TestStatus::Passed,
        });
        p.on_refused();
        let t = p.cases.iter().find(|c| c.name == "other::t").unwrap();
        assert_eq!(
            t.status,
            TestStatus::Passed,
            "reset drops the old snapshot, so a later refusal restores nothing"
        );
    }

    /// A run can complete without ever reporting a case it marked Running: a
    /// compile error (diagnostics only, no `test ...` lines), a test renamed
    /// since discovery (`--exact` matches nothing), a suite whose module no
    /// longer builds. The finish must roll those marks back like a refusal
    /// does, or the accent dot spins forever on a run that already died.
    #[test]
    fn a_finished_run_rolls_back_cases_that_never_reported() {
        let mut p = TestingPanel::new();
        p.on_busy_started(Activity::Running);
        p.apply_case(TestCase {
            name: "m::a".into(),
            status: TestStatus::Passed,
        });
        p.on_finished(Some(true));

        // Compile error: the run reports nothing for the marked case.
        p.start_single("m::a");
        p.on_finished(Some(false));
        let a = p.cases.iter().find(|c| c.name == "m::a").unwrap();
        assert_eq!(
            a.status,
            TestStatus::Passed,
            "a case the run never reported gets its old result back"
        );

        // A start-inserted case disappears again instead of lingering.
        p.start_single("m::renamed");
        p.on_finished(Some(false));
        assert!(
            !p.cases.iter().any(|c| c.name == "m::renamed"),
            "a case the start inserted is removed when the run never reports it"
        );

        // A case that DID report keeps its fresh result, not the snapshot.
        p.start_single("m::a");
        p.apply_case(TestCase {
            name: "m::a".into(),
            status: TestStatus::Failed,
        });
        p.on_finished(Some(false));
        let a = p.cases.iter().find(|c| c.name == "m::a").unwrap();
        assert_eq!(a.status, TestStatus::Failed);
    }

    /// A failed run whose failure never reached the tree (compile error, zero
    /// matching tests) must still be visible: the summary carries a "run
    /// failed" marker whenever the runner exited nonzero but no case shows
    /// Failed. Without it the panel silently keeps the old green tally.
    #[test]
    fn a_failed_run_with_no_reported_failure_is_visible_in_the_summary() {
        let mut p = TestingPanel::new();
        p.on_busy_started(Activity::Running);
        p.apply_case(TestCase {
            name: "m::a".into(),
            status: TestStatus::Passed,
        });
        p.on_finished(Some(true));
        p.start_single("m::a");
        p.on_finished(Some(false)); // compile error: nothing reported

        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let summary: String = (0..40).map(|x| buf[(x, 2)].symbol()).collect();
        assert!(
            summary.contains("run failed"),
            "a nonzero exit with no Failed case must say so, got {summary:?}"
        );
    }

    /// Double-width characters in a test name must clip to display columns,
    /// not chars: counting chars lets a CJK name paint twice its budget and
    /// overwrite the scrollbar lane and the panel's right border.
    #[test]
    fn wide_characters_never_bleed_past_the_scrollbar_lane() {
        let mut p = TestingPanel::new();
        p.on_busy_started(Activity::Running);
        p.apply_case(TestCase {
            name: "套件::测试用例的名字很长很长很长很长很长".into(),
            status: TestStatus::Passed,
        });
        p.on_finished(Some(true));

        // The buffer is the whole frame, wider than the panel: ratatui only
        // clips at the frame edge, so the panel must clip itself.
        let area = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        (&mut p).render(area, &mut buf);
        for y in 1..9 {
            assert_eq!(
                buf[(19, y)].symbol(),
                "│",
                "the right border at row {y} must survive a wide-char name"
            );
        }
        let bled: String = (20..40).map(|x| buf[(x, 4)].symbol()).collect();
        assert_eq!(
            bled.trim(),
            "",
            "nothing may paint past the panel into the neighbouring pane"
        );
    }

    #[test]
    fn glyph_clicks_run_and_name_clicks_show() {
        let mut p = TestingPanel::new();
        p.on_busy_started(Activity::Running);
        for n in ["m::a", "m::b"] {
            p.apply_case(TestCase {
                name: n.into(),
                status: TestStatus::Passed,
            });
        }
        p.on_finished(Some(true));
        // Mirror render's geometry: border at x=0 so inner.x = 1; the tree
        // starts at first_row_y with Header(m), Case(a), Case(b).
        p.last_area = Rect::new(0, 0, 30, 10);
        p.first_row_y = 3;
        p.viewport_rows = 5;
        p.build_rows();

        // Case rows: glyph column (inner.x+2 .. inner.x+4) runs, the name shows.
        assert_eq!(p.hit_at(3, 4), Some(RowHit::RunCase("m::a".into())));
        assert_eq!(p.hit_at(10, 4), Some(RowHit::ShowCase("m::a".into())));
        assert_eq!(p.hit_at(10, 5), Some(RowHit::ShowCase("m::b".into())));
        // Suite header: only its play glyph (inner.x+1 .. inner.x+3) runs; the
        // name itself is inert so a stray click never kicks a whole suite.
        assert_eq!(p.hit_at(2, 3), Some(RowHit::RunSuite("m".into())));
        assert_eq!(p.hit_at(10, 3), None);
        // The pane's border columns are chrome, not tree: a click on the
        // left border must never run a suite or a case, and one on the
        // right border must never jump to a case's source.
        assert_eq!(p.hit_at(0, 3), None, "left border on a suite header");
        assert_eq!(p.hit_at(0, 4), None, "left border on a case row");
        assert_eq!(p.hit_at(29, 4), None, "right border on a case row");
        // Outside the tree: nothing.
        assert_eq!(p.hit_at(3, 2), None);
        assert_eq!(p.hit_at(3, 9), None);
    }

    #[test]
    fn start_filter_marks_the_whole_suite_running_and_leaves_others() {
        let mut p = TestingPanel::new();
        p.on_busy_started(Activity::Running);
        for n in ["suite_a::one", "suite_a::two", "suite_b::three"] {
            p.apply_case(TestCase {
                name: n.into(),
                status: TestStatus::Passed,
            });
        }
        p.on_finished(Some(true));

        p.start_filter("suite_a");

        assert!(p.is_busy());
        let running: Vec<&str> = p
            .cases
            .iter()
            .filter(|c| c.status == TestStatus::Running)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(running, ["suite_a::one", "suite_a::two"]);
        let other = p.cases.iter().find(|c| c.name == "suite_b::three").unwrap();
        assert_eq!(other.status, TestStatus::Passed, "other suites untouched");
    }
}
