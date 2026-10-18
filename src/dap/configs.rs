//! Debug configurations (#250): `.vscode/launch.json` and `.croft/launch.json`.
//!
//! VS Code has carried program arguments, environment variables, working
//! directories, build steps, and attach targets in `launch.json` since 1.0;
//! croft's zero-config "Debug active file" covers none of that. This module
//! reads both files (JSONC tolerated, like tasks.json), resolves the
//! `${...}` substitution variables, and builds the DAP `launch`/`attach`
//! request for whichever adapter family the config's `type` names. Fields
//! croft doesn't know are passed through to the adapter verbatim — that is
//! how VS Code behaves, and it keeps adapter-specific keys (`sourceMaps`,
//! `justMyCode`, …) working without croft modelling them.
//!
//! Everything here is pure (filesystem in, values out); the App owns the
//! side effects (spawning adapters, running the preLaunchTask in a pane).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::dap::session::AdapterKind;

/// launch/attach — the two DAP session-starting requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Launch,
    Attach,
}

/// One configuration as read from a launch.json, unresolved: substitution
/// variables still in place, every field kept in `raw`. Resolution happens at
/// F5 time (in [`resolve`]) because `${file}` depends on the active editor.
// `Eq` is intentionally not derived: `presentation.order` is a `f64`, and
// floats break total equality. (`ResolvedConfig` below DOES derive `Eq` - it
// carries no float - so it is not the precedent this looks like.) None of
// these is ever a map key, so `PartialEq` is all the call sites and tests
// need.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugConfig {
    pub name: String,
    /// The `type` as written (`python`, `lldb`, `node`, …); mapped to an
    /// [`AdapterKind`] only at resolve time so an unsupported type shows a
    /// per-config error instead of being silently dropped from the picker.
    pub type_name: String,
    pub request: RequestKind,
    /// Which file declared it, for the picker's detail column.
    pub source: &'static str,
    /// The configuration's `presentation` (#318), when it declares a usable
    /// one. VS Code reads this on configurations as well as compounds and
    /// sorts BOTH kinds in one list, so a config that asks for a group must
    /// be able to sort against a compound that asks for the same one.
    pub presentation: Option<Presentation>,
    /// The workspace folder this configuration belongs to when it is not
    /// the active one: a compound member named with `"folder"` (#1425)
    /// resolves `${workspaceFolder}` against its own folder.
    pub root: Option<PathBuf>,
    raw: Map<String, Value>,
}

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
    let Ok(v) = serde_json::from_str::<Value>(&crate::tasks::strip_jsonc(text)) else {
        return Vec::new();
    };
    let list = match (&v, v.get("configurations")) {
        (_, Some(Value::Array(a))) => a.clone(),
        (Value::Array(a), _) => a.clone(),
        _ => return Vec::new(),
    };
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
                root: None,
                raw: obj.clone(),
            })
        })
        .collect()
}

/// A `presentation` block, as declared on a configuration OR a compound
/// (#318). VS Code reads the same three keys in both places and sorts the two
/// kinds together in one list, so croft parses it in both too.
// `Eq` is intentionally not derived: `presentation.order` is a `f64`, and
// floats break total equality. (`ResolvedConfig` below DOES derive `Eq` - it
// carries no float - so it is not the precedent this looks like.) None of
// these is ever a map key, so `PartialEq` is all the call sites and tests
// need.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Presentation {
    /// Do not list this entry in the picker.
    pub hidden: bool,
    /// The name entries are grouped under. Groups sort against each other by
    /// name; entries carrying one sort ahead of entries that do not.
    pub group: Option<String>,
    /// Position within the group, ascending. An entry with no numeric order
    /// sorts after the ones that have it.
    ///
    /// `f64` rather than an integer because VS Code's schema says `number`
    /// and its comparator subtracts one from the other, so `1.5` is a legal
    /// position between two neighbours - not the typo that a non-number is.
    pub order: Option<f64>,
}

/// Read a `presentation` block, if the entry declares one at all.
///
/// `None` only for ABSENT or unreadable. An empty block is still a block:
/// VS Code's comparator tests the object itself for truthiness (`if
/// (!first.presentation) … return 1`), so `{}` is truthy there and does sort
/// ahead of an entry carrying no block. Returning `None` for it would place
/// those rows differently than VS Code places them, which is the one thing a
/// transcription must not do.
///
/// The INNER checks stay truthiness-based, matching the same comparator: an
/// empty-string `group` is no group.
pub fn parse_presentation(obj: &Map<String, Value>) -> Option<Presentation> {
    let p = obj.get("presentation")?.as_object()?;
    let hidden = p.get("hidden").and_then(Value::as_bool).unwrap_or(false);
    let group = p
        .get("group")
        .and_then(Value::as_str)
        .filter(|g| !g.is_empty())
        .map(str::to_string);
    let order = p.get("order").and_then(Value::as_f64);
    Some(Presentation {
        hidden,
        group,
        order,
    })
}

/// The malformed `presentation` shapes this entry declares, named with their
/// parent so they can be found in the file (#318).
///
/// A value croft cannot read is a TYPO, and `{"order": "1"}` is the same
/// mistake as `{"hidden": "true"}`: quoted where it should be bare. Dropping
/// it silently would leave the user with a picker that ignores the ordering
/// they wrote and nothing to explain why.
///
/// Only COMPOUNDS act on this. A configuration has no refusal path to hang it
/// on - it launches directly rather than through a gate - so a config follows
/// VS Code and ignores what it cannot read.
pub fn malformed_presentation_keys(obj: &Map<String, Value>) -> Vec<&'static str> {
    let Some(value) = obj.get("presentation") else {
        return Vec::new();
    };
    // A key that REQUESTS NOTHING is honoured by doing nothing, and naming it
    // in a refusal would report a limitation croft does not have. `null`,
    // `false` and `""` are that shape; this list is the same one
    // `asks_for_something` applies to the sibling keys, kept in step with it
    // on purpose.
    if value.is_null() || value == &Value::Bool(false) || value.as_str().is_some_and(str::is_empty)
    {
        return Vec::new();
    }
    let Some(p) = value.as_object() else {
        return vec!["presentation"];
    };
    let mut out = Vec::new();
    for (key, path, well_formed) in [
        (
            "hidden",
            "presentation.hidden",
            Value::is_boolean as fn(&Value) -> bool,
        ),
        (
            "group",
            "presentation.group",
            Value::is_string as fn(&Value) -> bool,
        ),
        (
            "order",
            "presentation.order",
            Value::is_number as fn(&Value) -> bool,
        ),
    ] {
        if p.get(key).is_some_and(|v| !v.is_null() && !well_formed(v)) {
            out.push(path);
        }
    }
    out
}

/// Filter out hidden entries and sort the rest the way VS Code's
/// `getVisibleAndSorted` does (#318), so a launch.json ordered for VS Code
/// reads the same here.
///
/// The comparator is transcribed from `debugUtils.ts` rather than invented,
/// because every one of its rules is a decision croft would otherwise get
/// subtly wrong:
///
/// - an entry WITH a `presentation` sorts ahead of one without, so adding
///   `{"order": 1}` to a single entry lifts it above untouched neighbours;
/// - within those, an entry with a `group` sorts ahead of one without;
/// - groups compare by NAME, so grouping is alphabetical rather than
///   first-seen;
/// - inside a group, a numeric `order` sorts ahead of a missing one.
///
/// The sort is stable, matching JS, which is what keeps a workspace that
/// declares no `presentation` at all in exactly its file order - the case
/// that must not move.
///
/// One deliberate divergence: VS Code compares group names with
/// `localeCompare`, which is locale-aware collation; croft compares them
/// bytewise. The two agree on ASCII group names and can disagree on accented
/// ones, and matching it properly would mean taking a collation dependency
/// for the ordering of a picker.
pub fn visible_and_sorted<T>(
    items: Vec<T>,
    presentation: impl Fn(&T) -> Option<&Presentation>,
) -> Vec<T> {
    let mut kept: Vec<T> = items
        .into_iter()
        .filter(|i| !presentation(i).is_some_and(|p| p.hidden))
        .collect();
    kept.sort_by(|a, b| {
        use std::cmp::Ordering;
        let (Some(first), Some(second)) = (presentation(a), presentation(b)) else {
            return match (presentation(a), presentation(b)) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                _ => unreachable!("both-Some is handled by the outer let-else"),
            };
        };
        match (&first.group, &second.group) {
            (None, None) => compare_orders(first.order, second.order),
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(g1), Some(g2)) if g1 != g2 => g1.cmp(g2),
            _ => compare_orders(first.order, second.order),
        }
    });
    kept
}

/// `order` against `order`: a declared one wins over a missing one, and two
/// missing ones tie (leaving the stable sort to keep file order).
fn compare_orders(first: Option<f64>, second: Option<f64>) -> std::cmp::Ordering {
    match (first, second) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        // `total_cmp` rather than `partial_cmp` with a fallback: JSON cannot
        // carry a NaN, but a comparator that answered `Equal` for one would
        // break transitivity, which `sort_by` panics on. A total order has no
        // such arm to get wrong. It orders `-0.0` before `0.0` where VS Code's
        // subtraction calls them equal; `"order": -0.0` is expressible and
        // nothing else about the two differs.
        (Some(a), Some(b)) => a.total_cmp(&b),
    }
}

/// A `compounds` entry: several configurations launched together under one
/// name. Members are stored as written and resolved against the file's
/// `configurations` at launch time, so a compound naming a configuration that
/// is later renamed fails loudly rather than launching a subset.
// `Eq` is intentionally not derived: `presentation.order` is a `f64`, and
// floats break total equality. (`ResolvedConfig` below DOES derive `Eq` - it
// carries no float - so it is not the precedent this looks like.) None of
// these is ever a map key, so `PartialEq` is all the call sites and tests
// need.
#[derive(Debug, Clone, PartialEq)]
pub struct Compound {
    pub name: String,
    pub configurations: Vec<String>,
    /// Each member's `"folder"` (#1425), aligned with `configurations`:
    /// `Some("web")` names a configuration in the `web` workspace folder's
    /// launch.json; `None`, a bare name, one beside the compound.
    pub folders: Vec<Option<String>>,
    /// The compound's own `preLaunchTask`, if it asks for one (#318). Run
    /// before the member starts, and separate from the member's own task:
    /// VS Code runs both, so croft chains them rather than choosing.
    ///
    /// `None` for a key that is absent, `null`, or `""` - each of those
    /// requests nothing, and parking a launch behind a task named `""` would
    /// invent work the file never asked for.
    pub pre_launch_task: Option<String>,
    /// The compound's `presentation` (#318), when it declares a usable one.
    ///
    /// All three keys are honoured now: `hidden` filters the row, and
    /// `group`/`order` place it via [`visible_and_sorted`], which sorts
    /// compounds and configurations in ONE list the way VS Code does. Only
    /// the malformed shapes still refuse, and they are listed in
    /// `unsupported_keys`.
    pub presentation: Option<Presentation>,
    /// Keys croft does not honour that this compound ACTUALLY ASKS FOR,
    /// named with their parent so they can be found in the file. Since #318
    /// honoured all three, only VALUES croft cannot READ remain: a
    /// `presentation` that is not an object, or a `hidden`, `group` or
    /// `order` that is not a bool, string or number respectively.
    ///
    /// A `presentation` that is `null`, `false`, `""` or empty is not
    /// recorded: the first three ask for nothing, and an empty block asks
    /// only to be DECLARED, which the sort reads (it places such a row ahead
    /// of one carrying no block, as VS Code does). Inside the block only
    /// `null` is carved out, so `{"order": false}` IS recorded. A MALFORMED
    /// value is recorded rather than ignored: `{"hidden": "true"}` is the
    /// likely typo here, and launching it unhidden with no signal is worse
    /// than the refusal it replaces.
    ///
    /// A mistyped KEY (`{"grup": "a"}`) is still ignored in silence, on
    /// purpose: enumerating unknown keys would refuse whatever VS Code adds
    /// next.
    ///
    /// `stopAll` is honoured as a bool (#567) and recorded here only when
    /// its value is not one: `"true"` is the quoted-bool typo the
    /// presentation keys also refuse.
    ///
    /// Recorded rather than silently dropped because a ONE-member compound now
    /// launches, and launching it via its member alone would run neither the
    /// compound's own pre-launch task nor respect its presentation. Both
    /// arities consult this list now: the multi-member path used to discard
    /// it silently because it refused for the session count first, and #310
    /// removed that refusal.
    pub unsupported_keys: Vec<&'static str>,
    /// Which file declared it. A compound's members name configurations in
    /// ITS OWN file first: both files may declare the same name, and binding
    /// a `.vscode` compound to a `.croft` config of that name would debug a
    /// different program than the compound was written against.
    pub source: &'static str,
    /// `stopAll: true` (#567): one member ending stops the whole set. VS
    /// Code's default, `false`, lets the siblings keep running.
    pub stop_all: bool,
}

impl Compound {
    /// Whether the compound asked not to be listed (#318).
    pub fn hidden(&self) -> bool {
        self.presentation.as_ref().is_some_and(|p| p.hidden)
    }
}

impl DebugConfig {
    /// Whether the configuration asked not to be listed (#318). Same question
    /// as [`Compound::hidden`], and the picker's badge asks it of both kinds
    /// so the count and the list cannot disagree.
    pub fn hidden(&self) -> bool {
        self.presentation.as_ref().is_some_and(|p| p.hidden)
    }
}

/// Parse the `compounds` array of a launch.json body. Sibling key of
/// `configurations`, so this reads the same text [`parse_launch_json`] does.
/// Entries without a `name` or a non-empty `configurations` list are skipped —
/// VS Code refuses to run those too, and an empty compound would present as a
/// picker row that does nothing.
pub fn parse_compounds(text: &str, source: &'static str) -> Vec<Compound> {
    let Ok(v) = serde_json::from_str::<Value>(&crate::tasks::strip_jsonc(text)) else {
        return Vec::new();
    };
    let Some(Value::Array(list)) = v.get("compounds") else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|c| {
            let obj = c.as_object()?;
            let name = obj.get("name")?.as_str()?.to_string();
            // Members may be bare names or `{ "folder": …, "name": … }`
            // objects in multi-root workspaces; the folder is kept beside the
            // name (#1425) so the member resolves in that folder.
            //
            // Collect FALLIBLY: one unusable member drops the whole compound
            // rather than shrinking it to the members that happen to parse.
            // `resolve_compound` already refuses to launch a subset when a
            // member names no configuration; a malformed member is the same
            // situation one boundary earlier, and silently running two of the
            // three a user wrote is the outcome both guards exist to prevent.
            let members: Vec<(String, Option<String>)> = obj
                .get("configurations")?
                .as_array()?
                .iter()
                .map(|m| match m {
                    Value::String(s) => Some((s.clone(), None)),
                    Value::Object(o) => {
                        let name = o.get("name")?.as_str()?.to_string();
                        let folder = match o.get("folder") {
                            None | Some(Value::Null) => None,
                            Some(f) => Some(f.as_str()?.to_string()),
                        };
                        Some((name, folder))
                    }
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let (configurations, folders): (Vec<String>, Vec<Option<String>>) =
                members.into_iter().unzip();
            if configurations.is_empty() {
                return None;
            }
            // Only `name` and `configurations` are read above. Every other key
            // VS Code defines is dropped here, which was harmless while no
            // compound could launch and is not any more — so record which ones
            // ASK FOR SOMETHING rather than which ones are merely present.
            //
            // Presence is the wrong test. `stopAll: false` is VS Code's own
            // default written out, and `preLaunchTask: null` requests nothing;
            // croft honours both perfectly by doing nothing, so refusing to
            // launch would name a key whose behaviour it is already delivering.
            //
            // `stopAll` depends on ARITY. With one member it governs whether
            // ending a session ends the others, and there are no others, so
            // neither value can change the outcome and neither is a reason to
            // refuse. With several - which #310 made possible - `true` asks
            // for a teardown croft does not implement, and the siblings keep
            // running after a member ends. Documenting that mismatch does not
            // stop the launch, so it is recorded as unsupported below and
            // refused with a message naming the key, exactly as a malformed
            // one is. `false` and omission stay silent: both describe what
            // croft already does.
            let asks_for_something = |k: &str| match obj.get(k) {
                None | Some(Value::Null) => false,
                Some(Value::String(s)) => !s.is_empty(),
                Some(Value::Bool(b)) => *b,
                Some(Value::Object(o)) => !o.is_empty(),
                Some(_) => true,
            };
            // `preLaunchTask` left this list in #318: it is honoured now, so
            // naming it as a reason to refuse would report a limitation croft
            // no longer has. Its VALUE is captured instead - the key alone was
            // enough to refuse and is not enough to run anything.
            // `presentation` is no longer refused wholesale (#318): `hidden`
            // is a row filter and `group`/`order` place the row, all three
            // honoured by the picker's sort. Testing the SUB-KEYS rather than
            // the parent is what lets `{"hidden": true}` alone launch instead
            // of being refused for a capability it never asked for. Testing the SUB-KEYS rather than
            // the parent means `{"hidden": true}` alone launches instead of
            // being refused for a capability it never asked for.
            let presentation = parse_presentation(obj);
            // Only values croft cannot READ still refuse (#318). All three
            // keys are honoured, so what is left is a typo to fix rather than
            // a feature croft lacks: silently launching a compound whose
            // `{"hidden": "true"}` did nothing is a worse answer than the
            // refusal. Each is named WITH its parent, because a refusal
            // telling the user to "remove that key" would otherwise read as
            // deleting the whole `presentation` block, taking a working
            // `group` with it.
            let mut unsupported_keys: Vec<&'static str> = malformed_presentation_keys(obj);
            // A bool is honoured at any arity (#567): `true` tears the set
            // down when one member ends, `false` leaves the siblings running.
            // Anything else - the `"true"` quoted-bool typo above all - cannot
            // be read, and only matters where there ARE siblings to stop, so
            // it is refused there rather than launched as `false`.
            let stop_all = matches!(obj.get("stopAll"), Some(Value::Bool(true)));
            if configurations.len() > 1
                && !matches!(
                    obj.get("stopAll"),
                    None | Some(Value::Null) | Some(Value::Bool(_))
                )
            {
                unsupported_keys.push("stopAll");
            }
            let pre_launch_task = asks_for_something("preLaunchTask")
                .then(|| obj.get("preLaunchTask").and_then(|v| v.as_str()))
                .flatten()
                .map(str::to_string);
            // A non-STRING `preLaunchTask` asks for something and cannot be
            // read: `asks_for_something` passes `true` or `7`, and `as_str`
            // then drops it, so the compound would launch with the task
            // silently not run - debugging whatever the last build left
            // behind, the exact hazard the task exists to prevent.
            if pre_launch_task.is_none() && asks_for_something("preLaunchTask") {
                unsupported_keys.push("preLaunchTask");
            }
            Some(Compound {
                name,
                configurations,
                folders,
                source,
                pre_launch_task,
                presentation,
                unsupported_keys,
                stop_all,
            })
        })
        .collect()
}

/// Resolve a compound's member names against the configurations declared
/// beside it, in the compound's own order. Pass [`discover_configs_all`], not
/// [`discover_configs`]: the latter drops a lower-precedence duplicate by name,
/// which is exactly the entry a `.vscode` compound naming that name needs, so
/// the source preference below would have nothing to prefer. A member with a
/// `"folder"` (#1425) is looked up in that workspace folder's launch.json
/// instead, `folders` being the workspace's `(name, root)` pairs, and comes
/// back carrying that root. Errors naming the first member that does not
/// exist — launching the subset that happens to resolve would debug
/// something other than what the user asked for, and silently.
pub fn resolve_compound(
    compound: &Compound,
    configs: &[DebugConfig],
    folders: &[(String, PathBuf)],
) -> Result<Vec<DebugConfig>, String> {
    compound
        .configurations
        .iter()
        .enumerate()
        .map(|(i, want)| {
            if let Some(folder) = compound.folders.get(i).cloned().flatten() {
                let root = folder_root(folders, &folder).ok_or_else(|| {
                    format!(
                        "compound \"{}\" names folder \"{folder}\", which is not a folder of this workspace",
                        compound.name
                    )
                })?;
                let mut cfg = discover_configs_all(&root)
                    .into_iter()
                    .find(|c| &c.name == want)
                    .ok_or_else(|| {
                        format!(
                            "compound \"{}\" names configuration \"{want}\" in folder \"{folder}\", which no launch.json there declares",
                            compound.name
                        )
                    })?;
                cfg.root = Some(root);
                return Ok(cfg);
            }
            // The compound's own file first: `discover_configs` merges both
            // files with `.croft` winning a duplicate name, so a bare name
            // match would bind a `.vscode` compound to a `.croft` config that
            // may run a different program under a different adapter. Falling
            // back to any file keeps a compound working when its members live
            // in the sibling file.
            configs
                .iter()
                .find(|c| &c.name == want && c.source == compound.source)
                .or_else(|| configs.iter().find(|c| &c.name == want))
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "compound \"{}\" names configuration \"{}\", which no launch.json declares",
                        compound.name, want
                    )
                })
        })
        .collect()
}

