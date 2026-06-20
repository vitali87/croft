use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;

use anyhow::Result;
use lsp_types::{
    ClientCapabilities, CompletionClientCapabilities, CompletionItemCapability, CompletionItemKind,
    CompletionItemKindCapability, CompletionResponse, DeclarationCapability,
    DocumentChangeOperation, DocumentChanges, DocumentSymbol, DocumentSymbolClientCapabilities,
    DocumentSymbolResponse, GotoDefinitionResponse, HoverContents, HoverProviderCapability,
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
use crate::widgets::editor::TextSpanEdit;

#[derive(Debug, Clone)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: Option<String>,
    pub filter_text: Option<String>,
    pub kind: Option<CompletionItemKind>,
}

#[derive(Debug)]
pub struct CompletionResult {
    pub request_id: u64,
    pub path: PathBuf,
    pub items: Vec<CompletionItem>,
}

#[derive(Debug)]
pub struct HoverResult {
    pub request_id: u64,
    pub path: PathBuf,
    pub text: Option<String>,
}

#[derive(Debug)]
pub struct DefinitionResult {
    pub request_id: u64,
    pub path: PathBuf,
    pub target: Option<(PathBuf, u32, u32)>,
}

/// The kind of an outline symbol, normalised off the LSP `SymbolKind` wire
/// enum so the Outline widget never depends on `lsp-types`. Mirrors VS Code's
/// symbol categories one-to-one; the widget maps each to a codicon + colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineKind {
    File,
    Module,
    Namespace,
    Package,
    Class,
    Method,
    Property,
    Field,
    Constructor,
    Enum,
    Interface,
    Function,
    Variable,
    Constant,
    String,
    Number,
    Boolean,
    Array,
    Object,
    Key,
    Null,
    EnumMember,
    Struct,
    Event,
    Operator,
    TypeParameter,
}

impl OutlineKind {
    fn from_lsp(kind: SymbolKind) -> Self {
        match kind {
            SymbolKind::MODULE => Self::Module,
            SymbolKind::NAMESPACE => Self::Namespace,
            SymbolKind::PACKAGE => Self::Package,
            SymbolKind::CLASS => Self::Class,
            SymbolKind::METHOD => Self::Method,
            SymbolKind::PROPERTY => Self::Property,
            SymbolKind::FIELD => Self::Field,
            SymbolKind::CONSTRUCTOR => Self::Constructor,
            SymbolKind::ENUM => Self::Enum,
            SymbolKind::INTERFACE => Self::Interface,
            SymbolKind::FUNCTION => Self::Function,
            SymbolKind::VARIABLE => Self::Variable,
            SymbolKind::CONSTANT => Self::Constant,
            SymbolKind::STRING => Self::String,
            SymbolKind::NUMBER => Self::Number,
            SymbolKind::BOOLEAN => Self::Boolean,
            SymbolKind::ARRAY => Self::Array,
            SymbolKind::OBJECT => Self::Object,
            SymbolKind::KEY => Self::Key,
            SymbolKind::NULL => Self::Null,
            SymbolKind::ENUM_MEMBER => Self::EnumMember,
            SymbolKind::STRUCT => Self::Struct,
            SymbolKind::EVENT => Self::Event,
            SymbolKind::OPERATOR => Self::Operator,
            SymbolKind::TYPE_PARAMETER => Self::TypeParameter,
            // SymbolKind::FILE and any future/unknown kind fall back to File.
            _ => Self::File,
        }
    }
}

/// One row of the Outline, flattened from the server's symbol tree in document
/// order with a `depth` for indentation. `line`/`character` are the
/// `selectionRange` start (zero-based LSP UTF-16) — where the caret lands on
/// click — while `range_start_line`/`range_end_line` bound the whole construct
/// so the panel can highlight the symbol the editor caret currently sits in.
#[derive(Debug, Clone)]
pub struct OutlineSymbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: OutlineKind,
    pub depth: u16,
    pub line: u32,
    pub character: u32,
    pub range_start_line: u32,
    pub range_end_line: u32,
}

/// The Outline for one document. Keyed by `path` (the latest batch wins, like
/// `SemanticTokensUpdate`); the app applies it only when `path` still matches
/// the active editor, so a reply for a file the user navigated away from is
/// dropped without needing a request id.
#[derive(Debug)]
pub struct DocumentSymbolsResult {
    pub path: PathBuf,
    pub symbols: Vec<OutlineSymbol>,
}

#[derive(Debug)]
pub struct DeclarationResult {
    pub request_id: u64,
    pub path: PathBuf,
    pub target: Option<(PathBuf, u32, u32)>,
    /// True when no spawned server advertises a usable `declarationProvider`
    /// (e.g. vtsls, which sends `declarationProvider: false`). Lets the app
    /// say so plainly instead of silently doing nothing.
    pub unsupported: bool,
}

#[derive(Debug)]
pub struct TypeDefinitionResult {
    pub request_id: u64,
    pub path: PathBuf,
    pub target: Option<(PathBuf, u32, u32)>,
    /// True when no spawned server advertises a usable `typeDefinitionProvider`.
    /// Mirrors `DeclarationResult::unsupported` so the app can report it plainly
    /// rather than silently no-op.
    pub unsupported: bool,
}

#[derive(Debug)]
pub struct ImplementationResult {
    pub request_id: u64,
    pub path: PathBuf,
    /// Every implementation location the server returned. `textDocument/
    /// implementation` is the 1:many goto (one abstraction, several
    /// implementors), so all of them are carried through; the app jumps
    /// directly when there is one and shows a picker when there are several.
    pub targets: Vec<(PathBuf, u32, u32)>,
    /// True when no spawned server advertises a usable `implementationProvider`.
    pub unsupported: bool,
}

#[derive(Debug)]
pub struct ReferencesResult {
    pub request_id: u64,
    pub path: PathBuf,
    /// Every reference location the server returned. `textDocument/references`
    /// is inherently 1:many (a symbol used in many places), so all of them are
    /// carried through; the app jumps directly when there is one and shows a
    /// picker when there are several, exactly like Go to Implementations.
    pub targets: Vec<(PathBuf, u32, u32)>,
    /// True when no spawned server advertises a usable `referencesProvider`.
    pub unsupported: bool,
}

#[derive(Debug)]
pub struct RenameResult {
    pub request_id: u64,
    pub path: PathBuf,
    /// `None` when the server declined or returned no edits. Otherwise the
    /// per-file, char-indexed spans to apply across the workspace.
    pub edits: Option<Vec<(PathBuf, Vec<TextSpanEdit>)>>,
}

/// A fresh batch of semantic tokens for a document, pushed to the editor
/// to overlay on the tree-sitter highlights. Unlike the navigation
/// requests this carries no `request_id`: it is keyed by `path` and the
/// latest batch always wins. `data` is the raw relative-encoded LSP
/// array; the editor decodes it against its own buffer (where the text
/// lives) so UTF-16 columns convert to byte offsets correctly. `legend`
/// maps token-type indices to names.
#[derive(Debug)]
pub struct SemanticTokensUpdate {
    pub path: PathBuf,
    pub data: Vec<u32>,
    pub legend: Arc<Vec<String>>,
    /// `true` for a whole-document `semanticTokens/full` batch, `false` for a
    /// viewport-only `semanticTokens/range` batch. The editor refuses to let a
    /// range batch overwrite a full one for the same file, so a range reply
    /// that arrives after the full document can never erase off-screen colour.
    pub is_full: bool,
}

/// Severity of a diagnostic, normalised off the LSP `DiagnosticSeverity`
/// wire enum. Drives the underline colour the editor paints (VS Code: red
/// for errors, yellow for warnings, blue/teal for info & hints).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// One diagnostic from a language server, normalised off the LSP wire type.
/// Positions are LSP UTF-16 (`line`, `character`), zero-based; the editor
/// converts them to character columns against its own buffer, exactly as it
/// does for semantic tokens. A diagnostic can span several lines
/// (`start_line..=end_line`).
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub start_line: u32,
    pub start_char: u32,
    pub end_line: u32,
    pub end_char: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

/// A fresh, COMPLETE set of diagnostics for one document from one server,
/// pushed via `textDocument/publishDiagnostics`. LSP always republishes the
/// whole list (an empty `diagnostics` means "all clear"), so each update
/// replaces the prior set for the same `(path, server)` pair. Keyed by
/// `server` too, so two servers analysing one file (e.g. ty + ruff) layer
/// their diagnostics instead of clobbering each other.
#[derive(Debug, Clone)]
pub struct DiagnosticsUpdate {
    pub path: PathBuf,
    pub server: String,
    pub diagnostics: Vec<Diagnostic>,
}

