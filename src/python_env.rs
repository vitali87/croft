//! Which Python a project runs under (#2810): the venv folder names croft
//! knows, and the walk from a file's folder up to its workspace root that
//! finds the venv nearest it. Run, Debug, Debug Test, Run Test, the Python
//! tasks and the notebook kernel all ask here, so the same code imports
//! under one and the same interpreter.

use std::path::{Path, PathBuf};

/// The venv folder names croft looks for, in order: `uv` and `python -m
/// venv`'s usual `.venv`, then `venv` and `.env`.
pub(crate) const VENV_NAMES: [&str; 3] = [".venv", "venv", ".env"];

/// The python of a venv directly in `dir`, by [`VENV_NAMES`]' order.
pub(crate) fn venv_python_in(dir: &Path) -> Option<PathBuf> {
    VENV_NAMES
        .iter()
        .map(|name| dir.join(name).join("bin").join("python"))
        .find(|py| crate::proc::find::runnable(py))
}

/// The venv nearest `start`, walking up to and including `workspace_root`:
/// its python and the folder holding it, the natural folder to run in.
///
/// The walk stops at `workspace_root`: a venv that lives ABOVE the workspace
/// would belong to a different project the user has not opened, and using it
/// silently would surprise them.
pub(crate) fn nearest_venv(start: &Path, workspace_root: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut probe = start.to_path_buf();
    loop {
        if let Some(py) = venv_python_in(&probe) {
            return Some((py, probe));
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
