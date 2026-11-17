//! Renaming: rename and prepare rename, and the edits servers make when files are renamed.

use super::*;

#[derive(Debug)]
pub struct RenameResult {
    pub request_id: u64,
    /// `None` when the server declined or returned no edits. Otherwise the
    /// per-file, char-indexed spans to apply across the workspace.
    pub edits: Option<Vec<(PathBuf, Vec<TextSpanEdit>)>>,
}

/// One Explorer rename or move, as the servers see it (#610).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRenameOp {
    pub old: PathBuf,
    pub new: PathBuf,
    pub is_dir: bool,
}

/// The servers' combined answer to `willRenameFiles` (#610). Always sent,
/// so the pending move never waits on a server that is not interested.
#[derive(Debug)]
pub struct WillRenameFilesResult {
    pub request_id: u64,
    /// Per-file, char-indexed spans to apply before the move.
    pub edits: Vec<(PathBuf, Vec<TextSpanEdit>)>,
}

/// A `textDocument/prepareRename` verdict (#254). Always answered — the
/// rename prompt is waiting on it. `unsupported` routes the app to the
/// plain word-under-cursor prompt; `error` carries the server's own
/// refusal (fail fast with ITS message, not a generic one); otherwise
/// `target` holds the validated UTF-16 range and optional placeholder,
/// and `None` means the server said "nothing renameable here".
#[derive(Debug)]
pub struct PrepareRenameResult {
    pub request_id: u64,
    pub unsupported: bool,
    pub error: Option<String>,
    /// `((start_line, start_char, end_line, end_char), placeholder)`.
    #[allow(clippy::type_complexity)]
    pub target: Option<((u32, u32, u32, u32), Option<String>)>,
}

impl LspManager {
    /// Whether any running server wants `willRenameFiles` (#610). When none
    /// does, an Explorer move runs at once instead of waiting on an answer
    /// that can only be empty.
    pub fn any_server_wants_file_renames(&self) -> bool {
        self.capability_support
            .lock()
            .is_ok_and(|s| s.will_rename.values().any(|w| *w))
    }

    pub fn request_rename(
        &mut self,
        path: PathBuf,
        line: u32,
        character: u32,
        new_name: String,
    ) -> u64 {
        self.send_request(|request_id| Cmd::RequestRename {
            request_id,
            path,
            line,
            character,
            new_name,
        })
    }

    /// Validate a rename position and fetch its placeholder (#254);
    /// the reply always arrives via [`Self::drain_prepare_rename`].
    pub fn request_prepare_rename(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        self.send_request(|request_id| Cmd::RequestPrepareRename {
            request_id,
            path,
            line,
            character,
        })
    }

    pub fn drain_prepare_rename(&self) -> Option<PrepareRenameResult> {
        self.prepare_rename_rx.try_recv().ok()
    }

    pub fn drain_rename(&self) -> Option<RenameResult> {
        self.rename_rx.try_recv().ok()
    }

    /// Ask every interested server for its edit before `renames` happen
    /// (#610). Always answered, with no edits when no server cares, so the
    /// caller can wait on it.
    pub fn request_will_rename_files(&mut self, renames: Vec<FileRenameOp>) -> u64 {
        self.send_request(|request_id| Cmd::RequestWillRenameFiles {
            request_id,
            renames,
        })
    }

    pub fn drain_will_rename_files(&self) -> Option<WillRenameFilesResult> {
        self.will_rename_files_rx.try_recv().ok()
    }

    /// Tell interested servers that `renames` happened (#610).
    pub fn did_rename_files(&mut self, renames: Vec<FileRenameOp>) {
        let _ = self.cmd_tx.send(Cmd::DidRenameFiles { renames });
    }
}

