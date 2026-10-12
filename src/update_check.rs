//! Release-availability check and staged upgrade for a locally installed
//! croft (#333).
//!
//! The model is VS Code's and Zed's, not a package manager's: croft learns
//! in the background that a newer release exists, tells the user once in a
//! corner popup, and does nothing else until they ask. Choosing Update
//! builds the new version into a STAGING root under `~/.cache/croft` -
//! the binary on PATH is untouched, so a fresh `croft` launch tomorrow is
//! still the version the user has today. Only Relaunch copies the staged
//! binary over the installed one and re-execs into it. Later remembers the
//! version so the same release is not offered again. On Linux, a croft that
//! is a release (from crates.io or a release archive) stages the release's
//! own verified binary instead of building it, so a box with no Rust
//! toolchain updates too (#261); building stays the fallback.
//!
//! (Neovim has no updater at all and leaves it to the package manager; VS
//! Code downloads in the background and shows "Restart to Update"; Zed
//! does the same with a title-bar pill. The download-then-ask shape is the
//! one that never surprises, so it is the one here.)

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use crate::update_watch::UpdateEvent;

/// GitHub's "latest release" endpoint for this repository. Releases are
/// tags with notes, not binaries, so the version is all that is read.
pub const RELEASES_LATEST_URL: &str = "https://api.github.com/repos/vitali87/croft/releases/latest";
/// The crate `cargo install` builds when staging an update.
pub const CRATE_NAME: &str = "croft-software";
/// How long a cached answer stands before the network is asked again.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Network budget for the one request; a slow or absent network must never
/// be felt in the editor.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CACHE_FILE: &str = "update-check.json";
const STAGE_DIR: &str = "staged";

/// Where the running binary came from, as far as upgrading it is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallSource {
    /// Inside a Homebrew Cellar: Homebrew owns the file, and `brew upgrade`
    /// is the only upgrade that keeps its records true.
    Homebrew,
    /// Anything else (cargo, binstall, a release archive, a source build):
    /// croft may stage and swap the binary itself.
    SelfManaged,
}

/// What a Homebrew user runs instead of croft's own updater.
pub const BREW_UPGRADE: &str = "brew upgrade croft";

/// Classify `exe`, the canonical path of the running binary (symlinks such
/// as `/opt/homebrew/bin/croft` already resolved into the Cellar).
pub fn install_source(exe: &Path) -> InstallSource {
    // Homebrew keeps every formula at `<prefix>/Cellar/<formula>/<version>/`,
    // on every prefix it supports (/opt/homebrew, /usr/local, linuxbrew), so
    // a `Cellar/croft` pair of components is the signature, not the prefix.
    let parts: Vec<_> = exe.components().map(|c| c.as_os_str()).collect();
    // The whole `Cellar/croft/<version>/bin/croft` tail: a source checkout
    // that happens to sit under a `Cellar/croft` directory is not brew's.
    let owned = parts.len() >= 5
        && parts[parts.len() - 5] == "Cellar"
        && parts[parts.len() - 4] == "croft"
        && parts[parts.len() - 2] == "bin"
        && parts[parts.len() - 1] == "croft";
    if owned {
        InstallSource::Homebrew
    } else {
        InstallSource::SelfManaged
    }
}

/// The running binary's install source, resolving symlinks first so the
/// `bin/croft` link Homebrew puts on PATH is traced into the Cellar.
pub fn current_install_source() -> InstallSource {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map(|p| install_source(&p))
        .unwrap_or(InstallSource::SelfManaged)
}

/// The on-disk memory of the check, one small JSON file in the cache dir.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckCache {
    /// Unix seconds of the last successful query.
    #[serde(default)]
    pub checked_at: u64,
    /// The newest release tag seen (without the leading `v`).
    #[serde(default)]
    pub latest: Option<String>,
    /// A version the user chose Later on: never offered again.
    #[serde(default)]
    pub dismissed: Option<String>,
}

