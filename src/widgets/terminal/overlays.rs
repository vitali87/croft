//! What is painted over the text: find matches, quick-select hints, trigger highlights,
//! problem-matcher watches, links and the colour palette.

use super::*;

/// One quick-select hint for the render loop: the match span on absolute
/// grid line `line` (char indices into the spacer-skipped row text; the
/// colmap translates back to grid columns), the label overlaid at its start,
/// and how many label chars the user has already typed (those are consumed,
/// only the remainder renders).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintSpan {
    pub line: i32,
    pub start: usize,
    pub len: usize,
    pub label: String,
    pub typed: usize,
}

impl PtyTerminal {
    /// The OSC 8 hyperlink URI stored under viewport cell `(row, col)`, if
    /// any. Hyperlinked cells carry the URI invisibly; the app's
    /// Cmd/Ctrl+click handler checks this before the plain-text URL regex.
    pub fn hyperlink_at(&self, row: usize, col: usize) -> Option<String> {
        let term = self.term.lock();
        if col >= term.columns() {
            return None;
        }
        let line_idx = row as i32 - term.grid().display_offset() as i32;
        if line_idx < term.grid().topmost_line().0 || line_idx >= term.screen_lines() as i32 {
            return None;
        }
        term.grid()[Point::new(Line(line_idx), Column(col))]
            .hyperlink()
            .map(|h| h.uri().to_string())
    }

    /// Set (or clear) the find highlight. Every occurrence of `needle` is
    /// painted across the visible grid on the next render.
    pub fn set_search(&mut self, needle: Option<String>, opts: crate::widgets::search::SearchOpts) {
        self.search_needle = needle.filter(|s| !s.is_empty());
        self.search_opts = opts;
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Mark which occurrence is the active match `(abs_line, col, len)` so the
    /// render loop paints it in the brighter accent. `abs_line` is the first
    /// row of the match's logical line and `col` a char index into it (see
    /// [`FindLine`]). `clock` is the
    /// scroll-clock reading the line was captured against (the
    /// [`Self::find_lines_and_clock`] snapshot); the render re-bases per
    /// frame so the highlight follows its text through streaming output.
    pub fn set_current_match(&mut self, m: Option<(i32, usize, usize)>, clock: i64) {
        self.current_match = m.map(|(line, col, len)| (clock, line, col, len));
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Set (or clear) the quick-select hint overlay. The app owns hint-mode
    /// state and re-pushes the filtered set as the user types label chars;
    /// `clock` is the scroll-clock reading the span lines were captured at
    /// (from [`Self::visible_lines_and_clock`]), reused verbatim on every
    /// re-push so the render-time translation stays anchored to the open.
    pub fn set_hints(&mut self, hints: Option<Vec<HintSpan>>, clock: i64) {
        self.hints = hints.map(|h| (clock, h));
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Swap in a new trigger set. The app calls this for every pane on every
    /// drain tick (so a pane is covered no matter where it was created); the
    /// ptr-eq check makes the steady state a cheap no-op that never dirties
    /// the pane. The reader thread picks a new set up on its next chunk; the
    /// render loop on its next frame.
    pub fn set_triggers(&self, set: std::sync::Arc<crate::triggers::TriggerSet>) {
        let mut cur = self.triggers.lock().unwrap();
        if std::sync::Arc::ptr_eq(&cur, &set) {
            return;
        }
        *cur = set;
        drop(cur);
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Notify/bell trigger firings recorded by the reader thread since the
    /// last drain.
    pub fn drain_trigger_hits(&self) -> Vec<crate::triggers::TriggerHit> {
        self.trigger_rx.try_iter().collect()
    }

    /// Swap in the global background-matcher set (#252), same steady-state
    /// no-op contract as [`Self::set_triggers`].
    pub fn set_watch_set(&self, set: std::sync::Arc<crate::problem_matchers::WatchSet>) {
        let mut cur = self.watch_set.lock().unwrap();
        if std::sync::Arc::ptr_eq(&cur, &set) {
            return;
        }
        *cur = set;
    }

    /// Assign (or clear) the task-specific background matcher for this
    /// pane — the `problemMatcher` of the task running here.
    pub fn set_pane_watch(
        &self,
        matcher: Option<std::sync::Arc<crate::problem_matchers::CompiledMatcher>>,
    ) {
        *self.pane_watch.lock().unwrap() = matcher;
    }

    /// Watch batches published by the reader thread since the last drain:
    /// the pane cwd at publish time plus the cycle's diagnostics (possibly
    /// empty — an empty batch is what clears fixed errors).
    #[allow(clippy::type_complexity)]
    pub fn drain_watch_batches(
        &self,
    ) -> Vec<(
        Option<std::path::PathBuf>,
        Vec<crate::build_matchers::BuildDiag>,
    )> {
        self.watch_rx.try_iter().collect()
    }

    /// Swap the ANSI palette the render loop maps Named/Indexed 0-15 cell
    /// colors through (the theme sync calls this every pass; unchanged
    /// palettes are a no-op so the pane never dirties spuriously).
    pub fn set_palette(&mut self, palette: [(u8, u8, u8); 16]) {
        if self.palette != palette {
            self.palette = palette;
            self.pty_dirty.store(true, Ordering::Release);
        }
    }

    /// The OSC 8 hyperlink under a screen position (host cell coords), if
    /// any — the screen-coord twin of [`Self::hyperlink_at`].
    pub fn hyperlink_at_screen(&self, col: u16, row: u16) -> Option<String> {
        let (r, c) = self.cell_at(col, row)?;
        self.hyperlink_at(r as usize, c as usize)
    }

    /// Test-only: whether a quick-select hint overlay is currently set.
    #[cfg(test)]
    pub fn has_hints_for_test(&self) -> bool {
        self.hints.is_some()
    }

    /// Test-only: whether a find highlight needle is currently set.
    #[cfg(test)]
    pub fn has_search_for_test(&self) -> bool {
        self.search_needle.is_some()
    }
}
