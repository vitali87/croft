use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::hash::Hasher;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

mod clipboard;
mod control;
mod forward;
mod hosts;
mod install;
mod install_stream;
mod launch_command;
mod offer;
mod pull;
mod relay;
mod session;
pub(crate) use clipboard::MAX_COPY_PAYLOAD_BYTES;
use clipboard::*;
pub use control::AdoptedMaster;
pub use control::ssh_control_dir;
use control::*;
use forward::*;
pub use hosts::RemoteTarget;
pub use hosts::SshConfigState;
pub use hosts::discover_ssh_targets;
pub use hosts::primary_ssh_config_path;
pub use hosts::ssh_config_state;
use hosts::*;
pub(crate) use install::FORCE_SOURCE_BUILD;
pub use install::REMOTE_COMPILE_SIDE_EFFECTS;
pub use install::arch_to_musl_triple;
use install::*;
pub use install_stream::install_only_streaming;
use install_stream::*;
pub(crate) use launch_command::client_process_nonce;
use launch_command::*;
pub use offer::CwdReport;
#[cfg(test)]
pub use offer::REFUSED_HOST_TTL;
pub use offer::RefusedHosts;
pub use offer::forget_refused_host;
pub use offer::load_refused_hosts;
pub use offer::offer_allowed;
pub use offer::refused_hosts_path;
pub use offer::remember_refused_host;
pub use offer::remote_workspace_path;
pub use offer::ssh_pane_samples;
pub use offer::ssh_reroot_decision;
#[cfg(test)]
use offer::*;
use pull::*;
use relay::*;
pub use session::DROP_TO_LOCAL_EXIT_CODE;
pub use session::RemoteOutcome;
pub use session::launch_croft;
pub use session::launch_croft_with;
pub use session::launch_only;
pub use session::running_over_ssh;
#[cfg(test)]
use session::*;

/// Why a local static build for `triple` can't run, or `None` when it can.
/// Shared by the remote installer and dev containers (#617).
pub(crate) fn cross_build_unavailable(triple: &str) -> Option<String> {
    if let Some(reason) = cross_compile_unavailable_reason() {
        return Some(reason);
    }
    if !rust_target_installed(triple) {
        return Some(format!(
            "rustup target `{triple}` missing (run `rustup target add {triple}` once to enable the fast path)"
        ));
    }
    None
}

/// Cross-build a static croft for `triple` from this checkout and return the
/// binary's path.
pub(crate) fn cross_build_static(
    triple: &str,
    log_tx: &std::sync::mpsc::Sender<String>,
) -> Result<PathBuf> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // This build runs *concurrently* with the live remote session, whose
    // keystrokes are relayed by this same local machine. A default `-j N`
    // rustc build pins every core and starves the ssh relay + terminal
    // render, dropping the user's input. Run it niced and capped to half
    // the cores so the interactive session always preempts the compile -
    // the build is background, input latency is not. `nice` exists on both
    // macOS and Linux, so this stays identical on both launch targets.
    let jobs = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(1)
        .to_string();
    sync_workspace_lock(&source, |msg| {
        let _ = log_tx.send(msg);
    });
    // Belt and braces with the startup raise: the zig linker dies with
    // ProcessFdQuotaExceeded under macOS's default 256-fd soft limit, and
    // the resulting silent fallback compiles on the remote box instead.
    let fd_limit = raise_fd_limit();
    let _ = log_tx.send(format!(
        "Cross-compiling croft locally for {triple} (niced, {jobs} jobs, fd limit {fd_limit})"
    ));
    let zigbuild = cargo_zigbuild_command(&source, triple, &jobs);
    let status = run_command_streaming(zigbuild, log_tx).context("running cargo zigbuild")?;
    if !status.success() {
        anyhow::bail!("cargo zigbuild exited with {status}");
    }

    let binary = source
        .join("target")
        .join(triple)
        .join("remote-fast")
        .join("croft");
    if !binary.exists() {
        anyhow::bail!(
            "cargo zigbuild reported success but {} is missing",
            binary.display()
        );
    }
    Ok(binary)
}

pub struct SshControl {
    host: String,
    socket_dir: PathBuf,
    socket_path: PathBuf,
}

#[cfg(test)]
thread_local! {
    /// A stand-in for `ssh` in this test thread, so the commands built here
    /// can run against a local shim.
    static TEST_SSH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// The remote's config-file hashes, or `None` when they can't be read.
fn read_remote_hashes(mut command: Command) -> Option<std::collections::BTreeMap<String, String>> {
    // Waits out "Text file busy": ssh just installed or updated, or a
    // test's stand-in ssh written a moment ago while another test forked.
    let out = crate::review_ops::spawn_retrying_busy(&mut command)
        .and_then(std::process::Child::wait_with_output)
        .ok()?;
    if !out.status.success() {
        return None;
    }
    crate::config_sync::parse_remote_listing(&String::from_utf8_lossy(&out.stdout))
}

/// Raise this process's soft RLIMIT_NOFILE as far as the hard limit
/// allows (capped at 1M). macOS launchd hands GUI/login processes a soft
/// limit of 256; croft's cross-link opens ~250 rlibs at once, so the zig
/// linker spawned under that default dies with ProcessFdQuotaExceeded and
/// the installer silently falls back to compiling on the remote box —
/// the 2026-06-12 "horrendous latency on various" root cause. Children
/// inherit the raised limit. Returns the resulting soft limit.
pub(crate) fn raise_fd_limit() -> u64 {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return 0;
    }
    let ceiling: libc::rlim_t = 1 << 20;
    let target = if lim.rlim_max == libc::RLIM_INFINITY {
        ceiling
    } else {
        lim.rlim_max.min(ceiling)
    };
    // macOS rejects soft limits above kern.maxfilesperproc even when the
    // hard limit reads as unlimited, so step down through sane sizes
    // until one sticks. Never lower an already-high limit.
    for candidate in [target, 65536, 10240] {
        if candidate <= lim.rlim_cur {
            break;
        }
        let raised = libc::rlimit {
            rlim_cur: candidate,
            rlim_max: lim.rlim_max,
        };
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
            return candidate;
        }
    }
    lim.rlim_cur
}

