use crate::icons;
use crate::widgets::scrollbar;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Node {
    pub path: PathBuf,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub loaded: bool,
}

pub struct FileTree {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    pub selected: usize,
    pub scroll: usize,
    pub focused: bool,
    /// True under the Black theme. Draws the orange→green gradient border
    /// (when focused) instead of the solid blue one, and fills the selected
    /// row with the brand's dark-teal instead of the legacy blue. Set by the
    /// app's focus/theme sync.
    pub focus_gradient: bool,
    /// Active color theme. Drives the scrollbar track/thumb colors (which
    /// pre-blend against the theme background). Set by the app's theme sync.
    pub theme: crate::theme::Theme,
    /// Git-ignored paths (absolute; fully-ignored dirs collapsed to one
    /// entry), fed from the git worker's status refresh. Rows whose path is
    /// in the set — or under a dir in the set — render their name in the
    /// theme's dimmed foreground, VS Code's ignored-resource decoration.
    pub ignored: Arc<HashSet<PathBuf>>,
    pub last_inner: Rect,
    pub last_area: Rect,
    pub last_scrollbar: Rect,
    /// The sticky band painted this frame: `(screen y, node index)` per
    /// pinned ancestor row. Cleared at render start so the hit test always
    /// describes the painted frame (#103's invariant).
    sticky_rows: Vec<(u16, usize)>,
    pub anchor: usize,
    pub marked: BTreeSet<PathBuf>,
    /// While the user is mid-drag, the index of the directory row currently
    /// under the pointer (or the parent dir of a hovered file). Drawn with
    /// a highlighted bg so the drop target is unambiguous. Cleared on drop
    /// or cancel.
    pub drag_target: Option<usize>,
    /// Render-time pointer cell, fed from `App::pointer_cell` each frame so a
    /// row under the cursor can lift to the hover background. `None` when the
    /// pointer is outside the panel.
    pub hover_pointer: Option<(u16, u16)>,
    /// Hit-test rects for the Explorer header toolbar buttons, captured each
    /// render on the panel's top border row: VS Code's New File / New Folder /
    /// Refresh / Collapse Folders affordances. `Rect::default()` (zero width)
    /// when the panel is too narrow to paint them.
    pub header_new_file_btn: Rect,
    pub header_new_folder_btn: Rect,
    pub header_refresh_btn: Rect,
    pub header_collapse_btn: Rect,
    /// Hit-test rect for the "Views and More Actions" (⋯) button on the
    /// EXPLORER title line, which opens the menu that toggles the Explorer's
    /// stacked sub-views (Open Editors / Folders / Outline / Timeline / Rust
    /// Dependencies). `Rect::default()` (zero width) when the panel is too
    /// narrow to paint it.
    pub header_views_btn: Rect,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root: root.clone(),
            nodes: vec![Node {
                path: root,
                depth: 0,
                is_dir: true,
                expanded: true,
                loaded: false,
            }],
            selected: 0,
            scroll: 0,
            focused: true,
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            ignored: Arc::default(),
            last_inner: Rect::default(),
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            sticky_rows: Vec::new(),
            anchor: 0,
            marked: BTreeSet::new(),
            drag_target: None,
            hover_pointer: None,
            header_new_file_btn: Rect::default(),
            header_new_folder_btn: Rect::default(),
            header_refresh_btn: Rect::default(),
            header_collapse_btn: Rect::default(),
            header_views_btn: Rect::default(),
        };
        tree.load_children(0);
        tree
    }

    /// Replace the workspace root with `new_root` and reload from scratch.
    /// Drops the existing selection, scroll, marks, and drag state - the
    /// new tree is a fresh tree rooted at the supplied directory.
    pub fn set_root(&mut self, new_root: PathBuf) {
        self.root = new_root.clone();
        self.nodes = vec![Node {
            path: new_root,
            depth: 0,
            is_dir: true,
            expanded: true,
            loaded: false,
        }];
        self.selected = 0;
        self.scroll = 0;
        self.anchor = 0;
        self.marked.clear();
        self.drag_target = None;
        // The old workspace's ignore set is meaningless under the new root;
        // the git worker re-queries after its SetRoot and repopulates.
        self.ignored = Arc::default();
        self.load_children(0);
    }

    /// Append `path` as an additional workspace root (multi-root Phase 1b,
    /// #145): a new depth-0 section row at the end of the flattened list,
    /// expanded and loaded like the primary root. `self.root` stays the
    /// PRIMARY root — the launch identity — and `set_root` still collapses
    /// the tree back to a single root (a re-root changes what the window
    /// is, not the folder list). A path already present as a root is
    /// ignored rather than duplicated.
    pub fn add_root(&mut self, path: PathBuf) {
        if self.root_paths().any(|r| r == path) {
            return;
        }
        self.nodes.push(Node {
            path,
            depth: 0,
            is_dir: true,
            expanded: true,
            loaded: false,
        });
        let idx = self.nodes.len() - 1;
        self.load_children(idx);
    }

    /// Remove a non-primary root section (Remove Folder from Workspace,
    /// #147): the depth-0 row for `path` and its whole flattened subtree
    /// (every following row until the next depth-0 row) leave the list.
    /// The PRIMARY root (row 0) is refused — that is `set_root`'s job.
    /// Selection, anchor, and scroll clamp back into range; marks under
    /// the removed root are dropped. Returns whether a section was removed.
    pub fn remove_root(&mut self, path: &Path) -> bool {
        let Some(start) = self
            .nodes
            .iter()
            .position(|n| n.depth == 0 && n.path == path)
        else {
            return false;
        };
        if start == 0 {
            return false;
        }
        let end = self.nodes[start + 1..]
            .iter()
            .position(|n| n.depth == 0)
            .map(|off| start + 1 + off)
            .unwrap_or(self.nodes.len());
        self.nodes.drain(start..end);
        // Prune marks by VISIBILITY, not path prefix: a nested workspace
        // root keeps its own depth-0 section when an ancestor root is
        // removed, and its rows' marks must survive (#148 review).
        self.prune_marks();
        self.selected = self.selected.min(self.nodes.len().saturating_sub(1));
        self.anchor = self.anchor.min(self.nodes.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.nodes.len().saturating_sub(1));
        true
    }

    /// Every workspace root in display order: the depth-0 section rows.
    /// The first is always the primary root.
    pub fn root_paths(&self) -> impl Iterator<Item = &Path> {
        self.nodes
            .iter()
            .filter(|n| n.depth == 0)
            .map(|n| n.path.as_path())
    }

    /// The root owning `path`, by longest prefix over the tree's own root
    /// rows — the same resolution rule as `WorkspaceRoots::owning_root`,
    /// local so the widget stays app-independent.
    fn owning_root(&self, path: &Path) -> Option<&Path> {
        self.root_paths()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.components().count())
    }

    /// True when `path` names one of the workspace root rows: the ignored-set
    /// ancestor walk and the guards that protect root rows stop here.
    fn is_root_path(&self, path: &Path) -> bool {
        self.root_paths().any(|r| r == path)
    }

    /// True when `path` is git-ignored: either listed in the ignored set
    /// itself or a descendant of an ignored directory (the set stores a
    /// fully-ignored dir as one collapsed entry). Walks ancestors up to the
    /// workspace root, so the cost is O(depth) per visible row.
    pub fn is_ignored(&self, path: &Path) -> bool {
        if self.ignored.is_empty() {
            return false;
        }
        let mut p = path;
        loop {
            if self.ignored.contains(p) {
                return true;
            }
            // Stop at whichever workspace root owns the path (#145): the
            // walk must never escape a root into its parent directories.
            if self.is_root_path(p) {
                return false;
            }
            match p.parent() {
                Some(parent) => p = parent,
                None => return false,
            }
        }
    }

    /// Map a screen y coordinate to a node index, if any. Screen rows map
    /// straight onto consecutive node indices from the scroll offset.
    pub fn node_at_y(&self, y: u16) -> Option<usize> {
        if y < self.last_inner.y || y >= self.last_inner.y + self.last_inner.height {
            return None;
        }
        let row = (y - self.last_inner.y) as usize;
        let idx = self.scroll + row;
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

    fn load_children(&mut self, idx: usize) {
        if self.nodes[idx].loaded {
            return;
        }
        let path = self.nodes[idx].path.clone();
        let depth = self.nodes[idx].depth + 1;
        let mut entries: Vec<(PathBuf, bool)> = std::fs::read_dir(&path)
            .ok()
            .into_iter()
            .flat_map(|rd| rd.filter_map(Result::ok))
            .map(|e| {
                let p = e.path();
                let is_dir = e
                    .file_type()
                    .map(|ft| ft.is_dir())
                    .unwrap_or_else(|_| p.is_dir());
                (p, is_dir)
            })
            .collect();
        entries.sort_by(|a, b| match (a.1, b.1) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.0.file_name().cmp(&b.0.file_name()),
        });
        let new_nodes: Vec<Node> = entries
            .into_iter()
            .map(|(p, is_dir)| Node {
                path: p,
                depth,
                is_dir,
                expanded: false,
                loaded: false,
            })
            .collect();
        let insert_at = idx + 1;
        for (i, n) in new_nodes.into_iter().enumerate() {
            self.nodes.insert(insert_at + i, n);
        }
        self.nodes[idx].loaded = true;
    }

    fn load_children_preserving_expansion(
        &mut self,
        idx: usize,
        expanded_paths: &BTreeSet<PathBuf>,
    ) {
        self.load_children(idx);
        let child_depth = self.nodes[idx].depth + 1;
        let mut child = idx + 1;
        while child < self.nodes.len() {
            if self.nodes[child].depth < child_depth {
                break;
            }
            if self.nodes[child].depth > child_depth {
                child += 1;
                continue;
            }
            if self.nodes[child].is_dir && expanded_paths.contains(&self.nodes[child].path) {
                self.nodes[child].expanded = true;
                self.load_children_preserving_expansion(child, expanded_paths);
            }
            child += 1;
            while child < self.nodes.len() && self.nodes[child].depth > child_depth {
                child += 1;
            }
        }
    }

    fn collapse(&mut self, idx: usize) {
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

    fn scroll_to(&mut self, top: usize) {
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

    /// Indices into `self.nodes` painted in tree order — every node, in order,
    /// since the Explorer no longer filters its rows. Kept as a method so the
    /// render's scroll machinery can keep mapping a node index to its on-screen
    /// position uniformly.
    pub fn visible_indices(&self) -> Vec<usize> {
        (0..self.nodes.len()).collect()
    }

    /// Test-only view of the band painted this frame.
    #[cfg(test)]
    pub fn sticky_rows_for_test(&self) -> &[(u16, usize)] {
        &self.sticky_rows
    }

    /// Map a click on the sticky band to the pinned directory's node index.
    pub fn sticky_row_at(&self, y: u16) -> Option<usize> {
        self.sticky_rows
            .iter()
            .find(|&&(ry, _)| ry == y)
            .map(|&(_, idx)| idx)
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.nodes.get(self.selected).map(|n| n.path.as_path())
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
            let Some(idx) = self.nodes.iter().position(|n| n.path == current_path) else {
                return false;
            };
            if !is_last && self.nodes[idx].is_dir && !self.nodes[idx].expanded {
                self.nodes[idx].expanded = true;
                self.load_children(idx);
            }
        }
        let Some(final_idx) = self.nodes.iter().position(|n| n.path == target) else {
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
            let name = self.nodes[idx]
                .path
                .file_name()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if name.starts_with(&needle) {
                return Some(idx);
            }
        }
        None
    }

    /// Reload the children of the directory node at `idx`. Preserves the
    /// node's existing `expanded` state — an FS event arriving inside a
    /// folder the user just collapsed must not pop it back open. When the
    /// node is collapsed we just drop its stale children and mark it
    /// unloaded so reopening reloads fresh, skipping the directory walk
    /// entirely.
    pub fn refresh_children(&mut self, idx: usize) {
        if idx >= self.nodes.len() || !self.nodes[idx].is_dir {
            return;
        }
        let depth = self.nodes[idx].depth;
        let mut end = idx + 1;
        while end < self.nodes.len() && self.nodes[end].depth > depth {
            end += 1;
        }
        let expanded_paths: BTreeSet<PathBuf> = self.nodes[(idx + 1)..end]
            .iter()
            .filter(|n| n.is_dir && n.expanded)
            .map(|n| n.path.clone())
            .collect();
        let selected_path = self.selected_path().map(Path::to_path_buf);
        self.nodes.drain((idx + 1)..end);
        self.nodes[idx].loaded = false;
        if self.nodes[idx].expanded {
            self.load_children_preserving_expansion(idx, &expanded_paths);
        }
        if let Some(path) = selected_path
            && let Some(new_idx) = self.nodes.iter().position(|n| n.path == path)
        {
            self.selected = new_idx;
            self.prune_marks();
            if self.anchor >= self.nodes.len() {
                self.anchor = self.selected;
            }
            return;
        }
        if self.selected >= self.nodes.len() {
            self.selected = self.nodes.len().saturating_sub(1);
        }
        if self.anchor >= self.nodes.len() {
            self.anchor = self.nodes.len().saturating_sub(1);
        }
        self.prune_marks();
    }

    /// Drop multi-selection entries whose paths no longer exist in the
    /// flattened node list (e.g. their parent folder was collapsed). Marks
    /// always reference visible rows so a stale path can't accidentally
    /// participate in Cut/Copy/Drag.
    fn prune_marks(&mut self) {
        if self.marked.is_empty() {
            return;
        }
        let visible: BTreeSet<PathBuf> = self.nodes.iter().map(|n| n.path.clone()).collect();
        self.marked.retain(|p| visible.contains(p));
    }

    /// Index of the directory node whose path equals `path`, comparing both
    /// the raw stored path and its canonicalized form so this is robust to
    /// `/tmp` vs `/private/tmp` and similar symlink resolution differences.
    pub fn index_of_dir(&self, path: &Path) -> Option<usize> {
        let canon_target = path.canonicalize().ok();
        self.nodes.iter().position(|n| {
            if !n.is_dir {
                return false;
            }
            if n.path == path {
                return true;
            }
            if let Some(target) = canon_target.as_ref()
                && let Ok(canon_node) = n.path.canonicalize()
                && &canon_node == target
            {
                return true;
            }
            false
        })
    }
}

