use crate::icons;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
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
    pub last_inner: Rect,
    pub last_area: Rect,
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
            last_inner: Rect::default(),
            last_area: Rect::default(),
        };
        tree.load_children(0);
        tree
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
                let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or_else(|_| p.is_dir());
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
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.nodes.len() {
            self.selected += 1;
        }
    }

    pub fn page_up(&mut self, page: usize) {
        self.selected = self.selected.saturating_sub(page);
    }

    pub fn page_down(&mut self, page: usize) {
        self.selected = (self.selected + page).min(self.nodes.len().saturating_sub(1));
    }

    pub fn home(&mut self) {
        self.selected = 0;
    }

    pub fn end(&mut self) {
        self.selected = self.nodes.len().saturating_sub(1);
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

    pub fn selected_path(&self) -> Option<&Path> {
        self.nodes.get(self.selected).map(|n| n.path.as_path())
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
        self.nodes.drain((idx + 1)..end);
        self.nodes[idx].loaded = false;
        if self.nodes[idx].expanded {
            self.load_children(idx);
        }
        if self.selected >= self.nodes.len() {
            self.selected = self.nodes.len().saturating_sub(1);
        }
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
            if let Some(target) = canon_target.as_ref() {
                if let Ok(canon_node) = n.path.canonicalize() {
                    if &canon_node == target {
                        return true;
                    }
                }
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
    if &n.path == root {
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
    trash::delete(path).map_err(|e| std::io::Error::other(format!("{e}")))
}

/// Rename the entry at `old_path` to `new_name` within `parent`. Validates
/// `new_name` via `validate_new_name`, refuses to overwrite an existing
/// entry, and returns the new absolute path on success. A no-op rename
/// (same name) returns Ok with the original path unchanged so the user
/// can hit Enter on the prompt without typing.
pub fn rename_in(
    parent: &Path,
    old_path: &Path,
    new_name: &str,
) -> std::io::Result<PathBuf> {
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
        assert!(!f.exists(), "file should be gone from the workspace after trash");
    }

    #[test]
    fn move_to_trash_removes_folder_from_workspace() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path().join("byedir");
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join("inner.txt"), "x").unwrap();
        assert!(d.exists());
        move_to_trash(&d).unwrap();
        assert!(!d.exists(), "folder should be gone from the workspace after trash");
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
        assert!(!tree.nodes[sub_idx].expanded, "precondition: sub is collapsed");
        // External write fires a watcher event for `sub`.
        std::fs::write(sub.join("b.txt"), "").unwrap();
        tree.refresh_children(sub_idx);
        assert!(
            !tree.nodes[sub_idx].expanded,
            "refresh_children must not re-expand a folder the user collapsed"
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

impl Widget for &mut FileTree {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style)
            .title(Span::styled(
                " EXPLORER ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_inner = inner;
        self.last_area = area;

        let visible_height = inner.height as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible_height {
            self.scroll = self.selected + 1 - visible_height;
        }

        let end = (self.scroll + visible_height).min(self.nodes.len());
        for (row, idx) in (self.scroll..end).enumerate() {
            let node = &self.nodes[idx];
            let is_selected = idx == self.selected;
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
            let line_style = if is_selected {
                Style::default().bg(Color::Rgb(0x09, 0x4d, 0x77))
            } else {
                Style::default()
            };
            buf.set_style(Rect { x: inner.x, y, width: inner.width, height: 1 }, line_style);
            buf.set_line(inner.x, y, &line, inner.width);
        }
    }
}
