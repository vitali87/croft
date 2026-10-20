//! Layered settings (#251): one merged view over a chain of config files.
//!
//! [`crate::prefs`] knows a single `~/.config/croft/config.json`. This module
//! stacks layers over it, later layers winning per key:
//!
//! 1. built-in defaults (what `Prefs::default()` says)
//! 2. `~/.config/croft/config.json` — user, then
//!    `~/.config/croft/config.synced.json` — what config sync brought from
//!    the machine the user connected from (#262): only the appearance and
//!    editor keys a workspace may set, never trust state
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
//! matching platform. Objects deep-merge; arrays and scalars replace, except
//! the exclusion lists (`files_exclude`, `search_exclude`), which combine
//! across layers as VS Code's do, and drop a glob a later layer names with
//! a leading `!` (#1494).
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
    Synced,
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
            LayerKind::Synced => "synced",
            LayerKind::UserLocal => "user-local",
            LayerKind::VsCodeWorkspace => ".vscode",
            LayerKind::Workspace => "workspace",
            LayerKind::WorkspaceLocal => "workspace-local",
        }
    }

    /// Layers written by something other than this machine's user get the
    /// allowlist: repo-controlled ones, and what config sync brought from
    /// another machine. The user's own layers do not.
    fn is_allowlisted(self) -> bool {
        matches!(
            self,
            LayerKind::Synced
                | LayerKind::VsCodeWorkspace
                | LayerKind::Workspace
                | LayerKind::WorkspaceLocal
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
    "default_formatter",
    "language_formatters",
    "format_on_type",
    "auto_save",
    "auto_save_on_focus_change",
    "render_whitespace",
    "rulers",
    "word_based_suggestions",
    "disable_inline_blame",
    "disable_auto_close_pairs",
    "disable_paste_reindent",
    "disable_inline_values",
    "disable_save_before_debug",
    "disable_bracket_colors",
    "disable_indent_guides",
    "line_numbers",
    "disable_inlay_hints",
    "copy_on_select",
    "disable_log_highlight",
    "explorer_views",
    "files_exclude",
    "search_exclude",
    // Per-language overrides (#1440), which hold only the editor settings
    // in `crate::prefs::LANGUAGE_KEYS`; any other key in a block is dropped.
    "languages",
    "files_encoding",
];

/// The synced layer's file name (#262). Config sync pushes the projection
/// of the connecting machine's `config.json` here: a file of its own, so
/// the push can never overwrite consent or fingerprints the remote keeps in
/// its own `config.json`, and above that `config.json` in the merge, so the
/// theme follows the user. `config.local.json` still wins on the box.
pub const SYNCED_CONFIG_NAME: &str = "config.synced.json";

/// The keys of a `config.json` document that may travel to another machine
/// (#262): the [`WORKSPACE_ALLOWED_KEYS`] appearance and editor toggles,
/// read from the flat top level. `None` when the document has none of
/// them, so a machine with nothing to share pushes nothing.
pub fn synced_projection(doc: &Map<String, Value>) -> Option<Map<String, Value>> {
    let out: Map<String, Value> = doc
        .iter()
        .filter(|(k, _)| WORKSPACE_ALLOWED_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    (!out.is_empty()).then_some(out)
}

/// Where the synced layer is read from: beside the user's `config.json`.
pub fn synced_config_path() -> PathBuf {
    user_layers_dir().join(SYNCED_CONFIG_NAME)
}

/// Path of the workspace layer files under a root.
pub fn workspace_config_path(root: &Path) -> PathBuf {
    root.join(".croft").join("config.json")
}

pub fn workspace_local_config_path(root: &Path) -> PathBuf {
    root.join(".croft").join("config.local.json")
}

/// Path of the machine-local user layer. Test builds read the user layers
/// from [`user_layers_dir`] instead, so this path is not in their chain.
pub fn user_local_config_path() -> PathBuf {
    crate::prefs::config_dir().join("config.local.json")
}

/// Load the full merged view for a workspace (or just the user layers when
/// `workspace_root` is `None`), reading the user layers from
/// [`user_layers_dir`].
pub fn load_merged(workspace_root: Option<&Path>) -> MergedConfig {
    load_merged_from(&user_layers_dir(), workspace_root, current_platform())
}

/// Where [`load_merged`] reads the user layers (`config.json` and
/// `config.local.json`). Test builds use a per-process dir nothing creates,
/// so the user's own settings never steer a test (#624).
fn user_layers_dir() -> PathBuf {
    if cfg!(test) {
        std::env::temp_dir().join(format!("croft-test-config-{}", std::process::id()))
    } else {
        // The active profile's dir (#618), which is where every settings
        // change is saved; reading the bare config dir dropped them all.
        crate::profiles::file("config.json")
            .parent()
            .map_or_else(crate::prefs::config_dir, Path::to_path_buf)
    }
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
        (LayerKind::Synced, user_dir.join(SYNCED_CONFIG_NAME)),
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
            load_vscode_subset(&path, &mut warnings)
        } else {
            let mut visited = Vec::new();
            load_layer_document(&path, platform, &mut visited, &mut chain, &mut warnings)
        };
        let Some(doc) = doc else { continue };
        for (key, value) in doc {
            if kind.is_allowlisted() && !WORKSPACE_ALLOWED_KEYS.contains(&key.as_str()) {
                warnings.push(format!(
                    "{}: \"{key}\" is user-config only and was ignored ({} layers may set: appearance and editor toggles)",
                    path.display(),
                    kind.label()
                ));
                continue;
            }
            merge_setting(&mut merged, &key, &value);
            provenance.insert(key, kind);
        }
    }

    drop_negations(&mut merged);
    let prefs = deserialize_tolerantly(merged, &mut provenance, &mut warnings);
    MergedConfig {
        prefs,
        provenance,
        chain,
        warnings,
    }
}

