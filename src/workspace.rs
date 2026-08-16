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
/// list removes the key. Runs under [`update_json_store`]'s exclusive
/// lock: refuses a store it cannot parse and writes atomically.
pub fn save_folders_for_root(
    path: &Path,
    primary: &str,
    folders: Vec<PathBuf>,
) -> Result<(), String> {
    update_json_store::<Vec<PathBuf>, _>(path, |map| {
        if folders.is_empty() {
            map.remove(primary);
        } else {
            map.insert(primary.to_string(), folders.clone());
        }
    })
}

/// The shared read-modify-write for croft's per-workspace JSON stores
/// (`workspace_folders.json`, `terminal_sessions.json`): the whole
/// load→mutate→rename transaction holds an exclusive lock on a sibling
/// `.lock` file, so two croft windows saving concurrently serialize
/// instead of overwriting each other's keys (#158 review); the write
/// lands through a per-process-unique temp file renamed into place; a
/// store that exists but cannot be read or parsed refuses the update
/// and keeps its bytes, while a missing store is the normal first run.
pub(crate) fn update_json_store<V, F>(path: &Path, mutate: F) -> Result<(), String>
where
    V: serde::Serialize + serde::de::DeserializeOwned,
    F: FnOnce(&mut std::collections::HashMap<String, V>),
{
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let lock_path = path.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open {}: {e}", lock_path.display()))?;
    lock.lock()
        .map_err(|e| format!("lock {}: {e}", lock_path.display()))?;
    // Held to end of scope; unlock on drop covers every early return.
    let mut map: std::collections::HashMap<String, V> = match std::fs::read_to_string(path) {
        Ok(raw) => {
            serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    mutate(&mut map);
    let json = serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

/// Parse a VS Code-compatible `.code-workspace` file (#163): tolerant of
/// `//` comments (the keybindings.json treatment), `folders[].path`
/// resolved against the file's directory, `name` and `settings`/`launch`/
/// `tasks` sections ignored (croft's settings are global). Returns the
/// resolved folder list in file order; empty folders is an error — a
/// workspace names at least one folder.
pub fn parse_workspace_file(path: &Path) -> Result<Vec<PathBuf>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let stripped = strip_jsonc(&raw);
    let v: serde_json::Value =
        serde_json::from_str(&stripped).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let folders = v
        .get("folders")
        .and_then(|f| f.as_array())
        .ok_or_else(|| format!("{}: no folders array", path.display()))?;
    let mut out = Vec::new();
    for f in folders {
        // An entry without a string `path` is a malformed FILE, not a
        // skippable row: silently dropping it would open a partial
        // workspace that looks complete (#164 review).
        let p = f
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| format!("{}: folder entry without a path", path.display()))?;
        let p = Path::new(p);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            base.join(p)
        };
        out.push(abs.canonicalize().unwrap_or(abs));
    }
    if out.is_empty() {
        return Err(format!("{}: no folders", path.display()));
    }
    Ok(out)
}

