//! The workspace roots the Explorer lists: replacing the primary root, adding and
//! removing the others' sections, and which root owns a path.

use super::*;

impl FileTree {
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
            folded: 0,
        }];
        self.selected = 0;
        self.scroll = 0;
        self.anchor = 0;
        self.marked.clear();
        self.drag_target = None;
        // The old workspace's ignore set is meaningless under the new root;
        // the git worker re-queries after its SetRoot and repopulates.
        self.ignored = Arc::default();
        self.git_decorations = Arc::default();
        self.scrub_tree = None;
        // Same reasoning for the lane badges (#348): keyed by absolute root
        // path, so a re-root would otherwise carry the old roots' entries
        // until the next sync rebuilt them.
        self.root_badges = Arc::default();
        // Same for the agent review queue (#345), and it matters more: these
        // are ABSOLUTE paths, and the documented use of Make Root is
        // re-rooting into a CHILD, where the new tree's paths are the very
        // ones still in the set. Left behind, every stale dot re-renders on
        // the new tree, describing a lane the user has walked away from —
        // the exact staleness this decoration exists to avoid.
        self.agent_touched = Arc::default();
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
            folded: 0,
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
    pub(super) fn owning_root(&self, path: &Path) -> Option<&Path> {
        self.root_paths()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.components().count())
    }

    /// True when `path` names one of the workspace root rows: the ignored-set
    /// ancestor walk and the guards that protect root rows stop here.
    pub(super) fn is_root_path(&self, path: &Path) -> bool {
        self.root_paths().any(|r| r == path)
    }
}
