//! Going to code: definition, declaration, type definition, implementation, references,
//! and the call and type hierarchies.

use super::*;

#[derive(Debug)]
pub struct DefinitionResult {
    pub request_id: u64,
    pub target: Option<(PathBuf, u32, u32)>,
}

#[derive(Debug)]
pub struct DeclarationResult {
    pub request_id: u64,
    pub target: Option<(PathBuf, u32, u32)>,
    /// True when no spawned server advertises a usable `declarationProvider`
    /// (e.g. vtsls, which sends `declarationProvider: false`). Lets the app
    /// say so plainly instead of silently doing nothing.
    pub unsupported: bool,
}

#[derive(Debug)]
pub struct TypeDefinitionResult {
    pub request_id: u64,
    pub target: Option<(PathBuf, u32, u32)>,
    /// True when no spawned server advertises a usable `typeDefinitionProvider`.
    /// Mirrors `DeclarationResult::unsupported` so the app can report it plainly
    /// rather than silently no-op.
    pub unsupported: bool,
}

#[derive(Debug)]
pub struct ImplementationResult {
    pub request_id: u64,
    /// Every implementation location the server returned. `textDocument/
    /// implementation` is the 1:many goto (one abstraction, several
    /// implementors), so all of them are carried through; the app jumps
    /// directly when there is one and shows a picker when there are several.
    pub targets: Vec<(PathBuf, u32, u32)>,
    /// True when no spawned server advertises a usable `implementationProvider`.
    pub unsupported: bool,
}

#[derive(Debug)]
pub struct ReferencesResult {
    pub request_id: u64,
    /// Every reference location the server returned. `textDocument/references`
    /// is inherently 1:many (a symbol used in many places), so all of them are
    /// carried through; the app jumps directly when there is one and shows a
    /// picker when there are several, exactly like Go to Implementations.
    pub targets: Vec<(PathBuf, u32, u32)>,
    /// True when no spawned server advertises a usable `referencesProvider`.
    pub unsupported: bool,
}

/// One row of a call-hierarchy answer: a caller (incoming) or callee
/// (outgoing), with the location the picker jumps to — the call expression
/// for incoming calls when the server reports one, the callee's definition
/// for outgoing. Positions are LSP UTF-16, like every navigation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub name: String,
    pub path: PathBuf,
    pub line: u32,
    pub character: u32,
}

/// Which hierarchy a [`CallHierarchyResult`] answers: calls either way, or
/// a type's supertypes / subtypes (#613), which share the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyKind {
    IncomingCalls,
    OutgoingCalls,
    Supertypes,
    Subtypes,
}

impl HierarchyKind {
    /// How the status bar and picker name the rows.
    pub fn noun(self) -> &'static str {
        match self {
            HierarchyKind::IncomingCalls => "incoming calls",
            HierarchyKind::OutgoingCalls => "outgoing calls",
            HierarchyKind::Supertypes => "supertypes",
            HierarchyKind::Subtypes => "subtypes",
        }
    }
}

/// The callers or callees of the symbol at one caret position
/// (`prepareCallHierarchy` + `callHierarchy/incomingCalls|outgoingCalls`),
/// or the supertypes / subtypes of a type (#613), tagged with the request
/// id so stale replies drop.
#[derive(Debug)]
pub struct CallHierarchyResult {
    pub request_id: u64,
    pub kind: HierarchyKind,
    pub sites: Vec<CallSite>,
    /// True when no spawned server advertises the provider.
    pub unsupported: bool,
}

impl LspManager {
    pub fn request_definition(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        self.send_request(|request_id| Cmd::RequestDefinition {
            request_id,
            path,
            line,
            character,
        })
    }

    pub fn drain_definition(&self) -> Option<DefinitionResult> {
        self.def_rx.try_recv().ok()
    }

    /// Test hook: deliver a definition reply as if a server had answered.
    #[cfg(test)]
    pub fn push_definition_for_test(&self, result: DefinitionResult) {
        let _ = self.jump_test_tx.0.send(result);
    }

