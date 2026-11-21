//! The results list: the worker's hits added in order, the counts in the header, the
//! query's error, moving the selection and scrolling, and the mode toggles.

use super::*;

impl SearchPanel {
    /// Clear the search like VS Code's "Clear Search Results": empty the query
    /// (and the replace text typed against it) and drop every hit, resetting
    /// selection and scroll. The include/exclude glob filters are intentionally
    /// preserved — they are scoping settings, not search results.
    pub fn clear_all(&mut self) {
        self.query.clear();
        self.replace.clear();
        self.carets[SearchField::Query.index()] = None;
        self.carets[SearchField::Replace.index()] = None;
        self.hits.clear();
        self.selected = 0;
        self.scroll = 0;
        self.selection = None;
        self.field_selection = None;
    }

    /// Where a hit belongs in the list (#1340): by workspace root, in the
    /// order the workspace lists them, then by the path under that root,
    /// then by line. The walker is parallel, so files arrive in whatever
    /// order its threads finish them.
    pub(super) fn hit_order<'a>(&self, hit: &'a SearchHit) -> (usize, &'a Path, usize) {
        let roots = if self.roots.is_empty() {
            std::slice::from_ref(&self.root)
        } else {
            self.roots.as_slice()
        };
        // The deepest root that holds it, as the row label picks.
        let owner = roots
            .iter()
            .enumerate()
            .filter(|(_, r)| hit.path.starts_with(r))
            .max_by_key(|(_, r)| r.components().count());
        match owner {
            Some((i, r)) => (
                i,
                hit.path.strip_prefix(r).unwrap_or(&hit.path),
                hit.line_no,
            ),
            None => (usize::MAX, hit.path.as_path(), hit.line_no),
        }
    }

    /// Take in streamed hits, each landing in its final place
    /// ([`Self::hit_order`]) so the list reads the same however the walk
    /// went. The selection stays on its hit.
    ///
    /// This runs on the UI thread for every drain while a scan streams
    /// (#2007), so its cost must not grow with the hits already listed: each
    /// arriving file finds its place by binary search, and the listed hits
    /// are only moved, never compared one by one. Comparing every listed hit
    /// on each drain made a broad query's results crawl in, the UI busier
    /// with each file that arrived.
    pub fn add_hits(&mut self, mut batch: Vec<SearchHit>) {
        if batch.is_empty() {
            return;
        }
        batch.sort_by_cached_key(|h| {
            let (root, rel, line) = self.hit_order(h);
            (root, rel.to_path_buf(), line)
        });
        let old = std::mem::take(&mut self.hits);
        let selected = (self.selected < old.len()).then_some(self.selected);
        let mut merged = Vec::with_capacity(old.len() + batch.len());
        let mut rest = old.into_iter();
        // How many of the listed hits are already in `merged`.
        let mut moved = 0usize;
        let mut prev_path: Option<PathBuf> = None;
        for hit in batch {
            let pending = rest.as_slice();
            // The batch is sorted, so every pending hit sorts after the
            // previous arrival. When that was this same file, a pending hit
            // of another file is past this whole file: only one of this file
            // can still go first, and the search is skipped without it.
            let same_file = prev_path.as_deref() == Some(hit.path.as_path());
            let n = if same_file && pending.first().is_none_or(|o| o.path != hit.path) {
                0
            } else {
                // A listed hit stays ahead of an equal one arriving.
                let key = self.hit_order(&hit);
                pending.partition_point(|o| self.hit_order(o) <= key)
            };
            if let Some(s) = selected
                && (moved..moved + n).contains(&s)
            {
                self.selected = merged.len() + (s - moved);
            }
            merged.extend(rest.by_ref().take(n));
            moved += n;
            if !same_file {
                prev_path = Some(hit.path.clone());
            }
            merged.push(hit);
        }
        if let Some(s) = selected
            && s >= moved
        {
            self.selected = merged.len() + (s - moved);
        }
        merged.extend(rest);
        self.hits = merged;
    }

    /// `query_error` for the current query and toggles, compiled once per
    /// change rather than on every frame.
    pub fn query_error(&mut self) -> Option<String> {
        let fresh = matches!(&self.query_error_memo,
            Some((q, o, _)) if *q == self.query && *o == self.opts);
        if !fresh {
            let error = query_error(&self.query, self.opts);
            self.query_error_memo = Some((self.query.clone(), self.opts, error));
        }
        self.query_error_memo
            .as_ref()
            .and_then(|(_, _, e)| e.clone())
    }

    /// Run the current query, store the results, and reset selection.
    pub fn run_query(&mut self) {
        self.hits.clear();
        self.add_hits(search_workspace(&self.root, &self.query, self.opts));
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
        self.ensure_selected_visible();
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.hits.len() {
            self.selected += 1;
        }
        self.ensure_selected_visible();
    }

    /// Pull `scroll` so the current `selected` row sits inside the viewport.
    /// Only invoked from keyboard-driven moves; wheel scroll and scrollbar
    /// drag must NOT call this - the user's wheel position is sacrosanct
    /// and selection trails wherever they put it.
    pub(super) fn ensure_selected_visible(&mut self) {
        let viewport = self.results_viewport_height();
        if viewport == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + viewport {
            self.scroll = self.selected + 1 - viewport;
        }
    }

    pub fn scroll_up(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
    }

    pub fn scroll_down(&mut self, rows: usize) {
        let viewport = self.results_viewport_height();
        let max_scroll = self.hits.len().saturating_sub(viewport);
        self.scroll = self.scroll.saturating_add(rows).min(max_scroll);
    }

    /// Map a track-y from a scrollbar click/drag back into a scroll offset.
    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let viewport = self.results_viewport_height();
        let Some(metrics) = crate::widgets::scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.hits.len(),
            viewport,
            self.scroll,
        ) else {
            return false;
        };
        let new_scroll = crate::widgets::scrollbar::scroll_for_y(metrics, y);
        let max_scroll = self.hits.len().saturating_sub(viewport);
        self.scroll = new_scroll.min(max_scroll);
        true
    }

    pub(super) fn results_viewport_height(&self) -> usize {
        // Results start at `results_start_offset` rows below `inner.y`. That
        // offset is recomputed each render and grows when the Replace row or
        // the include/exclude detail rows are expanded.
        let inner = self.last_inner;
        if inner.height == 0 {
            return 0;
        }
        let results_top = self.results_start_offset;
        if inner.height <= results_top {
            return 0;
        }
        (inner.height - results_top) as usize
    }

    pub fn selected_hit(&self) -> Option<&SearchHit> {
        self.hits.get(self.selected)
    }

    /// Flip one of the mode toggles (`Aa` / `ab` / `.*`, and the replace
    /// row's `AB`), from a click or `Alt+C` / `Alt+W` / `Alt+R` / `Alt+P`
    /// (#860, #1612). Returns the new state.
    pub fn toggle_opt(&mut self, toggle: SearchToggle) -> bool {
        let flag = match toggle {
            SearchToggle::CaseSensitive => &mut self.opts.case_sensitive,
            SearchToggle::WholeWord => &mut self.opts.whole_word,
            SearchToggle::UseRegex => &mut self.opts.use_regex,
            SearchToggle::PreserveCase => &mut self.opts.preserve_case,
        };
        *flag = !*flag;
        *flag
    }

    /// `(results, files)` for the header: every occurrence counts, two on
    /// one line being two results, as in VS Code and the Replace All
    /// confirmation (#860).
    pub fn result_counts(&self) -> (usize, usize) {
        let results = self.hits.iter().map(|h| h.matches).sum();
        // Drawn on every render, so a path is hashed once per run of hits
        // rather than once per hit: a file's hits arrive together, and one
        // whose hits come in more than one run still counts once.
        let files = self
            .hits
            .first()
            .into_iter()
            .chain(
                self.hits
                    .windows(2)
                    .filter(|w| w[0].path != w[1].path)
                    .map(|w| &w[1]),
            )
            .map(|h| &h.path)
            .collect::<HashSet<_>>()
            .len();
        (results, files)
    }
}
