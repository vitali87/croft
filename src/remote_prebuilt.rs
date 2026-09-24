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

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

const RELEASES: &str = "https://github.com/vitali87/croft/releases/download";

/// Largest archive accepted: the release tarballs are a few MB compressed.
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

/// Whether this croft may use a release artifact for the remote: installed
/// from crates.io (its source is the published crate). `cargo install --git`
/// checkouts are arbitrary commits, not releases, so they do not qualify.
pub fn eligible(manifest_dir: &str) -> bool {
    manifest_dir.contains("/registry/src/")
}

/// The archive and checksum URLs for `version` (no `v`) and `triple`.
pub fn release_urls(version: &str, triple: &str) -> (String, String) {
    (
        format!("{RELEASES}/v{version}/croft-{triple}.tar.gz"),
        format!("{RELEASES}/v{version}/SHA256SUMS"),
    )
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
/// connect), verified, and return its path. `get` fetches a URL's bytes
/// (injected so tests need no network).
pub fn prepare(
    version: &str,
    triple: &str,
    cache: &Path,
    get: &dyn Fn(&str) -> Result<Vec<u8>>,
) -> Result<PathBuf> {
    let dir = cache.join(format!("v{version}")).join(triple);
    let binary = dir.join("croft");
    let recorded = dir.join("croft.sha256");
    // A cached binary is reused only when it still hashes to what was
    // recorded when it was verified, so a truncated or edited file is fetched
    // again rather than shipped.
    if let (Ok(bytes), Ok(expected)) = (std::fs::read(&binary), std::fs::read_to_string(&recorded))
        && sha256_hex(&bytes) == expected.trim()
    {
        return Ok(binary);
    }
    let (archive_url, sums_url) = release_urls(version, triple);
    let sums = String::from_utf8(get(&sums_url).context("fetching SHA256SUMS")?)
        .context("SHA256SUMS is not text")?;
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
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(binary)
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
    fn only_a_crates_io_install_is_eligible() {
        assert!(eligible(
            "/home/u/.cargo/registry/src/index.crates.io-1/croft-software-0.1.9"
        ));
        assert!(
            !eligible("/home/u/.cargo/git/checkouts/croft-abc/1234"),
            "an arbitrary commit"
        );
        assert!(
            !eligible("/home/u/src/croft"),
            "a source checkout cross-compiles"
        );
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
        let path = prepare("0.1.9", TRIPLE, tmp.path(), &get).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"BINARY");
        assert_eq!(fetches.get(), 2);
        prepare("0.1.9", TRIPLE, tmp.path(), &get).unwrap();
        assert_eq!(fetches.get(), 2, "the verified cache is reused");
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
        let err = prepare("0.1.9", TRIPLE, tmp.path(), &get).unwrap_err();
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
        let err = prepare("0.1.9", TRIPLE, tmp.path(), &missing).unwrap_err();
        assert!(is_not_published(&err), "{err:#}");
        let broken = |_: &str| -> Result<Vec<u8>> { Err(anyhow!("connection reset")) };
        let err = prepare("0.1.9", TRIPLE, tmp.path(), &broken).unwrap_err();
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
}
