//! croft-managed language-server provisioning.
//!
//! When a language server isn't already on the user's PATH, croft installs and
//! owns its own copy under `~/.croft/servers/` rather than touching the user's
//! global package managers. This sidesteps `npm -g` permission failures, pins a
//! known-good version where it matters, and means "open a file → get LSP" works
//! on a fresh remote box with nothing pre-installed (the local/remote parity
//! rule).
//!
//! Provisioning is keyed by ecosystem backend, not special-cased per server:
//!   - [`Provision::Npm`] — TypeScript's `vtsls`, installed with `npm install
//!     --prefix` and run via a discovered `node`.
//!   - [`Provision::Uv`] — Python's `ty` / `ruff`, installed with `uv tool
//!     install` into a croft-owned tool dir. uv resolves the platform, Python
//!     interpreter and version itself, so there is no tarball to unpack.
//!
//! Every install is lazy and best-effort: it fires the first time a file whose
//! server is missing is opened, runs on a detached thread so launch is never
//! blocked, and a failure just leaves that language's LSP unavailable (with a
//! status-bar note pointing at the cause).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::lsp::config::{Language, ServerConfig};
use crate::lsp::log_file;

/// How croft obtains a server it manages itself. Each variant maps to one
/// ecosystem installer. `version` pins an exact release for reproducibility;
/// `None` tracks latest (what the fast-moving Astral servers want, since they
/// are backward-compatible LSP daemons and a stale pin would just fail to
/// install).
#[derive(Debug, Clone, PartialEq)]
pub enum Provision {
    /// An npm package installed under `~/.croft/servers/<name>` and invoked via
    /// the discovered `node`. `bin` is the executable npm drops in
    /// `node_modules/.bin`.
    Npm {
        package: &'static str,
        version: Option<&'static str>,
        bin: &'static str,
    },
    /// A PyPI tool installed with `uv tool install` into croft's own uv tool
    /// dir (`~/.croft/servers/uv`). `bin` is the entry-point uv writes into
    /// `UV_TOOL_BIN_DIR`.
    Uv {
        package: &'static str,
        version: Option<&'static str>,
        bin: &'static str,
    },
}

/// Latest one-line status of a managed install, polled by the app each tick and
/// surfaced in the status bar so the background work isn't invisible. A single
/// slot: when several servers install at once the most recent message wins,
/// which is all the status bar can show anyway.
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

/// Languages whose managed server finished installing since the last call. The
/// app re-opens that language's open documents so the manager re-probes and
/// spawns the freshly-installed server — "installed" should mean "now working"
/// without waiting for the user's next action.
static JUST_INSTALLED: Mutex<Vec<Language>> = Mutex::new(Vec::new());

fn mark_installed(language: Language) {
    if let Ok(mut g) = JUST_INSTALLED.lock()
        && !g.contains(&language)
    {
        g.push(language);
    }
}

/// Drain the set of languages whose server just became available.
pub fn take_just_installed() -> Vec<Language> {
    JUST_INSTALLED
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default()
}

/// Ensures only one install thread is ever spawned per process per server, no
/// matter how many times the manager re-probes a missing client.
static INSTALL_STARTED: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());

/// The registry name for the TypeScript server, shared so config and the
/// manager's resolver agree on which server croft provisions itself.
pub const VTSLS_SERVER_NAME: &str = "vtsls";

// ---------------------------------------------------------------------------
// Resolution: turn a `Provision` into a spawnable command, kicking off a lazy
// background install when the server isn't there yet.
// ---------------------------------------------------------------------------

