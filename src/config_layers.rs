//! Layered settings (#251): one merged view over a chain of config files.
//!
//! [`crate::prefs`] knows a single `~/.config/croft/config.json`. This module
//! stacks layers over it, later layers winning per key:
//!
//! 1. built-in defaults (what `Prefs::default()` says)
//! 2. `~/.config/croft/config.json` — user
//! 3. `~/.config/croft/config.local.json` — machine-local user overrides,
//!    dotfile-repo friendly
//! 4. `<root>/.vscode/settings.json` — a small mapped subset, so existing
//!    repos behave sensibly (never broad VS Code compatibility)
//! 5. `<root>/.croft/config.json` — workspace, committed
//! 6. `<root>/.croft/config.local.json` — workspace-local, gitignored
//!
//! Mechanics: every layer may `extends` other JSON files (relative to itself
//! or `~`-expanded; cycles warn and stop), and may carry `"macos"` /
//! `"linux"` / `"android"` blocks that merge over its flat keys on the
//! matching platform. Objects deep-merge; arrays and scalars replace.
//!
//! Workspace layers (4–6) are repo-controlled input, so they pass an explicit
//! allowlist: keys that change trust posture or execute things
//! (`mcp_consented`, `disabled_extensions`, …) are refused with a visible
//! warning and never merged. The user layers can set everything.
//!
//! Everything here is pure over the filesystem (paths in, merged view +
//! warnings out); the App owns applying the result and watching the chain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::prefs::Prefs;

/// Which layer a value came from, for provenance display in the settings hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LayerKind {
    Default,
    User,
    UserLocal,
    VsCodeWorkspace,
    Workspace,
    WorkspaceLocal,
}

impl LayerKind {
    pub fn label(self) -> &'static str {
        match self {
            LayerKind::Default => "default",
            LayerKind::User => "user",
            LayerKind::UserLocal => "user-local",
            LayerKind::VsCodeWorkspace => ".vscode",
            LayerKind::Workspace => "workspace",
            LayerKind::WorkspaceLocal => "workspace-local",
        }
    }

    /// Repo-controlled layers get the allowlist; user layers do not.
    fn is_workspace(self) -> bool {
        matches!(
            self,
            LayerKind::VsCodeWorkspace | LayerKind::Workspace | LayerKind::WorkspaceLocal
        )
    }
}

/// The merged result: the effective preferences, which layer won each
/// top-level key, every file that participated (for hot-reload watching),
/// and human-readable warnings (parse failures, refused keys, cycles).
#[derive(Debug, Clone)]
pub struct MergedConfig {
    pub prefs: Prefs,
    pub provenance: BTreeMap<String, LayerKind>,
    /// Layer files and their `extends` targets, existing or not, in merge
    /// order. A save to any of these should trigger a re-merge.
    pub chain: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

/// The layer that set `key` per a provenance map, defaulting to
/// [`LayerKind::Default`]. Free function so the App can query the map it
/// stored without keeping the whole merged view around.
pub fn layer_of(provenance: &BTreeMap<String, LayerKind>, key: &str) -> LayerKind {
    provenance.get(key).copied().unwrap_or(LayerKind::Default)
}

/// Keys a repo-controlled layer may set: appearance and editor/terminal
/// behavior toggles. Everything else — extension enablement, MCP consent and
/// fingerprints, host accents, machine-state flags — is user-config only,
/// because a cloned repo must not be able to change what croft trusts or
/// executes. Extending this list is a deliberate review decision.
pub const WORKSPACE_ALLOWED_KEYS: &[&str] = &[
    "theme",
    "format_on_save",
    "auto_save",
    "auto_save_on_focus_change",
    "render_whitespace",
    "disable_inline_blame",
    "disable_auto_close_pairs",
    "disable_inline_values",
    "disable_bracket_colors",
    "disable_indent_guides",
    "disable_inlay_hints",
    "copy_on_select",
    "explorer_views",
];

/// Path of the workspace layer files under a root.
pub fn workspace_config_path(root: &Path) -> PathBuf {
    root.join(".croft").join("config.json")
}

pub fn workspace_local_config_path(root: &Path) -> PathBuf {
    root.join(".croft").join("config.local.json")
}

/// Path of the machine-local user layer.
pub fn user_local_config_path() -> PathBuf {
    crate::prefs::config_dir().join("config.local.json")
}

/// Load the full merged view for a workspace (or just the user layers when
/// `workspace_root` is `None`).
pub fn load_merged(workspace_root: Option<&Path>) -> MergedConfig {
    load_merged_from(
        &crate::prefs::config_dir(),
        workspace_root,
        current_platform(),
    )
}

/// The platform key active on this build, matching the scope-block names.
pub fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        "linux"
    }
}

