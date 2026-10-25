//! The curated MCP-server catalog: the "Available" tier of the Extensions
//! panel. Catalog entries (bundled in [`manifest::CATALOG_MANIFESTS`]) are
//! vetted sidecars the user can *add*; adding one writes its manifest into the
//! user extensions dir, after which it loads like any installed extension —
//! Available → Add → Installed → Enable → (provision on first use).
//!
//! This module owns only the data + the add action; the panel surfaces it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::lsp::manifest::{self, ExtensionSummary};

/// Every catalog entry's identity (id/name/description), unfiltered.
fn catalog_summaries() -> Vec<ExtensionSummary> {
    manifest::summaries(manifest::CATALOG_MANIFESTS)
}

/// The catalog manifest source whose declared id matches `id`, if any.
fn find_source(id: &str) -> Option<&'static str> {
    manifest::CATALOG_MANIFESTS
        .iter()
        .copied()
        .find(|s| manifest::parse(s).map(|m| m.id == id).unwrap_or(false))
}

/// Ids of the bundled built-ins, which are installed everywhere.
pub fn bundled_ids() -> BTreeSet<String> {
    manifest::summaries(manifest::BUNDLED_MANIFESTS)
        .into_iter()
        .map(|s| s.id)
        .collect()
}

/// Ids already installed: the bundled built-ins plus every manifest under the
/// user extensions dir. A catalog entry whose id is here is no longer "available".
pub fn installed_ids(user_dir: &Path) -> BTreeSet<String> {
    let mut ids = bundled_ids();
    for src in manifest::read_extension_sources(user_dir) {
        if let Ok(m) = manifest::parse(&src) {
            ids.insert(m.id);
        }
    }
    ids
}

/// Catalog entries not in `installed` — pure, for hermetic testing.
fn available_from(installed: &BTreeSet<String>) -> Vec<ExtensionSummary> {
    catalog_summaries()
        .into_iter()
        .filter(|s| !installed.contains(&s.id))
        .collect()
}

/// The entries available to add (not yet installed): the bundled catalog plus
/// the signed remote index, deduped (bundled wins) and minus what's installed.
/// Reads the real user extensions dir and the verified index cache.
pub fn available() -> Vec<ExtensionSummary> {
    let installed = installed_ids(&manifest::user_extensions_dir());
    let mut out = available_from(&installed);
    let bundled_catalog_ids: BTreeSet<String> =
        catalog_summaries().into_iter().map(|s| s.id).collect();
    out.extend(crate::mcp::registry_index::available_summaries(
        &bundled_catalog_ids,
        &installed,
    ));
    out
}

/// Write a catalog entry's manifest into `dir/<id>/extension.toml` — pure over
/// the target dir, for hermetic testing.
fn write_into(dir: &Path, id: &str) -> Result<PathBuf> {
    let source = find_source(id).with_context(|| format!("no catalog entry '{id}'"))?;
    let ext_dir = dir.join(id);
    std::fs::create_dir_all(&ext_dir).with_context(|| format!("creating {ext_dir:?}"))?;
    let path = ext_dir.join("extension.toml");
    std::fs::write(&path, source).with_context(|| format!("writing {path:?}"))?;
    Ok(path)
}

/// Add (install) an entry. A bundled catalog entry materializes from its
/// embedded manifest into `extensions_dir` (the app's own, as for
/// [`uninstall_in`]); a remote (signed-index) entry is fetched and its sha256
/// verified against the index before it is written to the user extensions
/// dir. Either way it loads like any installed extension on next refresh.
pub fn install_in(extensions_dir: &Path, id: &str) -> Result<PathBuf> {
    if find_source(id).is_some() {
        write_into(extensions_dir, id)
    } else {
        crate::mcp::registry_index::install_remote(id)
    }
}

/// Whether `id` names a catalog entry. Only catalog-sourced extensions are
/// croft-removable: croft wrote their manifest and can re-create it from the
/// catalog with one keystroke, so uninstalling is reversible. A user-dropped
/// manifest (not in the catalog) is the user's own file and is never deleted
/// by croft — see the "never delete user files unilaterally" rule.
pub fn is_catalog_entry(id: &str) -> bool {
    find_source(id).is_some()
}

/// Whether croft may uninstall `id`: it came from a curated source — the
/// bundled catalog or the signed remote index — so croft wrote its manifest and
/// can recreate it, making the uninstall reversible. A hand-dropped manifest
/// matches neither and is the user's own file (never deleted; see the "never
/// delete user files unilaterally" rule).
pub fn is_removable(id: &str) -> bool {
    is_catalog_entry(id) || crate::mcp::registry_index::is_index_entry(id)
}

/// Remove an added entry's manifest dir under `dir`, for hermetic testing.
/// Errors unless croft added `id`, from the catalog or the signed index
/// ([`is_removable`]): an index entry such as CodeQL (#1842) uninstalls too.
fn remove_from(dir: &Path, id: &str) -> Result<()> {
    if !is_removable(id) {
        anyhow::bail!(
            "'{id}' is not from the catalog or the extension index; croft only uninstalls what it added"
        );
    }
    let ext_dir = dir.join(id);
    if ext_dir.exists() {
        std::fs::remove_dir_all(&ext_dir).with_context(|| format!("removing {ext_dir:?}"))?;
    }
    Ok(())
}

