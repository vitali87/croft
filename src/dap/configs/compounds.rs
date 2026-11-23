//! Compounds: several configurations launched together, read from `compounds` and
//! resolved against the configurations beside them.

use super::*;

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
    /// The answers given for this launch to its members' `${input:…}`
    /// variables (#1424).
    pub answers: BTreeMap<String, String>,
}

/// Parse the `compounds` array of a launch.json body. Sibling key of
/// `configurations`, so this reads the same text [`parse_launch_json`] does.
/// Entries without a `name` or a non-empty `configurations` list are skipped —
/// VS Code refuses to run those too, and an empty compound would present as a
/// picker row that does nothing.
pub fn parse_compounds(text: &str, source: &'static str) -> Vec<Compound> {
    let Ok(v) = serde_json::from_str::<Value>(&crate::jsonc::strip_jsonc(text)) else {
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
                answers: BTreeMap::new(),
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
