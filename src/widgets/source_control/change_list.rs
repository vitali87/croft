//! The change list: taking in git's status, selecting rows (one, several or all), moving the
//! selection in the order rows are drawn, scrolling, and laying the list out as section
//! headers and entry rows.

use super::*;

impl SourceControlPanel {
    pub fn set_status(&mut self, status: GitStatus, entries: Vec<ChangeEntry>) {
        self.status = status;
        self.seed_prepared_message();
        let row_of = |i: usize| {
            self.entries
                .get(i)
                .map(|e| (e.path.clone(), e.kind.section()))
        };
        let selected = self.selected_change.and_then(row_of);
        let multi: Vec<_> = self
            .multi_selection
            .iter()
            .filter_map(|&i| row_of(i))
            .collect();
        self.entries = entries;
        // A change set refresh can re-order entries (staging an untracked
        // file moves it up): the selection follows its file, so the next
        // key acts on the row the user picked (#871), and on a partly
        // staged file's row in the same section while it has one. A file
        // that left the list hands its slot to the row that took its
        // place, and a slot past the end clears.
        let rows = &self.entries;
        let follow = |(path, section): &(String, crate::git::ChangeSection)| {
            let same_row = |e: &ChangeEntry| &e.path == path && e.kind.section() == *section;
            rows.iter()
                .position(same_row)
                .or_else(|| rows.iter().position(|e| &e.path == path))
        };
        // The multi-selection follows its files the same way, so Ctrl+S
        // stages what is highlighted; a file that left the list leaves it.
        let multi_selection = multi.iter().filter_map(follow).collect();
        if let Some(idx) = selected.as_ref().and_then(follow) {
            self.selected_change = Some(idx);
        } else if let Some(idx) = self.selected_change
            && idx >= self.entries.len()
        {
            self.selected_change = None;
        }
        self.multi_selection = multi_selection;
        if self.entries.is_empty() && self.keys == KeyStop::List {
            self.keys = KeyStop::Message;
        }
    }

    /// Click-to-select: returns the entry index now selected, if the
    /// click landed on an entry row. Resets the multi-selection to just
    /// this row — Cmd+A is the way to opt into multi.
    pub fn select_change_at(&mut self, y: u16) -> Option<usize> {
        let idx = self.entry_at_y(y)?;
        self.selected_change = Some(idx);
        self.multi_selection.clear();
        self.multi_selection.insert(idx);
        Some(idx)
    }

    /// Cmd+A in the SC pane: every entry row is now in `multi_selection`,
    /// so the Stage shortcut acts on all of them at once. Leaves
    /// `selected_change` alone so the diff editor keeps showing the same
    /// file. Returns the new selection count.
    pub fn select_all_changes(&mut self) -> usize {
        self.multi_selection = (0..self.entries.len()).collect();
        if self.selected_change.is_none() && !self.entries.is_empty() {
            self.selected_change = Some(0);
        }
        self.multi_selection.len()
    }

    pub fn changes_count(&self) -> usize {
        self.entries.len()
    }

    /// Hand the keyboard to the change list (#871), selecting its first
    /// row when no row is selected. False, leaving the keys where they
    /// were, when the list is empty.
    pub fn focus_list(&mut self) -> bool {
        let Some(&first) = self.entry_order().first() else {
            return false;
        };
        self.keys = KeyStop::List;
        if self.selected_change.is_none() {
            self.select_only(first);
        }
        self.reveal_selected();
        true
    }

    /// Move the selection one row up or down the list as it is drawn
    /// (sections first, so not `entries` order), scrolling it into view.
    /// False on Up from the first row, where the list ends.
    pub fn move_selection(&mut self, up: bool) -> bool {
        let order = self.entry_order();
        let pos = self
            .selected_change
            .and_then(|s| order.iter().position(|&i| i == s));
        let next = match pos {
            None => order.first(),
            Some(0) if up => return false,
            Some(p) if up => order.get(p - 1),
            Some(p) => order.get(p + 1),
        };
        if let Some(&idx) = next {
            self.select_only(idx);
            self.reveal_selected();
        }
        true
    }

    /// Entry indices in the order the list draws them.
    pub(super) fn entry_order(&self) -> Vec<usize> {
        self.list_layout()
            .into_iter()
            .filter_map(|line| match line {
                ListLine::Entry(i) => Some(i),
                ListLine::Header(_) => None,
            })
            .collect()
    }

    pub(super) fn select_only(&mut self, idx: usize) {
        self.selected_change = Some(idx);
        self.multi_selection.clear();
        self.multi_selection.insert(idx);
    }

    /// Scroll so the selected row is on screen, with its section header
    /// when that is the row above it.
    pub(super) fn reveal_selected(&mut self) {
        let viewport = self.last_list_area.height as usize;
        let lines = self.list_layout();
        let Some(line) = self
            .selected_change
            .and_then(|s| lines.iter().position(|l| *l == ListLine::Entry(s)))
        else {
            return;
        };
        if viewport == 0 {
            return;
        }
        let top = match line.checked_sub(1).map(|l| lines[l]) {
            Some(ListLine::Header(_)) => line - 1,
            _ => line,
        };
        if top < self.scroll {
            self.scroll = top;
        } else if line >= self.scroll + viewport {
            self.scroll = line + 1 - viewport;
        }
    }

    pub fn entry_at_y(&self, y: u16) -> Option<usize> {
        if y < self.last_list_area.y || y >= self.last_list_area.y + self.last_list_area.height {
            return None;
        }
        let row = (y - self.last_list_area.y) as usize;
        // The list interleaves section headers with rows; tag each visual
        // line with whether it points to an entry, then look up by offset.
        let lines = self.list_layout();
        let line_idx = self.scroll + row;
        match lines.get(line_idx)? {
            ListLine::Entry(idx) => Some(*idx),
            ListLine::Header(_) => None,
        }
    }

    pub fn scroll_up(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
    }

    pub fn scroll_down(&mut self, rows: usize) {
        let max = self
            .list_layout()
            .len()
            .saturating_sub(self.last_list_area.height as usize);
        self.scroll = (self.scroll + rows).min(max);
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let total = self.list_layout().len();
        let viewport = self.last_list_area.height as usize;
        let Some(metrics) =
            scrollbar::vertical_metrics(self.last_scrollbar, total, viewport, self.scroll)
        else {
            return false;
        };
        self.scroll = scrollbar::scroll_for_y(metrics, y);
        true
    }

    /// Compute the visual layout of the change list as a flat sequence of
    /// rows: section headers and entry rows. Used for hit-testing and
    /// scrollbar metrics.
    pub(super) fn list_layout(&self) -> Vec<ListLine> {
        let mut out = Vec::new();
        for section in [
            ChangeSection::Conflicts,
            ChangeSection::Staged,
            ChangeSection::Changes,
            ChangeSection::Untracked,
        ] {
            let mut any = false;
            for (i, entry) in self.entries.iter().enumerate() {
                if entry.kind.section() == section {
                    if !any {
                        out.push(ListLine::Header(section));
                        any = true;
                    }
                    out.push(ListLine::Entry(i));
                }
            }
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ListLine {
    Header(ChangeSection),
    Entry(usize),
}

pub(super) fn section_label(section: ChangeSection) -> &'static str {
    match section {
        ChangeSection::Conflicts => "MERGE CONFLICTS",
        ChangeSection::Staged => "STAGED CHANGES",
        ChangeSection::Changes => "CHANGES",
        ChangeSection::Untracked => "UNTRACKED",
    }
}