/// Decide where a "New File" / "New Folder" should be created relative to the
/// node the user right-clicked on.
///
/// * Right-click on a directory → create *inside* that directory.
/// * Right-click on a file       → create as a *sibling* (in the file's parent).
/// * Right-click on empty space  → create in the workspace root.
pub fn create_target_dir_for(node: Option<&Node>, root: &Path) -> PathBuf {
    let Some(node) = node else {
        return root.to_path_buf();
    };
    if node.is_dir {
        node.path.clone()
    } else {
        node.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf())
    }
}

/// Validate a name typed into the New File / New Folder prompt.
/// Returns Ok(()) if it can become a single child entry safely.
pub fn validate_new_name(name: &str) -> Result<(), &'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("name cannot be empty");
    }
    if trimmed == "." || trimmed == ".." {
        return Err("name cannot be '.' or '..'");
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err("name cannot contain slashes");
    }
    if trimmed.contains('\0') {
        return Err("name cannot contain NUL");
    }
    Ok(())
}

/// Create an empty file `name` inside `parent`. Errors if it already exists.
pub fn create_file_in(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    let target = parent.join(name);
    if target.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", target.display()),
        ));
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)?;
    Ok(target)
}

/// Decide whether a given node may be deleted via the explorer's right-click
/// menu, returning the path to delete. Refuses for the workspace root and
/// returns `None` if `node` is None (i.e. right-click on empty tree space).
pub fn delete_target_for(node: Option<&Node>, root: &Path) -> Option<PathBuf> {
    let n = node?;
    // Depth 0 is the root-row marker: with multiple workspace roots
    // (#145) every section row is protected, not just the primary.
    if n.depth == 0 || n.path == root {
        return None;
    }
    let canon_root = root.canonicalize().ok();
    let canon_target = n.path.canonicalize().ok();
    if canon_root.is_some() && canon_target == canon_root {
        return None;
    }
    Some(n.path.clone())
}

/// Move `path` to the OS trash (recoverable) rather than unlinking it.
pub fn move_to_trash(path: &Path) -> std::io::Result<()> {
    #[cfg(not(target_os = "android"))]
    {
        trash::delete(path).map_err(|e| std::io::Error::other(format!("{e}")))
    }
    #[cfg(target_os = "android")]
    {
        android_trash::trash_one(path)
    }
}

/// Bulk-trash a batch of paths in a single OS call. On macOS this routes
/// through Finder via one AppleScript so the system trash sound plays once
/// for the whole batch instead of once per file. On Linux/Windows the
/// underlying `trash::delete_all` already groups the request. Android has no
/// OS trash, so each entry is moved into the freedesktop home trash directly.
pub fn move_to_trash_bulk(paths: &[PathBuf]) -> std::io::Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::Finder);
        ctx.delete_all(paths)
            .map_err(|e| std::io::Error::other(format!("{e}")))
    }
    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    {
        trash::delete_all(paths).map_err(|e| std::io::Error::other(format!("{e}")))
    }
    #[cfg(target_os = "android")]
    {
        for p in paths {
            android_trash::trash_one(p)?;
        }
        Ok(())
    }
}

/// Freedesktop.org home-trash implementation for android, where the `trash`
/// crate has no backend (its freedesktop backend is gated out with
/// `not(target_os = "android")`). Moving an entry into
/// `$XDG_DATA_HOME/Trash/files/<name>` plus an `info/<name>.trashinfo` record
/// preserves the same recoverable semantics croft has on macOS/Linux instead
/// of permanently unlinking the file. See the freedesktop Trash spec v1.0.
#[cfg(target_os = "android")]
mod android_trash {
    use std::ffi::OsStr;
    use std::fs;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    pub fn trash_one(path: &Path) -> io::Result<()> {
        let trash = trash_dir()?;
        let files = trash.join("files");
        let info = trash.join("info");
        fs::create_dir_all(&files)?;
        fs::create_dir_all(&info)?;

        let base = path
            .file_name()
            .ok_or_else(|| io::Error::other("path has no file name"))?;
        let name = unique_name(&files, &info, base);

        // Spec: write the .trashinfo entry before moving the file so a reader
        // never sees a trashed file whose origin has not been recorded yet.
        let info_path = info.join(format!("{name}.trashinfo"));
        let origin = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut entry = fs::File::create(&info_path)?;
        write!(
            entry,
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            percent_encode(origin.as_os_str().as_encoded_bytes()),
            deletion_date()
        )?;

        let dest = files.join(&name);
        if let Err(e) = move_entry(path, &dest) {
            // The move failed: drop the info record so we never leak a
            // .trashinfo pointing at a file that was never trashed.
            let _ = fs::remove_file(&info_path);
            return Err(e);
        }
        Ok(())
    }

    fn trash_dir() -> io::Result<PathBuf> {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
            && !xdg.is_empty()
        {
            return Ok(PathBuf::from(xdg).join("Trash"));
        }
        let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
        Ok(PathBuf::from(home).join(".local/share/Trash"))
    }

    /// Pick a name unused in both `files/` and `info/`, suffixing `.1`, `.2`,
    /// ... on collision so two same-named deletions never clobber each other.
    fn unique_name(files: &Path, info: &Path, base: &OsStr) -> String {
        let base = base.to_string_lossy();
        let taken = |name: &str| {
            files.join(name).exists() || info.join(format!("{name}.trashinfo")).exists()
        };
        if !taken(&base) {
            return base.into_owned();
        }
        let mut n = 1u64;
        loop {
            let candidate = format!("{base}.{n}");
            if !taken(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Percent-encode per the spec (RFC 2396): keep unreserved bytes and the
    /// `/` separators literal, escape everything else as `%XX`.
    fn percent_encode(bytes: &[u8]) -> String {
        const UNRESERVED: &[u8] = b"-_.!~*'()";
        let mut out = String::with_capacity(bytes.len());
        for &b in bytes {
            if b.is_ascii_alphanumeric() || UNRESERVED.contains(&b) || b == b'/' {
                out.push(b as char);
            } else {
                out.push('%');
                out.push(hex(b >> 4));
                out.push(hex(b & 0x0f));
            }
        }
        out
    }

    fn hex(nibble: u8) -> char {
        match nibble {
            0..=9 => (b'0' + nibble) as char,
            _ => (b'A' + (nibble - 10)) as char,
        }
    }

    /// Local-time `YYYY-MM-DDThh:mm:ss` via libc, already a croft dependency,
    /// so no date crate is pulled in for one android-only field.
    fn deletion_date() -> String {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0) as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
            return String::new();
        }
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }

    /// Rename when possible; fall back to recursive copy+remove across a
    /// filesystem boundary (EXDEV), e.g. trashing from /sdcard into $HOME.
    fn move_entry(src: &Path, dst: &Path) -> io::Result<()> {
        match fs::rename(src, dst) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                copy_recursive(src, dst)?;
                remove_recursive(src)
            }
            Err(e) => Err(e),
        }
    }

    fn copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
        if fs::symlink_metadata(src)?.is_dir() {
            fs::create_dir_all(dst)?;
            for child in fs::read_dir(src)? {
                let child = child?;
                copy_recursive(&child.path(), &dst.join(child.file_name()))?;
            }
            Ok(())
        } else {
            fs::copy(src, dst).map(|_| ())
        }
    }

    fn remove_recursive(src: &Path) -> io::Result<()> {
        if fs::symlink_metadata(src)?.is_dir() {
            fs::remove_dir_all(src)
        } else {
            fs::remove_file(src)
        }
    }
}

