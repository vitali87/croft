#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
    Rust,
    Go,
    Json,
    Yaml,
    Toml,
    Bash,
    Markdown,
    Html,
    Css,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "py" | "pyi" => Some(Self::Python),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "mjs" | "cjs" => Some(Self::JavaScript),
            "jsx" => Some(Self::Jsx),
            "rs" => Some(Self::Rust),
            "go" => Some(Self::Go),
            "json" => Some(Self::Json),
            "yaml" | "yml" => Some(Self::Yaml),
            "toml" => Some(Self::Toml),
            "sh" | "bash" => Some(Self::Bash),
            "md" | "markdown" => Some(Self::Markdown),
            "html" | "htm" => Some(Self::Html),
            "css" => Some(Self::Css),
            _ => None,
        }
    }

    pub fn lsp_id(&self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Tsx => "typescriptreact",
            Self::JavaScript => "javascript",
            Self::Jsx => "javascriptreact",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Bash => "shellscript",
            Self::Markdown => "markdown",
            Self::Html => "html",
            Self::Css => "css",
        }
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

impl ServerConfig {
    pub fn ty() -> Self {
        Self {
            name: "ty",
            command: "ty".into(),
            args: vec!["server".into()],
            language: Language::Python,
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
            language: Language::Python,
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
            language: Language::Python,
            initialization_options: None,
            provision: None,
        }
    }

    pub fn ruff() -> Self {
        Self {
            name: "ruff",
            command: "ruff".into(),
            args: vec!["server".into()],
            language: Language::Python,
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
            language: Language::TypeScript,
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
            language: Language::TypeScript,
            initialization_options: None,
            provision: None,
        }
    }

    pub fn rust_analyzer() -> Self {
        Self {
            name: "rust-analyzer",
            command: "rust-analyzer".into(),
            args: vec![],
            language: Language::Rust,
            // No `cargo.targetDir` isolation: rust-analyzer shares the default
            // `target/`, reusing the warm build-script / proc-macro / dep
            // artifacts the user's own `cargo build` already produced, so the
            // crate-graph prime takes seconds. An isolated `target/rust-analyzer/`
            // (tried in 19df82c) is always cold on a large crate, so the prime
            // never finishes and every hover / completion / semantic-token
            // response comes back empty.
            initialization_options: None,
            provision: None,
        }
    }

    pub fn gopls() -> Self {
        Self {
            name: "gopls",
            command: "gopls".into(),
            args: vec!["serve".into()],
            language: Language::Go,
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
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("PY"), Some(Language::Python));
        assert_eq!(Language::from_extension("pyi"), Some(Language::Python));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("tsx"), Some(Language::Tsx));
        assert_eq!(Language::from_extension("js"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("mjs"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("jsx"), Some(Language::Jsx));
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("go"), Some(Language::Go));
        assert_eq!(Language::from_extension("yml"), Some(Language::Yaml));
    }

    #[test]
    fn from_extension_unknown() {
        assert_eq!(Language::from_extension("xyz"), None);
        assert_eq!(Language::from_extension(""), None);
    }

    #[test]
    fn lsp_id_react_variants() {
        assert_eq!(Language::Tsx.lsp_id(), "typescriptreact");
        assert_eq!(Language::Jsx.lsp_id(), "javascriptreact");
    }

    #[test]
    fn ty_config() {
        let c = ServerConfig::ty();
        assert_eq!(c.name, "ty");
        assert_eq!(c.command, "ty");
        assert_eq!(c.args, vec!["server"]);
        assert_eq!(c.language, Language::Python);
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
        assert_eq!(c.language, Language::Python);
    }

    #[test]
    fn vtsls_config() {
        let c = ServerConfig::vtsls();
        assert_eq!(c.command, "vtsls");
        assert_eq!(c.args, vec!["--stdio"]);
        assert_eq!(c.language, Language::TypeScript);
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
        // rust-analyzer / gopls are expected on the user's toolchain PATH;
        // croft doesn't install them.
        assert!(ServerConfig::rust_analyzer().provision.is_none());
        assert!(ServerConfig::gopls().provision.is_none());
        assert!(ServerConfig::basedpyright().provision.is_none());
    }

    #[test]
    fn rust_analyzer_config() {
        let c = ServerConfig::rust_analyzer();
        assert_eq!(c.command, "rust-analyzer");
        assert!(c.args.is_empty());
        assert_eq!(c.language, Language::Rust);
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
        assert_eq!(c.language, Language::Go);
    }
}
