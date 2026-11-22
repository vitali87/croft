//! The configuration for debugging tests: the one whose `purpose` says so, and laying
//! it over croft's own pytest launch.

use super::*;

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
pub(super) const DEBUG_TEST_META_KEYS: &[&str] = &["purpose", "presentation"];

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
