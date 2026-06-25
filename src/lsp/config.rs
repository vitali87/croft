/// A language identity, represented by its LSP `languageId` (interned to
/// `&'static`). Formerly a closed enum; now an open newtype so a declarative
/// extension can contribute a brand-new language (e.g. Zig) with no Rust
/// change. The per-language data the enum used to hardcode — file extensions,
/// project-root markers, the server family — lives in the language table built
/// from extension manifests (see [`crate::lsp::languages`]); the constants
/// below name the first-party languages for call sites and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Language(pub(crate) &'static str);

impl Language {
    pub const PYTHON: Language = Language("python");
    pub const TYPESCRIPT: Language = Language("typescript");
    pub const TSX: Language = Language("typescriptreact");
    pub const JAVASCRIPT: Language = Language("javascript");
    pub const JSX: Language = Language("javascriptreact");
    pub const RUST: Language = Language("rust");
    pub const GO: Language = Language("go");
    pub const JSON: Language = Language("json");
    pub const YAML: Language = Language("yaml");
    pub const TOML: Language = Language("toml");
    pub const BASH: Language = Language("shellscript");
    pub const MARKDOWN: Language = Language("markdown");
    pub const HTML: Language = Language("html");
    pub const CSS: Language = Language("css");
    pub const C: Language = Language("c");
    pub const CPP: Language = Language("cpp");

    /// The language whose manifests map this file extension, or `None`.
    /// Data-driven: the mapping comes from every loaded extension's
    /// `[[languages]]` blocks.
    pub fn from_extension(ext: &str) -> Option<Self> {
        crate::lsp::languages::from_extension(ext)
    }

    /// The LSP `languageId` sent in `initialize`/`didOpen`.
    pub fn lsp_id(&self) -> &'static str {
        self.0
    }

    /// Resolve an LSP `languageId` string to a known language, or `None` if no
    /// loaded extension contributes it.
    pub fn from_lsp_id(id: &str) -> Option<Self> {
        crate::lsp::languages::resolve(id)
    }

    /// Project-manifest filenames that anchor this language's server at the
    /// right sub-project root. Empty slice means "use the workspace root".
    pub fn root_markers(&self) -> &'static [&'static str] {
        crate::lsp::languages::root_markers(*self)
    }

    /// The language this one shares a managed server with (e.g. `typescriptreact`
    /// -> `typescript`); itself when it is its own family.
    pub fn server_family(&self) -> Self {
        crate::lsp::languages::server_family(*self)
    }
}

// `Eq` is intentionally not derived: `initialization_options` is a
// `serde_json::Value`, which is only `PartialEq` (floats break total
// equality). `ServerConfig` is never used as a map key, so `PartialEq` is all
// the call sites and tests need.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    pub name: &'static str,
    pub command: String,
    pub args: Vec<String>,
    pub language: Language,
    /// Server-specific `initializationOptions` sent in the LSP `initialize`
    /// request. `None` for servers that need none (the common case).
    pub initialization_options: Option<serde_json::Value>,
    /// How croft installs this server itself when it isn't already on the
    /// user's PATH. `None` means PATH-only (croft never provisions it). When
    /// `Some`, [`crate::lsp::manager`]'s resolver dispatches to the matching
    /// backend and kicks off a lazy background install on first miss.
    pub provision: Option<crate::lsp::install::Provision>,
}

