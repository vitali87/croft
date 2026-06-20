//! Declarative extension manifests (`extension.toml`).
//!
//! A croft extension is described by a TOML manifest. Phase B1 covers the
//! Tier-0 (pure data) case that matters first: language-server registrations.
//! The bundled Python/TypeScript/Rust/Go servers are expressed as manifests in
//! exactly the format a third-party extension uses, and
//! [`crate::lsp::ServerRegistry::with_defaults`] builds the registry by loading
//! them instead of hardcoding `ServerConfig`s in Rust.
//!
//! Strings parsed from a manifest are interned to `&'static` (via [`intern`]):
//! server/package names live for the whole process (the install path keys a
//! `static` set on them and moves them into background threads), so a leak-once
//! intern is the right representation and keeps `ServerConfig`/`Provision`
//! `Copy`-friendly and the install path untouched.

use serde::Deserialize;

use crate::lsp::config::{Language, ServerConfig};
use crate::lsp::install::Provision;

/// A parsed `extension.toml`. Only the fields phase B1 consumes are modelled;
/// unknown keys are ignored so the format can grow without breaking old builds.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub builtin: bool,
    pub api_version: u32,
    #[serde(default)]
    pub language_servers: Vec<LanguageServerDecl>,
}

/// One `[[language_servers]]` entry: a server and the language id(s) it serves.
#[derive(Debug, Clone, Deserialize)]
pub struct LanguageServerDecl {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Single language id (`language = "python"`). Combined with `languages`.
    #[serde(default)]
    pub language: Option<String>,
    /// Multiple language ids (`languages = ["typescript", …]`) for a server
    /// that serves a whole family from one process.
    #[serde(default)]
    pub languages: Vec<String>,
    /// Registration order within a language: lower wins each capability it
    /// advertises (the registry is first-registered-wins). Mirrors the call
    /// order the old hardcoded `with_defaults` relied on.
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub provision: Option<ProvisionDecl>,
    /// `initializationOptions` sent in the LSP `initialize` request. Absent for
    /// the common case.
    #[serde(default)]
    pub initialization_options: Option<serde_json::Value>,
}

/// How croft provisions the server when it isn't on PATH (maps to [`Provision`]).
#[derive(Debug, Clone, Deserialize)]
pub struct ProvisionDecl {
    pub kind: ProvisionKind,
    pub package: String,
    #[serde(default)]
    pub version: Option<String>,
    pub bin: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProvisionKind {
    Npm,
    Uv,
}

/// A server registration extracted from a manifest: its priority, the language
/// it is registered under, and the spawnable config.
#[derive(Debug, Clone)]
pub struct ServerEntry {
    pub priority: i64,
    pub language: Language,
    pub config: ServerConfig,
}

/// Parse an `extension.toml` source.
pub fn parse(src: &str) -> Result<ExtensionManifest, toml::de::Error> {
    toml::from_str(src)
}

/// Leak a parsed string to `&'static`. Sound because the data it represents
/// (server / package names) lives for the whole process; the count is bounded
/// by the number of installed extensions, loaded once at startup.
fn intern(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

impl ProvisionDecl {
    fn to_provision(&self) -> Provision {
        let package = intern(&self.package);
        let version = self.version.as_deref().map(intern);
        let bin = intern(&self.bin);
        match self.kind {
            ProvisionKind::Npm => Provision::Npm {
                package,
                version,
                bin,
            },
            ProvisionKind::Uv => Provision::Uv {
                package,
                version,
                bin,
            },
        }
    }
}

/// Flatten a manifest into the server registrations it contributes. One decl
/// that lists N languages yields N entries sharing one config (matching the old
/// `with_defaults`, which registered the same `ServerConfig` under each key).
/// The config's own `language` field is the decl's first language id (the
/// canonical one), again mirroring the old behaviour where `ServerConfig::vtsls`
/// carried `Language::TypeScript` even when registered under the React/JS keys.
/// Language ids this build can't represent yet are skipped (phase B2 opens the
/// `Language` enum).
pub fn entries(manifest: &ExtensionManifest) -> Vec<ServerEntry> {
    let mut out = Vec::new();
    for decl in &manifest.language_servers {
        let lang_ids: Vec<&str> = decl
            .language
            .as_deref()
            .into_iter()
            .chain(decl.languages.iter().map(String::as_str))
            .collect();
        let Some(canonical) = lang_ids.first().and_then(|id| Language::from_lsp_id(id)) else {
            continue;
        };
        let config = ServerConfig {
            name: intern(&decl.name),
            command: decl.command.clone(),
            args: decl.args.clone(),
            language: canonical,
            initialization_options: decl.initialization_options.clone(),
            provision: decl.provision.as_ref().map(ProvisionDecl::to_provision),
        };
        for id in &lang_ids {
            let Some(language) = Language::from_lsp_id(id) else {
                continue;
            };
            out.push(ServerEntry {
                priority: decl.priority,
                language,
                config: config.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PYTHON: &str = include_str!("../../assets/extensions/lsp-python/extension.toml");

    #[test]
    fn parses_python_manifest_header() {
        let m = parse(PYTHON).expect("python manifest parses");
        assert_eq!(m.id, "lsp-python");
        assert!(m.builtin);
        assert_eq!(m.api_version, 1);
        assert_eq!(m.language_servers.len(), 3);
    }

    #[test]
    fn entries_python_yields_three_servers_in_priority_order() {
        let m = parse(PYTHON).unwrap();
        let entries = entries(&m);
        let names: Vec<&str> = entries.iter().map(|e| e.config.name).collect();
        assert_eq!(names, vec!["ty", "basedpyright", "ruff"]);
        assert!(entries.iter().all(|e| e.language == Language::Python));
    }
}
