//! Resolving a configuration: substitution done, the `envFile` loaded and the mapped
//! fields split out, cppdbg's spellings and the keys lldb-dap ignores, and the adapter
//! family a `type` maps to.

use super::*;

/// Map a launch.json `type` onto the adapter family croft drives. Covers the
/// names the three bundled families are known under in existing repos.
pub fn adapter_for_type(type_name: &str) -> Option<AdapterKind> {
    match type_name {
        "python" | "debugpy" => Some(AdapterKind::Debugpy),
        "lldb" | "lldb-dap" | "cppdbg" | "codelldb" => Some(AdapterKind::LldbDap),
        "go" => Some(AdapterKind::Delve),
        "node" | "pwa-node" | "node-terminal" | "javascript" => Some(AdapterKind::JsDebug),
        // js-debug drives Chrome and Edge itself (#1410).
        t if browser_type(t).is_some() => Some(AdapterKind::JsDebug),
        _ => None,
    }
}

/// A configuration with substitution done and the mapped fields split out,
/// ready to become a DAP request. `extra` carries every field croft didn't
/// map, already substituted, for verbatim passthrough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub name: String,
    pub kind: AdapterKind,
    /// The launch.json `type` as written, which picks js-debug's own type.
    pub type_name: String,
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
pub(super) const MAPPED_KEYS: &[&str] = &[
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

/// cppdbg's spellings of mapped fields (#1394): `environment` joins `env`
/// and `stopAtEntry` is `stopOnEntry`.
pub(super) const CPPDBG_MAPPED_KEYS: &[&str] = &["environment", "stopAtEntry"];

/// cppdbg keys for gdb or the C/C++ extension that lldb-dap, which runs a
/// cppdbg configuration in croft, has no use for (#1394).
pub(super) const CPPDBG_IGNORED_KEYS: &[&str] = &[
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
            "config \"{}\": unsupported type \"{}\" (python, lldb, node, go, chrome and msedge families are supported)",
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
    // A browser config maps its URLs onto files under `webRoot`, which VS
    // Code defaults to the workspace folder (#1410).
    if browser_type(&cfg.type_name).is_some() && !extra.contains_key("webRoot") {
        extra.insert(
            "webRoot".into(),
            json!(ctx.workspace_folder.to_string_lossy()),
        );
    }
    if cfg.type_name == "node-terminal" && cfg.request == RequestKind::Launch {
        node_terminal_command(&mut extra);
    }
    let mut program = get_str("program");
    if kind == AdapterKind::Delve && cfg.request == RequestKind::Launch {
        resolve_go_auto_mode(&mut extra, &mut program, ctx.file.as_deref());
    }
    Ok(ResolvedConfig {
        name: cfg.name.clone(),
        kind,
        type_name: cfg.type_name.clone(),
        request: cfg.request,
        program,
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

/// Start from the passthrough fields and layer the mapped ones on top, so a
/// mapped field always wins over a duplicate left in `extra`.
pub(super) fn base_arguments(rc: &ResolvedConfig) -> Map<String, Value> {
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

pub(super) fn request_value(command: &str, arguments: Map<String, Value>) -> Value {
    json!({
        "type": "request",
        "command": command,
        "arguments": Value::Object(arguments),
    })
}
