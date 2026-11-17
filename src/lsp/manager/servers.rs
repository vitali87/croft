//! Running the servers: starting the ones a file needs, pre-warming the workspace,
//! restarting a server that died, swapping in a new registry, and which server serves a
//! document.

use super::*;

/// The names of the servers `registry` registers, per language.
pub(super) fn registered_servers_of(
    registry: &ServerRegistry,
) -> HashMap<Language, Vec<&'static str>> {
    registry
        .languages()
        .map(|lang| {
            let names = registry.for_language(lang).iter().map(|c| c.name).collect();
            (lang, names)
        })
        .collect()
}

impl LspManager {
    /// Pre-warm the workspace's evident language servers (VS Code's
    /// `workspaceContains` activation). Inert under test: the suite builds
    /// hundreds of Apps in marker-bearing tempdirs and must never spawn a
    /// real rust-analyzer as a construction side effect.
    pub fn prewarm_workspace(&self) {
        if cfg!(test) {
            return;
        }
        let _ = self.cmd_tx.send(Cmd::PrewarmWorkspace);
    }

    /// Replace the secondary workspace-root set (multi-root, #147). Inert
    /// under `cfg(test)` like `prewarm_workspace`, for the same reason:
    /// test-built Apps must never spawn real servers.
    pub fn set_extra_roots(&self, roots: Vec<PathBuf>) {
        if cfg!(test) {
            return;
        }
        let _ = self.cmd_tx.send(Cmd::SetExtraRoots { roots });
    }

    /// Restart the servers serving `path` (#1621): the user's "Language
    /// Servers: Restart for This File". The app re-opens its tabs when they
    /// are back, as after a crash restart.
    pub fn restart_servers_for(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(Cmd::RestartServersFor { path });
    }

    /// Follow an extension added, removed, turned on or off (#1842):
    /// `user_sources` are the installed manifests now and `disabled` the
    /// disable set. Their languages resolve at once, and when that changes
    /// which servers a language registers, the worker swaps the new set in:
    /// that language's running servers stop, and the app re-opens its tabs,
    /// so the next of its files starts what is registered now. Every other
    /// language's servers keep running. Returns the servers no language
    /// registers any more.
    pub fn reload_extensions(
        &mut self,
        user_sources: &[&str],
        disabled: &std::collections::BTreeSet<String>,
    ) -> Vec<&'static str> {
        crate::lsp::languages::reload_with_user_sources(user_sources);
        let registry = ServerRegistry::with_user_extensions_filtered(user_sources, disabled);
        let registered = registered_servers_of(&registry);
        if registered == self.registered_servers {
            return Vec::new();
        }
        let kept: std::collections::HashSet<&str> =
            registered.values().flatten().copied().collect();
        let mut gone: Vec<&'static str> = self
            .registered_servers
            .values()
            .flatten()
            .copied()
            .filter(|name| !kept.contains(name))
            .collect();
        gone.sort_unstable();
        gone.dedup();
        self.registered_servers = registered;
        let _ = self.cmd_tx.send(Cmd::ReplaceRegistry { registry });
        gone
    }

    /// Pop the next server progress update or `window/*` message, if any.
    /// The app keeps a per-server progress map so the status bar can show or
    /// clear "Indexing…", and shows the messages.
    pub fn drain_server_update(&self) -> Option<ServerUpdate> {
        self.progress_rx.try_recv().ok()
    }

    /// Returns and clears the "a language server was restarted" flag (#694).
    /// The restarted server knows no documents, so the app answers by
    /// re-opening every tab against it.
    pub fn take_servers_restarted(&self) -> bool {
        self.servers_restarted.swap(false, Ordering::Relaxed)
    }

    /// Whether the server named `name` is registered for `lang`, i.e. no
    /// disabled extension removed it (#1511).
    pub fn server_registered(&self, lang: Language, name: &str) -> bool {
        self.registered_servers
            .get(&lang)
            .is_some_and(|names| names.contains(&name))
    }

    /// Test hook: replace the servers registered for `lang`, as a disabled
    /// or added extension would.
    #[cfg(test)]
    pub fn set_registered_servers_for_test(&mut self, lang: Language, names: &[&'static str]) {
        self.registered_servers.insert(lang, names.to_vec());
    }

    /// Whether the server named `name` is open on the document `path`
    /// (#1626), whether or not it still reports diagnostics on it.
    pub fn document_served_by(&self, path: &Path, name: &str) -> bool {
        self.capability_support.lock().is_ok_and(|s| {
            s.document_clients
                .get(path)
                .is_some_and(|names| names.iter().any(|n| n == name))
        })
    }

    /// Whether any running server has the open document `path` (#1566):
    /// one that does answers its completions, and word completion waits for
    /// that answer instead of opening on the document's words at once.
    pub fn document_has_server(&self, path: &Path) -> bool {
        self.capability_support.lock().is_ok_and(|s| {
            s.document_servers
                .get(path)
                .is_some_and(|names| !names.is_empty())
        })
    }

    /// Test hook: record `names` as the servers reporting on `path`, as an
    /// open would, without starting any.
    #[cfg(test)]
    pub fn set_document_servers_for_test(&self, path: &Path, names: &[&str]) {
        if let Ok(mut support) = self.capability_support.lock() {
            support.document_servers.insert(
                path.to_path_buf(),
                names.iter().map(|n| n.to_string()).collect(),
            );
        }
    }

    /// Test hook: record `names` as the servers open on `path` (#1626),
    /// without starting any or marking them as reporting on it.
    #[cfg(test)]
    pub fn set_document_clients_for_test(&self, path: &Path, names: &[&str]) {
        if let Ok(mut support) = self.capability_support.lock() {
            support.document_clients.insert(
                path.to_path_buf(),
                names.iter().map(|n| n.to_string()).collect(),
            );
        }
    }
}

