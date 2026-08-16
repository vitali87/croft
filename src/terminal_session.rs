//! Terminal session restore (iTerm2 / WezTerm / Zellij's headline
//! "never lose my workspace" feature, scoped to croft's terminal panel):
//! the pane layout — count, order, per-pane cwd and manual name, and which
//! pane was active — is persisted per workspace root and resurrected on
//! the next croft launch in that workspace. Panes come back as fresh
//! shells in their directories (running programs are not re-run).
//!
//! One JSON file, `~/.config/croft/terminal_sessions.json`, maps workspace
//! root → [`SessionRecord`]. Saves happen on structural changes (split /
//! close / rename / reorder / undo close) and at quit; a trivial record
//! (one unnamed pane sitting at the workspace root — the default layout)
//! is pruned so plain workspaces never accumulate entries.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One pane to bring back.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaneRecord {
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A workspace's terminal panel layout.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionRecord {
    pub panes: Vec<PaneRecord>,
    #[serde(default)]
    pub active: usize,
}

impl SessionRecord {
    /// The default layout every workspace starts with; storing it would be
    /// noise, so [`save_for_root`] prunes it instead.
    pub fn is_trivial(&self, root: &str) -> bool {
        match self.panes.as_slice() {
            [] => true,
            [only] => only.name.is_none() && same_dir(&only.cwd, root),
            _ => false,
        }
    }
}

/// Directory equality through symlinks: a pane cwd sampled from the
/// process table comes back canonicalised (`/private/var/…` on macOS)
/// while the workspace root string may not be.
fn same_dir(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// The real store path. The App caches it in a field so tests can point
/// saves and restores at a tempdir.
pub fn path() -> PathBuf {
    crate::prefs::config_dir().join("terminal_sessions.json")
}

/// Load the whole map for READING: a missing file is an empty map (the
/// normal first-run state); any other failure also reads empty, since a
/// read-only consumer can do nothing better.
pub fn load(path: &Path) -> HashMap<String, SessionRecord> {
    load_checked(path).unwrap_or_default()
}

/// Load for WRITING: a missing file is `Ok(empty)`, but an unreadable or
/// unparsable one is an error — the read-modify-write in [`save_for_root`]
/// must never treat a corrupt store as empty and silently replace every
/// other workspace's saved layout with just the current one (#157, the
/// workspace-folder store's #156 hazard verbatim).
fn load_checked(path: &Path) -> Result<HashMap<String, SessionRecord>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// Read-modify-write one workspace's record; a trivial record removes the
/// key instead. Runs under [`crate::workspace::update_json_store`]'s
/// exclusive lock (#157/#158): refuses a store that exists but cannot be
/// read or parsed, writes atomically through a unique temp file, and
/// serializes against concurrent croft windows.
pub fn save_for_root(path: &Path, root: &str, record: SessionRecord) -> Result<(), String> {
    crate::workspace::update_json_store::<SessionRecord, _>(path, |map| {
        if record.is_trivial(root) {
            map.remove(root);
        } else {
            map.insert(root.to_string(), record.clone());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_corrupt_store_is_refused_not_replaced() {
        // #157: treating a corrupt store as empty and writing over it
        // silently discarded every other workspace's saved layout.
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("sessions.json");
        std::fs::write(&store, "{ not valid json").unwrap();
        let record = SessionRecord {
            panes: vec![PaneRecord {
                cwd: String::from("/w/a/sub"),
                name: Some(String::from("srv")),
            }],
            active: 0,
        };
        assert!(
            save_for_root(&store, "/w/a", record.clone()).is_err(),
            "a corrupt store must refuse the update"
        );
        assert_eq!(
            std::fs::read_to_string(&store).unwrap(),
            "{ not valid json",
            "the corrupt bytes stay untouched"
        );
        assert!(load(&store).is_empty(), "read-only loads degrade to empty");
        let fresh = tmp.path().join("fresh.json");
        save_for_root(&fresh, "/w/a", record).unwrap();
        assert_eq!(load(&fresh).get("/w/a").map(|r| r.panes.len()), Some(1));
    }

    #[test]
    fn save_round_trips_and_trivial_records_prune() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("s.json");
        let rec = SessionRecord {
            panes: vec![
                PaneRecord {
                    cwd: String::from("/repo"),
                    name: None,
                },
                PaneRecord {
                    cwd: String::from("/repo/sub"),
                    name: Some(String::from("srv")),
                },
            ],
            active: 1,
        };
        save_for_root(&p, "/repo", rec.clone()).unwrap();
        let map = load(&p);
        assert_eq!(map.get("/repo"), Some(&rec));

        // Back to the default single-pane layout: the key is pruned.
        save_for_root(
            &p,
            "/repo",
            SessionRecord {
                panes: vec![PaneRecord {
                    cwd: String::from("/repo"),
                    name: None,
                }],
                active: 0,
            },
        )
        .unwrap();
        assert!(!load(&p).contains_key("/repo"));

        // Corrupt files load as empty, never panic.
        std::fs::write(&p, "not json").unwrap();
        assert!(load(&p).is_empty());
    }
}
