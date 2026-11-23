//! Moving through the tree: the arrow, page, Home and End keys with and without
//! Shift, scrolling, expanding and collapsing, Collapse All, revealing a path, and
//! type-to-find.

use super::*;

impl FileTree {
    pub(super) fn collapse(&mut self, idx: usize) {
        let depth = self.nodes[idx].depth;
        self.nodes[idx].expanded = false;
        let mut end = idx + 1;
        while end < self.nodes.len() && self.nodes[end].depth > depth {
            end += 1;
        }
        self.nodes.drain((idx + 1)..end);
        self.nodes[idx].loaded = false;
        if self.selected >= self.nodes.len() {
            self.selected = self.nodes.len().saturating_sub(1);
        }
        if self.anchor >= self.nodes.len() {
            self.anchor = self.nodes.len().saturating_sub(1);
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
        self.anchor = self.selected;
        self.marked.clear();
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.nodes.len() {
            self.selected += 1;
        }
        self.anchor = self.selected;
        self.marked.clear();
    }

    pub fn page_up(&mut self, page: usize) {
        self.selected = self.selected.saturating_sub(page);
        self.anchor = self.selected;
        self.marked.clear();
    }

    pub fn page_down(&mut self, page: usize) {
        self.selected = (self.selected + page).min(self.nodes.len().saturating_sub(1));
        self.anchor = self.selected;
        self.marked.clear();
    }

    pub fn move_up_extend(&mut self) {
        if self.selected > 0 {
            let new_idx = self.selected - 1;
            self.extend_to(new_idx);
        }
    }

    pub fn move_down_extend(&mut self) {
        if self.selected + 1 < self.nodes.len() {
            let new_idx = self.selected + 1;
            self.extend_to(new_idx);
        }
    }

    pub fn page_up_extend(&mut self, page: usize) {
        let new_idx = self.selected.saturating_sub(page);
        self.extend_to(new_idx);
    }

    pub fn page_down_extend(&mut self, page: usize) {
        let new_idx = (self.selected + page).min(self.nodes.len().saturating_sub(1));
        self.extend_to(new_idx);
    }

    pub fn scroll_up(&mut self, rows: usize) {
        self.scroll_to(self.scroll.saturating_sub(rows));
    }

    pub fn scroll_down(&mut self, rows: usize) {
        self.scroll_to(self.scroll.saturating_add(rows));
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.nodes.len(),
            self.last_inner.height as usize,
            self.scroll,
        ) else {
            return false;
        };
        self.scroll_to(scrollbar::scroll_for_y(metrics, y));
        true
    }

    pub(super) fn scroll_to(&mut self, top: usize) {
        let viewport = self.last_inner.height as usize;
        if viewport == 0 || self.nodes.is_empty() {
            self.scroll = 0;
            self.selected = 0;
            return;
        }
        self.scroll = top.min(self.nodes.len().saturating_sub(viewport));
        let last_visible = (self.scroll + viewport - 1).min(self.nodes.len().saturating_sub(1));
        if self.selected < self.scroll {
            self.selected = self.scroll;
        } else if self.selected > last_visible {
            self.selected = last_visible;
        }
    }

    pub fn home(&mut self) {
        self.selected = 0;
        self.anchor = 0;
        self.marked.clear();
    }

    pub fn end(&mut self) {
        self.selected = self.nodes.len().saturating_sub(1);
        self.anchor = self.selected;
        self.marked.clear();
    }

    pub fn home_extend(&mut self) {
        self.extend_to(0);
    }

    pub fn end_extend(&mut self) {
        let last = self.nodes.len().saturating_sub(1);
        self.extend_to(last);
    }

    /// Activate the selected node. Returns Some(path) if a file should be opened.
    pub fn activate(&mut self) -> Option<PathBuf> {
        let idx = self.selected;
        if idx >= self.nodes.len() {
            return None;
        }
        if self.nodes[idx].is_dir {
            if self.nodes[idx].expanded {
                self.collapse(idx);
            } else {
                self.nodes[idx].expanded = true;
                self.load_children(idx);
            }
            None
        } else {
            Some(self.nodes[idx].path.clone())
        }
    }