/// rust-analyzer release `.gz` assets (tag `2026-06-22`) keyed by Rust's
/// `target_os`-`target_arch`. Listed in `BTreeMap` key order so this slice
/// compares equal, by value, to the one the bundled `lsp-rust` manifest interns
/// — the `reproduce_the_hardcoded_configs_exactly` invariant. Android is absent
/// on purpose: the linux-gnu build can't run on bionic, so Termux installs via
/// `pkg` instead (the `termux_pkg` field on the provision). Bump the tag in
/// both this const and `assets/extensions/lsp-rust/extension.toml` together.
const RUST_ANALYZER_TARGETS: &[(&str, &str)] = &[
    (
        "linux-aarch64",
        "https://github.com/rust-lang/rust-analyzer/releases/download/2026-06-22/rust-analyzer-aarch64-unknown-linux-gnu.gz",
    ),
    (
        "linux-x86_64",
        "https://github.com/rust-lang/rust-analyzer/releases/download/2026-06-22/rust-analyzer-x86_64-unknown-linux-gnu.gz",
    ),
    (
        "macos-aarch64",
        "https://github.com/rust-lang/rust-analyzer/releases/download/2026-06-22/rust-analyzer-aarch64-apple-darwin.gz",
    ),
    (
        "macos-x86_64",
        "https://github.com/rust-lang/rust-analyzer/releases/download/2026-06-22/rust-analyzer-x86_64-apple-darwin.gz",
    ),
];

impl ServerConfig {
    pub fn ty() -> Self {
        Self {
            name: "ty",
            command: "ty".into(),
            args: vec!["server".into()],
            language: Language::PYTHON,
            initialization_options: None,
            // Astral's type server is on PyPI; uv installs it (and pulls a
            // Python interpreter) with no node dependency. Latest, not pinned:
            // ty is fast-moving and a stale pin would just fail to install.
            provision: Some(crate::lsp::install::Provision::Uv {
                package: "ty",
                version: None,
                bin: "ty",
            }),
        }
    }

    pub fn basedpyright() -> Self {
        Self {
            name: "basedpyright",
            command: "basedpyright-langserver".into(),
            args: vec!["--stdio".into()],
            language: Language::PYTHON,
            initialization_options: None,
            // PATH-only: the registered fallback for the few capabilities `ty`
            // doesn't advertise. croft provisions `ty`/`ruff` for Python; a user
            // who wants basedpyright too can install it themselves.
            provision: None,
        }
    }

    pub fn pyright() -> Self {
        Self {
            name: "pyright",
            command: "pyright-langserver".into(),
            args: vec!["--stdio".into()],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        }
    }

    pub fn ruff() -> Self {
        Self {
            name: "ruff",
            command: "ruff".into(),
            args: vec!["server".into()],
            language: Language::PYTHON,
            initialization_options: None,
            // Ruff's linter LSP, installed from PyPI via uv alongside `ty`.
            provision: Some(crate::lsp::install::Provision::Uv {
                package: "ruff",
                version: None,
                bin: "ruff",
            }),
        }
    }

    pub fn vtsls() -> Self {
        Self {
            name: crate::lsp::install::VTSLS_SERVER_NAME,
            command: crate::lsp::install::VTSLS_SERVER_NAME.into(),
            args: vec!["--stdio".into()],
            language: Language::TYPESCRIPT,
            initialization_options: None,
            // croft owns an npm-installed copy pinned for local/remote parity;
            // a globally-installed `vtsls` is the fallback.
            provision: Some(crate::lsp::install::Provision::Npm {
                package: "@vtsls/language-server",
                version: Some("0.3.0"),
                bin: crate::lsp::install::VTSLS_SERVER_NAME,
            }),
        }
    }

    pub fn typescript_language_server() -> Self {
        Self {
            name: "typescript-language-server",
            command: "typescript-language-server".into(),
            args: vec!["--stdio".into()],
            language: Language::TYPESCRIPT,
            initialization_options: None,
            provision: None,
        }
    }

