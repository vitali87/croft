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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugConfig {
    pub name: String,
    /// The `type` as written (`python`, `lldb`, `node`, …); mapped to an
    /// [`AdapterKind`] only at resolve time so an unsupported type shows a
    /// per-config error instead of being silently dropped from the picker.
    pub type_name: String,
    pub request: RequestKind,
    /// Which file declared it, for the picker's detail column.
    pub source: &'static str,
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
                raw: obj.clone(),
            })
        })
        .collect()
}

/// A `compounds` entry: several configurations launched together under one
/// name. Members are stored as written and resolved against the file's
/// `configurations` at launch time, so a compound naming a configuration that
/// is later renamed fails loudly rather than launching a subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compound {
    pub name: String,
    pub configurations: Vec<String>,
    /// Keys croft does not honour that this compound ACTUALLY ASKS FOR:
    /// `preLaunchTask` with a non-empty value, or a non-empty `presentation`.
    /// A key present but requesting nothing (`null`, `""`, `{}`) is not
    /// recorded — croft delivers that behaviour by doing nothing.
    ///
    /// `stopAll` is deliberately absent at either value: it decides whether
    /// ending one session ends the others, which is meaningless for the
    /// single-session launch this list gates.
    ///
    /// Recorded rather than silently dropped because a ONE-member compound now
    /// launches, and launching it via its member alone would run neither the
    /// compound's own pre-launch task nor respect its presentation. Note the
    /// MULTI-member path still discards these silently: it refuses for the
    /// session count first, and #310 is the blocker the user must clear.
    pub unsupported_keys: Vec<&'static str>,
    /// Which file declared it. A compound's members name configurations in
    /// ITS OWN file first: both files may declare the same name, and binding
    /// a `.vscode` compound to a `.croft` config of that name would debug a
    /// different program than the compound was written against.
    pub source: &'static str,
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
            // objects in multi-root workspaces; croft resolves against one
            // root, so take the name either way rather than dropping the row.
            //
            // Collect FALLIBLY: one unusable member drops the whole compound
            // rather than shrinking it to the members that happen to parse.
            // `resolve_compound` already refuses to launch a subset when a
            // member names no configuration; a malformed member is the same
            // situation one boundary earlier, and silently running two of the
            // three a user wrote is the outcome both guards exist to prevent.
            let configurations: Vec<String> = obj
                .get("configurations")?
                .as_array()?
                .iter()
                .map(|m| match m {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(o) => o.get("name")?.as_str().map(str::to_string),
                    _ => None,
                })
                .collect::<Option<Vec<String>>>()?;
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
            // `stopAll` is excluded from the ONE-member gate entirely, at either
            // value: it governs whether ending one session ends the others, and
            // with a single session there are no others. It cannot change a
            // one-member outcome, so it cannot be a reason to refuse one.
            let asks_for_something = |k: &str| match obj.get(k) {
                None | Some(Value::Null) => false,
                Some(Value::String(s)) => !s.is_empty(),
                Some(Value::Bool(b)) => *b,
                Some(Value::Object(o)) => !o.is_empty(),
                Some(_) => true,
            };
            let unsupported_keys: Vec<&'static str> = ["preLaunchTask", "presentation"]
                .into_iter()
                .filter(|k| asks_for_something(k))
                .collect();
            Some(Compound {
                name,
                configurations,
                source,
                unsupported_keys,
            })
        })
        .collect()
}

/// Resolve a compound's member names against the configurations declared
/// beside it, in the compound's own order. Pass [`discover_configs_all`], not
/// [`discover_configs`]: the latter drops a lower-precedence duplicate by name,
/// which is exactly the entry a `.vscode` compound naming that name needs, so
/// the source preference below would have nothing to prefer. Errors naming the first member that
/// does not exist — launching the subset that happens to resolve would debug
/// something other than what the user asked for, and silently.
pub fn resolve_compound<'a>(
    compound: &Compound,
    configs: &'a [DebugConfig],
) -> Result<Vec<&'a DebugConfig>, String> {
    compound
        .configurations
        .iter()
        .map(|want| {
            // The compound's own file first: `discover_configs` merges both
            // files with `.croft` winning a duplicate name, so a bare name
            // match would bind a `.vscode` compound to a `.croft` config that
            // may run a different program under a different adapter. Falling
            // back to any file keeps a compound working when its members live
            // in the sibling file, which VS Code also allows via `folder`.
            configs
                .iter()
                .find(|c| &c.name == want && c.source == compound.source)
                .or_else(|| configs.iter().find(|c| &c.name == want))
                .ok_or_else(|| {
                    format!(
                        "compound \"{}\" names configuration \"{}\", which no launch.json declares",
                        compound.name, want
                    )
                })
        })
        .collect()
}