/// Reason the local cross-build fast path can't run, or `None` if it can. Each
/// tool is located via [`cross_tool_program`] (PATH plus standard toolchain
/// dirs) and then probed, so a GUI launch with a stripped launchd PATH still
/// finds `~/.cargo/bin` and Homebrew. Returning a reason (instead of a bare
/// bool) lets the caller log *why* it fell back to the slow from-scratch build.
fn cross_compile_unavailable_reason() -> Option<String> {
    // `cargo zigbuild --version` is rejected by cargo-zigbuild >=0.22 (the
    // `zigbuild` subcommand has no `--version`, exit 2); probe each binary
    // directly with the arg it accepts.
    for (tool, version_arg, install_hint) in [
        ("cargo", "--version", "install Rust via https://rustup.rs"),
        ("cargo-zigbuild", "--version", "run `croft setup-cross`"),
        ("zig", "version", "run `croft setup-cross`"),
    ] {
        if cross_tool_program(tool).is_none() {
            return Some(format!(
                "`{tool}` not found on PATH or standard toolchain locations ({install_hint})"
            ));
        }
        if !probe_cross_tool(tool, version_arg) {
            return Some(format!("`{tool} {version_arg}` failed ({install_hint})"));
        }
    }
    None
}

fn probe_cross_tool(tool: &str, version_arg: &str) -> bool {
    cross_tool_command(tool)
        .arg(version_arg)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Directories searched for the cross-build toolchain. A Croft.app / Ghostty
/// launch on macOS inherits launchd's stripped PATH, so `~/.cargo/bin` and
/// Homebrew are absent even though the same tools resolve in an interactive
/// shell. PATH entries come first (an explicit override wins), then the
/// standard rustup/Homebrew locations.
fn cross_tool_search_dirs_from(path: Option<&OsStr>, home: Option<&OsStr>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = path {
        for dir in std::env::split_paths(path) {
            push_unique_path(&mut dirs, dir);
        }
    }
    if let Some(home) = home {
        let home = PathBuf::from(home);
        push_unique_path(&mut dirs, home.join(".cargo").join("bin"));
        push_unique_path(&mut dirs, home.join(".local").join("bin"));
    }
    push_unique_path(&mut dirs, PathBuf::from("/opt/homebrew/bin"));
    push_unique_path(&mut dirs, PathBuf::from("/usr/local/bin"));
    dirs
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn cross_tool_search_dirs() -> Vec<PathBuf> {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME");
    cross_tool_search_dirs_from(path.as_deref(), home.as_deref())
}

/// A PATH that includes the standard toolchain dirs, handed to every spawned
/// cross-build tool so child processes (e.g. `cargo` invoking `cargo-zigbuild`
/// invoking `zig`) resolve each other even under a stripped launchd PATH.
fn cross_tool_path() -> OsString {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME");
    cross_tool_path_from(path.as_deref(), home.as_deref())
}

fn cross_tool_path_from(path: Option<&OsStr>, home: Option<&OsStr>) -> OsString {
    std::env::join_paths(cross_tool_search_dirs_from(path, home))
        .unwrap_or_else(|_| path.map(OsString::from).unwrap_or_default())
}

fn cross_tool_program(tool: &str) -> Option<PathBuf> {
    find_executable_in_dirs(tool, &cross_tool_search_dirs())
}

fn find_executable_in_dirs(tool: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(tool))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// A `Command` for a cross-build tool, resolved to its absolute path and run
/// with the augmented PATH so its own child processes resolve too.
fn cross_tool_command(tool: &str) -> Command {
    let program = cross_tool_program(tool).unwrap_or_else(|| PathBuf::from(tool));
    let mut command = Command::new(program);
    command.env("PATH", cross_tool_path());
    command
}

/// The checkout, when this binary still has one. A croft installed from a
/// release (or built in a Nix sandbox) has a `CARGO_MANIFEST_DIR` that does
/// not exist at runtime, and handing a missing directory to `current_dir`
/// fails the spawn with ENOENT before the tool ever runs - which would turn
/// "cargo is not installed" into the diagnosis on a machine where cargo works
/// fine. No checkout also means no cross-build, so the fast path is
/// unavailable either way and the existing reason reporting covers it.
fn checkout_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.is_dir().then_some(dir)
}

/// A cross-build tool run FROM THE CHECKOUT, for the calls whose ANSWER
/// depends on which toolchain is resolved. `rust-toolchain.toml` lives there
/// and rustup reads the channel from its working directory, so a target query
/// run anywhere else answers for the default toolchain while `cargo zigbuild`
/// (which sets `current_dir(source)`) answers for the pinned one. A target
/// present on only one of them makes the guard report success immediately
/// before the build dies with E0463 - four days of silent fallback to a
/// remote `cargo install` after the 1.95.0 to 1.97.1 bump.
///
/// Deliberately NOT used for the mere "is this binary present" probes: those
/// have the same answer in any directory, and running them in the checkout
/// would make an availability check trigger a multi-hundred-MB rustup
/// auto-install of the pinned channel.
fn cross_tool_command_in_checkout(tool: &str) -> Command {
    let mut command = cross_tool_command(tool);
    if let Some(dir) = checkout_dir() {
        command.current_dir(dir);
    }
    command
}

/// The niced, job-capped `cargo zigbuild` invocation, with `cargo` resolved to
/// an absolute path and the augmented PATH exported so it finds cargo-zigbuild
/// and zig.
fn cargo_zigbuild_command(source: &Path, triple: &str, jobs: &str) -> Command {
    let cargo = cross_tool_program("cargo").unwrap_or_else(|| PathBuf::from("cargo"));
    let mut zigbuild = Command::new("nice");
    zigbuild
        .args(["-n", "19"])
        .arg(cargo)
        .args([
            "zigbuild",
            "--profile",
            "remote-fast",
            "--locked",
            "--jobs",
            jobs,
            "--bin",
            "croft",
            "--target",
            triple,
        ])
        .env("PATH", cross_tool_path())
        .current_dir(source);
    zigbuild
}

/// Re-pin the croft workspace member in `Cargo.lock` to whatever `Cargo.toml`
/// now declares, immediately before a `--locked` cross-build.
///
/// Every behavioural change bumps the patch version in `Cargo.toml`, but
/// `cargo install --path .` never rewrites the on-disk lockfile - so the lock
/// drifts exactly one patch behind. `cargo zigbuild --locked` then refuses to
/// build, and the installer silently falls back to a minutes-long
/// from-scratch `cargo install` *on the remote host* (the thing the fast path
/// exists to avoid). `cargo update -p croft-software --offline` rewrites only croft's
/// own version line - it touches no dependency, needs no network, and so keeps
/// `--locked`'s real guarantee (a reproducible dependency graph) fully intact.
///
/// Best-effort: if the sync itself fails we log and still attempt the locked
/// build, preserving the old fall-back behaviour rather than blocking install.
fn sync_workspace_lock(source: &Path, log: impl Fn(String)) {
    match cross_tool_command("cargo")
        .args(["update", "-p", "croft-software", "--offline"])
        .current_dir(source)
        .output()
    {
        Ok(out) if out.status.success() => {
            log(
                "Synced Cargo.lock to the bumped croft version before the locked cross-build"
                    .to_string(),
            );
        }
        Ok(out) => log(format!(
            "Cargo.lock sync skipped (cargo update exited {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => log(format!("Cargo.lock sync skipped ({e})")),
    }
}

/// Push the user's syncable config to the remote (#262), so keybindings,
/// snippets, triggers and matchers follow the binary that already follows
/// them.
///
/// Called from `run_croft_session`'s prologue rather than off the back of
/// the install: when croft is already present the install runs on a
/// detached thread and the session opens without waiting for it, so an
/// install-completion hook would skip the common case entirely.
///
/// Best-effort by construction. A remote whose config cannot be written is
/// a remote that still opens with defaults, which is exactly today's
/// behaviour — failing the connect over it would be a regression. Every
/// outcome is reported so a silent no-op is distinguishable from a silent
/// success.
fn push_config_files(ssh: &SshControl, resolution: &SyncResolution, log: &mut dyn FnMut(String)) {
    // User layers only: a workspace must not decide what leaves the laptop.
    let prefs = crate::config_layers::load_merged(None).prefs;
    if crate::config_sync::host_excluded(&ssh.host, &prefs.config_sync_excluded_hosts) {
        log(format!(
            "Config sync: off for {} (config_sync_excluded_hosts)",
            ssh.host
        ));
        return;
    }
    let (files, skipped) = crate::config_sync::apply_exclusions(
        crate::config_sync::local_files(),
        &prefs.config_sync_excluded_files,
    );
    if !skipped.is_empty() {
        log(format!(
            "Config sync: not pushing {} (config_sync_excluded_files)",
            skipped.join(", ")
        ));
    }
    if files.is_empty() {
        return;
    }
    // `mkdir -p` first: rsync will not create the remote parent, and a
    // fresh box has no ~/.config/croft until croft has run there once.
    // `background_command` for its `-n`: this runs before the session
    // takes the terminal, and a command that inherits stdin would race the
    // shell for the user's keystrokes.
    let mut mk = ssh.background_shell("mkdir -p ~/.config/croft");
    if !matches!(mk.status(), Ok(st) if st.success()) {
        log(String::from(
            "Config sync: could not create ~/.config/croft on the remote; skipping",
        ));
        return;
    }

    // What is there now, so a copy edited on the remote is never replaced
    // (#262). Unreadable means unknown, and unknown is not permission.
    let Some(remote) = read_remote_hashes(ssh_socket_read(
        &ssh.socket_path,
        &ssh.host,
        &crate::config_sync::remote_hash_script(),
    )) else {
        log(String::from(
            "Config sync: could not read the remote's copies; pushing nothing rather than risk overwriting them",
        ));
        return;
    };
    let state_path = crate::config_sync::SyncState::path();
    let mut state = crate::config_sync::SyncState::load(&state_path);

    let bulk = crate::remote_bulk::establish(&ssh.host, &ssh.socket_path, |_| {});
    let mut pushed = Vec::new();
    let mut failed = Vec::new();
    let mut kept = Vec::new();
    let mut conflicts = Vec::new();
    for (syncable, local) in &files {
        let Ok(bytes) = std::fs::read(local) else {
            failed.push(syncable.name);
            continue;
        };
        let local_hash = crate::config_sync::content_hash(&bytes);
        let there = remote.get(syncable.name).map(String::as_str);
        let chosen = |list: &[String]| list.iter().any(|n| n == syncable.name);
        let plan = if chosen(&resolution.take_local) {
            crate::config_sync::Plan::Push
        } else if chosen(&resolution.keep_remote) && there.is_some_and(|r| r != local_hash) {
            let entry = state.file_mut(&ssh.host, syncable.name);
            entry.kept = there.map(|r| (r.to_string(), local_hash.clone()));
            crate::config_sync::Plan::Kept
        } else {
            crate::config_sync::plan(&local_hash, there, state.file(&ssh.host, syncable.name))
        };
        match plan {
            crate::config_sync::Plan::Push => {
                let dest = crate::config_sync::remote_dest(&ssh.host, syncable.name);
                let mut rsync = ship_file_rsync_command(&bulk.lane, &ssh.socket_path, local, &dest);
                match rsync.status() {
                    Ok(st) if st.success() => {
                        let entry = state.file_mut(&ssh.host, syncable.name);
                        entry.pushed = Some(local_hash);
                        entry.kept = None;
                        pushed.push(syncable);
                    }
                    _ => failed.push(syncable.name),
                }
            }
            crate::config_sync::Plan::UpToDate => {
                let entry = state.file_mut(&ssh.host, syncable.name);
                entry.pushed = Some(local_hash);
                entry.kept = None;
            }
            crate::config_sync::Plan::Kept => kept.push(syncable.name),
            crate::config_sync::Plan::Conflict => conflicts.push(syncable.name),
        }
    }
    if let Err(e) = state.save(&state_path) {
        log(format!(
            "Config sync: could not record what was pushed ({e})"
        ));
    }
    if !kept.is_empty() {
        log(format!(
            "Config sync: keeping the remote's own {} (as you chose)",
            kept.join(", ")
        ));
    }
    for name in &conflicts {
        log(format!(
            "Config sync: {name} was changed on {host} since croft last pushed it, so it was left alone. \
             `croft sync-config {host} --diff {name}` compares the two; `--take-local {name}` pushes yours, \
             `--keep-remote {name}` keeps theirs",
            host = ssh.host
        ));
    }

    if !pushed.is_empty() {
        // Name the files rather than counting them: "4 files" tells a user
        // nothing about whether the one they just edited went.
        let names: Vec<&str> = pushed.iter().map(|s| s.name).collect();
        log(format!("Config sync: pushed {}", names.join(", ")));
        // A croft already running on the remote applies them within its
        // config watch's interval (`ConfigWatch`, #262); one started later
        // reads them at launch.
        log(format!(
            "Config sync: {} applies on the remote within a few seconds",
            names.join(", ")
        ));
    }
    if !failed.is_empty() {
        log(format!(
            "Config sync: could not push {} (the remote keeps its own copy)",
            failed.join(", ")
        ));
    }
}

/// How the user settled config files that differ on the remote (#262):
/// push these anyway, or keep the remote's copy of these.
#[derive(Debug, Default, Clone)]
pub struct SyncResolution {
    pub take_local: Vec<String>,
    pub keep_remote: Vec<String>,
}

/// `croft sync-config <host>` (#262): push the syncable config now, the
/// same way a connect does, or with `diff` set show how the remote's copy
/// of that file differs from the local one.
pub fn sync_config_now(host: &str, resolution: &SyncResolution, diff: Option<&str>) -> Result<()> {
    for name in resolution
        .take_local
        .iter()
        .chain(&resolution.keep_remote)
        .map(String::as_str)
        .chain(diff)
    {
        if !crate::config_sync::SYNCABLE.iter().any(|s| s.name == name) {
            let names: Vec<&str> = crate::config_sync::SYNCABLE
                .iter()
                .map(|s| s.name)
                .collect();
            anyhow::bail!("{name} is not a synced file ({})", names.join(", "));
        }
    }
    let ssh = SshControl::start(host)?;
    match diff {
        Some(name) => print_config_diff(&ssh, name),
        None => {
            push_config_files(&ssh, resolution, &mut |msg| println!("{msg}"));
            Ok(())
        }
    }
}

/// Re-pushes a config file edited on this machine while a remote session
/// runs (#262), so a keybinding changed mid-session reaches the remote
/// croft, which applies it on arrival, instead of waiting for the next
/// connect. A thread polls the files with [`crate::config_sync::ConfigWatch`]
/// and ships each change over the bulk lane; it logs to
/// `~/.cache/croft/config-sync.log`, never to the terminal, which belongs to
/// the remote croft. Dropping it stops the thread.
struct ConfigRepush {
    /// Host and control socket to push through; swapped on a reconnect.
    target: std::sync::Arc<std::sync::Mutex<(String, PathBuf)>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Joined on drop, so no push starts after the session has ended.
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ConfigRepush {
    /// Start re-pushing changes relative to `watch`, whose baseline the
    /// caller took before the connect-time push.
    fn start(ssh: &SshControl, watch: crate::config_sync::ConfigWatch) -> Self {
        let target = std::sync::Arc::new(std::sync::Mutex::new((
            ssh.host.clone(),
            ssh.socket_path.clone(),
        )));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (t, s) = (target.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            let dir = crate::paths::config_dir();
            let mut state = RepushState::new(watch);
            let mut exclusions = SyncExclusions::load();
            let mut log = config_sync_log();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                // Checked after the sleep too, so a session that ended
                // during it starts no push.
                if s.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let (host, socket) = match t.lock() {
                    Ok(g) => g.clone(),
                    Err(_) => return,
                };
                repush_tick(
                    &mut state,
                    &dir,
                    std::time::Instant::now(),
                    &mut || match exclusions.current() {
                        None => RepushGate::Unknown,
                        Some((hosts, _)) if crate::config_sync::host_excluded(&host, &hosts) => {
                            RepushGate::HostExcluded
                        }
                        Some((_, files)) => RepushGate::Push(files),
                    },
                    &mut |files| push_over_bulk_lane(&host, &socket, files),
                    &mut log,
                );
            }
        });
        ConfigRepush {
            target,
            stop,
            worker: Some(worker),
        }
    }

    /// Push through the connection a reconnect just opened.
    fn retarget(&self, ssh: &SshControl) {
        if let Ok(mut g) = self.target.lock() {
            *g = (ssh.host.clone(), ssh.socket_path.clone());
        }
    }
}

/// Print a unified diff of the remote's copy of `name` against the local
/// one, remote first, so `+` lines are what a push would bring.
fn print_config_diff(ssh: &SshControl, name: &str) -> Result<()> {
    let local = crate::paths::config_dir().join(name);
    let out = ssh_socket_read(
        &ssh.socket_path,
        &ssh.host,
        &format!("cat ~/.config/croft/{name} 2>/dev/null"),
    )
    .output()?;
    let tmp = std::env::temp_dir().join(format!("croft-sync-{}-{name}", std::process::id()));
    std::fs::write(&tmp, &out.stdout)?;
    let local_arg = if local.is_file() {
        local
    } else {
        PathBuf::from("/dev/null")
    };
    let status = Command::new("diff")
        .args(["-u", "--label"])
        .arg(format!("{}:{name}", ssh.host))
        .arg("--label")
        .arg(format!("local:{name}"))
        .arg(&tmp)
        .arg(&local_arg)
        .status();
    let _ = std::fs::remove_file(&tmp);
    let status = status?;
    if status.code() == Some(0) {
        println!("{name} is the same on {} and here", ssh.host);
    }
    Ok(())
}

impl Drop for ConfigRepush {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        // At most one sleep plus a push in flight: the teardown waits for a
        // transfer already started rather than cutting it off.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The user's config-sync exclusions (#603) as a live re-push reads them.
///
/// Read again whenever a file changes, so an edited exclusion applies
/// without a reconnect. Only a clean read counts: one that skipped a layer
/// (a config file half-written or invalid at that moment) would report the
/// default, empty lists, and pushing on those would send a file to a host
/// the user had excluded. So a bad read keeps the last clean one, and until
/// there has been a clean read at all the exclusions are unknown and
/// nothing is pushed.
struct SyncExclusions {
    /// `(config_sync_excluded_hosts, config_sync_excluded_files)` from the
    /// last clean read; `None` before the first.
    known: Option<(Vec<String>, Vec<String>)>,
}

impl SyncExclusions {
    /// The exclusions as of the session start, if they read cleanly.
    fn load() -> Self {
        let mut ex = SyncExclusions { known: None };
        ex.update(crate::config_layers::load_merged(None));
        ex
    }

    /// Re-read the exclusions: the latest clean read, or `None` while no
    /// read has been clean. User layers only, as at connect: a workspace
    /// must not decide what leaves the laptop.
    fn current(&mut self) -> Option<(Vec<String>, Vec<String>)> {
        self.update(crate::config_layers::load_merged(None));
        self.known.clone()
    }

    fn update(&mut self, merged: crate::config_layers::MergedConfig) {
        if merged.warnings.is_empty() {
            self.known = Some((
                merged.prefs.config_sync_excluded_hosts,
                merged.prefs.config_sync_excluded_files,
            ));
        }
    }
}

/// What the exclusions say about a pending re-push.
enum RepushGate {
    /// Push, leaving out these files (`config_sync_excluded_files`).
    Push(Vec<String>),
    /// The host is in `config_sync_excluded_hosts`: push nothing, now or later.
    HostExcluded,
    /// The exclusions have not read cleanly: hold the files and try again.
    Unknown,
}

/// How long a failed re-push waits before it is tried again: long enough
/// not to hammer a connection that is down, short enough that an edit lands
/// soon after a reconnect.
const REPUSH_RETRY: std::time::Duration = std::time::Duration::from_secs(10);

/// What a live re-push carries between polls.
struct RepushState {
    watch: crate::config_sync::ConfigWatch,
    /// Changed files not yet pushed: a failed push stays here and is
    /// retried, so an edit made while the connection was down still lands
    /// after the reconnect.
    pending: std::collections::BTreeSet<PathBuf>,
    retry_at: Option<std::time::Instant>,
}

impl RepushState {
    fn new(watch: crate::config_sync::ConfigWatch) -> Self {
        RepushState {
            watch,
            pending: std::collections::BTreeSet::new(),
            retry_at: None,
        }
    }
}

/// Config files to push, each with the local path it is read from.
type Pushes = [(crate::config_sync::Syncable, PathBuf)];

/// What a re-push did not send: files that failed to transfer (tried
/// again), and files held because they were edited on the remote since
/// croft last pushed them (#262), which wait for the user to settle.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PushResult {
    failed: Vec<&'static str>,
    held: Vec<&'static str>,
}

/// One poll of a live re-push: push the syncable files that changed and
/// exist, and log what went and what did not. `push` returns the names it
/// could not ship.
fn repush_tick(
    state: &mut RepushState,
    dir: &Path,
    now: std::time::Instant,
    exclusions: &mut dyn FnMut() -> RepushGate,
    push: &mut dyn FnMut(&Pushes) -> PushResult,
    log: &mut dyn FnMut(String),
) {
    let changed = state.watch.poll(now);
    let retry_due = state.retry_at.is_some_and(|t| now >= t);
    if changed.is_empty() && !retry_due {
        return;
    }
    state.pending.extend(changed);
    // A pending file deleted since, or no longer syncable, drops out.
    let pending: Vec<PathBuf> = state.pending.iter().cloned().collect();
    let files = crate::config_sync::repush_targets(&pending, dir);
    state.pending = files.iter().map(|(_, p)| p.clone()).collect();
    state.retry_at = None;
    if files.is_empty() {
        return;
    }
    // The same rules as the push at connect (#603), read when a file
    // changes so an edited exclusion applies without a reconnect.
    let excluded = match exclusions() {
        RepushGate::Push(excluded) => excluded,
        RepushGate::HostExcluded => {
            state.pending.clear();
            return;
        }
        RepushGate::Unknown => {
            // Nothing leaves until the exclusions can be read; the files
            // stay pending and are tried again.
            state.retry_at = Some(now + REPUSH_RETRY);
            log(String::from(
                "Config sync: holding changes until the config-sync exclusions read cleanly",
            ));
            return;
        }
    };
    let (files, _) = crate::config_sync::apply_exclusions(files, &excluded);
    if files.is_empty() {
        state.pending.clear();
        return;
    }
    let PushResult { failed, held } = push(&files);
    state.pending = files
        .iter()
        .filter(|(s, _)| failed.contains(&s.name))
        .map(|(_, p)| p.clone())
        .collect();
    if !state.pending.is_empty() {
        state.retry_at = Some(now + REPUSH_RETRY);
    }
    let pushed: Vec<&str> = files
        .iter()
        .map(|(s, _)| s.name)
        .filter(|n| !failed.contains(n) && !held.contains(n))
        .collect();
    if !pushed.is_empty() {
        log(format!("Config sync: re-pushed {}", pushed.join(", ")));
    }
    for name in &held {
        log(format!(
            "Config sync: left {name} alone: it was changed on the remote since croft last pushed it \
             (`croft sync-config <host> --diff {name}`)"
        ));
    }
    if !failed.is_empty() {
        log(format!(
            "Config sync: could not re-push {} (the remote keeps its copy)",
            failed.join(", ")
        ));
    }
}

/// Ship `files` over a bulk lane opened for this batch; the names that
/// failed.
fn push_over_bulk_lane(
    host: &str,
    socket: &Path,
    files: &[(crate::config_sync::Syncable, PathBuf)],
) -> PushResult {
    // The connect-time push creates this only when it had files to send, so
    // a host that had none has no ~/.config/croft for the first file made
    // mid-session to land in.
    let mut mk = ssh_socket_command(socket, true);
    mk.arg(host).arg("mkdir -p ~/.config/croft");
    let all_failed = || PushResult {
        failed: files.iter().map(|(s, _)| s.name).collect(),
        held: Vec::new(),
    };
    if !matches!(mk.status(), Ok(st) if st.success()) {
        return all_failed();
    }
    // The connect-time rule (#262): never over a copy edited there since.
    let Some(remote) = read_remote_hashes(ssh_socket_read(
        socket,
        host,
        &crate::config_sync::remote_hash_script(),
    )) else {
        return all_failed();
    };
    let state_path = crate::config_sync::SyncState::path();
    let mut state = crate::config_sync::SyncState::load(&state_path);
    let bulk = crate::remote_bulk::establish(host, socket, |_| {});
    let mut result = PushResult::default();
    for (syncable, local) in files {
        let Ok(bytes) = std::fs::read(local) else {
            result.failed.push(syncable.name);
            continue;
        };
        let hash = crate::config_sync::content_hash(&bytes);
        let there = remote.get(syncable.name).map(String::as_str);
        match crate::config_sync::plan(&hash, there, state.file(host, syncable.name)) {
            crate::config_sync::Plan::Push => {
                let dest = crate::config_sync::remote_dest(host, syncable.name);
                let ok = matches!(
                    ship_file_rsync_command(&bulk.lane, socket, local, &dest).status(),
                    Ok(st) if st.success()
                );
                if ok {
                    let entry = state.file_mut(host, syncable.name);
                    entry.pushed = Some(hash);
                    entry.kept = None;
                } else {
                    result.failed.push(syncable.name);
                }
            }
            crate::config_sync::Plan::UpToDate => {
                let entry = state.file_mut(host, syncable.name);
                entry.pushed = Some(hash);
                entry.kept = None;
            }
            crate::config_sync::Plan::Kept | crate::config_sync::Plan::Conflict => {
                result.held.push(syncable.name)
            }
        }
    }
    let _ = state.save(&state_path);
    result
}

/// Appends timestamped lines to `~/.cache/croft/config-sync.log`.
fn config_sync_log() -> impl FnMut(String) {
    let path = install_log_path().with_file_name("config-sync.log");
    move |line: String| {
        use std::io::Write as _;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(f, "{secs} {line}");
        }
    }
}

/// Build the rsync that ships one local file to `dest`, routed and paced by
/// the bulk lane so it never queues ahead of the live session's keystrokes.
///
/// Nothing here is binary-specific: `--checksum` makes rsync compare content
/// rather than mtime, so a caller shipping config files (#262) gets
/// push-only-if-different for free and needs no stamp of its own.
fn ship_file_rsync_command(
    lane: &crate::remote_bulk::BulkLane,
    interactive_socket: &Path,
    local: &Path,
    dest: &str,
) -> Command {
    let mut rsync = Command::new("rsync");
    rsync.args(["-az", "--checksum"]);
    rsync.args(lane.rsync_throttle_args());
    rsync.arg("-e").arg(lane.rsync_ssh_arg(interactive_socket));
    rsync.arg(local).arg(dest);
    rsync
}

/// Prepare the remote source dir: clear every stale top-level entry, keeping
/// only `target/` (the incremental build cache the install depends on) and
/// the [`SOURCE_STAMP_INPUTS`] actually present in the local checkout.
///
/// Neither transfer can be trusted to clean up after a previous croft: the
/// allow-list's `--exclude=*` PROTECTS every unlisted path from rsync's
/// `--delete` (which is how the old deny-list's shipped `target.noindex`
/// trees, 23 to 27 GB per host, would have outlived the allow-list fix), and
/// the tar fallback never deletes at all, so an input since removed from the
/// checkout or a stale `.cargo/config.toml` - which would silently
/// reconfigure every later remote build - persists forever. Inputs the local
/// checkout still contains are deliberately KEPT: rsync deletes staleness
/// inside them itself, wiping them would re-ship the whole tree every sync,
/// and two croft processes updating the same host concurrently must never
/// yank the source out from under the other's running `cargo install`
/// (identical files are a no-op to `rsync -a`, so a concurrent sync of the
/// same checkout touches nothing).
fn remote_source_dir_prep(source: &Path) -> String {
    let keeps: String = std::iter::once("target")
        .chain(
            SOURCE_STAMP_INPUTS
                .iter()
                .copied()
                .filter(|name| source.join(name).exists()),
        )
        .map(|name| format!("! -name '{name}' "))
        .collect();
    // The `/.` dereferences a symlinked source dir (a relocated cache on a
    // small-root VPS): find does not follow a symlink given as its start
    // point, and would otherwise clean nothing while exiting 0.
    format!(
        "mkdir -p \"$HOME/.cache/croft/source\" && \
         find \"$HOME/.cache/croft/source/.\" -mindepth 1 -maxdepth 1 \
         {keeps}-exec rm -rf {{}} +"
    )
}

/// rsync filter rules that ship exactly [`SOURCE_STAMP_INPUTS`] and nothing
/// else.
///
/// An ALLOW-list, deliberately. The deny-list this replaces excluded `target`,
/// which never matched this repo's real build directory `target.noindex`, so
/// every fallback install pushed the whole 176 GB artifact tree to the remote
/// box at the bulk lane's couple of MB/s. A deny-list has to be updated every
/// time the tree grows a new directory; an allow-list derived from the very
/// list the stamp hashes cannot drift, and the two can never disagree about
/// what "the source" is.
fn source_sync_filter_args() -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    // First match wins in rsync, so the every-level denials go first: a root
    // allow-list alone would re-admit `assets/.DS_Store` and any nested
    // `target/`, which the stamp walk skips and would therefore ship without
    // ever being a reason to reinstall.
    for name in SOURCE_SKIP_NAMES {
        args.push(format!("--exclude={name}"));
    }
    for name in SOURCE_STAMP_INPUTS {
        // `/name` admits the entry itself; `/name/***` admits a directory's
        // whole subtree without needing a rule per level.
        args.push(format!("--include=/{name}"));
        args.push(format!("--include=/{name}/***"));
    }
    args.push(String::from("--exclude=*"));
    args
}

