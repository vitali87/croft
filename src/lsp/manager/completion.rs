//! Completion and its kin: completion lists and resolving an item, trigger characters,
//! signature help, and inline (ghost text) completion.

use super::*;

#[derive(Debug, Clone, Default)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: Option<String>,
    pub filter_text: Option<String>,
    pub kind: Option<CompletionItemKind>,
    /// The `insert_text` is a snippet body (`$1`/`$0` tab stops) rather than
    /// literal text: an LSP item with `insertTextFormat: Snippet`, or a user
    /// snippet injected into the popup. The app expands it through the editor's
    /// snippet engine on accept instead of inserting it verbatim.
    pub is_snippet: bool,
    /// The server's own edit for this item (`textEdit`; for an insert/
    /// replace edit, its insert range), UTF-16 columns. It can reach back
    /// before the typed word (TypeScript's `?.foo` replaces the `.`), which
    /// replacing the word alone got wrong.
    pub text_edit: Option<TextSpanEdit>,
    /// `additionalTextEdits`: an auto-import or `#include` elsewhere in the
    /// file, applied along with the item.
    pub additional_edits: Vec<TextSpanEdit>,
    /// The item's documentation, Markdown or plain text as the server sent
    /// it, for the popup's details pane (#1527).
    pub documentation: Option<String>,
    /// The item as the server listed it, `data` and all, when the server
    /// resolves items lazily and the list left its imports out (#928):
    /// vtsls sends an auto-import only from `completionItem/resolve`, which
    /// the accept sends this back for.
    pub resolve: Option<Arc<lsp_types::CompletionItem>>,
}

/// A completion reply. A `completionItem/resolve` reply is one too, its
/// single item the resolved one (#928).
#[derive(Debug)]
pub struct CompletionResult {
    pub request_id: u64,
    pub path: PathBuf,
    pub items: Vec<CompletionItem>,
    /// The server capped the list (`isIncomplete`): typing on must ask it
    /// again rather than only filter these items (#1529).
    pub is_incomplete: bool,
}

/// Why a completion is asked for, sent as the request's `context`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionTrigger {
    /// Ctrl+Space, or a word begun with quick suggestions on (#929).
    Invoked,
    /// A character the server lists in `triggerCharacters` was typed
    /// (#929): TypeScript's `"` and `/` in an import path, `.` for members.
    Character(char),
    /// Re-asking because the last list was incomplete (#1529).
    Incomplete,
}

/// One callable signature for the Signature Help popup, normalised off the LSP
/// `SignatureInformation` so the widget never touches lsp-types. `active_param`
/// is the (start, end) char range within `label` to bold (the parameter the
/// caret currently sits in), already resolved against the active signature.
#[derive(Clone, Debug)]
pub struct SignatureInfo {
    pub label: String,
    pub active_param: Option<(usize, usize)>,
}

#[derive(Debug)]
pub struct SignatureHelpResult {
    pub request_id: u64,
    pub path: PathBuf,
    pub signatures: Vec<SignatureInfo>,
    pub active_signature: usize,
}

/// One `textDocument/inlineCompletion` answer (LSP 3.18, #607): the
/// server's first suggestion, or `None` when it had nothing or failed.
#[derive(Debug)]
pub struct InlineCompletionResult {
    pub request_id: u64,
    pub item: Option<InlineItem>,
}

/// An inline completion as plain text, and the UTF-16 span
/// `(start_line, start_char, end_line, end_char)` it replaces when the
/// server gave one.
#[derive(Debug, PartialEq, Eq)]
pub struct InlineItem {
    pub text: String,
    pub range: Option<(u32, u32, u32, u32)>,
}

impl LspManager {
    /// Ask for completions at a UTF-16 position, saying why: a re-ask as
    /// the user types into a list the server marked incomplete (#1529), or
    /// a typed trigger character (#929).
    pub fn request_completion(
        &mut self,
        path: PathBuf,
        line: u32,
        character: u32,
        trigger: CompletionTrigger,
    ) -> u64 {
        self.send_request(|request_id| Cmd::RequestCompletion {
            request_id,
            path,
            line,
            character,
            trigger,
        })
    }

    /// Ask the completion server for the rest of an item it listed (#928).
    /// The reply comes through [`Self::drain_completion`] with this id, its
    /// one item the resolved one, or none when the server failed.
    pub fn request_completion_resolve(
        &mut self,
        path: PathBuf,
        item: Arc<lsp_types::CompletionItem>,
    ) -> u64 {
        self.send_request(|request_id| Cmd::ResolveCompletion {
            request_id,
            path,
            item,
        })
    }

