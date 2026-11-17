//! Ranges a server reports around the caret: occurrences of the symbol, linked editing
//! ranges, and the selection ranges that expand and shrink a selection.

use super::*;

/// One occurrence of the symbol under the caret
/// (`textDocument/documentHighlight`), normalised off the LSP wire type.
/// Positions are LSP UTF-16; the editor converts them to character columns
/// against its own buffer, exactly as it does for diagnostics. `write` marks a
/// Write-kind highlight (VS Code tints those stronger than reads).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccurrenceItem {
    pub start_line: u32,
    pub start_char: u32,
    pub end_line: u32,
    pub end_char: u32,
    pub write: bool,
}

/// The occurrences of the symbol at one caret position, tagged with the
/// request id so the app drops a reply the caret has since moved away from.
#[derive(Debug)]
pub struct DocumentHighlightsResult {
    pub request_id: u64,
    pub items: Vec<OccurrenceItem>,
}

/// One `textDocument/linkedEditingRange` answer (#254): the UTF-16
/// spans that rename together with the caret's, or empty when the caret
/// is not on a linked site. No `unsupported` flag — like occurrences,
/// the feature is a silent decoration that simply never activates for a
/// language without the provider.
#[derive(Debug)]
pub struct LinkedEditingResult {
    pub request_id: u64,
    /// `(start_line, start_char, end_line, end_char)` per span.
    pub ranges: Vec<(u32, u32, u32, u32)>,
}

/// One `textDocument/selectionRange` answer (#254): a strictly-ordered
/// smallest-first chain of UTF-16 spans per requested position (the
/// server's parent links flattened), index-aligned with the request's
/// cursors. `unsupported` is true when the request could not be routed
/// (untracked doc, or no server advertises the provider) so the app can
/// fall back to tree-sitter ancestry immediately instead of waiting.
#[derive(Debug)]
pub struct SelectionRangesResult {
    pub request_id: u64,
    pub unsupported: bool,
    /// Per cursor: `(start_line, start_char, end_line, end_char)`.
    pub chains: Vec<Vec<(u32, u32, u32, u32)>>,
}

impl LspManager {
    /// Fire a `textDocument/documentHighlight` for the symbol at the caret;
    /// the reply arrives via [`Self::drain_document_highlights`] tagged with
    /// the returned id. Silent when no server supports the method — the app
    /// simply never gets a reply to paint.
    pub fn request_document_highlights(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        self.send_request(|request_id| Cmd::RequestDocumentHighlights {
            request_id,
            path,
            line,
            character,
        })
    }

    pub fn drain_document_highlights(&self) -> Option<DocumentHighlightsResult> {
        self.doc_highlights_rx.try_recv().ok()
    }

    /// Ask which spans rename together with the caret's (#254). Same
    /// fire-and-forget id contract as occurrences; no reply arrives for
    /// a language without the provider.
    pub fn request_linked_editing(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        self.send_request(|request_id| Cmd::RequestLinkedEditing {
            request_id,
            path,
            line,
            character,
        })
    }

    /// Ask for the Expand Selection chains at every cursor (#254): one
    /// request carries all positions, primary first. Returns the request
    /// id the reply will echo.
    pub fn request_selection_ranges(&mut self, path: PathBuf, positions: Vec<(u32, u32)>) -> u64 {
        self.send_request(|request_id| Cmd::RequestSelectionRanges {
            request_id,
            path,
            positions,
        })
    }

    pub fn drain_linked_editing(&self) -> Option<LinkedEditingResult> {
        self.linked_editing_rx.try_recv().ok()
    }

    /// Test hook: deliver `result` as if a server had answered.
    #[cfg(test)]
    pub fn push_linked_editing_for_test(&self, result: LinkedEditingResult) {
        let _ = self.linked_editing_test_tx.send(result);
    }

    pub fn drain_selection_ranges(&self) -> Option<SelectionRangesResult> {
        self.selection_ranges_rx.try_recv().ok()
    }
}

