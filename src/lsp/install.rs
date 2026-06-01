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

/// Set when a managed install finishes successfully. The app consumes this to
/// re-open its TypeScript documents to the LSP, which makes the manager
/// re-probe and spawn the freshly-installed server without waiting for the
/// user's next action ("installed" should mean "now working").
static JUST_INSTALLED: AtomicBool = AtomicBool::new(false);

/// One-shot: true exactly once after a successful managed install.
pub fn take_just_installed() -> bool {
    JUST_INSTALLED.swap(false, Ordering::SeqCst)
}

/// A directory to PREPEND to PATH so a non-shell `exec` can find `node`/`npm`,
/// or `None` when `node` is already on croft's PATH (nothing to add) or none
/// could be discovered. Cached: discovery runs at most once.
///
/// Version managers (nvm, fnm, asdf, volta) keep node off croft's inherited
/// PATH (they add it only when a shell sources its init files). croft is exec'd
/// without a shell, so [`discover_node_dir`] finds it — first by reading the
/// well-known on-disk layouts, then by asking the user's shell in a detached
/// session that can't touch croft's terminal.
pub(crate) fn node_path_prepend() -> Option<PathBuf> {
    static NODE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    NODE_DIR
        .get_or_init(|| {
            if crate::lsp::manager::is_on_path("node") && crate::lsp::manager::is_on_path("npm") {
                return None;
            }
            discover_node_dir()
        })
        .clone()
}

/// True when croft can find a usable `node` + `npm`, either already on PATH or
/// via the discovered version-manager directory.
fn node_available() -> bool {
    (crate::lsp::manager::is_on_path("node") && crate::lsp::manager::is_on_path("npm"))
        || node_path_prepend().is_some()
}

/// Find a directory containing real `node` + `npm` binaries.
///
/// Fast path first: read the well-known on-disk layouts (nvm versions dir,
/// Volta, Homebrew) with no process spawn. Universal fallback: ask the user's
/// own login+interactive shell where `node` resolves — detached from croft's
/// controlling terminal so it can never do tty job control. The fallback is
/// what makes fnm / asdf / volta-shim / custom setups work, not just the ones
/// with a guessable directory.
fn discover_node_dir() -> Option<PathBuf> {
    if let Some(dir) = nvm_node_dir() {
        return Some(dir);
    }
    if let Some(dir) = well_known_node_dirs()
        .into_iter()
        .find(|dir| dir.join("node").is_file() && dir.join("npm").exists())
    {
        return Some(dir);
    }
    detached_shell_node_dir()
}

/// Marker the probe wraps `process.execPath` in, so the real path is
/// recoverable even if the user's shell init prints noise to stdout first.
const NODE_PROBE_MARKER: &str = "__CROFT_NODE__";

/// Ask the user's login+interactive shell where `node` lives, detached from
/// croft's controlling terminal. Runs `node -e` (so it works whether `node` is
/// a real binary or a version-manager shell function) and reports the absolute
/// `process.execPath`. Returns its directory.
///
/// SAFETY / why detached: an interactive shell does terminal job control
/// (`tcsetpgrp`) on its controlling tty. Sharing croft's tty would background
/// croft and corrupt the terminal. `setsid` in `pre_exec` puts the child in a
/// brand-new session with NO controlling terminal, so it physically cannot
/// touch croft's tty; stdin is `/dev/null` and stdout is captured. A timeout
/// guards against a pathological init file that never returns.
fn detached_shell_node_dir() -> Option<PathBuf> {
    use std::sync::mpsc;
    use std::time::Duration;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(run_detached_node_probe());
    });
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(result) => result,
        Err(_) => {
            log_file::log("lsp[vtsls] node shell-probe timed out");
            None
        }
    }
}

fn run_detached_node_probe() -> Option<PathBuf> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));
    let script =
        format!("node -e 'process.stdout.write(\"{NODE_PROBE_MARKER}\"+process.execPath)'");
    let mut cmd = Command::new(shell);
    cmd.args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Detach from croft's controlling terminal before exec so the interactive
    // shell's job control can never reach croft's tty.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let out = cmd.output().ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let path = parse_node_probe(&stdout)?;
    let node = Path::new(path);
    if !node.is_file() {
        return None;
    }
    node.parent().map(Path::to_path_buf)
}

