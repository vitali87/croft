//! croft-managed language-server provisioning.
//!
//! The TypeScript server (`vtsls`) is an internal dependency the user never
//! invokes directly, so croft installs and owns its own copy under
//! `~/.croft/servers/` rather than touching the user's global npm prefix. This
//! sidesteps `npm -g` permission failures, keeps the binary off the user's
//! PATH (croft invokes it by absolute path), and pins a known-good version so
//! every install behaves identically (the local/remote parity rule).
//!
//! The install is lazy and best-effort: it fires the first time a TypeScript
//! file is opened without a usable `vtsls`, runs on a detached thread so launch
//! is never blocked, and a failure just leaves TS LSP unavailable.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::lsp::log_file;

/// Latest one-line status of the managed install, polled by the app each tick
/// and surfaced in the status bar so the background work isn't invisible.
static STATUS: Mutex<Option<String>> = Mutex::new(None);

fn set_status(msg: impl Into<String>) {
    if let Ok(mut g) = STATUS.lock() {
        *g = Some(msg.into());
    }
}

/// Take the pending install status message, if any. The app calls this once
/// per tick; `Some` means "show this in the status bar and redraw".
pub fn take_status() -> Option<String> {
    STATUS.lock().ok().and_then(|mut g| g.take())
}

/// A directory to PREPEND to PATH so a non-shell `exec` can find `node`/`npm`,
/// or `None` when `node` is already on croft's PATH (nothing to add) or none
/// could be discovered. Cached: the login-shell probe runs at most once.
///
/// Version managers (nvm, fnm, asdf, volta) expose `node`/`npm` as shell
/// functions that only put the real binaries on PATH after a shell sources its
/// init files. croft is exec'd without a shell, so it asks the user's own
/// login+interactive shell where `node` resolves and reuses that directory.
pub(crate) fn node_path_prepend() -> Option<PathBuf> {
    static NODE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    NODE_DIR
        .get_or_init(|| {
            if crate::lsp::manager::is_on_path("node") && crate::lsp::manager::is_on_path("npm") {
                return None;
            }
            login_shell_node_dir()
        })
        .clone()
}

/// True when croft can find a usable `node` + `npm`, either already on PATH or
/// via the discovered version-manager directory.
fn node_available() -> bool {
    (crate::lsp::manager::is_on_path("node") && crate::lsp::manager::is_on_path("npm"))
        || node_path_prepend().is_some()
}

/// Ask the user's login+interactive shell where `node` lives and return its
/// directory. The interactive shell (`-i`) sources the init files where nvm /
/// fnm / asdf hooks are defined, so this resolves the same `node` the user
/// gets in their terminal.
///
/// It runs `node -e 'process.execPath'` rather than `command -v node`: version
/// managers expose `node` as a shell *function*, for which `command -v` prints
/// the name, not a path. Running node and asking for `process.execPath` returns
/// the real absolute binary path regardless of how `node` is exposed.
/// Best-effort: `None` on any failure.
fn login_shell_node_dir() -> Option<PathBuf> {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));
    let out = Command::new(shell)
        .args([
            "-l",
            "-i",
            "-c",
            "node -e 'process.stdout.write(process.execPath)'",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // `process.execPath` is an absolute path written without a trailing newline.
    // Scan from the end for the last absolute-looking line in case an init file
    // wrote noise to stdout first.
    let path = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('/') && Path::new(l).is_file())?;
    Path::new(path).parent().map(Path::to_path_buf)
}

/// Build a PATH value with `extra` prepended (so it wins over any stale entry),
/// or the current PATH unchanged when `extra` is `None`.
fn augmented_path(extra: Option<&Path>) -> OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let Some(dir) = extra else {
        return current;
    };
    let mut dirs = vec![dir.to_path_buf()];
    dirs.extend(std::env::split_paths(&current));
    std::env::join_paths(dirs).unwrap_or(current)
}

/// Build a PATH value with `dirs` prepended, for spawning a managed server
/// (e.g. so vtsls finds `node`). Returns the current PATH when `dirs` is empty.
pub(crate) fn prepend_paths(dirs: &[PathBuf]) -> OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    if dirs.is_empty() {
        return current;
    }
    let mut all: Vec<PathBuf> = dirs.to_vec();
    all.extend(std::env::split_paths(&current));
    std::env::join_paths(all).unwrap_or(current)
}

/// The registry name for the TypeScript server, shared so config and the
/// manager's resolver agree on which server croft provisions itself.
pub const VTSLS_SERVER_NAME: &str = "vtsls";

/// Pinned vtsls release. Bump deliberately so behaviour is reproducible across
/// machines rather than drifting with whatever a host happens to have.
const VTSLS_VERSION: &str = "0.3.0";

/// Ensures only one install thread is ever spawned per process, no matter how
/// many times the manager re-probes an empty TypeScript client list.
static INSTALL_STARTED: AtomicBool = AtomicBool::new(false);

/// croft's managed server store: `~/.croft/servers`. `None` when `$HOME` is
/// unset (the same guard the rest of croft uses for `~/.croft`).
fn servers_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".croft").join("servers"))
}

