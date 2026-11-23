//! vscode-js-debug's `launch` and `attach` requests: browsers and their headless
//! launch, `node-terminal`'s command, what a Node launch must name, and the
//! `__workspaceFolder` it sends.

use super::*;

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
pub(super) fn node_terminal_command(extra: &mut Map<String, Value>) {
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
pub(super) fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Whether `runtimeExecutable` names something to run: a nonempty string,
/// not `null` or `""`.
pub(super) fn names_runtime_executable(extra: &Map<String, Value>) -> bool {
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