/// Largest layer file croft will read: layers are hand-written settings, so
/// anything past this is a mistake (or a hostile `extends` target such as
/// `/dev/zero`) and refusing beats an unbounded read.
const LAYER_FILE_MAX: u64 = 1024 * 1024;

/// Read a layer file's text, refusing non-regular files (a FIFO would block
/// startup, a device file would read forever) and oversized ones, both with
/// a warning. A missing file stays silent — absent layers are normal.
fn read_layer_file(path: &Path, warnings: &mut Vec<String>) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        warnings.push(format!(
            "{}: not a regular file — layer ignored",
            path.display()
        ));
        return None;
    }
    if meta.len() > LAYER_FILE_MAX {
        warnings.push(format!(
            "{}: larger than {LAYER_FILE_MAX} bytes — layer ignored",
            path.display()
        ));
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Read one croft layer file: JSONC-tolerant parse, `extends` resolution
/// (depth-first, cycles warn and stop), then this platform's scope block
/// merged over the flat keys. `None` when the file is absent or unreadable.
///
/// `visited` is the ACTIVE recursion stack, not a set of everything ever
/// loaded: a base popped here may legitimately load again from a sibling
/// branch (diamond inheritance re-applies the shared base at its later
/// position); only a file already on the current stack is a true cycle.
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
    let doc = load_layer_document_inner(path, platform, visited, chain, warnings);
    visited.pop();
    doc
}