    pub fn drain_completion(&self) -> Option<CompletionResult> {
        self.replies.completion.try_recv().ok()
    }

    pub fn request_signature_help(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        self.send_request(|request_id| Cmd::RequestSignatureHelp {
            request_id,
            path,
            line,
            character,
        })
    }

    pub fn drain_signature_help(&self) -> Option<SignatureHelpResult> {
        self.replies.signature_help.try_recv().ok()
    }

    /// Ask the server that offers inline completions for ghost text at the
    /// caret (#607). Returns the id the reply echoes; no reply arrives for a
    /// document no such server has.
    pub fn request_inline_completion(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        self.send_request(|request_id| Cmd::RequestInlineCompletion {
            request_id,
            path,
            line,
            character,
        })
    }

    pub fn drain_inline_completion(&self) -> Option<InlineCompletionResult> {
        self.replies.inline_completion.try_recv().ok()
    }

    /// Test hook: deliver `result` as if a server had answered.
    #[cfg(test)]
    pub fn push_inline_completion_for_test(&self, result: InlineCompletionResult) {
        let _ = self.reply_test_tx.inline_completion.send(result);
    }

    /// Whether a running server at `path`'s project root offers inline
    /// completions (#607), so the app asks it rather than a model.
    pub fn document_offers_inline_completion(&self, path: &Path) -> bool {
        let Some(lang) = path_to_language(path) else {
            return false;
        };
        self.capability_support.lock().is_ok_and(|support| {
            support
                .document_roots
                .get(path)
                .is_some_and(|root| support.inline_completion.contains(&(lang, root.clone())))
        })
    }

    /// Test hook: `path` is open at its directory, whose servers offer
    /// inline completions, unstarted.
    #[cfg(test)]
    pub fn set_inline_completion_for_test(&self, path: &Path) {
        let Some(lang) = path_to_language(path) else {
            return;
        };
        let root = path.parent().unwrap_or(path).to_path_buf();
        if let Ok(mut support) = self.capability_support.lock() {
            support
                .document_roots
                .insert(path.to_path_buf(), root.clone());
            support.inline_completion.insert((lang, root));
        }
    }

    /// The characters that ask for completions as they are typed in
    /// `path` (#929): the `triggerCharacters` of the completion server at
    /// the file's project root, empty when it lists none. `None` while the
    /// file is open on no running server that completes, where nothing
    /// typed asks.
    pub fn completion_triggers(&self, path: &Path) -> Option<String> {
        let lang = path_to_language(path)?;
        let support = self.capability_support.lock().ok()?;
        let root = support.document_roots.get(path)?.clone();
        support.completion_triggers.get(&(lang, root)).cloned()
    }

    /// Test hook: `path` is open at its directory, whose completion server
    /// lists `triggers`, unstarted.
    #[cfg(test)]
    pub fn set_completion_triggers_for_test(&self, path: &Path, triggers: &str) {
        let Some(lang) = path_to_language(path) else {
            return;
        };
        let root = path.parent().unwrap_or(path).to_path_buf();
        if let Ok(mut support) = self.capability_support.lock() {
            support
                .document_roots
                .insert(path.to_path_buf(), root.clone());
            support
                .completion_triggers
                .insert((lang, root), triggers.to_string());
        }
    }
}