    pub fn expand_selected(&mut self) {
        let idx = self.selected;
        if idx >= self.nodes.len() || !self.nodes[idx].is_dir || self.nodes[idx].expanded {
            return;
        }
        self.nodes[idx].expanded = true;
        self.load_children(idx);
    }

    pub fn collapse_selected(&mut self) {
        let idx = self.selected;
        if idx >= self.nodes.len() || !self.nodes[idx].is_dir || !self.nodes[idx].expanded {
            return;
        }
        self.collapse(idx);
    }

    /// Collapse every expanded directory except the workspace root, so the
    /// tree shows just the root's immediate children (VS Code's "Collapse
    /// Folders in Explorer" title action). Walking from the bottom up keeps
    /// indices valid: `collapse` only drains nodes *after* the collapsed one,
    /// which we have already passed, and never shifts the lower indices we are
    /// still to visit. Resets the cursor and scroll to the root row.
    pub fn collapse_all(&mut self) {
        let mut idx = self.nodes.len();
        while idx > 1 {
            idx -= 1;
            if idx >= self.nodes.len() {
                continue;
            }
            // Every ROOT section row stays expanded — with multiple
            // workspace roots (#145) they are all "the workspace root"
            // the action's name spares, not just index 0.
            if self.nodes[idx].depth > 0 && self.nodes[idx].is_dir && self.nodes[idx].expanded {
                self.collapse(idx);
            }
        }
        self.selected = 0;
        self.anchor = 0;
        self.scroll = 0;
        self.marked.clear();
    }

    /// Expand every ancestor directory of `target` (relative to `self.root`)
    /// so the target row becomes visible in the flattened node list, then
    /// select that row and scroll it into view. No-op when `target` does
    /// not live under the workspace root or does not exist as a node after
    /// the parent walk (e.g. the FS changed underneath the open file). Used
    /// by the Cmd+P Quick Open finder so picking a file matches VS Code's
    /// `explorer.autoReveal` behaviour — picking a deeply-nested file pops
    /// open every parent and parks the cursor on the row.
    pub fn reveal_path(&mut self, target: &Path) -> bool {
        // The OWNING root (longest prefix, #145): a target under a second
        // root walks down from that root, not the primary.
        let Some(owner) = self.owning_root(target).map(Path::to_path_buf) else {
            return false;
        };
        let Ok(rel) = target.strip_prefix(&owner) else {
            return false;
        };
        let mut current_path = owner;
        for component in rel.components() {
            current_path.push(component.as_os_str());
            let is_last = current_path == target;
            let Some(idx) = self.row_of(&current_path) else {
                return false;
            };
            if !is_last && self.nodes[idx].is_dir && !self.nodes[idx].expanded {
                self.nodes[idx].expanded = true;
                self.load_children(idx);
            }
        }
        let Some(final_idx) = self.row_of(target) else {
            return false;
        };
        self.selected = final_idx;
        self.anchor = final_idx;
        self.marked.clear();
        self.make_selected_visible();
        true
    }

    /// Scroll so the currently-selected row is in view. Caller invokes this
    /// after `selected` is set programmatically (vs. via a keystroke that
    /// already scrolls in `scroll_to`).
    pub fn make_selected_visible(&mut self) {
        let viewport = self.last_inner.height as usize;
        if viewport == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + viewport {
            self.scroll = self.selected + 1 - viewport;
        }
        let max_scroll = self.nodes.len().saturating_sub(viewport);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    /// Locate the next visible node whose filename starts with `prefix`
    /// (ASCII case-insensitive). The search begins at `start` and wraps
    /// to 0 so a prefix that exists earlier in the list is still found
    /// when the caller is partway down. Returns `None` for an empty
    /// prefix, an empty tree, or no match.
    pub fn find_prefix(&self, prefix: &str, start: usize) -> Option<usize> {
        if prefix.is_empty() || self.nodes.is_empty() {
            return None;
        }
        let needle = prefix.to_ascii_lowercase();
        let n = self.nodes.len();
        let start = start.min(n - 1);
        for off in 0..n {
            let idx = (start + off) % n;
            let name = self.nodes[idx].label().to_ascii_lowercase();
            if name.starts_with(&needle) {
                return Some(idx);
            }
        }
        None
    }
}
