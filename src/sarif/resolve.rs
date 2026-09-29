//! Artifact locations (§3.4) mapped to files on this machine.
//!
//! A log is usually written somewhere else: a CI runner, a container, another
//! developer's checkout. Resolution tries, in order:
//!
//! 1. an end-user mapping for the location's `uriBaseId` (§3.4.4 step 1);
//! 2. the run's `originalUriBaseIds`, followed through nested bases;
//! 3. a relative path joined onto each workspace root;
//! 4. prefixes learned from earlier successful matches (including ones the
//!    user picked by hand with Locate…);
//! 5. a file name that is unique in the workspace.
//!
//! Every candidate is checked for existence through a caller-supplied probe,
//! so the logic is testable without touching the disk.

use super::model::{ArtifactLocation, Run};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// A location's URI after `index` lookup and `uriBaseId` expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    /// Absolute when `absolute`, otherwise a relative reference.
    pub uri: String,
    pub absolute: bool,
    /// The base id that could not be expanded (missing from
    /// `originalUriBaseIds`, or given there without a `uri`).
    pub unresolved_base: Option<String>,
}

/// `uri` and `uriBaseId` of a location, falling back to `run.artifacts[index]`.
pub fn location_parts(run: &Run, loc: &ArtifactLocation) -> Option<(String, Option<String>)> {
    let artifact = loc
        .index
        .and_then(|i| usize::try_from(i).ok())
        .and_then(|i| run.artifacts.get(i))
        .and_then(|a| a.location.as_ref());
    let uri = loc
        .uri
        .clone()
        .or_else(|| artifact.and_then(|a| a.uri.clone()))?;
    let base = if loc.uri.is_some() {
        loc.uri_base_id.clone()
    } else {
        loc.uri_base_id
            .clone()
            .or_else(|| artifact.and_then(|a| a.uri_base_id.clone()))
    };
    Some((uri, base))
}

/// A file's contents carried in the log itself (§3.24.8, `artifacts[].contents`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Embedded {
    Text(String),
    Binary(Vec<u8>),
}

/// The contents the log embeds for `loc`'s artifact (#577): the artifact
/// `loc.index` names, else the one whose location has the same `uri` and
/// `uriBaseId`. Text is preferred to binary; binary is base64 (§3.3.2).
pub fn embedded_contents(run: &Run, loc: &ArtifactLocation) -> Option<Embedded> {
    use base64::Engine;
    let by_index = loc
        .index
        .and_then(|i| usize::try_from(i).ok())
        .and_then(|i| run.artifacts.get(i));
    let by_uri = || {
        let uri = loc.uri.as_deref()?;
        run.artifacts.iter().find(|a| {
            a.location
                .as_ref()
                .is_some_and(|l| l.uri.as_deref() == Some(uri) && l.uri_base_id == loc.uri_base_id)
        })
    };
    let contents = by_index
        .filter(|a| a.contents.is_some())
        .or_else(by_uri)?
        .contents
        .as_ref()?;
    if let Some(text) = &contents.text {
        return Some(Embedded::Text(text.clone()));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(contents.binary.as_deref()?.trim())
        .ok()?;
    Some(Embedded::Binary(bytes))
}

/// Whether `uri` carries a scheme (RFC 3986 §3.1), making it absolute. A
/// single letter before the colon is a Windows drive, not a scheme.
fn has_scheme(uri: &str) -> bool {
    match uri.find(':') {
        Some(i) if i > 1 => {
            let scheme = &uri[..i];
            scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        _ => false,
    }
}

/// §3.14.14: expand `uriBaseId` through `originalUriBaseIds` by
/// concatenation, following nested bases. A loop (forbidden by the spec but
/// possible in a hand-edited log) stops expansion rather than hanging.
pub fn expand(run: &Run, loc: &ArtifactLocation) -> Option<Expanded> {
    let (uri, base) = location_parts(run, loc)?;
    if has_scheme(&uri) {
        return Some(Expanded {
            uri,
            absolute: true,
            unresolved_base: None,
        });
    }
    let mut out = uri;
    let mut next = base;
    let mut seen = Vec::new();
    while let Some(id) = next.take() {
        let entry = run.original_uri_base_ids.get(&id);
        let prefix = entry.and_then(|e| e.uri.as_deref());
        match prefix {
            Some(p) if !seen.contains(&id) => {
                out = format!("{p}{out}");
                if has_scheme(&out) {
                    return Some(Expanded {
                        uri: out,
                        absolute: true,
                        unresolved_base: None,
                    });
                }
                seen.push(id);
                next = entry.and_then(|e| e.uri_base_id.clone());
                if next.is_none() {
                    // A relative base with no base of its own: nothing more
                    // the log can tell us.
                    return Some(Expanded {
                        uri: out,
                        absolute: false,
                        unresolved_base: seen.last().cloned(),
                    });
                }
            }
            _ => {
                return Some(Expanded {
                    uri: out,
                    absolute: false,
                    unresolved_base: Some(id),
                });
            }
        }
    }
    Some(Expanded {
        uri: out,
        absolute: false,
        unresolved_base: None,
    })
}

/// A `file:` URI or a plain relative reference as a path. Percent-escapes are
/// decoded. `None` for any other scheme.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    if has_scheme(uri) {
        let parsed = url::Url::parse(uri).ok()?;
        if parsed.scheme() != "file" {
            return None;
        }
        return parsed.to_file_path().ok();
    }
    Some(PathBuf::from(percent_decode(uri)))
}

pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(h) = s.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(h, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn prefixes_path() -> PathBuf {
    crate::app::croft_cache_dir().join("sarif-locations.json")
}

/// Path prefixes learned for `workspace` (from Locate… and earlier matches),
/// as the resolver's `learned` list. Empty when none were saved.
pub fn saved_prefixes(workspace: &Path) -> Vec<(String, PathBuf)> {
    let all: BTreeMap<String, Vec<(String, PathBuf)>> = std::fs::read_to_string(prefixes_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    all.get(&workspace.display().to_string())
        .cloned()
        .unwrap_or_default()
}

/// Remember that `artifact_uri` lives at `local` for `workspace`, as the
/// prefix pair [`Resolver::learn`] derives. Newest first, no duplicates.
pub fn save_prefix(workspace: &Path, artifact_uri: &str, local: &Path) -> std::io::Result<()> {
    let mut r = Resolver::default();
    r.learn(artifact_uri, local);
    let Some(pair) = r.learned.into_iter().next() else {
        return Ok(());
    };
    let path = prefixes_path();
    let mut all: BTreeMap<String, Vec<(String, PathBuf)>> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let list = all.entry(workspace.display().to_string()).or_default();
    list.retain(|(from, _)| *from != pair.0);
    list.insert(0, pair);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&all).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

/// The state resolution needs beyond the log itself.
#[derive(Debug, Default, Clone)]
pub struct Resolver {
    /// Workspace roots, most specific first.
    pub roots: Vec<PathBuf>,
    /// End-user `uriBaseId` → local directory.
    pub base_overrides: BTreeMap<String, PathBuf>,
    /// Learned `(artifact prefix, local prefix)` pairs.
    pub learned: Vec<(String, PathBuf)>,
    /// File name → every workspace path with that name.
    pub names: HashMap<String, Vec<PathBuf>>,
    /// Files open in the editor (#577). An artifact that resolves nowhere
    /// else is the one open document whose path ends with the artifact's
    /// path, compared by whole components, when exactly one does.
    pub open: Vec<PathBuf>,
}

impl Resolver {
    pub fn resolve(
        &self,
        run: &Run,
        loc: &ArtifactLocation,
        exists: &dyn Fn(&Path) -> bool,
    ) -> Option<PathBuf> {
        let found = |p: PathBuf| exists(&p).then_some(p);
        let (raw, base) = location_parts(run, loc)?;

        // 1. An end-user mapping for the base id.
        if let Some(dir) = base.as_ref().and_then(|b| self.base_overrides.get(b))
            && !has_scheme(&raw)
            && let Some(rel) = uri_to_path(&raw)
            && let Some(p) = found(dir.join(rel))
        {
            return Some(p);
        }

        // 2. The log's own bases.
        let expanded = expand(run, loc)?;
        if let Some(p) = uri_to_path(&expanded.uri) {
            if p.is_absolute() {
                if let Some(p) = found(p) {
                    return Some(p);
                }
            } else {
                // 3. Relative: try each workspace root.
                for root in &self.roots {
                    if let Some(p) = found(root.join(&p)) {
                        return Some(p);
                    }
                }
            }
        }

        // 4. Learned prefixes.
        for (from, to) in &self.learned {
            if let Some(rest) = expanded.uri.strip_prefix(from.as_str())
                && let Some(rel) = uri_to_path(rest)
                && let Some(p) = found(to.join(rel))
            {
                return Some(p);
            }
        }

        // 5. An open document ending with the artifact's path: the longest
        //    match wins, and a tie between two is no answer.
        let parts: Vec<String> = expanded
            .uri
            .split('/')
            .filter(|s| !s.is_empty() && !s.contains(':'))
            .map(percent_decode)
            .collect();
        let trailing = |p: &Path| {
            let comps: Vec<String> = p
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            comps
                .iter()
                .rev()
                .zip(parts.iter().rev())
                .take_while(|(a, b)| a == b)
                .count()
        };
        let mut best: Option<(usize, &PathBuf)> = None;
        let mut tied = false;
        for p in &self.open {
            let n = trailing(p);
            if n == 0 {
                continue;
            }
            match best {
                Some((m, _)) if n < m => {}
                Some((m, _)) if n == m => tied = true,
                _ => {
                    best = Some((n, p));
                    tied = false;
                }
            }
        }
        if let (Some((_, p)), false) = (best, tied)
            && let Some(p) = found(p.clone())
        {
            return Some(p);
        }

        // 6. A unique file name.
        let name = expanded.uri.rsplit('/').next().map(percent_decode)?;
        match self.names.get(&name).map(Vec::as_slice) {
            Some([only]) => found(only.clone()),
            _ => None,
        }
    }

    /// Record that `artifact_uri` lives at `local`. The longest common
    /// trailing run of path components is dropped from both, and the two
    /// remaining prefixes are remembered as equivalent.
    pub fn learn(&mut self, artifact_uri: &str, local: &Path) {
        let local = local.to_string_lossy().replace('\\', "/");
        let a: Vec<&str> = artifact_uri.split('/').collect();
        let l: Vec<&str> = local.split('/').collect();
        let common = a
            .iter()
            .rev()
            .zip(l.iter().rev())
            .take_while(|(x, y)| percent_decode(x) == **y)
            .count();
        if common == 0 || common >= a.len() || common >= l.len() {
            return;
        }
        let from = format!("{}/", a[..a.len() - common].join("/"));
        let to = PathBuf::from(format!("{}/", l[..l.len() - common].join("/")));
        if !self.learned.iter().any(|(f, _)| *f == from) {
            self.learned.insert(0, (from, to));
        }
    }
}

/// Where `uri`, a location in `run`, sits on GitHub, when the run names a
/// GitHub repository and commit in its `versionControlProvenance`: the
/// case of a variant analysis, whose results are in other repositories.
/// `line` (1-based, 0 for none) becomes the URL's fragment.
pub fn github_blob_url(run: &Run, uri: &str, line: i64) -> Option<String> {
    let vcs = run.version_control_provenance.first()?;
    let repo = vcs
        .repository_uri
        .as_deref()?
        .strip_prefix("https://github.com/")?
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let rev = vcs.revision_id.as_deref().unwrap_or("HEAD");
    let path = uri
        .strip_prefix("file:///")
        .unwrap_or(uri)
        .trim_start_matches('/');
    if repo.is_empty() || path.is_empty() || path.contains("://") {
        return None;
    }
    let fragment = if line > 0 {
        format!("#L{line}")
    } else {
        String::new()
    };
    Some(format!(
        "https://github.com/{repo}/blob/{rev}/{path}{fragment}"
    ))
}

/// Where a missing file can be downloaded from (#577): the raw file at the
/// commit the run's `versionControlProvenance` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSource {
    /// The repository's host, which must be trusted before anything is
    /// fetched from it.
    pub host: String,
    pub url: String,
}

/// The raw-file URL of `loc` in the repository `run` was scanned from, for
/// GitHub, GitLab and Bitbucket repositories over https. The provenance
/// entry is the one whose `mappedTo` is `loc`'s base (§3.23.7), else the
/// run's only entry when it maps nothing; the path is `loc`'s relative
/// URI, less `mappedTo.uri` when that is set. The commit is `revisionId`,
/// else `branch`.
pub fn provenance_source(run: &Run, loc: &ArtifactLocation) -> Option<RemoteSource> {
    let (uri, base) = location_parts(run, loc)?;
    if uri.contains("://") || uri.starts_with('/') {
        return None;
    }
    let vcp = &run.version_control_provenance;
    let entry = vcp
        .iter()
        .find(|v| {
            v.mapped_to
                .as_ref()
                .is_some_and(|m| base.is_some() && m.uri_base_id == base)
        })
        .or_else(|| (vcp.len() == 1 && vcp[0].mapped_to.is_none()).then(|| &vcp[0]))?;
    let prefix = entry
        .mapped_to
        .as_ref()
        .and_then(|m| m.uri.as_deref())
        .unwrap_or("")
        .trim_start_matches("./");
    let path = uri.trim_start_matches("./");
    let path = if prefix.is_empty() {
        path
    } else {
        path.strip_prefix(prefix)?.trim_start_matches('/')
    };
    let rev = entry
        .revision_id
        .as_deref()
        .or(entry.branch.as_deref())
        .filter(|r| !r.is_empty())?;
    let rest = entry.repository_uri.as_deref()?.strip_prefix("https://")?;
    let (host, repo) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let repo = repo.trim_end_matches('/').trim_end_matches(".git");
    if path.is_empty() || repo.split('/').filter(|s| !s.is_empty()).count() < 2 {
        return None;
    }
    let url = match host.as_str() {
        "github.com" => format!("https://raw.githubusercontent.com/{repo}/{rev}/{path}"),
        "gitlab.com" => format!("https://gitlab.com/{repo}/-/raw/{rev}/{path}"),
        "bitbucket.org" => format!("https://bitbucket.org/{repo}/raw/{rev}/{path}"),
        _ => return None,
    };
    Some(RemoteSource { host, url })
}

fn trusted_hosts_path() -> PathBuf {
    crate::app::croft_cache_dir().join("sarif-trusted-hosts")
}

/// The hosts the user allowed croft to download SARIF source from.
pub fn trusted_hosts() -> Vec<String> {
    std::fs::read_to_string(trusted_hosts_path())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Allow downloads from `host` from now on.
pub fn trust_host(host: &str) -> std::io::Result<()> {
    let mut hosts = trusted_hosts();
    if hosts.iter().any(|h| h == host) {
        return Ok(());
    }
    hosts.push(host.to_string());
    let path = trusted_hosts_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, hosts.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn run(json: &str) -> Run {
        serde_json::from_str(json).unwrap()
    }

    fn loc(json: &str) -> ArtifactLocation {
        serde_json::from_str(json).unwrap()
    }

    fn disk(paths: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<PathBuf> = paths.iter().map(PathBuf::from).collect();
        move |p: &Path| set.contains(p)
    }

    #[test]
    fn provenance_source_builds_the_raw_url_at_the_scanned_commit() {
        let r = run(
            r#"{"tool":{"driver":{"name":"t"}},"versionControlProvenance":[
              {"repositoryUri":"https://github.com/o/r.git","revisionId":"abc123","mappedTo":{"uriBaseId":"SRCROOT"}}]}"#,
        );
        assert_eq!(
            provenance_source(&r, &loc(r#"{"uri":"src/a.rs","uriBaseId":"SRCROOT"}"#)),
            Some(RemoteSource {
                host: "github.com".into(),
                url: "https://raw.githubusercontent.com/o/r/abc123/src/a.rs".into()
            })
        );
        // Another base is not in that repository; an absolute URI never is.
        assert_eq!(
            provenance_source(&r, &loc(r#"{"uri":"a.rs","uriBaseId":"OTHER"}"#)),
            None
        );
        assert_eq!(
            provenance_source(
                &r,
                &loc(r#"{"uri":"file:///x/a.rs","uriBaseId":"SRCROOT"}"#)
            ),
            None
        );
    }

    #[test]
    fn provenance_source_takes_a_lone_unmapped_entry_and_strips_mapped_uri() {
        let lone = run(
            r#"{"tool":{"driver":{"name":"t"}},"versionControlProvenance":[
              {"repositoryUri":"https://gitlab.com/g/sub/p","branch":"main"}]}"#,
        );
        assert_eq!(
            provenance_source(&lone, &loc(r#"{"uri":"./lib/x.py"}"#)).map(|s| s.url),
            Some("https://gitlab.com/g/sub/p/-/raw/main/lib/x.py".into())
        );
        let mapped = run(
            r#"{"tool":{"driver":{"name":"t"}},"versionControlProvenance":[
              {"repositoryUri":"https://bitbucket.org/o/r","revisionId":"r1","mappedTo":{"uri":"repo/","uriBaseId":"WS"}}]}"#,
        );
        assert_eq!(
            provenance_source(&mapped, &loc(r#"{"uri":"repo/src/m.c","uriBaseId":"WS"}"#))
                .map(|s| s.url),
            Some("https://bitbucket.org/o/r/raw/r1/src/m.c".into())
        );
        let unknown_host = run(
            r#"{"tool":{"driver":{"name":"t"}},"versionControlProvenance":[
              {"repositoryUri":"https://git.example.com/o/r","revisionId":"r1"}]}"#,
        );
        assert_eq!(
            provenance_source(&unknown_host, &loc(r#"{"uri":"a.c"}"#)),
            None
        );
    }

    const BASES: &str = r#"{"originalUriBaseIds":{
        "PROJECTROOT":{"uri":"file:///C:/Users/Mary/code/TheProject/"},
        "SRCROOT":{"uri":"src/","uriBaseId":"PROJECTROOT"},
        "HIDDEN":{"description":{"text":"removed"}},
        "LOOP_A":{"uri":"a/","uriBaseId":"LOOP_B"},
        "LOOP_B":{"uri":"b/","uriBaseId":"LOOP_A"}
    },"artifacts":[{"location":{"uri":"lib/io.c","uriBaseId":"SRCROOT"}}]}"#;

    // ── expansion ───────────────────────────────────────────────────────

    #[test]
    fn nested_bases_concatenate() {
        let e = expand(
            &run(BASES),
            &loc(r#"{"uri":"main.c","uriBaseId":"SRCROOT"}"#),
        )
        .unwrap();
        assert_eq!(e.uri, "file:///C:/Users/Mary/code/TheProject/src/main.c");
        assert!(e.absolute);
        assert_eq!(e.unresolved_base, None);
    }

    #[test]
    fn index_supplies_uri_and_base() {
        let e = expand(&run(BASES), &loc(r#"{"index":0}"#)).unwrap();
        assert_eq!(e.uri, "file:///C:/Users/Mary/code/TheProject/src/lib/io.c");
    }

    #[test]
    fn base_without_uri_is_reported_unresolved() {
        let e = expand(&run(BASES), &loc(r#"{"uri":"x.c","uriBaseId":"HIDDEN"}"#)).unwrap();
        assert_eq!(e.uri, "x.c");
        assert!(!e.absolute);
        assert_eq!(e.unresolved_base.as_deref(), Some("HIDDEN"));
    }

    #[test]
    fn unknown_base_is_reported_unresolved() {
        let e = expand(
            &run(BASES),
            &loc(r#"{"uri":"x.c","uriBaseId":"%SRCROOT%"}"#),
        )
        .unwrap();
        assert_eq!(e.unresolved_base.as_deref(), Some("%SRCROOT%"));
    }

    #[test]
    fn base_loop_terminates() {
        let e = expand(&run(BASES), &loc(r#"{"uri":"x.c","uriBaseId":"LOOP_A"}"#)).unwrap();
        assert!(!e.absolute);
        assert!(e.unresolved_base.is_some());
    }

    #[test]
    fn absolute_uri_ignores_base() {
        let e = expand(
            &run(BASES),
            &loc(r#"{"uri":"file:///etc/hosts","uriBaseId":"SRCROOT"}"#),
        )
        .unwrap();
        assert_eq!(e.uri, "file:///etc/hosts");
        assert!(e.absolute);
    }

    #[test]
    fn no_uri_no_index_is_none() {
        assert!(expand(&run(BASES), &loc("{}")).is_none());
        assert!(expand(&run(BASES), &loc(r#"{"index":7}"#)).is_none());
    }

    // ── uri → path ──────────────────────────────────────────────────────

    #[test]
    fn file_uris_and_percent_escapes() {
        assert_eq!(
            uri_to_path("file:///home/me/my%20proj/a.rs"),
            Some(PathBuf::from("/home/me/my proj/a.rs"))
        );
        assert_eq!(
            uri_to_path("src/a%23b.rs"),
            Some(PathBuf::from("src/a#b.rs"))
        );
        assert_eq!(uri_to_path("https://example.com/a.rs"), None);
    }

    // ── resolution order ────────────────────────────────────────────────

    fn resolver(roots: &[&str]) -> Resolver {
        Resolver {
            roots: roots.iter().map(PathBuf::from).collect(),
            ..Resolver::default()
        }
    }

    #[test]
    fn absolute_file_that_exists_is_used() {
        let r = resolver(&["/ws"]);
        let run = run("{}");
        let got = r.resolve(
            &run,
            &loc(r#"{"uri":"file:///ws/a.rs"}"#),
            &disk(&["/ws/a.rs"]),
        );
        assert_eq!(got, Some(PathBuf::from("/ws/a.rs")));
    }

    #[test]
    fn relative_joins_workspace_root() {
        let r = resolver(&["/other", "/ws"]);
        let got = r.resolve(
            &run("{}"),
            &loc(r#"{"uri":"src/a.rs"}"#),
            &disk(&["/ws/src/a.rs"]),
        );
        assert_eq!(got, Some(PathBuf::from("/ws/src/a.rs")));
    }

    #[test]
    fn unresolved_base_still_tries_workspace_roots() {
        let r = resolver(&["/ws"]);
        let got = r.resolve(
            &run(r#"{"originalUriBaseIds":{"SRC":{}}}"#),
            &loc(r#"{"uri":"a.rs","uriBaseId":"SRC"}"#),
            &disk(&["/ws/a.rs"]),
        );
        assert_eq!(got, Some(PathBuf::from("/ws/a.rs")));
    }

    #[test]
    fn an_open_document_ending_with_the_path_resolves_it() {
        // The build ran elsewhere (/build/...); the file is open from a
        // checkout the roots do not cover (#577).
        let mut r = resolver(&["/ws"]);
        r.open = vec![
            PathBuf::from("/elsewhere/proj/src/db/query.rs"),
            PathBuf::from("/elsewhere/proj/src/api/query.rs"),
            PathBuf::from("/elsewhere/proj/README.md"),
        ];
        let files = [
            "/elsewhere/proj/src/db/query.rs",
            "/elsewhere/proj/src/api/query.rs",
            "/elsewhere/proj/README.md",
        ];
        let at = |uri: &str| {
            r.resolve(
                &run("{}"),
                &loc(&format!(r#"{{"uri":"{uri}"}}"#)),
                &disk(&files),
            )
        };
        assert_eq!(
            at("file:///build/proj/src/db/query.rs"),
            Some(PathBuf::from("/elsewhere/proj/src/db/query.rs")),
            "the longest trailing match"
        );
        assert_eq!(
            at("file:///build/other/query.rs"),
            None,
            "two tie on the name alone"
        );
        assert_eq!(at("file:///build/xyz.rs"), None, "nothing open matches");
        // Whole components: `ery.rs` is not a match for `query.rs`.
        r.open = vec![PathBuf::from("/elsewhere/proj/src/db/query.rs")];
        let at = |uri: &str| {
            r.resolve(
                &run("{}"),
                &loc(&format!(r#"{{"uri":"{uri}"}}"#)),
                &disk(&files),
            )
        };
        assert_eq!(at("file:///build/ery.rs"), None);
    }

    #[test]
    fn embedded_contents_are_found_by_index_or_uri() {
        let run = run(r#"{"artifacts":[
                {"location":{"uri":"gen/a.c"},"contents":{"text":"int a;"}},
                {"location":{"uri":"b.bin","uriBaseId":"SRC"},"contents":{"binary":"AAEC/w=="}},
                {"location":{"uri":"none.c"}}]}"#);
        assert_eq!(
            embedded_contents(&run, &loc(r#"{"index":0}"#)),
            Some(Embedded::Text(String::from("int a;")))
        );
        assert_eq!(
            embedded_contents(&run, &loc(r#"{"uri":"gen/a.c"}"#)),
            Some(Embedded::Text(String::from("int a;")))
        );
        assert_eq!(
            embedded_contents(&run, &loc(r#"{"uri":"b.bin","uriBaseId":"SRC"}"#)),
            Some(Embedded::Binary(vec![0, 1, 2, 255]))
        );
        assert_eq!(
            embedded_contents(&run, &loc(r#"{"uri":"b.bin"}"#)),
            None,
            "another base"
        );
        assert_eq!(
            embedded_contents(&run, &loc(r#"{"index":2}"#)),
            None,
            "no contents"
        );
        assert_eq!(embedded_contents(&run, &loc(r#"{"uri":"x.c"}"#)), None);
    }

    #[test]
    fn user_base_override_wins() {
        let mut r = resolver(&["/ws"]);
        r.base_overrides
            .insert("SRCROOT".into(), PathBuf::from("/mine/src"));
        let got = r.resolve(
            &run(BASES),
            &loc(r#"{"uri":"a.rs","uriBaseId":"SRCROOT"}"#),
            &disk(&["/mine/src/a.rs", "/ws/a.rs"]),
        );
        assert_eq!(got, Some(PathBuf::from("/mine/src/a.rs")));
    }

    #[test]
    fn ci_absolute_path_maps_by_learned_prefix() {
        let mut r = resolver(&["/ws"]);
        r.learn(
            "file:///home/runner/work/app/app/src/a.rs",
            Path::new("/ws/src/a.rs"),
        );
        let got = r.resolve(
            &run("{}"),
            &loc(r#"{"uri":"file:///home/runner/work/app/app/lib/b.rs"}"#),
            &disk(&["/ws/lib/b.rs"]),
        );
        assert_eq!(got, Some(PathBuf::from("/ws/lib/b.rs")));
    }

    #[test]
    fn learn_keeps_only_the_differing_prefix() {
        let mut r = Resolver::default();
        r.learn("file:///ci/build/src/x/y.rs", Path::new("/ws/src/x/y.rs"));
        assert_eq!(
            r.learned,
            vec![("file:///ci/build/".to_string(), PathBuf::from("/ws/"))]
        );
    }

    #[test]
    fn unique_file_name_is_a_last_resort() {
        let mut r = resolver(&["/ws"]);
        r.names
            .insert("only.rs".into(), vec![PathBuf::from("/ws/deep/only.rs")]);
        r.names.insert(
            "dup.rs".into(),
            vec![PathBuf::from("/ws/a/dup.rs"), PathBuf::from("/ws/b/dup.rs")],
        );
        let exists = disk(&["/ws/deep/only.rs", "/ws/a/dup.rs", "/ws/b/dup.rs"]);
        assert_eq!(
            r.resolve(
                &run("{}"),
                &loc(r#"{"uri":"file:///elsewhere/only.rs"}"#),
                &exists
            ),
            Some(PathBuf::from("/ws/deep/only.rs"))
        );
        assert_eq!(
            r.resolve(
                &run("{}"),
                &loc(r#"{"uri":"file:///elsewhere/dup.rs"}"#),
                &exists
            ),
            None
        );
    }

    #[test]
    fn nothing_matches_is_none() {
        let r = resolver(&["/ws"]);
        assert_eq!(
            r.resolve(&run("{}"), &loc(r#"{"uri":"nope.rs"}"#), &disk(&[])),
            None
        );
    }
    #[test]
    fn a_location_in_a_github_repository_links_to_its_commit() {
        use super::super::model::VersionControlDetails;
        let mut run = Run::default();
        assert_eq!(github_blob_url(&run, "src/a.py", 3), None, "no provenance");
        run.version_control_provenance = vec![VersionControlDetails {
            repository_uri: Some("https://github.com/a/b".into()),
            revision_id: Some("abc".into()),
            ..Default::default()
        }];
        assert_eq!(
            github_blob_url(&run, "src/a.py", 3).as_deref(),
            Some("https://github.com/a/b/blob/abc/src/a.py#L3")
        );
        assert_eq!(
            github_blob_url(&run, "/src/a.py", 0).as_deref(),
            Some("https://github.com/a/b/blob/abc/src/a.py")
        );
        assert_eq!(github_blob_url(&run, "https://x/y", 1), None);
        run.version_control_provenance[0].repository_uri = Some("https://gitlab.com/a/b".into());
        assert_eq!(github_blob_url(&run, "src/a.py", 3), None);
    }
}