const PLATFORM_KEYS: &[&str] = &["macos", "linux", "android"];

/// Testable core: explicit user config dir and platform.
pub fn load_merged_from(
    user_dir: &Path,
    workspace_root: Option<&Path>,
    platform: &str,
) -> MergedConfig {
    let mut layers: Vec<(LayerKind, PathBuf)> = vec![
        (LayerKind::User, user_dir.join("config.json")),
        (LayerKind::UserLocal, user_dir.join("config.local.json")),
    ];
    if let Some(root) = workspace_root {
        layers.push((
            LayerKind::VsCodeWorkspace,
            root.join(".vscode").join("settings.json"),
        ));
        layers.push((LayerKind::Workspace, workspace_config_path(root)));
        layers.push((LayerKind::WorkspaceLocal, workspace_local_config_path(root)));
    }

    let mut warnings = Vec::new();
    let mut chain = Vec::new();
    let mut merged = Map::new();
    let mut provenance = BTreeMap::new();

    for (kind, path) in layers {
        chain.push(path.clone());
        let doc = if kind == LayerKind::VsCodeWorkspace {
            load_vscode_subset(&path)
        } else {
            let mut visited = Vec::new();
            load_layer_document(&path, platform, &mut visited, &mut chain, &mut warnings)
        };
        let Some(doc) = doc else { continue };
        for (key, value) in doc {
            if kind.is_workspace() && !WORKSPACE_ALLOWED_KEYS.contains(&key.as_str()) {
                warnings.push(format!(
                    "{}: \"{key}\" is user-config only and was ignored (workspace layers may set: appearance and editor toggles)",
                    path.display()
                ));
                continue;
            }
            deep_merge(merged.entry(key.clone()).or_insert(Value::Null), &value);
            provenance.insert(key, kind);
        }
    }

    let prefs = deserialize_tolerantly(merged, &mut provenance, &mut warnings);
    MergedConfig {
        prefs,
        provenance,
        chain,
        warnings,
    }
}

/// Read one croft layer file: JSONC-tolerant parse, `extends` resolution
/// (depth-first, cycles warn and stop), then this platform's scope block
/// merged over the flat keys. `None` when the file is absent or unreadable.
fn load_layer_document(
    path: &Path,
    platform: &str,
    visited: &mut Vec<PathBuf>,
    chain: &mut Vec<PathBuf>,
    warnings: &mut Vec<String>,
) -> Option<Map<String, Value>> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if visited.contains(&canonical) {
        warnings.push(format!(
            "{}: extends cycle detected — this file is already in the chain and was skipped",
            path.display()
        ));
        return None;
    }
    visited.push(canonical);
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: Value = match serde_json::from_str(&crate::tasks::strip_jsonc(&text)) {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!(
                "{}: not valid JSON ({e}) — layer ignored",
                path.display()
            ));
            return None;
        }
    };
    let Value::Object(mut doc) = parsed else {
        warnings.push(format!(
            "{}: expected a JSON object at the top level — layer ignored",
            path.display()
        ));
        return None;
    };

    // `extends`: bases merge first (in listed order), then this file's own
    // keys over them.
    let mut acc = Map::new();
    if let Some(ext) = doc.remove("extends") {
        let bases: Vec<String> = match ext {
            Value::String(s) => vec![s],
            Value::Array(a) => a
                .into_iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            _ => {
                warnings.push(format!(
                    "{}: \"extends\" must be a path or an array of paths — ignored",
                    path.display()
                ));
                Vec::new()
            }
        };
        for base in bases {
            let base_path = resolve_extends_path(&base, path);
            chain.push(base_path.clone());
            if !base_path.exists() {
                warnings.push(format!(
                    "{}: extends {} which does not exist",
                    path.display(),
                    base_path.display()
                ));
                continue;
            }
            if let Some(base_doc) =
                load_layer_document(&base_path, platform, visited, chain, warnings)
            {
                for (k, v) in base_doc {
                    deep_merge(acc.entry(k).or_insert(Value::Null), &v);
                }
            }
        }
    }

    // Platform scope blocks: strip all three, merge the matching one over
    // the flat keys.
    let mut scoped = None;
    for key in PLATFORM_KEYS {
        let block = doc.remove(*key);
        if *key == platform {
            scoped = block;
        }
    }
    for (k, v) in doc {
        deep_merge(acc.entry(k).or_insert(Value::Null), &v);
    }
    if let Some(Value::Object(block)) = scoped {
        for (k, v) in block {
            deep_merge(acc.entry(k).or_insert(Value::Null), &v);
        }
    }
    Some(acc)
}

