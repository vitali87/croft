use std::ops::ControlFlow;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;

use anyhow::{Context, Result, anyhow};
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::tracing::TracingLayer;
use async_lsp::{LanguageServer, MainLoop, ServerSocket};
use lsp_types::DiagnosticSeverity as LspDiagnosticSeverity;
use lsp_types::notification::{LogMessage, Progress, PublishDiagnostics, ShowMessage};
use lsp_types::request::{SemanticTokensRefresh, WorkDoneProgressCreate};
use lsp_types::{
    ClientCapabilities, CodeAction, CodeActionContext, CodeActionParams, CodeActionResponse,
    CompletionContext, CompletionParams, CompletionResponse, CompletionTriggerKind,
    Diagnostic as LspDiagnostic, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentFormattingParams,
    DocumentSymbolParams, DocumentSymbolResponse, ExecuteCommandParams, FormattingOptions,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, InitializeParams,
    InitializedParams, Location, PartialResultParams, Position, ProgressParamsValue, Range,
    ReferenceContext, ReferenceParams, RenameParams, SemanticTokensParams,
    SemanticTokensRangeParams, SemanticTokensRangeResult, SemanticTokensResult, ServerCapabilities,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, TextEdit, Url, VersionedTextDocumentIdentifier, WorkDoneProgress,
    WorkDoneProgressParams, WorkspaceEdit, WorkspaceFolder,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tower::ServiceBuilder;

use crate::lsp::config::ServerConfig;
use crate::lsp::log_file;
use crate::lsp::manager::{Diagnostic, DiagnosticSeverity, DiagnosticsUpdate, ProgressUpdate};
use crate::output::{self, OutputLevel};

/// Map an LSP `window/{show,log}Message` severity to an OUTPUT channel level so
/// the panel colours a server's own messages the way it classified them.
fn msg_level(typ: lsp_types::MessageType) -> OutputLevel {
    use lsp_types::MessageType as MT;
    if typ == MT::ERROR {
        OutputLevel::Error
    } else if typ == MT::WARNING {
        OutputLevel::Warn
    } else if typ == MT::INFO {
        OutputLevel::Info
    } else {
        OutputLevel::Debug
    }
}

/// Classify a language-server stderr line by its tracing level token, so
/// rust-analyzer's WARN/ERROR lines (e.g. "overly long loop turn" during
/// cache priming) reach the OUTPUT panel with their real severity instead of
/// a blanket Info. The token sits near the start of a tracing-formatted line
/// (after an optional timestamp / opening bracket), so only the first few
/// words are inspected; anything unrecognised stays Info.
fn stderr_level(line: &str) -> OutputLevel {
    for word in line.split_whitespace().take(3) {
        match word.trim_matches(|c: char| !c.is_ascii_alphabetic()) {
            "ERROR" => return OutputLevel::Error,
            "WARN" | "WARNING" => return OutputLevel::Warn,
            "INFO" => return OutputLevel::Info,
            "DEBUG" | "TRACE" => return OutputLevel::Debug,
            _ => {}
        }
    }
    OutputLevel::Info
}

struct ClientState {
    name: String,
    // Set when the server sends `workspace/semanticTokens/refresh`. The app
    // polls and clears it, then re-pulls tokens for the visible editor(s).
    // rust-analyzer relies on this: it answers the first request from partial
    // syntax analysis, then sends refresh once the cargo/crate-graph analysis
    // resolves the richer type-aware tokens. (ty/basedpyright never send it.)
    semantic_refresh: Arc<AtomicBool>,
    // Same contract for `workspace/inlayHint/refresh`: the app polls and
    // clears it, then re-requests hints for the visible editor(s).
    inlay_refresh: Arc<AtomicBool>,
    // Server-pushed `textDocument/publishDiagnostics` batches are normalised
    // and forwarded here; the manager owns the receiver and the app drains it.
    diagnostics_tx: std_mpsc::Sender<DiagnosticsUpdate>,
    // Server `$/progress` work-done updates (rust-analyzer indexing, etc.)
    // forwarded here for the status bar.
    progress_tx: std_mpsc::Sender<ProgressUpdate>,
    // Begin carries the progress title; Report/End don't, so the title is
    // remembered per progress token to keep the status line meaningful.
    progress_titles: std::collections::HashMap<String, String>,
    // The folder list this client answers `workspace/workspaceFolders`
    // with — the same list initialize carried. A server that saw the
    // workspaceFolders capability may re-query it at any time (LSP 3.6).
    workspace_folders: Vec<WorkspaceFolder>,
}

/// Join a work-done progress title with its optional message and percentage
/// into one compact status string (e.g. `Indexing 112/340 33%`).
pub(crate) fn compose_progress(
    title: &str,
    message: Option<&str>,
    percentage: Option<u32>,
) -> String {
    let mut out = title.to_string();
    if let Some(m) = message.filter(|m| !m.is_empty()) {
        out.push(' ');
        out.push_str(m);
    }
    if let Some(p) = percentage {
        out.push_str(&format!(" {p}%"));
    }
    out
}

fn progress_token_key(token: &lsp_types::ProgressToken) -> String {
    match token {
        lsp_types::NumberOrString::Number(n) => n.to_string(),
        lsp_types::NumberOrString::String(s) => s.clone(),
    }
}

impl ClientState {
    fn router(
        name: String,
        semantic_refresh: Arc<AtomicBool>,
        inlay_refresh: Arc<AtomicBool>,
        diagnostics_tx: std_mpsc::Sender<DiagnosticsUpdate>,
        progress_tx: std_mpsc::Sender<ProgressUpdate>,
        workspace_folders: Vec<WorkspaceFolder>,
    ) -> Router<Self> {
        let mut router = Router::new(ClientState {
            name,
            semantic_refresh,
            inlay_refresh,
            diagnostics_tx,
            progress_tx,
            progress_titles: std::collections::HashMap::new(),
            workspace_folders,
        });
        router
            .request::<SemanticTokensRefresh, _>(|this, _params| {
                this.semantic_refresh.store(true, Ordering::Relaxed);
                log_file::log(&format!(
                    "lsp[{}] semanticTokens/refresh -> re-pull queued",
                    this.name
                ));
                std::future::ready(Ok(()))
            })
            // `workspace/inlayHint/refresh`: the server's hints changed for
            // reasons other than an edit (config reload, cross-file analysis);
            // acknowledge and queue a re-request, like the semantic flag above.
            .request::<lsp_types::request::InlayHintRefreshRequest, _>(|this, _params| {
                this.inlay_refresh.store(true, Ordering::Relaxed);
                log_file::log(&format!(
                    "lsp[{}] inlayHint/refresh -> re-request queued",
                    this.name
                ));
                std::future::ready(Ok(()))
            })
            // Servers (rust-analyzer) ask us to create a progress token before
            // streaming `$/progress`. Acknowledge it so the progress flows;
            // declining with METHOD_NOT_FOUND would suppress the indexing UI.
            .request::<WorkDoneProgressCreate, _>(|_this, _params| std::future::ready(Ok(())))
            // LSP 3.6 `workspace/workspaceFolders`: hand back the same
            // folder list initialize carried. Declining this (the old
            // unhandled_request fallback) told folder-aware servers the
            // list was unavailable and legally degraded them to
            // single-root behavior.
            .request::<lsp_types::request::WorkspaceFoldersRequest, _>(|this, _params| {
                std::future::ready(Ok(Some(this.workspace_folders.clone())))
            })
            .notification::<Progress>(|this, params| {
                let ProgressParamsValue::WorkDone(work) = params.value;
                let key = progress_token_key(&params.token);
                let message = match work {
                    WorkDoneProgress::Begin(b) => {
                        this.progress_titles.insert(key.clone(), b.title.clone());
                        Some(compose_progress(
                            &b.title,
                            b.message.as_deref(),
                            b.percentage,
                        ))
                    }
                    WorkDoneProgress::Report(r) => {
                        let title = this.progress_titles.get(&key).cloned().unwrap_or_default();
                        Some(compose_progress(&title, r.message.as_deref(), r.percentage))
                    }
                    WorkDoneProgress::End(_) => {
                        this.progress_titles.remove(&key);
                        None
                    }
                };
                let _ = this.progress_tx.send(ProgressUpdate {
                    server: this.name.clone(),
                    message,
                });
                ControlFlow::Continue(())
            })
            .notification::<PublishDiagnostics>(|this, params| {
                log_file::log(&format!(
                    "lsp[{}] diagnostics for {}: {} item(s)",
                    this.name,
                    params.uri,
                    params.diagnostics.len()
                ));
                // Forward the normalised batch to the app. A non-file URI
                // (e.g. an untitled buffer) has no editor to paint, so skip it.
                if let Ok(path) = params.uri.to_file_path() {
                    let diagnostics = params.diagnostics.iter().map(convert_diagnostic).collect();
                    let _ = this.diagnostics_tx.send(DiagnosticsUpdate {
                        path,
                        server: this.name.clone(),
                        diagnostics,
                    });
                }
                ControlFlow::Continue(())
            })
            .notification::<ShowMessage>(|this, params| {
                log_file::log(&format!(
                    "lsp[{}] {:?}: {}",
                    this.name, params.typ, params.message
                ));
                output::push(&this.name, msg_level(params.typ), &params.message);
                ControlFlow::Continue(())
            })
            .notification::<LogMessage>(|this, params| {
                log_file::log(&format!(
                    "lsp[{}] log {:?}: {}",
                    this.name, params.typ, params.message
                ));
                output::push(&this.name, msg_level(params.typ), &params.message);
                ControlFlow::Continue(())
            });
        router.unhandled_notification(|this, notif| {
            log_file::log(&format!(
                "lsp[{}] ignored notification {}",
                this.name, notif.method
            ));
            ControlFlow::Continue(())
        });
        router.unhandled_request(|this, req| {
            let method = req.method.clone();
            let name = this.name.clone();
            Box::pin(async move {
                log_file::log(&format!(
                    "lsp[{name}] declined unsupported request {method}"
                ));
                Err(async_lsp::ResponseError::new(
                    async_lsp::ErrorCode::METHOD_NOT_FOUND,
                    format!("No such method {method}"),
                ))
            })
        });
        router
    }
}

/// Normalise one LSP wire diagnostic into croft's `Diagnostic`. Severity
/// defaults to `Error` when the server omits it (the LSP spec says the client
/// may pick, and an unclassified problem is most safely shown as an error).
fn convert_diagnostic(d: &lsp_types::Diagnostic) -> Diagnostic {
    let severity = match d.severity {
        Some(LspDiagnosticSeverity::WARNING) => DiagnosticSeverity::Warning,
        Some(LspDiagnosticSeverity::INFORMATION) => DiagnosticSeverity::Information,
        Some(LspDiagnosticSeverity::HINT) => DiagnosticSeverity::Hint,
        _ => DiagnosticSeverity::Error,
    };
    Diagnostic {
        start_line: d.range.start.line,
        start_char: d.range.start.character,
        end_line: d.range.end.line,
        end_char: d.range.end.character,
        severity,
        message: d.message.clone(),
    }
}

pub struct LspClient {
    server: ServerSocket,
    capabilities: ServerCapabilities,
    name: String,
    // Holds the process so kill_on_drop only fires when this client is
    // dropped, not when spawn() returns. Without this the server is
    // SIGKILLed mid-handshake and the mainloop reader sees EOF.
    child: Child,
    // The async-lsp MainLoop driving this client. Aborted on drop, before
    // `server` (its ServerSocket sender) is dropped, so the loop never polls
    // a closed event channel and panics with "Sender is alive" (lib.rs:553).
    mainloop_task: tokio::task::JoinHandle<()>,
}

impl LspClient {
    // Spawn wiring: each channel/flag is an independent router input;
    // bundling them into a struct would add indirection without clarity.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        config: &ServerConfig,
        workspace_root: &Path,
        client_capabilities: ClientCapabilities,
        extra_path: &[std::path::PathBuf],
        semantic_refresh: Arc<AtomicBool>,
        inlay_refresh: Arc<AtomicBool>,
        diagnostics_tx: std_mpsc::Sender<DiagnosticsUpdate>,
        progress_tx: std_mpsc::Sender<ProgressUpdate>,
    ) -> Result<Self> {
        let workspace_uri = Url::from_file_path(workspace_root).map_err(|_| {
            anyhow!(
                "workspace root must be absolute: {}",
                workspace_root.display()
            )
        })?;
        let workspace_name = workspace_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "root".to_string());
        // One folder list serves both initialize and the router's
        // `workspace/workspaceFolders` responder, so the two can never
        // disagree about what the client claimed.
        let workspace_folders = vec![WorkspaceFolder {
            uri: workspace_uri,
            name: workspace_name,
        }];
        let router_folders = workspace_folders.clone();

        let mut command = Command::new(&config.command);
        command.args(&config.args).current_dir(workspace_root);
        // Version-manager node dirs aren't on croft's inherited PATH; prepend
        // them so a server like vtsls can find `node` for its shebang.
        if !extra_path.is_empty() {
            command.env("PATH", crate::lsp::install::prepend_paths(extra_path));
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn lsp server `{}`", config.command))?;

        let stdout = child
            .stdout
            .take()
            .context("server stdout missing")?
            .compat();
        let stdin = child
            .stdin
            .take()
            .context("server stdin missing")?
            .compat_write();
        let stderr = child.stderr.take().context("server stderr missing")?;

        let name = config.name.to_string();
        let router_name = name.clone();
        let (mainloop, mut server) = MainLoop::new_client(move |_server_socket| {
            ServiceBuilder::new()
                .layer(TracingLayer::default())
                .layer(CatchUnwindLayer::default())
                .layer(ConcurrencyLayer::default())
                .service(ClientState::router(
                    router_name,
                    semantic_refresh,
                    inlay_refresh,
                    diagnostics_tx,
                    progress_tx,
                    router_folders,
                ))
        });

        let stderr_name = name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log_file::log(&format!("lsp[{stderr_name}] stderr: {line}"));
                output::push(&stderr_name, stderr_level(&line), &line);
            }
        });

        // Tee both directions of the JSON-RPC stream into the server's
        // "<name> (trace)" OUTPUT channel. The tee is inert (one atomic load per
        // chunk) until the OUTPUT panel's RPC toggle turns tracing on.
        let stdout = crate::lsp::trace::TraceRead::new(stdout, name.clone());
        let stdin = crate::lsp::trace::TraceWrite::new(stdin, name.clone());

        let mainloop_name = name.clone();
        let mainloop_task = tokio::spawn(async move {
            if let Err(e) = mainloop.run_buffered(stdout, stdin).await {
                log_file::log(&format!("lsp[{mainloop_name}] mainloop exited: {e}"));
                output::push(
                    &mainloop_name,
                    OutputLevel::Error,
                    &format!("language server exited: {e}"),
                );
            }
        });

        let init = server
            .initialize(InitializeParams {
                process_id: Some(std::process::id()),
                capabilities: client_capabilities,
                initialization_options: config.initialization_options.clone(),
                workspace_folders: Some(workspace_folders),
                ..InitializeParams::default()
            })
            .await
            .context("lsp initialize")?;
        server
            .initialized(InitializedParams {})
            .context("lsp initialized notification")?;

        Ok(Self {
            server,
            capabilities: init.capabilities,
            name,
            child,
            mainloop_task,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn capabilities(&self) -> &ServerCapabilities {
        &self.capabilities
    }

    pub fn server_mut(&mut self) -> &mut ServerSocket {
        &mut self.server
    }

    pub fn did_open(
        &mut self,
        uri: Url,
        language_id: &str,
        version: i32,
        text: String,
    ) -> Result<()> {
        self.server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: language_id.to_string(),
                    version,
                    text,
                },
            })
            .context("did_open")
    }

    pub fn did_change_full(&mut self, uri: Url, version: i32, text: String) -> Result<()> {
        self.server
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier { uri, version },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text,
                }],
            })
            .context("did_change")
    }

    pub fn did_save(&mut self, uri: Url) -> Result<()> {
        // No `text`: rust-analyzer (the server this exists for) registers
        // save notifications with includeText=false; it re-reads from disk.
        self.server
            .did_save(DidSaveTextDocumentParams {
                text_document: TextDocumentIdentifier { uri },
                text: None,
            })
            .context("did_save")
    }

    pub fn did_close(&mut self, uri: Url) -> Result<()> {
        self.server
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri },
            })
            .context("did_close")
    }

    pub async fn completion(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
    ) -> Result<Option<CompletionResponse>> {
        self.server
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: Some(CompletionContext {
                    trigger_kind: CompletionTriggerKind::INVOKED,
                    trigger_character: None,
                }),
            })
            .await
            .context("completion")
    }

    pub async fn signature_help(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
    ) -> Result<Option<lsp_types::SignatureHelp>> {
        self.server
            .signature_help(lsp_types::SignatureHelpParams {
                context: None,
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .context("signature_help")
    }

    pub async fn hover(&mut self, uri: Url, line: u32, character: u32) -> Result<Option<Hover>> {
        self.server
            .hover(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .context("hover")
    }

    pub async fn definition(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
    ) -> Result<Option<GotoDefinitionResponse>> {
        self.server
            .definition(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("definition")
    }

    /// Ask the server for the document's symbol tree (`textDocument/
    /// documentSymbol`), the data behind the Outline view. Servers answer with
    /// either the modern `Nested` hierarchy or the legacy `Flat` list; the
    /// worker normalises both. No position: the request covers the whole file.
    pub async fn document_symbol(&mut self, uri: Url) -> Result<Option<DocumentSymbolResponse>> {
        self.server
            .document_symbol(DocumentSymbolParams {
                text_document: TextDocumentIdentifier { uri },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("document_symbol")
    }

    pub async fn declaration(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
    ) -> Result<Option<GotoDefinitionResponse>> {
        // `GotoDeclarationParams`/`GotoDeclarationResponse` are type aliases for
        // the definition variants in lsp-types, so the same params and the same
        // `def_location` parser are reused; only the wire method differs.
        self.server
            .declaration(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("declaration")
    }

    pub async fn type_definition(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
    ) -> Result<Option<GotoDefinitionResponse>> {
        // `GotoTypeDefinitionParams`/`GotoTypeDefinitionResponse` are type aliases
        // for the definition variants in lsp-types, so the same params and the
        // same `def_location` parser are reused; only the wire method differs
        // (`textDocument/typeDefinition`).
        self.server
            .type_definition(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("type_definition")
    }

    pub async fn implementation(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
    ) -> Result<Option<GotoDefinitionResponse>> {
        // `GotoImplementationParams`/`GotoImplementationResponse` are type aliases
        // for the definition variants in lsp-types, so the same params and the
        // same `def_location` parser are reused; only the wire method differs
        // (`textDocument/implementation`). Note this commonly returns an Array of
        // locations (one abstraction, many implementors); `def_location` jumps to
        // the first.
        self.server
            .implementation(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("implementation")
    }

    /// `workspace/symbol`: server-side fuzzy query over every symbol in the
    /// project. Both response shapes (flat `SymbolInformation` and nested
    /// `WorkspaceSymbol`) are normalised by the manager.
    pub async fn workspace_symbols(
        &mut self,
        query: String,
    ) -> Result<Option<lsp_types::WorkspaceSymbolResponse>> {
        self.server
            .symbol(lsp_types::WorkspaceSymbolParams {
                query,
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("workspace/symbol")
    }

    pub async fn references(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
    ) -> Result<Option<Vec<Location>>> {
        // `textDocument/references` is the odd one out of the navigation family:
        // it takes `ReferenceParams` (not `GotoDefinitionParams`) and returns a
        // flat `Vec<Location>` rather than a `GotoDefinitionResponse`, so it has
        // its own params and its own `reference_locations` mapper in the manager.
        // `include_declaration: true` matches VS Code's "Go to References", whose
        // peek list includes the declaration site alongside every use.
        self.server
            .references(ReferenceParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: ReferenceContext {
                    include_declaration: true,
                },
            })
            .await
            .context("references")
    }

    /// `textDocument/prepareCallHierarchy`: resolve the symbol at a position
    /// into the item the `callHierarchy/*` expansion requests take.
    pub async fn prepare_call_hierarchy(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
    ) -> Result<Option<Vec<lsp_types::CallHierarchyItem>>> {
        self.server
            .prepare_call_hierarchy(lsp_types::CallHierarchyPrepareParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .context("prepareCallHierarchy")
    }

    /// `callHierarchy/incomingCalls`: everyone who calls `item`.
    pub async fn incoming_calls(
        &mut self,
        item: lsp_types::CallHierarchyItem,
    ) -> Result<Option<Vec<lsp_types::CallHierarchyIncomingCall>>> {
        self.server
            .incoming_calls(lsp_types::CallHierarchyIncomingCallsParams {
                item,
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("incomingCalls")
    }

    /// `callHierarchy/outgoingCalls`: everything `item` calls.
    pub async fn outgoing_calls(
        &mut self,
        item: lsp_types::CallHierarchyItem,
    ) -> Result<Option<Vec<lsp_types::CallHierarchyOutgoingCall>>> {
        self.server
            .outgoing_calls(lsp_types::CallHierarchyOutgoingCallsParams {
                item,
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("outgoingCalls")
    }

    /// `textDocument/documentHighlight`: the read/write occurrences of the
    /// symbol at a position, within this document only. Backs the editor's
    /// occurrences tint (VS Code's word highlight), not any navigation.
    pub async fn document_highlight(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
    ) -> Result<Option<Vec<lsp_types::DocumentHighlight>>> {
        self.server
            .document_highlight(lsp_types::DocumentHighlightParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("document_highlight")
    }

    /// `textDocument/inlayHint` over the whole document (`line_count` caps the
    /// range end). VS Code requests per viewport and stitches; one whole-file
    /// request per edit-batch is simpler and matches how croft already pulls
    /// `semanticTokens/full`.
    pub async fn inlay_hints(
        &mut self,
        uri: Url,
        line_count: u32,
    ) -> Result<Option<Vec<lsp_types::InlayHint>>> {
        self.server
            .inlay_hint(lsp_types::InlayHintParams {
                text_document: TextDocumentIdentifier { uri },
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: line_count,
                        character: 0,
                    },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .context("inlay_hint")
    }

    pub async fn rename(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
        new_name: String,
    ) -> Result<Option<WorkspaceEdit>> {
        self.server
            .rename(RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                new_name,
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .context("rename")
    }

    /// `textDocument/codeAction`: ask the server for the quick fixes and
    /// refactors available over `range` (a point selection at the cursor, or a
    /// real selection). `diagnostics` are the diagnostics overlapping the range,
    /// passed as context so the server can offer fixes for them.
    pub async fn code_action(
        &mut self,
        uri: Url,
        range: Range,
        diagnostics: Vec<LspDiagnostic>,
    ) -> Result<Option<CodeActionResponse>> {
        self.server
            .code_action(CodeActionParams {
                text_document: TextDocumentIdentifier { uri },
                range,
                context: CodeActionContext {
                    diagnostics,
                    only: None,
                    trigger_kind: None,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("code_action")
    }

    /// `codeAction/resolve`: fill in the `edit` for an action the server
    /// returned without one (it carried only `data`). vtsls and rust-analyzer
    /// defer the expensive edit computation to this second round trip.
    pub async fn code_action_resolve(&mut self, action: CodeAction) -> Result<CodeAction> {
        self.server
            .code_action_resolve(action)
            .await
            .context("code_action_resolve")
    }

    /// `workspace/executeCommand`: run a command a code action asked for (some
    /// actions perform their effect through a command rather than an edit).
    pub async fn execute_command(
        &mut self,
        command: String,
        arguments: Vec<serde_json::Value>,
    ) -> Result<Option<serde_json::Value>> {
        self.server
            .execute_command(ExecuteCommandParams {
                command,
                arguments,
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .context("execute_command")
    }

    /// `textDocument/formatting`: ask the server to reformat the whole
    /// document. Returns the edits to apply (servers usually answer with a
    /// single full-document replacement, but the spec allows many disjoint
    /// edits). `tab_size`/`insert_spaces` carry the editor's indentation
    /// preference; most formatters (rustfmt, ruff, prettier) honour their own
    /// project config and ignore these, but the LSP fields are required.
    pub async fn formatting(
        &mut self,
        uri: Url,
        tab_size: u32,
        insert_spaces: bool,
    ) -> Result<Option<Vec<TextEdit>>> {
        self.server
            .formatting(DocumentFormattingParams {
                text_document: TextDocumentIdentifier { uri },
                options: FormattingOptions {
                    tab_size,
                    insert_spaces,
                    ..FormattingOptions::default()
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .context("formatting")
    }

    /// `textDocument/onTypeFormatting`: the server's formatting reaction
    /// to one just-typed trigger character (#254).
    pub async fn on_type_formatting(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
        ch: String,
        tab_size: u32,
        insert_spaces: bool,
    ) -> Result<Option<Vec<TextEdit>>> {
        self.server
            .on_type_formatting(lsp_types::DocumentOnTypeFormattingParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                ch,
                options: FormattingOptions {
                    tab_size,
                    insert_spaces,
                    ..FormattingOptions::default()
                },
            })
            .await
            .context("on_type_formatting")
    }

    pub async fn semantic_tokens_full(&mut self, uri: Url) -> Result<Option<SemanticTokensResult>> {
        self.server
            .semantic_tokens_full(SemanticTokensParams {
                text_document: TextDocumentIdentifier { uri },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("semantic_tokens_full")
    }

    /// Semantic tokens for just the visible line range. ty answers a viewport
    /// query in tens of milliseconds even on a cold workspace, so croft fires
    /// this on open to paint on-screen code immediately while the whole-file
    /// `semantic_tokens_full` request fills in the rest behind it (the VS Code
    /// / Zed first-paint model). `start`/`end` are zero-based line numbers.
    pub async fn semantic_tokens_range(
        &mut self,
        uri: Url,
        start: u32,
        end: u32,
    ) -> Result<Option<SemanticTokensRangeResult>> {
        self.server
            .semantic_tokens_range(SemanticTokensRangeParams {
                text_document: TextDocumentIdentifier { uri },
                range: Range {
                    start: Position {
                        line: start,
                        character: 0,
                    },
                    end: Position {
                        line: end,
                        character: 0,
                    },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .context("semantic_tokens_range")
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.server.shutdown(()).await.context("lsp shutdown")?;
        self.server.exit(()).context("lsp exit")?;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), self.child.wait()).await;
        Ok(())
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Abort the MainLoop before `server` (its ServerSocket) drops below.
        // Otherwise the loop polls a closed event channel and panics with
        // "Sender is alive" (async-lsp lib.rs:553). Graceful shutdown() leaves
        // the task already finished, so this is a no-op in that case.
        self.mainloop_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::runtime::LspRuntime;
    use async_lsp::{AnyNotification, LspService};
    use serde_json::json;

    #[test]
    fn stderr_level_reads_the_tracing_level_token() {
        // Issue #37: rust-analyzer writes warnings to stderr in tracing
        // format; croft showed them all as Info. The level token must map
        // to the matching OUTPUT level.
        assert_eq!(
            stderr_level(
                "2026-07-02T10:00:00Z WARN rust_analyzer::main_loop: overly long loop turn: 141ms"
            ),
            OutputLevel::Warn
        );
        assert_eq!(
            stderr_level("[ERROR rust_analyzer::lsp] request failed"),
            OutputLevel::Error
        );
        assert_eq!(
            stderr_level("2026-07-02T10:00:00Z INFO ra: loaded workspace"),
            OutputLevel::Info
        );
        assert_eq!(
            stderr_level("2026-07-02T10:00:00Z DEBUG ra: vfs tick"),
            OutputLevel::Debug
        );
        // A level word deep in the message body is not a level token.
        assert_eq!(
            stderr_level("loading config a b c d e ERROR handling disabled"),
            OutputLevel::Info
        );
        // Unstructured stderr chatter stays Info.
        assert_eq!(stderr_level("starting language server"), OutputLevel::Info);
    }

    #[test]
    fn compose_progress_joins_title_message_and_percentage() {
        // rust-analyzer's $/progress: Begin carries the title ("Indexing"),
        // Report carries an optional message ("112/340") and percentage. We
        // surface them as one compact status string.
        assert_eq!(compose_progress("Indexing", None, None), "Indexing");
        assert_eq!(
            compose_progress("Indexing", Some("112/340"), None),
            "Indexing 112/340"
        );
        assert_eq!(
            compose_progress("Indexing", Some("112/340"), Some(33)),
            "Indexing 112/340 33%"
        );
        assert_eq!(
            compose_progress("Roots Scanned", None, Some(100)),
            "Roots Scanned 100%"
        );
    }

    #[test]
    fn semantic_tokens_refresh_request_sets_the_repull_flag() {
        use async_lsp::AnyRequest;
        use tower::Service;
        // rust-analyzer sends `workspace/semanticTokens/refresh` once its
        // analysis upgrades the token set; the router must acknowledge it AND
        // raise the shared flag so the app re-pulls (vs the old behaviour of
        // declining it with METHOD_NOT_FOUND and never updating).
        let flag = Arc::new(AtomicBool::new(false));
        let inlay_flag = Arc::new(AtomicBool::new(false));
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let mut router = ClientState::router(
            "rust-analyzer".into(),
            flag.clone(),
            inlay_flag,
            diag_tx,
            prog_tx,
            Vec::new(),
        );
        let req: AnyRequest = serde_json::from_value(json!({
            "id": 1,
            "method": "workspace/semanticTokens/refresh",
            "params": null
        }))
        .expect("AnyRequest deserialization");
        let _ = futures::executor::block_on(router.call(req));
        assert!(
            flag.load(Ordering::Relaxed),
            "a workspace/semanticTokens/refresh request must set the re-pull flag"
        );
    }

    #[test]
    fn inlay_hint_refresh_request_sets_the_rerequest_flag() {
        use async_lsp::AnyRequest;
        use tower::Service;
        // rust-analyzer sends `workspace/inlayHint/refresh` when its analysis
        // (or a config reload) changes the hints without an edit; the router
        // must acknowledge it AND queue a re-request.
        let sem_flag = Arc::new(AtomicBool::new(false));
        let flag = Arc::new(AtomicBool::new(false));
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let mut router = ClientState::router(
            "rust-analyzer".into(),
            sem_flag.clone(),
            flag.clone(),
            diag_tx,
            prog_tx,
            Vec::new(),
        );
        let req: AnyRequest = serde_json::from_value(json!({
            "id": 1,
            "method": "workspace/inlayHint/refresh",
            "params": null
        }))
        .expect("AnyRequest deserialization");
        let _ = futures::executor::block_on(router.call(req));
        assert!(
            flag.load(Ordering::Relaxed),
            "a workspace/inlayHint/refresh request must set the re-request flag"
        );
        assert!(
            !sem_flag.load(Ordering::Relaxed),
            "the semantic-token flag must stay untouched"
        );
    }

    #[test]
    fn convert_diagnostic_maps_severity_and_utf16_range() {
        use lsp_types::{Position, Range};
        let wire = lsp_types::Diagnostic {
            range: Range {
                start: Position {
                    line: 6,
                    character: 25,
                },
                end: Position {
                    line: 6,
                    character: 32,
                },
            },
            severity: Some(LspDiagnosticSeverity::WARNING),
            message: "Property 'capitol' does not exist".into(),
            ..Default::default()
        };
        let d = convert_diagnostic(&wire);
        assert_eq!(d.start_line, 6);
        assert_eq!(d.start_char, 25);
        assert_eq!(d.end_line, 6);
        assert_eq!(d.end_char, 32);
        assert_eq!(d.severity, DiagnosticSeverity::Warning);
        assert_eq!(d.message, "Property 'capitol' does not exist");
    }

    #[test]
    fn convert_diagnostic_defaults_missing_severity_to_error() {
        use lsp_types::{Position, Range};
        let wire = lsp_types::Diagnostic {
            range: Range {
                start: Position::default(),
                end: Position::default(),
            },
            severity: None,
            message: "unclassified".into(),
            ..Default::default()
        };
        assert_eq!(
            convert_diagnostic(&wire).severity,
            DiagnosticSeverity::Error,
            "an unclassified problem is shown as an error, not silently dropped"
        );
    }

    #[test]
    fn router_continues_past_unhandled_vendor_notification_like_pyright_begin_progress() {
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let mut router = ClientState::router(
            "basedpyright".into(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            diag_tx,
            prog_tx,
            Vec::new(),
        );
        let notif: AnyNotification = serde_json::from_value(json!({
            "method": "pyright/beginProgress",
            "params": {}
        }))
        .expect("AnyNotification deserialization");
        match router.notify(notif) {
            ControlFlow::Continue(()) => {}
            ControlFlow::Break(result) => {
                panic!(
                    "router must absorb vendor-specific notifications (basedpyright streams pyright/beginProgress / pyright/endProgress / pyright/reportProgress during workspace indexing) instead of breaking the mainloop; got Break({result:?})"
                );
            }
        }
    }

    #[test]
    fn router_continues_past_arbitrary_unhandled_notification_methods() {
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let mut router = ClientState::router(
            "any".into(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            diag_tx,
            prog_tx,
            Vec::new(),
        );
        for method in ["foo/bar", "experimental/dap", "rust-analyzer/serverStatus"] {
            let notif: AnyNotification = serde_json::from_value(json!({
                "method": method,
                "params": {}
            }))
            .expect("AnyNotification deserialization");
            assert!(
                matches!(router.notify(notif), ControlFlow::Continue(())),
                "router must absorb the vendor notification {method} instead of killing the mainloop with an Unhandled-notification routing error"
            );
        }
    }

    #[test]
    fn workspace_folders_request_is_answered_not_declined() {
        use async_lsp::AnyRequest;
        use tower::Service;
        // LSP 3.6 `workspace/workspaceFolders` is a SERVER->CLIENT request:
        // a server that saw the workspaceFolders capability may re-query the
        // folder list at any time. Declining it with MethodNotFound tells a
        // folder-aware server the list is unavailable and legally degrades
        // it to single-root behavior.
        let sem_flag = Arc::new(AtomicBool::new(false));
        let inlay_flag = Arc::new(AtomicBool::new(false));
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let mut router = ClientState::router(
            "rust-analyzer".into(),
            sem_flag,
            inlay_flag,
            diag_tx,
            prog_tx,
            vec![WorkspaceFolder {
                uri: Url::from_file_path("/tmp/ws").unwrap(),
                name: "ws".into(),
            }],
        );
        let req: AnyRequest = serde_json::from_value(json!({
            "id": 1,
            "method": "workspace/workspaceFolders",
            "params": null
        }))
        .expect("AnyRequest deserialization");
        let out = futures::executor::block_on(router.call(req));
        let value = out
            .expect("the client must answer workspace/workspaceFolders with its folder list, not decline it");
        assert_eq!(
            value,
            json!([{ "uri": "file:///tmp/ws", "name": "ws" }]),
            "the reply is the same folder list initialize carried"
        );
    }

    fn server_on_path(cmd: &str) -> bool {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        for entry in std::env::split_paths(&path) {
            let candidate = entry.join(cmd);
            if !candidate.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&candidate)
                    && meta.permissions().mode() & 0o111 != 0
                {
                    return true;
                }
            }
            #[cfg(not(unix))]
            {
                return true;
            }
        }
        false
    }

    fn first_available_python_server() -> Option<ServerConfig> {
        if server_on_path("basedpyright-langserver") {
            Some(ServerConfig::basedpyright())
        } else if server_on_path("pyright-langserver") {
            Some(ServerConfig::pyright())
        } else {
            None
        }
    }

    fn run_initialize_shutdown(config: ServerConfig) {
        let rt = LspRuntime::new().expect("runtime");
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize tempdir");
        let expected_name = config.name;
        let display_name = config.name;

        let result: Result<()> = rt.handle().block_on(async move {
            let (diag_tx, _diag_rx) = std_mpsc::channel();
            let (prog_tx, _prog_rx) = std_mpsc::channel();
            let client = LspClient::spawn(
                &config,
                &root,
                ClientCapabilities::default(),
                &[],
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
                diag_tx,
                prog_tx,
            )
            .await?;
            assert_eq!(client.name(), expected_name);
            let mut client = client;
            client.shutdown().await
        });

        if let Err(e) = result {
            eprintln!("SKIPPED: {display_name} initialize/shutdown failed: {e}");
        }
    }

    #[test]
    fn initialize_and_shutdown_python_server() {
        let Some(config) = first_available_python_server() else {
            eprintln!("SKIPPED: no basedpyright/pyright on PATH");
            return;
        };
        run_initialize_shutdown(config);
    }

    #[test]
    fn initialize_and_shutdown_rust_analyzer() {
        if !server_on_path("rust-analyzer") {
            eprintln!("SKIPPED: rust-analyzer not on PATH");
            return;
        }
        run_initialize_shutdown(ServerConfig::rust_analyzer());
    }

    #[test]
    fn initialize_and_shutdown_ruff_server() {
        if !server_on_path("ruff") {
            eprintln!("SKIPPED: ruff not on PATH");
            return;
        }
        run_initialize_shutdown(ServerConfig::ruff());
    }
}
