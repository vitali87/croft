use std::collections::HashMap;
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
    DeclarationCapability, DocumentChangeOperation, DocumentChanges, DocumentSymbol,
    DocumentSymbolClientCapabilities, DocumentSymbolResponse, GotoDefinitionResponse,
    HoverContents, HoverProviderCapability, ImplementationProviderCapability, Location,
    MarkedString, MarkupKind, OneOf, Position, PublishDiagnosticsClientCapabilities,
    SemanticTokenModifier, SemanticTokenType, SemanticTokensClientCapabilities,
    SemanticTokensClientCapabilitiesRequests, SemanticTokensFullOptions, SemanticTokensRangeResult,
    SemanticTokensResult, SemanticTokensServerCapabilities,
    SemanticTokensWorkspaceClientCapabilities, ServerCapabilities, SymbolKind,
    TextDocumentClientCapabilities, TextEdit, TokenFormat, TypeDefinitionProviderCapability, Url,
    WindowClientCapabilities, WorkspaceClientCapabilities, WorkspaceEdit,
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
    /// The `insert_text` is a snippet body (`$1`/`$0` tab stops) rather than
    /// literal text: an LSP item with `insertTextFormat: Snippet`, or a user
    /// snippet injected into the popup. The app expands it through the editor's
    /// snippet engine on accept instead of inserting it verbatim.
    pub is_snippet: bool,
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

/// One callable signature for the Signature Help popup, normalised off the LSP
/// `SignatureInformation` so the widget never touches lsp-types. `active_param`
/// is the (start, end) char range within `label` to bold (the parameter the
/// caret currently sits in), already resolved against the active signature.
#[derive(Clone, Debug)]
pub struct SignatureInfo {
    pub label: String,
    pub active_param: Option<(usize, usize)>,
}

#[derive(Debug)]
pub struct SignatureHelpResult {
    pub request_id: u64,
    pub path: PathBuf,
    pub signatures: Vec<SignatureInfo>,
    pub active_signature: usize,
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
    /// Echo of the edit seq the request was issued at, so the app can drop a
    /// reply that answers an older buffer than the newest request.
    pub seq: u64,
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

/// One `workspace/symbol` hit, mapped to croft's outline vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSymbolItem {
    pub name: String,
    pub kind: OutlineKind,
    pub path: PathBuf,
    pub line: u32,
    pub character: u32,
    /// The enclosing symbol's name, for the dim qualifier in the picker row.
    pub container: Option<String>,
}

pub struct WorkspaceSymbolsResult {
    pub request_id: u64,
    pub symbols: Vec<WorkspaceSymbolItem>,
    /// True when no spawned server advertises a `workspaceSymbolProvider`.
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
    pub path: PathBuf,
    /// The available actions. `None` means the request errored; `Some(vec![])`
    /// means the server ran but offered nothing here. The same shape is reused
    /// for a `codeAction/resolve` reply (a single, now-resolved item).
    pub items: Option<Vec<CodeActionItem>>,
    /// True when no spawned server advertises a usable `codeActionProvider`, so
    /// the app can say "no quick fixes available" distinctly from an empty list.
    pub unsupported: bool,
}

#[derive(Debug)]
pub struct FormatResult {
    pub request_id: u64,
    pub path: PathBuf,
    /// `None` when no server advertises `documentFormattingProvider`, the
    /// server declined, or it returned no edits. Otherwise the char-indexed
    /// spans to apply to the single formatted document. Unlike rename,
    /// `textDocument/formatting` only ever touches the requested file.
    pub edits: Option<Vec<TextSpanEdit>>,
    /// True when no spawned server advertises a usable
    /// `documentFormattingProvider`, so the app can tell "nothing to format"
    /// apart from "no formatter for this language".
    pub unsupported: bool,
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

/// One inlay hint from a language server (`textDocument/inlayHint`),
/// normalised off the LSP wire type: the label is already flattened
/// (label parts joined, padding folded in as spaces, newlines stripped)
/// so the editor splices it into the row verbatim. `character` is LSP
/// UTF-16; the editor converts it to a character column against its own
/// buffer, exactly as it does for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHintItem {
    pub line: u32,
    pub character: u32,
    pub label: String,
}

