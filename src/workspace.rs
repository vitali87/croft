//! The workspace's root-folder set (multi-root Phase 1a, #143).
//!
//! croft historically held ONE workspace root as a bare `PathBuf` threaded
//! through every subsystem. Multi-root workspaces (VS Code's ordered
//! "workspace folders") need the set to be a first-class value with two
//! distinct questions answered explicitly:
//!
//! - [`WorkspaceRoots::primary`] — the FIRST root: the launch identity.
//!   Session sockets, the collab relay, pair records, and the window title
//!   stay keyed on it (the drop-relay precedent in `remote.rs`: identity
//!   follows the launch argument, never the mutable folder set).
//! - [`WorkspaceRoots::owning_root`] — which root contains a given file:
//!   the longest-prefix match, the base every per-root subsystem (watcher,
//!   git worker, Cmd+P index, label rule) resolves against once more than
//!   one root exists.
//!
//! The set is never empty: croft always opens SOMETHING, and "no folder"
//! has no meaning anywhere in the app, so the type makes the invariant
//! unrepresentable instead of sprinkling `Option` through ~40 call sites.

use std::path::{Path, PathBuf};

/// An ordered, never-empty set of workspace root folders. Order is the
/// user's: the first entry is the primary root, later entries render (and
/// resolve) in the order they were added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRoots {
    roots: Vec<PathBuf>,
}

impl WorkspaceRoots {
    /// The single-root workspace every croft session starts as.
    pub fn single(root: PathBuf) -> Self {
        Self { roots: vec![root] }
    }

    /// The first root: the workspace's launch identity, and — while the
    /// set holds one entry — exactly what `workspace_root` meant before
    /// this type existed.
    pub fn primary(&self) -> &Path {
        &self.roots[0]
    }

    /// The root that owns `path`, by LONGEST prefix match — with nested
    /// roots (a repo and a crate inside it both added), the deeper root
    /// wins, matching VS Code's resolution for overlapping folders. `None`
    /// for a path outside every root (out-of-root files keep their
    /// existing behavior: absolute-path display, no per-root features).
    pub fn owning_root(&self, path: &Path) -> Option<&Path> {
        self.roots
            .iter()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.components().count())
            .map(PathBuf::as_path)
    }

    /// Replace the whole set with one root: what a single-root re-root
    /// (Make Root, the zoxide jump, post-clone) means today. Once
    /// add/remove-folder exists this stays the semantic for those
    /// gestures — they change what the window IS, not the folder list.
    pub fn replace_primary(&mut self, root: PathBuf) {
        self.roots = vec![root];
    }

    /// Append `root` to the folder set (Add Folder to Workspace, #147).
    /// A root already present is a no-op; returns whether the set grew.
    pub fn add(&mut self, root: PathBuf) -> bool {
        if self.roots.contains(&root) {
            return false;
        }
        self.roots.push(root);
        true
    }

    /// Drop `root` from the set (Remove Folder from Workspace, #147).
    /// The PRIMARY root is refused — it is the launch identity everything
    /// session-shaped is keyed on; changing it is `replace_primary`'s
    /// job (a re-root), never a removal. Returns whether the set shrank.
    pub fn remove(&mut self, root: &Path) -> bool {
        if self.primary() == root {
            return false;
        }
        let before = self.roots.len();
        self.roots.retain(|r| r != root);
        self.roots.len() < before
    }

    /// Every root in user order; the first is always the primary.
    pub fn iter(&self) -> impl Iterator<Item = &Path> {
        self.roots.iter().map(PathBuf::as_path)
    }

    /// More than one folder: the point where the multi-root label rule
    /// (root-name prefixes) and per-root fan-outs engage.
    pub fn is_multi(&self) -> bool {
        self.roots.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_root_set_answers_primary_with_it() {
        let ws = WorkspaceRoots::single(PathBuf::from("/w/a"));
        assert_eq!(ws.primary(), Path::new("/w/a"));
        assert_eq!(ws.roots, vec![PathBuf::from("/w/a")]);
    }

    #[test]
    fn owning_root_takes_the_longest_prefix_and_refuses_outside_paths() {
        let mut ws = WorkspaceRoots::single(PathBuf::from("/w/repo"));
        ws.roots.push(PathBuf::from("/w/repo/crates/foo"));
        assert_eq!(
            ws.owning_root(Path::new("/w/repo/crates/foo/src/lib.rs")),
            Some(Path::new("/w/repo/crates/foo")),
            "with nested roots the DEEPER root owns the file"
        );
        assert_eq!(
            ws.owning_root(Path::new("/w/repo/README.md")),
            Some(Path::new("/w/repo")),
        );
        assert_eq!(
            ws.owning_root(Path::new("/elsewhere/x.rs")),
            None,
            "a path outside every root belongs to none"
        );
        // Prefix matching is component-wise, never textual: /w/repo2 is
        // not inside /w/repo.
        assert_eq!(ws.owning_root(Path::new("/w/repo2/x.rs")), None);
    }

    #[test]
    fn add_grows_the_set_once_and_remove_refuses_the_primary() {
        let mut ws = WorkspaceRoots::single(PathBuf::from("/w/a"));
        assert!(ws.add(PathBuf::from("/w/b")), "a new folder grows the set");
        assert!(!ws.add(PathBuf::from("/w/b")), "adding it again is a no-op");
        assert!(ws.is_multi());
        assert_eq!(
            ws.iter().collect::<Vec<_>>(),
            vec![Path::new("/w/a"), Path::new("/w/b")]
        );
        assert!(
            !ws.remove(Path::new("/w/a")),
            "the primary root is the launch identity; removal is refused"
        );
        assert!(ws.remove(Path::new("/w/b")), "a secondary root removes");
        assert!(!ws.is_multi());
        assert_eq!(ws.primary(), Path::new("/w/a"));
    }

    #[test]
    fn replace_primary_collapses_the_set_to_the_new_root() {
        let mut ws = WorkspaceRoots::single(PathBuf::from("/w/a"));
        ws.roots.push(PathBuf::from("/w/b"));
        ws.replace_primary(PathBuf::from("/w/c"));
        assert_eq!(ws.primary(), Path::new("/w/c"));
        assert_eq!(
            ws.roots,
            vec![PathBuf::from("/w/c")],
            "a re-root changes what the window IS"
        );
    }
}