    pub fn rust_analyzer() -> Self {
        Self {
            name: "rust-analyzer",
            command: "rust-analyzer".into(),
            args: vec![],
            language: Language::RUST,
            // No `cargo.targetDir` isolation: rust-analyzer shares the default
            // `target/`, reusing the warm build-script / proc-macro / dep
            // artifacts the user's own `cargo build` already produced, so the
            // crate-graph prime takes seconds. An isolated `target/rust-analyzer/`
            // (tried in 19df82c) is always cold on a large crate, so the prime
            // never finishes and every hover / completion / semantic-token
            // response comes back empty.
            initialization_options: None,
            // rust-analyzer ships as a single gzipped native binary per platform
            // (the same asset VS Code's extension downloads). resolve_config
            // probes PATH + ~/.cargo/bin BEFORE this, so a toolchain
            // rust-analyzer matching the user's rustc always wins and cargo
            // stays reachable; croft downloads its own copy only when none
            // exists anywhere — which is what makes "open a .rs → get LSP" hold
            // on a fresh box. On Termux the linux-gnu build can't run on bionic
            // libc, so the install reroutes to `pkg install rust-analyzer`.
            provision: Some(crate::lsp::install::Provision::Binary {
                targets: RUST_ANALYZER_TARGETS,
                bin: "rust-analyzer",
                archive: crate::lsp::install::ArchiveKind::Gz,
                bin_path: None,
                termux_pkg: Some("rust-analyzer"),
            }),
        }
    }

