//! Declarative extension manifests (`extension.toml`).
//!
//! A croft extension is described by a TOML manifest with two Tier-0 (pure
//! data) blocks: `[[languages]]` (a language identity — its lsp id, file
//! extensions, project-root markers, server family) and `[[language_servers]]`
//! (a server and the languages it serves). The bundled Python/TypeScript/Rust/Go
//! servers and the core language identities are expressed as manifests in
//! exactly the format a third-party extension uses; the registry and language
//! table are built by loading them plus any user extensions under
//! `~/.config/croft/extensions/`, instead of hardcoding the data in Rust.
//!
//! Strings parsed from a manifest are interned to `&'static` (via [`intern`]):
//! server/package names live for the whole process (the install path keys a
//! `static` set on them and moves them into background threads), so a leak-once
//! intern is the right representation and keeps `ServerConfig`/`Provision`
//! `Copy`-friendly and the install path untouched.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::lsp::config::{Language, ServerConfig};
use crate::lsp::install::Provision;

/// Bundled (first-party) extension manifests, embedded so they ship in the
/// binary and load identically on every host. The same declarative
/// `extension.toml` format a third-party extension uses. `core-languages`
/// declares the file-type identities for formats with no bundled server.
pub const BUNDLED_MANIFESTS: &[&str] = &[
    include_str!("../../assets/extensions/core-languages/extension.toml"),
    include_str!("../../assets/extensions/pdf/extension.toml"),
    include_str!("../../assets/extensions/csv/extension.toml"),
    include_str!("../../assets/extensions/vim/extension.toml"),
    include_str!("../../assets/extensions/lsp-python/extension.toml"),
    include_str!("../../assets/extensions/lsp-typescript/extension.toml"),
    include_str!("../../assets/extensions/lsp-rust/extension.toml"),
    include_str!("../../assets/extensions/lsp-go/extension.toml"),
    include_str!("../../assets/extensions/lsp-yaml/extension.toml"),
    include_str!("../../assets/extensions/lsp-json/extension.toml"),
    include_str!("../../assets/extensions/lsp-html/extension.toml"),
    include_str!("../../assets/extensions/lsp-css/extension.toml"),
    include_str!("../../assets/extensions/lsp-bash/extension.toml"),
];

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
    /// Infrastructure extensions (e.g. `core-languages`) set this so they don't
    /// appear in the user-facing Extensions panel.
    #[serde(default)]
    pub hidden: bool,
    pub api_version: u32,
    #[serde(default)]
    pub languages: Vec<LanguageDecl>,
    #[serde(default)]
    pub language_servers: Vec<LanguageServerDecl>,
}

/// One `[[languages]]` entry: a language identity contributed by an extension.
/// Carries the data the closed `Language` enum used to hardcode — its LSP
/// `languageId`, the file extensions that map to it, the project-root markers,
/// and the server "family" it shares a managed server with.
#[derive(Debug, Clone, Deserialize)]
pub struct LanguageDecl {
    /// LSP `languageId` (e.g. `"python"`, `"typescriptreact"`). The language's
    /// stable identity.
    pub id: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Project-manifest filenames that mark a directory as this language's
    /// project root (anchors the server at the right sub-project).
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// The canonical language id this one shares a managed server with (e.g.
    /// `typescriptreact` -> `typescript`). Absent means the language is its own
    /// family.
    #[serde(default)]
    pub family: Option<String>,
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

/// A user-facing extension entry for the Extensions panel: identity and blurb,
/// with no contribution detail. Enabled/disabled state is held separately (in
/// prefs), so this stays a pure projection of the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub builtin: bool,
}

