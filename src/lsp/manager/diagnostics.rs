//! Diagnostics: the problems servers report, pushed or pulled per document and for the
//! whole workspace, with their result ids, debounce and timeouts.

use super::*;

/// Severity of a diagnostic, normalised off the LSP `DiagnosticSeverity`
/// wire enum. Drives the underline colour the editor paints (VS Code: red
/// for errors, yellow for warnings, blue/teal for info & hints).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// One diagnostic from a language server, normalised off the LSP wire type.
/// Positions are LSP UTF-16 (`line`, `character`), zero-based; the editor
/// converts them to character columns against its own buffer, exactly as it
/// does for semantic tokens. A diagnostic can span several lines
/// (`start_line..=end_line`).
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub start_line: u32,
    pub start_char: u32,
    pub end_line: u32,
    pub end_char: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub tags: DiagnosticTags,
    /// The rule the diagnostic reports (#1540): ruff's `F401`, ty's
    /// `invalid-argument-type`, TypeScript's `2322`.
    pub code: Option<DiagnosticCode>,
    /// The server that published it and its diagnostic as sent, `None` for
    /// one croft made itself.
    pub origin: Option<std::sync::Arc<DiagnosticOrigin>>,
}

/// Two diagnostics are the same finding when everything croft shows of them
/// matches. `origin` is left out: a server that reports one finding both
/// pushed and pulled may give each copy different `data`, and the copies
/// must still collapse into one row (`diagnostics_by_source`). The related
/// locations read from it are shown, though (#1523), so they count: a copy
/// that names them is never dropped for one that does not.
impl PartialEq for Diagnostic {
    fn eq(&self, other: &Self) -> bool {
        let Diagnostic {
            start_line,
            start_char,
            end_line,
            end_char,
            severity,
            message,
            tags,
            code,
            origin: _,
        } = self;
        (start_line, start_char, end_line, end_char)
            == (
                &other.start_line,
                &other.start_char,
                &other.end_line,
                &other.end_char,
            )
            && (severity, message, tags, code)
                == (&other.severity, &other.message, &other.tags, &other.code)
            && self.related() == other.related()
    }
}

/// A diagnostic exactly as a language server published it. A code-action
/// request hands it back to that server untouched: tsserver picks its fixes
/// by the error `code` and ruff reads its fix from `data`, so a diagnostic
/// rebuilt from croft's display fields got no quick fix at all (#1030).
#[derive(Debug, PartialEq)]
pub struct DiagnosticOrigin {
    pub server: String,
    pub wire: lsp_types::Diagnostic,
}

impl Diagnostic {
    /// The places the server points at to explain this diagnostic (#1523):
    /// rustc's "immutable borrow occurs here", TypeScript's "'foo' is
    /// declared here". Not stored on croft's diagnostic: built on demand
    /// from the server's own, kept in `origin`; one croft made itself has
    /// none.
    pub fn related(&self) -> Vec<RelatedLocation> {
        let Some(origin) = self.origin.as_deref() else {
            return Vec::new();
        };
        origin
            .wire
            .related_information
            .iter()
            .flatten()
            .filter_map(|r| {
                Some(RelatedLocation {
                    path: r.location.uri.to_file_path().ok()?,
                    line: r.location.range.start.line,
                    character: r.location.range.start.character,
                    message: r.message.clone(),
                })
            })
            .collect()
    }
}

/// One of a diagnostic's related locations (#1523): where its range starts,
/// zero-based with a UTF-16 column like the diagnostic's own position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedLocation {
    pub path: PathBuf,
    pub line: u32,
    pub character: u32,
    pub message: String,
}

impl RelatedLocation {
    /// `file(line, col): message`, one-based, as VS Code appends it to the
    /// diagnostic's hover, with the file relative to `root`.
    pub fn label(&self, root: &Path) -> String {
        let file = self.path.strip_prefix(root).unwrap_or(&self.path);
        format!(
            "{}({}, {}): {}",
            file.display(),
            self.line + 1,
            self.character + 1,
            one_line(&self.message)
        )
    }
}

/// `text` on one line, each line break (`\r\n`, `\n` or `\r`) a space: a
/// related location's message, which the hover and PROBLEMS show one
/// location to a line (#1523).
pub fn one_line(text: &str) -> String {
    text.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

/// A diagnostic's rule code with the server's `source` and the rule's
/// documentation link, what a user needs to silence the rule or look it up
/// (#1540).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticCode {
    pub value: String,
    /// The diagnostic's own `source` (`Ruff`, `ty`, `ts`), if it named one.
    pub source: Option<String>,
    /// `codeDescription.href`, the rule's page.
    pub href: Option<String>,
}