    /// Test hook: deliver a declaration reply as if a server had answered.
    #[cfg(test)]
    pub fn push_declaration_for_test(&self, result: DeclarationResult) {
        let _ = self.jump_test_tx.1.send(result);
    }

    /// Test hook: deliver a type definition reply as if a server had
    /// answered.
    #[cfg(test)]
    pub fn push_type_definition_for_test(&self, result: TypeDefinitionResult) {
        let _ = self.jump_test_tx.2.send(result);
    }

    pub fn request_declaration(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        self.send_request(|request_id| Cmd::RequestDeclaration {
            request_id,
            path,
            line,
            character,
        })
    }

    pub fn drain_declaration(&self) -> Option<DeclarationResult> {
        self.decl_rx.try_recv().ok()
    }

    pub fn request_type_definition(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        self.send_request(|request_id| Cmd::RequestTypeDefinition {
            request_id,
            path,
            line,
            character,
        })
    }

    pub fn drain_type_definition(&self) -> Option<TypeDefinitionResult> {
        self.type_def_rx.try_recv().ok()
    }

    pub fn request_implementation(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        self.send_request(|request_id| Cmd::RequestImplementation {
            request_id,
            path,
            line,
            character,
        })
    }

    pub fn drain_implementation(&self) -> Option<ImplementationResult> {
        self.impl_rx.try_recv().ok()
    }

    pub fn request_references(&mut self, path: PathBuf, line: u32, character: u32) -> u64 {
        self.send_request(|request_id| Cmd::RequestReferences {
            request_id,
            path,
            line,
            character,
        })
    }

    pub fn drain_references(&self) -> Option<ReferencesResult> {
        self.ref_rx.try_recv().ok()
    }

    /// Fire a call-hierarchy expansion (callers when `incoming`, callees
    /// otherwise) for the symbol at the caret; the reply arrives via
    /// [`Self::drain_call_hierarchy`] tagged with the returned id.
    pub fn request_call_hierarchy(
        &mut self,
        path: PathBuf,
        line: u32,
        character: u32,
        incoming: bool,
    ) -> u64 {
        self.send_request(|request_id| Cmd::RequestCallHierarchy {
            request_id,
            path,
            line,
            character,
            incoming,
        })
    }

    pub fn drain_call_hierarchy(&self) -> Option<CallHierarchyResult> {
        self.calls_rx.try_recv().ok()
    }

    /// The supertypes or subtypes of the type at a position (#613); the
    /// reply arrives on the call-hierarchy drain with its [`HierarchyKind`].
    pub fn request_type_hierarchy(
        &mut self,
        path: PathBuf,
        line: u32,
        character: u32,
        supertypes: bool,
    ) -> u64 {
        self.send_request(|request_id| Cmd::RequestTypeHierarchy {
            request_id,
            path,
            line,
            character,
            supertypes,
        })
    }

    /// Whether the server for `lang` implements call hierarchy. `None` means
    /// no server has reported yet. Drives the right-click menu rows.
    pub fn language_supports_call_hierarchy(&self, lang: Language) -> Option<bool> {
        self.language_supports(lang, |s| &s.call_hierarchy)
    }

    /// Whether the server for `lang` implements `textDocument/declaration`.
    /// `None` means no server has reported yet (not spawned). Read synchronously
    /// by the app to decide whether to show the "Go to Declaration" menu item.
    pub fn language_supports_declaration(&self, lang: Language) -> Option<bool> {
        self.language_supports(lang, |s| &s.declaration)
    }

    /// Whether the server for `lang` implements `textDocument/typeDefinition`.
    /// `None` means no server has reported yet. Read synchronously by the app to
    /// decide whether to show the "Go to Type Definition" menu item.
    pub fn language_supports_type_definition(&self, lang: Language) -> Option<bool> {
        self.language_supports(lang, |s| &s.type_definition)
    }

    /// Whether the server for `lang` implements `textDocument/implementation`.
    /// `None` means no server has reported yet. Read synchronously by the app to
    /// decide whether to show the "Go to Implementations" menu item.
    pub fn language_supports_implementation(&self, lang: Language) -> Option<bool> {
        self.language_supports(lang, |s| &s.implementation)
    }

