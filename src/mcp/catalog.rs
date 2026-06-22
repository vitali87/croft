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

/// Ids already installed: the bundled built-ins plus every manifest under the
/// user extensions dir. A catalog entry whose id is here is no longer "available".
fn installed_ids(user_dir: &Path) -> BTreeSet<String> {
    let mut ids: BTreeSet<String> = manifest::summaries(manifest::BUNDLED_MANIFESTS)
        .into_iter()
        .map(|s| s.id)
        .collect();
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
/// embedded manifest; a remote (signed-index) entry is fetched and its sha256
/// verified against the index before it is written. Either way it lands in the
/// user extensions dir and loads like any installed extension on next refresh.
pub fn install(id: &str) -> Result<PathBuf> {
    if find_source(id).is_some() {
        write_into(&manifest::user_extensions_dir(), id)
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

/// Remove a catalog entry's manifest dir under `dir` — pure over the target
/// dir, for hermetic testing. Errors if `id` is not a catalog entry (croft
/// only removes what it installed).
fn remove_from(dir: &Path, id: &str) -> Result<()> {
    if find_source(id).is_none() {
        anyhow::bail!("'{id}' is not a catalog entry; croft only uninstalls what it added");
    }
    let ext_dir = dir.join(id);
    if ext_dir.exists() {
        std::fs::remove_dir_all(&ext_dir).with_context(|| format!("removing {ext_dir:?}"))?;
    }
    Ok(())
}

/// Uninstall a catalog entry: delete its manifest from the user extensions dir
/// so it drops back to AVAILABLE on the next refresh. Reversible via [`install`].
pub fn uninstall(id: &str) -> Result<()> {
    remove_from(&manifest::user_extensions_dir(), id)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