impl DiagnosticCode {
    /// `source(code)` as VS Code writes it after the message, with `source`
    /// standing in when the diagnostic named none; just the code without
    /// either.
    pub fn label(&self, fallback_source: &str) -> String {
        let source = self.source.as_deref().unwrap_or(fallback_source);
        if source.is_empty() {
            self.value.clone()
        } else {
            format!("{source}({})", self.value)
        }
    }
}

/// The LSP `DiagnosticTag`s croft draws (#1522). Servers tag unused names
/// and unreachable code `Unnecessary`, which the editor fades instead of
/// underlining, and `Deprecated` API, which it strikes through.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiagnosticTags {
    pub unnecessary: bool,
    pub deprecated: bool,
}

impl DiagnosticTags {
    /// Whether the diagnostic carries a tag the editor draws on the text.
    pub fn any(self) -> bool {
        self.unnecessary || self.deprecated
    }
}

/// A fresh, COMPLETE set of diagnostics for one document from one server,
/// pushed via `textDocument/publishDiagnostics`. LSP always republishes the
/// whole list (an empty `diagnostics` means "all clear"), so each update
/// replaces the prior set for the same `(path, server)` pair. Keyed by
/// `server` too, so two servers analysing one file (e.g. ty + ruff) layer
/// their diagnostics instead of clobbering each other.
#[derive(Debug, Clone)]
pub struct DiagnosticsUpdate {
    pub path: PathBuf,
    pub server: String,
    pub diagnostics: Vec<Diagnostic>,
    /// The batch answers a pull (`textDocument/diagnostic` or
    /// `workspace/diagnostic`) rather than a `publishDiagnostics` push. A
    /// server can do both with different findings, rust-analyzer pulling
    /// its own analysis and pushing `cargo check`, so the two are kept
    /// apart and neither replaces the other (#1102).
    pub pulled: bool,
}

/// Marks the store entry holding what a server answered to a pull, beside
/// the entry of what it pushed (#1102). rust-analyzer answers a pull with
/// its own analysis and pushes the `cargo check` results; one entry per
/// server let each empty the other, so an open file showed no compile
/// errors. A server name never holds a NUL.
pub(super) const PULLED_DIAGNOSTICS: &str = "\u{0}pulled";

/// The diagnostics store key for an update: its server's name, marked
/// when the update answers a pull.
pub fn diagnostics_store_key(u: &DiagnosticsUpdate) -> String {
    if u.pulled {
        format!("{}{PULLED_DIAGNOSTICS}", u.server)
    } else {
        u.server.clone()
    }
}

/// The server (or other source) a diagnostics store key names.
pub fn diagnostics_source(key: &str) -> &str {
    key.strip_suffix(PULLED_DIAGNOSTICS).unwrap_or(key)
}

/// One path's stored diagnostics, each with the source that reported it.
/// A server's pulled findings leave out any its push already carries, so a
/// server that reports the same finding both ways shows it once.
pub fn diagnostics_by_source(
    by_server: &std::collections::HashMap<String, Vec<Diagnostic>>,
) -> impl Iterator<Item = (&str, &Diagnostic)> {
    by_server.iter().flat_map(move |(key, diags)| {
        let source = diagnostics_source(key);
        let pushed = key
            .ends_with(PULLED_DIAGNOSTICS)
            .then(|| by_server.get(source))
            .flatten();
        diags
            .iter()
            .filter(move |d| !pushed.is_some_and(|p| p.contains(d)))
            .map(move |d| (source, d))
    })
}

impl LspManager {
    /// Pop the next server-pushed diagnostics batch, if any. Each batch is the
    /// complete set for one `(path, server)` pair; the app keeps a per-file,
    /// per-server store so layering and "all clear" (empty batch) both work.
    /// A server no extension registers now (#1842) may still have reports
    /// queued or in flight from before it stopped: only its withdrawals
    /// come through, so nothing it says lands where nothing would clear it.
    pub fn drain_diagnostics(&self) -> Option<DiagnosticsUpdate> {
        loop {
            let u = self.replies.diagnostics.try_recv().ok()?;
            let registered = self
                .registered_servers
                .values()
                .any(|names| names.contains(&u.server.as_str()));
            if registered || u.diagnostics.is_empty() {
                return Some(u);
            }
        }
    }

