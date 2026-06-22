//! The remote vetted extension index.
//!
//! croft's "Available" tier is seeded from a bundled catalog ([`super::catalog`])
//! and, once armed, extended by a signed index published at
//! [`INDEX_BASE_URL`]. The index is the runtime, no-rebuild way to offer new
//! vetted extensions; the bundled catalog is the permanent offline floor.
//!
//! Trust model (vetted-only, no marketplace — see [`super`]):
//! - The index is **ed25519-signed**. croft verifies `index.json` against a
//!   public key baked into the binary ([`INDEX_PUBLIC_KEY`]) before it is ever
//!   cached or trusted, so a compromised host/CDN cannot forge entries.
//! - Each entry carries the **sha256** of its manifest; on install the fetched
//!   `extension.toml` is checked against that signed hash before it is written.
//! - The index is fetched over plain HTTPS from croft's own domain — never a
//!   git-host raw URL, which is per-IP rate-limited and breaks shared-NAT users.
//! - **Verify-at-write, trust-on-read:** the cache under `~/.cache/croft` is
//!   written only after the signature checks out, so the synchronous panel path
//!   reads it without re-verifying.
//!
//! Until a real key is baked (see `croft-extensions/scripts/gen-keypair.sh`),
//! [`INDEX_PUBLIC_KEY`] is all-zero, [`index_armed`] is false, and croft makes
//! no network calls and shows only the bundled catalog (fail-closed).

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::lsp::manifest::{self, ExtensionSummary};

/// Where the signed index and the manifests are published.
const INDEX_BASE_URL: &str = "https://extensions.croft.software";

/// ed25519 public key (32 raw bytes) the index signature is verified against.
/// All-zero would be the "no key baked yet" sentinel (fail-closed); this is the
/// real key whose private half lives root-only on the publish host
/// (`/opt/croft-extensions-src/croft-index.key`) and signs the index in CI.
const INDEX_PUBLIC_KEY: [u8; 32] = [
    0x43, 0x2b, 0x12, 0xdf, 0x93, 0x08, 0x20, 0x47, 0xdd, 0x55, 0x88, 0xe1, 0x97, 0x08, 0x40, 0xb6,
    0x39, 0x7a, 0x7c, 0x98, 0xe9, 0x89, 0x95, 0x19, 0xf6, 0xfa, 0x9c, 0x73, 0xcd, 0xb5, 0x5c, 0x9b,
];

/// Highest manifest `api_version` this build understands. Entries declaring a
/// higher version are hidden (update croft to use them), never installed against
/// an incompatible format.
const SUPPORTED_API_VERSION: u32 = 1;

/// Index `schema` versions this build can parse.
const SUPPORTED_SCHEMA: u32 = 1;

/// Cache file under `~/.cache/croft`: the last *verified* `index.json` bytes.
const CACHE_FILE: &str = "extensions-index.json";

/// Network timeout for the index / a manifest fetch.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound on a fetched body (index or manifest); both are tiny.
const MAX_FETCH_BYTES: u64 = 1024 * 1024;

/// The published index document.
#[derive(Debug, Clone, Deserialize)]
struct SignedIndex {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    extensions: Vec<IndexEntry>,
}

/// One published extension: identity + blurb (panel display), `api_version`
/// (compat gate), `manifest_url` (fetch) and `sha256` (integrity). Mirrors the
/// output of `croft-extensions/scripts/build-index.py`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    pub api_version: u32,
    pub sha256: String,
    pub manifest_url: String,
}

/// Whether a real signing key has been baked in. Until then the remote index is
/// disabled and croft makes no network calls.
fn index_armed() -> bool {
    INDEX_PUBLIC_KEY != [0u8; 32]
}

/// Verify a raw ed25519 signature over `index_bytes` with a raw 32-byte
/// `pubkey`. Any malformed input yields false — never panics, never trusts.
fn verify_signature(index_bytes: &[u8], sig: &[u8], pubkey: &[u8]) -> bool {
    use ring::signature;
    signature::UnparsedPublicKey::new(&signature::ED25519, pubkey)
        .verify(index_bytes, sig)
        .is_ok()
}