/// Resolve an `extends` target: `~/` expands to the home directory, absolute
/// paths are taken as written, relative paths resolve against the extending
/// file's own directory.
fn resolve_extends_path(target: &str, extending_file: &Path) -> PathBuf {
    if let Some(rest) = target.strip_prefix("~/") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        return home.join(rest);
    }
    let p = PathBuf::from(target);
    if p.is_absolute() {
        p
    } else {
        extending_file.parent().unwrap_or(Path::new(".")).join(p)
    }
}

/// Deep-merge `over` into `base`: objects merge key-wise, everything else
/// (arrays included) replaces. The documented rule — arrays replace so a
/// workspace can *narrow* a list rather than only ever append.
fn deep_merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                deep_merge(b.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

/// The mapped `.vscode/settings.json` subset: only settings with an exact
/// croft equivalent, silently ignoring the rest (broad VS Code settings
/// compatibility is explicitly a non-goal).
fn load_vscode_subset(path: &Path) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&crate::tasks::strip_jsonc(&text)).ok()?;
    let obj = parsed.as_object()?;
    let mut out = Map::new();
    if let Some(v) = obj.get("editor.formatOnSave").and_then(Value::as_bool) {
        out.insert("format_on_save".into(), Value::Bool(v));
    }
    match obj.get("files.autoSave").and_then(Value::as_str) {
        Some("afterDelay") => {
            out.insert("auto_save".into(), Value::Bool(true));
        }
        Some("onFocusChange") => {
            out.insert("auto_save_on_focus_change".into(), Value::Bool(true));
        }
        Some("off") => {
            out.insert("auto_save".into(), Value::Bool(false));
            out.insert("auto_save_on_focus_change".into(), Value::Bool(false));
        }
        _ => {}
    }
    (!out.is_empty()).then_some(out)
}

/// Deserialize the merged map into [`Prefs`]. A single mistyped key must not
/// nuke every other setting, so on failure each top-level key is re-tried in
/// isolation and the offenders are dropped with a warning.
fn deserialize_tolerantly(
    merged: Map<String, Value>,
    provenance: &mut BTreeMap<String, LayerKind>,
    warnings: &mut Vec<String>,
) -> Prefs {
    match serde_json::from_value(Value::Object(merged.clone())) {
        Ok(p) => p,
        Err(_) => {
            let mut clean = Map::new();
            for (key, value) in merged {
                let mut probe = Map::new();
                probe.insert(key.clone(), value.clone());
                if serde_json::from_value::<Prefs>(Value::Object(probe)).is_ok() {
                    clean.insert(key, value);
                } else {
                    warnings.push(format!(
                        "setting \"{key}\" has the wrong type and was ignored"
                    ));
                    provenance.remove(&key);
                }
            }
            serde_json::from_value(Value::Object(clean)).unwrap_or_default()
        }
    }
}