    /// Whether the server for `lang` implements `textDocument/references`.
    /// `None` means no server has reported yet. Read synchronously by the app to
    /// decide whether to show the "Go to References" menu item.
    pub fn language_supports_references(&self, lang: Language) -> Option<bool> {
        self.language_supports(lang, |s| &s.references)
    }
}

impl WorkerState {
    pub(super) async fn request_definition(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<DefinitionResult>,
    ) {
        let (server_name, client_arc) =
            match self.pick_server(&path, |c| c.supports_definition).await {
                Ok(picked) => picked,
                Err(NoServer::Unsupported) => {
                    let _ = tx.send(DefinitionResult {
                        request_id,
                        target: None,
                    });
                    return;
                }
                Err(NoServer::Unknown) => return,
            };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        log_file::log(&format!(
            "definition request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.definition(uri, line, character).await;
            drop(client);
            let target = match resp {
                Ok(Some(r)) => def_location(&r),
                Ok(None) => None,
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] definition error: {e:#}"));
                    None
                }
            };
            log_file::log(&format!(
                "definition response id={request_id} server={server_name} has_target={}",
                target.is_some()
            ));
            let _ = tx.send(DefinitionResult { request_id, target });
        });
    }

    pub(super) async fn request_declaration(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<DeclarationResult>,
    ) {
        let (server_name, client_arc) =
            match self.pick_server(&path, |c| c.supports_declaration).await {
                Ok(picked) => picked,
                Err(NoServer::Unsupported) => {
                    let _ = tx.send(DeclarationResult {
                        request_id,
                        target: None,
                        unsupported: true,
                    });
                    return;
                }
                Err(NoServer::Unknown) => return,
            };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        log_file::log(&format!(
            "declaration request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.declaration(uri, line, character).await;
            drop(client);
            let target = match resp {
                Ok(Some(r)) => def_location(&r),
                Ok(None) => None,
                // `{e:#}` prints the full anyhow chain (the underlying JSON-RPC
                // error), not just the "declaration" context label.
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] declaration error: {e:#}"));
                    None
                }
            };
            log_file::log(&format!(
                "declaration response id={request_id} server={server_name} has_target={}",
                target.is_some()
            ));
            let _ = tx.send(DeclarationResult {
                request_id,
                target,
                unsupported: false,
            });
        });
    }

    pub(super) async fn request_type_definition(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<TypeDefinitionResult>,
    ) {
        let (server_name, client_arc) = match self
            .pick_server(&path, |c| c.supports_type_definition)
            .await
        {
            Ok(picked) => picked,
            Err(NoServer::Unsupported) => {
                let _ = tx.send(TypeDefinitionResult {
                    request_id,
                    target: None,
                    unsupported: true,
                });
                return;
            }
            Err(NoServer::Unknown) => return,
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        log_file::log(&format!(
            "type_definition request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.type_definition(uri, line, character).await;
            drop(client);
            let target = match resp {
                Ok(Some(r)) => def_location(&r),
                Ok(None) => None,
                // `{e:#}` prints the full anyhow chain (the underlying JSON-RPC
                // error), not just the "type_definition" context label.
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] type_definition error: {e:#}"));
                    None
                }
            };
            log_file::log(&format!(
                "type_definition response id={request_id} server={server_name} has_target={}",
                target.is_some()
            ));
            let _ = tx.send(TypeDefinitionResult {
                request_id,
                target,
                unsupported: false,
            });
        });
    }

    pub(super) async fn request_implementation(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<ImplementationResult>,
    ) {
        let (server_name, client_arc) =
            match self.pick_server(&path, |c| c.supports_implementation).await {
                Ok(picked) => picked,
                Err(NoServer::Unsupported) => {
                    let _ = tx.send(ImplementationResult {
                        request_id,
                        targets: Vec::new(),
                        unsupported: true,
                    });
                    return;
                }
                Err(NoServer::Unknown) => return,
            };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        log_file::log(&format!(
            "implementation request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.implementation(uri, line, character).await;
            drop(client);
            let targets = match resp {
                Ok(Some(r)) => def_locations(&r),
                Ok(None) => Vec::new(),
                // `{e:#}` prints the full anyhow chain (the underlying JSON-RPC
                // error), not just the "implementation" context label.
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] implementation error: {e:#}"));
                    Vec::new()
                }
            };
            log_file::log(&format!(
                "implementation response id={request_id} server={server_name} count={}",
                targets.len()
            ));
            let _ = tx.send(ImplementationResult {
                request_id,
                targets,
                unsupported: false,
            });
        });
    }

    pub(super) async fn request_references(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        tx: &std_mpsc::Sender<ReferencesResult>,
    ) {
        let (server_name, client_arc) =
            match self.pick_server(&path, |c| c.supports_references).await {
                Ok(picked) => picked,
                Err(NoServer::Unsupported) => {
                    let _ = tx.send(ReferencesResult {
                        request_id,
                        targets: Vec::new(),
                        unsupported: true,
                    });
                    return;
                }
                Err(NoServer::Unknown) => return,
            };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        log_file::log(&format!(
            "references request id={request_id} server={server_name} path={} line={line} char={character}",
            path.display()
        ));
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let resp = client.references(uri, line, character).await;
            drop(client);
            let targets = match resp {
                Ok(Some(locs)) => reference_locations(&locs),
                Ok(None) => Vec::new(),
                // `{e:#}` prints the full anyhow chain (the underlying JSON-RPC
                // error), not just the "references" context label.
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] references error: {e:#}"));
                    Vec::new()
                }
            };
            log_file::log(&format!(
                "references response id={request_id} server={server_name} count={}",
                targets.len()
            ));
            let _ = tx.send(ReferencesResult {
                request_id,
                targets,
                unsupported: false,
            });
        });
    }

    /// The two-step call-hierarchy flow: `prepareCallHierarchy` resolves the
    /// symbol at the caret into an item, then `incomingCalls` / `outgoingCalls`
    /// expands one level of it. One level per request — invoking again at a
    /// picked site walks the next level, which keeps the UI croft's existing
    /// location picker instead of a new tree widget.
    pub(super) async fn request_call_hierarchy(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        incoming: bool,
        tx: &std_mpsc::Sender<CallHierarchyResult>,
    ) {
        let kind = if incoming {
            HierarchyKind::IncomingCalls
        } else {
            HierarchyKind::OutgoingCalls
        };
        let (server_name, client_arc) =
            match self.pick_server(&path, |c| c.supports_call_hierarchy).await {
                Ok(picked) => picked,
                Err(NoServer::Unsupported) => {
                    let _ = tx.send(CallHierarchyResult {
                        request_id,
                        kind,
                        sites: Vec::new(),
                        unsupported: true,
                    });
                    return;
                }
                Err(NoServer::Unknown) => return,
            };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let item = match client.prepare_call_hierarchy(uri, line, character).await {
                Ok(Some(mut items)) if !items.is_empty() => items.remove(0),
                Ok(_) => {
                    drop(client);
                    let _ = tx.send(CallHierarchyResult {
                        request_id,
                        kind,
                        sites: Vec::new(),
                        unsupported: false,
                    });
                    return;
                }
                Err(e) => {
                    log_file::log(&format!(
                        "lsp[{server_name}] prepareCallHierarchy error: {e:#}"
                    ));
                    return;
                }
            };
            let sites = if incoming {
                match client.incoming_calls(item).await {
                    Ok(Some(calls)) => incoming_call_sites(calls),
                    Ok(None) => Vec::new(),
                    Err(e) => {
                        log_file::log(&format!("lsp[{server_name}] incomingCalls error: {e:#}"));
                        Vec::new()
                    }
                }
            } else {
                match client.outgoing_calls(item).await {
                    Ok(Some(calls)) => outgoing_call_sites(calls),
                    Ok(None) => Vec::new(),
                    Err(e) => {
                        log_file::log(&format!("lsp[{server_name}] outgoingCalls error: {e:#}"));
                        Vec::new()
                    }
                }
            };
            drop(client);
            let _ = tx.send(CallHierarchyResult {
                request_id,
                kind,
                sites,
                unsupported: false,
            });
        });
    }

    /// Type hierarchy (#613): prepare, then supertypes or subtypes, replied
    /// on the call-hierarchy channel so the app's picker serves both.
    pub(super) async fn request_type_hierarchy(
        &mut self,
        request_id: u64,
        path: PathBuf,
        line: u32,
        character: u32,
        supertypes: bool,
        tx: &std_mpsc::Sender<CallHierarchyResult>,
    ) {
        let kind = if supertypes {
            HierarchyKind::Supertypes
        } else {
            HierarchyKind::Subtypes
        };
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
            .find(|c| c.supports_type_hierarchy)
            .map(|c| (c.name.clone(), c.client.clone()));
        let reply = move |sites: Vec<CallSite>, unsupported: bool| CallHierarchyResult {
            request_id,
            kind,
            sites,
            unsupported,
        };
        let Some((server_name, client_arc)) = picked else {
            let _ = tx.send(reply(Vec::new(), true));
            return;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut client = client_arc.lock().await.requests();
            let item = match client.prepare_type_hierarchy(uri, line, character).await {
                Ok(Some(mut items)) if !items.is_empty() => items.remove(0),
                Ok(_) => {
                    drop(client);
                    let _ = tx.send(reply(Vec::new(), false));
                    return;
                }
                Err(e) => {
                    log_file::log(&format!(
                        "lsp[{server_name}] prepareTypeHierarchy error: {e:#}"
                    ));
                    return;
                }
            };
            let sites = match client.type_hierarchy(item, supertypes).await {
                Ok(Some(items)) => type_hierarchy_sites(items),
                Ok(None) => Vec::new(),
                Err(e) => {
                    log_file::log(&format!("lsp[{server_name}] {} error: {e:#}", kind.noun()));
                    Vec::new()
                }
            };
            drop(client);
            let _ = tx.send(reply(sites, false));
        });
    }
}

