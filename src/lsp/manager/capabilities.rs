//! What croft and a server tell each other they can do: the client capabilities sent
//! at initialize, the standard semantic token types and modifiers croft declares, and
//! what each server's reply says it supports.

use super::*;

/// The standard LSP semantic token types croft can render. Declared so
/// servers know which types we understand; they reply with a legend that
/// is a subset of this set.
pub(super) fn standard_semantic_token_types() -> Vec<SemanticTokenType> {
    vec![
        SemanticTokenType::NAMESPACE,
        SemanticTokenType::TYPE,
        SemanticTokenType::CLASS,
        SemanticTokenType::ENUM,
        SemanticTokenType::INTERFACE,
        SemanticTokenType::STRUCT,
        SemanticTokenType::TYPE_PARAMETER,
        SemanticTokenType::PARAMETER,
        SemanticTokenType::VARIABLE,
        SemanticTokenType::PROPERTY,
        SemanticTokenType::ENUM_MEMBER,
        SemanticTokenType::EVENT,
        SemanticTokenType::FUNCTION,
        SemanticTokenType::METHOD,
        SemanticTokenType::MACRO,
        SemanticTokenType::KEYWORD,
        SemanticTokenType::MODIFIER,
        SemanticTokenType::COMMENT,
        SemanticTokenType::STRING,
        SemanticTokenType::NUMBER,
        SemanticTokenType::REGEXP,
        SemanticTokenType::OPERATOR,
        SemanticTokenType::DECORATOR,
    ]
}

pub(super) fn standard_semantic_token_modifiers() -> Vec<SemanticTokenModifier> {
    vec![
        SemanticTokenModifier::DECLARATION,
        SemanticTokenModifier::DEFINITION,
        SemanticTokenModifier::READONLY,
        SemanticTokenModifier::STATIC,
        SemanticTokenModifier::DEPRECATED,
        SemanticTokenModifier::ABSTRACT,
        SemanticTokenModifier::ASYNC,
        SemanticTokenModifier::MODIFICATION,
        SemanticTokenModifier::DOCUMENTATION,
        SemanticTokenModifier::DEFAULT_LIBRARY,
    ]
}

