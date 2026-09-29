//! The prebuilt fast path for `croft remote` (#261): install the release
//! binary for the remote's target instead of cross-compiling one.
//!
//! Only when it is the SAME build. The remote's install stamp is a hash of
//! the local source (`remote::local_source_stamp`), so a prebuilt binary may
//! stand in for a local build only when both come from the same source: a
//! croft installed from crates.io, whose source directory is the published
//! crate and whose `CARGO_PKG_VERSION` names the release tag the artifact was
//! built from. A source checkout keeps cross-compiling, rather than pairing a
//! newer local with the nearest release (the version-skew rule in #261).
//!
//! The archive is downloaded LOCALLY and shipped over the existing SSH lane,
//! so the remote needs no outbound internet. It is checked against the
//! release's `SHA256SUMS` before anything is extracted, and a link entry in
//! it refuses the whole archive (the same posture as the LSP installer).
//!
//! `SHA256SUMS` itself is checked against the release's Sigstore bundle
//! (`SHA256SUMS.sigstore.json`, signed keylessly by `release.yml`) with
//! `cosign verify-blob`, pinned to that workflow at that tag, so a checksum
//! file the release workflow did not sign is refused before its sums are
//! trusted. Without `cosign` on this machine the sums rest on HTTPS to
//! GitHub, as they did before signing, and the install log says so.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

const RELEASES: &str = "https://github.com/vitali87/croft/releases/download";

/// Largest archive accepted: the release tarballs are a few MB compressed.
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

/// Whether this croft may use a release artifact for the remote: installed
/// from crates.io (its source is the published crate), or a release binary
/// itself, whose baked `CARGO_MANIFEST_DIR` is the CI checkout it was built
/// in and does not exist here (#261). `cargo install --git` checkouts are
/// arbitrary commits, not releases, so they do not qualify.
pub fn eligible(manifest_dir: &str) -> bool {
    eligible_with(manifest_dir, Path::new(manifest_dir).exists())
}

/// [`eligible`], given whether `manifest_dir` exists on this machine.
pub fn eligible_with(manifest_dir: &str, exists: bool) -> bool {
    manifest_dir.contains("/registry/src/") || !exists
}

/// The archive and checksum URLs for `version` (no `v`) and `triple`.
pub fn release_urls(version: &str, triple: &str) -> (String, String) {
    (
        format!("{RELEASES}/v{version}/croft-{triple}.tar.gz"),
        format!("{RELEASES}/v{version}/SHA256SUMS"),
    )
}

/// The Sigstore bundle `release.yml` publishes beside `SHA256SUMS`.
fn bundle_url(version: &str) -> String {
    format!("{RELEASES}/v{version}/SHA256SUMS.sigstore.json")
}

/// The signing identity a genuine release carries: `release.yml` of this
/// repository, run for the tag `v<version>`. Pinning the tag means a bundle
/// from another release cannot vouch for this one's sums.
pub fn release_identity(version: &str) -> String {
    format!("https://github.com/vitali87/croft/.github/workflows/release.yml@refs/tags/v{version}")
}

/// The OIDC issuer of the GitHub Actions token cosign signs with.
const RELEASE_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// What became of `SHA256SUMS`'s signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signature {
    /// cosign verified the bundle against the release identity.
    Verified,
    /// No `cosign` here to check it with: the sums rest on HTTPS to GitHub.
    Unchecked,
}

impl Signature {
    fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unchecked => "unchecked",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "verified" => Some(Self::Verified),
            "unchecked" => Some(Self::Unchecked),
            _ => None,
        }
    }
}

/// Checks `SHA256SUMS` against its Sigstore bundle for `version`:
/// `Ok(Verified)`, `Ok(Unchecked)` when there is nothing to check with, and
/// `Err` when the signature does not verify.
pub type VerifySums<'a> = dyn Fn(&[u8], &[u8], &str) -> Result<Signature> + 'a;