/// The root of the workspace folder called `name` (#1425): VS Code names a
/// folder by its directory name.
fn folder_root(folders: &[(String, PathBuf)], name: &str) -> Option<PathBuf> {
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

/// Map a launch.json `type` onto the adapter family croft drives. Covers the
/// names the three bundled families are known under in existing repos.
pub fn adapter_for_type(type_name: &str) -> Option<AdapterKind> {
    match type_name {
        "python" | "debugpy" => Some(AdapterKind::Debugpy),
        "lldb" | "lldb-dap" | "cppdbg" | "codelldb" => Some(AdapterKind::LldbDap),
        "go" => Some(AdapterKind::Delve),
        "node" | "pwa-node" | "node-terminal" | "javascript" => Some(AdapterKind::JsDebug),
        _ => None,
    }
}

/// The editor state `${...}` variables resolve against.
#[derive(Debug, Clone)]
pub struct SubstCtx {
    pub workspace_folder: PathBuf,
    /// Active editor file; `None` makes `${file}`-family variables error.
    pub file: Option<PathBuf>,
    /// The workspace's folders as `(name, root)`, for
    /// `${workspaceFolder:name}` (#1425).
    pub folders: Vec<(String, PathBuf)>,
}

/// Expand the supported `${...}` variables in `input`. An unknown or
/// unresolvable variable is a hard error naming it — VS Code launches with
/// the literal text and the program fails obscurely; croft refuses loudly
/// instead (issue #250 acceptance).
pub fn substitute(input: &str, ctx: &SubstCtx) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            // No closing brace: not a variable, keep the text as written.
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let var = &after[..end];
        out.push_str(&expand_variable(var, ctx)?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn expand_variable(var: &str, ctx: &SubstCtx) -> Result<String, String> {
    let file = || {
        ctx.file
            .as_deref()
            .ok_or_else(|| format!("${{{var}}}: no active file"))
    };
    match var {
        "workspaceFolder" | "workspaceRoot" => {
            Ok(ctx.workspace_folder.to_string_lossy().into_owned())
        }
        "workspaceFolderBasename" => Ok(ctx
            .workspace_folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()),
        "file" => Ok(file()?.to_string_lossy().into_owned()),
        "fileBasename" => Ok(file()?
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()),
        "fileBasenameNoExtension" => Ok(file()?
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()),
        "fileDirname" => Ok(file()?
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()),
        "fileExtname" => Ok(file()?
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default()),
        "cwd" => Ok(ctx.workspace_folder.to_string_lossy().into_owned()),
        "pathSeparator" => Ok(String::from(std::path::MAIN_SEPARATOR_STR)),
        _ => {
            if let Some(name) = var.strip_prefix("env:") {
                return std::env::var(name)
                    .map_err(|_| format!("${{env:{name}}}: environment variable not set"));
            }
            // `${workspaceFolder:web}` (#1425): the root of the workspace
            // folder named `web`, as multi-root configurations write it.
            if let Some(name) = var.strip_prefix("workspaceFolder:") {
                return folder_root(&ctx.folders, name)
                    .map(|r| r.to_string_lossy().into_owned())
                    .ok_or_else(|| format!("${{{var}}}: no workspace folder is named \"{name}\""));
            }
            Err(format!("${{{var}}}: unsupported substitution variable"))
        }
    }
}

/// Recursively expand `${...}` in every string of `v` (object values and
/// array elements included), so passthrough fields get substitution too.
fn substitute_value(v: &Value, ctx: &SubstCtx) -> Result<Value, String> {
    Ok(match v {
        Value::String(s) => Value::String(substitute(s, ctx)?),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| substitute_value(x, ctx))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| Ok((k.clone(), substitute_value(x, ctx)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

/// A configuration with substitution done and the mapped fields split out,
/// ready to become a DAP request. `extra` carries every field croft didn't
/// map, already substituted, for verbatim passthrough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub name: String,
    pub kind: AdapterKind,
    pub request: RequestKind,
    pub program: Option<String>,
    /// debugpy `module` (`"module": "pytest"` style launches).
    pub module: Option<String>,
    pub args: Vec<String>,
    /// `env` merged over `envFile` (explicit `env` wins per key).
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub pre_launch_task: Option<String>,
    /// The task to run when this configuration's debug run ends (#250),
    /// by the user stopping it or the debuggee finishing, not on relaunch.
    pub post_debug_task: Option<String>,
    pub stop_on_entry: bool,
    pub port: Option<u16>,
    pub process_id: Option<i64>,
    /// `"processId": "${command:pickProcess}"` (#250): the user chooses the
    /// process at launch, so `process_id` is `None` until they have.
    pub pick_process: bool,
    pub extra: Map<String, Value>,
    /// The cppdbg keys lldb-dap ignores (#1394), carried with the resolved
    /// config so every launch path (a preLaunchTask resume, a compound
    /// member) can say so in the debug console once the session starts.
    pub ignored_keys: Vec<&'static str>,
}

/// The fields [`resolve`] consumes; everything else lands in `extra`.
const MAPPED_KEYS: &[&str] = &[
    "name",
    "type",
    "request",
    "program",
    "module",
    "args",
    "env",
    "envFile",
    "cwd",
    "preLaunchTask",
    "postDebugTask",
    "stopOnEntry",
    "port",
    "processId",
    // Forced to internalConsole: croft debugs in-process and declines the
    // `runInTerminal` reverse request, so honouring an integratedTerminal
    // console would hang the session at launch.
    "console",
];

/// Whether `cfg` is for debugging tests (#1398): a debugpy configuration
/// whose `purpose` names `"debug-test"`, as VS Code's Python extension
/// reads it (a list, or a single string).
pub fn is_debug_test_config(cfg: &DebugConfig) -> bool {
    let debug_test = match cfg.raw.get("purpose") {
        Some(Value::String(s)) => s == "debug-test",
        Some(Value::Array(list)) => list.iter().any(|v| v.as_str() == Some("debug-test")),
        _ => false,
    };
    debug_test && adapter_for_type(&cfg.type_name) == Some(AdapterKind::Debugpy)
}

/// The workspace's configuration for debugging tests (#1398): the first
/// that [`is_debug_test_config`], as VS Code takes it.
pub fn debug_test_config(configs: &[DebugConfig]) -> Option<&DebugConfig> {
    configs.iter().find(|c| is_debug_test_config(c))
}

/// Keys of a debug-test configuration that describe the configuration,
/// not the launch, so they never reach debugpy.
const DEBUG_TEST_META_KEYS: &[&str] = &["purpose", "presentation"];

/// Overlay a resolved debug-test configuration (#1398) onto croft's pytest
/// launch `request`: its `env`, `cwd`, `python`, `justMyCode` and every
/// other debugpy key replace croft's, and its `args` follow pytest's own.
/// pytest stays the module, croft's test selection stays first, and the
/// console stays croft's own.
pub fn with_debug_test_config(mut request: Value, rc: &ResolvedConfig) -> Value {
    let Some(args) = request["arguments"].as_object_mut() else {
        return request;
    };
    for (key, value) in &rc.extra {
        if !DEBUG_TEST_META_KEYS.contains(&key.as_str()) {
            args.insert(key.clone(), value.clone());
        }
    }
    if !rc.env.is_empty() {
        args.insert("env".into(), json!(rc.env));
    }
    if let Some(cwd) = &rc.cwd {
        args.insert("cwd".into(), json!(cwd.to_string_lossy()));
    }
    if let Some(list) = args.get_mut("args").and_then(Value::as_array_mut) {
        list.extend(rc.args.iter().map(|a| json!(a)));
    }
    if rc.stop_on_entry {
        args.insert("stopOnEntry".into(), json!(true));
    }
    request
}

/// cppdbg's spellings of mapped fields (#1394): `environment` joins `env`
/// and `stopAtEntry` is `stopOnEntry`.
const CPPDBG_MAPPED_KEYS: &[&str] = &["environment", "stopAtEntry"];

/// cppdbg keys for gdb or the C/C++ extension that lldb-dap, which runs a
/// cppdbg configuration in croft, has no use for (#1394).
const CPPDBG_IGNORED_KEYS: &[&str] = &[
    "MIMode",
    "miDebuggerPath",
    "miDebuggerArgs",
    "miDebuggerServerAddress",
    "setupCommands",
    "externalConsole",
    "targetArchitecture",
    "symbolSearchPath",
    "additionalSOLibSearchPath",
    "sourceFileMap",
    "visualizerFile",
    "logging",
];

/// The keys of a cppdbg configuration that lldb-dap ignores (#1394), in
/// the order croft lists them, so the debug console can say why a gdb
/// path or a pretty-printer `setupCommands` has no effect. Empty for any
/// other type.
pub fn ignored_cppdbg_keys(cfg: &DebugConfig) -> Vec<&'static str> {
    if cfg.type_name != "cppdbg" {
        return Vec::new();
    }
    CPPDBG_IGNORED_KEYS
        .iter()
        .copied()
        .filter(|k| cfg.raw.contains_key(*k))
        .collect()
}

/// Resolve `cfg` against the editor state: substitute variables, load the
/// `envFile`, and split the mapped fields. Errors are user-facing strings
/// (unknown type, unresolved variable, unreadable envFile).
pub fn resolve(cfg: &DebugConfig, ctx: &SubstCtx) -> Result<ResolvedConfig, String> {
    let kind = adapter_for_type(&cfg.type_name).ok_or_else(|| {
        format!(
            "config \"{}\": unsupported type \"{}\" (python, lldb, node and go families are supported)",
            cfg.name, cfg.type_name
        )
    })?;
    // `${command:pickProcess}` is not a substitution but a request for a
    // picker at launch (#250), so it is taken out before substitution would
    // reject it as an unknown variable.
    let mut raw_obj = cfg.raw.clone();
    let pick_process = raw_obj
        .get("processId")
        .and_then(Value::as_str)
        .map(str::trim)
        == Some("${command:pickProcess}");
    if pick_process {
        raw_obj.remove("processId");
    }
    let raw = Value::Object(raw_obj);
    let raw = substitute_value(&raw, ctx).map_err(|e| format!("config \"{}\": {e}", cfg.name))?;
    let obj = raw.as_object().expect("substitution preserves the object");

    let get_str = |key: &str| obj.get(key).and_then(Value::as_str).map(str::to_string);
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    if let Some(env_file) = get_str("envFile") {
        let path = absolute_in(&env_file, &ctx.workspace_folder);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("config \"{}\": envFile {}: {e}", cfg.name, path.display()))?;
        env.extend(parse_env_file(&text));
    }
    // cppdbg's own spelling (#1394): `environment` is a list of
    // `{ name, value }`, under `env`, which wins per key.
    let cppdbg = cfg.type_name == "cppdbg";
    if cppdbg && let Some(list) = obj.get("environment").and_then(Value::as_array) {
        for item in list {
            if let Some(name) = item.get("name").and_then(Value::as_str) {
                let value = match item.get("value") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Null) | None => String::new(),
                    Some(other) => other.to_string(),
                };
                env.insert(name.to_string(), value);
            }
        }
    }
    if let Some(map) = obj.get("env").and_then(Value::as_object) {
        for (k, v) in map {
            // VS Code allows non-string scalars here; stringify them.
            let val = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            env.insert(k.clone(), val);
        }
    }
    let args = obj
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|x| match x {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let port = obj
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok());
    let process_id = match obj.get("processId") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
        _ => None,
    };
    let mut extra: Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| !MAPPED_KEYS.contains(&k.as_str()))
        .filter(|(k, _)| !(cppdbg && CPPDBG_MAPPED_KEYS.contains(&k.as_str())))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if cfg.type_name == "node-terminal" && cfg.request == RequestKind::Launch {
        node_terminal_command(&mut extra);
    }
    Ok(ResolvedConfig {
        name: cfg.name.clone(),
        kind,
        request: cfg.request,
        program: get_str("program"),
        module: get_str("module"),
        args,
        env,
        // A member from another workspace folder (#1425) runs there when it
        // names no cwd, not in the active folder the launch would default to.
        cwd: get_str("cwd")
            .map(|c| absolute_in(&c, &ctx.workspace_folder))
            .or_else(|| cfg.root.clone()),
        pre_launch_task: get_str("preLaunchTask"),
        post_debug_task: get_str("postDebugTask"),
        stop_on_entry: obj
            .get("stopOnEntry")
            .or_else(|| obj.get("stopAtEntry").filter(|_| cppdbg))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        port,
        process_id,
        pick_process,
        extra,
        ignored_keys: ignored_cppdbg_keys(cfg),
    })
}

/// A `node-terminal` launch's `command` (#1391), as the runtime js-debug
/// starts: croft runs debuggees in its own console, not a terminal, so
/// `"command": "npm run dev"` becomes `runtimeExecutable: "npm"` and
/// `runtimeArgs: ["run", "dev"]`, and a command with shell syntax (`&&`,
/// `|`, `$VAR`) or a leading `NAME=value` assignment runs under `sh -c`.
/// js-debug attaches to the Node processes it starts either way. An
/// explicit `runtimeExecutable` wins.
fn node_terminal_command(extra: &mut Map<String, Value>) {
    let Some(Value::String(command)) = extra.remove("command") else {
        return;
    };
    if names_runtime_executable(extra) || command.trim().is_empty() {
        return;
    }
    let shell_syntax = command.contains(['&', '|', ';', '<', '>', '$', '`', '(', ')', '*', '?']);
    let words = match shlex::split(&command) {
        Some(words) if !shell_syntax && !words.is_empty() && !is_assignment(&words[0]) => words,
        _ => vec![String::from("sh"), String::from("-c"), command],
    };
    let mut words = words.into_iter().map(Value::String);
    extra.insert("runtimeExecutable".into(), words.next().expect("not empty"));
    let mut runtime_args: Vec<Value> = words.collect();
    if let Some(Value::Array(more)) = extra.get("runtimeArgs") {
        runtime_args.extend(more.iter().cloned());
    }
    extra.insert("runtimeArgs".into(), Value::Array(runtime_args));
}

/// Whether a shell word is a variable assignment (`NODE_ENV=production`),
/// which a shell applies to the command after it rather than running.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Whether `runtimeExecutable` names something to run: a nonempty string,
/// not `null` or `""`.
fn names_runtime_executable(extra: &Map<String, Value>) -> bool {
    matches!(extra.get("runtimeExecutable"), Some(Value::String(s)) if !s.trim().is_empty())
}

/// Whether a node launch names something to run (#1391): a `program`, or
/// a `runtimeExecutable` / `runtimeArgs` (an npm script, `node-terminal`'s
/// `command`), which js-debug starts on its own.
pub fn js_launch_has_target(rc: &ResolvedConfig) -> bool {
    rc.program.is_some()
        || names_runtime_executable(&rc.extra)
        || matches!(rc.extra.get("runtimeArgs"), Some(Value::Array(a)) if !a.is_empty())
}