fn load_layer_document_inner(
    path: &Path,
    platform: &str,
    visited: &mut Vec<PathBuf>,
    chain: &mut Vec<PathBuf>,
    warnings: &mut Vec<String>,
) -> Option<Map<String, Value>> {
    let text = read_layer_file(path, warnings)?;
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
                    merge_setting(&mut acc, &k, &v);
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
    // A key no setting reads was dropped without a word, so a VS Code name
    // or a typo just did nothing (#1607). Checked here, per file, so a key
    // from an `extends` base names that base rather than the file extending
    // it.
    drop_unknown_keys(&mut doc, path, warnings);
    if let Some(Value::Object(block)) = scoped.as_mut() {
        drop_unknown_keys(block, path, warnings);
    }
    for (k, v) in doc {
        merge_setting(&mut acc, &k, &v);
    }
    if let Some(Value::Object(block)) = scoped {
        for (k, v) in block {
            merge_setting(&mut acc, &k, &v);
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

/// Settings whose lists combine across layers instead of replacing (#1494):
/// in VS Code, `files.exclude` and `search.exclude` from user and workspace
/// settings merge, so a repo's exclusions add to yours.
const UNION_KEYS: &[&str] = &["files_exclude", "search_exclude"];

/// Merge one setting of a layer into the accumulated map: the exclusion
/// lists combine, everything else deep-merges.
fn merge_setting(acc: &mut Map<String, Value>, key: &str, value: &Value) {
    let slot = acc.entry(key.to_string()).or_insert(Value::Null);
    if UNION_KEYS.contains(&key) {
        union_globs(slot, value);
    } else {
        deep_merge(slot, value);
    }
}

/// Add `over`'s globs to `base`'s, keeping order and dropping repeats. A
/// `!glob` entry removes `glob` from what the earlier layers set (VS Code's
/// `"glob": false`); it is kept until every layer is in, so a layer can
/// also take back a glob an `extends` base added, and is stripped by
/// [`drop_negations`]. A later plain `glob` cancels an earlier `!glob`.
/// A glob that really starts with `!` is written `\!glob`, which merges as
/// a plain glob. Anything but a list of strings replaces, so the usual type warning still
/// names it.
fn union_globs(base: &mut Value, over: &Value) {
    let Some(entries) = over.as_array().filter(|a| a.iter().all(Value::is_string)) else {
        *base = over.clone();
        return;
    };
    let mut out: Vec<String> = match base {
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    for entry in entries.iter().filter_map(Value::as_str) {
        let (drop, keep) = match entry.strip_prefix('!') {
            Some(glob) => (glob.to_string(), entry.to_string()),
            None => (format!("!{entry}"), entry.to_string()),
        };
        out.retain(|g| *g != drop);
        if !out.contains(&keep) {
            out.push(keep);
        }
    }
    *base = Value::from(out);
}

/// Strip the `!glob` removals once every layer is merged: they only take
/// globs away, and are not globs to match themselves. An escaped `\!glob`
/// then becomes the literal `!glob` the matcher reads.
fn drop_negations(merged: &mut Map<String, Value>) {
    for key in UNION_KEYS {
        if let Some(Value::Array(items)) = merged.get_mut(*key) {
            items.retain(|v| !v.as_str().is_some_and(|g| g.starts_with('!')));
            for item in items.iter_mut() {
                if let Some(literal) = item.as_str().and_then(|g| g.strip_prefix('\\'))
                    && literal.starts_with('!')
                {
                    *item = Value::from(literal);
                }
            }
        }
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
/// compatibility is explicitly a non-goal). A missing file is silent, but a
/// present-yet-unparsable one warns — otherwise a malformed file is
/// indistinguishable from an absent one.
fn load_vscode_subset(path: &Path, warnings: &mut Vec<String>) -> Option<Map<String, Value>> {
    let text = read_layer_file(path, warnings)?;
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
    let Some(obj) = parsed.as_object() else {
        warnings.push(format!(
            "{}: expected a JSON object at the top level — layer ignored",
            path.display()
        ));
        return None;
    };
    // ONE mapping, shared with `croft import-vscode`
    // (`crate::import_vscode::map_settings`). Keeping a second table here is
    // how the two came to disagree: this one ignored
    // `files.autoSave: "onWindowChange"` while the importer mapped it, so the
    // same file meant different things depending on which path read it.
    //
    // A workspace layer may only set the appearance and editor toggles in
    // `WORKSPACE_ALLOWED_KEYS`, so the shared result is filtered to those
    // here rather than warning about keys the user never wrote.
    let (mapped, _unmapped, _warnings) = crate::import_vscode::map_settings(obj);
    let out: Map<String, Value> = mapped
        .into_iter()
        .filter(|(k, _)| WORKSPACE_ALLOWED_KEYS.contains(&k.as_str()))
        .collect();
    (!out.is_empty()).then_some(out)
}

/// Every top-level key a settings layer may hold: each setting's name and
/// `extends` (#1607).
fn known_setting_keys() -> &'static std::collections::BTreeSet<String> {
    static KEYS: std::sync::OnceLock<std::collections::BTreeSet<String>> =
        std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        let mut keys: std::collections::BTreeSet<String> =
            match serde_json::to_value(Prefs::default()) {
                Ok(Value::Object(map)) => map.into_iter().map(|(k, _)| k).collect(),
                _ => Default::default(),
            };
        keys.insert(String::from("extends"));
        // Skipped from the serialized default while it is empty, but a
        // setting all the same (#1440).
        keys.insert(String::from("languages"));
        keys
    })
}

/// Remove from `doc` (a settings file at `path`) each key no setting reads,
/// warning about each one.
fn drop_unknown_keys(doc: &mut Map<String, Value>, path: &Path, warnings: &mut Vec<String>) {
    doc.retain(|key, _| match unknown_key_warning(path, key) {
        Some(warning) => {
            warnings.push(warning);
            false
        }
        None => true,
    });
}

/// The warning for `key` in the layer at `path` when no setting reads it,
/// naming the croft key a VS Code name maps to, or the closest setting a
/// typo was meant to be. `None` for a known key, and for a `$`-key such as
/// `$schema`, which editors read and croft ignores.
fn unknown_key_warning(path: &Path, key: &str) -> Option<String> {
    let known = known_setting_keys();
    if known.contains(key) || key.starts_with('$') {
        return None;
    }
    let hint = match crate::import_vscode::croft_keys_for(key) {
        Some([croft]) => format!(" (a VS Code setting; croft's is \"{croft}\")"),
        Some(croft) => format!(
            " (a VS Code setting; croft's are {})",
            croft
                .iter()
                .map(|k| format!("\"{k}\""))
                .collect::<Vec<_>>()
                .join(" and ")
        ),
        None => closest_setting(key, known)
            .map(|k| format!(" (did you mean \"{k}\"?)"))
            .unwrap_or_default(),
    };
    Some(format!(
        "{}: unknown setting \"{key}\" was ignored{hint}",
        path.display()
    ))
}

/// The known setting `key` most likely misspells: the same letters with the
/// underscores and case ignored, else the nearest by edit distance within a
/// third of the key's length.
fn closest_setting<'a>(
    key: &str,
    known: &'a std::collections::BTreeSet<String>,
) -> Option<&'a str> {
    let squash = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let wanted = squash(key);
    if let Some(k) = known.iter().find(|k| squash(k) == wanted) {
        return Some(k);
    }
    let limit = (key.chars().count() / 3).max(1);
    known
        .iter()
        .map(|k| (edit_distance(&squash(k), &wanted), k))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k.as_str())
}

