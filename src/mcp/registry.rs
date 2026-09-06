//! Data-driven MCP command/server registry.
//!
//! Mirrors `lsp::registry` and `dap::registry`: the palette commands a sidecar
//! extension contributes, and the servers that back them, are read from
//! `[[commands]]` / `[[mcp_servers]]` blocks in the bundled + user extension
//! manifests, skipping any extension disabled in the Extensions panel.
//!
//! Two reads serve two phases. [`contributed_commands`] is the eager list the
//! command palette registers at startup (so a command is discoverable before its
//! server ever spawns). [`resolve_command`] is the lazy lookup the app runs when
//! a command is invoked: it returns the tool to call plus the spawn intent for
//! its server (program/args/env and the pinned [`Provision`]), which the app
//! then ensures-installed and spawns. A command whose extension is disabled
//! resolves to nothing.

use std::collections::BTreeSet;

use crate::lsp::install::Provision;
use crate::lsp::manifest::{self, CommandDecl, McpServerDecl};

/// A contributed palette command's identity, for eager registration. The
/// `ext_id` is the contributing extension (the disabled-toggle key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributedCommand {
    pub ext_id: String,
    pub id: String,
    pub title: String,
}

/// How to spawn a contributed command's MCP server: the program line, the
/// least-privilege env, and the pinned provisioning when it isn't on PATH.
#[derive(Debug, Clone)]
pub struct ServerSpawn {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub provision: Option<Provision>,
}

/// A fully-resolved command ready to execute: which tool to call on which
/// (spawnable) server, and whether to collect a prompt argument first.
#[derive(Debug, Clone)]
pub struct ResolvedCommand {
    pub ext_id: String,
    pub command_id: String,
    pub title: String,
    pub tool: String,
    /// The tool argument name the collected input fills (paired with `prompt`).
    pub arg: Option<String>,
    pub prompt: Option<String>,
    pub server: ServerSpawn,
}

fn server_spawn(decl: &McpServerDecl) -> ServerSpawn {
    ServerSpawn {
        id: decl.id.clone(),
        command: decl.command.clone(),
        args: decl.args.clone(),
        env: decl.env.clone(),
        provision: decl.provision.as_ref().map(|p| p.to_provision()),
    }
}

/// Bundled + user manifest sources, in load order.
fn all_sources() -> Vec<String> {
    let mut sources: Vec<String> = manifest::BUNDLED_MANIFESTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    sources.extend(manifest::read_extension_sources(
        &manifest::user_extensions_dir(),
    ));
    sources
}

/// Pure: every contributed command across `sources` whose extension is enabled,
/// in source order (bundled before user).
fn contributed_in(sources: &[String], disabled: &BTreeSet<String>) -> Vec<ContributedCommand> {
    sources
        .iter()
        .filter_map(|s| manifest::parse(s).ok())
        .filter(|m| !disabled.contains(&m.id))
        .flat_map(|m| {
            let ext_id = m.id;
            m.commands.into_iter().map(move |c| ContributedCommand {
                ext_id: ext_id.clone(),
                id: c.id,
                title: c.title,
            })
        })
        .collect()
}

/// Pure: resolve `command_id` to its tool + spawnable server, skipping disabled
/// extensions. The server is looked up by id within the SAME manifest that
/// declares the command (an extension backs its own commands). First match wins.
fn resolve_in(
    sources: &[String],
    disabled: &BTreeSet<String>,
    command_id: &str,
) -> Option<ResolvedCommand> {
    for src in sources {
        let Ok(m) = manifest::parse(src) else {
            continue;
        };
        if disabled.contains(&m.id) {
            continue;
        }
        let Some(cmd) = m.commands.iter().find(|c| c.id == command_id) else {
            continue;
        };
        let server = m.mcp_servers.iter().find(|s| s.id == cmd.server)?;
        return Some(resolved(&m.id, cmd, server));
    }
    None
}

fn resolved(ext_id: &str, cmd: &CommandDecl, server: &McpServerDecl) -> ResolvedCommand {
    ResolvedCommand {
        ext_id: ext_id.to_string(),
        command_id: cmd.id.clone(),
        title: cmd.title.clone(),
        tool: cmd.tool.clone(),
        arg: cmd.arg.clone(),
        prompt: cmd.prompt.clone(),
        server: server_spawn(server),
    }
}

