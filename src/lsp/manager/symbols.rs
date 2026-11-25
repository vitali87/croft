//! Symbols: a document's outline, and workspace symbol search.

use super::*;

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
    pub(super) fn from_lsp(kind: SymbolKind) -> Self {
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
    /// The construct's start column on `range_start_line` and end column on
    /// `range_end_line` (#1221), so symbols sharing a line (a one-line
    /// interface's fields, a minified bundle's functions) are told apart.
    /// `0` / `u32::MAX` when only the lines are known.
    pub range_start_character: u32,
    pub range_end_character: u32,
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

impl LspManager {
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
        self.replies.document_symbols.try_recv().ok()
    }

    /// Fire a `workspace/symbol` query at every running server that
    /// supports it; the merged reply arrives via
    /// [`Self::drain_workspace_symbols`] tagged with the returned id.
    pub fn request_workspace_symbols(&mut self, query: String) -> u64 {
        self.send_request(|request_id| Cmd::RequestWorkspaceSymbols { request_id, query })
    }

    pub fn drain_workspace_symbols(&self) -> Option<WorkspaceSymbolsResult> {
        self.replies.workspace_symbols.try_recv().ok()
    }
}

impl WorkerState {
    pub(super) async fn request_document_symbols(
        &mut self,
        path: PathBuf,
        seq: u64,
        tx: &std_mpsc::Sender<DocumentSymbolsResult>,
    ) {
        let (server_name, client_arc) = match self
            .pick_server(&path, |c| c.supports_document_symbol)
            .await
        {
            Ok(picked) => picked,
            Err(NoServer::Unsupported) => {
                // No symbol provider: send an empty outline so the panel shows
                // "No symbols" rather than a stale tree from a previous file.
                let _ = tx.send(DocumentSymbolsResult {
                    path,
                    seq,
                    symbols: Vec::new(),
                });
                return;
            }
            Err(NoServer::Unknown) => return,
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
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

    /// `workspace/symbol`: fan the query out to every running server that
    /// advertises the capability (across every language and root — VS Code
    /// merges all providers the same way) and send one merged reply.
    pub(super) async fn request_workspace_symbols(
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
                let mut client = client_arc.lock().await.requests();
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
}

/// Flatten a `workspace/symbol` response (either shape) into picker rows.
/// Nested `WorkspaceSymbol`s may carry a range-less location (a bare URI);
/// those land on line 0, which is still a useful jump target.
pub(super) fn workspace_symbol_items(
    resp: lsp_types::WorkspaceSymbolResponse,
) -> Vec<WorkspaceSymbolItem> {
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

/// Declaration capability (`boolean | DeclarationOptions | DeclarationRegistrationOptions`).
/// vtsls sends `declarationProvider: false`, so the bare-`false` arm is what
/// stops croft from calling the unhandled `textDocument/declaration`.
/// Flatten the server's symbol response into a depth-tagged list in document
/// order — the shape the Outline widget renders directly. The modern `Nested`
/// hierarchy recurses (depth grows with nesting); the legacy `Flat` list has no
/// hierarchy, so every symbol sits at depth 0, sorted by position.
pub(super) fn flatten_symbols(resp: DocumentSymbolResponse) -> Vec<OutlineSymbol> {
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
                        range_start_character: info.location.range.start.character,
                        range_end_character: info.location.range.end.character,
                    }
                })
                .collect();
            out.sort_by_key(|s| (s.line, s.character));
            out
        }
    }
}

/// Each level in file order (#1220): tsserver sorts its navigation tree's
/// children by name, so vtsls replies `Cart` before `sum`; VS Code orders by
/// position client-side, and so does this.
pub(super) fn push_nested(syms: &[DocumentSymbol], depth: u16, out: &mut Vec<OutlineSymbol>) {
    let mut syms: Vec<&DocumentSymbol> = syms.iter().collect();
    syms.sort_by_key(|s| {
        (
            s.selection_range.start.line,
            s.selection_range.start.character,
        )
    });
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
            range_start_character: sym.range.start.character,
            range_end_character: sym.range.end.character,
        });
        if let Some(children) = &sym.children {
            push_nested(children, depth + 1, out);
        }
    }
}
