//! What servers lay over the text besides tokens: inlay hints, document links, code
//! lenses, folding ranges, and document colours and their presentations.

use super::*;

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

/// One server-resolved document link (#254): a UTF-16 range plus its
/// target URI string. Links the server left target-less are dropped at
/// normalisation (resolving them needs another round trip croft skips).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentLinkItem {
    pub line: u32,
    pub character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub target: String,
}

/// One code lens (#608): the title the server gave it and the command it
/// runs, on a 0-based line.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeLensItem {
    pub line: u32,
    pub title: String,
    pub command: Option<String>,
    pub arguments: Vec<serde_json::Value>,
    /// Whether the producing server lists the command in its
    /// `executeCommandProvider`, so croft can hand it back to run.
    pub server_side: bool,
}

/// A document's code lenses at one edit `seq` (#608).
#[derive(Debug)]
pub struct CodeLensUpdate {
    pub path: PathBuf,
    pub seq: u64,
    pub lenses: Vec<CodeLensItem>,
}

/// A fresh, complete link set for one document; same seq contract as
/// [`InlayHintsUpdate`].
#[derive(Debug)]
pub struct DocumentLinksUpdate {
    pub path: PathBuf,
    pub seq: u64,
    pub links: Vec<DocumentLinkItem>,
}

/// One server fold span (#254), 0-based inclusive line indexes matching
/// the editor's (header, end) fold model: collapsing hides
/// `start_line + 1 ..= end_line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldingRangeItem {
    pub start_line: usize,
    pub end_line: usize,
    pub kind: FoldRangeKind,
}

/// The LSP folding-range kind, `Other` for servers that send none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldRangeKind {
    Comment,
    Imports,
    Region,
    Other,
}

/// A complete fold-span set for one document; `seq` mirrors
/// [`InlayHintsUpdate`]'s staleness contract.
#[derive(Debug)]
pub struct FoldingRangesUpdate {
    pub path: PathBuf,
    pub seq: u64,
    pub ranges: Vec<FoldingRangeItem>,
}

/// One document color value (#254): the UTF-16 range of the literal and
/// its decoded RGB (alpha dropped — terminal cells can't blend). The
/// raw f32 channels ride along for the colorPresentation round trip.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorItem {
    pub line: u32,
    pub character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub raw: (f32, f32, f32, f32),
}

/// A fresh, complete color set for one document; same seq contract as
/// [`InlayHintsUpdate`].
#[derive(Debug)]
pub struct DocumentColorsUpdate {
    pub path: PathBuf,
    pub seq: u64,
    pub colors: Vec<ColorItem>,
}

/// The alternative spellings of one color value (#254), each with the
/// char-indexed edits that rewrite the document to it. Answered for a
/// user-initiated picker, so every early-out replies (`presentations`
/// empty ⇒ nothing to offer).
#[derive(Debug)]
pub struct ColorPresentationsResult {
    pub request_id: u64,
    pub presentations: Vec<(String, Vec<TextSpanEdit>)>,
}

impl LspManager {
    /// Ask the server for the whole document's inlay hints. Fire-and-forget
    /// like the semantic-token requests; the reply lands in
    /// [`drain_inlay_hints`] tagged with `seq` so the app can drop a stale
    /// batch. A no-op when no spawned server advertises an `inlayHintProvider`.
    /// `end` is where the document ends ([`document_end`]).
    pub fn request_inlay_hints(&self, path: PathBuf, end: Position, seq: u64) {
        let _ = self.cmd_tx.send(Cmd::RequestInlayHints { path, end, seq });
    }

    pub fn drain_inlay_hints(&self) -> Option<InlayHintsUpdate> {
        self.replies.inlay_hints.try_recv().ok()
    }

    /// Ask for the document's link set (#254); fire-and-forget on the
    /// open/change cadence, silent when no server advertises a
    /// documentLinkProvider.
    pub fn request_document_links(&self, path: PathBuf, seq: u64) {
        let _ = self.cmd_tx.send(Cmd::RequestDocumentLinks { path, seq });
    }

    pub fn drain_document_links(&self) -> Option<DocumentLinksUpdate> {
        self.replies.document_links.try_recv().ok()
    }

    /// Ask for the document's code lenses (#608); the reply carries `seq`.
    pub fn request_code_lens(&self, path: PathBuf, seq: u64) {
        let _ = self.cmd_tx.send(Cmd::RequestCodeLens { path, seq });
    }