impl CheckCache {
    pub fn load(cache_dir: &Path) -> Self {
        std::fs::read_to_string(cache_dir.join(CACHE_FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, cache_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(cache_dir)?;
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(cache_dir.join(CACHE_FILE), text)
    }
}

/// `1.2.3` or `v1.2.3` as a comparable triple; anything else is not a
/// release croft knows how to rank.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let mut it = s.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Whether `candidate` is a strictly newer release than `current`. Unparseable
/// on either side is "no": a development build must not be nagged.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

/// The release version out of GitHub's release JSON (`tag_name`), the
/// leading `v` stripped.
pub fn latest_from_release_json(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = v.get("tag_name")?.as_str()?.trim();
    let version = tag.trim_start_matches('v');
    parse_version(version).map(|_| version.to_string())
}

/// Whether the cache is stale enough to ask the network again. Keyed on
/// the last attempt, not the last answer: a failed request counts, so a
/// blocked endpoint is retried daily rather than on every launch.
pub fn should_query(cache: &CheckCache, now_secs: u64) -> bool {
    // A stamp from the future (clock skew, a restored backup) is stale,
    // not a reason to stay silent until the clock catches up.
    now_secs < cache.checked_at || now_secs - cache.checked_at >= CHECK_INTERVAL.as_secs()
}

/// The version to offer, if the cache knows one newer than `current` that
/// the user has not already declined.
pub fn offer_from(cache: &CheckCache, current: &str) -> Option<String> {
    let latest = cache.latest.as_deref()?;
    if !is_newer(latest, current) {
        return None;
    }
    if cache.dismissed.as_deref() == Some(latest) {
        return None;
    }
    Some(latest.to_string())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// crates.io's record of one version of the crate `cargo install` builds.
const CRATES_IO_VERSION_URL: &str = "https://crates.io/api/v1/crates/croft-software";

/// Whether the latest release carries the archive, checksums and signature
/// [`stage_update`] fetches for `triple` (`croft-<triple>.tar.gz`,
/// `SHA256SUMS` and `SHA256SUMS.sigstore.json`, without which the archive
/// is refused), read from GitHub's release JSON `assets`.
pub fn release_has_archive(json: &str, triple: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    let names: Vec<&str> = v
        .get("assets")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|a| a.get("name")?.as_str())
        .collect();
    let archive = format!("croft-{triple}.tar.gz");
    names.contains(&archive.as_str())
        && names.contains(&"SHA256SUMS")
        && names.contains(&"SHA256SUMS.sigstore.json")
}

/// Whether crates.io's JSON for one version says it is published and not
/// yanked, so `cargo install --version` can build it.
pub fn crate_version_listed(json: &str, version: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    let Some(entry) = v.get("version") else {
        return false;
    };
    entry.get("num").and_then(|n| n.as_str()) == Some(version)
        && entry.get("yanked").and_then(|y| y.as_bool()) != Some(true)
}

/// The latest release's version when this install can actually stage it
/// (#1579): the release has an archive for `triple`, this machine's
/// release target, or crates.io has the version for `cargo install` to
/// build. A release that is tagged but neither is never offered, since
/// choosing Update could only fail.
///
/// `on_crates_io` answers `None` when crates.io could not be asked; that
/// failure is passed on as `None` (rather than "not installable") so the
/// caller keeps what it had cached, as it does when GitHub fails.
pub fn installable_release(
    json: &str,
    triple: Option<&str>,
    on_crates_io: &dyn Fn(&str) -> Option<bool>,
) -> Option<Option<String>> {
    let Some(version) = latest_from_release_json(json) else {
        return Some(None);
    };
    if triple.is_some_and(|t| release_has_archive(json, t)) {
        return Some(Some(version));
    }
    Some(on_crates_io(&version)?.then_some(version))
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build()
}

/// `None` when crates.io could not be asked (a timeout, a 5xx, an
/// unreadable body); `Some(false)` when it says the version is absent.
fn crate_version_published(version: &str) -> Option<bool> {
    let resp = http_agent()
        .get(&format!("{CRATES_IO_VERSION_URL}/{version}"))
        .set("User-Agent", concat!("croft/", env!("CARGO_PKG_VERSION")))
        .call();
    match resp {
        Ok(resp) => Some(crate_version_listed(&resp.into_string().ok()?, version)),
        Err(ureq::Error::Status(404, _)) => Some(false),
        Err(_) => None,
    }
}

/// `None` when GitHub (or, when it is consulted, crates.io) could not be
/// asked; `Some(None)` when it answered
/// but the latest release is not one this install can stage.
fn fetch_latest() -> Option<Option<String>> {
    let resp = http_agent()
        .get(RELEASES_LATEST_URL)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", concat!("croft/", env!("CARGO_PKG_VERSION")))
        .call()
        .ok()?;
    let body = resp.into_string().ok()?;
    latest_from_release_json(&body)?;
    let triple =
        prebuilt_triple().filter(|_| crate::remote_prebuilt::eligible(env!("CARGO_MANIFEST_DIR")));
    installable_release(&body, triple, &crate_version_published)
}

/// One background answer: `Some(version)` when a newer release is available
/// and not dismissed, `None` otherwise (including every failure - the
/// editor never learns why, it just does not nag).
pub struct UpdateCheck {
    rx: Receiver<Option<String>>,
}

impl UpdateCheck {
    /// Consult the cache first; hit the network at most once per
    /// [`CHECK_INTERVAL`]. Everything runs off-thread.
    pub fn start(cache_dir: PathBuf, current_version: String) -> Self {
        Self::start_with(cache_dir, current_version, fetch_latest)
    }

    /// [`Self::start`] with the network call injected, so the throttle can
    /// be exercised without a network. `fetch` answers `None` when the
    /// request failed and `Some(None)` when no installable release exists.
    pub fn start_with(
        cache_dir: PathBuf,
        current_version: String,
        fetch: impl FnOnce() -> Option<Option<String>> + Send + 'static,
    ) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut cache = CheckCache::load(&cache_dir);
            if should_query(&cache, now_secs()) {
                // An answer replaces what was cached, an empty one too: a
                // version remembered before this check knew to ask whether
                // it can be installed is not offered again (#1579).
                if let Some(latest) = fetch() {
                    cache.latest = latest;
                }
                // Stamped on failure too: a blocked or rate-limited endpoint
                // must not be asked again on every launch, and every launch
                // would otherwise pay the request's timeout.
                cache.checked_at = now_secs();
                let _ = cache.save(&cache_dir);
            }
            let _ = tx.send(offer_from(&cache, &current_version));
        });
        Self { rx }
    }

    /// Block for the verdict (tests only; the app polls with [`Self::take`]).
    #[cfg(test)]
    pub fn wait(&self) -> Option<String> {
        self.rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the check answers")
    }

    /// The verdict once it lands; `None` while still checking.
    pub fn take(&self) -> Option<Option<String>> {
        self.rx.try_recv().ok()
    }

    #[cfg(test)]
    pub fn preloaded(offer: Option<String>) -> Self {
        let (tx, rx) = channel();
        let _ = tx.send(offer);
        Self { rx }
    }
}

