use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;

use anyhow::Result;
use lsp_types::{ClientCapabilities, CompletionItemKind, CompletionResponse, Url};
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

struct WorkerState {
    workspace_root: PathBuf,
    registry: ServerRegistry,
    clients: HashMap<Language, Arc<TokioMutex<LspClient>>>,
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
    async fn ensure_client(&mut self, lang: Language) -> Option<Arc<TokioMutex<LspClient>>> {
        if let Some(c) = self.clients.get(&lang) {
            return Some(c.clone());
        }
        let configs: Vec<ServerConfig> = self.registry.for_language(lang).to_vec();
        for config in configs.iter() {
            if !is_on_path(&config.command) {
                continue;
            }
            match LspClient::spawn(config, &self.workspace_root, ClientCapabilities::default())
                .await
            {
                Ok(client) => {
                    let arc = Arc::new(TokioMutex::new(client));
                    self.clients.insert(lang, arc.clone());
                    return Some(arc);
                }
                Err(e) => {
                    log_file::log(&format!("lsp[{}] spawn failed: {e}", config.name));
                    continue;
                }
            }
        }
        None
    }

    async fn open_doc(&mut self, path: PathBuf, text: String) {
        let Some(lang) = path_to_language(&path) else {
            return;
        };
        let Some(client_arc) = self.ensure_client(lang).await else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let mut client = client_arc.lock().await;
        if let Err(e) = client.did_open(uri, lang.lsp_id(), 0, text) {
            log_file::log(&format!("lsp did_open failed: {e}"));
            return;
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
        let Some(client_arc) = self.clients.get(&lang).cloned() else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let mut client = client_arc.lock().await;
        if let Err(e) = client.did_change_full(uri, version, text) {
            log_file::log(&format!("lsp did_change failed: {e}"));
        }
    }

    async fn close_doc(&mut self, path: PathBuf) {
        let Some(doc) = self.docs.remove(&path) else {
            return;
        };
        let Some(client_arc) = self.clients.get(&doc.language).cloned() else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let mut client = client_arc.lock().await;
        if let Err(e) = client.did_close(uri) {
            log_file::log(&format!("lsp did_close failed: {e}"));
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
        let Some(client_arc) = self.clients.get(&doc.language).cloned() else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "completion request id={request_id} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await;
            let resp = client.completion(uri, line, character).await;
            drop(client);
            let items: Vec<CompletionItem> = match resp {
                Ok(Some(CompletionResponse::Array(items))) => {
                    items.into_iter().map(into_item).collect()
                }
                Ok(Some(CompletionResponse::List(list))) => {
                    list.items.into_iter().map(into_item).collect()
                }
                Ok(None) => Vec::new(),
                Err(e) => {
                    log_file::log(&format!("lsp completion error: {e}"));
                    Vec::new()
                }
            };
            let preview: Vec<&str> = items
                .iter()
                .take(200)
                .map(|i| i.label.as_str())
                .collect();
            log_file::log(&format!(
                "completion response id={request_id} count={} labels={:?}",
                items.len(),
                preview
            ));
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

fn path_to_language(path: &Path) -> Option<Language> {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(Language::from_extension)
}

fn is_on_path(cmd: &str) -> bool {
    std::process::Command::new(cmd)
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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

    fn first_completion_python_config() -> Option<ServerConfig> {
        if is_on_path("ty") {
            Some(ServerConfig::ty())
        } else if is_on_path("basedpyright-langserver") {
            Some(ServerConfig::basedpyright())
        } else if is_on_path("pyright-langserver") {
            Some(ServerConfig::pyright())
        } else {
            None
        }
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
        std::thread::sleep(Duration::from_millis(400));
        manager.change_doc(file.clone(), String::from("x = 2\n"));
        std::thread::sleep(Duration::from_millis(400));
        manager.close_doc(file.clone());
        std::thread::sleep(Duration::from_millis(200));
        drop(manager);
    }

    #[test]
    fn manager_completion_against_python_lsp() {
        let Some(config) = first_completion_python_config() else {
            eprintln!("SKIPPED: no ty/basedpyright/pyright on PATH");
            return;
        };
        let server_name = config.name;
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize");
        let file = root.join("demo.py");
        let text = String::from("import os\nos.\n");
        std::fs::write(&file, &text).expect("write demo");

        let mut manager = LspManager::new(root).expect("manager");
        manager.open_doc(file.clone(), text);
        std::thread::sleep(Duration::from_millis(1500));

        let id = manager.request_completion(file.clone(), 1, 3);
        let result = drain_completion_blocking(&manager, Duration::from_secs(8))
            .expect("completion arrived");
        assert_eq!(result.request_id, id);
        assert_eq!(result.path, file);
        assert!(
            !result.items.is_empty(),
            "expected {server_name} to return completions for os."
        );
    }
}