    /// Test hook: deliver `update` as if a server had published it.
    #[cfg(test)]
    pub fn push_diagnostics_for_test(&self, update: DiagnosticsUpdate) {
        let _ = self.reply_test_tx.diagnostics.send(update);
    }

    /// Returns and clears the "a server asked us to re-pull workspace
    /// diagnostics" flag, set by any client that received
    /// `workspace/diagnostic/refresh` (#533). Mirrors
    /// [`take_semantic_refresh`](Self::take_semantic_refresh).
    pub fn take_diagnostic_refresh(&self) -> bool {
        self.diagnostic_refresh.swap(false, Ordering::Relaxed)
    }

    /// Pull whole-project diagnostics (LSP 3.17 `workspace/diagnostic`) from
    /// every spawned server that advertises `workspaceDiagnostics` (#533).
    ///
    /// Fire-and-forget: reports land in [`drain_diagnostics`] as ordinary
    /// per-file batches, keyed by the same server name the PUSH path uses, so
    /// PROBLEMS shows a file no editor has open without a manual command.
    /// A no-op when no server advertises the capability - the request is
    /// never sent, so a server that pulls per document only is untouched.
    pub fn request_workspace_diagnostics(&self) {
        let _ = self.cmd_tx.send(Cmd::RequestWorkspaceDiagnostics);
    }

    /// Whether the server named `name` reports on the open document `path`
    /// (#851). Read synchronously by the app: the built-in Markdown lint
    /// stands down for a file rumdl, which covers its rules, reports on.
    pub fn document_reported_by(&self, path: &Path, name: &str) -> bool {
        self.capability_support.lock().is_ok_and(|s| {
            s.document_servers
                .get(path)
                .is_some_and(|names| names.iter().any(|n| n == name))
        })
    }
}

impl WorkerState {
    /// Pull whole-project diagnostics from every spawned server that
    /// advertises `workspaceDiagnostics` (#533), and every open document's
    /// from each server that answers `textDocument/diagnostic` (#866): a
    /// server's `workspace/diagnostic/refresh` asks the client to re-pull
    /// everything it holds. Fire-and-forget: the reports land on the
    /// diagnostics channel the push path already feeds.
    pub(super) fn request_workspace_diagnostics(&self) {
        spawn_workspace_pull(
            workspace_pull_targets(self.clients.values().flatten()),
            self.diagnostics_tx.clone(),
        );
        for (path, doc) in &self.docs {
            let Ok(uri) = Url::from_file_path(path) else {
                continue;
            };
            let key: ClientKey = (doc.language, doc.project_root.clone());
            let pulls = self
                .clients
                .get(&key)
                .map(|cs| document_pull_targets(cs.iter(), path, &uri, &self.capability_support))
                .unwrap_or_default();
            spawn_document_pulls(
                pulls,
                self.diagnostics_tx.clone(),
                std::time::Duration::ZERO,
            );
        }
    }
}

impl DiagnosticResultIds {
    /// What to send as `previousResultIds` on the next pull.
    pub fn previous(&self) -> Vec<lsp_types::PreviousResultId> {
        self.by_uri
            .iter()
            .filter_map(|(uri, value)| {
                Url::parse(uri).ok().map(|uri| lsp_types::PreviousResultId {
                    uri,
                    value: value.clone(),
                })
            })
            .collect()
    }

    /// Record the id a `Full` report carried, or forget the file when it
    /// carried none.
    ///
    /// A report with no `result_id` is the server declining to offer
    /// incrementality for that file. Keeping the OLD id would then send back
    /// an id the server never issued for the current content, and a server
    /// that trusts it answers `Unchanged` for a file that has in fact
    /// changed - stale diagnostics that never refresh.
    pub fn record(&mut self, uri: &str, result_id: Option<&str>) {
        match result_id {
            Some(id) => {
                self.by_uri.insert(uri.to_string(), id.to_string());
            }
            None => {
                self.by_uri.remove(uri);
            }
        }
    }

    /// How many files carry an id, so a caller can tell a cold cache from a
    /// warm one without reaching inside.
    pub fn len(&self) -> usize {
        self.by_uri.len()
    }

