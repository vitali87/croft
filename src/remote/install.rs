//! Installing croft on the remote: whether it needs an install or an update, the release
//! binary when this croft is one, the remote build when nothing faster can run, and
//! swapping the new binary in.

use super::*;

pub(super) fn install_remote_croft(ssh: &SshControl, source_stamp: &str) -> Result<()> {
    // Fast path: cross-compile a static musl binary on the local Mac and
    // rsync it directly into the remote's ~/.cargo/bin. Skips the
    // crates.io index update, the dependency walk, and the release-mode
    // codegen+link of the croft crate on the remote. The legacy
    // source-rsync + `cargo install` fallback still exists for when the
    // tooling isn't present (zig + cargo-zigbuild + the matching rust
    // target), the remote arch can't be detected, or the build fails —
    // but it never engages silently: the user confirms it on the tty
    // first, because quitting to run `croft setup-cross` once is almost
    // always the better deal.
    //
    // Before either: the release binary for this exact version (#261), when
    // this croft IS that release.
    let mut ship = |binary: &Path| -> Result<()> {
        let ssh_e = format!(
            "ssh -S {} -o ControlMaster=no",
            shell_quote_for_e_arg(&ssh.socket_path),
        );
        let dest = format!("{}:.cargo/bin/croft.new", ssh.host);
        let status = Command::new("rsync")
            .args(["-az", "--checksum", "-e"])
            .arg(&ssh_e)
            .arg(binary)
            .arg(&dest)
            .status()
            .context("rsyncing the prebuilt croft to the remote")?;
        anyhow::ensure!(status.success(), "rsync exited with {status}");
        Ok(())
    };
    match try_prebuilt_install(ssh, source_stamp, &mut ship, &mut |m| println!("{m}")) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(e) => eprintln!("Prebuilt install failed ({e:#}); building instead"),
    }
    let reason = match try_local_cross_install(ssh, source_stamp) {
        Ok(None) => return Ok(()),
        Ok(Some(reason)) => reason,
        Err(e) => {
            eprintln!("Local cross-build failed ({e:#})");
            format!("local cross-build failed: {e:#}")
        }
    };
    if !confirm_remote_compile_on_tty(&ssh.host, &reason) {
        anyhow::bail!(
            "cross-build unavailable ({reason}); remote compile declined — run `croft setup-cross`, then `croft remote {}` again for the fast install",
            ssh.host
        );
    }
    sync_local_source_to_remote(ssh)?;
    let status = ssh
        .command()
        .arg(&ssh.host)
        .arg(remote_install_command(source_stamp))
        .status()
        .context("installing croft on remote")?;
    if !status.success() {
        anyhow::bail!("remote croft install failed with {status}");
    }
    Ok(())
}

