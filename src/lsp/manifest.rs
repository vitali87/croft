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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::lsp::config::{Language, ServerConfig};
use crate::lsp::install::{ArchiveKind, Provision};

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
    include_str!("../../assets/extensions/lsp-toml/extension.toml"),
    include_str!("../../assets/extensions/lsp-cpp/extension.toml"),
    include_str!("../../assets/extensions/dap-python/extension.toml"),
    include_str!("../../assets/extensions/dap-lldb/extension.toml"),
    include_str!("../../assets/extensions/dap-js/extension.toml"),
    include_str!("../../assets/extensions/themes/extension.toml"),
];

/// The curated MCP-server catalog: vetted sidecars a user can *add* from the
/// Extensions panel (Available → Add → Installed). Unlike [`BUNDLED_MANIFESTS`]
/// these are NOT loaded as active extensions; adding one writes its manifest
/// into the user extensions dir, after which it loads like any installed
/// extension. Same `extension.toml` format. Each is a keyless (or opt-in)
/// server croft can drive through its single-argument command model.
pub const CATALOG_MANIFESTS: &[&str] = &[
    include_str!("../../assets/catalog/mcp-fetch/extension.toml"),
    include_str!("../../assets/catalog/mcp-time/extension.toml"),
    include_str!("../../assets/catalog/mcp-markitdown/extension.toml"),
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
    /// Color themes this extension contributes. An extension can ship languages,
    /// servers, AND/OR themes; a pure theme extension declares only these.
    #[serde(default)]
    pub themes: Vec<ThemeDecl>,
    /// Debug adapters this extension contributes. Each maps a set of file
    /// extensions to one of croft's built-in launch mechanisms (its `kind`).
    /// Disabling the extension stops F5 from offering that debugger.
    #[serde(default)]
    pub debug_adapters: Vec<DebugAdapterDecl>,
    /// MCP sidecar servers this extension contributes (Tier-1 extensions). Each
    /// is a local process croft spawns lazily and drives over JSON-RPC/NDJSON
    /// stdio. Paired with [`commands`](Self::commands) that invoke their tools.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerDecl>,
    /// Palette commands this extension contributes. Each invokes one tool on one
    /// of this extension's [`mcp_servers`](Self::mcp_servers); registered eagerly
    /// in the command palette, the server is spawned lazily on first invocation.
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
}

/// One `[[mcp_servers]]` entry: a sidecar server croft spawns and drives over
/// MCP. `command`/`args` are the spawn line; `provision` (reused from the LSP
/// backends) installs it pinned into a host-managed dir when absent from PATH,
/// never fetching-at-launch; `env` is the explicit least-privilege environment
/// handed to the process (croft does not leak its own environment).
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerDecl {
    /// Stable id referenced by a [`CommandDecl::server`].
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub provision: Option<ProvisionDecl>,
    /// Explicit environment variables the server is launched with (e.g. an API
    /// key sourced by the host). Empty by default.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// One `[[commands]]` entry: a palette command that calls `tool` on `server`.
/// `prompt`, when set, is the label of a single string argument croft collects
/// from the user (e.g. a URL or search query) and passes to the tool.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandDecl {
    /// Stable command id (e.g. `fetch.url`).
    pub id: String,
    /// Human-facing palette label.
    pub title: String,
    /// The [`McpServerDecl::id`] this command's tool lives on.
    pub server: String,
    /// The MCP tool name to call.
    pub tool: String,
    /// When set, the tool's argument NAME that the collected input fills (e.g.
    /// `url`). Paired with `prompt`. Absent for a no-argument tool.
    #[serde(default)]
    pub arg: Option<String>,
    /// When set, the human label of the single string argument to collect from
    /// the user before calling the tool (e.g. `URL`). Absent for a no-argument
    /// tool.
    #[serde(default)]
    pub prompt: Option<String>,
}

/// One `[[debug_adapters]]` entry: a debugger contributed by an extension. The
/// `kind` selects which built-in launch mechanism croft drives (debugpy /
/// lldb-dap / vscode-js-debug — the launch flows are heterogeneous and stay
/// native, like the PDF/CSV viewers); the `extensions` list is the data that
/// used to live in the hardcoded `adapter_for_extension` match.
#[derive(Debug, Clone, Deserialize)]
pub struct DebugAdapterDecl {
    /// Stable id (matches the contributing extension's id for the toggle, e.g.
    /// the `debugpy` adapter id; the panel toggle keys on the manifest `id`).
    pub id: String,
    /// Human-facing adapter name (shown in docs / future debugger UI).
    pub label: String,
    /// Which built-in launch mechanism croft drives for this adapter.
    pub kind: AdapterKindDecl,
    /// Lowercased, dot-less file extensions this adapter debugs.
    #[serde(default)]
    pub extensions: Vec<String>,
}

/// The built-in debug-adapter launch mechanisms croft knows. Mirrors (and maps
/// onto) `crate::dap::session::AdapterKind`; kept here as pure manifest data so
/// the manifest layer carries no dependency on the dap module.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AdapterKindDecl {
    /// debugpy (CPython 3.14+), launches `.py` source under an interpreter.
    Debugpy,
    /// lldb-dap, builds then launches a compiled binary (Rust / C / C++).
    Lldb,
    /// vscode-js-debug, launches a Node program over its TCP multi-session.
    JsDebug,
}

