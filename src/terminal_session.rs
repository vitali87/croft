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

/// Load the whole map, tolerantly: unreadable or unparsable files are an
/// empty map, never an error.
pub fn load(path: &Path) -> HashMap<String, SessionRecord> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Read-modify-write one workspace's record; a trivial record removes the
/// key instead.
pub fn save_for_root(path: &Path, root: &str, record: SessionRecord) {
    let mut map = load(path);
    if record.is_trivial(root) {
        map.remove(root);
    } else {
        map.insert(root.to_string(), record);
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        save_for_root(&p, "/repo", rec.clone());
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
        );
        assert!(!load(&p).contains_key("/repo"));

        // Corrupt files load as empty, never panic.
        std::fs::write(&p, "not json").unwrap();
        assert!(load(&p).is_empty());
    }
}