pub(super) fn build_client_capabilities() -> ClientCapabilities {
    // `linkSupport` on every goto: without it a server flattens each
    // `LocationLink` into a `Location` over the whole declaration, and the
    // jump lands on its first character instead of the name, lines above it
    // in a multi-line declaration (#1284). `def_location` reads a link's
    // `targetSelectionRange`.
    let goto = || {
        Some(lsp_types::GotoCapability {
            dynamic_registration: Some(false),
            link_support: Some(true),
        })
    };
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            definition: goto(),
            declaration: goto(),
            type_definition: goto(),
            implementation: goto(),

            // croft sends didSave, and only to a server that advertised
            // `save` (#854). No dynamic registration: croft accepts
            // `client/registerCapability` for watched files alone (#908), and
            // rust-analyzer registers didSave dynamically when told it may,
            // instead of advertising `save`.
            synchronization: Some(lsp_types::TextDocumentSyncClientCapabilities {
                dynamic_registration: Some(false),
                will_save: Some(false),
                will_save_wait_until: Some(false),
                did_save: Some(true),
            }),
            completion: Some(CompletionClientCapabilities {
                dynamic_registration: Some(false),
                context_support: Some(true),
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(false),
                    commit_characters_support: Some(false),
                    documentation_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
                    deprecated_support: Some(true),
                    preselect_support: Some(true),
                    insert_replace_support: Some(false),
                    label_details_support: Some(true),
                    // Auto-imports may come from resolve (#928). Not docs:
                    // rust-analyzer would then leave them out of the list,
                    // which the details pane shows.
                    resolve_support: Some(lsp_types::CompletionItemCapabilityResolveSupport {
                        properties: vec![String::from("additionalTextEdits")],
                    }),
                    ..Default::default()
                }),
                completion_item_kind: Some(CompletionItemKindCapability {
                    value_set: Some(vec![
                        CompletionItemKind::TEXT,
                        CompletionItemKind::METHOD,
                        CompletionItemKind::FUNCTION,
                        CompletionItemKind::CONSTRUCTOR,
                        CompletionItemKind::FIELD,
                        CompletionItemKind::VARIABLE,
                        CompletionItemKind::CLASS,
                        CompletionItemKind::INTERFACE,
                        CompletionItemKind::MODULE,
                        CompletionItemKind::PROPERTY,
                        CompletionItemKind::UNIT,
                        CompletionItemKind::VALUE,
                        CompletionItemKind::ENUM,
                        CompletionItemKind::KEYWORD,
                        CompletionItemKind::SNIPPET,
                        CompletionItemKind::COLOR,
                        CompletionItemKind::FILE,
                        CompletionItemKind::REFERENCE,
                        CompletionItemKind::FOLDER,
                        CompletionItemKind::ENUM_MEMBER,
                        CompletionItemKind::CONSTANT,
                        CompletionItemKind::STRUCT,
                        CompletionItemKind::EVENT,
                        CompletionItemKind::OPERATOR,
                        CompletionItemKind::TYPE_PARAMETER,
                    ]),
                }),
                ..Default::default()
            }),
            signature_help: Some(lsp_types::SignatureHelpClientCapabilities {
                dynamic_registration: Some(false),
                signature_information: Some(lsp_types::SignatureInformationSettings {
                    documentation_format: Some(vec![MarkupKind::PlainText, MarkupKind::Markdown]),
                    parameter_information: Some(lsp_types::ParameterInformationSettings {
                        label_offset_support: Some(true),
                    }),
                    active_parameter_support: Some(true),
                }),
                context_support: Some(true),
            }),
            semantic_tokens: Some(SemanticTokensClientCapabilities {
                dynamic_registration: Some(false),
                requests: SemanticTokensClientCapabilitiesRequests {
                    // ty answers a viewport `semanticTokens/range` query in tens
                    // of ms even on a cold workspace, so croft fires one on open
                    // for instant first paint of on-screen code.
                    range: Some(true),
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                },
                token_types: standard_semantic_token_types(),
                token_modifiers: standard_semantic_token_modifiers(),
                formats: vec![TokenFormat::RELATIVE],
                overlapping_token_support: Some(false),
                multiline_token_support: Some(false),
                // Tell the server its tokens layer over croft's tree-sitter
                // highlighting (VS Code / Zed "combined" model), so it may
                // omit tokens that already match syntax.
                augments_syntax_tokens: Some(true),
                ..Default::default()
            }),
            // Advertise hierarchical Outline support so servers return the
            // nested `DocumentSymbol` tree (rust-analyzer falls back to the
            // flat `SymbolInformation` list without this), exactly as VS Code
            // does. `flatten_symbols` handles both shapes regardless.
            document_symbol: Some(DocumentSymbolClientCapabilities {
                dynamic_registration: Some(false),
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            // Advertise inlay-hint support so servers that gate the provider
            // on it (vtsls) publish `inlayHintProvider` and answer
            // `textDocument/inlayHint`.
            inlay_hint: Some(lsp_types::InlayHintClientCapabilities {
                dynamic_registration: Some(false),
                resolve_support: None,
            }),
            // Advertise call-hierarchy and documentHighlight support so
            // servers that gate their providers on the client declaring
            // them (vtsls does for several) publish the capability.
            call_hierarchy: Some(lsp_types::CallHierarchyClientCapabilities {
                dynamic_registration: Some(false),
            }),
            type_hierarchy: Some(lsp_types::TypeHierarchyClientCapabilities {
                dynamic_registration: Some(false),
            }),
            code_lens: Some(lsp_types::CodeLensClientCapabilities {
                dynamic_registration: Some(false),
            }),
            document_highlight: Some(lsp_types::DocumentHighlightClientCapabilities {
                dynamic_registration: Some(false),
            }),
            linked_editing_range: Some(lsp_types::LinkedEditingRangeClientCapabilities {
                dynamic_registration: Some(false),
            }),
            // LSP 3.18, still "proposed" in lsp-types (#607): a server that
            // offers ghost-text suggestions (Copilot's) answers for its files.
            inline_completion: Some(lsp_types::InlineCompletionClientCapabilities {
                dynamic_registration: Some(false),
            }),
            selection_range: Some(lsp_types::SelectionRangeClientCapabilities {
                dynamic_registration: Some(false),
            }),
            // Advertise prepare-rename support (#254 item 6). Per LSP 3.12 a
            // server publishes `RenameOptions.prepareProvider` only in answer
            // to this; without it most servers send a bare
            // `renameProvider: true`, which `prepare_rename_supported`
            // rejects — the validation would silently never engage.
            // `prepare_support_default_behavior` is what lets a server return
            // the `{ defaultBehavior: true }` shape at all.
            rename: Some(lsp_types::RenameClientCapabilities {
                dynamic_registration: Some(false),
                prepare_support: Some(true),
                prepare_support_default_behavior: Some(
                    lsp_types::PrepareSupportDefaultBehavior::IDENTIFIER,
                ),
                ..Default::default()
            }),
            // Declare push-diagnostics support. Several servers gate
            // `textDocument/publishDiagnostics` on the client advertising this
            // (ty and ruff push regardless, but vtsls stays silent without it);
            // declaring it is what every real LSP client (VS Code, Neovim) does.
            // `tagSupport` (#1522): servers that check it before tagging
            // unused or deprecated code would otherwise leave the tag out.
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                related_information: Some(true),
                // Servers may leave the rule's link out otherwise (#1540).
                code_description_support: Some(true),
                tag_support: Some(lsp_types::TagSupport {
                    value_set: vec![
                        lsp_types::DiagnosticTag::UNNECESSARY,
                        lsp_types::DiagnosticTag::DEPRECATED,
                    ],
                }),
                ..Default::default()
            }),
            // Advertise rich code-action support. Without codeActionLiteralSupport
            // a server (ruff, vtsls) must answer `textDocument/codeAction` with
            // plain `Command[]` and withholds kinded source actions, so "Organize
            // Imports" / "Fix all" never surface. dataSupport + resolveSupport for
            // `edit` let servers defer the (expensive) edit to `codeAction/resolve`
            // — vtsls auto-imports and ruff fixes rely on it. This is exactly what
            // VS Code and Neovim declare.
            code_action: Some(CodeActionClientCapabilities {
                dynamic_registration: Some(false),
                code_action_literal_support: Some(CodeActionLiteralSupport {
                    code_action_kind: CodeActionKindLiteralSupport {
                        value_set: vec![
                            String::new(),
                            "quickfix".to_string(),
                            "refactor".to_string(),
                            "refactor.extract".to_string(),
                            "refactor.inline".to_string(),
                            "refactor.rewrite".to_string(),
                            "source".to_string(),
                            "source.organizeImports".to_string(),
                            "source.fixAll".to_string(),
                        ],
                    },
                }),
                is_preferred_support: Some(true),
                data_support: Some(true),
                resolve_support: Some(CodeActionCapabilityResolveSupport {
                    properties: vec!["edit".to_string()],
                }),
                ..Default::default()
            }),
            // LSP 3.17 PULL diagnostics (#533). A conforming server publishes
            // `diagnosticProvider` only when the client declares this, so
            // without it `workspace_diagnostics_supported` reads `None` on
            // every server and the pull path can never engage - a gap that
            // would read as "no server supports it" rather than "croft never
            // asked". `relatedDocumentSupport` stays false: croft pulls per
            // WORKSPACE, so it has no use for a per-document report that
            // carries other files' diagnostics alongside.
            diagnostic: Some(lsp_types::DiagnosticClientCapabilities {
                dynamic_registration: Some(false),
                related_document_support: Some(false),
            }),
            ..Default::default()
        }),
        // Advertise that croft can re-pull tokens on a server's request. ty
        // does not currently send `workspace/semanticTokens/refresh` (croft
        // covers the cold-start gap with its own empty-response retry instead),
        // but declaring support is correct and future-proofs the path for
        // servers that do.
        workspace: Some(WorkspaceClientCapabilities {
            // Same re-pull contract for hints: rust-analyzer sends
            // `workspace/inlayHint/refresh` after a config change or when its
            // analysis upgrades, and croft re-requests for the visible editors.
            inlay_hint: Some(lsp_types::InlayHintWorkspaceClientCapabilities {
                refresh_support: Some(true),
            }),
            semantic_tokens: Some(SemanticTokensWorkspaceClientCapabilities {
                refresh_support: Some(true),
            }),
            // Without this declaration a server may legally IGNORE the
            // `workspace_folders` array in initialize (LSP 3.6): the
            // capability is what tells folder-aware servers (rust-analyzer,
            // gopls, ruff, ty) the client can answer
            // `workspace/workspaceFolders` and will send
            // `didChangeWorkspaceFolders` when the set changes.
            workspace_folders: Some(true),
            // The other half of the pull contract (#533): a server sends
            // `workspace/diagnostic/refresh` only when the client says it can
            // handle one. Without it a project-wide change (a config reload, a
            // dependency resolving) never reaches the pulled set, which then
            // ages silently until something else forces a re-pull.
            diagnostic: Some(lsp_types::DiagnosticWorkspaceClientCapabilities {
                refresh_support: Some(true),
            }),
            // Explorer renames and moves ask the server for its edit first
            // (#610), so imports follow the file.
            file_operations: Some(lsp_types::WorkspaceFileOperationsClientCapabilities {
                will_rename: Some(true),
                did_rename: Some(true),
                ..Default::default()
            }),
            // Servers that leave file watching to the client (ty, gopls,
            // rust-analyzer) register the globs they want, and croft
            // forwards its watcher's events (#908). Undeclared, a file
            // changed on disk outside a tab stayed stale in the server.
            did_change_watched_files: Some(lsp_types::DidChangeWatchedFilesClientCapabilities {
                dynamic_registration: Some(true),
                relative_pattern_support: Some(true),
            }),
            ..Default::default()
        }),
        // Declare `window.workDoneProgress` so servers stream `$/progress`
        // (rust-analyzer's "Indexing…", "Building CrateGraph", "Roots
        // Scanned"). The router forwards it to the status bar, so a server
        // that is busy priming is never silently mistaken for a dead one.
        // `showMessage` and `showDocument` (#1599): croft answers a server's
        // questions from a picker and opens the documents and links it
        // points at, so gopls sends "Browse documentation" as a link to
        // open rather than a line in OUTPUT.
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            show_message: Some(lsp_types::ShowMessageRequestClientCapabilities {
                message_action_item: Some(lsp_types::MessageActionItemCapabilities {
                    additional_properties_support: Some(true),
                }),
            }),
            show_document: Some(lsp_types::ShowDocumentClientCapabilities { support: true }),
        }),
        // rust-analyzer emits its Run / Debug / references lenses only for
        // client-side commands the client says it can run (#608), and these
        // are exactly the ones `crate::code_lens` maps.
        experimental: Some(serde_json::json!({
            "commands": {
                "commands": [
                    "rust-analyzer.runSingle",
                    "rust-analyzer.debugSingle",
                    "rust-analyzer.showReferences"
                ]
            }
        })),
        ..Default::default()
    }
}

