//! The language table: the data-driven replacement for the old closed
//! `Language` enum's hardcoded match arms.
//!
//! Every loaded extension's `[[languages]]` blocks are merged into one table
//! mapping file extension -> language, plus per-language project-root markers
//! and the server "family". [`crate::lsp::config::Language`]'s resolution
//! methods read this table, so adding a language is pure data — a new
//! `[[languages]]` block in some extension's manifest, no Rust change.
//!
//! Phase B1/B2a builds the table from the bundled manifests only (a fixed,
//! hermetic const set). User-installed extensions are merged in by a later
//! phase; until then resolution covers exactly the first-party languages.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::lsp::config::Language;
use crate::lsp::manifest;

struct LangRecord {
    root_markers: &'static [&'static str],
    family: Option<&'static str>,
}

struct LangTable {
    by_ext: HashMap<String, Language>,
    records: HashMap<&'static str, LangRecord>,
}

/// Leak a parsed string to `&'static`. Sound because a language identity lives
/// for the whole process; the count is bounded by the number of installed
/// extensions, built once.
fn intern(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

impl LangTable {
    fn from_sources(sources: &[&str]) -> Self {
        let mut by_ext = HashMap::new();
        let mut records = HashMap::new();
        for src in sources {
            let Ok(m) = manifest::parse(src) else {
                continue;
            };
            for decl in &m.languages {
                let id = intern(&decl.id);
                let lang = Language(id);
                for ext in &decl.extensions {
                    by_ext.insert(ext.to_ascii_lowercase(), lang);
                }
                let markers: Vec<&'static str> =
                    decl.root_markers.iter().map(|s| intern(s)).collect();
                records.insert(
                    id,
                    LangRecord {
                        root_markers: Box::leak(markers.into_boxed_slice()),
                        family: decl.family.as_deref().map(intern),
                    },
                );
            }
        }
        LangTable { by_ext, records }
    }
}

static TABLE: OnceLock<LangTable> = OnceLock::new();

/// The active table. Lazily built from the bundled manifests only if
/// [`init_with_user_sources`] was never called (the case in tests, which keeps
/// resolution hermetic — no disk, no user state).
fn table() -> &'static LangTable {
    TABLE.get_or_init(|| LangTable::from_sources(manifest::BUNDLED_MANIFESTS))
}

/// Initialise the table from the bundled manifests plus user extension sources.
/// Called once at LSP startup before any language is resolved; a no-op (and a
/// dropped `user`) if the table was already built, so call it early.
pub fn init_with_user_sources(user: &[&str]) {
    let mut sources: Vec<&str> = manifest::BUNDLED_MANIFESTS.to_vec();
    sources.extend_from_slice(user);
    let _ = TABLE.set(LangTable::from_sources(&sources));
}

/// The language a file extension maps to, or `None` if no extension claims it.
pub fn from_extension(ext: &str) -> Option<Language> {
    table().by_ext.get(&ext.to_ascii_lowercase()).copied()
}

/// Resolve an LSP `languageId` to a known language, or `None`.
pub fn resolve(id: &str) -> Option<Language> {
    table().records.get_key_value(id).map(|(k, _)| Language(k))
}

/// Project-root markers for a language (empty slice when none / unknown).
pub fn root_markers(lang: Language) -> &'static [&'static str] {
    table()
        .records
        .get(lang.lsp_id())
        .map(|r| r.root_markers)
        .unwrap_or(&[])
}

/// Languages whose project-root markers sit directly in `dir` — the
/// workspace-open equivalent of VS Code's `workspaceContains` activation.
/// Used to pre-warm the evident language servers at startup so their cold
/// indexing overlaps with the user browsing the tree instead of starting
/// only when the first file opens (#27). Deliberately non-recursive: the
/// scan is a handful of stat calls, never a walk.
pub fn languages_evident_in(dir: &std::path::Path) -> Vec<Language> {
    let mut out: Vec<Language> = table()
        .records
        .keys()
        .filter_map(|id| resolve(id))
        .filter(|&lang| {
            root_markers(lang)
                .iter()
                .any(|marker| dir.join(marker).exists())
        })
        .collect();
    // The table is a HashMap; a stable order keeps spawn logs and tests
    // deterministic.
    out.sort_by_key(|l| l.lsp_id());
    out.dedup();
    out
}

/// The server family a language belongs to; itself when it has no `family`.
pub fn server_family(lang: Language) -> Language {
    table()
        .records
        .get(lang.lsp_id())
        .and_then(|r| r.family)
        .map(Language)
        .unwrap_or(lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_extensions_map_to_languages() {
        assert_eq!(from_extension("py"), Some(Language::PYTHON));
        assert_eq!(from_extension("PY"), Some(Language::PYTHON));
        assert_eq!(from_extension("rs"), Some(Language::RUST));
        assert_eq!(from_extension("yml"), Some(Language::YAML));
        assert_eq!(from_extension("xyz"), None);
    }

    /// #27: the workspace-open scan behind server pre-warm. A marker at
    /// the root makes its language evident; nothing recurses, so a marker
    /// buried in a subdirectory does not.
    #[test]
    fn languages_evident_in_reads_root_markers_non_recursively() {
        let dir = tempfile::tempdir().unwrap();
        assert!(languages_evident_in(dir.path()).is_empty());
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        assert_eq!(languages_evident_in(dir.path()), vec![Language::RUST]);
        std::fs::write(dir.path().join("go.mod"), "module x\n").unwrap();
        let both = languages_evident_in(dir.path());
        assert!(both.contains(&Language::RUST) && both.contains(&Language::GO));
        // Buried markers stay invisible: the scan is stat calls, not a walk.
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("pyproject.toml"), "").unwrap();
        assert!(!languages_evident_in(dir.path()).contains(&Language::PYTHON));
    }

    #[test]
    fn bundled_root_markers_and_family() {
        assert_eq!(root_markers(Language::RUST), &["Cargo.toml"]);
        assert_eq!(root_markers(Language::GO), &["go.mod"]);
        assert!(root_markers(Language::JSON).is_empty());
        // vtsls family: the React/JS ids collapse onto TypeScript; TypeScript
        // is its own family.
        assert_eq!(server_family(Language::TSX), Language::TYPESCRIPT);
        assert_eq!(server_family(Language::JSX), Language::TYPESCRIPT);
        assert_eq!(server_family(Language::JAVASCRIPT), Language::TYPESCRIPT);
        assert_eq!(server_family(Language::TYPESCRIPT), Language::TYPESCRIPT);
        assert_eq!(server_family(Language::RUST), Language::RUST);
    }

    const ZIG_MANIFEST: &str = r#"
id = "lsp-zig"
name = "Zig"
api_version = 1
[[languages]]
id = "zig"
extensions = ["zig", "zon"]
root_markers = ["build.zig", "build.zig.zon"]
"#;

    #[test]
    fn a_user_manifest_contributes_a_new_language_with_zero_rust() {
        // Built locally (not the global table) so the test stays hermetic, but
        // exercises the exact code path init_with_user_sources uses at startup.
        let t = LangTable::from_sources(&[ZIG_MANIFEST]);
        let zig = Language("zig");
        assert_eq!(t.by_ext.get("zig"), Some(&zig));
        assert_eq!(t.by_ext.get("zon"), Some(&zig));
        let rec = t.records.get("zig").expect("zig language recorded");
        assert_eq!(rec.root_markers, &["build.zig", "build.zig.zon"]);
        assert!(rec.family.is_none());
    }
}