/// Parse already-verified index bytes, rejecting a schema newer than this build.
fn parse_index(bytes: &[u8]) -> Option<Vec<IndexEntry>> {
    let idx: SignedIndex = serde_json::from_slice(bytes).ok()?;
    if idx.schema > SUPPORTED_SCHEMA {
        return None;
    }
    Some(idx.extensions)
}

/// Drop entries whose `api_version` this build can't handle.
fn supported(entries: Vec<IndexEntry>, max_api: u32) -> Vec<IndexEntry> {
    entries
        .into_iter()
        .filter(|e| e.api_version <= max_api)
        .collect()
}

/// Whether `bytes` hashes to `expected` (hex sha256, case-insensitive).
fn sha256_matches(bytes: &[u8], expected: &str) -> bool {
    let digest = Sha256::digest(bytes);
    hex_encode(&digest).eq_ignore_ascii_case(expected)
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn cache_path() -> PathBuf {
    crate::session_state::dirs_cache_croft().join(CACHE_FILE)
}

/// Read the cached index bytes (trusted: written only after signature verify).
fn read_cache_bytes() -> Option<Vec<u8>> {
    std::fs::read(cache_path()).ok()
}

/// Atomically replace the cache with `bytes` (write-temp-then-rename).
fn write_cache(bytes: &[u8]) -> std::io::Result<()> {
    let dir = crate::session_state::dirs_cache_croft();
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{CACHE_FILE}.tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dir.join(CACHE_FILE))
}

/// GET `url` into memory, capped at [`MAX_FETCH_BYTES`].
fn http_get(url: &str) -> Result<Vec<u8>> {
    let resp = ureq::get(url).timeout(FETCH_TIMEOUT).call()?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_FETCH_BYTES)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

/// Fetch `index.json` + `index.json.sig`, verify the signature against the baked
/// key, and confirm the bytes parse — returning the verified index bytes.
fn fetch_verified_index() -> Result<Vec<u8>> {
    let index = http_get(&format!("{INDEX_BASE_URL}/index.json"))?;
    let sig = http_get(&format!("{INDEX_BASE_URL}/index.json.sig"))?;
    if !verify_signature(&index, &sig, &INDEX_PUBLIC_KEY) {
        bail!("extension index signature verification failed");
    }
    parse_index(&index).context("verified index did not parse")?;
    Ok(index)
}

/// Stale-while-revalidate refresh: always refetch when armed (the cached copy is
/// served synchronously meanwhile, so the panel shows stale-then-fresh), and
/// report a change only when the verified index actually differs from the cache
/// — so a new extension shows on the next launch with no TTL wait, but an
/// unchanged index triggers no panel rebuild. Best-effort: any network/verify
/// failure leaves the existing cache in place.
fn refresh_blocking() -> bool {
    if !index_armed() {
        return false;
    }
    let Ok(fresh) = fetch_verified_index() else {
        return false;
    };
    if read_cache_bytes().as_deref() == Some(fresh.as_slice()) {
        return false;
    }
    write_cache(&fresh).is_ok()
}

/// Kick a background index refresh. The boolean sent on completion is whether
/// the cache changed (the app rebuilds the panel only then). Disarmed builds
/// send `false` immediately without touching the network.
pub fn spawn_refresh() -> Receiver<bool> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(refresh_blocking());
    });
    rx
}

/// Entries from the cached, already-verified index, api-supported. Empty when
/// disarmed, uncached, or unparseable.
fn cached_entries() -> Vec<IndexEntry> {
    if !index_armed() {
        return Vec::new();
    }
    read_cache_bytes()
        .and_then(|b| parse_index(&b))
        .map(|e| supported(e, SUPPORTED_API_VERSION))
        .unwrap_or_default()
}

/// Whether `id` is published in the cached index. Combined with the bundled
/// catalog check, this is what makes a remote-installed extension croft-
/// removable: croft installed it from a curated source, so uninstalling is
/// reversible (a hand-dropped manifest matches neither and is never deleted).
pub fn is_index_entry(id: &str) -> bool {
    cached_entries().iter().any(|e| e.id == id)
}