/// Remember that `version` was declined so it is not offered again.
pub fn dismiss(cache_dir: &Path, version: &str) -> std::io::Result<()> {
    let mut cache = CheckCache::load(cache_dir);
    cache.dismissed = Some(version.to_string());
    cache.save(cache_dir)
}

/// Where a staged build lands: cargo's `--root` layout puts the binary
/// under `bin/`.
pub fn staged_binary_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(STAGE_DIR).join("bin").join("croft")
}

/// `cargo install croft-software --version X --locked --root <stage>`: a
/// full build of the published crate into the staging root. Nothing on
/// PATH changes until [`apply_staged`] runs.
pub fn stage_command(cargo: PathBuf, version: &str, stage_root: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(cargo);
    cmd.args([
        "install",
        CRATE_NAME,
        "--version",
        version,
        "--locked",
        "--root",
    ])
    .arg(stage_root);
    cmd
}

/// The release binary target this machine runs, when releases carry one:
/// the static musl builds for Linux on x86_64 and aarch64 (#261), and the
/// Apple silicon and Intel builds for macOS (#1579).
pub fn prebuilt_triple() -> Option<&'static str> {
    prebuilt_triple_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// [`prebuilt_triple`] for an explicit OS and architecture.
pub fn prebuilt_triple_for(os: &str, arch: &str) -> Option<&'static str> {
    match os {
        "linux" => crate::remote::arch_to_musl_triple(arch),
        "macos" => match arch {
            "aarch64" => Some("aarch64-apple-darwin"),
            "x86_64" => Some("x86_64-apple-darwin"),
            _ => None,
        },
        _ => None,
    }
}

/// Fetches `version`'s verified release binary for a target:
/// [`crate::remote_prebuilt::prepare`] with its cache and verifier bound.
pub type PrepareRelease<'a> =
    dyn Fn(&str, &str) -> anyhow::Result<(PathBuf, crate::remote_prebuilt::Signature)> + 'a;

