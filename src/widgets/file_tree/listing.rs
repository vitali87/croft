//! Listing a folder's entries: reading them, Explorer order, compact folders, loading
//! and refreshing children with their expansion kept, and which folder a file system
//! event refreshes.

use super::*;

impl FileTree {
    /// The entries the Explorer lists in `dir`, unsorted, each with whether
    /// it is a folder.
    pub(super) fn read_entries(&self, dir: &Path) -> Vec<(PathBuf, bool)> {
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
    pub(super) fn fold(&mut self, idx: usize) -> Vec<(PathBuf, bool)> {
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

    pub(super) fn load_children(&mut self, idx: usize) {
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
    pub(super) fn load_children_preserving_expansion(
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

/// Explorer order of two sibling paths: [`natural_cmp`] on their names,
/// then the raw `OsStr` names, so non-UTF-8 names whose lossy renderings
/// match still keep a fixed order.
pub(super) fn explorer_name_cmp(a: &Path, b: &Path) -> std::cmp::Ordering {
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
