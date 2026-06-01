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

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::lsp::log_file;

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
        if !crate::lsp::manager::is_on_path("npm") {
            log_file::log("lsp[vtsls] cannot auto-install: `npm` not on PATH");
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&prefix) {
            log_file::log(&format!("lsp[vtsls] could not create {prefix:?}: {e}"));
            return;
        }
        let spec = install_spec();
        log_file::log(&format!("lsp[vtsls] installing {spec} into {prefix:?}"));
        match Command::new("npm")
            .arg("install")
            .arg("--prefix")
            .arg(&prefix)
            .arg(&spec)
            .output()
        {
            Ok(out) if out.status.success() => {
                log_file::log("lsp[vtsls] managed install complete");
            }
            Ok(out) => {
                log_file::log(&format!(
                    "lsp[vtsls] install failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Err(e) => log_file::log(&format!("lsp[vtsls] install error: {e}")),
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