/// A fresh, complete set of inlay hints for one document. `seq` echoes the
/// edit sequence the request was fired for; the app drops a reply whose seq
/// no longer matches the buffer (the hints were computed against old text).
#[derive(Debug)]
pub struct InlayHintsUpdate {
    pub path: PathBuf,
    pub seq: u64,
    pub hints: Vec<InlayHintItem>,
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
    RequestInlayHints {
        path: PathBuf,
        line_count: u32,
        seq: u64,
    },
    ChangeDoc {
        path: PathBuf,
        text: String,
    },
    SaveDoc {
        path: PathBuf,
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
    RequestRename {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        new_name: String,
    },
    RequestFormatting {
        request_id: u64,
        path: PathBuf,
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
}
type CapabilitySupport = Arc<StdMutex<LangCapabilitySupport>>;

pub struct LspManager {
    cmd_tx: tokio_mpsc::UnboundedSender<Cmd>,
    completion_rx: std_mpsc::Receiver<CompletionResult>,
    signature_help_rx: std_mpsc::Receiver<SignatureHelpResult>,
    hover_rx: std_mpsc::Receiver<HoverResult>,
    def_rx: std_mpsc::Receiver<DefinitionResult>,
    doc_symbols_rx: std_mpsc::Receiver<DocumentSymbolsResult>,
    decl_rx: std_mpsc::Receiver<DeclarationResult>,
    type_def_rx: std_mpsc::Receiver<TypeDefinitionResult>,
    impl_rx: std_mpsc::Receiver<ImplementationResult>,
    ref_rx: std_mpsc::Receiver<ReferencesResult>,
    ws_symbols_rx: std_mpsc::Receiver<WorkspaceSymbolsResult>,
    rename_rx: std_mpsc::Receiver<RenameResult>,
    format_rx: std_mpsc::Receiver<FormatResult>,
    code_action_rx: std_mpsc::Receiver<CodeActionResult>,
    semantic_rx: std_mpsc::Receiver<SemanticTokensUpdate>,
    inlay_rx: std_mpsc::Receiver<InlayHintsUpdate>,
    diagnostics_rx: std_mpsc::Receiver<DiagnosticsUpdate>,
    progress_rx: std_mpsc::Receiver<ProgressUpdate>,
    capability_support: CapabilitySupport,
    semantic_refresh: Arc<AtomicBool>,
    inlay_refresh: Arc<AtomicBool>,
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
        let (signature_help_tx, signature_help_rx) = std_mpsc::channel();
        let (hover_tx, hover_rx) = std_mpsc::channel();
        let (def_tx, def_rx) = std_mpsc::channel();
        let (doc_symbols_tx, doc_symbols_rx) = std_mpsc::channel();
        let (decl_tx, decl_rx) = std_mpsc::channel();
        let (type_def_tx, type_def_rx) = std_mpsc::channel();
        let (impl_tx, impl_rx) = std_mpsc::channel();
        let (ref_tx, ref_rx) = std_mpsc::channel();
        let (ws_symbols_tx, ws_symbols_rx) = std_mpsc::channel();
        let (rename_tx, rename_rx) = std_mpsc::channel();
        let (format_tx, format_rx) = std_mpsc::channel();
        let (code_action_tx, code_action_rx) = std_mpsc::channel();
        let (semantic_tx, semantic_rx) = std_mpsc::channel();
        let (inlay_tx, inlay_rx) = std_mpsc::channel();
        let (diagnostics_tx, diagnostics_rx) = std_mpsc::channel();
        let (progress_tx, progress_rx) = std_mpsc::channel();
        let capability_support: CapabilitySupport =
            Arc::new(StdMutex::new(LangCapabilitySupport::default()));
        let semantic_refresh = Arc::new(AtomicBool::new(false));
        let inlay_refresh = Arc::new(AtomicBool::new(false));
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
        // language. The set is read from prefs at construction; a live toggle
        // takes effect on the next manager rebuild (re-root / relaunch).
        crate::lsp::languages::init_with_user_sources(&user_refs);
        let disabled = crate::prefs::Prefs::load_or_default().disabled_extensions;
        let registry = ServerRegistry::with_user_extensions_filtered(&user_refs, &disabled);
        runtime.handle().spawn(worker_loop(
            root,
            registry,
            cmd_rx,
            ResultSenders {
                completion: completion_tx,
                signature_help: signature_help_tx,
                hover: hover_tx,
                definition: def_tx,
                document_symbols: doc_symbols_tx,
                declaration: decl_tx,
                type_definition: type_def_tx,
                implementation: impl_tx,
                references: ref_tx,
                workspace_symbols: ws_symbols_tx,
                rename: rename_tx,
                formatting: format_tx,
                code_action: code_action_tx,
                semantic_tokens: semantic_tx,
                inlay_hints: inlay_tx,
                diagnostics: diagnostics_tx,
                progress: progress_tx,
            },
            capability_support.clone(),
            semantic_refresh.clone(),
            inlay_refresh.clone(),
        ));
        Ok(Self {
            cmd_tx,
            completion_rx,
            signature_help_rx,
            hover_rx,
            def_rx,
            doc_symbols_rx,
            decl_rx,
            type_def_rx,
            impl_rx,
            ref_rx,
            ws_symbols_rx,
            rename_rx,
            format_rx,
            code_action_rx,
            semantic_rx,
            inlay_rx,
            diagnostics_rx,
            progress_rx,
            capability_support,
            semantic_refresh,
            inlay_refresh,
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

    /// Notify the servers a document was written to disk. rust-analyzer only
    /// re-runs its check-on-save (`cargo check`, the source of most Rust
    /// PROBLEMS entries) on `textDocument/didSave`.
    pub fn save_doc(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(Cmd::SaveDoc { path });
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

    pub fn request_signature_help(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestSignatureHelp {
            request_id: id,
            path,
            line,
            character,
        });
        id
    }

    pub fn drain_signature_help(&self) -> Option<SignatureHelpResult> {
        self.signature_help_rx.try_recv().ok()
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

    /// Ask the server for the whole document's inlay hints. Fire-and-forget
    /// like the semantic-token requests; the reply lands in
    /// [`drain_inlay_hints`] tagged with `seq` so the app can drop a stale
    /// batch. A no-op when no spawned server advertises an `inlayHintProvider`.
    pub fn request_inlay_hints(&self, path: PathBuf, line_count: u32, seq: u64) {
        let _ = self.cmd_tx.send(Cmd::RequestInlayHints {
            path,
            line_count,
            seq,
        });
    }

    pub fn drain_inlay_hints(&self) -> Option<InlayHintsUpdate> {
        self.inlay_rx.try_recv().ok()
    }

    /// Returns and clears the "a server asked us to re-request inlay hints"
    /// flag, set by any client that received `workspace/inlayHint/refresh`.
    /// Mirrors [`take_semantic_refresh`](Self::take_semantic_refresh).
    pub fn take_inlay_refresh(&self) -> bool {
        self.inlay_refresh.swap(false, Ordering::Relaxed)
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
    /// the freshest reply for the active file wins. `seq` is echoed back in
    /// the result so the app can drop a reply answering an older buffer than
    /// the newest request (a stale reply's shifted ranges made the
    /// breadcrumb's symbol crumb flicker while typing on a slow server).
    pub fn request_document_symbols(&self, path: PathBuf, seq: u64) {
        let _ = self.cmd_tx.send(Cmd::RequestDocumentSymbols { path, seq });
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

    /// Fire a `workspace/symbol` query at every running server that
    /// supports it; the merged reply arrives via
    /// [`Self::drain_workspace_symbols`] tagged with the returned id.
    pub fn request_workspace_symbols(&mut self, query: String) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestWorkspaceSymbols {
            request_id: id,
            query,
        });
        id
    }

    pub fn drain_workspace_symbols(&self) -> Option<WorkspaceSymbolsResult> {
        self.ws_symbols_rx.try_recv().ok()
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

    pub fn request_formatting(&mut self, path: PathBuf, tab_size: u32, insert_spaces: bool) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestFormatting {
            request_id: id,
            path,
            tab_size,
            insert_spaces,
        });
        id
    }

    pub fn drain_formatting(&self) -> Option<FormatResult> {
        self.format_rx.try_recv().ok()
    }

    /// Whether the server for `lang` implements `textDocument/formatting`.
    /// `None` means no server has reported yet. Read synchronously by the app
    /// to decide whether to offer "Format Document".
    pub fn language_supports_formatting(&self, lang: Language) -> Option<bool> {
        self.capability_support
            .lock()
            .ok()?
            .formatting
            .get(&lang)
            .copied()
    }

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
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RequestCodeAction {
            request_id: id,
            path,
            start_line,
            start_char,
            end_line,
            end_char,
            diagnostics,
        });
        id
    }

    /// Resolve an action that arrived without its edit (it carried only `data`).
    /// The reply reuses [`CodeActionResult`] with a single, now-resolved item.
    pub fn request_code_action_resolve(
        &mut self,
        path: PathBuf,
        server: String,
        action: serde_json::Value,
    ) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::ResolveCodeAction {
            request_id: id,
            path,
            server,
            action,
        });
        id
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
        self.capability_support
            .lock()
            .ok()?
            .code_action
            .get(&lang)
            .copied()
    }
}