/// Stage `version` at `binary`, saying how in the returned log. With a
/// release target (`prebuilt`), the release's own binary is fetched,
/// checked against its signed `SHA256SUMS` and copied in: no compile, so a
/// box with no toolchain can update itself (#261). Without one, or when
/// that release has no binary or it fails its checks, `build` compiles the
/// published crate as before.
pub fn stage_update(
    version: &str,
    binary: &Path,
    prebuilt: Option<(&str, &PrepareRelease<'_>)>,
    build: &dyn Fn() -> std::io::Result<std::process::Output>,
) -> (UpdateEvent, String) {
    let mut log = String::new();
    if let Some((triple, prepare)) = prebuilt {
        match prepare(version, triple) {
            Ok((path, signature)) => {
                let copied = binary
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::copy(&path, binary).map(|_| ()));
                match copied {
                    Ok(()) => {
                        log.push_str(&format!(
                            "Staged the v{version} release binary for {triple} (signature {})\n",
                            match signature {
                                crate::remote_prebuilt::Signature::Verified => "verified",
                                crate::remote_prebuilt::Signature::Unchecked =>
                                    "NOT checked: cosign is not installed",
                            }
                        ));
                        return (UpdateEvent::Ready, log);
                    }
                    Err(e) => log.push_str(&format!(
                        "Could not stage the release binary ({e}); building instead\n"
                    )),
                }
            }
            Err(e) if crate::remote_prebuilt::is_not_published(&e) => log.push_str(&format!(
                "v{version} has no release binary for {triple}; building instead\n"
            )),
            Err(e) => log.push_str(&format!(
                "The v{version} release binary was refused ({e:#}); building instead\n"
            )),
        }
    }
    match build() {
        Ok(out) => {
            log.push_str(&String::from_utf8_lossy(&out.stdout));
            log.push_str(&String::from_utf8_lossy(&out.stderr));
            let event = if out.status.success() {
                UpdateEvent::Ready
            } else {
                UpdateEvent::Failed
            };
            (event, log)
        }
        Err(err) => {
            log.push_str(&format!("failed to run cargo: {err}"));
            (UpdateEvent::Failed, log)
        }
    }
}

/// A background staged build, reported through the same [`UpdateEvent`]
/// lifecycle the remote watcher and the drift reinstall use so the app's
/// spinner and "ready" pill need no third state machine.
pub struct StagedInstall {
    rx: Receiver<UpdateEvent>,
    binary: PathBuf,
    pub version: String,
}

impl StagedInstall {
    /// Build output lands in `log_path` so a failure is diagnosable.
    pub fn start(cache_dir: PathBuf, version: String, log_path: PathBuf) -> Self {
        let (tx, rx) = channel();
        let binary = staged_binary_path(&cache_dir);
        let stage_root = cache_dir.join(STAGE_DIR);
        let ver = version.clone();
        std::thread::spawn(move || {
            let _ = tx.send(UpdateEvent::InProgress);
            if let Some(dir) = log_path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let build = || {
                stage_command(
                    crate::widgets::dependencies::cargo_binary(),
                    &ver,
                    &stage_root,
                )
                .output()
            };
            let cache = crate::session_state::dirs_cache_croft().join("prebuilt");
            let prepare = |version: &str, triple: &str| {
                crate::remote_prebuilt::prepare(
                    version,
                    triple,
                    &cache,
                    &crate::remote_prebuilt::http_get,
                    &crate::remote_prebuilt::cosign_verify,
                )
            };
            // A release binary only stands in for a release: a croft from
            // crates.io or a release archive, never a source build.
            let prebuilt = prebuilt_triple()
                .filter(|_| crate::remote_prebuilt::eligible(env!("CARGO_MANIFEST_DIR")))
                .map(|t| (t, &prepare as &PrepareRelease<'_>));
            let staged = staged_binary_path(&cache_dir);
            let (event, log) = stage_update(&ver, &staged, prebuilt, &build);
            let _ = std::fs::write(&log_path, log);
            let _ = tx.send(event);
        });
        Self {
            rx,
            binary,
            version,
        }
    }

    /// The staged binary's path (present once `Ready` was sent).
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn drain(&self) -> Vec<UpdateEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    #[cfg(test)]
    pub fn preloaded(events: &[UpdateEvent], binary: PathBuf, version: &str) -> Self {
        let (tx, rx) = channel();
        for ev in events {
            let _ = tx.send(*ev);
        }
        Self {
            rx,
            binary,
            version: version.to_string(),
        }
    }
}

/// Put the staged binary in place of `target`: copy beside it, then rename
/// over, so a crash mid-copy never leaves a half-written croft on PATH and
/// the running process keeps its own (unlinked) image until it execs. The
/// temp file is created exclusively (`create_new`), so a pre-planted
/// symlink at the predictable name is refused rather than followed.
pub fn apply_staged(staged: &Path, target: &Path) -> std::io::Result<()> {
    let Some(dir) = target.parent() else {
        return Err(std::io::Error::other(
            "install target has no parent directory",
        ));
    };
    std::fs::create_dir_all(dir)?;
    let tmp = temp_name(dir);
    apply_staged_at(staged, target, &tmp)
}

/// The temp file beside the target. PIDs recycle: a croft killed between
/// open and rename would leave a file that every later process drawing
/// the same PID trips over, so the name carries a clock reading too.
/// `create_new` in [`apply_staged_at`] still refuses anything planted at
/// the name.
fn temp_name(dir: &Path) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!(".croft-update-{}-{nonce}", std::process::id()))
}

