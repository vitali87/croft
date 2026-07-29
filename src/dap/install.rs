//! debugpy environment provisioning.
//!
//! croft owns a dedicated debug virtualenv at `~/.croft/debug-venv` rather than
//! installing debugpy into the user's interpreter: PEP 668 marks the uv-managed
//! CPython externally-managed (pip refuses), and polluting the user's Python
//! would be wrong regardless. Mirrors the `~/.croft/servers` LSP store. The venv
//! is built from CPython 3.14+ (`uv venv -p 3.14`) — the only line croft's
//! debugger supports (PEP 768), with no fallback to older interpreters.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// Minimum CPython line croft debugs. `uv` resolves the newest matching.
const PYTHON_VERSION: &str = "3.14";

/// Pinned vscode-js-debug release. The `js-debug-dap-vX.Y.Z.tar.gz` asset is the
/// bundled standalone debug server (the same artifact Zed and nvim/Mason use);
/// it extracts to `js-debug/src/dapDebugServer.js`. Bump deliberately.
const JS_DEBUG_VERSION: &str = "v1.117.0";

/// Cap on the js-debug tarball download (the v1.117 asset is ~10 MB; this is a
/// generous ceiling that still refuses a runaway/redirected body).
const MAX_JS_DEBUG_BYTES: u64 = 64 * 1024 * 1024;

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

    // .output(), never .status(): uv paints progress bars on stderr, and an
    // inherited TTY would spray them over the running TUI (same class as the
    // pdftoppm trailer-dictionary spray).
    let venv = Command::new("uv")
        .args(["venv", "-p", PYTHON_VERSION])
        .arg(&dir)
        .output()
        .context("running `uv venv` (is uv installed and on PATH?)")?;
    if !venv.status.success() {
        bail!(
            "`uv venv -p {PYTHON_VERSION}` failed (is CPython {PYTHON_VERSION} available?): {}",
            String::from_utf8_lossy(&venv.stderr).trim()
        );
    }

    let pip = Command::new("uv")
        .args(["pip", "install", "--python"])
        .arg(&py)
        .arg("debugpy")
        .output()
        .context("running `uv pip install debugpy`")?;
    if !pip.status.success() {
        bail!(
            "`uv pip install debugpy` failed: {}",
            String::from_utf8_lossy(&pip.stderr).trim()
        );
    }
    Ok(py)
}

/// `~/.croft/js-debug`, the install root for the vscode-js-debug server.
pub(crate) fn js_debug_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".croft").join("js-debug"))
}

/// Path to the extracted `dapDebugServer.js` (the standalone DAP server).
fn js_debug_server_path(dir: &Path) -> PathBuf {
    dir.join("js-debug").join("src").join("dapDebugServer.js")
}

/// Resolve the `node` runtime croft launches the js-debug server (and the
/// debuggee) with, returning an absolute path or the bare `node` command.
///
/// Version managers (nvm, fnm, asdf, volta) expose node only inside an
/// interactive login shell, and nvm *lazy-loads* it, so node's directory is not
/// on the inherited PATH and is not even on a captured login-shell PATH until
/// node first runs. So when the plain PATH scan misses, croft asks the user's
/// login+interactive shell to run node and print its own path — which triggers
/// the lazy loader. This mirrors how Zed resolves the shell environment for
/// GUI/launcher-started processes. Errors with an actionable hint when node
/// genuinely cannot be found.
pub fn node_program() -> Result<String> {
    if which("node") {
        return Ok(String::from("node"));
    }
    if let Some(path) = resolve_node_via_login_shell() {
        return Ok(path);
    }
    bail!("`node` not found on PATH; install Node.js to debug JavaScript/TypeScript")
}

/// The directory containing the resolved node binary, when it is an absolute
/// path. Prepended to the js-debug server's PATH so the *debuggee* node process
/// js-debug spawns (with a bare `node`) resolves to the same runtime.
pub fn node_bin_dir(node: &str) -> Option<PathBuf> {
    let p = Path::new(node);
    p.is_absolute().then(|| p.parent().map(Path::to_path_buf))?
}