impl WorkerState {
    pub(super) async fn request_completion(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        trigger: CompletionTrigger,
        tx: &std_mpsc::Sender<CompletionResult>,
    ) {
        let Some(doc) = self.docs.get(&path) else {
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        // Re-probe so a server installed since the file was opened (e.g. the
        // managed vtsls finishing its lazy background install) is picked up
        // without reopening the file. Cheap once the list is non-empty.
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
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
                is_incomplete: false,
            });
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "completion request id={request_id} server={server_name} path={} line={line} char={character} trigger={trigger:?}",
            path.display()
        ));
        tokio::spawn(async move {
            let (mut client, resolves) = {
                let client = client_arc.lock().await;
                let options = client.capabilities().completion_provider.as_ref();
                let resolves = options.and_then(|o| o.resolve_provider) == Some(true);
                (client.requests(), resolves)
            };
            let resp = client.completion(uri, line, character, trigger).await;
            drop(client);
            let convert = |item| listed_item(item, resolves);
            let (is_incomplete, items): (Option<bool>, Vec<CompletionItem>) = match resp {
                Ok(Some(CompletionResponse::Array(items))) => {
                    (None, items.into_iter().map(convert).collect())
                }
                Ok(Some(CompletionResponse::List(list))) => (
                    Some(list.is_incomplete),
                    list.items.into_iter().map(convert).collect(),
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
                is_incomplete: is_incomplete == Some(true),
            });
        });
    }

    /// Send `completionItem/resolve` to the server `request_completion`
    /// asks, the one that listed the item (#928), and its answer back as a
    /// one-item completion reply, with no item when the server failed.
    pub(super) async fn resolve_completion(
        &mut self,
        request_id: u64,
        path: PathBuf,
        item: Arc<lsp_types::CompletionItem>,
        tx: &std_mpsc::Sender<CompletionResult>,
    ) {
        let Ok((server_name, client_arc)) =
            self.pick_server(&path, |c| c.supports_completion).await
        else {
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let items = match client.completion_resolve((*item).clone()).await {
                Ok(item) => vec![into_item(item)],
                Err(e) => {
                    log_file::log(&format!(
                        "lsp[{server_name}] completion_resolve error: {e:#}"
                    ));
                    Vec::new()
                }
            };
            let _ = tx.send(CompletionResult {
                request_id,
                path,
                items,
                is_incomplete: false,
            });
        });
    }

    pub(super) async fn request_signature_help(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<SignatureHelpResult>,
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
        // Every server that advertises the capability, in registration order.
        // Some servers (e.g. Astral's `ty`) advertise `signatureHelpProvider`
        // but return nothing, so we try each in turn and take the FIRST that
        // actually answers, rather than stopping at an empty stub.
        let candidates: Vec<(String, Arc<TokioMutex<LspClient>>)> = clients
            .iter()
            .filter(|c| c.supports_signature_help)
            .map(|c| (c.name.clone(), c.client.clone()))
            .collect();
        if candidates.is_empty() {
            log_file::log(&format!(
                "signatureHelp request id={request_id} dropped: no client advertises signature_help_provider for {lang:?}"
            ));
            let _ = tx.send(SignatureHelpResult {
                request_id,
                path,
                signatures: Vec::new(),
                active_signature: 0,
            });
            return;
        }
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        let path_clone = path.clone();
        log_file::log(&format!(
            "signatureHelp request id={request_id} servers={:?} line={line} char={character}",
            candidates
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
        ));
        tokio::spawn(async move {
            let mut signatures = Vec::new();
            let mut active_signature = 0;
            for (name, client_arc) in &candidates {
                let mut client = client_arc.lock().await.requests();
                let resp = client.signature_help(uri.clone(), line, character).await;
                drop(client);
                let (sigs, active) = match resp {
                    Ok(Some(help)) => normalise_signature_help(help),
                    Ok(None) => (Vec::new(), 0),
                    Err(e) => {
                        log_file::log(&format!("lsp[{name}] signatureHelp error: {e}"));
                        (Vec::new(), 0)
                    }
                };
                log_file::log(&format!(
                    "signatureHelp response id={request_id} server={name} count={}",
                    sigs.len()
                ));
                if !sigs.is_empty() {
                    signatures = sigs;
                    active_signature = active;
                    break;
                }
            }
            let _ = tx.send(SignatureHelpResult {
                request_id,
                path: path_clone,
                signatures,
                active_signature,
            });
        });
    }

    /// Inline completion at the caret (#607). Silent on every early-out,
    /// like linked editing: the app asks once per caret and edit, and a
    /// request left unanswered is simply replaced by the next one.
    pub(super) async fn request_inline_completion(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<InlineCompletionResult>,
    ) {
        let Ok((server_name, client_arc)) = self
            .pick_server(&path, |c| c.supports_inline_completion)
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
            let resp = client.inline_completion(uri, line, character).await;
            drop(client);
            let item = match resp {
                Ok(Some(answer)) => first_inline_item(answer),
                Ok(None) => None,
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] inlineCompletion error: {e:#}"));
                    None
                }
            };
            let _ = tx.send(InlineCompletionResult { request_id, item });
        });
    }
}

pub(super) fn into_item(item: lsp_types::CompletionItem) -> CompletionItem {
    let is_snippet = item.insert_text_format == Some(lsp_types::InsertTextFormat::SNIPPET);
    let text_edit = item.text_edit.as_ref().map(|te| match te {
        lsp_types::CompletionTextEdit::Edit(e) => text_edit_to_span(e),
        lsp_types::CompletionTextEdit::InsertAndReplace(e) => text_edit_to_span(&TextEdit {
            range: e.insert,
            new_text: e.new_text.clone(),
        }),
    });
    let additional_edits = item
        .additional_text_edits
        .iter()
        .flatten()
        .map(text_edit_to_span)
        .collect();
    let documentation = item.documentation.map(|d| match d {
        lsp_types::Documentation::String(s) => s,
        lsp_types::Documentation::MarkupContent(m) => m.value,
    });
    // A server that puts the signature only in `labelDetails` (#1527)
    // still gets it shown: its `detail` (the suffix after the label, often
    // the parameter list) then its `description` (a qualifier), both kept.
    let detail = item.detail.or_else(|| {
        item.label_details.and_then(|l| {
            let parts: Vec<String> = [l.detail, l.description]
                .into_iter()
                .flatten()
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            (!parts.is_empty()).then(|| parts.join(" "))
        })
    });
    CompletionItem {
        label: item.label,
        detail,
        documentation,
        insert_text: item.insert_text,
        filter_text: item.filter_text,
        kind: item.kind,
        is_snippet,
        text_edit,
        additional_edits,
        resolve: None,
    }
}

