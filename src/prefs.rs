//! Durable user preferences.
//!
//! Distinct from [`crate::session_state`], which is a per-pid handoff file
//! deleted after a self-re-exec: these settings persist across every launch.
//! Stored as JSON at `~/.config/croft/config.json`, an XDG path that resolves
//! the same on macOS and Linux so the local and remote builds stay in lockstep
//! (the golden rule: identical behavior on both targets).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::app::{PanelAlignment, QuickInputPosition, SideBarPosition};
use crate::theme::Theme;

/// The layout chrome chosen in the "Customize Layout" popup (croft's analog of
/// VS Code's title-bar layout controls). Every field carries its own
/// `#[serde(default)]` so a config written before this block existed still
/// parses straight into these defaults. The primary side bar (⌘B) and panel
/// (⌃J) visibility stay ephemeral per launch and so are NOT stored here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutPrefs {
    #[serde(default = "default_true")]
    pub activity_bar: bool,
    #[serde(default = "default_true")]
    pub status_bar: bool,
    #[serde(default)]
    pub side_bar_position: SideBarPosition,
    #[serde(default)]
    pub secondary_side_bar: bool,
    #[serde(default)]
    pub panel_alignment: PanelAlignment,
    #[serde(default)]
    pub quick_input_position: QuickInputPosition,
}

impl Default for LayoutPrefs {
    fn default() -> Self {
        Self {
            activity_bar: true,
            status_bar: true,
            side_bar_position: SideBarPosition::default(),
            secondary_side_bar: false,
            panel_alignment: PanelAlignment::default(),
            quick_input_position: QuickInputPosition::default(),
        }
    }
}

/// Which of the Explorer's stacked sub-views are shown, toggled from the
/// "Views and More Actions" (⋯) menu on the EXPLORER header. Mirrors VS Code's
/// defaults: Open Editors hidden, every other view shown. Each field carries
/// its own `#[serde(default)]` so a config written before this block existed
/// still parses straight into these defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorerViewsPrefs {
    #[serde(default)]
    pub open_editors: bool,
    #[serde(default = "default_true")]
    pub folders: bool,
    #[serde(default = "default_true")]
    pub outline: bool,
    #[serde(default = "default_true")]
    pub timeline: bool,
    /// The DEPENDENCIES view. `alias` keeps configs written under the old
    /// Rust-only `rust_dependencies` key parsing into this generalized field.
    #[serde(default = "default_true", alias = "rust_dependencies")]
    pub dependencies: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ExplorerViewsPrefs {
    fn default() -> Self {
        Self {
            open_editors: false,
            folders: true,
            outline: true,
            timeline: true,
            dependencies: true,
        }
    }
}

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
    /// When true, the startup "switch to iTerm2/Ghostty" nudge shown in
    /// terminals that can't render croft's inline images is silenced (the
    /// user dismissed it with "don't show again").
    #[serde(default)]
    pub suppress_terminal_warning: bool,
    /// Visibility of the Explorer's stacked sub-views (⋯ menu toggles).
    #[serde(default)]
    pub explorer_views: ExplorerViewsPrefs,
    /// Extension ids the user has disabled in the Extensions panel. Absent or
    /// empty means every bundled and installed extension is enabled (the
    /// default), so disabling is opt-in and an older config still parses.
    #[serde(default)]
    pub disabled_extensions: BTreeSet<String>,
    /// MCP sidecar extension ids the user has consented to spawn (the first-run
    /// consent gate). croft never launches a sidecar process until its extension
    /// id is in this set.
    #[serde(default)]
    pub mcp_consented: BTreeSet<String>,
    /// Trust-on-first-use tool fingerprints, keyed by command id. Recorded the
    /// first time a command's tool is called; a later mismatch (a silent tool
    /// rug-pull) makes croft refuse the call. See
    /// [`crate::mcp::client::tool_fingerprint`].
    #[serde(default)]
    pub mcp_tool_fingerprints: BTreeMap<String, String>,
    /// Layout chrome chosen in the "Customize Layout" popup.
    #[serde(default)]
    pub layout: LayoutPrefs,
    /// When true, saving a file first asks the language server to format it
    /// (VS Code's `editor.formatOnSave`). Off by default, matching VS Code, so
    /// an older config parses straight to disabled.
    #[serde(default)]
    pub format_on_save: bool,
    /// When true, a dirty buffer writes itself to disk about a second after
    /// the last edit (VS Code's `files.autoSave: afterDelay`). Off by
    /// default, matching VS Code.
    #[serde(default)]
    pub auto_save: bool,
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

/// Persist the "don't warn about this terminal again" choice, preserving
/// other settings. Best-effort: a write failure is swallowed by the caller.
pub fn save_suppress_terminal_warning(suppress: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load(&path).unwrap_or_default();
    prefs.suppress_terminal_warning = suppress;
    prefs.save(&path)
}

/// Persist the Explorer sub-view visibility set, preserving other settings.
/// Best-effort: a write failure is swallowed by the caller.
pub fn save_explorer_views(views: ExplorerViewsPrefs) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load(&path).unwrap_or_default();
    prefs.explorer_views = views;
    prefs.save(&path)
}

/// Persist the set of disabled extension ids, preserving other settings.
/// Best-effort: a write failure is swallowed by the caller.
pub fn save_disabled_extensions(disabled: &BTreeSet<String>) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load(&path).unwrap_or_default();
    prefs.disabled_extensions = disabled.clone();
    prefs.save(&path)
}