/// Rename the entry at `old_path` to `new_name` within `parent`. Validates
/// `new_name` via `validate_new_name`, refuses to overwrite an existing
/// entry, and returns the new absolute path on success. A no-op rename
/// (same name) returns Ok with the original path unchanged so the user
/// can hit Enter on the prompt without typing.
pub fn rename_in(parent: &Path, old_path: &Path, new_name: &str) -> std::io::Result<PathBuf> {
    let trimmed = new_name.trim();
    if let Err(msg) = validate_new_name(trimmed) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg));
    }
    let target = parent.join(trimmed);
    if target == old_path {
        return Ok(target);
    }
    if target.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", target.display()),
        ));
    }
    std::fs::rename(old_path, &target)?;
    Ok(target)
}

/// Given a filesystem event path and the tree's workspace root, return the
/// directory whose children should be refreshed.
///
/// * If `event_path` is the root or outside the root, returns `None`.
/// * Otherwise returns the parent directory of `event_path`. The watcher
///   layer is the one that decides whether the event is a create / remove /
///   rename; this helper is only concerned with which subtree to invalidate.
pub fn affected_dir_for_event(event_path: &Path, root: &Path) -> Option<PathBuf> {
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canon_event = event_path
        .canonicalize()
        .unwrap_or_else(|_| event_path.to_path_buf());
    if !canon_event.starts_with(&canon_root) {
        return None;
    }
    if canon_event == canon_root {
        return None;
    }
    canon_event.parent().map(Path::to_path_buf)
}

/// Suggest a non-colliding destination path for `source` placed inside
/// `dest_dir`. If a sibling with the source's basename already exists,
/// appends ` copy`, ` copy 2`, … until a free name is found, mirroring how
/// macOS Finder de-duplicates pasted names. Returns the resolved path; the
/// caller still has to perform the actual move/copy syscall.
pub fn unique_destination_in(dest_dir: &Path, source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = source.extension().map(|s| s.to_string_lossy().into_owned());
    let original_name = source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut candidate = dest_dir.join(&original_name);
    if !candidate.exists() {
        return candidate;
    }
    for n in 1.. {
        let suffix = if n == 1 {
            String::from(" copy")
        } else {
            format!(" copy {n}")
        };
        let name = match &ext {
            Some(e) if !stem.is_empty() => format!("{stem}{suffix}.{e}"),
            Some(e) => format!("{suffix}.{e}"),
            None => format!("{stem}{suffix}"),
        };
        candidate = dest_dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    candidate
}

/// Reject paste/drop operations that would move a directory into itself or
/// a descendant of itself (which would either error or, worse, produce an
/// infinite-recursion copy on filesystems that follow symlinks).
pub fn is_descendant_or_same(target: &Path, source: &Path) -> bool {
    let canon_target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let canon_source = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    canon_target == canon_source || canon_target.starts_with(&canon_source)
}

/// Move `source` to a fresh path inside `dest_dir`. Falls back to
/// copy-then-remove when the source and destination live on different
/// filesystems and `std::fs::rename` returns `EXDEV`. Returns the final
/// destination path on success.
pub fn move_into(dest_dir: &Path, source: &Path) -> std::io::Result<PathBuf> {
    if is_descendant_or_same(dest_dir, source) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("cannot move {} into itself", source.display()),
        ));
    }
    let dest = unique_destination_in(dest_dir, source);
    if std::fs::rename(source, &dest).is_ok() {
        return Ok(dest);
    }
    copy_recursive(source, &dest)?;
    remove_recursive(source)?;
    Ok(dest)
}

/// Recursively copy `source` to `dest`. `dest` must not already exist.
pub fn copy_into(dest_dir: &Path, source: &Path) -> std::io::Result<PathBuf> {
    if is_descendant_or_same(dest_dir, source) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("cannot copy {} into itself", source.display()),
        ));
    }
    let dest = unique_destination_in(dest_dir, source);
    copy_recursive(source, &dest)?;
    Ok(dest)
}

fn copy_recursive(source: &Path, dest: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(source)?;
    let ft = meta.file_type();
    if ft.is_dir() {
        std::fs::create_dir(dest)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let from = entry.path();
            let to = dest.join(entry.file_name());
            copy_recursive(&from, &to)?;
        }
        Ok(())
    } else if ft.is_symlink() {
        let link_target = std::fs::read_link(source)?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&link_target, dest)?;
        }
        #[cfg(not(unix))]
        {
            let _ = link_target;
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "symlink copy not supported on this platform",
            ));
        }
        Ok(())
    } else {
        std::fs::copy(source, dest).map(|_| ())
    }
}

fn remove_recursive(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Case-insensitive subsequence (fuzzy) match of `query` against `haystack`,
/// mirroring the VS Code tree filter, which accepts a hit when the query's
/// characters appear in order (not necessarily contiguously). Returns the byte
/// offsets in `haystack` of the matched characters, in order, so the renderer
/// can bold the matched run; `None` when the query is not a subsequence.
///
/// An empty query matches everything with no highlighted offsets.
pub fn subsequence_match(haystack: &str, query: &str) -> Option<Vec<usize>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let mut needles = query.chars().flat_map(char::to_lowercase).peekable();
    let mut offsets = Vec::new();
    for (byte_idx, hc) in haystack.char_indices() {
        let Some(&want) = needles.peek() else { break };
        if hc.to_lowercase().eq([want]) {
            offsets.push(byte_idx);
            needles.next();
        }
    }
    needles.peek().is_none().then_some(offsets)
}

/// Foreground for the matched characters of a filtered row name: VS Code's
/// `list.filterMatchForeground` amber, which reads as a clear "this is why this
/// row survived the filter" cue on both the dark and black themes.
const FILTER_MATCH_FG: Color = Color::Rgb(0xe5, 0xa3, 0x4b);

/// Push the file name onto `spans`, bolding the characters that the active
/// filter `query` matched (per [`subsequence_match`]). With an empty query the
/// whole name is pushed as a single span in `base` style, so the unfiltered
/// path is unchanged.
fn push_name_spans(spans: &mut Vec<Span<'static>>, name: &str, query: &str, base: Style) {
    let Some(offsets) = (!query.is_empty())
        .then(|| subsequence_match(name, query))
        .flatten()
    else {
        spans.push(Span::styled(name.to_string(), base));
        return;
    };
    let match_style = base.fg(FILTER_MATCH_FG).add_modifier(Modifier::BOLD);
    let mut cursor = 0usize;
    for &off in &offsets {
        if off > cursor {
            spans.push(Span::styled(name[cursor..off].to_string(), base));
        }
        // The matched character runs from its byte offset to the next char
        // boundary; index by the char's own UTF-8 length.
        let ch_len = name[off..].chars().next().map_or(0, char::len_utf8);
        let next = off + ch_len;
        spans.push(Span::styled(name[off..next].to_string(), match_style));
        cursor = next;
    }
    if cursor < name.len() {
        spans.push(Span::styled(name[cursor..].to_string(), base));
    }
}

/// Create directory `name` inside `parent`. Errors if it already exists.
pub fn create_folder_in(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    let target = parent.join(name);
    if target.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", target.display()),
        ));
    }
    std::fs::create_dir(&target)?;
    Ok(target)
}

