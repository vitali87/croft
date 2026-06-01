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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub name: &'static str,
    pub command: String,
    pub args: Vec<String>,
    pub language: Language,
}

impl ServerConfig {
    pub fn ty() -> Self {
        Self {
            name: "ty",
            command: "ty".into(),
            args: vec!["server".into()],
            language: Language::Python,
        }
    }

    pub fn basedpyright() -> Self {
        Self {
            name: "basedpyright",
            command: "basedpyright-langserver".into(),
            args: vec!["--stdio".into()],
            language: Language::Python,
        }
    }

    pub fn pyright() -> Self {
        Self {
            name: "pyright",
            command: "pyright-langserver".into(),
            args: vec!["--stdio".into()],
            language: Language::Python,
        }
    }

    pub fn ruff() -> Self {
        Self {
            name: "ruff",
            command: "ruff".into(),
            args: vec!["server".into()],
            language: Language::Python,
        }
    }

    pub fn vtsls() -> Self {
        Self {
            name: crate::lsp::install::VTSLS_SERVER_NAME,
            command: crate::lsp::install::VTSLS_SERVER_NAME.into(),
            args: vec!["--stdio".into()],
            language: Language::TypeScript,
        }
    }

    pub fn typescript_language_server() -> Self {
        Self {
            name: "typescript-language-server",
            command: "typescript-language-server".into(),
            args: vec!["--stdio".into()],
            language: Language::TypeScript,
        }
    }

    pub fn rust_analyzer() -> Self {
        Self {
            name: "rust-analyzer",
            command: "rust-analyzer".into(),
            args: vec![],
            language: Language::Rust,
        }
    }

    pub fn gopls() -> Self {
        Self {
            name: "gopls",
            command: "gopls".into(),
            args: vec!["serve".into()],
            language: Language::Go,
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
    fn gopls_config() {
        let c = ServerConfig::gopls();
        assert_eq!(c.command, "gopls");
        assert_eq!(c.args, vec!["serve"]);
        assert_eq!(c.language, Language::Go);
    }
}
