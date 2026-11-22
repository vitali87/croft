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

mod compounds;
mod discovery;
mod inputs;
mod presentation;
pub use compounds::Compound;
pub use compounds::resolve_compound;
use compounds::*;
pub use discovery::discover_compounds;
pub use discovery::discover_configs;
pub use discovery::discover_configs_all;
#[cfg(test)]
pub use discovery::parse_launch_json;
pub use discovery::workspace_folders;
use discovery::*;
pub use inputs::InputKind;
pub use inputs::LaunchInput;
pub use inputs::inputs_to_ask;
pub use inputs::parse_inputs;
pub use inputs::text_input_refs;
pub use presentation::Presentation;
pub use presentation::visible_and_sorted;
use presentation::*;

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
    /// The file's `inputs` (#1424): what a `${input:id}` in this
    /// configuration asks for.
    pub inputs: Vec<LaunchInput>,
    /// The answers given for this launch, by input id; filled in once the
    /// user has answered every input the configuration uses.
    pub answers: BTreeMap<String, String>,
    /// The workspace folder this configuration belongs to when it is not
    /// the active one: a compound member named with `"folder"` (#1425)
    /// resolves `${workspaceFolder}` against its own folder.
    pub root: Option<PathBuf>,
    raw: Map<String, Value>,
}

impl DebugConfig {
    /// The `preLaunchTask` label, as written.
    pub fn pre_launch_task(&self) -> Option<&str> {
        self.raw.get("preLaunchTask").and_then(Value::as_str)
    }
}

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

/// The js-debug type for a browser launch.json `type` (#1410): `chrome` and
/// `msedge` are VS Code's names for js-debug's `pwa-chrome` and `pwa-msedge`.
/// `None` for every type that is not a browser.
pub fn browser_type(type_name: &str) -> Option<&'static str> {
    match type_name {
        "chrome" | "pwa-chrome" => Some("pwa-chrome"),
        "msedge" | "pwa-msedge" => Some("pwa-msedge"),
        _ => None,
    }
}

/// The port js-debug attaches to a browser on when the config names none:
/// Chrome's and Edge's default remote debugging port, as in VS Code.
pub const BROWSER_DEBUG_PORT: u16 = 9222;

/// What a js-debug config is missing before it can start, if anything. A
/// Node launch needs something to run (#1391: [`js_launch_has_target`]) and a
/// Node attach needs the inspector's `port`; a browser needs neither (#1410):
/// with no `url` or `file` it opens a blank page, and it attaches on port 9222
/// by default.
pub fn js_config_error(rc: &ResolvedConfig) -> Option<&'static str> {
    if browser_type(&rc.type_name).is_some() {
        return None;
    }
    match rc.request {
        RequestKind::Launch if !js_launch_has_target(rc) => {
            Some("a node launch needs a \"program\", a \"runtimeExecutable\" or a \"command\"")
        }
        RequestKind::Attach if rc.port.is_none() => Some("a node attach needs a \"port\""),
        _ => None,
    }
}

/// Launch the browser with no window (#1410), for a croft with no display
/// to show one on: `--headless=new` joins the config's `runtimeArgs` unless
/// they already choose a headless mode.
pub fn with_headless_browser(mut request: Value) -> Value {
    if let Some(args) = request["arguments"].as_object_mut() {
        let runtime = args
            .entry("runtimeArgs")
            .or_insert_with(|| Value::Array(Vec::new()));
        // js-debug accepts `"runtimeArgs": null`, which means no arguments.
        if runtime.is_null() {
            *runtime = Value::Array(Vec::new());
        }
        if let Some(list) = runtime.as_array_mut() {
            let headless = list
                .iter()
                .any(|a| a.as_str().is_some_and(|a| a.starts_with("--headless")));
            if !headless {
                list.push(json!("--headless=new"));
            }
        }
    }
    request
}

/// The editor state `${...}` variables resolve against.
#[derive(Debug, Clone)]
pub struct SubstCtx {
    pub workspace_folder: PathBuf,
    /// Active editor file; `None` makes `${file}`-family variables error.
    pub file: Option<PathBuf>,
    /// The user's answers to `${input:id}` (#1424), by id.
    pub inputs: BTreeMap<String, String>,
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
            if let Some(id) = var.strip_prefix("input:") {
                return ctx
                    .inputs
                    .get(id.trim())
                    .cloned()
                    .ok_or_else(|| format!("${{{var}}}: no answer for this input"));
            }
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

/// The Go extension's `"mode": "auto"` (#1395), resolved the way the
/// extension resolves it before starting delve, which rejects `auto`:
/// `test` when the program is a `_test.go` file, or is a folder while the
/// active file is a `_test.go` file; `debug` otherwise. Delve tests a whole
/// package, so a test file's program becomes its folder.
fn resolve_go_auto_mode(
    extra: &mut Map<String, Value>,
    program: &mut Option<String>,
    active: Option<&Path>,
) {
    if extra.get("mode").and_then(Value::as_str) != Some("auto") {
        return;
    }
    let decides = match program.as_deref() {
        Some(p) if p.ends_with(".go") => Some(p.to_string()),
        _ => active.map(|f| f.to_string_lossy().into_owned()),
    };
    let test = decides.is_some_and(|f| f.ends_with("_test.go"));
    extra.insert("mode".into(), json!(if test { "test" } else { "debug" }));
    if let Some(file) = program.as_deref().filter(|p| p.ends_with("_test.go")) {
        let dir = Path::new(file)
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| String::from("."));
        *program = Some(dir);
    }
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

/// Parse a dotenv-style file as python-dotenv and VS Code read it (#1389):
/// `KEY=VALUE` lines, `#` comment lines, blank lines and an optional
/// `export ` prefix. An unquoted value ends at a `#` that follows whitespace,
/// an inline comment. A double-quoted value may span lines and takes the
/// `\n`, `\t`, `\r`, `\"` and `\\` escapes; a single-quoted one is literal.
/// `${NAME}` in an unquoted or double-quoted value expands to NAME from an
/// earlier line, else from croft's environment, else to nothing, and
/// `${NAME:-default}` to `default` when NAME is set nowhere.
pub fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    parse_env_file_in(text, |name| std::env::var(name).ok())
}