impl Widget for &mut FileTree {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        // Black theme: chipless brand header (panelTitle.activeForeground);
        // Croft Dark keeps the historical white-on-navy chip.
        let title = if self.focus_gradient {
            Span::styled(
                " EXPLORER ",
                Style::default()
                    .fg(crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                " EXPLORER ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                    .add_modifier(Modifier::BOLD),
            )
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style)
            .title(title.clone());
        let inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the orange→green
        // gradient (matching the welcome activity box), then re-stamp the
        // title the gradient top edge just overwrote.
        if self.focused && self.focus_gradient {
            crate::gradient::paint_gradient_box(buf, area);
            buf.set_span(area.x + 1, area.y, &title, title.width() as u16);
        }
        // The New File / New Folder / Refresh / Collapse toolbar is painted
        // later, on the root folder row. The "Views and More Actions" (⋯)
        // button sits here on the EXPLORER title line, mirroring VS Code's
        // view-container header. Reset the hit-test rects each frame so a
        // hidden affordance never registers stale clicks.
        self.header_new_file_btn = Rect::default();
        self.header_new_folder_btn = Rect::default();
        self.header_refresh_btn = Rect::default();
        self.header_collapse_btn = Rect::default();
        self.header_views_btn = Rect::default();
        self.last_area = area;
        self.last_scrollbar = Rect::default();
        self.sticky_rows.clear();

        // Paint the ⋯ button at the right end of the title border row. It is
        // always available (not focus-gated) so the view-toggle menu is one
        // click away even when the tree is unfocused, like VS Code. It wears
        // the shared header-action pill — a space-padded `" ⋯ "` inset one cell
        // from the rounded corner — so it matches the terminal `+` button
        // exactly instead of jamming a naked glyph against the corner.
        if area.width > 12
            && let Some(rect) = crate::widgets::header_pill::render_action(
                buf,
                area.x + area.width - 1,
                area.y,
                crate::widgets::header_pill::MORE_LABEL,
                self.focus_gradient,
                self.hover_pointer,
            )
        {
            self.header_views_btn = rect;
        }

        self.last_inner = inner;

        let visible_height = inner.height as usize;
        if visible_height == 0 {
            return;
        }
        // Every node, in tree order — `visible` holds node indices.
        let visible = self.visible_indices();
        // Position of the selected node within the filtered list, so scroll and
        // highlight track the cursor even when intermediate rows are hidden.
        let sel_pos = visible
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        // Scroll is tracked in filtered-row positions. Re-derive it each frame
        // from `self.scroll` (a node index) by mapping to a position, then keep
        // the selection on screen.
        let mut scroll_pos = visible.iter().position(|&i| i >= self.scroll).unwrap_or(0);
        if sel_pos < scroll_pos {
            scroll_pos = sel_pos;
        } else if sel_pos >= scroll_pos + visible_height {
            scroll_pos = sel_pos + 1 - visible_height;
        }
        // Persist the node index at the top of the viewport so the next frame's
        // mapping is stable.
        self.scroll = visible.get(scroll_pos).copied().unwrap_or(0);
        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: inner.y,
            width: u16::from(inner.width > 0),
            height: inner.height,
        };
        let scrollbar_metrics =
            scrollbar::vertical_metrics(scrollbar_area, visible.len(), visible_height, scroll_pos);
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }
        let row_width = inner
            .width
            .saturating_sub(u16::from(scrollbar_metrics.is_some()));

        let pointer = self.hover_pointer;
        let brand = self.focus_gradient;
        // No Explorer filter any more, so no characters are ever highlighted.
        let query = String::new();
        let end = (scroll_pos + visible_height).min(visible.len());
        for (row, &idx) in visible[scroll_pos..end].iter().enumerate() {
            let node = &self.nodes[idx];
            let is_selected = idx == self.selected;
            let is_marked = self.marked.contains(&node.path);
            let is_drop_target = self.drag_target == Some(idx);
            let y = inner.y + row as u16;

            let indent = "  ".repeat(node.depth);
            let mut spans: Vec<Span> = Vec::with_capacity(6);
            spans.push(Span::raw(indent));

            let name = node
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| node.path.display().to_string());

            // Git-ignored rows dim the *name* only — icons keep their color,
            // matching VS Code's ignored-resource decoration.
            let name_fg = if self.is_ignored(&node.path) {
                self.theme.ignored_fg()
            } else {
                Color::White
            };

            if node.is_dir {
                let chev = if node.expanded {
                    icons::CHEVRON_OPEN
                } else {
                    icons::CHEVRON_CLOSED
                };
                let icon = if node.expanded {
                    icons::FOLDER_OPEN
                } else {
                    icons::FOLDER_CLOSED
                };
                spans.push(Span::styled(
                    format!("{chev} "),
                    Style::default().fg(Color::Gray),
                ));
                spans.push(Span::styled(
                    format!("{} ", icon.glyph),
                    Style::default().fg(icon.color),
                ));
                let base = Style::default().fg(name_fg).add_modifier(Modifier::BOLD);
                push_name_spans(&mut spans, &name, &query, base);
            } else {
                let suffix = node
                    .path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let icon = icons::for_path(&name, &suffix);
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("{} ", icon.glyph),
                    Style::default().fg(icon.color),
                ));
                push_name_spans(&mut spans, &name, &query, Style::default().fg(name_fg));
            }

            let line = Line::from(spans);
            // Black theme: the selected/marked rows wear the brand's muted
            // dark-teal fill (identical to the right-click menu's selection),
            // replacing the legacy bright-blue accent. Croft Dark keeps the
            // historical blue. The drop target stays green either way.
            let (sel_bg, mark_bg) = if self.focus_gradient {
                (
                    crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG),
                    Color::Rgb(0x18, 0x35, 0x32),
                )
            } else {
                (Color::Rgb(0x09, 0x4d, 0x77), Color::Rgb(0x07, 0x33, 0x55))
            };
            let row_rect = Rect {
                x: inner.x,
                y,
                width: row_width,
                height: 1,
            };
            let line_style = if is_drop_target {
                Style::default().bg(Color::Rgb(0x2c, 0x60, 0x2e))
            } else if is_selected {
                Style::default().bg(sel_bg)
            } else if is_marked {
                Style::default().bg(mark_bg)
            } else if let Some(bg) = crate::widgets::hover::row_hover_bg(row_rect, pointer, brand) {
                Style::default().bg(bg)
            } else {
                Style::default()
            };
            buf.set_style(row_rect, line_style);
            buf.set_line(inner.x, y, &line, row_width);
        }
        // Explorer header toolbar: New File / New Folder / Refresh / Collapse
        // Folders, right-aligned on the root folder row (the "croft" header),
        // mirroring VS Code's view actions, which live on the workspace-folder
        // header rather than the EXPLORER title. Revealed only while the
        // Explorer pane is focused and the root row sits at the top of the
        // viewport; hidden otherwise (VS Code's hover/focus reveal). Painted
        // after the rows so the pills win over the root row's text/fill.
        if self.focused && self.scroll == 0 && !self.nodes.is_empty() {
            use crate::widgets::header_pill;
            const GLYPHS: [char; 4] = [
                header_pill::NEW_FILE_GLYPH,
                header_pill::NEW_FOLDER_GLYPH,
                header_pill::REFRESH_GLYPH,
                header_pill::COLLAPSE_ALL_GLYPH,
            ];
            // Each pill is a single-cell glyph; `step` adds a one-cell gap so
            // the chips read as four distinct buttons. `right_pad` keeps the
            // block clear of the scrollbar / right border.
            let step: u16 = 2;
            let count = GLYPHS.len() as u16;
            let block_w = count * step - 1;
            let right_pad: u16 = 1;
            // Approximate the root label footprint (chevron + space + folder
            // icon + space + name) so a narrow panel withholds the toolbar
            // rather than overprinting the folder name.
            let root_name = self.nodes[0]
                .path
                .file_name()
                .map(|n| n.to_string_lossy().chars().count())
                .unwrap_or(0) as u16;
            let label_w = 4 + root_name;
            if row_width > label_w + block_w + right_pad + 2 {
                let start_x = inner.x + row_width - right_pad - block_w;
                let y = inner.y;
                let brand = self.focus_gradient;
                for (i, &glyph) in GLYPHS.iter().enumerate() {
                    let x = start_x + i as u16 * step;
                    let rect = Rect {
                        x,
                        y,
                        width: step,
                        height: 1,
                    };
                    let hovered = crate::widgets::hover::contains(rect, self.hover_pointer);
                    header_pill::render(buf, x, y, glyph, brand, hovered);
                    match i {
                        0 => self.header_new_file_btn = rect,
                        1 => self.header_new_folder_btn = rect,
                        2 => self.header_refresh_btn = rect,
                        _ => self.header_collapse_btn = rect,
                    }
                }
            }
        }
        // Sticky band (#117): pin the off-screen ancestor directories of the
        // top visible row, overpainting the topmost content rows — the
        // editor sticky band's model, including its guard: never cover the
        // selected row (the scroll clamp above keeps it on screen, so its
        // band-relative row bounds the band's height).
        if scroll_pos > 0 {
            const STICKY_MAX: usize = 3;
            let top_idx = visible[scroll_pos];
            let chain = sticky_ancestors(&self.nodes, top_idx, STICKY_MAX);
            let sel_row = sel_pos.saturating_sub(scroll_pos);
            let band = chain
                .len()
                .min(sel_row)
                .min(visible_height.saturating_sub(1));
            let shown = &chain[chain.len() - band..];
            let bg = self.theme.sticky_scroll_bg();
            for (i, &aidx) in shown.iter().enumerate() {
                let y = inner.y + i as u16;
                for x in inner.x..inner.x + row_width {
                    buf[(x, y)].set_symbol(" ");
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
                let node = &self.nodes[aidx];
                let name = node
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| node.path.display().to_string());
                let name_fg = if self.is_ignored(&node.path) {
                    self.theme.ignored_fg()
                } else {
                    Color::White
                };
                let spans = vec![
                    Span::raw("  ".repeat(node.depth)),
                    Span::styled(
                        format!("{} ", icons::CHEVRON_OPEN),
                        Style::default().fg(Color::Gray).bg(bg),
                    ),
                    Span::styled(
                        format!("{} ", icons::FOLDER_OPEN.glyph),
                        Style::default().fg(icons::FOLDER_OPEN.color).bg(bg),
                    ),
                    Span::styled(
                        name,
                        Style::default()
                            .fg(name_fg)
                            .bg(bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                buf.set_line(inner.x, y, &Line::from(spans), row_width);
                self.sticky_rows.push((y, aidx));
            }
        }
        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }
    }
}