/// Recover the node path from probe stdout: the absolute path written right
/// after the last marker. Returns `None` if the marker is absent (the probe
/// didn't run node) or the text after it isn't an absolute path. Tolerates
/// arbitrary shell-init noise printed before the marker.
fn parse_node_probe(stdout: &str) -> Option<&str> {
    let after = &stdout[stdout.rfind(NODE_PROBE_MARKER)? + NODE_PROBE_MARKER.len()..];
    let path = after.trim();
    (path.starts_with('/') && !path.is_empty()).then_some(path)
}

/// Highest installed nvm node's `bin` dir: `$NVM_DIR/versions/node/vX.Y.Z/bin`
/// (defaulting `NVM_DIR` to `~/.nvm`). Picks the greatest semver so we get a
/// recent, working node; we only need *a* functional node to install/run vtsls,
/// not the user's exact `default` alias.
fn nvm_node_dir() -> Option<PathBuf> {
    let nvm = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".nvm")))?;
    nvm_node_dir_in(&nvm.join("versions").join("node"))
}

/// Pure core of [`nvm_node_dir`], split out so it's testable against a temp
/// directory without reading `$NVM_DIR`.
fn nvm_node_dir_in(versions: &Path) -> Option<PathBuf> {
    let mut best: Option<((u64, u64, u64), PathBuf)> = None;
    for entry in std::fs::read_dir(versions).ok()?.flatten() {
        let bin = entry.path().join("bin");
        if !bin.join("node").is_file() {
            continue;
        }
        let version = parse_node_version(&entry.file_name().to_string_lossy());
        if best.as_ref().is_none_or(|(b, _)| version > *b) {
            best = Some((version, bin));
        }
    }
    best.map(|(_, bin)| bin)
}

/// Parse an nvm version dir name like `v23.7.0` into a comparable tuple;
/// unparseable components become 0.
fn parse_node_version(name: &str) -> (u64, u64, u64) {
    let mut parts = name
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// Static install locations for other managers, checked after nvm.
fn well_known_node_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(&home).join(".volta").join("bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
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
                JUST_INSTALLED.store(true, Ordering::SeqCst);
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
    fn parse_node_version_orders_by_semver_not_lexically() {
        assert_eq!(parse_node_version("v23.7.0"), (23, 7, 0));
        assert_eq!(parse_node_version("v8.17.1"), (8, 17, 1));
        // The bug a lexical sort would hit: v8 must NOT outrank v18.
        assert!(parse_node_version("v18.0.0") > parse_node_version("v8.17.1"));
        assert_eq!(parse_node_version("garbage"), (0, 0, 0));
    }

    #[test]
    fn nvm_node_dir_picks_highest_version_with_a_node_binary() {
        let tmp = TempDir::new().unwrap();
        let versions = tmp.path();
        // Two versions with node, one stray dir without — highest wins.
        for v in ["v18.19.0", "v23.7.0"] {
            let bin = versions.join(v).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("node"), b"#!/bin/sh\n").unwrap();
        }
        std::fs::create_dir_all(versions.join("v20.0.0")).unwrap(); // no bin/node
        let dir = nvm_node_dir_in(versions).unwrap();
        assert_eq!(dir, versions.join("v23.7.0").join("bin"));
    }

    #[test]
    fn nvm_node_dir_is_none_when_no_versions_have_node() {
        let tmp = TempDir::new().unwrap();
        assert!(nvm_node_dir_in(tmp.path()).is_none());
    }

    #[test]
    fn parse_node_probe_extracts_path_after_marker_ignoring_init_noise() {
        // Shell init may print noise before the marker; the path is what
        // follows the last marker.
        let stdout = "Welcome to your shell!\nnvm loaded\n__CROFT_NODE__/Users/x/.nvm/versions/node/v23.7.0/bin/node";
        assert_eq!(
            parse_node_probe(stdout),
            Some("/Users/x/.nvm/versions/node/v23.7.0/bin/node")
        );
    }

    #[test]
    fn parse_node_probe_rejects_missing_marker_or_non_absolute() {
        assert_eq!(parse_node_probe("no marker here"), None);
        assert_eq!(parse_node_probe("__CROFT_NODE__"), None);
        assert_eq!(parse_node_probe("__CROFT_NODE__relative/path"), None);
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