/// Levenshtein distance between two strings, by chars.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
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
///
/// `.croft/` and `.gitignore` are repo-controlled paths, so a hostile clone
/// could make either a symlink into the user's own config
/// (`.croft/.gitignore → ~/.config/croft/config.json`) and turn this append
/// into config corruption. Both components are therefore refused as
/// symlinks, and the write itself opens with `O_NOFOLLOW` so a race cannot
/// swap one in between the check and the write.
pub fn ensure_workspace_local_ignored(root: &Path) -> std::io::Result<()> {
    use std::io::{Read, Write};

    let no_follow = |what: &Path| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is a symlink — refusing to write through it",
                what.display()
            ),
        )
    };
    let dir = root.join(".croft");
    std::fs::create_dir_all(&dir)?;
    if std::fs::symlink_metadata(&dir)?.file_type().is_symlink() {
        return Err(no_follow(&dir));
    }
    let gi = dir.join(".gitignore");
    match std::fs::symlink_metadata(&gi) {
        Ok(m) if m.file_type().is_symlink() => return Err(no_follow(&gi)),
        Ok(_) | Err(_) => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(&gi)?;
    let mut existing = String::new();
    f.read_to_string(&mut existing)?;
    if existing.lines().any(|l| l.trim() == "config.local.json") {
        return Ok(());
    }
    if !existing.is_empty() && !existing.ends_with('\n') {
        f.write_all(b"\n")?;
    }
    f.write_all(b"config.local.json\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #624: a test build reads the user layers from a dir outside the
    /// user's config dir, so their own settings never reach a test.
    #[test]
    fn a_test_build_reads_the_user_layers_outside_the_config_dir() {
        let dir = user_layers_dir();
        assert!(
            !dir.starts_with(crate::prefs::config_dir()),
            "user layers read from the real config dir: {}",
            dir.display()
        );
        assert!(!dir.exists(), "nothing creates the test user dir");
    }

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

    /// #1440: a workspace may set per-language editor settings. Its blocks
    /// merge with the user's a setting at a time, the repo's own
    /// `.vscode/settings.json` language blocks apply, and a block's keys
    /// outside the per-language set are dropped.
    #[test]
    fn language_blocks_merge_across_layers_and_keep_only_editor_keys() {
        let (_tmp, user, root) = setup();
        write(
            &user.join("config.json"),
            r#"{"languages":{"python":{"format_on_save":true,"render_whitespace":"all"}}}"#,
        );
        write(
            &root.join(".vscode/settings.json"),
            r#"{"[c]":{"editor.formatOnSave":true,"editor.fontSize":30}}"#,
        );
        write(
            &root.join(".croft/config.json"),
            r#"{"languages":{"python":{"format_on_save":false,"mcp_consented":["evil"]}}}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        let py = &m.prefs.languages["python"];
        assert_eq!(py.format_on_save, Some(false), "the workspace wins");
        assert_eq!(
            py.render_whitespace.as_deref(),
            Some("all"),
            "the user's other python setting survives"
        );
        assert_eq!(m.prefs.languages["c"].format_on_save, Some(true));
        assert!(m.prefs.mcp_consented.is_empty());
        assert_eq!(layer_of(&m.provenance, "languages"), LayerKind::Workspace);
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    /// #1524: rulers are display only, so a repository's `.croft/config.json`
    /// may set them (a project's line-length limit), over the user's own.
    #[test]
    fn a_workspace_layer_may_set_rulers() {
        let (_tmp, user, root) = setup();
        write(&user.join("config.json"), r#"{"rulers":[120]}"#);
        write(&root.join(".croft/config.json"), r#"{"rulers":[88]}"#);
        let m = load_merged_from(&user, Some(&root), "linux");
        assert_eq!(m.prefs.rulers, vec![88]);
        assert_eq!(layer_of(&m.provenance, "rulers"), LayerKind::Workspace);
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    #[test]
    fn workspace_layers_cannot_touch_trust_or_execution_settings() {
        let (_tmp, user, root) = setup();
        write(
            &user.join("config.json"),
            r#"{"disabled_extensions":["dap-python"],"fleet_groups":{"web":["web1","web2"]}}"#,
        );
        write(
            &root.join(".croft/config.json"),
            r#"{"theme":"nord","mcp_consented":["evil"],"disabled_extensions":[],"host_accents":[{"pattern":"*"}],"fleet_groups":{"all":["prod-db"]}}"#,
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
        // A fleet group is a list of machines to run commands on (#363), so
        // a checked-out repo naming one could add a production box to a set
        // the user then broadcasts to.
        // The user's own group survives the refused workspace override, so
        // this proves refusal rather than the key simply never deserializing
        // - without a user-layer value, an inert feature would pass too.
        assert_eq!(
            m.prefs.fleet_groups.get("web").map(Vec::len),
            Some(2),
            "a user layer CAN name a fleet group"
        );
        assert!(
            !m.prefs.fleet_groups.contains_key("all"),
            "a cloned repo must not name hosts for a fleet run"
        );
        let joined = m.warnings.join("\n");
        assert!(joined.contains("mcp_consented"), "{joined}");
        assert!(joined.contains("disabled_extensions"), "{joined}");
        assert!(joined.contains("host_accents"), "{joined}");
        assert!(joined.contains("fleet_groups"), "{joined}");
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

    /// An UNRECOGNISED `files.autoSave` value must change nothing.
    ///
    /// The mapper this layer used to carry matched three known values with a
    /// `_ => {}` arm. When the table was consolidated with the importer's,
    /// any string became authoritative, so a typo in a workspace file emitted
    /// `auto_save: false` and silently turned the user's own setting off.
    ///
    /// This goes through `load_merged_from`, the real chain. The test in
    /// `import_vscode` that names this agreement calls `map_settings`
    /// directly and never reaches this layer, so it would pass with the
    /// consolidation reverted entirely and could not see this regression.
    #[test]
    fn an_unrecognised_auto_save_value_does_not_override_the_user() {
        let (_tmp, user, root) = setup();
        write(&user.join("config.json"), r#"{ "auto_save": true }"#);
        write(
            &root.join(".vscode/settings.json"),
            r#"{ "files.autoSave": "afterDelayy" }"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(
            m.prefs.auto_save,
            "a typo in a workspace file must not turn the user's auto-save off"
        );
        assert_eq!(layer_of(&m.provenance, "auto_save"), LayerKind::User);
    }

    /// And `onWindowChange`, which VS Code does define, must reach this layer
    /// as a focus-change save. The two mappers disagreed on it before they
    /// were consolidated: the importer mapped it, this layer ignored it.
    #[test]
    fn on_window_change_reaches_the_workspace_layer() {
        let (_tmp, user, root) = setup();
        write(
            &root.join(".vscode/settings.json"),
            r#"{ "files.autoSave": "onWindowChange" }"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(
            m.prefs.auto_save_on_focus_change,
            "onWindowChange saves when the window loses focus"
        );
        assert_eq!(
            layer_of(&m.provenance, "auto_save_on_focus_change"),
            LayerKind::VsCodeWorkspace
        );
    }

    /// #1499: a repo's `.vscode` names the formatter beside its
    /// `formatOnSave`, for every file and per language; both reach croft
    /// from the workspace layer.
    #[test]
    fn vscode_default_formatters_reach_the_workspace_layer() {
        let (_tmp, user, root) = setup();
        write(
            &root.join(".vscode/settings.json"),
            r#"{
  "editor.formatOnSave": true,
  "editor.defaultFormatter": "biomejs.biome",
  "[python]": { "editor.defaultFormatter": "charliermarsh.ruff", "editor.tabSize": 4 },
  "[javascript][typescript]": { "editor.defaultFormatter": "esbenp.prettier-vscode" },
}"#,
        );
        let m = load_merged_from(&user, Some(&root), "linux");
        assert!(m.prefs.format_on_save);
        assert_eq!(m.prefs.default_formatter.as_deref(), Some("biomejs.biome"));
        assert_eq!(
            m.prefs.language_formatters,
            [
                ("javascript][typescript", "esbenp.prettier-vscode"),
                ("python", "charliermarsh.ruff"),
            ]
            .iter()
            .map(|(l, f)| (l.to_string(), f.to_string()))
            .collect::<BTreeMap<_, _>>()
        );
        assert_eq!(
            layer_of(&m.provenance, "default_formatter"),
            LayerKind::VsCodeWorkspace
        );
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    /// #1499: an imported user `[typescript]` formatter survives a workspace
    /// `[javascript][typescript]` one, as in VS Code, and a workspace
    /// `null` clears the user's formatter for every file.
    #[test]
    fn a_grouped_workspace_formatter_keeps_a_user_single_language_one() {
        let (_tmp, user, root) = setup();
        write(
            &user.join("config.json"),
            r#"{ "default_formatter": "esbenp.prettier-vscode", "language_formatters": { "typescript": "vscode.typescript-language-features" } }"#,
        );
        write(
            &root.join(".vscode/settings.json"),
            r#"{ "editor.defaultFormatter": null, "[javascript][typescript]": { "editor.defaultFormatter": "biomejs.biome" }, "[python]": { "editor.defaultFormatter": null } }"#,
        );
        let m = load_merged_from(&user, Some(&root), "linux");
        assert_eq!(m.prefs.default_formatter, None);
        let per = &m.prefs.language_formatters;
        let named = |l| crate::default_formatter::named(None, per, Some(l));
        assert_eq!(
            named("typescript"),
            Some("vscode.typescript-language-features")
        );
        assert_eq!(named("javascript"), Some("biomejs.biome"));
        assert_eq!(named("python"), None);
        assert_eq!(per.get("python").map(String::as_str), Some(""));
    }

    /// #1499, negative: a language block without a formatter, or with an
    /// empty one, names nothing.
    #[test]
    fn a_language_block_without_a_formatter_names_none() {
        let (_tmp, user, root) = setup();
        write(
            &root.join(".vscode/settings.json"),
            r#"{ "editor.defaultFormatter": "", "[go]": { "editor.tabSize": 8 }, "[rust]": {} }"#,
        );
        let m = load_merged_from(&user, Some(&root), "linux");
        assert_eq!(m.prefs.default_formatter, None);
        assert!(m.prefs.language_formatters.is_empty());
    }

    #[test]
    fn vscode_settings_map_a_small_subset_and_lose_to_croft_layers() {
        let (_tmp, user, root) = setup();
        write(
            &root.join(".vscode/settings.json"),
            r#"{
  // JSONC is fine here too
  "editor.formatOnSave": true,
  "editor.formatOnType": true,
  "files.autoSave": "afterDelay",
  "editor.fontSize": 13,
}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(m.prefs.format_on_save);
        assert!(m.prefs.format_on_type);
        assert_eq!(
            layer_of(&m.provenance, "format_on_type"),
            LayerKind::VsCodeWorkspace
        );
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
        assert_eq!(m.chain.len(), 6, "all six layer files are watch candidates");
        assert!(m.warnings.is_empty());
        // Without a workspace only the three user layers are in play.
        let m = load_merged_from(&user, None, "macos");
        assert_eq!(m.chain.len(), 3);
    }

    /// #262: the synced layer carries the connecting machine's appearance to
    /// this one. It wins over this machine's `config.json` (the theme follows
    /// the user) and loses to `config.local.json` (a box can still pin its
    /// own), and like a workspace layer it can set no trust state: a synced
    /// file naming MCP consent is refused with a warning, not merged.
    #[test]
    fn the_synced_layer_sits_between_user_and_local_and_sets_no_trust() {
        let (_tmp, user, _root) = setup();
        std::fs::write(
            user.join("config.json"),
            r#"{ "theme": "light", "format_on_save": false, "mcp_consented": ["local-srv"] }"#,
        )
        .unwrap();
        std::fs::write(
            user.join(SYNCED_CONFIG_NAME),
            r#"{ "theme": "solarized", "format_on_save": true, "mcp_consented": ["evil"] }"#,
        )
        .unwrap();
        let m = load_merged_from(&user, None, "linux");
        assert_eq!(
            m.prefs.theme, "solarized",
            "the synced theme wins over config.json"
        );
        assert!(m.prefs.format_on_save);
        assert_eq!(layer_of(&m.provenance, "theme"), LayerKind::Synced);
        assert!(
            m.prefs.mcp_consented.contains("local-srv") && !m.prefs.mcp_consented.contains("evil"),
            "consent comes from this machine only: {:?}",
            m.prefs.mcp_consented
        );
        assert!(
            m.warnings
                .iter()
                .any(|w| w.contains("mcp_consented") && w.contains("synced")),
            "{:?}",
            m.warnings
        );
        std::fs::write(user.join("config.local.json"), r#"{ "theme": "dark" }"#).unwrap();
        let m = load_merged_from(&user, None, "linux");
        assert_eq!(
            m.prefs.theme, "dark",
            "config.local.json still pins this box"
        );
    }

    /// #262: the projection that travels keeps the appearance and editor
    /// keys of a `config.json` and nothing that says what croft trusts.
    #[test]
    fn the_synced_projection_keeps_appearance_and_drops_trust() {
        let doc: Map<String, Value> = serde_json::from_str(
            r#"{ "theme": "light", "render_whitespace": "all",
                 "mcp_consented": ["a"], "mcp_tool_fingerprints": {"a": "x"},
                 "disabled_extensions": ["b"], "host_accents": [] }"#,
        )
        .unwrap();
        let p = synced_projection(&doc).unwrap();
        let keys: Vec<&str> = p.keys().map(String::as_str).collect();
        assert_eq!(keys, ["render_whitespace", "theme"]);
        let trust_only: Map<String, Value> =
            serde_json::from_str(r#"{ "mcp_consented": ["a"] }"#).unwrap();
        assert_eq!(synced_projection(&trust_only), None, "nothing to share");
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

    /// Diamond inheritance is not a cycle: two siblings extending the same
    /// base both load it, so the shared base re-applies at its later
    /// position (`b`'s inherited copy wins over `a`'s override).
    #[test]
    fn diamond_extends_loads_the_shared_base_from_both_branches() {
        let (_tmp, user, root) = setup();
        write(&user.join("common.json"), r#"{"theme":"nord"}"#);
        write(
            &user.join("a.json"),
            r#"{"extends":"./common.json","theme":"black"}"#,
        );
        write(&user.join("b.json"), r#"{"extends":"./common.json"}"#);
        write(
            &user.join("config.json"),
            r#"{"extends":["./a.json","./b.json"]}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(
            m.warnings.is_empty(),
            "a shared base is not a cycle: {:?}",
            m.warnings
        );
        assert_eq!(
            m.prefs.theme, "nord",
            "b's inherited base merges after a's override, exactly as if \
             its keys were written inline"
        );
        // A real cycle still warns and stops.
        write(&user.join("x.json"), r#"{"extends":"./y.json"}"#);
        write(&user.join("y.json"), r#"{"extends":"./x.json"}"#);
        write(&user.join("config.json"), r#"{"extends":"./x.json"}"#);
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(
            m.warnings.iter().any(|w| w.contains("cycle")),
            "{:?}",
            m.warnings
        );
    }

    /// Layer reads are bounded and regular-file only: an `extends` target
    /// that is a directory (or FIFO, device, …) is refused with a warning
    /// instead of blocking or ballooning startup, and so is an absurdly
    /// large file.
    #[test]
    fn non_regular_and_oversized_layer_files_are_refused() {
        let (_tmp, user, root) = setup();
        std::fs::create_dir_all(user.join("adir")).unwrap();
        write(
            &user.join("config.json"),
            r#"{"extends":"./adir","theme":"black"}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(
            m.warnings.iter().any(|w| w.contains("not a regular file")),
            "{:?}",
            m.warnings
        );
        assert_eq!(m.prefs.theme, "black", "the file's own keys still apply");

        let huge = " ".repeat(LAYER_FILE_MAX as usize + 1);
        write(&user.join("huge.json"), &huge);
        write(&user.join("config.json"), r#"{"extends":"./huge.json"}"#);
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(
            m.warnings.iter().any(|w| w.contains("larger than")),
            "{:?}",
            m.warnings
        );
    }

    /// A present-but-broken .vscode/settings.json warns; an absent one is
    /// silent (absent layers are the normal case).
    #[test]
    fn a_malformed_vscode_settings_file_warns_instead_of_vanishing() {
        let (_tmp, user, root) = setup();
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        write(&root.join(".vscode/settings.json"), "{ definitely not json");
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(
            m.warnings
                .iter()
                .any(|w| w.contains("settings.json") && w.contains("not valid JSON")),
            "{:?}",
            m.warnings
        );
        write(&root.join(".vscode/settings.json"), "[1,2,3]");
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(
            m.warnings.iter().any(|w| w.contains("JSON object")),
            "{:?}",
            m.warnings
        );
    }

    /// Repo-controlled symlinks must not redirect the gitignore write into
    /// the user's own files (`.croft/.gitignore → ~/.config/croft/…`).
    #[cfg(unix)]
    #[test]
    fn the_gitignore_write_refuses_symlinked_components() {
        let (tmp, _user, root) = setup();
        let victim = tmp.path().join("victim.json");
        std::fs::write(&victim, "{\"precious\": true}\n").unwrap();
        std::fs::create_dir_all(root.join(".croft")).unwrap();
        std::os::unix::fs::symlink(&victim, root.join(".croft/.gitignore")).unwrap();
        let err = ensure_workspace_local_ignored(&root).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "{\"precious\": true}\n",
            "the symlink target must be untouched"
        );

        // A symlinked .croft directory is refused the same way.
        let root2 = tmp.path().join("repo2");
        std::fs::create_dir_all(&root2).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root2.join(".croft")).unwrap();
        let err = ensure_workspace_local_ignored(&root2).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
    }

    /// #1494: the issue's three layers. User `**/tests`, `.vscode`
    /// `**/dist` and `.croft` `**/generated` + `*.log` all apply together,
    /// in layer order.
    #[test]
    fn exclusion_lists_combine_across_layers() {
        let user = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("config.json"),
            r#"{ "files_exclude": ["**/tests"], "search_exclude": ["**/fixtures"] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join(".vscode")).unwrap();
        std::fs::write(
            root.path().join(".vscode/settings.json"),
            r#"{ "files.exclude": { "**/dist": true } }"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join(".croft")).unwrap();
        std::fs::write(
            root.path().join(".croft/config.json"),
            r#"{ "files_exclude": ["**/generated", "*.log", "**/dist"] }"#,
        )
        .unwrap();
        let merged = load_merged_from(user.path(), Some(root.path()), "linux");
        assert_eq!(
            merged.prefs.files_exclude,
            ["**/tests", "**/dist", "**/generated", "*.log"]
        );
        assert_eq!(merged.prefs.search_exclude, ["**/fixtures"]);
    }

    /// #1494: a layer takes a glob back with `!glob`, or with VS Code's
    /// `"glob": false`; no `!` entry reaches the list croft matches with,
    /// and a later layer can add the glob again.
    #[test]
    fn a_negated_glob_removes_an_earlier_layer_s_exclusion() {
        let user = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("config.json"),
            r#"{ "files_exclude": ["**/tests", "**/build", "**/out"] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join(".vscode")).unwrap();
        std::fs::write(
            root.path().join(".vscode/settings.json"),
            r#"{ "files.exclude": { "**/tests": false, "**/out": false } }"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join(".croft")).unwrap();
        std::fs::write(
            root.path().join(".croft/config.json"),
            r#"{ "files_exclude": ["!**/build", "!**/never-set", "**/out"] }"#,
        )
        .unwrap();
        let merged = load_merged_from(user.path(), Some(root.path()), "linux");
        assert_eq!(merged.prefs.files_exclude, ["**/out"]);
    }

    /// #1494: a glob that really starts with `!` is escaped as `\!glob`; it
    /// merges as a plain glob, `!\!glob` takes it back, and it reaches the
    /// matcher as the literal `!glob`.
    #[test]
    fn an_escaped_bang_is_a_literal_glob_not_a_removal() {
        let user = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("config.json"),
            r#"{ "files_exclude": ["\\!important", "\\!draft"] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join(".croft")).unwrap();
        std::fs::write(
            root.path().join(".croft/config.json"),
            r#"{ "files_exclude": ["!\\!draft"] }"#,
        )
        .unwrap();
        let merged = load_merged_from(user.path(), Some(root.path()), "linux");
        assert_eq!(merged.prefs.files_exclude, ["!important"]);
    }

    /// #1494 negative: every other list still replaces, and a malformed
    /// exclusion value still replaces (and warns) rather than merging.
    #[test]
    fn other_lists_still_replace_and_a_bad_exclusion_value_warns() {
        let mut acc = Map::new();
        merge_setting(
            &mut acc,
            "remote_offer_excluded_hosts",
            &serde_json::json!(["a"]),
        );
        merge_setting(
            &mut acc,
            "remote_offer_excluded_hosts",
            &serde_json::json!(["b"]),
        );
        assert_eq!(acc["remote_offer_excluded_hosts"], serde_json::json!(["b"]));
        merge_setting(&mut acc, "files_exclude", &serde_json::json!(["a"]));
        merge_setting(&mut acc, "files_exclude", &serde_json::json!("b"));
        assert_eq!(acc["files_exclude"], serde_json::json!("b"));
    }

    /// #1607: a misspelt key was dropped without a word.
    #[test]
    fn a_misspelt_key_warns_naming_the_setting_it_meant() {
        let (_tmp, user, root) = setup();
        write(&user.join("config.json"), r#"{"autosave":true}"#);
        let m = load_merged_from(&user, Some(&root), "macos");
        assert_eq!(m.warnings.len(), 1, "{:?}", m.warnings);
        let w = &m.warnings[0];
        assert!(w.contains("unknown setting \"autosave\""), "{w}");
        assert!(w.contains("did you mean \"auto_save\"?"), "{w}");
        assert!(w.contains("config.json"), "names the file: {w}");
        assert!(!m.prefs.auto_save, "the typo still sets nothing");
    }

    /// #1607: a VS Code name warns with the croft key to use instead.
    #[test]
    fn a_vs_code_key_warns_naming_croft_s_setting() {
        let (_tmp, user, root) = setup();
        write(
            &user.join("config.json"),
            r#"{"editor.formatOnSave":true,"files.autoSave":"afterDelay"}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        let joined = m.warnings.join("\n");
        assert!(
            joined.contains(
                "unknown setting \"editor.formatOnSave\" was ignored (a VS Code setting; croft's is \"format_on_save\")"
            ),
            "{joined}"
        );
        assert!(
            joined.contains(
                "unknown setting \"files.autoSave\" was ignored (a VS Code setting; croft's are \"auto_save\" and \"auto_save_on_focus_change\")"
            ),
            "{joined}"
        );
        assert_eq!(m.warnings.len(), 2, "{joined}");
    }

    /// #1607: an unknown key in a workspace layer warns too, and a known
    /// key beside it still applies.
    #[test]
    fn an_unknown_key_in_a_workspace_layer_warns_and_the_rest_applies() {
        let (_tmp, user, root) = setup();
        write(
            &root.join(".croft/config.json"),
            r#"{"format_on_save":true,"fromat_on_save":false}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(m.prefs.format_on_save);
        assert_eq!(m.warnings.len(), 1, "{:?}", m.warnings);
        assert!(
            m.warnings[0].contains("\"fromat_on_save\"")
                && m.warnings[0].contains("did you mean \"format_on_save\"?"),
            "{:?}",
            m.warnings
        );
    }

    /// #1607 negative: known keys, `extends`, `$schema` and the platform
    /// blocks are all read, so none of them warns.
    #[test]
    fn known_keys_extends_schema_and_platform_blocks_do_not_warn() {
        let (_tmp, user, root) = setup();
        write(&user.join("base.json"), r#"{"theme":"nord"}"#);
        write(
            &user.join("config.json"),
            r#"{"$schema":"x","extends":"base.json","format_on_save":true,"explorer_views":{"rust_dependencies":false},"linux":{"theme":"black"},"macos":{"theme":"dracula"}}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        assert!(m.prefs.format_on_save);
        assert!(!m.prefs.explorer_views.dependencies);
        assert_eq!(m.prefs.theme, "dracula");
    }

    /// #1607 negative: `.vscode/settings.json` is full of keys croft does not
    /// read; only the mapped subset is taken, and none of the rest warns.
    #[test]
    fn a_vs_code_workspace_file_s_other_keys_do_not_warn() {
        let (_tmp, user, root) = setup();
        write(
            &root.join(".vscode/settings.json"),
            r#"{"editor.formatOnSave":true,"python.analysis.typeCheckingMode":"strict"}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        assert!(m.prefs.format_on_save);
    }

    /// #1607: an unknown key in an `extends` base names the base, not the
    /// file extending it.
    #[test]
    fn an_unknown_key_in_an_extends_base_names_the_base() {
        let (_tmp, user, root) = setup();
        write(&user.join("base.json"), r#"{"fromat_on_save":true}"#);
        write(
            &user.join("config.json"),
            r#"{"extends":"base.json","theme":"nord"}"#,
        );
        let m = load_merged_from(&user, Some(&root), "macos");
        assert_eq!(m.warnings.len(), 1, "{:?}", m.warnings);
        let w = &m.warnings[0];
        assert!(w.contains("unknown setting \"fromat_on_save\""), "{w}");
        assert!(w.contains("base.json"), "names the base: {w}");
        assert!(!w.contains("config.json"), "not the extending file: {w}");
        assert_eq!(m.prefs.theme, "nord");
    }

    /// #1607 negative: a word nowhere near any setting warns without a guess.
    #[test]
    fn an_unrelated_key_warns_without_a_guess() {
        let (_tmp, user, root) = setup();
        write(&user.join("config.json"), r#"{"zzqxv":1}"#);
        let m = load_merged_from(&user, Some(&root), "macos");
        assert_eq!(m.warnings.len(), 1, "{:?}", m.warnings);
        assert!(m.warnings[0].contains("unknown setting \"zzqxv\""));
        assert!(!m.warnings[0].contains("did you mean"), "{:?}", m.warnings);
    }
}
