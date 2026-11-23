//! Finding the configurations: `.croft/launch.json`, then `.vscode/launch.json`, each
//! workspace folder's, the configurations and compounds each declares, and the folders
//! `${workspaceFolder:name}` names.

use super::*;

/// Compounds declared beside the configurations, in the same precedence order
/// as [`discover_configs`]: `.croft/launch.json` wins over `.vscode`, and a
/// duplicate name from the lower-precedence file is dropped rather than
/// listed twice.
pub fn discover_compounds(root: &Path) -> Vec<Compound> {
    let mut out: Vec<Compound> = Vec::new();
    for rel in [".croft/launch.json", ".vscode/launch.json"] {
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        for c in parse_compounds(&text, rel) {
            if !out.iter().any(|existing| existing.name == c.name) {
                out.push(c);
            }
        }
    }
    out
}

/// Every configuration from every file, duplicates kept, in file-precedence
/// order. [`discover_configs`] is the picker's list and drops a lower-precedence
/// duplicate by name; a compound needs the ones it dropped, because its members
/// name configurations in ITS OWN file and both files may use the same name.
pub fn discover_configs_all(root: &Path) -> Vec<DebugConfig> {
    let mut out: Vec<DebugConfig> = Vec::new();
    for (rel, source) in [
        (".croft/launch.json", ".croft/launch.json"),
        (".vscode/launch.json", ".vscode/launch.json"),
    ] {
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        out.extend(parse_launch_json(&text, source));
    }
    out
}

/// Every configuration the workspace declares: `.croft/launch.json` first
/// (croft-native, same schema, no `.vscode/` directory required), then
/// `.vscode/launch.json`. Both are JSONC-tolerant. Order is preserved so the
/// picker lists them as written; duplicate names keep the first occurrence.
pub fn discover_configs(root: &Path) -> Vec<DebugConfig> {
    let mut out: Vec<DebugConfig> = Vec::new();
    for (rel, source) in [
        (".croft/launch.json", ".croft/launch.json"),
        (".vscode/launch.json", ".vscode/launch.json"),
    ] {
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        for cfg in parse_launch_json(&text, source) {
            if !out.iter().any(|c| c.name == cfg.name) {
                out.push(cfg);
            }
        }
    }
    out
}

/// Parse one launch.json body: either the VS Code shape
/// (`{"configurations": [...]}`) or a bare array. Entries without a `name`
/// or `type` are skipped (VS Code refuses to run those too). Pure.
pub fn parse_launch_json(text: &str, source: &'static str) -> Vec<DebugConfig> {
    let Ok(v) = serde_json::from_str::<Value>(&crate::jsonc::strip_jsonc(text)) else {
        return Vec::new();
    };
    let list = match (&v, v.get("configurations")) {
        (_, Some(Value::Array(a))) => a.clone(),
        (Value::Array(a), _) => a.clone(),
        _ => return Vec::new(),
    };
    let inputs = parse_inputs(&v);
    list.into_iter()
        .filter_map(|c| {
            let obj = c.as_object()?;
            let name = obj.get("name")?.as_str()?.to_string();
            let type_name = obj.get("type")?.as_str()?.to_string();
            let request = match obj.get("request").and_then(Value::as_str) {
                Some("attach") => RequestKind::Attach,
                // VS Code defaults an omitted `request` to launch.
                _ => RequestKind::Launch,
            };
            Some(DebugConfig {
                name,
                type_name,
                request,
                source,
                presentation: parse_presentation(obj),
                inputs: inputs.clone(),
                answers: BTreeMap::new(),
                root: None,
                raw: obj.clone(),
            })
        })
        .collect()
}

/// The root of the workspace folder called `name` (#1425): VS Code names a
/// folder by its directory name.
pub(super) fn folder_root(folders: &[(String, PathBuf)], name: &str) -> Option<PathBuf> {
    folders
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, root)| root.clone())
}

/// The `(name, root)` pairs `${workspaceFolder:name}` and a compound
/// member's `"folder"` look up (#1425): each root under its directory name.
pub fn workspace_folders<'a>(roots: impl Iterator<Item = &'a Path>) -> Vec<(String, PathBuf)> {
    roots
        .map(|r| {
            let name = r
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (name, r.to_path_buf())
        })
        .collect()
}