/// Project a set of manifest sources to the panel's extension list, skipping
/// unparseable and `hidden` (infrastructure) manifests.
pub fn summaries(sources: &[&str]) -> Vec<ExtensionSummary> {
    sources
        .iter()
        .filter_map(|s| parse(s).ok())
        .filter(|m| !m.hidden)
        .map(|m| ExtensionSummary {
            id: m.id,
            name: m.name,
            description: m.description,
            builtin: m.builtin,
        })
        .collect()
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
/// canonical one), mirroring the old behaviour where `ServerConfig::vtsls`
/// carried `Language::TypeScript` even when registered under the React/JS keys.
///
/// The language is built straight from the decl's id, not looked up in the
/// global language table — a manifest declaring a server for a brand-new
/// language must register it even before (or without) a matching `[[languages]]`
/// block, which is what lets a user add a language with zero Rust.
pub fn entries(manifest: &ExtensionManifest) -> Vec<ServerEntry> {
    let mut out = Vec::new();
    for decl in &manifest.language_servers {
        let lang_ids: Vec<&str> = decl
            .language
            .as_deref()
            .into_iter()
            .chain(decl.languages.iter().map(String::as_str))
            .collect();
        let Some(first) = lang_ids.first() else {
            continue;
        };
        let config = ServerConfig {
            name: intern(&decl.name),
            command: decl.command.clone(),
            args: decl.args.clone(),
            language: Language(intern(first)),
            initialization_options: decl.initialization_options.clone(),
            provision: decl.provision.as_ref().map(ProvisionDecl::to_provision),
        };
        for id in &lang_ids {
            out.push(ServerEntry {
                priority: decl.priority,
                language: Language(intern(id)),
                config: config.clone(),
            });
        }
    }
    out
}

/// The directory user-installed extensions live in: `<config>/extensions`
/// (e.g. `~/.config/croft/extensions`). Each extension is a subdirectory
/// holding an `extension.toml`.
pub fn user_extensions_dir() -> PathBuf {
    crate::prefs::config_dir().join("extensions")
}

/// Read every `<dir>/<id>/extension.toml` into a source string, sorted by path
/// for a deterministic load order. Best-effort: a missing directory or an
/// unreadable entry yields no source rather than an error, so a fresh box with
/// no user extensions simply gets the bundled set.
pub fn read_extension_sources(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut subdirs: Vec<PathBuf> = read
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    subdirs
        .iter()
        .filter_map(|p| std::fs::read_to_string(p.join("extension.toml")).ok())
        .collect()
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
        assert!(entries.iter().all(|e| e.language == Language::PYTHON));
    }

    #[test]
    fn bundled_summaries_list_user_facing_extensions_and_hide_infra() {
        let s = summaries(BUNDLED_MANIFESTS);
        let ids: Vec<&str> = s.iter().map(|e| e.id.as_str()).collect();
        // The user-facing built-ins appear...
        for id in ["pdf", "csv", "vim", "lsp-python", "lsp-rust"] {
            assert!(ids.contains(&id), "missing {id} in {ids:?}");
        }
        // ...and the hidden infrastructure manifest does not.
        assert!(!ids.contains(&"core-languages"));
        assert!(s.iter().all(|e| e.builtin));
    }

    #[test]
    fn reads_sorted_extension_manifests_skipping_subdirs_without_one() {
        use std::fs;
        let base = std::env::temp_dir().join(format!("croft-ext-read-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("bbb")).unwrap();
        fs::create_dir_all(base.join("aaa")).unwrap();
        fs::create_dir_all(base.join("no-manifest")).unwrap();
        fs::write(base.join("aaa/extension.toml"), "id='aaa'").unwrap();
        fs::write(base.join("bbb/extension.toml"), "id='bbb'").unwrap();
        let sources = read_extension_sources(&base);
        let _ = fs::remove_dir_all(&base);
        // Sorted by path, and the manifest-less subdir is skipped.
        assert_eq!(
            sources,
            vec!["id='aaa'".to_string(), "id='bbb'".to_string()]
        );
        // A missing directory is not an error.
        assert!(read_extension_sources(&base.join("gone")).is_empty());
    }
}
