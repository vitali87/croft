//! pytest's command line: the interpreter or script that runs it, whether the project's
//! Python has it, and its verbosity for runs and discovery.

use super::*;

/// How to launch pytest for a workspace: a program, and the arguments that
/// come before pytest's own, looked up on `path_var`. A project venv
/// ([`crate::python_env::VENV_NAMES`]: `.venv/`, `venv/`, then `.env/`) with
/// pytest installed (its `pytest` script, or a python that imports it) wins,
/// run as `<venv>/bin/python -m pytest` the way VS Code runs it: `-m` puts
/// the root on `sys.path`, so tests import the project's own top-level
/// modules, which the `pytest` script alone does not (#845). Without one, `python3 -m pytest` when that `python3` can
/// import pytest: a `pytest` script on `PATH` may belong to another
/// interpreter (a uv tool's), which cannot import the project's
/// dependencies. Then the `pytest` script; with no script anywhere,
/// `python3 -m pytest`, which at least fails with a reason instead of a
/// spawn error.
pub(super) fn pytest_launch_on(
    root: &Path,
    path_var: Option<&std::ffi::OsStr>,
) -> (PathBuf, &'static [&'static str]) {
    const MODULE: &[&str] = &["-m", "pytest"];
    for venv in crate::python_env::VENV_NAMES {
        let bin = root.join(venv).join("bin");
        let python = bin.join("python");
        // `-m pytest` needs the module: the console script says it is
        // installed without a process, and without the script the venv's
        // python is asked.
        if crate::proc::find::runnable(&python)
            && (crate::proc::find::runnable(&bin.join("pytest"))
                || python_has_pytest(root, &python))
        {
            return (python, MODULE);
        }
    }
    let python3 = python3_program(path_var);
    if python_has_pytest(root, &python3) {
        return (python3, MODULE);
    }
    match pytest_script(path_var) {
        Some(script) => (script, &[]),
        None => (python3, MODULE),
    }
}

/// [`pytest_launch_on`] for a run of the tests in `file`: under the venv
/// nearest the file when it has pytest, the interpreter Debug Test runs the
/// same test under (#864, #2810); else the workspace's.
pub(super) fn pytest_launch_for(
    root: &Path,
    file: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
) -> (PathBuf, &'static [&'static str]) {
    let nearest = file
        .and_then(Path::parent)
        .and_then(|dir| crate::python_env::nearest_venv(dir, root));
    if let Some((python, _)) = nearest
        && (python
            .parent()
            .is_some_and(|bin| crate::proc::find::runnable(&bin.join("pytest")))
            || python_has_pytest(root, &python))
    {
        return (python, &["-m", "pytest"]);
    }
    pytest_launch_on(root, path_var)
}

/// The test file a pytest node id or path prefix (`tests/test_x.py::test_a`,
/// `tests/test_x.py`) names under `root`, when it names one.
pub(super) fn pytest_node_file(root: &Path, node: &str) -> Option<PathBuf> {
    let path = node.split("::").next()?;
    path.ends_with(".py").then(|| root.join(path))
}

/// Absolute path to `python3`: `PATH`, then Homebrew's and the usual
/// local prefix, for the GUI-stripped-PATH reason [`pytest_script`]
/// gives. Bare `python3` when none is found.
pub(super) fn python3_program(path_var: Option<&std::ffi::OsStr>) -> PathBuf {
    let path_dirs = path_var.map(std::env::split_paths).into_iter().flatten();
    let fallback = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
    crate::proc::find::first_in("python3", path_dirs.chain(fallback))
        .unwrap_or_else(|| PathBuf::from("python3"))
}

/// Whether `python` can import pytest when run in `root`, asked once per
/// root and interpreter for the life of the process: discovery and every
/// run launch pytest, and the answer only changes when pytest is
/// installed or removed.
pub(super) fn python_has_pytest(root: &Path, python: &Path) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static ANSWERS: OnceLock<Mutex<HashMap<(PathBuf, PathBuf), bool>>> = OnceLock::new();
    let key = (root.to_path_buf(), python.to_path_buf());
    let answers = ANSWERS.get_or_init(Default::default);
    if let Some(&known) = answers.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return known;
    }
    let has = Command::new(python)
        .args(["-c", "import pytest"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    answers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, has);
    has
}

/// Absolute path to a `pytest` script outside any project venv: `PATH`,
/// then the usual user/tool install dirs — resolved absolutely like
/// [`cargo_cmd`] because a GUI-launched croft inherits the stripped
/// launchd PATH.
pub(super) fn pytest_script(path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let path_dirs = path_var.map(std::env::split_paths).into_iter().flatten();
    let user_bin = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("bin"));
    let dirs = path_dirs
        .chain(user_bin)
        .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    crate::proc::find::first_in("pytest", dirs)
}

/// pytest's verbosity for a run: `<id> PASSED` lines are printed from 1
/// up. Set outright, not with `-v`, which counts on from the project's
/// `addopts`: with the common `-q` there, `-v` lands on 0 and the run
/// prints only dots, so no result is read (#1502).
pub(super) const PYTEST_RUN_VERBOSITY: &str = "--verbosity=1";

/// pytest's verbosity for `--collect-only`: one node id per line is level
/// -1 exactly. `-q` would stack on an `addopts` `-q` to -2, which prints a
/// count per file instead (#1502).
pub(super) const PYTEST_COLLECT_VERBOSITY: &str = "--verbosity=-1";

/// `pytest <args>` rooted at the workspace, launched as [`pytest_launch_on`]
/// says on croft's `PATH`, with piped stdio. Discovery, runs and coverage all come through
/// here, so they always agree on the interpreter.
pub(super) fn pytest_cmd(root: &Path, args: &[&str]) -> Command {
    pytest_cmd_in(root, None, args)
}

/// [`pytest_cmd`] for a run of the tests in `file`, launched as
/// [`pytest_launch_for`] says.
pub(super) fn pytest_cmd_in(root: &Path, file: Option<&Path>, args: &[&str]) -> Command {
    let (program, lead) = pytest_launch_for(root, file, std::env::var_os("PATH").as_deref());
    let mut cmd = Command::new(program);
    cmd.args(lead)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}