/// A server's work-done progress update, forwarded from a `$/progress`
/// notification. `message` is `Some(text)` while a task is running (Begin /
/// Report) and `None` when it ends (End), so the app can show or clear the
/// per-server status (e.g. "rust-analyzer: Indexing 112/340 33%").
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    pub server: String,
    pub message: Option<String>,
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
    RequestSemanticTokens {
        path: PathBuf,
    },
    RequestSemanticTokensRange {
        path: PathBuf,
        start_line: u32,
        end_line: u32,
    },
    ChangeDoc {
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
    },
    RequestHover {
        request_id: u64,
        path: PathBuf,
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
    RequestReferences {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
    },
    RequestRename {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        new_name: String,
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
}
type CapabilitySupport = Arc<StdMutex<LangCapabilitySupport>>;

pub struct LspManager {
    cmd_tx: tokio_mpsc::UnboundedSender<Cmd>,
    completion_rx: std_mpsc::Receiver<CompletionResult>,
    hover_rx: std_mpsc::Receiver<HoverResult>,
    def_rx: std_mpsc::Receiver<DefinitionResult>,
    doc_symbols_rx: std_mpsc::Receiver<DocumentSymbolsResult>,
    decl_rx: std_mpsc::Receiver<DeclarationResult>,
    type_def_rx: std_mpsc::Receiver<TypeDefinitionResult>,
    impl_rx: std_mpsc::Receiver<ImplementationResult>,
    ref_rx: std_mpsc::Receiver<ReferencesResult>,
    rename_rx: std_mpsc::Receiver<RenameResult>,
    semantic_rx: std_mpsc::Receiver<SemanticTokensUpdate>,
    diagnostics_rx: std_mpsc::Receiver<DiagnosticsUpdate>,
    progress_rx: std_mpsc::Receiver<ProgressUpdate>,
    capability_support: CapabilitySupport,
    semantic_refresh: Arc<AtomicBool>,
    next_request_id: u64,
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

impl LspManager {
    pub fn new(workspace_root: PathBuf) -> Result<Self> {
        let runtime = LspRuntime::new()?;
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let (completion_tx, completion_rx) = std_mpsc::channel();
        let (hover_tx, hover_rx) = std_mpsc::channel();
        let (def_tx, def_rx) = std_mpsc::channel();
        let (doc_symbols_tx, doc_symbols_rx) = std_mpsc::channel();
        let (decl_tx, decl_rx) = std_mpsc::channel();
        let (type_def_tx, type_def_rx) = std_mpsc::channel();
        let (impl_tx, impl_rx) = std_mpsc::channel();
        let (ref_tx, ref_rx) = std_mpsc::channel();
        let (rename_tx, rename_rx) = std_mpsc::channel();
        let (semantic_tx, semantic_rx) = std_mpsc::channel();
        let (diagnostics_tx, diagnostics_rx) = std_mpsc::channel();
        let (progress_tx, progress_rx) = std_mpsc::channel();
        let capability_support: CapabilitySupport =
            Arc::new(StdMutex::new(LangCapabilitySupport::default()));
        let semantic_refresh = Arc::new(AtomicBool::new(false));
        let root = workspace_root.clone();
        runtime.handle().spawn(worker_loop(
            root,
            ServerRegistry::with_defaults(),
            cmd_rx,
            ResultSenders {
                completion: completion_tx,
                hover: hover_tx,
                definition: def_tx,
                document_symbols: doc_symbols_tx,
                declaration: decl_tx,
                type_definition: type_def_tx,
                implementation: impl_tx,
                references: ref_tx,
                rename: rename_tx,
                semantic_tokens: semantic_tx,
                diagnostics: diagnostics_tx,
                progress: progress_tx,
            },
            capability_support.clone(),
            semantic_refresh.clone(),
        ));
        Ok(Self {
            cmd_tx,
            completion_rx,
            hover_rx,
            def_rx,
            doc_symbols_rx,
            decl_rx,
            type_def_rx,
            impl_rx,
            ref_rx,
            rename_rx,
            semantic_rx,
            diagnostics_rx,
            progress_rx,
            capability_support,
            semantic_refresh,
            next_request_id: 1,
            workspace_root,
            _runtime: runtime,
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn open_doc(&self, path: PathBuf, text: String) {
        let _ = self.cmd_tx.send(Cmd::OpenDoc { path, text });
    }

    pub fn change_doc(&self, path: PathBuf, text: String) {
        let _ = self.cmd_tx.send(Cmd::ChangeDoc { path, text });
    }

    pub fn close_doc(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(Cmd::CloseDoc { path });
    }

    pub fn request_completion(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestCompletion {
            request_id: id,
            path,
            line,
            character,
        });
        id
    }

    pub fn drain_completion(&self) -> Option<CompletionResult> {
        self.completion_rx.try_recv().ok()
    }

    pub fn request_hover(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestHover {
            request_id: id,
            path,
            line,
            character,
        });
        id
    }

    pub fn drain_hover(&self) -> Option<HoverResult> {
        self.hover_rx.try_recv().ok()
    }

    /// Ask the server for the whole document's semantic tokens. Fire this
    /// on open and (debounced) after edits; the freshest batch wins. No
    /// request id: results are keyed by path. A no-op for languages whose
    /// server advertises no `semanticTokensProvider`.
    pub fn request_semantic_tokens(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(Cmd::RequestSemanticTokens { path });
    }

    /// Ask the server for semantic tokens covering only `start_line..end_line`
    /// (zero-based, half-open). Fire this on open with the editor's viewport so
    /// the visible code colours immediately, ahead of the whole-file request.
    pub fn request_semantic_tokens_range(&self, path: PathBuf, start_line: u32, end_line: u32) {
        let _ = self.cmd_tx.send(Cmd::RequestSemanticTokensRange {
            path,
            start_line,
            end_line,
        });
    }

    pub fn drain_semantic_tokens(&self) -> Option<SemanticTokensUpdate> {
        self.semantic_rx.try_recv().ok()
    }

    /// Pop the next server-pushed diagnostics batch, if any. Each batch is the
    /// complete set for one `(path, server)` pair; the app keeps a per-file,
    /// per-server store so layering and "all clear" (empty batch) both work.
    pub fn drain_diagnostics(&self) -> Option<DiagnosticsUpdate> {
        self.diagnostics_rx.try_recv().ok()
    }

    /// Pop the next server work-done progress update, if any. `message` is
    /// `Some` while a task runs and `None` when it ends; the app keeps a
    /// per-server map so the status bar can show or clear "Indexing…".
    pub fn drain_progress(&self) -> Option<ProgressUpdate> {
        self.progress_rx.try_recv().ok()
    }

    /// Returns and clears the "a server asked us to re-pull semantic tokens"
    /// flag, set by any client that received `workspace/semanticTokens/refresh`
    /// (rust-analyzer does this once its analysis upgrades the token set). The
    /// app re-requests tokens for the visible editor(s) when this is true.
    pub fn take_semantic_refresh(&self) -> bool {
        self.semantic_refresh.swap(false, Ordering::Relaxed)
    }

    pub fn request_definition(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestDefinition {
            request_id: id,
            path,
            line,
            character,
        });
        id
    }

    pub fn drain_definition(&self) -> Option<DefinitionResult> {
        self.def_rx.try_recv().ok()
    }

    /// Ask the server for the document's symbol tree (the Outline). Fire on
    /// open, on the active editor tab changing, and (debounced) after edits;
    /// the freshest reply for the active file wins. No request id: replies are
    /// keyed by path and the app drops any whose path is no longer active.
    pub fn request_document_symbols(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(Cmd::RequestDocumentSymbols { path });
    }

    pub fn drain_document_symbols(&self) -> Option<DocumentSymbolsResult> {
        self.doc_symbols_rx.try_recv().ok()
    }

    pub fn request_declaration(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestDeclaration {
            request_id: id,
            path,
            line,
            character,
        });
        id
    }

    pub fn drain_declaration(&self) -> Option<DeclarationResult> {
        self.decl_rx.try_recv().ok()
    }

    pub fn request_type_definition(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestTypeDefinition {
            request_id: id,
            path,
            line,
            character,
        });
        id
    }

    pub fn drain_type_definition(&self) -> Option<TypeDefinitionResult> {
        self.type_def_rx.try_recv().ok()
    }

    pub fn request_implementation(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestImplementation {
            request_id: id,
            path,
            line,
            character,
        });
        id
    }

    pub fn drain_implementation(&self) -> Option<ImplementationResult> {
        self.impl_rx.try_recv().ok()
    }

    pub fn request_references(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestReferences {
            request_id: id,
            path,
            line,
            character,
        });
        id
    }

    pub fn drain_references(&self) -> Option<ReferencesResult> {
        self.ref_rx.try_recv().ok()
    }

    /// Whether the server for `lang` implements `textDocument/declaration`.
    /// `None` means no server has reported yet (not spawned). Read synchronously
    /// by the app to decide whether to show the "Go to Declaration" menu item.
    pub fn language_supports_declaration(&self, lang: Language) -> Option<bool> {
        self.capability_support
            .lock()
            .ok()?
            .declaration
            .get(&lang)
            .copied()
    }

    /// Whether the server for `lang` implements `textDocument/typeDefinition`.
    /// `None` means no server has reported yet. Read synchronously by the app to
    /// decide whether to show the "Go to Type Definition" menu item.
    pub fn language_supports_type_definition(&self, lang: Language) -> Option<bool> {
        self.capability_support
            .lock()
            .ok()?
            .type_definition
            .get(&lang)
            .copied()
    }

    /// Whether the server for `lang` implements `textDocument/implementation`.
    /// `None` means no server has reported yet. Read synchronously by the app to
    /// decide whether to show the "Go to Implementations" menu item.
    pub fn language_supports_implementation(&self, lang: Language) -> Option<bool> {
        self.capability_support
            .lock()
            .ok()?
            .implementation
            .get(&lang)
            .copied()
    }

    /// Whether the server for `lang` implements `textDocument/references`.
    /// `None` means no server has reported yet. Read synchronously by the app to
    /// decide whether to show the "Go to References" menu item.
    pub fn language_supports_references(&self, lang: Language) -> Option<bool> {
        self.capability_support
            .lock()
            .ok()?
            .references
            .get(&lang)
            .copied()
    }

    pub fn request_rename(
        &mut self,
        path: PathBuf,
        line: u32,
        character: u32,
        new_name: String,
    ) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestRename {
            request_id: id,
            path,
            line,
            character,
            new_name,
        });
        id
    }

    pub fn drain_rename(&self) -> Option<RenameResult> {
        self.rename_rx.try_recv().ok()
    }
}

struct ManagedClient {
    name: String,
    client: Arc<TokioMutex<LspClient>>,
    supports_completion: bool,
    supports_hover: bool,
    supports_definition: bool,
    supports_document_symbol: bool,
    supports_declaration: bool,
    supports_type_definition: bool,
    supports_implementation: bool,
    supports_references: bool,
    supports_rename: bool,
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
}

/// Servers are keyed by language AND the file's project root, not language
/// alone. A monorepo has one `.venv` (and `pyproject.toml`) per sub-project, so
/// a single server rooted at croft's workspace root can't resolve any of them.
/// Following Zed's model, croft runs a server instance per (language, project
/// root); each is rooted at the project so basedpyright/rust-analyzer/gopls
/// auto-detect that project's environment.
type ClientKey = (Language, PathBuf);

