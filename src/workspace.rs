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

/// Display labels for a root set, disambiguated: the folder name alone
/// when unique; colliding names gain their parent (`api (work)` vs
/// `api (archive)`), and a still-colliding pair falls back to the full
/// path — so Quick Open / search prefixes never render two different
/// roots identically (#148 review).
pub fn root_display_labels(roots: &[PathBuf]) -> Vec<String> {
    let names: Vec<String> = roots
        .iter()
        .map(|r| {
            r.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| r.display().to_string())
        })
        .collect();
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            if names.iter().filter(|n| *n == name).count() == 1 {
                return name.clone();
            }
            let with_parent = |idx: usize| {
                roots[idx]
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|s| format!("{} ({})", names[idx], s.to_string_lossy()))
            };
            let mine = with_parent(i);
            let unique = mine.as_ref().is_some_and(|label| {
                names
                    .iter()
                    .enumerate()
                    .filter(|(j, n)| *j != i && *n == name)
                    .all(|(j, _)| with_parent(j).as_ref() != Some(label))
            });
            match mine {
                Some(label) if unique => label,
                _ => roots[i].display().to_string(),
            }
        })
        .collect()
}

/// The persisted folder-set store (#153): `~/.config/croft/
/// workspace_folders.json`, a map from the PRIMARY root's display path to
/// its secondary folders — the `terminal_session.rs` model exactly. An
/// empty list prunes its key, so plain single-folder workspaces never
/// grow the file.
pub fn folders_store_path() -> PathBuf {
    crate::prefs::config_dir().join("workspace_folders.json")
}

/// Load the whole map for READING: a missing file is an empty map (the
/// normal first-run state); any other failure also reads empty, since a
/// read-only consumer can do nothing better.
pub fn load_folders(path: &Path) -> std::collections::HashMap<String, Vec<PathBuf>> {
    load_folders_checked(path).unwrap_or_default()
}

/// Load for WRITING: a missing file is `Ok(empty)`, but an unreadable or
/// unparsable one is an error — the read-modify-write in
/// [`save_folders_for_root`] must never treat a corrupt store as empty
/// and silently replace every other workspace's saved arrangement with
/// just the current one (#156 review).
fn load_folders_checked(
    path: &Path,
) -> Result<std::collections::HashMap<String, Vec<PathBuf>>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// Read-modify-write one primary root's secondary-folder list; an empty
/// list removes the key. Refuses to touch a store it cannot parse, and
/// writes through a sibling temp file renamed into place so an
/// interrupted write can never leave truncated JSON behind.
pub fn save_folders_for_root(
    path: &Path,
    primary: &str,
    folders: Vec<PathBuf>,
) -> Result<(), String> {
    let mut map = load_folders_checked(path)?;
    if folders.is_empty() {
        map.remove(primary);
    } else {
        map.insert(primary.to_string(), folders);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
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
    fn a_corrupt_folders_store_is_refused_not_replaced() {
        // #156 review: treating a corrupt store as empty and writing over
        // it silently discarded EVERY other workspace's arrangement.
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("folders.json");
        std::fs::write(&store, "{ not valid json").unwrap();
        let err = save_folders_for_root(&store, "/w/a", vec![PathBuf::from("/w/b")]);
        assert!(err.is_err(), "a corrupt store must refuse the update");
        assert_eq!(
            std::fs::read_to_string(&store).unwrap(),
            "{ not valid json",
            "the corrupt bytes stay untouched for the user to inspect"
        );
        // A read-only load degrades to empty rather than erroring.
        assert!(load_folders(&store).is_empty());
        // And a MISSING file is the normal first-run state: the save
        // proceeds and round-trips.
        let fresh = tmp.path().join("fresh.json");
        save_folders_for_root(&fresh, "/w/a", vec![PathBuf::from("/w/b")]).unwrap();
        assert_eq!(
            load_folders(&fresh).get("/w/a"),
            Some(&vec![PathBuf::from("/w/b")])
        );
    }

    #[test]
    fn root_labels_disambiguate_same_named_folders() {
        let roots = vec![
            PathBuf::from("/work/api"),
            PathBuf::from("/archive/api"),
            PathBuf::from("/work/web"),
        ];
        assert_eq!(
            root_display_labels(&roots),
            vec!["api (work)", "api (archive)", "web"],
            "colliding folder names gain their parent; unique ones stay bare"
        );
        let twins = vec![PathBuf::from("/x/api"), PathBuf::from("/y/x/api")];
        let labels = root_display_labels(&twins);
        assert_ne!(
            labels[0], labels[1],
            "labels are never identical: {labels:?}"
        );
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
