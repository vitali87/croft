use crate::icons;
use crate::widgets::scrollbar;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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
    pub last_inner: Rect,
    pub last_area: Rect,
    pub last_scrollbar: Rect,
    pub anchor: usize,
    pub marked: BTreeSet<PathBuf>,
    /// While the user is mid-drag, the index of the directory row currently
    /// under the pointer (or the parent dir of a hovered file). Drawn with
    /// a highlighted bg so the drop target is unambiguous. Cleared on drop
    /// or cancel.
    pub drag_target: Option<usize>,
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
            last_inner: Rect::default(),
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            anchor: 0,
            marked: BTreeSet::new(),
            drag_target: None,
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
        self.load_children(0);
    }

    /// Map a screen y coordinate to a node index, if any.
    pub fn node_at_y(&self, y: u16) -> Option<usize> {
        if y < self.last_inner.y || y >= self.last_inner.y + self.last_inner.height {
            return None;
        }
        let row = (y - self.last_inner.y) as usize;
        let idx = self.scroll + row;
        if idx < self.nodes.len() {
            Some(idx)
        } else {
            None
        }
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
        let Ok(rel) = target.strip_prefix(&self.root) else {
            return false;
        };
        let mut current_path = self.root.clone();
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
    if n.path == root {
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
        self.last_inner = inner;
        self.last_area = area;
        self.last_scrollbar = Rect::default();

        let visible_height = inner.height as usize;
        if visible_height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible_height {
            self.scroll = self.selected + 1 - visible_height;
        }
        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: inner.y,
            width: u16::from(inner.width > 0),
            height: inner.height,
        };
        let scrollbar_metrics = scrollbar::vertical_metrics(
            scrollbar_area,
            self.nodes.len(),
            visible_height,
            self.scroll,
        );
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }
        let row_width = inner
            .width
            .saturating_sub(u16::from(scrollbar_metrics.is_some()));

        let end = (self.scroll + visible_height).min(self.nodes.len());
        for (row, idx) in (self.scroll..end).enumerate() {
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
                spans.push(Span::styled(
                    name,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ));
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
                spans.push(Span::styled(name, Style::default().fg(Color::White)));
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
            let line_style = if is_drop_target {
                Style::default().bg(Color::Rgb(0x2c, 0x60, 0x2e))
            } else if is_selected {
                Style::default().bg(sel_bg)
            } else if is_marked {
                Style::default().bg(mark_bg)
            } else {
                Style::default()
            };
            buf.set_style(
                Rect {
                    x: inner.x,
                    y,
                    width: row_width,
                    height: 1,
                },
                line_style,
            );
            buf.set_line(inner.x, y, &line, row_width);
        }
        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical(buf, metrics, self.focused);
        }
    }
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
}
