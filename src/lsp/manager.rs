use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;

use anyhow::Result;
use lsp_types::{
    ClientCapabilities, CompletionClientCapabilities, CompletionItemCapability,
    CompletionItemKind, CompletionItemKindCapability, CompletionResponse, MarkupKind,
    TextDocumentClientCapabilities, Url,
};
use tokio::sync::{Mutex as TokioMutex, mpsc as tokio_mpsc};

use crate::lsp::client::LspClient;
use crate::lsp::config::{Language, ServerConfig};
use crate::lsp::log_file;
use crate::lsp::registry::ServerRegistry;
use crate::lsp::runtime::LspRuntime;

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
}

pub struct LspManager {
    cmd_tx: tokio_mpsc::UnboundedSender<Cmd>,
    completion_rx: std_mpsc::Receiver<CompletionResult>,
    next_request_id: u64,
    workspace_root: PathBuf,
    _runtime: LspRuntime,
}

impl LspManager {
    pub fn new(workspace_root: PathBuf) -> Result<Self> {
        let runtime = LspRuntime::new()?;
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let (completion_tx, completion_rx) = std_mpsc::channel();
        let root = workspace_root.clone();
        runtime.handle().spawn(worker_loop(
            root,
            ServerRegistry::with_defaults(),
            cmd_rx,
            completion_tx,
        ));
        Ok(Self {
            cmd_tx,
            completion_rx,
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
}

struct ManagedClient {
    name: String,
    client: Arc<TokioMutex<LspClient>>,
    supports_completion: bool,
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
                        log_file::log(&format!(
                            "lsp[{}] spawned, supports_completion={supports}",
                            config.name
                        ));
                        spawned.push(ManagedClient {
                            name: config.name.to_string(),
                            client: Arc::new(TokioMutex::new(client)),
                            supports_completion: supports,
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

fn build_client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            completion: Some(CompletionClientCapabilities {
                dynamic_registration: Some(false),
                context_support: Some(true),
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(false),
                    commit_characters_support: Some(false),
                    documentation_format: Some(vec![
                        MarkupKind::Markdown,
                        MarkupKind::PlainText,
                    ]),
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
            if let Ok(meta) = std::fs::metadata(&candidate) {
                if meta.permissions().mode() & 0o111 != 0 {
                    return true;
                }
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
    use std::time::{Duration, Instant};

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
        let text = String::from(
            "def f(input, num):\n    input_split = input.split()\n    inp\n",
        );
        std::fs::write(&file, &text).expect("write demo");

        let mut manager = LspManager::new(root).expect("manager");
        manager.open_doc(file.clone(), text);
        std::thread::sleep(Duration::from_millis(2500));

        let id = manager.request_completion(file.clone(), 2, 7);
        let result = drain_completion_blocking(&manager, Duration::from_secs(10))
            .expect("completion arrived");
        assert_eq!(result.request_id, id);
        assert_eq!(result.path, file);
        let labels: Vec<&str> = result.items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.starts_with("input")),
            "expected at least one item starting with `input`, got: {labels:?}"
        );
    }
}
