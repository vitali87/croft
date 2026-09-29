//! The CodeQL CLI croft runs (#578): which `codeql` to use, and a copy of
//! GitHub's release that croft downloads and keeps in its cache, as VS
//! Code's CodeQL extension does.
//!
//! A CLI named in settings wins, then one on `PATH`, then the newest
//! managed copy under `<cache>/codeql/cli/<version>/codeql/`. With none of
//! them, plain `codeql` is run and fails with the usual "could not run
//! codeql" status, which is the prompt to download one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The release "CodeQL: Download CLI" installs until "CodeQL: Check for
/// CLI Updates" has found a newer one.
pub const PINNED_VERSION: &str = "2.27.1";

/// GitHub's "latest release" endpoint for the CLI binaries.
pub const RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/github/codeql-cli-binaries/releases/latest";

/// What the extracted CLI may add up to on disk. The real bundle is a
/// couple of gigabytes; the cap only stops a runaway archive filling it.
const EXTRACT_LIMIT: u64 = 8 << 30;

/// The CLI's executable name on this platform.
pub fn exe_name() -> &'static str {
    if cfg!(windows) {
        "codeql.exe"
    } else {
        "codeql"
    }
}

/// The release asset for a platform (`std::env::consts::OS` and `ARCH`),
/// or `None` where GitHub ships no CLI. The macOS build is universal, so
/// Apple Silicon takes it too.
pub fn asset_name(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("codeql-linux64.zip"),
        ("macos", "x86_64" | "aarch64") => Some("codeql-osx64.zip"),
        ("windows", "x86_64") => Some("codeql-win64.zip"),
        _ => None,
    }
}

/// Where release `version`'s `asset` downloads from.
pub fn download_url(version: &str, asset: &str) -> String {
    format!("https://github.com/github/codeql-cli-binaries/releases/download/v{version}/{asset}")
}

/// The folder managed CLIs live in, one sub-folder per version.
pub fn cli_root(cache: &Path) -> PathBuf {
    cache.join("codeql").join("cli")
}

/// The executable of managed version `version`.
pub fn managed_program(root: &Path, version: &str) -> PathBuf {
    root.join(version).join("codeql").join(exe_name())
}

/// The managed versions installed under `root`, newest first. A folder
/// that is not a version, or holds no executable (an interrupted
/// install), is skipped.
pub fn installed_versions(root: &Path) -> Vec<String> {
    let mut found: Vec<((u64, u64, u64), String)> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| crate::update_check::parse_version(&name).map(|v| (v, name)))
        .filter(|(_, name)| managed_program(root, name).is_file())
        .collect();
    found.sort_by_key(|v| std::cmp::Reverse(v.0));
    found.into_iter().map(|(_, name)| name).collect()
}

/// Where the `codeql` in use came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `codeql_cli_path` in settings.
    Settings,
    /// Found on `PATH`.
    Path,
    /// A managed copy of this version.
    Managed(String),
    /// None was found: plain `codeql`, which fails to run.
    Fallback,
}

/// The `codeql` to run, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub program: PathBuf,
    pub source: Source,
}

/// Pick the `codeql` to run: the settings path when it is a file (`~/`
/// meaning `home`), then `codeql` on `path_env`, then the newest managed
/// copy under `cache`, else plain `codeql`.
pub fn resolve(
    setting: Option<&str>,
    home: Option<&Path>,
    path_env: Option<&std::ffi::OsStr>,
    cache: &Path,
) -> Resolved {
    let configured = setting.map(str::trim).filter(|s| !s.is_empty()).map(|s| {
        match (s.strip_prefix("~/"), home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(s),
        }
    });
    if let Some(program) = configured.filter(|p| p.is_file()) {
        return Resolved {
            program,
            source: Source::Settings,
        };
    }
    let on_path = path_env
        .map(|p| std::env::split_paths(p).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|dir| dir.join(exe_name()))
        .find(|p| p.is_file());
    if let Some(program) = on_path {
        return Resolved {
            program,
            source: Source::Path,
        };
    }
    let root = cli_root(cache);
    if let Some(version) = installed_versions(&root).into_iter().next() {
        return Resolved {
            program: managed_program(&root, &version),
            source: Source::Managed(version),
        };
    }
    Resolved {
        program: PathBuf::from("codeql"),
        source: Source::Fallback,
    }
}