    /// Whether the cache is cold.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.by_uri.is_empty()
    }
}

/// How long to wait for a `workspace/diagnostic` answer before abandoning
/// the pull (#533 regression). Generous, because a cold whole-project
/// analysis is genuinely slow — this is a stuck-server backstop, not a
/// latency budget.
pub(super) const WORKSPACE_PULL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The servers among `clients` that answer `workspace/diagnostic` (#533).
///
/// The filter is the whole "a server that does not advertise it is untouched"
/// guarantee: a server that pulls per document but not per workspace answers
/// `-32601 Unhandled method`, so it must never be asked.
pub(super) fn workspace_pull_targets<'a>(
    clients: impl Iterator<Item = &'a ManagedClient>,
) -> Vec<WorkspacePull> {
    clients
        .filter(|c| c.supports_workspace_diagnostics)
        // A server that already let one pull expire is never asked again:
        // see `diagnostic_pull_stalled`.
        .filter(|c| !c.diagnostic_pull_stalled.load(Ordering::Relaxed))
        .map(|c| WorkspacePull {
            name: c.name.clone(),
            client: c.client.clone(),
            identifier: c.diagnostic_identifier.clone(),
            result_ids: c.diagnostic_result_ids.clone(),
            stalled: c.diagnostic_pull_stalled.clone(),
        })
        .collect()
}

/// Run a workspace pull against each target off the worker loop (#533).
///
/// Sequential per server, like `request_workspace_symbols`: each pull locks
/// that client, and a whole-project analysis is the kind of request a server
/// answers slowly, so the loop must not be holding the worker while it does.
pub(super) fn spawn_workspace_pull(
    targets: Vec<WorkspacePull>,
    tx: std_mpsc::Sender<DiagnosticsUpdate>,
) {
    spawn_workspace_pull_within(targets, tx, WORKSPACE_PULL_TIMEOUT);
}

/// [`spawn_workspace_pull`] with the ceiling passed in, so a test can watch
/// the timeout path without waiting [`WORKSPACE_PULL_TIMEOUT`] for it.
pub(super) fn spawn_workspace_pull_within(
    targets: Vec<WorkspacePull>,
    tx: std_mpsc::Sender<DiagnosticsUpdate>,
    ceiling: std::time::Duration,
) {
    if targets.is_empty() {
        return;
    }
    tokio::spawn(async move {
        for target in targets {
            // The RESULT-ID lock is what serialises a server's pulls, and it
            // is held end to end: snapshotting the ids without it let two
            // overlapping pulls (the spawn one and a refresh) take the SAME
            // `previous` set, so the second sent ids the first had already
            // replaced and its older report could land after the newer one
            // and win, the app's store being last-write-wins per (file,
            // server).
            //
            // The CLIENT lock is deliberately NOT held across the await. A
            // server that accepts `workspace/diagnostic` and never answers it
            // (ty 0.0.73) would otherwise own the client forever, and
            // `open_doc` — which takes the same lock ON the worker loop,
            // before it registers the document — blocked behind it, wedging
            // every later command including Go to Definition (#533
            // regression, 0.1.939). Cloning the socket keeps the request on
            // the same server while leaving the client free.
            // Lock order: result ids BEFORE the client, never the reverse.
            // The pull task is the only consumer of the ids, so nothing can
            // hold them and want the client today — but reversing this without
            // that still being true would deadlock.
            let mut ids = target.result_ids.lock().await;
            // Re-check retirement HERE, not only in `workspace_pull_targets`.
            // That filter reads the flag when the target list is built; this
            // task may then have waited the full ceiling on the ids behind an
            // earlier pull for the same server, which timed out and retired it
            // meanwhile. Acting on the stale snapshot would send exactly the
            // un-reapable request the retirement exists to prevent.
            if target.stalled.load(Ordering::Relaxed) {
                log_file::log(&format!(
                    "lsp[{}] workspace/diagnostic skipped: retired while queued",
                    target.name
                ));
                continue;
            }
            let previous = ids.previous();
            let server = {
                let client = target.client.lock().await;
                client.detached_server()
            };
            log_file::log(&format!(
                "lsp[{}] workspace/diagnostic pull, {} previous result id(s)",
                target.name,
                previous.len()
            ));
            // Bounded, because a pull can go unanswered indefinitely: `ty`
            // advertises `workspaceDiagnostics` and then never replies
            // (checked on 0.0.73 and again on 0.0.82). Without a ceiling the
            // task stays parked on a response that will not come and the
            // result ids stay locked behind it, so the NEXT pull for that
            // server never starts either. Giving up frees the task and the
            // ids — but NOT the mainloop's `outgoing` entry, which only a
            // response with that id can drain, so the abandoned request leaks
            // one entry. That is why a timeout also retires the server from
            // future pulls: one leak per client, not one per refresh.
            let resp = match tokio::time::timeout(
                ceiling,
                crate::lsp::client::LspRequests::workspace_diagnostics_on(
                    server,
                    target.identifier.clone(),
                    previous,
                ),
            )
            .await
            {
                Ok(resp) => resp,
                Err(_) => {
                    target.stalled.store(true, Ordering::Relaxed);
                    log_file::log(&format!(
                        "lsp[{}] workspace/diagnostic timed out after {}s; \
                         retiring it from workspace pulls for this session",
                        target.name,
                        ceiling.as_secs()
                    ));
                    continue;
                }
            };
            match resp {
                Ok(result) => {
                    let forwarded = apply_workspace_report(&target.name, result, &mut ids, &tx);
                    log_file::log(&format!(
                        "lsp[{}] workspace/diagnostic: {forwarded} file(s) reported, {} id(s) cached",
                        target.name,
                        ids.len()
                    ));
                }
                Err(e) => {
                    log_file::log(&format!(
                        "lsp[{}] workspace/diagnostic error: {e:#}",
                        target.name
                    ));
                }
            }
            drop(ids);
        }
    });
}

