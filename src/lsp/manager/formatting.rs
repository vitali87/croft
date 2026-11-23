//! Formatting: a whole document, a range, and as you type.

use super::*;

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
    /// The server that answered, `None` when none did (#1511), so the app
    /// can attribute an empty reply to the formatter that sent it.
    pub server: Option<String>,
}

impl LspManager {
    pub fn request_formatting(&mut self, path: PathBuf, tab_size: u32, insert_spaces: bool) -> u64 {
        self.send_request(|request_id| Cmd::RequestFormatting {
            request_id,
            path,
            tab_size,
            insert_spaces,
        })
    }

    /// Fire `textDocument/onTypeFormatting` for the just-typed trigger
    /// character (#254). Silent for a language without the provider —
    /// on-type formatting is a background behavior, not a command.
    #[allow(clippy::too_many_arguments)]
    pub fn request_on_type_formatting(
        &mut self,
        path: PathBuf,
        line: u32,
        character: u32,
        ch: char,
        tab_size: u32,
        insert_spaces: bool,
    ) -> u64 {
        self.send_request(|request_id| Cmd::RequestOnTypeFormatting {
            request_id,
            path,
            line,
            character,
            ch,
            tab_size,
            insert_spaces,
        })
    }

    pub fn drain_on_type_formatting(&self) -> Option<FormatResult> {
        self.replies.on_type_formatting.try_recv().ok()
    }

    /// The on-type trigger characters some spawned server advertises for
    /// `lang` — `None` until a server for the language has been probed,
    /// or when none carries the provider. The app consults this before
    /// firing, so nothing is ever sent unadvertised.
    pub fn language_on_type_triggers(&self, lang: crate::lsp::Language) -> Option<String> {
        self.capability_support
            .lock()
            .ok()?
            .on_type_triggers
            .get(&lang)
            .cloned()
    }

    /// Format only a UTF-16 span ("Format Selection", #254). Same reply
    /// channel and unsupported contract as whole-document formatting;
    /// the app tells the two apart by the returned id.
    pub fn request_range_formatting(
        &mut self,
        path: PathBuf,
        start: (u32, u32),
        end: (u32, u32),
        tab_size: u32,
        insert_spaces: bool,
    ) -> u64 {
        self.send_request(|request_id| Cmd::RequestRangeFormatting {
            request_id,
            path,
            start,
            end,
            tab_size,
            insert_spaces,
        })
    }

    pub fn drain_formatting(&self) -> Option<FormatResult> {
        self.replies.formatting.try_recv().ok()
    }

    /// Whether the server for `lang` implements `textDocument/formatting`.
    /// `None` means no server has reported yet. Read synchronously by the app
    /// to decide whether to offer "Format Document".
    pub fn language_supports_formatting(&self, lang: Language) -> Option<bool> {
        self.language_supports(lang, |s| &s.formatting)
    }
}

impl WorkerState {
    pub(super) async fn request_formatting(
        &mut self,
        request_id: u64,
        path: PathBuf,
        tab_size: u32,
        insert_spaces: bool,
        tx: &std_mpsc::Sender<FormatResult>,
    ) {
        // Every bail below MUST still answer. The app defers the format-on-save
        // write until a reply lands, so a silent `return` strands the latch:
        // no write, no error, and the user believes the file was saved.
        let Some(doc) = self.docs.get(&path) else {
            answer_unsupported(tx, request_id, path);
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        // Re-probe so a formatter installed since the file opened is picked up
        // without reopening (mirrors `request_rename`).
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            answer_unsupported(tx, request_id, path);
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
                server: None,
            });
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            answer_unsupported(tx, request_id, path);
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "formatting request id={request_id} server={server_name} path={} tab_size={tab_size} insert_spaces={insert_spaces}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
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
                server: Some(server_name),
            });
        });
    }

    /// On-type formatting (#254): the server's reaction to one just-typed
    /// trigger character. Silent on every early-out — this is a
    /// background behavior with no status line waiting on it, and the
    /// app pre-checks the trigger set so an unroutable request means a
    /// race (doc closed), not a misconfiguration. This deliberately
    /// diverges from `request_formatting`'s "every bail must still
    /// answer" invariant: nothing blocks on this reply, so an unanswered
    /// bail costs nothing.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn request_on_type_formatting(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        ch: char,
        tab_size: u32,
        insert_spaces: bool,
        tx: &std_mpsc::Sender<FormatResult>,
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
            .find(|c| c.on_type_triggers.as_ref().is_some_and(|t| t.contains(ch)))
            .map(|c| (c.name.clone(), c.client.clone()));
        let Some((server_name, client_arc)) = picked else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client
                .on_type_formatting(
                    uri,
                    line,
                    character,
                    ch.to_string(),
                    tab_size,
                    insert_spaces,
                )
                .await;
            drop(client);
            let edits = match resp {
                Ok(Some(tes)) => Some(tes.iter().map(text_edit_to_span).collect()),
                Ok(None) => None,
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] onTypeFormatting error: {e}"));
                    None
                }
            };
            let _ = tx.send(FormatResult {
                request_id,
                path: path_clone,
                edits,
                unsupported: false,
                server: Some(server_name),
            });
        });
    }

    /// "Format Selection" (#254): `textDocument/rangeFormatting` over one
    /// UTF-16 span. Same always-answer contract as whole-document
    /// formatting — the app shows status off this reply.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn request_range_formatting(
        &mut self,
        request_id: u64,
        path: PathBuf,
        start: (u32, u32),
        end: (u32, u32),
        tab_size: u32,
        insert_spaces: bool,
        tx: &std_mpsc::Sender<FormatResult>,
    ) {
        let Ok((server_name, client_arc)) = self
            .pick_server(&path, |c| c.supports_range_formatting)
            .await
        else {
            answer_unsupported(tx, request_id, path);
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            answer_unsupported(tx, request_id, path);
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client
                .range_formatting(uri, start.0, start.1, end.0, end.1, tab_size, insert_spaces)
                .await;
            drop(client);
            let edits = match resp {
                Ok(Some(tes)) => Some(tes.iter().map(text_edit_to_span).collect()),
                Ok(None) => None,
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] range_formatting error: {e}"));
                    None
                }
            };
            let _ = tx.send(FormatResult {
                request_id,
                path: path_clone,
                edits,
                unsupported: false,
                server: Some(server_name),
            });
        });
    }
}

/// Answer a formatting request that cannot reach a server. `unsupported` is
/// the honest shape for every one of these: no client in this worker can
/// format the file. The app turns it into "No formatter available for this
/// file" and, crucially, still performs the deferred save.
pub(super) fn answer_unsupported(
    tx: &std_mpsc::Sender<FormatResult>,
    request_id: u64,
    path: PathBuf,
) {
    let _ = tx.send(FormatResult {
        request_id,
        path,
        edits: None,
        unsupported: true,
        server: None,
    });
}
