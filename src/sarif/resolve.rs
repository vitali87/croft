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

fn percent_decode(s: &str) -> String {
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

        // 5. A unique file name.
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
}