/// The build inputs that actually exist under `source`, as relative paths.
/// The tar fallback has no filter language worth the name, so it names the
/// members instead - the same allow-list, expressed the way tar accepts it.
fn source_sync_tar_members(source: &Path) -> Vec<&'static str> {
    SOURCE_STAMP_INPUTS
        .iter()
        .copied()
        .filter(|name| source.join(name).exists())
        .collect()
}

fn source_sync_rsync_command(
    lane: &crate::remote_bulk::BulkLane,
    interactive_socket: &Path,
    source_arg: &std::ffi::OsStr,
    dest: &str,
) -> Command {
    let mut rsync = Command::new("rsync");
    rsync.args(["-a", "-z", "--delete"]);
    rsync.args(source_sync_filter_args());
    rsync.args(lane.rsync_throttle_args());
    rsync.arg("-e").arg(lane.rsync_ssh_arg(interactive_socket));
    rsync.arg(source_arg).arg(dest);
    rsync
}

fn rust_target_installed(triple: &str) -> bool {
    let Ok(output) = cross_tool_command_in_checkout("rustup")
        .args(["target", "list", "--installed"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.trim() == triple)
}

/// `Ok(None)` = binary shipped via the fast path; `Ok(Some(reason))` = the
/// fast path is unavailable (the reason feeds the fallback confirmation);
/// `Err` = the fast path was attempted and broke.
fn try_local_cross_install(ssh: &SshControl, source_stamp: &str) -> Result<Option<String>> {
    if let Some(reason) = cross_compile_unavailable_reason() {
        eprintln!("Local cross-build skipped: {reason}");
        return Ok(Some(reason));
    }
    let Some(triple) = remote_target_triple(ssh)? else {
        let reason = String::from("could not detect the remote architecture");
        eprintln!("Local cross-build skipped: {reason}");
        return Ok(Some(reason));
    };
    if !rust_target_installed(triple) {
        let reason = format!(
            "rustup target `{triple}` not installed (run `rustup target add {triple}` once to enable the fast path)"
        );
        eprintln!("Local cross-build skipped: {reason}");
        return Ok(Some(reason));
    }

    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    sync_workspace_lock(&source, |msg| println!("{msg}"));
    raise_fd_limit();
    let jobs = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(1)
        .to_string();
    println!("Cross-compiling croft locally for {triple} (niced, {jobs} jobs)");
    let status = cargo_zigbuild_command(&source, triple, &jobs)
        .status()
        .context("running cargo zigbuild")?;
    if !status.success() {
        anyhow::bail!("cargo zigbuild exited with {status}");
    }

    let binary = source
        .join("target")
        .join(triple)
        .join("remote-fast")
        .join("croft");
    if !binary.exists() {
        anyhow::bail!(
            "cargo zigbuild reported success but {} is missing",
            binary.display()
        );
    }

    let mkdir = ssh
        .command()
        .arg(&ssh.host)
        .arg("mkdir -p \"$HOME/.cargo/bin\" \"$HOME/.cache/croft\"")
        .status()
        .context("creating remote install dirs")?;
    if !mkdir.success() {
        anyhow::bail!("remote mkdir exited with {mkdir}");
    }

    let ssh_e = format!(
        "ssh -S {} -o ControlMaster=no",
        shell_quote_for_e_arg(&ssh.socket_path),
    );
    let dest = format!("{}:.cargo/bin/croft.new", ssh.host);
    let rsync_status = Command::new("rsync")
        .args(["-az", "--checksum", "-e"])
        .arg(&ssh_e)
        .arg(&binary)
        .arg(&dest)
        .status()
        .context("rsyncing croft binary to remote")?;
    if !rsync_status.success() {
        anyhow::bail!("rsync exited with {rsync_status}");
    }

    // Atomic-swap the freshly-shipped binary into place. `mv` on the
    // same filesystem is the standard way to avoid the race where a
    // mid-flight process opens a half-written executable.
    let activate_status = ssh
        .command()
        .arg(&ssh.host)
        .arg(activate_command(source_stamp))
        .status()
        .context("activating remote croft binary")?;
    if !activate_status.success() {
        anyhow::bail!("remote activation exited with {activate_status}");
    }
    println!("Installed croft on remote via local cross-build.");
    Ok(None)
}

fn sync_local_source_to_remote(ssh: &SshControl) -> Result<()> {
    match sync_via_rsync(ssh) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Fallback path so first-time installs on minimal remotes
            // (alpine images, locked-down VMs) that ship without rsync
            // still succeed. The trade-off is a from-scratch dependency
            // rebuild on those hosts, same as the legacy behaviour.
            eprintln!("rsync sync failed ({e}); falling back to tar pipe");
            sync_via_tar(ssh)
        }
    }
}