struct WorkerState {
    workspace_root: PathBuf,
    registry: ServerRegistry,
    clients: HashMap<ClientKey, Vec<ManagedClient>>,
    docs: HashMap<PathBuf, DocState>,
    capability_support: CapabilitySupport,
    // Shared with every spawned client's router; a client sets it on a
    // `workspace/semanticTokens/refresh` and the app polls + clears it.
    semantic_refresh: Arc<AtomicBool>,
    // Cloned into every spawned client's router so the server-pushed
    // `textDocument/publishDiagnostics` notifications reach the app.
    diagnostics_tx: std_mpsc::Sender<DiagnosticsUpdate>,
    // Cloned into every spawned client's router so server `$/progress`
    // notifications (rust-analyzer indexing, etc.) reach the status bar.
    progress_tx: std_mpsc::Sender<ProgressUpdate>,
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
    hover: std_mpsc::Sender<HoverResult>,
    definition: std_mpsc::Sender<DefinitionResult>,
    document_symbols: std_mpsc::Sender<DocumentSymbolsResult>,
    declaration: std_mpsc::Sender<DeclarationResult>,
    type_definition: std_mpsc::Sender<TypeDefinitionResult>,
    implementation: std_mpsc::Sender<ImplementationResult>,
    references: std_mpsc::Sender<ReferencesResult>,
    rename: std_mpsc::Sender<RenameResult>,
    semantic_tokens: std_mpsc::Sender<SemanticTokensUpdate>,
    diagnostics: std_mpsc::Sender<DiagnosticsUpdate>,
    progress: std_mpsc::Sender<ProgressUpdate>,
}