/// [`VerifySums`] through the `cosign` CLI, the verifier Sigstore ships:
/// `cosign verify-blob` with the bundle and the release identity.
pub fn cosign_verify(sums: &[u8], bundle: &[u8], version: &str) -> Result<Signature> {
    // A fresh directory per check: `create_dir` fails on anything already
    // there, a planted symlink included, so the files below are ours.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("croft-cosign-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).context("staging SHA256SUMS for cosign")?;
    let result = run_cosign(&dir, sums, bundle, version);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn run_cosign(dir: &Path, sums: &[u8], bundle: &[u8], version: &str) -> Result<Signature> {
    let (sums_path, bundle_path) = (dir.join("SHA256SUMS"), dir.join("bundle"));
    std::fs::write(&sums_path, sums)?;
    std::fs::write(&bundle_path, bundle)?;
    let out = match std::process::Command::new("cosign")
        .arg("verify-blob")
        .arg("--bundle")
        .arg(&bundle_path)
        .arg("--certificate-identity")
        .arg(release_identity(version))
        .arg("--certificate-oidc-issuer")
        .arg(RELEASE_OIDC_ISSUER)
        .arg(&sums_path)
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Signature::Unchecked),
        Err(e) => return Err(anyhow::Error::new(e).context("running cosign")),
    };
    if out.status.success() {
        return Ok(Signature::Verified);
    }
    let why = String::from_utf8_lossy(&out.stderr);
    let why = why
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    bail!("cosign did not verify it ({why})")
}

/// The hex digest `SHA256SUMS` lists for `name` (`<hex>  <name>`, or
/// `<hex> *<name>` in binary mode). `None` when the name is not listed or
/// the digest is not 64 hex digits.
pub fn parse_sha256sums(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (hex, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start();
        let file = file.strip_prefix('*').unwrap_or(file);
        (file == name && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// The `croft-<triple>/croft` binary out of a release `archive` (gzip'd tar).
/// Any link entry refuses the archive: a release of one binary has no
/// business carrying one, and a link named like the binary could point
/// anywhere.
pub fn unpack_binary(archive: &[u8], triple: &str) -> Result<Vec<u8>> {
    let want = format!("croft-{triple}/croft");
    let mut found = None;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries().context("reading the release archive")? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            bail!("the release archive carries a link entry; refusing it");
        }
        if entry.path()?.to_string_lossy() == want && kind.is_file() {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            found = Some(bytes);
        }
    }
    found.ok_or_else(|| anyhow!("the release archive has no {want}"))
}

/// Download `version`'s binary for `triple` into `cache` (reused on a later
/// connect), verified, and return its path with what became of the release's
/// signature. `get` fetches a URL's bytes and `verify` checks `SHA256SUMS`
/// against its bundle (both injected so tests need no network or cosign).
pub fn prepare(
    version: &str,
    triple: &str,
    cache: &Path,
    get: &dyn Fn(&str) -> Result<Vec<u8>>,
    verify: &VerifySums<'_>,
) -> Result<(PathBuf, Signature)> {
    let dir = cache.join(format!("v{version}")).join(triple);
    let binary = dir.join("croft");
    let recorded = dir.join("croft.sha256");
    let signed = dir.join("croft.signature");
    // A cached binary is reused only when it still hashes to what was
    // recorded when it was verified, so a truncated or edited file is fetched
    // again rather than shipped. One cached before its signature could be
    // checked is checked again, so installing cosign takes effect at once.
    if let (Ok(bytes), Ok(expected), Some(Signature::Verified)) = (
        std::fs::read(&binary),
        std::fs::read_to_string(&recorded),
        std::fs::read_to_string(&signed)
            .ok()
            .and_then(|s| Signature::parse(&s)),
    ) && sha256_hex(&bytes) == expected.trim()
    {
        return Ok((binary, Signature::Verified));
    }
    let (archive_url, sums_url) = release_urls(version, triple);
    let sums_bytes = get(&sums_url).context("fetching SHA256SUMS")?;
    // Every release that carries archives carries the bundle: release.yml
    // publishes them together. So a missing one is refused rather than
    // skipped, since skipping it is exactly what a forged release would ask.
    let bundle = get(&bundle_url(version)).map_err(|e| {
        if is_not_published(&e) {
            anyhow!("v{version}'s SHA256SUMS has no signature bundle; refusing it")
        } else {
            e.context("fetching the SHA256SUMS signature bundle")
        }
    })?;
    let signature = verify(&sums_bytes, &bundle, version).with_context(|| {
        format!("v{version}'s SHA256SUMS signature is not the release's; refusing it")
    })?;
    let sums = String::from_utf8(sums_bytes).context("SHA256SUMS is not text")?;
    let name = format!("croft-{triple}.tar.gz");
    let expected = parse_sha256sums(&sums, &name)
        .ok_or_else(|| anyhow!("SHA256SUMS for v{version} does not list {name}"))?;
    let archive = get(&archive_url).context("downloading the release archive")?;
    let actual = sha256_hex(&archive);
    if actual != expected {
        bail!("{name} does not match SHA256SUMS (got {actual}, expected {expected}); refusing it");
    }
    let bytes = unpack_binary(&archive, triple)?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&binary, &bytes)?;
    std::fs::write(&recorded, sha256_hex(&bytes))?;
    std::fs::write(&signed, signature.as_str())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok((binary, signature))
}

