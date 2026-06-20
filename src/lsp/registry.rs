use std::collections::HashMap;

use crate::lsp::config::{Language, ServerConfig};
use crate::lsp::manifest;

/// Bundled (first-party) extension manifests, embedded so they ship in the
/// binary and load identically on every host. These are the same declarative
/// `extension.toml` format a third-party extension uses; the language servers
/// they declare reproduce the configs `with_defaults` used to hardcode.
const BUNDLED_MANIFESTS: &[&str] = &[
    include_str!("../../assets/extensions/lsp-python/extension.toml"),
    include_str!("../../assets/extensions/lsp-typescript/extension.toml"),
    include_str!("../../assets/extensions/lsp-rust/extension.toml"),
    include_str!("../../assets/extensions/lsp-go/extension.toml"),
];

pub struct ServerRegistry {
    by_language: HashMap<Language, Vec<ServerConfig>>,
}

impl ServerRegistry {
    pub fn new() -> Self {
        Self {
            by_language: HashMap::new(),
        }
    }

    /// Build the registry from the bundled extension manifests. The manifests
    /// encode the same policy the hardcoded version did: ty (Astral) is
    /// registered first (priority 0) so every per-capability selector picks it
    /// for the features it advertises (completion, hover, definition,
    /// declaration, type-definition, references, rename, semantic tokens,
    /// diagnostics) — ty answers in tens of ms even on a huge cold workspace
    /// where basedpyright pays a slow whole-tree enumeration. basedpyright
    /// (priority 1) is the fallback for what ty doesn't advertise yet
    /// (go-to-implementation, inlay hints); ruff (priority 2) lints. Servers are
    /// registered in ascending priority so the registry's first-registered-wins
    /// rule matches that policy.
    pub fn with_defaults() -> Self {
        Self::from_manifest_sources(BUNDLED_MANIFESTS)
    }

    /// Build a registry from a set of `extension.toml` sources. Servers are
    /// registered per language in ascending `priority` (stable within a
    /// priority, so a manifest's declaration order breaks ties).
    pub fn from_manifest_sources(sources: &[&str]) -> Self {
        let mut entries: Vec<manifest::ServerEntry> = Vec::new();
        for src in sources {
            let parsed = manifest::parse(src).expect("bundled manifest must parse");
            entries.extend(manifest::entries(&parsed));
        }
        entries.sort_by_key(|e| e.priority);
        let mut r = Self::new();
        for entry in entries {
            r.register(entry.language, entry.config);
        }
        r
    }

    pub fn register(&mut self, language: Language, config: ServerConfig) {
        self.by_language.entry(language).or_default().push(config);
    }

    pub fn for_language(&self, language: Language) -> &[ServerConfig] {
        self.by_language
            .get(&language)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn for_extension(&self, ext: &str) -> &[ServerConfig] {
        match Language::from_extension(ext) {
            Some(lang) => self.for_language(lang),
            None => &[],
        }
    }

    pub fn languages(&self) -> impl Iterator<Item = Language> + '_ {
        self.by_language.keys().copied()
    }
}

impl Default for ServerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_returns_no_servers() {
        let r = ServerRegistry::new();
        assert!(r.for_extension("py").is_empty());
        assert!(r.for_language(Language::PYTHON).is_empty());
    }

    #[test]
    fn defaults_cover_python() {
        let r = ServerRegistry::with_defaults();
        let servers = r.for_extension("py");
        let names: Vec<_> = servers.iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["ty", "basedpyright", "ruff"]);
    }

    #[test]
    fn defaults_cover_tsx_via_vtsls() {
        let r = ServerRegistry::with_defaults();
        let servers = r.for_extension("tsx");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "vtsls");
    }

    #[test]
    fn defaults_cover_rust_and_go() {
        let r = ServerRegistry::with_defaults();
        assert_eq!(r.for_extension("rs")[0].name, "rust-analyzer");
        assert_eq!(r.for_extension("go")[0].name, "gopls");
    }

    #[test]
    fn unknown_extension_yields_empty() {
        let r = ServerRegistry::with_defaults();
        assert!(r.for_extension("xyz").is_empty());
        assert!(r.for_extension("").is_empty());
    }

    #[test]
    fn register_appends_to_language() {
        let mut r = ServerRegistry::new();
        r.register(Language::PYTHON, ServerConfig::pyright());
        r.register(Language::PYTHON, ServerConfig::ruff());
        assert_eq!(r.for_language(Language::PYTHON).len(), 2);
    }

    #[test]
    fn bundled_manifests_reproduce_the_hardcoded_configs_exactly() {
        // The whole point of phase B1: loading the bundled manifests must yield
        // byte-for-byte the same `ServerConfig`s the factory methods produce, so
        // the move from Rust to TOML is provably behaviour-preserving. (Interned
        // manifest strings compare equal to the `&'static` literals by value.)
        let r = ServerRegistry::with_defaults();
        assert_eq!(
            r.for_language(Language::PYTHON),
            &[
                ServerConfig::ty(),
                ServerConfig::basedpyright(),
                ServerConfig::ruff(),
            ]
        );
        // vtsls serves all four TS/JS languages; each key holds the identical
        // config (language field = TypeScript), as the old loop produced.
        for lang in [
            Language::TYPESCRIPT,
            Language::TSX,
            Language::JAVASCRIPT,
            Language::JSX,
        ] {
            assert_eq!(r.for_language(lang), &[ServerConfig::vtsls()]);
        }
        assert_eq!(
            r.for_language(Language::RUST),
            &[ServerConfig::rust_analyzer()]
        );
        assert_eq!(r.for_language(Language::GO), &[ServerConfig::gopls()]);
    }
}
