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
mod debug_test;
mod discovery;
mod env_files;
mod go;
mod inputs;
mod lldb;
mod presentation;
mod python;
mod resolve;
mod substitution;
pub use compounds::Compound;
pub use compounds::resolve_compound;
use compounds::*;
pub use debug_test::debug_test_config;
pub use debug_test::with_debug_test_config;
#[cfg(test)]
use debug_test::*;
pub use discovery::discover_compounds;
pub use discovery::discover_configs;
pub use discovery::discover_configs_all;
#[cfg(test)]
pub use discovery::parse_launch_json;
pub use discovery::workspace_folders;
use discovery::*;
use env_files::*;
pub use go::delve_remote_address;
pub use go::delve_request;
use go::*;
pub use inputs::InputKind;
pub use inputs::LaunchInput;
pub use inputs::inputs_to_ask;
pub use inputs::parse_inputs;
pub use inputs::text_input_refs;
pub use lldb::CargoSpec;
pub use lldb::build_cargo_program;
pub use lldb::cargo_spec;
pub use lldb::lldb_attach_has_target;
pub use lldb::lldb_request;
pub use lldb::parse_ps_processes;
#[cfg(test)]
use lldb::*;
pub use presentation::Presentation;
pub use presentation::visible_and_sorted;
use presentation::*;
pub use python::debugpy_connect_address;
pub use python::debugpy_request;
pub use resolve::ResolvedConfig;
pub use resolve::absolute_in;
pub use resolve::resolve;
use resolve::*;
pub use substitution::SubstCtx;
pub use substitution::substitute;
use substitution::*;

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
