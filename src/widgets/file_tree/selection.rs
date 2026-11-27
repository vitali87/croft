//! Selecting rows: the cursor, the multi-selection and its shift-anchor, Select All,
//! and the paths Cut, Copy, Delete and drag act on.

use super::*;

impl FileTree {
    /// The node a pointer at screen row `y` meets: the rows as the last
    /// frame drew them (#2751), which the screen shows until the next one.
    pub fn node_at_y(&self, y: u16) -> Option<usize> {
        self.node_at_y_from(y, self.last_scroll)
    }

    /// The node the next frame draws at screen row `y`, for what that frame
    /// shows beside it, as a menu opened from the keyboard.
    pub fn node_at_y_next_frame(&self, y: u16) -> Option<usize> {
        self.node_at_y_from(y, self.scroll)
    }

    /// Screen rows map straight onto consecutive node indices from `scroll`.
    fn node_at_y_from(&self, y: u16, scroll: usize) -> Option<usize> {
        if y < self.last_inner.y || y >= self.last_inner.y + self.last_inner.height {
            return None;
        }
        let row = (y - self.last_inner.y) as usize;
        let idx = scroll + row;
        (idx < self.nodes.len()).then_some(idx)
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.nodes.len() {
            self.selected = idx;
        }
    }

    /// Single-select: clear the multi-selection, set both selected and the
    /// shift-anchor to `idx`.
    pub fn select_replace(&mut self, idx: usize) {
        if idx >= self.nodes.len() {
            return;
        }
        self.selected = idx;
        self.anchor = idx;
        self.marked.clear();
    }

    /// Toggle whether the node at `idx` is in the multi-selection. Moves
    /// the cursor to `idx` and resets the shift-anchor to it (matching VS
    /// Code's Cmd/Ctrl+click behaviour). When entering multi-select mode
    /// from a single selection, the previously-focused path is also added
    /// to the marked set so the original cursor row is not silently lost.
    pub fn toggle_mark(&mut self, idx: usize) {
        if idx >= self.nodes.len() {
            return;
        }
        let entering_multi = self.marked.is_empty();
        if entering_multi
            && let Some(prev) = self.nodes.get(self.selected).map(|n| n.path.clone())
            && self.selected != idx
        {
            self.marked.insert(prev);
        }
        let path = self.nodes[idx].path.clone();
        if !self.marked.remove(&path) {
            self.marked.insert(path);
        }
        self.selected = idx;
        self.anchor = idx;
    }

    /// Replace the multi-selection with the inclusive range between the
    /// current shift-anchor and `idx`. Moves the cursor to `idx`.
    pub fn extend_to(&mut self, idx: usize) {
        if idx >= self.nodes.len() {
            return;
        }
        let anchor = self.anchor.min(self.nodes.len().saturating_sub(1));
        let (lo, hi) = if anchor <= idx {
            (anchor, idx)
        } else {
            (idx, anchor)
        };
        self.marked.clear();
        for i in lo..=hi {
            self.marked.insert(self.nodes[i].path.clone());
        }
        self.selected = idx;
    }

    pub fn clear_marks(&mut self) {
        self.marked.clear();
        self.anchor = self.selected;
    }

    pub fn is_marked(&self, idx: usize) -> bool {
        self.nodes
            .get(idx)
            .is_some_and(|n| self.marked.contains(&n.path))
    }

    /// Mark every visible node (root included). Anchor goes to 0, selected
    /// stays where it is so the user's caret doesn't jump.
    pub fn select_all_visible(&mut self) {
        self.marked.clear();
        for n in &self.nodes {
            self.marked.insert(n.path.clone());
        }
        self.anchor = 0;
    }

    /// Paths that should be acted on by Cut/Copy/Delete/Drag. If the
    /// multi-selection is non-empty, returns the marked paths in tree
    /// order plus the cursor row (so a Cmd-toggled row never excludes the
    /// originally-focused row). Otherwise returns just the cursor path.
    pub fn action_paths(&self) -> Vec<PathBuf> {
        let Some(focused) = self.nodes.get(self.selected).map(|n| n.path.clone()) else {
            return Vec::new();
        };
        if self.marked.is_empty() {
            return vec![focused];
        }
        let mut paths: Vec<PathBuf> = self
            .nodes
            .iter()
            .filter(|n| self.marked.contains(&n.path) || n.path == focused)
            .map(|n| n.path.clone())
            .collect();
        paths.dedup();
        paths
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.nodes.get(self.selected).map(|n| n.path.as_path())
    }

    /// Drop multi-selection entries whose paths no longer exist in the
    /// flattened node list (e.g. their parent folder was collapsed). Marks
    /// always reference visible rows so a stale path can't accidentally
    /// participate in Cut/Copy/Drag.
    pub(super) fn prune_marks(&mut self) {
        if self.marked.is_empty() {
            return;
        }
        let visible: BTreeSet<PathBuf> = self.nodes.iter().map(|n| n.path.clone()).collect();
        self.marked.retain(|p| visible.contains(p));
    }
}
