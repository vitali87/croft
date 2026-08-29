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
//! version so the same release is not offered again.
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

fn fetch_latest() -> Option<String> {
    let resp = ureq::AgentBuilder::new()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .get(RELEASES_LATEST_URL)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", concat!("croft/", env!("CARGO_PKG_VERSION")))
        .call()
        .ok()?;
    let body = resp.into_string().ok()?;
    latest_from_release_json(&body)
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
    /// be exercised without a network.
    pub fn start_with(
        cache_dir: PathBuf,
        current_version: String,
        fetch: impl FnOnce() -> Option<String> + Send + 'static,
    ) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut cache = CheckCache::load(&cache_dir);
            if should_query(&cache, now_secs()) {
                if let Some(latest) = fetch() {
                    cache.latest = Some(latest);
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
            let cargo = crate::widgets::dependencies::cargo_binary();
            let event = match stage_command(cargo, &ver, &stage_root).output() {
                Ok(out) => {
                    let mut log = out.stdout;
                    log.extend_from_slice(&out.stderr);
                    let _ = std::fs::write(&log_path, &log);
                    if out.status.success() {
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
            Some("0.2.0".into())
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
}