/// Mirror the local source tree onto the remote with rsync, preserving
/// mtimes and skipping unchanged files. Critically, this does NOT delete
/// `target/` on the remote, so `cargo install` does an incremental rebuild
/// (typically rebuilding only the 1-3 crates that actually changed)
/// instead of starting from scratch every time.
fn sync_via_rsync(ssh: &SshControl) -> Result<()> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mkdir_status = ssh
        .command()
        .arg(&ssh.host)
        .arg(remote_source_dir_prep(&source))
        .status()
        .context("creating remote source dir")?;
    if !mkdir_status.success() {
        anyhow::bail!("remote mkdir exited with {mkdir_status}");
    }
    let ssh_e = format!(
        "ssh -S {} -o ControlMaster=no",
        shell_quote_for_e_arg(&ssh.socket_path),
    );
    let mut source_arg: std::ffi::OsString = source.clone().into_os_string();
    source_arg.push("/");
    let dest = format!("{}:.cache/croft/source/", ssh.host);
    let status = Command::new("rsync")
        .args(["-a", "-z", "--delete"])
        .args(source_sync_filter_args())
        .arg("-e")
        .arg(&ssh_e)
        .arg(&source_arg)
        .arg(&dest)
        .status()
        .context("running rsync to remote")?;
    if !status.success() {
        anyhow::bail!("rsync exited with {status}");
    }
    Ok(())
}

