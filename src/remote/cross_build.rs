//! The local cross-build: whether a static croft for the remote can be built here,
//! finding the toolchain from the checkout, building it and shipping it.

use super::*;

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
pub(super) fn cross_compile_unavailable_reason() -> Option<String> {
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

pub(super) fn probe_cross_tool(tool: &str, version_arg: &str) -> bool {
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
pub(super) fn cross_tool_search_dirs_from(
    path: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Vec<PathBuf> {
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

pub(super) fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

pub(super) fn cross_tool_search_dirs() -> Vec<PathBuf> {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME");
    cross_tool_search_dirs_from(path.as_deref(), home.as_deref())
}

/// A PATH that includes the standard toolchain dirs, handed to every spawned
/// cross-build tool so child processes (e.g. `cargo` invoking `cargo-zigbuild`
/// invoking `zig`) resolve each other even under a stripped launchd PATH.
pub(super) fn cross_tool_path() -> OsString {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME");
    cross_tool_path_from(path.as_deref(), home.as_deref())
}

pub(super) fn cross_tool_path_from(path: Option<&OsStr>, home: Option<&OsStr>) -> OsString {
    std::env::join_paths(cross_tool_search_dirs_from(path, home))
        .unwrap_or_else(|_| path.map(OsString::from).unwrap_or_default())
}

pub(super) fn cross_tool_program(tool: &str) -> Option<PathBuf> {
    find_executable_in_dirs(tool, &cross_tool_search_dirs())
}

pub(super) fn find_executable_in_dirs(tool: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(tool))
        .find(|candidate| is_executable_file(candidate))
}

pub(super) fn is_executable_file(path: &Path) -> bool {
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
pub(super) fn cross_tool_command(tool: &str) -> Command {
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
pub(super) fn checkout_dir() -> Option<PathBuf> {
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
pub(super) fn cross_tool_command_in_checkout(tool: &str) -> Command {
    let mut command = cross_tool_command(tool);
    if let Some(dir) = checkout_dir() {
        command.current_dir(dir);
    }
    command
}

/// The niced, job-capped `cargo zigbuild` invocation, with `cargo` resolved to
/// an absolute path and the augmented PATH exported so it finds cargo-zigbuild
/// and zig.
pub(super) fn cargo_zigbuild_command(source: &Path, triple: &str, jobs: &str) -> Command {
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
pub(super) fn sync_workspace_lock(source: &Path, log: impl Fn(String)) {
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

pub(super) fn rust_target_installed(triple: &str) -> bool {
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
pub(super) fn try_local_cross_install(
    ssh: &SshControl,
    source_stamp: &str,
) -> Result<Option<String>> {
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

    let ssh_e = rsync_transport(&ssh.socket_path);
    let dest = format!("{}:.cargo/bin/croft.new", ssh.host);
    let rsync_status = crate::proc::command("rsync")
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
