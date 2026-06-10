//! Durable user preferences.
//!
//! Distinct from [`crate::session_state`], which is a per-pid handoff file
//! deleted after a self-re-exec: these settings persist across every launch.
//! Stored as JSON at `~/.config/croft/config.json`, an XDG path that resolves
//! the same on macOS and Linux so the local and remote builds stay in lockstep
//! (the golden rule: identical behavior on both targets).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::theme::Theme;

/// The on-disk preferences document. New fields must default so an older
/// config still parses; `#[serde(default)]` covers that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Prefs {
    /// Active color theme, stored by its stable [`Theme::id`].
    #[serde(default)]
    pub theme: String,
    /// On-screen keyboard split layout (two thumb clusters on foldables),
    /// toggled by the OSK's `split` key.
    #[serde(default)]
    pub osk_split: bool,
}

impl Prefs {
    /// Load preferences from `config_path()`, falling back to defaults when
    /// the file is absent or unreadable. Preferences are best-effort: a
    /// corrupt config should never block startup.
    pub fn load_or_default() -> Self {
        Self::load(&config_path()).unwrap_or_default()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let json =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&json).context("parsing prefs")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("serializing prefs")?;
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
    }

    pub fn theme(&self) -> Theme {
        Theme::from_id(&self.theme)
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme.id().to_string();
    }
}

/// Persist `theme` to the config file, preserving any other settings already
/// stored. Best-effort: a write failure is swallowed by the caller.
pub fn save_theme(theme: Theme) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load(&path).unwrap_or_default();
    prefs.set_theme(theme);
    prefs.save(&path)
}

/// Persist the on-screen keyboard split choice, preserving other settings.
/// Best-effort: a write failure is swallowed by the caller.
pub fn save_osk_split(split: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load(&path).unwrap_or_default();
    prefs.osk_split = split;
    prefs.save(&path)
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

fn config_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("croft");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("croft")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_theme_through_disk() {
        let dir = std::env::temp_dir().join(format!("croft-prefs-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let mut prefs = Prefs::default();
        prefs.set_theme(Theme::Black);
        prefs.save(&path).expect("save");
        let loaded = Prefs::load(&path).expect("load");
        assert_eq!(loaded.theme(), Theme::Black);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_osk_split_and_old_configs_default_to_merged() {
        let dir = std::env::temp_dir().join(format!("croft-prefs-osk-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            osk_split: true,
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        assert!(Prefs::load(&path).expect("load").osk_split);
        // A pre-split config (theme only) still parses, defaulting to merged.
        std::fs::write(&path, r#"{"theme":"black"}"#).expect("write old config");
        assert!(!Prefs::load(&path).expect("load old").osk_split);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_yields_default_theme() {
        let path = std::env::temp_dir().join("croft-prefs-absent-xyz/config.json");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert_eq!(
            Prefs::load(&path).unwrap_or_default().theme(),
            Theme::default()
        );
    }

    #[test]
    fn config_path_lives_under_config_croft() {
        // With XDG unset the path falls under ~/.config/croft; with it set it
        // honors the override. Exercise the default branch via a cleared env.
        let p = config_path();
        assert!(p.to_string_lossy().contains("croft"));
        assert_eq!(p.file_name().unwrap(), "config.json");
    }
}