pub(super) fn call_hierarchy_supported(
    cap: &Option<lsp_types::CallHierarchyServerCapability>,
) -> bool {
    use lsp_types::CallHierarchyServerCapability as C;
    match cap {
        Some(C::Simple(b)) => *b,
        Some(C::Options(_)) => true,
        None => false,
    }
}

pub(super) fn declaration_supported(cap: &Option<DeclarationCapability>) -> bool {
    match cap {
        Some(DeclarationCapability::Simple(b)) => *b,
        Some(DeclarationCapability::RegistrationOptions(_) | DeclarationCapability::Options(_)) => {
            true
        }
        None => false,
    }
}

/// Type-definition capability (`boolean | TypeDefinitionOptions | ...`), same
/// bare-`false` rule as hover/declaration. vtsls, basedpyright, rust-analyzer
/// and gopls all advertise it, so the row is shown for those; a server that
/// omits it (or sends `false`) gets the row hidden instead of a -32601.
pub(super) fn type_definition_supported(cap: &Option<TypeDefinitionProviderCapability>) -> bool {
    match cap {
        Some(TypeDefinitionProviderCapability::Simple(b)) => *b,
        Some(TypeDefinitionProviderCapability::Options(_)) => true,
        None => false,
    }
}

/// Implementation capability (`boolean | ImplementationOptions | ...`), same
/// bare-`false` rule as hover/declaration/type-definition. rust-analyzer, gopls
/// and vtsls advertise it; a server that omits it (or sends `false`) gets the
/// "Go to Implementations" row hidden instead of a -32601.
pub(super) fn implementation_supported(cap: &Option<ImplementationProviderCapability>) -> bool {
    match cap {
        Some(ImplementationProviderCapability::Simple(b)) => *b,
        Some(ImplementationProviderCapability::Options(_)) => true,
        None => false,
    }
}

