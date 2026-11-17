//! Code actions: asking for them with the diagnostics at hand, resolving one, and running
//! a server command.

use super::*;

/// A command to run after a code action's edits, via `workspace/executeCommand`
/// (LSP's `Command`). Carries the opaque command id and its arguments verbatim.
#[derive(Debug, Clone)]
pub struct CodeActionCommand {
    pub command: String,
    pub arguments: Vec<serde_json::Value>,
}

/// One selectable entry in the Quick Fix menu, normalised from an LSP
/// `CodeActionOrCommand` into croft's own char-indexed edit representation so
/// the app layer never touches `lsp_types`. Mirrors how [`RenameResult`] carries
/// already-normalised edits.
#[derive(Debug, Clone)]
pub struct CodeActionItem {
    pub title: String,
    /// The language server that produced this action. A `codeAction/resolve`
    /// for the action must go back to the same server, and the title is shown
    /// as-is (servers already prefix, e.g. "Ruff: Organize imports").
    pub server: String,
    /// Per-file, char-indexed spans to apply (empty when the action has no
    /// inline edit; it then runs `command` and/or must be resolved first).
    pub edits: Vec<(PathBuf, Vec<TextSpanEdit>)>,
    /// A command to run after the edits (or instead of them), if any.
    pub command: Option<CodeActionCommand>,
    /// True when the action carries `data` but neither an inline edit nor a
    /// command, so its edit must be fetched via `codeAction/resolve` first.
    pub needs_resolve: bool,
    /// The full original `CodeAction` serialized to JSON, kept only when
    /// `needs_resolve`. `codeAction/resolve` must round-trip the WHOLE action
    /// (ruff rejects a resolve missing `kind` with "No kind was given"), so we
    /// send this back verbatim rather than reconstructing from `title`+`data`.
    pub resolve_action: Option<serde_json::Value>,
    /// The server's "preferred" hint (VS Code's auto-fix target).
    pub is_preferred: bool,
}

#[derive(Debug)]
pub struct CodeActionResult {
    pub request_id: u64,
    /// The available actions. `None` means the request errored; `Some(vec![])`
    /// means the server ran but offered nothing here. The same shape is reused
    /// for a `codeAction/resolve` reply (a single, now-resolved item).
    pub items: Option<Vec<CodeActionItem>>,
    /// True when no spawned server advertises a usable `codeActionProvider`, so
    /// the app can say "no quick fixes available" distinctly from an empty list.
    pub unsupported: bool,
}

impl LspManager {
    /// Request the quick fixes / refactors available over a range (use the same
    /// position for start and end for a caret request). `diagnostics` are the
    /// app's diagnostics overlapping the range, forwarded as context so the
    /// server can attach fixes to them. Reply arrives via [`Self::drain_code_action`].
    #[allow(clippy::too_many_arguments)]
    pub fn request_code_action(
        &mut self,
        path: PathBuf,
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
        diagnostics: Vec<Diagnostic>,
    ) -> u64 {
        self.send_request(|request_id| Cmd::RequestCodeAction {
            request_id,
            path,
            start_line,
            start_char,
            end_line,
            end_char,
            diagnostics,
        })
    }

    /// Resolve an action that arrived without its edit (it carried only `data`).
    /// The reply reuses [`CodeActionResult`] with a single, now-resolved item.
    pub fn request_code_action_resolve(
        &mut self,
        path: PathBuf,
        server: String,
        action: serde_json::Value,
    ) -> u64 {
        self.send_request(|request_id| Cmd::ResolveCodeAction {
            request_id,
            path,
            server,
            action,
        })
    }

    /// Fire a `workspace/executeCommand` for a command-style code action. The
    /// server performs the effect; croft does not await a reply.
    pub fn execute_command(
        &mut self,
        path: PathBuf,
        command: String,
        arguments: Vec<serde_json::Value>,
    ) {
        let _ = self.cmd_tx.send(Cmd::ExecuteCommand {
            path,
            command,
            arguments,
        });
    }

    pub fn drain_code_action(&self) -> Option<CodeActionResult> {
        self.code_action_rx.try_recv().ok()
    }

    /// Whether the server for `lang` implements `textDocument/codeAction`.
    /// `None` means no server has reported yet.
    pub fn language_supports_code_action(&self, lang: Language) -> Option<bool> {
        self.language_supports(lang, |s| &s.code_action)
    }
}

