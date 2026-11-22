//! lldb-dap's `launch` and `attach` requests, the processes an attach can pick from, and
//! CodeLLDB's `cargo` block: building with cargo and picking the executable it built.

use super::*;

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
