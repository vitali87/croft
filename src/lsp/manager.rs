use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;

use anyhow::Result;
use lsp_types::{
    ClientCapabilities, CodeActionCapabilityResolveSupport, CodeActionClientCapabilities,
    CodeActionKindLiteralSupport, CodeActionLiteralSupport, CodeActionOrCommand,
    CodeActionProviderCapability, CodeActionResponse, CompletionClientCapabilities,
    CompletionItemCapability, CompletionItemKind, CompletionItemKindCapability, CompletionResponse,
    DeclarationCapability, DiagnosticServerCapabilities, DocumentChangeOperation, DocumentChanges,
    DocumentSymbol, DocumentSymbolClientCapabilities, DocumentSymbolResponse,
    GotoDefinitionResponse, HoverContents, HoverProviderCapability,
    ImplementationProviderCapability, Location, MarkedString, MarkupKind, OneOf, Position,
    PublishDiagnosticsClientCapabilities, SemanticTokenModifier, SemanticTokenType,
    SemanticTokensClientCapabilities, SemanticTokensClientCapabilitiesRequests,
    SemanticTokensFullOptions, SemanticTokensRangeResult, SemanticTokensResult,
    SemanticTokensServerCapabilities, SemanticTokensWorkspaceClientCapabilities,
    ServerCapabilities, SymbolKind, TextDocumentClientCapabilities, TextEdit, TokenFormat,
    TypeDefinitionProviderCapability, Url, WindowClientCapabilities, WorkspaceClientCapabilities,
    WorkspaceEdit,
};
use tokio::sync::{Mutex as TokioMutex, mpsc as tokio_mpsc, oneshot};

use crate::lsp::client::LspClient;
use crate::lsp::config::{Language, ServerConfig};
use crate::lsp::log_file;
use crate::lsp::registry::ServerRegistry;
use crate::lsp::runtime::LspRuntime;
use crate::text_edit::TextSpanEdit;

mod capabilities;
mod code_actions;
mod completion;
mod decorations;
mod diagnostics;
mod discovery;
mod formatting;
mod hover;
mod navigation;
mod rename;
mod semantic_tokens;
mod symbols;
mod text_ranges;
use capabilities::*;
#[cfg(test)]
pub use code_actions::CodeActionCommand;
pub use code_actions::CodeActionItem;
use code_actions::*;
pub use completion::CompletionItem;
pub use completion::CompletionResult;
pub use completion::CompletionTrigger;
pub use completion::InlineCompletionResult;
pub use completion::InlineItem;
pub use completion::SignatureInfo;
use completion::*;
pub use decorations::ColorItem;
pub use decorations::DocumentLinkItem;
pub use decorations::FoldRangeKind;
pub use decorations::FoldingRangeItem;
pub use decorations::InlayHintItem;
use decorations::*;
pub use diagnostics::Diagnostic;
pub use diagnostics::DiagnosticCode;
pub use diagnostics::DiagnosticOrigin;
pub use diagnostics::DiagnosticSeverity;
pub use diagnostics::DiagnosticTags;
pub use diagnostics::DiagnosticsUpdate;
pub use diagnostics::diagnostics_by_source;
pub use diagnostics::diagnostics_source;
pub use diagnostics::diagnostics_store_key;
pub use diagnostics::one_line;
use diagnostics::*;
pub(crate) use discovery::is_executable_file;
pub(crate) use discovery::is_on_path;
pub(crate) use discovery::path_to_language;
use discovery::*;
pub use formatting::FormatResult;
pub use hover::HoverResult;
pub use hover::RustAnalyzerResult;
use hover::*;
pub use navigation::DeclarationResult;
pub use navigation::DefinitionResult;
pub use navigation::HierarchyKind;
pub use navigation::TypeDefinitionResult;
use navigation::*;
pub use rename::FileRenameOp;
pub use rename::PrepareRenameResult;
use rename::*;
use semantic_tokens::*;
pub use symbols::OutlineKind;
pub use symbols::OutlineSymbol;
pub use symbols::WorkspaceSymbolItem;
use symbols::*;
pub use text_ranges::LinkedEditingResult;
pub use text_ranges::OccurrenceItem;
use text_ranges::*;

/// Something a server tells the user rather than the editor: its work-done
/// progress and its `window/*` messages, in the order it sent them.
#[derive(Debug)]
pub struct ServerUpdate {
    pub server: String,
    pub event: ServerEvent,
}

#[derive(Debug)]
pub enum ServerEvent {
    /// A `$/progress` update: `Some(text)` while a task is running (Begin /
    /// Report) and `None` when it ends (End), so the app can show or clear
    /// the per-server status (e.g. "rust-analyzer: Indexing 112/340 33%").
    Progress(Option<String>),
    /// A `window/showMessage` the user should see (#1599): an error, a
    /// warning, or a note with a link in it, such as rust-analyzer's
    /// "Failed to load workspaces" or gopls's "open your browser to …".
    /// Other notes and every `window/logMessage` stay in OUTPUT.
    Message {
        /// Read by the tests, to prove the level is forwarded.
        #[cfg_attr(not(test), allow(dead_code))]
        level: lsp_types::MessageType,
        text: String,
    },
    /// A `window/showMessageRequest`: a question with `actions` to pick
    /// from. The pick, or `None` when the user dismisses it, goes back on
    /// `reply`; dropping `reply` answers `None` too.
    Question {
        text: String,
        actions: Vec<lsp_types::MessageActionItem>,
        reply: oneshot::Sender<Option<lsp_types::MessageActionItem>>,
    },
    /// A `window/showDocument`: open `uri` in a tab, or in the browser when
    /// `external`, with `selection` selected, moving keyboard focus to it
    /// only when `take_focus`. Whether it was shown goes back on `reply`;
    /// dropping `reply` answers "not shown".
    ShowDocument {
        uri: lsp_types::Url,
        external: bool,
        take_focus: bool,
        selection: Option<lsp_types::Range>,
        reply: oneshot::Sender<bool>,
    },
}

/// Whether a `window/showMessage` is worth interrupting the user for: an
/// error or a warning, or a note that points somewhere (gopls answers
/// "Browse documentation" with "open your browser to <url>"). Anything else
/// is chatter that OUTPUT keeps.
pub fn message_is_shown(level: lsp_types::MessageType, text: &str) -> bool {
    use lsp_types::MessageType as MT;
    level == MT::ERROR
        || level == MT::WARNING
        || (level == MT::INFO && (text.contains("https://") || text.contains("http://")))
}

