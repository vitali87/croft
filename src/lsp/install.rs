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
    /// A prebuilt binary downloaded from a release URL and unpacked under
    /// `~/.croft/servers/<name>/`. Host-agnostic: each `targets` entry is a full
    /// literal URL (GitHub, Codeberg, GitLab, or any host), so neither the host
    /// nor the project's asset-naming scheme is baked into croft. This is how
    /// VS Code, Zed and Mason provision servers that ship as native binaries
    /// (clangd, taplo) rather than npm/PyPI packages.
    Binary {
        /// Supported platforms mapped to their literal download URL. Keys are
        /// `"<os>-<arch>"` (e.g. `linux-x86_64`) or a bare `"<os>"` (e.g.
        /// `macos` for a universal build that serves every arch), using Rust's
        /// `target_os`/`target_arch` tokens. Resolution tries the specific
        /// `os-arch` key first, then the bare `os`. A platform absent from the
        /// map is unsupported for managed install and falls back to PATH — this
        /// is how an irregular matrix (clangd: universal mac, x86_64-only linux,
        /// no linux-aarch64) is expressed exactly.
        targets: &'static [(&'static str, &'static str)],
        /// Executable name croft invokes after unpacking (also the PATH-probe
        /// name for a user-installed copy).
        bin: &'static str,
        /// Archive format of the downloaded asset.
        archive: ArchiveKind,
        /// Literal path to the executable inside the unpacked archive, relative
        /// to `~/.croft/servers/<name>/`. `None` for a single-file `.gz` whose
        /// decompressed bytes ARE the binary (placed at `<name>/<bin>`). Set for
        /// `.zip` payloads that carry sibling files the binary needs at a fixed
        /// relative path (e.g. clangd's `clangd_<ver>/bin/clangd` beside `lib/`).
        bin_path: Option<&'static str>,
    },
}

/// Archive format of a [`Provision::Binary`] download.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArchiveKind {
    /// A single gzip-compressed executable; decompressed bytes are the binary.
    Gz,
    /// A zip holding the binary plus any sibling files it needs; extracted whole.
    Zip,
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
        Provision::Binary { bin, bin_path, .. } => {
            if let Some(command) = binary_command(config.name, bin, *bin_path) {
                let mut resolved = config.clone();
                resolved.command = command;
                // A self-contained native binary launched by absolute path; the
                // archive carries any sibling files it needs, so no extra PATH.
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

// ---------------------------------------------------------------------------
// Binary backend: prebuilt release binaries (clangd, taplo). Host-agnostic —
// the manifest carries a full URL template plus per-project OS/arch tokens, so
// a server hosted on GitHub, Codeberg, GitLab or anywhere else is just data.
// ---------------------------------------------------------------------------

/// Generous ceiling on a release-binary download (clangd's zip is ~50 MB); a
/// runaway or redirected body is refused past this.
const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;

/// The download URL for the running platform, or `None` if unsupported. Tries
/// the specific `"<os>-<arch>"` key first (e.g. `linux-x86_64`), then a bare
/// `"<os>"` key (e.g. `macos` for a universal build). Pure and testable.
fn target_url<'a>(targets: &'a [(&'a str, &'a str)], os: &str, arch: &str) -> Option<&'a str> {
    let specific = format!("{os}-{arch}");
    targets
        .iter()
        .find(|(k, _)| *k == specific)
        .or_else(|| targets.iter().find(|(k, _)| *k == os))
        .map(|(_, url)| *url)
}

/// The executable inside a managed binary install. `bin_path` locates a binary
/// nested in an extracted archive; absent, the binary sits directly at
/// `<name>/<bin>`.
fn managed_binary_path(name: &str, bin: &str, bin_path: Option<&str>) -> Option<PathBuf> {
    Some(servers_dir()?.join(name).join(bin_path.unwrap_or(bin)))
}

/// Resolve an invocable command for a binary-provisioned server. PATH-first
/// (mirrors Zed: a user's own clangd/taplo, matching their toolchain, wins),
/// then croft's managed copy. `None` when neither exists yet.
fn binary_command(name: &str, bin: &str, bin_path: Option<&str>) -> Option<String> {
    if crate::lsp::manager::is_on_path(bin) {
        return Some(bin.to_string());
    }
    if let Some(p) = managed_binary_path(name, bin, bin_path)
        && p.is_file()
    {
        return Some(p.to_string_lossy().into_owned());
    }
    None
}

