//! The interpreter a debugged Python program runs under: the project's own (its venv, or
//! `python3`), the one a launch request or a launch.json config names, Debug Test's, and
//! the note on one older than attach's floor.

use super::*;

/// The interpreter that runs a Python project's own code: its `.venv` python
/// (where the project's deps, pytest included, are importable) or the system
/// python3. The debug venv's python is only the debugpy ADAPTER host — the
/// debuggee must be this one, or a pytest module launch can't import pytest.
pub(in crate::app) fn project_python(root: &Path) -> PathBuf {
    project_python_on(root, std::env::var_os("PATH").as_deref())
        .unwrap_or_else(|| PathBuf::from("python3"))
}

/// [`project_python`] with `path_var` as `PATH`, None where it would fall
/// back to a bare `python3`: no `.venv` and no `python3` on `path_var`.
pub(in crate::app) fn project_python_on(
    root: &Path,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let venv = root.join(".venv").join("bin").join("python");
    if venv.is_file() {
        return Some(venv);
    }
    std::env::split_paths(path_var?)
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
}

/// Walk from `start` up to and including `workspace_root` looking for a
/// venv-style Python interpreter. Returns `(interpreter_path, owning_dir)`
/// where `owning_dir` is the directory that contains the venv folder — the
/// natural cwd for the run.
///
/// The walk stops at `workspace_root`: a venv that lives ABOVE the workspace
/// would belong to a different project the user has not opened, and using it
/// silently would surprise them.
pub(in crate::app) fn find_python_venv(
    start: &Path,
    workspace_root: &Path,
) -> Option<(PathBuf, PathBuf)> {
    let mut probe = start.to_path_buf();
    loop {
        for venv_name in [".venv", "venv", ".env"] {
            let py = probe.join(venv_name).join("bin").join("python");
            if py.is_file() {
                return Some((py, probe));
            }
        }
        if probe == workspace_root {
            return None;
        }
        match probe.parent() {
            Some(parent) if parent != probe => {
                if !parent.starts_with(workspace_root) && parent != workspace_root {
                    return None;
                }
                probe = parent.to_path_buf();
            }
            _ => return None,
        }
    }
}

/// The interpreter that runs Python code in `dir`: the nearest venv up to
/// `workspace_root` ([`find_python_venv`]), else [`project_python`]. Live
/// Run and the debugger's program both use it, so a script debugs against
/// the same installed packages it runs with (#864).
pub(in crate::app) fn project_python_for(dir: &Path, workspace_root: &Path) -> PathBuf {
    find_python_venv(dir, workspace_root)
        .map(|(py, _)| py)
        .unwrap_or_else(|| project_python(workspace_root))
}

/// The interpreter a debugged Python program in `dir` runs under (#864):
/// the one Run would use ([`project_python_for`]) when it exists, a venv or
/// a `python3` on `path_var`. None otherwise, and the launch keeps the debug
/// venv's own python, where the program always ran, rather than a bare
/// `python3` that cannot start.
pub(in crate::app) fn debuggee_python(
    dir: &Path,
    workspace_root: &Path,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    find_python_venv(dir, workspace_root)
        .map(|(py, _)| py)
        .or_else(|| project_python_on(workspace_root, path_var))
}