/// Make sure `<root>/.croft/.gitignore` ignores the workspace-local layer,
/// creating the directory and file as needed. Called before the local layer
/// is first opened for editing so it can never land in a commit by accident.
pub fn ensure_workspace_local_ignored(root: &Path) -> std::io::Result<()> {
    let dir = root.join(".croft");
    std::fs::create_dir_all(&dir)?;
    let gi = dir.join(".gitignore");
    let existing = std::fs::read_to_string(&gi).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == "config.local.json") {
        return Ok(());
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str("config.local.json\n");
    std::fs::write(&gi, updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A tempdir with a user config dir and a workspace root inside it.
    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        (tmp, user, root)
    }

    #[test]
    fn later_layers_win_and_provenance_tracks_the_winner() {
        let (_tmp, user, root) = setup();
        write(
            &user.join("config.json"),
            r#"{"theme":"dark","format_on_save":true}"#,
        );
        write(&user.join("config.local.json"), r#"{"theme":"black"}"#);
        write(
            &root.join(".croft/config.json"),
            r#"{"theme":"nord","auto_save":true}"#,
        );
        write(
            &root.join(".croft/config.local.json"),
            r#"{"theme":"dracula"}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert_eq!(m.prefs.theme, "dracula");
        assert!(
            m.prefs.format_on_save,
            "user layer survives under overrides"
        );
        assert!(m.prefs.auto_save, "workspace layer applies");
        assert_eq!(layer_of(&m.provenance, "theme"), LayerKind::WorkspaceLocal);
        assert_eq!(layer_of(&m.provenance, "format_on_save"), LayerKind::User);
        assert_eq!(layer_of(&m.provenance, "auto_save"), LayerKind::Workspace);
        assert_eq!(
            layer_of(&m.provenance, "copy_on_select"),
            LayerKind::Default
        );
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    #[test]
    fn workspace_layers_cannot_touch_trust_or_execution_settings() {
        let (_tmp, user, root) = setup();
        write(
            &user.join("config.json"),
            r#"{"disabled_extensions":["dap-python"]}"#,
        );
        write(
            &root.join(".croft/config.json"),
            r#"{"theme":"nord","mcp_consented":["evil"],"disabled_extensions":[],"host_accents":[{"pattern":"*"}]}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert_eq!(m.prefs.theme, "nord", "allowlisted key applies");
        assert!(
            m.prefs.mcp_consented.is_empty(),
            "a cloned repo must not grant MCP consent"
        );
        assert!(
            m.prefs.disabled_extensions.contains("dap-python"),
            "the user's own value survives the refused workspace override"
        );
        assert!(m.prefs.host_accents.is_empty());
        let joined = m.warnings.join("\n");
        assert!(joined.contains("mcp_consented"), "{joined}");
        assert!(joined.contains("disabled_extensions"), "{joined}");
        assert!(joined.contains("host_accents"), "{joined}");
    }

    #[test]
    fn extends_composes_and_cycles_warn_instead_of_hanging() {
        let (_tmp, user, root) = setup();
        write(
            &user.join("base.json"),
            r#"{"format_on_save":true,"theme":"nord"}"#,
        );
        write(
            &user.join("config.json"),
            r#"{"extends":"base.json","theme":"black"}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(m.prefs.format_on_save, "inherited from the base");
        assert_eq!(m.prefs.theme, "black", "own keys beat the base");
        assert!(m.chain.iter().any(|p| p.ends_with("base.json")));

        // A cycle: a -> b -> a. Must terminate with a warning.
        write(
            &user.join("a.json"),
            r#"{"extends":"b.json","auto_save":true}"#,
        );
        write(&user.join("b.json"), r#"{"extends":"a.json"}"#);
        write(&user.join("config.json"), r#"{"extends":"a.json"}"#);
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(m.prefs.auto_save, "keys before the cycle still merge");
        assert!(
            m.warnings.iter().any(|w| w.contains("cycle")),
            "{:?}",
            m.warnings
        );
    }

    #[test]
    fn platform_blocks_apply_only_on_their_platform() {
        let (_tmp, user, root) = setup();
        write(
            &user.join("config.json"),
            r#"{"theme":"dark","linux":{"theme":"nord"},"macos":{"format_on_save":true}}"#,
        );
        let linux = load_merged_from(&user, Some(&root), "linux");
        assert_eq!(linux.prefs.theme, "nord");
        assert!(!linux.prefs.format_on_save);
        let mac = load_merged_from(&user, Some(&root), "macos");
        assert_eq!(mac.prefs.theme, "dark");
        assert!(mac.prefs.format_on_save);
    }

    #[test]
    fn objects_deep_merge_and_arrays_replace() {
        let (_tmp, user, root) = setup();
        write(
            &user.join("config.json"),
            r#"{"explorer_views":{"timeline":false},"host_accents":[{"pattern":"prod-*"},{"pattern":"stage-*"}]}"#,
        );
        write(
            &user.join("config.local.json"),
            r#"{"explorer_views":{"outline":false},"host_accents":[{"pattern":"dev-*"}]}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(!m.prefs.explorer_views.timeline, "user key survives");
        assert!(!m.prefs.explorer_views.outline, "local key merges in");
        assert!(
            m.prefs.explorer_views.folders,
            "untouched keys keep defaults"
        );
        assert_eq!(
            m.prefs.host_accents.len(),
            1,
            "arrays replace wholesale, they never concatenate"
        );
        assert_eq!(m.prefs.host_accents[0].pattern, "dev-*");
    }

    #[test]
    fn vscode_settings_map_a_small_subset_and_lose_to_croft_layers() {
        let (_tmp, user, root) = setup();
        write(
            &root.join(".vscode/settings.json"),
            r#"{
  // JSONC is fine here too
  "editor.formatOnSave": true,
  "files.autoSave": "afterDelay",
  "editor.fontSize": 13,
}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(m.prefs.format_on_save);
        assert!(m.prefs.auto_save);
        assert_eq!(
            layer_of(&m.provenance, "format_on_save"),
            LayerKind::VsCodeWorkspace
        );

        // The croft workspace layer sits above the mapped subset.
        write(
            &root.join(".croft/config.json"),
            r#"{"format_on_save":false}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(!m.prefs.format_on_save);
        assert_eq!(
            layer_of(&m.provenance, "format_on_save"),
            LayerKind::Workspace
        );
    }

    #[test]
    fn a_mistyped_key_is_dropped_alone_with_a_warning() {
        let (_tmp, user, root) = setup();
        write(
            &user.join("config.json"),
            r#"{"theme":"nord","terminal_scrollback":"lots"}"#,
        );
        // (terminal_scrollback is a user-layer key; the type check runs on
        // user layers too.)
        let m = load_merged_from(&user, Some(&root), "macos");
        assert_eq!(m.prefs.theme, "nord", "healthy keys survive");
        assert_eq!(m.prefs.terminal_scrollback, 0, "offender falls to default");
        assert!(
            m.warnings.iter().any(|w| w.contains("terminal_scrollback")),
            "{:?}",
            m.warnings
        );
        assert_eq!(
            layer_of(&m.provenance, "terminal_scrollback"),
            LayerKind::Default
        );
    }

    #[test]
    fn parse_errors_and_comments_behave() {
        let (_tmp, user, root) = setup();
        write(
            &user.join("config.json"),
            "{\n  // comments are tolerated\n  \"theme\": \"nord\",\n}\n",
        );
        write(&root.join(".croft/config.json"), "not json at all");
        let m = load_merged_from(&user, Some(&root), "macos");
        assert_eq!(m.prefs.theme, "nord");
        assert!(
            m.warnings.iter().any(|w| w.contains("not valid JSON")),
            "{:?}",
            m.warnings
        );
    }

    #[test]
    fn missing_files_merge_to_defaults_and_chain_lists_every_candidate() {
        let (_tmp, user, root) = setup();
        let m = load_merged_from(&user, Some(&root), "macos");
        assert_eq!(m.prefs, Prefs::default());
        assert_eq!(
            m.chain.len(),
            5,
            "all five layer files are watch candidates"
        );
        assert!(m.warnings.is_empty());
        // Without a workspace only the two user layers are in play.
        let m = load_merged_from(&user, None, "macos");
        assert_eq!(m.chain.len(), 2);
    }

    #[test]
    fn gitignore_append_is_idempotent_and_preserves_content() {
        let (_tmp, _user, root) = setup();
        ensure_workspace_local_ignored(&root).unwrap();
        ensure_workspace_local_ignored(&root).unwrap();
        let gi = std::fs::read_to_string(root.join(".croft/.gitignore")).unwrap();
        assert_eq!(gi.matches("config.local.json").count(), 1);
        // Existing entries survive an append.
        std::fs::write(root.join(".croft/.gitignore"), "scratch/\n").unwrap();
        ensure_workspace_local_ignored(&root).unwrap();
        let gi = std::fs::read_to_string(root.join(".croft/.gitignore")).unwrap();
        assert!(gi.contains("scratch/"));
        assert!(gi.contains("config.local.json"));
    }
}