/// Last-resort tar pipe sync used when rsync isn't available on either
/// side. This path also keeps the remote source dir in place rather than
/// blowing it away — extracting over an existing tree overwrites changed
/// files, so `target/` still survives between installs even when the user
/// is forced down this branch.
fn sync_via_tar(ssh: &SshControl) -> Result<()> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut tar = Command::new("tar")
        .env("COPYFILE_DISABLE", "1")
        .args(["-czf", "-"])
        .args(
            SOURCE_SKIP_NAMES
                .iter()
                .map(|name| format!("--exclude={name}")),
        )
        .arg("-C")
        .arg(&source)
        .args(source_sync_tar_members(&source))
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("packing {}", source.display()))?;

    let tar_stdout = tar.stdout.take().context("opening tar stdout")?;
    let mut remote = ssh
        .command()
        .arg(&ssh.host)
        .arg(format!(
            "{} && tar -xzf - -C \"$HOME/.cache/croft/source\"",
            remote_source_dir_prep(&source)
        ))
        .stdin(Stdio::from(tar_stdout))
        .spawn()
        .context("copying croft source to remote")?;

    let ssh_status = remote.wait().context("waiting for remote source copy")?;
    let tar_status = tar.wait().context("waiting for local source archive")?;
    if !tar_status.success() {
        anyhow::bail!("local source archive failed with {tar_status}");
    }
    if !ssh_status.success() {
        anyhow::bail!("remote source copy failed with {ssh_status}");
    }
    Ok(())
}