/// The release has no artifact at that URL (404): not an error to report,
/// just nothing to install, so the caller builds instead and says so plainly.
#[derive(Debug)]
pub struct NotPublished(pub String);

impl std::fmt::Display for NotPublished {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} is not published", self.0)
    }
}

impl std::error::Error for NotPublished {}

/// Whether `err` (anywhere in its chain) is a [`NotPublished`].
pub fn is_not_published(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|e| e.downcast_ref::<NotPublished>().is_some())
}

/// Fetch `url` over HTTPS, capped at [`MAX_ARCHIVE_BYTES`].
pub fn http_get(url: &str) -> Result<Vec<u8>> {
    let resp = match ureq::get(url)
        .timeout(std::time::Duration::from_secs(60))
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) => return Err(NotPublished(url.to_string()).into()),
        Err(e) => return Err(anyhow::Error::new(e).context(format!("GET {url}"))),
    };
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(MAX_ARCHIVE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        bail!("{url} is larger than {MAX_ARCHIVE_BYTES} bytes; refusing it");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIPLE: &str = "aarch64-unknown-linux-musl";

    fn signed(_: &[u8], _: &[u8], _: &str) -> Result<Signature> {
        Ok(Signature::Verified)
    }

    fn archive_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn only_a_crates_io_install_or_a_release_binary_is_eligible() {
        assert!(eligible_with(
            "/home/u/.cargo/registry/src/index.crates.io-1/croft-software-0.1.9",
            true
        ));
        assert!(
            !eligible_with("/home/u/.cargo/git/checkouts/croft-abc/1234", true),
            "an arbitrary commit"
        );
        assert!(
            !eligible_with("/home/u/src/croft", true),
            "a source checkout cross-compiles"
        );
        assert!(
            eligible_with("/home/runner/work/croft/croft", false),
            "a release binary, built in a checkout that is not here"
        );
        let tmp = tempfile::tempdir().unwrap();
        assert!(!eligible(&tmp.path().display().to_string()), "it exists");
        assert!(eligible(&tmp.path().join("gone").display().to_string()));
    }

    #[test]
    fn sha256sums_lines_are_found_by_name_in_either_mode() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  croft-x.tar.gz\n{b} *croft-y.tar.gz\n");
        assert_eq!(parse_sha256sums(&sums, "croft-x.tar.gz"), Some(a));
        assert_eq!(
            parse_sha256sums(&sums, "croft-y.tar.gz"),
            Some("b".repeat(64))
        );
        assert_eq!(parse_sha256sums(&sums, "croft-z.tar.gz"), None);
        assert_eq!(
            parse_sha256sums("nothex  croft-x.tar.gz", "croft-x.tar.gz"),
            None
        );
    }

    /// The binary comes out of the release layout, verified; a second
    /// connect reuses the cache without fetching again.
    #[test]
    fn prepare_verifies_extracts_and_caches_the_binary() {
        let archive = archive_with(&[
            (&format!("croft-{TRIPLE}/croft"), b"BINARY"),
            (&format!("croft-{TRIPLE}/README.md"), b"r"),
        ]);
        let sums = format!("{}  croft-{TRIPLE}.tar.gz\n", sha256_hex(&archive));
        let tmp = tempfile::tempdir().unwrap();
        let fetches = std::cell::Cell::new(0);
        let get = |url: &str| -> Result<Vec<u8>> {
            fetches.set(fetches.get() + 1);
            if url.ends_with("SHA256SUMS") {
                Ok(sums.clone().into_bytes())
            } else {
                Ok(archive.clone())
            }
        };
        let (path, sig) = prepare("0.1.9", TRIPLE, tmp.path(), &get, &signed).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"BINARY");
        assert_eq!(sig, Signature::Verified);
        assert_eq!(fetches.get(), 3, "SHA256SUMS, its bundle, the archive");
        prepare("0.1.9", TRIPLE, tmp.path(), &get, &signed).unwrap();
        assert_eq!(fetches.get(), 3, "the verified cache is reused");
    }

    #[test]
    fn a_checksum_mismatch_is_refused_and_nothing_is_cached() {
        let archive = archive_with(&[(&format!("croft-{TRIPLE}/croft"), b"BINARY")]);
        let sums = format!("{}  croft-{TRIPLE}.tar.gz\n", "0".repeat(64));
        let tmp = tempfile::tempdir().unwrap();
        let get = |url: &str| -> Result<Vec<u8>> {
            if url.ends_with("SHA256SUMS") {
                Ok(sums.clone().into_bytes())
            } else {
                Ok(archive.clone())
            }
        };
        let err = prepare("0.1.9", TRIPLE, tmp.path(), &get, &signed).unwrap_err();
        assert!(
            err.to_string().contains("does not match SHA256SUMS"),
            "{err}"
        );
        assert!(
            !tmp.path()
                .join("v0.1.9")
                .join(TRIPLE)
                .join("croft")
                .exists()
        );
    }

    /// A release with no artifacts (a 404) is told apart from a real
    /// failure, through the context `prepare` adds.
    #[test]
    fn a_missing_release_is_not_published_rather_than_a_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = |url: &str| -> Result<Vec<u8>> { Err(NotPublished(url.to_string()).into()) };
        let err = prepare("0.1.9", TRIPLE, tmp.path(), &missing, &signed).unwrap_err();
        assert!(is_not_published(&err), "{err:#}");
        let broken = |_: &str| -> Result<Vec<u8>> { Err(anyhow!("connection reset")) };
        let err = prepare("0.1.9", TRIPLE, tmp.path(), &broken, &signed).unwrap_err();
        assert!(!is_not_published(&err), "{err:#}");
    }

    #[test]
    fn an_archive_without_the_binary_or_with_a_link_is_refused() {
        let empty = archive_with(&[(&format!("croft-{TRIPLE}/README.md"), b"r")]);
        assert!(
            unpack_binary(&empty, TRIPLE)
                .unwrap_err()
                .to_string()
                .contains("has no")
        );

        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_link(&mut header, format!("croft-{TRIPLE}/croft"), "/etc/passwd")
            .unwrap();
        let tar = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        let linked = gz.finish().unwrap();
        assert!(
            unpack_binary(&linked, TRIPLE)
                .unwrap_err()
                .to_string()
                .contains("link entry")
        );
    }
    /// #261: a `SHA256SUMS` whose signature does not verify is refused
    /// before its sums are trusted: the archive is never fetched and nothing
    /// is cached to ship.
    #[test]
    fn a_bad_signature_is_refused_before_the_archive_is_fetched() {
        let tmp = tempfile::tempdir().unwrap();
        let fetched = std::cell::RefCell::new(Vec::new());
        let get = |url: &str| -> Result<Vec<u8>> {
            fetched.borrow_mut().push(url.to_string());
            Ok(b"sums or bundle".to_vec())
        };
        let forged = |_: &[u8], _: &[u8], _: &str| -> Result<Signature> {
            bail!("cosign did not verify it (none of the expected identities matched)")
        };
        let err = prepare("0.1.9", TRIPLE, tmp.path(), &get, &forged).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("signature is not the release's"), "{msg}");
        assert!(msg.contains("none of the expected identities"), "{msg}");
        assert!(
            fetched.borrow().iter().all(|u| !u.ends_with(".tar.gz")),
            "the archive is not downloaded: {:?}",
            fetched.borrow()
        );
        assert!(!tmp.path().join("v0.1.9").exists(), "nothing is cached");
    }

    /// A release whose bundle is missing is refused, not treated as a
    /// release with no binary: release.yml publishes the two together.
    #[test]
    fn a_missing_signature_bundle_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let get = |url: &str| -> Result<Vec<u8>> {
            if url.ends_with(".sigstore.json") {
                Err(NotPublished(url.to_string()).into())
            } else {
                Ok(b"sums".to_vec())
            }
        };
        let err = prepare("0.1.9", TRIPLE, tmp.path(), &get, &signed).unwrap_err();
        assert!(!is_not_published(&err), "a refusal, not a skip: {err:#}");
        assert!(err.to_string().contains("no signature bundle"), "{err:#}");
    }

    /// The verifier is handed the bytes that were fetched and the version,
    /// and a binary cached before its signature could be checked is checked
    /// again on the next connect rather than trusted from the cache.
    #[test]
    fn an_unchecked_cache_is_verified_again_once_it_can_be() {
        let archive = archive_with(&[(&format!("croft-{TRIPLE}/croft"), b"BINARY")]);
        let sums = format!("{}  croft-{TRIPLE}.tar.gz\n", sha256_hex(&archive));
        let tmp = tempfile::tempdir().unwrap();
        let fetches = std::cell::Cell::new(0);
        let get = |url: &str| -> Result<Vec<u8>> {
            fetches.set(fetches.get() + 1);
            if url.ends_with("/SHA256SUMS") {
                Ok(sums.clone().into_bytes())
            } else if url.ends_with("/SHA256SUMS.sigstore.json") {
                Ok(b"BUNDLE".to_vec())
            } else {
                Ok(archive.clone())
            }
        };
        let seen = std::cell::RefCell::new(None);
        let no_cosign = |s: &[u8], b: &[u8], v: &str| -> Result<Signature> {
            *seen.borrow_mut() = Some((s.to_vec(), b.to_vec(), v.to_string()));
            Ok(Signature::Unchecked)
        };
        let (_, sig) = prepare("0.1.9", TRIPLE, tmp.path(), &get, &no_cosign).unwrap();
        assert_eq!(sig, Signature::Unchecked);
        assert_eq!(
            seen.borrow().clone(),
            Some((
                sums.clone().into_bytes(),
                b"BUNDLE".to_vec(),
                String::from("0.1.9")
            ))
        );
        let (_, sig) = prepare("0.1.9", TRIPLE, tmp.path(), &get, &signed).unwrap();
        assert_eq!(sig, Signature::Verified);
        assert_eq!(fetches.get(), 6, "the unchecked cache was not trusted");
        prepare("0.1.9", TRIPLE, tmp.path(), &get, &signed).unwrap();
        assert_eq!(fetches.get(), 6, "the verified one is");
    }

    /// The identity cosign is told to require names this repository's
    /// release workflow at this version's tag, the identity release.yml
    /// verifies its own signature with.
    #[test]
    fn the_release_identity_is_the_workflow_at_the_versions_tag() {
        assert_eq!(
            release_identity("0.1.9"),
            "https://github.com/vitali87/croft/.github/workflows/release.yml@refs/tags/v0.1.9"
        );
        assert_eq!(
            bundle_url("0.1.9"),
            format!("{RELEASES}/v0.1.9/SHA256SUMS.sigstore.json")
        );
    }
}