/// The version "CodeQL: Download CLI" installs: the newest release a
/// check has found, else the pinned one.
pub fn install_version(latest: Option<&str>) -> &str {
    match latest {
        Some(l) if crate::update_check::is_newer(l, PINNED_VERSION) => l,
        _ => PINNED_VERSION,
    }
}

/// The status line after "CodeQL: Check for CLI Updates" found `latest`,
/// with `current` the version in use (`None` when no CLI runs).
pub fn update_status(latest: &str, current: Option<&str>) -> String {
    match current {
        Some(cur) if !crate::update_check::is_newer(latest, cur) => {
            format!("The CodeQL CLI is up to date (v{cur})")
        }
        _ => format!(
            "CodeQL CLI v{latest} is available \u{2014} run CodeQL: Download CLI to install it"
        ),
    }
}

/// Put back the executable bits `zip` records: [`crate::codeql_db::extract_zip`]
/// writes plain files, and the CLI's launcher and tools must run.
#[cfg(unix)]
fn restore_modes(zip: &Path, dest: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let file = std::fs::File::open(zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("not a zip: {e}"))?;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let (Some(rel), Some(mode)) = (entry.enclosed_name(), entry.unix_mode()) else {
            continue;
        };
        if entry.is_file() && mode & 0o111 != 0 {
            std::fs::set_permissions(
                dest.join(rel),
                std::fs::Permissions::from_mode(mode & 0o777),
            )
            .map_err(|e| e.to_string())?;
        }
    }
    // Archives made on Windows record no modes; the launcher must run
    // regardless.
    let launcher = dest.join("codeql").join(exe_name());
    let mode = std::fs::metadata(&launcher)
        .map_err(|e| e.to_string())?
        .permissions()
        .mode();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(mode | 0o755))
        .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn restore_modes(_zip: &Path, _dest: &Path) -> Result<(), String> {
    Ok(())
}

/// Install the release archive `zip` as managed version `version` under
/// `root`, returning its executable. It is unpacked beside the final
/// folder and renamed into place, so an interrupted install never looks
/// installed.
pub fn install_from_zip(zip: &Path, root: &Path, version: &str) -> Result<PathBuf, String> {
    let partial = root.join(format!("{version}.partial"));
    let _ = std::fs::remove_dir_all(&partial);
    crate::codeql_db::extract_zip(zip, &partial, Some(EXTRACT_LIMIT))?;
    if !partial.join("codeql").join(exe_name()).is_file() {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(format!("the archive holds no codeql/{}", exe_name()));
    }
    restore_modes(zip, &partial)?;
    let dest = root.join(version);
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&partial, &dest).map_err(|e| e.to_string())?;
    Ok(managed_program(root, version))
}

/// Fetch the newest release's version from [`RELEASES_LATEST_URL`].
fn fetch_latest() -> Result<String, String> {
    let resp = ureq::AgentBuilder::new()
        .try_proxy_from_env(true)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .get(RELEASES_LATEST_URL)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", concat!("croft/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| e.to_string())?;
    let body = resp.into_string().map_err(|e| e.to_string())?;
    crate::update_check::latest_from_release_json(&body)
        .ok_or_else(|| String::from("GitHub's answer named no release version"))
}