/// [`parse_env_file`] with `process_env` standing in for croft's
/// environment.
fn parse_env_file_in(
    text: &str,
    process_env: impl Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut pos = 0;
    while pos < text.len() {
        let line_end = text[pos..].find('\n').map_or(text.len(), |i| pos + i);
        let line = &text[pos..line_end];
        pos = line_end + 1;
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let entry = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((key, raw)) = entry.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        let value = raw.trim_start();
        // Where `value` starts in `text`, for a quoted value that runs on
        // past this line.
        let start = line_end - line.len() + (line.len() - value.len());
        let quoted = match value.chars().next() {
            Some(q @ ('"' | '\'')) => closing_quote(text, start + 1, q).map(|end| (q, end)),
            _ => None,
        };
        let value = match quoted {
            Some(('\'', end)) => {
                pos = pos.max(next_line(text, end));
                text[start + 1..end].to_string()
            }
            Some((_, end)) => {
                pos = pos.max(next_line(text, end));
                let unescaped = unescape_double_quoted(&text[start + 1..end]);
                expand_env_refs(&unescaped, &out, &process_env)
            }
            None => {
                let comment = raw
                    .char_indices()
                    .find(|&(i, c)| c == '#' && raw[..i].ends_with(char::is_whitespace))
                    .map_or(raw.len(), |(i, _)| i);
                expand_env_refs(raw[..comment].trim(), &out, &process_env)
            }
        };
        out.insert(key.to_string(), value);
    }
    out
}

/// The index of the quote closing a value whose body starts at `from`, which
/// may be on a later line; `None` when it never closes. A double-quoted body
/// skips `\"` and any other backslash pair.
fn closing_quote(text: &str, from: usize, quote: char) -> Option<usize> {
    let mut chars = text[from..].char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' if quote == '"' => {
                chars.next();
            }
            c if c == quote => return Some(from + i),
            _ => {}
        }
    }
    None
}

/// Where the line holding `at` ends, past its newline: what follows a
/// closing quote on its line, a comment, is no part of the next entry.
fn next_line(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i + 1)
}

/// A double-quoted dotenv value's escapes, decoded; an unknown escape keeps
/// its backslash.
fn unescape_double_quoted(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(c @ ('"' | '\\')) => out.push(c),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `${NAME}` and `${NAME:-default}` in a dotenv value, expanded from the
/// file's earlier entries, then `process_env`. An unterminated `${` is kept
/// as written.
fn expand_env_refs(
    value: &str,
    file: &BTreeMap<String, String>,
    process_env: &impl Fn(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let (name, default) = match after[..end].split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (&after[..end], None),
        };
        match file.get(name).cloned().or_else(|| process_env(name)) {
            Some(found) => out.push_str(&found),
            None => out.push_str(default.unwrap_or("")),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
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
/// The Go extension's `auto` never reaches here: [`resolve`] settles it.
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

/// `request` with `__workspaceFolder` set to `root` unless the config set it
/// (#1414). VS Code sends it with every js-debug launch and attach; js-debug
/// resolves its `${workspaceFolder}` defaults (`rootPath`,
/// `resolveSourceMapLocations`) against it, and without it a breakpoint in a
/// compiled TypeScript file is bound only after startup code has run past it.
pub fn with_workspace_folder(mut request: Value, root: &Path) -> Value {
    if let Some(args) = request.get_mut("arguments").and_then(Value::as_object_mut) {
        args.entry("__workspaceFolder")
            .or_insert_with(|| json!(root.to_string_lossy()));
    }
    request
}

/// Build the vscode-js-debug `launch`/`attach` request (`pwa-node`, the
/// canonical Node type — croft already normalises onto it for zero-config).
/// A browser config becomes `pwa-chrome` or `pwa-msedge` (#1410), its `url`,
/// `file`, `webRoot` and the rest passed through as written.
pub fn js_request(rc: &ResolvedConfig) -> Value {
    let browser = browser_type(&rc.type_name);
    let mut args = base_arguments(rc);
    args.insert("type".into(), json!(browser.unwrap_or("pwa-node")));
    if !rc.env.is_empty() {
        args.insert("env".into(), json!(rc.env));
    }
    match rc.request {
        RequestKind::Launch => {
            args.insert("request".into(), json!("launch"));
            // A browser's console is its own; only Node has a choice.
            if browser.is_none() {
                args.insert("console".into(), json!("internalConsole"));
            }
            if !args.contains_key("sourceMaps") {
                args.insert("sourceMaps".into(), json!(true));
            }
            request_value("launch", args)
        }
        RequestKind::Attach => {
            args.insert("request".into(), json!("attach"));
            let port = rc.port.or(browser.map(|_| BROWSER_DEBUG_PORT));
            if let Some(port) = port {
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
mod tests;