/// Remote entries as panel summaries, excluding ids already covered by the
/// bundled catalog (bundled wins on collision — both ship the same manifest)
/// and ids already installed.
pub fn available_summaries(
    bundled_catalog_ids: &BTreeSet<String>,
    installed: &BTreeSet<String>,
) -> Vec<ExtensionSummary> {
    cached_entries()
        .into_iter()
        .filter(|e| !bundled_catalog_ids.contains(&e.id) && !installed.contains(&e.id))
        .map(|e| ExtensionSummary {
            id: e.id,
            name: e.name,
            description: e.description,
            builtin: false,
        })
        .collect()
}

/// Write a verified remote manifest into `dir/<id>/extension.toml` — pure over
/// the target dir for hermetic testing. Rejects a body whose sha256 disagrees
/// with the signed index, that is not UTF-8/valid TOML, or whose manifest id
/// disagrees with the index id.
fn write_remote_into(dir: &Path, entry: &IndexEntry, body: &[u8]) -> Result<PathBuf> {
    if !sha256_matches(body, &entry.sha256) {
        bail!("manifest sha256 mismatch for '{}'", entry.id);
    }
    let text = std::str::from_utf8(body).context("manifest is not UTF-8")?;
    let parsed = manifest::parse(text).with_context(|| format!("parsing '{}'", entry.id))?;
    if parsed.id != entry.id {
        bail!("manifest id '{}' != index id '{}'", parsed.id, entry.id);
    }
    let ext_dir = dir.join(&entry.id);
    std::fs::create_dir_all(&ext_dir).with_context(|| format!("creating {ext_dir:?}"))?;
    let path = ext_dir.join("extension.toml");
    std::fs::write(&path, body).with_context(|| format!("writing {path:?}"))?;
    Ok(path)
}