/// The current user's processes as `(pid, command name)` from
/// `ps -x -o pid=,comm=` output (BSD-style flags, which both macOS and
/// procps accept), for the attach picker (#250). `skip` (croft's own pid) is
/// left out: attaching a debugger to the editor running it would freeze it.
pub fn parse_ps_processes(output: &str, skip: i64) -> Vec<(i64, String)> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (pid, comm) = line.split_once(char::is_whitespace)?;
            let pid: i64 = pid.parse().ok()?;
            let comm = comm.trim();
            (pid != skip && !comm.is_empty()).then(|| (pid, comm.to_string()))
        })
        .collect()
}

/// Resolve a possibly-relative path against the workspace folder (VS Code
/// resolves `envFile` and `cwd` the same way).
pub fn absolute_in(path: &str, workspace_folder: &Path) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        workspace_folder.join(p)
    }
}

/// Parse a dotenv-style file: `KEY=VALUE` lines, `#` comments, blank lines,
/// an optional `export ` prefix, and single/double quotes around the value.
pub fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        out.insert(key.to_string(), value.to_string());
    }
    out
}

/// Start from the passthrough fields and layer the mapped ones on top, so a
/// mapped field always wins over a duplicate left in `extra`.
fn base_arguments(rc: &ResolvedConfig) -> Map<String, Value> {
    let mut args = rc.extra.clone();
    if let Some(p) = &rc.program {
        args.insert("program".into(), json!(p));
    }
    if !rc.args.is_empty() {
        args.insert("args".into(), json!(rc.args));
    }
    if let Some(c) = &rc.cwd {
        args.insert("cwd".into(), json!(c.to_string_lossy()));
    }
    args.insert("stopOnEntry".into(), json!(rc.stop_on_entry));
    args
}

fn request_value(command: &str, arguments: Map<String, Value>) -> Value {
    json!({
        "type": "request",
        "command": command,
        "arguments": Value::Object(arguments),
    })
}

/// Build the debugpy `launch`/`attach` request. `project_python` is the
/// project's own interpreter (its venv's, as Run uses), which the program
/// runs under when the config names no `python` (or legacy `pythonPath`):
/// croft's debug venv only hosts the adapter, and debugpy injects itself
/// into the debuggee, so the project needs no debugpy of its own (#864).
pub fn debugpy_request(rc: &ResolvedConfig, project_python: &Path) -> Value {
    let mut args = base_arguments(rc);
    match rc.request {
        RequestKind::Launch => {
            args.insert("request".into(), json!("launch"));
            if let Some(m) = &rc.module {
                args.insert("module".into(), json!(m));
                // debugpy rejects a launch naming both program and module.
                args.remove("program");
            }
            if !rc.env.is_empty() {
                args.insert("env".into(), json!(rc.env));
            }
            // debugpy refuses a launch that names both spellings.
            if !args.contains_key("python") && !args.contains_key("pythonPath") {
                args.insert("python".into(), json!([project_python.to_string_lossy()]));
            }
            args.insert("console".into(), json!("internalConsole"));
            // debugpy's and VS Code's default (#1519): step through the
            // user's own code only. A config that sets it keeps its value.
            if !args.contains_key("justMyCode") {
                args.insert("justMyCode".into(), json!(true));
            }
            request_value("launch", args)
        }
        RequestKind::Attach => {
            args.insert("request".into(), json!("attach"));
            if let Some(port) = rc.port
                && !args.contains_key("connect")
            {
                let host = args.remove("host").unwrap_or_else(|| json!("127.0.0.1"));
                args.insert("connect".into(), json!({ "host": host, "port": port }));
            }
            request_value("attach", args)
        }
    }
}

/// Where a debugpy `attach` with `connect` goes (#1417): the address a
/// `debugpy --listen` / `debugpy.listen()` process serves. That port is
/// the debuggee's own adapter, so croft talks DAP to it directly, as VS
/// Code does, rather than putting a second adapter in front of it. `None`
/// for a launch, and for the `listen` form, where the debuggee connects
/// back to an adapter croft runs.
pub fn debugpy_connect_address(request: &Value) -> Option<(String, u16)> {
    if request.get("command").and_then(Value::as_str) != Some("attach") {
        return None;
    }
    let args = request.get("arguments")?;
    if args.get("listen").is_some() {
        return None;
    }
    let connect = args.get("connect")?;
    let port = match connect.get("port")? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .and_then(|p| u16::try_from(p).ok())?;
    let host = connect
        .get("host")
        .and_then(Value::as_str)
        .filter(|h| !h.trim().is_empty())
        .unwrap_or("127.0.0.1");
    Some((host.to_string(), port))
}

/// Build the lldb-dap `launch`/`attach` request. `env` uses the
/// `"VAR=VALUE"` string-array format lldb-dap has accepted since
/// lldb-vscode. Attach targets `pid`.
pub fn lldb_request(rc: &ResolvedConfig) -> Value {
    let mut args = base_arguments(rc);
    // CodeLLDB's build step (#1392), done before the launch; not lldb-dap's.
    args.remove("cargo");
    if !rc.env.is_empty() {
        let pairs: Vec<String> = rc.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        args.insert("env".into(), json!(pairs));
    }
    match rc.request {
        RequestKind::Launch => request_value("launch", args),
        RequestKind::Attach => {
            if let Some(pid) = rc.process_id {
                args.insert("pid".into(), json!(pid));
            }
            // Attaching runs an existing process; launch-only keys confuse
            // some lldb-dap builds.
            args.remove("stopOnEntry");
            request_value("attach", args)
        }
    }
}

/// Whether an lldb `attach` names something to attach to (#1419): a
/// `processId`, a `program` (lldb finds the running process by its
/// executable, or waits for it with `waitFor`), or `attachCommands` (such as
/// `gdb-remote host:port` for gdbserver, QEMU or an embedded probe).
pub fn lldb_attach_has_target(rc: &ResolvedConfig) -> bool {
    rc.process_id.is_some()
        || rc.program.as_deref().is_some_and(|p| !p.trim().is_empty())
        || rc
            .extra
            .get("attachCommands")
            .and_then(Value::as_array)
            .is_some_and(|c| !c.is_empty())
}

/// A CodeLLDB `cargo` block (#1392): the program is whatever executable
/// `cargo <args>` builds that matches `filter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoSpec {
    pub args: Vec<String>,
    /// `filter.name`: the target's name.
    pub name: Option<String>,
    /// `filter.kind`: one of the target's kinds (`bin`, `lib`, `test`, …).
    pub kind: Option<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
}

/// The `cargo` block of an lldb-family launch, if it has one; Err when it
/// is there but unreadable.
pub fn cargo_spec(rc: &ResolvedConfig) -> Option<Result<CargoSpec, String>> {
    let cargo = rc.extra.get("cargo")?;
    let strings = |v: Option<&Value>| -> Option<Vec<String>> {
        v?.as_array()?
            .iter()
            .map(|a| a.as_str().map(str::to_string))
            .collect()
    };
    let Some(args) = strings(cargo.get("args")).filter(|a| !a.is_empty()) else {
        return Some(Err(String::from(
            "\"cargo.args\" must list cargo's arguments (\"build\", …)",
        )));
    };
    let filter = cargo.get("filter");
    let text = |key: &str| {
        filter
            .and_then(|f| f.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let env = cargo
        .get("env")
        .and_then(Value::as_object)
        .map(|e| {
            e.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Some(Ok(CargoSpec {
        args,
        name: text("name"),
        kind: text("kind"),
        env,
        cwd: cargo.get("cwd").and_then(Value::as_str).map(PathBuf::from),
    }))
}

/// The cargo command line for `spec`, as CodeLLDB runs it: JSON messages,
/// and `test` builds only (`--no-run`).
pub fn cargo_build_args(spec: &CargoSpec) -> Vec<String> {
    let mut args = spec.args.clone();
    if args.first().map(String::as_str) == Some("test") && !args.iter().any(|a| a == "--no-run") {
        args.insert(1, String::from("--no-run"));
    }
    if !args.iter().any(|a| a.starts_with("--message-format")) {
        args.push(String::from("--message-format=json"));
    }
    args
}

/// The one executable in cargo's JSON output that matches the filter: a
/// `compiler-artifact` with an `executable`, whose target name is `name`
/// and whose kinds include `kind`, when those are set. None or several is
/// an error naming the filter.
pub fn cargo_artifact(output: &str, spec: &CargoSpec) -> Result<PathBuf, String> {
    let mut found: Vec<PathBuf> = Vec::new();
    for line in output.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(exe) = v.get("executable").and_then(Value::as_str) else {
            continue;
        };
        let target = v.get("target");
        let name_ok = spec.name.as_deref().is_none_or(|want| {
            target.and_then(|t| t.get("name")).and_then(Value::as_str) == Some(want)
        });
        let kind_ok = spec.kind.as_deref().is_none_or(|want| {
            target
                .and_then(|t| t.get("kind"))
                .and_then(Value::as_array)
                .is_some_and(|k| k.iter().any(|k| k.as_str() == Some(want)))
        });
        let exe = PathBuf::from(exe);
        if name_ok && kind_ok && !found.contains(&exe) {
            found.push(exe);
        }
    }
    let filter = match (&spec.name, &spec.kind) {
        (Some(n), Some(k)) => format!("name \"{n}\", kind \"{k}\""),
        (Some(n), None) => format!("name \"{n}\""),
        (None, Some(k)) => format!("kind \"{k}\""),
        (None, None) => String::from("no filter"),
    };
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(format!("cargo built no executable matching {filter}")),
        n => Err(format!(
            "cargo built {n} executables matching {filter} ({}): set \"cargo.filter\" to pick one",
            found
                .iter()
                .filter_map(|p| p.file_name())
                .map(|f| f.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Run `cargo` for `spec` in `cwd` and pick its executable. A failed build
/// is an error carrying cargo's first error line. Blocking: the App runs
/// this on a background thread.
pub fn build_cargo_program(spec: &CargoSpec, cwd: &Path) -> Result<PathBuf, String> {
    let dir = spec.cwd.as_deref().map_or_else(
        || cwd.to_path_buf(),
        |c| absolute_in(&c.to_string_lossy(), cwd),
    );
    let out = std::process::Command::new("cargo")
        .args(cargo_build_args(spec))
        .envs(&spec.env)
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cargo: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        // With JSON messages, the compile errors are on stdout; cargo's own
        // complaints (bad flags, no Cargo.toml) on stderr.
        let rendered = stdout.lines().find_map(|l| {
            let v: Value = serde_json::from_str(l).ok()?;
            let m = v.get("message")?;
            (m.get("level")?.as_str()? == "error")
                .then(|| m.get("message")?.as_str().map(str::to_string))?
        });
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = rendered
            .or_else(|| {
                stderr
                    .lines()
                    .find(|l| l.starts_with("error"))
                    .map(str::to_string)
            })
            .unwrap_or_else(|| stderr.lines().last().unwrap_or("").to_string());
        return Err(format!(
            "cargo {} failed: {}",
            spec.args.join(" "),
            why.trim()
        ));
    }
    cargo_artifact(&stdout, spec)
}

/// Where a Go "Connect to server" attach finds its headless delve (#1418):
/// `"mode": "remote"` with a `port`, at `host` (default `127.0.0.1`). `None`
/// for a launch, a local attach, or a remote one with no port.
pub fn delve_remote_address(rc: &ResolvedConfig) -> Option<(String, u16)> {
    if rc.request != RequestKind::Attach
        || rc.extra.get("mode").and_then(Value::as_str) != Some("remote")
    {
        return None;
    }
    let host = rc
        .extra
        .get("host")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .unwrap_or("127.0.0.1");
    Some((host.to_string(), rc.port?))
}

/// Build the delve `launch`/`attach` request (#264). `mode` passes through
/// from the config and defaults to delve's own defaults: `debug` to launch
/// (build and run the package at `program`), `local` to attach to `processId`.
pub fn delve_request(rc: &ResolvedConfig) -> Value {
    let mut args = base_arguments(rc);
    if !rc.env.is_empty() {
        args.insert("env".into(), json!(rc.env));
    }
    match rc.request {
        RequestKind::Launch => {
            args.entry("mode").or_insert_with(|| json!("debug"));
            request_value("launch", args)
        }
        RequestKind::Attach => {
            args.entry("mode").or_insert_with(|| json!("local"));
            if let Some(pid) = rc.process_id {
                args.insert("processId".into(), json!(pid));
            }
            args.remove("stopOnEntry");
            request_value("attach", args)
        }
    }
}

/// Build the vscode-js-debug `launch`/`attach` request (`pwa-node`, the
/// canonical Node type — croft already normalises onto it for zero-config).
pub fn js_request(rc: &ResolvedConfig) -> Value {
    let mut args = base_arguments(rc);
    args.insert("type".into(), json!("pwa-node"));
    if !rc.env.is_empty() {
        args.insert("env".into(), json!(rc.env));
    }
    match rc.request {
        RequestKind::Launch => {
            args.insert("request".into(), json!("launch"));
            args.insert("console".into(), json!("internalConsole"));
            if !args.contains_key("sourceMaps") {
                args.insert("sourceMaps".into(), json!(true));
            }
            request_value("launch", args)
        }
        RequestKind::Attach => {
            args.insert("request".into(), json!("attach"));
            if let Some(port) = rc.port {
                args.insert("port".into(), json!(port));
            }
            if !args.contains_key("address") {
                args.insert("address".into(), json!("127.0.0.1"));
            }
            args.remove("stopOnEntry");
            request_value("attach", args)
        }
    }
}

/// A field of the **Debug: Add Configuration…** flow (#250), in the order
/// [`ConfigDraft::next_field`] asks for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftField {
    Name,
    Program,
    Args,
    Cwd,
    Env,
    PreLaunchTask,
    /// The port a python or node debuggee listens on (attach).
    Port,
    /// The process id to attach lldb or delve to.
    ProcessId,
}

/// The adapters the flow offers: `(type, label)`, the type as written into
/// launch.json. One per [`AdapterKind`], so every choice resolves.
pub const DRAFT_TYPES: [(&str, &str); 4] = [
    ("python", "Python — debugpy"),
    ("lldb", "Rust / C / C++ — lldb-dap"),
    ("go", "Go — delve"),
    ("node", "JavaScript / TypeScript — Node"),
];

/// A configuration being built by **Debug: Add Configuration…** (#250):
/// the native form for what a launch.json entry says, written to
/// `.croft/launch.json` when it is complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigDraft {
    pub type_name: String,
    pub request: RequestKind,
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub pre_launch_task: String,
    pub port: Option<u16>,
    pub process_id: Option<u32>,
}

impl ConfigDraft {
    pub fn new(type_name: &str, request: RequestKind) -> Self {
        Self {
            type_name: type_name.to_string(),
            request,
            name: String::new(),
            program: String::new(),
            args: Vec::new(),
            cwd: String::new(),
            env: Vec::new(),
            pre_launch_task: String::new(),
            port: None,
            process_id: None,
        }
    }

    /// Attaching to python or node means a port the debuggee listens on;
    /// lldb and delve attach to a process.
    fn attaches_by_port(&self) -> bool {
        matches!(
            adapter_for_type(&self.type_name),
            Some(AdapterKind::Debugpy | AdapterKind::JsDebug)
        )
    }

    /// The field to ask for after `after` (`None`: the first), or `None`
    /// when the draft is complete.
    pub fn next_field(&self, after: Option<DraftField>) -> Option<DraftField> {
        use DraftField::*;
        let order: &[DraftField] = match self.request {
            RequestKind::Launch => &[Name, Program, Args, Cwd, Env, PreLaunchTask],
            RequestKind::Attach if self.attaches_by_port() => &[Name, Port],
            RequestKind::Attach => &[Name, ProcessId],
        };
        match after {
            None => order.first().copied(),
            Some(f) => order
                .iter()
                .position(|&o| o == f)
                .and_then(|i| order.get(i + 1).copied()),
        }
    }

    /// Whether `field` may be left blank (then it is left out).
    pub fn optional(field: DraftField) -> bool {
        matches!(
            field,
            DraftField::Args | DraftField::Cwd | DraftField::Env | DraftField::PreLaunchTask
        )
    }

    /// The prompt's title, hint, and the value it starts with.
    pub fn prompt(&self, field: DraftField) -> (String, &'static str, String) {
        let seed = match field {
            DraftField::Name => {
                let label = DRAFT_TYPES
                    .iter()
                    .find(|(t, _)| *t == self.type_name)
                    .map_or(self.type_name.as_str(), |(_, l)| {
                        l.split(" — ").next().unwrap_or(l)
                    });
                match self.request {
                    RequestKind::Launch => format!("Launch {label}"),
                    RequestKind::Attach => format!("Attach to {label}"),
                }
            }
            DraftField::Program => match adapter_for_type(&self.type_name) {
                Some(AdapterKind::LldbDap) => String::from("${workspaceFolder}/target/debug/"),
                Some(AdapterKind::Delve) => String::from("${workspaceFolder}"),
                _ => String::from("${file}"),
            },
            DraftField::Cwd => String::from("${workspaceFolder}"),
            _ => String::new(),
        };
        let (title, hint) = match field {
            DraftField::Name => ("Name", "What the configuration is called"),
            DraftField::Program => ("Program", "The program to debug"),
            DraftField::Args => (
                "Arguments",
                "Space-separated, \"quoted\" for spaces; Enter on blank for none",
            ),
            DraftField::Cwd => ("Working directory", "Enter on blank for the default"),
            DraftField::Env => ("Environment", "NAME=value pairs; Enter on blank for none"),
            DraftField::PreLaunchTask => (
                "Pre-launch task",
                "A task label to run first; Enter on blank for none",
            ),
            DraftField::Port => ("Port", "The port the debuggee listens on"),
            DraftField::ProcessId => ("Process id", "The process to attach to"),
        };
        (format!("Add Configuration: {title}"), hint, seed)
    }

    /// Take the typed value for `field`, or say why it does not fit.
    pub fn set(&mut self, field: DraftField, value: &str) -> Result<(), String> {
        let value = value.trim();
        match field {
            DraftField::Name => self.name = value.to_string(),
            DraftField::Program => self.program = value.to_string(),
            DraftField::Args => self.args = split_args(value)?,
            DraftField::Cwd => self.cwd = value.to_string(),
            DraftField::Env => self.env = parse_env_pairs(value)?,
            DraftField::PreLaunchTask => self.pre_launch_task = value.to_string(),
            DraftField::Port => {
                self.port = Some(
                    value
                        .parse::<u16>()
                        .ok()
                        .filter(|p| *p > 0)
                        .ok_or_else(|| format!("Not a port: {value}"))?,
                );
            }
            DraftField::ProcessId => {
                self.process_id = Some(
                    value
                        .parse::<u32>()
                        .ok()
                        .filter(|p| *p > 0)
                        .ok_or_else(|| format!("Not a process id: {value}"))?,
                );
            }
        }
        Ok(())
    }

    /// The launch.json entry, in the fields [`resolve`] maps: python and
    /// node attach by `port`, lldb and delve by `processId` (delve in its
    /// `local` mode).
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("name".into(), json!(self.name));
        o.insert("type".into(), json!(self.type_name));
        let request = match self.request {
            RequestKind::Launch => "launch",
            RequestKind::Attach => "attach",
        };
        o.insert("request".into(), json!(request));
        let adapter = adapter_for_type(&self.type_name);
        match self.request {
            RequestKind::Launch => {
                o.insert("program".into(), json!(self.program));
                if !self.args.is_empty() {
                    o.insert("args".into(), json!(self.args));
                }
                if !self.cwd.is_empty() {
                    o.insert("cwd".into(), json!(self.cwd));
                }
                if !self.env.is_empty() {
                    let env: Map<String, Value> = self
                        .env
                        .iter()
                        .map(|(k, v)| (k.clone(), json!(v)))
                        .collect();
                    o.insert("env".into(), Value::Object(env));
                }
                if !self.pre_launch_task.is_empty() {
                    o.insert("preLaunchTask".into(), json!(self.pre_launch_task));
                }
            }
            RequestKind::Attach => {
                if self.attaches_by_port() {
                    o.insert("port".into(), json!(self.port));
                } else {
                    if adapter == Some(AdapterKind::Delve) {
                        o.insert("mode".into(), json!("local"));
                    }
                    o.insert("processId".into(), json!(self.process_id));
                }
            }
        }
        Value::Object(o)
    }
}

/// Split an argument line the way a shell would for the simple cases:
/// whitespace separates, and "double" or 'single' quotes keep spaces.
fn split_args(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_arg = false;
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_arg = true;
            }
            None if c.is_whitespace() => {
                if in_arg {
                    out.push(std::mem::take(&mut cur));
                    in_arg = false;
                }
            }
            None => {
                cur.push(c);
                in_arg = true;
            }
        }
    }
    if quote.is_some() {
        return Err(String::from("Unclosed quote in the arguments"));
    }
    if in_arg {
        out.push(cur);
    }
    Ok(out)
}

