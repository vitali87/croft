use std::collections::HashMap;

use crate::lsp::config::{Language, ServerConfig};

pub struct ServerRegistry {
    by_language: HashMap<Language, Vec<ServerConfig>>,
}

impl ServerRegistry {
    pub fn new() -> Self {
        Self {
            by_language: HashMap::new(),
        }
    }

    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        // ty (Astral) first: every per-capability selector picks the first
        // registered server that advertises the capability, so ty handles all
        // the type-aware LSP features it supports (completion, hover,
        // definition, declaration, type-definition, references, rename,
        // semantic tokens, diagnostics). ty is a fast incremental Rust server
        // that answers in tens of ms even on a huge cold workspace, where
        // basedpyright pays a slow whole-tree enumeration first. basedpyright
        // stays registered as the fallback for the capabilities ty does not
        // yet advertise (go-to-implementation, inlay hints). ruff spawns for
        // lint diagnostics.
        r.register(Language::Python, ServerConfig::ty());
        r.register(Language::Python, ServerConfig::basedpyright());
        r.register(Language::Python, ServerConfig::ruff());
        for lang in [
            Language::TypeScript,
            Language::Tsx,
            Language::JavaScript,
            Language::Jsx,
        ] {
            r.register(lang, ServerConfig::vtsls());
        }
        r.register(Language::Rust, ServerConfig::rust_analyzer());
        r.register(Language::Go, ServerConfig::gopls());
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
        assert!(r.for_language(Language::Python).is_empty());
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
        r.register(Language::Python, ServerConfig::pyright());
        r.register(Language::Python, ServerConfig::ruff());
        assert_eq!(r.for_language(Language::Python).len(), 2);
    }
}