/// Directory `npm install --prefix` targets for vtsls.
fn vtsls_prefix() -> Option<PathBuf> {
    Some(servers_dir()?.join("vtsls"))
}

/// Path to the vtsls binary under a given prefix. `npm install --prefix DIR
/// PKG` drops executables in `DIR/node_modules/.bin`. Pure, so it's testable
/// without touching `$HOME` (mutating env is unsafe under the parallel suite).
fn vtsls_binary_in(prefix: &std::path::Path) -> PathBuf {
    prefix.join("node_modules").join(".bin").join("vtsls")
}

/// Absolute path to the managed vtsls binary, whether or not it exists yet.
fn managed_vtsls_binary() -> Option<PathBuf> {
    Some(vtsls_binary_in(&vtsls_prefix()?))
}

/// The npm package spec croft installs, pinned for reproducibility.
fn install_spec() -> String {
    format!("@vtsls/language-server@{VTSLS_VERSION}")
}

/// Resolve an invocable `vtsls` command: prefer croft's managed copy (absolute
/// path, no PATH dependency), then fall back to a `vtsls` already on PATH so a
/// user who installed one globally still works. `None` when neither exists.
pub fn vtsls_command() -> Option<String> {
    if let Some(bin) = managed_vtsls_binary()
        && bin.is_file()
    {
        return Some(bin.to_string_lossy().into_owned());
    }
    if crate::lsp::manager::is_on_path("vtsls") {
        return Some("vtsls".to_string());
    }
    None
}

/// Install vtsls into the managed dir on a detached thread if it isn't already
/// usable. Idempotent per process (guarded by `INSTALL_STARTED`), best-effort,
/// and non-blocking. Requires `npm` on PATH; `node` must also be present for
/// the installed server to actually run.
pub fn ensure_vtsls_in_background() {
    if INSTALL_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        if vtsls_command().is_some() {
            return;
        }
        let Some(prefix) = vtsls_prefix() else {
            return;
        };
        if !node_available() {
            log_file::log("lsp[vtsls] cannot auto-install: no `node`/`npm` found");
            set_status("TypeScript server unavailable: install Node.js (node/npm not found)");
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&prefix) {
            log_file::log(&format!("lsp[vtsls] could not create {prefix:?}: {e}"));
            set_status("TypeScript server install failed (could not create install dir)");
            return;
        }
        let extra = node_path_prepend();
        let npm: PathBuf = match &extra {
            Some(dir) => dir.join("npm"),
            None => PathBuf::from("npm"),
        };
        let spec = install_spec();
        log_file::log(&format!("lsp[vtsls] installing {spec} into {prefix:?}"));
        set_status("Installing TypeScript server (vtsls)…");
        match Command::new(&npm)
            .arg("install")
            .arg("--prefix")
            .arg(&prefix)
            .arg(&spec)
            .env("PATH", augmented_path(extra.as_deref()))
            .output()
        {
            Ok(out) if out.status.success() => {
                log_file::log("lsp[vtsls] managed install complete");
                set_status("TypeScript server installed");
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                log_file::log(&format!("lsp[vtsls] install failed: {}", err.trim()));
                set_status("TypeScript server install failed (see ~/.croft/lsp.log)");
            }
            Err(e) => {
                log_file::log(&format!("lsp[vtsls] install error: {e}"));
                set_status("TypeScript server install failed (see ~/.croft/lsp.log)");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn vtsls_binary_lands_in_npm_bin_dir() {
        let prefix = std::path::Path::new("/tmp/croft/servers/vtsls");
        assert_eq!(
            vtsls_binary_in(prefix),
            std::path::Path::new("/tmp/croft/servers/vtsls/node_modules/.bin/vtsls"),
            "npm install --prefix drops executables under <prefix>/node_modules/.bin"
        );
    }

    #[test]
    fn install_spec_pins_the_version() {
        assert_eq!(
            install_spec(),
            format!("@vtsls/language-server@{VTSLS_VERSION}")
        );
        assert!(
            install_spec().contains('@') && install_spec().ends_with(VTSLS_VERSION),
            "the spec must pin an exact version for reproducible installs, got {}",
            install_spec()
        );
    }

    #[test]
    fn prepend_paths_puts_the_extra_dir_first() {
        let dir = PathBuf::from("/opt/croft-test/node/bin");
        let result = prepend_paths(std::slice::from_ref(&dir));
        let first = std::env::split_paths(&result).next().unwrap();
        assert_eq!(
            first, dir,
            "the discovered node dir must win over croft's inherited PATH"
        );
    }

    #[test]
    fn prepend_paths_empty_leaves_path_unchanged() {
        assert_eq!(
            prepend_paths(&[]),
            std::env::var_os("PATH").unwrap_or_default()
        );
    }

    #[test]
    fn binary_is_detected_only_once_npm_has_populated_the_bin_dir() {
        // Before install, the bin path doesn't exist; after a file appears
        // there, `is_file()` (what `vtsls_command` keys off) flips to true.
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("vtsls");
        let bin = vtsls_binary_in(&prefix);
        assert!(!bin.is_file());
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"#!/usr/bin/env node\n").unwrap();
        assert!(bin.is_file());
    }
}