/// Resolve a managed server to a spawnable `(config, extra_PATH)` pair, or
/// `None` (after starting a background install) when it isn't installed yet.
/// The manager calls this for any `ServerConfig` carrying a `provision`.
pub fn resolve_managed(
    config: &ServerConfig,
    provision: &Provision,
    log_skip: bool,
) -> Option<(ServerConfig, Vec<PathBuf>)> {
    match provision {
        Provision::Npm { bin, .. } => {
            if let Some(command) = npm_command(config.name, bin) {
                let mut resolved = config.clone();
                resolved.command = command;
                // vtsls is a Node script; its `env node` shebang needs node on
                // the spawned process's PATH, which (for version managers) means
                // the discovered node dir, not croft's inherited PATH.
                let extra: Vec<PathBuf> = node_path_prepend().into_iter().collect();
                return Some((resolved, extra));
            }
        }
        Provision::Uv { bin, .. } => {
            if let Some(command) = uv_command(bin) {
                let mut resolved = config.clone();
                resolved.command = command;
                // uv writes a self-contained launcher (absolute shebang to the
                // tool's own venv python), so no extra PATH entry is needed.
                return Some((resolved, Vec::new()));
            }
        }
    }
    // Not installed yet: start the one-shot managed install. This open skips the
    // server; a later request re-probes once the install lands.
    ensure_in_background(config, provision);
    if log_skip {
        log_file::log(&format!(
            "lsp[{}] not installed; starting croft-managed install",
            config.name
        ));
    }
    None
}

/// Resolve an invocable command for an npm-provisioned server: prefer croft's
/// managed copy (absolute path, no PATH dependency), then fall back to a binary
/// already on PATH so a user who installed one globally still works. `None`
/// when neither exists. Managed-first because vtsls is an internal dependency
/// croft pins, not a tool the user drives directly.
fn npm_command(name: &str, bin: &str) -> Option<String> {
    if let Some(p) = managed_npm_binary(name, bin)
        && p.is_file()
    {
        return Some(p.to_string_lossy().into_owned());
    }
    if crate::lsp::manager::is_on_path(bin) {
        return Some(bin.to_string());
    }
    None
}

/// Resolve an invocable command for a uv-provisioned server. Unlike vtsls,
/// `ty`/`ruff` are user-facing tools the user may want to control the version
/// of, so a copy already on PATH wins over croft's managed one; the managed
/// copy is the fallback for a box that has neither.
fn uv_command(bin: &str) -> Option<String> {
    if crate::lsp::manager::is_on_path(bin) {
        return Some(bin.to_string());
    }
    if let Some(p) = managed_uv_binary(bin)
        && p.is_file()
    {
        return Some(p.to_string_lossy().into_owned());
    }
    None
}

// ---------------------------------------------------------------------------
// Managed install locations.
// ---------------------------------------------------------------------------

/// croft's managed server store: `~/.croft/servers`. `None` when `$HOME` is
/// unset (the same guard the rest of croft uses for `~/.croft`).
fn servers_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".croft").join("servers"))
}

/// Directory `npm install --prefix` targets for a given server.
fn npm_prefix(name: &str) -> Option<PathBuf> {
    Some(servers_dir()?.join(name))
}

/// Path to an npm `bin` under a prefix. `npm install --prefix DIR PKG` drops
/// executables in `DIR/node_modules/.bin`. Pure, so it's testable without
/// touching `$HOME`.
fn npm_bin_in(prefix: &Path, bin: &str) -> PathBuf {
    prefix.join("node_modules").join(".bin").join(bin)
}

/// Absolute path to a managed npm-installed binary, whether or not it exists.
fn managed_npm_binary(name: &str, bin: &str) -> Option<PathBuf> {
    Some(npm_bin_in(&npm_prefix(name)?, bin))
}

/// croft's uv tool dir (`uv` installs each tool's venv here).
fn uv_tool_dir() -> Option<PathBuf> {
    Some(servers_dir()?.join("uv").join("tools"))
}

/// croft's uv bin dir (`uv` writes entry-point launchers here).
fn uv_bin_dir() -> Option<PathBuf> {
    Some(servers_dir()?.join("uv").join("bin"))
}

