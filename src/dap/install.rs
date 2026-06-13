//! debugpy environment provisioning.
//!
//! croft owns a dedicated debug virtualenv at `~/.croft/debug-venv` rather than
//! installing debugpy into the user's interpreter: PEP 668 marks the uv-managed
//! CPython externally-managed (pip refuses), and polluting the user's Python
//! would be wrong regardless. Mirrors the `~/.croft/servers` LSP store. The venv
//! is built from CPython 3.14+ (`uv venv -p 3.14`) — the only line croft's
//! debugger supports (PEP 768), with no fallback to older interpreters.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Minimum CPython line croft debugs. `uv` resolves the newest matching.
const PYTHON_VERSION: &str = "3.14";

/// `~/.croft/debug-venv`, or `None` when `$HOME` is unset (the guard the rest of
/// croft uses for `~/.croft`).
pub fn debug_venv_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".croft").join("debug-venv"))
}

/// The venv's interpreter, used both to run `-m debugpy.adapter` and as the
/// debuggee interpreter.
pub fn debug_venv_python() -> Option<PathBuf> {
    Some(debug_venv_dir()?.join("bin").join("python"))
}

/// Ensure the debug venv exists with debugpy, creating it via `uv` on first use.
/// Returns the venv interpreter path. Blocking: the first call shells out to
/// `uv venv` + `uv pip install debugpy` (a few seconds); subsequent calls are a
/// cheap existence check.
pub fn ensure_debug_venv() -> Result<PathBuf> {
    let py = debug_venv_python().context("$HOME unset; cannot locate ~/.croft/debug-venv")?;
    if py.exists() {
        return Ok(py);
    }
    let dir = debug_venv_dir().context("$HOME unset")?;

    let venv = Command::new("uv")
        .args(["venv", "-p", PYTHON_VERSION])
        .arg(&dir)
        .status()
        .context("running `uv venv` (is uv installed and on PATH?)")?;
    if !venv.success() {
        bail!("`uv venv -p {PYTHON_VERSION}` failed (is CPython {PYTHON_VERSION} available?)");
    }

    let pip = Command::new("uv")
        .args(["pip", "install", "--python"])
        .arg(&py)
        .arg("debugpy")
        .status()
        .context("running `uv pip install debugpy`")?;
    if !pip.success() {
        bail!("`uv pip install debugpy` failed");
    }
    Ok(py)
}
