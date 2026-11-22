use crate::icons;
use crate::widgets::scrollbar;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod decorations;
mod roots;

pub struct Node {
    pub path: PathBuf,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub loaded: bool,
    /// Compact folders (#1486), VS Code's `explorer.compactFolders`: how
    /// many folders above `path` this row folds in, each the only entry of
    /// the one above it. 0 for an ordinary row. The row
    /// `src/main/java/com/acme` folds 4, and its `path` is the deepest
    /// folder, `acme`, so New File, paste and drop land there.
    pub folded: usize,
}

impl Node {
    /// Every folder the row shows, deepest first: `path` and the `folded`
    /// folders above it.
    pub fn shown_paths(&self) -> impl Iterator<Item = &Path> {
        self.path.ancestors().take(self.folded + 1)
    }

    /// Whether this row shows `path`, as its own or as a folded-in folder.
    pub fn shows(&self, path: &Path) -> bool {
        self.shown_paths().any(|p| p == path)
    }

    /// The row's name: the entry's own, or a compact row's folder names
    /// joined by `/`, as VS Code labels one on every platform.
    pub fn label(&self) -> String {
        let name = |p: &Path| {
            p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        };
        if self.folded == 0 {
            return name(&self.path);
        }
        let shown: Vec<&Path> = self.shown_paths().collect();
        let mut label = String::new();
        for p in shown.into_iter().rev() {
            if !label.is_empty() {
                label.push('/');
            }
            label.push_str(&name(p));
        }
        label
    }
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
    /// Git status decorations (#1513), fed from the same status refresh as
    /// `ignored`: each changed file and each folder above one, absolute,
    /// with the status it wears. Rows colour their name by it and show its
    /// letter (files) or a dot (folders) at the right edge, as VS Code does.
    pub git_decorations: Arc<HashMap<PathBuf, crate::git::GitDecoration>>,
    /// While the history scrubber sits on a commit (#371): the directory
    /// the listing covers and every path in that commit's tree under it,
    /// absolute, folders included. A row under that directory and absent
    /// from it (submodule contents count as present) did not exist there,
    /// and its name takes the same dimmed foreground as an ignored one.
    /// `None` everywhere else, including at the working tree.
    pub scrub_tree: Option<(PathBuf, Arc<crate::git::CommitTree>)>,
    /// Workspace files an agent changed and the user has not reviewed
    /// (#345), absolute. Rows in this set take a TRAILING dot in the theme's
    /// yellow; it clears when the file is marked reviewed.
    ///
    /// One colour, not one per agent: `AgentLane` carries no colour today,
    /// and inventing one here would put the mapping in the wrong place.
    ///
    /// Membership is EXACT, unlike `ignored`, which covers a directory's
    /// descendants. A directory is not something an agent wrote and not
    /// something "mark reviewed" can clear, so a dot on one would be a mark
    /// no gesture removes. A collapsed folder's dimmer rollup dot is derived
    /// from this set at paint time instead (see
    /// [`Self::hides_agent_touched`]). Fed from `AgentLedger` on the app's
    /// sync.
    pub agent_touched: Arc<HashSet<PathBuf>>,
    /// The project's `files_exclude` globs (#1345): matching entries are
    /// not listed, like VS Code's `files.exclude`.
    pub exclude: crate::widgets::search::PathFilter,
    /// Fold a folder whose only entry is a folder into one row with it
    /// (#1486). On by default; the `disable_compact_folders` setting clears
    /// it through [`Self::set_compact_folders`].
    pub compact_folders: bool,
    /// Text painted after a ROOT row's name (#348): a worktree lane's
    /// branch and the badge of the agent seated in its pane, keyed by the
    /// root path. Swapped in by the app whenever a lane or a seat changes;
    /// a root with no entry paints as before.
    pub root_badges: Arc<HashMap<PathBuf, String>>,
    /// Unresolved sticky notes per file (#367), painted as a count after
    /// the name.
    pub note_counts: Arc<HashMap<PathBuf, usize>>,
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

/// The unreviewed-file marker (#345). A middle dot rather than a filled
/// circle: it must read as a small annotation beside a filename, not compete
/// with the file-type icon at the start of the row.
const AGENT_DOT: char = '\u{b7}';

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
                folded: 0,
            }],
            selected: 0,
            scroll: 0,
            focused: true,
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            ignored: Arc::default(),
            git_decorations: Arc::default(),
            scrub_tree: None,
            agent_touched: Arc::default(),
            exclude: Default::default(),
            compact_folders: true,
            note_counts: Arc::default(),
            root_badges: Arc::default(),
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

    /// The entries the Explorer lists in `dir`, unsorted, each with whether
    /// it is a folder.
    fn read_entries(&self, dir: &Path) -> Vec<(PathBuf, bool)> {
        std::fs::read_dir(dir)
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
            .filter(|(p, _)| !self.exclude.excludes(&self.root, p))
            .collect()
    }

    /// Re-derive row `idx`'s compact chain from disk (#1486) and return the
    /// entries of the folder it ends on: from its top folder, fold in each
    /// folder that is the only entry of the one above. Only a listed row
    /// folds (on expand, as VS Code resolves single-child folders), at one
    /// listing per level, what expanding each would have cost. A workspace
    /// root never folds: it heads its section.
    fn fold(&mut self, idx: usize) -> Vec<(PathBuf, bool)> {
        let node = &mut self.nodes[idx];
        if let Some(top) = node.path.ancestors().nth(node.folded) {
            node.path = top.to_path_buf();
        }
        node.folded = 0;
        let mut entries = self.read_entries(&self.nodes[idx].path);
        while self.compact_folders
            && self.nodes[idx].depth > 0
            && let [(only, true)] = entries.as_slice()
        {
            let only = only.clone();
            entries = self.read_entries(&only);
            let node = &mut self.nodes[idx];
            node.path = only;
            node.folded += 1;
        }
        entries
    }

    fn load_children(&mut self, idx: usize) {
        if self.nodes[idx].loaded {
            return;
        }
        let depth = self.nodes[idx].depth + 1;
        let mut entries = self.fold(idx);
        entries.sort_by(|a, b| match (a.1, b.1) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => explorer_name_cmp(&a.0, &b.0),
        });
        let new_nodes: Vec<Node> = entries
            .into_iter()
            .map(|(p, is_dir)| Node {
                path: p,
                depth,
                is_dir,
                expanded: false,
                loaded: false,
                folded: 0,
            })
            .collect();
        let insert_at = idx + 1;
        for (i, n) in new_nodes.into_iter().enumerate() {
            self.nodes.insert(insert_at + i, n);
        }
        self.nodes[idx].loaded = true;
    }

    /// Reload row `idx`'s children, re-expanding those in `expanded_paths`
    /// and re-folding the collapsed compact rows whose top folder is in
    /// `folded_tops` (#1486), so a refresh leaves both as they were.
    fn load_children_preserving_expansion(
        &mut self,
        idx: usize,
        expanded_paths: &BTreeSet<PathBuf>,
        folded_tops: &BTreeSet<PathBuf>,
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
                self.load_children_preserving_expansion(child, expanded_paths, folded_tops);
            } else if folded_tops.contains(&self.nodes[child].path) {
                self.fold(child);
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
        // Every folder an open compact row shows counts as open (#1486),
        // this row's own included: when its chain splits, the folders it
        // splits into open again, so the rows under it stay on screen.
        let expanded_paths: BTreeSet<PathBuf> = self.nodes[idx..end]
            .iter()
            .filter(|n| n.is_dir && n.expanded)
            .flat_map(Node::shown_paths)
            .map(Path::to_path_buf)
            .collect();
        let folded_tops: BTreeSet<PathBuf> = self.nodes[(idx + 1)..end]
            .iter()
            .filter(|n| !n.expanded && n.folded > 0)
            .filter_map(|n| n.shown_paths().last().map(Path::to_path_buf))
            .collect();
        let selected_path = self.selected_path().map(Path::to_path_buf);
        self.nodes.drain((idx + 1)..end);
        self.nodes[idx].loaded = false;
        if self.nodes[idx].expanded {
            self.load_children_preserving_expansion(idx, &expanded_paths, &folded_tops);
        } else if self.nodes[idx].folded > 0 {
            // A collapsed compact row checks its chain too, so an entry
            // added to a folder it folds in splits it now.
            self.fold(idx);
        }
        if let Some(path) = selected_path
            && let Some(new_idx) = self.row_of(&path)
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
    ///
    /// A compact row (#1486) answers for every folder it folds in, so an
    /// entry added to `main` under `src/main/java` refreshes, and splits,
    /// that row.
    pub fn index_of_dir(&self, path: &Path) -> Option<usize> {
        let canon_target = path.canonicalize().ok();
        self.nodes.iter().position(|n| {
            if !n.is_dir {
                return false;
            }
            if n.shows(path) {
                return true;
            }
            if let Some(target) = canon_target.as_ref()
                && let Ok(canon_node) = n.path.canonicalize()
                && canon_node
                    .ancestors()
                    .take(n.folded + 1)
                    .any(|p| p == target)
            {
                return true;
            }
            false
        })
    }

    /// The row showing `path`: its own, or the compact row that folds it
    /// in (#1486).
    pub fn row_of(&self, path: &Path) -> Option<usize> {
        self.nodes.iter().position(|n| n.shows(path))
    }

    /// Turn compact folders (#1486) on or off, re-listing every root so
    /// open rows fold or split at once. Only listed folders fold, so with
    /// none open below the roots, as at launch, nothing is re-listed.
    pub fn set_compact_folders(&mut self, on: bool) {
        if self.compact_folders == on {
            return;
        }
        self.compact_folders = on;
        if !self
            .nodes
            .iter()
            .any(|n| n.depth > 0 && (n.expanded || n.folded > 0))
        {
            return;
        }
        // Bottom up, so re-listing one section leaves the earlier roots'
        // indices where they were.
        for idx in (0..self.nodes.len()).rev() {
            if self.nodes[idx].depth == 0 {
                self.refresh_children(idx);
            }
        }
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
mod android_trash;

/// Rename the entry at `old_path` to `new_name` within `parent`. Validates
/// `new_name` via `validate_new_name`, refuses to overwrite an existing
/// entry, and returns the new absolute path on success. A no-op rename
/// (same name) returns Ok with the original path unchanged so the user
/// can hit Enter on the prompt without typing.
pub fn rename_in(parent: &Path, old_path: &Path, new_name: &str) -> std::io::Result<PathBuf> {
    let target = rename_target(parent, old_path, new_name)?;
    if target != old_path {
        std::fs::rename(old_path, &target)?;
    }
    Ok(target)
}

/// Whether renaming `old_path` to `new_name` in `parent` only respells the
/// same entry on a case-insensitive volume (macOS by default), where
/// `README.md` "exists" while `readme.md` is renamed to it. True only when
/// the directory does not list `new_name` as an entry of its own (so a
/// separate file, or a hard link to the same file, is still a clash) and
/// the name resolves to the entry being renamed.
fn is_case_only_respelling(parent: &Path, old_path: &Path, new_name: &str) -> bool {
    let listed = std::fs::read_dir(parent).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name() == std::ffi::OsStr::new(new_name))
    });
    if listed {
        return false;
    }
    let target = parent.join(new_name);
    match (
        std::fs::symlink_metadata(&target),
        std::fs::symlink_metadata(old_path),
    ) {
        #[cfg(unix)]
        (Ok(a), Ok(b)) => {
            use std::os::unix::fs::MetadataExt;
            a.dev() == b.dev() && a.ino() == b.ino()
        }
        // No inode to compare: a name the directory does not list that
        // still resolves can only be the same entry respelled.
        #[cfg(not(unix))]
        (Ok(_), Ok(_)) => true,
        _ => false,
    }
}