struct ManagedClient {
    name: String,
    client: Arc<TokioMutex<LspClient>>,
    supports_completion: bool,
    supports_signature_help: bool,
    supports_hover: bool,
    supports_definition: bool,
    supports_document_symbol: bool,
    supports_declaration: bool,
    supports_type_definition: bool,
    supports_implementation: bool,
    supports_references: bool,
    supports_workspace_symbols: bool,
    supports_rename: bool,
    supports_formatting: bool,
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
    // Same contract for `workspace/inlayHint/refresh`.
    inlay_refresh: Arc<AtomicBool>,
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
    signature_help: std_mpsc::Sender<SignatureHelpResult>,
    hover: std_mpsc::Sender<HoverResult>,
    definition: std_mpsc::Sender<DefinitionResult>,
    document_symbols: std_mpsc::Sender<DocumentSymbolsResult>,
    declaration: std_mpsc::Sender<DeclarationResult>,
    type_definition: std_mpsc::Sender<TypeDefinitionResult>,
    implementation: std_mpsc::Sender<ImplementationResult>,
    references: std_mpsc::Sender<ReferencesResult>,
    workspace_symbols: std_mpsc::Sender<WorkspaceSymbolsResult>,
    rename: std_mpsc::Sender<RenameResult>,
    formatting: std_mpsc::Sender<FormatResult>,
    code_action: std_mpsc::Sender<CodeActionResult>,
    semantic_tokens: std_mpsc::Sender<SemanticTokensUpdate>,
    inlay_hints: std_mpsc::Sender<InlayHintsUpdate>,
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
    inlay_refresh: Arc<AtomicBool>,
) {
    let mut state = WorkerState {
        workspace_root,
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support,
        semantic_refresh,
        inlay_refresh,
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
            Cmd::RequestInlayHints {
                path,
                line_count,
                seq,
            } => {
                state
                    .request_inlay_hints(path, line_count, seq, &tx.inlay_hints)
                    .await
            }
            Cmd::ChangeDoc { path, text } => state.change_doc(path, text).await,
            Cmd::SaveDoc { path } => state.save_doc(path).await,
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
                    let inlay_refresh = self.inlay_refresh.clone();
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
                        let supports_signature_help =
                            signature_help_supported(&caps.signature_help_provider);
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
                        let supports_workspace_symbols =
                            one_of_supported(&caps.workspace_symbol_provider);
                        let supports_rename = one_of_supported(&caps.rename_provider);
                        let supports_formatting =
                            one_of_supported(&caps.document_formatting_provider);
                        let supports_code_action =
                            code_action_supported(&caps.code_action_provider);
                        let semantic_legend = semantic_legend_of(caps).map(Arc::new);
                        let semantic_supports_range = semantic_tokens_range_supported(caps);
                        let supports_inlay_hints = one_of_supported(&caps.inlay_hint_provider);
                        log_file::log(&format!(
                            "lsp[{}] spawned, root={} supports_completion={supports} supports_signature_help={supports_signature_help} supports_hover={supports_hover} supports_definition={supports_definition} supports_declaration={supports_declaration} supports_type_definition={supports_type_definition} supports_implementation={supports_implementation} supports_references={supports_references} supports_rename={supports_rename}",
                            config.name,
                            root.display()
                        ));
                        spawned.push(ManagedClient {
                            name: config.name.to_string(),
                            client: Arc::new(TokioMutex::new(client)),
                            supports_completion: supports,
                            supports_signature_help,
                            supports_hover,
                            supports_definition,
                            supports_document_symbol,
                            supports_declaration,
                            supports_type_definition,
                            supports_implementation,
                            supports_references,
                            supports_workspace_symbols,
                            supports_rename,
                            supports_formatting,
                            supports_code_action,
                            semantic_legend,
                            semantic_supports_range,
                            supports_inlay_hints,
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
            if let Ok(mut support) = self.capability_support.lock() {
                support.declaration.insert(lang, supports_declaration);
                support
                    .type_definition
                    .insert(lang, supports_type_definition);
                support.implementation.insert(lang, supports_implementation);
                support.references.insert(lang, supports_references);
                support.formatting.insert(lang, supports_formatting);
                support.code_action.insert(lang, supports_code_action);
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

    async fn save_doc(&mut self, path: PathBuf) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
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
            if let Err(e) = client.did_save(uri.clone()) {
                log_file::log(&format!("lsp[{name}] did_save failed: {e}"));
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

    async fn request_signature_help(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<SignatureHelpResult>,
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
        // Every server that advertises the capability, in registration order.
        // Some servers (e.g. Astral's `ty`) advertise `signatureHelpProvider`
        // but return nothing, so we try each in turn and take the FIRST that
        // actually answers, rather than stopping at an empty stub.
        let candidates: Vec<(String, Arc<TokioMutex<LspClient>>)> = clients
            .iter()
            .filter(|c| c.supports_signature_help)
            .map(|c| (c.name.clone(), c.client.clone()))
            .collect();
        if candidates.is_empty() {
            log_file::log(&format!(
                "signatureHelp request id={request_id} dropped: no client advertises signature_help_provider for {lang:?}"
            ));
            let _ = tx.send(SignatureHelpResult {
                request_id,
                path,
                signatures: Vec::new(),
                active_signature: 0,
            });
            return;
        }
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "signatureHelp request id={request_id} servers={:?} line={line} char={character}",
            candidates
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
        ));
        tokio::spawn(async move {
            let mut signatures = Vec::new();
            let mut active_signature = 0;
            for (name, client_arc) in &candidates {
                let mut client = client_arc.lock().await;
                let resp = client.signature_help(uri.clone(), line, character).await;
                drop(client);
                let (sigs, active) = match resp {
                    Ok(Some(help)) => normalise_signature_help(help),
                    Ok(None) => (Vec::new(), 0),
                    Err(e) => {
                        log_file::log(&format!("lsp[{name}] signatureHelp error: {e}"));
                        (Vec::new(), 0)
                    }
                };
                log_file::log(&format!(
                    "signatureHelp response id={request_id} server={name} count={}",
                    sigs.len()
                ));
                if !sigs.is_empty() {
                    signatures = sigs;
                    active_signature = active;
                    break;
                }
            }
            let _ = tx.send(SignatureHelpResult {
                request_id,
                path: path_clone,
                signatures,
                active_signature,
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

    async fn request_inlay_hints(
        &mut self,
        path: PathBuf,
        line_count: u32,
        seq: u64,
        tx: &std_mpsc::Sender<InlayHintsUpdate>,
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
            .find(|c| c.supports_inlay_hints)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.inlay_hints(uri, line_count).await;
            drop(client);
            let hints = match resp {
                Ok(Some(hints)) => hints.into_iter().map(normalise_inlay_hint).collect(),
                Ok(None) => Vec::new(),
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] inlay_hints error: {e}"));
                    Vec::new()
                }
            };
            log_file::log(&format!(
                "inlay_hints response server={server_name} path={} hints={}",
                path.display(),
                hints.len()
            ));
            let _ = tx.send(InlayHintsUpdate { path, seq, hints });
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
        seq: u64,
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
                seq,
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
                seq,
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

    /// `workspace/symbol`: fan the query out to every running server that
    /// advertises the capability (across every language and root — VS Code
    /// merges all providers the same way) and send one merged reply.
    async fn request_workspace_symbols(
        &mut self,
        request_id: u64,
        query: String,
        tx: &std_mpsc::Sender<WorkspaceSymbolsResult>,
    ) {
        let picked: Vec<(String, Arc<TokioMutex<LspClient>>)> = self
            .clients
            .values()
            .flatten()
            .filter(|c| c.supports_workspace_symbols)
            .map(|c| (c.name.clone(), c.client.clone()))
            .collect();
        if picked.is_empty() {
            let _ = tx.send(WorkspaceSymbolsResult {
                request_id,
                symbols: Vec::new(),
                unsupported: true,
            });
            return;
        }
        let tx = tx.clone();
        log_file::log(&format!(
            "workspace symbols request id={request_id} servers={} query={query:?}",
            picked.len()
        ));
        tokio::spawn(async move {
            let mut symbols: Vec<WorkspaceSymbolItem> = Vec::new();
            for (server_name, client_arc) in picked {
                let mut client = client_arc.lock().await;
                let resp = client.workspace_symbols(query.clone()).await;
                drop(client);
                match resp {
                    Ok(Some(r)) => symbols.extend(workspace_symbol_items(r)),
                    Ok(None) => {}
                    Err(e) => {
                        log_file::log(&format!(
                            "lsp[{server_name}] workspace symbols error: {e:#}"
                        ));
                    }
                }
            }
            log_file::log(&format!(
                "workspace symbols response id={request_id} count={}",
                symbols.len()
            ));
            let _ = tx.send(WorkspaceSymbolsResult {
                request_id,
                symbols,
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

    #[allow(clippy::too_many_arguments)]
    async fn request_code_action(
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
                path,
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
        let diags: Vec<lsp_types::Diagnostic> = diagnostics.iter().map(to_lsp_diagnostic).collect();
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "code_action request id={request_id} servers={} path={} line={start_line}",
            servers.len(),
            path.display()
        ));
        tokio::spawn(async move {
            let mut combined: Vec<CodeActionItem> = Vec::new();
            let mut any_ran = false;
            for (server_name, client_arc) in servers {
                let mut client = client_arc.lock().await;
                let resp = client.code_action(uri.clone(), range, diags.clone()).await;
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
                path: path_clone,
                // `None` only when every server errored; an empty Vec means a
                // server answered with nothing.
                items: if any_ran { Some(combined) } else { None },
                unsupported: false,
            });
        });
    }

    async fn resolve_code_action(
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
                path,
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
                path,
                items: None,
                unsupported: true,
            });
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
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
                path: path_clone,
                items,
                unsupported: false,
            });
        });
    }

    async fn execute_command(
        &mut self,
        path: PathBuf,
        command: String,
        arguments: Vec<serde_json::Value>,
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
            .find(|c| c.supports_code_action)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            return;
        };
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            if let Err(e) = client.execute_command(command, arguments).await {
                log_file::log(&format!("lsp[{server_name}] execute_command error: {e:#}"));
            }
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

    async fn request_formatting(
        &mut self,
        request_id: u64,
        path: PathBuf,
        tab_size: u32,
        insert_spaces: bool,
        tx: &std_mpsc::Sender<FormatResult>,
    ) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        // Re-probe so a formatter installed since the file opened is picked up
        // without reopening (mirrors `request_rename`).
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            return;
        };
        let picked = clients
            .iter()
            .find(|c| c.supports_formatting)
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            let _ = tx.send(FormatResult {
                request_id,
                path,
                edits: None,
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
            "formatting request id={request_id} server={server_name} path={} tab_size={tab_size} insert_spaces={insert_spaces}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.formatting(uri, tab_size, insert_spaces).await;
            drop(client);
            let edits = match resp {
                Ok(Some(tes)) => Some(tes.iter().map(text_edit_to_span).collect()),
                Ok(None) => None,
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] formatting error: {e}"));
                    None
                }
            };
            log_file::log(&format!(
                "formatting response id={request_id} server={server_name} edits={}",
                edits.as_ref().map(Vec::len).unwrap_or(0)
            ));
            let _ = tx.send(FormatResult {
                request_id,
                path: path_clone,
                edits,
                unsupported: false,
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

/// Normalise an LSP `textDocument/codeAction` response into croft's selectable
/// [`CodeActionItem`]s. Disabled actions are dropped (VS Code hides them from
/// the lightbulb menu); inline `WorkspaceEdit`s are flattened with the same
/// [`workspace_edits`] path rename uses; an action with neither an edit nor a
/// command but with `data` is flagged for a `codeAction/resolve` round trip.
fn code_action_items(resp: &CodeActionResponse, server: &str) -> Vec<CodeActionItem> {
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
/// so it can ride along as `codeAction` context. We carry range, severity, and
/// message (the fields croft keeps); the server matches its fixes by range.
fn to_lsp_diagnostic(d: &Diagnostic) -> lsp_types::Diagnostic {
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
        ..Default::default()
    }
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
    let is_snippet = item.insert_text_format == Some(lsp_types::InsertTextFormat::SNIPPET);
    CompletionItem {
        label: item.label,
        detail: item.detail,
        insert_text: item.insert_text,
        filter_text: item.filter_text,
        kind: item.kind,
        is_snippet,
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
/// Whether the server advertises a usable `textDocument/codeAction` provider.
/// `Simple(false)` and `None` are unsupported; `Simple(true)` or an options
/// object (which may also declare `resolveProvider`) is supported.
fn code_action_supported(cap: &Option<CodeActionProviderCapability>) -> bool {
    match cap {
        Some(CodeActionProviderCapability::Simple(b)) => *b,
        Some(CodeActionProviderCapability::Options(_)) => true,
        None => false,
    }
}

/// Flatten a `workspace/symbol` response (either shape) into picker rows.
/// Nested `WorkspaceSymbol`s may carry a range-less location (a bare URI);
/// those land on line 0, which is still a useful jump target.
fn workspace_symbol_items(resp: lsp_types::WorkspaceSymbolResponse) -> Vec<WorkspaceSymbolItem> {
    use lsp_types::WorkspaceSymbolResponse;
    let item = |name: String,
                kind: SymbolKind,
                container: Option<String>,
                uri: &lsp_types::Url,
                pos: Option<Position>| {
        let path = uri.to_file_path().ok()?;
        Some(WorkspaceSymbolItem {
            name,
            kind: OutlineKind::from_lsp(kind),
            path,
            line: pos.map(|p| p.line).unwrap_or(0),
            character: pos.map(|p| p.character).unwrap_or(0),
            container,
        })
    };
    match resp {
        WorkspaceSymbolResponse::Flat(list) => list
            .into_iter()
            .filter_map(|si| {
                item(
                    si.name,
                    si.kind,
                    si.container_name,
                    &si.location.uri,
                    Some(si.location.range.start),
                )
            })
            .collect(),
        WorkspaceSymbolResponse::Nested(list) => list
            .into_iter()
            .filter_map(|ws| match ws.location {
                OneOf::Left(loc) => item(
                    ws.name,
                    ws.kind,
                    ws.container_name,
                    &loc.uri,
                    Some(loc.range.start),
                ),
                OneOf::Right(wloc) => item(ws.name, ws.kind, ws.container_name, &wloc.uri, None),
            })
            .collect(),
    }
}

/// Normalise one LSP wire inlay hint: flatten a parts label into one string,
/// fold the padding flags in as literal spaces, and strip newlines (a label
/// is spliced into a single rendered row, where a line break would corrupt
/// the cell run). The editor consumes the label verbatim after this.
fn normalise_inlay_hint(h: lsp_types::InlayHint) -> InlayHintItem {
    let mut label = match h.label {
        lsp_types::InlayHintLabel::String(s) => s,
        lsp_types::InlayHintLabel::LabelParts(parts) => {
            parts.into_iter().map(|p| p.value).collect()
        }
    };
    if label.contains(['\n', '\r']) {
        label = label.replace(['\n', '\r'], " ");
    }
    if h.padding_left == Some(true) {
        label.insert(0, ' ');
    }
    if h.padding_right == Some(true) {
        label.push(' ');
    }
    InlayHintItem {
        line: h.position.line,
        character: h.position.character,
        label,
    }
}

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

/// Whether the server advertises `signatureHelpProvider`.
fn signature_help_supported(cap: &Option<lsp_types::SignatureHelpOptions>) -> bool {
    cap.is_some()
}

/// Flatten an LSP `SignatureHelp` into the widget-facing [`SignatureInfo`]
/// list, resolving each parameter label to a (start, end) char range within
/// the signature label so the popup can bold the active parameter. The active
/// parameter is taken per-signature when set, else from the help-level field.
/// ponytail: parameter label offsets are UTF-16 per the spec; treated as char
/// offsets here, which is exact for ASCII/BMP signatures (the overwhelming
/// common case) and only off for astral-plane identifiers.
fn normalise_signature_help(help: lsp_types::SignatureHelp) -> (Vec<SignatureInfo>, usize) {
    use lsp_types::ParameterLabel;
    let help_active_param = help.active_parameter;
    let signatures: Vec<SignatureInfo> = help
        .signatures
        .into_iter()
        .map(|sig| {
            let active_idx = sig.active_parameter.or(help_active_param).unwrap_or(0) as usize;
            let active_param = sig.parameters.as_ref().and_then(|params| {
                params.get(active_idx).and_then(|p| match &p.label {
                    ParameterLabel::LabelOffsets([s, e]) => Some((*s as usize, *e as usize)),
                    ParameterLabel::Simple(text) => sig.label.find(text.as_str()).map(|byte| {
                        let start = sig.label[..byte].chars().count();
                        (start, start + text.chars().count())
                    }),
                })
            });
            SignatureInfo {
                label: sig.label,
                active_param,
            }
        })
        .collect();
    let active_signature =
        (help.active_signature.unwrap_or(0) as usize).min(signatures.len().saturating_sub(1));
    (signatures, active_signature)
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
            // Declare push-diagnostics support. Several servers gate
            // `textDocument/publishDiagnostics` on the client advertising this
            // (ty and ruff push regardless, but vtsls stays silent without it);
            // declaring it is what every real LSP client (VS Code, Neovim) does.
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                related_information: Some(true),
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
        // Native-binary servers (rust-analyzer, clangd, taplo) are toolchain
        // tools: a real copy on PATH or in a toolchain dir (~/.cargo/bin) must
        // win over a croft-downloaded one so it matches the user's compiler, and
        // for rust-analyzer `resolve_path_only` prepends that dir to the child
        // PATH so the server can find cargo/rustc under a stripped GUI launch.
        // Only when no real binary exists anywhere do we fall back to a managed
        // download / Termux `pkg` install. npm/uv servers keep their own
        // managed-first (vtsls) or PATH-first (ty/ruff) ordering inside
        // `resolve_managed`, so this preference is scoped to the Binary backend.
        if matches!(provision, crate::lsp::install::Provision::Binary { .. })
            && let Some(resolved) = resolve_path_only(
                config,
                is_on_path(&config.command),
                &toolchain_fallback_dirs(),
            )
        {
            return Some(resolved);
        }
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
    fn normalise_inlay_hint_flattens_parts_and_folds_padding() {
        // vtsls sends parts labels; rust-analyzer sends plain strings with
        // padding flags. Both must land as one splice-ready string.
        let parts = lsp_types::InlayHint {
            position: Position {
                line: 3,
                character: 14,
            },
            label: lsp_types::InlayHintLabel::LabelParts(vec![
                lsp_types::InlayHintLabelPart {
                    value: ": ".into(),
                    ..Default::default()
                },
                lsp_types::InlayHintLabelPart {
                    value: "Vec<String>".into(),
                    ..Default::default()
                },
            ]),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        };
        let item = normalise_inlay_hint(parts);
        assert_eq!(item.line, 3);
        assert_eq!(item.character, 14);
        assert_eq!(item.label, ": Vec<String>");

        let padded = lsp_types::InlayHint {
            position: Position {
                line: 0,
                character: 9,
            },
            label: lsp_types::InlayHintLabel::String("param:".into()),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: Some(true),
            padding_right: Some(true),
            data: None,
        };
        assert_eq!(normalise_inlay_hint(padded).label, " param: ");

        let multiline = lsp_types::InlayHint {
            position: Position {
                line: 0,
                character: 0,
            },
            label: lsp_types::InlayHintLabel::String("a\nb".into()),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        };
        assert_eq!(
            normalise_inlay_hint(multiline).label,
            "a b",
            "a newline would corrupt the spliced row"
        );
    }

    #[test]
    fn workspace_symbol_items_maps_the_flat_response() {
        use lsp_types::{SymbolInformation, SymbolKind, Url, WorkspaceSymbolResponse};
        #[allow(deprecated)]
        let resp = WorkspaceSymbolResponse::Flat(vec![SymbolInformation {
            name: "main".into(),
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            location: Location {
                uri: Url::from_file_path("/w/src/main.rs").unwrap(),
                range: lsp_types::Range {
                    start: Position {
                        line: 4,
                        character: 3,
                    },
                    end: Position {
                        line: 4,
                        character: 7,
                    },
                },
            },
            container_name: Some("app".into()),
        }]);
        let items = workspace_symbol_items(resp);
        assert_eq!(
            items,
            vec![WorkspaceSymbolItem {
                name: "main".into(),
                kind: OutlineKind::Function,
                path: PathBuf::from("/w/src/main.rs"),
                line: 4,
                character: 3,
                container: Some("app".into()),
            }]
        );
    }

    #[test]
    fn workspace_symbol_items_maps_the_nested_response_and_bare_uris() {
        use lsp_types::{
            SymbolKind, Url, WorkspaceLocation, WorkspaceSymbol, WorkspaceSymbolResponse,
        };
        let resp = WorkspaceSymbolResponse::Nested(vec![
            WorkspaceSymbol {
                name: "Config".into(),
                kind: SymbolKind::STRUCT,
                tags: None,
                container_name: None,
                location: OneOf::Left(Location {
                    uri: Url::from_file_path("/w/src/config.rs").unwrap(),
                    range: lsp_types::Range {
                        start: Position {
                            line: 9,
                            character: 0,
                        },
                        end: Position {
                            line: 9,
                            character: 6,
                        },
                    },
                }),
                data: None,
            },
            WorkspaceSymbol {
                name: "helper".into(),
                kind: SymbolKind::FUNCTION,
                tags: None,
                container_name: None,
                location: OneOf::Right(WorkspaceLocation {
                    uri: Url::from_file_path("/w/src/lib.rs").unwrap(),
                }),
                data: None,
            },
        ]);
        let items = workspace_symbol_items(resp);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, OutlineKind::Struct);
        assert_eq!(items[0].line, 9);
        assert_eq!(
            items[1].path,
            PathBuf::from("/w/src/lib.rs"),
            "a range-less nested location still yields a jump target"
        );
        assert_eq!(items[1].line, 0, "bare-URI locations land on line 0");
    }

    #[test]
    fn normalise_signature_help_resolves_active_param_range() {
        use lsp_types::{
            ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation,
        };
        // Two params; help-level active_parameter = 1 (the second, "b: i32").
        let help = SignatureHelp {
            signatures: vec![SignatureInformation {
                label: "foo(a: i32, b: i32)".to_string(),
                documentation: None,
                parameters: Some(vec![
                    ParameterInformation {
                        label: ParameterLabel::LabelOffsets([4, 10]),
                        documentation: None,
                    },
                    ParameterInformation {
                        label: ParameterLabel::LabelOffsets([12, 18]),
                        documentation: None,
                    },
                ]),
                active_parameter: None,
            }],
            active_signature: Some(0),
            active_parameter: Some(1),
        };
        let (sigs, active) = normalise_signature_help(help);
        assert_eq!(active, 0);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].active_param, Some((12, 18)));
    }

    #[test]
    fn normalise_signature_help_resolves_simple_label_to_range() {
        use lsp_types::{
            ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation,
        };
        let help = SignatureHelp {
            signatures: vec![SignatureInformation {
                label: "foo(a, b)".to_string(),
                documentation: None,
                parameters: Some(vec![ParameterInformation {
                    label: ParameterLabel::Simple("a".to_string()),
                    documentation: None,
                }]),
                active_parameter: None,
            }],
            active_signature: None,
            active_parameter: Some(0),
        };
        let (sigs, _) = normalise_signature_help(help);
        // "a" sits at char index 4 in "foo(a, b)".
        assert_eq!(sigs[0].active_param, Some((4, 5)));
    }

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
    fn code_action_items_skips_disabled_and_normalises_edits() {
        use lsp_types::{CodeAction, CodeActionDisabled, CodeActionOrCommand, Command};
        let uri = Url::from_file_path("/tmp/foo.rs").unwrap();
        let mut changes = HashMap::new();
        changes.insert(
            uri,
            vec![TextEdit {
                range: def_range(0, 0),
                new_text: "import os\n".to_string(),
            }],
        );
        let edit = WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        };
        let resp: CodeActionResponse = vec![
            // A bare command action (no inline edit; runs via executeCommand).
            CodeActionOrCommand::Command(Command {
                title: "Run fix".to_string(),
                command: "fix.run".to_string(),
                arguments: Some(vec![serde_json::json!(1)]),
            }),
            // A CodeAction carrying an inline workspace edit, marked preferred.
            CodeActionOrCommand::CodeAction(CodeAction {
                title: "Add import 'os'".to_string(),
                edit: Some(edit),
                is_preferred: Some(true),
                ..Default::default()
            }),
            // A disabled action: must be filtered out, mirroring VS Code's
            // lightbulb menu which hides disabled actions.
            CodeActionOrCommand::CodeAction(CodeAction {
                title: "Disabled thing".to_string(),
                disabled: Some(CodeActionDisabled {
                    reason: "nope".to_string(),
                }),
                ..Default::default()
            }),
        ];
        let items = code_action_items(&resp, "ruff");
        assert_eq!(items.len(), 2, "the disabled action must be filtered out");
        assert_eq!(items[0].title, "Run fix");
        assert_eq!(
            items[0].server, "ruff",
            "each item is tagged with its server"
        );
        assert!(items[0].edits.is_empty());
        assert_eq!(items[0].command.as_ref().unwrap().command, "fix.run");
        assert!(!items[0].needs_resolve);
        assert_eq!(items[1].title, "Add import 'os'");
        assert!(items[1].is_preferred);
        assert_eq!(items[1].edits.len(), 1);
        assert_eq!(items[1].edits[0].1[0].new_text, "import os\n");
        assert!(!items[1].needs_resolve);
    }

    #[test]
    fn code_action_items_flags_resolve_when_edit_absent() {
        use lsp_types::{CodeAction, CodeActionOrCommand};
        // ty / rust-analyzer style: the action arrives with `data` and no edit;
        // the edit is filled in by a later codeAction/resolve round trip.
        let resp: CodeActionResponse = vec![CodeActionOrCommand::CodeAction(CodeAction {
            title: "Import symbol".to_string(),
            data: Some(serde_json::json!({"id": 7})),
            ..Default::default()
        })];
        let items = code_action_items(&resp, "basedpyright");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].server, "basedpyright");
        assert!(items[0].edits.is_empty());
        assert!(items[0].command.is_none());
        assert!(
            items[0].needs_resolve,
            "an action with data but no edit/command must be marked for resolve"
        );
    }

    #[test]
    fn client_advertises_code_action_literal_and_resolve_support() {
        // Servers like ruff only return rich CodeAction objects (kinds such as
        // source.organizeImports / source.fixAll, plus resolvable edits) when the
        // client advertises codeActionLiteralSupport + resolveSupport. Without it
        // ty/ruff answer codeAction with nothing useful, so "Organize Imports"
        // never appears.
        let caps = build_client_capabilities();
        let ca = caps
            .text_document
            .and_then(|td| td.code_action)
            .expect("code action client capability must be declared");
        let kinds = ca
            .code_action_literal_support
            .expect("codeActionLiteralSupport must be declared")
            .code_action_kind
            .value_set;
        assert!(
            kinds.iter().any(|k| k == "source.organizeImports"),
            "the advertised kinds must include source.organizeImports"
        );
        assert!(
            kinds.iter().any(|k| k == "quickfix"),
            "the advertised kinds must include quickfix"
        );
        let resolves = ca
            .resolve_support
            .expect("resolveSupport must be declared so deferred edits resolve")
            .properties;
        assert!(resolves.iter().any(|p| p == "edit"));
        assert_eq!(ca.data_support, Some(true));
    }

    #[test]
    fn code_action_capability_detection() {
        use lsp_types::{CodeActionOptions, CodeActionProviderCapability};
        assert!(
            !code_action_supported(&None),
            "absent provider is unsupported"
        );
        assert!(
            !code_action_supported(&Some(CodeActionProviderCapability::Simple(false))),
            "a bare false is unsupported"
        );
        assert!(code_action_supported(&Some(
            CodeActionProviderCapability::Simple(true)
        )));
        assert!(
            code_action_supported(&Some(CodeActionProviderCapability::Options(
                CodeActionOptions::default()
            ))),
            "an options object means supported"
        );
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

    /// Minimal LSP server that answers `initialize` and appends every
    /// incoming method name to the log file given as argv[1]. Lets a test
    /// assert exactly which notifications croft put on the wire.
    const FAKE_LSP_RECORDER: &str = r#"
import json, sys

log = open(sys.argv[1], "a")

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method", "")
    log.write(method + "\n")
    log.flush()
    if "id" in msg:
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {}}})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    if method == "exit":
        break
