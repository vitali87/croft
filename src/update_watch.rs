use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// Transitions a remote-launched croft observes while a newer binary is
/// being installed under it by the local cross-build that launched it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateEvent {
    /// The `updating` marker appeared: an install is in flight.
    InProgress,
    /// The marker vanished without the stamp advancing: the install
    /// failed or was abandoned. The old binary keeps running.
    Failed,
    /// The install-stamp advanced to a value different from the one this
    /// process launched with: the new binary is in place at the install
    /// path and croft should re-exec into it.
    Ready,
}

/// Filesystem signals the watcher polls, relative to `~/.cache/croft`.
const STAMP_FILE: &str = "install-stamp";
const MARKER_FILE: &str = "updating";
const POLL_INTERVAL: Duration = Duration::from_millis(1000);

pub struct UpdateWatch {
    rx: Receiver<UpdateEvent>,
}

impl UpdateWatch {
    /// Spawn a 1 Hz poller over the two single files under `cache_dir`.
    /// `launch_stamp` is the install-stamp content present when this
    /// process started; any different non-empty value means a newer
    /// binary has landed. Polling one stat per second is negligible and
    /// avoids the per-tree inotify/FSEvents asymmetry the workspace
    /// watcher has to manage.
    pub fn start(cache_dir: PathBuf, launch_stamp: String) -> Self {
        let (tx, rx): (Sender<UpdateEvent>, Receiver<UpdateEvent>) = channel();
        std::thread::spawn(move || poll_loop(&cache_dir, &launch_stamp, &tx));
        Self { rx }
    }

    pub fn drain(&self) -> Vec<UpdateEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

/// One-shot background probe answering: does the repo this binary was
/// installed from now sit at a different commit/dirty state than the binary
/// has baked in? The local half of the deploy-verification story (#242) —
/// remotes are re-stamped on every connect, but `~/.cargo/bin/croft` only
/// changes when someone reinstalls, so it can silently fall behind the tree
/// it came from.
pub struct DriftProbe {
    rx: Receiver<Option<String>>,
}

impl DriftProbe {
    /// `manifest_dir` is the baked `CARGO_MANIFEST_DIR`, `baked_full` the
    /// baked `CROFT_GIT_HASH_FULL` — the full commit ID, because
    /// abbreviation length follows `core.abbrev`/repo size and can differ
    /// between build time and probe time for the same commit. The git
    /// calls run off-thread so a slow or unreachable directory can never
    /// stall startup.
    pub fn start(manifest_dir: String, baked_full: String) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(probe_drift(&manifest_dir, &baked_full));
        });
        Self { rx }
    }

    /// The probe's verdict once it lands: `Some(Some(hash))` = the repo has
    /// moved to `hash` (a short display label), `Some(None)` = no drift (or
    /// no way to tell), `None` = still probing.
    pub fn take(&self) -> Option<Option<String>> {
        self.rx.try_recv().ok()
    }
}

fn probe_drift(manifest_dir: &str, baked_full: &str) -> Option<String> {
    // `unknown` = built without its own repo (registry/git snapshot,
    // tarball, or a checkout tracked only as a subdirectory of a larger
    // repo): drift can't be judged, so the probe stays silent.
    if baked_full == "unknown" {
        return None;
    }
    // git discovery walks up from `-C`, so a manifest dir that merely sits
    // INSIDE some repo (a snapshot unpacked under a $HOME that is itself a
    // git repo) would be answered for by that ancestor — and the hint would
    // then offer to `cargo install` an immutable snapshot over itself. Only
    // a dir that is its own repo toplevel speaks for this source (mirrors
    // the same guard in `build.rs`).
    let toplevel = git_in(manifest_dir, &["rev-parse", "--show-toplevel"])?;
    let dir = std::fs::canonicalize(manifest_dir).ok()?;
    if Some(dir) != std::fs::canonicalize(&toplevel).ok() {
        return None;
    }
    let head = git_in(manifest_dir, &["rev-parse", "HEAD"])?;
    let dirty = !git_in(manifest_dir, &["status", "--porcelain"])?.is_empty();
    drift_label(baked_full, &head, dirty)?;
    // Drift confirmed on full IDs; what surfaces in the status bar is the
    // short label, matching how the baked hash is displayed.
    let short = git_in(manifest_dir, &["rev-parse", "--short", "HEAD"])
        .unwrap_or_else(|| head.chars().take(7).collect());
    Some(if dirty {
        format!("{short}-dirty")
    } else {
        short
    })
}

