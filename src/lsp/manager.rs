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
mod documents;
mod formatting;
mod hover;
mod navigation;
mod rename;
mod semantic_tokens;
mod servers;
mod symbols;
mod text_ranges;
mod worker;
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
use documents::*;
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
use servers::*;
pub use symbols::OutlineKind;
pub use symbols::OutlineSymbol;
pub use symbols::WorkspaceSymbolItem;
use symbols::*;
#[cfg(test)]
pub use text_ranges::DocumentHighlightsResult;
pub use text_ranges::LinkedEditingResult;
pub use text_ranges::OccurrenceItem;
use text_ranges::*;
use worker::*;

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
    /// Test hook: a sender into `doc_highlights_rx`, so app tests can
    /// answer a documentHighlight request without a server.
    #[cfg(test)]
    doc_highlights_test_tx: std_mpsc::Sender<DocumentHighlightsResult>,
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
        #[cfg(test)]
        let doc_highlights_test_tx = doc_highlights_tx.clone();
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
            #[cfg(test)]
            doc_highlights_test_tx,
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

    /// Allocate the next request id, send the command `make` builds around
    /// it, and return the id the reply will echo.
    fn send_request(&mut self, make: impl FnOnce(u64) -> Cmd) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(make(id));
        id
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
