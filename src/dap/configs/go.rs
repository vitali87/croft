//! delve's `launch` and `attach` requests: the Go extension's `auto` mode resolved, and
//! where Connect to server finds a headless delve.

use super::*;

/// The Go extension's `"mode": "auto"` (#1395), resolved the way the
/// extension resolves it before starting delve, which rejects `auto`:
/// `test` when the program is a `_test.go` file, or is a folder while the
/// active file is a `_test.go` file; `debug` otherwise. Delve tests a whole
/// package, so a test file's program becomes its folder.
pub(super) fn resolve_go_auto_mode(
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