/// Map a launch.json `type` onto the adapter family croft drives. Covers the
/// names the three bundled families are known under in existing repos.
pub fn adapter_for_type(type_name: &str) -> Option<AdapterKind> {
    match type_name {
        "python" | "debugpy" => Some(AdapterKind::Debugpy),
        "lldb" | "lldb-dap" | "cppdbg" | "codelldb" => Some(AdapterKind::LldbDap),
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
    pub stop_on_entry: bool,
    pub port: Option<u16>,
    pub process_id: Option<i64>,
    pub extra: Map<String, Value>,
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
    "stopOnEntry",
    "port",
    "processId",
    // Forced to internalConsole: croft debugs in-process and declines the
    // `runInTerminal` reverse request, so honouring an integratedTerminal
    // console would hang the session at launch.
    "console",
];

/// Resolve `cfg` against the editor state: substitute variables, load the
/// `envFile`, and split the mapped fields. Errors are user-facing strings
/// (unknown type, unresolved variable, unreadable envFile).
pub fn resolve(cfg: &DebugConfig, ctx: &SubstCtx) -> Result<ResolvedConfig, String> {
    let kind = adapter_for_type(&cfg.type_name).ok_or_else(|| {
        format!(
            "config \"{}\": unsupported type \"{}\" (python, lldb, and node families are supported)",
            cfg.name, cfg.type_name
        )
    })?;
    let raw = Value::Object(cfg.raw.clone());
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
    let extra: Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| !MAPPED_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Ok(ResolvedConfig {
        name: cfg.name.clone(),
        kind,
        request: cfg.request,
        program: get_str("program"),
        module: get_str("module"),
        args,
        env,
        cwd: get_str("cwd").map(|c| absolute_in(&c, &ctx.workspace_folder)),
        pre_launch_task: get_str("preLaunchTask"),
        stop_on_entry: obj
            .get("stopOnEntry")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        port,
        process_id,
        extra,
    })
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

/// Build the debugpy `launch`/`attach` request. `debug_venv_python` is the
/// interpreter of croft's debug venv, used as the debuggee interpreter when
/// the config doesn't name its own `python`.
pub fn debugpy_request(rc: &ResolvedConfig, debug_venv_python: &Path) -> Value {
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
            if !args.contains_key("python") {
                args.insert(
                    "python".into(),
                    json!([debug_venv_python.to_string_lossy()]),
                );
            }
            args.insert("console".into(), json!("internalConsole"));
            if !args.contains_key("justMyCode") {
                args.insert("justMyCode".into(), json!(false));
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

/// Build the lldb-dap `launch`/`attach` request. `env` uses the
/// `"VAR=VALUE"` string-array format lldb-dap has accepted since
/// lldb-vscode. Attach targets `pid`.
pub fn lldb_request(rc: &ResolvedConfig) -> Value {
    let mut args = base_arguments(rc);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SubstCtx {
        SubstCtx {
            workspace_folder: PathBuf::from("/work/proj"),
            file: Some(PathBuf::from("/work/proj/src/main.py")),
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

    #[test]
    fn maps_the_three_adapter_families_and_rejects_unknown_types() {
        use AdapterKind::*;
        for (t, k) in [
            ("python", Debugpy),
            ("debugpy", Debugpy),
            ("lldb", LldbDap),
            ("cppdbg", LldbDap),
            ("node", JsDebug),
            ("pwa-node", JsDebug),
        ] {
            assert_eq!(adapter_for_type(t), Some(k), "{t}");
        }
        assert_eq!(adapter_for_type("go"), None);
        let cfg = config(r#"[{ "name": "Go", "type": "go", "program": "main.go" }]"#);
        let err = resolve(&cfg, &ctx()).unwrap_err();
        assert!(err.contains("unsupported type \"go\""), "{err}");
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
        assert_eq!(req["arguments"]["justMyCode"], false);
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

        let members = resolve_compound(&compounds[0], &configs).expect("resolves");
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

        let err = resolve_compound(&compounds[0], &cfgs)
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
    /// `stopAll` is excluded at EITHER value: it decides whether ending one
    /// session ends the others, and a one-member compound has no others.
    #[test]
    fn a_compound_key_that_requests_nothing_is_not_recorded_as_unsupported() {
        let text = r#"{
            "configurations": [ { "name": "A", "type": "node", "program": "a.js" } ],
            "compounds": [
                { "name": "DefaultStopAll", "configurations": ["A"], "stopAll": false },
                { "name": "TrueStopAll", "configurations": ["A"], "stopAll": true },
                { "name": "NulledTask", "configurations": ["A"], "preLaunchTask": null },
                { "name": "EmptyTask", "configurations": ["A"], "preLaunchTask": "" },
                { "name": "EmptyPresentation", "configurations": ["A"], "presentation": {} },
                { "name": "RealTask", "configurations": ["A"], "preLaunchTask": "build" }
            ]
        }"#;
        let cs = parse_compounds(text, ".vscode/launch.json");
        assert_eq!(cs.len(), 6, "precondition: every compound parsed");
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

        // The positive control: without it, six empties could equally mean the
        // detection never records anything at all.
        assert_eq!(
            by("RealTask").unsupported_keys,
            vec!["preLaunchTask"],
            "and a key that DOES ask for something is still recorded"
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
        let members = resolve_compound(&compounds[0], &cfgs).unwrap();
        assert_eq!(
            members.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["Server", "Client"],
            "compound order wins over the order in `configurations`"
        );
    }
}