"#;

    /// Issue #37: rust-analyzer re-runs its check-on-save (`cargo check`, the
    /// source of the Rust PROBLEMS entries) only when the client sends
    /// `textDocument/didSave`. Saving a file must therefore emit didSave, or
    /// the PROBLEMS panel goes permanently stale after the first open.
    #[test]
    fn save_doc_sends_did_save_notification_to_the_server() {
        if !is_on_path("python3") {
            eprintln!("SKIPPED: python3 not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let file = root.join("demo.py");
        std::fs::write(&file, "x = 1\n").expect("write demo");
        let script = root.join("fake_lsp.py");
        std::fs::write(&script, FAKE_LSP_RECORDER).expect("write fake server");
        let log = root.join("methods.log");

        let mut registry = ServerRegistry::new();
        registry.register(
            Language::PYTHON,
            ServerConfig {
                name: "fake-recorder",
                command: "python3".into(),
                args: vec![script.display().to_string(), log.display().to_string()],
                language: Language::PYTHON,
                initialization_options: None,
                provision: None,
            },
        );
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let mut state = WorkerState {
            workspace_root: root.clone(),
            registry,
            clients: HashMap::new(),
            docs: HashMap::new(),
            capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
            semantic_refresh: Arc::new(AtomicBool::new(false)),
            inlay_refresh: Arc::new(AtomicBool::new(false)),
            diagnostics_tx: diag_tx,
            progress_tx: prog_tx,
        };

        let runtime = LspRuntime::new().expect("runtime");
        runtime.handle().clone().block_on(async {
            state.open_doc(file.clone(), String::from("x = 1\n")).await;
            state.save_doc(file.clone()).await;
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        let methods = loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if text.contains("textDocument/didSave") || Instant::now() >= deadline {
                break text;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            methods.contains("textDocument/didOpen"),
            "sanity: the fake server must have received didOpen; got: {methods:?}"
        );
        assert!(
            methods.contains("textDocument/didSave"),
            "saving a document must send textDocument/didSave; server received: {methods:?}"
        );
        runtime.handle().clone().block_on(state.shutdown_all());
    }

    /// A fake server that advertises `inlayHintProvider` and answers
    /// `textDocument/inlayHint` with one plain-string hint (with padding) and
    /// one label-parts hint, covering both wire shapes end to end.
    const FAKE_LSP_INLAY: &str = r#"
import json, sys

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method", "")
    if "id" in msg:
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "result": {"capabilities": {"inlayHintProvider": True}}})
        elif method == "textDocument/inlayHint":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": [
                {"position": {"line": 0, "character": 5}, "label": ": int"},
                {"position": {"line": 0, "character": 9},
                 "label": [{"value": "n"}, {"value": ":"}],
                 "paddingRight": True},
            ]})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    if method == "exit":
        break
