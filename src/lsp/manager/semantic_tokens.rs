//! Semantic tokens: whole-document and range requests, the server's legend, and turning
//! its token stream into the editor's batches.

use super::*;

/// A fresh batch of semantic tokens for a document, pushed to the editor
/// to overlay on the tree-sitter highlights. Unlike the navigation
/// requests this carries no `request_id`: it is keyed by `path` + `seq`,
/// and the latest batch for the CURRENT text always wins. `data` is the raw relative-encoded LSP
/// array; the editor decodes it against its own buffer (where the text
/// lives) so UTF-16 columns convert to byte offsets correctly. `legend`
/// maps token-type indices to names.
#[derive(Debug)]
pub struct SemanticTokensUpdate {
    pub path: PathBuf,
    /// Echoes the edit sequence the request was fired for. The app drops a
    /// reply whose seq no longer matches the buffer — an external reload
    /// swaps the whole text while a request is still in the retry backoff,
    /// and decoding that batch over the new lines paints the old file's
    /// colours at the wrong offsets (and would poison the on-disk cache
    /// under the NEW content's key). Same guard as `InlayHintsUpdate`.
    pub seq: u64,
    /// Monotonic per-request stamp; the app drops a reply older than the
    /// newest one already applied for this path (see `semantic_generation`).
    pub generation: u64,
    pub data: Vec<u32>,
    pub legend: Arc<Vec<String>>,
    /// `true` for a whole-document `semanticTokens/full` batch, `false` for a
    /// viewport-only `semanticTokens/range` batch. The editor refuses to let a
    /// range batch overwrite a full one for the same file, so a range reply
    /// that arrives after the full document can never erase off-screen colour.
    pub is_full: bool,
}

impl LspManager {
    /// Ask the server for the whole document's semantic tokens. Fire this
    /// on open and (debounced) after edits. The reply carries two stamps and
    /// the app needs both: `seq` identifies the document CONTENT the batch
    /// was computed against (so a reply that outlived an external reload is
    /// dropped), while `generation` orders overlapping requests for the same
    /// path (two can be in flight at one `seq`, and the retry loop emits an
    /// empty batch once its backoff runs out, so the loser must not win).
    /// A no-op for languages whose server advertises no
    /// `semanticTokensProvider`.
    pub fn request_semantic_tokens(&self, path: PathBuf, seq: u64) {
        let generation = self.next_semantic_generation();
        let _ = self.cmd_tx.send(Cmd::RequestSemanticTokens {
            path,
            seq,
            generation,
        });
    }

    pub(super) fn next_semantic_generation(&self) -> u64 {
        self.semantic_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Ask the server for semantic tokens covering only `start_line` up to
    /// `end` (zero-based, end exclusive; see [`rows_end`]). Fire this on open
    /// with the editor's viewport so the visible code colours immediately,
    /// ahead of the whole-file request.
    pub fn request_semantic_tokens_range(
        &self,
        path: PathBuf,
        start_line: u32,
        end: Position,
        seq: u64,
    ) {
        let generation = self.next_semantic_generation();
        let _ = self.cmd_tx.send(Cmd::RequestSemanticTokensRange {
            path,
            start_line,
            end,
            seq,
            generation,
        });
    }

    pub fn drain_semantic_tokens(&self) -> Option<SemanticTokensUpdate> {
        self.replies.semantic_tokens.try_recv().ok()
    }

    /// Returns and clears the "a server asked us to re-pull semantic tokens"
    /// flag, set by any client that received `workspace/semanticTokens/refresh`
    /// (rust-analyzer does this once its analysis upgrades the token set). The
    /// app re-requests tokens for the visible editor(s) when this is true.
    pub fn take_semantic_refresh(&self) -> bool {
        self.semantic_refresh.swap(false, Ordering::Relaxed)
    }
}

impl WorkerState {
    pub(super) async fn request_semantic_tokens(
        &mut self,
        path: PathBuf,
        seq: u64,
        generation: u64,
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
                let mut client = client_arc.lock().await.requests();
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
                seq,
                generation,
                data,
                legend,
                is_full: true,
            });
        });
    }

    pub(super) async fn request_semantic_tokens_range(
        &mut self,
        path: PathBuf,
        start_line: u32,
        end: Position,
        seq: u64,
        generation: u64,
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
            let mut client = client_arc.lock().await.requests();
            let resp = client.semantic_tokens_range(uri, start_line, end).await;
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
                "semantic_tokens range response server={server_name} path={} lines={start_line}..{} tokens={}",
                path_clone.display(),
                end.line,
                data.len() / 5
            ));
            let _ = tx.send(SemanticTokensUpdate {
                path: path_clone,
                seq,
                generation,
                data,
                legend,
                is_full: false,
            });
        });
    }
}

/// Extract a server's semantic-token legend (token-type names in index
/// order) from its advertised capabilities, but only when it supports
/// full-document requests, since that is the only variant croft issues.
pub(super) fn semantic_legend_of(caps: &ServerCapabilities) -> Option<Vec<String>> {
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
pub(super) fn semantic_tokens_range_supported(caps: &ServerCapabilities) -> bool {
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
pub(super) fn flatten_semantic_tokens(tokens: &[lsp_types::SemanticToken]) -> Vec<u32> {
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