/// One `[[viewers]]` entry from an enabled extension (#465): a terminal
/// program that opens a file kind in a pane of its own.
#[derive(Clone)]
pub struct ContributedViewer {
    pub ext_id: String,
    pub id: String,
    pub label: String,
    pub command: String,
    pub args: Vec<String>,
    pub extensions: Vec<String>,
    pub provision: Option<crate::lsp::install::Provision>,
}

/// Palette command id for a viewer: `viewer:<id>`.
pub const VIEWER_COMMAND_PREFIX: &str = "viewer:";

/// Pure: every viewer across `sources` whose extension is enabled, in
/// source order. Extensions are lower-cased once here so a lookup is a
/// plain comparison.
fn viewers_in(sources: &[String], disabled: &BTreeSet<String>) -> Vec<ContributedViewer> {
    sources
        .iter()
        .filter_map(|s| manifest::parse(s).ok())
        .filter(|m| !disabled.contains(&m.id))
        .flat_map(|m| {
            let ext_id = m.id;
            m.viewers.into_iter().map(move |v| ContributedViewer {
                ext_id: ext_id.clone(),
                id: v.id,
                label: v.label,
                command: v.command,
                args: v.args,
                extensions: v.extensions.iter().map(|e| e.to_lowercase()).collect(),
                provision: v.provision.as_ref().map(|p| p.to_provision()),
            })
        })
        .collect()
}

/// Pure: the first enabled viewer offered for `path`'s extension, compared
/// case-insensitively (`data.CSV` is a CSV).
fn viewer_for_path_in(
    sources: &[String],
    disabled: &BTreeSet<String>,
    path: &std::path::Path,
) -> Option<ContributedViewer> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    viewers_in(sources, disabled)
        .into_iter()
        .find(|v| v.extensions.contains(&ext))
}

/// Pure: the palette rows for every enabled viewer, titled
/// `<extension id>: <label>` with id `viewer:<id>`.
fn viewer_commands_in(sources: &[String], disabled: &BTreeSet<String>) -> Vec<ContributedCommand> {
    viewers_in(sources, disabled)
        .into_iter()
        .map(|v| ContributedCommand {
            title: format!("{}: {}", v.ext_id, v.label),
            id: format!("{VIEWER_COMMAND_PREFIX}{}", v.id),
            ext_id: v.ext_id,
        })
        .collect()
}

/// The enabled viewer for `path`, if any extension contributes one.
pub fn viewer_for_path(path: &std::path::Path) -> Option<ContributedViewer> {
    let disabled = crate::prefs::Prefs::load_or_default().disabled_extensions;
    viewer_for_path_in(&all_sources(), &disabled, path)
}

/// The enabled viewer with this id, if any.
pub fn viewer_by_id(id: &str) -> Option<ContributedViewer> {
    let disabled = crate::prefs::Prefs::load_or_default().disabled_extensions;
    viewers_in(&all_sources(), &disabled)
        .into_iter()
        .find(|v| v.id == id)
}

/// Palette rows for the enabled viewers.
pub fn contributed_viewer_commands() -> Vec<ContributedCommand> {
    let disabled = crate::prefs::Prefs::load_or_default().disabled_extensions;
    viewer_commands_in(&all_sources(), &disabled)
}

/// The contributed commands to register eagerly in the palette (enabled
/// extensions only). Reads prefs + manifests fresh; invoked when the palette is
/// built, not on a hot path.
pub fn contributed_commands() -> Vec<ContributedCommand> {
    let disabled = crate::prefs::Prefs::load_or_default().disabled_extensions;
    contributed_in(&all_sources(), &disabled)
}