/// `NAME=value` pairs, separated by whitespace or `;`. A value keeps
/// everything after the first `=`.
fn parse_env_pairs(line: &str) -> Result<Vec<(String, String)>, String> {
    split_args(&line.replace(';', " "))?
        .into_iter()
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) if !k.is_empty() => Ok((k.to_string(), v.to_string())),
            _ => Err(format!("Not NAME=value: {pair}")),
        })
        .collect()
}

/// Add `draft` to `.croft/launch.json` under `root`, creating the file when
/// there is none; returns the file written.
///
/// croft owns that file, so it is rewritten whole, but only when it parses
/// as plain JSON: a file with comments or trailing commas would lose them,
/// so it is left alone and the entry is handed back to add by hand. A name
/// the file already uses is refused, since the picker lists by name.
///
/// The new file is written aside and renamed into place, so a kill or a full
/// disk mid-write leaves the old one whole. The workspace controls `.croft`,
/// so a symlink there (or at the file) is refused rather than followed: the
/// rewrite must not land somewhere else.
pub fn add_to_croft_launch_json(root: &Path, draft: &ConfigDraft) -> Result<PathBuf, String> {
    let dir = root.join(".croft");
    let path = dir.join("launch.json");
    for p in [&dir, &path] {
        if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!(
                "{} is a symlink; not rewriting it",
                p.strip_prefix(root).unwrap_or(p).display()
            ));
        }
    }
    let entry = draft.to_json();
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Value>(&text).map_err(|_| {
            format!(
                ".croft/launch.json has comments or trailing commas, which rewriting it would lose; add this by hand: {entry}"
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json!({ "version": "0.2.0", "configurations": [] })
        }
        Err(e) => return Err(format!("Could not read .croft/launch.json: {e}")),
    };
    let list = match &mut doc {
        Value::Array(list) => list,
        Value::Object(o) => match o
            .entry("configurations")
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            Value::Array(list) => list,
            _ => {
                return Err(String::from(
                    ".croft/launch.json: \"configurations\" is not a list",
                ));
            }
        },
        _ => {
            return Err(String::from(
                ".croft/launch.json is neither a list nor an object",
            ));
        }
    };
    if list
        .iter()
        .any(|c| c.get("name").and_then(Value::as_str) == Some(draft.name.as_str()))
    {
        return Err(format!(
            ".croft/launch.json already has a configuration named \"{}\"",
            draft.name
        ));
    }
    list.push(entry);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create .croft: {e}"))?;
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    let (tmp, mut file) =
        create_fresh_temp(&dir).map_err(|e| format!("Could not write .croft/launch.json: {e}"))?;
    use std::io::Write as _;
    file.write_all((text + "\n").as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("Could not write .croft/launch.json: {e}")
        })?;
    Ok(path)
}

/// The name [`create_fresh_temp`] tries `n`th for this process.
fn temp_candidate(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("launch.json.{}.{n}.tmp", std::process::id()))
}