/// Set by `croft remote --build` (#261): skip the prebuilt release binary
/// and build from source, for a source tree ahead of the latest release.
pub(crate) static FORCE_SOURCE_BUILD: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The prebuilt fast path (#261): when this croft was installed from
/// crates.io, ship the release binary for its exact version and the remote's
/// target instead of building one. `Ok(true)` = installed; `Ok(false)` = not
/// applicable (said why); `Err` = attempted and broke. The caller falls back
/// to the cross-build either way. `ship` copies a local binary to the
/// remote's `~/.cargo/bin/croft.new` over whichever transport the caller has.
pub(super) fn try_prebuilt_install(
    ssh: &SshControl,
    source_stamp: &str,
    ship: &mut dyn FnMut(&Path) -> Result<()>,
    log: &mut dyn FnMut(String),
) -> Result<bool> {
    if FORCE_SOURCE_BUILD.load(std::sync::atomic::Ordering::Relaxed) {
        log(String::from(
            "Prebuilt install skipped: --build asked for a source build",
        ));
        return Ok(false);
    }
    if !crate::remote_prebuilt::eligible(env!("CARGO_MANIFEST_DIR")) {
        log(String::from(
            "Prebuilt install skipped: this croft is a source build, so the remote gets the same source",
        ));
        return Ok(false);
    }
    let Some(triple) = remote_target_triple(ssh)? else {
        log(String::from(
            "Prebuilt install skipped: no release for the remote's architecture",
        ));
        return Ok(false);
    };
    let version = env!("CARGO_PKG_VERSION");
    let cache = crate::session_state::dirs_cache_croft().join("prebuilt");
    let binary = match crate::remote_prebuilt::prepare(
        version,
        triple,
        &cache,
        &crate::remote_prebuilt::http_get,
        &crate::remote_prebuilt::cosign_verify,
    ) {
        Ok((b, crate::remote_prebuilt::Signature::Verified)) => {
            log(format!(
                "Release signature verified: v{version}'s SHA256SUMS was signed by its release workflow"
            ));
            b
        }
        Ok((b, crate::remote_prebuilt::Signature::Unchecked)) => {
            log(format!(
                "Release signature NOT checked: cosign is not installed here, so v{version}'s SHA256SUMS rests on HTTPS to GitHub; install cosign to verify it"
            ));
            b
        }
        // Not every version has release artifacts; that is a plain skip,
        // not a failure to shout about on every connect.
        Err(e) if crate::remote_prebuilt::is_not_published(&e) => {
            log(format!(
                "Prebuilt install skipped: v{version} has no release binary for {triple}"
            ));
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    let size_mb = std::fs::metadata(&binary).map(|m| m.len()).unwrap_or(0) as f64 / 1e6;
    log(format!(
        "Installing prebuilt croft v{version} for {triple} ({size_mb:.0} MB) over SSH"
    ));
    let mkdir = ssh
        .command()
        .arg(&ssh.host)
        .arg("mkdir -p \"$HOME/.cargo/bin\" \"$HOME/.cache/croft\"")
        .status()
        .context("creating remote install dirs")?;
    anyhow::ensure!(mkdir.success(), "remote mkdir exited with {mkdir}");
    ship(&binary)?;
    let activate = ssh
        .command()
        .arg(&ssh.host)
        .arg(activate_command(source_stamp))
        .status()
        .context("activating the prebuilt croft on the remote")?;
    anyhow::ensure!(
        activate.success(),
        "remote activation exited with {activate}"
    );
    log(format!("Installed prebuilt croft v{version} on the remote"));
    Ok(true)
}

pub(super) fn remote_target_triple(ssh: &SshControl) -> Result<Option<&'static str>> {
    let output = ssh
        .command()
        .arg(&ssh.host)
        .arg("uname -m")
        .output()
        .context("probing remote architecture")?;
    if !output.status.success() {
        return Ok(None);
    }
    let arch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(arch_to_musl_triple(&arch))
}

pub fn arch_to_musl_triple(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" | "amd64" => Some("x86_64-unknown-linux-musl"),
        "aarch64" | "arm64" => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

/// What the on-box compile changes on the remote besides building croft,
/// said before asking (#950): a "compile anyway?" that installs a toolchain
/// and system packages without naming them is not consent.
/// [`remote_install_command`] is the script these describe.
pub const REMOTE_COMPILE_SIDE_EFFECTS: [&str; 2] = [
    "If Rust is missing there, rustup installs it into ~/.rustup and ~/.cargo (about 1 GB); shell startup files are left alone.",
    "If a C compiler, pkg-config or dtach is missing, or curl and ca-certificates to fetch Rust, the system package manager installs them, through `sudo -n` when not root.",
];

/// Ask on the controlling terminal before the slow on-box compile. The
/// answer defaults to "no": quitting to run `croft setup-cross` once turns
/// every future install into a seconds-long prebuilt-binary ship, so the
/// slow path must be an explicit choice, never a silent fallback.
/// Non-interactive stdin (scripts, CI) declines for the same reason.
pub(super) fn confirm_remote_compile_on_tty(host: &str, reason: &str) -> bool {
    use std::io::{IsTerminal as _, Write as _};
    eprintln!("Fast install unavailable: {reason}");
    eprintln!(
        "Croft can compile itself on {host} instead; the first build can take several minutes and loads the box."
    );
    for line in REMOTE_COMPILE_SIDE_EFFECTS {
        eprintln!("  - {line}");
    }
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "stdin is not a terminal; declining the remote compile. Run `croft setup-cross`, then retry."
        );
        return false;
    }
    eprint!(
        "Compile on {host} anyway? [y/N] (N quits; run `croft setup-cross` once and reconnect — much faster) "
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

pub(super) fn remote_install_needed(ssh: &SshControl, local_stamp: &str) -> Result<bool> {
    let output = ssh
        .command()
        .arg(&ssh.host)
        .arg(remote_install_check_command())
        .output()
        .context("checking remote croft install")?;
    if !output.status.success() {
        return Ok(true);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() != local_stamp)
}

pub(super) fn remote_install_check_command() -> &'static str {
    r#"if [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"
command -v croft >/dev/null 2>&1 && cat "$HOME/.cache/croft/install-stamp" 2>/dev/null"#
}

pub(super) fn remote_install_command(source_stamp: &str) -> String {
    format!(
        r#"set -e
if [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"

# Package installs need root. Run them directly when already root or when
# there is no sudo, otherwise through sudo. `sudo -n` never prompts: an
# account that needs a password fails at once with a clear message instead
# of sudo's TTY errors on a session with no terminal (#950). A function, not
# a variable holding "sudo -n": zsh, a common login shell, does not split an
# unquoted variable into words, so it would look for a command "sudo -n".
croft_sudo() {{
  if [ "$(id -u)" != "0" ] && command -v sudo >/dev/null 2>&1; then
    sudo -n "$@"
  else
    "$@"
  fi
}}

# Install system packages with whatever package manager the box ships.
croft_pkg_install() {{
  if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    croft_sudo apt-get update
    croft_sudo apt-get install -y "$@"
  elif command -v dnf >/dev/null 2>&1; then
    croft_sudo dnf install -y "$@"
  elif command -v yum >/dev/null 2>&1; then
    croft_sudo yum install -y "$@"
  elif command -v apk >/dev/null 2>&1; then
    croft_sudo apk add "$@"
  elif command -v pacman >/dev/null 2>&1; then
    croft_sudo pacman -Sy --noconfirm "$@"
  elif command -v zypper >/dev/null 2>&1; then
    croft_sudo zypper install -y "$@"
  else
    return 1
  fi
}}

# croft compiles native crates from source, so the final link step needs a C
# compiler/linker (`cc`) and pkg-config. Stock cloud images (Ubuntu Server,
# minimal Fedora, Alpine, ...) ship without them, which is why a bare
# `cargo install` dies with `linker `cc` not found`. The user opted into a
# from-source install, so install the toolchain rather than fail. This runs
# unconditionally, even when cargo already exists, because cargo can be
# present on a box that still lacks cc. The per-manager package names differ:
# Debian bundles the compiler in build-essential; others name it separately.
croft_ensure_build_toolchain() {{
  if command -v cc >/dev/null 2>&1 && command -v pkg-config >/dev/null 2>&1; then
    return 0
  fi
  if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    croft_sudo apt-get update
    croft_sudo apt-get install -y build-essential pkg-config
  elif command -v dnf >/dev/null 2>&1; then
    croft_sudo dnf install -y gcc make pkgconf-pkg-config
  elif command -v yum >/dev/null 2>&1; then
    croft_sudo yum install -y gcc make pkgconfig
  elif command -v apk >/dev/null 2>&1; then
    croft_sudo apk add build-base pkgconf
  elif command -v pacman >/dev/null 2>&1; then
    croft_sudo pacman -Sy --noconfirm base-devel pkgconf
  elif command -v zypper >/dev/null 2>&1; then
    croft_sudo zypper install -y gcc make pkg-config
  else
    echo 'croft: no supported package manager found to install a C toolchain (need cc + pkg-config)' >&2
    return 1
  fi
}}

if ! command -v cargo >/dev/null 2>&1; then
  if ! command -v curl >/dev/null 2>&1; then
    if ! croft_pkg_install curl ca-certificates; then
      echo 'cargo and curl are missing and no supported package manager was found to bootstrap them' >&2
      exit 127
    fi
  fi
  # --no-modify-path: rustup would otherwise append `. "$HOME/.cargo/env"`
  # to the remote user's ~/.bashrc and ~/.profile (#950). This script puts
  # ~/.cargo/bin on PATH itself, just below.
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
fi
export PATH="$HOME/.cargo/bin:$PATH"
# Ensure the C toolchain before compiling, regardless of whether cargo was
# already installed: a box can have rustup/cargo but no cc.
croft_ensure_build_toolchain
# dtach lets a remote croft session survive an SSH transport drop (laptop
# sleep, network change): croft launches itself under it for reconnect. Best
# effort and never fatal; a box without dtach just runs without persistence.
command -v dtach >/dev/null 2>&1 || croft_pkg_install dtach || true
# Cap the host build to ~half its cores and run it niced: this fallback
# compiles on the shared remote (often while other workloads run and,
# with launch-now, while a live croft session is using the box), so the
# default `cargo install -j <all cores>` would pin the machine. (nproc+1)/2
# stays >=1 even on a single core; nice is best-effort if present.
CROFT_JOBS=$(( ( $(nproc 2>/dev/null || echo 2) + 1 ) / 2 ))
# A live croft session on this box outranks the build entirely: the user
# is typing into a shell that shares these cores, RAM, and disk. Half the
# cores still wrecks a small VPS, so drop to one compile job and put all
# codegen IO in the idle class; the update simply takes longer.
CROFT_LIVE=""
if pgrep -x croft >/dev/null 2>&1; then
  CROFT_JOBS=1
  CROFT_LIVE=1
fi
# nice and ionice ration CPU and IO, not memory (#694). Each rustc job on
# croft costs well over a GB, so on a host under 16 GB two of them next to
# rust-analyzer were enough to wake the OOM killer, which then took croft or
# a terminal pane. Below that, compile one crate at a time.
CROFT_MEM_KB=$(awk '/^MemTotal:/ {{ print $2 }}' /proc/meminfo 2>/dev/null || true)
CROFT_AVAIL_KB=$(awk '/^MemAvailable:/ {{ print $2 }}' /proc/meminfo 2>/dev/null || true)
if [ -n "$CROFT_MEM_KB" ] && [ "$CROFT_MEM_KB" -lt 16777216 ]; then
  CROFT_JOBS=1
fi
# Little memory free right now counts too, whatever the total: a big host
# can still be mostly spoken for by the time the update runs.
if [ -n "$CROFT_AVAIL_KB" ] && [ "$CROFT_AVAIL_KB" -lt 2097152 ]; then
  CROFT_JOBS=1
  echo "croft: only $(( CROFT_AVAIL_KB / 1024 )) MB of memory available; building with one job" >&2
fi
# And cap the build's memory outright where systemd can: the compile runs
# in its own scope limited to 60% of RAM with no swap, so if it outgrows
# that it is the BUILD the kernel kills, inside its scope, and never croft
# or a shell. Probed first, because a user manager or a delegated memory
# controller is not a given on a server; without one the build runs uncapped
# but still single-job on a small host.
CROFT_MEMCAP=""
# The probe reads the limit back from inside a test scope: systemd accepts
# MemoryMax without complaint where the memory controller is not delegated
# to the user manager (cgroup v1 and hybrid hosts), and enforces nothing.
if [ -n "$CROFT_MEM_KB" ] && command -v systemd-run >/dev/null 2>&1 \
  && [ "$(systemd-run --user --scope --quiet -p MemoryMax=64M -p MemorySwapMax=0 \
    sh -c 'cat "/sys/fs/cgroup$(sed -n "s/^0:://p" /proc/self/cgroup)/memory.max"' 2>/dev/null)" = 67108864 ]; then
  CROFT_MEMCAP="systemd-run --user --scope --quiet -p MemoryMax=$(( CROFT_MEM_KB * 6 / 10 ))K -p MemorySwapMax=0"
fi
CROFT_NICE=""
if command -v nice >/dev/null 2>&1; then CROFT_NICE="nice -n 19"; fi
CROFT_IONICE=""
if command -v ionice >/dev/null 2>&1; then CROFT_IONICE="ionice -c3"; fi
# Tell a croft running on this box that a compile is under way, so it stops
# its own language servers (rust-analyzer alone held 2.4 GB in the OOM that
# prompted #694) and restarts them once no marker names a live build.
#
# One marker per build (`building.<this shell's pid>`), so a second install
# finishing first cannot clear the first's. It holds the pid of the COMPILE
# once it starts (systemd-run --scope, nice and ionice all exec, so `$!` is
# cargo itself): a shell killed mid-build leaves cargo running as an orphan,
# and the marker must keep naming it. Markers whose pid is gone are stale;
# they are swept here, and croft ignores them and any from before a reboot.
#
# Which boot a marker belongs to is `building.<pid>.boot`, the kernel's
# boot_id (#736): unlike boot time it does not move when the wall clock is
# stepped. A sidecar rather than a second field, so a croft from before it,
# which reads the marker as one pid, still pauses for this build.
mkdir -p "$HOME/.cache/croft"
# The script runs under the login shell, and zsh aborts on a glob that
# matches nothing (NOMATCH), which an empty cache always is (#734).
# null_glob makes it expand to nothing, as the `[ -f ]` below expects of
# every other shell. Not `emulate sh`: that also clears ERR_EXIT, and a
# failed compile would then write the install stamp.
[ -z "${{ZSH_VERSION:-}}" ] || setopt null_glob
CROFT_BOOT=$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || true)
for CROFT_OLD in "$HOME/.cache/croft"/building.*; do
  [ -f "$CROFT_OLD" ] || continue
  case "$CROFT_OLD" in
    # Its marker sorts first, so a sidecar whose marker is gone is orphaned.
    *.boot) [ -e "${{CROFT_OLD%.boot}}" ] || rm -f "$CROFT_OLD"; continue ;;
  esac
  CROFT_OLD_PID=$(cat "$CROFT_OLD" 2>/dev/null || true)
  CROFT_OLD_BOOT=$(cat "$CROFT_OLD.boot" 2>/dev/null || true)
  if [ -z "$CROFT_OLD_PID" ] || ! kill -0 "$CROFT_OLD_PID" 2>/dev/null \
    || {{ [ -n "$CROFT_OLD_BOOT" ] && [ -n "$CROFT_BOOT" ] && [ "$CROFT_OLD_BOOT" != "$CROFT_BOOT" ]; }}; then
    rm -f "$CROFT_OLD" "$CROFT_OLD.boot"
  fi
done
CROFT_MARK="$HOME/.cache/croft/building.$$"
CROFT_BUILD_PID=""
if [ -n "$CROFT_BOOT" ]; then printf %s "$CROFT_BOOT" > "$CROFT_MARK.boot"; fi
printf %s "$$" > "$CROFT_MARK"
# Keep the marker while the compile outlives this shell; croft sees it go
# stale the moment the compile ends.
trap 'if [ -z "$CROFT_BUILD_PID" ] || ! kill -0 "$CROFT_BUILD_PID" 2>/dev/null; then rm -f "$CROFT_MARK" "$CROFT_MARK.boot"; fi' EXIT
if [ -n "$CROFT_LIVE" ]; then
  # croft polls the marker once a second, and a server gets up to three
  # seconds to shut down cleanly: let both finish before the first rustc.
  sleep 5
fi
# eval, because this script runs under the remote user's login shell and
# zsh does not word-split unquoted parameters: bare `$CROFT_NICE ...` would
# try to run a command literally named "nice -n 19". eval re-parses the
# assembled line, which splits correctly under both sh/bash and zsh.
# The subshell marks the build (and every rustc under it) as the kernel's
# first choice when memory runs out, without touching this shell (#694),
# then `exec`s it, so `$!` below is still the compile's own pid.
(
  echo 1000 > /proc/self/oom_score_adj 2>/dev/null || true
  eval "exec $CROFT_MEMCAP $CROFT_NICE $CROFT_IONICE"' cargo install --path "$HOME/.cache/croft/source" --jobs "$CROFT_JOBS" --force --locked'
) &
CROFT_BUILD_PID=$!
printf %s "$CROFT_BUILD_PID" > "$CROFT_MARK"
wait "$CROFT_BUILD_PID"
rm -f "$CROFT_MARK" "$CROFT_MARK.boot"
printf %s {stamp} > "$HOME/.cache/croft/install-stamp"
rm -f "$HOME/.cache/croft/updating"
"#,
        stamp = shell_quote(source_stamp)
    )
}

/// Final swap step shared by the cross-build and rsync fast paths: chmod
/// the staged binary, atomically `mv` it over the live one, write the new
/// install-stamp, then clear the `updating` marker. The stamp is written
/// only after the binary is in place, so a remote-launched croft that
/// watches the stamp never re-execs into a half-shipped binary.
pub(super) fn activate_command(source_stamp: &str) -> String {
    format!(
        "chmod 755 \"$HOME/.cargo/bin/croft.new\" && mv \"$HOME/.cargo/bin/croft.new\" \"$HOME/.cargo/bin/croft\" && printf %s {stamp} > \"$HOME/.cache/croft/install-stamp\" && rm -f \"$HOME/.cache/croft/updating\"",
        stamp = shell_quote(source_stamp)
    )
}

/// Drop the `updating` marker so a remote-launched croft shows its
/// "Updating" indicator for the whole build+ship, not just the final
/// remote-side activation. Written before the (long) local cross-compile.
pub(super) fn mark_remote_updating(ssh: &SshControl) {
    let _ = ssh
        .background_command()
        .arg(&ssh.host)
        .arg("mkdir -p \"$HOME/.cache/croft\" && : > \"$HOME/.cache/croft/updating\"")
        .status();
}

/// Clear the marker after a failed install so the indicator resolves to a
/// brief "update failed" rather than hanging on "Updating" forever.
pub(super) fn clear_remote_updating(ssh: &SshControl) {
    let _ = ssh
        .background_command()
        .arg(&ssh.host)
        .arg("rm -f \"$HOME/.cache/croft/updating\"")
        .status();
}

/// True when a croft binary is already resolvable on the remote PATH,
/// regardless of version. Drives the launch-now path: an existing binary
/// can be launched immediately while a newer one installs underneath it.
pub(super) fn remote_croft_present(ssh: &SshControl) -> Result<bool> {
    let output = ssh
        .command()
        .arg(&ssh.host)
        .arg(
            r#"if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
export PATH="$HOME/.cargo/bin:$PATH"
command -v croft >/dev/null 2>&1 && echo yes"#,
        )
        .output()
        .context("probing remote croft presence")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "yes")
}