/// Fold one server's `workspace/diagnostic` answer into its result-id cache
/// and the app-facing diagnostics stream, returning how many files it
/// reported on (#533).
///
/// The reports go out on the same channel and under the same server name as
/// the PUSH path, so PROBLEMS needs no third producer: the app already stores
/// diagnostics per (file, server) and replaces a server's set wholesale. What
/// a pull adds is files no editor has open, which that store already carries
/// and `rebuild_problems` already walks.
pub(super) fn apply_workspace_report(
    server: &str,
    result: lsp_types::WorkspaceDiagnosticReportResult,
    ids: &mut DiagnosticResultIds,
    tx: &std_mpsc::Sender<DiagnosticsUpdate>,
) -> usize {
    let items = match result {
        lsp_types::WorkspaceDiagnosticReportResult::Report(r) => r.items,
        lsp_types::WorkspaceDiagnosticReportResult::Partial(p) => p.items,
    };
    let mut forwarded = 0;
    for item in items {
        match item {
            // Unchanged is what `previousResultIds` bought: the server is
            // saying the id we sent still describes the file, so there is
            // nothing to re-process. Its reissued id still has to be recorded
            // - the server may rotate it on every answer.
            lsp_types::WorkspaceDocumentDiagnosticReport::Unchanged(u) => {
                ids.record(
                    u.uri.as_str(),
                    Some(u.unchanged_document_diagnostic_report.result_id.as_str()),
                );
            }
            lsp_types::WorkspaceDocumentDiagnosticReport::Full(f) => {
                let report = f.full_document_diagnostic_report;
                ids.record(f.uri.as_str(), report.result_id.as_deref());
                // A URI that is not a file (a virtual or untitled document)
                // has no row to show and no editor to paint.
                let Ok(path) = f.uri.to_file_path() else {
                    continue;
                };
                let diagnostics = report
                    .items
                    .iter()
                    .map(|d| crate::lsp::client::convert_diagnostic(server, d))
                    .collect();
                // An EMPTY list is the server saying this file is clear now,
                // which is how a fixed file loses its rows - the app's store
                // removes the server's entry on exactly that. Skipping empties
                // would leave the last report's problems on screen forever.
                let _ = tx.send(DiagnosticsUpdate {
                    path,
                    server: server.to_string(),
                    diagnostics,
                    pulled: true,
                });
                forwarded += 1;
            }
        }
    }
    forwarded
}

/// Settle time before a document pull goes out after an edit (#866): a
/// burst of keystrokes sends one pull, for its last edit, not one per key.
pub(super) const DOCUMENT_PULL_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(150);