fn apply_staged_at(staged: &Path, target: &Path, tmp: &Path) -> std::io::Result<()> {
    let result = (|| {
        let mut src = std::fs::File::open(staged)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o755);
        }
        let mut dst = opts.open(tmp)?;
        std::io::copy(&mut src, &mut dst)?;
        dst.sync_all()?;
        drop(dst);
        std::fs::rename(tmp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_in_a_homebrew_cellar_is_homebrew_owned() {
        for p in [
            "/opt/homebrew/Cellar/croft/0.1.961/bin/croft",
            "/usr/local/Cellar/croft/0.1.961/bin/croft",
            "/home/linuxbrew/.linuxbrew/Cellar/croft/0.1.961/bin/croft",
        ] {
            assert_eq!(install_source(Path::new(p)), InstallSource::Homebrew, "{p}");
        }
    }

    #[test]
    fn other_installs_are_self_managed() {
        for p in [
            "/Users/me/.cargo/bin/croft",
            "/usr/local/bin/croft",
            // Another formula's Cellar is not ours, and a folder merely named
            // Cellar outside a Homebrew layout is not Homebrew.
            "/opt/homebrew/Cellar/other/1.0/bin/croft",
            "/Users/me/Cellar/notes/croft",
            "/Users/me/Cellar/croft/target/release/croft",
            "/Users/me/code/croft/target/release/croft",
        ] {
            assert_eq!(
                install_source(Path::new(p)),
                InstallSource::SelfManaged,
                "{p}"
            );
        }
    }

    #[test]
    fn versions_parse_with_or_without_the_v_and_compare_numerically() {
        assert_eq!(parse_version("v0.1.800"), Some((0, 1, 800)));
        assert_eq!(parse_version("0.1.9"), Some((0, 1, 9)));
        assert_eq!(parse_version("0.1"), None);
        assert_eq!(parse_version("0.1.9-dirty"), None);
        assert!(is_newer("0.1.800", "0.1.799"));
        assert!(is_newer("0.2.0", "0.1.999"), "numeric, not lexical");
        assert!(!is_newer("0.1.799", "0.1.799"));
        assert!(!is_newer("garbage", "0.1.799"), "unparseable never nags");
    }

    #[test]
    fn the_release_tag_is_read_from_github_json() {
        let json = r#"{"tag_name":"v0.1.900","name":"croft v0.1.900","assets":[]}"#;
        assert_eq!(latest_from_release_json(json).as_deref(), Some("0.1.900"));
        assert_eq!(latest_from_release_json(r#"{"message":"Not Found"}"#), None);
        assert_eq!(latest_from_release_json("not json"), None);
    }

    #[test]
    fn the_network_is_asked_once_a_day() {
        let fresh = CheckCache {
            checked_at: 1_000_000,
            latest: Some("0.1.5".into()),
            dismissed: None,
        };
        assert!(!should_query(&fresh, 1_000_000 + 3600));
        assert!(should_query(&fresh, 1_000_000 + CHECK_INTERVAL.as_secs()));
        assert!(
            should_query(&CheckCache::default(), 1_700_000_000),
            "a never-stamped cache asks"
        );
        assert!(
            should_query(&fresh, 1_000_000 - 60),
            "a stamp from the future is stale, not a gag"
        );
    }

    #[test]
    fn an_offer_needs_a_newer_undismissed_release() {
        let mut cache = CheckCache {
            checked_at: 1,
            latest: Some("0.1.5".into()),
            dismissed: None,
        };
        assert_eq!(offer_from(&cache, "0.1.4").as_deref(), Some("0.1.5"));
        assert_eq!(offer_from(&cache, "0.1.5"), None, "already current");
        assert_eq!(offer_from(&cache, "0.1.6"), None, "ahead of the release");
        cache.dismissed = Some("0.1.5".into());
        assert_eq!(offer_from(&cache, "0.1.4"), None, "Later means not again");
        cache.latest = Some("0.1.6".into());
        assert_eq!(
            offer_from(&cache, "0.1.4").as_deref(),
            Some("0.1.6"),
            "a newer release than the dismissed one is offered"
        );
    }

    #[test]
    fn dismiss_persists_through_the_cache_file() {
        let dir = tempfile::tempdir().unwrap();
        CheckCache {
            checked_at: 7,
            latest: Some("0.1.5".into()),
            dismissed: None,
        }
        .save(dir.path())
        .unwrap();
        dismiss(dir.path(), "0.1.5").unwrap();
        let cache = CheckCache::load(dir.path());
        assert_eq!(cache.dismissed.as_deref(), Some("0.1.5"));
        assert_eq!(
            cache.latest.as_deref(),
            Some("0.1.5"),
            "dismiss keeps the rest"
        );
        assert_eq!(cache.checked_at, 7);
        assert_eq!(
            CheckCache::load(&dir.path().join("nowhere")),
            CheckCache::default(),
            "a missing file is an empty cache"
        );
    }

    /// A failed or blocked request is throttled like a successful one:
    /// the stamp advances so the next launch does not pay the timeout
    /// again, and the empty answer never offers anything.
    #[test]
    fn a_failed_check_is_stamped_and_not_retried_until_the_interval() {
        let dir = tempfile::tempdir().unwrap();
        let check = UpdateCheck::start_with(dir.path().to_path_buf(), "0.1.0".into(), || None);
        assert_eq!(check.wait(), None);
        let cache = CheckCache::load(dir.path());
        assert!(cache.checked_at > 0, "the failed attempt is stamped");
        assert_eq!(cache.latest, None);
        assert!(
            !should_query(&cache, cache.checked_at + 60),
            "not asked again for a day"
        );
        // A later successful fetch after the interval offers normally.
        let check = UpdateCheck::start_with(dir.path().to_path_buf(), "0.1.0".into(), || {
            Some(Some("0.2.0".into()))
        });
        assert_eq!(
            check.wait(),
            None,
            "inside the interval the cache answers, and it is empty"
        );
    }

    #[test]
    fn apply_refuses_a_planted_symlink_at_the_temp_name() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        std::fs::write(&staged, b"new").unwrap();
        let target = dir.path().join("bin").join("croft");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"old").unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        // The temp name carries a clock nonce, so plant the symlink at the
        // exact name the next call will use by exercising the helper.
        let tmp = temp_name(target.parent().unwrap());
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();
        assert!(
            apply_staged_at(&staged, &target, &tmp).is_err(),
            "an existing entry is refused"
        );
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"precious",
            "the symlink target is untouched"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
    }

    #[test]
    fn staging_builds_the_published_crate_into_the_cache_root() {
        let cmd = stage_command(PathBuf::from("/x/cargo"), "0.1.900", Path::new("/c/staged"));
        assert_eq!(cmd.get_program(), "/x/cargo");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "install",
                "croft-software",
                "--version",
                "0.1.900",
                "--locked",
                "--root",
                "/c/staged"
            ]
        );
        assert_eq!(
            staged_binary_path(Path::new("/c")),
            PathBuf::from("/c/staged/bin/croft")
        );
    }

    #[test]
    fn apply_copies_the_staged_binary_over_the_target_executably() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged").join("bin").join("croft");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, b"#!/bin/sh\necho new\n").unwrap();
        let target = dir.path().join("bin").join("croft");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"old").unwrap();
        apply_staged(&staged, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"#!/bin/sh\necho new\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "executable by everyone: {mode:o}");
        }
        let leftovers: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".croft-update-")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp file survives a successful swap"
        );
        assert!(
            apply_staged(&dir.path().join("missing"), &target).is_err(),
            "a missing staged binary is an error, not a silent no-op"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"#!/bin/sh\necho new\n",
            "the failed apply left the target alone"
        );
    }
    #[test]
    fn a_release_binary_is_staged_without_building() {
        use crate::remote_prebuilt::{NotPublished, Signature};
        let tmp = tempfile::tempdir().unwrap();
        let release = tmp.path().join("release-croft");
        std::fs::write(&release, b"release bytes").unwrap();
        let staged = tmp.path().join("staged/bin/croft");
        let built = std::cell::Cell::new(0);
        let build = || {
            built.set(built.get() + 1);
            std::process::Command::new("sh")
                .args(["-c", "echo compiled; exit 0"])
                .output()
        };
        let asked = std::cell::RefCell::new(Vec::new());
        let found = |v: &str, t: &str| {
            asked.borrow_mut().push(format!("{v} {t}"));
            Ok((release.clone(), Signature::Verified))
        };
        let (event, log) = stage_update(
            "0.2.0",
            &staged,
            Some(("x86_64-unknown-linux-musl", &found)),
            &build,
        );
        assert_eq!(event, UpdateEvent::Ready);
        assert_eq!(std::fs::read(&staged).unwrap(), b"release bytes");
        assert_eq!(built.get(), 0, "no compile");
        assert_eq!(*asked.borrow(), ["0.2.0 x86_64-unknown-linux-musl"]);
        assert!(log.contains("signature verified"), "{log}");

        // No binary for that release: build as before.
        let missing = |_: &str, _: &str| -> anyhow::Result<(PathBuf, Signature)> {
            Err(anyhow::Error::new(NotPublished(String::from("x"))))
        };
        let (event, log) = stage_update("0.2.1", &staged, Some(("t", &missing)), &build);
        assert_eq!((event, built.get()), (UpdateEvent::Ready, 1));
        assert!(
            log.contains("v0.2.1 has no release binary for t; building instead"),
            "{log}"
        );
        assert!(log.contains("compiled"), "{log}");

        // A release that fails its checks is refused, and built instead.
        let forged = |_: &str, _: &str| -> anyhow::Result<(PathBuf, Signature)> {
            anyhow::bail!("does not match SHA256SUMS")
        };
        let (_, log) = stage_update("0.2.2", &staged, Some(("t", &forged)), &build);
        assert!(log.contains("refused (does not match SHA256SUMS)"), "{log}");
        assert_eq!(built.get(), 2);

        // No release target (macOS, a source build): only the build.
        let failing = || {
            std::process::Command::new("sh")
                .args(["-c", "echo broken >&2; exit 1"])
                .output()
        };
        let (event, log) = stage_update("0.2.3", &staged, None, &failing);
        assert_eq!(event, UpdateEvent::Failed);
        assert!(log.contains("broken"), "{log}");
    }

    /// #1579: GitHub's v0.2.28 release with its macOS and Linux archives.
    const RELEASE_0_2_28: &str = r#"{"tag_name":"v0.2.28","assets":[
        {"name":"croft-aarch64-apple-darwin.tar.gz"},
        {"name":"croft-x86_64-apple-darwin.tar.gz"},
        {"name":"croft-x86_64-unknown-linux-musl.tar.gz"},
        {"name":"croft-aarch64-unknown-linux-musl.tar.gz"},
        {"name":"SHA256SUMS"},
        {"name":"SHA256SUMS.sigstore.json"}]}"#;

    /// #1579: the issue's case. v0.2.28 is tagged but crates.io stops at
    /// 0.2.3, so a machine with no release target could only run a
    /// `cargo install --version 0.2.28` that fails: nothing is offered.
    #[test]
    fn a_release_with_no_crate_and_no_archive_for_this_machine_is_not_offered() {
        let not_on_crates_io = |_: &str| Some(false);
        assert_eq!(
            installable_release(RELEASE_0_2_28, None, &not_on_crates_io),
            Some(None)
        );
        assert_eq!(
            installable_release(
                RELEASE_0_2_28,
                Some("riscv64gc-unknown-linux-gnu"),
                &not_on_crates_io
            ),
            Some(None),
            "an archive for some other machine does not count"
        );
    }

    /// #1579: either route makes the release installable: its archive for
    /// this machine, or the version on crates.io.
    #[test]
    fn a_release_is_offered_when_its_archive_or_its_crate_exists() {
        let asked = std::cell::RefCell::new(Vec::new());
        let on_crates_io = |v: &str| {
            asked.borrow_mut().push(v.to_string());
            Some(v == "0.2.28")
        };
        assert_eq!(
            installable_release(RELEASE_0_2_28, None, &on_crates_io),
            Some(Some("0.2.28".to_string()))
        );
        assert_eq!(
            *asked.borrow(),
            ["0.2.28"],
            "crates.io is asked for the tag"
        );
        for triple in [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-musl",
        ] {
            assert_eq!(
                installable_release(RELEASE_0_2_28, Some(triple), &|_| Some(false)),
                Some(Some("0.2.28".to_string())),
                "{triple}"
            );
        }
        // An archive without the checksums `stage_update` verifies is not one.
        let unsummed =
            r#"{"tag_name":"v0.2.29","assets":[{"name":"croft-x86_64-apple-darwin.tar.gz"}]}"#;
        assert_eq!(
            installable_release(unsummed, Some("x86_64-apple-darwin"), &|_| Some(false)),
            Some(None)
        );
        // Nor without the signature bundle `remote_prebuilt` refuses to go
        // without: only the crate route is left, and here it is missing.
        let unsigned = r#"{"tag_name":"v0.2.29","assets":[
            {"name":"croft-x86_64-apple-darwin.tar.gz"},{"name":"SHA256SUMS"}]}"#;
        assert_eq!(
            installable_release(unsigned, Some("x86_64-apple-darwin"), &|_| Some(false)),
            Some(None)
        );
        assert_eq!(
            installable_release("not json", None, &|_| Some(true)),
            Some(None)
        );
    }

    /// A crates.io request that fails is a failed check, not a "no": the
    /// caller keeps its cached offer instead of clearing it for a day.
    #[test]
    fn a_crates_io_failure_is_not_an_uninstallable_answer() {
        assert_eq!(installable_release(RELEASE_0_2_28, None, &|_| None), None);
        // An archive for this machine needs no crates.io answer at all.
        assert_eq!(
            installable_release(RELEASE_0_2_28, Some("x86_64-apple-darwin"), &|_| None),
            Some(Some("0.2.28".to_string()))
        );
    }

    #[test]
    fn crates_io_says_which_versions_cargo_can_install() {
        let listed = r#"{"version":{"num":"0.2.28","yanked":false}}"#;
        assert!(crate_version_listed(listed, "0.2.28"));
        assert!(!crate_version_listed(listed, "0.2.29"), "another version");
        assert!(!crate_version_listed(
            r#"{"version":{"num":"0.2.28","yanked":true}}"#,
            "0.2.28"
        ));
        assert!(!crate_version_listed(
            r#"{"errors":[{"detail":"crate `croft-software` does not have a version `0.2.28`"}]}"#,
            "0.2.28"
        ));
        assert!(!crate_version_listed("<html>", "0.2.28"));
    }

    /// #1579: macOS stages the release's own archive, as Linux does, so it
    /// no longer depends on crates.io keeping up with every tag.
    #[test]
    fn macos_and_linux_have_release_targets() {
        assert_eq!(
            prebuilt_triple_for("macos", "aarch64"),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(
            prebuilt_triple_for("macos", "x86_64"),
            Some("x86_64-apple-darwin")
        );
        assert_eq!(
            prebuilt_triple_for("linux", "x86_64"),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            prebuilt_triple_for("linux", "aarch64"),
            Some("aarch64-unknown-linux-musl")
        );
        assert_eq!(prebuilt_triple_for("macos", "powerpc"), None);
        assert_eq!(prebuilt_triple_for("windows", "x86_64"), None);
        assert_eq!(prebuilt_triple_for("freebsd", "x86_64"), None);
    }

    /// #1579: a version cached before croft asked whether it can be
    /// installed is dropped once the check says it cannot, rather than
    /// being offered from the cache every day.
    #[test]
    fn an_uninstallable_answer_clears_a_cached_offer() {
        let dir = tempfile::tempdir().unwrap();
        CheckCache {
            checked_at: 1,
            latest: Some("0.2.28".into()),
            dismissed: None,
        }
        .save(dir.path())
        .unwrap();
        let check =
            UpdateCheck::start_with(dir.path().to_path_buf(), "0.2.3".into(), || Some(None));
        assert_eq!(check.wait(), None);
        assert_eq!(CheckCache::load(dir.path()).latest, None);
    }

    /// #1579 negative: a request that fails keeps what the cache knew, as
    /// before; only an answer replaces it.
    #[test]
    fn a_failed_request_keeps_the_cached_offer() {
        let dir = tempfile::tempdir().unwrap();
        CheckCache {
            checked_at: 1,
            latest: Some("0.2.28".into()),
            dismissed: None,
        }
        .save(dir.path())
        .unwrap();
        let check = UpdateCheck::start_with(dir.path().to_path_buf(), "0.2.3".into(), || None);
        assert_eq!(check.wait().as_deref(), Some("0.2.28"));
    }
}