/// How often a (language, root)'s servers may be restarted after exiting,
/// and within what window (#694). A server that dies on every start would
/// otherwise respawn on every keystroke; past the limit croft leaves it
/// down and says so, as it did before restarts existed.
pub(super) const MAX_RESTARTS: usize = 3;

pub(super) const RESTART_WINDOW: std::time::Duration = std::time::Duration::from_secs(10 * 60);

impl ServerRestarts {
    pub(super) fn reporting_to(restarted: Arc<AtomicBool>) -> Self {
        Self {
            restarted,
            ..Self::default()
        }
    }

    /// Record a restart of `key` at `now` if the budget allows one.
    pub(super) fn try_restart(&mut self, key: &ClientKey, now: std::time::Instant) -> bool {
        let attempts = self.attempts.entry(key.clone()).or_default();
        attempts.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
        if attempts.len() >= MAX_RESTARTS {
            self.gave_up.insert(key.clone());
            return false;
        }
        attempts.push(now);
        true
    }
}

impl WorkerState {
    /// Gracefully shut down every managed client concurrently. Each client's
    /// `shutdown` self-bounds its child-wait, so a single unresponsive server
    /// can't block the others. Clients are left in the map; their eventual Drop
    /// aborts the (now-finished) MainLoop and reaps the (now-exited) child.
    pub(super) async fn shutdown_all(&mut self) {
        let clients: Vec<Arc<TokioMutex<LspClient>>> = self
            .clients
            .values()
            .flatten()
            .map(|m| m.client.clone())
            .collect();
        let mut tasks = Vec::with_capacity(clients.len());
        for client in clients {
            tasks.push(tokio::spawn(async move {
                let mut client = client.lock().await;
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), client.shutdown())
                    .await;
            }));
        }
        for t in tasks {
            let _ = t.await;
        }
    }

    /// Restart the servers of every (language, root) with a server that has
    /// exited (#694): killed by the OOM killer, which croft asks to pick
    /// language servers first, or crashed. Their documents are forgotten and
    /// the app is told to re-open its tabs, since a new server knows none of
    /// them. Past [`MAX_RESTARTS`] in [`RESTART_WINDOW`] the dead servers are
    /// dropped and left down instead.
    pub(super) async fn restart_dead_servers(&mut self) {
        let dead: Vec<ClientKey> = self
            .clients
            .iter()
            .filter(|(key, clients)| {
                !self.restarts.gave_up.contains(*key)
                    && (self.restarts.incomplete.contains_key(*key)
                        || clients.iter().any(|managed| {
                            // A client in use is not a dead one; skip it rather
                            // than wait on the lock.
                            managed
                                .client
                                .try_lock()
                                .is_ok_and(|client| client.has_exited())
                        }))
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in dead {
            let Some(clients) = self.clients.remove(&key) else {
                continue;
            };
            let names: Vec<&str> = clients.iter().map(|c| c.name.as_str()).collect();
            // A server that never came up is not one the restart lost; a set
            // still short from the last restart keeps what it is missing.
            let expected = self
                .restarts
                .incomplete
                .get(&key)
                .cloned()
                .unwrap_or_else(|| names.iter().map(|n| n.to_string()).collect());
            if !self.restarts.try_restart(&key, std::time::Instant::now()) {
                let msg = format!(
                    "{} stopped {MAX_RESTARTS} times in 10 minutes; leaving it off until croft restarts or the workspace reopens",
                    names.join(", ")
                );
                log_file::log(&format!("lsp restart: {msg}"));
                crate::output::push("Language Servers", crate::output::OutputLevel::Error, &msg);
                // Keep the survivors; `gave_up` stops both this pass and the
                // empty-list re-probe from spawning the dead ones again.
                let alive: Vec<ManagedClient> = clients
                    .into_iter()
                    .filter(|m| m.client.try_lock().map_or(true, |c| !c.has_exited()))
                    .collect();
                // The dead ones report on nothing any more.
                if let Ok(mut support) = self.capability_support.lock() {
                    for (path, doc) in &self.docs {
                        if (doc.language, doc.project_root.clone()) != key {
                            continue;
                        }
                        if let Some(names) = support.document_servers.get_mut(path) {
                            names.retain(|n| alive.iter().any(|m| &m.name == n));
                        }
                        if let Some(names) = support.document_clients.get_mut(path) {
                            names.retain(|n| alive.iter().any(|m| &m.name == n));
                        }
                    }
                }
                self.restarts.incomplete.remove(&key);
                self.clients.insert(key, alive);
                continue;
            }
            let msg = format!("{} stopped; restarting", names.join(", "));
            log_file::log(&format!("lsp restart: {msg} for {}", key.1.display()));
            crate::output::push("Language Servers", crate::output::OutputLevel::Info, &msg);
            // The survivors go too: servers for one key are spawned and
            // opened as a set, and a half-restarted set would hold documents
            // the new server never saw.
            for managed in clients {
                let mut client = managed.client.lock().await;
                if !client.has_exited() {
                    let _ =
                        tokio::time::timeout(std::time::Duration::from_secs(3), client.shutdown())
                            .await;
                }
            }
            if let Ok(mut support) = self.capability_support.lock() {
                for (path, doc) in &self.docs {
                    if (doc.language, doc.project_root.clone()) == key {
                        support.document_servers.remove(path);
                        support.document_clients.remove(path);
                    }
                }
            }
            self.docs
                .retain(|_, doc| (doc.language, doc.project_root.clone()) != key);
            // Respawn as a re-probe, not a first attempt, so a set that comes
            // back short is marked incomplete and tried again.
            self.clients.insert(key.clone(), Vec::new());
            self.restarts.respawn = Some(expected);
            self.ensure_clients(key.0, &key.1).await;
            self.restarts.restarted.store(true, Ordering::Relaxed);
        }
    }

    /// Restart the servers serving `path` at the user's request (#1621): the
    /// (language, root) its document was opened under, else the one its
    /// language and project root would open it under, so the servers of
    /// other roots keep running. Unlike a crash restart this spends no
    /// budget and lifts a give-up, since the user asked for it.
    pub(super) async fn restart_servers_for(&mut self, path: &Path) {
        let keys: Vec<ClientKey> = match self.docs.get(path) {
            Some(doc) => vec![(doc.language, doc.project_root.clone())],
            None => path_to_language(path)
                .map(|lang| {
                    (
                        lang,
                        project_root_for_roots(path, lang, &self.workspace_root, &self.extra_roots),
                    )
                })
                .filter(|key| self.clients.contains_key(key))
                .into_iter()
                .collect(),
        };
        if keys.is_empty() {
            crate::output::push(
                "Language Servers",
                crate::output::OutputLevel::Info,
                &format!("No language server serves {}", path.display()),
            );
            return;
        }
        for key in keys {
            self.restart_key(key).await;
        }
        self.restarts.restarted.store(true, Ordering::Relaxed);
    }

    /// Shut the servers of `key` down and start them again, forgetting its
    /// documents, which the new servers never saw.
    pub(super) async fn restart_key(&mut self, key: ClientKey) {
        let names = self.retire_key(&key).await;
        let expected = self.restarts.incomplete.remove(&key).unwrap_or(names);
        self.clients.insert(key.clone(), Vec::new());
        if !expected.is_empty() {
            self.restarts.respawn = Some(expected.clone());
        }
        self.ensure_clients(key.0, &key.1).await;
        let msg = if expected.is_empty() {
            format!("Started the language servers for {}", key.1.display())
        } else {
            format!("Restarted {} for {}", expected.join(", "), key.1.display())
        };
        log_file::log(&format!("lsp restart: {msg}"));
        crate::output::push("Language Servers", crate::output::OutputLevel::Info, &msg);
    }

    /// Shut the servers of `key` down and forget them, its documents and
    /// its restart budget, returning the names of the servers that ran.
    pub(super) async fn retire_key(&mut self, key: &ClientKey) -> Vec<String> {
        let clients = self.clients.remove(key).unwrap_or_default();
        let names: Vec<String> = clients.iter().map(|c| c.name.clone()).collect();
        // Withdraw what the old servers published or answered to a pull
        // (#1102) for their documents: a server that does not come back
        // would never clear it, and one that does reports again once the
        // app re-opens the tabs.
        for (path, doc) in &self.docs {
            if (doc.language, doc.project_root.clone()) != *key {
                continue;
            }
            for name in &names {
                for pulled in [false, true] {
                    let _ = self.diagnostics_tx.send(DiagnosticsUpdate {
                        path: path.clone(),
                        server: name.clone(),
                        diagnostics: Vec::new(),
                        pulled,
                    });
                }
            }
        }
        for managed in clients {
            let mut client = managed.client.lock().await;
            if !client.has_exited() {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), client.shutdown())
                    .await;
            }
        }
        // Nothing serves its documents now, as after a crash; whatever
        // starts next says what it serves.
        if let Ok(mut support) = self.capability_support.lock() {
            for (path, doc) in &self.docs {
                if (doc.language, doc.project_root.clone()) == *key {
                    support.document_servers.remove(path);
                    support.document_clients.remove(path);
                }
            }
            support.completion_triggers.remove(key);
            support.inline_completion.remove(key);
        }
        self.docs
            .retain(|_, doc| (doc.language, doc.project_root.clone()) != *key);
        self.restarts.gave_up.remove(key);
        self.restarts.attempts.remove(key);
        names
    }

    /// Serve from `registry` from now on (#1842). A (language, root) whose
    /// servers it changes has the running ones shut down and is forgotten,
    /// so the next of its files starts what is registered now, as a first
    /// open does; the app re-opens its tabs for that. Every other key's
    /// servers keep running.
    pub(super) async fn replace_registry(&mut self, registry: ServerRegistry) {
        let old = std::mem::replace(&mut self.registry, registry);
        // A file an added or removed extension maps to another language or
        // project root now leaves the servers of its old one, so the app's
        // re-open reaches the new one's as an open, not as a change.
        let moved: Vec<PathBuf> = self
            .docs
            .iter()
            .filter(|(path, doc)| {
                path_to_language(path).map(|lang| {
                    let root =
                        project_root_for_roots(path, lang, &self.workspace_root, &self.extra_roots);
                    (lang, root)
                }) != Some((doc.language, doc.project_root.clone()))
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in moved {
            self.close_doc(path).await;
        }
        let names = |r: &ServerRegistry, lang: Language| -> Vec<&'static str> {
            r.for_language(lang).iter().map(|c| c.name).collect()
        };
        let changed: Vec<ClientKey> = self
            .clients
            .keys()
            .filter(|(lang, _)| names(&old, *lang) != names(&self.registry, *lang))
            .cloned()
            .collect();
        for key in changed {
            let stopped = self.retire_key(&key).await;
            self.restarts.incomplete.remove(&key);
            if !stopped.is_empty() {
                let msg = format!(
                    "Stopped {} for {}: the extensions serving it changed",
                    stopped.join(", "),
                    key.1.display()
                );
                log_file::log(&format!("lsp registry: {msg}"));
                crate::output::push("Language Servers", crate::output::OutputLevel::Info, &msg);
            }
        }
        // Tabs the old registry served nothing for were never opened here,
        // so every tab goes out again, as after a restart.
        self.restarts.restarted.store(true, Ordering::Relaxed);
    }

    pub(super) async fn ensure_clients(&mut self, lang: Language, root: &Path) -> &[ManagedClient] {
        // Re-probe when the cached list is empty: a server (e.g. the managed
        // vtsls) may have finished its background install since the first
        // attempt, so this is how TS LSP comes alive without a relaunch.
        // A non-empty list is stable and never re-probed.
        let key: ClientKey = (lang, root.to_path_buf());
        let first_attempt = !self.clients.contains_key(&key);
        // A key whose servers crash-looped stays down (#694): the empty-list
        // re-probe below would otherwise respawn them on every open.
        let empty = !first_attempt
            && self
                .clients
                .get(&key)
                .is_some_and(|clients| clients.is_empty());
        let respawn = self.restarts.respawn.take();
        let prepaid = respawn.is_some();
        let mut should_try = !self.restarts.gave_up.contains(&key) && (first_attempt || empty);
        // An empty list left by a restart whose respawn failed is re-probed
        // by every request. Each re-probe is another restart and spends the
        // same budget, so a server that never comes back stops being launched.
        if should_try
            && empty
            && !prepaid
            && self.restarts.attempts.contains_key(&key)
            && !self.restarts.try_restart(&key, std::time::Instant::now())
        {
            let msg = format!(
                "language servers for {} did not come back after {MAX_RESTARTS} restarts in 10 minutes; leaving them off until croft restarts or the workspace reopens",
                key.1.display()
            );
            log_file::log(&format!("lsp restart: {msg}"));
            crate::output::push("Language Servers", crate::output::OutputLevel::Error, &msg);
            should_try = false;
        }
        if should_try {
            // A launch's first frame comes first (see `startup_gate`): a file
            // restored at launch, or a restored extra root, otherwise starts
            // its servers, and the `rustup` that resolves rust-analyzer,
            // while that frame is still being drawn.
            crate::startup_gate::wait_async().await;
            let configs: Vec<ServerConfig> = self.registry.for_language(lang).to_vec();
            // Spawn every server for this root concurrently rather than awaiting
            // each in turn. The worker loop processes commands one at a time, so
            // a sequential spawn chain made the OpenDoc command (and the
            // semantic-token request queued right behind it) block on the SUM of
            // ty + basedpyright + ruff init handshakes. Concurrent spawn bounds
            // the cold-root stall to the SLOWEST single handshake instead.
            let resolved: Vec<(ServerConfig, Vec<PathBuf>)> = futures::future::join_all(
                configs
                    .iter()
                    .map(|config| resolve_for_root(config, root, first_attempt)),
            )
            .await
            .into_iter()
            .flatten()
            .collect();
            if first_attempt
                && resolved.is_empty()
                && let Some(note) =
                    missing_servers_note(lang, &configs, crate::lsp::install::missing_installer)
            {
                report_missing_servers_once(lang, &note);
            }
            let outcomes =
                futures::future::join_all(resolved.into_iter().map(|(config, extra_path)| {
                    let caps = build_client_capabilities();
                    let refresh = self.semantic_refresh.clone();
                    let inlay_refresh = self.inlay_refresh.clone();
                    let diagnostic_refresh = self.diagnostic_refresh.clone();
                    let diagnostics = self.diagnostics_tx.clone();
                    let progress = self.progress_tx.clone();
                    async move {
                        let result = LspClient::spawn(
                            &config,
                            root,
                            caps,
                            &extra_path,
                            refresh,
                            inlay_refresh,
                            diagnostic_refresh,
                            diagnostics,
                            progress,
                        )
                        .await;
                        (config, result)
                    }
                }))
                .await;
            let mut spawned: Vec<ManagedClient> = Vec::new();
            for (config, result) in outcomes {
                match result {
                    Ok(client) => {
                        let caps = client.capabilities();
                        // A capability that can be sent as a bare `false` (the
                        // bool-or-options shapes) must be read as its inner bool:
                        // `Option::is_some` treats `Some(false)` as supported,
                        // which made croft call `textDocument/declaration` on
                        // vtsls (which advertises `declarationProvider: false`)
                        // and get a -32601 "Unhandled method" back.
                        let supports = caps.completion_provider.is_some();
                        let completion_triggers = completion_triggers_of(caps);
                        let supports_signature_help =
                            signature_help_supported(&caps.signature_help_provider);
                        let supports_workspace_diagnostics =
                            workspace_diagnostics_supported(&caps.diagnostic_provider);
                        let supports_document_diagnostics = caps.diagnostic_provider.is_some();
                        let diagnostic_identifier =
                            diagnostic_identifier_of(&caps.diagnostic_provider);
                        let save_notify = save_notify_of(&caps.text_document_sync);
                        let supports_hover = hover_supported(&caps.hover_provider);
                        let supports_definition = one_of_supported(&caps.definition_provider);
                        let supports_document_symbol =
                            one_of_supported(&caps.document_symbol_provider);
                        let supports_declaration =
                            declaration_supported(&caps.declaration_provider);
                        let supports_type_definition =
                            type_definition_supported(&caps.type_definition_provider);
                        let supports_implementation =
                            implementation_supported(&caps.implementation_provider);
                        let supports_references = one_of_supported(&caps.references_provider);
                        let supports_document_highlight =
                            one_of_supported(&caps.document_highlight_provider);
                        let supports_linked_editing =
                            linked_editing_supported(&caps.linked_editing_range_provider);
                        let supports_inline_completion =
                            one_of_supported(&caps.inline_completion_provider);
                        let supports_selection_range =
                            selection_range_supported(&caps.selection_range_provider);
                        let supports_call_hierarchy =
                            call_hierarchy_supported(&caps.call_hierarchy_provider);
                        let supports_type_hierarchy = client.supports_type_hierarchy();
                        let supports_workspace_symbols =
                            one_of_supported(&caps.workspace_symbol_provider);
                        let supports_rename = one_of_supported(&caps.rename_provider);
                        let supports_prepare_rename =
                            prepare_rename_supported(&caps.rename_provider);
                        let supports_formatting =
                            one_of_supported(&caps.document_formatting_provider);
                        let on_type_triggers =
                            caps.document_on_type_formatting_provider.as_ref().map(|o| {
                                let mut t = o.first_trigger_character.clone();
                                for more in o.more_trigger_character.iter().flatten() {
                                    t.push_str(more);
                                }
                                t
                            });
                        let supports_range_formatting =
                            one_of_supported(&caps.document_range_formatting_provider);
                        let supports_code_action =
                            code_action_supported(&caps.code_action_provider);
                        let semantic_legend = semantic_legend_of(caps).map(Arc::new);
                        let semantic_supports_range = semantic_tokens_range_supported(caps);
                        let supports_inlay_hints = one_of_supported(&caps.inlay_hint_provider);
                        let supports_document_link = caps.document_link_provider.is_some();
                        let supports_code_lens = caps.code_lens_provider.is_some();
                        let code_lens_resolve = caps
                            .code_lens_provider
                            .as_ref()
                            .and_then(|o| o.resolve_provider)
                            .unwrap_or(false);
                        let server_commands = Arc::new(
                            caps.execute_command_provider
                                .as_ref()
                                .map(|o| o.commands.clone())
                                .unwrap_or_default(),
                        );
                        let supports_folding_range =
                            folding_range_supported(&caps.folding_range_provider);
                        let supports_document_color =
                            color_provider_supported(&caps.color_provider);
                        let file_ops = caps
                            .workspace
                            .as_ref()
                            .and_then(|w| w.file_operations.as_ref());
                        let compile = |reg: Option<
                            &lsp_types::FileOperationRegistrationOptions,
                        >| {
                            reg.map(|r| {
                                Arc::new(crate::lsp::file_ops::FileOpFilters::from_registration(r))
                            })
                        };
                        let will_rename = compile(file_ops.and_then(|f| f.will_rename.as_ref()));
                        let did_rename = compile(file_ops.and_then(|f| f.did_rename.as_ref()));
                        log_file::log(&format!(
                            "lsp[{}] spawned, root={} supports_completion={supports} supports_signature_help={supports_signature_help} supports_hover={supports_hover} supports_definition={supports_definition} supports_declaration={supports_declaration} supports_type_definition={supports_type_definition} supports_implementation={supports_implementation} supports_references={supports_references} supports_rename={supports_rename}",
                            config.name,
                            root.display()
                        ));
                        spawned.push(ManagedClient {
                            name: config.name.to_string(),
                            client: Arc::new(TokioMutex::new(client)),
                            supports_completion: supports,
                            completion_triggers,
                            supports_signature_help,
                            supports_workspace_diagnostics,
                            diagnostic_identifier,
                            diagnostic_result_ids: Arc::new(TokioMutex::new(
                                DiagnosticResultIds::default(),
                            )),
                            diagnostic_pull_stalled: Arc::new(AtomicBool::new(false)),
                            save_notify,
                            supports_document_diagnostics,
                            document_pull_stalled: Arc::new(AtomicBool::new(false)),
                            prepare_rename_stalled: Arc::new(AtomicBool::new(false)),
                            document_pull_generations: Arc::new(StdMutex::new(HashMap::new())),
                            supports_hover,
                            supports_definition,
                            supports_document_symbol,
                            supports_declaration,
                            supports_type_definition,
                            supports_implementation,
                            supports_references,
                            supports_document_highlight,
                            supports_linked_editing,
                            supports_inline_completion,
                            supports_selection_range,
                            supports_call_hierarchy,
                            supports_type_hierarchy,
                            supports_workspace_symbols,
                            supports_rename,
                            supports_prepare_rename,
                            supports_formatting,
                            on_type_triggers,
                            supports_range_formatting,
                            supports_code_action,
                            semantic_legend,
                            semantic_supports_range,
                            supports_inlay_hints,
                            supports_document_link,
                            supports_code_lens,
                            code_lens_resolve,
                            server_commands,
                            supports_folding_range,
                            supports_document_color,
                            will_rename,
                            did_rename,
                        });
                    }
                    Err(e) => {
                        log_file::log(&format!("lsp[{}] spawn failed: {e}", config.name));
                    }
                }
            }
            // Publish declaration / type-definition / implementation support for
            // this language so the app can show or hide the matching "Go to ..."
            // menu items synchronously. Written even when empty (value false) so
            // a missing entry means "not probed yet" rather than "unsupported".
            let supports_declaration = spawned.iter().any(|c| c.supports_declaration);
            let supports_type_definition = spawned.iter().any(|c| c.supports_type_definition);
            let supports_implementation = spawned.iter().any(|c| c.supports_implementation);
            let supports_references = spawned.iter().any(|c| c.supports_references);
            let supports_formatting = spawned.iter().any(|c| c.supports_formatting);
            let supports_code_action = spawned.iter().any(|c| c.supports_code_action);
            let supports_call_hierarchy = spawned.iter().any(|c| c.supports_call_hierarchy);
            if let Ok(mut support) = self.capability_support.lock() {
                support.declaration.insert(lang, supports_declaration);
                support
                    .type_definition
                    .insert(lang, supports_type_definition);
                support.implementation.insert(lang, supports_implementation);
                support.references.insert(lang, supports_references);
                support.formatting.insert(lang, supports_formatting);
                let triggers: String = spawned
                    .iter()
                    .filter_map(|c| c.on_type_triggers.as_deref())
                    .collect();
                // No entry when nothing advertises the provider — a missing
                // entry is the documented "no server carries it" answer.
                // Removed (not skipped) on the empty case so a re-probe of
                // the same language overwrites last-write-wins, like every
                // sibling capability map above.
                if triggers.is_empty() {
                    support.on_type_triggers.remove(&lang);
                } else {
                    support.on_type_triggers.insert(lang, triggers);
                }
                // The server `request_completion` picks: the first that
                // completes.
                match spawned.iter().find(|c| c.supports_completion) {
                    Some(c) => support
                        .completion_triggers
                        .insert(key.clone(), c.completion_triggers.clone()),
                    None => support.completion_triggers.remove(&key),
                };
                support.code_action.insert(lang, supports_code_action);
                support.call_hierarchy.insert(lang, supports_call_hierarchy);
                if spawned.iter().any(|c| c.supports_inline_completion) {
                    support.inline_completion.insert(key.clone());
                } else {
                    support.inline_completion.remove(&key);
                }
                support
                    .will_rename
                    .insert(lang, spawned.iter().any(|c| c.will_rename.is_some()));
            }
            // The first pull (#533). A server that answers workspace
            // diagnostics reports on files no editor has opened, and firing
            // here is what puts them in PROBLEMS without a manual command -
            // there is no other moment that covers a workspace whose files
            // are all still closed. Servers that do not advertise it drop out
            // in `workspace_pull_targets`, so this stays silent for them.
            spawn_workspace_pull(
                workspace_pull_targets(spawned.iter()),
                self.diagnostics_tx.clone(),
            );
            // An empty list that comes back is a restart as far as the app is
            // concerned: it sent its tabs while the list was empty, so these
            // servers have none of them (#694).
            if !first_attempt && !spawned.is_empty() {
                self.restarts.restarted.store(true, Ordering::Relaxed);
            }
            // A restart that lost a server which ran before it is retried as
            // a set by the next `restart_dead_servers` pass. One that never
            // came up is not a reason to restart the others.
            match respawn {
                Some(expected)
                    if !spawned.is_empty()
                        && expected
                            .iter()
                            .any(|name| !spawned.iter().any(|c| &c.name == name)) =>
                {
                    self.restarts.incomplete.insert(key.clone(), expected);
                }
                _ => {
                    self.restarts.incomplete.remove(&key);
                }
            }
            self.clients.insert(key.clone(), spawned);
        }
        self.clients.get(&key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The first server running for the open document `path` that `supports`
    /// a method, as its name and client. Re-probes first, so a server
    /// installed since the file was opened (e.g. the managed vtsls finishing
    /// its lazy background install) is picked up without reopening the file;
    /// cheap once the list is non-empty.
    pub(super) async fn pick_server(
        &mut self,
        path: &Path,
        supports: fn(&ManagedClient) -> bool,
    ) -> Result<(String, Arc<TokioMutex<LspClient>>), NoServer> {
        let doc = self.docs.get(path).ok_or(NoServer::Unknown)?;
        let lang = doc.language;
        let root = doc.project_root.clone();
        self.ensure_clients(lang, &root).await;
        self.clients
            .get(&(lang, root))
            .ok_or(NoServer::Unknown)?
            .iter()
            .find(|c| supports(c))
            .map(|c| (c.name.clone(), c.client.clone()))
            .ok_or(NoServer::Unsupported)
    }

    /// Spawn (or begin provisioning) the servers for every language whose
    /// root marker is evident in the workspace root — exactly what the
    /// first `open_doc` of that language would do, minus the document.
    pub(super) async fn prewarm_workspace(&mut self) {
        self.prewarm_root(self.workspace_root.clone()).await;
    }

    /// Prewarm one root: spawn the servers for every language whose
    /// marker sits at that root. Startup runs it on the primary; adding a
    /// workspace folder (#147) runs it on the new root so its servers
    /// come up like a fresh workspace's.
    pub(super) async fn prewarm_root(&mut self, root: PathBuf) {
        for lang in crate::lsp::languages::languages_evident_in(&root) {
            let clients = self.ensure_clients(lang, &root).await;
            if !clients.is_empty() {
                log_file::log(&format!(
                    "lsp prewarm: {} server(s) up for {} at {}",
                    clients.len(),
                    lang.lsp_id(),
                    root.display()
                ));
            }
        }
    }
}

/// Whether a `boolean | <Options>`-shaped capability (definition, rename, ...)
/// is actually supported. `None` is unsupported; a bare `false` is unsupported;
/// `true` or an options object is supported. The bare-`false` case is the one
/// `Option::is_some` got wrong.
pub(super) fn one_of_supported<B>(cap: &Option<OneOf<bool, B>>) -> bool {
    match cap {
        Some(OneOf::Left(b)) => *b,
        Some(OneOf::Right(_)) => true,
        None => false,
    }
}
