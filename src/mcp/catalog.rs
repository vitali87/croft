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

/// The catalog entries available to add (not yet installed). Reads the real
/// user extensions dir.
pub fn available() -> Vec<ExtensionSummary> {
    available_from(&installed_ids(&manifest::user_extensions_dir()))
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

/// Add (install) a catalog entry: materialize its manifest into the user
/// extensions dir so it loads like any installed extension on the next refresh.
pub fn install(id: &str) -> Result<PathBuf> {
    write_into(&manifest::user_extensions_dir(), id)
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
}