impl WorkerState {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn request_code_action(
        &mut self,
        request_id: u64,
        path: PathBuf,
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
        diagnostics: Vec<Diagnostic>,
        tx: &std_mpsc::Sender<CodeActionResult>,
    ) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            return;
        };
        // Aggregate across EVERY server that supports code actions, like VS Code:
        // for Python that is ty (type fixes), basedpyright (import fixes), and
        // ruff (lint quick-fixes + "Organize Imports" / "Fix all"). Picking only
        // the first server would silently drop ruff's source actions.
        let servers: Vec<(String, Arc<TokioMutex<LspClient>>)> = clients
            .iter()
            .filter(|c| c.supports_code_action)
            .map(|c| (c.name.clone(), c.client.clone()))
            .collect();
        if servers.is_empty() {
            let _ = tx.send(CodeActionResult {
                request_id,
                items: None,
                unsupported: true,
            });
            return;
        }
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let range = lsp_types::Range {
            start: lsp_types::Position {
                line: start_line,
                character: start_char,
            },
            end: lsp_types::Position {
                line: end_line,
                character: end_char,
            },
        };
        let tx = tx.clone();
        log_file::log(&format!(
            "code_action request id={request_id} servers={} path={} line={start_line}",
            servers.len(),
            path.display()
        ));
        tokio::spawn(async move {
            let mut combined: Vec<CodeActionItem> = Vec::new();
            let mut any_ran = false;
            for (server_name, client_arc) in servers {
                let diags = code_action_diagnostics(&diagnostics, &server_name);
                let mut client = client_arc.lock().await.requests();
                let resp = client.code_action(uri.clone(), range, diags).await;
                drop(client);
                match resp {
                    Ok(Some(r)) => {
                        any_ran = true;
                        combined.extend(code_action_items(&r, &server_name));
                    }
                    Ok(None) => any_ran = true,
                    Err(e) => {
                        log_file::log(&format!("lsp[{server_name}] code_action error: {e:#}"));
                    }
                }
            }
            // Preferred actions (e.g. the server's chosen auto-fix) float to the
            // top of the menu, matching VS Code's ordering.
            combined.sort_by_key(|i| !i.is_preferred);
            log_file::log(&format!(
                "code_action response id={request_id} count={} any_ran={any_ran}",
                combined.len()
            ));
            let _ = tx.send(CodeActionResult {
                request_id,
                // `None` only when every server errored; an empty Vec means a
                // server answered with nothing.
                items: if any_ran { Some(combined) } else { None },
                unsupported: false,
            });
        });
    }

    pub(super) async fn resolve_code_action(
        &mut self,
        request_id: u64,
        path: PathBuf,
        server: String,
        action: serde_json::Value,
        tx: &std_mpsc::Sender<CodeActionResult>,
    ) {
        // Deserialize the original action back intact (kind/data/diagnostics all
        // preserved); a malformed payload simply yields no resolution.
        let Ok(action) = serde_json::from_value::<lsp_types::CodeAction>(action) else {
            let _ = tx.send(CodeActionResult {
                request_id,
                items: None,
                unsupported: false,
            });
            return;
        };
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            return;
        };
        // Resolve against the SAME server that produced the action; fall back to
        // any code-action server only if that one is gone.
        let picked = clients
            .iter()
            .find(|c| c.supports_code_action && c.name == server)
            .or_else(|| clients.iter().find(|c| c.supports_code_action))
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            let _ = tx.send(CodeActionResult {
                request_id,
                items: None,
                unsupported: true,
            });
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resolved = client.code_action_resolve(action).await;
            drop(client);
            let items = match resolved {
                Ok(a) => {
                    let one = vec![CodeActionOrCommand::CodeAction(a)];
                    Some(code_action_items(&one, &server_name))
                }
                Err(e) => {
                    log_file::log(&format!(
                        "lsp[{server_name}] code_action_resolve error: {e:#}"
                    ));
                    None
                }
            };
            let _ = tx.send(CodeActionResult {
                request_id,
                items,
                unsupported: false,
            });
        });
    }

    pub(super) async fn execute_command(
        &mut self,
        path: PathBuf,
        command: String,
        arguments: Vec<serde_json::Value>,
    ) {
        let Ok((server_name, client_arc)) =
            self.pick_server(&path, |c| c.supports_code_action).await
        else {
            return;
        };
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            if let Err(e) = client.execute_command(command, arguments).await {
                log_file::log(&format!("lsp[{server_name}] execute_command error: {e:#}"));
            }
        });
    }
}