"#;

    /// The full worker wire path: capability gate, `textDocument/inlayHint`
    /// request, label normalisation, and the seq-tagged reply on the drain
    /// channel.
    #[test]
    fn request_inlay_hints_round_trips_through_a_hinting_server() {
        if !is_on_path("python3") {
            eprintln!("SKIPPED: python3 not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let file = root.join("demo.py");
        std::fs::write(&file, "x = f(1)\n").expect("write demo");
        let script = root.join("fake_inlay_lsp.py");
        std::fs::write(&script, FAKE_LSP_INLAY).expect("write fake server");

        let mut registry = ServerRegistry::new();
        registry.register(
            Language::PYTHON,
            ServerConfig {
                name: "fake-inlay",
                command: "python3".into(),
                args: vec![script.display().to_string()],
                language: Language::PYTHON,
                initialization_options: None,
                provision: None,
            },
        );
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let mut state = WorkerState {
            workspace_root: root.clone(),
            registry,
            clients: HashMap::new(),
            docs: HashMap::new(),
            capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
            semantic_refresh: Arc::new(AtomicBool::new(false)),
            inlay_refresh: Arc::new(AtomicBool::new(false)),
            diagnostics_tx: diag_tx,
            progress_tx: prog_tx,
        };

        let (tx, rx) = std_mpsc::channel();
        let runtime = LspRuntime::new().expect("runtime");
        runtime.handle().clone().block_on(async {
            state
                .open_doc(file.clone(), String::from("x = f(1)\n"))
                .await;
            state.request_inlay_hints(file.clone(), 2, 42, &tx).await;
        });

        let update = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("an inlay-hint reply must reach the drain channel");
        assert_eq!(update.path, file);
        assert_eq!(update.seq, 42, "the reply must echo the request's seq");
        assert_eq!(
            update.hints,
            vec![
                InlayHintItem {
                    line: 0,
                    character: 5,
                    label: String::from(": int"),
                },
                InlayHintItem {
                    line: 0,
                    character: 9,
                    label: String::from("n: "),
                },
            ],
            "both label shapes must normalise, padding folded in"
        );
        runtime.handle().clone().block_on(state.shutdown_all());
    }
}