/// What one server said it can do, read off its initialize reply as it
/// starts. The fields are named, and mean, as on [`ManagedClient`], which
/// keeps them for the server's life.
pub(super) struct ServerSupport {
    pub(super) supports_completion: bool,
    pub(super) completion_triggers: String,
    pub(super) supports_signature_help: bool,
    pub(super) supports_workspace_diagnostics: bool,
    pub(super) diagnostic_identifier: Option<String>,
    pub(super) save_notify: SaveNotify,
    pub(super) supports_document_diagnostics: bool,
    pub(super) supports_hover: bool,
    pub(super) supports_definition: bool,
    pub(super) supports_document_symbol: bool,
    pub(super) supports_declaration: bool,
    pub(super) supports_type_definition: bool,
    pub(super) supports_implementation: bool,
    pub(super) supports_references: bool,
    pub(super) supports_document_highlight: bool,
    pub(super) supports_linked_editing: bool,
    pub(super) supports_inline_completion: bool,
    pub(super) supports_selection_range: bool,
    pub(super) supports_call_hierarchy: bool,
    pub(super) supports_type_hierarchy: bool,
    pub(super) supports_workspace_symbols: bool,
    pub(super) supports_rename: bool,
    pub(super) supports_prepare_rename: bool,
    pub(super) supports_formatting: bool,
    pub(super) on_type_triggers: Option<String>,
    pub(super) supports_range_formatting: bool,
    pub(super) supports_code_action: bool,
    pub(super) semantic_legend: Option<Arc<Vec<String>>>,
    pub(super) semantic_supports_range: bool,
    pub(super) supports_inlay_hints: bool,
    pub(super) supports_document_link: bool,
    pub(super) supports_code_lens: bool,
    pub(super) code_lens_resolve: bool,
    pub(super) server_commands: Arc<Vec<String>>,
    pub(super) supports_folding_range: bool,
    pub(super) supports_document_color: bool,
    pub(super) will_rename: Option<Arc<crate::lsp::file_ops::FileOpFilters>>,
    pub(super) did_rename: Option<Arc<crate::lsp::file_ops::FileOpFilters>>,
}