impl WorkerState {
    pub(super) async fn request_document_highlights(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<DocumentHighlightsResult>,
    ) {
        // No unsupported reply: occurrences are a silent background
        // decoration, so a language without the capability just never
        // paints (the app cleared the old set when the caret moved).
        let Ok((server_name, client_arc)) = self
            .pick_server(&path, |c| c.supports_document_highlight)
            .await
        else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.document_highlight(uri, line, character).await;
            drop(client);
            let items = match resp {
                Ok(Some(hs)) => occurrence_items(hs),
                Ok(None) => Vec::new(),
                Err(e) => {
                    log_file::log(&format!(
                        "lsp[{server_name}] documentHighlight error: {e:#}"
                    ));
                    Vec::new()
                }
            };
            let _ = tx.send(DocumentHighlightsResult { request_id, items });
        });
    }

    /// Linked-editing spans at the caret (#254). Silent on every
    /// early-out like occurrences: the mirror is a background behavior
    /// that simply never engages for a language without the provider.
    pub(super) async fn request_linked_editing(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<LinkedEditingResult>,
    ) {
        let Ok((server_name, client_arc)) =
            self.pick_server(&path, |c| c.supports_linked_editing).await
        else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.linked_editing_range(uri, line, character).await;
            drop(client);
            let ranges = match resp {
                Ok(Some(l)) => l
                    .ranges
                    .iter()
                    .map(|r| (r.start.line, r.start.character, r.end.line, r.end.character))
                    .collect(),
                Ok(None) => Vec::new(),
                Err(e) => {
                    log_file::log(&format!(
                        "lsp[{server_name}] linkedEditingRange error: {e:#}"
                    ));
                    Vec::new()
                }
            };
            let _ = tx.send(LinkedEditingResult { request_id, ranges });
        });
    }

    /// Expand Selection chains (#254). UNLIKE occurrences, every early-out
    /// answers with `unsupported: true` — the gesture is waiting on this
    /// reply, and an unsupported verdict is what tells the app to grow the
    /// selection from tree-sitter ancestry right now instead of never.
    pub(super) async fn request_selection_ranges(
        &mut self,
        request_id: u64,
        path: PathBuf,
        positions: Vec<(u32, u32)>,
        tx: &std_mpsc::Sender<SelectionRangesResult>,
    ) {
        let unsupported = |tx: &std_mpsc::Sender<SelectionRangesResult>| {
            let _ = tx.send(SelectionRangesResult {
                request_id,
                unsupported: true,
                chains: Vec::new(),
            });
        };
        let Ok((server_name, client_arc)) = self
            .pick_server(&path, |c| c.supports_selection_range)
            .await
        else {
            unsupported(tx);
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            unsupported(tx);
            return;
        };
        let lsp_positions: Vec<lsp_types::Position> = positions
            .iter()
            .map(|&(line, character)| lsp_types::Position { line, character })
            .collect();
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.selection_ranges(uri, lsp_positions).await;
            drop(client);
            let (unsupported, chains) = match resp {
                Ok(Some(ranges)) => (false, ranges.iter().map(flatten_selection_chain).collect()),
                // An empty answer is a real answer: nothing to grow to.
                Ok(None) => (false, Vec::new()),
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] selectionRange error: {e:#}"));
                    // Treat a wire error like unsupported so the gesture
                    // still resolves via the tree-sitter fallback.
                    (true, Vec::new())
                }
            };
            let _ = tx.send(SelectionRangesResult {
                request_id,
                unsupported,
                chains,
            });
        });
    }
}

/// Whether the server advertises a usable `textDocument/linkedEditingRange`
/// provider (#254). Same shape rules as `code_action_supported`.
pub(super) fn linked_editing_supported(
    cap: &Option<lsp_types::LinkedEditingRangeServerCapabilities>,
) -> bool {
    use lsp_types::LinkedEditingRangeServerCapabilities as L;
    match cap {
        Some(L::Simple(b)) => *b,
        Some(L::Options(_)) | Some(L::RegistrationOptions(_)) => true,
        None => false,
    }
}

/// Flatten one `SelectionRange` parent chain into smallest-first UTF-16
/// spans, dropping non-growing links (some servers repeat a range).
pub(super) fn flatten_selection_chain(sr: &lsp_types::SelectionRange) -> Vec<(u32, u32, u32, u32)> {
    let mut out = Vec::new();
    let mut cur = Some(sr);
    let mut last: Option<(u32, u32, u32, u32)> = None;
    while let Some(r) = cur {
        let span = (
            r.range.start.line,
            r.range.start.character,
            r.range.end.line,
            r.range.end.character,
        );
        if last != Some(span) {
            out.push(span);
            last = Some(span);
        }
        cur = r.parent.as_deref();
    }
    out
}

/// Whether the server advertises a usable `textDocument/selectionRange`
/// provider (#254). Same shape rules as `code_action_supported`.
pub(super) fn selection_range_supported(
    cap: &Option<lsp_types::SelectionRangeProviderCapability>,
) -> bool {
    use lsp_types::SelectionRangeProviderCapability as S;
    match cap {
        Some(S::Simple(b)) => *b,
        Some(S::Options(_)) | Some(S::RegistrationOptions(_)) => true,
        None => false,
    }
}

/// Normalise `textDocument/documentHighlight` wire items into croft's own
/// UTF-16-positioned occurrence spans. A missing kind defaults to a read
/// tint, matching VS Code (Text and Read share `wordHighlightBackground`;
/// only Write gets the strong tint).
pub(super) fn occurrence_items(hs: Vec<lsp_types::DocumentHighlight>) -> Vec<OccurrenceItem> {
    hs.into_iter()
        .map(|h| OccurrenceItem {
            start_line: h.range.start.line,
            start_char: h.range.start.character,
            end_line: h.range.end.line,
            end_char: h.range.end.character,
            write: h.kind == Some(lsp_types::DocumentHighlightKind::WRITE),
        })
        .collect()
}