    pub fn drain_code_lens(&self) -> Option<CodeLensUpdate> {
        self.replies.code_lens.try_recv().ok()
    }

    /// Ask the server for the document's fold spans (#254). Fire-and-forget
    /// on the same open/change cadence as inlay hints; the reply lands in
    /// [`drain_folding_ranges`] tagged with `seq`. A no-op when no spawned
    /// server advertises a `foldingRangeProvider`, which is what keeps the
    /// editor's indentation scan authoritative there.
    pub fn request_folding_ranges(&self, path: PathBuf, seq: u64) {
        let _ = self.cmd_tx.send(Cmd::RequestFoldingRanges { path, seq });
    }

    pub fn drain_folding_ranges(&self) -> Option<FoldingRangesUpdate> {
        self.replies.folding_ranges.try_recv().ok()
    }

    /// Ask for the document's color values (#254); same fire-and-forget
    /// seq contract as inlay hints, silent when no server advertises a
    /// colorProvider — the swatches simply never appear.
    pub fn request_document_colors(&self, path: PathBuf, seq: u64) {
        let _ = self.cmd_tx.send(Cmd::RequestDocumentColors { path, seq });
    }

    pub fn drain_document_colors(&self) -> Option<DocumentColorsUpdate> {
        self.replies.document_colors.try_recv().ok()
    }

    /// Ask for one color value's alternative spellings (#254); the reply
    /// always arrives (a picker is waiting on it).
    pub fn request_color_presentations(
        &mut self,
        path: PathBuf,
        color: (f32, f32, f32, f32),
        start: (u32, u32),
        end: (u32, u32),
    ) -> u64 {
        self.send_request(|request_id| Cmd::RequestColorPresentations {
            request_id,
            path,
            color,
            start,
            end,
        })
    }

    pub fn drain_color_presentations(&self) -> Option<ColorPresentationsResult> {
        self.replies.color_presentations.try_recv().ok()
    }

    /// Returns and clears the "a server asked us to re-request inlay hints"
    /// flag, set by any client that received `workspace/inlayHint/refresh`.
    /// Mirrors [`take_semantic_refresh`](Self::take_semantic_refresh).
    pub fn take_inlay_refresh(&self) -> bool {
        self.inlay_refresh.swap(false, Ordering::Relaxed)
    }
}

