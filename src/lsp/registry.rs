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
    include_str!("../../assets/extensions/lsp-yaml/extension.toml"),
    include_str!("../../assets/extensions/lsp-json/extension.toml"),
    include_str!("../../assets/extensions/lsp-html/extension.toml"),
    include_str!("../../assets/extensions/lsp-css/extension.toml"),
    include_str!("../../assets/extensions/lsp-bash/extension.toml"),
    include_str!("../../assets/extensions/lsp-toml/extension.toml"),
    include_str!("../../assets/extensions/lsp-cpp/extension.toml"),
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

    /// Build the registry from the bundled manifests plus user extension
    /// sources (read from `~/.config/croft/extensions`). User servers register
    /// after the bundled ones at equal priority, so a user extension extends
    /// the language coverage without displacing a first-party server.
    pub fn with_user_extensions(user_sources: &[&str]) -> Self {
        let mut sources: Vec<&str> = manifest::BUNDLED_MANIFESTS.to_vec();
        sources.extend_from_slice(user_sources);
        Self::from_manifest_sources(&sources)
    }

    /// Like [`with_user_extensions`], but skip the servers contributed by any
    /// extension whose id is in `disabled` (the Extensions panel's disable set).
    /// Only server *registration* is gated: the language table is built
    /// separately and keeps every language identity, so a disabled `lsp-rust`
    /// still recognises `.rs` (and its highlighting) while no rust-analyzer
    /// spawns. An unparseable source is kept rather than silently dropped.
    pub fn with_user_extensions_filtered(
        user_sources: &[&str],
        disabled: &std::collections::BTreeSet<String>,
    ) -> Self {
        let mut sources: Vec<&str> = manifest::BUNDLED_MANIFESTS.to_vec();
        sources.extend_from_slice(user_sources);
        let enabled: Vec<&str> = sources
            .into_iter()
            .filter(|s| {
                manifest::parse(s)
                    .map(|m| !disabled.contains(&m.id))
                    .unwrap_or(true)
            })
            .collect();
        Self::from_manifest_sources(&enabled)
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

    #[test]
    fn bundled_manifests_register_yaml_language_server() {
        // YAML is the first npm-provisioned language added after the founding
        // four. The bundled set must register yaml-language-server for .yaml/.yml
        // (provisioned from npm), proving a new language ships as data alone.
        let r = ServerRegistry::with_defaults();
        let servers = r.for_language(Language::YAML);
        assert_eq!(servers.len(), 1, "exactly one server for YAML");
        let yls = &servers[0];
        assert_eq!(yls.name, "yaml-language-server");
        assert_eq!(yls.command, "yaml-language-server");
        assert_eq!(yls.args, vec!["--stdio".to_string()]);
        assert!(
            matches!(
                yls.provision,
                Some(crate::lsp::install::Provision::Npm {
                    bin: "yaml-language-server",
                    ..
                })
            ),
            "yaml-language-server is npm-provisioned"
        );
        // Resolves by both file extensions.
        assert_eq!(r.for_extension("yaml")[0].name, "yaml-language-server");
        assert_eq!(r.for_extension("yml")[0].name, "yaml-language-server");
    }

    #[test]
    fn bundled_manifests_register_json_html_css_language_servers() {
        // JSON, HTML and CSS each get their own extension/server, all three
        // provisioned from the single maintained `@t1ckbase/vscode-langservers-extracted`
        // npm package (one bin per language). Distinct server names keep their
        // managed installs and re-probe signals independent.
        let r = ServerRegistry::with_defaults();
        for (lang, ext, server_bin) in [
            (Language::JSON, "json", "vscode-json-language-server"),
            (Language::HTML, "html", "vscode-html-language-server"),
            (Language::CSS, "css", "vscode-css-language-server"),
        ] {
            let servers = r.for_language(lang);
            assert_eq!(servers.len(), 1, "exactly one server for {ext}");
            let s = &servers[0];
            assert_eq!(s.name, server_bin);
            assert_eq!(s.command, server_bin);
            assert_eq!(s.args, vec!["--stdio".to_string()]);
            assert!(
                matches!(
                    s.provision,
                    Some(crate::lsp::install::Provision::Npm {
                        package: "@t1ckbase/vscode-langservers-extracted",
                        ..
                    })
                ),
                "{ext} server installs from @t1ckbase/vscode-langservers-extracted"
            );
            assert_eq!(r.for_extension(ext)[0].name, server_bin);
        }
    }

    #[test]
    fn bundled_manifests_register_bash_language_server() {
        // Bash (the `shellscript` language id, .sh/.bash) gets bash-language-server,
        // npm-provisioned. Note its invocation is the `start` subcommand, not the
        // `--stdio` flag the other servers use.
        let r = ServerRegistry::with_defaults();
        let servers = r.for_language(Language::BASH);
        assert_eq!(servers.len(), 1, "exactly one server for bash");
        let s = &servers[0];
        assert_eq!(s.name, "bash-language-server");
        assert_eq!(s.command, "bash-language-server");
        assert_eq!(s.args, vec!["start".to_string()]);
        assert!(
            matches!(
                s.provision,
                Some(crate::lsp::install::Provision::Npm {
                    package: "bash-language-server",
                    ..
                })
            ),
            "bash-language-server is npm-provisioned"
        );
        assert_eq!(r.for_extension("sh")[0].name, "bash-language-server");
        assert_eq!(r.for_extension("bash")[0].name, "bash-language-server");
    }

    #[test]
    fn bundled_manifests_register_toml_and_cpp_binary_servers() {
        use crate::lsp::install::{ArchiveKind, Provision};
        let r = ServerRegistry::with_defaults();

        // TOML -> taplo, provisioned as a single gzipped binary.
        let toml = r.for_language(Language::TOML);
        assert_eq!(toml.len(), 1, "one server for toml");
        assert_eq!(toml[0].name, "taplo");
        assert_eq!(toml[0].command, "taplo");
        assert_eq!(toml[0].args, vec!["lsp".to_string(), "stdio".to_string()]);
        assert!(
            matches!(
                toml[0].provision,
                Some(Provision::Binary {
                    bin: "taplo",
                    archive: ArchiveKind::Gz,
                    ..
                })
            ),
            "taplo is a gz binary download"
        );
        assert_eq!(r.for_extension("toml")[0].name, "taplo");

        // C and C++ -> clangd (one server serves both), provisioned as a zip.
        for (lang, ext) in [(Language::C, "c"), (Language::CPP, "cpp")] {
            let s = r.for_language(lang);
            assert_eq!(s.len(), 1, "one server for {ext}");
            assert_eq!(s[0].name, "clangd");
            assert!(
                matches!(
                    s[0].provision,
                    Some(Provision::Binary {
                        bin: "clangd",
                        archive: ArchiveKind::Zip,
                        ..
                    })
                ),
                "clangd is a zip binary download"
            );
        }
        assert_eq!(r.for_extension("c")[0].name, "clangd");
        assert_eq!(r.for_extension("cpp")[0].name, "clangd");
    }

    #[test]
    fn bundled_manifest_provisions_rust_analyzer_as_gz_binary_with_termux_fallback() {
        // rust-analyzer used to be PATH-only (provision: None), so on a box
        // without it on PATH — most of all Termux, where there is no rustup —
        // Rust LSP silently never started. It now carries a gz-binary provision
        // (the asset VS Code downloads) plus a Termux `pkg` fallback for Android,
        // where the linux-gnu build can't run on bionic libc. Guard both.
        use crate::lsp::install::{ArchiveKind, Provision};
        let r = ServerRegistry::with_defaults();
        let rust = r.for_language(Language::RUST);
        assert_eq!(rust.len(), 1, "one server for rust");
        assert_eq!(rust[0].name, "rust-analyzer");
        assert_eq!(rust[0].command, "rust-analyzer");
        let Some(Provision::Binary {
            bin,
            archive,
            bin_path,
            termux_pkg,
            targets,
        }) = &rust[0].provision
        else {
            panic!("rust-analyzer must carry a Binary provision");
        };
        assert_eq!(*bin, "rust-analyzer");
        assert_eq!(*archive, ArchiveKind::Gz, "single gzipped binary");
        assert_eq!(*bin_path, None, "gz decompresses straight to the binary");
        assert_eq!(
            *termux_pkg,
            Some("rust-analyzer"),
            "Termux reroutes to `pkg install rust-analyzer`"
        );
        // Desktop platforms croft can download for; Android is intentionally
        // absent (handled by the Termux pkg fallback above).
        for key in [
            "macos-aarch64",
            "macos-x86_64",
            "linux-x86_64",
            "linux-aarch64",
        ] {
            assert!(
                targets
                    .iter()
                    .any(|(k, url)| *k == key && url.ends_with(".gz")),
                "rust-analyzer must have a .gz target for {key}"
            );
        }
        assert!(
            !targets.iter().any(|(k, _)| k.starts_with("android")),
            "android must not be a download target (uses pkg instead)"
        );
    }

    #[test]
    fn a_user_manifest_registers_a_new_language_server_with_zero_rust() {
        // The phase-B payoff: a Zig extension declared purely as data registers
        // zls for .zig with no Rust change. (Hermetic: the manifest is passed
        // as a source, not read from disk.)
        const ZIG: &str = r#"
id = "lsp-zig"
name = "Zig"
api_version = 1
[[languages]]
id = "zig"
extensions = ["zig", "zon"]
root_markers = ["build.zig", "build.zig.zon"]
[[language_servers]]
name = "zls"
command = "zls"
args = []
language = "zig"
priority = 0
"#;
        let r = ServerRegistry::from_manifest_sources(&[ZIG]);
        let servers = r.for_language(Language("zig"));
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "zls");
        assert_eq!(servers[0].command, "zls");
        // The bundled languages still resolve when a user manifest is mixed in.
        let mixed = ServerRegistry::with_user_extensions(&[ZIG]);
        assert_eq!(mixed.for_language(Language("zig"))[0].name, "zls");
        assert_eq!(mixed.for_language(Language::RUST)[0].name, "rust-analyzer");
    }

    #[test]
    fn a_disabled_lsp_extension_registers_no_server_for_its_language() {
        let mut disabled = std::collections::BTreeSet::new();
        disabled.insert("lsp-python".to_string());
        let r = ServerRegistry::with_user_extensions_filtered(&[], &disabled);
        assert!(
            r.for_language(Language::PYTHON).is_empty(),
            "a disabled lsp-python must register no server for Python"
        );
        // A language whose extension is still enabled is unaffected.
        assert_eq!(
            r.for_language(Language::RUST)[0].name,
            "rust-analyzer",
            "disabling one LSP extension must not gate the others"
        );
    }
}