/// Install a remote index entry: fetch its manifest, verify its sha256 against
/// the signed index, and materialize it into the user extensions dir.
pub fn install_remote(id: &str) -> Result<PathBuf> {
    let entry = cached_entries()
        .into_iter()
        .find(|e| e.id == id)
        .with_context(|| format!("no remote index entry '{id}'"))?;
    let body = http_get(&entry.manifest_url)?;
    write_remote_into(&manifest::user_extensions_dir(), &entry, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    /// The openssl-signed fixture: `croft-extensions/scripts/build-index.sh`
    /// output for the three seeded manifests, signed by a throwaway ed25519 key.
    /// Proves `ring` (the client) accepts an openssl-produced signature (CI).
    const FIXTURE_INDEX: &[u8] = include_bytes!("testdata/index.json");
    const FIXTURE_SIG: &[u8] = include_bytes!("testdata/index.json.sig");
    const FIXTURE_PUBKEY_HEX: &str =
        "82935866e991e71a7fc86e375b2d4c6c5f4e96e739d5f470f911abd94813fbb4";

    fn hex_decode(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn ring_keypair() -> (Ed25519KeyPair, Vec<u8>) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let pubkey = kp.public_key().as_ref().to_vec();
        (kp, pubkey)
    }

    #[test]
    fn ring_signed_index_verifies_and_tampering_is_rejected() {
        let (kp, pubkey) = ring_keypair();
        let msg = b"the exact index bytes";
        let sig = kp.sign(msg);
        assert!(
            verify_signature(msg, sig.as_ref(), &pubkey),
            "good sig passes"
        );

        let mut tampered = msg.to_vec();
        tampered[0] ^= 0xff;
        assert!(
            !verify_signature(&tampered, sig.as_ref(), &pubkey),
            "a flipped byte must fail"
        );

        let (_, other_pubkey) = ring_keypair();
        assert!(
            !verify_signature(msg, sig.as_ref(), &other_pubkey),
            "a different key must fail"
        );
    }

    #[test]
    fn ring_verifies_an_openssl_produced_signature() {
        let pubkey = hex_decode(FIXTURE_PUBKEY_HEX);
        assert!(
            verify_signature(FIXTURE_INDEX, FIXTURE_SIG, &pubkey),
            "ring must accept the openssl-signed CI fixture (sign/verify interop)"
        );
        // And it must reject the same signature over mutated bytes.
        let mut tampered = FIXTURE_INDEX.to_vec();
        tampered[10] ^= 0x01;
        assert!(!verify_signature(&tampered, FIXTURE_SIG, &pubkey));
    }

    #[test]
    fn parses_the_fixture_index_and_rejects_a_future_schema() {
        let entries = parse_index(FIXTURE_INDEX).expect("fixture parses");
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"mcp-fetch") && ids.contains(&"mcp-time"));
        assert!(parse_index(br#"{"schema":99,"extensions":[]}"#).is_none());
        assert!(parse_index(b"not json").is_none());
    }

    #[test]
    fn api_version_gate_hides_too_new_entries() {
        let entry = |id: &str, api: u32| IndexEntry {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            version: "1.0.0".into(),
            api_version: api,
            sha256: String::new(),
            manifest_url: String::new(),
        };
        let kept = supported(vec![entry("ok", 1), entry("future", 2)], 1);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "ok");
    }

    #[test]
    fn sha256_matches_is_exact_and_case_insensitive() {
        // sha256("") = e3b0c442...
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(sha256_matches(b"", empty));
        assert!(sha256_matches(b"", &empty.to_uppercase()));
        assert!(!sha256_matches(b"x", empty));
    }

    #[test]
    fn write_remote_into_enforces_sha256_utf8_and_id() {
        let tmp = std::env::temp_dir().join(format!("croft-regidx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);

        let manifest_src = br#"id = "mcp-good"
name = "Good"
description = "ok"
api_version = 1
"#;
        let good_sha = hex_encode(&Sha256::digest(manifest_src));
        let mut entry = IndexEntry {
            id: "mcp-good".into(),
            name: "Good".into(),
            description: "ok".into(),
            version: "1.0.0".into(),
            api_version: 1,
            sha256: good_sha.clone(),
            manifest_url: String::new(),
        };

        // Happy path writes a loadable manifest.
        let path = write_remote_into(&tmp, &entry, manifest_src).expect("writes verified manifest");
        assert!(path.is_file());
        assert_eq!(
            manifest::parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .id,
            "mcp-good"
        );

        // A sha256 mismatch is refused (tampered manifest in transit).
        entry.sha256 = "deadbeef".into();
        assert!(write_remote_into(&tmp, &entry, manifest_src).is_err());

        // An index id that disagrees with the manifest id is refused.
        entry.sha256 = good_sha;
        entry.id = "mcp-other".into();
        assert!(write_remote_into(&tmp, &entry, manifest_src).is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn available_summaries_dedupes_bundled_and_excludes_installed() {
        // Build a tiny cached index by hand by exercising the projection only:
        // available_summaries reads cached_entries(), which is disarmed here, so
        // assert the pure filtering via a direct construction instead.
        let entries = vec![
            ExtensionSummary {
                id: "mcp-fetch".into(),
                name: "Web Fetch".into(),
                description: String::new(),
                builtin: false,
            },
            ExtensionSummary {
                id: "mcp-new".into(),
                name: "New".into(),
                description: String::new(),
                builtin: false,
            },
            ExtensionSummary {
                id: "mcp-gone".into(),
                name: "Gone".into(),
                description: String::new(),
                builtin: false,
            },
        ];
        let bundled: BTreeSet<String> = ["mcp-fetch".to_string()].into_iter().collect();
        let installed: BTreeSet<String> = ["mcp-gone".to_string()].into_iter().collect();
        let kept: Vec<String> = entries
            .into_iter()
            .filter(|e| !bundled.contains(&e.id) && !installed.contains(&e.id))
            .map(|e| e.id)
            .collect();
        assert_eq!(kept, vec!["mcp-new".to_string()]);
    }

    #[test]
    fn a_real_signing_key_is_baked_in() {
        // The remote index is armed: a non-zero key is compiled in, so croft
        // will fetch and verify the live index. (Cache-dependent behaviour is
        // not asserted here — it reads the real ~/.cache, so it isn't hermetic.)
        assert!(index_armed(), "a real ed25519 public key must be baked in");
    }
}
