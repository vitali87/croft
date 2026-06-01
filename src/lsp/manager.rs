use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;

use anyhow::Result;
use lsp_types::{
    ClientCapabilities, CompletionClientCapabilities, CompletionItemCapability, CompletionItemKind,
    CompletionItemKindCapability, CompletionResponse, DocumentChangeOperation, DocumentChanges,
    GotoDefinitionResponse, HoverContents, MarkedString, MarkupKind, OneOf,
    TextDocumentClientCapabilities, TextEdit, Url, WorkspaceEdit,
};
use tokio::sync::{Mutex as TokioMutex, mpsc as tokio_mpsc};

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

#[derive(Debug)]
pub struct RenameResult {
    pub request_id: u64,
    pub path: PathBuf,
    /// `None` when the server declined or returned no edits. Otherwise the
    /// per-file, char-indexed spans to apply across the workspace.
    pub edits: Option<Vec<(PathBuf, Vec<TextSpanEdit>)>>,
}

enum Cmd {
    OpenDoc {
        path: PathBuf,
        text: String,
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
    RequestRename {
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        new_name: String,
    },
}

pub struct LspManager {
    cmd_tx: tokio_mpsc::UnboundedSender<Cmd>,
    completion_rx: std_mpsc::Receiver<CompletionResult>,
    hover_rx: std_mpsc::Receiver<HoverResult>,
    def_rx: std_mpsc::Receiver<DefinitionResult>,
    rename_rx: std_mpsc::Receiver<RenameResult>,
    next_request_id: u64,
    workspace_root: PathBuf,
    _runtime: LspRuntime,
}

impl LspManager {
    pub fn new(workspace_root: PathBuf) -> Result<Self> {
        let runtime = LspRuntime::new()?;
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let (completion_tx, completion_rx) = std_mpsc::channel();
        let (hover_tx, hover_rx) = std_mpsc::channel();
        let (def_tx, def_rx) = std_mpsc::channel();
        let (rename_tx, rename_rx) = std_mpsc::channel();
        let root = workspace_root.clone();
        runtime.handle().spawn(worker_loop(
            root,
            ServerRegistry::with_defaults(),
            cmd_rx,
            completion_tx,
            hover_tx,
            def_tx,
            rename_tx,
        ));
        Ok(Self {
            cmd_tx,
            completion_rx,
            hover_rx,
            def_rx,
            rename_rx,
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
    supports_rename: bool,
}

struct WorkerState {
    workspace_root: PathBuf,
    registry: ServerRegistry,
    clients: HashMap<Language, Vec<ManagedClient>>,
    docs: HashMap<PathBuf, DocState>,
}

struct DocState {
    language: Language,
    version: i32,
}

async fn worker_loop(
    workspace_root: PathBuf,
    registry: ServerRegistry,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Cmd>,
    completion_tx: std_mpsc::Sender<CompletionResult>,
    hover_tx: std_mpsc::Sender<HoverResult>,
    def_tx: std_mpsc::Sender<DefinitionResult>,
    rename_tx: std_mpsc::Sender<RenameResult>,
) {
    let mut state = WorkerState {
        workspace_root,
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
    };
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Cmd::OpenDoc { path, text } => state.open_doc(path, text).await,
            Cmd::ChangeDoc { path, text } => state.change_doc(path, text).await,
            Cmd::CloseDoc { path } => state.close_doc(path).await,
            Cmd::RequestCompletion {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_completion(request_id, path, line, character, &completion_tx)
                    .await
            }
            Cmd::RequestHover {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_hover(request_id, path, line, character, &hover_tx)
                    .await
            }
            Cmd::RequestDefinition {
                request_id,
                path,
                line,
                character,
            } => {
                state
                    .request_definition(request_id, path, line, character, &def_tx)
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
                    .request_rename(request_id, path, line, character, new_name, &rename_tx)
                    .await
            }
        }
    }
}

impl WorkerState {
    async fn ensure_clients(&mut self, lang: Language) -> &[ManagedClient] {
        if !self.clients.contains_key(&lang) {
            let configs: Vec<ServerConfig> = self.registry.for_language(lang).to_vec();
            let mut spawned: Vec<ManagedClient> = Vec::new();
            for config in configs.iter() {
                if !is_on_path(&config.command) {
                    log_file::log(&format!(
                        "lsp[{}] skip: `{}` not on PATH",
                        config.name, config.command
                    ));
                    continue;
                }
                match LspClient::spawn(config, &self.workspace_root, build_client_capabilities())
                    .await
                {
                    Ok(client) => {
                        let supports = client.capabilities().completion_provider.is_some();
                        let supports_hover = client.capabilities().hover_provider.is_some();
                        let supports_definition =
                            client.capabilities().definition_provider.is_some();
                        let supports_rename = client.capabilities().rename_provider.is_some();
                        log_file::log(&format!(
                            "lsp[{}] spawned, supports_completion={supports} supports_hover={supports_hover} supports_definition={supports_definition} supports_rename={supports_rename}",
                            config.name
                        ));
                        spawned.push(ManagedClient {
                            name: config.name.to_string(),
                            client: Arc::new(TokioMutex::new(client)),
                            supports_completion: supports,
                            supports_hover,
                            supports_definition,
                            supports_rename,
                        });
                    }
                    Err(e) => {
                        log_file::log(&format!("lsp[{}] spawn failed: {e}", config.name));
                    }
                }
            }
            self.clients.insert(lang, spawned);
        }
        self.clients.get(&lang).map(Vec::as_slice).unwrap_or(&[])
    }

    async fn open_doc(&mut self, path: PathBuf, text: String) {
        let Some(lang) = path_to_language(&path) else {
            return;
        };
        let clients = self.ensure_clients(lang).await;
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
        let lang = doc.language;
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let arcs: Vec<(String, Arc<TokioMutex<LspClient>>)> = match self.clients.get(&lang) {
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
        let arcs: Vec<(String, Arc<TokioMutex<LspClient>>)> = match self.clients.get(&doc.language)
        {
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
        let Some(clients) = self.clients.get(&lang) else {
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
        let Some(clients) = self.clients.get(&lang) else {
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
        let Some(clients) = self.clients.get(&lang) else {
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
                    log_file::log(&format!("lsp[{server_name}] definition error: {e}"));
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
        let Some(clients) = self.clients.get(&lang) else {
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

/// Walk `$PATH` looking for an executable file named `cmd`. Avoids
/// invoking the binary with `--help` or `--version` because LSP servers
/// are JSON-RPC daemons with no consistent CLI flags (basedpyright-
/// langserver exits 1 on --help and crashes on --version, rustup shims
/// pretend to exist even when their component isn't installed).
fn is_on_path(cmd: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{LanguageString, Location, LocationLink, MarkupContent};
    use std::time::{Duration, Instant};

    fn def_range(line: u32, ch: u32) -> lsp_types::Range {
        let p = lsp_types::Position {
            line,
            character: ch,
        };
        lsp_types::Range { start: p, end: p }
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
}