/// Resolve a contributed command id to its executable form, or `None` if no
/// enabled extension contributes it.
pub fn resolve_command(command_id: &str) -> Option<ResolvedCommand> {
    let disabled = crate::prefs::Prefs::load_or_default().disabled_extensions;
    resolve_in(&all_sources(), &disabled, command_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSVLENS: &str = r#"id = "csvlens"
name = "csvlens"
description = "Browse CSV and TSV files in csvlens, a terminal spreadsheet viewer."
builtin = false
api_version = 1

[[viewers]]
id = "csvlens"
label = "Open in csvlens"
command = "csvlens"
args = ["{file}"]
extensions = ["csv", "tsv"]
provision = { kind = "binary", bin = "csvlens", archive = "tar.xz", targets = { "macos-aarch64" = "https://example.invalid/csvlens-aarch64-apple-darwin.tar.xz" } }
"#;

    /// #465: a viewer is offered for its file kinds, case-insensitively, and
    /// only while its extension is enabled; it reaches the palette as one row.
    #[test]
    fn an_enabled_viewer_is_offered_for_its_file_kinds_and_a_disabled_one_is_not() {
        let sources = vec![CSVLENS.to_string()];
        let none = BTreeSet::new();
        let v = viewer_for_path_in(&sources, &none, std::path::Path::new("/tmp/data.CSV"))
            .expect("csv is one of the viewer's kinds");
        assert_eq!(
            (v.ext_id.as_str(), v.id.as_str(), v.command.as_str()),
            ("csvlens", "csvlens", "csvlens")
        );
        assert_eq!(v.args, vec!["{file}"]);
        assert!(
            viewer_for_path_in(&sources, &none, std::path::Path::new("/tmp/notes.md")).is_none(),
            "a kind no viewer claims gets nothing"
        );
        let disabled: BTreeSet<String> = ["csvlens".to_string()].into_iter().collect();
        assert!(
            viewer_for_path_in(&sources, &disabled, std::path::Path::new("/tmp/data.csv"))
                .is_none(),
            "a disabled extension contributes nothing"
        );
        let cmds = viewer_commands_in(&sources, &none);
        assert_eq!(cmds.len(), 1, "one palette row per viewer");
        assert_eq!(
            (
                cmds[0].ext_id.as_str(),
                cmds[0].id.as_str(),
                cmds[0].title.as_str()
            ),
            ("csvlens", "viewer:csvlens", "csvlens: Open in csvlens")
        );
        assert!(viewer_commands_in(&sources, &disabled).is_empty());
    }

    const FETCH: &str = r#"
id = "mcp-fetch"
name = "Fetch"
api_version = 1
[[mcp_servers]]
id = "fetch"
command = "mcp-server-fetch"
provision = { kind = "uv", package = "mcp-server-fetch", version = "0.1.0", bin = "mcp-server-fetch" }
[[commands]]
id = "fetch.url"
title = "Fetch: URL to Markdown"
server = "fetch"
tool = "fetch"
prompt = "URL"
"#;

    fn sources() -> Vec<String> {
        vec![FETCH.to_string()]
    }

    #[test]
    fn contributed_lists_enabled_commands_and_skips_disabled() {
        let s = sources();
        let none = BTreeSet::new();
        let cmds = contributed_in(&s, &none);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].id, "fetch.url");
        assert_eq!(cmds[0].title, "Fetch: URL to Markdown");
        assert_eq!(cmds[0].ext_id, "mcp-fetch");

        let mut disabled = BTreeSet::new();
        disabled.insert("mcp-fetch".to_string());
        assert!(contributed_in(&s, &disabled).is_empty());
    }

    #[test]
    fn resolve_returns_tool_prompt_and_spawnable_server() {
        let s = sources();
        let none = BTreeSet::new();
        let r = resolve_in(&s, &none, "fetch.url").expect("resolves");
        assert_eq!(r.tool, "fetch");
        assert_eq!(r.prompt.as_deref(), Some("URL"));
        assert_eq!(r.server.id, "fetch");
        assert_eq!(r.server.command, "mcp-server-fetch");
        // The provision decl maps to a real uv Provision (pinned, host-managed).
        assert!(matches!(r.server.provision, Some(Provision::Uv { .. })));
    }

    #[test]
    fn resolve_skips_disabled_and_unknown() {
        let s = sources();
        let mut disabled = BTreeSet::new();
        disabled.insert("mcp-fetch".to_string());
        assert!(resolve_in(&s, &disabled, "fetch.url").is_none());

        let none = BTreeSet::new();
        assert!(resolve_in(&s, &none, "nope.command").is_none());
    }

    #[test]
    fn resolve_returns_none_when_command_references_an_absent_server() {
        let src = r#"
id = "mcp-broken"
name = "Broken"
api_version = 1
[[commands]]
id = "x.run"
title = "X"
server = "missing"
tool = "t"
"#;
        let s = vec![src.to_string()];
        let none = BTreeSet::new();
        assert!(resolve_in(&s, &none, "x.run").is_none());
    }
}
