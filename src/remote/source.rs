//! This croft's source: the stamp that says whether the remote is up to date, and
//! syncing the source to the remote by allow-list.

use super::*;

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
pub(super) fn remote_source_dir_prep(source: &Path) -> String {
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
pub(super) fn source_sync_filter_args() -> Vec<String> {
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
pub(super) fn source_sync_tar_members(source: &Path) -> Vec<&'static str> {
    SOURCE_STAMP_INPUTS
        .iter()
        .copied()
        .filter(|name| source.join(name).exists())
        .collect()
}

pub(super) fn source_sync_rsync_command(
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

pub(super) fn sync_local_source_to_remote(ssh: &SshControl) -> Result<()> {
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
pub(super) fn sync_via_rsync(ssh: &SshControl) -> Result<()> {
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
pub(super) fn sync_via_tar(ssh: &SshControl) -> Result<()> {
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

pub(super) fn local_source_stamp() -> Result<String> {
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
pub(super) fn build_stamp_for(root: &PathBuf, version: &str) -> Result<String> {
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
pub(super) fn source_snapshot_warning() -> Option<String> {
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
pub(super) fn source_dir_is_snapshot(dir: &str) -> bool {
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
pub(super) const SOURCE_SKIP_NAMES: &[&str] = &[".git", "target", ".DS_Store"];

pub(super) const SOURCE_STAMP_INPUTS: &[&str] = &[
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

pub(super) fn source_stamp_for(root: &PathBuf) -> Result<String> {
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

pub(super) fn hash_source_dir(
    root: &PathBuf,
    dir: &PathBuf,
    hasher: &mut impl Hasher,
) -> Result<()> {
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