/// Stream `url` into `dest`, reporting the bytes written so far. A read
/// timeout rather than a total one, since the archive is large and a slow
/// link is still progress.
fn download(url: &str, dest: &Path, progress: &mut dyn FnMut(u64)) -> Result<(), String> {
    use std::io::{Read, Write};
    let resp = ureq::AgentBuilder::new()
        .try_proxy_from_env(true)
        .timeout_connect(std::time::Duration::from_secs(30))
        .timeout_read(std::time::Duration::from_secs(60))
        .build()
        .get(url)
        .set("User-Agent", concat!("croft/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| e.to_string())?;
    let mut reader = resp.into_reader();
    let mut out = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        total += n as u64;
        progress(total);
    }
    out.flush().map_err(|e| e.to_string())
}

/// Fetches the latest release's version.
pub type LatestFn = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;
/// Downloads a URL into a file, reporting bytes written.
pub type DownloadFn =
    Arc<dyn Fn(&str, &Path, &mut dyn FnMut(u64)) -> Result<(), String> + Send + Sync>;

/// The network calls the CLI commands make, swappable so tests run
/// without a network.
#[derive(Clone)]
pub struct Net {
    pub latest: LatestFn,
    pub download: DownloadFn,
}

impl Default for Net {
    fn default() -> Self {
        Self {
            latest: Arc::new(fetch_latest),
            download: Arc::new(download),
        }
    }
}

/// What the download worker reports.
#[derive(Debug)]
pub enum JobUpdate {
    /// A status line while it works.
    Progress(String),
    /// The installed version and its executable, or why it failed.
    Done(Result<(String, PathBuf), String>),
}

/// Download and install `version` under `root` for the `asset` platform,
/// reporting through `send`. An install already there is reused.
pub fn download_and_install(
    net: &Net,
    root: &Path,
    version: &str,
    asset: &str,
    send: &dyn Fn(JobUpdate),
) -> Result<PathBuf, String> {
    let program = managed_program(root, version);
    if program.is_file() {
        return Ok(program);
    }
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let zip = root.join(format!("{version}.download.zip"));
    let url = download_url(version, asset);
    let mut last_mb = u64::MAX;
    let fetched = (net.download)(&url, &zip, &mut |bytes| {
        let mb = bytes >> 20;
        // One line per 10 MB keeps the main loop from redrawing per chunk.
        if mb / 10 != last_mb / 10 {
            last_mb = mb;
            send(JobUpdate::Progress(format!(
                "Downloading CodeQL CLI v{version}\u{2026} {mb} MB"
            )));
        }
    });
    if let Err(e) = fetched {
        let _ = std::fs::remove_file(&zip);
        return Err(format!("download failed: {e}"));
    }
    send(JobUpdate::Progress(format!(
        "Extracting CodeQL CLI v{version}\u{2026}"
    )));
    let installed = install_from_zip(&zip, root, version);
    let _ = std::fs::remove_file(&zip);
    installed
}

/// Write a stand-in release archive to `zip` for tests: `codeql/codeql`
/// holding `launcher`, and a tool, both executable, plus a plain file.
#[cfg(test)]
pub fn write_test_archive(zip: &Path, launcher: &str) {
    use std::io::Write;
    let mut z = zip::ZipWriter::new(std::fs::File::create(zip).unwrap());
    let exec = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
    let plain = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
    z.start_file(format!("codeql/{}", exe_name()), exec)
        .unwrap();
    z.write_all(launcher.as_bytes()).unwrap();
    z.start_file("codeql/tools/linux64/java", exec).unwrap();
    z.write_all(b"").unwrap();
    z.start_file("codeql/LICENSE.md", plain).unwrap();
    z.write_all(b"license").unwrap();
    z.finish().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn asset_names_follow_the_release_assets() {
        assert_eq!(asset_name("linux", "x86_64"), Some("codeql-linux64.zip"));
        assert_eq!(asset_name("macos", "x86_64"), Some("codeql-osx64.zip"));
        assert_eq!(asset_name("macos", "aarch64"), Some("codeql-osx64.zip"));
        assert_eq!(asset_name("windows", "x86_64"), Some("codeql-win64.zip"));
        assert_eq!(asset_name("linux", "aarch64"), None);
        assert_eq!(asset_name("android", "aarch64"), None);
        assert_eq!(asset_name("windows", "aarch64"), None);
        assert_eq!(
            download_url("2.20.0", "codeql-linux64.zip"),
            "https://github.com/github/codeql-cli-binaries/releases/download/v2.20.0/codeql-linux64.zip"
        );
    }

    #[test]
    fn resolve_prefers_settings_then_path_then_the_newest_managed_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let root = cli_root(&cache);
        let bin = tmp.path().join("bin");
        let configured = tmp.path().join("mine").join("codeql");
        touch(&configured);
        touch(&bin.join(exe_name()));
        for v in ["2.9.0", "2.20.0", "2.19.4"] {
            touch(&managed_program(&root, v));
        }
        // Half-installed and stray folders are not versions to run.
        std::fs::create_dir_all(root.join("2.99.0")).unwrap();
        std::fs::create_dir_all(root.join("2.30.0.partial/codeql")).unwrap();
        let path = std::env::join_paths([tmp.path().join("nothing"), bin.clone()]).unwrap();

        let setting = configured.to_str();
        let r = resolve(setting, None, Some(&path), &cache);
        assert_eq!(
            (r.program, r.source),
            (configured.clone(), Source::Settings)
        );

        let r = resolve(Some("~/mine/codeql"), Some(tmp.path()), None, &cache);
        assert_eq!((r.program, r.source), (configured, Source::Settings));

        // A settings path that is not a file falls through.
        let r = resolve(Some("/no/such/codeql"), None, Some(&path), &cache);
        assert_eq!((r.program, r.source), (bin.join(exe_name()), Source::Path));

        let r = resolve(None, None, None, &cache);
        assert_eq!(r.program, managed_program(&root, "2.20.0"));
        assert_eq!(r.source, Source::Managed("2.20.0".into()));

        let r = resolve(Some(" "), None, None, &tmp.path().join("empty"));
        assert_eq!(
            (r.program, r.source),
            (PathBuf::from("codeql"), Source::Fallback)
        );
    }

    #[test]
    fn install_version_takes_a_newer_release_over_the_pin() {
        assert_eq!(install_version(None), PINNED_VERSION);
        assert_eq!(install_version(Some("99.0.0")), "99.0.0");
        assert_eq!(install_version(Some("1.0.0")), PINNED_VERSION);
        assert_eq!(install_version(Some("garbage")), PINNED_VERSION);
    }

    #[test]
    fn update_status_compares_versions_numerically() {
        assert_eq!(
            update_status("2.20.0", Some("2.9.9")),
            "CodeQL CLI v2.20.0 is available \u{2014} run CodeQL: Download CLI to install it"
        );
        assert_eq!(
            update_status("2.20.0", Some("2.20.0")),
            "The CodeQL CLI is up to date (v2.20.0)"
        );
        assert_eq!(
            update_status("2.20.0", Some("2.21.0")),
            "The CodeQL CLI is up to date (v2.21.0)"
        );
        assert_eq!(
            update_status("2.20.0", None),
            "CodeQL CLI v2.20.0 is available \u{2014} run CodeQL: Download CLI to install it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_from_zip_keeps_the_executable_bits_and_renames_into_place() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("codeql.zip");
        write_test_archive(&zip, "#!/bin/sh\necho 2.20.0\n");
        let root = tmp.path().join("cli");
        let program = install_from_zip(&zip, &root, "2.20.0").unwrap();
        assert_eq!(program, managed_program(&root, "2.20.0"));
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&program), 0o755);
        assert_eq!(mode(&root.join("2.20.0/codeql/tools/linux64/java")), 0o755);
        assert_eq!(mode(&root.join("2.20.0/codeql/LICENSE.md")) & 0o111, 0);
        assert!(!root.join("2.20.0.partial").exists());
        assert_eq!(installed_versions(&root), vec![String::from("2.20.0")]);

        // An archive without the launcher is refused and leaves nothing.
        let bad = tmp.path().join("bad.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&bad).unwrap());
        z.start_file("other/file", zip::write::SimpleFileOptions::default())
            .unwrap();
        z.finish().unwrap();
        assert!(install_from_zip(&bad, &root, "2.21.0").is_err());
        assert!(!root.join("2.21.0").exists());
        assert!(!root.join("2.21.0.partial").exists());
    }
}