impl WorkerState {
    pub(super) async fn request_rename(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        new_name: String,
        tx: &std_mpsc::Sender<RenameResult>,
    ) {
        // Every early exit answers with no edits, so the caller's "Renaming
        // symbol" always resolves (#1193).
        let unanswerable = || {
            let _ = tx.send(RenameResult {
                request_id,
                edits: None,
            });
        };
        let Ok((server_name, client_arc)) = self.pick_server(&path, |c| c.supports_rename).await
        else {
            unanswerable();
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            unanswerable();
            return;
        };
        let tx = tx.clone();
        log_file::log(&format!(
            "rename request id={request_id} server={server_name} path={} line={line} char={character} new_name={new_name}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.rename(uri, line, character, new_name).await;
            drop(client);
            let edits = match resp {
                Ok(Some(we)) => Some(workspace_edits(&we)),
                Ok(None) => None,
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] rename error: {e}"));
                    None
                }
            };
            log_file::log(&format!(
                "rename response id={request_id} server={server_name} files={}",
                edits.as_ref().map(Vec::len).unwrap_or(0)
            ));
            let _ = tx.send(RenameResult { request_id, edits });
        });
    }

    /// The servers under a root that holds one of `renames` and whose
    /// filters accept it, paired with the renames each one asked about.
    pub(super) fn file_op_clients(
        &self,
        renames: &[FileRenameOp],
        pick: impl Fn(&ManagedClient) -> Option<&Arc<crate::lsp::file_ops::FileOpFilters>>,
    ) -> Vec<(
        String,
        Arc<TokioMutex<LspClient>>,
        Vec<lsp_types::FileRename>,
    )> {
        let mut out = Vec::new();
        for ((_, root), clients) in &self.clients {
            for c in clients {
                let Some(filters) = pick(c) else {
                    continue;
                };
                let files: Vec<lsp_types::FileRename> = renames
                    .iter()
                    .filter(|r| r.old.starts_with(root) && filters.matches(&r.old, r.is_dir))
                    .filter_map(|r| {
                        Some(lsp_types::FileRename {
                            old_uri: Url::from_file_path(&r.old).ok()?.to_string(),
                            new_uri: Url::from_file_path(&r.new).ok()?.to_string(),
                        })
                    })
                    .collect();
                if !files.is_empty() {
                    out.push((c.name.clone(), c.client.clone(), files));
                }
            }
        }
        out
    }

    /// `willRenameFiles` fan-out (#610): every interested server, each with
    /// a deadline so a stalled one cannot hold the Explorer move hostage.
    pub(super) fn request_will_rename_files(
        &mut self,
        request_id: u64,
        renames: Vec<FileRenameOp>,
        tx: &std_mpsc::Sender<WillRenameFilesResult>,
    ) {
        let targets = self.file_op_clients(&renames, |c| c.will_rename.as_ref());
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut edits = Vec::new();
            for (name, client_arc, files) in targets {
                let resp = tokio::time::timeout(WILL_RENAME_TIMEOUT, async {
                    let mut requests = client_arc.lock().await.requests();
                    requests.will_rename_files(files).await
                })
                .await;
                match resp {
                    Ok(Ok(Some(we))) => edits.extend(workspace_edits(&we)),
                    Ok(Ok(None)) => {}
                    Ok(Err(e)) => log_file::log(&format!("lsp[{name}] willRenameFiles error: {e}")),
                    Err(_) => log_file::log(&format!("lsp[{name}] willRenameFiles timed out")),
                }
            }
            let _ = tx.send(WillRenameFilesResult { request_id, edits });
        });
    }

    pub(super) fn did_rename_files(&mut self, renames: Vec<FileRenameOp>) {
        for (name, client_arc, files) in self.file_op_clients(&renames, |c| c.did_rename.as_ref()) {
            tokio::spawn(async move {
                if let Err(e) = client_arc.lock().await.did_rename_files(files) {
                    log_file::log(&format!("lsp[{name}] didRenameFiles error: {e}"));
                }
            });
        }
    }

    /// Prepare-rename (#254): always answers, because the rename prompt
    /// is deferred on this verdict.
    pub(super) async fn request_prepare_rename(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<PrepareRenameResult>,
    ) {
        let unsupported = |tx: &std_mpsc::Sender<PrepareRenameResult>| {
            let _ = tx.send(PrepareRenameResult {
                request_id,
                unsupported: true,
                error: None,
                target: None,
            });
        };
        let Some(doc) = self.docs.get(&path) else {
            unsupported(tx);
            return;
        };
        let lang = doc.language;
        let root = doc.project_root.clone();
        self.ensure_clients(lang, &root).await;
        let Some(clients) = self.clients.get(&(lang, root)) else {
            unsupported(tx);
            return;
        };
        let picked = clients.iter().find(|c| c.supports_prepare_rename).map(|c| {
            (
                c.name.clone(),
                c.client.clone(),
                c.prepare_rename_stalled.clone(),
            )
        });
        let Some((server_name, client_arc, stalled)) = picked else {
            unsupported(tx);
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            unsupported(tx);
            return;
        };
        spawn_prepare_rename(
            PrepareRenameAsk {
                request_id,
                server_name,
                client: client_arc,
                uri,
                line,
                character,
                stalled,
            },
            PREPARE_RENAME_TIMEOUT,
            tx,
        );
    }
}

