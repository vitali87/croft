use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One open editor tab, captured so a re-exec into a freshly-installed
/// croft binary can reopen the file at the same cursor + scroll position.
/// `unsaved_text` is `Some` only when the buffer is dirty, so an in-place
/// update never silently drops uncommitted edits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenTabState {
    pub path: PathBuf,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub scroll: usize,
    pub scroll_col: usize,
    pub dirty: bool,
    pub unsaved_text: Option<String>,
}

/// The slice of running-croft state worth carrying across a self-re-exec.
///
/// Terminal panes are deliberately ABSENT, and that is not an oversight: the
/// per-workspace terminal store (`save_terminal_session` /
/// `restore_terminal_session`) already records pane cwds AND names, is written
/// before the re-exec, and is replayed at startup ahead of this handoff.
/// Duplicating panes here would append a second copy on top of the ones that
/// store already restored (#249 review).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub workspace_root: PathBuf,
    pub tabs: Vec<OpenTabState>,
    pub active_tab: usize,
    pub sidebar_view: String,
    pub sidebar_width: u16,
    pub terminal_height: Option<u16>,
    pub focus_editor: bool,
}

impl SessionState {
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let json = serde_json::to_string(self).context("serializing session state")?;
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let json =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&json).context("parsing session state")
    }
}

/// Per-pid scratch path for the session handoff file. Pid-scoping keeps
/// two concurrent croft sessions on the same remote from clobbering each
/// other's restore file.
pub fn handoff_path() -> PathBuf {
    let base = dirs_cache_croft();
    base.join(format!("session-{}.json", std::process::id()))
}

pub(crate) fn dirs_cache_croft() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("croft")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("croft-session-test-{}", std::process::id()));
        let path = dir.join("state.json");
        let state = SessionState {
            workspace_root: PathBuf::from("/work/repo"),
            tabs: vec![
                OpenTabState {
                    path: PathBuf::from("/work/repo/src/main.rs"),
                    cursor_row: 12,
                    cursor_col: 4,
                    scroll: 3,
                    scroll_col: 0,
                    dirty: false,
                    unsaved_text: None,
                },
                OpenTabState {
                    path: PathBuf::from("/work/repo/README.md"),
                    cursor_row: 0,
                    cursor_col: 0,
                    scroll: 0,
                    scroll_col: 0,
                    dirty: true,
                    unsaved_text: Some(String::from("edited\nbody")),
                },
            ],
            active_tab: 1,
            sidebar_view: String::from("Explorer"),
            sidebar_width: 32,
            terminal_height: Some(14),
            focus_editor: true,
        };
        state.save(&path).expect("save");
        let loaded = SessionState::load(&path).expect("load");
        assert_eq!(state, loaded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handoff_path_is_pid_scoped_under_cache_croft() {
        let p = handoff_path();
        assert!(p.to_string_lossy().contains(".cache/croft"));
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("session-")
        );
    }

    /// #249: the handoff must NOT carry terminal panes. The per-workspace
    /// store already restores them (with names) and runs first at startup, so
    /// panes here would be appended on top of those — 2 panes becoming 3.
    /// Serialising the struct and asserting the key is absent pins that.
    #[test]
    fn the_handoff_does_not_carry_terminal_panes() {
        let state = SessionState {
            workspace_root: PathBuf::from("/work/repo"),
            tabs: Vec::new(),
            active_tab: 0,
            sidebar_view: String::from("Explorer"),
            sidebar_width: 32,
            terminal_height: None,
            focus_editor: true,
        };
        let json = serde_json::to_string(&state).expect("serialize");
        assert!(
            !json.contains("terminals"),
            "panes belong to the per-workspace store, not the handoff: {json}"
        );
    }
}
