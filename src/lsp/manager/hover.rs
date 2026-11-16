//! Hover, and rust-analyzer's own requests (expand macro, parent module and others).

use super::*;

/// The answer to one of rust-analyzer's own requests (#1626), raw; `Err`
/// says why there is none (no rust-analyzer for the file, or its error).
#[derive(Debug)]
pub struct RustAnalyzerResult {
    pub request_id: u64,
    pub request: crate::lsp::rust_analyzer::RaRequest,
    pub outcome: Result<serde_json::Value, String>,
}

#[derive(Debug)]
pub struct HoverResult {
    pub request_id: u64,
    pub text: Option<String>,
}

impl LspManager {
    pub fn request_hover(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        self.send_request(|request_id| Cmd::RequestHover {
            request_id,
            path,
            line,
            character,
        })
    }

    pub fn drain_hover(&self) -> Option<HoverResult> {
        self.hover_rx.try_recv().ok()
    }

    /// Test hook: deliver a hover reply as if a server had answered.
    #[cfg(test)]
    pub fn push_hover_for_test(&self, result: HoverResult) {
        self.hover_test_tx.send(result).expect("hover channel open");
    }

    /// Send one of rust-analyzer's own requests (#1626) for the caret at
    /// UTF-16 `(line, character)` in `path`; the answer arrives on
    /// [`LspManager::drain_rust_analyzer`] under the returned id.
    pub fn request_rust_analyzer(
        &mut self,
        path: PathBuf,
        request: crate::lsp::rust_analyzer::RaRequest,
        line: u32,
        character: u32,
    ) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let _ = self.cmd_tx.send(Cmd::RustAnalyzer {
            request_id: id,
            path,
            request,
            line,
            character,
        });
        id
    }

    pub fn drain_rust_analyzer(&self) -> Option<RustAnalyzerResult> {
        self.rust_analyzer_rx.try_recv().ok()
    }
}

impl WorkerState {
    /// Send `request` to the rust-analyzer serving `path` (#1626). Every
    /// path answers: with no rust-analyzer for the file the reply says so,
    /// so the command that asked never waits on nothing.
    pub(super) async fn request_rust_analyzer(
        &mut self,
        request_id: u64,
        path: PathBuf,
        request: crate::lsp::rust_analyzer::RaRequest,
        (line, character): (u32, u32),
        tx: &std_mpsc::Sender<RustAnalyzerResult>,
    ) {
        let answer = |outcome: Result<serde_json::Value, String>| RustAnalyzerResult {
            request_id,
            request,
            outcome,
        };
        let not_running = || {
            answer(Err(format!(
                "{} is not running for this file",
                crate::lsp::rust_analyzer::SERVER
            )))
        };
        let Some(doc) = self.docs.get(&path) else {
            let _ = tx.send(not_running());
            return;
        };
        let key = (doc.language, doc.project_root.clone());
        self.ensure_clients(key.0, &key.1).await;
        let picked = self.clients.get(&key).and_then(|clients| {
            clients
                .iter()
                .find(|c| c.name == crate::lsp::rust_analyzer::SERVER)
                .map(|c| c.client.clone())
        });
        let (Some(client_arc), Ok(uri)) = (picked, Url::from_file_path(&path)) else {
            let _ = tx.send(not_running());
            return;
        };
        let params = request.params(&uri, line, character);
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let outcome = client
                .rust_analyzer(request, params)
                .await
                .map_err(|e| format!("{e:#}"));
            if let Err(e) = &outcome {
                log_file::log(&format!(
                    "lsp[rust-analyzer] {} error: {e}",
                    request.method()
                ));
            }
            let _ = tx.send(RustAnalyzerResult {
                request_id,
                request,
                outcome,
            });
        });
    }

    pub(super) async fn request_hover(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<HoverResult>,
    ) {
        // Every hover is answered, an empty answer when no server can give
        // one: the app holds a problem's message until the reply, and Show
        // Hover says when there is nothing (#1518).
        let none = || {
            let _ = tx.send(HoverResult {
                request_id,
                text: None,
            });
        };
        let (server_name, client_arc) = match self.pick_server(&path, |c| c.supports_hover).await {
            Ok(picked) => picked,
            Err(NoServer::Unsupported | NoServer::Unknown) => return none(),
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return none();
        };
        let tx = tx.clone();
        log_file::log(&format!(
            "hover request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
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
            let _ = tx.send(HoverResult { request_id, text });
        });
    }
}

pub(super) fn marked_string_text(m: &MarkedString) -> &str {
    match m {
        MarkedString::String(s) => s,
        MarkedString::LanguageString(ls) => &ls.value,
    }
}

pub(super) fn hover_text(contents: &HoverContents) -> Option<String> {
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

/// Hover capability (`boolean | HoverOptions`), same bare-`false` rule.
pub(super) fn hover_supported(cap: &Option<HoverProviderCapability>) -> bool {
    match cap {
        Some(HoverProviderCapability::Simple(b)) => *b,
        Some(HoverProviderCapability::Options(_)) => true,
        None => false,
    }
}