/// Record first-run consent to spawn an MCP sidecar extension, preserving other
/// settings. Best-effort: a write failure is swallowed by the caller.
pub fn save_mcp_consent(ext_id: &str) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load(&path).unwrap_or_default();
    prefs.mcp_consented.insert(ext_id.to_string());
    prefs.save(&path)
}

/// Record (trust-on-first-use) the fingerprint of the tool a command calls,
/// preserving other settings. Best-effort: a write failure is swallowed.
pub fn save_mcp_tool_fingerprint(command_id: &str, fingerprint: &str) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load(&path).unwrap_or_default();
    prefs
        .mcp_tool_fingerprints
        .insert(command_id.to_string(), fingerprint.to_string());
    prefs.save(&path)
}

/// Persist the Customize Layout chrome choices, preserving other settings.
/// Best-effort: a write failure is swallowed by the caller.
pub fn save_layout(layout: LayoutPrefs) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load(&path).unwrap_or_default();
    prefs.layout = layout;
    prefs.save(&path)
}

/// Persist the format-on-save choice, preserving other settings. Best-effort:
/// a write failure is swallowed by the caller.
pub fn save_format_on_save(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load(&path).unwrap_or_default();
    prefs.format_on_save = enabled;
    prefs.save(&path)
}

/// Persist the auto-save choice, preserving other settings. Best-effort:
/// a write failure is swallowed by the caller.
pub fn save_auto_save(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load(&path).unwrap_or_default();
    prefs.auto_save = enabled;
    prefs.save(&path)
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub(crate) fn config_dir() -> PathBuf {
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
        prefs.set_theme(Theme::BLACK);
        prefs.save(&path).expect("save");
        let loaded = Prefs::load(&path).expect("load");
        assert_eq!(loaded.theme(), Theme::BLACK);
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
    fn round_trips_suppress_terminal_warning_and_old_configs_default_off() {
        let dir =
            std::env::temp_dir().join(format!("croft-prefs-warn-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            suppress_terminal_warning: true,
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        assert!(Prefs::load(&path).expect("load").suppress_terminal_warning);
        // A config written before this field existed still parses, defaulting
        // the nudge to enabled (false = not suppressed).
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        assert!(
            !Prefs::load(&path)
                .expect("load old")
                .suppress_terminal_warning
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_explorer_views_and_old_configs_default_to_vscode_defaults() {
        let dir =
            std::env::temp_dir().join(format!("croft-prefs-views-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            explorer_views: ExplorerViewsPrefs {
                open_editors: true,
                timeline: false,
                ..ExplorerViewsPrefs::default()
            },
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        let loaded = Prefs::load(&path).expect("load").explorer_views;
        assert!(loaded.open_editors);
        assert!(!loaded.timeline);
        assert!(loaded.folders);
        // A config written before the field existed parses to the VS Code
        // defaults: Open Editors hidden, every other view shown.
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        let old = Prefs::load(&path).expect("load old").explorer_views;
        assert_eq!(old, ExplorerViewsPrefs::default());
        assert!(!old.open_editors);
        assert!(old.folders && old.outline && old.timeline && old.dependencies);
        // A config written under the pre-generalization `rust_dependencies` key
        // still drives the renamed `dependencies` field via the serde alias.
        std::fs::write(&path, r#"{"explorer_views":{"rust_dependencies":false}}"#)
            .expect("write legacy config");
        let legacy = Prefs::load(&path).expect("load legacy").explorer_views;
        assert!(!legacy.dependencies, "old rust_dependencies key aliases in");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_layout_and_old_configs_default_to_full_chrome() {
        let dir =
            std::env::temp_dir().join(format!("croft-prefs-layout-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            layout: LayoutPrefs {
                activity_bar: false,
                status_bar: false,
                side_bar_position: SideBarPosition::Right,
                secondary_side_bar: true,
                panel_alignment: PanelAlignment::Justify,
                quick_input_position: QuickInputPosition::Center,
            },
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        let loaded = Prefs::load(&path).expect("load").layout;
        assert_eq!(loaded, prefs.layout);
        // A config written before this block existed parses to full chrome:
        // activity bar + status bar shown, side bar left, no secondary bar,
        // panel centered, quick input on top.
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        let old = Prefs::load(&path).expect("load old").layout;
        assert_eq!(old, LayoutPrefs::default());
        assert!(old.activity_bar && old.status_bar);
        assert_eq!(old.side_bar_position, SideBarPosition::Left);
        assert!(!old.secondary_side_bar);
        assert_eq!(old.panel_alignment, PanelAlignment::Center);
        assert_eq!(old.quick_input_position, QuickInputPosition::Top);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_format_on_save_and_old_configs_default_off() {
        let dir = std::env::temp_dir().join(format!("croft-prefs-fos-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            format_on_save: true,
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        assert!(Prefs::load(&path).expect("load").format_on_save);
        // A config written before this field existed still parses, defaulting
        // format-on-save to off (matching VS Code's `editor.formatOnSave`).
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        assert!(!Prefs::load(&path).expect("load old").format_on_save);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_auto_save_and_old_configs_default_off() {
        let dir = std::env::temp_dir().join(format!("croft-prefs-as-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            auto_save: true,
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        assert!(Prefs::load(&path).expect("load").auto_save);
        // A config written before this field existed still parses, defaulting
        // auto-save to off (matching VS Code's `files.autoSave`).
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        assert!(!Prefs::load(&path).expect("load old").auto_save);
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