impl WorkerState {
    pub(super) async fn request_inlay_hints(
        &mut self,
        path: PathBuf,
        end: Position,
        seq: u64,
        tx: &std_mpsc::Sender<InlayHintsUpdate>,
    ) {
        let Ok((server_name, client_arc)) =
            self.pick_server(&path, |c| c.supports_inlay_hints).await
        else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.inlay_hints(uri, end).await;
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

    /// Document links (#254): fire-and-forget, seq-gated, silent when
    /// unsupported — Ctrl+click simply keeps meaning Go to Definition.
    pub(super) async fn request_document_links(
        &mut self,
        path: PathBuf,
        seq: u64,
        tx: &std_mpsc::Sender<DocumentLinksUpdate>,
    ) {
        let Ok((server_name, client_arc)) =
            self.pick_server(&path, |c| c.supports_document_link).await
        else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.document_links(uri).await;
            drop(client);
            let links = match resp {
                Ok(Some(ls)) => ls
                    .iter()
                    .filter_map(|l| {
                        let target = l.target.as_ref()?;
                        Some(DocumentLinkItem {
                            line: l.range.start.line,
                            character: l.range.start.character,
                            end_line: l.range.end.line,
                            end_character: l.range.end.character,
                            target: target.to_string(),
                        })
                    })
                    .collect(),
                Ok(None) => Vec::new(),
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] documentLink error: {e:#}"));
                    Vec::new()
                }
            };
            let _ = tx.send(DocumentLinksUpdate { path, seq, links });
        });
    }

    /// Fetch the document's code lenses (#608), resolving the ones the
    /// server left without a command. Silent when no client advertises
    /// `codeLensProvider`.
    pub(super) async fn request_code_lens(
        &mut self,
        path: PathBuf,
        seq: u64,
        tx: &std_mpsc::Sender<CodeLensUpdate>,
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
        let picked = clients.iter().find(|c| c.supports_code_lens).map(|c| {
            (
                c.name.clone(),
                c.client.clone(),
                c.code_lens_resolve,
                c.server_commands.clone(),
            )
        });
        let Some((server_name, client_arc, resolve, server_commands)) = picked else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            // Detached, and bounded: the worker loop's did-change for every
            // keystroke waits on this client's lock, so the lock is held only
            // to take a handle, and a server that never answers a resolve
            // costs at most the deadline (the #533 failure mode otherwise).
            let server = client_arc.lock().await.detached_server();
            let lenses = match tokio::time::timeout(
                CODE_LENS_TIMEOUT,
                LspClient::code_lens_on(server.clone(), uri),
            )
            .await
            {
                Ok(Ok(Some(ls))) => ls,
                Ok(Ok(None)) => Vec::new(),
                Ok(Err(e)) => {
                    log_file::log(&format!("lsp[{server_name}] codeLens error: {e:#}"));
                    return;
                }
                Err(_) => {
                    log_file::log(&format!("lsp[{server_name}] codeLens timed out"));
                    return;
                }
            };
            let deadline = tokio::time::Instant::now() + CODE_LENS_TIMEOUT;
            let mut out = Vec::new();
            for lens in lenses.into_iter().take(MAX_CODE_LENSES) {
                let lens = if lens.command.is_none() && resolve {
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    match tokio::time::timeout(
                        left,
                        LspClient::code_lens_resolve_on(server.clone(), lens),
                    )
                    .await
                    {
                        Ok(Ok(l)) => l,
                        Ok(Err(e)) => {
                            log_file::log(&format!(
                                "lsp[{server_name}] codeLens/resolve error: {e:#}"
                            ));
                            continue;
                        }
                        Err(_) => {
                            log_file::log(&format!(
                                "lsp[{server_name}] codeLens/resolve timed out; showing what resolved"
                            ));
                            break;
                        }
                    }
                } else {
                    lens
                };
                let Some(cmd) = lens.command else {
                    continue;
                };
                if cmd.title.trim().is_empty() {
                    continue;
                }
                out.push(CodeLensItem {
                    line: lens.range.start.line,
                    server_side: server_commands.contains(&cmd.command),
                    title: cmd.title,
                    command: (!cmd.command.is_empty()).then_some(cmd.command),
                    arguments: cmd.arguments.unwrap_or_default(),
                });
            }
            let _ = tx.send(CodeLensUpdate {
                path,
                seq,
                lenses: out,
            });
        });
    }

    /// Fetch the document's fold spans (#254). Silent when the path is
    /// untracked or no client advertises `foldingRangeProvider` — the app
    /// falls back to its indentation/region scan by never hearing back.
    pub(super) async fn request_folding_ranges(
        &mut self,
        path: PathBuf,
        seq: u64,
        tx: &std_mpsc::Sender<FoldingRangesUpdate>,
    ) {
        let Ok((server_name, client_arc)) =
            self.pick_server(&path, |c| c.supports_folding_range).await
        else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.folding_ranges(uri).await;
            drop(client);
            let ranges = match resp {
                Ok(Some(ranges)) => ranges
                    .into_iter()
                    .filter_map(normalise_folding_range)
                    .collect(),
                Ok(None) => Vec::new(),
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] folding_ranges error: {e}"));
                    Vec::new()
                }
            };
            let _ = tx.send(FoldingRangesUpdate { path, seq, ranges });
        });
    }

    /// Document colors (#254): fire-and-forget, seq-gated, silent when
    /// unsupported — the swatch decoration simply never appears.
    pub(super) async fn request_document_colors(
        &mut self,
        path: PathBuf,
        seq: u64,
        tx: &std_mpsc::Sender<DocumentColorsUpdate>,
    ) {
        let Ok((server_name, client_arc)) =
            self.pick_server(&path, |c| c.supports_document_color).await
        else {
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.document_color(uri).await;
            drop(client);
            let colors = match resp {
                Ok(infos) => infos.iter().map(color_item).collect(),
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] documentColor error: {e:#}"));
                    Vec::new()
                }
            };
            let _ = tx.send(DocumentColorsUpdate { path, seq, colors });
        });
    }

    /// Color presentations (#254): always answers — a picker is waiting.
    pub(super) async fn request_color_presentations(
        &mut self,
        request_id: u64,
        path: PathBuf,
        color: (f32, f32, f32, f32),
        start: (u32, u32),
        end: (u32, u32),
        tx: &std_mpsc::Sender<ColorPresentationsResult>,
    ) {
        let empty = |tx: &std_mpsc::Sender<ColorPresentationsResult>| {
            let _ = tx.send(ColorPresentationsResult {
                request_id,
                presentations: Vec::new(),
            });
        };
        let Ok((server_name, client_arc)) =
            self.pick_server(&path, |c| c.supports_document_color).await
        else {
            empty(tx);
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            empty(tx);
            return;
        };
        let lsp_color = lsp_types::Color {
            red: color.0,
            green: color.1,
            blue: color.2,
            alpha: color.3,
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.color_presentations(uri, lsp_color, start, end).await;
            drop(client);
            let presentations = match resp {
                Ok(ps) => ps
                    .into_iter()
                    .map(|p| {
                        // A presentation without an explicit edit means
                        // "replace the color's range with the label".
                        let mut edits: Vec<TextSpanEdit> = Vec::new();
                        match &p.text_edit {
                            Some(te) => edits.push(text_edit_to_span(te)),
                            None => edits.push(TextSpanEdit {
                                start: (start.0 as usize, start.1 as usize),
                                end: (end.0 as usize, end.1 as usize),
                                new_text: p.label.clone(),
                                utf16: true,
                            }),
                        }
                        for te in p.additional_text_edits.iter().flatten() {
                            edits.push(text_edit_to_span(te));
                        }
                        (p.label, edits)
                    })
                    .collect(),
                Err(e) => {
                    log_file::log(&format!(
                        "lsp[{server_name}] colorPresentation error: {e:#}"
                    ));
                    Vec::new()
                }
            };
            let _ = tx.send(ColorPresentationsResult {
                request_id,
                presentations,
            });
        });
    }
}