/// Pure comparison mirroring how `build.rs` composes `CROFT_GIT_HASH_FULL`:
/// the repo's current label is `<head>` or `<head>-dirty`, and any
/// difference from the baked value means the binary predates its own source
/// tree. (Same-hash-still-dirty edits are invisible here — content-precise
/// comparison is the remote stamp's job; this is a hint, not a proof.)
fn drift_label(baked_hash: &str, head: &str, dirty: bool) -> Option<String> {
    let current = if dirty {
        format!("{head}-dirty")
    } else {
        head.to_string()
    };
    (current != baked_hash).then_some(current)
}

fn git_in(dir: &str, args: &[&str]) -> Option<String> {
    // Same env scrub as `git_output` in `build.rs`: an inherited `GIT_DIR`
    // (without a worktree) makes git treat `-C`'s dir as the toplevel, which
    // would satisfy the guard in `probe_drift` by construction while HEAD
    // answers from the foreign repo — guaranteed false drift.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

/// The `bin/croft` a local `cargo install` will actually write, mirroring
/// cargo's own root resolution: `CARGO_INSTALL_ROOT`, then `CARGO_HOME`,
/// then `~/.cargo`. (An `install.root` from cargo config files is not read
/// here - best effort; #245.) The re-exec after a self-install must follow
/// the same resolution, or a custom root leaves it relaunching a stale
/// `~/.cargo/bin/croft` forever.
pub fn cargo_install_bin() -> Option<PathBuf> {
    install_bin_from(
        std::env::var_os("CARGO_INSTALL_ROOT"),
        std::env::var_os("CARGO_HOME"),
        std::env::var_os("HOME"),
    )
}

fn install_bin_from(
    install_root: Option<std::ffi::OsString>,
    cargo_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    // Cargo treats an empty env var as unset (home crate filters it);
    // honoring one here would yield the RELATIVE path `bin/croft`, resolved
    // against the workspace being edited - an arbitrary workspace file could
    // then pass the exists() check and get exec'd as the "update".
    let set = |v: Option<std::ffi::OsString>| v.filter(|v| !v.is_empty());
    let root = set(install_root)
        .or_else(|| set(cargo_home))
        .map(PathBuf::from)
        .or_else(|| set(home).map(|h| PathBuf::from(h).join(".cargo")))?;
    Some(root.join("bin").join("croft"))
}

/// The destination cargo itself reported (`Installing`/`Replacing <path>`
/// in its output) - authoritative where env resolution is not: an
/// `install.root` in cargo config is invisible to [`cargo_install_bin`],
/// and re-execing the env-resolved guess would silently relaunch a stale
/// `~/.cargo/bin/croft` forever. Package-progress lines ("Installing
/// croft-software v…") are skipped by requiring an absolute path to the
/// binary.
fn parse_installed_binary(output: &str) -> Option<PathBuf> {
    output.lines().rev().find_map(|line| {
        let line = line.trim();
        let rest = line
            .strip_prefix("Installing ")
            .or_else(|| line.strip_prefix("Replacing "))?;
        let path = std::path::Path::new(rest.trim());
        (path.is_absolute() && path.file_name() == Some(std::ffi::OsStr::new("croft")))
            .then(|| path.to_path_buf())
    })
}

/// True when the manifest declares `pkg` as its `[package]` name. Parsed
/// semantically - a line scanner is fooled by multiline strings (a
/// `description = \"\"\"...\"\"\"` can contain `[package]`/`name` lines), and a
/// `name` under `[[bin]]` or a dependency table must not vouch for the
/// package.
fn manifest_declares_package(manifest: &str, pkg: &str) -> bool {
    manifest
        .parse::<toml::Table>()
        .ok()
        .and_then(|table| {
            let name = table.get("package")?.get("name")?.as_str()?;
            Some(name == pkg)
        })
        .unwrap_or(false)
}

/// Best-effort cleanup of per-process install logs, which otherwise
/// accumulate one file per croft run forever. A week keeps every recent
/// diagnostic (including whatever a live sibling instance just wrote)
/// while bounding the pile; the legacy shared `self-install.log` ages out
/// under the same rule.
fn prune_old_install_logs(dir: &std::path::Path, keep: &std::path::Path) {
    const WEEK: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("self-install") || !name.ends_with(".log") || entry.path() == keep {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > WEEK);
        if expired {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The `cargo install` invocation for the self-install, with cargo resolved
/// to an absolute path: a GUI-launched croft inherits launchd's stripped
/// `PATH`, where spawning a bare `cargo` fails with "No such file or
/// directory" even though a terminal launch finds it.
fn self_install_command(manifest_dir: &str) -> std::process::Command {
    self_install_command_with(crate::widgets::dependencies::cargo_binary(), manifest_dir)
}

/// Split from [`self_install_command`] so tests can inject a deterministic
/// cargo path instead of comparing against the resolver's own answer.
fn self_install_command_with(cargo: PathBuf, manifest_dir: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(cargo);
    cmd.args(["install", "--path", manifest_dir, "--locked"]);
    cmd
}

/// A background `cargo install --path <repo> --locked` reinstalling the
/// local croft, reported through the same [`UpdateEvent`] lifecycle the
/// remote watcher uses so the app consumes both with one state machine:
/// `InProgress` immediately, then `Ready` (new binary at the install path,
/// F9 re-execs into it) or `Failed` (old binary keeps running).
pub struct SelfInstall {
    rx: Receiver<UpdateEvent>,
    /// Where cargo said it wrote the binary, parsed from its output on
    /// success; read by the F9 re-exec.
    installed: std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>,
}

impl SelfInstall {
    /// Full build output lands in `log_path` so a failure is diagnosable
    /// without scrollback.
    pub fn start(manifest_dir: String, log_path: PathBuf) -> Self {
        let (tx, rx) = channel();
        let installed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let installed_writer = std::sync::Arc::clone(&installed);
        std::thread::spawn(move || {
            let _ = tx.send(UpdateEvent::InProgress);
            // The cache dir may not exist yet (nothing else in the local
            // drift flow creates it), and for a refusal below the log is
            // the only place the reason is recorded.
            if let Some(dir) = log_path.parent() {
                let _ = std::fs::create_dir_all(dir);
                prune_old_install_logs(dir, &log_path);
            }
            // The manifest dir is a path baked at build time; nothing stops
            // the filesystem from hosting a different crate there by now.
            // `cargo install` would happily build that foreign package, exit
            // 0, and the app would announce a croft update that never
            // happened - so refuse unless the manifest still declares THIS
            // package (#245).
            let manifest_ok =
                std::fs::read_to_string(std::path::Path::new(&manifest_dir).join("Cargo.toml"))
                    .is_ok_and(|s| manifest_declares_package(&s, env!("CARGO_PKG_NAME")));
            if !manifest_ok {
                let _ = std::fs::write(
                    &log_path,
                    format!(
                        "refusing to install: {manifest_dir}/Cargo.toml no longer declares \
                         package `{}` - the source tree this croft was built from has been \
                         replaced",
                        env!("CARGO_PKG_NAME")
                    ),
                );
                let _ = tx.send(UpdateEvent::Failed);
                return;
            }
            let result = self_install_command(&manifest_dir).output();
            let event = match result {
                Ok(out) => {
                    let mut log = out.stdout;
                    log.extend_from_slice(&out.stderr);
                    let _ = std::fs::write(&log_path, &log);
                    if out.status.success() {
                        if let Ok(mut slot) = installed_writer.lock() {
                            *slot = parse_installed_binary(&String::from_utf8_lossy(&log));
                        }
                        UpdateEvent::Ready
                    } else {
                        UpdateEvent::Failed
                    }
                }
                Err(err) => {
                    let _ = std::fs::write(&log_path, format!("failed to run cargo: {err}"));
                    UpdateEvent::Failed
                }
            };
            let _ = tx.send(event);
        });
        Self { rx, installed }
    }

    /// The path cargo reported installing to, once `Ready` has been sent.
    /// `None` before that, or when the output had no binary line.
    pub fn installed_path(&self) -> Option<PathBuf> {
        self.installed.lock().ok()?.clone()
    }

    pub fn drain(&self) -> Vec<UpdateEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// A finished install with a scripted outcome, for exercising the app's
    /// event handling without running a real `cargo install`.
    #[cfg(test)]
    pub fn preloaded(events: &[UpdateEvent]) -> Self {
        let (tx, rx) = channel();
        for ev in events {
            let _ = tx.send(*ev);
        }
        Self {
            rx,
            installed: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

fn poll_loop(cache_dir: &std::path::Path, launch_stamp: &str, tx: &Sender<UpdateEvent>) {
    let stamp_path = cache_dir.join(STAMP_FILE);
    let marker_path = cache_dir.join(MARKER_FILE);
    let mut announced_in_progress = false;
    loop {
        std::thread::sleep(POLL_INTERVAL);
        let stamp = std::fs::read_to_string(&stamp_path)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if !stamp.is_empty() && stamp != launch_stamp {
            let _ = tx.send(UpdateEvent::Ready);
            return;
        }
        let marker_present = marker_path.exists();
        if marker_present && !announced_in_progress {
            announced_in_progress = true;
            if tx.send(UpdateEvent::InProgress).is_err() {
                return;
            }
        } else if !marker_present && announced_in_progress {
            announced_in_progress = false;
            if tx.send(UpdateEvent::Failed).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("croft-update-watch-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn manifest_name_check_is_section_aware() {
        let croft = "[package]\nname = \"croft-software\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"croft\"\n";
        assert!(manifest_declares_package(croft, "croft-software"));
        assert!(
            !manifest_declares_package(croft, "croft"),
            "a [[bin]] name must not vouch for the package"
        );
        assert!(
            manifest_declares_package("[package]\nname=\"croft-software\"\n", "croft-software"),
            "TOML does not require spaces around ="
        );
        assert!(
            manifest_declares_package("[package]\nname = 'croft-software'\n", "croft-software"),
            "single quotes are legal TOML"
        );
        assert!(
            !manifest_declares_package(
                "[package]\nname = \"other\"\n\n[dependencies.x]\nname = \"croft-software\"\n",
                "croft-software"
            ),
            "a name in another table must not vouch for the package"
        );
        assert!(!manifest_declares_package("", "croft-software"));
        let multiline_trap = "[package]\nname = \"foreign\"\nversion = \"0.1.0\"\n\
                              description = \"\"\"\n[package]\nname = \"croft-software\"\n\"\"\"\n";
        assert!(
            !manifest_declares_package(multiline_trap, "croft-software"),
            "package/name lines inside a multiline string must not vouch for the package"
        );
        assert!(manifest_declares_package(multiline_trap, "foreign"));
    }

    #[test]
    fn self_install_spawns_the_injected_cargo_verbatim() {
        let cmd = self_install_command_with(PathBuf::from("/fake/toolchain/cargo"), "/some/repo");
        assert_eq!(
            PathBuf::from(cmd.get_program()),
            PathBuf::from("/fake/toolchain/cargo")
        );
        let args: Vec<_> = cmd.get_args().map(|a| a.to_os_string()).collect();
        assert_eq!(args, ["install", "--path", "/some/repo", "--locked"]);
    }

    #[test]
    fn self_install_wires_in_the_resolved_cargo() {
        // A GUI-launched croft inherits launchd's stripped PATH, so the
        // self-install must go through the dependencies widget's resolver
        // instead of spawning a bare `cargo`. The resolver itself may fall
        // back to a bare `cargo` on a machine with no toolchain at all;
        // this test proves the wiring, the one above proves the command
        // uses whatever path it is given.
        let cmd = self_install_command("/some/repo");
        assert_eq!(
            PathBuf::from(cmd.get_program()),
            crate::widgets::dependencies::cargo_binary(),
        );
    }

    #[test]
    fn installed_binary_is_parsed_from_cargo_output_not_progress_lines() {
        let output = "  Installing croft-software v0.1.758 (/repo)\n\
                         Compiling croft-software v0.1.758\n\
                        Finished `release` profile [optimized] target(s) in 57s\n\
                       Replacing /custom/root/bin/croft\n\
                        Replaced package `croft-software v0.1.758` with `croft-software v0.1.758`\n";
        assert_eq!(
            parse_installed_binary(output),
            Some(PathBuf::from("/custom/root/bin/croft")),
            "the binary line, not the package-progress line, names the destination"
        );
        assert_eq!(parse_installed_binary("nothing relevant"), None);
    }

    #[test]
    fn install_bin_follows_cargo_root_resolution() {
        let bin = |p: Option<&str>| p.map(std::ffi::OsString::from);
        assert_eq!(
            install_bin_from(
                bin(Some("/custom/root")),
                bin(Some("/ch")),
                bin(Some("/home/u"))
            ),
            Some(PathBuf::from("/custom/root/bin/croft")),
            "CARGO_INSTALL_ROOT wins over everything"
        );
        assert_eq!(
            install_bin_from(None, bin(Some("/ch")), bin(Some("/home/u"))),
            Some(PathBuf::from("/ch/bin/croft")),
            "CARGO_HOME beats the HOME default"
        );
        assert_eq!(
            install_bin_from(None, None, bin(Some("/home/u"))),
            Some(PathBuf::from("/home/u/.cargo/bin/croft")),
            "default is ~/.cargo/bin"
        );
        assert_eq!(install_bin_from(None, None, None), None);
        assert_eq!(
            install_bin_from(bin(Some("")), bin(Some("")), bin(Some("/home/u"))),
            Some(PathBuf::from("/home/u/.cargo/bin/croft")),
            "empty env vars are unset (cargo's rule) - honoring one would \
             yield a relative path resolved against the edited workspace"
        );
    }

    // The baked manifest path can outlive the tree it pointed at; if some
    // OTHER crate lives there now, the install must refuse rather than
    // build the foreign package and report a croft update that never
    // happened.
    #[test]
    fn self_install_refuses_a_foreign_package_at_the_baked_path() {
        let dir = scratch_dir("foreign-pkg");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"definitely-not-croft\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let log = dir.join("install.log");
        let install = SelfInstall::start(dir.to_string_lossy().into_owned(), log.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut events = Vec::new();
        while std::time::Instant::now() < deadline {
            events.extend(install.drain());
            if events.contains(&UpdateEvent::Failed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            events,
            vec![UpdateEvent::InProgress, UpdateEvent::Failed],
            "a foreign package must fail the install, not build it"
        );
        let log = std::fs::read_to_string(&log).unwrap();
        assert!(
            log.contains("refusing to install"),
            "the log must say WHY nothing was built: {log:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn wait_for(watch: &UpdateWatch, want: UpdateEvent) -> bool {
        for _ in 0..50 {
            if watch.drain().contains(&want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn emits_ready_when_stamp_advances() {
        let dir = scratch_dir("ready");
        std::fs::write(dir.join(STAMP_FILE), "old").unwrap();
        let watch = UpdateWatch::start(dir.clone(), String::from("old"));
        std::fs::write(dir.join(STAMP_FILE), "new").unwrap();
        assert!(wait_for(&watch, UpdateEvent::Ready));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drift_label_matches_how_build_rs_composes_the_hash() {
        assert_eq!(drift_label("abc123", "abc123", false), None);
        assert_eq!(drift_label("abc123-dirty", "abc123", true), None);
        assert_eq!(
            drift_label("abc123", "def456", false),
            Some(String::from("def456"))
        );
        // The same commit gaining uncommitted changes is drift: the binary
        // no longer matches the tree.
        assert_eq!(
            drift_label("abc123", "abc123", true),
            Some(String::from("abc123-dirty"))
        );
        // ...and so is the reverse (the dirty edits were committed away).
        assert_eq!(
            drift_label("abc123-dirty", "abc123", false),
            Some(String::from("abc123"))
        );
    }

    // 2026-08-22 (#242): a Mac croft shipped a remote main @ 84a31a0 while
    // itself running a binary built an hour earlier — the machine deployed
    // code newer than itself, invisibly. The probe exists so that state
    // announces itself.
    #[test]
    fn probe_reports_drift_against_a_real_repo_and_silence_when_current() {
        let dir = scratch_dir("drift");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "one",
        ]);
        let full = git(&["rev-parse", "HEAD"]);
        let short = git(&["rev-parse", "--short", "HEAD"]);

        assert_eq!(probe_drift(dir.to_str().unwrap(), &full), None);
        assert_eq!(probe_drift(dir.to_str().unwrap(), "unknown"), None);
        // The comparison runs on full commit IDs, so an abbreviation-length
        // change between build and probe (`core.abbrev` is auto-scaled by
        // repo size) can never read as drift...
        git(&["config", "core.abbrev", "12"]);
        assert_eq!(probe_drift(dir.to_str().unwrap(), &full), None);
        git(&["config", "--unset", "core.abbrev"]);
        // ...and a baked short label must never be passed for comparison.
        assert_ne!(probe_drift(dir.to_str().unwrap(), &short), None);
        // Real drift surfaces the short display label, not the full ID.
        assert_eq!(
            probe_drift(dir.to_str().unwrap(), &"0".repeat(40)),
            Some(short.clone())
        );
        std::fs::write(dir.join("scratch.txt"), "edit").unwrap();
        assert_eq!(
            probe_drift(dir.to_str().unwrap(), &full),
            Some(format!("{short}-dirty"))
        );
        // A directory git can't answer for (deleted repo, moved tree) is
        // silence, never a false alarm.
        assert_eq!(probe_drift("/nonexistent-croft-drift-probe", "abc"), None);
        // A directory merely INSIDE a repo is answered for by its ancestor
        // (`cargo publish` verify under target/package/, a snapshot under a
        // $HOME that is a git repo): that ancestor's commits say nothing
        // about this source, so the probe must stay silent rather than
        // offer to reinstall an immutable snapshot over itself.
        let nested = dir.join("vendored").join("croft-software-0.1.758");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            probe_drift(nested.to_str().unwrap(), &"0".repeat(40)),
            None,
            "an ancestor repo must never supply drift provenance"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn emits_in_progress_then_failed_when_marker_clears_without_stamp_change() {
        let dir = scratch_dir("fail");
        std::fs::write(dir.join(STAMP_FILE), "same").unwrap();
        let watch = UpdateWatch::start(dir.clone(), String::from("same"));
        std::fs::write(dir.join(MARKER_FILE), "").unwrap();
        assert!(wait_for(&watch, UpdateEvent::InProgress));
        std::fs::remove_file(dir.join(MARKER_FILE)).unwrap();
        assert!(wait_for(&watch, UpdateEvent::Failed));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