/// What a Python debug launch says about the interpreter its program runs
/// under (#864). Err is the refusal when debugpy cannot run there at all;
/// Some is the clause naming one older than attach's 3.14 floor, `under
/// Python 3.12 (.venv); attach needs 3.14+`; None is for 3.14+, or a version
/// that cannot be read (the launch then reports for itself).
pub(in crate::app) fn debuggee_python_note(python: &Path) -> Result<Option<String>, String> {
    use crate::dap::install::{DEBUGPY_MIN_PYTHON, python_version, pyvenv_cfg};
    use crate::dap::remote_attach::MIN_ATTACH_VERSION;
    let Some(version) = python_version(python) else {
        return Ok(None);
    };
    let (major, minor) = (version.major, version.minor);
    // A venv by its folder name, as the issue's `.venv`; else the path.
    let label = pyvenv_cfg(python)
        .and_then(|cfg| Some(cfg.parent()?.file_name()?.to_string_lossy().into_owned()))
        .unwrap_or_else(|| python.display().to_string());
    // Minor versions only: a 3.10.0 debuggee meets debugpy's 3.10 floor.
    if (major, minor) < (DEBUGPY_MIN_PYTHON.major, DEBUGPY_MIN_PYTHON.minor) {
        return Err(format!(
            "Python {major}.{minor} ({label}) is too old to debug: debugpy needs {}.{}+",
            DEBUGPY_MIN_PYTHON.major, DEBUGPY_MIN_PYTHON.minor
        ));
    }
    if !version.supports_remote_attach() {
        return Ok(Some(format!(
            "under Python {major}.{minor} ({label}); attach needs {}.{}+",
            MIN_ATTACH_VERSION.major, MIN_ATTACH_VERSION.minor
        )));
    }
    Ok(None)
}

/// `lead` followed by the note on the debuggee's interpreter, if any (#864).
pub(in crate::app) fn with_python_note(lead: String, note: Option<&str>) -> String {
    match note {
        Some(note) => format!("{lead} {note}"),
        None => lead,
    }
}

/// The interpreter a debugpy `launch` request runs its program under: its
/// `python` (a path, or a command line whose first word is one) or legacy
/// `pythonPath`. A relative path with a folder in it is found where debugpy
/// starts it, so the version check runs the same file (#864 review): the
/// request's `cwd`, else the folder of a `program`, else `adapter_dir`, the
/// folder the adapter runs in. A bare name is left to PATH, as debugpy
/// leaves it.
pub(in crate::app) fn launch_request_python(
    request: &serde_json::Value,
    adapter_dir: &Path,
) -> Option<PathBuf> {
    let args = &request["arguments"];
    let python = args.get("python").or_else(|| args.get("pythonPath"))?;
    let python = PathBuf::from(python.as_str().or_else(|| python.get(0)?.as_str())?);
    if python.is_absolute() || python.components().count() < 2 {
        return Some(python);
    }
    let program_dir = args["program"]
        .as_str()
        .and_then(|p| Path::new(p).parent())
        .filter(|dir| !dir.as_os_str().is_empty());
    let launch_dir = match args["cwd"].as_str() {
        Some(cwd) => adapter_dir.join(cwd),
        None => program_dir.map_or_else(|| adapter_dir.to_path_buf(), |d| adapter_dir.join(d)),
    };
    Some(launch_dir.join(python))
}

/// The interpreter Debug Test's pytest runs under once any debug-test
/// config is applied (#1398): the request's own `python`, a config's
/// included, but not the debug venv's stand-in for a project that has no
/// interpreter of its own, which was never the project's to report on.
pub(in crate::app) fn debug_test_python(
    request: &serde_json::Value,
    root: &Path,
    adapter_py: &Path,
    project_found: bool,
) -> Option<PathBuf> {
    launch_request_python(request, root).filter(|py| project_found || py != adapter_py)
}

/// The interpreter a launch.json Python config's program runs under when
/// the config names none (#864): resolved from the program's directory,
/// or from `cwd` for a `module` launch, as [`debuggee_python`] does, with
/// `adapter_python` (the debug venv's) only when that finds nothing.
pub(in crate::app) fn config_project_python(
    rc: &crate::dap::configs::ResolvedConfig,
    cwd: &Path,
    workspace_root: &Path,
    adapter_python: &Path,
) -> PathBuf {
    // A `module` launch drops `program` from the request (debugpy refuses
    // both), so the program's directory says nothing about what runs.
    let program = rc.program.as_deref().filter(|_| rc.module.is_none());
    let dir = program
        .map(|p| crate::dap::configs::absolute_in(p, cwd))
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| cwd.to_path_buf());
    debuggee_python(&dir, workspace_root, std::env::var_os("PATH").as_deref())
        .unwrap_or_else(|| adapter_python.to_path_buf())
}