/// Absolute path to a managed uv-installed binary, whether or not it exists.
fn managed_uv_binary(bin: &str) -> Option<PathBuf> {
    Some(uv_bin_dir()?.join(bin))
}

/// Build the package spec npm installs, pinning the version when given.
fn npm_spec(package: &str, version: Option<&str>) -> String {
    match version {
        Some(v) => format!("{package}@{v}"),
        None => package.to_string(),
    }
}

/// Build the package spec uv installs, pinning the version when given.
fn uv_spec(package: &str, version: Option<&str>) -> String {
    match version {
        Some(v) => format!("{package}=={v}"),
        None => package.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Background installers.
// ---------------------------------------------------------------------------

/// Install `config`'s server into the managed store on a detached thread, if no
/// thread for it is already running this process. Idempotent per server,
/// best-effort, and non-blocking.
pub fn ensure_in_background(config: &ServerConfig, provision: &Provision) {
    {
        let mut started = match INSTALL_STARTED.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if !started.insert(config.name) {
            return; // an install thread for this server is already running
        }
    }
    let name = config.name;
    let language = config.language;
    let provision = provision.clone();
    std::thread::spawn(move || match &provision {
        Provision::Npm {
            package, version, ..
        } => run_npm_install(name, language, package, *version),
        Provision::Uv {
            package, version, ..
        } => run_uv_install(name, language, package, *version),
    });
}

/// `npm install --prefix ~/.croft/servers/<name> <pkg>`. Requires `npm` on PATH
/// (or a discovered version-manager dir); `node` must also be present for the
/// installed server to actually run.
fn run_npm_install(name: &'static str, language: Language, package: &str, version: Option<&str>) {
    let Some(prefix) = npm_prefix(name) else {
        return;
    };
    if !node_available() {
        log_file::log(&format!(
            "lsp[{name}] cannot auto-install: no `node`/`npm` found"
        ));
        set_status(format!(
            "{name} unavailable: install Node.js (node/npm not found)"
        ));
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&prefix) {
        log_file::log(&format!("lsp[{name}] could not create {prefix:?}: {e}"));
        set_status(format!(
            "{name} install failed (could not create install dir)"
        ));
        return;
    }
    let extra = node_path_prepend();
    let npm: PathBuf = match &extra {
        Some(dir) => dir.join("npm"),
        None => PathBuf::from("npm"),
    };
    let spec = npm_spec(package, version);
    log_file::log(&format!("lsp[{name}] installing {spec} into {prefix:?}"));
    set_status(format!("Installing {name}…"));
    let output = Command::new(&npm)
        .arg("install")
        .arg("--prefix")
        .arg(&prefix)
        .arg(&spec)
        .env("PATH", augmented_path(extra.as_deref()))
        .output();
    finish_install(name, language, output);
}

/// `uv tool install <pkg>` into croft's own uv tool dir. Requires `uv` on the
/// system; uv pulls a suitable Python interpreter itself, so nothing else is
/// needed on the box.
fn run_uv_install(name: &'static str, language: Language, package: &str, version: Option<&str>) {
    let Some(uv) = ensure_uv() else {
        log_file::log(&format!(
            "lsp[{name}] cannot auto-install: `uv` unavailable and could not be bootstrapped"
        ));
        set_status(format!(
            "{name} unavailable: could not install uv automatically (see ~/.croft/lsp.log)"
        ));
        return;
    };
    let (Some(tool_dir), Some(bin_dir)) = (uv_tool_dir(), uv_bin_dir()) else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&bin_dir) {
        log_file::log(&format!("lsp[{name}] could not create {bin_dir:?}: {e}"));
        set_status(format!(
            "{name} install failed (could not create install dir)"
        ));
        return;
    }
    let spec = uv_spec(package, version);
    log_file::log(&format!(
        "lsp[{name}] installing {spec} via uv into {tool_dir:?}"
    ));
    set_status(format!("Installing {name} (uv)…"));
    // `--force` so a half-finished previous attempt is overwritten cleanly.
    let output = Command::new(&uv)
        .arg("tool")
        .arg("install")
        .arg("--force")
        .arg(&spec)
        .env("UV_TOOL_DIR", &tool_dir)
        .env("UV_TOOL_BIN_DIR", &bin_dir)
        .output();
    finish_install(name, language, output);
}

/// Shared completion handling for both backends: log + status + mark the
/// language installed on success, log + status the failure otherwise.
fn finish_install(
    name: &'static str,
    language: Language,
    output: std::io::Result<std::process::Output>,
) {
    match output {
        Ok(out) if out.status.success() => {
            log_file::log(&format!("lsp[{name}] managed install complete"));
            mark_installed(language);
            set_status(format!("{name} installed"));
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            log_file::log(&format!("lsp[{name}] install failed: {}", err.trim()));
            set_status(format!("{name} install failed (see ~/.croft/lsp.log)"));
        }
        Err(e) => {
            log_file::log(&format!("lsp[{name}] install error: {e}"));
            set_status(format!("{name} install failed (see ~/.croft/lsp.log)"));
        }
    }
}

/// Where croft bootstraps its own `uv` when the box has none: the official
/// installer is pointed here via `UV_INSTALL_DIR` so the binary is croft-owned
/// and cleanly removable, rather than scattered in the user's `~/.local/bin`.
fn uv_dist_dir() -> Option<PathBuf> {
    Some(servers_dir()?.join("uv-dist"))
}

/// Locate the `uv` executable: PATH first, then croft's own bootstrapped copy,
/// then the dirs uv's official installer drops it in (`~/.local/bin`,
/// `~/.cargo/bin`) plus the common Homebrew/local prefixes — a non-login `exec`
/// won't have those on PATH. `None` when uv isn't installed anywhere croft
/// looks.
fn uv_program() -> Option<PathBuf> {
    if crate::lsp::manager::is_on_path("uv") {
        return Some(PathBuf::from("uv"));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dist) = uv_dist_dir() {
        candidates.push(dist.join("uv"));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join(".local").join("bin").join("uv"));
        candidates.push(home.join(".cargo").join("bin").join("uv"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/uv"));
    candidates.push(PathBuf::from("/usr/local/bin/uv"));
    candidates.into_iter().find(|p| p.is_file())
}

/// Return a usable `uv`, bootstrapping it via the official installer if the box
/// has none. Serialized by a process-global lock so `ty` and `ruff` opening at
/// once don't both kick off a uv install — the second waits, then finds the uv
/// the first just laid down.
fn ensure_uv() -> Option<PathBuf> {
    if let Some(uv) = uv_program() {
        return Some(uv);
    }
    static UV_BOOTSTRAP: Mutex<()> = Mutex::new(());
    let _guard = UV_BOOTSTRAP.lock().ok()?;
    // Re-probe under the lock: another server's install thread may have just
    // bootstrapped uv while we waited.
    if let Some(uv) = uv_program() {
        return Some(uv);
    }
    bootstrap_uv()
}

/// Pick the fetcher for the uv install script: `curl` preferred, `wget` as a
/// fallback (the official one-liner ships both forms). `None` when neither is
/// available — uv can't be fetched without an HTTP client on the box.
fn uv_fetch_command(has_curl: bool, has_wget: bool) -> Option<&'static str> {
    if has_curl {
        Some("curl -LsSf https://astral.sh/uv/install.sh")
    } else if has_wget {
        Some("wget -qO- https://astral.sh/uv/install.sh")
    } else {
        None
    }
}

/// Install uv with Astral's official script, pointed at croft's own dir and
/// told not to touch the user's shell rc files. Best-effort; returns the uv
/// path on success.
fn bootstrap_uv() -> Option<PathBuf> {
    let dir = uv_dist_dir()?;
    let Some(fetch) = uv_fetch_command(
        crate::lsp::manager::is_on_path("curl"),
        crate::lsp::manager::is_on_path("wget"),
    ) else {
        log_file::log("lsp[uv] cannot bootstrap: neither `curl` nor `wget` found");
        set_status("Python LSP needs uv: install uv or curl/wget (see ~/.croft/lsp.log)");
        return None;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log_file::log(&format!("lsp[uv] could not create {dir:?}: {e}"));
        set_status("uv install failed (could not create install dir)");
        return None;
    }
    let script = format!("{fetch} | sh");
    log_file::log(&format!(
        "lsp[uv] bootstrapping uv via official installer into {dir:?}"
    ));
    set_status("Installing uv…");
    // `UV_INSTALL_DIR` puts uv in croft's dir; `INSTALLER_NO_MODIFY_PATH` keeps
    // the script from editing the user's shell profiles. If the env vars are
    // ignored by some installer version, uv lands in `~/.local/bin`, which
    // `uv_program` also searches, so discovery still succeeds.
    match Command::new("sh")
        .arg("-c")
        .arg(&script)
        .env("UV_INSTALL_DIR", &dir)
        .env("INSTALLER_NO_MODIFY_PATH", "1")
        .output()
    {
        Ok(out) if out.status.success() => {
            log_file::log("lsp[uv] bootstrap complete");
            uv_program()
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            log_file::log(&format!("lsp[uv] bootstrap failed: {}", err.trim()));
            set_status("uv install failed (see ~/.croft/lsp.log)");
            None
        }
        Err(e) => {
            log_file::log(&format!("lsp[uv] bootstrap error: {e}"));
            set_status("uv install failed (see ~/.croft/lsp.log)");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// node discovery (needed by the npm backend and by spawning vtsls).
// ---------------------------------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn npm_binary_lands_in_npm_bin_dir() {
        let prefix = std::path::Path::new("/tmp/croft/servers/vtsls");
        assert_eq!(
            npm_bin_in(prefix, "vtsls"),
            std::path::Path::new("/tmp/croft/servers/vtsls/node_modules/.bin/vtsls"),
            "npm install --prefix drops executables under <prefix>/node_modules/.bin"
        );
    }

    #[test]
    fn npm_spec_pins_the_version_when_given() {
        assert_eq!(
            npm_spec("@vtsls/language-server", Some("0.3.0")),
            "@vtsls/language-server@0.3.0"
        );
        assert_eq!(
            npm_spec("@vtsls/language-server", None),
            "@vtsls/language-server",
            "no pin installs latest"
        );
    }

    #[test]
    fn uv_spec_uses_double_equals_for_pinning() {
        // PyPI version pinning is `pkg==X`, not npm's `pkg@X`.
        assert_eq!(uv_spec("ruff", Some("0.9.0")), "ruff==0.9.0");
        assert_eq!(uv_spec("ty", None), "ty", "no pin tracks latest");
    }

    #[test]
    fn uv_fetch_command_prefers_curl_then_wget_then_gives_up() {
        assert_eq!(
            uv_fetch_command(true, true),
            Some("curl -LsSf https://astral.sh/uv/install.sh"),
            "curl wins when both are present"
        );
        assert_eq!(
            uv_fetch_command(false, true),
            Some("wget -qO- https://astral.sh/uv/install.sh"),
            "wget is the fallback"
        );
        assert_eq!(
            uv_fetch_command(false, false),
            None,
            "no fetcher → can't bootstrap uv"
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
    fn npm_binary_is_detected_only_once_the_bin_dir_is_populated() {
        // Before install, the bin path doesn't exist; after a file appears
        // there, `is_file()` (what `npm_command` keys off) flips to true.
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("vtsls");
        let bin = npm_bin_in(&prefix, "vtsls");
        assert!(!bin.is_file());
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"#!/usr/bin/env node\n").unwrap();
        assert!(bin.is_file());
    }
}