/// Ceiling on one `textDocument/diagnostic` pull (#866). A server that lets
/// one expire is retired from document pulls, for the reason
/// `ManagedClient::diagnostic_pull_stalled` gives.
pub(super) const DOCUMENT_PULL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Generation numbers for document pulls (#866), unique for the whole
/// session. A per-document count would start over when a document is
/// closed and reopened, and a pull from before the close could then pass
/// for the reopened buffer's latest.
pub(super) static NEXT_DOCUMENT_PULL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// One `textDocument/diagnostic` pull (#866): a server that answers it, the
/// document, and the generation this pull stands for.
pub(super) struct DocumentPull {
    name: String,
    client: Arc<TokioMutex<LspClient>>,
    identifier: Option<String>,
    stalled: Arc<AtomicBool>,
    generations: Arc<StdMutex<HashMap<PathBuf, u64>>>,
    generation: u64,
    path: PathBuf,
    uri: Url,
    /// Whether the server also answers the workspace pull, which keeps its
    /// sets fresh after it is retired from document pulls.
    reports_per_workspace: bool,
    /// Where the documents the server reports on are recorded (#851).
    support: CapabilitySupport,
}

impl DocumentPull {
    /// Whether no later pull for this document has been issued since this
    /// one. Otherwise an edit overtook it, and its answer would describe text
    /// the buffer no longer holds.
    pub(super) fn is_current(&self) -> bool {
        let generations = self.generations.lock().unwrap_or_else(|e| e.into_inner());
        generations.get(&self.path) == Some(&self.generation)
    }

    /// Once a pull expired and retired the server from document pulls: it
    /// reports on none of its documents any more, so it leaves their
    /// reporter lists (#851; the built-in Markdown lint comes back where it
    /// stood down for rumdl), and the sets it pulled for them are withdrawn,
    /// as nothing will refresh them after an edit. A server that also
    /// reports per workspace keeps those.
    pub(super) fn retire(&self, tx: &std_mpsc::Sender<DiagnosticsUpdate>) {
        let served: Vec<PathBuf> = self
            .generations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        if let Ok(mut support) = self.support.lock() {
            for path in &served {
                if let Some(names) = support.document_servers.get_mut(path) {
                    names.retain(|n| *n != self.name);
                }
            }
        }
        if self.reports_per_workspace {
            return;
        }
        for path in served {
            let _ = tx.send(DiagnosticsUpdate {
                path,
                server: self.name.clone(),
                diagnostics: Vec::new(),
                pulled: true,
            });
        }
    }
}

/// The pulls an opened or edited document calls for (#866): one per server
/// among `clients` that answers `textDocument/diagnostic` and has not been
/// retired, each stamped with a new generation so any older pull for the
/// document stands down.
pub(super) fn document_pull_targets<'a>(
    clients: impl Iterator<Item = &'a ManagedClient>,
    path: &Path,
    uri: &Url,
    support: &CapabilitySupport,
) -> Vec<DocumentPull> {
    clients
        .filter(|c| c.supports_document_diagnostics)
        .filter(|c| !c.document_pull_stalled.load(Ordering::Relaxed))
        .map(|c| {
            let generation = {
                let mut generations = c
                    .document_pull_generations
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let next = NEXT_DOCUMENT_PULL.fetch_add(1, Ordering::Relaxed);
                generations.insert(path.to_path_buf(), next);
                next
            };
            DocumentPull {
                name: c.name.clone(),
                client: c.client.clone(),
                identifier: c.diagnostic_identifier.clone(),
                stalled: c.document_pull_stalled.clone(),
                generations: c.document_pull_generations.clone(),
                generation,
                path: path.to_path_buf(),
                uri: uri.clone(),
                reports_per_workspace: c.supports_workspace_diagnostics,
                support: support.clone(),
            }
        })
        .collect()
}

/// Run each pull off the worker loop once `debounce` has passed (#866),
/// forwarding its report under the server's name on the channel the push
/// path feeds, so the app stores it like any published set.
pub(super) fn spawn_document_pulls(
    targets: Vec<DocumentPull>,
    tx: std_mpsc::Sender<DiagnosticsUpdate>,
    debounce: std::time::Duration,
) {
    spawn_document_pulls_within(targets, tx, debounce, DOCUMENT_PULL_TIMEOUT);
}