/// Whether the server advertises a usable `textDocument/documentColor`
/// provider (#254). Same shape rules as `code_action_supported`.
pub(super) fn color_provider_supported(cap: &Option<lsp_types::ColorProviderCapability>) -> bool {
    use lsp_types::ColorProviderCapability as C;
    match cap {
        Some(C::Simple(b)) => *b,
        Some(C::ColorProvider(_)) | Some(C::Options(_)) => true,
        None => false,
    }
}

/// Wire → internal color value (#254): 0..=1 f32 channels scaled to u8
/// for the swatch cell, the raw channels kept for the presentation
/// round trip.
pub(super) fn color_item(ci: &lsp_types::ColorInformation) -> ColorItem {
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    ColorItem {
        line: ci.range.start.line,
        character: ci.range.start.character,
        end_line: ci.range.end.line,
        end_character: ci.range.end.character,
        red: to8(ci.color.red),
        green: to8(ci.color.green),
        blue: to8(ci.color.blue),
        raw: (ci.color.red, ci.color.green, ci.color.blue, ci.color.alpha),
    }
}

/// Whether the server advertises a usable `textDocument/foldingRange`
/// provider (#254). Same shape rules as `code_action_supported`.
pub(super) fn folding_range_supported(
    cap: &Option<lsp_types::FoldingRangeProviderCapability>,
) -> bool {
    use lsp_types::FoldingRangeProviderCapability as F;
    match cap {
        Some(F::Simple(b)) => *b,
        Some(F::FoldingProvider(_)) | Some(F::Options(_)) => true,
        None => false,
    }
}

/// Normalise one LSP wire inlay hint: flatten a parts label into one string,
/// fold the padding flags in as literal spaces, and strip newlines (a label
/// is spliced into a single rendered row, where a line break would corrupt
/// the cell run). The editor consumes the label verbatim after this.
pub(super) fn normalise_inlay_hint(h: lsp_types::InlayHint) -> InlayHintItem {
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

/// Wire → internal fold span (#254). A range whose end does not extend past
/// its start folds nothing and is dropped; `end_character` nuances are
/// deliberately ignored — croft folds whole lines, like VS Code's default.
pub(super) fn normalise_folding_range(r: lsp_types::FoldingRange) -> Option<FoldingRangeItem> {
    let start = r.start_line as usize;
    let end = r.end_line as usize;
    if end <= start {
        return None;
    }
    let kind = match r.kind {
        Some(lsp_types::FoldingRangeKind::Comment) => FoldRangeKind::Comment,
        Some(lsp_types::FoldingRangeKind::Imports) => FoldRangeKind::Imports,
        Some(lsp_types::FoldingRangeKind::Region) => FoldRangeKind::Region,
        None => FoldRangeKind::Other,
    };
    Some(FoldingRangeItem {
        start_line: start,
        end_line: end,
        kind,
    })
}

/// The most lenses resolved per document (#608): each resolve is a round
/// trip, and a generated file can carry thousands.
pub(super) const MAX_CODE_LENSES: usize = 500;

/// How long a codeLens request, and then all its resolves together, may take.
pub(super) const CODE_LENS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
