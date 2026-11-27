//! What a click lands on: the toggles, the buttons beside the fields, the fields
//! themselves, a result row and its path, and the tooltip under the pointer.

use super::*;

impl SearchPanel {
    pub fn paste_button_at(&self, col: u16, row: u16) -> bool {
        if self.paste_button_w == 0 || row != self.paste_button_y {
            return false;
        }
        col >= self.paste_button_x && col < self.paste_button_x + self.paste_button_w
    }

    /// True when `(col, row)` lands on the expand chevron (toggle Replace row).
    pub fn chevron_at(&self, col: u16, row: u16) -> bool {
        row == self.chevron_y && col == self.chevron_x
    }

    /// True when `(col, row)` lands on the `...` ellipsis (toggle details).
    pub fn ellipsis_at(&self, col: u16, row: u16) -> bool {
        self.ellipsis_x != 0 && row == self.ellipsis_y && col == self.ellipsis_x
    }

    /// True when `(col, row)` lands on the replace-all action icon.
    pub fn replace_all_at(&self, col: u16, row: u16) -> bool {
        self.replace_all_x != 0 && row == self.replace_all_y && col == self.replace_all_x
    }

    /// True when `(col, row)` lands on the header "Refresh" action icon.
    pub fn refresh_at(&self, col: u16, row: u16) -> bool {
        self.refresh_x != 0 && row == self.refresh_y && col == self.refresh_x
    }

    /// True when `(col, row)` lands on the header "Clear Search Results" icon.
    pub fn clear_at(&self, col: u16, row: u16) -> bool {
        self.clear_x != 0 && row == self.clear_y && col == self.clear_x
    }

    /// The hover-tooltip label for whatever actionable control sits under
    /// `(col, row)`, or `None` over plain text/fields/empty space. Reuses the
    /// same hit-tests the click handler uses, so a hint targets exactly the
    /// cells that act. Text inputs deliberately return `None`: their purpose is
    /// shown by the inline placeholder, not a tooltip.
    pub fn tooltip_at(&self, col: u16, row: u16) -> Option<&'static str> {
        if self.refresh_at(col, row) {
            return Some("Refresh");
        }
        if self.clear_at(col, row) {
            return Some("Clear Search Results");
        }
        if self.chevron_at(col, row) {
            return Some(if self.replace_open {
                "Collapse Replace"
            } else {
                "Toggle Replace"
            });
        }
        // The keyboard route rides the label, as in VS Code (#860).
        if self.ellipsis_at(col, row) {
            return Some("Toggle Search Details (Alt+D)");
        }
        if self.replace_all_at(col, row) {
            return Some("Replace All");
        }
        if let Some(toggle) = self.toggle_at(col, row) {
            return Some(match toggle {
                SearchToggle::CaseSensitive => "Match Case (Alt+C)",
                SearchToggle::WholeWord => "Match Whole Word (Alt+W)",
                SearchToggle::UseRegex => "Use Regular Expression (Alt+R)",
                SearchToggle::PreserveCase => "Preserve Case (Alt+P)",
            });
        }
        None
    }

    /// The full path of the result row under `(col, row)`, for its hover hint
    /// (#2008): a row names only the file, so hits in same-named files
    /// (`mod.rs`, `index.ts`) read the same. `None` off the rows and on the
    /// scrollbar.
    pub fn result_path_at(&self, col: u16, row: u16) -> Option<&Path> {
        let inner = self.last_inner;
        // The scrollbar takes the right column once the hits overflow, as the
        // render draws it. Judged by the hits listed now, not the last
        // frame's bar: hits that stream in under a resting pointer can make
        // the list overflow before the next render.
        let overflows = self.hits.len() > self.results_viewport_height();
        let right = inner.x + inner.width - u16::from(overflows && inner.width > 0);
        if col < inner.x || col >= right {
            return None;
        }
        self.hit_at_y_from(row, self.scroll)
            .map(|i| self.hits[i].path.as_path())
    }

    /// Map a click to the input field whose editable row it lands on, for
    /// click-to-focus. Returns `None` when the click is outside every input.
    pub fn field_at(&self, col: u16, row: u16) -> Option<SearchField> {
        let on = |r: Rect| r.width != 0 && row == r.y && col >= r.x && col < r.x + r.width;
        if on(self.query_input_rect) {
            return Some(SearchField::Query);
        }
        if on(self.replace_input_rect) {
            return Some(SearchField::Replace);
        }
        if on(self.include_input_rect) {
            return Some(SearchField::Include);
        }
        if on(self.exclude_input_rect) {
            return Some(SearchField::Exclude);
        }
        None
    }

    /// True when a click lands on the project-exclusions checkbox row
    /// (#1345).
    pub fn project_exclude_toggle_at(&self, col: u16, row: u16) -> bool {
        let r = self.project_exclude_rect;
        r.width != 0 && row == r.y && col >= r.x && col < r.x + r.width
    }

    /// If the cell `(col, row)` falls on one of the three toggle glyphs
    /// (each rendered as a 2-cell pair), return a mutable pointer to the
    /// corresponding flag so the caller can flip it. Returns `None`
    /// otherwise.
    pub fn toggle_at(&self, col: u16, row: u16) -> Option<SearchToggle> {
        let preserve = self.toggle_preserve_x;
        if preserve != 0 && row == self.toggle_preserve_y && col >= preserve && col < preserve + 2 {
            return Some(SearchToggle::PreserveCase);
        }
        if row != self.toggle_y {
            return None;
        }
        for (start, kind) in [
            (self.toggle_case_x, SearchToggle::CaseSensitive),
            (self.toggle_word_x, SearchToggle::WholeWord),
            (self.toggle_regex_x, SearchToggle::UseRegex),
        ] {
            if start != 0 && col >= start && col < start + 2 {
                return Some(kind);
            }
        }
        None
    }

    /// Map a click row to a hit index, if any. Hits sit below the input
    /// cluster; the exact first row depends on which optional rows (Replace,
    /// include/exclude) are expanded, captured in `results_start_offset`.
    pub fn hit_at_y(&self, y: u16) -> Option<usize> {
        self.hit_at_y_from(y, self.last_scroll)
    }

    /// The row a pointer at `y` meets, from `scroll`: the drawn scroll for a
    /// click (#2751), which acts on what the screen shows, and the live one
    /// for the hover hint, which the next frame draws beside those rows.
    fn hit_at_y_from(&self, y: u16, scroll: usize) -> Option<usize> {
        let inner = self.last_inner;
        let results_start = inner.y + self.results_start_offset;
        if y < results_start || y >= inner.y + inner.height {
            return None;
        }
        let row_in_results = (y - results_start) as usize;
        let idx = scroll + row_in_results;
        if idx < self.hits.len() {
            Some(idx)
        } else {
            None
        }
    }
}