/// Mark a downloaded file executable (no-op on non-unix; croft's targets are
/// macOS/Linux/Android, all unix).
#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms)
}
#[cfg(not(unix))]
fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Stream `url` into memory, capped at `max`. Follows redirects (release URLs
/// 302 to a CDN). `None` on any transport error.
fn download_capped(url: &str, max: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let resp = ureq::get(url).call().ok()?;
    let mut bytes = Vec::new();
    resp.into_reader().take(max).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// Gunzip a single-file `.gz` payload (the decompressed bytes ARE the binary)
/// to `target` and mark it executable.
fn extract_gz(bytes: &[u8], target: &Path) -> std::io::Result<()> {
    use std::io::Read;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut decoder = flate2::read::GzDecoder::new(bytes);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    std::fs::write(target, &out)?;
    set_executable(target)
}

/// Extract a `.zip` payload whole into `dir` (so the binary keeps its sibling
/// files, e.g. clangd's `lib/`), then mark the resolved binary executable.
fn extract_zip(bytes: &[u8], dir: &Path, target: &Path) -> std::io::Result<()> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    archive.extract(dir).map_err(std::io::Error::other)?;
    if target.is_file() {
        set_executable(target)?;
    }
    Ok(())
}

/// Download + unpack a prebuilt release binary into `~/.croft/servers/<name>/`.
/// Resolves the platform tokens, builds the URL, fetches, extracts per archive
/// kind, and marks the language installed on success. A platform the project
/// ships no asset for is logged + surfaced and left to PATH (e.g. clangd has no
/// linux-aarch64 build).
fn run_binary_install(
    name: &'static str,
    language: Language,
    targets: &[(&str, &str)],
    bin: &str,
    archive: ArchiveKind,
    bin_path: Option<&str>,
) {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let Some(url) = target_url(targets, os, arch) else {
        log_file::log(&format!("lsp[{name}] no prebuilt binary for {os}-{arch}"));
        set_status(format!(
            "{name} unavailable: no prebuilt binary for this platform (install {bin} manually)"
        ));
        return;
    };

    let Some(dir) = servers_dir().map(|d| d.join(name)) else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log_file::log(&format!("lsp[{name}] could not create {dir:?}: {e}"));
        set_status(format!(
            "{name} install failed (could not create install dir)"
        ));
        return;
    }

    log_file::log(&format!("lsp[{name}] downloading {url}"));
    set_status(format!("Installing {name}…"));
    let Some(bytes) = download_capped(url, MAX_BINARY_BYTES) else {
        log_file::log(&format!("lsp[{name}] download failed: {url}"));
        set_status(format!(
            "{name} install failed (download error, see ~/.croft/lsp.log)"
        ));
        return;
    };

    let target = dir.join(bin_path.unwrap_or(bin));
    let extracted = match archive {
        ArchiveKind::Gz => extract_gz(&bytes, &target),
        ArchiveKind::Zip => extract_zip(&bytes, &dir, &target),
    };
    if let Err(e) = extracted {
        log_file::log(&format!("lsp[{name}] extract failed: {e}"));
        set_status(format!(
            "{name} install failed (extract error, see ~/.croft/lsp.log)"
        ));
        return;
    }
    if !target.is_file() {
        log_file::log(&format!(
            "lsp[{name}] expected binary missing after extract: {target:?}"
        ));
        set_status(format!("{name} install failed (unexpected archive layout)"));
        return;
    }
    log_file::log(&format!("lsp[{name}] managed binary install complete"));
    mark_installed(language);
    set_status(format!("{name} installed"));
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
        Provision::Binary {
            targets,
            bin,
            archive,
            bin_path,
        } => run_binary_install(name, language, targets, bin, *archive, *bin_path),
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

/// `pkg install` argument vector for a uv-provisioned server on Termux. The
/// uv chain is structurally impossible there: stock Termux has no curl/wget
/// to bootstrap uv, Astral ships no aarch64-linux-android uv build, and the
/// ty/ruff PyPI wheels don't target Android. Termux's own repo packages both
/// servers under their PyPI names, and `pkg` installs into the
/// always-on-PATH `$PREFIX/bin`, which `uv_command`'s PATH-first probe then
/// resolves with no further plumbing. The repo carries one rolling version,
/// so any `Provision::Uv` pin is ignored on this path (ty/ruff track latest
/// anyway).
fn termux_pkg_args(package: &str) -> Vec<String> {
    vec![
        String::from("install"),
        String::from("-y"),
        String::from(package),
    ]
}

/// apt/dpkg take a single process-global lock, so two `pkg install` calls can
/// never run concurrently. Opening one Python file provisions both `ty` and
/// `ruff` on separate install threads; without this gate they race for the
/// lock and the loser dies with `Could not get lock …/apt/lists/lock`, leaving
/// its server uninstalled (observed for `ty` in lsp.log 2026-06-11). Every
/// Termux `pkg` invocation serializes through this mutex so croft never
/// contends with itself.
fn termux_pkg_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// How many extra times to retry a `pkg install` whose only problem was that
/// the apt lock was held — by an *external* apt the mutex can't serialize.
const PKG_LOCK_RETRIES: u32 = 5;

/// True when `pkg`'s only failure was apt's lock being held elsewhere, i.e. the
/// install should be retried rather than reported as a hard failure. A genuine
/// error (missing package, network) is not contention and falls through.
fn pkg_lock_contended(output: &std::io::Result<std::process::Output>) -> bool {
    match output {
        Ok(o) if !o.status.success() => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            stderr.contains("Could not get lock") || stderr.contains("Unable to lock")
        }
        _ => false,
    }
}