/// One `[[themes]]` entry: a complete IDE color palette. All colors are
/// `#rrggbb` strings; `gradient` selects the teal brand/gradient chrome scheme
/// (vs the flat-accent scheme). croft's first-party Black/Dark-Blue ship as two
/// of these; a third party adds a theme by dropping a manifest with another.
#[derive(Debug, Clone, Deserialize)]
pub struct ThemeDecl {
    /// Stable id persisted in prefs (e.g. `"black"`).
    pub id: String,
    /// Human-facing name shown in the theme picker.
    pub label: String,
    /// Editor/panel background (the `SetColors` session fill).
    pub background: String,
    /// Primary accent (selected-row text, active chrome).
    pub accent: String,
    /// Selected-row fill in lists/popups.
    pub selection: String,
    /// Filter/search input fill.
    pub search: String,
    /// Primary-button / lit-toggle fill.
    pub button: String,
    /// Whether this theme uses the gradient brand chrome (focused-pane gradient
    /// border, popup gradient, brand accents) vs the flat-accent look.
    #[serde(default)]
    pub gradient: bool,
    /// On-screen-keyboard normal key cap fill (Termux).
    pub osk_key: String,
    /// On-screen-keyboard special key cap fill (Termux).
    pub osk_special: String,
    /// On-screen-keyboard armed (held) key fill (Termux).
    pub osk_armed: String,
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
/// `npm`/`uv` use `package`; `binary` uses `url` + `archive` + the OS/arch token
/// maps. Unused fields for a given kind are simply absent.
#[derive(Debug, Clone, Deserialize)]
pub struct ProvisionDecl {
    pub kind: ProvisionKind,
    /// npm/uv package name (unused by `binary`).
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub bin: String,
    /// `binary`: supported platform -> literal download URL. Keys are
    /// `"<os>-<arch>"` (e.g. `linux-x86_64`) or a bare `"<os>"` (e.g. `macos`
    /// for a universal build), using Rust's `target_os`/`target_arch` tokens.
    /// Host-agnostic: each URL is a full literal (GitHub, Codeberg, anywhere).
    #[serde(default)]
    pub targets: BTreeMap<String, String>,
    /// `binary`: archive format of the downloaded asset.
    #[serde(default)]
    pub archive: Option<ArchiveKindDecl>,
    /// `binary`: literal path to the executable inside the unpacked archive.
    /// Absent for a single-file `.gz`.
    #[serde(default)]
    pub bin_path: Option<String>,
    /// `binary`: Termux/Android package name for `pkg install`, used when the
    /// cross-distro release can't run on Android (absent → PATH fallback).
    #[serde(default)]
    pub termux_pkg: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProvisionKind {
    Npm,
    Uv,
    Binary,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveKindDecl {
    Gz,
    Zip,
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

/// Leak a parsed string map to `&'static [(&'static str, &'static str)]` (the
/// OS/arch token form `Provision::Binary` carries). Same soundness as
/// [`intern`]: the data lives for the whole process and is bounded by the
/// installed extension count.
fn intern_pairs(m: &BTreeMap<String, String>) -> &'static [(&'static str, &'static str)] {
    let v: Vec<(&'static str, &'static str)> =
        m.iter().map(|(k, val)| (intern(k), intern(val))).collect();
    Box::leak(v.into_boxed_slice())
}

impl ProvisionDecl {
    pub(crate) fn to_provision(&self) -> Provision {
        let version = self.version.as_deref().map(intern);
        let bin = intern(&self.bin);
        let package = || intern(self.package.as_deref().unwrap_or_default());
        match self.kind {
            ProvisionKind::Npm => Provision::Npm {
                package: package(),
                version,
                bin,
            },
            ProvisionKind::Uv => Provision::Uv {
                package: package(),
                version,
                bin,
            },
            ProvisionKind::Binary => Provision::Binary {
                targets: intern_pairs(&self.targets),
                bin,
                archive: match self.archive.unwrap_or(ArchiveKindDecl::Gz) {
                    ArchiveKindDecl::Gz => ArchiveKind::Gz,
                    ArchiveKindDecl::Zip => ArchiveKind::Zip,
                },
                bin_path: self.bin_path.as_deref().map(intern),
                termux_pkg: self.termux_pkg.as_deref().map(intern),
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
        // The user-facing built-ins appear (incl. the color-themes extension)...
        for id in ["pdf", "csv", "vim", "lsp-python", "lsp-rust", "themes"] {
            assert!(ids.contains(&id), "missing {id} in {ids:?}");
        }
        // ...and the hidden infrastructure manifest does not.
        assert!(!ids.contains(&"core-languages"));
        assert!(s.iter().all(|e| e.builtin));
    }

    #[test]
    fn parses_an_mcp_sidecar_manifest() {
        let src = r#"
id = "mcp-fetch"
name = "Fetch"
api_version = 1

[[mcp_servers]]
id = "fetch"
command = "mcp-server-fetch"
args = ["--quiet"]
env = { USER_AGENT = "croft" }

[[commands]]
id = "fetch.url"
title = "Fetch: URL to Markdown"
server = "fetch"
tool = "fetch"
prompt = "URL"
"#;
        let m = parse(src).expect("sidecar manifest parses");
        assert_eq!(m.mcp_servers.len(), 1);
        assert_eq!(m.mcp_servers[0].id, "fetch");
        assert_eq!(m.mcp_servers[0].command, "mcp-server-fetch");
        assert_eq!(m.mcp_servers[0].env.get("USER_AGENT").unwrap(), "croft");
        assert_eq!(m.commands.len(), 1);
        let c = &m.commands[0];
        assert_eq!(
            (c.id.as_str(), c.server.as_str(), c.tool.as_str()),
            ("fetch.url", "fetch", "fetch")
        );
        assert_eq!(c.prompt.as_deref(), Some("URL"));
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