/// Ask the user's login+interactive shell to print node's absolute path by
/// running it (`process.execPath`). `-i` is required because nvm defines its
/// lazy `node` function in the interactive rc; the call is bounded so a
/// misbehaving rc cannot hang croft.
fn resolve_node_via_login_shell() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| String::from("/bin/sh"));
    let out = run_bounded(
        &shell,
        &["-lic", "node -e 'process.stdout.write(process.execPath)'"],
        Duration::from_secs(8),
    )?;
    let path = out.trim();
    (!path.is_empty() && Path::new(path).is_file()).then(|| path.to_string())
}

/// Run `program args...` capturing stdout, killing it if it outlives `timeout`
/// (a login shell with a chatty/blocking rc must never wedge croft). Returns the
/// captured stdout on a clean exit, else `None`.
fn run_bounded(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    use std::io::Read;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
    let mut buf = String::new();
    child.stdout.take()?.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// Whether `bin` resolves on PATH (a `command -v` check via the shell-less
/// `which`-style scan of PATH entries).
fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
        .unwrap_or(false)
}

/// Ensure the vscode-js-debug server is installed, downloading + extracting the
/// pinned release tarball on first use. Returns the path to `dapDebugServer.js`.
/// Blocking: the first call downloads ~10 MB and shells out to `tar`; later
/// calls are a cheap existence check. Mirrors [`ensure_debug_venv`].
pub fn ensure_js_debug() -> Result<PathBuf> {
    let dir = js_debug_dir().context("$HOME unset; cannot locate ~/.croft/js-debug")?;
    let server = js_debug_server_path(&dir);
    if server.exists() {
        return Ok(server);
    }
    std::fs::create_dir_all(&dir).context("creating ~/.croft/js-debug")?;

    let url = format!(
        "https://github.com/microsoft/vscode-js-debug/releases/download/{JS_DEBUG_VERSION}/js-debug-dap-{JS_DEBUG_VERSION}.tar.gz"
    );
    let tarball = dir.join("js-debug-dap.tar.gz");
    download_to_file(&url, &tarball)?;

    // The asset extracts a top-level `js-debug/` directory; -C lands it under
    // ~/.croft/js-debug so the server is at js-debug/src/dapDebugServer.js.
    let tar = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&dir)
        .output()
        .context("running `tar -xzf` on the js-debug release")?;
    let _ = std::fs::remove_file(&tarball);
    if !tar.status.success() {
        bail!(
            "extracting the js-debug release failed: {}",
            String::from_utf8_lossy(&tar.stderr).trim()
        );
    }
    if !server.exists() {
        bail!("js-debug release did not contain dapDebugServer.js (layout changed?)");
    }
    Ok(server)
}

/// Stream `url` to `dest`, capped at [`MAX_JS_DEBUG_BYTES`]. Follows redirects
/// (the GitHub release URL 302s to a CDN).
fn download_to_file(url: &str, dest: &Path) -> Result<()> {
    use std::io::Read;
    let resp = ureq::get(url)
        .call()
        .context("downloading js-debug release")?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(MAX_JS_DEBUG_BYTES)
        .read_to_end(&mut bytes)
        .context("reading js-debug release body")?;
    std::fs::write(dest, &bytes).context("writing js-debug release tarball")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_bin_dir_is_the_parent_of_an_absolute_node_path() {
        assert_eq!(
            node_bin_dir("/Users/x/.nvm/versions/node/v23.7.0/bin/node"),
            Some(PathBuf::from("/Users/x/.nvm/versions/node/v23.7.0/bin"))
        );
    }

    #[test]
    fn node_bin_dir_is_none_for_a_bare_command() {
        // A bare `node` (already on PATH) has no directory to inject.
        assert_eq!(node_bin_dir("node"), None);
    }
}