/// Install a uv-provisioned server from the Termux repo. Same lifecycle as
/// the other backends: logged, surfaced in the status bar, completion via
/// `finish_install` so the language's open documents re-probe on success.
/// Serialized against croft's other pkg installs and retried while the apt
/// lock is held, so concurrent `ty`/`ruff` provisioning can't lose the race.
fn run_termux_pkg_install(name: &'static str, language: Language, package: &str) {
    log_file::log(&format!(
        "lsp[{name}] installing {package} via pkg (Termux repo)"
    ));
    set_status(format!("Installing {name} (pkg)…"));
    // Recover the guard even if another install thread panicked mid-pkg: a
    // poisoned lock still serializes correctly, the data is just `()`.
    let _guard = termux_pkg_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut output = Command::new("pkg").args(termux_pkg_args(package)).output();
    for attempt in 1..=PKG_LOCK_RETRIES {
        if !pkg_lock_contended(&output) {
            break;
        }
        log_file::log(&format!(
            "lsp[{name}] apt lock held, retrying pkg install ({attempt}/{PKG_LOCK_RETRIES})"
        ));
        std::thread::sleep(std::time::Duration::from_secs(2));
        output = Command::new("pkg").args(termux_pkg_args(package)).output();
    }
    finish_install(name, language, output);
}