/// Whether the server's rename provider also validates positions via
/// `prepareRename` (#254): only an options-shaped provider can carry
/// `prepareProvider: true`.
pub(super) fn prepare_rename_supported(
    cap: &Option<OneOf<bool, lsp_types::RenameOptions>>,
) -> bool {
    matches!(cap, Some(OneOf::Right(opts)) if opts.prepare_provider == Some(true))
}

/// A `Range` as the UTF-16 4-tuple the app-facing results carry.
pub(super) fn range_tuple(r: &lsp_types::Range) -> (u32, u32, u32, u32) {
    (r.start.line, r.start.character, r.end.line, r.end.character)
}

/// How long a server may take to answer `textDocument/prepareRename`
/// (#1677) before F2 opens the word-under-cursor prompt instead. A
/// responsive server answers in about a millisecond; one that is busy,
/// paused or cut off must not strand "Preparing rename…" for good.
pub(super) const PREPARE_RENAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Ask the server and always send a verdict (#254). Waiting on the lock and
/// on the reply together is bounded by `timeout` (#1677): past it the verdict
/// is `unsupported`, which opens the word-under-cursor prompt, so the rename
/// still goes through. A server that has let one expire is not asked again.
pub(super) fn spawn_prepare_rename(
    ask: PrepareRenameAsk,
    timeout: std::time::Duration,
    tx: &std_mpsc::Sender<PrepareRenameResult>,
) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let PrepareRenameAsk {
            request_id,
            server_name,
            client,
            uri,
            line,
            character,
            stalled,
        } = ask;
        let unsupported = |request_id| PrepareRenameResult {
            request_id,
            unsupported: true,
            error: None,
            target: None,
        };
        if stalled.load(Ordering::Relaxed) {
            let _ = tx.send(unsupported(request_id));
            return;
        }
        // Only a request that actually went out can leak, so only then does
        // the timeout retire the server; waiting on a busy lock does not.
        let sent = AtomicBool::new(false);
        let resp = tokio::time::timeout(timeout, async {
            let mut requests = client.lock().await.requests();
            sent.store(true, Ordering::Relaxed);
            requests.prepare_rename(uri, line, character).await
        })
        .await;
        let Ok(resp) = resp else {
            let retire = sent.load(Ordering::Relaxed);
            if retire {
                stalled.store(true, Ordering::Relaxed);
            }
            log_file::log(&format!(
                "lsp[{server_name}] prepareRename timed out after {timeout:?}{}",
                if retire {
                    "; renames use the word under the cursor for this session"
                } else {
                    ""
                }
            ));
            let _ = tx.send(unsupported(request_id));
            return;
        };
        let (error, target, fallback) = match resp {
            Ok(Some(lsp_types::PrepareRenameResponse::Range(r))) => {
                (None, Some((range_tuple(&r), None)), false)
            }
            Ok(Some(lsp_types::PrepareRenameResponse::RangeWithPlaceholder {
                range,
                placeholder,
            })) => (None, Some((range_tuple(&range), Some(placeholder))), false),
            // "Default behavior": the server validates the position
            // but wants the client's own word logic — route the app
            // to its word-under-cursor prompt via `unsupported`.
            Ok(Some(lsp_types::PrepareRenameResponse::DefaultBehavior { .. })) => {
                (None, None, true)
            }
            Ok(None) => (None, None, false),
            Err(e) => {
                log_file::log(&format!("lsp[{server_name}] prepareRename error: {e:#}"));
                // Surface the server's own message: the fail-fast half
                // of prepare-rename. Trim the anyhow context chain to
                // the root cause line.
                let msg = e
                    .root_cause()
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or("rename rejected")
                    .to_string();
                (Some(msg), None, false)
            }
        };
        let _ = tx.send(PrepareRenameResult {
            request_id,
            unsupported: fallback,
            error,
            target,
        });
    });
}

/// How long one server may take to answer `willRenameFiles` (#610) before
/// the Explorer move goes ahead without its edit. VS Code waits about as
/// long; a rename the user is watching must not hang on a slow server.
pub(super) const WILL_RENAME_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);