/// [`spawn_document_pulls`] with the ceiling passed in, so a test can watch
/// the timeout path without waiting [`DOCUMENT_PULL_TIMEOUT`] for it.
pub(super) fn spawn_document_pulls_within(
    targets: Vec<DocumentPull>,
    tx: std_mpsc::Sender<DiagnosticsUpdate>,
    debounce: std::time::Duration,
    ceiling: std::time::Duration,
) {
    for target in targets {
        let tx = tx.clone();
        tokio::spawn(async move {
            if !debounce.is_zero() {
                tokio::time::sleep(debounce).await;
            }
            // Overtaken by a later edit while it waited, or the server was
            // retired meanwhile: nothing to ask.
            if !target.is_current() || target.stalled.load(Ordering::Relaxed) {
                return;
            }
            // As in the workspace pull, the client is not held across the
            // await: a server that never answers must not wedge `open_doc`.
            let server = {
                let client = target.client.lock().await;
                client.detached_server()
            };
            let resp = tokio::time::timeout(
                ceiling,
                crate::lsp::client::LspRequests::document_diagnostics_on(
                    server,
                    target.uri.clone(),
                    target.identifier.clone(),
                ),
            )
            .await;
            let report = match resp {
                Ok(Ok(report)) => report,
                Ok(Err(e)) => {
                    log_file::log(&format!(
                        "lsp[{}] textDocument/diagnostic error: {e:#}",
                        target.name
                    ));
                    return;
                }
                Err(_) => {
                    target.stalled.store(true, Ordering::Relaxed);
                    log_file::log(&format!(
                        "lsp[{}] textDocument/diagnostic timed out after {}s; \
                         retiring it from document pulls for this session",
                        target.name,
                        ceiling.as_secs()
                    ));
                    target.retire(&tx);
                    return;
                }
            };
            if !target.is_current() {
                return;
            }
            let Some(items) = document_report_items(report) else {
                return;
            };
            let _ = tx.send(DiagnosticsUpdate {
                path: target.path.clone(),
                server: target.name.clone(),
                diagnostics: items
                    .iter()
                    .map(|d| crate::lsp::client::convert_diagnostic(&target.name, d))
                    .collect(),
                pulled: true,
            });
        });
    }
}

/// The diagnostics a `textDocument/diagnostic` answer carries for the
/// document itself (#866). `None` for an answer with none of its own: an
/// `Unchanged` report, which croft never invites by sending a previous result
/// id, or a bare partial result.
pub(super) fn document_report_items(
    result: lsp_types::DocumentDiagnosticReportResult,
) -> Option<Vec<lsp_types::Diagnostic>> {
    match result {
        lsp_types::DocumentDiagnosticReportResult::Report(
            lsp_types::DocumentDiagnosticReport::Full(full),
        ) => Some(full.full_document_diagnostic_report.items),
        _ => None,
    }
}

/// The server's `diagnosticProvider.identifier`, echoed back on every pull.
///
/// A server that registered several diagnostic sources uses it to tell which
/// source a previous result id belongs to, so sending `None` where it
/// published an identifier throws its incrementality away. Read from both
/// capability variants for the same reason
/// [`workspace_diagnostics_supported`] is.
pub(super) fn diagnostic_identifier_of(
    cap: &Option<DiagnosticServerCapabilities>,
) -> Option<String> {
    match cap {
        Some(DiagnosticServerCapabilities::Options(o)) => o.identifier.clone(),
        Some(DiagnosticServerCapabilities::RegistrationOptions(r)) => {
            r.diagnostic_options.identifier.clone()
        }
        None => None,
    }
}

/// Whether the server answers `workspace/diagnostic`, the LSP 3.17 PULL of
/// whole-project diagnostics (#533).
///
/// The inner `workspace_diagnostics` flag, NOT `Option::is_some`. A server
/// that supports only per-document pulls still advertises
/// `diagnosticProvider` with that flag `false`; reading the outer `Option`
/// would send it a request it does not implement and get `-32601 Unhandled
/// method` back, which is the exact shape that made croft call
/// `textDocument/declaration` on vtsls.
///
/// Both capability variants carry the same options, so both are unwrapped
/// rather than treating `RegistrationOptions` as an unconditional yes.
pub(super) fn workspace_diagnostics_supported(cap: &Option<DiagnosticServerCapabilities>) -> bool {
    match cap {
        Some(DiagnosticServerCapabilities::Options(o)) => o.workspace_diagnostics,
        Some(DiagnosticServerCapabilities::RegistrationOptions(r)) => {
            r.diagnostic_options.workspace_diagnostics
        }
        None => false,
    }
}
