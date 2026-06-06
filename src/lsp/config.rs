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
    /// request. `None` for servers that need none. For rust-analyzer this
    /// carries `cargo.targetDir` so its `cargo check` uses an isolated target
    /// directory.
    pub initialization_options: Option<serde_json::Value>,
}

impl ServerConfig {
    pub fn ty() -> Self {
        Self {
            name: "ty",
            command: "ty".into(),
            args: vec!["server".into()],
            language: Language::Python,
            initialization_options: None,
        }
    }

    pub fn basedpyright() -> Self {
        Self {
            name: "basedpyright",
            command: "basedpyright-langserver".into(),
            args: vec!["--stdio".into()],
            language: Language::Python,
            initialization_options: None,
        }
    }

    pub fn pyright() -> Self {
        Self {
            name: "pyright",
            command: "pyright-langserver".into(),
            args: vec!["--stdio".into()],
            language: Language::Python,
            initialization_options: None,
        }
    }

    pub fn ruff() -> Self {
        Self {
            name: "ruff",
            command: "ruff".into(),
            args: vec!["server".into()],
            language: Language::Python,
            initialization_options: None,
        }
    }

    pub fn vtsls() -> Self {
        Self {
            name: crate::lsp::install::VTSLS_SERVER_NAME,
            command: crate::lsp::install::VTSLS_SERVER_NAME.into(),
            args: vec!["--stdio".into()],
            language: Language::TypeScript,
            initialization_options: None,
        }
    }

    pub fn typescript_language_server() -> Self {
        Self {
            name: "typescript-language-server",
            command: "typescript-language-server".into(),
            args: vec!["--stdio".into()],
            language: Language::TypeScript,
            initialization_options: None,
        }
    }

    pub fn rust_analyzer() -> Self {
        Self {
            name: "rust-analyzer",
            command: "rust-analyzer".into(),
            args: vec![],
            language: Language::Rust,
            // Give rust-analyzer its own `target/rust-analyzer/` so its
            // `cargo check` no longer fights the user's `cargo build`/`cargo
            // install` over the target-dir lock, and its build-script and
            // proc-macro artifacts survive a rebuild instead of forcing a
            // fresh ~14s cold crate-graph analysis on the next file open.
            initialization_options: Some(serde_json::json!({
                "cargo": { "targetDir": true }
            })),
        }
    }

    pub fn gopls() -> Self {
        Self {
            name: "gopls",
            command: "gopls".into(),
            args: vec!["serve".into()],
            language: Language::Go,
            initialization_options: None,
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
    }

    #[test]
    fn rust_analyzer_config() {
        let c = ServerConfig::rust_analyzer();
        assert_eq!(c.command, "rust-analyzer");
        assert!(c.args.is_empty());
        assert_eq!(c.language, Language::Rust);
    }

    #[test]
    fn rust_analyzer_isolates_its_target_dir() {
        // A dedicated `target/rust-analyzer/` keeps the user's
        // `cargo install`/`cargo build` from invalidating rust-analyzer's
        // build-script and proc-macro artifacts (and vice versa), so the cold
        // crate-graph analysis is re-paid far less often.
        let c = ServerConfig::rust_analyzer();
        let opts = c
            .initialization_options
            .expect("rust-analyzer must send initializationOptions");
        assert_eq!(opts["cargo"]["targetDir"], serde_json::json!(true));
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