/// [`into_item`] for an item of a list, kept whole for
/// `completionItem/resolve` when its server `resolves` items and the list
/// left its imports out (#928).
pub(super) fn listed_item(item: lsp_types::CompletionItem, resolves: bool) -> CompletionItem {
    let imports = item.additional_text_edits.as_ref();
    let resolve = (resolves && imports.is_none_or(Vec::is_empty)).then(|| Arc::new(item.clone()));
    CompletionItem {
        resolve,
        ..into_item(item)
    }
}

/// The first inline completion with any text (#607), as plain text: a
/// snippet's placeholders become their defaults, since ghost text has no
/// tab stops. Its range is kept only when it stays on one line, as the
/// protocol requires.
pub(super) fn first_inline_item(answer: lsp_types::InlineCompletionResponse) -> Option<InlineItem> {
    let items = match answer {
        lsp_types::InlineCompletionResponse::Array(items) => items,
        lsp_types::InlineCompletionResponse::List(list) => list.items,
    };
    items.into_iter().find_map(|item| {
        let text = if item.insert_text_format == Some(lsp_types::InsertTextFormat::SNIPPET) {
            crate::snippets::parse_body(&item.insert_text, &|_| None).text
        } else {
            item.insert_text
        };
        let range = match item.range {
            Some(r) if r.start.line != r.end.line => return None,
            r => r.map(|r| (r.start.line, r.start.character, r.end.line, r.end.character)),
        };
        (!text.trim().is_empty()).then_some(InlineItem { text, range })
    })
}

/// Whether the server advertises `signatureHelpProvider`.
pub(super) fn signature_help_supported(cap: &Option<lsp_types::SignatureHelpOptions>) -> bool {
    cap.is_some()
}

/// The completion `triggerCharacters` a server lists (#929), as one string.
/// The protocol allows a longer trigger, but one key types one character,
/// so `->` is left out rather than read as `-` and `>`.
pub(super) fn completion_triggers_of(caps: &ServerCapabilities) -> String {
    let options = caps.completion_provider.as_ref();
    let triggers = options.and_then(|o| o.trigger_characters.as_deref());
    let single = |t: &&String| t.chars().count() == 1;
    let triggers = triggers.unwrap_or_default().iter().filter(single);
    triggers.map(String::as_str).collect()
}

/// Flatten an LSP `SignatureHelp` into the widget-facing [`SignatureInfo`]
/// list, resolving each parameter label to a (start, end) char range within
/// the signature label so the popup can bold the active parameter. The active
/// parameter is taken per-signature when set, else from the help-level field.
/// ponytail: parameter label offsets are UTF-16 per the spec; treated as char
/// offsets here, which is exact for ASCII/BMP signatures (the overwhelming
/// common case) and only off for astral-plane identifiers.
pub(super) fn normalise_signature_help(
    help: lsp_types::SignatureHelp,
) -> (Vec<SignatureInfo>, usize) {
    use lsp_types::ParameterLabel;
    let help_active_param = help.active_parameter;
    let signatures: Vec<SignatureInfo> = help
        .signatures
        .into_iter()
        .map(|sig| {
            let active_idx = sig.active_parameter.or(help_active_param).unwrap_or(0) as usize;
            let active_param = sig.parameters.as_ref().and_then(|params| {
                params.get(active_idx).and_then(|p| match &p.label {
                    ParameterLabel::LabelOffsets([s, e]) => Some((*s as usize, *e as usize)),
                    ParameterLabel::Simple(text) => sig.label.find(text.as_str()).map(|byte| {
                        let start = sig.label[..byte].chars().count();
                        (start, start + text.chars().count())
                    }),
                })
            });
            SignatureInfo {
                label: sig.label,
                active_param,
            }
        })
        .collect();
    let active_signature =
        (help.active_signature.unwrap_or(0) as usize).min(signatures.len().saturating_sub(1));
    (signatures, active_signature)
}
