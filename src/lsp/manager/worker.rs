//! The worker loop: the manager's thread that takes each command and routes it to the
//! servers.

use super::*;

// Every flag and channel here is an independent worker input, the same shape
// `LspClient::spawn` carries: bundling them behind one struct would add a
// layer of indirection without making any call site clearer. `ResultSenders`
// already bundles the part that grows per REQUEST KIND; these do not grow
// that way.
#[allow(clippy::too_many_arguments)]
pub(super) async fn worker_loop(
    workspace_root: PathBuf,
    registry: ServerRegistry,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Cmd>,
    tx: ResultSenders,
    capability_support: CapabilitySupport,
    semantic_refresh: Arc<AtomicBool>,
    inlay_refresh: Arc<AtomicBool>,
    diagnostic_refresh: Arc<AtomicBool>,
    servers_restarted: Arc<AtomicBool>,
) {
    let mut state = WorkerState {
        workspace_root,
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support,
        semantic_refresh,
        inlay_refresh,
        diagnostic_refresh,
        diagnostics_tx: tx.diagnostics.clone(),
        progress_tx: tx.progress.clone(),
        restarts: ServerRestarts::reporting_to(servers_restarted),
    };
    while let Some(cmd) = cmd_rx.recv().await {
        // Before any command touches a server: one the kernel killed, or
        // that crashed, comes back here rather than staying down (#694).
        if !matches!(cmd, Cmd::Shutdown { .. }) {
            state.restart_dead_servers().await;
        }
        match cmd {
            Cmd::Shutdown { done } => {
                state.shutdown_all().await;
                let _ = done.send(());
                return;
            }
            Cmd::OpenDoc { path, text } => state.open_doc(path, text).await,
            Cmd::RestartServersFor { path } => state.restart_servers_for(&path).await,
            Cmd::ReplaceRegistry { registry } => state.replace_registry(registry).await,
            Cmd::PrewarmWorkspace => state.prewarm_workspace().await,
            Cmd::SetExtraRoots { roots } => {
                for root in &roots {
                    if !state.extra_roots.contains(root) {
                        state.prewarm_root(root.clone()).await;
                    }
                }
                // Retire clients whose project root lives under a REMOVED
                // workspace root and serves no open document (#148 review):
                // without this, repeated add/remove cycles accumulate live
                // server processes and workspace-symbol fan-outs keep
                // querying folders the user closed. A client still serving
                // an open doc survives — the file became out-of-workspace,
                // not closed.
                let removed: Vec<PathBuf> = state
                    .extra_roots
                    .iter()
                    .filter(|old_root| !roots.contains(old_root))
                    .cloned()
                    .collect();
                if !removed.is_empty() {
                    let doomed: Vec<ClientKey> = state
                        .clients
                        .keys()
                        .filter(|(_, project_root)| {
                            removed.iter().any(|r| project_root.starts_with(r))
                                && !state.docs.values().any(|d| &d.project_root == project_root)
                        })
                        .cloned()
                        .collect();
                    for key in doomed {
                        if let Some(clients) = state.clients.remove(&key) {
                            for managed in clients {
                                let mut client = managed.client.lock().await;
                                // Graceful LSP shutdown+exit; the transport's
                                // kill_on_drop reaps a server that ignores it.
                                // Bounded: a server that never answers
                                // `shutdown` must not wedge the worker loop.
                                let _ = tokio::time::timeout(
                                    std::time::Duration::from_secs(3),
                                    client.shutdown(),
                                )
                                .await;
                                log_file::log(&format!(
                                    "lsp retire: {} for removed workspace folder {}",
                                    managed.name,
                                    key.1.display()
                                ));
                            }
                        }
                    }
                }
                state.extra_roots = roots;
            }
            Cmd::RequestWorkspaceDiagnostics => state.request_workspace_diagnostics(),
            Cmd::RequestSemanticTokens {
                path,
                seq,
                generation,
            } => {
                state
                    .request_semantic_tokens(path, seq, generation, &tx.semantic_tokens)
                    .await
            }
            Cmd::RequestSemanticTokensRange {
                path,
                start_line,
                end,
                seq,
                generation,
            } => {
                state
                    .request_semantic_tokens_range(
                        path,
                        start_line,
                        end,
                        seq,
                        generation,
                        &tx.semantic_tokens,
                    )
                    .await
            }
            Cmd::RequestInlayHints { path, end, seq } => {
                state
                    .request_inlay_hints(path, end, seq, &tx.inlay_hints)
                    .await
            }
            Cmd::RequestCodeLens { path, seq } => {
                state.request_code_lens(path, seq, &tx.code_lens).await
            }
            Cmd::RequestDocumentLinks { path, seq } => {
                state
                    .request_document_links(path, seq, &tx.document_links)
                    .await
            }
            Cmd::RequestFoldingRanges { path, seq } => {
                state
                    .request_folding_ranges(path, seq, &tx.folding_ranges)
                    .await
            }
            Cmd::RequestDocumentColors { path, seq } => {
                state
                    .request_document_colors(path, seq, &tx.document_colors)
                    .await
            }
            Cmd::RequestColorPresentations {
                request_id,
                path,
                color,
                start,
                end,
            } => {
                state
                    .request_color_presentations(
                        request_id,
                        path,
                        color,
                        start,
                        end,
                        &tx.color_presentations,
                    )
                    .await
            }
            Cmd::ChangeDoc { path, text } => state.change_doc(path, text).await,
            Cmd::SaveDoc { path, text } => state.save_doc(path, text).await,
            Cmd::CloseDoc { path } => state.close_doc(path).await,
            Cmd::RequestCompletion {
                request_id,
                path,
                line,
                character,
                trigger,
            } => {
                state
                    .request_completion(request_id, path, line, character, trigger, &tx.completion)
                    .await
            }
            Cmd::ResolveCompletion {
                request_id,
                path,
                item,
            } => {
                state
                    .resolve_completion(request_id, path, item, &tx.completion)
                    .await
            }
            Cmd::RequestSignatureHelp {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_signature_help(request_id, path, line, character, &tx.signature_help)
                    .await
            }
            Cmd::RequestHover {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_hover(request_id, path, line, character, &tx.hover)
                    .await
            }
            Cmd::RustAnalyzer {
                request_id,
                path,
                request,
                line,
                character,
            } => {
                state
                    .request_rust_analyzer(
                        request_id,
                        path,
                        request,
                        (line, character),
                        &tx.rust_analyzer,
                    )
                    .await
            }
            Cmd::RequestDefinition {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_definition(request_id, path, line, character, &tx.definition)
                    .await
            }
            Cmd::RequestDocumentSymbols { path, seq } => {
                state
                    .request_document_symbols(path, seq, &tx.document_symbols)
                    .await
            }
            Cmd::RequestDeclaration {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_declaration(request_id, path, line, character, &tx.declaration)
                    .await
            }
            Cmd::RequestTypeDefinition {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_type_definition(request_id, path, line, character, &tx.type_definition)
                    .await
            }
            Cmd::RequestImplementation {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_implementation(request_id, path, line, character, &tx.implementation)
                    .await
            }
            Cmd::RequestWorkspaceSymbols { request_id, query } => {
                state
                    .request_workspace_symbols(request_id, query, &tx.workspace_symbols)
                    .await
            }
            Cmd::RequestReferences {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_references(request_id, path, line, character, &tx.references)
                    .await
            }
            Cmd::RequestDocumentHighlights {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_document_highlights(
                        request_id,
                        path,
                        line,
                        character,
                        &tx.document_highlights,
                    )
                    .await
            }
            Cmd::RequestLinkedEditing {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_linked_editing(request_id, path, line, character, &tx.linked_editing)
                    .await
            }
            Cmd::RequestInlineCompletion {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_inline_completion(
                        request_id,
                        path,
                        line,
                        character,
                        &tx.inline_completion,
                    )
                    .await
            }
            Cmd::RequestSelectionRanges {
                request_id,
                path,
                positions,
            } => {
                state
                    .request_selection_ranges(request_id, path, positions, &tx.selection_ranges)
                    .await
            }
            Cmd::RequestCallHierarchy {
                request_id,
                path,
                line,
                character,
                incoming,
            } => {
                state
                    .request_call_hierarchy(
                        request_id,
                        path,
                        line,
                        character,
                        incoming,
                        &tx.call_hierarchy,
                    )
                    .await
            }
            Cmd::RequestTypeHierarchy {
                request_id,
                path,
                line,
                character,
                supertypes,
            } => {
                state
                    .request_type_hierarchy(
                        request_id,
                        path,
                        line,
                        character,
                        supertypes,
                        &tx.call_hierarchy,
                    )
                    .await
            }
            Cmd::RequestRename {
                request_id,
                path,
                line,
                character,
                new_name,
            } => {
                state
                    .request_rename(request_id, path, line, character, new_name, &tx.rename)
                    .await
            }
            Cmd::RequestWillRenameFiles {
                request_id,
                renames,
            } => state.request_will_rename_files(request_id, renames, &tx.will_rename_files),
            Cmd::DidRenameFiles { renames } => state.did_rename_files(renames),
            Cmd::DidChangeWatchedFiles { changes } => state.did_change_watched_files(changes),
            Cmd::RequestPrepareRename {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_prepare_rename(request_id, path, line, character, &tx.prepare_rename)
                    .await
            }
            Cmd::RequestFormatting {
                request_id,
                path,
                tab_size,
                insert_spaces,
            } => {
                state
                    .request_formatting(request_id, path, tab_size, insert_spaces, &tx.formatting)
                    .await
            }
            Cmd::RequestOnTypeFormatting {
                request_id,
                path,
                line,
                character,
                ch,
                tab_size,
                insert_spaces,
            } => {
                state
                    .request_on_type_formatting(
                        request_id,
                        path,
                        line,
                        character,
                        ch,
                        tab_size,
                        insert_spaces,
                        &tx.on_type_formatting,
                    )
                    .await
            }
            Cmd::RequestRangeFormatting {
                request_id,
                path,
                start,
                end,
                tab_size,
                insert_spaces,
            } => {
                state
                    .request_range_formatting(
                        request_id,
                        path,
                        start,
                        end,
                        tab_size,
                        insert_spaces,
                        &tx.formatting,
                    )
                    .await
            }
            Cmd::RequestCodeAction {
                request_id,
                path,
                start_line,
                start_char,
                end_line,
                end_char,
                diagnostics,
            } => {
                state
                    .request_code_action(
                        request_id,
                        path,
                        start_line,
                        start_char,
                        end_line,
                        end_char,
                        diagnostics,
                        &tx.code_action,
                    )
                    .await
            }
            Cmd::ResolveCodeAction {
                request_id,
                path,
                server,
                action,
            } => {
                state
                    .resolve_code_action(request_id, path, server, action, &tx.code_action)
                    .await
            }
            Cmd::ExecuteCommand {
                path,
                command,
                arguments,
            } => state.execute_command(path, command, arguments).await,
        }
    }
}
