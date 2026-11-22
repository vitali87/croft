//! debugpy's `launch` and `attach` requests, and where an attach with `connect` goes.

use super::*;

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