pub(super) fn def_location(resp: &GotoDefinitionResponse) -> Option<(PathBuf, u32, u32)> {
    let (uri, pos) = match resp {
        GotoDefinitionResponse::Scalar(loc) => (&loc.uri, loc.range.start),
        GotoDefinitionResponse::Array(locs) => {
            let loc = locs.first()?;
            (&loc.uri, loc.range.start)
        }
        GotoDefinitionResponse::Link(links) => {
            let link = links.first()?;
            (&link.target_uri, link.target_selection_range.start)
        }
    };
    let path = uri.to_file_path().ok()?;
    Some((path, pos.line, pos.character))
}

/// Every location in a goto response, not just the first. Used by Go to
/// Implementations, whose response is commonly an `Array` of every implementor
/// (a Scalar collapses to one element). Locations whose URI is not a local file
/// path are dropped.
pub(super) fn def_locations(resp: &GotoDefinitionResponse) -> Vec<(PathBuf, u32, u32)> {
    let raw: Vec<(&Url, Position)> = match resp {
        GotoDefinitionResponse::Scalar(loc) => vec![(&loc.uri, loc.range.start)],
        GotoDefinitionResponse::Array(locs) => {
            locs.iter().map(|l| (&l.uri, l.range.start)).collect()
        }
        GotoDefinitionResponse::Link(links) => links
            .iter()
            .map(|l| (&l.target_uri, l.target_selection_range.start))
            .collect(),
    };
    raw.into_iter()
        .filter_map(|(uri, pos)| Some((uri.to_file_path().ok()?, pos.line, pos.character)))
        .collect()
}