/// Remove an added catalog extension from an explicit user-extensions dir (see
/// `prefs::save_mcp_consent_in` for why the dir is a parameter).
pub fn uninstall_in(extensions_dir: &Path, id: &str) -> Result<()> {
    remove_from(extensions_dir, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #465: csvlens is offered under AVAILABLE and leaves the list once added.
    #[test]
    fn csvlens_is_in_the_catalog_until_installed() {
        let none = BTreeSet::new();
        assert!(
            available_from(&none).iter().any(|s| s.id == "csvlens"),
            "csvlens is a catalog entry"
        );
        let installed: BTreeSet<String> = ["csvlens".to_string()].into_iter().collect();
        assert!(!available_from(&installed).iter().any(|s| s.id == "csvlens"));
    }

    /// #1842: CodeQL is neither built in nor bundled in the catalog. It comes
    /// from the signed index, under AVAILABLE until added, and croft can
    /// uninstall it again: an index entry is croft's to remove too.
    #[test]
    fn codeql_comes_from_the_index_and_uninstalls() {
        use crate::mcp::registry_index::{CODEQL_MANIFEST, available_summaries};
        assert!(!bundled_ids().contains("codeql"));
        assert!(!is_catalog_entry("codeql"));
        let cache = tempfile::tempdir().unwrap();
        let _cache = crate::session_state::CacheDirForThisTestThread::set(cache.path());
        crate::mcp::registry_index::cache_index_for_test(&[CODEQL_MANIFEST]);
        let dir = tempfile::tempdir().unwrap();
        let offered = |dir: &Path| {
            available_summaries(&BTreeSet::new(), &installed_ids(dir))
                .iter()
                .any(|s| s.id == "codeql" && !s.builtin)
        };
        assert!(offered(dir.path()), "under AVAILABLE");
        let added = dir.path().join("codeql");
        std::fs::create_dir_all(&added).unwrap();
        std::fs::write(added.join("extension.toml"), CODEQL_MANIFEST).unwrap();
        assert!(!offered(dir.path()), "added, it leaves AVAILABLE");
        uninstall_in(dir.path(), "codeql").expect("an index entry uninstalls");
        assert!(!added.exists());
        assert!(offered(dir.path()), "and is back under AVAILABLE");
    }

    #[test]
    fn catalog_lists_the_seeded_servers() {
        let ids: Vec<String> = catalog_summaries().into_iter().map(|s| s.id).collect();
        for id in ["mcp-fetch", "mcp-time", "mcp-markitdown"] {
            assert!(
                ids.contains(&id.to_string()),
                "catalog missing {id}: {ids:?}"
            );
        }
    }

    #[test]
    fn available_excludes_installed_ids() {
        let mut installed = BTreeSet::new();
        installed.insert("mcp-fetch".to_string());
        let avail: Vec<String> = available_from(&installed)
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert!(
            !avail.contains(&"mcp-fetch".to_string()),
            "added entry must drop out"
        );
        assert!(
            avail.contains(&"mcp-time".to_string()),
            "others stay available"
        );
    }

    #[test]
    fn install_writes_a_loadable_manifest_and_rejects_unknown() {
        let tmp = std::env::temp_dir().join(format!("croft-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let path = write_into(&tmp, "mcp-time").expect("writes the manifest");
        assert!(path.is_file());
        let m = manifest::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(m.id, "mcp-time");
        assert_eq!(m.mcp_servers.len(), 1);
        assert_eq!(m.commands.len(), 1);
        assert!(write_into(&tmp, "no-such-entry").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn uninstall_removes_a_catalog_entry_and_refuses_non_catalog_ids() {
        let tmp = std::env::temp_dir().join(format!("croft-catalog-rm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // Install then uninstall round-trips the manifest dir off disk.
        let path = write_into(&tmp, "mcp-time").expect("writes the manifest");
        assert!(path.is_file());
        remove_from(&tmp, "mcp-time").expect("uninstalls the catalog entry");
        assert!(!tmp.join("mcp-time").exists(), "manifest dir must be gone");
        // A user-dropped (non-catalog) extension is the user's file: refuse it,
        // and leave any such dir untouched.
        let user_dir = tmp.join("my-own-ext");
        std::fs::create_dir_all(&user_dir).unwrap();
        assert!(remove_from(&tmp, "my-own-ext").is_err());
        assert!(user_dir.exists(), "a non-catalog dir is never deleted");
        // Uninstalling an absent catalog entry is a no-op success (idempotent).
        remove_from(&tmp, "mcp-fetch").expect("absent catalog entry is a no-op");
        assert!(is_catalog_entry("mcp-fetch"));
        assert!(!is_catalog_entry("my-own-ext"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Every uv-provisioned catalog server, run at its pinned version, must
    /// answer `initialize`: an upstream dependency drift (mcp 2.x breaking
    /// the 2026.6.4 Time and Fetch servers, #1297) fails here before a user
    /// hits it. Needs uv and network; run with
    /// `cargo test --bin croft -- --ignored catalog_uv_servers`.
    #[test]
    #[ignore = "needs uv and network"]
    fn catalog_uv_servers_answer_initialize_at_their_pins() {
        use std::io::Write;
        let mut checked = 0;
        for src in manifest::CATALOG_MANIFESTS {
            let doc: toml::Value = toml::from_str(src).unwrap();
            for server in doc
                .get("mcp_servers")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(p) = server.get("provision") else {
                    continue;
                };
                if p.get("kind").and_then(toml::Value::as_str) != Some("uv") {
                    continue;
                }
                let field = |k: &str| p.get(k).and_then(toml::Value::as_str).unwrap().to_string();
                let spec = format!("{}=={}", field("package"), field("version"));
                let mut child = std::process::Command::new("uvx")
                    .args(["--from", &spec, &field("bin")])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"croft","version":"test"}}}"#;
                writeln!(child.stdin.take().unwrap(), "{init}").unwrap();
                let out = child.wait_with_output().unwrap();
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(
                    stdout.contains(r#""id":1,"result""#),
                    "{spec} did not answer initialize:\n{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                checked += 1;
            }
        }
        assert!(checked >= 3, "found {checked} uv catalog servers");
    }
}