enum Cmd {
    /// Gracefully shut every server down (LSP `shutdown`+`exit`, then await the
    /// child) and acknowledge on `done`. Sent by `LspManager`'s Drop so servers
    /// exit cleanly instead of being SIGKILLed by `kill_on_drop`.
    Shutdown {
        done: oneshot::Sender<()>,
    },
    OpenDoc {
        path: PathBuf,
        text: String,
    },
    /// Stop and start again the servers that serve `path` (#1621), leaving
    /// every other language's servers running.
    RestartServersFor {
        path: PathBuf,
    },
    /// Serve from `registry` from now on: the installed extensions or the
    /// disable set changed (#1842).
    ReplaceRegistry {
        registry: ServerRegistry,
    },
    /// Spawn the servers for every language whose project-root marker sits
    /// in the workspace root, before any file of theirs is opened (#27):
    /// rust-analyzer's cold indexing then overlaps with the user browsing
    /// the tree instead of starting at the first `.rs` open.
    PrewarmWorkspace,
    /// Replace the secondary workspace-root set (multi-root, #147): widens
    /// the project-root bound for future opens and prewarms roots that are
    /// new, so an added folder's servers start like a fresh workspace's.
    SetExtraRoots {
        roots: Vec<PathBuf>,
    },
    /// Pull whole-project diagnostics from every server that advertises
    /// `workspaceDiagnostics` (#533). Carries nothing: the previous result
    /// ids live with the client they were issued by.
    RequestWorkspaceDiagnostics,
    RequestSemanticTokens {
        path: PathBuf,
        seq: u64,
        generation: u64,
    },
    RequestSemanticTokensRange {
        path: PathBuf,
        start_line: u32,
        end: Position,
        seq: u64,
        generation: u64,
    },
    RequestInlayHints {
        path: PathBuf,
        end: Position,
        seq: u64,
    },
    RequestDocumentLinks {
        path: PathBuf,
        seq: u64,
    },
    RequestCodeLens {
        path: PathBuf,
        seq: u64,
    },
    RequestFoldingRanges {
        path: PathBuf,
        seq: u64,
    },
    RequestDocumentColors {
        path: PathBuf,
        seq: u64,
    },
    RequestColorPresentations {
        request_id: u64,
        path: PathBuf,
        color: (f32, f32, f32, f32),
        start: (u32, u32),
        end: (u32, u32),
    },
    ChangeDoc {
        path: PathBuf,
        text: String,
    },
    SaveDoc {
        path: PathBuf,
        text: String,
    },
    CloseDoc {
        path: PathBuf,
    },
    RequestCompletion {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        trigger: CompletionTrigger,
    },
    ResolveCompletion {
        request_id: u64,
        path: PathBuf,
        item: Arc<lsp_types::CompletionItem>,
    },
    RequestSignatureHelp {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RequestHover {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RustAnalyzer {
        request_id: u64,
        path: PathBuf,
        request: crate::lsp::rust_analyzer::RaRequest,
        line: u32,
        character: u32,
    },
    RequestDefinition {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RequestDocumentSymbols {
        path: PathBuf,
        seq: u64,
    },
    RequestDeclaration {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RequestTypeDefinition {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RequestImplementation {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RequestWorkspaceSymbols {
        request_id: u64,
        query: String,
    },
    RequestReferences {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RequestDocumentHighlights {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RequestLinkedEditing {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RequestInlineCompletion {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RequestSelectionRanges {
        request_id: u64,
        path: PathBuf,
        positions: Vec<(u32, u32)>,
    },
    RequestCallHierarchy {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        incoming: bool,
    },
    RequestTypeHierarchy {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        supertypes: bool,
    },
    RequestRename {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        new_name: String,
    },
    RequestPrepareRename {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RequestWillRenameFiles {
        request_id: u64,
        renames: Vec<FileRenameOp>,
    },
    DidRenameFiles {
        renames: Vec<FileRenameOp>,
    },
    DidChangeWatchedFiles {
        changes: Vec<(PathBuf, lsp_types::FileChangeType)>,
    },
    RequestFormatting {
        request_id: u64,
        path: PathBuf,
        tab_size: u32,
        insert_spaces: bool,
    },
    RequestOnTypeFormatting {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        ch: char,
        tab_size: u32,
        insert_spaces: bool,
    },
    RequestRangeFormatting {
        request_id: u64,
        path: PathBuf,
        start: (u32, u32),
        end: (u32, u32),
        tab_size: u32,
        insert_spaces: bool,
    },
    RequestCodeAction {
        request_id: u64,
        path: PathBuf,
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
        diagnostics: Vec<Diagnostic>,
    },
    ResolveCodeAction {
        request_id: u64,
        path: PathBuf,
        server: String,
        /// The full original `CodeAction` as JSON, sent back to the server intact.
        action: serde_json::Value,
    },
    ExecuteCommand {
        path: PathBuf,
        command: String,
        arguments: Vec<serde_json::Value>,
    },
}

/// Per-language support for the optional, capability-gated navigation methods,
/// published by the worker once a server has spawned and read synchronously by
/// the app at menu-build time. The app only shows "Go to Declaration" /
/// "Go to Type Definition" for languages whose server actually implements the
/// matching method (so Declaration is hidden for TypeScript, whose vtsls
/// advertises `declarationProvider: false`), mirroring VS Code.
#[derive(Default)]
struct LangCapabilitySupport {
    declaration: HashMap<Language, bool>,
    type_definition: HashMap<Language, bool>,
    implementation: HashMap<Language, bool>,
    references: HashMap<Language, bool>,
    formatting: HashMap<Language, bool>,
    code_action: HashMap<Language, bool>,
    call_hierarchy: HashMap<Language, bool>,
    /// The server instances, by (language, project root) as
    /// `request_inline_completion` picks them, one of whose running servers
    /// offers inline completions (#607).
    inline_completion: HashSet<ClientKey>,
    /// Whether a server for the language wants `willRenameFiles` (#610).
    will_rename: HashMap<Language, bool>,
    /// Concatenated on-type trigger characters per language (#254);
    /// a missing entry means "not probed yet".
    on_type_triggers: HashMap<Language, String>,
    /// The `triggerCharacters` of each server instance's completion server
    /// (#929), by (language, project root) as `request_completion` picks it;
    /// no entry while none of that instance's running servers completes.
    completion_triggers: HashMap<ClientKey, String>,
    /// The project root each open document's servers run at (#929), so a
    /// file's typed triggers are those of the server that answers it.
    document_roots: HashMap<PathBuf, PathBuf>,
    /// The servers reporting on each open document, by name (#851): the
    /// built-in Markdown lint stands down for a file only while rumdl
    /// reports on it. A document the app never opened here (one shown only
    /// in a split's other group) has no entry, and a server leaves the
    /// lists it was on when it is retired from document pulls or given up
    /// on after crashing.
    document_servers: HashMap<PathBuf, Vec<String>>,
    /// Every server open on each open document, by name (#1626), retired
    /// from document pulls or not: a server that let one diagnostic pull
    /// lapse still answers other requests, so this, not
    /// `document_servers`, says whether it serves the file.
    document_clients: HashMap<PathBuf, Vec<String>>,
}
type CapabilitySupport = Arc<StdMutex<LangCapabilitySupport>>;

/// The names of the servers `registry` registers, per language.
fn registered_servers_of(registry: &ServerRegistry) -> HashMap<Language, Vec<&'static str>> {
    registry
        .languages()
        .map(|lang| {
            let names = registry.for_language(lang).iter().map(|c| c.name).collect();
            (lang, names)
        })
        .collect()
}

pub struct LspManager {
    /// The servers registered per language once the disable set is
    /// applied (#1511), so the app can tell whether a server is in play.
    registered_servers: HashMap<Language, Vec<&'static str>>,
    /// Monotonic stamp handed to each semantic-token request. `seq`
    /// identifies CONTENT, not a request: `sync_lsp` and a server-driven
    /// refresh can have two full requests in flight at the same seq, and the
    /// retry loop emits an EMPTY batch after exhausting its backoff — so
    /// without an ordering stamp a late empty loser can blank a good overlay
    /// and be persisted to the content-keyed cache.
    semantic_generation: std::sync::atomic::AtomicU64,
    cmd_tx: tokio_mpsc::UnboundedSender<Cmd>,
    completion_rx: std_mpsc::Receiver<CompletionResult>,
    signature_help_rx: std_mpsc::Receiver<SignatureHelpResult>,
    hover_rx: std_mpsc::Receiver<HoverResult>,
    /// Test hook: a sender into `hover_rx`, so app tests can answer a
    /// hover request without a server.
    #[cfg(test)]
    hover_test_tx: std_mpsc::Sender<HoverResult>,
    rust_analyzer_rx: std_mpsc::Receiver<RustAnalyzerResult>,
    def_rx: std_mpsc::Receiver<DefinitionResult>,
    /// Test hooks: senders into the three jump channels, so app tests can
    /// answer a definition, declaration or type definition request
    /// without a server.
    #[cfg(test)]
    jump_test_tx: (
        std_mpsc::Sender<DefinitionResult>,
        std_mpsc::Sender<DeclarationResult>,
        std_mpsc::Sender<TypeDefinitionResult>,
    ),
    doc_symbols_rx: std_mpsc::Receiver<DocumentSymbolsResult>,
    decl_rx: std_mpsc::Receiver<DeclarationResult>,
    type_def_rx: std_mpsc::Receiver<TypeDefinitionResult>,
    impl_rx: std_mpsc::Receiver<ImplementationResult>,
    ref_rx: std_mpsc::Receiver<ReferencesResult>,
    doc_highlights_rx: std_mpsc::Receiver<DocumentHighlightsResult>,
    linked_editing_rx: std_mpsc::Receiver<LinkedEditingResult>,
    /// Test hook: a sender into `linked_editing_rx`, so app tests can
    /// answer a linkedEditingRange request without a server.
    #[cfg(test)]
    linked_editing_test_tx: std_mpsc::Sender<LinkedEditingResult>,
    inline_completion_rx: std_mpsc::Receiver<InlineCompletionResult>,
    /// Test hook: a sender into `inline_completion_rx`, so app tests can
    /// answer an inlineCompletion request without a server.
    #[cfg(test)]
    inline_completion_test_tx: std_mpsc::Sender<InlineCompletionResult>,
    /// Test hook: the text each document was last opened or changed with.
    #[cfg(test)]
    synced_texts: std::sync::Mutex<HashMap<PathBuf, String>>,
    selection_ranges_rx: std_mpsc::Receiver<SelectionRangesResult>,
    calls_rx: std_mpsc::Receiver<CallHierarchyResult>,
    ws_symbols_rx: std_mpsc::Receiver<WorkspaceSymbolsResult>,
    rename_rx: std_mpsc::Receiver<RenameResult>,
    prepare_rename_rx: std_mpsc::Receiver<PrepareRenameResult>,
    will_rename_files_rx: std_mpsc::Receiver<WillRenameFilesResult>,
    format_rx: std_mpsc::Receiver<FormatResult>,
    on_type_rx: std_mpsc::Receiver<FormatResult>,
    code_action_rx: std_mpsc::Receiver<CodeActionResult>,
    semantic_rx: std_mpsc::Receiver<SemanticTokensUpdate>,
    inlay_rx: std_mpsc::Receiver<InlayHintsUpdate>,
    links_rx: std_mpsc::Receiver<DocumentLinksUpdate>,
    code_lens_rx: std_mpsc::Receiver<CodeLensUpdate>,
    folding_rx: std_mpsc::Receiver<FoldingRangesUpdate>,
    colors_rx: std_mpsc::Receiver<DocumentColorsUpdate>,
    color_presentations_rx: std_mpsc::Receiver<ColorPresentationsResult>,
    diagnostics_rx: std_mpsc::Receiver<DiagnosticsUpdate>,
    /// Test hook: a sender into `diagnostics_rx`, so app tests can deliver
    /// a server's report without a server.
    #[cfg(test)]
    diagnostics_test_tx: std_mpsc::Sender<DiagnosticsUpdate>,
    progress_rx: std_mpsc::Receiver<ServerUpdate>,
    capability_support: CapabilitySupport,
    semantic_refresh: Arc<AtomicBool>,
    inlay_refresh: Arc<AtomicBool>,
    diagnostic_refresh: Arc<AtomicBool>,
    /// Set by the worker when it restarted a language server that exited
    /// (#694); see [`LspManager::take_servers_restarted`].
    servers_restarted: Arc<AtomicBool>,
    next_request_id: u64,
    /// Read by the app tests, to prove the servers follow a workspace change.
    #[cfg_attr(not(test), allow(dead_code))]
    workspace_root: PathBuf,
    _runtime: LspRuntime,
}

impl Drop for LspManager {
    fn drop(&mut self) {
        // Ask the worker to gracefully shut every server down, and block until
        // it acks. We run on the app's main thread (not inside the LSP runtime),
        // so block_on the runtime handle is valid. An overall cap keeps an
        // unresponsive server from hanging app exit; afterwards the fields drop
        // — `cmd_tx` closes the worker loop and `_runtime` joins the thread.
        let (done_tx, done_rx) = oneshot::channel();
        if self.cmd_tx.send(Cmd::Shutdown { done: done_tx }).is_ok() {
            let _ = self._runtime.handle().block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await
            });
        }
    }
}

/// Where a document ends, as an LSP position: the last line's index and its
/// length in UTF-16 units. A text ending in `\n` ends at the start of the
/// empty line after it. This is the end of a whole-document request range;
/// strict servers such as rust-analyzer reject any later end (#1490).
pub fn document_end(text: &str) -> Position {
    let line = text.matches('\n').count() as u32;
    let last = text.rsplit('\n').next().unwrap_or("");
    Position::new(line, last.encode_utf16().count() as u32)
}

/// The end of a range over a document's rows up to `end_row` (exclusive):
/// the start of that row, or the document's end when the range reaches it,
/// since a row past the last one is out of range (#1490).
pub fn rows_end(text: &str, end_row: u32) -> Position {
    let end = document_end(text);
    if end_row <= end.line {
        Position::new(end_row, 0)
    } else {
        end
    }
}

impl LspManager {
    pub fn new(workspace_root: PathBuf) -> Result<Self> {
        let runtime = LspRuntime::new()?;
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let (completion_tx, completion_rx) = std_mpsc::channel();
        let (signature_help_tx, signature_help_rx) = std_mpsc::channel();
        let (hover_tx, hover_rx) = std_mpsc::channel();
        #[cfg(test)]
        let hover_test_tx = hover_tx.clone();
        let (rust_analyzer_tx, rust_analyzer_rx) = std_mpsc::channel();
        let (def_tx, def_rx) = std_mpsc::channel();
        let (doc_symbols_tx, doc_symbols_rx) = std_mpsc::channel();
        let (decl_tx, decl_rx) = std_mpsc::channel();
        let (type_def_tx, type_def_rx) = std_mpsc::channel();
        #[cfg(test)]
        let jump_test_tx = (def_tx.clone(), decl_tx.clone(), type_def_tx.clone());
        let (impl_tx, impl_rx) = std_mpsc::channel();
        let (ref_tx, ref_rx) = std_mpsc::channel();
        let (doc_highlights_tx, doc_highlights_rx) = std_mpsc::channel();
        let (linked_editing_tx, linked_editing_rx) = std_mpsc::channel();
        #[cfg(test)]
        let linked_editing_test_tx = linked_editing_tx.clone();
        let (inline_completion_tx, inline_completion_rx) = std_mpsc::channel();
        #[cfg(test)]
        let inline_completion_test_tx = inline_completion_tx.clone();
        let (selection_ranges_tx, selection_ranges_rx) = std_mpsc::channel();
        let (calls_tx, calls_rx) = std_mpsc::channel();
        let (ws_symbols_tx, ws_symbols_rx) = std_mpsc::channel();
        let (rename_tx, rename_rx) = std_mpsc::channel();
        let (prepare_rename_tx, prepare_rename_rx) = std_mpsc::channel();
        let (will_rename_files_tx, will_rename_files_rx) = std_mpsc::channel();
        let (format_tx, format_rx) = std_mpsc::channel();
        let (on_type_tx, on_type_rx) = std_mpsc::channel();
        let (code_action_tx, code_action_rx) = std_mpsc::channel();
        let (semantic_tx, semantic_rx) = std_mpsc::channel();
        let (inlay_tx, inlay_rx) = std_mpsc::channel();
        let (links_tx, links_rx) = std_mpsc::channel();
        let (code_lens_tx, code_lens_rx) = std_mpsc::channel();
        let (folding_tx, folding_rx) = std_mpsc::channel();
        let (colors_tx, colors_rx) = std_mpsc::channel();
        let (color_presentations_tx, color_presentations_rx) = std_mpsc::channel();
        let (diagnostics_tx, diagnostics_rx) = std_mpsc::channel();
        #[cfg(test)]
        let diagnostics_test_tx = diagnostics_tx.clone();
        let (progress_tx, progress_rx) = std_mpsc::channel();
        let capability_support: CapabilitySupport =
            Arc::new(StdMutex::new(LangCapabilitySupport::default()));
        let semantic_refresh = Arc::new(AtomicBool::new(false));
        let inlay_refresh = Arc::new(AtomicBool::new(false));
        let diagnostic_refresh = Arc::new(AtomicBool::new(false));
        let servers_restarted = Arc::new(AtomicBool::new(false));
        let root = workspace_root.clone();
        // Load user-installed extensions (`~/.config/croft/extensions`) and merge
        // them with the bundled ones. The language table must be initialised
        // before the first file open resolves a language, and the registry must
        // see the same user servers, so both are built from one read here.
        let user_sources = crate::lsp::manifest::read_extension_sources(
            &crate::lsp::manifest::user_extensions_dir(),
        );
        let user_refs: Vec<&str> = user_sources.iter().map(String::as_str).collect();
        // The language table keeps every language identity (so file detection and
        // highlighting stay intact); only server registration honors the disable
        // set, so a disabled LSP extension stops spawning without losing its
        // language. The set is read from prefs at construction; a live add,
        // removal or toggle reaches a running manager through
        // `reload_extensions`.
        crate::lsp::languages::reload_with_user_sources(&user_refs);
        let disabled = crate::prefs::Prefs::load_or_default().disabled_extensions;
        let registry = ServerRegistry::with_user_extensions_filtered(&user_refs, &disabled);
        let registered_servers = registered_servers_of(&registry);
        runtime.handle().spawn(worker_loop(
            root,
            registry,
            cmd_rx,
            ResultSenders {
                completion: completion_tx,
                signature_help: signature_help_tx,
                hover: hover_tx,
                rust_analyzer: rust_analyzer_tx,
                definition: def_tx,
                document_symbols: doc_symbols_tx,
                declaration: decl_tx,
                type_definition: type_def_tx,
                implementation: impl_tx,
                references: ref_tx,
                document_highlights: doc_highlights_tx,
                linked_editing: linked_editing_tx,
                inline_completion: inline_completion_tx,
                selection_ranges: selection_ranges_tx,
                call_hierarchy: calls_tx,
                workspace_symbols: ws_symbols_tx,
                rename: rename_tx,
                prepare_rename: prepare_rename_tx,
                will_rename_files: will_rename_files_tx,
                formatting: format_tx,
                on_type_formatting: on_type_tx,
                code_action: code_action_tx,
                semantic_tokens: semantic_tx,
                inlay_hints: inlay_tx,
                document_links: links_tx,
                code_lens: code_lens_tx,
                folding_ranges: folding_tx,
                document_colors: colors_tx,
                color_presentations: color_presentations_tx,
                diagnostics: diagnostics_tx,
                progress: progress_tx,
            },
            capability_support.clone(),
            semantic_refresh.clone(),
            inlay_refresh.clone(),
            diagnostic_refresh.clone(),
            servers_restarted.clone(),
        ));
        Ok(Self {
            registered_servers,
            semantic_generation: std::sync::atomic::AtomicU64::new(0),
            cmd_tx,
            completion_rx,
            signature_help_rx,
            hover_rx,
            #[cfg(test)]
            hover_test_tx,
            rust_analyzer_rx,
            def_rx,
            #[cfg(test)]
            jump_test_tx,
            doc_symbols_rx,
            decl_rx,
            type_def_rx,
            impl_rx,
            ref_rx,
            doc_highlights_rx,
            linked_editing_rx,
            #[cfg(test)]
            linked_editing_test_tx,
            inline_completion_rx,
            #[cfg(test)]
            inline_completion_test_tx,
            #[cfg(test)]
            synced_texts: Default::default(),
            selection_ranges_rx,
            calls_rx,
            ws_symbols_rx,
            rename_rx,
            prepare_rename_rx,
            will_rename_files_rx,
            format_rx,
            on_type_rx,
            code_action_rx,
            semantic_rx,
            inlay_rx,
            links_rx,
            code_lens_rx,
            folding_rx,
            colors_rx,
            color_presentations_rx,
            diagnostics_rx,
            #[cfg(test)]
            diagnostics_test_tx,
            progress_rx,
            capability_support,
            semantic_refresh,
            inlay_refresh,
            diagnostic_refresh,
            servers_restarted,
            next_request_id: 1,
            workspace_root,
            _runtime: runtime,
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn open_doc(&self, path: PathBuf, text: String) {
        #[cfg(test)]
        self.record_synced_text(&path, &text);
        let _ = self.cmd_tx.send(Cmd::OpenDoc { path, text });
    }

    /// Pre-warm the workspace's evident language servers (VS Code's
    /// `workspaceContains` activation). Inert under test: the suite builds
    /// hundreds of Apps in marker-bearing tempdirs and must never spawn a
    /// real rust-analyzer as a construction side effect.
    pub fn prewarm_workspace(&self) {
        if cfg!(test) {
            return;
        }
        let _ = self.cmd_tx.send(Cmd::PrewarmWorkspace);
    }

    /// Replace the secondary workspace-root set (multi-root, #147). Inert
    /// under `cfg(test)` like `prewarm_workspace`, for the same reason:
    /// test-built Apps must never spawn real servers.
    pub fn set_extra_roots(&self, roots: Vec<PathBuf>) {
        if cfg!(test) {
            return;
        }
        let _ = self.cmd_tx.send(Cmd::SetExtraRoots { roots });
    }

    pub fn change_doc(&self, path: PathBuf, text: String) {
        #[cfg(test)]
        self.record_synced_text(&path, &text);
        let _ = self.cmd_tx.send(Cmd::ChangeDoc { path, text });
    }

    /// Notify the servers a document was written to disk. rust-analyzer only
    /// re-runs its check-on-save (`cargo check`, the source of most Rust
    /// PROBLEMS entries) on `textDocument/didSave`.
    ///
    /// `text` is the buffer as saved, in the form `change_doc` sends it. Only
    /// a server whose `save` options ask for `includeText` receives it, and
    /// only a server that asked for saves at all is notified (#854).
    pub fn save_doc(&self, path: PathBuf, text: String) {
        let _ = self.cmd_tx.send(Cmd::SaveDoc { path, text });
    }

    pub fn close_doc(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(Cmd::CloseDoc { path });
    }

    /// Restart the servers serving `path` (#1621): the user's "Language
    /// Servers: Restart for This File". The app re-opens its tabs when they
    /// are back, as after a crash restart.
    pub fn restart_servers_for(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(Cmd::RestartServersFor { path });
    }

    /// Follow an extension added, removed, turned on or off (#1842):
    /// `user_sources` are the installed manifests now and `disabled` the
    /// disable set. Their languages resolve at once, and when that changes
    /// which servers a language registers, the worker swaps the new set in:
    /// that language's running servers stop, and the app re-opens its tabs,
    /// so the next of its files starts what is registered now. Every other
    /// language's servers keep running. Returns the servers no language
    /// registers any more.
    pub fn reload_extensions(
        &mut self,
        user_sources: &[&str],
        disabled: &std::collections::BTreeSet<String>,
    ) -> Vec<&'static str> {
        crate::lsp::languages::reload_with_user_sources(user_sources);
        let registry = ServerRegistry::with_user_extensions_filtered(user_sources, disabled);
        let registered = registered_servers_of(&registry);
        if registered == self.registered_servers {
            return Vec::new();
        }
        let kept: std::collections::HashSet<&str> =
            registered.values().flatten().copied().collect();
        let mut gone: Vec<&'static str> = self
            .registered_servers
            .values()
            .flatten()
            .copied()
            .filter(|name| !kept.contains(name))
            .collect();
        gone.sort_unstable();
        gone.dedup();
        self.registered_servers = registered;
        let _ = self.cmd_tx.send(Cmd::ReplaceRegistry { registry });
        gone
    }

    /// Allocate the next request id, send the command `make` builds around
    /// it, and return the id the reply will echo.
    fn send_request(&mut self, make: impl FnOnce(u64) -> Cmd) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(make(id));
        id
    }

    /// Pop the next server progress update or `window/*` message, if any.
    /// The app keeps a per-server progress map so the status bar can show or
    /// clear "Indexing…", and shows the messages.
    pub fn drain_server_update(&self) -> Option<ServerUpdate> {
        self.progress_rx.try_recv().ok()
    }

    /// Returns and clears the "a language server was restarted" flag (#694).
    /// The restarted server knows no documents, so the app answers by
    /// re-opening every tab against it.
    pub fn take_servers_restarted(&self) -> bool {
        self.servers_restarted.swap(false, Ordering::Relaxed)
    }

    /// Whether the server named `name` is registered for `lang`, i.e. no
    /// disabled extension removed it (#1511).
    pub fn server_registered(&self, lang: Language, name: &str) -> bool {
        self.registered_servers
            .get(&lang)
            .is_some_and(|names| names.contains(&name))
    }

    /// Test hook: replace the servers registered for `lang`, as a disabled
    /// or added extension would.
    #[cfg(test)]
    pub fn set_registered_servers_for_test(&mut self, lang: Language, names: &[&'static str]) {
        self.registered_servers.insert(lang, names.to_vec());
    }

    /// Whether the server named `name` is open on the document `path`
    /// (#1626), whether or not it still reports diagnostics on it.
    pub fn document_served_by(&self, path: &Path, name: &str) -> bool {
        self.capability_support.lock().is_ok_and(|s| {
            s.document_clients
                .get(path)
                .is_some_and(|names| names.iter().any(|n| n == name))
        })
    }

    /// Whether any running server has the open document `path` (#1566):
    /// one that does answers its completions, and word completion waits for
    /// that answer instead of opening on the document's words at once.
    pub fn document_has_server(&self, path: &Path) -> bool {
        self.capability_support.lock().is_ok_and(|s| {
            s.document_servers
                .get(path)
                .is_some_and(|names| !names.is_empty())
        })
    }

    /// Test hook: record `names` as the servers reporting on `path`, as an
    /// open would, without starting any.
    #[cfg(test)]
    pub fn set_document_servers_for_test(&self, path: &Path, names: &[&str]) {
        if let Ok(mut support) = self.capability_support.lock() {
            support.document_servers.insert(
                path.to_path_buf(),
                names.iter().map(|n| n.to_string()).collect(),
            );
        }
    }

    /// Test hook: record `names` as the servers open on `path` (#1626),
    /// without starting any or marking them as reporting on it.
    #[cfg(test)]
    pub fn set_document_clients_for_test(&self, path: &Path, names: &[&str]) {
        if let Ok(mut support) = self.capability_support.lock() {
            support.document_clients.insert(
                path.to_path_buf(),
                names.iter().map(|n| n.to_string()).collect(),
            );
        }
    }

    /// Test hook: the text the last open or change sent for `path`, as
    /// every server on it would read it.
    #[cfg(test)]
    pub fn synced_text_for_test(&self, path: &Path) -> Option<String> {
        self.synced_texts.lock().ok()?.get(path).cloned()
    }

    #[cfg(test)]
    fn record_synced_text(&self, path: &Path, text: &str) {
        if let Ok(mut texts) = self.synced_texts.lock() {
            texts.insert(path.to_path_buf(), text.to_string());
        }
    }

    /// Tell the servers that watch them that files changed on disk (#908).
    /// A file gone by the time this is sent goes out as deleted.
    pub fn did_change_watched_files(&mut self, changes: Vec<(PathBuf, lsp_types::FileChangeType)>) {
        let _ = self.cmd_tx.send(Cmd::DidChangeWatchedFiles { changes });
    }

    /// What the servers for `lang` reported in the per-language capability
    /// map `pick` names. `None` means no server has reported yet.
    fn language_supports(
        &self,
        lang: Language,
        pick: fn(&LangCapabilitySupport) -> &HashMap<Language, bool>,
    ) -> Option<bool> {
        pick(&*self.capability_support.lock().ok()?)
            .get(&lang)
            .copied()
    }
}

struct ManagedClient {
    name: String,
    client: Arc<TokioMutex<LspClient>>,
    supports_completion: bool,
    /// Its completion `triggerCharacters` (#929), as one string.
    completion_triggers: String,
    supports_signature_help: bool,
    /// Whether the server answers `workspace/diagnostic` (#533), read from
    /// the capability's inner `workspaceDiagnostics` flag rather than the
    /// outer `Option` - see `workspace_diagnostics_supported`.
    ///
    /// Written but not yet read: the request slice filters on it. Verified
    /// not to warn - a forced non-test rebuild under `-D warnings` with no
    /// `allow` is clean, because the field is written in a real constructor.
    supports_workspace_diagnostics: bool,
    /// The server's own `diagnosticProvider.identifier`, echoed back on every
    /// pull (#533). A server that registered more than one diagnostic source
    /// uses it to tell which one a previous result id belongs to, so sending
    /// `None` where it published an identifier throws its incrementality away.
    diagnostic_identifier: Option<String>,
    /// Result ids from this server's last pull (#533), behind the same async
    /// mutex the client is: a pull runs off the worker loop, so the cache has
    /// to outlive the borrow that started it.
    diagnostic_result_ids: Arc<TokioMutex<DiagnosticResultIds>>,
    /// Set once this server has let a `workspace/diagnostic` request expire,
    /// after which it is never pulled again for the life of the client.
    ///
    /// A request that times out cannot be cancelled: async-lsp assigns the id
    /// and parks the response sender in the mainloop's `outgoing` map, which
    /// is drained only by a response ARRIVING with that id (async-lsp 0.2.4
    /// `lib.rs:604`). A server that never answers therefore leaves one entry
    /// behind per request, forever. Since `workspace/diagnostic/refresh` is
    /// server-driven and unrated, a server that refreshes and then stalls
    /// would accrue one leak per refresh for the whole session. Asking it
    /// once and then stopping bounds the damage at exactly one.
    diagnostic_pull_stalled: Arc<AtomicBool>,
    /// Whether to send this server `textDocument/didSave`, and with the text
    /// or without it, read from its `textDocumentSync` (#854).
    save_notify: SaveNotify,
    /// Whether the server answers `textDocument/diagnostic` (#866), which
    /// advertising `diagnosticProvider` at all promises. croft declares the
    /// pull capability for the workspace pull (#533), and servers such as
    /// rumdl and the VS Code JSON, CSS and HTML servers then stop pushing,
    /// so for them a document pull is the only way diagnostics arrive.
    supports_document_diagnostics: bool,
    /// Set once this server lets a document pull expire; it is not asked
    /// again, for the reason `diagnostic_pull_stalled` gives.
    document_pull_stalled: Arc<AtomicBool>,
    /// Set once this server lets a prepareRename expire (#1677); later
    /// renames go straight to the word-under-cursor prompt, for the reason
    /// `diagnostic_pull_stalled` gives: an abandoned request leaks a
    /// mainloop entry, so one leak per client, not one per F2.
    prepare_rename_stalled: Arc<AtomicBool>,
    /// The newest pull generation per document (#866). A pull an edit has
    /// overtaken neither goes out nor, answered late, lands over a newer one.
    document_pull_generations: Arc<StdMutex<HashMap<PathBuf, u64>>>,
    supports_hover: bool,
    supports_definition: bool,
    supports_document_symbol: bool,
    supports_declaration: bool,
    supports_type_definition: bool,
    supports_implementation: bool,
    supports_references: bool,
    supports_document_highlight: bool,
    supports_linked_editing: bool,
    /// Offers `textDocument/inlineCompletion` (#607).
    supports_inline_completion: bool,
    supports_selection_range: bool,
    supports_call_hierarchy: bool,
    supports_type_hierarchy: bool,
    supports_workspace_symbols: bool,
    supports_rename: bool,
    supports_prepare_rename: bool,
    supports_formatting: bool,
    /// The server's on-type formatting trigger characters, when it
    /// advertises the provider (#254).
    on_type_triggers: Option<String>,
    supports_range_formatting: bool,
    supports_code_action: bool,
    /// The server's semantic-token legend (token-type names by index),
    /// captured at spawn. `None` when the server advertises no
    /// `semanticTokensProvider` with full-document support.
    semantic_legend: Option<Arc<Vec<String>>>,
    /// Whether the server's `semanticTokensProvider` also advertises range
    /// support. Used to prefer a purpose-built, incremental highlighter (e.g.
    /// ty, which answers in tens of ms even on a huge cold workspace and
    /// supports range) over one that only does full-document tokens after a
    /// slow whole-tree enumeration (e.g. basedpyright). See
    /// `request_semantic_tokens`.
    semantic_supports_range: bool,
    /// Whether the server advertises an `inlayHintProvider`
    /// (rust-analyzer, vtsls, gopls do; ruff does not).
    supports_inlay_hints: bool,
    supports_document_link: bool,
    /// `codeLensProvider` (#608), and whether it resolves lenses lazily.
    supports_code_lens: bool,
    code_lens_resolve: bool,
    /// The commands the server runs itself (`executeCommandProvider`).
    server_commands: Arc<Vec<String>>,
    supports_folding_range: bool,
    supports_document_color: bool,
    /// The paths this server wants `willRenameFiles` / `didRenameFiles`
    /// for (#610); `None` when it does not advertise the operation.
    will_rename: Option<Arc<crate::lsp::file_ops::FileOpFilters>>,
    did_rename: Option<Arc<crate::lsp::file_ops::FileOpFilters>>,
}

/// Servers are keyed by language AND the file's project root, not language
/// alone. A monorepo has one `.venv` (and `pyproject.toml`) per sub-project, so
/// a single server rooted at croft's workspace root can't resolve any of them.
/// Following Zed's model, croft runs a server instance per (language, project
/// root); each is rooted at the project so basedpyright/rust-analyzer/gopls
/// auto-detect that project's environment.
type ClientKey = (Language, PathBuf);

/// Why [`WorkerState::pick_server`] found no server to ask.
enum NoServer {
    /// The file is not open, or no server set exists for its language and
    /// root: the request is dropped without a reply (a hover gets an empty
    /// one).
    Unknown,
    /// Servers run for the file, but none carries the method.
    Unsupported,
}

struct WorkerState {
    workspace_root: PathBuf,
    /// Secondary workspace roots (multi-root, #147). Servers stay one
    /// instance per (language, project root); these only widen the bound
    /// `project_root_for_roots` walks within, so a file under a secondary root
    /// spawns its servers AT that root instead of falling back to the
    /// primary — the per-root-instance model every server supports.
    extra_roots: Vec<PathBuf>,
    registry: ServerRegistry,
    clients: HashMap<ClientKey, Vec<ManagedClient>>,
    docs: HashMap<PathBuf, DocState>,
    capability_support: CapabilitySupport,
    // Shared with every spawned client's router; a client sets it on a
    // `workspace/semanticTokens/refresh` and the app polls + clears it.
    semantic_refresh: Arc<AtomicBool>,
    // Same contract for `workspace/inlayHint/refresh`.
    inlay_refresh: Arc<AtomicBool>,
    // Same contract for `workspace/diagnostic/refresh` (#533); the app polls
    // it and asks for a fresh workspace pull.
    diagnostic_refresh: Arc<AtomicBool>,
    // Cloned into every spawned client's router so the server-pushed
    // `textDocument/publishDiagnostics` notifications reach the app.
    diagnostics_tx: std_mpsc::Sender<DiagnosticsUpdate>,
    // Cloned into every spawned client's router so server `$/progress`
    // notifications (rust-analyzer indexing, etc.) reach the status bar.
    progress_tx: std_mpsc::Sender<ServerUpdate>,
    restarts: ServerRestarts,
}

/// How often a (language, root)'s servers may be restarted after exiting,
/// and within what window (#694). A server that dies on every start would
/// otherwise respawn on every keystroke; past the limit croft leaves it
/// down and says so, as it did before restarts existed.
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Restart bookkeeping for servers that exited under a live workspace.
#[derive(Default)]
struct ServerRestarts {
    /// When each key's servers were last restarted, newest last.
    attempts: HashMap<ClientKey, Vec<std::time::Instant>>,
    /// Keys whose servers used up [`MAX_RESTARTS`]: left down until the
    /// workspace re-roots or croft restarts, so a crash loop cannot spin.
    gave_up: std::collections::HashSet<ClientKey>,
    /// Keys whose restart lost a server that ran before it, with the names
    /// of the servers that ran. The next pass restarts them as a set again: a
    /// non-empty list is never re-probed, and a lone missing server cannot be
    /// added to documents the others hold.
    incomplete: HashMap<ClientKey, Vec<String>>,
    /// Set by [`WorkerState::restart_dead_servers`] for the respawn it has
    /// already counted, so `ensure_clients` does not count it again: the
    /// servers that ran before it, which the respawn must bring back.
    respawn: Option<Vec<String>>,
    /// Raised after a restart; the app polls it to re-open its tabs.
    restarted: Arc<AtomicBool>,
}

impl ServerRestarts {
    fn reporting_to(restarted: Arc<AtomicBool>) -> Self {
        Self {
            restarted,
            ..Self::default()
        }
    }

    /// Record a restart of `key` at `now` if the budget allows one.
    fn try_restart(&mut self, key: &ClientKey, now: std::time::Instant) -> bool {
        let attempts = self.attempts.entry(key.clone()).or_default();
        attempts.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
        if attempts.len() >= MAX_RESTARTS {
            self.gave_up.insert(key.clone());
            return false;
        }
        attempts.push(now);
        true
    }
}

struct DocState {
    language: Language,
    project_root: PathBuf,
    version: i32,
}

/// The reply channels the worker sends results back on, bundled so
/// `worker_loop` keeps a small argument list as request kinds grow.
struct ResultSenders {
    completion: std_mpsc::Sender<CompletionResult>,
    signature_help: std_mpsc::Sender<SignatureHelpResult>,
    hover: std_mpsc::Sender<HoverResult>,
    rust_analyzer: std_mpsc::Sender<RustAnalyzerResult>,
    definition: std_mpsc::Sender<DefinitionResult>,
    document_symbols: std_mpsc::Sender<DocumentSymbolsResult>,
    declaration: std_mpsc::Sender<DeclarationResult>,
    type_definition: std_mpsc::Sender<TypeDefinitionResult>,
    implementation: std_mpsc::Sender<ImplementationResult>,
    references: std_mpsc::Sender<ReferencesResult>,
    document_highlights: std_mpsc::Sender<DocumentHighlightsResult>,
    linked_editing: std_mpsc::Sender<LinkedEditingResult>,
    inline_completion: std_mpsc::Sender<InlineCompletionResult>,
    selection_ranges: std_mpsc::Sender<SelectionRangesResult>,
    call_hierarchy: std_mpsc::Sender<CallHierarchyResult>,
    workspace_symbols: std_mpsc::Sender<WorkspaceSymbolsResult>,
    rename: std_mpsc::Sender<RenameResult>,
    prepare_rename: std_mpsc::Sender<PrepareRenameResult>,
    will_rename_files: std_mpsc::Sender<WillRenameFilesResult>,
    formatting: std_mpsc::Sender<FormatResult>,
    on_type_formatting: std_mpsc::Sender<FormatResult>,
    code_action: std_mpsc::Sender<CodeActionResult>,
    semantic_tokens: std_mpsc::Sender<SemanticTokensUpdate>,
    inlay_hints: std_mpsc::Sender<InlayHintsUpdate>,
    document_links: std_mpsc::Sender<DocumentLinksUpdate>,
    code_lens: std_mpsc::Sender<CodeLensUpdate>,
    folding_ranges: std_mpsc::Sender<FoldingRangesUpdate>,
    document_colors: std_mpsc::Sender<DocumentColorsUpdate>,
    color_presentations: std_mpsc::Sender<ColorPresentationsResult>,
    diagnostics: std_mpsc::Sender<DiagnosticsUpdate>,
    progress: std_mpsc::Sender<ServerUpdate>,
}

// Every flag and channel here is an independent worker input, the same shape
// `LspClient::spawn` carries: bundling them behind one struct would add a
// layer of indirection without making any call site clearer. `ResultSenders`
// already bundles the part that grows per REQUEST KIND; these do not grow
// that way.
#[allow(clippy::too_many_arguments)]
async fn worker_loop(
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

impl WorkerState {
    /// Gracefully shut down every managed client concurrently. Each client's
    /// `shutdown` self-bounds its child-wait, so a single unresponsive server
    /// can't block the others. Clients are left in the map; their eventual Drop
    /// aborts the (now-finished) MainLoop and reaps the (now-exited) child.
    async fn shutdown_all(&mut self) {
        let clients: Vec<Arc<TokioMutex<LspClient>>> = self
            .clients
            .values()
            .flatten()
            .map(|m| m.client.clone())
            .collect();
        let mut tasks = Vec::with_capacity(clients.len());
        for client in clients {
            tasks.push(tokio::spawn(async move {
                let mut client = client.lock().await;
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), client.shutdown())
                    .await;
            }));
        }
        for t in tasks {
            let _ = t.await;
        }
    }

    /// Restart the servers of every (language, root) with a server that has
    /// exited (#694): killed by the OOM killer, which croft asks to pick
    /// language servers first, or crashed. Their documents are forgotten and
    /// the app is told to re-open its tabs, since a new server knows none of
    /// them. Past [`MAX_RESTARTS`] in [`RESTART_WINDOW`] the dead servers are
    /// dropped and left down instead.
    async fn restart_dead_servers(&mut self) {
        let dead: Vec<ClientKey> = self
            .clients
            .iter()
            .filter(|(key, clients)| {
                !self.restarts.gave_up.contains(*key)
                    && (self.restarts.incomplete.contains_key(*key)
                        || clients.iter().any(|managed| {
                            // A client in use is not a dead one; skip it rather
                            // than wait on the lock.
                            managed
                                .client
                                .try_lock()
                                .is_ok_and(|client| client.has_exited())
                        }))
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in dead {
            let Some(clients) = self.clients.remove(&key) else {
                continue;
            };
            let names: Vec<&str> = clients.iter().map(|c| c.name.as_str()).collect();
            // A server that never came up is not one the restart lost; a set
            // still short from the last restart keeps what it is missing.
            let expected = self
                .restarts
                .incomplete
                .get(&key)
                .cloned()
                .unwrap_or_else(|| names.iter().map(|n| n.to_string()).collect());
            if !self.restarts.try_restart(&key, std::time::Instant::now()) {
                let msg = format!(
                    "{} stopped {MAX_RESTARTS} times in 10 minutes; leaving it off until croft restarts or the workspace reopens",
                    names.join(", ")
                );
                log_file::log(&format!("lsp restart: {msg}"));
                crate::output::push("Language Servers", crate::output::OutputLevel::Error, &msg);
                // Keep the survivors; `gave_up` stops both this pass and the
                // empty-list re-probe from spawning the dead ones again.
                let alive: Vec<ManagedClient> = clients
                    .into_iter()
                    .filter(|m| m.client.try_lock().map_or(true, |c| !c.has_exited()))
                    .collect();
                // The dead ones report on nothing any more.
                if let Ok(mut support) = self.capability_support.lock() {
                    for (path, doc) in &self.docs {
                        if (doc.language, doc.project_root.clone()) != key {
                            continue;
                        }
                        if let Some(names) = support.document_servers.get_mut(path) {
                            names.retain(|n| alive.iter().any(|m| &m.name == n));
                        }
                        if let Some(names) = support.document_clients.get_mut(path) {
                            names.retain(|n| alive.iter().any(|m| &m.name == n));
                        }
                    }
                }
                self.restarts.incomplete.remove(&key);
                self.clients.insert(key, alive);
                continue;
            }
            let msg = format!("{} stopped; restarting", names.join(", "));
            log_file::log(&format!("lsp restart: {msg} for {}", key.1.display()));
            crate::output::push("Language Servers", crate::output::OutputLevel::Info, &msg);
            // The survivors go too: servers for one key are spawned and
            // opened as a set, and a half-restarted set would hold documents
            // the new server never saw.
            for managed in clients {
                let mut client = managed.client.lock().await;
                if !client.has_exited() {
                    let _ =
                        tokio::time::timeout(std::time::Duration::from_secs(3), client.shutdown())
                            .await;
                }
            }
            if let Ok(mut support) = self.capability_support.lock() {
                for (path, doc) in &self.docs {
                    if (doc.language, doc.project_root.clone()) == key {
                        support.document_servers.remove(path);
                        support.document_clients.remove(path);
                    }
                }
            }
            self.docs
                .retain(|_, doc| (doc.language, doc.project_root.clone()) != key);
            // Respawn as a re-probe, not a first attempt, so a set that comes
            // back short is marked incomplete and tried again.
            self.clients.insert(key.clone(), Vec::new());
            self.restarts.respawn = Some(expected);
            self.ensure_clients(key.0, &key.1).await;
            self.restarts.restarted.store(true, Ordering::Relaxed);
        }
    }

    /// Restart the servers serving `path` at the user's request (#1621): the
    /// (language, root) its document was opened under, else the one its
    /// language and project root would open it under, so the servers of
    /// other roots keep running. Unlike a crash restart this spends no
    /// budget and lifts a give-up, since the user asked for it.
    async fn restart_servers_for(&mut self, path: &Path) {
        let keys: Vec<ClientKey> = match self.docs.get(path) {
            Some(doc) => vec![(doc.language, doc.project_root.clone())],
            None => path_to_language(path)
                .map(|lang| {
                    (
                        lang,
                        project_root_for_roots(path, lang, &self.workspace_root, &self.extra_roots),
                    )
                })
                .filter(|key| self.clients.contains_key(key))
                .into_iter()
                .collect(),
        };
        if keys.is_empty() {
            crate::output::push(
                "Language Servers",
                crate::output::OutputLevel::Info,
                &format!("No language server serves {}", path.display()),
            );
            return;
        }
        for key in keys {
            self.restart_key(key).await;
        }
        self.restarts.restarted.store(true, Ordering::Relaxed);
    }

    /// Shut the servers of `key` down and start them again, forgetting its
    /// documents, which the new servers never saw.
    async fn restart_key(&mut self, key: ClientKey) {
        let names = self.retire_key(&key).await;
        let expected = self.restarts.incomplete.remove(&key).unwrap_or(names);
        self.clients.insert(key.clone(), Vec::new());
        if !expected.is_empty() {
            self.restarts.respawn = Some(expected.clone());
        }
        self.ensure_clients(key.0, &key.1).await;
        let msg = if expected.is_empty() {
            format!("Started the language servers for {}", key.1.display())
        } else {
            format!("Restarted {} for {}", expected.join(", "), key.1.display())
        };
        log_file::log(&format!("lsp restart: {msg}"));
        crate::output::push("Language Servers", crate::output::OutputLevel::Info, &msg);
    }

    /// Shut the servers of `key` down and forget them, its documents and
    /// its restart budget, returning the names of the servers that ran.
    async fn retire_key(&mut self, key: &ClientKey) -> Vec<String> {
        let clients = self.clients.remove(key).unwrap_or_default();
        let names: Vec<String> = clients.iter().map(|c| c.name.clone()).collect();
        // Withdraw what the old servers published or answered to a pull
        // (#1102) for their documents: a server that does not come back
        // would never clear it, and one that does reports again once the
        // app re-opens the tabs.
        for (path, doc) in &self.docs {
            if (doc.language, doc.project_root.clone()) != *key {
                continue;
            }
            for name in &names {
                for pulled in [false, true] {
                    let _ = self.diagnostics_tx.send(DiagnosticsUpdate {
                        path: path.clone(),
                        server: name.clone(),
                        diagnostics: Vec::new(),
                        pulled,
                    });
                }
            }
        }
        for managed in clients {
            let mut client = managed.client.lock().await;
            if !client.has_exited() {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), client.shutdown())
                    .await;
            }
        }
        // Nothing serves its documents now, as after a crash; whatever
        // starts next says what it serves.
        if let Ok(mut support) = self.capability_support.lock() {
            for (path, doc) in &self.docs {
                if (doc.language, doc.project_root.clone()) == *key {
                    support.document_servers.remove(path);
                    support.document_clients.remove(path);
                }
            }
            support.completion_triggers.remove(key);
            support.inline_completion.remove(key);
        }
        self.docs
            .retain(|_, doc| (doc.language, doc.project_root.clone()) != *key);
        self.restarts.gave_up.remove(key);
        self.restarts.attempts.remove(key);
        names
    }

    /// Serve from `registry` from now on (#1842). A (language, root) whose
    /// servers it changes has the running ones shut down and is forgotten,
    /// so the next of its files starts what is registered now, as a first
    /// open does; the app re-opens its tabs for that. Every other key's
    /// servers keep running.
    async fn replace_registry(&mut self, registry: ServerRegistry) {
        let old = std::mem::replace(&mut self.registry, registry);
        // A file an added or removed extension maps to another language or
        // project root now leaves the servers of its old one, so the app's
        // re-open reaches the new one's as an open, not as a change.
        let moved: Vec<PathBuf> = self
            .docs
            .iter()
            .filter(|(path, doc)| {
                path_to_language(path).map(|lang| {
                    let root =
                        project_root_for_roots(path, lang, &self.workspace_root, &self.extra_roots);
                    (lang, root)
                }) != Some((doc.language, doc.project_root.clone()))
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in moved {
            self.close_doc(path).await;
        }
        let names = |r: &ServerRegistry, lang: Language| -> Vec<&'static str> {
            r.for_language(lang).iter().map(|c| c.name).collect()
        };
        let changed: Vec<ClientKey> = self
            .clients
            .keys()
            .filter(|(lang, _)| names(&old, *lang) != names(&self.registry, *lang))
            .cloned()
            .collect();
        for key in changed {
            let stopped = self.retire_key(&key).await;
            self.restarts.incomplete.remove(&key);
            if !stopped.is_empty() {
                let msg = format!(
                    "Stopped {} for {}: the extensions serving it changed",
                    stopped.join(", "),
                    key.1.display()
                );
                log_file::log(&format!("lsp registry: {msg}"));
                crate::output::push("Language Servers", crate::output::OutputLevel::Info, &msg);
            }
        }
        // Tabs the old registry served nothing for were never opened here,
        // so every tab goes out again, as after a restart.
        self.restarts.restarted.store(true, Ordering::Relaxed);
    }

    async fn ensure_clients(&mut self, lang: Language, root: &Path) -> &[ManagedClient] {
        // Re-probe when the cached list is empty: a server (e.g. the managed
        // vtsls) may have finished its background install since the first
        // attempt, so this is how TS LSP comes alive without a relaunch.
        // A non-empty list is stable and never re-probed.
        let key: ClientKey = (lang, root.to_path_buf());
        let first_attempt = !self.clients.contains_key(&key);
        // A key whose servers crash-looped stays down (#694): the empty-list
        // re-probe below would otherwise respawn them on every open.
        let empty = !first_attempt
            && self
                .clients
                .get(&key)
                .is_some_and(|clients| clients.is_empty());
        let respawn = self.restarts.respawn.take();
        let prepaid = respawn.is_some();
        let mut should_try = !self.restarts.gave_up.contains(&key) && (first_attempt || empty);
        // An empty list left by a restart whose respawn failed is re-probed
        // by every request. Each re-probe is another restart and spends the
        // same budget, so a server that never comes back stops being launched.
        if should_try
            && empty
            && !prepaid
            && self.restarts.attempts.contains_key(&key)
            && !self.restarts.try_restart(&key, std::time::Instant::now())
        {
            let msg = format!(
                "language servers for {} did not come back after {MAX_RESTARTS} restarts in 10 minutes; leaving them off until croft restarts or the workspace reopens",
                key.1.display()
            );
            log_file::log(&format!("lsp restart: {msg}"));
            crate::output::push("Language Servers", crate::output::OutputLevel::Error, &msg);
            should_try = false;
        }
        if should_try {
            // A launch's first frame comes first (see `startup_gate`): a file
            // restored at launch, or a restored extra root, otherwise starts
            // its servers, and the `rustup` that resolves rust-analyzer,
            // while that frame is still being drawn.
            crate::startup_gate::wait_async().await;
            let configs: Vec<ServerConfig> = self.registry.for_language(lang).to_vec();
            // Spawn every server for this root concurrently rather than awaiting
            // each in turn. The worker loop processes commands one at a time, so
            // a sequential spawn chain made the OpenDoc command (and the
            // semantic-token request queued right behind it) block on the SUM of
            // ty + basedpyright + ruff init handshakes. Concurrent spawn bounds
            // the cold-root stall to the SLOWEST single handshake instead.
            let resolved: Vec<(ServerConfig, Vec<PathBuf>)> = futures::future::join_all(
                configs
                    .iter()
                    .map(|config| resolve_for_root(config, root, first_attempt)),
            )
            .await
            .into_iter()
            .flatten()
            .collect();
            if first_attempt
                && resolved.is_empty()
                && let Some(note) =
                    missing_servers_note(lang, &configs, crate::lsp::install::missing_installer)
            {
                report_missing_servers_once(lang, &note);
            }
            let outcomes =
                futures::future::join_all(resolved.into_iter().map(|(config, extra_path)| {
                    let caps = build_client_capabilities();
                    let refresh = self.semantic_refresh.clone();
                    let inlay_refresh = self.inlay_refresh.clone();
                    let diagnostic_refresh = self.diagnostic_refresh.clone();
                    let diagnostics = self.diagnostics_tx.clone();
                    let progress = self.progress_tx.clone();
                    async move {
                        let result = LspClient::spawn(
                            &config,
                            root,
                            caps,
                            &extra_path,
                            refresh,
                            inlay_refresh,
                            diagnostic_refresh,
                            diagnostics,
                            progress,
                        )
                        .await;
                        (config, result)
                    }
                }))
                .await;
            let mut spawned: Vec<ManagedClient> = Vec::new();
            for (config, result) in outcomes {
                match result {
                    Ok(client) => {
                        let caps = client.capabilities();
                        // A capability that can be sent as a bare `false` (the
                        // bool-or-options shapes) must be read as its inner bool:
                        // `Option::is_some` treats `Some(false)` as supported,
                        // which made croft call `textDocument/declaration` on
                        // vtsls (which advertises `declarationProvider: false`)
                        // and get a -32601 "Unhandled method" back.
                        let supports = caps.completion_provider.is_some();
                        let completion_triggers = completion_triggers_of(caps);
                        let supports_signature_help =
                            signature_help_supported(&caps.signature_help_provider);
                        let supports_workspace_diagnostics =
                            workspace_diagnostics_supported(&caps.diagnostic_provider);
                        let supports_document_diagnostics = caps.diagnostic_provider.is_some();
                        let diagnostic_identifier =
                            diagnostic_identifier_of(&caps.diagnostic_provider);
                        let save_notify = save_notify_of(&caps.text_document_sync);
                        let supports_hover = hover_supported(&caps.hover_provider);
                        let supports_definition = one_of_supported(&caps.definition_provider);
                        let supports_document_symbol =
                            one_of_supported(&caps.document_symbol_provider);
                        let supports_declaration =
                            declaration_supported(&caps.declaration_provider);
                        let supports_type_definition =
                            type_definition_supported(&caps.type_definition_provider);
                        let supports_implementation =
                            implementation_supported(&caps.implementation_provider);
                        let supports_references = one_of_supported(&caps.references_provider);
                        let supports_document_highlight =
                            one_of_supported(&caps.document_highlight_provider);
                        let supports_linked_editing =
                            linked_editing_supported(&caps.linked_editing_range_provider);
                        let supports_inline_completion =
                            one_of_supported(&caps.inline_completion_provider);
                        let supports_selection_range =
                            selection_range_supported(&caps.selection_range_provider);
                        let supports_call_hierarchy =
                            call_hierarchy_supported(&caps.call_hierarchy_provider);
                        let supports_type_hierarchy = client.supports_type_hierarchy();
                        let supports_workspace_symbols =
                            one_of_supported(&caps.workspace_symbol_provider);
                        let supports_rename = one_of_supported(&caps.rename_provider);
                        let supports_prepare_rename =
                            prepare_rename_supported(&caps.rename_provider);
                        let supports_formatting =
                            one_of_supported(&caps.document_formatting_provider);
                        let on_type_triggers =
                            caps.document_on_type_formatting_provider.as_ref().map(|o| {
                                let mut t = o.first_trigger_character.clone();
                                for more in o.more_trigger_character.iter().flatten() {
                                    t.push_str(more);
                                }
                                t
                            });
                        let supports_range_formatting =
                            one_of_supported(&caps.document_range_formatting_provider);
                        let supports_code_action =
                            code_action_supported(&caps.code_action_provider);
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
                        let supports_folding_range =
                            folding_range_supported(&caps.folding_range_provider);
                        let supports_document_color =
                            color_provider_supported(&caps.color_provider);
                        let file_ops = caps
                            .workspace
                            .as_ref()
                            .and_then(|w| w.file_operations.as_ref());
                        let compile = |reg: Option<
                            &lsp_types::FileOperationRegistrationOptions,
                        >| {
                            reg.map(|r| {
                                Arc::new(crate::lsp::file_ops::FileOpFilters::from_registration(r))
                            })
                        };
                        let will_rename = compile(file_ops.and_then(|f| f.will_rename.as_ref()));
                        let did_rename = compile(file_ops.and_then(|f| f.did_rename.as_ref()));
                        log_file::log(&format!(
                            "lsp[{}] spawned, root={} supports_completion={supports} supports_signature_help={supports_signature_help} supports_hover={supports_hover} supports_definition={supports_definition} supports_declaration={supports_declaration} supports_type_definition={supports_type_definition} supports_implementation={supports_implementation} supports_references={supports_references} supports_rename={supports_rename}",
                            config.name,
                            root.display()
                        ));
                        spawned.push(ManagedClient {
                            name: config.name.to_string(),
                            client: Arc::new(TokioMutex::new(client)),
                            supports_completion: supports,
                            completion_triggers,
                            supports_signature_help,
                            supports_workspace_diagnostics,
                            diagnostic_identifier,
                            diagnostic_result_ids: Arc::new(TokioMutex::new(
                                DiagnosticResultIds::default(),
                            )),
                            diagnostic_pull_stalled: Arc::new(AtomicBool::new(false)),
                            save_notify,
                            supports_document_diagnostics,
                            document_pull_stalled: Arc::new(AtomicBool::new(false)),
                            prepare_rename_stalled: Arc::new(AtomicBool::new(false)),
                            document_pull_generations: Arc::new(StdMutex::new(HashMap::new())),
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
                        });
                    }
                    Err(e) => {
                        log_file::log(&format!("lsp[{}] spawn failed: {e}", config.name));
                    }
                }
            }
            // Publish declaration / type-definition / implementation support for
            // this language so the app can show or hide the matching "Go to ..."
            // menu items synchronously. Written even when empty (value false) so
            // a missing entry means "not probed yet" rather than "unsupported".
            let supports_declaration = spawned.iter().any(|c| c.supports_declaration);
            let supports_type_definition = spawned.iter().any(|c| c.supports_type_definition);
            let supports_implementation = spawned.iter().any(|c| c.supports_implementation);
            let supports_references = spawned.iter().any(|c| c.supports_references);
            let supports_formatting = spawned.iter().any(|c| c.supports_formatting);
            let supports_code_action = spawned.iter().any(|c| c.supports_code_action);
            let supports_call_hierarchy = spawned.iter().any(|c| c.supports_call_hierarchy);
            if let Ok(mut support) = self.capability_support.lock() {
                support.declaration.insert(lang, supports_declaration);
                support
                    .type_definition
                    .insert(lang, supports_type_definition);
                support.implementation.insert(lang, supports_implementation);
                support.references.insert(lang, supports_references);
                support.formatting.insert(lang, supports_formatting);
                let triggers: String = spawned
                    .iter()
                    .filter_map(|c| c.on_type_triggers.as_deref())
                    .collect();
                // No entry when nothing advertises the provider — a missing
                // entry is the documented "no server carries it" answer.
                // Removed (not skipped) on the empty case so a re-probe of
                // the same language overwrites last-write-wins, like every
                // sibling capability map above.
                if triggers.is_empty() {
                    support.on_type_triggers.remove(&lang);
                } else {
                    support.on_type_triggers.insert(lang, triggers);
                }
                // The server `request_completion` picks: the first that
                // completes.
                match spawned.iter().find(|c| c.supports_completion) {
                    Some(c) => support
                        .completion_triggers
                        .insert(key.clone(), c.completion_triggers.clone()),
                    None => support.completion_triggers.remove(&key),
                };
                support.code_action.insert(lang, supports_code_action);
                support.call_hierarchy.insert(lang, supports_call_hierarchy);
                if spawned.iter().any(|c| c.supports_inline_completion) {
                    support.inline_completion.insert(key.clone());
                } else {
                    support.inline_completion.remove(&key);
                }
                support
                    .will_rename
                    .insert(lang, spawned.iter().any(|c| c.will_rename.is_some()));
            }
            // The first pull (#533). A server that answers workspace
            // diagnostics reports on files no editor has opened, and firing
            // here is what puts them in PROBLEMS without a manual command -
            // there is no other moment that covers a workspace whose files
            // are all still closed. Servers that do not advertise it drop out
            // in `workspace_pull_targets`, so this stays silent for them.
            spawn_workspace_pull(
                workspace_pull_targets(spawned.iter()),
                self.diagnostics_tx.clone(),
            );
            // An empty list that comes back is a restart as far as the app is
            // concerned: it sent its tabs while the list was empty, so these
            // servers have none of them (#694).
            if !first_attempt && !spawned.is_empty() {
                self.restarts.restarted.store(true, Ordering::Relaxed);
            }
            // A restart that lost a server which ran before it is retried as
            // a set by the next `restart_dead_servers` pass. One that never
            // came up is not a reason to restart the others.
            match respawn {
                Some(expected)
                    if !spawned.is_empty()
                        && expected
                            .iter()
                            .any(|name| !spawned.iter().any(|c| &c.name == name)) =>
                {
                    self.restarts.incomplete.insert(key.clone(), expected);
                }
                _ => {
                    self.restarts.incomplete.remove(&key);
                }
            }
            self.clients.insert(key.clone(), spawned);
        }
        self.clients.get(&key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The first server running for the open document `path` that `supports`
    /// a method, as its name and client. Re-probes first, so a server
    /// installed since the file was opened (e.g. the managed vtsls finishing
    /// its lazy background install) is picked up without reopening the file;
    /// cheap once the list is non-empty.
    async fn pick_server(
        &mut self,
        path: &Path,
        supports: fn(&ManagedClient) -> bool,
    ) -> Result<(String, Arc<TokioMutex<LspClient>>), NoServer> {
        let doc = self.docs.get(path).ok_or(NoServer::Unknown)?;
        let lang = doc.language;
        let root = doc.project_root.clone();
        self.ensure_clients(lang, &root).await;
        self.clients
            .get(&(lang, root))
            .ok_or(NoServer::Unknown)?
            .iter()
            .find(|c| supports(c))
            .map(|c| (c.name.clone(), c.client.clone()))
            .ok_or(NoServer::Unsupported)
    }

    /// Spawn (or begin provisioning) the servers for every language whose
    /// root marker is evident in the workspace root — exactly what the
    /// first `open_doc` of that language would do, minus the document.
    async fn prewarm_workspace(&mut self) {
        self.prewarm_root(self.workspace_root.clone()).await;
    }

    /// Prewarm one root: spawn the servers for every language whose
    /// marker sits at that root. Startup runs it on the primary; adding a
    /// workspace folder (#147) runs it on the new root so its servers
    /// come up like a fresh workspace's.
    async fn prewarm_root(&mut self, root: PathBuf) {
        for lang in crate::lsp::languages::languages_evident_in(&root) {
            let clients = self.ensure_clients(lang, &root).await;
            if !clients.is_empty() {
                log_file::log(&format!(
                    "lsp prewarm: {} server(s) up for {} at {}",
                    clients.len(),
                    lang.lsp_id(),
                    root.display()
                ));
            }
        }
    }

    async fn open_doc(&mut self, path: PathBuf, text: String) {
        // The app re-sends every tab after a restart (#694). A document whose
        // servers did not restart is still open there, and a second didOpen
        // would break the protocol, so it goes out as a full-text change.
        if self.docs.contains_key(&path) {
            self.change_doc(path, text).await;
            return;
        }
        let Some(lang) = path_to_language(&path) else {
            return;
        };
        let project_root =
            project_root_for_roots(&path, lang, &self.workspace_root, &self.extra_roots);
        let clients = self.ensure_clients(lang, &project_root).await;
        if clients.is_empty() {
            return;
        }
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let arcs: Vec<(String, Arc<TokioMutex<LspClient>>)> = clients
            .iter()
            .map(|c| (c.name.clone(), c.client.clone()))
            .collect();
        for (name, client_arc) in arcs {
            let mut client = client_arc.lock().await;
            if let Err(e) = client.did_open(uri.clone(), lang.lsp_id(), 0, text.clone()) {
                log_file::log(&format!("lsp[{name}] did_open failed: {e}"));
            }
        }
        // Pull-mode servers report only when asked (#866); ask at once, since
        // nothing is typed yet to debounce.
        let key: ClientKey = (lang, project_root.clone());
        let pulls = self
            .clients
            .get(&key)
            .map(|cs| document_pull_targets(cs.iter(), &path, &uri, &self.capability_support))
            .unwrap_or_default();
        spawn_document_pulls(
            pulls,
            self.diagnostics_tx.clone(),
            std::time::Duration::ZERO,
        );
        // Who reports on it (#851): every server open on it, less any retired
        // from document pulls, since those report nothing.
        let reporting: Vec<String> = self
            .clients
            .get(&key)
            .into_iter()
            .flatten()
            .filter(|c| !c.document_pull_stalled.load(Ordering::Relaxed))
            .map(|c| c.name.clone())
            .collect();
        let open_on: Vec<String> = self
            .clients
            .get(&key)
            .into_iter()
            .flatten()
            .map(|c| c.name.clone())
            .collect();
        if let Ok(mut support) = self.capability_support.lock() {
            support.document_servers.insert(path.clone(), reporting);
            support.document_clients.insert(path.clone(), open_on);
            support
                .document_roots
                .insert(path.clone(), project_root.clone());
        }
        self.docs.insert(
            path,
            DocState {
                language: lang,
                project_root,
                version: 0,
            },
        );
    }

    async fn change_doc(&mut self, path: PathBuf, text: String) {
        let Some(doc) = self.docs.get_mut(&path) else {
            return;
        };
        doc.version += 1;
        let version = doc.version;
        let key: ClientKey = (doc.language, doc.project_root.clone());
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let arcs: Vec<(String, Arc<TokioMutex<LspClient>>)> = match self.clients.get(&key) {
            Some(cs) => cs
                .iter()
                .map(|c| (c.name.clone(), c.client.clone()))
                .collect(),
            None => return,
        };
        for (name, client_arc) in arcs {
            let mut client = client_arc.lock().await;
            if let Err(e) = client.did_change_full(uri.clone(), version, text.clone()) {
                log_file::log(&format!("lsp[{name}] did_change failed: {e}"));
            }
        }
        let pulls = self
            .clients
            .get(&key)
            .map(|cs| document_pull_targets(cs.iter(), &path, &uri, &self.capability_support))
            .unwrap_or_default();
        spawn_document_pulls(pulls, self.diagnostics_tx.clone(), DOCUMENT_PULL_DEBOUNCE);
    }

    async fn save_doc(&mut self, path: PathBuf, text: String) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let key: ClientKey = (doc.language, doc.project_root.clone());
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        // Only the servers that asked for saves (#854): ruff advertises no
        // `save` and warns "no handler" on every notification it is sent.
        let arcs: Vec<(String, Arc<TokioMutex<LspClient>>, SaveNotify)> =
            match self.clients.get(&key) {
                Some(cs) => cs
                    .iter()
                    .filter(|c| c.save_notify != SaveNotify::Never)
                    .map(|c| (c.name.clone(), c.client.clone(), c.save_notify))
                    .collect(),
                None => return,
            };
        // The text is cloned only for a server that asks for it while another
        // still waits after it; the last one gets the buffer itself.
        let mut wanting = arcs
            .iter()
            .filter(|(_, _, notify)| *notify == SaveNotify::WithText)
            .count();
        let mut text = Some(text);
        for (name, client_arc, notify) in arcs {
            let text = if notify == SaveNotify::WithText {
                wanting -= 1;
                if wanting == 0 {
                    text.take()
                } else {
                    text.clone()
                }
            } else {
                None
            };
            let mut client = client_arc.lock().await;
            if let Err(e) = client.did_save(uri.clone(), text) {
                log_file::log(&format!("lsp[{name}] did_save failed: {e}"));
            }
        }
    }

    async fn close_doc(&mut self, path: PathBuf) {
        let Some(doc) = self.docs.remove(&path) else {
            return;
        };
        if let Ok(mut support) = self.capability_support.lock() {
            support.document_servers.remove(&path);
            support.document_clients.remove(&path);
            support.document_roots.remove(&path);
        }
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let key: ClientKey = (doc.language, doc.project_root.clone());
        let arcs: Vec<(String, Arc<TokioMutex<LspClient>>)> = match self.clients.get(&key) {
            Some(cs) => cs
                .iter()
                .map(|c| (c.name.clone(), c.client.clone()))
                .collect(),
            None => return,
        };
        for (name, client_arc) in arcs {
            let mut client = client_arc.lock().await;
            if let Err(e) = client.did_close(uri.clone()) {
                log_file::log(&format!("lsp[{name}] did_close failed: {e}"));
            }
        }
        // A pulled set describes an open buffer (#866): withdraw it, and
        // drop the generation so a pull still in flight stands down. A
        // server that also reports per workspace keeps covering the closed
        // file through that pull, so its rows stay.
        for managed in self.clients.get(&key).into_iter().flatten() {
            if !managed.supports_document_diagnostics {
                continue;
            }
            managed
                .document_pull_generations
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&path);
            if !managed.supports_workspace_diagnostics {
                let _ = self.diagnostics_tx.send(DiagnosticsUpdate {
                    path: path.clone(),
                    server: managed.name.clone(),
                    diagnostics: Vec::new(),
                    pulled: true,
                });
            }
        }
    }

    /// `workspace/didChangeWatchedFiles` fan-out (#908): each server gets
    /// the changes its watchers cover, less the documents it has open, which
    /// reach it through didChange. Existence is read here, off the UI
    /// thread, so a file that is gone by now goes out as deleted.
    fn did_change_watched_files(&mut self, changes: Vec<(PathBuf, lsp_types::FileChangeType)>) {
        let changes: Vec<_> = changes
            .into_iter()
            .map(|(path, typ)| match path.exists() {
                true => (path, typ),
                false => (path, lsp_types::FileChangeType::DELETED),
            })
            .collect();
        for ((lang, root), clients) in &self.clients {
            let closed: Arc<Vec<_>> = Arc::new(
                changes
                    .iter()
                    .filter(|(path, _)| {
                        !self
                            .docs
                            .get(path)
                            .is_some_and(|d| d.language == *lang && d.project_root == *root)
                    })
                    .cloned()
                    .collect(),
            );
            if closed.is_empty() {
                continue;
            }
            for c in clients {
                let (name, client, closed) = (c.name.clone(), c.client.clone(), closed.clone());
                tokio::spawn(async move {
                    if let Err(e) = client.lock().await.did_change_watched_files(&closed) {
                        log_file::log(&format!("lsp[{name}] didChangeWatchedFiles error: {e}"));
                    }
                });
            }
        }
    }
}

/// Normalise a `WorkspaceEdit` into per-file char-indexed spans. Handles both
/// the `changes` map and the `document_changes` form (edit operations only;
/// create/rename/delete file ops are ignored, which is correct for a symbol
/// rename). LSP positions are treated as char indices, matching how croft's
/// editor maps positions everywhere else.
fn workspace_edits(edit: &WorkspaceEdit) -> Vec<(PathBuf, Vec<TextSpanEdit>)> {
    let mut out: Vec<(PathBuf, Vec<TextSpanEdit>)> = Vec::new();
    if let Some(changes) = edit.changes.as_ref() {
        for (uri, edits) in changes {
            if let Ok(path) = uri.to_file_path() {
                out.push((path, edits.iter().map(text_edit_to_span).collect()));
            }
        }
    }
    if let Some(doc_changes) = edit.document_changes.as_ref() {
        match doc_changes {
            DocumentChanges::Edits(edits) => {
                for tde in edits {
                    if let Ok(path) = tde.text_document.uri.to_file_path() {
                        let spans = tde
                            .edits
                            .iter()
                            .map(|e| match e {
                                OneOf::Left(te) => text_edit_to_span(te),
                                OneOf::Right(ate) => text_edit_to_span(&ate.text_edit),
                            })
                            .collect();
                        out.push((path, spans));
                    }
                }
            }
            DocumentChanges::Operations(ops) => {
                for op in ops {
                    if let DocumentChangeOperation::Edit(tde) = op
                        && let Ok(path) = tde.text_document.uri.to_file_path()
                    {
                        let spans = tde
                            .edits
                            .iter()
                            .map(|e| match e {
                                OneOf::Left(te) => text_edit_to_span(te),
                                OneOf::Right(ate) => text_edit_to_span(&ate.text_edit),
                            })
                            .collect();
                        out.push((path, spans));
                    }
                }
            }
        }
    }
    out
}

fn text_edit_to_span(te: &TextEdit) -> TextSpanEdit {
    TextSpanEdit {
        start: (
            te.range.start.line as usize,
            te.range.start.character as usize,
        ),
        end: (te.range.end.line as usize, te.range.end.character as usize),
        new_text: te.new_text.clone(),
        utf16: true,
    }
}

/// Whether a `boolean | <Options>`-shaped capability (definition, rename, ...)
/// is actually supported. `None` is unsupported; a bare `false` is unsupported;
/// `true` or an options object is supported. The bare-`false` case is the one
/// `Option::is_some` got wrong.
fn one_of_supported<B>(cap: &Option<OneOf<bool, B>>) -> bool {
    match cap {
        Some(OneOf::Left(b)) => *b,
        Some(OneOf::Right(_)) => true,
        None => false,
    }
}

/// Whether a server asked for `textDocument/didSave`, and with the saved
/// text or without it (#854).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveNotify {
    /// Not asked for, so not sent: ruff advertises no `save` and warns "no
    /// handler" on every Ctrl+S that reaches it.
    Never,
    /// Sent bare; the server re-reads the file (rust-analyzer's `save: {}`).
    WithoutText,
    /// Sent carrying the text, for `save: { includeText: true }`.
    WithText,
}

/// Read [`SaveNotify`] off a server's `textDocumentSync` capability (#854).
///
/// The options form says it outright: `save` present asks for the
/// notification, absent asks for none ("if omitted the notification should
/// not be sent"), and `includeText` asks for the text. The bare
/// `TextDocumentSyncKind` number predates `save`, and VS Code, Neovim and
/// Helix all read a kind other than `None` as "saves, without the text"
/// (LSP issue #288). croft does the same, so a server that picked the old
/// form keeps hearing about saves as it does in every other editor.
///
/// Dynamic registration is not consulted because croft declares none: with
/// `synchronization.dynamicRegistration` false rust-analyzer advertises
/// `save: {}` here instead of registering it later, which is what keeps its
/// check-on-save running.
fn save_notify_of(sync: &Option<lsp_types::TextDocumentSyncCapability>) -> SaveNotify {
    use lsp_types::{TextDocumentSyncCapability as Sync, TextDocumentSyncSaveOptions as Save};
    match sync {
        Some(Sync::Options(options)) => match &options.save {
            Some(Save::SaveOptions(save)) if save.include_text == Some(true) => {
                SaveNotify::WithText
            }
            Some(Save::SaveOptions(_)) | Some(Save::Supported(true)) => SaveNotify::WithoutText,
            Some(Save::Supported(false)) | None => SaveNotify::Never,
        },
        Some(Sync::Kind(kind)) if *kind != lsp_types::TextDocumentSyncKind::NONE => {
            SaveNotify::WithoutText
        }
        Some(Sync::Kind(_)) | None => SaveNotify::Never,
    }
}

/// Result ids from the last `workspace/diagnostic` pull, per file (#533).
///
/// The incrementality contract: send back what the server last told us, and
/// it answers `Unchanged` for anything that has not moved instead of
/// resending the diagnostics. Without this every pull is a full re-report of
/// the whole workspace, which is the cost the pull path exists to avoid.
///
/// Keyed by URI string rather than `PathBuf` because that is what the server
/// echoes back, and round-tripping through a path loses the distinction on
/// any URI that is not a plain file (a server may report `untitled:` or a
/// virtual scheme). Comparing what it sent to what it sends is the whole job.
#[derive(Default)]
pub struct DiagnosticResultIds {
    by_uri: std::collections::HashMap<String, String>,
}

/// One routed prepare-rename: the server picked for it and where the
/// caret is.
struct PrepareRenameAsk {
    request_id: u64,
    server_name: String,
    client: Arc<TokioMutex<LspClient>>,
    uri: Url,
    line: u32,
    character: u32,
    /// The client's `prepare_rename_stalled`.
    stalled: Arc<AtomicBool>,
}

/// One server's share of a workspace pull (#533), gathered off the client
/// list so the request itself can run without holding the worker loop.
struct WorkspacePull {
    name: String,
    client: Arc<TokioMutex<LspClient>>,
    identifier: Option<String>,
    result_ids: Arc<TokioMutex<DiagnosticResultIds>>,
    /// Shared with the [`ManagedClient`], so a timeout recorded here is seen
    /// by every later `workspace_pull_targets` call.
    stalled: Arc<AtomicBool>,
}

/// Why a rustup proxy cannot start its server for a project (#927), and the
/// command that mends it for the toolchain that project selects.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RustupLack {
    why: String,
    fix: String,
}

#[cfg(test)]
mod tests;