/// Strip VS Code's JSONC extras so serde can parse: `//` and `/* */`
/// comments outside strings, and trailing commas before `]`/`}` (#164
/// review — `.code-workspace` files legitimately carry all three).
fn strip_jsonc(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut in_str = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
                i += 1;
            }
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            ',' => {
                // Trailing comma: swallow it when the next non-space,
                // non-comment token closes the container.
                let mut j = i + 1;
                loop {
                    while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                        j += 1;
                    }
                    if j + 1 < bytes.len() && bytes[j] == b'/' && bytes[j + 1] == b'/' {
                        while j < bytes.len() && bytes[j] != b'\n' {
                            j += 1;
                        }
                        continue;
                    }
                    if j + 1 < bytes.len() && bytes[j] == b'/' && bytes[j + 1] == b'*' {
                        j += 2;
                        while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                            j += 1;
                        }
                        j = (j + 2).min(bytes.len());
                        continue;
                    }
                    break;
                }
                if j < bytes.len() && (bytes[j] == b']' || bytes[j] == b'}') {
                    i += 1; // drop the comma; the closer re-processes
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Write the folder set as a `.code-workspace` (#163): paths relative to
/// the file's directory where possible — VS Code's recommendation, so the
/// file survives being moved with its folders — absolute otherwise.
pub fn write_workspace_file(path: &Path, folders: &[PathBuf]) -> Result<(), String> {
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let base_canon = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let entries: Vec<serde_json::Value> = folders
        .iter()
        .map(|f| serde_json::json!({ "path": relative_or_absolute(&base_canon, f) }))
        .collect();
    let doc = serde_json::json!({ "folders": entries });
    let json = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

/// `target` relative to `base` with `..` steps where they help (the
/// file's own directory becomes `.`), or the absolute path when the two
/// share no useful prefix (different mount points, or a walk that would
/// be all `..`): VS Code's recommendation is relative folders so a
/// workspace file survives moving with its folders.
fn relative_or_absolute(base: &Path, target: &Path) -> String {
    let base: Vec<_> = base.components().collect();
    let tgt: Vec<_> = target.components().collect();
    let common = base
        .iter()
        .zip(tgt.iter())
        .take_while(|(a, b)| a == b)
        .count();
    if common <= 1 {
        // Only the root (or nothing) shared: relative would be noise.
        return target.display().to_string();
    }
    let ups = base.len() - common;
    let mut out = PathBuf::new();
    for _ in 0..ups {
        out.push("..");
    }
    for c in &tgt[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        return String::from(".");
    }
    out.display().to_string()
}

/// True when `path` names a VS Code workspace file.
pub fn is_workspace_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "code-workspace")
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
    fn workspace_file_round_trips_with_relative_paths_and_tolerates_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("alpha");
        let b = tmp.path().join("beta");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let file = tmp.path().join("proj.code-workspace");
        write_workspace_file(&file, &[a.clone(), b.clone()]).unwrap();
        let raw = std::fs::read_to_string(&file).unwrap();
        assert!(
            raw.contains("\"alpha\"") && raw.contains("\"beta\""),
            "folders under the file's dir are written RELATIVE: {raw}"
        );
        let parsed = parse_workspace_file(&file).unwrap();
        assert_eq!(
            parsed,
            vec![a.canonicalize().unwrap(), b.canonicalize().unwrap()]
        );

        // A file INSIDE a folder still relativizes: itself as `.`, the
        // sibling through `..` — VS Code's shareable shape.
        let inside = a.join("self.code-workspace");
        write_workspace_file(
            &inside,
            &[a.canonicalize().unwrap(), b.canonicalize().unwrap()],
        )
        .unwrap();
        let raw2 = std::fs::read_to_string(&inside).unwrap();
        assert!(raw2.contains("\".\""), "own dir is '.': {raw2}");
        assert!(raw2.contains("../beta"), "sibling uses ..: {raw2}");
        assert_eq!(
            parse_workspace_file(&inside).unwrap(),
            vec![a.canonicalize().unwrap(), b.canonicalize().unwrap()]
        );

        // Comments and absolute paths parse too; order is preserved.
        let commented = tmp.path().join("c.code-workspace");
        std::fs::write(
            &commented,
            format!(
                "{{\n  // the arrangement\n  \"folders\": [\n    {{ \"path\": \"{}\" }},\n    {{ \"path\": \"alpha\" }}\n  ]\n}}\n",
                b.display()
            ),
        )
        .unwrap();
        let parsed = parse_workspace_file(&commented).unwrap();
        assert_eq!(
            parsed,
            vec![b.canonicalize().unwrap(), a.canonicalize().unwrap()]
        );

        // Malformed: a real error, never an empty default.
        let bad = tmp.path().join("bad.code-workspace");
        std::fs::write(&bad, "{ nope").unwrap();
        assert!(parse_workspace_file(&bad).is_err());
        assert!(is_workspace_file(&bad));
        assert!(!is_workspace_file(Path::new("x.rs")));

        // VS Code JSONC: block comments and trailing commas parse (#164
        // review), and an entry without a string path REJECTS the file
        // rather than opening a partial workspace.
        let jsonc = tmp.path().join("jsonc.code-workspace");
        std::fs::write(
            &jsonc,
            "{\n  /* block\n     comment */\n  \"folders\": [\n    { \"path\": \"alpha\" }, // tail\n    { \"path\": \"beta\" },\n  ],\n}\n",
        )
        .unwrap();
        assert_eq!(
            parse_workspace_file(&jsonc).unwrap(),
            vec![a.canonicalize().unwrap(), b.canonicalize().unwrap()]
        );
        let mixed = tmp.path().join("mixed.code-workspace");
        std::fs::write(
            &mixed,
            "{ \"folders\": [ { \"path\": \"alpha\" }, { \"name\": \"nope\" } ] }",
        )
        .unwrap();
        assert!(
            parse_workspace_file(&mixed).is_err(),
            "an entry without a path rejects the whole file"
        );
    }

    #[test]
    fn concurrent_saves_serialize_instead_of_overwriting_each_other() {
        // #158 review: without the lock, two writers could each load the
        // pre-update map and the second rename would discard the first
        // writer's key.
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("folders.json");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    save_folders_for_root(
                        &store,
                        &format!("/w/root{i}"),
                        vec![PathBuf::from(format!("/w/extra{i}"))],
                    )
                    .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let map = load_folders(&store);
        assert_eq!(map.len(), 8, "every writer's key survives: {map:?}");
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