async fn worker_loop(
    workspace_root: PathBuf,
    registry: ServerRegistry,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Cmd>,
    tx: ResultSenders,
    capability_support: CapabilitySupport,
    semantic_refresh: Arc<AtomicBool>,
) {
    let mut state = WorkerState {
        workspace_root,
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support,
        semantic_refresh,
        diagnostics_tx: tx.diagnostics.clone(),
        progress_tx: tx.progress.clone(),
    };
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Cmd::Shutdown { done } => {
                state.shutdown_all().await;
                let _ = done.send(());
                return;
            }
            Cmd::OpenDoc { path, text } => state.open_doc(path, text).await,
            Cmd::RequestSemanticTokens { path } => {
                state
                    .request_semantic_tokens(path, &tx.semantic_tokens)
                    .await
            }
            Cmd::RequestSemanticTokensRange {
                path,
                start_line,
                end_line,
            } => {
                state
                    .request_semantic_tokens_range(path, start_line, end_line, &tx.semantic_tokens)
                    .await
            }
            Cmd::ChangeDoc { path, text } => state.change_doc(path, text).await,
            Cmd::CloseDoc { path } => state.close_doc(path).await,
            Cmd::RequestCompletion {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_completion(request_id, path, line, character, &tx.completion)
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
            Cmd::RequestDocumentSymbols { path } => {
                state
                    .request_document_symbols(path, &tx.document_symbols)
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
                let _ = client.lock().await.shutdown().await;
            }));
        }
        for t in tasks {
            let _ = t.await;
        }
    }

    async fn ensure_clients(&mut self, lang: Language, root: &Path) -> &[ManagedClient] {
        // Re-probe when the cached list is empty: a server (e.g. the managed
        // vtsls) may have finished its background install since the first
        // attempt, so this is how TS LSP comes alive without a relaunch.
        // A non-empty list is stable and never re-probed.
        let key: ClientKey = (lang, root.to_path_buf());
        let first_attempt = !self.clients.contains_key(&key);
        let should_try = first_attempt
            || self
                .clients
                .get(&key)
                .is_some_and(|clients| clients.is_empty());
        if should_try {
            let configs: Vec<ServerConfig> = self.registry.for_language(lang).to_vec();
            // Spawn every server for this root concurrently rather than awaiting
            // each in turn. The worker loop processes commands one at a time, so
            // a sequential spawn chain made the OpenDoc command (and the
            // semantic-token request queued right behind it) block on the SUM of
            // ty + basedpyright + ruff init handshakes. Concurrent spawn bounds
            // the cold-root stall to the SLOWEST single handshake instead.
            let resolved: Vec<(ServerConfig, Vec<PathBuf>)> = configs
                .iter()
                .filter_map(|config| resolve_config(config, first_attempt))
                .collect();
            let outcomes =
                futures::future::join_all(resolved.into_iter().map(|(config, extra_path)| {
                    let caps = build_client_capabilities();
                    let refresh = self.semantic_refresh.clone();
                    let diagnostics = self.diagnostics_tx.clone();
                    let progress = self.progress_tx.clone();
                    async move {
                        let result = LspClient::spawn(
                            &config,
                            root,
                            caps,
                            &extra_path,
                            refresh,
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
                        let supports_rename = one_of_supported(&caps.rename_provider);
                        let semantic_legend = semantic_legend_of(caps).map(Arc::new);
                        let semantic_supports_range = semantic_tokens_range_supported(caps);
                        log_file::log(&format!(
                            "lsp[{}] spawned, root={} supports_completion={supports} supports_hover={supports_hover} supports_definition={supports_definition} supports_declaration={supports_declaration} supports_type_definition={supports_type_definition} supports_implementation={supports_implementation} supports_references={supports_references} supports_rename={supports_rename}",
                            config.name,
                            root.display()
                        ));
                        spawned.push(ManagedClient {
                            name: config.name.to_string(),
                            client: Arc::new(TokioMutex::new(client)),
                            supports_completion: supports,
                            supports_hover,
                            supports_definition,
                            supports_document_symbol,
                            supports_declaration,
                            supports_type_definition,
                            supports_implementation,
                            supports_references,
                            supports_rename,
                            semantic_legend,
                            semantic_supports_range,
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
            if let Ok(mut support) = self.capability_support.lock() {
                support.declaration.insert(lang, supports_declaration);
                support
                    .type_definition
                    .insert(lang, supports_type_definition);
                support.implementation.insert(lang, supports_implementation);
                support.references.insert(lang, supports_references);
            }
            self.clients.insert(key.clone(), spawned);
        }
        self.clients.get(&key).map(Vec::as_slice).unwrap_or(&[])
    }

    async fn open_doc(&mut self, path: PathBuf, text: String) {
        let Some(lang) = path_to_language(&path) else {
            return;
        };
        let project_root = project_root_for(&path, lang, &self.workspace_root);
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
    }

    async fn close_doc(&mut self, path: PathBuf) {
        let Some(doc) = self.docs.remove(&path) else {
            return;
        };
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
    }

    async fn request_completion(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<CompletionResult>,
    ) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        // Re-probe so a server installed since the file was opened (e.g. the
        // managed vtsls finishing its lazy background install) is picked up
        // without reopening the file. Cheap once the list is non-empty.
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            return;
        };
        let picked = clients
            .iter()
            .find(|c| c.supports_completion)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            log_file::log(&format!(
                "completion request id={request_id} dropped: no client advertises completion_provider for {lang:?}"
            ));
            let _ = tx.send(CompletionResult {
                request_id,
                path,
                items: Vec::new(),
            });
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "completion request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.completion(uri, line, character).await;
            drop(client);
            let (is_incomplete, items): (Option<bool>, Vec<CompletionItem>) = match resp {
                Ok(Some(CompletionResponse::Array(items))) => {
                    (None, items.into_iter().map(into_item).collect())
                }
                Ok(Some(CompletionResponse::List(list))) => (
                    Some(list.is_incomplete),
                    list.items.into_iter().map(into_item).collect(),
                ),
                Ok(None) => (None, Vec::new()),
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] completion error: {e}"));
                    (None, Vec::new())
                }
            };
            let preview: Vec<&str> = items.iter().take(200).map(|i| i.label.as_str()).collect();
            let sample: Vec<String> = items
                .iter()
                .take(5)
                .map(|i| {
                    format!(
                        "{{label={:?} kind={:?} detail={:?} filter_text={:?} insert_text={:?}}}",
                        i.label, i.kind, i.detail, i.filter_text, i.insert_text
                    )
                })
                .collect();
            log_file::log(&format!(
                "completion response id={request_id} server={server_name} is_incomplete={is_incomplete:?} count={} labels={:?}",
                items.len(),
                preview
            ));
            if !sample.is_empty() {
                log_file::log(&format!(
                    "completion response id={request_id} server={server_name} sample={sample:?}"
                ));
            }
            let _ = tx.send(CompletionResult {
                request_id,
                path: path_clone,
                items,
            });
        });
    }

    async fn request_hover(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<HoverResult>,
    ) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        // Re-probe so a server installed since the file was opened (e.g. the
        // managed vtsls finishing its lazy background install) is picked up
        // without reopening the file. Cheap once the list is non-empty.
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            return;
        };
        let picked = clients
            .iter()
            .find(|c| c.supports_hover)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            let _ = tx.send(HoverResult {
                request_id,
                path,
                text: None,
            });
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "hover request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.hover(uri, line, character).await;
            drop(client);
            let text = match resp {
                Ok(Some(h)) => hover_text(&h.contents),
                Ok(None) => None,
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] hover error: {e}"));
                    None
                }
            };
            log_file::log(&format!(
                "hover response id={request_id} server={server_name} has_text={}",
                text.is_some()
            ));
            let _ = tx.send(HoverResult {
                request_id,
                path: path_clone,
                text,
            });
        });
    }

    async fn request_semantic_tokens(
        &mut self,
        path: PathBuf,
        tx: &std_mpsc::Sender<SemanticTokensUpdate>,
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
        // Prefer a range-capable semantic-token provider (ty answers in tens
        // of ms even on a huge cold workspace and is a purpose-built
        // incremental highlighter) over a full-only one (basedpyright, which
        // pays a slow whole-tree enumeration before its first response). Fall
        // back to any server with a legend so single-server languages and
        // setups without ty still get tokens.
        let pick = |c: &ManagedClient| {
            c.semantic_legend
                .clone()
                .map(|leg| (c.name.clone(), c.client.clone(), leg))
        };
        let picked = clients
            .iter()
            .filter(|c| c.semantic_supports_range)
            .find_map(&pick)
            .or_else(|| clients.iter().find_map(&pick));
        let Some((server_name, client_arc, legend)) = picked else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        tokio::spawn(async move {
            // A server freshly spawned for a cold root can answer the very
            // first `semanticTokens/full` (fired microseconds after didOpen)
            // with an empty set before its analysis is ready, then have the
            // real tokens available a beat later. ty does NOT send
            // `workspace/semanticTokens/refresh` to prompt a re-pull (verified
            // empirically), and the doc-sync loop only re-requests on an edit,
            // so without this retry an unedited file would stay uncoloured for
            // seconds. Retry on empty with a short bounded backoff; stop at the
            // first non-empty batch (or after the last attempt, emitting empty
            // so a genuinely token-free file still resolves).
            const BACKOFF_MS: [u64; 5] = [0, 80, 160, 320, 640];
            let mut data: Vec<u32> = Vec::new();
            for (attempt, delay) in BACKOFF_MS.iter().enumerate() {
                if *delay > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(*delay)).await;
                }
                let mut client = client_arc.lock().await;
                let resp = client.semantic_tokens_full(uri.clone()).await;
                drop(client);
                data = match resp {
                    Ok(Some(SemanticTokensResult::Tokens(t))) => flatten_semantic_tokens(&t.data),
                    Ok(Some(SemanticTokensResult::Partial(p))) => flatten_semantic_tokens(&p.data),
                    Ok(None) => Vec::new(),
                    Err(e) => {
                        log_file::log(&format!("lsp[{server_name}] semantic_tokens error: {e}"));
                        Vec::new()
                    }
                };
                if !data.is_empty() {
                    if attempt > 0 {
                        log_file::log(&format!(
                            "semantic_tokens server={server_name} resolved on retry #{attempt} for {}",
                            path_clone.display()
                        ));
                    }
                    break;
                }
            }
            log_file::log(&format!(
                "semantic_tokens response server={server_name} path={} tokens={}",
                path_clone.display(),
                data.len() / 5
            ));
            let _ = tx.send(SemanticTokensUpdate {
                path: path_clone,
                data,
                legend,
                is_full: true,
            });
        });
    }

    async fn request_semantic_tokens_range(
        &mut self,
        path: PathBuf,
        start_line: u32,
        end_line: u32,
        tx: &std_mpsc::Sender<SemanticTokensUpdate>,
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
        // A range request only makes sense to a server that actually advertises
        // `semanticTokens/range`; there is no full-only fallback here because
        // the whole-document `request_semantic_tokens` covers those servers.
        let picked = clients
            .iter()
            .filter(|c| c.semantic_supports_range)
            .find_map(|c| {
                c.semantic_legend
                    .clone()
                    .map(|leg| (c.name.clone(), c.client.clone(), leg))
            });
        let Some((server_name, client_arc, legend)) = picked else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client
                .semantic_tokens_range(uri, start_line, end_line)
                .await;
            drop(client);
            let data: Vec<u32> = match resp {
                Ok(Some(SemanticTokensRangeResult::Tokens(t))) => flatten_semantic_tokens(&t.data),
                Ok(Some(SemanticTokensRangeResult::Partial(p))) => flatten_semantic_tokens(&p.data),
                Ok(None) => Vec::new(),
                Err(e) => {
                    log_file::log(&format!(
                        "lsp[{server_name}] semantic_tokens_range error: {e}"
                    ));
                    Vec::new()
                }
            };
            log_file::log(&format!(
                "semantic_tokens range response server={server_name} path={} lines={start_line}..{end_line} tokens={}",
                path_clone.display(),
                data.len() / 5
            ));
            let _ = tx.send(SemanticTokensUpdate {
                path: path_clone,
                data,
                legend,
                is_full: false,
            });
        });
    }

    async fn request_definition(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<DefinitionResult>,
    ) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        // Re-probe so a server installed since the file was opened (e.g. the
        // managed vtsls finishing its lazy background install) is picked up
        // without reopening the file. Cheap once the list is non-empty.
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            return;
        };
        let picked = clients
            .iter()
            .find(|c| c.supports_definition)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            let _ = tx.send(DefinitionResult {
                request_id,
                path,
                target: None,
            });
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "definition request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.definition(uri, line, character).await;
            drop(client);
            let target = match resp {
                Ok(Some(r)) => def_location(&r),
                Ok(None) => None,
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] definition error: {e:#}"));
                    None
                }
            };
            log_file::log(&format!(
                "definition response id={request_id} server={server_name} has_target={}",
                target.is_some()
            ));
            let _ = tx.send(DefinitionResult {
                request_id,
                path: path_clone,
                target,
            });
        });
    }

    async fn request_document_symbols(
        &mut self,
        path: PathBuf,
        tx: &std_mpsc::Sender<DocumentSymbolsResult>,
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
        let picked = clients
            .iter()
            .find(|c| c.supports_document_symbol)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            // No symbol provider: send an empty outline so the panel shows
            // "No symbols" rather than a stale tree from a previous file.
            let _ = tx.send(DocumentSymbolsResult {
                path,
                symbols: Vec::new(),
            });
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.document_symbol(uri).await;
            drop(client);
            let symbols = match resp {
                Ok(Some(r)) => flatten_symbols(r),
                Ok(None) => Vec::new(),
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] document_symbol error: {e:#}"));
                    Vec::new()
                }
            };
            log_file::log(&format!(
                "document_symbol response server={server_name} count={}",
                symbols.len()
            ));
            let _ = tx.send(DocumentSymbolsResult {
                path: path_clone,
                symbols,
            });
        });
    }

    async fn request_declaration(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<DeclarationResult>,
    ) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        // Re-probe so a server installed since the file was opened (e.g. the
        // managed vtsls finishing its lazy background install) is picked up
        // without reopening the file. Cheap once the list is non-empty.
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            return;
        };
        let picked = clients
            .iter()
            .find(|c| c.supports_declaration)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            let _ = tx.send(DeclarationResult {
                request_id,
                path,
                target: None,
                unsupported: true,
            });
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "declaration request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.declaration(uri, line, character).await;
            drop(client);
            let target = match resp {
                Ok(Some(r)) => def_location(&r),
                Ok(None) => None,
                // `{e:#}` prints the full anyhow chain (the underlying JSON-RPC
                // error), not just the "declaration" context label.
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] declaration error: {e:#}"));
                    None
                }
            };
            log_file::log(&format!(
                "declaration response id={request_id} server={server_name} has_target={}",
                target.is_some()
            ));
            let _ = tx.send(DeclarationResult {
                request_id,
                path: path_clone,
                target,
                unsupported: false,
            });
        });
    }

    async fn request_type_definition(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<TypeDefinitionResult>,
    ) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        // Re-probe so a server installed since the file was opened (e.g. the
        // managed vtsls finishing its lazy background install) is picked up
        // without reopening the file. Cheap once the list is non-empty.
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            return;
        };
        let picked = clients
            .iter()
            .find(|c| c.supports_type_definition)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            let _ = tx.send(TypeDefinitionResult {
                request_id,
                path,
                target: None,
                unsupported: true,
            });
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "type_definition request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.type_definition(uri, line, character).await;
            drop(client);
            let target = match resp {
                Ok(Some(r)) => def_location(&r),
                Ok(None) => None,
                // `{e:#}` prints the full anyhow chain (the underlying JSON-RPC
                // error), not just the "type_definition" context label.
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] type_definition error: {e:#}"));
                    None
                }
            };
            log_file::log(&format!(
                "type_definition response id={request_id} server={server_name} has_target={}",
                target.is_some()
            ));
            let _ = tx.send(TypeDefinitionResult {
                request_id,
                path: path_clone,
                target,
                unsupported: false,
            });
        });
    }

    async fn request_implementation(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<ImplementationResult>,
    ) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        // Re-probe so a server installed since the file was opened (e.g. the
        // managed vtsls finishing its lazy background install) is picked up
        // without reopening the file. Cheap once the list is non-empty.
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            return;
        };
        let picked = clients
            .iter()
            .find(|c| c.supports_implementation)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            let _ = tx.send(ImplementationResult {
                request_id,
                path,
                targets: Vec::new(),
                unsupported: true,
            });
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "implementation request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.implementation(uri, line, character).await;
            drop(client);
            let targets = match resp {
                Ok(Some(r)) => def_locations(&r),
                Ok(None) => Vec::new(),
                // `{e:#}` prints the full anyhow chain (the underlying JSON-RPC
                // error), not just the "implementation" context label.
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] implementation error: {e:#}"));
                    Vec::new()
                }
            };
            log_file::log(&format!(
                "implementation response id={request_id} server={server_name} count={}",
                targets.len()
            ));
            let _ = tx.send(ImplementationResult {
                request_id,
                path: path_clone,
                targets,
                unsupported: false,
            });
        });
    }

    async fn request_references(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<ReferencesResult>,
    ) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        // Re-probe so a server installed since the file was opened (e.g. the
        // managed vtsls finishing its lazy background install) is picked up
        // without reopening the file. Cheap once the list is non-empty.
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            return;
        };
        let picked = clients
            .iter()
            .find(|c| c.supports_references)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            let _ = tx.send(ReferencesResult {
                request_id,
                path,
                targets: Vec::new(),
                unsupported: true,
            });
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "references request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.references(uri, line, character).await;
            drop(client);
            let targets = match resp {
                Ok(Some(locs)) => reference_locations(&locs),
                Ok(None) => Vec::new(),
                // `{e:#}` prints the full anyhow chain (the underlying JSON-RPC
                // error), not just the "references" context label.
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] references error: {e:#}"));
                    Vec::new()
                }
            };
            log_file::log(&format!(
                "references response id={request_id} server={server_name} count={}",
                targets.len()
            ));
            let _ = tx.send(ReferencesResult {
                request_id,
                path: path_clone,
                targets,
                unsupported: false,
            });
        });
    }

    async fn request_rename(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        new_name: String,
        tx: &std_mpsc::Sender<RenameResult>,
    ) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        // Re-probe so a server installed since the file was opened (e.g. the
        // managed vtsls finishing its lazy background install) is picked up
        // without reopening the file. Cheap once the list is non-empty.
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            return;
        };
        let picked = clients
            .iter()
            .find(|c| c.supports_rename)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            let _ = tx.send(RenameResult {
                request_id,
                path,
                edits: None,
            });
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "rename request id={request_id} server={server_name} path={} line={line} char={character} new_name={new_name}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.rename(uri, line, character, new_name).await;
            drop(client);
            let edits = match resp {
                Ok(Some(we)) => Some(workspace_edits(&we)),
                Ok(None) => None,
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] rename error: {e}"));
                    None
                }
            };
            log_file::log(&format!(
                "rename response id={request_id} server={server_name} files={}",
                edits.as_ref().map(Vec::len).unwrap_or(0)
            ));
            let _ = tx.send(RenameResult {
                request_id,
                path: path_clone,
                edits,
            });
        });
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
    }
}