/// Every reference location the server returned, as local-file targets. Unlike
/// the goto family, `textDocument/references` replies with a flat `Vec<Location>`
/// (not a `GotoDefinitionResponse`), so it gets its own mapper rather than going
/// through `def_locations`. Locations whose URI is not a local file path are
/// dropped (the same rule `def_locations` applies).
/// Normalise `callHierarchy/incomingCalls` replies: each caller becomes a
/// site at its first reported call range (the call expression) or, when the
/// server sends none, at the caller's own selection range.
pub(super) fn incoming_call_sites(
    calls: Vec<lsp_types::CallHierarchyIncomingCall>,
) -> Vec<CallSite> {
    calls
        .into_iter()
        .filter_map(|c| {
            let path = c.from.uri.to_file_path().ok()?;
            let pos = c
                .from_ranges
                .first()
                .map(|r| r.start)
                .unwrap_or(c.from.selection_range.start);
            Some(CallSite {
                name: c.from.name,
                path,
                line: pos.line,
                character: pos.character,
            })
        })
        .collect()
}

/// Normalise `callHierarchy/outgoingCalls` replies: each callee becomes a
/// site at its definition (`selection_range`) — the `from_ranges` live in
/// the CALLER's document and are not jump targets here.
pub(super) fn outgoing_call_sites(
    calls: Vec<lsp_types::CallHierarchyOutgoingCall>,
) -> Vec<CallSite> {
    calls
        .into_iter()
        .filter_map(|c| {
            let path = c.to.uri.to_file_path().ok()?;
            let pos = c.to.selection_range.start;
            Some(CallSite {
                name: c.to.name,
                path,
                line: pos.line,
                character: pos.character,
            })
        })
        .collect()
}

/// Type-hierarchy items (#613) as picker rows, each landing on the type's
/// name rather than the top of its range.
pub(super) fn type_hierarchy_sites(items: Vec<lsp_types::TypeHierarchyItem>) -> Vec<CallSite> {
    items
        .into_iter()
        .filter_map(|t| {
            let path = t.uri.to_file_path().ok()?;
            let pos = t.selection_range.start;
            Some(CallSite {
                name: t.name,
                path,
                line: pos.line,
                character: pos.character,
            })
        })
        .collect()
}

pub(super) fn reference_locations(locs: &[Location]) -> Vec<(PathBuf, u32, u32)> {
    locs.iter()
        .filter_map(|l| {
            Some((
                l.uri.to_file_path().ok()?,
                l.range.start.line,
                l.range.start.character,
            ))
        })
        .collect()
}