/// Quote a path for embedding inside rsync's `-e "ssh -S <path> "`
/// argument. rsync executes the remote-shell string through `/bin/sh -c`,
/// so any spaces or shell metacharacters in the control-socket path would
/// break the invocation. The control socket lives under
/// `std::env::temp_dir()` which is normally space-free, but the socket
/// path is process-id + millisecond-stamped so quote defensively rather
/// than trust the format.
pub(crate) fn shell_quote_for_e_arg(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn local_source_stamp() -> Result<String> {
    build_stamp_for(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        env!("CARGO_PKG_VERSION"),
    )
}

/// The stamp of a croft built from `root` at `version`: the hash of its
/// source, or `release-v<version>` when `root` does not exist here. A
/// release binary bakes the CI checkout it was built in; hashing a missing
/// tree gives one constant for every version, so after the first install a
/// remote would never update again, its watcher never fire and F9 never
/// offer the relaunch (#261). A release's version names its binary exactly.
fn build_stamp_for(root: &PathBuf, version: &str) -> Result<String> {
    if !root.exists() {
        return Ok(format!("release-v{version}"));
    }
    source_stamp_for(root)
}

/// A croft installed with `cargo install` from a registry or git snapshot
/// bakes that frozen checkout as its `CARGO_MANIFEST_DIR`, so the stamp it
/// hashes can never change: "already up to date" then reports on a dead tree
/// while the user's working repo drifts, and installs silently ship nothing.
/// (2026-08-22: a session-host fix believed deployed was never shipped
/// because of exactly this.) Every install path surfaces this warning so the
/// no-op is loud instead of silent.
fn source_snapshot_warning() -> Option<String> {
    let dir = env!("CARGO_MANIFEST_DIR");
    source_dir_is_snapshot(dir).then(|| {
        format!(
            "WARNING: this croft was installed from an immutable snapshot ({dir}); remote installs can never ship local changes. Reinstall with `cargo install --path <your repo>` to make installs track your tree."
        )
    })
}

/// The two places `cargo install` unpacks immutable sources: registry
/// tarballs and git checkouts. Both are subtrees of `$CARGO_HOME`, and a
/// custom `CARGO_HOME` need not contain `.cargo` anywhere in its path
/// (`CARGO_HOME=/custom/cargo-home` unpacks to
/// `/custom/cargo-home/registry/src/...`), so only the two layout-defining
/// components are matched, with no assumption about the prefix.
fn source_dir_is_snapshot(dir: &str) -> bool {
    dir.contains("/registry/src/") || dir.contains("/git/checkouts/")
}

/// Names of the root entries that shape the shipped binary: the crate source,
/// the embedded asset tree, and the build/toolchain pins. The stamp hashes
/// exactly these, so build artifacts (`target`, `target.noindex`), `.git`,
/// docs, and editor scratch dirs can never stall the hash or force a reship —
/// the old deny-list stamp read a 98 GB `target.noindex` on every connect and
/// changed after every local build.
/// Names skipped at EVERY level of the crate source, by the stamp walk and by
/// the sync filters alike. Sharing the list is what makes "the stamp and the
/// sync agree on what the source is" a fact rather than a hope.
const SOURCE_SKIP_NAMES: &[&str] = &[".git", "target", ".DS_Store"];

const SOURCE_STAMP_INPUTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "rust-toolchain.toml",
    "src",
    "assets",
    // Dependencies croft builds from its own tree (`[patch.crates-io]` in
    // Cargo.toml): without them a remote `cargo install --locked` fails.
    "vendor",
];

fn source_stamp_for(root: &PathBuf) -> Result<String> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for name in SOURCE_STAMP_INPUTS {
        let path = root.join(name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        hasher.write(name.as_bytes());
        if meta.is_dir() {
            hash_source_dir(root, &path, &mut hasher)?;
        } else {
            hasher.write_u64(meta.len());
            hasher.write(
                &std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
            );
        }
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn hash_source_dir(root: &PathBuf, dir: &PathBuf, hasher: &mut impl Hasher) -> Result<()> {
    let mut entries = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("reading entries in {}", dir.display()))?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SOURCE_SKIP_NAMES.contains(&name.as_ref()) {
            continue;
        }
        let meta = entry
            .metadata()
            .with_context(|| format!("reading metadata for {}", path.display()))?;
        let rel = path.strip_prefix(root).unwrap_or(&path);
        hasher.write(rel.to_string_lossy().as_bytes());
        if meta.is_dir() {
            hasher.write_u8(b'/');
            hash_source_dir(root, &path, hasher)?;
        } else if meta.is_file() {
            hasher.write_u64(meta.len());
            hasher.write(
                &std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
            );
        }
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod offer_tests;