fn into_item(item: lsp_types::CompletionItem) -> CompletionItem {
    CompletionItem {
        label: item.label,
        detail: item.detail,
        insert_text: item.insert_text,
        filter_text: item.filter_text,
        kind: item.kind,
    }
}

fn marked_string_text(m: &MarkedString) -> &str {
    match m {
        MarkedString::String(s) => s,
        MarkedString::LanguageString(ls) => &ls.value,
    }
}

fn hover_text(contents: &HoverContents) -> Option<String> {
    let text = match contents {
        HoverContents::Scalar(m) => marked_string_text(m).to_string(),
        HoverContents::Markup(mc) => mc.value.clone(),
        HoverContents::Array(items) => items
            .iter()
            .map(marked_string_text)
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
    };
    let text = text.trim().to_string();
    if text.is_empty() { None } else { Some(text) }
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

/// Hover capability (`boolean | HoverOptions`), same bare-`false` rule.
fn hover_supported(cap: &Option<HoverProviderCapability>) -> bool {
    match cap {
        Some(HoverProviderCapability::Simple(b)) => *b,
        Some(HoverProviderCapability::Options(_)) => true,
        None => false,
    }
}

/// Declaration capability (`boolean | DeclarationOptions | DeclarationRegistrationOptions`).
/// vtsls sends `declarationProvider: false`, so the bare-`false` arm is what
/// stops croft from calling the unhandled `textDocument/declaration`.
/// Flatten the server's symbol response into a depth-tagged list in document
/// order — the shape the Outline widget renders directly. The modern `Nested`
/// hierarchy recurses (depth grows with nesting); the legacy `Flat` list has no
/// hierarchy, so every symbol sits at depth 0, sorted by position.
fn flatten_symbols(resp: DocumentSymbolResponse) -> Vec<OutlineSymbol> {
    match resp {
        DocumentSymbolResponse::Nested(syms) => {
            let mut out = Vec::new();
            push_nested(&syms, 0, &mut out);
            out
        }
        DocumentSymbolResponse::Flat(infos) => {
            let mut out: Vec<OutlineSymbol> = infos
                .into_iter()
                .map(|info| {
                    let start = info.location.range.start;
                    OutlineSymbol {
                        name: info.name,
                        detail: None,
                        kind: OutlineKind::from_lsp(info.kind),
                        depth: 0,
                        line: start.line,
                        character: start.character,
                        range_start_line: info.location.range.start.line,
                        range_end_line: info.location.range.end.line,
                    }
                })
                .collect();
            out.sort_by_key(|s| (s.line, s.character));
            out
        }
    }
}

fn push_nested(syms: &[DocumentSymbol], depth: u16, out: &mut Vec<OutlineSymbol>) {
    for sym in syms {
        out.push(OutlineSymbol {
            name: sym.name.clone(),
            detail: sym.detail.clone(),
            kind: OutlineKind::from_lsp(sym.kind),
            depth,
            line: sym.selection_range.start.line,
            character: sym.selection_range.start.character,
            range_start_line: sym.range.start.line,
            range_end_line: sym.range.end.line,
        });
        if let Some(children) = &sym.children {
            push_nested(children, depth + 1, out);
        }
    }
}

fn declaration_supported(cap: &Option<DeclarationCapability>) -> bool {
    match cap {
        Some(DeclarationCapability::Simple(b)) => *b,
        Some(DeclarationCapability::RegistrationOptions(_) | DeclarationCapability::Options(_)) => {
            true
        }
        None => false,
    }
}

/// Type-definition capability (`boolean | TypeDefinitionOptions | ...`), same
/// bare-`false` rule as hover/declaration. vtsls, basedpyright, rust-analyzer
/// and gopls all advertise it, so the row is shown for those; a server that
/// omits it (or sends `false`) gets the row hidden instead of a -32601.
fn type_definition_supported(cap: &Option<TypeDefinitionProviderCapability>) -> bool {
    match cap {
        Some(TypeDefinitionProviderCapability::Simple(b)) => *b,
        Some(TypeDefinitionProviderCapability::Options(_)) => true,
        None => false,
    }
}

/// Implementation capability (`boolean | ImplementationOptions | ...`), same
/// bare-`false` rule as hover/declaration/type-definition. rust-analyzer, gopls
/// and vtsls advertise it; a server that omits it (or sends `false`) gets the
/// "Go to Implementations" row hidden instead of a -32601.
fn implementation_supported(cap: &Option<ImplementationProviderCapability>) -> bool {
    match cap {
        Some(ImplementationProviderCapability::Simple(b)) => *b,
        Some(ImplementationProviderCapability::Options(_)) => true,
        None => false,
    }
}

fn def_location(resp: &GotoDefinitionResponse) -> Option<(PathBuf, u32, u32)> {
    let (uri, pos) = match resp {
        GotoDefinitionResponse::Scalar(loc) => (&loc.uri, loc.range.start),
        GotoDefinitionResponse::Array(locs) => {
            let loc = locs.first()?;
            (&loc.uri, loc.range.start)
        }
        GotoDefinitionResponse::Link(links) => {
            let link = links.first()?;
            (&link.target_uri, link.target_selection_range.start)
        }
    };
    let path = uri.to_file_path().ok()?;
    Some((path, pos.line, pos.character))
}

/// Every location in a goto response, not just the first. Used by Go to
/// Implementations, whose response is commonly an `Array` of every implementor
/// (a Scalar collapses to one element). Locations whose URI is not a local file
/// path are dropped.
fn def_locations(resp: &GotoDefinitionResponse) -> Vec<(PathBuf, u32, u32)> {
    let raw: Vec<(&Url, Position)> = match resp {
        GotoDefinitionResponse::Scalar(loc) => vec![(&loc.uri, loc.range.start)],
        GotoDefinitionResponse::Array(locs) => {
            locs.iter().map(|l| (&l.uri, l.range.start)).collect()
        }
        GotoDefinitionResponse::Link(links) => links
            .iter()
            .map(|l| (&l.target_uri, l.target_selection_range.start))
            .collect(),
    };
    raw.into_iter()
        .filter_map(|(uri, pos)| Some((uri.to_file_path().ok()?, pos.line, pos.character)))
        .collect()
}

/// Every reference location the server returned, as local-file targets. Unlike
/// the goto family, `textDocument/references` replies with a flat `Vec<Location>`
/// (not a `GotoDefinitionResponse`), so it gets its own mapper rather than going
/// through `def_locations`. Locations whose URI is not a local file path are
/// dropped (the same rule `def_locations` applies).
fn reference_locations(locs: &[Location]) -> Vec<(PathBuf, u32, u32)> {
    locs.iter()
        .filter_map(|l| {
            Some((
                l.uri.to_file_path().ok()?,
                l.range.start.line,
                l.range.start.character,
            ))
        })
        .collect()
}

/// Extract a server's semantic-token legend (token-type names in index
/// order) from its advertised capabilities, but only when it supports
/// full-document requests, since that is the only variant croft issues.
fn semantic_legend_of(caps: &ServerCapabilities) -> Option<Vec<String>> {
    let opts = match caps.semantic_tokens_provider.as_ref()? {
        SemanticTokensServerCapabilities::SemanticTokensOptions(o) => o,
        SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(o) => {
            &o.semantic_tokens_options
        }
    };
    let full_ok = matches!(
        opts.full,
        Some(SemanticTokensFullOptions::Bool(true)) | Some(SemanticTokensFullOptions::Delta { .. })
    );
    if !full_ok {
        return None;
    }
    Some(
        opts.legend
            .token_types
            .iter()
            .map(|t| t.as_str().to_string())
            .collect(),
    )
}

/// Whether the server's `semanticTokensProvider` advertises range support
/// (`textDocument/semanticTokens/range`). A server that supports range is a
/// purpose-built incremental highlighter (ty), which is preferred for
/// semantic tokens over a full-only provider (basedpyright) that pays a slow
/// whole-workspace enumeration before its first response.
fn semantic_tokens_range_supported(caps: &ServerCapabilities) -> bool {
    let opts = match caps.semantic_tokens_provider.as_ref() {
        Some(SemanticTokensServerCapabilities::SemanticTokensOptions(o)) => o,
        Some(SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(o)) => {
            &o.semantic_tokens_options
        }
        None => return false,
    };
    matches!(opts.range, Some(true))
}

/// Flatten lsp-types `SemanticToken` structs back into the raw
/// relative-encoded `[deltaLine, deltaStart, length, type, modifiers]`
/// u32 array that `highlight::decode_semantic_tokens` consumes.
fn flatten_semantic_tokens(tokens: &[lsp_types::SemanticToken]) -> Vec<u32> {
    let mut out = Vec::with_capacity(tokens.len() * 5);
    for t in tokens {
        out.push(t.delta_line);
        out.push(t.delta_start);
        out.push(t.length);
        out.push(t.token_type);
        out.push(t.token_modifiers_bitset);
    }
    out
}

/// The standard LSP semantic token types croft can render. Declared so
/// servers know which types we understand; they reply with a legend that
/// is a subset of this set.
fn standard_semantic_token_types() -> Vec<SemanticTokenType> {
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

fn standard_semantic_token_modifiers() -> Vec<SemanticTokenModifier> {
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

fn build_client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
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
            // Declare push-diagnostics support. Several servers gate
            // `textDocument/publishDiagnostics` on the client advertising this
            // (ty and ruff push regardless, but vtsls stays silent without it);
            // declaring it is what every real LSP client (VS Code, Neovim) does.
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                related_information: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        // Advertise that croft can re-pull tokens on a server's request. ty
        // does not currently send `workspace/semanticTokens/refresh` (croft
        // covers the cold-start gap with its own empty-response retry instead),
        // but declaring support is correct and future-proofs the path for
        // servers that do.
        workspace: Some(WorkspaceClientCapabilities {
            semantic_tokens: Some(SemanticTokensWorkspaceClientCapabilities {
                refresh_support: Some(true),
            }),
            ..Default::default()
        }),
        // Declare `window.workDoneProgress` so servers stream `$/progress`
        // (rust-analyzer's "Indexing…", "Building CrateGraph", "Roots
        // Scanned"). The router forwards it to the status bar, so a server
        // that is busy priming is never silently mistaken for a dead one.
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn path_to_language(path: &Path) -> Option<Language> {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(Language::from_extension)
}

/// Project-manifest filenames whose presence marks a directory as the root of a
/// `lang` project. Used to anchor the language server at the right sub-project
/// (so basedpyright finds that project's `.venv`, rust-analyzer its `Cargo.toml`,
/// etc.). An empty slice means "no per-project rooting" → use the workspace root.
fn manifest_names(lang: Language) -> &'static [&'static str] {
    lang.root_markers()
}

/// The project root a file's language server should be anchored at: the nearest
/// ancestor holding a language manifest (e.g. `pyproject.toml`, `Cargo.toml`,
/// `go.mod`, `package.json`), bounded by croft's workspace root. Falls back to
/// the workspace root when none is found, so a plain single-project workspace
/// behaves exactly as before.
fn project_root_for(path: &Path, lang: Language, workspace_root: &Path) -> PathBuf {
    let manifests = manifest_names(lang);
    if !manifests.is_empty() {
        let mut dir = path.parent();
        while let Some(d) = dir {
            if !d.starts_with(workspace_root) {
                break;
            }
            if manifests.iter().any(|m| d.join(m).exists()) {
                return d.to_path_buf();
            }
            if d == workspace_root {
                break;
            }
            dir = d.parent();
        }
    }
    workspace_root.to_path_buf()
}

/// Walk `$PATH` looking for an executable file named `cmd`. Avoids
/// invoking the binary with `--help` or `--version` because LSP servers
/// are JSON-RPC daemons with no consistent CLI flags (basedpyright-
/// langserver exits 1 on --help and crashes on --version, rustup shims
/// pretend to exist even when their component isn't installed).
/// Resolve a server config to a spawnable command, or `None` to skip it.
/// A server carrying a `provision` is handled by croft's managed installer
/// (resolved to an absolute path under `~/.croft/servers`; a lazy background
/// install is kicked off when it's absent). Every other server is resolved
/// against PATH unchanged. `log_skip` gates the "not available" log so empty
/// re-probes don't spam the log on every request.
fn resolve_config(config: &ServerConfig, log_skip: bool) -> Option<(ServerConfig, Vec<PathBuf>)> {
    if let Some(provision) = &config.provision {
        return crate::lsp::install::resolve_managed(config, provision, log_skip);
    }
    if let Some(resolved) = resolve_path_only(
        config,
        is_on_path(&config.command),
        &toolchain_fallback_dirs(),
    ) {
        return Some(resolved);
    }
    if log_skip {
        log_file::log(&format!(
            "lsp[{}] skip: `{}` not on PATH",
            config.name, config.command
        ));
    }
    None
}

pub(crate) fn is_on_path(cmd: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|entry| is_executable_file(&entry.join(cmd)))
}

/// True when `path` is a regular file with at least one execute bit set. On
/// non-unix any existing file counts (no mode bits to inspect).
fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Toolchain `bin` dirs that a macOS GUI launch (Finder / Dock / the Croft.app
/// bundle, which `open`s Ghostty under the stripped launchd PATH) drops because
/// the login-shell profile is never sourced. rust-analyzer and gopls live here
/// but resolve by bare name, so a GUI-launched croft can't find them even
/// though a terminal launch (full PATH) can. Probed only when the command is
/// already absent from the inherited PATH, so a normal launch never reaches it,
/// which keeps local and remote behaviour identical.
fn toolchain_fallback_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".cargo").join("bin"));
        dirs.push(home.join("go").join("bin"));
        dirs.push(home.join(".local").join("bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// Absolute path to `cmd` in the first `dirs` entry that holds an executable
/// file of that name, or `None`.
fn find_executable_in(cmd: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(cmd))
        .find(|candidate| is_executable_file(candidate))
}

/// Resolve a PATH-only server (no managed provision) to a spawnable
/// `(config, extra_path)` pair, or `None` to skip it. When `on_path` the
/// bare command spawns as-is; otherwise the toolchain `fallback_dirs` are
/// probed and a hit is pinned to its absolute path (with its dir prepended to
/// the child's PATH) so a stripped GUI PATH can't hide it.
fn resolve_path_only(
    config: &ServerConfig,
    on_path: bool,
    fallback_dirs: &[PathBuf],
) -> Option<(ServerConfig, Vec<PathBuf>)> {
    if on_path {
        return Some((config.clone(), Vec::new()));
    }
    let abs = find_executable_in(&config.command, fallback_dirs)?;
    let extra = abs.parent().map(Path::to_path_buf).into_iter().collect();
    let mut resolved = config.clone();
    resolved.command = abs.to_string_lossy().into_owned();
    Some((resolved, extra))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        LanguageString, Location, LocationLink, MarkupContent, SemanticTokensLegend,
        SemanticTokensOptions,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn client_capabilities_advertise_work_done_progress() {
        // Servers only emit `$/progress` (rust-analyzer's "Indexing…") when the
        // client declares `window.workDoneProgress`. Without this the status bar
        // could never show indexing state.
        let caps = build_client_capabilities();
        let window = caps.window.expect("window capabilities must be set");
        assert_eq!(window.work_done_progress, Some(true));
    }

    fn def_range(line: u32, ch: u32) -> lsp_types::Range {
        let p = lsp_types::Position {
            line,
            character: ch,
        };
        lsp_types::Range { start: p, end: p }
    }

    #[test]
    fn resolve_config_skips_a_server_not_on_path_and_keeps_one_that_is() {
        let absent = ServerConfig {
            name: "fake",
            command: "definitely-not-a-real-binary-zzzqqq".into(),
            args: vec![],
            language: Language::GO,
            initialization_options: None,
            provision: None,
        };
        assert!(
            resolve_config(&absent, false).is_none(),
            "a server whose command isn't on PATH must be skipped"
        );
        let present = ServerConfig {
            name: "shell",
            command: "sh".into(),
            args: vec![],
            language: Language::BASH,
            initialization_options: None,
            provision: None,
        };
        assert!(
            resolve_config(&present, false).is_some(),
            "a server whose command is on PATH must resolve"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_path_only_pins_a_fallback_dir_binary_to_its_absolute_path() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let bin = dir.join("rust-analyzer");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = ServerConfig {
            name: "rust-analyzer",
            command: "rust-analyzer".into(),
            args: vec![],
            language: Language::RUST,
            initialization_options: None,
            provision: None,
        };

        // Not on PATH, but present in a fallback dir: must resolve to the
        // ABSOLUTE binary path (so a GUI-launched croft with a stripped PATH
        // still spawns it) and prepend that dir to the child's PATH.
        let (resolved, extra) = resolve_path_only(&cfg, false, std::slice::from_ref(&dir))
            .expect("a binary in a fallback dir must resolve");
        assert_eq!(resolved.command, bin.to_string_lossy());
        assert_eq!(extra, vec![dir.clone()]);

        // Already on PATH: spawn the bare command, no extra PATH entries.
        let (resolved, extra) = resolve_path_only(&cfg, true, std::slice::from_ref(&dir))
            .expect("an on-PATH server must resolve");
        assert_eq!(resolved.command, "rust-analyzer");
        assert!(extra.is_empty());

        // Neither on PATH nor in any fallback dir: skip.
        assert!(
            resolve_path_only(&cfg, false, &[]).is_none(),
            "a server missing everywhere must be skipped"
        );
    }

    #[test]
    fn workspace_edits_reads_changes_map() {
        let uri = Url::from_file_path("/tmp/foo.rs").unwrap();
        let mut changes = HashMap::new();
        changes.insert(
            uri,
            vec![TextEdit {
                range: def_range(2, 4),
                new_text: "renamed".to_string(),
            }],
        );
        let we = WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        };
        let out = workspace_edits(&we);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, PathBuf::from("/tmp/foo.rs"));
        assert_eq!(out[0].1.len(), 1);
        assert_eq!(out[0].1[0].new_text, "renamed");
        assert_eq!(out[0].1[0].start, (2, 4));
    }

    #[test]
    fn project_root_is_nearest_manifest_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // monorepo: root/app/svc is a Python sub-project with its own pyproject.
        let svc = root.join("app").join("svc");
        std::fs::create_dir_all(svc.join("src/pkg")).unwrap();
        std::fs::write(svc.join("pyproject.toml"), b"[project]\n").unwrap();
        let file = svc.join("src/pkg/mod.py");
        assert_eq!(project_root_for(&file, Language::PYTHON, root), svc);
    }

    #[test]
    fn project_root_falls_back_to_workspace_root_without_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let file = root.join("a/b/loose.py");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        // No pyproject anywhere up the tree → workspace root.
        assert_eq!(project_root_for(&file, Language::PYTHON, root), root);
    }

    #[test]
    fn project_root_ignores_manifest_of_other_languages() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        // A Cargo.toml must not anchor a Python file; Python keeps walking to root.
        std::fs::write(sub.join("Cargo.toml"), b"[package]\n").unwrap();
        let file = sub.join("script.py");
        assert_eq!(project_root_for(&file, Language::PYTHON, root), root);
        // ...but it does anchor a Rust file.
        let rs = sub.join("src/main.rs");
        std::fs::create_dir_all(rs.parent().unwrap()).unwrap();
        assert_eq!(project_root_for(&rs, Language::RUST, root), sub);
    }

    #[test]
    fn declaration_capability_false_reads_as_unsupported() {
        // The bug: vtsls sends `declarationProvider: false`, which lsp-types
        // parses to `Some(DeclarationCapability::Simple(false))`. `Option::is_some`
        // read that as supported and croft called the unhandled method.
        assert!(!declaration_supported(&Some(
            DeclarationCapability::Simple(false)
        )));
        assert!(declaration_supported(&Some(DeclarationCapability::Simple(
            true
        ))));
        assert!(!declaration_supported(&None));
    }

    #[test]
    fn one_of_capability_false_reads_as_unsupported() {
        let off: Option<OneOf<bool, ()>> = Some(OneOf::Left(false));
        let on: Option<OneOf<bool, ()>> = Some(OneOf::Left(true));
        let opts: Option<OneOf<bool, ()>> = Some(OneOf::Right(()));
        assert!(!one_of_supported(&off));
        assert!(one_of_supported(&on));
        assert!(one_of_supported(&opts));
        assert!(!one_of_supported(&None::<OneOf<bool, ()>>));
    }

    #[test]
    fn hover_capability_false_reads_as_unsupported() {
        assert!(!hover_supported(&Some(HoverProviderCapability::Simple(
            false
        ))));
        assert!(hover_supported(&Some(HoverProviderCapability::Simple(
            true
        ))));
        assert!(!hover_supported(&None));
    }

    #[test]
    fn type_definition_capability_false_reads_as_unsupported() {
        assert!(!type_definition_supported(&Some(
            TypeDefinitionProviderCapability::Simple(false)
        )));
        assert!(type_definition_supported(&Some(
            TypeDefinitionProviderCapability::Simple(true)
        )));
        assert!(!type_definition_supported(&None));
    }

    #[test]
    fn implementation_capability_false_reads_as_unsupported() {
        assert!(!implementation_supported(&Some(
            ImplementationProviderCapability::Simple(false)
        )));
        assert!(implementation_supported(&Some(
            ImplementationProviderCapability::Simple(true)
        )));
        assert!(!implementation_supported(&None));
    }

    #[test]
    fn reference_locations_maps_every_location() {
        // Go to References is 1:many: a symbol used in three places comes back
        // as a flat `Vec<Location>` (not a `GotoDefinitionResponse`), and every
        // one must survive the mapping, in order.
        let locs = vec![
            Location {
                uri: Url::from_file_path("/tmp/a.rs").unwrap(),
                range: def_range(2, 4),
            },
            Location {
                uri: Url::from_file_path("/tmp/b.rs").unwrap(),
                range: def_range(7, 1),
            },
            Location {
                uri: Url::from_file_path("/tmp/a.rs").unwrap(),
                range: def_range(11, 0),
            },
        ];
        assert_eq!(
            reference_locations(&locs),
            vec![
                (PathBuf::from("/tmp/a.rs"), 2, 4),
                (PathBuf::from("/tmp/b.rs"), 7, 1),
                (PathBuf::from("/tmp/a.rs"), 11, 0),
            ]
        );
    }

    #[test]
    fn reference_locations_empty_is_empty() {
        assert!(reference_locations(&[]).is_empty());
    }

    #[test]
    fn def_location_reads_scalar() {
        let uri = Url::from_file_path("/tmp/foo.rs").unwrap();
        let resp = GotoDefinitionResponse::Scalar(Location {
            uri,
            range: def_range(3, 5),
        });
        assert_eq!(
            def_location(&resp),
            Some((PathBuf::from("/tmp/foo.rs"), 3, 5))
        );
    }

    #[test]
    fn def_location_reads_first_of_array() {
        let resp = GotoDefinitionResponse::Array(vec![
            Location {
                uri: Url::from_file_path("/tmp/a.rs").unwrap(),
                range: def_range(1, 0),
            },
            Location {
                uri: Url::from_file_path("/tmp/b.rs").unwrap(),
                range: def_range(9, 9),
            },
        ]);
        assert_eq!(
            def_location(&resp),
            Some((PathBuf::from("/tmp/a.rs"), 1, 0))
        );
    }

    #[test]
    fn def_location_reads_link_target_selection_range() {
        let resp = GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: None,
            target_uri: Url::from_file_path("/tmp/c.rs").unwrap(),
            target_range: def_range(10, 0),
            target_selection_range: def_range(12, 4),
        }]);
        assert_eq!(
            def_location(&resp),
            Some((PathBuf::from("/tmp/c.rs"), 12, 4))
        );
    }

    #[test]
    fn def_location_is_none_for_empty_array() {
        let resp = GotoDefinitionResponse::Array(vec![]);
        assert_eq!(def_location(&resp), None);
    }

    #[test]
    fn def_locations_reads_every_entry_of_array() {
        // Go to Implementations is 1:many: a trait with two implementors comes
        // back as an Array of both, and `def_locations` must keep both (whereas
        // `def_location` keeps only the first).
        let resp = GotoDefinitionResponse::Array(vec![
            Location {
                uri: Url::from_file_path("/tmp/a.rs").unwrap(),
                range: def_range(1, 0),
            },
            Location {
                uri: Url::from_file_path("/tmp/b.rs").unwrap(),
                range: def_range(9, 9),
            },
        ]);
        assert_eq!(
            def_locations(&resp),
            vec![
                (PathBuf::from("/tmp/a.rs"), 1, 0),
                (PathBuf::from("/tmp/b.rs"), 9, 9),
            ]
        );
    }

    #[test]
    fn def_locations_collapses_scalar_to_one_entry() {
        let resp = GotoDefinitionResponse::Scalar(Location {
            uri: Url::from_file_path("/tmp/foo.rs").unwrap(),
            range: def_range(3, 5),
        });
        assert_eq!(
            def_locations(&resp),
            vec![(PathBuf::from("/tmp/foo.rs"), 3, 5)]
        );
    }

    #[test]
    fn def_locations_reads_all_links() {
        let resp = GotoDefinitionResponse::Link(vec![
            LocationLink {
                origin_selection_range: None,
                target_uri: Url::from_file_path("/tmp/c.rs").unwrap(),
                target_range: def_range(10, 0),
                target_selection_range: def_range(12, 4),
            },
            LocationLink {
                origin_selection_range: None,
                target_uri: Url::from_file_path("/tmp/d.rs").unwrap(),
                target_range: def_range(20, 0),
                target_selection_range: def_range(22, 2),
            },
        ]);
        assert_eq!(
            def_locations(&resp),
            vec![
                (PathBuf::from("/tmp/c.rs"), 12, 4),
                (PathBuf::from("/tmp/d.rs"), 22, 2),
            ]
        );
    }

    #[test]
    fn hover_text_reads_plain_markup() {
        let c = HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: "fn foo(x: i32) -> i32".into(),
        });
        assert_eq!(hover_text(&c).as_deref(), Some("fn foo(x: i32) -> i32"));
    }

    #[test]
    fn hover_text_reads_scalar_language_string() {
        let c = HoverContents::Scalar(MarkedString::LanguageString(LanguageString {
            language: "rust".into(),
            value: "fn foo()".into(),
        }));
        assert_eq!(hover_text(&c).as_deref(), Some("fn foo()"));
    }

    #[test]
    fn hover_text_reads_scalar_plain_string() {
        let c = HoverContents::Scalar(MarkedString::String("just text".into()));
        assert_eq!(hover_text(&c).as_deref(), Some("just text"));
    }

    #[test]
    fn hover_text_joins_array_entries_and_skips_blanks() {
        let c = HoverContents::Array(vec![
            MarkedString::String("line one".into()),
            MarkedString::String("   ".into()),
            MarkedString::LanguageString(LanguageString {
                language: "rust".into(),
                value: "line two".into(),
            }),
        ]);
        assert_eq!(hover_text(&c).as_deref(), Some("line one\n\nline two"));
    }

    #[test]
    fn hover_text_is_none_when_blank() {
        let c = HoverContents::Markup(MarkupContent {
            kind: MarkupKind::PlainText,
            value: "   ".into(),
        });
        assert_eq!(hover_text(&c), None);
    }

    #[test]
    fn hover_text_is_none_for_empty_array() {
        let c = HoverContents::Array(vec![]);
        assert_eq!(hover_text(&c), None);
    }

    fn drain_completion_blocking(
        manager: &LspManager,
        timeout: Duration,
    ) -> Option<CompletionResult> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(r) = manager.drain_completion() {
                return Some(r);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn any_python_completion_server_on_path() -> bool {
        is_on_path("basedpyright-langserver")
            || is_on_path("pyright-langserver")
            || is_on_path("ty")
    }

    #[test]
    fn manager_open_change_close_pipe_via_ruff() {
        if !is_on_path("ruff") {
            eprintln!("SKIPPED: ruff not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let file = root.join("demo.py");
        std::fs::write(&file, "x = 1\n").expect("write demo");

        let manager = LspManager::new(root).expect("manager");
        manager.open_doc(file.clone(), String::from("x = 1\n"));
        std::thread::sleep(Duration::from_millis(800));
        manager.change_doc(file.clone(), String::from("x = 2\n"));
        std::thread::sleep(Duration::from_millis(800));
        manager.close_doc(file.clone());
        std::thread::sleep(Duration::from_millis(400));
        drop(manager);
    }

    #[test]
    fn manager_completion_against_python_lsp() {
        if !any_python_completion_server_on_path() {
            eprintln!("SKIPPED: no basedpyright/pyright/ty on PATH");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let file = root.join("demo.py");
        let text = String::from("def f(input, num):\n    input_split = input.split()\n    inp\n");
        std::fs::write(&file, &text).expect("write demo");

        let mut manager = LspManager::new(root).expect("manager");
        manager.open_doc(file.clone(), text);
        std::thread::sleep(Duration::from_millis(2500));

        let id = manager.request_completion(file.clone(), 2, 7);
        // 30s: basedpyright is a Node.js process that initialises slowly
        // under heavy parallel `cargo test` load (the suite spawns ~900
        // tests, many of which open PTYs). 10s was reliable in isolation
        // but flaked under that load; 30s leaves plenty of headroom
        // without slowing the happy path (the loop returns the moment
        // the completion arrives).
        let result = drain_completion_blocking(&manager, Duration::from_secs(30))
            .expect("completion arrived");
        assert_eq!(result.request_id, id);
        assert_eq!(result.path, file);
        let labels: Vec<&str> = result.items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.starts_with("input")),
            "expected at least one item starting with `input`, got: {labels:?}"
        );
    }

    fn drain_hover_blocking(manager: &LspManager, timeout: Duration) -> Option<HoverResult> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(r) = manager.drain_hover() {
                return Some(r);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn manager_hover_against_python_lsp() {
        if !any_python_completion_server_on_path() {
            eprintln!("SKIPPED: no basedpyright/pyright/ty on PATH");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let file = root.join("demo.py");
        let text = String::from("def greet(name):\n    return 'hi ' + name\n\ngreet('x')\n");
        std::fs::write(&file, &text).expect("write demo");

        let mut manager = LspManager::new(root).expect("manager");
        manager.open_doc(file.clone(), text);
        std::thread::sleep(Duration::from_millis(2500));

        let id = manager.request_hover(file.clone(), 3, 0);
        let result =
            drain_hover_blocking(&manager, Duration::from_secs(30)).expect("hover arrived");
        assert_eq!(result.request_id, id);
        assert_eq!(result.path, file);
        let hover = result.text.expect("hover text for the greet call");
        assert!(
            hover.contains("greet"),
            "hover over the greet call should name the function, got: {hover:?}"
        );
    }

    fn drain_definition_blocking(
        manager: &LspManager,
        timeout: Duration,
    ) -> Option<DefinitionResult> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(r) = manager.drain_definition() {
                return Some(r);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn manager_definition_against_python_lsp() {
        if !any_python_completion_server_on_path() {
            eprintln!("SKIPPED: no basedpyright/pyright/ty on PATH");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let file = root.join("demo.py");
        let text = String::from("def greet(name):\n    return name\n\ngreet('x')\n");
        std::fs::write(&file, &text).expect("write demo");

        let mut manager = LspManager::new(root).expect("manager");
        manager.open_doc(file.clone(), text);
        std::thread::sleep(Duration::from_millis(2500));

        let id = manager.request_definition(file.clone(), 3, 0);
        let result = drain_definition_blocking(&manager, Duration::from_secs(30))
            .expect("definition arrived");
        assert_eq!(result.request_id, id);
        let (target_path, target_line, _col) = result.target.expect("definition target found");
        assert_eq!(target_path, file, "greet is defined in the same file");
        assert_eq!(target_line, 0, "greet is defined on line 0");
    }

    fn semantic_caps(range: Option<bool>) -> ServerCapabilities {
        ServerCapabilities {
            semantic_tokens_provider: Some(
                SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    legend: SemanticTokensLegend {
                        token_types: vec![SemanticTokenType::PARAMETER],
                        token_modifiers: vec![],
                    },
                    range,
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                    work_done_progress_options: Default::default(),
                }),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn range_support_detected_only_when_advertised() {
        // ty advertises range; basedpyright advertises full-only.
        assert!(semantic_tokens_range_supported(&semantic_caps(Some(true))));
        assert!(!semantic_tokens_range_supported(&semantic_caps(None)));
        assert!(!semantic_tokens_range_supported(&semantic_caps(Some(
            false
        ))));
        assert!(!semantic_tokens_range_supported(
            &ServerCapabilities::default()
        ));
    }

    fn drain_semantic_blocking(
        manager: &LspManager,
        timeout: Duration,
    ) -> Option<SemanticTokensUpdate> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(r) = manager.drain_semantic_tokens() {
                return Some(r);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn manager_semantic_tokens_against_python_lsp() {
        if !any_python_completion_server_on_path() {
            eprintln!("SKIPPED: no basedpyright/pyright/ty on PATH");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let file = root.join("demo.py");
        // `value` is a parameter referenced in the body; tree-sitter alone
        // cannot know the body use is the parameter, the LSP can.
        let text = String::from("def calc(value):\n    return value + 1\n");
        std::fs::write(&file, &text).expect("write demo");

        let manager = LspManager::new(root).expect("manager");
        manager.open_doc(file.clone(), text.clone());
        std::thread::sleep(Duration::from_millis(2500));
        manager.request_semantic_tokens(file.clone());

        let update = drain_semantic_blocking(&manager, Duration::from_secs(30))
            .expect("semantic tokens arrived");
        assert_eq!(update.path, file);
        assert!(
            update.legend.iter().any(|t| t == "parameter"),
            "the server legend should include the `parameter` token type, got: {:?}",
            update.legend
        );
        assert!(!update.data.is_empty(), "expected a non-empty token batch");

        // Decode and confirm the `value` reference in the body (line 1)
        // carries the parameter color, end-to-end through the real server.
        let line_starts = crate::highlight::compute_line_starts(text.as_bytes());
        let spans = crate::highlight::decode_semantic_tokens(
            &update.data,
            &update.legend,
            text.as_bytes(),
            &line_starts,
        );
        let body = "    return value + 1";
        let col = body.find("value").expect("value in body");
        let hit = spans[1]
            .iter()
            .find(|s| s.start <= col && col < s.end)
            .expect("a semantic span should cover the body `value`");
        assert_eq!(
            hit.style.fg,
            Some(ratatui::style::Color::Rgb(0xd0, 0x87, 0x70)),
            "the parameter referenced in the body must carry the parameter color"
        );
    }

    #[test]
    fn manager_semantic_tokens_range_against_python_lsp() {
        // The viewport-range first-paint path: a `semanticTokens/range` query
        // over the opening rows must return tokens and arrive flagged
        // `is_full == false`. Requires a range-capable server (ty); when only
        // full-only basedpyright is present the request yields nothing, so the
        // assertion is gated on ty being on PATH.
        if !is_on_path("ty") {
            eprintln!("SKIPPED: ty (range-capable) not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let file = root.join("range_demo.py");
        let text = String::from("def calc(value):\n    return value + 1\n");
        std::fs::write(&file, &text).expect("write range_demo");

        let manager = LspManager::new(root).expect("manager");
        manager.open_doc(file.clone(), text.clone());
        std::thread::sleep(Duration::from_millis(2500));
        manager.request_semantic_tokens_range(file.clone(), 0, 2);

        let update = drain_semantic_blocking(&manager, Duration::from_secs(30))
            .expect("range semantic tokens arrived");
        assert_eq!(update.path, file);
        assert!(
            !update.is_full,
            "a range reply must be flagged is_full=false so it cannot clobber the full batch"
        );
        assert!(
            !update.data.is_empty(),
            "the viewport range must carry tokens for the visible function"
        );
    }

    // Reproduces the FULL real-app path for a file with a stray lone `\r`
    // (the n-gram.py bug): opened through `Editor::open`, synced the way
    // `sync_lsp` does (`lines.join("\n")`), then the batch applied via
    // `Editor::apply_semantic_tokens`. The lone `\r` is a line break for the
    // LSP but not for Rust's `str::lines`, so before the fix every token
    // below it landed one row off and the body parameter went uncolored.
    #[test]
    fn editor_overlay_aligns_with_lsp_across_a_lone_cr() {
        if !any_python_completion_server_on_path() {
            eprintln!("SKIPPED: no basedpyright/pyright/ty on PATH");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let file = root.join("mixed.py");
        // A lone `\r` (the `\n\r` after line 0) sits above the function, the
        // same shape as the real n-gram.py. Buffer lines after the fix:
        //   0:"x = 1"  1:""  2:"def calc(value):"  3:"    return value + 1"
        let disk = "x = 1\n\rdef calc(value):\n    return value + 1\n";
        std::fs::write(&file, disk).expect("write mixed");

        let mut editor = crate::widgets::editor::Editor::new();
        editor.open(&file).expect("open");
        // The lone CR must split a line so our numbering matches the server's.
        assert_eq!(
            editor.lines_for_test().len(),
            4,
            "lone CR must split a line"
        );
        let sent = editor.lines_for_test().join("\n");

        let manager = LspManager::new(root).expect("manager");
        manager.open_doc(file.clone(), sent);
        std::thread::sleep(Duration::from_millis(2500));
        manager.request_semantic_tokens(file.clone());
        let update = drain_semantic_blocking(&manager, Duration::from_secs(30))
            .expect("semantic tokens arrived");

        // When ty is installed, the range-capable preference must route the
        // request to ty (instant on any workspace), not full-only basedpyright.
        // ty's legend uniquely carries `selfParameter`; basedpyright's does not.
        if is_on_path("ty") {
            assert!(
                update.legend.iter().any(|t| t == "selfParameter"),
                "ty (range-capable) must be preferred for semantic tokens; legend={:?}",
                update.legend
            );
        }

        editor.apply_semantic_tokens(update.path, update.data, update.legend, update.is_full);
        let overlay = editor.semantic_overlay_for_test();
        // The `value` reference in the body is on buffer line 3. It must carry
        // the parameter color there, and NOT bleed onto the wrong row.
        let orange = ratatui::style::Color::Rgb(0xd0, 0x87, 0x70);
        let colored_on = |line: usize| {
            overlay
                .get(line)
                .map(|s| s.iter().any(|sp| sp.style.fg == Some(orange)))
                .unwrap_or(false)
        };
        assert!(
            colored_on(3),
            "body parameter must be colored on its real row (line 3); overlay={overlay:?}"
        );
    }
}