/// Normalise an LSP `textDocument/codeAction` response into croft's selectable
/// [`CodeActionItem`]s. Disabled actions are dropped (VS Code hides them from
/// the lightbulb menu); inline `WorkspaceEdit`s are flattened with the same
/// [`workspace_edits`] path rename uses; an action with neither an edit nor a
/// command but with `data` is flagged for a `codeAction/resolve` round trip.
pub(super) fn code_action_items(resp: &CodeActionResponse, server: &str) -> Vec<CodeActionItem> {
    let to_command = |c: &lsp_types::Command| CodeActionCommand {
        command: c.command.clone(),
        arguments: c.arguments.clone().unwrap_or_default(),
    };
    resp.iter()
        .filter_map(|entry| match entry {
            CodeActionOrCommand::Command(cmd) => Some(CodeActionItem {
                title: cmd.title.clone(),
                server: server.to_string(),
                edits: Vec::new(),
                command: Some(to_command(cmd)),
                needs_resolve: false,
                resolve_action: None,
                is_preferred: false,
            }),
            CodeActionOrCommand::CodeAction(action) => {
                if action.disabled.is_some() {
                    return None;
                }
                let edits = action
                    .edit
                    .as_ref()
                    .map(workspace_edits)
                    .unwrap_or_default();
                let command = action.command.as_ref().map(to_command);
                let needs_resolve =
                    action.edit.is_none() && command.is_none() && action.data.is_some();
                // Keep the whole action so resolve can send it back intact.
                let resolve_action = if needs_resolve {
                    serde_json::to_value(action).ok()
                } else {
                    None
                };
                Some(CodeActionItem {
                    title: action.title.clone(),
                    server: server.to_string(),
                    edits,
                    command,
                    needs_resolve,
                    resolve_action,
                    is_preferred: action.is_preferred.unwrap_or(false),
                })
            }
        })
        .collect()
}

/// Re-materialise one of croft's normalised diagnostics into the LSP wire type
/// so it can ride along as `codeAction` context. We carry range, severity,
/// message and tags (the fields croft keeps). A server's own diagnostics go
/// back as it sent them instead (`code_action_diagnostics`): its fixes key
/// on `code` and `data`, which this leaves out.
pub(super) fn to_lsp_diagnostic(d: &Diagnostic) -> lsp_types::Diagnostic {
    let severity = match d.severity {
        DiagnosticSeverity::Error => lsp_types::DiagnosticSeverity::ERROR,
        DiagnosticSeverity::Warning => lsp_types::DiagnosticSeverity::WARNING,
        DiagnosticSeverity::Information => lsp_types::DiagnosticSeverity::INFORMATION,
        DiagnosticSeverity::Hint => lsp_types::DiagnosticSeverity::HINT,
    };
    lsp_types::Diagnostic {
        range: lsp_types::Range {
            start: lsp_types::Position {
                line: d.start_line,
                character: d.start_char,
            },
            end: lsp_types::Position {
                line: d.end_line,
                character: d.end_char,
            },
        },
        severity: Some(severity),
        message: d.message.clone(),
        tags: d.tags.any().then(|| {
            let mut tags = Vec::new();
            if d.tags.unnecessary {
                tags.push(lsp_types::DiagnosticTag::UNNECESSARY);
            }
            if d.tags.deprecated {
                tags.push(lsp_types::DiagnosticTag::DEPRECATED);
            }
            tags
        }),
        ..Default::default()
    }
}

/// The diagnostics `server` is asked for code actions on: its own, as it
/// published them, and the ones croft made itself, rebuilt. Another server's
/// diagnostics are not its to fix, as in VS Code (#1030).
pub(super) fn code_action_diagnostics(
    diagnostics: &[Diagnostic],
    server: &str,
) -> Vec<lsp_types::Diagnostic> {
    diagnostics
        .iter()
        .filter_map(|d| match d.origin.as_deref() {
            Some(origin) => (origin.server == server).then(|| origin.wire.clone()),
            None => Some(to_lsp_diagnostic(d)),
        })
        .collect()
}

/// Whether the server advertises a usable `textDocument/codeAction` provider.
/// `Simple(false)` and `None` are unsupported; `Simple(true)` or an options
/// object (which may also declare `resolveProvider`) is supported.
pub(super) fn code_action_supported(cap: &Option<CodeActionProviderCapability>) -> bool {
    match cap {
        Some(CodeActionProviderCapability::Simple(b)) => *b,
        Some(CodeActionProviderCapability::Options(_)) => true,
        None => false,
    }
}