/// Where [`rename_in`] would put `old_path`, with the same validation and
/// without touching the disk, so a caller can ask language servers about
/// the rename before it happens (#610).
pub fn rename_target(parent: &Path, old_path: &Path, new_name: &str) -> std::io::Result<PathBuf> {
    let trimmed = new_name.trim();
    if let Err(msg) = validate_new_name(trimmed) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg));
    }
    let target = parent.join(trimmed);
    if target != old_path && target.exists() && !is_case_only_respelling(parent, old_path, trimmed)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", target.display()),
        ));
    }
    Ok(target)
}

/// Given a filesystem event path and the tree's workspace root, return the
/// directory whose children should be refreshed.
///
/// * If `event_path` is the root or outside the root, returns `None`.
/// * Otherwise returns the parent directory of `event_path`. The watcher
///   layer is the one that decides whether the event is a create / remove /
///   rename; this helper is only concerned with which subtree to invalidate.
///
/// `canon_root` is `root` resolved through symlinks. The caller supplies it
/// because the watch root cannot change without a rebind, so it is resolved
/// once per bind rather than once per path of every event.
pub fn affected_dir_for_event(
    event_path: &Path,
    root: &Path,
    canon_root: &Path,
) -> Option<PathBuf> {
    // Fast path: zero syscalls. Taken only when the root is ALREADY its own
    // canonical form and the event path is spelled under it — the ordinary
    // case, and the one that was costing everything. When the root reaches
    // the tree through a symlink (a macOS TempDir under /var -> /private/var,
    // a symlinked checkout) the answer must be the resolved parent, so that
    // case falls through to the original path below and is unchanged.
    //
    // `canonicalize` is not a cheap accessor: glibc `realpath()` walks the
    // path a component at a time and issues a `readlink` per component, each
    // failing `EINVAL` on an ordinary directory. Resolving the root here for
    // every path of every event, every frame, is what made
    // `__x64_sys_readlink` 30% of process cycles and held ~210% CPU on a
    // home-dir workspace — `readlink("/root")` alone fired ~770 times a
    // second purely to re-learn that the root is not a symlink.
    if root == canon_root && event_path.starts_with(root) {
        if event_path == root {
            return None;
        }
        return event_path.parent().map(Path::to_path_buf);
    }
    let canon_event = event_path
        .canonicalize()
        .unwrap_or_else(|_| event_path.to_path_buf());
    if !canon_event.starts_with(canon_root) {
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

/// Whether `source` already sits directly in `dir`, so moving it there moves
/// nothing (#1174). A source that no longer exists (deleted or renamed since
/// it was cut) is not in place: it takes the ordinary path, which reports it
/// missing. A dangling symlink still exists, as the link itself.
pub fn is_directly_in(dir: &Path, source: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    source.symlink_metadata().is_ok()
        && source
            .parent()
            .is_some_and(|parent| canon(parent) == canon(dir))
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
    // Already there: a fresh name would rename it to "<name> copy" (#1174).
    if is_directly_in(dest_dir, source) {
        return Ok(source.to_path_buf());
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
fn push_name_spans(
    spans: &mut Vec<Span<'static>>,
    name: &str,
    query: &str,
    base: Style,
    theme: crate::theme::Theme,
) {
    let Some(offsets) = (!query.is_empty())
        .then(|| subsequence_match(name, query))
        .flatten()
    else {
        spans.push(Span::styled(name.to_string(), base));
        return;
    };
    let match_style = base
        .fg(theme.ui(FILTER_MATCH_FG))
        .add_modifier(Modifier::BOLD);
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
            Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        // Black theme: chipless brand header (panelTitle.activeForeground);
        // Croft Dark keeps the historical white-on-navy chip.
        let label = format!(" {} ", crate::i18n::tr("EXPLORER"));
        let label_width = unicode_width::UnicodeWidthStr::width(label.as_str());
        let tones = crate::widgets::surface::tones(self.theme);
        // Croft Loch: the view title is the one label in the side bar that
        // says where focus is, so it is the strong ink while the tree has
        // the keyboard and recedes to muted otherwise. Never bold: weight
        // is kept for the workspace root below it.
        let title = if let Some(t) = tones {
            Span::styled(
                label,
                Style::default().fg(if self.focused { t.strong() } else { t.muted() }),
            )
        } else if self.focus_gradient {
            Span::styled(
                label,
                Style::default()
                    .fg(crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                label,
                Style::default()
                    .fg(self.theme.ui(Color::White))
                    .bg(self.theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e)))
                    .add_modifier(Modifier::BOLD),
            )
        };
        let block =
            crate::widgets::surface::view_block(self.theme, true, block_style).title(title.clone());
        let inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the orange→green
        // gradient (matching the welcome activity box), then re-stamp the
        // title the gradient top edge just overwrote. Croft Loch marks focus
        // with the accent lip, which starts one cell past the title's own
        // padding: the title keeps a breath of space on both sides and the
        // lip's accent end sits right beside the name of the focused view.
        if tones.is_some() {
            if self.focused {
                let lip_x = area
                    .x
                    .saturating_add(1)
                    .saturating_add(u16::try_from(label_width).unwrap_or(u16::MAX))
                    .min(area.right());
                crate::widgets::surface::paint_focus_rule(
                    buf,
                    Rect {
                        x: lip_x,
                        width: area.right() - lip_x,
                        ..area
                    },
                    self.theme,
                );
            }
        } else if crate::widgets::surface::paint_view_focus(
            buf,
            area,
            self.theme,
            self.focused,
            self.focus_gradient,
        ) {
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
        // Only when the pill clears the (translated) title, which starts one
        // cell in from the corner: `x + 1 + label_width <= x + width - 4`.
        if usize::from(area.width) >= label_width + 5
            && let Some(rect) = crate::widgets::header_pill::render_action(
                buf,
                area.x + area.width - 1,
                area.y,
                crate::widgets::header_pill::MORE_LABEL,
                self.theme,
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
        // No Explorer filter any more, so no characters are ever highlighted.
        let query = String::new();
        let end = (scroll_pos + visible_height).min(visible.len());
        // Croft Loch's indent guides: the guide of the selected row's parent
        // folder is drawn a step brighter down that folder's children (VS
        // Code's active indent guide), as `(level, first, end)` node indices.
        let active_guide = tones.and_then(|_| self.active_indent_guide());
        for (row, &idx) in visible[scroll_pos..end].iter().enumerate() {
            let node = &self.nodes[idx];
            let is_selected = idx == self.selected;
            let is_marked = self.marked.contains(&node.path);
            let is_drop_target = self.drag_target == Some(idx);
            let y = inner.y + row as u16;

            let mut spans: Vec<Span> = Vec::with_capacity(6);
            match tones {
                // Croft Loch: a thin guide under each ancestor folder's
                // chevron, in the cells the indent already takes. The
                // workspace root's level gets none: every row is under it,
                // so its guide would only be a second left edge.
                Some(t) if node.depth >= 2 => {
                    spans.push(Span::raw("  "));
                    for level in 1..node.depth {
                        let active = active_guide.is_some_and(|(l, first, end)| {
                            l == level && (first..end).contains(&idx)
                        });
                        spans.push(Span::styled(
                            "\u{258f} ",
                            Style::default().fg(if active { t.faint() } else { t.hairline() }),
                        ));
                    }
                }
                _ => spans.push(Span::raw("  ".repeat(node.depth))),
            }

            let name = node.label();

            // Git-ignored and changed rows colour the *name* only — icons
            // keep their color, matching VS Code's git decorations.
            let name_fg = self.name_fg(&node.path);
            // Croft Loch's quiet layer: chevrons, the rolled-up dot, the
            // root badge and note counts all in the muted ink.
            let mark_fg = tones.map_or_else(|| self.theme.ui(Color::Gray), |t| t.muted());

            if node.is_dir {
                let chev = crate::widgets::surface::chevron(self.theme, node.expanded);
                let icon = if node.expanded {
                    icons::FOLDER_OPEN
                } else {
                    icons::FOLDER_CLOSED
                };
                spans.push(Span::styled(
                    format!("{chev} "),
                    Style::default().fg(mark_fg),
                ));
                spans.push(Span::styled(
                    format!("{} ", icon.glyph),
                    Style::default().fg(self.folder_icon_fg(icon.color)),
                ));
                // Bold marks a folder on a boxed theme. Croft Loch keeps
                // weight for the workspace root alone, so the tree reads as
                // one calm list under a single heading.
                let base = if tones.is_some() && node.depth > 0 {
                    Style::default().fg(name_fg)
                } else {
                    Style::default().fg(name_fg).add_modifier(Modifier::BOLD)
                };
                push_name_spans(&mut spans, &name, &query, base, self.theme);
                // A collapsed folder hiding an unreviewed agent write shows a
                // dimmer copy of the file dot (#345), so the lane's changes
                // stay findable without expanding every folder. Only while
                // collapsed: expanded, the file's own dot is on screen.
                if !node.expanded && self.hides_agent_touched(&node.path) {
                    spans.push(Span::styled(
                        format!(" {AGENT_DOT}"),
                        Style::default().fg(mark_fg),
                    ));
                }
                // A lane root wears its branch and its agent after the name
                // (#348), dim so the folder name stays the thing the eye
                // lands on; trailing for the same reason as the agent dot.
                if node.depth == 0
                    && let Some(badge) = self.root_badges.get(&node.path)
                {
                    // `Gray`, the dim the theme maps for light grounds;
                    // `DarkGray` is a named colour `Theme::ui` passes through.
                    spans.push(Span::styled(
                        format!("  {badge}"),
                        Style::default().fg(mark_fg),
                    ));
                }
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
                    Style::default().fg(self.theme.ui(icon.color)),
                ));
                push_name_spans(
                    &mut spans,
                    &name,
                    &query,
                    Style::default().fg(name_fg),
                    self.theme,
                );
                // The agent-lane dot (#345), AFTER the name rather than
                // before it: a leading marker would shift every name in the
                // tree by a column as writes land, so the eye loses the
                // alignment it reads the tree by. Trailing, it appears and
                // clears without moving anything.
                if self.is_agent_touched(&node.path) {
                    // Croft Loch takes its own palette's yellow rather than
                    // the named colour, which follows the host terminal.
                    let dot = match tones {
                        Some(_) => crate::gradient::rgb_color(self.theme.ansi()[3]),
                        None => self.theme.ui(Color::Yellow),
                    };
                    spans.push(Span::styled(
                        format!(" {AGENT_DOT}"),
                        Style::default().fg(dot),
                    ));
                }
                if let Some(n) = self.note_counts.get(&node.path) {
                    spans.push(Span::styled(
                        format!(" \u{270e}{n}"),
                        Style::default().fg(mark_fg),
                    ));
                }
            }

            let line = Line::from(spans);
            let row_rect = Rect {
                x: inner.x,
                y,
                width: row_width,
                height: 1,
            };
            let drop_bg = self.theme.ui(Color::Rgb(0x2c, 0x60, 0x2e));
            if let Some(t) = tones {
                // Croft Loch: the one selected-row anatomy, the band with the
                // accent bar standing in the pane's left padding column so
                // the names keep their column; greyed while another pane has
                // the keyboard. Marked rows wear a softer tint of the band,
                // hover a neutral lift.
                if is_drop_target {
                    buf.set_style(row_rect, Style::default().bg(drop_bg));
                } else if is_selected {
                    crate::widgets::surface::paint_selected_row(
                        buf,
                        Rect {
                            x: area.x,
                            width: row_width.saturating_add(inner.x - area.x),
                            ..row_rect
                        },
                        t,
                        t.chrome,
                        self.focused,
                    );
                } else if is_marked {
                    let tint = crate::gradient::lerp_rgb(t.chrome, t.accent, 0.12);
                    buf.set_style(
                        row_rect,
                        Style::default().bg(crate::gradient::rgb_color(tint)),
                    );
                } else if let Some(bg) =
                    crate::widgets::hover::row_hover_bg(row_rect, pointer, self.theme)
                {
                    buf.set_style(row_rect, Style::default().bg(bg));
                }
            } else {
                // Black theme: the selected/marked rows wear the brand's
                // muted dark-teal fill (identical to the right-click menu's
                // selection), replacing the legacy bright-blue accent. Croft
                // Dark keeps the historical blue. The drop target stays
                // green either way.
                let (sel_bg, mark_bg) = if self.focus_gradient {
                    (
                        crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG),
                        self.theme.ui(Color::Rgb(0x18, 0x35, 0x32)),
                    )
                } else {
                    (
                        self.theme.ui(Color::Rgb(0x09, 0x4d, 0x77)),
                        self.theme.ui(Color::Rgb(0x07, 0x33, 0x55)),
                    )
                };
                let line_style = if is_drop_target {
                    Style::default().bg(drop_bg)
                } else if is_selected {
                    Style::default().bg(sel_bg)
                } else if is_marked {
                    Style::default().bg(mark_bg)
                } else if let Some(bg) =
                    crate::widgets::hover::row_hover_bg(row_rect, pointer, self.theme)
                {
                    Style::default().bg(bg)
                } else {
                    Style::default()
                };
                buf.set_style(row_rect, line_style);
            }
            // A changed row's status sits at the right edge (#1513): the
            // file's letter, or a dot on a folder with changes inside. The
            // name is clipped short of it so a long name never covers it.
            let badge = self
                .git_decoration(&node.path)
                .filter(|_| row_width >= 8 && !self.is_ignored(&node.path));
            match badge {
                Some(dec) => {
                    let glyph = if node.is_dir {
                        '\u{25cf}'
                    } else {
                        dec.letter()
                    };
                    buf.set_line(inner.x, y, &line, row_width - 3);
                    buf.set_string(
                        inner.x + row_width - 2,
                        y,
                        glyph.to_string(),
                        Style::default().fg(self.theme.git_decoration_fg(dec)),
                    );
                }
                None => {
                    buf.set_line(inner.x, y, &line, row_width);
                }
            }
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
                for (i, &glyph) in GLYPHS.iter().enumerate() {
                    let x = start_x + i as u16 * step;
                    let rect = Rect {
                        x,
                        y,
                        width: step,
                        height: 1,
                    };
                    let hovered = crate::widgets::hover::contains(rect, self.hover_pointer);
                    header_pill::render(buf, x, y, glyph, self.theme, hovered);
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
            // Croft Loch lifts the band onto the raised tone: the theme's
            // sticky tint is a hair off the chrome and vanished against it.
            let bg = tones.map_or_else(|| self.theme.sticky_scroll_bg(), |t| t.raised());
            let mark_fg = tones.map_or_else(|| self.theme.ui(Color::Gray), |t| t.muted());
            for (i, &aidx) in shown.iter().enumerate() {
                let y = inner.y + i as u16;
                for x in inner.x..inner.x + row_width {
                    buf[(x, y)].set_symbol(" ");
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
                let node = &self.nodes[aidx];
                let name = node.label();
                let name_fg = self.name_fg(&node.path);
                let name_style = if tones.is_some() && node.depth > 0 {
                    Style::default().fg(name_fg).bg(bg)
                } else {
                    Style::default()
                        .fg(name_fg)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD)
                };
                let spans = vec![
                    Span::raw("  ".repeat(node.depth)),
                    Span::styled(
                        format!("{} ", crate::widgets::surface::chevron(self.theme, true)),
                        Style::default().fg(mark_fg).bg(bg),
                    ),
                    Span::styled(
                        format!("{} ", icons::FOLDER_OPEN.glyph),
                        Style::default()
                            .fg(self.folder_icon_fg(icons::FOLDER_OPEN.color))
                            .bg(bg),
                    ),
                    Span::styled(name, name_style),
                ];
                buf.set_line(inner.x, y, &Line::from(spans), row_width);
                self.sticky_rows.push((y, aidx));
            }
        }
        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical_on_chrome(buf, metrics, self.focused, self.theme);
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

/// Explorer order of two sibling paths: [`natural_cmp`] on their names,
/// then the raw `OsStr` names, so non-UTF-8 names whose lossy renderings
/// match still keep a fixed order.
fn explorer_name_cmp(a: &Path, b: &Path) -> std::cmp::Ordering {
    let (a, b) = (
        a.file_name().unwrap_or_default(),
        b.file_name().unwrap_or_default(),
    );
    natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()).then_with(|| a.cmp(b))
}

/// Explorer order (#1365), VS Code's default: digit runs compare by their
/// value, so `chapter2` comes before `chapter10`, and text compares without
/// case, so `apple.py` comes before `Makefile`. Names equal that way fall
/// back to the shorter first (`v1` before `v01`, as VS Code does), then
/// their bytes (`A` before `a`), so the order is total and stable.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut run = String::new();
                    while let Some(c) = it.next_if(char::is_ascii_digit) {
                        run.push(c);
                    }
                    run
                };
                let (m, n) = (take(&mut x), take(&mut y));
                // By value without parsing, so a 30-digit run can't
                // overflow: fewer significant digits is smaller.
                let (m, n) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let by_value = m.len().cmp(&n.len()).then_with(|| m.cmp(n));
                if by_value != Ordering::Equal {
                    return by_value;
                }
            }
            (Some(c), Some(d)) => {
                let by_text = c.to_lowercase().cmp(d.to_lowercase());
                if by_text != Ordering::Equal {
                    return by_text;
                }
                x.next();
                y.next();
            }
        }
    }
}

#[cfg(test)]
mod tests;