    pub fn gopls() -> Self {
        Self {
            name: "gopls",
            command: "gopls".into(),
            args: vec!["serve".into()],
            language: Language::GO,
            initialization_options: None,
            provision: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_extension_known() {
        assert_eq!(Language::from_extension("py"), Some(Language::PYTHON));
        assert_eq!(Language::from_extension("PY"), Some(Language::PYTHON));
        assert_eq!(Language::from_extension("pyi"), Some(Language::PYTHON));
        assert_eq!(Language::from_extension("ts"), Some(Language::TYPESCRIPT));
        assert_eq!(Language::from_extension("tsx"), Some(Language::TSX));
        assert_eq!(Language::from_extension("js"), Some(Language::JAVASCRIPT));
        assert_eq!(Language::from_extension("mjs"), Some(Language::JAVASCRIPT));
        assert_eq!(Language::from_extension("jsx"), Some(Language::JSX));
        assert_eq!(Language::from_extension("rs"), Some(Language::RUST));
        assert_eq!(Language::from_extension("go"), Some(Language::GO));
        assert_eq!(Language::from_extension("yml"), Some(Language::YAML));
    }

    #[test]
    fn from_extension_unknown() {
        assert_eq!(Language::from_extension("xyz"), None);
        assert_eq!(Language::from_extension(""), None);
    }

    #[test]
    fn lsp_id_react_variants() {
        assert_eq!(Language::TSX.lsp_id(), "typescriptreact");
        assert_eq!(Language::JSX.lsp_id(), "javascriptreact");
    }

    #[test]
    fn from_lsp_id_round_trips_every_language() {
        // `from_lsp_id` is the inverse of `lsp_id`; an extension manifest names
        // languages by their wire id, so the two must never drift.
        for lang in [
            Language::PYTHON,
            Language::TYPESCRIPT,
            Language::TSX,
            Language::JAVASCRIPT,
            Language::JSX,
            Language::RUST,
            Language::GO,
            Language::JSON,
            Language::YAML,
            Language::TOML,
            Language::BASH,
            Language::MARKDOWN,
            Language::HTML,
            Language::CSS,
        ] {
            assert_eq!(Language::from_lsp_id(lang.lsp_id()), Some(lang));
        }
        assert_eq!(Language::from_lsp_id("nonesuch"), None);
    }

    #[test]
    fn ty_config() {
        let c = ServerConfig::ty();
        assert_eq!(c.name, "ty");
        assert_eq!(c.command, "ty");
        assert_eq!(c.args, vec!["server"]);
        assert_eq!(c.language, Language::PYTHON);
        // Python servers are provisioned via uv (PyPI), so a fresh box gets
        // them with no manual install and no node dependency.
        assert_eq!(
            c.provision,
            Some(crate::lsp::install::Provision::Uv {
                package: "ty",
                version: None,
                bin: "ty",
            })
        );
    }

    #[test]
    fn basedpyright_config() {
        let c = ServerConfig::basedpyright();
        assert_eq!(c.command, "basedpyright-langserver");
        assert_eq!(c.args, vec!["--stdio"]);
    }

    #[test]
    fn ruff_config() {
        let c = ServerConfig::ruff();
        assert_eq!(c.command, "ruff");
        assert_eq!(c.args, vec!["server"]);
        assert_eq!(c.language, Language::PYTHON);
    }

    #[test]
    fn vtsls_config() {
        let c = ServerConfig::vtsls();
        assert_eq!(c.command, "vtsls");
        assert_eq!(c.args, vec!["--stdio"]);
        assert_eq!(c.language, Language::TYPESCRIPT);
        // vtsls is npm-provisioned and version-pinned for local/remote parity.
        assert_eq!(
            c.provision,
            Some(crate::lsp::install::Provision::Npm {
                package: "@vtsls/language-server",
                version: Some("0.3.0"),
                bin: "vtsls",
            })
        );
    }

    #[test]
    fn path_only_servers_have_no_provision() {
        // gopls / basedpyright are expected on the user's toolchain PATH; croft
        // doesn't install them. (rust-analyzer USED to be here too, but now
        // carries a Binary provision — see `rust_analyzer_provisions_as_binary`.)
        assert!(ServerConfig::gopls().provision.is_none());
        assert!(ServerConfig::basedpyright().provision.is_none());
    }

    #[test]
    fn rust_analyzer_provisions_as_binary_with_termux_fallback() {
        // rust-analyzer is PATH-first (resolve_config probes PATH + ~/.cargo/bin
        // before downloading), but no longer PATH-ONLY: when absent croft
        // downloads the official release `.gz`, and on Termux reroutes to
        // `pkg install rust-analyzer` since the glibc build can't run on bionic.
        use crate::lsp::install::{ArchiveKind, Provision};
        let Some(Provision::Binary {
            bin,
            archive,
            bin_path,
            termux_pkg,
            targets,
        }) = ServerConfig::rust_analyzer().provision
        else {
            panic!("rust-analyzer must carry a Binary provision");
        };
        assert_eq!(bin, "rust-analyzer");
        assert_eq!(archive, ArchiveKind::Gz);
        assert_eq!(bin_path, None);
        assert_eq!(termux_pkg, Some("rust-analyzer"));
        // No `android` key: Android is served by the Termux pkg fallback.
        assert!(!targets.iter().any(|(k, _)| k.starts_with("android")));
        assert!(targets.iter().any(|(k, _)| *k == "linux-x86_64"));
    }

    #[test]
    fn rust_analyzer_config() {
        let c = ServerConfig::rust_analyzer();
        assert_eq!(c.command, "rust-analyzer");
        assert!(c.args.is_empty());
        assert_eq!(c.language, Language::RUST);
    }

    #[test]
    fn rust_analyzer_shares_the_default_target_dir() {
        // rust-analyzer must NOT isolate its `cargo check` into
        // `target/rust-analyzer/`. On a large crate that isolated dir is always
        // cold, so the initial crate-graph prime effectively never finishes and
        // hover / completion / semantic tokens come back empty. Sharing the
        // default `target/` lets RA reuse the warm artifacts the user's
        // `cargo build` already produced, so it primes in seconds.
        let c = ServerConfig::rust_analyzer();
        assert!(
            c.initialization_options.is_none(),
            "rust-analyzer must not send cargo.targetDir (the isolated dir never primes on a big crate)"
        );
    }

    #[test]
    fn most_servers_send_no_initialization_options() {
        assert!(ServerConfig::ty().initialization_options.is_none());
        assert!(ServerConfig::gopls().initialization_options.is_none());
    }

    #[test]
    fn gopls_config() {
        let c = ServerConfig::gopls();
        assert_eq!(c.command, "gopls");
        assert_eq!(c.args, vec!["serve"]);
        assert_eq!(c.language, Language::GO);
    }
}
