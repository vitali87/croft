//! Keeping servers in step with open documents: open, change, save and close, the text
//! each server last saw, and the files the workspace watcher reports changed.

use super::*;

impl LspManager {
    pub fn open_doc(&self, path: PathBuf, text: String) {
        #[cfg(test)]
        self.record_synced_text(&path, &text);
        let _ = self.cmd_tx.send(Cmd::OpenDoc { path, text });
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

    /// Test hook: the text the last open or change sent for `path`, as
    /// every server on it would read it.
    #[cfg(test)]
    pub fn synced_text_for_test(&self, path: &Path) -> Option<String> {
        self.synced_texts.lock().ok()?.get(path).cloned()
    }

    #[cfg(test)]
    pub(super) fn record_synced_text(&self, path: &Path, text: &str) {
        if let Ok(mut texts) = self.synced_texts.lock() {
            texts.insert(path.to_path_buf(), text.to_string());
        }
    }

    /// Tell the servers that watch them that files changed on disk (#908).
    /// A file gone by the time this is sent goes out as deleted.
    pub fn did_change_watched_files(&mut self, changes: Vec<(PathBuf, lsp_types::FileChangeType)>) {
        let _ = self.cmd_tx.send(Cmd::DidChangeWatchedFiles { changes });
    }
}

impl WorkerState {
    pub(super) async fn open_doc(&mut self, path: PathBuf, text: String) {
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

    pub(super) async fn change_doc(&mut self, path: PathBuf, text: String) {
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

    pub(super) async fn save_doc(&mut self, path: PathBuf, text: String) {
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

    pub(super) async fn close_doc(&mut self, path: PathBuf) {
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
    pub(super) fn did_change_watched_files(
        &mut self,
        changes: Vec<(PathBuf, lsp_types::FileChangeType)>,
    ) {
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

/// Whether a server asked for `textDocument/didSave`, and with the saved
/// text or without it (#854).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SaveNotify {
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
pub(super) fn save_notify_of(sync: &Option<lsp_types::TextDocumentSyncCapability>) -> SaveNotify {
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