/// `uv tool install <pkg>` into croft's own uv tool dir. Requires `uv` on the
/// system; uv pulls a suitable Python interpreter itself, so nothing else is
/// needed on the box. On Termux the install reroutes to the native `pkg`
/// backend instead of the unreachable uv chain.
fn run_uv_install(name: &'static str, language: Language, package: &str, version: Option<&str>) {
    if crate::iterm2_inline::detect_termux() {
        run_termux_pkg_install(name, language, package);
        return;
    }
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
    fn termux_pkg_args_install_the_package_noninteractively() {
        assert_eq!(
            termux_pkg_args("ty"),
            ["install", "-y", "ty"],
            "background installs can never answer an apt prompt, so -y is required"
        );
        assert_eq!(termux_pkg_args("ruff"), ["install", "-y", "ruff"]);
    }

    #[test]
    fn apt_lock_contention_is_a_retryable_outcome_not_a_hard_failure() {
        // Opening one Python file provisions ty AND ruff on separate threads;
        // apt's single global lock means the loser sees this exact stderr.
        // It must be treated as "retry", not "give up" — otherwise ty stays
        // uninstalled forever (observed in lsp.log 2026-06-11).
        let locked = fake_output(
            false,
            "E: Could not get lock /data/.../apt/lists/lock. It is held by process 10355 (apt)\n\
             E: Unable to lock directory /data/.../apt/lists/\n",
        );
        assert!(
            pkg_lock_contended(&Ok(locked)),
            "apt lock-held stderr must be recognized as retryable contention"
        );

        // A clean success is not contention.
        assert!(!pkg_lock_contended(&Ok(fake_output(true, ""))));
        // A genuine failure (e.g. no such package) is not lock contention —
        // retrying it would just waste time, so it must fall through to fail.
        assert!(!pkg_lock_contended(&Ok(fake_output(
            false,
            "E: Unable to locate package nope\n"
        ))));
    }

    fn fake_output(success: bool, stderr: &str) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(if success { 0 } else { 256 }),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn uv_provisioned_python_servers_use_their_termux_repo_package_names() {
        // The Termux pkg backend reuses `Provision::Uv`'s PyPI package name
        // as the Termux package name. That only works because Termux packages
        // ty and ruff under exactly those names (verified in termux-packages
        // on 2026-06-10); this test pins the assumption on croft's side.
        for (config, expected) in [
            (crate::lsp::config::ServerConfig::ty(), "ty"),
            (crate::lsp::config::ServerConfig::ruff(), "ruff"),
        ] {
            match config.provision {
                Some(Provision::Uv { package, .. }) => assert_eq!(package, expected),
                other => panic!("expected Uv provision for {expected}, got {other:?}"),
            }
        }
    }

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
    fn target_url_prefers_os_arch_then_falls_back_to_bare_os() {
        // clangd's matrix: universal mac (a bare `macos` key serves any arch),
        // x86_64-only linux, and no linux-aarch64 build at all.
        let targets = [("macos", "MAC"), ("linux-x86_64", "LINUX64")];
        assert_eq!(target_url(&targets, "macos", "aarch64"), Some("MAC"));
        assert_eq!(target_url(&targets, "macos", "x86_64"), Some("MAC"));
        assert_eq!(target_url(&targets, "linux", "x86_64"), Some("LINUX64"));
        // Unsupported platform -> None, so the caller falls back to PATH.
        assert_eq!(target_url(&targets, "linux", "aarch64"), None);
        assert_eq!(target_url(&targets, "android", "aarch64"), None);
    }

    #[test]
    fn target_url_specific_os_arch_key_wins_over_bare_os() {
        let targets = [("macos", "GENERIC"), ("macos-aarch64", "ARM")];
        assert_eq!(target_url(&targets, "macos", "aarch64"), Some("ARM"));
        assert_eq!(target_url(&targets, "macos", "x86_64"), Some("GENERIC"));
    }

    /// End-to-end exercise of the real download + gunzip path against taplo's
    /// release. `#[ignore]`d because it hits the network and is platform-gated;
    /// run explicitly with `cargo test --bin croft -- --ignored taplo_gz`.
    /// Proves `download_capped` + `extract_gz` produce a runnable binary.
    #[test]
    #[ignore = "network: downloads a real taplo release"]
    fn taplo_gz_binary_downloads_extracts_and_runs() {
        let url = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => {
                "https://github.com/tamasfe/taplo/releases/download/0.10.0/taplo-darwin-aarch64.gz"
            }
            ("macos", "x86_64") => {
                "https://github.com/tamasfe/taplo/releases/download/0.10.0/taplo-darwin-x86_64.gz"
            }
            ("linux", "x86_64") => {
                "https://github.com/tamasfe/taplo/releases/download/0.10.0/taplo-linux-x86_64.gz"
            }
            other => panic!("no taplo asset wired for {other:?}"),
        };
        let bytes = download_capped(url, MAX_BINARY_BYTES).expect("download taplo");
        let dir = std::env::temp_dir().join(format!("croft-taplo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("taplo");
        extract_gz(&bytes, &target).expect("gunzip taplo");
        assert!(target.is_file(), "extracted taplo binary exists");
        let out = Command::new(&target)
            .arg("--version")
            .output()
            .expect("run taplo --version");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "taplo --version exits 0");
        let v = String::from_utf8_lossy(&out.stdout);
        assert!(v.contains("0.10.0"), "taplo reports its version: {v}");
    }

    /// End-to-end exercise of the real download + zip-extract path against
    /// clangd (the riskier archive format: whole-archive extract, nested
    /// `bin_path`, chmod). `#[ignore]`d: ~50 MB network download, platform-gated.
    /// Run with `cargo test --bin croft -- --ignored clangd_zip`.
    #[test]
    #[ignore = "network: downloads a real ~50MB clangd release"]
    fn clangd_zip_binary_downloads_extracts_and_runs() {
        let url = match std::env::consts::OS {
            "macos" => {
                "https://github.com/clangd/clangd/releases/download/22.1.0/clangd-mac-22.1.0.zip"
            }
            "linux" => {
                "https://github.com/clangd/clangd/releases/download/22.1.0/clangd-linux-22.1.0.zip"
            }
            other => panic!("no clangd asset wired for {other}"),
        };
        let bytes = download_capped(url, MAX_BINARY_BYTES).expect("download clangd");
        let dir = std::env::temp_dir().join(format!("croft-clangd-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("clangd_22.1.0/bin/clangd");
        extract_zip(&bytes, &dir, &target).expect("unzip clangd");
        assert!(
            target.is_file(),
            "clangd binary exists at the nested bin_path"
        );
        let out = Command::new(&target)
            .arg("--version")
            .output()
            .expect("run clangd --version");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "clangd --version exits 0");
        let v = String::from_utf8_lossy(&out.stdout);
        assert!(
            v.to_lowercase().contains("clangd"),
            "clangd reports itself: {v}"
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