/// The ancestor directory chain of `top_idx`, outermost first, capped at
/// `max` keeping the DEEPEST entries (the closest context matters most —
/// VS Code's tree sticky truncates the outer levels the same way). An
/// ancestor is the nearest earlier node with a strictly smaller depth,
/// walked to the root; a top-level node has none.
pub(crate) fn sticky_ancestors(nodes: &[Node], top_idx: usize, max: usize) -> Vec<usize> {
    let Some(top) = nodes.get(top_idx) else {
        return Vec::new();
    };
    let mut chain = Vec::new();
    let mut want_depth = top.depth;
    for i in (0..top_idx).rev() {
        if want_depth == 0 {
            break;
        }
        if nodes[i].depth < want_depth {
            chain.push(i);
            want_depth = nodes[i].depth;
        }
    }
    chain.reverse(); // outermost first
    let skip = chain.len().saturating_sub(max);
    chain.split_off(skip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, FileTree) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("README.md"), "# hi\n").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        let tree = FileTree::new(root.to_path_buf());
        (tmp, tree)
    }

    /// Two-root fixture: the primary from `fixture()` plus a second root
    /// holding `lib/util.rs`, appended via `add_root`.
    fn two_root_fixture() -> (TempDir, TempDir, FileTree) {
        let (tmp, mut tree) = fixture();
        let second = TempDir::new().unwrap();
        fs::create_dir(second.path().join("lib")).unwrap();
        fs::write(second.path().join("lib/util.rs"), "pub fn u() {}\n").unwrap();
        fs::write(second.path().join("Cargo.toml"), "[package]\n").unwrap();
        tree.add_root(second.path().to_path_buf());
        (tmp, second, tree)
    }

    #[test]
    fn add_root_appends_a_loaded_depth_zero_section_and_keeps_the_primary() {
        let (tmp, second, tree) = two_root_fixture();
        assert_eq!(tree.root, tmp.path(), "the primary root is unchanged");
        let roots: Vec<_> = tree.root_paths().collect();
        assert_eq!(roots, vec![tmp.path(), second.path()]);
        let second_idx = tree
            .nodes
            .iter()
            .position(|n| n.path == second.path())
            .expect("the second root is a row");
        assert_eq!(tree.nodes[second_idx].depth, 0);
        assert!(tree.nodes[second_idx].expanded);
        assert!(
            tree.nodes[second_idx + 1..]
                .iter()
                .any(|n| n.path.ends_with("lib")),
            "the second root's children load beneath it"
        );
        // Idempotent: adding an existing root is a no-op, not a duplicate.
        let count = tree.nodes.len();
        let mut tree = tree;
        tree.add_root(second.path().to_path_buf());
        assert_eq!(tree.nodes.len(), count);
    }

    #[test]
    fn remove_root_drops_the_section_and_its_subtree_but_refuses_the_primary() {
        let (tmp, second, mut tree) = two_root_fixture();
        assert!(
            !tree.remove_root(tmp.path()),
            "the primary root is never removable through remove_root"
        );
        let before = tree.nodes.len();
        assert!(tree.remove_root(second.path()));
        assert!(
            tree.nodes.len() < before,
            "the section row and its subtree leave the list"
        );
        assert!(
            !tree.nodes.iter().any(|n| n.path.starts_with(second.path())),
            "no row under the removed root survives"
        );
        assert_eq!(
            tree.root_paths().collect::<Vec<_>>(),
            vec![tmp.path()],
            "back to a single-root tree"
        );
        assert!(tree.selected < tree.nodes.len(), "selection clamps");
        assert!(
            !tree.remove_root(second.path()),
            "removing again is a no-op"
        );
    }

    #[test]
    fn removing_an_ancestor_root_keeps_marks_in_a_surviving_nested_root_section() {
        // A nested workspace root stays its own depth-0 section when its
        // ancestor root is removed; pruning marks by path prefix wrongly
        // swept the survivor's marks too (#148 review).
        let (_tmp, second, mut tree) = two_root_fixture();
        let nested = second.path().join("lib");
        tree.add_root(nested.clone());
        let util_idx = tree
            .nodes
            .iter()
            .rposition(|n| n.path.ends_with("util.rs"))
            .expect("the nested section lists its file");
        tree.toggle_mark(util_idx);
        let marked_path = tree.nodes[util_idx].path.clone();

        assert!(tree.remove_root(second.path()));

        assert!(
            tree.root_paths().any(|r| r == nested),
            "the nested root's own section survives"
        );
        assert!(
            tree.marked.contains(&marked_path),
            "the surviving section's marks survive with it"
        );
    }

    #[test]
    fn collapse_all_keeps_every_root_section_expanded() {
        let (_tmp, second, mut tree) = two_root_fixture();
        // Expand a subfolder in each root first.
        let src_idx = tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("src"))
            .unwrap();
        tree.selected = src_idx;
        tree.expand_selected();
        let lib_idx = tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("lib"))
            .unwrap();
        tree.selected = lib_idx;
        tree.expand_selected();

        tree.collapse_all();

        let second_root = tree
            .nodes
            .iter()
            .find(|n| n.path == second.path())
            .expect("the second root row survives Collapse All");
        assert!(
            second_root.expanded,
            "every ROOT section stays expanded, exactly like the primary"
        );
        assert!(
            tree.nodes
                .iter()
                .filter(|n| n.depth > 0)
                .all(|n| !(n.is_dir && n.expanded)),
            "all non-root folders collapse"
        );
    }

    #[test]
    fn reveal_path_resolves_through_the_owning_root() {
        let (_tmp, second, mut tree) = two_root_fixture();
        let target = second.path().join("lib/util.rs");
        assert!(
            tree.reveal_path(&target),
            "a file under the SECOND root must be revealable"
        );
        assert_eq!(tree.nodes[tree.selected].path, target);
    }

    #[test]
    fn delete_guard_refuses_every_root_row() {
        let (_tmp, second, tree) = two_root_fixture();
        let second_node = tree.nodes.iter().find(|n| n.path == second.path());
        assert_eq!(
            delete_target_for(second_node, &tree.root),
            None,
            "a workspace root row is never a delete target, whichever root it is"
        );
    }

    #[test]
    fn ignored_walk_stops_at_the_owning_root() {
        let (_tmp, second, mut tree) = two_root_fixture();
        // Only the SECOND root's parent dir is in the ignored set — a state
        // that cannot legitimately mark anything INSIDE that root ignored,
        // because the ancestor walk must stop at the owning root before
        // reaching outside it.
        let mut set = std::collections::HashSet::new();
        set.insert(second.path().parent().unwrap().to_path_buf());
        tree.ignored = std::sync::Arc::new(set);
        assert!(
            !tree.is_ignored(&second.path().join("lib/util.rs")),
            "the ancestor walk must stop at the owning root, never escaping into its parents"
        );
    }

    #[test]
    fn focused_gradient_border_draws_rounded_corner_and_keeps_title() {
        use crate::gradient::{GRAD_TL, rgb_color};
        let (_tmp, mut tree) = fixture();
        tree.focused = true;
        tree.focus_gradient = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        (&mut tree).render(area, &mut buf);
        // Rounded top-left corner in the gradient's top-left colour.
        assert_eq!(buf[(0, 0)].symbol(), "\u{256d}");
        assert_eq!(buf[(0, 0)].fg, rgb_color(GRAD_TL));
        // The EXPLORER title must survive the gradient repaint of the top edge.
        let top: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(
            top.contains("EXPLORER"),
            "title clobbered by gradient: {top:?}"
        );
    }

    #[test]
    fn collapse_all_closes_nested_folders_but_keeps_the_root_expanded() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src/inner")).unwrap();
        fs::write(root.join("src/inner/deep.rs"), "//\n").unwrap();
        let mut tree = FileTree::new(root.to_path_buf());
        // Expand every ancestor of the deep file so three depth levels show.
        assert!(
            tree.reveal_path(&root.join("src/inner/deep.rs")),
            "precondition: reveal expands src and src/inner"
        );
        assert!(
            tree.nodes.iter().any(|n| n.path.ends_with("deep.rs")),
            "precondition: the deep file is visible before collapse"
        );

        tree.collapse_all();

        assert!(tree.nodes[0].expanded, "the workspace root stays expanded");
        assert_eq!(tree.nodes[0].depth, 0);
        assert!(
            tree.nodes.iter().skip(1).all(|n| !(n.is_dir && n.expanded)),
            "no subfolder remains expanded"
        );
        assert!(
            !tree.nodes.iter().any(|n| n.path.ends_with("deep.rs")),
            "nested descendants are dropped from the flattened list"
        );
        assert_eq!(tree.selected, 0);
        assert_eq!(tree.scroll, 0);
    }

    #[test]
    fn explorer_header_toolbar_paints_five_pills_and_hit_tests() {
        let (_tmp, mut tree) = fixture();
        tree.focused = true;
        tree.focus_gradient = false; // Croft Dark
        let area = Rect {
            x: 0,
            y: 0,
            width: 44,
            height: 8,
        };
        let mut buf = Buffer::empty(area);
        (&mut tree).render(area, &mut buf);

        // The toolbar lives on the root folder row (inner.y), not the title
        // border, mirroring VS Code's workspace-folder header actions.
        let root_y = tree.last_inner.y;
        let root_row: String = (0..area.width).map(|x| buf[(x, root_y)].symbol()).collect();
        for glyph in [
            crate::widgets::header_pill::NEW_FILE_GLYPH,
            crate::widgets::header_pill::NEW_FOLDER_GLYPH,
            crate::widgets::header_pill::REFRESH_GLYPH,
            crate::widgets::header_pill::COLLAPSE_ALL_GLYPH,
        ] {
            assert!(
                root_row.contains(glyph),
                "root row missing glyph {glyph:?}: {root_row:?}"
            );
        }

        for btn in [
            tree.header_new_file_btn,
            tree.header_new_folder_btn,
            tree.header_refresh_btn,
            tree.header_collapse_btn,
        ] {
            assert!(btn.width > 0, "button rect was not captured");
            assert_eq!(btn.y, root_y, "button sits on the root folder row");
        }
        // Painted left to right in VS Code's order, clear of the right border.
        assert!(tree.header_new_file_btn.x < tree.header_new_folder_btn.x);
        assert!(tree.header_new_folder_btn.x < tree.header_refresh_btn.x);
        assert!(tree.header_refresh_btn.x < tree.header_collapse_btn.x);
        assert!(tree.header_collapse_btn.x + tree.header_collapse_btn.width < area.x + area.width);

        // The ⋯ "Views and More Actions" button sits on the EXPLORER title
        // border row, not the toolbar row — that's the whole point of the
        // rework (the view-toggle affordance lives on the Explorer line).
        assert!(
            tree.header_views_btn.width > 0,
            "views (⋯) button rect was not captured"
        );
        assert_eq!(
            tree.header_views_btn.y, area.y,
            "the ⋯ button sits on the EXPLORER title line"
        );
        let title_row: String = (0..area.width).map(|x| buf[(x, area.y)].symbol()).collect();
        assert!(
            title_row.contains(crate::widgets::header_pill::MORE_LABEL.trim()),
            "title row missing the ⋯ glyph: {title_row:?}"
        );
    }

    #[test]
    fn explorer_header_toolbar_is_withheld_when_the_panel_is_too_narrow() {
        let (_tmp, mut tree) = fixture();
        tree.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 14,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        (&mut tree).render(area, &mut buf);
        assert_eq!(tree.header_new_file_btn.width, 0);
        assert_eq!(tree.header_collapse_btn.width, 0);
    }

    #[test]
    fn explorer_header_toolbar_is_withheld_when_the_panel_is_unfocused() {
        let (_tmp, mut tree) = fixture();
        tree.focused = false;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 8,
        };
        let mut buf = Buffer::empty(area);
        (&mut tree).render(area, &mut buf);
        assert_eq!(tree.header_new_file_btn.width, 0);
        assert_eq!(tree.header_collapse_btn.width, 0);
    }

    #[test]
    fn hovering_an_unselected_row_lifts_it_without_touching_the_selection() {
        let (_tmp, mut tree) = fixture();
        tree.focused = true;
        tree.focus_gradient = false; // Croft Dark
        tree.selected = 0; // the root row stays selected
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 8,
        };
        // Rest the pointer on the second visible row (a child of the root).
        let inner_y = area.y + 1;
        tree.hover_pointer = Some((area.x + 1, inner_y + 1));
        let mut buf = Buffer::empty(area);
        (&mut tree).render(area, &mut buf);
        assert_eq!(
            buf[(area.x + 1, inner_y + 1)].bg,
            Color::Rgb(0x2b, 0x31, 0x42),
            "the hovered child row wears the Croft Dark hover lift"
        );
        assert_eq!(
            buf[(area.x + 1, inner_y)].bg,
            Color::Rgb(0x09, 0x4d, 0x77),
            "the selected row keeps its selection bg, not the hover lift"
        );
    }

    #[test]
    fn a_selected_row_is_not_overridden_by_hovering_it() {
        let (_tmp, mut tree) = fixture();
        tree.focused = true;
        tree.focus_gradient = false;
        tree.selected = 0;
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 8,
        };
        let inner_y = area.y + 1;
        // Hover the selected row itself.
        tree.hover_pointer = Some((area.x + 1, inner_y));
        let mut buf = Buffer::empty(area);
        (&mut tree).render(area, &mut buf);
        assert_eq!(
            buf[(area.x + 1, inner_y)].bg,
            Color::Rgb(0x09, 0x4d, 0x77),
            "selection outranks hover so the row never dims to the hover lift"
        );
    }

    #[test]
    fn croft_dark_keeps_solid_blue_focus_border() {
        let (_tmp, mut tree) = fixture();
        tree.focused = true;
        tree.focus_gradient = false;
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        (&mut tree).render(area, &mut buf);
        // Square corner, solid blue: the historical Croft Dark highlight.
        assert_eq!(buf[(0, 0)].symbol(), "\u{250c}");
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(0x4e, 0x9a, 0xff));
    }

    #[test]
    fn new_lists_root_and_children() {
        let (_tmp, tree) = fixture();
        // Root + 3 children (src/, main.rs, README.md).
        assert_eq!(tree.nodes.len(), 4);
        assert!(tree.nodes[0].is_dir);
        assert!(tree.nodes[0].expanded);
    }

    #[test]
    fn explorer_lists_gitignored_files() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::write(tmp.path().join(".gitignore"), "*.txt\n").unwrap();
        fs::write(tmp.path().join("dummy2.txt"), "").unwrap();
        let tree = FileTree::new(tmp.path().to_path_buf());
        assert!(
            tree.nodes.iter().any(|n| n.path.ends_with("dummy2.txt")),
            "Explorer must show disk reality, including gitignored files"
        );
    }

    #[test]
    fn is_ignored_covers_set_members_and_their_descendants() {
        let (_tmp, mut tree) = fixture();
        let root = tree.root.clone();
        tree.ignored = std::sync::Arc::new(std::collections::HashSet::from([
            root.join("target"),
            root.join("debug.log"),
        ]));
        assert!(tree.is_ignored(&root.join("debug.log")));
        assert!(tree.is_ignored(&root.join("target")));
        assert!(
            tree.is_ignored(&root.join("target/debug/deps/foo.rlib")),
            "descendants of an ignored dir are ignored too"
        );
        assert!(!tree.is_ignored(&root.join("main.rs")));
        assert!(!tree.is_ignored(&root));
    }

    #[test]
    fn ignored_rows_render_with_the_dimmed_foreground() {
        let (_tmp, mut tree) = fixture();
        let root = tree.root.clone();
        tree.ignored =
            std::sync::Arc::new(std::collections::HashSet::from([root.join("README.md")]));
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut tree).render(area, &mut buf);
        let dim = tree.theme.ignored_fg();
        // Multi-byte icon glyphs mean a byte offset into the joined row text
        // is not a column; walk the cells to map the match back to its column.
        let row_fg = |needle: &str| {
            for y in 0..area.height {
                let cells: Vec<String> = (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect();
                let joined = cells.concat();
                let Some(target) = joined.find(needle) else {
                    continue;
                };
                let mut acc = 0usize;
                for (x, s) in cells.iter().enumerate() {
                    if acc == target {
                        return buf[(x as u16, y)].fg;
                    }
                    acc += s.len();
                }
            }
            panic!("{needle} not on screen");
        };
        assert_eq!(
            row_fg("README.md"),
            dim,
            "a gitignored file's name wears the dimmed foreground"
        );
        assert_eq!(
            row_fg("main.rs"),
            Color::White,
            "non-ignored names keep the normal foreground"
        );
    }

    #[test]
    fn directories_sort_before_files() {
        let (_tmp, tree) = fixture();
        // Skip root (idx 0). Next should be the directory.
        assert!(tree.nodes[1].is_dir);
        assert!(!tree.nodes[2].is_dir);
        assert!(!tree.nodes[3].is_dir);
    }

    #[test]
    fn move_up_clamps_at_zero() {
        let (_tmp, mut tree) = fixture();
        tree.selected = 0;
        tree.move_up();
        assert_eq!(tree.selected, 0);
    }

    #[test]
    fn move_down_clamps_at_last() {
        let (_tmp, mut tree) = fixture();
        let last = tree.nodes.len() - 1;
        tree.selected = last;
        tree.move_down();
        assert_eq!(tree.selected, last);
    }

    #[test]
    fn move_down_then_up() {
        let (_tmp, mut tree) = fixture();
        tree.move_down();
        assert_eq!(tree.selected, 1);
        tree.move_down();
        assert_eq!(tree.selected, 2);
        tree.move_up();
        assert_eq!(tree.selected, 1);
    }

    #[test]
    fn end_jumps_to_last() {
        let (_tmp, mut tree) = fixture();
        tree.end();
        assert_eq!(tree.selected, tree.nodes.len() - 1);
    }

    #[test]
    fn home_jumps_to_first() {
        let (_tmp, mut tree) = fixture();
        tree.selected = 3;
        tree.home();
        assert_eq!(tree.selected, 0);
    }

    #[test]
    fn activate_file_returns_path() {
        let (_tmp, mut tree) = fixture();
        // Find a file node.
        let file_idx = tree.nodes.iter().position(|n| !n.is_dir).unwrap();
        tree.selected = file_idx;
        let path = tree.activate();
        assert!(path.is_some());
        assert!(path.unwrap().is_file());
    }

    #[test]
    fn activate_directory_expands_and_collapses() {
        let (_tmp, mut tree) = fixture();
        // src/ at index 1 (after directories-first sort).
        tree.selected = 1;
        let total_before = tree.nodes.len();
        // Expand src/.
        let opened = tree.activate();
        assert!(opened.is_none()); // no file opened
        assert!(tree.nodes[1].expanded);
        assert!(tree.nodes.len() > total_before);
        // Collapse it again.
        let collapsed = tree.activate();
        assert!(collapsed.is_none());
        assert!(!tree.nodes[1].expanded);
        assert_eq!(tree.nodes.len(), total_before);
    }

    #[test]
    fn expand_selected_does_not_collapse_open_directory() {
        let (_tmp, mut tree) = fixture();
        tree.selected = 1;
        tree.expand_selected();
        let expanded_len = tree.nodes.len();

        tree.expand_selected();

        assert!(tree.nodes[1].expanded);
        assert_eq!(tree.nodes.len(), expanded_len);
    }

    #[test]
    fn collapse_selected_does_not_expand_closed_directory() {
        let (_tmp, mut tree) = fixture();
        tree.selected = 1;

        tree.collapse_selected();

        assert!(!tree.nodes[1].expanded);
        assert_eq!(tree.nodes.len(), 4);
    }

    #[test]
    fn select_clamps_to_valid_index() {
        let (_tmp, mut tree) = fixture();
        let last = tree.nodes.len() - 1;
        tree.select(last);
        assert_eq!(tree.selected, last);
        // Out of range: select() should be a no-op.
        let prev = tree.selected;
        tree.select(9999);
        assert_eq!(tree.selected, prev);
    }

    #[test]
    fn collapse_resets_selection_if_above_new_len() {
        let (_tmp, mut tree) = fixture();
        // Expand src/, point selection at a child, then collapse.
        tree.selected = 1;
        tree.activate();
        // Move selection inside the expanded subtree.
        let inside = tree.nodes.len() - 1;
        tree.selected = inside;
        // Collapse src/.
        tree.selected = 1;
        tree.activate();
        assert!(tree.selected < tree.nodes.len());
    }

    #[test]
    fn validate_new_name_accepts_normal() {
        assert!(validate_new_name("hello.rs").is_ok());
        assert!(validate_new_name("My Folder").is_ok());
        assert!(validate_new_name("a").is_ok());
    }

    #[test]
    fn validate_new_name_rejects_empty_or_whitespace() {
        assert!(validate_new_name("").is_err());
        assert!(validate_new_name("   ").is_err());
    }

    #[test]
    fn validate_new_name_rejects_dots() {
        assert!(validate_new_name(".").is_err());
        assert!(validate_new_name("..").is_err());
    }

    #[test]
    fn validate_new_name_rejects_path_separators() {
        assert!(validate_new_name("a/b").is_err());
        assert!(validate_new_name("a\\b").is_err());
        assert!(validate_new_name("../escape").is_err());
    }

    #[test]
    fn validate_new_name_rejects_nul() {
        assert!(validate_new_name("evil\0name").is_err());
    }

    #[test]
    fn create_target_dir_for_directory_returns_self() {
        let dir = Node {
            path: PathBuf::from("/r/a"),
            depth: 1,
            is_dir: true,
            expanded: false,
            loaded: false,
        };
        assert_eq!(
            create_target_dir_for(Some(&dir), Path::new("/r")),
            PathBuf::from("/r/a")
        );
    }

    #[test]
    fn create_target_dir_for_file_returns_parent() {
        let file = Node {
            path: PathBuf::from("/r/a/b.txt"),
            depth: 2,
            is_dir: false,
            expanded: false,
            loaded: false,
        };
        assert_eq!(
            create_target_dir_for(Some(&file), Path::new("/r")),
            PathBuf::from("/r/a")
        );
    }

    #[test]
    fn create_target_dir_for_none_returns_root() {
        assert_eq!(
            create_target_dir_for(None, Path::new("/r")),
            PathBuf::from("/r")
        );
    }

    #[test]
    fn create_file_in_creates_empty_file() {
        let tmp = TempDir::new().unwrap();
        let path = create_file_in(tmp.path(), "new.txt").unwrap();
        assert!(path.is_file());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    }

    #[test]
    fn create_file_in_errors_when_already_exists() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("x.txt"), "stuff").unwrap();
        let err = create_file_in(tmp.path(), "x.txt").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn create_folder_in_creates_directory() {
        let tmp = TempDir::new().unwrap();
        let path = create_folder_in(tmp.path(), "newdir").unwrap();
        assert!(path.is_dir());
    }

    #[test]
    fn create_folder_in_errors_when_already_exists() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        let err = create_folder_in(tmp.path(), "d").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn delete_target_for_root_returns_none() {
        let tmp = TempDir::new().unwrap();
        let root_node = Node {
            path: tmp.path().to_path_buf(),
            depth: 0,
            is_dir: true,
            expanded: true,
            loaded: true,
        };
        assert!(delete_target_for(Some(&root_node), tmp.path()).is_none());
    }

    #[test]
    fn delete_target_for_empty_space_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(delete_target_for(None, tmp.path()).is_none());
    }

    #[test]
    fn delete_target_for_file_returns_path() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("doomed.txt");
        std::fs::write(&f, "").unwrap();
        let node = Node {
            path: f.clone(),
            depth: 1,
            is_dir: false,
            expanded: false,
            loaded: false,
        };
        assert_eq!(delete_target_for(Some(&node), tmp.path()), Some(f));
    }

    #[test]
    fn delete_target_for_subfolder_returns_path() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let node = Node {
            path: sub.clone(),
            depth: 1,
            is_dir: true,
            expanded: false,
            loaded: false,
        };
        assert_eq!(delete_target_for(Some(&node), tmp.path()), Some(sub));
    }

    #[test]
    fn move_to_trash_removes_file_from_workspace() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("bye.txt");
        std::fs::write(&f, "see ya").unwrap();
        assert!(f.exists());
        move_to_trash(&f).unwrap();
        assert!(
            !f.exists(),
            "file should be gone from the workspace after trash"
        );
    }

    #[test]
    fn move_to_trash_removes_folder_from_workspace() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path().join("byedir");
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join("inner.txt"), "x").unwrap();
        assert!(d.exists());
        move_to_trash(&d).unwrap();
        assert!(
            !d.exists(),
            "folder should be gone from the workspace after trash"
        );
    }

    #[test]
    fn affected_dir_for_event_returns_parent() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let f = sub.join("a.txt");
        std::fs::write(&f, "").unwrap();
        let res = affected_dir_for_event(&f, tmp.path()).unwrap();
        assert_eq!(res, sub.canonicalize().unwrap());
    }

    #[test]
    fn affected_dir_for_event_returns_none_for_root_itself() {
        let tmp = TempDir::new().unwrap();
        assert!(affected_dir_for_event(tmp.path(), tmp.path()).is_none());
    }

    #[test]
    fn affected_dir_for_event_returns_none_when_outside_root() {
        let tmp = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let f = other.path().join("x.txt");
        std::fs::write(&f, "").unwrap();
        assert!(affected_dir_for_event(&f, tmp.path()).is_none());
    }

    #[test]
    fn affected_dir_for_top_level_file_returns_root() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("top.txt");
        std::fs::write(&f, "").unwrap();
        let res = affected_dir_for_event(&f, tmp.path()).unwrap();
        assert_eq!(res, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn affected_dir_for_event_resolves_correctly_for_external_creation() {
        // Simulating: an external process creates `<root>/sub/x.txt`. The
        // watcher fires for that path; the helper should report `<root>/sub`
        // as the directory to refresh.
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let f = sub.join("x.txt");
        std::fs::write(&f, "external write").unwrap();
        let dir = affected_dir_for_event(&f, tmp.path()).expect("inside root");
        // Reload the tree's children of `sub` and verify x.txt is found.
        let mut tree = FileTree::new(tmp.path().to_path_buf());
        // Expand `sub` so its children are loaded.
        let sub_idx = tree
            .nodes
            .iter()
            .position(|n| n.is_dir && n.path.ends_with("sub"))
            .unwrap();
        tree.selected = sub_idx;
        tree.activate();
        // Now create another file externally and confirm refresh_children
        // (driven by the watcher path the helper produces) picks it up.
        let f2 = sub.join("y.txt");
        std::fs::write(&f2, "").unwrap();
        let canon_dir = dir.clone();
        let target_idx = tree.index_of_dir(&canon_dir).unwrap();
        tree.refresh_children(target_idx);
        assert!(
            tree.nodes.iter().any(|n| n.path.ends_with("y.txt")),
            "y.txt should appear after refresh_children driven by the watcher event"
        );
    }

    #[test]
    fn refresh_children_preserves_collapsed_state_on_external_change() {
        // FS events fire asynchronously inside any expanded ancestor. If the
        // user just collapsed a folder (e.g. .git), an event arriving inside
        // it must NOT pop it back open — refresh_children must preserve the
        // user's collapse choice.
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.txt"), "").unwrap();
        let mut tree = FileTree::new(tmp.path().to_path_buf());
        // Expand `sub`, then collapse it again to simulate the user's choice.
        let sub_idx = tree
            .nodes
            .iter()
            .position(|n| n.is_dir && n.path.ends_with("sub"))
            .unwrap();
        tree.selected = sub_idx;
        tree.activate();
        let sub_idx = tree
            .nodes
            .iter()
            .position(|n| n.is_dir && n.path.ends_with("sub"))
            .unwrap();
        tree.selected = sub_idx;
        tree.activate();
        assert!(
            !tree.nodes[sub_idx].expanded,
            "precondition: sub is collapsed"
        );
        // External write fires a watcher event for `sub`.
        std::fs::write(sub.join("b.txt"), "").unwrap();
        tree.refresh_children(sub_idx);
        assert!(
            !tree.nodes[sub_idx].expanded,
            "refresh_children must not re-expand a folder the user collapsed"
        );
    }

    #[test]
    fn refresh_children_preserves_expanded_descendants_when_parent_refreshes() {
        let (_tmp, mut tree) = fixture();
        tree.selected = 1;
        tree.activate();
        let src_path = tree.nodes[1].path.clone();
        assert!(tree.nodes[1].expanded);
        assert!(
            tree.nodes.iter().any(|n| n.path.ends_with("src/lib.rs")),
            "precondition: expanded child should be visible"
        );

        tree.refresh_children(0);

        let src_idx = tree
            .nodes
            .iter()
            .position(|n| n.path == src_path)
            .expect("src should still be present");
        assert!(
            tree.nodes[src_idx].expanded,
            "expanded child directory should survive a root refresh"
        );
        assert!(
            tree.nodes.iter().any(|n| n.path.ends_with("src/lib.rs")),
            "expanded descendant children should be reloaded after parent refresh"
        );
    }

    #[test]
    fn refresh_children_picks_up_newly_created_file() {
        let (tmp, mut tree) = fixture();
        let root_dir_idx = tree.index_of_dir(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("brand-new.txt"), "").unwrap();
        let before = tree.nodes.len();
        tree.refresh_children(root_dir_idx);
        let after = tree.nodes.len();
        assert!(after > before, "expected new node to appear after refresh");
        assert!(
            tree.nodes.iter().any(|n| n.path.ends_with("brand-new.txt")),
            "the newly created file should be in the tree"
        );
    }

    #[test]
    fn page_up_and_page_down() {
        let (_tmp, mut tree) = fixture();
        let last = tree.nodes.len().saturating_sub(1);
        tree.page_down(100);
        assert_eq!(tree.selected, last);
        tree.page_up(100);
        assert_eq!(tree.selected, 0);
    }

    #[test]
    fn rename_in_renames_a_file_within_its_parent() {
        let tmp = TempDir::new().unwrap();
        let old = tmp.path().join("old.txt");
        std::fs::write(&old, "hello").unwrap();
        let new_path = rename_in(tmp.path(), &old, "new.txt").unwrap();
        assert_eq!(new_path, tmp.path().join("new.txt"));
        assert!(!old.exists(), "old name must be gone");
        assert!(new_path.exists(), "new name must exist");
        assert_eq!(std::fs::read_to_string(&new_path).unwrap(), "hello");
    }

    #[test]
    fn rename_in_renames_a_folder_within_its_parent() {
        let tmp = TempDir::new().unwrap();
        let old = tmp.path().join("olddir");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("inner.txt"), "x").unwrap();
        let new_path = rename_in(tmp.path(), &old, "newdir").unwrap();
        assert_eq!(new_path, tmp.path().join("newdir"));
        assert!(new_path.is_dir());
        assert_eq!(
            std::fs::read_to_string(new_path.join("inner.txt")).unwrap(),
            "x"
        );
    }

    #[test]
    fn rename_in_errors_when_target_already_exists() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let err = rename_in(tmp.path(), &a, "b.txt").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        // Both originals must still exist intact.
        assert!(a.exists());
        assert!(b.exists());
    }

    #[test]
    fn rename_in_rejects_invalid_names() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a.txt");
        std::fs::write(&a, "a").unwrap();
        // Slashes and reserved names must be rejected before the syscall.
        assert!(rename_in(tmp.path(), &a, "sub/x.txt").is_err());
        assert!(rename_in(tmp.path(), &a, "").is_err());
        assert!(rename_in(tmp.path(), &a, ".").is_err());
        assert!(rename_in(tmp.path(), &a, "..").is_err());
        // Original is untouched.
        assert!(a.exists());
    }

    #[test]
    fn select_replace_clears_marks_and_sets_anchor() {
        let (_tmp, mut tree) = fixture();
        tree.marked.insert(tree.nodes[1].path.clone());
        tree.select_replace(2);
        assert_eq!(tree.selected, 2);
        assert_eq!(tree.anchor, 2);
        assert!(tree.marked.is_empty());
    }

    #[test]
    fn extend_to_marks_inclusive_range() {
        let (_tmp, mut tree) = fixture();
        tree.select_replace(1);
        tree.extend_to(3);
        assert_eq!(tree.selected, 3);
        assert!(tree.is_marked(1));
        assert!(tree.is_marked(2));
        assert!(tree.is_marked(3));
    }

    #[test]
    fn extend_to_works_in_either_direction() {
        let (_tmp, mut tree) = fixture();
        tree.select_replace(3);
        tree.extend_to(1);
        assert_eq!(tree.selected, 1);
        assert!(tree.is_marked(1));
        assert!(tree.is_marked(2));
        assert!(tree.is_marked(3));
    }

    #[test]
    fn extend_to_replaces_previous_range() {
        let (_tmp, mut tree) = fixture();
        tree.select_replace(0);
        tree.extend_to(2);
        tree.extend_to(1);
        assert!(tree.is_marked(0));
        assert!(tree.is_marked(1));
        assert!(!tree.is_marked(2));
    }

    #[test]
    fn toggle_mark_adds_then_removes() {
        let (_tmp, mut tree) = fixture();
        tree.toggle_mark(1);
        assert!(tree.is_marked(1));
        tree.toggle_mark(1);
        assert!(!tree.is_marked(1));
    }

    #[test]
    fn action_paths_returns_marks_in_tree_order() {
        let (_tmp, mut tree) = fixture();
        tree.select_replace(1);
        tree.toggle_mark(2);
        let paths = tree.action_paths();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], tree.nodes[1].path);
        assert_eq!(paths[1], tree.nodes[2].path);
    }

    #[test]
    fn action_paths_falls_back_to_selected_when_no_marks() {
        let (_tmp, mut tree) = fixture();
        tree.selected = 2;
        let paths = tree.action_paths();
        assert_eq!(paths, vec![tree.nodes[2].path.clone()]);
    }

    #[test]
    fn move_up_and_down_clear_marks() {
        let (_tmp, mut tree) = fixture();
        tree.select_replace(1);
        tree.extend_to(2);
        assert!(!tree.marked.is_empty());
        tree.move_down();
        assert!(tree.marked.is_empty());
        assert_eq!(tree.anchor, tree.selected);
    }

    #[test]
    fn move_down_extend_grows_selection_from_anchor() {
        let (_tmp, mut tree) = fixture();
        tree.select_replace(1);
        tree.move_down_extend();
        assert!(tree.is_marked(1));
        assert!(tree.is_marked(2));
    }

    #[test]
    fn unique_destination_in_avoids_collision() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a.txt");
        std::fs::write(&src, "x").unwrap();
        let dst = tmp.path().join("dest");
        std::fs::create_dir(&dst).unwrap();
        let p = unique_destination_in(&dst, &src);
        assert_eq!(p, dst.join("a.txt"));
        std::fs::write(&p, "x").unwrap();
        let p2 = unique_destination_in(&dst, &src);
        assert_eq!(p2, dst.join("a copy.txt"));
    }

    #[test]
    fn move_into_renames_when_no_collision() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a.txt");
        std::fs::write(&src, "x").unwrap();
        let dst_dir = tmp.path().join("dest");
        std::fs::create_dir(&dst_dir).unwrap();
        let placed = move_into(&dst_dir, &src).unwrap();
        assert_eq!(placed, dst_dir.join("a.txt"));
        assert!(!src.exists());
        assert!(placed.exists());
    }

    #[test]
    fn move_into_dedupes_collision() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a.txt");
        std::fs::write(&src, "src").unwrap();
        let dst_dir = tmp.path().join("dest");
        std::fs::create_dir(&dst_dir).unwrap();
        std::fs::write(dst_dir.join("a.txt"), "preexisting").unwrap();
        let placed = move_into(&dst_dir, &src).unwrap();
        assert_eq!(placed.file_name().unwrap(), "a copy.txt");
        assert_eq!(std::fs::read_to_string(&placed).unwrap(), "src");
        assert_eq!(
            std::fs::read_to_string(dst_dir.join("a.txt")).unwrap(),
            "preexisting"
        );
    }

    #[test]
    fn move_into_refuses_to_move_directory_into_itself() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path().join("d");
        std::fs::create_dir(&d).unwrap();
        let inner = d.join("inner");
        std::fs::create_dir(&inner).unwrap();
        assert!(move_into(&inner, &d).is_err());
    }

    #[test]
    fn copy_into_recurses_into_subfolders() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("d");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("a.txt"), "hi").unwrap();
        let inner = src.join("inner");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(inner.join("b.txt"), "deep").unwrap();
        let dst_dir = tmp.path().join("dest");
        std::fs::create_dir(&dst_dir).unwrap();
        let placed = copy_into(&dst_dir, &src).unwrap();
        assert_eq!(placed, dst_dir.join("d"));
        assert_eq!(std::fs::read_to_string(placed.join("a.txt")).unwrap(), "hi");
        assert_eq!(
            std::fs::read_to_string(placed.join("inner/b.txt")).unwrap(),
            "deep"
        );
        assert!(src.exists(), "copy must leave source intact");
    }

    #[test]
    fn find_prefix_matches_visible_nodes_from_start() {
        let (_tmp, tree) = fixture();
        let idx_src = tree
            .nodes
            .iter()
            .position(|n| n.path.file_name().is_some_and(|s| s == "src"))
            .unwrap();
        let idx_readme = tree
            .nodes
            .iter()
            .position(|n| n.path.file_name().is_some_and(|s| s == "README.md"))
            .unwrap();
        let idx_main = tree
            .nodes
            .iter()
            .position(|n| n.path.file_name().is_some_and(|s| s == "main.rs"))
            .unwrap();
        assert_eq!(tree.find_prefix("s", 0), Some(idx_src));
        assert_eq!(tree.find_prefix("r", 0), Some(idx_readme));
        assert_eq!(tree.find_prefix("m", 0), Some(idx_main));
    }

    #[test]
    fn find_prefix_is_case_insensitive() {
        let (_tmp, tree) = fixture();
        let want = tree
            .nodes
            .iter()
            .position(|n| n.path.file_name().is_some_and(|s| s == "README.md"))
            .unwrap();
        assert_eq!(tree.find_prefix("README", 0), Some(want));
        assert_eq!(tree.find_prefix("readme", 0), Some(want));
        assert_eq!(tree.find_prefix("ReAdMe", 0), Some(want));
    }

    #[test]
    fn find_prefix_wraps_around_to_find_earlier_match() {
        let (_tmp, tree) = fixture();
        let want = tree
            .nodes
            .iter()
            .position(|n| n.path.file_name().is_some_and(|s| s == "README.md"))
            .unwrap();
        let after = tree.nodes.len() - 1;
        assert_eq!(
            tree.find_prefix("r", after),
            Some(want),
            "wrap-around must find the only 'r' node when start is past it"
        );
    }

    #[test]
    fn find_prefix_returns_none_when_no_match() {
        let (_tmp, tree) = fixture();
        assert_eq!(tree.find_prefix("zzzz", 0), None);
    }

    #[test]
    fn find_prefix_returns_none_for_empty_prefix() {
        let (_tmp, tree) = fixture();
        assert_eq!(tree.find_prefix("", 0), None);
    }

    #[test]
    fn rename_in_no_op_returns_ok_with_same_path() {
        // Renaming to the same name is a useful early-return: it covers the
        // case where the user opens the prompt, doesn't change anything,
        // and hits Enter. Treat it as success rather than ErrorKind::AlreadyExists.
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("keep.txt");
        std::fs::write(&a, "k").unwrap();
        let new_path = rename_in(tmp.path(), &a, "keep.txt").unwrap();
        assert_eq!(new_path, a);
        assert!(a.exists());
    }

    // --- Explorer filter (issue #27) ---------------------------------------

    #[test]
    fn subsequence_match_is_case_insensitive_and_fuzzy() {
        // Exact substring.
        assert!(subsequence_match("README.md", "read").is_some());
        // Case-insensitive.
        assert!(subsequence_match("README.md", "READ").is_some());
        assert!(subsequence_match("README.md", "rEaD").is_some());
        // Fuzzy: characters in order but not contiguous ("rdme" ⊂ "README.md").
        assert!(subsequence_match("README.md", "rdme").is_some());
        // Order matters: "daer" is not a subsequence of "README".
        assert!(subsequence_match("README.md", "dRr").is_none());
        // No match.
        assert!(subsequence_match("main.rs", "xyz").is_none());
    }

    #[test]
    fn subsequence_match_reports_matched_byte_offsets() {
        // "lib" matches the first three chars of "lib.rs".
        assert_eq!(subsequence_match("lib.rs", "lib"), Some(vec![0, 1, 2]));
        // Empty query matches with no highlighted offsets.
        assert_eq!(subsequence_match("anything", ""), Some(Vec::new()));
        // Fuzzy offsets: "mn" in "main" -> m@0, n@3.
        assert_eq!(subsequence_match("main", "mn"), Some(vec![0, 3]));
    }

    #[test]
    fn visible_indices_returns_everything_when_no_query() {
        let (_tmp, tree) = fixture();
        let all: Vec<usize> = (0..tree.nodes.len()).collect();
        // No filter open at all.
        assert_eq!(tree.visible_indices(), all);
    }
    fn deep_fixture() -> (TempDir, FileTree) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("outer/inner")).unwrap();
        for i in 0..12 {
            fs::write(root.join(format!("outer/inner/f{i:02}.rs")), "x\n").unwrap();
        }
        let mut tree = FileTree::new(root.to_path_buf());
        // Expand outer, then inner, so the deep files are real rows.
        let outer = tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("outer"))
            .unwrap();
        tree.selected = outer;
        tree.expand_selected();
        let inner = tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("inner"))
            .unwrap();
        tree.selected = inner;
        tree.expand_selected();
        (tmp, tree)
    }

    #[test]
    fn sticky_ancestors_walk_outermost_first_and_cap_keeps_the_deepest() {
        let (_tmp, tree) = deep_fixture();
        let deep = tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("f08.rs"))
            .unwrap();
        let chain = sticky_ancestors(&tree.nodes, deep, 3);
        assert_eq!(chain.len(), 3, "root, outer, inner");
        assert!(tree.nodes[chain[0]].depth < tree.nodes[chain[1]].depth);
        assert!(tree.nodes[chain[2]].path.ends_with("inner"));
        let capped = sticky_ancestors(&tree.nodes, deep, 2);
        assert_eq!(capped.len(), 2);
        assert!(
            tree.nodes[capped[1]].path.ends_with("inner")
                && tree.nodes[capped[0]].path.ends_with("outer"),
            "the cap drops the OUTERMOST level, keeping the closest context"
        );
        assert!(
            sticky_ancestors(&tree.nodes, 0, 3).is_empty(),
            "the root row has no ancestors"
        );
    }

    #[test]
    fn sticky_band_pins_ancestors_and_maps_clicks_but_never_covers_the_selection() {
        let (_tmp, mut tree) = deep_fixture();
        let deep = tree
            .nodes
            .iter()
            .position(|n| n.path.ends_with("f09.rs"))
            .unwrap();
        tree.selected = deep;
        tree.scroll = deep.saturating_sub(3);
        let area = Rect {
            x: 0,
            y: 0,
            width: 34,
            height: 8,
        };
        let mut buf = Buffer::empty(area);
        (&mut tree).render(area, &mut buf);
        assert!(
            !tree.sticky_rows.is_empty(),
            "scrolled deep inside inner/, the band must pin ancestors"
        );
        let (y, idx) = tree.sticky_rows[0];
        assert!(tree.nodes[idx].is_dir, "only directories pin");
        assert_eq!(tree.sticky_row_at(y), Some(idx));
        let row: String = (area.x..area.x + area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        let name = tree.nodes[idx]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            row.contains(&name),
            "the pinned row shows the directory name; row {row:?}"
        );

        // Selection on the top visible row: the band must yield entirely.
        tree.selected = tree.scroll;
        let mut buf = Buffer::empty(area);
        (&mut tree).render(area, &mut buf);
        assert!(
            tree.sticky_rows.is_empty(),
            "the band never covers the selected row"
        );
    }
}