/// Read [`ServerSupport`] off a server's capabilities. `type_hierarchy` comes
/// from the raw initialize reply instead, because [`ServerCapabilities`]
/// cannot carry `typeHierarchyProvider` in `lsp-types` 0.95 (#613).
pub(super) fn server_support(caps: &ServerCapabilities, type_hierarchy: bool) -> ServerSupport {
    // A capability that can be sent as a bare `false` (the bool-or-options
    // shapes) must be read as its inner bool: `Option::is_some` treats
    // `Some(false)` as supported, which made croft call
    // `textDocument/declaration` on vtsls (which advertises
    // `declarationProvider: false`) and get a -32601 "Unhandled method" back.
    let supports_completion = caps.completion_provider.is_some();
    let completion_triggers = completion_triggers_of(caps);
    let supports_signature_help = signature_help_supported(&caps.signature_help_provider);
    let supports_workspace_diagnostics = workspace_diagnostics_supported(&caps.diagnostic_provider);
    let supports_document_diagnostics = caps.diagnostic_provider.is_some();
    let diagnostic_identifier = diagnostic_identifier_of(&caps.diagnostic_provider);
    let save_notify = save_notify_of(&caps.text_document_sync);
    let supports_hover = hover_supported(&caps.hover_provider);
    let supports_definition = one_of_supported(&caps.definition_provider);
    let supports_document_symbol = one_of_supported(&caps.document_symbol_provider);
    let supports_declaration = declaration_supported(&caps.declaration_provider);
    let supports_type_definition = type_definition_supported(&caps.type_definition_provider);
    let supports_implementation = implementation_supported(&caps.implementation_provider);
    let supports_references = one_of_supported(&caps.references_provider);
    let supports_document_highlight = one_of_supported(&caps.document_highlight_provider);
    let supports_linked_editing = linked_editing_supported(&caps.linked_editing_range_provider);
    let supports_inline_completion = one_of_supported(&caps.inline_completion_provider);
    let supports_selection_range = selection_range_supported(&caps.selection_range_provider);
    let supports_call_hierarchy = call_hierarchy_supported(&caps.call_hierarchy_provider);
    let supports_type_hierarchy = type_hierarchy;
    let supports_workspace_symbols = one_of_supported(&caps.workspace_symbol_provider);
    let supports_rename = one_of_supported(&caps.rename_provider);
    let supports_prepare_rename = prepare_rename_supported(&caps.rename_provider);
    let supports_formatting = one_of_supported(&caps.document_formatting_provider);
    let on_type_triggers = caps.document_on_type_formatting_provider.as_ref().map(|o| {
        let mut t = o.first_trigger_character.clone();
        for more in o.more_trigger_character.iter().flatten() {
            t.push_str(more);
        }
        t
    });
    let supports_range_formatting = one_of_supported(&caps.document_range_formatting_provider);
    let supports_code_action = code_action_supported(&caps.code_action_provider);
    let semantic_legend = semantic_legend_of(caps).map(Arc::new);
    let semantic_supports_range = semantic_tokens_range_supported(caps);
    let supports_inlay_hints = one_of_supported(&caps.inlay_hint_provider);
    let supports_document_link = caps.document_link_provider.is_some();
    let supports_code_lens = caps.code_lens_provider.is_some();
    let code_lens_resolve = caps
        .code_lens_provider
        .as_ref()
        .and_then(|o| o.resolve_provider)
        .unwrap_or(false);
    let server_commands = Arc::new(
        caps.execute_command_provider
            .as_ref()
            .map(|o| o.commands.clone())
            .unwrap_or_default(),
    );
    let supports_folding_range = folding_range_supported(&caps.folding_range_provider);
    let supports_document_color = color_provider_supported(&caps.color_provider);
    let file_ops = caps
        .workspace
        .as_ref()
        .and_then(|w| w.file_operations.as_ref());
    let compile = |reg: Option<&lsp_types::FileOperationRegistrationOptions>| {
        reg.map(|r| Arc::new(crate::lsp::file_ops::FileOpFilters::from_registration(r)))
    };
    let will_rename = compile(file_ops.and_then(|f| f.will_rename.as_ref()));
    let did_rename = compile(file_ops.and_then(|f| f.did_rename.as_ref()));
    ServerSupport {
        supports_completion,
        completion_triggers,
        supports_signature_help,
        supports_workspace_diagnostics,
        diagnostic_identifier,
        save_notify,
        supports_document_diagnostics,
        supports_hover,
        supports_definition,
        supports_document_symbol,
        supports_declaration,
        supports_type_definition,
        supports_implementation,
        supports_references,
        supports_document_highlight,
        supports_linked_editing,
        supports_inline_completion,
        supports_selection_range,
        supports_call_hierarchy,
        supports_type_hierarchy,
        supports_workspace_symbols,
        supports_rename,
        supports_prepare_rename,
        supports_formatting,
        on_type_triggers,
        supports_range_formatting,
        supports_code_action,
        semantic_legend,
        semantic_supports_range,
        supports_inlay_hints,
        supports_document_link,
        supports_code_lens,
        code_lens_resolve,
        server_commands,
        supports_folding_range,
        supports_document_color,
        will_rename,
        did_rename,
    }
}