/// A new, empty temp file beside `launch.json` that nothing else could have
/// put there: created exclusively, so an existing path (a symlink the
/// workspace planted at a guessable name included) is never opened or
/// followed; the next name is tried instead.
fn create_fresh_temp(dir: &Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    for n in 0..64 {
        let tmp = temp_candidate(dir, n);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free temporary file name beside it",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SubstCtx {
        SubstCtx {
            workspace_folder: PathBuf::from("/work/proj"),
            file: Some(PathBuf::from("/work/proj/src/main.py")),
            folders: Vec::new(),
        }
    }

    fn config(json_text: &str) -> DebugConfig {
        parse_launch_json(json_text, ".vscode/launch.json")
            .into_iter()
            .next()
            .expect("one config")
    }

    #[test]
    fn parses_vscode_shape_bare_arrays_and_jsonc() {
        let vscode = r#"{
  // launch.json allows comments
  "version": "0.2.0",
  "configurations": [
    { "name": "Run API", "type": "python", "request": "launch", "program": "api.py", },
  ],
}"#;
        let cfgs = parse_launch_json(vscode, ".vscode/launch.json");
        assert_eq!(cfgs.len(), 1);
        assert_eq!(cfgs[0].name, "Run API");
        assert_eq!(cfgs[0].request, RequestKind::Launch);

        let bare = r#"[ { "name": "A", "type": "lldb", "request": "attach", "pid": 1 } ]"#;
        let cfgs = parse_launch_json(bare, ".croft/launch.json");
        assert_eq!(cfgs.len(), 1);
        assert_eq!(cfgs[0].request, RequestKind::Attach);
    }

    #[test]
    fn entries_without_name_or_type_are_skipped_and_request_defaults_to_launch() {
        let text = r#"{ "configurations": [
            { "type": "python", "program": "a.py" },
            { "name": "no type" },
            { "name": "ok", "type": "node", "program": "s.js" }
        ]}"#;
        let cfgs = parse_launch_json(text, ".vscode/launch.json");
        assert_eq!(cfgs.len(), 1);
        assert_eq!(cfgs[0].name, "ok");
        assert_eq!(cfgs[0].request, RequestKind::Launch);
    }

    #[test]
    fn croft_file_wins_duplicate_names_and_both_files_are_read() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
        std::fs::write(
            tmp.path().join(".croft/launch.json"),
            r#"[ { "name": "Serve", "type": "python", "program": "croft.py" } ]"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join(".vscode/launch.json"),
            r#"{ "configurations": [
                { "name": "Serve", "type": "python", "program": "vscode.py" },
                { "name": "Worker", "type": "node", "program": "w.js" }
            ]}"#,
        )
        .unwrap();
        let cfgs = discover_configs(tmp.path());
        assert_eq!(cfgs.len(), 2);
        assert_eq!(cfgs[0].name, "Serve");
        assert_eq!(cfgs[0].source, ".croft/launch.json");
        assert_eq!(cfgs[0].raw["program"], "croft.py");
        assert_eq!(cfgs[1].name, "Worker");
    }

    /// #264: a `type: "go"` config defaults delve's `mode` the way delve
    /// does, and an explicit `mode` (e.g. `test`) passes through.
    #[test]
    fn delve_request_defaults_mode_and_passes_an_explicit_one_through() {
        let launch = resolve(
            &config(
                r#"[{ "name": "G", "type": "go", "request": "launch", "program": "./cmd/app" }]"#,
            ),
            &ctx(),
        )
        .unwrap();
        assert_eq!(launch.kind, AdapterKind::Delve);
        let req = delve_request(&launch);
        assert_eq!(req["command"], "launch");
        assert_eq!(req["arguments"]["mode"], "debug");
        assert_eq!(req["arguments"]["program"], "./cmd/app");

        let test = resolve(
            &config(r#"[{ "name": "T", "type": "go", "request": "launch", "mode": "test", "program": "." }]"#),
            &ctx(),
        )
        .unwrap();
        assert_eq!(delve_request(&test)["arguments"]["mode"], "test");

        let attach = resolve(
            &config(r#"[{ "name": "A", "type": "go", "request": "attach", "processId": 77 }]"#),
            &ctx(),
        )
        .unwrap();
        let req = delve_request(&attach);
        assert_eq!(req["command"], "attach");
        assert_eq!(req["arguments"]["mode"], "local");
        assert_eq!(req["arguments"]["processId"], 77);
        assert!(req["arguments"].get("stopOnEntry").is_none());
    }

    /// #1418: the Go extension's "Connect to server" config (attach, mode
    /// remote, host and port) targets that headless delve, and its request
    /// keeps `mode: "remote"`.
    #[test]
    fn a_go_remote_attach_targets_the_headless_delve() {
        let rc = resolve(
            &config(
                r#"[{ "name": "Connect to server", "type": "go", "request": "attach",
                      "mode": "remote", "port": 2345, "host": "10.0.0.5",
                      "substitutePath": [{ "from": "${workspaceFolder}", "to": "/app" }] }]"#,
            ),
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            delve_remote_address(&rc),
            Some((String::from("10.0.0.5"), 2345))
        );
        let req = delve_request(&rc);
        assert_eq!(req["command"], "attach");
        assert_eq!(req["arguments"]["mode"], "remote");
        assert_eq!(
            req["arguments"]["substitutePath"][0]["from"], "/work/proj",
            "path mapping passes through"
        );
        assert!(req["arguments"].get("processId").is_none());

        let local_host = resolve(
            &config(r#"[{ "name": "C", "type": "go", "request": "attach", "mode": "remote", "port": 2345 }]"#),
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            delve_remote_address(&local_host),
            Some((String::from("127.0.0.1"), 2345))
        );
    }

    /// #1418, negative: a remote attach with no port, a local attach and a
    /// launch have no headless delve to connect to.
    #[test]
    fn only_a_remote_go_attach_with_a_port_connects_to_a_server() {
        for text in [
            r#"[{ "name": "A", "type": "go", "request": "attach", "mode": "remote" }]"#,
            r#"[{ "name": "A", "type": "go", "request": "attach", "processId": 77, "port": 2345 }]"#,
            r#"[{ "name": "A", "type": "go", "request": "launch", "mode": "remote", "port": 2345, "program": "." }]"#,
        ] {
            let rc = resolve(&config(text), &ctx()).unwrap();
            assert_eq!(delve_remote_address(&rc), None, "{text}");
        }
    }

    #[test]
    fn maps_the_adapter_families_and_rejects_unknown_types() {
        use AdapterKind::*;
        for (t, k) in [
            ("python", Debugpy),
            ("debugpy", Debugpy),
            ("lldb", LldbDap),
            ("cppdbg", LldbDap),
            ("node", JsDebug),
            ("pwa-node", JsDebug),
            ("go", Delve),
        ] {
            assert_eq!(adapter_for_type(t), Some(k), "{t}");
        }
        let cfg = config(r#"[{ "name": "J", "type": "java", "program": "Main.java" }]"#);
        let err = resolve(&cfg, &ctx()).unwrap_err();
        assert!(err.contains("unsupported type \"java\""), "{err}");
        assert!(err.contains("go"), "the supported list names go: {err}");
    }

    /// `${command:pickProcess}` asks for a picker at launch (#250) rather
    /// than failing as an unknown variable; a literal pid is unchanged.
    #[test]
    fn pick_process_resolves_to_a_picker_request_not_an_error() {
        let cfg = config(
            r#"[{ "name": "Attach", "type": "lldb", "request": "attach",
                 "processId": "${command:pickProcess}" }]"#,
        );
        let rc = resolve(&cfg, &ctx()).expect("pickProcess resolves");
        assert!(rc.pick_process);
        assert_eq!(rc.process_id, None);
        assert!(!rc.extra.contains_key("processId"));

        let cfg = config(
            r#"[{ "name": "Attach", "type": "lldb", "request": "attach", "processId": 4242 }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        assert!(!rc.pick_process);
        assert_eq!(rc.process_id, Some(4242));
    }

    #[test]
    fn ps_output_parses_to_pids_and_names_without_our_own() {
        let out = "  101 /usr/bin/zsh\n  202 server\n\n  303 croft\n bad line\n  404 \n";
        assert_eq!(
            parse_ps_processes(out, 303),
            vec![
                (101, String::from("/usr/bin/zsh")),
                (202, String::from("server"))
            ]
        );
    }

    #[test]
    fn substitutes_workspace_and_file_variables() {
        let c = ctx();
        assert_eq!(
            substitute("${workspaceFolder}/bin", &c).unwrap(),
            "/work/proj/bin"
        );
        assert_eq!(
            substitute("${workspaceFolderBasename}", &c).unwrap(),
            "proj"
        );
        assert_eq!(substitute("${file}", &c).unwrap(), "/work/proj/src/main.py");
        assert_eq!(substitute("${fileBasename}", &c).unwrap(), "main.py");
        assert_eq!(
            substitute("${fileBasenameNoExtension}", &c).unwrap(),
            "main"
        );
        assert_eq!(substitute("${fileDirname}", &c).unwrap(), "/work/proj/src");
        assert_eq!(substitute("${fileExtname}", &c).unwrap(), ".py");
        assert_eq!(
            substitute("a ${fileBasename} b ${fileBasename}", &c).unwrap(),
            "a main.py b main.py"
        );
        // Text without variables (or with an unclosed brace) passes through.
        assert_eq!(substitute("plain", &c).unwrap(), "plain");
        assert_eq!(substitute("odd ${file", &c).unwrap(), "odd ${file");
    }

    #[test]
    fn env_variable_expands_and_unknowns_error_loudly() {
        let c = ctx();
        // SAFETY: test-only env mutation, name is unique to this test.
        unsafe { std::env::set_var("CROFT_TEST_SUBST_VAR", "xyz") };
        assert_eq!(
            substitute("${env:CROFT_TEST_SUBST_VAR}", &c).unwrap(),
            "xyz"
        );
        let err = substitute("${env:CROFT_TEST_UNSET_VAR}", &c).unwrap_err();
        assert!(err.contains("CROFT_TEST_UNSET_VAR"), "{err}");
        let err = substitute("${command:pickProcess}", &c).unwrap_err();
        assert!(err.contains("command:pickProcess"), "{err}");
        // ${file} family errors when no file is active.
        let no_file = SubstCtx {
            workspace_folder: PathBuf::from("/w"),
            file: None,
            folders: Vec::new(),
        };
        assert!(substitute("${fileBasename}", &no_file).is_err());
    }

    #[test]
    fn resolve_splits_mapped_fields_and_passes_the_rest_through() {
        let cfg = config(
            r#"[{
                "name": "API",
                "type": "python",
                "request": "launch",
                "program": "${workspaceFolder}/api.py",
                "args": ["--port", 8080],
                "env": { "MODE": "dev", "RETRIES": 3 },
                "cwd": "${workspaceFolder}",
                "preLaunchTask": "build",
                "postDebugTask": "cleanup",
                "stopOnEntry": true,
                "justMyCode": true,
                "subProcess": "${workspaceFolderBasename}"
            }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        assert_eq!(rc.program.as_deref(), Some("/work/proj/api.py"));
        assert_eq!(rc.args, vec!["--port", "8080"]);
        assert_eq!(rc.env["MODE"], "dev");
        assert_eq!(rc.env["RETRIES"], "3");
        assert_eq!(rc.cwd.as_deref(), Some(Path::new("/work/proj")));
        assert_eq!(rc.pre_launch_task.as_deref(), Some("build"));
        // A mapped key, so croft runs it rather than handing it to the adapter.
        assert_eq!(rc.post_debug_task.as_deref(), Some("cleanup"));
        assert!(!rc.extra.contains_key("postDebugTask"));
        assert!(rc.stop_on_entry);
        // Unmapped fields survive, substituted, for verbatim passthrough.
        assert_eq!(rc.extra["justMyCode"], json!(true));
        assert_eq!(rc.extra["subProcess"], json!("proj"));
        assert!(!rc.extra.contains_key("program"));
    }

    #[test]
    fn a_relative_cwd_resolves_against_the_workspace_folder() {
        let cfg =
            config(r#"[{ "name": "R", "type": "python", "program": "a.py", "cwd": "backend" }]"#);
        let rc = resolve(&cfg, &ctx()).unwrap();
        assert_eq!(rc.cwd.as_deref(), Some(Path::new("/work/proj/backend")));
        // An absolute cwd is kept as written.
        let cfg =
            config(r#"[{ "name": "R", "type": "python", "program": "a.py", "cwd": "/srv/app" }]"#);
        let rc = resolve(&cfg, &ctx()).unwrap();
        assert_eq!(rc.cwd.as_deref(), Some(Path::new("/srv/app")));
    }

    #[test]
    fn env_file_merges_under_explicit_env() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".env"),
            "# comment\nexport FROM_FILE=1\nSHARED=file\nQUOTED=\"a b\"\n",
        )
        .unwrap();
        let cfg = config(
            r#"[{
                "name": "E", "type": "python", "program": "a.py",
                "envFile": "${workspaceFolder}/.env",
                "env": { "SHARED": "config" }
            }]"#,
        );
        let c = SubstCtx {
            workspace_folder: tmp.path().to_path_buf(),
            file: None,
            folders: Vec::new(),
        };
        let rc = resolve(&cfg, &c).unwrap();
        assert_eq!(rc.env["FROM_FILE"], "1");
        assert_eq!(rc.env["QUOTED"], "a b");
        assert_eq!(rc.env["SHARED"], "config", "explicit env wins per key");
        // A missing envFile is a hard, named error.
        let cfg = config(
            r#"[{ "name": "E", "type": "python", "program": "a.py", "envFile": "/nope/.env" }]"#,
        );
        let err = resolve(&cfg, &c).unwrap_err();
        assert!(err.contains("/nope/.env"), "{err}");
    }

    /// #1519: a launch.json configuration that does not set `justMyCode`
    /// steps only through the user's code, debugpy's and VS Code's default,
    /// instead of croft forcing it off.
    #[test]
    fn a_debugpy_config_without_just_my_code_steps_through_user_code_only() {
        let cfg = config(r#"[{ "name": "API", "type": "python", "program": "/p/api.py" }]"#);
        let rc = resolve(&cfg, &ctx()).unwrap();
        let req = debugpy_request(&rc, Path::new("/venv/bin/python"));
        assert_eq!(req["arguments"]["justMyCode"], true);
    }

    /// Negative: a configuration that turns `justMyCode` off (to step into
    /// a library) keeps it off.
    #[test]
    fn a_debugpy_config_can_still_step_into_libraries() {
        let cfg = config(
            r#"[{ "name": "API", "type": "python", "program": "/p/api.py", "justMyCode": false }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        let req = debugpy_request(&rc, Path::new("/venv/bin/python"));
        assert_eq!(req["arguments"]["justMyCode"], false);
    }

    #[test]
    fn debugpy_launch_request_carries_env_args_and_forces_internal_console() {
        let cfg = config(
            r#"[{
                "name": "API", "type": "python", "program": "/p/api.py",
                "args": ["--serve"], "env": { "MODE": "dev" },
                "console": "integratedTerminal"
            }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        let req = debugpy_request(&rc, Path::new("/venv/bin/python"));
        assert_eq!(req["command"], "launch");
        assert_eq!(req["arguments"]["program"], "/p/api.py");
        assert_eq!(req["arguments"]["args"][0], "--serve");
        assert_eq!(req["arguments"]["env"]["MODE"], "dev");
        assert_eq!(req["arguments"]["python"][0], "/venv/bin/python");
        // integratedTerminal would hang on the declined runInTerminal.
        assert_eq!(req["arguments"]["console"], "internalConsole");
    }

    #[test]
    fn debugpy_module_launch_drops_program_and_attach_builds_connect() {
        let cfg = config(
            r#"[{ "name": "T", "type": "python", "module": "pytest", "args": ["-k", "x"] }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        let req = debugpy_request(&rc, Path::new("/venv/bin/python"));
        assert_eq!(req["arguments"]["module"], "pytest");
        assert!(req["arguments"].get("program").is_none());

        let cfg =
            config(r#"[{ "name": "A", "type": "python", "request": "attach", "port": 5678 }]"#);
        let rc = resolve(&cfg, &ctx()).unwrap();
        let req = debugpy_request(&rc, Path::new("/venv/bin/python"));
        assert_eq!(req["command"], "attach");
        assert_eq!(req["arguments"]["connect"]["port"], 5678);
        assert_eq!(req["arguments"]["connect"]["host"], "127.0.0.1");
    }

    /// #864: the project interpreter is only the default. A config naming
    /// its own `python` keeps it, and one using the legacy `pythonPath`
    /// gets no `python` beside it, which debugpy would refuse.
    #[test]
    fn a_configs_own_python_wins_over_the_project_interpreter() {
        let cfg = config(
            r#"[{ "name": "P", "type": "python", "program": "a.py", "python": "/opt/py/bin/python" }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        let req = debugpy_request(&rc, Path::new("/proj/.venv/bin/python"));
        assert_eq!(req["arguments"]["python"], "/opt/py/bin/python");

        let cfg = config(
            r#"[{ "name": "L", "type": "python", "program": "a.py", "pythonPath": "/opt/py/bin/python" }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        let req = debugpy_request(&rc, Path::new("/proj/.venv/bin/python"));
        assert!(req["arguments"].get("python").is_none());
        assert_eq!(req["arguments"]["pythonPath"], "/opt/py/bin/python");
    }

    #[test]
    fn lldb_requests_use_kv_env_and_attach_by_pid() {
        let cfg = config(
            r#"[{
                "name": "Bin", "type": "lldb", "program": "/t/debug/app",
                "args": ["--flag"], "env": { "RUST_LOG": "debug" }
            }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        let req = lldb_request(&rc);
        assert_eq!(req["command"], "launch");
        assert_eq!(req["arguments"]["program"], "/t/debug/app");
        assert_eq!(req["arguments"]["env"][0], "RUST_LOG=debug");

        let cfg = config(
            r#"[{ "name": "At", "type": "lldb", "request": "attach", "processId": "4242" }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        assert_eq!(rc.process_id, Some(4242), "numeric string processId parses");
        let req = lldb_request(&rc);
        assert_eq!(req["command"], "attach");
        assert_eq!(req["arguments"]["pid"], 4242);
        assert!(req["arguments"].get("stopOnEntry").is_none());
    }

    /// #1419: an lldb attach by program name (with `waitFor`) or through
    /// `attachCommands` (gdbserver, QEMU, an embedded probe) has a target and
    /// goes out as written, with no `pid`.
    #[test]
    fn an_lldb_attach_by_program_or_attach_commands_has_a_target() {
        let cfg = config(
            r#"[{ "name": "By name", "type": "lldb", "request": "attach",
                  "program": "${workspaceFolder}/app", "waitFor": true }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        assert!(lldb_attach_has_target(&rc));
        let req = lldb_request(&rc);
        assert_eq!(req["command"], "attach");
        assert_eq!(req["arguments"]["program"], "/work/proj/app");
        assert_eq!(req["arguments"]["waitFor"], true);
        assert!(req["arguments"].get("pid").is_none());
        assert!(req["arguments"].get("stopOnEntry").is_none());

        let cfg = config(
            r#"[{ "name": "gdbserver", "type": "lldb", "request": "attach",
                  "program": "${workspaceFolder}/app",
                  "attachCommands": ["target create ${workspaceFolder}/app", "gdb-remote localhost:1234"] }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        assert!(lldb_attach_has_target(&rc));
        let req = lldb_request(&rc);
        assert_eq!(
            req["arguments"]["attachCommands"],
            json!(["target create /work/proj/app", "gdb-remote localhost:1234"])
        );
        assert!(req["arguments"].get("pid").is_none());

        // attachCommands alone is enough.
        let cfg = config(
            r#"[{ "name": "Remote", "type": "lldb", "request": "attach",
                  "attachCommands": ["gdb-remote 10.0.0.2:3333"] }]"#,
        );
        assert!(lldb_attach_has_target(&resolve(&cfg, &ctx()).unwrap()));
    }

    /// #1419, negative: an lldb attach with no pid, no program and no (or
    /// empty) attachCommands has nothing to attach to.
    #[test]
    fn an_lldb_attach_with_nothing_to_attach_to_has_no_target() {
        for text in [
            r#"[{ "name": "A", "type": "lldb", "request": "attach" }]"#,
            r#"[{ "name": "A", "type": "lldb", "request": "attach", "attachCommands": [] }]"#,
            r#"[{ "name": "A", "type": "lldb", "request": "attach", "program": "  " }]"#,
        ] {
            let rc = resolve(&config(text), &ctx()).unwrap();
            assert!(!lldb_attach_has_target(&rc), "{text}");
        }
    }

    #[test]
    fn js_requests_are_pwa_node_and_attach_by_port() {
        let cfg = config(
            r#"[{ "name": "S", "type": "node", "program": "/p/server.js", "sourceMaps": false }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        let req = js_request(&rc);
        assert_eq!(req["command"], "launch");
        assert_eq!(req["arguments"]["type"], "pwa-node");
        assert_eq!(
            req["arguments"]["sourceMaps"], false,
            "user value respected"
        );
        assert_eq!(req["arguments"]["console"], "internalConsole");

        let cfg = config(r#"[{ "name": "A", "type": "node", "request": "attach", "port": 9229 }]"#);
        let rc = resolve(&cfg, &ctx()).unwrap();
        let req = js_request(&rc);
        assert_eq!(req["command"], "attach");
        assert_eq!(req["arguments"]["port"], 9229);
        assert_eq!(req["arguments"]["address"], "127.0.0.1");
    }

    /// The issue's launch.json: a debugpy config for debugging tests.
    const DEBUG_TESTS: &str = r#"[
        { "name": "Run", "type": "debugpy", "request": "launch", "program": "${file}" },
        { "name": "Debug Tests", "type": "debugpy", "request": "launch",
          "purpose": ["debug-test"], "console": "integratedTerminal",
          "env": { "APP_MODE": "test" }, "justMyCode": false,
          "args": ["--maxfail=1"], "cwd": "${workspaceFolder}/tests",
          "subProcess": true }
    ]"#;

    /// #1398: a debugpy config whose `purpose` names `debug-test` is the
    /// one Debug Test uses; a plain one is not.
    #[test]
    fn the_debug_test_config_is_found_by_its_purpose() {
        let cfgs = parse_launch_json(DEBUG_TESTS, ".vscode/launch.json");
        assert_eq!(
            debug_test_config(&cfgs).map(|c| c.name.as_str()),
            Some("Debug Tests")
        );
        assert!(!is_debug_test_config(&cfgs[0]));
        let single = config(r#"[{ "name": "T", "type": "python", "purpose": "debug-test" }]"#);
        assert!(is_debug_test_config(&single), "a single string counts too");
    }

    /// #1398, negative: no `purpose`, another purpose, or a debug-test
    /// purpose on a non-Python config is not a test-debugging config.
    #[test]
    fn other_purposes_and_other_debuggers_are_not_debug_test_configs() {
        let cfgs = parse_launch_json(
            r#"[
                { "name": "A", "type": "debugpy", "purpose": ["debug-in-terminal"] },
                { "name": "B", "type": "node", "purpose": ["debug-test"], "program": "x.js" },
                { "name": "C", "type": "debugpy" }
            ]"#,
            ".vscode/launch.json",
        );
        assert!(debug_test_config(&cfgs).is_none());
    }

    /// #1398: the config's env, cwd, justMyCode and other keys land on the
    /// pytest request; its args follow the test selection; pytest stays
    /// the module and the console stays croft's.
    #[test]
    fn a_debug_test_config_overlays_the_pytest_request() {
        let cfgs = parse_launch_json(DEBUG_TESTS, ".vscode/launch.json");
        let rc = resolve(debug_test_config(&cfgs).unwrap(), &ctx()).unwrap();
        let base = crate::dap::session::pytest_debug_launch_request(
            Path::new("/venv/bin/python"),
            Path::new("/work/proj"),
            "test_mode",
        );
        let req = with_debug_test_config(base, &rc);
        let a = &req["arguments"];
        assert_eq!(a["env"]["APP_MODE"], "test");
        assert_eq!(a["cwd"], "/work/proj/tests");
        assert_eq!(a["justMyCode"], false);
        assert_eq!(a["subProcess"], true);
        assert_eq!(a["args"], json!(["-v", "-k", "test_mode", "--maxfail=1"]));
        assert_eq!(a["module"], "pytest");
        assert_eq!(a["console"], "internalConsole");
        assert_eq!(a["python"], json!(["/venv/bin/python"]));
        assert!(a.get("purpose").is_none(), "{a}");
    }

    /// #1398, negative: a config that sets nothing leaves croft's request
    /// as it was.
    #[test]
    fn an_empty_debug_test_config_changes_nothing() {
        let cfg = config(r#"[{ "name": "T", "type": "debugpy", "purpose": ["debug-test"] }]"#);
        let rc = resolve(&cfg, &ctx()).unwrap();
        let base = crate::dap::session::pytest_debug_launch_request(
            Path::new("/venv/bin/python"),
            Path::new("/work/proj"),
            "test_mode",
        );
        assert_eq!(with_debug_test_config(base.clone(), &rc), base);
    }

    /// The issue's cppdbg configuration (#1394).
    const CPPDBG: &str = r#"[{
        "name": "(gdb) Launch", "type": "cppdbg", "request": "launch",
        "program": "${workspaceFolder}/app", "args": ["one"],
        "stopAtEntry": true, "cwd": "${workspaceFolder}",
        "environment": [{ "name": "GREETING", "value": "hi" }, { "name": "N", "value": 3 }],
        "externalConsole": false, "MIMode": "gdb",
        "setupCommands": [{ "text": "-enable-pretty-printing" }]
    }]"#;

    /// #1394: cppdbg's `environment` list and `stopAtEntry` reach lldb-dap
    /// as its `env` and `stopOnEntry`.
    #[test]
    fn a_cppdbg_environment_and_stop_at_entry_reach_lldb_dap() {
        let rc = resolve(&config(CPPDBG), &ctx()).unwrap();
        let req = lldb_request(&rc);
        let a = &req["arguments"];
        assert_eq!(a["env"], json!(["GREETING=hi", "N=3"]));
        assert_eq!(a["stopOnEntry"], true);
        assert_eq!(a["args"], json!(["one"]));
        assert!(a.get("environment").is_none(), "{a}");
        assert!(a.get("stopAtEntry").is_none(), "{a}");
    }

    /// #1394: an explicit `env` wins over `environment` for the same key.
    #[test]
    fn a_cppdbg_env_wins_over_its_environment_list() {
        let cfg = config(
            r#"[{ "name": "C", "type": "cppdbg", "program": "/a",
                  "environment": [{ "name": "A", "value": "1" }, { "name": "B", "value": "x" }],
                  "env": { "A": "2" } }]"#,
        );
        let req = lldb_request(&resolve(&cfg, &ctx()).unwrap());
        assert_eq!(req["arguments"]["env"], json!(["A=2", "B=x"]));
    }

    /// #1394: the gdb-only keys are named, so the debug console can say
    /// lldb-dap ignores them; only for cppdbg.
    #[test]
    fn a_cppdbg_config_names_the_keys_lldb_dap_ignores() {
        assert_eq!(
            ignored_cppdbg_keys(&config(CPPDBG)),
            ["MIMode", "setupCommands", "externalConsole"]
        );
        // Carried on the resolved config, which is all a preLaunchTask
        // resume or a compound member launch has to go on.
        assert_eq!(
            resolve(&config(CPPDBG), &ctx()).unwrap().ignored_keys,
            ["MIMode", "setupCommands", "externalConsole"]
        );
    }

    /// #1394, negative: an lldb config keeps its own spellings: an
    /// `environment` or `stopAtEntry` key there is not lldb-dap's and is
    /// passed through, not mapped, and it names no ignored keys.
    #[test]
    fn only_cppdbg_configs_get_cppdbg_spellings() {
        let cfg = config(
            r#"[{ "name": "L", "type": "lldb", "program": "/a", "MIMode": "gdb",
                  "environment": [{ "name": "A", "value": "1" }], "stopAtEntry": true }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        assert!(rc.env.is_empty());
        assert!(!rc.stop_on_entry);
        assert!(ignored_cppdbg_keys(&cfg).is_empty());
        let rc = resolve(
            &config(r#"[{ "name": "C", "type": "cppdbg", "program": "/a" }]"#),
            &ctx(),
        )
        .unwrap();
        assert!(rc.env.is_empty());
        assert!(!rc.stop_on_entry);
    }

    #[test]
    fn parse_env_file_handles_comments_export_and_quotes() {
        let env = parse_env_file(
            "# a comment\n\nexport A=1\nB = spaced \nC='sq'\nnot a pair\nBAD KEY=x\n",
        );
        assert_eq!(env["A"], "1");
        assert_eq!(env["B"], "spaced");
        assert_eq!(env["C"], "sq");
        assert!(!env.contains_key("not a pair"));
        assert!(!env.contains_key("BAD KEY"));
    }

    /// #250 phase 2: `compounds` names several configurations to launch
    /// together. It is a sibling key of `configurations` in the same file, so
    /// it parses alongside them rather than needing its own discovery pass.
    /// Multi-root workspaces write members as `{ "folder": …, "name": … }`
    /// rather than bare strings. croft resolves against one root, so the name
    /// is taken either way — dropping the row would silently shrink a compound
    /// to the members that happen to be written in the short form.
    #[test]
    fn compound_members_may_be_folder_name_objects_as_well_as_bare_strings() {
        let text = r#"{"configurations":[
              {"name":"Server","type":"lldb","request":"launch"},
              {"name":"Client","type":"lldb","request":"launch"}],
            "compounds":[{"name":"Mixed","configurations":[
              "Server",
              {"folder":"web","name":"Client"}]}]}"#;

        let compounds = parse_compounds(text, ".vscode/launch.json");
        assert_eq!(compounds.len(), 1, "precondition: the compound parsed");
        assert_eq!(
            compounds[0].configurations,
            vec!["Server", "Client"],
            "an object member contributes its name, exactly as a bare string does"
        );
    }

    /// A compound must bind to the configurations ITS OWN FILE declares. Both
    /// files may declare the same name — `.croft` wins the picker row, but a
    /// `.vscode` compound naming "Server" means .vscode's Server, which may be
    /// a different program under a different adapter. Matching by bare name
    /// against the merged list launches something other than what the user
    /// wrote, which is the same rule the parse and resolve boundaries already
    /// enforce for missing and malformed members.
    #[test]
    fn a_compound_binds_to_its_own_files_configuration_not_a_same_named_one() {
        let croft = r#"{"configurations":[
              {"name":"Server","type":"python","request":"launch","program":"croft.py"}]}"#;
        let vscode = r#"{"configurations":[
              {"name":"Server","type":"node","request":"launch","program":"vscode.js"}],
            "compounds":[{"name":"VsCompound","configurations":["Server"]}]}"#;

        // Through the PRODUCTION path, not hand-stitched input. Building the
        // config list with two `parse_launch_json` calls asserts a property on
        // an input shape the shipped program never produces: `discover_configs`
        // keeps only the first occurrence per name, so the duplicate this
        // preference exists to disambiguate would already be gone.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
        std::fs::write(tmp.path().join(".croft/launch.json"), croft).unwrap();
        std::fs::write(tmp.path().join(".vscode/launch.json"), vscode).unwrap();

        let configs = discover_configs_all(tmp.path());
        let compounds = discover_compounds(tmp.path());
        assert_eq!(compounds.len(), 1, "precondition: the compound parsed");
        assert_eq!(
            compounds[0].source, ".vscode/launch.json",
            "precondition: the compound remembers which file declared it"
        );
        assert_eq!(
            configs.iter().filter(|c| c.name == "Server").count(),
            2,
            "precondition: the list resolve_compound receives still holds BOTH \
             Servers — `discover_configs` would have dropped one, leaving the \
             source preference nothing to prefer"
        );
        assert_eq!(
            discover_configs(tmp.path())
                .iter()
                .filter(|c| c.name == "Server")
                .count(),
            1,
            "and the picker's list drops it, which is why the two lists differ"
        );

        let members = resolve_compound(&compounds[0], &configs, &[]).expect("resolves");
        assert_eq!(
            members[0].source, ".vscode/launch.json",
            "the .vscode compound binds .vscode's Server, not .croft's same-named one"
        );
        assert_eq!(
            members[0].type_name, "node",
            "which is a different adapter — binding .croft's python config here \
             would debug a different program than the compound declares"
        );
    }

    /// A compound with SOME unusable members must be dropped whole, not
    /// silently shrunk to the ones that parse. `resolve_compound` already
    /// refuses to launch a subset when a member names no configuration —
    /// "launching the subset that happens to resolve would debug something
    /// other than what the user asked for, and silently" — and a malformed
    /// member is the same situation one boundary earlier.
    #[test]
    fn a_compound_with_one_unusable_member_is_dropped_rather_than_silently_shrunk() {
        let text = r#"{"configurations":[
              {"name":"A","type":"lldb","request":"launch"},
              {"name":"B","type":"lldb","request":"launch"}],
            "compounds":[
              {"name":"Partial","configurations":["A", 7, "B"]},
              {"name":"Whole","configurations":["A","B"]}]}"#;

        let names: Vec<String> = parse_compounds(text, ".vscode/launch.json")
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            names,
            vec!["Whole"],
            "the compound with a malformed member is dropped entirely — shrinking \
             it to [\"A\", \"B\"] would launch two of the three the user wrote"
        );
    }

    /// A compound whose members all fail to parse would present as a picker row
    /// that does nothing, so it is skipped entirely rather than listed empty.
    #[test]
    fn a_compound_with_no_usable_members_is_skipped_rather_than_listed_empty() {
        let text = r#"{"configurations":[{"name":"A","type":"lldb","request":"launch"}],
            "compounds":[
              {"name":"Empty","configurations":[]},
              {"name":"Unusable","configurations":[7, true]},
              {"name":"Real","configurations":["A"]}]}"#;

        let names: Vec<String> = parse_compounds(text, ".vscode/launch.json")
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            names,
            vec!["Real"],
            "an empty compound and one whose members are all unusable are both dropped"
        );
    }

    #[test]
    fn discover_compounds_prefers_croft_over_vscode_for_a_duplicate_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".croft")).unwrap();
        std::fs::create_dir_all(root.join(".vscode")).unwrap();
        std::fs::write(
            root.join(".croft/launch.json"),
            r#"{"configurations":[{"name":"A","type":"lldb","request":"launch"}],
                "compounds":[{"name":"Both","configurations":["A"]}]}"#,
        )
        .unwrap();
        std::fs::write(
            root.join(".vscode/launch.json"),
            r#"{"configurations":[{"name":"B","type":"lldb","request":"launch"}],
                "compounds":[{"name":"Both","configurations":["B"]},
                             {"name":"VsOnly","configurations":["B"]}]}"#,
        )
        .unwrap();

        let found = discover_compounds(root);
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Both", "VsOnly"],
            "both files contribute, .croft first"
        );
        assert_eq!(
            found[0].configurations,
            vec!["A"],
            "the .croft definition of a duplicate name wins, not .vscode's"
        );
    }

    #[test]
    fn compounds_are_parsed_alongside_the_configurations_they_name() {
        let text = r#"{
            "configurations": [
                { "name": "Server", "type": "node", "program": "s.js" },
                { "name": "Client", "type": "node", "program": "c.js" }
            ],
            "compounds": [
                { "name": "Full Stack", "configurations": ["Server", "Client"] }
            ]
        }"#;
        // Precondition: the configurations themselves parse, so a compound
        // finding nothing cannot be blamed on the file being unreadable.
        let cfgs = parse_launch_json(text, ".vscode/launch.json");
        assert_eq!(cfgs.len(), 2, "precondition: both configurations parsed");

        let compounds = parse_compounds(text, ".vscode/launch.json");
        assert_eq!(compounds.len(), 1, "the compound is found");
        assert_eq!(compounds[0].name, "Full Stack");
        assert_eq!(compounds[0].configurations, vec!["Server", "Client"]);
    }

    /// A compound naming a configuration that does not exist must fail loudly
    /// at resolve time rather than silently launching the subset it can find —
    /// the same posture as an unresolvable `${...}` variable (#250).
    #[test]
    fn a_compound_naming_a_missing_configuration_errors_rather_than_launching_a_subset() {
        let text = r#"{
            "configurations": [ { "name": "Server", "type": "node", "program": "s.js" } ],
            "compounds": [ { "name": "Both", "configurations": ["Server", "Ghost"] } ]
        }"#;
        let cfgs = parse_launch_json(text, ".vscode/launch.json");
        let compounds = parse_compounds(text, ".vscode/launch.json");
        assert_eq!(compounds.len(), 1, "precondition: the compound parsed");

        let err = resolve_compound(&compounds[0], &cfgs, &[])
            .expect_err("a missing member is an error, not a shorter list");
        assert!(
            err.contains("Ghost"),
            "the error names the missing configuration, got: {err}"
        );
    }

    /// A key PRESENT but requesting nothing must not be recorded as
    /// unsupported. `contains_key` was the first attempt and it refused these:
    /// `stopAll: false` is VS Code's own default written out, and a null or
    /// empty `preLaunchTask` asks for nothing at all. croft honours every one
    /// of them perfectly by doing nothing, so naming them in a refusal tells
    /// the user croft cannot do what it is already doing.
    ///
    /// `stopAll` is excluded at either value for ONE member: it decides whether
    /// ending one session ends the others, and a one-member compound has no
    /// others. At two it is recorded at `true` only - see
    /// `stop_all_true_is_refused_only_where_there_are_siblings_to_stop`.
    #[test]
    fn a_compound_key_that_requests_nothing_is_not_recorded_as_unsupported() {
        let text = r#"{
            "configurations": [
                { "name": "A", "type": "node", "program": "a.js" },
                { "name": "B", "type": "node", "program": "b.js" }
            ],
            "compounds": [
                { "name": "DefaultStopAll", "configurations": ["A"], "stopAll": false },
                { "name": "TrueStopAll", "configurations": ["A"], "stopAll": true },
                { "name": "NulledTask", "configurations": ["A"], "preLaunchTask": null },
                { "name": "EmptyTask", "configurations": ["A"], "preLaunchTask": "" },
                { "name": "BoolTask", "configurations": ["A"], "preLaunchTask": true },
                { "name": "EmptyPresentation", "configurations": ["A"], "presentation": {} },
                { "name": "RealPresentation", "configurations": ["A"], "presentation": { "order": 1 } },
                { "name": "Malformed", "configurations": ["A"], "presentation": 7 }
            ]
        }"#;
        let cs = parse_compounds(text, ".vscode/launch.json");
        assert_eq!(cs.len(), 8, "precondition: every compound parsed");
        let by = |n: &str| {
            cs.iter()
                .find(|c| c.name == n)
                .unwrap_or_else(|| panic!("{n} parsed"))
        };

        assert!(
            by("DefaultStopAll").unsupported_keys.is_empty(),
            "stopAll:false is VS Code's default and cannot change a one-member \
             outcome: {:?}",
            by("DefaultStopAll").unsupported_keys
        );
        // A task that asks for something croft cannot READ is refused, unlike
        // one that asks for nothing. Without this pair the rule reads as
        // "preLaunchTask is never recorded", which was the bug.
        assert_eq!(
            by("BoolTask").unsupported_keys,
            vec!["preLaunchTask"],
            "a non-string task cannot be named or run, so it is refused \
             rather than skipped in silence"
        );
        assert!(
            by("NulledTask").unsupported_keys.is_empty(),
            "while a null one asks for nothing: {:?}",
            by("NulledTask").unsupported_keys
        );
        assert!(
            by("TrueStopAll").unsupported_keys.is_empty(),
            "nor can stopAll:true — with one session there are no others to \
             stop: {:?}",
            by("TrueStopAll").unsupported_keys
        );
        assert!(
            by("NulledTask").unsupported_keys.is_empty(),
            "a null preLaunchTask asks for nothing: {:?}",
            by("NulledTask").unsupported_keys
        );
        assert!(
            by("EmptyTask").unsupported_keys.is_empty(),
            "nor does an empty one: {:?}",
            by("EmptyTask").unsupported_keys
        );
        assert!(
            by("EmptyPresentation").unsupported_keys.is_empty(),
            "nor an empty presentation object: {:?}",
            by("EmptyPresentation").unsupported_keys
        );

        // The positive control: without it, six empties could equally mean
        // the detection never records anything at all. Every honourable key
        // is honoured now (#318), so the only shapes left to carry the
        // control are the MALFORMED ones - a `presentation` that is not an
        // object is the cheapest.
        assert_eq!(
            by("Malformed").unsupported_keys,
            vec!["presentation"],
            "and a shape croft cannot act on is still recorded"
        );
        assert_eq!(
            by("RealPresentation").unsupported_keys,
            Vec::<&str>::new(),
            "while a well-formed order is honoured rather than refused"
        );
    }

    /// `stopAll: true` is honoured, not refused (#567); only an unreadable
    /// value is refused, and only where there are siblings to stop.
    #[test]
    fn stop_all_true_is_honoured_and_only_an_unreadable_value_refused() {
        let text = r#"{
            "configurations": [
                { "name": "A", "type": "node", "program": "a.js" },
                { "name": "B", "type": "node", "program": "b.js" }
            ],
            "compounds": [
                { "name": "OneTrue", "configurations": ["A"], "stopAll": true },
                { "name": "TwoTrue", "configurations": ["A", "B"], "stopAll": true },
                { "name": "TwoQuoted", "configurations": ["A", "B"], "stopAll": "true" },
                { "name": "TwoFalse", "configurations": ["A", "B"], "stopAll": false },
                { "name": "TwoAbsent", "configurations": ["A", "B"] }
            ]
        }"#;
        let cs = parse_compounds(text, ".vscode/launch.json");
        assert_eq!(cs.len(), 5, "precondition: every compound parsed");
        let by = |n: &str| cs.iter().find(|c| c.name == n).expect("parsed");

        assert!(
            by("TwoTrue").unsupported_keys.is_empty() && by("TwoTrue").stop_all,
            "`true` is honoured: the set is torn down when a member ends"
        );
        assert!(!by("TwoFalse").stop_all && !by("TwoAbsent").stop_all);
        assert_eq!(
            by("TwoQuoted").unsupported_keys,
            vec!["stopAll"],
            "and `\"true\"` is the same typo the presentation keys already \
             refuse - matching only the BOOL let it launch as `false`"
        );
        assert!(
            by("OneTrue").unsupported_keys.is_empty(),
            "but one member has no siblings to stop, so the same value is \
             honoured by doing nothing: {:?}",
            by("OneTrue").unsupported_keys
        );
        assert!(
            by("TwoFalse").unsupported_keys.is_empty(),
            "and `false` is VS Code's default, which croft already does: {:?}",
            by("TwoFalse").unsupported_keys
        );
        assert!(
            by("TwoAbsent").unsupported_keys.is_empty(),
            "as is omitting it: {:?}",
            by("TwoAbsent").unsupported_keys
        );
    }

    /// #318: a compound's own `preLaunchTask` is now read and run, so it is no
    /// longer a reason to refuse the launch. The VALUE has to survive parsing
    /// — recording only that the key was present was enough to refuse, and is
    /// not enough to honour it.
    #[test]
    fn a_compounds_pre_launch_task_is_captured_and_no_longer_unsupported() {
        let text = r#"{
            "configurations": [ { "name": "A", "type": "node", "program": "a.js" } ],
            "compounds": [
                { "name": "Builds", "configurations": ["A"], "preLaunchTask": "build" },
                { "name": "Nulled", "configurations": ["A"], "preLaunchTask": null },
                { "name": "Empty", "configurations": ["A"], "preLaunchTask": "" },
                { "name": "Presents", "configurations": ["A"], "presentation": { "order": 1 } }
            ]
        }"#;
        let cs = parse_compounds(text, ".vscode/launch.json");
        let by = |n: &str| {
            cs.iter()
                .find(|c| c.name == n)
                .unwrap_or_else(|| panic!("{n} parsed"))
        };

        assert_eq!(
            by("Builds").pre_launch_task.as_deref(),
            Some("build"),
            "the task's NAME is what running it needs; the key alone cannot"
        );
        assert!(
            by("Builds").unsupported_keys.is_empty(),
            "preLaunchTask is honoured now, so it must stop being a refusal              reason: {:?}",
            by("Builds").unsupported_keys
        );

        // Requesting nothing stays nothing: null and "" must not park a launch
        // behind a task that does not exist.
        assert_eq!(by("Nulled").pre_launch_task, None);
        assert_eq!(by("Empty").pre_launch_task, None);

        // `presentation.order` is HONOURED now (#318), so it must stop being
        // a refusal reason - and it must be read, not merely tolerated.
        assert!(
            by("Presents").unsupported_keys.is_empty(),
            "order is honoured now and must stop refusing: {:?}",
            by("Presents").unsupported_keys
        );
        assert_eq!(
            by("Presents").presentation.as_ref().and_then(|p| p.order),
            Some(1.0),
            "the order's VALUE is what sorting needs; tolerating the key cannot"
        );
        assert_eq!(by("Presents").pre_launch_task, None);
    }

    #[test]
    fn presentation_keys_are_honoured_and_only_unreadable_values_refuse() {
        let text = r#"{
          "compounds": [
            { "name": "Hid",   "configurations": ["A"], "presentation": { "hidden": true } },
            { "name": "Shown", "configurations": ["A"], "presentation": { "hidden": false } },
            { "name": "Grouped", "configurations": ["A"], "presentation": { "group": "g" } },
            { "name": "Ordered", "configurations": ["A"], "presentation": { "order": 2 } },
            { "name": "Both",  "configurations": ["A"],
              "presentation": { "hidden": true, "order": 1 } },
            { "name": "Empty", "configurations": ["A"], "presentation": {} }
          ]
        }"#;
        let cs = parse_compounds(text, "test");
        let by = |n: &str| cs.iter().find(|c| c.name == n).expect("compound present");

        assert!(by("Hid").hidden(), "hidden: true asks not to be listed");
        assert!(
            !by("Shown").hidden(),
            "hidden: false is the default, spelled out"
        );
        assert!(
            !by("Empty").hidden(),
            "an empty presentation asks for nothing"
        );

        // A compound asking ONLY to be hidden is fully served, so it must not
        // be refused for a capability it never requested.
        assert!(
            by("Hid").unsupported_keys.is_empty(),
            "hidden alone is honoured, so nothing is unsupported: {:?}",
            by("Hid").unsupported_keys
        );
        assert!(
            by("Empty").unsupported_keys.is_empty(),
            "an empty presentation refuses nothing"
        );

        // `group` and `order` are honoured now (#318): read, not refused.
        assert!(
            by("Grouped").unsupported_keys.is_empty(),
            "group no longer refuses: {:?}",
            by("Grouped").unsupported_keys
        );
        assert_eq!(
            by("Grouped")
                .presentation
                .as_ref()
                .and_then(|p| p.group.clone()),
            Some(String::from("g")),
            "and its VALUE is what places the row"
        );
        assert!(by("Ordered").unsupported_keys.is_empty());
        assert_eq!(
            by("Ordered").presentation.as_ref().and_then(|p| p.order),
            Some(2.0)
        );
        // A malformed shape must keep refusing. On main these refused and the
        // user found out; silently launching a compound whose typo'd
        // `{"hidden": "true"}` did nothing would be a regression dressed as
        // a feature.
        let bad = parse_compounds(
            r#"{
              "compounds": [
                { "name": "QuotedBool", "configurations": ["A"],
                  "presentation": { "hidden": "true" } },
                { "name": "NotAnObject", "configurations": ["A"],
                  "presentation": "grouped" }
              ]
            }"#,
            "test",
        );
        let bad_by = |n: &str| bad.iter().find(|c| c.name == n).expect("present");
        assert!(
            !bad_by("QuotedBool").hidden(),
            "a quoted bool is not a bool"
        );
        assert_eq!(
            bad_by("QuotedBool").unsupported_keys,
            vec!["presentation.hidden"],
            "and it refuses rather than launching unhidden in silence"
        );
        assert_eq!(
            bad_by("NotAnObject").unsupported_keys,
            vec!["presentation"],
            "a non-object presentation refuses under its own name"
        );

        assert!(by("Both").hidden(), "Both asks to be hidden");
        assert!(
            by("Both").unsupported_keys.is_empty(),
            "and both halves are honoured now, so nothing refuses: {:?}",
            by("Both").unsupported_keys
        );
    }

    /// A value croft cannot READ is a typo, whichever key carries it (#318).
    /// `{"order": "1"}` is the same mistake as `{"hidden": "true"}`, and
    /// dropping it silently would leave a picker that ignores the ordering the
    /// user wrote with nothing to explain why.
    #[test]
    fn a_malformed_presentation_value_refuses_whichever_key_it_is_on() {
        let cs = parse_compounds(
            r#"{
              "compounds": [
                { "name": "QuotedOrder", "configurations": ["A"],
                  "presentation": { "order": "1" } },
                { "name": "NumberGroup", "configurations": ["A"],
                  "presentation": { "group": 7 } },
                { "name": "FractionalOrder", "configurations": ["A"],
                  "presentation": { "order": 1.5 } },
                { "name": "NulledKeys", "configurations": ["A"],
                  "presentation": { "group": null, "order": null } }
              ]
            }"#,
            ".vscode/launch.json",
        );
        assert_eq!(cs.len(), 4, "precondition: every compound parsed");
        let by = |n: &str| {
            cs.iter()
                .find(|c| c.name == n)
                .unwrap_or_else(|| panic!("{n} parsed"))
        };
        assert_eq!(
            by("QuotedOrder").unsupported_keys,
            vec!["presentation.order"]
        );
        assert_eq!(
            by("NumberGroup").unsupported_keys,
            vec!["presentation.group"]
        );
        // A FRACTION is not a typo: VS Code's schema says `number` and its
        // comparator subtracts, so 1.5 is a legal position between two
        // neighbours and must be honoured rather than refused.
        assert!(
            by("FractionalOrder").unsupported_keys.is_empty(),
            "a fractional order is a position, not a mistake: {:?}",
            by("FractionalOrder").unsupported_keys
        );
        assert_eq!(
            by("FractionalOrder")
                .presentation
                .as_ref()
                .and_then(|p| p.order),
            Some(1.5)
        );
        // Explicit nulls ask for nothing, which croft delivers by doing
        // nothing - naming them would report a limitation that is not one.
        assert!(
            by("NulledKeys").unsupported_keys.is_empty(),
            "null asks for nothing: {:?}",
            by("NulledKeys").unsupported_keys
        );
    }

    /// #318: the picker's order is VS Code's `getVisibleAndSorted`, which is a
    /// transcription and not a guess. Each arm below is a rule croft would
    /// plausibly have got wrong on its own.
    #[test]
    fn presentation_sorts_the_picker_the_way_vs_code_does() {
        fn p(group: Option<&str>, order: Option<f64>) -> Option<Presentation> {
            Some(Presentation {
                hidden: false,
                group: group.map(str::to_string),
                order,
            })
        }
        let items: Vec<(&str, Option<Presentation>)> = vec![
            ("plain-first", None),
            ("b-group", p(Some("b"), None)),
            ("no-group-ord-2", p(None, Some(2.0))),
            (
                "hidden",
                Some(Presentation {
                    hidden: true,
                    group: None,
                    order: None,
                }),
            ),
            ("a-group-ord-9", p(Some("a"), Some(9.0))),
            ("plain-second", None),
            ("a-group-ord-1", p(Some("a"), Some(1.0))),
            ("a-group-no-ord", p(Some("a"), None)),
            ("no-group-ord-1", p(None, Some(1.0))),
        ];
        let sorted: Vec<&str> = visible_and_sorted(items, |(_, p)| p.as_ref())
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            sorted,
            vec![
                // grouped first, groups by name, order ascending within one,
                // and a missing order last inside its group
                "a-group-ord-1",
                "a-group-ord-9",
                "a-group-no-ord",
                "b-group",
                // a presentation WITHOUT a group still beats no presentation
                "no-group-ord-1",
                "no-group-ord-2",
                // and entries that asked for nothing keep their file order
                "plain-first",
                "plain-second",
            ],
            "hidden is dropped, and every other rule is VS Code's"
        );
    }

    /// An empty `presentation` is still a `presentation`, and VS Code places
    /// it accordingly: its comparator tests the OBJECT for truthiness, so
    /// `{}` sorts ahead of an entry carrying no block at all. Reading it as
    /// "no preference" would put those rows somewhere VS Code does not.
    #[test]
    fn an_empty_presentation_block_still_outranks_no_block() {
        let cfgs = parse_launch_json(
            r#"{ "configurations": [
                { "name": "Bare", "type": "node", "program": "a.js" },
                { "name": "EmptyBlock", "type": "node", "program": "b.js",
                  "presentation": {} },
                { "name": "OnlyShown", "type": "node", "program": "c.js",
                  "presentation": { "hidden": false } }
            ] }"#,
            ".vscode/launch.json",
        );
        let by = |n: &str| {
            cfgs.iter()
                .find(|c| c.name == n)
                .unwrap_or_else(|| panic!("{n} parsed"))
                .clone()
        };
        assert_eq!(by("Bare").presentation, None, "no block is no block");
        assert_eq!(
            by("EmptyBlock").presentation,
            Some(Presentation::default()),
            "an empty block asks for nothing but is still declared"
        );
        assert_eq!(by("OnlyShown").presentation, Some(Presentation::default()));

        let sorted: Vec<String> = visible_and_sorted(cfgs, |c| c.presentation.as_ref())
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            sorted,
            vec!["EmptyBlock", "OnlyShown", "Bare"],
            "both declared blocks sort ahead of the bare entry, in file order"
        );
    }

    /// A key that REQUESTS NOTHING is honoured by doing nothing, so it must
    /// not be named in a refusal - that would report a limitation croft does
    /// not have. `false` and `""` are that shape, and both used to launch
    /// before `presentation` gained its own reader.
    #[test]
    fn a_presentation_that_requests_nothing_is_not_a_refusal() {
        let cs = parse_compounds(
            r#"{
              "compounds": [
                { "name": "FalseBlock", "configurations": ["A"], "presentation": false },
                { "name": "EmptyString", "configurations": ["A"], "presentation": "" },
                { "name": "NulledBlock", "configurations": ["A"], "presentation": null },
                { "name": "Truthy", "configurations": ["A"], "presentation": true }
              ]
            }"#,
            ".vscode/launch.json",
        );
        assert_eq!(cs.len(), 4, "precondition: every compound parsed");
        let by = |n: &str| {
            cs.iter()
                .find(|c| c.name == n)
                .unwrap_or_else(|| panic!("{n} parsed"))
        };
        for name in ["FalseBlock", "EmptyString", "NulledBlock"] {
            assert!(
                by(name).unsupported_keys.is_empty(),
                "{name} asks for nothing and must launch: {:?}",
                by(name).unsupported_keys
            );
        }
        // The control: a value that DOES claim to say something, and is still
        // unreadable, must refuse - or the three above prove only that this
        // function never records anything.
        assert_eq!(
            by("Truthy").unsupported_keys,
            vec!["presentation"],
            "a non-object that asks for something is unreadable"
        );
    }

    /// The property that must not break: a launch.json that says nothing
    /// about presentation is left exactly as written.
    #[test]
    fn a_workspace_with_no_presentation_keeps_its_file_order() {
        let items: Vec<(&str, Option<Presentation>)> = vec![("z", None), ("a", None), ("m", None)];
        let sorted: Vec<&str> = visible_and_sorted(items, |(_, p)| p.as_ref())
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            sorted,
            vec!["z", "a", "m"],
            "sorting must be stable, or introducing the feature would silently \
             reorder every existing workspace's picker"
        );
    }

    /// A configuration carries `presentation` too, and sorts against
    /// compounds in one list - VS Code puts both kinds through the same
    /// comparator, so a config and a compound naming one group belong
    /// together.
    #[test]
    fn a_configuration_reads_presentation_like_a_compound_does() {
        let cfgs = parse_launch_json(
            r#"{ "configurations": [
                { "name": "Plain", "type": "node", "program": "a.js" },
                { "name": "Grouped", "type": "node", "program": "b.js",
                  "presentation": { "group": "servers", "order": 3 } },
                { "name": "Hid", "type": "node", "program": "c.js",
                  "presentation": { "hidden": true } }
            ] }"#,
            ".vscode/launch.json",
        );
        let by = |n: &str| {
            cfgs.iter()
                .find(|c| c.name == n)
                .unwrap_or_else(|| panic!("{n} parsed"))
                .clone()
        };
        assert_eq!(by("Plain").presentation, None, "no block asks for nothing");
        assert_eq!(
            by("Grouped").presentation,
            Some(Presentation {
                hidden: false,
                group: Some(String::from("servers")),
                order: Some(3.0),
            })
        );
        assert!(by("Hid").presentation.is_some_and(|p| p.hidden));
    }

    /// #1425: a multi-root workspace, `api` and `web`, each with its own
    /// launch.json.
    fn multi_root() -> (tempfile::TempDir, Vec<(String, PathBuf)>) {
        let tmp = tempfile::tempdir().unwrap();
        for (folder, text) in [
            (
                "api",
                r#"{ "configurations": [
                    { "name": "Run api", "type": "debugpy", "request": "launch", "program": "${workspaceFolder}/main.py" }
                ],
                "compounds": [
                    { "name": "API + web", "configurations": ["Run api", { "folder": "web", "name": "Run web" }] },
                    { "name": "Nowhere", "configurations": [{ "folder": "docs", "name": "Run web" }] },
                    { "name": "Missing", "configurations": [{ "folder": "web", "name": "Run api" }] }
                ] }"#,
            ),
            (
                "web",
                r#"{ "configurations": [
                    { "name": "Run web", "type": "debugpy", "request": "launch", "program": "${workspaceFolder}/main.py" }
                ] }"#,
            ),
        ] {
            std::fs::create_dir_all(tmp.path().join(folder).join(".vscode")).unwrap();
            std::fs::write(tmp.path().join(folder).join(".vscode/launch.json"), text).unwrap();
        }
        let roots = [tmp.path().join("api"), tmp.path().join("web")];
        let folders = workspace_folders(roots.iter().map(PathBuf::as_path));
        (tmp, folders)
    }

    /// #1425: a member written `{ "folder": "web", "name": "Run web" }`
    /// resolves to web's own configuration, with `${workspaceFolder}` the
    /// web root; a bare member still resolves beside the compound.
    #[test]
    fn a_folder_member_resolves_in_that_workspace_folder() {
        let (tmp, folders) = multi_root();
        let api = tmp.path().join("api");
        let compounds = discover_compounds(&api);
        assert_eq!(compounds[0].folders, vec![None, Some(String::from("web"))]);
        let members =
            resolve_compound(&compounds[0], &discover_configs_all(&api), &folders).unwrap();
        assert_eq!(
            members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["Run api", "Run web"]
        );
        assert_eq!(members[0].root, None);
        let web = tmp.path().join("web");
        assert_eq!(members[1].root.as_deref(), Some(web.as_path()));
        let ctx = SubstCtx {
            workspace_folder: members[1].root.clone().unwrap(),
            file: None,
            folders: folders.clone(),
        };
        let rc = resolve(&members[1], &ctx).unwrap();
        assert_eq!(
            rc.program.as_deref(),
            Some(web.join("main.py").to_string_lossy().as_ref())
        );
        assert_eq!(
            rc.cwd.as_deref(),
            Some(web.as_path()),
            "a foreign member with no cwd runs in its own folder"
        );
    }

    /// #1425, negative: a folder the workspace does not have, or a name
    /// that folder's launch.json does not declare, is refused by name
    /// rather than resolved against the compound's own file.
    #[test]
    fn a_folder_member_with_no_such_folder_or_config_is_refused() {
        let (tmp, folders) = multi_root();
        let api = tmp.path().join("api");
        let compounds = discover_compounds(&api);
        let all = discover_configs_all(&api);
        let err = resolve_compound(&compounds[1], &all, &folders).unwrap_err();
        assert_eq!(
            err,
            "compound \"Nowhere\" names folder \"docs\", which is not a folder of this workspace"
        );
        let err = resolve_compound(&compounds[2], &all, &folders).unwrap_err();
        assert_eq!(
            err,
            "compound \"Missing\" names configuration \"Run api\" in folder \"web\", which no launch.json there declares"
        );
        // A non-string folder is a malformed member: the compound is dropped.
        let bad = parse_compounds(
            r#"{ "compounds": [{ "name": "Bad", "configurations": [{ "folder": 3, "name": "x" }] }] }"#,
            ".vscode/launch.json",
        );
        assert!(bad.is_empty());
    }

    /// #1425: `${workspaceFolder:web}` is the web root; an unknown folder
    /// name is an error naming it.
    #[test]
    fn workspace_folder_by_name_expands_or_names_the_missing_folder() {
        let (tmp, folders) = multi_root();
        let ctx = SubstCtx {
            workspace_folder: tmp.path().join("api"),
            file: None,
            folders,
        };
        assert_eq!(
            substitute("${workspaceFolder:web}/main.py", &ctx).unwrap(),
            tmp.path().join("web/main.py").to_string_lossy()
        );
        assert_eq!(
            substitute("${workspaceFolder}", &ctx).unwrap(),
            tmp.path().join("api").to_string_lossy()
        );
        assert_eq!(
            substitute("${workspaceFolder:docs}/x", &ctx).unwrap_err(),
            "${workspaceFolder:docs}: no workspace folder is named \"docs\""
        );
    }

    /// A compound resolves to its members in the order written, so the picker
    /// and the launch sequence agree with the file.
    #[test]
    fn a_resolved_compound_yields_its_members_in_declaration_order() {
        let text = r#"{
            "configurations": [
                { "name": "Client", "type": "node", "program": "c.js" },
                { "name": "Server", "type": "node", "program": "s.js" }
            ],
            "compounds": [ { "name": "Both", "configurations": ["Server", "Client"] } ]
        }"#;
        let cfgs = parse_launch_json(text, ".vscode/launch.json");
        let compounds = parse_compounds(text, ".vscode/launch.json");
        let members = resolve_compound(&compounds[0], &cfgs, &[]).unwrap();
        assert_eq!(
            members.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["Server", "Client"],
            "compound order wins over the order in `configurations`"
        );
    }

    /// #250: the flow asks what each request needs, in order: a launch its
    /// program and the optional rest, an attach only where to attach.
    #[test]
    fn a_draft_asks_for_what_its_request_needs() {
        use DraftField::*;
        let fields = |d: &ConfigDraft| {
            let mut out = Vec::new();
            let mut at = d.next_field(None);
            while let Some(f) = at {
                out.push(f);
                at = d.next_field(Some(f));
            }
            out
        };
        let launch = ConfigDraft::new("python", RequestKind::Launch);
        assert_eq!(
            fields(&launch),
            [Name, Program, Args, Cwd, Env, PreLaunchTask]
        );
        for t in ["python", "node"] {
            assert_eq!(
                fields(&ConfigDraft::new(t, RequestKind::Attach)),
                [Name, Port]
            );
        }
        for t in ["lldb", "go"] {
            assert_eq!(
                fields(&ConfigDraft::new(t, RequestKind::Attach)),
                [Name, ProcessId]
            );
        }
        // Every offered type resolves to an adapter.
        for (t, _) in DRAFT_TYPES {
            assert!(adapter_for_type(t).is_some(), "{t}");
        }
    }

    /// #250: typed values are checked as they are entered, and what is
    /// written reads back as the configuration that was described.
    #[test]
    fn a_launch_draft_writes_an_entry_that_reads_back() {
        let mut d = ConfigDraft::new("python", RequestKind::Launch);
        assert_eq!(d.prompt(DraftField::Name).2, "Launch Python");
        assert_eq!(d.prompt(DraftField::Program).2, "${file}");
        d.set(DraftField::Name, "Serve").unwrap();
        d.set(DraftField::Program, "${workspaceFolder}/api.py")
            .unwrap();
        d.set(DraftField::Args, r#"--port 8000 "a b" 'c d'"#)
            .unwrap();
        assert_eq!(d.args, ["--port", "8000", "a b", "c d"]);
        assert!(d.set(DraftField::Args, r#"--x "open"#).is_err());
        d.set(DraftField::Cwd, "").unwrap();
        d.set(DraftField::Env, "DEBUG=1; URL=http://x?a=b").unwrap();
        assert!(d.set(DraftField::Env, "NOEQUALS").is_err());
        d.set(DraftField::PreLaunchTask, "build").unwrap();
        let text = serde_json::to_string(&json!({ "configurations": [d.to_json()] })).unwrap();
        let cfgs = parse_launch_json(&text, ".croft/launch.json");
        assert_eq!(cfgs.len(), 1);
        assert_eq!(cfgs[0].name, "Serve");
        assert_eq!(cfgs[0].request, RequestKind::Launch);
        let raw = d.to_json();
        assert_eq!(raw["args"], json!(["--port", "8000", "a b", "c d"]));
        assert_eq!(raw["env"], json!({ "DEBUG": "1", "URL": "http://x?a=b" }));
        assert_eq!(raw["preLaunchTask"], json!("build"));
        assert!(raw.get("cwd").is_none(), "a blank field is left out");
    }

    /// #250: each adapter is attached to the way it reads the request.
    #[test]
    fn an_attach_draft_uses_each_adapters_own_fields() {
        let attach = |t: &str, field: DraftField, value: &str| {
            let mut d = ConfigDraft::new(t, RequestKind::Attach);
            d.set(DraftField::Name, "A").unwrap();
            d.set(field, value).unwrap();
            d.to_json()
        };
        assert_eq!(
            attach("python", DraftField::Port, "5678")["port"],
            json!(5678)
        );
        assert_eq!(
            attach("node", DraftField::Port, "9229")["port"],
            json!(9229)
        );
        assert_eq!(
            attach("lldb", DraftField::ProcessId, "42")["processId"],
            json!(42)
        );
        let go = attach("go", DraftField::ProcessId, "7");
        assert_eq!(
            (go["processId"].clone(), go["mode"].clone()),
            (json!(7), json!("local"))
        );
        // And each resolves to the attach request its adapter is sent.
        let resolved = |t: &str, field: DraftField, value: &str| {
            let text = json!({ "configurations": [attach(t, field, value)] }).to_string();
            resolve(&config(&text), &ctx()).unwrap()
        };
        let py = debugpy_request(
            &resolved("python", DraftField::Port, "5678"),
            Path::new("/usr/bin/python3"),
        );
        assert_eq!(py["arguments"]["connect"]["port"], 5678);
        let lldb = lldb_request(&resolved("lldb", DraftField::ProcessId, "42"));
        assert_eq!(lldb["arguments"]["pid"], 42);
        let dlv = delve_request(&resolved("go", DraftField::ProcessId, "7"));
        assert_eq!(dlv["arguments"]["processId"], 7);
        let mut d = ConfigDraft::new("node", RequestKind::Attach);
        assert!(d.set(DraftField::Port, "0").is_err());
        assert!(d.set(DraftField::Port, "70000").is_err());
        assert!(d.set(DraftField::ProcessId, "pid").is_err());
    }

    /// #250: the entry goes into `.croft/launch.json`, created when absent
    /// and added to when present; a duplicate name and a file whose
    /// comments rewriting would lose are refused, the file untouched.
    #[test]
    fn a_draft_is_added_to_croft_launch_json() {
        let tmp = tempfile::tempdir().unwrap();
        let mut d = ConfigDraft::new("node", RequestKind::Launch);
        d.set(DraftField::Name, "Web").unwrap();
        d.set(DraftField::Program, "${workspaceFolder}/index.js")
            .unwrap();
        let path = add_to_croft_launch_json(tmp.path(), &d).unwrap();
        assert_eq!(path, tmp.path().join(".croft/launch.json"));
        let mut d2 = d.clone();
        d2.name = String::from("Web 2");
        add_to_croft_launch_json(tmp.path(), &d2).unwrap();
        let names: Vec<String> = discover_configs(tmp.path())
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["Web", "Web 2"]);
        let err = add_to_croft_launch_json(tmp.path(), &d).unwrap_err();
        assert!(err.contains("already has"), "{err}");
        // A bare list is added to as a list.
        let bare = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(bare.path().join(".croft")).unwrap();
        std::fs::write(bare.path().join(".croft/launch.json"), "[]").unwrap();
        add_to_croft_launch_json(bare.path(), &d).unwrap();
        assert_eq!(discover_configs(bare.path()).len(), 1);
        // Comments would be lost: refused, and the file is as it was.
        let jsonc = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(jsonc.path().join(".croft")).unwrap();
        let text = "{ // mine\n \"configurations\": [] }";
        std::fs::write(jsonc.path().join(".croft/launch.json"), text).unwrap();
        let err = add_to_croft_launch_json(jsonc.path(), &d).unwrap_err();
        assert!(err.contains("by hand") && err.contains("\"Web\""), "{err}");
        // Written aside and renamed: nothing is left beside the file.
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path().join(".croft"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, [std::ffi::OsString::from("launch.json")]);
        assert_eq!(
            std::fs::read_to_string(jsonc.path().join(".croft/launch.json")).unwrap(),
            text
        );
    }

    /// #250: `.croft` is workspace content, so a symlink there or at the
    /// file is refused rather than followed, and its target is untouched.
    #[cfg(unix)]
    #[test]
    fn adding_a_configuration_refuses_a_symlinked_croft_launch_json() {
        let mut d = ConfigDraft::new("node", RequestKind::Launch);
        d.set(DraftField::Name, "Web").unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let target = elsewhere.path().join("victim.json");
        std::fs::write(&target, "[]").unwrap();
        let file_link = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(file_link.path().join(".croft")).unwrap();
        std::os::unix::fs::symlink(&target, file_link.path().join(".croft/launch.json")).unwrap();
        let err = add_to_croft_launch_json(file_link.path(), &d).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        let dir_link = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir_link.path().join(".croft")).unwrap();
        let err = add_to_croft_launch_json(dir_link.path(), &d).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "[]");
        assert!(!elsewhere.path().join("launch.json").exists());

        // A link planted at the temp file's name is not written through: the
        // real file keeps what it had until the new one replaces it whole.
        let planted = tempfile::tempdir().unwrap();
        let dir = planted.path().join(".croft");
        std::fs::create_dir_all(&dir).unwrap();
        let mut first = ConfigDraft::new("node", RequestKind::Launch);
        first.set(DraftField::Name, "First").unwrap();
        add_to_croft_launch_json(planted.path(), &first).unwrap();
        let before = std::fs::read_to_string(dir.join("launch.json")).unwrap();
        let bait = elsewhere.path().join("bait.json");
        std::fs::write(&bait, "bait").unwrap();
        std::os::unix::fs::symlink(&bait, temp_candidate(&dir, 0)).unwrap();
        add_to_croft_launch_json(planted.path(), &d).unwrap();
        assert_eq!(
            std::fs::read_to_string(&bait).unwrap(),
            "bait",
            "the link is not followed"
        );
        let names: Vec<String> = discover_configs(planted.path())
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["First", "Web"], "and the file is whole: {before}");
    }

    fn attach_address(cfg: &str) -> Option<(String, u16)> {
        let rc = resolve(&config(cfg), &ctx()).unwrap();
        debugpy_connect_address(&debugpy_request(&rc, Path::new("python3")))
    }

    /// #1417: a debugpy attach with `connect` (or the older `host`/`port`)
    /// goes to that address, the `debugpy --listen` process's own adapter.
    #[test]
    fn a_debugpy_connect_attach_goes_to_the_listening_port() {
        assert_eq!(
            attach_address(
                r#"[{ "name": "R", "type": "debugpy", "request": "attach",
                     "connect": { "host": "127.0.0.1", "port": 5679 } }]"#
            ),
            Some((String::from("127.0.0.1"), 5679))
        );
        assert_eq!(
            attach_address(
                r#"[{ "name": "R", "type": "debugpy", "request": "attach",
                     "connect": { "host": "box", "port": "5680" } }]"#
            ),
            Some((String::from("box"), 5680))
        );
        assert_eq!(
            attach_address(
                r#"[{ "name": "R", "type": "python", "request": "attach",
                     "host": "10.0.0.5", "port": 5678 }]"#
            ),
            Some((String::from("10.0.0.5"), 5678))
        );
        assert_eq!(
            attach_address(
                r#"[{ "name": "R", "type": "debugpy", "request": "attach", "connect": { "port": 5678 } }]"#
            ),
            Some((String::from("127.0.0.1"), 5678)),
            "no host is this machine"
        );
    }

    /// #1417 negatives: the `listen` form (the debuggee connects back), a
    /// launch, an attach by process id and a port that is not one keep the
    /// local adapter.
    #[test]
    fn a_debugpy_listen_attach_or_launch_keeps_the_local_adapter() {
        for cfg in [
            r#"[{ "name": "L", "type": "debugpy", "request": "attach", "listen": { "host": "0.0.0.0", "port": 5678 } }]"#,
            r#"[{ "name": "P", "type": "debugpy", "request": "launch", "program": "a.py", "connect": { "port": 5678 } }]"#,
            r#"[{ "name": "I", "type": "debugpy", "request": "attach", "processId": 4242 }]"#,
            r#"[{ "name": "B", "type": "debugpy", "request": "attach", "connect": { "port": 70000 } }]"#,
        ] {
            assert_eq!(attach_address(cfg), None, "{cfg}");
        }
    }

    /// #1391: a node launch that starts an npm script names no `program`;
    /// js-debug runs its `runtimeExecutable` and `runtimeArgs` instead.
    #[test]
    fn a_node_launch_can_start_an_npm_script() {
        let cfg = config(
            r#"[{ "name": "npm start", "type": "node", "request": "launch",
                 "runtimeExecutable": "npm", "runtimeArgs": ["run", "start"],
                 "cwd": "${workspaceFolder}" }]"#,
        );
        let rc = resolve(&cfg, &ctx()).unwrap();
        assert!(js_launch_has_target(&rc));
        let req = js_request(&rc);
        assert_eq!(req["arguments"]["type"], "pwa-node");
        assert_eq!(req["arguments"]["runtimeExecutable"], "npm");
        assert_eq!(req["arguments"]["runtimeArgs"], json!(["run", "start"]));
        assert!(req["arguments"].get("program").is_none());
    }

    /// #1391: a `node-terminal` launch's `command` is the runtime js-debug
    /// starts, split into words, or run by `sh -c` when it uses shell syntax.
    #[test]
    fn a_node_terminal_command_becomes_the_runtime() {
        let rc = |command: &str| {
            let text = format!(
                r#"[{{ "name": "dev", "type": "node-terminal", "request": "launch", "command": {} }}]"#,
                json!(command)
            );
            resolve(&config(&text), &ctx()).unwrap()
        };
        let dev = rc("npm run dev -- --port 3001");
        assert!(js_launch_has_target(&dev));
        let req = js_request(&dev);
        assert_eq!(req["arguments"]["type"], "pwa-node");
        assert_eq!(req["arguments"]["runtimeExecutable"], "npm");
        assert_eq!(
            req["arguments"]["runtimeArgs"],
            json!(["run", "dev", "--", "--port", "3001"])
        );
        assert!(req["arguments"].get("command").is_none());
        let quoted = js_request(&rc("node 'my app.js'"));
        assert_eq!(quoted["arguments"]["runtimeArgs"], json!(["my app.js"]));
        let chained = js_request(&rc("npm run build && npm start"));
        assert_eq!(chained["arguments"]["runtimeExecutable"], "sh");
        assert_eq!(
            chained["arguments"]["runtimeArgs"],
            json!(["-c", "npm run build && npm start"])
        );
        let env = js_request(&rc("NODE_ENV=production npm start"));
        assert_eq!(env["arguments"]["runtimeExecutable"], "sh");
        assert_eq!(
            env["arguments"]["runtimeArgs"],
            json!(["-c", "NODE_ENV=production npm start"])
        );
        let flag = js_request(&rc("node --max-old-space-size=4096 app.js"));
        assert_eq!(flag["arguments"]["runtimeExecutable"], "node");
    }

    /// #1391 negatives: a node launch with nothing to run still has no
    /// target; `command` means nothing on a plain `node` config; an
    /// explicit `runtimeExecutable` on `node-terminal` wins over `command`;
    /// an empty command adds nothing.
    #[test]
    fn a_node_launch_with_nothing_to_run_has_no_target() {
        let one = |text: &str| resolve(&config(text), &ctx()).unwrap();
        let bare = one(r#"[{ "name": "n", "type": "node", "request": "launch" }]"#);
        assert!(!js_launch_has_target(&bare));
        let node = one(r#"[{ "name": "n", "type": "node", "command": "npm start" }]"#);
        assert!(
            !js_launch_has_target(&node),
            "only node-terminal reads command"
        );
        assert_eq!(node.extra["command"], "npm start");
        let explicit = one(
            r#"[{ "name": "t", "type": "node-terminal", "command": "npm start",
                 "runtimeExecutable": "yarn", "runtimeArgs": ["dev"] }]"#,
        );
        assert_eq!(explicit.extra["runtimeExecutable"], "yarn");
        assert_eq!(explicit.extra["runtimeArgs"], json!(["dev"]));
        assert!(!explicit.extra.contains_key("command"));
        let empty = one(r#"[{ "name": "t", "type": "node-terminal", "command": "  " }]"#);
        assert!(!js_launch_has_target(&empty));
        let hollow = one(r#"[{ "name": "n", "type": "node", "request": "launch",
                 "runtimeExecutable": null, "runtimeArgs": [] }]"#);
        assert!(!js_launch_has_target(&hollow));
        let blank = one(r#"[{ "name": "n", "type": "node", "runtimeExecutable": "" }]"#);
        assert!(!js_launch_has_target(&blank));
    }

    /// What `cargo build`/`cargo test --no-run --message-format=json` print
    /// for a crate with a lib and a bin named `rc` (shapes from cargo 1.97).
    const CARGO_JSON_1392: &str = concat!(
        r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"rc","src_path":"/w/src/lib.rs"},"profile":{"test":false},"executable":null}"#,
        "\n",
        r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"rc","src_path":"/w/src/main.rs"},"profile":{"test":false},"executable":"/w/target/debug/rc"}"#,
        "\n",
        r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"rc","src_path":"/w/src/lib.rs"},"profile":{"test":true},"executable":"/w/target/debug/deps/rc-1a2b"}"#,
        "\n",
        r#"{"reason":"build-finished","success":true}"#,
        "\n"
    );

    fn cargo_cfg(cargo: &str) -> ResolvedConfig {
        let text = format!(
            r#"[{{ "name": "Debug executable 'rc'", "type": "lldb", "request": "launch", "cargo": {cargo}, "args": [] }}]"#
        );
        resolve(&config(&text), &ctx()).unwrap()
    }

    /// #1392: a CodeLLDB `cargo` launch builds with cargo's JSON messages
    /// and debugs the executable its filter picks; the block is croft's
    /// build step, not something lldb-dap is sent.
    #[test]
    fn a_cargo_launch_debugs_the_executable_its_filter_picks() {
        let rc = cargo_cfg(
            r#"{ "args": ["build", "--bin=rc", "--package=rc"], "filter": { "name": "rc", "kind": "bin" },
                 "env": { "RUSTFLAGS": "-g" }, "cwd": "sub" }"#,
        );
        assert!(rc.program.is_none());
        let spec = cargo_spec(&rc).expect("a cargo block").unwrap();
        assert_eq!(
            cargo_build_args(&spec),
            ["build", "--bin=rc", "--package=rc", "--message-format=json"]
        );
        assert_eq!(spec.env.get("RUSTFLAGS").map(String::as_str), Some("-g"));
        assert_eq!(spec.cwd, Some(PathBuf::from("sub")));
        assert_eq!(
            cargo_artifact(CARGO_JSON_1392, &spec).unwrap(),
            PathBuf::from("/w/target/debug/rc")
        );
        let lib_tests = cargo_spec(&cargo_cfg(
            r#"{ "args": ["test", "--lib"], "filter": { "kind": "lib" } }"#,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(
            cargo_build_args(&lib_tests),
            ["test", "--no-run", "--lib", "--message-format=json"]
        );
        assert_eq!(
            cargo_artifact(CARGO_JSON_1392, &lib_tests).unwrap(),
            PathBuf::from("/w/target/debug/deps/rc-1a2b")
        );
        let mut with_program = rc.clone();
        with_program.program = Some(String::from("/w/target/debug/rc"));
        let req = lldb_request(&with_program);
        assert!(req["arguments"].get("cargo").is_none(), "{req}");
        assert_eq!(req["arguments"]["program"], "/w/target/debug/rc");
    }

    /// #1392 negatives: no executable or several matching the filter is an
    /// error naming it; a `cargo` block with no args is refused; a config
    /// without one has no cargo step.
    #[test]
    fn a_cargo_launch_refuses_no_match_or_several() {
        let spec = |cargo: &str| cargo_spec(&cargo_cfg(cargo)).unwrap();
        let none = spec(r#"{ "args": ["build"], "filter": { "name": "nope" } }"#).unwrap();
        let err = cargo_artifact(CARGO_JSON_1392, &none).unwrap_err();
        assert!(
            err.contains("no executable matching name \"nope\""),
            "{err}"
        );
        let both = spec(r#"{ "args": ["build"] }"#).unwrap();
        let err = cargo_artifact(CARGO_JSON_1392, &both).unwrap_err();
        assert!(
            err.contains("2 executables matching no filter") && err.contains("rc, rc-1a2b"),
            "{err}"
        );
        assert!(spec(r#"{ "filter": { "name": "rc" } }"#).is_err());
        assert!(spec(r#"{ "args": [] }"#).is_err());
        let plain = config(r#"[{ "name": "P", "type": "lldb", "program": "/bin/true" }]"#);
        assert!(cargo_spec(&resolve(&plain, &ctx()).unwrap()).is_none());
    }

    /// #1392 end to end: cargo really builds a crate and the bin is picked;
    /// a crate that does not compile is an error carrying cargo's message.
    #[test]
    fn cargo_builds_the_program_or_says_why_not() {
        let tmp = tempfile::tempdir().unwrap();
        let w = tmp.path();
        std::fs::create_dir_all(w.join("src")).unwrap();
        std::fs::write(
            w.join("Cargo.toml"),
            "[package]\nname = \"rc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(w.join("src/main.rs"), "fn main() { println!(\"hi\"); }\n").unwrap();
        let spec = CargoSpec {
            args: vec![
                String::from("build"),
                String::from("--bin=rc"),
                String::from("--offline"),
            ],
            name: Some(String::from("rc")),
            kind: Some(String::from("bin")),
            // Its own target dir: the suite's is locked by the outer cargo.
            env: BTreeMap::from([(
                String::from("CARGO_TARGET_DIR"),
                w.join("target").to_string_lossy().into_owned(),
            )]),
            cwd: None,
        };
        let exe = build_cargo_program(&spec, w).unwrap();
        assert!(exe.is_file(), "{}", exe.display());
        assert_eq!(exe.file_name().unwrap(), "rc");
        std::fs::write(w.join("src/main.rs"), "fn main() { let x: u8 = \"no\"; }\n").unwrap();
        let err = build_cargo_program(&spec, w).unwrap_err();
        assert!(
            err.starts_with("cargo build --bin=rc --offline failed: ")
                && err.contains("mismatched types"),
            "{err}"
        );
    }
}
